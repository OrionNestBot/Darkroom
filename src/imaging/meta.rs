//! Capture metadata (EXIF) reading.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PhotoMeta {
    /// "YYYY-MM-DD HH:MM:SS"
    pub capture_time: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    /// Seconds
    pub exposure: Option<f32>,
    pub fnumber: Option<f32>,
    pub focal: Option<f32>,
    /// 35mm-equivalent focal length (EXIF FocalLengthIn35mmFilm), used for lens lookup when the crop factor is unknown
    pub focal35: Option<f32>,
    pub width: u32,
    pub height: u32,
    pub orientation: u16,
    pub gps: Option<(f64, f64)>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
    pub rating: Option<u32>,
}

impl PhotoMeta {
    pub fn exposure_label(&self) -> String {
        match self.exposure {
            Some(t) if t > 0.0 && t < 0.5 => format!("1/{}s", (1.0 / t).round() as u32),
            Some(t) => format!("{t:.1}s"),
            None => String::new(),
        }
    }

    pub fn summary(&self) -> String {
        let mut v = Vec::new();
        if let Some(f) = self.focal {
            v.push(format!("{f:.0}mm"));
        }
        if let Some(n) = self.fnumber {
            v.push(format!("f/{n:.1}"));
        }
        let e = self.exposure_label();
        if !e.is_empty() {
            v.push(e);
        }
        if let Some(i) = self.iso {
            v.push(format!("ISO {i}"));
        }
        v.join("  ·  ")
    }
}

/// EXIF "YYYY:MM:DD HH:MM:SS" -> "YYYY-MM-DD HH:MM:SS"
pub fn normalize_exif_date(s: &str) -> Option<String> {
    let s = s.trim().trim_matches('"');
    if s.len() < 19 {
        return None;
    }
    let b = s.as_bytes();
    if !b[0..4].iter().all(u8::is_ascii_digit) || b[0..4] == *b"0000" {
        return None;
    }
    let mut out = String::with_capacity(19);
    out.push_str(&s[0..4]);
    out.push('-');
    out.push_str(&s[5..7]);
    out.push('-');
    out.push_str(&s[8..10]);
    out.push(' ');
    out.push_str(&s[11..19]);
    Some(out)
}

fn rat(v: &exif::Value) -> Option<f32> {
    match v {
        exif::Value::Rational(r) if !r.is_empty() && r[0].denom != 0 => Some(r[0].num as f32 / r[0].denom as f32),
        exif::Value::SRational(r) if !r.is_empty() && r[0].denom != 0 => Some(r[0].num as f32 / r[0].denom as f32),
        _ => None,
    }
}

fn gps_coord(v: &exif::Value) -> Option<f64> {
    if let exif::Value::Rational(r) = v
        && r.len() >= 3 && r.iter().all(|x| x.denom != 0) {
            return Some(r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0);
        }
    None
}

