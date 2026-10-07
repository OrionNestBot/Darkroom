//! File decoding: scene-linear originals for Develop and 8-bit images for thumbnails/previews.

use super::heic;
use crate::config;
use crate::develop::color::{srgb8_to_linear_table, srgb16_to_linear_table};
use crate::develop::image::{LinearImage, SourceImage};
use anyhow::{Context, Result, anyhow};
use fast_image_resize as fr;
use rayon::prelude::*;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Raw,
    Raster,
    Heif,
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if config::RAW_EXTS.contains(&ext.as_str()) {
        Some(Kind::Raw)
    } else if config::RASTER_EXTS.contains(&ext.as_str()) {
        Some(Kind::Raster)
    } else if config::HEIF_EXTS.contains(&ext.as_str()) {
        Some(Kind::Heif)
    } else {
        None
    }
}

/// 8-bit RGBA image.
#[derive(Clone)]
pub struct Rgba8 {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Rgba8 {
    pub fn from_dynamic(img: image::DynamicImage) -> Self {
        let rgba = img.into_rgba8();
        let (w, h) = rgba.dimensions();
        Self { w, h, data: rgba.into_raw() }
    }

    /// High-quality downscale so the long edge is at most `max_edge` (SIMD).
    pub fn fit(self, max_edge: u32) -> Self {
        if self.w.max(self.h) <= max_edge {
            return self;
        }
        let k = max_edge as f32 / self.w.max(self.h) as f32;
        let nw = ((self.w as f32 * k).round() as u32).max(1);
        let nh = ((self.h as f32 * k).round() as u32).max(1);
        self.resize(nw, nh)
    }

    pub fn resize(self, nw: u32, nh: u32) -> Self {
        let (w, h) = (self.w, self.h);
        let src = match fr::images::Image::from_vec_u8(w, h, self.data, fr::PixelType::U8x4) {
            Ok(s) => s,
            Err(_) => return Self { w: nw, h: nh, data: vec![0; (nw * nh * 4) as usize] },
        };
        let mut dst = fr::images::Image::new(nw, nh, fr::PixelType::U8x4);
        let mut rs = fr::Resizer::new();
        let opts = fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3));
        let _ = rs.resize(&src, &mut dst, &opts);
        Self { w: nw, h: nh, data: dst.into_vec() }
    }

    /// Applies EXIF orientation.
    pub fn oriented(self, o: u16) -> Self {
        if o <= 1 || o > 8 {
            return self;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let swap = o >= 5;
        let (nw, nh) = if swap { (h, w) } else { (w, h) };
        let mut out = vec![0u8; nw * nh * 4];
        let src = &self.data;
        out.par_chunks_mut(nw * 4).enumerate().for_each(|(ny, row)| {
            for nx in 0..nw {
                let (sx, sy) = match o {
                    2 => (w - 1 - nx, ny),
                    3 => (w - 1 - nx, h - 1 - ny),
                    4 => (nx, h - 1 - ny),
                    5 => (ny, nx),
                    6 => (ny, h - 1 - nx),
                    7 => (w - 1 - ny, h - 1 - nx),
                    8 => (w - 1 - ny, nx),
                    _ => (nx, ny),
                };
                let si = (sy * w + sx) * 4;
                row[nx * 4..nx * 4 + 4].copy_from_slice(&src[si..si + 4]);
            }
        });
        Self { w: nw as u32, h: nh as u32, data: out }
    }

    pub fn encode_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        let rgb: Vec<u8> = self.data.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        image::ImageEncoder::write_image(enc, &rgb, self.w, self.h, image::ExtendedColorType::Rgb8)?;
        Ok(out)
    }

    pub fn load_jpeg_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)?;
        Ok(Self::from_dynamic(img))
    }
}

fn raw_orientation_u16(o: rawler::decoders::Orientation) -> u16 {
    o.to_u16()
}

