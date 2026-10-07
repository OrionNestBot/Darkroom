//! Develop pipeline. Preview and export share this code so their output matches.
//!
//! Stages: sampling (geometry) -> linear adjustments (WB, exposure, calibration, lens vignetting, dehaze)
//!      -> local tone (highlights/shadows, clarity, texture) -> highlight shoulder + gamma + tone LUT
//!      -> color (HSL, B&W, color grading, vibrance/saturation) -> detail (noise reduction, sharpening)
//!      -> effects (post-crop vignette, grain) -> 8-bit output, histogram, clipping display

use super::color::{self, Lut, fast_encode, luma, monotone_spline, rgb_hue, rotate_hue};
use super::filters;
use super::geometry::GeoMap;
use super::image::{LinearImage, SourceImage, sample_bilinear};
use super::mask::{BrushCache, PreparedMask, prepare, smoothstep};
use super::settings::*;
use crate::config::{ACCENT, RAW_BASELINE_EV};
use rayon::prelude::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Slider strength constants, tuned by comparing rendered output with reference renders (see tools/calib).
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub contrast: f32,
    pub whites: f32,
    pub whites_pow: f32,
    /// Whites model: 1 = highlight-weighted exposure in the local tone stage (image-adaptive), 0 = measured tone curve.
    pub wh_mode: f32,
    pub wh_lo: f32,
    pub wh_adapt: f32,
    pub whites_neg: f32,
    /// Luminance used for whites weighting: 0 = blurred base luminance, 1 = pixel luminance.
    pub wh_pix: f32,
    pub blacks: f32,
    pub blacks_pow: f32,
    /// Parametric curve: shadows, darks, lights, highlights.
    pub param: [f32; 4],
    pub highlights: f32,
    pub shadows: f32,
    pub hl_lo: f32,
    pub sh_hi: f32,
    /// Shadows/highlights response exponents (|v|^gamma) and negative-direction gain multipliers.
    pub sh_gamma: f32,
    pub hl_gamma: f32,
    pub shadows_neg: f32,
    pub highlights_pos: f32,
    /// Local tone base map: guided filter radius (fraction of long edge) and edge threshold (log2 squared).
    pub base_r: f32,
    pub base_eps: f32,
    /// Shadow adaptation: shrink the shadow range when the image key is darker than the reference.
    pub sh_adapt: f32,
    pub sh_key_ref: f32,
    /// Grading luminance gains per range (shadows, midtones, highlights, global).
    pub grade_lum_w: [f32; 4],
    /// Grading range width multipliers at blend 0/100, saturation scale with blend, and balance shift.
    pub grade_blend_lo: f32,
    pub grade_blend_hi: f32,
    pub grade_blend_sat: f32,
    pub grade_bal_k: f32,
    pub clarity: f32,
    /// Clarity gain for the fine-to-mid band (luminance minus medium blur).
    pub clarity_fine: f32,
    /// Clarity medium blur radius (fraction of long edge).
    pub clar_sigma: f32,
    /// Clarity large-radius base (fraction of long edge; 0 = edge-preserving base map).
    pub clar_large_sigma: f32,
    /// Saturation reduction for positive clarity.
    pub clarity_sat: f32,
    /// Clarity base map: guided filter radius (fraction of long edge) and edge threshold (log2 squared).
    pub clar_base_r: f32,
    pub clar_eps: f32,
    /// Highlight protection exponent when clarity brightens (higher protects only the brightest areas).
    pub clar_prot: f32,
    pub texture: f32,
    /// Negative texture/clarity multipliers (positive and negative responses differ).
    pub texture_neg: f32,
    pub clarity_neg: f32,
    /// Texture blur radius (fraction of long edge).
    pub tex_sigma: f32,
    pub dehaze: f32,
    pub dehaze_neg: f32,
    /// Dehaze local contrast (clarity band) gain: positive/negative.
    pub dehaze_loc: f32,
    pub dehaze_loc_neg: f32,
    /// Dehaze airlight color removal/addition gain: positive/negative.
    pub dehaze_air: f32,
    pub dehaze_air_neg: f32,
    /// Dehaze saturation gain: positive/negative.
    pub dehaze_sat: f32,
    pub dehaze_sat_neg: f32,
    /// Global dehaze model: 1 = atmospheric scattering (separate luminance and chroma, hue preserved), 0 = measured tone curve.
    pub dz_mode: f32,
    /// Transmission floor, dark channel exponent, chroma gain, and negative-side desaturation.
    pub dz_t0: f32,
    pub dz_gamma: f32,
    pub dz_chroma: f32,
    /// Saturation scale for the airlight color (0 = neutral airlight; limits yellow loss in warm indoor shots).
    pub dz_air_sat: f32,
    pub dz_desat: f32,
    /// Scales negative dehaze tone change by the image airlight (top luminance): (air / ref)^pow (pow 0 = fixed curve).
    pub dz_neg_ref: f32,
    pub dz_neg_pow: f32,
    /// Airlight cap (clipped highlights overestimate airlight).
    pub dz_neg_cap: f32,
    pub grade_sat: f32,
    pub grade_lum: f32,
    pub grade_s_w: f32,
    pub grade_h_w: f32,
    pub grade_m_w: f32,
    /// Color space for grading hue angles: 0 = sRGB gamma, 1 = ProPhoto linear, 2 = ProPhoto gamma 1.8.
    pub grade_space: f32,
    pub hsl_hue: f32,
    pub hsl_sat: f32,
    pub hsl_lum: f32,
    pub vibrance: f32,
    pub vib_sat_ref: f32,
    /// How much negative vibrance darkens in proportion to the removed saturation.
    pub vib_dark: f32,
    /// Negative vibrance gain (separate from positive).
    pub vib_neg: f32,
    /// Positive vibrance nonlinearity (stronger toward the slider end): v * (1 + k*v).
    pub vib_curve: f32,
    pub saturation: f32,
    /// Saturation space: 0 = display (gamma) RGB, 1 = linear light (luminance preserving).
    pub sat_linear: f32,
    pub vignette: f32,
    pub band_hues: [f32; 8],
    /// Calibration primary hue rotation (degrees per unit) and saturation scale (R, G, B).
    pub calib_hue: [f32; 3],
    pub calib_sat: [f32; 3],
}

impl Default for Tuning {
    /// Defaults from automatic tuning against reference renders across several camera bodies.
    fn default() -> Self {
        Self {
            contrast: 0.180,
            whites: 2.259,
            whites_pow: 0.522,
            // The local whites model is less accurate than the measured curve, so it is off by default. Test switch: DARKROOM_WH_LOCAL=1 enables it.
            wh_mode: if std::env::var("DARKROOM_WH_LOCAL").is_ok() { 1.0 } else { 0.0 },
            wh_lo: 0.259,
            wh_adapt: 1.002,
            whites_neg: 0.201,
            wh_pix: 0.0,
            blacks: 0.167,
            blacks_pow: 1.03,
            param: [0.104, 0.228, 0.236, 0.148],
            highlights: 0.610,
            shadows: 1.397,
            hl_lo: 0.0,
            sh_hi: 0.712,
            sh_gamma: 1.092,
            hl_gamma: 1.212,
            shadows_neg: 0.934,
            highlights_pos: 0.813,
            base_r: 0.0293,
            sh_adapt: 0.921,
            sh_key_ref: 0.522,
            base_eps: 0.343,
            grade_lum_w: [0.570, 1.046, 1.053, 1.0],
            grade_blend_lo: 1.1993,
            grade_blend_hi: 1.2972,
            grade_blend_sat: 0.9217,
            grade_bal_k: 0.4213,
            clarity: 1.029,
            clarity_fine: 1.375,
            clar_sigma: 0.00986,
            clar_large_sigma: 0.0,
            clarity_sat: 0.016,
            clar_base_r: 0.123,
            clar_eps: 1.297,
            clar_prot: 2.976,
            texture: 0.733,
            texture_neg: 0.854,
            clarity_neg: 0.354,
            tex_sigma: 0.0017,
            dehaze: 0.593,
            dehaze_neg: 0.55,
            dehaze_loc: 0.611,
            dehaze_loc_neg: 1.643,
            dehaze_air: 0.618,
            dehaze_air_neg: 1.358,
            dehaze_sat: 0.149,
            dehaze_sat_neg: 0.584,
            dz_mode: if std::env::var("DARKROOM_DZ_OLD").is_ok() { 0.0 } else { 1.0 },
            dz_t0: 0.517,
            dz_gamma: 1.029,
            dz_chroma: 0.614,
            dz_air_sat: 1.0,
            dz_desat: 0.568,
            dz_neg_ref: 0.6,
            dz_neg_pow: 0.4,
            dz_neg_cap: 1.0,
            grade_sat: 0.8991,
            grade_lum: 0.302,
            grade_s_w: 0.4735,
            grade_h_w: 0.3405,
            grade_m_w: 0.2221,
            grade_space: 1.0,
            hsl_hue: 34.77,
            hsl_sat: 0.957,
            hsl_lum: 0.270,
            vibrance: 1.170,
            vib_sat_ref: 0.510,
            vib_dark: 0.273,
            vib_neg: 2.120,
            vib_curve: 0.298,
            saturation: 1.079,
            sat_linear: 1.0,
            vignette: 1.0,
            band_hues: [0.0, 17.26, 57.10, 90.03, 157.76, 207.50, 275.73, 290.02],
            calib_hue: [0.285, 0.161, 0.225],
            calib_sat: [0.650, 0.662, 0.659],
        }
    }
}

static TUNING: std::sync::RwLock<Option<Tuning>> = std::sync::RwLock::new(None);

pub fn tuning() -> Tuning {
    TUNING.read().ok().and_then(|g| *g).unwrap_or_default()
}

#[allow(dead_code)] // used by the calibration tool (calib)
pub fn set_tuning(t: Tuning) {
    if let Ok(mut g) = TUNING.write() {
        *g = Some(t);
    }
}

pub struct RenderRequest<'a> {
    pub settings: &'a DevelopSettings,
    pub out_w: usize,
    pub out_h: usize,
    /// Normalized region of the cropped result [x0,y0,x1,y1] (zoom/pan).
    pub region: [f32; 4],
    pub draft: bool,
    pub clipping: bool,
    pub mask_overlay: Option<usize>,
    /// Keep a float result for export (display sRGB encoded, 0..1).
    pub keep_float: bool,
}

#[derive(Clone)]
pub struct Histogram {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    pub l: [u32; 256],
}

impl Default for Histogram {
    fn default() -> Self {
        Self { r: [0; 256], g: [0; 256], b: [0; 256], l: [0; 256] }
    }
}

impl Histogram {
    pub fn from_rgba(px: &[u8]) -> Self {
        let mut h = Histogram::default();
        for p in px.as_chunks::<4>().0 {
            h.r[p[0] as usize] += 1;
            h.g[p[1] as usize] += 1;
            h.b[p[2] as usize] += 1;
            let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as usize;
            h.l[l.min(255)] += 1;
        }
        h
    }

    fn merge(mut self, o: &Histogram) -> Self {
        for i in 0..256 {
            self.r[i] += o.r[i];
            self.g[i] += o.g[i];
            self.b[i] += o.b[i];
            self.l[i] += o.l[i];
        }
        self
    }
}

pub struct RenderOutput {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub float: Option<Vec<f32>>,
    pub hist: Histogram,
}

/// Low-resolution global base maps (grid in normalized source coordinates).
struct BaseMap {
    w: usize,
    h: usize,
    /// Edge-preserving smoothed log2 luminance.
    base: Vec<f32>,
    /// Large-radius edge-preserving smoothed log2 luminance for clarity.
    cbase: Vec<f32>,
    /// Dark channel for dehaze.
    dark: Vec<f32>,
    /// Airlight.
    air: f32,
    /// Airlight color (linear, mean of the top dark-channel region).
    air_rgb: [f32; 3],
    /// Image key (median perceptual brightness of the base map, 0..1).
    key: f32,
}

impl BaseMap {
    #[inline]
    fn sample(&self, buf: &[f32], u: f32, v: f32) -> f32 {
        let x = (u * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let y = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let x0 = x as usize;
        let y0 = y as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let tx = x - x0 as f32;
        let ty = y - y0 as f32;
        let a = buf[y0 * self.w + x0] + (buf[y0 * self.w + x1] - buf[y0 * self.w + x0]) * tx;
        let b = buf[y1 * self.w + x0] + (buf[y1 * self.w + x1] - buf[y1 * self.w + x0]) * tx;
        a + (b - a) * ty
    }
}

/// Parts of the embedded lens correction that always apply regardless of profile correction (chosen to match reference renders).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmbPolicy {
    pub dist: bool,
    pub ca: bool,
    pub vig: bool,
    /// Apply lens-data (lensfun or built-in table) vignetting by default; on Nikon Z, reference renders follow the camera's vignette control setting.
    pub lf_vig: bool,
    /// Strength for that vignetting (0..1).
    pub lf_vig_amount: f64,
}

impl EmbPolicy {
    pub fn any(&self) -> bool {
        self.dist || self.ca || self.vig || self.lf_vig
    }
}

/// Nikon mirrorless (Z mount) body.
fn nikon_z(li: &super::image::LensInfo) -> bool {
    li.make.to_ascii_uppercase().contains("NIKON") && li.model.to_ascii_uppercase().replace("NIKON", "").trim_start().starts_with('Z')
}

pub fn embedded_policy(li: &super::image::LensInfo) -> EmbPolicy {
    let mut p = embedded_policy_inner(li);
    if nikon_z(li) && !li.shading_comp && std::env::var("DARKROOM_EMB").is_err() {
        // Scale with the camera's vignette control level (off means no correction); assume Normal if unknown.
        let vc = li.embedded.as_ref().and_then(|e| e.vig_control).unwrap_or(3) as f64;
        p.lf_vig_amount = (crate::config::NIKON_VC_NORMAL * vc / 3.0).clamp(0.0, 1.0);
        p.lf_vig = p.lf_vig_amount > 0.0;
    }
    if li.shading_comp {
        p.vig = false;
    }
    p
}

fn embedded_policy_inner(li: &super::image::LensInfo) -> EmbPolicy {
    let Some(e) = &li.embedded else { return EmbPolicy::default() };
    let flags = |v: &str| EmbPolicy { dist: v.contains('d') && e.has_dist, ca: v.contains('c') && e.has_ca, vig: v.contains('v') && e.has_vig, lf_vig: false, lf_vig_amount: 0.0 };
    // Test switch: DARKROOM_EMB=0 disables, or any combination of d/c/v (distortion, CA, vignetting).
    if let Ok(v) = std::env::var("DARKROOM_EMB") {
        return flags(&v);
    }
    // DNG opcodes are part of the file and always apply.
    if e.maker == "DNG" {
        return flags("dcv");
    }
    // The file records that correction was on in camera (e.g. Nikon Z); apply only then.
    if let Some(a) = e.auto {
        return flags(a);
    }
    let (model, lens) = (li.model.trim(), li.lens.trim());
    crate::config::EMBEDDED_AUTO
        .iter()
        .find(|(c, l, _)| (c.is_empty() || c.eq_ignore_ascii_case(model)) && (l.is_empty() || l.eq_ignore_ascii_case(lens)))
        .map(|(_, _, f)| flags(f))
        .unwrap_or_default()
}

/// For the profile fitting tool: render returns values right after the input color transform.
static PRE_DUMP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[allow(dead_code)]
pub fn set_pre_dump(on: bool) {
    PRE_DUMP.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[inline]
fn pre_dump() -> bool {
    PRE_DUMP.load(std::sync::atomic::Ordering::Relaxed)
}

fn lens_profiles_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("DARKROOM_LENS_PROFILES").map(|v| v != "0").unwrap_or(true))
}

/// Cache state owned by the render thread.
#[derive(Default)]
pub struct Engine {
    brush: BrushCache,
    base: Option<(u64, Arc<BaseMap>)>,
    luts: Option<(u64, Arc<[Lut; 3]>)>,
    /// LUTs for user point curves (RGB and channels) applied in ProPhoto primaries; None if unused.
    user_curves: Option<Arc<[Lut; 3]>>,
    xform: Option<(u64, Arc<super::dcp::ColorXform>)>,
    lens: Option<(String, Option<Arc<super::lensmodel::LensModel>>)>,
    /// Prepared state per spot (the heal grid solve is costly, so reuse while spot and source are unchanged).
    spots: std::collections::HashMap<u64, Arc<PreparedSpot>>,
}

/// Input (source) to linear working space transform. The DNG path follows the DNG specification color model; otherwise a plain matrix.
pub enum InputXf {
    Legacy([[f32; 3]; 3]),
    Dng { xf: Arc<super::dcp::ColorXform>, calib: [[f32; 3]; 3], calib_id: bool },
}

impl InputXf {
    #[inline]
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        match self {
            InputXf::Legacy(m) => mat_mul(m, p),
            InputXf::Dng { xf, calib, calib_id } => {
                let q = xf.camera_to_working(p);
                if *calib_id { q } else { mat_mul(calib, q) }
            }
        }
    }
    pub fn dng(&self) -> Option<&super::dcp::ColorXform> {
        match self {
            InputXf::Dng { xf, .. } => Some(xf),
            _ => None,
        }
    }
}

// Local field indices
const F_TEMP: usize = 0;
const F_TINT: usize = 1;
const F_EXP: usize = 2;
const F_CONTRAST: usize = 3;
const F_HIGH: usize = 4;
const F_SHADOW: usize = 5;
const F_WHITE: usize = 6;
const F_BLACK: usize = 7;
const F_TEXTURE: usize = 8;
const F_CLARITY: usize = 9;
const F_DEHAZE: usize = 10;
const F_HUE: usize = 11;
const F_SAT: usize = 12;
const F_SHARP: usize = 13;
const F_NOISE: usize = 14;
const NF: usize = 15;

fn local_values(a: &LocalAdjust) -> [f32; NF] {
    [
        a.temp, a.tint, a.exposure, a.contrast, a.highlights, a.shadows, a.whites, a.blacks, a.texture, a.clarity, a.dehaze, a.hue,
        a.saturation, a.sharpness, a.noise,
    ]
}

/// Local adjustment field buffer; only used fields get a slot.
struct Locals {
    slot: [Option<usize>; NF],
    nf: usize,
    data: Vec<f32>,
}

impl Locals {
    #[inline]
    fn get(&self, i: usize, f: usize) -> f32 {
        match self.slot[f] {
            Some(s) => self.data[i * self.nf + s],
            None => 0.0,
        }
    }
    fn has(&self, f: usize) -> bool {
        self.slot[f].is_some()
    }
    /// Maximum value of that field (0 if absent).
    fn max(&self, f: usize) -> f32 {
        match self.slot[f] {
            Some(s) => self.data.iter().skip(s).step_by(self.nf.max(1)).fold(0.0f32, |a, v| a.max(*v)),
            None => 0.0,
        }
    }
}

fn hash_f32s(h: &mut impl Hasher, v: &[f32]) {
    for x in v {
        x.to_bits().hash(h);
    }
}

