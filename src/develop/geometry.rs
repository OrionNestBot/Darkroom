//! Maps output pixels to source pixel coordinates (in the base orientation).
//! Forward order: source -> lens distortion correction -> flip -> 90° rotation -> perspective/scale -> fine rotation -> crop -> view region (zoom).
//! The renderer computes the source coordinate backwards for each output pixel and resamples only once.

use super::settings::{Geometry, LensCorrection};

#[derive(Clone, Debug)]
pub struct GeoMap {
    pub bw: f32,
    pub bh: f32,
    pub fw: f32,
    pub fh: f32,
    crop: [f32; 4],
    region: [f32; 4],
    pub ow: f32,
    pub oh: f32,
    cos: f32,
    sin: f32,
    rot90: u8,
    flip_h: bool,
    flip_v: bool,
    pv: f32,
    ph: f32,
    scale: f32,
    dist_k: f32,
    /// Output pixels per source pixel.
    pub out_per_base: f32,
    /// Lens profile distortion model.
    lens_model: Option<std::sync::Arc<super::lensmodel::LensModel>>,
}

impl GeoMap {
    pub fn new(bw: usize, bh: usize, g: &Geometry, lens: &LensCorrection, region: [f32; 4], ow: usize, oh: usize) -> Self {
        let (bw, bh) = (bw as f32, bh as f32);
        let rot90 = g.rotate90 % 4;
        let (fw, fh) = if rot90 % 2 == 1 { (bh, bw) } else { (bw, bh) };
        let (sin, cos) = g.angle.to_radians().sin_cos();
        let scale = (g.scale / 100.0).max(0.1);
        let cw = (g.crop[2] - g.crop[0]).max(1e-4);
        let rw = (region[2] - region[0]).max(1e-6);
        let out_per_base = ow as f32 / (rw * cw * fw / scale);
        Self {
            bw,
            bh,
            fw,
            fh,
            crop: g.crop,
            region,
            ow: ow as f32,
            oh: oh as f32,
            cos,
            sin,
            rot90,
            flip_h: g.flip_h,
            flip_v: g.flip_v,
            pv: g.vertical / 100.0 * 0.35,
            ph: g.horizontal / 100.0 * 0.35,
            scale,
            dist_k: -lens.distortion / 100.0 * 0.25,
            out_per_base,
            lens_model: None,
        }
    }

    pub fn with_lens(mut self, lm: Option<std::sync::Arc<super::lensmodel::LensModel>>) -> Self {
        self.lens_model = lm;
        self
    }

    /// Output pixel (center coordinates) -> normalized crop coordinates.
    #[inline]
    pub fn out_to_cropnorm(&self, ox: f32, oy: f32) -> (f32, f32) {
        let r = &self.region;
        (r[0] + ox / self.ow * (r[2] - r[0]), r[1] + oy / self.oh * (r[3] - r[1]))
    }

    #[inline]
    pub fn cropnorm_to_frame(&self, cx: f32, cy: f32) -> (f32, f32) {
        let c = &self.crop;
        ((c[0] + cx * (c[2] - c[0])) * self.fw, (c[1] + cy * (c[3] - c[1])) * self.fh)
    }

    /// Frame pixel (before rotation/crop) -> source pixel.
    #[inline]
    pub fn frame_to_base(&self, fx: f32, fy: f32) -> (f32, f32) {
        let dx = fx - self.fw * 0.5;
        let dy = fy - self.fh * 0.5;
        // Inverse fine rotation
        let rx = self.cos * dx + self.sin * dy;
        let ry = -self.sin * dx + self.cos * dy;
        // Scale + perspective (keystone)
        let hs = 0.5 * self.fw.max(self.fh);
        let mut qx = rx / hs / self.scale;
        let mut qy = ry / hs / self.scale;
        if self.pv != 0.0 || self.ph != 0.0 {
            let w = 1.0 + self.pv * qy + self.ph * qx;
            let w = if w.abs() < 0.05 { 0.05f32.copysign(w) } else { w };
            qx /= w;
            qy /= w;
        }
        let mut x = qx * hs + self.fw * 0.5;
        let mut y = qy * hs + self.fh * 0.5;
        // Inverse 90° rotation
        let (mut cw, mut ch) = (self.fw, self.fh);
        for _ in 0..self.rot90 {
            let (px, py) = (y, cw - x);
            x = px;
            y = py;
            std::mem::swap(&mut cw, &mut ch);
        }
        // Flip
        if self.flip_h {
            x = self.bw - x;
        }
        if self.flip_v {
            y = self.bh - y;
        }
        // Lens distortion
        if self.dist_k != 0.0 {
            let hd = 0.5 * (self.bw * self.bw + self.bh * self.bh).sqrt();
            let dx = (x - self.bw * 0.5) / hd;
            let dy = (y - self.bh * 0.5) / hd;
            let f = 1.0 + self.dist_k * (dx * dx + dy * dy);
            x = self.bw * 0.5 + dx * f * hd;
            y = self.bh * 0.5 + dy * f * hd;
        }
        // Lens profile distortion (corrected -> actual coordinates)
        if let Some(lm) = &self.lens_model {
            let (lx, ly) = lm.distort(x, y);
            x = lx;
            y = ly;
        }
        (x, y)
    }

    /// Output pixel -> source pixel.
    #[inline]
    pub fn map(&self, ox: f32, oy: f32) -> (f32, f32) {
        let (cx, cy) = self.out_to_cropnorm(ox, oy);
        let (fx, fy) = self.cropnorm_to_frame(cx, cy);
        self.frame_to_base(fx, fy)
    }

    #[inline]
    pub fn in_bounds(&self, bx: f32, by: f32) -> bool {
        bx >= -0.5 && by >= -0.5 && bx <= self.bw + 0.5 && by <= self.bh + 0.5
    }

