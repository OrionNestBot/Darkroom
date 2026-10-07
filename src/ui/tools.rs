//! Library tools: batch rename, slideshow, shortcut help, command palette, merges, print, relink, sidecars, stacks.

use super::app::*;
use super::theme::*;
use crate::catalog::PhotoId;
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Ui, pos2, vec2};
use std::path::PathBuf;
use std::time::Instant;

// ───────────────────────── Batch rename ─────────────────────────

pub struct RenameDialog {
    pub ids: Vec<PhotoId>,
    pub template: String,
    pub custom: String,
    pub start: u32,
    pub report: Option<String>,
}

pub fn open_rename(app: &mut App) {
    let ids: Vec<PhotoId> = app.targets().into_iter().filter(|id| app.cat.get(*id).map(|p| p.master.is_none()).unwrap_or(false)).collect();
    if ids.is_empty() {
        app.toast(tr!("이름을 바꿀 사진을 선택하세요 (가상 사본 제외)", "Select photos to rename (virtual copies excluded)"));
        return;
    }
    app.rename = Some(RenameDialog { ids, template: "{date}_{seq:3}".into(), custom: String::new(), start: 1, report: None });
}

fn new_name(app: &App, d: &RenameDialog, idx: usize, id: PhotoId) -> Option<(PathBuf, PathBuf)> {
    let p = app.cat.get(id)?;
    let ex = crate::export::ExportSettings { rename: true, template: d.template.clone(), custom_text: d.custom.clone(), ..Default::default() };
    let stem = crate::export::file_stem(p, &ex, d.start + idx as u32);
    let ext = p.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let np = p.path.with_file_name(format!("{stem}.{ext}"));
    Some((p.path.clone(), np))
}

pub fn rename_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.rename.take() else { return };
    let open = true;
    let mut go = false;
    let mut close = false;
    let plan: Vec<(PathBuf, PathBuf)> = d.ids.iter().enumerate().filter_map(|(i, id)| new_name(app, &d, i, *id)).collect();
    // Conflict: duplicate new names, or another file already exists
    let mut seen = std::collections::HashSet::new();
    let conflicts: Vec<bool> = plan
        .iter()
        .map(|(o, n)| {
            let dup = !seen.insert(n.to_string_lossy().to_lowercase());
            dup || (n != o && n.exists() && !plan.iter().any(|(o2, _)| o2 == n))
        })
        .collect();
    let n_conf = conflicts.iter().filter(|c| **c).count();
    let has_report = d.report.is_some();
    let n_ids = d.ids.len();
    let mut close_f = false;
    let (_, _, close_x) = super::form::modal(
        ctx,
        "rename",
        tr!("이름 일괄 변경", "Batch Rename"),
        &trf!("{n_ids}장의 원본 파일 이름을 바꿉니다 (같은 이름의 .xmp 사이드카도 함께)", "Renames the original files of {n_ids} photos (and their .xmp sidecars)"),
        vec2(640.0, 600.0),
        |ui| {
            if let Some(r) = &d.report {
                super::form::card(ui, tr!("결과", "Result"), "", |ui| {
                    ui.label(egui::RichText::new(r).color(TEXT()));
                });
                return;
            }
            super::form::card(ui, tr!("형식", "Format"), "", |ui| {
                super::form::row(ui, tr!("이름 형식", "Name format"), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.template).desired_width(240.0));
                    ui.menu_button(tr!("토큰 ▼", "Tokens ▼"), |ui| {
                        for (t, desc) in crate::export::NAME_TOKENS {
                            if ui.button(format!("{t}  {}", crate::i18n::t(desc))).clicked() {
                                d.template.push_str(t);
                                ui.close();
                            }
                        }
                    });
                });
                super::form::row(ui, tr!("사용자 텍스트", "Custom text"), |ui| ui.add(egui::TextEdit::singleline(&mut d.custom).desired_width(160.0)));
                super::form::row(ui, tr!("시작 번호", "Start number"), |ui| ui.add(egui::DragValue::new(&mut d.start).range(0..=999999)));
            });
            super::form::card(ui, tr!("미리보기", "Preview"), if n_conf > 0 { tr!("빨간 이름은 충돌 — 형식에 {seq} 토큰을 넣으세요", "Red names conflict — add a {seq} token to the format") } else { "" }, |ui| {
                egui::ScrollArea::vertical().id_salt("rename_list").max_height(220.0).show(ui, |ui| {
                    for (i, (o, n)) in plan.iter().enumerate().take(200) {
                        ui.horizontal(|ui| {
                            ui.label(mono(o.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()).size(11.0).color(TEXT_WEAK()));
                            ui.label(egui::RichText::new("→").color(TEXT_DIM()));
                            let col = if conflicts[i] { ACCENT } else { TEXT() };
                            ui.label(mono(n.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()).size(11.0).color(col));
                        });
                    }
                });
            });
        },
        |ui| {
            if n_conf > 0 && !has_report {
                super::form::footer_note(ui, &trf!("충돌 {n_conf}건", "{n_conf} conflicts"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if has_report {
                    if super::form::primary(ui, tr!("닫기", "Close"), true).clicked() {
                        close_f = true;
                    }
                } else {
                    if super::form::primary(ui, tr!("이름 바꾸기", "Rename"), n_conf == 0).clicked() {
                        go = true;
                    }
                    if super::form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                        close_f = true;
                    }
                }
            });
        },
    );
    if close_x || close_f {
        close = true;
    }
    if go {
        let mut ok = 0;
        let mut fail = Vec::new();
        for ((o, n), id) in plan.iter().zip(d.ids.clone()) {
            if o == n {
                continue;
            }
            match std::fs::rename(o, n) {
                Ok(()) => {
                    let ox = o.with_extension("xmp");
                    if ox.exists() {
                        let _ = std::fs::rename(&ox, n.with_extension("xmp"));
                    }
                    let _ = app.cat.update_path(id, n);
                    ok += 1;
                }
                Err(e) => fail.push(format!("{}: {e}", o.display())),
            }
        }
        app.visible_dirty = true;
        d.report = Some(if fail.is_empty() { trf!("{ok}장 이름을 바꿨습니다", "Renamed {ok} photos") } else { trf!("{ok}장 완료, {}건 실패\n{}", "{ok} done, {} failed\n{}", fail.len(), fail.join("\n")) });
    }
    if open && !close {
        app.rename = Some(d);
    }
}

// ───────────────────────── Slideshow ─────────────────────────

pub struct Slideshow {
    pub ids: Vec<PhotoId>,
    pub idx: usize,
    pub started: Instant,
    pub seconds: f32,
    pub paused: bool,
    pub prev_tex: Option<egui::TextureHandle>,
    pub fade_from: Option<(egui::TextureHandle, Instant)>,
}

