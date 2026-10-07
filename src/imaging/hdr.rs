//! HDR merge: several bracketed RAW exposures → one high-dynamic-range original (.drhdr).
//! Merges in camera-native RGB (before white balance) and stores the camera color info,
//! so the result goes through the same DNG-style color processing and develop tools as a normal RAW.
//!
//! File format: "DRHDR1" + u32 (JSON length) + JSON header + zlib(log-encoded u16 RGB)

use crate::develop::image::LinearImage;
use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 6] = b"DRHDR1";
/// Log encoding range: 2^LOG_MIN .. 2^(LOG_MIN+LOG_SPAN)
const LOG_MIN: f32 = -20.0;
const LOG_SPAN: f32 = 28.0;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Header {
    pub w: usize,
    pub h: usize,
    pub camera: String,
    pub neutral: [f64; 3],
    pub baseline_exposure: f64,
    /// rawler color matrix (for the fallback profile)
    pub fallback_cm: [f64; 9],
    pub frames: Vec<String>,
    /// Exposure of each frame relative to the reference (EV)
    pub ev: Vec<f32>,
    pub lens: String,
    pub make: String,
    pub focal: f32,
    pub fnumber: f32,
    /// "Enhanced-NR"/"Enhanced-SR" for AI enhance results (empty = HDR merge)
    #[serde(default)]
    pub enhance: String,
}

fn enc(v: f32) -> u16 {
    if v <= 0.0 {
        return 0;
    }
    let t = ((v.log2() - LOG_MIN) / LOG_SPAN).clamp(0.0, 1.0);
    (t * 65534.0 + 1.0).round() as u16
}

fn dec(c: u16) -> f32 {
    if c == 0 {
        return 0.0;
    }
    ((c as f32 - 1.0) / 65534.0 * LOG_SPAN + LOG_MIN).exp2()
}

pub fn write(path: &Path, hdr: &Header, img: &LinearImage) -> Result<()> {
    let json = serde_json::to_vec(hdr)?;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).with_context(|| trf!("저장 실패: {}", "Couldn't save: {}", path.display()))?);
    f.write_all(MAGIC)?;
    f.write_all(&(json.len() as u32).to_le_bytes())?;
    f.write_all(&json)?;
    // Row-wise delta (neighbors in the same channel) for better compression
    let codes: Vec<u16> = img.data.par_iter().map(|v| enc(*v)).collect();
    let w3 = img.w * 3;
    let mut bytes = Vec::with_capacity(codes.len() * 2);
    for row in codes.chunks(w3) {
        let mut prev = [0u16; 3];
        for (i, c) in row.iter().enumerate() {
            let d = c.wrapping_sub(prev[i % 3]);
            prev[i % 3] = *c;
            bytes.extend_from_slice(&d.to_le_bytes());
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(f, flate2::Compression::fast());
    z.write_all(&bytes)?;
    z.finish()?;
    Ok(())
}

/// Reads only the header (for size checks)
pub fn read_header(path: &Path) -> Result<Header> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 10];
    f.read_exact(&mut head)?;
    if &head[..6] != MAGIC {
        return Err(anyhow!("{}", trf!("HDR 파일 형식이 아님", "Not an HDR file")));
    }
    let jl = u32::from_le_bytes(head[6..10].try_into().unwrap()) as usize;
    let mut json = vec![0u8; jl];
    f.read_exact(&mut json)?;
    Ok(serde_json::from_slice(&json)?)
}

pub fn read(path: &Path) -> Result<(Header, LinearImage)> {
    let data = std::fs::read(path)?;
    if data.len() < 10 || &data[..6] != MAGIC {
        return Err(anyhow!("{}", trf!("HDR 파일 형식이 아님", "Not an HDR file")));
    }
    let jl = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;
    let hdr: Header = serde_json::from_slice(&data[10..10 + jl])?;
    let mut raw = Vec::with_capacity(hdr.w * hdr.h * 6);
    flate2::read::ZlibDecoder::new(&data[10 + jl..]).read_to_end(&mut raw)?;
    let w3 = hdr.w * 3;
    if raw.len() < hdr.w * hdr.h * 6 {
        return Err(anyhow!("{}", trf!("HDR 데이터가 잘림", "HDR data is truncated")));
    }
    let mut out = vec![0.0f32; hdr.w * hdr.h * 3];
    out.par_chunks_mut(w3).enumerate().for_each(|(y, row)| {
        let src = &raw[y * w3 * 2..(y + 1) * w3 * 2];
        let mut prev = [0u16; 3];
        for i in 0..w3 {
            let d = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]);
            let c = prev[i % 3].wrapping_add(d);
            prev[i % 3] = c;
            row[i] = dec(c);
        }
    });
    let (w, h) = (hdr.w, hdr.h);
    Ok((hdr, LinearImage { w, h, data: out }))
}

