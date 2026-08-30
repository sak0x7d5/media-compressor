//! Real encodes against real FFmpeg.
//!
//! These generate their own source clips rather than checking a binary into the
//! repo, which also means the fixtures cannot drift from what the tests assume.
//!
//! Skipped with a printed note when FFmpeg cannot be found, so `cargo test` on
//! a bare machine stays green instead of failing for the wrong reason.

use media_compressor_lib::ffmpeg::encode::{self, CancelToken, EncodeJob, Pass, Speed};
use media_compressor_lib::ffmpeg::probe::probe;
use media_compressor_lib::ffmpeg::tools::FfmpegTools;
use media_compressor_lib::images::ImageFormat;
use media_compressor_lib::pipeline::{compress, compress_media, CompressRequest, Stage};
use media_compressor_lib::strategy::ladder::Scale;
use media_compressor_lib::strategy::plan::{EncodePlan, Options, RateControl};
use media_compressor_lib::strategy::{Target, VideoCodec};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tools() -> Option<FfmpegTools> {
    // Same resolution the app uses, so these run against whatever FFmpeg the
    // machine actually offers — including one on PATH, which is how they find
    // anything on a dev box that has never run the installer.
    let cache = std::env::temp_dir().join("media-compressor-e2e-cache");
    let found = media_compressor_lib::ffmpeg::resolve(&cache)?;
    found.verify().ok()?;
    Some(found)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("media-compressor-e2e")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a source clip that is genuinely expensive to encode, so a target of a
/// couple of megabytes forces real decisions rather than fitting by accident.
fn make_source(tools: &FfmpegTools, path: &Path, seconds: u32, width: u32, height: u32, fps: u32) {
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={width}x{height}:rate={fps}"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", &seconds.to_string()])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k"])
        .arg(path)
        .status()
        .expect("ffmpeg should run");

    assert!(status.success(), "failed to build the test source");
    assert!(path.is_file(), "test source was not written");
}

fn request(input: &Path, output: &Path, work: &Path, limit: u64) -> CompressRequest {
    CompressRequest {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        target: Target::new(limit),
        options: Options::default(),
        // Test runs are about correctness, not compression efficiency.
        speed: Speed::Fast,
        work_dir: work.to_path_buf(),
        image_format: ImageFormat::Webp,
        max_dimension: None,
    }
}

/// The encoder check, against a real build rather than a fixture.
///
/// What it can assert depends on the machine, so it does not assume the build
/// has any particular encoder. It asserts the thing that must hold everywhere:
/// that our answer matches what `ffmpeg -encoders` actually printed. That is
/// what a hand-written parser gets wrong, and a fixture cannot catch a change
/// in the real output format.
#[test]
fn the_encoder_check_agrees_with_what_ffmpeg_printed() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let listing = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("a working ffmpeg lists its encoders");
    let printed = String::from_utf8_lossy(&listing.stdout);

    let missing = tools.missing_encoders().expect("a working ffmpeg answers -encoders");

    for codec in VideoCodec::ALL {
        let encoder = codec.ffmpeg_encoder();
        // The name column, standing alone — which is what "has this encoder"
        // means, as distinct from being named in another encoder's description.
        let present = printed
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1))
            .any(|name| name == encoder);

        assert_eq!(
            !present,
            missing.contains(&encoder),
            "{encoder} is {}listed, so it must {}be reported missing",
            if present { "" } else { "not " },
            if present { "not " } else { "" }
        );
    }
}

