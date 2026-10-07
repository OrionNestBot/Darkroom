//! Non-destructive develop settings. Stored as JSON in the catalog; every field has a default
//! (`#[serde(default)]`) so older catalogs still open after new fields are added.

use serde::{Deserialize, Serialize};

pub const HSL_BANDS: usize = 8;
pub const HSL_BAND_NAMES: [&str; HSL_BANDS] = ["빨강", "주황", "노랑", "초록", "아쿠아", "파랑", "보라", "마젠타"];
/// Center hue of each band (degrees).
pub const HSL_BAND_HUES: [f32; HSL_BANDS] = [0.0, 30.0, 60.0, 120.0, 180.0, 225.0, 270.0, 315.0];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevelopSettings {
    pub version: u32,
    pub treatment: Treatment,
    /// Camera profile name (empty = default color profile). RAW only.
    pub profile: String,
    /// Profile amount (creative profiles, 0..200, default 100).
    pub profile_amount: f32,
    /// RAW white balance: false = as shot, true = use temp_k/tint_k.
    pub wb_custom: bool,
    /// RAW color temperature (Kelvin).
    pub temp_k: f32,
    /// RAW tint (-150..150).
    pub tint_k: f32,
    // Basic (non-RAW files use relative temp/tint, -100..100)
    pub temp: f32,
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
    pub curve: ToneCurve,
    pub hsl: [HslBand; HSL_BANDS],
    pub bw_mix: [f32; HSL_BANDS],
    pub grading: ColorGrading,
    pub detail: Detail,
    pub lens: LensCorrection,
    pub geometry: Geometry,
    pub effects: Effects,
    pub calibration: Calibration,
    pub masks: Vec<Mask>,
    /// Redaction regions (e.g. face mosaic).
    pub privacy: Vec<PrivacyRegion>,
    /// Spot removal (heal/clone).
    pub spots: Vec<Spot>,
    /// Red-eye removal.
    pub red_eye: Vec<RedEye>,
    /// Point color: shift hue/saturation/luminance of colors close to a picked color.
    pub point_colors: Vec<PointColor>,
    /// Display only: soft-proof preset index (not saved, 0 = off).
    #[serde(skip)]
    pub view_proof: u32,
}

/// Red-eye region (normalized coords after orientation; radius x relative to width, y to height).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedEye {
    pub center: [f32; 2],
    pub radius: [f32; 2],
    /// Pupil size 0..100 (larger also covers less-red pixels).
    pub pupil: f32,
    /// Darken amount 0..100.
    pub darken: f32,
}

impl Default for RedEye {
    fn default() -> Self {
        Self { center: [0.5, 0.5], radius: [0.02, 0.02], pupil: 50.0, darken: 50.0 }
    }
}

/// Spot removal in normalized source coords (after orientation). Radius is relative to the long side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spot {
    pub dst: [f32; 2],
    pub src: [f32; 2],
    pub radius: f32,
    /// 0..1 (edge softness)
    pub feather: f32,
    /// 0..1
    pub opacity: f32,
    /// true = heal (match surrounding brightness/color), false = clone.
    pub heal: bool,
    /// Brush spot: painted path (normalized coords, first point = dst). Empty means circular.
    pub path: Vec<[f32; 2]>,
    /// Remove (content fill): use a patch filled from the surroundings instead of the source.
    #[serde(default)]
    pub remove: bool,
    #[serde(default)]
    pub fill: Option<SpotFill>,
}

/// Remove result patch: catalog key, normalized source region, brightness gain, and the shape it was made for (refilled when the shape changes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct SpotFill {
    pub key: String,
    pub bbox: [f32; 4],
    pub scale: f32,
    pub sig: u64,
}

impl Default for Spot {
    fn default() -> Self {
        Self { dst: [0.5, 0.5], src: [0.55, 0.5], radius: 0.01, feather: 0.5, opacity: 1.0, heal: true, path: Vec::new(), remove: false, fill: None }
    }
}

impl Spot {
    /// Fingerprint of the painted shape (position, path, size), used to check whether the remove patch still fits.
    pub fn shape_sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in self.dst.iter().chain(self.path.iter().flatten()).chain(std::iter::once(&self.radius)) {
            v.to_bits().hash(&mut h);
        }
        h.finish()
    }

    /// A remove spot whose patch is missing or whose shape changed.
    pub fn needs_fill(&self) -> bool {
        self.remove && self.fill.as_ref().map(|f| f.sig != self.shape_sig()).unwrap_or(true)
    }

    /// Move the target (including its path).
    pub fn translate_dst(&mut self, dx: f32, dy: f32) {
        self.dst[0] += dx;
        self.dst[1] += dy;
        for p in &mut self.path {
            p[0] += dx;
            p[1] += dy;
        }
        self.src[0] += dx;
        self.src[1] += dy;
    }
}

