//! System font list from the registry (font files are not read, so it returns immediately).

use crate::config::SYSTEM_FONT_DIR;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct FontEntry {
    /// Display name (e.g. "Malgun Gothic Bold")
    pub name: String,
    pub path: PathBuf,
    /// Face index within a TTC
    pub index: u32,
}

pub fn system_fonts() -> &'static Vec<FontEntry> {
    static FONTS: OnceLock<Vec<FontEntry>> = OnceLock::new();
    FONTS.get_or_init(load_fonts)
}

fn load_fonts() -> Vec<FontEntry> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let mut out = Vec::new();
    let sub = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let Ok(key) = RegKey::predef(hive).open_subkey(sub) else { continue };
        for (name, value) in key.enum_values().flatten() {
            let file = value.to_string();
            let file = file.trim_end_matches('\0').trim();
            let lower = file.to_lowercase();
            if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) {
                continue;
            }
            let path = if file.contains('\\') { PathBuf::from(file) } else { PathBuf::from(SYSTEM_FONT_DIR).join(file) };
            // "A & B (TrueType)": one entry per TTC face
            let clean = name
                .trim_end_matches(" (TrueType)")
                .trim_end_matches(" (OpenType)")
                .trim()
                .to_string();
            for (i, part) in clean.split(" & ").enumerate() {
                let index = if lower.ends_with(".ttc") { i as u32 } else { 0 };
                out.push(FontEntry { name: part.trim().to_string(), path: path.clone(), index });
            }
        }
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Default watermark font.
pub fn default_font() -> FontEntry {
    let fonts = system_fonts();
    for want in ["Malgun Gothic", "맑은 고딕", "Segoe UI", "Arial"] {
        if let Some(f) = fonts.iter().find(|f| f.name.eq_ignore_ascii_case(want)) {
            return f.clone();
        }
    }
    fonts.first().cloned().unwrap_or(FontEntry {
        name: "Malgun Gothic".into(),
        path: PathBuf::from(SYSTEM_FONT_DIR).join("malgun.ttf"),
        index: 0,
    })
}
