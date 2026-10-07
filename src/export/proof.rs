//! Soft proofing: previews colors converted screen (sRGB) → printer/output profile → back to screen.
//! Conversion uses Windows color management (ICM, mscms.dll); the result is baked into a 33³ 3D LUT applied only to the preview.
//! © 2026 OrionNest

use anyhow::{Result, anyhow};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const N: usize = 33;
/// Out-of-gamut tolerance (round-trip ΔE76); larger than the round-trip error of CMYK LUT profiles.
const GAMUT_DE: f32 = 6.0;

fn srgb_lab(c: &[u8]) -> [f32; 3] {
    let lin = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lin8(v: u8) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn enc8(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let v = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0 + 0.5) as u8
}

/// Stretch the grid's first (black) and last (white) points to screen black and white (linear light, per channel)
fn normalize_wk(rgb: &mut [[u8; 3]]) {
    let (w, k) = (rgb[rgb.len() - 1], rgb[0]);
    for c in rgb.iter_mut() {
        for i in 0..3 {
            let (lw, lk) = (lin8(w[i]), lin8(k[i]));
            if lw - lk > 0.05 {
                c[i] = enc8((lin8(c[i]) - lk) / (lw - lk));
            }
        }
    }
}

fn delta_e(a: &[u8], b: &[u8]) -> f32 {
    let (p, q) = (srgb_lab(a), srgb_lab(b));
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Intent {
    #[default]
    Perceptual,
    Relative,
}

/// Proof LUT: corrected RGB and out-of-gamut flag per grid point
pub struct ProofLut {
    rgb: Vec<[u8; 3]>,
    out: Vec<bool>,
}

/// Installed output profile
#[derive(Clone, Debug)]
pub struct ProfileInfo {
    pub path: PathBuf,
    pub name: String,
    /// Profile kind label (printer or color space)
    pub kind: &'static str,
    pub cmyk: bool,
}

// Current LUT used by the render thread (identified by proof id)
static CURRENT: RwLock<Option<(u32, Arc<ProofLut>)>> = RwLock::new(None);

pub fn install(id: u32, lut: Arc<ProofLut>) {
    *CURRENT.write().unwrap() = Some((id, lut));
}

fn current(id: u32) -> Option<Arc<ProofLut>> {
    CURRENT.read().unwrap().as_ref().filter(|(i, _)| *i == id).map(|(_, l)| l.clone())
}

/// `view_proof` value: 0 = off, otherwise (LUT id << 1) | gamut warning
pub fn encode(id: u32, gamut: bool) -> u32 {
    (id << 1) | gamut as u32
}

/// Apply the proof to rendered RGBA8; out-of-gamut pixels are painted with `warn`.
pub fn apply_rgba(view_proof: u32, rgba: &mut [u8], warn: [u8; 3]) -> bool {
    let Some(lut) = current(view_proof >> 1) else { return false };
    let gamut = view_proof & 1 == 1;
    rgba.par_chunks_mut(4 * 1024).for_each(|ch| {
        for px in ch.as_chunks_mut::<4>().0 {
            let (o, out) = lut.lookup([px[0], px[1], px[2]]);
            if gamut && out {
                px[..3].copy_from_slice(&warn);
            } else {
                px[..3].copy_from_slice(&o);
            }
        }
    });
    true
}

impl ProofLut {
    /// Trilinear interpolation
    pub fn lookup(&self, c: [u8; 3]) -> ([u8; 3], bool) {
        let s = (N - 1) as f32 / 255.0;
        let f = [c[0] as f32 * s, c[1] as f32 * s, c[2] as f32 * s];
        let i = [f[0].floor().min((N - 2) as f32) as usize, f[1].floor().min((N - 2) as f32) as usize, f[2].floor().min((N - 2) as f32) as usize];
        let t = [f[0] - i[0] as f32, f[1] - i[1] as f32, f[2] - i[2] as f32];
        let idx = |r: usize, g: usize, b: usize| (r * N + g) * N + b;
        let mut acc = [0.0f32; 3];
        for (dr, wr) in [(0, 1.0 - t[0]), (1, t[0])] {
            for (dg, wg) in [(0, 1.0 - t[1]), (1, t[1])] {
                for (db, wb) in [(0, 1.0 - t[2]), (1, t[2])] {
                    let w = wr * wg * wb;
                    if w == 0.0 {
                        continue;
                    }
                    let v = self.rgb[idx(i[0] + dr, i[1] + dg, i[2] + db)];
                    acc[0] += v[0] as f32 * w;
                    acc[1] += v[1] as f32 * w;
                    acc[2] += v[2] as f32 * w;
                }
            }
        }
        // Gamut flag of the nearest grid point
        let near = idx(i[0] + (t[0] > 0.5) as usize, i[1] + (t[1] > 0.5) as usize, i[2] + (t[2] > 0.5) as usize);
        ([(acc[0] + 0.5) as u8, (acc[1] + 0.5) as u8, (acc[2] + 0.5) as u8], self.out[near])
    }
}

fn grid() -> Vec<u8> {
    let mut v = Vec::with_capacity(N * N * N * 3);
    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                for c in [r, g, b] {
                    v.push(((c * 255 + (N - 1) / 2) / (N - 1)) as u8);
                }
            }
        }
    }
    v
}

