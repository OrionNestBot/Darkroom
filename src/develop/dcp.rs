//! Color engine: the DNG color model (temperature-interpolated color/forward matrices, HSV maps, look tables, baseline exposure) and the DCP profile format.
//! Profile priority: profile embedded in a DNG, then built-in profiles (`assets/profiles.json`), then the generic profile, then public color matrices.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub type M3 = [[f64; 3]; 3];

// ───────────────────────── Matrix / coordinate helpers ─────────────────────────

pub fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                r[i][j] += a[i][k] * b[k][j];
            }
        }
    }
    r
}

pub fn mulv(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

pub fn inv(m: &M3) -> M3 {
    let a = m;
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let d = if det.abs() < 1e-12 { 1e-12 } else { det };
    [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) / d,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / d,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / d,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) / d,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / d,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / d,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) / d,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / d,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / d,
        ],
    ]
}

fn diag(v: [f64; 3]) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

fn lerp_m(a: &M3, b: &M3, g: f64) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = g * a[i][j] + (1.0 - g) * b[i][j];
        }
    }
    r
}

fn scale_m(a: &M3, s: f64) -> M3 {
    let mut r = *a;
    for row in &mut r {
        for v in row.iter_mut() {
            *v *= s;
        }
    }
    r
}

fn max3(v: [f64; 3]) -> f64 {
    v[0].max(v[1]).max(v[2])
}

pub const D50_XY: [f64; 2] = [0.3457, 0.3585];

pub fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    let x = xy[0].clamp(0.000001, 0.999999);
    let y = xy[1].clamp(0.000001, 0.999999);
    let y = if x + y > 0.999999 { 0.999999 - x } else { y };
    [x / y, 1.0, (1.0 - x - y) / y]
}

pub fn xyz_to_xy(v: [f64; 3]) -> [f64; 2] {
    let t = v[0] + v[1] + v[2];
    if t > 0.0 { [v[0] / t, v[1] / t] } else { D50_XY }
}

/// PCS white (XYZ D50)
fn pcs_xyz() -> [f64; 3] {
    xy_to_xyz(D50_XY)
}

/// Linear ProPhoto (ROMM) to XYZ D50
pub const PROPHOTO_TO_PCS: M3 = [[0.7977, 0.1352, 0.0313], [0.2880, 0.7119, 0.0001], [0.0000, 0.0000, 0.8249]];
/// Linear sRGB to XYZ D50 (Bradford-adapted)
pub const SRGB_TO_PCS: M3 = [[0.4361, 0.3851, 0.1431], [0.2225, 0.7169, 0.0606], [0.0139, 0.0971, 0.7141]];

pub fn prophoto_to_srgb() -> &'static M3 {
    static M: OnceLock<M3> = OnceLock::new();
    M.get_or_init(|| mul(&inv(&SRGB_TO_PCS), &PROPHOTO_TO_PCS))
}

#[allow(dead_code)]
pub fn srgb_to_prophoto() -> &'static M3 {
    static M: OnceLock<M3> = OnceLock::new();
    M.get_or_init(|| mul(&inv(&PROPHOTO_TO_PCS), &SRGB_TO_PCS))
}

/// Bradford white-point adaptation matrix
fn map_white_matrix(w1: [f64; 2], w2: [f64; 2]) -> M3 {
    let mb: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let a = mulv(&mb, xy_to_xyz(w1)).map(|v| v.max(0.0));
    let b = mulv(&mb, xy_to_xyz(w2)).map(|v| v.max(0.0));
    let s = |i: usize| if a[i] > 0.0 { (b[i] / a[i]).clamp(0.1, 10.0) } else { 10.0 };
    mul(&mul(&inv(&mb), &diag([s(0), s(1), s(2)])), &mb)
}

// ───────────────────────── Color temperature (Robertson method) ─────────────────────────

const TINT_SCALE: f64 = -3000.0;
const TEMP_TABLE: [[f64; 4]; 31] = [
    [0.0, 0.18006, 0.26352, -0.24341],
    [10.0, 0.18066, 0.26589, -0.25479],
    [20.0, 0.18133, 0.26846, -0.26876],
    [30.0, 0.18208, 0.27119, -0.28539],
    [40.0, 0.18293, 0.27407, -0.30470],
    [50.0, 0.18388, 0.27709, -0.32675],
    [60.0, 0.18494, 0.28021, -0.35156],
    [70.0, 0.18611, 0.28342, -0.37915],
    [80.0, 0.18740, 0.28668, -0.40955],
    [90.0, 0.18880, 0.28997, -0.44278],
    [100.0, 0.19032, 0.29326, -0.47888],
    [125.0, 0.19462, 0.30141, -0.58204],
    [150.0, 0.19962, 0.30921, -0.70471],
    [175.0, 0.20525, 0.31647, -0.84901],
    [200.0, 0.21142, 0.32312, -1.0182],
    [225.0, 0.21807, 0.32909, -1.2168],
    [250.0, 0.22511, 0.33439, -1.4512],
    [275.0, 0.23247, 0.33904, -1.7298],
    [300.0, 0.24010, 0.34308, -2.0637],
    [325.0, 0.24702, 0.34655, -2.4681],
    [350.0, 0.25591, 0.34951, -2.9641],
    [375.0, 0.26400, 0.35200, -3.5814],
    [400.0, 0.27218, 0.35407, -4.3633],
    [425.0, 0.28039, 0.35577, -5.3762],
    [450.0, 0.28863, 0.35714, -6.7262],
    [475.0, 0.29685, 0.35823, -8.5955],
    [500.0, 0.30505, 0.35907, -11.324],
    [525.0, 0.31320, 0.35968, -15.628],
    [550.0, 0.32129, 0.36011, -23.325],
    [575.0, 0.32931, 0.36038, -40.770],
    [600.0, 0.33724, 0.36051, -116.45],
];

/// xy to (temperature K, tint)
pub fn xy_to_temp(xy: [f64; 2]) -> (f64, f64) {
    let u = 2.0 * xy[0] / (1.5 - xy[0] + 6.0 * xy[1]);
    let v = 3.0 * xy[1] / (1.5 - xy[0] + 6.0 * xy[1]);
    let (mut last_dt, mut last_dv, mut last_du) = (0.0, 0.0, 0.0);
    for index in 1..=30 {
        let mut du = 1.0;
        let mut dv = TEMP_TABLE[index][3];
        let len = (1.0 + dv * dv).sqrt();
        du /= len;
        dv /= len;
        let mut uu = u - TEMP_TABLE[index][1];
        let mut vv = v - TEMP_TABLE[index][2];
        let mut dt = -uu * dv + vv * du;
        if dt <= 0.0 || index == 30 {
            if dt > 0.0 {
                dt = 0.0;
            }
            dt = -dt;
            let f = if index == 1 { 0.0 } else { dt / (last_dt + dt) };
            let temp = 1.0e6 / (TEMP_TABLE[index - 1][0] * f + TEMP_TABLE[index][0] * (1.0 - f));
            uu = u - (TEMP_TABLE[index - 1][1] * f + TEMP_TABLE[index][1] * (1.0 - f));
            vv = v - (TEMP_TABLE[index - 1][2] * f + TEMP_TABLE[index][2] * (1.0 - f));
            du = du * (1.0 - f) + last_du * f;
            dv = dv * (1.0 - f) + last_dv * f;
            let len = (du * du + dv * dv).sqrt();
            du /= len;
            dv /= len;
            return (temp, (uu * du + vv * dv) * TINT_SCALE);
        }
        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }
    (5000.0, 0.0)
}

/// (temperature K, tint) to xy
pub fn temp_to_xy(temp: f64, tint: f64) -> [f64; 2] {
    let r = 1.0e6 / temp.max(1.0);
    let offset = tint * (1.0 / TINT_SCALE);
    for index in 0..=29 {
        if r < TEMP_TABLE[index + 1][0] || index == 29 {
            let f = (TEMP_TABLE[index + 1][0] - r) / (TEMP_TABLE[index + 1][0] - TEMP_TABLE[index][0]);
            let mut u = TEMP_TABLE[index][1] * f + TEMP_TABLE[index + 1][1] * (1.0 - f);
            let mut v = TEMP_TABLE[index][2] * f + TEMP_TABLE[index + 1][2] * (1.0 - f);
            let (mut uu1, mut vv1) = (1.0, TEMP_TABLE[index][3]);
            let (mut uu2, mut vv2) = (1.0, TEMP_TABLE[index + 1][3]);
            let l1 = (1.0 + vv1 * vv1).sqrt();
            let l2 = (1.0 + vv2 * vv2).sqrt();
            uu1 /= l1;
            vv1 /= l1;
            uu2 /= l2;
            vv2 /= l2;
            let mut uu3 = uu1 * f + uu2 * (1.0 - f);
            let mut vv3 = vv1 * f + vv2 * (1.0 - f);
            let l3 = (uu3 * uu3 + vv3 * vv3).sqrt();
            uu3 /= l3;
            vv3 /= l3;
            u += uu3 * offset;
            v += vv3 * offset;
            return [1.5 * u / (u - 4.0 * v + 2.0), v / (u - 4.0 * v + 2.0)];
        }
    }
    D50_XY
}

