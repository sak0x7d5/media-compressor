//! Predicting what a quality-targeted encode will weigh, without doing it.
//!
//! A CRF encode's size depends entirely on how hard the content is, which no
//! formula knows. So we encode a few seconds of the real thing and extrapolate.
//!
//! Short files skip this entirely: sampling a 12-second clip costs about what
//! encoding it costs, so we just encode it and look at the result.

use super::encode::{CancelToken, EncodeError, Speed};
use super::tools::FfmpegTools;
use crate::strategy::plan::{EncodePlan, RateControl};
use crate::strategy::MediaInfo;
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Below this duration, sampling is not worth it — encode the file instead.
pub const DIRECT_ENCODE_THRESHOLD_SECS: f64 = 20.0;

const SEGMENT_SECS: f64 = 4.0;

/// Where in the file to sample, as fractions of the duration.
///
/// Not 0.0 and not 1.0: the first and last seconds of a clip are unusually
/// often a fade, a title card, or a black frame, none of which say anything
/// about how hard the rest is to encode.
const SEGMENT_POSITIONS: &[f64] = &[0.2, 0.5, 0.8];

/// `(start_seconds, length_seconds)` pairs to sample.
pub fn segments(duration_secs: f64) -> Vec<(f64, f64)> {
    if duration_secs <= 0.0 {
        return Vec::new();
    }

    // Never sample more than half the file; past that, just encode it.
    let length = SEGMENT_SECS.min(duration_secs / (SEGMENT_POSITIONS.len() as f64 * 2.0));
    if length <= 0.05 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(SEGMENT_POSITIONS.len());
    for position in SEGMENT_POSITIONS {
        let start = (duration_secs * position - length / 2.0).max(0.0);
        let start = start.min((duration_secs - length).max(0.0));
        out.push((start, length));
    }
    out
}

/// What a sample run concluded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    /// Extrapolated size of the *video* stream over the whole file.
    pub video_bytes: u64,
    /// Extrapolated size of the muxed file, audio and overhead included.
    pub total_bytes: u64,
    pub sampled_secs: f64,
}

fn sample_args(
    input: &Path,
    output: &Path,
    plan: &EncodePlan,
    source: &MediaInfo,
    speed: Speed,
    start: f64,
    length: f64,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(24);
    let mut push = |value: &str| args.push(OsString::from(value));

    push("-hide_banner");
    push("-nostdin");
    push("-y");
    // Seeking before -i is the fast path: FFmpeg jumps to the nearest keyframe
    // rather than decoding everything up to that point and throwing it away.
    push("-ss");
    push(&format!("{start:.3}"));

    args.push(OsString::from("-i"));
    args.push(input.to_path_buf().into_os_string());

    let mut push = |value: &str| args.push(OsString::from(value));
    push("-t");
    push(&format!("{length:.3}"));
    push("-map");
    push("0:v:0");
    push("-an");
    push("-sn");
    push("-dn");

    push("-c:v");
    push(plan.codec.ffmpeg_encoder());
    push("-preset");
    push(speed.preset_for(plan.codec));

    // A sample of a two-pass plan is meaningless — the point of sampling is to
    // find out what quality-targeting costs, so always sample at CRF.
    let crf = match plan.rate_control {
        RateControl::Crf { crf } => crf,
        RateControl::TwoPass { .. } => plan.codec.default_crf(),
    };
    push("-crf");
    push(&crf.to_string());

    if plan.scale.width != source.width || plan.scale.height != source.height {
        push("-vf");
        push(&format!("scale={}:{}:flags=lanczos,setsar=1", plan.scale.width, plan.scale.height));
    }

    push("-pix_fmt");
    push("yuv420p");
    args.push(output.to_path_buf().into_os_string());

    args
}

