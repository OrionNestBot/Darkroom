//! Preset import: develop presets (.xmp / .lrtemplate) and watermark presets (.lrtemplate).
//! Maps XMP develop settings (crs namespace) to Darkroom settings: values with the same range are copied,
//! values with a different meaning (e.g. absolute color temperature) are approximated and reported as warnings.

use crate::develop::settings::*;
use crate::export::watermark::{Watermark, WmAlign, WmKind, WmSize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ImportedPreset {
    pub group: String,
    pub name: String,
    pub settings: DevelopSettings,
    pub groups: SettingGroups,
    pub warnings: Vec<String>,
}

/// Default preset folders of another photo editor, if it is installed.
pub fn default_dirs() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else { return vec![] };
    [
        appdata.join("Adobe").join("CameraRaw").join("Settings"),
        appdata.join("Adobe").join("Lightroom").join("Develop Presets"),
    ]
    .into_iter()
    .filter(|p| p.is_dir())
    .collect()
}

pub fn watermark_dir() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("APPDATA")?).join("Adobe").join("Lightroom").join("Watermarks");
    p.is_dir().then_some(p)
}

/// Collects preset files from a folder, including subfolders.
pub fn collect_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).map(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e))).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    // Handle UTF-8 BOM / UTF-16 LE
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16(&u).ok();
    }
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    Some(String::from_utf8_lossy(b).into_owned())
}

fn stem_and_parent(path: &Path) -> (String, String) {
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let group = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Lightroom".into());
    (group, name)
}

/// Imports one file; the format is chosen by extension.
pub fn import_file(path: &Path) -> Result<ImportedPreset, String> {
    let text = read_text(path).ok_or(tr!("읽기 실패", "Read failed"))?;
    let (g, n) = stem_and_parent(path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "xmp" => parse_xmp(&text, &g, &n),
        "lrtemplate" => parse_lrtemplate(&text, &g, &n),
        _ => Err(tr!("지원하지 않는 형식", "Unsupported format").into()),
    }
}

// ───────────────────────── XMP ─────────────────────────

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&#xA;", "\n").replace("&amp;", "&")
}

/// The li values of `<crs:Tag> ... <rdf:li ...>value</rdf:li> ... </crs:Tag>`.
fn xmp_li_values(text: &str, tag: &str) -> Option<Vec<String>> {
    let open = format!("<crs:{tag}>");
    let close = format!("</crs:{tag}>");
    let s = text.find(&open)? + open.len();
    let e = s + text[s..].find(&close)?;
    let body = &text[s..e];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("<rdf:li") {
        let after = &rest[i..];
        let gt = after.find('>')?;
        if after[..gt].ends_with('/') {
            out.push(String::new());
            rest = &after[gt + 1..];
            continue;
        }
        let end = after.find("</rdf:li>")?;
        out.push(xml_unescape(after[gt + 1..end].trim()));
        rest = &after[end + 9..];
    }
    Some(out)
}

pub fn parse_xmp(text: &str, fallback_group: &str, fallback_name: &str) -> Result<ImportedPreset, String> {
    if !text.contains("camera-raw-settings") {
        return Err(tr!("XMP 현상 설정이 아님", "Not XMP develop settings").into());
    }
    // Parameters inside <crs:Look>…</crs:Look> (look tone curve, ProcessVersion, clarity, ...) belong to the profile;
    // reading them as user settings would apply the look curve twice, so only the look name is read separately.
    let full = text;
    let stripped: String;
    let text: &str = match (full.find("<crs:Look>"), full.find("</crs:Look>")) {
        (Some(a), Some(b)) if b > a => {
            stripped = format!("{}{}", &full[..a], &full[b + "</crs:Look>".len()..]);
            &stripped
        }
        _ => full,
    };
    let mut map: HashMap<String, String> = HashMap::new();
    // Attribute form: crs:Key="value"
    let mut rest = text;
    while let Some(i) = rest.find("crs:") {
        let after = &rest[i + 4..];
        let key_end = after.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(after.len());
        let key = &after[..key_end];
        let tail = &after[key_end..];
        if let Some(v) = tail.strip_prefix("=\"") {
            if let Some(q) = v.find('"') {
                map.insert(key.to_string(), xml_unescape(&v[..q]));
                rest = &v[q..];
                continue;
            }
        } else if let Some(v) = tail.strip_prefix('>') {
            // Element form: <crs:Key>value</crs:Key> (only when it has no child elements)
            if let Some(end) = v.find('<') {
                let val = v[..end].trim();
                if !val.is_empty() && v[end..].starts_with(&format!("</crs:{key}>")) {
                    map.insert(key.to_string(), xml_unescape(val));
                }
            }
        }
        rest = &after[key_end..];
    }
    if map.get("HasSettings").map(|v| v.eq_ignore_ascii_case("false")).unwrap_or(false) {
        return Err(tr!("설정이 없는 프리셋 (프로파일 전용)", "Preset has no settings (profile only)").into());
    }
    let mut curves: HashMap<String, Vec<[f32; 2]>> = HashMap::new();
    for t in ["ToneCurvePV2012", "ToneCurvePV2012Red", "ToneCurvePV2012Green", "ToneCurvePV2012Blue"] {
        if let Some(vals) = xmp_li_values(text, t) {
            let pts: Vec<[f32; 2]> = vals
                .iter()
                .filter_map(|v| {
                    let mut it = v.split(',').map(|x| x.trim().parse::<f32>().ok());
                    Some([it.next()?? / 255.0, it.next()?? / 255.0])
                })
                .collect();
            if pts.len() >= 2 {
                curves.insert(t.to_string(), pts);
            }
        }
    }
    let name = xmp_li_values(text, "Name").and_then(|v| v.into_iter().find(|s| !s.is_empty())).unwrap_or_else(|| fallback_name.to_string());
    let group = xmp_li_values(text, "Group").and_then(|v| v.into_iter().find(|s| !s.is_empty())).unwrap_or_else(|| fallback_group.to_string());
    let has_masks = text.contains("MaskGroupBasedCorrections") || text.contains("<crs:PaintBasedCorrections") || text.contains("<crs:GradientBasedCorrections") || text.contains("<crs:CircularGradientBasedCorrections");
    // XMP look: <crs:Look><rdf:Description crs:Name="..." .../>
    if let Some(i) = full.find("<crs:Look") {
        let text = full;
        let tail = &text[i..];
        let end = tail.find("</crs:Look>").unwrap_or(tail.len().min(4000));
        let block = &tail[..end];
        if let Some(j) = block.find("crs:Name=\"") {
            let rest = &block[j + 10..];
            if let Some(q) = rest.find('"') {
                map.insert("LookName".into(), xml_unescape(&rest[..q]));
            }
        }
        // Look amount (crs:Amount), read only from the look header so it is not confused with values inside the parameters
        let head = &block[..block.find("<crs:Parameters").unwrap_or(block.len())];
        if let Some(j) = head.find("crs:Amount=\"") {
            let rest = &head[j + 12..];
            if let Some(q) = rest.find('"') {
                map.insert("LookAmount".into(), rest[..q].to_string());
            }
        }
    }
    let (settings, groups, mut warnings) = from_crs(&map, &curves);
    // Warn only for profiles Darkroom does not know (look name or camera profile); common ones map to built-in profiles and looks
    let has_look = map.get("LookName").or_else(|| map.get("CameraProfile")).map(|n| !crate::develop::dcp::profile_known(n)).unwrap_or(false);
    let mut settings = settings;
    if has_masks {
        let (masks, w) = xmp_masks(text);
        if masks.is_empty() {
            warnings.push(tr!("로컬 보정(마스크)은 가져오지 않음", "Local adjustments (masks) were not imported").into());
        } else {
            settings.masks = masks;
        }
        warnings.extend(w);
    }
    if has_look {
        warnings.push(tr!("이 프로파일은 Darkroom에 없음 — 표준 프로파일로 대체", "This profile is not available in Darkroom — replaced with the standard profile").into());
    }
    if !groups_any(&groups) {
        return Err(tr!("가져올 현상 설정이 없음", "No develop settings to import").into());
    }
    Ok(ImportedPreset { group, name, settings, groups, warnings })
}

