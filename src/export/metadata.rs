//! Export metadata: builds EXIF (TIFF structure) and XMP packets and embeds them in JPEG/PNG.

use crate::catalog::{ColorLabel, Photo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MetaMode {
    None,
    CopyrightOnly,
    AllExceptCamera,
    #[default]
    All,
}

impl MetaMode {
    pub const ALL: [MetaMode; 4] = [MetaMode::None, MetaMode::CopyrightOnly, MetaMode::AllExceptCamera, MetaMode::All];
    pub fn name(self) -> &'static str {
        match self {
            MetaMode::None => tr!("포함 안 함", "Don't include"),
            MetaMode::CopyrightOnly => tr!("저작권만", "Copyright only"),
            MetaMode::AllExceptCamera => tr!("카메라 정보 제외 전부", "All except camera info"),
            MetaMode::All => tr!("전부", "All"),
        }
    }
}

pub struct MetaInput<'a> {
    pub photo: &'a Photo,
    pub mode: MetaMode,
    pub remove_location: bool,
    pub artist: &'a str,
    pub copyright: &'a str,
    pub ppi: u32,
    pub srgb: bool,
    pub width: u32,
    pub height: u32,
}

enum Val {
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<(u32, u32)>),
    Undefined(Vec<u8>),
    Byte(Vec<u8>),
}

impl Val {
    fn encode(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Val::Ascii(s) => {
                let mut b: Vec<u8> = s.bytes().filter(|c| *c != 0).collect();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Val::Short(v) => (3, v.len() as u32, v.iter().flat_map(|x| x.to_be_bytes()).collect()),
            Val::Long(v) => (4, v.len() as u32, v.iter().flat_map(|x| x.to_be_bytes()).collect()),
            Val::Rational(v) => (5, v.len() as u32, v.iter().flat_map(|(n, d)| [n.to_be_bytes(), d.to_be_bytes()].concat()).collect()),
            Val::Undefined(v) => (7, v.len() as u32, v.clone()),
            Val::Byte(v) => (1, v.len() as u32, v.clone()),
        }
    }
}

fn rational(v: f64) -> (u32, u32) {
    if v <= 0.0 {
        return (0, 1);
    }
    if v < 1.0 {
        // Keep the 1/x form (e.g. shutter speed)
        let d = (1.0 / v).round().max(1.0) as u32;
        if ((1.0 / d as f64) - v).abs() < v * 0.01 {
            return (1, d);
        }
    }
    ((v * 10000.0).round() as u32, 10000)
}

fn ifd_size(entries: &[(u16, Val)]) -> usize {
    let data: usize = entries
        .iter()
        .map(|(_, v)| {
            let (_, _, b) = v.encode();
            if b.len() > 4 { b.len() + (b.len() & 1) } else { 0 }
        })
        .sum();
    2 + entries.len() * 12 + 4 + data
}

fn write_ifd(out: &mut Vec<u8>, base: usize, entries: &mut [(u16, Val)]) {
    entries.sort_by_key(|e| e.0);
    let start = out.len() - base;
    let mut data_off = start + 2 + entries.len() * 12 + 4;
    let mut data = Vec::new();
    out.extend((entries.len() as u16).to_be_bytes());
    for (tag, v) in entries.iter() {
        let (ty, count, bytes) = v.encode();
        out.extend(tag.to_be_bytes());
        out.extend(ty.to_be_bytes());
        out.extend(count.to_be_bytes());
        if bytes.len() <= 4 {
            let mut b = bytes.clone();
            b.resize(4, 0);
            out.extend(b);
        } else {
            out.extend((data_off as u32).to_be_bytes());
            data.extend(&bytes);
            if bytes.len() & 1 == 1 {
                data.push(0);
            }
            data_off += bytes.len() + (bytes.len() & 1);
        }
    }
    out.extend(0u32.to_be_bytes()); // no next IFD
    out.extend(data);
}