pub fn start_slideshow(app: &mut App) {
    let ids: Vec<PhotoId> = if app.selected.len() > 1 { app.selected.clone() } else { app.visible.clone() };
    if ids.is_empty() {
        return;
    }
    let idx = app.current.and_then(|c| ids.iter().position(|x| *x == c)).unwrap_or(0);
    app.slideshow = Some(Slideshow { ids, idx, started: Instant::now(), seconds: 4.0, paused: false, prev_tex: None, fade_from: None });
}

pub fn slideshow_ui(app: &mut App, ctx: &egui::Context) -> bool {
    let Some(mut ss) = app.slideshow.take() else { return false };
    let mut exit = false;
    let (esc, left, right, space, up, down) = ctx.input(|i| {
        (i.key_pressed(Key::Escape), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight), i.key_pressed(Key::Space), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown))
    });
    if esc {
        exit = true;
    }
    if space {
        ss.paused = !ss.paused;
        ss.started = Instant::now();
    }
    if up {
        ss.seconds = (ss.seconds + 1.0).min(30.0);
    }
    if down {
        ss.seconds = (ss.seconds - 1.0).max(1.0);
    }
    let mut step = 0i64;
    if right || (!ss.paused && ss.started.elapsed().as_secs_f32() > ss.seconds) {
        step = 1;
    }
    if left {
        step = -1;
    }
    if step != 0 {
        if let Some(t) = &ss.prev_tex {
            ss.fade_from = Some((t.clone(), Instant::now()));
        }
        let n = ss.ids.len() as i64;
        ss.idx = ((ss.idx as i64 + step).rem_euclid(n)) as usize;
        ss.started = Instant::now();
    }
    let id = ss.ids[ss.idx];
    // Request the next photo's preview in advance
    let next = ss.ids[(ss.idx + 1) % ss.ids.len()];
    let _ = app.request_preview(next);
    let tex = app.request_preview(id);
    if let Some(t) = &tex {
        ss.prev_tex = Some(t.clone());
    }
    egui::Area::new(egui::Id::new("slideshow")).fixed_pos(pos2(0.0, 0.0)).order(egui::Order::Foreground).show(ctx, |ui| {
        let rect = ctx.content_rect();
        let resp = ui.allocate_rect(rect, Sense::click());
        let p = ui.painter();
        p.rect_filled(rect, 0.0, Color32::BLACK);
        let draw = |t: &egui::TextureHandle, alpha: f32| {
            let r = super::viewer::fit_rect(rect.shrink(24.0), t.size_vec2());
            p.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha((alpha * 255.0) as u8));
        };
        let fade = ss.fade_from.as_ref().map(|(_, t0)| (t0.elapsed().as_secs_f32() / 0.45).min(1.0)).unwrap_or(1.0);
        if let Some((ft, _)) = &ss.fade_from
            && fade < 1.0 {
                draw(ft, 1.0 - fade);
            }
        match &tex {
            Some(t) => draw(t, fade),
            None => {
                p.text(rect.center(), Align2::CENTER_CENTER, tr!("불러오는 중…", "Loading…"), FontId::proportional(14.0), Color32::from_gray(120));
            }
        }
        if fade >= 1.0 {
            ss.fade_from = None;
        }
        let name = app.cat.get(id).map(|p| p.display_name()).unwrap_or_default();
        let info = trf!("{} / {}  ·  {name}  ·  {:.0}초{}  ·  ←/→ 이동 · Space 일시정지 · ↑/↓ 간격 · Esc 끝내기", "{} / {}  ·  {name}  ·  {:.0}s{}  ·  ←/→ move · Space pause · ↑/↓ interval · Esc exit", ss.idx + 1, ss.ids.len(), ss.seconds, if ss.paused { tr!(" (일시정지)", " (paused)") } else { "" });
        let hover = ui.input(|i| i.pointer.time_since_last_movement() < 2.0);
        if hover {
            p.text(rect.center_bottom() - vec2(0.0, 18.0), Align2::CENTER_BOTTOM, info, FontId::proportional(12.0), Color32::from_gray(160));
        }
        if resp.clicked() {
            ss.started = Instant::now() - std::time::Duration::from_secs_f32(ss.seconds + 1.0);
        }
    });
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
    if !exit {
        app.slideshow = Some(ss);
    }
    true
}

// ───────────────────────── Shortcut help · command palette ─────────────────────────

pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("공통", "G / D", "라이브러리 그리드 / 현상"),
    ("공통", "E / C / N", "루페 / 비교 / 서베이"),
    ("공통", "Ctrl+K", "명령 찾기"),
    ("공통", "?", "단축키 도움말"),
    ("공통", "Ctrl+Shift+I / E", "가져오기 / 내보내기"),
    ("공통", "Tab / Shift+Tab", "옆 패널 / 모든 패널 숨기기"),
    ("공통", "F11", "슬라이드쇼"),
    ("공통", "F6", "미리보기 줄 위치 (왼쪽·아래·숨김)"),
    ("공통", "Ctrl+M", "파노라마 병합"),
    ("공통", "Ctrl+Alt+Shift+M", "총 노출 맞추기"),
    ("평가", "0–5", "별점"),
    ("평가", "P / X / U", "선택 / 거부 / 깃발 없음"),
    ("평가", "6–9", "색상 라벨"),
    ("평가", "B", "빠른 컬렉션"),
    ("라이브러리", "F2", "이름 일괄 변경"),
    ("라이브러리", "Ctrl+P", "인쇄 · 레이아웃 (PDF)"),
    ("라이브러리", "Ctrl+S", "XMP 사이드카에 저장"),
    ("라이브러리", "Ctrl+G", "스택으로 묶기"),
    ("라이브러리", "Ctrl+Shift+G", "스택 해제"),
    ("라이브러리", "S", "스택 펼치기 / 접기"),
    ("라이브러리", "Ctrl+'", "가상 사본"),
    ("현상", "R", "자르기·회전"),
    ("현상", "Q", "스팟 제거"),
    ("현상", "Shift+W", "마스크"),
    ("현상", "Shift+F", "가리기 (얼굴 모자이크)"),
    ("현상", "W", "화이트 밸런스 스포이드"),
    ("현상", "\\ / Y / Shift+Y", "이전 보기 / 나란히 / 분할"),
    ("현상", "Shift+R", "참조 보기 (다른 사진을 왼쪽에 고정)"),
    ("현상", "S", "소프트 교정 (인쇄 미리 보기)"),
    ("현상", "Alt + 슬라이더", "끄는 동안 클리핑 표시"),
    ("현상", "J / O", "클리핑 / 마스크 오버레이"),
    ("현상", "Z / Space", "100% 확대 전환"),
    ("현상", "Ctrl+Z / Ctrl+Shift+Z", "실행 취소 / 다시 실행"),
    ("현상", "Ctrl+Shift+C / V", "설정 복사 / 붙여넣기"),
    ("현상", "Ctrl+Alt+V", "이전 사진 설정 적용"),
    ("현상", "Shift+A", "자동 톤"),
    ("현상", "[ / ]", "브러시·스팟 크기"),
];