// ───────────────────────── HSV tables ─────────────────────────

#[derive(Clone, Debug)]
pub struct HsvTable {
    pub hue: usize,
    pub sat: usize,
    pub val: usize,
    /// (hue shift in degrees, saturation scale, value scale), ordered value, hue, saturation
    pub data: Vec<[f32; 3]>,
    /// 0 = linear value axis, 1 = value encoded with sRGB gamma
    pub srgb_encoding: bool,
}

impl HsvTable {
    fn lerp(a: &HsvTable, b: &HsvTable, g: f64) -> HsvTable {
        let g = g as f32;
        HsvTable {
            data: a.data.iter().zip(&b.data).map(|(x, y)| [g * x[0] + (1.0 - g) * y[0], g * x[1] + (1.0 - g) * y[1], g * x[2] + (1.0 - g) * y[2]]).collect(),
            ..a.clone()
        }
    }

    /// Hue/saturation map lookup as defined by the DNG specification.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [r, g, b] = rgb;
        let v = r.max(g).max(b);
        let gap = v - r.min(g).min(b);
        let (mut h, mut s) = (0.0f32, 0.0f32);
        if gap > 0.0 {
            h = if r == v {
                let x = (g - b) / gap;
                if x < 0.0 { x + 6.0 } else { x }
            } else if g == v {
                2.0 + (b - r) / gap
            } else {
                4.0 + (r - g) / gap
            };
            s = gap / v;
        }
        let h_scale = if self.hue < 2 { 0.0 } else { self.hue as f32 / 6.0 };
        let s_scale = (self.sat as i32 - 1) as f32;
        let v_scale = (self.val as i32 - 1) as f32;
        let max_h0 = self.hue as i32 - 1;
        let max_s0 = self.sat as i32 - 2;
        let max_v0 = self.val as i32 - 2;
        let hue_step = self.sat as i32;
        let val_step = (self.hue * self.sat) as i32;
        let d = &self.data;
        let mut v_enc = v;
        let (hs, ss, vs);
        let hs_ = h * h_scale;
        let ss_ = s * s_scale;
        let mut h0 = hs_ as i32;
        let s0 = (ss_ as i32).min(max_s0).max(0);
        let mut h1 = h0 + 1;
        if h0 >= max_h0 {
            h0 = max_h0;
            h1 = 0;
        }
        let hf1 = hs_ - h0 as f32;
        let sf1 = ss_ - s0 as f32;
        let (hf0, sf0) = (1.0 - hf1, 1.0 - sf1);
        if self.val < 2 {
            let e00 = (h0 * hue_step + s0) as usize;
            let e01 = (h1 * hue_step + s0) as usize;
            let mix = |k: usize| {
                let a = hf0 * d[e00][k] + hf1 * d[e01][k];
                let b = hf0 * d[e00 + 1][k] + hf1 * d[e01 + 1][k];
                sf0 * a + sf1 * b
            };
            hs = mix(0);
            ss = mix(1);
            vs = mix(2);
        } else {
            if self.srgb_encoding {
                v_enc = super::color::srgb_encode(v.clamp(0.0, 1.0));
            }
            let vsc = v_enc * v_scale;
            let v0 = (vsc as i32).min(max_v0).max(0);
            let vf1 = vsc - v0 as f32;
            let vf0 = 1.0 - vf1;
            let e00 = (v0 * val_step + h0 * hue_step + s0) as usize;
            let e01 = (v0 * val_step + h1 * hue_step + s0) as usize;
            let e10 = e00 + val_step as usize;
            let e11 = e01 + val_step as usize;
            let mix = |k: usize| {
                let a = vf0 * (hf0 * d[e00][k] + hf1 * d[e01][k]) + vf1 * (hf0 * d[e10][k] + hf1 * d[e11][k]);
                let b = vf0 * (hf0 * d[e00 + 1][k] + hf1 * d[e01 + 1][k]) + vf1 * (hf0 * d[e10 + 1][k] + hf1 * d[e11 + 1][k]);
                sf0 * a + sf1 * b
            };
            hs = mix(0);
            ss = mix(1);
            vs = mix(2);
        }
        let h = h + hs * (6.0 / 360.0);
        let s = (s * ss).min(1.0);
        v_enc = (v_enc * vs).clamp(0.0, 1.0);
        let v = if self.val >= 2 && self.srgb_encoding { super::color::srgb_decode(v_enc) } else { v_enc };
        // Values above 1.0 (highlight headroom) keep their original value ratio
        let v = if self.val < 2 { if rgb_max_over(rgb) { v.max(0.0) } else { v } } else { v };
        hsv_to_rgb(h, s, v)
    }
}

#[inline]
fn rgb_max_over(_rgb: [f32; 3]) -> bool {
    false
}

#[inline]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [v, v, v];
    }
    let mut h = h % 6.0;
    if h < 0.0 {
        h += 6.0;
    }
    let i = h as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// DNG-style RGB tone: apply the curve to the max and min channels and interpolate the middle one (preserves hue).
#[inline]
pub fn rgb_tone(rgb: [f32; 3], curve: &impl Fn(f32) -> f32) -> [f32; 3] {
    let [r, g, b] = rgb;
    let tone = |hi: f32, mid: f32, lo: f32| -> (f32, f32, f32) {
        let hh = curve(hi);
        let ll = curve(lo);
        let mm = if hi > lo { ll + (hh - ll) * (mid - lo) / (hi - lo) } else { hh };
        (hh, mm, ll)
    };
    if r >= g {
        if g > b {
            let (rr, gg, bb) = tone(r, g, b);
            [rr, gg, bb]
        } else if b > r {
            let (bb, rr, gg) = tone(b, r, g);
            [rr, gg, bb]
        } else if b > g {
            let (rr, bb, gg) = tone(r, b, g);
            [rr, gg, bb]
        } else {
            let rr = curve(r);
            let gg = curve(g);
            [rr, gg, gg]
        }
    } else if r >= b {
        let (gg, rr, bb) = tone(g, r, b);
        [rr, gg, bb]
    } else if b > g {
        let (bb, gg, rr) = tone(b, g, r);
        [rr, gg, bb]
    } else {
        let (gg, bb, rr) = tone(g, b, r);
        [rr, gg, bb]
    }
}

/// Default tone curve for profiles without one, fitted against reference renders of public RAW files (tools/fit_tone.py).
/// Nodes are log2 outputs at log2 inputs -14, -13.5, ..., 0.
pub const TONE_NODES: [f64; 29] = [
    -15.21269, -14.67903, -14.14537, -13.61171, -13.07805, -12.54439, -12.01073, -11.47707, -10.94341, -10.40975, -9.87597, -9.33851, -8.78335, -8.19137, -7.55303,
    -6.87069, -6.14764, -5.38161, -4.60351, -3.84510, -3.14114, -2.48869, -1.88327, -1.34974, -0.90420, -0.55437, -0.30680, -0.13456, 0.00000,
];
const TONE_LO: f64 = -14.0;
const TONE_STEP: f64 = 0.5;

