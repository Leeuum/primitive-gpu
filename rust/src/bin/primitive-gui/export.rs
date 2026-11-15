//! Exports: one picture (PNG, JPG, SVG), a timelapse video (MP4, WebM, GIF),
//! file names from the user's pattern, the background writer for image
//! sequences, and zipping a finished sequence folder.

use crate::video;
use crate::worker::{jpeg, ExportData};
use primitive_gpu::load;
use primitive_gpu::model;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Every export format. The `-seq` ones are only for videos and sequences.
pub const IMAGE_FORMATS: &[&str] = &["png", "jpg", "svg", "mp4", "webm", "gif"];
pub const VIDEO_FORMATS: &[&str] = &["mp4", "webm", "gif", "png-seq", "jpg-seq", "svg-seq"];

pub fn mode_name(mode: i32) -> &'static str {
    match mode {
        0 => "combo",
        1 => "triangle",
        2 => "rect",
        3 => "ellipse",
        4 => "circle",
        5 => "rotatedrect",
        6 => "curve",
        7 => "rotatedellipse",
        8 => "polygon",
        _ => "shapes",
    }
}

fn date_text(fmt: &str) -> String {
    chrono::Local::now().format(fmt).to_string()
}

/// Turns the user's name pattern into file names. Tags: {name} {frame}
/// {shapes} {mode} {date} {time}.
#[derive(Clone)]
pub struct Naming {
    pub pattern: String,
    pub digits: u32,
    pub start: u64,
    pub name: String,
    pub shapes: usize,
    pub mode: i32,
    pub date: String,
    pub time: String,
}

impl Naming {
    pub fn new(pattern: &str, digits: u32, start: u64, source_name: &str, shapes: usize, mode: i32) -> Naming {
        let stem = Path::new(source_name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        Naming {
            pattern: if pattern.trim().is_empty() { "{name}".into() } else { pattern.to_string() },
            digits,
            start,
            name: if stem.is_empty() { "picture".into() } else { stem },
            shapes,
            mode,
            date: date_text("%Y-%m-%d"),
            time: date_text("%H-%M-%S"),
        }
    }

    fn fill(&self, pattern: &str, frame: Option<u64>) -> String {
        let mut p = pattern.to_string();
        match frame {
            Some(f) => p = p.replace("{frame}", &format!("{:0w$}", self.start + f, w = self.digits as usize)),
            // No frame number: drop the tag and one separator in front of it.
            None => {
                while let Some(i) = p.find("{frame}") {
                    let mut a = i;
                    if let Some(c) = p[..i].chars().last() {
                        if "_-. ".contains(c) {
                            a = i - c.len_utf8();
                        }
                    }
                    p.replace_range(a..i + 7, "");
                }
            }
        }
        let p = p
            .replace("{name}", &self.name)
            .replace("{shapes}", &self.shapes.to_string())
            .replace("{mode}", mode_name(self.mode))
            .replace("{date}", &self.date)
            .replace("{time}", &self.time);
        let p: String = p.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
        let p = p.trim().trim_start_matches('.').trim().to_string();
        if p.is_empty() {
            "picture".into()
        } else {
            p
        }
    }

    /// Name of a single file, without the extension.
    pub fn single(&self) -> String {
        self.fill(&self.pattern, None)
    }

    /// Name of frame `f` (counted from 0) of a sequence, without the
    /// extension. A pattern without {frame} gets "_{frame}" added, so the
    /// files do not overwrite each other.
    pub fn frame(&self, f: u64) -> String {
        if self.pattern.contains("{frame}") {
            self.fill(&self.pattern, Some(f))
        } else {
            self.fill(&format!("{}_{{frame}}", self.pattern), Some(f))
        }
    }
}

/// `dir/stem.ext`, or `dir/stem-2.ext` and so on when that already exists.
/// An empty `ext` makes a folder name.
pub fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let make = |s: &str| if ext.is_empty() { dir.join(s) } else { dir.join(format!("{s}.{ext}")) };
    let mut p = make(stem);
    let mut n = 2;
    while p.exists() {
        p = make(&format!("{stem}-{n}"));
        n += 1;
    }
    p
}

/// JPG has no transparency: put the picture onto the background color.
pub fn flatten(img: &mut image::RgbaImage, bg: [u8; 4]) {
    for p in img.pixels_mut() {
        let a = p.0[3] as u32;
        for c in 0..3 {
            p.0[c] = ((p.0[c] as u32 * a + bg[c] as u32 * (255 - a)) / 255) as u8;
        }
        p.0[3] = 255;
    }
}

