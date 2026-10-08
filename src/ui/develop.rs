//! Develop module: edit panels, crop/mask/white-balance tools, history, snapshots, presets and sync.

use super::app::*;
use super::theme::*;
use super::viewer::{ViewInfo, Viewer};
use super::widgets::{self, Edit, section, slider};
use super::workers::DevEvent;
use crate::catalog::{HistoryEntry, PhotoId, Snapshot};
use crate::config;
use crate::develop::geometry::constrain_crop;
use crate::develop::settings::*;
use egui::{Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    None,
    Crop,
    Mask,
    WbPicker,
    Privacy,
    Spot,
    RedEye,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BeforeAfter {
    Off,
    Before,
    SideBySide,
    /// Single view split left/right: left = before, right = after (drag the divider to move it)
    Split,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurveMode {
    Parametric,
    Rgb,
    Red,
    Green,
    Blue,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HslTab {
    Hue,
    Sat,
    Lum,
    All,
}

#[derive(Clone, Copy, Debug)]
pub struct BrushOpts {
    pub size: f32,
    pub feather: f32,
    pub flow: f32,
    pub erase: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    /// Crop: Ctrl+drag draws a horizontal (or vertical) reference line (start point)
    Straighten(Pos2),
    None,
    CropMove,
    CropEdge(i8, i8),
    CropRotate { start_angle: f32, start_ptr: f32 },
    LinP0,
    LinP1,
    LinMove,
    RadCenter,
    RadX,
    RadY,
    RadRotate,
    Painting,
    NewLinear([f32; 2]),
    NewRadial([f32; 2]),
    /// Dragging empty space while a tool is active pans the view
    Pan,
}

/// Target picked by right-clicking the canvas
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CtxTarget {
    None,
    Spot(usize),
    Privacy(usize),
    Mask(usize),
    RedEye(usize),
}

pub struct DevState {
    pub id: PhotoId,
    pub is_raw: bool,
    pub settings: DevelopSettings,
    pub viewer: Viewer,
    pub before: Option<Viewer>,
    /// Detail panel 1:1 preview (sharpening and noise reduction are not visible at fit size)
    pub detail_view: Option<Viewer>,
    /// Split before/after divider position (0..1)
    pub split_pos: f32,
    /// Click the view to choose the 1:1 preview location
    pub detail_pick: bool,
    pub opened_settings: DevelopSettings,
    pub history: Vec<HistoryEntry>,
    pub hist_pos: usize,
    last_label: String,
    last_commit: Instant,
    pub snapshots: Vec<Snapshot>,
    pub tool: Tool,
    pub before_after: BeforeAfter,
    pub clipping: bool,
    pub mask_sel: Option<usize>,
    pub comp_sel: Option<usize>,
    pub overlay: bool,
    pub brush: BrushOpts,
    pub curve_mode: CurveMode,
    pub hsl_tab: HslTab,
    pub active: bool,
    drag: Drag,
    crop_aspect_label: String,
    /// User-chosen crop box (before rotation); rotation and aspect changes shrink from this only as much as needed
    crop_intent: Option<[f32; 4]>,
    pub dirty: bool,
    pending_label: Option<String>,
    next_mask_id: u64,
    pub hover_preset: Option<DevelopSettings>,
    pub panel_tab: PanelTab,
    /// Temporarily disabled sections (reset to defaults only in the display render)
    pub bypass: std::collections::HashSet<&'static str>,
    pub hsl_band: usize,
    /// Point color: selected entry, and whether a color is being picked from the photo
    pub pc_sel: usize,
    pub pc_pick: bool,
    /// AI masks: kind clicked in the panel (the app starts the job) and progress display
    pub ai_request: Option<crate::imaging::aimask::AiKind>,
    pub ai_busy: Option<crate::imaging::aimask::AiKind>,
    pub ai_status: Option<String>,
    /// Already tried recomputing pasted AI masks (so a failure is not retried every frame)
    pub ai_refresh_tried: std::collections::HashSet<String>,
    /// Remove-fill in progress indicator
    pub fill_status: Option<String>,
    /// Remove-fill already attempted for this shape (so a failure is not retried)
    pub fill_tried: std::collections::HashSet<u64>,
    /// Color grading view: 0 = three-way, 1..4 = shadows/midtones/highlights/global
    pub grade_sel: u8,
    pub privacy_sel: Option<usize>,
    /// 0 none, 1 move, 2 resize, 3 redraw
    pub redeye_sel: Option<usize>,
    pub privacy_drag: u8,
    pub privacy_anchor: [f32; 2],
    pub privacy_msg: String,
    /// HSL targeted adjustment on the photo: 0 hue, 1 saturation, 2 luminance
    pub tat: Option<u8>,
    tat_w: [f32; 8],
    tat_band: String,
    pub spot_sel: Option<usize>,
    /// Defaults for new spots
    pub spot_new: Spot,
    /// 0 none, 1 move target, 2 move source
    spot_drag: u8,
    /// Hide spot overlay (H), path being painted, and request to re-pick the source
    pub spot_hide: bool,
    pub spot_paint: Vec<[f32; 2]>,
    pub spot_refind: bool,
    /// Canvas right-click target (kept while the menu is open)
    ctx_target: CtxTarget,
    /// Cached spot overlay texture (union outline)
    spot_overlay: Option<(u64, egui::TextureHandle)>,
    /// Newly created linear/radial mask: the first drag draws its shape (after that, dragging empty space pans)
    pub mask_fresh: bool,
    /// Draw-to-create: choosing linear/radial waits, then dragging on the photo creates it
    /// (shape: 1 = linear, 2 = radial; op: None = new mask, Some = add a component to the selected mask)
    pub mask_arm: Option<(u8, Option<MaskOp>)>,
    mask_arm_start: Option<[f32; 2]>,
    /// Choosing add/subtract/intersect for a mask (shape picker expanded)
    mask_add_op: Option<MaskOp>,
}

impl DevState {
    pub fn on_event(&mut self, ev: &DevEvent) {
        if let DevEvent::Failed { id, error } = ev
            && *id == self.id {
                let _ = error;
            }
    }

}

// Open/close/history

impl App {
    pub fn open_develop(&mut self, id: PhotoId) {
        if self.devst.as_ref().map(|d| d.id == id).unwrap_or(false) {
            return;
        }
        // Finish the previous photo (tool and view state are kept)
        let keep = self.devst.as_ref().map(|d| (d.tool, d.clipping, d.curve_mode, d.hsl_tab, d.brush, d.before_after, d.viewer.zoom));
        let keep_ui = self.devst.as_ref().map(|d| (d.panel_tab, d.hsl_band, d.grade_sel));
        self.leave_develop();
        let Some(p) = self.cat.get(id).cloned() else { return };
        let settings = p.settings();
        let mut viewer = Viewer::new(id, p.path.clone(), p.meta.orientation, 0, &self.dev);
        viewer.placeholder = self.request_preview(id);
        let mut history = self.cat.history(id);
        if history.is_empty() {
            let label = if p.has_edits() { tr!("가져오기 시 설정", "Settings on import") } else { tr!("원본", "Original") };
            if let Ok(hid) = self.cat.push_history(id, label, &settings) {
                history.push(HistoryEntry { id: hid, label: label.into(), settings: settings.clone(), ts: crate::catalog::now() });
            }
        }
        // If current settings differ from the last history entry (changed in the library), add an entry
        if history.last().map(|h| h.settings != settings).unwrap_or(false)
            && let Ok(hid) = self.cat.push_history(id, tr!("라이브러리에서 변경", "Changed in Library"), &settings) {
                history.push(HistoryEntry { id: hid, label: tr!("라이브러리에서 변경", "Changed in Library").into(), settings: settings.clone(), ts: crate::catalog::now() });
            }
        let hist_pos = history.len().saturating_sub(1);
        let next_mask_id = settings.masks.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let mut st = DevState {
            id,
            is_raw: p.is_raw,
            opened_settings: settings.clone(),
            settings,
            viewer,
            before: None,
            detail_view: None,
            split_pos: 0.5,
            detail_pick: false,
            history,
            hist_pos,
            last_label: String::new(),
            last_commit: Instant::now(),
            snapshots: self.cat.snapshots(id),
            tool: Tool::None,
            before_after: BeforeAfter::Off,
            clipping: false,
            mask_sel: None,
            comp_sel: None,
            overlay: false,
            brush: BrushOpts { size: 0.04, feather: 0.6, flow: 1.0, erase: false },
            curve_mode: CurveMode::Parametric,
            hsl_tab: HslTab::All,
            active: false,
            drag: Drag::None,
            crop_aspect_label: tr!("원본", "Original").into(),
            crop_intent: None,
            dirty: false,
            pending_label: None,
            next_mask_id,
            hover_preset: None,
            panel_tab: PanelTab::Light,
            bypass: Default::default(),
            hsl_band: 0,
            pc_sel: 0,
            pc_pick: false,
            ai_request: None,
            ai_busy: None,
            ai_status: None,
            ai_refresh_tried: Default::default(),
            fill_status: None,
            fill_tried: Default::default(),
            grade_sel: 0,
            privacy_sel: None,
            redeye_sel: None,
            privacy_drag: 0,
            privacy_anchor: [0.0, 0.0],
            privacy_msg: String::new(),
            tat: None,
            tat_w: [0.0; 8],
            tat_band: String::new(),
            spot_sel: None,
            spot_new: Spot { radius: 0.012, feather: 0.25, ..Default::default() },
            spot_drag: 0,
            spot_hide: false,
            spot_paint: Vec::new(),
            spot_refind: false,
            spot_overlay: None,
            ctx_target: CtxTarget::None,
            mask_fresh: false,
            mask_arm: None,
            mask_arm_start: None,
            mask_add_op: None,
        };
        if let Some((tool, clip, cm, ht, brush, ba, zoom)) = keep {
            st.tool = if tool == Tool::Crop || tool == Tool::Mask { tool } else { Tool::None };
            st.clipping = clip;
            st.curve_mode = cm;
            st.hsl_tab = ht;
            st.brush = brush;
            st.before_after = ba;
            if zoom.is_some() {
                st.viewer.zoom = zoom;
            }
        }
        if let Some((t, hb, gs)) = keep_ui {
            st.panel_tab = t;
            st.hsl_band = hb;
            st.grade_sel = gs;
        }
        // Restore this photo's previous zoom/position (a photo seen for the first time keeps the last zoom)
        if let Some((z, c)) = self.view_mem.get(&id) {
            st.viewer.zoom = *z;
            st.viewer.center = *c;
        }
        self.devst = Some(st);
    }

    /// Save the current photo and request a thumbnail update.
    pub fn leave_develop(&mut self) {
        let Some(d) = self.devst.take() else { return };
        if d.settings != d.opened_settings || d.dirty {
            let _ = self.cat.save_develop_quiet(d.id, &d.settings);
            self.cat.bump_thumb(d.id);
            if let Some(p) = self.cat.get(d.id) {
                self.dev.make_previews(d.id, p.thumb_ver, p.path.clone(), d.settings.clone());
            }
        }
    }

    pub fn commit_develop_now(&mut self) {
        if let Some(d) = &mut self.devst {
            let _ = self.cat.save_develop_quiet(d.id, &d.settings);
        }
    }

    /// Commit a settings change to history (consecutive changes to the same control are merged).
    pub fn commit(&mut self, label: &str) {
        let Some(d) = &mut self.devst else { return };
        if d.history.get(d.hist_pos).map(|h| h.settings == d.settings).unwrap_or(false) {
            return;
        }
        let before = d.history.get(d.hist_pos).map(|h| h.settings.clone());
        // A new edit after undo discards the later history
        if d.hist_pos + 1 < d.history.len() {
            let hid = d.history[d.hist_pos].id;
            let _ = self.cat.truncate_history_after(d.id, hid);
            d.history.truncate(d.hist_pos + 1);
        }
        let coalesce = d.last_label == label && d.last_commit.elapsed().as_millis() < config::HISTORY_COALESCE_MS && d.history.len() > 1;
        if coalesce {
            if let Some(last) = d.history.last_mut() {
                last.settings = d.settings.clone();
                let _ = self.cat.replace_last_history(last.id, label, &d.settings);
            }
        } else if let Ok(hid) = self.cat.push_history(d.id, label, &d.settings) {
            d.history.push(HistoryEntry { id: hid, label: label.to_string(), settings: d.settings.clone(), ts: crate::catalog::now() });
        }
        d.hist_pos = d.history.len() - 1;
        d.last_label = label.to_string();
        d.last_commit = Instant::now();
        d.dirty = true;
        let _ = self.cat.save_develop_quiet(d.id, &d.settings);
        if self.prefs.auto_sync
            && let Some(before) = before {
                self.auto_sync_apply(&before, label);
            }
    }

    /// Auto sync: apply only the just-changed items to the other selected photos.
    fn auto_sync_apply(&mut self, before: &DevelopSettings, label: &str) {
        let Some(d) = &self.devst else { return };
        let (cur, now) = (d.id, d.settings.clone());
        let others: Vec<PhotoId> = self.selected.iter().copied().filter(|x| *x != cur).collect();
        for id in others {
            let Some(p) = self.cat.get(id) else { continue };
            let mut s = p.settings();
            let old = s.clone();
            s.apply_changes(before, &now);
            if s != old {
                let _ = self.cat.set_develop(id, &s);
                let _ = self.cat.push_history(id, &trf!("자동 동기화: {label}", "Auto Sync: {label}"), &s);
            }
        }
    }

    pub fn undo(&mut self) {
        let Some(d) = &mut self.devst else { return };
        if d.hist_pos > 0 {
            d.hist_pos -= 1;
            d.settings = d.history[d.hist_pos].settings.clone();
            d.last_label.clear();
            d.dirty = true;
            let _ = self.cat.save_develop_quiet(d.id, &d.settings);
        }
    }

    pub fn redo(&mut self) {
        let Some(d) = &mut self.devst else { return };
        if d.hist_pos + 1 < d.history.len() {
            d.hist_pos += 1;
            d.settings = d.history[d.hist_pos].settings.clone();
            d.last_label.clear();
            d.dirty = true;
            let _ = self.cat.save_develop_quiet(d.id, &d.settings);
        }
    }

    pub fn paste_settings(&mut self) {
        let Some((src, groups)) = self.clipboard.clone() else {
            self.toast(tr!("복사한 설정이 없습니다", "No copied settings"));
            return;
        };
        if self.module == Module::Develop {
            if let Some(d) = &mut self.devst {
                d.settings.copy_groups_from(&src, &groups);
            }
            self.commit(tr!("설정 붙여넣기", "Paste settings"));
            return;
        }
        let t = self.targets();
        for id in &t {
            if let Some(p) = self.cat.get(*id) {
                let mut s = p.settings();
                s.copy_groups_from(&src, &groups);
                let _ = self.cat.set_develop(*id, &s);
                let _ = self.cat.push_history(*id, tr!("설정 붙여넣기", "Paste settings"), &s);
            }
        }
        self.toast(trf!("{}장에 설정 붙여넣기", "Paste settings to {} photos", t.len()));
        self.visible_dirty = self.filter.edited.is_some();
    }

    /// Apply all settings from the previous photo (the one just before in the filmstrip).
    pub fn paste_previous(&mut self) {
        let Some(c) = self.current else { return };
        let Some(i) = self.visible.iter().position(|x| *x == c) else { return };
        if i == 0 {
            return;
        }
        let Some(prev) = self.cat.get(self.visible[i - 1]).map(|p| p.settings()) else { return };
        if let Some(d) = &mut self.devst {
            d.settings.copy_groups_from(&prev, &SettingGroups::all());
            self.commit(tr!("이전 설정 적용", "Apply previous settings"));
        } else {
            let mut s = self.cat.get(c).map(|p| p.settings()).unwrap_or_default();
            s.copy_groups_from(&prev, &SettingGroups::all());
            let _ = self.cat.set_develop(c, &s);
        }
    }

    pub fn copy_settings(&mut self, groups: SettingGroups) {
        let s = match &self.devst {
            Some(d) => d.settings.clone(),
            None => self.current.and_then(|c| self.cat.get(c)).map(|p| p.settings()).unwrap_or_default(),
        };
        self.clipboard = Some((s, groups));
        self.toast(tr!("현상 설정을 복사했습니다", "Develop settings copied"));
    }

    /// Sync current settings to the other selected photos.
    pub fn sync_settings(&mut self, groups: SettingGroups) {
        let Some(d) = &self.devst else { return };
        let src = d.settings.clone();
        let cur = d.id;
        let others: Vec<PhotoId> = self.selected.iter().copied().filter(|x| *x != cur).collect();
        for id in &others {
            if let Some(p) = self.cat.get(*id) {
                let mut s = p.settings();
                s.copy_groups_from(&src, &groups);
                let _ = self.cat.set_develop(*id, &s);
                let _ = self.cat.push_history(*id, tr!("동기화", "Sync"), &s);
            }
        }
        self.toast(trf!("{}장 동기화", "Sync {} photos", others.len()));
    }
}

/// Histogram-based auto tone (algorithmic, not AI).
pub fn auto_tone(hist: &crate::develop::pipeline::Histogram, s: &mut DevelopSettings) {
    let total: u32 = hist.l.iter().sum();
    if total == 0 {
        return;
    }
    let pct = |p: f32| -> f32 {
        let target = (total as f32 * p) as u32;
        let mut acc = 0;
        for (i, c) in hist.l.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i as f32 / 255.0;
            }
        }
        1.0
    };
    let median = pct(0.5).max(0.02);
    let lo = pct(0.005);
    let hi = pct(0.995);
    // Bring midtones near 0.46: approximate the gamma-space ratio as EV
    let target = 0.46;
    let ev = ((target / median).powf(2.2)).log2().clamp(-2.5, 2.5) * 0.7;
    s.exposure = ((s.exposure + ev) * 100.0).round() / 100.0;
    s.exposure = s.exposure.clamp(-5.0, 5.0);
    s.highlights = if hi > 0.97 { -((hi - 0.9) * 600.0).clamp(0.0, 60.0) } else { 0.0 }.round();
    s.shadows = if lo < 0.05 { ((0.08 - lo) * 500.0).clamp(0.0, 45.0) } else { 0.0 }.round();
    s.whites = ((0.98 - hi) * 150.0).clamp(-30.0, 40.0).round();
    s.blacks = ((0.02 - lo) * -150.0).clamp(-30.0, 20.0).round();
    let spread = hi - lo;
    s.contrast = ((0.85 - spread) * 60.0).clamp(-20.0, 30.0).round();
    s.vibrance = s.vibrance.max(10.0);
}

/// For the library: renders a downscaled image with the develop engine so it works without a cached preview.
pub fn auto_tone_photo(app: &mut App, id: PhotoId) {
    let Some(p) = app.cat.get(id).cloned() else { return };
    let Ok(src) = crate::imaging::decode::decode_source(&p.path, p.meta.orientation) else { return };
    let mut s = p.settings();
    let mut e = crate::develop::pipeline::Engine::default();
    let out = e.render(
        &src,
        &crate::develop::pipeline::RenderRequest {
            settings: &s,
            out_w: 512,
            out_h: 512 * src.height() / src.width().max(1),
            region: [0.0, 0.0, 1.0, 1.0],
            draft: true,
            clipping: false,
            mask_overlay: None,
            keep_float: false,
        },
    );
    auto_tone(&out.hist, &mut s);
    let _ = app.cat.set_develop(id, &s);
    let _ = app.cat.push_history(id, tr!("자동 톤", "Auto Tone"), &s);
}

/// Match total exposure: compute the light received from shot info (shutter, aperture, ISO) and set each photo's
/// exposure to match the brightness of the current (reference) photo. For bracketed shots or sequences under changing light
pub fn match_exposures(app: &mut App) {
    let ids = app.multi_targets();
    let Some(r) = app.current.filter(|c| ids.contains(c)) else {
        app.toast(tr!("기준이 될 사진을 포함해 2장 이상 선택하세요", "Select 2 or more photos, including the reference photo"));
        return;
    };
    if ids.len() < 2 {
        app.toast(tr!("2장 이상 선택하세요", "Select 2 or more photos"));
        return;
    }
    // Light received (log2): shutter * ISO / aperture^2
    let light = |m: &crate::imaging::meta::PhotoMeta| -> Option<f32> {
        let (t, n, iso) = (m.exposure?, m.fnumber?, m.iso? as f32);
        (t > 0.0 && n > 0.0 && iso > 0.0).then(|| t.log2() + (iso / 100.0).log2() - 2.0 * n.log2())
    };
    let Some(ref_l) = app.cat.get(r).and_then(|p| light(&p.meta)) else {
        app.toast(tr!("기준 사진에 셔터·조리개·ISO 정보가 없습니다", "The reference photo has no shutter/aperture/ISO info"));
        return;
    };
    let ref_exp = match &app.devst {
        Some(d) if d.id == r => d.settings.exposure,
        _ => app.cat.get(r).map(|p| p.settings().exposure).unwrap_or(0.0),
    };
    let (mut done, mut skipped) = (0, 0);
    let mut open_changed = false;
    for id in ids {
        if id == r {
            continue;
        }
        let Some(l) = app.cat.get(id).and_then(|p| light(&p.meta)) else {
            skipped += 1;
            continue;
        };
        let ev = ((ref_exp + (ref_l - l)) * 100.0).round() / 100.0;
        let ev = ev.clamp(-5.0, 5.0);
        if let Some(d) = app.devst.as_mut().filter(|d| d.id == id) {
            d.settings.exposure = ev;
            open_changed = true;
        } else if let Some(p) = app.cat.get(id).cloned() {
            let mut s = p.settings();
            s.exposure = ev;
            let _ = app.cat.set_develop(id, &s);
            let _ = app.cat.push_history(id, tr!("총 노출 맞추기", "Match Total Exposures"), &s);
            app.cat.bump_thumb(id);
            if let Some(p2) = app.cat.get(id) {
                app.dev.make_previews(id, p2.thumb_ver, p2.path.clone(), s.clone());
            }
        }
        done += 1;
    }
    if open_changed {
        app.commit(tr!("총 노출 맞추기", "Match Total Exposures"));
    }
    app.toast(if skipped > 0 { trf!("{done}장의 노출을 맞췄습니다 (촬영 정보 없는 {skipped}장 제외)", "Matched exposure of {done} photos ({skipped} without shooting info skipped)") } else { trf!("{done}장의 노출을 기준 사진에 맞췄습니다", "Matched exposure of {done} photos to the reference photo") });
}

/// Toggle reference view. If no reference photo is set, pin the current photo as the reference.
pub fn toggle_reference(app: &mut App) {
    if app.ref_view {
        app.ref_view = false;
        return;
    }
    if app.module != Module::Develop {
        app.set_module(Module::Develop);
    }
    let cur = app.devst.as_ref().map(|d| d.id).or(app.current);
    if app.dev_ref.and_then(|r| app.cat.get(r)).is_none() {
        app.dev_ref = cur;
    }
    app.ref_view = true;
    if app.dev_ref == cur {
        app.toast(tr!("이 사진을 참조로 고정했습니다 — 필름 스트립에서 편집할 사진을 고르세요", "Pinned this photo as the reference — pick a photo to edit in the filmstrip"));
    }
}

/// Set the reference photo (Photo menu)
pub fn set_reference(app: &mut App, id: PhotoId) {
    app.dev_ref = Some(id);
    app.ref_view = true;
    if app.module != Module::Develop {
        app.set_module(Module::Develop);
    }
    app.toast(tr!("참조 사진으로 고정했습니다 (Shift+R: 참조 보기 끄기/켜기)", "Pinned as the reference photo (Shift+R: toggle reference view)"));
}

/// Reference photo in the left pane (preview with its edits applied)
fn paint_reference(app: &mut App, ui: &mut Ui, r: Rect, rid: PhotoId, same: bool) {
    let p = ui.painter_at(r);
    p.rect_filled(r, 0.0, CANVAS());
    if same {
        p.text(r.center(), Align2::CENTER_CENTER, tr!("필름 스트립에서 편집할 다른 사진을 고르세요\n이 사진은 참조로 왼쪽에 고정됩니다", "Pick another photo to edit in the filmstrip\nThis photo stays pinned on the left as the reference"), FontId::proportional(13.0), TEXT_WEAK());
    } else if let Some(t) = app.request_preview(rid) {
        let sz = t.size_vec2();
        let s = ((r.width() - 24.0) / sz.x).min((r.height() - 24.0) / sz.y);
        let ir = Rect::from_center_size(r.center(), sz * s);
        p.image(t.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        p.text(r.center(), Align2::CENTER_CENTER, tr!("미리보기 준비 중…", "Preparing preview…"), FontId::proportional(12.0), TEXT_WEAK());
    }
    let name = app.cat.get(rid).map(|p| p.display_name()).unwrap_or_default();
    let g = p.layout_no_wrap(trf!("참조 · {name}", "Reference · {name}"), FontId::proportional(11.5), Color32::WHITE);
    let br = Align2::LEFT_TOP.anchor_size(r.left_top() + vec2(10.0, 8.0), g.size() + vec2(12.0, 6.0));
    p.rect_filled(br, 4.0, Color32::from_black_alpha(150));
    p.galley(br.center() - g.size() * 0.5, g, Color32::WHITE);
    // Close button
    let xr = Rect::from_center_size(r.right_top() + vec2(-16.0, 16.0), vec2(20.0, 20.0));
    let xresp = ui.interact(xr, egui::Id::new("ref_close"), Sense::click()).on_hover_text(tr!("참조 보기 끄기 (Shift+R)", "Turn off reference view (Shift+R)"));
    p.circle_filled(xr.center(), 10.0, Color32::from_black_alpha(if xresp.hovered() { 200 } else { 140 }));
    for s in [-1.0f32, 1.0] {
        p.line_segment([xr.center() + vec2(-4.0, -4.0 * s), xr.center() + vec2(4.0, 4.0 * s)], Stroke::new(1.4, Color32::WHITE));
    }
    if xresp.clicked() {
        app.ref_view = false;
    }
}

// Shortcuts

pub fn shortcut(app: &mut App, k: Key, m: egui::Modifiers) -> bool {
    let cmd = m.command;
    match (k, cmd, m.shift) {
        (Key::Z, true, false) => app.undo(),
        (Key::Z, true, true) | (Key::Y, true, _) => app.redo(),
        (Key::R, true, true) => {
            if let Some(d) = &mut app.devst {
                d.settings = DevelopSettings::default_for(d.is_raw);
            }
            app.commit(tr!("모두 초기화", "Reset all"));
        }
        (Key::N, true, false) => {
            if let Some(d) = &app.devst {
                let name = trf!("스냅샷 {}", "Snapshot {}", d.snapshots.len() + 1);
                let (id, s) = (d.id, d.settings.clone());
                let _ = app.cat.add_snapshot(id, &name, &s);
                if let Some(d) = &mut app.devst {
                    d.snapshots = app.cat.snapshots(id);
                }
            }
        }
        (Key::R, false, false) => toggle_tool(app, Tool::Crop),
        (Key::R, false, true) => toggle_reference(app),
        (Key::S, false, false) => super::proof_ui::toggle(app),
        (Key::W, false, false) => toggle_tool(app, Tool::WbPicker),
        (Key::W, false, true) => toggle_tool(app, Tool::Mask),
        (Key::Q, false, false) => toggle_tool(app, Tool::Spot),
        (Key::A, false, true) => {
            let done = match &mut app.devst {
                Some(d) => match d.viewer.hist.clone() {
                    Some(h) => {
                        auto_tone(&h, &mut d.settings);
                        true
                    }
                    None => false,
                },
                None => false,
            };
            if done {
                app.commit(tr!("자동 톤", "Auto Tone"));
            }
        }
        (Key::F, false, true) => toggle_tool(app, Tool::Privacy),
        (Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown, false, _)
            if app.devst.as_ref().map(|d| d.tool == Tool::Spot && d.spot_sel.is_some()).unwrap_or(false) =>
        {
            // Move the selected spot's source by 1 screen point (10 with Shift)
            if let Some(d) = &mut app.devst {
                let step = if m.shift { 10.0 } else { 1.0 };
                let (dx, dy) = match k {
                    Key::ArrowLeft => (-step, 0.0),
                    Key::ArrowRight => (step, 0.0),
                    Key::ArrowUp => (0.0, -step),
                    _ => (0.0, step),
                };
                if let Some(v) = d.viewer.last_view() {
                    let c = v.canvas.center();
                    let (a, b) = (v.screen_to_base(c), v.screen_to_base(c + vec2(dx, dy)));
                    if let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get_mut(i)) {
                        sp.src[0] += b[0] - a[0];
                        sp.src[1] += b[1] - a[1];
                    }
                }
            }
            app.commit(tr!("스팟 원본 이동", "Move spot source"));
        }
        (Key::H, false, false) if app.devst.as_ref().map(|d| d.tool == Tool::Spot).unwrap_or(false) => {
            // Handled by the spot tool (hide overlay)
        }
        (Key::K, false, false) => new_mask(app, MaskShape::Brush { strokes: vec![] }),
        (Key::M, false, false) => new_mask(app, MaskShape::Linear { p0: [0.5, 0.2], p1: [0.5, 0.5] }),
        (Key::M, false, true) => new_mask(app, MaskShape::Radial { center: [0.5, 0.5], radius: [0.25, 0.25], angle: 0.0, feather: 50.0 }),
        (Key::O, false, false) => {
            if let Some(d) = &mut app.devst {
                d.overlay = !d.overlay;
            }
        }
        (Key::J, false, false) => {
            if let Some(d) = &mut app.devst {
                d.clipping = !d.clipping;
            }
        }
        (Key::Backslash, false, false) => {
            if let Some(d) = &mut app.devst {
                d.before_after = if d.before_after == BeforeAfter::Before { BeforeAfter::Off } else { BeforeAfter::Before };
            }
        }
        (Key::Y, false, true) => {
            if let Some(d) = &mut app.devst {
                d.before_after = if d.before_after == BeforeAfter::Split { BeforeAfter::Off } else { BeforeAfter::Split };
            }
        }
        (Key::Y, false, false) => {
            if let Some(d) = &mut app.devst {
                d.before_after = if d.before_after == BeforeAfter::SideBySide { BeforeAfter::Off } else { BeforeAfter::SideBySide };
            }
        }
        (Key::Space, false, _) if app.devst.as_ref().map(|d| d.tool != Tool::None).unwrap_or(false) => {
            // While a tool is active: pan while held (no zoom toggle)
        }
        (Key::Space, false, _) | (Key::Z, false, false) => {
            if let Some(d) = &mut app.devst
                && (d.tool != Tool::Mask || d.drag == Drag::None) {
                    let at = d.viewer.last_view().map(|v| v.canvas.center());
                    let hp = None.or(at);
                    d.viewer.toggle_zoom(hp);
                }
        }
        (Key::OpenBracket, false, _) => {
            if let Some(d) = &mut app.devst {
                d.brush.size = (d.brush.size / 1.15).max(0.002);
            }
        }
        (Key::CloseBracket, false, _) => {
            if let Some(d) = &mut app.devst {
                d.brush.size = (d.brush.size * 1.15).min(0.5);
            }
        }
        (Key::Escape, false, _) => {
            if let Some(d) = &mut app.devst {
                if d.mask_arm.is_some() {
                    d.mask_arm = None;
                    d.mask_arm_start = None;
                } else if d.tool != Tool::None {
                    d.tool = Tool::None;
                } else {
                    return false;
                }
            }
        }
        // Crop: swap aspect orientation (landscape/portrait)
        (Key::X, false, false) if app.devst.as_ref().map(|d| d.tool == Tool::Crop).unwrap_or(false) => {
            if let Some(d) = &mut app.devst {
                let (bw, bh) = d.viewer.source.as_ref().map(|s| (s.width(), s.height())).unwrap_or((3, 2));
                let g = &mut d.settings.geometry;
                if let Some(a) = g.aspect.filter(|a| (*a - 1.0).abs() > 1e-3) {
                    let (fw, fh) = crate::develop::geometry::frame_dims(bw, bh, g);
                    g.aspect = Some(1.0 / a);
                    let want = reshape_crop(d.crop_intent.unwrap_or(g.crop), 1.0 / a, fw as f32, fh as f32);
                    d.crop_intent = Some(want);
                    g.crop = fit_rotated(want, g, bw, bh, &d.settings.lens);
                    app.commit(tr!("자르기 비율 방향", "Crop aspect orientation"));
                }
            }
        }
        (Key::Enter, false, _) => {
            if let Some(d) = &mut app.devst {
                if d.tool == Tool::Crop {
                    d.tool = Tool::None;
                    app.commit(tr!("자르기", "Crop"));
                } else {
                    return false;
                }
            }
        }
        (Key::Delete, false, _) | (Key::Backspace, false, _) => {
            // Mask tool: delete the selected mask
            let mut removed = false;
            let mut label = tr!("마스크 삭제", "Delete mask");
            if let Some(d) = &mut app.devst {
                if d.tool == Tool::Spot {
                    if let Some(i) = d.spot_sel.filter(|i| *i < d.settings.spots.len()) {
                        d.settings.spots.remove(i);
                        d.spot_sel = None;
                        removed = true;
                        label = tr!("스팟 삭제", "Delete spot");
                    } else {
                        return true; // Keep Delete from falling through to photo removal while a tool is active
                    }
                } else if d.tool == Tool::RedEye {
                    if let Some(i) = d.redeye_sel.filter(|i| *i < d.settings.red_eye.len()) {
                        d.settings.red_eye.remove(i);
                        d.redeye_sel = None;
                        removed = true;
                        label = tr!("적목 영역 삭제", "Delete red eye area");
                    } else {
                        return true;
                    }
                } else if d.tool == Tool::Privacy {
                    if let Some(i) = d.privacy_sel.filter(|i| *i < d.settings.privacy.len()) {
                        d.settings.privacy.remove(i);
                        d.privacy_sel = None;
                        removed = true;
                        label = tr!("가리기 삭제", "Delete privacy region");
                    } else {
                        return true;
                    }
                } else if d.tool == Tool::Mask
                    && let Some(i) = d.mask_sel
                        && i < d.settings.masks.len() {
                            d.settings.masks.remove(i);
                            d.mask_sel = None;
                            d.comp_sel = None;
                            removed = true;
                        }
            }
            if removed {
                app.commit(label);
            } else { return app.devst.as_ref().map(|d| d.tool != Tool::None).unwrap_or(false) }
        }
        _ => return false,
    }
    true
}

fn toggle_tool(app: &mut App, t: Tool) {
    let mut commit = false;
    if let Some(d) = &mut app.devst {
        if d.tool == t {
            if t == Tool::Crop {
                commit = true;
            }
            d.tool = Tool::None;
        } else {
            d.tool = t;
        }
    }
    if commit {
        app.commit(tr!("자르기", "Crop"));
    }
}

fn new_mask(app: &mut App, shape: MaskShape) {
    let Some(d) = &mut app.devst else { return };
    if let Some(kind) = drawn_kind(&shape) {
        d.mask_arm = Some((kind, None));
        d.mask_arm_start = None;
        d.tool = Tool::Mask;
        d.overlay = true;
        return;
    }
    let id = d.next_mask_id;
    d.next_mask_id += 1;
    let name = trf!("마스크 {}", "Mask {}", d.settings.masks.len() + 1);
    let is_brush = matches!(shape, MaskShape::Brush { .. });
    d.settings.masks.push(Mask {
        id,
        name,
        components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape }],
        adj: LocalAdjust { exposure: if is_brush { 0.0 } else { 0.0 }, ..Default::default() },
        ..Default::default()
    });
    d.mask_sel = Some(d.settings.masks.len() - 1);
    d.comp_sel = Some(0);
    d.mask_fresh = matches!(d.settings.masks.last().and_then(|m| m.components.first()).map(|c| &c.shape), Some(MaskShape::Linear { .. } | MaskShape::Radial { .. }));
    d.tool = Tool::Mask;
    d.overlay = true;
    app.commit(tr!("마스크 추가", "Add mask"));
}

