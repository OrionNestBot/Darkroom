//! Lensfun lens data: a public lens-correction database (https://lensfun.github.io, data CC BY-SA 3.0).
//! A pinned snapshot of the XML is embedded in the executable; XML in the data folder's `lensfun` directory replaces or extends it.
//!
//! Model (same math as lensfun, coordinates relative to the image center):
//!   Distortion and TCA use Hugin coordinates: r = 1 is half the calibration sensor's short side (converted to mm via crop factor and aspect ratio)
//!     ptlens: r_d = r·(a r³ + b r² + c r + 1 − a − b − c),  poly3: r_d = r·(1 − k1 + k1 r²),  poly5: r_d = r·(1 + k1 r² + k2 r⁴)
//!     TCA poly3: r_c = r·(b r² + c r + v) (red and blue, relative to green),  linear: r_c = k·r
//!   Vignetting pa: r = 1 is half the calibration sensor diagonal,  V = 1 + k1 r² + k2 r⁴ + k3 r⁶ (corrected = value / V)
//!   Focal interpolation uses lensfun's Hermite spline (term × focal length); vignetting uses inverse-distance weighting over focal, aperture and distance (p = 3.5)
//! © 2026 OrionNest

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Built-in data: lensfun XML from assets/lensfun, compressed and embedded by build.rs
static BUNDLED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lensfun.bin"));
pub const BUNDLED_VERSION: &str = "2026-09-24 (bbd4332)";

/// Extra data folder (test switch: DARKROOM_LENSFUN_DIR): XML here replaces built-in files of the same name; new names are added
/// so newer lensfun data can be used without updating the program.
pub fn dir() -> PathBuf {
    std::env::var_os("DARKROOM_LENSFUN_DIR").map(PathBuf::from).unwrap_or_else(|| crate::config::data_dir().join("lensfun"))
}

/// Built-in files (name, contents)
fn bundled_files() -> Vec<(String, String)> {
    use std::io::Read;
    let mut raw = Vec::new();
    if flate2::read::DeflateDecoder::new(BUNDLED).read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    raw.split(|b| *b == 0)
        .filter_map(|chunk| {
            let nl = chunk.iter().position(|b| *b == b'\n')?;
            Some((String::from_utf8_lossy(&chunk[..nl]).to_string(), String::from_utf8_lossy(&chunk[nl + 1..]).to_string()))
        })
        .collect()
}

/// XML files in the extra data folder
pub fn user_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("xml")).unwrap_or(false))
        .collect();
    v.sort();
    v
}

// ───────────────────────── Data ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DistModel {
    Poly3,
    Poly5,
    PtLens,
}

#[derive(Clone, Copy, Debug)]
struct CalDist {
    focal: f64,
    model: DistModel,
    /// ptlens: a b c / poly3: k1 / poly5: k1 k2
    t: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TcaModel {
    Linear,
    Poly3,
}

#[derive(Clone, Copy, Debug)]
struct CalTca {
    focal: f64,
    model: TcaModel,
    /// vr vb cr cb br bb (linear: kr kb)
    t: [f64; 6],
}

#[derive(Clone, Copy, Debug)]
struct CalVig {
    focal: f64,
    aperture: f64,
    distance: f64,
    k: [f64; 3],
}

#[derive(Clone, Debug)]
pub struct Lens {
    pub model: String,
    pub crop: f64,
    pub aspect: f64,
    min_focal: f64,
    max_focal: f64,
    dist: Vec<CalDist>,
    tca: Vec<CalTca>,
    vig: Vec<CalVig>,
    words: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub models: Vec<String>,
    pub crop: f64,
}

#[derive(Default, Debug)]
pub struct Db {
    pub lenses: Vec<Lens>,
    pub cameras: Vec<Camera>,
}

static DB: RwLock<Option<Arc<Db>>> = RwLock::new(None);
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Lens data (built-in + extra folder), loaded on first use
pub fn db() -> Option<Arc<Db>> {
    if let Some(d) = DB.read().ok().and_then(|g| g.clone()) {
        return Some(d);
    }
    let d = Arc::new(load_all());
    if let Ok(mut g) = DB.write() {
        *g = Some(d.clone());
    }
    Some(d)
}

/// Data generation counter (part of the lens model cache key)
pub fn generation() -> u64 {
    GEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reload the extra folder (from the settings screen)
pub fn reload() {
    if let Ok(mut g) = DB.write() {
        *g = None;
    }
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn load_all() -> Db {
    let mut db = Db::default();
    // Extra-folder files first (the first entry wins for the same lens); skip built-in files with the same name
    let user = user_files();
    let names: std::collections::HashSet<String> = user.iter().filter_map(|p| p.file_name().map(|f| f.to_string_lossy().to_lowercase())).collect();
    for p in &user {
        if let Ok(s) = std::fs::read_to_string(p) {
            parse_into(&s, &mut db);
        }
    }
    for (name, text) in bundled_files() {
        if !names.contains(&name.to_lowercase()) {
            parse_into(&text, &mut db);
        }
    }
    db
}

/// Custom vignetting table (`assets/lens_vig.json`) for common lenses, fitted against reference renders (lens correction off / vignetting only).
/// { EXIF lens name: [[focal, aperture, k1, k2, k3], …] }, pa model, r = half the full-frame diagonal.
/// Takes precedence over lensfun data.
fn own_vig() -> &'static std::collections::HashMap<String, Vec<[f64; 5]>> {
    static V: std::sync::OnceLock<std::collections::HashMap<String, Vec<[f64; 5]>>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let text = std::env::var("DARKROOM_OWN_VIG").ok().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| include_str!("../../assets/lens_vig.json").to_string());
        let m: std::collections::HashMap<String, Vec<[f64; 5]>> = serde_json::from_str(&text).unwrap_or_default();
        m.into_iter().map(|(k, v)| (k.trim().to_lowercase(), v)).collect()
    })
}