/// Low-resolution log luminance (for alignment and exposure ratios)
fn small_luma(img: &LinearImage, k: usize) -> (Vec<f32>, usize, usize) {
    let (w, h) = (img.w / k, img.h / k);
    let mut v = vec![0.0f32; w * h];
    v.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut s = 0.0;
            for dy in 0..k {
                for dx in 0..k {
                    let i = ((y * k + dy) * img.w + x * k + dx) * 3;
                    s += img.data[i] + img.data[i + 1] + img.data[i + 2];
                }
            }
            row[x] = s / (3 * k * k) as f32;
        }
    });
    (v, w, h)
}

/// Offset from the reference (source pixels): minimize gradient-magnitude SAD at low resolution, then refine at high resolution
fn align(reference: &LinearImage, img: &LinearImage, ratio: f32) -> (i32, i32) {
    let grad = |v: &[f32], w: usize, h: usize, scale: f32| -> Vec<f32> {
        let l: Vec<f32> = v.iter().map(|x| (x * scale).max(1e-5).log2()).collect();
        let mut g = vec![0.0f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                g[i] = (l[i + 1] - l[i - 1]).abs() + (l[i + w] - l[i - w]).abs();
            }
        }
        g
    };
    let mut best = (0i32, 0i32);
    for (k, range) in [(16usize, 12i32), (4, 6), (1, 5)] {
        let (a, w, h) = small_luma(reference, k);
        let (b, _, _) = small_luma(img, k);
        if w < 16 || h < 16 {
            continue;
        }
        let ga = grad(&a, w, h, 1.0);
        let gb = grad(&b, w, h, 1.0 / ratio);
        let base = if k == 16 { (0, 0) } else { (best.0 / k as i32, best.1 / k as i32) };
        let m = (range + 2) as usize;
        let cands: Vec<(i32, i32)> = (-range..=range).flat_map(|dy| (-range..=range).map(move |dx| (base.0 + dx, base.1 + dy))).collect();
        let scored: Vec<(f32, (i32, i32))> = cands
            .par_iter()
            .map(|&(dx, dy)| {
                let mut s = 0.0f32;
                let mut n = 0.0f32;
                let step = if k == 1 { 3 } else { 1 };
                let mut y = m + 1;
                while y < h - m - 1 {
                    let mut x = m + 1;
                    while x < w - m - 1 {
                        let yb = (y as i32 + dy) as usize;
                        let xb = (x as i32 + dx) as usize;
                        if yb < h && xb < w {
                            s += (ga[y * w + x] - gb[yb * w + xb]).abs();
                            n += 1.0;
                        }
                        x += step;
                    }
                    y += step;
                }
                (s / n.max(1.0), (dx, dy))
            })
            .collect();
        let b = scored.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1).unwrap_or(base);
        best = (b.0 * k as i32, b.1 * k as i32);
    }
    best
}

/// Median brightness ratio (img / reference) over pixels valid in both
fn exposure_ratio(reference: &LinearImage, img: &LinearImage) -> f32 {
    let (a, _, _) = small_luma(reference, 8);
    let (b, _, _) = small_luma(img, 8);
    let mut r: Vec<f32> = a.iter().zip(&b).filter(|(x, y)| **x > 0.02 && **x < 0.7 && **y > 0.02 && **y < 0.7).map(|(x, y)| y / x).collect();
    if r.len() < 50 {
        return 1.0;
    }
    let m = r.len() / 2;
    *r.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1
}

pub struct MergeInput {
    pub path: PathBuf,
    pub orientation: u16,
}

