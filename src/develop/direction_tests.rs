//! Slider direction checks: every parameter must move the image in the expected direction.
//! Each test checks numerically what a positive value should change and how.

use super::color::{luma, rgb_hue, srgb_decode};
use super::image::{LinearImage, SourceImage};
use super::pipeline::{Engine, RenderRequest};
use super::settings::*;

const W: usize = 240;
const H: usize = 160;

/// Test image: dark gray top left, light gray top right, textured mid gray in the middle,
/// color patches at the bottom (red, blue, green, orange), and a white dot at the right middle.
fn source(is_raw: bool) -> SourceImage {
    let mut img = LinearImage::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let (r, g, b) = if y < 50 {
                if x < 80 {
                    (0.12, 0.12, 0.12)
                } else if x < 160 {
                    // Textured mid gray (for clarity, texture and noise measurements)
                    let t = if (x / 3 + y / 3) % 2 == 0 { 0.40 } else { 0.55 };
                    (t, t, t)
                } else {
                    (0.85, 0.85, 0.85)
                }
            } else if y < 100 {
                // Horizontal brightness ramp
                let v = x as f32 / (W - 1) as f32;
                (v, v, v)
            } else if x < 60 {
                (0.75, 0.18, 0.15)
            } else if x < 120 {
                (0.15, 0.25, 0.75)
            } else if x < 180 {
                (0.2, 0.65, 0.2)
            } else {
                (0.85, 0.5, 0.15)
            };
            let i = (y * W + x) * 3;
            img.data[i] = srgb_decode(r);
            img.data[i + 1] = srgb_decode(g);
            img.data[i + 2] = srgb_decode(b);
        }
    }
    SourceImage::new(img, is_raw, 32)
}

fn render(s: &DevelopSettings) -> Vec<f32> {
    let src = source(false);
    let mut e = Engine::default();
    e.render(
        &src,
        &RenderRequest { settings: s, out_w: W, out_h: H, region: [0.0, 0.0, 1.0, 1.0], draft: false, clipping: false, mask_overlay: None, keep_float: true },
    )
    .float
    .unwrap()
}

fn mean(img: &[f32], x0: usize, y0: usize, x1: usize, y1: usize) -> [f32; 3] {
    let mut a = [0.0f32; 3];
    let mut n = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            for c in 0..3 {
                a[c] += img[(y * W + x) * 3 + c];
            }
            n += 1.0;
        }
    }
    [a[0] / n, a[1] / n, a[2] / n]
}

fn lum(p: [f32; 3]) -> f32 {
    luma(p[0], p[1], p[2])
}

fn std_l(img: &[f32], x0: usize, y0: usize, x1: usize, y1: usize) -> f32 {
    let m = lum(mean(img, x0, y0, x1, y1));
    let mut s = 0.0;
    let mut n = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * W + x) * 3;
            let l = luma(img[i], img[i + 1], img[i + 2]);
            s += (l - m) * (l - m);
            n += 1.0;
        }
    }
    (s / n).sqrt()
}

fn chroma(p: [f32; 3]) -> f32 {
    p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])
}

// Regions
fn dark(i: &[f32]) -> [f32; 3] {
    mean(i, 10, 10, 70, 40)
}
fn mid(i: &[f32]) -> [f32; 3] {
    mean(i, 90, 10, 150, 40)
}
fn bright(i: &[f32]) -> [f32; 3] {
    mean(i, 170, 10, 230, 40)
}
fn red(i: &[f32]) -> [f32; 3] {
    mean(i, 10, 110, 50, 150)
}
fn blue(i: &[f32]) -> [f32; 3] {
    mean(i, 70, 110, 110, 150)
}
fn ramp_at(i: &[f32], t: f32) -> f32 {
    let x = (t * (W - 1) as f32) as usize;
    lum(mean(i, x.saturating_sub(2), 60, (x + 3).min(W), 90))
}

fn base() -> DevelopSettings {
    DevelopSettings::default()
}

/// Helper that checks the change relative to the baseline
fn check(name: &str, cond: bool, detail: String) {
    assert!(cond, "[{name}] 방향이 Lightroom과 다름: {detail}");
}

