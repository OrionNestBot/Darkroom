//! Linear RGB float image and resolution pyramid.

use rayon::prelude::*;
use std::sync::Arc;

/// Scene-linear RGB with sRGB primaries, interleaved. 1.0 = sensor white.
#[derive(Clone)]
pub struct LinearImage {
    pub w: usize,
    pub h: usize,
    pub data: Vec<f32>,
}

impl LinearImage {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, data: vec![0.0; w * h * 3] }
    }

    #[inline]
    pub fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.w + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// 2x box downsample (parallel).
    pub fn half(&self) -> LinearImage {
        let w2 = (self.w / 2).max(1);
        let h2 = (self.h / 2).max(1);
        let mut out = LinearImage::new(w2, h2);
        let sw = self.w;
        let src = &self.data;
        let (sw_max, sh_max) = (self.w - 1, self.h - 1);
        out.data.par_chunks_mut(w2 * 3).enumerate().for_each(|(y, row)| {
            let y0 = (y * 2).min(sh_max);
            let y1 = (y * 2 + 1).min(sh_max);
            for x in 0..w2 {
                let x0 = (x * 2).min(sw_max);
                let x1 = (x * 2 + 1).min(sw_max);
                for c in 0..3 {
                    let s = src[(y0 * sw + x0) * 3 + c]
                        + src[(y0 * sw + x1) * 3 + c]
                        + src[(y1 * sw + x0) * 3 + c]
                        + src[(y1 * sw + x1) * 3 + c];
                    row[x * 3 + c] = s * 0.25;
                }
            }
        });
        out
    }

    /// Apply EXIF orientation (1..8).
    pub fn oriented(self, orientation: u16) -> LinearImage {
        if orientation <= 1 || orientation > 8 {
            return self;
        }
        let (w, h) = (self.w, self.h);
        let swap = orientation >= 5;
        let (nw, nh) = if swap { (h, w) } else { (w, h) };
        let mut out = LinearImage::new(nw, nh);
        let src = &self.data;
        out.data.par_chunks_mut(nw * 3).enumerate().for_each(|(ny, row)| {
            for nx in 0..nw {
                // Output (nx, ny) to source (sx, sy)
                let (sx, sy) = match orientation {
                    2 => (w - 1 - nx, ny),
                    3 => (w - 1 - nx, h - 1 - ny),
                    4 => (nx, h - 1 - ny),
                    5 => (ny, nx),
                    6 => (ny, h - 1 - nx),
                    7 => (w - 1 - ny, h - 1 - nx),
                    8 => (w - 1 - ny, nx),
                    _ => (nx, ny),
                };
                let si = (sy * w + sx) * 3;
                row[nx * 3..nx * 3 + 3].copy_from_slice(&src[si..si + 3]);
            }
        });
        out
    }
}

/// Decoded source plus pyramid. levels[0] = full resolution.
pub struct SourceImage {
    pub levels: Vec<Arc<LinearImage>>,
    pub is_raw: bool,
    /// Source already rendered for display (JPEG etc.), not scene-linear
    pub display_referred: bool,
    /// RAW color info for DNG-style color processing. When present, data is camera-native RGB (before white balance).
    pub raw_color: Option<Arc<super::dcp::RawColor>>,
    /// Capture info for lens correction
    pub lens_info: Option<Arc<LensInfo>>,
}

/// Capture info for lens profile selection and interpolation
#[derive(Clone, Debug, Default)]
pub struct LensInfo {
    pub make: String,
    pub model: String,
    pub lens: String,
    pub focal: f32,
    pub fnumber: f32,
    /// EXIF orientation (1/3/6/8)
    pub orientation: u16,
    /// Lens correction data recorded by the camera in the file (Sony, Fujifilm, Olympus, Panasonic, DNG)
    pub embedded: Option<Arc<super::embedded::Embedded>>,
    /// Size before the in-camera aspect ratio crop (sensor orientation); lens correction coordinates refer to this full frame
    pub full_dims: Option<(u32, u32)>,
    /// Crop factor from the EXIF 35mm-equivalent focal length (for cameras missing from the lensfun database)
    pub crop: Option<f32>,
    /// Camera already applied vignetting correction to the RAW (Panasonic 0x008a, Olympus 0x050c), so vignetting correction is skipped
    pub shading_comp: bool,
}

impl SourceImage {
    pub fn new(base: LinearImage, is_raw: bool, min_edge: usize) -> Self {
        let mut levels = vec![Arc::new(base)];
        loop {
            let last = levels.last().unwrap();
            if last.w.max(last.h) / 2 < min_edge {
                break;
            }
            let next = last.half();
            levels.push(Arc::new(next));
        }
        Self { levels, is_raw, display_referred: !is_raw, raw_color: None, lens_info: None }
    }

    pub fn width(&self) -> usize {
        self.levels[0].w
    }
    pub fn height(&self) -> usize {
        self.levels[0].h
    }

    /// Pick the pyramid level for the given output-per-base-pixel scale (`out_per_base`):
    /// the smallest level that needs no upsampling.
    pub fn level_for(&self, out_per_base: f32) -> (usize, &Arc<LinearImage>) {
        let mut best = 0;
        for (i, _) in self.levels.iter().enumerate() {
            let lvl_scale = 1.0 / (1u32 << i) as f32;
            if lvl_scale >= out_per_base * 0.999 {
                best = i;
            }
        }
        (best, &self.levels[best])
    }
}

/// Bilinear sample (pixel-center coordinates, edge clamped).
#[inline]
pub fn sample_bilinear(img: &LinearImage, x: f32, y: f32) -> [f32; 3] {
    let fx = (x - 0.5).clamp(0.0, (img.w - 1) as f32);
    let fy = (y - 0.5).clamp(0.0, (img.h - 1) as f32);
    let x0 = fx as usize;
    let y0 = fy as usize;
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let d = &img.data;
    let i00 = (y0 * img.w + x0) * 3;
    let i10 = (y0 * img.w + x1) * 3;
    let i01 = (y1 * img.w + x0) * 3;
    let i11 = (y1 * img.w + x1) * 3;
    let mut o = [0.0f32; 3];
    for c in 0..3 {
        let a = d[i00 + c] + (d[i10 + c] - d[i00 + c]) * tx;
        let b = d[i01 + c] + (d[i11 + c] - d[i01 + c]) * tx;
        o[c] = a + (b - a) * ty;
    }
    o
}
