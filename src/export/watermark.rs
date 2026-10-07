//! Watermarks: text/graphic watermark editor with outline, line/letter spacing and metadata tokens.
//! All sizes and positions are relative to the image size, so the preview matches the export.

use crate::fonts;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WmKind {
    #[default]
    Text,
    Graphic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WmAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WmSize {
    /// Percent of image width
    #[default]
    Proportional,
    /// Fit to image width
    Fit,
    /// Largest size that fits inside the image
    Fill,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    pub name: String,
    pub kind: WmKind,
    // Text
    pub text: String,
    pub font_name: String,
    pub font_path: String,
    pub font_index: u32,
    pub align: WmAlign,
    pub color: [u8; 3],
    pub text_opacity: f32,
    pub line_spacing: f32,
    pub letter_spacing: f32,
    // Shadow
    pub shadow: bool,
    pub shadow_opacity: f32,
    pub shadow_offset: f32,
    pub shadow_radius: f32,
    pub shadow_angle: f32,
    // Outline
    pub stroke: bool,
    pub stroke_width: f32,
    pub stroke_color: [u8; 3],
    pub stroke_opacity: f32,
    // Graphic
    pub image_path: String,
    // Shared effects
    pub opacity: f32,
    pub size_mode: WmSize,
    pub size: f32,
    pub inset_h: f32,
    pub inset_v: f32,
    /// 0..8: top-left, top, top-right, left, center, right, bottom-left, bottom, bottom-right
    pub anchor: u8,
    /// Clockwise rotation in 90° steps, 0..3
    pub rotation: u8,
}

impl Default for Watermark {
    fn default() -> Self {
        let f = fonts::default_font();
        Self {
            name: tr!("기본 워터마크", "Default watermark").into(),
            kind: WmKind::Text,
            text: "© {year} {artist}".into(),
            font_name: f.name,
            font_path: f.path.to_string_lossy().to_string(),
            font_index: f.index,
            align: WmAlign::Center,
            color: [255, 255, 255],
            text_opacity: 100.0,
            line_spacing: 100.0,
            letter_spacing: 0.0,
            shadow: true,
            shadow_opacity: 40.0,
            shadow_offset: 10.0,
            shadow_radius: 20.0,
            shadow_angle: -45.0,
            stroke: false,
            stroke_width: 10.0,
            stroke_color: [0, 0, 0],
            stroke_opacity: 80.0,
            image_path: String::new(),
            opacity: 85.0,
            size_mode: WmSize::Proportional,
            size: 22.0,
            inset_h: 4.0,
            inset_v: 4.0,
            anchor: 8,
            rotation: 0,
        }
    }
}

/// Photo info used for watermark token substitution.
#[derive(Clone, Debug, Default)]
pub struct TokenCtx {
    pub copyright: String,
    pub artist: String,
    pub capture_time: String,
    pub file_name: String,
    pub title: String,
    pub camera: String,
    pub lens: String,
    pub exif: String,
}

pub const TOKENS: &[(&str, &str)] = &[
    ("{copyright}", "저작권"),
    ("{artist}", "작가"),
    ("{year}", "촬영 연도"),
    ("{date}", "촬영 날짜"),
    ("{filename}", "파일명"),
    ("{title}", "제목"),
    ("{camera}", "카메라"),
    ("{lens}", "렌즈"),
    ("{exif}", "촬영 정보"),
];

pub fn expand_tokens(text: &str, c: &TokenCtx) -> String {
    let year = if c.capture_time.len() >= 4 {
        c.capture_time[..4].to_string()
    } else {
        current_year().to_string()
    };
    let date = if c.capture_time.len() >= 10 { c.capture_time[..10].to_string() } else { String::new() };
    text.replace("{copyright}", &c.copyright)
        .replace("{artist}", &c.artist)
        .replace("{year}", &year)
        .replace("{date}", &date)
        .replace("{filename}", &c.file_name)
        .replace("{title}", &c.title)
        .replace("{camera}", &c.camera)
        .replace("{lens}", &c.lens)
        .replace("{exif}", &c.exif)
}

pub fn current_year() -> i32 {
    // Days since 1970-01-01 -> year (Gregorian)
    let days = crate::catalog::now() / 86400;
    let mut y = 1970;
    let mut d = days;
    loop {
        let len = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    y
}

fn font_cache() -> &'static Mutex<HashMap<(String, u32), Arc<FontVec>>> {
    static C: OnceLock<Mutex<HashMap<(String, u32), Arc<FontVec>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_font(path: &str, index: u32) -> Option<Arc<FontVec>> {
    let key = (path.to_string(), index);
    if let Some(f) = font_cache().lock().get(&key) {
        return Some(f.clone());
    }
    let data = std::fs::read(path).ok()?;
    let f = FontVec::try_from_vec_and_index(data, index).ok()?;
    let f = Arc::new(f);
    font_cache().lock().insert(key, f.clone());
    Some(f)
}

fn graphic_cache() -> &'static Mutex<Option<(String, Arc<image::RgbaImage>)>> {
    static C: OnceLock<Mutex<Option<(String, Arc<image::RgbaImage>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn load_graphic(path: &str) -> Option<Arc<image::RgbaImage>> {
    if let Some((p, img)) = graphic_cache().lock().as_ref()
        && p == path {
            return Some(img.clone());
        }
    let img = Arc::new(image::open(Path::new(path)).ok()?.into_rgba8());
    *graphic_cache().lock() = Some((path.to_string(), img.clone()));
    Some(img)
}

/// Premultiplied RGBA (0..1) layer.
pub struct Layer {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 4]>,
}

impl Layer {
    fn new(w: usize, h: usize) -> Self {
        Self { w: w.max(1), h: h.max(1), px: vec![[0.0; 4]; w.max(1) * h.max(1)] }
    }

    fn rotated(self, rot: u8) -> Layer {
        let rot = rot % 4;
        if rot == 0 {
            return self;
        }
        let (w, h) = (self.w, self.h);
        let (nw, nh) = if rot % 2 == 1 { (h, w) } else { (w, h) };
        let mut out = Layer::new(nw, nh);
        for y in 0..nh {
            for x in 0..nw {
                let (sx, sy) = match rot {
                    1 => (y, h - 1 - x),
                    2 => (w - 1 - x, h - 1 - y),
                    _ => (w - 1 - y, x),
                };
                out.px[y * nw + x] = self.px[sy * w + sx];
            }
        }
        out
    }
}

fn blur_alpha(a: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 {
        return a.to_vec();
    }
    crate::develop::filters::gauss_blur(a, w, h, sigma)
}

/// Builds the text alpha mask and layout at font size `px`. Returns (mask, width, height, padding).
fn text_alpha(wm: &Watermark, text: &str, px: f32, pad: usize) -> Option<(Vec<f32>, usize, usize)> {
    let font = load_font(&wm.font_path, wm.font_index).or_else(|| {
        let d = fonts::default_font();
        load_font(&d.path.to_string_lossy(), d.index)
    })?;
    let sf = font.as_scaled(PxScale::from(px));
    let line_h = (sf.ascent() - sf.descent() + sf.line_gap()) * wm.line_spacing.max(10.0) / 100.0;
    let spacing = wm.letter_spacing / 100.0 * px * 0.5;
    let lines: Vec<&str> = text.lines().collect();
    let lines = if lines.is_empty() { vec![""] } else { lines };
    // Measure line widths
    let measure = |line: &str| -> f32 {
        let mut x = 0.0;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            x += sf.h_advance(id) + spacing;
            prev = Some(id);
        }
        (x - spacing).max(0.0)
    };
    let widths: Vec<f32> = lines.iter().map(|l| measure(l)).collect();
    let block_w = widths.iter().cloned().fold(1.0, f32::max).ceil() as usize;
    let block_h = ((line_h * (lines.len() as f32 - 1.0)) + sf.ascent() - sf.descent()).ceil().max(1.0) as usize;
    let w = block_w + pad * 2;
    let h = block_h + pad * 2;
    let mut alpha = vec![0.0f32; w * h];
    for (li, line) in lines.iter().enumerate() {
        let lw = widths[li];
        let x0 = match wm.align {
            WmAlign::Left => 0.0,
            WmAlign::Center => (block_w as f32 - lw) * 0.5,
            WmAlign::Right => block_w as f32 - lw,
        } + pad as f32;
        let baseline = pad as f32 + sf.ascent() + line_h * li as f32;
        let mut x = x0;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let g = id.with_scale_and_position(PxScale::from(px), point(x, baseline));
            if let Some(og) = font.outline_glyph(g) {
                let b = og.px_bounds();
                og.draw(|gx, gy, c| {
                    let xx = b.min.x as i32 + gx as i32;
                    let yy = b.min.y as i32 + gy as i32;
                    if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                        let a = &mut alpha[yy as usize * w + xx as usize];
                        *a = (*a + c).min(1.0);
                    }
                });
            }
            x += sf.h_advance(id) + spacing;
            prev = Some(id);
        }
    }
    Some((alpha, w, h))
}

