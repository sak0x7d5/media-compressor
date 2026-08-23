//! Self-updating, and deciding what the user has not yet been told.
//!
//! The app asks a small file on the release host what the newest version is,
//! and installs it if that is newer than what is running. Every update must
//! carry a signature made with the project's private key; the matching public
//! key is compiled into the binary, so a compromised download host cannot push
//! anything the app will accept.
//!
//! The download-and-install half is `tauri-plugin-updater`'s job. What lives
//! here is the part that plugin has no opinion about — whether to look at all,
//! how to explain a check that came back empty, and which changelog entries
//! are new to *this* profile. That last decision is pure and tested. Getting it
//! wrong is not a crash, it is the far more annoying failure of an app that
//! greets you with release notes every single launch.
//!
//! ## Where releases come from
//!
//! The endpoint is the `latest.json` attached to the newest *published* GitHub
//! release. The release workflow creates every release as a draft, and GitHub
//! serves nothing from a draft, so until someone publishes it the check comes
//! back empty-handed and says so. That is the whole reason the updater ships
//! before there is anything to update to: the capability has to be inside the
//! build people already have, or a newer version can never reach them.

use crate::changelog::{self, Release};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

/// Event channel for [`UpdateProgress`], mirrored in `ipc.ts`.
pub const EVENT_UPDATE: &str = "update-progress";

/// What is available, when something is.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

/// How far an install has got, for the bar that started it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum UpdateProgress {
    Downloading { received_bytes: u64, total_bytes: Option<u64> },
    /// Downloaded and verified; the installer is running. On Windows it closes
    /// the app to replace it, so this is often the last thing the UI hears.
    Installing,
}

/// Ask whether a newer version exists.
///
/// `Ok(None)` means "you are up to date"; an `Err` means we could not find out,
/// which is a different thing and is reported as such rather than being passed
/// off as good news.
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app
        .updater()
        .map_err(|e| format!("the updater is not configured: {e}"))?;

    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            notes: update.body.clone(),
            date: update.date.map(|date| date.to_string()),
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(describe(&error)),
    }
}

/// Say what went wrong in words the settings panel can show.
///
/// The plugin uses the same jargon for "the server answered, but not with a
/// release" — a 404 because nothing is published yet — as for a request that
/// never got through, and those call for different reactions. Each message
/// states only what was observed; why it happened is not something the app
/// can know, so it does not guess.
fn describe(error: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;

    match error {
        Error::ReleaseNotFound => "the update server answered, but has no release to offer".into(),
        Error::Reqwest(e) if e.is_request() => {
            format!("couldn't reach the update server: {}", root_cause(e))
        }
        other => format!("couldn't check for updates: {}", root_cause(other)),
    }
}

/// The innermost cause, which is the one that says something useful. `reqwest`
/// itself only reports "error sending request" and leaves the DNS failure or
/// timeout at the bottom of the chain.
fn root_cause(error: &dyn std::error::Error) -> String {
    let mut cause = error;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

/// Download and install the available update, then restart into it.
///
/// Progress goes out on [`EVENT_UPDATE`]. Returns only on failure — a success
/// replaces this process.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|e| format!("the updater is not configured: {e}"))?;

    let update = updater
        .check()
        .await
        .map_err(|e| describe(&e))?
        .ok_or_else(|| "there is no update to install".to_string())?;

    // Chunks arrive every few kilobytes. Reporting each one would spend more
    // on IPC than on the download, so a report goes out when the percentage
    // moves — or every quarter megabyte when the size is unknown.
    let mut received: u64 = 0;
    let mut reported = Reported::default();
    let on_chunk = {
        let app = app.clone();
        move |chunk: usize, total: Option<u64>| {
            received += chunk as u64;
            if reported.worth_sending(received, total) {
                let _ = app.emit(
                    EVENT_UPDATE,
                    UpdateProgress::Downloading { received_bytes: received, total_bytes: total },
                );
            }
        }
    };
    let on_downloaded = {
        let app = app.clone();
        move || {
            let _ = app.emit(EVENT_UPDATE, UpdateProgress::Installing);
        }
    };

    update
        .download_and_install(on_chunk, on_downloaded)
        .await
        .map_err(|e| format!("could not install the update: {e}"))?;

    // The installer has replaced the files on disk; restarting is what actually
    // moves the user onto the new version.
    app.restart();
}

/// The last progress report that went out, so the next one can be judged
/// against it.
#[derive(Default)]
struct Reported {
    percent: Option<u64>,
    bytes: u64,
}

impl Reported {
    /// Bytes between reports when the total is unknown and there is no
    /// percentage to move.
    const STRIDE: u64 = 256 * 1024;

    fn worth_sending(&mut self, received: u64, total: Option<u64>) -> bool {
        let send = match total.filter(|total| *total > 0) {
            Some(total) => {
                let percent = received * 100 / total;
                let moved = self.percent != Some(percent);
                self.percent = Some(percent);
                moved
            }
            None => received - self.bytes >= Self::STRIDE || self.bytes == 0,
        };
        if send {
            self.bytes = received;
        }
        send
    }
}

