//! The host owns its window, controls, menus, and close decision.

use gpui::{prelude::*, *};
use markraft_workspace::{WorkspaceEvent, WorkspaceOptions, WorkspaceView, bind_workspace_keys};

actions!(integration_host, [QuitHost]);

struct Host {
    workspace: Option<Entity<WorkspaceView>>,
    status: String,
    busy: bool,
    pending_close: Option<bool>,
    events: Option<Subscription>,
    _quit: Subscription,
    // Retain the directories until the storage worker releases its lock.
    data: tempfile::TempDir,
}

impl Host {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = tempfile::tempdir().expect("create the sample data directory");
        let notes = data.path().join("notes");
        let state = data.path().join("state");
        let cache = data.path().join("cache");
        for directory in [&notes, &state, &cache] {
            std::fs::create_dir_all(directory).expect("create a sample directory");
        }
        std::fs::write(
            notes.join("Welcome.md"),
            "# An embedded workspace\n\nEdit this note, then use the host's Save button.\n\n- The host owns this window.\n- The workspace owns note editing.\n",
        )
        .expect("write the sample note");
        let mut options = WorkspaceOptions::new(notes, state);
        options.cache_directory = Some(cache);
        options.preferences.remote_images = false;
        options.preferences.auto_height = false;
        options.preferences.hide_on_deactivate = false;
        let workspace = cx.new(|cx| {
            WorkspaceView::open(options, window, cx).expect("open the temporary workspace")
        });
        let events = cx.subscribe_in(
            &workspace,
            window,
            |host, _, event: &WorkspaceEvent, window, cx| match event {
                WorkspaceEvent::QuitRequested => host.close(true, window, cx),
                WorkspaceEvent::HideRequested => host.close(false, window, cx),
                WorkspaceEvent::OpenSettings => {
                    host.status = "The host received a settings request.".into();
                    cx.notify();
                }
                WorkspaceEvent::ReportIssue => {
                    host.status = "The host received a support request.".into();
                    cx.notify();
                }
                WorkspaceEvent::OptionsChanged(preferences) => {
                    let path = host.data.path().join("host-preferences.json");
                    let saved = serde_json::to_vec_pretty(preferences)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            std::fs::write(&path, bytes).map_err(|error| error.to_string())
                        });
                    host.status = match saved {
                        Ok(()) => format!("Host saved editor options to {}.", path.display()),
                        Err(error) => format!("Host preferences could not be saved: {error}"),
                    };
                    cx.notify();
                }
                _ => {}
            },
        );
        let editor = workspace.read(cx).active_editor();
        window.focus(&editor.focus_handle(cx), cx);
        let quit = cx.on_app_quit(|host, cx| {
            let pending = host.workspace.as_ref().and_then(|workspace| {
                if workspace.read(cx).is_closed() {
                    None
                } else {
                    Some(workspace.update(cx, |workspace, cx| workspace.flush_on_system_quit(cx)))
                }
            });
            async move {
                if let Some(pending) = pending
                    && let Err(error) = pending.await
                {
                    eprintln!("The system quit save failed: {error}");
                }
            }
        });
        Self {
            workspace: Some(workspace),
            status: format!("Sample data: {}", data.path().display()),
            busy: false,
            pending_close: None,
            events: Some(events),
            _quit: quit,
            data,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        self.busy = true;
        self.status = "Saving the committed document…".into();
        let host = cx.entity().downgrade();
        workspace.update(cx, |workspace, cx| {
            workspace.flush(window, cx, move |result, window, cx| {
                // A failure can complete synchronously. Defer to avoid reentering Host.
                window.defer(cx, move |window, cx| {
                    let _ = host.update(cx, |host, cx| {
                        host.busy = false;
                        host.status = match result {
                            Ok(receipt) => format!(
                                "Saved revision {}. Markdown files: {}.",
                                receipt.revision,
                                receipt.markdown_paths.len()
                            ),
                            Err(error) => format!("Save failed. Retry after resolving: {error}"),
                        };
                        if let Some(close_window) = host.pending_close.take() {
                            host.close(close_window, window, cx);
                        }
                        cx.notify();
                    });
                });
            });
        });
        cx.notify();
    }

    fn close(&mut self, close_window: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.pending_close = Some(close_window || self.pending_close.unwrap_or(false));
            return;
        }
        let Some(workspace) = self.workspace.clone() else {
            if close_window {
                window.remove_window();
                cx.quit();
            }
            return;
        };
        self.busy = true;
        self.status = "Saving and closing the workspace…".into();
        let host = cx.entity().downgrade();
        workspace.update(cx, |workspace, cx| {
            workspace.prepare_close(window, cx, move |result, window, cx| {
                window.defer(cx, move |window, cx| {
                    let _ = host.update(cx, |host, cx| {
                        host.busy = false;
                        let queued_window_close = host.pending_close.take().unwrap_or(false);
                        let close_window = close_window || queued_window_close;
                        match result {
                            Ok(()) => {
                                host.events.take();
                                host.workspace.take();
                                host.status =
                                    "Workspace closed. The host still owns this window.".into();
                                if close_window {
                                    window.remove_window();
                                    cx.quit();
                                }
                            }
                            Err(error) => {
                                host.status =
                                    format!("Close failed. The workspace remains open: {error}");
                            }
                        }
                        cx.notify();
                    });
                });
            });
        });
        cx.notify();
    }
}

fn button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(0x283e50))
        .text_color(rgb(0xffffff))
        .cursor_pointer()
        .child(label)
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xf2f4f6))
            .text_color(rgb(0x172c3a))
            .child(
                div()
                    .p_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().child("Example host · embedded Markraft"))
                    .child(
                        button("save", "Save").on_click(cx.listener(|host, _, window, cx| {
                            host.save(window, cx);
                        })),
                    )
                    .child(button("close-workspace", "Close workspace").on_click(
                        cx.listener(|host, _, window, cx| host.close(false, window, cx)),
                    )),
            )
            .child(div().px_3().pb_2().text_sm().child(self.status.clone()))
            .child(div().flex_1().min_h_0().children(self.workspace.clone()))
    }
}

fn main() {
    gpui_platform::application().run(|cx| {
        bind_workspace_keys(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", QuitHost, None)]);
        cx.set_menus([Menu::new("Example host").items([MenuItem::action("Quit", QuitHost)])]);
        let window = cx
            .open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("Markraft integration host".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(800.), px(620.)),
                        cx,
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let host = cx.new(|cx| Host::new(window, cx));
                    let weak = host.downgrade();
                    window.on_window_should_close(cx, move |window, cx| {
                        let _ = weak.update(cx, |host, cx| host.close(true, window, cx));
                        false
                    });
                    host
                },
            )
            .expect("open the host window");
        cx.on_action(move |_: &QuitHost, cx| {
            // Keyboard dispatch already borrows this window. Update it after dispatch ends.
            cx.defer(move |cx| {
                if let Err(error) = window.update(cx, |host, window, cx| {
                    host.close(true, window, cx);
                }) {
                    eprintln!("The host quit request could not reach its window: {error}");
                }
            });
        });
        cx.activate(true);
    });
}