// View

pub fn show(app: &mut App, ui: &mut Ui) {
    if app.devst.is_none() {
        if let Some(c) = app.current {
            app.open_develop(c);
        } else {
            app.module = Module::Library;
            return;
        }
    }
    app.scroll_to_current = false;
    let mut edit = Edit::default();
    let mut label = String::new();
    // Left, right and bottom cards (drag to rearrange)
    super::devlayout::panels(app, ui, &mut edit, &mut label);
    egui::CentralPanel::no_frame().show(ui, |ui| {
        let rect = ui.available_rect_before_wrap();
        let canvas = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - 30.0));
        let (e, l) = canvas_ui(app, ui, canvas);
        navigator_overlay(app, ui, canvas);
        if e.changed || e.committed {
            edit.merge(e);
            label = l;
        }
        bottom_bar(app, ui, Rect::from_min_max(pos2(rect.min.x, rect.max.y - 30.0), rect.max));
    });
    if let Some(d) = &mut app.devst {
        d.active = edit.active;
        if edit.changed {
            d.pending_label = Some(label.clone());
        }
    }
    if edit.committed {
        let l = app.devst.as_mut().and_then(|d| d.pending_label.take()).unwrap_or(label);
        if !l.is_empty() {
            app.commit(&l);
        }
    }
}

/// Histogram card (drag a region left/right to adjust that tone)
pub(super) fn histogram_card(app: &mut App, ui: &mut Ui) -> (Edit, String) {
    let mut edit = Edit::default();
    let mut label = String::new();
    let Some(d) = app.devst.as_mut() else { return (edit, label) };
    let info = d.viewer.source.as_ref().map(|s| format!("{}×{}  {:.0}ms", s.width(), s.height(), d.viewer.last_ms)).unwrap_or_default();
    if let Some((z, dx, stopped)) = widgets::histogram(ui, d.viewer.hist.as_ref(), &mut d.clipping, &info) {
        let st = &mut d.settings;
        let (v, k, lo, hi): (&mut f32, f32, f32, f32) = match z {
            0 => (&mut st.blacks, 200.0, -100.0, 100.0),
            1 => (&mut st.shadows, 200.0, -100.0, 100.0),
            2 => (&mut st.exposure, 4.0, -5.0, 5.0),
            3 => (&mut st.highlights, 200.0, -100.0, 100.0),
            _ => (&mut st.whites, 200.0, -100.0, 100.0),
        };
        if dx != 0.0 {
            *v = (*v + dx * k).clamp(lo, hi);
            if z != 2 {
                *v = v.round();
            } else {
                *v = (*v * 100.0).round() / 100.0;
            }
            edit.changed = true;
            edit.active = true;
            label = trf!("{} (히스토그램)", "{} (histogram)", crate::i18n::t(widgets::HIST_ZONES[z].0));
        }
        if stopped {
            edit.committed = true;
            label = trf!("{} (히스토그램)", "{} (histogram)", crate::i18n::t(widgets::HIST_ZONES[z].0));
        }
    }
    (edit, label)
}

pub(super) fn tool_strip(app: &mut App, ui: &mut Ui) {
    let Some(d) = &mut app.devst else { return };
    let mut toggle = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        use widgets::ToolIcon as I;
        for (t, icon, tip) in [
            (Tool::Crop, I::Crop, tr!("자르기 · 회전 (R)", "Crop · Rotate (R)")),
            (Tool::Mask, I::Mask, tr!("마스크 (Shift+W) — 브러시 K · 선형 M · 방사형 Shift+M", "Masks (Shift+W) — Brush K · Linear M · Radial Shift+M")),
            (Tool::Spot, I::Heal, tr!("스팟 제거 · 복구 브러시 (Q) — 클릭: 점 · 드래그: 칠하기", "Spot removal · Healing brush (Q) — click: spot · drag: paint")),
            (Tool::RedEye, I::RedEye, tr!("적목 현상 제거 — 눈을 드래그로 감싸거나 클릭", "Red eye removal — drag around the eye or click")),
            (Tool::Privacy, I::Privacy, tr!("가리기 — 얼굴 자동 모자이크 (Shift+F)", "Privacy — automatic face mosaic (Shift+F)")),
            (Tool::WbPicker, I::Picker, tr!("화이트 밸런스 선택 (W)", "White balance selector (W)")),
        ] {
            if widgets::tool_button(ui, icon, d.tool == t, tip).clicked() {
                toggle = Some(t);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if widgets::tool_button(ui, I::Clipping, d.clipping, tr!("클리핑 표시 (J)", "Show clipping (J)")).clicked() {
                d.clipping = !d.clipping;
            }
            if widgets::tool_button(ui, I::BeforeAfter, d.before_after != BeforeAfter::Off, tr!("이전/이후 — 역슬래시: 이전 · Y: 나란히 · Shift+Y: 분할", "Before/After — backslash: before · Y: side by side · Shift+Y: split")).clicked() {
                d.before_after = match d.before_after {
                    BeforeAfter::Off => BeforeAfter::SideBySide,
                    BeforeAfter::SideBySide => BeforeAfter::Split,
                    BeforeAfter::Split => BeforeAfter::Before,
                    BeforeAfter::Before => BeforeAfter::Off,
                };
            }
        });
    });
    if let Some(t) = toggle {
        toggle_tool(app, t);
    }
}

// Left panel

/// Copy/paste/sync settings (pinned to the bottom of the left panel)
pub(super) fn sync_controls(app: &mut App, ui: &mut Ui) {
    match widgets::button_row(ui, &[(tr!("복사…", "Copy…"), true, "Ctrl+Shift+C"), (tr!("붙여넣기", "Paste"), app.clipboard.is_some(), "Ctrl+Shift+V"), (tr!("이전", "Previous"), true, tr!("이전 사진 설정 적용 (Ctrl+Alt+V)", "Apply previous photo's settings (Ctrl+Alt+V)"))]) {
        Some(0) => app.dlg.copy_settings = Some(SettingGroups::all()),
        Some(1) => app.paste_settings(),
        Some(2) => app.paste_previous(),
        _ => {}
    }
    let n_sel = app.selected.len();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let auto = app.prefs.auto_sync;
        let txt = if auto { trf!("자동 동기화 ({n_sel}장)", "Auto Sync ({n_sel} photos)") } else { trf!("동기화… ({n_sel}장)", "Sync… ({n_sel} photos)") };
        let col = if auto && n_sel > 1 { ACCENT } else { TEXT() };
        let btn = egui::Button::new(egui::RichText::new(txt).color(col).size(12.0)).min_size(vec2(ui.available_width() - 40.0, 22.0));
        if ui
            .add_enabled(n_sel > 1 || auto, btn)
            .on_hover_text(tr!("필름스트립에서 Ctrl/Shift+클릭으로 여러 장 선택\n자동 동기화: 값을 바꾸면 선택한 모든 사진에 같은 변경이 적용됩니다", "Select several in the filmstrip with Ctrl/Shift+click\nAuto Sync: every change you make is applied to all selected photos"))
            .clicked()
            && !auto
        {
            app.dlg.sync_settings = Some(SettingGroups::all());
        }
        let mut on = app.prefs.auto_sync;
        if widgets::toggle(ui, &mut on).on_hover_text(tr!("자동 동기화 켜기/끄기", "Toggle Auto Sync")).changed() {
            app.prefs.auto_sync = on;
            app.save_prefs();
        }
    });
}

/// Small navigator at the canvas's lower right while zoomed: click or drag to pan
fn navigator_overlay(app: &mut App, ui: &mut Ui, canvas: Rect) {
    let Some(d) = &mut app.devst else { return };
    if d.viewer.zoom.is_none() {
        return;
    }
    let Some(t) = d.viewer.placeholder.clone() else { return };
    let size = t.size_vec2();
    let k = (180.0 / size.x).min(130.0 / size.y);
    let box_size = size * k;
    let frame = Rect::from_min_size(canvas.right_bottom() - box_size - vec2(14.0, 14.0), box_size);
    ui.painter().rect_filled(frame.expand(4.0), 6.0, Color32::from_black_alpha(170));
    let resp = ui.interact(frame, egui::Id::new("nav_overlay"), Sense::click_and_drag());
    {
        let ir = frame;
        ui.painter().image(t.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // The preview is the cropped result, so it matches crop-normalized coordinates
        if let (Some(_z), Some(v)) = (d.viewer.zoom, d.viewer.last_view()) {
            let c0 = v.screen_to_cropnorm(v.canvas.min);
            let c1 = v.screen_to_cropnorm(v.canvas.max);
            let r = Rect::from_min_max(
                pos2(ir.left() + c0[0].max(0.0) * ir.width(), ir.top() + c0[1].max(0.0) * ir.height()),
                pos2(ir.left() + c1[0].min(1.0) * ir.width(), ir.top() + c1[1].min(1.0) * ir.height()),
            );
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.5, ACCENT), StrokeKind::Middle);
        }
        if (resp.clicked() || resp.dragged_by(egui::PointerButton::Primary))
            && let Some(pp) = resp.interact_pointer_pos() {
                if d.viewer.zoom.is_none() {
                    d.viewer.zoom = Some(1.0);
                }
                d.viewer.center = [((pp.x - ir.left()) / ir.width()).clamp(0.0, 1.0), ((pp.y - ir.top()) / ir.height()).clamp(0.0, 1.0)];
            }
    }
}

pub(super) fn presets(app: &mut App, ui: &mut Ui) {
    let list = app.cat.presets();
    ui.horizontal(|ui| {
        if ui.small_button(tr!("+ 저장…", "+ Save…")).on_hover_text(tr!("현재 설정을 프리셋으로 저장", "Save current settings as a preset")).clicked() {
            app.dlg.save_preset = Some((tr!("사용자 프리셋", "User presets").into(), String::new(), SettingGroups::all()));
        }
        if ui.small_button(tr!("Lightroom에서 가져오기…", "Import from Lightroom…")).clicked() {
            super::dialogs::open_lr_import(app);
        }
    });
    if list.is_empty() {
        ui.label(egui::RichText::new(tr!("저장된 프리셋이 없습니다", "No saved presets")).color(TEXT_DIM()));
        if let Some(d) = &mut app.devst {
            d.hover_preset = None;
        }
        return;
    }
    ui.add(egui::TextEdit::singleline(&mut app.dlg.preset_filter).hint_text(tr!("프리셋 검색", "Search presets")).desired_width(ui.available_width()));
    let filter = app.dlg.preset_filter.to_lowercase();
    let mut groups: Vec<String> = list.iter().map(|p| p.group.clone()).collect();
    groups.dedup();
    let mut apply = None;
    let mut delete = None;
    let mut delete_group = None;
    let mut hover = None;
    for g in groups {
        let items: Vec<&crate::catalog::Preset> = list
            .iter()
            .filter(|p| p.group == g && (filter.is_empty() || p.name.to_lowercase().contains(&filter) || g.to_lowercase().contains(&filter)))
            .collect();
        if items.is_empty() {
            continue;
        }
        let id = ui.make_persistent_id(("preset_group", &g));
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let open = st.is_open() || !filter.is_empty();
        let hr = ui.horizontal(|ui| {
            let r = ui.add(egui::Button::new(egui::RichText::new(format!("{} {}", if open { "−" } else { "+" }, g)).size(12.0)).frame(false));
            ui.label(egui::RichText::new(items.len().to_string()).monospace().size(10.0).color(TEXT_DIM()));
            r
        });
        if hr.inner.clicked() {
            st.toggle(ui);
        }
        hr.inner.context_menu(|ui| {
            if ui.button(tr!("그룹 전체 삭제", "Delete whole group")).clicked() {
                delete_group = Some(g.clone());
                ui.close();
            }
        });
        st.store(ui.ctx());
        if open {
            for p in items {
                let r = ui.selectable_label(false, format!("    {}", p.name));
                if r.hovered() {
                    hover = Some(p.clone());
                }
                if r.clicked() {
                    apply = Some(p.clone());
                }
                r.context_menu(|ui| {
                    if ui.button(tr!("삭제", "Delete")).clicked() {
                        delete = Some(p.id);
                        ui.close();
                    }
                });
            }
        }
    }
    // Hover preview: apply the preset under the mouse to the photo
    if let Some(d) = &mut app.devst {
        d.hover_preset = hover.map(|p| {
            let mut s = d.settings.clone();
            s.copy_groups_from(&p.settings, &p.groups);
            s
        });
    }
    if let Some(p) = apply {
        if let Some(d) = &mut app.devst {
            d.settings.copy_groups_from(&p.settings, &p.groups);
            d.hover_preset = None;
        }
        app.commit(&trf!("프리셋: {}", "Preset: {}", p.name));
    }
    if let Some(id) = delete {
        let _ = app.cat.delete_preset(id);
    }
    if let Some(g) = delete_group {
        let _ = app.cat.delete_preset_group(&g);
    }
}

pub(super) fn snapshots(app: &mut App, ui: &mut Ui) {
    let Some(d) = &mut app.devst else { return };
    if ui.small_button(tr!("+ 스냅샷 (Ctrl+N)", "+ Snapshot (Ctrl+N)")).clicked() {
        let name = trf!("스냅샷 {}", "Snapshot {}", d.snapshots.len() + 1);
        let _ = app.cat.add_snapshot(d.id, &name, &d.settings);
        d.snapshots = app.cat.snapshots(d.id);
    }
    let mut apply = None;
    let mut del = None;
    for s in &d.snapshots {
        let r = widgets::list_row(ui, false, &crate::i18n::label(&s.name), TEXT());
        if r.clicked() {
            apply = Some(s.clone());
        }
        r.context_menu(|ui| {
            if ui.button(tr!("삭제", "Delete")).clicked() {
                del = Some(s.id);
                ui.close();
            }
        });
    }
    if let Some(id) = del {
        let _ = app.cat.delete_snapshot(id);
        d.snapshots = app.cat.snapshots(d.id);
    }
    if let Some(s) = apply {
        d.settings = s.settings.clone();
        app.commit(&trf!("스냅샷: {}", "Snapshot: {}", s.name));
    }
}

pub(super) fn history(app: &mut App, ui: &mut Ui) {
    let Some(d) = &mut app.devst else { return };
    let mut jump = None;
    let mut snap: Option<usize> = None;
    let row = widgets::button_row(ui, &[(tr!("실행 취소", "Undo"), d.hist_pos > 0, "Ctrl+Z"), (tr!("다시 실행", "Redo"), d.hist_pos + 1 < d.history.len(), "Ctrl+Shift+Z"), (tr!("기록 지우기", "Clear history"), d.history.len() > 1, "")]);
    ui.add_space(2.0);
    {
        if row == Some(0) {
            jump = Some(d.hist_pos.saturating_sub(1));
        }
        if row == Some(1) {
            jump = Some((d.hist_pos + 1).min(d.history.len().saturating_sub(1)));
        }
        if row == Some(2) {
            let _ = app.cat.clear_history(d.id);
            if let Ok(hid) = app.cat.push_history(d.id, tr!("현재 상태", "Current state"), &d.settings) {
                d.history = vec![HistoryEntry { id: hid, label: tr!("현재 상태", "Current state").into(), settings: d.settings.clone(), ts: crate::catalog::now() }];
                d.hist_pos = 0;
            }
        }
    }
    egui::ScrollArea::vertical().id_salt("hist_list").auto_shrink([false, false]).show(ui, |ui| {
        for (i, h) in d.history.iter().enumerate().rev() {
            let col = if i > d.hist_pos { TEXT_DIM() } else { TEXT() };
            let r = widgets::list_row(ui, i == d.hist_pos, &crate::i18n::label(&h.label), col);
            if r.clicked() {
                jump = Some(i);
            }
            r.context_menu(|ui| {
                if ui.button(tr!("이 단계로 되돌리기", "Revert to this step")).clicked() {
                    jump = Some(i);
                    ui.close();
                }
                if ui.button(tr!("이 단계를 스냅샷으로", "Make snapshot from this step")).clicked() {
                    snap = Some(i);
                    ui.close();
                }
            });
        }
    });
    if let Some(i) = snap.filter(|i| *i < d.history.len()) {
        let name = d.history[i].label.clone();
        let _ = app.cat.add_snapshot(d.id, &name, &d.history[i].settings);
        d.snapshots = app.cat.snapshots(d.id);
    }
    if let Some(i) = jump
        && i < d.history.len() {
            d.hist_pos = i;
            d.settings = d.history[i].settings.clone();
            d.last_label.clear();
            d.dirty = true;
            let _ = app.cat.save_develop_quiet(d.id, &d.settings);
        }
}

// Right panel

macro_rules! sl {
    ($ui:expr, $e:expr, $lab:expr, $name:expr, $v:expr, $min:expr, $max:expr, $def:expr, $dec:expr) => {{
        let r = slider($ui, $name, $v, $min, $max, $def, $dec, $name);
        if r.changed || r.committed {
            *$lab = format!("{} {:+.*}", $name, $dec, *$v);
        }
        $e.merge(r);
    }};
}

/// Panel tabs (grouped by task instead of one long vertical scroll)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    Light,
    Color,
    Detail,
    Optics,
    Effects,
    All,
}

impl PanelTab {
    const ALL: [(PanelTab, &'static str, &'static str); 6] = [
        (PanelTab::Light, "빛", "기본 · 톤 커브"),
        (PanelTab::Color, "색", "HSL 믹서 · 컬러 그레이딩 · 보정"),
        (PanelTab::Detail, "디테일", "샤프닝 · 노이즈 감소"),
        (PanelTab::Optics, "광학", "렌즈 교정 · 변형"),
        (PanelTab::Effects, "효과", "비네팅 · 그레인"),
        (PanelTab::All, "전체", "모든 섹션 한 번에"),
    ];
    fn shows(self, t: PanelTab) -> bool {
        self == PanelTab::All || self == t
    }
}

/// Panel tab <-> index, for saving workspace state
pub fn panel_tab_index(t: PanelTab) -> u8 {
    PanelTab::ALL.iter().position(|x| x.0 == t).unwrap_or(0) as u8
}

pub fn panel_tab_from(i: u8) -> PanelTab {
    PanelTab::ALL.get(i as usize).map(|x| x.0).unwrap_or(PanelTab::Light)
}

/// Section key -> settings group (for reset and temporary disable)
fn section_groups(key: &str) -> SettingGroups {
    let mut g = SettingGroups {
        white_balance: false,
        basic_tone: false,
        presence: false,
        treatment: false,
        tone_curve: false,
        hsl: false,
        color_grading: false,
        detail: false,
        lens: false,
        crop: false,
        effects: false,
        calibration: false,
        masks: false,
        point_color: false,
    };
    match key {
        "basic" => {
            g.white_balance = true;
            g.basic_tone = true;
            g.presence = true;
        }
        "curve" => g.tone_curve = true,
        "hsl" => g.hsl = true,
        "point" => g.point_color = true,
        "grading" => g.color_grading = true,
        "detail" => g.detail = true,
        "lens" => g.lens = true,
        "effects" => g.effects = true,
        "calib" => g.calibration = true,
        _ => {}
    }
    g
}

/// Display settings with temporarily disabled sections reset to defaults
pub fn apply_bypass(s: &mut DevelopSettings, bypass: &std::collections::HashSet<&'static str>, is_raw: bool) {
    if bypass.is_empty() {
        return;
    }
    let d = DevelopSettings::default_for(is_raw);
    for k in bypass {
        s.copy_groups_from(&d, &section_groups(k));
        if *k == "bw" {
            s.bw_mix = d.bw_mix;
        }
        if *k == "point" {
            s.point_colors.clear();
        }
        if *k == "transform" {
            s.geometry.vertical = 0.0;
            s.geometry.horizontal = 0.0;
            s.geometry.scale = 100.0;
        }
    }
}

fn section_modified(s: &DevelopSettings, key: &str, is_raw: bool) -> bool {
    let d = DevelopSettings::default_for(is_raw);
    match key {
        "bw" => s.bw_mix != d.bw_mix,
        "point" => !s.point_colors.is_empty(),
        "transform" => s.geometry.vertical != 0.0 || s.geometry.horizontal != 0.0 || s.geometry.scale != 100.0 || s.geometry.angle != 0.0,
        _ => {
            let mut t = s.clone();
            t.copy_groups_from(&d, &section_groups(key));
            t != *s
        }
    }
}

