//! Fitting the picture to the budget.
//!
//! This is the single biggest quality lever in the app, and the step most
//! size-targeting tools skip. Encoding 1080p at 900 kbps does not produce a
//! soft 1080p — it produces blocking and smear. The same 900 kbps at 540p
//! produces a clean 540p. Given a fixed budget, choosing the resolution is
//! choosing the quality.
//!
//! The measure is bits per pixel: `bitrate / (width * height * fps)`. Each
//! codec has a floor below which it visibly falls apart; we pick the largest
//! picture that still clears it.

use super::{MediaInfo, SharpnessBias, VideoCodec};
use serde::{Deserialize, Serialize};

/// Standard short-side sizes, largest first.
///
/// Applied to the *short* side rather than the height so portrait video
/// (1080x1920) is treated like the 1080-class source it is, instead of being
/// crushed to 607x1080.
const SHORT_SIDE_LADDER: &[u32] = &[2160, 1440, 1080, 900, 720, 540, 480, 360, 240, 144];

/// Framerate fallbacks, best first. We never raise a source's framerate.
///
/// 50 is deliberately absent: 60 -> 50 is a bad step (it needs frame blending
/// and buys almost nothing), while 60 -> 30 is exact frame dropping. A 50 fps
/// source still keeps its own rate, because the source rate is always a
/// candidate whether or not it appears here.
const FPS_LADDER: &[f64] = &[60.0, 30.0, 24.0];

/// Resolution is spent down to this size before framerate is touched at all.
///
/// Without this gate the search would happily return 240p60, having cleared
/// the bpp floor by shrinking the picture ninefold rather than dropping half
/// the frames. Below 480p, detail loss hurts more than judder does.
const FPS_DROP_GATE: u32 = 480;

/// The chosen output geometry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scale {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Bits per pixel this selection achieves. Surfaced for diagnostics.
    pub bpp: f64,
    /// True when the floor could not be met even at the smallest rung.
    ///
    /// Not an error — `budget::compute` has already refused the genuinely
    /// impossible ones, so this means "as good as this budget gets".
    pub below_floor: bool,
}

impl Scale {
    pub fn is_rescaled_from(&self, info: &MediaInfo) -> bool {
        self.width != info.width || self.height != info.height
    }

    pub fn is_framerate_reduced_from(&self, info: &MediaInfo) -> bool {
        self.fps < info.fps - 0.01
    }
}

/// Round to an even number — yuv420p chroma subsampling requires it, and an
/// odd dimension makes libx264 fail outright rather than fix it for you.
fn even(value: f64) -> u32 {
    let rounded = (value / 2.0).round() * 2.0;
    (rounded as u32).max(2)
}

fn bits_per_pixel(video_bps: u64, width: u32, height: u32, fps: f64) -> f64 {
    video_bps as f64 / (width as f64 * height as f64 * fps)
}

/// Short-side rungs at or below the source, plus the source itself.
///
/// Never upscales: a 480p source stays 480p even on a generous budget, because
/// inventing pixels costs bits and adds nothing.
fn short_side_candidates(source_short: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(SHORT_SIDE_LADDER.len() + 1);
    if !SHORT_SIDE_LADDER.contains(&source_short) {
        out.push(source_short);
    }
    out.extend(SHORT_SIDE_LADDER.iter().copied().filter(|&rung| rung <= source_short));
    out
}

/// Framerates at or below the source, best first.
///
/// The source rate is always the first candidate — including when it is itself
/// a ladder rung, which is why the filter is `<=` rather than `<`. With `<`, a
/// 60 fps source would silently lose 60 as an option and start the search at 30.
fn fps_candidates(source_fps: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(FPS_LADDER.len() + 1);
    if !FPS_LADDER.iter().any(|&rung| (rung - source_fps).abs() < 0.01) {
        out.push(source_fps);
    }
    out.extend(FPS_LADDER.iter().copied().filter(|&rung| rung <= source_fps + 0.01));
    out
}

fn geometry(info: &MediaInfo, short_side: u32) -> (u32, u32) {
    let source_short = info.width.min(info.height);
    let factor = short_side as f64 / source_short as f64;
    (even(info.width as f64 * factor), even(info.height as f64 * factor))
}