fn text_layer(wm: &Watermark, text: &str, px: f32) -> Option<Layer> {
    let stroke_r = if wm.stroke { wm.stroke_width / 100.0 * px * 0.12 } else { 0.0 };
    let sh_off = if wm.shadow { wm.shadow_offset / 100.0 * px * 0.3 } else { 0.0 };
    let sh_r = if wm.shadow { wm.shadow_radius / 100.0 * px * 0.25 } else { 0.0 };
    let pad = (stroke_r + sh_off + sh_r * 3.0 + 2.0).ceil() as usize;
    let (alpha, w, h) = text_alpha(wm, text, px, pad)?;
    let mut layer = Layer::new(w, h);
    // Outline: blur then amplify for a soft dilation
    let stroke_a = if wm.stroke && stroke_r > 0.0 {
        let b = blur_alpha(&alpha, w, h, stroke_r * 0.5);
        Some(b.iter().map(|v| (v * 4.0).min(1.0)).collect::<Vec<f32>>())
    } else {
        None
    };
    // Shadow: offset + blurred alpha of the shape (including outline)
    if wm.shadow && wm.shadow_opacity > 0.0 {
        let base = stroke_a.as_ref().unwrap_or(&alpha);
        let ang = wm.shadow_angle.to_radians();
        let dx = (ang.cos() * sh_off).round() as i32;
        let dy = (-ang.sin() * sh_off).round() as i32;
        let mut shifted = vec![0.0f32; w * h];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let sx = x - dx;
                let sy = y - dy;
                if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                    shifted[y as usize * w + x as usize] = base[sy as usize * w + sx as usize];
                }
            }
        }
        let sb = blur_alpha(&shifted, w, h, sh_r);
        let op = wm.shadow_opacity / 100.0;
        for (p, a) in layer.px.iter_mut().zip(sb) {
            *p = [0.0, 0.0, 0.0, a * op];
        }
    }
    let over = |dst: &mut [f32; 4], c: [u8; 3], a: f32| {
        let s = [c[0] as f32 / 255.0 * a, c[1] as f32 / 255.0 * a, c[2] as f32 / 255.0 * a, a];
        for k in 0..4 {
            dst[k] = s[k] + dst[k] * (1.0 - a);
        }
    };
    if let Some(sa) = &stroke_a {
        let op = wm.stroke_opacity / 100.0;
        for (p, a) in layer.px.iter_mut().zip(sa) {
            over(p, wm.stroke_color, a * op);
        }
    }
    let op = wm.text_opacity / 100.0;
    for (p, a) in layer.px.iter_mut().zip(alpha) {
        over(p, wm.color, a * op);
    }
    Some(layer)
}

