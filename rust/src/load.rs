//! Picture loading shared by both programs: open, shrink, premultiply,
//! average color, and turning a canvas back into a plain image.

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};
use std::process::Command;
use tiny_skia::Pixmap;

/// Opens a picture and turns it upright from its rotation tag (phone
/// photos). Formats the `image` crate cannot read (HEIC, for example) go
/// through ffmpeg.
pub fn open_image(path: &str) -> Result<DynamicImage, String> {
    match open_with_image_crate(path) {
        Ok(img) => Ok(img),
        Err(first) => open_with_ffmpeg(path).map_err(|_| format!("{path}: {first}")),
    }
}

fn open_with_image_crate(path: &str) -> Result<DynamicImage, String> {
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

fn open_with_ffmpeg(path: &str) -> Result<DynamicImage, String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i", path, "-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    image::load_from_memory(&out.stdout).map_err(|e| e.to_string())
}

/// Shrinks the picture so neither side is larger than `size` (0 = keep).
pub fn shrink(img: DynamicImage, size: u32) -> DynamicImage {
    if size > 0 && (img.width() > size || img.height() > size) {
        img.resize(size, size, FilterType::Triangle)
    } else {
        img
    }
}

/// The working picture: tightly packed premultiplied RGBA8, with its size.
pub fn premultiplied(img: &DynamicImage) -> (Vec<u8>, i32, i32) {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as i32, rgba.height() as i32);
    let mut target = rgba.into_raw();
    premultiply(&mut target);
    (target, w, h)
}

/// Premultiplies tightly packed RGBA8 in place.
pub fn premultiply(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = (*c as u32 * a / 255) as u8;
            }
        }
    }
}

/// The average color of a premultiplied picture, as an opaque color.
pub fn average_color(target: &[u8]) -> [u8; 4] {
    let n = (target.len() / 4).max(1) as u64;
    let mut sum = [0u64; 3];
    for px in target.chunks_exact(4) {
        for c in 0..3 {
            sum[c] += px[c] as u64;
        }
    }
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]
}

/// Parses "#rgb", "#rgba", "#rrggbb" or "#rrggbbaa". Anything else is black.
pub fn parse_hex(x: &str) -> [u8; 4] {
    let x = x.trim().trim_matches('#');
    let hex = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0);
    let d = |c: &str| {
        let v = hex(c);
        (v << 4) | v
    };
    if !x.is_ascii() {
        return [0, 0, 0, 255];
    }
    match x.len() {
        3 => [d(&x[0..1]), d(&x[1..2]), d(&x[2..3]), 255],
        4 => [d(&x[0..1]), d(&x[1..2]), d(&x[2..3]), d(&x[3..4])],
        6 => [hex(&x[0..2]), hex(&x[2..4]), hex(&x[4..6]), 255],
        8 => [hex(&x[0..2]), hex(&x[2..4]), hex(&x[4..6]), hex(&x[6..8])],
        _ => [0, 0, 0, 255],
    }
}

/// A canvas as a plain (not premultiplied) RGBA image.
pub fn pixmap_to_rgba(pm: &Pixmap) -> RgbaImage {
    let mut out = RgbaImage::new(pm.width(), pm.height());
    for (px, dst) in pm.pixels().iter().zip(out.pixels_mut()) {
        let c = px.demultiply();
        dst.0 = [c.red(), c.green(), c.blue(), c.alpha()];
    }
    out
}
