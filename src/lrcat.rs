//! Reader for the catalog format (.lrcat). The original file is never modified:
//! it is copied (with its WAL) to a temp folder and opened from there, so it is safe while another app has it open.
//! Imports file paths, rating/flag/color label, title/caption, keywords, develop settings (including local masks),
//! user rotation, virtual copies, collections (regular, smart, sets, quick collection), develop history and snapshots.

use crate::catalog::{ColorLabel, Flag, SmartRule, SmartRules};
use crate::develop::settings::DevelopSettings;
use crate::lrpreset::{Lua, crs_maps_from_lua, from_crs, masks_from_lua, parse_lua};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maximum history steps imported per photo (older steps are dropped).
pub const MAX_HISTORY_STEPS: usize = 60;

#[derive(Clone, Debug)]
pub struct LrPhoto {
    pub local_id: i64,
    pub path: PathBuf,
    pub rating: u8,
    pub flag: Flag,
    pub label: ColorLabel,
    pub title: String,
    pub caption: String,
    pub keywords: Vec<String>,
    pub develop: Option<DevelopSettings>,
    /// User rotation (catalog orientation string AB/BC/CD/DA).
    pub orientation: String,
    /// For a virtual copy, the local_id of the master photo.
    pub master: Option<i64>,
    pub copy_name: String,
    pub history: Vec<(String, DevelopSettings)>,
    pub snapshots: Vec<(String, DevelopSettings)>,
    pub capture_time: Option<String>,
    /// Last edit time (Unix seconds).
    pub touched: i64,
}

