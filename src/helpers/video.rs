//! Video and animation probing through FFmpeg.
//!
//! Decoding video needs a codec stack that libvips does not provide, so the
//! server shells out to `ffprobe` for metadata and `ffmpeg` for one frame.
//! Both binaries are configured in [`AppConfig`](crate::config::AppConfig), so
//! an installation can point at a bundled copy instead of relying on `PATH`.

use std::path::Path;
use std::process::{Command, Stdio};

/// Metadata read from a video or animated image stream.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoMeta {
    /// Coded frame width, after applying container rotation.
    pub width: i64,
    /// Coded frame height, after applying container rotation.
    pub height: i64,
    /// Seconds. `None` when the container does not report a usable duration.
    pub duration_sec: Option<f64>,
    /// Frames per second. `None` for a single-frame stream.
    pub framerate: Option<f64>,
    /// Total frames. Estimated from duration and framerate when the container
    /// does not store an exact count.
    pub frame_count: Option<i64>,
}

impl VideoMeta {
    /// Smallest shape that still describes something renderable.
    pub fn has_dimensions(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// Reads width, height, duration, framerate and frame count from a video or
/// animation. A missing or unusable `ffprobe` is not an error: the file is
/// simply indexed without video metadata.
pub fn probe(ffprobe: &Path, source: &Path) -> Option<VideoMeta> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(source)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_probe(&output.stdout)
}

/// Parses the JSON emitted by `ffprobe -show_format -show_streams`.
///
/// Split from [`probe`] so the decoding rules are testable without a binary.
pub fn parse_probe(json: &[u8]) -> Option<VideoMeta> {
    let value: serde_json::Value = serde_json::from_slice(json).ok()?;
    let streams = value.get("streams")?.as_array()?;
    let stream = streams
        .iter()
        .find(|stream| {
            stream.get("codec_type").and_then(|kind| kind.as_str()) == Some("video")
                && !is_attached_picture(stream)
        })
        .or_else(|| {
            streams.iter().find(|stream| {
                stream.get("codec_type").and_then(|kind| kind.as_str()) == Some("video")
            })
        })?;

    let mut width = integer(stream.get("width")).unwrap_or(0);
    let mut height = integer(stream.get("height")).unwrap_or(0);

    // A phone video is stored as landscape frames plus a rotation flag. Preview
    // and transcode both display it upright, so metadata must match.
    if matches!(
        rotation(stream).map(|value| value.rem_euclid(360.0) as i64),
        Some(90 | 270)
    ) {
        std::mem::swap(&mut width, &mut height);
    }

    let duration_sec = number(
        value
            .get("format")
            .and_then(|format| format.get("duration")),
    )
    .or_else(|| number(stream.get("duration")))
    .filter(|duration| *duration > 0.0);

    let framerate = number(stream.get("avg_frame_rate"))
        .or_else(|| number(stream.get("r_frame_rate")))
        .filter(|rate| *rate > 0.0);

    let frame_count = integer(stream.get("nb_frames"))
        .filter(|count| *count > 0)
        .or_else(|| {
            duration_sec
                .zip(framerate)
                .map(|(duration, rate)| (duration * rate).round() as i64)
                .filter(|count| *count > 0)
        });

    Some(VideoMeta {
        width,
        height,
        duration_sec,
        framerate,
        frame_count,
    })
}

/// Extracts one representative frame and returns it as lossless PNG.
///
/// The `thumbnail` filter picks the most representative frame of a short
/// window; when that is unavailable the frame at the fallback timestamp (the
/// second second by default) is used. An attached container picture is the
/// last resort. Runs on a blocking thread: it starts an external process.
pub fn extract_frame(
    ffmpeg: &Path,
    source: &Path,
    duration_sec: Option<f64>,
) -> Result<Vec<u8>, String> {
    let seek = frame_timestamp(duration_sec);
    let mut errors = Vec::new();

    // Significant frame: let FFmpeg score a short run of frames and keep the
    // one that best represents the scene.
    match run_png(ffmpeg, source, seek, Some("thumbnail=100")) {
        Ok(bytes) => return Ok(bytes),
        Err(err) => errors.push(err),
    }

    // Plain frame at the fallback timestamp.
    match run_png(ffmpeg, source, seek, None) {
        Ok(bytes) => return Ok(bytes),
        Err(err) => errors.push(err),
    }

    // Some very short or badly seekable files have no frame at the timestamp.
    if seek > 0.0 {
        match run_png(ffmpeg, source, 0.0, None) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => errors.push(err),
        }
    }

    Err(format!(
        "cannot extract a frame from `{}`: {}",
        source.display(),
        errors.join("; ")
    ))
}

/// Timestamp used when no significant frame can be selected.
///
/// The second second is the product default; very short clips fall back to the
/// middle so that at least one frame exists.
pub fn frame_timestamp(duration_sec: Option<f64>) -> f64 {
    match duration_sec {
        Some(duration) if duration > 2.0 => 2.0,
        Some(duration) if duration > 0.0 => duration / 2.0,
        _ => 0.0,
    }
}

fn run_png(
    ffmpeg: &Path,
    source: &Path,
    seek: f64,
    filter: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-ss")
        .arg(format!("{seek:.3}"))
        .arg("-i")
        .arg(source)
        .args(["-map", "0:V:0"]);

    if let Some(filter) = filter {
        command.args(["-vf", filter]);
    }

    command.args([
        "-frames:v",
        "1",
        "-an",
        "-sn",
        "-dn",
        "-f",
        "image2pipe",
        "-c:v",
        "png",
        "pipe:1",
    ]);

    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("cannot run `{}`: {err}", ffmpeg.display()))?;

    if !output.status.success() {
        return Err(stderr_message(&output.stderr));
    }
    if output.stdout.is_empty() {
        return Err("ffmpeg produced no frame".to_string());
    }

    Ok(output.stdout)
}