#[test]
fn temp_plus_is_warmer() {
    let a = mid(&render(&base()));
    let b = mid(&render(&DevelopSettings { temp: 40.0, ..base() }));
    check("색온도", b[0] - b[2] > a[0] - a[2] + 0.02, format!("{a:?} → {b:?}"));
}

#[test]
fn tint_plus_is_magenta() {
    let a = mid(&render(&base()));
    let b = mid(&render(&DevelopSettings { tint: 40.0, ..base() }));
    let ga = a[1] - (a[0] + a[2]) * 0.5;
    let gb = b[1] - (b[0] + b[2]) * 0.5;
    check("색조", gb < ga - 0.01, format!("{ga} → {gb}"));
}

#[test]
fn exposure_plus_is_brighter() {
    let a = lum(mid(&render(&base())));
    let b = lum(mid(&render(&DevelopSettings { exposure: 1.0, ..base() })));
    check("노출", b > a + 0.05, format!("{a} → {b}"));
}

#[test]
fn contrast_plus_spreads_tones() {
    let a = render(&base());
    let b = render(&DevelopSettings { contrast: 60.0, ..base() });
    check("대비", lum(dark(&b)) < lum(dark(&a)) - 0.01, format!("어두운 영역 {} → {}", lum(dark(&a)), lum(dark(&b))));
    check("대비", lum(bright(&b)) > lum(bright(&a)) + 0.01, format!("밝은 영역 {} → {}", lum(bright(&a)), lum(bright(&b))));
}

#[test]
fn highlights_minus_darkens_highlights() {
    let a = render(&base());
    let b = render(&DevelopSettings { highlights: -80.0, ..base() });
    check("하이라이트", lum(bright(&b)) < lum(bright(&a)) - 0.02, format!("{} → {}", lum(bright(&a)), lum(bright(&b))));
    let c = render(&DevelopSettings { highlights: 80.0, ..base() });
    check("하이라이트+", lum(bright(&c)) > lum(bright(&a)), format!("{} → {}", lum(bright(&a)), lum(bright(&c))));
}

#[test]
fn shadows_plus_lifts_shadows() {
    let a = render(&base());
    let b = render(&DevelopSettings { shadows: 80.0, ..base() });
    check("섀도", lum(dark(&b)) > lum(dark(&a)) + 0.02, format!("{} → {}", lum(dark(&a)), lum(dark(&b))));
}

#[test]
fn whites_plus_brightens_top() {
    let a = render(&base());
    let b = render(&DevelopSettings { whites: 80.0, ..base() });
    check("화이트", ramp_at(&b, 0.85) > ramp_at(&a, 0.85) + 0.01, format!("{} → {}", ramp_at(&a, 0.85), ramp_at(&b, 0.85)));
}

#[test]
fn blacks_plus_lifts_bottom() {
    let a = render(&base());
    let b = render(&DevelopSettings { blacks: 80.0, ..base() });
    check("블랙", ramp_at(&b, 0.08) > ramp_at(&a, 0.08) + 0.01, format!("{} → {}", ramp_at(&a, 0.08), ramp_at(&b, 0.08)));
}

#[test]
fn clarity_and_texture_plus_add_local_contrast() {
    let a = std_l(&render(&base()), 90, 10, 150, 40);
    let c = std_l(&render(&DevelopSettings { clarity: 80.0, ..base() }), 90, 10, 150, 40);
    let t = std_l(&render(&DevelopSettings { texture: 80.0, ..base() }), 90, 10, 150, 40);
    check("명료도", c > a * 1.03, format!("{a} → {c}"));
    check("텍스처", t > a * 1.03, format!("{a} → {t}"));
    let cm = std_l(&render(&DevelopSettings { clarity: -80.0, ..base() }), 90, 10, 150, 40);
    check("명료도-", cm < a, format!("{a} → {cm}"));
}

#[test]
fn dehaze_plus_removes_haze() {
    let a = render(&base());
    let b = render(&DevelopSettings { dehaze: 60.0, ..base() });
    check("디헤이즈", lum(dark(&b)) < lum(dark(&a)), format!("{} → {}", lum(dark(&a)), lum(dark(&b))));
}

#[test]
fn vibrance_saturation_plus_add_color() {
    let a = chroma(blue(&render(&base())));
    let v = chroma(blue(&render(&DevelopSettings { vibrance: 60.0, ..base() })));
    let s = chroma(blue(&render(&DevelopSettings { saturation: 60.0, ..base() })));
    check("바이브런스", v > a + 0.01, format!("{a} → {v}"));
    check("채도", s > a + 0.01, format!("{a} → {s}"));
}