/// Lookup table joining the log-log nodes with monotone cubic (Fritsch-Carlson) interpolation (linear input 0..1, 1025 entries)
fn base_tone_table() -> &'static [f32; 1025] {
    static T: OnceLock<[f32; 1025]> = OnceLock::new();
    T.get_or_init(|| {
        // Test switch: DARKROOM_TONE_NODES=<file> overrides the 29 nodes (comma or whitespace separated)
        let nodes: Vec<f64> = std::env::var_os("DARKROOM_TONE_NODES")
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|v| v.parse().ok()).collect::<Vec<f64>>())
            .filter(|v| v.len() == TONE_NODES.len())
            .unwrap_or_else(|| TONE_NODES.to_vec());
        let n = nodes.len();
        let d: Vec<f64> = (0..n - 1).map(|k| (nodes[k + 1] - nodes[k]) / TONE_STEP).collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { 2.0 / (1.0 / d[k - 1] + 1.0 / d[k]) };
        }
        let eval = |lx: f64| -> f64 {
            if lx <= TONE_LO {
                return nodes[0] + d[0] * (lx - TONE_LO);
            }
            let t = ((lx - TONE_LO) / TONE_STEP).min((n - 1) as f64 - 1e-9);
            let k = t.floor() as usize;
            let f = t - k as f64;
            let (h00, h10, h01, h11) = (2.0 * f * f * f - 3.0 * f * f + 1.0, f * f * f - 2.0 * f * f + f, -2.0 * f * f * f + 3.0 * f * f, f * f * f - f * f);
            h00 * nodes[k] + h10 * TONE_STEP * m[k] + h01 * nodes[k + 1] + h11 * TONE_STEP * m[k + 1]
        };
        let mut out = [0f32; 1025];
        for (i, o) in out.iter_mut().enumerate().skip(1) {
            *o = 2f64.powf(eval((i as f64 / 1024.0).log2())).min(1.0) as f32;
        }
        out
    })
}

/// Default tone curve (linear in, linear out)
#[inline]
pub fn base_tone(x: f32) -> f32 {
    let t = base_tone_table();
    let y = x.clamp(0.0, 1.0) * 1024.0;
    let i = (y as usize).min(1023);
    let f = y - i as f32;
    t[i] * (1.0 - f) + t[i + 1] * f
}

/// DNG-style negative exposure tone: linear below, quadratic above mapping 1 to 1 (keeps highlights). Test switch: DARKROOM_EXPO_RAMP=1.
pub fn exposure_tone(exposure: f64) -> impl Fn(f32) -> f32 {
    let nop = exposure >= 0.0;
    let slope = 2f64.powf(exposure.min(0.0));
    let a = 16.0 / 9.0 * (1.0 - slope);
    let b = slope - 0.5 * a;
    let c = 1.0 - a - b;
    move |x: f32| {
        if nop {
            return x;
        }
        let x = x as f64;
        (if x <= 0.25 { x * slope } else { (a * x + b) * x + c }) as f32
    }
}

// ───────────────────────── DCP profiles ─────────────────────────

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub name: String,
    pub camera: String,
    pub illum1: u16,
    pub illum2: u16,
    pub cm1: Option<M3>,
    pub cm2: Option<M3>,
    pub fm1: Option<M3>,
    pub fm2: Option<M3>,
    pub hsm1: Option<HsvTable>,
    pub hsm2: Option<HsvTable>,
    pub look: Option<HsvTable>,
    /// ProfileToneCurve as (x, y) pairs; when absent the default tone curve (base_tone) is used.
    pub tone_curve: Option<Vec<[f32; 2]>>,
    pub baseline_exposure_offset: f64,
    /// Compute camera calibration sliders from the public color matrix primaries (for models whose fitted forward-matrix primaries are far off)
    pub cal_public: bool,
}

pub fn illuminant_temp(code: u16) -> f64 {
    match code {
        17 | 3 => 2850.0,
        23 => 5000.0,
        20 | 1 | 9 | 4 | 18 => 5500.0,
        21 | 19 | 10 => 6500.0,
        22 | 11 => 7500.0,
        12 => (5700.0 + 7100.0) * 0.5,
        13 => (4600.0 + 5500.0) * 0.5,
        14 | 2 => (3800.0 + 4500.0) * 0.5,
        15 => (3250.0 + 3800.0) * 0.5,
        24 => 3200.0,
        _ => 0.0,
    }
}

#[allow(dead_code)]
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
/// Endian-aware TIFF read helpers
struct Tiff<'a> {
    b: &'a [u8],
    be: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, o: usize) -> u16 {
        let a = [self.b[o], self.b[o + 1]];
        if self.be { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) }
    }
    fn u32(&self, o: usize) -> u32 {
        let a = [self.b[o], self.b[o + 1], self.b[o + 2], self.b[o + 3]];
        if self.be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    }
    fn i32(&self, o: usize) -> i32 {
        self.u32(o) as i32
    }
    fn f32(&self, o: usize) -> f32 {
        f32::from_bits(self.u32(o))
    }
}

/// Color-related tags read from IFD0 of a DCP or DNG
#[derive(Default)]
pub struct ColorTags {
    pub profile: Option<Profile>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub as_shot_white_xy: Option<[f64; 2]>,
    pub baseline_exposure: f64,
}

/// Parse color tags from IFD0 of a DCP ("IIRC") or DNG (TIFF).
pub fn parse_color_tags(b: &[u8]) -> Option<ColorTags> {
    if b.len() < 8 {
        return None;
    }
    let be = match &b[0..2] {
        b"II" => false,
        b"MM" => true,
        _ => return None,
    };
    let t = Tiff { b, be };
    let off = t.u32(4) as usize;
    if off + 2 > b.len() {
        return None;
    }
    let n = t.u16(off) as usize;
    let mut p = Profile::default();
    let mut ct = ColorTags::default();
    let mut hsm_dims = (0usize, 0usize, 0usize);
    let mut look_dims = (0usize, 0usize, 0usize);
    let (mut hsm_d1, mut hsm_d2, mut look_d): (Vec<f32>, Vec<f32>, Vec<f32>) = (vec![], vec![], vec![]);
    let (mut hsm_enc, mut look_enc) = (0u32, 0u32);
    for i in 0..n {
        let e = off + 2 + i * 12;
        if e + 12 > b.len() {
            break;
        }
        let tag = t.u16(e);
        let typ = t.u16(e + 2);
        let cnt = t.u32(e + 4) as usize;
        let unit = match typ {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => 1,
        };
        let sz = unit * cnt;
        let d0 = if sz > 4 { t.u32(e + 8) as usize } else { e + 8 };
        if d0 + sz > b.len() {
            continue;
        }
        let floats = || -> Vec<f32> { (0..cnt).map(|k| t.f32(d0 + k * 4)).collect() };
        let rats = || -> Vec<f64> {
            (0..cnt)
                .map(|k| {
                    let (nn, dd) = if typ == 10 {
                        (t.i32(d0 + k * 8) as f64, t.i32(d0 + k * 8 + 4) as f64)
                    } else {
                        (t.u32(d0 + k * 8) as f64, t.u32(d0 + k * 8 + 4) as f64)
                    };
                    if dd != 0.0 { nn / dd } else { 0.0 }
                })
                .collect()
        };
        let mat = || -> Option<M3> {
            let v = rats();
            (v.len() >= 9).then(|| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
        };
        let ascii = || String::from_utf8_lossy(&b[d0..d0 + sz]).trim_end_matches(' ').to_string();
        let u32s = || -> Vec<u32> { (0..cnt).map(|k| if typ == 3 { t.u16(d0 + k * 2) as u32 } else { t.u32(d0 + k * 4) }).collect() };
        match tag {
            50708 => p.camera = ascii(),
            50936 => p.name = ascii(),
            50721 => p.cm1 = mat(),
            50722 => p.cm2 = mat(),
            50964 => p.fm1 = mat(),
            50965 => p.fm2 = mat(),
            50778 => p.illum1 = t.u16(d0),
            50779 => p.illum2 = t.u16(d0),
            50937 => {
                let v = u32s();
                if v.len() >= 3 {
                    hsm_dims = (v[0] as usize, v[1] as usize, v[2] as usize);
                }
            }
            50938 => hsm_d1 = floats(),
            50939 => hsm_d2 = floats(),
            50981 => {
                let v = u32s();
                if v.len() >= 3 {
                    look_dims = (v[0] as usize, v[1] as usize, v[2] as usize);
                }
            }
            50982 => look_d = floats(),
            51107 => hsm_enc = t.u32(d0),
            51108 => look_enc = t.u32(d0),
            50940 => {
                let v = floats();
                p.tone_curve = Some(v.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]).collect());
            }
            51109 => p.baseline_exposure_offset = rats().first().copied().unwrap_or(0.0),
            50728 => {
                let v = rats();
                if v.len() >= 3 {
                    ct.as_shot_neutral = Some([v[0], v[1], v[2]]);
                }
            }
            50729 => {
                let v = rats();
                if v.len() >= 2 {
                    ct.as_shot_white_xy = Some([v[0], v[1]]);
                }
            }
            50730 => ct.baseline_exposure = rats().first().copied().unwrap_or(0.0),
            _ => {}
        }
    }
    let mk = |dims: (usize, usize, usize), data: Vec<f32>, enc: u32| -> Option<HsvTable> {
        let (h, s, v) = dims;
        let v = v.max(1);
        if data.is_empty() || h * s * v * 3 != data.len() {
            return None;
        }
        Some(HsvTable { hue: h, sat: s, val: v, data: data.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect(), srgb_encoding: enc == 1 })
    };
    p.hsm1 = mk(hsm_dims, hsm_d1, hsm_enc);
    p.hsm2 = mk(hsm_dims, hsm_d2, hsm_enc);
    p.look = mk(look_dims, look_d, look_enc);
    if p.cm1.is_some() {
        ct.profile = Some(p);
    }
    Some(ct)
}

