//! Shells out to system `ffmpeg` to stitch saved PNGs into a video on clean exit.
//!
//! `ffmpeg` must be on `PATH`; the Docker runtime image includes it.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use std::path::Path;
use std::process::Command;

pub fn encode_run(
    out_dir: &Path,
    run_id: i64,
    fps: u32,
    codec: &str,
    container: &str,
) -> MbResult<std::path::PathBuf> {
    if codec == "none" {
        return Err(MbError::Ffmpeg("video codec is 'none'".into()));
    }

    let out_path = out_dir.join(format!("run_{}.{}", run_id, container));

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-hide_banner").arg("-loglevel").arg("warning").arg("-stats");
    cmd.arg("-framerate").arg(fps.to_string());
    // Input frames. On Linux/Docker use ffmpeg's glob demuxer so a decimated
    // set (--save-every > 1) or any gaps still encode in sorted order. Windows
    // ffmpeg builds (gyan.dev) lack glob support, so fall back to the sprintf
    // pattern there (needs contiguous frame_000001.. — i.e. save_every = 1).
    if cfg!(target_os = "windows") {
        cmd.arg("-i").arg(out_dir.join("frame_%06d.png"));
    } else {
        cmd.arg("-pattern_type").arg("glob")
            .arg("-i").arg(out_dir.join("frame_*.png"));
    }

    cmd.args(codec_args(codec)?);
    cmd.arg("-pix_fmt").arg(if codec == "prores" { "yuv444p10le" } else { "yuv420p" }).arg(&out_path);

    tracing::info!("invoking ffmpeg: {:?}", cmd);
    let status = cmd
        .status()
        .map_err(|e| MbError::Ffmpeg(format!("spawn ffmpeg: {e}")))?;
    if !status.success() {
        return Err(MbError::Ffmpeg(format!("ffmpeg exited with {status}")));
    }
    Ok(out_path)
}

/// ffmpeg arguments for one of the supported codecs.
pub fn codec_args(codec: &str) -> MbResult<Vec<String>> {
    // Low-RAM, ~4-thread encoder settings. The default x264/x265 "slow" preset
    // keeps ~50 frames of lookahead resident — ~1.25 GB at 4K, which blows a
    // 2 GB memory cap. "medium" + a short rc-lookahead holds it to a few hundred
    // MB at negligible quality cost (crf unchanged), so the encoder step fits
    // the same memory budget as the renderer.
    let args: Vec<String> = match codec {
        "h264" => vec![
            "-c:v".into(), "libx264".into(),
            "-crf".into(), "18".into(),
            "-preset".into(), "medium".into(),
            "-threads".into(), "4".into(),
            "-x264-params".into(), "rc-lookahead=10:sync-lookahead=0".into(),
        ],
        "h265" => vec![
            "-c:v".into(), "libx265".into(),
            "-crf".into(), "22".into(),
            "-preset".into(), "medium".into(),
            "-x265-params".into(), "frame-threads=2:rc-lookahead=10:pools=4".into(),
        ],
        "vp9" => vec![
            "-c:v".into(), "libvpx-vp9".into(),
            "-crf".into(), "30".into(),
            "-b:v".into(), "0".into(),
            "-threads".into(), "4".into(),
        ],
        // ProRes 4444: the profile that keeps an alpha channel.
        "prores" => vec![
            "-c:v".into(), "prores_ks".into(),
            "-profile:v".into(), "4444".into(),
        ],
        other => return Err(MbError::Ffmpeg(format!("unknown codec {other}"))),
    };
    Ok(args)
}

/// Where a video's picture lives: beside it, same name, `.jpg` (`zoom-2.mp4` →
/// `zoom-2.jpg`). The app shows it next to the video so recolours compare at
/// a glance.
pub fn picture_path(video: &Path) -> std::path::PathBuf {
    video.with_extension("jpg")
}

/// Where a video's timings live: beside it, same name, `.json`.
pub fn timing_path(video: &Path) -> std::path::PathBuf {
    video.with_extension("json")
}

/// At most 640 pixels on the long side, keeping the shape.
pub fn picture_size(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h).max(1);
    if long <= 640 {
        return (w, h);
    }
    ((u64::from(w) * 640 / u64::from(long)).max(1) as u32, (u64::from(h) * 640 / u64::from(long)).max(1) as u32)
}

/// The middle frame of an existing video, taken out of the file with ffmpeg,
/// as its picture: for videos made before pictures were written alongside.
pub fn picture_from_video(video: &Path) -> MbResult<std::path::PathBuf> {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "format=duration:stream=width,height", "-of", "default=nw=1"])
        .arg(video)
        .output()
        .map_err(|e| MbError::Ffmpeg(format!("running ffprobe: {e}")))?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).and_then(|v| v.trim().parse::<f64>().ok());
    let (Some(duration), Some(w), Some(h)) = (field("duration"), field("width"), field("height")) else {
        return Err(MbError::Ffmpeg(format!("ffprobe could not read {}", video.display())));
    };
    let (pw, ph) = picture_size(w as u32, h as u32);
    let out = picture_path(video);
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", duration / 2.0), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", &format!("scale={pw}:{ph}"), "-q:v", "3"])
        .arg(&out)
        .status()
        .map_err(|e| MbError::Ffmpeg(format!("running ffmpeg: {e}")))?;
    if !status.success() || !out.exists() {
        return Err(MbError::Ffmpeg(format!("ffmpeg could not take a picture of {}", video.display())));
    }
    Ok(out)
}
