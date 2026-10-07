//! Enhance dialog: AI denoise and AI 2x super-resolution.
//! Preview in the dialog -> background processing -> new file next to the original, stacked with it, develop settings copied.
//!
//! Preview: the left (original) side uses the same viewer as Develop (smooth panning, wheel zoom); the right (result) side
//! runs AI only on the visible region once movement stops, and shows the original while moving.

use super::app::App;
use super::theme::*;
use super::viewer::Viewer;
use crate::catalog::PhotoId;
use crate::develop::image::{LinearImage, SourceImage};
use crate::imaging::ai::{self, Options};
use crossbeam_channel::Receiver;
use egui::{Color32, Rect, Sense, Ui, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Maximum region computed at once (source pixels); larger views compute only the center
const PREV_MAX: usize = 1024;
/// Delay after movement stops before computing
const SETTLE: Duration = Duration::from_millis(300);
/// Preview viewer slot (render workers: 0 main, 1 compare, 2 detail 1:1, 3 enhance)
const SLOT: u8 = 3;

enum DlMsg {
    Progress(u64, u64, String),
    Done(Result<(), String>),
}

struct Download {
    rx: Receiver<DlMsg>,
    done: u64,
    total: u64,
    file: String,
}

/// A computed result tile: normalized source region + render for display
struct After {
    region: [f32; 4],
    tex: egui::TextureHandle,
    key: AfterKey,
}

/// Conditions that decide whether the result must be recomputed
#[derive(Clone, Copy, PartialEq, Debug)]
struct AfterKey {
    denoise: bool,
    superres: bool,
    amount: i32,
    /// Display scale (screen physical pixels per source pixel x100)
    zoom: i32,
}

type AfterOut = Result<(egui::ColorImage, [f32; 4], AfterKey, bool), String>;

pub struct EnhanceDialog {
    ids: Vec<PhotoId>,
    skipped: usize,
    pub opts: Options,
    view: Option<Viewer>,
    after: Option<After>,
    after_rx: Option<Receiver<AfterOut>>,
    gpu: bool,
    /// Last view state and when it changed (to detect when movement stops)
    last_view: ([f32; 2], Option<f32>),
    moved_at: Instant,
    msg: String,
    dl: Option<Download>,
}

enum JobMsg {
    Status(String, f32),
    Done(PhotoId, Result<ai::EnhanceResult, String>),
    Finished,
}

pub struct EnhanceJob {
    rx: Receiver<JobMsg>,
    pub status: String,
    pub frac: f32,
    pub done: usize,
    pub total: usize,
    pub cancel: Arc<AtomicBool>,
    opts: Options,
    last_new: Option<PhotoId>,
    failed: Vec<String>,
    secs: f32,
    gpu: bool,
}

/// Open the dialog for the selected RAW (excluding virtual copies and enhance results)
pub fn open(app: &mut App) {
    if app.enhance_job.is_some() {
        app.toast(tr!("이미 향상 작업 중입니다", "Already enhancing"));
        return;
    }
    let targets = app.multi_targets();
    let ids: Vec<PhotoId> = targets.iter().copied().filter(|id| app.cat.get(*id).map(|p| p.is_raw && p.master.is_none()).unwrap_or(false)).collect();
    if ids.is_empty() {
        app.toast(tr!("향상은 RAW 사진에만 쓸 수 있습니다", "Enhance works on RAW photos only"));
        return;
    }
    let opts: Options = app.cat.kv_get_json("enhance_opts").unwrap_or_default();
    let view = app.cat.get(ids[0]).map(|p| {
        let mut v = Viewer::new(ids[0], p.path.clone(), p.meta.orientation, SLOT, &app.dev);
        v.zoom = Some(1.0);
        // If it is the photo being developed, reuse the already-loaded source
        v.source = app.devst.as_ref().filter(|d| d.id == ids[0]).and_then(|d| d.viewer.source.clone());
        v
    });
    app.enhance = Some(EnhanceDialog {
        skipped: targets.len() - ids.len(),
        ids,
        opts,
        view,
        after: None,
        after_rx: None,
        gpu: false,
        last_view: ([0.5, 0.5], Some(1.0)),
        moved_at: Instant::now(),
        msg: String::new(),
        dl: None,
    });
}

impl EnhanceDialog {
    /// For event dispatch (source loaded, render results)
    pub fn viewer_mut(&mut self) -> Option<&mut Viewer> {
        self.view.as_mut()
    }
}

fn mb(b: u64) -> String {
    if b < 1_048_576 { tr!("1MB 미만", "under 1MB").into() } else { format!("{:.0}MB", b as f64 / 1_048_576.0) }
}

fn start_download(d: &mut EnhanceDialog) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let opts = d.opts;
    std::thread::Builder::new()
        .name("ai-download".into())
        .spawn(move || {
            let p = |a: u64, b: u64, f: &str| {
                let _ = tx.send(DlMsg::Progress(a, b, f.to_string()));
            };
            let r = ai::download(&opts, &p).map_err(|e| format!("{e:#}"));
            let _ = tx.send(DlMsg::Done(r));
        })
        .expect("download thread");
    d.dl = Some(Download { rx, done: 0, total: ai::missing(&opts).1, file: String::new() });
    d.msg.clear();
}