/// Parse a DCP file.
pub fn parse_dcp(b: &[u8]) -> Option<Profile> {
    parse_color_tags(b)?.profile
}

/// User profile folder (%LOCALAPPDATA%\Darkroom\profiles): DCP camera profiles placed here (e.g. made with dcamprof)
/// are added to that camera's (UniqueCameraModel) profile list, so new cameras work without a program update.
/// Read at startup.
pub fn user_profile_dir() -> std::path::PathBuf {
    crate::config::data_dir().join("profiles")
}

fn user_profiles() -> &'static Vec<Arc<Profile>> {
    static P: OnceLock<Vec<Arc<Profile>>> = OnceLock::new();
    P.get_or_init(|| {
        let mut v: Vec<Arc<Profile>> = std::fs::read_dir(user_profile_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("dcp")).unwrap_or(false))
            .filter_map(|path| {
                let mut p = parse_dcp(&std::fs::read(&path).ok()?)?;
                if p.name.trim().is_empty() {
                    p.name = path.file_stem()?.to_string_lossy().to_string();
                }
                // Skip names that clash with built-in profiles or looks so saved settings stay unambiguous
                let reserved = p.name == DEFAULT_PROFILE || p.name == "Adobe Standard" || p.name == "Embedded" || p.name == "Matrix" || own_lut(&p.name).is_some();
                (!p.camera.trim().is_empty() && !reserved).then(|| Arc::new(p))
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    })
}

/// User profiles for this camera
fn user_profile(camera: &str, name: &str) -> Option<Arc<Profile>> {
    user_profiles().iter().find(|p| p.name == name && p.camera.trim().eq_ignore_ascii_case(camera.trim())).cloned()
}

// ───────────────────────── Color spec (DNG color model) ─────────────────────────

pub struct ColorSpec {
    t1: f64,
    t2: f64,
    cm1: M3,
    cm2: M3,
    fm1: Option<M3>,
    fm2: Option<M3>,
    pub profile: Arc<Profile>,
}

fn normalize_fm(m: M3) -> M3 {
    let xyz = mulv(&m, [1.0, 1.0, 1.0]);
    let p = pcs_xyz();
    mul(&diag([p[0] / xyz[0], p[1] / xyz[1], p[2] / xyz[2]]), &m)
}

impl ColorSpec {
    pub fn new(profile: Arc<Profile>) -> Self {
        let cm1 = profile.cm1.unwrap();
        let mut t1 = illuminant_temp(profile.illum1);
        let mut t2 = illuminant_temp(profile.illum2);
        let mut fm1 = profile.fm1.map(normalize_fm);
        let (mut cm1_, mut cm2_, mut fm2);
        cm1_ = cm1;
        match profile.cm2 {
            Some(cm2) if t1 > 0.0 && t2 > 0.0 && t1 != t2 => {
                cm2_ = cm2;
                fm2 = profile.fm2.map(normalize_fm);
                if t1 > t2 {
                    std::mem::swap(&mut t1, &mut t2);
                    std::mem::swap(&mut cm1_, &mut cm2_);
                    std::mem::swap(&mut fm1, &mut fm2);
                }
            }
            _ => {
                t1 = 5000.0;
                t2 = 5000.0;
                cm2_ = cm1_;
                fm2 = fm1;
            }
        }
        Self { t1, t2, cm1: cm1_, cm2: cm2_, fm1, fm2, profile }
    }

    /// Interpolation weight from the white point's color temperature (toward the first illuminant)
    fn weight(&self, white: [f64; 2]) -> f64 {
        let (t, _) = xy_to_temp(white);
        if t <= self.t1 {
            1.0
        } else if t >= self.t2 {
            0.0
        } else {
            (1.0 / t - 1.0 / self.t2) / (1.0 / self.t1 - 1.0 / self.t2)
        }
    }