#[cfg(windows)]
mod icm {
    use super::*;
    use windows::Win32::UI::ColorSystem::*;

    pub struct Profile(pub isize, #[allow(dead_code)] Vec<u8>);
    impl Drop for Profile {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseColorProfile(Some(self.0));
            }
        }
    }

    pub fn open_mem(data: Vec<u8>) -> Result<Profile> {
        let pr = PROFILE { dwType: PROFILE_MEMBUFFER, pProfileData: data.as_ptr() as *mut _, cbDataSize: data.len() as u32 };
        let h = unsafe { OpenColorProfileW(&pr, PROFILE_READ, 1, 3) };
        if h == 0 {
            return Err(anyhow!("{}", trf!("색 프로필을 열 수 없습니다", "Can't open the color profile")));
        }
        Ok(Profile(h, data))
    }

    pub struct Xform(pub isize);
    impl Drop for Xform {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteColorTransform(self.0);
            }
        }
    }

    pub fn xform(profiles: &[isize], intents: &[u32], flags: u32) -> Result<Xform> {
        let h = unsafe { CreateMultiProfileTransform(profiles, intents, flags, 0) };
        if h == 0 {
            return Err(anyhow!("{}", trf!("색 변환을 만들 수 없습니다 (프로필이 손상되었거나 지원하지 않는 형식)", "Can't create the color transform (profile damaged or unsupported)")));
        }
        Ok(Xform(h))
    }

    /// Convert one row of 8-bit RGB (ICM's "BGR" format is R,G,B order in memory)
    pub fn translate(x: &Xform, src: &[u8]) -> Result<Vec<u8>> {
        let n = (src.len() / 3) as u32;
        let mut dst = vec![0u8; src.len()];
        let ok = unsafe { TranslateBitmapBits(x.0, src.as_ptr() as *const _, BM_BGRTRIPLETS, n, 1, 0, dst.as_mut_ptr() as *mut _, BM_BGRTRIPLETS, 0, None, None) };
        if !ok.as_bool() {
            return Err(anyhow!("{}", trf!("색 변환 실패", "Color conversion failed")));
        }
        Ok(dst)
    }

    pub const PERCEPTUAL: u32 = INTENT_PERCEPTUAL;
    pub const RELATIVE: u32 = INTENT_RELATIVE_COLORIMETRIC;
    pub const ABSOLUTE: u32 = INTENT_ABSOLUTE_COLORIMETRIC;
    pub const BEST: u32 = BEST_MODE;
}

/// Build the proof LUT. `paper`: simulate paper color and ink density (absolute colorimetric).
pub fn build(profile: &Path, intent: Intent, paper: bool) -> Result<ProofLut> {
    let data = std::fs::read(profile)?;
    build_from(data, intent, paper)
}

