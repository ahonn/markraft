//! Nonblocking workspace operations and their completion boundaries.
use super::*;

pub(super) type SaveContinuation =
    Box<dyn FnOnce(&mut MarkraftApp, &mut Window, &mut Context<MarkraftApp>)>;

/// Real workers are outside GPUI's deterministic test scheduler. Tests poll
/// their replies using the virtual clock, without installing a scheduler waker
/// on an OS thread. Production uses the channel's normal event-driven wakeup.
pub(super) async fn receive<T>(
    future: impl std::future::Future<Output = T>,
    executor: BackgroundExecutor,
) -> T {
    #[cfg(not(test))]
    {
        let _ = executor;
        future.await
    }
    #[cfg(test)]
    {
        let mut future = std::pin::pin!(future);
        loop {
            let polled = future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            if let std::task::Poll::Ready(result) = polled {
                return result;
            }
            executor.timer(Duration::from_millis(1)).await;
        }
    }
}

impl MarkraftApp {
    /// Hold `panel`, a system file panel just opened, until it answers, with the
    /// note stepped down from floating meanwhile. macOS opens these panels at
    /// the normal window level, so a note kept above other windows would cover
    /// the panel it asked for. The answer comes back unchanged.
    pub(super) fn file_panel<T: 'static>(
        &mut self,
        panel: futures_channel::oneshot::Receiver<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures_channel::oneshot::Receiver<T> {
        if !self.preferences.always_on_top {
            return panel;
        }
        if let Some(platform) = &self.platform {
            let _ = platform.set_always_on_top(window, false);
        }
        let (answer, answered) = futures_channel::oneshot::channel();
        cx.spawn_in(window, async move |this, cx| {
            let reply = panel.await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |this, _| {
                    if let Some(platform) = &this.platform {
                        let _ = platform.set_always_on_top(window, this.preferences.always_on_top);
                    }
                })
            });
            if let Ok(reply) = reply {
                let _ = answer.send(reply);
            }
        })
        .detach();
        answered
    }

    pub(super) fn is_reloading(&self) -> bool {
        self.reloading.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn watch_persistence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._persistence_wake = None;
        let wake = self.persistence.as_mut().and_then(Persistence::take_wake);
        #[cfg(test)]
        {
            let _ = (wake, window, cx);
        }
        #[cfg(not(test))]
        if let Some(mut wake) = wake {
            use futures_util::StreamExt;
            self._persistence_wake = Some(cx.spawn_in(window, async move |this, cx| {
                while wake.next().await.is_some() {
                    if cx
                        .update(|window, cx| this.update(cx, |this, cx| this.poll(window, cx)))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
    }
    pub(super) fn start_persistence(
        store: Store,
        house: markraft_commonmark::HouseStyleHandle,
    ) -> Persistence {
        #[cfg(test)]
        {
            Persistence::new_unwatched(store, house)
        }
        #[cfg(not(test))]
        {
            Persistence::new(store, house)
        }
    }

    pub(super) fn run_io<T: 'static>(
        &mut self,
        future: impl std::future::Future<Output = Result<T, StoreError>> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, StoreError>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let epoch = self.io.epoch;
        self.io.pending += 1;
        let started = Instant::now();
        cx.spawn_in(window, async move |this, cx| {
            let result = receive(future, cx.background_executor().clone()).await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |this, cx| {
                    if this.io.epoch != epoch {
                        return;
                    }
                    this.io.pending -= 1;
                    log::debug!(
                        "workspace operation completed in {}ms",
                        started.elapsed().as_millis()
                    );
                    done(this, result, window, cx);
                    cx.notify();
                })
            });
        })
        .detach();
        cx.notify();
    }

    /// A barrier finishes only once the current edits are durable. Edits made
    /// while awaiting its receipt require another barrier before the continuation.
    pub(super) fn flush_then(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.save_waiters.push(Box::new(done));
        if self.io.flushing {
            return;
        }
        self.flush_pending(window, cx);
    }

    fn flush_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        let Some(persistence) = &self.persistence else {
            self.save_waiters.clear();
            return;
        };
        self.io.flushing = true;
        let revision = self.save.barrier();
        let future =
            persistence.flush_async(revision, self.library.clone(), self.preferences.clone());
        self.run_io(future, window, cx, move |this, result, window, cx| {
            this.io.flushing = false;
            match result {
                Ok(saved) => {
                    let success = saved.result.is_ok();
                    this.apply_saved(saved, cx);
                    if success {
                        if this.save.is_dirty() || !this.library.changes.is_empty() {
                            this.flush_pending(window, cx);
                        } else {
                            let waiters = std::mem::take(&mut this.save_waiters);
                            for done in waiters {
                                done(this, window, cx);
                            }
                        }
                    } else {
                        this.save_waiters.clear();
                    }
                }
                Err(error) => {
                    this.save.apply_completion(revision, false);
                    this.feedback.set_error(error);
                    this.save_waiters.clear();
                }
            }
        });
    }

    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_refresh_files(&self) {
        if let Some(persistence) = &self.persistence {
            persistence.refresh();
        }
    }

    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_io_pending(&self) -> bool {
        self.io.pending > 0
    }

    /// Start a flush barrier; the returned cell turns true once it completes.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn test_flush(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> std::rc::Rc<std::cell::Cell<bool>> {
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = done.clone();
        self.flush_then(window, cx, move |_, _, _| flag.set(true));
        done
    }
}
