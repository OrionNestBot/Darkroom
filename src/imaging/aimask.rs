//! AI masks: subject (BiRefNet-lite, MIT) and sky/people (SegFormer-B2 ADE20K).
//! Input is a preview rendered with default settings; output is an 8-bit gray bitmap covering the whole source (default orientation).
//! Model output is coarse, so edges are refined with a guided filter using the photo's luminance.
//! © 2026 OrionNest

use super::ai::{self, Asset};
use super::decode::Rgba8;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub const SUBJECT_MODEL: Asset = Asset {
    file: "birefnet_lite.onnx",
    url: "https://huggingface.co/onnx-community/BiRefNet_lite-ONNX/resolve/de15b22ba131738a16dff04aab8bdf8dc32e3ac1/onnx/model.onnx",
    entry: None,
    size: 224005088,
    sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
};

pub const SEG_MODEL: Asset = Asset {
    file: "segformer_b2_ade.onnx",
    url: "https://huggingface.co/Xenova/segformer-b2-finetuned-ade-512-512/resolve/df795789e70f4089c8658907679c6fd2367c89a5/onnx/model.onnx",
    entry: None,
    size: 110445327,
    sha256: "819c15e6af8c4de3359c1de7ab0a17d0dde495df1d16f8908a7163f8038e0fa0",
};

/// ADE20K class indices.
const ADE_SKY: usize = 2;
const ADE_PERSON: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AiKind {
    #[default]
    Subject,
    Background,
    Sky,
    People,
}

impl AiKind {
    pub const ALL: [AiKind; 4] = [AiKind::Subject, AiKind::Sky, AiKind::Background, AiKind::People];
    pub fn name(self) -> &'static str {
        match self {
            AiKind::Subject => tr!("피사체", "Subject"),
            AiKind::Background => tr!("배경", "Background"),
            AiKind::Sky => tr!("하늘", "Sky"),
            AiKind::People => tr!("사람", "People"),
        }
    }
    fn model(self) -> &'static Asset {
        match self {
            AiKind::Subject | AiKind::Background => &SUBJECT_MODEL,
            AiKind::Sky | AiKind::People => &SEG_MODEL,
        }
    }
}

pub fn assets(kind: AiKind) -> Vec<&'static Asset> {
    ai::RUNTIME.iter().chain(std::iter::once(kind.model())).collect()
}

pub fn missing_bytes(kind: AiKind) -> u64 {
    assets(kind).iter().filter(|a| !ai::present(a)).map(|a| if a.entry.is_some() { a.size / 2 } else { a.size }).sum()
}

pub fn download(kind: AiKind, progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    ai::download_list(&assets(kind), progress)
}

/// 8-bit gray mask (0 = outside, 255 = inside).
#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive);
        let _ = image::ImageEncoder::write_image(enc, &self.data, self.w as u32, self.h as u32, image::ExtendedColorType::L8);
        out
    }

    pub fn from_png(b: &[u8]) -> Option<Self> {
        let img = image::load_from_memory(b).ok()?.into_luma8();
        let (w, h) = img.dimensions();
        Some(Self { w: w as usize, h: h as usize, data: img.into_raw() })
    }
}

