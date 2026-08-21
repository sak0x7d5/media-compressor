//! Size-targeted image compression.
//!
//! Images are a much easier problem than video: an encode takes milliseconds,
//! so there is no need to predict anything. We binary-search the quality
//! parameter against the real encoder until the file lands just under target.
//!
//! ## Why FFmpeg rather than `ravif`/`mozjpeg`/`oxipng`
//!
//! The plan named those crates. FFmpeg already ships `libwebp`, `libaom-av1`,
//! `mjpeg` and `png`, and we already depend on it — so using it here trades
//! three native-build dependencies (two of which need `cmake` and `nasm` on
//! Windows) for zero. The cost is real but small: `mozjpeg` would produce
//! roughly 10% smaller JPEGs than `mjpeg` at equal quality. That barely
//! matters, because the default output is WebP or AVIF, both of which beat
//! even a perfectly-encoded JPEG by far more than 10%.

use crate::ffmpeg::encode::{CancelToken, EncodeError};
use crate::ffmpeg::tools::FfmpegTools;
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// How many encodes we will spend finding the quality that fits.
///
/// Seven halvings take a 1–100 range down to a single step, and an image
/// encode is fast enough that seven of them is still under a second for WebP.
const MAX_SEARCH_STEPS: u8 = 7;

/// How many times we will shrink the image when no quality setting fits.
const MAX_DOWNSCALE_STEPS: u8 = 4;