/// Configures the RAW decoder once, before the first decode: extra camera definitions are read from cameras/*.toml
/// in the data folder, so new models can be added without an app update. DARKROOM_EXTRA_CAMERAS=<dir> overrides the folder.
pub fn init_decoder() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::var_os("DARKROOM_EXTRA_CAMERAS").map(std::path::PathBuf::from).unwrap_or_else(|| crate::config::data_dir().join("cameras"));
        rawler::set_extra_camera_dir(dir);
    });
}

/// Turns a panic inside the decoder (damaged or unusual file) into an error so one file cannot stop a worker thread or the app
pub fn no_panic<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    init_decoder();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(anyhow!("{}", tr!("이 파일을 읽다가 디코더가 멈췄습니다 (손상되었거나 아직 지원하지 않는 형식)", "The decoder failed on this file (damaged or not supported yet)"))))
}

pub fn decode_source(path: &Path, orientation: u16) -> Result<SourceImage> {
    no_panic(|| decode_source_inner(path, orientation))
}

fn decode_source_inner(path: &Path, orientation: u16) -> Result<SourceImage> {
    // HDR merge result (.drhdr): orientation was already applied when saved
    if path.extension().map(|e| e.eq_ignore_ascii_case("drhdr")).unwrap_or(false) {
        let (img, rc, h) = super::hdr::load_source(path)?;
        let mut src = SourceImage::new(img, true, config::PYRAMID_MIN_EDGE);
        src.raw_color = Some(std::sync::Arc::new(rc));
        src.lens_info = Some(std::sync::Arc::new(crate::develop::image::LensInfo {
            make: h.make.clone(),
            model: h.camera.clone(),
            lens: h.lens.clone(),
            focal: h.focal,
            fnumber: h.fnumber,
            orientation: 1,
            embedded: None,
            full_dims: None,
            crop: None,
            shading_comp: false,
        }));
        return Ok(src);
    }
    let kind = kind_of(path).ok_or_else(|| anyhow!("{}", trf!("지원하지 않는 형식", "Unsupported format")))?;
    let mut raw_color = None;
    let mut full_dims = None;
    let base = match kind {
        Kind::Raw => {
            let (img, rc, full) = decode_raw_linear(path, orientation)?;
            raw_color = rc;
            full_dims = full;
            img
        }
        Kind::Raster => {
            let img = image::ImageReader::open(path)?.with_guessed_format()?.decode().context(tr!("이미지 디코딩 실패", "Image decoding failed"))?;
            dynamic_to_linear(img).oriented(orientation)
        }
        Kind::Heif => {
            let (data, w, h) = heic::decode_rgba(path, None)?;
            rgba8_to_linear(&data, w as usize, h as usize).oriented(orientation)
        }
    };
    let mut src = SourceImage::new(base, kind == Kind::Raw, config::PYRAMID_MIN_EDGE);
    src.raw_color = raw_color.map(std::sync::Arc::new);
    // Shooting info for lens correction (EXIF)
    let m = if kind == Kind::Raw { super::meta::read_raw_meta(path).or_else(|| super::meta::read_exif(path)) } else { super::meta::read_exif(path) };
    if let Some(m) = m {
        // Crop factor = EXIF 35mm equivalent / focal length (rawler metadata has no 35mm equivalent, so EXIF is used: TIFF-based RAW, JPEG)
        let f35 = if kind == Kind::Raw { super::meta::read_exif(path).and_then(|x| x.focal35) } else { m.focal35 };
        let crop35 = match (f35, m.focal) {
            (Some(a), Some(f)) if f > 0.0 => Some(a / f).filter(|c| (0.4..8.0).contains(c)),
            _ => None,
        };
        src.lens_info = Some(std::sync::Arc::new(crate::develop::image::LensInfo {
            make: m.make.unwrap_or_default(),
            model: m.model.unwrap_or_default(),
            // For lens profile lookup: the lens name in the file, otherwise a name built from the focal length
            lens: if kind == Kind::Raw {
                super::meta::raw_lens_model(path).or_else(|| super::meta::canon_focal_name(path)).or(m.lens).unwrap_or_default()
            } else {
                m.lens.unwrap_or_default()
            },
            focal: m.focal.unwrap_or(0.0),
            fnumber: m.fnumber.unwrap_or(0.0),
            orientation,
            embedded: if kind == Kind::Raw { crate::develop::embedded::read(path).map(std::sync::Arc::new) } else { None },
            full_dims,
            crop: crop35,
            shading_comp: kind == Kind::Raw && crate::develop::embedded::shading_compensated(path),
        }));
    }
    Ok(src)
}

