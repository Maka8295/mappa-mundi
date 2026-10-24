//! Video support through the system `ffmpeg`: poster frames for videos and
//! silent in-app playback of Live Photo clips.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context as _;
use image::RgbaImage;

pub fn ffmpeg_available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

pub fn thumbnail(path: &Path) -> anyhow::Result<RgbaImage> {
    anyhow::ensure!(ffmpeg_available(), "ffmpeg is not installed");
    // Try a little way in first (the first frame is often black), then the start.
    for seek in ["0.5", "0"] {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-ss", seek, "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale=640:640:force_original_aspect_ratio=decrease"])
            .args(["-f", "image2pipe", "-vcodec", "png", "-"])
            .output()?;
        if out.status.success() && !out.stdout.is_empty() {
            return Ok(image::load_from_memory(&out.stdout)?.to_rgba8());
        }
    }
    anyhow::bail!("ffmpeg could not extract a frame from {}", path.display())
}

/// Plays a video once (no audio), decoding frames in real time on a
/// background thread. Frames are letterboxed into exactly `size`.
pub struct Player {
    child: Child,
    frame: Arc<Mutex<Option<egui::ColorImage>>>,
    finished: Arc<AtomicBool>,
}

impl Player {
    pub fn start(path: &Path, size: [usize; 2], ctx: egui::Context) -> anyhow::Result<Self> {
        anyhow::ensure!(ffmpeg_available(), "ffmpeg is not installed");
        let [w, h] = size;
        let filter = format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,format=rgba,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0"
        );
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-re", "-i"])
            .arg(path)
            .args(["-an", "-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting ffmpeg")?;
        let mut stdout = child.stdout.take().unwrap();
        let frame = Arc::new(Mutex::new(None));
        let finished = Arc::new(AtomicBool::new(false));
        let (frame2, finished2) = (frame.clone(), finished.clone());
        std::thread::Builder::new().name("live-player".into()).spawn(move || {
            let mut buf = vec![0u8; w * h * 4];
            while stdout.read_exact(&mut buf).is_ok() {
                *frame2.lock().unwrap() = Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &buf));
                ctx.request_repaint();
            }
            finished2.store(true, Ordering::Release);
            ctx.request_repaint();
        })?;
        Ok(Self { child, frame, finished })
    }

    /// The newest decoded frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<egui::ColorImage> {
        self.frame.lock().unwrap().take()
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opens a file with the desktop's default application.
pub fn open_external(path: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = Command::new(opener).arg(path).spawn() {
        eprintln!("mappa-mundi: could not run {opener}: {e}");
    }
}