/// Redaction style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PrivacyKind {
    #[default]
    Mosaic,
    Blur,
    Fill,
}

impl PrivacyKind {
    pub const ALL: [PrivacyKind; 3] = [PrivacyKind::Mosaic, PrivacyKind::Blur, PrivacyKind::Fill];
    pub fn name(self) -> &'static str {
        match self {
            PrivacyKind::Mosaic => tr!("모자이크", "Mosaic"),
            PrivacyKind::Blur => tr!("흐림", "Blur"),
            PrivacyKind::Fill => tr!("채우기", "Fill"),
        }
    }
}

/// Redaction region: ellipse/rectangle in normalized source coords (after orientation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyRegion {
    pub center: [f32; 2],
    /// Radius (x relative to width, y relative to height).
    pub radius: [f32; 2],
    pub ellipse: bool,
    pub kind: PrivacyKind,
    /// Strength 1..100 (mosaic cell size / blur amount).
    pub strength: f32,
    /// Region created by automatic detection.
    pub auto: bool,
}

impl Default for PrivacyRegion {
    fn default() -> Self {
        Self { center: [0.5, 0.5], radius: [0.08, 0.08], ellipse: true, kind: PrivacyKind::Mosaic, strength: 50.0, auto: false }
    }
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            version: 1,
            treatment: Treatment::Color,
            profile: String::new(),
            profile_amount: 100.0,
            wb_custom: false,
            temp_k: 5500.0,
            tint_k: 0.0,
            temp: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            curve: ToneCurve::default(),
            hsl: [HslBand::default(); HSL_BANDS],
            bw_mix: [0.0; HSL_BANDS],
            grading: ColorGrading::default(),
            detail: Detail::default(),
            lens: LensCorrection::default(),
            geometry: Geometry::default(),
            effects: Effects::default(),
            calibration: Calibration::default(),
            masks: Vec::new(),
            privacy: Vec::new(),
            red_eye: Vec::new(),
            point_colors: Vec::new(),
            view_proof: 0,
            spots: Vec::new(),
        }
    }
}

impl DevelopSettings {
    /// Defaults for the source format. RAW files get default sharpening and color noise reduction.
    pub fn default_for(is_raw: bool) -> Self {
        let mut s = Self::default();
        if is_raw {
            s.detail.sharpen_amount = 40.0;
            s.detail.nr_color = 25.0;
        }
        s
    }

    pub fn is_default_for(&self, is_raw: bool) -> bool {
        self.normalized() == Self::default_for(is_raw)
    }

    /// Normalize for comparison: with as-shot white balance the Kelvin/tint values are meaningless.
    fn normalized(&self) -> Self {
        let mut s = self.clone();
        if !s.wb_custom {
            let d = Self::default();
            s.temp_k = d.temp_k;
            s.tint_k = d.tint_k;
        }
        s
    }

    /// Edit summary for thumbnail badges.
    pub fn summary(&self, is_raw: bool) -> EditSummary {
        let s = self.normalized();
        let d = Self::default_for(is_raw);
        let mut g = s.geometry.clone();
        g.aspect = None;
        let crop = g != Geometry::default();
        let masks = !s.masks.is_empty() || !s.privacy.is_empty() || !s.spots.is_empty() || !s.red_eye.is_empty();
        let mut t = s;
        t.geometry = d.geometry.clone();
        t.masks.clear();
        t.privacy.clear();
        t.spots.clear();
        t.red_eye.clear();
        EditSummary { crop, masks, adjust: t != d }
    }