/// Develop settings for the preview: color and tone only, without position-dependent steps (crop, lens, vignette, masks, ...).
/// With crop removed, viewer coordinates equal normalized source coordinates (orientation applied)
fn preview_settings(s: &crate::develop::settings::DevelopSettings, after: bool, opts: &Options) -> crate::develop::settings::DevelopSettings {
    let mut s = s.clone();
    s.geometry = Default::default();
    s.lens = Default::default();
    s.effects = Default::default();
    s.masks.clear();
    s.spots.clear();
    s.privacy.clear();
    s.red_eye.clear();
    if after && opts.denoise {
        s.detail.nr_luma = 0.0;
        s.detail.nr_color = 0.0;
    }
    s
}

/// Center of the visible region (normalized source) clamped to PREV_MAX pixels -> source pixel rectangle
fn compute_rect(src: &SourceImage, vis: [f32; 4]) -> (usize, usize, usize, usize) {
    let (bw, bh) = (src.levels[0].w, src.levels[0].h);
    let x0 = (vis[0].clamp(0.0, 1.0) * bw as f32) as usize;
    let y0 = (vis[1].clamp(0.0, 1.0) * bh as f32) as usize;
    let x1 = ((vis[2].clamp(0.0, 1.0) * bw as f32).ceil() as usize).min(bw);
    let y1 = ((vis[3].clamp(0.0, 1.0) * bh as f32).ceil() as usize).min(bh);
    let (w, h) = ((x1.saturating_sub(x0)).max(8), (y1.saturating_sub(y0)).max(8));
    let (cw, ch) = (w.min(PREV_MAX), h.min(PREV_MAX));
    let cx = (x0 + (w - cw) / 2).min(bw.saturating_sub(cw));
    let cy = (y0 + (h - ch) / 2).min(bh.saturating_sub(ch));
    (cx, cy, cw.min(bw), ch.min(bh))
}

fn start_after(app: &App, d: &mut EnhanceDialog, vis: [f32; 4], zoom_px: f32, key: AfterKey) {
    let Some(src) = d.view.as_ref().and_then(|v| v.source.clone()) else { return };
    let Some(rc) = src.raw_color.clone() else { return };
    let settings = app.cat.get(d.ids[0]).map(|p| p.settings()).unwrap_or_default();
    let opts = d.opts;
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("ai-preview".into())
        .spawn(move || {
            let r = (|| -> anyhow::Result<_> {
                let (x0, y0, w, h) = compute_rect(&src, vis);
                let base = &src.levels[0];
                let mut data = Vec::with_capacity(w * h * 3);
                for y in 0..h {
                    data.extend_from_slice(&base.data[((y0 + y) * base.w + x0) * 3..((y0 + y) * base.w + x0 + w) * 3]);
                }
                let crop = LinearImage { w, h, data };
                let (out, gpu) = ai::process(&crop, rc.neutral, rc.baseline_exposure, &opts, &|_, _| {})?;
                let mut s = SourceImage::new(out, true, 64);
                s.raw_color = Some(rc.clone());
                let st = preview_settings(&settings, true, &opts);
                // Render at screen scale (zooming in is handled by the texture)
                let edge = ((w.max(h) as f32 * zoom_px.min(2.0)).round() as u32).clamp(64, 4096);
                let img = crate::imaging::decode::render_preview_with(&s, &st, edge);
                let ci = egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.data);
                let (bw, bh) = (base.w as f32, base.h as f32);
                let region = [x0 as f32 / bw, y0 as f32 / bh, (x0 + w) as f32 / bw, (y0 + h) as f32 / bh];
                Ok((ci, region, key, gpu))
            })();
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
        })
        .expect("preview thread");
    d.after_rx = Some(rx);
}

