//! Library module: source panel, filter bar, grid/loupe/compare/survey views, quick develop, keywords, metadata, filmstrip.

use super::app::*;
use super::theme::*;
use super::widgets::{self, section};
use crate::catalog::{ColorLabel, Flag, PhotoId};
use crate::config;
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

pub fn show(app: &mut App, ui: &mut Ui) {
    if app.prefs.show_left {
        let film_tab = app.prefs.film_pos == FilmPos::Left && app.lib_view != LibView::Grid;
        egui::Panel::left("lib_left")
            .default_size(config::LEFT_PANEL_WIDTH)
            .min_size(180.0)
            .max_size(420.0)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                if film_tab {
                    let names = vec![tr!("카탈로그", "Catalog").to_string(), trf!("사진 {}", "Photos {}", app.visible.len())];
                    if app.left_tab_lib > 1 {
                        app.left_tab_lib = 1;
                    }
                    widgets::tab_bar(ui, &mut app.left_tab_lib, &names);
                }
                if film_tab && app.left_tab_lib == 1 {
                    film_vertical(app, ui);
                } else {
                    egui::ScrollArea::vertical().id_salt("lib_left_scroll").show(ui, |ui| left_panel(app, ui));
                }
            });
    }
    if app.prefs.show_right {
        egui::Panel::right("lib_right")
            .default_size(config::RIGHT_PANEL_WIDTH - 20.0)
            .min_size(240.0)
            .max_size(420.0)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("lib_right_scroll").show(ui, |ui| right_panel(app, ui));
            });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        if app.lib_view == LibView::Grid {
            filter_bar(app, ui);
        }
        let rect = ui.available_rect_before_wrap();
        let rect = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - 30.0));
        match app.lib_view {
            LibView::Grid => grid(app, ui, rect),
            LibView::Loupe => loupe(app, ui, rect),
            LibView::Compare => compare(app, ui, rect),
            LibView::Survey => survey(app, ui, rect),
        }
        toolbar(app, ui);
    });
}

fn source_row(app: &mut App, ui: &mut Ui, label: &str, src: Source, n: usize) -> egui::Response {
    // List row (same look as the history and mask lists): full width, selected = darker fill + accent bar, count on the right
    let sel = app.source == src;
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let p = ui.painter();
    if sel {
        p.rect_filled(rect, 4.0, ACTIVE());
        p.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(2.5, 12.0)), 1.0, ACCENT);
    } else if r.hovered() {
        p.rect_filled(rect, 4.0, HOVER());
    }
    let cnt = p.layout_no_wrap(n.to_string(), FontId::monospace(10.0), TEXT_DIM());
    let cw = cnt.size().x;
    p.galley(pos2(rect.right() - 6.0 - cw, rect.center().y - cnt.size().y * 0.5), cnt, TEXT_DIM());
    let g = p.layout_no_wrap(label.to_string(), FontId::proportional(12.0), if sel { STRONG() } else { TEXT() });
    let clip = Rect::from_min_max(rect.min, pos2(rect.right() - cw - 12.0, rect.max.y));
    ui.painter_at(clip).galley(pos2(rect.left() + 9.0, rect.center().y - g.size().y * 0.5), g, TEXT());
    if r.clicked() {
        app.set_source(src);
    }
    r
}

fn left_panel(app: &mut App, ui: &mut Ui) {
    section(ui, tr!("카탈로그", "Catalog"), true, |ui| {
        let all = app.cat.photos.len();
        let q = app.cat.quick_collection.len();
        let li = app.cat.photos.iter().filter(|p| p.import_id == app.cat.last_import && p.import_id != 0).count();
        source_row(app, ui, tr!("모든 사진", "All Photographs"), Source::All, all);
        source_row(app, ui, tr!("빠른 컬렉션  (B)", "Quick Collection  (B)"), Source::Quick, q);
        source_row(app, ui, tr!("이전 가져오기", "Previous Import"), Source::LastImport, li);
        let miss = app.cat.photos.iter().filter(|p| p.missing).count();
        if miss > 0 {
            let r = source_row(app, ui, tr!("누락된 사진", "Missing Photos"), Source::Missing, miss).on_hover_text(tr!("원본 파일을 찾을 수 없는 사진 — 오른쪽 클릭: 다시 찾기", "Photos whose original file can't be found — right-click: locate again"));
            r.context_menu(|ui| {
                if ui.button(tr!("폴더에서 다시 찾기…", "Locate in folder…")).clicked() {
                    ui.close();
                    super::tools::relink_missing(app);
                }
            });
            if widgets::button_row(ui, &[(tr!("누락된 파일 다시 찾기…", "Locate missing files…"), true, "")]).is_some() {
                super::tools::relink_missing(app);
            }
        }
    });
    section(ui, tr!("인물", "People"), true, |ui| super::people_ui::section(app, ui));
    section(ui, tr!("폴더", "Folder"), true, |ui| {
        let folders = app.cat.folders();
        if folders.is_empty() {
            ui.label(egui::RichText::new(tr!("가져온 폴더가 없습니다", "No imported folders")).color(TEXT_DIM()));
        }
        for (f, n) in folders {
            let comps: Vec<String> = f.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
            let short = if comps.len() > 2 { format!("… › {}", comps[comps.len() - 2..].join(" › ")) } else { comps.join(" › ") };
            let r = source_row(app, ui, &short, Source::Folder(f.clone()), n).on_hover_text(f.to_string_lossy());
            r.context_menu(|ui| {
                if ui.button(tr!("탐색기에서 열기", "Open in Explorer")).clicked() {
                    let _ = std::process::Command::new("explorer").arg(&f).spawn();
                    ui.close();
                }
                if ui.button(tr!("폴더 위치 다시 지정…", "Relocate folder…")).on_hover_text(tr!("드라이브·폴더를 옮겼을 때 새 위치로 연결", "Link to the new location after moving a drive or folder")).clicked() {
                    ui.close();
                    super::tools::relocate_folder(app, &f);
                }
                if ui.button(tr!("폴더 동기화 (새 파일 가져오기)", "Synchronize folder (import new files)")).clicked() {
                    super::dialogs::start_import(
                        app,
                        super::dialogs::ImportPlan { source: f.clone(), recursive: false, ..Default::default() },
                    );
                    ui.close();
                }
            });
        }
    });
    section(ui, tr!("컬렉션", "Collections"), true, |ui| {
        match widgets::button_row(ui, &[(tr!("+ 컬렉션", "+ Collection"), true, tr!("선택한 사진을 모아 둘 컬렉션", "A collection that holds selected photos")), (tr!("+ 스마트 컬렉션", "+ Smart collection"), true, tr!("조건(별점·키워드 등)에 맞는 사진이 자동으로 모임", "Photos matching rules (rating, keywords…) gather automatically"))]) {
            Some(0) => app.dlg.new_collection = Some(String::new()),
            Some(_) => app.dlg.smart = Some(super::dialogs::SmartEditor::new(None, tr!("스마트 컬렉션", "Smart collection").into(), Default::default())),
            None => {}
        }
        ui.add_space(2.0);
        let cols = app.cat.collections.clone();
        for c in cols {
            let n = match &c.smart {
                Some(r) => app.smart_matches_count(r),
                None => app.cat.collection_members.get(&c.id).map(|v| v.len()).unwrap_or(0),
            };
            let label = if c.smart.is_some() { format!("◆ {}", c.name) } else { c.name.clone() };
            let r = source_row(app, ui, &label, Source::Collection(c.id), n);
            r.context_menu(|ui| {
                if c.smart.is_none() && ui.button(tr!("선택한 사진 추가", "Add selected photos")).clicked() {
                    let t = app.targets();
                    let _ = app.cat.add_to_collection(c.id, &t);
                    app.toast(trf!("'{}'에 {}장 추가", "Add {} photos to '{}'", c.name, t.len()));
                    ui.close();
                }
                if c.smart.is_none() && app.source == Source::Collection(c.id) && ui.button(tr!("선택한 사진 빼기", "Remove selected photos")).clicked() {
                    let t = app.targets();
                    let _ = app.cat.remove_from_collection(c.id, &t);
                    app.recompute_visible();
                    ui.close();
                }
                if let Some(r) = &c.smart
                    && ui.button(tr!("규칙 편집…", "Edit rules…")).clicked() {
                        app.dlg.smart = Some(super::dialogs::SmartEditor::new(Some(c.id), c.name.clone(), r.clone()));
                        ui.close();
                    }
                if ui.button(tr!("이름 바꾸기…", "Rename…")).clicked() {
                    app.dlg.rename_collection = Some((c.id, c.name.clone()));
                    ui.close();
                }
                if ui.button(tr!("삭제", "Delete")).clicked() {
                    let _ = app.cat.delete_collection(c.id);
                    if app.source == Source::Collection(c.id) {
                        app.set_source(Source::All);
                    }
                    ui.close();
                }
            });
        }
    });
}

