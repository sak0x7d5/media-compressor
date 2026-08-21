//! Getting FFmpeg onto the machine on first run.
//!
//! ## On integrity checking
//!
//! The plan called for verifying a published SHA-256. That is not achievable
//! against the upstream distribution as it actually exists: the download URL is
//! a rolling "latest" build that is replaced in place, and no per-build hash is
//! published alongside it. Pinning a hash would mean pinning a URL that stops
//! working, and a hash we computed ourselves proves only that the file did not
//! change since *we* looked — not that it is what the vendor shipped.
//!
//! So what is actually enforced here:
//!
//! - the download happens over HTTPS from the known upstream host;
//! - the extracted binaries must **execute successfully** before the install is
//!   accepted, which is what catches a truncated or corrupted download;
//! - the archive hash and resolved version are recorded in a manifest, so a
//!   later launch can detect that the cached install changed underneath us, and
//!   so the value can be compared against upstream by hand if anyone wants to.
//!
//! That is a weaker guarantee than the plan implied, and it is stated plainly
//! rather than dressed up.

use super::tools::FfmpegTools;
use ffmpeg_sidecar::download::{
    download_ffmpeg_package_with_progress, ffmpeg_download_url, unpack_ffmpeg,
    FfmpegDownloadProgressEvent,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MANIFEST: &str = "install.json";

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum AcquireProgress {
    Starting,
    Downloading { downloaded_bytes: u64, total_bytes: u64 },
    Unpacking,
    Verifying,
    Done,
}

/// What we installed, recorded next to the binaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub source_url: String,
    pub archive_sha256: String,
    pub version: Option<String>,
    pub installed_at: String,
}

#[derive(Debug, Error)]
pub enum AcquireError {
    #[error("this platform has no published FFmpeg build: {0}")]
    UnsupportedPlatform(String),

    #[error("download failed: {0}")]
    Download(String),

    #[error("could not unpack the download: {0}")]
    Unpack(String),

    #[error("the download completed but the binaries would not run: {0}")]
    Unusable(#[from] super::tools::ToolsError),

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_iso8601() -> String {
    // Good enough for a provenance note; not worth a date-time dependency.
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => format!("unix:{}", elapsed.as_secs()),
        Err(_) => "unknown".to_string(),
    }
}

pub fn manifest_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(MANIFEST)
}

pub fn read_manifest(cache_dir: &Path) -> Option<InstallManifest> {
    let raw = std::fs::read_to_string(manifest_path(cache_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Download and install FFmpeg into `cache_dir`, replacing whatever is there.
pub fn install(
    cache_dir: &Path,
    on_progress: impl Fn(AcquireProgress),
) -> Result<FfmpegTools, AcquireError> {
    std::fs::create_dir_all(cache_dir)?;
    on_progress(AcquireProgress::Starting);

    let url = ffmpeg_download_url().map_err(|e| AcquireError::UnsupportedPlatform(e.to_string()))?;

    let archive = download_ffmpeg_package_with_progress(url, cache_dir, |event| match event {
        FfmpegDownloadProgressEvent::Downloading { total_bytes, downloaded_bytes } => {
            on_progress(AcquireProgress::Downloading { downloaded_bytes, total_bytes });
        }
        FfmpegDownloadProgressEvent::UnpackingArchive => on_progress(AcquireProgress::Unpacking),
        FfmpegDownloadProgressEvent::Starting => on_progress(AcquireProgress::Starting),
        FfmpegDownloadProgressEvent::Done => {}
    })
    .map_err(|e| AcquireError::Download(e.to_string()))?;

    let archive_sha256 = sha256_file(&archive).unwrap_or_else(|_| "unavailable".to_string());

    on_progress(AcquireProgress::Unpacking);
    unpack_ffmpeg(&archive, cache_dir).map_err(|e| AcquireError::Unpack(e.to_string()))?;
    // The archive is tens of megabytes and has served its purpose.
    let _ = std::fs::remove_file(&archive);

    on_progress(AcquireProgress::Verifying);
    let tools = FfmpegTools::in_cache(cache_dir);
    // This is the check that matters: a half-extracted or truncated binary has
    // the right name and the right size and fails the instant it is executed.
    tools.verify()?;

    let manifest = InstallManifest {
        source_url: url.to_string(),
        archive_sha256,
        version: tools.version(),
        installed_at: now_iso8601(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::write(manifest_path(cache_dir), json);
    }

    on_progress(AcquireProgress::Done);
    Ok(tools)
}

/// Return a working install, downloading one only if there isn't one already.
pub fn ensure(
    cache_dir: &Path,
    on_progress: impl Fn(AcquireProgress),
) -> Result<FfmpegTools, AcquireError> {
    if let Ok(existing) = FfmpegTools::locate(cache_dir) {
        // Existence is not health. A cached install that no longer runs — an
        // interrupted download, an antivirus quarantine — should be replaced
        // rather than reported as working.
        if existing.verify().is_ok() {
            return Ok(existing);
        }
    }
    install(cache_dir, on_progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_is_stable_and_content_dependent() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        std::fs::write(&a, b"the same bytes").unwrap();
        std::fs::write(&b, b"the same bytes").unwrap();

        let hash_a = sha256_file(&a).unwrap();
        assert_eq!(hash_a, sha256_file(&b).unwrap(), "identical content must hash identically");
        assert_eq!(hash_a.len(), 64, "sha-256 renders as 64 hex characters");

        std::fs::write(&b, b"different bytes").unwrap();
        assert_ne!(hash_a, sha256_file(&b).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_manifest_is_absent_rather_than_an_error() {
        assert!(read_manifest(Path::new("nowhere-at-all")).is_none());
    }
}
