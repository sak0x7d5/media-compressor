//! One file, start to finish: probe, plan, encode, verify, correct.
//!
//! This is where the strategy meets the encoder. Everything it decides is
//! reported through `on_stage` so the UI can show the reasoning rather than a
//! bare percentage.

use crate::ffmpeg::encode::{self, CancelToken, EncodeError, EncodeJob, EncodeProgress, Speed};
use crate::ffmpeg::probe::{probe, ProbeError};
use crate::ffmpeg::sample::{self, DIRECT_ENCODE_THRESHOLD_SECS};
use crate::ffmpeg::tools::FfmpegTools;
use crate::images::{self, ImageError, ImageFormat, ImageOutcome, ImageRequest, ImageStep};
use crate::strategy::plan::{EncodePlan, Options, PlanContext};
use crate::strategy::{MediaInfo, PlanError, Target};
use serde::Serialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct CompressRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub target: Target,
    pub options: Options,
    pub speed: Speed,
    /// Scratch space for sample encodes and two-pass logs. Created if absent
    /// and emptied when the job finishes, however it finishes.
    pub work_dir: PathBuf,
    /// Only consulted when the input is a still image.
    pub image_format: ImageFormat,
    /// Longest side to allow for images, before target-driven downscaling.
    pub max_dimension: Option<u32>,
}

/// The result of compressing something, whichever kind of thing it was.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MediaOutcome {
    Video(Box<CompressOutcome>),
    Image(ImageOutcome),
}

impl MediaOutcome {
    pub fn output_bytes(&self) -> u64 {
        match self {
            Self::Video(outcome) => outcome.output_bytes,
            Self::Image(outcome) => outcome.output_bytes,
        }
    }

    pub fn within_limit(&self) -> bool {
        match self {
            Self::Video(outcome) => outcome.within_limit,
            Self::Image(outcome) => outcome.within_limit,
        }
    }
}

/// What the pipeline is doing right now, for display.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "kebab-case")]
pub enum Stage {
    Probing,
    /// Encoding short slices to find out what quality-targeting would cost.
    Predicting,
    /// The plan has been chosen; the UI can show the geometry and rate control.
    Planned { plan: EncodePlan, predicted_bytes: Option<u64> },
    Encoding(EncodeProgress),
    /// The output missed the target and is being re-encoded.
    Correcting { attempt: u8, actual_bytes: u64 },
    /// One probe of an image quality search.
    Searching(ImageStep),
}

#[derive(Debug, Clone, Serialize)]
pub struct CompressOutcome {
    pub info: MediaInfo,
    pub plan: EncodePlan,
    pub output_bytes: u64,
    pub attempts: u8,
    /// False when we ran out of attempts still over the limit. The file is
    /// still written — the caller decides whether to keep it.
    pub within_limit: bool,
}

