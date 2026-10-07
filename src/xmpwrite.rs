//! XMP sidecar writing: Darkroom settings -> XMP develop settings (crs namespace) plus rating, label, title and keywords.
//! Lets other photo editors open the same edit (the reverse of the import conversion).
//! Local masks are written in the MaskGroupBasedCorrections format (linear, radial, brush, luminance range). Spot removal is not written yet.

use crate::catalog::{ColorLabel, Photo};
use crate::develop::settings::*;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Look profiles (name, UUID, supports amount) with the identifiers written to XMP (only those matching a built-in look name)
const LR_LOOKS: [(&str, &str, bool); 4] = [
    ("Adobe Color", "B952C231111CD8E0ECCF14B86BAA7077", false),
    ("Vintage 08", "8AC303446BAD50DBAE693845AEAFD3B2", true),
    ("Modern 02", "88DC6C1939F646FBA7F3216B9CC5948C", true),
    ("Vintage 01", "97291D549FC232787BDFD3353151BB62", true),
];

const BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn signed(v: f32) -> String {
    if v > 0.0 { format!("+{}", fmt_num(v)) } else { fmt_num(v) }
}

fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 { format!("{}", v.round() as i64) } else { format!("{v:.2}") }
}

/// Sidecar path (same name with the extension replaced by .xmp)
pub fn sidecar_path(p: &Photo) -> PathBuf {
    p.path.with_extension("xmp")
}