/// Each downscale step keeps this fraction of the previous width.
const DOWNSCALE_FACTOR: f64 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// Fast, excellent compression, alpha, and Discord displays it inline.
    #[default]
    Webp,
    /// Smaller still, but libaom takes seconds per image rather than
    /// milliseconds, and a binary search multiplies that by seven.
    Avif,
    /// Maximum compatibility, no alpha.
    Jpeg,
    /// Lossless. Quality search does nothing here — only downscaling shrinks it.
    Png,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    pub fn is_lossless(self) -> bool {
        matches!(self, Self::Png)
    }

    /// Map our 1–100 "higher is better" scale onto the encoder's own scale.
    ///
    /// Each encoder disagrees about direction and range, and getting this
    /// backwards produces a search that converges confidently on the worst
    /// possible setting — so it is a pure function with tests.
    fn encoder_quality_args(self, quality: u8) -> Vec<String> {
        let quality = quality.clamp(1, 100);

        match self {
            // libwebp: 0–100, higher is better. Same direction as ours.
            Self::Webp => vec!["-quality".into(), quality.to_string()],

            // libaom: CRF 0–63, *lower* is better.
            Self::Avif => {
                let crf = (63.0 - (f64::from(quality) / 100.0) * 63.0).round() as u32;
                vec![
                    "-still-picture".into(),
                    "1".into(),
                    // Without this, a single large still can take tens of
                    // seconds, and we are about to do it seven times.
                    "-cpu-used".into(),
                    "6".into(),
                    "-crf".into(),
                    crf.to_string(),
                ]
            }

            // mjpeg: qscale 2–31, lower is better.
            Self::Jpeg => {
                let qscale = (31.0 - (f64::from(quality) / 100.0) * 29.0).round().clamp(2.0, 31.0);
                vec!["-q:v".into(), format!("{qscale:.0}")]
            }

            // PNG is lossless; the knob only trades encode time for size.
            Self::Png => vec!["-compression_level".into(), "100".into()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub target_bytes: u64,
    pub format: ImageFormat,
    /// Longest side to allow, before any target-driven downscaling.
    pub max_dimension: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageOutcome {
    pub output_bytes: u64,
    pub quality: u8,
    pub width: u32,
    pub height: u32,
    pub downscale_steps: u8,
    pub encodes: u8,
    pub within_limit: bool,
}

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("could not run ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("ffmpeg could not encode the image: {0}")]
    Failed(String),

    #[error("cancelled")]
    Cancelled,

    #[error("no output was produced")]
    NoOutput,
}

impl From<EncodeError> for ImageError {
    fn from(error: EncodeError) -> Self {
        match error {
            EncodeError::Cancelled => Self::Cancelled,
            EncodeError::Spawn(inner) => Self::Spawn(inner),
            EncodeError::NoOutput => Self::NoOutput,
            EncodeError::Failed { detail, .. } => Self::Failed(detail),
        }
    }
}

/// One step of the binary search. Pure, so the bookkeeping is testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Search {
    pub low: u8,
    pub high: u8,
    /// Best quality found so far that produced a file under target.
    pub best: Option<u8>,
}

impl Default for Search {
    fn default() -> Self {
        Self { low: 1, high: 100, best: None }
    }
}

impl Search {
    pub fn probe(&self) -> u8 {
        self.low + (self.high - self.low) / 2
    }

    pub fn narrowed(mut self, quality: u8, fits: bool) -> Self {
        if fits {
            // Keep it, and reach for better quality.
            if self.best.is_none_or(|best| quality > best) {
                self.best = Some(quality);
            }
            self.low = quality.saturating_add(1);
        } else {
            self.high = quality.saturating_sub(1);
        }
        self
    }

    pub fn exhausted(&self) -> bool {
        self.low > self.high
    }
}

fn scale_filter(max_dimension: Option<u32>, factor: f64) -> Option<String> {
    let mut chain: Vec<String> = Vec::new();

    if let Some(max) = max_dimension {
        // -1 keeps the aspect ratio; -2 additionally forces an even result,
        // which some encoders require and none object to.
        chain.push(format!("scale='min({max},iw)':-2"));
    }

    if factor < 0.999 {
        chain.push(format!("scale=iw*{factor:.4}:-2"));
    }

    (!chain.is_empty()).then(|| chain.join(","))
}

fn encode_args(
    request: &ImageRequest,
    quality: u8,
    factor: f64,
    output: &Path,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(20);
    let mut push = |value: &str| args.push(OsString::from(value));

    push("-hide_banner");
    push("-nostdin");
    push("-y");

    args.push(OsString::from("-i"));
    args.push(request.input.to_path_buf().into_os_string());

    let mut push = |value: &str| args.push(OsString::from(value));
    // Animated sources (an APNG, a multi-frame WebP) would otherwise write a
    // numbered sequence instead of the single image we asked for.
    push("-frames:v");
    push("1");
    push("-map");
    push("0:v:0");
    push("-an");

    if let Some(chain) = scale_filter(request.max_dimension, factor) {
        push("-vf");
        push(&chain);
    }

    push("-c:v");
    push(match request.format {
        ImageFormat::Webp => "libwebp",
        ImageFormat::Avif => "libaom-av1",
        ImageFormat::Jpeg => "mjpeg",
        ImageFormat::Png => "png",
    });

    for arg in request.format.encoder_quality_args(quality) {
        push(&arg);
    }

    // JPEG has no alpha channel, and the default conversion produces a black
    // background where transparency was rather than a white one.
    if matches!(request.format, ImageFormat::Jpeg) {
        push("-pix_fmt");
        push("yuvj420p");
    }

    args.push(output.to_path_buf().into_os_string());
    args
}

fn run(tools: &FfmpegTools, args: Vec<OsString>, cancel: &CancelToken) -> Result<(), ImageError> {
    let mut child = FfmpegCommand::new_with_path(&tools.ffmpeg)
        .create_no_window()
        .args(args)
        .spawn()
        .map_err(ImageError::Spawn)?;

    let mut errors: Vec<String> = Vec::new();
    let events = child
        .iter()
        .map_err(|e| ImageError::Failed(format!("could not read ffmpeg output: {e}")))?;

    for event in events {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ImageError::Cancelled);
        }
        if let FfmpegEvent::Log(LogLevel::Error | LogLevel::Fatal, line) = event {
            errors.push(line);
            if errors.len() > 6 {
                errors.remove(0);
            }
        }
    }

    let status = child.wait().map_err(ImageError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(ImageError::Failed(errors.join("; ")))
    }
}