/// Black/white level correction of integer Bayer data (negatives preserved). Returns true on success.
fn scale_keep_negative(raw: &mut rawler::RawImage) -> bool {
    use rawler::RawImageData;
    if !matches!(raw.photometric, rawler::rawimage::RawPhotometricInterpretation::Cfa(_)) {
        return false;
    }
    let RawImageData::Integer(d) = &raw.data else { return false };
    let bl = raw.blacklevel.as_bayer_array();
    let wl = raw.whitelevel.as_bayer_array();
    let w = raw.width;
    let max = [wl[0] - bl[0], wl[1] - bl[1], wl[2] - bl[2], wl[3] - bl[3]];
    if max.iter().any(|m| *m <= 0.0) || w == 0 {
        return false;
    }
    use rayon::prelude::*;
    let out: Vec<f32> = d
        .par_chunks(w)
        .enumerate()
        .flat_map_iter(|(y, row)| {
            let r = (y & 1) * 2;
            row.iter().enumerate().map(move |(x, v)| {
                let c = r + (x & 1);
                (*v as f32 - bl[c]) / max[c]
            })
        })
        .collect();
    raw.data = RawImageData::Float(out);
    raw.blacklevel.levels.iter_mut().for_each(|x| *x = rawler::formats::tiff::Rational::new(0, 1));
    raw.whitelevel.0.iter_mut().for_each(|x| *x = 1);
    true
}

/// Canon R6/R5 (white level 14888 generation) files recorded with black level 512 (electronic shutter etc.) are normalized
/// to the same signal range as black-2048 files (white − 2048); otherwise they come out darker than reference renders.
/// Not applied to R7/R8/M6/M100. Test switch: DARKROOM_BLACK512=0 disables it.
fn black512_offset(camera: &str, white: f64, black: f64) -> f64 {
    const CAMS: &[&str] = &["Canon EOS R6", "Canon EOS R5"];
    if std::env::var("DARKROOM_BLACK512").as_deref() == Ok("0") || !CAMS.iter().any(|c| c.eq_ignore_ascii_case(camera)) || black > 1000.0 || white < 4000.0 {
        return 0.0;
    }
    ((white - black) / (white - 2048.0)).log2()
}

/// Offset for photos deliberately underexposed by Highlight Tone Priority or DR modes (Test switch: DARKROOM_MODE_EXPOSURE=0 disables it)
fn mode_offset(path: &Path) -> f64 {
    if std::env::var("DARKROOM_MODE_EXPOSURE").map(|v| v == "0").unwrap_or(false) {
        return 0.0;
    }
    crate::develop::embedded::mode_exposure_offset(path)
}

/// Canon extended low ISO (ISO 50 "L") is ISO 100 with extra exposure, so the image is darkened by that amount (ISO 50 → −1 EV)
fn low_iso_offset(path: &Path, camera: &str) -> f64 {
    let iso = || crate::imaging::meta::read_raw_meta(path).and_then(|m| m.iso).unwrap_or(0);
    // Per-model low-ISO step (config::LOW_ISO_STEP): below that ISO the baseline exposure is one step lower (same rule as the camera's own DNG BaselineExposure)
    if let Some((_, below, ev)) = crate::config::LOW_ISO_STEP.iter().find(|(c, _, _)| c.eq_ignore_ascii_case(camera)) {
        let i = iso();
        return if i > 0 && i < *below { *ev } else { 0.0 };
    }
    if !camera.starts_with("Canon") {
        return 0.0;
    }
    match iso() {
        iso if iso > 0 && iso < 100 => -(100.0 / iso as f64).log2(),
        _ => 0.0,
    }
}

