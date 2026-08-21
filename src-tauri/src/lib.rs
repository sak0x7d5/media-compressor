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

use tauri::{Emitter, Manager};

/// Event fired when a second launch hands us files — the Explorer context menu
/// path.
pub const EVENT_OPEN_FILES: &str = "open-files";

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

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The queue's worker thread needs an AppHandle to emit events, so
            // state is built here rather than before the builder runs.
            let state = commands::AppState::new(app.handle());
            let config_dir = state.config_dir.clone();
            app.manage(state);

            // Refresh the preset list in the background. This is a no-op unless
            // the user has configured a source URL, and a failure is never
            // allowed to affect startup — a stale limit still compresses files.
            std::thread::spawn(move || {
                let _ = presets::refresh(&config_dir, false);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_presets,
            commands::save_presets,
            commands::ffmpeg_status,
            commands::install_ffmpeg,
            commands::probe_file,
            commands::add_files,
            commands::cancel_job,
            commands::cancel_all,
            commands::copy_to_clipboard,
            commands::reveal_in_folder,
            commands::pending_files,
            commands::preview_pair,
            commands::shell_menu_status,
            commands::set_shell_menu,
            commands::set_presets_url,
            commands::refresh_presets,
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