pub fn shortcuts_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_shortcuts {
        return;
    }
    let mut done = false;
    let (_, _, close) = super::form::modal(
        ctx,
        "shortcuts",
        tr!("단축키", "Shortcuts"),
        tr!("Ctrl+K: 명령 찾기 — 메뉴에 없는 기능도 이름으로 실행", "Ctrl+K: command palette — run features by name, even ones not in a menu"),
        vec2(720.0, 640.0),
        |ui| {
            egui::ScrollArea::vertical().id_salt("sc_body").auto_shrink([false, false]).show(ui, |ui| {
                // Each group once, in first-appearance order (removing only consecutive duplicates would let a group reappear mid-list)
                let groups: Vec<&str> = SHORTCUTS.iter().map(|x| x.0).fold(Vec::new(), |mut v, g| {
                    if !v.contains(&g) {
                        v.push(g);
                    }
                    v
                });
                ui.columns(2, |cols| {
                    for (gi, g) in groups.iter().enumerate() {
                        let ui = &mut cols[gi % 2];
                        super::form::card(ui, crate::i18n::t(g), "", |ui| {
                            for (_, k, d) in SHORTCUTS.iter().filter(|x| x.0 == *g) {
                                // Fixed key column; descriptions wrap within their column
                                ui.horizontal_top(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(132.0, 20.0), Sense::hover());
                                    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, crate::i18n::t(k), FontId::monospace(11.5), STRONG());
                                    ui.add(egui::Label::new(egui::RichText::new(crate::i18n::t(d)).color(TEXT_WEAK())).wrap());
                                });
                            }
                        });
                    }
                });
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if super::form::primary(ui, tr!("닫기", "Close"), true).clicked() {
                    done = true;
                }
            });
        },
    );
    let open = !(close || done);
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        app.show_shortcuts = false;
    }
}

/// Command palette entry: (name, action)
fn commands() -> Vec<(&'static str, fn(&mut App))> {
    vec![
        (tr!("가져오기…", "Import…"), |a| a.dlg.open_import(&a.prefs.clone())),
        (tr!("Lightroom 카탈로그 가져오기…", "Import Lightroom catalog…"), |a| super::lrcat_import::open(a)),
        (tr!("Lightroom 프리셋 가져오기…", "Import Lightroom presets…"), |a| super::dialogs::open_lr_import(a)),
        (tr!("내보내기…", "Export…"), |a| super::dialogs::open_export(a)),
        (tr!("이름 일괄 변경…", "Batch rename…"), open_rename),
        (tr!("슬라이드쇼", "Slideshow"), start_slideshow),
        (tr!("인쇄 · 레이아웃 (PDF)…", "Print · Layout (PDF)…"), open_print),
        (tr!("HDR 병합", "HDR merge"), start_hdr),
        (tr!("파노라마 병합 (Ctrl+M)…", "Panorama merge (Ctrl+M)…"), open_pano),
        (tr!("총 노출 맞추기 (Ctrl+Alt+Shift+M)", "Match total exposures (Ctrl+Alt+Shift+M)"), super::develop::match_exposures),
        (tr!("향상 (AI 노이즈 감소 · 해상도 2배)…", "Enhance (AI Denoise · Super Resolution 2×)…"), super::enhance::open),
        (tr!("환경설정…", "Preferences…"), |a| super::dialogs::open_prefs(a, "일반")),
        (tr!("참조 보기 (Shift+R)", "Reference view (Shift+R)"), super::develop::toggle_reference),
        (tr!("소프트 교정 (S)", "Soft proofing (S)"), |a| {
            if a.module != Module::Develop {
                a.set_module(Module::Develop);
            }
            super::proof_ui::toggle(a)
        }),
        (tr!("새 카탈로그…", "New catalog…"), super::catalog_ui::open_new),
        (tr!("인물 찾기 (얼굴로 사진 묶기)", "Find people (group photos by face)"), super::people_ui::start_scan),
        (tr!("카탈로그 백업", "Back up catalog"), |a| super::catalog_ui::start_backup(a, false)),
        (tr!("워터마크 만들기 · 편집…", "Create · edit watermarks…"), |a| super::dialogs::open_prefs(a, "워터마크")),
        (tr!("미리보기 줄 위치 바꾸기 (F6)", "Change filmstrip position (F6)"), |a| {
            a.prefs.film_pos = match a.prefs.film_pos {
                FilmPos::Left => FilmPos::Bottom,
                FilmPos::Bottom => FilmPos::Hidden,
                FilmPos::Hidden => FilmPos::Left,
            };
            let fp = a.prefs.film_pos;
            a.prefs.dev_layout.set_film(fp);
        }),
        (tr!("현상 화면 배치 잠그기 / 풀기", "Lock / unlock Develop layout"), |a| {
            a.prefs.dev_layout.locked = !a.prefs.dev_layout.locked;
            let on = a.prefs.dev_layout.locked;
            a.save_prefs();
            a.toast(if on { tr!("배치를 잠갔습니다 (위치·폭·높이 고정)", "Layout locked (position·width·height fixed)") } else { tr!("배치 잠금을 풀었습니다", "Layout unlocked") });
        }),
        (tr!("현상 화면 배치 초기화", "Reset Develop layout"), |a| {
            a.prefs.dev_layout = super::devlayout::DevLayout::default();
            a.prefs.film_pos = FilmPos::Left;
        }),
        (tr!("XMP 사이드카에 저장", "Save to XMP sidecar"), save_xmp),
        (tr!("스택으로 묶기", "Group into stack"), stack_selected),
        (tr!("스택 해제", "Unstack"), unstack_selected),
        (tr!("스택 펼치기/접기", "Expand/collapse stack"), toggle_stack),
        (tr!("촬영 시간으로 자동 스택 (10초)", "Auto-stack by capture time (10 s)"), |a| auto_stack(a, 10.0)),
        (tr!("XMP 사이드카에서 불러오기", "Read from XMP sidecar"), load_xmp),
        (tr!("라이브러리로", "Go to Library"), |a| a.set_module(Module::Library)),
        (tr!("현상으로", "Go to Develop"), |a| a.set_module(Module::Develop)),
        (tr!("환경설정", "Preferences"), |a| a.dlg.prefs_open = true),
        (tr!("단축키 도움말", "Shortcut help"), |a| a.show_shortcuts = true),
        (tr!("가상 사본 만들기", "Create virtual copy"), |a| a.virtual_copy()),
        (tr!("현상 설정 복사", "Copy Develop Settings"), |a| a.dlg.copy_settings = Some(crate::develop::settings::SettingGroups::all())),
        (tr!("현상 설정 붙여넣기", "Paste develop settings"), |a| a.paste_settings()),
        (tr!("자동 동기화 켜기/끄기", "Toggle Auto Sync"), |a| {
            a.prefs.auto_sync = !a.prefs.auto_sync;
            let on = a.prefs.auto_sync;
            a.toast(if on { tr!("자동 동기화 켬", "Auto Sync on") } else { tr!("자동 동기화 끔", "Auto Sync off") });
        }),
        (tr!("테마: 다크", "Theme: Dark"), |a| set_theme(a, ThemeKind::Dark)),
        (tr!("테마: 미드나잇", "Theme: Midnight"), |a| set_theme(a, ThemeKind::Midnight)),
        (tr!("테마: 그래파이트", "Theme: Graphite"), |a| set_theme(a, ThemeKind::Graphite)),
        (tr!("테마: 라이트", "Theme: Light"), |a| set_theme(a, ThemeKind::Light)),
        (tr!("탐색기에서 보기", "Show in Explorer"), |a| a.show_in_explorer()),
        (tr!("필터 끄기", "Filter off"), |a| {
            a.filter = Default::default();
            a.recompute_visible();
        }),
        (tr!("편집된 사진만", "Edited photos only"), |a| {
            a.filter.edited = Some(true);
            a.recompute_visible();
        }),
        (tr!("내보내지 않은 사진만", "Unexported photos only"), |a| {
            a.filter.exported = Some(false);
            a.recompute_visible();
        }),
    ]
}