fn filter_bar(app: &mut App, ui: &mut Ui) {
    let before = app.filter.clone();
    egui::Frame::new().fill(PANEL2()).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        // Paint the background across the full width, not just the content width
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            let te = ui.add(egui::TextEdit::singleline(&mut app.filter.text).hint_text(tr!("검색 (Ctrl+F)", "Search (Ctrl+F)")).desired_width(170.0));
            if app.dlg.focus_search {
                te.request_focus();
                app.dlg.focus_search = false;
            }
            ui.separator();
            ui.label(egui::RichText::new(tr!("별점", "Rating")).color(TEXT_WEAK()));
            egui::ComboBox::from_id_salt("rop")
                .width(36.0)
                .selected_text(match app.filter.rating_op {
                    RatingOp::AtLeast => "≥",
                    RatingOp::Exactly => "=",
                    RatingOp::AtMost => "≤",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.filter.rating_op, RatingOp::AtLeast, tr!("≥ 이상", "≥ at least"));
                    ui.selectable_value(&mut app.filter.rating_op, RatingOp::Exactly, tr!("= 정확히", "= exactly"));
                    ui.selectable_value(&mut app.filter.rating_op, RatingOp::AtMost, tr!("≤ 이하", "≤ at most"));
                });
            let (rect, resp) = ui.allocate_exact_size(vec2(80.0, 18.0), Sense::click());
            let p = ui.painter();
            for i in 0..5u8 {
                let c = pos2(rect.left() + 8.0 + i as f32 * 16.0, rect.center().y);
                if i < app.filter.rating {
                    widgets::star_shape(p, c, 6.0, TEXT());
                } else {
                    widgets::star_shape(p, c, 6.0, TEXT_DIM());
                }
            }
            if resp.clicked()
                && let Some(pp) = resp.interact_pointer_pos() {
                    let r = (((pp.x - rect.left()) / 16.0).floor() as u8 + 1).min(5);
                    app.filter.rating = if app.filter.rating == r { 0 } else { r };
                }
            ui.separator();
            egui::ComboBox::from_id_salt("flagf")
                .width(80.0)
                .selected_text(match app.filter.flag {
                    FlagFilter::All => tr!("깃발: 전체", "Flag: all"),
                    FlagFilter::Picked => tr!("선택됨", "Picked"),
                    FlagFilter::Unflagged => tr!("깃발 없음", "Unflagged"),
                    FlagFilter::Rejected => tr!("거부됨", "Rejected"),
                    FlagFilter::NotRejected => tr!("거부 제외", "Hide rejected"),
                })
                .show_ui(ui, |ui| {
                    for (v, n) in [
                        (FlagFilter::All, tr!("전체", "All")),
                        (FlagFilter::Picked, tr!("선택됨 (P)", "Picked (P)")),
                        (FlagFilter::Unflagged, tr!("깃발 없음", "Unflagged")),
                        (FlagFilter::Rejected, tr!("거부됨 (X)", "Rejected (X)")),
                        (FlagFilter::NotRejected, tr!("거부 제외", "Hide rejected")),
                    ] {
                        ui.selectable_value(&mut app.filter.flag, v, n);
                    }
                });
            ui.separator();
            for l in &ColorLabel::ALL[1..] {
                let on = app.filter.labels.contains(l);
                let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                widgets::label_dot(ui.painter(), r.center(), if on { 6.5 } else { 5.0 }, *l);
                if on {
                    ui.painter().circle_stroke(r.center(), 7.5, Stroke::new(1.0, TEXT()));
                }
                if resp.on_hover_text(l.name()).clicked() {
                    if on {
                        app.filter.labels.retain(|x| x != l);
                    } else {
                        app.filter.labels.push(*l);
                    }
                }
            }
            ui.separator();
            egui::ComboBox::from_id_salt("kindf")
                .width(70.0)
                .selected_text(match app.filter.kind {
                    KindFilter::All => tr!("형식: 전체", "Type: all"),
                    KindFilter::Raw => "RAW",
                    KindFilter::Jpeg => "JPEG",
                    KindFilter::Heif => "HEIC",
                    KindFilter::Other => tr!("기타", "Other"),
                })
                .show_ui(ui, |ui| {
                    for (v, n) in [(KindFilter::All, tr!("전체", "All")), (KindFilter::Raw, "RAW"), (KindFilter::Jpeg, "JPEG"), (KindFilter::Heif, "HEIC/HEIF"), (KindFilter::Other, tr!("기타 (PNG/TIFF)", "Other (PNG/TIFF)"))] {
                        ui.selectable_value(&mut app.filter.kind, v, n);
                    }
                });
            egui::ComboBox::from_id_salt("editf")
                .width(70.0)
                .selected_text(match app.filter.edited {
                    None => tr!("편집: 전체", "Edit: all"),
                    Some(true) => tr!("편집됨", "Edited"),
                    Some(false) => tr!("미편집", "Unedited"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.filter.edited, None, tr!("전체", "All"));
                    ui.selectable_value(&mut app.filter.edited, Some(true), tr!("편집됨", "Edited"));
                    ui.selectable_value(&mut app.filter.edited, Some(false), tr!("미편집", "Unedited"));
                });
            if app.filter.is_active() && ui.button(tr!("필터 해제", "Clear filter")).clicked() {
                app.filter = Default::default();
            }
        });
    });
    if app.filter != before {
        app.recompute_visible();
    }
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 30.0), rect.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
        ui.painter().rect_filled(bar, 0.0, PANEL2());
        ui.horizontal_centered(|ui| {
            ui.add_space(8.0);
            for (v, t, k) in [(LibView::Grid, tr!("그리드", "Grid"), "G"), (LibView::Loupe, tr!("루페", "Loupe"), "E"), (LibView::Compare, tr!("비교", "Compare"), "C"), (LibView::Survey, tr!("서베이", "Survey"), "N")] {
                if widgets::icon_button(ui, t, app.lib_view == v, k).clicked() {
                    app.set_lib_view(v);
                }
            }
            ui.separator();
            ui.label(egui::RichText::new(tr!("정렬", "Sort")).color(TEXT_WEAK()));
            let before = (app.prefs.sort, app.prefs.sort_desc);
            egui::ComboBox::from_id_salt("sort").width(90.0).selected_text(app.prefs.sort.name()).show_ui(ui, |ui| {
                for k in SortKey::ALL {
                    ui.selectable_value(&mut app.prefs.sort, k, k.name());
                }
            });
            if ui.button(if app.prefs.sort_desc { "↓" } else { "↑" }).on_hover_text(tr!("정렬 방향", "Sort direction")).clicked() {
                app.prefs.sort_desc = !app.prefs.sort_desc;
            }
            if before != (app.prefs.sort, app.prefs.sort_desc) {
                app.recompute_visible();
            }
            if app.lib_view == LibView::Loupe
                && let Some(l) = &mut app.loupe {
                    ui.separator();
                    let ppp = ui.ctx().pixels_per_point();
                    ui.label(mono(l.zoom_label(ppp)).color(TEXT_WEAK()));
                    for (lbl, z) in [(tr!("맞춤", "Fit"), None), ("50%", Some(0.5)), ("100%", Some(1.0)), ("200%", Some(2.0))] {
                        if ui.small_button(lbl).clicked() {
                            l.set_zoom(z);
                        }
                    }
                }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if app.lib_view == LibView::Grid {
                    ui.add(egui::Slider::new(&mut app.prefs.grid_cell, config::GRID_CELL_MIN..=config::GRID_CELL_MAX).show_value(false))
                        .on_hover_text(tr!("썸네일 크기", "Thumbnail size"));
                }
                ui.label(mono(trf!("{}장 · 선택 {}", "{} photos · {} selected", app.visible.len(), app.selected.len())).color(TEXT_WEAK()));
            });
        });
    });
}

