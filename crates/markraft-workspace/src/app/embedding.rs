//! Host-facing operations; no process or window lifecycle is implied by mounting.
use super::*;
use crate::host::{SaveReceipt, WorkspaceError, WorkspaceOptions};

impl MarkraftApp {
    /// Create a workspace bound to this window. The host must retain the window
    /// until `prepare_close` completes before releasing or moving the workspace.
    pub fn open(
        folders: markraft_notes::NotesConfig,
        options: WorkspaceOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, WorkspaceError> {
        options.preferences.validate()?;
        let state_directory = folders.state_dir;
        let settings_path = state_directory.join("workspace.json");
        let notes_directory = crate::storage::ensure_notes_folder(&folders.notes_dir)?;
        let (store, library) = Store::open_library(notes_directory.clone(), state_directory)?;
        let mut view = Self::new(
            Some(notes_directory),
            settings_path,
            Some(store),
            library,
            options.preferences,
            None,
            None,
            Box::new(crate::updater::Disabled),
            Box::new(crate::instance::NoRequests),
            window,
            cx,
        );
        view.host_settings = false;
        view.remote_fetcher = Some(crate::remote_images::fetcher(options.cache_directory));
        // Initial editors were created before the explicit network capability existed.
        for editor in view.editors() {
            let fetcher = view.remote_image_fetcher();
            editor.update(cx, |editor, cx| editor.set_remote_images(fetcher, cx));
        }
        Ok(view)
    }

    /// Mount host-owned storage without creating a Markdown directory.
    pub fn with_backend(
        backend: Box<dyn markraft_notes::NotesBackend>,
        options: WorkspaceOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, WorkspaceError> {
        options.preferences.validate()?;
        let house = markraft_commonmark::HouseStyleHandle::default();
        let session = markraft_notes::NotesSession::from_backend(backend, house)?;
        Ok(Self::with_session(session, options, window, cx))
    }

    /// Mount a session prepared by the host, including sessions opened off the UI thread.
    /// The host must validate `options.preferences` before mounting.
    pub fn with_session(
        session: markraft_notes::NotesSession,
        options: WorkspaceOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let house = session.house();
        let (library, persistence) = session.into_parts();
        let mut view = Self::new_with_session(
            None,
            PathBuf::new(),
            Notes {
                library,
                persistence,
            },
            house,
            options.preferences,
            None,
            None,
            Box::new(crate::updater::Disabled),
            Box::new(crate::instance::NoRequests),
            window,
            cx,
        );
        view.host_settings = false;
        view.set_remote_image_cache(options.cache_directory, cx);
        view
    }

    /// Request reconciliation after the host commits external changes.
    pub fn refresh_from_storage(&mut self, cx: &mut Context<Self>) {
        self.sync_documents(cx);
        if let Some(persistence) = &self.notes.persistence {
            persistence.refresh();
        }
    }

    /// Reconcile only the named notes after the host changes them in storage.
    /// A note that storage no longer returns is treated as deleted.
    pub fn refresh_notes_from_storage(
        &mut self,
        ids: Vec<markraft_notes::NoteId>,
        cx: &mut Context<Self>,
    ) {
        self.sync_documents(cx);
        if let Some(persistence) = &self.notes.persistence {
            persistence.refresh_notes(ids);
        }
    }

    /// Supply a per-workspace image cache; no cache writes occur outside this path.
    pub fn set_remote_image_cache(&mut self, cache: Option<PathBuf>, cx: &mut Context<Self>) {
        self.remote_fetcher = Some(crate::remote_images::fetcher(cache));
        for editor in self.editors() {
            let fetcher = self.remote_image_fetcher();
            editor.update(cx, |editor, cx| editor.set_remote_images(fetcher, cx));
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closing && self.notes.persistence.is_none()
    }

    pub fn active_editor(&self) -> Entity<EditorView> {
        self.editor().clone()
    }
    pub fn preferences(&self) -> &Preferences {
        &self.preferences
    }
    pub fn set_options(
        &mut self,
        preferences: Preferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), WorkspaceError> {
        if self.closing {
            return Err(if self.is_closed() {
                WorkspaceError::Closed
            } else {
                WorkspaceError::Busy
            });
        }
        preferences.validate()?;
        let before = std::mem::replace(&mut self.preferences, preferences);
        self.apply_preferences(&before, window, cx);
        self.refresh_slash_commands();
        self.schedule_save(cx);
        cx.notify();
        Ok(())
    }
    pub fn create_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing {
            self.new_note(window, cx);
        }
    }
    pub fn open_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing {
            self.open_paths(paths, window, cx);
        }
    }

    /// Capture a final save before the host's non-cancellable system quit hook.
    /// Interactive close should use `prepare_close`, which can recover on failure.
    pub fn flush_on_system_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> crate::persistence::Pending<SaveReceipt> {
        self.sync_documents(cx);
        let revision = self.save.barrier();
        let Some(persistence) = &self.notes.persistence else {
            return Box::pin(async {
                Err(StoreError::Worker(Message::new("error.worker-unavailable")))
            });
        };
        let pending = persistence.flush_async(
            revision,
            self.notes.library.clone(),
            self.preferences.clone(),
        );
        Box::pin(async move {
            let saved = pending.await?;
            saved.result?;
            Ok(SaveReceipt {
                revision: saved.revision,
                notes: saved.outcomes.clone(),
                markdown_paths: saved.paths,
                conflict_notes: saved.conflicts,
            })
        })
    }

    /// Save exactly the currently committed revision. Later edits remain dirty.
    /// Retain both the entity and its window until the callback. The entity need
    /// not remain mounted in a view. Destroying the window cancels the callback.
    pub fn flush(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<SaveReceipt, WorkspaceError>, &mut Window, &mut Context<Self>)
        + 'static,
    ) {
        self.flush_with(window, cx, move |_, result, window, cx| {
            done(result, window, cx)
        });
    }