#[derive(Debug, Error)]
pub enum CompressError {
    #[error(transparent)]
    Probe(#[from] ProbeError),

    #[error(transparent)]
    Plan(#[from] PlanError),

    #[error(transparent)]
    Encode(#[from] EncodeError),

    #[error(transparent)]
    Image(#[from] ImageError),

    #[error("could not prepare the working directory: {0}")]
    Io(#[from] std::io::Error),
}

impl CompressError {
    pub fn is_cancellation(&self) -> bool {
        matches!(self, Self::Encode(EncodeError::Cancelled) | Self::Image(ImageError::Cancelled))
    }
}

/// Compress whatever this is — video or still image — to fit under the target.
///
/// A run that ends in an error — cancelled, or FFmpeg giving up part way — has
/// usually already written some of the output. That half-file is worse than
/// nothing: it carries the name a finished result would have carried, it will
/// not play, and the next run steps around it instead of reusing the name. So
/// anything we created and did not finish is removed on the way out.
pub fn compress_media(
    tools: &FfmpegTools,
    request: &CompressRequest,
    cancel: &CancelToken,
    on_stage: impl FnMut(Stage),
) -> Result<MediaOutcome, CompressError> {
    // Only ever delete a file this run brought into being. A path that was
    // already occupied belongs to someone else, however this ends.
    let pre_existing = request.output.exists();

    let result = run_media(tools, request, cancel, on_stage);

    if result.is_err() && !pre_existing {
        let _ = std::fs::remove_file(&request.output);
    }

    result
}

fn run_media(
    tools: &FfmpegTools,
    request: &CompressRequest,
    cancel: &CancelToken,
    mut on_stage: impl FnMut(Stage),
) -> Result<MediaOutcome, CompressError> {
    if images::is_image(&request.input) {
        let image_request = ImageRequest {
            input: request.input.clone(),
            output: request.output.clone(),
            target_bytes: request.target.effective_bytes(),
            format: request.image_format,
            max_dimension: request.max_dimension,
        };

        let outcome = images::compress_image(tools, &image_request, cancel, |step| {
            on_stage(Stage::Searching(step));
        })?;

        return Ok(MediaOutcome::Image(outcome));
    }

    compress(tools, request, cancel, on_stage).map(|outcome| MediaOutcome::Video(Box::new(outcome)))
}

/// Compress one file to fit under a target size.
pub fn compress(
    tools: &FfmpegTools,
    request: &CompressRequest,
    cancel: &CancelToken,
    mut on_stage: impl FnMut(Stage),
) -> Result<CompressOutcome, CompressError> {
    std::fs::create_dir_all(&request.work_dir)?;
    let result = run(tools, request, cancel, &mut on_stage);
    clean_work_dir(&request.work_dir);
    result
}

fn run(
    tools: &FfmpegTools,
    request: &CompressRequest,
    cancel: &CancelToken,
    on_stage: &mut impl FnMut(Stage),
) -> Result<CompressOutcome, CompressError> {
    on_stage(Stage::Probing);
    let info = probe(tools, &request.input)?;

    let context = PlanContext::new(info.clone(), request.target, request.options)?;

    // Short files skip prediction. Sampling a 12-second clip costs about what
    // encoding it costs, so we encode it and look at the answer instead of
    // guessing at it.
    let (mut plan, predicted) = if info.duration_secs <= DIRECT_ENCODE_THRESHOLD_SECS {
        (context.optimistic(), None)
    } else {
        on_stage(Stage::Predicting);
        let optimistic = context.optimistic();
        let prediction = sample::predict(
            tools,
            &request.input,
            &optimistic,
            &info,
            context.budget.total_bytes,
            request.speed,
            &request.work_dir,
            cancel,
        );

        match prediction {
            Ok(prediction) => {
                (context.decide(prediction.total_bytes), Some(prediction.total_bytes))
            }
            // A failed prediction is not a failed job. Assume the file is too
            // big — the safe assumption, since it lands on two-pass, which
            // respects the cap exactly.
            Err(EncodeError::Cancelled) => return Err(EncodeError::Cancelled.into()),
            Err(_) => (context.decide(u64::MAX), None),
        }
    };

    on_stage(Stage::Planned { plan: plan.clone(), predicted_bytes: predicted });

    let mut output_bytes;
    // What the statistics files in the work directory describe, once some
    // attempt has written them. A correction that keeps the same picture spends
    // them instead of recomputing them, which halves its cost.
    let mut stats = None;

    loop {
        let job = EncodeJob {
            input: request.input.clone(),
            output: request.output.clone(),
            plan: plan.clone(),
            source: info.clone(),
            speed: request.speed,
            passlog_prefix: request.work_dir.join("pass"),
            reusable_stats: stats,
        };

        output_bytes = encode::run(tools, &job, cancel, |progress| {
            on_stage(Stage::Encoding(progress));
        })?;

        // Only a two-pass attempt leaves an analysis behind; a CRF one never
        // writes the log at all, so there is nothing for the next attempt to
        // inherit.
        stats = plan.is_two_pass().then(|| job.pass_log());

        match context.correct(&plan, output_bytes) {
            Some(corrected) => {
                on_stage(Stage::Correcting { attempt: corrected.attempt, actual_bytes: output_bytes });
                plan = corrected;
                on_stage(Stage::Planned { plan: plan.clone(), predicted_bytes: None });
            }
            None => break,
        }
    }

    Ok(CompressOutcome {
        info,
        attempts: plan.attempt,
        plan,
        output_bytes,
        within_limit: output_bytes <= request.target.limit_bytes,
    })
}

/// Remove scratch files without caring whether it worked.
///
/// A leftover two-pass log would be silently reused by the next attempt, and a
/// leftover sample is just litter — but neither is worth failing a finished
/// encode over.
fn clean_work_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaning_a_work_dir_removes_it_entirely() {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pass-0.log"), b"stats").unwrap();
        std::fs::write(dir.join("sample-0.mp4"), b"bytes").unwrap();

        clean_work_dir(&dir);
        assert!(!dir.exists(), "work dir should be gone");
    }

    #[test]
    fn cleaning_a_missing_work_dir_is_not_an_error() {
        clean_work_dir(Path::new("this-directory-does-not-exist-anywhere"));
    }
}