/// Encode a few slices of the file and extrapolate.
pub fn predict(
    tools: &FfmpegTools,
    input: &Path,
    plan: &EncodePlan,
    source: &MediaInfo,
    speed: Speed,
    work_dir: &Path,
    cancel: &CancelToken,
) -> Result<Prediction, EncodeError> {
    let slices = segments(source.duration_secs);
    if slices.is_empty() {
        return Err(EncodeError::NoOutput);
    }

    let mut total_bytes = 0u64;
    let mut sampled_secs = 0.0;

    for (index, (start, length)) in slices.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(EncodeError::Cancelled);
        }

        let sample_path = work_dir.join(format!("sample-{index}.mp4"));
        let args = sample_args(input, &sample_path, plan, source, speed, *start, *length);

        let outcome = run_quietly(tools, args, cancel);
        let size = std::fs::metadata(&sample_path).map(|meta| meta.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&sample_path);
        outcome?;

        // A zero-byte sample means the seek landed past the end of a file whose
        // reported duration was wrong. Skip it rather than counting it as
        // "this part of the video is free".
        if size > 0 {
            total_bytes += size;
            sampled_secs += length;
        }
    }

    if sampled_secs <= 0.0 {
        return Err(EncodeError::NoOutput);
    }

    // Samples carry MP4 container overhead of their own, and each one starts on
    // a fresh keyframe, so this runs a little pessimistic. That is the safe
    // direction: it biases towards two-pass, which respects the cap exactly.
    let bytes_per_second = total_bytes as f64 / sampled_secs;
    let video_bytes = (bytes_per_second * source.duration_secs).round() as u64;
    let audio_bytes = (plan.audio_bps as f64 * source.duration_secs / 8.0).round() as u64;

    Ok(Prediction {
        video_bytes,
        total_bytes: video_bytes + audio_bytes,
        sampled_secs,
    })
}

/// Run an FFmpeg command we do not need progress from.
fn run_quietly(
    tools: &FfmpegTools,
    args: Vec<OsString>,
    cancel: &CancelToken,
) -> Result<(), EncodeError> {
    let mut child = FfmpegCommand::new_with_path(&tools.ffmpeg)
        .create_no_window()
        .args(args)
        .spawn()
        .map_err(EncodeError::Spawn)?;

    let mut errors: Vec<String> = Vec::new();
    let events = child.iter().map_err(|e| EncodeError::Failed {
        code: None,
        detail: format!("could not read ffmpeg output: {e}"),
    })?;

    for event in events {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EncodeError::Cancelled);
        }
        if let FfmpegEvent::Log(LogLevel::Error | LogLevel::Fatal, line) = event {
            errors.push(line);
            if errors.len() > 6 {
                errors.remove(0);
            }
        }
    }

    let status = child.wait().map_err(EncodeError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(EncodeError::Failed { code: status.code(), detail: errors.join("; ") })
    }
}

/// Convenience for callers that hold a [`PathBuf`] work directory.
pub fn work_file(work_dir: &Path, stem: &str) -> PathBuf {
    work_dir.join(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_segments_are_spread_through_the_file() {
        let slices = segments(180.0);
        assert_eq!(slices.len(), 3);

        for (start, length) in &slices {
            assert!(*length > 0.0);
            assert!(*start >= 0.0);
            assert!(start + length <= 180.0 + 1e-9, "segment {start}+{length} runs past the end");
        }

        // Spread out, not clustered.
        assert!(slices[1].0 > slices[0].0);
        assert!(slices[2].0 > slices[1].0);
    }

    #[test]
    fn segments_never_cover_more_than_half_the_file() {
        for duration in [21.0f64, 30.0, 45.0, 60.0, 600.0] {
            let sampled: f64 = segments(duration).iter().map(|(_, length)| length).sum();
            assert!(
                sampled <= duration / 2.0 + 1e-9,
                "sampled {sampled}s of a {duration}s file"
            );
        }
    }

    #[test]
    fn segments_stay_inside_a_short_file() {
        let slices = segments(5.0);
        for (start, length) in &slices {
            assert!(start + length <= 5.0 + 1e-9, "segment {start}+{length} runs past 5s");
        }
    }

    #[test]
    fn a_zero_length_file_yields_no_segments() {
        assert!(segments(0.0).is_empty());
        assert!(segments(-1.0).is_empty());
    }
}