/// Global linear matrix: exposure x calibration x white balance.
fn linear_matrix(s: &DevelopSettings, is_raw: bool) -> [[f32; 3]; 3] {
    linear_matrix_ex(s, is_raw, true)
}

/// If `with_wb_exposure` is false, calibration only (the DNG path handles WB and exposure in the color transform).
fn linear_matrix_ex(s: &DevelopSettings, is_raw: bool, with_wb_exposure: bool) -> [[f32; 3]; 3] {
    let (t, ti) = if with_wb_exposure { (s.temp / 100.0, s.tint / 100.0) } else { (0.0f32, 0.0f32) };
    let mut wb = [2f32.powf(0.5 * t), 2f32.powf(-0.35 * ti), 2f32.powf(-0.5 * t)];
    let l = luma(wb[0], wb[1], wb[2]);
    for v in &mut wb {
        *v /= l;
    }
    let gain = if with_wb_exposure { 2f32.powf(s.exposure + if is_raw { RAW_BASELINE_EV } else { 0.0 }) } else { 1.0 };
    // Calibration: hue rotation and saturation per primary column.
    let c = &s.calibration;
    let mut m = [[0.0f32; 3]; 3];
    let prim = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let hs = [(c.red_hue, c.red_sat), (c.green_hue, c.green_sat), (c.blue_hue, c.blue_sat)];
    let tu = tuning();
    for (j, p) in prim.iter().enumerate() {
        let (hue, sat) = hs[j];
        let mut col = rotate_hue(*p, hue * tu.calib_hue[j]);
        let y = luma(col[0], col[1], col[2]);
        let k = 1.0 + sat / 100.0 * tu.calib_sat[j];
        for v in &mut col {
            *v = y + (*v - y) * k;
        }
        for i in 0..3 {
            m[i][j] = col[i];
        }
    }
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = gain * m[i][j] * wb[j];
        }
    }
    out
}