pub(super) fn right_panel(app: &mut App, ui: &mut Ui) -> (Edit, String) {
    let mut e = Edit::default();
    let mut lab = String::new();
    let d = app.devst.as_mut().unwrap();
    match d.tool {
        Tool::Crop => {
            crop_panel(d, ui, &mut e, &mut lab);
            return (e, lab);
        }
        Tool::Mask => {
            mask_panel(d, ui, &mut e, &mut lab);
            return (e, lab);
        }
        Tool::Privacy => {
            privacy_panel(d, ui, &mut e, &mut lab);
            return (e, lab);
        }
        Tool::RedEye => {
            redeye_panel(d, ui, &mut e, &mut lab);
            return (e, lab);
        }
        Tool::Spot => {
            spot_panel(d, ui, &mut e, &mut lab);
            return (e, lab);
        }
        _ => {}
    }
    let rc = d.viewer.source.as_ref().and_then(|x| x.raw_color.clone());
    let src_for_auto = d.viewer.source.clone();
    let src_dims = d.viewer.source.as_ref().map(|s| (s.width(), s.height()));
    let lens_info = d.viewer.source.as_ref().and_then(|s| s.lens_info.clone());
    let is_raw = d.is_raw;
    let src_hist = d.viewer.hist.clone();
    // UI state is copied out and restored at the end (avoids borrowing it together with the settings)
    let mut tab = d.panel_tab;
    let mut bypass = d.bypass.clone();
    let mut hsl_sel = d.hsl_band;
    let mut pc_sel = d.pc_sel;
    let mut pc_pick = d.pc_pick;
    let mut grade_sel = d.grade_sel;
    let mut curve_mode = d.curve_mode;
    let mut tat = d.tat;
    // Detail 1:1 preview (taken out temporarily to avoid borrowing it together with the settings)
    let mut dview = d.detail_view.take();
    let mut detail_pick = d.detail_pick;
    if tab.shows(PanelTab::Detail) && dview.is_none() {
        let mut v = Viewer::new(d.id, d.viewer.path.clone(), d.viewer.orientation, 2, &app.dev);
        v.lock_zoom = true;
        v.zoom = Some(1.0);
        dview = Some(v);
    }
    if let Some(v) = &mut dview
        && v.source.is_none() {
            v.source = d.viewer.source.clone();
        }
    let mut detail_settings = d.settings.clone();
    apply_bypass(&mut detail_settings, &d.bypass, d.is_raw);
    let s = &mut d.settings;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let w = (ui.available_width() - 10.0) / 6.0;
        for (t, name, tip) in PanelTab::ALL {
            let sel = tab == t;
            let r = ui.add(
                egui::Button::new(egui::RichText::new(crate::i18n::t(name)).size(12.0).color(if sel { TEXT() } else { TEXT_WEAK() }))
                    .selected(sel)
                    .min_size(vec2(w, 24.0)),
            );
            if r.on_hover_text(crate::i18n::t(tip)).clicked() {
                tab = t;
            }
        }
    });
    ui.add_space(4.0);

    let mut reset_key: Option<&'static str> = None;
    let mut byp = |bypass: &mut std::collections::HashSet<&'static str>, key: &'static str| -> bool { bypass.contains(key) };
    let _ = &mut byp;

    // Light
    if tab.shows(PanelTab::Light) {
        let mut off = bypass.contains("basic");
        let (_, rs) = widgets::section_ex(ui, tr!("기본", "Basic"), true, section_modified(s, "basic", is_raw), Some(&mut off), |ui| {
            // Process and profile
            ui.horizontal(|ui| {
                for (t, n) in [(Treatment::Color, tr!("컬러", "Color")), (Treatment::BlackWhite, tr!("흑백", "B&W"))] {
                    if ui.selectable_label(s.treatment == t, n).clicked() && s.treatment != t {
                        s.treatment = t;
                        e.changed = true;
                        e.committed = true;
                        lab = n.into();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr!("자동 톤", "Auto Tone")).on_hover_text(tr!("히스토그램 기반 자동 노출·대비·화이트·블랙 (Shift+A)", "Auto exposure·contrast·whites·blacks from the histogram (Shift+A)")).clicked()
                        && let Some(h) = &src_hist {
                            auto_tone(h, s);
                            e.changed = true;
                            e.committed = true;
                            lab = tr!("자동 톤", "Auto Tone").into();
                        }
                });
            });
            if let Some(rc) = &rc {
                ui.horizontal(|ui| {
                    // Make the label column match the sliders so the dropdown lines up with the track start
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (lr, _) = ui.allocate_exact_size(vec2(widgets::SLIDER_LABEL_W, 20.0), Sense::hover());
                    ui.painter().text(lr.left_center(), Align2::LEFT_CENTER, tr!("프로파일", "Profile"), FontId::proportional(12.0), TEXT_WEAK());
                    let cur = if s.profile.is_empty() { crate::develop::dcp::DEFAULT_PROFILE.to_string() } else { s.profile.clone() };
                    egui::ComboBox::from_id_salt("profile").truncate().width(ui.available_width() - 8.0).height(520.0).selected_text(crate::develop::dcp::profile_display(&cur)).show_ui(ui, |ui| {
                        for (group, names) in crate::develop::dcp::profile_groups(rc) {
                            ui.label(egui::RichText::new(group).size(10.5).color(TEXT_DIM()));
                            for name in names {
                                if ui.selectable_label(cur == name, format!("   {}", crate::develop::dcp::profile_display(&name))).clicked() && cur != name {
                                    s.profile = name.clone();
                                    e.changed = true;
                                    e.committed = true;
                                    lab = trf!("프로파일: {name}", "Profile: {name}");
                                }
                            }
                        }
                    });
                });
                if crate::develop::dcp::profile_supports_amount(&s.profile) {
                    sl!(ui, e, &mut lab, tr!("프로파일 양", "Profile amount"), &mut s.profile_amount, 0.0, 200.0, 100.0, 0);
                }
                if let Some(other) = &rc.decoder_fallback {
                    ui.label(egui::RichText::new(trf!("아직 모르는 기종이라 {other} 설정으로 열었습니다 — 밝기·색이 다를 수 있음", "Unknown camera model — opened with the settings of {other}; brightness and colors may be off")).size(10.5).color(ACCENT))
                        .on_hover_text(tr!("데이터 폴더의 cameras 폴더에 이 기종의 rawler 카메라 정의(.toml)를 넣으면 정확한 설정으로 엽니다", "Put a rawler camera definition (.toml) for this model in the cameras folder of the data folder to open it with the exact settings"));
                }
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                theme_label(ui, tr!("화이트 밸런스", "White Balance"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if rc.is_some() {
                        let cur = if s.wb_custom {
                            crate::develop::dcp::WB_PRESETS
                                .iter()
                                .find(|(_, t, ti)| (*t as f32 - s.temp_k).abs() < 1.0 && (*ti as f32 - s.tint_k).abs() < 0.5)
                                .map(|p| p.0)
                                .unwrap_or(tr!("사용자", "Custom"))
                        } else {
                            tr!("촬영 시", "As Shot")
                        };
                        egui::ComboBox::from_id_salt("wb_preset").truncate().width(80.0).selected_text(crate::i18n::t(cur)).show_ui(ui, |ui| {
                            if ui.selectable_label(!s.wb_custom, tr!("촬영 시", "As Shot")).clicked() {
                                s.wb_custom = false;
                                e.changed = true;
                                e.committed = true;
                                lab = tr!("화이트 밸런스: 촬영 시", "White balance: As Shot").into();
                            }
                            if ui.selectable_label(false, tr!("자동", "Auto")).clicked()
                                && let Some((t, ti)) = src_for_auto.as_ref().and_then(|x| crate::develop::dcp::auto_wb(x, &s.profile)) {
                                    s.wb_custom = true;
                                    s.temp_k = (t as f32 / 50.0).round() * 50.0;
                                    s.tint_k = ti.round() as f32;
                                    e.changed = true;
                                    e.committed = true;
                                    lab = tr!("화이트 밸런스: 자동", "White balance: Auto").into();
                                }
                            for (name, t, ti) in crate::develop::dcp::WB_PRESETS {
                                if ui.selectable_label(cur == *name, crate::i18n::t(name)).clicked() {
                                    s.wb_custom = true;
                                    s.temp_k = *t as f32;
                                    s.tint_k = *ti as f32;
                                    e.changed = true;
                                    e.committed = true;
                                    lab = trf!("화이트 밸런스: {name}", "White balance: {name}");
                                }
                            }
                        });
                    } else if ui.small_button(tr!("촬영 시", "As Shot")).clicked() {
                        s.temp = 0.0;
                        s.tint = 0.0;
                        e.changed = true;
                        e.committed = true;
                        lab = tr!("화이트 밸런스: 촬영 시", "White balance: As Shot").into();
                    }
                });
            });
            let tint_grad = [Color32::from_rgb(0x4C, 0xA0, 0x4C), Color32::from_gray(150), Color32::from_rgb(0xC0, 0x4C, 0xB8)];
            if let Some(rc) = &rc {
                let (t0, ti0) = crate::develop::dcp::as_shot_temp_tint(rc, &s.profile);
                let (t0, ti0) = ((t0 as f32 / 50.0).round() * 50.0, ti0.round() as f32);
                let mut tk = if s.wb_custom { s.temp_k } else { t0 };
                let mut tik = if s.wb_custom { s.tint_k } else { ti0 };
                let r1 = widgets::slider_kelvin(ui, tr!("색온도", "Temp"), &mut tk, t0);
                let r2 = widgets::slider_grad(ui, tr!("색조", "Tint"), &mut tik, -150.0, 150.0, ti0, 0, "tint_k", &tint_grad);
                for (r, name) in [(r1, tr!("색온도", "Temp")), (r2, tr!("색조", "Tint"))] {
                    if r.changed {
                        s.wb_custom = !(tk == t0 && tik == ti0);
                        s.temp_k = tk;
                        s.tint_k = tik;
                        lab = format!("{name} {:.0}", if name == tr!("색온도", "Temp") { tk } else { tik });
                    }
                    e.merge(r);
                }
            } else {
                let temp_grad = [Color32::from_rgb(0x50, 0x78, 0xDC), Color32::from_gray(150), Color32::from_rgb(0xE0, 0xC0, 0x40)];
                let r = widgets::slider_grad(ui, tr!("색온도", "Temp"), &mut s.temp, -100.0, 100.0, 0.0, 0, "temp", &temp_grad);
                if r.changed || r.committed {
                    lab = trf!("색온도 {:+.0}", "Temp {:+.0}", s.temp);
                }
                e.merge(r);
                let r = widgets::slider_grad(ui, tr!("색조", "Tint"), &mut s.tint, -100.0, 100.0, 0.0, 0, "tint", &tint_grad);
                if r.changed || r.committed {
                    lab = trf!("색조 {:+.0}", "Tint {:+.0}", s.tint);
                }
                e.merge(r);
            }
            theme_label(ui, tr!("톤", "Tone"));
            sl!(ui, e, &mut lab, tr!("노출", "Exposure"), &mut s.exposure, -5.0, 5.0, 0.0, 2);
            sl!(ui, e, &mut lab, tr!("대비", "Contrast"), &mut s.contrast, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("하이라이트", "Highlights"), &mut s.highlights, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("섀도", "Shadows"), &mut s.shadows, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("화이트", "Whites"), &mut s.whites, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("블랙", "Blacks"), &mut s.blacks, -100.0, 100.0, 0.0, 0);
            theme_label(ui, tr!("외관", "Presence"));
            sl!(ui, e, &mut lab, tr!("텍스처", "Texture"), &mut s.texture, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("명료도", "Clarity"), &mut s.clarity, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("디헤이즈", "Dehaze"), &mut s.dehaze, -100.0, 100.0, 0.0, 0);
            let vib_grad = [Color32::from_gray(140), widgets::band_color(20.0, 0.6, 0.85), widgets::band_color(200.0, 0.9, 0.9)];
            let sat_grad = [Color32::from_gray(140), widgets::band_color(330.0, 0.5, 0.85), widgets::band_color(30.0, 1.0, 0.95)];
            let r = widgets::slider_grad(ui, tr!("바이브런스", "Vibrance"), &mut s.vibrance, -100.0, 100.0, 0.0, 0, "vib", &vib_grad);
            if r.changed || r.committed {
                lab = trf!("바이브런스 {:+.0}", "Vibrance {:+.0}", s.vibrance);
            }
            e.merge(r);
            let r = widgets::slider_grad(ui, tr!("채도", "Saturation"), &mut s.saturation, -100.0, 100.0, 0.0, 0, "sat", &sat_grad);
            if r.changed || r.committed {
                lab = trf!("채도 {:+.0}", "Saturation {:+.0}", s.saturation);
            }
            e.merge(r);
        });
        if rs {
            reset_key = Some("basic");
        }
        toggle_set(&mut bypass, "basic", off);

        let mut off = bypass.contains("curve");
        let (_, rs) = widgets::section_ex(ui, tr!("톤 커브", "Tone Curve"), false, section_modified(s, "curve", is_raw), Some(&mut off), |ui| {
            super::form::seg(ui, &mut curve_mode, &[(CurveMode::Parametric, tr!("영역", "Region")), (CurveMode::Rgb, tr!("포인트", "Point")), (CurveMode::Red, "R"), (CurveMode::Green, "G"), (CurveMode::Blue, "B")]);
            ui.add_space(4.0);
            let c = &mut s.curve;
            match curve_mode {
                CurveMode::Parametric => {
                    param_curve_preview(ui, c);
                    sl!(ui, e, &mut lab, tr!("하이라이트 ", "Highlights "), &mut c.highlights, -100.0, 100.0, 0.0, 0);
                    sl!(ui, e, &mut lab, tr!("밝은 영역", "Lights"), &mut c.lights, -100.0, 100.0, 0.0, 0);
                    sl!(ui, e, &mut lab, tr!("어두운 영역", "Darks"), &mut c.darks, -100.0, 100.0, 0.0, 0);
                    sl!(ui, e, &mut lab, tr!("섀도 ", "Shadows "), &mut c.shadows, -100.0, 100.0, 0.0, 0);
                    egui::CollapsingHeader::new(egui::RichText::new(tr!("영역 분할점", "Region split points")).size(11.0).color(TEXT_WEAK())).id_salt("curve_splits").show(ui, |ui| {
                        let mut sp = c.splits;
                        let r1 = slider(ui, tr!("분할 1", "Split 1"), &mut sp[0], 0.05, 0.45, 0.25, 2, "sp0");
                        let r2 = slider(ui, tr!("분할 2", "Split 2"), &mut sp[1], 0.3, 0.7, 0.5, 2, "sp1");
                        let r3 = slider(ui, tr!("분할 3", "Split 3"), &mut sp[2], 0.55, 0.95, 0.75, 2, "sp2");
                        for r in [r1, r2, r3] {
                            if r.changed || r.committed {
                                lab = tr!("커브 영역 분할", "Curve region split").into();
                            }
                            e.merge(r);
                        }
                        c.splits = sp;
                    });
                }
                mode => {
                    let (pts, col, name) = match mode {
                        CurveMode::Red => (&mut c.red, Color32::from_rgb(0xD0, 0x60, 0x60), tr!("R 커브", "R curve")),
                        CurveMode::Green => (&mut c.green, Color32::from_rgb(0x60, 0xC0, 0x60), tr!("G 커브", "G curve")),
                        CurveMode::Blue => (&mut c.blue, Color32::from_rgb(0x60, 0x80, 0xE0), tr!("B 커브", "B curve")),
                        _ => (&mut c.rgb, Color32::from_gray(220), tr!("포인트 커브", "Point curve")),
                    };
                    let r = widgets::curve_editor(ui, pts, col, name);
                    if r.changed || r.committed {
                        lab = name.into();
                    }
                    e.merge(r);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr!("클릭: 점 추가 · 드래그 · 더블클릭/밖으로: 삭제", "Click: add point · drag · double-click/drag out: delete")).size(10.5).color(TEXT_DIM()));
                        if ui.small_button(tr!("초기화", "Reset")).clicked() {
                            *pts = identity_curve();
                            e.changed = true;
                            e.committed = true;
                            lab = trf!("{name} 초기화", "Reset {name}");
                        }
                    });
                }
            }
        });
        if rs {
            reset_key = Some("curve");
        }
        toggle_set(&mut bypass, "curve", off);
    }

    // Color
    if tab.shows(PanelTab::Color) {
        if s.treatment == Treatment::Color {
            let mut off = bypass.contains("hsl");
            let (_, rs) = widgets::section_ex(ui, tr!("HSL 컬러 믹서", "HSL Color Mixer"), true, section_modified(s, "hsl", is_raw), Some(&mut off), |ui| {
                theme_label(ui, tr!("사진 위에서 끌어 조정", "Drag on the photo to adjust"));
                if let Some(i) = widgets::toggle_row(ui, &[tr!("색조", "Hue"), tr!("채도", "Saturation"), tr!("광도", "Luminance")], tat.map(|t| t as usize), tr!("켜고 사진 위에서 위아래로 끌면 그 색에 해당하는 대역이 바뀝니다", "Turn on, then drag up/down on the photo to change the band of that color")) {
                    tat = if tat == Some(i as u8) { None } else { Some(i as u8) };
                }
                ui.add_space(4.0);
                let (r, l) = widgets::hsl_mixer(ui, &mut s.hsl, &mut hsl_sel, &HSL_BAND_NAMES.map(crate::i18n::t), &HSL_BAND_HUES);
                if !l.is_empty() {
                    lab = l;
                }
                e.merge(r);
            });
            if rs {
                reset_key = Some("hsl");
            }
            toggle_set(&mut bypass, "hsl", off);
            let mut off = bypass.contains("point");
            let (_, rs) = widgets::section_ex(ui, tr!("포인트 컬러", "Point Color"), true, !s.point_colors.is_empty(), Some(&mut off), |ui| {
                point_color_ui(ui, &mut s.point_colors, &mut pc_sel, &mut pc_pick, &mut e, &mut lab);
            });
            if rs {
                reset_key = Some("point");
            }
            toggle_set(&mut bypass, "point", off);
        } else {
            let mut off = bypass.contains("bw");
            let (_, rs) = widgets::section_ex(ui, tr!("흑백 혼합", "B&W Mix"), true, section_modified(s, "bw", is_raw), Some(&mut off), |ui| {
                for b in 0..HSL_BANDS {
                    let name = crate::i18n::t(HSL_BAND_NAMES[b]);
                    let g = [widgets::band_color(HSL_BAND_HUES[b], 0.6, 0.25), widgets::band_color(HSL_BAND_HUES[b], 0.6, 0.6), widgets::band_color(HSL_BAND_HUES[b], 0.3, 1.0)];
                    let r = widgets::slider_grad(ui, name, &mut s.bw_mix[b], -100.0, 100.0, 0.0, 0, name, &g);
                    if r.changed || r.committed {
                        lab = trf!("흑백 혼합 {name}", "B&W mix {name}");
                    }
                    e.merge(r);
                }
            });
            if rs {
                reset_key = Some("bw");
            }
            toggle_set(&mut bypass, "bw", off);
        }
        let mut off = bypass.contains("grading");
        let (_, rs) = widgets::section_ex(ui, tr!("컬러 그레이딩", "Color Grading"), true, section_modified(s, "grading", is_raw), Some(&mut off), |ui| {
            let g = &mut s.grading;
            // Region selection: mini wheel chips (0 = three-way view)
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mw = ((ui.available_width() - 10.0) / 5.0 - 8.0).clamp(22.0, 40.0);
                // Center the five chips
                ui.add_space(((ui.available_width() - 5.0 * (mw + 8.0) - 8.0) / 2.0).max(0.0));
                let three = Wheel { hue: 0.0, sat: 0.0, lum: 0.0 };
                if widgets::mini_wheel(ui, &three, mw, grade_sel == 0, tr!("3분할", "3-way")).on_hover_text(tr!("섀도·미드톤·하이라이트 한 번에", "Shadows·midtones·highlights at once")).clicked() {
                    grade_sel = 0;
                }
                for (i, (wh, n)) in [(&g.shadows, tr!("섀도", "Shadows")), (&g.midtones, tr!("미드톤", "Midtones")), (&g.highlights, tr!("하이라이트", "Highlights")), (&g.global, tr!("전체", "Global"))].into_iter().enumerate() {
                    if widgets::mini_wheel(ui, wh, mw, grade_sel == i as u8 + 1, n).clicked() {
                        grade_sel = i as u8 + 1;
                    }
                }
            });
            ui.add_space(4.0);
            if grade_sel == 0 {
                let w = ((ui.available_width() - 12.0) / 3.0).clamp(60.0, 130.0);
                ui.horizontal(|ui| {
                    for (wh, n) in [(&mut g.shadows, tr!("섀도", "Shadows")), (&mut g.midtones, tr!("미드톤", "Midtones")), (&mut g.highlights, tr!("하이라이트", "Highlights"))] {
                        let r = widgets::color_wheel(ui, wh, w, n);
                        if r.changed || r.committed {
                            lab = trf!("컬러 그레이딩 {n}", "Color grading {n}");
                        }
                        e.merge(r);
                    }
                });
                theme_label(ui, tr!("광도", "Luminance"));
                for (wh, n) in [(&mut g.shadows, tr!("섀도", "Shadows")), (&mut g.midtones, tr!("미드톤", "Midtones")), (&mut g.highlights, tr!("하이라이트", "Highlights"))] {
                    sl!(ui, e, &mut lab, n, &mut wh.lum, -100.0, 100.0, 0.0, 0);
                }
            } else {
                let (wh, n) = match grade_sel {
                    1 => (&mut g.shadows, tr!("섀도", "Shadows")),
                    2 => (&mut g.midtones, tr!("미드톤", "Midtones")),
                    3 => (&mut g.highlights, tr!("하이라이트", "Highlights")),
                    _ => (&mut g.global, tr!("전체", "Global")),
                };
                let size = (ui.available_width() - 16.0).clamp(120.0, 220.0);
                ui.vertical_centered(|ui| {
                    let r = widgets::color_wheel(ui, wh, size, n);
                    if r.changed || r.committed {
                        lab = trf!("컬러 그레이딩 {n}", "Color grading {n}");
                    }
                    e.merge(r);
                });
                let hue_grad: Vec<Color32> = (0..=12).map(|i| widgets::band_color(i as f32 * 30.0, 0.75, 0.9)).collect();
                let r = widgets::slider_grad(ui, tr!("색상", "Hue"), &mut wh.hue, 0.0, 360.0, 0.0, 0, "gr_hue", &hue_grad);
                e.merge(r);
                let sg = [Color32::from_gray(140), widgets::band_color(wh.hue, 1.0, 0.9)];
                let r = widgets::slider_grad(ui, tr!("채도", "Saturation"), &mut wh.sat, 0.0, 100.0, 0.0, 0, "gr_sat", &sg);
                e.merge(r);
                sl!(ui, e, &mut lab, tr!("광도", "Luminance"), &mut wh.lum, -100.0, 100.0, 0.0, 0);
                if e.changed && lab.is_empty() {
                    lab = trf!("컬러 그레이딩 {n}", "Color grading {n}");
                }
            }
            theme_label(ui, tr!("전체 조정", "Global"));
            sl!(ui, e, &mut lab, tr!("혼합", "Blending"), &mut g.blending, 0.0, 100.0, 50.0, 0);
            sl!(ui, e, &mut lab, tr!("균형", "Balance"), &mut g.balance, -100.0, 100.0, 0.0, 0);
        });
        if rs {
            reset_key = Some("grading");
        }
        toggle_set(&mut bypass, "grading", off);

        let mut off = bypass.contains("calib");
        let (_, rs) = widgets::section_ex(ui, tr!("보정 (캘리브레이션)", "Calibration"), false, section_modified(s, "calib", is_raw), Some(&mut off), |ui| {
            let c = &mut s.calibration;
            let tint_grad = [Color32::from_rgb(0x4C, 0xA0, 0x4C), Color32::from_gray(150), Color32::from_rgb(0xC0, 0x4C, 0xB8)];
            let r = widgets::slider_grad(ui, tr!("섀도 색조", "Shadows tint"), &mut c.shadows_tint, -100.0, 100.0, 0.0, 0, "cal_st", &tint_grad);
            e.merge(r);
            for (name, h, hv, sv) in [(tr!("빨강", "Red"), 0.0, &mut c.red_hue, &mut c.red_sat), (tr!("초록", "Green"), 120.0, &mut c.green_hue, &mut c.green_sat), (tr!("파랑", "Blue"), 240.0, &mut c.blue_hue, &mut c.blue_sat)] {
                theme_label(ui, &trf!("{name} 원색", "{name} primary"));
                let hg = [widgets::band_color(h - 40.0, 0.75, 0.9), widgets::band_color(h, 0.75, 0.9), widgets::band_color(h + 40.0, 0.75, 0.9)];
                let r = widgets::slider_grad(ui, tr!("색조", "Hue"), hv, -100.0, 100.0, 0.0, 0, &format!("cal_{name}_h"), &hg);
                e.merge(r);
                let sg = [widgets::band_color(h, 0.1, 0.8), widgets::band_color(h, 1.0, 0.9)];
                let r = widgets::slider_grad(ui, tr!("채도", "Saturation"), sv, -100.0, 100.0, 0.0, 0, &format!("cal_{name}_s"), &sg);
                e.merge(r);
            }
            if e.changed && lab.is_empty() {
                lab = tr!("보정", "Calibration").into();
            }
        });
        if rs {
            reset_key = Some("calib");
        }
        toggle_set(&mut bypass, "calib", off);
    }

    // Detail
    if tab.shows(PanelTab::Detail) {
        if let Some(v) = &mut dview {
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(vec2(w, (w * 0.62).round()), Sense::hover());
            v.zoom = Some(1.0);
            let (resp, _) = v.show(ui, r, &app.dev, &detail_settings, false, false, None, true);
            let p = ui.painter_at(r);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            p.text(r.left_top() + vec2(6.0, 5.0), Align2::LEFT_TOP, "100%", FontId::monospace(10.0), Color32::WHITE);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr!("끌어서 이동", "Drag to move")).size(10.5).color(TEXT_DIM()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let t = if detail_pick { tr!("사진을 누르세요…", "Click on the photo…") } else { tr!("사진에서 위치 고르기", "Pick a location on the photo") };
                    if ui.selectable_label(detail_pick, egui::RichText::new(t).size(11.0)).clicked() {
                        detail_pick = !detail_pick;
                    }
                });
            });
            ui.add_space(4.0);
        }
        let mut off = bypass.contains("detail");
        let (_, rs) = widgets::section_ex(ui, tr!("디테일", "Detail"), true, section_modified(s, "detail", is_raw), Some(&mut off), |ui| {
            let dt = &mut s.detail;
            theme_label(ui, tr!("샤프닝", "Sharpening"));
            sl!(ui, e, &mut lab, tr!("양", "Amount"), &mut dt.sharpen_amount, 0.0, 150.0, if is_raw { 40.0 } else { 0.0 }, 0);
            sl!(ui, e, &mut lab, tr!("반경", "Radius"), &mut dt.sharpen_radius, 0.5, 3.0, 1.0, 1);
            sl!(ui, e, &mut lab, tr!("디테일", "Detail"), &mut dt.sharpen_detail, 0.0, 100.0, 25.0, 0);
            sl!(ui, e, &mut lab, tr!("마스킹", "Masking"), &mut dt.sharpen_masking, 0.0, 100.0, 0.0, 0);
            theme_label(ui, tr!("노이즈 감소", "Noise Reduction"));
            sl!(ui, e, &mut lab, tr!("휘도", "Luminance"), &mut dt.nr_luma, 0.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("휘도 디테일", "Luminance detail"), &mut dt.nr_luma_detail, 0.0, 100.0, 50.0, 0);
            sl!(ui, e, &mut lab, tr!("휘도 대비", "Luminance contrast"), &mut dt.nr_luma_contrast, 0.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("색상", "Color"), &mut dt.nr_color, 0.0, 100.0, if is_raw { 25.0 } else { 0.0 }, 0);
            sl!(ui, e, &mut lab, tr!("색상 디테일", "Color detail"), &mut dt.nr_color_detail, 0.0, 100.0, 50.0, 0);
            sl!(ui, e, &mut lab, tr!("색상 매끄럽게", "Color smoothness"), &mut dt.nr_color_smooth, 0.0, 100.0, 50.0, 0);
            if is_raw {
                ui.add_space(4.0);
                if widgets::button_row(ui, &[(tr!("AI 노이즈 감소… (Ctrl+Alt+I)", "AI Denoise… (Ctrl+Alt+I)"), true, tr!("AI로 노이즈를 지운 새 파일을 만들어 원본과 스택", "Creates a new AI-denoised file stacked with the original"))]).is_some() {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("open_enhance"), true));
                }
            }
            ui.label(egui::RichText::new(tr!("샤프닝·노이즈는 축소 화면에선 거의 보이지 않습니다 — 위 100% 미리보기나 Z(100%)로 확인", "Sharpening and noise are barely visible zoomed out — check the 100% preview above or press Z (100%)")).size(10.5).color(TEXT_DIM()));
        });
        if rs {
            reset_key = Some("detail");
        }
        toggle_set(&mut bypass, "detail", off);
    }

    // Optics
    if tab.shows(PanelTab::Optics) {
        let mut off = bypass.contains("lens");
        let (_, rs) = widgets::section_ex(ui, tr!("렌즈 교정", "Lens Corrections"), true, section_modified(s, "lens", is_raw), Some(&mut off), |ui| {
            let l = &mut s.lens;
            lens_profile_ui(ui, l, lens_info.as_deref(), &mut e, &mut lab);
            theme_label(ui, tr!("수동 교정", "Manual corrections"));
            sl!(ui, e, &mut lab, tr!("왜곡", "Distortion"), &mut l.distortion, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("비네팅", "Vignetting"), &mut l.vignette, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("중간점", "Midpoint"), &mut l.vignette_midpoint, 0.0, 100.0, 0.0, 0);
            theme_label(ui, tr!("프린지 제거", "Defringe"));
            let pg = [Color32::from_gray(140), Color32::from_rgb(0xA0, 0x50, 0xD0)];
            let gg = [Color32::from_gray(140), Color32::from_rgb(0x50, 0xB0, 0x50)];
            let r = widgets::slider_grad(ui, tr!("보라 프린지", "Purple fringe"), &mut l.defringe_purple, 0.0, 20.0, 0.0, 0, "dfp", &pg);
            if r.changed || r.committed {
                lab = tr!("프린지 제거", "Defringe").into();
            }
            e.merge(r);
            let r = widgets::slider_grad(ui, tr!("초록 프린지", "Green fringe"), &mut l.defringe_green, 0.0, 20.0, 0.0, 0, "dfg", &gg);
            if r.changed || r.committed {
                lab = tr!("프린지 제거", "Defringe").into();
            }
            e.merge(r);
        });
        if rs {
            reset_key = Some("lens");
        }
        toggle_set(&mut bypass, "lens", off);

        let mut off = bypass.contains("transform");
        let (_, rs) = widgets::section_ex(ui, tr!("변형", "Transform"), false, section_modified(s, "transform", is_raw), Some(&mut off), |ui| {
            let g = &mut s.geometry;
            sl!(ui, e, &mut lab, tr!("수직", "Vertical"), &mut g.vertical, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("수평", "Horizontal"), &mut g.horizontal, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("회전", "Rotate"), &mut g.angle, -45.0, 45.0, 0.0, 1);
            sl!(ui, e, &mut lab, tr!("배율", "Scale"), &mut g.scale, 50.0, 150.0, 100.0, 0);
            // Auto perspective: vertical = level + vertical perspective, auto = also horizontal perspective
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr!("자동 원근", "Upright")).color(TEXT_WEAK()));
                for (name, vonly, tip) in [
                    (tr!("수직", "Vertical"), true, tr!("세로선을 곧게 (건물·실내를 올려다·내려다 찍은 사진)", "Straighten verticals (photos shot looking up/down at buildings or interiors)")),
                    (tr!("자동", "Auto"), false, tr!("세로·가로선을 함께 곧게", "Straighten verticals and horizontals")),
                ] {
                    if ui.button(name).on_hover_text(tip).clicked()
                        && let Some(src) = &src_for_auto {
                            match crate::develop::pipeline::auto_upright(src, g, vonly) {
                                Some((a, v, hz)) if a != 0.0 || v != 0.0 || hz != 0.0 => {
                                    g.angle = a;
                                    g.vertical = v;
                                    g.horizontal = if vonly { 0.0 } else { hz.clamp(-25.0, 25.0) };
                                    g.crop = constrain_crop(src.width(), src.height(), g, &s.lens);
                                    e.changed = true;
                                    e.committed = true;
                                    lab = trf!("자동 원근 ({name}) 회전 {:+.1}° 수직 {:+.0} 수평 {:+.0}", "Upright ({name}) rotate {:+.1}° vertical {:+.0} horizontal {:+.0}", g.angle, g.vertical, g.horizontal);
                                }
                                _ => lab = tr!("자동 원근: 바로잡을 직선을 찾지 못했습니다", "Upright: no lines found to correct").into(),
                            }
                        }
                }
            });
            if ui.button(tr!("자르기 자동 맞춤", "Auto-fit crop")).on_hover_text(tr!("빈 영역이 보이지 않도록 자르기 축소", "Shrink the crop so no empty area shows")).clicked()
                && let Some((w, h)) = src_dims {
                    g.crop = constrain_crop(w, h, g, &s.lens);
                    e.changed = true;
                    e.committed = true;
                    lab = tr!("자르기 맞춤", "Fit crop").into();
                }
        });
        if rs {
            reset_key = Some("transform");
        }
        toggle_set(&mut bypass, "transform", off);
    }

    // Effects
    if tab.shows(PanelTab::Effects) {
        let mut off = bypass.contains("effects");
        let (_, rs) = widgets::section_ex(ui, tr!("효과", "Effects"), true, section_modified(s, "effects", is_raw), Some(&mut off), |ui| {
            let f = &mut s.effects;
            theme_label(ui, tr!("자르기 후 비네팅", "Post-Crop Vignetting"));
            sl!(ui, e, &mut lab, tr!("양 ", "Amount "), &mut f.vignette_amount, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("중간점 ", "Midpoint "), &mut f.vignette_midpoint, 0.0, 100.0, 50.0, 0);
            sl!(ui, e, &mut lab, tr!("원형률", "Roundness"), &mut f.vignette_roundness, -100.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("페더", "Feather"), &mut f.vignette_feather, 0.0, 100.0, 50.0, 0);
            sl!(ui, e, &mut lab, tr!("하이라이트 보호", "Highlight protection"), &mut f.vignette_highlights, 0.0, 100.0, 0.0, 0);
            theme_label(ui, tr!("그레인", "Grain"));
            sl!(ui, e, &mut lab, tr!("양  ", "Amount  "), &mut f.grain_amount, 0.0, 100.0, 0.0, 0);
            sl!(ui, e, &mut lab, tr!("크기", "Size"), &mut f.grain_size, 0.0, 100.0, 25.0, 0);
            sl!(ui, e, &mut lab, tr!("거칠기", "Roughness"), &mut f.grain_roughness, 0.0, 100.0, 50.0, 0);
        });
        if rs {
            reset_key = Some("effects");
        }
        toggle_set(&mut bypass, "effects", off);
    }

    if let Some(k) = reset_key {
        let dflt = DevelopSettings::default_for(is_raw);
        match k {
            "bw" => s.bw_mix = dflt.bw_mix,
            "point" => s.point_colors.clear(),
            "transform" => {
                s.geometry.vertical = 0.0;
                s.geometry.horizontal = 0.0;
                s.geometry.scale = 100.0;
                s.geometry.angle = 0.0;
            }
            _ => s.copy_groups_from(&dflt, &section_groups(k)),
        }
        bypass.remove(k);
        e.changed = true;
        e.committed = true;
        lab = tr!("섹션 초기화", "Reset section").into();
    }

    ui.add_space(8.0);
    let mut items = vec![(tr!("모두 초기화", "Reset all").to_string(), true, "Ctrl+Shift+R")];
    if !bypass.is_empty() {
        items.push((trf!("꺼 둔 섹션 {}개 다시 켜기", "Turn {} disabled sections back on", bypass.len()), true, ""));
    }
    let refs: Vec<(&str, bool, &str)> = items.iter().map(|(a, b, c)| (a.as_str(), *b, *c)).collect();
    match widgets::button_row(ui, &refs) {
        Some(0) => {
            *s = DevelopSettings::default_for(is_raw);
            e.changed = true;
            e.committed = true;
            lab = tr!("모두 초기화", "Reset all").into();
        }
        Some(1) => bypass.clear(),
        _ => {}
    }
    ui.add_space(20.0);
    d.detail_view = dview;
    d.detail_pick = detail_pick;
    d.panel_tab = tab;
    d.bypass = bypass;
    d.hsl_band = hsl_sel;
    d.pc_sel = pc_sel;
    d.pc_pick = pc_pick;
    d.grade_sel = grade_sel;
    d.curve_mode = curve_mode;
    d.tat = tat;
    (e, lab)
}