/// Custom vignetting by EXIF lens name (full-frame k), using lensfun's inverse-distance interpolation
pub fn own_vig_for(exif_lens: &str, focal: f64, fnum: f64) -> Option<[f64; 3]> {
    if std::env::var("DARKROOM_OWN_VIG").map(|v| v == "0").unwrap_or(false) {
        return None;
    }
    let t = own_vig().get(&exif_lens.trim().to_lowercase())?;
    let (lo, hi) = t.iter().fold((f64::MAX, f64::MIN), |(a, z), e| (a.min(e[0]), z.max(e[0])));
    let lens = Lens {
        model: String::new(),
        crop: 1.0,
        aspect: 1.5,
        min_focal: lo,
        max_focal: hi,
        dist: Vec::new(),
        tca: Vec::new(),
        vig: t.iter().map(|e| CalVig { focal: e[0], aperture: e[1], distance: 1000.0, k: [e[2], e[3], e[4]] }).collect(),
        words: Vec::new(),
    };
    lens.interp_vig(focal, fnum, 1000.0)
}

impl Model {
    /// Replace vignetting with full-frame k values (this model's unit = half the calibration sensor diagonal = full frame / crop)
    pub fn set_vig_ff(&mut self, k: [f64; 3], lens_crop: f64) {
        let c2 = lens_crop * lens_crop;
        self.vig = Some([k[0] / c2, k[1] / (c2 * c2), k[2] / (c2 * c2 * c2)]);
    }
}

// ───────────────────────── XML (minimal parser for the lensfun format only) ─────────────────────────

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Blocks starting with `<tag>` or `<tag attrs>` and ending with `</tag>`
fn blocks<'a>(s: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = s[i..].find(&open) {
        let st = i + p;
        let after = st + open.len();
        // Distinguish <lens> from <lensdatabase>
        if !s[after..].starts_with('>') && !s[after..].starts_with(' ') {
            i = after;
            continue;
        }
        let Some(e) = s[after..].find(&close) else { break };
        out.push(&s[st..after + e]);
        i = after + e + close.len();
    }
    out
}

/// Simple element values: (lang attribute, value)
fn elems(block: &str, tag: &str) -> Vec<(Option<String>, String)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = block[i..].find(&open) {
        let st = i + p + open.len();
        let rest = &block[st..];
        if !(rest.starts_with('>') || rest.starts_with(' ')) {
            i = st;
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let head = &rest[..gt];
        if head.ends_with('/') {
            i = st + gt;
            continue;
        }
        let body = &rest[gt + 1..];
        let Some(e) = body.find(&close) else { break };
        out.push((attr(head, "lang"), unescape(body[..e].trim())));
        i = st + gt + 1 + e + close.len();
    }
    out
}