#[inline]
fn mat_mul(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

impl Engine {
    /// Source to working space transform (DNG transform cached while settings are unchanged).
    pub fn input_xf(&mut self, src: &SourceImage, s: &DevelopSettings) -> InputXf {
        let Some(rc) = &src.raw_color else {
            return InputXf::Legacy(linear_matrix(s, src.is_raw));
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (Arc::as_ptr(rc) as usize).hash(&mut h);
        s.profile.hash(&mut h);
        s.wb_custom.hash(&mut h);
        hash_f32s(&mut h, &[s.temp_k, s.tint_k, s.exposure, s.profile_amount]);
        let c = &s.calibration;
        let cal = [c.red_hue, c.red_sat, c.green_hue, c.green_sat, c.blue_hue, c.blue_sat];
        let tu = tuning();
        hash_f32s(&mut h, &cal);
        hash_f32s(&mut h, &tu.calib_hue);
        hash_f32s(&mut h, &tu.calib_sat);
        let key = h.finish();
        let xf = match &self.xform {
            Some((k, x)) if *k == key => x.clone(),
            _ => {
                let wb = if s.wb_custom { Some((s.temp_k as f64, s.tint_k as f64)) } else { None };
                let f = |a: [f32; 3]| [a[0] as f64, a[1] as f64, a[2] as f64];
                let cal64 = [cal[0] as f64, cal[1] as f64, cal[2] as f64, cal[3] as f64, cal[4] as f64, cal[5] as f64];
                let x = Arc::new(super::dcp::build_xform_full(rc, &s.profile, wb, s.exposure as f64, cal64, f(tu.calib_hue), f(tu.calib_sat), s.profile_amount as f64 / 100.0));
                self.xform = Some((key, x.clone()));
                x
            }
        };
        // Calibration is applied inside the color transform matrix.
        InputXf::Dng { xf, calib: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], calib_id: true }
    }

    /// Lens correction model (cached while data, shooting conditions and amount are unchanged).
    fn lens_model(&mut self, src: &SourceImage, s: &DevelopSettings) -> Option<Arc<super::lensmodel::LensModel>> {
        let li = src.lens_info.as_ref()?;
        let l = &s.lens;
        // Embedded correction parts that always apply, regardless of profile correction.
        let emb_auto = embedded_policy(li);
        if !lens_profiles_on() || (!l.profile_enable && !emb_auto.any()) {
            return None;
        }
        let key = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
            l.profile_file, li.lens, li.focal, li.fnumber, l.profile_dist, l.profile_vig, l.remove_ca, li.orientation, li.model, super::lensfun::generation(), l.profile_enable, emb_auto
        );
        if let Some((k, m)) = &self.lens
            && *k == key {
                return m.clone();
            }
        let (w, h) = (src.width() as f64, src.height() as f64);
        // Full frame of the actual image (sensor orientation) before the camera's aspect-ratio crop.
        let (cw, ch) = if li.orientation >= 5 { (h, w) } else { (w, h) };
        let (sw, sh) = match li.full_dims {
            Some((fw, fh)) => (fw as f64, fh as f64),
            None => (cw, ch),
        };
        let mk = |lf: super::lensfun::Model| {
            Arc::new(super::lensmodel::LensModel {
                sw,
                sh,
                cw,
                ch,
                orient: li.orientation,
                dist_scale: (l.profile_dist / 100.0) as f64,
                vig_scale: if li.shading_comp { 0.0 } else { (l.profile_vig / 100.0) as f64 },
                ca: l.remove_ca,
                lf: Some(lf),
                fill: 1.0,
                emb: None,
            }
            .with_fill())
        };
        // With profile correction off: only the always-on embedded parts (plus lens-data vignetting on Nikon Z).
        if !l.profile_enable {
            let lf = if emb_auto.lf_vig {
                super::lensfun::db().and_then(|db| {
                    let (lens, crop) = super::lensfun::find_for(&db, li, None)?;
                    let mut m = lens.model(li.focal as f64, li.fnumber as f64, sw, sh, crop);
                    if let Some(k) = super::lensfun::own_vig_for(&li.lens, li.focal as f64, li.fnumber as f64) {
                        m.set_vig_ff(k, lens.crop);
                    }
                    m.vig.is_some().then_some(m)
                })
            } else {
                None
            };
            let e = li.embedded.clone();
            if e.is_none() && lf.is_none() {
                self.lens = Some((key, None));
                return None;
            }
            let m = Arc::new(
                super::lensmodel::LensModel {
                    sw,
                    sh,
                    cw,
                    ch,
                    orient: li.orientation,
                    // lensfun models supply vignetting only (distortion and CA come from embedded data).
                    dist_scale: if emb_auto.dist && e.as_ref().map(|e| e.has_dist).unwrap_or(false) { 1.0 } else { 0.0 },
                    vig_scale: if lf.is_some() { emb_auto.lf_vig_amount } else if emb_auto.vig { 1.0 } else { 0.0 },
                    ca: emb_auto.ca,
                    lf: lf.map(|mut m| {
                        m.dist = None;
                        m.tca = None;
                        m
                    }),
                    fill: 1.0,
                    emb: e,
                }
                .with_fill(),
            );
            self.lens = Some((key, Some(m.clone())));
            return Some(m);
        }
        // lensfun lens picked manually ("lensfun:<name>")
        let manual_lf = l.profile_file.strip_prefix(super::lensfun::PREFIX);
        // Public lensfun data (plus the built-in vignetting table).
        let model = super::lensfun::db().and_then(|db| {
            let (lens, crop) = super::lensfun::find_for(&db, li, manual_lf)?;
            let mut m = lens.model(li.focal as f64, li.fnumber as f64, sw, sh, crop);
            // Lenses with a built-in vignetting table use it (tuned against reference renders).
            if let Some(k) = super::lensfun::own_vig_for(&li.lens, li.focal as f64, li.fnumber as f64) {
                m.set_vig_ff(k, lens.crop);
            }
            Some(mk(m))
        });
        // Without a profile, use all embedded camera corrections (distortion, vignetting, CA; scaled by the amount slider).
        let model = model.or_else(|| {
            let e = li.embedded.clone()?;
            Some(Arc::new(
                super::lensmodel::LensModel {
                    sw,
                    sh,
                    cw,
                    ch,
                    orient: li.orientation,
                    dist_scale: (l.profile_dist / 100.0) as f64,
                    vig_scale: if li.shading_comp { 0.0 } else { (l.profile_vig / 100.0) as f64 },
                    ca: true,
                    lf: None,
                    fill: 1.0,
                    emb: Some(e),
                }
                .with_fill(),
            ))
        });
        self.lens = Some((key, model.clone()));
        model
    }

    fn base_map(&mut self, src: &SourceImage, s: &DevelopSettings, xf: &InputXf) -> Arc<BaseMap> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (Arc::as_ptr(&src.levels[0]) as usize).hash(&mut h);
        hash_f32s(&mut h, &[s.exposure, s.temp, s.tint, s.temp_k, s.tint_k]);
        s.wb_custom.hash(&mut h);
        s.profile.hash(&mut h);
        let tq = tuning();
        hash_f32s(&mut h, &[tq.clar_base_r, tq.clar_eps, tq.base_r, tq.base_eps]);
        hash_f32s(
            &mut h,
            &[s.calibration.red_hue, s.calibration.red_sat, s.calibration.green_hue, s.calibration.green_sat, s.calibration.blue_hue, s.calibration.blue_sat],
        );
        // Include spots in the base map so removed objects don't come back as ghosts through local tone/clarity.
        if !s.spots.is_empty() {
            serde_json::to_string(&s.spots).unwrap_or_default().hash(&mut h);
        }
        let key = h.finish();
        if let Some((k, b)) = &self.base
            && *k == key {
                return b.clone();
            }
        let healed: Option<LinearImage> = if s.spots.is_empty() {
            None
        } else {
            let lvl = src.levels.last().unwrap().clone();
            let (bw, bh) = (src.width() as f32, src.height() as f32);
            let spots = self.prepare_spots_cached(&s.spots, src, bw, bh);
            let k = lvl.w as f32 / bw;
            let mut out = (*lvl).clone();
            out.data.par_chunks_mut(lvl.w * 3).enumerate().for_each(|(y, row)| {
                for x in 0..lvl.w {
                    let (bx, by) = ((x as f32 + 0.5) / k, (y as f32 + 0.5) / k);
                    for sp in &spots {
                        let Some((w, sx, sy, gain)) = sp.eval(bx, by) else { continue };
                        let px = &mut row[x * 3..x * 3 + 3];
                        if let Some(f) = sp.fill_at(bx, by) {
                            for c in 0..3 {
                                px[c] = px[c] * (1.0 - w) + f[c] * w;
                            }
                            continue;
                        }
                        let q = sample_bilinear(&lvl, (sx * k).clamp(0.0, lvl.w as f32 - 1.001), (sy * k).clamp(0.0, lvl.h as f32 - 1.001));
                        for c in 0..3 {
                            px[c] = px[c] * (1.0 - w) + q[c] * gain[c] * w;
                        }
                    }
                }
            });
            Some(out)
        };
        let img: &LinearImage = healed.as_ref().unwrap_or_else(|| src.levels.last().unwrap());
        let n = img.w * img.h;
        let mut l = vec![0.0f32; n];
        let mut dark = vec![0.0f32; n];
        l.par_iter_mut().zip(dark.par_iter_mut()).enumerate().for_each(|(i, (lv, dv))| {
            let p = xf.apply([img.data[i * 3], img.data[i * 3 + 1], img.data[i * 3 + 2]]);
            *lv = luma(p[0], p[1], p[2]).max(1e-5).log2();
            *dv = p[0].min(p[1]).min(p[2]).max(0.0);
        });
        let long = img.w.max(img.h);
        let r = ((long as f32 * tq.base_r) as usize).max(2);
        let base = filters::guided_self(&l, img.w, img.h, r, tq.base_eps, 2);
        let cr = ((long as f32 * tq.clar_base_r) as usize).max(2);
        let cbase = filters::guided_self(&l, img.w, img.h, cr, tq.clar_eps, 2);
        // Blur the dark channel edge-preserving (a Gaussian blur leaves bright halos around edges).
        let dark = if std::env::var_os("DARKROOM_DZ_GAUSS").is_some() {
            filters::gauss_blur(&dark, img.w, img.h, long as f32 * 0.008)
        } else {
            filters::guided_self(&dark, img.w, img.h, ((long as f32 * 0.012) as usize).max(2), 0.002, 1)
        };
        // Airlight: top 0.5% luminance.
        let mut lin: Vec<f32> = l.iter().step_by(7).map(|v| v.exp2()).collect();
        let k = ((lin.len() as f32) * 0.995) as usize;
        let k = k.min(lin.len().saturating_sub(1));
        let air = if lin.is_empty() {
            1.0
        } else {
            *lin.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
        };
        // Airlight color: mean color of the top 0.2% dark-channel pixels.
        let air_rgb = {
            let mut idx: Vec<(f32, usize)> = (0..n).step_by(3).map(|i| (dark[i], i)).collect();
            let k = (idx.len() / 500).max(1);
            let pos = idx.len().saturating_sub(k);
            if pos < idx.len() {
                idx.select_nth_unstable_by(pos, |a, b| a.0.total_cmp(&b.0));
            }
            let mut acc = [0.0f64; 3];
            for (_, i) in &idx[pos..] {
                let p = xf.apply([img.data[i * 3], img.data[i * 3 + 1], img.data[i * 3 + 2]]);
                for c in 0..3 {
                    acc[c] += p[c].max(0.0) as f64;
                }
            }
            let m = (idx.len() - pos).max(1) as f64;
            [(acc[0] / m) as f32, (acc[1] / m) as f32, (acc[2] / m) as f32]
        };
        let img_key = {
            let mut v: Vec<f32> = base.iter().step_by(5).map(|b| fast_encode(b.exp2().min(4.0) * 0.9).min(1.0)).collect();
            if v.is_empty() {
                0.5
            } else {
                let m = v.len() / 2;
                *v.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1
            }
        };
        let bm = Arc::new(BaseMap { w: img.w, h: img.h, base, cbase, dark, air: air.max(0.05), air_rgb, key: img_key });
        self.base = Some((key, bm.clone()));
        bm
    }

    /// Master and per-channel tone LUTs (gamma space).
    #[cfg(all(test, feature = "calib"))]
    pub fn tone_luts_pub(&mut self, s: &DevelopSettings) -> Arc<[Lut; 3]> {
        self.tone_luts(s, false, 1.0)
    }

    fn tone_luts(&mut self, s: &DevelopSettings, is_raw: bool, dz_scale: f32) -> Arc<[Lut; 3]> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        is_raw.hash(&mut h);
        let tq = tuning();
        hash_f32s(&mut h, &[s.contrast, s.whites, s.blacks, s.dehaze, dz_scale, tq.contrast, tq.whites, tq.whites_pow, tq.wh_mode, tq.dz_mode, tq.blacks, tq.blacks_pow]);
        hash_f32s(&mut h, &tq.param);
        let c = &s.curve;
        hash_f32s(&mut h, &[c.highlights, c.lights, c.darks, c.shadows]);
        hash_f32s(&mut h, &c.splits);
        for pts in [&c.rgb, &c.red, &c.green, &c.blue] {
            hash_f32s(&mut h, &pts.iter().flatten().copied().collect::<Vec<_>>());
        }
        let key = h.finish();
        if let Some((k, l)) = &self.luts
            && *k == key {
                return l.clone();
            }
        let tu = tuning();
        let contrast = s.contrast / 100.0;
        let (s_contrast, s_whites, s_blacks, s_dehaze) = (s.contrast, s.whites, s.blacks, s.dehaze);
        let wh_local = tu.wh_mode > 0.5;
        let rgb = monotone_spline(&c.rgb);
        let sp = c.splits;
        let (ph, pl, pd, ps) = (c.highlights / 100.0, c.lights / 100.0, c.darks / 100.0, c.shadows / 100.0);
        let measured = Lut::from_fn(|x| {
            let mut y = x;
            // Default RAW tone curve (gentle S).
            if is_raw {
                let sx = y * y * (3.0 - 2.0 * y);
                y += 0.22 * (sx - y);
            }
            // Contrast, whites, blacks, parametric: measured response curves (interpolated between measured points).
            use super::measured_curves as mc;
            let _ = (&tu, contrast);
            y += measured_delta(&[(-100.0, &mc::CONTRAST01_M100), (-50.0, &mc::CONTRAST01_M50), (50.0, &mc::CONTRAST01_P50), (100.0, &mc::CONTRAST01_P100)], s_contrast, y);
            if !wh_local {
                if std::env::var_os("DARKROOM_WH_V1").is_some() {
                    y += measured_delta(&[(-100.0, &mc::WHITES01_M100), (-50.0, &mc::WHITES01_M50), (50.0, &mc::WHITES01_P50), (100.0, &mc::WHITES01_P100)], s_whites, y);
                } else {
                    y += measured_delta(&[(-100.0, &mc::WHITES_C_M100), (-50.0, &mc::WHITES_C_M50), (30.0, &mc::WHITES_C_P30), (64.0, &mc::WHITES_C_P64), (100.0, &mc::WHITES_C_P100)], s_whites, y);
                }
            }
            // Negative (adding haze) uses the measured curve, which matches reference renders better.
            if tu.dz_mode < 0.5 || s_dehaze < 0.0 {
                let k = if s_dehaze < 0.0 { dz_scale } else { 1.0 };
                y += k * measured_delta(&[(-100.0, &mc::DEHAZE_M100), (-50.0, &mc::DEHAZE_M50), (50.0, &mc::DEHAZE_P50), (100.0, &mc::DEHAZE_P100)], s_dehaze, y);
            }
            y += measured_delta(&[(-100.0, &mc::BLACKS01_M100), (-50.0, &mc::BLACKS01_M50), (50.0, &mc::BLACKS01_P50), (100.0, &mc::BLACKS01_P100)], s_blacks, y);
            // Parametric: measured at default splits (25/50/75), so map current splits to default positions before evaluating.
            let warp = |y: f32| -> f32 {
                let xs = [0.0, sp[0], sp[1], sp[2], 1.0];
                let ys = [0.0, 0.25, 0.5, 0.75, 1.0];
                for k in 0..4 {
                    if y <= xs[k + 1] {
                        let t = (y - xs[k]) / (xs[k + 1] - xs[k]).max(1e-6);
                        return ys[k] + t * (ys[k + 1] - ys[k]);
                    }
                }
                1.0
            };
            let yw = warp(y);
            let mut dsum = 0.0;
            dsum += measured_delta(&[(-100.0, &mc::PARAMETRICSHADOWS_M100), (100.0, &mc::PARAMETRICSHADOWS_P100)], ps * 100.0, yw);
            dsum += measured_delta(&[(-100.0, &mc::PARAMETRICDARKS_M100), (100.0, &mc::PARAMETRICDARKS_P100)], pd * 100.0, yw);
            dsum += measured_delta(&[(-100.0, &mc::PARAMETRICLIGHTS_M100), (100.0, &mc::PARAMETRICLIGHTS_P100)], pl * 100.0, yw);
            dsum += measured_delta(&[(-100.0, &mc::PARAMETRICHIGHLIGHTS_M100), (100.0, &mc::PARAMETRICHIGHLIGHTS_P100)], ph * 100.0, yw);
            y += dsum;
            y.clamp(0.0, 1.0)
        });
        // Isotonic regression (PAVA) to the nearest monotone curve, so noise in measured curves can't cause tone inversions or color blotches.
        let iso = isotonic(&measured.t);
        let measured = Lut { t: iso };
        // User point curves are applied per channel in ProPhoto primaries (sRGB gamma); the same curve shifts color more in wider primaries.
        // Test switch: DARKROOM_CURVE_SPACE=srgb applies them in sRGB instead.
        let pp_curves = curve_space_pp() && !(is_identity_curve(&c.rgb) && is_identity_curve(&c.red) && is_identity_curve(&c.green) && is_identity_curve(&c.blue));
        let master = Lut::from_fn(|x| {
            let y = measured.eval(x);
            if pp_curves { y } else { rgb(y).clamp(0.0, 1.0) }
        });
        self.user_curves = if pp_curves {
            let ch = |pts: &Vec<[f32; 2]>| {
                let f = monotone_spline(pts);
                let id = is_identity_curve(pts);
                Lut::from_fn(|x| {
                    let y = rgb(x).clamp(0.0, 1.0);
                    if id { y } else { f(y).clamp(0.0, 1.0) }
                })
            };
            Some(Arc::new([ch(&c.red), ch(&c.green), ch(&c.blue)]))
        } else {
            None
        };
        let mk = |pts: &Vec<[f32; 2]>| {
            if pp_curves || is_identity_curve(pts) {
                master.clone()
            } else {
                let f = monotone_spline(pts);
                Lut::from_fn(|x| f(master.eval(x)).clamp(0.0, 1.0))
            }
        };
        let luts = Arc::new([mk(&c.red), mk(&c.green), mk(&c.blue)]);
        self.luts = Some((key, luts.clone()));
        luts
    }

    pub fn render(&mut self, src: &SourceImage, req: &RenderRequest) -> RenderOutput {
        let s = req.settings;
        let tu = tuning();
        let w = req.out_w.max(1);
        let h = req.out_h.max(1);
        let n = w * h;
        let bw = src.width();
        let bh = src.height();
        let (bwf, bhf) = (bw as f32, bh as f32);
        let long_base = bwf.max(bhf);
        let lens_model = self.lens_model(src, s);
        let geo = GeoMap::new(bw, bh, &s.geometry, &s.lens, req.region, w, h).with_lens(lens_model.clone());
        let (lvl, img) = src.level_for(geo.out_per_base);
        let lscale = 1.0 / (1u32 << lvl) as f32;
        let opb = geo.out_per_base;

        let mut prof = Prof::new();
        // Spot removal prep: healing precomputes surrounding ring mean color ratios at the smallest source level.
        let spots = self.prepare_spots_cached(&s.spots, src, bwf, bhf);
        // -- 1. Sampling + source coordinates --
        let mut rgb = vec![0.0f32; n * 3];
        let mut uv = vec![[0.0f32; 2]; n];
        rgb.par_chunks_mut(w * 3).zip(uv.par_chunks_mut(w)).enumerate().for_each(|(y, (row, uvr))| {
            for x in 0..w {
                let (bx, by) = geo.map(x as f32 + 0.5, y as f32 + 0.5);
                uvr[x] = [bx, by];
                if geo.in_bounds(bx, by) {
                    let mut p = sample_bilinear(img, bx * lscale, by * lscale);
                    // Lateral CA: read red and blue at profile-corrected positions.
                    if let Some(((rx, ry), (qx, qy))) = lens_model.as_ref().and_then(|lm| lm.ca_offsets(bx, by)) {
                        p[0] = sample_bilinear(img, rx * lscale, ry * lscale)[0];
                        p[2] = sample_bilinear(img, qx * lscale, qy * lscale)[2];
                    }
                    // Spot removal: inside the target circle, blend in pixels from the source position.
                    for sp in &spots {
                        let Some((w, sx, sy, gain)) = sp.eval(bx, by) else { continue };
                        if let Some(f) = sp.fill_at(bx, by) {
                            for c in 0..3 {
                                p[c] = p[c] * (1.0 - w) + f[c] * w;
                            }
                            continue;
                        }
                        let q = sample_bilinear(img, sx * lscale, sy * lscale);
                        for c in 0..3 {
                            p[c] = p[c] * (1.0 - w) + q[c] * gain[c] * w;
                        }
                    }
                    row[x * 3..x * 3 + 3].copy_from_slice(&p);
                } else {
                    row[x * 3..x * 3 + 3].copy_from_slice(&[f32::NAN; 3]);
                }
            }
        });

        prof.lap("2. 마스크");

        // -- 2. Masks -> local fields --
        let active: Vec<(usize, PreparedMask, [f32; NF])> = s
            .masks
            .iter()
            .enumerate()
            .filter(|(i, m)| m.visible && !m.components.is_empty() && (!m.adj.is_zero() || req.mask_overlay == Some(*i)))
            .map(|(i, m)| (i, prepare(m, bw, bh, &mut self.brush), local_values(&m.adj)))
            .collect();
        let mut slot = [None; NF];
        let mut nf = 0;
        for f in 0..NF {
            if active.iter().any(|(_, _, v)| v[f] != 0.0) {
                slot[f] = Some(nf);
                nf += 1;
            }
        }
        let mut overlay: Option<Vec<f32>> = None;
        let locals = if nf > 0 || req.mask_overlay.is_some() {
            let mut data = vec![0.0f32; n * nf.max(1)];
            let ov_idx = req.mask_overlay;
            // Without an overlay, use a one-element dummy buffer per row to keep zip lengths equal.
            let ov_chunk = if ov_idx.is_some() { w } else { 1 };
            let mut ov = vec![0.0f32; ov_chunk * h];
            let need_color = active.iter().any(|(_, p, _)| p.needs_color());
            let enc = |v: f32| fast_encode(v);
            let nfs = nf.max(1);
            data.par_chunks_mut(w * nfs)
                .zip(ov.par_chunks_mut(ov_chunk))
                .enumerate()
                .for_each(|(y, (drow, orow))| {
                    for x in 0..w {
                        let i = y * w + x;
                        let [bx, by] = uv[i];
                        let (lum, hue, chroma) = if need_color && !rgb[i * 3].is_nan() {
                            let (r, g, b) = (enc(rgb[i * 3]), enc(rgb[i * 3 + 1]), enc(rgb[i * 3 + 2]));
                            let (hh, c) = rgb_hue(r, g, b);
                            (luma(r, g, b), hh, c)
                        } else {
                            (0.0, 0.0, 0.0)
                        };
                        for (mi, pm, vals) in &active {
                            let wgt = pm.eval(bx, by, bwf, bhf, lum, hue, chroma);
                            if ov_idx == Some(*mi) {
                                orow[x] = wgt;
                            }
                            if wgt <= 0.0 {
                                continue;
                            }
                            for f in 0..NF {
                                if let Some(sl) = slot[f] {
                                    drow[x * nfs + sl] += wgt * vals[f];
                                }
                            }
                        }
                    }
                });
            if ov_idx.is_some() {
                overlay = Some(ov);
            }
            Locals { slot, nf: nfs, data }
        } else {
            Locals { slot, nf: 1, data: Vec::new() }
        };
        let has_locals = nf > 0;

        prof.lap("3. 선형");

        // -- 3. Linear adjustments --
        let xf = self.input_xf(src, s);
        let lens_v = s.lens.vignette / 100.0;
        let lens_mid = s.lens.vignette_midpoint / 100.0;
        // Global dehaze is handled by the measured curve (tone LUT); only mask dehaze here.
        let dehaze_g = 0.0f32;
        let needs_base = dehaze_g != 0.0
            || s.highlights != 0.0
            || s.shadows != 0.0
            || (s.whites != 0.0 && tu.wh_mode > 0.5)
            || s.clarity != 0.0
            || (s.dehaze > 0.0 && tu.dz_mode > 0.5)
            || (s.dehaze != 0.0 && (tu.dehaze_loc != 0.0 || tu.dehaze_loc_neg != 0.0 || tu.dehaze_air != 0.0 || tu.dehaze_air_neg != 0.0))
            || locals.has(F_DEHAZE)
            || locals.has(F_HIGH)
            || locals.has(F_SHADOW)
            || locals.has(F_CLARITY);
        let base = if needs_base { Some(self.base_map(src, s, &xf)) } else { None };
        let hd = 0.5 * (bwf * bwf + bhf * bhf).sqrt();
        rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let i = y * w + x;
                let p = &mut row[x * 3..x * 3 + 3];
                if p[0].is_nan() {
                    continue;
                }
                let mut q = xf.apply([p[0], p[1], p[2]]);
                if pre_dump() {
                    p.copy_from_slice(&q);
                    continue;
                }
                let [bx, by] = uv[i];
                if let Some(lm) = &lens_model {
                    let g = lm.vignette_gain(bx, by);
                    if g != 1.0 {
                        q = [q[0] * g, q[1] * g, q[2] * g];
                    }
                }
                if lens_v != 0.0 {
                    let dx = (bx - bwf * 0.5) / hd;
                    let dy = (by - bhf * 0.5) / hd;
                    let r2 = dx * dx + dy * dy;
                    let t = smoothstep(lens_mid * 0.6, 1.0, r2.sqrt());
                    let g = 2f32.powf(lens_v * 1.6 * t);
                    q = [q[0] * g, q[1] * g, q[2] * g];
                }
                if has_locals {
                    let lt = locals.get(i, F_TEMP) / 100.0;
                    let lti = locals.get(i, F_TINT) / 100.0;
                    let le = locals.get(i, F_EXP);
                    if lt != 0.0 || lti != 0.0 || le != 0.0 {
                        let mut wb = [2f32.powf(0.5 * lt), 2f32.powf(-0.35 * lti), 2f32.powf(-0.5 * lt)];
                        let l = luma(wb[0], wb[1], wb[2]);
                        let g = 2f32.powf(le) / l;
                        for v in &mut wb {
                            *v *= g;
                        }
                        q = [q[0] * wb[0], q[1] * wb[1], q[2] * wb[2]];
                    }
                }
                if let Some(b) = &base {
                    // Airlight color component of global dehaze (tone is handled by the measured curve).
                    let dg = s.dehaze / 100.0;
                    if dg != 0.0 {
                        let ay = luma(b.air_rgb[0], b.air_rgb[1], b.air_rgb[2]).max(1e-4);
                        let air_c = |c: usize| ay + (b.air_rgb[c] - ay) * tu.dz_air_sat;
                        let dk = b.sample(&b.dark, bx / bwf, by / bhf);
                        let k = if dg > 0.0 { dg * tu.dehaze_air * (dk / b.air).min(1.0) } else { dg * tu.dehaze_air_neg };
                        if k != 0.0 {
                            let ql = luma(q[0], q[1], q[2]);
                            for c in 0..3 {
                                // Subtract (+) or add (-) airlight chromaticity (relative to gray) in proportion to current brightness.
                                q[c] = (q[c] - k * (air_c(c) / ay - 1.0) * ql).max(0.0);
                            }
                        }
                    }
                    // Global dehaze (atmospheric scattering model): luminance J = (I - A)/t + A; color only gains saturation with transmission.
                    // Not clipped per channel, so no color blotches or burnt areas.
                    if dg > 0.0 && tu.dz_mode > 0.5 {
                        let y0 = luma(q[0], q[1], q[2]).max(1e-6);
                        let a = b.air;
                        if dg > 0.0 {
                            let dk = b.sample(&b.dark, bx / bwf, by / bhf);
                            let hz = (dk / a).clamp(0.0, 1.0).powf(tu.dz_gamma.max(0.1));
                            let t = (1.0 - tu.dehaze * dg.min(1.0) * hz).max(tu.dz_t0.clamp(0.05, 0.95));
                            // Roll off smoothly near 0 in the darks (avoids abrupt black clumps).
                            let lin = (y0 - a) / t + a;
                            let floor = y0 * 0.08;
                            let y1 = if lin > floor { lin } else { floor * ((lin - floor) / floor).max(-12.0).exp() };
                            // Chroma (deviation from luminance) grows with 1/t as in the scattering model, so lowering brightness doesn't reduce color.
                            // Low-saturation images gain more; the exponent controls strength.
                            let cs = (1.0 / t).powf(tu.dz_chroma);
                            for v in &mut q {
                                *v = y1 + (*v - y0) * cs;
                            }
                        } else {
                            let k = (-dg).min(1.0) * tu.dehaze_neg;
                            let y1 = y0 * (1.0 - k) + a * 0.9 * k;
                            let g = y1 / y0;
                            let cs = (1.0 - k * tu.dz_desat).max(0.0);
                            for v in &mut q {
                                *v = y1 + (*v * g - y1) * cs;
                            }
                        }
                        // Out of gamut: reduce saturation while keeping luminance.
                        let mn = q[0].min(q[1]).min(q[2]);
                        if mn < 0.0 {
                            let y = luma(q[0], q[1], q[2]).max(0.0);
                            let tt = if y > 0.0 { y / (y - mn) } else { 0.0 };
                            for v in &mut q {
                                *v = (y + (*v - y) * tt).max(0.0);
                            }
                        }
                    }
                    let dz = dehaze_g + if has_locals { locals.get(i, F_DEHAZE) / 100.0 } else { 0.0 };
                    if dz > 0.0 {
                        let dk = b.sample(&b.dark, bx / bwf, by / bhf);
                        let a = b.air;
                        let t = (1.0 - tu.dehaze * dz.min(1.0) * (dk / a).min(1.0)).max(0.12);
                        for v in &mut q {
                            *v = ((*v - a) / t + a).max(0.0);
                        }
                    } else if dz < 0.0 {
                        let k = (-dz).min(1.0) * tu.dehaze_neg;
                        let a = b.air * 0.9;
                        for v in &mut q {
                            *v = *v * (1.0 - k) + a * k;
                        }
                    }
                }
                p.copy_from_slice(&q);
            }
        });
        if pre_dump() {
            // For the profile fitting tool: return linear ProPhoto values right after the input transform (before tone).
            return RenderOutput { w, h, rgba: Vec::new(), float: Some(rgb), hist: Histogram::default() };
        }

        prof.lap("4. 지역 톤");

        // -- 4. Local tone (highlights/shadows/clarity/texture/local contrast, whites, blacks) --
        let hl = s.highlights / 100.0;
        let sh = s.shadows / 100.0;
        let cl = s.clarity / 100.0;
        let tx = s.texture / 100.0;
        // Dehaze local contrast component (added to the clarity band).
        let dz_loc = {
            let d = s.dehaze / 100.0;
            if d > 0.0 { d * tu.dehaze_loc } else { d * tu.dehaze_loc_neg }
        };
        let cl = cl + dz_loc / tu.clarity.max(1e-3);
        let any_local_tone = [F_HIGH, F_SHADOW, F_CLARITY, F_TEXTURE, F_CONTRAST, F_WHITE, F_BLACK].iter().any(|f| locals.has(*f));
        let wt = if tu.wh_mode > 0.5 { s.whites / 100.0 } else { 0.0 };
        if hl != 0.0 || sh != 0.0 || cl != 0.0 || tx != 0.0 || wt != 0.0 || any_local_tone {
            // (cl includes the dehaze local contrast component)
            let lbuf: Vec<f32> = rgb
                .par_chunks(3)
                .map(|p| if p[0].is_nan() { -8.0 } else { luma(p[0], p[1], p[2]).max(1e-5).log2() })
                .collect();
            let need_clar = cl != 0.0 || locals.has(F_CLARITY);
            let need_tex = tx != 0.0 || locals.has(F_TEXTURE);
            let mid = if need_clar {
                Some(filters::gauss_blur(&lbuf, w, h, (long_base * tu.clar_sigma * opb).max(0.8)))
            } else {
                None
            };
            let fine = if need_tex {
                Some(filters::gauss_blur(&lbuf, w, h, (long_base * tu.tex_sigma * opb).max(0.7)))
            } else {
                None
            };
            let large = if need_clar && tu.clar_large_sigma > 0.0 {
                Some(filters::gauss_blur(&lbuf, w, h, (long_base * tu.clar_large_sigma * opb).max(1.0)))
            } else {
                None
            };
            let base_ref = base.clone();
            rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
                for x in 0..w {
                    let i = y * w + x;
                    let p = &mut row[x * 3..x * 3 + 3];
                    if p[0].is_nan() {
                        continue;
                    }
                    let [bx, by] = uv[i];
                    let l = lbuf[i];
                    let le = if has_locals { locals.get(i, F_EXP) } else { 0.0 };
                    let b = match &base_ref {
                        Some(bm) => bm.sample(&bm.base, bx / bwf, by / bhf) + le,
                        None => l,
                    };
                    // Perceptual position of the base luminance (0..1).
                    let pb = fast_encode(b.exp2().min(4.0) * 0.9).min(1.0);
                    let (mut h_amt, mut s_amt, mut c_amt, mut t_amt) = (hl, sh, cl, tx);
                    let (mut lc, mut lw, mut lb) = (0.0, 0.0, 0.0);
                    if has_locals {
                        h_amt += locals.get(i, F_HIGH) / 100.0;
                        s_amt += locals.get(i, F_SHADOW) / 100.0;
                        c_amt += locals.get(i, F_CLARITY) / 100.0;
                        t_amt += locals.get(i, F_TEXTURE) / 100.0;
                        lc = locals.get(i, F_CONTRAST) / 100.0;
                        lw = locals.get(i, F_WHITE) / 100.0;
                        lb = locals.get(i, F_BLACK) / 100.0;
                    }
                    if c_amt < 0.0 {
                        c_amt *= tu.clarity_neg;
                    }
                    if t_amt < 0.0 {
                        t_amt *= tu.texture_neg;
                    }
                    let wh = smoothstep(tu.hl_lo, 1.0, pb);
                    let key = base_ref.as_ref().map(|bm| bm.key).unwrap_or(0.6);
                    let pbs = (pb + tu.sh_adapt * (tu.sh_key_ref - key)).clamp(0.0, 1.0);
                    let ws = 1.0 - smoothstep(0.0, tu.sh_hi, pbs);
                    // Response curve: sign-preserving power plus per-direction gain.
                    let s_eff = s_amt.signum() * s_amt.abs().powf(tu.sh_gamma) * if s_amt < 0.0 { tu.shadows_neg } else { 1.0 };
                    let h_eff = h_amt.signum() * h_amt.abs().powf(tu.hl_gamma) * if h_amt > 0.0 { tu.highlights_pos } else { 1.0 };
                    let mut ev = h_eff * tu.highlights * wh + s_eff * tu.shadows * ws;
                    // Whites: highlight-weighted exposure, stronger for darker images (image-adaptive response).
                    if wt != 0.0 {
                        let pl = fast_encode(l.exp2().min(4.0) * 0.9).min(1.0);
                        let pw = (pb + (pl - pb) * tu.wh_pix + tu.wh_adapt * (tu.sh_key_ref - key)).clamp(0.0, 1.0);
                        let wgt = smoothstep(tu.wh_lo, 1.0, pw).powf(tu.whites_pow.max(0.1));
                        let w_eff = if wt < 0.0 { wt * tu.whites_neg } else { wt };
                        ev += w_eff * tu.whites * wgt;
                    }
                    if let Some(md) = &mid {
                        let mt = 4.0 * pb * (1.0 - pb);
                        let anchor = match (&large, &base_ref) {
                            (Some(lg), _) => lg[i] + le,
                            (None, Some(bm)) => bm.sample(&bm.cbase, bx / bwf, by / bhf) + le,
                            (None, None) => b,
                        };
                        // Large-radius local contrast (main clarity component); the brightening side protects highlights.
                        let mut coarse = c_amt * tu.clarity * (md[i] - anchor).clamp(-3.0, 3.0);
                        if coarse > 0.0 {
                            coarse *= 1.0 - pb.clamp(0.0, 1.0).powf(tu.clar_prot.max(0.1));
                        }
                        ev += coarse + c_amt * tu.clarity_fine * (l - md[i]).clamp(-3.0, 3.0) * (0.35 + 0.65 * mt);
                    }
                    if let Some(fd) = &fine {
                        ev += t_amt * tu.texture * (l - fd[i]).clamp(-2.0, 2.0);
                    }
                    if lc != 0.0 {
                        ev += lc * 0.45 * (l - (-2.47)).clamp(-4.0, 4.0);
                    }
                    if lw != 0.0 {
                        ev += lw * 0.8 * smoothstep(0.55, 1.0, fast_encode(l.exp2().min(4.0)));
                    }
                    if lb != 0.0 {
                        ev += lb * 1.2 * (1.0 - smoothstep(0.0, 0.3, fast_encode(l.exp2().min(4.0))));
                    }
                    if ev != 0.0 {
                        let g = ev.clamp(-5.0, 5.0).exp2();
                        p[0] *= g;
                        p[1] *= g;
                        p[2] *= g;
                    }
                }
            });
        }

        prof.lap("5. 숄더");

        // -- 5. Shoulder + gamma + tone LUT --
        // Only the legacy RAW path (no color profile data) uses the built-in default curve.
        let legacy_raw = src.is_raw && src.raw_color.is_none();
        // Negative dehaze (adding haze) lifts by the image airlight brightness, so scale the measured curve change by airlight.
        let dz_scale = match &base {
            Some(b) if s.dehaze < 0.0 && tu.dz_neg_pow > 0.0 => (b.air.min(tu.dz_neg_cap) / tu.dz_neg_ref.max(1e-3)).powf(tu.dz_neg_pow).clamp(0.3, 2.0),
            _ => 1.0,
        };
        let luts = self.tone_luts(s, legacy_raw, dz_scale);
        let user_curves = self.user_curves.clone();
        let s2pp = super::dcp::srgb_to_prophoto();
        let s2pp = [
            [s2pp[0][0] as f32, s2pp[0][1] as f32, s2pp[0][2] as f32],
            [s2pp[1][0] as f32, s2pp[1][1] as f32, s2pp[1][2] as f32],
            [s2pp[2][0] as f32, s2pp[2][1] as f32, s2pp[2][2] as f32],
        ];
        let dng = xf.dng();
        let pp2s = super::dcp::prophoto_to_srgb();
        let pp2s = [
            [pp2s[0][0] as f32, pp2s[0][1] as f32, pp2s[0][2] as f32],
            [pp2s[1][0] as f32, pp2s[1][1] as f32, pp2s[1][2] as f32],
            [pp2s[2][0] as f32, pp2s[2][1] as f32, pp2s[2][2] as f32],
        ];
        let knee = if src.display_referred { 1.0f32 } else { 0.8 };
        let shoulder = move |x: f32| -> f32 {
            if x <= knee {
                x.max(0.0)
            } else if knee >= 1.0 {
                1.0
            } else {
                knee + (1.0 - knee) * (1.0 - (-(x - knee) / (1.0 - knee)).exp())
            }
        };
        rgb.par_chunks_mut(3).for_each(|p| {
            if p[0].is_nan() {
                return;
            }
            if let Some(ax) = dng {
                // Look table + default tone curve (ProPhoto linear) -> linear sRGB -> gamma -> user curve
                let t = ax.finish([p[0], p[1], p[2]]);
                let q = mat_mul(&pp2s, t);
                for c in 0..3 {
                    p[c] = luts[c].eval(fast_encode(q[c].clamp(0.0, 1.0)));
                }
            } else {
                for c in 0..3 {
                    p[c] = luts[c].eval(fast_encode(shoulder(p[c])));
                }
            }
            if let Some(u) = &user_curves {
                // sRGB gamma -> linear -> ProPhoto linear -> gamma -> user curve -> back
                let lin = [color::fast_decode(p[0]), color::fast_decode(p[1]), color::fast_decode(p[2])];
                let pp = mat_mul(&s2pp, lin);
                let g = [u[0].eval(fast_encode(pp[0].clamp(0.0, 1.0))), u[1].eval(fast_encode(pp[1].clamp(0.0, 1.0))), u[2].eval(fast_encode(pp[2].clamp(0.0, 1.0)))];
                let back = mat_mul(&pp2s, [color::fast_decode(g[0]), color::fast_decode(g[1]), color::fast_decode(g[2])]);
                for c in 0..3 {
                    p[c] = fast_encode(back[c].clamp(0.0, 1.0));
                }
            }
        });

        prof.lap("6. 컬러");

        // -- 6. Color --
        let hsl_active = s.hsl.iter().any(|b| b.hue != 0.0 || b.sat != 0.0 || b.lum != 0.0);
        let bw = s.treatment == Treatment::BlackWhite;
        let grading = !s.grading.is_neutral();
        let vib = s.vibrance / 100.0;
        let sat = s.saturation / 100.0;
        let dz_sat = {
            let d = s.dehaze / 100.0;
            let c = (s.clarity / 100.0).max(0.0);
            (if d > 0.0 { d * tu.dehaze_sat } else { d * tu.dehaze_sat_neg }) - c * tu.clarity_sat
        };
        let sh_tint = s.calibration.shadows_tint / 100.0;
        let pcs: Vec<super::settings::PointColor> = s.point_colors.iter().filter(|p| !p.is_noop()).copied().collect();
        let color_needed =
            !pcs.is_empty() || hsl_active || bw || grading || vib != 0.0 || sat != 0.0 || dz_sat != 0.0 || sh_tint != 0.0 || locals.has(F_HUE) || locals.has(F_SAT);
        if color_needed {
            let g = &s.grading;
            let c_mid = 0.5 - tu.grade_bal_k * g.balance / 100.0;
            let bt = g.blending / 100.0;
            let bl_w = (tu.grade_blend_lo + (tu.grade_blend_hi - tu.grade_blend_lo) * bt) * 0.38;
            let blend_sat = 1.0 + (bt - 0.5) * tu.grade_blend_sat;
            rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
                for x in 0..w {
                    let i = y * w + x;
                    let p = &mut row[x * 3..x * 3 + 3];
                    if p[0].is_nan() {
                        continue;
                    }
                    let mut q = [p[0], p[1], p[2]];
                    let (hue, chroma) = rgb_hue(q[0], q[1], q[2]);
                    let (bi, bj, tw) = band_of_with(hue, &tu.band_hues);
                    let protect = smoothstep(0.0, 0.12, chroma);
                    if bw {
                        let y0 = luma(q[0], q[1], q[2]);
                        let mix = s.bw_mix[bi] * (1.0 - tw) + s.bw_mix[bj] * tw;
                        let gray = (y0 * (1.0 + mix / 100.0 * 0.9 * chroma.min(1.0) * 2.0)).clamp(0.0, 1.0);
                        q = [gray; 3];
                    } else {
                        if hsl_active && protect > 0.0 {
                            let bi_ = &s.hsl[bi];
                            let bj_ = &s.hsl[bj];
                            let dh = (bi_.hue * (1.0 - tw) + bj_.hue * tw) / 100.0 * tu.hsl_hue * protect;
                            let sat_raw = (bi_.sat * (1.0 - tw) + bj_.sat * tw) / 100.0;
                            // Negative (desaturation) reaches full gray at -100; only positive uses the measured scale.
                            let ds = if sat_raw < 0.0 { sat_raw } else { sat_raw * tu.hsl_sat } * protect;
                            let dl = (bi_.lum * (1.0 - tw) + bj_.lum * tw) / 100.0 * protect;
                            q = rotate_hue(q, dh);
                            let y0 = luma(q[0], q[1], q[2]);
                            for v in &mut q {
                                *v = y0 + (*v - y0) * (1.0 + ds);
                            }
                            let k = 1.0 + dl * tu.hsl_lum;
                            for v in &mut q {
                                *v *= k;
                            }
                        }
                        if !pcs.is_empty() {
                            q = apply_point_colors(q, &pcs);
                        }
                        let mut lsat = 0.0;
                        if has_locals {
                            let lh = locals.get(i, F_HUE);
                            if lh != 0.0 {
                                q = rotate_hue(q, lh);
                            }
                            lsat = locals.get(i, F_SAT) / 100.0;
                        }
                        if vib != 0.0 || sat != 0.0 || lsat != 0.0 || dz_sat != 0.0 {
                            let mx = q[0].max(q[1]).max(q[2]).max(1e-5);
                            let s_hsv = (mx - q[0].min(q[1]).min(q[2])) / mx;
                            let skin = if (10.0..50.0).contains(&hue) { 0.5 } else { 1.0 };
                            // Stronger for low-saturation colors, with a weak response even for saturated ones.
                            let vg = if vib < 0.0 { tu.vib_neg } else { tu.vibrance * (1.0 + tu.vib_curve * vib) };
                            let fv = 1.0 + vib * vg * (1.0 - tu.vib_sat_ref * smoothstep(0.0, 0.8, s_hsv)) * skin;
                            let f = (fv * (1.0 + sat * tu.saturation + lsat) * (1.0 + dz_sat)).max(0.0);
                            // Adjust saturation in linear light preserving luminance (blended with display space by sat_linear).
                            let y0 = luma(q[0], q[1], q[2]);
                            let gam = [y0 + (q[0] - y0) * f, y0 + (q[1] - y0) * f, y0 + (q[2] - y0) * f];
                            if tu.sat_linear > 0.0 {
                                let mut lin = [color::fast_decode(q[0]), color::fast_decode(q[1]), color::fast_decode(q[2])];
                                let yl = luma(lin[0], lin[1], lin[2]);
                                for v in &mut lin {
                                    *v = (yl + (*v - yl) * f).max(0.0);
                                }
                                let k = tu.sat_linear.min(1.0);
                                for c in 0..3 {
                                    q[c] = gam[c] * (1.0 - k) + fast_encode(lin[c]) * k;
                                }
                            } else {
                                q = gam;
                            }
                            if vib < 0.0 && tu.vib_dark > 0.0 {
                                let d = 1.0 + vib * tu.vib_dark * s_hsv;
                                for v in &mut q {
                                    *v *= d.max(0.0);
                                }
                            }
                        }
                    }
                    if sh_tint != 0.0 {
                        let y0 = luma(q[0], q[1], q[2]);
                        q[1] -= sh_tint * 0.06 * (1.0 - y0).powi(2);
                    }
                    if grading {
                        // Add a color vector proportional to luminance in linear light.
                        let ye = luma(q[0], q[1], q[2]).clamp(0.0, 1.0);
                        let mut lin = [color::fast_decode(q[0]), color::fast_decode(q[1]), color::fast_decode(q[2])];
                        let ll = luma(lin[0], lin[1], lin[2]).max(1e-5);
                        // Range weights (measured shape): balance shifts the reference luminance, blend scales the width.
                        let yb = (ye + (0.5 - c_mid)).clamp(0.0, 1.0);
                        let bf = bl_w / 0.38;
                        let ws = (-(yb / (tu.grade_s_w * bf)).powi(2)).exp();
                        let wh = 1.0 - (-(yb / (tu.grade_h_w * bf)).powi(2)).exp();
                        let wm = (-((yb - 0.5) / (tu.grade_m_w * bf)).powi(2)).exp();
                        for (wi, (wheel, wt)) in [(g.shadows, ws), (g.midtones, wm), (g.highlights, wh), (g.global, 1.0)].into_iter().enumerate() {
                            if wt <= 0.0 || (wheel.sat == 0.0 && wheel.lum == 0.0) {
                                continue;
                            }
                            // Color direction: HSV(hue,1,1) normalized to unit luminance, minus gray.
                            let hc = grade_hue_color(wheel.hue, tu.grade_space);
                            // Color direction: chroma vector relative to gray normalized to length 1 (dividing by luminance blows up blues).
                            let hl = luma(hc[0], hc[1], hc[2]);
                            let dv = [hc[0] - hl, hc[1] - hl, hc[2] - hl];
                            let dn = (dv[0] * dv[0] + dv[1] * dv[1] + dv[2] * dv[2]).sqrt().max(1e-4);
                            let k = wheel.sat / 100.0 * tu.grade_sat * blend_sat;
                            let hv = [dv[0] / dn * k, dv[1] / dn * k, dv[2] / dn * k];
                            let lk = (1.0 + wheel.lum / 100.0 * tu.grade_lum * tu.grade_lum_w[wi] * wt).max(0.05);
                            for c in 0..3 {
                                lin[c] = ((lin[c] + hv[c] * ll * wt) * lk).max(0.0);
                            }
                        }
                        q = [fast_encode(lin[0]), fast_encode(lin[1]), fast_encode(lin[2])];
                    }
                    for c in 0..3 {
                        p[c] = q[c].clamp(0.0, 1.0);
                    }
                }
            });
        }

        prof.lap("7. 디테일");

        // -- 7. Detail (noise reduction, sharpening) --
        if !req.draft {
            self.detail(&mut rgb, w, h, s, opb, &locals, long_base);
        }

        prof.lap("8. 효과");

        // -- 8. Effects --
        let fx = &s.effects;
        let vig = (fx.vignette_amount / 100.0 * tu.vignette).clamp(-1.0, 1.0);
        let grain = fx.grain_amount / 100.0;
        if vig != 0.0 || (grain > 0.0 && !req.draft) {
            let (cw, ch) = (
                (s.geometry.crop[2] - s.geometry.crop[0]) * geo.fw,
                (s.geometry.crop[3] - s.geometry.crop[1]) * geo.fh,
            );
            let aspect = (cw / ch.max(1.0)).max(1e-3);
            let round = fx.vignette_roundness / 100.0;
            let p_exp = if round < 0.0 { 2.0 - round * 6.0 } else { 2.0 };
            let mid = fx.vignette_midpoint / 100.0;
            let feather = 0.04 + fx.vignette_feather / 100.0 * 0.9;
            let vh = fx.vignette_highlights / 100.0;
            let gsize = (1.0 + fx.grain_size / 25.0) * long_base / 2600.0;
            let rough = fx.grain_roughness / 100.0;
            rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
                for x in 0..w {
                    let p = &mut row[x * 3..x * 3 + 3];
                    if p[0].is_nan() {
                        continue;
                    }
                    if vig != 0.0 {
                        let (cx, cy) = geo.out_to_cropnorm(x as f32 + 0.5, y as f32 + 0.5);
                        let mut dx = (cx - 0.5) * 2.0;
                        let mut dy = (cy - 0.5) * 2.0;
                        if round > 0.0 {
                            // Toward round: stretch the long axis so the shape is closer to a true circle.
                            if aspect > 1.0 {
                                dx *= 1.0 + (aspect - 1.0) * round;
                            } else {
                                dy *= 1.0 + (1.0 / aspect - 1.0) * round;
                            }
                        }
                        let d = (dx.abs().powf(p_exp) + dy.abs().powf(p_exp)).powf(1.0 / p_exp) / 2f32.powf(1.0 / p_exp) * 1.414;
                        // Measured vignette profile (midpoint 50, feather 50 as reference), applied in linear light.
                        let mid_k = 0.6 + 0.8 * mid; // midpoint 50 -> 1.0
                        let soft = (0.5 / feather.max(0.02)).sqrt().clamp(0.4, 4.0); // feather 50 -> 1.0
                        let lin = [color::srgb_decode(p[0]), color::srgb_decode(p[1]), color::srgb_decode(p[2])];
                        let out = if vig < 0.0 {
                            let r = 0.913 * mid_k;
                            let v = 1.0 - (-(d / r).powf(5.5 * soft)).exp();
                            let y0 = luma(p[0], p[1], p[2]);
                            let protect = vh * smoothstep(0.55, 1.0, y0);
                            let k = 1.0 + vig * v * (1.0 - protect);
                            [lin[0] * k, lin[1] * k, lin[2] * k]
                        } else {
                            let r = 0.988 * mid_k;
                            let v = 1.0 - (-(d / r).powf(9.2 * soft)).exp();
                            [lin[0] + (1.0 - lin[0]) * vig * v, lin[1] + (1.0 - lin[1]) * vig * v, lin[2] + (1.0 - lin[2]) * vig * v]
                        };
                        for c in 0..3 {
                            p[c] = fast_encode(out[c].clamp(0.0, 1.0));
                        }
                    }
                    if grain > 0.0 && !req.draft {
                        let [bx, by] = uv[y * w + x];
                        let nz = value_noise(bx / gsize, by / gsize) * (1.0 - rough * 0.5)
                            + value_noise(bx / gsize * 2.3 + 17.0, by / gsize * 2.3 + 5.0) * rough * 0.5;
                        let y0 = luma(p[0], p[1], p[2]);
                        let amp = grain * 0.16 * (0.25 + 3.0 * y0 * (1.0 - y0));
                        for c in p.iter_mut() {
                            *c = (*c + nz * amp).clamp(0.0, 1.0);
                        }
                    }
                }
            });
        }

        // -- Red-eye removal --
        if !s.red_eye.is_empty() {
            apply_red_eye(&mut rgb, &uv, w, h, &s.red_eye, bwf, bhf);
        }

        // -- Privacy masking (face mosaic etc.) --
        if !s.privacy.is_empty() {
            apply_privacy(&mut rgb, &uv, w, h, &s.privacy, bwf, bhf, opb);
        }

        prof.lap("9. 출력");

        // -- 9. Output --
        let accent = ACCENT;
        let mut rgba = vec![0u8; n * 4];
        let hist = rgba
            .par_chunks_mut(w * 4)
            .zip(rgb.par_chunks(w * 3))
            .enumerate()
            .fold(Histogram::default, |mut hst, (y, (orow, irow))| {
                for x in 0..w {
                    let p = &irow[x * 3..x * 3 + 3];
                    let o = &mut orow[x * 4..x * 4 + 4];
                    if p[0].is_nan() {
                        o.copy_from_slice(&[0, 0, 0, 255]);
                        continue;
                    }
                    let r = (p[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    let g = (p[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    let b = (p[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    hst.r[r as usize] += 1;
                    hst.g[g as usize] += 1;
                    hst.b[b as usize] += 1;
                    let l = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) as usize;
                    hst.l[l.min(255)] += 1;
                    let mut out = [r, g, b];
                    if req.clipping {
                        if r == 255 || g == 255 || b == 255 {
                            out = accent;
                        } else if r <= 1 && g <= 1 && b <= 1 {
                            out = [255, 255, 255];
                        }
                    }
                    if let Some(ov) = &overlay {
                        let a = ov[y * w + x].clamp(0.0, 1.0) * 0.55;
                        for c in 0..3 {
                            out[c] = (out[c] as f32 * (1.0 - a) + accent[c] as f32 * a) as u8;
                        }
                    }
                    o[0] = out[0];
                    o[1] = out[1];
                    o[2] = out[2];
                    o[3] = 255;
                }
                hst
            })
            .reduce(Histogram::default, |a, b| a.merge(&b));

        let float = if req.keep_float {
            for v in rgb.iter_mut() {
                if v.is_nan() {
                    *v = 0.0;
                }
            }
            Some(rgb)
        } else {
            None
        };
        prof.lap(tr!("끝", "finished"));
        RenderOutput { w, h, rgba, float, hist }
    }

    fn detail(&mut self, rgb: &mut [f32], w: usize, h: usize, s: &DevelopSettings, opb: f32, locals: &Locals, _long: f32) {
        let d = &s.detail;
        let n = w * h;
        let has_local_sharp = locals.has(F_SHARP);
        let has_local_noise = locals.has(F_NOISE);
        let y_of = |rgb: &[f32]| -> Vec<f32> {
            rgb.par_chunks(3).map(|p| if p[0].is_nan() { 0.0 } else { luma(p[0], p[1], p[2]) }).collect()
        };
        // Defringe (purple/green): desaturate only those hues near brightness edges.
        let (ap, ag) = ((s.lens.defringe_purple / 20.0).clamp(0.0, 1.0), (s.lens.defringe_green / 20.0).clamp(0.0, 1.0));
        if ap > 0.0 || ag > 0.0 {
            defringe(rgb, w, h, ap, ag, opb);
        }
        // Color noise reduction: smooth the chroma channels.
        if d.nr_color > 0.0 {
            let sigma = (d.nr_color / 100.0) * 5.0 * opb.min(1.0) * (0.5 + d.nr_color_smooth / 100.0);
            if sigma >= 0.75 {
                let y = y_of(rgb);
                let chroma = |off: usize| -> Vec<f32> {
                    rgb.par_chunks(3)
                        .zip(y.par_iter())
                        .map(|(p, yy)| if p[0].is_nan() { 0.0 } else { p[off] - yy })
                        .collect()
                };
                let eps = 0.0004 + (1.0 - d.nr_color_detail / 100.0) * 0.004;
                // Chroma is low-frequency, so computing at 2x subsampling costs no visible quality.
                let r = ((sigma * 2.0).round() as usize).max(2);
                let (cb, cr) = rayon::join(
                    || filters::guided_self(&chroma(2), w, h, r, eps, 2),
                    || filters::guided_self(&chroma(0), w, h, r, eps, 2),
                );
                rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
                    if p[0].is_nan() {
                        return;
                    }
                    let yy = y[i];
                    let r = yy + cr[i];
                    let b = yy + cb[i];
                    let g = (yy - 0.2126 * r - 0.0722 * b) / 0.7152;
                    p[0] = r.clamp(0.0, 1.0);
                    p[1] = g.clamp(0.0, 1.0);
                    p[2] = b.clamp(0.0, 1.0);
                });
            }
        }
        // Luminance noise reduction
        if d.nr_luma > 0.0 || has_local_noise {
            // Smoothing strength: the larger of the global and local values (so local-only NR still works).
            let strength = if has_local_noise { d.nr_luma.max(locals.max(F_NOISE).clamp(0.0, 100.0)) } else { d.nr_luma };
            let r = ((1.0 + strength.max(30.0) / 100.0 * 3.0) * opb.min(1.0)).round().max(1.0) as usize;
            let y = y_of(rgb);
            let eps_base = 0.00002 + (strength / 100.0).powi(2) * 0.0025 * (1.2 - d.nr_luma_detail / 100.0);
            if opb >= 0.25 {
                let sm = filters::guided_self(&y, w, h, r, eps_base.max(0.00002), 1);
                let contrast_keep = d.nr_luma_contrast / 100.0;
                rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
                    if p[0].is_nan() {
                        return;
                    }
                    let mut amt = (d.nr_luma / 100.0).min(1.0);
                    if has_local_noise {
                        amt = (amt + locals.get(i, F_NOISE) / 100.0).clamp(0.0, 1.0);
                    }
                    let dy = (sm[i] - y[i]) * amt * (1.0 - contrast_keep * 0.5);
                    for c in p.iter_mut() {
                        *c = (*c + dy).clamp(0.0, 1.0);
                    }
                });
            }
        }
        // Sharpening: luminance unsharp mask + detail (halo suppression) + masking (edges only).
        if d.sharpen_amount > 0.0 || has_local_sharp {
            let sigma = d.sharpen_radius * opb;
            // Skip sharpening in heavily downscaled previews (e.g. fit view) where it isn't visible.
            if sigma >= 0.45 {
                let y = y_of(rgb);
                let bl = filters::gauss_blur(&y, w, h, sigma.max(0.5));
                let amount = d.sharpen_amount / 100.0 * 1.4;
                let limit = 0.015 + d.sharpen_detail / 100.0 * 0.2;
                let mask_t = d.sharpen_masking / 100.0;
                let edge = if mask_t > 0.0 {
                    let g: Vec<f32> = (0..n)
                        .into_par_iter()
                        .map(|i| {
                            let x = i % w;
                            let yy = i / w;
                            let xr = (x + 1).min(w - 1);
                            let yd = (yy + 1).min(h - 1);
                            let gx = bl[yy * w + xr] - bl[i];
                            let gy = bl[yd * w + x] - bl[i];
                            (gx * gx + gy * gy).sqrt()
                        })
                        .collect();
                    Some(filters::box_blur(&g, w, h, 1))
                } else {
                    None
                };
                rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
                    if p[0].is_nan() {
                        return;
                    }
                    let mut a = amount;
                    if has_local_sharp {
                        a = (a + locals.get(i, F_SHARP) / 100.0).max(0.0);
                    }
                    let mut dd = (y[i] - bl[i]).clamp(-limit, limit) * a;
                    if let Some(e) = &edge {
                        dd *= smoothstep(mask_t * 0.02, mask_t * 0.06 + 0.001, e[i]);
                    }
                    for c in p.iter_mut() {
                        *c = (*c + dd).clamp(0.0, 1.0);
                    }
                });
            }
        }
    }
}

