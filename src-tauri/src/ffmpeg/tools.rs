//! Finding `ffmpeg` and `ffprobe`.
//!
//! Three places are searched, in this order:
//!
//! 1. the app's own download in its app-data folder;
//! 2. `MEDIA_COMPRESSOR_FFMPEG`, pointing at a directory containing both;
//! 3. PATH — a build the user already has.
//!
//! The third one used to be compiled into debug builds only. The reasoning was
//! that system FFmpeg builds differ in which encoders were compiled in, so
//! trusting one turns "unknown encoder libsvtav1" into an unreproducible bug
//! report. That reasoning is sound, but the conclusion was too broad: it threw
//! away a perfectly good binary rather than spending one process asking whether
//! it was good. So PATH is now searched in every build, and what it finds is
//! adopted only if it can actually encode everything this app encodes — see
//! [`FfmpegTools::missing_encoders`] and [`super::system::adopt`].
//!
//! The env override is deliberately *not* held to that bar. Its whole purpose
//! is pointing the app at an unusual build on purpose, and a gate there would
//! block the one job it has.

use crate::strategy::VideoCodec;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Points the app at a specific FFmpeg build. Useful when bisecting an encoder
/// bug against a different version; honoured in every build, and exempt from
/// the encoder check.
pub const FFMPEG_OVERRIDE_ENV: &str = "MEDIA_COMPRESSOR_FFMPEG";

/// Where the binaries in use came from. Shown in Settings, because "which
/// FFmpeg is this actually running?" is otherwise unanswerable from the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolsSource {
    /// The app's own download, in its app-data folder.
    Private,
    /// A directory named by `MEDIA_COMPRESSOR_FFMPEG`.
    Override,
    /// A build already on the user's PATH, checked for the encoders we need.
    System,
}

#[derive(Debug, Error)]
pub enum ToolsError {
    #[error("FFmpeg is not installed yet")]
    NotInstalled,

    #[error("found {kind} at {} but it would not run: {source}", path.display())]
    NotRunnable {
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("found {kind} at {} but it reported an error", path.display())]
    Unhealthy { kind: &'static str, path: PathBuf },
}

/// The platform's name for an executable.
pub(crate) fn exe(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// A located, matched pair of binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub source: ToolsSource,
}

impl FfmpegTools {
    /// Both binaries in one directory, whether or not they exist yet.
    fn in_dir(dir: &Path, source: ToolsSource) -> Self {
        Self {
            ffmpeg: dir.join(exe("ffmpeg")),
            ffprobe: dir.join(exe("ffprobe")),
            source,
        }
    }

    /// The paths a fresh install will occupy. Used by the installer, which
    /// needs to name them before they exist.
    pub fn in_cache(cache_dir: &Path) -> Self {
        Self::in_dir(cache_dir, ToolsSource::Private)
    }

    fn both_exist(&self) -> bool {
        self.ffmpeg.is_file() && self.ffprobe.is_file()
    }

    /// Find a pair without running anything.
    ///
    /// Filesystem checks only, because this sits on the startup path. Adopting
    /// a build from PATH costs a process and is handled separately, by
    /// [`super::system::adopt`].
    pub fn locate(cache_dir: &Path) -> Result<Self, ToolsError> {
        let cached = Self::in_dir(cache_dir, ToolsSource::Private);
        if cached.both_exist() {
            return Ok(cached);
        }

        if let Some(dir) = std::env::var_os(FFMPEG_OVERRIDE_ENV) {
            let overridden = Self::in_dir(Path::new(&dir), ToolsSource::Override);
            if overridden.both_exist() {
                return Ok(overridden);
            }
        }

        Err(ToolsError::NotInstalled)
    }

    /// The first directory on PATH holding both binaries. Filesystem only —
    /// whether this build is *usable* is a separate question.
    pub fn on_path() -> Option<Self> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = Self::in_dir(&dir, ToolsSource::System);
            if candidate.both_exist() {
                return Some(candidate);
            }
        }
        None
    }

    /// Which of the encoders this app needs are missing from this build.
    ///
    /// An empty vector means the build can encode everything we would ever ask
    /// it for. This is the check that makes trusting a stranger's FFmpeg
    /// reasonable: the failure it prevents otherwise surfaces minutes later, as
    /// a dead job on a file the user already dropped.
    pub fn missing_encoders(&self) -> Result<Vec<&'static str>, ToolsError> {
        let mut command = Command::new(&self.ffmpeg);
        command.args(["-hide_banner", "-encoders"]);
        super::hide_console(&mut command);

        let output = command.output().map_err(|source| ToolsError::NotRunnable {
            kind: "ffmpeg",
            path: self.ffmpeg.clone(),
            source,
        })?;

        if !output.status.success() {
            return Err(ToolsError::Unhealthy { kind: "ffmpeg", path: self.ffmpeg.clone() });
        }

        let listing = String::from_utf8_lossy(&output.stdout);
        Ok(VideoCodec::ALL
            .iter()
            .map(|codec| codec.ffmpeg_encoder())
            .filter(|encoder| !lists_encoder(&listing, encoder))
            .collect())
    }

    /// Actually run both binaries. Existence is not usability: a truncated
    /// download or a half-extracted archive leaves files of the right name that
    /// fail the moment they are executed.
    pub fn verify(&self) -> Result<(), ToolsError> {
        Self::verify_one("ffmpeg", &self.ffmpeg)?;
        Self::verify_one("ffprobe", &self.ffprobe)
    }

    fn verify_one(kind: &'static str, path: &Path) -> Result<(), ToolsError> {
        let mut command = Command::new(path);
        command.arg("-version");
        super::hide_console(&mut command);

        let output = command
            .output()
            .map_err(|source| ToolsError::NotRunnable { kind, path: path.to_path_buf(), source })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(ToolsError::Unhealthy { kind, path: path.to_path_buf() })
        }
    }

    /// The version banner's first line, for display in Settings.
    pub fn version(&self) -> Option<String> {
        let mut command = Command::new(&self.ffmpeg);
        command.arg("-version");
        super::hide_console(&mut command);

        let output = command.output().ok()?;
        let banner = String::from_utf8_lossy(&output.stdout);
        banner.lines().next().map(|line| line.trim().to_string())
    }
}