fn toggle_set(set: &mut std::collections::HashSet<&'static str>, key: &'static str, on: bool) {
    if on {
        set.insert(key);
    } else {
        set.remove(key);
    }
}

/// Lens profile correction (lensfun, falling back to the camera's embedded data)
fn lens_profile_ui(ui: &mut Ui, l: &mut LensCorrection, li: Option<&crate::develop::image::LensInfo>, e: &mut Edit, lab: &mut String) {
    ui.horizontal(|ui| {
        if ui.checkbox(&mut l.profile_enable, tr!("프로파일 교정 사용", "Enable profile corrections")).on_hover_text(tr!("렌즈 프로파일로 왜곡·주변부 어두움·색수차 교정 — 공개 자료 lensfun을 쓰고, 맞는 렌즈가 없으면 카메라가 RAW에 적어 둔 내장 보정 자료를 씀", "Correct distortion, vignetting and chromatic aberration with lens profiles — uses the open lensfun database, or the correction data the camera recorded in the RAW file when no lens matches")).changed() {
            e.changed = true;
            e.committed = true;
            *lab = if l.profile_enable { tr!("렌즈 프로파일 켜기", "Lens profile on").into() } else { tr!("렌즈 프로파일 끄기", "Lens profile off").into() };
        }
    });
    if !l.profile_enable {
        if let Some(li) = li {
            if !li.lens.is_empty() {
                ui.label(egui::RichText::new(trf!("렌즈: {}", "Lens: {}", li.lens)).size(10.5).color(TEXT_DIM()));
            }
            // Embedded camera corrections that are always applied
            let p = crate::develop::pipeline::embedded_policy(li);
            if p.any() {
                let mut parts = Vec::new();
                if p.dist {
                    parts.push(tr!("왜곡", "distortion"));
                }
                if p.ca {
                    parts.push(tr!("색수차", "chromatic aberration"));
                }
                if p.vig {
                    parts.push(tr!("비네팅", "vignetting"));
                }
                ui.label(egui::RichText::new(trf!("카메라 내장 보정 적용됨: {}", "Built-in lens correction applied: {}", parts.join(", "))).size(10.5).color(TEXT_DIM()))
                    .on_hover_text(tr!("RAW 파일에 카메라가 적어 둔 렌즈 보정 자료", "Lens correction data recorded by the camera in the RAW file"));
            }
        }
        return;
    }
    // Current profile: lensfun (manually chosen lens, otherwise auto from EXIF), else the camera's embedded corrections
    use crate::develop::lensfun;
    let manual_lf = l.profile_file.strip_prefix(lensfun::PREFIX).map(|s| s.to_string());
    let lf_db = lensfun::db();
    let lf_match = match (&lf_db, li) {
        (Some(db), Some(li)) => lensfun::find_for(db, li, manual_lf.as_deref()).map(|(lens, _)| lens.model.clone()),
        _ => None,
    };
    let emb = li.and_then(|li| li.embedded.as_ref());
    let (shown, source) = match (&lf_match, emb) {
        (Some(m), _) => (m.clone(), "lensfun"),
        (None, Some(_)) => (tr!("카메라 내장 보정", "Camera built-in correction").to_string(), "RAW"),
        _ => (tr!("맞는 프로파일 없음", "No matching profile").to_string(), ""),
    };
    let found = lf_match.is_some() || emb.is_some();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(if l.profile_file.is_empty() { tr!("자동", "Auto") } else { tr!("지정", "Manual") }).size(10.5).color(TEXT_WEAK()));
        ui.label(egui::RichText::new(&shown).color(if found { TEXT() } else { ACCENT }));
        if !source.is_empty() {
            ui.label(egui::RichText::new(source).size(10.0).color(TEXT_DIM())).on_hover_text(if source == "RAW" {
                tr!("카메라가 RAW 파일에 적어 둔 렌즈 보정 자료 (왜곡·비네팅·색수차)", "Lens correction data recorded by the camera in the RAW file (distortion, vignetting, chromatic aberration)")
            } else {
                tr!("공개 렌즈 자료 lensfun (lensfun.github.io, CC BY-SA 3.0)", "Open lens database lensfun (lensfun.github.io, CC BY-SA 3.0)")
            });
        }
    });
    if let Some(li) = li {
        ui.label(egui::RichText::new(format!("{} · {:.0}mm · f/{:.1}", li.lens, li.focal, li.fnumber)).size(10.5).color(TEXT_DIM()));
    }
    let sid = ui.id().with("lens_search");
    let mut q: String = ui.data(|d| d.get_temp(sid)).unwrap_or_default();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut q).hint_text(tr!("다른 렌즈 찾기 (예: 24-70)", "Find another lens (e.g. 24-70)")).desired_width(ui.available_width() - 60.0));
        if r.changed() {
            ui.data_mut(|d| d.insert_temp(sid, q.clone()));
        }
        if !l.profile_file.is_empty() && ui.small_button(tr!("자동", "Auto")).on_hover_text(tr!("EXIF 렌즈 정보로 자동 선택", "Pick automatically from EXIF lens info")).clicked() {
            l.profile_file.clear();
            l.profile_name.clear();
            e.changed = true;
            e.committed = true;
            *lab = tr!("렌즈 프로파일 자동", "Lens profile auto").into();
        }
    });
    if q.trim().len() >= 2 {
        let ql = q.to_lowercase();
        let mut shown_n = 0;
        egui::ScrollArea::vertical().id_salt("lens_results").max_height(140.0).show(ui, |ui| {
            let mut seen = std::collections::HashSet::new();
            // lensfun (open lens database)
            if let Some(db) = lensfun::db() {
                for lens in &db.lenses {
                    if shown_n >= 60 {
                        break;
                    }
                    if !lens.model.to_lowercase().contains(&ql) || !seen.insert(lens.model.clone()) {
                        continue;
                    }
                    if ui.selectable_label(false, format!("{} · lensfun", lens.model)).clicked() {
                        l.profile_file = format!("{}{}", lensfun::PREFIX, lens.model);
                        l.profile_name = lens.model.clone();
                        e.changed = true;
                        e.committed = true;
                        *lab = tr!("렌즈 프로파일 선택", "Lens profile selected").into();
                        ui.data_mut(|d| d.insert_temp(sid, String::new()));
                    }
                    shown_n += 1;
                }
            }
        });
    }
    let r = slider(ui, tr!("왜곡 적용", "Distortion amount"), &mut l.profile_dist, 0.0, 200.0, 100.0, 0, "lp_dist");
    if r.changed || r.committed {
        *lab = tr!("렌즈 프로파일 왜곡", "Lens profile distortion").into();
    }
    e.merge(r);
    let r = slider(ui, tr!("비네팅 적용", "Vignetting amount"), &mut l.profile_vig, 0.0, 200.0, 100.0, 0, "lp_vig");
    if r.changed || r.committed {
        *lab = tr!("렌즈 프로파일 비네팅", "Lens profile vignetting").into();
    }
    e.merge(r);
}

/// Point color panel: swatch row (pick/add) plus shift amounts and ranges of the selected entry
fn point_color_ui(ui: &mut Ui, pcs: &mut Vec<crate::develop::settings::PointColor>, sel: &mut usize, pick: &mut bool, e: &mut Edit, lab: &mut String) {
    let swatch = |p: &crate::develop::settings::PointColor| {
        let c = crate::develop::color::hsv_to_rgb(p.hue, p.sat.min(1.0), (p.lum * 1.25).clamp(0.15, 1.0));
        Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, p) in pcs.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
            ui.painter().rect_filled(r.shrink(2.0), 4.0, swatch(p));
            let st = if i == *sel { Stroke::new(2.0, ACCENT) } else { Stroke::new(1.0, BORDER()) };
            ui.painter().rect_stroke(r.shrink(1.0), 5.0, st, egui::StrokeKind::Inside);
            if resp.on_hover_text(trf!("색상 {:.0}° · 채도 {:.0}% · 밝기 {:.0}%", "Hue {:.0}° · Saturation {:.0}% · Brightness {:.0}%", p.hue, p.sat * 100.0, p.lum * 100.0)).clicked() {
                *sel = i;
            }
        }
        let full = pcs.len() >= 8;
        let t = if *pick { tr!("사진에서 누르세요…", "Click on the photo…") } else { tr!("+ 색 고르기", "+ Pick color") };
        if ui.add_enabled(!full, egui::Button::selectable(*pick, t)).on_hover_text(tr!("스포이드: 사진에서 바꿀 색을 누릅니다 (최대 8개)", "Eyedropper: click the color to change on the photo (up to 8)")).clicked() {
            *pick = !*pick;
        }
    });
    if pcs.is_empty() {
        theme_label(ui, tr!("고른 색과 비슷한 색만 골라 색조·채도·광도를 바꿉니다 (하늘·피부·옷 색 등)", "Changes hue·saturation·luminance only for colors similar to the picked one (sky, skin, clothes…)"));
        return;
    }
    *sel = (*sel).min(pcs.len() - 1);
    let p = &mut pcs[*sel];
    ui.add_space(4.0);
    sl!(ui, e, lab, tr!("색조 이동", "Hue shift"), &mut p.hue_shift, -100.0, 100.0, 0.0, 0);
    sl!(ui, e, lab, tr!("채도 이동", "Saturation shift"), &mut p.sat_shift, -100.0, 100.0, 0.0, 0);
    sl!(ui, e, lab, tr!("광도 이동", "Luminance shift"), &mut p.lum_shift, -100.0, 100.0, 0.0, 0);
    theme_label(ui, tr!("범위 (비슷한 색으로 볼 너비)", "Range (how wide \"similar\" is)"));
    sl!(ui, e, lab, tr!("색상 범위", "Color Range"), &mut p.hue_range, 0.0, 100.0, 50.0, 0);
    sl!(ui, e, lab, tr!("채도 범위", "Saturation range"), &mut p.sat_range, 0.0, 100.0, 50.0, 0);
    sl!(ui, e, lab, tr!("밝기 범위", "Brightness range"), &mut p.lum_range, 0.0, 100.0, 50.0, 0);
    if widgets::button_row(ui, &[(tr!("이 색 지우기", "Remove this color"), true, "")]).is_some() {
        pcs.remove(*sel);
        *sel = sel.saturating_sub(1);
        e.changed = true;
        e.committed = true;
        *lab = tr!("포인트 컬러 지우기", "Remove point color").into();
    }
}

fn theme_label(ui: &mut Ui, t: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(t).size(10.5).color(TEXT_DIM()));
}

fn param_curve_preview(ui: &mut Ui, c: &ToneCurve) {
    // Same square as the point curve editor (size stays when switching modes)
    let w = widgets::curve_box_size(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(w, w), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_rgb(0x0E, 0x0E, 0x0E));
    // Display the same curve approximation as the engine
    let s = DevelopSettings { curve: ToneCurve { rgb: identity_curve(), red: identity_curve(), green: identity_curve(), blue: identity_curve(), ..c.clone() }, ..Default::default() };
    let mut eng = crate::develop::pipeline::Engine::default();
    let _ = &mut eng;
    let sp = c.splits;
    for x in sp {
        let xx = rect.left() + x * rect.width();
        p.line_segment([pos2(xx, rect.top()), pos2(xx, rect.bottom())], Stroke::new(1.0, Color32::from_gray(34)));
    }
    let f = |y: f32| -> f32 {
        let bump = |y: f32, lo: f32, hi: f32| if y <= lo || y >= hi { 0.0 } else { (std::f32::consts::PI * (y - lo) / (hi - lo)).sin() };
        let amp = 0.12;
        let (ph, pl, pd, ps) = (s.curve.highlights / 100.0, s.curve.lights / 100.0, s.curve.darks / 100.0, s.curve.shadows / 100.0);
        let mut v = y;
        v += ps * amp * bump(v, 0.0, sp[0] * 2.0_f32.min(1.0) + 0.0001);
        v += pd * amp * bump(v, 0.0, sp[1]) * 0.5 + pd * amp * bump(v, sp[0] * 0.5, sp[1]) * 0.5;
        v += pl * amp * bump(v, sp[1], 1.0) * 0.5 + pl * amp * bump(v, sp[1], sp[2] + (1.0 - sp[2]) * 0.5) * 0.5;
        v += ph * amp * bump(v, sp[2], 1.0);
        v.clamp(0.0, 1.0)
    };
    let pts: Vec<Pos2> = (0..=64).map(|i| {
        let x = i as f32 / 64.0;
        pos2(rect.left() + x * rect.width(), rect.bottom() - f(x) * rect.height())
    }).collect();
    p.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, Color32::from_gray(40)));
    p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::from_gray(220))));
}

fn crop_panel(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) {
    let mut done = false;
    let mut reset = false;
    section(ui, tr!("자르기 · 회전", "Crop · Rotate"), true, |ui| {
        let (bw, bh) = d.viewer.source.as_ref().map(|s| (s.width(), s.height())).unwrap_or((3, 2));
        let g = &mut d.settings.geometry;
        let (fw, fh) = crate::develop::geometry::frame_dims(bw, bh, g);
        // Aspect ratios: pairs (4:5 / 5:4 etc.) are merged via the orientation toggle
        let orig = fw.max(fh) as f32 / fw.min(fh).max(1) as f32;
        let options: [(&str, Option<f32>); 6] = [(tr!("자유", "Free"), None), (tr!("원본", "Original"), Some(orig)), ("1:1", Some(1.0)), ("4:5", Some(1.25)), ("2:3", Some(1.5)), ("16:9", Some(16.0 / 9.0))];
        let names: Vec<&str> = options.iter().map(|o| o.0).collect();
        let cur = options.iter().position(|o| o.0 == d.crop_aspect_label);
        let portrait = g.aspect.map(|a| a < 1.0).unwrap_or(fh > fw);
        let src_dims = d.viewer.source.as_ref().map(|s| (s.width(), s.height()));
        let lens = d.settings.lens.clone();
        let intent = &mut d.crop_intent;
        let mut apply = |g: &mut Geometry, base: Option<f32>, portrait: bool| {
            g.aspect = base.map(|r| if portrait && r != 1.0 { 1.0 / r } else { r });
            let cur = intent.unwrap_or(g.crop);
            let want = match g.aspect {
                Some(a) => reshape_crop(cur, a, fw as f32, fh as f32),
                None => cur,
            };
            *intent = Some(want);
            g.crop = match src_dims {
                Some((w, h)) => fit_rotated(want, g, w, h, &lens),
                None => want,
            };
        };
        theme_label(ui, tr!("비율", "Aspect"));
        let _ = &names;
        // Row of six tiles: icon on top (actual ratio, current orientation), label below
        let gap = 4.0;
        let tw = widgets::tile_w(ui, 6, gap);
        let mut pick = None;
        for row in options.chunks(6).enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (k, (name, base)) in row.1.iter().enumerate() {
                    let i = row.0 * 6 + k;
                    let shown = base.map(|r| if portrait && r != 1.0 { 1.0 / r } else { r });
                    let tip = match base {
                        None => tr!("자유 비율", "Free aspect").to_string(),
                        Some(_) if *name == tr!("원본", "Original") => tr!("사진 원래 비율", "Original aspect").to_string(),
                        Some(_) => trf!("{name} 비율 (X: 가로/세로)", "{name} aspect (X: landscape/portrait)"),
                    };
                    if widgets::ratio_tile(ui, tw, 50.0, shown, name, cur == Some(i)).on_hover_text(tip).clicked() {
                        pick = Some(i);
                    }
                }
            });
            ui.add_space(gap);
        }
        if let Some(i) = pick {
            let (name, base) = options[i];
            d.crop_aspect_label = name.into();
            apply(g, base, portrait);
            e.changed = true;
            e.committed = true;
            *lab = trf!("자르기 비율 {name}", "Crop aspect {name}");
        }
        let base = cur.and_then(|i| options[i].1);
        let can_turn = base.map(|r| r != 1.0).unwrap_or(false);
        ui.add_enabled_ui(can_turn, |ui| {
            if let Some(i) = widgets::toggle_row(ui, &[tr!("가로", "Landscape"), tr!("세로", "Portrait")], Some(if portrait { 1 } else { 0 }), tr!("비율 방향 (X)", "Aspect orientation (X)")) {
                let want = i == 1;
                if want != portrait {
                    apply(g, base, want);
                    e.changed = true;
                    e.committed = true;
                    *lab = if want { tr!("세로 비율", "Portrait aspect").into() } else { tr!("가로 비율", "Landscape aspect").into() };
                }
            }
        });
        ui.add_space(4.0);
        theme_label(ui, tr!("기울기", "Straighten"));
        let r = slider(ui, tr!("각도", "Angle"), &mut g.angle, -45.0, 45.0, 0.0, 1, "angle");
        if r.changed {
            *lab = trf!("각도 {:+.1}", "Angle {:+.1}", g.angle);
            if let Some((w, h)) = src_dims {
                let want = intent.unwrap_or(g.crop);
                *intent = Some(want);
                g.crop = fit_rotated(want, g, w, h, &lens);
            }
        }
        e.merge(r);
        let all = widgets::geo_button_row(ui);
        let row = all.filter(|k| *k < 3);
        let row2 = all.filter(|k| *k >= 3).map(|k| k - 3);
        match row {
            Some(0) => {
                if let Some(src) = &d.viewer.source {
                    match crate::develop::pipeline::auto_straighten(src) {
                        Some(a) => {
                            g.angle = a.clamp(-45.0, 45.0);
                            let want = intent.unwrap_or(g.crop);
                            *intent = Some(want);
                            g.crop = fit_rotated(want, g, src.width(), src.height(), &lens);
                            e.changed = true;
                            e.committed = true;
                            *lab = trf!("자동 수평 {:+.1}°", "Auto straighten {:+.1}°", g.angle);
                        }
                        None => *lab = String::new(),
                    }
                }
            }
            Some(k) => {
                g.rotate90 = (g.rotate90 + if k == 1 { 3 } else { 1 }) % 4;
                g.crop = [0.0, 0.0, 1.0, 1.0];
                *intent = None;
                e.changed = true;
                e.committed = true;
                *lab = if k == 1 { tr!("왼쪽으로 회전", "Rotate left").into() } else { tr!("오른쪽으로 회전", "Rotate right").into() };
            }
            None => {}
        }
        match row2 {
            Some(0) => {
                g.flip_h = !g.flip_h;
                e.changed = true;
                e.committed = true;
                *lab = tr!("좌우 반전", "Flip horizontal").into();
            }
            Some(_) => {
                g.flip_v = !g.flip_v;
                e.changed = true;
                e.committed = true;
                *lab = tr!("상하 반전", "Flip vertical").into();
            }
            None => {}
        }
        let (cw, ch) = crate::develop::geometry::cropped_dims(bw, bh, g);
        ui.add_space(4.0);
        ui.label(mono(format!("{cw} × {ch} px")).size(11.0).color(TEXT_WEAK()));
        widgets::help_box(ui, "crop", &[(tr!("모서리·변 끌기", "Drag corners·edges"), tr!("크기", "Size")), (tr!("안쪽 끌기", "Drag inside"), tr!("이동", "Move")), (tr!("모서리 밖 손잡이·바깥 끌기", "Handles outside corners · drag outside"), tr!("회전", "Rotate")), (tr!("Ctrl+끌기", "Ctrl+drag"), tr!("선을 그어 수평 맞추기", "Draw a line to straighten")), ("X", tr!("가로/세로 비율 바꾸기", "Swap landscape/portrait")), ("Enter / Esc", tr!("완료 / 취소", "Done / Cancel"))]);
        match widgets::action_bar(ui, &[(tr!("초기화", "Reset"), true, tr!("자르기·회전 되돌리기", "Undo crop·rotate"))], (tr!("완료  Enter", "Done  Enter"), true, "")) {
            widgets::Act::Primary => done = true,
            widgets::Act::Secondary(_) => reset = true,
            widgets::Act::None => {}
        }
    });
    let g = &mut d.settings.geometry;
    if reset {
        *g = Geometry { vertical: g.vertical, horizontal: g.horizontal, scale: g.scale, ..Default::default() };
        d.crop_aspect_label = tr!("자유", "Free").into();
        d.crop_intent = None;
        e.changed = true;
        e.committed = true;
        *lab = tr!("자르기 초기화", "Reset crop").into();
    }
    if done {
        d.tool = Tool::None;
        e.committed = true;
        *lab = tr!("자르기", "Crop").into();
    }
}

/// Angle that makes a line drawn on screen (a->b) horizontal in the result (vertical if closer).
/// Maps both endpoints to source coordinates, measures the result's slope at trial angles and solves linearly (independent of the rotation sign convention)
fn straighten_angle(info: &ViewInfo, g: &Geometry, a: Pos2, b: Pos2) -> Option<f32> {
    use crate::develop::geometry::GeoMap;
    let (ba, bb) = (info.screen_to_base(a), info.screen_to_base(b));
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    let out_angle = |ang: f32| -> f32 {
        let mut gg = g.clone();
        gg.angle = ang;
        gg.crop = [0.0, 0.0, 1.0, 1.0];
        let m = GeoMap::new(info.bw, info.bh, &gg, &info.lens, [0.0, 0.0, 1.0, 1.0], 2000, 2000);
        let (x0, y0) = m.inverse(ba[0] * bw, ba[1] * bh);
        let (x1, y1) = m.inverse(bb[0] * bw, bb[1] * bh);
        (y1 - y0).atan2(x1 - x0).to_degrees()
    };
    let wrap = |d: f32| (d + 540.0).rem_euclid(360.0) - 180.0;
    let a0 = g.angle;
    let t0 = out_angle(a0);
    // Target: the nearest 0/90/180 degrees
    let target = (t0 / 90.0).round() * 90.0;
    let slope = wrap(out_angle(a0 + 1.0) - t0);
    if slope.abs() < 0.2 {
        return None;
    }
    let mut ang = a0 - wrap(t0 - target) / slope;
    // Nonlinear corrections (perspective etc.): one more pass
    let t1 = out_angle(ang);
    ang -= wrap(t1 - target) / slope;
    Some(ang)
}

/// Change aspect: keep the intended box's area and center, change only its shape (shrink if outside the frame), so toggling repeatedly does not shrink it
fn reshape_crop(c: [f32; 4], aspect: f32, fw: f32, fh: f32) -> [f32; 4] {
    let area = ((c[2] - c[0]) * fw * (c[3] - c[1]) * fh).max(1.0);
    let mut w = (area * aspect).sqrt();
    let mut h = w / aspect;
    let k = (fw / w).min(fh / h).min(1.0);
    w *= k;
    h *= k;
    let (hw, hh) = (w / fw * 0.5, h / fh * 0.5);
    let cx = ((c[0] + c[2]) * 0.5).clamp(hw, 1.0 - hw);
    let cy = ((c[1] + c[3]) * 0.5).clamp(hh, 1.0 - hh);
    [cx - hw, cy - hh, cx + hw, cy + hh]
}

/// Avoid empty corners after rotation: shrink the intended box only as needed (never grow to the maximum)
fn fit_rotated(intent: [f32; 4], g: &Geometry, bw: usize, bh: usize, lens: &crate::develop::settings::LensCorrection) -> [f32; 4] {
    let mut gg = g.clone();
    gg.crop = intent;
    constrain_crop(bw, bh, &gg, lens)
}


/// Mask kind icon and name
fn shape_icon(s: &MaskShape) -> &'static str {
    match s {
        MaskShape::Brush { .. } => "✒",
        MaskShape::Linear { .. } => "▤",
        MaskShape::Radial { .. } => "○",
        MaskShape::Luminance { .. } => "◐",
        MaskShape::Color { .. } => "◆",
        MaskShape::All => "■",
        MaskShape::Ai { kind, .. } => ai_icon(*kind),
    }
}

fn ai_icon(k: crate::imaging::aimask::AiKind) -> &'static str {
    use crate::imaging::aimask::AiKind;
    match k {
        AiKind::Subject => "●",
        AiKind::Background => "◎",
        AiKind::Sky => "☁",
        AiKind::People => "☺",
    }
}

// AI mask jobs

pub enum AiMaskMsg {
    Status(String),
    Done(Result<(crate::imaging::aimask::Gray, Vec<u8>), String>),
}

pub struct AiMaskJob {
    rx: crossbeam_channel::Receiver<AiMaskMsg>,
    pub status: String,
    pub kind: crate::imaging::aimask::AiKind,
    photo: PhotoId,
    /// When recomputing a mask pasted from another photo: the old key to replace
    replace: Option<String>,
}

/// Start an AI mask computation (in the background: fetch model -> preview -> compute)
pub fn start_ai_mask(app: &mut App, kind: crate::imaging::aimask::AiKind, replace: Option<String>) {
    use crate::imaging::aimask;
    if app.ai_mask_job.is_some() {
        app.toast(tr!("AI 마스크를 만드는 중입니다", "An AI mask is being created"));
        return;
    }
    let Some(d) = &app.devst else { return };
    let Some(src) = d.viewer.source.clone() else {
        app.toast(tr!("원본을 불러오는 중입니다 — 잠시 뒤 다시 누르세요", "Loading the original — click again in a moment"));
        return;
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let photo = d.id;
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("aimask".into())
        .spawn(move || {
            let r = (|| -> anyhow::Result<(aimask::Gray, Vec<u8>)> {
                if aimask::missing_bytes(kind) > 0 {
                    let t2 = tx.clone();
                    aimask::download(kind, &move |d, t, f| {
                        let _ = t2.send(AiMaskMsg::Status(trf!("모델 받는 중 {:.0}/{:.0}MB · {f}", "Downloading model {:.0}/{:.0}MB · {f}", d as f64 / 1_048_576.0, t as f64 / 1_048_576.0)));
                    })?;
                }
                let _ = tx.send(AiMaskMsg::Status(trf!("{} 찾는 중…", "Finding {}…", kind.name())));
                let img = crate::imaging::decode::render_default_preview(&src, crate::config::AI_MASK_EDGE);
                let g = aimask::compute(&img, kind)?;
                let png = g.to_png();
                Ok((g, png))
            })();
            let _ = tx.send(AiMaskMsg::Done(r.map_err(|e| format!("{e:#}"))));
        })
        .expect("aimask thread");
    app.ai_mask_job = Some(AiMaskJob { rx, status: trf!("{} 준비 중", "Preparing {}", kind.name()), kind, photo, replace });
}

/// Turn the result into a mask (only if it is for the currently open photo)
pub fn pump_ai_mask(app: &mut App) {
    let Some(job) = &mut app.ai_mask_job else { return };
    let mut done = None;
    for m in job.rx.try_iter() {
        match m {
            AiMaskMsg::Status(s) => job.status = s,
            AiMaskMsg::Done(r) => done = Some(r),
        }
    }
    let Some(r) = done else { return };
    let job = app.ai_mask_job.take().unwrap();
    let (g, png) = match r {
        Ok(x) => x,
        Err(e) => {
            app.toast_err(trf!("AI 마스크 실패: {e}", "AI mask failed: {e}"));
            return;
        }
    };
    // (Recompute is attempted only once regardless of the result)
    if app.devst.as_ref().map(|d| d.id) != Some(job.photo) {
        app.toast(tr!("다른 사진으로 옮겨 AI 마스크를 넣지 않았습니다", "Moved to another photo, so the AI mask wasn't added"));
        return;
    }
    let key = crate::develop::aistore::key_of(&png);
    if let Err(e) = app.cat.save_ai_mask(&key, &png) {
        app.toast_err(trf!("마스크 저장 실패: {e}", "Couldn't save mask: {e}"));
        return;
    }
    crate::develop::aistore::put(&key, &g);
    let src = job.photo.to_string();
    let d = app.devst.as_mut().unwrap();
    
    let label = match job.replace {
        // Pasted mask: point every component using the same old key at this photo's result
        Some(old) => {
            for m in &mut d.settings.masks {
                for c in &mut m.components {
                    if let MaskShape::Ai { key: k, src: s, .. } = &mut c.shape
                        && *k == old {
                            *k = key.clone();
                            *s = src.clone();
                        }
                }
            }
            trf!("AI 마스크 다시 계산 ({})", "Recalculate AI mask ({})", job.kind.name())
        }
        None => {
            let id = d.next_mask_id;
            d.next_mask_id += 1;
            d.settings.masks.push(Mask {
                id,
                name: job.kind.name().to_string(),
                components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape: MaskShape::Ai { kind: job.kind, key, src } }],
                ..Default::default()
            });
            d.mask_sel = Some(d.settings.masks.len() - 1);
            d.comp_sel = Some(0);
            d.overlay = true;
            d.tool = Tool::Mask;
            trf!("AI 마스크: {}", "AI mask: {}", job.kind.name())
        }
    };
    app.commit(&label);
}

// Remove (content-aware fill)

pub enum FillMsg {
    Status(String),
    Done(Result<(crate::imaging::inpaint::Fill, Vec<u8>, f32), String>),
}

pub struct FillJob {
    rx: crossbeam_channel::Receiver<FillMsg>,
    pub status: String,
    photo: PhotoId,
    sig: u64,
}