fn groups_any(g: &SettingGroups) -> bool {
    let mut g = g.clone();
    g.fields_mut().iter().any(|(_, v)| **v)
}

// ───────────────────────── Conversion ─────────────────────────

const BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

fn num(map: &HashMap<String, String>, k: &str) -> Option<f32> {
    map.get(k).and_then(|v| v.trim().trim_start_matches('+').parse::<f32>().ok())
}

fn truthy(map: &HashMap<String, String>, k: &str) -> Option<bool> {
    map.get(k).map(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// XMP settings map → Darkroom settings + included groups + warnings.
pub fn from_crs(map: &HashMap<String, String>, curves: &HashMap<String, Vec<[f32; 2]>>) -> (DevelopSettings, SettingGroups, Vec<String>) {
    let mut s = DevelopSettings::default();
    let mut g = SettingGroups {
        white_balance: false,
        basic_tone: false,
        presence: false,
        treatment: false,
        tone_curve: false,
        hsl: false,
        color_grading: false,
        detail: false,
        lens: false,
        crop: false,
        effects: false,
        calibration: false,
        masks: false,
        point_color: false,
    };
    let warn = Vec::new();
    let set = |v: &mut f32, k: &str, flag: &mut bool| {
        if let Some(x) = num(map, k) {
            *v = x;
            *flag = true;
        }
    };
    // White balance
    match map.get("WhiteBalance").map(String::as_str) {
        Some("As Shot") => g.white_balance = true,
        Some(_) | None => {
            if let Some(t) = num(map, "IncrementalTemperature") {
                s.temp = t.clamp(-100.0, 100.0);
                g.white_balance = true;
            } else if let Some(k) = num(map, "Temperature") {
                // RAW color temperature (K) and tint use the same units and are copied as is
                s.wb_custom = true;
                s.temp_k = k.clamp(2000.0, 50000.0);
                // Relative approximation for when the preset is applied to JPEGs etc.
                s.temp = (1.0e6 / 5500.0 - 1.0e6 / k.max(1000.0)).clamp(-100.0, 100.0).round();
                g.white_balance = true;
            }
            if let Some(t) = num(map, "IncrementalTint") {
                s.tint = t.clamp(-100.0, 100.0);
                g.white_balance = true;
            } else if let Some(t) = num(map, "Tint") {
                s.tint_k = t.clamp(-150.0, 150.0);
                s.tint = (t / 1.5).clamp(-100.0, 100.0).round();
                g.white_balance = true;
            }
        }
    }
    // Basic tone (PV2012 names first, older names as fallback)
    let bt = &mut g.basic_tone;
    for (v, k, old) in [
        (&mut s.exposure, "Exposure2012", "Exposure"),
        (&mut s.contrast, "Contrast2012", "Contrast"),
        (&mut s.highlights, "Highlights2012", "HighlightRecovery"),
        (&mut s.shadows, "Shadows2012", "FillLight"),
        (&mut s.whites, "Whites2012", ""),
        (&mut s.blacks, "Blacks2012", ""),
    ] {
        if let Some(x) = num(map, k) {
            *v = x;
            *bt = true;
        } else if !old.is_empty()
            && let Some(x) = num(map, old) {
                // PV2010 → approximation (highlight recovery goes negative)
                *v = match old {
                    "HighlightRecovery" => -x,
                    "Contrast" => (x - 25.0) * 1.0,
                    _ => x,
                };
                *bt = true;
            }
    }
    let pr = &mut g.presence;
    set(&mut s.texture, "Texture", pr);
    set(&mut s.clarity, "Clarity2012", pr);
    if !*pr {
        set(&mut s.clarity, "Clarity", pr);
    }
    set(&mut s.dehaze, "Dehaze", pr);
    set(&mut s.vibrance, "Vibrance", pr);
    set(&mut s.saturation, "Saturation", pr);
    // Profile: the look name if present, otherwise the camera profile
    if let Some(prof) = map.get("LookName").or_else(|| map.get("CameraProfile")) {
        s.profile = prof.clone();
        g.treatment = true;
    }
    if let Some(a) = num(map, "LookAmount") {
        s.profile_amount = (a * 100.0).clamp(0.0, 200.0);
    }
    // Treatment / black & white mix
    if let Some(bw) = truthy(map, "ConvertToGrayscale") {
        s.treatment = if bw { Treatment::BlackWhite } else { Treatment::Color };
        g.treatment = true;
    }
    for (i, b) in BANDS.iter().enumerate() {
        set(&mut s.bw_mix[i], &format!("GrayMixer{b}"), &mut g.treatment);
    }
    // Tone curve
    let tc = &mut g.tone_curve;
    set(&mut s.curve.highlights, "ParametricHighlights", tc);
    set(&mut s.curve.lights, "ParametricLights", tc);
    set(&mut s.curve.darks, "ParametricDarks", tc);
    set(&mut s.curve.shadows, "ParametricShadows", tc);
    for (i, k) in ["ParametricShadowSplit", "ParametricMidtoneSplit", "ParametricHighlightSplit"].iter().enumerate() {
        if let Some(x) = num(map, k) {
            s.curve.splits[i] = (x / 100.0).clamp(0.05, 0.95);
            *tc = true;
        }
    }
    for (k, dst) in [
        ("ToneCurvePV2012", &mut s.curve.rgb),
        ("ToneCurvePV2012Red", &mut s.curve.red),
        ("ToneCurvePV2012Green", &mut s.curve.green),
        ("ToneCurvePV2012Blue", &mut s.curve.blue),
    ] {
        if let Some(p) = curves.get(k) {
            let mut p = p.clone();
            p.sort_by(|a, b| a[0].total_cmp(&b[0]));
            p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-4);
            *dst = p;
            *tc = true;
        }
    }
    // HSL
    for (i, b) in BANDS.iter().enumerate() {
        set(&mut s.hsl[i].hue, &format!("HueAdjustment{b}"), &mut g.hsl);
        set(&mut s.hsl[i].sat, &format!("SaturationAdjustment{b}"), &mut g.hsl);
        set(&mut s.hsl[i].lum, &format!("LuminanceAdjustment{b}"), &mut g.hsl);
    }
    // Color grading (new names first, old split toning as fallback)
    let cg = &mut g.color_grading;
    let mut wheel = |w: &mut Wheel, pre: &str, legacy: Option<&str>| {
        let hue = num(map, &format!("ColorGrade{pre}Hue")).or_else(|| legacy.and_then(|l| num(map, &format!("SplitToning{l}Hue"))));
        let sat = num(map, &format!("ColorGrade{pre}Sat")).or_else(|| legacy.and_then(|l| num(map, &format!("SplitToning{l}Saturation"))));
        let lum = num(map, &format!("ColorGrade{pre}Lum"));
        if let Some(h) = hue {
            w.hue = h;
            *cg = true;
        }
        if let Some(x) = sat {
            w.sat = x;
            *cg = true;
        }
        if let Some(x) = lum {
            w.lum = x;
            *cg = true;
        }
    };
    wheel(&mut s.grading.shadows, "Shadow", Some("Shadow"));
    wheel(&mut s.grading.midtones, "Midtone", None);
    wheel(&mut s.grading.highlights, "Highlight", Some("Highlight"));
    wheel(&mut s.grading.global, "Global", None);
    set(&mut s.grading.blending, "ColorGradeBlending", cg);
    if let Some(b) = num(map, "ColorGradeBalance").or_else(|| num(map, "SplitToningBalance")) {
        s.grading.balance = b;
        *cg = true;
    }
    // Detail
    let dt = &mut g.detail;
    set(&mut s.detail.sharpen_amount, "Sharpness", dt);
    set(&mut s.detail.sharpen_radius, "SharpenRadius", dt);
    set(&mut s.detail.sharpen_detail, "SharpenDetail", dt);
    set(&mut s.detail.sharpen_masking, "SharpenEdgeMasking", dt);
    set(&mut s.detail.nr_luma, "LuminanceSmoothing", dt);
    set(&mut s.detail.nr_luma_detail, "LuminanceNoiseReductionDetail", dt);
    set(&mut s.detail.nr_luma_contrast, "LuminanceNoiseReductionContrast", dt);
    set(&mut s.detail.nr_color, "ColorNoiseReduction", dt);
    set(&mut s.detail.nr_color_detail, "ColorNoiseReductionDetail", dt);
    set(&mut s.detail.nr_color_smooth, "ColorNoiseReductionSmoothness", dt);
    // Lens
    let ln = &mut g.lens;
    set(&mut s.lens.distortion, "LensManualDistortionAmount", ln);
    set(&mut s.lens.vignette, "VignetteAmount", ln);
    set(&mut s.lens.vignette_midpoint, "VignetteMidpoint", ln);
    set(&mut s.lens.defringe_purple, "DefringePurpleAmount", ln);
    set(&mut s.lens.defringe_green, "DefringeGreenAmount", ln);
    if let Some(b) = truthy(map, "AutoLateralCA") {
        s.lens.remove_ca = b;
        *ln = true;
    }
    if let Some(on) = truthy(map, "LensProfileEnable") {
        s.lens.profile_enable = on;
        *ln = true;
        if let Some(f) = map.get("LensProfileFilename") {
            s.lens.profile_file = f.clone();
        }
        if let Some(n) = map.get("LensProfileName") {
            s.lens.profile_name = n.clone();
        }
        if let Some(v) = num(map, "LensProfileDistortionScale") {
            s.lens.profile_dist = v;
        }
        if let Some(v) = num(map, "LensProfileVignettingScale") {
            s.lens.profile_vig = v;
        }
    }
    // Transform / crop
    let cr = &mut g.crop;
    set(&mut s.geometry.vertical, "PerspectiveVertical", cr);
    set(&mut s.geometry.horizontal, "PerspectiveHorizontal", cr);
    set(&mut s.geometry.scale, "PerspectiveScale", cr);
    if let Some(a) = num(map, "PerspectiveRotate") {
        s.geometry.angle += a;
        *cr = true;
    }
    if truthy(map, "HasCrop") == Some(true) {
        if let (Some(l), Some(t), Some(r), Some(b)) = (num(map, "CropLeft"), num(map, "CropTop"), num(map, "CropRight"), num(map, "CropBottom")) {
            s.geometry.crop = [l, t, r, b];
            *cr = true;
        }
        if let Some(a) = num(map, "CropAngle") {
            s.geometry.angle += a;
        }
    }
    // Effects
    let fx = &mut g.effects;
    set(&mut s.effects.vignette_amount, "PostCropVignetteAmount", fx);
    set(&mut s.effects.vignette_midpoint, "PostCropVignetteMidpoint", fx);
    set(&mut s.effects.vignette_roundness, "PostCropVignetteRoundness", fx);
    set(&mut s.effects.vignette_feather, "PostCropVignetteFeather", fx);
    set(&mut s.effects.vignette_highlights, "PostCropVignetteHighlightContrast", fx);
    set(&mut s.effects.grain_amount, "GrainAmount", fx);
    set(&mut s.effects.grain_size, "GrainSize", fx);
    set(&mut s.effects.grain_roughness, "GrainFrequency", fx);
    // Calibration
    let ca = &mut g.calibration;
    set(&mut s.calibration.shadows_tint, "ShadowTint", ca);
    set(&mut s.calibration.red_hue, "RedHue", ca);
    set(&mut s.calibration.red_sat, "RedSaturation", ca);
    set(&mut s.calibration.green_hue, "GreenHue", ca);
    set(&mut s.calibration.green_sat, "GreenSaturation", ca);
    set(&mut s.calibration.blue_hue, "BlueHue", ca);
    set(&mut s.calibration.blue_sat, "BlueSaturation", ca);
    (s, g, warn)
}

// ───────────────────────── XMP masks (XML → Lua structure) ─────────────────────────

/// Minimal XML tree (handles only the XMP rdf structure)
#[derive(Debug, Default)]
struct XNode {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XNode>,
    text: String,
}

fn parse_xml(s: &str) -> Vec<XNode> {
    fn local(n: &str) -> String {
        n.rsplit(':').next().unwrap_or(n).to_string()
    }
    let b = s.as_bytes();
    let mut stack: Vec<XNode> = vec![XNode::default()];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if s[i..].starts_with("<!--") {
                i = s[i..].find("-->").map(|k| i + k + 3).unwrap_or(b.len());
                continue;
            }
            if s[i..].starts_with("<?") || s[i..].starts_with("<!") {
                i = s[i..].find('>').map(|k| i + k + 1).unwrap_or(b.len());
                continue;
            }
            let end = match s[i..].find('>') {
                Some(k) => i + k,
                None => break,
            };
            let tag = &s[i + 1..end];
            if let Some(name) = tag.strip_prefix('/') {
                let _ = name;
                if stack.len() > 1 {
                    let n = stack.pop().unwrap();
                    stack.last_mut().unwrap().children.push(n);
                }
            } else {
                let selfclose = tag.ends_with('/');
                let tag = tag.trim_end_matches('/');
                let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
                let mut node = XNode { name: local(&tag[..name_end]), ..Default::default() };
                // Attribute: key="value"
                let mut rest = &tag[name_end..];
                while let Some(eq) = rest.find('=') {
                    let key = rest[..eq].trim().to_string();
                    let after = &rest[eq + 1..];
                    let q = after.chars().next().unwrap_or('"');
                    let after = &after[q.len_utf8()..];
                    let Some(close) = after.find(q) else { break };
                    node.attrs.push((local(&key), xml_unescape(&after[..close])));
                    rest = &after[close + 1..];
                }
                if selfclose {
                    stack.last_mut().unwrap().children.push(node);
                } else {
                    stack.push(node);
                }
            }
            i = end + 1;
        } else {
            let next = s[i..].find('<').map(|k| i + k).unwrap_or(b.len());
            let t = s[i..next].trim();
            if !t.is_empty() {
                stack.last_mut().unwrap().text.push_str(&xml_unescape(t));
            }
            i = next;
        }
    }
    while stack.len() > 1 {
        let n = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(n);
    }
    stack.pop().map(|r| r.children).unwrap_or_default()
}