/// EXIF bytes in TIFF structure ("MM" big-endian). None if there is nothing to include.
pub fn build_exif(m: &MetaInput) -> Option<Vec<u8>> {
    if m.mode == MetaMode::None {
        return None;
    }
    let p = m.photo;
    let meta = &p.meta;
    let mut ifd0: Vec<(u16, Val)> = vec![
        (0x0112, Val::Short(vec![1])),
        (0x011A, Val::Rational(vec![(m.ppi, 1)])),
        (0x011B, Val::Rational(vec![(m.ppi, 1)])),
        (0x0128, Val::Short(vec![2])),
        (0x0131, Val::Ascii(format!("{} {}", crate::config::APP_NAME, crate::config::APP_VERSION))),
    ];
    if !m.copyright.is_empty() {
        ifd0.push((0x8298, Val::Ascii(m.copyright.to_string())));
    }
    if m.mode == MetaMode::CopyrightOnly {
        let mut out = b"MM\0\x2a\0\0\0\x08".to_vec();
        write_ifd(&mut out, 0, &mut ifd0);
        return Some(out);
    }
    if !m.artist.is_empty() {
        ifd0.push((0x013B, Val::Ascii(m.artist.to_string())));
    }
    if !p.caption.is_empty() {
        ifd0.push((0x010E, Val::Ascii(p.caption.clone())));
    }
    let exif_date = meta.capture_time.as_ref().map(|t| t.replacen('-', ":", 2));
    if let Some(d) = &exif_date {
        ifd0.push((0x0132, Val::Ascii(d.clone())));
    }
    let mut exif: Vec<(u16, Val)> = vec![
        (0x9000, Val::Undefined(b"0232".to_vec())),
        (0xA001, Val::Short(vec![if m.srgb { 1 } else { 0xFFFF }])),
        (0xA002, Val::Long(vec![m.width])),
        (0xA003, Val::Long(vec![m.height])),
    ];
    if let Some(d) = &exif_date {
        exif.push((0x9003, Val::Ascii(d.clone())));
    }
    if m.mode == MetaMode::All {
        if let Some(v) = &meta.make {
            ifd0.push((0x010F, Val::Ascii(v.clone())));
        }
        if let Some(v) = &meta.model {
            ifd0.push((0x0110, Val::Ascii(v.clone())));
        }
        if let Some(v) = meta.exposure {
            exif.push((0x829A, Val::Rational(vec![rational(v as f64)])));
        }
        if let Some(v) = meta.fnumber {
            exif.push((0x829D, Val::Rational(vec![rational(v as f64)])));
        }
        if let Some(v) = meta.iso {
            exif.push((0x8827, Val::Short(vec![v.min(65535) as u16])));
        }
        if let Some(v) = meta.focal {
            exif.push((0x920A, Val::Rational(vec![rational(v as f64)])));
        }
        if let Some(v) = &meta.lens {
            exif.push((0xA434, Val::Ascii(v.clone())));
        }
    }
    let gps: Option<Vec<(u16, Val)>> = match (meta.gps, m.remove_location) {
        (Some((lat, lon)), false) => {
            let dms = |v: f64| {
                let v = v.abs();
                let d = v.floor();
                let mi = ((v - d) * 60.0).floor();
                let s = ((v - d) * 60.0 - mi) * 60.0;
                vec![(d as u32, 1), (mi as u32, 1), ((s * 1000.0).round() as u32, 1000)]
            };
            Some(vec![
                (0x0000, Val::Byte(vec![2, 3, 0, 0])),
                (0x0001, Val::Ascii(if lat >= 0.0 { "N" } else { "S" }.into())),
                (0x0002, Val::Rational(dms(lat))),
                (0x0003, Val::Ascii(if lon >= 0.0 { "E" } else { "W" }.into())),
                (0x0004, Val::Rational(dms(lon))),
            ])
        }
        _ => None,
    };
    // Insert pointer tags first, then compute sizes
    ifd0.push((0x8769, Val::Long(vec![0])));
    if gps.is_some() {
        ifd0.push((0x8825, Val::Long(vec![0])));
    }
    let exif_off = 8 + ifd_size(&ifd0);
    let gps_off = exif_off + ifd_size(&exif);
    for e in ifd0.iter_mut() {
        if e.0 == 0x8769 {
            e.1 = Val::Long(vec![exif_off as u32]);
        }
        if e.0 == 0x8825 {
            e.1 = Val::Long(vec![gps_off as u32]);
        }
    }
    let mut out = b"MM\0\x2a\0\0\0\x08".to_vec();
    write_ifd(&mut out, 0, &mut ifd0);
    debug_assert_eq!(out.len(), exif_off);
    write_ifd(&mut out, 0, &mut exif);
    if let Some(mut g) = gps {
        write_ifd(&mut out, 0, &mut g);
    }
    Some(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn build_xmp(m: &MetaInput) -> Option<String> {
    if m.mode == MetaMode::None {
        return None;
    }
    let p = m.photo;
    let mut attrs = format!(r#" xmp:CreatorTool="{}""#, crate::config::APP_NAME);
    let mut body = String::new();
    if !m.copyright.is_empty() {
        body += &format!(
            r#"<dc:rights><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:rights>"#,
            xml_escape(m.copyright)
        );
    }
    if m.mode != MetaMode::CopyrightOnly {
        if p.rating > 0 {
            attrs += &format!(r#" xmp:Rating="{}""#, p.rating);
        }
        if p.label != ColorLabel::None {
            let l = match p.label {
                ColorLabel::Red => "Red",
                ColorLabel::Yellow => "Yellow",
                ColorLabel::Green => "Green",
                ColorLabel::Blue => "Blue",
                ColorLabel::Purple => "Purple",
                ColorLabel::None => "",
            };
            attrs += &format!(r#" xmp:Label="{l}""#);
        }
        if !m.artist.is_empty() {
            body += &format!(r#"<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>"#, xml_escape(m.artist));
        }
        if !p.title.is_empty() {
            body += &format!(
                r#"<dc:title><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:title>"#,
                xml_escape(&p.title)
            );
        }
        if !p.caption.is_empty() {
            body += &format!(
                r#"<dc:description><rdf:Alt><rdf:li xml:lang="x-default">{}</rdf:li></rdf:Alt></dc:description>"#,
                xml_escape(&p.caption)
            );
        }
        if !p.keywords.is_empty() {
            body += "<dc:subject><rdf:Bag>";
            for k in &p.keywords {
                body += &format!("<rdf:li>{}</rdf:li>", xml_escape(k));
            }
            body += "</rdf:Bag></dc:subject>";
        }
    }
    Some(format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"{attrs}>{body}</rdf:Description>\
</rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    ))
}

/// Insert a segment right after the JPEG SOI (and JFIF APP0).
pub fn inject_jpeg(jpeg: &[u8], exif: Option<&[u8]>, xmp: Option<&str>, icc: Option<&[u8]>) -> Vec<u8> {
    let mut segs: Vec<u8> = Vec::new();
    let mut seg = |marker: u8, payload: &[u8]| {
        if payload.len() + 2 > 65535 {
            return;
        }
        segs.extend([0xFF, marker]);
        segs.extend(((payload.len() + 2) as u16).to_be_bytes());
        segs.extend(payload);
    };
    if let Some(e) = exif {
        let mut p = b"Exif\0\0".to_vec();
        p.extend(e);
        seg(0xE1, &p);
    }
    if let Some(x) = xmp {
        let mut p = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        p.extend(x.as_bytes());
        seg(0xE1, &p);
    }
    if let Some(icc) = icc {
        let chunk = 65519;
        let n = icc.len().div_ceil(chunk);
        for (i, part) in icc.chunks(chunk).enumerate() {
            let mut p = b"ICC_PROFILE\0".to_vec();
            p.push(i as u8 + 1);
            p.push(n as u8);
            p.extend(part);
            seg(0xE2, &p);
        }
    }
    // Find the position after SOI and any existing APP0/APP2 (ICC) segments
    let mut pos = 2;
    while pos + 4 <= jpeg.len() && jpeg[pos] == 0xFF && (jpeg[pos + 1] == 0xE0) {
        let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        pos += 2 + len;
    }
    let mut out = Vec::with_capacity(jpeg.len() + segs.len());
    out.extend(&jpeg[..pos]);
    out.extend(segs);
    out.extend(&jpeg[pos..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut c = 0xFFFFFFFFu32;
    for b in data {
        c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFFFFFF
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
    out
}

/// Insert eXIf / iTXt (XMP) chunks after the PNG IHDR.
pub fn inject_png(png: &[u8], exif: Option<&[u8]>, xmp: Option<&str>) -> Vec<u8> {
    if png.len() < 33 {
        return png.to_vec();
    }
    let ihdr_end = 8 + 4 + 4 + 13 + 4;
    let mut extra = Vec::new();
    if let Some(e) = exif {
        extra.extend(png_chunk(b"eXIf", e));
    }
    if let Some(x) = xmp {
        let mut d = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
        d.extend(x.as_bytes());
        extra.extend(png_chunk(b"iTXt", &d));
    }
    let mut out = png[..ihdr_end].to_vec();
    out.extend(extra);
    out.extend(&png[ihdr_end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::meta::PhotoMeta;

    fn photo() -> Photo {
        let mut c = crate::catalog::Catalog::open(&tempfile::tempdir().unwrap().path().join("x.db")).unwrap();
        let meta = PhotoMeta {
            capture_time: Some("2025-03-04 05:06:07".into()),
            make: Some("Canon".into()),
            model: Some("EOS R5".into()),
            exposure: Some(1.0 / 250.0),
            fnumber: Some(2.8),
            iso: Some(400),
            focal: Some(50.0),
            gps: Some((37.5665, 126.978)),
            ..Default::default()
        };
        let ids = c.add_photos(&[(std::path::PathBuf::from("C:/a/b.cr3"), 1, meta, None, vec!["서울".into()])], 1).unwrap();
        c.get(ids[0]).unwrap().clone()
    }

    #[test]
    fn exif_parses_back() {
        let p = photo();
        let m = MetaInput {
            photo: &p,
            mode: MetaMode::All,
            remove_location: false,
            artist: "OrionNest",
            copyright: "© 2026 OrionNest",
            ppi: 300,
            srgb: true,
            width: 100,
            height: 80,
        };
        let tiff = build_exif(&m).unwrap();
        let ex = exif::Reader::new().read_raw(tiff).unwrap();
        let model = ex.get_field(exif::Tag::Model, exif::In::PRIMARY).unwrap();
        assert!(model.display_value().to_string().contains("EOS R5"));
        let iso = ex.get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY).unwrap();
        assert_eq!(iso.value.get_uint(0), Some(400));
        assert!(ex.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_some());
        let et = ex.get_field(exif::Tag::ExposureTime, exif::In::PRIMARY).unwrap();
        assert_eq!(et.display_value().to_string(), "1/250");
    }

    #[test]
    fn jpeg_injection_keeps_decodable() {
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([100, 150, 200]));
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90).encode_image(&img).unwrap();
        let p = photo();
        let m = MetaInput { photo: &p, mode: MetaMode::All, remove_location: true, artist: "", copyright: "c", ppi: 72, srgb: true, width: 16, height: 16 };
        let out = inject_jpeg(&buf, build_exif(&m).as_deref(), build_xmp(&m).as_deref(), Some(&crate::export::icc::build_profile(crate::export::icc::ColorSpace::Srgb)));
        assert!(image::load_from_memory(&out).is_ok());
        let ex = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&out)).unwrap();
        assert!(ex.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_none(), "위치 제거");
    }
}