/// Runs the merge. Returns the saved file path
pub fn merge(inputs: &[MergeInput], out: &Path, progress: &dyn Fn(&str)) -> Result<PathBuf> {
    if inputs.len() < 2 {
        return Err(anyhow!("{}", trf!("2장 이상 선택하세요", "Select 2 or more photos")));
    }
    let mut frames = Vec::new();
    for (i, inp) in inputs.iter().enumerate() {
        progress(&trf!("원본 읽는 중 {}/{}", "Reading originals {}/{}", i + 1, inputs.len()));
        let src = super::decode::decode_source(&inp.path, inp.orientation)?;
        let rc = src.raw_color.clone().ok_or_else(|| anyhow!("{}", trf!("{}: RAW 파일만 병합할 수 있습니다", "{}: only RAW files can be merged", inp.path.display())))?;
        let li = src.lens_info.clone();
        let base = src.levels[0].clone();
        frames.push((base, rc, li));
    }
    let (w, h) = (frames[0].0.w, frames[0].0.h);
    if frames.iter().any(|f| f.0.w != w || f.0.h != h) {
        return Err(anyhow!("{}", trf!("크기가 다른 사진은 병합할 수 없습니다", "Photos of different sizes can't be merged")));
    }
    // Reference: the frame with the median average brightness
    let mut means: Vec<(usize, f32)> = frames.iter().enumerate().map(|(i, f)| (i, small_luma(&f.0, 16).0.iter().sum::<f32>())).collect();
    means.sort_by(|a, b| a.1.total_cmp(&b.1));
    let ri = means[means.len() / 2].0;
    let reference = frames[ri].0.clone();
    let mut ratios = vec![1.0f32; frames.len()];
    let mut shifts = vec![(0i32, 0i32); frames.len()];
    for i in 0..frames.len() {
        if i == ri {
            continue;
        }
        progress(&trf!("정렬 중 {}/{}", "Aligning {}/{}", i + 1, frames.len()));
        ratios[i] = exposure_ratio(&reference, &frames[i].0);
        shifts[i] = align(&reference, &frames[i].0, ratios[i]);
    }
    progress(tr!("합성 중", "Merging"));
    let clip = 0.92f32;
    let mut out_img = LinearImage::new(w, h);
    let fr: Vec<&LinearImage> = frames.iter().map(|f| f.0.as_ref()).collect();
    out_img.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let ri_px = {
                let i = (y * w + x) * 3;
                [reference.data[i], reference.data[i + 1], reference.data[i + 2]]
            };
            let ref_ok = ri_px.iter().all(|v| *v < clip);
            let ref_l = (ri_px[0] + ri_px[1] + ri_px[2]) / 3.0;
            let mut acc = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for (k, img) in fr.iter().enumerate() {
                let (sx, sy) = shifts[k];
                let (xx, yy) = (x as i32 + sx, y as i32 + sy);
                if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                    continue;
                }
                let i = (yy as usize * w + xx as usize) * 3;
                let p = [img.data[i], img.data[i + 1], img.data[i + 2]];
                let mx = p[0].max(p[1]).max(p[2]);
                if mx >= clip {
                    continue;
                }
                // Trust mid-tones most (avoids noise and clipping); brighter exposures have less noise
                let t = (mx / clip).clamp(0.0, 1.0);
                let mut wgt = (1.0 - (2.0 * t - 1.0).powi(8)).max(0.0) * ratios[k].sqrt();
                let q = [p[0] / ratios[k], p[1] / ratios[k], p[2] / ratios[k]];
                // Ghost suppression: where a frame differs strongly from the reference (moving objects), prefer the reference
                if ref_ok && k != ri && ref_l > 0.003 {
                    let ql = (q[0] + q[1] + q[2]) / 3.0;
                    let d = (ql.max(1e-5) / ref_l.max(1e-5)).log2().abs();
                    if d > 0.6 {
                        wgt *= (1.0 - (d - 0.6) * 2.0).max(0.02);
                    }
                }
                if wgt <= 0.0 {
                    continue;
                }
                for c in 0..3 {
                    acc[c] += q[c] * wgt;
                }
                wsum += wgt;
            }
            let o = &mut row[x * 3..x * 3 + 3];
            if wsum > 1e-6 {
                for c in 0..3 {
                    o[c] = acc[c] / wsum;
                }
            } else {
                // All frames clipped: use the darkest frame's value, exposure-compensated
                let (di, _) = ratios.iter().enumerate().fold((0, f32::MAX), |a, (i, r)| if *r < a.1 { (i, *r) } else { a });
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    o[c] = fr[di].data[i + c] / ratios[di];
                }
            }
        }
    });
    let rc = &frames[ri].1;
    let li = frames[ri].2.clone();
    let cm = rc.fallback.cm1.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    let hdr = Header {
        w,
        h,
        camera: rc.camera.clone(),
        neutral: rc.neutral,
        baseline_exposure: rc.baseline_exposure,
        fallback_cm: [cm[0][0], cm[0][1], cm[0][2], cm[1][0], cm[1][1], cm[1][2], cm[2][0], cm[2][1], cm[2][2]],
        frames: inputs.iter().map(|i| i.path.to_string_lossy().to_string()).collect(),
        ev: ratios.iter().map(|r| r.log2()).collect(),
        lens: li.as_ref().map(|l| l.lens.clone()).unwrap_or_default(),
        make: li.as_ref().map(|l| l.make.clone()).unwrap_or_default(),
        focal: li.as_ref().map(|l| l.focal).unwrap_or(0.0),
        fnumber: li.as_ref().map(|l| l.fnumber).unwrap_or(0.0),
        enhance: String::new(),
    };
    progress(tr!("저장 중", "Saving"));
    write(out, &hdr, &out_img)?;
    Ok(out.to_path_buf())
}