/// Attribute strings of empty elements (`<distortion … />`)
fn empty_elems<'a>(block: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag} ");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = block[i..].find(&open) {
        let st = i + p + open.len();
        let Some(gt) = block[st..].find('>') else { break };
        out.push(&block[st..st + gt]);
        i = st + gt;
    }
    out
}

fn attr(head: &str, name: &str) -> Option<String> {
    let mut i = 0;
    let pat = format!("{name}=\"");
    while let Some(p) = head[i..].find(&pat) {
        let st = i + p;
        // Name boundary (so "k1" does not match inside "dk1")
        if st > 0 && !head.as_bytes()[st - 1].is_ascii_whitespace() {
            i = st + pat.len();
            continue;
        }
        let v = &head[st + pat.len()..];
        let e = v.find('"')?;
        return Some(unescape(&v[..e]));
    }
    None
}

fn num(head: &str, name: &str) -> Option<f64> {
    attr(head, name).and_then(|v| v.trim().parse().ok())
}

fn aspect_of(s: &str) -> Option<f64> {
    if let Some((a, b)) = s.split_once(':') {
        let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        return (b > 0.0).then(|| a / b);
    }
    s.trim().parse().ok()
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(p) = s[i..].find("<!--") {
        out.push_str(&s[i..i + p]);
        match s[i + p..].find("-->") {
            Some(e) => i = i + p + e + 3,
            None => return out,
        }
    }
    out.push_str(&s[i..]);
    out
}

fn first_plain(v: &[(Option<String>, String)]) -> String {
    v.iter().find(|(l, _)| l.is_none()).or_else(|| v.first()).map(|(_, s)| s.clone()).unwrap_or_default()
}

fn parse_into(text: &str, db: &mut Db) {
    let text = strip_comments(text);
    for b in blocks(&text, "camera") {
        let models: Vec<String> = elems(b, "model").into_iter().map(|(_, s)| s).collect();
        let crop = elems(b, "cropfactor").first().and_then(|(_, s)| s.parse().ok()).unwrap_or(1.0);
        db.cameras.push(Camera { models, crop });
    }
    for b in blocks(&text, "lens") {
        let model = first_plain(&elems(b, "model"));
        if model.is_empty() {
            continue;
        }
        let crop = elems(b, "cropfactor").first().and_then(|(_, s)| s.parse().ok()).unwrap_or(1.0);
        let aspect = elems(b, "aspect-ratio").first().and_then(|(_, s)| aspect_of(s)).unwrap_or(1.5);
        let mut dist = Vec::new();
        for h in empty_elems(b, "distortion") {
            let Some(focal) = num(h, "focal") else { continue };
            let (model, t) = match attr(h, "model").as_deref() {
                Some("ptlens") => (DistModel::PtLens, [num(h, "a").unwrap_or(0.0), num(h, "b").unwrap_or(0.0), num(h, "c").unwrap_or(0.0)]),
                Some("poly3") => (DistModel::Poly3, [num(h, "k1").unwrap_or(0.0), 0.0, 0.0]),
                Some("poly5") => (DistModel::Poly5, [num(h, "k1").unwrap_or(0.0), num(h, "k2").unwrap_or(0.0), 0.0]),
                _ => continue,
            };
            dist.push(CalDist { focal, model, t });
        }
        let mut tca = Vec::new();
        for h in empty_elems(b, "tca") {
            let Some(focal) = num(h, "focal") else { continue };
            let (model, t) = match attr(h, "model").as_deref() {
                Some("poly3") => (
                    TcaModel::Poly3,
                    [num(h, "vr").unwrap_or(1.0), num(h, "vb").unwrap_or(1.0), num(h, "cr").unwrap_or(0.0), num(h, "cb").unwrap_or(0.0), num(h, "br").unwrap_or(0.0), num(h, "bb").unwrap_or(0.0)],
                ),
                Some("linear") => (TcaModel::Linear, [num(h, "kr").unwrap_or(1.0), num(h, "kb").unwrap_or(1.0), 0.0, 0.0, 0.0, 0.0]),
                _ => continue,
            };
            tca.push(CalTca { focal, model, t });
        }
        let mut vig = Vec::new();
        for h in empty_elems(b, "vignetting") {
            if attr(h, "model").as_deref() != Some("pa") {
                continue;
            }
            let (Some(focal), Some(aperture)) = (num(h, "focal"), num(h, "aperture")) else { continue };
            vig.push(CalVig { focal, aperture, distance: num(h, "distance").unwrap_or(1000.0), k: [num(h, "k1").unwrap_or(0.0), num(h, "k2").unwrap_or(0.0), num(h, "k3").unwrap_or(0.0)] });
        }
        if dist.is_empty() && tca.is_empty() && vig.is_empty() {
            continue;
        }
        // Focal range: <focal min max> / <focal value>, else from the name, else from the calibration entries
        let (mut lo, mut hi) = (f64::NAN, f64::NAN);
        for h in empty_elems(b, "focal") {
            if let Some(v) = num(h, "value") {
                (lo, hi) = (v, v);
            } else if let (Some(a), Some(z)) = (num(h, "min"), num(h, "max")) {
                (lo, hi) = (a, z);
            }
        }
        if lo.is_nan() {
            if let Some((a, z)) = name_focal(&model) {
                (lo, hi) = (a, z);
            } else {
                let fs = dist.iter().map(|c| c.focal).chain(tca.iter().map(|c| c.focal)).chain(vig.iter().map(|c| c.focal));
                let (a, z) = fs.fold((f64::MAX, f64::MIN), |(a, z), f| (a.min(f), z.max(f)));
                (lo, hi) = (a, z);
            }
        }
        let words = words(&model);
        db.lenses.push(Lens {
            model,
            crop,
            aspect,
            min_focal: lo,
            max_focal: hi,
            dist,
            tca,
            vig,
            words,
        });
    }
}

