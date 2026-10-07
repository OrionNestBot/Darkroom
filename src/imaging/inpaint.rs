//! Remove (content-aware fill): fills a painted area from its surroundings. LaMa (Suvorov et al. 2022, Apache-2.0).
//! Crops around the area from the source (linear camera light), runs the model at 512x512, converts the result back to linear light and stores it as a patch.
//! © 2026 OrionNest

use super::ai::{self, Asset};
use crate::develop::image::{SourceImage, sample_bilinear};
use anyhow::{Result, anyhow};

pub const LAMA: Asset = Asset {
    file: "lama_fp32.onnx",
    url: "https://huggingface.co/Carve/LaMa-ONNX/resolve/c3c0c9e468934d62e79c329e35d82dd09ff8c444/lama_fp32.onnx",
    entry: None,
    size: 208044816,
    sha256: "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6",
};

const S: usize = 512;

pub fn assets() -> Vec<&'static Asset> {
    ai::RUNTIME.iter().chain(std::iter::once(&LAMA)).collect()
}

pub fn missing_bytes() -> u64 {
    assets().iter().filter(|a| !ai::present(a)).map(|a| if a.entry.is_some() { a.size / 2 } else { a.size }).sum()
}

pub fn download(progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    ai::download_list(&assets(), progress)
}

/// Fill result: area [x0,y0,x1,y1] in normalized source coordinates, patch size, linear RGB
pub struct Fill {
    pub bbox: [f32; 4],
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<f32>,
}

impl Fill {
    /// Encode as 16-bit PNG (square-root encoding + max scale) -> (PNG, scale)
    pub fn to_png(&self) -> (Vec<u8>, f32) {
        let scale = self.rgb.iter().cloned().fold(1e-6f32, f32::max);
        let mut buf: Vec<u8> = Vec::with_capacity(self.rgb.len() * 2);
        for v in &self.rgb {
            let e = ((v.max(0.0) / scale).sqrt() * 65535.0 + 0.5) as u16;
            buf.extend_from_slice(&e.to_ne_bytes());
        }
        let mut out = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive);
        let _ = image::ImageEncoder::write_image(enc, &buf, self.w as u32, self.h as u32, image::ExtendedColorType::Rgb16);
        (out, scale)
    }

    pub fn from_png(b: &[u8], scale: f32) -> Option<(usize, usize, Vec<f32>)> {
        let img = image::load_from_memory(b).ok()?.into_rgb16();
        let (w, h) = img.dimensions();
        let data = img.into_raw().into_iter().map(|v| (v as f32 / 65535.0).powi(2) * scale).collect();
        Some((w as usize, h as usize, data))
    }
}

