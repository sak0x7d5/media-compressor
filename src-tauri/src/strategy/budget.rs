//! Turning "fit under N bytes" into a bitrate.
//!
//! The split between video and audio matters more than it looks. On a 20 MB
//! cap and a three-minute clip the whole budget is ~880 kbps; 128 kbps of
//! stereo AAC would be 15% of the file. Audio gets a ceiling, and gives ground
//! before video does.

use super::{MediaInfo, PlanError, Target};

/// Fraction of the target reserved for container overhead — the moov atom,
/// per-packet framing, the faststart relocation. Measured at roughly 1% on
/// long files and worse on short ones; 2% is the safe side of that.
const CONTAINER_OVERHEAD: f64 = 0.02;

/// Audio bitrate ladders in bps, best first.
///
/// Starts at 96k rather than 128k deliberately: at these budgets the extra
/// 32 kbps buys less than the video loses.
const AUDIO_LADDER_STEREO: &[u32] = &[96_000, 64_000, 48_000, 32_000];
const AUDIO_LADDER_MONO: &[u32] = &[64_000, 48_000, 32_000, 24_000];

/// Audio never takes more than this share of the budget.
const MAX_AUDIO_SHARE: f64 = 0.20;

/// Below this, no resolution rescues the video and we should say so rather
/// than produce a smear.
const MIN_VIDEO_BPS: u64 = 90_000;

/// The bitrate split a plan is built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The size we are planning against, after the safety margin.
    pub total_bytes: u64,
    pub video_bps: u64,
    /// Zero when the source has no audio stream.
    pub audio_bps: u32,
}

impl Budget {
    /// Bits available to the muxed file, after reserving container overhead.
    fn usable_bits(total_bytes: u64) -> f64 {
        total_bytes as f64 * 8.0 * (1.0 - CONTAINER_OVERHEAD)
    }
}

/// Pick the best audio rung that stays under the audio share ceiling.
///
/// Returns the *lowest* rung when even that busts the ceiling — the caller
/// finds out the target is impossible from the video bitrate, which is a
/// better error than "audio did not fit".
fn pick_audio_bps(channels: u8, usable_bits: f64, duration_secs: f64) -> u32 {
    let ladder = if channels >= 2 { AUDIO_LADDER_STEREO } else { AUDIO_LADDER_MONO };
    let ceiling_bits = usable_bits * MAX_AUDIO_SHARE;

    ladder
        .iter()
        .copied()
        .find(|&bps| (bps as f64) * duration_secs <= ceiling_bits)
        .unwrap_or_else(|| *ladder.last().expect("ladders are never empty"))
}

/// Split a target size into a video and audio bitrate.
pub fn compute(info: &MediaInfo, target: Target) -> Result<Budget, PlanError> {
    if !(info.duration_secs > 0.0) {
        return Err(PlanError::ZeroDuration);
    }
    if info.width == 0 || info.height == 0 {
        return Err(PlanError::InvalidDimensions { width: info.width, height: info.height });
    }

    let total_bytes = target.effective_bytes();
    let usable_bits = Budget::usable_bits(total_bytes);

    let audio_bps = match info.audio {
        Some(audio) => pick_audio_bps(audio.channels, usable_bits, info.duration_secs),
        None => 0,
    };

    let audio_bits = audio_bps as f64 * info.duration_secs;
    let video_bits = usable_bits - audio_bits;
    let video_bps = (video_bits / info.duration_secs).floor();

    if video_bps < MIN_VIDEO_BPS as f64 {
        return Err(PlanError::TargetTooSmall {
            target_bytes: target.limit_bytes,
            duration_secs: info.duration_secs.round() as u64,
            minimum_bytes: minimum_viable_bytes(info, target),
        });
    }

    Ok(Budget { total_bytes, video_bps: video_bps as u64, audio_bps })
}

