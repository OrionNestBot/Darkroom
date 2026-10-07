//! Print layout: places photos on pages by paper size, grid, margins and captions; saves a multi-page PDF or one JPEG per page.
//! Each page is rendered at the chosen resolution, so captions and color management match the screen.

use crate::catalog::Photo;
use crate::develop::geometry::cropped_dims;
use crate::develop::pipeline::Engine;
use crate::develop::settings::DevelopSettings;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Paper {
    #[default]
    A4,
    A3,
    A5,
    Letter,
    P4x6,
    P5x7,
    P8x10,
}

impl Paper {
    pub const ALL: [Paper; 7] = [Paper::A4, Paper::A3, Paper::A5, Paper::Letter, Paper::P4x6, Paper::P5x7, Paper::P8x10];
    pub fn name(self) -> &'static str {
        match self {
            Paper::A4 => "A4",
            Paper::A3 => "A3",
            Paper::A5 => "A5",
            Paper::Letter => "Letter",
            Paper::P4x6 => "4×6 in (10×15)",
            Paper::P5x7 => "5×7 in",
            Paper::P8x10 => "8×10 in",
        }
    }
    /// Portrait dimensions in mm.
    pub fn mm(self) -> (f32, f32) {
        match self {
            Paper::A4 => (210.0, 297.0),
            Paper::A3 => (297.0, 420.0),
            Paper::A5 => (148.0, 210.0),
            Paper::Letter => (215.9, 279.4),
            Paper::P4x6 => (101.6, 152.4),
            Paper::P5x7 => (127.0, 177.8),
            Paper::P8x10 => (203.2, 254.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Caption {
    #[default]
    None,
    FileName,
    Title,
    Exif,
}

impl Caption {
    pub const ALL: [Caption; 4] = [Caption::None, Caption::FileName, Caption::Title, Caption::Exif];
    pub fn name(self) -> &'static str {
        match self {
            Caption::None => tr!("없음", "None"),
            Caption::FileName => tr!("파일 이름", "File name"),
            Caption::Title => tr!("제목", "Title"),
            Caption::Exif => tr!("촬영 정보", "Shooting info"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintLayout {
    pub paper: Paper,
    pub landscape: bool,
    pub margin_mm: f32,
    pub rows: u32,
    pub cols: u32,
    pub gap_mm: f32,
    /// true = crop to fill the cell, false = fit inside the cell.
    pub fill: bool,
    /// Rotate photos 90° to match the cell orientation.
    pub auto_rotate: bool,
    pub caption: Caption,
    pub dpi: u32,
    pub pdf: bool,
    pub sharpen: bool,
}

impl Default for PrintLayout {
    fn default() -> Self {
        Self { paper: Paper::A4, landscape: false, margin_mm: 10.0, rows: 1, cols: 1, gap_mm: 5.0, fill: false, auto_rotate: true, caption: Caption::None, dpi: 300, pdf: true, sharpen: true }
    }
}

impl PrintLayout {
    pub fn page_mm(&self) -> (f32, f32) {
        let (w, h) = self.paper.mm();
        if self.landscape { (h, w) } else { (w, h) }
    }
    pub fn per_page(&self) -> usize {
        (self.rows.max(1) * self.cols.max(1)) as usize
    }
    /// Cell rectangles on the page in mm: (x, y, w, h), including caption height.
    pub fn cells_mm(&self) -> Vec<(f32, f32, f32, f32)> {
        let (pw, ph) = self.page_mm();
        let (r, c) = (self.rows.max(1) as f32, self.cols.max(1) as f32);
        let cw = (pw - 2.0 * self.margin_mm - (c - 1.0) * self.gap_mm) / c;
        let ch = (ph - 2.0 * self.margin_mm - (r - 1.0) * self.gap_mm) / r;
        let mut v = Vec::new();
        for i in 0..self.rows.max(1) {
            for j in 0..self.cols.max(1) {
                v.push((self.margin_mm + j as f32 * (cw + self.gap_mm), self.margin_mm + i as f32 * (ch + self.gap_mm), cw.max(1.0), ch.max(1.0)));
            }
        }
        v
    }
    pub fn caption_mm(&self) -> f32 {
        if self.caption == Caption::None { 0.0 } else { (self.cells_mm()[0].3 * 0.07).clamp(3.0, 9.0) }
    }
}

fn caption_text(p: &Photo, c: Caption) -> String {
    match c {
        Caption::None => String::new(),
        Caption::FileName => p.display_name(),
        Caption::Title => {
            if p.title.is_empty() {
                p.display_name()
            } else {
                p.title.clone()
            }
        }
        Caption::Exif => {
            let m = &p.meta;
            let mut parts = Vec::new();
            if let Some(f) = m.focal {
                parts.push(format!("{f:.0}mm"));
            }
            if let Some(n) = m.fnumber {
                parts.push(format!("f/{n:.1}"));
            }
            let e = m.exposure_label();
            if !e.is_empty() {
                parts.push(e);
            }
            if let Some(i) = m.iso {
                parts.push(format!("ISO {i}"));
            }
            parts.join("  ·  ")
        }
    }
}

/// Renders one page (sRGB gamma 0..1, white background).
pub fn render_page(items: &[(Photo, DevelopSettings)], lay: &PrintLayout, engine: &mut Engine) -> (Vec<f32>, usize, usize) {
    let (pw_mm, ph_mm) = lay.page_mm();
    let px = lay.dpi as f32 / 25.4;
    let (pw, ph) = ((pw_mm * px).round() as usize, (ph_mm * px).round() as usize);
    let mut page = vec![1.0f32; pw * ph * 3];
    let cap_mm = lay.caption_mm();
    for ((p, s), (cx, cy, cw, ch)) in items.iter().zip(lay.cells_mm()) {
        let Ok(src) = crate::imaging::decode::decode_source(&p.path, p.meta.orientation) else { continue };
        let mut s = s.clone();
        let (iw, ih) = cropped_dims(src.width(), src.height(), &s.geometry);
        let (bx, by) = ((cx * px).round() as usize, (cy * px).round() as usize);
        let (bw, bh) = ((cw * px).round() as usize, ((ch - cap_mm) * px).round().max(8.0) as usize);
        // Rotate when the cell and photo orientations differ.
        let cell_land = bw >= bh;
        let img_land = iw >= ih;
        let (iw, ih) = if lay.auto_rotate && cell_land != img_land {
            s.geometry.rotate90 = (s.geometry.rotate90 + 1) % 4;
            s.geometry.crop = {
                let c = s.geometry.crop;
                // Same region after a 90° rotation: (x0,y0,x1,y1) -> (1-y1, x0, 1-y0, x1).
                [1.0 - c[3], c[0], 1.0 - c[1], c[2]]
            };
            (ih, iw)
        } else {
            (iw, ih)
        };
        let ia = iw as f32 / ih as f32;
        let ca = bw as f32 / bh as f32;
        let (rw, rh) = if lay.fill == (ia > ca) { ((bh as f32 * ia).round() as usize, bh) } else { (bw, (bw as f32 / ia).round() as usize) };
        let (rw, rh) = (rw.max(1), rh.max(1));
        let mut img = super::render_full(&src, &s, rw, rh, engine);
        if lay.sharpen {
            super::output_sharpen(&mut img, rw, rh, super::SharpenFor::Matte, super::SharpenLevel::Standard);
        }
        // Center in the cell and clip anything outside it.
        let ox = bx as i64 + (bw as i64 - rw as i64) / 2;
        let oy = by as i64 + (bh as i64 - rh as i64) / 2;
        for y in 0..rh {
            let py = oy + y as i64;
            if py < by as i64 || py >= (by + bh) as i64 || py < 0 || py >= ph as i64 {
                continue;
            }
            for x in 0..rw {
                let pxx = ox + x as i64;
                if pxx < bx as i64 || pxx >= (bx + bw) as i64 || pxx < 0 || pxx >= pw as i64 {
                    continue;
                }
                let si = (y * rw + x) * 3;
                let di = (py as usize * pw + pxx as usize) * 3;
                page[di..di + 3].copy_from_slice(&img[si..si + 3]);
            }
        }
        // Caption
        let text = caption_text(p, lay.caption);
        if !text.is_empty() {
            let capw = bw;
            let caph = (cap_mm * px).round().max(8.0) as usize;
            let mut strip = vec![1.0f32; capw * caph * 3];
            let wm = super::watermark::Watermark {
                text,
                color: [60, 60, 60],
                shadow: false,
                stroke: false,
                size_mode: super::watermark::WmSize::Fill,
                inset_h: 2.0,
                inset_v: 30.0,
                anchor: 4,
                opacity: 100.0,
                ..Default::default()
            };
            let ctx = super::token_ctx(p, "", "");
            super::watermark::apply_f32(&mut strip, capw, caph, &wm, &ctx);
            let top = by + bh;
            for y in 0..caph {
                if top + y >= ph {
                    break;
                }
                let di = ((top + y) * pw + bx) * 3;
                let n = capw.min(pw - bx) * 3;
                page[di..di + n].copy_from_slice(&strip[y * capw * 3..y * capw * 3 + n]);
            }
        }
    }
    (page, pw, ph)
}

fn to_jpeg(page: &[f32], w: usize, h: usize, q: u8) -> Result<Vec<u8>> {
    let rgb: Vec<u8> = page.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q);
    enc.encode(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
    Ok(buf)
}

/// Minimal PDF writer: one full-page JPEG per page.
pub fn write_pdf(path: &Path, pages: &[(Vec<u8>, usize, usize)], page_mm: (f32, f32)) -> Result<()> {
    let (wpt, hpt) = (page_mm.0 / 25.4 * 72.0, page_mm.1 / 25.4 * 72.0);
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let n = pages.len();
    // Object numbers: 1 catalog, 2 page tree, then (page, contents, image) per page.
    let page_obj = |i: usize| 3 + i * 3;
    let push_obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(body);
    };
    push_obj(&mut out, &mut offsets, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", page_obj(i))).collect();
    push_obj(&mut out, &mut offsets, format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {n} >>\nendobj\n", kids.join(" ")).as_bytes());
    for (i, (jpg, w, h)) in pages.iter().enumerate() {
        let (po, co, io) = (page_obj(i), page_obj(i) + 1, page_obj(i) + 2);
        push_obj(
            &mut out,
            &mut offsets,
            format!("{po} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {wpt:.2} {hpt:.2}] /Resources << /XObject << /Im0 {io} 0 R >> >> /Contents {co} 0 R >>\nendobj\n").as_bytes(),
        );
        let content = format!("q {wpt:.2} 0 0 {hpt:.2} 0 0 cm /Im0 Do Q");
        push_obj(&mut out, &mut offsets, format!("{co} 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n", content.len()).as_bytes());
        let mut img = format!("{io} 0 obj\n<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n", jpg.len()).into_bytes();
        img.extend_from_slice(jpg);
        img.extend_from_slice(b"\nendstream\nendobj\n");
        push_obj(&mut out, &mut offsets, &img);
    }
    let xref = out.len();
    let total = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}

/// Runs a full print job: renders pages and saves one PDF or one JPEG per page. Returns the saved files.
pub fn run(items: Vec<(Photo, DevelopSettings)>, lay: &PrintLayout, out: &Path, progress: &dyn Fn(usize, usize)) -> Result<Vec<std::path::PathBuf>> {
    let per = lay.per_page();
    let chunks: Vec<&[(Photo, DevelopSettings)]> = items.chunks(per).collect();
    let mut engine = Engine::default();
    let mut jpgs = Vec::new();
    let mut files = Vec::new();
    for (i, ch) in chunks.iter().enumerate() {
        progress(i, chunks.len());
        let (page, w, h) = render_page(ch, lay, &mut engine);
        let jpg = to_jpeg(&page, w, h, 93)?;
        if lay.pdf {
            jpgs.push((jpg, w, h));
        } else {
            let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "print".into());
            let p = out.with_file_name(format!("{stem}-{:02}.jpg", i + 1));
            std::fs::write(&p, jpg)?;
            files.push(p);
        }
    }
    if lay.pdf {
        write_pdf(out, &jpgs, lay.page_mm())?;
        files.push(out.to_path_buf());
    }
    progress(chunks.len(), chunks.len());
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_structure() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.pdf");
        let page = vec![0.5f32; 30 * 40 * 3];
        let jpg = to_jpeg(&page, 30, 40, 90).unwrap();
        write_pdf(&p, &[(jpg.clone(), 30, 40), (jpg, 30, 40)], (210.0, 297.0)).unwrap();
        let b = std::fs::read(&p).unwrap();
        let t = String::from_utf8_lossy(&b);
        assert!(t.starts_with("%PDF-1.4"));
        assert!(t.contains("/Count 2"));
        assert!(t.contains("startxref"));
        // xref offsets must match the actual object positions (in bytes).
        // Use the real xref table ("\nxref\n"), not "startxref".
        let xi = b.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 1;
        let tail = String::from_utf8_lossy(&b[xi..]).to_string();
        let first = tail.lines().nth(3).unwrap();
        let off: usize = first[..10].parse().unwrap();
        assert!(b[off..].starts_with(b"1 0 obj"));
    }

    #[test]
    fn grid_cells() {
        let l = PrintLayout { rows: 2, cols: 3, margin_mm: 10.0, gap_mm: 5.0, ..Default::default() };
        let c = l.cells_mm();
        assert_eq!(c.len(), 6);
        let (pw, _) = l.page_mm();
        let last = c[2];
        assert!((last.0 + last.2 - (pw - 10.0)).abs() < 0.01);
    }
}