/// `cols`: cells per row (1 = single column, stack bands join vertically)
fn draw_cell(app: &mut App, ui: &mut Ui, rect: Rect, id: PhotoId, idx: usize, compact: bool, cols: usize) -> egui::Response {
    let resp = ui.interact(rect, egui::Id::new(("cell", id, compact)), Sense::click());
    let selected = app.sel_set.contains(&id);
    let current = app.current == Some(id);
    let p = ui.painter_at(rect);
    let bg = if current {
        ACTIVE()
    } else if selected {
        HOVER()
    } else if resp.hovered() {
        WIDGET()
    } else {
        PANEL2()
    };
    // Expanded stack: join member cells into one band with an outline (rounded at start and end, open where the row wraps)
    let sid = app.cat.get(id).map(|p| p.stack_id).unwrap_or(0);
    let open_stack = sid != 0 && app.expanded_stacks.contains(&sid);
    let same_at = |j: Option<usize>| j.and_then(|j| app.visible.get(j)).and_then(|o| app.cat.get(*o)).map(|o| o.stack_id == sid).unwrap_or(false);
    let prev_same = open_stack && idx > 0 && same_at(Some(idx - 1));
    let next_same = open_stack && same_at(Some(idx + 1));
    if open_stack {
        let vertical = cols == 1;
        let mut br = rect.shrink(1.0);
        // Continuing sides extend to the cell edge so neighbors touch
        if prev_same {
            if vertical { br.min.y = rect.min.y } else { br.min.x = rect.min.x }
        }
        if next_same {
            if vertical { br.max.y = rect.max.y } else { br.max.x = rect.max.x }
        }
        let rr = |closed: bool| if closed { 5u8 } else { 0u8 };
        let (c_start, c_end) = (!prev_same, !next_same);
        let corner = if vertical {
            egui::CornerRadius { nw: rr(c_start), ne: rr(c_start), sw: rr(c_end), se: rr(c_end) }
        } else {
            egui::CornerRadius { nw: rr(c_start), sw: rr(c_start), ne: rr(c_end), se: rr(c_end) }
        };
        let fill = if current || selected || resp.hovered() { bg } else { WIDGET() };
        p.rect_filled(br, corner, fill);
        // Outline: on open sides draw a rectangle extended past the cell and clipped to it, so corner curves join smoothly
        let mut sr = br;
        if prev_same {
            if vertical { sr.min.y -= 8.0 } else { sr.min.x -= 8.0 }
        }
        if next_same {
            if vertical { sr.max.y += 8.0 } else { sr.max.x += 8.0 }
        }
        let sc = egui::CornerRadius { nw: 5, ne: 5, sw: 5, se: 5 };
        p.with_clip_rect(rect).rect_stroke(sr, sc, Stroke::new(1.0, TEXT_DIM()), StrokeKind::Inside);
    } else {
        p.rect_filled(rect.shrink(1.0), 3.0, bg);
    }
    let mut tex = app.thumb(id);
    // The photo being developed shows live edits (the viewer's latest render),
    let mut live_size = None;
    // except during soft proofing or with mask/clipping overlays on, where the saved thumbnail is used
    // so those colors do not leak into the grid
    if let Some(d) = app.devst.as_ref().filter(|d| d.id == id && !app.proof.on && !(d.overlay && d.tool == super::develop::Tool::Mask) && !d.clipping)
        && let Some(t) = d.viewer.live_tex() {
            tex = Some(t.id());
            live_size = Some(t.size_vec2());
        }
    let Some(photo) = app.cat.get(id) else { return resp };
    let summary = match app.devst.as_ref().filter(|d| d.id == id) {
        Some(d) => d.settings.summary(photo.is_raw),
        None => photo.develop.as_ref().map(|s| s.summary(photo.is_raw)).unwrap_or_default(),
    };
    let exported = photo.exported_at > 0;
    let pad = if compact { 4.0 } else { 10.0 };
    let footer = if compact { 0.0 } else { 18.0 };
    let header = if compact { 0.0 } else { 14.0 };
    let inner = Rect::from_min_max(rect.min + vec2(pad, pad + header), rect.max - vec2(pad, pad + footer));
    if let Some(t) = tex {
        let size = live_size.or_else(|| app.tex.peek(&id).map(|(_, h)| h.size_vec2())).unwrap_or(vec2(3.0, 2.0));
        let mut r = super::viewer::fit_rect(inner, size);
        let collapsed_stack = sid != 0 && !open_stack && app.cat.stack_size(sid) > 1;
        if collapsed_stack {
            // Two offset cards behind, so a stack is recognizable at a glance
            let off = if compact { 2.5 } else { 3.5 };
            r = Rect::from_min_max(r.min + vec2(0.0, off * 2.0), r.max - vec2(off * 2.0, 0.0));
            for k in [2.0f32, 1.0] {
                let b = r.translate(vec2(off * k, -off * k));
                p.rect_filled(b, 1.0, Color32::from_gray(if k > 1.5 { 58 } else { 78 }));
                p.rect_stroke(b, 1.0, Stroke::new(1.0, Color32::from_black_alpha(120)), StrokeKind::Inside);
            }
        }
        p.image(t, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), if photo.flag == Flag::Reject { Color32::from_gray(90) } else { Color32::WHITE });
        if current {
            p.rect_stroke(r.expand(1.0), 0.0, Stroke::new(1.0, TEXT()), StrokeKind::Outside);
        }
    } else {
        let msg = if photo.missing {
            tr!("파일 없음", "File missing")
        } else if app.failed_thumbs.contains(&(id, photo.thumb_ver)) {
            tr!("미리보기 실패", "Preview failed")
        } else {
            "…"
        };
        p.text(inner.center(), Align2::CENTER_CENTER, msg, FontId::proportional(11.0), TEXT_DIM());
    }
    // Stack chip: stack icon + count (expanded: first shows "n", others "k/n"); click to expand/collapse
    let stack = if photo.stack_id != 0 { Some((photo.stack_id, photo.stack_pos, app.cat.stack_size(photo.stack_id))) } else { None };
    let mut stack_click = false;
    if let Some((sid, pos, n)) = stack {
        let open = app.expanded_stacks.contains(&sid);
        let first = !prev_same;
        let label = if !open || first { format!("{n}") } else { format!("{}/{}", pos + 1, n) };
        let font = FontId::proportional(if compact { 10.0 } else { 11.0 });
        let galley = ui.painter().layout_no_wrap(label, font, TEXT());
        let icon_w = 12.0;
        let chip_h = if compact { 15.0 } else { 17.0 };
        let at = if compact { rect.left_top() + vec2(4.0, 4.0) } else { rect.left_top() + vec2(26.0, 2.0) };
        let chip = Rect::from_min_size(at, vec2(icon_w + galley.size().x + 12.0, chip_h));
        let tip = if open { trf!("스택 {n}장 — 접기 (S)", "Stack of {n} — collapse (S)") } else { trf!("스택 {n}장 — 펼치기 (S)", "Stack of {n} — expand (S)") };
        let cr = ui.interact(chip, egui::Id::new(("stackchip", id, compact)), Sense::click()).on_hover_text(tip);
        let fill = if cr.hovered() { HOVER() } else if open { PANEL2() } else { Color32::from_black_alpha(170) };
        if open && !first {
            p.galley(chip.min + vec2(4.0, (chip_h - galley.size().y) * 0.5), galley, TEXT_WEAK());
        } else {
            p.rect_filled(chip, 3.0, fill);
            p.rect_stroke(chip, 3.0, Stroke::new(1.0, if open { TEXT_DIM() } else { Color32::from_white_alpha(40) }), StrokeKind::Inside);
            // Icon: two overlapping rectangles (side by side when expanded)
            let c = pos2(chip.left() + 5.0 + icon_w * 0.5, chip.center().y);
            let col = Color32::from_gray(225);
            if open {
                for dx in [-3.0f32, 3.0] {
                    p.rect_stroke(Rect::from_center_size(c + vec2(dx, 0.0), vec2(5.0, 7.0)), 0.5, Stroke::new(1.0, col), StrokeKind::Inside);
                }
            } else {
                p.rect_filled(Rect::from_center_size(c + vec2(1.5, -1.5), vec2(8.0, 7.0)), 0.5, Color32::from_gray(120));
                p.rect_filled(Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 7.0)), 0.5, col);
            }
            p.galley(pos2(chip.left() + 8.0 + icon_w, chip.center().y - galley.size().y * 0.5), galley, Color32::WHITE);
        }
        if cr.clicked() {
            stack_click = true;
        }
    }
    if stack_click
        && let Some((sid, ..)) = stack {
            if !app.expanded_stacks.remove(&sid) {
                app.expanded_stacks.insert(sid);
            }
            app.visible_dirty = true;
        }
    let Some(photo) = app.cat.get(id) else { return resp };
    if !compact {
        p.text(rect.left_top() + vec2(8.0, 6.0), Align2::LEFT_TOP, format!("{}", idx + 1), FontId::monospace(9.5), TEXT_DIM());
        p.text(rect.right_top() + vec2(-8.0, 6.0), Align2::RIGHT_TOP, &photo.ext, FontId::monospace(9.5), TEXT_DIM());
        let fy = rect.bottom() - pad - footer * 0.5;
        widgets::stars(&p, pos2(rect.left() + 12.0, fy), photo.rating, 7.0);
        let mut x = rect.right() - 12.0;
        if photo.label != ColorLabel::None {
            widgets::label_dot(&p, pos2(x, fy), 4.5, photo.label);
            x -= 14.0;
        }
        if photo.flag != Flag::None {
            widgets::flag_icon(&p, Rect::from_center_size(pos2(x, fy), vec2(9.0, 10.0)), photo.flag);
            x -= 14.0;
        }
        let badges = widgets::edit_badges(&p, pos2(x + 6.0, fy), summary, exported, false);
        x -= badges.len() as f32 * 15.0;
        badge_tips(ui, &badges);
        if photo.master.is_some() {
            p.text(pos2(x, fy), Align2::CENTER_CENTER, "VC", FontId::monospace(9.0), TEXT_WEAK());
            x -= 14.0;
        }
        if app.cat.quick_collection.contains(&id) {
            p.circle_filled(pos2(x, fy), 3.0, TEXT_WEAK());
        }
    } else {
        let badges = widgets::edit_badges(&p, rect.right_bottom() + vec2(-6.0, -11.0), summary, exported, true);
        badge_tips(ui, &badges);
        if photo.flag == Flag::Pick {
            widgets::flag_icon(&p, Rect::from_min_size(rect.left_top() + vec2(5.0, 5.0), vec2(8.0, 9.0)), photo.flag);
        }
        if photo.label != ColorLabel::None {
            widgets::label_dot(&p, rect.right_top() + vec2(-8.0, 8.0), 3.5, photo.label);
        }
    }
    resp
}

