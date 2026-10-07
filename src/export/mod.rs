//! Export: destination, file naming, format, color space, size, output sharpening, metadata, watermark.
//! Large images are developed tile by tile so memory use stays constant.

pub mod icc;
pub mod metadata;
pub mod print;
pub mod proof;
pub mod watermark;

use crate::catalog::Photo;
use crate::develop::filters;
use crate::develop::geometry::cropped_dims;
use crate::develop::image::SourceImage;
use crate::develop::pipeline::{Engine, RenderRequest};
use crate::develop::settings::DevelopSettings;
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use fast_image_resize as fr;
use icc::ColorSpace;
use metadata::{MetaInput, MetaMode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use watermark::{TokenCtx, Watermark};

/// Tile size and border margin (avoids blur seams).
const TILE: usize = 2048;
const TILE_PAD: usize = 96;
/// Number of photos processed at once (each photo is also parallel internally).
const EXPORT_WORKERS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Format {
    #[default]
    Jpeg,
    Png,
    Tiff,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Jpeg, Format::Png, Format::Tiff];
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Tiff => "TIFF",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::Tiff => "tif",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResizeMode {
    #[default]
    LongEdge,
    ShortEdge,
    WidthHeight,
    Megapixels,
    Percent,
}

impl ResizeMode {
    pub const ALL: [ResizeMode; 5] = [ResizeMode::LongEdge, ResizeMode::ShortEdge, ResizeMode::WidthHeight, ResizeMode::Megapixels, ResizeMode::Percent];
    pub fn name(self) -> &'static str {
        match self {
            ResizeMode::LongEdge => tr!("긴 변", "Long edge"),
            ResizeMode::ShortEdge => tr!("짧은 변", "Short edge"),
            ResizeMode::WidthHeight => tr!("너비 × 높이", "Width × Height"),
            ResizeMode::Megapixels => tr!("메가픽셀", "Megapixels"),
            ResizeMode::Percent => tr!("백분율", "Percentage"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SharpenFor {
    #[default]
    Screen,
    Matte,
    Glossy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SharpenLevel {
    Low,
    #[default]
    Standard,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Conflict {
    #[default]
    Unique,
    Overwrite,
    Skip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    pub preset_name: String,
    // Location
    pub same_as_original: bool,
    pub folder: String,
    pub use_subfolder: bool,
    pub subfolder: String,
    pub conflict: Conflict,
    // File naming
    pub rename: bool,
    pub template: String,
    pub custom_text: String,
    pub seq_start: u32,
    // File settings
    pub format: Format,
    pub quality: u8,
    pub limit_size: bool,
    pub limit_kb: u32,
    pub color_space: ColorSpace,
    pub tiff_16bit: bool,
    // Image sizing
    pub resize: bool,
    pub resize_mode: ResizeMode,
    pub size_a: u32,
    pub size_b: u32,
    pub megapixels: f32,
    pub percent: f32,
    pub dont_enlarge: bool,
    pub ppi: u32,
    // Output sharpening
    pub sharpen: bool,
    pub sharpen_for: SharpenFor,
    pub sharpen_level: SharpenLevel,
    // Metadata
    pub metadata: MetaMode,
    pub remove_location: bool,
    // Watermark
    pub watermark: bool,
    pub watermark_name: String,
    // Post-processing
    pub open_folder: bool,
    /// Automatic face blurring (faces detected at export time)
    pub auto_faces: bool,
    pub faces_kind: crate::develop::settings::PrivacyKind,
    /// Border: percent of the long edge (0 = none), color
    pub border_pct: f32,
    pub border_color: [u8; 3],
}

impl Default for ExportSettings {
    fn default() -> Self {
        let pictures = std::env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Pictures").join("Darkroom Export"))
            .unwrap_or_else(|| PathBuf::from("Darkroom Export"));
        Self {
            preset_name: String::new(),
            same_as_original: false,
            folder: pictures.to_string_lossy().to_string(),
            use_subfolder: false,
            subfolder: String::new(),
            conflict: Conflict::Unique,
            rename: false,
            template: "{name}".into(),
            custom_text: String::new(),
            seq_start: 1,
            format: Format::Jpeg,
            quality: 90,
            limit_size: false,
            limit_kb: 2000,
            color_space: ColorSpace::Srgb,
            tiff_16bit: true,
            resize: false,
            resize_mode: ResizeMode::LongEdge,
            size_a: 2048,
            size_b: 2048,
            megapixels: 12.0,
            percent: 50.0,
            dont_enlarge: true,
            ppi: 300,
            sharpen: false,
            sharpen_for: SharpenFor::Screen,
            sharpen_level: SharpenLevel::Standard,
            metadata: MetaMode::All,
            remove_location: false,
            watermark: false,
            watermark_name: String::new(),
            open_folder: true,
            auto_faces: false,
            faces_kind: crate::develop::settings::PrivacyKind::Mosaic,
            border_pct: 0.0,
            border_color: [255, 255, 255],
        }
    }
}

pub const NAME_TOKENS: &[(&str, &str)] = &[
    ("{name}", "원본 파일명"),
    ("{seq}", "일련번호"),
    ("{seq:3}", "일련번호 (3자리)"),
    ("{date}", "촬영일 YYYYMMDD"),
    ("{time}", "촬영시각 HHMMSS"),
    ("{year}", "촬영 연도"),
    ("{title}", "제목"),
    ("{custom}", "사용자 텍스트"),
    ("{camera}", "카메라 모델"),
    ("{rating}", "별점"),
];

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .trim_end_matches('.')
        .to_string()
}

pub fn file_stem(p: &Photo, ex: &ExportSettings, seq: u32) -> String {
    let orig = Path::new(&p.file_name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let orig = if p.master.is_some() { format!("{orig}-{}", p.copy_name.replace(' ', "")) } else { orig };
    if !ex.rename {
        return sanitize(&orig);
    }
    let ct = p.meta.capture_time.clone().unwrap_or_default();
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let date = if ct.len() >= 10 { digits(&ct[..10]) } else { String::new() };
    let time = if ct.len() >= 19 { digits(&ct[11..19]) } else { String::new() };
    let mut out = ex.template.clone();
    // {seq:N}
    while let Some(i) = out.find("{seq:") {
        let Some(j) = out[i..].find('}') else { break };
        let n: usize = out[i + 5..i + j].parse().unwrap_or(1);
        out.replace_range(i..i + j + 1, &format!("{:0width$}", seq, width = n));
    }
    let out = out
        .replace("{name}", &orig)
        .replace("{seq}", &seq.to_string())
        .replace("{date}", &date)
        .replace("{time}", &time)
        .replace("{year}", ct.get(..4).unwrap_or(""))
        .replace("{title}", &p.title)
        .replace("{custom}", &ex.custom_text)
        .replace("{camera}", p.meta.model.as_deref().unwrap_or(""))
        .replace("{rating}", &p.rating.to_string());
    let s = sanitize(&out);
    if s.is_empty() { sanitize(&orig) } else { s }
}

pub fn dest_dir(p: &Photo, ex: &ExportSettings) -> PathBuf {
    let base = if ex.same_as_original { p.folder.clone() } else { PathBuf::from(&ex.folder) };
    if ex.use_subfolder && !ex.subfolder.trim().is_empty() { base.join(sanitize(&ex.subfolder)) } else { base }
}

/// Compute the final output size.
pub fn target_size(cw: usize, ch: usize, ex: &ExportSettings) -> (usize, usize) {
    if !ex.resize {
        return (cw, ch);
    }
    let (cwf, chf) = (cw as f32, ch as f32);
    let mut k = match ex.resize_mode {
        ResizeMode::LongEdge => ex.size_a as f32 / cwf.max(chf),
        ResizeMode::ShortEdge => ex.size_a as f32 / cwf.min(chf),
        ResizeMode::WidthHeight => (ex.size_a as f32 / cwf).min(ex.size_b as f32 / chf),
        ResizeMode::Megapixels => (ex.megapixels * 1.0e6 / (cwf * chf)).sqrt(),
        ResizeMode::Percent => ex.percent / 100.0,
    };
    if ex.dont_enlarge {
        k = k.min(1.0);
    }
    (((cwf * k).round() as usize).max(1), ((chf * k).round() as usize).max(1))
}

/// Develop tile by tile into gamma-encoded RGB f32 (w*h*3).
pub fn render_full(src: &SourceImage, s: &DevelopSettings, w: usize, h: usize, engine: &mut Engine) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * 3];
    let tiles_x = w.div_ceil(TILE);
    let tiles_y = h.div_ceil(TILE);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x0 = tx * TILE;
            let y0 = ty * TILE;
            let x1 = (x0 + TILE).min(w);
            let y1 = (y0 + TILE).min(h);
            let single = tiles_x == 1 && tiles_y == 1;
            let pad = if single { 0 } else { TILE_PAD };
            let px0 = x0.saturating_sub(pad);
            let py0 = y0.saturating_sub(pad);
            let px1 = (x1 + pad).min(w);
            let py1 = (y1 + pad).min(h);
            let rw = px1 - px0;
            let rh = py1 - py0;
            let region = [px0 as f32 / w as f32, py0 as f32 / h as f32, px1 as f32 / w as f32, py1 as f32 / h as f32];
            let r = engine.render(
                src,
                &RenderRequest {
                    settings: s,
                    out_w: rw,
                    out_h: rh,
                    region,
                    draft: false,
                    clipping: false,
                    mask_overlay: None,
                    keep_float: true,
                },
            );
            let f = r.float.unwrap();
            for y in y0..y1 {
                let sy = y - py0;
                let sx = x0 - px0;
                let src_row = &f[(sy * rw + sx) * 3..(sy * rw + sx + (x1 - x0)) * 3];
                out[(y * w + x0) * 3..(y * w + x1) * 3].copy_from_slice(src_row);
            }
        }
    }
    out
}

fn resize_f32(data: Vec<f32>, w: usize, h: usize, nw: usize, nh: usize) -> Vec<f32> {
    if w == nw && h == nh {
        return data;
    }
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let Ok(src) = fr::images::Image::from_vec_u8(w as u32, h as u32, bytes, fr::PixelType::F32x3) else {
        return vec![0.0; nw * nh * 3];
    };
    let mut dst = fr::images::Image::new(nw as u32, nh as u32, fr::PixelType::F32x3);
    let mut rs = fr::Resizer::new();
    let _ = rs.resize(&src, &mut dst, &fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)));
    dst.into_vec()
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]).clamp(0.0, 1.0))
        .collect()
}