fn set_theme(a: &mut App, k: ThemeKind) {
    a.prefs.theme = k;
    a.pending_theme = Some(k);
    a.save_prefs();
}

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
}

pub fn palette_ui(app: &mut App, ctx: &egui::Context) {
    let Some(mut pal) = app.palette.take() else { return };
    let q = pal.query.to_lowercase();
    let list: Vec<(&'static str, fn(&mut App))> = commands().into_iter().filter(|(n, _)| q.is_empty() || n.to_lowercase().contains(&q) || q.split_whitespace().all(|w| n.to_lowercase().contains(w))).collect();
    let mut run: Option<fn(&mut App)> = None;
    let mut close = false;
    egui::Area::new(egui::Id::new("palette")).anchor(Align2::CENTER_TOP, vec2(0.0, 80.0)).order(egui::Order::Foreground).show(ctx, |ui: &mut Ui| {
        egui::Frame::new().fill(PANEL2()).stroke(egui::Stroke::new(1.0, BORDER())).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(460.0);
            let r = ui.add(egui::TextEdit::singleline(&mut pal.query).hint_text(tr!("명령 찾기…", "Find a command…")).desired_width(440.0).font(FontId::proportional(15.0)));
            r.request_focus();
            let (down, up, enter, esc) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if down {
                pal.sel = (pal.sel + 1).min(list.len().saturating_sub(1));
            }
            if up {
                pal.sel = pal.sel.saturating_sub(1);
            }
            if esc {
                close = true;
            }
            ui.add_space(6.0);
            for (i, (n, f)) in list.iter().enumerate().take(12) {
                let sel = i == pal.sel.min(list.len().saturating_sub(1));
                if ui.selectable_label(sel, *n).clicked() || (enter && sel) {
                    run = Some(*f);
                }
            }
            if list.is_empty() {
                ui.label(egui::RichText::new(tr!("일치하는 명령이 없습니다", "No matching commands")).color(TEXT_DIM()));
            }
        });
    });
    if let Some(f) = run {
        f(app);
        return;
    }
    if !close {
        app.palette = Some(pal);
    }
}

// ───────────────────────── HDR merge ─────────────────────────

pub enum HdrMsg {
    Status(String),
    Done(Result<PathBuf, String>),
}

pub struct HdrJob {
    pub rx: crossbeam_channel::Receiver<HdrMsg>,
    pub status: String,
    pub ref_id: PhotoId,
}

