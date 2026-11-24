//! The `.primitive` save file: the user's own work, to go on with later.
//! It is a zip file (stored, not compressed) that holds:
//!
//! - `save.json`: the recipe (see `recipe.rs`), where the source file is on
//!   this computer, and the shapes made so far on a still picture.
//! - `source.<ext>` (only when the user ticks "Include original picture",
//!   or when the picture was uploaded and its real place is not known):
//!   a copy of the still picture. Videos are never copied in.
//!
//! Layout of `save.json` (version 1):
//! ```json
//! { "app": "primitive-gui", "save": 1,
//!   "source": { "kind": "image", "name": "cat.png", "path": "/home/.../cat.png", "included": "source.png" },
//!   "recipe": { ... },
//!   "shapes": { "w": 256, "h": 192, "bg": [..], "shapes": [..], "colors": [..], "scores": [..], "head": null } }
//! ```
//! Unlike a recipe, a save file holds a folder path, so it is for the
//! user's own use, not for sharing.

use crate::worker::{SaveData, SavedShapes, STILL_EXT};
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The newest save version this program writes.
pub const VERSION: u64 = 1;
const JSON_NAME: &str = "save.json";
/// Largest `save.json` this program reads.
const JSON_MAX: u64 = 512 << 20;
/// Largest included picture this program reads.
const SOURCE_MAX: u64 = 2 << 30;

/// Writes the save file `out`. `include` puts a copy of a still picture in
/// it. A picture whose real place is not known is always put in.
pub fn write(d: &SaveData, include: bool, out: &Path) -> Result<(), String> {
    let included = match (&d.work_file, d.kind) {
        (Some(f), "image") if include || d.origin.is_none() => {
            let ext = Path::new(&d.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let ext = if STILL_EXT.contains(&ext.as_str()) { ext } else { "png".into() };
            Some((f.clone(), format!("source.{ext}")))
        }
        _ => None,
    };
    let meta = json!({
        "app": "primitive-gui",
        "save": VERSION,
        "source": {
            "kind": d.kind,
            "name": d.name,
            "path": d.origin,
            "included": included.as_ref().map(|(_, n)| n),
        },
        "recipe": d.recipe,
        "shapes": d.shapes,
    });
    let part = out.with_extension("part");
    let res = (|| -> Result<(), String> {
        let f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
        z.start_file(JSON_NAME, opts).map_err(|e| e.to_string())?;
        z.write_all(meta.to_string().as_bytes()).map_err(|e| e.to_string())?;
        if let Some((file, name)) = &included {
            z.start_file(name.as_str(), opts).map_err(|e| e.to_string())?;
            let mut src = std::fs::File::open(file).map_err(|e| format!("could not read the picture: {e}"))?;
            std::io::copy(&mut src, &mut z).map_err(|e| e.to_string())?;
        }
        z.finish().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())?;
        std::fs::rename(&part, out).map_err(|e| e.to_string())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    res
}

/// A save file as read back.
pub struct SaveFile {
    pub kind: String,
    pub name: String,
    /// Where the source was when the file was saved.
    pub path: Option<PathBuf>,
    /// The name of the included picture inside the zip, if there is one.
    pub included: Option<String>,
    pub recipe: serde_json::Value,
    pub shapes: Option<SavedShapes>,
}

const NOT_A_SAVE: &str = "This is not a save file of this program, or it is damaged.";

/// Reads `save.json` from the save file `zip`.
pub fn read(zip: &Path) -> Result<SaveFile, String> {
    let f = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|_| NOT_A_SAVE.to_string())?;
    let mut entry = z.by_name(JSON_NAME).map_err(|_| NOT_A_SAVE.to_string())?;
    if entry.size() > JSON_MAX {
        return Err(NOT_A_SAVE.into());
    }
    let mut text = String::new();
    (&mut entry).take(JSON_MAX).read_to_string(&mut text).map_err(|_| NOT_A_SAVE.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| NOT_A_SAVE.to_string())?;
    if v["app"] != "primitive-gui" || v["save"].as_u64().is_none() || !v["recipe"].is_object() {
        return Err(NOT_A_SAVE.into());
    }
    let src = &v["source"];
    // A plain file name only: it becomes part of a temp file name.
    let included = src["included"].as_str().filter(|n| {
        let ext = Path::new(n).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        n.starts_with("source.") && !n.contains(['/', '\\']) && STILL_EXT.contains(&ext.as_str())
    });
    Ok(SaveFile {
        kind: src["kind"].as_str().unwrap_or("none").to_string(),
        name: src["name"].as_str().map(|n| Path::new(n).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_default(),
        path: src["path"].as_str().map(PathBuf::from),
        included: included.map(str::to_string),
        recipe: v["recipe"].clone(),
        shapes: serde_json::from_value(v["shapes"].clone()).ok(),
    })
}

/// Copies the included picture `name` out of the save file `zip` into `out`.
pub fn extract(zip: &Path, name: &str, out: &Path) -> Result<(), String> {
    let f = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|_| NOT_A_SAVE.to_string())?;
    let mut entry = z.by_name(name).map_err(|_| NOT_A_SAVE.to_string())?;
    if entry.size() > SOURCE_MAX {
        return Err("The picture in this save file is too large.".into());
    }
    let mut o = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let r = std::io::copy(&mut (&mut entry).take(SOURCE_MAX), &mut o);
    if r.is_err() {
        let _ = std::fs::remove_file(out);
    }
    r.map(|_| ()).map_err(|_| NOT_A_SAVE.to_string())
}
