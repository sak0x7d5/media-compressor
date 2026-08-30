//! Building FFmpeg command lines and running them.
//!
//! Argument construction is a pure function so the fiddly parts — two-pass log
//! files, when a scale filter is warranted, dropping audio from the first pass
//! — can be asserted without spawning anything.

use super::tools::FfmpegTools;
use crate::strategy::plan::{EncodePlan, RateControl};
use crate::strategy::{MediaInfo, VideoCodec};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;

/// Where the first pass sends its discarded output.
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// How hard the encoder works per bit. Slower is smaller at equal quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Speed {
    Fast,
    #[default]
    Balanced,
    Slow,
}

impl Speed {
    /// SVT-AV1 takes a number where x264 and x265 take a word, but both use the
    /// `-preset` flag, so only the value differs.
    pub(crate) fn preset_for(self, codec: VideoCodec) -> &'static str {
        match codec {
            VideoCodec::H264 | VideoCodec::Hevc => match self {
                Self::Fast => "veryfast",
                Self::Balanced => "medium",
                Self::Slow => "slow",
            },
            VideoCodec::Av1 => match self {
                Self::Fast => "8",
                Self::Balanced => "6",
                Self::Slow => "4",
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// One encode, quality-targeted.
    Single,
    /// Analysis only; output discarded.
    First,
    /// The real encode, spending the stats the first pass gathered.
    Second,
}

impl Pass {
    fn index(self) -> Option<&'static str> {
        match self {
            Self::Single => None,
            Self::First => Some("1"),
            Self::Second => Some("2"),
        }
    }

    /// The first pass analyses video only — muxing audio it will throw away is
    /// wasted time, and on a long file it is not a small amount of it.
    fn wants_audio(self) -> bool {
        !matches!(self, Self::First)
    }
}

/// What a two-pass statistics file describes.
///
/// The log holds per-frame complexity for one particular picture analysed by
/// one particular encoder at one particular preset. Change any of those and it
/// stops describing the frames being encoded.
///
/// Bitrate is deliberately absent: spending a different number of bits on the
/// same analysis is precisely what the log exists for, and it is the only thing
/// a correction usually changes.
///
/// Compared exactly, the floating-point framerate included. Both sides come out
/// of the same ladder by the same route, so the only inexactness available is a
/// false *mismatch* — which costs a first pass that could have been skipped,
/// never a log that should not have been trusted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassLog {
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps: f64,
    speed: Speed,
}

#[derive(Debug, Clone)]
pub struct EncodeJob {
    pub input: PathBuf,
    pub output: PathBuf,
    pub plan: EncodePlan,
    pub source: MediaInfo,
    pub speed: Speed,
    /// Prefix for the two-pass statistics files. FFmpeg appends `-0.log` and
    /// `-0.log.mbtree`.
    pub passlog_prefix: PathBuf,
    /// What the statistics files at `passlog_prefix` already describe, if an
    /// earlier attempt on this file left any behind.
    pub reusable_stats: Option<PassLog>,
}

impl EncodeJob {
    /// What this job's own first pass would write to the statistics file.
    pub fn pass_log(&self) -> PassLog {
        PassLog {
            codec: self.plan.codec,
            width: self.plan.scale.width,
            height: self.plan.scale.height,
            fps: self.plan.scale.fps,
            speed: self.speed,
        }
    }

    /// The passes this job needs, in order.
    ///
    /// A correction that only moves the bitrate reuses the statistics the
    /// previous attempt wrote. Its first pass would analyse the same frames at
    /// the same settings and reach the same numbers, so running it again
    /// doubles the cost of the correction to learn nothing.
    pub fn passes(&self) -> Vec<Pass> {
        if !self.plan.is_two_pass() {
            return vec![Pass::Single];
        }
        if self.reusable_stats == Some(self.pass_log()) {
            return vec![Pass::Second];
        }
        vec![Pass::First, Pass::Second]
    }

    fn needs_scaling(&self) -> bool {
        self.plan.scale.width != self.source.width || self.plan.scale.height != self.source.height
    }

    fn needs_framerate_change(&self) -> bool {
        self.plan.scale.fps < self.source.fps - 0.01
    }