fn clean(s: String) -> Option<String> {
    let t = s.trim().trim_matches('"').trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Based on kamadak-exif (JPEG/TIFF/HEIF/PNG/DNG).
pub fn read_exif(path: &Path) -> Option<PhotoMeta> {
    use exif::{In, Tag};
    let file = std::fs::File::open(path).ok()?;
    let mut r = std::io::BufReader::new(file);
    let ex = exif::Reader::new().read_from_container(&mut r).ok()?;
    let s = |t: Tag| ex.get_field(t, In::PRIMARY).and_then(|f| clean(f.display_value().to_string()));
    let mut m = PhotoMeta {
        orientation: ex.get_field(Tag::Orientation, In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap_or(1) as u16,
        ..Default::default()
    };
    m.capture_time = ex
        .get_field(Tag::DateTimeOriginal, In::PRIMARY)
        .or_else(|| ex.get_field(Tag::DateTime, In::PRIMARY))
        .and_then(|f| match &f.value {
            exif::Value::Ascii(v) if !v.is_empty() => normalize_exif_date(&String::from_utf8_lossy(&v[0])),
            _ => None,
        });
    m.make = s(Tag::Make);
    m.model = s(Tag::Model);
    m.lens = s(Tag::LensModel);
    m.artist = s(Tag::Artist);
    m.copyright = s(Tag::Copyright);
    m.iso = ex
        .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0));
    m.exposure = ex.get_field(Tag::ExposureTime, In::PRIMARY).and_then(|f| rat(&f.value));
    m.fnumber = ex.get_field(Tag::FNumber, In::PRIMARY).and_then(|f| rat(&f.value));
    m.focal = ex.get_field(Tag::FocalLength, In::PRIMARY).and_then(|f| rat(&f.value));
    m.focal35 = ex.get_field(Tag::FocalLengthIn35mmFilm, In::PRIMARY).and_then(|f| f.value.get_uint(0)).map(|v| v as f32).filter(|v| *v > 0.0);
    m.width = ex
        .get_field(Tag::PixelXDimension, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(0);
    m.height = ex
        .get_field(Tag::PixelYDimension, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(0);
    if let (Some(la), Some(lo)) = (ex.get_field(Tag::GPSLatitude, In::PRIMARY), ex.get_field(Tag::GPSLongitude, In::PRIMARY))
        && let (Some(mut lat), Some(mut lon)) = (gps_coord(&la.value), gps_coord(&lo.value)) {
            if s(Tag::GPSLatitudeRef).as_deref() == Some("S") {
                lat = -lat;
            }
            if s(Tag::GPSLongitudeRef).as_deref() == Some("W") {
                lon = -lon;
            }
            m.gps = Some((lat, lon));
        }
    Some(m)
}

/// RAW: rawler metadata.
/// Lens id from the Canon RAW maker note (CameraSettings[22] = LensType). Supports CR3 (CMT3 box) and CR2 (TIFF)
pub fn canon_lens_type(path: &Path) -> Option<u32> {
    let cs = canon_camera_settings(path)?;
    let v = *cs.get(22)? as u32;
    (v != 0 && v != 65535).then_some(v)
}

#[cfg(test)]
/// Canon maker note LightingOpt (0x4018, int32 array): [1] peripheral illumination correction, [2] auto lighting optimizer, [3] highlight tone priority (HTP), ...
pub fn canon_lighting_opt(path: &Path) -> Option<Vec<i32>> {
    let (t, ifd, le) = canon_makernote(path)?;
    let (typ, cnt, off) = ifd_find(&t, ifd, 0x4018, le)?;
    if typ != 9 && typ != 4 {
        return None;
    }
    (0..cnt as usize).map(|i| rd32(&t, off as usize + i * 4, le).map(|v| v as i32)).collect()
}

/// Canon maker note IFD (TIFF data, IFD offset, little-endian)
#[cfg(test)]
fn canon_makernote(path: &Path) -> Option<(Vec<u8>, usize, bool)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 512 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext == "cr3" {
        let p = buf.windows(4).position(|w| w == b"CMT3")?;
        let t = buf[p + 4..].to_vec();
        let le = t.get(..2)? == b"II";
        let first = rd32(&t, 4, le)? as usize;
        Some((t, first, le))
    } else if ext == "cr2" {
        let le = buf.get(..2)? == b"II";
        let ifd0 = rd32(&buf, 4, le)? as usize;
        let exif = ifd_find(&buf, ifd0, 0x8769, le)?.2 as usize;
        let mn = ifd_find(&buf, exif, 0x927C, le)?.2 as usize;
        Some((buf, mn, le))
    } else {
        None
    }
}

/// Fallback lens name built from the focal length range when the file has none ("30.0 mm", "18.0-35.0 mm")
pub fn canon_focal_name(path: &Path) -> Option<String> {
    let cs = canon_camera_settings(path)?;
    let (mx, mn, unit) = (*cs.get(23)? as f32, *cs.get(24)? as f32, (*cs.get(25)? as f32).max(1.0));
    if mn <= 0.0 {
        return None;
    }
    let (a, b) = (mn / unit, mx / unit);
    Some(if (a - b).abs() < 0.05 { format!("{a:.1} mm") } else { format!("{a:.1}-{b:.1} mm") })
}

/// Canon maker note CameraSettings (0x0001) array
fn canon_camera_settings(path: &Path) -> Option<Vec<u16>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 512 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let (tiff, mn) = if ext == "cr3" {
        // CR3: the 'CMT3' box holds a TIFF containing the maker note IFD
        let p = buf.windows(4).position(|w| w == b"CMT3")?;
        let t = &buf[p + 4..];
        let le = t.get(..2)? == b"II";
        let first = rd32(t, 4, le)? as usize;
        (t, (first, le))
    } else if ext == "cr2" {
        // CR2: IFD0 -> Exif IFD (0x8769) -> MakerNote (0x927C)
        let t = &buf[..];
        let le = t.get(..2)? == b"II";
        let ifd0 = rd32(t, 4, le)? as usize;
        let exif = ifd_find(t, ifd0, 0x8769, le)?.2 as usize;
        let mn = ifd_find(t, exif, 0x927C, le)?.2 as usize;
        (t, (mn, le))
    } else {
        return None;
    };
    let (ifd, le) = mn;
    let (typ, cnt, off) = ifd_find(tiff, ifd, 0x0001, le)?;
    if typ != 3 || cnt <= 25 {
        return None;
    }
    (0..cnt as usize).map(|i| rd16(tiff, off as usize + i * 2, le)).collect()
}