pub fn start_hdr(app: &mut App) {
    if app.hdr_job.is_some() {
        app.toast(tr!("이미 HDR 병합 중입니다", "Already merging HDR"));
        return;
    }
    let ids: Vec<PhotoId> = app.multi_targets().into_iter().filter(|id| app.cat.get(*id).map(|p| p.is_raw && p.master.is_none()).unwrap_or(false)).collect();
    if ids.len() < 2 {
        app.toast(tr!("노출이 다른 RAW를 2장 이상 선택하세요", "Select 2 or more RAWs with different exposures"));
        return;
    }
    let inputs: Vec<crate::imaging::hdr::MergeInput> = ids
        .iter()
        .filter_map(|id| app.cat.get(*id))
        .map(|p| crate::imaging::hdr::MergeInput { path: p.path.clone(), orientation: p.meta.orientation })
        .collect();
    let first = app.cat.get(ids[0]).unwrap();
    let stem = first.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = first.folder.join(format!("{stem}-HDR.drhdr"));
    let mut n = 2;
    while out.exists() {
        out = first.folder.join(format!("{stem}-HDR-{n}.drhdr"));
        n += 1;
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let ctx_tx = tx.clone();
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("hdr".into())
        .spawn(move || {
            let prog = |m: &str| {
                let _ = ctx_tx.send(HdrMsg::Status(m.to_string()));
            };
            let r = crate::imaging::hdr::merge(&inputs, &out, &prog).map_err(|e| format!("{e:#}"));
            let _ = tx.send(HdrMsg::Done(r));
        })
        .expect("hdr thread");
    app.hdr_job = Some(HdrJob { rx, status: trf!("HDR 병합 준비 ({}장)", "Preparing HDR merge ({} photos)", ids.len()), ref_id: ids[0] });
}

pub fn pump_hdr(app: &mut App) {
    let Some(job) = &mut app.hdr_job else { return };
    let mut done = None;
    for m in job.rx.try_iter() {
        match m {
            HdrMsg::Status(s) => job.status = s,
            HdrMsg::Done(r) => done = Some(r),
        }
    }
    let Some(r) = done else { return };
    let ref_id = job.ref_id;
    app.hdr_job = None;
    match r {
        Ok(path) => {
            let meta = app.cat.get(ref_id).map(|p| {
                let mut m = p.meta.clone();
                m.orientation = 1; // Orientation already applied when saved
                m
            });
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let import_id = app.cat.next_import_id();
            // HDR highlights exceed 1.0, so start with highlights lowered
            let mut s = crate::develop::settings::DevelopSettings::default_for(true);
            s.highlights = -60.0;
            s.shadows = 30.0;
            match app.cat.add_photos(&[(path.clone(), size, meta.unwrap_or_default(), Some(s), vec!["HDR".to_string()])], import_id) {
                Ok(ids) => {
                    app.visible_dirty = true;
                    app.recompute_visible();
                    if let Some(id) = ids.first() {
                        app.select_single(*id);
                    }
                    app.toast(trf!("HDR 병합 완료: {}", "HDR merge done: {}", path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()));
                }
                Err(e) => app.toast_err(trf!("카탈로그 추가 실패: {e}", "Couldn't add to catalog: {e}")),
            }
        }
        Err(e) => app.toast_err(trf!("HDR 병합 실패: {e}", "HDR merge failed: {e}")),
    }
}

// ───────────────────────── Panorama merge ─────────────────────────

/// Panorama dialog (choose projection and crop)
pub struct PanoDialog {
    pub ids: Vec<PhotoId>,
    pub projection: crate::imaging::pano::Projection,
    pub auto_crop: bool,
}

pub struct PanoJob {
    pub rx: crossbeam_channel::Receiver<HdrMsg>,
    pub status: String,
    pub ref_id: PhotoId,
}

/// Selected RAWs sorted by capture time (neighbors must overlap)
pub fn open_pano(app: &mut App) {
    if app.pano_job.is_some() {
        app.toast(tr!("이미 파노라마 병합 중입니다", "Already merging a panorama"));
        return;
    }
    let mut ids: Vec<PhotoId> = app.multi_targets().into_iter().filter(|id| app.cat.get(*id).map(|p| p.is_raw && p.master.is_none()).unwrap_or(false)).collect();
    if ids.len() < 2 {
        app.toast(tr!("겹치게 찍은 RAW를 2장 이상 선택하세요", "Select 2 or more overlapping RAWs"));
        return;
    }
    ids.sort_by_key(|id| app.cat.get(*id).map(|p| (p.meta.capture_time.clone().unwrap_or_default(), p.file_name.clone())).unwrap_or_default());
    app.pano = Some(PanoDialog { ids, projection: Default::default(), auto_crop: true });
}

pub fn pano_dialog(app: &mut App, ctx: &egui::Context) {
    use crate::imaging::pano::Projection;
    let Some(mut d) = app.pano.take() else { return };
    let n = d.ids.len();
    let mut go = false;
    let mut close_f = false;
    let names: Vec<String> = d.ids.iter().filter_map(|id| app.cat.get(*id).map(|p| p.file_name.clone())).collect();
    let (_, _, close_x) = super::form::modal(
        ctx,
        "pano",
        tr!("파노라마 병합", "Panorama merge"),
        &trf!("{n}장 · 촬영 순서대로 이웃끼리 겹쳐야 합니다", "{n} photos · neighbors in capture order must overlap"),
        vec2(520.0, 360.0),
        |ui| {
            super::form::card(ui, tr!("투영", "Projection"), "", |ui| {
                super::form::row(ui, tr!("방식", "Method"), |ui| {
                    super::form::seg(ui, &mut d.projection, &[(Projection::Auto, tr!("자동", "Auto")), (Projection::Cylindrical, tr!("원통", "Cylindrical")), (Projection::Perspective, tr!("원근", "Perspective"))]);
                });
                super::form::hint(ui, tr!("자동: 회전하며 찍은 사진이면 원통(넓은 풍경), 아니면 원근(건물·평면). 원근은 직선이 곧게 남지만 넓으면 가장자리가 크게 늘어납니다.", "Auto: cylindrical (wide landscapes) if shot while rotating, otherwise perspective (buildings·flat subjects). Perspective keeps lines straight but stretches edges a lot when wide."));
                super::form::switch_row(ui, tr!("빈 테두리 자르기", "Crop empty borders"), &mut d.auto_crop, tr!("이어 붙인 뒤 생기는 빈 가장자리를 잘라냄", "Crops the empty edges left after stitching"));
            });
            super::form::card(ui, tr!("사진", "Photos"), "", |ui| {
                ui.label(egui::RichText::new(names.join(" · ")).size(11.0).color(TEXT_WEAK()));
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if super::form::primary(ui, &trf!("병합 ({n}장)", "Merge ({n} photos)"), true).clicked() {
                    go = true;
                }
                if super::form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                    close_f = true;
                }
            });
        },
    );
    if go {
        start_pano(app, &d);
        return;
    }
    if close_x || close_f {
        return;
    }
    app.pano = Some(d);
}