/// .drhdr → (linear image, color info)
pub fn load_source(path: &Path) -> Result<(LinearImage, crate::develop::dcp::RawColor, Header)> {
    let (hdr, img) = read(path)?;
    let c = hdr.fallback_cm;
    let fallback = crate::develop::dcp::Profile {
        name: "Matrix".into(),
        cm1: Some([[c[0], c[1], c[2]], [c[3], c[4], c[5]], [c[6], c[7], c[8]]]),
        illum1: 21,
        ..Default::default()
    };
    let rc = crate::develop::dcp::RawColor {
        camera: hdr.camera.clone(),
        neutral: hdr.neutral,
        baseline_exposure: hdr.baseline_exposure,
        embedded: None,
        fallback: std::sync::Arc::new(fallback),
        // AI enhance results share the original's range (and clipping handling)
        hdr: hdr.enhance.is_empty(),
        decoder_fallback: None,
    };
    Ok((img, rc, hdr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Merging two copies of the same RAW must equal the original (offset 0, exposure ratio 1). DARKROOM_PROBE_FILE=<raw>
    #[test]
    #[ignore]
    fn merge_identity() {
        let Some(p) = std::env::var_os("DARKROOM_PROBE_FILE") else { return };
        let p = PathBuf::from(p);
        let o = crate::imaging::meta::read_raw_meta(&p).map(|m| m.orientation).unwrap_or(1);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("t.drhdr");
        let t = std::time::Instant::now();
        let inputs = [MergeInput { path: p.clone(), orientation: o }, MergeInput { path: p.clone(), orientation: o }];
        merge(&inputs, &out, &|m| println!("  {m}")).unwrap();
        println!("merge {:?}, size {} MB", t.elapsed(), std::fs::metadata(&out).unwrap().len() / 1_000_000);
        let (img, rc, h) = load_source(&out).unwrap();
        let src = crate::imaging::decode::decode_source(&p, o).unwrap();
        println!("ev {:?} camera {}", h.ev, rc.camera);
        let a = &src.levels[0].data;
        let mut maxd = 0.0f32;
        for (x, y) in a.iter().zip(&img.data).step_by(97) {
            if *x > 0.001 && *x < 0.9 {
                maxd = maxd.max((x - y).abs() / x);
            }
        }
        assert!(maxd < 0.01, "최대 상대 오차 {maxd}");
    }

    #[test]
    fn log_codec_roundtrip() {
        for v in [1e-5f32, 0.001, 0.18, 1.0, 7.5, 120.0] {
            let d = dec(enc(v));
            assert!((d / v - 1.0).abs() < 0.001, "{v} → {d}");
        }
        assert_eq!(dec(enc(0.0)), 0.0);
    }

    #[test]
    fn file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.drhdr");
        let mut img = LinearImage::new(17, 9);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = (i as f32 * 0.37).sin().abs() * 3.0;
        }
        let hdr = Header {
            w: 17,
            h: 9,
            camera: "X".into(),
            neutral: [0.5, 1.0, 0.6],
            baseline_exposure: 0.0,
            fallback_cm: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            frames: vec![],
            ev: vec![],
            lens: String::new(),
            make: String::new(),
            focal: 0.0,
            fnumber: 0.0,
            enhance: String::new(),
        };
        write(&p, &hdr, &img).unwrap();
        let (h2, img2) = read(&p).unwrap();
        assert_eq!(h2.w, 17);
        for (a, b) in img.data.iter().zip(&img2.data) {
            assert!((a - b).abs() <= a.abs() * 0.002 + 1e-6);
        }
    }
}
