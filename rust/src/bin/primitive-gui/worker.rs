//! The worker: one thread that owns the GPU, the loaded file and all the
//! job state. The web server sends it commands and shows what it publishes.

use crate::export::{self, FileWriter, Naming, Payload};
use crate::video::{self, VideoInfo};
use image::codecs::jpeg::JpegEncoder;
use primitive_gpu::core::Metric;
use primitive_gpu::load;
use primitive_gpu::constraints::{AngleRule, Constraints, Detail, Mask};
use primitive_gpu::model::{self, Model, SearchConfig};
use primitive_gpu::shapes::Shape;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_skia::Pixmap;
use tokio::sync::{oneshot, watch};

/// Long side of the live picture. The output size is separate: shapes are
/// stored and redrawn at any size on export.
const PREVIEW: u32 = 1024;
/// Most pictures and status messages sent per second.
const PUBLISH_EVERY: Duration = Duration::from_millis(50);
/// Largest temp file for decoded video frames.
const MAX_DECODE_BYTES: f64 = 4.0e9;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub count: usize,
    /// Stop a picture early once this share of its contrast is matched,
    /// in percent (see `Settings::done`). 0 = off.
    pub stop_match: f64,
    /// Lowest score that `stop_match` asks for, whatever the picture's
    /// contrast. 0 = off.
    pub stop_floor: f64,
    /// Fewest shapes before `stop_match` may stop a picture.
    pub min_count: usize,
    pub mode: i32,
    pub alpha: i32,
    /// Hex color, or empty for the average color of the picture.
    pub background: String,
    pub work_size: u32,
    /// Long side of exports and renders. Worked out from `out_preset` by
    /// the program; the page does not set it.
    pub out_size: u32,
    /// "source", "720", "1080", "1440", "2160" or "custom".
    pub out_preset: String,
    /// Box for "custom": the picture fits inside it.
    pub out_w: u32,
    pub out_h: u32,
    /// Export file type, one of `export::IMAGE_FORMATS` or `VIDEO_FORMATS`.
    pub export_fmt: String,
    /// File name pattern with tags such as {name} and {frame}.
    pub name_pattern: String,
    /// Digits of {frame}, padded with zeros.
    pub frame_digits: u32,
    /// Number of the first frame.
    pub frame_start: u64,
    /// Frame rate of an image sequence.
    pub seq_fps: f64,
    pub repeat: usize,
    pub chains: usize,
    pub random: usize,
    pub age: usize,
    pub k: usize,
    /// Empty for a seed from the clock.
    pub seed: String,
    /// Video: each frame starts from the previous frame's picture.
    pub build_on: bool,
    /// "Build on" mode: each frame is built for exactly its own length of
    /// time (1 / frame rate), so the build runs at playback speed.
    pub realtime: bool,
    /// "Build on" mode: the first frame is drawn on top of the last
    /// frame's picture, so the video joins up when it plays in a loop.
    pub loop_video: bool,
    /// How the gap to the original is counted: "squared" or "plain".
    pub metric: String,
    /// Shape rules (creative constraints). Direction: "free", "fixed",
    /// "steps", "toward" or "around".
    pub c_angle: String,
    pub c_angle_deg: f64,
    pub c_angle_step: f64,
    /// The spot for "toward" and "around", in percent of width and height.
    pub c_x: f64,
    pub c_y: f64,
    /// Smallest and largest shape in percent of the long side. 0 = off.
    pub c_min_size: f64,
    pub c_max_size: f64,
    /// The largest size falls to the smallest over `count` shapes.
    pub c_shrink: bool,
    /// Limits for long side / short side. 0 = off.
    pub c_min_ratio: f64,
    pub c_max_ratio: f64,
    /// Grid cells along the long side. 0 = off.
    pub c_grid: u32,
    /// What the painted area means: "only" (shapes stay inside it) or
    /// "detail" (small shapes inside it, big shapes in the rest).
    pub c_area: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            count: 250,
            stop_match: 0.0,
            stop_floor: 0.01,
            min_count: 0,
            mode: 1,
            alpha: 128,
            background: String::new(),
            work_size: 256,
            out_size: 1024,
            out_preset: "source".into(),
            out_w: 1920,
            out_h: 1080,
            export_fmt: "png".into(),
            name_pattern: "{name}-{shapes}_{frame}".into(),
            frame_digits: 4,
            frame_start: 1,
            seq_fps: 24.0,
            repeat: 0,
            chains: 16,
            random: 1000,
            age: 100,
            k: 64,
            seed: String::new(),
            build_on: false,
            realtime: false,
            loop_video: false,
            metric: "squared".into(),
            c_angle: "free".into(),
            c_angle_deg: 0.0,
            c_angle_step: 45.0,
            c_x: 50.0,
            c_y: 50.0,
            c_min_size: 0.0,
            c_max_size: 0.0,
            c_shrink: false,
            c_min_ratio: 0.0,
            c_max_ratio: 0.0,
            c_grid: 0,
            c_area: "only".into(),
        }
    }
}

impl Settings {
    /// The score at which `stop_match` stops a picture of this contrast,
    /// or None when it is off. The floor keeps a nearly flat picture from
    /// taking shapes that only chase noise.
    fn stop_score(&self, contrast: f64) -> Option<f64> {
        (self.stop_match > 0.0).then(|| (contrast * (1.0 - self.stop_match / 100.0)).max(self.stop_floor))
    }

    /// True when a picture with `n` shapes, this score and this contrast
    /// needs no more shapes: it has the shape limit, or it has the fewest
    /// shapes and its score is at or below `stop_score`.
    fn done(&self, n: usize, score: Option<f64>, contrast: f64) -> bool {
        let reached = || matches!((score, self.stop_score(contrast)), (Some(sc), Some(t)) if sc <= t);
        n >= self.count || (n >= self.min_count && reached())
    }

    fn clamped(mut self) -> Settings {
        self.count = self.count.clamp(1, 100_000);
        self.min_count = self.min_count.min(100_000);
        if !(self.stop_match > 0.0 && self.stop_match < 100.0) {
            self.stop_match = 0.0;
        }
        if !(self.stop_floor >= 0.0 && self.stop_floor < 1.0) {
            self.stop_floor = 0.0;
        }
        self.mode = self.mode.clamp(0, 8);
        self.alpha = self.alpha.clamp(0, 255);
        self.work_size = self.work_size.clamp(16, 2048);
        self.out_size = self.out_size.clamp(16, 16384);
        if !["source", "720", "1080", "1440", "2160", "custom"].contains(&self.out_preset.as_str()) {
            self.out_preset = "source".into();
        }
        self.out_w = self.out_w.clamp(16, 16384);
        self.out_h = self.out_h.clamp(16, 16384);
        self.name_pattern = self.name_pattern.chars().take(200).collect();
        self.frame_digits = self.frame_digits.min(10);
        self.frame_start = self.frame_start.min(1_000_000_000);
        self.seq_fps = if self.seq_fps.is_finite() { self.seq_fps.clamp(1.0, 240.0) } else { 24.0 };
        self.repeat = self.repeat.min(100);
        self.chains = self.chains.clamp(1, 1024);
        self.random = self.random.clamp(1, 100_000);
        self.age = self.age.clamp(1, 100_000);
        self.k = self.k.clamp(1, 1024);
        self.background = self.background.trim().to_string();
        self.seed = self.seed.trim().to_string();
        self.metric = self.metric_kind().name().into();
        if !["free", "fixed", "steps", "toward", "around"].contains(&self.c_angle.as_str()) {
            self.c_angle = "free".into();
        }
        let fin = |v: f64, lo: f64, hi: f64, d: f64| if v.is_finite() { v.clamp(lo, hi) } else { d };
        self.c_angle_deg = fin(self.c_angle_deg, -360.0, 360.0, 0.0);
        self.c_angle_step = fin(self.c_angle_step, 1.0, 180.0, 45.0);
        self.c_x = fin(self.c_x, -100.0, 200.0, 50.0);
        self.c_y = fin(self.c_y, -100.0, 200.0, 50.0);
        self.c_min_size = fin(self.c_min_size, 0.0, 100.0, 0.0);
        self.c_max_size = fin(self.c_max_size, 0.0, 100.0, 0.0);
        self.c_min_ratio = fin(self.c_min_ratio, 0.0, 100.0, 0.0);
        self.c_max_ratio = fin(self.c_max_ratio, 0.0, 100.0, 0.0);
        self.c_grid = self.c_grid.min(256);
        if self.c_area != "detail" {
            self.c_area = "only".into();
        }
        self
    }

    /// The shape rules, without the painted area.
    fn constraints(&self) -> Constraints {
        let (angle, angle_step) = match self.c_angle.as_str() {
            "fixed" => (AngleRule::Fixed, 0.0),
            "steps" => (AngleRule::Free, self.c_angle_step),
            "toward" => (AngleRule::Toward, 0.0),
            "around" => (AngleRule::Around, 0.0),
            _ => (AngleRule::Free, 0.0),
        };
        Constraints {
            angle,
            angle_deg: self.c_angle_deg,
            angle_step,
            point: (self.c_x / 100.0, self.c_y / 100.0),
            min_size: self.c_min_size / 100.0,
            max_size: self.c_max_size / 100.0,
            shrink_over: if self.c_shrink { self.count } else { 0 },
            min_ratio: self.c_min_ratio,
            max_ratio: self.c_max_ratio,
            grid_cells: self.c_grid,
            mask: None,
            detail: None,
        }
    }