    /// Auto-sync: apply to `self` only the values that changed between `old` and `new`.
    pub fn apply_changes(&mut self, old: &DevelopSettings, new: &DevelopSettings) {
        let (Ok(o), Ok(n), Ok(mut me)) = (serde_json::to_value(old), serde_json::to_value(new), serde_json::to_value(&*self)) else { return };
        let mut changes = Vec::new();
        json_diff(&o, &n, &mut Vec::new(), &mut changes);
        if changes.is_empty() {
            return;
        }
        // White balance syncs as one group (changing temperature also syncs tint and mode)
        const WB: [&str; 5] = ["wb_custom", "temp_k", "tint_k", "temp", "tint"];
        if changes.iter().any(|(p, _)| matches!(p.first(), Some(Seg::Key(k)) if WB.contains(&k.as_str()))) {
            for k in WB {
                if let Some(v) = n.get(k) {
                    changes.push((vec![Seg::Key(k.to_string())], v.clone()));
                }
            }
        }
        for (path, v) in changes {
            json_set(&mut me, &path, v);
        }
        if let Ok(x) = serde_json::from_value::<DevelopSettings>(me) {
            *self = x;
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }

    /// Copy only the selected groups from `src` (paste settings / sync).
    pub fn copy_groups_from(&mut self, src: &DevelopSettings, g: &SettingGroups) {
        if g.white_balance {
            self.temp = src.temp;
            self.tint = src.tint;
            self.wb_custom = src.wb_custom;
            self.temp_k = src.temp_k;
            self.tint_k = src.tint_k;
        }

        if g.basic_tone {
            self.exposure = src.exposure;
            self.contrast = src.contrast;
            self.highlights = src.highlights;
            self.shadows = src.shadows;
            self.whites = src.whites;
            self.blacks = src.blacks;
        }
        if g.presence {
            self.texture = src.texture;
            self.clarity = src.clarity;
            self.dehaze = src.dehaze;
            self.vibrance = src.vibrance;
            self.saturation = src.saturation;
        }
        if g.treatment {
            self.profile = src.profile.clone();
            self.profile_amount = src.profile_amount;
            self.treatment = src.treatment;
            self.bw_mix = src.bw_mix;
        }
        if g.tone_curve {
            self.curve = src.curve.clone();
        }
        if g.hsl {
            self.hsl = src.hsl;
        }
        if g.point_color {
            self.point_colors = src.point_colors.clone();
        }
        if g.color_grading {
            self.grading = src.grading.clone();
        }
        if g.detail {
            self.detail = src.detail.clone();
        }
        if g.lens {
            self.lens = src.lens.clone();
        }
        if g.crop {
            self.geometry = src.geometry.clone();
        }
        if g.effects {
            self.effects = src.effects.clone();
        }
        if g.calibration {
            self.calibration = src.calibration.clone();
        }
        if g.masks {
            self.masks = src.masks.clone();
        }
        // Redaction regions differ per photo, so group copy skips them
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingGroups {
    pub white_balance: bool,
    pub basic_tone: bool,
    pub presence: bool,
    pub treatment: bool,
    pub tone_curve: bool,
    pub hsl: bool,
    pub color_grading: bool,
    pub detail: bool,
    pub lens: bool,
    pub crop: bool,
    pub effects: bool,
    pub calibration: bool,
    pub masks: bool,
    #[serde(default)]
    pub point_color: bool,
}

impl SettingGroups {
    pub fn all() -> Self {
        Self {
            white_balance: true,
            basic_tone: true,
            presence: true,
            treatment: true,
            tone_curve: true,
            hsl: true,
            color_grading: true,
            detail: true,
            lens: true,
            crop: false,
            effects: true,
            calibration: true,
            masks: false,
            point_color: true,
        }
    }

    pub fn fields_mut(&mut self) -> [(&'static str, &mut bool); 14] {
        [
            (tr!("화이트 밸런스", "White Balance"), &mut self.white_balance),
            (tr!("기본 톤", "Basic Tone"), &mut self.basic_tone),
            (tr!("외관(텍스처·명료도·디헤이즈·채도)", "Presence (Texture·Clarity·Dehaze·Saturation)"), &mut self.presence),
            (tr!("처리 방식·프로파일", "Treatment·Profile"), &mut self.treatment),
            (tr!("톤 커브", "Tone Curve"), &mut self.tone_curve),
            (tr!("HSL / 컬러 믹서", "HSL / Color Mixer"), &mut self.hsl),
            (tr!("포인트 컬러", "Point Color"), &mut self.point_color),
            (tr!("컬러 그레이딩", "Color Grading"), &mut self.color_grading),
            (tr!("디테일", "Detail"), &mut self.detail),
            (tr!("렌즈 교정", "Lens Corrections"), &mut self.lens),
            (tr!("자르기·회전", "Crop·Rotate"), &mut self.crop),
            (tr!("효과", "Effects"), &mut self.effects),
            (tr!("보정(캘리브레이션)", "Calibration"), &mut self.calibration),
            (tr!("마스크", "Masks"), &mut self.masks),
        ]
    }
}

impl Default for SettingGroups {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Treatment {
    #[default]
    Color,
    BlackWhite,
}

// ── Tone curve ─────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneCurve {
    pub highlights: f32,
    pub lights: f32,
    pub darks: f32,
    pub shadows: f32,
    /// Parametric region splits (0..1).
    pub splits: [f32; 3],
    /// Point curve (x,y in 0..1, ascending x). Empty means identity.
    pub rgb: Vec<[f32; 2]>,
    pub red: Vec<[f32; 2]>,
    pub green: Vec<[f32; 2]>,
    pub blue: Vec<[f32; 2]>,
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            splits: [0.25, 0.5, 0.75],
            rgb: identity_curve(),
            red: identity_curve(),
            green: identity_curve(),
            blue: identity_curve(),
        }
    }
}

pub fn identity_curve() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

pub fn is_identity_curve(c: &[[f32; 2]]) -> bool {
    c.iter().all(|p| (p[0] - p[1]).abs() < 1e-4)
}

// ── HSL / color grading ────────────────────────────────────

/// One point color: picked color (hue/saturation/brightness), shift amounts, and range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointColor {
    /// Picked color: hue (degrees), saturation (0..1, HSV), brightness (0..1, gamma luminance).
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
    /// Shift -100..100
    pub hue_shift: f32,
    pub sat_shift: f32,
    pub lum_shift: f32,
    /// Range 0..100 (default 50)
    pub hue_range: f32,
    pub sat_range: f32,
    pub lum_range: f32,
}

impl Default for PointColor {
    fn default() -> Self {
        Self { hue: 0.0, sat: 0.5, lum: 0.5, hue_shift: 0.0, sat_shift: 0.0, lum_shift: 0.0, hue_range: 50.0, sat_range: 50.0, lum_range: 50.0 }
    }
}

impl PointColor {
    /// Influence on a color (0..1): product of smooth windows over hue, saturation and brightness distance.
    #[inline]
    pub fn weight(&self, hue: f32, sat: f32, lum: f32) -> f32 {
        let mut dh = (hue - self.hue).abs() % 360.0;
        if dh > 180.0 {
            dh = 360.0 - dh;
        }
        let rh = 8.0 + self.hue_range * 0.6;
        let rs = 0.08 + self.sat_range / 100.0 * 0.7;
        let rl = 0.08 + self.lum_range / 100.0 * 0.7;
        let win = |d: f32, r: f32| {
            let t = (d / r).clamp(0.0, 1.0);
            let t = 1.0 - t * t;
            t * t
        };
        // Hue is meaningless near neutral: ignore it for a neutral picked color, and fade it out for near-neutral pixels
        let hue_w = if self.sat < 0.06 { 1.0 } else { win(dh, rh) * crate::develop::mask::smoothstep(0.0, 0.05, sat) };
        hue_w * win((sat - self.sat).abs(), rs) * win((lum - self.lum).abs(), rl)
    }

