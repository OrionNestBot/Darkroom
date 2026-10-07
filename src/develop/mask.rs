//! Local mask evaluation. Shapes are computed analytically in normalized source coordinates;
//! only brushes are rasterized to a low-resolution grid and cached.

use super::settings::{BrushStroke, Mask, MaskOp, MaskShape};
use crate::config::BRUSH_MASK_RES;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x >= e1 { 1.0 } else { 0.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub struct BrushRaster {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl BrushRaster {
    #[inline]
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        let x = (u * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let y = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let x0 = x as usize;
        let y0 = y as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let tx = x - x0 as f32;
        let ty = y - y0 as f32;
        let d = &self.data;
        let a = d[y0 * self.w + x0] + (d[y0 * self.w + x1] - d[y0 * self.w + x0]) * tx;
        let b = d[y1 * self.w + x0] + (d[y1 * self.w + x1] - d[y1 * self.w + x0]) * tx;
        a + (b - a) * ty
    }
}

pub fn rasterize_strokes(strokes: &[BrushStroke], bw: usize, bh: usize) -> BrushRaster {
    let long = bw.max(bh) as f32;
    let k = BRUSH_MASK_RES as f32 / long;
    let w = ((bw as f32 * k).round() as usize).max(1);
    let h = ((bh as f32 * k).round() as usize).max(1);
    let mut data = vec![0.0f32; w * h];
    let glong = w.max(h) as f32;
    for s in strokes {
        let r = (s.size * glong).max(0.75);
        let inner = r * (1.0 - s.feather.clamp(0.0, 1.0));
        let spacing = (r * 0.2).max(0.5);
        // Split the polyline into dabs at a fixed spacing
        let mut dabs: Vec<(f32, f32)> = Vec::new();
        let pts: Vec<(f32, f32)> = s.points.iter().map(|p| (p[0] * w as f32, p[1] * h as f32)).collect();
        if pts.is_empty() {
            continue;
        }
        dabs.push(pts[0]);
        let mut carry = 0.0f32;
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            let mut t = spacing - carry;
            while t <= len {
                let f = t / len.max(1e-6);
                dabs.push((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f));
                t += spacing;
            }
            carry = len - (t - spacing);
        }
        // Flow per dab: normalized so the result does not depend on spacing
        let flow = s.flow.clamp(0.0, 1.0);
        let per_dab = 1.0 - (1.0 - flow).powf(spacing / r.max(1.0));
        let per_dab = if flow >= 0.999 { 1.0 } else { per_dab.max(0.01) };
        for (cx, cy) in dabs {
            let x0 = ((cx - r).floor().max(0.0)) as usize;
            let x1 = ((cx + r).ceil() as usize).min(w - 1);
            let y0 = ((cy - r).floor().max(0.0)) as usize;
            let y1 = ((cy + r).ceil() as usize).min(h - 1);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                    if d > r {
                        continue;
                    }
                    let v = (1.0 - smoothstep(inner, r, d)) * per_dab;
                    let m = &mut data[y * w + x];
                    if s.erase {
                        *m *= 1.0 - v;
                    } else {
                        // Flow 1 gives the maximum; otherwise dabs build up
                        *m = if flow >= 0.999 { m.max(v) } else { *m + (1.0 - *m) * v };
                    }
                }
            }
        }
    }
    BrushRaster { w, h, data }
}

fn hash_strokes(strokes: &[BrushStroke], bw: usize, bh: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (bw, bh, strokes.len()).hash(&mut h);
    for s in strokes {
        s.size.to_bits().hash(&mut h);
        s.feather.to_bits().hash(&mut h);
        s.flow.to_bits().hash(&mut h);
        s.erase.hash(&mut h);
        s.points.len().hash(&mut h);
        for p in &s.points {
            p[0].to_bits().hash(&mut h);
            p[1].to_bits().hash(&mut h);
        }
    }
    h.finish()
}

/// Brush raster cache owned by the render thread.
#[derive(Default)]
pub struct BrushCache {
    map: HashMap<u64, Arc<BrushRaster>>,
}

impl BrushCache {
    pub fn get(&mut self, strokes: &[BrushStroke], bw: usize, bh: usize) -> Arc<BrushRaster> {
        let key = hash_strokes(strokes, bw, bh);
        if let Some(r) = self.map.get(&key) {
            return r.clone();
        }
        if self.map.len() > 32 {
            self.map.clear();
        }
        let r = Arc::new(rasterize_strokes(strokes, bw, bh));
        self.map.insert(key, r.clone());
        r
    }
}

/// Prepared mask for per-pixel evaluation.
pub struct PreparedMask {
    pub comps: Vec<(MaskOp, bool, PreparedShape)>,
    pub invert: bool,
    pub amount: f32,
}

pub enum PreparedShape {
    Brush(Arc<BrushRaster>),
    Linear { p0: (f32, f32), d: (f32, f32), inv_len2: f32 },
    Radial { c: (f32, f32), rx: f32, ry: f32, cos: f32, sin: f32, inner: f32 },
    Luminance { lo: f32, hi: f32, s: f32 },
    Color { hue: f32, range: f32 },
    All,
}