    /// Source pixel -> output pixel (inverted with Newton iteration); used for UI overlays.
    pub fn inverse(&self, bx: f32, by: f32) -> (f32, f32) {
        let mut o = (self.ow * 0.5, self.oh * 0.5);
        let h = 0.5f32;
        for _ in 0..24 {
            let (mx, my) = self.map(o.0, o.1);
            let ex = mx - bx;
            let ey = my - by;
            if ex * ex + ey * ey < 1e-4 {
                break;
            }
            let (ax, ay) = self.map(o.0 + h, o.1);
            let (cx, cy) = self.map(o.0, o.1 + h);
            let j00 = (ax - mx) / h;
            let j10 = (ay - my) / h;
            let j01 = (cx - mx) / h;
            let j11 = (cy - my) / h;
            let det = j00 * j11 - j01 * j10;
            if det.abs() < 1e-9 {
                break;
            }
            let dx = (j11 * ex - j01 * ey) / det;
            let dy = (-j10 * ex + j00 * ey) / det;
            o.0 -= dx;
            o.1 -= dy;
        }
        o
    }
}

/// Shrinks the crop rectangle around its center so empty areas from rotation/perspective are not visible.
pub fn constrain_crop(bw: usize, bh: usize, g: &Geometry, lens: &LensCorrection) -> [f32; 4] {
    let c = g.crop;
    let cx = (c[0] + c[2]) * 0.5;
    let cy = (c[1] + c[3]) * 0.5;
    let hw = (c[2] - c[0]) * 0.5;
    let hh = (c[3] - c[1]) * 0.5;
    let fits = |k: f32| -> bool {
        let mut gg = g.clone();
        gg.crop = [0.0, 0.0, 1.0, 1.0];
        let m = GeoMap::new(bw, bh, &gg, lens, [0.0, 0.0, 1.0, 1.0], 1000, 1000);
        let pts = 8;
        for i in 0..=pts {
            let t = i as f32 / pts as f32;
            for (x, y) in [
                (cx - hw * k + 2.0 * hw * k * t, cy - hh * k),
                (cx - hw * k + 2.0 * hw * k * t, cy + hh * k),
                (cx - hw * k, cy - hh * k + 2.0 * hh * k * t),
                (cx + hw * k, cy - hh * k + 2.0 * hh * k * t),
            ] {
                let (fx, fy) = (x * m.fw, y * m.fh);
                let (bx, by) = m.frame_to_base(fx, fy);
                if bx < -0.01 || by < -0.01 || bx > m.bw + 0.01 || by > m.bh + 0.01 {
                    return false;
                }
            }
        }
        true
    };
    if fits(1.0) {
        return c;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    [cx - hw * lo, cy - hh * lo, cx + hw * lo, cy + hh * lo]
}

/// Frame size (px) after rotation and flips.
pub fn frame_dims(bw: usize, bh: usize, g: &Geometry) -> (usize, usize) {
    if g.rotate90 % 2 == 1 { (bh, bw) } else { (bw, bh) }
}

/// Crop result size (in source pixels).
pub fn cropped_dims(bw: usize, bh: usize, g: &Geometry) -> (usize, usize) {
    let (fw, fh) = frame_dims(bw, bh, g);
    let s = (g.scale / 100.0).max(0.1);
    let w = ((g.crop[2] - g.crop[0]) * fw as f32 / s).round().max(1.0) as usize;
    let h = ((g.crop[3] - g.crop[1]) * fh as f32 / s).round().max(1.0) as usize;
    // When zoomed in (scale > 1), keep the source-based size so output is not upscaled beyond real resolution.
    if s > 1.0 {
        (((g.crop[2] - g.crop[0]) * fw as f32).round() as usize, ((g.crop[3] - g.crop[1]) * fh as f32).round() as usize)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_pixel_centers() {
        let g = Geometry::default();
        let m = GeoMap::new(400, 300, &g, &LensCorrection::default(), [0.0, 0.0, 1.0, 1.0], 400, 300);
        let (x, y) = m.map(10.5, 20.5);
        assert!((x - 10.5).abs() < 1e-3 && (y - 20.5).abs() < 1e-3);
    }

    #[test]
    fn rotate90_maps_corners() {
        let g = Geometry { rotate90: 1, ..Default::default() };
        // 400x300 source rotated 90° clockwise -> 300x400 frame
        let m = GeoMap::new(400, 300, &g, &LensCorrection::default(), [0.0, 0.0, 1.0, 1.0], 300, 400);
        // Output top-left is the source bottom-left
        let (x, y) = m.map(0.0, 0.0);
        assert!(x.abs() < 1e-3 && (y - 300.0).abs() < 1e-3, "{x},{y}");
    }

    #[test]
    fn inverse_roundtrip() {
        let g = Geometry { angle: 7.0, crop: [0.1, 0.1, 0.9, 0.85], vertical: 20.0, ..Default::default() };
        let m = GeoMap::new(600, 400, &g, &LensCorrection { distortion: 30.0, ..Default::default() }, [0.0, 0.0, 1.0, 1.0], 500, 300);
        let (bx, by) = m.map(123.0, 77.0);
        let (ox, oy) = m.inverse(bx, by);
        assert!((ox - 123.0).abs() < 0.05 && (oy - 77.0).abs() < 0.05, "{ox},{oy}");
    }

    #[test]
    fn constrain_shrinks_rotated_crop() {
        let g = Geometry { angle: 10.0, ..Default::default() };
        let c = constrain_crop(600, 400, &g, &LensCorrection::default());
        assert!(c[0] > 0.0 && c[2] < 1.0);
    }
}
