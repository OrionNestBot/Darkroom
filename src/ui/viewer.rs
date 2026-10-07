//! Photo viewer: zoom/pan, async render requests and texture display. Shared by the loupe, develop and compare views.

use super::theme::*;
use super::workers::{DevEvent, DevService, RenderJob};
use crate::catalog::PhotoId;
use crate::develop::geometry::{GeoMap, cropped_dims};
use crate::develop::image::SourceImage;
use crate::develop::pipeline::Histogram;
use crate::develop::settings::DevelopSettings;
use crate::imaging::decode::Rgba8;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, PartialEq)]
struct ReqKey {
    settings: DevelopSettings,
    w: usize,
    h: usize,
    region: [i32; 4],
    draft: bool,
    clipping: bool,
    overlay: Option<usize>,
    /// Bumped when lens profile data is downloaded, forcing a re-render
    lens_gen: u64,
}

pub struct Viewer {
    pub id: PhotoId,
    pub path: PathBuf,
    pub orientation: u16,
    pub slot: u8,
    pub source: Option<Arc<SourceImage>>,
    pub loading: bool,
    pub error: Option<String>,
    tex: Option<TextureHandle>,
    tex_region: [f32; 4],
    tex_gen: u64,
    /// Whole image at fit size: underlay while zoomed and the filmstrip live preview. Kept separate from the view texture
    fit_tex: Option<TextureHandle>,
    fit_gen: u64,
    last_fit_key: Option<ReqKey>,
    pub placeholder: Option<TextureHandle>,
    /// None = fit to screen, Some(z) = physical pixels per image pixel
    pub zoom: Option<f32>,
    /// View center, normalized to the cropped image
    pub center: [f32; 2],
    generation: u64,
    last_key: Option<ReqKey>,
    pub hist: Option<Histogram>,
    pub last_ms: f32,
    pub last_image: Option<Arc<Rgba8>>,
    last_view: Option<ViewInfo>,
    /// Disable wheel zoom (detail 1:1 preview)
    pub lock_zoom: bool,
}

#[derive(Clone)]
pub struct ViewInfo {
    pub canvas: Rect,
    /// Screen rect of the whole cropped image (may extend past the canvas)
    pub image_rect: Rect,
    pub bw: usize,
    pub bh: usize,
    pub settings_geo: crate::develop::settings::Geometry,
    pub lens: crate::develop::settings::LensCorrection,
    #[allow(dead_code)]
    pub ppp: f32,
}

impl ViewInfo {
    pub fn screen_to_cropnorm(&self, p: Pos2) -> [f32; 2] {
        [(p.x - self.image_rect.left()) / self.image_rect.width(), (p.y - self.image_rect.top()) / self.image_rect.height()]
    }
    pub fn cropnorm_to_screen(&self, c: [f32; 2]) -> Pos2 {
        pos2(self.image_rect.left() + c[0] * self.image_rect.width(), self.image_rect.top() + c[1] * self.image_rect.height())
    }
    fn geo(&self) -> GeoMap {
        GeoMap::new(
            self.bw,
            self.bh,
            &self.settings_geo,
            &self.lens,
            [0.0, 0.0, 1.0, 1.0],
            self.image_rect.width().max(1.0) as usize,
            self.image_rect.height().max(1.0) as usize,
        )
    }
    /// Screen position -> normalized source coordinates (mask coordinate space).
    pub fn screen_to_base(&self, p: Pos2) -> [f32; 2] {
        let g = self.geo();
        let sx = g.ow / self.image_rect.width();
        let sy = g.oh / self.image_rect.height();
        let (bx, by) = g.map((p.x - self.image_rect.left()) * sx, (p.y - self.image_rect.top()) * sy);
        [bx / self.bw as f32, by / self.bh as f32]
    }
    pub fn base_to_screen(&self, b: [f32; 2]) -> Pos2 {
        let g = self.geo();
        let (ox, oy) = g.inverse(b[0] * self.bw as f32, b[1] * self.bh as f32);
        let sx = self.image_rect.width() / g.ow;
        let sy = self.image_rect.height() / g.oh;
        pos2(self.image_rect.left() + ox * sx, self.image_rect.top() + oy * sy)
    }
    /// Fraction of the image's long edge -> screen length.
    pub fn long_edge_screen(&self) -> f32 {
        let (cw, _) = cropped_dims(self.bw, self.bh, &self.settings_geo);
        let z = self.image_rect.width() / cw as f32;
        self.bw.max(self.bh) as f32 * z
    }
}

