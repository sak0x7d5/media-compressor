//! Size-targeted encode planning.
//!
//! Everything in this module is a pure function over plain structs: no FFmpeg,
//! no I/O, no async. That is deliberate — this is where the quality decisions
//! live, so it has to be testable without spawning a process. The tests are the
//! specification.

pub mod budget;
pub mod ladder;
pub mod plan;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What `ffprobe` told us about the source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// `None` when the source has no audio stream at all.
    pub audio: Option<AudioInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub channels: u8,
}

/// The size we are aiming at, before the safety margin is applied.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Target {
    /// The platform's stated limit, in bytes.
    pub limit_bytes: u64,
    /// Fraction of the limit to actually aim for. Defaults to 0.95.
    ///
    /// "20 MB" is ambiguous between 20,000,000 and 20,971,520 bytes and
    /// platforms disagree about which they mean, so we never aim at the line.
    pub safety_margin: f64,
}

impl Target {
    pub const DEFAULT_SAFETY_MARGIN: f64 = 0.95;

    pub fn new(limit_bytes: u64) -> Self {
        Self { limit_bytes, safety_margin: Self::DEFAULT_SAFETY_MARGIN }
    }

    /// The size we actually plan against.
    pub fn effective_bytes(&self) -> u64 {
        (self.limit_bytes as f64 * self.safety_margin).floor() as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    /// Bits per pixel below which this codec visibly falls apart.
    ///
    /// Measured as `bitrate / (width * height * fps)`. The H.264 number is the
    /// anchor: 1080p30 at 5 Mbps is ~0.080 bpp and looks good, ~0.055 is the
    /// point where softness sets in but blocking has not yet started.
    fn base_bpp_floor(self) -> f64 {
        match self {
            Self::H264 => 0.055,
            Self::Hevc => 0.037,
            Self::Av1 => 0.033,
        }
    }

    /// The CRF we encode at when the file already fits the budget.
    pub fn default_crf(self) -> u8 {
        match self {
            Self::H264 => 23,
            Self::Hevc => 28,
            Self::Av1 => 32,
        }
    }

    pub fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::Hevc => "libx265",
            Self::Av1 => "libsvtav1",
        }
    }
}

/// How to spend the budget when resolution and sharpness compete for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharpnessBias {
    /// Keep resolution high and accept softness.
    ResolutionFirst,
    #[default]
    Balanced,
    /// Drop resolution aggressively to keep every pixel crisp.
    SharpnessFirst,
}

impl SharpnessBias {
    fn floor_multiplier(self) -> f64 {
        match self {
            Self::ResolutionFirst => 0.72,
            Self::Balanced => 1.0,
            Self::SharpnessFirst => 1.36,
        }
    }
}

impl VideoCodec {
    pub fn bpp_floor(self, bias: SharpnessBias) -> f64 {
        self.base_bpp_floor() * bias.floor_multiplier()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error(
        "a {target_bytes} byte target cannot hold {duration_secs} seconds of video: \
         even at the lowest settings this needs about {minimum_bytes} bytes"
    )]
    TargetTooSmall { target_bytes: u64, duration_secs: u64, minimum_bytes: u64 },

    #[error("source reports a duration of zero; nothing to encode")]
    ZeroDuration,

    #[error("source reports invalid dimensions ({width}x{height})")]
    InvalidDimensions { width: u32, height: u32 },
}