/// Grading wheel hue -> linear sRGB color (in the selected hue-angle color space).
fn grade_hue_color(hue: f32, space: f32) -> [f32; 3] {
    let c = color::hsv_to_rgb(hue, 1.0, 1.0);
    let sp = space.round() as i32;
    if sp == 0 {
        return c;
    }
    let lin = if sp == 2 { [c[0].powf(1.8), c[1].powf(1.8), c[2].powf(1.8)] } else { c };
    let m = super::dcp::prophoto_to_srgb();
    
    [
        (m[0][0] * lin[0] as f64 + m[0][1] * lin[1] as f64 + m[0][2] * lin[2] as f64).max(0.0) as f32,
        (m[1][0] * lin[0] as f64 + m[1][1] * lin[1] as f64 + m[1][2] * lin[2] as f64).max(0.0) as f32,
        (m[2][0] * lin[0] as f64 + m[2][1] * lin[1] as f64 + m[2][2] * lin[2] as f64).max(0.0) as f32,
    ]
}

/// Measured curve interpolation: points = (slider value, curve); 0 is identity. Returns the change at y.
fn measured_delta(points: &[(f32, &[f32; 256])], v: f32, y: f32) -> f32 {
    if v == 0.0 {
        return 0.0;
    }
    let eval = |c: &[f32; 256]| -> f32 {
        let p = y.clamp(0.0, 1.0) * 255.0;
        let i = (p as usize).min(254);
        let f = p - i as f32;
        c[i] + (c[i + 1] - c[i]) * f - y
    };
    // Piecewise-linear interpolation over measured points of the same sign (0 -> 0).
    let mut prev = (0.0f32, 0.0f32);
    let side: Vec<&(f32, &[f32; 256])> = if v > 0.0 {
        points.iter().filter(|p| p.0 > 0.0).collect()
    } else {
        points.iter().filter(|p| p.0 < 0.0).rev().collect()
    };
    for (pv, c) in side {
        let d = eval(c);
        if v.abs() <= pv.abs() {
            let t = (v.abs() - prev.0.abs()) / (pv.abs() - prev.0.abs()).max(1e-6);
            return prev.1 + (d - prev.1) * t;
        }
        prev = (*pv, d);
    }
    // Beyond the measured range, extrapolate proportionally from the last value.
    prev.1 * (v.abs() / prev.0.abs().max(1.0))
}