    fn filters(&self) -> Option<String> {
        let mut chain: Vec<String> = Vec::new();

        if self.needs_scaling() {
            // Lanczos is the right choice for downscaling: sharper than
            // bicubic, and the whole reason we are downscaling is to preserve
            // apparent detail.
            chain.push(format!(
                "scale={}:{}:flags=lanczos",
                self.plan.scale.width, self.plan.scale.height
            ));
            // Anamorphic sources would otherwise carry a stale aspect ratio.
            chain.push("setsar=1".to_string());
        }

        if self.needs_framerate_change() {
            chain.push(format!("fps={}", self.plan.scale.fps));
        }

        (!chain.is_empty()).then(|| chain.join(","))
    }

    /// Build the full argument vector for one pass.
    pub fn args(&self, pass: Pass) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::with_capacity(32);
        let mut push = |value: &str| args.push(OsString::from(value));

        push("-hide_banner");
        // Without this, FFmpeg can block forever waiting on a stdin it will
        // never receive, which looks exactly like a hung encode.
        push("-nostdin");
        push("-y");

        args.push(OsString::from("-i"));
        args.push(self.input.clone().into_os_string());

        // Take exactly one video stream and at most one audio stream. Left to
        // itself, FFmpeg will try to carry subtitles and data streams into MP4
        // and fail on the ones it cannot represent.
        let mut push = |value: &str| args.push(OsString::from(value));
        push("-map");
        push("0:v:0");
        let has_audio = self.source.audio.is_some() && self.plan.audio_bps > 0;
        if has_audio && pass.wants_audio() {
            push("-map");
            push("0:a:0?");
        }
        push("-sn");
        push("-dn");

        push("-c:v");
        push(self.plan.codec.ffmpeg_encoder());
        push("-preset");
        push(self.speed.preset_for(self.plan.codec));

        match self.plan.rate_control {
            RateControl::Crf { crf } => {
                push("-crf");
                push(&crf.to_string());
            }
            RateControl::TwoPass { video_bps } => {
                push("-b:v");
                push(&video_bps.to_string());
            }
        }

        if let Some(index) = pass.index() {
            push("-pass");
            push(index);
            args.push(OsString::from("-passlogfile"));
            args.push(self.passlog_prefix.clone().into_os_string());
        }

        if let Some(chain) = self.filters() {
            args.push(OsString::from("-vf"));
            args.push(OsString::from(chain));
        }

        let mut push = |value: &str| args.push(OsString::from(value));
        push("-pix_fmt");
        push("yuv420p");

        if has_audio && pass.wants_audio() {
            push("-c:a");
            push("aac");
            push("-b:a");
            push(&self.plan.audio_bps.to_string());
            // A 5.1 source at 96 kbps is worse than a stereo downmix at 96 kbps.
            if self.source.audio.map(|audio| audio.channels).unwrap_or(2) > 2 {
                push("-ac");
                push("2");
            }
        } else {
            push("-an");
        }

        if matches!(pass, Pass::First) {
            push("-f");
            push("null");
            push(NULL_DEVICE);
        } else {
            // Puts the index at the front so the file starts playing before it
            // has finished downloading — which is how Discord serves it.
            push("-movflags");
            push("+faststart");
            args.push(self.output.clone().into_os_string());
        }

        args
    }
}

/// A cooperative stop signal, shared with whatever is watching the queue.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct EncodeProgress {
    /// 1-based index of the pass currently running.
    pub pass: u8,
    pub of_passes: u8,
    /// Progress through this pass, 0.0 to 1.0.
    pub pass_fraction: f64,
    /// Progress through the whole job, passes weighted equally.
    pub overall_fraction: f64,
    pub fps: f32,
    pub speed: f32,
    pub output_bytes: u64,
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("could not start ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("ffmpeg failed{}: {detail}", match code { Some(c) => format!(" (exit {c})"), None => String::new() })]
    Failed { code: Option<i32>, detail: String },

    #[error("cancelled")]
    Cancelled,

    #[error("ffmpeg produced no output file")]
    NoOutput,
}

/// Parse FFmpeg's `HH:MM:SS.ss` progress timestamp into seconds.
pub fn parse_timestamp(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("N/A") {
        return None;
    }

    let mut seconds = 0.0;
    for part in value.split(':') {
        let part: f64 = part.parse().ok()?;
        seconds = seconds * 60.0 + part;
    }
    (seconds >= 0.0).then_some(seconds)
}