#[derive(Clone, Debug)]
pub struct LrCollection {
    pub name: String,
    /// Path built from the parent set names (e.g. "Travel / 2024").
    pub path: String,
    pub kind: LrCollectionKind,
    pub images: Vec<i64>,
    pub smart: Option<SmartRules>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LrCollectionKind {
    Normal,
    Smart,
    Quick,
}

#[derive(Default, Debug)]
pub struct LrCatalog {
    pub photos: Vec<LrPhoto>,
    pub collections: Vec<LrCollection>,
    /// Stack: member local_ids (top first).
    pub stacks: Vec<Vec<i64>>,
    pub warnings: Vec<String>,
}

impl LrCatalog {
    pub fn edited_count(&self) -> usize {
        self.photos.iter().filter(|p| p.develop.is_some()).count()
    }
}

/// Opens a copy in a temp folder so the original is never touched.
/// Returns (temp dir, connection): when bound together the connection drops first, then the folder is removed
/// (in the other order the open DB file keeps the folder from being deleted on Windows).
fn open_copy(path: &Path) -> Result<(tempdir::Guard, Connection)> {
    let guard = tempdir::Guard::new()?;
    let dst = guard.path.join("catalog.lrcat");
    std::fs::copy(path, &dst).with_context(|| trf!("카탈로그 복사 실패: {}", "Catalog copy failed: {}", path.display()))?;
    // Changes since the last checkpoint may still be in the WAL
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    if std::fs::metadata(&wal).map(|m| m.len() > 0).unwrap_or(false) {
        let _ = std::fs::copy(&wal, guard.path.join("catalog.lrcat-wal"));
    }
    let conn = Connection::open_with_flags(&dst, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    Ok((guard, conn))
}

/// Composes decomposed Hangul jamo (leading + vowel [+ trailing], the NFD used by macOS file names) into precomposed syllables.
pub fn compose_hangul(s: &str) -> String {
    const L0: u32 = 0x1100;
    const V0: u32 = 0x1161;
    const T0: u32 = 0x11A7;
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        if let Some(&last) = out.last() {
            let lu = last as u32;
            // Leading consonant + vowel
            if (L0..L0 + 19).contains(&lu) && (V0..V0 + 21).contains(&u) {
                *out.last_mut().unwrap() = char::from_u32(0xAC00 + ((lu - L0) * 21 + (u - V0)) * 28).unwrap();
                continue;
            }
            // Syllable without a final consonant + trailing consonant
            if (0xAC00..=0xD7A3).contains(&lu) && (lu - 0xAC00).is_multiple_of(28) && (T0 + 1..T0 + 28).contains(&u) {
                *out.last_mut().unwrap() = char::from_u32(lu + (u - T0)).unwrap();
                continue;
            }
        }
        out.push(c);
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod hangul_tests {
    #[test]
    fn compose_nfd() {
        // A four-syllable word written as decomposed jamo
        let nfd = "\u{1100}\u{1161}\u{110C}\u{1169}\u{11A8}\u{1109}\u{1161}\u{110C}\u{1175}\u{11AB}";
        assert_eq!(super::compose_hangul(nfd), "가족사진");
        assert_eq!(super::compose_hangul("D:/사진/IMG_1.CR3"), "D:/사진/IMG_1.CR3");
    }
}

/// Removes temp copies left over from earlier runs (except this process's own).
pub fn cleanup_stale_temp() {
    let me = format!("darkroom-lrcat-{}-", std::process::id());
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("darkroom-lrcat-") && !n.starts_with(&me) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

mod tempdir {
    use std::path::PathBuf;
    pub struct Guard {
        pub path: PathBuf,
    }
    impl Guard {
        pub fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir().join(format!("darkroom-lrcat-{}-{}", std::process::id(), crate::catalog::now()));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Catalog text column: a plain string, or for large values a blob of [4-byte big-endian length + zlib data].
fn col_text(r: &rusqlite::Row, i: usize) -> Option<String> {
    use rusqlite::types::ValueRef;
    match r.get_ref(i).ok()? {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) if b.len() > 4 => {
            use std::io::Read;
            let mut out = String::new();
            flate2::read::ZlibDecoder::new(&b[4..]).read_to_string(&mut out).ok()?;
            Some(out)
        }
        _ => None,
    }
}

fn table_exists(c: &Connection, name: &str) -> bool {
    c.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [name], |_| Ok(())).is_ok()
}

fn has_column(c: &Connection, table: &str, col: &str) -> bool {
    c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().any(|n| n == col)))
        .unwrap_or(false)
}

/// Develop settings text (Lua `s = { ... }`) to Darkroom settings.
pub fn develop_from_text(text: &str, is_raw: bool, warnings: &mut Vec<String>) -> Option<DevelopSettings> {
    let root = parse_lua(text)?;
    let (map, curves) = crs_maps_from_lua(&root)?;
    let (s, _g, w) = from_crs(&map, &curves);
    let mut d = DevelopSettings::default_for(is_raw);
    let mut all = crate::develop::settings::SettingGroups::all();
    all.crop = true;
    d.copy_groups_from(&s, &all);
    // A missing key in the catalog means the default. from_crs leaves detail defaults (e.g. RAW sharpening 40)
    // at 0, so keep the Darkroom default when the key is absent
    if !map.contains_key("Sharpness") {
        d.detail.sharpen_amount = DevelopSettings::default_for(is_raw).detail.sharpen_amount;
    }
    if !map.contains_key("ColorNoiseReduction") {
        d.detail.nr_color = DevelopSettings::default_for(is_raw).detail.nr_color;
    }
    let (masks, mw) = masks_from_lua(&root);
    d.masks = masks;
    d.spots = crate::lrpreset::spots_from_lua(&root);
    for m in w.into_iter().chain(mw) {
        if !warnings.contains(&m) {
            warnings.push(m);
        }
    }
    Some(d)
}

fn label_from_text(s: &str) -> ColorLabel {
    let l = s.to_lowercase();
    if l.is_empty() {
        ColorLabel::None
    } else if l.contains("red") || l.contains("빨") || l.contains("rot") || l.contains("rouge") {
        ColorLabel::Red
    } else if l.contains("yellow") || l.contains("노") || l.contains("gelb") || l.contains("jaune") {
        ColorLabel::Yellow
    } else if l.contains("green") || l.contains("녹") || l.contains("초록") || l.contains("grün") || l.contains("vert") {
        ColorLabel::Green
    } else if l.contains("blue") || l.contains("파") || l.contains("blau") || l.contains("bleu") {
        ColorLabel::Blue
    } else if l.contains("purple") || l.contains("자주") || l.contains("보라") || l.contains("lila") || l.contains("violet") {
        ColorLabel::Purple
    } else {
        ColorLabel::None
    }
}

fn xmp_title(xmp: &str) -> Option<String> {
    let i = xmp.find("<dc:title>")?;
    let rest = &xmp[i..];
    let end = rest.find("</dc:title>")?;
    let block = &rest[..end];
    let li = block.find("<rdf:li")?;
    let after = &block[li..];
    let gt = after.find('>')?;
    let close = after.find("</rdf:li>")?;
    let t = &after[gt + 1..close];
    Some(t.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'"))
}

pub fn read(path: &Path) -> Result<LrCatalog> {
    let (_guard, c) = open_copy(path)?;
    if !table_exists(&c, "Adobe_images") || !table_exists(&c, "AgLibraryFile") {
        anyhow::bail!("{}", trf!("Lightroom 카탈로그가 아닙니다", "Not a Lightroom catalog"));
    }
    let mut cat = LrCatalog::default();
    // File paths
    // Older and some other catalogs have no touchTime column
    let touch_col = if has_column(&c, "Adobe_images", "touchTime") { "i.touchTime" } else { "0" };
    let mut st = c.prepare(&format!(
        "SELECT i.id_local, rf.absolutePath, fo.pathFromRoot, f.baseName, f.extension, i.rating, i.pick, i.colorLabels,
                i.orientation, i.masterImage, i.copyName, i.captureTime, i.fileFormat, {touch_col}
         FROM Adobe_images i
         JOIN AgLibraryFile f ON f.id_local = i.rootFile
         JOIN AgLibraryFolder fo ON fo.id_local = f.folder
         JOIN AgLibraryRootFolder rf ON rf.id_local = fo.rootFolder
         ORDER BY i.id_local"
    ))?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            r.get::<_, Option<String>>(7)?.unwrap_or_default(),
            r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            r.get::<_, Option<i64>>(9)?,
            r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<String>>(12)?.unwrap_or_default(),
            r.get::<_, Option<f64>>(13)?.unwrap_or(0.0),
        ))
    })?;
    let mut idx: HashMap<i64, usize> = HashMap::new();
    for row in rows.flatten() {
        let (id, root, sub, base, ext, rating, pick, label, orient, master, copy_name, ctime, fmt, touch) = row;
        // Catalogs made on macOS store Hangul decomposed (NFD); compose it to match Windows file names
        let full = compose_hangul(&format!("{root}{sub}{base}{}{ext}", if ext.is_empty() { "" } else { "." }));
        let path = PathBuf::from(full.replace('/', "\\"));
        let _ = fmt;
        idx.insert(id, cat.photos.len());
        cat.photos.push(LrPhoto {
            local_id: id,
            path,
            rating: rating.clamp(0.0, 5.0) as u8,
            flag: if pick > 0.5 {
                Flag::Pick
            } else if pick < -0.5 {
                Flag::Reject
            } else {
                Flag::None
            },
            label: label_from_text(&label),
            title: String::new(),
            caption: String::new(),
            keywords: Vec::new(),
            develop: None,
            orientation: orient,
            master,
            copy_name,
            history: Vec::new(),
            snapshots: Vec::new(),
            capture_time: ctime.map(|t| t.replace('T', " ")),
            touched: if touch > 0.0 { touch as i64 + 978_307_200 } else { 0 },
        });
    }
    drop(st);
    let is_raw = |cat: &LrCatalog, i: usize| {
        cat.photos[i].path.extension().and_then(|e| e.to_str()).map(crate::config::is_raw_ext).unwrap_or(false)
    };
    // Develop settings
    if table_exists(&c, "Adobe_imageDevelopSettings") {
        let adj_col = if has_column(&c, "Adobe_imageDevelopSettings", "hasDevelopAdjustmentsEx") { "hasDevelopAdjustmentsEx" } else { "hasDevelopAdjustments" };
        let mut st = c.prepare(&format!("SELECT image, text, {adj_col} FROM Adobe_imageDevelopSettings WHERE text IS NOT NULL"))?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, col_text(r, 1).unwrap_or_default(), r.get::<_, Option<f64>>(2)?.unwrap_or(1.0))))?;
        let mut warnings = Vec::new();
        for (img, text, adj) in rows.flatten() {
            let Some(&i) = idx.get(&img) else { continue };
            if adj <= 0.0 && !text.contains("HasCrop = true") {
                continue;
            }
            let raw = is_raw(&cat, i);
            if let Some(d) = develop_from_text(&text, raw, &mut warnings)
                && !d.is_default_for(raw) {
                    cat.photos[i].develop = Some(d);
                }
        }
        cat.warnings.extend(warnings);
    }
    // History
    if table_exists(&c, "Adobe_libraryImageDevelopHistoryStep") {
        let mut st = c.prepare("SELECT image, name, text FROM Adobe_libraryImageDevelopHistoryStep WHERE text IS NOT NULL ORDER BY image, dateCreated, id_local")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), col_text(r, 2).unwrap_or_default())))?;
        let mut sink = Vec::new();
        for (img, name, text) in rows.flatten() {
            let Some(&i) = idx.get(&img) else { continue };
            let raw = is_raw(&cat, i);
            if let Some(d) = develop_from_text(&text, raw, &mut sink) {
                let label = if name.is_empty() { tr!("가져온 단계", "Imported step").to_string() } else { clean_history_name(&name) };
                cat.photos[i].history.push((label, d));
            }
        }
        for p in &mut cat.photos {
            if p.history.len() > MAX_HISTORY_STEPS {
                let cut = p.history.len() - MAX_HISTORY_STEPS;
                p.history.drain(..cut);
            }
        }
    }
    // Snapshots
    if table_exists(&c, "Adobe_libraryImageDevelopSnapshot") {
        let mut st = c.prepare("SELECT image, name, text FROM Adobe_libraryImageDevelopSnapshot WHERE text IS NOT NULL ORDER BY id_local")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), col_text(r, 2).unwrap_or_default())))?;
        let mut sink = Vec::new();
        for (img, name, text) in rows.flatten() {
            let Some(&i) = idx.get(&img) else { continue };
            let raw = is_raw(&cat, i);
            if let Some(d) = develop_from_text(&text, raw, &mut sink) {
                cat.photos[i].snapshots.push((if name.is_empty() { tr!("스냅샷", "Snapshots").into() } else { name }, d));
            }
        }
    }
    // Caption
    if table_exists(&c, "AgLibraryIPTC") {
        let mut st = c.prepare("SELECT image, caption FROM AgLibraryIPTC WHERE caption IS NOT NULL AND caption != ''")?;
        for (img, cap) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.flatten() {
            if let Some(&i) = idx.get(&img) {
                cat.photos[i].caption = cap;
            }
        }
    }
    // Title (stored inside the XMP)
    if table_exists(&c, "Adobe_AdditionalMetadata") {
        let mut st = c.prepare("SELECT image, xmp FROM Adobe_AdditionalMetadata WHERE xmp LIKE '%dc:title%'")?;
        for (img, xmp) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.flatten() {
            if let (Some(&i), Some(t)) = (idx.get(&img), xmp_title(&xmp)) {
                cat.photos[i].title = t;
            }
        }
    }
    // Keywords (hierarchical keywords keep only the leaf name; Darkroom keywords are flat)
    if table_exists(&c, "AgLibraryKeywordImage") {
        let mut st = c.prepare(
            "SELECT ki.image, k.name FROM AgLibraryKeywordImage ki JOIN AgLibraryKeyword k ON k.id_local = ki.tag WHERE k.name IS NOT NULL AND k.name != ''",
        )?;
        for (img, name) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.flatten() {
            if let Some(&i) = idx.get(&img)
                && !cat.photos[i].keywords.contains(&name) {
                    cat.photos[i].keywords.push(name);
                }
        }
    }
    // Collections
    if table_exists(&c, "AgLibraryCollection") {
        let mut st = c.prepare("SELECT id_local, name, parent, creationId, systemOnly FROM AgLibraryCollection")?;
        let all: Vec<(i64, String, Option<i64>, String, String)> = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    r.get::<_, Option<rusqlite::types::Value>>(4)?.map(|v| format!("{v:?}")).unwrap_or_default(),
                ))
            })?
            .flatten()
            .collect();
        drop(st);
        let names: HashMap<i64, (String, Option<i64>)> = all.iter().map(|(id, n, p, _, _)| (*id, (n.clone(), *p))).collect();
        let set_path = |mut parent: Option<i64>| {
            let mut parts = Vec::new();
            let mut guard = 0;
            while let Some(pid) = parent {
                let Some((n, pp)) = names.get(&pid) else { break };
                parts.push(n.clone());
                parent = *pp;
                guard += 1;
                if guard > 32 {
                    break;
                }
            }
            parts.reverse();
            parts.join(" / ")
        };
        // Stacks (AgLibraryFolderStackImage: position 1 = top)
        if table_exists(&c, "AgLibraryFolderStackImage") {
            let mut by: HashMap<i64, Vec<(f64, i64)>> = HashMap::new();
            let mut st = c.prepare("SELECT stack, image, position FROM AgLibraryFolderStackImage")?;
            for (s, img, pos) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<f64>>(2)?.unwrap_or(0.0))))?.flatten() {
                by.entry(s).or_default().push((pos, img));
            }
            let mut keys: Vec<i64> = by.keys().copied().collect();
            keys.sort();
            for k in keys {
                let mut v = by.remove(&k).unwrap_or_default();
                v.sort_by(|a, b| a.0.total_cmp(&b.0));
                if v.len() >= 2 {
                    cat.stacks.push(v.into_iter().map(|x| x.1).collect());
                }
            }
        }
        let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
        if table_exists(&c, "AgLibraryCollectionImage") {
            let mut st = c.prepare("SELECT collection, image FROM AgLibraryCollectionImage ORDER BY collection, positionInCollection")?;
            for (col, img) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?.flatten() {
                members.entry(col).or_default().push(img);
            }
        }
        let mut smart_text: HashMap<i64, String> = HashMap::new();
        if table_exists(&c, "AgLibraryCollectionContent") {
            let mut st = c.prepare("SELECT collection, content FROM AgLibraryCollectionContent WHERE owningModule = 'ag.library.smart_collection'")?;
            for (col, txt) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?.flatten() {
                smart_text.insert(col, txt);
            }
        }
        for (id, name, parent, creation, system) in all {
            let quick = system.contains('1') && (name.to_lowercase().contains("quick") || name.contains("빠른"));
            let kind = match creation.as_str() {
                _ if quick => LrCollectionKind::Quick,
                "com.adobe.ag.library.collection" => LrCollectionKind::Normal,
                "com.adobe.ag.library.smart_collection" => LrCollectionKind::Smart,
                _ if system.contains('1') && name.to_lowercase().contains("quick") => LrCollectionKind::Quick,
                _ if system.contains('1') && name.contains("빠른") => LrCollectionKind::Quick,
                _ => continue, // sets, publish services, web/print, etc.
            };
            let mut smart = None;
            if kind == LrCollectionKind::Smart {
                let (rules, w) = smart_rules_from_lua(smart_text.get(&id).map(String::as_str).unwrap_or(""));
                for x in w {
                    if !cat.warnings.contains(&x) {
                        cat.warnings.push(x);
                    }
                }
                smart = Some(rules);
            }
            cat.collections.push(LrCollection {
                path: set_path(parent),
                name,
                kind,
                images: members.remove(&id).unwrap_or_default(),
                smart,
            });
        }
    }
    Ok(cat)
}