#[inline]
#[allow(dead_code)]
/// Whether user point curves are applied in ProPhoto primaries (default on). Test switch: DARKROOM_CURVE_SPACE=srgb disables.
fn curve_space_pp() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("DARKROOM_CURVE_SPACE").map(|v| v != "srgb").unwrap_or(true))
}

fn apply_point_colors(mut q: [f32; 3], pcs: &[super::settings::PointColor]) -> [f32; 3] {
    let (hue, _) = rgb_hue(q[0], q[1], q[2]);
    let mx = q[0].max(q[1]).max(q[2]).max(1e-5);
    let sat = (mx - q[0].min(q[1]).min(q[2])) / mx;
    let lum = luma(q[0], q[1], q[2]);
    let (mut dh, mut ds, mut dl) = (0.0f32, 0.0f32, 0.0f32);
    for p in pcs {
        let w = p.weight(hue, sat, lum);
        if w <= 0.0 {
            continue;
        }
        dh += p.hue_shift / 100.0 * 60.0 * w;
        ds += p.sat_shift / 100.0 * w;
        dl += p.lum_shift / 100.0 * w;
    }
    if dh != 0.0 {
        q = rotate_hue(q, dh);
    }
    if ds != 0.0 {
        let y0 = luma(q[0], q[1], q[2]);
        let f = (1.0 + ds).max(0.0);
        for v in &mut q {
            *v = y0 + (*v - y0) * f;
        }
    }
    if dl != 0.0 {
        // Brightness acts like exposure (+-100 = about +-1 stop).
        let k = 2f32.powf(dl);
        for v in &mut q {
            *v = color::fast_encode((color::fast_decode(v.max(0.0)) * k).min(1.5));
        }
    }
    q
}


#[inline]
fn band_of_with(hue: f32, hs: &[f32; 8]) -> (usize, usize, f32) {
    let hs = *hs;
    for i in 0..HSL_BANDS {
        let a = hs[i];
        let b = if i + 1 < HSL_BANDS { hs[i + 1] } else { 360.0 };
        if hue >= a && hue < b {
            let t = (hue - a) / (b - a);
            let t = t * t * (3.0 - 2.0 * t);
            return (i, (i + 1) % HSL_BANDS, t);
        }
    }
    (0, 1, 0.0)
}

#[inline]
fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

/// Deterministic value noise (based on source coordinates, so grain is the same at any zoom or region).
#[inline]
fn value_noise(x: f32, y: f32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let tx = x - xi;
    let ty = y - yi;
    let (xi, yi) = (xi as i32, yi as i32);
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let a = hash2(xi, yi);
    let b = hash2(xi + 1, yi);
    let c = hash2(xi, yi + 1);
    let d = hash2(xi + 1, yi + 1);
    let ab = a + (b - a) * sx;
    let cd = c + (d - c) * sx;
    ab + (cd - ab) * sy
}

/// Output size matching the crop result (long edge limited).
pub fn fit_size(src_w: usize, src_h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    let k = (max_w as f32 / src_w as f32).min(max_h as f32 / src_h as f32).min(1.0);
    (((src_w as f32 * k).round() as usize).max(1), ((src_h as f32 * k).round() as usize).max(1))
}

/// Temperature/tint values that make the clicked point neutral gray (white balance eyedropper).
pub fn wb_from_sample(sample_linear: [f32; 3], current: &DevelopSettings) -> (f32, f32) {
    // Solve, in log space, for the extra correction needed on top of the current temp/tint.
    let [r, g, b] = sample_linear;
    if r <= 0.0 || g <= 0.0 || b <= 0.0 {
        return (current.temp, current.tint);
    }
    // temp: r *= 2^(0.5t), b *= 2^(-0.5t) -> log2(b/r) = t_needed
    let dt = (b / r).log2(); // r*2^(0.5dt) = b*2^(-0.5dt)
    let rb = (r * b).sqrt();
    // tint: g *= 2^(-0.35 ti) -> g*2^(-0.35 dti) = rb
    let dti = (g / rb).log2() / 0.35;
    (
        (current.temp + dt * 100.0).clamp(-100.0, 100.0),
        (current.tint + dti * 100.0).clamp(-100.0, 100.0),
    )
}

#[allow(dead_code)]
pub fn kelvin_label(temp: f32) -> f32 {
    color::temp_slider_to_kelvin_label(5500.0, temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_source(is_raw: bool) -> SourceImage {
        let (w, h) = (256, 160);
        let mut img = LinearImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = x as f32 / (w - 1) as f32;
                let i = (y * w + x) * 3;
                img.data[i] = color::srgb_decode(v);
                img.data[i + 1] = color::srgb_decode(v * 0.8);
                img.data[i + 2] = color::srgb_decode(v * 0.6);
            }
        }
        SourceImage::new(img, is_raw, 64)
    }

    fn req(s: &DevelopSettings, w: usize, h: usize) -> RenderRequest<'_> {
        RenderRequest {
            settings: s,
            out_w: w,
            out_h: h,
            region: [0.0, 0.0, 1.0, 1.0],
            draft: false,
            clipping: false,
            mask_overlay: None,
            keep_float: true,
        }
    }

    #[test]
    fn jpeg_default_is_identity() {
        let src = gradient_source(false);
        let s = DevelopSettings::default_for(false);
        let mut e = Engine::default();
        let out = e.render(&src, &req(&s, 256, 160));
        // Within +-2 of the source 8-bit values.
        for x in [0usize, 40, 128, 200, 255] {
            let o = &out.rgba[(80 * 256 + x) * 4..(80 * 256 + x) * 4 + 3];
            let v = x as f32 / 255.0;
            let exp = [(v * 255.0).round(), (v * 0.8 * 255.0).round(), (v * 0.6 * 255.0).round()];
            for c in 0..3 {
                assert!((o[c] as f32 - exp[c]).abs() <= 2.0, "x={x} c={c} got {} exp {}", o[c], exp[c]);
            }
        }
    }

    #[test]
    fn exposure_brightens() {
        let src = gradient_source(false);
        let mut s = DevelopSettings::default();
        let mut e = Engine::default();
        let a = e.render(&src, &req(&s, 128, 80));
        s.exposure = 1.0;
        let b = e.render(&src, &req(&s, 128, 80));
        let i = (40 * 128 + 50) * 4;
        assert!(b.rgba[i] > a.rgba[i] + 20);
    }

    #[test]
    fn every_module_runs_without_nan() {
        let src = gradient_source(true);
        let mut s = DevelopSettings::default_for(true);
        s.highlights = -60.0;
        s.shadows = 50.0;
        s.clarity = 40.0;
        s.texture = 30.0;
        s.dehaze = 25.0;
        s.vibrance = 30.0;
        s.saturation = -10.0;
        s.hsl[5].sat = -40.0;
        s.grading.shadows = Wheel { hue: 220.0, sat: 30.0, lum: 0.0 };
        s.detail.nr_luma = 30.0;
        s.effects.vignette_amount = -30.0;
        s.effects.grain_amount = 20.0;
        s.geometry.angle = 5.0;
        s.geometry.crop = [0.1, 0.1, 0.9, 0.9];
        s.lens.distortion = 20.0;
        s.masks.push(Mask {
            components: vec![MaskComponent {
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: [0.5, 0.5], radius: [0.3, 0.3], angle: 0.0, feather: 50.0 },
            }],
            adj: LocalAdjust { exposure: 1.0, clarity: 20.0, saturation: 20.0, ..Default::default() },
            ..Default::default()
        });
        let mut e = Engine::default();
        let out = e.render(&src, &RenderRequest { mask_overlay: Some(0), ..req(&s, 200, 120) });
        assert!(out.float.unwrap().iter().all(|v| v.is_finite()));
        // B&W plus grading keeps color by design (split toning), so disable grading and check for neutral.
        s.treatment = Treatment::BlackWhite;
        s.grading = ColorGrading::default();
        let out = e.render(&src, &req(&s, 200, 120));
        let i = (60 * 200 + 100) * 4;
        assert_eq!(out.rgba[i], out.rgba[i + 1]);
    }

    #[test]
    fn wb_picker_neutralizes() {
        let s = DevelopSettings::default();
        let (t, ti) = wb_from_sample([0.5, 0.4, 0.3], &s);
        let s2 = DevelopSettings { temp: t, tint: ti, ..Default::default() };
        let m = linear_matrix(&s2, false);
        let q = mat_mul(&m, [0.5, 0.4, 0.3]);
        assert!((q[0] - q[2]).abs() < 0.01 && (q[0] - q[1]).abs() < 0.01, "{q:?}");
    }
}

/// Test switch: setting DARKROOM_PROFILE prints per-stage timings to stderr.
struct Prof {
    on: bool,
    t: std::time::Instant,
    prev: &'static str,
}

impl Prof {
    fn new() -> Self {
        Self { on: std::env::var_os("DARKROOM_PROFILE").is_some(), t: std::time::Instant::now(), prev: "1. 샘플링" }
    }
    fn lap(&mut self, next: &'static str) {
        if self.on {
            eprintln!("  [prof] {:<10} {:>7.2}ms", self.prev, self.t.elapsed().as_secs_f64() * 1000.0);
        }
        self.t = std::time::Instant::now();
        self.prev = next;
    }
}

#[cfg(test)]
mod bench {
    /// Test switch: DARKROOM_PROBE_FILE=<raw> cargo test --profile calib pipeline::bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn render_speed() {
        let Some(p) = std::env::var_os("DARKROOM_PROBE_FILE") else { return };
        let p = std::path::PathBuf::from(p);
        let o = crate::imaging::meta::read_raw_meta(&p).map(|m| m.orientation).unwrap_or(1);
        let src = crate::imaging::decode::decode_source(&p, o).unwrap();
        let mut e = super::Engine::default();
        let mut s = crate::develop::settings::DevelopSettings::default_for(true);
        s.clarity = 25.0;
        s.texture = 100.0;
        s.dehaze = -100.0;
        s.highlights = -60.0;
        s.shadows = 40.0;
        s.vibrance = 30.0;
        for i in 0..4 {
            s.exposure = 0.1 * i as f32;
            let t = std::time::Instant::now();
            let _ = e.render(&src, &super::RenderRequest { settings: &s, out_w: 1100, out_h: 1650, region: [0.0, 0.0, 1.0, 1.0], draft: false, clipping: false, mask_overlay: None, keep_float: false });
            println!("render {i}: {:?}", t.elapsed());
        }
        // While dragging a slider (draft: screen size x DRAFT_SCALE).
        let k = crate::config::DRAFT_SCALE;
        for i in 0..3 {
            s.exposure = 0.1 * i as f32;
            let t = std::time::Instant::now();
            let _ = e.render(&src, &super::RenderRequest { settings: &s, out_w: (1100.0 * k) as usize, out_h: (1650.0 * k) as usize, region: [0.0, 0.0, 1.0, 1.0], draft: true, clipping: false, mask_overlay: None, keep_float: false });
            println!("draft {i}: {:?}", t.elapsed());
        }
        // 100% region (2560x1400 screen).
        for i in 0..2 {
            s.exposure = 0.1 * i as f32;
            let t = std::time::Instant::now();
            let (bw, bh) = (src.width() as f32, src.height() as f32);
            let (rw, rh) = (2560.0 / bw, 1400.0 / bh);
            let _ = e.render(&src, &super::RenderRequest { settings: &s, out_w: 2560, out_h: 1400, region: [0.3, 0.3, 0.3 + rw, 0.3 + rh], draft: false, clipping: false, mask_overlay: None, keep_float: false });
            println!("100% {i}: {:?}", t.elapsed());
        }
    }
}