fn start_pano(app: &mut App, d: &PanoDialog) {
    let inputs: Vec<crate::imaging::pano::PanoInput> = d.ids.iter().filter_map(|id| app.cat.get(*id)).map(|p| crate::imaging::pano::PanoInput { path: p.path.clone(), orientation: p.meta.orientation }).collect();
    let Some(first) = app.cat.get(d.ids[0]) else { return };
    let out = crate::imaging::pano::out_path(&first.path);
    let (tx, rx) = crossbeam_channel::unbounded();
    let ctx_tx = tx.clone();
    let (proj, crop) = (d.projection, d.auto_crop);
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("pano".into())
        .spawn(move || {
            let prog = |m: &str| {
                let _ = ctx_tx.send(HdrMsg::Status(m.to_string()));
            };
            let r = crate::imaging::pano::merge(&inputs, proj, crop, &out, &prog).map(|r| r.path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(HdrMsg::Done(r));
        })
        .expect("pano thread");
    let ref_id = d.ids[d.ids.len() / 2];
    app.pano_job = Some(PanoJob { rx, status: trf!("파노라마 준비 ({}장)", "Preparing panorama ({} photos)", d.ids.len()), ref_id });
}

pub fn pump_pano(app: &mut App) {
    let Some(job) = &mut app.pano_job else { return };
    let mut done = None;
    for m in job.rx.try_iter() {
        match m {
            HdrMsg::Status(s) => job.status = s,
            HdrMsg::Done(r) => done = Some(r),
        }
    }
    let Some(r) = done else { return };
    let ref_id = job.ref_id;
    app.pano_job = None;
    match r {
        Ok(path) => {
            let (w, h) = crate::imaging::hdr::read_header(&path).map(|h| (h.w as u32, h.h as u32)).unwrap_or((0, 0));
            let meta = app.cat.get(ref_id).map(|p| {
                let mut m = p.meta.clone();
                m.orientation = 1;
                m.width = w;
                m.height = h;
                m
            });
            let settings = app.cat.get(ref_id).map(|p| {
                let mut s = p.settings();
                // Position-dependent adjustments do not fit the new image, so they are dropped
                s.geometry = Default::default();
                s.masks.clear();
                s.spots.clear();
                s.privacy.clear();
                s.red_eye.clear();
                s
            });
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let import_id = app.cat.next_import_id();
            match app.cat.add_photos(&[(path.clone(), size, meta.unwrap_or_default(), settings, vec![tr!("파노라마", "Panorama").to_string()])], import_id) {
                Ok(ids) => {
                    app.visible_dirty = true;
                    app.recompute_visible();
                    if let Some(id) = ids.first() {
                        app.select_single(*id);
                    }
                    app.toast(trf!("파노라마 완료: {} ({w}×{h})", "Panorama done: {} ({w}×{h})", path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()));
                }
                Err(e) => app.toast_err(trf!("카탈로그 추가 실패: {e}", "Couldn't add to catalog: {e}")),
            }
        }
        Err(e) => app.toast_err(trf!("파노라마 실패: {e}", "Panorama failed: {e}")),
    }
}

// ───────────────────────── Print ─────────────────────────

pub struct PrintDialog {
    pub ids: Vec<PhotoId>,
    pub lay: crate::export::print::PrintLayout,
    pub page: usize,
    pub rx: Option<crossbeam_channel::Receiver<Result<Vec<PathBuf>, String>>>,
    pub prog: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub total_pages: usize,
    pub report: Option<String>,
}

pub fn open_print(app: &mut App) {
    let ids: Vec<PhotoId> = if app.selected.len() > 1 { app.selected.clone() } else { app.current.into_iter().collect() };
    if ids.is_empty() {
        app.toast(tr!("인쇄할 사진을 선택하세요", "Select photos to print"));
        return;
    }
    let lay = app.cat.kv_get_json("print_layout").unwrap_or_default();
    app.print = Some(PrintDialog { ids, lay, page: 0, rx: None, prog: Default::default(), total_pages: 0, report: None });
}

pub fn print_dialog(app: &mut App, ctx: &egui::Context) {
    use crate::export::print::{Caption, Paper};
    let Some(mut d) = app.print.take() else { return };
    let open = true;
    let mut go = false;
    let mut close = false;
    if let Some(rx) = &d.rx {
        if let Ok(r) = rx.try_recv() {
            d.report = Some(match r {
                Ok(files) => trf!("저장했습니다:\n{}", "Saved:\n{}", files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join("\n")),
                Err(e) => trf!("실패: {e}", "Failed: {e}"),
            });
            d.rx = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
    let per = d.lay.per_page();
    let pages = d.ids.len().div_ceil(per).max(1);
    d.page = d.page.min(pages - 1);
    let has_report = d.report.is_some();
    let busy = d.rx.is_some();
    let prog_text = trf!("{} / {} 쪽 만드는 중…", "Making page {} / {}…", d.prog.load(std::sync::atomic::Ordering::Relaxed), d.total_pages);
    let save_label = if d.lay.pdf { tr!("PDF로 저장…", "Save as PDF…") } else { tr!("JPEG로 저장…", "Save as JPEG…") };
    let mut close_f = false;
    let thumbs: Vec<Option<(egui::TextureId, egui::Vec2, String)>> = d
        .ids
        .iter()
        .skip(d.page * per)
        .take(per)
        .map(|id| {
            let t = app.thumb(*id)?;
            let size = app.tex.peek(id).map(|(_, h)| h.size_vec2()).unwrap_or(vec2(3.0, 2.0));
            Some((t, size, app.cat.get(*id).map(|p| p.display_name()).unwrap_or_default()))
        })
        .collect();
    let (_, _, close_x) = super::form::modal(
        ctx,
        "print",
        tr!("인쇄 · 레이아웃", "Print · Layout"),
        &trf!("사진 {}장 · {} 쪽", "{} photos · {} pages", d.ids.len(), pages),
        vec2(960.0, 700.0),
        |ui| {
            if let Some(r) = &d.report {
                super::form::card(ui, tr!("결과", "Result"), "", |ui| {
                    ui.label(egui::RichText::new(r).color(TEXT()));
                });
                return;
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(420.0);
                    egui::ScrollArea::vertical().id_salt("print_opts").auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(408.0);
                        let l = &mut d.lay;
                        super::form::card(ui, tr!("용지", "Paper"), "", |ui| {
                            super::form::row(ui, tr!("크기", "Size"), |ui| {
                                egui::ComboBox::from_id_salt("paper").width(160.0).selected_text(l.paper.name()).show_ui(ui, |ui| {
                                    for p in Paper::ALL {
                                        ui.selectable_value(&mut l.paper, p, p.name());
                                    }
                                });
                            });
                            super::form::row(ui, tr!("방향", "Orientation"), |ui| super::form::seg(ui, &mut l.landscape, &[(false, tr!("세로", "Portrait")), (true, tr!("가로", "Landscape"))]));
                            super::form::row(ui, tr!("여백", "Margins"), |ui| ui.add(egui::Slider::new(&mut l.margin_mm, 0.0..=40.0).suffix(" mm")));
                        });
                        super::form::card(ui, tr!("배치", "Placement"), "", |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for (r, c, n) in [(1u32, 1u32, tr!("1장", "1 up")), (2, 1, tr!("2장", "2 up")), (2, 2, tr!("4장", "4 up")), (3, 2, tr!("6장", "6 up")), (4, 3, tr!("12장", "12 up")), (5, 4, tr!("밀착 20", "Contact 20")), (6, 5, tr!("밀착 30", "Contact 30"))] {
                                    if ui.selectable_label(l.rows == r && l.cols == c, n).clicked() {
                                        l.rows = r;
                                        l.cols = c;
                                    }
                                }
                            });
                            super::form::row(ui, tr!("행 × 열", "Rows × Columns"), |ui| {
                                ui.add(egui::DragValue::new(&mut l.rows).range(1..=12));
                                ui.label("×");
                                ui.add(egui::DragValue::new(&mut l.cols).range(1..=12));
                            });
                            super::form::row(ui, tr!("간격", "Spacing"), |ui| ui.add(egui::Slider::new(&mut l.gap_mm, 0.0..=20.0).suffix(" mm")));
                            super::form::row(ui, tr!("칸에", "In cell"), |ui| super::form::seg(ui, &mut l.fill, &[(false, tr!("맞춤", "Fit")), (true, tr!("채우기 (잘라냄)", "Fill (crop)"))]));
                            super::form::switch_row(ui, tr!("자동 회전", "Auto-rotate"), &mut l.auto_rotate, tr!("칸 방향에 맞춰 돌림", "Rotates to match cell orientation"));
                        });
                        super::form::card(ui, tr!("캡션", "Caption"), "", |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for c in Caption::ALL {
                                    ui.selectable_value(&mut l.caption, c, c.name());
                                }
                            });
                        });
                        super::form::card(ui, tr!("출력", "Output"), "", |ui| {
                            super::form::row(ui, tr!("파일", "File"), |ui| super::form::seg(ui, &mut l.pdf, &[(true, tr!("PDF 한 파일", "Single PDF")), (false, tr!("쪽별 JPEG", "JPEG per page"))]));
                            super::form::row(ui, tr!("해상도", "Resolution"), |ui| super::form::seg(ui, &mut l.dpi, &[(150u32, "150"), (240, "240"), (300, "300"), (360, "360 dpi")]));
                            super::form::switch_row(ui, tr!("인쇄 샤프닝", "Print sharpening"), &mut l.sharpen, "");
                        });
                    });
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    let (pw, ph) = d.lay.page_mm();
                    let avail = vec2(ui.available_width().min(460.0), 520.0);
                    let k = (avail.x / pw).min(avail.y / ph);
                    let (rect, _) = ui.allocate_exact_size(vec2(pw * k, ph * k), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect.translate(vec2(3.0, 3.0)), 2.0, Color32::from_black_alpha(80));
                    p.rect_filled(rect, 2.0, Color32::WHITE);
                    let cap = d.lay.caption_mm();
                    for (i, (cx, cy, cw, ch)) in d.lay.cells_mm().into_iter().enumerate() {
                        let cell = Rect::from_min_size(rect.min + vec2(cx * k, cy * k), vec2(cw * k, (ch - cap) * k));
                        p.rect_stroke(cell, 0.0, egui::Stroke::new(0.5, Color32::from_gray(210)), egui::StrokeKind::Inside);
                        let Some(Some((t, size, name))) = thumbs.get(i) else { continue };
                        let (r, uv) = if d.lay.fill {
                            let s = (cell.width() / size.x).max(cell.height() / size.y);
                            let full = Rect::from_center_size(cell.center(), *size * s);
                            let fx = (cell.width() / full.width()).min(1.0);
                            let fy = (cell.height() / full.height()).min(1.0);
                            (full.intersect(cell), Rect::from_center_size(pos2(0.5, 0.5), vec2(fx, fy)))
                        } else {
                            (super::viewer::fit_rect(cell, *size), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)))
                        };
                        p.image(*t, r, uv, Color32::WHITE);
                        if cap > 0.0 {
                            p.text(pos2(cell.center().x, cell.bottom() + cap * k * 0.5), Align2::CENTER_CENTER, name, FontId::proportional((cap * k * 0.6).clamp(6.0, 11.0)), Color32::from_gray(80));
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if super::form::small(ui, "◀").clicked() && d.page > 0 {
                            d.page -= 1;
                        }
                        ui.label(mono(trf!("{} / {} 쪽", "Page {} / {}", d.page + 1, pages)).color(TEXT_WEAK()));
                        if super::form::small(ui, "▶").clicked() && d.page + 1 < pages {
                            d.page += 1;
                        }
                    });
                });
            });
        },
        |ui| {
            if busy {
                ui.spinner();
                super::form::footer_note(ui, &prog_text);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if has_report {
                    if super::form::primary(ui, tr!("닫기", "Close"), true).clicked() {
                        close_f = true;
                    }
                } else {
                    if super::form::primary(ui, save_label, !busy).clicked() {
                        go = true;
                    }
                    if super::form::secondary(ui, tr!("닫기", "Close")).clicked() {
                        close_f = true;
                    }
                }
            });
        },
    );
    if close_x || close_f {
        close = true;
    }
    if go {
        let ext = if d.lay.pdf { "pdf" } else { "jpg" };
        if let Some(path) = rfd::FileDialog::new().add_filter(if d.lay.pdf { "PDF" } else { "JPEG" }, &[ext]).set_file_name(trf!("Darkroom 인쇄.{ext}", "Darkroom Print.{ext}")).save_file() {
            app.cat.kv_set_json("print_layout", &d.lay);
            let items: Vec<(crate::catalog::Photo, crate::develop::settings::DevelopSettings)> = d.ids.iter().filter_map(|id| app.cat.get(*id)).map(|p| (p.clone(), p.settings())).collect();
            let lay = d.lay.clone();
            let (tx, rx) = crossbeam_channel::unbounded();
            let prog = d.prog.clone();
            d.total_pages = pages;
            std::thread::Builder::new()
                .stack_size(crate::config::WORKER_STACK)
                .name("print".into())
                .spawn(move || {
                    let r = crate::export::print::run(items, &lay, &path, &|i, _| prog.store(i, std::sync::atomic::Ordering::Relaxed)).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(r);
                })
                .expect("print thread");
            d.rx = Some(rx);
        }
    }
    if open && !close {
        app.print = Some(d);
    }
}