#[test]
fn parametric_curve_regions() {
    let a = render(&base());
    let mut s = base();
    s.curve.highlights = 80.0;
    check("커브 하이라이트", ramp_at(&render(&s), 0.88) > ramp_at(&a, 0.88), String::new());
    let mut s = base();
    s.curve.lights = 80.0;
    check("커브 밝은 영역", ramp_at(&render(&s), 0.62) > ramp_at(&a, 0.62), String::new());
    let mut s = base();
    s.curve.darks = 80.0;
    check("커브 어두운 영역", ramp_at(&render(&s), 0.38) > ramp_at(&a, 0.38), String::new());
    let mut s = base();
    s.curve.shadows = 80.0;
    check("커브 섀도", ramp_at(&render(&s), 0.12) > ramp_at(&a, 0.12), String::new());
}

#[test]
fn point_curve_raise_brightens() {
    let a = render(&base());
    let mut s = base();
    s.curve.rgb = vec![[0.0, 0.0], [0.5, 0.65], [1.0, 1.0]];
    check("포인트 커브", ramp_at(&render(&s), 0.5) > ramp_at(&a, 0.5) + 0.05, String::new());
    let mut s = base();
    s.curve.red = vec![[0.0, 0.0], [0.5, 0.65], [1.0, 1.0]];
    let m = mid(&render(&s));
    check("R 커브", m[0] > m[2] + 0.03, format!("{m:?}"));
}

fn hue_of(p: [f32; 3]) -> f32 {
    rgb_hue(p[0], p[1], p[2]).0
}

/// Angle difference (-180..180)
fn dh(a: f32, b: f32) -> f32 {
    let mut d = b - a;
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d
}

#[test]
fn hsl_hue_plus_moves_along_wheel() {
    // Red hue + shifts toward orange (hue angle increases), blue hue + toward purple (increases)
    let a = render(&base());
    let mut s = base();
    s.hsl[0].hue = 80.0;
    let b = render(&s);
    check("HSL 빨강 색조", dh(hue_of(red(&a)), hue_of(red(&b))) > 3.0, format!("{} → {}", hue_of(red(&a)), hue_of(red(&b))));
    let mut s = base();
    s.hsl[5].hue = 80.0;
    let c = render(&s);
    check("HSL 파랑 색조", dh(hue_of(blue(&a)), hue_of(blue(&c))) > 3.0, format!("{} → {}", hue_of(blue(&a)), hue_of(blue(&c))));
}

#[test]
fn hsl_sat_lum_plus() {
    let a = render(&base());
    let mut s = base();
    s.hsl[0].sat = 80.0;
    check("HSL 채도", chroma(red(&render(&s))) > chroma(red(&a)), String::new());
    let mut s = base();
    s.hsl[0].lum = 80.0;
    check("HSL 광도", lum(red(&render(&s))) > lum(red(&a)), String::new());
    let mut s = base();
    s.hsl[0].sat = -100.0;
    check("HSL 채도 -100", chroma(red(&render(&s))) < 0.05, String::new());
}

#[test]
fn bw_mix_plus_brightens_band() {
    let mut s = base();
    s.treatment = Treatment::BlackWhite;
    let a = render(&s);
    s.bw_mix[0] = 80.0;
    let b = render(&s);
    check("흑백 빨강", lum(red(&b)) > lum(red(&a)) + 0.01, format!("{} → {}", lum(red(&a)), lum(red(&b))));
    let p = red(&b);
    check("흑백 무채색", chroma(p) < 0.01, format!("{p:?}"));
}

#[test]
fn color_grading_wheels() {
    let mut s = base();
    s.grading.shadows = Wheel { hue: 220.0, sat: 60.0, lum: 0.0 };
    let d = dark(&render(&s));
    check("섀도 휠(파랑)", d[2] > d[0] + 0.01, format!("{d:?}"));
    let mut s = base();
    s.grading.highlights = Wheel { hue: 40.0, sat: 60.0, lum: 0.0 };
    let b = bright(&render(&s));
    check("하이라이트 휠(주황)", b[0] > b[2] + 0.01, format!("{b:?}"));
    let mut s = base();
    s.grading.global = Wheel { hue: 0.0, sat: 0.0, lum: 60.0 };
    check("전체 광도", lum(mid(&render(&s))) > lum(mid(&render(&base()))), String::new());
}