    /// Public requests complete while their entity and window remain alive, even
    /// when a directory switch invalidates their revision.
    fn run_host_io<T: 'static>(
        &mut self,
        pending: impl std::future::Future<Output = Result<T, StoreError>> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, WorkspaceError>, &mut Window, &mut Context<Self>)
        + 'static,
    ) {
        let epoch = self.io.epoch;
        self.io.pending += 1;
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = io::receive(pending, cx.background_executor().clone()).await;
            // Operation accounting belongs to the entity, even if the host
            // destroys its window before this request finishes.
            let Ok(result) = this.update(cx, |this, cx| {
                let result = if this.io.epoch == epoch {
                    this.io.pending -= 1;
                    result.map_err(WorkspaceError::from)
                } else {
                    Err(WorkspaceError::Superseded)
                };
                cx.notify();
                result
            }) else {
                return;
            };
            let _ = window.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| {
                    done(this, result, window, cx);
                    cx.notify();
                })
            });
        })
        .detach();
        cx.notify();
    }

    fn flush_with(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(
            &mut Self,
            Result<SaveReceipt, WorkspaceError>,
            &mut Window,
            &mut Context<Self>,
        ) + 'static,
    ) {
        self.sync_documents(cx);
        let revision = self.save.barrier();
        let Some(persistence) = &self.notes.persistence else {
            done(self, Err(WorkspaceError::Closed), window, cx);
            return;
        };
        let pending = persistence.flush_async(
            revision,
            self.notes.library.clone(),
            self.preferences.clone(),
        );
        self.run_host_io(
            pending,
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(saved) => {
                    let receipt = saved
                        .result
                        .clone()
                        .map_err(WorkspaceError::from)
                        .map(|()| SaveReceipt {
                            revision: saved.revision,
                            notes: saved.outcomes.clone(),
                            markdown_paths: saved.paths.clone(),
                            conflict_notes: saved.conflicts.clone(),
                        });
                    this.apply_saved(saved, cx);
                    done(this, receipt, window, cx);
                }
                Err(error) => done(this, Err(error), window, cx),
            },
        );
    }

    /// Freeze edits, save, and wait for the worker and directory lock to stop.
    /// On failure the workspace returns to an editable, retryable state.
    /// A busy workspace must finish its accepted operation before retrying.
    /// Retain the entity until the callback before releasing it or its window.
    pub fn prepare_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<(), WorkspaceError>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.is_closed() {
            done(Ok(()), window, cx);
            return;
        }
        if self.closing || self.is_reloading() || self.io.pending > 0 || self.io.flushing {
            done(Err(WorkspaceError::Busy), window, cx);
            return;
        }
        // Standalone startup can present a storage error before a worker exists.
        // That error screen has no accepted writes and must still be closable.
        if self.notes.persistence.is_none() {
            self.closing = true;
            done(Ok(()), window, cx);
            return;
        }
        self.context_menus.dismiss();
        self.cancel_checking_panel();
        self.close_popover(cx);
        self.cancel_input(cx);
        self.editor()
            .update(cx, |editor, cx| editor.cancel_composition(cx));
        self.closing = true;
        self.reloading
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.flush_with(window, cx, move |this, saved, window, cx| {
            if let Err(error) = saved {
                this.closing = false;
                this.reloading
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                done(Err(error), window, cx);
                return;
            }
            let pending = this
                .notes
                .persistence
                .as_ref()
                .expect("saved worker exists")
                .shutdown_async();
            this.run_host_io(pending, window, cx, move |this, result, window, cx| {
                if result.is_ok() {
                    this.notes.persistence.take();
                } else {
                    this.closing = false;
                    this.reloading
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
                done(result, window, cx);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::e2e::harness::open;
    use crate::host::WorkspaceError;
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    fn a_startup_error_without_a_worker_can_close(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            super::MarkraftApp::new(
                None,
                root.path().join("settings.json"),
                None,
                Default::default(),
                Default::default(),
                None,
                None,
                Box::new(crate::updater::Disabled),
                Box::new(crate::instance::NoRequests),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.prepare_close(window, cx, |result, _, _| assert!(result.is_ok()));
                assert!(view.is_closed());
            });
        });
    }

    #[gpui::test]
    fn replacing_the_library_completes_a_public_save_with_superseded(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut h = open(cx, |_| {});
        h.type_text("A save request with a lifecycle owner");
        h.wait_for_io();
        let result = Rc::new(RefCell::new(None));
        let completed = result.clone();
        h.cx.update(|window, cx| {
            h.app.update(cx, |view, cx| {
                view.flush(window, cx, move |saved, _, _| {
                    *completed.borrow_mut() = Some(saved)
                });
                view.replace_library(view.notes.library.clone(), window, cx);
            })
        });
        h.wait_until(|_| result.borrow().is_some());
        assert!(matches!(
            result.borrow_mut().take(),
            Some(Err(WorkspaceError::Superseded))
        ));
    }

    #[gpui::test]
    fn destroying_the_window_still_finishes_public_operation_accounting(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut h = open(cx, |_| {});
        h.wait_for_io();
        let called = Rc::new(std::cell::Cell::new(false));
        let completed = called.clone();
        h.cx.update(|window, cx| {
            h.app.update(cx, |view, cx| {
                view.run_host_io(std::future::ready(Ok(())), window, cx, move |_, _, _, _| {
                    completed.set(true)
                });
                assert_eq!(view.io.pending, 1);
            });
            window.remove_window();
        });
        h.cx.run_until_parked();
        assert_eq!(h.app.update(h.cx, |view, _| view.io.pending), 0);
        assert!(!called.get(), "a removed window cannot receive a callback");
    }
}