// ───────────────────────── Lookup ─────────────────────────

/// Focal range in a name ("24-105mm" → (24, 105), "50mm" → (50, 50))
pub fn name_focal(name: &str) -> Option<(f64, f64)> {
    let s = name.to_lowercase();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() && b[i - 1] != b'f' || b[i - 1] == b'.')) {
            let st = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let a: f64 = s[st..i].parse().ok()?;
            let mut z = a;
            let mut j = i;
            if j < b.len() && b[j] == b'-' {
                let st2 = j + 1;
                j = st2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                    j += 1;
                }
                if j > st2 {
                    z = s[st2..j].parse().ok()?;
                }
            }
            let rest = s[j..].trim_start();
            if rest.starts_with("mm") {
                return Some((a, z));
            }
            i = j.max(i);
            continue;
        }
        i += 1;
    }
    None
}

/// Matching tokens: lowercase, split at letter/digit boundaries, drop "mm"/"f" unit letters and symbols
fn words(name: &str) -> Vec<String> {
    let s = name.to_lowercase().replace("f/", "f ").replace('ƒ', "f");
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut kind = 0u8; // 1 = letter, 2 = digit
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            let w = std::mem::take(cur);
            if w != "mm" && w != "f" {
                out.push(w);
            }
        }
    };
    for c in s.chars() {
        let k = if c.is_ascii_digit() || (c == '.' && kind == 2) {
            2
        } else if c.is_alphanumeric() {
            1
        } else {
            0
        };
        if k != kind || k == 0 {
            flush(&mut cur, &mut out);
        }
        if k != 0 {
            cur.push(c);
        }
        kind = k;
    }
    flush(&mut cur, &mut out);
    // Treat "2.80" and "2.8" as equal
    for w in out.iter_mut() {
        if w.contains('.') {
            let t = w.trim_end_matches('0').trim_end_matches('.').to_string();
            *w = t;
        }
    }
    out.sort();
    out.dedup();
    out
}

impl Db {
    /// Camera crop factor (None if unknown)
    pub fn camera_crop(&self, make: &str, model: &str) -> Option<f64> {
        let m = model.trim().to_lowercase();
        let mk = make.trim().to_lowercase();
        let short = m.strip_prefix(&mk).map(|s| s.trim().to_string()).unwrap_or_else(|| m.clone());
        self.cameras
            .iter()
            .find(|c| c.models.iter().any(|x| {
                let x = x.to_lowercase();
                x == m || x == short || x.strip_prefix(&mk).map(|s| s.trim() == short).unwrap_or(false)
            }))
            .map(|c| c.crop)
    }

    pub fn by_name(&self, model: &str) -> Option<&Lens> {
        self.lenses.iter().find(|l| l.model == model)
    }