/// Run one pass to completion.
pub fn run_pass(
    tools: &FfmpegTools,
    job: &EncodeJob,
    pass: Pass,
    pass_index: u8,
    total_passes: u8,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(EncodeProgress),
) -> Result<(), EncodeError> {
    let mut child = FfmpegCommand::new_with_path(&tools.ffmpeg)
        .create_no_window()
        .args(job.args(pass))
        .spawn()
        .map_err(EncodeError::Spawn)?;

    // FFmpeg reports failures across several lines and the last one is usually
    // the least informative ("Conversion failed!"), so keep a short tail.
    let mut errors: Vec<String> = Vec::new();
    let duration = job.source.duration_secs.max(0.001);

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

        match event {
            FfmpegEvent::Progress(progress) => {
                let elapsed = parse_timestamp(&progress.time).unwrap_or(0.0);
                let pass_fraction = (elapsed / duration).clamp(0.0, 1.0);
                let completed = f64::from(pass_index - 1);

                on_progress(EncodeProgress {
                    pass: pass_index,
                    of_passes: total_passes,
                    pass_fraction,
                    overall_fraction: ((completed + pass_fraction) / f64::from(total_passes))
                        .clamp(0.0, 1.0),
                    fps: progress.fps,
                    speed: progress.speed,
                    output_bytes: u64::from(progress.size_kb) * 1024,
                });
            }
            FfmpegEvent::Log(LogLevel::Error | LogLevel::Fatal, line) => {
                errors.push(line);
                if errors.len() > 8 {
                    errors.remove(0);
                }
            }
            FfmpegEvent::Error(line) => errors.push(line),
            _ => {}
        }
    }

    let status = child.wait().map_err(EncodeError::Spawn)?;

    if !status.success() {
        return Err(EncodeError::Failed {
            code: status.code(),
            detail: if errors.is_empty() {
                "no diagnostic output".to_string()
            } else {
                errors.join("; ")
            },
        });
    }

    Ok(())
}

/// Run every pass this job needs, then report the size of what landed.
pub fn run(
    tools: &FfmpegTools,
    job: &EncodeJob,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(EncodeProgress),
) -> Result<u64, EncodeError> {
    let passes = job.passes();
    let total = passes.len() as u8;

    let result = (|| {
        for (offset, pass) in passes.iter().enumerate() {
            run_pass(
                tools,
                job,
                *pass,
                offset as u8 + 1,
                total,
                cancel,
                &mut on_progress,
            )?;
        }
        Ok(())
    })();

    // The statistics files are deliberately left behind. A correction that
    // keeps the same picture reuses them, one that does not overwrites them in
    // its own first pass before the second pass ever reads them, and the
    // pipeline clears the whole working directory once the file is done. So
    // there is no window in which a stale log can be believed.
    result?;

    output_size(&job.output)
}

