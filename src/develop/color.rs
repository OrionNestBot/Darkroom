//! Color/tone math: sRGB transfer function, LUTs, monotone cubic spline, hue utilities.

use crate::config::LUT_SIZE;
use std::sync::OnceLock;

#[inline]
pub fn srgb_encode(x: f32) -> f32 {
    if x <= 0.0031308 {
        (x * 12.92).max(0.0)
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

#[inline]
pub fn srgb_decode(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// 8-bit sRGB to linear table.
pub fn srgb8_to_linear_table() -> &'static [f32; 256] {
    static T: OnceLock<[f32; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0.0; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_decode(i as f32 / 255.0);
        }
        t
    })
}

/// 16-bit sRGB to linear table.
pub fn srgb16_to_linear_table() -> &'static Vec<f32> {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| (0..65536).map(|i| srgb_decode(i as f32 / 65535.0)).collect())
}

/// 1D LUT over the [0,1] domain with linear interpolation.
#[derive(Clone)]
pub struct Lut {
    pub t: Vec<f32>,
}

impl Lut {
    pub fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        let n = LUT_SIZE;
        Self { t: (0..n).map(|i| f(i as f32 / (n - 1) as f32)).collect() }
    }

    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let n1 = (self.t.len() - 1) as f32;
        let p = (x.clamp(0.0, 1.0)) * n1;
        let i = p as usize;
        if i >= self.t.len() - 1 {
            return self.t[self.t.len() - 1];
        }
        let f = p - i as f32;
        self.t[i] + (self.t[i + 1] - self.t[i]) * f
    }
}

/// Linear to display sRGB encoding LUT (clamped to 0..1). Used instead of a per-pixel powf.
pub fn encode_lut() -> &'static Lut {
    static L: OnceLock<Lut> = OnceLock::new();
    L.get_or_init(|| {
        // Sample in sqrt space for precision in the shadows
        let n = LUT_SIZE * 4;
        Lut { t: (0..n).map(|i| srgb_encode((i as f32 / (n - 1) as f32).powi(2))).collect() }
    })
}

#[inline]
pub fn fast_encode(x: f32) -> f32 {
    encode_lut().eval(x.max(0.0).sqrt())
}

/// Display sRGB to linear decoding LUT (clamped to 0..1). Used instead of a per-pixel powf.
pub fn decode_lut() -> &'static Lut {
    static L: OnceLock<Lut> = OnceLock::new();
    L.get_or_init(|| {
        let n = LUT_SIZE * 4;
        Lut { t: (0..n).map(|i| srgb_decode(i as f32 / (n - 1) as f32)).collect() }
    })
}

#[inline]
pub fn fast_decode(x: f32) -> f32 {
    decode_lut().eval(x.clamp(0.0, 1.0))
}

/// Monotone cubic Hermite spline (Fritsch–Carlson). No overshoot.
pub fn monotone_spline(points: &[[f32; 2]]) -> impl Fn(f32) -> f32 + '_ {
    let n = points.len();
    let mut m = vec![0.0f32; n];
    if n >= 2 {
        let mut d = vec![0.0f32; n - 1];
        for i in 0..n - 1 {
            let dx = (points[i + 1][0] - points[i][0]).max(1e-6);
            d[i] = (points[i + 1][1] - points[i][1]) / dx;
        }
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            if d[i - 1] * d[i] <= 0.0 {
                m[i] = 0.0;
            } else {
                m[i] = (d[i - 1] + d[i]) * 0.5;
            }
        }
        for i in 0..n - 1 {
            if d[i].abs() < 1e-9 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    move |x: f32| {
        if n == 0 {
            return x;
        }
        if n == 1 || x <= points[0][0] {
            return points[0][1];
        }
        if x >= points[n - 1][0] {
            return points[n - 1][1];
        }
        let mut k = 0;
        while k < n - 2 && x > points[k + 1][0] {
            k += 1;
        }
        let (x0, y0) = (points[k][0], points[k][1]);
        let (x1, y1) = (points[k + 1][0], points[k + 1][1]);
        let h = (x1 - x0).max(1e-6);
        let t = (x - x0) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * y0 + h10 * h * m[k] + h01 * y1 + h11 * h * m[k + 1]
    }
}

#[inline]
pub fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// RGB to (hue 0..360, chroma, max). HSV-style hue.
#[inline]
pub fn rgb_hue(r: f32, g: f32, b: f32) -> (f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let c = mx - mn;
    if c <= 1e-6 {
        return (0.0, 0.0);
    }
    let h = if mx == r {
        ((g - b) / c).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / c + 2.0
    } else {
        (r - g) / c + 4.0
    };
    (h * 60.0, c)
}

/// Hue rotation (approximate YIQ-style rotation, preserves luminance).
#[inline]
pub fn rotate_hue(rgb: [f32; 3], deg: f32) -> [f32; 3] {
    if deg.abs() < 1e-4 {
        return rgb;
    }
    let (s, c) = deg.to_radians().sin_cos();
    let k = 1.0 / 3.0;
    let sq = (1.0f32 / 3.0).sqrt();
    // Rodrigues rotation around the gray axis
    let m00 = c + (1.0 - c) * k;
    let m01 = k * (1.0 - c) - sq * s;
    let m02 = k * (1.0 - c) + sq * s;
    let m10 = k * (1.0 - c) + sq * s;
    let m11 = c + k * (1.0 - c);
    let m12 = k * (1.0 - c) - sq * s;
    let m20 = k * (1.0 - c) - sq * s;
    let m21 = k * (1.0 - c) + sq * s;
    let m22 = c + k * (1.0 - c);
    [
        m00 * rgb[0] + m01 * rgb[1] + m02 * rgb[2],
        m10 * rgb[0] + m11 * rgb[1] + m12 * rgb[2],
        m20 * rgb[0] + m21 * rgb[1] + m22 * rgb[2],
    ]
}

/// HSV (0..360, 0..1, 0..1) to RGB (for UI color display).
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// Approximate color temperature for display (K): converts the temp slider value to a readable number.
pub fn temp_slider_to_kelvin_label(base_k: f32, temp: f32) -> f32 {
    // Log scale: ±100 maps to roughly ×0.5..×2 in mireds
    let mired = 1.0e6 / base_k;
    let m = mired * 2f32.powf(-temp / 100.0);
    1.0e6 / m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!((srgb_decode(srgb_encode(x)) - x).abs() < 1e-4);
            assert!((fast_encode(x) - srgb_encode(x)).abs() < 2e-3, "x={x}");
        }
    }

    #[test]
    fn spline_identity_and_monotone() {
        let pts = vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.9], [1.0, 1.0]];
        let f = monotone_spline(&pts);
        let mut prev = -1.0;
        for i in 0..=100 {
            let y = f(i as f32 / 100.0);
            assert!(y >= prev - 1e-6);
            prev = y;
        }
        let id = vec![[0.0, 0.0], [1.0, 1.0]];
        let g = monotone_spline(&id);
        assert!((g(0.3) - 0.3).abs() < 1e-5);
    }

    #[test]
    fn hue_rotation_preserves_gray() {
        let g = rotate_hue([0.4, 0.4, 0.4], 47.0);
        assert!(g.iter().all(|v| (v - 0.4).abs() < 1e-5));
    }
}
