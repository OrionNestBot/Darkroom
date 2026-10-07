//! Lens correction model (distortion, vignetting, lateral CA) from camera-embedded data (RAW makernotes, DNG opcodes) or lensfun (+ custom vignetting table).
//! Computes coordinate mapping, fill scale and per-channel CA positions; data parsing lives in `embedded` and `lensfun`.
//! © 2026 OrionNest

/// Lens model in source pixel coordinates (after orientation). Calibration data uses sensor coordinates (before rotation), so orientation is undone first.
#[derive(Clone, Debug, PartialEq)]
pub struct LensModel {
    /// Full sensor frame size (before orientation and camera aspect crop); the reference for calibration coordinates
    pub sw: f64,
    pub sh: f64,
    /// Actual image size (before orientation); smaller than the full frame when the camera crops to an aspect ratio
    pub cw: f64,
    pub ch: f64,
    pub orient: u16,
    pub dist_scale: f64,
    pub vig_scale: f64,
    pub ca: bool,
    /// Correction from lensfun data (+ custom vignetting table)
    pub lf: Option<super::lensfun::Model>,
    /// Scale that fills the frame after correction (no empty areas, least cropping; 1 = unchanged), computed by `with_fill`
    pub fill: f64,
    /// Correction from camera-embedded data (instead of a profile)
    pub emb: Option<std::sync::Arc<super::embedded::Embedded>>,
}

impl LensModel {
    #[inline]
    fn to_sensor(&self, bx: f64, by: f64) -> (f64, f64) {
        let (w, h) = (self.cw, self.ch);
        let (ox, oy) = ((self.sw - w) * 0.5, (self.sh - h) * 0.5);
        let (x, y) = match self.orient {
            6 => (by, h - bx),
            8 => (w - by, bx),
            3 => (w - bx, h - by),
            _ => (bx, by),
        };
        (x + ox, y + oy)
    }
    #[inline]
    fn from_sensor(&self, sx: f64, sy: f64) -> (f64, f64) {
        let (w, h) = (self.cw, self.ch);
        let (sx, sy) = (sx - (self.sw - w) * 0.5, sy - (self.sh - h) * 0.5);
        match self.orient {
            6 => (h - sy, sx),
            8 => (sy, w - sx),
            3 => (w - sx, h - sy),
            _ => (sx, sy),
        }
    }

    /// Corrected (ideal) source coordinate → coordinate to read in the actual source
    #[inline]
    pub fn distort(&self, bx: f32, by: f32) -> (f32, f32) {
        if self.dist_scale <= 0.0 || !self.has_dist() {
            return (bx, by);
        }
        let (sx, sy) = self.to_sensor(bx as f64, by as f64);
        let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
        let (sx, sy) = (cx + (sx - cx) / self.fill, cy + (sy - cy) / self.fill);
        let (ex, ey) = self.sensor_distort(sx, sy);
        let (ox, oy) = self.from_sensor(ex, ey);
        (ox as f32, oy as f32)
    }

    fn has_dist(&self) -> bool {
        if let Some(e) = &self.emb {
            return e.has_dist;
        }
        self.lf.as_ref().map(|m| m.dist.is_some()).unwrap_or(false)
    }

    /// Sensor coordinates: corrected point → point to read in the source (amount applied, before fill scale)
    fn sensor_distort(&self, sx: f64, sy: f64) -> (f64, f64) {
        let (dx, dy) = if let Some(e) = &self.emb {
            let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
            let hd = cx.hypot(cy);
            let k = e.cor_at(1, (sx - cx).hypot(sy - cy) / hd);
            (cx + (sx - cx) * k, cy + (sy - cy) * k)
        } else if let Some(m) = &self.lf {
            let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
            let (x, y) = ((sx - cx) / m.hugin_px, (sy - cy) / m.hugin_px);
            let k = m.dist_ratio((x * x + y * y).sqrt());
            (cx + x * k * m.hugin_px, cy + y * k * m.hugin_px)
        } else {
            (sx, sy)
        };
        let k = self.dist_scale;
        (sx + (dx - sx) * k, sy + (dy - sy) * k)
    }