fn graphic_layer(wm: &Watermark, target_w: usize, target_h: usize) -> Option<Layer> {
    let img = load_graphic(&wm.image_path)?;
    let resized = image::imageops::resize(img.as_ref(), target_w.max(1) as u32, target_h.max(1) as u32, image::imageops::FilterType::Lanczos3);
    let mut l = Layer::new(target_w, target_h);
    for (p, s) in l.px.iter_mut().zip(resized.pixels()) {
        let a = s[3] as f32 / 255.0;
        *p = [s[0] as f32 / 255.0 * a, s[1] as f32 / 255.0 * a, s[2] as f32 / 255.0 * a, a];
    }
    Some(l)
}

/// Final layer placed on a WxH image and its top-left position.
pub fn build(wm: &Watermark, img_w: usize, img_h: usize, ctx: &TokenCtx) -> Option<(Layer, i64, i64)> {
    if img_w < 8 || img_h < 8 {
        return None;
    }
    let ix = (wm.inset_h / 100.0 * img_w as f32 * 0.5).round();
    let iy = (wm.inset_v / 100.0 * img_h as f32 * 0.5).round();
    let avail_w = (img_w as f32 - 2.0 * ix).max(4.0);
    let avail_h = (img_h as f32 - 2.0 * iy).max(4.0);
    let rot = wm.rotation % 4;
    let swap = rot % 2 == 1;
    // Target scale based on the displayed size after rotation
    let fit_scale = |nat_w: f32, nat_h: f32| -> f32 {
        let (dw, dh) = if swap { (nat_h, nat_w) } else { (nat_w, nat_h) };
        match wm.size_mode {
            WmSize::Proportional => (wm.size / 100.0 * img_w as f32) / dw,
            WmSize::Fit => avail_w / dw,
            WmSize::Fill => (avail_w / dw).min(avail_h / dh),
        }
    };
    let layer = match wm.kind {
        WmKind::Text => {
            let text = expand_tokens(&wm.text, ctx);
            if text.trim().is_empty() {
                return None;
            }
            const REF: f32 = 100.0;
            let (_, nw, nh) = text_alpha(wm, &text, REF, 0)?;
            let k = fit_scale(nw as f32, nh as f32);
            let px = (REF * k).clamp(4.0, 4000.0);
            text_layer(wm, &text, px)?
        }
        WmKind::Graphic => {
            let img = load_graphic(&wm.image_path)?;
            let k = fit_scale(img.width() as f32, img.height() as f32);
            let tw = ((img.width() as f32 * k).round() as usize).max(1);
            let th = ((img.height() as f32 * k).round() as usize).max(1);
            graphic_layer(wm, tw, th)?
        }
    };
    let layer = layer.rotated(rot);
    let (lw, lh) = (layer.w as f32, layer.h as f32);
    let col = wm.anchor % 3;
    let row = (wm.anchor / 3).min(2);
    let x = match col {
        0 => ix,
        1 => (img_w as f32 - lw) * 0.5,
        _ => img_w as f32 - lw - ix,
    };
    let y = match row {
        0 => iy,
        1 => (img_h as f32 - lh) * 0.5,
        _ => img_h as f32 - lh - iy,
    };
    // The text layer includes shadow padding, so shift it outward by the padding (keeps the visual inset)
    Some((layer, x.round() as i64, y.round() as i64))
}