/// Center crop to the in-camera aspect ratio (sensor orientation) → (cropped image, size before cropping)
fn aspect_crop(img: LinearImage, path: &Path) -> (LinearImage, Option<(u32, u32)>) {
    if std::env::var("DARKROOM_CAMERA_ASPECT").map(|v| v == "0").unwrap_or(false) {
        return (img, None);
    }
    let (w, h) = (img.w, img.h);
    // DNG in-camera crop (DefaultUserCrop): use that rectangle as is
    if let Some([t, l, b, r]) = crate::develop::embedded::dng_user_crop(path) {
        let (x0, y0) = ((l * w as f64).round() as usize, (t * h as f64).round() as usize);
        let (x1, y1) = (((r * w as f64).round() as usize).clamp(x0 + 1, w), ((b * h as f64).round() as usize).clamp(y0 + 1, h));
        let (nw, nh) = (x1 - x0, y1 - y0);
        let mut data = Vec::with_capacity(nw * nh * 3);
        for y in y0..y1 {
            data.extend_from_slice(&img.data[(y * w + x0) * 3..(y * w + x1) * 3]);
        }
        return (LinearImage { w: nw, h: nh, data }, Some((w as u32, h as u32)));
    }
    let Some((aw, ah)) = crate::develop::embedded::camera_aspect(path) else { return (img, None) };
    let r = aw / ah;
    let cur = w as f64 / h as f64;
    if (cur / r - 1.0).abs() < 0.01 {
        return (img, None);
    }
    let (nw, nh) = if cur > r { (((h as f64) * r).round() as usize, h) } else { (w, ((w as f64) / r).round() as usize) };
    let (nw, nh) = (nw.clamp(1, w), nh.clamp(1, h));
    let (x0, y0) = ((w - nw) / 2, (h - nh) / 2);
    let mut data = Vec::with_capacity(nw * nh * 3);
    for y in y0..y0 + nh {
        data.extend_from_slice(&img.data[(y * w + x0) * 3..(y * w + x0 + nw) * 3]);
    }
    (LinearImage { w: nw, h: nh, data }, Some((w as u32, h as u32)))
}