fn output_size(path: &Path) -> Result<u64, EncodeError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 0 => Ok(meta.len()),
        _ => Err(EncodeError::NoOutput),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::ladder::Scale;
    use crate::strategy::AudioInfo;

    fn scale(width: u32, height: u32, fps: f64) -> Scale {
        Scale { width, height, fps, bpp: 0.06, below_floor: false }
    }

    fn source(width: u32, height: u32, fps: f64, channels: Option<u8>) -> MediaInfo {
        MediaInfo {
            duration_secs: 60.0,
            width,
            height,
            fps,
            audio: channels.map(|channels| AudioInfo { channels }),
        }
    }

    fn make_job(plan: EncodePlan, source: MediaInfo) -> EncodeJob {
        EncodeJob {
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            plan,
            source,
            speed: Speed::Balanced,
            passlog_prefix: PathBuf::from("prefix"),
            reusable_stats: None,
        }
    }

    fn crf_plan(scale: Scale, audio_bps: u32) -> EncodePlan {
        EncodePlan {
            codec: VideoCodec::H264,
            scale,
            rate_control: RateControl::Crf { crf: 23 },
            audio_bps,
            attempt: 1,
        }
    }

    fn two_pass_plan(scale: Scale, audio_bps: u32, video_bps: u64) -> EncodePlan {
        EncodePlan {
            codec: VideoCodec::H264,
            scale,
            rate_control: RateControl::TwoPass { video_bps },
            audio_bps,
            attempt: 1,
        }
    }

    fn rendered(job: &EncodeJob, pass: Pass) -> Vec<String> {
        job.args(pass).iter().map(|arg| arg.to_string_lossy().to_string()).collect()
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        args.iter().position(|arg| arg == flag).and_then(|index| args.get(index + 1)).cloned()
    }

    #[test]
    fn a_crf_encode_is_one_pass_and_carries_no_pass_flags() {
        let job = make_job(crf_plan(scale(1920, 1080, 60.0), 96_000), source(1920, 1080, 60.0, Some(2)));
        assert_eq!(job.passes(), vec![Pass::Single]);

        let args = rendered(&job, Pass::Single);
        assert_eq!(value_after(&args, "-crf").as_deref(), Some("23"));
        assert!(!args.iter().any(|arg| arg == "-pass"), "single pass must not set -pass");
        assert!(!args.iter().any(|arg| arg == "-b:v"), "CRF must not also set a bitrate");
    }

    #[test]
    fn a_two_pass_encode_runs_both_passes_with_a_shared_log() {
        let job = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        assert_eq!(job.passes(), vec![Pass::First, Pass::Second]);

        let first = rendered(&job, Pass::First);
        let second = rendered(&job, Pass::Second);

        assert_eq!(value_after(&first, "-pass").as_deref(), Some("1"));
        assert_eq!(value_after(&second, "-pass").as_deref(), Some("2"));
        assert_eq!(value_after(&first, "-passlogfile"), value_after(&second, "-passlogfile"));
        assert_eq!(value_after(&second, "-b:v").as_deref(), Some("730000"));
    }

    #[test]
    fn a_correction_that_keeps_the_picture_skips_the_first_pass() {
        let first = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );

        // The correction the planner actually produces: same ladder rung, a
        // different number of bits to spend on it.
        let mut corrected = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 610_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        corrected.reusable_stats = Some(first.pass_log());

        assert_eq!(
            corrected.passes(),
            vec![Pass::Second],
            "analysis of an unchanged picture must not be repeated"
        );

        // And it must still be a real second pass, spending the new bitrate
        // against the log the first attempt left.
        let args = rendered(&corrected, Pass::Second);
        assert_eq!(value_after(&args, "-pass").as_deref(), Some("2"));
        assert_eq!(value_after(&args, "-b:v").as_deref(), Some("610000"));
        assert_eq!(value_after(&args, "-passlogfile").as_deref(), Some("prefix"));
    }

    #[test]
    fn statistics_are_only_reused_when_they_describe_the_same_encode() {
        let original = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        let stats = Some(original.pass_log());

        // Every field the log actually depends on. Any of them changing means
        // the stored frame data no longer describes what we are encoding, and
        // believing it would corrupt rate control rather than merely slow it.
        let mut smaller = make_job(
            two_pass_plan(scale(640, 360, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        smaller.reusable_stats = stats;
        assert_eq!(smaller.passes(), vec![Pass::First, Pass::Second], "geometry changed");

        let mut slower_fps = make_job(
            two_pass_plan(scale(854, 480, 24.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        slower_fps.reusable_stats = stats;
        assert_eq!(slower_fps.passes(), vec![Pass::First, Pass::Second], "framerate changed");

        let mut other_codec = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        other_codec.plan.codec = VideoCodec::Hevc;
        other_codec.reusable_stats = stats;
        assert_eq!(other_codec.passes(), vec![Pass::First, Pass::Second], "codec changed");

        let mut other_preset = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );
        other_preset.speed = Speed::Slow;
        other_preset.reusable_stats = stats;
        assert_eq!(other_preset.passes(), vec![Pass::First, Pass::Second], "preset changed");
    }

    #[test]
    fn a_crf_attempt_leaves_nothing_worth_reusing() {
        // Nothing writes a log on the CRF path, so a correction after one has
        // to analyse from scratch however the flag is set.
        let mut job = make_job(crf_plan(scale(1920, 1080, 60.0), 96_000), source(1920, 1080, 60.0, Some(2)));
        job.reusable_stats = Some(job.pass_log());
        assert_eq!(job.passes(), vec![Pass::Single]);
    }

    #[test]
    fn the_first_pass_discards_its_output_and_skips_audio() {
        let job = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );

        let first = rendered(&job, Pass::First);
        assert!(first.iter().any(|arg| arg == "-an"), "first pass should not encode audio");
        assert_eq!(value_after(&first, "-f").as_deref(), Some("null"));
        assert!(first.last().is_some_and(|last| last == NULL_DEVICE));
        assert!(!first.iter().any(|arg| arg == "out.mp4"), "first pass must not write the output");

        let second = rendered(&job, Pass::Second);
        assert!(second.iter().any(|arg| arg == "aac"), "second pass should encode audio");
        assert!(second.last().is_some_and(|last| last == "out.mp4"));
    }

    #[test]
    fn no_scale_filter_when_the_geometry_is_unchanged() {
        let job = make_job(crf_plan(scale(1920, 1080, 60.0), 96_000), source(1920, 1080, 60.0, Some(2)));
        let args = rendered(&job, Pass::Single);

        assert!(
            !args.iter().any(|arg| arg == "-vf"),
            "an unscaled, unretimed encode needs no filter chain"
        );
    }

    #[test]
    fn scaling_and_framerate_changes_share_one_filter_chain() {
        let job = make_job(
            two_pass_plan(scale(854, 480, 30.0), 96_000, 730_000),
            source(1920, 1080, 60.0, Some(2)),
        );

        let args = rendered(&job, Pass::Second);
        let chain = value_after(&args, "-vf").expect("expected a filter chain");

        assert!(chain.contains("scale=854:480"), "chain was {chain}");
        assert!(chain.contains("fps=30"), "chain was {chain}");
        assert!(chain.contains("setsar=1"), "chain was {chain}");
        assert_eq!(args.iter().filter(|arg| *arg == "-vf").count(), 1);
    }

    #[test]
    fn framerate_is_never_raised() {
        // Plan says 60, source is 30: the ladder never does this, but the filter
        // must not invent frames if it ever did.
        let job = make_job(crf_plan(scale(1280, 720, 60.0), 96_000), source(1280, 720, 30.0, Some(2)));
        let args = rendered(&job, Pass::Single);

        assert!(!args.iter().any(|arg| arg.starts_with("fps=")), "must not raise framerate");
    }

    #[test]
    fn a_silent_source_gets_no_audio_encoder() {
        let job = make_job(crf_plan(scale(1280, 720, 30.0), 0), source(1280, 720, 30.0, None));
        let args = rendered(&job, Pass::Single);

        assert!(args.iter().any(|arg| arg == "-an"));
        assert!(!args.iter().any(|arg| arg == "-c:a"));
    }

    #[test]
    fn surround_sources_are_downmixed_to_stereo() {
        let job = make_job(crf_plan(scale(1280, 720, 30.0), 96_000), source(1280, 720, 30.0, Some(6)));
        let args = rendered(&job, Pass::Single);
        assert_eq!(value_after(&args, "-ac").as_deref(), Some("2"));

        let stereo = make_job(crf_plan(scale(1280, 720, 30.0), 96_000), source(1280, 720, 30.0, Some(2)));
        let stereo_args = rendered(&stereo, Pass::Single);
        assert!(!stereo_args.iter().any(|arg| arg == "-ac"), "stereo needs no downmix");
    }

    #[test]
    fn only_the_first_video_and_audio_streams_are_carried() {
        let job = make_job(crf_plan(scale(1280, 720, 30.0), 96_000), source(1280, 720, 30.0, Some(2)));
        let args = rendered(&job, Pass::Single);

        assert_eq!(value_after(&args, "-map").as_deref(), Some("0:v:0"));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:a:0?"]));
        assert!(args.iter().any(|arg| arg == "-sn"), "subtitles must be dropped");
        assert!(args.iter().any(|arg| arg == "-dn"), "data streams must be dropped");
    }

    #[test]
    fn the_output_is_written_for_progressive_playback() {
        let job = make_job(crf_plan(scale(1280, 720, 30.0), 96_000), source(1280, 720, 30.0, Some(2)));
        let args = rendered(&job, Pass::Single);
        assert_eq!(value_after(&args, "-movflags").as_deref(), Some("+faststart"));
    }

    #[test]
    fn av1_takes_a_numeric_preset_and_x264_a_named_one() {
        let mut plan = crf_plan(scale(1280, 720, 30.0), 96_000);
        plan.codec = VideoCodec::Av1;
        let av1 = make_job(plan, source(1280, 720, 30.0, Some(2)));
        let args = rendered(&av1, Pass::Single);
        assert_eq!(value_after(&args, "-c:v").as_deref(), Some("libsvtav1"));
        assert_eq!(value_after(&args, "-preset").as_deref(), Some("6"));

        let h264 = make_job(crf_plan(scale(1280, 720, 30.0), 96_000), source(1280, 720, 30.0, Some(2)));
        let h264_args = rendered(&h264, Pass::Single);
        assert_eq!(value_after(&h264_args, "-preset").as_deref(), Some("medium"));
    }

    #[test]
    fn timestamps_parse_to_seconds() {
        assert_eq!(parse_timestamp("00:00:00.00"), Some(0.0));
        assert_eq!(parse_timestamp("00:01:30.50"), Some(90.5));
        assert_eq!(parse_timestamp("01:00:00.00"), Some(3600.0));
        assert_eq!(parse_timestamp("N/A"), None);
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("garbage"), None);
    }
}
