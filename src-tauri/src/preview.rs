//! Pulling a matched pair of frames out of the source and the result.
//!
//! The point is to answer "what did this actually cost me" before you send the
//! file. Both frames are taken at the same timestamp and scaled to the same
//! width, so the only difference on screen is the compression.

use crate::ffmpeg::tools::FfmpegTools;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};
use thiserror::Error;

/// Wide enough to show artefacts, small enough to move over IPC as base64
/// without the payload becoming the slow part.
const PREVIEW_WIDTH: u32 = 720;

/// How far before the asked-for moment to actually seek.
///
/// A request that lands exactly on a frame boundary is ambiguous: "the frame at
/// or after T" can fall either side of it, and two files resolving that
/// differently is how a comparison ends up showing two different moments. It
/// comes up constantly rather than rarely, because the timeline rounds to
/// tenths of a second and a tenth is a whole number of frames at every common
/// rate.
///
/// Captured sources make it worse. A recording is rarely exactly constant rate:
/// its frame timestamps wander by tens of microseconds against the fixed grid
/// of the re-encode, so the two files agree near the start and then disagree
/// once that wander has crossed a boundary — which is why this looked like the
/// comparison drifting out of sync partway through a clip.
///
/// Two milliseconds lands clearly inside the intended frame while staying well
/// under half a frame at any rate worth previewing — 4ms at 120fps — so it can
/// never reach back into the frame before.
const SEEK_BIAS_SECONDS: f64 = 0.002;

/// Where to seek for a frame meant to represent `at_seconds`.
fn seek_target(at_seconds: f64) -> f64 {
    if !at_seconds.is_finite() {
        return 0.0;
    }
    (at_seconds - SEEK_BIAS_SECONDS).max(0.0)
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewPair {
    /// `data:` URLs, ready to drop into an `<img src>`.
    pub before: String,
    pub after: String,
    pub at_seconds: f64,
}

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("could not run ffmpeg: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("could not read a frame from {which}")]
    NoFrame { which: &'static str },
}

/// Grab one frame as JPEG bytes, straight off stdout.
fn grab_frame(tools: &FfmpegTools, path: &Path, at_seconds: f64) -> Result<Vec<u8>, PreviewError> {
    let mut command = Command::new(&tools.ffmpeg);
    command
        .args(["-hide_banner", "-v", "error", "-nostdin"])
        // Seeking before -i jumps to the nearest keyframe instead of decoding
        // everything up to that point.
        .args(["-ss", &format!("{at_seconds:.3}")])
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-map", "0:v:0", "-an"])
        .args(["-vf", &format!("scale={PREVIEW_WIDTH}:-2:flags=lanczos")])
        .args(["-q:v", "3", "-f", "mjpeg", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::ffmpeg::hide_console(&mut command);

    let output = command.output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(PreviewError::NoFrame { which: "the file" });
    }
    Ok(output.stdout)
}

fn as_data_url(bytes: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes))
}

/// Take the same frame from both files.
///
/// `at_seconds` defaults to the midpoint: the first and last moments of a clip
/// are too often a fade or a title card to be worth comparing.
pub fn compare(
    tools: &FfmpegTools,
    before: &Path,
    after: &Path,
    at_seconds: Option<f64>,
) -> Result<PreviewPair, PreviewError> {
    let at_seconds = at_seconds.unwrap_or_else(|| {
        crate::ffmpeg::probe::probe(tools, before)
            .map(|info| info.duration_secs / 2.0)
            .unwrap_or(0.0)
    });
    // A still image has no timeline to seek along.
    let at_seconds = if at_seconds.is_finite() && at_seconds > 0.0 { at_seconds } else { 0.0 };

    // Both files are read at once. `-ss` lands on the keyframe before the
    // timestamp and decodes forward from there, so the original dominates: a
    // 4K source costs ~1.2s mid-GOP against ~0.4s at a keyframe, while the
    // result — small, and freshly encoded with a short GOP — costs ~0.2s. In
    // series that smaller cost is paid on top of the larger one for nothing.
    // Both files are asked for the same instant, biased identically, so that
    // whatever each one rounds to it rounds to the same frame.
    let seek_at = seek_target(at_seconds);
    let (original, result) = std::thread::scope(|scope| {
        let original = scope.spawn(|| grab_frame(tools, before, seek_at));
        let result = grab_frame(tools, after, seek_at);
        (original.join(), result)
    });

    let before_frame = match original {
        Ok(Ok(bytes)) => bytes,
        // A panic in the worker reads the same as a frame that would not come.
        _ => return Err(PreviewError::NoFrame { which: "the original" }),
    };
    let after_frame = result.map_err(|_| PreviewError::NoFrame { which: "the result" })?;

    Ok(PreviewPair {
        before: as_data_url(&before_frame),
        after: as_data_url(&after_frame),
        at_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_wrapped_as_data_urls() {
        let url = as_data_url(&[0xff, 0xd8, 0xff]);
        assert!(url.starts_with("data:image/jpeg;base64,"));
        assert!(url.len() > "data:image/jpeg;base64,".len());
    }

    #[test]
    fn a_seek_lands_just_inside_the_frame_it_asks_for() {
        // Not on the boundary, which is the whole point, but nowhere near the
        // frame before it either.
        assert!((seek_target(33.0) - 32.998).abs() < 1e-9);
    }

    #[test]
    fn the_bias_never_seeks_past_the_start_of_the_file() {
        assert_eq!(seek_target(0.0), 0.0);
        assert_eq!(seek_target(0.001), 0.0);
    }

    #[test]
    fn a_timestamp_that_is_not_a_number_falls_back_to_the_start() {
        assert_eq!(seek_target(f64::NAN), 0.0);
        assert_eq!(seek_target(f64::INFINITY), 0.0);
    }

    #[test]
    fn an_empty_frame_still_produces_a_well_formed_url() {
        // Not a useful image, but it must not panic or produce a broken prefix.
        assert_eq!(as_data_url(&[]), "data:image/jpeg;base64,");
    }
}