impl Viewer {
    pub fn new(id: PhotoId, path: PathBuf, orientation: u16, slot: u8, dev: &DevService) -> Self {
        dev.open(id, path.clone(), orientation);
        Self {
            id,
            path,
            orientation,
            slot,
            source: None,
            loading: true,
            error: None,
            tex: None,
            tex_region: [0.0, 0.0, 1.0, 1.0],
            tex_gen: 0,
            lock_zoom: false,
            fit_tex: None,
            fit_gen: 0,
            last_fit_key: None,
            placeholder: None,
            zoom: None,
            center: [0.5, 0.5],
            generation: 0,
            last_key: None,
            hist: None,
            last_ms: 0.0,
            last_image: None,
            last_view: None,
        }
    }

    pub fn handle(&mut self, ctx: &egui::Context, ev: &DevEvent) {
        match ev {
            DevEvent::Opened { id, source } if *id == self.id => {
                self.source = Some(source.clone());
                self.loading = false;
                self.last_key = None;
            }
            DevEvent::Failed { id, error } if *id == self.id => {
                self.loading = false;
                self.error = Some(error.clone());
            }
            DevEvent::Rendered { id, generation, slot, underlay: true, image, .. } if *id == self.id && *slot == self.slot => {
                if *generation < self.fit_gen {
                    return;
                }
                self.fit_gen = *generation;
                let ci = egui::ColorImage::from_rgba_unmultiplied([image.w as usize, image.h as usize], &image.data);
                self.set_fit(ctx, ci);
            }
            DevEvent::Rendered { id, generation, slot, region, draft: _, image, hist, ms, .. } if *id == self.id && *slot == self.slot => {
                if *generation < self.tex_gen {
                    return;
                }
                self.tex_gen = *generation;
                let ci = egui::ColorImage::from_rgba_unmultiplied([image.w as usize, image.h as usize], &image.data);
                let opts = TextureOptions::LINEAR;
                match &mut self.tex {
                    Some(t) if t.size() == [image.w as usize, image.h as usize] => t.set(ci, opts),
                    _ => self.tex = Some(ctx.load_texture(format!("view{}", self.slot), ci, opts)),
                }
                self.tex_region = *region;
                // Keep the full fit render as the underlay too (copied: sharing the texture would let the zoomed render overwrite it)
                if *region == [0.0, 0.0, 1.0, 1.0] {
                    let ci = egui::ColorImage::from_rgba_unmultiplied([image.w as usize, image.h as usize], &image.data);
                    self.set_fit(ctx, ci);
                    self.fit_gen = self.fit_gen.max(*generation);
                }
                self.hist = Some(hist.clone());
                self.last_ms = *ms;
                self.last_image = Some(Arc::new(image.clone()));
            }
            _ => {}
        }
    }

    fn set_fit(&mut self, ctx: &egui::Context, ci: egui::ColorImage) {
        let opts = TextureOptions::LINEAR;
        match &mut self.fit_tex {
            Some(t) if t.size() == ci.size => t.set(ci, opts),
            _ => self.fit_tex = Some(ctx.load_texture(format!("fit{}", self.slot), ci, opts)),
        }
    }

    pub fn fit_zoom(&self, canvas: Rect, cw: usize, ch: usize, ppp: f32) -> f32 {
        let k = ((canvas.width() - 24.0) / cw as f32).min((canvas.height() - 24.0) / ch as f32).max(0.01);
        k * ppp
    }