fn attr_val(v: &str) -> Lua {
    match v.to_ascii_lowercase().as_str() {
        "true" => Lua::Bool(true),
        "false" => Lua::Bool(false),
        _ => Lua::Str(v.to_string()),
    }
}

/// rdf structure → Lua table: Description = table of attributes and child properties, Seq/Bag = ordered list, li = content
fn xnode_value(n: &XNode) -> Lua {
    match n.name.as_str() {
        "Description" => {
            let mut v: Vec<(Option<String>, Lua)> = n.attrs.iter().filter(|(k, _)| k != "about").map(|(k, x)| (Some(k.clone()), attr_val(x))).collect();
            for c in &n.children {
                v.push((Some(c.name.clone()), prop_value(c)));
            }
            Lua::Table(v)
        }
        "Seq" | "Bag" | "Alt" => Lua::Table(n.children.iter().filter(|c| c.name == "li").map(|c| (None, prop_value(c))).collect()),
        _ => prop_value(n),
    }
}

/// Value of a property element (crs:X) or li: its structure if it has child elements, otherwise its text
fn prop_value(n: &XNode) -> Lua {
    if let Some(c) = n.children.first() {
        return xnode_value(c);
    }
    if !n.attrs.is_empty() {
        // e.g. rdf:parseType="Resource": attributes become a table
        return Lua::Table(n.attrs.iter().map(|(k, x)| (Some(k.clone()), attr_val(x))).collect());
    }
    attr_val(&n.text)
}