pub fn dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.enhance.take() else { return };
    // Download progress
    if let Some(dl) = &mut d.dl {
        let mut fin = None;
        for m in dl.rx.try_iter() {
            match m {
                DlMsg::Progress(a, b, f) => {
                    dl.done = a;
                    dl.total = b.max(1);
                    dl.file = f;
                }
                DlMsg::Done(r) => fin = Some(r),
            }
        }
        if let Some(r) = fin {
            d.dl = None;
            if let Err(e) = r {
                d.msg = trf!("받기 실패: {e}", "Download failed: {e}");
            }
        }
    }
    // Result tile arrived
    if let Some(rx) = &d.after_rx
        && let Ok(r) = rx.try_recv() {
            match r {
                Ok((ci, region, key, gpu)) => {
                    let tex = ctx.load_texture("enh_after", ci, egui::TextureOptions::LINEAR);
                    d.after = Some(After { region, tex, key });
                    d.gpu = gpu;
                    d.msg.clear();
                }
                Err(e) => d.msg = trf!("미리보기 실패: {e}", "Preview failed: {e}"),
            }
            d.after_rx = None;
        }
    let (missing, miss_bytes) = ai::missing(&d.opts);
    let ready = missing.is_empty();
    let settings = app.cat.get(d.ids[0]).map(|p| p.settings()).unwrap_or_default();
    let before_settings = preview_settings(&settings, false, &d.opts);

    let n = d.ids.len();
    let sub = if d.skipped > 0 { trf!("RAW {n}장 (RAW가 아닌 {}장은 제외)", "{n} RAW photos ({} non-RAW excluded)", d.skipped) } else if n == 1 { app.cat.get(d.ids[0]).map(|p| p.file_name.clone()).unwrap_or_default() } else { trf!("RAW {n}장", "{n} RAW photos") };
    let mut go = false;
    let mut close_f = false;
    let mut get = false;
    let busy_dl = d.dl.is_some();
    let (any, gpu) = (d.opts.any(), d.gpu);
    let has_after = d.after.is_some();
    let mut want: Option<([f32; 4], f32, AfterKey)> = None;
    let mut opts_changed = false;
    let (_, _, close_x) = super::form::modal(
        ctx,
        "enhance",
        tr!("향상", "Enhance"),
        &sub,
        vec2(900.0, 760.0),
        |ui| {
            // If required files are missing, show a download prompt in place of the preview (it cannot be computed anyway)
            if !ready && d.opts.any() {
                super::form::card(ui, tr!("처음 한 번 필요한 파일", "Required files (one time)"), tr!("Microsoft ONNX Runtime(DirectML, GPU 가속)과 AI 모델을 공식 배포처에서 받아 데이터 폴더에 둡니다.", "Downloads Microsoft ONNX Runtime (DirectML, GPU-accelerated) and AI models from their official sources into the data folder."), |ui| {
                    egui::Grid::new("enh_files").num_columns(2).spacing(vec2(24.0, 2.0)).show(ui, |ui| {
                        for a in &missing {
                            ui.label(mono(a.file).size(11.0).color(TEXT_WEAK()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(mono(mb(a.size)).size(11.0).color(TEXT_DIM())));
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    if let Some(dl) = &d.dl {
                        let f = dl.done as f32 / dl.total.max(1) as f32;
                        ui.add(egui::ProgressBar::new(f.min(1.0)).text(mono(format!("{} / {}  {}", mb(dl.done), mb(dl.total), dl.file))));
                    } else if super::form::primary(ui, &trf!("받기 (약 {})", "Download (about {})", mb(miss_bytes)), true).clicked() {
                        get = true;
                    }
                });
            } else {

            super::form::card(ui, tr!("미리보기", "Preview"), tr!("끌어서 이동 · 휠로 확대/축소 · 왼쪽 원본, 오른쪽 결과 (멈추면 계산)", "Drag to move · wheel to zoom · original left, result right (computes when you stop)"), |ui| {
                let gap = 8.0;
                let pw = ((ui.available_width() - gap) / 2.0).floor();
                let ph = (pw * 0.78).floor();
                let (row, _) = ui.allocate_exact_size(vec2(pw * 2.0 + gap, ph), Sense::hover());
                let lr = Rect::from_min_size(row.min, vec2(pw, ph));
                let rr = Rect::from_min_size(row.min + vec2(pw + gap, 0.0), vec2(pw, ph));
                let ppp = ui.ctx().pixels_per_point();
                ui.ctx().data_mut(|m| m.insert_temp(egui::Id::new("enh_left"), lr));
                let Some(v) = &mut d.view else { return };
                let (resp, info) = v.show(ui, lr, &app.dev, &before_settings, false, false, None, true);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
                }
                let p = ui.painter_at(rr);
                p.rect_filled(rr, 0.0, CANVAS());
                // Result side: draw the same source image, then the computed tiles at their positions
                v.paint_copy(&p, rr.min - lr.min);
                // Movement detection
                let cur = (v.center, v.zoom);
                if cur != d.last_view {
                    d.last_view = cur;
                    d.moved_at = Instant::now();
                }
                let moving = resp.dragged() || d.moved_at.elapsed() < SETTLE;
                let mut covered = false;
                if let (Some(info), Some(a)) = (&info, &d.after) {
                    let r = Rect::from_min_max(info.cropnorm_to_screen([a.region[0], a.region[1]]), info.cropnorm_to_screen([a.region[2], a.region[3]])).translate(rr.min - lr.min);
                    p.image(a.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    // Outline the computed area when only part is covered
                    if !r.contains_rect(rr.shrink(1.0)) {
                        p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_white_alpha(90)), egui::StrokeKind::Outside);
                    }
                    let vis = info.image_rect.intersect(info.canvas);
                    let vr = r.translate(lr.min - rr.min);
                    covered = vr.expand(1.0).contains_rect(vis);
                }
                // Compute when stopped and the visible area is not covered by results or the conditions changed
                if let Some(info) = &info {
                    let zoom_px = info.image_rect.width() * ppp / info.bw.max(1) as f32;
                    let key = AfterKey { denoise: d.opts.denoise, superres: d.opts.superres, amount: d.opts.amount.round() as i32, zoom: (zoom_px * 100.0).round() as i32 };
                    let stale = d.after.as_ref().map(|a| a.key != key).unwrap_or(true);
                    if ready && any && !moving && d.after_rx.is_none() && (stale || !covered) {
                        let vis = info.image_rect.intersect(info.canvas);
                        let a = info.screen_to_cropnorm(vis.min);
                        let b = info.screen_to_cropnorm(vis.max);
                        want = Some(([a[0], a[1], b[0], b[1]], zoom_px, key));
                    }
                }
                // Labels and status
                p.text(rr.left_top() + vec2(8.0, 7.0), egui::Align2::LEFT_TOP, tr!("향상", "Enhance"), egui::FontId::proportional(11.0), Color32::WHITE);
                ui.painter_at(lr).text(lr.left_top() + vec2(8.0, 7.0), egui::Align2::LEFT_TOP, tr!("원본", "Original"), egui::FontId::proportional(11.0), Color32::WHITE);
                let status = if !ready {
                    Some(tr!("필요한 파일을 받으면 결과가 나옵니다", "Results appear once the required files are downloaded"))
                } else if !any {
                    Some(tr!("향상 항목을 고르세요", "Choose what to enhance"))
                } else if moving {
                    None
                } else if d.after_rx.is_some() {
                    Some(tr!("계산 중…", "Computing…"))
                } else {
                    None
                };
                if let Some(t) = status {
                    let tr = Rect::from_center_size(rr.center_bottom() - vec2(0.0, 18.0), vec2(220.0, 22.0));
                    p.rect_filled(tr, 11.0, Color32::from_black_alpha(160));
                    p.text(tr.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional(11.5), Color32::WHITE);
                }
                // Zoom buttons
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ppp = ui.ctx().pixels_per_point();
                    for (label, z) in [(tr!("맞춤", "Fit"), None), ("50%", Some(0.5)), ("100%", Some(1.0)), ("200%", Some(2.0))] {
                        let sel = match (v.zoom, z) {
                            (None, None) => true,
                            (Some(a), Some(b)) => (a / ppp - b).abs() < 0.01 || (a - b).abs() < 0.01,
                            _ => false,
                        };
                        if ui.selectable_label(sel, label).clicked() {
                            v.set_zoom(z);
                        }
                    }
                    ui.label(egui::RichText::new(v.zoom_label(ppp)).size(11.0).color(TEXT_DIM()));
                });
            });
            }
            super::form::card(ui, tr!("향상", "Enhance"), "", |ui| {
                let mut ch = false;
                ch |= super::form::switch_row(ui, tr!("AI 노이즈 감소", "AI Denoise"), &mut d.opts.denoise, tr!("고감도 노이즈를 지우고 디테일은 살림 (NAFNet)", "Removes high-ISO noise while keeping detail (NAFNet)"));
                if d.opts.denoise {
                    super::form::row(ui, tr!("양", "Amount"), |ui| {
                        let r = ui.add(egui::Slider::new(&mut d.opts.amount, 0.0..=100.0).step_by(1.0).fixed_decimals(0));
                        // No computing while dragging; apply on release
                        if r.dragged() {
                            d.moved_at = Instant::now();
                        }
                        ch |= r.changed();
                    });
                }
                ch |= super::form::switch_row(ui, tr!("AI 해상도 2배", "AI Super Resolution 2×"), &mut d.opts.superres, tr!("가로·세로 2배 (화소 4배). 원본 질감을 유지하도록 보정 (Real-ESRGAN)", "2× width and height (4× pixels), corrected to keep the original texture (Real-ESRGAN)"));
                if ch {
                    opts_changed = true;
                }
                super::form::hint(ui, tr!("결과는 원본 옆 새 파일(…-Enhanced.drhdr)로 저장해 원본과 스택으로 묶고, 현상 설정을 복사합니다.", "The result is saved as a new file next to the original (…-Enhanced.drhdr), stacked with it, with develop settings copied."));
            });
            if !d.msg.is_empty() {
                ui.label(egui::RichText::new(&d.msg).size(11.5).color(ACCENT));
            }
        },
        |ui| {
            if ready && has_after {
                super::form::footer_note(ui, if gpu { tr!("GPU(DirectML)로 처리", "Process on GPU (DirectML)") } else { tr!("CPU로 처리 (GPU를 쓸 수 없음 — 느릴 수 있음)", "Process on CPU (GPU unavailable — may be slow)") });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if n > 1 { trf!("향상 ({n}장)", "Enhance ({n} photos)") } else { tr!("향상", "Enhance").to_string() };
                if super::form::primary(ui, &label, ready && any && !busy_dl).clicked() {
                    go = true;
                }
                if super::form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                    close_f = true;
                }
            });
        },
    );
    if opts_changed {
        app.cat.kv_set_json("enhance_opts", &d.opts);
    }
    if let Some((vis, zoom_px, key)) = want {
        start_after(app, &mut d, vis, zoom_px, key);
    }
    if d.after_rx.is_some() || d.dl.is_some() || d.moved_at.elapsed() < SETTLE * 2 {
        ctx.request_repaint_after(Duration::from_millis(60));
    }
    if get {
        start_download(&mut d);
    }
    if go {
        start_job(app, &d);
        return;
    }
    if close_x || close_f {
        if app.enhance_job.is_none() {
            // Release GPU memory on a separate thread (it may wait for a running preview computation)
            std::thread::spawn(ai::release);
        }
        return;
    }
    app.enhance = Some(d);
}