/// The smallest target that could hold this source, for the error message.
///
/// Inverts the whole calculation: floor video bitrate plus the cheapest audio
/// rung, grossed back up through container overhead and the safety margin.
fn minimum_viable_bytes(info: &MediaInfo, target: Target) -> u64 {
    let cheapest_audio = match info.audio {
        Some(audio) if audio.channels >= 2 => *AUDIO_LADDER_STEREO.last().unwrap(),
        Some(_) => *AUDIO_LADDER_MONO.last().unwrap(),
        None => 0,
    };

    let bits = (MIN_VIDEO_BPS + cheapest_audio as u64) as f64 * info.duration_secs;
    let bytes = bits / 8.0 / (1.0 - CONTAINER_OVERHEAD) / target.safety_margin;
    bytes.ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::AudioInfo;

    fn source(duration_secs: f64, channels: Option<u8>) -> MediaInfo {
        MediaInfo {
            duration_secs,
            width: 1920,
            height: 1080,
            fps: 60.0,
            audio: channels.map(|channels| AudioInfo { channels }),
        }
    }

    const MB20: u64 = 20 * 1000 * 1000;

    #[test]
    fn safety_margin_is_applied_before_anything_else() {
        let target = Target::new(MB20);
        assert_eq!(target.effective_bytes(), 19_000_000);
    }

    #[test]
    fn splits_a_20mb_target_over_three_minutes() {
        let budget = compute(&source(180.0, Some(2)), Target::new(MB20)).unwrap();

        // 19 MB, less 2% overhead, is 148.96 Mbit over 180s = ~827 kbps total.
        // Stereo at 96k is 17.28 Mbit, comfortably under the 20% ceiling.
        assert_eq!(budget.audio_bps, 96_000);
        assert_eq!(budget.total_bytes, 19_000_000);
        assert!(
            (730_000..=740_000).contains(&budget.video_bps),
            "video_bps was {}",
            budget.video_bps
        );
    }

    #[test]
    fn audio_steps_down_rather_than_eating_the_budget() {
        // 20 MB over 40 minutes: the whole budget is ~62 kbps, so 96k stereo
        // audio alone would be larger than the entire file.
        let budget = compute(&source(2400.0, Some(2)), Target::new(MB20));

        // It must not silently hand audio the file; it must refuse the target.
        assert!(matches!(budget, Err(PlanError::TargetTooSmall { .. })), "got {budget:?}");
    }

    #[test]
    fn tight_but_viable_targets_drop_audio_down_the_ladder() {
        // 25 MB over 10 minutes: 186.2 Mbit usable, so the 20% audio ceiling is
        // 37.24 Mbit. 96k would need 57.6 Mbit and 64k would need 38.4 Mbit —
        // over by 3% — so 48k is the best rung that actually fits.
        let budget = compute(&source(600.0, Some(2)), Target::new(25 * 1000 * 1000)).unwrap();
        assert_eq!(budget.audio_bps, 48_000);
        assert!(budget.video_bps > MIN_VIDEO_BPS, "the target should still be viable");
    }

    #[test]
    fn mono_sources_get_the_mono_ladder() {
        let budget = compute(&source(180.0, Some(1)), Target::new(MB20)).unwrap();
        assert_eq!(budget.audio_bps, 64_000);
    }

    #[test]
    fn silent_sources_spend_nothing_on_audio() {
        let budget = compute(&source(180.0, None), Target::new(MB20)).unwrap();
        assert_eq!(budget.audio_bps, 0);

        let with_audio = compute(&source(180.0, Some(2)), Target::new(MB20)).unwrap();
        assert!(
            budget.video_bps > with_audio.video_bps,
            "a silent source should give its audio budget to video"
        );
    }

    #[test]
    fn impossible_targets_are_refused_with_a_usable_number() {
        let err = compute(&source(600.0, Some(2)), Target::new(1_000_000)).unwrap_err();

        match err {
            PlanError::TargetTooSmall { minimum_bytes, duration_secs, .. } => {
                assert_eq!(duration_secs, 600);
                // Floor video (90k) + cheapest stereo audio (32k) over 600s.
                assert!(
                    (9_000_000..=11_000_000).contains(&minimum_bytes),
                    "minimum_bytes was {minimum_bytes}"
                );
            }
            other => panic!("expected TargetTooSmall, got {other:?}"),
        }
    }

    #[test]
    fn zero_duration_is_an_error_not_a_division_by_zero() {
        assert_eq!(compute(&source(0.0, Some(2)), Target::new(MB20)), Err(PlanError::ZeroDuration));
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        let mut info = source(10.0, Some(2));
        info.width = 0;
        assert!(matches!(
            compute(&info, Target::new(MB20)),
            Err(PlanError::InvalidDimensions { .. })
        ));
    }
}
