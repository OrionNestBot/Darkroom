//! Fast single-channel filters. All blurs use running sums, O(n) regardless of radius.

use rayon::prelude::*;

/// Horizontal box blur (edge clamped).
fn box_h(src: &[f32], dst: &mut [f32], w: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    dst.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(d, s)| {
        let last = w - 1;
        let mut acc = s[0] * (r + 1) as f32;
        for i in 1..=r {
            acc += s[i.min(last)];
        }
        for x in 0..w {
            d[x] = acc * norm;
            let add = s[(x + r + 1).min(last)];
            let sub = s[x.saturating_sub(r)];
            acc += add - sub;
        }
    });
}

/// Pointer for writing non-overlapping column ranges of a shared output buffer in parallel.
#[derive(Clone, Copy)]
struct SyncPtr(*mut f32);
unsafe impl Send for SyncPtr {}
unsafe impl Sync for SyncPtr {}

/// Vertical box blur: running sums per column strip. Row-contiguous access lets it auto-vectorize.
fn box_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    let strip = 256usize.min(w).max(1);
    let n_strips = w.div_ceil(strip);
    let out = SyncPtr(dst.as_mut_ptr());
    (0..n_strips).into_par_iter().for_each(|si| {
        let out = out;
        let x0 = si * strip;
        let x1 = (x0 + strip).min(w);
        let sw = x1 - x0;
        let row = |y: usize| &src[y * w + x0..y * w + x1];
        let mut acc: Vec<f32> = row(0).iter().map(|v| v * (r + 1) as f32).collect();
        for i in 1..=r {
            for (a, v) in acc.iter_mut().zip(row(i.min(h - 1))) {
                *a += *v;
            }
        }
        for y in 0..h {
            // SAFETY: each strip writes only columns [x0, x1) and strips never overlap.
            let d = unsafe { std::slice::from_raw_parts_mut(out.0.add(y * w + x0), sw) };
            let add = row((y + r + 1).min(h - 1));
            let sub = row(y.saturating_sub(r));
            for k in 0..sw {
                d[k] = acc[k] * norm;
                acc[k] += add[k] - sub[k];
            }
        }
    });
}

fn alloc(n: usize) -> Vec<f32> {
    vec![0.0f32; n]
}

/// Separable box blur.
pub fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return src.to_vec();
    }
    let mut tmp = alloc(w * h);
    box_h(src, &mut tmp, w, r);
    let mut out = alloc(w * h);
    box_v(&tmp, &mut out, w, h, r);
    out
}

/// Gaussian approximated by three box blurs.
pub fn gauss_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 {
        return src.to_vec();
    }
    if sigma > 4.0 && w > 64 && h > 64 {
        // Large sigmas are computed at half resolution and upsampled (negligible difference)
        let (d, dw, dh) = downsample(src, w, h, 2);
        let b = gauss_blur(&d, dw, dh, sigma * 0.5);
        return upsample(&b, dw, dh, w, h);
    }
    let wi = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let r = (((wi - 1.0) / 2.0).round() as usize).max(1);
    let mut a = alloc(w * h);
    let mut b = alloc(w * h);
    box_h(src, &mut a, w, r);
    box_h(&a, &mut b, w, r);
    box_h(&b, &mut a, w, r);
    box_v(&a, &mut b, w, h, r);
    box_v(&b, &mut a, w, h, r);
    box_v(&a, &mut b, w, h, r);
    b
}

/// Integer-factor average downsample.
pub fn downsample(src: &[f32], w: usize, h: usize, f: usize) -> (Vec<f32>, usize, usize) {
    if f <= 1 {
        return (src.to_vec(), w, h);
    }
    let w2 = w.div_ceil(f);
    let h2 = h.div_ceil(f);
    let mut out = vec![0.0f32; w2 * h2];
    out.par_chunks_mut(w2).enumerate().for_each(|(y2, row)| {
        let ys = y2 * f;
        let ye = (ys + f).min(h);
        for (x2, o) in row.iter_mut().enumerate() {
            let xs = x2 * f;
            let xe = (xs + f).min(w);
            let mut s = 0.0;
            for y in ys..ye {
                let r = &src[y * w + xs..y * w + xe];
                s += r.iter().sum::<f32>();
            }
            *o = s / ((ye - ys) * (xe - xs)) as f32;
        }
    });
    (out, w2, h2)
}

/// Bilinear upsample (small grid to w*h), cell-center aligned.
pub fn upsample(src: &[f32], sw: usize, sh: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let fx = sw as f32 / w as f32;
    let fy = sh as f32 / h as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let y0 = sy as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let ty = sy - y0 as f32;
        let r0 = &src[y0 * sw..y0 * sw + sw];
        let r1 = &src[y1 * sw..y1 * sw + sw];
        for (x, o) in row.iter_mut().enumerate() {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = sx as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let tx = sx - x0 as f32;
            let a = r0[x0] + (r0[x1] - r0[x0]) * tx;
            let b = r1[x0] + (r1[x1] - r1[x0]) * tx;
            *o = a + (b - a) * ty;
        }
    });
    out
}

