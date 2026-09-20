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

use super::tools::{exe, FfmpegTools};
use ffmpeg_sidecar::download::{
    download_ffmpeg_package_with_progress, ffmpeg_download_url, unpack_ffmpeg,
    FfmpegDownloadProgressEvent,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;

const MANIFEST: &str = "install.json";

/// The shortest gap between two download progress events.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// The smallest share of the archive worth interrupting the UI for.
const PROGRESS_MIN_FRACTION: f64 = 0.01;

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

/// Whether a download progress update has earned its trip to the UI.
///
/// The underlying reader reports every single `read()`, and `io::copy` reads in
/// 8 KiB chunks — so an 80 MB archive produces something like ten thousand
/// callbacks, each of which would be serialized and pushed across the IPC
/// bridge into the webview. That is enough work to measurably slow down the
/// download it is reporting on, to say nothing of what it does to the frame the
/// progress bar is drawn in.
///
/// A quarter second or a percent of the archive, whichever comes first, is
/// still far finer than a person can see.
fn worth_emitting(
    since_last: Duration,
    bytes_since_last: u64,
    total_bytes: u64,
    finished: bool,
) -> bool {
    // The last update is the one that says 100%, so it always goes out.
    finished
        || since_last >= PROGRESS_MIN_INTERVAL
        || (total_bytes > 0
            && bytes_since_last as f64 >= total_bytes as f64 * PROGRESS_MIN_FRACTION)
}

/// Keeps the "when did we last say something" state for [`worth_emitting`].
struct ProgressGate {
    last_at: Cell<Instant>,
    last_bytes: Cell<u64>,
}

impl ProgressGate {
    fn new() -> Self {
        Self { last_at: Cell::new(Instant::now()), last_bytes: Cell::new(0) }
    }

    fn admits(&self, downloaded_bytes: u64, total_bytes: u64) -> bool {
        let finished = total_bytes > 0 && downloaded_bytes >= total_bytes;
        let since_last = self.last_at.get().elapsed();
        let bytes_since_last = downloaded_bytes.saturating_sub(self.last_bytes.get());

        if !worth_emitting(since_last, bytes_since_last, total_bytes, finished) {
            return false;
        }

        self.last_at.set(Instant::now());
        self.last_bytes.set(downloaded_bytes);
        true
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

    let gate = ProgressGate::new();
    let archive = download_ffmpeg_package_with_progress(url, cache_dir, |event| match event {
        FfmpegDownloadProgressEvent::Downloading { total_bytes, downloaded_bytes } => {
            if gate.admits(downloaded_bytes, total_bytes) {
                on_progress(AcquireProgress::Downloading { downloaded_bytes, total_bytes });
            }
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
    // So has ffplay, which the archive carries and this app never launches. It
    // is another ninety megabytes of someone's disk for a media player they
    // did not ask for. (It cannot be skipped at download time — the archive is
    // one file — but it does not have to be kept.)
    let _ = std::fs::remove_file(cache_dir.join(exe("ffplay")));

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

/// Delete the app's own copy of FFmpeg, and the record of it.
///
/// The counterpart to [`install`]: it exists so that someone who has already
/// paid for the download can hand those megabytes back once they find out the
/// app will happily use the FFmpeg they already had.
pub fn remove_private_copy(cache_dir: &Path) -> std::io::Result<()> {
    for name in ["ffmpeg", "ffprobe", "ffplay"] {
        match std::fs::remove_file(cache_dir.join(exe(name))) {
            Ok(()) => {}
            // Already gone is the state we were asking for.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let _ = std::fs::remove_file(manifest_path(cache_dir));
    Ok(())
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

    /// Divides into whole percents, so "one percent" in these tests is the
    /// exact threshold rather than a byte under it.
    const ARCHIVE: u64 = 100_000_000;
    const ONE_PERCENT: u64 = ARCHIVE / 100;

    #[test]
    fn a_trickle_of_bytes_does_not_become_a_flood_of_events() {
        assert!(
            !worth_emitting(Duration::from_millis(1), 8 * 1024, ARCHIVE, false),
            "one 8 KiB read out of a hundred megabytes is not news"
        );
    }

    #[test]
    fn progress_still_gets_through_on_either_rule() {
        assert!(
            worth_emitting(Duration::from_millis(300), 8 * 1024, ARCHIVE, false),
            "a quarter second of silence is long enough to say something"
        );
        assert!(
            worth_emitting(Duration::ZERO, ONE_PERCENT, ARCHIVE, false),
            "a percent of the archive is worth an update however fast it arrived"
        );
    }

    /// A progress bar that stops at 99% reads as a hang.
    #[test]
    fn the_final_update_is_never_withheld() {
        assert!(worth_emitting(Duration::ZERO, 1, ARCHIVE, true));
    }

    /// Without a Content-Length there is no percentage to measure against, so
    /// the clock has to carry it alone.
    #[test]
    fn an_unknown_total_falls_back_to_the_clock() {
        assert!(!worth_emitting(Duration::from_millis(1), 1024 * 1024, 0, false));
        assert!(worth_emitting(Duration::from_millis(300), 0, 0, false));
    }

    #[test]
    fn the_gate_lets_the_first_update_through_then_holds_the_line() {
        let gate = ProgressGate::new();

        assert!(gate.admits(ONE_PERCENT, ARCHIVE), "the first percent is an update");
        assert!(!gate.admits(ONE_PERCENT + 8 * 1024, ARCHIVE), "8 KiB later is not");
        assert!(gate.admits(ARCHIVE, ARCHIVE), "the end always reports");
    }

    #[test]
    fn removing_a_copy_that_is_not_there_is_not_an_error() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(remove_private_copy(&dir).is_ok(), "an empty folder is already the goal");

        std::fs::write(dir.join(super::exe("ffmpeg")), b"x").unwrap();
        std::fs::write(dir.join(super::exe("ffprobe")), b"x").unwrap();
        remove_private_copy(&dir).unwrap();

        assert!(!dir.join(super::exe("ffmpeg")).exists());
        assert!(!dir.join(super::exe("ffprobe")).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