    /// Find by EXIF lens name: the focal range in the name must match; pick the lens sharing the most tokens.
    /// Calibrations with a larger crop factor than the photo (smaller sensor) are skipped since they don't cover the corners (lensfun rule)
    pub fn find(&self, lens: &str, img_crop: f64) -> Option<&Lens> {
        let fr = name_focal(&canon_spaced(lens))?;
        let w = words(&canon_spaced(lens));
        // Name is only a focal range ("17.0-50.0 mm", third-party lenses): accept only if exactly one lens has that range
        if w.iter().all(|x| x.chars().all(|c| c.is_ascii_digit() || c == '.')) {
            let mut c = self.lenses.iter().filter(|l| img_crop / l.crop >= 0.96 && name_focal(&l.model).map(|f| (f.0 - fr.0).abs() < 0.05 && (f.1 - fr.1).abs() < 0.05).unwrap_or(false));
            let first = c.next()?;
            return if c.next().is_none() { Some(first) } else { None };
        }
        let mut best: Option<(f64, &Lens)> = None;
        for l in &self.lenses {
            if img_crop / l.crop < 0.96 {
                continue;
            }
            let Some(lf) = name_focal(&l.model) else { continue };
            if (lf.0 - fr.0).abs() > 0.05 || (lf.1 - fr.1).abs() > 0.05 {
                continue;
            }
            let inter = w.iter().filter(|x| l.words.contains(x)).count() as f64;
            let union = (w.len() + l.words.len()) as f64 - inter;
            // Too many missing name tokens means a different lens (II, IS, L, etc.)
            let cover = inter / w.len().max(1) as f64;
            let score = inter / union.max(1.0) + 0.5 * cover;
            if score > best.map(|b| b.0).unwrap_or(0.7) {
                best = Some((score, l));
            }
        }
        best.map(|b| b.1)
    }
}

/// Insert a space in Canon EXIF lens names ("EF24-105mm" → "EF 24-105mm", "RF70-200mm" → "RF 70-200mm")
fn canon_spaced(s: &str) -> String {
    let t = s.trim();
    for p in ["EF-S", "EF-M", "RF-S", "EF", "RF"] {
        if let Some(rest) = t.strip_prefix(p)
            && rest.starts_with(|c: char| c.is_ascii_digit()) {
                return format!("Canon {p} {rest}");
            }
    }
    t.to_string()
}

/// Prefix for a manually chosen lensfun lens stored in settings ("lensfun:<name>")
pub const PREFIX: &str = "lensfun:";

/// Find the photo's lens → (lens, camera crop factor). Unknown cameras are treated as full frame, then retried as APS-C (1.6)
pub fn find_for<'a>(db: &'a Db, li: &crate::develop::image::LensInfo, manual: Option<&str>) -> Option<(&'a Lens, f64)> {
    // Crop factor: lensfun camera list → EXIF 35mm equivalent → otherwise match by lens name and use that lens's format
    let crop = db.camera_crop(&li.make, &li.model).or(li.crop.map(|c| c as f64));
    if let Some(name) = manual {
        let l = db.by_name(name)?;
        return Some((l, crop.unwrap_or(l.crop)));
    }
    match crop {
        Some(c) => db.find(&li.lens, c).map(|l| (l, c)),
        None => db.find(&li.lens, 1.0).map(|l| (l, 1.0)).or_else(|| db.find(&li.lens, 1.6).map(|l| (l, 1.6))).or_else(|| db.find(&li.lens, 100.0).map(|l| (l, l.crop))),
    }
}

// ───────────────────────── Model for the shooting conditions ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dist {
    Poly3(f64),
    Poly5(f64, f64),
    PtLens(f64, f64, f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tca {
    Linear { kr: f64, kb: f64 },
    /// [red, blue]
    Poly3 { v: [f64; 2], c: [f64; 2], b: [f64; 2] },
}

/// Lensfun model for one photo (coordinates in source pixels)
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub lens: String,
    pub dist: Option<Dist>,
    pub tca: Option<Tca>,
    pub vig: Option<[f64; 3]>,
    /// Pixels per Hugin unit (distortion, TCA)
    pub hugin_px: f64,
    /// Pixels per vignetting unit
    pub vig_px: f64,
}

/// Lensfun Hermite interpolation (_lf_interpolate); None if a neighbor is missing at either end
fn hermite(y1: Option<f64>, y2: f64, y3: f64, y4: Option<f64>, t: f64) -> f64 {
    let tg2 = match y1 {
        Some(y1) => (y3 - y1) * 0.5,
        None => y3 - y2,
    };
    let tg3 = match y4 {
        Some(y4) => (y4 - y2) * 0.5,
        None => y3 - y2,
    };
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y2 + (t3 - 2.0 * t2 + t) * tg2 + (-2.0 * t3 + 3.0 * t2) * y3 + (t3 - t2) * tg3
}