fn dimensions(tools: &FfmpegTools, path: &Path) -> (u32, u32) {
    crate::ffmpeg::probe::probe(tools, path)
        .map(|info| (info.width, info.height))
        .unwrap_or((0, 0))
}

/// One probe of the quality search, reported so the UI can show the hunt
/// rather than a frozen spinner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ImageStep {
    pub encode: u8,
    pub of: u8,
    pub quality: u8,
    pub bytes: u64,
    pub fits: bool,
}

/// Compress an image to fit under a target size.
pub fn compress_image(
    tools: &FfmpegTools,
    request: &ImageRequest,
    cancel: &CancelToken,
    mut on_step: impl FnMut(ImageStep),
) -> Result<ImageOutcome, ImageError> {
    let mut encodes: u8 = 0;
    let mut factor = 1.0_f64;

    for downscale_steps in 0..=MAX_DOWNSCALE_STEPS {
        // Lossless formats have no quality axis; one encode tells us whether
        // this size fits, and if it does not, only shrinking will help.
        if request.format.is_lossless() {
            run(tools, encode_args(request, 100, factor, &request.output), cancel)?;
            encodes += 1;
            let size = file_size(&request.output)?;

            if size <= request.target_bytes || downscale_steps == MAX_DOWNSCALE_STEPS {
                let (width, height) = dimensions(tools, &request.output);
                return Ok(ImageOutcome {
                    output_bytes: size,
                    quality: 100,
                    width,
                    height,
                    downscale_steps,
                    encodes,
                    within_limit: size <= request.target_bytes,
                });
            }

            factor *= DOWNSCALE_FACTOR;
            continue;
        }

        let mut search = Search::default();
        let mut best_size: Option<u64> = None;

        for _ in 0..MAX_SEARCH_STEPS {
            if search.exhausted() {
                break;
            }
            if cancel.is_cancelled() {
                return Err(ImageError::Cancelled);
            }

            let quality = search.probe();
            run(tools, encode_args(request, quality, factor, &request.output), cancel)?;
            encodes += 1;

            let size = file_size(&request.output)?;
            let fits = size <= request.target_bytes;
            if fits {
                best_size = Some(size);
            }
            on_step(ImageStep {
                encode: encodes,
                of: MAX_SEARCH_STEPS,
                quality,
                bytes: size,
                fits,
            });
            search = search.narrowed(quality, fits);
        }

        if let (Some(quality), Some(size)) = (search.best, best_size) {
            // The last encode in the loop is not necessarily the best one, so
            // write the winner back out before reporting it.
            run(tools, encode_args(request, quality, factor, &request.output), cancel)?;
            encodes += 1;
            let size = file_size(&request.output).unwrap_or(size);
            let (width, height) = dimensions(tools, &request.output);

            return Ok(ImageOutcome {
                output_bytes: size,
                quality,
                width,
                height,
                downscale_steps,
                encodes,
                within_limit: size <= request.target_bytes,
            });
        }

        // Nothing fit at any quality. Shrink and try the whole range again.
        factor *= DOWNSCALE_FACTOR;
    }

    // Out of downscale steps. Whatever is on disk is the smallest we managed.
    let size = file_size(&request.output)?;
    let (width, height) = dimensions(tools, &request.output);

    Ok(ImageOutcome {
        output_bytes: size,
        quality: 1,
        width,
        height,
        downscale_steps: MAX_DOWNSCALE_STEPS,
        encodes,
        within_limit: size <= request.target_bytes,
    })
}

fn file_size(path: &Path) -> Result<u64, ImageError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 0 => Ok(meta.len()),
        _ => Err(ImageError::NoOutput),
    }
}

