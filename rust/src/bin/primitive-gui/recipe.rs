//! The recipe: everything needed to make a picture again, as one JSON
//! value that can be shared. It holds the settings, the keyframes and the
//! painted areas (as small gray PNGs). It holds no file names or folder
//! paths. `Worker::recipe` writes it and `Worker::apply_recipe` reads it.
//!
//! Layout (version 1):
//! ```json
//! { "app": "primitive-gui", "recipe": 1,
//!   "settings": { ... },          // values before the first keyframe
//!   "area": "<base64 PNG>",       // painted area at the start, or null
//!   "keys": { "alpha": { "smooth": true, "keys": [[0, 128], [40, 255]] },
//!             "mask":  { "smooth": true, "keys": [[20, "<base64 PNG>"], [30, null]] } } }
//! ```
//! A file with no "recipe" number is an older settings file: only its
//! settings are used.

use base64::Engine;
use std::io::Cursor;

/// The newest recipe version this program writes. Files with a higher
/// number still load: the parts this version knows are used.
pub const VERSION: u64 = 1;

/// Long side of a painted area, the same as `MASK_LONG` in `app.js`.
const AREA_LONG: usize = 128;
/// Largest painted area a recipe may hold, the same limit as the page.
const AREA_MAX: u32 = 1024;

/// A painted area: width, height, gray values (0 or 255).
pub type Area = (usize, usize, Vec<u8>);

/// A painted area as a base64 gray PNG.
pub fn area_to_text(a: &Area) -> Option<String> {
    let (w, h, gray) = a;
    let img = image::GrayImage::from_raw(*w as u32, *h as u32, gray.clone())?;
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(png))
}

/// Reads a painted area written by `area_to_text`. None for null, bad
/// data, an empty area, or an area that is too large.
pub fn area_from_text(v: &serde_json::Value) -> Option<Area> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(v.as_str()?).ok()?;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(AREA_MAX);
    limits.max_image_height = Some(AREA_MAX);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_luma8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let gray: Vec<u8> = img.into_raw().into_iter().map(|g| if g >= 128 { 255 } else { 0 }).collect();
    (w > 0 && h > 0 && gray.contains(&255)).then_some((w, h, gray))
}

/// Stretches a painted area to the proportions of a `pw` x `ph` picture,
/// so a recipe made on a picture of another shape still covers the same
/// part of the picture. An area that already fits is kept as it is.
pub fn fit_area(a: Area, pw: u32, ph: u32) -> Area {
    let (w, h, gray) = &a;
    if pw == 0 || ph == 0 {
        return a;
    }
    let want = pw as f64 / ph as f64;
    // The page drops an area that is more than 0.03 off the picture's
    // proportions (`placeOverlay` in `app.js`), so stay well inside that.
    if ((*w as f64 / *h as f64) - want).abs() <= 0.01 {
        return a;
    }
    let (nw, nh) = if want >= 1.0 {
        (AREA_LONG, ((AREA_LONG as f64 / want).round() as usize).max(1))
    } else {
        (((AREA_LONG as f64 * want).round() as usize).max(1), AREA_LONG)
    };
    let out = (0..nw * nh).map(|i| gray[(i / nw * h / nh) * w + (i % nw) * w / nw]).collect();
    (nw, nh, out)
}

/// The keyword of the PNG text chunk and the id of the SVG element that
/// hold a recipe. `app.js` looks for the same names.
pub const TAG: &str = "primitive-recipe";

/// CRC-32 as PNG chunks use it.
fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Puts the recipe into a PNG file as an uncompressed iTXt chunk, just
/// after the header chunk. A file that does not look like a PNG is kept
/// as it is.
pub fn into_png(png: Vec<u8>, recipe: &serde_json::Value) -> Vec<u8> {
    // 8 bytes signature, then IHDR: 4 length + 4 type + 13 data + 4 CRC.
    const AFTER_IHDR: usize = 8 + 25;
    if png.len() < AFTER_IHDR || &png[12..16] != b"IHDR" {
        return png;
    }
    // iTXt data: keyword, 0, no compression (0, 0), no language, 0,
    // no translated keyword, 0, then the UTF-8 text.
    let mut chunk = b"iTXt".to_vec();
    chunk.extend_from_slice(TAG.as_bytes());
    chunk.extend_from_slice(&[0, 0, 0, 0, 0]);
    chunk.extend_from_slice(recipe.to_string().as_bytes());
    let mut out = Vec::with_capacity(png.len() + chunk.len() + 8);
    out.extend_from_slice(&png[..AFTER_IHDR]);
    out.extend_from_slice(&((chunk.len() - 4) as u32).to_be_bytes());
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&crc32(&chunk).to_be_bytes());
    out.extend_from_slice(&png[AFTER_IHDR..]);
    out
}

/// Puts the recipe into an SVG file as a `<metadata>` element just inside
/// the `<svg>` element.
pub fn into_svg(svg: String, recipe: &serde_json::Value) -> String {
    let Some(end) = svg.find('>') else { return svg };
    let text = recipe.to_string().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("{}\n<metadata id=\"{TAG}\">{text}</metadata>{}", &svg[..=end], &svg[end + 1..])
}