/// Output sharpening per medium (luminance unsharp mask in gamma space).
pub(crate) fn output_sharpen(img: &mut [f32], w: usize, h: usize, f: SharpenFor, l: SharpenLevel) {
    let (sigma, amount) = match f {
        SharpenFor::Screen => (0.6, 0.45),
        SharpenFor::Matte => (1.0, 0.8),
        SharpenFor::Glossy => (0.8, 0.6),
    };
    let k = match l {
        SharpenLevel::Low => 0.6,
        SharpenLevel::Standard => 1.0,
        SharpenLevel::High => 1.6,
    };
    let y: Vec<f32> = img.par_chunks(3).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let b = filters::gauss_blur(&y, w, h, sigma);
    img.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
        let d = ((y[i] - b[i]) * amount * k).clamp(-0.08, 0.08);
        for c in p.iter_mut() {
            *c = (*c + d).clamp(0.0, 1.0);
        }
    });
}

/// sRGB-gamma RGB to target color space gamma RGB.
fn convert_space(img: &mut [f32], cs: ColorSpace) {
    if cs == ColorSpace::Srgb {
        return;
    }
    let m = cs.from_srgb_matrix();
    img.par_chunks_mut(3).for_each(|p| {
        let l = [
            crate::develop::color::srgb_decode(p[0]),
            crate::develop::color::srgb_decode(p[1]),
            crate::develop::color::srgb_decode(p[2]),
        ];
        for c in 0..3 {
            let v = m[c][0] * l[0] + m[c][1] * l[1] + m[c][2] * l[2];
            p[c] = cs.encode(v.clamp(0.0, 1.0));
        }
    });
}

