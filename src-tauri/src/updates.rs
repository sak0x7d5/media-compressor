//! Self-updating.
//!
//! The app asks a small file on the release host what the newest version is,
//! and installs it if that is newer than what is running. Every update must
//! carry a signature made with the project's private key; the matching public
//! key is compiled into the binary, so a compromised download host cannot push
//! anything the app will accept.
//!
//! ## Dormant until releases exist
//!
//! The endpoint points at GitHub releases for a repository that is currently
//! private, so the check simply finds nothing. That is deliberate and harmless:
//! the capability has to be inside the build people already have, or it can
//! never reach them. Making the repository public is the only step left to turn
//! this on — no code change.

use serde::Serialize;
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
        Err(error) => Err(format!("could not check for updates: {error}")),
    }
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
        .map_err(|e| format!("could not check for updates: {e}"))?
        .ok_or_else(|| "there is no update to install".to_string())?;

    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| format!("could not install the update: {e}"))?;

    // The installer has replaced the files on disk; restarting is what actually
    // moves the user onto the new version.
    app.restart();
}

/// Check quietly in the background at startup.
///
/// Failure is silent by design. Someone compressing a video does not need a
/// dialog because a release server was unreachable, and while the repository is
/// private this is expected to find nothing every time.
pub fn check_in_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Ok(updater) = app.updater() {
            let _ = updater.check().await;
        }
    });
}