/// Four focal neighbors (two below, two above), same selection as lensfun __insert_spline
fn spline_pick<T: Copy>(items: &[(f64, T)], focal: f64) -> Option<(Option<(f64, T)>, (f64, T), (f64, T), Option<(f64, T)>)> {
    let mut below: Vec<(f64, T)> = items.iter().copied().filter(|(f, _)| *f < focal).collect();
    let mut above: Vec<(f64, T)> = items.iter().copied().filter(|(f, _)| *f > focal).collect();
    below.sort_by(|a, b| b.0.total_cmp(&a.0));
    above.sort_by(|a, b| a.0.total_cmp(&b.0));
    match (below.first(), above.first()) {
        (Some(&b1), Some(&a1)) => Some((below.get(1).copied(), b1, a1, above.get(1).copied())),
        _ => None,
    }
}

impl Lens {
    fn interp_dist(&self, focal: f64) -> Option<Dist> {
        let m0 = self.dist.first()?.model;
        let items: Vec<(f64, [f64; 3])> = self.dist.iter().filter(|c| c.model == m0).map(|c| (c.focal, c.t)).collect();
        let t = if let Some(e) = items.iter().find(|(f, _)| (*f - focal).abs() < 1e-9) {
            e.1
        } else if let Some((p0, p1, p2, p3)) = spline_pick(&items, focal) {
            let u = (focal - p1.0) / (p2.0 - p1.0);
            let mut t = [0.0; 3];
            for (i, v) in t.iter_mut().enumerate() {
                // ptlens/poly terms are multiplied by focal length before interpolation, then divided (lensfun __parameter_scales)
                *v = hermite(p0.map(|p| p.1[i] * p.0), p1.1[i] * p1.0, p2.1[i] * p2.0, p3.map(|p| p.1[i] * p.0), u) / focal;
            }
            t
        } else {
            // Out of range: nearest entry
            items.iter().min_by(|a, b| (a.0 - focal).abs().total_cmp(&(b.0 - focal).abs()))?.1
        };
        Some(match m0 {
            DistModel::Poly3 => Dist::Poly3(t[0]),
            DistModel::Poly5 => Dist::Poly5(t[0], t[1]),
            DistModel::PtLens => Dist::PtLens(t[0], t[1], t[2]),
        })
    }

    fn interp_tca(&self, focal: f64) -> Option<Tca> {
        let m0 = self.tca.first()?.model;
        let items: Vec<(f64, [f64; 6])> = self.tca.iter().filter(|c| c.model == m0).map(|c| (c.focal, c.t)).collect();
        let t = if let Some(e) = items.iter().find(|(f, _)| (*f - focal).abs() < 1e-9) {
            e.1
        } else if let Some((p0, p1, p2, p3)) = spline_pick(&items, focal) {
            let u = (focal - p1.0) / (p2.0 - p1.0);
            let mut t = [0.0; 6];
            for (i, v) in t.iter_mut().enumerate() {
                // v (scale) term unchanged; c and b terms scaled by focal length
                let sc = |f: f64| if i < 2 { 1.0 } else { f };
                *v = hermite(p0.map(|p| p.1[i] * sc(p.0)), p1.1[i] * sc(p1.0), p2.1[i] * sc(p2.0), p3.map(|p| p.1[i] * sc(p.0)), u) / sc(focal);
            }
            t
        } else {
            items.iter().min_by(|a, b| (a.0 - focal).abs().total_cmp(&(b.0 - focal).abs()))?.1
        };
        Some(match m0 {
            TcaModel::Linear => Tca::Linear { kr: t[0], kb: t[1] },
            TcaModel::Poly3 => Tca::Poly3 { v: [t[0], t[1]], c: [t[2], t[3]], b: [t[4], t[5]] },
        })
    }

