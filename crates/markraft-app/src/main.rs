mod app;
mod instance;
mod persistence;
mod platform;
mod storage;

use app::{NotesApp, bind_app_keys};
use gpui::*;
use instance::{Instance, Launch};
use platform::Platform;
use std::{env, path::PathBuf};
use storage::{Library, Store};

const HELP: &str = "\
Markraft Notes — a floating, local-first notepad.

Usage: markraft-app [--file PATH]

  --file PATH   Use this note library instead of the default:
                ~/Library/Application Support/Markraft/notes.json

The app stays in the menu bar while its window is hidden. ⌥N toggles the
window and ⌘K lists every action with its shortcut.
";

fn main() {
    let mut args = env::args().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--file" => {
                path = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| fail("--file requires a path")),
                ))
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return;
            }
            _ => fail(&format!("Unknown argument: {arg}. Use --help.")),
        }
    }
    let path = path.unwrap_or_else(|| {
        PathBuf::from(
            env::var_os("HOME").unwrap_or_else(|| fail("HOME is unavailable; use --file PATH")),
        )
        .join("Library/Application Support/Markraft/notes.json")
    });
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir().unwrap().join(path)
    };
    let instance = match Instance::acquire(&path).unwrap_or_else(|e| fail(&e)) {
        Launch::Forwarded => return,
        Launch::Primary(instance) => instance,
    };
    let (store, library, error) = match Store::open(path.clone()) {
        Ok((store, mut library)) => {
            // Import the old default single-note file once, without modifying it.
            let legacy = path.with_file_name("note.json");
            if !path.exists()
                && path.file_name().is_some_and(|n| n == "notes.json")
                && legacy.exists()
            {
                match std::fs::read_to_string(&legacy)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        markraft_core::Document::from_json(&text).map_err(|e| e.to_string())
                    }) {
                    Ok(document) => {
                        let id = library.active_id.clone();
                        library.set_document(&id, document);
                    }
                    Err(error) => eprintln!(
                        "Legacy note was not imported: {error}. Original: {}",
                        legacy.display()
                    ),
                }
            }
            (Some(store), library, None)
        }
        Err(error) => (None, Library::default(), Some(error)),
    };
    let application = gpui_platform::application();
    application.on_reopen(|cx| {
        cx.dispatch_action(&app::Show);
    });
    application.run(move |cx: &mut App| {
        markraft_gpui::bind_keys(cx);
        bind_app_keys(cx);
        cx.set_reduce_motion(Platform::system_reduce_motion());
        let platform = Platform::new();
        let size = size(px(480.), px(320.));
        let mut bounds = Bounds::centered(None, size, cx);
        if let Some([x, y, w, h]) = library.preferences.window_bounds {
            // Clamp to the primary display so an unplugged monitor cannot strand the note.
            if let Some(display) = cx.primary_display() {
                let screen = display.visible_bounds();
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
                    NotesApp::new(path, store, library, error, platform, instance, window, cx)
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
                app
            },
        )
        .expect("open Markraft Notes");
        cx.activate(true);
    });
}
fn window_handle_show(app: &WeakEntity<NotesApp>, cx: &mut App) -> Result<(), ()> {
    let handle = cx.windows().first().copied().ok_or(())?;
    handle
        .update(cx, |_, window, cx| {
            let _ = app.update(cx, |app, cx| app.show(window, cx));
        })
        .map_err(|_| ())
}
fn fail(message: &str) -> ! {
    eprintln!("Markraft: {message}");
    std::process::exit(1)
}
