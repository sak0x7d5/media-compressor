pub mod changelog;
pub mod clipboard;
pub mod commands;
pub mod ffmpeg;
pub mod images;
pub mod pipeline;
pub mod presets;
pub mod preview;
pub mod queue;
pub mod shell_integration;
pub mod shell_menu;
pub mod strategy;
pub mod trace;
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
    trace::mark("window shown");
}

/// Record that the window is already up, so nothing reveals it a second time.
fn mark_revealed(app: &AppHandle) {
    if let Some(latch) = app.try_state::<WindowReveal>() {
        latch.spend();
    }
}

/// What a launch was asked to do.
///
/// The submenu entries pass `--target <bytes>` alongside the paths, so a launch
/// carries both the files and, when the user picked a size from the menu rather
/// than opening the app cold, the size they picked.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Launch {
    pub files: Vec<String>,
    pub target_bytes: Option<u64>,
}

impl Launch {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Pull the files and the requested target out of a command line.
///
/// Anything that is not a flag we recognise and not a file on disk is dropped,
/// so a stray argument can never enqueue something that is not there. An
/// unparseable or absurd `--target` is ignored rather than fatal: the user
/// still gets their files, just at the default size.
pub fn parse_launch<S: AsRef<str>>(argv: &[S]) -> Launch {
    /// A target below this cannot hold any encodable output, and above it the
    /// value is meaningless as an upload limit. Either way it is not something
    /// we wrote into the registry.
    const MIN_TARGET: u64 = 10_000;
    const MAX_TARGET: u64 = 100_000_000_000;

    let mut launch = Launch::default();
    let mut arguments = argv.iter().skip(1).map(|argument| argument.as_ref());

    while let Some(argument) = arguments.next() {
        let value = match argument.strip_prefix("--target") {
            // `--target=N`
            Some(rest) if rest.starts_with('=') => Some(rest[1..].to_string()),
            // `--target N`
            Some(rest) if rest.is_empty() => arguments.next().map(|next| next.to_string()),
            _ => None,
        };

        if let Some(value) = value {
            launch.target_bytes = value
                .parse::<u64>()
                .ok()
                .filter(|bytes| (MIN_TARGET..=MAX_TARGET).contains(bytes));
            continue;
        }

        if argument.starts_with('-') {
            continue;
        }
        if std::path::Path::new(argument).is_file() {
            launch.files.push(argument.to_string());
        }
    }

    launch
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance must be registered first: a second launch has to be
        // intercepted before it builds a window of its own. Without it, every
        // right-click "Shrink" opens another copy of the app.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let launch = parse_launch(&argv);
            if !launch.is_empty() {
                let _ = app.emit(EVENT_OPEN_FILES, launch);
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
        // Updating in place. The plugin verifies the downloaded installer
        // against the public key in tauri.conf.json before running it, so a
        // compromised release host still cannot ship anyone a binary.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Relaunching after an update. On Windows the NSIS installer usually
        // closes the app itself, but the other platforms need this.
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            trace::mark("setup: entered (window and webview created)");

            // The queue's worker thread needs an AppHandle to emit events, so
            // state is built here rather than before the builder runs.
            let state = commands::AppState::new(app.handle());
            trace::mark("setup: app state built");
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


            // Refresh the preset list in the background. This is a no-op unless
            // the user has configured a source URL, and a failure is never
            // allowed to affect startup — a stale limit still compresses files.
            std::thread::spawn(move || {
                let _ = presets::refresh(&config_dir, false);

                // Then bring the Explorer menu in line with whatever that left
                // behind, and with this build. Off the UI thread because it
                // touches the registry, and after the refresh so a list that
                // just changed is the one the menu is built from.
                commands::resync_shell_menu(&config_dir);
            });

            trace::mark("setup: done, waiting on the frontend");
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
            commands::use_system_ffmpeg,
            commands::system_ffmpeg,
            commands::probe_file,
            commands::add_files,
            commands::cancel_job,
            commands::cancel_all,
            commands::pause_queue,
            commands::resume_queue,
            commands::copy_to_clipboard,
            commands::reveal_in_folder,
            commands::preview_pair,
            commands::shell_menu_status,
            commands::set_shell_menu,
            commands::set_presets_url,
            commands::refresh_presets,
            updates::check_for_update,
            updates::install_update,
            commands::whats_new,
            commands::dismiss_whats_new,
            commands::changelog,
            commands::update_prefs,
            commands::set_auto_check,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_itself_is_never_treated_as_an_input() {
        let launch = parse_launch(&["media-compressor.exe"]);
        assert!(launch.is_empty());
        assert_eq!(launch.target_bytes, None);
    }

    #[test]
    fn flags_and_missing_paths_are_ignored() {
        let launch = parse_launch(&[
            "media-compressor.exe",
            "--some-flag",
            "C:/definitely/not/here.mp4",
        ]);
        assert!(launch.is_empty(), "got {launch:?}");
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
        let file = scratch_file("argv");
        let path = file.to_string_lossy().to_string();

        let launch = parse_launch(&["media-compressor.exe".to_string(), path.clone()]);

        assert_eq!(launch.files, vec![path]);
        assert_eq!(launch.target_bytes, None);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_target_is_read_in_either_spelling() {
        let file = scratch_file("target");
        let path = file.to_string_lossy().to_string();

        for argv in [
            vec!["app".to_string(), "--target".into(), "20000000".into(), path.clone()],
            vec!["app".to_string(), "--target=20000000".into(), path.clone()],
        ] {
            let launch = parse_launch(&argv);
            assert_eq!(launch.target_bytes, Some(20_000_000), "for {argv:?}");
            assert_eq!(launch.files, vec![path.clone()], "for {argv:?}");
        }

        let _ = std::fs::remove_file(&file);
    }

    /// The registry is user-editable, so the value we get back may not be the
    /// one we wrote. A nonsense size must cost the user their target, not their
    /// files.
    #[test]
    fn an_unusable_target_is_dropped_but_the_files_survive() {
        let file = scratch_file("bad-target");
        let path = file.to_string_lossy().to_string();

        for bad in ["0", "12", "not-a-number", "999999999999999"] {
            let launch = parse_launch(&[
                "app".to_string(),
                "--target".into(),
                bad.to_string(),
                path.clone(),
            ]);
            assert_eq!(launch.target_bytes, None, "{bad} should not be accepted");
            assert_eq!(launch.files, vec![path.clone()], "{bad} lost the files");
        }

        let _ = std::fs::remove_file(&file);
    }

    /// `--target` at the very end has nothing to consume, and must not swallow
    /// a path that isn't there or panic reaching for one.
    #[test]
    fn a_dangling_target_flag_is_harmless() {
        let launch = parse_launch(&["app", "--target"]);
        assert_eq!(launch.target_bytes, None);
        assert!(launch.is_empty());
    }

    fn scratch_file(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("media-compressor-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{tag}-{}.mp4", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        file
    }
}
