//! Parameter audit: changes each numeric setting on a real RAW and checks the render changes.
//! Test switch: `DARKROOM_AUDIT_RAW=<path to RAW> cargo test audit -- --ignored --nocapture`
//! Renders both fit-to-size and a 100% center crop so zoom-only effects (sharpening, noise) are caught.

use super::pipeline::{Engine, RenderRequest};
use super::settings::*;
use serde_json::Value;

fn render(e: &mut Engine, src: &super::image::SourceImage, s: &DevelopSettings, full: bool) -> Vec<u8> {
    let (w, h, region) = if full {
        // 100% view: center 600x400 pixels
        let (bw, bh) = (src.width() as f32, src.height() as f32);
        let (rw, rh) = (600.0 / bw, 400.0 / bh);
        (600, 400, [0.5 - rw / 2.0, 0.5 - rh / 2.0, 0.5 + rw / 2.0, 0.5 + rh / 2.0])
    } else {
        let k = 900.0 / src.width().max(src.height()) as f32;
        (((src.width() as f32) * k) as usize, ((src.height() as f32) * k) as usize, [0.0, 0.0, 1.0, 1.0])
    };
    e.render(src, &RenderRequest { settings: s, out_w: w, out_h: h, region, draft: false, clipping: false, mask_overlay: None, keep_float: false }).rgba
}

fn diff(a: &[u8], b: &[u8]) -> (f32, u8) {
    let mut s = 0u64;
    let mut m = 0u8;
    for (x, y) in a.iter().zip(b) {
        let d = x.abs_diff(*y);
        s += d as u64;
        m = m.max(d);
    }
    (s as f32 / a.len() as f32, m)
}

/// Collects paths of numeric leaf settings (lists like masks and spots are handled separately).
fn leaves(v: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, f64)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if ["masks", "privacy", "spots", "red_eye", "version", "points"].contains(&k.as_str()) {
                    continue;
                }
                path.push(k.clone());
                leaves(x, path, out);
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(i.to_string());
                leaves(x, path, out);
                path.pop();
            }
        }
        Value::Number(n) => out.push((path.clone(), n.as_f64().unwrap_or(0.0))),
        _ => {}
    }
}

fn set(v: &mut Value, path: &[String], x: f64) {
    let mut cur = v;
    for p in path {
        cur = match cur {
            Value::Object(m) => m.get_mut(p).unwrap(),
            Value::Array(a) => &mut a[p.parse::<usize>().unwrap()],
            _ => unreachable!(),
        };
    }
    *cur = serde_json::json!(x);
}

/// Value to probe: within the known range when available, otherwise a large offset from the default.
fn probe_value(name: &str, def: f64) -> f64 {
    match name {
        "exposure" => 1.0,
        "temp_k" => 3500.0,
        "tint_k" => 40.0,
        "profile_amount" => 160.0,
        "sharpen_radius" => 2.5,
        "angle" => 8.0,
        "scale" => 80.0,
        "hue" if def == 0.0 => 40.0,
        _ if def == 0.0 => 60.0,
        _ if def >= 50.0 => def - 45.0,
        _ => def + 40.0,
    }
}

/// Settings that only matter when another one is enabled: (name match, setting to enable first).
fn enable(base: &mut DevelopSettings, path: &[String]) {
    let k = path.join(".");
    let d = &mut base.detail;
    if k.starts_with("detail.sharpen_") && k != "detail.sharpen_amount" {
        d.sharpen_amount = 80.0;
    }
    if k.starts_with("detail.nr_luma_") {
        d.nr_luma = 60.0;
    }
    if k.starts_with("detail.nr_color_") {
        d.nr_color = 60.0;
    }
    if k.starts_with("effects.vignette_") && k != "effects.vignette_amount" {
        base.effects.vignette_amount = -60.0;
    }
    if k.starts_with("effects.grain_") && k != "effects.grain_amount" {
        base.effects.grain_amount = 60.0;
    }
    if k == "lens.vignette_midpoint" {
        base.lens.vignette = 60.0;
    }
    if k.starts_with("grading.") && (k.ends_with("blending") || k.ends_with("balance")) {
        base.grading.shadows.sat = 60.0;
        base.grading.shadows.hue = 220.0;
        base.grading.highlights.sat = 60.0;
        base.grading.highlights.hue = 40.0;
    }
    if k.starts_with("grading.") && k.ends_with(".hue") {
        let g = &mut base.grading;
        for w in [&mut g.shadows, &mut g.midtones, &mut g.highlights, &mut g.global] {
            w.sat = 50.0;
        }
    }
    if k.starts_with("bw_mix") {
        base.treatment = Treatment::BlackWhite;
    }
    if k == "temp_k" || k == "tint_k" {
        base.wb_custom = true;
        base.temp_k = 5000.0;
    }
    if k.starts_with("lens.profile_") {
        base.lens.profile_enable = true;
    }
}