    pub fn is_noop(&self) -> bool {
        self.hue_shift == 0.0 && self.sat_shift == 0.0 && self.lum_shift == 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HslBand {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Wheel {
    /// 0..360
    pub hue: f32,
    /// 0..100
    pub sat: f32,
    /// -100..100
    pub lum: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorGrading {
    pub shadows: Wheel,
    pub midtones: Wheel,
    pub highlights: Wheel,
    pub global: Wheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self {
            shadows: Wheel::default(),
            midtones: Wheel::default(),
            highlights: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

impl ColorGrading {
    pub fn is_neutral(&self) -> bool {
        [self.shadows, self.midtones, self.highlights, self.global]
            .iter()
            .all(|w| w.sat.abs() < 1e-3 && w.lum.abs() < 1e-3)
    }
}

// ── Detail ────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub sharpen_amount: f32,
    pub sharpen_radius: f32,
    pub sharpen_detail: f32,
    pub sharpen_masking: f32,
    pub nr_luma: f32,
    pub nr_luma_detail: f32,
    pub nr_luma_contrast: f32,
    pub nr_color: f32,
    pub nr_color_detail: f32,
    pub nr_color_smooth: f32,
}

impl Default for Detail {
    fn default() -> Self {
        Self {
            sharpen_amount: 0.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            nr_luma: 0.0,
            nr_luma_detail: 50.0,
            nr_luma_contrast: 0.0,
            nr_color: 0.0,
            nr_color_detail: 50.0,
            nr_color_smooth: 50.0,
        }
    }
}

// ── Lens corrections ──────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensCorrection {
    /// Enable lens profile (LCP) correction.
    pub profile_enable: bool,
    /// LCP file (path or file name; empty = pick automatically from the EXIF lens).
    pub profile_file: String,
    pub profile_name: String,
    /// Profile distortion/vignetting amount (0..200, default 100).
    pub profile_dist: f32,
    pub profile_vig: f32,
    /// -100..100 (positive corrects barrel distortion)
    pub distortion: f32,
    /// -100..100 (positive brightens the corners)
    pub vignette: f32,
    /// 0..100
    pub vignette_midpoint: f32,
    /// Remove chromatic aberration.
    pub remove_ca: bool,
    /// Purple fringe removal amount 0..20
    pub defringe_purple: f32,
    /// Green fringe removal amount 0..20
    pub defringe_green: f32,
}

impl Default for LensCorrection {
    fn default() -> Self {
        Self {
            profile_enable: false,
            profile_file: String::new(),
            profile_name: String::new(),
            profile_dist: 100.0,
            profile_vig: 100.0,
            distortion: 0.0,
            vignette: 0.0,
            vignette_midpoint: 0.0,
            remove_ca: false,
            defringe_purple: 0.0,
            defringe_green: 0.0,
        }
    }
}

// ── Geometry (crop/rotate/flip) ───────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// Normalized crop rectangle [x0, y0, x1, y1] in the frame after rotation/flip.
    pub crop: [f32; 4],
    /// Fine rotation (degrees), -45..45
    pub angle: f32,
    /// Number of clockwise 90-degree rotations, 0..3
    pub rotate90: u8,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Locked aspect ratio (w/h). None = free.
    pub aspect: Option<f32>,
    // Transform
    pub vertical: f32,
    pub horizontal: f32,
    pub scale: f32,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            crop: [0.0, 0.0, 1.0, 1.0],
            angle: 0.0,
            rotate90: 0,
            flip_h: false,
            flip_v: false,
            aspect: None,
            vertical: 0.0,
            horizontal: 0.0,
            scale: 100.0,
        }
    }
}

impl Geometry {
    #[allow(dead_code)]
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
            || (self.crop == [0.0, 0.0, 1.0, 1.0]
                && self.angle == 0.0
                && self.rotate90.is_multiple_of(4)
                && !self.flip_h
                && !self.flip_v
                && self.vertical == 0.0
                && self.horizontal == 0.0
                && self.scale == 100.0)
    }
}

// ── Effects ───────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    pub vignette_highlights: f32,
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_roughness: f32,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            vignette_highlights: 0.0,
            grain_amount: 0.0,
            grain_size: 25.0,
            grain_roughness: 50.0,
        }
    }
}