/// RAW decoding. When DNG-style color processing is possible, returns camera-native RGB plus color info;
/// otherwise (4-color CFA etc.) rawler's linear sRGB result (orientation applied). Also returns the size before the aspect crop (sensor orientation).
fn decode_raw_linear(path: &Path, orientation_hint: u16) -> Result<(LinearImage, Option<crate::develop::dcp::RawColor>, Option<(u32, u32)>)> {
    use crate::develop::dcp;
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
    let src = rawler::rawsource::RawSource::new(path).context(tr!("RAW 파일 열기 실패", "Couldn't open RAW file"))?;
    let dec = rawler::get_decoder(&src).map_err(|e| anyhow!("{}", trf!("지원하지 않는 RAW: {e}", "Unsupported RAW: {e}")))?;
    let raw = dec.raw_image(&src, &RawDecodeParams::default(), false).map_err(|e| anyhow!("{}", trf!("RAW 디코딩 실패: {e}", "RAW decoding failed: {e}")))?;
    // For formats where rawler cannot read the orientation (some CR2 etc.), use the EXIF orientation
    let orient = match raw_orientation_u16(raw.orientation) {
        0 | 1 => orientation_hint,
        o => o,
    };
    let is_dng = path.extension().map(|e| e.eq_ignore_ascii_case("dng")).unwrap_or(false);
    // For DNG, read the embedded profile, as-shot neutral and baseline exposure directly
    let tags = if is_dng { std::fs::read(path).ok().and_then(|b| dcp::parse_color_tags(&b)) } else { None };
    // Black level correction is done here because rawler clips negatives to 0, which lifts the mean of high-ISO shadows
    let mut raw = raw;
    // Levels before correction (for the lowered white level at some ISOs)
    let (white0, black0) = (raw.whitelevel.as_bayer_array()[0] as f64, raw.blacklevel.as_bayer_array()[0] as f64);
    let self_scaled = scale_keep_negative(&mut raw);
    // Camera-native RGB (before white balance and color matrix)
    let mut steps = vec![ProcessingStep::Demosaic, ProcessingStep::FujiRotate, ProcessingStep::CropActiveArea, ProcessingStep::CropDefault];
    if !self_scaled {
        steps.insert(0, ProcessingStep::Rescale);
    }
    let native = RawDevelop::new_with(&steps);
    let inter = native.develop_intermediate(&raw).map_err(|e| anyhow!("{}", trf!("현상 실패: {e}", "Development failed: {e}")))?;
    let (w, h) = (inter.dim().w, inter.dim().h);
    match inter {
        Intermediate::ThreeColor(p) => {
            let data: Vec<f32> = p.data.into_iter().flatten().collect();
            let wb = raw.wb_coeffs;
            let mut neutral = if wb[0].is_finite() && wb[0] > 0.0 && wb[1] > 0.0 && wb[2] > 0.0 {
                [1.0 / wb[0] as f64, 1.0 / wb[1] as f64, 1.0 / wb[2] as f64]
            } else {
                [1.0, 1.0, 1.0]
            };
            if let Some(n) = tags.as_ref().and_then(|t| t.as_shot_neutral) {
                neutral = n;
            }
            let m = neutral[0].max(neutral[1]).max(neutral[2]).max(1e-9);
            neutral = [neutral[0] / m, neutral[1] / m, neutral[2] / m];
            // Fallback profile based on the rawler color matrix (D65)
            let cm = raw
                .color_matrix_find_first([rawler::imgop::xyz::Illuminant::D65, rawler::imgop::xyz::Illuminant::D50, rawler::imgop::xyz::Illuminant::A])
                .filter(|(_, m)| m.len() >= 9);
            let fallback = dcp::Profile {
                name: "Matrix".into(),
                cm1: Some(match &cm {
                    Some((_, m)) => [[m[0] as f64, m[1] as f64, m[2] as f64], [m[3] as f64, m[4] as f64, m[5] as f64], [m[6] as f64, m[7] as f64, m[8] as f64]],
                    None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                }),
                illum1: 21,
                ..Default::default()
            };
            let camera = format!("{} {}", raw.clean_make, raw.clean_model).trim().to_string();
            // DNG uses the file's BaselineExposure; other RAW files use per-camera measured values
            let baseline_exposure = match &tags {
                Some(t) => t.baseline_exposure,
                None => dcp::camera_baseline_exposure(&camera) + dcp::white_level_offset(&camera, white0, black0) + low_iso_offset(path, &camera) + black512_offset(&camera, white0, black0) + mode_offset(path),
            };
            let rc = dcp::RawColor {
                camera,
                neutral,
                baseline_exposure,
                embedded: tags.and_then(|t| t.profile).map(std::sync::Arc::new),
                fallback: std::sync::Arc::new(fallback),
                hdr: false,
                decoder_fallback: raw.camera.remark.as_deref().and_then(|r| r.strip_prefix("darkroom-fallback:")).map(|s| s.to_string()),
            };
            let (img, full) = aspect_crop(LinearImage { w, h, data }, path);
            Ok((img.oriented(orient), Some(rc), full))
        }
        _ => {
            // 4-color CFA and monochrome sensors: plain rawler path
            let dev = RawDevelop::new_with(&[
                ProcessingStep::Rescale,
                ProcessingStep::Demosaic,
                ProcessingStep::FujiRotate,
                ProcessingStep::CropActiveArea,
                ProcessingStep::WhiteBalance,
                ProcessingStep::Calibrate,
                ProcessingStep::CropDefault,
            ]);
            let inter = dev.develop_intermediate(&raw).map_err(|e| anyhow!("{}", trf!("현상 실패: {e}", "Development failed: {e}")))?;
            let (w, h) = (inter.dim().w, inter.dim().h);
            let data: Vec<f32> = match inter {
                Intermediate::ThreeColor(p) => p.data.into_iter().flatten().collect(),
                Intermediate::Monochrome(p) => p.data.into_iter().flat_map(|v| [v, v, v]).collect(),
                Intermediate::FourColor(p) => p.data.into_iter().flat_map(|v| [v[0], v[1], v[2]]).collect(),
            };
            let (img, full) = aspect_crop(LinearImage { w, h, data }, path);
            Ok((img.oriented(orient), None, full))
        }
    }
}

