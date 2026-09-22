//! An A/B stopwatch for the compression pipeline. Not part of the suite —
//! it is `#[ignore]`d so a plain `cargo test` never runs it.
//!
//! ```text
//! MC_BENCH_INPUT=/path/to/clip.mp4 MC_BENCH_TARGET=10000000 \
//!   cargo test --manifest-path src-tauri/Cargo.toml \
//!     --test bench_speed -- --ignored --nocapture
//! ```
//!
//! With no `MC_BENCH_INPUT` it builds a synthetic clip that opens on static
//! bars and ends on noise — the shape that makes a prediction overshoot, and
//! the case worth checking before trusting any change to sampling.
//!
//! Compare a branch against its base by stashing: the reported `attempts` is
//! usually the more informative number, since one extra attempt is a whole
//! re-encode and swamps anything sampling saves.

use media_compressor_lib::ffmpeg::encode::{CancelToken, Speed};
use media_compressor_lib::ffmpeg::tools::FfmpegTools;
use media_compressor_lib::images::ImageFormat;
use media_compressor_lib::pipeline::{compress, CompressRequest, Stage};
use media_compressor_lib::strategy::plan::Options;
use media_compressor_lib::strategy::Target;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

fn tools() -> Option<FfmpegTools> {
    let found = FfmpegTools::locate(&std::env::temp_dir().join("mc-bench-no-cache")).ok()?;
    found.verify().ok()?;
    Some(found)
}

/// Static bars for 30s then heavy noise for 15s. A uniform test pattern cannot
/// exercise the correction path at all — x264's rate control simply hits its
/// mark — so the interesting source is one whose difficulty changes partway.
fn synthetic(tools: &FfmpegTools, dir: &PathBuf) -> PathBuf {
    let path = dir.join("bench-source.mp4");
    if path.is_file() {
        return path;
    }

    let run = |args: Vec<String>| {
        let ok = Command::new(&tools.ffmpeg).args(&args).status().expect("ffmpeg should run");
        assert!(ok.success(), "failed building the bench source");
    };
    let s = |v: &str| v.to_string();

    let calm = dir.join("calm.mp4");
    let busy = dir.join("busy.mp4");
    let common = || {
        vec![s("-c:v"), s("libx264"), s("-preset"), s("veryfast"), s("-crf"), s("18"),
             s("-pix_fmt"), s("yuv420p")]
    };

    let mut a = vec![s("-hide_banner"), s("-v"), s("error"), s("-y"), s("-f"), s("lavfi"),
                     s("-i"), s("smptebars=size=1280x720:rate=30"), s("-t"), s("30")];
    a.extend(common());
    a.push(calm.to_string_lossy().to_string());
    run(a);

    let mut b = vec![s("-hide_banner"), s("-v"), s("error"), s("-y"), s("-f"), s("lavfi"),
                     s("-i"), s("nullsrc=size=1280x720:rate=30,geq=random(1)*255:128:128"),
                     s("-t"), s("15")];
    b.extend(common());
    b.push(busy.to_string_lossy().to_string());
    run(b);

    let list = dir.join("list.txt");
    std::fs::write(
        &list,
        format!("file '{}'\nfile '{}'\n", calm.to_string_lossy(), busy.to_string_lossy()),
    )
    .unwrap();

    let mut c = vec![s("-hide_banner"), s("-v"), s("error"), s("-y"), s("-f"), s("concat"),
                     s("-safe"), s("0"), s("-i"), list.to_string_lossy().to_string(),
                     s("-f"), s("lavfi"), s("-i"), s("sine=frequency=440:sample_rate=48000"),
                     s("-shortest")];
    c.extend(common());
    c.extend([s("-c:a"), s("aac"), s("-b:a"), s("128k")]);
    c.push(path.to_string_lossy().to_string());
    run(c);

    path
}

#[test]
#[ignore = "benchmark; run explicitly with --ignored"]
fn bench() {
    let Some(tools) = tools() else {
        eprintln!("skipping: no ffmpeg available");
        return;
    };

    let dir = std::env::temp_dir().join("media-compressor-bench");
    std::fs::create_dir_all(&dir).unwrap();

    let input = match std::env::var("MC_BENCH_INPUT") {
        Ok(path) => PathBuf::from(path),
        Err(_) => synthetic(&tools, &dir),
    };
    assert!(input.is_file(), "no such input: {}", input.display());

    let target: u64 = std::env::var("MC_BENCH_TARGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8_000_000);

    let output = dir.join("bench-out.mp4");
    let _ = std::fs::remove_file(&output);

    // Prediction is the stage the sampling early-out acts on, so time it apart
    // from the encode rather than only reporting the total.
    let mut predict_started = None;
    let mut predict_secs = 0.0;

    let started = Instant::now();
    let outcome = compress(
        &tools,
        &CompressRequest {
            input: input.clone(),
            output: output.clone(),
            target: Target::new(target),
            options: Options::default(),
            speed: Speed::Fast,
            work_dir: dir.join("work"),
            image_format: ImageFormat::Webp,
            max_dimension: None,
        },
        &CancelToken::new(),
        |stage| match stage {
            Stage::Predicting => predict_started = Some(Instant::now()),
            Stage::Planned { .. } => {
                if let Some(at) = predict_started.take() {
                    predict_secs = at.elapsed().as_secs_f64();
                }
            }
            _ => {}
        },
    )
    .expect("compression should succeed");

    println!(
        "\nBENCH {}\n  target      {target} bytes\n  total       {:.1}s\n  predicting  {:.1}s\
         \n  attempts    {}\n  output      {} bytes (within limit: {})\n",
        input.display(),
        started.elapsed().as_secs_f64(),
        predict_secs,
        outcome.attempts,
        outcome.output_bytes,
        outcome.within_limit,
    );
}
