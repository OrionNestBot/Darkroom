//! Face detection using the built-in Windows FaceDetector (WinRT); no model files or network.
//! Returned boxes are normalized [x, y, w, h] relative to the input image.

use anyhow::{Result, anyhow};

pub fn detect(gray: &[u8], w: u32, h: u32) -> Result<Vec<[f32; 4]>> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, BitmapSize, SoftwareBitmap};
    use windows::Media::FaceAnalysis::FaceDetector;
    use windows::Security::Cryptography::CryptographicBuffer;
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
    }
    if !FaceDetector::IsSupported().unwrap_or(false) {
        return Err(anyhow!("{}", trf!("이 PC에서는 Windows 얼굴 감지를 사용할 수 없습니다", "Windows face detection isn't available on this PC")));
    }
    let det = FaceDetector::CreateAsync()?.join()?;
    let min = (w.min(h) / 60).max(20);
    let _ = det.SetMinDetectableFaceSize(BitmapSize { Width: min, Height: min });
    let bmp = SoftwareBitmap::Create(BitmapPixelFormat::Gray8, w as i32, h as i32)?;
    let buf = CryptographicBuffer::CreateFromByteArray(gray)?;
    bmp.CopyFromBuffer(&buf)?;
    let faces = det.DetectFacesAsync(&bmp)?.join()?;
    let mut out = Vec::new();
    for f in faces {
        let b = f.FaceBox()?;
        out.push([b.X as f32 / w as f32, b.Y as f32 / h as f32, b.Width as f32 / w as f32, b.Height as f32 / h as f32]);
    }
    Ok(out)
}

/// Detects faces in the source and returns privacy regions (normalized, padded to cover hair and chin).
pub fn auto_regions(src: &crate::develop::image::SourceImage, kind: crate::develop::settings::PrivacyKind, strength: f32) -> std::result::Result<Vec<crate::develop::settings::PrivacyRegion>, String> {
    let lvl = src.levels.iter().find(|l| l.w.max(l.h) <= 2400).cloned().unwrap_or_else(|| src.levels.last().unwrap().clone());
    let (g, gw, gh) = gray_from_linear(&lvl.data, lvl.w, lvl.h, 1600);
    let faces = detect(&g, gw, gh).map_err(|e| format!("{e:#}"))?;
    Ok(faces
        .iter()
        .map(|f| crate::develop::settings::PrivacyRegion {
            center: [f[0] + f[2] * 0.5, f[1] + f[3] * 0.48],
            radius: [f[2] * 0.62, f[3] * 0.78],
            ellipse: true,
            kind,
            strength,
            auto: true,
        })
        .collect())
}

/// Converts a linear RGB pyramid level to 8-bit gamma-encoded gray for detection, long edge at most `max_edge`.
pub fn gray_from_linear(data: &[f32], w: usize, h: usize, max_edge: usize) -> (Vec<u8>, u32, u32) {
    let k = (w.max(h) as f32 / max_edge as f32).max(1.0);
    let (ow, oh) = (((w as f32 / k) as usize).max(1), ((h as f32 / k) as usize).max(1));
    // Normalize brightness by the 99th percentile so dark photos still detect.
    let mut lum: Vec<f32> = data.as_chunks::<3>().0.iter().step_by(13).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let i = ((lum.len() as f32) * 0.99) as usize;
    let top = if lum.is_empty() {
        1.0
    } else {
        let i = i.min(lum.len() - 1);
        *lum.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1
    }
    .max(1e-4);
    let mut g = vec![0u8; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            let sx = ((x as f32 + 0.5) * k) as usize;
            let sy = ((y as f32 + 0.5) * k) as usize;
            let j = (sy.min(h - 1) * w + sx.min(w - 1)) * 3;
            let l = (0.2126 * data[j] + 0.7152 * data[j + 1] + 0.0722 * data[j + 2]) / top;
            g[y * ow + x] = (crate::develop::color::srgb_encode(l.clamp(0.0, 1.0)) * 255.0) as u8;
        }
    }
    (g, ow as u32, oh as u32)
}