pub fn prepare(mask: &Mask, bw: usize, bh: usize, cache: &mut BrushCache) -> PreparedMask {
    let (fw, fh) = (bw as f32, bh as f32);
    let comps = mask
        .components
        .iter()
        .map(|c| {
            let ps = match &c.shape {
                MaskShape::Brush { strokes } => PreparedShape::Brush(cache.get(strokes, bw, bh)),
                MaskShape::Linear { p0, p1 } => {
                    let a = (p0[0] * fw, p0[1] * fh);
                    let d = (p1[0] * fw - a.0, p1[1] * fh - a.1);
                    let l2 = (d.0 * d.0 + d.1 * d.1).max(1e-6);
                    PreparedShape::Linear { p0: a, d, inv_len2: 1.0 / l2 }
                }
                MaskShape::Radial { center, radius, angle, feather } => {
                    let (s, co) = angle.to_radians().sin_cos();
                    PreparedShape::Radial {
                        c: (center[0] * fw, center[1] * fh),
                        rx: (radius[0] * fw).max(1.0),
                        ry: (radius[1] * fh).max(1.0),
                        cos: co,
                        sin: s,
                        inner: 1.0 - (feather / 100.0).clamp(0.0, 1.0),
                    }
                }
                MaskShape::Luminance { lo, hi, smooth } => PreparedShape::Luminance { lo: *lo, hi: *hi, s: smooth.max(0.001) },
                MaskShape::Color { hue, range, .. } => PreparedShape::Color { hue: *hue, range: range.max(1.0) },
                MaskShape::All => PreparedShape::All,
                MaskShape::Ai { key, .. } => PreparedShape::Brush(super::aistore::get(key).unwrap_or_else(|| Arc::new(BrushRaster { w: 1, h: 1, data: vec![0.0] }))),
            };
            (c.op, c.invert, ps)
        })
        .collect();
    PreparedMask { comps, invert: mask.invert, amount: mask.amount }
}

impl PreparedMask {
    /// bx, by: source pixel coordinates. lum: gamma luminance (0..1), hue/chroma: source color.
    #[inline]
    pub fn eval(&self, bx: f32, by: f32, bw: f32, bh: f32, lum: f32, hue: f32, chroma: f32) -> f32 {
        let mut m = 0.0f32;
        let mut first = true;
        for (op, inv, shape) in &self.comps {
            let mut v = match shape {
                PreparedShape::Brush(r) => r.sample(bx / bw, by / bh),
                PreparedShape::Linear { p0, d, inv_len2 } => {
                    let t = ((bx - p0.0) * d.0 + (by - p0.1) * d.1) * inv_len2;
                    1.0 - smoothstep(0.0, 1.0, t)
                }
                PreparedShape::Radial { c, rx, ry, cos, sin, inner } => {
                    let dx = bx - c.0;
                    let dy = by - c.1;
                    let lx = (cos * dx + sin * dy) / rx;
                    let ly = (-sin * dx + cos * dy) / ry;
                    let t = (lx * lx + ly * ly).sqrt();
                    1.0 - smoothstep(*inner, 1.0, t)
                }
                PreparedShape::Luminance { lo, hi, s } => smoothstep(lo - s, *lo, lum) * (1.0 - smoothstep(*hi, hi + s, lum)),
                PreparedShape::Color { hue: h0, range } => {
                    let mut dh = (hue - h0).abs() % 360.0;
                    if dh > 180.0 {
                        dh = 360.0 - dh;
                    }
                    (1.0 - smoothstep(range * 0.5, *range, dh)) * smoothstep(0.02, 0.08, chroma)
                }
                PreparedShape::All => 1.0,
            };
            if *inv {
                v = 1.0 - v;
            }
            if first {
                m = match op {
                    MaskOp::Subtract => 0.0,
                    _ => v,
                };
                first = false;
            } else {
                m = match op {
                    MaskOp::Add => m.max(v),
                    MaskOp::Subtract => m * (1.0 - v),
                    MaskOp::Intersect => m * v,
                };
            }
        }
        if self.invert {
            m = 1.0 - m;
        }
        m * self.amount
    }

    pub fn needs_color(&self) -> bool {
        self.comps
            .iter()
            .any(|(_, _, s)| matches!(s, PreparedShape::Luminance { .. } | PreparedShape::Color { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::settings::{MaskComponent, MaskShape};

    #[test]
    fn radial_center_full_outside_zero() {
        let mask = Mask {
            components: vec![MaskComponent {
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: [0.5, 0.5], radius: [0.2, 0.2], angle: 0.0, feather: 50.0 },
            }],
            ..Default::default()
        };
        let mut c = BrushCache::default();
        let p = prepare(&mask, 100, 100, &mut c);
        assert!((p.eval(50.0, 50.0, 100.0, 100.0, 0.5, 0.0, 0.0) - 1.0).abs() < 1e-4);
        assert!(p.eval(95.0, 50.0, 100.0, 100.0, 0.5, 0.0, 0.0) < 1e-4);
    }

    #[test]
    fn brush_paints_and_erases() {
        let stroke = BrushStroke { points: vec![[0.2, 0.5], [0.8, 0.5]], size: 0.05, feather: 0.2, flow: 1.0, erase: false };
        let r = rasterize_strokes(&[stroke.clone()], 1000, 500);
        assert!(r.sample(0.5, 0.5) > 0.95);
        assert!(r.sample(0.5, 0.1) < 0.01);
        let mut e = stroke;
        e.erase = true;
        e.points = vec![[0.5, 0.5], [0.5, 0.5]];
        let r2 = rasterize_strokes(&[r_to_stroke(), e], 1000, 500);
        assert!(r2.sample(0.5, 0.5) < 0.05);
    }

    fn r_to_stroke() -> BrushStroke {
        BrushStroke { points: vec![[0.2, 0.5], [0.8, 0.5]], size: 0.05, feather: 0.2, flow: 1.0, erase: false }
    }
}