    fn metric_kind(&self) -> Metric {
        Metric::parse(&self.metric).unwrap_or_default()
    }

    fn search(&self) -> SearchConfig {
        SearchConfig {
            chains: self.chains,
            random: self.random,
            age: self.age,
            k: self.k,
            gpu_draw: true,
            check: false,
            metric: self.metric_kind(),
        }
    }

    /// The settings that change the starting picture or the score and need
    /// "Restart".
    fn restart_key(&self) -> (u32, String, String, String) {
        (self.work_size, self.background.to_lowercase(), self.seed.clone(), self.metric.clone())
    }
}

pub enum Cmd {
    Settings(Settings),
    Play,
    Pause,
    /// "Build on" mode: stop (true) or go on with (false) building frames.
    PauseBuild(bool),
    Restart,
    /// The painted area where shapes may be: `w` x `h` gray values, or
    /// empty for no limit.
    Mask { w: usize, h: usize, gray: Vec<u8> },
    Seek(usize),
    Render,
    Cancel,
    DismissError,
    Load { path: PathBuf, name: String },
    /// A folder of uploaded pictures, named so that they sort in order.
    LoadSequence { dir: PathBuf, name: String },
    Export(oneshot::Sender<Result<ExportData, String>>),
    Quit,
}

/// Everything an image export needs, copied out of the worker.
pub struct ExportData {
    /// File name from the user's pattern, without the extension.
    pub stem: String,
    pub shapes: Vec<Shape>,
    pub colors: Vec<[u8; 4]>,
    pub bg: [u8; 4],
    pub w: i32,
    pub h: i32,
    pub out_size: u32,
}

/// The picture to show: a canvas to encode, or a ready JPEG.
pub enum Pic {
    Raw(Pixmap),
    Jpeg(Vec<u8>),
}

/// Hands the newest picture to the encoder thread. Older pictures that were
/// not taken yet are dropped.
#[derive(Default)]
pub struct PicSlot {
    pub pic: Mutex<Option<Pic>>,
    pub ready: Condvar,
}

impl PicSlot {
    fn put(&self, p: Pic) {
        *self.pic.lock().unwrap() = Some(p);
        self.ready.notify_one();
    }
}

pub fn jpeg(rgba: &image::RgbaImage, quality: u8) -> Vec<u8> {
    let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
    let mut out = Vec::new();
    let _ = JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb);
    out
}

/// The encoder thread: turns canvases into JPEGs for the pages.
pub fn encoder(slot: Arc<PicSlot>, out: watch::Sender<Arc<Vec<u8>>>) {
    loop {
        let pic = {
            let mut g = slot.pic.lock().unwrap();
            while g.is_none() {
                g = slot.ready.wait(g).unwrap();
            }
            g.take().unwrap()
        };
        let bytes = match pic {
            Pic::Raw(pm) => jpeg(&load::pixmap_to_rgba(&pm), 85),
            Pic::Jpeg(b) => b,
        };
        out.send_replace(Arc::new(bytes));
    }
}

/// Saved result of one video frame.
#[derive(Clone, Default)]
struct FrameData {
    shapes: Vec<Shape>,
    colors: Vec<[u8; 4]>,
    scores: Vec<f64>,
    /// `Model::contrast` of the frame's picture, for `Settings::done`.
    contrast: f64,
    /// Time spent adding the frame's shapes.
    time: Duration,
}

struct ImageSrc {
    path: PathBuf,
    name: String,
}

struct VideoSrc {
    /// The uploaded file, kept for its sound. For an image sequence, the
    /// folder that holds the pictures.
    path: PathBuf,
    /// Image sequence: the pictures in order.
    files: Option<Vec<PathBuf>>,
    name: String,
    info: VideoInfo,
    /// Decoded frames at working size, raw RGBA, one after the other.
    raw: PathBuf,
    w: u32,
    h: u32,
    frames: Vec<FrameData>,
    /// Frame loaded in the model.
    cur: usize,
    /// Frame shown (the playhead).
    view: usize,
    /// "Build on" mode: first frame that is not finished.
    frontier: usize,
    /// Playback clock: when play started, and from which frame.
    clock: Option<(Instant, usize)>,
    /// Background of frame 0, used by "build on" mode.
    bg0: [u8; 4],
    /// "Loop video": the second build of every frame. It starts on the
    /// last frame's picture of the first build, so the search picks shapes
    /// for that picture instead of a plain color. Empty until it starts.
    pass2: Vec<FrameData>,
    /// First frame of the second build that is not finished.
    pass2_next: usize,
    /// The last frame's picture of the first build, where the second build
    /// starts.
    end1: Option<(Vec<u8>, Pixmap)>,
}

/// What turns the source into raw frames: ffmpeg for a video, a thread
/// for an image sequence.
enum Decoder {
    Ffmpeg(Child),
    Images { handle: Option<JoinHandle<Result<(), String>>>, stop: Arc<AtomicBool> },
}