/// For automated checks: same as the dialog's Enhance button
pub fn start_for_test(app: &mut App, d: &EnhanceDialog) {
    start_job(app, d);
}

fn start_job(app: &mut App, d: &EnhanceDialog) {
    let items: Vec<(PhotoId, PathBuf, u16, PathBuf)> = d
        .ids
        .iter()
        .filter_map(|id| app.cat.get(*id).map(|p| (*id, p.path.clone(), p.meta.orientation, ai::out_path(&p.path, &d.opts))))
        .collect();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let opts = d.opts;
    let total = items.len();
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("ai-enhance".into())
        .spawn(move || {
            for (k, (id, path, o, out)) in items.into_iter().enumerate() {
                if c2.load(Ordering::Relaxed) {
                    break;
                }
                let prefix = if total > 1 { format!("{}/{} ", k + 1, total) } else { String::new() };
                let p = |m: &str, f: &f32| {
                    let _ = tx.send(JobMsg::Status(format!("{prefix}{m}"), (k as f32 + f) / total as f32));
                };
                let r = ai::enhance(&path, o, &opts, &out, &|m, f| p(m, &f)).map_err(|e| format!("{e:#}"));
                let _ = tx.send(JobMsg::Done(id, r));
            }
            ai::release();
            let _ = tx.send(JobMsg::Finished);
        })
        .expect("enhance thread");
    app.enhance_job = Some(EnhanceJob { rx, status: tr!("향상 준비", "Preparing enhance").into(), frac: 0.0, done: 0, total, cancel, opts, last_new: None, failed: Vec::new(), secs: 0.0, gpu: false });
}