fn find_node<'a>(nodes: &'a [XNode], name: &str) -> Option<&'a XNode> {
    for n in nodes {
        if n.name == name {
            return Some(n);
        }
        if let Some(x) = find_node(&n.children, name) {
            return Some(x);
        }
    }
    None
}

/// Local adjustments (new and old style) in XMP text → masks
pub fn xmp_masks(text: &str) -> (Vec<Mask>, Vec<String>) {
    let tree = parse_xml(text);
    let mut root: Vec<(Option<String>, Lua)> = Vec::new();
    for key in ["MaskGroupBasedCorrections", "GradientBasedCorrections", "CircularGradientBasedCorrections", "PaintBasedCorrections"] {
        if let Some(n) = find_node(&tree, key) {
            root.push((Some(key.to_string()), prop_value(n)));
        }
    }
    masks_from_lua(&Lua::Table(root))
}

// ───────────────────────── Lua (.lrtemplate) ─────────────────────────

#[derive(Clone, Debug)]
pub enum Lua {
    Num(f64),
    Str(String),
    Bool(bool),
    Table(Vec<(Option<String>, Lua)>),
    Nil,
}

impl Lua {
    pub fn get(&self, k: &str) -> Option<&Lua> {
        match self {
            Lua::Table(v) => v.iter().find(|(key, _)| key.as_deref() == Some(k)).map(|(_, x)| x),
            _ => None,
        }
    }
    fn as_string(&self) -> Option<String> {
        match self {
            Lua::Num(n) => Some(format!("{n}")),
            Lua::Str(s) => Some(s.clone()),
            Lua::Bool(b) => Some(if *b { "True".into() } else { "False".into() }),
            _ => None,
        }
    }
    pub fn str(&self, k: &str) -> Option<String> {
        self.get(k).and_then(|v| v.as_string())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        match self.get(k)? {
            Lua::Num(n) => Some(*n),
            Lua::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn bool(&self, k: &str) -> Option<bool> {
        match self.get(k)? {
            Lua::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

struct LuaParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> LuaParser<'a> {
    fn ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"--") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }
    fn ident(&mut self) -> Option<String> {
        let st = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > st).then(|| String::from_utf8_lossy(&self.s[st..self.i]).into_owned())
    }
    fn string(&mut self) -> Option<String> {
        let q = self.s[self.i];
        if q == b'[' {
            // [[ long string ]] / [=[ ... ]=]
            let mut eq = 0;
            let mut j = self.i + 1;
            while j < self.s.len() && self.s[j] == b'=' {
                eq += 1;
                j += 1;
            }
            if self.s.get(j) != Some(&b'[') {
                return None;
            }
            let close = format!("]{}]", "=".repeat(eq));
            let body_start = j + 1;
            let rest = &self.s[body_start..];
            let end = rest.windows(close.len()).position(|w| w == close.as_bytes())?;
            self.i = body_start + end + close.len();
            return Some(String::from_utf8_lossy(&rest[..end]).into_owned());
        }
        self.i += 1;
        let mut out = Vec::new();
        while self.i < self.s.len() && self.s[self.i] != q {
            if self.s[self.i] == b'\\' && self.i + 1 < self.s.len() {
                self.i += 1;
                out.push(match self.s[self.i] {
                    b'n' => b'\n',
                    b't' => b'\t',
                    c => c,
                });
            } else {
                out.push(self.s[self.i]);
            }
            self.i += 1;
        }
        self.i += 1;
        Some(String::from_utf8_lossy(&out).into_owned())
    }
    fn value(&mut self) -> Option<Lua> {
        self.ws();
        let c = *self.s.get(self.i)?;
        match c {
            b'{' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        break;
                    }
                    // key = value | [ "key" ] = value | value
                    let save = self.i;
                    let mut key = None;
                    if self.s[self.i] == b'[' && matches!(self.s.get(self.i + 1), Some(b'"') | Some(b'\'')) {
                        self.i += 1;
                        key = self.string();
                        self.ws();
                        self.i += 1; // ]
                        self.ws();
                        if self.s.get(self.i) == Some(&b'=') {
                            self.i += 1;
                        }
                    } else if let Some(id) = self.ident() {
                        self.ws();
                        if self.s.get(self.i) == Some(&b'=') && self.s.get(self.i + 1) != Some(&b'=') {
                            self.i += 1;
                            key = Some(id);
                        } else {
                            self.i = save;
                        }
                    }
                    let v = self.value()?;
                    items.push((key, v));
                    self.ws();
                    if matches!(self.s.get(self.i), Some(b',') | Some(b';')) {
                        self.i += 1;
                    }
                }
                Some(Lua::Table(items))
            }
            b'"' | b'\'' | b'[' => self.string().map(Lua::Str),
            _ => {
                let st = self.i;
                while self.i < self.s.len() && !matches!(self.s[self.i], b',' | b'}' | b';') && !self.s[self.i].is_ascii_whitespace() {
                    self.i += 1;
                }
                let tok = String::from_utf8_lossy(&self.s[st..self.i]).into_owned();
                Some(match tok.as_str() {
                    "true" => Lua::Bool(true),
                    "false" => Lua::Bool(false),
                    "nil" => Lua::Nil,
                    t => t.parse::<f64>().map(Lua::Num).unwrap_or(Lua::Str(t.to_string())),
                })
            }
        }
    }
}

