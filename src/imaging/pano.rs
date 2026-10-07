//! Panorama merge: several overlapping photos into one source file (.drhdr, linear camera-native RGB).
//! 1) At reduced size: corner features + patch descriptors, match neighboring photos, RANSAC homography
//! 2) Estimate focal length from the homographies, derive camera rotations, cylindrical projection (planar if estimation fails)
//! 3) Per-photo brightness matching, feathered blending at the edges, automatic crop of empty borders

use crate::develop::image::LinearImage;
use anyhow::{Result, anyhow};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}
fn tr(a: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[j][i];
        }
    }
    r
}
fn apply(h: &M3, x: f64, y: f64) -> (f64, f64, f64) {
    (h[0][0] * x + h[0][1] * y + h[0][2], h[1][0] * x + h[1][1] * y + h[1][2], h[2][0] * x + h[2][1] * y + h[2][2])
}
fn apply3(h: &M3, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    (h[0][0] * x + h[0][1] * y + h[0][2] * z, h[1][0] * x + h[1][1] * y + h[1][2] * z, h[2][0] * x + h[2][1] * y + h[2][2] * z)
}
fn inv(m: &M3) -> Option<M3> {
    let d = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if d.abs() < 1e-12 {
        return None;
    }
    let c = |a: usize, b: usize, c: usize, d2: usize| m[a][b] * m[c][d2] - m[a][d2] * m[c][b];
    Some([
        [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
        [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
    ])
}

/// Orthonormalize to a rotation matrix (polar decomposition iteration: R <- (R + R^-T)/2)
fn orthonormalize(m: &M3) -> M3 {
    let mut r = *m;
    for _ in 0..30 {
        let Some(it) = inv(&r) else { break };
        let it = tr(&it);
        for i in 0..3 {
            for j in 0..3 {
                r[i][j] = 0.5 * (r[i][j] + it[i][j]);
            }
        }
    }
    r
}

/// Solve an n x n linear system (Gaussian elimination)
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in 0..n {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..n {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    Some((0..n).map(|i| b[i] / a[i][i]).collect())
}

/// Homography from point correspondences (h33 = 1, least squares)
fn homography(pairs: &[((f64, f64), (f64, f64))]) -> Option<M3> {
    let mut ata = vec![vec![0.0; 8]; 8];
    let mut atb = vec![0.0; 8];
    for &((x, y), (u, v)) in pairs {
        let rows = [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)];
        for (r, t) in rows {
            for i in 0..8 {
                atb[i] += r[i] * t;
                for j in 0..8 {
                    ata[i][j] += r[i] * r[j];
                }
            }
        }
    }
    let h = solve(ata, atb)?;
    Some([[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]])
}

/// Grayscale image for alignment (reduced size, gamma encoded)
struct Gray {
    w: usize,
    h: usize,
    v: Vec<f32>,
    /// Scale relative to the original
    scale: f32,
}

fn gray_of(img: &LinearImage, long: usize) -> Gray {
    let k = (img.w.max(img.h) as f32 / long as f32).max(1.0);
    let (w, h) = ((img.w as f32 / k) as usize, (img.h as f32 / k) as usize);
    let mut v = vec![0.0f32; w * h];
    v.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            // Box-average downscale
            let (x0, y0) = ((x as f32 * k) as usize, (y as f32 * k) as usize);
            let (x1, y1) = (((x + 1) as f32 * k) as usize, ((y + 1) as f32 * k) as usize);
            let mut s = 0.0f32;
            let mut n = 0.0f32;
            for yy in y0..y1.min(img.h) {
                for xx in x0..x1.min(img.w) {
                    let i = (yy * img.w + xx) * 3;
                    s += img.data[i] * 0.3 + img.data[i + 1] * 0.55 + img.data[i + 2] * 0.15;
                    n += 1.0;
                }
            }
            *o = (s / n.max(1.0)).max(0.0).powf(1.0 / 2.2);
        }
    });
    Gray { w, h, v, scale: 1.0 / k }
}

struct Feat {
    x: f32,
    y: f32,
    d: [f32; 64],
}