/// Is this a still image we should route through the image path?
pub fn is_image(path: &Path) -> bool {
    const IMAGE_EXTENSIONS: &[&str] =
        &["png", "jpg", "jpeg", "webp", "avif", "bmp", "tif", "tiff", "heic", "heif"];

    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webp_quality_runs_the_same_direction_as_ours() {
        let low = ImageFormat::Webp.encoder_quality_args(10);
        let high = ImageFormat::Webp.encoder_quality_args(90);

        assert_eq!(low, vec!["-quality".to_string(), "10".to_string()]);
        assert_eq!(high, vec!["-quality".to_string(), "90".to_string()]);
    }

    #[test]
    fn avif_quality_is_inverted_because_crf_is_backwards() {
        let args = |quality: u8| {
            let rendered = ImageFormat::Avif.encoder_quality_args(quality);
            let index = rendered.iter().position(|arg| arg == "-crf").unwrap();
            rendered[index + 1].parse::<u32>().unwrap()
        };

        assert!(args(90) < args(10), "higher quality must mean a lower CRF");
        assert_eq!(args(100), 0);
        assert_eq!(args(1), 62);
    }

    #[test]
    fn jpeg_quality_is_inverted_and_stays_in_range() {
        let qscale = |quality: u8| {
            let rendered = ImageFormat::Jpeg.encoder_quality_args(quality);
            let index = rendered.iter().position(|arg| arg == "-q:v").unwrap();
            rendered[index + 1].parse::<f64>().unwrap()
        };

        assert!(qscale(90) < qscale(10), "higher quality must mean a lower qscale");
        for quality in 1..=100u8 {
            let value = qscale(quality);
            assert!((2.0..=31.0).contains(&value), "qscale {value} out of range at q{quality}");
        }
    }

    #[test]
    fn the_search_converges_on_the_best_quality_that_fits() {
        // Model an encoder where everything at or below 60 fits.
        let mut search = Search::default();
        for _ in 0..MAX_SEARCH_STEPS {
            if search.exhausted() {
                break;
            }
            let quality = search.probe();
            search = search.narrowed(quality, quality <= 60);
        }

        let best = search.best.expect("something should have fit");
        assert!(best <= 60, "chose {best}, which would not have fit");
        assert!(best >= 55, "chose {best}, leaving quality on the table");
    }

    #[test]
    fn the_search_reports_nothing_when_even_the_lowest_quality_busts() {
        let mut search = Search::default();
        for _ in 0..MAX_SEARCH_STEPS {
            if search.exhausted() {
                break;
            }
            let quality = search.probe();
            search = search.narrowed(quality, false);
        }

        assert!(search.best.is_none(), "nothing fits, so there is no best");
    }

    #[test]
    fn the_search_takes_the_top_when_everything_fits() {
        let mut search = Search::default();
        for _ in 0..MAX_SEARCH_STEPS {
            if search.exhausted() {
                break;
            }
            let quality = search.probe();
            search = search.narrowed(quality, true);
        }

        let best = search.best.expect("everything fits");
        assert!(best >= 99, "should reach for the top of the range, got {best}");
    }

    #[test]
    fn the_search_always_terminates() {
        // Every possible fit/no-fit response pattern must exhaust or converge.
        for threshold in 0..=101u8 {
            let mut search = Search::default();
            let mut steps = 0;
            while !search.exhausted() && steps < 64 {
                let quality = search.probe();
                search = search.narrowed(quality, quality <= threshold as u8);
                steps += 1;
            }
            assert!(steps < 64, "search failed to terminate at threshold {threshold}");
        }
    }

    #[test]
    fn scale_filters_are_only_emitted_when_they_do_something() {
        assert!(scale_filter(None, 1.0).is_none());
        assert!(scale_filter(Some(2048), 1.0).unwrap().contains("2048"));
        assert!(scale_filter(None, 0.75).unwrap().contains("0.75"));

        let both = scale_filter(Some(2048), 0.75).unwrap();
        assert!(both.contains("2048") && both.contains("0.75"));
    }

    #[test]
    fn image_extensions_are_recognised_case_insensitively() {
        assert!(is_image(Path::new("shot.PNG")));
        assert!(is_image(Path::new("photo.jpeg")));
        assert!(is_image(Path::new("a.webp")));
        assert!(!is_image(Path::new("clip.mp4")));
        assert!(!is_image(Path::new("noextension")));
    }
}