    fn find_xyz_to_camera(&self, white: [f64; 2]) -> (M3, Option<M3>, f64) {
        let g = self.weight(white);
        let cm = if g >= 1.0 {
            self.cm1
        } else if g <= 0.0 {
            self.cm2
        } else {
            lerp_m(&self.cm1, &self.cm2, g)
        };
        let fm = match (self.fm1, self.fm2) {
            (Some(a), Some(b)) => Some(if g >= 1.0 {
                a
            } else if g <= 0.0 {
                b
            } else {
                lerp_m(&a, &b, g)
            }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            _ => None,
        };
        (cm, fm, g)
    }

    /// Camera neutral (as-shot neutral) to white xy (iterative)
    pub fn neutral_to_xy(&self, neutral: [f64; 3]) -> [f64; 2] {
        let mut last = D50_XY;
        for pass in 0..30 {
            let (cm, _, _) = self.find_xyz_to_camera(last);
            let mut next = xyz_to_xy(mulv(&inv(&cm), neutral));
            if (next[0] - last[0]).abs() + (next[1] - last[1]).abs() < 1e-7 {
                return next;
            }
            if pass == 29 {
                next = [(last[0] + next[0]) * 0.5, (last[1] + next[1]) * 0.5];
            }
            last = next;
        }
        last
    }

    /// White xy to (camera white, camera-to-ProPhoto linear matrix, HSM weight)
    pub fn camera_to_prophoto(&self, white: [f64; 2]) -> ([f64; 3], M3, f64) {
        let (cm, fm, g) = self.find_xyz_to_camera(white);
        let mut cam_white = mulv(&cm, xy_to_xyz(white));
        let m = max3(cam_white).max(1e-9);
        for v in &mut cam_white {
            *v = (*v / m).clamp(0.001, 1.0);
        }
        let to_pcs = match fm {
            Some(fm) => mul(&fm, &inv(&diag(cam_white))),
            None => {
                let pcs_to_cam = mul(&cm, &map_white_matrix(D50_XY, white));
                let scale = max3(mulv(&pcs_to_cam, pcs_xyz())).max(1e-9);
                inv(&scale_m(&pcs_to_cam, 1.0 / scale))
            }
        };
        (cam_white, mul(&inv(&PROPHOTO_TO_PCS), &to_pcs), g)
    }

    pub fn hsm_for(&self, g: f64) -> Option<HsvTable> {
        let p = &self.profile;
        match (&p.hsm1, &p.hsm2) {
            (Some(a), Some(b)) if a.data.len() == b.data.len() => {
                // HSM interpolation follows the profile's original illuminant order
                let (t1, t2) = (illuminant_temp(p.illum1), illuminant_temp(p.illum2));
                if t1 > t2 { Some(HsvTable::lerp(b, a, g)) } else { Some(HsvTable::lerp(a, b, g)) }
            }
            (Some(a), _) => Some(a.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_roundtrip() {
        for (t, ti) in [(2850.0, 0.0), (5000.0, 10.0), (6500.0, -20.0), (10000.0, 5.0)] {
            let xy = temp_to_xy(t, ti);
            let (t2, ti2) = xy_to_temp(xy);
            assert!((t2 - t).abs() / t < 0.002, "{t} → {t2}");
            assert!((ti2 - ti).abs() < 0.5, "{ti} → {ti2}");
        }
        // D50 is about 5003K
        let (t, _) = xy_to_temp(D50_XY);
        assert!((t - 5003.0).abs() < 20.0, "{t}");
    }

    #[test]
    fn rgb_tone_preserves_order_and_gray() {
        let c = |x: f32| x * x;
        let g = rgb_tone([0.5, 0.5, 0.5], &c);
        assert!((g[0] - 0.25).abs() < 1e-6 && g[0] == g[1] && g[1] == g[2]);
        let o = rgb_tone([0.8, 0.5, 0.2], &c);
        assert!(o[0] > o[1] && o[1] > o[2]);
    }

}

// ───────────────────────── RAW color info / render transforms ─────────────────────────

/// Color info read from the RAW at decode time (pixel data is camera-native RGB before white balance).
pub struct RawColor {
    /// Camera name used for profile lookup (e.g. "Canon EOS 70D")
    pub camera: String,
    /// As-shot camera neutral (max 1)
    pub neutral: [f64; 3],
    pub baseline_exposure: f64,
    /// Profile embedded in a DNG
    pub embedded: Option<Arc<Profile>>,
    /// Fallback profile built from rawler's color matrix when no profile is found
    pub fallback: Arc<Profile>,
    /// HDR merge result (no sensor white clipping; values above 1.0 are valid)
    pub hdr: bool,
    /// Set when the model is unknown to the decoder and was opened with this similar model's settings
    pub decoder_fallback: Option<String>,
}

/// Default RAW profile name (kept compatible with XMP develop settings)
pub const DEFAULT_PROFILE: &str = "Adobe Color";

/// Profile name to (base profile, look table override, effective name)
pub fn resolve_profile(rc: &RawColor, name: &str) -> (Arc<Profile>, Option<HsvTable>, String) {
    let name = if name.is_empty() { DEFAULT_PROFILE } else { name };
    if name == "Embedded"
        && let Some(e) = &rc.embedded {
            return (e.clone(), None, name.into());
        }
    // A user-supplied DCP was selected
    if let Some(p) = user_profile(&rc.camera, name) {
        return (p, None, name.into());
    }
    // Priority: embedded DNG profile, built-in profile, generic profile, matrix
    if let Some(e) = &rc.embedded {
        // Matrix-only DNGs (no color tables, e.g. in-camera DNGs): add the generic profile's shared tables, keep the file's BaselineExposure.
        // Test switch: DARKROOM_DNG_TABLE=0 disables this.
        if e.hsm1.is_none() && e.look.is_none() && std::env::var("DARKROOM_DNG_TABLE").as_deref() != Ok("0")
            && let Some(g) = generic_profile(e, &rc.camera) {
                let mut p = (*g).clone();
                p.baseline_exposure_offset = 0.0;
                p.name = "Embedded".into();
                return (Arc::new(p), None, "Embedded".into());
            }
        return (e.clone(), None, "Embedded".into());
    }
    if let Some(p) = own_profile(&rc.fallback, &rc.camera, name) {
        let n = p.name.clone();
        return (p, None, n);
    }
    // Model without a built-in profile: public matrix + shared multi-maker color tables + per-maker exposure default
    if let Some(p) = generic_profile(&rc.fallback, &rc.camera) {
        return (p, None, OWN_PROFILE.into());
    }
    (rc.fallback.clone(), None, "Matrix".into())
}

/// Maker key for exposure defaults: first word of the camera name, Olympus and OM merged
pub fn maker_key(camera: &str) -> String {
    let m = camera.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    if m == "OM" || m == "OLYMPUS" { "OLYMPUS/OM".into() } else { m }
}

fn generic_profile(fallback: &Profile, camera: &str) -> Option<Arc<Profile>> {
    if std::env::var("DARKROOM_GENERIC").map(|v| v == "0").unwrap_or(false) {
        return None;
    }
    let d = own_profiles().iter().find(|p| p.camera == "*")?;
    let mut p = fallback.clone();
    p.name = OWN_PROFILE.into();
    p.camera = camera.into();
    p.hsm1 = Some(HsvTable { hue: d.hue_divs, sat: d.sat_divs, val: 1, data: d.hsm1.clone(), srgb_encoding: false });
    p.hsm2 = Some(HsvTable { hue: d.hue_divs, sat: d.sat_divs, val: 1, data: d.hsm2.clone(), srgb_encoding: false });
    // Two tables need two illuminants: use the single public matrix for both
    if p.cm2.is_none() {
        p.illum2 = 21;
        p.illum1 = 17;
        p.cm2 = p.cm1;
    }
    p.baseline_exposure_offset = d.maker_exposure.get(&maker_key(camera)).copied().unwrap_or(0.0);
    Some(Arc::new(p))
}

/// Built-in look cube: maps sRGB-gamma-encoded linear ProPhoto (after the tone curve) to output in the same space.
/// Fitted from pairs of standard and look reference renders of the same photos (`assets/looks.json`); camera independent.
#[derive(Debug, serde::Deserialize)]
pub struct OwnLut {
    pub name: String,
    pub n: usize,
    /// Index order (r*n + g)*n + b
    pub data: Vec<[f32; 3]>,
}

impl OwnLut {
    fn enc(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    }
    fn dec(v: f32) -> f32 {
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }
    /// Apply the look to linear ProPhoto (after the tone curve); amount 0..2 (1 = as fitted)
    pub fn apply(&self, p: [f32; 3], amount: f32) -> [f32; 3] {
        let n = self.n;
        let e = [Self::enc(p[0]), Self::enc(p[1]), Self::enc(p[2])];
        let pos = |x: f32| {
            let t = x * (n - 1) as f32;
            let i = (t as usize).min(n - 2);
            (i, t - i as f32)
        };
        let (ri, rf) = pos(e[0]);
        let (gi, gf) = pos(e[1]);
        let (bi, bf) = pos(e[2]);
        let mut o = [0.0f32; 3];
        for (dr, wr) in [(0, 1.0 - rf), (1, rf)] {
            for (dg, wg) in [(0, 1.0 - gf), (1, gf)] {
                for (db, wb) in [(0, 1.0 - bf), (1, bf)] {
                    let w = wr * wg * wb;
                    if w == 0.0 {
                        continue;
                    }
                    let v = self.data[((ri + dr) * n + gi + dg) * n + bi + db];
                    for c in 0..3 {
                        o[c] += w * v[c];
                    }
                }
            }
        }
        let l = [Self::dec(o[0].clamp(0.0, 1.0)), Self::dec(o[1].clamp(0.0, 1.0)), Self::dec(o[2].clamp(0.0, 1.0))];
        if (amount - 1.0).abs() < 1e-4 {
            return l;
        }
        [p[0] + (l[0] - p[0]) * amount, p[1] + (l[1] - p[1]) * amount, p[2] + (l[2] - p[2]) * amount]
    }
}

fn own_luts() -> &'static Vec<Arc<OwnLut>> {
    static L: OnceLock<Vec<Arc<OwnLut>>> = OnceLock::new();
    L.get_or_init(|| {
        if std::env::var("DARKROOM_OWN_LOOKS").as_deref() == Ok("0") {
            return Vec::new();
        }
        let text = std::env::var_os("DARKROOM_OWN_LOOKS").and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| include_str!("../../assets/looks.json").to_string());
        let v: Vec<OwnLut> = serde_json::from_str(&text).unwrap_or_default();
        v.into_iter().filter(|l| l.n >= 2 && l.data.len() == l.n * l.n * l.n).map(Arc::new).collect()
    })
}

/// Built-in look, looked up by its profile name
pub fn own_lut(name: &str) -> Option<Arc<OwnLut>> {
    own_luts().iter().find(|l| l.name.eq_ignore_ascii_case(name)).cloned()
}

/// Profile name shown in the UI
/// (settings and XMP keep the standard profile names for compatibility; only the names are used, no external profile files are read)
pub fn profile_display(name: &str) -> String {
    match name {
        "Adobe Standard" | "Adobe Standard v2" => tr!("표준", "Standard").into(),
        "Adobe Color" => tr!("컬러", "Color").into(),
        "Matrix" => tr!("기본 행렬", "Basic matrix").into(),
        n if !profile_known(n) => trf!("{n} (없음 — 표준으로)", "{n} (not available — Standard)"),
        n => n.to_string(),
    }
}

/// Whether Darkroom has this profile (unknown profiles named by other programs' presets render as Standard)
pub fn profile_known(name: &str) -> bool {
    name.is_empty()
        || matches!(name, "Adobe Standard" | "Adobe Standard v2" | "Embedded" | "Matrix" | OWN_PROFILE)
        || own_lut(name).is_some()
        || user_profiles().iter().any(|p| p.name == name)
}

/// Built-in camera profile name (public color matrix plus forward matrices and HSV tables fitted to reference renders)
pub const OWN_PROFILE: &str = "Darkroom Standard";

#[derive(serde::Deserialize)]
struct OwnProfileData {
    camera: String,
    #[serde(default)]
    cm1: Option<M3>,
    #[serde(default)]
    cm2: Option<M3>,
    /// 1: tungsten (A), 2: daylight (D65)
    fm1: M3,
    fm2: M3,
    exposure_offset: f64,
    hue_divs: usize,
    sat_divs: usize,
    hsm1: Vec<[f32; 3]>,
    hsm2: Vec<[f32; 3]>,
    #[serde(default)]
    looks: Vec<OwnLookData>,
    /// Compute calibration sliders from the public color matrix primaries (models whose fitted primaries are far off)
    #[serde(default)]
    cal_public: bool,
    /// Generic profile ("*" entry): per-maker exposure defaults (median exposure offset of that maker's models)
    #[serde(default)]
    maker_exposure: std::collections::HashMap<String, f64>,
    /// Reference white level (for models that record different white levels per ISO; see `white_level_offset`)
    #[serde(default)]
    ref_white: Option<f64>,
}

#[derive(serde::Deserialize)]
struct OwnLookData {
    name: String,
    hue_divs: usize,
    sat_divs: usize,
    val_divs: usize,
    data: Vec<[f32; 3]>,
}

/// Number of loaded built-in profiles (lets the fitting tool check whether the set is empty)
#[allow(dead_code)]
pub fn own_profile_count() -> usize {
    own_profiles().len()
}

fn own_profiles() -> &'static Vec<OwnProfileData> {
    static P: OnceLock<Vec<OwnProfileData>> = OnceLock::new();
    P.get_or_init(|| {
        // Test switch: DARKROOM_OWN_PROFILES=<profiles.json> loads a different profile file
        let text = std::env::var_os("DARKROOM_OWN_PROFILES").and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| include_str!("../../assets/profiles.json").to_string());
        if std::env::var("DARKROOM_OWN_PROFILES").as_deref() == Ok("0") {
            return Vec::new();
        }
        serde_json::from_str(&text).unwrap_or_default()
    })
}