impl Decoder {
    fn kill(&mut self) {
        match self {
            Decoder::Ffmpeg(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            Decoder::Images { handle, stop } => {
                stop.store(true, Ordering::Relaxed);
                if let Some(h) = handle.take() {
                    let _ = h.join();
                }
            }
        }
    }

    /// None while it runs; then whether it worked.
    fn poll(&mut self) -> Option<Result<(), String>> {
        match self {
            Decoder::Ffmpeg(c) => match c.try_wait() {
                Ok(None) => None,
                Ok(Some(st)) => {
                    let mut err = String::new();
                    if let Some(mut e) = c.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    Some(if st.success() { Ok(()) } else { Err(err.trim().to_string()) })
                }
                Err(e) => Some(Err(format!("ffmpeg: {e}"))),
            },
            Decoder::Images { handle, .. } => {
                if !handle.as_ref().is_some_and(|h| h.is_finished()) {
                    return None;
                }
                Some(handle.take().unwrap().join().unwrap_or_else(|_| Err("the picture reader crashed".into())))
            }
        }
    }
}

struct Decode {
    job: Decoder,
    files: Option<Vec<PathBuf>>,
    path: PathBuf,
    name: String,
    info: VideoInfo,
    raw: PathBuf,
    w: u32,
    h: u32,
    expected: f64,
}

/// Where a render goes.
enum Sink {
    Video { child: Child, stdin: ChildStdin, video_only: PathBuf, final_path: PathBuf, container: &'static str },
    Files { dir: PathBuf, fmt: &'static str, writer: FileWriter, naming: Naming },
}

/// Most laps "Loop video" draws before it takes the picture as settled.
const MAX_LOOP_LAPS: u32 = 5;
/// Two pictures are the same for "Loop video" when no color value differs
/// by more than this.
const LOOP_SAME: u8 = 2;

/// "Loop video": the preview frames drawn again on top of the last
/// frame's picture. One lap draws the shapes of every frame in order.
enum LoopState {
    /// Not made yet.
    Idle,
    Running(LoopPass),
    /// The loop frames are in the cache. `laps` is how many laps from the
    /// plain background the picture needs before it stops changing.
    Ready { laps: u32 },
}

struct LoopPass {
    canvas: Pixmap,
    /// The picture at the start of this lap.
    start: Pixmap,
    next: usize,
    laps: u32,
    /// This lap saves its frames to the cache.
    writing: bool,
}

struct RenderJob {
    next: usize,
    /// "Loop video": laps drawn so far without saving frames.
    lap: u32,
    sink: Sink,
    /// "Build on" mode with SVG files: every shape so far.
    acc: Option<(Vec<Shape>, Vec<[u8; 4]>)>,
    /// "Build on" mode: the output canvas, to which each frame adds its own
    /// shapes.
    out: Option<Pixmap>,
    ow: u32,
    oh: u32,
    scale: f64,
    started: Instant,
    first: usize,
}

enum Source {
    None,
    Image(ImageSrc),
    Video(VideoSrc),
}

pub struct Worker {
    rx: Receiver<Cmd>,
    tmp: PathBuf,
    renders: PathBuf,
    settings: Settings,
    /// `restart_key` of the settings the current job was started with.
    applied: (u32, String, String, String),
    /// The painted area where shapes may be: width, height, gray values.
    mask: Option<(usize, usize, Vec<u8>)>,
    /// The seed the current search was started with (shown on the page).
    used_seed: u64,
    source: Source,
    model: Option<Model>,
    playing: bool,
    /// "Build on" mode: when the user paused the building of new frames
    /// (and of a render), the moment they paused. None = building runs.
    build_paused: Option<Instant>,
    /// "Real time" building: the moment the frame that is being built must
    /// be finished. None = set it when the next frame starts to build.
    rt_due: Option<Instant>,
    decode: Option<Decode>,
    render: Option<RenderJob>,
    render_file: Option<String>,
    /// The last render was an image sequence: `render_file` is its folder.
    render_folder: bool,
    /// Full size of the loaded picture or video, for "same as source".
    src_dims: (u32, u32),
    error: Option<String>,
    original_version: u64,
    status_tx: Arc<watch::Sender<String>>,
    original_tx: watch::Sender<Arc<Vec<u8>>>,
    pics: Arc<PicSlot>,
    /// Set while the worker has work no page needs to watch (see `is_busy`).
    busy: Arc<AtomicBool>,
    last_publish: Instant,
    /// Time spent adding the shapes of the picture in the model (shown on
    /// the page next to the match).
    build_time: Duration,
    dirty_pic: bool,
    dirty_status: bool,
    /// Last shown "build on" frame from the cache, so it is not sent twice.
    shown_cached: Option<usize>,
    loop_pass: LoopState,
    /// Restart with a new working size: the frame the playhead was on, to
    /// go back to when the video is decoded again.
    restart_view: Option<usize>,
}

fn bg_from(background: &str, target: &[u8]) -> [u8; 4] {
    if background.is_empty() {
        load::average_color(target)
    } else {
        load::parse_hex(background)
    }
}

/// Starts a thread that reads the pictures of an image sequence in order,
/// scales each to `w` x `h` and appends it to `raw` as straight RGBA, the
/// same layout ffmpeg writes for a video.
fn read_pictures(files: Vec<PathBuf>, w: u32, h: u32, raw: PathBuf) -> Decoder {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::spawn(move || -> Result<(), String> {
        let mut out = std::io::BufWriter::new(File::create(&raw).map_err(|e| e.to_string())?);
        for (i, f) in files.iter().enumerate() {
            if flag.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            let img = load::open_image(&f.to_string_lossy()).map_err(|_| format!("picture {} of {} is not a picture this program can read", i + 1, files.len()))?;
            let mut rgba = img.to_rgba8();
            if rgba.dimensions() != (w, h) {
                rgba = image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Triangle);
            }
            out.write_all(rgba.as_raw()).map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())
    });
    Decoder::Images { handle: Some(handle), stop }
}

/// Removes a file, or a whole folder (an uploaded image sequence).
fn remove_path(p: &Path) {
    if p.is_dir() {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

/// A random seed from the clock, kept to 9 digits so it is easy to read
/// and type back in.
fn now_seed() -> u64 {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    n % 999_999_999 + 1
}

impl Worker {
    pub fn new(
        rx: Receiver<Cmd>,
        tmp: PathBuf,
        renders: PathBuf,
        status_tx: Arc<watch::Sender<String>>,
        original_tx: watch::Sender<Arc<Vec<u8>>>,
        pics: Arc<PicSlot>,
        busy: Arc<AtomicBool>,
    ) -> Worker {
        let settings = Settings::default();
        Worker {
            rx,
            tmp,
            renders,
            applied: settings.restart_key(),
            mask: None,
            used_seed: 0,
            settings,
            source: Source::None,
            model: None,
            playing: false,
            build_paused: None,
            rt_due: None,
            decode: None,
            render: None,
            render_file: None,
            render_folder: false,
            src_dims: (0, 0),
            error: None,
            original_version: 0,
            status_tx,
            original_tx,
            pics,
            busy,
            last_publish: Instant::now(),
            build_time: Duration::ZERO,
            dirty_pic: false,
            dirty_status: true,
            shown_cached: None,
            loop_pass: LoopState::Idle,
            restart_view: None,
        }
    }

    fn fail(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        eprintln!("error: {m}");
        self.error = Some(m);
        self.dirty_status = true;
    }

    /// Runs until `Quit` or until the server goes away.
    pub fn run(&mut self) {
        loop {
            let wait = self.idle_wait();
            let first = match wait {
                None => self.rx.try_recv().ok(),
                Some(d) => match self.rx.recv_timeout(d) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return self.shutdown(),
                },
            };
            let mut cmds: Vec<Cmd> = first.into_iter().collect();
            cmds.extend(self.rx.try_iter());
            for c in cmds {
                if matches!(c, Cmd::Quit) {
                    return self.shutdown();
                }
                self.command(c);
            }
            self.tick();
            self.publish(false);
            self.busy.store(self.is_busy(), Ordering::Relaxed);
        }
    }

    /// True while loading a video, rendering, or still adding shapes. The
    /// server does not stop by itself while this is true. Playing a video
    /// does not count.
    fn is_busy(&self) -> bool {
        self.decode.is_some() || self.render.is_some() || self.idle_wait().is_none() || self.rt_wait().is_some()
    }

    fn shutdown(&mut self) {
        if let Some(mut d) = self.decode.take() {
            d.job.kill();
        }
        self.cancel_render();
    }

    // ---------------------------------------------------------------- state

    fn video(&self) -> Option<&VideoSrc> {
        match &self.source {
            Source::Video(v) => Some(v),
            _ => None,
        }
    }

    fn shape_count(&self) -> usize {
        self.model.as_ref().map_or(0, |m| m.shapes.len())
    }

    /// True when frame building is paused. Only "build on" mode can pause.
    fn building_paused(&self) -> bool {
        self.build_paused.is_some() && self.settings.build_on && self.video().is_some()
    }

    /// "Real time" building is on for this source.
    fn realtime(&self) -> bool {
        self.settings.realtime && self.settings.build_on && self.video().is_some()
    }

    /// "Loop video" is on for this source.
    fn looping(&self) -> bool {
        self.settings.loop_video && self.settings.build_on && self.video().is_some_and(|v| !v.frames.is_empty())
    }

    /// "Loop video": every frame of the first build is done and the second
    /// build is not finished.
    fn pass2_building(&self) -> bool {
        self.looping() && self.video().is_some_and(|v| v.frontier >= v.frames.len() && v.pass2_next < v.frames.len())
    }

    /// "Loop video": how many laps a render draws without saving before the
    /// lap it saves. The first is the first build, the rest are the second
    /// build drawn again. None = the loop frames are not made yet.
    fn render_laps(&self) -> Option<u32> {
        match self.loop_pass {
            LoopState::Ready { laps } => Some(laps + 1),
            _ => None,
        }
    }

    /// "Loop video": every frame is built and the loop frames are not
    /// made yet.
    fn loop_pending(&self) -> bool {
        self.looping()
            && self.video().is_some_and(|v| v.frontier >= v.frames.len())
            && !matches!(self.loop_pass, LoopState::Ready { .. })
    }

    /// "Loop video": a short piece of the redraw of the second build. It
    /// starts on the second build's last picture. Laps without saving run
    /// until a lap ends on the picture it started from. Then one more lap
    /// saves every frame to the cache.
    fn loop_step(&mut self) {
        let (Some(m), Source::Video(v)) = (&self.model, &self.source) else { return };
        let n = v.frames.len();
        if !matches!(self.loop_pass, LoopState::Running(_)) {
            let pass = LoopPass { canvas: m.canvas.clone(), start: m.canvas.clone(), next: 0, laps: 1, writing: false };
            self.loop_pass = LoopState::Running(pass);
            self.dirty_status = true;
        }
        let LoopState::Running(p) = &mut self.loop_pass else { return };
        let dir = self.tmp.join("built");
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(30) {
            if p.next < n {
                let fd = &v.pass2[p.next];
                model::draw_shapes(&mut p.canvas, &fd.shapes, &fd.colors, m.scale);
                if p.writing {
                    let bytes = jpeg(&load::pixmap_to_rgba(&p.canvas), 85);
                    if let Err(e) = std::fs::write(dir.join(format!("loop-{}.jpg", p.next)), bytes) {
                        self.loop_pass = LoopState::Idle;
                        self.settings.loop_video = false;
                        return self.fail(format!("writing the frame cache: {e}"));
                    }
                }
                p.next += 1;
            } else if p.writing {
                self.loop_pass = LoopState::Ready { laps: p.laps };
                self.shown_cached = None;
                self.dirty_pic = true;
                self.dirty_status = true;
                return;
            } else {
                let same = p.canvas.data().iter().zip(p.start.data()).all(|(a, b)| a.abs_diff(*b) <= LOOP_SAME);
                if same || p.laps >= MAX_LOOP_LAPS {
                    p.writing = true;
                } else {
                    p.laps += 1;
                    p.start = p.canvas.clone();
                }
                p.next = 0;
            }
        }
    }

    /// "Real time" building: the frame has all its shapes before its time
    /// is up. How long the loop must wait until the frame's time is up.
    fn rt_wait(&self) -> Option<Duration> {
        if !self.realtime() || !self.full() {
            return None;
        }
        let v = self.video()?;
        let n = v.frames.len();
        let building = (v.frontier < n && self.render.as_ref().is_none_or(|r| r.next >= v.frontier))
            || (self.pass2_building() && self.render.as_ref().is_none_or(|r| r.next >= n));
        if !building {
            return None;
        }
        self.rt_due.map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// "Build on" mode: one piece of work on the first unfinished frame.
    /// Normally a frame is finished when it is full. In "real time" the
    /// clock decides: the frame gets shapes until its time is up, and a
    /// frame that is full early waits for the clock.
    fn build_frontier(&mut self) {
        if self.pass2_building() && self.video().is_some_and(|v| v.pass2.is_empty()) {
            self.start_pass2();
        }
        if self.realtime() {
            let fps = self.video().map_or(24.0, |v| v.info.fps).max(0.1);
            let frame = Duration::from_secs_f64(1.0 / fps);
            let now = Instant::now();
            let due = *self.rt_due.get_or_insert(now + frame);
            if now >= due {
                // A step that ran over takes its extra time from the next frame.
                self.rt_due = Some((due + frame).max(now));
                self.finish_frontier();
            } else if !self.full() {
                self.step();
            }
        } else if !self.full() {
            self.step();
        } else {
            self.finish_frontier();
        }
    }

    /// `Settings::done` for the picture that is being worked on now.
    fn full(&self) -> bool {
        self.model.as_ref().is_some_and(|m| self.settings.done(m.shapes.len(), Some(m.score), m.contrast))
    }

    /// True when there is search work that "Restart" or a mode flip would
    /// throw away.
    fn has_work(&self) -> bool {
        match &self.source {
            Source::Video(v) => self.shape_count() > 0 || v.frames.iter().any(|f| !f.shapes.is_empty()),
            Source::Image(_) => self.shape_count() > 0,
            Source::None => false,
        }
    }

    /// How long the loop may wait for a command. None = there is work to
    /// do now.
    fn idle_wait(&self) -> Option<Duration> {
        if self.decode.is_some() {
            return Some(Duration::from_millis(100));
        }
        if self.render.is_some() {
            if self.building_paused() {
                return Some(Duration::from_millis(100));
            }
            return self.rt_wait();
        }
        if self.model.is_none() {
            return Some(Duration::from_secs(1));
        }
        let full = self.full();
        match &self.source {
            Source::Image(_) => (self.playing && !full).then_some(()).map_or(Some(Duration::from_secs(1)), |_| None),
            Source::Video(v) => {
                if self.pass2_building() {
                    if !self.building_paused() {
                        return self.rt_wait();
                    }
                } else if self.loop_pending() {
                    return None;
                }
                if self.settings.build_on && v.frontier < v.frames.len() && !self.building_paused() {
                    return self.rt_wait();
                }
                if !full && !self.settings.build_on {
                    return None;
                }
                if self.playing {
                    let fps = v.info.fps;
                    let (t0, f0) = v.clock.unwrap_or((Instant::now(), v.view));
                    let shown = v.view.max(f0);
                    let next_due = t0 + Duration::from_secs_f64((shown + 1 - f0) as f64 / fps);
                    Some(next_due.saturating_duration_since(Instant::now()).min(Duration::from_millis(100)))
                } else {
                    Some(Duration::from_secs(1))
                }
            }
            Source::None => Some(Duration::from_secs(1)),
        }
    }

    // ------------------------------------------------------------- commands

    fn command(&mut self, c: Cmd) {
        self.dirty_status = true;
        match c {
            Cmd::Settings(s) => {
                if self.render.is_some() {
                    return;
                }
                self.apply_settings(s.clamped());
            }
            Cmd::Play => {
                if self.render.is_some() {
                    return;
                }
                if let Source::Image(_) = self.source {
                    if self.full() {
                        return;
                    }
                }
                self.playing = true;
                if let Source::Video(v) = &mut self.source {
                    v.clock = Some((Instant::now(), v.view));
                }
            }
            Cmd::Pause => {
                self.playing = false;
                if let Source::Video(v) = &mut self.source {
                    v.clock = None;
                }
            }
            Cmd::PauseBuild(pause) => {
                match (pause, self.build_paused) {
                    (true, None) => self.build_paused = Some(Instant::now()),
                    (false, Some(t)) => {
                        // Leave the paused time out of the render's time left.
                        if let Some(r) = &mut self.render {
                            r.started += t.elapsed();
                        }
                        self.build_paused = None;
                        self.rt_due = None;
                    }
                    _ => {}
                }
            }
            Cmd::Restart => {
                if self.render.is_none() {
                    self.restart();
                }
            }
            Cmd::Mask { w, h, gray } => {
                if self.render.is_some() {
                    return;
                }
                self.mask = (w > 0 && h > 0 && gray.len() == w * h).then_some((w, h, gray));
                self.push_constraints();
            }
            Cmd::Seek(f) => {
                if self.render.is_none() {
                    self.seek(f);
                }
            }
            Cmd::Render => self.start_render(),
            Cmd::Cancel => self.cancel_render(),
            Cmd::DismissError => self.error = None,
            Cmd::Load { path, name } => {
                if self.render.is_some() {
                    self.fail("A render is running. Cancel it before loading a new file.");
                    let _ = std::fs::remove_file(&path);
                    return;
                }
                self.load(path, name);
            }
            Cmd::LoadSequence { dir, name } => {
                if self.render.is_some() {
                    self.fail("A render is running. Cancel it before loading a new file.");
                    let _ = std::fs::remove_dir_all(&dir);
                    return;
                }
                self.load_sequence(dir, name);
            }
            Cmd::Export(reply) => {
                let _ = reply.send(self.export_data());
            }
            Cmd::Quit => {}
        }
    }

    fn apply_settings(&mut self, s: Settings) {
        let old = std::mem::replace(&mut self.settings, s);
        self.settings.out_size = old.out_size;
        self.resolve_out();
        self.fix_format();
        let new = self.settings.clone();
        // A new frame rate for an image sequence changes playback and renders.
        if new.seq_fps != old.seq_fps {
            let fps = new.seq_fps;
            if let Source::Video(v) = &mut self.source {
                if v.files.is_some() {
                    v.info.fps = fps;
                    v.info.fps_text = format!("{fps}");
                    v.info.duration = v.frames.len() as f64 / fps;
                    if v.clock.is_some() {
                        v.clock = Some((Instant::now(), v.view));
                    }
                }
            }
        }
        if let Some(m) = &mut self.model {
            m.set_search(new.search());
        }
        self.push_constraints();
        if old.realtime != new.realtime {
            self.rt_due = None;
        }
        if old.loop_video != new.loop_video {
            self.shown_cached = None;
            self.dirty_pic = true;
        }
        // A flip of "fresh / build on" clears every frame.
        if old.build_on != new.build_on && self.video().is_some() {
            self.reset_video_frames();
        }
        // A higher shape count or another stop setting lets a finished
        // image go on.
        let loosened = new.count > old.count || new.stop_match != old.stop_match || new.stop_floor != old.stop_floor || new.min_count > old.min_count;
        if loosened && matches!(self.source, Source::Image(_)) && !self.full() {
            let was_full = self.model.as_ref().is_some_and(|m| old.done(m.shapes.len(), Some(m.score), m.contrast));
            if was_full {
                self.playing = true;
            }
        }
        // A lower shape count cuts the current picture.
        if new.count < old.count && !(self.settings.build_on && self.video().is_some()) {
            let n = self.settings.count;
            if let Some(m) = &mut self.model {
                if m.shapes.len() > n {
                    m.truncate(n);
                    self.dirty_pic = true;
                }
            }
        }
    }

    /// Works out the output long side from the size choice and the size of
    /// the loaded file. The picture keeps its shape and fits inside the box.
    /// The presets turn the box upright for an upright picture.
    fn resolve_out(&mut self) {
        let (sw, sh) = self.src_dims;
        if sw == 0 || sh == 0 {
            return;
        }
        let s = &self.settings;
        let (bw, bh) = match s.out_preset.as_str() {
            "custom" => (s.out_w, s.out_h),
            "source" => (sw, sh),
            p => {
                let short: u32 = p.parse().unwrap_or(1080);
                let long = short * 16 / 9;
                if sw >= sh {
                    (long, short)
                } else {
                    (short, long)
                }
            }
        };
        let scale = (bw as f64 / sw as f64).min(bh as f64 / sh as f64);
        self.settings.out_size = ((sw.max(sh) as f64 * scale).round() as u32).clamp(16, 16384);
    }

    /// Picks a format that fits the loaded file when the current one does not.
    fn fix_format(&mut self) {
        let list = if self.video().is_some() { export::VIDEO_FORMATS } else { export::IMAGE_FORMATS };
        if !list.contains(&self.settings.export_fmt.as_str()) {
            self.settings.export_fmt = list[0].to_string();
        }
    }

    fn source_name(&self) -> &str {
        match &self.source {
            Source::Image(i) => &i.name,
            Source::Video(v) => &v.name,
            Source::None => "",
        }
    }

    fn naming(&self) -> Naming {
        let s = &self.settings;
        let shapes = if self.video().is_some() { s.count } else { self.shape_count() };
        Naming::new(&s.name_pattern, s.frame_digits, s.frame_start, self.source_name(), shapes, s.mode)
    }

    /// Example file name for the page, for the chosen format.
    fn name_example(&self) -> String {
        if matches!(self.source, Source::None) {
            return String::new();
        }
        let n = self.naming();
        let fmt = self.settings.export_fmt.as_str();
        match fmt.strip_suffix("-seq") {
            Some(ext) => format!("{}/{}.{ext}", n.single(), n.frame(0)),
            None => format!("{}.{fmt}", n.single()),
        }
    }

    fn seek(&mut self, f: usize) {
        let build_on = self.settings.build_on;
        let Source::Video(v) = &mut self.source else { return };
        let n = v.frames.len();
        let mut f = f.min(n.saturating_sub(1));
        if build_on {
            f = f.min(v.frontier.min(n - 1));
        }
        v.view = f;
        if self.playing {
            v.clock = Some((Instant::now(), f));
        }
        if !build_on {
            self.switch_to(f);
        }
        self.dirty_pic = true;
    }

    // ----------------------------------------------------------- loading

    fn clear_source(&mut self) {
        if let Some(mut d) = self.decode.take() {
            d.job.kill();
            let _ = std::fs::remove_file(&d.raw);
            remove_path(&d.path);
        }
        match std::mem::replace(&mut self.source, Source::None) {
            Source::Image(i) => {
                let _ = std::fs::remove_file(&i.path);
            }
            Source::Video(v) => {
                remove_path(&v.path);
                let _ = std::fs::remove_file(&v.raw);
            }
            Source::None => {}
        }
        let _ = std::fs::remove_dir_all(self.tmp.join("built"));
        self.model = None;
        self.playing = false;
        self.build_paused = None;
        self.rt_due = None;
        self.shown_cached = None;
        self.loop_pass = LoopState::Idle;
    }

    fn load(&mut self, path: PathBuf, name: String) {
        self.clear_source();
        self.error = None;
        self.render_file = None;
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let still = ["png", "jpg", "jpeg", "bmp", "tif", "tiff", "webp", "tga", "ico", "pnm", "ppm", "qoi"];
        let info = if still.contains(&ext.as_str()) {
            None
        } else {
            match video::probe(&path) {
                Ok(i) => i,
                Err(e) => {
                    self.fail(e);
                    return;
                }
            }
        };
        match info {
            Some(info) => self.start_video(path, name, info),
            None => {
                self.source = Source::Image(ImageSrc { path, name });
                self.start_image(true);
            }
        }
    }

    /// An image sequence: the pictures in `dir` in name order become the
    /// frames of a video.
    fn load_sequence(&mut self, dir: PathBuf, name: String) {
        self.clear_source();
        self.error = None;
        self.render_file = None;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect())
            .unwrap_or_default();
        files.sort();
        if files.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            return self.fail("The image sequence has no pictures.");
        }
        let first = match load::open_image(&files[0].to_string_lossy()) {
            Ok(i) => i,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                return self.fail(format!("Could not read the first picture of {name}."));
            }
        };
        let fps = self.settings.seq_fps;
        let info = VideoInfo {
            fps_text: format!("{fps}"),
            fps,
            duration: files.len() as f64 / fps,
            has_audio: false,
            width: first.width(),
            height: first.height(),
        };
        self.src_dims = (info.width, info.height);
        self.decode_video(dir, name, info, Some(files));
    }