/// Defringe: desaturate purple (~285 deg) and green (~120 deg) pixels near edges (luminance gradient).
/// Edge search radius scales with amount (source pixels, adjusted for output scale); same-colored objects away from edges stay.
pub fn defringe(rgb: &mut [f32], w: usize, h: usize, ap: f32, ag: f32, opb: f32) {
    use super::color::{luma, rgb_hue};
    let y: Vec<f32> = rgb.par_chunks(3).map(|p| if p[0].is_nan() { 0.0 } else { luma(p[0], p[1], p[2]) }).collect();
    // Luminance gradient magnitude
    let g: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, yy) = (i % w, i / w);
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let (yu, yd) = (yy.saturating_sub(1), (yy + 1).min(h - 1));
            let gx = y[yy * w + xr] - y[yy * w + xl];
            let gy = y[yd * w + x] - y[yu * w + x];
            (gx * gx + gy * gy).sqrt()
        })
        .collect();
    // Widen around edges (approximate a box max with an enlarged box mean).
    let rad = ((1.0 + 3.0 * ap.max(ag)) * opb.max(0.25)).round().max(1.0) as usize;
    let near = super::filters::box_blur(&g, w, h, rad);
    let band = |hue: f32, c: f32, width: f32| -> f32 {
        let d = ((hue - c + 540.0).rem_euclid(360.0) - 180.0).abs();
        let t = (1.0 - d / width).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    rgb.par_chunks_mut(3).enumerate().for_each(|(i, p)| {
        if p[0].is_nan() {
            return;
        }
        let (hue, chroma) = rgb_hue(p[0], p[1], p[2]);
        if chroma < 0.02 {
            return;
        }
        let edge = smoothstep(0.004, 0.03, near[i]);
        let k = edge * (ap * band(hue, 285.0, 50.0) + ag * band(hue, 120.0, 45.0)) * smoothstep(0.02, 0.08, chroma);
        if k <= 0.0 {
            return;
        }
        let yy = y[i];
        for c in p.iter_mut() {
            *c = yy + (*c - yy) * (1.0 - k.min(1.0));
        }
    });
}

/// Red-eye removal: inside the ellipse, softly select strongly red pixels, replace red with the green/blue mean and darken.
/// The mask is slightly blurred relative to region size to avoid blotchy edges.
pub fn apply_red_eye(rgb: &mut [f32], uv: &[[f32; 2]], w: usize, h: usize, regions: &[super::settings::RedEye], bw: f32, bh: f32) {
    for r in regions {
        let rx = (r.radius[0] * bw).max(1.0);
        let ry = (r.radius[1] * bh).max(1.0);
        let (cx, cy) = (r.center[0] * bw, r.center[1] * bh);
        // Output bounding box
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                let [bx, by] = uv[y * w + x];
                let (dx, dy) = ((bx - cx) / rx, (by - cy) / ry);
                if dx * dx + dy * dy <= 1.0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x0 > x1 || y0 > y1 {
            continue;
        }
        let (bw_, bh_) = (x1 - x0 + 1, y1 - y0 + 1);
        // Pupil size -> redness threshold (50 -> 0.35)
        let t0 = (0.55 - r.pupil / 100.0 * 0.4).clamp(0.05, 0.6);
        let t1 = t0 + 0.15;
        let mut m = vec![0.0f32; bw_ * bh_];
        for y in 0..bh_ {
            for x in 0..bw_ {
                let i = (y + y0) * w + x + x0;
                let [bx, by] = uv[i];
                let (dx, dy) = ((bx - cx) / rx, (by - cy) / ry);
                let d = (dx * dx + dy * dy).sqrt();
                if d > 1.0 {
                    continue;
                }
                let (rr, gg, bb) = (rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]);
                if rr.is_nan() {
                    continue;
                }
                let red = (rr - gg.max(bb)) / rr.max(1e-3);
                let k = ((red - t0) / (t1 - t0)).clamp(0.0, 1.0);
                // Fade over the outer 20%.
                let edge = ((1.0 - d) / 0.2).clamp(0.0, 1.0);
                m[y * bw_ + x] = k * k * (3.0 - 2.0 * k) * edge;
            }
        }
        // Small box blur (~4% of region diameter).
        let rad = ((bw_.max(bh_) as f32) * 0.04).round() as usize;
        if rad > 0 {
            let mut t = vec![0.0f32; m.len()];
            for y in 0..bh_ {
                for x in 0..bw_ {
                    let (a, b) = (x.saturating_sub(rad), (x + rad).min(bw_ - 1));
                    let s: f32 = m[y * bw_ + a..=y * bw_ + b].iter().sum();
                    t[y * bw_ + x] = s / (b - a + 1) as f32;
                }
            }
            for y in 0..bh_ {
                for x in 0..bw_ {
                    let (a, b) = (y.saturating_sub(rad), (y + rad).min(bh_ - 1));
                    let mut s = 0.0;
                    for yy in a..=b {
                        s += t[yy * bw_ + x];
                    }
                    m[y * bw_ + x] = s / (b - a + 1) as f32;
                }
            }
        }
        let dk = r.darken / 100.0;
        for y in 0..bh_ {
            for x in 0..bw_ {
                let k = m[y * bw_ + x];
                if k <= 0.0 {
                    continue;
                }
                let i = ((y + y0) * w + x + x0) * 3;
                let (gg, bb) = (rgb[i + 1], rgb[i + 2]);
                // Replace the red reflection with the green/blue mean (toward neutral), then darken.
                let n = (gg + bb) * 0.5;
                let s = 1.0 - dk * 0.85;
                let target = [n * s, gg * s, bb * s];
                for c in 0..3 {
                    rgb[i + c] += (target[c] - rgb[i + c]) * k;
                }
            }
        }
    }
}

/// Privacy region processing (output gamma space). Mosaic cells use source coordinates, so screen scale and export match.
#[allow(clippy::too_many_arguments)]
fn apply_privacy(rgb: &mut [f32], uv: &[[f32; 2]], w: usize, h: usize, regions: &[super::settings::PrivacyRegion], bw: f32, bh: f32, opb: f32) {
    use super::settings::PrivacyKind;
    for r in regions {
        let rx = (r.radius[0] * bw).max(1.0);
        let ry = (r.radius[1] * bh).max(1.0);
        let (cx, cy) = (r.center[0] * bw, r.center[1] * bh);
        let dist = |bx: f32, by: f32| -> f32 {
            let dx = (bx - cx) / rx;
            let dy = (by - cy) / ry;
            if r.ellipse { (dx * dx + dy * dy).sqrt() } else { dx.abs().max(dy.abs()) }
        };
        // Output bounding box
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                let [bx, by] = uv[y * w + x];
                if dist(bx, by) <= 1.0 && !rgb[(y * w + x) * 3].is_nan() {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x0 == usize::MAX {
            continue;
        }
        let k = r.strength.clamp(1.0, 100.0) / 100.0;
        match r.kind {
            PrivacyKind::Mosaic => {
                let blk = (rx.min(ry) * (0.08 + 0.42 * k)).max(2.0);
                let mut acc: std::collections::HashMap<(i32, i32), ([f32; 3], f32)> = std::collections::HashMap::new();
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let i = y * w + x;
                        let [bx, by] = uv[i];
                        if dist(bx, by) > 1.0 || rgb[i * 3].is_nan() {
                            continue;
                        }
                        let key = (((bx - cx) / blk).floor() as i32, ((by - cy) / blk).floor() as i32);
                        let e = acc.entry(key).or_insert(([0.0; 3], 0.0));
                        for c in 0..3 {
                            e.0[c] += rgb[i * 3 + c];
                        }
                        e.1 += 1.0;
                    }
                }
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let i = y * w + x;
                        let [bx, by] = uv[i];
                        if dist(bx, by) > 1.0 || rgb[i * 3].is_nan() {
                            continue;
                        }
                        let key = (((bx - cx) / blk).floor() as i32, ((by - cy) / blk).floor() as i32);
                        if let Some((sum, n)) = acc.get(&key) {
                            for c in 0..3 {
                                rgb[i * 3 + c] = sum[c] / n;
                            }
                        }
                    }
                }
            }
            PrivacyKind::Blur => {
                let sigma = (rx.min(ry) * opb * (0.04 + 0.3 * k)).max(1.0);
                let pad = (sigma * 3.0) as usize + 1;
                let (bx0, by0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
                let (bx1, by1) = ((x1 + pad).min(w - 1), (y1 + pad).min(h - 1));
                let (cw, ch) = (bx1 - bx0 + 1, by1 - by0 + 1);
                let mut chans = vec![vec![0.0f32; cw * ch]; 3];
                for y in 0..ch {
                    for x in 0..cw {
                        let i = (by0 + y) * w + bx0 + x;
                        for c in 0..3 {
                            let v = rgb[i * 3 + c];
                            chans[c][y * cw + x] = if v.is_nan() { 0.0 } else { v };
                        }
                    }
                }
                let blurred: Vec<Vec<f32>> = chans.iter().map(|ch_| filters::gauss_blur(ch_, cw, ch, sigma)).collect();
                for y in 0..ch {
                    for x in 0..cw {
                        let i = (by0 + y) * w + bx0 + x;
                        let [bx, by] = uv[i];
                        let d = dist(bx, by);
                        if d > 1.0 || rgb[i * 3].is_nan() {
                            continue;
                        }
                        let a = 1.0 - smoothstep(0.85, 1.0, d);
                        for c in 0..3 {
                            rgb[i * 3 + c] = rgb[i * 3 + c] * (1.0 - a) + blurred[c][y * cw + x] * a;
                        }
                    }
                }
            }
            PrivacyKind::Fill => {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let i = y * w + x;
                        let [bx, by] = uv[i];
                        if dist(bx, by) <= 1.0 && !rgb[i * 3].is_nan() {
                            rgb[i * 3] = 0.06;
                            rgb[i * 3 + 1] = 0.06;
                            rgb[i * 3 + 2] = 0.06;
                        }
                    }
                }
            }
        }
    }
}

/// Heal correction grid: the interior is filled by a membrane (Laplace equation) from boundary differences, as in membrane cloning.
struct HealGrid {
    x0: f32,
    y0: f32,
    cell: f32,
    w: usize,
    h: usize,
    /// Per-channel log ratio (target/source).
    v: Vec<[f32; 3]>,
}

impl HealGrid {
    #[inline]
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let fx = ((x - self.x0) / self.cell - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = ((y - self.y0) / self.cell - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (ix, iy) = ((fx as usize).min(self.w.saturating_sub(2)), (fy as usize).min(self.h.saturating_sub(2)));
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let at = |x: usize, y: usize| self.v[y.min(self.h - 1) * self.w + x.min(self.w - 1)];
        let (a, b, c, d) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
        let mut o = [0.0f32; 3];
        for k in 0..3 {
            o[k] = (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
        }
        o
    }
}

struct PreparedSpot {
    /// Target path (source pixels; a circle is a single point).
    nodes: Vec<[f32; 2]>,
    /// Source position = target + (ox, oy).
    ox: f32,
    oy: f32,
    r: f32,
    feather: f32,
    opacity: f32,
    heal: Option<HealGrid>,
    /// Influence radius (source pixels).
    bbox: [f32; 4],
    /// Clone patch and its region (source pixels).
    fill: Option<(Arc<super::aistore::RgbPatch>, [f32; 4])>,
}

impl PreparedSpot {
    /// Clone patch value (None outside the region or without a patch).
    #[inline]
    fn fill_at(&self, bx: f32, by: f32) -> Option<[f32; 3]> {
        let (p, b) = self.fill.as_ref()?;
        let u = (bx - b[0]) / (b[2] - b[0]) * p.w as f32 - 0.5;
        let v = (by - b[1]) / (b[3] - b[1]) * p.h as f32 - 0.5;
        let x = u.clamp(0.0, (p.w - 1) as f32);
        let y = v.clamp(0.0, (p.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(p.w - 1), (y0 + 1).min(p.h - 1));
        let (tx, ty) = (x - x0 as f32, y - y0 as f32);
        let g = |xx: usize, yy: usize, c: usize| p.data[(yy * p.w + xx) * 3 + c];
        let mut o = [0.0f32; 3];
        for (c, v) in o.iter_mut().enumerate() {
            let a = g(x0, y0, c) * (1.0 - tx) + g(x1, y0, c) * tx;
            let b2 = g(x0, y1, c) * (1.0 - tx) + g(x1, y1, c) * tx;
            *v = a * (1.0 - ty) + b2 * ty;
        }
        Some(o)
    }
}

/// Distance from a point to the path (source pixels).
#[inline]
fn path_dist(nodes: &[[f32; 2]], x: f32, y: f32) -> f32 {
    if nodes.len() == 1 {
        return (x - nodes[0][0]).hypot(y - nodes[0][1]);
    }
    let mut best = f32::MAX;
    for w in nodes.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let l2 = (ex * ex + ey * ey).max(1e-6);
        let t = (((x - a[0]) * ex + (y - a[1]) * ey) / l2).clamp(0.0, 1.0);
        best = best.min((x - a[0] - ex * t).hypot(y - a[1] - ey * t));
    }
    best
}

impl PreparedSpot {
    /// For a pixel (source coordinates): (weight, source position, heal scale).
    #[inline]
    fn eval(&self, bx: f32, by: f32) -> Option<(f32, f32, f32, [f32; 3])> {
        if bx < self.bbox[0] || by < self.bbox[1] || bx > self.bbox[2] || by > self.bbox[3] {
            return None;
        }
        let rho = path_dist(&self.nodes, bx, by) / self.r;
        if rho >= 1.0 {
            return None;
        }
        let w = self.opacity * (1.0 - smoothstep(1.0 - self.feather, 1.0, rho));
        if w <= 0.0 {
            return None;
        }
        let gain = match &self.heal {
            Some(g) => {
                let l = g.sample(bx, by);
                [l[0].exp(), l[1].exp(), l[2].exp()]
            }
            None => [1.0; 3],
        };
        Some((w, bx + self.ox, by + self.oy, gain))
    }
}

/// Resample the path at spacing `step` (source pixels).
fn resample_path(pts: &[[f32; 2]], step: f32, max_n: usize) -> Vec<[f32; 2]> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let total: f32 = pts.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).sum();
    let step = step.max(total / max_n as f32).max(0.5);
    let mut out = vec![pts[0]];
    let mut acc = 0.0f32;
    for w in pts.windows(2) {
        let seg = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        let mut t = step - acc;
        while t <= seg {
            let f = t / seg.max(1e-6);
            out.push([w[0][0] + (w[1][0] - w[0][0]) * f, w[0][1] + (w[1][1] - w[0][1]) * f]);
            t += step;
        }
        acc = (acc + seg) % step;
    }
    let last = *pts.last().unwrap();
    if out.last().map(|p| (p[0] - last[0]).hypot(p[1] - last[1]) > step * 0.3).unwrap_or(true) {
        out.push(last);
    }
    out
}

/// Build the heal grid: fix log ratios on the boundary band outside the radius and solve the interior with the Laplace equation (SOR).
/// Boundary start and boundary sampling radius (relative to the radius) were chosen with a texture blotch metric (heal_blotch_probe).
const HEAL_BAND: f32 = 1.05;
const HEAL_FOOT: f32 = 0.15;

fn build_heal_grid(lvl: &LinearImage, k: f32, nodes: &[[f32; 2]], ox: f32, oy: f32, r: f32, bbox: [f32; 4]) -> HealGrid {
    let span = (bbox[2] - bbox[0]).max(bbox[3] - bbox[1]);
    let cell = (r / 5.0).max(span / 140.0).max(0.75);
    let w = (((bbox[2] - bbox[0]) / cell).ceil() as usize + 1).max(3);
    let h = (((bbox[3] - bbox[1]) / cell).ceil() as usize + 1).max(3);
    let (x0, y0) = (bbox[0], bbox[1]);
    // Source pixels -> sample level coordinates, averaged over a cell to reduce noise.
    let foot = HEAL_FOOT;
    let sample_avg = |x: f32, y: f32| -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        let o = if foot > 0.0 { r * foot } else { cell * 0.35 };
        let mut n = 0.0f32;
        for (dx, dy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0), (0.0, 0.0), (-1.4, 0.0), (1.4, 0.0), (0.0, -1.4), (0.0, 1.4)] {
            if foot <= 0.0 && (dx.abs() > 1.2 || dy.abs() > 1.2) {
                continue;
            }
            let (sx, sy) = (((x + dx * o) * k).clamp(0.0, lvl.w as f32 - 1.001), ((y + dy * o) * k).clamp(0.0, lvl.h as f32 - 1.001));
            let p = sample_bilinear(lvl, sx, sy);
            for c in 0..3 {
                acc[c] += p[c];
            }
            n += 1.0;
        }
        [acc[0] / n, acc[1] / n, acc[2] / n]
    };
    let n = w * h;
    let mut v = vec![[0.0f32; 3]; n];
    let mut fixed = vec![false; n];
    let mut sum = [0.0f32; 3];
    let mut cnt = 0.0f32;
    for gy in 0..h {
        for gx in 0..w {
            let (x, y) = (x0 + (gx as f32 + 0.5) * cell, y0 + (gy as f32 + 0.5) * cell);
            let d = path_dist(nodes, x, y);
            // Inside the radius is unknown; outside is the boundary value (target/source ratio).
            if d >= r * HEAL_BAND || gx == 0 || gy == 0 || gx == w - 1 || gy == h - 1 {
                let a = sample_avg(x, y);
                let b = sample_avg(x + ox, y + oy);
                let mut l = [0.0f32; 3];
                for c in 0..3 {
                    l[c] = (a[c].max(1e-5) / b[c].max(1e-5)).clamp(0.1, 10.0).ln();
                }
                v[gy * w + gx] = l;
                fixed[gy * w + gx] = true;
                if d < r * 1.6 {
                    for c in 0..3 {
                        sum[c] += l[c];
                    }
                    cnt += 1.0;
                }
            }
        }
    }
    let mean = if cnt > 0.0 { [sum[0] / cnt, sum[1] / cnt, sum[2] / cnt] } else { [0.0; 3] };
    for i in 0..n {
        if !fixed[i] {
            v[i] = mean;
        }
    }
    // SOR (successive over-relaxation Gauss-Seidel)
    let omega = 1.85f32;
    let iters = (w.max(h) * 3).clamp(60, 600);
    for _ in 0..iters {
        let mut delta = 0.0f32;
        for gy in 1..h - 1 {
            for gx in 1..w - 1 {
                let i = gy * w + gx;
                if fixed[i] {
                    continue;
                }
                for c in 0..3 {
                    let avg = 0.25 * (v[i - 1][c] + v[i + 1][c] + v[i - w][c] + v[i + w][c]);
                    let nv = v[i][c] + omega * (avg - v[i][c]);
                    delta = delta.max((nv - v[i][c]).abs());
                    v[i][c] = nv;
                }
            }
        }
        if delta < 1e-4 {
            break;
        }
    }
    HealGrid { x0, y0, cell, w, h, v }
}

