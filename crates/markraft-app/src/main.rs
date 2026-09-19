mod app;
mod doc;
mod instance;
mod legacy;
mod persistence;
mod platform;
mod storage;
mod updater;
mod vault;

use app::{NotesApp, bind_app_keys};
use gpui::*;
use instance::{Instance, Launch};
use platform::Platform;
use std::{env, path::PathBuf};
use storage::{Library, Settings};
use vault::Store;

const HELP: &str = "\
Markraft Notes — a floating, local-first notepad.

Usage: markraft-app [--dir PATH] [--settings PATH]

  --dir PATH        Keep notes as Markdown files in this folder for this run,
                    instead of the one chosen in the app.
  --settings PATH   Use this settings file instead of the default:
                    ~/Library/Application Support/Markraft/settings.json

The app stays in the menu bar while its window is hidden. ⌥N toggles the
window and ⌘K lists every action with its shortcut.
";

fn main() {
    let mut args = env::args().skip(1);
    let mut directory = None;
    let mut settings_path = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            absolute(PathBuf::from(
                args.next()
                    .unwrap_or_else(|| fail(&format!("{name} requires a path"))),
            ))
        };
        match arg.as_str() {
            "--dir" => directory = Some(value("--dir")),
            "--settings" => settings_path = Some(value("--settings")),
            "--help" | "-h" => {
                print!("{HELP}");
                return;
            }
            _ => fail(&format!("Unknown argument: {arg}. Use --help.")),
        }
    }
    let support = || {
        PathBuf::from(
            env::var_os("HOME").unwrap_or_else(|| fail("HOME is unavailable; use --dir PATH")),
        )
        .join(if cfg!(feature = "updater-mock") {
            "Library/Application Support/Markraft Update Test"
        } else {
            "Library/Application Support/Markraft"
        })
    };
    let settings_path = settings_path.unwrap_or_else(|| support().join("settings.json"));
    // This read is what sets a damaged settings file aside, so its notice travels
    // from here: the store's own read then finds no file at all.
    let settings = Settings::read(&settings_path).unwrap_or_default();
    // Until a folder has been chosen the app opens on that question.
    let directory = directory.or_else(|| settings.notes_folder.clone());
    let instance = match Instance::acquire(&settings_path).unwrap_or_else(|e| fail(&e)) {
        Launch::Forwarded => return,
        Launch::Primary(instance) => instance,
    };
    let opened = directory
        .clone()
        .map(|directory| Store::open(directory, settings_path.clone()));
    let (store, library, error) = match opened {
        Some(Ok((mut store, library))) => {
            if let Some(notice) = settings.recovery_notice() {
                store.notices().raise(notice);
            }
            let library = store.import_legacy(library, &app::legacy_library(&settings_path));
            (Some(store), library, None)
        }
        Some(Err(error)) => (None, Library::default(), Some(error)),
        None => (None, Library::default(), None),
    };
    let application = gpui_platform::application();
    application.on_reopen(|cx| {
        cx.dispatch_action(&app::Show);
    });
    application.run(move |cx: &mut App| {
        markraft_gpui::bind_keys(cx);
        // After the editor's own bindings, so that at the editor's context depth vim's
        // win; its predicates stand aside for the typeahead where that matters.
        markraft_vim::bind_keys(cx);
        bind_app_keys(cx);
        cx.set_reduce_motion(Platform::system_reduce_motion());
        let platform = Platform::new();
        let size = size(px(480.), px(320.));
        let mut bounds = Bounds::centered(None, size, cx);
        if let Some([x, y, w, h]) = library.preferences.window_bounds {
            // Clamp to the display the window was last on, so a note left on a
            // second monitor is not dragged back to the primary one. An unplugged
            // monitor falls to the display nearest to where the note used to be.
            let origin = point(px(x), px(y));
            let screen = cx
                .displays()
                .into_iter()
                .map(|display| display.visible_bounds())
                .min_by(|a, b| distance(*a, origin).total_cmp(&distance(*b, origin)))
                .or_else(|| cx.primary_display().map(|display| display.visible_bounds()));
            if let Some(screen) = screen {
                let w = px(w).max(px(360.)).min(screen.size.width);
                let h = px(h).max(px(220.)).min(screen.size.height);
                bounds = Bounds::new(
                    point(
                        px(x).max(screen.left()).min(screen.right() - w),
                        px(y).max(screen.top()).min(screen.bottom() - h),
                    ),
                    gpui::size(w, h),
                );
            }
        }
        cx.open_window(
            WindowOptions {
                kind: WindowKind::Floating,
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(gpui::size(px(360.), px(220.))),
                is_minimizable: false,
                titlebar: Some(TitlebarOptions {
                    title: Some("Markraft Notes".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(20.), px(20.))),
                }),
                ..Default::default()
            },
            move |window, cx| {
                let app = cx.new(|cx| {
                    NotesApp::new(
                        directory,
                        settings_path,
                        store,
                        library,
                        error,
                        platform,
                        instance,
                        window,
                        cx,
                    )
                });
                let weak = app.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let _ = weak.update(cx, |app, cx| app.hide(window, cx));
                    false
                });
                let weak = app.downgrade();
                cx.on_action(move |_: &app::Show, cx| {
                    let _ = window_handle_show(&weak, cx);
                });
                let weak = app.downgrade();
                let handle = window.window_handle();
                cx.on_action(move |_: &app::CheckForUpdates, cx| {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = weak.update(cx, |app, cx| app.check_for_updates(window, cx));
                    });
                });
                forward_menu_action::<app::Settings>(handle, cx);
                forward_menu_action::<app::Quit>(handle, cx);
                forward_menu_action::<app::NewNote>(handle, cx);
                forward_menu_action::<app::Browse>(handle, cx);
                forward_menu_action::<app::Save>(handle, cx);
                forward_menu_action::<app::Import>(handle, cx);
                forward_menu_action::<app::Export>(handle, cx);
                forward_menu_action::<markraft_gpui::Undo>(handle, cx);
                forward_menu_action::<markraft_gpui::Redo>(handle, cx);
                forward_menu_action::<markraft_gpui::Cut>(handle, cx);
                forward_menu_action::<markraft_gpui::Copy>(handle, cx);
                forward_menu_action::<markraft_gpui::Paste>(handle, cx);
                forward_menu_action::<markraft_gpui::PastePlain>(handle, cx);
                forward_menu_action::<markraft_gpui::PasteMarkdown>(handle, cx);
                forward_menu_action::<markraft_gpui::SelectAll>(handle, cx);
                app
            },
        )
        .expect("open Markraft Notes");
        cx.activate(true);
    });
}