// ── Calibration ───────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Calibration {
    pub shadows_tint: f32,
    pub red_hue: f32,
    pub red_sat: f32,
    pub green_hue: f32,
    pub green_sat: f32,
    pub blue_hue: f32,
    pub blue_sat: f32,
}

impl Calibration {
    #[allow(dead_code)]
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}

// ── Local masks ───────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub id: u64,
    pub name: String,
    pub visible: bool,
    pub invert: bool,
    /// Overall mask effect strength 0..1
    pub amount: f32,
    pub components: Vec<MaskComponent>,
    pub adj: LocalAdjust,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            visible: true,
            invert: false,
            amount: 1.0,
            components: Vec::new(),
            adj: LocalAdjust::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MaskOp {
    #[default]
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskComponent {
    pub op: MaskOp,
    pub invert: bool,
    pub shape: MaskShape,
}

/// All coordinates are normalized (0..1) in the default-orientation source (after EXIF rotation, before user rotation/crop).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MaskShape {
    Brush { strokes: Vec<BrushStroke> },
    /// 100% at p0, fading to 0% at p1.
    Linear { p0: [f32; 2], p1: [f32; 2] },
    /// Center, radius (normalized; x relative to width, y to height), rotation (degrees), feather 0..100.
    Radial { center: [f32; 2], radius: [f32; 2], angle: f32, feather: f32 },
    /// Luminance range (0..1), smooth 0..1
    Luminance { lo: f32, hi: f32, smooth: f32 },
    /// Color range: reference hue (degrees) and saturation, tolerance (degrees)
    Color { hue: f32, sat: f32, range: f32 },
    /// Whole image
    All,
    /// AI mask (subject/sky/background/people): bitmap key stored in the catalog and the photo it was made from (recomputed when pasted onto another photo).
    Ai { kind: crate::imaging::aimask::AiKind, key: String, src: String },
}