impl Engine {
    fn prepare_spots_cached(&mut self, spots: &[super::settings::Spot], src: &SourceImage, bw: f32, bh: f32) -> Vec<Arc<PreparedSpot>> {
        let src_key = (Arc::as_ptr(&src.levels[0]) as usize, src.width(), src.height());
        let mut used = std::collections::HashSet::new();
        let out: Vec<Arc<PreparedSpot>> = spots
            .iter()
            .map(|sp| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                src_key.hash(&mut h);
                serde_json::to_string(sp).unwrap_or_default().hash(&mut h);
                let key = h.finish();
                used.insert(key);
                self.spots.entry(key).or_insert_with(|| Arc::new(prepare_spots(std::slice::from_ref(sp), src, bw, bh).pop().unwrap())).clone()
            })
            .collect();
        self.spots.retain(|k, _| used.contains(k));
        out
    }
}

fn prepare_spots(spots: &[super::settings::Spot], src: &SourceImage, bw: f32, bh: f32) -> Vec<PreparedSpot> {
    if spots.is_empty() {
        return Vec::new();
    }
    let long = bw.max(bh);
    // Boundary color comes from a mid-resolution level (long edge >= 1500px so small spots register).
    let lvl = src.levels.iter().rev().find(|l| l.w.max(l.h) >= 1500).unwrap_or(&src.levels[0]).clone();
    let k = lvl.w as f32 / bw;
    spots
        .iter()
        .map(|s| {
            let r = (s.radius * long).max(1.0);
            let (dx, dy, sx, sy) = (s.dst[0] * bw, s.dst[1] * bh, s.src[0] * bw, s.src[1] * bh);
            let (ox, oy) = (sx - dx, sy - dy);
            let path: Vec<[f32; 2]> = if s.path.len() >= 2 { s.path.iter().map(|p| [p[0] * bw, p[1] * bh]).collect() } else { vec![[dx, dy]] };
            let nodes = if path.len() >= 2 { resample_path(&path, r * 0.5, 400) } else { path };
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in &nodes {
                x0 = x0.min(p[0] - r);
                y0 = y0.min(p[1] - r);
                x1 = x1.max(p[0] + r);
                y1 = y1.max(p[1] + r);
            }
            let bbox = [x0, y0, x1, y1];
            let heal = if s.heal {
                let m = r * 0.25 + 2.0;
                Some(build_heal_grid(&lvl, k, &nodes, ox, oy, r, [x0 - m, y0 - m, x1 + m, y1 + m]))
            } else {
                None
            };
            let fill = if s.remove && !s.needs_fill() {
                s.fill.as_ref().and_then(|f| super::aistore::get_rgb(&f.key, f.scale).map(|p| (p, [f.bbox[0] * bw, f.bbox[1] * bh, f.bbox[2] * bw, f.bbox[3] * bh])))
            } else {
                None
            };
            PreparedSpot { nodes, ox, oy, r, feather: s.feather.clamp(0.0, 1.0).max(0.02), opacity: s.opacity.clamp(0.0, 1.0), heal, bbox, fill }
        })
        .collect()
}

/// Auto source offset for brush spots: direction and distance where moving the whole path best matches surrounding texture (normalized offset).
pub fn auto_stroke_offset(src: &SourceImage, path: &[[f32; 2]], radius: f32) -> [f32; 2] {
    let lvl = src.levels.last().unwrap();
    let (w, h) = (lvl.w as f32, lvl.h as f32);
    let r = (radius * w.max(h)).max(1.5);
    let pts: Vec<[f32; 2]> = path.iter().map(|p| [p[0] * w, p[1] * h]).collect();
    let pts = resample_path(&pts, r, 24);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let extent = (x1 - x0).max(y1 - y0) * 0.5 + r;
    let ring = |cx: f32, cy: f32| -> Option<Vec<[f32; 3]>> {
        let mut v = Vec::with_capacity(16);
        for k in 0..8 {
            let a = k as f32 / 8.0 * std::f32::consts::TAU;
            for rr in [0.6f32, 1.3] {
                let (px, py) = (cx + a.cos() * r * rr, cy + a.sin() * r * rr);
                if px < 1.0 || py < 1.0 || px >= w - 2.0 || py >= h - 2.0 {
                    return None;
                }
                v.push(sample_bilinear(lvl, px, py));
            }
        }
        Some(v)
    };
    let targets: Vec<Option<Vec<[f32; 3]>>> = pts.iter().map(|p| ring(p[0], p[1])).collect();
    // Default: one path width to the right (or left).
    let mut best = (f32::MAX, [if x1 + extent * 2.2 < w { extent * 2.2 } else { -extent * 2.2 }, 0.0]);
    for dist_k in [1.3f32, 1.8, 2.5, 3.4] {
        for k in 0..24 {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            let (ox, oy) = (a.cos() * extent * dist_k, a.sin() * extent * dist_k);
            let mut e = 0.0f32;
            let mut ok = true;
            for (p, t) in pts.iter().zip(&targets) {
                let Some(t) = t else { continue };
                let Some(c) = ring(p[0] + ox, p[1] + oy) else {
                    ok = false;
                    break;
                };
                for (pp, qq) in t.iter().zip(&c) {
                    for ch in 0..3 {
                        let d = pp[ch].max(1e-4).ln() - qq[ch].max(1e-4).ln();
                        e += d * d;
                    }
                }
            }
            if !ok {
                continue;
            }
            e *= 1.0 + 0.05 * dist_k;
            if e < best.0 {
                best = (e, [ox, oy]);
            }
        }
    }
    [best.1[0] / w, best.1[1] / h]
}

/// Auto source position for spots: the nearby position most similar to the ring around the target (smallest source level).
pub fn auto_spot_source(src: &SourceImage, dst: [f32; 2], radius: f32) -> [f32; 2] {
    let lvl = src.levels.last().unwrap();
    let (w, h) = (lvl.w as f32, lvl.h as f32);
    let r = (radius * w.max(h)).max(1.5);
    let (cx, cy) = (dst[0] * w, dst[1] * h);
    let ring = |x: f32, y: f32| -> Option<Vec<[f32; 3]>> {
        let mut v = Vec::with_capacity(48);
        for k in 0..24 {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            for rr in [0.5f32, 1.3] {
                let px = x + a.cos() * r * rr;
                let py = y + a.sin() * r * rr;
                if px < 1.0 || py < 1.0 || px >= w - 2.0 || py >= h - 2.0 {
                    return None;
                }
                v.push(sample_bilinear(lvl, px, py));
            }
        }
        Some(v)
    };
    let Some(target) = ring(cx, cy) else { return [(dst[0] + radius * 2.5).min(0.98), dst[1]] };
    let mut best = (f32::MAX, [(dst[0] + radius * 2.5).min(0.98), dst[1]]);
    for dist_k in [1.8f32, 2.4, 3.2, 4.2] {
        for k in 0..24 {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            let (x, y) = (cx + a.cos() * r * dist_k, cy + a.sin() * r * dist_k);
            let Some(c) = ring(x, y) else { continue };
            // Healing corrects brightness differences, so compare mainly the variance of log differences (texture).
            let mut e = 0.0f32;
            for (p, q) in target.iter().zip(&c) {
                for ch in 0..3 {
                    let d = (p[ch].max(1e-4)).ln() - (q[ch].max(1e-4)).ln();
                    e += d * d;
                }
            }
            e *= 1.0 + 0.05 * dist_k;
            if e < best.0 {
                best = (e, [x / w, y / h]);
            }
        }
    }
    best.1
}

/// Auto level: finds the tilt from the horizontal/vertical axes in the orientation histogram of strong edges; returns the correction angle (degrees, + = clockwise).
pub fn auto_straighten(src: &SourceImage) -> Option<f32> {
    let lvl = src.levels.iter().rev().find(|l| l.w.max(l.h) >= 900).unwrap_or(src.levels.last().unwrap());
    let (w, h) = (lvl.w, lvl.h);
    if w < 16 || h < 16 {
        return None;
    }
    let l: Vec<f32> = lvl.data.as_chunks::<3>().0.iter().map(|p| luma(p[0], p[1], p[2]).max(1e-4).log2()).collect();
    // Slight blur so pixel stair-step edges don't register as axis-aligned.
    let l = filters::gauss_blur(&l, w, h, 1.6);
    let mut grads = Vec::with_capacity(w * h / 2);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let at = |dx: isize, dy: isize| l[((y as isize + dy) as usize) * w + (x as isize + dx) as usize];
            let gx = (at(1, -1) + 2.0 * at(1, 0) + at(1, 1)) - (at(-1, -1) + 2.0 * at(-1, 0) + at(-1, 1));
            let gy = (at(-1, 1) + 2.0 * at(0, 1) + at(1, 1)) - (at(-1, -1) + 2.0 * at(0, -1) + at(1, -1));
            let m = (gx * gx + gy * gy).sqrt();
            if m > 0.0 {
                grads.push((m, gx, gy));
            }
        }
    }
    if grads.len() < 100 {
        return None;
    }
    let mut mags: Vec<f32> = grads.iter().map(|g| g.0).collect();
    let k = mags.len() * 9 / 10;
    let thr = *mags.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1;
    // 0.1 degree steps, +-15 degrees
    const N: usize = 301;
    let mut hist = [0.0f32; N];
    for (m, gx, gy) in grads {
        if m < thr {
            continue;
        }
        let line = gy.atan2(gx).to_degrees() + 90.0;
        let dev = (line + 45.0).rem_euclid(90.0) - 45.0;
        if dev.abs() > 15.0 {
            continue;
        }
        let b = ((dev + 15.0) * 10.0).round() as usize;
        hist[b.min(N - 1)] += m;
    }
    // Smooth to find the peak.
    let mut sm = [0.0f32; N];
    for i in 0..N {
        let mut acc = 0.0;
        for d in -5i32..=5 {
            let j = i as i32 + d;
            if j >= 0 && (j as usize) < N {
                acc += hist[j as usize] * (1.0 - d.abs() as f32 / 6.0);
            }
        }
        sm[i] = acc;
    }
    let total: f32 = sm.iter().sum();
    let (bi, bv) = sm.iter().enumerate().fold((0, 0.0f32), |a, (i, v)| if *v > a.1 { (i, *v) } else { a });
    if total <= 0.0 || bv / total < 0.012 {
        return None;
    }
    let dev = bi as f32 / 10.0 - 15.0;
    if std::env::var_os("DARKROOM_DEBUG_STRAIGHT").is_some() {
        eprintln!("straighten: peak {dev} frac {:.4} level {}x{}", bv / total, w, h);
    }
    if dev.abs() < 0.15 {
        return Some(0.0);
    }
    Some(-dev)
}

/// Auto perspective: from strong edges, jointly finds angle, vertical and horizontal perspective so verticals become vertical
/// and horizontals horizontal. Returns (angle in degrees, vertical, horizontal) in Geometry units.
/// `vertical_only`: level and vertical perspective only (for photos with intended horizontal perspective, e.g. building facades).
pub fn auto_upright(src: &SourceImage, base: &Geometry, vertical_only: bool) -> Option<(f32, f32, f32)> {
    let lvl = src.levels.iter().rev().find(|l| l.w.max(l.h) >= 900).unwrap_or(src.levels.last().unwrap());
    let (w, h) = (lvl.w, lvl.h);
    if w < 16 || h < 16 {
        return None;
    }
    let (bw, bh) = (src.width(), src.height());
    let k = bw as f32 / w as f32;
    let l: Vec<f32> = lvl.data.as_chunks::<3>().0.iter().map(|p| luma(p[0], p[1], p[2]).max(1e-4).log2()).collect();
    let l = filters::gauss_blur(&l, w, h, 1.6);
    // (position in source coordinates, unit line direction, strength)
    let mut edges: Vec<([f32; 2], [f32; 2], f32)> = Vec::new();
    let mut mags = Vec::new();
    for y in (2..h - 2).step_by(2) {
        for x in (2..w - 2).step_by(2) {
            let at = |dx: isize, dy: isize| l[((y as isize + dy) as usize) * w + (x as isize + dx) as usize];
            let gx = (at(1, -1) + 2.0 * at(1, 0) + at(1, 1)) - (at(-1, -1) + 2.0 * at(-1, 0) + at(-1, 1));
            let gy = (at(-1, 1) + 2.0 * at(0, 1) + at(1, 1)) - (at(-1, -1) + 2.0 * at(0, -1) + at(1, -1));
            let m = (gx * gx + gy * gy).sqrt();
            if m > 0.0 {
                // Line direction is perpendicular to the gradient.
                edges.push(([x as f32 * k, y as f32 * k], [-gy / m, gx / m], m));
                mags.push(m);
            }
        }
    }
    if edges.len() < 200 {
        return None;
    }
    let kth = mags.len() * 92 / 100;
    let thr = *mags.select_nth_unstable_by(kth, |a, b| a.total_cmp(b)).1;
    let mut edges: Vec<_> = edges.into_iter().filter(|e| e.2 >= thr).collect();
    // Limit the amount of work.
    if edges.len() > 6000 {
        let step = edges.len() / 6000 + 1;
        edges = edges.into_iter().step_by(step).collect();
    }
    let lens = super::settings::LensCorrection::default();
    let cost = |ang: f32, v: f32, hz: f32| -> f32 {
        let mut g = base.clone();
        g.crop = [0.0, 0.0, 1.0, 1.0];
        g.angle = ang;
        g.vertical = v;
        g.horizontal = hz;
        g.scale = 100.0;
        let (fw, fh) = super::geometry::frame_dims(bw, bh, &g);
        let gm = super::geometry::GeoMap::new(bw, bh, &g, &lens, [0.0, 0.0, 1.0, 1.0], fw, fh);
        let mut c = 0.0f32;
        for (p, d, m) in &edges {
            let o = gm.inverse(p[0], p[1]);
            // Map source direction to output direction via the local output-to-source Jacobian.
            let (mx, my) = gm.map(o.0, o.1);
            let (ax, ay) = gm.map(o.0 + 1.0, o.1);
            let (bx2, by2) = gm.map(o.0, o.1 + 1.0);
            let (j00, j10, j01, j11) = (ax - mx, ay - my, bx2 - mx, by2 - my);
            let det = j00 * j11 - j01 * j10;
            if det.abs() < 1e-9 {
                continue;
            }
            let fx = (j11 * d[0] - j01 * d[1]) / det;
            let fy = (-j10 * d[0] + j00 * d[1]) / det;
            let deg = fy.atan2(fx).to_degrees();
            // Difference from the nearest axis (-45..45); 0 = horizontal, 90 = vertical.
            let dev = (deg + 45.0).rem_euclid(90.0) - 45.0;
            let near_vertical = ((deg.rem_euclid(180.0)) - 90.0).abs() < 45.0;
            // Maximize the amount of exactly axis-aligned lines (Gaussian kernel); axis-independent edges like patterns or fur stay near 0.
            let wgt = if near_vertical { 1.0 } else { 0.6 };
            let q = dev / 1.5;
            c -= m * wgt * (-q * q).exp();
        }
        // Penalize strong corrections (prefer the lighter option).
        c * (1.0 - 0.00002 * (v * v + hz * hz) - 0.002 * ang * ang)
    };
    let golden = |f: &dyn Fn(f32) -> f32, lo: f32, hi: f32, iters: usize| -> f32 {
        let gr = 0.618_034f32;
        let (mut a, mut b) = (lo, hi);
        let mut c1 = b - gr * (b - a);
        let mut c2 = a + gr * (b - a);
        let (mut f1, mut f2) = (f(c1), f(c2));
        for _ in 0..iters {
            if f1 < f2 {
                b = c2;
                c2 = c1;
                f2 = f1;
                c1 = b - gr * (b - a);
                f1 = f(c1);
            } else {
                a = c1;
                c1 = c2;
                f1 = f2;
                c2 = a + gr * (b - a);
                f2 = f(c2);
            }
        }
        0.5 * (a + b)
    };
    // Coarse grid over angle x vertical perspective (may not be unimodal), in parallel.
    let grid: Vec<(f32, f32)> = (-8..=8).flat_map(|vi| (-6..=6).map(move |ai| (ai as f32, vi as f32 * 10.0))).collect();
    let best = grid
        .par_iter()
        .map(|&(a, vv)| (cost(a, vv, 0.0), a, vv))
        .reduce(|| (f32::MAX, 0.0, 0.0), |x, y| if y.0 < x.0 { y } else { x });
    let (mut ang, mut v, mut hz) = (best.1, best.2, 0.0f32);
    for _ in 0..3 {
        v = golden(&|x| cost(ang, x, hz), (v - 12.0).max(-100.0), (v + 12.0).min(100.0), 12);
        ang = golden(&|x| cost(x, v, hz), ang - 1.5, ang + 1.5, 12);
        if !vertical_only {
            hz = golden(&|x| cost(ang, v, x), (hz - 30.0).max(-100.0), (hz + 30.0).min(100.0), 12);
        }
    }
    let base_cost = cost(0.0, 0.0, 0.0);
    let new_cost = cost(ang, v, hz);
    if std::env::var_os("DARKROOM_DEBUG_STRAIGHT").is_some() {
        eprintln!("upright: ang {ang:.2} v {v:.1} h {hz:.1} cost {base_cost:.1} -> {new_cost:.1}");
    }
    // Apply perspective only when it clearly helps (enough axis-aligned lines); otherwise still fix the tilt.
    // (Cost is negative: amount of axis-aligned lines.) Perspective needs a 20%+ gain; otherwise level only.
    if new_cost >= base_cost * 1.2 {
        let a = auto_straighten(src).unwrap_or(0.0);
        if a != 0.0 && cost(a, 0.0, 0.0) < base_cost * 1.1 {
            return Some(((a * 10.0).round() / 10.0, 0.0, 0.0));
        }
        return Some((0.0, 0.0, 0.0));
    }
    let r = |x: f32| (x * 10.0).round() / 10.0;
    Some((r(ang), r(v).round(), r(hz).round()))
}