/// Choose the largest picture the budget can carry cleanly.
pub fn select(info: &MediaInfo, video_bps: u64, codec: VideoCodec, bias: SharpnessBias) -> Scale {
    let floor = codec.bpp_floor(bias);
    let source_short = info.width.min(info.height);
    let shorts = short_side_candidates(source_short);
    let rates = fps_candidates(info.fps);

    let build = |short_side: u32, fps: f64, below_floor: bool| {
        let (width, height) = geometry(info, short_side);
        Scale { width, height, fps, bpp: bits_per_pixel(video_bps, width, height, fps), below_floor }
    };

    // Pass 1 — spend resolution down to the gate before touching framerate.
    // Pass 2 — at the slowest framerate, keep spending resolution.
    let gated = shorts.iter().copied().filter(|&s| s >= FPS_DROP_GATE);
    let slowest = *rates.last().expect("candidates always include the source rate");

    for &fps in &rates {
        for short_side in gated.clone() {
            let candidate = build(short_side, fps, false);
            if candidate.bpp >= floor {
                return candidate;
            }
        }
    }

    for &short_side in &shorts {
        let candidate = build(short_side, slowest, false);
        if candidate.bpp >= floor {
            return candidate;
        }
    }

    // Nothing clears the floor. Return the smallest rung and say so.
    build(*shorts.last().expect("ladder is never empty"), slowest, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::AudioInfo;

    fn source(width: u32, height: u32, fps: f64) -> MediaInfo {
        MediaInfo {
            duration_secs: 180.0,
            width,
            height,
            fps,
            audio: Some(AudioInfo { channels: 2 }),
        }
    }

    const BALANCED: SharpnessBias = SharpnessBias::Balanced;

    #[test]
    fn a_generous_budget_keeps_the_source_untouched() {
        let info = source(1920, 1080, 60.0);
        // 0.055 floor at 1080p60 needs ~6.8 Mbps; give it 12.
        let scale = select(&info, 12_000_000, VideoCodec::H264, BALANCED);

        assert_eq!((scale.width, scale.height), (1920, 1080));
        assert_eq!(scale.fps, 60.0);
        assert!(!scale.is_rescaled_from(&info));
        assert!(!scale.below_floor);
    }

    #[test]
    fn never_upscales_a_small_source() {
        let info = source(854, 480, 30.0);
        let scale = select(&info, 50_000_000, VideoCodec::H264, BALANCED);

        assert_eq!((scale.width, scale.height), (854, 480));
        assert_eq!(scale.fps, 30.0);
    }

    #[test]
    fn a_discord_sized_budget_drops_a_1080p60_clip_to_something_watchable() {
        let info = source(1920, 1080, 60.0);
        // ~735 kbps: what 20 MB over three minutes actually buys.
        let scale = select(&info, 735_000, VideoCodec::H264, BALANCED);

        assert!(scale.bpp >= VideoCodec::H264.bpp_floor(BALANCED), "bpp was {}", scale.bpp);
        assert!(scale.height <= 540, "expected a real downscale, got {}p", scale.height);
        assert!(!scale.below_floor);
    }

    #[test]
    fn spends_framerate_before_dropping_below_480p() {
        // Chosen so that at 60fps nothing at or above 480p clears the floor,
        // but 480p30 does. Without the gate this would return 240p60.
        let info = source(1920, 1080, 60.0);
        let scale = select(&info, 700_000, VideoCodec::H264, BALANCED);

        assert!(
            scale.height >= 480,
            "should have spent framerate before shrinking past 480p, got {}p{}",
            scale.height,
            scale.fps
        );
        assert!(scale.fps < 60.0, "expected a framerate drop, got {}", scale.fps);
    }

    #[test]
    fn portrait_video_is_scaled_by_its_short_side() {
        // A 1080x1920 phone clip is a 1080-class source, not a 1920-class one.
        let info = source(1080, 1920, 30.0);
        let scale = select(&info, 1_500_000, VideoCodec::H264, BALANCED);

        assert!(scale.width < scale.height, "portrait orientation must be preserved");
        let source_aspect = 1080.0 / 1920.0;
        let out_aspect = scale.width as f64 / scale.height as f64;
        assert!((source_aspect - out_aspect).abs() < 0.02, "aspect drifted to {out_aspect}");
    }

    #[test]
    fn every_dimension_is_even() {
        // 1918x804 is deliberately awkward: odd-ish and cinematic.
        for bps in [400_000u64, 900_000, 2_000_000, 8_000_000] {
            let scale = select(&source(1918, 804, 29.97), bps, VideoCodec::H264, BALANCED);
            assert_eq!(scale.width % 2, 0, "odd width {} at {bps}bps", scale.width);
            assert_eq!(scale.height % 2, 0, "odd height {} at {bps}bps", scale.height);
        }
    }

    #[test]
    fn av1_keeps_more_resolution_than_h264_on_the_same_budget() {
        let info = source(1920, 1080, 30.0);
        let h264 = select(&info, 1_200_000, VideoCodec::H264, BALANCED);
        let av1 = select(&info, 1_200_000, VideoCodec::Av1, BALANCED);

        assert!(
            av1.height >= h264.height,
            "av1 ({}p) should hold at least as much resolution as h264 ({}p)",
            av1.height,
            h264.height
        );
    }

    #[test]
    fn sharpness_bias_trades_resolution_for_crispness() {
        let info = source(1920, 1080, 30.0);
        let bps = 1_200_000;

        let resolution_first = select(&info, bps, VideoCodec::H264, SharpnessBias::ResolutionFirst);
        let sharpness_first = select(&info, bps, VideoCodec::H264, SharpnessBias::SharpnessFirst);

        assert!(
            resolution_first.height >= sharpness_first.height,
            "resolution-first ({}p) should not be smaller than sharpness-first ({}p)",
            resolution_first.height,
            sharpness_first.height
        );
    }

    #[test]
    fn a_hopeless_budget_bottoms_out_and_admits_it() {
        let info = source(1920, 1080, 60.0);
        let scale = select(&info, 20_000, VideoCodec::H264, BALANCED);

        assert!(scale.below_floor, "should flag that the floor was never met");
        assert_eq!(scale.fps, 24.0, "should have spent every framerate rung");
    }

    #[test]
    fn source_framerate_is_preserved_when_it_is_not_on_the_ladder() {
        let info = source(1280, 720, 29.97);
        let scale = select(&info, 6_000_000, VideoCodec::H264, BALANCED);
        assert_eq!(scale.fps, 29.97);
    }
}