/// Composites onto a float RGB (0..1, gamma-encoded) image.
pub fn apply_f32(img: &mut [f32], w: usize, h: usize, wm: &Watermark, ctx: &TokenCtx) {
    let Some((layer, ox, oy)) = build(wm, w, h, ctx) else { return };
    let op = wm.opacity / 100.0;
    for ly in 0..layer.h {
        let y = oy + ly as i64;
        if y < 0 || y >= h as i64 {
            continue;
        }
        for lx in 0..layer.w {
            let x = ox + lx as i64;
            if x < 0 || x >= w as i64 {
                continue;
            }
            let p = layer.px[ly * layer.w + lx];
            let a = p[3] * op;
            if a <= 0.0 {
                continue;
            }
            let i = (y as usize * w + x as usize) * 3;
            for c in 0..3 {
                img[i + c] = p[c] * op + img[i + c] * (1.0 - a);
            }
        }
    }
}

/// Composites onto an RGBA8 image (editor preview).
pub fn apply_rgba8(img: &mut [u8], w: usize, h: usize, wm: &Watermark, ctx: &TokenCtx) {
    let Some((layer, ox, oy)) = build(wm, w, h, ctx) else { return };
    let op = wm.opacity / 100.0;
    for ly in 0..layer.h {
        let y = oy + ly as i64;
        if y < 0 || y >= h as i64 {
            continue;
        }
        for lx in 0..layer.w {
            let x = ox + lx as i64;
            if x < 0 || x >= w as i64 {
                continue;
            }
            let p = layer.px[ly * layer.w + lx];
            let a = p[3] * op;
            if a <= 0.0 {
                continue;
            }
            let i = (y as usize * w + x as usize) * 4;
            for c in 0..3 {
                let v = p[c] * op * 255.0 + img[i + c] as f32 * (1.0 - a);
                img[i + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_watermark_renders_bottom_right() {
        let wm = Watermark { text: "© 2026 OrionNest".into(), ..Default::default() };
        let (w, h) = (800usize, 600usize);
        let mut img = vec![0.5f32; w * h * 3];
        apply_f32(&mut img, w, h, &wm, &TokenCtx::default());
        // Bottom-right quadrant changes, top-left stays the same
        let changed = |x0: usize, y0: usize, x1: usize, y1: usize| {
            (y0..y1).any(|y| (x0..x1).any(|x| (img[(y * w + x) * 3] - 0.5).abs() > 0.05))
        };
        assert!(changed(400, 300, 800, 600));
        assert!(!changed(0, 0, 400, 300));
    }

    #[test]
    fn proportional_size_matches_width() {
        let wm = Watermark { text: "WATERMARK".into(), shadow: false, size: 50.0, ..Default::default() };
        let (layer, _, _) = build(&wm, 1000, 800, &TokenCtx::default()).unwrap();
        // Width excluding padding (2px) is about 50% of the image width
        assert!((layer.w as i32 - 500).abs() < 30, "w={}", layer.w);
    }

    #[test]
    fn tokens_expand() {
        let c = TokenCtx { artist: "OrionNest".into(), capture_time: "2025-06-01 10:00:00".into(), ..Default::default() };
        assert_eq!(expand_tokens("© {year} {artist}", &c), "© 2025 OrionNest");
    }
}