/// Built-in profile for this camera (on top of the public `fallback` matrix). If `look` names a built-in look, its look table is applied.
pub fn own_profile(fallback: &Profile, camera: &str, look: &str) -> Option<Arc<Profile>> {
    let d = own_profiles().iter().find(|p| p.camera.eq_ignore_ascii_case(camera))?;
    let mut p = fallback.clone();
    p.name = OWN_PROFILE.into();
    p.cal_public = d.cal_public;
    // Models without a per-camera look table fall back to the shared look cube (own_lut)
    if let Some(l) = d.looks.iter().find(|l| l.name.eq_ignore_ascii_case(look)) {
        p.look = Some(HsvTable { hue: l.hue_divs, sat: l.sat_divs, val: l.val_divs, data: l.data.clone(), srgb_encoding: true });
        p.name = l.name.clone();
    }
    p.camera = camera.into();
    // Two illuminants: the public color matrix on both sides (for white balance); forward matrices and HSV tables per illuminant
    p.illum1 = 17;
    p.illum2 = 21;
    p.cm2 = p.cm1;
    if let (Some(a), Some(b)) = (d.cm1, d.cm2) {
        p.cm1 = Some(a);
        p.cm2 = Some(b);
    }
    p.fm1 = Some(d.fm1);
    p.fm2 = Some(d.fm2);
    p.hsm1 = Some(HsvTable { hue: d.hue_divs, sat: d.sat_divs, val: 1, data: d.hsm1.clone(), srgb_encoding: false });
    p.hsm2 = Some(HsvTable { hue: d.hue_divs, sat: d.sat_divs, val: 1, data: d.hsm2.clone(), srgb_encoding: false });
    p.baseline_exposure_offset = d.exposure_offset;
    // Calibration slider basis: use the public matrix when the fitted primaries (xy of camera R, G, B) are far from the public ones
    p.cal_public = d.cal_public || primary_distance(&p, fallback) > crate::config::CAL_PRIMARY_DIST;
    Some(Arc::new(p))
}

/// Largest distance between the two profiles' primaries (xy of the camera R, G, B unit vectors) at a 5500K white
fn primary_distance(a: &Profile, b: &Profile) -> f64 {
    let w = temp_to_xy(5500.0, 0.0);
    let (_, ma, _) = ColorSpec::new(Arc::new(a.clone())).camera_to_prophoto(w);
    let (_, mb, _) = ColorSpec::new(Arc::new(b.clone())).camera_to_prophoto(w);
    let (xa, xb) = (mul(&PROPHOTO_TO_PCS, &ma), mul(&PROPHOTO_TO_PCS, &mb));
    let xy = |x: &M3, j: usize| {
        let s = x[0][j] + x[1][j] + x[2][j];
        if s.abs() < 1e-9 { (f64::INFINITY, f64::INFINITY) } else { (x[0][j] / s, x[1][j] / s) }
    };
    (0..3).map(|j| {
        let (p, q) = (xy(&xa, j), xy(&xb, j));
        (p.0 - q.0).hypot(p.1 - q.1)
    }).fold(0.0, f64::max)
}

/// Input color transform fixed for one render (same stage order as the DNG specification).
#[allow(dead_code)]
pub struct ColorXform {
    pub cam_white: [f32; 3],
    /// Camera to linear ProPhoto
    pub to_pp: [[f32; 3]; 3],
    pub hsm: Option<HsvTable>,
    pub look: Option<HsvTable>,
    /// Built-in look cube and its amount
    pub own_lut: Option<(Arc<OwnLut>, f32)>,
    /// 2^max(0, total exposure)
    pub gain: f32,
    /// Tone curve table combining negative exposure (handled as a tone curve) and the base curve (linear 0..1)
    pub tone: Vec<f32>,
    pub white_xy: [f64; 2],
}

impl ColorXform {
    #[inline]
    pub fn camera_to_working(&self, p: [f32; 3]) -> [f32; 3] {
        let mut a = p[0].min(self.cam_white[0]);
        let mut b = p[1].min(self.cam_white[1]);
        let mut c = p[2].min(self.cam_white[2]);
        // Near sensor saturation one clipped channel (usually G) gives pink/purple blotches, so blend toward neutral white by saturation level
        let ko = hl_knee();
        if ko < 1.0 {
            let r = (a / self.cam_white[0]).max(b / self.cam_white[1]).max(c / self.cam_white[2]);
            if r > ko {
                let t = ((r - ko) / (1.0 - ko)).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                a += (self.cam_white[0] * r - a) * t;
                b += (self.cam_white[1] * r - b) * t;
                c += (self.cam_white[2] * r - c) * t;
            }
        }
        let m = &self.to_pp;
        // No upper clip here: clipping channels separately near white shifts hue and causes color contours.
        // Values above 1 are handled by the tone stage and the hue-preserving roll-off in finish().
        let mut q = [
            m[0][0] * a + m[0][1] * b + m[0][2] * c,
            m[1][0] * a + m[1][1] * b + m[1][2] * c,
            m[2][0] * a + m[2][1] * b + m[2][2] * c,
        ];
        // Out of gamut (negative channels): per-channel clipping flattens saturated colors into contours,
        // so reduce saturation just enough to reach 0 while keeping luminance
        let mn = q[0].min(q[1]).min(q[2]);
        if mn < 0.0 {
            let y = (0.288_04 * q[0] + 0.711_874 * q[1] + 0.000_086 * q[2]).max(0.0);
            if gamut_mode() == 0 && y > 0.0 {
                let t = y / (y - mn);
                for v in &mut q {
                    *v = (y + (*v - y) * t).max(0.0);
                }
            } else {
                for v in &mut q {
                    *v = v.max(0.0);
                }
            }
        }
        if let Some(h) = &self.hsm {
            q = h.apply(q);
        }
        [q[0] * self.gain, q[1] * self.gain, q[2] * self.gain]
    }

    #[inline]
    pub fn tone_eval(&self, x: f32) -> f32 {
        let n = (self.tone.len() - 1) as f32;
        let y = x.clamp(0.0, 1.0) * n;
        let i = (y as usize).min(self.tone.len() - 2);
        let f = y - i as f32;
        self.tone[i] * (1.0 - f) + self.tone[i + 1] * f
    }

    /// Look table + tone curve (linear ProPhoto in, linear ProPhoto out)
    #[inline]
    pub fn finish(&self, p: [f32; 3]) -> [f32; 3] {
        let mut q = [p[0].max(0.0), p[1].max(0.0), p[2].max(0.0)];
        // Above 1: roll toward white keeping hue instead of clipping per channel (q/m + (1 - q/m)(1 - 1/m)), continuous at m=1
        let m = q[0].max(q[1]).max(q[2]);
        if m > 1.0 && clip_mode() == 0 {
            let t = 1.0 - 1.0 / m;
            for v in &mut q {
                let n = *v / m;
                *v = n + (1.0 - n) * t;
            }
        } else {
            for v in &mut q {
                *v = v.min(1.0);
            }
        }
        if let Some(l) = &self.look {
            q = l.apply(q);
        }
        let mut o = rgb_tone(q, &|x| self.tone_eval(x));
        if let Some((l, a)) = &self.own_lut {
            o = l.apply(o, *a);
        }
        o
    }
}