/// Harris corners + 8x8 patch descriptors (normalized to zero mean and unit length, robust to brightness differences)
fn features(g: &Gray) -> Vec<Feat> {
    let (w, h) = (g.w, g.h);
    let at = |x: usize, y: usize| g.v[y * w + x];
    let mut ixx = vec![0.0f32; w * h];
    let mut iyy = vec![0.0f32; w * h];
    let mut ixy = vec![0.0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = at(x + 1, y) - at(x - 1, y);
            let gy = at(x, y + 1) - at(x, y - 1);
            let i = y * w + x;
            ixx[i] = gx * gx;
            iyy[i] = gy * gy;
            ixy[i] = gx * gy;
        }
    }
    let blur = |a: &[f32]| crate::develop::filters::box_blur(a, w, h, 2);
    let (sxx, syy, sxy) = (blur(&ixx), blur(&iyy), blur(&ixy));
    let resp: Vec<f32> = (0..w * h).map(|i| sxx[i] * syy[i] - sxy[i] * sxy[i] - 0.04 * (sxx[i] + syy[i]).powi(2)).collect();
    let mx = resp.iter().cloned().fold(0.0f32, f32::max);
    // Keep the strongest point per grid cell so features spread evenly
    let cell = 20;
    let margin = 16;
    let mut pts = Vec::new();
    for cy in (margin..h.saturating_sub(margin)).step_by(cell) {
        for cx in (margin..w.saturating_sub(margin)).step_by(cell) {
            let mut best = (0.0f32, 0usize, 0usize);
            for y in cy..(cy + cell).min(h - margin) {
                for x in cx..(cx + cell).min(w - margin) {
                    let r = resp[y * w + x];
                    if r > best.0 {
                        best = (r, x, y);
                    }
                }
            }
            if best.0 > mx * 0.002 {
                pts.push((best.1, best.2));
            }
        }
    }
    let sm = crate::develop::filters::box_blur(&g.v, w, h, 1);
    pts.par_iter()
        .map(|&(x, y)| {
            let mut d = [0.0f32; 64];
            for j in 0..8 {
                for i in 0..8 {
                    let xx = (x as i32 + (i as i32 - 4) * 3 + 1).clamp(0, w as i32 - 1) as usize;
                    let yy = (y as i32 + (j as i32 - 4) * 3 + 1).clamp(0, h as i32 - 1) as usize;
                    d[j * 8 + i] = sm[yy * w + xx];
                }
            }
            let m = d.iter().sum::<f32>() / 64.0;
            let mut n = 0.0;
            for v in d.iter_mut() {
                *v -= m;
                n += *v * *v;
            }
            let n = n.sqrt().max(1e-6);
            for v in d.iter_mut() {
                *v /= n;
            }
            Feat { x: x as f32, y: y as f32, d }
        })
        .collect()
}

/// Descriptor matching: nearest-neighbor ratio test + mutual check
fn match_feats(a: &[Feat], b: &[Feat]) -> Vec<(usize, usize)> {
    let best = |p: &Feat, set: &[Feat]| -> (usize, f32, f32) {
        let mut b1 = (usize::MAX, f32::MAX);
        let mut b2 = f32::MAX;
        for (j, q) in set.iter().enumerate() {
            let mut s = 0.0;
            for k in 0..64 {
                let t = p.d[k] - q.d[k];
                s += t * t;
            }
            if s < b1.1 {
                b2 = b1.1;
                b1 = (j, s);
            } else if s < b2 {
                b2 = s;
            }
        }
        (b1.0, b1.1, b2)
    };
    let ab: Vec<(usize, f32, f32)> = a.par_iter().map(|p| best(p, b)).collect();
    let ba: Vec<usize> = b.par_iter().map(|q| best(q, a).0).collect();
    ab.iter()
        .enumerate()
        .filter(|(i, (j, d1, d2))| *j != usize::MAX && *d1 < 0.64 * *d2 && ba.get(*j) == Some(i))
        .map(|(i, (j, _, _))| (i, *j))
        .collect()
}