    /// Toggle fit <-> 1:1, anchored at the pointer.
    pub fn toggle_zoom(&mut self, at: Option<Pos2>) {
        if self.zoom.is_some() {
            self.zoom = None;
            self.center = [0.5, 0.5];
        } else {
            self.zoom = Some(1.0);
            if let (Some(p), Some(v)) = (at, &self.last_view) {
                let c = v.screen_to_cropnorm(p);
                self.center = [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0)];
            }
        }
    }

    pub fn set_zoom(&mut self, z: Option<f32>) {
        self.zoom = z;
        if z.is_none() {
            self.center = [0.5, 0.5];
        }
    }

    pub fn zoom_label(&self, ppp: f32) -> String {
        match (self.zoom, &self.last_view) {
            (None, Some(v)) => {
                let (cw, _) = cropped_dims(v.bw, v.bh, &v.settings_geo);
                trf!("맞춤 {:.0}%", "Fit {:.0}%", v.image_rect.width() * ppp / cw as f32 * 100.0)
            }
            (None, None) => tr!("맞춤", "Fit").into(),
            (Some(z), _) => format!("{:.0}%", z * 100.0),
        }
    }

    /// Draw the viewer and request renders. When `pan` is true, dragging moves the view.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        dev: &DevService,
        settings: &DevelopSettings,
        draft: bool,
        clipping: bool,
        overlay: Option<usize>,
        pan: bool,
    ) -> (egui::Response, Option<ViewInfo>) {
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, CANVAS());
        let ppp = ui.ctx().pixels_per_point();

        let Some(src) = self.source.clone() else {
            // Placeholder until the source image is loaded
            if let Some(ph) = &self.placeholder {
                let s = ph.size_vec2();
                let k = ((rect.width() - 24.0) / s.x).min((rect.height() - 24.0) / s.y);
                let r = Rect::from_center_size(rect.center(), s * k);
                painter.image(ph.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            let msg = match &self.error {
                Some(e) => trf!("열 수 없음: {e}", "Can't open: {e}"),
                None => tr!("불러오는 중…", "Loading…").into(),
            };
            painter.text(rect.left_top() + vec2(10.0, 10.0), Align2::LEFT_TOP, msg, FontId::monospace(11.0), TEXT_WEAK());
            return (resp, None);
        };

        let (bw, bh) = (src.width(), src.height());
        let (cw, ch) = cropped_dims(bw, bh, &settings.geometry);
        let fit = self.fit_zoom(rect, cw, ch, ppp);

        // Wheel zoom around the pointer
        if resp.hovered() && !self.lock_zoom {
            let (scroll, zd) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = if zd != 1.0 { zd } else if scroll != 0.0 { 1.0015f32.powf(scroll) } else { 1.0 };
            if factor != 1.0 {
                let cur = self.zoom.unwrap_or(fit);
                let nz = (cur * factor).clamp(fit.min(0.05), 16.0);
                if let (Some(p), Some(v)) = (resp.hover_pos(), &self.last_view) {
                    let pc = v.screen_to_cropnorm(p);
                    let new_size = vec2(cw as f32 * nz / ppp, ch as f32 * nz / ppp);
                    let off = p - rect.center();
                    self.center = [pc[0] - off.x / new_size.x, pc[1] - off.y / new_size.y];
                }
                self.zoom = if nz <= fit * 1.001 { None } else { Some(nz) };
            }
        }
        let z = self.zoom.unwrap_or(fit);
        let size = vec2(cw as f32 * z / ppp, ch as f32 * z / ppp);
        if self.zoom.is_none() {
            self.center = [0.5, 0.5];
        }
        // Pan
        if pan && self.zoom.is_some() && resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.center[0] -= d.x / size.x;
            self.center[1] -= d.y / size.y;
        }
        if resp.dragged_by(egui::PointerButton::Middle) && self.zoom.is_some() {
            let d = resp.drag_delta();
            self.center[0] -= d.x / size.x;
            self.center[1] -= d.y / size.y;
        }
        // Clamp the center so the image stays on screen
        for (k, (s, r)) in [(size.x, rect.width()), (size.y, rect.height())].into_iter().enumerate() {
            if s <= r {
                self.center[k] = 0.5;
            } else {
                let half = r / s * 0.5;
                self.center[k] = self.center[k].clamp(half, 1.0 - half);
            }
        }
        let image_rect = Rect::from_center_size(
            rect.center() + vec2((0.5 - self.center[0]) * size.x, (0.5 - self.center[1]) * size.y),
            size,
        );
        let info = ViewInfo {
            canvas: rect,
            image_rect,
            bw,
            bh,
            settings_geo: settings.geometry.clone(),
            lens: settings.lens.clone(),
            ppp,
        };

        // Render request
        let vis = image_rect.intersect(rect);
        if vis.width() >= 1.0 && vis.height() >= 1.0 {
            let c0 = info.screen_to_cropnorm(vis.min);
            let c1 = info.screen_to_cropnorm(vis.max);
            let region = [c0[0].max(0.0), c0[1].max(0.0), c1[0].min(1.0), c1[1].min(1.0)];
            let w = (vis.width() * ppp).round() as usize;
            let h = (vis.height() * ppp).round() as usize;
            let q = |v: f32| (v * 100000.0).round() as i32;
            let key = ReqKey {
                settings: settings.clone(),
                w,
                h,
                region: [q(region[0]), q(region[1]), q(region[2]), q(region[3])],
                draft,
                clipping,
                overlay,
                lens_gen: crate::develop::lensfun::generation(),
            };
            if self.last_key.as_ref() != Some(&key) {
                self.generation += 1;
                dev.render(RenderJob {
                    id: self.id,
                    path: self.path.clone(),
                    settings: settings.clone(),
                    w,
                    h,
                    region,
                    draft,
                    clipping,
                    overlay,
                    generation: self.generation,
                    slot: self.slot,
                    underlay: false,
                });
                self.last_key = Some(key);
            }
        }
        // While zoomed, keep the full-image underlay up to date so unrendered areas are not blank when panning
        if self.zoom.is_some() {
            let fz = fit;
            let w = ((cw as f32 * fz).round() as usize).max(1);
            let h = ((ch as f32 * fz).round() as usize).max(1);
            let key = ReqKey { settings: settings.clone(), w, h, region: [0, 0, 100000, 100000], draft: true, clipping, overlay, lens_gen: crate::develop::lensfun::generation() };
            if self.last_fit_key.as_ref() != Some(&key) {
                self.generation += 1;
                dev.render(RenderJob {
                    id: self.id,
                    path: self.path.clone(),
                    settings: settings.clone(),
                    w,
                    h,
                    region: [0.0, 0.0, 1.0, 1.0],
                    draft: true,
                    clipping,
                    overlay,
                    generation: self.generation,
                    slot: self.slot,
                    underlay: true,
                });
                self.last_fit_key = Some(key);
            }
        }

        // Display: when zoomed, draw the fit texture underneath and the region texture on top
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        if self.zoom.is_some()
            && let Some(ft) = &self.fit_tex {
                painter.image(ft.id(), image_rect, uv, Color32::WHITE);
            }
        if let Some(t) = &self.tex {
            let r = Rect::from_min_max(
                info.cropnorm_to_screen([self.tex_region[0], self.tex_region[1]]),
                info.cropnorm_to_screen([self.tex_region[2], self.tex_region[3]]),
            );
            painter.image(t.id(), r, uv, Color32::WHITE);
        } else if let Some(ph) = &self.placeholder {
            painter.image(ph.id(), image_rect, uv, Color32::WHITE);
        }
        self.last_view = Some(info.clone());
        (resp, Some(info))
    }

    /// Repaint the last drawn image shifted by `offset` (background for the enhance dialog's result side)
    pub fn paint_copy(&self, painter: &egui::Painter, offset: Vec2) {
        let Some(info) = &self.last_view else { return };
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        if self.zoom.is_some()
            && let Some(ft) = &self.fit_tex {
                painter.image(ft.id(), info.image_rect.translate(offset), uv, Color32::WHITE);
            }
        if let Some(t) = &self.tex {
            let r = Rect::from_min_max(
                info.cropnorm_to_screen([self.tex_region[0], self.tex_region[1]]),
                info.cropnorm_to_screen([self.tex_region[2], self.tex_region[3]]),
            );
            painter.image(t.id(), r.translate(offset), uv, Color32::WHITE);
        }
    }

    /// Most recent render at fit size (filmstrip live preview).
    pub fn live_tex(&self) -> Option<&TextureHandle> {
        self.fit_tex.as_ref()
    }

    /// Whether a full-image underlay exists while zoomed (used by self-tests)
    pub fn has_underlay(&self) -> bool {
        self.fit_tex.is_some()
    }

    pub fn last_view(&self) -> Option<&ViewInfo> {
        self.last_view.as_ref()
    }

    /// Color at a screen position from the rendered output (8-bit sRGB).
    pub fn sample_output(&self, p: Pos2) -> Option<[u8; 3]> {
        let img = self.last_image.as_ref()?;
        let v = self.last_view.as_ref()?;
        let c = v.screen_to_cropnorm(p);
        let r = self.tex_region;
        let u = (c[0] - r[0]) / (r[2] - r[0]);
        let w = (c[1] - r[1]) / (r[3] - r[1]);
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&w) {
            return None;
        }
        let x = (u * img.w as f32) as usize;
        let y = (w * img.h as f32) as usize;
        let i = (y * img.w as usize + x) * 4;
        Some([img.data[i], img.data[i + 1], img.data[i + 2]])
    }

    /// Averaged source (linear) color sample, for the white-balance eyedropper.
    pub fn sample_source(&self, base_norm: [f32; 2]) -> Option<[f32; 3]> {
        let src = self.source.as_ref()?;
        let lvl = src.levels.last()?;
        let x = (base_norm[0] * lvl.w as f32) as i64;
        let y = (base_norm[1] * lvl.h as f32) as i64;
        let mut acc = [0.0f32; 3];
        let mut n = 0.0;
        for dy in -2..=2 {
            for dx in -2..=2 {
                let (xx, yy) = (x + dx, y + dy);
                if xx >= 0 && yy >= 0 && (xx as usize) < lvl.w && (yy as usize) < lvl.h {
                    let p = lvl.px(xx as usize, yy as usize);
                    for c in 0..3 {
                        acc[c] += p[c];
                    }
                    n += 1.0;
                }
            }
        }
        if n == 0.0 {
            return None;
        }
        Some([acc[0] / n, acc[1] / n, acc[2] / n])
    }
}

pub fn fit_rect(container: Rect, size: Vec2) -> Rect {
    let k = (container.width() / size.x).min(container.height() / size.y);
    Rect::from_center_size(container.center(), size * k)
}