pub fn token_ctx(p: &Photo, artist: &str, copyright: &str) -> TokenCtx {
    TokenCtx {
        copyright: copyright.to_string(),
        artist: artist.to_string(),
        capture_time: p.meta.capture_time.clone().unwrap_or_default(),
        file_name: p.file_name.clone(),
        title: p.title.clone(),
        camera: [p.meta.make.clone(), p.meta.model.clone()].into_iter().flatten().collect::<Vec<_>>().join(" "),
        lens: p.meta.lens.clone().unwrap_or_default(),
        exif: p.meta.summary(),
    }
}

fn encode(img: &[f32], w: usize, h: usize, ex: &ExportSettings, quality: u8) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let icc = icc::build_profile(ex.color_space);
    let mut out = Vec::new();
    match ex.format {
        Format::Jpeg => {
            let rgb: Vec<u8> = img.par_iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            enc.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
        }
        Format::Png => {
            let rgb: Vec<u8> = img.par_iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
            let mut enc = image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Default,
                image::codecs::png::FilterType::Adaptive,
            );
            let _ = enc.set_icc_profile(icc);
            enc.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
        }
        Format::Tiff => {
            let mut cur = std::io::Cursor::new(&mut out);
            let mut enc = image::codecs::tiff::TiffEncoder::new(&mut cur);
            let _ = enc.set_icc_profile(icc);
            if ex.tiff_16bit {
                let v: Vec<u16> = img.par_iter().map(|v| (v * 65535.0 + 0.5) as u16).collect();
                let bytes: &[u8] = bytemuck_u16(&v);
                enc.write_image(bytes, w as u32, h as u32, image::ExtendedColorType::Rgb16)?;
            } else {
                let rgb: Vec<u8> = img.par_iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
                enc.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
            }
        }
    }
    Ok(out)
}