pub fn build(p: &Photo, s: &DevelopSettings) -> String {
    let mut a: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: String| a.push((k.to_string(), v));
    put("Version", "17.0".into());
    put("ProcessVersion", "15.4".into());
    // White balance
    if p.is_raw {
        if s.wb_custom {
            put("WhiteBalance", "Custom".into());
            put("Temperature", fmt_num(s.temp_k));
            put("Tint", signed(s.tint_k));
        } else {
            put("WhiteBalance", "As Shot".into());
        }
    } else {
        put("WhiteBalance", if s.temp == 0.0 && s.tint == 0.0 { "As Shot".into() } else { "Custom".into() });
        put("IncrementalTemperature", signed(s.temp));
        put("IncrementalTint", signed(s.tint));
    }
    // Basic
    put("Exposure2012", if s.exposure >= 0.0 { format!("+{:.2}", s.exposure) } else { format!("{:.2}", s.exposure) });
    put("Contrast2012", signed(s.contrast));
    put("Highlights2012", signed(s.highlights));
    put("Shadows2012", signed(s.shadows));
    put("Whites2012", signed(s.whites));
    put("Blacks2012", signed(s.blacks));
    put("Texture", signed(s.texture));
    put("Clarity2012", signed(s.clarity));
    put("Dehaze", signed(s.dehaze));
    put("Vibrance", signed(s.vibrance));
    put("Saturation", signed(s.saturation));
    // Process and black & white
    put("ConvertToGrayscale", if s.treatment == Treatment::BlackWhite { "True".into() } else { "False".into() });
    for (i, b) in BANDS.iter().enumerate() {
        put(&format!("GrayMixer{b}"), signed(s.bw_mix[i]));
    }
    // Parametric curve
    let c = &s.curve;
    put("ParametricShadows", signed(c.shadows));
    put("ParametricDarks", signed(c.darks));
    put("ParametricLights", signed(c.lights));
    put("ParametricHighlights", signed(c.highlights));
    put("ParametricShadowSplit", fmt_num((c.splits[0] * 100.0).round()));
    put("ParametricMidtoneSplit", fmt_num((c.splits[1] * 100.0).round()));
    put("ParametricHighlightSplit", fmt_num((c.splits[2] * 100.0).round()));
    // HSL
    for (i, b) in BANDS.iter().enumerate() {
        put(&format!("HueAdjustment{b}"), signed(s.hsl[i].hue));
        put(&format!("SaturationAdjustment{b}"), signed(s.hsl[i].sat));
        put(&format!("LuminanceAdjustment{b}"), signed(s.hsl[i].lum));
    }
    // Color grading (shadow/highlight hue and saturation use the SplitToning keys, as the XMP format expects)
    let g = &s.grading;
    put("SplitToningShadowHue", fmt_num(g.shadows.hue.round()));
    put("SplitToningShadowSaturation", fmt_num(g.shadows.sat.round()));
    put("SplitToningHighlightHue", fmt_num(g.highlights.hue.round()));
    put("SplitToningHighlightSaturation", fmt_num(g.highlights.sat.round()));
    put("SplitToningBalance", signed(g.balance));
    put("ColorGradeShadowLum", signed(g.shadows.lum));
    put("ColorGradeHighlightLum", signed(g.highlights.lum));
    put("ColorGradeMidtoneHue", fmt_num(g.midtones.hue.round()));
    put("ColorGradeMidtoneSat", fmt_num(g.midtones.sat.round()));
    put("ColorGradeMidtoneLum", signed(g.midtones.lum));
    put("ColorGradeGlobalHue", fmt_num(g.global.hue.round()));
    put("ColorGradeGlobalSat", fmt_num(g.global.sat.round()));
    put("ColorGradeGlobalLum", signed(g.global.lum));
    put("ColorGradeBlending", fmt_num(g.blending));
    // Detail
    let d = &s.detail;
    put("Sharpness", fmt_num(d.sharpen_amount));
    put("SharpenRadius", format!("{:+.1}", d.sharpen_radius));
    put("SharpenDetail", fmt_num(d.sharpen_detail));
    put("SharpenEdgeMasking", fmt_num(d.sharpen_masking));
    put("LuminanceSmoothing", fmt_num(d.nr_luma));
    put("LuminanceNoiseReductionDetail", fmt_num(d.nr_luma_detail));
    put("LuminanceNoiseReductionContrast", fmt_num(d.nr_luma_contrast));
    put("ColorNoiseReduction", fmt_num(d.nr_color));
    put("ColorNoiseReductionDetail", fmt_num(d.nr_color_detail));
    put("ColorNoiseReductionSmoothness", fmt_num(d.nr_color_smooth));
    // Lens
    let l = &s.lens;
    put("LensProfileEnable", if l.profile_enable { "1".into() } else { "0".into() });
    if l.profile_enable {
        // Lenses picked from lensfun have names other editors do not know, so leave the setup on Auto
        let lf = l.profile_file.starts_with(crate::develop::lensfun::PREFIX);
        put("LensProfileSetup", if l.profile_file.is_empty() || lf { "Auto".into() } else { "Custom".into() });
        if !l.profile_file.is_empty() && !lf {
            put("LensProfileFilename", esc(&l.profile_file));
        }
        if !l.profile_name.is_empty() && !lf {
            put("LensProfileName", esc(&l.profile_name));
        }
        put("LensProfileDistortionScale", fmt_num(l.profile_dist));
        put("LensProfileVignettingScale", fmt_num(l.profile_vig));
    }
    put("AutoLateralCA", if l.remove_ca { "1".into() } else { "0".into() });
    put("LensManualDistortionAmount", signed(l.distortion));
    put("VignetteAmount", signed(l.vignette));
    put("VignetteMidpoint", fmt_num(l.vignette_midpoint));
    put("DefringePurpleAmount", fmt_num(l.defringe_purple));
    put("DefringeGreenAmount", fmt_num(l.defringe_green));
    // Transform and crop
    let ge = &s.geometry;
    put("PerspectiveVertical", signed(ge.vertical));
    put("PerspectiveHorizontal", signed(ge.horizontal));
    put("PerspectiveScale", fmt_num(ge.scale));
    let has_crop = ge.crop != [0.0, 0.0, 1.0, 1.0] || ge.angle != 0.0;
    put("HasCrop", if has_crop { "True".into() } else { "False".into() });
    if has_crop {
        put("CropLeft", format!("{:.6}", ge.crop[0]));
        put("CropTop", format!("{:.6}", ge.crop[1]));
        put("CropRight", format!("{:.6}", ge.crop[2]));
        put("CropBottom", format!("{:.6}", ge.crop[3]));
        put("CropAngle", format!("{:.2}", ge.angle));
    }
    // Effects
    let f = &s.effects;
    put("PostCropVignetteAmount", signed(f.vignette_amount));
    put("PostCropVignetteMidpoint", fmt_num(f.vignette_midpoint));
    put("PostCropVignetteRoundness", signed(f.vignette_roundness));
    put("PostCropVignetteFeather", fmt_num(f.vignette_feather));
    put("PostCropVignetteHighlightContrast", fmt_num(f.vignette_highlights));
    put("GrainAmount", fmt_num(f.grain_amount));
    put("GrainSize", fmt_num(f.grain_size));
    put("GrainFrequency", fmt_num(f.grain_roughness));
    // Calibration
    let ca = &s.calibration;
    put("ShadowTint", signed(ca.shadows_tint));
    put("RedHue", signed(ca.red_hue));
    put("RedSaturation", signed(ca.red_sat));
    put("GreenHue", signed(ca.green_hue));
    put("GreenSaturation", signed(ca.green_sat));
    put("BlueHue", signed(ca.blue_hue));
    put("BlueSaturation", signed(ca.blue_sat));
    put("HasSettings", "True".into());

    // Profile: a look (color or creative) is written as its base camera profile + crs:Look (name, UUID); otherwise CameraProfile.
    // Only the compatible names and UUIDs are written (editors look profiles up by UUID); no profile file contents are included
    let profile = if s.profile.is_empty() { crate::develop::dcp::DEFAULT_PROFILE.to_string() } else { s.profile.clone() };
    let mut look_xml = String::new();
    match LR_LOOKS.iter().find(|(n, _, _)| *n == profile) {
        Some((name, uuid, amount)) if p.is_raw => {
            a.push(("CameraProfile".into(), "Adobe Standard".into()));
            let tf = |b: bool| if b { "True" } else { "False" };
            let _ = write!(
                look_xml,
                "
   <crs:Look>
    <rdf:Description crs:Name=\"{}\" crs:Amount=\"{:.2}\" crs:UUID=\"{uuid}\" crs:SupportsAmount=\"{}\" crs:SupportsMonochrome=\"False\" crs:SupportsOutputReferred=\"{}\" />
   </crs:Look>",
                esc(name),
                if *amount { s.profile_amount / 100.0 } else { 1.0 },
                tf(*amount),
                tf(*amount)
            );
        }
        _ if p.is_raw => a.push(("CameraProfile".into(), esc(&profile))),
        _ => {}
    }

    let curve_xml = |name: &str, pts: &[[f32; 2]]| -> String {
        if is_identity_curve(pts) {
            return String::new();
        }
        let lis: String = pts.iter().map(|q| format!("<rdf:li>{}, {}</rdf:li>", (q[0] * 255.0).round() as i32, (q[1] * 255.0).round() as i32)).collect();
        format!("\n   <crs:{name}><rdf:Seq>{lis}</rdf:Seq></crs:{name}>")
    };
    let mut body = String::new();
    body += &curve_xml("ToneCurvePV2012", &c.rgb);
    body += &curve_xml("ToneCurvePV2012Red", &c.red);
    body += &curve_xml("ToneCurvePV2012Green", &c.green);
    body += &curve_xml("ToneCurvePV2012Blue", &c.blue);
    body += &look_xml;
    body += &masks_xml(&s.masks);
    if !p.title.is_empty() {
        let _ = write!(body, "\n   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>", esc(&p.title));
    }
    if !p.caption.is_empty() {
        let _ = write!(body, "\n   <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>", esc(&p.caption));
    }
    if !p.keywords.is_empty() {
        let lis: String = p.keywords.iter().map(|k| format!("<rdf:li>{}</rdf:li>", esc(k))).collect();
        let _ = write!(body, "\n   <dc:subject><rdf:Bag>{lis}</rdf:Bag></dc:subject>");
    }
    let label = match p.label {
        ColorLabel::None => "",
        ColorLabel::Red => "Red",
        ColorLabel::Yellow => "Yellow",
        ColorLabel::Green => "Green",
        ColorLabel::Blue => "Blue",
        ColorLabel::Purple => "Purple",
    };
    let attrs: String = a.iter().map(|(k, v)| format!("\n   crs:{k}=\"{v}\"")).collect();
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Darkroom\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n    xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n   xmp:Rating=\"{}\"{}\n   xmp:CreatorTool=\"Darkroom\"{attrs}>{body}\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n",
        p.rating,
        if label.is_empty() { String::new() } else { format!("\n   xmp:Label=\"{label}\"") },
    )
}

