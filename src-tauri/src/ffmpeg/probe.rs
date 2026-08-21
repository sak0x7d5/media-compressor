//! Asking `ffprobe` what a file is.
//!
//! The JSON parsing is split out from the process call so the awkward cases —
//! rotated phone video, album art masquerading as a video stream, containers
//! with no duration — are testable without a binary on disk.

use super::tools::FfmpegTools;
use crate::strategy::{AudioInfo, MediaInfo};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("could not run ffprobe: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("ffprobe rejected the file: {0}")]
    Rejected(String),

    #[error("ffprobe returned output we could not read: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error("no video stream found")]
    NoVideoStream,

    #[error("no usable duration; the file may be truncated or still being written")]
    NoDuration,
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    format: Option<Format>,
    #[serde(default)]
    streams: Vec<Stream>,
}

#[derive(Debug, Deserialize)]
struct Format {
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Stream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    channels: Option<u8>,
    #[serde(default)]
    disposition: Option<Disposition>,
    #[serde(default)]
    side_data_list: Option<Vec<SideData>>,
    #[serde(default)]
    tags: Option<Tags>,
}

#[derive(Debug, Deserialize)]
struct Disposition {
    attached_pic: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct SideData {
    rotation: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Tags {
    rotate: Option<String>,
}

impl Stream {
    fn is_kind(&self, kind: &str) -> bool {
        self.codec_type.as_deref() == Some(kind)
    }

    /// Cover art in an audio file is reported as a video stream with a single
    /// frame. Treating it as the video would plan an encode for a JPEG.
    fn is_attached_picture(&self) -> bool {
        self.disposition.as_ref().and_then(|d| d.attached_pic) == Some(1)
    }

    /// Display rotation in degrees, from either the modern side-data field or
    /// the legacy `rotate` tag.
    fn rotation_degrees(&self) -> f64 {
        if let Some(list) = &self.side_data_list {
            if let Some(rotation) = list.iter().find_map(|entry| entry.rotation) {
                return rotation;
            }
        }
        self.tags
            .as_ref()
            .and_then(|tags| tags.rotate.as_deref())
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0)
    }
}

/// FFmpeg reports framerates as exact rationals: "30000/1001", not "29.97".
fn parse_rational(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator: f64 = numerator.trim().parse().ok()?;
    let denominator: f64 = denominator.trim().parse().ok()?;
    if denominator == 0.0 || numerator <= 0.0 {
        return None;
    }
    Some(numerator / denominator)
}

/// A framerate we are willing to plan against.
///
/// Some containers report nonsense here — 0/0 for a still image, or 90000/1
/// from a timebase mistaken for a framerate. Neither should reach the ladder,
/// where it would silently distort every bits-per-pixel calculation.
fn plausible_fps(value: f64) -> Option<f64> {
    (value > 0.0 && value <= 1000.0).then_some(value)
}

fn parse_seconds(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|seconds| *seconds > 0.0)
}

/// Turn `ffprobe -print_format json` output into a [`MediaInfo`].
pub fn parse_probe_json(json: &str) -> Result<MediaInfo, ProbeError> {
    let probed: ProbeOutput = serde_json::from_str(json)?;

    let video = probed
        .streams
        .iter()
        .find(|stream| stream.is_kind("video") && !stream.is_attached_picture())
        .ok_or(ProbeError::NoVideoStream)?;

    let (width, height) = match (video.width, video.height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => (width, height),
        _ => return Err(ProbeError::NoVideoStream),
    };

    // A phone shooting "portrait" stores 1920x1080 plus a 90 degree rotation.
    // Planning against the stored geometry would letterbox the result.
    let quarter_turn = {
        let degrees = video.rotation_degrees().abs() % 180.0;
        (degrees - 90.0).abs() < 1.0
    };
    let (width, height) = if quarter_turn { (height, width) } else { (width, height) };

    let fps = video
        .avg_frame_rate
        .as_deref()
        .and_then(parse_rational)
        .and_then(plausible_fps)
        .or_else(|| video.r_frame_rate.as_deref().and_then(parse_rational).and_then(plausible_fps))
        .unwrap_or(30.0);

    let duration_secs = probed
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(parse_seconds)
        .or_else(|| video.duration.as_deref().and_then(parse_seconds))
        .ok_or(ProbeError::NoDuration)?;

    let audio = probed
        .streams
        .iter()
        .find(|stream| stream.is_kind("audio"))
        .map(|stream| AudioInfo { channels: stream.channels.unwrap_or(2).max(1) });

    Ok(MediaInfo { duration_secs, width, height, fps, audio })
}