/// If there are remove spots to fill, fill them one at a time in the background (fetch model -> compute)
pub fn pump_fill(app: &mut App) {
    if let Some(job) = &mut app.fill_job {
        let mut done = None;
        for m in job.rx.try_iter() {
            match m {
                FillMsg::Status(s) => job.status = s,
                FillMsg::Done(r) => done = Some(r),
            }
        }
        let st = job.status.clone();
        if let Some(d) = &mut app.devst {
            d.fill_status = Some(st);
        }
        let Some(r) = done else { return };
        let job = app.fill_job.take().unwrap();
        if let Some(d) = &mut app.devst {
            d.fill_status = None;
        }
        let (fill, png, scale) = match r {
            Ok(x) => x,
            Err(e) => {
                app.toast_err(trf!("지우기 실패: {e}", "Remove failed: {e}"));
                return;
            }
        };
        if app.devst.as_ref().map(|d| d.id) != Some(job.photo) {
            return;
        }
        let key = crate::develop::aistore::key_of(&png);
        if let Err(e) = app.cat.save_ai_mask(&key, &png) {
            app.toast_err(trf!("지우기 저장 실패: {e}", "Couldn't save removal: {e}"));
            return;
        }
        crate::develop::aistore::put_rgb(&key, crate::develop::aistore::RgbPatch { w: fill.w, h: fill.h, data: fill.rgb });
        let d = app.devst.as_mut().unwrap();
        let mut hit = false;
        for sp in &mut d.settings.spots {
            if sp.remove && sp.shape_sig() == job.sig {
                sp.fill = Some(crate::develop::settings::SpotFill { key: key.clone(), bbox: fill.bbox, scale, sig: job.sig });
                hit = true;
            }
        }
        if hit {
            app.commit(tr!("지우기", "Remove"));
        }
        return;
    }
    // Find the next one to fill
    let Some(d) = &mut app.devst else { return };
    let Some(src) = d.viewer.source.clone() else { return };
    let Some(sp) = d.settings.spots.iter().find(|s| s.needs_fill() && !d.fill_tried.contains(&s.shape_sig())).cloned() else { return };
    let sig = sp.shape_sig();
    d.fill_tried.insert(sig);
    let (tx, rx) = crossbeam_channel::unbounded();
    let photo = d.id;
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("fill".into())
        .spawn(move || {
            use crate::imaging::inpaint;
            let r = (|| -> anyhow::Result<(inpaint::Fill, Vec<u8>, f32)> {
                if inpaint::missing_bytes() > 0 {
                    let t2 = tx.clone();
                    inpaint::download(&move |d, t, f| {
                        let _ = t2.send(FillMsg::Status(trf!("지우기 모델 받는 중 {:.0}/{:.0}MB · {f}", "Downloading remove model {:.0}/{:.0}MB · {f}", d as f64 / 1_048_576.0, t as f64 / 1_048_576.0)));
                    })?;
                }
                let _ = tx.send(FillMsg::Status(tr!("주변 내용으로 채우는 중…", "Filling from surroundings…").into()));
                let (bw, bh) = (src.width() as f32, src.height() as f32);
                let path: Vec<[f32; 2]> = if sp.path.len() >= 2 { sp.path.iter().map(|p| [p[0] * bw, p[1] * bh]).collect() } else { vec![[sp.dst[0] * bw, sp.dst[1] * bh]] };
                let f = inpaint::fill(&src, &path, sp.radius * bw.max(bh))?;
                let (png, scale) = f.to_png();
                Ok((f, png, scale))
            })();
            let _ = tx.send(FillMsg::Done(r.map_err(|e| format!("{e:#}"))));
        })
        .expect("fill thread");
    app.fill_job = Some(FillJob { rx, status: tr!("지우기 준비 중", "Preparing removal").into(), photo, sig });
}

/// If AI masks were pasted from another photo, recompute them for this photo
pub fn refresh_ai_masks(app: &mut App) {
    if app.ai_mask_job.is_some() {
        return;
    }
    let Some(d) = &mut app.devst else { return };
    if d.viewer.source.is_none() {
        return;
    }
    let me = d.id.to_string();
    let stale = d.settings.masks.iter().flat_map(|m| m.components.iter()).find_map(|c| match &c.shape {
        MaskShape::Ai { kind, key, src } if *src != me && !d.ai_refresh_tried.contains(key) => Some((*kind, key.clone())),
        _ => None,
    });
    if let Some((kind, key)) = stale {
        d.ai_refresh_tried.insert(key.clone());
        start_ai_mask(app, kind, Some(key));
    }
}

/// Default shapes for new masks/components
fn shape_templates() -> [(&'static str, &'static str, MaskShape); 6] {
    [
        ("✒", tr!("브러시", "Brush"), MaskShape::Brush { strokes: vec![] }),
        ("▤", tr!("선형", "Linear"), MaskShape::Linear { p0: [0.5, 0.15], p1: [0.5, 0.5] }),
        ("○", tr!("방사형", "Radial"), MaskShape::Radial { center: [0.5, 0.5], radius: [0.25, 0.25], angle: 0.0, feather: 50.0 }),
        ("◐", tr!("휘도 범위", "Luminance Range"), MaskShape::Luminance { lo: 0.6, hi: 1.0, smooth: 0.15 }),
        ("◆", tr!("색상 범위", "Color Range"), MaskShape::Color { hue: 210.0, sat: 0.5, range: 40.0 }),
        ("■", tr!("전체", "Whole Image"), MaskShape::All),
    ]
}

/// Short names for tiles
fn short_shape_name(n: &str) -> &str {
    if n == tr!("휘도 범위", "Luminance Range") {
        tr!("휘도", "Luminance")
    } else if n == tr!("색상 범위", "Color Range") {
        tr!("색상", "Color")
    } else {
        n
    }
}

/// Shapes created by drawing (1 = linear, 2 = radial)
fn drawn_kind(s: &MaskShape) -> Option<u8> {
    match s {
        MaskShape::Linear { .. } => Some(1),
        MaskShape::Radial { .. } => Some(2),
        _ => None,
    }
}

fn mask_panel(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) {
    // New mask: icon tiles
    section(ui, tr!("마스크", "Masks"), true, |ui| {
        let mut add: Option<MaskShape> = None;
        // New mask: row of six tiles (icon on top, name below)
        let w = widgets::tile_w(ui, 6, 4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (icon, name, shape) in shape_templates() {
                let key = if name == tr!("브러시", "Brush") {
                    " (K)"
                } else if name == tr!("선형", "Linear") {
                    " (M)"
                } else if name == tr!("방사형", "Radial") {
                    " (Shift+M)"
                } else {
                    ""
                };
                if widgets::icon_tile(ui, w, 44.0, icon, short_shape_name(name), false).on_hover_text(trf!("새 {name} 마스크{key}", "New {name} mask{key}")).clicked() {
                    add = Some(shape);
                }
            }
        });
        // AI mask row
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w4 = widgets::tile_w(ui, 4, 4.0);
            for k in crate::imaging::aimask::AiKind::ALL {
                let busy = d.ai_busy.map(|b| b == k).unwrap_or(false);
                let tip = trf!("AI가 사진에서 {}을(를) 찾아 마스크로 (처음 한 번 모델을 받음)", "AI finds the {} in the photo and makes a mask (downloads the model once)", k.name());
                if widgets::icon_tile(ui, w4, 44.0, ai_icon(k), &format!("{}{}", k.name(), if busy { "…" } else { "" }), busy).on_hover_text(tip).clicked() && d.ai_busy.is_none() {
                    d.ai_request = Some(k);
                }
            }
        });
        if let Some(st) = &d.ai_status {
            ui.label(egui::RichText::new(st).size(11.0).color(TEXT_WEAK()));
        }
        if let Some(kind) = add.as_ref().and_then(drawn_kind) {
            d.mask_arm = Some((kind, None));
            d.mask_arm_start = None;
            add = None;
        }
        if let Some(shape) = add {
            let id = d.next_mask_id;
            d.next_mask_id += 1;
            d.mask_arm = None;
            d.settings.masks.push(Mask {
                id,
                name: trf!("마스크 {}", "Mask {}", d.settings.masks.len() + 1),
                components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape }],
                ..Default::default()
            });
            d.mask_sel = Some(d.settings.masks.len() - 1);
            d.comp_sel = Some(0);
            d.overlay = true;
            e.changed = true;
            e.committed = true;
            *lab = tr!("마스크 추가", "Add mask").into();
        }
        if let Some((kind, op)) = d.mask_arm {
            ui.add_space(4.0);
            egui::Frame::new().fill(WIDGET()).corner_radius(5.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let what = if kind == 1 { tr!("선형 그라디언트", "Linear Gradient") } else { tr!("방사형 그라디언트", "Radial Gradient") };
                let how = match op {
                    None => tr!("새 마스크", "New mask"),
                    Some(MaskOp::Add) => tr!("더하기", "Add"),
                    Some(MaskOp::Subtract) => tr!("빼기", "Subtract"),
                    Some(MaskOp::Intersect) => tr!("교차", "Intersect"),
                };
                ui.label(egui::RichText::new(format!("{what} · {how}")).size(12.0).strong().color(STRONG()));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr!("사진 위를 드래그해 그리세요", "Drag on the photo to draw")).size(11.0).color(TEXT_WEAK()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(tr!("취소", "Cancel")).on_hover_text("Esc").clicked() {
                            d.mask_arm = None;
                        }
                    });
                });
            });
        }
        ui.add_space(4.0);
        widgets::toggle_line(ui, tr!("영역 표시", "Show overlay"), &mut d.overlay, "O");
        ui.add_space(4.0);
        // Mask list
        if d.settings.masks.is_empty() {
            ui.label(egui::RichText::new(tr!("위에서 마스크 종류를 고르세요", "Pick a mask type above")).size(11.0).color(TEXT_DIM()));
        }
        let mut remove = None;
        let mut dup = None;
        let full = ui.available_width();
        for (i, m) in d.settings.masks.iter_mut().enumerate() {
            let sel = d.mask_sel == Some(i);
            let (rect, resp) = ui.allocate_exact_size(vec2(full, 30.0), Sense::click());
            let pnt = ui.painter();
            if sel {
                pnt.rect_filled(rect, 5.0, WIDGET());
                pnt.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 7.0), vec2(3.0, 16.0)), 1.5, ACCENT);
            } else if resp.hovered() {
                pnt.rect_filled(rect, 5.0, HOVER());
            }
            // Visibility toggle (eye)
            let eye = Rect::from_center_size(rect.left_center() + vec2(16.0, 0.0), vec2(20.0, 20.0));
            let er = ui.interact(eye, ui.id().with(("mask_eye", i)), Sense::click()).on_hover_text(if m.visible { tr!("숨기기", "Hide") } else { tr!("보이기", "Show") });
            pnt.text(eye.center(), Align2::CENTER_CENTER, if m.visible { "●" } else { "○" }, FontId::proportional(12.0), if m.visible { TEXT() } else { TEXT_DIM() });
            if er.clicked() {
                m.visible = !m.visible;
                e.changed = true;
                e.committed = true;
                *lab = tr!("마스크 표시 전환", "Toggle mask visibility").into();
            }
            let icons: String = m.components.iter().map(|c| shape_icon(&c.shape)).collect::<Vec<_>>().join("");
            pnt.text(rect.left_center() + vec2(32.0, 0.0), Align2::LEFT_CENTER, crate::i18n::label(&m.name), FontId::proportional(12.5), if sel { STRONG() } else if m.visible { TEXT() } else { TEXT_DIM() });
            pnt.text(rect.right_center() - vec2(30.0, 0.0), Align2::RIGHT_CENTER, &icons, FontId::proportional(11.0), TEXT_WEAK());
            let xr = Rect::from_center_size(rect.right_center() - vec2(14.0, 0.0), vec2(20.0, 20.0));
            let xresp = ui.interact(xr, ui.id().with(("mask_x", i)), Sense::click()).on_hover_text(tr!("삭제 (Delete)", "Delete (Delete)"));
            pnt.text(xr.center(), Align2::CENTER_CENTER, "×", FontId::proportional(13.0), if xresp.hovered() { ACCENT } else { TEXT_DIM() });
            if xresp.clicked() {
                remove = Some(i);
            } else if resp.clicked() && !er.clicked() {
                d.mask_sel = Some(i);
                d.comp_sel = Some(0);
                d.mask_fresh = false;
            }
            resp.context_menu(|ui| {
                if ui.button(tr!("복제", "Duplicate")).clicked() {
                    dup = Some(i);
                    ui.close();
                }
                if ui.button(if m.invert { tr!("반전 해제", "Uninvert") } else { tr!("반전", "Invert") }).clicked() {
                    m.invert = !m.invert;
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("마스크 반전", "Invert mask").into();
                    ui.close();
                }
                if ui.button(tr!("삭제", "Delete")).clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = dup {
            let mut m = d.settings.masks[i].clone();
            m.id = d.next_mask_id;
            d.next_mask_id += 1;
            m.name = trf!("{} 사본", "{} copy", m.name);
            d.settings.masks.push(m);
            d.mask_sel = Some(d.settings.masks.len() - 1);
            e.changed = true;
            e.committed = true;
            *lab = tr!("마스크 복제", "Duplicate mask").into();
        }
        if let Some(i) = remove {
            d.settings.masks.remove(i);
            d.mask_sel = if d.settings.masks.is_empty() { None } else { Some(i.min(d.settings.masks.len() - 1)) };
            d.comp_sel = d.mask_sel.map(|_| 0);
            e.changed = true;
            e.committed = true;
            *lab = tr!("마스크 삭제", "Delete mask").into();
        }
    });
    let Some(mi) = d.mask_sel else {
        if widgets::action_bar(ui, &[], (tr!("완료", "Done"), true, "")) == widgets::Act::Primary {
            d.tool = Tool::None;
            d.overlay = false;
        }
        return;
    };
    if mi >= d.settings.masks.len() {
        d.mask_sel = None;
        return;
    }
    let brush = &mut d.brush;
    let comp_sel = &mut d.comp_sel;
    let fresh = &mut d.mask_fresh;
    let arm = &mut d.mask_arm;
    let add_op = &mut d.mask_add_op;
    let m = &mut d.settings.masks[mi];
    // Components
    section(ui, tr!("영역", "Region"), true, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (lr, _) = ui.allocate_exact_size(vec2(widgets::SLIDER_LABEL_W, 22.0), Sense::hover());
            widgets::fit_label(ui, lr, tr!("이름", "Name"), TEXT_WEAK());
            let w = ui.available_width();
            if ui.add_sized(vec2(w, 22.0), egui::TextEdit::singleline(&mut m.name)).lost_focus() {
                e.changed = true;
                e.committed = true;
                *lab = tr!("마스크 이름", "Mask name").into();
            }
        });
        ui.add_space(2.0);
        // Component chip list
        let mut remove_c = None;
        let full = ui.available_width();
        for (ci, c) in m.components.iter().enumerate() {
            let on = *comp_sel == Some(ci);
            let (rect, resp) = ui.allocate_exact_size(vec2(full, 24.0), Sense::click());
            let pnt = ui.painter();
            if on {
                pnt.rect_filled(rect, 4.0, WIDGET());
            } else if resp.hovered() {
                pnt.rect_filled(rect, 4.0, HOVER());
            }
            let (op, opc) = match c.op {
                MaskOp::Add => ("+", TEXT()),
                MaskOp::Subtract => ("−", ACCENT),
                MaskOp::Intersect => ("∩", TEXT_WEAK()),
            };
            pnt.text(rect.left_center() + vec2(10.0, 0.0), Align2::CENTER_CENTER, op, FontId::monospace(13.0), opc);
            pnt.text(
                rect.left_center() + vec2(24.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{} {}{}", shape_icon(&c.shape), c.shape.kind_name(), if c.invert { tr!(" · 반전", " · inverted") } else { "" }),
                FontId::proportional(12.0),
                if on { STRONG() } else { TEXT() },
            );
            let mut x_clicked = false;
            if m.components.len() > 1 {
                let xr = Rect::from_center_size(rect.right_center() - vec2(12.0, 0.0), vec2(18.0, 18.0));
                let xresp = ui.interact(xr, ui.id().with(("comp_x", ci)), Sense::click()).on_hover_text(tr!("이 구성 빼기", "Remove this component"));
                pnt.text(xr.center(), Align2::CENTER_CENTER, "×", FontId::proportional(12.0), if xresp.hovered() { ACCENT } else { TEXT_DIM() });
                x_clicked = xresp.clicked();
            }
            if x_clicked {
                remove_c = Some(ci);
            } else if resp.clicked() {
                *comp_sel = Some(ci);
                *fresh = false;
            }
        }
        if let Some(ci) = remove_c {
            m.components.remove(ci);
            *comp_sel = Some(0);
            e.changed = true;
            e.committed = true;
            *lab = tr!("구성 삭제", "Delete component").into();
        }
        // Add/subtract/intersect: equal-width buttons with the shape picker right below
        ui.add_space(2.0);
        let ops = [(MaskOp::Add, tr!("+ 더하기", "+ Add")), (MaskOp::Subtract, tr!("− 빼기", "− Subtract")), (MaskOp::Intersect, tr!("∩ 교차", "∩ Intersect"))];
        let sel_op = ops.iter().position(|o| Some(o.0) == *add_op);
        if let Some(i) = widgets::toggle_row(ui, &[tr!("+ 더하기", "+ Add"), tr!("− 빼기", "− Subtract"), tr!("∩ 교차", "∩ Intersect")], sel_op, tr!("이 마스크에 영역 더하기 · 빼기 · 겹치는 부분만", "Add to · subtract from · intersect with this mask")) {
            *add_op = if *add_op == Some(ops[i].0) { None } else { Some(ops[i].0) };
        }
        if let Some(op) = *add_op {
            let tpl = shape_templates();
            let w = widgets::tile_w(ui, 6, 4.0);
            let mut chosen = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (k, (icon, n, _)) in tpl.iter().enumerate() {
                    if widgets::icon_tile(ui, w, 40.0, icon, short_shape_name(n), false).on_hover_text(*n).clicked() {
                        chosen = Some(k);
                    }
                }
            });
            if let Some(k) = chosen {
                let sh = tpl[k].2.clone();
                if let Some(kind) = drawn_kind(&sh) {
                    *arm = Some((kind, Some(op)));
                } else {
                    *fresh = false;
                    m.components.push(MaskComponent { op, invert: false, shape: sh });
                    *comp_sel = Some(m.components.len() - 1);
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("마스크 구성 추가", "Add mask component").into();
                }
                *add_op = None;
            }
        }
        ui.add_space(2.0);
        if widgets::toggle_line(ui, tr!("전체 반전", "Invert all"), &mut m.invert, "") {
            e.changed = true;
            e.committed = true;
            *lab = tr!("마스크 반전", "Invert mask").into();
        }
        let mut amt = m.amount * 100.0;
        let r = slider(ui, tr!("강도", "Amount"), &mut amt, 0.0, 100.0, 100.0, 0, "mamount");
        m.amount = amt / 100.0;
        if r.changed || r.committed {
            *lab = tr!("마스크 강도", "Mask amount").into();
        }
        e.merge(r);
        // Settings of the selected component
        if let Some(ci) = *comp_sel
            && ci < m.components.len() {
                let c = &mut m.components[ci];
                ui.add_space(6.0);
                theme_label(ui, &trf!("선택한 구성: {}", "Selected component: {}", c.shape.kind_name()));
                let ops = [MaskOp::Add, MaskOp::Subtract, MaskOp::Intersect];
                if let Some(k) = widgets::toggle_row(ui, &[tr!("+ 더하기", "+ Add"), tr!("− 빼기", "− Subtract"), tr!("∩ 교차", "∩ Intersect")], ops.iter().position(|o| *o == c.op), tr!("이 구성을 마스크에 합치는 방식", "How this component combines with the mask"))
                    && c.op != ops[k] {
                        c.op = ops[k];
                        e.changed = true;
                        e.committed = true;
                        *lab = tr!("마스크 연산", "Mask operation").into();
                    }
                if widgets::toggle_line(ui, tr!("구성 반전", "Invert component"), &mut c.invert, "") {
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("구성 반전", "Invert component").into();
                }
                let hint = |ui: &mut Ui, t: &str| ui.label(egui::RichText::new(t).size(10.5).color(TEXT_DIM()));
                match &mut c.shape {
                    MaskShape::Brush { strokes } => {
                        let mut size = brush.size * 100.0;
                        if widgets::slider_pow(ui, tr!("크기", "Size"), &mut size, 0.2, 50.0, 4.0, 1, "bsize", 2.5).changed {
                            brush.size = size / 100.0;
                        }
                        let mut f = brush.feather * 100.0;
                        if widgets::slider_pow(ui, tr!("페더", "Feather"), &mut f, 0.0, 100.0, 60.0, 0, "bfeather", 1.6).changed {
                            brush.feather = f / 100.0;
                        }
                        let mut fl = brush.flow * 100.0;
                        if widgets::slider_pow(ui, tr!("흐름", "Flow"), &mut fl, 1.0, 100.0, 100.0, 0, "bflow", 2.5).changed {
                            brush.flow = fl / 100.0;
                        }
                        if let Some(i) = widgets::toggle_row(ui, &[tr!("칠하기", "Paint"), tr!("지우기", "Erase")], Some(brush.erase as usize), tr!("Alt를 누른 채 칠해도 지우기", "Hold Alt while painting to erase")) {
                            brush.erase = i == 1;
                        }
                        if widgets::button_row(ui, &[(tr!("획 모두 지우기", "Erase all strokes"), !strokes.is_empty(), "")]).is_some() {
                            strokes.clear();
                            e.changed = true;
                            e.committed = true;
                            *lab = tr!("브러시 지우기", "Erase brush").into();
                        }
                        widgets::help_box(ui, "brush", &[("[ ]", tr!("브러시 크기", "Brush size")), (tr!("Alt+칠하기", "Alt+paint"), tr!("지우기", "Erase")), (tr!("Space+끌기", "Space+drag"), tr!("화면 이동", "Pan"))]);
                    }
                    MaskShape::Radial { feather, .. } => {
                        let r = slider(ui, tr!("페더", "Feather"), feather, 0.0, 100.0, 50.0, 0, "rfeather");
                        if r.changed || r.committed {
                            *lab = tr!("방사형 페더", "Radial feather").into();
                        }
                        e.merge(r);
                        widgets::help_box(ui, "radial", &[(tr!("가운데 점", "Center point"), tr!("이동", "Move")), (tr!("테두리 점", "Edge point"), tr!("크기", "Size")), (tr!("바깥 점", "Outer point"), tr!("회전", "Rotate")), (tr!("빈 곳 끌기", "Drag empty area"), tr!("화면 이동", "Pan"))]);
                    }
                    MaskShape::Linear { .. } => {
                        widgets::help_box(ui, "linear", &[(tr!("양 끝 점", "End points"), tr!("범위", "Range")), (tr!("가운데 점", "Center point"), tr!("이동", "Move")), (tr!("빈 곳 끌기", "Drag empty area"), tr!("화면 이동", "Pan"))]);
                    }
                    MaskShape::Luminance { lo, hi, smooth } => {
                        let mut a = *lo * 100.0;
                        let mut b = *hi * 100.0;
                        let mut sm = *smooth * 100.0;
                        for (r, n) in [
                            (slider(ui, tr!("최소", "Min"), &mut a, 0.0, 100.0, 0.0, 0, "llo"), tr!("휘도 범위", "Luminance Range")),
                            (slider(ui, tr!("최대", "Max"), &mut b, 0.0, 100.0, 100.0, 0, "lhi"), tr!("휘도 범위", "Luminance Range")),
                            (slider(ui, tr!("부드럽게", "Smoothness"), &mut sm, 0.0, 50.0, 15.0, 0, "lsm"), tr!("휘도 범위", "Luminance Range")),
                        ] {
                            if r.changed || r.committed {
                                *lab = n.into();
                            }
                            e.merge(r);
                        }
                        *lo = a.min(b) / 100.0;
                        *hi = b.max(a) / 100.0;
                        *smooth = sm / 100.0;
                    }
                    MaskShape::Color { hue, range, .. } => {
                        let hue_grad: Vec<Color32> = (0..=12).map(|i| widgets::band_ui(i as f32 * 30.0, 0.75, 0.9)).collect();
                        let r1 = widgets::slider_grad(ui, tr!("색상", "Hue"), hue, 0.0, 360.0, 0.0, 0, "chue", &hue_grad);
                        let r2 = slider(ui, tr!("범위", "Range"), range, 5.0, 180.0, 40.0, 0, "crange");
                        for r in [r1, r2] {
                            if r.changed || r.committed {
                                *lab = tr!("색상 범위", "Color Range").into();
                            }
                            e.merge(r);
                        }
                        hint(ui, tr!("사진을 클릭하면 그 색을 고릅니다", "Click on the photo to pick that color"));
                    }
                    MaskShape::All => {
                        hint(ui, tr!("사진 전체 — 다른 구성과 빼기·교차해 쓰세요", "Whole photo — use it with other components to subtract or intersect"));
                    }
                    MaskShape::Ai { kind, .. } => {
                        hint(ui, &trf!("AI가 찾은 {} — 브러시·그라디언트를 더하거나 빼서 다듬으세요", "{} found by AI — refine by adding or subtracting brushes and gradients", kind.name()));
                    }
                }
            }
    });
    // Adjustments (by group)
    let a = &mut m.adj;
    let mut l2 = String::new();
    let mut ee = Edit::default();
    let mut reset = false;
    section(ui, tr!("보정", "Calibration"), true, |ui| {
        theme_label(ui, tr!("빛", "Light"));
        sl!(ui, ee, &mut l2, tr!("노출", "Exposure"), &mut a.exposure, -4.0, 4.0, 0.0, 2);
        sl!(ui, ee, &mut l2, tr!("대비", "Contrast"), &mut a.contrast, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("하이라이트", "Highlights"), &mut a.highlights, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("섀도", "Shadows"), &mut a.shadows, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("화이트", "Whites"), &mut a.whites, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("블랙", "Blacks"), &mut a.blacks, -100.0, 100.0, 0.0, 0);
        theme_label(ui, tr!("색", "Color"));
        sl!(ui, ee, &mut l2, tr!("색온도", "Temp"), &mut a.temp, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("색조", "Tint"), &mut a.tint, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("색상", "Hue"), &mut a.hue, -180.0, 180.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("채도", "Saturation"), &mut a.saturation, -100.0, 100.0, 0.0, 0);
        theme_label(ui, tr!("외관", "Presence"));
        sl!(ui, ee, &mut l2, tr!("텍스처", "Texture"), &mut a.texture, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("명료도", "Clarity"), &mut a.clarity, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("디헤이즈", "Dehaze"), &mut a.dehaze, -100.0, 100.0, 0.0, 0);
        theme_label(ui, tr!("디테일", "Detail"));
        sl!(ui, ee, &mut l2, tr!("선명도", "Sharpness"), &mut a.sharpness, -100.0, 100.0, 0.0, 0);
        sl!(ui, ee, &mut l2, tr!("노이즈", "Noise"), &mut a.noise, -100.0, 100.0, 0.0, 0);
        ui.add_space(4.0);
        if widgets::button_row(ui, &[(tr!("보정 초기화", "Reset adjustments"), !a.is_zero(), tr!("이 마스크의 보정 값을 모두 0으로", "Set all adjustments of this mask to 0"))]).is_some() {
            reset = true;
        }
    });
    if reset {
        *a = LocalAdjust::default();
        ee.changed = true;
        ee.committed = true;
        l2 = tr!("마스크 보정 초기화", "Reset mask adjustments").into();
    }
    if !l2.is_empty() {
        *lab = trf!("마스크: {l2}", "Mask: {l2}");
    }
    e.merge(ee);
    if widgets::action_bar(ui, &[], (tr!("완료", "Done"), true, "")) == widgets::Act::Primary {
        d.tool = Tool::None;
        d.overlay = false;
    }
}

// Canvas and tool interaction