/// Masks -> MaskGroupBasedCorrections local corrections. Coordinates are the same normalized source coordinates.
/// Combine modes: add = MaskBlendMode 0, intersect = 1, subtract = 1 + invert. Color range and whole-image masks use a different format and are skipped
fn masks_xml(masks: &[crate::develop::settings::Mask]) -> String {
    use crate::develop::settings::{MaskOp, MaskShape};
    if masks.is_empty() {
        return String::new();
    }
    let f = |v: f32| format!("{:.6}", v);
    let mut out = String::from("\n   <crs:MaskGroupBasedCorrections>\n    <rdf:Seq>");
    for m in masks {
        let a = &m.adj;
        let mut comps = String::new();
        for c in &m.components {
            let (blend, inverted) = match c.op {
                MaskOp::Add => (0, c.invert),
                MaskOp::Intersect => (1, c.invert),
                MaskOp::Subtract => (1, true),
            };
            let common = format!("crs:MaskActive=\"true\" crs:MaskBlendMode=\"{blend}\" crs:MaskInverted=\"{inverted}\" crs:MaskValue=\"1\"");
            let li = match &c.shape {
                MaskShape::Linear { p0, p1 } => format!(
                    "<rdf:li><rdf:Description crs:What=\"Mask/Gradient\" {common} crs:ZeroX=\"{}\" crs:ZeroY=\"{}\" crs:FullX=\"{}\" crs:FullY=\"{}\"/></rdf:li>",
                    f(p1[0]), f(p1[1]), f(p0[0]), f(p0[1])
                ),
                MaskShape::Radial { center, radius, angle, feather } => format!(
                    "<rdf:li><rdf:Description crs:What=\"Mask/CircularGradient\" {common} crs:Top=\"{}\" crs:Left=\"{}\" crs:Bottom=\"{}\" crs:Right=\"{}\" crs:Angle=\"{}\" crs:Midpoint=\"50\" crs:Roundness=\"0\" crs:Feather=\"{}\" crs:Flipped=\"false\"/></rdf:li>",
                    f(center[1] - radius[1]), f(center[0] - radius[0]), f(center[1] + radius[1]), f(center[0] + radius[0]), f(*angle), f(*feather)
                ),
                MaskShape::Brush { strokes } => {
                    // One paint mask per stroke (eraser strokes have MaskValue 0)
                    let mut s = String::new();
                    for st in strokes {
                        let dabs: String = st.points.iter().map(|p| format!("<rdf:li>d {} {}</rdf:li>", f(p[0]), f(p[1]))).collect();
                        let mv = if st.erase { 0 } else { 1 };
                        let _ = write!(
                            s,
                            "<rdf:li><rdf:Description crs:What=\"Mask/Paint\" crs:MaskActive=\"true\" crs:MaskBlendMode=\"{blend}\" crs:MaskInverted=\"{inverted}\" crs:MaskValue=\"{mv}\" crs:Radius=\"{}\" crs:Flow=\"{}\" crs:CenterWeight=\"{}\"><crs:Dabs><rdf:Seq>{dabs}</rdf:Seq></crs:Dabs></rdf:Description></rdf:li>",
                            f(st.size), f(st.flow), f(1.0 - st.feather)
                        );
                    }
                    s
                }
                MaskShape::Luminance { lo, hi, smooth } => {
                    let h = smooth * 0.5;
                    format!(
                        "<rdf:li><rdf:Description crs:What=\"Mask/RangeMask\" {common}><crs:CorrectionRangeMask><rdf:Description crs:Version=\"3\" crs:Type=\"2\" crs:LumRange=\"{} {} {} {}\" crs:LumFeather=\"{}\"/></crs:CorrectionRangeMask></rdf:Description></rdf:li>",
                        f((lo - h).max(0.0)), f((lo + h).min(1.0)), f((hi - h).max(0.0)), f((hi + h).min(1.0)), f(*smooth)
                    )
                }
                // AI masks are recomputed from the photo by the reading editor, so the bitmap is not written
                MaskShape::Color { .. } | MaskShape::All | MaskShape::Ai { .. } => String::new(),
            };
            comps.push_str(&li);
        }
        if comps.is_empty() {
            continue;
        }
        let _ = write!(
            out,
            "\n     <rdf:li>\n      <rdf:Description crs:What=\"Correction\" crs:CorrectionActive=\"{}\" crs:CorrectionName=\"{}\" crs:CorrectionAmount=\"{}\" crs:LocalExposure2012=\"{}\" crs:LocalContrast2012=\"{}\" crs:LocalHighlights2012=\"{}\" crs:LocalShadows2012=\"{}\" crs:LocalWhites2012=\"{}\" crs:LocalBlacks2012=\"{}\" crs:LocalClarity2012=\"{}\" crs:LocalTexture=\"{}\" crs:LocalDehaze=\"{}\" crs:LocalTemperature=\"{}\" crs:LocalTint=\"{}\" crs:LocalHue=\"{}\" crs:LocalSaturation=\"{}\" crs:LocalSharpness=\"{}\" crs:LocalLuminanceNoise=\"{}\">\n       <crs:CorrectionMasks><rdf:Seq>{comps}</rdf:Seq></crs:CorrectionMasks>\n      </rdf:Description>\n     </rdf:li>",
            m.visible,
            esc(&m.name),
            f(m.amount),
            f(a.exposure / 4.0),
            f(a.contrast / 100.0),
            f(a.highlights / 100.0),
            f(a.shadows / 100.0),
            f(a.whites / 100.0),
            f(a.blacks / 100.0),
            f(a.clarity / 100.0),
            f(a.texture / 100.0),
            f(a.dehaze / 100.0),
            f(a.temp / 100.0),
            f(a.tint / 100.0),
            f(a.hue / 180.0),
            f(a.saturation / 100.0),
            f(a.sharpness / 100.0),
            f(a.noise / 100.0),
        );
    }
    out.push_str("\n    </rdf:Seq>\n   </crs:MaskGroupBasedCorrections>");
    out
}