/// RANSAC homography (a to b, center-origin coordinates). Returns (H, inlier count)
fn ransac(pa: &[(f64, f64)], pb: &[(f64, f64)], thresh: f64) -> Option<(M3, usize)> {
    let n = pa.len();
    if n < 8 {
        return None;
    }
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut rnd = |m: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % m as u64) as usize
    };
    let inliers = |h: &M3| -> Vec<usize> {
        (0..n)
            .filter(|&i| {
                let (x, y, z) = apply(h, pa[i].0, pa[i].1);
                if z.abs() < 1e-9 {
                    return false;
                }
                let (dx, dy) = (x / z - pb[i].0, y / z - pb[i].1);
                dx * dx + dy * dy < thresh * thresh
            })
            .collect()
    };
    let mut best: Option<(M3, Vec<usize>)> = None;
    for _ in 0..1500 {
        let idx = [rnd(n), rnd(n), rnd(n), rnd(n)];
        if idx[0] == idx[1] || idx[0] == idx[2] || idx[0] == idx[3] || idx[1] == idx[2] || idx[1] == idx[3] || idx[2] == idx[3] {
            continue;
        }
        let pairs: Vec<_> = idx.iter().map(|&i| (pa[i], pb[i])).collect();
        let Some(h) = homography(&pairs) else { continue };
        let inl = inliers(&h);
        if best.as_ref().map(|b| inl.len() > b.1.len()).unwrap_or(true) {
            best = Some((h, inl));
        }
    }
    let (_, inl) = best?;
    if inl.len() < 12 {
        return None;
    }
    // Refit on all inliers (twice)
    let mut h = homography(&inl.iter().map(|&i| (pa[i], pb[i])).collect::<Vec<_>>())?;
    let mut inl2 = inliers(&h);
    if inl2.len() >= 12 {
        h = homography(&inl2.iter().map(|&i| (pa[i], pb[i])).collect::<Vec<_>>())?;
        inl2 = inliers(&h);
    }
    Some((h, inl2.len()))
}

/// Focal length from a rotating-camera homography (center-origin coordinates, standard focals-from-homography formula)
fn focal_from_h(h: &M3) -> Option<f64> {
    let s = h[2][2];
    let h: Vec<f64> = h.iter().flatten().map(|v| v / s).collect();
    let pick = |v1: f64, v2: f64, d1: f64, d2: f64| -> Option<f64> {
        let (v1, v2) = if v1 < v2 { (v2, v1) } else { (v1, v2) };
        if v1 > 0.0 && v2 > 0.0 {
            Some(if d1.abs() > d2.abs() { v1.sqrt() } else { v2.sqrt() })
        } else if v1 > 0.0 {
            Some(v1.sqrt())
        } else {
            None
        }
    };
    let d1 = h[6] * h[7];
    let d2 = (h[7] - h[6]) * (h[7] + h[6]);
    let f1 = pick(-(h[0] * h[1] + h[3] * h[4]) / d1, (h[0] * h[0] + h[3] * h[3] - h[1] * h[1] - h[4] * h[4]) / d2, d1, d2)?;
    let d1 = h[0] * h[3] + h[1] * h[4];
    let d2 = h[0] * h[0] + h[1] * h[1] - h[3] * h[3] - h[4] * h[4];
    let f0 = pick(-h[2] * h[5] / d1, (h[5] * h[5] - h[2] * h[2]) / d2, d1, d2)?;
    let f = (f0 * f1).sqrt();
    f.is_finite().then_some(f)
}

