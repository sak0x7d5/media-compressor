//! Finding `ffmpeg` and `ffprobe`.
//!
//! The shipped app uses its own copy, downloaded on first run into the user's
//! app-data directory. It deliberately does **not** fall back to whatever is on
//! PATH: system FFmpeg builds differ wildly in which encoders were compiled in,
//! which turns "libsvtav1 not found" into an unreproducible bug report.
//!
//! Two escape hatches exist for development only, both documented and both
//! off by default in release builds.

use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Points the app at a specific FFmpeg build. Useful when bisecting an encoder
/// bug against a different version; honoured in every build.
pub const FFMPEG_OVERRIDE_ENV: &str = "MEDIA_COMPRESSOR_FFMPEG";

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

fn exe(stem: &str) -> String {
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
}

impl FfmpegTools {
    /// Both binaries in one directory, whether or not they exist yet.
    fn in_dir(dir: &Path) -> Self {
        Self { ffmpeg: dir.join(exe("ffmpeg")), ffprobe: dir.join(exe("ffprobe")) }
    }

    /// The paths a fresh install will occupy. Used by the installer, which
    /// needs to name them before they exist.
    pub fn in_cache(cache_dir: &Path) -> Self {
        Self::in_dir(cache_dir)
    }

    fn both_exist(&self) -> bool {
        self.ffmpeg.is_file() && self.ffprobe.is_file()
    }

    /// Find a usable pair, in preference order.
    ///
    /// 1. The app's own download in `cache_dir` — the normal case.
    /// 2. `MEDIA_COMPRESSOR_FFMPEG`, pointing at a directory containing both.
    /// 3. PATH, **debug builds only**, so `cargo run` works on a dev machine
    ///    without waiting for a download.
    pub fn locate(cache_dir: &Path) -> Result<Self, ToolsError> {
        let cached = Self::in_dir(cache_dir);
        if cached.both_exist() {
            return Ok(cached);
        }

        if let Some(dir) = std::env::var_os(FFMPEG_OVERRIDE_ENV) {
            let overridden = Self::in_dir(Path::new(&dir));
            if overridden.both_exist() {
                return Ok(overridden);
            }
        }

        if cfg!(debug_assertions) {
            if let Some(found) = Self::from_path_env() {
                return Ok(found);
            }
        }

        Err(ToolsError::NotInstalled)
    }

    /// Scan PATH for both binaries. Debug convenience only — see the module doc.
    fn from_path_env() -> Option<Self> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = Self::in_dir(&dir);
            if candidate.both_exist() {
                return Some(candidate);
            }
        }
        None
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
    ///
    /// This costs a process spawn, and the binary is around 100 MB: the first
    /// run after a boot waits for Windows Defender to read all of it, which
    /// takes seconds. Never call this from a thread that has to stay
    /// responsive.
    pub fn version(&self) -> Option<String> {
        let mut command = Command::new(&self.ffmpeg);
        command.arg("-version");
        super::hide_console(&mut command);

        let output = command.output().ok()?;
        let banner = String::from_utf8_lossy(&output.stdout);
        banner.lines().next().map(|line| line.trim().to_string())
    }
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

    #[test]
    fn an_empty_cache_reports_not_installed() {
        let dir = temp_dir("empty");
        // The PATH fallback is compiled in for debug builds, and a dev machine
        // may genuinely have FFmpeg installed, so only assert the negative when
        // the fallback cannot fire.
        match FfmpegTools::locate(&dir) {
            Err(ToolsError::NotInstalled) => {}
            Ok(found) => assert!(
                cfg!(debug_assertions) && found.ffmpeg.parent() != Some(dir.as_path()),
                "an empty cache must not resolve to the cache itself"
            ),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn a_populated_cache_wins_over_everything_else() {
        let dir = temp_dir("populated");
        fake_install(&dir);

        let found = FfmpegTools::locate(&dir).expect("cache should be found");
        assert_eq!(found.ffmpeg, dir.join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, dir.join(exe("ffprobe")));
    }

    #[test]
    fn a_half_installed_cache_is_not_accepted() {
        let dir = temp_dir("half");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(exe("ffmpeg")), b"only ffmpeg").unwrap();

        // ffprobe is missing, so this directory must not be chosen.
        if let Ok(found) = FfmpegTools::locate(&dir) {
            assert_ne!(
                found.ffmpeg,
                dir.join(exe("ffmpeg")),
                "a directory missing ffprobe must not be accepted"
            );
        }
    }

    #[test]
    fn verify_rejects_files_that_are_not_really_binaries() {
        let dir = temp_dir("bogus");
        fake_install(&dir);

        let tools = FfmpegTools::locate(&dir).unwrap();
        assert!(tools.verify().is_err(), "a text file named ffmpeg.exe must not pass verify");
    }
}