/// Fill the area painted along a path (source pixels) with a radius
pub fn fill(src: &SourceImage, path: &[[f32; 2]], r: f32) -> Result<Fill> {
    let (bw, bh) = (src.width() as f32, src.height() as f32);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in path {
        x0 = x0.min(p[0] - r);
        y0 = y0.min(p[1] - r);
        x1 = x1.max(p[0] + r);
        y1 = y1.max(p[1] + r);
    }
    // Surrounding context: a square about 2.5x the area size (at least 160 px)
    let side = ((x1 - x0).max(y1 - y0) * 2.5).max(160.0).min(bw.max(bh));
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let (mut rx0, mut ry0) = (cx - side * 0.5, cy - side * 0.5);
    rx0 = rx0.clamp(0.0, (bw - side).max(0.0));
    ry0 = ry0.clamp(0.0, (bh - side).max(0.0));
    let (rw, rh) = (side.min(bw), side.min(bh));
    // Read resolution level: a step slightly finer than the 512 grid
    let li = src.levels.iter().rposition(|l| (rw * l.w as f32 / bw) >= S as f32).unwrap_or(0);
    let lvl = &src.levels[li];
    let k = lvl.w as f32 / bw;
    let mut lin = vec![0.0f32; 3 * S * S];
    for y in 0..S {
        for x in 0..S {
            let bx = rx0 + (x as f32 + 0.5) * rw / S as f32;
            let by = ry0 + (y as f32 + 0.5) * rh / S as f32;
            let p = sample_bilinear(lvl, bx * k, by * k);
            for c in 0..3 {
                lin[(y * S + x) * 3 + c] = p[c].max(0.0);
            }
        }
    }
    // Brightness normalization: map the top 1% brightness to 1, then gamma
    let mut ys: Vec<f32> = lin.as_chunks::<3>().0.iter().map(|p| p[0].max(p[1]).max(p[2])).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    let gain = 1.0 / ys[(ys.len() as f32 * 0.99) as usize].max(1e-4);
    let mut img = vec![0.0f32; 3 * S * S];
    let mut mask = vec![0.0f32; S * S];
    let pr: Vec<[f32; 2]> = path.iter().map(|p| [(p[0] - rx0) * S as f32 / rw, (p[1] - ry0) * S as f32 / rh]).collect();
    let rr = r * S as f32 / rw * 1.15 + 2.0;
    for y in 0..S {
        for x in 0..S {
            let i = y * S + x;
            for c in 0..3 {
                img[c * S * S + i] = (lin[i * 3 + c] * gain).clamp(0.0, 1.0).powf(1.0 / 2.2);
            }
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let inside = if pr.len() < 2 {
                (fx - pr[0][0]).hypot(fy - pr[0][1]) <= rr
            } else {
                pr.windows(2).any(|s| seg_dist(fx, fy, s[0], s[1]) <= rr)
            };
            mask[i] = inside as u8 as f32;
        }
    }
    let model = ai::ai_dir().join("models").join(LAMA.file);
    let out = ai::with_session(&model, S, |sess, _| {
        let ti = ort::value::Tensor::from_array((vec![1i64, 3, S as i64, S as i64], img.clone())).map_err(|e| anyhow!("{e}"))?;
        let tm = ort::value::Tensor::from_array((vec![1i64, 1, S as i64, S as i64], mask.clone())).map_err(|e| anyhow!("{e}"))?;
        let o = sess.run(ort::inputs!["image" => ti, "mask" => tm]).map_err(|e| anyhow!("{}", trf!("지우기 실패: {e}", "Remove failed: {e}")))?;
        Ok(o[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?.1.to_vec())
    })?;
    let div = if out.iter().cloned().fold(0.0f32, f32::max) > 1.5 { 255.0 } else { 1.0 };
    // Undo gamma -> linear light
    let mut rgb = vec![0.0f32; 3 * S * S];
    for i in 0..S * S {
        for c in 0..3 {
            let v = (out[c * S * S + i] / div).clamp(0.0, 1.0).powf(2.2) / gain;
            // Keep only the painted area plus a margin; the rest is unused, so zero it (smaller stored size)
            rgb[i * 3 + c] = if mask[i] > 0.5 { v } else { 0.0 };
        }
    }
    Ok(Fill { bbox: [rx0 / bw, ry0 / bh, (rx0 + rw) / bw, (ry0 + rh) / bh], w: S, h: S, rgb })
}

fn seg_dist(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((px - a[0]) * dx + (py - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (px - a[0] - t * dx).hypot(py - a[1] - t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png16_roundtrip() {
        let f = Fill { bbox: [0.0, 0.0, 1.0, 1.0], w: 2, h: 1, rgb: vec![0.1, 0.5, 2.0, 0.0, 0.01, 1.0] };
        let (png, scale) = f.to_png();
        let (w, h, d) = Fill::from_png(&png, scale).unwrap();
        assert_eq!((w, h), (2, 1));
        for (a, b) in d.iter().zip(&f.rgb) {
            assert!((a - b).abs() < 0.002 * scale.max(1.0), "{a} {b}");
        }
    }

    /// Real model test: remove a line through the middle of a photo (DARKROOM_AI_DIR, DARKROOM_MASK_IMG)
    #[test]
    #[ignore]
    fn real_fill() {
        let dir = std::env::var("DARKROOM_MASK_IMG").expect("DARKROOM_MASK_IMG");
        let p = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().map(|x| x.eq_ignore_ascii_case("cr3")).unwrap_or(false)).unwrap();
        let src = crate::imaging::decode::decode_source(&p, 1).unwrap();
        let (w, h) = (src.width() as f32, src.height() as f32);
        let path = vec![[w * 0.45, h * 0.5], [w * 0.55, h * 0.52]];
        let t = std::time::Instant::now();
        let f = fill(&src, &path, w * 0.01).unwrap();
        let (png, scale) = f.to_png();
        eprintln!("fill {}x{} bbox {:?} scale {scale:.3} png {}KB in {:?}", f.w, f.h, f.bbox, png.len() / 1024, t.elapsed());
        assert!(f.rgb.iter().all(|v| v.is_finite()));
    }
}