/// Isotonic regression (pool adjacent violators): least-squares monotone increasing sequence.
fn isotonic(v: &[f32]) -> Vec<f32> {
    // Stack of (sum, count) blocks
    let mut blocks: Vec<(f64, usize)> = Vec::with_capacity(v.len());
    for &x in v {
        blocks.push((x as f64, 1));
        while blocks.len() >= 2 {
            let (s1, n1) = blocks[blocks.len() - 1];
            let (s0, n0) = blocks[blocks.len() - 2];
            if s0 / n0 as f64 <= s1 / n1 as f64 {
                break;
            }
            blocks.pop();
            let last = blocks.len() - 1;
            blocks[last] = (s0 + s1, n0 + n1);
        }
    }
    // Block means produce flat runs (banding), so connect block centers with a polyline (still monotone).
    // Anchors: untouched points (single blocks) and both ends; merged runs are bridged by straight lines between anchors.
    let mut knots: Vec<(f64, f64)> = Vec::with_capacity(blocks.len());
    let mut start = 0usize;
    let len = v.len();
    for (s, n) in &blocks {
        let m = s / *n as f64;
        if *n == 1 {
            knots.push((start as f64, m));
        } else if start == 0 {
            knots.push((0.0, m));
        } else if start + n == len {
            knots.push(((len - 1) as f64, m));
        }
        start += n;
    }
    let mut out = Vec::with_capacity(v.len());
    let mut k = 0usize;
    for i in 0..v.len() {
        let x = i as f64;
        while k + 1 < knots.len() && knots[k + 1].0 <= x {
            k += 1;
        }
        let y = if k + 1 >= knots.len() || x <= knots[k].0 {
            knots[k].1
        } else {
            let (x0, y0) = knots[k];
            let (x1, y1) = knots[k + 1];
            y0 + (y1 - y0) * (x - x0) / (x1 - x0)
        };
        out.push(y as f32);
    }
    // Flat runs at both ends stay within the original value range (black/white fixed points).
    out
}

#[cfg(test)]
mod isotonic_tests {
    #[test]
    fn isotonic_fixes_dips() {
        let r = super::isotonic(&[0.0, 0.3, 0.2, 0.4, 0.1, 0.9]);
        assert!(r.windows(2).all(|w| w[0] <= w[1]));
        // Increasing with no flat runs (except at the ends).
        let r = super::isotonic(&[0.0, 0.1, 0.5, 0.3, 0.6, 0.8, 0.7, 0.9, 1.0]);
        assert!(r.windows(2).all(|w| w[0] <= w[1]));
        assert!(r[1..r.len() - 1].windows(2).all(|w| w[0] < w[1]), "{r:?}");
    }
}

#[cfg(test)]
mod upright_tests {
    use super::*;

    /// An image that becomes straight verticals after +40 vertical correction; auto perspective should find +40.
    #[test]
    fn upright_recovers_keystone() {
        let (w, h) = (900usize, 1200usize);
        // Source (straight verticals)
        let straight = |x: f32, _y: f32| -> f32 { 0.25 + 0.2 * (x * 0.07).sin() };
        // Image seen with keystone v=+40 = source sampled through the inverse of frame_to_base.
        let mut g = Geometry::default();
        g.vertical = 40.0;
        let lens = crate::develop::settings::LensCorrection::default();
        let gm = crate::develop::geometry::GeoMap::new(w, h, &g, &lens, [0.0, 0.0, 1.0, 1.0], w, h);
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                // Pixel (x,y) of the tilted photo is at the inverse position in the straight scene.
                let (sx, sy) = gm.inverse(x as f32 + 0.5, y as f32 + 0.5);
                let v = straight(sx, sy);
                let i = (y * w + x) * 3;
                data[i] = v;
                data[i + 1] = v;
                data[i + 2] = v;
            }
        }
        let src = SourceImage::new(LinearImage { w, h, data }, false, 256);
        let (_a, v, _hz) = auto_upright(&src, &Geometry::default(), true).expect("결과");
        // Value that undoes the applied distortion (opposite sign, similar magnitude).
        // Render reads source map(o) at output o, so a source of straight(inverse(b)) becomes straight at v=+40.
        assert!((v - 40.0).abs() < 5.0, "v = {v}");
        assert!(_a.abs() < 1.0, "angle = {_a}");
    }
}

#[cfg(test)]
mod upright_probe {
    /// Test switch: DARKROOM_UPRIGHT_DIR=<folder> prints auto perspective results and timing for each RAW in the folder.
    #[test]
    #[ignore]
    fn upright_probe() {
        let Some(dir) = std::env::var_os("DARKROOM_UPRIGHT_DIR") else { return };
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if !crate::config::is_raw_ext(p.extension().and_then(|x| x.to_str()).unwrap_or("")) {
                continue;
            }
            let o = crate::imaging::meta::read_raw_meta(&p).map(|m| m.orientation).unwrap_or(1);
            let Ok(src) = crate::imaging::decode::decode_source(&p, o) else { continue };
            let t = std::time::Instant::now();
            let r = super::auto_upright(&src, &Default::default(), false);
            println!("{} → {:?} ({:?})", p.display(), r, t.elapsed());
            if let (Some(out), Some((a, v, hz))) = (std::env::var_os("DARKROOM_UPRIGHT_OUT"), r) {
                let mut e = super::Engine::default();
                let mut imgs = Vec::new();
                for corr in [false, true] {
                    let mut s = crate::develop::settings::DevelopSettings::default_for(true);
                    if corr {
                        s.geometry.angle = a;
                        s.geometry.vertical = v;
                        s.geometry.horizontal = hz;
                    }
                    let (bw, bh) = (src.width() as f32, src.height() as f32);
                    let k = 600.0 / bw.max(bh);
                    let (w, h) = ((bw * k) as usize, (bh * k) as usize);
                    let r = e.render(&src, &super::RenderRequest { settings: &s, out_w: w, out_h: h, region: [0.0, 0.0, 1.0, 1.0], draft: false, clipping: false, mask_overlay: None, keep_float: false });
                    imgs.push(r);
                }
                let (w, h) = (imgs[0].w, imgs[0].h);
                let mut canvas = image::RgbaImage::new((w * 2) as u32, h as u32);
                for (i, r) in imgs.iter().enumerate() {
                    let im = image::RgbaImage::from_raw(r.w as u32, r.h as u32, r.rgba.clone()).unwrap();
                    image::imageops::overlay(&mut canvas, &im, (i * w) as i64, 0);
                }
                let name = p.file_stem().unwrap().to_string_lossy().to_string();
                canvas.save(std::path::Path::new(&out).join(format!("up_{name}.png"))).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod spot_tests {
    use super::*;

    /// Heal a point on a horizontal brightness gradient from a different brightness area; error vs. the ideal background must be small (no seam).
    #[test]
    fn heal_matches_gradient() {
        let (w, h) = (800usize, 600usize);
        let bg = |x: f32, y: f32| 0.15 + 0.6 * x / w as f32 + 0.1 * y / h as f32;
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let mut v = bg(x as f32, y as f32);
                let (dx, dy) = (x as f32 - 400.0, y as f32 - 300.0);
                if dx * dx + dy * dy < 15.0 * 15.0 {
                    v *= 0.2; // spot
                }
                let i = (y * w + x) * 3;
                data[i] = v;
                data[i + 1] = v * 0.9;
                data[i + 2] = v * 0.8;
            }
        }
        let src = SourceImage::new(LinearImage { w, h, data }, false, 256);
        let run = |spot: crate::develop::settings::Spot| -> f32 {
            let sp = prepare_spots(&[spot], &src, w as f32, h as f32);
            let img = &src.levels[0];
            let mut worst = 0.0f32;
            for y in 250..350 {
                for x in 350..450 {
                    let (bx, by) = (x as f32 + 0.5, y as f32 + 0.5);
                    let mut p = sample_bilinear(img, bx, by);
                    if let Some((wt, sx, sy, g)) = sp[0].eval(bx, by) {
                        let q = sample_bilinear(img, sx, sy);
                        for c in 0..3 {
                            p[c] = p[c] * (1.0 - wt) + q[c] * g[c] * wt;
                        }
                    }
                    let ideal = bg(bx, by);
                    worst = worst.max((p[0] / ideal - 1.0).abs());
                }
            }
            worst
        };
        let r = 30.0 / 800.0;
        // Circular spot: source taken from the right (brighter side).
        let circle = crate::develop::settings::Spot { dst: [0.5, 0.5], src: [0.5 + 0.12, 0.5 + 0.05], radius: r, feather: 0.3, ..Default::default() };
        let e1 = run(circle);
        // Brush spot: short stroke across the point.
        let stroke = crate::develop::settings::Spot {
            dst: [0.48, 0.5],
            src: [0.48 + 0.1, 0.5 - 0.08],
            radius: r,
            feather: 0.3,
            path: vec![[0.48, 0.5], [0.5, 0.5], [0.52, 0.5]],
            ..Default::default()
        };
        let e2 = run(stroke);
        println!("heal worst rel error: circle {e1:.4} stroke {e2:.4}");
        assert!(e1 < 0.03, "circle {e1}");
        assert!(e2 < 0.03, "stroke {e2}");
    }
}

#[cfg(test)]
mod heal_blotch_tests {
    use super::*;

    /// Measures low-frequency error (blotches) after healing on a skin-like textured background (pores, mottling).
    #[test]
    fn heal_blotch_probe() {
        let (w, h) = (900usize, 700usize);
        let hash = |x: usize, y: usize| -> f32 {
            let mut v = (x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663);
            v ^= v >> 13;
            v = v.wrapping_mul(0x5bd1e995);
            v ^= v >> 15;
            (v & 0xffff) as f32 / 65535.0 - 0.5
        };
        let fine: Vec<f32> = (0..w * h).map(|i| hash(i % w, i / w)).collect();
        let fine = filters::gauss_blur(&fine, w, h, 1.2);
        let mid: Vec<f32> = (0..w * h).map(|i| hash(i % w + 7777, i / w + 333)).collect();
        let mid = filters::gauss_blur(&mid, w, h, 7.0);
        let bgv = |x: usize, y: usize| -> f32 {
            let g = 0.25 + 0.35 * x as f32 / w as f32 + 0.15 * (y as f32 / h as f32 * 3.0).sin();
            g * (1.0 + 0.9 * fine[y * w + x] + 3.0 * mid[y * w + x])
        };
        let mut data = vec![0.0f32; w * h * 3];
        let mut ideal = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let b = bgv(x, y);
                ideal[y * w + x] = b;
                let (dx, dy) = (x as f32 - 450.0, y as f32 - 350.0);
                let d = (dx * dx + dy * dy).sqrt();
                let blem = 1.0 - 0.55 * (1.0 - smoothstep(10.0, 14.0, d));
                let v = b * blem;
                let i = (y * w + x) * 3;
                data[i] = v;
                data[i + 1] = v * 0.82;
                data[i + 2] = v * 0.7;
            }
        }
        let src = SourceImage::new(LinearImage { w, h, data }, false, 256);
        let lowpass = |v: &[f32]| filters::gauss_blur(v, w, h, 4.0);
        let ideal_lp = lowpass(&ideal);
        {
            for (feather, rad) in [(0.12f32, 22.0f32), (0.3, 22.0), (0.12, 16.0), (0.3, 16.0)] {
                let spot = crate::develop::settings::Spot { dst: [0.5, 0.5], src: [0.5 + 0.09, 0.5 - 0.05], radius: rad / 900.0, feather, ..Default::default() };
                let sp = prepare_spots(&[spot], &src, w as f32, h as f32);
                let img = &src.levels[0];
                let mut out = vec![0.0f32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        let (bx, by) = (x as f32 + 0.5, y as f32 + 0.5);
                        let mut p = sample_bilinear(img, bx, by);
                        if let Some((wt, sx, sy, g)) = sp[0].eval(bx, by) {
                            let q = sample_bilinear(img, sx, sy);
                            p[0] = p[0] * (1.0 - wt) + q[0] * g[0] * wt;
                        }
                        out[y * w + x] = p[0];
                    }
                }
                let out_lp = lowpass(&out);
                let (mut worst, mut sum, mut n) = (0.0f32, 0.0f32, 0.0f32);
                for y in 310..390 {
                    for x in 410..490 {
                        let e = (out_lp[y * w + x] / ideal_lp[y * w + x] - 1.0).abs();
                        worst = worst.max(e);
                        sum += e;
                        n += 1.0;
                    }
                }
                println!("feather {feather:.2} r {rad}: blotch mean {:.4} worst {:.4}", sum / n, worst);
                assert!(sum / n < 0.008, "얼룩 {}", sum / n);
            }
        }
    }
}

#[cfg(test)]
mod spot_visual {
    /// Test switch: DARKROOM_SPOT_RAW=<raw> DARKROOM_SPOT_OUT=<png> renders before/after crops of circle and brush spots on a real photo.
    #[test]
    #[ignore]
    fn spot_visual_probe() {
        let (Some(raw), Some(out)) = (std::env::var_os("DARKROOM_SPOT_RAW"), std::env::var_os("DARKROOM_SPOT_OUT")) else { return };
        let raw = std::path::PathBuf::from(raw);
        let o = crate::imaging::meta::read_raw_meta(&raw).map(|m| m.orientation).unwrap_or(1);
        let src = crate::imaging::decode::decode_source(&raw, o).unwrap();
        let cx: f32 = std::env::var("DARKROOM_SPOT_X").ok().and_then(|v| v.parse().ok()).unwrap_or(0.45);
        let cy: f32 = std::env::var("DARKROOM_SPOT_Y").ok().and_then(|v| v.parse().ok()).unwrap_or(0.45);
        let mut s = crate::develop::settings::DevelopSettings::default_for(true);
        let r = 0.012f32;
        let mk = |dst: [f32; 2], path: Vec<[f32; 2]>| {
            let mut sp = crate::develop::settings::Spot { dst, radius: r, feather: 0.25, path, ..Default::default() };
            let off = if sp.path.len() >= 2 {
                super::auto_stroke_offset(&src, &sp.path, r)
            } else {
                let q = super::auto_spot_source(&src, dst, r);
                [q[0] - dst[0], q[1] - dst[1]]
            };
            sp.src = [dst[0] + off[0], dst[1] + off[1]];
            sp
        };
        s.spots.push(mk([cx, cy], vec![]));
        let p0 = [cx + 0.03, cy - 0.02];
        s.spots.push(mk(p0, vec![p0, [cx + 0.05, cy], [cx + 0.035, cy + 0.015], [cx + 0.06, cy + 0.03]]));
        let region = [cx - 0.04, cy - 0.04, cx + 0.1, cy + 0.07];
        let mut e = super::Engine::default();
        let (w, h) = (700usize, 550usize);
        let mut canvas = image::RgbaImage::new((w * 2) as u32, h as u32);
        for (k, with) in [false, true].into_iter().enumerate() {
            let mut ss = s.clone();
            if !with {
                ss.spots.clear();
            }
            let r = e.render(&src, &super::RenderRequest { settings: &ss, out_w: w, out_h: h, region, draft: false, clipping: false, mask_overlay: None, keep_float: false });
            let im = image::RgbaImage::from_raw(r.w as u32, r.h as u32, r.rgba).unwrap();
            image::imageops::overlay(&mut canvas, &im, (k * w) as i64, 0);
        }
        canvas.save(std::path::PathBuf::from(out)).unwrap();
    }
}

#[cfg(test)]
mod spot_ghost_tests {
    use super::*;

    /// Red-eye removal: the red pupil turns dark and neutral; surrounding skin and outside the region stay.
    #[test]
    fn red_eye_fixes_pupil_only() {
        let (w, h) = (64usize, 64usize);
        let mut rgb = vec![0.0f32; w * h * 3];
        let mut uv = vec![[0.0f32; 2]; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                uv[i] = [x as f32 + 0.5, y as f32 + 0.5];
                let d = ((x as f32 - 32.0).powi(2) + (y as f32 - 32.0).powi(2)).sqrt();
                let c = if d < 8.0 { [0.70, 0.10, 0.10] } else { [0.80, 0.55, 0.45] };
                rgb[i * 3..i * 3 + 3].copy_from_slice(&c);
            }
        }
        let orig = rgb.clone();
        let re = crate::develop::settings::RedEye { center: [0.5, 0.5], radius: [0.25, 0.25], ..Default::default() };
        apply_red_eye(&mut rgb, &uv, w, h, &[re], w as f32, h as f32);
        let px = |b: &[f32], x: usize, y: usize| [b[(y * w + x) * 3], b[(y * w + x) * 3 + 1], b[(y * w + x) * 3 + 2]];
        let c = px(&rgb, 32, 32);
        assert!(c[0] < 0.12 && (c[0] - c[1]).abs() < 0.03, "pupil {c:?}");
        // Skin (inside the region but less red) and outside the region stay unchanged.
        for (x, y) in [(32usize, 44usize), (20, 32), (2, 2), (60, 60)] {
            let (a, b) = (px(&orig, x, y), px(&rgb, x, y));
            assert!((a[0] - b[0]).abs() < 0.02, "skin changed at {x},{y}: {a:?} -> {b:?}");
        }
    }

    /// Removed text must not come back as a ghost through the local tone (highlights, shadows, clarity) base map.
    #[test]
    fn healed_text_leaves_no_ghost() {
        let (w, h) = (1200usize, 800usize);
        let make = |text: bool| {
            let mut data = vec![0.0f32; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    let mut v = 0.42 + 0.05 * (y as f32 / h as f32);
                    // Text shape: thick vertical bars between x 400..800, y 200..260.
                    if text && (400..800).contains(&x) && (200..260).contains(&y) && (x / 18) % 3 == 0 {
                        v = 0.9;
                    }
                    let i = (y * w + x) * 3;
                    data[i] = v;
                    data[i + 1] = v;
                    data[i + 2] = v;
                }
            }
            SourceImage::new(LinearImage { w, h, data }, false, 256)
        };
        let mut s = crate::develop::settings::DevelopSettings::default_for(false);
        s.highlights = -80.0;
        s.shadows = 60.0;
        s.clarity = 50.0;
        s.texture = 30.0;
        // Brush spot covering the text, source in the flat area below.
        s.spots.push(crate::develop::settings::Spot {
            dst: [380.0 / w as f32, 230.0 / h as f32],
            src: [380.0 / w as f32, 500.0 / h as f32],
            radius: 50.0 / w as f32,
            feather: 0.25,
            path: vec![[380.0 / w as f32, 230.0 / h as f32], [820.0 / w as f32, 230.0 / h as f32]],
            ..Default::default()
        });
        let render = |src: &SourceImage, s: &crate::develop::settings::DevelopSettings| {
            let mut e = Engine::default();
            e.render(src, &RenderRequest { settings: s, out_w: 600, out_h: 400, region: [0.0, 0.0, 1.0, 1.0], draft: false, clipping: false, mask_overlay: None, keep_float: false }).rgba
        };
        let a = render(&make(true), &s);
        let mut s2 = s.clone();
        s2.spots.clear();
        let b = render(&make(false), &s2);
        // Text location (output coords 200..400 x 100..130).
        let mut worst = 0i32;
        for y in 100..130 {
            for x in 200..400 {
                let i = (y * 600 + x) * 4;
                worst = worst.max((a[i] as i32 - b[i] as i32).abs());
            }
        }
        println!("ghost worst diff (8bit): {worst}");
        assert!(worst <= 6, "잔상 {worst}");
    }
}