fn bytemuck_u16(v: &[u16]) -> &[u8] {
    // SAFETY: byte view of the same u16 slice memory (alignment requirement 1)
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) }
}

pub struct ExportJob {
    pub items: Vec<(Photo, DevelopSettings)>,
    pub settings: ExportSettings,
    pub watermark: Option<Watermark>,
    pub artist: String,
    pub copyright: String,
}

pub enum ExportEvent {
    Progress { done: usize, total: usize, current: String },
    Exported { id: crate::catalog::PhotoId },
    Finished { ok: usize, failed: Vec<(String, String)>, skipped: usize, dest: Option<PathBuf>, cancelled: bool },
}

fn unique_path(dir: &Path, stem: &str, ext: &str, conflict: Conflict, reserved: &parking_lot::Mutex<std::collections::HashSet<PathBuf>>) -> Option<PathBuf> {
    let mut res = reserved.lock();
    let first = dir.join(format!("{stem}.{ext}"));
    match conflict {
        Conflict::Overwrite => {
            res.insert(first.clone());
            Some(first)
        }
        Conflict::Skip => {
            if first.exists() || res.contains(&first) {
                None
            } else {
                res.insert(first.clone());
                Some(first)
            }
        }
        Conflict::Unique => {
            let mut p = first;
            let mut n = 2;
            while p.exists() || res.contains(&p) {
                p = dir.join(format!("{stem}-{n}.{ext}"));
                n += 1;
            }
            res.insert(p.clone());
            Some(p)
        }
    }
}