/// History names like "Exposure: +0.50" are kept as is; localization keys ("$$$/...") are cleaned up.
fn clean_history_name(n: &str) -> String {
    if let Some(rest) = n.strip_prefix("$$$/") {
        rest.rsplit('=').next().unwrap_or(rest).to_string()
    } else {
        n.to_string()
    }
}

/// Smart collection rules (Lua) to Darkroom rules. Unsupported conditions produce warnings.
pub fn smart_rules_from_lua(text: &str) -> (SmartRules, Vec<String>) {
    let mut out = SmartRules { match_all: true, rules: Vec::new() };
    let mut warn = Vec::new();
    let Some(root) = parse_lua(text) else { return (out, warn) };
    out.match_all = root.str("combine").map(|c| c != "union").unwrap_or(true);
    let Lua::Table(items) = &root else { return (out, warn) };
    for (k, v) in items {
        if k.is_some() {
            continue;
        }
        let crit = v.str("criteria").unwrap_or_default();
        let op = v.str("operation").unwrap_or_default();
        let val = v.str("value").unwrap_or_default();
        let num = v.num("value").unwrap_or(0.0);
        let rule = match crit.as_str() {
            "rating" => match op.as_str() {
                ">=" | ">" => Some(SmartRule::RatingAtLeast((num as u8 + if op == ">" { 1 } else { 0 }).min(5))),
                "<=" | "<" => Some(SmartRule::RatingAtMost((num as i32 - if op == "<" { 1 } else { 0 }).clamp(0, 5) as u8)),
                "==" => {
                    out.rules.push(SmartRule::RatingAtLeast(num as u8));
                    Some(SmartRule::RatingAtMost(num as u8))
                }
                _ => None,
            },
            "pick" => Some(SmartRule::FlagIs(match num as i32 {
                1 => Flag::Pick,
                -1 => Flag::Reject,
                _ => Flag::None,
            })),
            // Numeric labels: 1 red, 2 yellow, 3 green, 4 blue, 5 purple (0 = none)
            "labelColor" if matches!(v.get("value"), Some(Lua::Num(_))) => Some(SmartRule::LabelIs(ColorLabel::from_i(num as i64))),
            "labelColor" | "labelText" => Some(SmartRule::LabelIs(label_from_text(&val))),
            "keywords" if op == "empty" => Some(SmartRule::HasKeywords(false)),
            "keywords" if op == "notEmpty" => Some(SmartRule::HasKeywords(true)),
            "keywords" => Some(SmartRule::KeywordContains(val.clone())),
            "captureTime" | "touchTime" if op == "inLast" => {
                let unit = v.str("value_units").unwrap_or_else(|| "days".into());
                let mul = match unit.as_str() {
                    "weeks" => 7.0,
                    "months" => 30.0,
                    "years" => 365.0,
                    "hours" => 1.0 / 24.0,
                    _ => 1.0,
                };
                let days = (num * mul).ceil().max(1.0) as u32;
                Some(if crit == "captureTime" { SmartRule::CapturedWithinDays(days) } else { SmartRule::EditedWithinDays(days) })
            }
            "allText" | "filename" | "title" | "caption" => Some(SmartRule::TextContains(val.clone())),
            "camera" | "cameraModel" => Some(SmartRule::CameraContains(val.clone())),
            "lens" => Some(SmartRule::LensContains(val.clone())),
            "fileFormat" => Some(SmartRule::FileTypeIs(val.clone())),
            "hasAdjustments" | "developPreset" => Some(SmartRule::HasEdits(v.bool("value").unwrap_or(num != 0.0))),
            "isoSpeedRating" => match op.as_str() {
                ">=" | ">" => Some(SmartRule::IsoAtLeast(num as u32)),
                "<=" | "<" => Some(SmartRule::IsoAtMost(num as u32)),
                _ => None,
            },
            "focalLength" => match op.as_str() {
                ">=" | ">" => Some(SmartRule::FocalAtLeast(num as f32)),
                "<=" | "<" => Some(SmartRule::FocalAtMost(num as f32)),
                _ => None,
            },
            "folder" => Some(SmartRule::InFolder(val.clone())),
            "captureTime" => match op.as_str() {
                ">" | ">=" | "after" => Some(SmartRule::CapturedAfter(val.clone())),
                "<" | "<=" | "before" => Some(SmartRule::CapturedBefore(val.clone())),
                _ => None,
            },
            _ => None,
        };
        match rule {
            Some(r) => out.rules.push(r),
            None => {
                let m = trf!("스마트 컬렉션 조건 '{crit} {op}'은(는) 지원하지 않아 생략", "Smart collection rule '{crit} {op}' isn't supported — skipped");
                if !warn.contains(&m) {
                    warn.push(m);
                }
            }
        }
    }
    (out, warn)
}