#[test]
fn a_long_clip_is_sampled_then_encoded_under_the_target() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("long");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");

    // Over the 20s threshold, so this exercises the sample-and-predict path.
    make_source(&tools, &input, 25, 1280, 720, 30);

    let limit = 2 * 1000 * 1000;
    let mut stages: Vec<String> = Vec::new();

    let outcome = compress(
        &tools,
        &request(&input, &output, &dir.join("work"), limit),
        &CancelToken::new(),
        |stage| {
            let label = match stage {
                Stage::Probing => "probing".to_string(),
                Stage::Predicting => "predicting".to_string(),
                Stage::Planned { .. } => "planned".to_string(),
                Stage::Encoding(_) => "encoding".to_string(),
                Stage::Correcting { .. } => "correcting".to_string(),
                Stage::Searching(_) => "searching".to_string(),
            };
            if stages.last() != Some(&label) {
                stages.push(label);
            }
        },
    )
    .expect("compression should succeed");

    assert!(outcome.within_limit, "output was {} bytes, limit {limit}", outcome.output_bytes);
    assert!(outcome.output_bytes <= limit, "output busted the limit");

    // The real point: it should use most of the budget. Landing at 300 KB when
    // 1.9 MB was available means quality was thrown away for nothing.
    assert!(
        outcome.output_bytes > limit / 2,
        "output was only {} bytes of a {limit} byte budget — the budget was wasted",
        outcome.output_bytes
    );

    assert!(stages.contains(&"predicting".to_string()), "a 25s clip should be sampled");
    assert!(output.is_file(), "output file should exist");

    // And it should still be a real, readable video.
    let result = probe(&tools, &output).expect("output should probe cleanly");
    assert!((result.duration_secs - 25.0).abs() < 1.0, "duration drifted to {}", result.duration_secs);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_short_clip_skips_prediction_and_is_encoded_directly() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("short");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");

    // Under the 20s threshold: sampling would cost as much as encoding.
    make_source(&tools, &input, 8, 1280, 720, 30);

    let limit = 20 * 1000 * 1000;
    let mut sampled = false;

    let outcome = compress(
        &tools,
        &request(&input, &output, &dir.join("work"), limit),
        &CancelToken::new(),
        |stage| {
            if matches!(stage, Stage::Predicting) {
                sampled = true;
            }
        },
    )
    .expect("compression should succeed");

    assert!(!sampled, "a short clip should not be sampled");
    assert!(outcome.within_limit);

    // 8 seconds of 720p fits inside 20 MB comfortably, so this should stay on
    // the quality-targeted path at full resolution rather than inflating to
    // fill the cap.
    assert!(
        matches!(outcome.plan.rate_control, RateControl::Crf { .. }),
        "an easy file should stay on CRF, got {:?}",
        outcome.plan.rate_control
    );
    assert_eq!((outcome.plan.scale.width, outcome.plan.scale.height), (1280, 720));
    assert!(
        outcome.output_bytes < limit / 2,
        "a CRF encode of an easy file should not fill the budget"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tight_target_forces_two_pass_and_a_downscale() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("tight");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");

    make_source(&tools, &input, 24, 1920, 1080, 30);

    // 700 KB for 24 seconds of 1080p is roughly 230 kbps: nowhere near enough
    // to hold 1080p, so the ladder has to spend resolution.
    let limit = 700 * 1000;

    let outcome = compress(
        &tools,
        &request(&input, &output, &dir.join("work"), limit),
        &CancelToken::new(),
        |_| {},
    )
    .expect("compression should succeed");

    assert!(
        matches!(outcome.plan.rate_control, RateControl::TwoPass { .. }),
        "a tight target must use two-pass, got {:?}",
        outcome.plan.rate_control
    );
    assert!(
        outcome.plan.scale.height < 1080,
        "expected a downscale, stayed at {}p",
        outcome.plan.scale.height
    );
    assert!(
        outcome.output_bytes <= limit,
        "output was {} bytes against a {limit} byte limit",
        outcome.output_bytes
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_stops_the_encode_and_leaves_no_scratch_files() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("cancel");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");
    let work = dir.join("work");

    make_source(&tools, &input, 20, 1280, 720, 30);

    let cancel = CancelToken::new();
    let mut seen_progress = 0;

    let result = compress(&tools, &request(&input, &output, &work, 2_000_000), &cancel, |stage| {
        if matches!(stage, Stage::Encoding(_)) {
            seen_progress += 1;
            // Let it get going, then pull the plug.
            if seen_progress >= 2 {
                cancel.cancel();
            }
        }
    });

    let error = result.expect_err("a cancelled job must not report success");
    assert!(error.is_cancellation(), "expected cancellation, got {error}");
    assert!(!work.exists(), "the work directory should be cleaned up on cancel");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_image_is_quality_searched_down_to_the_target() {
    use media_compressor_lib::images::{compress_image, ImageRequest};

    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("image");
    let input = dir.join("source.png");
    let output = dir.join("out.webp");

    // A detailed synthetic image: a gradient alone would compress to almost
    // nothing and the search would never have to work.
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=1920x1080")
        .args(["-frames:v", "1", "-c:v", "png"])
        .arg(&input)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success());

    let source_bytes = std::fs::metadata(&input).unwrap().len();
    let target = 120 * 1000;
    assert!(source_bytes > target, "the source must be too big for the test to mean anything");

    let mut steps = 0;
    let outcome = compress_image(
        &tools,
        &ImageRequest {
            input: input.clone(),
            output: output.clone(),
            target_bytes: target,
            format: ImageFormat::Webp,
            max_dimension: None,
        },
        &CancelToken::new(),
        |_| steps += 1,
    )
    .expect("image compression should succeed");

    assert!(outcome.within_limit, "landed at {} against {target}", outcome.output_bytes);
    assert!(outcome.output_bytes <= target);
    assert!(steps > 1, "a binary search should take more than one probe");

    // The point of searching rather than guessing: it should land near the
    // target, not far below it.
    assert!(
        outcome.output_bytes > target / 2,
        "used only {} of a {target} byte budget",
        outcome.output_bytes
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A cancelled job used to leave a multi-megabyte, unplayable `.mp4` sitting
/// next to the original under exactly the name a finished result would have
/// had. Whatever we started and did not finish has to go with us.
#[test]
fn cancelling_leaves_no_half_written_output_behind() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("partial");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");

    make_source(&tools, &input, 20, 1280, 720, 30);

    let cancel = CancelToken::new();
    let mut seen_progress = 0;

    let result = compress_media(
        &tools,
        &request(&input, &output, &dir.join("work"), 20_000_000),
        &cancel,
        |stage| {
            if matches!(stage, Stage::Encoding(_)) {
                seen_progress += 1;
                // Far enough in that ffmpeg has definitely written something.
                if seen_progress >= 3 {
                    cancel.cancel();
                }
            }
        },
    );

    assert!(result.is_err(), "a cancelled job must not report success");
    assert!(
        !output.exists(),
        "a partial output was left behind ({:?} bytes)",
        std::fs::metadata(&output).map(|meta| meta.len())
    );
    assert!(input.is_file(), "the source must be untouched");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The cleanup above must only ever remove a file this run created. A path that
/// was already occupied belongs to someone else, however the job ends.
#[test]
fn a_pre_existing_file_at_the_output_path_is_never_deleted() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("preexisting");
    let input = dir.join("source.mp4");
    let output = dir.join("out.mp4");

    make_source(&tools, &input, 5, 320, 240, 15);
    std::fs::write(&output, b"someone else's file").unwrap();

    // Cancelled before it can start, so the job fails with the file in place.
    let cancel = CancelToken::new();
    cancel.cancel();

    let result = compress_media(
        &tools,
        &request(&input, &output, &dir.join("work"), 20_000_000),
        &cancel,
        |_| {},
    );

    assert!(result.is_err(), "a cancelled job must not report success");
    assert!(output.is_file(), "a file we did not create must survive");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The assumption the correction speed-up rests on: that x264 will accept a
/// statistics file written by an *earlier* job's first pass and produce a
/// correct, rate-controlled file from it.
///
/// Unit tests can only show that `passes()` returns one pass. Whether FFmpeg
/// then honours the log — rather than erroring on it, or quietly ignoring it
/// and free-running the bitrate — is a question only the real encoder answers.
#[test]
fn a_second_pass_can_spend_statistics_an_earlier_attempt_wrote() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = scratch("passlog-reuse");
    let input = dir.join("source.mp4");
    make_source(&tools, &input, 12, 1280, 720, 30);
    let info = probe(&tools, &input).expect("source should probe");

    let passlog_prefix = dir.join("pass");
    let scale = Scale { width: 640, height: 360, fps: 30.0, bpp: 0.06, below_floor: false };

    let job = |video_bps: u64, output: &Path, reusable_stats| EncodeJob {
        input: input.clone(),
        output: output.to_path_buf(),
        plan: EncodePlan {
            codec: VideoCodec::H264,
            scale,
            rate_control: RateControl::TwoPass { video_bps },
            audio_bps: 96_000,
            attempt: 1,
        },
        source: info.clone(),
        speed: Speed::Fast,
        passlog_prefix: passlog_prefix.clone(),
        reusable_stats,
    };

    // The first attempt analyses the picture and leaves the log behind.
    let first_out = dir.join("first.mp4");
    let first = job(1_200_000, &first_out, None);
    assert_eq!(first.passes(), vec![Pass::First, Pass::Second]);
    let first_bytes = encode::run(&tools, &first, &CancelToken::new(), |_| {})
        .expect("the first attempt should encode");

    // The correction: same picture, fewer bits. It must not re-analyse.
    let second_out = dir.join("second.mp4");
    let second = job(400_000, &second_out, Some(first.pass_log()));
    assert_eq!(second.passes(), vec![Pass::Second], "the analysis must be reused");
    let second_bytes = encode::run(&tools, &second, &CancelToken::new(), |_| {})
        .expect("a second pass alone should encode against the inherited log");

    // The log was genuinely spent, not ignored: a third of the video bitrate
    // has to show up as a substantially smaller file. Without rate control
    // taking effect these would be the same size.
    assert!(
        (second_bytes as f64) < first_bytes as f64 * 0.75,
        "reusing the log did not apply the new bitrate: {first_bytes} then {second_bytes}"
    );

    // And it is still a real video of the right length and shape.
    let result = probe(&tools, &second_out).expect("the result should probe cleanly");
    assert_eq!((result.width, result.height), (640, 360));
    assert!(
        (result.duration_secs - 12.0).abs() < 1.0,
        "duration drifted to {}",
        result.duration_secs
    );

    let _ = std::fs::remove_dir_all(&dir);
}