fn badge_tips(ui: &Ui, badges: &[(Rect, &'static str)]) {
    if let Some(hp) = ui.ctx().pointer_hover_pos()
        && let Some((_, name)) = badges.iter().find(|(r, _)| r.expand(1.0).contains(hp)) {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new("badge_tip"), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(*name);
            });
        }
}

fn cell_interact(app: &mut App, ui: &Ui, resp: &egui::Response, id: PhotoId, open_loupe_on_double: bool) {
    if resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        if m.command {
            app.toggle_select(id);
        } else if m.shift {
            app.range_select(id);
        } else {
            app.select_single(id);
        }
    }
    if resp.double_clicked() {
        app.select_single(id);
        if open_loupe_on_double {
            app.set_lib_view(LibView::Loupe);
        }
    }
    if resp.secondary_clicked() && !app.sel_set.contains(&id) {
        app.select_single(id);
    }
    resp.context_menu(|ui| photo_menu(app, ui));
}

pub fn photo_menu(app: &mut App, ui: &mut Ui) {
    let n = app.multi_targets().len();
    ui.label(egui::RichText::new(trf!("{n}장 선택", "{n} selected")).color(TEXT_WEAK()));
    super::people_ui::photo_menu_items(app, ui);
    ui.menu_button(tr!("별점", "Rating"), |ui| {
        for r in 0..=5u8 {
            if ui.button(if r == 0 { tr!("없음 (0)", "None (0)").to_string() } else { format!("{} ({r})", "★".repeat(r as usize)) }).clicked() {
                app.apply_rating(r);
                ui.close();
            }
        }
    });
    ui.menu_button(tr!("깃발", "Flag"), |ui| {
        for (f, t) in [(Flag::Pick, tr!("선택 (P)", "Pick (P)")), (Flag::None, tr!("없음 (U)", "None (U)")), (Flag::Reject, tr!("거부 (X)", "Reject (X)"))] {
            if ui.button(t).clicked() {
                app.apply_flag(f);
                ui.close();
            }
        }
    });
    ui.menu_button(tr!("색상 라벨", "Color label"), |ui| {
        for l in ColorLabel::ALL {
            if ui.button(l.name()).clicked() {
                let t = app.targets();
                let _ = app.cat.set_label(&t, l);
                ui.close();
            }
        }
    });
    ui.menu_button(tr!("컬렉션에 추가", "Add to collection"), |ui| {
        let cols: Vec<_> = app.cat.collections.iter().filter(|c| c.smart.is_none()).map(|c| (c.id, c.name.clone())).collect();
        if cols.is_empty() {
            ui.label(tr!("일반 컬렉션이 없습니다", "No regular collections"));
        }
        for (cid, name) in cols {
            if ui.button(&name).clicked() {
                let t = app.targets();
                let _ = app.cat.add_to_collection(cid, &t);
                app.toast(trf!("'{name}'에 {}장 추가", "Add {} photos to '{name}'", t.len()));
                ui.close();
            }
        }
        ui.separator();
        if ui.button(tr!("새 컬렉션…", "New collection…")).clicked() {
            app.dlg.new_collection = Some(String::new());
            app.dlg.new_collection_add = true;
            ui.close();
        }
    });
    if ui.button(tr!("빠른 컬렉션 토글 (B)", "Toggle Quick Collection (B)")).clicked() {
        let t = app.targets();
        let _ = app.cat.toggle_quick(&t);
        ui.close();
    }
    ui.separator();
    if ui.button(tr!("현상 설정 복사 (Ctrl+Shift+C)", "Copy develop settings (Ctrl+Shift+C)")).clicked() {
        app.dlg.copy_settings = Some(crate::develop::settings::SettingGroups::all());
        ui.close();
    }
    if ui.add_enabled(app.clipboard.is_some(), egui::Button::new(tr!("현상 설정 붙여넣기 (Ctrl+Shift+V)", "Paste develop settings (Ctrl+Shift+V)"))).clicked() {
        app.paste_settings();
        ui.close();
    }
    if ui.button(tr!("현상 설정 초기화", "Reset develop settings")).clicked() {
        let t = app.targets();
        for id in t {
            if let Some(p) = app.cat.get(id) {
                let s = crate::develop::settings::DevelopSettings::default_for(p.is_raw);
                let _ = app.cat.set_develop(id, &s);
                let _ = app.cat.push_history(id, tr!("초기화", "Reset"), &s);
            }
        }
        app.recompute_visible();
        ui.close();
    }
    if let Some(c) = app.current
        && ui.button(tr!("참조 사진으로 고정 (현상 · Shift+R)", "Pin as reference photo (Develop · Shift+R)")).on_hover_text(tr!("현상에서 이 사진을 왼쪽에 띄워 두고 다른 사진을 맞춥니다", "Keeps this photo on the left in Develop so you can match other photos to it")).clicked() {
            super::develop::set_reference(app, c);
            ui.close();
        }
    if ui.button(tr!("가상 사본 만들기 (Ctrl+')", "Create virtual copy (Ctrl+')")).clicked() {
        app.virtual_copy();
        ui.close();
    }
    ui.separator();
    if ui.button(tr!("향상… (Ctrl+Alt+I)", "Enhance… (Ctrl+Alt+I)")).on_hover_text(tr!("AI 노이즈 감소 · AI 해상도 2배 — 새 파일로 저장해 원본과 스택", "AI Denoise · AI Super Resolution 2× — saved as a new file stacked with the original")).clicked() {
        super::enhance::open(app);
        ui.close();
    }
    if ui.add_enabled(n >= 2, egui::Button::new(trf!("총 노출 맞추기 ({n}장)", "Match total exposures ({n} photos)"))).on_hover_text(tr!("셔터·조리개·ISO로 계산해 현재 사진과 같은 밝기가 되도록 노출 조정 (Ctrl+Alt+Shift+M)", "Adjusts exposure from shutter·aperture·ISO to match the current photo's brightness (Ctrl+Alt+Shift+M)")).clicked() {
        super::develop::match_exposures(app);
        ui.close();
    }
    if ui.add_enabled(n >= 2, egui::Button::new(trf!("파노라마 병합 ({n}장)  Ctrl+M", "Panorama merge ({n} photos)  Ctrl+M"))).on_hover_text(tr!("겹치게 찍은 사진들을 이어 붙여 하나의 원본을 만듭니다", "Stitches overlapping photos into a single original")).clicked() {
        super::tools::open_pano(app);
        ui.close();
    }
    if ui.add_enabled(n >= 2, egui::Button::new(trf!("HDR 병합 ({n}장)", "HDR merge ({n} photos)"))).on_hover_text(tr!("노출을 달리 찍은 RAW를 정렬·합성해 하나의 HDR 원본을 만듭니다", "Aligns and merges bracketed RAWs into a single HDR original")).clicked() {
        super::tools::start_hdr(app);
        ui.close();
    }
    if ui.button(tr!("이름 일괄 변경… (F2)", "Batch rename… (F2)")).clicked() {
        super::tools::open_rename(app);
        ui.close();
    }
    ui.menu_button(tr!("스택", "Stack"), |ui| {
        if ui.button(tr!("선택한 사진 스택으로 묶기 (Ctrl+G)", "Group selected photos into a stack (Ctrl+G)")).clicked() {
            super::tools::stack_selected(app);
            ui.close();
        }
        if ui.button(tr!("스택 해제 (Ctrl+Shift+G)", "Unstack (Ctrl+Shift+G)")).clicked() {
            super::tools::unstack_selected(app);
            ui.close();
        }
        if ui.button(tr!("스택에서 빼기", "Remove from stack")).clicked() {
            let t = app.targets();
            let _ = app.cat.unstack(&t);
            app.visible_dirty = true;
            ui.close();
        }
        if ui.button(tr!("이 사진을 맨 위로", "Move this photo to top")).clicked() {
            if let Some(c) = app.current {
                let old = app.cat.get(c).map(|p| p.stack_id).unwrap_or(0);
                let _ = app.cat.set_stack_top(c);
                if app.expanded_stacks.remove(&old) {
                    app.expanded_stacks.insert(c);
                }
                app.visible_dirty = true;
            }
            ui.close();
        }
        if ui.button(tr!("펼치기 / 접기 (S)", "Expand / collapse (S)")).clicked() {
            super::tools::toggle_stack(app);
            ui.close();
        }
        ui.separator();
        ui.label(egui::RichText::new(tr!("촬영 시간으로 자동 스택", "Auto-stack by capture time")).color(TEXT_WEAK()));
        ui.horizontal(|ui| {
            for (name, gap) in [(tr!("2초", "2 seconds"), 2.0), (tr!("10초", "10 seconds"), 10.0), (tr!("1분", "1 minute"), 60.0), (tr!("5분", "5 minutes"), 300.0)] {
                if ui.button(name).on_hover_text(trf!("선택한 사진(없으면 보이는 사진)에서 촬영 간격 {name} 이하로 이어지는 사진끼리", "Groups consecutive photos taken within {name} of each other, from the selection (or visible photos if none)")).clicked() {
                    super::tools::auto_stack(app, gap);
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button(tr!("모든 스택 펼치기", "Expand all stacks")).clicked() {
            let all: Vec<i64> = app.cat.photos.iter().map(|p| p.stack_id).filter(|s| *s != 0).collect();
            app.expanded_stacks.extend(all);
            app.visible_dirty = true;
            ui.close();
        }
        if ui.button(tr!("모든 스택 접기", "Collapse all stacks")).clicked() {
            app.expanded_stacks.clear();
            app.visible_dirty = true;
            ui.close();
        }
    });
    ui.menu_button(tr!("XMP 사이드카", "XMP sidecar"), |ui| {
        if ui.button(tr!("현상 설정을 XMP에 저장 (Ctrl+S)", "Save develop settings to XMP (Ctrl+S)")).on_hover_text(tr!("XMP를 읽는 다른 사진 보정 프로그램에서도 같은 편집으로 열립니다", "Other photo editors that read XMP open it with the same edits")).clicked() {
            super::tools::save_xmp(app);
            ui.close();
        }
        if ui.button(tr!("XMP에서 설정 불러오기", "Read settings from XMP")).on_hover_text(tr!("다른 프로그램에서 편집해 저장한 XMP를 읽습니다", "Reads an XMP edited and saved in another program")).clicked() {
            super::tools::load_xmp(app);
            ui.close();
        }
    });
    if ui.button(tr!("인쇄 · 레이아웃… (Ctrl+P)", "Print · Layout… (Ctrl+P)")).clicked() {
        super::tools::open_print(app);
        ui.close();
    }
    if ui.button(tr!("슬라이드쇼 (F11)", "Slideshow (F11)")).clicked() {
        super::tools::start_slideshow(app);
        ui.close();
    }
    if ui.button(tr!("탐색기에서 보기", "Show in Explorer")).clicked() {
        app.show_in_explorer();
        ui.close();
    }
    if ui.button(tr!("내보내기…", "Export…")).clicked() {
        super::dialogs::open_export(app);
        ui.close();
    }
    if ui.button(tr!("카탈로그에서 제거…", "Remove from catalog…")).clicked() {
        app.dlg.confirm_remove = true;
        ui.close();
    }
}

fn grid(app: &mut App, ui: &mut Ui, rect: Rect) {
    let cell = app.prefs.grid_cell;
    let cell_h = cell + 26.0;
    let n = app.visible.len();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.painter().rect_filled(rect, 0.0, CANVAS());
        if n == 0 {
            let msg = if app.cat.photos.is_empty() { tr!("가져오기(Ctrl+Shift+I)로 사진을 추가하세요", "Add photos with Import (Ctrl+Shift+I)") } else { tr!("조건에 맞는 사진이 없습니다", "No photos match the filter") };
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), TEXT_WEAK());
            return;
        }
        let avail_w = rect.width() - 16.0;
        let cols = ((avail_w / cell).floor() as usize).max(1);
        app.dlg.grid_cols = cols;
        let cw = avail_w / cols as f32;
        let rows = n.div_ceil(cols);
        let cur_row = app.current.and_then(|c| app.visible.iter().position(|x| *x == c)).map(|i| i / cols);
        let scroll_to = if app.scroll_to_current { cur_row } else { None };
        app.scroll_to_current = false;
        let mut sa = egui::ScrollArea::vertical().id_salt("grid").auto_shrink([false, false]);
        if let Some(r) = scroll_to {
            // Scroll so the current row is visible (no-op if already visible)
            let st = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("grid_off"))).unwrap_or(0.0);
            let top = r as f32 * cell_h;
            let vh = rect.height();
            if top < st || top + cell_h > st + vh {
                sa = sa.vertical_scroll_offset((top - vh * 0.5 + cell_h * 0.5).max(0.0));
            }
        }
        let out = sa.show_viewport(ui, |ui, vp| {
            ui.set_height(rows as f32 * cell_h + 8.0);
            let first = (vp.top() / cell_h).floor().max(0.0) as usize;
            let last = ((vp.bottom() / cell_h).ceil() as usize + 1).min(rows);
            let origin = ui.min_rect().min + vec2(8.0, 4.0);
            for r in first..last {
                for c in 0..cols {
                    let i = r * cols + c;
                    if i >= n {
                        break;
                    }
                    let id = app.visible[i];
                    let cr = Rect::from_min_size(origin + vec2(c as f32 * cw, r as f32 * cell_h), vec2(cw, cell_h));
                    let resp = draw_cell(app, ui, cr, id, i, false, cols);
                    cell_interact(app, ui, &resp, id, true);
                }
            }
            // Drop requests outside the visible range (keeps the queue small during fast scrolling)
            app.thumbs.trim((last - first + 4) * cols * 2);
        });
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("grid_off"), out.state.offset.y));
    });
}