pub fn build_from(dst_icc: Vec<u8>, intent: Intent, paper: bool) -> Result<ProofLut> {
    let srgb = icm::open_mem(super::icc::build_profile(super::icc::ColorSpace::Srgb))?;
    let disp = icm::open_mem(super::icc::build_profile(super::icc::ColorSpace::Srgb))?;
    let dst = icm::open_mem(dst_icc)?;
    let i0 = match intent {
        Intent::Perceptual => icm::PERCEPTUAL,
        Intent::Relative => icm::RELATIVE,
    };
    let i1 = if paper { icm::ABSOLUTE } else { icm::RELATIVE };
    let src = grid();
    let x = icm::xform(&[srgb.0, dst.0, disp.0], &[i0, i1, i1], icm::BEST)?;
    let out = icm::translate(&x, &src)?;
    let mut rgb: Vec<[u8; 3]> = out.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect();
    if !paper {
        // Without paper/ink simulation, map paper white → screen white and ink black → screen black
        normalize_wk(&mut rgb);
    }
    // Gamut check: with black point compensation, first map screen colors into the output's black-to-white range,
    // then round-trip relative colorimetric; colors that drift are out of gamut, so unreachable deep blacks are not all flagged.
    // (ICM's CheckBitmapBits is unreliable for matrix profiles, so the check is done directly)
    let rx = icm::xform(&[srgb.0, dst.0, disp.0], &[icm::RELATIVE, icm::RELATIVE, icm::RELATIVE], icm::BEST)?;
    let e = icm::translate(&rx, &[0, 0, 0, 255, 255, 255])?;
    let ends: [u8; 6] = [e[0], e[1], e[2], e[3], e[4], e[5]];
    let scaled: Vec<u8> = src
        .as_chunks::<3>().0.iter()
        .flat_map(|c| (0..3).map(move |i| enc8(lin8(ends[i]) + lin8(c[i]) * (lin8(ends[3 + i]) - lin8(ends[i])))))
        .collect();
    let rt = icm::translate(&rx, &scaled)?;
    let out_flags = scaled.as_chunks::<3>().0.iter().zip(rt.chunks_exact(3)).map(|(a, b)| delta_e(a, b) > GAMUT_DE).collect();
    Ok(ProofLut { rgb, out: out_flags })
}

/// Installed profiles (printer and output color spaces), excluding display profiles.
pub fn installed_profiles() -> Vec<ProfileInfo> {
    let dir = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows")).join("System32\\spool\\drivers\\color");
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if ext != "icc" && ext != "icm" {
                continue;
            }
            if let Some(info) = describe(&p) {
                v.push(info);
            }
        }
    }
    v.sort_by(|a, b| (a.kind != tr!("프린터", "Printer"), a.name.to_lowercase()).cmp(&(b.kind != tr!("프린터", "Printer"), b.name.to_lowercase())));
    v
}

/// Read the ICC header and description tag
pub fn describe(p: &Path) -> Option<ProfileInfo> {
    let d = std::fs::read(p).ok()?;
    if d.len() < 132 || &d[36..40] != b"acsp" {
        return None;
    }
    let class = &d[12..16];
    let space = &d[16..20];
    let kind = match class {
        b"prtr" => tr!("프린터", "Printer"),
        b"spac" => tr!("색 공간", "Color space"),
        _ => return None,
    };
    if space != b"RGB " && space != b"CMYK" {
        return None;
    }
    let name = icc_desc(&d).unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
    Some(ProfileInfo { path: p.to_path_buf(), name, kind, cmyk: space == b"CMYK" })
}

fn be32(d: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?) as usize)
}