// ───────────────────────── Relink missing files ─────────────────────────

/// Finds files named like the missing photos in the chosen folder (including subfolders) and relinks them (also compares size when recorded)
pub fn relink_missing(app: &mut App) {
    let Some(dir) = rfd::FileDialog::new().set_title(tr!("누락된 사진을 찾을 폴더", "Folder to search for missing photos")).pick_folder() else { return };
    let missing: Vec<(PhotoId, String, u64)> = app
        .cat
        .photos
        .iter()
        .filter(|p| p.missing && p.master.is_none())
        .map(|p| (p.id, p.file_name.to_lowercase(), p.file_size))
        .collect();
    if missing.is_empty() {
        app.toast(tr!("누락된 사진이 없습니다", "No missing photos"));
        return;
    }
    // Folder index (name → paths)
    let mut index: std::collections::HashMap<String, Vec<PathBuf>> = std::collections::HashMap::new();
    let mut stack = vec![dir];
    let mut scanned = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(n) = p.file_name() {
                index.entry(n.to_string_lossy().to_lowercase()).or_default().push(p);
                scanned += 1;
            }
        }
        if scanned > 2_000_000 {
            break;
        }
    }
    let mut found = 0;
    for (id, name, size) in missing {
        let Some(cands) = index.get(&name) else { continue };
        let pick = cands.iter().find(|c| size == 0 || std::fs::metadata(c).map(|m| m.len() == size).unwrap_or(false)).or(cands.first());
        if let Some(np) = pick
            && app.cat.update_path(id, np).is_ok() {
                found += 1;
            }
    }
    app.cat.check_missing();
    app.visible_dirty = true;
    let left = app.cat.photos.iter().filter(|p| p.missing).count();
    app.toast(trf!("{found}장을 다시 연결했습니다{}", "Relinked {found} photos{}", if left > 0 { trf!(" (남은 누락 {left}장)", " ({left} still missing)") } else { String::new() }));
}