    /// True when a setting changed that only "Restart" applies. An empty
    /// seed (random) or the seed already in use needs no restart, so ticking
    /// "Manual seed" keeps the current picture.
    fn needs_restart(&self) -> bool {
        let (k, a) = (self.settings.restart_key(), &self.applied);
        let seed_ok = k.2.is_empty() || k.2 == self.used_seed.to_string();
        k.0 != a.0 || k.1 != a.1 || k.3 != a.3 || !seed_ok
    }

    fn seed(&self) -> u64 {
        self.settings.seed.parse().unwrap_or_else(|_| now_seed())
    }

    /// Background for a new search: the settings' color, or the average
    /// color of `target` when it is empty.
    fn bg_for(&self, target: &[u8]) -> [u8; 4] {
        bg_from(&self.settings.background, target)
    }

    /// Give the model the shape rules from the settings and the painted
    /// area (stretched to the working size).
    fn push_constraints(&mut self) {
        let Some(m) = &mut self.model else {
            return;
        };
        let mut c = self.settings.constraints();
        if let Some((sw, sh, src)) = &self.mask {
            let (w, h) = (m.w as usize, m.h as usize);
            let gray: Vec<u8> = (0..w * h).map(|i| src[(i / w * sh / h) * sw + (i % w) * sw / w]).collect();
            if self.settings.c_area == "detail" {
                c.detail = Detail::new(m.w, m.h, &gray).map(std::sync::Arc::new);
            } else {
                c.mask = Mask::new(m.w, m.h, &gray).map(std::sync::Arc::new);
            }
        }
        m.set_constraints(c);
    }