fn canvas_ui(app: &mut App, ui: &mut Ui, rect: Rect) -> (Edit, String) {
    let mut e = Edit::default();
    let mut lab = String::new();
    // Reference view: left half shows the reference photo, the rest of the canvas is the right half
    let rect = match (app.ref_view, app.dev_ref.filter(|r| app.cat.get(*r).is_some())) {
        (true, Some(rid)) => {
            let half = rect.width() * 0.5;
            let lr = Rect::from_min_size(rect.min, vec2(half - 1.0, rect.height()));
            let same = app.devst.as_ref().map(|d| d.id == rid).unwrap_or(false);
            paint_reference(app, ui, lr, rid, same);
            Rect::from_min_size(rect.min + vec2(half + 1.0, 0.0), vec2(half - 1.0, rect.height()))
        }
        _ => rect,
    };
    // Soft proofing: settings bar over the canvas
    let proof_v = super::proof_ui::view_value(app);
    let proof_name = (proof_v != 0).then(|| super::proof_ui::profile_name(app));
    let rect = if app.proof.on {
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
        ui.painter().rect_filled(bar, 0.0, PANEL2());
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 0.0))), |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new(tr!("교정", "Proof")).strong().color(ACCENT));
                super::proof_ui::controls(app, ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(tr!("끄기 (S)", "Off (S)")).clicked() {
                        app.proof.on = false;
                    }
                });
            });
        });
        Rect::from_min_max(rect.min + vec2(0.0, 31.0), rect.max)
    } else {
        rect
    };
    let d = app.devst.as_mut().unwrap();
    // Display settings (full frame while the crop tool is active)
    let mut view_settings = d.settings.clone();
    apply_bypass(&mut view_settings, &d.bypass, d.is_raw);
    if d.tool == Tool::Crop {
        view_settings.geometry.crop = [0.0, 0.0, 1.0, 1.0];
    }
    view_settings.view_proof = proof_v;
    let before_settings = {
        let mut b = DevelopSettings::default_for(d.is_raw);
        b.geometry = view_settings.geometry.clone();
        b.view_proof = proof_v;
        b
    };
    let overlay = if d.tool == Tool::Mask && d.overlay { d.mask_sel } else { None };
    let pan = (matches!(d.tool, Tool::None | Tool::WbPicker) && d.tat.is_none() && !d.pc_pick) || ui.input(|i| i.key_down(Key::Space));
    let draft = d.active;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let split = d.before_after == BeforeAfter::Split;
        let (main_rect, before_rect) = if d.before_after == BeforeAfter::SideBySide {
            let half = rect.width() * 0.5;
            (
                Rect::from_min_size(rect.min + vec2(half + 1.0, 0.0), vec2(half - 1.0, rect.height())),
                Some(Rect::from_min_size(rect.min, vec2(half - 1.0, rect.height()))),
            )
        } else if split {
            // Draw the before view in the same pane, cover it with the after view, then repaint only the left side with before
            (rect, Some(rect))
        } else {
            (rect, None)
        };
        // Before view (side by side)
        if let Some(br) = before_rect {
            if d.before.is_none() {
                let mut b = Viewer::new(d.id, d.viewer.path.clone(), d.viewer.orientation, 1, &app.dev);
                b.source = d.viewer.source.clone();
                b.loading = false;
                b.placeholder = d.viewer.placeholder.clone();
                d.before = Some(b);
            }
            let b = d.before.as_mut().unwrap();
            if b.source.is_none() {
                b.source = d.viewer.source.clone();
            }
            b.zoom = d.viewer.zoom;
            b.center = d.viewer.center;
            b.show(ui, br, &app.dev, &before_settings, draft, false, None, false);
            if !split {
                ui.painter().text(br.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, tr!("이전", "Before"), FontId::monospace(11.0), TEXT_WEAK());
            }
        } else {
            d.before = None;
        }
        let hover = if d.tool == Tool::None { d.hover_preset.clone() } else { None };
        let show_settings = if d.before_after == BeforeAfter::Before {
            &before_settings
        } else if let Some(h) = &hover {
            h
        } else {
            &view_settings
        };
        // Show clipping only while dragging a slider with Alt held
        let clip_view = d.clipping || (d.active && ui.input(|i| i.modifiers.alt));
        let (resp, info) = d.viewer.show(ui, main_rect, &app.dev, show_settings, draft, clip_view, overlay, pan);
        if split
            && let Some(b) = &d.before {
                let x = rect.left() + rect.width() * d.split_pos;
                let left = Rect::from_min_max(rect.min, pos2(x, rect.max.y));
                let lp = ui.painter_at(left);
                lp.rect_filled(left, 0.0, CANVAS());
                b.paint_copy(&lp, vec2(0.0, 0.0));
                // Divider and handle (drag to move)
                let hr = Rect::from_center_size(pos2(x, rect.center().y), vec2(14.0, rect.height()));
                let hresp = ui.interact(hr, egui::Id::new("split_handle"), Sense::drag());
                // Decide the cursor from the current pointer position directly (egui hit-testing can miss the handle or leave it stuck after dragging)
                let near = ui.input(|i| i.pointer.hover_pos()).map(|pp| rect.contains(pp) && (pp.x - x).abs() <= 7.0).unwrap_or(false);
                if near || hresp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if hresp.dragged()
                    && let Some(pp) = hresp.interact_pointer_pos() {
                        d.split_pos = ((pp.x - rect.left()) / rect.width()).clamp(0.02, 0.98);
                    }
                let p = ui.painter_at(rect);
                p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(3.0, Color32::from_black_alpha(120)));
                p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
                let c = pos2(x, rect.center().y);
                p.circle_filled(c, 11.0, Color32::from_black_alpha(170));
                p.circle_stroke(c, 11.0, Stroke::new(1.2, Color32::WHITE));
                for dx in [-1.0f32, 1.0] {
                    let tip = c + vec2(dx * 7.0, 0.0);
                    p.add(egui::Shape::convex_polygon(vec![tip, tip - vec2(dx * 4.0, 3.5), tip - vec2(dx * 4.0, -3.5)], Color32::WHITE, Stroke::NONE));
                }
                for (txt, at, al) in [(tr!("이전", "Before"), rect.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP), (tr!("이후", "After"), rect.right_top() + vec2(-10.0, 8.0), Align2::RIGHT_TOP)] {
                    let g = p.layout_no_wrap(txt.to_string(), FontId::proportional(11.5), Color32::WHITE);
                    let br = al.anchor_size(at, g.size() + vec2(12.0, 6.0));
                    p.rect_filled(br, 4.0, Color32::from_black_alpha(150));
                    p.galley(br.center() - g.size() * 0.5, g, Color32::WHITE);
                }
            }
        app.view_mem.insert(d.id, (d.viewer.zoom, d.viewer.center));
        if let Some(n) = &proof_name {
            super::proof_ui::badge(n, ui, main_rect);
        }
        if hover.is_some() {
            ui.painter().text(main_rect.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, tr!("프리셋 미리보기", "Preset preview"), FontId::monospace(11.0), ACCENT);
        } else if d.before_after == BeforeAfter::Before {
            ui.painter().text(main_rect.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, tr!("이전 (\\)", "Before (\\)"), FontId::monospace(11.0), ACCENT);
        } else if d.before_after == BeforeAfter::SideBySide {
            ui.painter().text(main_rect.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, tr!("이후", "After"), FontId::monospace(11.0), TEXT_WEAK());
        }
        let Some(info) = info else { return };
        if d.tool == Tool::None {
            if let Some(row) = d.tat {
                hsl_tat(d, ui, &resp, row, &mut e, &mut lab);
            }
            // Detail 1:1 preview location: clicked point in pick mode; the Detail tab shows the region
            if d.panel_tab.shows(PanelTab::Detail)
                && let Some(v) = &mut d.detail_view
                    && d.detail_pick {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        if resp.clicked()
                            && let Some(pp) = resp.interact_pointer_pos() {
                                let c = info.screen_to_cropnorm(pp);
                                v.center = [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0)];
                                d.detail_pick = false;
                            }
                    }
        }
        let _ = &info;
        if d.pc_pick {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if let Some(hp) = resp.hover_pos() {
                ui.painter().text(hp + vec2(14.0, 14.0), Align2::LEFT_TOP, tr!("바꿀 색 누르기 · Esc 취소", "Click the color to change · Esc cancels"), FontId::proportional(11.5), Color32::WHITE);
            }
            if resp.clicked()
                && let Some(c) = resp.interact_pointer_pos().and_then(|pp| d.viewer.sample_output(pp)) {
                    let q = [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0];
                    let (hue, _) = crate::develop::color::rgb_hue(q[0], q[1], q[2]);
                    let mx = q[0].max(q[1]).max(q[2]).max(1e-5);
                    let sat = (mx - q[0].min(q[1]).min(q[2])) / mx;
                    let lum = crate::develop::color::luma(q[0], q[1], q[2]);
                    d.settings.point_colors.push(crate::develop::settings::PointColor { hue, sat, lum, ..Default::default() });
                    d.pc_sel = d.settings.point_colors.len() - 1;
                    d.pc_pick = false;
                    e.changed = true;
                    e.committed = true;
                    lab = tr!("포인트 컬러 추가", "Add point color").into();
                }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                d.pc_pick = false;
            }
        } else {
            match d.tool {
            Tool::Crop => crop_tool(d, ui, &resp, &info, &mut e, &mut lab),
            Tool::Mask => mask_tool(d, ui, &resp, &info, &mut e, &mut lab),
            Tool::Privacy => privacy_tool(d, ui, &resp, &info, &mut e, &mut lab),
            Tool::RedEye => redeye_tool(d, ui, &resp, &info, &mut e, &mut lab),
            Tool::Spot => spot_tool(d, ui, &resp, &info, &mut e, &mut lab),
            Tool::WbPicker => {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                let rc = d.viewer.source.as_ref().and_then(|x| x.raw_color.clone());
                // RAW: camera neutral -> temperature/tint (DNG-style); otherwise relative values
                let pick = |c: [f32; 3], s: &DevelopSettings| -> (f32, f32, bool) {
                    match &rc {
                        Some(rc) => match crate::develop::dcp::temp_tint_from_camera_rgb(rc, &s.profile, c) {
                            Some((t, ti)) => (((t / 50.0).round() * 50.0) as f32, ti.round() as f32, true),
                            None => (s.temp_k, s.tint_k, true),
                        },
                        None => {
                            let (t, ti) = crate::develop::pipeline::wb_from_sample(c, &DevelopSettings::default());
                            (t.round(), ti.round(), false)
                        }
                    }
                };
                if let Some(hp) = resp.hover_pos() {
                    let b = info.screen_to_base(hp);
                    if let Some(c) = d.viewer.sample_source(b) {
                        let (t, ti, k) = pick(c, &d.settings);
                        let txt = if k { trf!("{t:.0} K  색조 {ti:+.0}", "{t:.0} K  Tint {ti:+.0}") } else { trf!("색온도 {t:+.0}  색조 {ti:+.0}", "Temp {t:+.0}  Tint {ti:+.0}") };
                        ui.painter().text(hp + vec2(14.0, 14.0), Align2::LEFT_TOP, txt, FontId::monospace(11.0), Color32::WHITE);
                    }
                }
                if resp.clicked()
                    && let Some(pp) = resp.interact_pointer_pos() {
                        let b = info.screen_to_base(pp);
                        if let Some(c) = d.viewer.sample_source(b) {
                            let (t, ti, k) = pick(c, &d.settings);
                            if k {
                                d.settings.wb_custom = true;
                                d.settings.temp_k = t;
                                d.settings.tint_k = ti;
                            } else {
                                d.settings.temp = t;
                                d.settings.tint = ti;
                            }
                            e.changed = true;
                            e.committed = true;
                            lab = tr!("화이트 밸런스 선택", "Select white balance").into();
                            d.tool = Tool::None;
                        }
                    }
            }
            Tool::None => {
                if resp.double_clicked() {
                    d.viewer.toggle_zoom(resp.interact_pointer_pos());
                }
            }
        }
        }
        // Right-click: pick the target under the pointer (spot, redaction, mask) and show its menu
        if resp.secondary_clicked() {
            d.ctx_target = canvas_hit(d, &info, resp.interact_pointer_pos());
            match d.ctx_target {
                CtxTarget::Spot(i) => d.spot_sel = Some(i),
                CtxTarget::Privacy(i) => d.privacy_sel = Some(i),
                CtxTarget::RedEye(i) => d.redeye_sel = Some(i),
                CtxTarget::Mask(i) => {
                    if d.mask_sel != Some(i) {
                        d.mask_sel = Some(i);
                        d.comp_sel = Some(0);
                    }
                }
                CtxTarget::None => {}
            }
        }
        let mut go_library = false;
        resp.context_menu(|ui| {
            if canvas_menu(d, ui, &mut e, &mut lab) {
                ui.separator();
            }
            if d.tool != Tool::None && ui.button(tr!("도구 끝내기 (Esc)", "Exit tool (Esc)")).clicked() {
                d.tool = Tool::None;
                d.overlay = false;
                ui.close();
            }
            if ui.button(tr!("라이브러리에서 보기 (G)", "View in Library (G)")).clicked() {
                go_library = true;
                ui.close();
            }
        });
        if go_library {
            app.dlg.pending_module = Some(Module::Library);
        }
    });
    if let Some(m) = app.dlg.pending_module.take()
        && m == Module::Library {
            app.set_lib_view(LibView::Grid);
        }
    (e, lab)
}

fn crop_tool(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    let g = &mut d.settings.geometry;
    let (fw, fh) = crate::develop::geometry::frame_dims(info.bw, info.bh, g);
    // The view shows the whole rotated frame (crop=[0,0,1,1]), so crop-normalized = frame-normalized
    let to_s = |x: f32, y: f32| info.cropnorm_to_screen([x, y]);
    let c = g.crop;
    let r = Rect::from_min_max(to_s(c[0], c[1]), to_s(c[2], c[3]));
    let p = ui.painter_at(info.canvas);
    let dim = Color32::from_black_alpha(150);
    let ir = info.image_rect;
    p.rect_filled(Rect::from_min_max(ir.min, pos2(ir.max.x, r.min.y)), 0.0, dim);
    p.rect_filled(Rect::from_min_max(pos2(ir.min.x, r.max.y), ir.max), 0.0, dim);
    p.rect_filled(Rect::from_min_max(pos2(ir.min.x, r.min.y), pos2(r.min.x, r.max.y)), 0.0, dim);
    p.rect_filled(Rect::from_min_max(pos2(r.max.x, r.min.y), pos2(ir.max.x, r.max.y)), 0.0, dim);
    p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Middle);
    for i in 1..3 {
        let t = i as f32 / 3.0;
        let x = r.left() + r.width() * t;
        let y = r.top() + r.height() * t;
        p.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(1.0, Color32::from_white_alpha(70)));
        p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(70)));
    }
    for (hx, hy) in [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (0.0, 0.5), (1.0, 0.5), (0.0, 1.0), (0.5, 1.0), (1.0, 1.0)] {
        let hp = pos2(r.left() + r.width() * hx, r.top() + r.height() * hy);
        p.rect_filled(Rect::from_center_size(hp, vec2(8.0, 8.0)), 0.0, Color32::WHITE);
    }
    // Rotation handles outside the four corners (arc arrows); drag to rotate
    let rot_handles: Vec<Pos2> = [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .iter()
        .map(|(sx, sy)| {
            let corner = pos2(if *sx < 0.0 { r.left() } else { r.right() }, if *sy < 0.0 { r.top() } else { r.bottom() });
            // Pull handles inward when off screen (so they can be grabbed even if the box touches the edge)
            let c = info.canvas.shrink(14.0);
            let q = corner + vec2(sx * 16.0, sy * 16.0);
            pos2(q.x.clamp(c.left(), c.right()), q.y.clamp(c.top(), c.bottom()))
        })
        .collect();
    let hover = resp.hover_pos();
    for (k, hc) in rot_handles.iter().enumerate() {
        let hot = hover.map(|h| (h - *hc).length() < 12.0).unwrap_or(false) || matches!(d.drag, Drag::CropRotate { .. });
        let col = if hot { ACCENT } else { Color32::WHITE };
        p.circle_filled(*hc, 11.0, Color32::from_black_alpha(170));
        p.circle_stroke(*hc, 11.0, Stroke::new(1.0, Color32::from_white_alpha(60)));
        // Curve bowed outward (away from the corner) with arrowheads at both ends: can rotate either way
        let (sx, sy) = [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)][k];
        let out = sy.atan2(sx);
        let rr = 6.5;
        let ctr = *hc - vec2(sx, sy).normalized() * 2.5;
        let n = 12;
        let pts: Vec<Pos2> = (0..=n).map(|i| {
            let t = out - 1.15 + 2.3 * i as f32 / n as f32;
            ctr + vec2(t.cos(), t.sin()) * rr
        }).collect();
        p.add(egui::Shape::line(pts.clone(), Stroke::new(1.5, col)));
        for (end, prev) in [(pts[0], pts[1]), (pts[n], pts[n - 1])] {
            let dir = (end - prev).normalized();
            let nrm = vec2(-dir.y, dir.x);
            let tip = end + dir * 2.6;
            p.add(egui::Shape::convex_polygon(vec![tip, end - dir * 0.8 + nrm * 2.6, end - dir * 0.8 - nrm * 2.6], col, Stroke::NONE));
        }
    }
    // While rotating: fine grid and angle
    if matches!(d.drag, Drag::CropRotate { .. }) {
        for i in 1..9 {
            let t = i as f32 / 9.0;
            let x = r.left() + r.width() * t;
            let y = r.top() + r.height() * t;
            p.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(1.0, Color32::from_white_alpha(45)));
            p.line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(1.0, Color32::from_white_alpha(45)));
        }
    }
    if matches!(d.drag, Drag::CropRotate { .. } | Drag::Straighten(_)) {
        let txt = format!("{:+.1}°", g.angle);
        let at = pos2(r.center().x, r.top() - 18.0).max(pos2(r.center().x, info.canvas.top() + 14.0));
        let gal = p.layout_no_wrap(txt, FontId::monospace(12.0), Color32::WHITE);
        let br = Rect::from_center_size(at, gal.size() + vec2(14.0, 6.0));
        p.rect_filled(br, 4.0, Color32::from_black_alpha(190));
        p.galley(br.center() - gal.size() * 0.5, gal, Color32::WHITE);
    }
    // Straighten ruler: line being drawn
    if let Drag::Straighten(a) = d.drag
        && let Some(b) = resp.interact_pointer_pos() {
            p.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(160)));
            p.line_segment([a, b], Stroke::new(1.5, ACCENT));
            p.circle_filled(a, 3.0, ACCENT);
            p.circle_filled(b, 3.0, ACCENT);
        }
    let Some(pp) = resp.interact_pointer_pos().or(resp.hover_pos()) else { return };
    // Drag start: handle hit test
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let pp = press_pos(ui, resp).unwrap_or(pp);
        let near = |a: f32, b: f32| (a - b).abs() < GRAB;
        let ex = if near(pp.x, r.left()) { -1 } else if near(pp.x, r.right()) { 1 } else { 0 };
        let ey = if near(pp.y, r.top()) { -1 } else if near(pp.y, r.bottom()) { 1 } else { 0 };
        let inside_band = r.expand(GRAB).contains(pp);
        let on_rot = rot_handles.iter().any(|h| (pp - *h).length() < 14.0);
        let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        d.drag = if ctrl {
            Drag::Straighten(pp)
        } else if on_rot {
            let cpt = r.center();
            Drag::CropRotate { start_angle: g.angle, start_ptr: (pp.y - cpt.y).atan2(pp.x - cpt.x) }
        } else if (ex != 0 || ey != 0) && inside_band {
            Drag::CropEdge(ex, ey)
        } else if r.contains(pp) {
            Drag::CropMove
        } else {
            let cpt = r.center();
            Drag::CropRotate { start_angle: g.angle, start_ptr: (pp.y - cpt.y).atan2(pp.x - cpt.x) }
        };
    }
    if resp.dragged_by(egui::PointerButton::Primary) {
        let cn = info.screen_to_cropnorm(pp);
        let dd = resp.drag_delta();
        let dx = dd.x / ir.width();
        let dy = dd.y / ir.height();
        match d.drag {
            Drag::CropMove => {
                let w = c[2] - c[0];
                let h = c[3] - c[1];
                let nx = (c[0] + dx).clamp(0.0, 1.0 - w);
                let ny = (c[1] + dy).clamp(0.0, 1.0 - h);
                g.crop = [nx, ny, nx + w, ny + h];
                d.crop_intent = Some(g.crop);
            }
            Drag::CropEdge(ex, ey) => {
                let mut n = c;
                let min = 0.02;
                if ex < 0 {
                    n[0] = cn[0].clamp(0.0, n[2] - min);
                }
                if ex > 0 {
                    n[2] = cn[0].clamp(n[0] + min, 1.0);
                }
                if ey < 0 {
                    n[1] = cn[1].clamp(0.0, n[3] - min);
                }
                if ey > 0 {
                    n[3] = cn[1].clamp(n[1] + min, 1.0);
                }
                if let Some(a) = g.aspect {
                    // Keep aspect: derive the other axis from the moved one (relative to the fixed opposite side)
                    let w = (n[2] - n[0]) * fw as f32;
                    let h = (n[3] - n[1]) * fh as f32;
                    if ex != 0 && (ey == 0 || w / a > h) {
                        let nh = w / a / fh as f32;
                        if ey < 0 { n[1] = n[3] - nh } else { n[3] = n[1] + nh }
                    } else {
                        let nw = h * a / fw as f32;
                        if ex < 0 { n[0] = n[2] - nw } else { n[2] = n[0] + nw }
                    }
                    if n[0] < 0.0 || n[1] < 0.0 || n[2] > 1.0 || n[3] > 1.0 {
                        n = c;
                    }
                }
                g.crop = n;
                d.crop_intent = Some(n);
            }
            Drag::CropRotate { start_angle, start_ptr } => {
                let cpt = r.center();
                let a = (pp.y - cpt.y).atan2(pp.x - cpt.x);
                g.angle = (start_angle + (a - start_ptr).to_degrees()).clamp(-45.0, 45.0);
                let want = *d.crop_intent.get_or_insert(c);
                g.crop = fit_rotated(want, g, info.bw, info.bh, &info.lens);
            }
            _ => {}
        }
        e.changed = true;
        e.active = true;
        *lab = tr!("자르기", "Crop").into();
    } else if resp.hovered() {
        let on_rot = rot_handles.iter().any(|h| (pp - *h).length() < 14.0);
        // Grabbable edge/corner (same rule as the drag-start hit test)
        let near = |a: f32, b: f32| (a - b).abs() < GRAB;
        let hx = if near(pp.x, r.left()) { -1 } else if near(pp.x, r.right()) { 1 } else { 0 };
        let hy = if near(pp.y, r.top()) { -1 } else if near(pp.y, r.bottom()) { 1 } else { 0 };
        let on_frame = (hx != 0 || hy != 0) && r.expand(GRAB).contains(pp);
        ui.ctx().set_cursor_icon(if ui.input(|i| i.modifiers.command || i.modifiers.ctrl) {
            egui::CursorIcon::Crosshair
        } else if on_rot {
            egui::CursorIcon::Grab
        } else if on_frame {
            match (hx, hy) {
                (0, _) => egui::CursorIcon::ResizeVertical,
                (_, 0) => egui::CursorIcon::ResizeHorizontal,
                (x, y) if x == y => egui::CursorIcon::ResizeNwSe,
                _ => egui::CursorIcon::ResizeNeSw,
            }
        } else if r.contains(pp) {
            egui::CursorIcon::Move
        } else {
            egui::CursorIcon::Alias
        });
    }
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        if let Drag::Straighten(a) = d.drag {
            if (pp - a).length() > 12.0
                && let Some(na) = straighten_angle(info, g, a, pp) {
                    g.angle = na.clamp(-45.0, 45.0);
                    let want = *d.crop_intent.get_or_insert(g.crop);
                    g.crop = fit_rotated(want, g, info.bw, info.bh, &info.lens);
                    e.changed = true;
                    *lab = trf!("수평 맞추기 {:+.1}°", "Straighten {:+.1}°", g.angle);
                }
        } else {
            *lab = tr!("자르기", "Crop").into();
        }
        d.drag = Drag::None;
        e.committed = true;
    }
    if resp.double_clicked() {
        d.tool = Tool::None;
        e.committed = true;
        *lab = tr!("자르기", "Crop").into();
    }
}

/// Detect faces in the source and add them as redaction regions. Returns the number found
pub fn detect_faces_into(src: &crate::develop::image::SourceImage, s: &mut DevelopSettings, kind: PrivacyKind, strength: f32) -> Result<usize, String> {
    let regs = crate::imaging::faces::auto_regions(src, kind, strength)?;
    // Replace previous auto regions (manual regions are kept)
    s.privacy.retain(|r| !r.auto);
    let n = regs.len();
    s.privacy.extend(regs);
    Ok(n)
}

fn privacy_panel(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) {
    let mut done = false;
    let mut clear = false;
    section(ui, tr!("가리기", "Privacy"), true, |ui| {
        ui.label(egui::RichText::new(tr!("얼굴·번호판 등을 모자이크·흐림·채우기로 덮습니다 (원본은 그대로, 내보낼 때 적용)", "Covers faces, license plates etc. with mosaic·blur·fill (original untouched, applied on export)")).size(11.0).color(TEXT_WEAK()));
        ui.add_space(4.0);
        let kind = d.settings.privacy.last().map(|r| r.kind).unwrap_or_default();
        let strength = d.settings.privacy.last().map(|r| r.strength).unwrap_or(50.0);
        if widgets::button_row(ui, &[(tr!("얼굴 자동 감지", "Auto-detect faces"), true, tr!("Windows 내장 얼굴 감지 (인터넷·외부 모델 사용 안 함)", "Windows built-in face detection (no internet or external models)"))]).is_some()
            && let Some(src) = d.viewer.source.clone() {
                match detect_faces_into(&src, &mut d.settings, kind, strength) {
                    Ok(n) => {
                        d.privacy_msg = if n == 0 { tr!("얼굴을 찾지 못했습니다 — 드래그로 직접 추가하세요", "No faces found — add them by dragging").into() } else { trf!("얼굴 {n}개를 가렸습니다", "Covered {n} faces") };
                        e.changed = true;
                        e.committed = true;
                        *lab = tr!("얼굴 자동 가리기", "Auto face privacy").into();
                    }
                    Err(m) => d.privacy_msg = m,
                }
            }
        if !d.privacy_msg.is_empty() {
            ui.label(egui::RichText::new(&d.privacy_msg).size(11.0).color(TEXT_WEAK()));
        }
        widgets::help_box(ui, "privacy", &[(tr!("빈 곳 끌기", "Drag empty area"), tr!("새 영역", "New region")), (tr!("영역 안 끌기", "Drag inside region"), tr!("이동", "Move")), (tr!("가장자리 끌기", "Drag edge"), tr!("크기", "Size")), (tr!("Delete·우클릭", "Delete·right-click"), tr!("삭제", "Delete"))]);
    });
    let n = d.settings.privacy.len();
    if n > 0 {
        section(ui, &trf!("영역 {n}개", "{n} regions"), true, |ui| {
            let mut remove = None;
            for i in 0..n {
                let sel = d.privacy_sel == Some(i);
                let r = &d.settings.privacy[i];
                let name = format!("{} {}", if r.auto { tr!("얼굴", "Face") } else { tr!("영역", "Region") }, i + 1);
                let resp = widgets::list_row(ui, sel, &name, TEXT());
                if resp.clicked() {
                    d.privacy_sel = Some(i);
                }
                resp.context_menu(|ui| {
                    if ui.button(tr!("삭제", "Delete")).clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }
            if let Some(i) = remove {
                d.settings.privacy.remove(i);
                d.privacy_sel = None;
                e.changed = true;
                e.committed = true;
                *lab = tr!("가리기 영역 삭제", "Delete privacy region").into();
            }
            let idx = d.privacy_sel.filter(|i| *i < d.settings.privacy.len()).unwrap_or(d.settings.privacy.len().saturating_sub(1));
            if let Some(r) = d.settings.privacy.get_mut(idx) {
                ui.add_space(6.0);
                theme_label(ui, &trf!("{}번 방식", "Method {}", idx + 1));
                let kinds: Vec<&str> = PrivacyKind::ALL.iter().map(|k| k.name()).collect();
                if let Some(k) = widgets::toggle_row(ui, &kinds, PrivacyKind::ALL.iter().position(|k| *k == r.kind), "")
                    && r.kind != PrivacyKind::ALL[k] {
                        r.kind = PrivacyKind::ALL[k];
                        e.changed = true;
                        e.committed = true;
                        *lab = trf!("가리기: {}", "Privacy: {}", r.kind.name());
                    }
                if let Some(k) = widgets::toggle_row(ui, &[tr!("타원", "Ellipse"), tr!("사각형", "Rectangle")], Some(if r.ellipse { 0 } else { 1 }), "") {
                    let el = k == 0;
                    if el != r.ellipse {
                        r.ellipse = el;
                        e.changed = true;
                        e.committed = true;
                        *lab = tr!("가리기 모양", "Privacy shape").into();
                    }
                }
                if r.kind != PrivacyKind::Fill {
                    let rr = slider(ui, tr!("세기", "Strength"), &mut r.strength, 1.0, 100.0, 50.0, 0, "priv_str");
                    if rr.changed || rr.committed {
                        *lab = tr!("가리기 세기", "Privacy strength").into();
                    }
                    e.merge(rr);
                }
                if n > 1 && widgets::button_row(ui, &[(tr!("모든 영역에 같은 방식", "Same method for all regions"), true, "")]).is_some() {
                    let (k, st, el) = (r.kind, r.strength, r.ellipse);
                    for x in &mut d.settings.privacy {
                        x.kind = k;
                        x.strength = st;
                        x.ellipse = el;
                    }
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("가리기 설정 통일", "Unify privacy settings").into();
                }
            }
        });
    }
    match widgets::action_bar(ui, &[(tr!("모두 지우기", "Clear all"), n > 0, "")], (tr!("완료", "Done"), true, "")) {
        widgets::Act::Primary => done = true,
        widgets::Act::Secondary(_) => clear = true,
        widgets::Act::None => {}
    }
    if clear {
        d.settings.privacy.clear();
        d.privacy_sel = None;
        e.changed = true;
        e.committed = true;
        *lab = tr!("가리기 모두 지우기", "Clear all privacy regions").into();
    }
    if done {
        d.tool = Tool::None;
    }
}

fn privacy_tool(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    let p = ui.painter_at(info.canvas);
    // Region overlay
    for (i, r) in d.settings.privacy.iter().enumerate() {
        let sel = d.privacy_sel == Some(i);
        let col = if sel { ACCENT } else { Color32::WHITE };
        let pts = if r.ellipse {
            ellipse_points(info, r.center, r.radius, 0.0, 1.0)
        } else {
            let c = |dx: f32, dy: f32| info.base_to_screen([r.center[0] + dx * r.radius[0], r.center[1] + dy * r.radius[1]]);
            vec![c(-1.0, -1.0), c(1.0, -1.0), c(1.0, 1.0), c(-1.0, 1.0), c(-1.0, -1.0)]
        };
        p.add(egui::Shape::line(pts, Stroke::new(if sel { 2.0 } else { 1.2 }, col)));
    }
    let hit = |regs: &[PrivacyRegion], pos: Pos2| -> Option<(usize, bool)> {
        let b = info.screen_to_base(pos);
        for (i, r) in regs.iter().enumerate().rev() {
            let dx = (b[0] - r.center[0]) / r.radius[0].max(1e-4);
            let dy = (b[1] - r.center[1]) / r.radius[1].max(1e-4);
            let dd = if r.ellipse { (dx * dx + dy * dy).sqrt() } else { dx.abs().max(dy.abs()) };
            // Grabbable up to GRAB screen points outside
            let rs = (r.radius[0] * bw).max(r.radius[1] * bh) * info.image_rect.width() / bw.max(1.0);
            let tol = 1.0 + GRAB / rs.max(8.0);
            if dd <= tol {
                return Some((i, dd > 1.0 - (GRAB * 0.6) / rs.max(8.0)));
            }
        }
        None
    };
    if let Some(hp) = resp.hover_pos() {
        match hit(&d.settings.privacy, hp) {
            Some((_, true)) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe),
            Some((_, false)) => ui.ctx().set_cursor_icon(egui::CursorIcon::Move),
            None => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
        }
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(pp) = press_pos(ui, resp) {
            match hit(&d.settings.privacy, pp) {
                Some((i, edge)) => {
                    d.privacy_sel = Some(i);
                    d.privacy_drag = if edge { 2 } else { 1 };
                }
                None => {
                    let b = info.screen_to_base(pp);
                    let kind = d.settings.privacy.last().map(|r| r.kind).unwrap_or_default();
                    let strength = d.settings.privacy.last().map(|r| r.strength).unwrap_or(50.0);
                    d.settings.privacy.push(PrivacyRegion { center: b, radius: [0.005, 0.005 * bw / bh], ellipse: true, kind, strength, auto: false });
                    d.privacy_sel = Some(d.settings.privacy.len() - 1);
                    d.privacy_drag = 3;
                    d.privacy_anchor = b;
                }
            }
        }
    if resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(i), Some(pp)) = (d.privacy_sel, resp.interact_pointer_pos()) {
            let b = info.screen_to_base(pp);
            let anchor = d.privacy_anchor;
            if let Some(r) = d.settings.privacy.get_mut(i) {
                match d.privacy_drag {
                    1 => {
                        let pb = info.screen_to_base(pp - resp.drag_delta());
                        r.center[0] += b[0] - pb[0];
                        r.center[1] += b[1] - pb[1];
                    }
                    2 => {
                        r.radius[0] = (b[0] - r.center[0]).abs().max(0.004);
                        r.radius[1] = (b[1] - r.center[1]).abs().max(0.004);
                    }
                    3 => {
                        r.center = [(anchor[0] + b[0]) * 0.5, (anchor[1] + b[1]) * 0.5];
                        r.radius = [((b[0] - anchor[0]) * 0.5).abs().max(0.004), ((b[1] - anchor[1]) * 0.5).abs().max(0.004)];
                    }
                    _ => {}
                }
                e.changed = true;
                e.active = true;
                *lab = tr!("가리기 영역", "Privacy region").into();
            }
        }
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        d.privacy_drag = 0;
        e.committed = true;
    }
    if resp.clicked()
        && let Some(pp) = resp.interact_pointer_pos() {
            d.privacy_sel = hit(&d.settings.privacy, pp).map(|h| h.0);
        }
    if let Some(i) = d.privacy_sel {
        let _ = i; // Deletion is handled by the Delete shortcut
    }
}