fn rgba8_to_linear(data: &[u8], w: usize, h: usize) -> LinearImage {
    let t = srgb8_to_linear_table();
    let mut out = LinearImage::new(w, h);
    out.data.par_chunks_mut(3).zip(data.par_chunks(4)).for_each(|(o, p)| {
        o[0] = t[p[0] as usize];
        o[1] = t[p[1] as usize];
        o[2] = t[p[2] as usize];
    });
    out
}

fn dynamic_to_linear(img: image::DynamicImage) -> LinearImage {
    use image::DynamicImage as D;
    match img {
        D::ImageRgb16(_) | D::ImageRgba16(_) | D::ImageLuma16(_) | D::ImageLumaA16(_) => {
            let rgb = img.into_rgb16();
            let (w, h) = rgb.dimensions();
            let t = srgb16_to_linear_table();
            let raw = rgb.into_raw();
            let data: Vec<f32> = raw.par_iter().map(|v| t[*v as usize]).collect();
            LinearImage { w: w as usize, h: h as usize, data }
        }
        D::ImageRgb32F(_) | D::ImageRgba32F(_) => {
            let rgb = img.into_rgb32f();
            let (w, h) = rgb.dimensions();
            let data: Vec<f32> = rgb.into_raw().par_iter().map(|v| crate::develop::color::srgb_decode(*v)).collect();
            LinearImage { w: w as usize, h: h as usize, data }
        }
        _ => {
            let rgba = img.into_rgba8();
            let (w, h) = rgba.dimensions();
            rgba8_to_linear(rgba.as_raw(), w as usize, h as usize)
        }
    }
}

/// 8-bit image for thumbnails/previews (orientation applied). For RAW the embedded JPEG is preferred (fast).
pub fn decode_preview(path: &Path, max_edge: u32, orientation: u16) -> Result<Rgba8> {
    no_panic(|| decode_preview_inner(path, max_edge, orientation))
}

fn decode_preview_inner(path: &Path, max_edge: u32, orientation: u16) -> Result<Rgba8> {
    if path.extension().map(|e| e.eq_ignore_ascii_case("drhdr")).unwrap_or(false) {
        let src = decode_source(path, 1)?;
        return Ok(render_preview_with(&src, &crate::develop::settings::DevelopSettings::default_for(true), max_edge));
    }
    let kind = kind_of(path).ok_or_else(|| anyhow!("{}", trf!("지원하지 않는 형식", "Unsupported format")))?;
    match kind {
        Kind::Raw => {
            use rawler::decoders::RawDecodeParams;
            let src = rawler::rawsource::RawSource::new(path)?;
            let dec = rawler::get_decoder(&src).map_err(|e| anyhow!("{e}"))?;
            let p = RawDecodeParams::default();
            let mut best: Option<image::DynamicImage> = None;
            // Pick the smallest embedded image that is at least the required size
            for cand in [dec.thumbnail_image(&src, &p), dec.preview_image(&src, &p), dec.full_image(&src, &p)] {
                if let Ok(Some(img)) = cand {
                    let edge = img.width().max(img.height());
                    let better = match &best {
                        None => true,
                        Some(b) => {
                            let be = b.width().max(b.height());
                            (be < max_edge && edge > be) || (edge >= max_edge && edge < be)
                        }
                    };
                    if better {
                        best = Some(img);
                    }
                    if best.as_ref().map(|b| b.width().max(b.height()) >= max_edge).unwrap_or(false) {
                        break;
                    }
                }
            }
            match best {
                Some(img) => Ok(Rgba8::from_dynamic(img).fit(max_edge).oriented(orientation)),
                None => {
                    // No embedded preview: run a real develop
                    let srcimg = decode_source(path, orientation)?;
                    Ok(render_default_preview(&srcimg, max_edge))
                }
            }
        }
        Kind::Raster => {
            let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?;
            Ok(Rgba8::from_dynamic(img).fit(max_edge).oriented(orientation))
        }
        Kind::Heif => {
            let (data, w, h) = heic::decode_rgba(path, Some(max_edge))?;
            Ok(Rgba8 { w, h, data }.oriented(orientation))
        }
    }
}