/// EXIF lens name (0xA434) exactly as stored in the file, for Canon RAW (CR3: CMT2 box, CR2: Exif IFD).
/// rawler fills in a name guessed from the lens id, so this one is used for lens profile lookup
pub fn raw_lens_model(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 512 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let (t, ifd, le): (&[u8], usize, bool) = if ext == "cr3" {
        let p = buf.windows(4).position(|w| w == b"CMT2")?;
        let t = &buf[p + 4..];
        let le = t.get(..2)? == b"II";
        (t, rd32(t, 4, le)? as usize, le)
    } else if ext == "cr2" {
        let t = &buf[..];
        let le = t.get(..2)? == b"II";
        let ifd0 = rd32(t, 4, le)? as usize;
        (t, ifd_find(t, ifd0, 0x8769, le)?.2 as usize, le)
    } else {
        return None;
    };
    let (typ, cnt, off) = ifd_find(t, ifd, 0xA434, le)?;
    if typ != 2 {
        return None;
    }
    let (cnt, off) = (cnt as usize, off as usize);
    let bytes = if cnt <= 4 { t.get(ifd..0)?.to_vec() } else { t.get(off..off + cnt)?.to_vec() };
    let s = String::from_utf8_lossy(&bytes).trim_end_matches(' ').trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn rd16(b: &[u8], o: usize, le: bool) -> Option<u16> {
    let s: [u8; 2] = b.get(o..o + 2)?.try_into().ok()?;
    Some(if le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
}

fn rd32(b: &[u8], o: usize, le: bool) -> Option<u32> {
    let s: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
    Some(if le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
}

/// Find a tag in an IFD -> (type, count, value/offset)
fn ifd_find(b: &[u8], ifd: usize, tag: u16, le: bool) -> Option<(u16, u32, u32)> {
    let n = rd16(b, ifd, le)? as usize;
    for i in 0..n.min(512) {
        let e = ifd + 2 + i * 12;
        if rd16(b, e, le)? == tag {
            return Some((rd16(b, e + 2, le)?, rd32(b, e + 4, le)?, rd32(b, e + 8, le)?));
        }
    }
    None
}

pub fn read_raw_meta(path: &Path) -> Option<PhotoMeta> {
    // A decoder panic (damaged file) means no metadata
    super::decode::init_decoder();
    std::panic::catch_unwind(|| read_raw_meta_inner(path)).ok().flatten()
}

fn read_raw_meta_inner(path: &Path) -> Option<PhotoMeta> {
    use rawler::decoders::RawDecodeParams;
    let src = rawler::rawsource::RawSource::new(path).ok()?;
    let dec = rawler::get_decoder(&src).ok()?;
    let md = dec.raw_metadata(&src, &RawDecodeParams::default()).ok()?;
    let e = &md.exif;
    let r = |x: Option<rawler::formats::tiff::Rational>| x.and_then(|v| if v.d != 0 { Some(v.n as f32 / v.d as f32) } else { None });
    let mut m = PhotoMeta {
        capture_time: e.date_time_original.as_deref().and_then(normalize_exif_date),
        make: clean(md.make.clone()),
        model: clean(md.model.clone()),
        // Prefer the lens name stored in the file (EXIF LensModel); lens profiles are matched by this name.
        // rawler's name guessed from the lens id is wrong when several lenses share an id (e.g. Sigma 30mm Art reported as an older EX)
        lens: e
            .lens_model
            .clone()
            .and_then(clean)
            .or_else(|| md.lens.as_ref().map(|l| format!("{} {}", l.lens_make, l.lens_model).trim().to_string()).and_then(clean)),
        iso: e.iso_speed_ratings.map(|v| v as u32).or(e.iso_speed),
        exposure: r(e.exposure_time),
        fnumber: r(e.fnumber),
        focal: r(e.focal_length),
        orientation: e.orientation.unwrap_or(1),
        artist: e.artist.clone().and_then(clean),
        copyright: e.copyright.clone().and_then(clean),
        rating: md.rating,
        ..Default::default()
    };
    if let Some(g) = &e.gps {
        let conv = |v: &Option<[rawler::formats::tiff::Rational; 3]>| {
            v.as_ref().map(|a| {
                let f = |x: &rawler::formats::tiff::Rational| if x.d != 0 { x.n as f64 / x.d as f64 } else { 0.0 };
                f(&a[0]) + f(&a[1]) / 60.0 + f(&a[2]) / 3600.0
            })
        };
        if let (Some(mut lat), Some(mut lon)) = (conv(&g.gps_latitude), conv(&g.gps_longitude)) {
            if g.gps_latitude_ref.as_deref().map(|s| s.starts_with('S')).unwrap_or(false) {
                lat = -lat;
            }
            if g.gps_longitude_ref.as_deref().map(|s| s.starts_with('W')).unwrap_or(false) {
                lon = -lon;
            }
            if lat != 0.0 || lon != 0.0 {
                m.gps = Some((lat, lon));
            }
        }
    }
    // Fill in DNG and others from kamadak-exif
    if (m.capture_time.is_none() || m.make.is_none())
        && let Some(x) = read_exif(path) {
            m.capture_time = m.capture_time.or(x.capture_time);
            m.make = m.make.or(x.make);
            m.model = m.model.or(x.model);
            m.lens = m.lens.or(x.lens);
        }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_normalization() {
        assert_eq!(normalize_exif_date("2024:05:01 12:34:56").as_deref(), Some("2024-05-01 12:34:56"));
        assert_eq!(normalize_exif_date("0000:00:00 00:00:00"), None);
    }
}

#[cfg(test)]
mod lens_type_tests {
    /// Print the lens name and Canon lens id for every RAW in DARKROOM_LENSTYPE_DIR
    #[test]
    #[ignore]
    fn canon_lens_type_probe() {
        let Ok(d) = std::env::var("DARKROOM_LENSTYPE_DIR") else { return };
        for e in std::fs::read_dir(d).unwrap().flatten().take(std::env::var("DARKROOM_LENSTYPE_TAKE").ok().and_then(|v| v.parse().ok()).unwrap_or(6)) {
            let p = e.path();
            let m = super::read_raw_meta(&p);
            eprintln!("{}: ISO {:?} 셔터 {:?} f/{:?} · {:?} / 파일 {:?} → {:?} / 조명 최적화 {:?}", p.file_name().unwrap().to_string_lossy(), m.as_ref().and_then(|m| m.iso), m.as_ref().and_then(|m| m.exposure), m.as_ref().and_then(|m| m.fnumber), m.as_ref().and_then(|m| m.lens.clone()), super::raw_lens_model(&p), super::canon_lens_type(&p), super::canon_lighting_opt(&p));
        }
    }
}