/// Fast self-guided filter: edge-preserving smoothing.
/// `r`: radius at full resolution, `eps`: edge sensitivity (smaller keeps more edges), `sub`: subsampling factor.
pub fn guided_self(i: &[f32], w: usize, h: usize, r: usize, eps: f32, sub: usize) -> Vec<f32> {
    let sub = sub.max(1).min(r.max(1));
    let (is, ws, hs) = downsample(i, w, h, sub);
    let rs = (r / sub).max(1);
    let mean_i = box_blur(&is, ws, hs, rs);
    let sq: Vec<f32> = is.par_iter().map(|v| v * v).collect();
    let mean_ii = box_blur(&sq, ws, hs, rs);
    let mut a = vec![0.0f32; ws * hs];
    let mut b = vec![0.0f32; ws * hs];
    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .enumerate()
        .for_each(|(k, (av, bv))| {
            let m = mean_i[k];
            let var = (mean_ii[k] - m * m).max(0.0);
            let aa = var / (var + eps);
            *av = aa;
            *bv = m - aa * m;
        });
    let ma = box_blur(&a, ws, hs, rs);
    let mb = box_blur(&b, ws, hs, rs);
    if sub == 1 {
        return i.par_iter().enumerate().map(|(k, v)| ma[k] * v + mb[k]).collect();
    }
    // Upsample and combine in one pass (no full-resolution intermediate buffer)
    let mut q = vec![0.0f32; w * h];
    let fx = ws as f32 / w as f32;
    let fy = hs as f32 / h as f32;
    q.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (hs - 1) as f32);
        let y0 = sy as usize;
        let y1 = (y0 + 1).min(hs - 1);
        let ty = sy - y0 as f32;
        let irow = &i[y * w..y * w + w];
        for (x, o) in row.iter_mut().enumerate() {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (ws - 1) as f32);
            let x0 = sx as usize;
            let x1 = (x0 + 1).min(ws - 1);
            let tx = sx - x0 as f32;
            let bil = |m: &[f32]| {
                let a0 = m[y0 * ws + x0] + (m[y0 * ws + x1] - m[y0 * ws + x0]) * tx;
                let a1 = m[y1 * ws + x0] + (m[y1 * ws + x1] - m[y1 * ws + x0]) * tx;
                a0 + (a1 - a0) * ty
            };
            *o = bil(&ma) * irow[x] + bil(&mb);
        }
    });
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_blur_preserves_constant() {
        let w = 37;
        let h = 23;
        let src = vec![0.5f32; w * h];
        let out = box_blur(&src, w, h, 5);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-5));
        let g = gauss_blur(&src, w, h, 4.0);
        assert!(g.iter().all(|v| (v - 0.5).abs() < 1e-5));
    }

    #[test]
    fn guided_preserves_step_edge() {
        let w = 64;
        let h = 16;
        let src: Vec<f32> = (0..w * h).map(|i| if i % w < 32 { 0.1 } else { 0.9 }).collect();
        let out = guided_self(&src, w, h, 8, 1e-4, 2);
        // Far from edges keep the original value
        assert!((out[8 * w + 4] - 0.1).abs() < 0.02);
        assert!((out[8 * w + 60] - 0.9).abs() < 0.02);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn bench_filters() {
        let (w, h) = (2560usize, 1707usize);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 1000) as f32 / 1000.0).collect();
        for _ in 0..2 {
            let a = 0;
            let t = std::time::Instant::now();
            let _ = box_blur(&src, w, h, 4);
            let b = t.elapsed();
            let t = std::time::Instant::now();
            let _ = gauss_blur(&src, w, h, 2.0);
            let c = t.elapsed();
            let t = std::time::Instant::now();
            let _ = guided_self(&src, w, h, 4, 0.001, 2);
            let d = t.elapsed();
            let t = std::time::Instant::now();
            let _ = downsample(&src, w, h, 2);
            let e = t.elapsed();
            let mut d1 = vec![0.0f32; w * h];
            let mut d2 = vec![0.0f32; w * h];
            box_h(&src, &mut d1, w, 4);
            let t = std::time::Instant::now();
            box_h(&src, &mut d1, w, 4);
            let f = t.elapsed();
            box_v(&src, &mut d2, w, h, 4);
            let t = std::time::Instant::now();
            box_v(&src, &mut d2, w, h, 4);
            let g = t.elapsed();
            let t = std::time::Instant::now();
            let z = vec![0.0f32; w * h];
            let zz: f32 = z.par_chunks(4096).map(|c| c[0]).sum();
            let hh = t.elapsed();
            println!("{a} box {b:?} gauss {c:?} guided {d:?} down {e:?} | box_h(warm) {f:?} box_v(warm) {g:?} alloc+touch {hh:?} {zz}");
        }
    }
}
