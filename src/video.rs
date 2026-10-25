//! Video support through the system `ffmpeg`: poster frames for videos, and
//! in-app playback of videos and Live Photo clips with their sound.

use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, SendTimeoutError, TryRecvError};
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

static MUTED: AtomicBool = AtomicBool::new(false);

/// Whether playback is silenced, for every player until it's turned back on.
pub fn muted() -> bool {
    MUTED.load(Ordering::Relaxed)
}

pub fn set_muted(muted: bool) {
    MUTED.store(muted, Ordering::Relaxed);
}

/// Plays a video once with its sound. ffmpeg decodes the picture and the
/// sound in two processes; each frame is shown when the sound reaches it,
/// or on the wall clock when there's no sound. Frames are letterboxed into
/// exactly `size`.
pub struct Player {
    shared: Arc<Shared>,
}

struct Shared {
    frame: Mutex<Option<egui::ColorImage>>,
    finished: AtomicBool,
    stop: AtomicBool,
    /// The ffmpeg processes, killed when the player is dropped.
    children: Mutex<Vec<Child>>,
    clock: Clock,
}

impl Player {
    pub fn start(path: &Path, size: [usize; 2], ctx: egui::Context) -> anyhow::Result<Self> {
        anyhow::ensure!(ffmpeg_available(), "ffmpeg is not installed");
        let shared = Arc::new(Shared {
            frame: Mutex::new(None),
            finished: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            children: Mutex::new(Vec::new()),
            clock: Clock::default(),
        });
        let (path, s) = (path.to_owned(), shared.clone());
        std::thread::Builder::new().name("player".into()).spawn(move || {
            play(&path, size, &s, &ctx);
            s.finished.store(true, Ordering::Release);
            ctx.request_repaint();
        })?;
        Ok(Self { shared })
    }

    /// The newest decoded frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<egui::ColorImage> {
        self.shared.frame.lock().unwrap().take()
    }

    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for child in self.shared.children.lock().unwrap().iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Shared {
    /// Starts `cmd` with its output piped, unless the player has stopped.
    fn spawn(&self, cmd: &mut Command, stderr: bool) -> Option<(ChildStdout, Option<ChildStderr>)> {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped());
        cmd.stderr(if stderr { Stdio::piped() } else { Stdio::null() });
        let mut child = cmd.spawn().map_err(|e| eprintln!("mappa-mundi: could not start ffmpeg: {e}")).ok()?;
        let pipes = (child.stdout.take()?, child.stderr.take());
        // Checked under the lock, so a player dropped meanwhile can't miss it.
        let mut children = self.children.lock().unwrap();
        if self.stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        children.push(child);
        Some(pipes)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

fn play(path: &Path, [w, h]: [usize; 2], shared: &Arc<Shared>, ctx: &egui::Context) {
    let streams = probe(path);
    match streams.audio {
        Some(map) => {
            let (path, s) = (path.to_owned(), shared.clone());
            let started = std::thread::Builder::new().name("player-sound".into()).spawn(move || {
                if let Err(e) = sound(&path, &map, &s) {
                    eprintln!("mappa-mundi: playing without sound: {e:#}");
                    s.clock.no_sound();
                }
            });
            if started.is_err() {
                shared.clock.no_sound();
            }
        }
        None => shared.clock.no_sound(),
    }

    // Slow-motion clips are 120 or 240 fps; the screen doesn't need that many.
    let fast = streams.fps.is_some_and(|f| f > 61.0);
    let frame_len = 1.0 / streams.fps.unwrap_or(30.0).min(60.0);
    // showinfo logs each frame's time just before ffmpeg writes the frame.
    let filter = format!(
        "{}scale={w}:{h}:force_original_aspect_ratio=decrease,format=rgba,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0,showinfo=checksum=0",
        if fast { "fps=60," } else { "" }
    );
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "info", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", &streams.video, "-fps_mode", "passthrough"])
        .args(["-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    let Some((mut stdout, Some(stderr))) = shared.spawn(&mut cmd, true) else { return };
    let times = frame_times(stderr);

    let mut buf = vec![0u8; w * h * 4];
    let mut timed = true;
    let mut last: Option<f64> = None;
    while stdout.read_exact(&mut buf).is_ok() {
        // Frame times are exact (Live Photo clips have uneven frame rates);
        // if they ever stop coming, assume an even rate.
        let t = match times.recv_timeout(Duration::from_millis(if timed { 500 } else { 0 })) {
            Ok(t) => t,
            Err(_) => {
                timed = false;
                last.map_or(0.0, |l| l + frame_len)
            }
        };
        let first = last.is_none();
        last = Some(t);
        if !first {
            if !wait_until(shared, t) {
                return;
            }
            // Behind (the decoder couldn't keep up): skip to catch up.
            if shared.clock.now().is_some_and(|p| p > t + frame_len) {
                continue;
            }
        }
        *shared.frame.lock().unwrap() = Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &buf));
        ctx.request_repaint();
        if first {
            shared.clock.video_ready();
        }
    }
    // Leave the last frame up for as long as it lasts.
    if let Some(t) = last {
        wait_until(shared, t + frame_len);
    }
}

/// Waits until the clock reaches `t`; false if the player stopped.
fn wait_until(shared: &Shared, t: f64) -> bool {
    let mut give_up = None;
    loop {
        if shared.stopped() {
            return false;
        }
        let now = shared.clock.now();
        if now.is_some_and(|p| p >= t) {
            return true;
        }
        // If the sound doesn't start, or stalls, carry on without it rather
        // than freeze the picture.
        let ahead = (t - now.unwrap_or(0.0)).max(0.0);
        let give_up = *give_up.get_or_insert_with(|| Instant::now() + Duration::from_secs_f64(ahead + 3.0));
        if Instant::now() > give_up {
            shared.clock.no_sound();
        }
        std::thread::sleep(Duration::from_secs_f64(ahead.clamp(0.001, 0.01)));
    }
}

/// The time of each frame ffmpeg outputs, read from showinfo's log lines.
fn frame_times(stderr: ChildStderr) -> Receiver<f64> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        // Read to the end even once nobody's listening, so ffmpeg never blocks on a full pipe.
        for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&line);
            let time = line.split_once(" pts_time:").and_then(|(_, rest)| rest.split_whitespace().next()?.parse().ok());
            if let Some(t) = time {
                let _ = tx.send(t);
            }
        }
    });
    rx
}