    /// Vignetting: inverse-distance weighting in (focal, 4/aperture, 0.1/distance) space (lensfun InterpolateVignetting)
    fn interp_vig(&self, focal: f64, aperture: f64, distance: f64) -> Option<[f64; 3]> {
        if self.vig.is_empty() || aperture <= 0.0 {
            return None;
        }
        let df = self.max_focal - self.min_focal;
        let nf = |f: f64| if df != 0.0 { (f - self.min_focal) / df } else { f - self.min_focal };
        let mut acc = [0.0f64; 3];
        let mut wsum = 0.0;
        let mut smallest = f64::MAX;
        for c in &self.vig {
            let d = ((nf(c.focal) - nf(focal)).powi(2) + (4.0 / c.aperture - 4.0 / aperture).powi(2) + (0.1 / c.distance - 0.1 / distance).powi(2)).sqrt();
            if d < 1e-4 {
                return Some(c.k);
            }
            smallest = smallest.min(d);
            let w = 1.0 / d.powf(3.5);
            for i in 0..3 {
                acc[i] += w * c.k[i];
            }
            wsum += w;
        }
        if smallest > 1.0 || wsum <= 0.0 {
            return None;
        }
        Some([acc[0] / wsum, acc[1] / wsum, acc[2] / wsum])
    }

    /// Model for the shooting conditions and image size. w, h are sensor-orientation pixels (before rotation); img_crop is the camera crop factor
    pub fn model(&self, focal: f64, fnum: f64, w: f64, h: f64, img_crop: f64) -> Model {
        let diag_px = (w * w + h * h).sqrt();
        let px_per_mm = diag_px / (36f64.hypot(24.0) / img_crop.max(0.1));
        // Hugin unit: half the calibration sensor's short side (mm)
        let hugin_mm = 36f64.hypot(24.0) / self.crop / self.aspect.hypot(1.0) / 2.0;
        // Vignetting unit: half the calibration sensor diagonal (mm)
        let vig_mm = 36f64.hypot(24.0) / self.crop / 2.0;
        Model {
            lens: self.model.clone(),
            dist: self.interp_dist(focal),
            tca: self.interp_tca(focal),
            vig: self.interp_vig(focal, fnum, 1000.0),
            hugin_px: hugin_mm * px_per_mm,
            vig_px: vig_mm * px_per_mm,
        }
    }
}

impl Model {
    /// Corrected radius → source (distorted) radius scale, in Hugin units.
    /// The Hugin formula has scale 1 at r = 1 (half the short side), which leaves empty edges, so normalize like lensfun so the center scale is 1
    /// (barrel distortion then reads the edges from inside, leaving no gaps)
    #[inline]
    pub fn dist_ratio(&self, r: f64) -> f64 {
        match self.dist {
            Some(Dist::PtLens(a, b, c)) => {
                let d = 1.0 - a - b - c;
                (a * r * r * r + b * r * r + c * r + d) / d
            }
            Some(Dist::Poly3(k1)) => (1.0 - k1 + k1 * r * r) / (1.0 - k1),
            Some(Dist::Poly5(k1, k2)) => 1.0 + k1 * r * r + k2 * r * r * r * r,
            None => 1.0,
        }
    }

    /// Green (source) radius → red/blue position scale
    #[inline]
    pub fn tca_ratio(&self, r: f64) -> Option<(f64, f64)> {
        match self.tca? {
            Tca::Linear { kr, kb } => Some((kr, kb)),
            Tca::Poly3 { v, c, b } => Some((b[0] * r * r + c[0] * r + v[0], b[1] * r * r + c[1] * r + v[1])),
        }
    }