/// Catalog orientation string (EXIF orientation combined with user rotation) to the number of clockwise 90-degree turns.
/// AB = EXIF 1, BC = 6 (90° CW), CD = 3 (180°), DA = 8 (90° CCW). For mirrored values (BA/DC/AD/CB) only the rotation part is used.
pub fn orientation_steps(o: &str) -> Option<u8> {
    match o {
        "AB" | "BA" => Some(0),
        "BC" | "CB" => Some(1),
        "CD" | "DC" => Some(2),
        "DA" | "AD" => Some(3),
        _ => None,
    }
}

/// EXIF orientation (1/6/3/8) to the number of clockwise 90-degree turns.
pub fn exif_steps(o: u16) -> u8 {
    match o {
        6 | 7 => 1,
        3 | 4 => 2,
        8 | 5 => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic catalog containing only the columns of the real schema that the reader needs.
    fn make_catalog(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            r#"
CREATE TABLE Adobe_images(id_local INTEGER PRIMARY KEY, id_global, rating, pick, colorLabels, orientation, masterImage, copyName, captureTime, fileFormat, rootFile, touchTime);
CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, baseName, extension, folder);
CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot, rootFolder);
CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath, name);
CREATE TABLE Adobe_imageDevelopSettings(id_local INTEGER PRIMARY KEY, image, text, hasDevelopAdjustmentsEx);
CREATE TABLE Adobe_libraryImageDevelopHistoryStep(id_local INTEGER PRIMARY KEY, image, name, text, dateCreated);
CREATE TABLE Adobe_libraryImageDevelopSnapshot(id_local INTEGER PRIMARY KEY, image, name, text);
CREATE TABLE AgLibraryIPTC(id_local INTEGER PRIMARY KEY, image, caption);
CREATE TABLE Adobe_AdditionalMetadata(id_local INTEGER PRIMARY KEY, image, xmp);
CREATE TABLE AgLibraryKeyword(id_local INTEGER PRIMARY KEY, name, parent);
CREATE TABLE AgLibraryKeywordImage(id_local INTEGER PRIMARY KEY, image, tag);
CREATE TABLE AgLibraryCollection(id_local INTEGER PRIMARY KEY, name, parent, creationId, systemOnly);
CREATE TABLE AgLibraryCollectionImage(id_local INTEGER PRIMARY KEY, collection, image, positionInCollection);
CREATE TABLE AgLibraryCollectionContent(id_local INTEGER PRIMARY KEY, collection, content, owningModule);
INSERT INTO AgLibraryRootFolder VALUES(1, 'F:/사진/', '사진');
INSERT INTO AgLibraryFolder VALUES(1, '2024/여행/', 1);
INSERT INTO AgLibraryFile VALUES(1, 'IMG_0001', 'CR3', 1);
INSERT INTO AgLibraryFile VALUES(2, 'IMG_0002', 'JPG', 1);
INSERT INTO Adobe_images VALUES(10, 'g1', 4, 1, 'Red', 'BC', NULL, NULL, '2024-05-01T10:00:00', 'RAW', 1, 0);
INSERT INTO Adobe_images VALUES(11, 'g2', 0, -1, '', 'AB', NULL, NULL, '2024-05-01T10:01:00', 'JPG', 2, 0);
INSERT INTO Adobe_images VALUES(12, 'g3', 2, 0, '녹색', 'AB', 10, '흑백', '2024-05-01T10:00:00', 'RAW', 1, 0);
INSERT INTO AgLibraryIPTC VALUES(1, 10, '바다 풍경');
INSERT INTO Adobe_AdditionalMetadata VALUES(1, 10, '<x><dc:title><rdf:Alt><rdf:li xml:lang="x-default">해변 &amp; 노을</rdf:li></rdf:Alt></dc:title></x>');
INSERT INTO AgLibraryKeyword VALUES(1, '바다', NULL);
INSERT INTO AgLibraryKeyword VALUES(2, '여행', NULL);
INSERT INTO AgLibraryKeywordImage VALUES(1, 10, 1);
INSERT INTO AgLibraryKeywordImage VALUES(2, 10, 2);
INSERT INTO AgLibraryCollection VALUES(1, '2024 여행', NULL, 'com.adobe.ag.library.group', 0);
INSERT INTO AgLibraryCollection VALUES(2, '베스트', 1, 'com.adobe.ag.library.collection', 0);
INSERT INTO AgLibraryCollection VALUES(3, '별 3개 이상', NULL, 'com.adobe.ag.library.smart_collection', 0);
INSERT INTO AgLibraryCollectionImage VALUES(1, 2, 10, 1);
INSERT INTO AgLibraryCollectionImage VALUES(2, 2, 11, 2);
INSERT INTO AgLibraryCollectionContent VALUES(1, 3, 's = { { criteria = "rating", operation = ">=", value = 3, }, { criteria = "gps", operation = "==", value = true, }, combine = "intersect", }', 'ag.library.smart_collection');
"#,
        )
        .unwrap();
        let dev = r#"s = { AutoLateralCA = 0, Blacks2012 = -12, CameraProfile = "Adobe Standard", Contrast2012 = 15, Exposure2012 = 0.65,
 HasCrop = true, CropLeft = 0.1, CropTop = 0.05, CropRight = 0.9, CropBottom = 0.95, CropAngle = 1.5,
 Highlights2012 = -40, ProcessVersion = "11.0", Shadows2012 = 30, Temperature = 4800, Tint = 8, WhiteBalance = "Custom",
 ToneCurvePV2012 = { 0, 0, 64, 50, 192, 210, 255, 255, }, Sharpness = 25, ColorNoiseReduction = 20,
 MaskGroupBasedCorrections = { { What = "Correction", CorrectionActive = true, CorrectionAmount = 1, CorrectionName = "하늘",
   LocalExposure2012 = -0.25, LocalDehaze = 0.3,
   CorrectionMasks = { { What = "Mask/Gradient", MaskValue = 1, ZeroX = 0.5, ZeroY = 0.6, FullX = 0.5, FullY = 0.2, }, }, },
  { What = "Correction", CorrectionActive = true, CorrectionAmount = 1, LocalSaturation = -0.5,
   CorrectionMasks = { { What = "Mask/CircularGradient", Top = 0.2, Left = 0.3, Bottom = 0.6, Right = 0.7, Angle = 10, Feather = 60, Flipped = false, }, }, },
  { What = "Correction", CorrectionActive = true, CorrectionAmount = 1, LocalExposure2012 = 0.5,
   CorrectionMasks = { { What = "Mask/Image", MaskSubType = 1, }, }, }, },
}"#;
        c.execute("INSERT INTO Adobe_imageDevelopSettings VALUES(1, 10, ?1, 1)", [dev]).unwrap();
        c.execute("INSERT INTO Adobe_imageDevelopSettings VALUES(2, 11, 's = { ProcessVersion = \"11.0\", WhiteBalance = \"As Shot\", }', 0)", []).unwrap();
        c.execute("INSERT INTO Adobe_imageDevelopSettings VALUES(3, 12, 's = { ConvertToGrayscale = true, Exposure2012 = 0.3, }', 1)", []).unwrap();
        c.execute("INSERT INTO Adobe_libraryImageDevelopHistoryStep VALUES(1, 10, '가져오기', 's = { WhiteBalance = \"As Shot\", }', 1)", []).unwrap();
        c.execute("INSERT INTO Adobe_libraryImageDevelopHistoryStep VALUES(2, 10, '노출: +0.65', 's = { Exposure2012 = 0.65, }', 2)", []).unwrap();
        c.execute("INSERT INTO Adobe_libraryImageDevelopSnapshot VALUES(1, 10, '원본 비교', 's = { Exposure2012 = 0.2, }')", []).unwrap();
    }

    #[test]
    fn reads_synthetic_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.lrcat");
        make_catalog(&p);
        let before = std::fs::read(&p).unwrap();
        let cat = read(&p).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), before, "원본 카탈로그가 바뀌면 안 됨");
        assert_eq!(cat.photos.len(), 3);
        let a = &cat.photos[0];
        assert_eq!(a.path, PathBuf::from(r"F:\사진\2024\여행\IMG_0001.CR3"));
        assert_eq!((a.rating, a.flag, a.label), (4, Flag::Pick, ColorLabel::Red));
        assert_eq!(a.title, "해변 & 노을");
        assert_eq!(a.caption, "바다 풍경");
        assert_eq!(a.keywords, vec!["바다".to_string(), "여행".to_string()]);
        assert_eq!(orientation_steps(&a.orientation), Some(1));
        let d = a.develop.as_ref().expect("현상 설정");
        assert!((d.exposure - 0.65).abs() < 1e-4);
        assert_eq!((d.contrast, d.highlights, d.shadows, d.blacks), (15.0, -40.0, 30.0, -12.0));
        assert!(d.wb_custom && d.temp_k == 4800.0 && d.tint_k == 8.0);
        assert_eq!(d.profile, "Adobe Standard");
        assert_eq!(d.geometry.crop, [0.1, 0.05, 0.9, 0.95]);
        assert!((d.geometry.angle - 1.5).abs() < 1e-4);
        assert_eq!(d.curve.rgb.len(), 4);
        assert_eq!(d.detail.sharpen_amount, 25.0);
        // Masks: gradient + radial + AI subject (bitmap is recomputed in develop)
        assert_eq!(d.masks.len(), 3);
        assert!(matches!(d.masks[2].components[0].shape, crate::develop::settings::MaskShape::Ai { kind: crate::imaging::aimask::AiKind::Subject, .. }));
        assert_eq!(d.masks[0].name, "하늘");
        assert!((d.masks[0].adj.exposure + 1.0).abs() < 1e-4);
        assert!((d.masks[0].adj.dehaze - 30.0).abs() < 1e-3);
        match &d.masks[0].components[0].shape {
            crate::develop::settings::MaskShape::Linear { p0, p1 } => {
                assert_eq!(*p0, [0.5, 0.2]);
                assert_eq!(*p1, [0.5, 0.6]);
            }
            s => panic!("{s:?}"),
        }
        match &d.masks[1].components[0].shape {
            crate::develop::settings::MaskShape::Radial { center, radius, .. } => {
                assert!((center[0] - 0.5).abs() < 1e-5 && (center[1] - 0.4).abs() < 1e-5);
                assert!((radius[0] - 0.2).abs() < 1e-5 && (radius[1] - 0.2).abs() < 1e-5);
            }
            s => panic!("{s:?}"),
        }
        assert!(!cat.warnings.iter().any(|w| w.contains("AI 마스크")), "피사체 AI 마스크는 이제 가져옴");
        assert_eq!(a.history.len(), 2);
        assert_eq!(a.snapshots.len(), 1);
        // JPEG: no adjustments, so no develop settings; rejected flag
        assert!(cat.photos[1].develop.is_none());
        assert_eq!(cat.photos[1].flag, Flag::Reject);
        // Virtual copy
        let vc = &cat.photos[2];
        assert_eq!(vc.master, Some(10));
        assert_eq!(vc.copy_name, "흑백");
        assert_eq!(vc.label, ColorLabel::Green);
        assert_eq!(vc.develop.as_ref().unwrap().treatment, crate::develop::settings::Treatment::BlackWhite);
        // Collections: set path, smart rules
        let best = cat.collections.iter().find(|c| c.name == "베스트").unwrap();
        assert_eq!(best.path, "2024 여행");
        assert_eq!(best.images, vec![10, 11]);
        let smart = cat.collections.iter().find(|c| c.kind == LrCollectionKind::Smart).unwrap();
        let rules = smart.smart.as_ref().unwrap();
        assert!(rules.match_all);
        assert_eq!(rules.rules, vec![SmartRule::RatingAtLeast(3)]);
        assert!(cat.warnings.iter().any(|w| w.contains("gps")));
    }

    #[test]
    fn rejects_non_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.lrcat");
        Connection::open(&p).unwrap().execute_batch("CREATE TABLE foo(a);").unwrap();
        assert!(read(&p).is_err());
    }
}