/// What this profile has chosen and already seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    /// The newest version whose notes this profile has already been shown.
    /// `None` means a fresh install — nobody wants a changelog as a greeting.
    #[serde(default)]
    pub last_seen_version: Option<String>,
    /// Look for a new release shortly after launch. On by default: an updater
    /// nobody remembers to trigger is a version number that never moves.
    #[serde(default = "enabled")]
    pub auto_check: bool,
}

fn enabled() -> bool {
    true
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self { last_seen_version: None, auto_check: true }
    }
}

pub fn prefs_path(config_dir: &Path) -> PathBuf {
    config_dir.join("updates.json")
}

/// Load preferences, falling back to defaults rather than failing.
///
/// A file that will not parse costs the user their auto-check preference. It
/// must not cost them the ability to update.
pub fn load(config_dir: &Path) -> UpdatePrefs {
    std::fs::read_to_string(prefs_path(config_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(config_dir: &Path, prefs: &UpdatePrefs) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(prefs_path(config_dir), json)
}

/// Which entries to show on this launch.
///
/// Empty means "say nothing", which covers three cases that all deserve
/// silence: a first install, a version already seen, and a downgrade — where
/// the notes would describe changes that just went *away*.
pub fn unseen(last_seen: Option<&str>, current: &str, releases: &[Release]) -> Vec<Release> {
    let Some(last_seen) = last_seen else {
        return Vec::new();
    };
    changelog::since(releases, last_seen)
        .into_iter()
        .filter(|release| !changelog::is_newer(&release.version, current))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_release_is_not_blamed_on_anything() {
        let message = describe(&tauri_plugin_updater::Error::ReleaseNotFound);
        assert_eq!(message, "the update server answered, but has no release to offer");
    }

    #[test]
    fn the_root_cause_is_the_innermost_error() {
        let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "no such host");
        let outer = std::io::Error::other(inner);
        assert_eq!(root_cause(&outer), "no such host");
    }

    #[test]
    fn an_error_without_a_cause_speaks_for_itself() {
        let message = describe(&tauri_plugin_updater::Error::UnsupportedOs);
        assert!(message.starts_with("couldn't check for updates: Unsupported OS"), "{message}");
    }

    #[test]
    fn progress_is_reported_once_per_percent_when_the_size_is_known() {
        let mut reported = Reported::default();
        let total = Some(10_000);

        assert!(reported.worth_sending(0, total), "the first report always goes out");
        assert!(!reported.worth_sending(50, total), "still 0%");
        assert!(reported.worth_sending(100, total), "1%");
        assert!(!reported.worth_sending(150, total));
        assert!(reported.worth_sending(10_000, total), "100%");
    }

    #[test]
    fn progress_is_reported_by_distance_when_the_size_is_unknown() {
        let mut reported = Reported::default();

        assert!(reported.worth_sending(8_192, None), "the first report always goes out");
        assert!(!reported.worth_sending(16_384, None));
        assert!(reported.worth_sending(8_192 + Reported::STRIDE, None));
        assert!(!reported.worth_sending(8_192 + Reported::STRIDE + 1, None));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("updates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HISTORY: &str = "\
## [0.3.0] — 2026-09-01
- Three.

## [0.2.0] — 2026-08-21
- Two.

## [0.1.0] — 2026-08-01
- One.
";

    fn history() -> Vec<Release> {
        changelog::parse(HISTORY)
    }

    #[test]
    fn a_fresh_install_is_not_greeted_with_a_changelog() {
        assert!(unseen(None, "0.3.0", &history()).is_empty());
    }

    #[test]
    fn a_version_already_seen_says_nothing() {
        assert!(unseen(Some("0.3.0"), "0.3.0", &history()).is_empty());
    }

    #[test]
    fn an_update_shows_every_entry_that_was_skipped() {
        let shown = unseen(Some("0.1.0"), "0.3.0", &history());

        let versions: Vec<&str> = shown.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["0.3.0", "0.2.0"], "newest first, and 0.1.0 was already seen");
    }

    #[test]
    fn notes_for_versions_this_build_does_not_contain_are_withheld() {
        // The changelog is compiled in, so it can describe a release newer than
        // the binary reading it — an installer that failed halfway, or a
        // deliberate rollback. Promising features that are not here is worse
        // than saying nothing.
        let shown = unseen(Some("0.1.0"), "0.2.0", &history());

        let versions: Vec<&str> = shown.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["0.2.0"]);
    }

    #[test]
    fn a_downgrade_says_nothing() {
        assert!(unseen(Some("0.3.0"), "0.1.0", &history()).is_empty());
    }

    #[test]
    fn preferences_survive_a_round_trip() {
        let dir = temp_dir("round-trip");
        let prefs = UpdatePrefs {
            last_seen_version: Some("0.2.0".to_string()),
            auto_check: false,
        };

        save(&dir, &prefs).unwrap();
        assert_eq!(load(&dir), prefs);
    }

    #[test]
    fn the_launch_check_is_on_until_it_is_turned_off() {
        let dir = temp_dir("default");
        assert!(load(&dir).auto_check, "an unconfigured profile should still be offered updates");
        assert_eq!(load(&dir).last_seen_version, None);
    }

    #[test]
    fn a_corrupt_preferences_file_does_not_disable_updating() {
        let dir = temp_dir("corrupt");
        std::fs::write(prefs_path(&dir), "{ not json at all").unwrap();

        assert_eq!(load(&dir), UpdatePrefs::default());
    }
}