/// Parses the `s = { ... }` form.
pub fn parse_lua(text: &str) -> Option<Lua> {
    let start = text.find('{')?;
    let mut p = LuaParser { s: text.as_bytes(), i: start };
    p.value()
}

pub fn parse_lrtemplate(text: &str, fallback_group: &str, fallback_name: &str) -> Result<ImportedPreset, String> {
    let root = parse_lua(text).ok_or(tr!("Lua 형식 파싱 실패", "Lua parsing failed"))?;
    let ty = root.str("type").unwrap_or_default();
    if !ty.is_empty() && ty != "Develop" {
        return Err(trf!("현상 프리셋이 아님 ({ty})", "Not a develop preset ({ty})"));
    }
    let settings = root.get("value").and_then(|v| v.get("settings")).ok_or(tr!("settings 없음", "settings missing"))?;
    let (map, curves) = crs_maps_from_lua(settings).ok_or(tr!("settings 형식 오류", "settings format error"))?;
    let title = root.str("title").filter(|t| !t.is_empty()).unwrap_or_else(|| fallback_name.to_string());
    // Use the file name when the title is a localization key ("$$$/...")
    let name = if title.starts_with("$$$") { fallback_name.to_string() } else { title };
    let (mut s, mut g, mut warnings) = from_crs(&map, &curves);
    let (masks, mw) = masks_from_lua(settings);
    if !masks.is_empty() {
        s.masks = masks;
        g.masks = true;
    }
    warnings.extend(mw);
    if !groups_any(&g) {
        return Err(tr!("가져올 현상 설정이 없음", "No develop settings to import").into());
    }
    Ok(ImportedPreset { group: fallback_group.to_string(), name, settings: s, groups: g, warnings })
}

/// Lua settings table (crs name = value) → value map + tone curve points.
pub fn crs_maps_from_lua(settings: &Lua) -> Option<(HashMap<String, String>, HashMap<String, Vec<[f32; 2]>>)> {
    let Lua::Table(items) = settings else { return None };
    let mut map = HashMap::new();
    let mut curves = HashMap::new();
    for (k, v) in items {
        let Some(k) = k else { continue };
        match v {
            Lua::Table(arr) if k.starts_with("ToneCurvePV2012") => {
                let nums: Vec<f32> = arr.iter().filter_map(|(_, x)| if let Lua::Num(n) = x { Some(*n as f32) } else { None }).collect();
                let pts: Vec<[f32; 2]> = nums.as_chunks::<2>().0.iter().map(|c| [c[0] / 255.0, c[1] / 255.0]).collect();
                if pts.len() >= 2 {
                    curves.insert(k.clone(), pts);
                }
            }
            Lua::Table(_) if k == "Look" => {
                // Look profile (looks are stored here, not in CameraProfile)
                if let Some(n) = v.str("Name").filter(|n| !n.is_empty()) {
                    map.insert("LookName".into(), n);
                }
                if let Some(a) = v.str("Amount") {
                    map.insert("LookAmount".into(), a);
                }
            }
            other => {
                if let Some(sv) = other.as_string() {
                    map.insert(k.clone(), sv);
                }
            }
        }
    }
    Some((map, curves))
}

/// Preset local adjustments → Darkroom masks.
/// Handles both the new (MaskGroupBasedCorrections) and old (Gradient/CircularGradient/PaintBasedCorrections) forms.
/// Coordinates are normalized to the original image. AI masks (subject, sky, ...) that cannot be rebuilt are skipped.
pub fn masks_from_lua(settings: &Lua) -> (Vec<Mask>, Vec<String>) {
    let mut out = Vec::new();
    let mut warn = Vec::new();
    let mut skipped_ai = 0;
    for key in ["MaskGroupBasedCorrections", "GradientBasedCorrections", "CircularGradientBasedCorrections", "PaintBasedCorrections"] {
        let Some(Lua::Table(list)) = settings.get(key) else { continue };
        for (_, corr) in list {
            if corr.bool("CorrectionActive") == Some(false) {
                continue;
            }
            let mut m = Mask { id: out.len() as u64 + 1, ..Default::default() };
            m.name = corr.str("CorrectionName").filter(|n| !n.is_empty()).unwrap_or_else(|| trf!("마스크 {}", "Mask {}", out.len() + 1));
            m.amount = corr.num("CorrectionAmount").unwrap_or(1.0) as f32;
            m.adj = local_adjust(corr);
            if let Some(Lua::Table(masks)) = corr.get("CorrectionMasks") {
                for (_, cm) in masks {
                    match mask_component(cm) {
                        Some(c) => m.components.push(c),
                        None => {
                            let what = cm.str("What").unwrap_or_default();
                            if what.contains("Image") || what.contains("AI") || cm.get("MaskSubType").is_some() && what != "Mask/Paint" {
                                skipped_ai += 1;
                            }
                        }
                    }
                }
            }
            if !m.components.is_empty() {
                out.push(m);
            }
        }
    }
    if skipped_ai > 0 {
        warn.push(trf!("AI 마스크 {skipped_ai}개는 가져오지 않음 (지원하지 않는 종류)", "{skipped_ai} AI masks not imported (unsupported type)"));
    }
    (out, warn)
}