fn loupe(app: &mut App, ui: &mut Ui, rect: Rect) {
    let Some(cur) = app.current else { return };
    if app.loupe.as_ref().map(|l| l.id != cur).unwrap_or(true) {
        app.open_loupe(cur);
    }
    let Some(p) = app.cat.get(cur) else { return };
    let settings = p.settings();
    let info = format!("{}   {}", p.display_name(), p.meta.summary());
    let rating = p.rating;
    let mut loupe = app.loupe.take().unwrap();
    if loupe.placeholder.is_none() {
        loupe.placeholder = app.request_preview(cur);
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let (resp, _) = loupe.show(ui, rect, &app.dev, &settings, false, false, None, true);
        app.view_mem.insert(loupe.id, (loupe.zoom, loupe.center));
        if resp.clicked() && !resp.dragged() {
            loupe.toggle_zoom(resp.interact_pointer_pos());
        }
        if resp.double_clicked() {
            app.dlg.pending_module = Some(Module::Develop);
        }
        resp.context_menu(|ui| photo_menu(app, ui));
        let pt = ui.painter();
        pt.text(rect.left_top() + vec2(12.0, 10.0), Align2::LEFT_TOP, &info, FontId::proportional(12.0), TEXT());
        widgets::stars(pt, rect.left_top() + vec2(18.0, 34.0), rating, 8.0);
    });
    app.loupe = Some(loupe);
    if let Some(m) = app.dlg.pending_module.take() {
        app.set_module(m);
    }
}