    fn new_model(&mut self, target: Vec<u8>, w: i32, h: i32) -> bool {
        let bg = self.bg_for(&target);
        let seed = self.seed();
        match Model::new(target, w, h, bg, PREVIEW, self.settings.search(), seed) {
            Ok(m) => {
                self.used_seed = seed;
                self.model = Some(m);
                self.build_time = Duration::ZERO;
                self.push_constraints();
                // A new build starts its own "real time" clock. An old one
                // would be in the past and leave the first frames empty.
                self.rt_due = None;
                self.applied = self.settings.restart_key();
                self.dirty_pic = true;
                self.publish_original();
                true
            }
            Err(e) => {
                self.fail(format!("GPU: {e}"));
                false
            }
        }
    }

    /// (Re)starts the search on the loaded image. `fresh_file` sets the
    /// output size to the picture's own size.
    fn start_image(&mut self, fresh_file: bool) {
        let Source::Image(src) = &self.source else { return };
        let img = match load::open_image(&src.path.to_string_lossy()) {
            Ok(i) => i,
            Err(e) => {
                let name = src.name.clone();
                let e = e.replace(&format!("{}: ", src.path.display()), "");
                self.clear_source();
                self.fail(format!("Could not read {name}. It is not a picture or video this program can open. ({e})"));
                return;
            }
        };
        if fresh_file {
            self.src_dims = (img.width(), img.height());
            self.resolve_out();
            self.fix_format();
        }
        let img = load::shrink(img, self.settings.work_size);
        let (target, w, h) = load::premultiplied(&img);
        if self.new_model(target, w, h) {
            self.playing = true;
        }
    }

    fn start_video(&mut self, path: PathBuf, name: String, info: VideoInfo) {
        self.src_dims = (info.width, info.height);
        self.decode_video(path, name, info, None);
    }

    /// Decodes every frame at working size into one raw file. `files` is
    /// the picture list of an image sequence, None for a video.
    fn decode_video(&mut self, path: PathBuf, name: String, info: VideoInfo, files: Option<Vec<PathBuf>>) {
        self.resolve_out();
        self.restart_view = None;
        let (w, h) = video::fit(info.width, info.height, self.settings.work_size);
        let expected = (info.duration * info.fps).max(1.0);
        let bytes = expected * (w * h * 4) as f64;
        if bytes > MAX_DECODE_BYTES {
            remove_path(&path);
            self.fail(format!(
                "{name} is too long: it would need {:.1} GB of temp space (the limit is 4 GB, about 15 minutes at 30 fps). Try a shorter clip or a smaller working size.",
                bytes / 1e9
            ));
            return;
        }
        let raw = self.tmp.join("frames.rgba");
        let job = match &files {
            Some(list) => Ok(read_pictures(list.clone(), w, h, raw.clone())),
            None => video::start_decode(&path, &info, w, h, &raw).map(Decoder::Ffmpeg),
        };
        match job {
            Ok(job) => self.decode = Some(Decode { job, files, path, name, info, raw, w, h, expected }),
            Err(e) => {
                remove_path(&path);
                self.fail(e);
            }
        }
    }

    fn decode_progress(&self) -> Option<f64> {
        let d = self.decode.as_ref()?;
        let size = std::fs::metadata(&d.raw).map(|m| m.len()).unwrap_or(0) as f64;
        Some((size / (d.expected * (d.w * d.h * 4) as f64)).min(1.0))
    }

    fn poll_decode(&mut self) {
        let Some(d) = &mut self.decode else { return };
        match d.job.poll() {
            None => {
                self.dirty_status = true;
            }
            Some(res) => {
                let d = self.decode.take().unwrap();
                let keep = self.restart_view.take().unwrap_or(0);
                let frame_bytes = (d.w * d.h * 4) as u64;
                let n = std::fs::metadata(&d.raw).map(|m| m.len()).unwrap_or(0) / frame_bytes;
                if res.is_err() || n == 0 {
                    let _ = std::fs::remove_file(&d.raw);
                    remove_path(&d.path);
                    self.fail(format!("Could not decode {}: {}", d.name, res.err().unwrap_or_default()));
                    return;
                }
                let keep = if self.settings.build_on { 0 } else { keep.min(n as usize - 1) };
                let v = VideoSrc {
                    path: d.path,
                    files: d.files,
                    name: d.name,
                    info: d.info,
                    raw: d.raw,
                    w: d.w,
                    h: d.h,
                    frames: vec![FrameData::default(); n as usize],
                    cur: keep,
                    view: keep,
                    frontier: 0,
                    clock: None,
                    bg0: [0; 4],
                    pass2: Vec::new(),
                    pass2_next: 0,
                    end1: None,
                };
                self.source = Source::Video(v);
                self.start_frames_at(keep);
                self.fix_format();
                self.dirty_status = true;
            }
        }
    }

    /// Frame `i` of the video at working size, premultiplied.
    fn read_frame(&self, i: usize) -> Result<Vec<u8>, String> {
        let v = self.video().ok_or("no video")?;
        let n = (v.w * v.h * 4) as usize;
        let mut buf = vec![0u8; n];
        let mut f = File::open(&v.raw).map_err(|e| format!("reading frames: {e}"))?;
        f.seek(SeekFrom::Start((i * n) as u64)).map_err(|e| format!("reading frames: {e}"))?;
        f.read_exact(&mut buf).map_err(|e| format!("reading frame {i}: {e}"))?;
        load::premultiply(&mut buf);
        Ok(buf)
    }