#[test]
#[ignore]
fn audit_parameters() {
    let Some(p) = std::env::var_os("DARKROOM_AUDIT_RAW") else { return };
    let src = crate::imaging::decode::decode_source(std::path::Path::new(&p), 1).unwrap();
    let def = DevelopSettings::default_for(src.is_raw);
    let mut e = Engine::default();
    let mut lv = Vec::new();
    leaves(&serde_json::to_value(&def).unwrap(), &mut Vec::new(), &mut lv);
    let mut dead = Vec::new();
    eprintln!("{} numeric settings", lv.len());
    for (path, d) in &lv {
        let k = path.join(".");
        // Skip relative temp/tint (unused for RAW) and crop/aspect geometry.
        if ["temp", "tint"].contains(&k.as_str()) || k.starts_with("geometry.crop") || k.starts_with("geometry.aspect") || k.starts_with("geometry.rotate90") {
            continue;
        }
        let mut base = def.clone();
        enable(&mut base, path);
        let mut v = serde_json::to_value(&base).unwrap();
        let x = probe_value(path.last().unwrap(), *d);
        set(&mut v, path, x);
        let Ok(s2) = serde_json::from_value::<DevelopSettings>(v) else {
            eprintln!("  ?? {k}: 값 설정 실패");
            continue;
        };
        let (fa, fm) = diff(&render(&mut e, &src, &base, false), &render(&mut e, &src, &s2, false));
        let (za, zm) = diff(&render(&mut e, &src, &base, true), &render(&mut e, &src, &s2, true));
        let ok = fa > 0.05 || fm > 3 || za > 0.05 || zm > 3;
        eprintln!("  {} {k} = {x} (기본 {d}): 맞춤 평균 {fa:.2} 최대 {fm} / 100% 평균 {za:.2} 최대 {zm}", if ok { "  " } else { "!!" });
        if !ok {
            dead.push(k);
        }
    }
    // Local mask adjustments, using a centered radial mask.
    let adj_names = ["temp", "tint", "exposure", "contrast", "highlights", "shadows", "whites", "blacks", "texture", "clarity", "dehaze", "hue", "saturation", "sharpness", "noise"];
    for n in adj_names {
        let mut base = def.clone();
        base.masks.push(Mask {
            id: 1,
            name: "m".into(),
            components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape: MaskShape::Radial { center: [0.5, 0.5], radius: [0.3, 0.3], angle: 0.0, feather: 50.0 } }],
            ..Default::default()
        });
        let mut v = serde_json::to_value(&base).unwrap();
        let x = match n {
            "exposure" => 1.0,
            "noise" | "sharpness" => 80.0,
            _ => 60.0,
        };
        v["masks"][0]["adj"][n] = serde_json::json!(x);
        let s2: DevelopSettings = serde_json::from_value(v).unwrap();
        let (fa, fm) = diff(&render(&mut e, &src, &base, false), &render(&mut e, &src, &s2, false));
        let (za, zm) = diff(&render(&mut e, &src, &base, true), &render(&mut e, &src, &s2, true));
        let ok = fa > 0.05 || fm > 3 || za > 0.05 || zm > 3;
        eprintln!("  {} mask.adj.{n} = {x}: 맞춤 평균 {fa:.2} 최대 {fm} / 100% 평균 {za:.2} 최대 {zm}", if ok { "  " } else { "!!" });
        if !ok {
            dead.push(format!("mask.adj.{n}"));
        }
    }
    eprintln!("반응 없음 {}개: {dead:?}", dead.len());
}

/// Defringe removes purple fringes at a white bar's edges but leaves purple objects away from edges alone.
#[test]
fn defringe_targets_edges_only() {
    let (w, h) = (80usize, 40usize);
    let mut rgb = vec![0.1f32; w * h * 3];
    let purple = [0.55f32, 0.2, 0.7];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            if (30..34).contains(&x) {
                rgb[i..i + 3].copy_from_slice(&[0.95, 0.95, 0.95]);
            } else if x == 29 || x == 34 {
                rgb[i..i + 3].copy_from_slice(&purple);
            } else if (52..78).contains(&x) && (6..36).contains(&y) {
                // Purple object away from edges (only its center is compared, since its border is an edge).
                rgb[i..i + 3].copy_from_slice(&purple);
            }
        }
    }
    let orig = rgb.clone();
    super::pipeline::defringe(&mut rgb, w, h, 1.0, 0.0, 1.0);
    let sat = |p: &[f32]| p.iter().cloned().fold(0.0f32, f32::max) - p.iter().cloned().fold(1.0f32, f32::min);
    let at = |b: &[f32], x: usize, y: usize| b[(y * w + x) * 3..(y * w + x) * 3 + 3].to_vec();
    assert!(sat(&at(&rgb, 29, 20)) < sat(&at(&orig, 29, 20)) * 0.3, "fringe kept: {:?}", at(&rgb, 29, 20));
    // Center of the purple object.
    assert!((sat(&at(&rgb, 65, 20)) - sat(&at(&orig, 65, 20))).abs() < 0.02, "object changed: {:?}", at(&rgb, 65, 20));
    // Green-only defringe leaves purple untouched.
    let mut g = orig.clone();
    super::pipeline::defringe(&mut g, w, h, 0.0, 1.0, 1.0);
    assert!((sat(&at(&g, 29, 20)) - sat(&at(&orig, 29, 20))).abs() < 0.02);
}