/// Apply job progress (every frame)
pub fn pump(app: &mut App) {
    let Some(job) = &mut app.enhance_job else { return };
    let msgs: Vec<JobMsg> = job.rx.try_iter().collect();
    let mut finished = false;
    for m in msgs {
        match m {
            JobMsg::Status(s, f) => {
                if let Some(j) = &mut app.enhance_job {
                    j.status = s;
                    j.frac = f;
                }
            }
            JobMsg::Done(id, r) => {
                let opts = app.enhance_job.as_ref().map(|j| j.opts).unwrap_or_default();
                match r {
                    Ok(res) => {
                        let new = add_result(app, id, &res, &opts);
                        if let Some(j) = &mut app.enhance_job {
                            j.done += 1;
                            j.last_new = new.or(j.last_new);
                            j.secs += res.secs;
                            j.gpu |= res.gpu;
                        }
                    }
                    Err(e) => {
                        let name = app.cat.get(id).map(|p| p.file_name.clone()).unwrap_or_default();
                        if let Some(j) = &mut app.enhance_job {
                            j.done += 1;
                            j.failed.push(format!("{name}: {e}"));
                        }
                    }
                }
            }
            JobMsg::Finished => finished = true,
        }
    }
    if !finished {
        return;
    }
    let Some(job) = app.enhance_job.take() else { return };
    app.visible_dirty = true;
    app.recompute_visible();
    if job.total == 1
        && let Some(id) = job.last_new {
            app.select_single(id);
        }
    if job.failed.is_empty() {
        app.toast(trf!("향상 완료: {}장 ({}, {:.0}초)", "Enhance done: {} photos ({}, {:.0}s)", job.done, if job.gpu { "GPU" } else { "CPU" }, job.secs));
    } else {
        app.toast_err(trf!("향상 실패 {}건 — {}", "Enhance failed for {} — {}", job.failed.len(), job.failed.join(" / ")));
    }
}