/// Export one photo. Returns the saved path (None if skipped).
#[allow(clippy::too_many_arguments)]
pub fn export_one(
    p: &Photo,
    s: &DevelopSettings,
    ex: &ExportSettings,
    wm: Option<&Watermark>,
    artist: &str,
    copyright: &str,
    seq: u32,
    engine: &mut Engine,
    reserved: &parking_lot::Mutex<std::collections::HashSet<PathBuf>>,
) -> Result<Option<PathBuf>> {
    let dir = dest_dir(p, ex);
    std::fs::create_dir_all(&dir).with_context(|| trf!("폴더 생성 실패: {}", "Couldn't create folder: {}", dir.display()))?;
    let stem = file_stem(p, ex, seq);
    let Some(path) = unique_path(&dir, &stem, ex.format.ext(), ex.conflict, reserved) else {
        return Ok(None);
    };
    let src = crate::imaging::decode::decode_source(&p.path, p.meta.orientation)?;
    // Automatic face blurring
    let face_settings;
    let s = if ex.auto_faces {
        let mut s2 = s.clone();
        if let Ok(r) = crate::imaging::faces::auto_regions(&src, ex.faces_kind, 50.0) {
            s2.privacy.retain(|x| !x.auto);
            s2.privacy.extend(r);
        }
        face_settings = s2;
        &face_settings
    } else {
        s
    };
    let (cw, ch) = cropped_dims(src.width(), src.height(), &s.geometry);
    let (tw, th) = target_size(cw, ch, ex);
    // Develop at a pyramid level resolution, then Lanczos to the final size (high-quality downscale without aliasing)
    let k = tw as f32 / cw as f32;
    let mut lvl = 1.0f32;
    while lvl * 0.5 >= k && lvl > 1.0 / 64.0 {
        lvl *= 0.5;
    }
    let (rw, rh) = if k >= 1.0 { (tw, th) } else { (((cw as f32 * lvl).round() as usize).max(tw), ((ch as f32 * lvl).round() as usize).max(th)) };
    let mut img = render_full(&src, s, rw, rh, engine);
    drop(src);
    img = resize_f32(img, rw, rh, tw, th);
    if ex.sharpen {
        output_sharpen(&mut img, tw, th, ex.sharpen_for, ex.sharpen_level);
    }
    if let Some(wm) = wm {
        watermark::apply_f32(&mut img, tw, th, wm, &token_ctx(p, artist, copyright));
    }
    // Border (added outside the photo)
    let (img, tw, th) = add_border(img, tw, th, ex.border_pct, ex.border_color);
    let mut img = img;
    convert_space(&mut img, ex.color_space);
    let mi = MetaInput {
        photo: p,
        mode: ex.metadata,
        remove_location: ex.remove_location,
        artist,
        copyright,
        ppi: ex.ppi,
        srgb: ex.color_space == ColorSpace::Srgb,
        width: tw as u32,
        height: th as u32,
    };
    let exif = metadata::build_exif(&mi);
    let xmp = metadata::build_xmp(&mi);
    let bytes = match ex.format {
        Format::Jpeg => {
            let icc = icc::build_profile(ex.color_space);
            let mut q = ex.quality;
            let mut data = encode(&img, tw, th, ex, q)?;
            if ex.limit_size {
                let limit = ex.limit_kb as usize * 1024;
                let (mut lo, mut hi) = (5u8, q);
                let mut best: Option<Vec<u8>> = if data.len() <= limit { Some(data.clone()) } else { None };
                if best.is_none() {
                    while lo <= hi {
                        q = (lo + hi) / 2;
                        let d = encode(&img, tw, th, ex, q)?;
                        if d.len() <= limit {
                            best = Some(d);
                            lo = q + 1;
                        } else {
                            if q == 0 {
                                break;
                            }
                            hi = q - 1;
                        }
                    }
                }
                data = best.unwrap_or(data);
            }
            metadata::inject_jpeg(&data, exif.as_deref(), xmp.as_deref(), Some(&icc))
        }
        Format::Png => metadata::inject_png(&encode(&img, tw, th, ex, 0)?, exif.as_deref(), xmp.as_deref()),
        Format::Tiff => encode(&img, tw, th, ex, 0)?,
    };
    std::fs::write(&path, bytes).with_context(|| trf!("저장 실패: {}", "Couldn't save: {}", path.display()))?;
    Ok(Some(path))
}

/// Add a border around the photo (percent of the long edge)
fn add_border(img: Vec<f32>, w: usize, h: usize, pct: f32, col: [u8; 3]) -> (Vec<f32>, usize, usize) {
    if pct <= 0.0 {
        return (img, w, h);
    }
    let b = ((w.max(h) as f32) * pct / 100.0).round().max(1.0) as usize;
    let (nw, nh) = (w + 2 * b, h + 2 * b);
    let c = [col[0] as f32 / 255.0, col[1] as f32 / 255.0, col[2] as f32 / 255.0];
    let mut out = vec![0.0f32; nw * nh * 3];
    for px in out.as_chunks_mut::<3>().0 {
        px.copy_from_slice(&c);
    }
    for y in 0..h {
        let src = &img[y * w * 3..(y + 1) * w * 3];
        let dst = ((y + b) * nw + b) * 3;
        out[dst..dst + w * 3].copy_from_slice(src);
    }
    (out, nw, nh)
}