#[cfg(test)]
mod real_probe {
    /// Test switch: DARKROOM_LRCAT_PROBE=<path to .lrcat> reads a real catalog.
    #[test]
    #[ignore]
    fn real_catalog() {
        let Some(p) = std::env::var_os("DARKROOM_LRCAT_PROBE") else { return };
        let t = std::time::Instant::now();
        let cat = super::read(std::path::Path::new(&p)).unwrap();
        println!("read {} photos in {:?}, edited {}, collections {}", cat.photos.len(), t.elapsed(), cat.edited_count(), cat.collections.len());
        for w in &cat.warnings {
            println!("warn: {w}");
        }
        for c in &cat.collections {
            println!("col: {:?} {} / {} imgs={} smart={:?}", c.kind, c.path, c.name, c.images.len(), c.smart);
        }
        let mut pairs = std::collections::BTreeMap::new();
        for ph in cat.photos.iter().take(400) {
            if !ph.path.exists() {
                continue;
            }
            let o = crate::imaging::meta::read_raw_meta(&ph.path).map(|m| m.orientation).unwrap_or(0);
            *pairs.entry((ph.orientation.clone(), o)).or_insert(0) += 1;
        }
        println!("(LR orientation, EXIF) counts: {pairs:?}");
        let missing = cat.photos.iter().filter(|p| !p.path.exists()).count();
        println!("missing files {missing}");
        let hist: usize = cat.photos.iter().map(|p| p.history.len()).sum();
        println!("history steps {hist}");
    }
}