/// Save the sidecar. An existing file is backed up once to .xmp.bak.
pub fn write(p: &Photo, s: &DevelopSettings) -> std::io::Result<PathBuf> {
    let path = sidecar_path(p);
    if path.exists() {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        if !bak.exists() {
            std::fs::copy(&path, &bak)?;
        }
    }
    std::fs::write(&path, build(p, s))?;
    Ok(path)
}

#[allow(dead_code)]
pub fn exists(p: &Path) -> bool {
    p.with_extension("xmp").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writing and re-reading masks gives the same shapes and corrections (MaskGroupBasedCorrections round trip)
    #[test]
    fn masks_roundtrip() {
        use crate::develop::settings::*;
        let mut s = DevelopSettings::default_for(true);
        let mut m = Mask { id: 1, name: "하늘 & 앞".into(), amount: 0.8, ..Default::default() };
        m.adj.exposure = -0.6;
        m.adj.clarity = 25.0;
        m.adj.temp = -12.0;
        m.components.push(MaskComponent { op: MaskOp::Add, invert: false, shape: MaskShape::Linear { p0: [0.5, 0.1], p1: [0.5, 0.45] } });
        m.components.push(MaskComponent { op: MaskOp::Subtract, invert: false, shape: MaskShape::Radial { center: [0.4, 0.6], radius: [0.2, 0.15], angle: 12.0, feather: 40.0 } });
        // (intersect + invert means the same as subtract and is stored identically, so test intersect without invert)
        m.components.push(MaskComponent { op: MaskOp::Intersect, invert: false, shape: MaskShape::Luminance { lo: 0.3, hi: 0.8, smooth: 0.2 } });
        m.components.push(MaskComponent {
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Brush { strokes: vec![BrushStroke { points: vec![[0.1, 0.2], [0.15, 0.22]], size: 0.03, feather: 0.6, flow: 0.7, erase: false }] },
        });
        s.masks.push(m.clone());
        let p = crate::catalog::Photo {
            id: 1,
            path: PathBuf::from("C:/x/x.CR3"),
            folder: PathBuf::from("C:/x"),
            file_name: "x.CR3".into(),
            ext: "CR3".into(),
            file_size: 0,
            is_raw: true,
            meta: Default::default(),
            rating: 0,
            flag: crate::catalog::Flag::None,
            label: ColorLabel::None,
            title: String::new(),
            caption: String::new(),
            master: None,
            copy_name: String::new(),
            develop: None,
            import_id: 0,
            imported_at: 0,
            keywords: vec![],
            thumb_ver: 0,
            missing: false,
            exported_at: 0,
            edited_at: 0,
            stack_id: 0,
            stack_pos: 0,
        };
        let xmp = build(&p, &s);
        let (back, warn) = crate::lrpreset::xmp_masks(&xmp);
        assert_eq!(back.len(), 1, "{warn:?}\n{xmp}");
        let b = &back[0];
        assert_eq!(b.name, m.name);
        assert!((b.amount - 0.8).abs() < 1e-4);
        assert!((b.adj.exposure + 0.6).abs() < 1e-3 && (b.adj.clarity - 25.0).abs() < 1e-3 && (b.adj.temp + 12.0).abs() < 1e-3, "{:?}", b.adj);
        assert_eq!(b.components.len(), 4);
        for (x, y) in b.components.iter().zip(m.components.iter()) {
            assert_eq!(x.op, y.op, "{:?}", x.shape);
            assert_eq!(x.invert, y.invert, "{:?}", x.shape);
        }
        match (&b.components[1].shape, &m.components[1].shape) {
            (MaskShape::Radial { center: c1, radius: r1, angle: a1, .. }, MaskShape::Radial { center: c2, radius: r2, angle: a2, .. }) => {
                assert!((c1[0] - c2[0]).abs() < 1e-4 && (r1[1] - r2[1]).abs() < 1e-4 && (a1 - a2).abs() < 1e-3);
            }
            x => panic!("{x:?}"),
        }
        match &b.components[2].shape {
            MaskShape::Luminance { lo, hi, .. } => assert!((lo - 0.3).abs() < 1e-3 && (hi - 0.8).abs() < 1e-3),
            x => panic!("{x:?}"),
        }
    }

    /// Re-reading the written XMP with the import converter gives the same values (round-trip check)
    #[test]
    fn roundtrip_through_preset_parser() {
        let mut s = DevelopSettings::default_for(true);
        s.exposure = 0.65;
        s.contrast = 20.0;
        s.shadows = 45.0;
        s.wb_custom = true;
        s.temp_k = 4850.0;
        s.tint_k = 12.0;
        s.hsl[2].sat = -30.0;
        s.grading.shadows = Wheel { hue: 220.0, sat: 20.0, lum: -10.0 };
        s.grading.blending = 70.0;
        s.curve.rgb = vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]];
        s.geometry.crop = [0.1, 0.05, 0.9, 0.95];
        s.calibration.red_hue = 15.0;
        s.lens.profile_enable = true;
        let p = crate::catalog::Photo {
            id: 1,
            path: PathBuf::from(r"C:\x\a.CR3"),
            folder: PathBuf::from(r"C:\x"),
            file_name: "a.CR3".into(),
            ext: "CR3".into(),
            file_size: 0,
            is_raw: true,
            meta: Default::default(),
            rating: 4,
            flag: crate::catalog::Flag::None,
            label: ColorLabel::Red,
            title: "제목 & 테스트".into(),
            caption: String::new(),
            master: None,
            copy_name: String::new(),
            develop: None,
            import_id: 0,
            imported_at: 0,
            keywords: vec!["여행".into()],
            thumb_ver: 0,
            missing: false,
            exported_at: 0,
            edited_at: 0,
        stack_id: 0,
        stack_pos: 0,
        };
        let x = build(&p, &s);
        assert!(x.contains("xmp:Rating=\"4\""));
        assert!(x.contains("제목 &amp; 테스트"));
        let r = crate::lrpreset::parse_xmp(&x, "", "").unwrap();
        let t = r.settings;
        assert!((t.exposure - 0.65).abs() < 1e-3);
        assert_eq!((t.contrast, t.shadows), (20.0, 45.0));
        assert!(t.wb_custom && t.temp_k == 4850.0 && t.tint_k == 12.0);
        assert_eq!(t.hsl[2].sat, -30.0);
        assert_eq!(t.grading.shadows.hue, 220.0);
        assert_eq!(t.grading.shadows.lum, -10.0);
        assert_eq!(t.grading.blending, 70.0);
        assert_eq!(t.curve.rgb.len(), 4);
        assert_eq!(t.geometry.crop, [0.1, 0.05, 0.9, 0.95]);
        assert_eq!(t.calibration.red_hue, 15.0);
        assert!(t.lens.profile_enable);
        assert_eq!(t.profile, "Adobe Color");
    }
}
