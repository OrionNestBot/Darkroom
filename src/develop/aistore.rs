//! Store for AI-generated bitmaps (masks). They are kept as PNG in the catalog's `ai_masks` table;
//! render/export/preview threads fetch them by key (opening the catalog read-only to load them when not cached).
//! © 2026 OrionNest

use super::mask::BrushRaster;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

static CACHE: Mutex<Option<HashMap<String, Arc<BrushRaster>>>> = Mutex::new(None);

fn to_raster(g: &crate::imaging::aimask::Gray) -> BrushRaster {
    BrushRaster { w: g.w, h: g.h, data: g.data.iter().map(|v| *v as f32 / 255.0).collect() }
}

/// Caches a freshly made mask so it can be used right away
pub fn put(key: &str, g: &crate::imaging::aimask::Gray) {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let m = c.get_or_insert_with(HashMap::new);
    if m.len() > 64 {
        m.clear();
    }
    m.insert(key.to_string(), Arc::new(to_raster(g)));
}

/// Fetches a mask by key (cache, then catalog)
pub fn get(key: &str) -> Option<Arc<BrushRaster>> {
    if let Some(r) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(key).cloned()) {
        return Some(r);
    }
    let conn = rusqlite::Connection::open_with_flags(crate::config::catalog_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let png: Vec<u8> = conn.query_row("SELECT png FROM ai_masks WHERE key=?1", [key], |r| r.get(0)).ok()?;
    let g = crate::imaging::aimask::Gray::from_png(&png)?;
    put(key, &g);
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(key).cloned())
}

/// Erase patch (linear-light RGB)
pub struct RgbPatch {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

static RGB: Mutex<Option<HashMap<String, Arc<RgbPatch>>>> = Mutex::new(None);

pub fn put_rgb(key: &str, p: RgbPatch) {
    let mut c = RGB.lock().unwrap_or_else(|e| e.into_inner());
    let m = c.get_or_insert_with(HashMap::new);
    if m.len() > 64 {
        m.clear();
    }
    m.insert(key.to_string(), Arc::new(p));
}

pub fn get_rgb(key: &str, scale: f32) -> Option<Arc<RgbPatch>> {
    if let Some(r) = RGB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(key).cloned()) {
        return Some(r);
    }
    let conn = rusqlite::Connection::open_with_flags(crate::config::catalog_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let png: Vec<u8> = conn.query_row("SELECT png FROM ai_masks WHERE key=?1", [key], |r| r.get(0)).ok()?;
    let (w, h, data) = crate::imaging::inpaint::Fill::from_png(&png, scale)?;
    put_rgb(key, RgbPatch { w, h, data });
    RGB.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(key).cloned())
}

/// Key derived from the mask content (same result, same key)
pub fn key_of(png: &[u8]) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(png);
    h.iter().take(12).map(|b| format!("{b:02x}")).collect()
}