/// Start of near-saturation neutralization (relative to sensor white, 1 = off). Test switch: DARKROOM_HLKNEE=0.9
fn hl_knee() -> f32 {
    static M: OnceLock<f32> = OnceLock::new();
    *M.get_or_init(|| std::env::var("DARKROOM_HLKNEE").ok().and_then(|v| v.parse().ok()).unwrap_or(0.9))
}

/// Out-of-gamut handling (0 = desaturate keeping luminance, 1 = per-channel clip). Test switch: DARKROOM_GAMUT=1
fn gamut_mode() -> u8 {
    static M: OnceLock<u8> = OnceLock::new();
    *M.get_or_init(|| if std::env::var("DARKROOM_GAMUT").map(|v| v == "1").unwrap_or(false) { 1 } else { 0 })
}

/// Handling above 1 (0 = hue-preserving roll-off, 1 = per-channel clip). Test switch: DARKROOM_CLIP=1
fn clip_mode() -> u8 {
    static M: OnceLock<u8> = OnceLock::new();
    *M.get_or_init(|| if std::env::var("DARKROOM_CLIP").map(|v| v == "1").unwrap_or(false) { 1 } else { 0 })
}

/// Camera calibration: rotate (hue) and scale (saturation) the chromaticity of each camera primary (column) around the white point,
/// then rescale the columns so camera white still maps to white. cal = [R hue, R sat, G hue, G sat, B hue, B sat] (-100..100)
pub fn apply_calibration(m: &M3, cam_white: [f64; 3], cal: [f64; 6], k_hue: [f64; 3], k_sat: [f64; 3]) -> M3 {
    if cal.iter().all(|v| *v == 0.0) {
        return *m;
    }
    let x = mul(&PROPHOTO_TO_PCS, m);
    let target = mulv(&x, cam_white);
    let ts = (target[0] + target[1] + target[2]).max(1e-12);
    let (wx, wy) = (target[0] / ts, target[1] / ts);
    let mut nx = x;
    for j in 0..3 {
        let col = [x[0][j], x[1][j], x[2][j]];
        let sum = col[0] + col[1] + col[2];
        if sum.abs() < 1e-12 {
            continue;
        }
        let (cx, cy) = (col[0] / sum, col[1] / sum);
        let (dx, dy) = (cx - wx, cy - wy);
        let th = (cal[j * 2] * k_hue[j]).to_radians();
        let sc = 1.0 + cal[j * 2 + 1] / 100.0 * k_sat[j];
        let (rx, ry) = ((dx * th.cos() - dy * th.sin()) * sc, (dx * th.sin() + dy * th.cos()) * sc);
        let (nxx, nyy) = (wx + rx, (wy + ry).max(1e-4));
        // Keep the same sum: XYZ = sum * (x, y, 1-x-y)
        nx[0][j] = sum * nxx;
        nx[1][j] = sum * nyy;
        nx[2][j] = sum * (1.0 - nxx - nyy);
    }
    // Column scale d: nx * diag(d) * cam_white = target
    let u = [
        [nx[0][0] * cam_white[0], nx[0][1] * cam_white[1], nx[0][2] * cam_white[2]],
        [nx[1][0] * cam_white[0], nx[1][1] * cam_white[1], nx[1][2] * cam_white[2]],
        [nx[2][0] * cam_white[0], nx[2][1] * cam_white[1], nx[2][2] * cam_white[2]],
    ];
    let d = mulv(&inv(&u), target);
    let mut out = nx;
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = nx[i][j] * d[j];
        }
    }
    mul(&inv(&PROPHOTO_TO_PCS), &out)
}

/// look_amount: profile amount slider (0..2)
#[allow(clippy::too_many_arguments)]
pub fn build_xform_full(rc: &RawColor, profile_name: &str, wb: Option<(f64, f64)>, exposure: f64, cal: [f64; 6], k_hue: [f64; 3], k_sat: [f64; 3], look_amount: f64) -> ColorXform {
    let (prof, look_override, _) = resolve_profile(rc, profile_name);
    let spec = ColorSpec::new(prof.clone());
    let white_xy = match wb {
        Some((t, ti)) => {
            let (ti_t, ti_ti) = wb_to_internal(&rc.camera, t, ti);
            temp_to_xy(ti_t, ti_ti)
        }
        None => spec.neutral_to_xy(rc.neutral),
    };
    let (cw, m, g) = spec.camera_to_prophoto(white_xy);
    let force = std::env::var("DARKROOM_OWN_CALIB").ok();
    let use_public = match force.as_deref() {
        Some("0") => false,
        Some("1") => prof.name == OWN_PROFILE || prof.cal_public,
        _ => prof.cal_public,
    };
    let m = if use_public && !cal.iter().all(|v| *v == 0.0) {
        // When the fitted primaries are far off, the same slider value acts much stronger, so compute the change from the
        // public matrix primaries and apply it as a ProPhoto-space correction matrix (white unchanged)
        let (cw_ref, m_ref, _) = ColorSpec::new(rc.fallback.clone()).camera_to_prophoto(white_xy);
        let m_cal = apply_calibration(&m_ref, cw_ref, cal, k_hue, k_sat);
        mul(&mul(&m_cal, &inv(&m_ref)), &m)
    } else {
        apply_calibration(&m, cw, cal, k_hue, k_sat)
    };
    let hsm = spec.hsm_for(g);
    let look = look_override.or_else(|| prof.look.clone());
    let total = exposure + rc.baseline_exposure + prof.baseline_exposure_offset;
    // Exposure (user + baseline) is a linear gain regardless of sign, which matches reference renders.
    // Test switch: DARKROOM_EXPO_RAMP=1 uses the DNG-style exposure tone instead.
    let ramp = std::env::var("DARKROOM_EXPO_RAMP").as_deref() == Ok("1");
    let et = exposure_tone(if ramp { total } else { 0.0 });
    let curve: Box<dyn Fn(f32) -> f32> = match &prof.tone_curve {
        Some(pts) if pts.len() >= 2 => {
            let pts = pts.clone();
            Box::new(move |x| super::color::monotone_spline(&pts)(x))
        }
        _ => Box::new(base_tone),
    };
    let n = 4096;
    let tone = (0..n).map(|i| curve(et(i as f32 / (n - 1) as f32))).collect();
    let to_pp = [
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
    ];
    let cw = if rc.hdr { [1.0e6; 3] } else { cw };
    let lut_name = if profile_name.is_empty() { DEFAULT_PROFILE } else { profile_name };
    ColorXform {
        cam_white: [cw[0] as f32, cw[1] as f32, cw[2] as f32],
        to_pp,
        hsm,
        look,
        // Built-in look cube. A per-camera HSV look table is preferred when present (closer per camera);
        // with an embedded DNG look table only the default look is replaced, creative looks are applied on top
        own_lut: if prof.look.is_none() || (prof.name != OWN_PROFILE && lut_name != DEFAULT_PROFILE) { own_lut(lut_name).map(|l| (l, look_amount as f32)) } else { None },
        gain: 2f64.powf(if ramp { total.max(0.0) } else { total }) as f32,
        tone,
        white_xy,
    }
}

/// As-shot temperature/tint (for UI display)
pub fn as_shot_temp_tint(rc: &RawColor, profile_name: &str) -> (f64, f64) {
    let (prof, _, _) = resolve_profile(rc, profile_name);
    let (t, ti) = xy_to_temp(ColorSpec::new(prof).neutral_to_xy(rc.neutral));
    wb_to_display(&rc.camera, t, ti)
}

/// White balance eyedropper: camera-native RGB sample to temperature/tint
pub fn temp_tint_from_camera_rgb(rc: &RawColor, profile_name: &str, cam: [f32; 3]) -> Option<(f64, f64)> {
    let m = cam[0].max(cam[1]).max(cam[2]) as f64;
    if m <= 1e-6 || cam.iter().any(|v| *v <= 0.0) {
        return None;
    }
    let (prof, _, _) = resolve_profile(rc, profile_name);
    let xy = ColorSpec::new(prof).neutral_to_xy([cam[0] as f64 / m, cam[1] as f64 / m, cam[2] as f64 / m]);
    let (t, ti) = xy_to_temp(xy);
    Some(wb_to_display(&rc.camera, t, ti))
}

// ───────────────────────── Per-camera baseline exposure ─────────────────────────