fn compare(app: &mut App, ui: &mut Ui, rect: Rect) {
    let Some((mut a, mut b)) = app.compare.take() else {
        app.lib_view = LibView::Grid;
        return;
    };
    let half = rect.width() * 0.5;
    let ra = Rect::from_min_size(rect.min + vec2(0.0, 26.0), vec2(half - 1.0, rect.height() - 26.0));
    let rb = Rect::from_min_size(rect.min + vec2(half + 1.0, 26.0), vec2(half - 1.0, rect.height() - 26.0));
    let sa = app.cat.get(a.id).map(|p| p.settings()).unwrap_or_default();
    let sb = app.cat.get(b.id).map(|p| p.settings()).unwrap_or_default();
    let mut swap = false;
    let mut next_cand: i64 = 0;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(mono(tr!("선택", "Select")).color(TEXT()));
            ui.label(egui::RichText::new(app.cat.get(a.id).map(|p| p.display_name()).unwrap_or_default()).color(TEXT_WEAK()));
            ui.add_space(half - 220.0);
            ui.label(mono(tr!("후보", "Candidate")).color(TEXT()));
            ui.label(egui::RichText::new(app.cat.get(b.id).map(|p| p.display_name()).unwrap_or_default()).color(TEXT_WEAK()));
            if ui.small_button("◀").clicked() {
                next_cand = -1;
            }
            if ui.small_button("▶").clicked() {
                next_cand = 1;
            }
            if ui.small_button(tr!("⇄ 교체", "⇄ Swap")).clicked() {
                swap = true;
            }
        });
        let (r1, _) = a.show(ui, ra, &app.dev, &sa, false, false, None, true);
        let (r2, _) = b.show(ui, rb, &app.dev, &sb, false, false, None, true);
        // Synchronized zoom
        if r1.clicked() {
            a.toggle_zoom(r1.interact_pointer_pos());
            b.zoom = a.zoom;
            b.center = a.center;
        }
        if r2.clicked() {
            b.toggle_zoom(r2.interact_pointer_pos());
            a.zoom = b.zoom;
            a.center = b.center;
        }
        if r1.dragged() {
            b.center = a.center;
        }
        if r2.dragged() {
            a.center = b.center;
        }
        ui.painter().rect_stroke(ra, 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
    });
    if swap {
        std::mem::swap(&mut a, &mut b);
        a.slot = 0;
        b.slot = 1;
        app.current = Some(a.id);
    }
    if next_cand != 0
        && let Some(i) = app.visible.iter().position(|x| *x == b.id) {
            let ni = (i as i64 + next_cand).rem_euclid(app.visible.len() as i64) as usize;
            let mut nid = app.visible[ni];
            if nid == a.id {
                nid = app.visible[((ni as i64 + next_cand).rem_euclid(app.visible.len() as i64)) as usize];
            }
            if let Some(p) = app.cat.get(nid) {
                let mut nb = super::viewer::Viewer::new(nid, p.path.clone(), p.meta.orientation, 1, &app.dev);
                nb.placeholder = app.request_preview(nid);
                b = nb;
            }
        }
    app.compare = Some((a, b));
}

fn survey(app: &mut App, ui: &mut Ui, rect: Rect) {
    let ids: Vec<PhotoId> = if app.selected.len() > 1 { app.selected.clone() } else { app.visible.iter().take(12).copied().collect() };
    let n = ids.len().max(1);
    let aspect = rect.width() / rect.height();
    let cols = ((n as f32 * aspect).sqrt().ceil() as usize).clamp(1, n);
    let rows = n.div_ceil(cols);
    let cw = rect.width() / cols as f32;
    let ch = rect.height() / rows as f32;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.painter().rect_filled(rect, 0.0, CANVAS());
        let mut drop = None;
        for (i, id) in ids.iter().enumerate() {
            let r = Rect::from_min_size(rect.min + vec2((i % cols) as f32 * cw, (i / cols) as f32 * ch), vec2(cw, ch)).shrink(6.0);
            let tex = app.request_preview(*id);
            let resp = ui.interact(r, egui::Id::new(("survey", id)), Sense::click());
            if let Some(t) = tex {
                let ir = super::viewer::fit_rect(r.shrink(4.0), t.size_vec2());
                ui.painter().image(t.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                if app.current == Some(*id) {
                    ui.painter().rect_stroke(ir.expand(2.0), 0.0, Stroke::new(1.5, STRONG()), StrokeKind::Outside);
                }
            }
            if let Some(p) = app.cat.get(*id) {
                widgets::stars(ui.painter(), r.left_bottom() + vec2(10.0, -10.0), p.rating, 7.0);
            }
            if resp.clicked() {
                app.current = Some(*id);
            }
            if resp.hovered() {
                let xr = Rect::from_min_size(r.right_top() + vec2(-22.0, 4.0), vec2(18.0, 18.0));
                let xresp = ui.interact(xr, egui::Id::new(("survey_x", id)), Sense::click());
                ui.painter().text(xr.center(), Align2::CENTER_CENTER, "×", FontId::proportional(14.0), TEXT());
                if xresp.clicked() {
                    drop = Some(*id);
                }
            }
            resp.context_menu(|ui| photo_menu(app, ui));
        }
        if let Some(d) = drop {
            app.toggle_select(d);
        }
    });
}

fn right_panel(app: &mut App, ui: &mut Ui) {
    if let Some(l) = &app.loupe
        && let Some(h) = &l.hist {
            let mut clip = false;
            widgets::histogram(ui, Some(h), &mut clip, "");
            ui.add_space(4.0);
        }
    section(ui, tr!("빠른 현상", "Quick Develop"), true, |ui| quick_develop(app, ui));
    section(ui, tr!("키워드", "Keywords"), true, |ui| keywords(app, ui));
    section(ui, tr!("메타데이터", "Metadata"), true, |ui| metadata(app, ui));
}

fn quick_develop(app: &mut App, ui: &mut Ui) {
    type Getter = fn(&mut crate::develop::settings::DevelopSettings) -> &mut f32;
    let rows: [(&str, Getter, f32, f32); 10] = [
        (tr!("색온도", "Temp"), |s| &mut s.temp, 5.0, 20.0),
        (tr!("색조", "Tint"), |s| &mut s.tint, 5.0, 20.0),
        (tr!("노출", "Exposure"), |s| &mut s.exposure, 1.0 / 3.0, 1.0),
        (tr!("대비", "Contrast"), |s| &mut s.contrast, 5.0, 20.0),
        (tr!("하이라이트", "Highlights"), |s| &mut s.highlights, 5.0, 20.0),
        (tr!("섀도", "Shadows"), |s| &mut s.shadows, 5.0, 20.0),
        (tr!("화이트", "Whites"), |s| &mut s.whites, 5.0, 20.0),
        (tr!("블랙", "Blacks"), |s| &mut s.blacks, 5.0, 20.0),
        (tr!("명료도", "Clarity"), |s| &mut s.clarity, 5.0, 20.0),
        (tr!("바이브런스", "Vibrance"), |s| &mut s.vibrance, 5.0, 20.0),
    ];
    let targets = app.targets();
    for (name, get, small, big) in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Label column matches the develop slider width; the four buttons share the remaining width
            let (lr, _) = ui.allocate_exact_size(vec2(widgets::SLIDER_LABEL_W, 20.0), Sense::hover());
            ui.painter().text(lr.left_center(), Align2::LEFT_CENTER, name, FontId::proportional(12.0), TEXT_WEAK());
            let bw = ((ui.available_width() - 12.0) / 4.0).floor();
            let mut delta = 0.0;
            for (lbl, d, tip) in [("◀◀", -big, tr!("크게 줄이기", "Decrease a lot")), ("◀", -small, tr!("줄이기", "Decrease")), ("▶", small, tr!("늘리기", "Increase")), ("▶▶", big, tr!("크게 늘리기", "Increase a lot"))] {
                if ui.add(egui::Button::new(egui::RichText::new(lbl).size(10.0)).min_size(vec2(bw, 20.0))).on_hover_text(tip).clicked() {
                    delta = d;
                }
            }
            if delta != 0.0 {
                for id in &targets {
                    if let Some(p) = app.cat.get(*id) {
                        let mut s = p.settings();
                        let v = get(&mut s);
                        *v = (*v + delta).clamp(if name == tr!("노출", "Exposure") { -5.0 } else { -100.0 }, if name == tr!("노출", "Exposure") { 5.0 } else { 100.0 });
                        let _ = app.cat.set_develop(*id, &s);
                        let _ = app.cat.push_history(*id, &trf!("빠른 현상: {name}", "Quick Develop: {name}"), &s);
                    }
                }
            }
        });
    }
    ui.add_space(4.0);
    match widgets::button_row(ui, &[(tr!("자동 톤", "Auto Tone"), true, tr!("히스토그램 기반 자동 노출·톤", "Auto exposure·tone from the histogram")), (tr!("모두 초기화", "Reset all"), true, "")]) {
        Some(0) => {
            for id in &targets {
                super::develop::auto_tone_photo(app, *id);
            }
        }
        Some(1) => {
            for id in &targets {
                if let Some(p) = app.cat.get(*id) {
                    let s = crate::develop::settings::DevelopSettings::default_for(p.is_raw);
                    let _ = app.cat.set_develop(*id, &s);
                }
            }
        }
        _ => {}
    }
}