/// Add the result to the catalog: copy metadata, keywords and develop settings; stack with the original (result on top)
fn add_result(app: &mut App, orig: PhotoId, res: &ai::EnhanceResult, opts: &Options) -> Option<PhotoId> {
    let p = app.cat.get(orig)?;
    let mut meta = p.meta.clone();
    meta.orientation = 1; // orientation already applied when saved
    meta.width = res.w as u32;
    meta.height = res.h as u32;
    let mut s = p.settings();
    if opts.denoise {
        // AI already removed the noise, so turn off the existing noise reduction
        s.detail.nr_luma = 0.0;
        s.detail.nr_color = 0.0;
    }
    let kw = p.keywords.clone();
    let size = std::fs::metadata(&res.path).map(|m| m.len()).unwrap_or(0);
    let import_id = app.cat.next_import_id();
    match app.cat.add_photos(&[(res.path.clone(), size, meta, Some(s), kw)], import_id) {
        Ok(ids) => {
            let new = *ids.first()?;
            if app.cat.stack(&[orig, new]).is_ok() {
                let _ = app.cat.set_stack_top(new);
            }
            Some(new)
        }
        Err(e) => {
            app.toast_err(trf!("카탈로그 추가 실패: {e}", "Couldn't add to catalog: {e}"));
            None
        }
    }
}

/// Status bar: progress bar + cancel
pub fn status(app: &App, ui: &mut Ui) -> bool {
    let Some(j) = &app.enhance_job else { return false };
    let mut cancel = false;
    if ui.small_button("×").on_hover_text(tr!("향상 취소 (지금 사진까지 처리)", "Cancel enhance (finishes the current photo)")).clicked() {
        cancel = true;
    }
    ui.add(egui::ProgressBar::new(j.frac.clamp(0.0, 1.0)).desired_width(220.0).text(mono(&j.status)));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    if cancel {
        j.cancel.store(true, Ordering::Relaxed);
    }
    cancel
}