    /// "Restart": start the search again with the current settings. For a
    /// video, every frame's shapes are cleared, and a new working size
    /// decodes the video again.
    fn restart(&mut self) {
        self.render_file = None;
        match &self.source {
            Source::Image(_) => self.start_image(false),
            Source::Video(v) => {
                let (w, _) = video::fit(v.info.width, v.info.height, self.settings.work_size);
                if w != v.w {
                    let Source::Video(v) = std::mem::replace(&mut self.source, Source::None) else { unreachable!() };
                    let _ = std::fs::remove_file(&v.raw);
                    self.model = None;
                    self.playing = false;
                    let view = v.view;
                    self.decode_video(v.path, v.name, v.info, v.files);
                    if self.decode.is_some() {
                        self.restart_view = Some(view);
                    }
                } else {
                    self.reset_video_frames();
                }
            }
            Source::None => {}
        }
    }

    /// Clears every frame's shapes and starts again. "Fresh" mode stays on
    /// the frame the playhead is on. "Build on" mode goes back to frame 0,
    /// because every frame there needs the frames before it.
    fn reset_video_frames(&mut self) {
        let build_on = self.settings.build_on;
        let _ = std::fs::remove_dir_all(self.tmp.join("built"));
        self.shown_cached = None;
        self.rt_due = None;
        self.loop_pass = LoopState::Idle;
        let Source::Video(v) = &mut self.source else { return };
        for f in &mut v.frames {
            *f = FrameData::default();
        }
        v.pass2.clear();
        v.pass2_next = 0;
        v.end1 = None;
        let keep = if build_on { 0 } else { v.view.min(v.frames.len().saturating_sub(1)) };
        v.cur = keep;
        v.view = keep;
        v.frontier = 0;
        if v.clock.is_some() {
            v.clock = Some((Instant::now(), keep));
        }
        self.start_frames_at(keep);
    }

    /// Starts a new model on frame `keep`. The background of frame 0 is
    /// kept for "build on" mode.
    fn start_frames_at(&mut self, keep: usize) {
        let Source::Video(v) = &self.source else { return };
        let (w, h) = (v.w as i32, v.h as i32);
        let first = match self.read_frame(0) {
            Ok(t) => t,
            Err(e) => return self.fail(e),
        };
        let bg = self.bg_for(&first);
        if let Source::Video(v) = &mut self.source {
            v.bg0 = bg;
        }
        let target = if keep == 0 {
            first
        } else {
            match self.read_frame(keep) {
                Ok(t) => t,
                Err(e) => return self.fail(e),
            }
        };
        self.new_model(target, w, h);
    }

    // ------------------------------------------------------------- video

    /// "Fresh" mode: save the loaded frame's shapes and load frame `i`.
    fn switch_to(&mut self, i: usize) {
        let count = self.settings.count;
        let Some(m) = &mut self.model else { return };
        let Source::Video(v) = &mut self.source else { return };
        if v.cur == i && m.shapes.len() <= count {
            return;
        }
        let fd = &mut v.frames[v.cur];
        fd.shapes = m.shapes.clone();
        fd.colors = m.colors.clone();
        fd.scores = m.scores.clone();
        fd.contrast = m.contrast;
        fd.time = self.build_time;
        let target = match self.read_frame(i) {
            Ok(t) => t,
            Err(e) => return self.fail(e),
        };
        let bg = bg_from(&self.applied.1, &target);
        let (Some(m), Source::Video(v)) = (&mut self.model, &mut self.source) else { return };
        let fd = &mut v.frames[i];
        fd.shapes.truncate(count);
        fd.colors.truncate(count);
        fd.scores.truncate(count);
        m.set_target(target, bg, None);
        m.load_shapes(&fd.shapes, &fd.colors, &fd.scores);
        self.build_time = fd.time;
        v.cur = i;
        self.dirty_pic = true;
        self.publish_original();
    }

    fn step(&mut self) {
        let s = &self.settings;
        let (mode, alpha, repeat, count) = (s.mode, s.alpha, s.repeat, s.count);
        if let Some(m) = &mut self.model {
            let t0 = Instant::now();
            // Extra shapes stop at the shape count. Cutting shapes afterwards
            // would lose the picture a "build on" frame started from.
            let room = count.saturating_sub(m.shapes.len() + 1);
            m.step(mode, alpha, repeat.min(room));
            self.build_time += t0.elapsed();
            self.dirty_pic = true;
            self.dirty_status = true;
        }
    }

    /// "Build on" mode: the frontier frame is full. Keep it and start the
    /// next frame from its picture.
    fn finish_frontier(&mut self) {
        if self.pass2_building() {
            return self.finish_pass2();
        }
        let Some(m) = &mut self.model else { return };
        let Source::Video(v) = &mut self.source else { return };
        let i = v.frontier;
        v.frames[i] = FrameData { shapes: m.shapes.clone(), colors: m.colors.clone(), scores: m.scores.clone(), contrast: m.contrast, time: self.build_time };
        self.build_time = Duration::ZERO;
        let dir = self.tmp.join("built");
        let _ = std::fs::create_dir_all(&dir);
        let bytes = jpeg(&load::pixmap_to_rgba(&m.canvas), 85);
        if let Err(e) = std::fs::write(dir.join(format!("{i}.jpg")), bytes) {
            return self.fail(format!("writing the frame cache: {e}"));
        }
        v.frontier += 1;
        // A new frame makes the loop frames out of date.
        self.loop_pass = LoopState::Idle;
        v.pass2.clear();
        v.pass2_next = 0;
        v.end1 = None;
        if v.frontier >= v.frames.len() {
            v.end1 = Some((m.current().to_vec(), m.canvas.clone()));
        }
        let (next, bg0) = (v.frontier, v.bg0);
        if next < v.frames.len() {
            let target = match self.read_frame(next) {
                Ok(t) => t,
                Err(e) => return self.fail(e),
            };
            let (Some(m), Source::Video(v)) = (&mut self.model, &mut self.source) else { return };
            let start = (m.current().to_vec(), m.canvas.clone());
            m.set_target(target, bg0, Some(start));
            v.cur = next;
            self.publish_original();
        }
        self.dirty_status = true;
        self.dirty_pic = true;
    }

    /// "Loop video": start the second build at frame 0, on top of the
    /// last frame's picture of the first build.
    fn start_pass2(&mut self) {
        let target = match self.read_frame(0) {
            Ok(t) => t,
            Err(e) => return self.fail(e),
        };
        let (Some(m), Source::Video(v)) = (&mut self.model, &mut self.source) else { return };
        let start = v.end1.clone().unwrap_or_else(|| (m.current().to_vec(), m.canvas.clone()));
        v.pass2 = vec![FrameData::default(); v.frames.len()];
        v.pass2_next = 0;
        m.set_target(target, v.bg0, Some(start));
        self.build_time = Duration::ZERO;
        v.cur = 0;
        self.publish_original();
        self.dirty_status = true;
    }

    /// "Loop video": a frame of the second build is done. Keep it and start
    /// the next frame from its picture.
    fn finish_pass2(&mut self) {
        let (Some(m), Source::Video(v)) = (&mut self.model, &mut self.source) else { return };
        let i = v.pass2_next;
        v.pass2[i] = FrameData { shapes: m.shapes.clone(), colors: m.colors.clone(), scores: m.scores.clone(), contrast: m.contrast, time: self.build_time };
        self.build_time = Duration::ZERO;
        v.pass2_next += 1;
        let next = v.pass2_next;
        if next < v.frames.len() {
            let target = match self.read_frame(next) {
                Ok(t) => t,
                Err(e) => return self.fail(e),
            };
            let (Some(m), Source::Video(v)) = (&mut self.model, &mut self.source) else { return };
            let start = (m.current().to_vec(), m.canvas.clone());
            m.set_target(target, v.bg0, Some(start));
            v.cur = next;
            self.publish_original();
        }
        self.dirty_status = true;
    }

    /// Moves the playhead by the playback clock. Loops at the end. In
    /// "build on" mode the playhead waits at the first unfinished frame.
    fn advance_clock(&mut self) {
        let build_on = self.settings.build_on;
        let Source::Video(v) = &mut self.source else { return };
        let Some((t0, f0)) = v.clock else { return };
        let n = v.frames.len();
        let mut due = f0 + (t0.elapsed().as_secs_f64() * v.info.fps) as usize;
        if build_on && v.frontier < n && due > v.frontier {
            due = v.frontier;
            v.clock = Some((Instant::now(), due));
        } else if due >= n {
            due %= n;
            v.clock = Some((Instant::now(), due));
        }
        if due != v.view {
            v.view = due;
            self.dirty_pic = true;
            self.dirty_status = true;
            if !build_on {
                self.switch_to(due);
            }
        }
    }

    // ----------------------------------------------------------- the tick