pub struct PanoInput {
    pub path: PathBuf,
    pub orientation: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum Projection {
    #[default]
    Auto,
    Cylindrical,
    Perspective,
}

#[allow(dead_code)]
pub struct PanoResult {
    pub path: PathBuf,
    pub w: usize,
    pub h: usize,
    pub projection: Projection,
}

/// Run the merge (photos in the given order; neighbors must overlap)
pub fn merge(inputs: &[PanoInput], projection: Projection, auto_crop: bool, out: &Path, progress: &dyn Fn(&str)) -> Result<PanoResult> {
    if inputs.len() < 2 {
        return Err(anyhow!("{}", trf!("2장 이상 선택하세요", "Select 2 or more photos")));
    }
    let mut imgs = Vec::new();
    let mut rc = None;
    let mut li = None;
    for (i, p) in inputs.iter().enumerate() {
        progress(&trf!("원본 읽는 중 {}/{}", "Reading originals {}/{}", i + 1, inputs.len()));
        let src = super::decode::decode_source(&p.path, p.orientation)?;
        if i == inputs.len() / 2 {
            rc = src.raw_color.clone();
            li = src.lens_info.clone();
        }
        imgs.push(src.levels[0].clone());
    }
    let rc = rc.ok_or_else(|| anyhow!("{}", trf!("RAW 파일만 병합할 수 있습니다", "Only RAW files can be merged")))?;
    let (img, proj) = stitch(&imgs, projection, auto_crop, progress)?;
    progress(tr!("저장 중", "Saving"));
    let cm = rc.fallback.cm1.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    let hdr = super::hdr::Header {
        w: img.w,
        h: img.h,
        camera: rc.camera.clone(),
        neutral: rc.neutral,
        baseline_exposure: rc.baseline_exposure,
        fallback_cm: [cm[0][0], cm[0][1], cm[0][2], cm[1][0], cm[1][1], cm[1][2], cm[2][0], cm[2][1], cm[2][2]],
        frames: inputs.iter().map(|i| i.path.to_string_lossy().to_string()).collect(),
        ev: vec![],
        lens: li.as_ref().map(|l| l.lens.clone()).unwrap_or_default(),
        make: li.as_ref().map(|l| l.make.clone()).unwrap_or_default(),
        focal: li.as_ref().map(|l| l.focal).unwrap_or(0.0),
        fnumber: li.as_ref().map(|l| l.fnumber).unwrap_or(0.0),
        enhance: "Pano".into(),
    };
    super::hdr::write(out, &hdr, &img)?;
    Ok(PanoResult { path: out.to_path_buf(), w: img.w, h: img.h, projection: proj })
}

/// Align, project and blend (stays in linear RGB)
pub fn stitch(imgs: &[std::sync::Arc<LinearImage>], projection: Projection, auto_crop: bool, progress: &dyn Fn(&str)) -> Result<(LinearImage, Projection)> {
    let n = imgs.len();
    progress(tr!("특징점 찾는 중", "Finding feature points"));
    let grays: Vec<Gray> = imgs.iter().map(|im| gray_of(im, 1600)).collect();
    let feats: Vec<Vec<Feat>> = grays.par_iter().map(features).collect();
    // Neighbor pairs: homography i to i+1 (reduced size, center-origin coordinates)
    let mut hs: Vec<M3> = Vec::new();
    for i in 0..n - 1 {
        progress(&trf!("정렬 중 {}/{}", "Aligning {}/{}", i + 1, n - 1));
        let m = match_feats(&feats[i], &feats[i + 1]);
        let (ga, gb) = (&grays[i], &grays[i + 1]);
        let pa: Vec<(f64, f64)> = m.iter().map(|&(a, _)| ((feats[i][a].x - ga.w as f32 * 0.5) as f64, (feats[i][a].y - ga.h as f32 * 0.5) as f64)).collect();
        let pb: Vec<(f64, f64)> = m.iter().map(|&(_, b)| ((feats[i + 1][b].x - gb.w as f32 * 0.5) as f64, (feats[i + 1][b].y - gb.h as f32 * 0.5) as f64)).collect();
        let (h, ninl) = ransac(&pa, &pb, 2.5).ok_or_else(|| anyhow!("{}", trf!("{}번째와 {}번째 사진의 겹치는 부분을 찾지 못했습니다 (짝 {}개)", "Couldn't find overlap between photo {} and photo {} ({} pairs)", i + 1, i + 2, m.len())))?;
        let _ = ninl;
        hs.push(h);
    }
    // Reduced size to full size, scaled per photo (sizes may differ)
    let full_h: Vec<M3> = hs
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let (sa, sb) = (grays[i].scale as f64, grays[i + 1].scale as f64);
            let a = [[sa, 0.0, 0.0], [0.0, sa, 0.0], [0.0, 0.0, 1.0]];
            let b_inv = [[1.0 / sb, 0.0, 0.0], [0.0, 1.0 / sb, 0.0], [0.0, 0.0, 1.0]];
            mul(&b_inv, &mul(h, &a))
        })
        .collect();
    // Focal length (full-size pixels)
    let mut fs: Vec<f64> = full_h.iter().filter_map(focal_from_h).filter(|f| *f > 100.0 && *f < 1e6).collect();
    fs.sort_by(|a, b| a.total_cmp(b));
    let f = fs.get(fs.len() / 2).copied();
    let proj = match (projection, f) {
        (Projection::Perspective, _) | (_, None) => Projection::Perspective,
        _ => Projection::Cylindrical,
    };
    let refi = n / 2;
    // Transform from each photo to the reference photo
    let mut to_ref: Vec<M3> = vec![[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]; n];
    if proj == Projection::Cylindrical {
        let f = f.unwrap();
        let k = [[f, 0.0, 0.0], [0.0, f, 0.0], [0.0, 0.0, 1.0]];
        let ki = [[1.0 / f, 0.0, 0.0], [0.0, 1.0 / f, 0.0], [0.0, 0.0, 1.0]];
        // R_{i->i+1} = K^-1 H K (normalized)
        let rs: Vec<M3> = full_h.iter().map(|h| orthonormalize(&mul(&ki, &mul(h, &k)))).collect();
        for i in (0..refi).rev() {
            to_ref[i] = mul(&to_ref[i + 1], &rs[i]);
        }
        for i in refi + 1..n {
            to_ref[i] = mul(&to_ref[i - 1], &tr(&rs[i - 1]));
        }
    } else {
        for i in (0..refi).rev() {
            to_ref[i] = mul(&to_ref[i + 1], &full_h[i]);
        }
        for i in refi + 1..n {
            let hi = inv(&full_h[i - 1]).ok_or_else(|| anyhow!("{}", trf!("정렬 계산 실패", "Alignment calculation failed")))?;
            to_ref[i] = mul(&to_ref[i - 1], &hi);
        }
    }
    let fval = f.unwrap_or(1.0);
    // Projection coordinates: cylindrical = (f*theta, f*h), planar = reference photo coordinates (both centered on the reference)
    let fwd = |i: usize, x: f64, y: f64| -> Option<(f64, f64)> {
        let m = &to_ref[i];
        if proj == Projection::Cylindrical {
            let (rx, ry, rz) = apply(m, x / fval, y / fval);
            let r = (rx * rx + rz * rz).sqrt();
            if r < 1e-9 {
                return None;
            }
            Some((fval * rx.atan2(rz), fval * ry / r))
        } else {
            let (u, v, w) = apply(m, x, y);
            (w > 1e-9).then(|| (u / w, v / w))
        }
    };
    // Canvas bounds
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut boxes = Vec::new();
    for (i, im) in imgs.iter().enumerate() {
        let (hw, hh) = (im.w as f64 * 0.5, im.h as f64 * 0.5);
        let (mut bx0, mut by0, mut bx1, mut by1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for k in 0..=20 {
            let t = k as f64 / 20.0;
            for (x, y) in [(-hw + 2.0 * hw * t, -hh), (-hw + 2.0 * hw * t, hh), (-hw, -hh + 2.0 * hh * t), (hw, -hh + 2.0 * hh * t)] {
                if let Some((u, v)) = fwd(i, x, y) {
                    bx0 = bx0.min(u);
                    by0 = by0.min(v);
                    bx1 = bx1.max(u);
                    by1 = by1.max(v);
                }
            }
        }
        boxes.push((bx0, by0, bx1, by1));
        x0 = x0.min(bx0);
        y0 = y0.min(by0);
        x1 = x1.max(bx1);
        y1 = y1.max(by1);
    }
    let (cw, ch) = ((x1 - x0).ceil() as usize, (y1 - y0).ceil() as usize);
    if cw * ch > 400_000_000 || cw == 0 || ch == 0 {
        return Err(anyhow!("{}", trf!("결과가 너무 큽니다 ({cw}×{ch}) — 원근 투영 대신 원통을 쓰거나 사진 수를 줄이세요", "Result is too large ({cw}×{ch}) — use cylindrical instead of perspective, or fewer photos")));
    }
    // Inverse mapping: canvas to photo i coordinates
    let inv_m: Vec<Option<M3>> = to_ref.iter().map(inv).collect();
    let back = |i: usize, u: f64, v: f64| -> Option<(f64, f64)> {
        let mi = inv_m[i].as_ref()?;
        if proj == Projection::Cylindrical {
            let th = u / fval;
            let hh = v / fval;
            let (rx, ry, rz) = apply3(mi, th.sin(), hh, th.cos());
            if rz <= 1e-6 {
                return None;
            }
            Some((fval * rx / rz, fval * ry / rz))
        } else {
            let (x, y, w) = apply(mi, u, v);
            (w.abs() > 1e-9).then(|| (x / w, y / w))
        }
    };
    // Brightness matching: mean luminance ratio in overlaps (between neighbors, reference photo = 1)
    progress(tr!("밝기 맞추는 중", "Matching brightness"));
    let lum_at = |i: usize, x: f64, y: f64| -> Option<f32> {
        let im = &imgs[i];
        let (px, py) = (x + im.w as f64 * 0.5, y + im.h as f64 * 0.5);
        if px < 1.0 || py < 1.0 || px >= im.w as f64 - 1.0 || py >= im.h as f64 - 1.0 {
            return None;
        }
        let j = (py as usize * im.w + px as usize) * 3;
        Some(im.data[j] * 0.3 + im.data[j + 1] * 0.55 + im.data[j + 2] * 0.15)
    };
    let mut gains = vec![1.0f32; n];
    let mut ratio = vec![1.0f32; n.saturating_sub(1)];
    for i in 0..n - 1 {
        let (a0, a1) = (boxes[i].0.max(boxes[i + 1].0), boxes[i].2.min(boxes[i + 1].2));
        let (b0, b1) = (boxes[i].1.max(boxes[i + 1].1), boxes[i].3.min(boxes[i + 1].3));
        let (mut sa, mut sb) = (0.0f64, 0.0f64);
        for k in 0..60 {
            for l in 0..60 {
                let u = a0 + (a1 - a0) * (k as f64 + 0.5) / 60.0;
                let v = b0 + (b1 - b0) * (l as f64 + 0.5) / 60.0;
                if let (Some(p), Some(q)) = (back(i, u, v), back(i + 1, u, v))
                    && let (Some(la), Some(lb)) = (lum_at(i, p.0, p.1), lum_at(i + 1, q.0, q.1))
                        && la > 0.002 && lb > 0.002 && la < 0.9 && lb < 0.9 {
                            sa += la as f64;
                            sb += lb as f64;
                        }
            }
        }
        ratio[i] = if sa > 0.0 && sb > 0.0 { (sa / sb) as f32 } else { 1.0 };
    }
    for i in refi + 1..n {
        gains[i] = gains[i - 1] * ratio[i - 1];
    }
    for i in (0..refi).rev() {
        gains[i] = gains[i + 1] / ratio[i];
    }
    // Blend: weight by distance to the photo edge (feather)
    progress(tr!("합치는 중", "Blending"));
    let mut out = LinearImage::new(cw, ch);
    let mut cover = vec![0u8; cw * ch];
    out.data.par_chunks_mut(cw * 3).zip(cover.par_chunks_mut(cw)).enumerate().for_each(|(yy, (row, cov))| {
        let v = y0 + yy as f64 + 0.5;
        for xx in 0..cw {
            let u = x0 + xx as f64 + 0.5;
            let mut acc = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for i in 0..n {
                let b = boxes[i];
                if u < b.0 || u > b.2 || v < b.1 || v > b.3 {
                    continue;
                }
                let Some((x, y)) = back(i, u, v) else { continue };
                let im = &imgs[i];
                let (px, py) = (x + im.w as f64 * 0.5 - 0.5, y + im.h as f64 * 0.5 - 0.5);
                if px < 0.0 || py < 0.0 || px >= im.w as f64 - 1.0 || py >= im.h as f64 - 1.0 {
                    continue;
                }
                let ex = (px.min(im.w as f64 - 1.0 - px) / im.w as f64) as f32;
                let ey = (py.min(im.h as f64 - 1.0 - py) / im.h as f64) as f32;
                let wgt = (ex.min(ey) * 8.0).min(1.0).powi(2) + 1e-4;
                let (ix, iy) = (px as usize, py as usize);
                let (fx, fy) = ((px - ix as f64) as f32, (py - iy as f64) as f32);
                let s = |xx: usize, yy: usize, c: usize| im.data[(yy * im.w + xx) * 3 + c];
                for c in 0..3 {
                    let top = s(ix, iy, c) * (1.0 - fx) + s(ix + 1, iy, c) * fx;
                    let bot = s(ix, iy + 1, c) * (1.0 - fx) + s(ix + 1, iy + 1, c) * fx;
                    acc[c] += (top * (1.0 - fy) + bot * fy) * gains[i] * wgt;
                }
                wsum += wgt;
            }
            if wsum > 0.0 {
                for c in 0..3 {
                    row[xx * 3 + c] = acc[c] / wsum;
                }
                cov[xx] = 1;
            }
        }
    });
    if !auto_crop {
        return Ok((out, proj));
    }
    // Auto crop: largest rectangle without empty pixels (per-row histogram heights, maximal rectangle)
    progress(tr!("테두리 자르는 중", "Cropping borders"));
    let mut heights = vec![0usize; cw];
    let mut best = (0usize, 0usize, 0usize, 0usize, 0usize); // area, x, y, w, h
    for yy in 0..ch {
        for xx in 0..cw {
            heights[xx] = if cover[yy * cw + xx] == 1 { heights[xx] + 1 } else { 0 };
        }
        let mut st: Vec<usize> = Vec::new();
        for xx in 0..=cw {
            let hcur = if xx < cw { heights[xx] } else { 0 };
            while let Some(&top) = st.last() {
                if heights[top] <= hcur {
                    break;
                }
                st.pop();
                let hgt = heights[top];
                let left = st.last().map(|l| l + 1).unwrap_or(0);
                let area = hgt * (xx - left);
                if area > best.0 {
                    best = (area, left, yy + 1 - hgt, xx - left, hgt);
                }
            }
            st.push(xx);
        }
    }
    let (_, bx, by, bw, bh) = best;
    if bw < 16 || bh < 16 {
        return Ok((out, proj));
    }
    let mut d = Vec::with_capacity(bw * bh * 3);
    for yy in by..by + bh {
        d.extend_from_slice(&out.data[(yy * cw + bx) * 3..(yy * cw + bx + bw) * 3]);
    }
    Ok((LinearImage { w: bw, h: bh, data: d }, proj))
}

