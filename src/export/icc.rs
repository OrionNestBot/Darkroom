//! Builds matrix/TRC ICC v2 display profiles (sRGB, opRGB / 1998 wide-gamut RGB compatible, Display P3).
//! Profiles are generated from standard primaries, without external profile files.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorSpace {
    #[default]
    Srgb,
    AdobeRgb,
    DisplayP3,
}

impl ColorSpace {
    pub const ALL: [ColorSpace; 3] = [ColorSpace::Srgb, ColorSpace::AdobeRgb, ColorSpace::DisplayP3];
    pub fn name(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "sRGB",
            ColorSpace::AdobeRgb => tr!("Adobe RGB (1998) 호환", "Adobe RGB (1998) compatible"),
            ColorSpace::DisplayP3 => "Display P3",
        }
    }

    /// Linear sRGB → target-space linear RGB matrix (D65).
    pub fn from_srgb_matrix(self) -> [[f32; 3]; 3] {
        match self {
            ColorSpace::Srgb => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            ColorSpace::AdobeRgb => [[0.715119, 0.284881, 0.0], [0.0, 1.0, 0.0], [0.0, 0.041161, 0.958839]],
            ColorSpace::DisplayP3 => [[0.822462, 0.177538, 0.0], [0.033194, 0.966806, 0.0], [0.017083, 0.072397, 0.910520]],
        }
    }

    /// Transfer function of the target space (linear → encoded).
    pub fn encode(self, x: f32) -> f32 {
        match self {
            ColorSpace::AdobeRgb => x.max(0.0).powf(1.0 / 2.199_218_8),
            _ => crate::develop::color::srgb_encode(x),
        }
    }

    /// D50-adapted primaries XYZ (rXYZ, gXYZ, bXYZ).
    fn primaries(self) -> [[f64; 3]; 3] {
        match self {
            ColorSpace::Srgb => [[0.4360747, 0.2225045, 0.0139322], [0.3850649, 0.7168786, 0.0971045], [0.1430804, 0.0606169, 0.7141733]],
            ColorSpace::AdobeRgb => [[0.6097559, 0.3111242, 0.0194811], [0.2052401, 0.6256560, 0.0608902], [0.1492240, 0.0632197, 0.7448387]],
            ColorSpace::DisplayP3 => [[0.5151, 0.2412, -0.0011], [0.2920, 0.6922, 0.0419], [0.1571, 0.0666, 0.7841]],
        }
    }

    pub fn profile_desc(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "sRGB (Darkroom)",
            ColorSpace::AdobeRgb => "Compatible with Adobe RGB (1998) (Darkroom)",
            ColorSpace::DisplayP3 => "Display P3 (Darkroom)",
        }
    }
}

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    t.extend(s15f16(x));
    t.extend(s15f16(y));
    t.extend(s15f16(z));
    t
}

fn desc_tag(s: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    let ascii: Vec<u8> = s.bytes().chain(std::iter::once(0)).collect();
    t.extend((ascii.len() as u32).to_be_bytes());
    t.extend(&ascii);
    t.extend([0u8; 4]); // unicode lang
    t.extend([0u8; 4]); // unicode count
    t.extend([0u8; 2]); // scriptcode code
    t.push(0); // scriptcode count
    t.extend([0u8; 67]);
    t
}

fn text_tag(s: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend(s.bytes());
    t.push(0);
    t
}

fn trc_tag(cs: ColorSpace) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    match cs {
        ColorSpace::AdobeRgb => {
            t.extend(1u32.to_be_bytes());
            t.extend(563u16.to_be_bytes()); // 2.19921875 = 563/256
        }
        _ => {
            let n = 1024u32;
            t.extend(n.to_be_bytes());
            for i in 0..n {
                let v = crate::develop::color::srgb_decode(i as f32 / (n - 1) as f32);
                t.extend(((v * 65535.0).round() as u16).to_be_bytes());
            }
        }
    }
    t
}

pub fn build_profile(cs: ColorSpace) -> Vec<u8> {
    let p = cs.primaries();
    let trc = trc_tag(cs);
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc_tag(cs.profile_desc())),
        (b"cprt", text_tag("No copyright, use freely")),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz_tag(p[0][0], p[0][1], p[0][2])),
        (b"gXYZ", xyz_tag(p[1][0], p[1][1], p[1][2])),
        (b"bXYZ", xyz_tag(p[2][0], p[2][1], p[2][2])),
        (b"rTRC", trc.clone()),
        (b"gTRC", trc.clone()),
        (b"bTRC", trc),
    ];
    let table_len = 4 + tags.len() * 12;
    let mut offset = 128 + table_len;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    for (sig, body) in &tags {
        let pad = (4 - offset % 4) % 4;
        data.extend(std::iter::repeat_n(0u8, pad));
        offset += pad;
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(body);
        offset += body.len();
    }
    let total = 128 + table.len() + data.len();
    let mut h = vec![0u8; 128];
    h[0..4].copy_from_slice(&(total as u32).to_be_bytes());
    h[8..12].copy_from_slice(&[0x02, 0x10, 0, 0]); // v2.1
    h[12..16].copy_from_slice(b"mntr");
    h[16..20].copy_from_slice(b"RGB ");
    h[20..24].copy_from_slice(b"XYZ ");
    h[24..36].copy_from_slice(&[0x07, 0xEA, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]); // creation date
    h[36..40].copy_from_slice(b"acsp");
    h[40..44].copy_from_slice(b"MSFT");
    h[64..68].copy_from_slice(&0u32.to_be_bytes()); // perceptual
    h[68..72].copy_from_slice(&s15f16(0.9642));
    h[72..76].copy_from_slice(&s15f16(1.0));
    h[76..80].copy_from_slice(&s15f16(0.8249));
    let mut out = h;
    out.extend(table);
    out.extend(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_is_well_formed() {
        for cs in ColorSpace::ALL {
            let p = build_profile(cs);
            let len = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize;
            assert_eq!(len, p.len());
            assert_eq!(&p[36..40], b"acsp");
            let n = u32::from_be_bytes([p[128], p[129], p[130], p[131]]) as usize;
            assert_eq!(n, 9);
            for i in 0..n {
                let e = 132 + i * 12;
                let off = u32::from_be_bytes([p[e + 4], p[e + 5], p[e + 6], p[e + 7]]) as usize;
                let sz = u32::from_be_bytes([p[e + 8], p[e + 9], p[e + 10], p[e + 11]]) as usize;
                assert!(off % 4 == 0 && off + sz <= p.len());
            }
        }
    }
}