#[test]
fn calibration_directions() {
    let a = render(&base());
    let mut s = base();
    s.calibration.red_hue = 80.0;
    let b = render(&s);
    // Red primary hue + shifts toward yellow (hue angle increases)
    check("캘리브레이션 빨강 색조", dh(hue_of(red(&a)), hue_of(red(&b))) > 2.0, format!("{} → {}", hue_of(red(&a)), hue_of(red(&b))));
    let mut s = base();
    s.calibration.red_sat = 80.0;
    check("캘리브레이션 빨강 채도", chroma(red(&render(&s))) > chroma(red(&a)), String::new());
    let mut s = base();
    s.calibration.shadows_tint = 80.0;
    let d = dark(&render(&s));
    check("섀도 색조(+ = 마젠타)", d[1] < (d[0] + d[2]) * 0.5 - 0.005, format!("{d:?}"));
}

#[test]
fn post_crop_vignette() {
    let a = render(&base());
    let b = render(&DevelopSettings { effects: Effects { vignette_amount: -80.0, ..Default::default() }, ..base() });
    let corner = |i: &[f32]| lum(mean(i, 0, 0, 12, 8));
    check("비네팅 -", corner(&b) < corner(&a) - 0.01, format!("{} → {}", corner(&a), corner(&b)));
    let c = render(&DevelopSettings { effects: Effects { vignette_amount: 80.0, ..Default::default() }, ..base() });
    check("비네팅 +", corner(&c) > corner(&a) + 0.01, format!("{} → {}", corner(&a), corner(&c)));
}

#[test]
fn lens_vignette_plus_brightens_corners() {
    let a = render(&base());
    let b = render(&DevelopSettings { lens: LensCorrection { vignette: 80.0, ..Default::default() }, ..base() });
    let corner = |i: &[f32]| lum(mean(i, 0, 0, 12, 8));
    check("렌즈 비네팅", corner(&b) > corner(&a), format!("{} → {}", corner(&a), corner(&b)));
}

#[test]
fn grain_and_sharpen() {
    let a = std_l(&render(&base()), 10, 10, 70, 40);
    let g = std_l(&render(&DevelopSettings { effects: Effects { grain_amount: 60.0, ..Default::default() }, ..base() }), 10, 10, 70, 40);
    check("그레인", g > a + 0.002, format!("{a} → {g}"));
    let s0 = std_l(&render(&base()), 90, 10, 150, 40);
    let mut s = base();
    s.detail.sharpen_amount = 120.0;
    let s1 = std_l(&render(&s), 90, 10, 150, 40);
    check("샤프닝", s1 > s0, format!("{s0} → {s1}"));
    let mut s = base();
    s.detail.nr_luma = 80.0;
    let n1 = std_l(&render(&s), 90, 10, 150, 40);
    check("휘도 노이즈 감소", n1 < s0, format!("{s0} → {n1}"));
}

#[test]
fn angle_plus_rotates_clockwise() {
    use super::geometry::GeoMap;
    // Angle + rotates clockwise. The output's right-middle dot must come from above it in the source (before the clockwise turn).
    let g = Geometry { angle: 10.0, ..Default::default() };
    let m = GeoMap::new(200, 200, &g, &LensCorrection::default(), [0.0, 0.0, 1.0, 1.0], 200, 200);
    let (_, by) = m.map(190.0, 100.0);
    check("회전 각도", by < 100.0, format!("원본 y={by}"));
}

#[test]
fn vertical_minus_widens_top() {
    use super::geometry::GeoMap;
    // Vertical - widens the top (corrects buildings narrowing upward). The output's top corners sample further inside the source.
    let g = Geometry { vertical: -50.0, ..Default::default() };
    let m = GeoMap::new(200, 200, &g, &LensCorrection::default(), [0.0, 0.0, 1.0, 1.0], 200, 200);
    let (tx, _) = m.map(10.0, 5.0);
    let (bx, _) = m.map(10.0, 195.0);
    check("수직 변형", tx > bx, format!("위 {tx} / 아래 {bx}"));
}

