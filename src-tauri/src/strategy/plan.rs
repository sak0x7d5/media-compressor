//! Deciding how to encode: quality-targeted when the file already fits,
//! size-targeted when it does not.
//!
//! The order matters. We try the *native* picture at a quality target first,
//! because an easy source may already fit under the cap at full resolution —
//! and downscaling it would throw away detail to solve a problem that does not
//! exist. Only when the optimistic encode is predicted to bust the cap do we
//! compute a budget, fit the picture to it, and switch to two-pass.

use super::budget::{self, Budget};
use super::ladder::{self, Scale};
use super::{MediaInfo, PlanError, SharpnessBias, Target, VideoCodec};
use serde::{Deserialize, Serialize};

/// How many encodes we are willing to spend on one file, corrections included.
///
/// An unbounded bisection converges beautifully and turns a 40-second job into
/// four minutes. Two corrections is enough in practice.
pub const MAX_ATTEMPTS: u8 = 3;

/// A two-pass result this far under budget means rate control missed, not that
/// the source was easy.
const UNDERSHOOT_TOLERANCE: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum RateControl {
    /// Quality-targeted. Spends *fewer* bits than the budget on easy sources,
    /// which is the whole reason to prefer it when the file already fits.
    Crf { crf: u8 },
    /// Size-targeted. Pass one gathers scene complexity, pass two spends the
    /// budget where the complexity is.
    TwoPass { video_bps: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub codec: VideoCodec,
    pub bias: SharpnessBias,
    /// Overrides the codec's default quality target.
    pub crf: Option<u8>,
}

impl Default for Options {
    fn default() -> Self {
        Self { codec: VideoCodec::H264, bias: SharpnessBias::default(), crf: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodePlan {
    pub codec: VideoCodec,
    pub scale: Scale,
    pub rate_control: RateControl,
    pub audio_bps: u32,
    /// 1 for the first encode, incremented by each correction.
    pub attempt: u8,
}

impl EncodePlan {
    pub fn is_two_pass(&self) -> bool {
        matches!(self.rate_control, RateControl::TwoPass { .. })
    }

    pub fn video_bps(&self) -> Option<u64> {
        match self.rate_control {
            RateControl::TwoPass { video_bps } => Some(video_bps),
            RateControl::Crf { .. } => None,
        }
    }
}

/// Everything the decisions are made against, computed once per file.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanContext {
    pub info: MediaInfo,
    pub target: Target,
    pub options: Options,
    pub budget: Budget,
}

impl PlanContext {
    pub fn new(info: MediaInfo, target: Target, options: Options) -> Result<Self, PlanError> {
        let budget = budget::compute(&info, target)?;
        Ok(Self { info, target, options, budget })
    }

    fn crf(&self) -> u8 {
        self.options.crf.unwrap_or_else(|| self.options.codec.default_crf())
    }

    /// The picture at native geometry — what the optimistic encode uses.
    fn native_scale(&self) -> Scale {
        Scale {
            width: self.info.width,
            height: self.info.height,
            fps: self.info.fps,
            // Informational only on this path: a CRF encode picks its own
            // bitrate, so this reports what the *budget* would afford here.
            bpp: self.budget.video_bps as f64
                / (self.info.width as f64 * self.info.height as f64 * self.info.fps),
            below_floor: false,
        }
    }

    /// The encode we try first: full resolution, quality-targeted.
    ///
    /// Run this against a short sample to predict the full-file size, then
    /// hand the prediction to [`Self::decide`].
    pub fn optimistic(&self) -> EncodePlan {
        EncodePlan {
            codec: self.options.codec,
            scale: self.native_scale(),
            rate_control: RateControl::Crf { crf: self.crf() },
            audio_bps: self.budget.audio_bps,
            attempt: 1,
        }
    }

    /// The plan we commit to, given what the optimistic encode is predicted to weigh.
    pub fn decide(&self, predicted_bytes: u64) -> EncodePlan {
        if predicted_bytes <= self.budget.total_bytes {
            // It already fits. Ship it at full resolution and spend fewer bits
            // than the budget allows — inflating an easy file to the cap buys
            // nothing.
            return self.optimistic();
        }

        let scale =
            ladder::select(&self.info, self.budget.video_bps, self.options.codec, self.options.bias);

        EncodePlan {
            codec: self.options.codec,
            scale,
            rate_control: RateControl::TwoPass { video_bps: self.budget.video_bps },
            audio_bps: self.budget.audio_bps,
            attempt: 1,
        }
    }

    /// A corrective re-encode, if the real output warrants one.
    ///
    /// Overshoot is always corrected — the file is unusable otherwise.
    ///
    /// Undershoot is corrected **only for two-pass**, where landing short means
    /// rate control missed. On the CRF path, landing short is the intended
    /// outcome: the source was easy, quality is already at target, and
    /// re-encoding at a lower CRF would spend a lot of bits for very little.
    pub fn correct(&self, plan: &EncodePlan, actual_bytes: u64) -> Option<EncodePlan> {
        if plan.attempt >= MAX_ATTEMPTS {
            return None;
        }

        let over = actual_bytes > self.target.limit_bytes;
        let short = (actual_bytes as f64)
            < self.budget.total_bytes as f64 * (1.0 - UNDERSHOOT_TOLERANCE);

        if !over && !(short && plan.is_two_pass()) {
            return None;
        }

        let next_bps = self.rescaled_video_bps(plan, actual_bytes)?;

        // A correction always uses two-pass: we now know the exact size we are
        // aiming at, which is precisely what two-pass is for.
        let scale = ladder::select(&self.info, next_bps, self.options.codec, self.options.bias);

        Some(EncodePlan {
            codec: self.options.codec,
            scale,
            rate_control: RateControl::TwoPass { video_bps: next_bps },
            audio_bps: plan.audio_bps,
            attempt: plan.attempt + 1,
        })
    }

    /// Scale the video bitrate by how far the real output missed.
    ///
    /// Audio is a fixed cost, so it comes out of both sides before the ratio is
    /// taken — otherwise a file that is mostly audio produces a wild correction.
    fn rescaled_video_bps(&self, plan: &EncodePlan, actual_bytes: u64) -> Option<u64> {
        let audio_bytes = (plan.audio_bps as f64 * self.info.duration_secs / 8.0).round();

        let actual_video = actual_bytes as f64 - audio_bytes;
        let desired_video = self.budget.total_bytes as f64 - audio_bytes;
        if actual_video <= 0.0 || desired_video <= 0.0 {
            return None;
        }

        // The bitrate that produced `actual_video`. On the CRF path we have no
        // configured bitrate to scale, so derive it from what actually landed.
        let observed_bps = match plan.rate_control {
            RateControl::TwoPass { video_bps } => video_bps as f64,
            RateControl::Crf { .. } => actual_video * 8.0 / self.info.duration_secs,
        };

        let corrected = observed_bps * (desired_video / actual_video);
        Some(corrected.floor().max(1.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::AudioInfo;

    const MB20: u64 = 20 * 1000 * 1000;

    fn source(duration_secs: f64, width: u32, height: u32, fps: f64) -> MediaInfo {
        MediaInfo {
            duration_secs,
            width,
            height,
            fps,
            audio: Some(AudioInfo { channels: 2 }),
        }
    }

    fn context(info: MediaInfo, limit: u64) -> PlanContext {
        PlanContext::new(info, Target::new(limit), Options::default()).unwrap()
    }

    #[test]
    fn an_easy_file_that_already_fits_stays_at_full_resolution() {
        let ctx = context(source(30.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(8_000_000);

        assert_eq!((plan.scale.width, plan.scale.height), (1920, 1080));
        assert_eq!(plan.scale.fps, 60.0);
        assert!(matches!(plan.rate_control, RateControl::Crf { crf: 23 }));
    }

    #[test]
    fn a_file_that_busts_the_cap_switches_to_two_pass_and_downscales() {
        let ctx = context(source(180.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(140_000_000);

        assert!(plan.is_two_pass(), "expected two-pass, got {:?}", plan.rate_control);
        assert!(plan.scale.height < 1080, "expected a downscale, got {}p", plan.scale.height);
        assert_eq!(plan.video_bps(), Some(ctx.budget.video_bps));
    }

    #[test]
    fn the_crf_path_is_preferred_right_up_to_the_budget() {
        let ctx = context(source(60.0, 1280, 720, 30.0), MB20);

        let exactly_fits = ctx.decide(ctx.budget.total_bytes);
        assert!(!exactly_fits.is_two_pass(), "a file that exactly fits should stay on CRF");

        let one_byte_over = ctx.decide(ctx.budget.total_bytes + 1);
        assert!(one_byte_over.is_two_pass(), "a file over budget must switch to two-pass");
    }

    #[test]
    fn overshoot_is_corrected_downward() {
        let ctx = context(source(180.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(140_000_000);

        // Came out at 24 MB against a 20 MB limit.
        let fix = ctx.correct(&plan, 24_000_000).expect("overshoot must be corrected");

        assert_eq!(fix.attempt, 2);
        assert!(fix.is_two_pass());
        assert!(
            fix.video_bps().unwrap() < plan.video_bps().unwrap(),
            "correction should lower the bitrate"
        );
    }

    #[test]
    fn a_result_comfortably_under_the_limit_is_left_alone() {
        let ctx = context(source(180.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(140_000_000);

        // Landed at 18.5 MB against a 19 MB budget: on target.
        assert!(ctx.correct(&plan, 18_500_000).is_none());
    }

    #[test]
    fn an_easy_crf_file_that_lands_far_under_is_not_re_encoded() {
        // This is the deliberate asymmetry: undershooting on the CRF path is
        // the intended outcome, not a miss to correct.
        let ctx = context(source(30.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(5_000_000);

        assert!(!plan.is_two_pass());
        assert!(
            ctx.correct(&plan, 5_000_000).is_none(),
            "a small CRF result is a success, not something to inflate"
        );
    }

    #[test]
    fn a_two_pass_file_that_lands_far_under_is_re_encoded_larger() {
        let ctx = context(source(180.0, 1920, 1080, 60.0), MB20);
        let plan = ctx.decide(140_000_000);

        // Two-pass aimed at ~19 MB and produced 12 MB: rate control missed.
        let fix = ctx.correct(&plan, 12_000_000).expect("a two-pass miss should be corrected");

        assert!(
            fix.video_bps().unwrap() > plan.video_bps().unwrap(),
            "correction should raise the bitrate"
        );
    }

    #[test]
    fn corrections_stop_after_the_attempt_budget() {
        let ctx = context(source(180.0, 1920, 1080, 60.0), MB20);
        let mut plan = ctx.decide(140_000_000);

        // Two corrections are allowed; the third must be refused.
        plan = ctx.correct(&plan, 30_000_000).expect("first correction");
        plan = ctx.correct(&plan, 28_000_000).expect("second correction");
        assert_eq!(plan.attempt, MAX_ATTEMPTS);
        assert!(ctx.correct(&plan, 26_000_000).is_none(), "must stop at MAX_ATTEMPTS");
    }

    #[test]
    fn an_impossible_target_is_refused_at_construction() {
        let err = PlanContext::new(
            source(3600.0, 1920, 1080, 60.0),
            Target::new(1_000_000),
            Options::default(),
        )
        .unwrap_err();

        assert!(matches!(err, PlanError::TargetTooSmall { .. }), "got {err:?}");
    }

    #[test]
    fn the_correction_accounts_for_audio_being_a_fixed_cost() {
        // A short clip where audio is a large share of the file — which is
        // exactly where a naive total-bytes ratio goes wrong.
        let duration = 20.0;
        let ctx = context(source(duration, 1280, 720, 30.0), 1_000_000);
        let plan = ctx.decide(5_000_000);

        let configured_bps = plan.video_bps().unwrap();
        let actual_bytes = 1_200_000u64;
        let fix = ctx.correct(&plan, actual_bytes).expect("overshoot must be corrected");

        let audio_bytes = plan.audio_bps as f64 * duration / 8.0;
        let bytes_from = |bps: u64| bps as f64 * duration / 8.0;
        let budget = ctx.budget.total_bytes as f64;

        // The encoder exceeded its configured bitrate by this factor. Assume it
        // does so again on the retry, and the correction should land on budget.
        let overshoot = (actual_bytes as f64 - audio_bytes) / bytes_from(configured_bps);
        let predicted = bytes_from(fix.video_bps().unwrap()) * overshoot + audio_bytes;

        assert!(
            (predicted - budget).abs() < budget * 0.01,
            "corrected encode should land on the budget ({budget}), predicted {predicted:.0}"
        );

        // Scaling by total bytes instead would leave audio's fixed cost sitting
        // on top of an already-full budget, and bust the target a second time.
        let naive_bps = (configured_bps as f64 * (budget / actual_bytes as f64)) as u64;
        let naive_predicted = bytes_from(naive_bps) * overshoot + audio_bytes;

        assert!(
            naive_predicted > budget,
            "the naive correction should overshoot ({naive_predicted:.0} vs {budget})"
        );
    }
}