fn redeye_panel(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) {
    section(ui, tr!("적목 현상 제거", "Red Eye Removal"), true, |ui| {
        ui.label(egui::RichText::new(tr!("플래시로 붉게 찍힌 눈동자를 자연스러운 어두운 색으로", "Turns flash-red pupils into a natural dark color")).size(11.0).color(TEXT_WEAK()));
        widgets::help_box(ui, "redeye", &[(tr!("눈 감싸 끌기·클릭", "Drag around an eye · click"), tr!("추가", "Add")), (tr!("영역 안 끌기", "Drag inside region"), tr!("이동", "Move")), (tr!("가장자리 끌기", "Drag edge"), tr!("크기", "Size")), (tr!("Delete·우클릭", "Delete·right-click"), tr!("삭제", "Delete"))]);
    });
    let n = d.settings.red_eye.len();
    if n > 0 {
        section(ui, &trf!("눈 {n}개", "{n} eyes"), true, |ui| {
            let mut remove = None;
            for i in 0..n {
                let sel = d.redeye_sel == Some(i);
                let r = widgets::list_row(ui, sel, &trf!("눈 {}", "Eye {}", i + 1), TEXT());
                if r.clicked() {
                    d.redeye_sel = Some(i);
                }
                r.context_menu(|ui| {
                    if ui.button(tr!("삭제", "Delete")).clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }
            if let Some(i) = remove {
                d.settings.red_eye.remove(i);
                d.redeye_sel = None;
                e.changed = true;
                e.committed = true;
                *lab = tr!("적목 영역 삭제", "Delete red eye area").into();
            }
            let idx = d.redeye_sel.filter(|i| *i < d.settings.red_eye.len()).unwrap_or(d.settings.red_eye.len().saturating_sub(1));
            if let Some(r) = d.settings.red_eye.get_mut(idx) {
                ui.add_space(6.0);
                theme_label(ui, &trf!("눈 {} 설정", "Eye {} settings", idx + 1));
                let rr = slider(ui, tr!("눈동자 크기", "Pupil size"), &mut r.pupil, 0.0, 100.0, 50.0, 0, "re_pupil");
                if rr.changed || rr.committed {
                    *lab = tr!("적목: 눈동자 크기", "Red eye: pupil size").into();
                }
                e.merge(rr);
                let rr = slider(ui, tr!("어둡게", "Darken"), &mut r.darken, 0.0, 100.0, 50.0, 0, "re_dark");
                if rr.changed || rr.committed {
                    *lab = tr!("적목: 어둡게", "Red eye: darken").into();
                }
                e.merge(rr);
            }
        });
    }
    match widgets::action_bar(ui, &[(tr!("모두 지우기", "Clear all"), n > 0, "")], (tr!("완료", "Done"), true, "")) {
        widgets::Act::Primary => d.tool = Tool::None,
        widgets::Act::Secondary(_) => {
            d.settings.red_eye.clear();
            d.redeye_sel = None;
            e.changed = true;
            e.committed = true;
            *lab = tr!("적목 모두 지우기", "Clear all red eye").into();
        }
        widgets::Act::None => {}
    }
}

fn redeye_tool(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    use crate::develop::settings::RedEye;
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    let p = ui.painter_at(info.canvas);
    for (i, r) in d.settings.red_eye.iter().enumerate() {
        let sel = d.redeye_sel == Some(i);
        let pts = ellipse_points(info, r.center, r.radius, 0.0, 1.0);
        p.add(egui::Shape::line(pts.clone(), Stroke::new(3.0, Color32::from_black_alpha(140))));
        p.add(egui::Shape::line(pts, Stroke::new(if sel { 1.8 } else { 1.1 }, if sel { ACCENT } else { Color32::WHITE })));
    }
    let hit = |regs: &[RedEye], pos: Pos2| -> Option<(usize, bool)> {
        let b = info.screen_to_base(pos);
        for (i, r) in regs.iter().enumerate().rev() {
            let dx = (b[0] - r.center[0]) / r.radius[0].max(1e-4);
            let dy = (b[1] - r.center[1]) / r.radius[1].max(1e-4);
            let dd = (dx * dx + dy * dy).sqrt();
            let rs = (r.radius[0] * bw).max(r.radius[1] * bh) * info.image_rect.width() / bw.max(1.0);
            if dd <= 1.0 + GRAB / rs.max(8.0) {
                return Some((i, dd > 1.0 - (GRAB * 0.6) / rs.max(8.0)));
            }
        }
        None
    };
    if let Some(hp) = resp.hover_pos() {
        match hit(&d.settings.red_eye, hp) {
            Some((_, true)) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe),
            Some((_, false)) => ui.ctx().set_cursor_icon(egui::CursorIcon::Move),
            None => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
        }
    }
    let last = d.settings.red_eye.last().cloned().unwrap_or_default();
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(pp) = press_pos(ui, resp) {
            match hit(&d.settings.red_eye, pp) {
                Some((i, edge)) => {
                    d.redeye_sel = Some(i);
                    d.privacy_drag = if edge { 2 } else { 1 };
                }
                None => {
                    let b = info.screen_to_base(pp);
                    d.settings.red_eye.push(RedEye { center: b, radius: [0.003, 0.003 * bw / bh], ..last.clone() });
                    d.redeye_sel = Some(d.settings.red_eye.len() - 1);
                    d.privacy_drag = 3;
                    d.privacy_anchor = b;
                }
            }
        }
    if resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(i), Some(pp)) = (d.redeye_sel, resp.interact_pointer_pos()) {
            let b = info.screen_to_base(pp);
            let anchor = d.privacy_anchor;
            if let Some(r) = d.settings.red_eye.get_mut(i) {
                match d.privacy_drag {
                    1 => {
                        let pb = info.screen_to_base(pp - resp.drag_delta());
                        r.center[0] += b[0] - pb[0];
                        r.center[1] += b[1] - pb[1];
                    }
                    2 => {
                        r.radius[0] = (b[0] - r.center[0]).abs().max(0.002);
                        r.radius[1] = (b[1] - r.center[1]).abs().max(0.002);
                    }
                    3 => {
                        r.center = [(anchor[0] + b[0]) * 0.5, (anchor[1] + b[1]) * 0.5];
                        r.radius = [((b[0] - anchor[0]) * 0.5).abs().max(0.002), ((b[1] - anchor[1]) * 0.5).abs().max(0.002)];
                    }
                    _ => {}
                }
                e.changed = true;
                e.active = true;
                *lab = tr!("적목 영역", "Red eye area").into();
            }
        }
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        d.privacy_drag = 0;
        e.committed = true;
    }
    if resp.clicked()
        && let Some(pp) = resp.interact_pointer_pos() {
            match hit(&d.settings.red_eye, pp) {
                Some((i, _)) => d.redeye_sel = Some(i),
                None => {
                    // Click: add a circle about 40 screen points in size
                    let b = info.screen_to_base(pp);
                    let rr = 40.0 / info.image_rect.width().max(1.0);
                    d.settings.red_eye.push(RedEye { center: b, radius: [rr, rr * bw / bh], ..last });
                    d.redeye_sel = Some(d.settings.red_eye.len() - 1);
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("적목 제거", "Red eye removal").into();
                }
            }
        }
}

/// HSL targeted adjustment: adjust by vertical drag, weighted by the bands of the clicked color
fn hsl_tat(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, row: u8, e: &mut Edit, lab: &mut String) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    let weights = |c: [u8; 3]| -> ([f32; 8], usize) {
        let (h, chroma) = crate::develop::color::rgb_hue(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
        let mut w = [0.0f32; 8];
        let mut best = (f32::MAX, 0usize);
        for b in 0..8 {
            let d = ((h - HSL_BAND_HUES[b] + 540.0).rem_euclid(360.0) - 180.0).abs();
            if d < best.0 {
                best = (d, b);
            }
        }
        let b0 = best.1;
        let (bl, br) = ((b0 + 7) % 8, (b0 + 1) % 8);
        let dist = |a: f32, b: f32| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs();
        let b1 = if dist(h, HSL_BAND_HUES[bl]) < dist(h, HSL_BAND_HUES[br]) { bl } else { br };
        let span = dist(HSL_BAND_HUES[b0], HSL_BAND_HUES[b1]).max(1.0);
        let t = (dist(h, HSL_BAND_HUES[b0]) / span).clamp(0.0, 1.0);
        let k = (chroma * 4.0).clamp(0.2, 1.0);
        w[b0] = (1.0 - t) * k;
        w[b1] += t * k;
        (w, b0)
    };
    if let Some(hp) = resp.hover_pos()
        && let Some(c) = d.viewer.sample_output(hp) {
            let (_, b0) = weights(c);
            ui.painter().text(hp + vec2(14.0, 14.0), Align2::LEFT_TOP, trf!("{} · 위아래로 드래그", "{} · drag up/down", crate::i18n::t(HSL_BAND_NAMES[b0])), FontId::proportional(11.5), Color32::WHITE);
        }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(c) = resp.interact_pointer_pos().and_then(|p| d.viewer.sample_output(p)) {
            let (w, b0) = weights(c);
            d.tat_w = w;
            d.tat_band = crate::i18n::t(HSL_BAND_NAMES[b0]).to_string();
            d.hsl_band = b0;
        }
    if resp.dragged_by(egui::PointerButton::Primary) {
        let dv = -resp.drag_delta().y * if ui.input(|i| i.modifiers.shift) { 0.1 } else { 0.4 };
        if dv != 0.0 {
            for b in 0..8 {
                let w = d.tat_w[b];
                if w <= 0.0 {
                    continue;
                }
                let v = match row {
                    0 => &mut d.settings.hsl[b].hue,
                    1 => &mut d.settings.hsl[b].sat,
                    _ => &mut d.settings.hsl[b].lum,
                };
                *v = (*v + dv * w).clamp(-100.0, 100.0);
            }
            e.changed = true;
            e.active = true;
            *lab = format!("HSL {} {}", d.tat_band, [tr!("색조", "Hue"), tr!("채도", "Saturation"), tr!("광도", "Luminance")][row as usize]);
        }
    }
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        for b in &mut d.settings.hsl {
            b.hue = b.hue.round();
            b.sat = b.sat.round();
            b.lum = b.lum.round();
        }
        e.changed = true;
        e.committed = true;
    }
}

fn spot_panel(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) {
    section(ui, tr!("스팟 제거", "Spot Removal"), true, |ui| {
        ui.label(egui::RichText::new(tr!("클릭: 점 지우기 · 드래그: 칠한 모양대로 지우기", "Click: remove a spot · drag: remove the painted shape")).size(11.0).color(TEXT_WEAK()));
        ui.add_space(4.0);
        let sel = d.spot_sel.filter(|i| *i < d.settings.spots.len());
        // Edit the selected spot if any, otherwise the defaults for new spots
        let (target, is_sel): (&mut Spot, bool) = match sel {
            Some(i) => (&mut d.settings.spots[i], true),
            None => (&mut d.spot_new, false),
        };
        theme_label(ui, if is_sel { tr!("선택한 스팟", "Selected spot") } else { tr!("새 스팟", "New spot") });
        let cur = if target.remove { 2 } else if target.heal { 0 } else { 1 };
        if let Some(i) = widgets::toggle_row(ui, &[tr!("복구", "Heal"), tr!("복제", "Duplicate"), tr!("지우기 (AI)", "Remove (AI)")], Some(cur), tr!("복구: 주변 밝기·색에 맞춤 · 복제: 그대로 복사 · 지우기: 칠한 곳을 주변 내용으로 새로 채움(AI, 처음 한 번 모델을 받음)", "Heal: matches surrounding brightness·color · Clone: copies as-is · Remove: refills the painted area from surrounding content (AI, downloads the model once)"))
            && i != cur {
                target.remove = i == 2;
                target.heal = i == 0;
                if target.remove {
                    // Remove spots do not use a source position
                    target.src = target.dst;
                }
                if is_sel {
                    e.changed = true;
                    e.committed = true;
                    *lab = [tr!("스팟 복구", "Spot heal"), tr!("스팟 복제", "Spot clone"), tr!("지우기", "Remove")][i].into();
                }
            }
        if let Some(st) = &d.fill_status {
            ui.label(egui::RichText::new(st).size(11.0).color(TEXT_WEAK()));
        }
        ui.add_space(2.0);
        let mut size = target.radius * 1000.0;
        let r = widgets::slider_pow(ui, tr!("크기", "Size"), &mut size, 2.0, 150.0, 12.0, 0, "spot_size", 2.5);
        target.radius = size / 1000.0;
        let mut fe = target.feather * 100.0;
        let r2 = widgets::slider_pow(ui, tr!("페더", "Feather"), &mut fe, 0.0, 100.0, 50.0, 0, "spot_feather", 1.6);
        target.feather = fe / 100.0;
        let mut op = target.opacity * 100.0;
        let r3 = slider(ui, tr!("불투명도", "Opacity"), &mut op, 0.0, 100.0, 100.0, 0, "spot_opacity");
        target.opacity = op / 100.0;
        if is_sel {
            for rr in [r, r2, r3] {
                if rr.changed || rr.committed {
                    *lab = tr!("스팟 설정", "Spot settings").into();
                }
                e.merge(rr);
            }
        }
        let mut show = !d.spot_hide;
        if widgets::toggle_line(ui, tr!("영역 표시", "Show overlay"), &mut show, "H") {
            d.spot_hide = !show;
        }
        widgets::help_box(
            ui,
            "spot",
            &[
                (tr!("흰 테두리", "White outline"), tr!("고칠 곳 (끌어서 이동)", "Area to fix (drag to move)")),
                (tr!("회색", "Gray"), tr!("가져올 곳 (손잡이·Shift+끌기·방향키)", "Source (handle·Shift+drag·arrow keys)")),
                ("[ ]", tr!("크기", "Size")),
                ("/", tr!("가져올 곳 다시 찾기", "Find source again")),
                (tr!("Delete·우클릭", "Delete·right-click"), tr!("삭제", "Delete")),
                ("H", tr!("표시 끄기", "Hide overlay")),
            ],
        );
    });
    let n = d.settings.spots.len();
    if n > 0 {
        section(ui, &trf!("스팟 {n}개", "{n} spots"), true, |ui| {
            let has_sel = d.spot_sel.is_some();
            match widgets::button_row(ui, &[(tr!("가져올 곳 다시 찾기", "Find source again"), has_sel, tr!("선택한 스팟 (/)", "Selected spot (/)")), (tr!("선택 해제", "Deselect"), has_sel, "")]) {
                Some(0) => d.spot_refind = true,
                Some(_) => d.spot_sel = None,
                None => {}
            }
        });
    }
    match widgets::action_bar(ui, &[(tr!("모두 지우기", "Clear all"), n > 0, tr!("스팟 전부 삭제", "Delete all spots"))], (tr!("완료", "Done"), true, "")) {
        widgets::Act::Primary => d.tool = Tool::None,
        widgets::Act::Secondary(_) => {
            d.settings.spots.clear();
            d.spot_sel = None;
            e.changed = true;
            e.committed = true;
            *lab = tr!("스팟 모두 지우기", "Clear all spots").into();
        }
        widgets::Act::None => {}
    }
}

/// Distance to a spot (relative to the long edge): center for circles, path for brush spots
fn spot_dist(sp: &Spot, b: [f32; 2], bw: f32, bh: f32, src: bool) -> f32 {
    let long = bw.max(bh);
    let (ox, oy) = if src { (sp.src[0] - sp.dst[0], sp.src[1] - sp.dst[1]) } else { (0.0, 0.0) };
    let pt = |p: [f32; 2]| [(p[0] + ox) * bw, (p[1] + oy) * bh];
    let q = [b[0] * bw, b[1] * bh];
    if sp.path.len() < 2 {
        let c = pt(sp.dst);
        return (q[0] - c[0]).hypot(q[1] - c[1]) / long;
    }
    let mut best = f32::MAX;
    for w in sp.path.windows(2) {
        let (a, c) = (pt(w[0]), pt(w[1]));
        let (ex, ey) = (c[0] - a[0], c[1] - a[1]);
        let t = (((q[0] - a[0]) * ex + (q[1] - a[1]) * ey) / (ex * ex + ey * ey).max(1e-6)).clamp(0.0, 1.0);
        best = best.min((q[0] - a[0] - ex * t).hypot(q[1] - a[1] - ey * t));
    }
    best / long
}

fn spot_tool(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    let long = bw.max(bh);
    let p = ui.painter_at(info.canvas);
    let px_per_long = info.long_edge_screen();
    // Overlay: rasterize the union of painted paths and draw only the outer outline (overlapping strokes merge)
    if !d.spot_hide || !d.spot_paint.is_empty() {
        let to_screen = |sp: &Spot, src: bool| -> (Vec<Pos2>, f32) {
            let (ox, oy) = if src { (sp.src[0] - sp.dst[0], sp.src[1] - sp.dst[1]) } else { (0.0, 0.0) };
            let pts: Vec<Pos2> = if sp.path.len() >= 2 { sp.path.iter().map(|q| info.base_to_screen([q[0] + ox, q[1] + oy])).collect() } else { vec![info.base_to_screen([sp.dst[0] + ox, sp.dst[1] + oy])] };
            (pts, (sp.radius * px_per_long).max(2.0))
        };
        // Layers: 0 = other spots (white), 1 = selected (highlight), 2 = selected source (gray), 3 = being painted (white)
        let mut layers: Vec<(u8, Vec<Pos2>, f32)> = Vec::new();
        if !d.spot_hide {
            for (i, sp) in d.settings.spots.iter().enumerate() {
                let sel = d.spot_sel == Some(i);
                let (pts, r) = to_screen(sp, false);
                layers.push((if sel { 1 } else { 0 }, pts, r));
                // Remove spots have no source position
                if sel && !sp.remove {
                    let (pts, r) = to_screen(sp, true);
                    layers.push((2, pts, r));
                }
            }
        }
        if !d.spot_paint.is_empty() {
            let pts: Vec<Pos2> = d.spot_paint.iter().map(|q| info.base_to_screen(*q)).collect();
            layers.push((3, pts, (d.spot_new.radius * px_per_long).max(2.0)));
        }
        if !layers.is_empty() {
            let tex = spot_overlay_texture(ui.ctx(), &mut d.spot_overlay, info.canvas, &layers);
            p.image(tex, info.canvas, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // Selected spot: source handle plus source -> target arrow
        if let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get(i)).filter(|_| !d.spot_hide) {
            let a = info.base_to_screen(sp.src);
            let b = info.base_to_screen(sp.dst);
            if a.distance(b) > 8.0 {
                let dir = (b - a).normalized();
                let tip = b - dir * (sp.radius * px_per_long).min(a.distance(b) * 0.5);
                p.line_segment([a, tip], Stroke::new(3.0, Color32::from_black_alpha(120)));
                p.line_segment([a, tip], Stroke::new(1.2, Color32::WHITE));
                let n = vec2(-dir.y, dir.x) * 5.0;
                p.add(egui::Shape::convex_polygon(vec![tip, tip - dir * 9.0 + n, tip - dir * 9.0 - n], Color32::WHITE, Stroke::new(1.0, Color32::from_black_alpha(140))));
            }
            // Source handle (drag to move the source)
            let hot = resp.hover_pos().map(|h| h.distance(a) <= GRAB).unwrap_or(false) || d.spot_drag == 2;
            p.circle_filled(a, if hot { 8.0 } else { 6.5 }, Color32::from_black_alpha(150));
            p.circle_filled(a, if hot { 6.5 } else { 5.0 }, Color32::from_gray(200));
            p.circle_stroke(a, if hot { 6.5 } else { 5.0 }, Stroke::new(1.2, Color32::WHITE));
        }
    }
    // Size shortcuts
    let r_now = d.spot_sel.and_then(|i| d.settings.spots.get(i)).map(|s| s.radius).unwrap_or(d.spot_new.radius);
    let (dec, inc, hide, refind) = ui.input(|i| (i.key_pressed(Key::OpenBracket), i.key_pressed(Key::CloseBracket), i.key_pressed(Key::H), i.key_pressed(Key::Slash)));
    let typing = ui.ctx().egui_wants_keyboard_input();
    if !typing && (dec || inc) {
        let nr = (r_now * if inc { 1.15 } else { 1.0 / 1.15 }).clamp(0.002, 0.15);
        match d.spot_sel.and_then(|i| d.settings.spots.get_mut(i)) {
            Some(sp) => {
                sp.radius = nr;
                e.changed = true;
                e.committed = true;
                *lab = tr!("스팟 크기", "Spot size").into();
            }
            None => d.spot_new.radius = nr,
        }
    }
    if !typing && hide {
        d.spot_hide = !d.spot_hide;
    }
    if (!typing && refind) || d.spot_refind {
        d.spot_refind = false;
        if let (Some(i), Some(src)) = (d.spot_sel, d.viewer.source.clone())
            && let Some(sp) = d.settings.spots.get_mut(i) {
                let off = spot_auto_offset(&src, sp, true);
                sp.src = [sp.dst[0] + off[0], sp.dst[1] + off[1]];
                e.changed = true;
                e.committed = true;
                *lab = tr!("스팟 원본 다시 찾기", "Find spot source again").into();
            }
    }
    // Brush size preview at the mouse (double line, visible on bright areas too)
    if let Some(hp) = resp.hover_pos() {
        let over_src = d.spot_sel.and_then(|i| d.settings.spots.get(i)).map(|sp| info.base_to_screen(sp.src).distance(hp) <= GRAB).unwrap_or(false);
        if d.spot_drag == 1 || d.spot_drag == 2 || over_src || (ui.input(|i| i.modifiers.shift) && d.spot_sel.is_some()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        } else {
            let rr = if d.spot_sel.is_some() && d.spot_drag == 0 { d.spot_new.radius } else { r_now };
            brush_cursor(&p, hp, rr * px_per_long, None, false);
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }
    }
    let hit = |spots: &[Spot], sel_now: Option<usize>, pos: Pos2| -> Option<(usize, u8)> {
        let b = info.screen_to_base(pos);
        // Test the selected spot's source handle/area first (so the source can be grabbed when overlapping), with a few screen points of slack
        if let Some(i) = sel_now
            && let Some(sp) = spots.get(i)
                && (info.base_to_screen(sp.src).distance(pos) <= GRAB || spot_dist(sp, b, bw, bh, true) <= sp.radius + 6.0 / px_per_long) {
                    return Some((i, 2));
                }
        // Target: nearest spot (radius + slack)
        spots
            .iter()
            .enumerate()
            .map(|(i, sp)| (i, spot_dist(sp, b, bw, bh, false) - sp.radius))
            .filter(|(_, d)| *d <= 8.0 / px_per_long)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| (i, 1))
    };
    let shift = ui.input(|i| i.modifiers.shift);
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(pp) = press_pos(ui, resp) {
            // Shift + drag: move the selected spot's source from anywhere
            let pick = if shift && d.spot_sel.is_some() {
                d.spot_sel.map(|i| (i, 2u8))
            } else if d.spot_hide {
                None
            } else {
                hit(&d.settings.spots, d.spot_sel, pp)
            };
            match pick {
                Some((i, part)) => {
                    d.spot_sel = Some(i);
                    d.spot_drag = part;
                }
                None => {
                    // Dragging empty space paints a new spot
                    d.spot_drag = 3;
                    d.spot_paint = vec![info.screen_to_base(pp)];
                }
            }
        }
    if resp.dragged_by(egui::PointerButton::Primary) && d.spot_drag > 0
        && let Some(pp) = resp.interact_pointer_pos() {
            let b = info.screen_to_base(pp);
            let pb = info.screen_to_base(pp - resp.drag_delta());
            let (ddx, ddy) = (b[0] - pb[0], b[1] - pb[1]);
            match d.spot_drag {
                1 | 2 => {
                    if let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get_mut(i)) {
                        if d.spot_drag == 1 {
                            sp.translate_dst(ddx, ddy);
                        } else {
                            sp.src[0] += ddx;
                            sp.src[1] += ddy;
                        }
                        e.changed = true;
                        e.active = true;
                        *lab = tr!("스팟 이동", "Move spot").into();
                    }
                }
                _ => {
                    // Add points every quarter radius (in source pixels)
                    let last = *d.spot_paint.last().unwrap_or(&b);
                    if ((b[0] - last[0]) * bw).hypot((b[1] - last[1]) * bh) > d.spot_new.radius * long * 0.25 {
                        d.spot_paint.push(b);
                    }
                }
            }
        }
    if resp.drag_stopped_by(egui::PointerButton::Primary) && d.spot_drag > 0 {
        if d.spot_drag == 3 {
            let path = std::mem::take(&mut d.spot_paint);
            let r = d.spot_new.radius;
            let extent = path.iter().map(|q| ((q[0] - path[0][0]) * bw).hypot((q[1] - path[0][1]) * bh)).fold(0.0f32, f32::max) / long;
            let mut sp = d.spot_new.clone();
            sp.dst = path[0];
            // A drag shorter than the radius makes a circular spot
            sp.path = if extent > r * 0.6 && path.len() >= 2 { path } else { Vec::new() };
            if sp.remove {
                sp.src = sp.dst;
                sp.fill = None;
            } else if let Some(src) = d.viewer.source.clone() {
                let off = spot_auto_offset(&src, &sp, false);
                sp.src = [sp.dst[0] + off[0], sp.dst[1] + off[1]];
            } else {
                sp.src = [sp.dst[0] + r * 2.5, sp.dst[1]];
            }
            let brush = !sp.path.is_empty();
            d.settings.spots.push(sp);
            d.spot_sel = Some(d.settings.spots.len() - 1);
            e.changed = true;
            *lab = if brush { tr!("스팟 브러시", "Spot brush").into() } else { tr!("스팟 제거", "Spot Removal").into() };
        }
        d.spot_drag = 0;
        e.committed = true;
    }
    if resp.clicked()
        && let Some(pp) = press_pos(ui, resp) {
            match if d.spot_hide { None } else { hit(&d.settings.spots, d.spot_sel, pp) } {
                Some((i, _)) => d.spot_sel = Some(i),
                None => {
                    let b = info.screen_to_base(pp);
                    let mut sp = d.spot_new.clone();
                    sp.dst = b;
                    sp.path.clear();
                    sp.fill = None;
                    let off = if sp.remove {
                        [0.0, 0.0]
                    } else {
                        match &d.viewer.source {
                            Some(src) => spot_auto_offset(src, &sp, false),
                            None => [sp.radius * 2.5, 0.0],
                        }
                    };
                    sp.src = [b[0] + off[0], b[1] + off[1]];
                    d.settings.spots.push(sp);
                    d.spot_sel = Some(d.settings.spots.len() - 1);
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("스팟 제거", "Spot Removal").into();
                }
            }
        }
}

/// Spot union outline texture (screen pixel resolution). Fills per layer and colors only the borders
/// Layers: 0 = other spots (white), 1 = selected (highlight), 2 = source (gray), 3 = being painted (white)
fn spot_overlay_texture(ctx: &egui::Context, cache: &mut Option<(u64, egui::TextureHandle)>, canvas: Rect, layers: &[(u8, Vec<Pos2>, f32)]) -> egui::TextureId {
    use std::hash::{Hash, Hasher};
    // Actual screen pixel resolution (crisp 1-pixel outlines)
    let k = ctx.pixels_per_point().clamp(1.0, 2.0);
    let (w, h) = (((canvas.width() * k).ceil() as usize).max(1), ((canvas.height() * k).ceil() as usize).max(1));
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (w, h).hash(&mut hs);
    for (l, pts, r) in layers {
        l.hash(&mut hs);
        r.to_bits().hash(&mut hs);
        for q in pts {
            ((q.x - canvas.left()).to_bits(), (q.y - canvas.top()).to_bits()).hash(&mut hs);
        }
    }
    let key = hs.finish();
    if let Some((kk, t)) = cache
        && *kk == key {
            return t.id();
        }
    // Per-layer bit mask (bit l)
    let mut m = vec![0u8; w * h];
    for (l, pts, r) in layers {
        let bit = 1u8 << l;
        let rr = r * k;
        let to = |q: Pos2| ((q.x - canvas.left()) * k, (q.y - canvas.top()) * k);
        let mut mark_capsule = |a: (f32, f32), b: (f32, f32)| {
            let (x0, x1) = ((a.0.min(b.0) - rr).floor().max(0.0) as usize, ((a.0.max(b.0) + rr).ceil() as usize).min(w));
            let (y0, y1) = ((a.1.min(b.1) - rr).floor().max(0.0) as usize, ((a.1.max(b.1) + rr).ceil() as usize).min(h));
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let l2 = (ex * ex + ey * ey).max(1e-6);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let t = (((px - a.0) * ex + (py - a.1) * ey) / l2).clamp(0.0, 1.0);
                    let (dx, dy) = (px - a.0 - ex * t, py - a.1 - ey * t);
                    if dx * dx + dy * dy <= rr * rr {
                        m[y * w + x] |= bit;
                    }
                }
            }
        };
        if pts.len() == 1 {
            mark_capsule(to(pts[0]), to(pts[0]));
        } else {
            for s in pts.windows(2) {
                mark_capsule(to(s[0]), to(s[1]));
            }
        }
    }
    let mut img = egui::ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]);
    let edge_col = |l: u8| match l {
        1 => ACCENT,
        2 => Color32::from_gray(200),
        _ => Color32::WHITE,
    };
    for y in 0..h {
        for x in 0..w {
            let v = m[y * w + x];
            let nb = |dx: isize, dy: isize| -> u8 {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize { 0 } else { m[ny as usize * w + nx as usize] }
            };
            let around = [nb(-1, 0), nb(1, 0), nb(0, -1), nb(0, 1)];
            // Border (priority: selected > source > others)
            let mut px = Color32::TRANSPARENT;
            for l in [1u8, 2, 3, 0] {
                let bit = 1 << l;
                if v & bit != 0 && around.iter().any(|a| a & bit == 0) {
                    px = edge_col(l);
                    break;
                }
            }
            if px == Color32::TRANSPARENT {
                // Interior is not filled (so it does not cloud the healed result); only outline and outer halo
                if v == 0 && around.iter().any(|a| *a != 0) {
                    // Outer halo (keeps the outline visible on bright backgrounds)
                    px = Color32::from_black_alpha(130);
                }
            }
            img.pixels[y * w + x] = px;
        }
    }
    match cache {
        Some((kk, t)) => {
            t.set(img, egui::TextureOptions::LINEAR);
            *kk = key;
            t.id()
        }
        None => {
            let t = ctx.load_texture("spot_overlay", img, egui::TextureOptions::LINEAR);
            let id = t.id();
            *cache = Some((key, t));
            id
        }
    }
}

/// Auto-pick the source position (offset). `avoid_current`: choose a candidate different from the current source (re-pick)
fn spot_auto_offset(src: &crate::develop::image::SourceImage, sp: &Spot, avoid_current: bool) -> [f32; 2] {
    let cur = [sp.src[0] - sp.dst[0], sp.src[1] - sp.dst[1]];
    let off = if sp.path.len() >= 2 {
        crate::develop::pipeline::auto_stroke_offset(src, &sp.path, sp.radius)
    } else {
        let s = crate::develop::pipeline::auto_spot_source(src, sp.dst, sp.radius);
        [s[0] - sp.dst[0], s[1] - sp.dst[1]]
    };
    if avoid_current && (off[0] - cur[0]).hypot(off[1] - cur[1]) < sp.radius * 0.5 {
        // If it is the same spot, use the opposite direction
        [-cur[0], -cur[1]]
    } else {
        off
    }
}