    fn tick(&mut self) {
        if self.decode.is_some() {
            return self.poll_decode();
        }
        if self.render.is_some() {
            if self.building_paused() {
                return;
            }
            return self.render_tick();
        }
        if self.model.is_none() {
            return;
        }
        match &self.source {
            Source::Image(_) => {
                if self.playing {
                    if !self.full() {
                        self.step();
                    }
                    if self.full() {
                        self.playing = false;
                        self.dirty_status = true;
                    }
                }
            }
            Source::Video(_) if self.settings.build_on => {
                if self.playing {
                    self.advance_clock();
                }
                let Source::Video(v) = &self.source else { return };
                if v.frontier < v.frames.len() {
                    if !self.building_paused() {
                        self.build_frontier();
                    }
                } else if self.pass2_building() {
                    if !self.building_paused() {
                        self.build_frontier();
                    }
                } else if self.loop_pending() {
                    self.loop_step();
                }
            }
            Source::Video(_) => {
                if self.playing {
                    self.advance_clock();
                }
                if !self.full() {
                    self.step();
                }
            }
            Source::None => {}
        }
    }

    // ------------------------------------------------------------ render

    fn start_render(&mut self) {
        if self.render.is_some() || self.decode.is_some() {
            return;
        }
        let Some(v) = self.video() else { return };
        let fmt = self.settings.export_fmt.clone();
        let (sw, sh, scale) = model::output_size(v.w as i32, v.h as i32, self.settings.out_size);
        let (ow, oh) = if fmt.ends_with("-seq") { (sw.max(1), sh.max(1)) } else { ((sw & !1).max(2), (sh & !1).max(2)) };
        let (fps_text, bg0) = (v.info.fps_text.clone(), v.bg0);
        let naming = self.naming();
        if let Err(e) = std::fs::create_dir_all(&self.renders) {
            return self.fail(format!("could not make {}: {e}", self.renders.display()));
        }
        let sink = match fmt.as_str() {
            "png-seq" | "jpg-seq" | "svg-seq" => {
                let ext: &'static str = match fmt.as_str() {
                    "png-seq" => "png",
                    "jpg-seq" => "jpg",
                    _ => "svg",
                };
                let dir = export::unique(&self.renders, &naming.single(), "");
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return self.fail(format!("could not make {}: {e}", dir.display()));
                }
                Sink::Files { dir, fmt: ext, writer: FileWriter::new(ext, bg0), naming }
            }
            _ => {
                let container: &'static str = match fmt.as_str() {
                    "webm" => "webm",
                    "gif" => "gif",
                    _ => "mp4",
                };
                let final_path = export::unique(&self.renders, &naming.single(), container);
                let file = final_path.file_name().unwrap().to_string_lossy().to_string();
                let video_only = self.renders.join(format!(".{file}.video.{container}"));
                match video::start_encode(ow, oh, &fps_text, &video_only, container) {
                    Ok((child, stdin)) => Sink::Video { child, stdin, video_only, final_path, container },
                    Err(e) => return self.fail(e),
                }
            }
        };
        let svg = matches!(sink, Sink::Files { fmt: "svg", .. });
        let build_on = self.settings.build_on;
        let out = (build_on && !svg).then(|| model::blank_canvas(ow, oh, bg0));
        let acc = (build_on && svg).then(|| (Vec::new(), Vec::new()));
        self.playing = false;
        if let Source::Video(v) = &mut self.source {
            v.clock = None;
        }
        self.render_file = None;
        self.rt_due = None;
        self.render = Some(RenderJob { next: 0, lap: 0, sink, acc, out, ow, oh, scale, started: Instant::now(), first: 0 });
    }

    fn cancel_render(&mut self) {
        if let Some(r) = self.render.take() {
            match r.sink {
                Sink::Video { mut child, stdin, video_only, .. } => {
                    drop(stdin);
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&video_only);
                }
                Sink::Files { dir, writer, .. } => {
                    let _ = writer.finish();
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        }
    }

    /// True when the render needs a drawn picture for each frame (not SVG).
    fn render_wants_pixels(&self) -> bool {
        !matches!(self.render.as_ref().map(|r| &r.sink), Some(Sink::Files { fmt: "svg", .. }))
    }

    fn write_frame(&mut self, frame: Payload) -> Result<(), String> {
        let r = self.render.as_mut().unwrap();
        let i = r.next as u64;
        match (&mut r.sink, frame) {
            (Sink::Video { stdin, .. }, Payload::Pixels(pm)) => {
                stdin.write_all(pm.data()).map_err(|e| format!("ffmpeg stopped taking frames: {e}"))?;
            }
            (Sink::Files { dir, fmt, writer, naming }, p) => {
                writer.send(dir.join(format!("{}.{fmt}", naming.frame(i))), p)?;
            }
            _ => return Err("wrong frame type for this render".into()),
        }
        r.next += 1;
        self.dirty_status = true;
        Ok(())
    }

    fn render_tick(&mut self) {
        let build_on = self.settings.build_on;
        let Source::Video(v) = &self.source else { return self.cancel_render() };
        let n = v.frames.len();
        let i = self.render.as_ref().unwrap().next;
        // "Loop video": the render first draws laps without saving, so the
        // saved lap starts on the last frame's picture.
        let looping = self.looping();
        let need = self.render_laps().unwrap_or(u32::MAX);
        if i >= n {
            if looping {
                if need == u32::MAX {
                    if self.pass2_building() {
                        return self.build_frontier();
                    }
                    return self.loop_step();
                }
                let r = self.render.as_mut().unwrap();
                if r.lap < need {
                    r.lap += 1;
                    r.next = 0;
                    if r.lap == need {
                        r.started = Instant::now();
                    }
                    self.dirty_status = true;
                    return;
                }
            }
            return self.finish_render();
        }
        let lap = self.render.as_ref().unwrap().lap;
        let silent = looping && lap < need;
        let pixels = self.render_wants_pixels();
        let result = if build_on {
            if i < v.frontier {
                // "Loop video": the first lap is the first build, the
                // rest are the second build.
                let fd = if looping && lap > 0 { v.pass2[i].clone() } else { v.frames[i].clone() };
                let r = self.render.as_mut().unwrap();
                let frame = if pixels {
                    let out = r.out.as_mut().unwrap();
                    model::draw_shapes(out, &fd.shapes, &fd.colors, r.scale);
                    (!silent).then(|| Payload::Pixels(out.clone()))
                } else {
                    let (shapes, colors) = r.acc.as_mut().unwrap();
                    shapes.extend_from_slice(&fd.shapes);
                    colors.extend_from_slice(&fd.colors);
                    let bg0 = if let Source::Video(v) = &self.source { v.bg0 } else { [0; 4] };
                    (!silent).then(|| Payload::Svg(model::svg_of(r.ow, r.oh, r.scale, bg0, shapes, colors)))
                };
                let res = match frame {
                    Some(frame) => self.write_frame(frame),
                    None => {
                        r.next += 1;
                        self.dirty_status = true;
                        Ok(())
                    }
                };
                if let Source::Video(v) = &mut self.source {
                    v.view = i;
                }
                self.dirty_pic = true;
                self.rt_due = None;
                res
            } else {
                if let Source::Video(v) = &mut self.source {
                    v.view = v.frontier.min(n - 1);
                }
                self.build_frontier();
                Ok(())
            }
        } else {
            if v.cur != i {
                if let Source::Video(v) = &mut self.source {
                    v.view = i;
                }
                self.switch_to(i);
            }
            if !self.full() {
                self.step();
                Ok(())
            } else {
                let m = self.model.as_ref().unwrap();
                let r = self.render.as_ref().unwrap();
                let frame = if pixels {
                    let mut pm = model::blank_canvas(r.ow, r.oh, m.bg);
                    model::draw_shapes(&mut pm, &m.shapes, &m.colors, r.scale);
                    Payload::Pixels(pm)
                } else {
                    Payload::Svg(model::svg_of(r.ow, r.oh, r.scale, m.bg, &m.shapes, &m.colors))
                };
                self.write_frame(frame)
            }
        };
        if let Err(e) = result {
            let mut msg = e;
            if let Some(r) = self.render.take() {
                match r.sink {
                    Sink::Video { child, stdin, video_only, .. } => {
                        drop(stdin);
                        if let Err(e2) = video::finish_encode(child) {
                            msg = e2;
                        }
                        let _ = std::fs::remove_file(&video_only);
                    }
                    Sink::Files { dir, writer, .. } => {
                        let _ = writer.finish();
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                }
            }
            self.fail(msg);
        }
    }

    fn finish_render(&mut self) {
        let r = self.render.take().unwrap();
        self.dirty_status = true;
        let (child, stdin, video_only, final_path, container) = match r.sink {
            Sink::Video { child, stdin, video_only, final_path, container } => (child, stdin, video_only, final_path, container),
            Sink::Files { dir, writer, .. } => {
                if let Err(e) = writer.finish() {
                    return self.fail(e);
                }
                self.render_file = dir.file_name().map(|n| n.to_string_lossy().to_string());
                self.render_folder = true;
                eprintln!("rendered {}", dir.display());
                return;
            }
        };
        drop(stdin);
        if let Err(e) = video::finish_encode(child) {
            let _ = std::fs::remove_file(&video_only);
            return self.fail(e);
        }
        let v = self.video().unwrap();
        let res = if v.info.has_audio && container != "gif" {
            let res = video::add_sound(&video_only, &v.path, &final_path, container);
            let _ = std::fs::remove_file(&video_only);
            res
        } else {
            std::fs::rename(&video_only, &final_path).map_err(|e| e.to_string())
        };
        match res {
            Ok(()) => {
                self.render_file = final_path.file_name().map(|n| n.to_string_lossy().to_string());
                self.render_folder = false;
                eprintln!("rendered {}", final_path.display());
            }
            Err(e) => self.fail(e),
        }
    }

    // ------------------------------------------------------------ export

    fn export_data(&self) -> Result<ExportData, String> {
        let Some(m) = &self.model else {
            return Err("Load a picture or a video first.".into());
        };
        // A video: the frame on screen.
        let (stem, shapes, colors, bg) = match &self.source {
            Source::None => return Err("Load a picture or a video first.".into()),
            Source::Image(_) => (self.naming().single(), m.shapes.clone(), m.colors.clone(), m.bg),
            Source::Video(v) => {
                let i = v.view.min(v.frames.len().saturating_sub(1));
                let stem = self.naming().frame(i as u64);
                if self.settings.build_on {
                    // The picture is every shape of every frame up to this one.
                    let done = v.frontier.min(v.frames.len());
                    let mut parts: Vec<&FrameData> = Vec::new();
                    let mut live = false;
                    match self.loop_pass {
                        // "Loop video": the same laps as a render draws.
                        LoopState::Ready { laps } if self.settings.loop_video && v.pass2.len() == v.frames.len() => {
                            parts.extend(v.frames.iter());
                            for _ in 0..laps {
                                parts.extend(v.pass2.iter());
                            }
                            parts.extend(v.pass2[..=i].iter());
                        }
                        _ => {
                            parts.extend(v.frames[..done.min(i + 1)].iter());
                            live = i >= done;
                        }
                    }
                    let mut shapes = Vec::new();
                    let mut colors = Vec::new();
                    for f in parts {
                        shapes.extend_from_slice(&f.shapes);
                        colors.extend_from_slice(&f.colors);
                    }
                    if live {
                        shapes.extend_from_slice(&m.shapes);
                        colors.extend_from_slice(&m.colors);
                    }
                    (stem, shapes, colors, v.bg0)
                } else if i == v.cur {
                    (stem, m.shapes.clone(), m.colors.clone(), m.bg)
                } else {
                    let f = &v.frames[i];
                    let bg = bg_from(&self.applied.1, &self.read_frame(i)?);
                    (stem, f.shapes.clone(), f.colors.clone(), bg)
                }
            }
        };
        Ok(ExportData {
            stem,
            shapes,
            colors,
            bg,
            w: m.w,
            h: m.h,
            out_size: self.settings.out_size,
        })
    }

    // ----------------------------------------------------------- publish

    fn publish_original(&mut self) {
        if let Some(m) = &self.model {
            let mut img = image::RgbaImage::from_raw(m.w as u32, m.h as u32, m.target().to_vec()).unwrap();
            for p in img.pixels_mut() {
                let a = p.0[3] as u32;
                if a > 0 && a < 255 {
                    for c in 0..3 {
                        p.0[c] = (p.0[c] as u32 * 255 / a).min(255) as u8;
                    }
                }
            }
            self.original_tx.send_replace(Arc::new(jpeg(&img, 90)));
            self.original_version += 1;
            self.dirty_status = true;
        }
    }

    /// Shape count, score and contrast of the frame on screen. In video
    /// mode the playhead can sit on a stored frame that is not in the model.
    fn shown_stats(&self) -> (usize, Option<f64>, f64, Duration) {
        let live = (self.shape_count(), self.model.as_ref().map(|m| m.score), self.model.as_ref().map_or(0.0, |m| m.contrast), self.build_time);
        let Some(v) = self.video() else { return live };
        let in_model = if self.settings.build_on { v.view == v.frontier } else { v.view == v.cur };
        match v.frames.get(v.view) {
            Some(f) if !in_model && !f.shapes.is_empty() => (f.shapes.len(), f.scores.last().copied(), f.contrast, f.time),
            Some(_) if !in_model => (0, None, 0.0, Duration::ZERO),
            _ => live,
        }
    }

    fn timeline(&self) -> String {
        let Some(v) = self.video() else { return String::new() };
        let build_on = self.settings.build_on;
        v.frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let live = (!build_on && i == v.cur) || (build_on && i == v.frontier);
                let (n, full) = if live { (self.shape_count(), self.full()) } else { (f.shapes.len(), self.settings.done(f.shapes.len(), f.scores.last().copied(), f.contrast)) };
                if build_on && i < v.frontier || full {
                    '2'
                } else if n > 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect()
    }

    fn status(&self) -> String {
        let m = self.model.as_ref();
        let (kind, name) = match &self.source {
            Source::None => ("none", String::new()),
            Source::Image(i) => ("image", i.name.clone()),
            Source::Video(v) => ("video", v.name.clone()),
        };
        let loading = self.decode.as_ref().map(|d| {
            let what = if d.files.is_some() { "Reading pictures of" } else { "Loading video" };
            json!({ "what": format!("{what} {}", d.name), "progress": self.decode_progress() })
        });
        let render = self.render.as_ref().map(|r| {
            let n = self.video().map_or(1, |v| v.frames.len());
            let done = r.next - r.first;
            let now = self.build_paused.filter(|_| self.building_paused()).unwrap_or_else(Instant::now);
            let el = now.saturating_duration_since(r.started).as_secs_f64();
            let eta = if done > 0 { Some(el / done as f64 * (n - r.next) as f64) } else { None };
            // "Loop video": the laps before the saved one.
            let prep = self.looping() && self.render_laps().is_none_or(|laps| r.lap < laps);
            json!({ "frame": r.next, "frames": n, "eta": if prep { None } else { eta }, "prep": prep })
        });
        let video = self.video().map(|v| {
            json!({
                "frames": v.frames.len(),
                "fps": v.info.fps,
                "view": v.view,
                "frontier": v.frontier,
                "timeline": self.timeline(),
                "sequence": v.files.is_some(),
                "loop_prep": self.loop_pending(),
                "pass2_next": v.pass2_next,
            })
        });
        let (ow, oh) = m.map_or((0, 0), |m| {
            let (w, h, _) = model::output_size(m.w, m.h, self.settings.out_size);
            if kind == "video" && !self.settings.export_fmt.ends_with("-seq") {
                ((w & !1).max(2), (h & !1).max(2))
            } else {
                (w, h)
            }
        });
        let shown = self.shown_stats();
        json!({
            "settings": self.settings,
            "needs_restart": m.is_some() && self.needs_restart(),
            "seed": m.map(|_| self.used_seed.to_string()),
            "kind": kind,
            "name": name,
            "ready": m.is_some(),
            "work": m.map(|m| [m.w, m.h]),
            "out": [ow, oh],
            "playing": self.playing,
            "build_paused": self.building_paused(),
            "shapes": shown.0,
            "score": shown.1,
            // Seconds spent adding this picture's shapes.
            "time": shown.3.as_secs_f64(),
            // Share of the picture's contrast that is matched, in percent.
            "matched": shown.1.filter(|_| shown.2 > 0.0).map(|sc| ((1.0 - sc / shown.2) * 100.0).clamp(0.0, 100.0)),
            // The score "Stop at match" stops this picture at, and whether
            // that is the fixed floor and not the percentage.
            "stop_score": self.settings.stop_score(shown.2),
            "stop_floor": self.settings.stop_floor > 0.0 && self.settings.stop_score(shown.2).is_some_and(|t| t <= self.settings.stop_floor),
            "gpu": m.map(|m| m.gpu_name().to_string()),
            "has_work": self.has_work(),
            "loading": loading,
            "render": render,
            "render_file": self.render_file,
            "render_folder": self.render_folder,
            "renders_dir": self.renders.display().to_string(),
            "name_example": self.name_example(),
            "error": self.error,
            "original": self.original_version,
            "video": video,
        })
        .to_string()
    }

    fn publish(&mut self, force: bool) {
        if !force && self.last_publish.elapsed() < PUBLISH_EVERY {
            return;
        }
        if !(self.dirty_pic || self.dirty_status || force) {
            return;
        }
        self.last_publish = Instant::now();
        if self.dirty_status || force {
            self.status_tx.send_replace(self.status());
            self.dirty_status = false;
        }
        if self.dirty_pic {
            self.dirty_pic = false;
            self.publish_picture();
        }
    }

    fn publish_picture(&mut self) {
        let Some(m) = &self.model else { return };
        if let (true, Source::Video(v)) = (self.settings.build_on, &self.source) {
            if v.view < v.frontier {
                if self.shown_cached == Some(v.view) {
                    return;
                }
                let looped = self.settings.loop_video && matches!(self.loop_pass, LoopState::Ready { .. });
                let file = if looped { format!("loop-{}.jpg", v.view) } else { format!("{}.jpg", v.view) };
                let path = self.tmp.join("built").join(file);
                if let Ok(b) = std::fs::read(path) {
                    self.shown_cached = Some(v.view);
                    self.pics.put(Pic::Jpeg(b));
                    return;
                }
            }
        }
        self.shown_cached = None;
        self.pics.put(Pic::Raw(m.canvas.clone()));
    }
}
