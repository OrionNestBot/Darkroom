//! Library 'People' view: detects faces in photos, groups them by person, and lets you name people and browse their photos.
//! Detection runs in the background (decoding at preview size); catalog writes happen on the UI thread.
//! © 2026 OrionNest

use super::app::*;
use super::theme::*;
use super::widgets;
use crate::catalog::PhotoId;
use crate::imaging::people::{self, FaceHit};
use egui::{Rect, Sense, Ui, pos2, vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub enum Msg {
    Status(String),
    Faces(PhotoId, Vec<FaceHit>),
    Failed(PhotoId),
    Done(Result<(), String>),
}

pub struct PeopleJob {
    rx: crossbeam_channel::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    pub done: usize,
    pub total: usize,
    pub status: String,
    found: usize,
}

#[derive(Default)]
pub struct PeopleUi {
    pub job: Option<PeopleJob>,
    /// Name editing: (person id, name being typed).
    pub rename: Option<(i64, String)>,
    tex: HashMap<i64, egui::TextureHandle>,
}

/// Source photos not yet scanned for faces.
pub fn unscanned(app: &App) -> Vec<(PhotoId, PathBuf, u16)> {
    app.cat
        .photos
        .iter()
        .filter(|p| p.master.is_none() && !p.missing && !app.cat.face_scanned.contains(&p.id))
        .map(|p| (p.id, p.path.clone(), p.meta.orientation))
        .collect()
}

pub fn start_scan(app: &mut App) {
    if app.people.job.is_some() {
        return;
    }
    let list = unscanned(app);
    if list.is_empty() {
        recluster(app);
        app.toast(tr!("새로 찾을 사진이 없습니다", "No new photos to scan"));
        return;
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let total = list.len();
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("people".into())
        .spawn(move || {
            let r = (|| -> anyhow::Result<()> {
                if !people::ready() {
                    let t2 = tx.clone();
                    people::download(&move |d, t, f| {
                        let _ = t2.send(Msg::Status(trf!("인물 모델 받는 중 {:.0}/{:.0}MB · {f}", "Downloading people model {:.0}/{:.0}MB · {f}", d as f64 / 1_048_576.0, t as f64 / 1_048_576.0)));
                    })?;
                }
                for (id, path, orient) in list {
                    if c2.load(Ordering::Relaxed) {
                        break;
                    }
                    let r = crate::imaging::decode::decode_preview(&path, crate::config::FACE_SCAN_EDGE, orient).and_then(|img| people::analyze(&img));
                    let _ = tx.send(match r {
                        Ok(f) => Msg::Faces(id, f),
                        Err(e) => {
                            eprintln!("얼굴 찾기 실패 {}: {e:#}", path.display());
                            Msg::Failed(id)
                        }
                    });
                }
                Ok(())
            })();
            people::release();
            let _ = tx.send(Msg::Done(r.map_err(|e| format!("{e:#}"))));
        })
        .expect("people thread");
    app.people.job = Some(PeopleJob { rx, cancel, done: 0, total, status: tr!("얼굴 찾는 중", "Finding faces").into(), found: 0 });
}

pub fn cancel(app: &mut App) {
    if let Some(j) = &app.people.job {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

/// Assign unclustered faces to existing or new people.
pub fn recluster(app: &mut App) -> usize {
    let input: Vec<(i64, &[f32], i64, f32)> = app.cat.faces.iter().filter(|f| f.person >= 0).map(|f| (f.id, f.emb.as_slice(), f.person, f.score)).collect();
    let ch = people::cluster(&input);
    if ch.is_empty() {
        return 0;
    }
    match app.cat.apply_face_groups(&ch) {
        Ok(n) => {
            app.visible_dirty = true;
            n
        }
        Err(e) => {
            app.toast_err(trf!("인물 묶기 실패: {e}", "Grouping people failed: {e}"));
            0
        }
    }
}

pub fn pump(app: &mut App) {
    let Some(job) = &mut app.people.job else { return };
    let mut faces = Vec::new();
    let mut done = None;
    for m in job.rx.try_iter() {
        match m {
            Msg::Status(s) => job.status = s,
            Msg::Faces(id, f) => {
                job.done += 1;
                job.found += f.len();
                job.status = trf!("얼굴 찾는 중 {}/{}", "Finding faces {}/{}", job.done, job.total);
                faces.push((id, f));
            }
            // Record unreadable photos as an empty result so they are not retried.
            Msg::Failed(id) => {
                job.done += 1;
                faces.push((id, Vec::new()));
            }
            Msg::Done(r) => done = Some(r),
        }
    }
    let found = job.found;
    if !faces.is_empty() {
        app.cat.begin_batch();
        for (id, f) in faces {
            if let Err(e) = app.cat.add_faces(id, &f) {
                app.toast_err(trf!("얼굴 기록 실패: {e}", "Couldn't save faces: {e}"));
            }
        }
        app.cat.end_batch();
    }
    let Some(r) = done else { return };
    app.people.job = None;
    match r {
        Ok(()) => {
            let n = recluster(app);
            app.toast(trf!("얼굴 {found}개를 찾았습니다{}", "Found {found} faces{}", if n > 0 { trf!(" · 새 인물 {n}명", " · {n} new people") } else { String::new() }));
        }
        Err(e) => app.toast_err(trf!("인물 찾기 실패: {e}", "Finding people failed: {e}")),
    }
    app.visible_dirty = true;
}

pub fn person_label(p: &crate::catalog::Person) -> String {
    if p.name.is_empty() { trf!("이름 없음 {}", "Unnamed {}", p.id) } else { p.name.clone() }
}

fn face_tex(app: &mut App, ctx: &egui::Context, face: i64) -> Option<egui::TextureHandle> {
    if let Some(t) = app.people.tex.get(&face) {
        return Some(t.clone());
    }
    let bytes = app.cat.face_thumb(face)?;
    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let t = ctx.load_texture(format!("face{face}"), egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &img.into_raw()), egui::TextureOptions::LINEAR);
    app.people.tex.insert(face, t.clone());
    Some(t)
}

/// Person row: round face crop, name and photo count.
fn person_row(app: &mut App, ui: &mut Ui, p: &crate::catalog::Person, n: usize) -> egui::Response {
    let src = Source::Person(p.id);
    let sel = app.source == src;
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let cover = app.cat.person_cover(p.id);
    let tex = cover.and_then(|f| face_tex(app, ui.ctx(), f));
    let painter = ui.painter();
    if sel {
        painter.rect_filled(rect, 4.0, ACTIVE());
        painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 9.0), vec2(2.5, 12.0)), 1.0, ACCENT);
    } else if r.hovered() {
        painter.rect_filled(rect, 4.0, HOVER());
    }
    let c = pos2(rect.left() + 21.0, rect.center().y);
    let rad = 11.5;
    match tex {
        Some(t) => {
            // Face inside a circle, drawn as a round mesh.
            let mut mesh = egui::Mesh::with_texture(t.id());
            let seg = 28;
            mesh.vertices.push(egui::epaint::Vertex { pos: c, uv: pos2(0.5, 0.5), color: egui::Color32::WHITE });
            for i in 0..=seg {
                let a = i as f32 / seg as f32 * std::f32::consts::TAU;
                let (s, co) = a.sin_cos();
                mesh.vertices.push(egui::epaint::Vertex { pos: c + vec2(co, s) * rad, uv: pos2(0.5 + co * 0.5, 0.5 + s * 0.5), color: egui::Color32::WHITE });
                if i > 0 {
                    mesh.indices.extend_from_slice(&[0, i as u32, i as u32 + 1]);
                }
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        None => {
            painter.circle_filled(c, rad, HOVER());
        }
    }
    painter.circle_stroke(c, rad, egui::Stroke::new(1.0, BORDER()));
    let cnt = painter.layout_no_wrap(n.to_string(), egui::FontId::monospace(10.0), TEXT_DIM());
    let cw = cnt.size().x;
    painter.galley(pos2(rect.right() - 6.0 - cw, rect.center().y - cnt.size().y * 0.5), cnt, TEXT_DIM());
    let col = if p.name.is_empty() { TEXT_WEAK() } else if sel { STRONG() } else { TEXT() };
    let g = painter.layout_no_wrap(person_label(p), egui::FontId::proportional(12.0), col);
    let clip = Rect::from_min_max(rect.min, pos2(rect.right() - cw - 12.0, rect.max.y));
    ui.painter_at(clip).galley(pos2(rect.left() + 40.0, rect.center().y - g.size().y * 0.5), g, col);
    if r.clicked() {
        app.set_source(src);
    }
    r
}

/// Contents of the left panel's 'People' section.
pub fn section(app: &mut App, ui: &mut Ui) {
    if let Some(j) = &app.people.job {
        let frac = if j.total > 0 { j.done as f32 / j.total as f32 } else { 0.0 };
        let status = j.status.clone();
        ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width()).text(mono(status).size(10.0)));
        if widgets::button_row(ui, &[(tr!("취소", "Cancel"), true, "")]).is_some() {
            cancel(app);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    let todo = if app.people.job.is_none() { unscanned(app).len() } else { 0 };
    if app.cat.face_scanned.is_empty() && app.people.job.is_none() {
        let dl = people::missing_bytes();
        let model = if dl > 0 { trf!("처음 한 번 모델({:.0}MB)을 받으며, ", "Downloads a model ({:.0}MB) the first time; ", dl as f64 / 1_048_576.0) } else { String::new() };
        ui.label(egui::RichText::new(trf!("사진 속 얼굴을 찾아 같은 사람끼리 묶습니다. {model}모든 처리는 이 PC 안에서만 합니다.", "Finds faces in your photos and groups the same person together. {model}All processing stays on this PC.")).size(11.0).color(TEXT_WEAK()));
        ui.add_space(4.0);
        if widgets::button_row(ui, &[(&trf!("인물 찾기 ({todo}장)", "Find people ({todo} photos)"), todo > 0, "")]).is_some() {
            start_scan(app);
        }
        return;
    }
    // Named people first, then by photo count.
    let mut list: Vec<(crate::catalog::Person, usize)> = app.cat.persons.iter().map(|p| (p.clone(), app.cat.person_photos(p.id).len())).collect();
    list.sort_by(|a, b| a.0.name.is_empty().cmp(&b.0.name.is_empty()).then(b.1.cmp(&a.1)).then(a.0.name.cmp(&b.0.name)));
    if list.is_empty() && app.people.job.is_none() {
        let loose = app.cat.faces.iter().filter(|f| f.person == 0).count();
        ui.label(egui::RichText::new(trf!("두 장 이상에 나온 사람이 아직 없습니다 (얼굴 {loose}개)", "Nobody appears in two or more photos yet ({loose} faces)")).size(11.0).color(TEXT_WEAK()));
    }
    let others: Vec<(i64, String)> = list.iter().map(|(p, _)| (p.id, person_label(p))).collect();
    for (p, n) in &list {
        let r = person_row(app, ui, p, *n).on_hover_text(tr!("클릭: 이 사람이 나온 사진 · 오른쪽 클릭: 이름 · 합치기", "Click: photos with this person · right-click: name · merge"));
        r.context_menu(|ui| {
            if ui.button(tr!("이름 짓기…", "Name…")).clicked() {
                app.people.rename = Some((p.id, p.name.clone()));
                ui.close();
            }
            ui.menu_button(tr!("다른 인물과 합치기", "Merge with another person"), |ui| {
                for (oid, ol) in others.iter().filter(|(o, _)| *o != p.id) {
                    if ui.button(ol).clicked() {
                        if let Err(e) = app.cat.merge_person(p.id, *oid) {
                            app.toast_err(trf!("합치기 실패: {e}", "Merge failed: {e}"));
                        }
                        if app.source == Source::Person(p.id) {
                            app.set_source(Source::Person(*oid));
                        }
                        app.visible_dirty = true;
                        ui.close();
                    }
                }
            });
            if !p.name.is_empty() && ui.button(tr!("사진에 이름 키워드 달기", "Add name keyword to photos")).on_hover_text(tr!("이 사람이 나온 사진에 이름을 키워드로 붙임 (내보낼 때 메타데이터에 포함)", "Adds the name as a keyword to photos with this person (included in metadata on export)")).clicked() {
                let ids: Vec<PhotoId> = app.cat.person_photos(p.id).into_iter().collect();
                match app.cat.add_keywords(&ids, std::slice::from_ref(&p.name)) {
                    Ok(()) => app.toast(trf!("{}장에 키워드 '{}'를 달았습니다", "Added keyword '{}' to {} photos", ids.len(), p.name)),
                    Err(e) => app.toast_err(trf!("키워드 실패: {e}", "Keyword failed: {e}")),
                }
                ui.close();
            }
            ui.separator();
            if ui.button(tr!("묶음 풀기", "Ungroup")).on_hover_text(tr!("이 인물을 없애고 얼굴들은 다시 묶지 않음", "Removes this person; their faces won't be grouped again")).clicked() {
                let _ = app.cat.dissolve_person(p.id);
                if app.source == Source::Person(p.id) {
                    app.set_source(Source::All);
                }
                app.visible_dirty = true;
                ui.close();
            }
        });
    }
    if app.people.job.is_none() {
        ui.add_space(4.0);
        if todo > 0
            && widgets::button_row(ui, &[(&trf!("새 사진에서 찾기 ({todo}장)", "Scan new photos ({todo} photos)"), true, "")]).is_some() {
                start_scan(app);
            }
    }
}

/// Photo menu: remove from the person currently shown.
pub fn photo_menu_items(app: &mut App, ui: &mut Ui) {
    let Source::Person(pid) = app.source else { return };
    let ids = app.multi_targets();
    if ui.button(trf!("이 사람이 아님 ({}장)", "Not this person ({} photos)", ids.len())).on_hover_text(tr!("선택한 사진의 이 인물 얼굴을 빼고 다시 묶지 않음", "Removes this person's face from the selected photos and won't group it again")).clicked() {
        let faces: Vec<i64> = app.cat.faces.iter().filter(|f| f.person == pid && ids.contains(&f.photo)).map(|f| f.id).collect();
        let _ = app.cat.set_face_person(&faces, -1);
        app.visible_dirty = true;
        ui.close();
    }
}

pub fn rename_dialog(app: &mut App, ctx: &egui::Context) {
    let Some((pid, mut name)) = app.people.rename.take() else { return };
    let cover = app.cat.person_cover(pid);
    let tex = cover.and_then(|f| face_tex(app, ctx, f));
    let (_, (ok, cancel), close) = super::form::modal(
        ctx,
        "person_name",
        tr!("이름 짓기", "Name person"),
        "",
        vec2(380.0, 220.0),
        |ui| {
            super::form::card(ui, "", "", |ui| {
                ui.horizontal(|ui| {
                    if let Some(t) = &tex {
                        ui.add(egui::Image::new(t).fit_to_exact_size(vec2(64.0, 64.0)).corner_radius(32.0));
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(tr!("이름", "Name")).desired_width(220.0));
                    r.request_focus();
                })
            });
        },
        |ui| {
            let mut out = (false, false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                out.0 = super::form::primary(ui, tr!("확인", "OK"), true).clicked();
                out.1 = super::form::secondary(ui, tr!("취소", "Cancel")).clicked();
            });
            out
        },
    );
    if ok || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        // Merge into an existing person with the same name.
        let same = app.cat.persons.iter().find(|p| p.id != pid && !name.trim().is_empty() && p.name == name.trim()).map(|p| p.id);
        let r = match same {
            Some(o) => app.cat.merge_person(pid, o).map(|_| {
                if app.source == Source::Person(pid) {
                    app.set_source(Source::Person(o));
                }
            }),
            None => app.cat.rename_person(pid, &name),
        };
        if let Err(e) = r {
            app.toast_err(trf!("이름 바꾸기 실패: {e}", "Rename failed: {e}"));
        }
        app.visible_dirty = true;
        return;
    }
    if cancel || close {
        return;
    }
    app.people.rename = Some((pid, name));
}
