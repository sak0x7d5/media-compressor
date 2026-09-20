//! Deciding whether the FFmpeg the user already has is good enough to use.
//!
//! Plenty of machines already carry a perfectly capable FFmpeg, and downloading
//! a second copy of it is eighty megabytes and a wait for nothing. The catch is
//! that "an ffmpeg" is not one thing: builds differ in which encoders were
//! compiled in, and one missing encoder turns into a job that dies minutes
//! later on a file the user already dropped.
//!
//! So a build on PATH is adopted only after it has been asked, out loud, what
//! it can encode. That costs one process — and this module makes sure it costs
//! one process *ever*, not one per launch, by remembering the answer next to
//! the binary's size and timestamp. A build that changes underneath us fails
//! that match and gets asked again.

use super::tools::FfmpegTools;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Sits beside `install.json` in the app's FFmpeg folder.
const VERDICT: &str = "system.json";

/// How long a build on PATH gets to answer before we stop waiting on it.
///
/// [`adopt`] runs during startup, before the window is on screen, and an
/// `ffmpeg` on a disconnected network drive or meeting an antivirus for the
/// first time can take a very long time to say anything. Giving up costs this
/// launch the adoption — it falls back to asking, which is the behaviour that
/// existed before any of this — and that is a far better outcome than a window
/// that does not appear.
///
/// The work is not cancelled, only stopped being waited on. It finishes in its
/// own time and writes the answer down, so the next launch reads it for free.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the app found on PATH. Reported to the UI so that "there is one, but
/// it can't do AV1" is sayable, rather than silently downloading.
#[derive(Debug, Clone, Serialize)]
pub struct SystemBuild {
    pub location: String,
    pub usable: bool,
    pub missing_encoders: Vec<String>,
}

/// A remembered answer, valid only for the exact binaries it was taken from.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Verdict {
    ffmpeg: String,
    ffmpeg_len: u64,
    ffmpeg_modified: Option<u64>,
    ffprobe_len: u64,
    /// False when the binary would not run at all. Worth remembering: a
    /// quarantined or stub `ffmpeg.exe` on PATH would otherwise be re-launched
    /// on every startup, which is the slowest possible way to learn nothing.
    runs: bool,
    /// Empty when the build has every encoder the app can ask for. Anything
    /// listed here is why it was turned down, in a form a bug report can quote.
    missing_encoders: Vec<String>,
}

impl Verdict {
    fn usable(&self) -> bool {
        self.runs && self.missing_encoders.is_empty()
    }

    /// Whether this answer was taken from the binaries sitting there now.
    ///
    /// Size and mtime rather than a hash: hashing 90 MB on every launch would
    /// cost more than the process this cache exists to avoid.
    fn still_describes(&self, tools: &FfmpegTools, print: &Fingerprint) -> bool {
        self.ffmpeg == tools.ffmpeg.to_string_lossy()
            && self.ffmpeg_len == print.ffmpeg_len
            && self.ffmpeg_modified == print.ffmpeg_modified
            && self.ffprobe_len == print.ffprobe_len
    }
}

struct Fingerprint {
    ffmpeg_len: u64,
    ffmpeg_modified: Option<u64>,
    ffprobe_len: u64,
}

fn modified_secs(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

fn fingerprint(tools: &FfmpegTools) -> Option<Fingerprint> {
    let ffmpeg = std::fs::metadata(&tools.ffmpeg).ok()?;
    let ffprobe = std::fs::metadata(&tools.ffprobe).ok()?;

    Some(Fingerprint {
        ffmpeg_len: ffmpeg.len(),
        ffmpeg_modified: modified_secs(&ffmpeg),
        ffprobe_len: ffprobe.len(),
    })
}

fn verdict_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(VERDICT)
}

