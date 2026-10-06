#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod app;
mod crash;
mod daily;
mod doc;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod e2e;
mod export;
mod file_access;
mod fs;
mod instance;
mod locale;
mod logging;
mod persistence;
mod platform;
mod remote_images;
mod send;
mod storage;
mod updater;
mod vault;

use app::{MarkraftApp, bind_app_keys};
use gpui::*;
use instance::{Instance, Launch, Request};
use platform::Platform;
use std::{env, path::PathBuf};
use storage::{Library, Settings, notes_folder_matches, resolve_notes_folder};
use vault::Store;

const HELP: &str = "\
Markraft — a floating, local-first notepad.

Usage: markraft-app [--dir PATH] [--settings PATH] [--] [FILE.md ...]

  --dir PATH        Keep notes as Markdown files in this folder for this run,
                    instead of the default or the one chosen in the app.
  --settings PATH   Use this settings file instead of the default:
                    ~/Library/Application Support/Markraft/settings.json

Positional files open into the notes folder session. Use -- before filenames
beginning with -.

Notes default to ~/Documents/Markraft. Change the folder in Settings (for
example to an existing notes folder). The app stays in the menu bar while its window
is hidden. ⌥N toggles the window and ⌘K lists every action with its shortcut.
";

fn main() {
    let crash_reports = crash::directory();
    if let Some(directory) = &crash_reports {
        logging::init(directory);
        crash::install(directory.clone());
    }
    let mut args = env::args_os().skip(1);
    let mut directory = None;
    let mut settings_path = None;
    let mut paths = Vec::new();
    let mut positional = false;
    while let Some(arg) = args.next() {
        if positional {
            paths.push(absolute(PathBuf::from(arg)));
            continue;
        }
        let mut value = |name: &str| {
            absolute(PathBuf::from(
                args.next()
                    .unwrap_or_else(|| fail(&format!("{name} requires a path"))),
            ))
        };
        match arg.to_str() {
            Some("--dir") => directory = Some(value("--dir")),
            Some("--settings") => settings_path = Some(value("--settings")),
            Some("--help" | "-h") => {
                print!("{HELP}");
                return;
            }
            Some("--") => positional = true,
            Some(value) if value.starts_with('-') => {
                fail(&format!("Unknown argument: {value}. Use --help."));
            }
            _ => paths.push(absolute(PathBuf::from(arg))),
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
    let restore_files = paths.is_empty();
    let initial_request = if paths.is_empty() {
        Request::Show
    } else {
        Request::OpenPaths(paths)
    };
    let instance = match Instance::acquire(&settings_path, initial_request)
        .unwrap_or_else(|e| fail(&e.to_string()))
    {
        Launch::Forwarded => return,
        Launch::Primary(instance) => instance,
    };
    // After the handover, because this read is what sets a damaged settings file
    // aside — a second launch that is only passing a request along must not move
    // the file this one is using, and its notice would have nowhere to be shown.
    let mut settings = Settings::read(&settings_path).unwrap_or_default();
    let (mut file_access, mut access_notices) = file_access::FileAccess::load(&settings_path);
    access_notices.extend(file_access.restore(&mut settings));
    // After the handover too: a launch that only passes a request along must
    // not take the notice from the one that will show it.
    let crash_notice = crash_reports.as_deref().and_then(crash::take_notice);
    let settings_notice = settings.recovery_notice();
    let preferences = settings.preferences.clone();
    if restore_files {
        // Only the primary process restores the previous session. A second
        // ordinary launch just raises the current one. Queue individual paths
        // so a saved session is not constrained by the IPC batch byte limit.
        let sender = instance.sender();
        for path in &settings.open_files {
            if let Err(error) = sender.send(Request::OpenPaths(vec![path.clone()])) {
                log::warn!("could not reopen {}: {error}", path.display());
            }
        }
    }
    let home = env::var_os("HOME").map(PathBuf::from);
    let directory = resolve_notes_folder(directory, settings.notes_folder.clone(), home.as_deref())
        .unwrap_or_else(|error| fail(&error.to_string()));
    let opened = Store::open(directory.clone(), settings_path.clone(), settings.clone());
    let (store, library, error) = match opened {
        Ok((mut store, library)) => {
            // Remember --dir, the default folder, or a path that only matched
            // after canonicalization.
            let folder = store.directory().to_owned();
            if !notes_folder_matches(settings.notes_folder.as_deref(), &folder)
                && let Err(error) =
                    store.update_settings(|settings| settings.notes_folder = Some(folder))
            {
                log::warn!("could not remember the notes folder: {error}");
            }
            (Some(store), library, None)
        }
        Err(error) => (None, Library::default(), Some(error.into())),
    };
    let sender = instance.sender();
    let application = gpui_platform::application();
    application.on_open_urls(move |urls| {
        if let Err(error) = sender.open_urls(urls) {
            log::warn!("{error}");
        }
    });
    application.on_reopen(|cx| {
        cx.dispatch_action(&app::Show);
    });
    application.run(move |cx: &mut App| {
        // Its controls' bindings live in their own key contexts, so binding it first
        // leaves every editor and app binding free to answer where it applies.
        gpui_base::init(cx);
        markraft_gpui::bind_keys(cx);
        markraft_gpui::use_system_pasteboard(cx);
        // After the editor's own bindings, so that at the editor's context depth vim's
        // win; its predicates stand aside for the typeahead where that matters.
        markraft_vim::bind_keys(cx);
        bind_app_keys(cx);
        cx.set_reduce_motion(Platform::system_reduce_motion());
        let platform = Platform::new();
        let size = size(px(480.), px(320.));
        let mut bounds = Bounds::centered(None, size, cx);
        if let Some([x, y, w, h]) = preferences.window_bounds {
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
                    title: Some("Markraft".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(20.), px(20.))),
                }),
                ..Default::default()
            },
            move |window, cx| {
                let app = cx.new(|cx| {
                    MarkraftApp::new(
                        Some(directory),
                        settings_path,
                        store,
                        library,
                        preferences,
                        error,
                        Some(platform),
                        updater::Updater::new(),
                        instance,
                        window,
                        cx,
                    )
                    .with_file_access(file_access)
                });
                for notice in access_notices {
                    app.update(cx, |app, cx| app.announce(notice, cx));
                }
                if let Some((notice, damaged)) = settings_notice {
                    app.update(cx, |app, cx| app.announce_with_reveal(notice, damaged, cx));
                }
                if let Some((notice, report)) = crash_notice {
                    app.update(cx, |app, cx| app.announce_with_reveal(notice, report, cx));
                }
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
                forward_menu_action::<app::OpenMarkdown>(handle, cx);
                forward_menu_action::<app::Export>(handle, cx);
                forward_menu_action::<app::ExportHtml>(handle, cx);
                forward_menu_action::<app::ExportPdf>(handle, cx);
                forward_menu_action::<app::Print>(handle, cx);
                forward_menu_action::<app::Find>(handle, cx);
                forward_menu_action::<app::FindNext>(handle, cx);
                forward_menu_action::<app::FindPrevious>(handle, cx);
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
        .expect("open Markraft");
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

fn window_handle_show(app: &WeakEntity<MarkraftApp>, cx: &mut App) -> Result<(), ()> {
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