/// Resize a preview image into the model input (NCHW, ImageNet normalization).
fn to_tensor(img: &Rgba8, tw: usize, th: usize) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let mut v = vec![0.0f32; 3 * tw * th];
    let (sw, sh) = (img.w as f32, img.h as f32);
    for y in 0..th {
        for x in 0..tw {
            let p = sample(img, (x as f32 + 0.5) * sw / tw as f32 - 0.5, (y as f32 + 0.5) * sh / th as f32 - 0.5);
            for c in 0..3 {
                v[c * tw * th + y * tw + x] = (p[c] / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    v
}

fn sample(img: &Rgba8, x: f32, y: f32) -> [f32; 3] {
    let w = img.w as i32;
    let h = img.h as i32;
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as i32, y as i32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let g = |xx: i32, yy: i32, c: usize| img.data[((yy * w + xx) * 4) as usize + c] as f32;
    let mut o = [0.0; 3];
    for (c, v) in o.iter_mut().enumerate() {
        let a = g(x0, y0, c) * (1.0 - tx) + g(x1, y0, c) * tx;
        let b = g(x0, y1, c) * (1.0 - tx) + g(x1, y1, c) * tx;
        *v = a * (1.0 - ty) + b * ty;
    }
    o
}

/// Bilinearly upscale a low-res probability map (mw×mh) to the preview size.
fn upsample(p: &[f32], mw: usize, mh: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * mh as f32 / h as f32 - 0.5).clamp(0.0, (mh - 1) as f32);
        let (y0, ty) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(mh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * mw as f32 / w as f32 - 0.5).clamp(0.0, (mw - 1) as f32);
            let (x0, tx) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(mw - 1);
            let a = p[y0 * mw + x0] * (1.0 - tx) + p[y0 * mw + x1] * tx;
            let b = p[y1 * mw + x0] * (1.0 - tx) + p[y1 * mw + x1] * tx;
            out[y * w + x] = a * (1.0 - ty) + b * ty;
        }
    }
    out
}

/// Box mean (integral image).
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut ii = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0f64;
        for x in 0..w {
            row += v[y * w + x] as f64;
            ii[(y + 1) * (w + 1) + x + 1] = ii[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let s = ii[y1 * (w + 1) + x1] - ii[y0 * (w + 1) + x1] - ii[y1 * (w + 1) + x0] + ii[y0 * (w + 1) + x0];
            out[y * w + x] = (s / ((y1 - y0) * (x1 - x0)) as f64) as f32;
        }
    }
    out
}