/// Find the edit target at a screen position (for the current tool)
fn canvas_hit(d: &DevState, info: &ViewInfo, pos: Option<Pos2>) -> CtxTarget {
    let Some(pos) = pos else { return CtxTarget::None };
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    let b = info.screen_to_base(pos);
    let px_per_long = info.long_edge_screen().max(1.0);
    match d.tool {
        Tool::Spot => d
            .settings
            .spots
            .iter()
            .enumerate()
            .map(|(i, sp)| (i, spot_dist(sp, b, bw, bh, false) - sp.radius))
            .filter(|(_, dd)| *dd <= 10.0 / px_per_long)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| CtxTarget::Spot(i))
            .unwrap_or(CtxTarget::None),
        Tool::Privacy => d
            .settings
            .privacy
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| {
                let dx = (b[0] - r.center[0]) / r.radius[0].max(1e-4);
                let dy = (b[1] - r.center[1]) / r.radius[1].max(1e-4);
                let dd = if r.ellipse { (dx * dx + dy * dy).sqrt() } else { dx.abs().max(dy.abs()) };
                dd <= 1.25
            })
            .map(|(i, _)| CtxTarget::Privacy(i))
            .unwrap_or(CtxTarget::None),
        Tool::RedEye => d
            .settings
            .red_eye
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| {
                let dx = (b[0] - r.center[0]) / r.radius[0].max(1e-4);
                let dy = (b[1] - r.center[1]) / r.radius[1].max(1e-4);
                (dx * dx + dy * dy).sqrt() <= 1.25
            })
            .map(|(i, _)| CtxTarget::RedEye(i))
            .unwrap_or(CtxTarget::None),
        Tool::Mask => {
            // Another mask's pin -> that mask, otherwise the selected mask
            for (i, m) in d.settings.masks.iter().enumerate() {
                let pin = m.components.first().and_then(|c| match &c.shape {
                    MaskShape::Radial { center, .. } => Some(*center),
                    MaskShape::Linear { p0, p1 } => Some([(p0[0] + p1[0]) * 0.5, (p0[1] + p1[1]) * 0.5]),
                    _ => None,
                });
                if let Some(pb) = pin
                    && info.base_to_screen(pb).distance(pos) <= GRAB {
                        return CtxTarget::Mask(i);
                    }
            }
            d.mask_sel.filter(|i| *i < d.settings.masks.len()).map(CtxTarget::Mask).unwrap_or(CtxTarget::None)
        }
        _ => CtxTarget::None,
    }
}

/// Right-click menu items (per target). Returns true if any item was drawn
fn canvas_menu(d: &mut DevState, ui: &mut Ui, e: &mut Edit, lab: &mut String) -> bool {
    let commit = |e: &mut Edit, lab: &mut String, l: &str| {
        e.changed = true;
        e.committed = true;
        *lab = l.into();
    };
    match d.ctx_target {
        CtxTarget::Spot(i) if i < d.settings.spots.len() => {
            ui.label(egui::RichText::new(if d.settings.spots[i].path.is_empty() { tr!("스팟", "Spot") } else { tr!("브러시 스팟", "Brush spot") }).size(11.0).color(TEXT_WEAK()));
            let heal = d.settings.spots[i].heal;
            if ui.button(if heal { tr!("복제로 바꾸기", "Change to clone") } else { tr!("복구로 바꾸기", "Change to heal") }).clicked() {
                d.settings.spots[i].heal = !heal;
                commit(e, lab, if heal { tr!("스팟 복제", "Spot clone") } else { tr!("스팟 복구", "Spot heal") });
                ui.close();
            }
            if ui.button(tr!("원본 다시 찾기   /", "Find source again   /")).clicked() {
                d.spot_sel = Some(i);
                d.spot_refind = true;
                ui.close();
            }
            if ui.button(tr!("삭제   Delete", "Delete   Delete")).clicked() {
                d.settings.spots.remove(i);
                d.spot_sel = None;
                commit(e, lab, tr!("스팟 삭제", "Delete spot"));
                ui.close();
            }
            true
        }
        CtxTarget::RedEye(i) if i < d.settings.red_eye.len() => {
            ui.label(egui::RichText::new(trf!("눈 {}", "Eye {}", i + 1)).size(11.0).color(TEXT_WEAK()));
            if ui.button(tr!("삭제   Delete", "Delete   Delete")).clicked() {
                d.settings.red_eye.remove(i);
                d.redeye_sel = None;
                commit(e, lab, tr!("적목 영역 삭제", "Delete red eye area"));
                ui.close();
            }
            true
        }
        CtxTarget::Privacy(i) if i < d.settings.privacy.len() => {
            ui.label(egui::RichText::new(tr!("가리기 영역", "Privacy region")).size(11.0).color(TEXT_WEAK()));
            for k in PrivacyKind::ALL {
                if ui.selectable_label(d.settings.privacy[i].kind == k, k.name()).clicked() {
                    d.settings.privacy[i].kind = k;
                    commit(e, lab, tr!("가리기 방식", "Privacy method"));
                    ui.close();
                }
            }
            let ell = d.settings.privacy[i].ellipse;
            if ui.button(if ell { tr!("사각형으로", "Make rectangle") } else { tr!("원으로", "Make circle") }).clicked() {
                d.settings.privacy[i].ellipse = !ell;
                commit(e, lab, tr!("가리기 모양", "Privacy shape"));
                ui.close();
            }
            if ui.button(tr!("삭제   Delete", "Delete   Delete")).clicked() {
                d.settings.privacy.remove(i);
                d.privacy_sel = None;
                commit(e, lab, tr!("가리기 삭제", "Delete privacy region"));
                ui.close();
            }
            true
        }
        CtxTarget::Mask(i) if i < d.settings.masks.len() => {
            ui.label(egui::RichText::new(d.settings.masks[i].name.clone()).size(11.0).color(TEXT_WEAK()));
            let vis = d.settings.masks[i].visible;
            if ui.button(if vis { tr!("숨기기", "Hide") } else { tr!("보이기", "Show") }).clicked() {
                d.settings.masks[i].visible = !vis;
                commit(e, lab, tr!("마스크 표시 전환", "Toggle mask visibility"));
                ui.close();
            }
            let inv = d.settings.masks[i].invert;
            if ui.button(if inv { tr!("반전 해제", "Uninvert") } else { tr!("반전", "Invert") }).clicked() {
                d.settings.masks[i].invert = !inv;
                commit(e, lab, tr!("마스크 반전", "Invert mask"));
                ui.close();
            }
            if ui.button(tr!("복제", "Duplicate")).clicked() {
                let mut m = d.settings.masks[i].clone();
                m.id = d.next_mask_id;
                d.next_mask_id += 1;
                m.name = trf!("{} 사본", "{} copy", m.name);
                d.settings.masks.push(m);
                d.mask_sel = Some(d.settings.masks.len() - 1);
                commit(e, lab, tr!("마스크 복제", "Duplicate mask"));
                ui.close();
            }
            let ci = d.comp_sel.unwrap_or(0);
            if d.settings.masks[i].components.len() > 1 && ci < d.settings.masks[i].components.len() && ui.button(tr!("선택한 구성 빼기", "Remove selected component")).clicked() {
                d.settings.masks[i].components.remove(ci);
                d.comp_sel = Some(0);
                commit(e, lab, tr!("구성 삭제", "Delete component"));
                ui.close();
            }
            if ui.button(tr!("마스크 삭제   Delete", "Delete mask   Delete")).clicked() {
                d.settings.masks.remove(i);
                d.mask_sel = if d.settings.masks.is_empty() { None } else { Some(i.min(d.settings.masks.len() - 1)) };
                d.comp_sel = d.mask_sel.map(|_| 0);
                commit(e, lab, tr!("마스크 삭제", "Delete mask"));
                ui.close();
            }
            true
        }
        _ => false,
    }
}

/// Handle grab radius (screen points), forgiving slightly off clicks
const GRAB: f32 = 18.0;

/// Drag hit-test position: where the button was pressed (egui starts a drag only after a few pixels of movement, so its position would miss)
fn press_pos(ui: &Ui, resp: &egui::Response) -> Option<Pos2> {
    ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
}

/// Index of the nearest handle within the radius
fn nearest_handle(pp: Pos2, hs: &[Pos2], r: f32) -> Option<usize> {
    hs.iter().enumerate().map(|(i, h)| (i, h.distance(pp))).filter(|(_, d)| *d <= r).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
}

/// Pan by dragging empty space while a tool is active (only when zoomed)
fn pan_by(center: &mut [f32; 2], info: &ViewInfo, delta: egui::Vec2, zoomed: bool) {
    if !zoomed {
        return;
    }
    center[0] -= delta.x / info.image_rect.width();
    center[1] -= delta.y / info.image_rect.height();
}

fn ellipse_points(info: &ViewInfo, center: [f32; 2], radius: [f32; 2], angle: f32, k: f32) -> Vec<Pos2> {
    let (s, c) = angle.to_radians().sin_cos();
    let (bw, bh) = (info.bw as f32, info.bh as f32);
    (0..=48)
        .map(|i| {
            let t = i as f32 / 48.0 * std::f32::consts::TAU;
            let lx = t.cos() * radius[0] * bw * k;
            let ly = t.sin() * radius[1] * bh * k;
            let x = center[0] * bw + c * lx - s * ly;
            let y = center[1] * bh + s * lx + c * ly;
            info.base_to_screen([x / bw, y / bh])
        })
        .collect()
}

/// In draw-pending state, create a linear/radial mask by dragging (preview while drawing)
fn mask_draw(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    let Some((kind, op)) = d.mask_arm else { return };
    let p = ui.painter_at(info.canvas);
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    if let (Some(hp), None) = (resp.hover_pos(), d.mask_arm_start) {
        let t = if kind == 1 { tr!("드래그: 시작 → 끝", "Drag: start → end") } else { tr!("드래그: 중심 → 크기 (Shift 정원)", "Drag: center → size (Shift: circle)") };
        let g = ui.painter().layout_no_wrap(t.to_string(), FontId::proportional(11.5), Color32::WHITE);
        let r = Rect::from_min_size(hp + vec2(16.0, 16.0), g.size() + vec2(12.0, 6.0));
        p.rect_filled(r, 4.0, Color32::from_black_alpha(170));
        p.galley(r.min + vec2(6.0, 3.0), g, Color32::WHITE);
    }
    let shape_from = |a: [f32; 2], b: [f32; 2], shift: bool| -> MaskShape {
        if kind == 1 {
            MaskShape::Linear { p0: a, p1: b }
        } else {
            let (bw, bh) = (info.bw as f32, info.bh as f32);
            let (mut rx, mut ry) = ((b[0] - a[0]).abs().max(0.005), (b[1] - a[1]).abs().max(0.005));
            if shift {
                let m = (rx * bw).max(ry * bh);
                rx = m / bw;
                ry = m / bh;
            }
            MaskShape::Radial { center: a, radius: [rx, ry], angle: 0.0, feather: 50.0 }
        }
    };
    if resp.drag_started_by(egui::PointerButton::Primary) {
        d.mask_arm_start = press_pos(ui, resp).map(|pp| info.screen_to_base(pp));
    }
    let shift = ui.input(|i| i.modifiers.shift);
    if let (Some(a), Some(pp)) = (d.mask_arm_start, resp.interact_pointer_pos()) {
        let b = info.screen_to_base(pp);
        // Preview outline
        match shape_from(a, b, shift) {
            MaskShape::Linear { p0, p1 } => {
                let (s0, s1) = (info.base_to_screen(p0), info.base_to_screen(p1));
                let dir = (s1 - s0).normalized();
                let perp = vec2(-dir.y, dir.x) * 3000.0;
                for (q, al) in [(s0, 255u8), (s1, 140)] {
                    p.line_segment([q - perp, q + perp], Stroke::new(3.0, Color32::from_black_alpha(120)));
                    p.line_segment([q - perp, q + perp], Stroke::new(1.2, Color32::from_white_alpha(al)));
                }
            }
            MaskShape::Radial { center, radius, angle, .. } => {
                let pts = ellipse_points(info, center, radius, angle, 1.0);
                p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(3.0, Color32::from_black_alpha(120))));
                p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, Color32::WHITE)));
            }
            _ => {}
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            let shape = shape_from(a, b, shift);
            d.mask_arm = None;
            d.mask_arm_start = None;
            d.mask_fresh = false;
            match (op, d.mask_sel.filter(|i| *i < d.settings.masks.len())) {
                (Some(op), Some(mi)) => {
                    let m = &mut d.settings.masks[mi];
                    m.components.push(MaskComponent { op, invert: false, shape });
                    d.comp_sel = Some(m.components.len() - 1);
                    *lab = tr!("마스크 구성 추가", "Add mask component").into();
                }
                _ => {
                    let id = d.next_mask_id;
                    d.next_mask_id += 1;
                    d.settings.masks.push(Mask {
                        id,
                        name: trf!("마스크 {}", "Mask {}", d.settings.masks.len() + 1),
                        components: vec![MaskComponent { op: MaskOp::Add, invert: false, shape }],
                        ..Default::default()
                    });
                    d.mask_sel = Some(d.settings.masks.len() - 1);
                    d.comp_sel = Some(0);
                    *lab = if kind == 1 { tr!("선형 그라디언트", "Linear Gradient").into() } else { tr!("방사형 그라디언트", "Radial Gradient").into() };
                }
            }
            d.overlay = true;
            e.changed = true;
            e.committed = true;
        }
    }
}

/// Brush cursor: black rim + white line double stroke (visible on light and dark areas), inner circle = feather start, center dot
fn brush_cursor(p: &egui::Painter, c: Pos2, r: f32, inner: Option<f32>, erase: bool) {
    let r = r.max(2.0);
    p.circle_stroke(c, r, Stroke::new(3.2, Color32::from_black_alpha(150)));
    p.circle_stroke(c, r, Stroke::new(1.4, if erase { ACCENT } else { Color32::WHITE }));
    if let Some(ri) = inner.filter(|ri| *ri > 2.0 && *ri < r - 1.5) {
        p.circle_stroke(c, ri, Stroke::new(2.2, Color32::from_black_alpha(90)));
        p.circle_stroke(c, ri, Stroke::new(1.0, Color32::from_white_alpha(150)));
    }
    p.circle_filled(c, 1.8, Color32::from_black_alpha(160));
    p.circle_filled(c, 1.0, Color32::WHITE);
    if erase {
        p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], Stroke::new(1.4, ACCENT));
    }
}

/// Double-stroke closed outline (black rim + colored line)
fn outline2(p: &egui::Painter, pts: Vec<Pos2>, col: Color32, width: f32) {
    p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(width + 2.0, Color32::from_black_alpha(140))));
    p.add(egui::Shape::closed_line(pts, Stroke::new(width, col)));
}

fn mask_tool(d: &mut DevState, ui: &mut Ui, resp: &egui::Response, info: &ViewInfo, e: &mut Edit, lab: &mut String) {
    if d.mask_arm.is_some() {
        mask_draw(d, ui, resp, info, e, lab);
        return;
    }
    let Some(mi) = d.mask_sel else { return };
    if mi >= d.settings.masks.len() {
        return;
    }
    let ci = d.comp_sel.unwrap_or(0);
    let p = ui.painter_at(info.canvas);
    let hp = resp.hover_pos();
    let alt = ui.input(|i| i.modifiers.alt);
    let brush = d.brush;
    let comps_len = d.settings.masks[mi].components.len();
    if ci >= comps_len {
        return;
    }
    // Pins of other masks
    for (i, m) in d.settings.masks.iter().enumerate() {
        if i == mi {
            continue;
        }
        if let Some(c) = m.components.first() {
            let pin = match &c.shape {
                MaskShape::Radial { center, .. } => Some(*center),
                MaskShape::Linear { p0, p1 } => Some([(p0[0] + p1[0]) * 0.5, (p0[1] + p1[1]) * 0.5]),
                _ => None,
            };
            if let Some(b) = pin {
                let sp = info.base_to_screen(b);
                p.circle_filled(sp, 5.0, Color32::from_gray(140));
                p.circle_stroke(sp, 5.0, Stroke::new(1.0, Color32::BLACK));
                let pr = ui.interact(Rect::from_center_size(sp, vec2(GRAB * 1.4, GRAB * 1.4)), egui::Id::new(("pin", i)), Sense::click());
                if pr.clicked() {
                    d.mask_sel = Some(i);
                    d.comp_sel = Some(0);
                    return;
                }
            }
        }
    }
    let shape = &mut d.settings.masks[mi].components[ci].shape;
    let handle = |p: &egui::Painter, pos: Pos2, filled: bool| {
        p.circle_filled(pos, 6.0, if filled { Color32::WHITE } else { Color32::from_black_alpha(120) });
        p.circle_stroke(pos, 6.0, Stroke::new(1.5, Color32::WHITE));
    };
    match shape {
        MaskShape::Brush { strokes } => {
            if let Some(h) = hp {
                let r = brush.size * info.long_edge_screen();
                brush_cursor(&p, h, r, Some(r * (1.0 - brush.feather)), brush.erase || alt);
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
            if resp.drag_started_by(egui::PointerButton::Primary)
                && let Some(pp) = press_pos(ui, resp) {
                    let b = info.screen_to_base(pp);
                    strokes.push(BrushStroke { points: vec![b, b], size: brush.size, feather: brush.feather, flow: brush.flow, erase: brush.erase || alt });
                    d.drag = Drag::Painting;
                }
            if resp.dragged_by(egui::PointerButton::Primary) && d.drag == Drag::Painting
                && let (Some(pp), Some(s)) = (resp.interact_pointer_pos(), strokes.last_mut()) {
                    let b = info.screen_to_base(pp);
                    let last = *s.points.last().unwrap();
                    let dist = ((b[0] - last[0]) * info.bw as f32).hypot((b[1] - last[1]) * info.bh as f32);
                    // Point spacing: 1/8 of the brush size (in source pixels)
                    if dist > brush.size * info.bw.max(info.bh) as f32 * 0.125 {
                        s.points.push(b);
                        e.changed = true;
                    }
                    e.active = true;
                }
            if resp.clicked()
                && let Some(pp) = resp.interact_pointer_pos() {
                    let b = info.screen_to_base(pp);
                    strokes.push(BrushStroke { points: vec![b, b], size: brush.size, feather: brush.feather, flow: brush.flow, erase: brush.erase || alt });
                    e.changed = true;
                    e.committed = true;
                    *lab = tr!("브러시", "Brush").into();
                }
            if resp.drag_stopped_by(egui::PointerButton::Primary) && d.drag == Drag::Painting {
                d.drag = Drag::None;
                e.changed = true;
                e.committed = true;
                *lab = if brush.erase || alt { tr!("브러시 지우기", "Erase brush").into() } else { tr!("브러시", "Brush").into() };
            }
        }
        MaskShape::Linear { p0, p1 } => {
            let s0 = info.base_to_screen(*p0);
            let s1 = info.base_to_screen(*p1);
            let dir = (s1 - s0).normalized();
            let perp = vec2(-dir.y, dir.x) * 3000.0;
            let clip = info.canvas;
            let pc = ui.painter_at(clip);
            for (q, al) in [(s0, 255u8), (s1, 150)] {
                pc.line_segment([q - perp, q + perp], Stroke::new(3.0, Color32::from_black_alpha(110)));
                pc.line_segment([q - perp, q + perp], Stroke::new(1.2, Color32::from_white_alpha(al)));
            }
            let mid = s0 + (s1 - s0) * 0.5;
            pc.line_segment([s0, s1], Stroke::new(1.0, Color32::from_white_alpha(80)));
            handle(&p, s0, true);
            handle(&p, s1, false);
            handle(&p, mid, false);
            if resp.drag_started_by(egui::PointerButton::Primary)
                && let Some(pp) = press_pos(ui, resp) {
                    d.drag = match nearest_handle(pp, &[s0, s1, mid], GRAB) {
                        Some(0) => Drag::LinP0,
                        Some(1) => Drag::LinP1,
                        Some(_) => Drag::LinMove,
                        None => Drag::None,
                    };
                    d.drag = if d.drag != Drag::None {
                        d.drag
                    } else if d.mask_fresh {
                        Drag::NewLinear(info.screen_to_base(pp))
                    } else {
                        Drag::Pan
                    };
                }
            if resp.dragged_by(egui::PointerButton::Primary)
                && let Some(pp) = resp.interact_pointer_pos() {
                    let b = info.screen_to_base(pp);
                    match d.drag {
                        Drag::LinP0 => *p0 = b,
                        Drag::LinP1 => *p1 = b,
                        Drag::LinMove => {
                            let bm = info.screen_to_base(mid);
                            let dv = [b[0] - bm[0], b[1] - bm[1]];
                            *p0 = [p0[0] + dv[0], p0[1] + dv[1]];
                            *p1 = [p1[0] + dv[0], p1[1] + dv[1]];
                        }
                        Drag::NewLinear(start) => {
                            *p0 = start;
                            *p1 = b;
                        }
                        _ => {}
                    }
                    if d.drag == Drag::Pan {
                        pan_by(&mut d.viewer.center, info, resp.drag_delta(), d.viewer.zoom.is_some());
                    } else {
                        e.changed = true;
                        e.active = true;
                    }
                }
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                if d.drag != Drag::Pan {
                    e.committed = true;
                    *lab = tr!("선형 그라디언트", "Linear Gradient").into();
                }
                if matches!(d.drag, Drag::NewLinear(_)) {
                    d.mask_fresh = false;
                }
                d.drag = Drag::None;
            }
        }
        MaskShape::Radial { center, radius, angle, feather } => {
            let pts = ellipse_points(info, *center, *radius, *angle, 1.0);
            outline2(&ui.painter_at(info.canvas), pts.clone(), Color32::WHITE, 1.2);
            let inner = ellipse_points(info, *center, *radius, *angle, 1.0 - *feather / 100.0);
            ui.painter_at(info.canvas).add(egui::Shape::closed_line(inner, Stroke::new(1.0, Color32::from_white_alpha(110))));
            let sc = info.base_to_screen(*center);
            let hx = pts[0];
            let hy = pts[12];
            let hr = pts[24];
            handle(&p, sc, true);
            handle(&p, hx, false);
            handle(&p, hy, false);
            p.circle_stroke(hr, 5.0, Stroke::new(1.0, Color32::from_white_alpha(160)));
            if resp.drag_started_by(egui::PointerButton::Primary)
                && let Some(pp) = press_pos(ui, resp) {
                    let inside = {
                        let b = info.screen_to_base(pp);
                        let (bw, bh) = (info.bw as f32, info.bh as f32);
                        let (sn, cs) = angle.to_radians().sin_cos();
                        let (dx, dy) = ((b[0] - center[0]) * bw, (b[1] - center[1]) * bh);
                        let (lx, ly) = (cs * dx + sn * dy, -sn * dx + cs * dy);
                        (lx / (radius[0] * bw).max(1e-3)).powi(2) + (ly / (radius[1] * bh).max(1e-3)).powi(2) < 0.6
                    };
                    d.drag = match nearest_handle(pp, &[sc, hx, hy, hr], GRAB) {
                        Some(0) => Drag::RadCenter,
                        Some(1) => Drag::RadX,
                        Some(2) => Drag::RadY,
                        Some(_) => Drag::RadRotate,
                        // Dragging the wide area inside the circle moves the whole shape
                        None if inside => Drag::RadCenter,
                        None => Drag::None,
                    };
                    d.drag = if d.drag != Drag::None {
                        d.drag
                    } else if d.mask_fresh {
                        Drag::NewRadial(info.screen_to_base(pp))
                    } else {
                        Drag::Pan
                    };
                }
            if resp.dragged_by(egui::PointerButton::Primary)
                && let Some(pp) = resp.interact_pointer_pos() {
                    let b = info.screen_to_base(pp);
                    let (bw, bh) = (info.bw as f32, info.bh as f32);
                    let (s, co) = angle.to_radians().sin_cos();
                    let dxp = (b[0] - center[0]) * bw;
                    let dyp = (b[1] - center[1]) * bh;
                    let lx = co * dxp + s * dyp;
                    let ly = -s * dxp + co * dyp;
                    match d.drag {
                        Drag::RadCenter => {
                            let pb = info.screen_to_base(pp - resp.drag_delta());
                            center[0] += b[0] - pb[0];
                            center[1] += b[1] - pb[1];
                        }
                        Drag::RadX => radius[0] = (lx.abs() / bw).max(0.005),
                        Drag::RadY => radius[1] = (ly.abs() / bh).max(0.005),
                        Drag::RadRotate => {
                            *angle = (dyp.atan2(dxp).to_degrees() - 180.0).rem_euclid(360.0);
                        }
                        Drag::NewRadial(start) => {
                            *center = start;
                            *angle = 0.0;
                            radius[0] = ((b[0] - start[0]).abs()).max(0.005);
                            radius[1] = ((b[1] - start[1]).abs()).max(0.005);
                            if ui.input(|i| i.modifiers.shift) {
                                let m = (radius[0] * bw).max(radius[1] * bh);
                                radius[0] = m / bw;
                                radius[1] = m / bh;
                            }
                        }
                        _ => {}
                    }
                    if d.drag == Drag::Pan {
                        pan_by(&mut d.viewer.center, info, resp.drag_delta(), d.viewer.zoom.is_some());
                    } else {
                        e.changed = true;
                        e.active = true;
                    }
                }
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                if d.drag != Drag::Pan {
                    e.committed = true;
                    *lab = tr!("방사형 그라디언트", "Radial Gradient").into();
                }
                if matches!(d.drag, Drag::NewRadial(_)) {
                    d.mask_fresh = false;
                }
                d.drag = Drag::None;
            }
        }
        MaskShape::Color { hue, sat, .. } => {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if resp.clicked()
                && let Some(pp) = resp.interact_pointer_pos()
                    && let Some(c) = d.viewer.sample_output(pp) {
                        let (h, ch) = crate::develop::color::rgb_hue(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
                        *hue = h;
                        *sat = ch;
                        e.changed = true;
                        e.committed = true;
                        *lab = tr!("색상 범위 선택", "Pick color range").into();
                    }
        }
        _ => {}
    }
}

fn bottom_bar(app: &mut App, ui: &mut Ui, bar: Rect) {
    let ppp = ui.ctx().pixels_per_point();
    let mut ref_toggle = false;
    let ref_on = app.ref_view;
    let mut proof_toggle = false;
    let proof_on = app.proof.on;
    let Some(d) = &mut app.devst else { return };
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
        ui.painter().rect_filled(bar, 0.0, PANEL2());
        ui.horizontal_centered(|ui| {
            ui.add_space(8.0);
            ui.label(mono(d.viewer.zoom_label(ppp)).color(TEXT_WEAK()));
            for (lbl, z) in [(tr!("맞춤", "Fit"), None), ("50%", Some(0.5)), ("100%", Some(1.0)), ("200%", Some(2.0)), ("400%", Some(4.0))] {
                if ui.small_button(lbl).clicked() {
                    d.viewer.set_zoom(z);
                }
            }
            ui.separator();
            if ui.selectable_label(d.before_after == BeforeAfter::SideBySide, egui::RichText::new(tr!("이전|이후 (Y)", "Before|After (Y)")).monospace()).clicked() {
                d.before_after = if d.before_after == BeforeAfter::SideBySide { BeforeAfter::Off } else { BeforeAfter::SideBySide };
            }
            if ui.selectable_label(d.before_after == BeforeAfter::Split, egui::RichText::new(tr!("분할 (Shift+Y)", "Split (Shift+Y)")).monospace()).clicked() {
                d.before_after = if d.before_after == BeforeAfter::Split { BeforeAfter::Off } else { BeforeAfter::Split };
            }
            if ui.selectable_label(d.before_after == BeforeAfter::Before, egui::RichText::new(tr!("이전 (\\)", "Before (\\)")).monospace()).clicked() {
                d.before_after = if d.before_after == BeforeAfter::Before { BeforeAfter::Off } else { BeforeAfter::Before };
            }
            ui.separator();
            if ui.selectable_label(ref_on, egui::RichText::new(tr!("참조 (Shift+R)", "Reference (Shift+R)")).monospace()).on_hover_text(tr!("다른 사진을 왼쪽에 고정해 두고 색·톤을 맞춥니다", "Pin another photo on the left to match color and tone")).clicked() {
                ref_toggle = true;
            }
            if ui.selectable_label(proof_on, egui::RichText::new(tr!("교정 (S)", "Proof (S)")).monospace()).on_hover_text(tr!("프린터 프로필로 인쇄 결과를 미리 봅니다 (소프트 교정)", "Preview print output with a printer profile (soft proofing)")).clicked() {
                proof_toggle = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if d.viewer.loading {
                    ui.spinner();
                    ui.label(egui::RichText::new(tr!("원본 디코딩 중", "Decoding original")).color(TEXT_WEAK()));
                }
                if let Some(err) = &d.viewer.error {
                    ui.label(egui::RichText::new(err).color(ACCENT));
                }
            });
        });
    });
    if ref_toggle {
        toggle_reference(app);
    }
    if proof_toggle {
        super::proof_ui::toggle(app);
    }
}

#[cfg(test)]
mod crop_tests {
    use super::*;

    /// Toggling landscape/portrait repeatedly must not shrink the box
    #[test]
    fn orientation_toggle_keeps_size() {
        let (fw, fh) = (6000.0f32, 4000.0f32);
        let mut c = [0.1f32, 0.1, 0.9, 0.9];
        c = reshape_crop(c, 1.5, fw, fh);
        let area0 = (c[2] - c[0]) * (c[3] - c[1]);
        for k in 0..20 {
            let a = if k % 2 == 0 { 1.0 / 1.5 } else { 1.5 };
            c = reshape_crop(c, a, fw, fh);
        }
        let area1 = (c[2] - c[0]) * (c[3] - c[1]);
        assert!(area1 > area0 * 0.6, "{area0} -> {area1}");
        let ratio = (c[2] - c[0]) * fw / ((c[3] - c[1]) * fh);
        assert!((ratio - 1.5).abs() < 0.01, "{ratio}");
    }

    /// Rotating after a small crop must not grow the box to the whole photo (only shrink if needed)
    #[test]
    fn rotate_keeps_small_crop() {
        let small = [0.4f32, 0.4, 0.6, 0.55];
        let g = Geometry { angle: 8.0, crop: small, ..Default::default() };
        let lens = crate::develop::settings::LensCorrection::default();
        let c = fit_rotated(small, &g, 6000, 4000, &lens);
        assert!((c[2] - c[0]) <= 0.2 + 1e-4 && (c[3] - c[1]) <= 0.15 + 1e-4, "{c:?}");
        assert!((c[2] - c[0]) > 0.19, "작은 상자는 회전해도 그대로여야 함: {c:?}");
        // A large box shrinks so no empty area is visible
        let big = [0.0f32, 0.0, 1.0, 1.0];
        let c2 = fit_rotated(big, &Geometry { angle: 8.0, crop: big, ..Default::default() }, 6000, 4000, &lens);
        assert!(c2[2] - c2[0] < 0.95, "{c2:?}");
    }
}