fn forward_menu_action<A: Action>(handle: AnyWindowHandle, cx: &mut App) {
    // GPUI's macOS active-window lookup excludes NSPanel, so native menus need
    // an application listener even though keyboard actions reach this window.
    cx.on_action(move |action: &A, cx| {
        let _ = handle.update(cx, |_, window, cx| {
            if window.is_action_available(action, cx)
                && let Some(focus) = window.focused(cx)
            {
                window.activate_window();
                // Dispatch synchronously while this global listener is removed
                // by GPUI, avoiding recursion if a focused control propagates.
                focus.dispatch_action(action, window, cx);
            }
        });
    });
}

/// How far a point lies outside a display: zero for the one holding it, so the
/// display a window was last on wins and the closest remaining one takes over
/// when it is gone.
fn distance(bounds: Bounds<Pixels>, point: Point<Pixels>) -> f32 {
    let outside = |low: Pixels, high: Pixels, value: Pixels| {
        f32::from(low - value).max(f32::from(value - high)).max(0.)
    };
    outside(bounds.left(), bounds.right(), point.x).hypot(outside(
        bounds.top(),
        bounds.bottom(),
        point.y,
    ))
}

fn window_handle_show(app: &WeakEntity<NotesApp>, cx: &mut App) -> Result<(), ()> {
    let handle = cx.windows().first().copied().ok_or(())?;
    handle
        .update(cx, |_, window, cx| {
            let _ = app.update(cx, |app, cx| app.show(window, cx));
        })
        .map_err(|_| ())
}
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap().join(path)
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Markraft: {message}");
    std::process::exit(1)
}
