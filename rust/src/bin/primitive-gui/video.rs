//! Video in and out through ffprobe and ffmpeg.

use serde_json::Value;
use std::path::Path;
use primitive_gpu::load::tool;
use std::process::{Child, ChildStdin, Stdio};

/// What ffprobe says about a video, with the size after ffmpeg turns a
/// sideways phone video upright.
#[derive(Clone, Debug)]
pub struct VideoInfo {
    /// Frame rate as ffmpeg writes it ("30000/1001").
    pub fps_text: String,
    pub fps: f64,
    pub duration: f64,
    pub has_audio: bool,
    pub width: u32,
    pub height: u32,
}

fn rational(s: &str) -> Option<f64> {
    let (a, b) = s.split_once('/').unwrap_or((s, "1"));
    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b > 0.0 && a > 0.0).then_some(a / b)
}

/// Returns `Ok(None)` when the file is a still picture (or ffprobe cannot
/// read it), and the video facts when it has at least two frames.
pub fn probe(path: &Path) -> Result<Option<VideoInfo>, String> {
    let out = tool("ffprobe")
        .args(["-v", "error", "-show_streams", "-show_format", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run ffprobe: {e}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe output: {e}"))?;
    let format = v["format"]["format_name"].as_str().unwrap_or("");
    if format == "image2" || format.ends_with("_pipe") {
        return Ok(None);
    }
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let Some(vs) = streams.iter().find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1) else {
        return Ok(None);
    };
    let (mut width, mut height) = (vs["width"].as_u64().unwrap_or(0) as u32, vs["height"].as_u64().unwrap_or(0) as u32);
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let mut rotation = vs["tags"]["rotate"].as_str().and_then(|r| r.parse::<i64>().ok()).unwrap_or(0);
    if let Some(list) = vs["side_data_list"].as_array() {
        for sd in list {
            if let Some(r) = sd["rotation"].as_i64() {
                rotation = r;
            }
        }
    }
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut width, &mut height);
    }
    let text = |k: &str| vs[k].as_str().unwrap_or("").to_string();
    let (fps_text, fps) = [text("avg_frame_rate"), text("r_frame_rate")]
        .into_iter()
        .find_map(|t| rational(&t).filter(|f| (0.5..=240.0).contains(f)).map(|f| (t, f)))
        .unwrap_or(("25".into(), 25.0));
    let (fps_text, fps) = if fps > 60.0 { ("30".to_string(), 30.0) } else { (fps_text, fps) };
    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| vs["duration"].as_str().and_then(|d| d.parse().ok()))
        .unwrap_or(0.0);
    if duration * fps < 1.5 {
        return Ok(None);
    }
    Ok(Some(VideoInfo { fps_text, fps, duration, has_audio, width, height }))
}

/// Working size: the picture shrunk so its long side is at most `size`.
pub fn fit(width: u32, height: u32, size: u32) -> (u32, u32) {
    let long = width.max(height);
    if size == 0 || long <= size {
        return (width, height);
    }
    let s = size as f64 / long as f64;
    (((width as f64 * s).round() as u32).max(1), ((height as f64 * s).round() as u32).max(1))
}

/// Starts decoding every frame at a fixed frame rate and the working size,
/// as raw RGBA into `out`.
pub fn start_decode(input: &Path, info: &VideoInfo, w: u32, h: u32, out: &Path) -> Result<Child, String> {
    tool("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-an", "-vf", &format!("fps={},scale={w}:{h}:flags=area", info.fps_text)])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))
}

/// Starts a video encoder that reads raw RGBA frames of `w` x `h` (both
/// even) from its input and writes a video-only file to `out`. `container`
/// is "mp4" (H.264), "webm" (VP9) or "gif" (one palette made for the whole
/// clip).
pub fn start_encode(w: u32, h: u32, fps_text: &str, out: &Path, container: &str) -> Result<(Child, ChildStdin), String> {
    let codec: &[&str] = match container {
        "webm" => &["-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-row-mt", "1", "-deadline", "good", "-cpu-used", "4", "-pix_fmt", "yuv420p"],
        "gif" => &["-vf", "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a", "-loop", "0"],
        _ => &["-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart"],
    };
    let mut child = tool("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{w}x{h}"), "-framerate", fps_text, "-i", "-"])
        .args(codec)
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let stdin = child.stdin.take().ok_or("ffmpeg has no input pipe")?;
    Ok((child, stdin))
}

/// Waits for an encoder started by `start_encode` and reports its error.
pub fn finish_encode(child: Child) -> Result<(), String> {
    let out = child.wait_with_output().map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Puts the sound of `original` into the video-only file `video`, writing
/// `out`. Copies the sound unchanged, or encodes it (AAC for MP4, Opus for
/// WebM) when the file type cannot hold it as it is.
pub fn add_sound(video: &Path, original: &Path, out: &Path, container: &str) -> Result<(), String> {
    let mut last = String::new();
    let fallback = if container == "webm" { ["-c:a", "libopus", "-b:a", "160k"] } else { ["-c:a", "aac", "-b:a", "192k"] };
    for audio in [["-c:a", "copy", "", ""], fallback] {
        let st = tool("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
            .arg(video)
            .arg("-i")
            .arg(original)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy"])
            .args(audio.iter().filter(|a| !a.is_empty()))
            .args(["-shortest"])
            .args(if container == "mp4" { &["-movflags", "+faststart"][..] } else { &[][..] })
            .arg(out)
            .output()
            .map_err(|e| format!("could not run ffmpeg: {e}"))?;
        if st.status.success() {
            return Ok(());
        }
        last = String::from_utf8_lossy(&st.stderr).trim().to_string();
    }
    Err(format!("ffmpeg could not add the sound: {last}"))
}