/// New file name: <first photo name>-Pano.drhdr
pub fn out_path(first: &Path) -> PathBuf {
    let stem = first.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = dir.join(format!("{stem}-Pano.drhdr"));
    let mut k = 2;
    while out.exists() {
        out = dir.join(format!("{stem}-Pano-{k}.drhdr"));
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render three rotating-camera views of one wide synthetic scene and stitch them back:
    /// checks focal length estimation, seams and brightness matching
    #[test]
    fn stitch_synthetic_rotation() {
        // Scene: a wide textured band on a cylinder (spanning 360 degrees)
        let f = 900.0f64;
        // Scene: multi-octave value noise on cylinder coordinates (non-repeating texture like a real photo)
        let hash = |x: i64, y: i64| -> f64 {
            let mut v = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263)) as u64;
            v = (v ^ (v >> 13)).wrapping_mul(1274126177);
            ((v ^ (v >> 16)) & 0xffff) as f64 / 65535.0
        };
        let noise = |x: f64, y: f64| -> f64 {
            let (xi, yi) = (x.floor() as i64, y.floor() as i64);
            let (fx, fy) = (x - xi as f64, y - yi as f64);
            let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
            let a = hash(xi, yi) * (1.0 - sx) + hash(xi + 1, yi) * sx;
            let b = hash(xi, yi + 1) * (1.0 - sx) + hash(xi + 1, yi + 1) * sx;
            a * (1.0 - sy) + b * sy
        };
        let scene = |th: f64, hgt: f64| -> f32 {
            let (x, y) = (th * f, hgt);
            let v = noise(x / 60.0, y / 60.0) * 0.5 + noise(x / 17.0, y / 17.0) * 0.3 + noise(x / 5.0, y / 5.0) * 0.2;
            (0.03 + 0.6 * v * v) as f32
        };
        let (w, h) = (800usize, 600usize);
        let mut imgs = Vec::new();
        for (k, yaw) in [-0.42f64, 0.0, 0.42].iter().enumerate() {
            let gain = [1.0f32, 1.2, 0.85][k];
            let mut im = LinearImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let (cx, cy) = (x as f64 - w as f64 * 0.5 + 0.5, y as f64 - h as f64 * 0.5 + 0.5);
                    // Camera ray, yaw rotation, then cylinder coordinates
                    let (rx, ry, rz) = (cx / f, cy / f, 1.0);
                    let (c, s) = (yaw.cos(), yaw.sin());
                    let (wx, wz) = (c * rx + s * rz, -s * rx + c * rz);
                    let th = wx.atan2(wz);
                    let hh = ry / (wx * wx + wz * wz).sqrt() * f;
                    let v = scene(th, hh) * gain;
                    let i = (y * w + x) * 3;
                    im.data[i] = v;
                    im.data[i + 1] = v * 0.9;
                    im.data[i + 2] = v * 0.8;
                }
            }
            imgs.push(std::sync::Arc::new(im));
        }
        let (out, proj) = stitch(&imgs, Projection::Auto, true, &|_| {}).unwrap();
        eprintln!("pano {}x{} {:?}", out.w, out.h, proj);
        if let Some(dir) = std::env::var_os("DARKROOM_PANO_DUMP") {
            let px: Vec<u8> = out.data.iter().map(|v| (crate::develop::color::srgb_encode(v.clamp(0.0, 1.0)) * 255.0) as u8).collect();
            image::save_buffer(PathBuf::from(&dir).join("pano_test.png"), &px, out.w as u32, out.h as u32, image::ExtendedColorType::Rgb8).unwrap();
            for (k, im) in imgs.iter().enumerate() {
                let px: Vec<u8> = im.data.iter().map(|v| (crate::develop::color::srgb_encode(v.clamp(0.0, 1.0)) * 255.0) as u8).collect();
                image::save_buffer(PathBuf::from(&dir).join(format!("pano_in{k}.png")), &px, im.w as u32, im.h as u32, image::ExtendedColorType::Rgb8).unwrap();
            }
        }
        assert_eq!(proj, Projection::Cylindrical);
        // Width is about (0.84 rad + field of view) * f
        let fov = 2.0 * ((w as f64 * 0.5) / f).atan();
        let expect = (0.84 + fov) * f;
        assert!(out.w as f64 > expect * 0.8 && (out.w as f64) < expect * 1.1, "w {} expect ~{expect:.0}", out.w);
        // Brightness matching to the middle (reference) photo: the mean of the left and right edge strips should be close to the scene mean
        let col_mean = |x0: usize, x1: usize| -> f32 {
            let mut s = 0.0;
            let mut n = 0.0;
            for y in 0..out.h {
                for x in x0..x1 {
                    s += out.data[(y * out.w + x) * 3];
                    n += 1.0;
                }
            }
            s / n
        };
        let (l, m, r) = (col_mean(0, out.w / 6), col_mean(out.w * 5 / 12, out.w * 7 / 12), col_mean(out.w * 5 / 6, out.w));
        eprintln!("means l {l:.3} m {m:.3} r {r:.3}");
        assert!((l / m - 1.0).abs() < 0.35 && (r / m - 1.0).abs() < 0.35, "{l} {m} {r}");
    }

    /// Test with real photo texture: treat one photo as a planar scene, render three rotated views and stitch them.
    /// Test switch: DARKROOM_PANO_REAL=<RAW> enables it; result PNGs go to the DARKROOM_PANO_DUMP folder
    #[test]
    #[ignore]
    fn stitch_real_texture() {
        let Some(p) = std::env::var_os("DARKROOM_PANO_REAL") else { return };
        let src = crate::imaging::decode::decode_source(Path::new(&p), 1).unwrap();
        let base = &src.levels[1];
        let (bw, bh) = (base.w as f64, base.h as f64);
        let f = bw * 0.9;
        let (w, h) = ((bw * 0.45) as usize, (bh * 0.6) as usize);
        let mut imgs = Vec::new();
        for yaw in [-0.22f64, 0.0, 0.22] {
            let mut im = LinearImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let (cx, cy) = (x as f64 - w as f64 * 0.5, y as f64 - h as f64 * 0.5);
                    let (c, s) = (yaw.cos(), yaw.sin());
                    // Where the rotated ray hits the original (planar) scene
                    let (rx, rz) = (c * cx / f + s, -s * cx / f + c);
                    if rz <= 0.0 {
                        continue;
                    }
                    let (u, v) = (rx / rz * f + bw * 0.5, cy / f / rz * f + bh * 0.5);
                    if u < 0.0 || v < 0.0 || u >= bw - 1.0 || v >= bh - 1.0 {
                        continue;
                    }
                    let j = (v as usize * base.w + u as usize) * 3;
                    let i = (y * w + x) * 3;
                    im.data[i..i + 3].copy_from_slice(&base.data[j..j + 3]);
                }
            }
            imgs.push(std::sync::Arc::new(im));
        }
        let (out, proj) = stitch(&imgs, Projection::Auto, true, &|m| eprintln!("{m}")).unwrap();
        eprintln!("real pano {}x{} {:?} (입력 {w}x{h} 세 장)", out.w, out.h, proj);
        if let Some(dir) = std::env::var_os("DARKROOM_PANO_DUMP") {
            let px: Vec<u8> = out.data.iter().map(|v| (crate::develop::color::srgb_encode((v * 2.5).clamp(0.0, 1.0)) * 255.0) as u8).collect();
            image::save_buffer(PathBuf::from(&dir).join("pano_real.png"), &px, out.w as u32, out.h as u32, image::ExtendedColorType::Rgb8).unwrap();
        }
    }
}