impl MaskShape {
    pub fn kind_name(&self) -> &'static str {
        match self {
            MaskShape::Brush { .. } => tr!("브러시", "Brush"),
            MaskShape::Linear { .. } => tr!("선형 그라디언트", "Linear Gradient"),
            MaskShape::Radial { .. } => tr!("방사형 그라디언트", "Radial Gradient"),
            MaskShape::Luminance { .. } => tr!("휘도 범위", "Luminance Range"),
            MaskShape::Color { .. } => tr!("색상 범위", "Color Range"),
            MaskShape::All => tr!("전체", "Whole Image"),
            MaskShape::Ai { kind, .. } => kind.name(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushStroke {
    pub points: Vec<[f32; 2]>,
    /// Radius (fraction of the image's long side)
    pub size: f32,
    /// 0..1
    pub feather: f32,
    /// 0..1
    pub flow: f32,
    pub erase: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LocalAdjust {
    pub temp: f32,
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub hue: f32,
    pub saturation: f32,
    pub sharpness: f32,
    pub noise: f32,
}

impl LocalAdjust {
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditSummary {
    pub crop: bool,
    pub adjust: bool,
    pub masks: bool,
}

#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Idx(usize),
}

/// Paths of changed values. The mask list counts as one entry (mask setups differ per photo).
fn json_diff(o: &serde_json::Value, n: &serde_json::Value, path: &mut Vec<Seg>, out: &mut Vec<(Vec<Seg>, serde_json::Value)>) {
    use serde_json::Value;
    if o == n {
        return;
    }
    let whole = matches!(path.first(), Some(Seg::Key(k)) if k == "masks");
    match (o, n) {
        (Value::Object(a), Value::Object(b)) if !whole => {
            for (k, nv) in b {
                path.push(Seg::Key(k.clone()));
                match a.get(k) {
                    Some(ov) => json_diff(ov, nv, path, out),
                    None => out.push((path.clone(), nv.clone())),
                }
                path.pop();
            }
        }
        (Value::Array(a), Value::Array(b)) if !whole && a.len() == b.len() && !path.is_empty() => {
            for (i, (ov, nv)) in a.iter().zip(b).enumerate() {
                path.push(Seg::Idx(i));
                json_diff(ov, nv, path, out);
                path.pop();
            }
        }
        _ => out.push((path.clone(), n.clone())),
    }
}

fn json_set(root: &mut serde_json::Value, path: &[Seg], v: serde_json::Value) {
    let mut cur = root;
    for seg in path {
        let next = match seg {
            Seg::Key(k) => cur.get_mut(k.as_str()),
            Seg::Idx(i) => cur.get_mut(*i),
        };
        match next {
            Some(x) => cur = x,
            None => return,
        }
    }
    *cur = v;
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn auto_sync_copies_only_changed_fields() {
        let old = DevelopSettings::default();
        let mut new = old.clone();
        new.exposure = 0.7;
        new.hsl[2].sat = -30.0;
        let mut other = DevelopSettings::default();
        other.contrast = 25.0;
        other.hsl[5].hue = 10.0;
        other.geometry.crop = [0.1, 0.1, 0.9, 0.9];
        other.apply_changes(&old, &new);
        assert_eq!(other.exposure, 0.7);
        assert_eq!(other.hsl[2].sat, -30.0);
        assert_eq!(other.contrast, 25.0);
        assert_eq!(other.hsl[5].hue, 10.0);
        assert_eq!(other.geometry.crop, [0.1, 0.1, 0.9, 0.9]);
    }

    #[test]
    fn auto_sync_white_balance_as_group() {
        let old = DevelopSettings::default();
        let mut new = old.clone();
        new.wb_custom = true;
        new.temp_k = 4300.0;
        let mut other = DevelopSettings::default();
        other.tint_k = 12.0;
        other.apply_changes(&old, &new);
        assert!(other.wb_custom);
        assert_eq!(other.temp_k, 4300.0);
        assert_eq!(other.tint_k, 0.0);
    }

    #[test]
    fn as_shot_display_values_are_not_edits() {
        let mut s = DevelopSettings::default_for(true);
        s.temp_k = 4650.0;
        s.tint_k = 23.0;
        assert!(s.is_default_for(true));
        assert_eq!(s.summary(true), EditSummary::default());
        s.geometry.crop = [0.0, 0.0, 0.5, 1.0];
        assert!(s.summary(true).crop && !s.summary(true).adjust);
    }
}