fn png_bytes(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

/// One picture or one timelapse. Returns the file, its type and its
/// download name.
pub fn export(d: &ExportData, fmt: &str, tmp: &Path) -> Result<(Vec<u8>, &'static str, String), String> {
    let name = |ext: &str| format!("{}.{ext}", d.stem);
    let (sw, sh, scale) = model::output_size(d.w, d.h, d.out_size);
    let (sw, sh) = (sw.max(1), sh.max(1));
    let full = || {
        let mut pm = model::blank_canvas(sw, sh, d.bg);
        model::draw_shapes(&mut pm, &d.shapes, &d.colors, scale);
        pm
    };
    match fmt {
        "png" => Ok((png_bytes(&load::pixmap_to_rgba(&full()))?, "image/png", name("png"))),
        "jpg" => {
            let mut img = load::pixmap_to_rgba(&full());
            flatten(&mut img, d.bg);
            Ok((jpeg(&img, 95), "image/jpeg", name("jpg")))
        }
        "svg" => Ok((model::svg_of(sw, sh, scale, d.bg, &d.shapes, &d.colors).into_bytes(), "image/svg+xml", name("svg"))),
        "mp4" | "webm" | "gif" => {
            let mime = match fmt {
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                _ => "image/gif",
            };
            timelapse(d, tmp, fmt).map(|b| (b, mime, name(fmt)))
        }
        _ => Err(format!("unknown export format: {fmt}")),
    }
}

/// One shape per frame at 30 fps (several when there are many, so it stays
/// under about 30 seconds), then the last picture for 2 seconds.
fn timelapse(d: &ExportData, tmp: &Path, container: &str) -> Result<Vec<u8>, String> {
    let (sw, sh, scale) = model::output_size(d.w, d.h, d.out_size);
    let (w, h) = ((sw & !1).max(2), (sh & !1).max(2));
    let path = tmp.join(format!("timelapse-{}.{container}", std::process::id()));
    let (child, mut stdin) = video::start_encode(w, h, "30", &path, container)?;
    let per = d.shapes.len().div_ceil(28 * 30).max(1);
    let mut pm = model::blank_canvas(w, h, d.bg);
    // Flatten transparency the same way for every frame: the videos have none.
    let opaque = |pm: &tiny_skia::Pixmap| -> Vec<u8> {
        let mut out = pm.data().to_vec();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in 0..3 {
                p[c] = (p[c] as u32 + d.bg[c] as u32 * (255 - a) / 255).min(255) as u8;
            }
            p[3] = 255;
        }
        out
    };
    let mut write = |pm: &tiny_skia::Pixmap| stdin.write_all(&opaque(pm)).map_err(|e| format!("ffmpeg: {e}"));
    let mut res = write(&pm);
    for (s, c) in d.shapes.chunks(per).zip(d.colors.chunks(per)) {
        if res.is_err() {
            break;
        }
        model::draw_shapes(&mut pm, s, c, scale);
        res = write(&pm);
    }
    for _ in 0..60 {
        if res.is_err() {
            break;
        }
        res = write(&pm);
    }
    drop(write);
    drop(stdin);
    let fin = video::finish_encode(child);
    res.and(fin)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    bytes
}

/// One frame of an image sequence, waiting to be encoded and saved.
pub enum Payload {
    Pixels(tiny_skia::Pixmap),
    Svg(String),
}

/// Encodes and saves sequence frames on background threads, so the search
/// does not wait for PNG encoding. At most a few frames wait at a time.
pub struct FileWriter {
    tx: Option<SyncSender<(PathBuf, Payload)>>,
    threads: Vec<JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}

impl FileWriter {
    /// `fmt` is "png", "jpg" or "svg". `bg` is used to flatten JPGs.
    pub fn new(fmt: &'static str, bg: [u8; 4]) -> FileWriter {
        let (tx, rx) = sync_channel::<(PathBuf, Payload)>(6);
        let rx = Arc::new(Mutex::new(rx));
        let error = Arc::new(Mutex::new(None));
        let threads = (0..3)
            .map(|_| {
                let (rx, error): (Arc<Mutex<Receiver<(PathBuf, Payload)>>>, _) = (rx.clone(), error.clone());
                std::thread::spawn(move || loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((path, payload)) = job else { return };
                    let bytes = match payload {
                        Payload::Svg(s) => Ok(s.into_bytes()),
                        Payload::Pixels(pm) => {
                            let mut img = load::pixmap_to_rgba(&pm);
                            if fmt == "jpg" {
                                flatten(&mut img, bg);
                                Ok(jpeg(&img, 95))
                            } else {
                                png_bytes(&img)
                            }
                        }
                    };
                    let res = bytes.and_then(|b| std::fs::write(&path, b).map_err(|e| format!("writing {}: {e}", path.display())));
                    if let Err(e) = res {
                        error.lock().unwrap().get_or_insert(e);
                    }
                })
            })
            .collect();
        FileWriter { tx: Some(tx), threads, error }
    }

    pub fn send(&self, path: PathBuf, p: Payload) -> Result<(), String> {
        if let Some(e) = self.error.lock().unwrap().clone() {
            return Err(e);
        }
        self.tx.as_ref().unwrap().send((path, p)).map_err(|_| "the file writer stopped".to_string())
    }

    /// Waits until every frame is saved.
    pub fn finish(mut self) -> Result<(), String> {
        self.tx = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.error.lock().unwrap().take().map_or(Ok(()), Err)
    }
}

/// Packs the folder `dir` into the zip file `out`. Pictures are already
/// compressed, so the files are stored as they are.
pub fn zip_folder(dir: &Path, out: &Path) -> Result<(), String> {
    let folder = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let part = out.with_extension("part");
    let f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    for p in names {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        z.start_file(format!("{folder}/{name}"), opts).map_err(|e| e.to_string())?;
        let mut src = std::fs::File::open(&p).map_err(|e| e.to_string())?;
        std::io::copy(&mut src, &mut z).map_err(|e| e.to_string())?;
    }
    z.finish().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())?;
    std::fs::rename(&part, out).map_err(|e| e.to_string())
}
