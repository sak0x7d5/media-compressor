pub mod clipboard;
pub mod commands;
pub mod ffmpeg;
pub mod images;
pub mod pipeline;
pub mod presets;
pub mod preview;
pub mod queue;
pub mod shell_integration;
pub mod strategy;
pub mod updates;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// How long the main window may stay hidden while the frontend gets ready.
///
/// The frontend normally beats this by an order of magnitude. The timer is
/// here so that a bundle which fails to boot leaves the user with a visible
/// window to close rather than a process with no window at all.
const WINDOW_REVEAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Event fired when a second launch hands us files — the Explorer context menu
/// path.
pub const EVENT_OPEN_FILES: &str = "open-files";

/// A one-shot latch over showing the main window.
///
/// The window is created hidden — see `visible` in `tauri.conf.json` — because
/// a Tauri window exists well before WebView2 has anything to draw in it, and
/// showing it early means the user stares at an empty frame for as long as the
/// webview takes to start. Two things race to reveal it: the frontend, once it
/// has a populated UI, and a timeout. This makes sure only the first one wins.
#[derive(Default)]
pub struct WindowReveal(AtomicBool);

impl WindowReveal {
    /// True for exactly one caller, ever.
    fn claim(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    /// Give up the claim without using it — the window is already up.
    fn spend(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Show the main window, once.
///
/// Safe to call from any thread and any number of times; every call after the
/// first does nothing, so a late reveal can never drag the window back in front
/// of whatever the user moved on to.
pub fn reveal_main_window(app: &AppHandle) {
    let Some(latch) = app.try_state::<WindowReveal>() else { return };
    if !latch.claim() {
        return;
    }

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Record that the window is already up, so nothing reveals it a second time.
fn mark_revealed(app: &AppHandle) {
    if let Some(latch) = app.try_state::<WindowReveal>() {
        latch.spend();
    }
}

/// Pull real file paths out of a command line, ignoring flags and anything that
/// is not actually on disk.
pub fn collect_file_args<S: AsRef<str>>(argv: &[S]) -> Vec<String> {
    argv.iter()
        .skip(1)
        .map(|arg| arg.as_ref())
        .filter(|arg| !arg.starts_with('-'))
        .filter(|arg| std::path::Path::new(arg).is_file())
        .map(|arg| arg.to_string())
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance must be registered first: a second launch has to be
        // intercepted before it builds a window of its own. Without it, every
        // right-click "Compress for Discord" opens another copy of the app.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let files = collect_file_args(&argv);
            if !files.is_empty() {
                let _ = app.emit(EVENT_OPEN_FILES, files);
            }

            // A second launch is a request to look at the app, so it brings
            // the window forward whether or not the first launch has revealed
            // it yet.
            mark_revealed(app);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // The updater needs this to relaunch into the version it just installed.
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // The queue's worker thread needs an AppHandle to emit events, so
            // state is built here rather than before the builder runs.
            let state = commands::AppState::new(app.handle());
            let config_dir = state.config_dir.clone();
            app.manage(state);
            app.manage(WindowReveal::default());

            // The frontend calls `ui_ready` as soon as it has something worth
            // looking at. This is only the backstop for when it never does.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(WINDOW_REVEAL_TIMEOUT);
                reveal_main_window(&handle);
            });

            updates::check_in_background(app.handle());

            // Refresh the preset list in the background. This is a no-op unless
            // the user has configured a source URL, and a failure is never
            // allowed to affect startup — a stale limit still compresses files.
            std::thread::spawn(move || {
                let _ = presets::refresh(&config_dir, false);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup,
            commands::ui_ready,
            commands::list_presets,
            commands::save_presets,
            commands::ffmpeg_status,
            commands::ffmpeg_version,
            commands::install_ffmpeg,
            commands::probe_file,
            commands::add_files,
            commands::cancel_job,
            commands::cancel_all,
            commands::copy_to_clipboard,
            commands::reveal_in_folder,
            commands::preview_pair,
            commands::shell_menu_status,
            commands::set_shell_menu,
            commands::set_presets_url,
            commands::refresh_presets,
            updates::check_for_update,
            updates::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_itself_is_never_treated_as_an_input() {
        let files = collect_file_args(&["media-compressor.exe"]);
        assert!(files.is_empty());
    }

    #[test]
    fn flags_and_missing_paths_are_ignored() {
        let files = collect_file_args(&[
            "media-compressor.exe",
            "--some-flag",
            "C:/definitely/not/here.mp4",
        ]);
        assert!(files.is_empty(), "got {files:?}");
    }

    /// The window starts hidden, so whichever of the frontend signal and the
    /// timeout arrives first is what the user sees. The loser must do nothing:
    /// a second show-and-focus seconds later would yank the window back over
    /// whatever they had switched to.
    #[test]
    fn only_the_first_caller_reveals_the_window() {
        let latch = WindowReveal::default();

        assert!(latch.claim(), "the first caller has to be the one that shows the window");
        assert!(!latch.claim(), "a second reveal would steal focus back from the user");
    }

    /// A second launch shows the window itself, so the pending timeout must
    /// find the claim already gone.
    #[test]
    fn marking_the_window_up_stops_a_later_reveal() {
        let latch = WindowReveal::default();
        latch.spend();

        assert!(!latch.claim());
    }

    #[test]
    fn real_files_are_collected() {
        let dir = std::env::temp_dir().join("media-compressor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("argv-{}.mp4", std::process::id()));
        std::fs::write(&file, b"x").unwrap();

        let path = file.to_string_lossy().to_string();
        let files = collect_file_args(&["media-compressor.exe".to_string(), path.clone()]);

        assert_eq!(files, vec![path]);
        let _ = std::fs::remove_file(&file);
    }
}