/// Whether `ffmpeg -encoders` listed this encoder.
///
/// Each row is `FLAGS name description`, and only the name column counts. The
/// description column mentions other encoders by name — the `libx264rgb` row
/// says "libx264" in its description — so a substring search over the whole
/// line would report an encoder present on the strength of a neighbour's
/// description.
fn lists_encoder(listing: &str, encoder: &str) -> bool {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .any(|name| name == encoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory holding files named like the binaries, so `locate` finds
    /// them without anything being executable.
    fn fake_install(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(exe("ffmpeg")), b"not really ffmpeg").unwrap();
        std::fs::write(dir.join(exe("ffprobe")), b"not really ffprobe").unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two rows in the shape `ffmpeg -encoders` actually prints, including the
    /// header it prints above them.
    const LISTING: &str = "Encoders:\n\
         V..... = Video\n\
         ------\n\
         V....D libx264              libx264 H.264 / AVC (codec h264)\n\
         V....D libx264rgb           libx264 H.264 / AVC RGB (codec h264)\n\
         V....D libsvtav1            SVT-AV1 encoder (codec av1)\n";

    #[test]
    fn an_empty_cache_reports_not_installed() {
        let dir = temp_dir("empty");
        // `locate` no longer consults PATH, so this is unconditional now.
        assert!(matches!(FfmpegTools::locate(&dir), Err(ToolsError::NotInstalled)));
    }

    #[test]
    fn a_populated_cache_wins_over_everything_else() {
        let dir = temp_dir("populated");
        fake_install(&dir);

        let found = FfmpegTools::locate(&dir).expect("cache should be found");
        assert_eq!(found.ffmpeg, dir.join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, dir.join(exe("ffprobe")));
        assert_eq!(found.source, ToolsSource::Private);
    }

    #[test]
    fn a_half_installed_cache_is_not_accepted() {
        let dir = temp_dir("half");
        std::fs::write(dir.join(exe("ffmpeg")), b"only ffmpeg").unwrap();

        // ffprobe is missing, so this directory must not be chosen.
        assert!(matches!(FfmpegTools::locate(&dir), Err(ToolsError::NotInstalled)));
    }

    #[test]
    fn verify_rejects_files_that_are_not_really_binaries() {
        let dir = temp_dir("bogus");
        fake_install(&dir);

        let tools = FfmpegTools::locate(&dir).unwrap();
        assert!(tools.verify().is_err(), "a text file named ffmpeg.exe must not pass verify");
    }

    #[test]
    fn an_encoder_is_found_by_its_own_row() {
        assert!(lists_encoder(LISTING, "libx264"));
        assert!(lists_encoder(LISTING, "libsvtav1"));
    }

    /// The whole reason for parsing the name column rather than the line: the
    /// `libx264rgb` row names libx264 in its description, and libx265 appears
    /// nowhere at all.
    #[test]
    fn a_description_mentioning_an_encoder_does_not_count_as_having_it() {
        assert!(!lists_encoder(LISTING, "libx265"));

        let rgb_only = "V....D libx264rgb           libx264 H.264 / AVC RGB (codec h264)\n";
        assert!(
            !lists_encoder(rgb_only, "libx264"),
            "libx264rgb's description must not stand in for libx264 itself"
        );
    }

    /// Adding a codec to the enum has to widen the check automatically,
    /// otherwise the two drift and the drift is only found at encode time.
    #[test]
    fn every_codec_the_app_offers_is_checked_for() {
        for codec in VideoCodec::ALL {
            let encoder = codec.ffmpeg_encoder();
            let row = format!("V....D {encoder}              something (codec x)\n");
            assert!(lists_encoder(&row, encoder), "{encoder} must be recognisable");
            assert!(!lists_encoder(LISTING, "definitely-not-an-encoder"));
        }
    }
}
