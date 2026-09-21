//! Self-updating.
//!
//! The app asks a small file on the release host what the newest version is,
//! and installs it if that is newer than what is running. Every update must
//! carry a signature made with the project's private key; the matching public
//! key is compiled into the binary, so a compromised download host cannot push
//! anything the app will accept.
//!
//! ## Where releases come from
//!
//! The endpoint is the `latest.json` attached to the newest *published* GitHub
//! release. The release workflow creates every release as a draft, and GitHub
//! serves nothing from a draft, so until someone publishes it the check comes
//! back empty-handed and says so. That is the whole reason the updater ships
//! before there is anything to update to: the capability has to be inside the
//! build people already have, or a newer version can never reach them.

use serde::Serialize;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// What is available, when something is.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
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
/// Returns only on failure — a success replaces this process.
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

    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| format!("could not install the update: {e}"))?;

    // The installer has replaced the files on disk; restarting is what actually
    // moves the user onto the new version.
    app.restart();
}

/// How long to leave startup alone before asking about updates.
///
/// A cold DNS lookup and TLS handshake want the same moments the window is
/// trying to appear in, and nothing about an update is urgent enough to
/// compete for them.
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// Check quietly in the background, shortly after startup.
///
/// Failure is silent by design. Someone compressing a video does not need a
/// dialog because a release server was unreachable; the settings panel is where
/// a failed check gets explained, when someone asks.
pub fn check_in_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        if let Ok(updater) = app.updater() {
            let _ = updater.check().await;
        }
    });
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
}