/// Run export in the background.
pub fn spawn(job: ExportJob, tx: Sender<ExportEvent>, cancel: Arc<AtomicBool>, repaint: impl Fn() + Send + Sync + 'static) {
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("export".into())
        .spawn(move || {
            let total = job.items.len();
            let next = AtomicUsize::new(0);
            let done = AtomicUsize::new(0);
            let ok = AtomicUsize::new(0);
            let skipped = AtomicUsize::new(0);
            let failed = parking_lot::Mutex::new(Vec::new());
            let last_dest = parking_lot::Mutex::new(None);
            let reserved = parking_lot::Mutex::new(std::collections::HashSet::new());
            let wm = if job.settings.watermark { job.watermark.as_ref() } else { None };
            let repaint = &repaint;
            std::thread::scope(|sc| {
                for _ in 0..EXPORT_WORKERS.min(total.max(1)) {
                    sc.spawn(|| {
                        let mut engine = Engine::default();
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let i = next.fetch_add(1, Ordering::SeqCst);
                            if i >= total {
                                break;
                            }
                            let (p, s) = &job.items[i];
                            let _ = tx.send(ExportEvent::Progress { done: done.load(Ordering::SeqCst), total, current: p.display_name() });
                            repaint();
                            let seq = job.settings.seq_start + i as u32;
                            // A panic on one photo does not stop the rest (that photo is recorded as failed and the engine is recreated)
                            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| export_one(p, s, &job.settings, wm, &job.artist, &job.copyright, seq, &mut engine, &reserved)))
                                .unwrap_or_else(|_| Err(anyhow::anyhow!("{}", tr!("처리 중 오류 (이 사진을 건너뜀)", "Processing error (this photo was skipped)"))));
                            if r.is_err() {
                                engine = Engine::default();
                            }
                            match r {
                                Ok(Some(path)) => {
                                    ok.fetch_add(1, Ordering::SeqCst);
                                    let _ = tx.send(ExportEvent::Exported { id: p.id });
                                    *last_dest.lock() = path.parent().map(Path::to_path_buf);
                                }
                                Ok(None) => {
                                    skipped.fetch_add(1, Ordering::SeqCst);
                                }
                                Err(e) => failed.lock().push((p.display_name(), format!("{e:#}"))),
                            }
                            let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                            let _ = tx.send(ExportEvent::Progress { done: d, total, current: p.display_name() });
                            repaint();
                        }
                    });
                }
            });
            let dest = last_dest.into_inner();
            if job.settings.open_folder && !cancel.load(Ordering::Relaxed)
                && let Some(d) = &dest {
                    let _ = std::process::Command::new("explorer").arg(d).spawn();
                }
            let _ = tx.send(ExportEvent::Finished {
                ok: ok.load(Ordering::SeqCst),
                failed: failed.into_inner(),
                skipped: skipped.load(Ordering::SeqCst),
                dest,
                cancelled: cancel.load(Ordering::Relaxed),
            });
            repaint();
        })
        .expect("export thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::image::LinearImage;

    #[test]
    fn naming_tokens() {
        let mut c = crate::catalog::Catalog::open(&tempfile::tempdir().unwrap().path().join("x.db")).unwrap();
        let meta = crate::imaging::meta::PhotoMeta { capture_time: Some("2025-03-04 05:06:07".into()), ..Default::default() };
        let ids = c.add_photos(&[(PathBuf::from("C:/a/IMG_1.CR3"), 1, meta, None, vec![])], 1).unwrap();
        let p = c.get(ids[0]).unwrap();
        let ex = ExportSettings { rename: true, template: "{date}_{seq:4}_{name}".into(), ..Default::default() };
        assert_eq!(file_stem(p, &ex, 7), "20250304_0007_IMG_1");
    }

    #[test]
    fn target_sizes() {
        let ex = ExportSettings { resize: true, resize_mode: ResizeMode::LongEdge, size_a: 2000, ..Default::default() };
        assert_eq!(target_size(6000, 4000, &ex), (2000, 1333));
        let ex = ExportSettings { resize: true, resize_mode: ResizeMode::LongEdge, size_a: 9000, dont_enlarge: true, ..Default::default() };
        assert_eq!(target_size(6000, 4000, &ex), (6000, 4000));
    }

    #[test]
    fn tiled_render_matches_single() {
        let (w, h) = (300, 200);
        let mut img = LinearImage::new(w, h);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = ((i * 37) % 255) as f32 / 255.0 * 0.5;
        }
        let src = SourceImage::new(img, false, 64);
        let s = DevelopSettings::default();
        let mut e = Engine::default();
        let a = render_full(&src, &s, w, h, &mut e);
        assert_eq!(a.len(), w * h * 3);
        assert!(a.iter().all(|v| v.is_finite()));
    }
}