/// Plays the sound of `path` (stream `map`) on the default output device.
/// What the device has played drives the clock.
fn sound(path: &Path, map: &str, shared: &Arc<Shared>) -> anyhow::Result<()> {
    let device = cpal::default_host().default_output_device().context("no sound output device")?;
    let supported = device.default_output_config()?;
    let mut config = supported.config();
    // Short buffers (about 20 ms) keep the clock fine-grained.
    if let cpal::SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
        config.buffer_size = cpal::BufferSize::Fixed(1024.clamp(min, max));
    }
    // ~0.2 s decoded ahead; ffmpeg waits while it's full.
    let (tx, rx) = crossbeam_channel::bounded(8);
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => output::<f32>(&device, config, rx, shared),
        cpal::SampleFormat::I16 => output::<i16>(&device, config, rx, shared),
        cpal::SampleFormat::I32 => output::<i32>(&device, config, rx, shared),
        cpal::SampleFormat::U16 => output::<u16>(&device, config, rx, shared),
        cpal::SampleFormat::U8 => output::<u8>(&device, config, rx, shared),
        f => anyhow::bail!("the output device wants {f} samples"),
    }?;
    stream.play()?;

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", map, "-af", "aresample=async=1:first_pts=0"])
        .args(["-ac", &config.channels.to_string(), "-ar", &config.sample_rate.to_string(), "-f", "f32le", "-"]);
    let Some((mut stdout, _)) = shared.spawn(&mut cmd, false) else { return Ok(()) };
    let mut bytes = vec![0u8; 1024 * config.channels as usize * 4];
    loop {
        let n = read_full(&mut stdout, &mut bytes);
        let mut chunk: Vec<f32> = bytes[..n].as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect();
        if chunk.is_empty() {
            break;
        }
        loop {
            match tx.send_timeout(chunk, Duration::from_millis(50)) {
                Ok(()) => break,
                Err(SendTimeoutError::Timeout(c)) if !shared.stopped() => chunk = c,
                Err(_) => return Ok(()),
            }
        }
        if n < bytes.len() {
            break;
        }
    }
    // Hang up, so the device knows the sound has ended once it's played
    // what's queued, and keep it open until the player stops.
    drop(tx);
    while !shared.stopped() {
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    rx: Receiver<Vec<f32>>,
    shared: &Arc<Shared>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let (rate, channels) = (config.sample_rate as f64, config.channels as usize);
    let (mut chunk, mut used, mut played) = (Vec::new(), 0, 0u64);
    let s = shared.clone();
    let data = move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
        let stamp = info.timestamp();
        // What's written now is heard after what's already queued.
        let heard = Instant::now() + stamp.playback.saturating_duration_since(stamp.callback);
        let mut n = 0;
        // Wait for the picture's first frame, then keep up with it.
        if s.clock.wants_sound() {
            let gain = if muted() { 0.0 } else { 1.0 };
            while n < out.len() {
                if used == chunk.len() {
                    match rx.try_recv() {
                        Ok(c) => (chunk, used) = (c, 0),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            s.clock.no_sound();
                            break;
                        }
                    }
                    continue;
                }
                let k = (chunk.len() - used).min(out.len() - n);
                for (o, &v) in out[n..n + k].iter_mut().zip(&chunk[used..used + k]) {
                    *o = T::from_sample(v * gain);
                }
                (n, used) = (n + k, used + k);
            }
            if n > 0 {
                let start = played as f64 / rate;
                played += (n / channels) as u64;
                s.clock.heard(heard, start, played as f64 / rate);
            }
        }
        out[n..].fill(T::EQUILIBRIUM);
    };
    let s = shared.clone();
    let error = move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied => {}
        _ => {
            eprintln!("mappa-mundi: sound stopped: {e}");
            s.clock.no_sound();
        }
    };
    Ok(device.build_output_stream(config, data, error, None)?)
}