/// Develops the original with default settings to make a preview.
pub fn render_default_preview(src: &SourceImage, max_edge: u32) -> Rgba8 {
    let s = crate::develop::settings::DevelopSettings::default_for(src.is_raw);
    render_preview_with(src, &s, max_edge)
}

pub fn render_preview_with(src: &SourceImage, s: &crate::develop::settings::DevelopSettings, max_edge: u32) -> Rgba8 {
    use crate::develop::geometry::cropped_dims;
    use crate::develop::pipeline::{Engine, RenderRequest, fit_size};
    let (cw, ch) = cropped_dims(src.width(), src.height(), &s.geometry);
    let (w, h) = fit_size(cw, ch, max_edge as usize, max_edge as usize);
    let mut e = Engine::default();
    let out = e.render(
        src,
        &RenderRequest {
            settings: s,
            out_w: w,
            out_h: h,
            region: [0.0, 0.0, 1.0, 1.0],
            draft: false,
            clipping: false,
            mask_overlay: None,
            keep_float: false,
        },
    );
    Rgba8 { w: w as u32, h: h as u32, data: out.rgba }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// Benchmark: DARKROOM_SAMPLE=<path> cargo test --release bench_sample -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_sample() {
        let Some(p) = std::env::var_os("DARKROOM_SAMPLE") else { return };
        let path = std::path::PathBuf::from(p);
        let t = Instant::now();
        let meta = crate::imaging::meta::read_raw_meta(&path).or_else(|| crate::imaging::meta::read_exif(&path));
        println!("meta {:?} in {:?}", meta.as_ref().map(|m| (&m.model, m.orientation, m.summary())), t.elapsed());
        let o = meta.map(|m| m.orientation).unwrap_or(1);
        let t = Instant::now();
        let pv = decode_preview(&path, config::THUMB_SIZE, o).unwrap();
        println!("thumb {}x{} in {:?}", pv.w, pv.h, t.elapsed());
        let t = Instant::now();
        let src = decode_source(&path, o).unwrap();
        println!("full decode {}x{} levels={} in {:?}", src.width(), src.height(), src.levels.len(), t.elapsed());
        let mut s = crate::develop::settings::DevelopSettings::default_for(src.is_raw);
        let mut e = crate::develop::pipeline::Engine::default();
        for (label, w, h) in [("fit 1600", 1600usize, 1067usize), ("fit 2560", 2560, 1707)] {
            for pass in 0..2 {
                let t = Instant::now();
                let _ = e.render(&src, &crate::develop::pipeline::RenderRequest {
                    settings: &s, out_w: w, out_h: h, region: [0.0, 0.0, 1.0, 1.0], draft: false,
                    clipping: false, mask_overlay: None, keep_float: false });
                println!("render {label} pass{pass}: {:?}", t.elapsed());
            }
        }
        s.highlights = -50.0; s.shadows = 40.0; s.clarity = 30.0; s.texture = 20.0; s.dehaze = 10.0; s.vibrance = 20.0;
        s.detail.nr_luma = 20.0;
        for pass in 0..3 {
            s.exposure = pass as f32 * 0.1;
            let t = Instant::now();
            let _ = e.render(&src, &crate::develop::pipeline::RenderRequest {
                settings: &s, out_w: 2560, out_h: 1707, region: [0.0, 0.0, 1.0, 1.0], draft: false,
                clipping: false, mask_overlay: None, keep_float: false });
            println!("render heavy 2560 pass{pass}: {:?}", t.elapsed());
        }
        let t = Instant::now();
        let out = render_preview_with(&src, &s, 1600);
        image::save_buffer(std::env::temp_dir().join("darkroom_bench.png"), &out.data, out.w, out.h, image::ExtendedColorType::Rgba8).unwrap();
        println!("saved preview in {:?}", t.elapsed());
    }
}