    /// Fill scale: smallest scale at which every border point reads inside the source (below 1 for barrel, above 1 for pincushion)
    pub fn with_fill(mut self) -> Self {
        self.fill = 1.0;
        if self.dist_scale <= 0.0 || !self.has_dist() {
            return self;
        }
        // DNG opcodes already define the output frame (no rescaling)
        if self.emb.as_ref().map(|e| e.maker == "DNG").unwrap_or(false) {
            return self;
        }
        // Actual image area in sensor coordinates (the central part when aspect-cropped)
        let (w, h) = (self.cw, self.ch);
        let (ox, oy) = ((self.sw - w) * 0.5, (self.sh - h) * 0.5);
        let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
        let n = 48;
        let mut border = Vec::with_capacity(n * 4);
        for i in 0..=n {
            let t = i as f64 / n as f64;
            border.extend([(ox + t * w, oy), (ox + t * w, oy + h), (ox, oy + t * h), (ox + w, oy + t * h)]);
        }
        let inside = |s: f64| {
            border.iter().all(|&(x, y)| {
                let (dx, dy) = self.sensor_distort(cx + (x - cx) / s, cy + (y - cy) / s);
                dx >= ox - 0.5 && dx <= ox + w + 0.5 && dy >= oy - 0.5 && dy <= oy + h + 0.5
            })
        };
        let (mut lo, mut hi) = (0.7, 1.5);
        if !inside(hi) {
            return self;
        }
        if inside(lo) {
            self.fill = lo;
            return self;
        }
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if inside(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        self.fill = hi;
        self
    }

    /// Actual source coordinate (green) → coordinate to read the red/blue channel from
    #[inline]
    pub fn ca_offsets(&self, bx: f32, by: f32) -> Option<((f32, f32), (f32, f32))> {
        if !self.ca {
            return None;
        }
        if let Some(e) = self.emb.as_ref().filter(|e| e.has_ca || self.lf.is_none()) {
            if !e.has_ca {
                return None;
            }
            // Source radius where green was read → output radius (fixed point) → red/blue source radius ratio
            let (sx, sy) = self.to_sensor(bx as f64, by as f64);
            let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
            let hd = cx.hypot(cy);
            let rg = (sx - cx).hypot(sy - cy) / hd;
            let k = if self.has_dist() { self.dist_scale } else { 0.0 };
            let geff = |r: f64| 1.0 + (e.cor_at(1, r) - 1.0) * k;
            let mut ro = rg;
            for _ in 0..8 {
                ro = rg / geff(ro).max(0.1);
            }
            let (g, gc) = (geff(ro), e.cor_at(1, ro));
            let kr = (g + e.cor_at(0, ro) - gc) / g;
            let kb = (g + e.cor_at(2, ro) - gc) / g;
            let (rx, ry) = self.from_sensor(cx + (sx - cx) * kr, cy + (sy - cy) * kr);
            let (qx, qy) = self.from_sensor(cx + (sx - cx) * kb, cy + (sy - cy) * kb);
            return Some(((rx as f32, ry as f32), (qx as f32, qy as f32)));
        }
        if let Some(m) = &self.lf {
            let (sx, sy) = self.to_sensor(bx as f64, by as f64);
            let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
            let (x, y) = ((sx - cx) / m.hugin_px, (sy - cy) / m.hugin_px);
            let (kr, kb) = m.tca_ratio((x * x + y * y).sqrt())?;
            let (rx, ry) = self.from_sensor(cx + x * kr * m.hugin_px, cy + y * kr * m.hugin_px);
            let (qx, qy) = self.from_sensor(cx + x * kb * m.hugin_px, cy + y * kb * m.hugin_px);
            return Some(((rx as f32, ry as f32), (qx as f32, qy as f32)));
        }
        None
    }

    /// Actual source coordinate → vignetting correction gain (≥ 1)
    #[inline]
    pub fn vignette_gain(&self, bx: f32, by: f32) -> f32 {
        if self.vig_scale <= 0.0 {
            return 1.0;
        }
        if let Some(e) = self.emb.as_ref().filter(|e| e.has_vig || self.lf.is_none()) {
            if !e.has_vig {
                return 1.0;
            }
            let (sx, sy) = self.to_sensor(bx as f64, by as f64);
            let (cx, cy) = (self.sw * 0.5, self.sh * 0.5);
            let f = e.vig_at((sx - cx).hypot(sy - cy) / cx.hypot(cy)).clamp(0.05, 2.0);
            return (1.0 / f).powf(self.vig_scale) as f32;
        }
        if let Some(m) = &self.lf {
            if m.vig.is_none() {
                return 1.0;
            }
            let (sx, sy) = self.to_sensor(bx as f64, by as f64);
            let r = (sx - self.sw * 0.5).hypot(sy - self.sh * 0.5) / m.vig_px;
            let f = m.vig_value(r).clamp(0.05, 2.0);
            return (1.0 / f).powf(self.vig_scale) as f32;
        }
        1.0
    }
}