/// Guided filter (He et al.): aligns mask edges with luminance edges in the photo.
fn guided(guide: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let mi = box_mean(guide, w, h, r);
    let mp = box_mean(p, w, h, r);
    let ip: Vec<f32> = guide.iter().zip(p).map(|(a, b)| a * b).collect();
    let ii: Vec<f32> = guide.iter().map(|a| a * a).collect();
    let mip = box_mean(&ip, w, h, r);
    let mii = box_mean(&ii, w, h, r);
    let mut a = vec![0.0f32; w * h];
    let mut b = vec![0.0f32; w * h];
    for i in 0..w * h {
        let cov = mip[i] - mi[i] * mp[i];
        let var = mii[i] - mi[i] * mi[i];
        a[i] = cov / (var + eps);
        b[i] = mp[i] - a[i] * mi[i];
    }
    let ma = box_mean(&a, w, h, r);
    let mb = box_mean(&b, w, h, r);
    (0..w * h).map(|i| (ma[i] * guide[i] + mb[i]).clamp(0.0, 1.0)).collect()
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Compute a mask. `img`: preview with default settings (long side 1024-1600 recommended).
pub fn compute(img: &Rgba8, kind: AiKind) -> Result<Gray> {
    let (w, h) = (img.w as usize, img.h as usize);
    let model = ai::ai_dir().join("models").join(kind.model().file);
    let prob: Vec<f32> = match kind {
        AiKind::Subject | AiKind::Background => {
            const S: usize = 1024;
            let x = to_tensor(img, S, S);
            let out = ai::with_session(&model, S, |sess, _| {
                let t = ort::value::Tensor::from_array((vec![1i64, 3, S as i64, S as i64], x)).map_err(|e| anyhow!("{e}"))?;
                let o = sess.run(ort::inputs!["input_image" => t]).map_err(|e| anyhow!("{}", trf!("피사체 찾기 실패: {e}", "Subject detection failed: {e}")))?;
                Ok(o[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?.1.to_vec())
            })?;
            // Apply sigmoid if the output is logits
            let raw = out.iter().any(|v| *v < -0.01 || *v > 1.01);
            let m: Vec<f32> = out.into_iter().map(|v| if raw { sigmoid(v) } else { v }).collect();
            upsample(&m, S, S, w, h)
        }
        AiKind::Sky | AiKind::People => {
            // Long side 512, multiple of 32
            let k = 512.0 / w.max(h) as f32;
            let tw = (((w as f32 * k) / 32.0).round() as usize).max(1) * 32;
            let th = (((h as f32 * k) / 32.0).round() as usize).max(1) * 32;
            let x = to_tensor(img, tw, th);
            let cls = if kind == AiKind::Sky { ADE_SKY } else { ADE_PERSON };
            let (shape, logits) = ai::with_session_dims(&model, &[("batch_size", 1), ("num_channels", 3), ("height", th as i64), ("width", tw as i64)], |sess, _| {
                let t = ort::value::Tensor::from_array((vec![1i64, 3, th as i64, tw as i64], x)).map_err(|e| anyhow!("{e}"))?;
                let o = sess.run(ort::inputs!["pixel_values" => t]).map_err(|e| anyhow!("{}", trf!("영역 나누기 실패: {e}", "Segmentation failed: {e}")))?;
                let (s, d) = o[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
                Ok((s.iter().map(|v| *v as usize).collect::<Vec<usize>>(), d.to_vec()))
            })?;
            let (nc, mh, mw) = (shape[1], shape[2], shape[3]);
            let plane = mh * mw;
            let mut p = vec![0.0f32; plane];
            for i in 0..plane {
                let mut mx = f32::MIN;
                for c in 0..nc {
                    mx = mx.max(logits[c * plane + i]);
                }
                let mut sum = 0.0f32;
                for c in 0..nc {
                    sum += (logits[c * plane + i] - mx).exp();
                }
                p[i] = (logits[cls * plane + i] - mx).exp() / sum;
            }
            upsample(&p, mw, mh, w, h)
        }
    };
    // Edge refinement: luminance guide, radius 0.6% of the long side
    let guide: Vec<f32> = img.data.as_chunks::<4>().0.iter().map(|c| (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0).collect();
    let r = ((w.max(h) as f32 * 0.006) as usize).max(2);
    let mut m = guided(&guide, &prob, w, h, r, 1e-3);
    if kind == AiKind::Background {
        m.iter_mut().for_each(|v| *v = 1.0 - *v);
    }
    Ok(Gray { w, h, data: m.into_iter().map(|v| (v * 255.0 + 0.5) as u8).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_keeps_constant_and_png_roundtrip() {
        let (w, h) = (40, 30);
        let g = vec![0.5f32; w * h];
        let p: Vec<f32> = (0..w * h).map(|i| if i % w < 20 { 1.0 } else { 0.0 }).collect();
        let o = guided(&g, &p, w, h, 3, 1e-3);
        assert!(o[5 * w + 2] > 0.9 && o[5 * w + 37] < 0.1);
        let gr = Gray { w, h, data: o.iter().map(|v| (v * 255.0) as u8).collect() };
        let back = Gray::from_png(&gr.to_png()).unwrap();
        assert_eq!(back.data, gr.data);
    }

    /// Real model test (models in DARKROOM_AI_DIR, photo folder in DARKROOM_MASK_IMG).
    #[test]
    #[ignore]
    fn real_masks() {
        let dir = std::env::var("DARKROOM_MASK_IMG").expect("DARKROOM_MASK_IMG");
        let out = std::env::var("DARKROOM_MASK_OUT").ok();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if !p.extension().map(|x| crate::config::is_supported_ext(&x.to_string_lossy())).unwrap_or(false) {
                continue;
            }
            let src = crate::imaging::decode::decode_source(&p, crate::imaging::meta::read_raw_meta(&p).map(|m| m.orientation).unwrap_or(1)).unwrap();
            let img = crate::imaging::decode::render_default_preview(&src, 1280);
            for k in AiKind::ALL {
                let t = std::time::Instant::now();
                let m = compute(&img, k).unwrap();
                let cover = m.data.iter().filter(|v| **v > 127).count() as f32 / m.data.len() as f32;
                eprintln!("{} {:?}: {}x{} 덮음 {:.1}% · {:?}", p.file_name().unwrap().to_string_lossy(), k, m.w, m.h, cover * 100.0, t.elapsed());
                if let Some(o) = &out {
                    let _ = std::fs::create_dir_all(o);
                    let _ = std::fs::write(std::path::Path::new(o).join(format!("{}_{:?}.png", p.file_stem().unwrap().to_string_lossy(), k)), m.to_png());
                }
            }
        }
    }
}