/// Preset spot removal → Darkroom spots. Handles both new RetouchAreas and old RetouchInfo strings.
pub fn spots_from_lua(settings: &Lua) -> Vec<Spot> {
    let mut out = Vec::new();
    if let Some(Lua::Table(list)) = settings.get("RetouchAreas") {
        for (_, a) in list {
            let Some(Lua::Table(masks)) = a.get("Masks") else { continue };
            let Some((_, m)) = masks.first() else { continue };
            let (Some(cx), Some(cy)) = (m.num("CenterX"), m.num("CenterY")) else { continue };
            let r = m.num("Radius").unwrap_or(0.01);
            let sx = a.num("SourceX").unwrap_or(cx + r * 2.0);
            let sy = cy + a.num("OffsetY").unwrap_or(0.0);
            out.push(Spot {
                dst: [cx as f32, cy as f32],
                src: [sx as f32, sy as f32],
                radius: r as f32,
                feather: a.num("Feather").unwrap_or(0.5) as f32,
                opacity: a.num("Opacity").unwrap_or(1.0) as f32,
                heal: a.str("SpotType").map(|t| t != "clone").unwrap_or(true),
                path: Vec::new(),
                // Content-aware removal (SpotType "heal_patchmatch") maps to our erase tool; the patch is computed when opened in Develop
                remove: a.str("SpotType").map(|t| t == "heal_patchmatch" || t.to_lowercase().contains("remove")).unwrap_or(false),
                fill: None,
            });
        }
    }
    if let Some(Lua::Table(list)) = settings.get("RetouchInfo") {
        for (_, v) in list {
            let Lua::Str(text) = v else { continue };
            let kv: HashMap<String, String> = text
                .split(',')
                .filter_map(|p| {
                    let mut it = p.splitn(2, '=');
                    Some((it.next()?.trim().to_string(), it.next()?.trim().to_string()))
                })
                .collect();
            let f = |k: &str| kv.get(k).and_then(|v| v.parse::<f32>().ok());
            let (Some(cx), Some(cy)) = (f("centerX"), f("centerY")) else { continue };
            let r = f("radius").unwrap_or(0.01);
            out.push(Spot {
                dst: [cx, cy],
                src: [f("sourceX").unwrap_or(cx + r * 2.0), f("sourceY").unwrap_or(cy)],
                radius: r,
                feather: 0.5,
                opacity: 1.0,
                heal: kv.get("spotType").map(|t| t != "clone").unwrap_or(true),
                path: Vec::new(),
                remove: false,
                fill: None,
            });
        }
    }
    out
}

fn local_adjust(c: &Lua) -> LocalAdjust {
    // Local values are -1..1 (exposure -1..1 = -4..4 EV)
    let n = |k: &str| c.num(k).unwrap_or(0.0) as f32;
    let pick = |a: &str, b: &str| if c.get(a).is_some() { n(a) } else { n(b) };
    LocalAdjust {
        temp: n("LocalTemperature") * 100.0,
        tint: n("LocalTint") * 100.0,
        exposure: pick("LocalExposure2012", "LocalExposure") * 4.0,
        contrast: pick("LocalContrast2012", "LocalContrast") * 100.0,
        highlights: n("LocalHighlights2012") * 100.0,
        shadows: n("LocalShadows2012") * 100.0,
        whites: n("LocalWhites2012") * 100.0,
        blacks: n("LocalBlacks2012") * 100.0,
        texture: n("LocalTexture") * 100.0,
        clarity: pick("LocalClarity2012", "LocalClarity") * 100.0,
        dehaze: n("LocalDehaze") * 100.0,
        hue: n("LocalHue") * 180.0,
        saturation: n("LocalSaturation") * 100.0,
        sharpness: n("LocalSharpness") * 100.0,
        noise: n("LocalLuminanceNoise") * 100.0,
    }
}

fn mask_component(cm: &Lua) -> Option<MaskComponent> {
    let what = cm.str("What").unwrap_or_default();
    let n = |k: &str| cm.num(k).map(|v| v as f32);
    // New-style mask combine mode: 0 = add, 1 = intersect (subtract is stored as invert + intersect)
    let inverted = cm.bool("MaskInverted").unwrap_or(false);
    let blend = cm.num("MaskBlendMode").unwrap_or(0.0) as i32;
    let op = match blend {
        1 if inverted => MaskOp::Subtract,
        1 => MaskOp::Intersect,
        _ => MaskOp::Add,
    };
    let invert = inverted && op != MaskOp::Subtract;
    let shape = match what.as_str() {
        "Mask/Gradient" => {
            let (zx, zy, fx, fy) = (n("ZeroX")?, n("ZeroY")?, n("FullX")?, n("FullY")?);
            MaskShape::Linear { p0: [fx, fy], p1: [zx, zy] }
        }
        "Mask/CircularGradient" => {
            let (t, l, b, r) = (n("Top")?, n("Left")?, n("Bottom")?, n("Right")?);
            let flipped = cm.bool("Flipped").unwrap_or(false);
            let shape = MaskShape::Radial {
                center: [(l + r) * 0.5, (t + b) * 0.5],
                radius: [((r - l) * 0.5).abs(), ((b - t) * 0.5).abs()],
                angle: n("Angle").unwrap_or(0.0),
                feather: n("Feather").unwrap_or(50.0),
            };
            // Flipped = apply outside the ellipse (old-style default)
            return Some(MaskComponent { op, invert: invert ^ flipped, shape });
        }
        "Mask/Paint" => {
            let radius = n("Radius").unwrap_or(0.05);
            let flow = n("Flow").unwrap_or(1.0);
            let feather = n("CenterWeight").map(|c| 1.0 - c).unwrap_or(0.5);
            let erase = n("MaskValue").map(|v| v <= 0.0).unwrap_or(false);
            let mut pts = Vec::new();
            if let Some(Lua::Table(d)) = cm.get("Dabs") {
                for (_, x) in d {
                    if let Lua::Str(s) = x {
                        // "d 0.512 0.334"
                        let v: Vec<f32> = s.split_whitespace().skip(1).filter_map(|t| t.parse().ok()).collect();
                        if v.len() >= 2 {
                            pts.push([v[0], v[1]]);
                        }
                    }
                }
            }
            if pts.is_empty() {
                return None;
            }
            MaskShape::Brush { strokes: vec![BrushStroke { points: pts, size: radius, feather: feather.clamp(0.0, 1.0), flow: flow.clamp(0.0, 1.0), erase }] }
        }
        "Mask/RangeMask" => {
            let rm = cm.get("CorrectionRangeMask")?;
            let t = rm.num("Type").unwrap_or(0.0) as i32;
            if t == 2 {
                // Luminance range: LumRange "lo hi lo2 hi2" (0..1)
                let lr = rm.str("LumRange").unwrap_or_default();
                let v: Vec<f32> = lr.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                let (lo, hi) = if v.len() >= 4 { ((v[0] + v[1]) * 0.5, (v[2] + v[3]) * 0.5) } else { (0.0, 1.0) };
                MaskShape::Luminance { lo, hi, smooth: rm.num("LumFeather").unwrap_or(0.5) as f32 }
            } else {
                return None;
            }
        }
        // AI masks: only the kind is imported; the bitmap is recomputed for this photo when opened in Develop
        // (MaskSubType 1 = subject, 2 = sky, 0 = category: 22 background, others people/objects)
        "Mask/Image" => {
            use crate::imaging::aimask::AiKind;
            let kind = match (cm.num("MaskSubType").unwrap_or(-1.0) as i32, cm.num("MaskSubCategoryID").unwrap_or(0.0) as i32) {
                (1, _) => AiKind::Subject,
                (2, _) => AiKind::Sky,
                (0, 22) => AiKind::Background,
                (0, _) => AiKind::People,
                _ => return None,
            };
            MaskShape::Ai { kind, key: String::new(), src: String::new() }
        }
        _ => return None,
    };
    Some(MaskComponent { op, invert, shape })
}