fn read_verdict(cache_dir: &Path) -> Option<Verdict> {
    let raw = std::fs::read_to_string(verdict_path(cache_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_verdict(cache_dir: &Path, verdict: &Verdict) {
    // Best effort throughout: failing to cache the answer costs a process on
    // the next launch, which is not worth failing startup over.
    let _ = std::fs::create_dir_all(cache_dir);
    if let Ok(json) = serde_json::to_string_pretty(verdict) {
        let _ = std::fs::write(verdict_path(cache_dir), json);
    }
}

/// Ask this build what it can encode, or recall the last time we asked.
fn check(cache_dir: &Path, tools: &FfmpegTools) -> Verdict {
    let print = fingerprint(tools);

    if let (Some(print), Some(remembered)) = (&print, read_verdict(cache_dir)) {
        if remembered.still_describes(tools, print) {
            return remembered;
        }
    }

    let (runs, missing_encoders) = match tools.missing_encoders() {
        Ok(missing) => (true, missing.into_iter().map(str::to_string).collect()),
        // Either it would not start or it answered with an error. Both mean the
        // same thing here, and neither is worth asking twice.
        Err(_) => (false, Vec::new()),
    };

    let verdict = Verdict {
        ffmpeg: tools.ffmpeg.to_string_lossy().to_string(),
        ffmpeg_len: print.as_ref().map(|p| p.ffmpeg_len).unwrap_or_default(),
        ffmpeg_modified: print.as_ref().and_then(|p| p.ffmpeg_modified),
        ffprobe_len: print.as_ref().map(|p| p.ffprobe_len).unwrap_or_default(),
        runs,
        missing_encoders,
    };

    // Only worth remembering if the fingerprint it is keyed on is real.
    if print.is_some() {
        write_verdict(cache_dir, &verdict);
    }
    verdict
}

/// The build on PATH, if there is one that can encode everything we ask for.
///
/// Bounded by [`PROBE_TIMEOUT`], because this runs before the window is shown.
/// A remembered answer needs no process at all and returns immediately.
pub fn adopt(cache_dir: &Path) -> Option<FfmpegTools> {
    let candidate = FfmpegTools::on_path()?;

    let (tx, rx) = std::sync::mpsc::channel();
    let probing = candidate.clone();
    let dir = cache_dir.to_path_buf();
    std::thread::Builder::new()
        .name("ffmpeg-probe".to_string())
        .spawn(move || {
            // A closed channel means startup stopped waiting. The answer is
            // still worth writing down, which `check` has already done.
            let _ = tx.send(check(&dir, &probing).usable());
        })
        .ok()?;

    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(true) => Some(candidate),
        Ok(false) => None,
        Err(_) => {
            crate::trace::mark("ffmpeg: gave up waiting on the build on PATH");
            None
        }
    }
}

/// The same question, asked without a deadline.
///
/// For a switch the user pressed a button for. They are already watching it
/// happen, and answering "no usable FFmpeg" because a cold disk took six
/// seconds would be false. A cached answer still short-circuits, so the usual
/// case costs nothing here either.
pub fn adopt_without_deadline(cache_dir: &Path) -> Option<FfmpegTools> {
    let candidate = FfmpegTools::on_path()?;
    check(cache_dir, &candidate).usable().then_some(candidate)
}

/// The build on PATH and what is wrong with it, if anything. For Settings.
pub fn inspect(cache_dir: &Path) -> Option<SystemBuild> {
    let candidate = FfmpegTools::on_path()?;
    let verdict = check(cache_dir, &candidate);

    Some(SystemBuild {
        location: candidate.ffmpeg.to_string_lossy().to_string(),
        usable: verdict.usable(),
        missing_encoders: verdict.missing_encoders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::tools::{exe, ToolsSource};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Files named like the binaries, which exist and are not executable.
    fn fake_build(dir: &Path) -> FfmpegTools {
        std::fs::write(dir.join(exe("ffmpeg")), b"not really ffmpeg").unwrap();
        std::fs::write(dir.join(exe("ffprobe")), b"not really ffprobe").unwrap();
        FfmpegTools {
            ffmpeg: dir.join(exe("ffmpeg")),
            ffprobe: dir.join(exe("ffprobe")),
            source: ToolsSource::System,
        }
    }

    /// The point of the whole module: a build that cannot run is turned down,
    /// and the refusal is written down so the next launch does not re-spawn it.
    #[test]
    fn a_build_that_will_not_run_is_rejected_and_remembered() {
        let dir = temp_dir("system-unrunnable");
        let cache = dir.join("cache");
        let tools = fake_build(&dir);

        let verdict = check(&cache, &tools);
        assert!(!verdict.runs);
        assert!(!verdict.usable());

        let remembered = read_verdict(&cache).expect("the answer must be cached");
        assert!(!remembered.runs);
        assert!(remembered.still_describes(&tools, &fingerprint(&tools).unwrap()));
    }

    /// A cached answer must not outlive the binary it describes — an upgraded
    /// FFmpeg is a different build, and may well have gained the encoder that
    /// got the old one turned down.
    #[test]
    fn replacing_the_binary_invalidates_the_cached_answer() {
        let dir = temp_dir("system-stale");
        let cache = dir.join("cache");
        let tools = fake_build(&dir);

        check(&cache, &tools);
        let remembered = read_verdict(&cache).unwrap();

        std::fs::write(&tools.ffmpeg, b"a different ffmpeg entirely, and longer").unwrap();
        assert!(
            !remembered.still_describes(&tools, &fingerprint(&tools).unwrap()),
            "a changed binary must not inherit the old verdict"
        );
    }

    /// The cache is the whole reason adoption is cheap on every launch after
    /// the first, so prove it is actually consulted rather than decorative: a
    /// text file could never run, so a "usable" answer here can only have come
    /// from the remembered verdict.
    #[test]
    fn a_matching_verdict_is_trusted_without_running_anything() {
        let dir = temp_dir("system-cached");
        let cache = dir.join("cache");
        let tools = fake_build(&dir);
        let print = fingerprint(&tools).unwrap();

        write_verdict(
            &cache,
            &Verdict {
                ffmpeg: tools.ffmpeg.to_string_lossy().to_string(),
                ffmpeg_len: print.ffmpeg_len,
                ffmpeg_modified: print.ffmpeg_modified,
                ffprobe_len: print.ffprobe_len,
                runs: true,
                missing_encoders: Vec::new(),
            },
        );

        assert!(
            check(&cache, &tools).usable(),
            "a remembered answer must short-circuit the probe"
        );
    }

    /// Missing encoders are what a support question will ask about, so they
    /// have to survive the round trip in a readable form.
    #[test]
    fn a_verdict_keeps_the_names_of_what_was_missing() {
        let dir = temp_dir("system-missing");
        let cache = dir.join("cache");
        let verdict = Verdict {
            ffmpeg: "C:/ffmpeg/bin/ffmpeg.exe".to_string(),
            ffmpeg_len: 1,
            ffmpeg_modified: Some(2),
            ffprobe_len: 3,
            runs: true,
            missing_encoders: vec!["libsvtav1".to_string()],
        };
        write_verdict(&cache, &verdict);

        let read_back = read_verdict(&cache).unwrap();
        assert!(!read_back.usable(), "a build missing an encoder is not usable");
        assert_eq!(read_back.missing_encoders, vec!["libsvtav1".to_string()]);
    }
}