/// Per-camera baseline exposure (EV) that matches default render brightness, measured against reference renders.
const MEASURED_BASELINE: &[(&str, f64)] = &[
    ("Canon EOS 70D", -0.20),
    ("Canon EOS R", 0.05),
    ("Canon EOS R6", 0.0),
    ("Canon EOS R7", 0.0),
    ("Canon EOS RP", 0.15),
    ("Canon EOS M100", -0.30),
    ("Canon EOS R8", -0.20),
    ("Canon EOS R5", 0.25),
    ("Canon EOS M6", -0.25),
    ("Sony ILCE-7RM2", 0.15),
];

/// Per-camera nominal white level. Some ISOs record a lowered white level, while reference renders still use
/// the nominal level for brightness, so those photos render darker by the difference.
const NOMINAL_WHITE: &[(&str, f64)] = &[
    ("Canon EOS R", 14448.0),
    ("Canon EOS R5", 14008.0),
    ("Canon EOS R6", 14888.0),
    ("Canon EOS R7", 12735.0),
    ("Canon EOS R8", 12735.0),
    ("Canon EOS RP", 14558.0),
    ("Canon EOS M100", 11892.0),
    ("Canon EOS M6", 11892.0),
];

/// Exposure correction (EV, negative) when a photo's white level is below the camera's nominal value.
/// Models not in the table use the built-in profile's reference white (`ref_white`), because brightness follows a
/// fixed per-model level rather than the white level written in each file.
pub fn white_level_offset(camera: &str, white: f64, black: f64) -> f64 {
    if white <= black + 100.0 {
        return 0.0;
    }
    if let Some((_, nw)) = NOMINAL_WHITE.iter().find(|(c, _)| c.eq_ignore_ascii_case(camera)) {
        return if white < nw * 0.97 { ((white - black) / (nw - black)).log2() } else { 0.0 };
    }
    let fit = FIT_REF_WHITE.lock().get(&camera.to_ascii_lowercase()).copied();
    let rw = fit.or_else(|| own_profiles().iter().find(|p| p.camera.eq_ignore_ascii_case(camera)).and_then(|p| p.ref_white));
    // Only ISO-dependent adjustments (around 0.8x); more than 0.5EV apart means a different bit depth or storage format
    match rw {
        Some(rw) if rw > black + 100.0 => {
            let ev = ((white - black) / (rw - black)).log2();
            if ev.abs() > 0.02 && ev.abs() < 0.5 { ev } else { 0.0 }
        }
        _ => 0.0,
    }
}

/// Per-model reference white level used by the fitting tool (same rule as at runtime)
static FIT_REF_WHITE: parking_lot::Mutex<std::collections::BTreeMap<String, f64>> = parking_lot::Mutex::new(std::collections::BTreeMap::new());

#[allow(dead_code)]
pub fn set_fit_ref_white(camera: &str, white: f64) {
    FIT_REF_WHITE.lock().insert(camera.to_ascii_lowercase(), white);
}

/// User measurement file: %LOCALAPPDATA%\Darkroom\camera_baseline.json  {"camera": EV}
pub fn camera_baseline_exposure(camera: &str) -> f64 {
    static USER: OnceLock<HashMap<String, f64>> = OnceLock::new();
    let user = USER.get_or_init(|| {
        std::fs::read_to_string(crate::config::data_dir().join("camera_baseline.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    });
    if let Some(v) = user.get(camera) {
        return *v;
    }
    MEASURED_BASELINE.iter().find(|(c, _)| c.eq_ignore_ascii_case(camera)).map(|(_, v)| *v).unwrap_or(0.0)
}

/// White balance presets (RAW): name, temperature, tint
pub const WB_PRESETS: &[(&str, f64, f64)] = &[
    ("주광", 5500.0, 10.0),
    ("흐림", 6500.0, 10.0),
    ("그늘", 7500.0, 10.0),
    ("텅스텐", 2850.0, 0.0),
    ("형광등", 3800.0, 21.0),
    ("플래시", 5500.0, 0.0),
];

/// Auto white balance: gray-world average of camera-native RGB, excluding clipped, dark and highly saturated pixels
pub fn auto_wb(src: &super::image::SourceImage, profile_name: &str) -> Option<(f64, f64)> {
    let rc = src.raw_color.as_ref()?;
    let lvl = src.levels.last()?;
    let mut acc = [0.0f64; 3];
    let mut n = 0.0;
    for p in lvl.data.as_chunks::<3>().0 {
        let m = p[0].max(p[1]).max(p[2]);
        if !(0.02..=0.95).contains(&m) {
            continue;
        }
        let mn = p[0].min(p[1]).min(p[2]);
        if mn / m < 0.15 {
            continue;
        }
        for c in 0..3 {
            acc[c] += p[c] as f64;
        }
        n += 1.0;
    }
    if n < 100.0 {
        return None;
    }
    temp_tint_from_camera_rgb(rc, profile_name, [(acc[0] / n) as f32, (acc[1] / n) as f32, (acc[2] / n) as f32])
}

/// Profile list for the UI menu: [("Basic", [...]), ("User", [...])]. Settings store the compatible names, the UI shows profile_display names.
pub fn profile_groups(rc: &RawColor) -> Vec<(String, Vec<String>)> {
    let mut v = vec!["Adobe Standard".to_string()];
    v.extend(own_luts().iter().map(|l| l.name.clone()));
    let mut out = vec![(tr!("기본", "Basic").into(), v)];
    let user: Vec<String> = user_profiles().iter().filter(|p| p.camera.trim().eq_ignore_ascii_case(rc.camera.trim())).map(|p| p.name.clone()).collect();
    if !user.is_empty() {
        out.push((tr!("사용자", "User").into(), user));
    }
    out
}

/// Whether this is a creative profile with an adjustable amount (the default look has none)
pub fn profile_supports_amount(name: &str) -> bool {
    name != DEFAULT_PROFILE && own_lut(name).is_some()
}


// ───────────────────────── Custom white balance offsets ─────────────────────────

/// Per-camera offset (mired, tint) from displayed temperature/tint numbers to internal engine values,
/// derived so that the same numbers match reference renders (tools/calib).
const MEASURED_WB: &[(&str, f64, f64)] = &[
    ("Canon EOS 70D", -7.9, 10.0),
    ("Canon EOS R", -5.6, -2.9),
    ("Canon EOS R6", -6.2, -0.4),
    ("Canon EOS R7", 4.3, 3.0),
    ("Canon EOS M100", -12.6, 5.9),
    ("Canon EOS R8", -5.6, 0.1),
    ("Canon EOS R5", -2.6, -1.9),
    ("Canon EOS M6", -5.6, 1.1),
    ("Sony ILCE-7RM2", -18.0, 7.0),
];

fn wb_offset(camera: &str) -> (f64, f64) {
    // Built-in profiles already align their color matrices to the recorded temperature, so no offset
    if own_profiles().iter().any(|p| p.camera.eq_ignore_ascii_case(camera) && p.cm1.is_some()) {
        return (0.0, 0.0);
    }
    static USER: OnceLock<HashMap<String, (f64, f64)>> = OnceLock::new();
    let user = USER.get_or_init(|| {
        std::fs::read_to_string(crate::config::data_dir().join("camera_wb.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    });
    if let Some(v) = user.get(camera) {
        return *v;
    }
    if let Some((_, m, t)) = MEASURED_WB.iter().find(|(c, _, _)| c.eq_ignore_ascii_case(camera)) {
        return (*m, *t);
    }
    // Unmeasured camera: average of the same maker's measurements (overall average if none)
    let make = camera.split_whitespace().next().unwrap_or("").to_lowercase();
    let same: Vec<&(&str, f64, f64)> = MEASURED_WB.iter().filter(|x| x.0.to_lowercase().starts_with(&make)).collect();
    let pool: Vec<&(&str, f64, f64)> = if same.is_empty() { MEASURED_WB.iter().collect() } else { same };
    let n = pool.len() as f64;
    (pool.iter().map(|x| x.1).sum::<f64>() / n, pool.iter().map(|x| x.2).sum::<f64>() / n)
}

/// Displayed value to internal engine value
pub fn wb_to_internal(camera: &str, t: f64, ti: f64) -> (f64, f64) {
    let (dm, dt) = wb_offset(camera);
    (1.0e6 / (1.0e6 / t.max(1000.0) + dm).max(10.0), ti + dt)
}

/// Internal engine value to displayed value
pub fn wb_to_display(camera: &str, t: f64, ti: f64) -> (f64, f64) {
    let (dm, dt) = wb_offset(camera);
    (1.0e6 / (1.0e6 / t.max(1000.0) - dm).max(10.0), ti - dt)
}