/// Length of a streaming preview, in seconds.
pub const TRANSCODE_SECONDS: u32 = 10;
/// Output frame rate of a streaming preview.
pub const TRANSCODE_FPS: u32 = 30;

/// Starts an FFmpeg process that writes a fragmented MP4 of the first
/// [`TRANSCODE_SECONDS`] seconds to stdout.
///
/// The output is capped at the requested width or height without upscaling,
/// has no audio and runs at a constant [`TRANSCODE_FPS`], mirroring what the
/// thumbnail endpoint produces for still images. Fragmented MP4 lets the
/// player start before the whole clip is encoded.
pub fn spawn_transcode(
    ffmpeg: &Path,
    source: &Path,
    direction: char,
    size: u32,
) -> Result<tokio::process::Child, String> {
    // `-2` keeps the other axis proportional and even; the rounded expression
    // keeps the requested axis even too, which H.264 4:2:0 requires.
    let scale = match direction {
        'w' => format!("scale='trunc(min(iw,{size})/2)*2':-2"),
        'h' => format!("scale=-2:'trunc(min(ih,{size})/2)*2'"),
        _ => return Err("transcode direction must be w or h".to_string()),
    };

    tokio::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-i")
        .arg(source)
        .args([
            "-map",
            "0:V:0",
            "-vf",
            &scale,
            "-t",
            &TRANSCODE_SECONDS.to_string(),
            "-r",
            &TRANSCODE_FPS.to_string(),
            "-an",
            "-sn",
            "-dn",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "-f",
            "mp4",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A client that disconnects early must not leave FFmpeg running.
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("cannot run `{}`: {err}", ffmpeg.display()))
}

fn stderr_message(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        "ffmpeg failed without a message".to_string()
    } else {
        text.to_string()
    }
}

fn is_attached_picture(stream: &serde_json::Value) -> bool {
    stream
        .get("disposition")
        .and_then(|disposition| disposition.get("attached_pic"))
        .and_then(serde_json::Value::as_i64)
        == Some(1)
}

/// Container rotation in degrees, if the stream declares one.
fn rotation(stream: &serde_json::Value) -> Option<f64> {
    stream
        .get("side_data_list")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find_map(|entry| number(entry.get("rotation")))
        })
        .or_else(|| {
            stream
                .get("tags")
                .and_then(|tags| tags.get("rotate"))
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse().ok())
        })
}

fn integer(value: Option<&serde_json::Value>) -> Option<i64> {
    match value? {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// FFprobe reports rates as `"30000/1001"` strings, durations as decimal
/// strings.
fn number(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text
            .split_once('/')
            .map(|(numerator, denominator)| {
                let numerator: f64 = numerator.parse().ok()?;
                let denominator: f64 = denominator.parse().ok()?;
                (denominator != 0.0 && numerator > 0.0).then(|| numerator / denominator)
            })
            .unwrap_or_else(|| text.parse().ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rates_durations_and_rotates_portrait_video() {
        let json = br#"{
            "streams": [{
                "codec_type": "video",
                "width": 1920,
                "height": 1080,
                "avg_frame_rate": "30000/1001",
                "nb_frames": "601",
                "duration": "20.02",
                "side_data_list": [{ "rotation": -90 }]
            }],
            "format": { "duration": "20.020000" }
        }"#;

        let meta = parse_probe(json).unwrap();
        assert_eq!(meta.width, 1080);
        assert_eq!(meta.height, 1920);
        assert_eq!(meta.duration_sec, Some(20.02));
        assert_eq!(meta.frame_count, Some(601));
        assert!((meta.framerate.unwrap() - 29.97).abs() < 0.01);
    }

    #[test]
    fn estimates_missing_frame_count() {
        let json = br#"{
            "streams": [{
                "codec_type": "video",
                "width": 640,
                "height": 480,
                "r_frame_rate": "25/1",
                "duration": "3.0"
            }]
        }"#;

        let meta = parse_probe(json).unwrap();
        assert_eq!(meta.frame_count, Some(75));
    }

    #[test]
    fn prefers_real_video_over_attached_cover() {
        let json = br#"{
            "streams": [
                {
                    "codec_type": "video",
                    "width": 600,
                    "height": 600,
                    "disposition": { "attached_pic": 1 }
                },
                {
                    "codec_type": "video",
                    "width": 1280,
                    "height": 720,
                    "avg_frame_rate": "24/1",
                    "nb_frames": "48",
                    "duration": "2"
                }
            ],
            "format": { "duration": "2" }
        }"#;

        let meta = parse_probe(json).unwrap();
        assert_eq!((meta.width, meta.height), (1280, 720));
        assert_eq!(meta.frame_count, Some(48));
    }

    #[test]
    fn falls_back_to_attached_preview_when_there_is_no_video_stream() {
        let json = br#"{
            "streams": [{
                "codec_type": "video",
                "width": 320,
                "height": 320,
                "disposition": { "attached_pic": 1 }
            }]
        }"#;

        let meta = parse_probe(json).unwrap();
        assert_eq!((meta.width, meta.height), (320, 320));
    }

    #[test]
    fn frame_timestamp_uses_second_second_then_midpoint() {
        assert_eq!(frame_timestamp(Some(10.0)), 2.0);
        assert_eq!(frame_timestamp(Some(1.0)), 0.5);
        assert_eq!(frame_timestamp(None), 0.0);
    }
}