    /// Source radius (vignetting units) → vignetting V
    #[inline]
    pub fn vig_value(&self, r: f64) -> f64 {
        match self.vig {
            Some(k) => {
                let r2 = r * r;
                1.0 + k[0] * r2 + k[1] * r2 * r2 + k[2] * r2 * r2 * r2
            }
            None => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_words() {
        assert_eq!(name_focal("Canon EF 24-105mm f/4L IS USM"), Some((24.0, 105.0)));
        assert_eq!(name_focal("Canon RF 50mm F1.8 STM"), Some((50.0, 50.0)));
        assert_eq!(name_focal("18-35mm F1.8 DC HSM | Art 013"), Some((18.0, 35.0)));
        assert_eq!(name_focal(&canon_spaced("EF-S17-55mm f/2.8 IS USM")), Some((17.0, 55.0)));
        assert_eq!(words("Canon EF 24-105mm f/4L IS II USM"), vec!["105", "24", "4", "canon", "ef", "ii", "is", "l", "usm"]);
    }

    #[test]
    fn parse_small() {
        let xml = r#"<lensdatabase version="2"><!-- 주석 <lens> -->
        <camera><maker>Canon</maker><model>Canon EOS R6</model><model lang="en">EOS R6</model><mount>Canon RF</mount><cropfactor>1</cropfactor></camera>
        <lens><maker>Canon</maker><model>Canon RF 24-105mm F4L IS USM</model><mount>Canon RF</mount><cropfactor>1.0</cropfactor>
        <calibration>
            <distortion model="ptlens" focal="24.0" a="0.024775" b="-0.084337" c="0.064191" />
            <distortion model="ptlens" focal="35.0" a="0.005304" b="-0.008849" c="0.009513" />
            <tca model="poly3" focal="24.0" vr="1.0002511" vb="1.0000958" />
            <vignetting model="pa" focal="24" aperture="4" distance="1000" k1="-0.5432" k2="-0.6651" k3="0.4241"/>
        </calibration></lens></lensdatabase>"#;
        let mut db = Db::default();
        parse_into(xml, &mut db);
        assert_eq!(db.lenses.len(), 1);
        assert_eq!(db.camera_crop("Canon", "Canon EOS R6"), Some(1.0));
        let l = db.find("RF24-105mm F4 L IS USM", 1.0).expect("찾기");
        assert_eq!(l.model, "Canon RF 24-105mm F4L IS USM");
        assert!(db.find("RF24-70mm F2.8 L IS USM", 1.0).is_none());
        // Focal-only name: use it when there is a single candidate
        assert_eq!(db.find("24.0-105.0 mm", 1.0).map(|l| l.model.as_str()), Some("Canon RF 24-105mm F4L IS USM"));
        let m = l.model(24.0, 4.0, 6000.0, 4000.0, 1.0);
        assert_eq!(m.dist, Some(Dist::PtLens(0.024775, -0.084337, 0.064191)));
        // Full frame 6000×4000: Hugin 1 = half the short side = 2000 px, vignetting 1 = half the diagonal
        assert!((m.hugin_px - 2000.0).abs() < 0.5, "{}", m.hugin_px);
        assert!((m.vig_px - 6000f64.hypot(4000.0) / 2.0).abs() < 0.5);
        assert!((m.vig.unwrap()[0] + 0.5432).abs() < 1e-9);
        // Center scale is 1
        assert!((m.dist_ratio(0.0) - 1.0).abs() < 1e-12);
        // An intermediate focal length lies between the two entries
        let mid = l.model(29.0, 4.0, 6000.0, 4000.0, 1.0);
        let r0 = l.model(24.0, 4.0, 6000.0, 4000.0, 1.0).dist_ratio(0.5);
        let r1 = l.model(35.0, 4.0, 6000.0, 4000.0, 1.0).dist_ratio(0.5);
        let rm = mid.dist_ratio(0.5);
        assert!(rm > r0.min(r1) - 1e-6 && rm < r0.max(r1) + 1e-6, "{r0} {rm} {r1}");
    }
}

/// Built-in data is complete
#[cfg(test)]
#[test]
fn bundled_complete() {
    assert_eq!(bundled_files().len(), 56);
    let mut db = Db::default();
    for (_, t) in bundled_files() {
        parse_into(&t, &mut db);
    }
    assert!(db.lenses.len() > 1000 && db.cameras.len() > 500, "렌즈 {} · 카메라 {}", db.lenses.len(), db.cameras.len());
}

/// Check lens matching and lensfun data for photos in a folder (test switch: DARKROOM_PROBE_DIR=<raw folder>)
#[cfg(test)]
#[test]
#[ignore]
fn lensfun_probe_dir() {
    let db = db().expect("자료");
    let dir = std::env::var("DARKROOM_PROBE_DIR").expect("폴더");
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let Ok(src) = crate::imaging::decode::decode_source(&p, 1) else { continue };
        let Some(li) = src.lens_info.as_ref() else { continue };
        let found = find_for(&db, li, None);
        let desc = found.map(|(l, c)| {
            let m = l.model(li.focal as f64, li.fnumber as f64, 6000.0, 4000.0, c);
            format!("{} crop {c} · 왜곡 {:?} · 비네팅 {:?} · 비네팅 자료 {}개", l.model, m.dist.is_some(), m.vig, l.vig.len())
        });
        println!("{:?}: {} {} · {}mm f/{} → {:?}", p.file_name().unwrap(), li.model, li.lens, li.focal, li.fnumber, desc);
    }
}