#[cfg(test)]
mod black_probe {
    /// File black level vs. optical black area mean. DARKROOM_PROBE=<folder>
    #[test]
    #[ignore]
    fn black_levels() {
        let Some(dir) = std::env::var_os("DARKROOM_PROBE") else { return };
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let Ok(src) = rawler::rawsource::RawSource::new(&p) else { continue };
            let Ok(dec) = rawler::get_decoder(&src) else { continue };
            let Ok(raw) = dec.raw_image(&src, &rawler::decoders::RawDecodeParams::default(), false) else { continue };
            let rawler::RawImageData::Integer(d) = &raw.data else { continue };
            let mut acc = [0.0f64; 4];
            let mut n = [0.0f64; 4];
            let mut areas = raw.blackareas.clone();
            if areas.is_empty() {
                if let Some(a) = raw.active_area {
                    // CR3: the strip left of the active area is the optical black area
                    if a.p.x > 24 {
                        areas.push(rawler::imgop::Rect::new(rawler::imgop::Point::new(8, a.p.y), rawler::imgop::Dim2::new(a.p.x - 16, a.d.h)));
                    }
                }
            }
            for r in &areas {
                for y in r.p.y..r.p.y + r.d.h {
                    for x in r.p.x..r.p.x + r.d.w {
                        let c = (y % 2) * 2 + (x % 2);
                        acc[c] += d[y * raw.width + x] as f64;
                        n[c] += 1.0;
                    }
                }
            }
            let ob: Vec<String> = (0..4).map(|c| format!("{:.1}", acc[c] / n[c].max(1.0))).collect();
            println!(
                "{} [{} {}] black={:?} white={:?} areas={:?} active={:?} OB=[{}]",
                p.file_name().unwrap().to_string_lossy(),
                raw.clean_make,
                raw.clean_model,
                raw.blacklevel.levels.iter().map(|r| r.as_f32()).collect::<Vec<_>>(),
                raw.whitelevel,
                raw.blackareas,
                raw.active_area,
                ob.join(", ")
            );
        }
    }
}

#[cfg(test)]
mod neg_probe {
    #[test]
    #[ignore]
    fn negatives_kept() {
        let Some(p) = std::env::var_os("DARKROOM_PROBE_FILE") else { return };
        let (img, _, _) = super::decode_raw_linear(std::path::Path::new(&p), 1).unwrap();
        let neg = img.data.iter().filter(|v| **v < 0.0).count();
        let mean: f64 = img.data.iter().map(|v| *v as f64).sum::<f64>() / img.data.len() as f64;
        println!("negatives {neg} / {}  mean {mean:.5}", img.data.len());
    }
}

#[cfg(test)]
mod level_probe {
    /// Prints rawler black/white levels for each RAW in DARKROOM_LEVEL_DIR
    #[test]
    #[ignore]
    fn raw_levels() {
        let Ok(d) = std::env::var("DARKROOM_LEVEL_DIR") else { return };
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if !p.extension().map(|x| x.eq_ignore_ascii_case("cr3") || x.eq_ignore_ascii_case("cr2")).unwrap_or(false) {
                continue;
            }
            let src = rawler::rawsource::RawSource::new(&p).unwrap();
            let dec = rawler::get_decoder(&src).unwrap();
            let raw = dec.raw_image(&src, &rawler::decoders::RawDecodeParams::default(), false).unwrap();
            eprintln!("{}: white {:?} black {:?}", p.file_name().unwrap().to_string_lossy(), raw.whitelevel.as_bayer_array(), raw.blacklevel.as_bayer_array());
        }
    }
}