#[test]
fn horizontal_minus_widens_left() {
    use super::geometry::GeoMap;
    // Horizontal - enlarges the left side (perspective toward the right). The output's top-left corner samples further inside the source.
    let g = Geometry { horizontal: -50.0, ..Default::default() };
    let m = GeoMap::new(200, 200, &g, &LensCorrection::default(), [0.0, 0.0, 1.0, 1.0], 200, 200);
    let (_, ly) = m.map(5.0, 10.0);
    let (_, ry) = m.map(195.0, 10.0);
    check("수평 변형", ly > ry, format!("왼쪽 y {ly} / 오른쪽 y {ry}"));
}

#[test]
fn distortion_plus_fixes_barrel() {
    use super::geometry::GeoMap;
    // Distortion + corrects barrel distortion, pushing corner content outward (output corners sample further inside the source)
    let l = LensCorrection { distortion: 60.0, ..Default::default() };
    let m = GeoMap::new(200, 200, &Geometry::default(), &l, [0.0, 0.0, 1.0, 1.0], 200, 200);
    let (x, _) = m.map(199.0, 100.0);
    check("왜곡", x < 199.0, format!("원본 x={x}"));
}

#[test]
fn local_mask_directions() {
    let mut s = base();
    s.masks.push(Mask {
        components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape: MaskShape::All }],
        adj: LocalAdjust { exposure: 1.0, temp: 40.0, saturation: 50.0, ..Default::default() },
        ..Default::default()
    });
    let a = render(&base());
    let b = render(&s);
    check("마스크 노출", lum(mid(&b)) > lum(mid(&a)) + 0.05, String::new());
    let m = mid(&b);
    check("마스크 색온도", m[0] > m[2], format!("{m:?}"));
    check("마스크 채도", chroma(blue(&b)) > chroma(blue(&a)), String::new());
    let mut s = base();
    s.masks.push(Mask {
        components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape: MaskShape::All }],
        adj: LocalAdjust { highlights: -80.0, shadows: 80.0, ..Default::default() },
        ..Default::default()
    });
    let c = render(&s);
    check("마스크 하이라이트", lum(bright(&c)) < lum(bright(&a)) + 0.02 || lum(dark(&c)) > lum(dark(&a)), String::new());
}

#[test]
fn raw_pipeline_same_directions() {
    // Same directions on the RAW path (with the default tone curve and shoulder)
    let src = source(true);
    let mut e = Engine::default();
    let r = |s: &DevelopSettings, e: &mut Engine| {
        e.render(&src, &RenderRequest { settings: s, out_w: W, out_h: H, region: [0.0, 0.0, 1.0, 1.0], draft: false, clipping: false, mask_overlay: None, keep_float: true })
            .float
            .unwrap()
    };
    let a = r(&DevelopSettings::default_for(true), &mut e);
    let b = r(&DevelopSettings { contrast: 60.0, ..DevelopSettings::default_for(true) }, &mut e);
    check("RAW 대비", lum(bright(&b)) > lum(bright(&a)) && lum(dark(&b)) < lum(dark(&a)), String::new());
    let c = r(&DevelopSettings { highlights: -80.0, ..DevelopSettings::default_for(true) }, &mut e);
    check("RAW 하이라이트", lum(bright(&c)) < lum(bright(&a)), String::new());
}

#[test]
fn auto_straighten_finds_tilt() {
    // Horizontal stripes tilted 3 degrees clockwise -> correction angle about -3 degrees
    let (w, h) = (1200usize, 800usize);
    let mut img = crate::develop::image::LinearImage::new(w, h);
    let t = 3f32.to_radians();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - 600.0, y as f32 - 400.0);
            let v = -dx * t.sin() + dy * t.cos();
            // Soft edges like a real photo (hard step edges are locally horizontal and hide the tilt)
            let s = 0.45 + 0.35 * (v / 40.0 * std::f32::consts::PI).sin();
            let i = (y * w + x) * 3;
            img.data[i] = s;
            img.data[i + 1] = s;
            img.data[i + 2] = s;
        }
    }
    let src = crate::develop::image::SourceImage::new(img, false, 256);
    let a = crate::develop::pipeline::auto_straighten(&src).expect("각도");
    assert!((a + 3.0).abs() < 0.4, "{a}");
}