// ───────────────────────── Watermark ─────────────────────────

pub fn parse_lr_watermark(text: &str, fallback_name: &str) -> Result<(Watermark, Vec<String>), String> {
    let root = parse_lua(text).ok_or(tr!("Lua 형식 파싱 실패", "Lua parsing failed"))?;
    if root.str("type").as_deref() != Some("WatermarkingPreset") {
        return Err(tr!("워터마크 프리셋이 아님", "Not a watermark preset").into());
    }
    let item = root.get("value").and_then(|v| v.get("items")).and_then(|t| if let Lua::Table(v) = t { v.first().map(|x| x.1.clone()) } else { None }).ok_or(tr!("항목 없음", "No items"))?;
    let mut w = Watermark { name: root.str("title").unwrap_or_else(|| fallback_name.to_string()), ..Default::default() };
    let mut warn = Vec::new();
    w.kind = if item.str("kind").as_deref() == Some("image") { WmKind::Graphic } else { WmKind::Text };
    if let Some(t) = item.str("text") {
        w.text = t;
    }
    if let Some(p) = item.str("imagePath") {
        if w.kind == WmKind::Graphic && !Path::new(&p).exists() {
            warn.push(trf!("이미지 파일이 없음: {p}", "Image file not found: {p}"));
        }
        w.image_path = p;
    }
    let f = |k: &str| item.num(k).map(|v| v as f32);
    if let Some(o) = f("opacity") {
        w.opacity = (o * 100.0).clamp(0.0, 100.0);
    }
    if let (Some(r), Some(g), Some(b)) = (f("textColorRed"), f("textColorGreen"), f("textColorBlue")) {
        w.color = [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8];
    }
    if let Some(font) = item.str("textFontName") {
        let base = font.split('-').next().unwrap_or(&font).to_lowercase();
        match crate::fonts::system_fonts().iter().find(|x| x.name.to_lowercase() == base || x.name.to_lowercase().starts_with(&base)) {
            Some(fe) => {
                w.font_name = fe.name.clone();
                w.font_path = fe.path.to_string_lossy().to_string();
                w.font_index = fe.index;
            }
            None => {
                if w.kind == WmKind::Text {
                    warn.push(trf!("글꼴 '{font}' 없음 → 기본 글꼴 사용", "Font '{font}' not found → using default font"));
                }
            }
        }
    }
    w.align = match item.str("textHorizontalAlignment").as_deref() {
        Some("left") => WmAlign::Left,
        Some("right") => WmAlign::Right,
        _ => WmAlign::Center,
    };
    if let Some(b) = item.bool("textShadowEnabled") {
        w.shadow = b;
    }
    if let Some(v) = f("textShadowOpacity") {
        w.shadow_opacity = v * 100.0;
    }
    if let Some(v) = f("textShadowOffset") {
        w.shadow_offset = v * 100.0;
    }
    if let Some(v) = f("textShadowRadius") {
        w.shadow_radius = v * 100.0;
    }
    if let Some(v) = f("textShadowAngle") {
        let mut a = v;
        while a > 180.0 {
            a -= 360.0;
        }
        w.shadow_angle = a;
    }
    w.size_mode = match item.str("sizeKind").as_deref() {
        Some("fit") => WmSize::Fit,
        Some("fill") => WmSize::Fill,
        _ => WmSize::Proportional,
    };
    if let Some(v) = f("sizeRelativeAmount") {
        w.size = (v * 100.0).clamp(1.0, 100.0);
    }
    // Preset margins are a fraction of the image size; Darkroom insets are % of half the size
    if let Some(v) = f("horizontalPadding") {
        w.inset_h = (v * 200.0).clamp(0.0, 100.0);
    }
    if let Some(v) = f("verticalPadding") {
        w.inset_v = (v * 200.0).clamp(0.0, 100.0);
    }
    w.anchor = match item.str("anchor").unwrap_or_default().to_lowercase().as_str() {
        "topleft" => 0,
        "top" => 1,
        "topright" => 2,
        "left" => 3,
        "center" => 4,
        "right" => 5,
        "bottomleft" => 6,
        "bottom" => 7,
        _ => 8,
    };
    w.rotation = match item.str("orientation").as_deref() {
        Some("BC") => 1,
        Some("CD") => 2,
        Some("DA") => 3,
        _ => 0,
    };
    Ok((w, warn))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:WhiteBalance="Custom" crs:Temperature="5450" crs:Tint="+14"
   crs:Exposure2012="+0.45" crs:Contrast2012="+54" crs:Highlights2012="-58" crs:Shadows2012="-62"
   crs:Whites2012="-24" crs:Blacks2012="+17" crs:Clarity2012="+37" crs:Vibrance="-16" crs:Saturation="-23"
   crs:HueAdjustmentOrange="-100" crs:SaturationAdjustmentRed="-42" crs:LuminanceAdjustmentPurple="-85"
   crs:SplitToningShadowHue="37" crs:SplitToningShadowSaturation="14" crs:SplitToningHighlightHue="215"
   crs:ColorGradeBlending="100" crs:ConvertToGrayscale="False" crs:RedHue="+10" crs:HasSettings="True">
   <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">FK 1</rdf:li></rdf:Alt></crs:Name>
   <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">BETH_FUJI_KODAK</rdf:li></rdf:Alt></crs:Group>
   <crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 11</rdf:li><rdf:li>255, 250</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
   <crs:ToneCurvePV2012Red><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>73, 56</rdf:li><rdf:li>255, 253</rdf:li></rdf:Seq></crs:ToneCurvePV2012Red>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn xmp_maps_values() {
        let p = parse_xmp(XMP, "g", "n").unwrap();
        assert_eq!(p.name, "FK 1");
        assert_eq!(p.group, "BETH_FUJI_KODAK");
        let s = &p.settings;
        assert!((s.exposure - 0.45).abs() < 1e-4);
        assert_eq!(s.contrast, 54.0);
        assert_eq!(s.highlights, -58.0);
        assert_eq!(s.blacks, 17.0);
        assert_eq!(s.clarity, 37.0);
        assert_eq!(s.hsl[1].hue, -100.0);
        assert_eq!(s.hsl[0].sat, -42.0);
        assert_eq!(s.hsl[6].lum, -85.0);
        assert_eq!(s.grading.shadows.hue, 37.0);
        assert_eq!(s.grading.shadows.sat, 14.0);
        assert_eq!(s.grading.highlights.hue, 215.0);
        assert_eq!(s.calibration.red_hue, 10.0);
        assert_eq!(s.curve.rgb.len(), 2);
        assert!((s.curve.rgb[0][1] - 11.0 / 255.0).abs() < 1e-4);
        assert_eq!(s.curve.red.len(), 3);
        assert!(p.groups.basic_tone && p.groups.hsl && p.groups.tone_curve && p.groups.white_balance);
        assert!(!p.groups.detail && !p.groups.crop);
        assert!(s.wb_custom && (s.temp_k - 5450.0).abs() < 0.1 && (s.tint_k - 14.0).abs() < 0.1);
    }

    #[test]
    fn lrtemplate_develop() {
        let t = r#"s = {
	id = "X", internalName = "Warm", title = "따뜻하게", type = "Develop",
	value = { settings = { Exposure2012 = 0.3, Contrast2012 = 20, ConvertToGrayscale = false,
		ToneCurvePV2012 = { 0, 0, 128, 140, 255, 255, }, HueAdjustmentRed = -5, Sharpness = 40, },
		uuid = "Y", },
	version = 0,
}"#;
        let p = parse_lrtemplate(t, "그룹", "파일").unwrap();
        assert_eq!(p.name, "따뜻하게");
        assert!((p.settings.exposure - 0.3).abs() < 1e-5);
        assert_eq!(p.settings.curve.rgb.len(), 3);
        assert_eq!(p.settings.hsl[0].hue, -5.0);
        assert!(p.groups.detail && p.groups.treatment);
    }

    #[test]
    fn lr_watermark() {
        let t = r#"s = { id = "4C", internalName = "ORIONNEST", title = "ORIONNEST", type = "WatermarkingPreset",
	value = { items = { { anchor = "bottomright", horizontalPadding = 0.02, imagePath = "F:\\x\\a.png", kind = "image",
		opacity = 0.496, orientation = "AB", sizeKind = "relative", sizeRelativeAmount = 0.031, text = "©OrionNest",
		textColorBlue = 1, textColorGreen = 1, textColorRed = 1, textFontName = "Adobe Clean-Regular",
		textHorizontalAlignment = "left", textShadowAngle = 270, textShadowEnabled = true, textShadowOffset = 0.1,
		textShadowOpacity = 0.8, textShadowRadius = 0.2, verticalPadding = 0.006, }, }, }, version = 0, }"#;
        let (w, warn) = parse_lr_watermark(t, "x").unwrap();
        assert_eq!(w.name, "ORIONNEST");
        assert_eq!(w.kind, WmKind::Graphic);
        assert_eq!(w.image_path, "F:\\x\\a.png");
        assert_eq!(w.anchor, 8);
        assert!((w.opacity - 49.6).abs() < 0.01);
        assert!((w.inset_h - 4.0).abs() < 0.01);
        assert_eq!(w.shadow_angle, -90.0);
        assert_eq!(w.text, "©OrionNest");
        assert!(!warn.is_empty(), "없는 이미지 경고");
    }

    /// Parses every installed preset, if any.
    #[test]
    fn installed_presets_parse() {
        let mut ok = 0;
        let mut errs = Vec::new();
        for d in default_dirs() {
            for f in collect_files(&d, &["xmp", "lrtemplate"]) {
                match import_file(&f) {
                    Ok(_) => ok += 1,
                    Err(e) => errs.push((f, e)),
                }
            }
        }
        eprintln!("설치된 프리셋 {ok}개 성공, {}개 건너뜀", errs.len());
        for (f, e) in errs.iter().take(15) {
            eprintln!("  {} — {e}", f.display());
        }
        if let Some(d) = watermark_dir() {
            for f in collect_files(&d, &["lrtemplate"]) {
                let t = read_text(&f).unwrap();
                let r = parse_lr_watermark(&t, "x");
                eprintln!("워터마크 {}: {:?}", f.display(), r.as_ref().map(|(w, warn)| (&w.name, warn)));
                assert!(r.is_ok());
            }
        }
    }
}