/// Reads until `buf` is full or the input ends; returns how much it read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    n
}

/// The playback position, in seconds into the video. While there's sound it
/// follows what has been heard; without sound (or once it ends) it runs on
/// the wall clock.
#[derive(Default)]
struct Clock(Mutex<ClockState>);

#[derive(Default)]
struct ClockState {
    /// The first frame is up, so the sound can start.
    ready: bool,
    /// No sound (none in the file, ended, or given up on): run freely.
    free: bool,
    /// The position heard, or shown, at an instant.
    anchor: Option<(Instant, f64)>,
    /// The end of the sound handed to the device so far; the clock waits
    /// there for more.
    limit: f64,
    /// The last position reported; the clock never runs backwards.
    last: f64,
}

impl ClockState {
    fn position(&self, now: Instant) -> Option<f64> {
        let (at, t) = self.anchor?;
        let since = match now.checked_duration_since(at) {
            Some(d) => d.as_secs_f64(),
            None => -(at - now).as_secs_f64(),
        };
        let p = t + since;
        Some(if self.free { p } else { p.min(self.limit) }.max(self.last))
    }
}

impl Clock {
    /// The position now, or None before playback has started.
    fn now(&self) -> Option<f64> {
        let mut s = self.0.lock().unwrap();
        let p = s.position(Instant::now())?;
        s.last = p;
        Some(p)
    }

    fn video_ready(&self) {
        let mut s = self.0.lock().unwrap();
        s.ready = true;
        if s.free && s.anchor.is_none() {
            s.anchor = Some((Instant::now(), 0.0));
        }
    }

    fn wants_sound(&self) -> bool {
        let s = self.0.lock().unwrap();
        s.ready && !s.free
    }

    /// Sound from `start` to `end` will be heard from `at`.
    fn heard(&self, at: Instant, start: f64, end: f64) {
        let mut s = self.0.lock().unwrap();
        if !s.free {
            s.anchor = Some((at, start));
            s.limit = end;
        }
    }

    /// Carries on from where the sound got to (or from the start) on the wall clock.
    fn no_sound(&self) {
        let mut s = self.0.lock().unwrap();
        if s.free {
            return;
        }
        let now = Instant::now();
        let p = s.position(now).unwrap_or(0.0);
        s.free = true;
        if s.ready {
            s.anchor = Some((now, p));
        }
    }
}

/// The streams to play, as ffmpeg `-map` specifiers, and the frame rate.
struct Streams {
    video: String,
    audio: Option<String>,
    fps: Option<f64>,
}

fn probe(path: &Path) -> Streams {
    #[derive(serde::Deserialize)]
    struct Probe {
        streams: Vec<Stream>,
    }
    #[derive(serde::Deserialize)]
    struct Stream {
        index: usize,
        codec_type: String,
        avg_frame_rate: Option<String>,
        #[serde(default)]
        disposition: Disposition,
    }
    #[derive(serde::Deserialize, Default)]
    struct Disposition {
        #[serde(default)]
        attached_pic: u8,
    }

    let out = Command::new("ffprobe")
        .args(["-v", "error", "-of", "json", "-show_entries"])
        .arg("stream=index,codec_type,avg_frame_rate:stream_disposition=attached_pic")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let probe = out.ok().filter(|o| o.status.success()).and_then(|o| serde_json::from_slice::<Probe>(&o.stdout).ok());
    let Some(probe) = probe else {
        // No ffprobe: let ffmpeg choose, with sound if there is any.
        return Streams { video: "0:v:0".into(), audio: Some("0:a:0?".into()), fps: None };
    };
    // Cover art in MP4s is a "video" stream of one picture.
    let video = probe.streams.iter().find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0);
    let audio = probe.streams.iter().find(|s| s.codec_type == "audio");
    let rate = |r: &str| {
        let (a, b) = r.split_once('/')?;
        let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
        (a > 0.0 && b > 0.0).then(|| a / b)
    };
    Streams {
        video: video.map_or("0:v:0".into(), |s| format!("0:{}", s.index)),
        audio: audio.map(|s| format!("0:{}", s.index)),
        fps: video.and_then(|s| rate(s.avg_frame_rate.as_deref()?)),
    }
}

/// Opens a file with the desktop's default application.
pub fn open_external(path: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = Command::new(opener).arg(path).spawn() {
        eprintln!("mappa-mundi: could not run {opener}: {e}");
    }
}