fn keywords(app: &mut App, ui: &mut Ui) {
    let targets = app.targets();
    let Some(cur) = app.current.and_then(|c| app.cat.get(c)).map(|p| p.keywords.clone()) else {
        return;
    };
    ui.horizontal_wrapped(|ui| {
        for k in &cur {
            let r = ui.add(egui::Button::new(format!("{k}  ×")).small());
            if r.clicked() {
                let _ = app.cat.remove_keyword(&targets, k);
            }
        }
        if cur.is_empty() {
            ui.label(egui::RichText::new(tr!("키워드 없음", "No keywords")).color(TEXT_DIM()));
        }
    });
    let mut submit = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let w = ui.available_width() - 48.0;
        let r = ui.add_sized(vec2(w, 22.0), egui::TextEdit::singleline(&mut app.dlg.keyword_input).hint_text(tr!("키워드 추가 (쉼표로 구분)", "Add keywords (comma separated)")));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        if ui.add(egui::Button::new(tr!("추가", "Add")).min_size(vec2(44.0, 22.0))).clicked() {
            submit = true;
        }
    });
    if submit {
        let kws: Vec<String> = app.dlg.keyword_input.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if !kws.is_empty() {
            let _ = app.cat.add_keywords(&targets, &kws);
        }
        app.dlg.keyword_input.clear();
    }
    let all = app.cat.keyword_counts();
    if !all.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(tr!("자주 쓰는 키워드", "Frequent keywords")).size(11.0).color(TEXT_DIM()));
        ui.horizontal_wrapped(|ui| {
            let mut sorted = all.clone();
            sorted.sort_by(|a, b| b.1.cmp(&a.1));
            for (k, n) in sorted.into_iter().take(24) {
                if ui.add(egui::Button::new(egui::RichText::new(format!("{k} {n}")).size(11.0)).small()).clicked() {
                    let _ = app.cat.add_keywords(&targets, &[k]);
                }
            }
        });
    }
}

fn metadata(app: &mut App, ui: &mut Ui) {
    let Some(id) = app.current else { return };
    let Some(p) = app.cat.get(id).cloned() else { return };
    let tkey = egui::Id::new(("meta_text", id));
    let (mut title, mut caption): (String, String) = ui.data(|d| d.get_temp(tkey)).unwrap_or((p.title.clone(), p.caption.clone()));
    // Title and caption use the same label column as the info table below (inputs get an exact width so they do not touch the panel edge)
    const LW: f32 = 52.0;
    let field = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui, f32) -> egui::Response| -> egui::Response {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (lr, _) = ui.allocate_exact_size(vec2(LW, 22.0), Sense::hover());
            ui.painter().text(lr.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(11.5), TEXT_WEAK());
            let w = ui.available_width();
            add(ui, w)
        })
        .inner
    };
    let r1 = field(ui, tr!("제목", "Title"), &mut |ui, w| ui.add_sized(vec2(w, 22.0), egui::TextEdit::singleline(&mut title)));
    let r2 = field(ui, tr!("캡션", "Caption"), &mut |ui, w| ui.add_sized(vec2(w, 44.0), egui::TextEdit::multiline(&mut caption)));
    if r1.lost_focus() || r2.lost_focus() {
        let _ = app.cat.set_text(id, &title, &caption);
        ui.data_mut(|d| d.remove::<(String, String)>(tkey));
    } else if r1.changed() || r2.changed() {
        ui.data_mut(|d| d.insert_temp(tkey, (title, caption)));
    }
    ui.add_space(6.0);
    let m = &p.meta;
    let mut rows: Vec<(&str, String)> = vec![
        (tr!("파일", "File"), p.display_name()),
        (tr!("폴더", "Folder"), p.folder.to_string_lossy().to_string()),
        (tr!("크기", "Size"), fmt_size(p.file_size)),
    ];
    if m.width > 0 {
        rows.push((tr!("해상도", "Resolution"), format!("{} × {}", m.width, m.height)));
    }
    rows.push((tr!("촬영", "Capture"), m.capture_time.clone().unwrap_or_default()));
    rows.push((tr!("카메라", "Camera"), [m.make.clone(), m.model.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ")));
    rows.push((tr!("렌즈", "Lens"), m.lens.clone().unwrap_or_default()));
    rows.push((tr!("노출", "Exposure"), m.summary()));
    if let Some((la, lo)) = m.gps {
        rows.push((tr!("위치", "Location"), format!("{la:.5}, {lo:.5}")));
    }
    if p.missing {
        rows.push((tr!("상태", "Status"), tr!("원본 파일을 찾을 수 없음", "Original file not found").into()));
    }
    egui::Grid::new("meta_grid").num_columns(2).min_col_width(52.0).spacing(vec2(8.0, 3.0)).show(ui, |ui| {
        for (k, v) in rows {
            if v.is_empty() {
                continue;
            }
            ui.label(egui::RichText::new(k).color(TEXT_WEAK()).size(11.5));
            ui.add(egui::Label::new(egui::RichText::new(v).size(11.5)).wrap());
            ui.end_row();
        }
    });
}

/// Filmstrip filter menu. Shares state with the library filter.
fn film_filter_menu(app: &mut App, ui: &mut Ui) {
    let f = &app.filter;
    let mut parts: Vec<String> = Vec::new();
    match f.edited {
        Some(true) => parts.push(tr!("편집됨", "Edited").into()),
        Some(false) => parts.push(tr!("편집 안 됨", "Not edited").into()),
        None => {}
    }
    match f.exported {
        Some(true) => parts.push(tr!("내보냄", "Exported").into()),
        Some(false) => parts.push(tr!("내보내지 않음", "Not exported").into()),
        None => {}
    }
    match f.flag {
        FlagFilter::Picked => parts.push(tr!("픽", "Pick").into()),
        FlagFilter::Unflagged => parts.push(tr!("플래그 없음", "Unflagged").into()),
        FlagFilter::Rejected => parts.push(tr!("거부", "Reject").into()),
        FlagFilter::NotRejected => parts.push(tr!("거부 제외", "Hide rejected").into()),
        FlagFilter::All => {}
    }
    if f.rating > 0 {
        parts.push(trf!("별 {}{}", "{}{} stars",
            f.rating,
            match f.rating_op {
                RatingOp::AtLeast => "+",
                RatingOp::Exactly => "",
                RatingOp::AtMost => "-",
            }
        ));
    }
    if !f.labels.is_empty() {
        parts.push(f.labels.iter().map(|l| l.name()).collect::<Vec<_>>().join("·"));
    }
    if f.kind != KindFilter::All || !f.text.trim().is_empty() {
        parts.push(tr!("기타", "Other").into());
    }
    let active = !parts.is_empty();
    let text = if active { parts.join(", ") } else { tr!("없음", "None").into() };
    let mut nf = app.filter.clone();
    let label = egui::RichText::new(trf!("필터: {text}", "Filter: {text}")).size(10.5).color(if active { ACCENT } else { TEXT_WEAK() });
    egui::ComboBox::from_id_salt("film_filter").selected_text(label).width(190.0).height(440.0).show_ui(ui, |ui| {
        ui.set_min_width(190.0);
        if ui.selectable_label(!active, tr!("필터 끄기", "Filter off")).clicked() {
            nf = Filter::default();
        }
        ui.separator();
        ui.label(egui::RichText::new(tr!("편집", "Edit")).size(10.0).color(TEXT_DIM()));
        for (lbl, v) in [(tr!("편집됨", "Edited"), Some(true)), (tr!("편집 안 됨", "Not edited"), Some(false))] {
            if ui.selectable_label(nf.edited == v, lbl).clicked() {
                nf.edited = if nf.edited == v { None } else { v };
            }
        }
        ui.label(egui::RichText::new(tr!("내보내기", "Export")).size(10.0).color(TEXT_DIM()));
        for (lbl, v) in [(tr!("내보냄", "Exported"), Some(true)), (tr!("내보내지 않음", "Not exported"), Some(false))] {
            if ui.selectable_label(nf.exported == v, lbl).clicked() {
                nf.exported = if nf.exported == v { None } else { v };
            }
        }
        ui.label(egui::RichText::new(tr!("플래그", "Flag")).size(10.0).color(TEXT_DIM()));
        for (lbl, v) in [(tr!("픽", "Pick"), FlagFilter::Picked), (tr!("플래그 없음", "Unflagged"), FlagFilter::Unflagged), (tr!("거부", "Reject"), FlagFilter::Rejected), (tr!("거부 제외", "Hide rejected"), FlagFilter::NotRejected)] {
            if ui.selectable_label(nf.flag == v, lbl).clicked() {
                nf.flag = if nf.flag == v { FlagFilter::All } else { v };
            }
        }
        ui.label(egui::RichText::new(tr!("별점 (이상)", "Rating (at least)")).size(10.0).color(TEXT_DIM()));
        ui.horizontal(|ui| {
            for r in 1..=5u8 {
                let on = nf.rating == r && nf.rating_op == RatingOp::AtLeast;
                if ui.selectable_label(on, format!("{r}+")).on_hover_text(trf!("별 {r}개 이상", "{r}+ stars")).clicked() {
                    if on {
                        nf.rating = 0;
                    } else {
                        nf.rating = r;
                        nf.rating_op = RatingOp::AtLeast;
                    }
                }
            }
        });
        ui.label(egui::RichText::new(tr!("색상 라벨", "Color label")).size(10.0).color(TEXT_DIM()));
        ui.horizontal(|ui| {
            for l in ColorLabel::ALL.iter().skip(1) {
                let on = nf.labels.contains(l);
                let [r, g, b] = l.rgb();
                let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                ui.painter().circle_filled(rect.center(), if on { 7.0 } else { 5.0 }, Color32::from_rgb(r, g, b));
                if on {
                    ui.painter().circle_stroke(rect.center(), 8.0, Stroke::new(1.0, STRONG()));
                }
                if resp.on_hover_text(l.name()).clicked() {
                    if on {
                        nf.labels.retain(|x| x != l);
                    } else {
                        nf.labels.push(*l);
                    }
                }
            }
        });
    });
    if nf != app.filter {
        app.filter = nf;
        app.recompute_visible();
        app.cat.kv_set_json("filter", &app.filter);
        app.dlg.film_follow = true;
    }
}

pub fn filmstrip(app: &mut App, ui: &mut Ui) {
    egui::Panel::bottom("film")
        .exact_size(config::FILMSTRIP_HEIGHT)
        .frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin::symmetric(6, 4)))
        .show(ui, |ui| film_horizontal(app, ui));
}