#[cfg(test)]
mod look_xmp_tests {
    /// Sidecar with a look: look parameters (tone curve, clarity) are not user settings
    #[test]
    fn look_parameters_stay_in_look() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Version="17.0" crs:ProcessVersion="15.4" crs:Exposure2012="+0.50" crs:CameraProfile="Adobe Standard" crs:HasSettings="True">
<crs:Look><rdf:Description crs:Name="Adobe Vivid" crs:Amount="0.6"><crs:Parameters><rdf:Description crs:ProcessVersion="10.0" crs:Clarity2012="+10" crs:LookTable="ABC">
<crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>32, 22</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
</rdf:Description></crs:Parameters></rdf:Description></crs:Look>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let p = super::parse_xmp(x, "", "").unwrap();
        assert_eq!(p.settings.exposure, 0.5);
        assert_eq!(p.settings.clarity, 0.0, "룩의 명료도가 사용자 값으로 들어오면 안 됨");
        assert!(p.settings.curve.rgb.len() <= 2, "룩 커브가 사용자 커브로 들어오면 안 됨: {:?}", p.settings.curve.rgb);
        assert_eq!(p.settings.profile, "Adobe Vivid");
        assert!((p.settings.profile_amount - 60.0).abs() < 0.5, "룩 적용량 {}", p.settings.profile_amount);
    }
}