fn icc_desc(d: &[u8]) -> Option<String> {
    let n = be32(d, 128)?;
    for i in 0..n.min(200) {
        let e = 132 + i * 12;
        if d.get(e..e + 4)? != b"desc" {
            continue;
        }
        let off = be32(d, e + 4)?;
        let t = d.get(off..off + 4)?;
        if t == b"desc" {
            let len = be32(d, off + 8)?;
            let s = d.get(off + 12..off + 12 + len)?;
            return Some(String::from_utf8_lossy(s).trim_end_matches('\0').trim().to_string());
        }
        if t == b"mluc" {
            let len = be32(d, off + 20)?;
            let so = be32(d, off + 24)?;
            let s = d.get(off + so..off + so + len)?;
            let u: Vec<u16> = s.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            return Some(String::from_utf16_lossy(&u).trim_end_matches('\0').trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::icc::{ColorSpace, build_profile};

    #[test]
    fn identity_through_srgb() {
        // sRGB → sRGB → sRGB is nearly unchanged
        let lut = build_from(build_profile(ColorSpace::Srgb), Intent::Relative, false).unwrap();
        for c in [[0u8, 0, 0], [255, 255, 255], [200, 30, 40], [12, 140, 230], [128, 128, 128]] {
            let (o, out) = lut.lookup(c);
            for k in 0..3 {
                assert!((o[k] as i32 - c[k] as i32).abs() <= 3, "{c:?} -> {o:?}");
            }
            assert!(!out, "sRGB 안의 색은 영역 안 {c:?}");
        }
    }

    #[test]
    fn channel_order_and_gamut() {
        // Proofing screen sRGB to a wider RGB space keeps colors unchanged (round trip) with nothing out of gamut
        let lut = build_from(build_profile(ColorSpace::AdobeRgb), Intent::Relative, false).unwrap();
        let (o, out) = lut.lookup([255, 0, 0]);
        assert!(o[0] > 240 && o[1] < 20 && o[2] < 20, "빨강 유지 {o:?}");
        assert!(!out);
        // Check channel order with a direct transform: sRGB red → about (219,0,0) in the wider space
        let s = icm::open_mem(build_profile(ColorSpace::Srgb)).unwrap();
        let a = icm::open_mem(build_profile(ColorSpace::AdobeRgb)).unwrap();
        let x = icm::xform(&[s.0, a.0], &[icm::RELATIVE, icm::RELATIVE], icm::BEST).unwrap();
        let r = icm::translate(&x, &[255, 0, 0, 0, 0, 255]).unwrap();
        eprintln!("sRGB red/blue → AdobeRGB {r:?}");
        assert!((r[0] as i32 - 219).abs() <= 6 && r[1] < 10 && r[2] < 10, "{r:?}");
        assert!(r[3] < 10 && r[4] < 10 && (r[5] as i32 - 250).abs() <= 6, "{r:?}");
    }

    #[test]
    fn list_installed() {
        let v = installed_profiles();
        eprintln!("{} profiles: {:?}", v.len(), v.iter().take(8).map(|p| (&p.name, p.kind)).collect::<Vec<_>>());
        // With a CMYK print profile: neon green is out of gamut, mid gray and skin tones are in gamut
        if let Some(p) = v.iter().find(|p| p.cmyk) {
            for paper in [false, true] {
                let lut = build(&p.path, Intent::Perceptual, paper).unwrap();
                let (g, gout) = lut.lookup([0, 255, 0]);
                let (m, mout) = lut.lookup([128, 128, 128]);
                let (k, kout) = lut.lookup([200, 160, 130]);
                eprintln!("white {:?} black {:?}", lut.lookup([255, 255, 255]).0, lut.lookup([0, 0, 0]).0);
                eprintln!("{} paper={paper}: green {g:?} out={gout}, gray {m:?} out={mout}, skin {k:?} out={kout}", p.name);
                assert!(gout && !mout && !kout);
                assert!(g[1] < 245, "초록이 약해져야 함");
                // Dark colors must not be flagged, thanks to black point compensation
                for c in [[20u8, 18, 16], [45, 35, 30], [60, 60, 60]] {
                    assert!(!lut.lookup(c).1, "어두운 색 {c:?} 영역 안");
                }
            }
        }
    }
}