/// Run `ffprobe` against a file.
pub fn probe(tools: &FfmpegTools, input: &Path) -> Result<MediaInfo, ProbeError> {
    let mut command = Command::new(&tools.ffprobe);
    command
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(input);
    super::hide_console(&mut command);

    let output = command.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ProbeError::Rejected(stderr.trim().to_string()));
    }

    parse_probe_json(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANDSCAPE_1080P60: &str = r#"{
      "streams": [
        {"codec_type": "video", "width": 1920, "height": 1080,
         "avg_frame_rate": "60/1", "r_frame_rate": "60/1"},
        {"codec_type": "audio", "channels": 2}
      ],
      "format": {"duration": "183.400000"}
    }"#;

    #[test]
    fn reads_an_ordinary_landscape_clip() {
        let info = parse_probe_json(LANDSCAPE_1080P60).unwrap();

        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.fps, 60.0);
        assert_eq!(info.duration_secs, 183.4);
        assert_eq!(info.audio.unwrap().channels, 2);
    }

    #[test]
    fn ntsc_framerates_stay_exact() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 1280, "height": 720,
                       "avg_frame_rate": "30000/1001"}],
          "format": {"duration": "10.0"}
        }"#;

        let info = parse_probe_json(json).unwrap();
        assert!((info.fps - 29.97002997).abs() < 1e-6, "fps was {}", info.fps);
    }

    #[test]
    fn a_rotated_phone_clip_reports_its_display_geometry() {
        // Stored landscape, displayed portrait. Planning against 1920x1080 here
        // would produce a letterboxed mess.
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 1920, "height": 1080,
                       "avg_frame_rate": "30/1",
                       "side_data_list": [{"rotation": -90}]}],
          "format": {"duration": "12.0"}
        }"#;

        let info = parse_probe_json(json).unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
    }

    #[test]
    fn the_legacy_rotate_tag_is_honoured_too() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 1920, "height": 1080,
                       "avg_frame_rate": "30/1", "tags": {"rotate": "270"}}],
          "format": {"duration": "12.0"}
        }"#;

        let info = parse_probe_json(json).unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
    }

    #[test]
    fn a_180_degree_rotation_does_not_swap_dimensions() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 1920, "height": 1080,
                       "avg_frame_rate": "30/1", "side_data_list": [{"rotation": 180}]}],
          "format": {"duration": "12.0"}
        }"#;

        let info = parse_probe_json(json).unwrap();
        assert_eq!((info.width, info.height), (1920, 1080));
    }

    #[test]
    fn album_art_is_not_mistaken_for_a_video_stream() {
        let json = r#"{
          "streams": [
            {"codec_type": "video", "width": 600, "height": 600,
             "avg_frame_rate": "90000/1", "disposition": {"attached_pic": 1}},
            {"codec_type": "audio", "channels": 2}
          ],
          "format": {"duration": "240.0"}
        }"#;

        assert!(matches!(parse_probe_json(json), Err(ProbeError::NoVideoStream)));
    }

    #[test]
    fn a_nonsense_framerate_falls_back_rather_than_poisoning_the_plan() {
        // 0/0 is what a still image reports. 90000/1 is a timebase, not a rate.
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 1280, "height": 720,
                       "avg_frame_rate": "0/0", "r_frame_rate": "90000/1"}],
          "format": {"duration": "10.0"}
        }"#;

        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.fps, 30.0, "should fall back, not accept 90000 fps");
    }

    #[test]
    fn duration_falls_back_to_the_stream_when_the_container_omits_it() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 640, "height": 480,
                       "avg_frame_rate": "25/1", "duration": "8.5"}],
          "format": {}
        }"#;

        assert_eq!(parse_probe_json(json).unwrap().duration_secs, 8.5);
    }

    #[test]
    fn a_file_with_no_duration_anywhere_is_an_error() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 640, "height": 480,
                       "avg_frame_rate": "25/1"}],
          "format": {}
        }"#;

        assert!(matches!(parse_probe_json(json), Err(ProbeError::NoDuration)));
    }

    #[test]
    fn a_silent_clip_reports_no_audio() {
        let json = r#"{
          "streams": [{"codec_type": "video", "width": 640, "height": 480,
                       "avg_frame_rate": "25/1"}],
          "format": {"duration": "5.0"}
        }"#;

        assert!(parse_probe_json(json).unwrap().audio.is_none());
    }

    #[test]
    fn garbage_output_is_an_error_not_a_panic() {
        assert!(matches!(parse_probe_json("not json at all"), Err(ProbeError::Malformed(_))));
        assert!(matches!(parse_probe_json("{}"), Err(ProbeError::NoVideoStream)));
    }
}