/// For a moved folder: rewrites all photo paths under the old folder (including subfolders) to the new folder
pub fn relocate_folder(app: &mut App, old: &std::path::Path) {
    let Some(new) = rfd::FileDialog::new().set_title(trf!("'{}'의 새 위치", "New location of '{}'", old.display())).pick_folder() else { return };
    let targets: Vec<(PhotoId, PathBuf)> = app
        .cat
        .photos
        .iter()
        .filter(|p| p.path.starts_with(old))
        .filter_map(|p| p.path.strip_prefix(old).ok().map(|rel| (p.id, new.join(rel))))
        .collect();
    let mut moved = 0;
    for (id, np) in targets {
        if np.exists() && app.cat.update_path(id, &np).is_ok() {
            moved += 1;
        }
    }
    app.cat.check_missing();
    app.visible_dirty = true;
    if app.source == Source::Folder(old.to_path_buf()) {
        app.set_source(Source::Folder(new.clone()));
    }
    app.toast(trf!("{moved}장을 새 위치로 연결했습니다", "Linked {moved} photos to the new location"));
}


// ───────────────────────── XMP sidecar ─────────────────────────

pub fn save_xmp(app: &mut App) {
    let ids = app.targets();
    let mut ok = 0;
    let mut fail = 0;
    for id in ids {
        let Some(p) = app.cat.get(id) else { continue };
        if p.master.is_some() {
            continue; // Virtual copies have no file
        }
        let s = match &app.devst {
            Some(d) if d.id == id => d.settings.clone(),
            _ => p.settings(),
        };
        match crate::xmpwrite::write(p, &s) {
            Ok(_) => ok += 1,
            Err(_) => fail += 1,
        }
    }
    if fail == 0 {
        app.toast(trf!("XMP 사이드카 {ok}개 저장", "Saved {ok} XMP sidecars"));
    } else {
        app.toast_err(trf!("XMP {ok}개 저장, {fail}개 실패", "Saved {ok} XMP, {fail} failed"));
    }
}

/// Reads settings from the XMP sidecar and applies them (imports edits made in other programs)
pub fn load_xmp(app: &mut App) {
    let ids = app.targets();
    let mut ok = 0;
    for id in ids {
        let Some(p) = app.cat.get(id) else { continue };
        let side = crate::xmpwrite::sidecar_path(p);
        let Ok(text) = std::fs::read_to_string(&side) else { continue };
        if let Some(s) = settings_from_xmp(&text, p.is_raw) {
            let _ = app.cat.set_develop(id, &s);
            let _ = app.cat.push_history(id, tr!("XMP에서 불러옴", "Read from XMP"), &s);
            ok += 1;
        }
        if let Some(r) = xmp_rating(&text) {
            let _ = app.cat.set_rating(&[id], r);
        }
    }
    if let Some(d) = &mut app.devst
        && let Some(p) = app.cat.get(d.id) {
            d.settings = p.settings();
        }
    app.visible_dirty = true;
    app.toast(trf!("XMP에서 {ok}장 설정을 불러왔습니다", "Read settings for {ok} photos from XMP"));
}

pub fn settings_from_xmp(text: &str, is_raw: bool) -> Option<crate::develop::settings::DevelopSettings> {
    let r = crate::lrpreset::parse_xmp(text, "", "").ok()?;
    let mut s = crate::develop::settings::DevelopSettings::default_for(is_raw);
    let mut g = r.groups.clone();
    g.crop = true;
    s.copy_groups_from(&r.settings, &g);
    Some(s)
}

pub fn xmp_rating(text: &str) -> Option<u8> {
    let i = text.find("xmp:Rating=\"")? + 12;
    let j = text[i..].find('"')?;
    text[i..i + j].trim().parse::<i32>().ok().map(|v| v.clamp(0, 5) as u8)
}


// ───────────────────────── Stacks ─────────────────────────

pub fn stack_selected(app: &mut App) {
    let mut ids = app.multi_targets();
    if ids.len() < 2 {
        app.toast(tr!("스택으로 묶을 사진을 2장 이상 선택하세요", "Select 2 or more photos to stack"));
        return;
    }
    // Move the current photo to the top
    if let Some(c) = app.current
        && let Some(i) = ids.iter().position(|x| *x == c) {
            ids.remove(i);
            ids.insert(0, c);
        }
    match app.cat.stack(&ids) {
        Ok(sid) => {
            let n = app.cat.stack_size(sid);
            app.visible_dirty = true;
            app.toast(trf!("{n}장을 스택으로 묶었습니다 (S: 펼치기/접기)", "Stacked {n} photos (S: expand/collapse)"));
        }
        Err(e) => app.toast_err(trf!("스택 실패: {e}", "Stack failed: {e}")),
    }
}

pub fn unstack_selected(app: &mut App) {
    let ids = app.multi_targets();
    let had = ids.iter().filter_map(|i| app.cat.get(*i)).any(|p| p.stack_id != 0);
    if !had {
        app.toast(tr!("선택한 사진은 스택에 없습니다", "The selected photos aren't in a stack"));
        return;
    }
    let _ = app.cat.dissolve_stacks(&ids);
    app.visible_dirty = true;
    app.toast(tr!("스택을 해제했습니다", "Unstacked"));
}

pub fn toggle_stack(app: &mut App) {
    let Some(c) = app.current else { return };
    let sid = app.cat.get(c).map(|p| p.stack_id).unwrap_or(0);
    if sid == 0 {
        return;
    }
    if !app.expanded_stacks.remove(&sid) {
        app.expanded_stacks.insert(sid);
    }
    app.visible_dirty = true;
}

pub fn auto_stack(app: &mut App, gap: f64) {
    let ids = if app.selected.len() >= 2 { app.selected.clone() } else { app.visible.clone() };
    match app.cat.auto_stack(&ids, gap) {
        Ok(n) => {
            app.visible_dirty = true;
            app.toast(trf!("자동 스택 {n}개를 만들었습니다", "Created {n} auto stacks"));
        }
        Err(e) => app.toast_err(trf!("자동 스택 실패: {e}", "Auto stack failed: {e}")),
    }
}