/// Horizontal preview strip (bottom panel or card): source and filter on top, cells laid out sideways
pub fn film_horizontal(app: &mut App, ui: &mut Ui) {
    {
        {
            let src_name = match &app.source {
                Source::All => tr!("모든 사진", "All Photographs").to_string(),
                Source::Quick => tr!("빠른 컬렉션", "Quick Collection").into(),
                Source::LastImport => tr!("이전 가져오기", "Previous Import").into(),
                Source::Missing => tr!("누락된 사진", "Missing Photos").into(),
                Source::Folder(f) => f.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                Source::Collection(c) => app.cat.collections.iter().find(|x| x.id == *c).map(|x| x.name.clone()).unwrap_or_default(),
                Source::Person(pid) => app.cat.persons.iter().find(|x| x.id == *pid).map(super::people_ui::person_label).unwrap_or_default(),
            };
            let pos = app.current.and_then(|c| app.visible.iter().position(|x| *x == c)).map(|i| i + 1).unwrap_or(0);
            let name = app.current.and_then(|c| app.cat.get(c)).map(|p| p.display_name()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(mono(format!("{src_name} · {pos}/{} · {name}", app.visible.len())).size(10.0).color(TEXT_WEAK()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    film_filter_menu(app, ui);
                });
            });
            let h = ui.available_height();
            let cw = h * 1.25;
            let n = app.visible.len();
            let cur_idx = app.current.and_then(|c| app.visible.iter().position(|x| *x == c));
            let mut sa = egui::ScrollArea::horizontal().id_salt("film_scroll").auto_shrink([false, false]);
            if app.scroll_to_current || app.dlg.film_follow {
                if let Some(i) = cur_idx {
                    let vw = ui.available_width();
                    let st = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("film_off"))).unwrap_or(0.0);
                    let x = i as f32 * cw;
                    if x < st || x + cw > st + vw {
                        sa = sa.horizontal_scroll_offset((x - vw * 0.5 + cw * 0.5).max(0.0));
                    }
                }
                app.dlg.film_follow = false;
            }
            let out = sa.show_viewport(ui, |ui, vp| {
                ui.set_width(n as f32 * cw);
                ui.set_height(h);
                let first = (vp.left() / cw).floor().max(0.0) as usize;
                let last = ((vp.right() / cw).ceil() as usize + 1).min(n);
                let origin = ui.min_rect().min;
                for i in first..last {
                    let id = app.visible[i];
                    let r = Rect::from_min_size(origin + vec2(i as f32 * cw, 0.0), vec2(cw, h));
                    let resp = draw_cell(app, ui, r, id, i, true, usize::MAX);
                    let is_lib_grid = app.module == Module::Library && app.lib_view == LibView::Grid;
                    cell_interact(app, ui, &resp, id, is_lib_grid);
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("film_off"), out.state.offset.x));
        }
    }
}


/// Vertical preview strip (left panel): source and filter on top, cells below (1 to 3 columns depending on width)
pub fn film_vertical(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        let pos = app.current.and_then(|c| app.visible.iter().position(|x| *x == c)).map(|i| i + 1).unwrap_or(0);
        ui.label(mono(format!("{pos}/{}", app.visible.len())).size(10.5).color(TEXT_WEAK()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| film_filter_menu(app, ui));
    });
    ui.add_space(2.0);
    let w = ui.available_width();
    let cols = ((w / 118.0).floor() as usize).clamp(1, 4);
    let cw = (w / cols as f32).floor();
    let ch = (cw * 0.8).floor();
    let n = app.visible.len();
    let rows = n.div_ceil(cols);
    let cur_idx = app.current.and_then(|c| app.visible.iter().position(|x| *x == c));
    let mut sa = egui::ScrollArea::vertical().id_salt("film_v").auto_shrink([false, false]);
    if app.scroll_to_current || app.dlg.film_follow {
        if let Some(i) = cur_idx {
            let vh = ui.available_height();
            let st = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("film_v_off"))).unwrap_or(0.0);
            let y = (i / cols) as f32 * ch;
            if y < st || y + ch > st + vh {
                sa = sa.vertical_scroll_offset((y - vh * 0.5 + ch * 0.5).max(0.0));
            }
        }
        app.dlg.film_follow = false;
    }
    let out = sa.show_viewport(ui, |ui, vp| {
        ui.set_width(cw * cols as f32);
        ui.set_height(rows as f32 * ch);
        let first = (vp.top() / ch).floor().max(0.0) as usize;
        let last = ((vp.bottom() / ch).ceil() as usize + 1).min(rows);
        let origin = ui.min_rect().min;
        for r in first..last {
            for c in 0..cols {
                let i = r * cols + c;
                if i >= n {
                    break;
                }
                let id = app.visible[i];
                let rect = Rect::from_min_size(origin + vec2(c as f32 * cw, r as f32 * ch), vec2(cw, ch));
                let resp = draw_cell(app, ui, rect, id, i, true, cols);
                cell_interact(app, ui, &resp, id, false);
            }
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("film_v_off"), out.state.offset.y));
}
