//! Self-test mode (test switches: DARKROOM_AUTOTEST=<import folder>, DARKROOM_SHOTS=<screenshot folder>).
//! Import → library capture → develop capture → capture after edits → exit. Normal runs are unaffected.

use super::app::{App, LibView, Module, Source};
use std::path::PathBuf;
use std::time::Instant;

pub struct AutoTest {
    src: PathBuf,
    out: PathBuf,
    step: usize,
    t: Instant,
    pending_shot: Option<String>,
    skip_color: bool,
    /// Fake input batches, one injected per frame (mouse/keyboard tests)
    events: std::collections::VecDeque<Vec<egui::Event>>,
    /// Log for interaction tests
    mark: [f32; 4],
    /// Modifier state injected along with fake input (Shift-drag etc.)
    mods: egui::Modifiers,
    mods_sent: egui::Modifiers,
    /// Watchdog: log and exit if stuck on one step too long (prevents orphan processes)
    watch: (usize, Instant),
}

impl AutoTest {
    pub fn from_env() -> Option<Self> {
        let src = PathBuf::from(std::env::var_os("DARKROOM_AUTOTEST")?);
        let out = std::env::var_os("DARKROOM_SHOTS").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&out);
        Some(Self { src, out, step: 0, t: Instant::now(), pending_shot: None, skip_color: false, events: Default::default(), mark: [0.0; 4], mods: egui::Modifiers::NONE, mods_sent: egui::Modifiers::NONE, watch: (usize::MAX, Instant::now()) })
    }
}

/// Inject fake events into raw input (called from the eframe raw_input_hook)
pub fn inject(app: &mut App, raw: &mut egui::RawInput) {
    if let Some(at) = &mut app.autotest {
        if let Some(batch) = at.events.pop_front() {
            raw.events.extend(batch);
        }
        // Send a modifiers-changed event when the modifier state changes
        if at.mods != at.mods_sent {
            raw.events.insert(0, egui::Event::ModifiersChanged(at.mods));
            at.mods_sent = at.mods;
        }
    }
}

fn ev_click(q: &mut std::collections::VecDeque<Vec<egui::Event>>, p: egui::Pos2) {
    let m = egui::Modifiers::NONE;
    q.push_back(vec![egui::Event::PointerMoved(p)]);
    q.push_back(vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: m }]);
    q.push_back(vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: m }]);
    q.push_back(vec![]);
}

fn ev_drag(q: &mut std::collections::VecDeque<Vec<egui::Event>>, a: egui::Pos2, b: egui::Pos2, steps: usize) {
    let m = egui::Modifiers::NONE;
    q.push_back(vec![egui::Event::PointerMoved(a)]);
    q.push_back(vec![egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m }]);
    for k in 1..=steps {
        let t = k as f32 / steps as f32;
        q.push_back(vec![egui::Event::PointerMoved(a + (b - a) * t)]);
    }
    q.push_back(vec![egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: m }]);
    q.push_back(vec![]);
}

/// Drag through several points (spending a few frames per point)
fn ev_path(q: &mut std::collections::VecDeque<Vec<egui::Event>>, pts: &[egui::Pos2], mods: egui::Modifiers) {
    q.push_back(vec![egui::Event::PointerMoved(pts[0])]);
    q.push_back(vec![egui::Event::PointerButton { pos: pts[0], button: egui::PointerButton::Primary, pressed: true, modifiers: mods }]);
    for w in pts.windows(2) {
        for k in 1..=6 {
            let t = k as f32 / 6.0;
            q.push_back(vec![egui::Event::PointerMoved(w[0] + (w[1] - w[0]) * t)]);
        }
    }
    let last = *pts.last().unwrap();
    q.push_back(vec![egui::Event::PointerButton { pos: last, button: egui::PointerButton::Primary, pressed: false, modifiers: mods }]);
    q.push_back(vec![]);
}

/// Drag with the middle (wheel) button
fn ev_middle_drag(q: &mut std::collections::VecDeque<Vec<egui::Event>>, a: egui::Pos2, b: egui::Pos2) {
    let m = egui::Modifiers::NONE;
    q.push_back(vec![egui::Event::PointerMoved(a)]);
    q.push_back(vec![egui::Event::PointerButton { pos: a, button: egui::PointerButton::Middle, pressed: true, modifiers: m }]);
    for k in 1..=8 {
        q.push_back(vec![egui::Event::PointerMoved(a + (b - a) * (k as f32 / 8.0))]);
    }
    q.push_back(vec![egui::Event::PointerButton { pos: b, button: egui::PointerButton::Middle, pressed: false, modifiers: m }]);
    q.push_back(vec![]);
}

fn ev_key(q: &mut std::collections::VecDeque<Vec<egui::Event>>, key: egui::Key) {
    let m = egui::Modifiers::NONE;
    q.push_back(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m }]);
    q.push_back(vec![egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m }]);
    q.push_back(vec![]);
}

fn log(msg: &str) {
    eprintln!("[autotest] {msg}");
}

pub fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(mut at) = app.autotest.take() else { return };
    // Receive capture results
    let shots: Vec<std::sync::Arc<egui::ColorImage>> = ctx.input(|i| {
        i.raw.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }).collect()
    });
    if let (Some(name), Some(img)) = (at.pending_shot.clone(), shots.first()) {
        let path = at.out.join(format!("{name}.png"));
        let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let _ = image::save_buffer(&path, &bytes, img.size[0] as u32, img.size[1] as u32, image::ExtendedColorType::Rgba8);
        let rw: Option<f32> = ctx.data(|d| d.get_temp(egui::Id::new("dev_right_w")));
        log(&format!("saved {} (right panel {:?})", path.display(), rw));
        at.pending_shot = None;
        at.step += 1;
        at.t = Instant::now();
    }
    let el = at.t.elapsed().as_secs_f32();
    if at.watch.0 != at.step {
        at.watch = (at.step, Instant::now());
    } else if at.watch.1.elapsed().as_secs() > 240 {
        log(&format!("watchdog: step {} stuck for 240s — closing", at.step));
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        at.watch.1 = Instant::now();
    }
    if std::env::var_os("DARKROOM_AUTOTEST_VERBOSE").is_some() && ((el * 10.0) as u32).is_multiple_of(20) {
        log(&format!("step {} el {:.1} import={} pending={:?} photos={} shots_in={}", at.step, el, app.import.as_ref().map(|i| format!("{}/{}", i.done, i.total)).unwrap_or("-".into()), at.pending_shot, app.cat.photos.len(), shots.len()));
    }
    let shot = |at: &mut AutoTest, name: &str| {
        at.pending_shot = Some(name.to_string());
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    };
    // Session-state restore test: set = create state and exit, check = log the restored state and exit
    if let Some(mode) = std::env::var_os("DARKROOM_SESSION") {
        let state = |app: &App| format!("module {:?} view {:?} source {:?} current {:?} selected {} zoom {:?} tab {:?}", app.module, app.lib_view, app.source, app.current, app.selected.len(), app.devst.as_ref().map(|d| d.viewer.zoom), app.devst.as_ref().map(|d| d.panel_tab));
        if mode == "check" {
            if el > 3.0 {
                log(&format!("restored: {}", state(app)));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else if at.step == 0 && el > 3.0 && !app.visible.is_empty() {
            let folder = app.cat.photos.first().map(|p| p.folder.clone());
            if let Some(f) = folder {
                app.set_source(Source::Folder(f));
            }
            let ids: Vec<_> = app.visible.iter().copied().take(4).collect();
            app.select_single(*ids.last().unwrap());
            app.set_module(Module::Develop);
            if let Some(d) = &mut app.devst {
                d.viewer.zoom = Some(0.5);
                d.viewer.center = [0.3, 0.6];
                d.panel_tab = super::develop::panel_tab_from(2);
            }
            at.step = 1;
            at.t = Instant::now();
        } else if at.step == 1 && el > 2.0 {
            log(&format!("saved: {}", state(app)));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            at.step = 2;
        }
        app.autotest = Some(at);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    if at.pending_shot.is_none() {
        match at.step {
            0 => {
                if let Some(lr) = std::env::var_os("DARKROOM_LRCAT") {
                    log("lrcat import");
                    if let Err(e) = super::lrcat_import::autostart(app, std::path::Path::new(&lr)) {
                        log(&format!("lrcat error {e}"));
                    }
                } else {
                    log("import");
                    super::dialogs::start_import(app, super::dialogs::ImportPlan { source: at.src.clone(), recursive: false, ..Default::default() });
                }
                at.step = 1;
                at.t = Instant::now();
            }
            1 if app.import.is_none() && app.lrcat_progress.is_none() && el > 6.0 => {
                if at.pending_shot.is_none() {
                    let edited = app.cat.photos.iter().filter(|p| p.has_edits()).count();
                    log(&format!("catalog: {} photos, {} edited, {} collections, report={:?}", app.cat.photos.len(), edited, app.cat.collections.len(), app.dlg.lrcat.as_ref().and_then(|d| d.report.clone())));
                    for p in &app.cat.photos {
                        let s = p.settings();
                        log(&format!("  {} r{} {:?} {:?} exp={:.2} masks={} rot={} kw={:?} copy={:?}", p.file_name, p.rating, p.flag, p.label, s.exposure, s.masks.len(), s.geometry.rotate90, p.keywords, p.copy_name));
                    }
                    app.dlg.lrcat = None;
                }
                shot(&mut at, "01_library")
            }
            // AI enhance (test switch: DARKROOM_AUTOTEST_AI=1, models in DARKROOM_AI_DIR): dialog → preview → process → check the stack
            2 if std::env::var_os("DARKROOM_AUTOTEST_AI").is_some() => {
                let raw = app.visible.iter().copied().find(|id| app.cat.get(*id).map(|p| p.is_raw).unwrap_or(false));
                if let Some(id) = raw {
                    app.select_single(id);
                    at.mark[0] = id as f32;
                    at.mark[1] = app.cat.photos.len() as f32;
                }
                super::enhance::open(app);
                if let Some(d) = &mut app.enhance {
                    d.opts = crate::imaging::ai::Options { denoise: true, amount: 60.0, superres: true };
                }
                at.step = 300;
                at.t = Instant::now();
            }
            300 if el > 25.0 => shot(&mut at, "40_enhance_dialog"),
            // Preview drag: screens right after dragging (computation paused, original shown) and after it stops (computed)
            301 => {
                let lr: Option<egui::Rect> = ctx.data(|d| d.get_temp(egui::Id::new("enh_left")));
                if let Some(r) = lr {
                    ev_drag(&mut at.events, r.center(), r.center() + egui::vec2(-160.0, -90.0), 12);
                }
                at.step = 310;
                at.t = Instant::now();
            }
            310 if at.events.is_empty() => shot(&mut at, "40b_enhance_dragging"),
            311 if el > 8.0 => shot(&mut at, "40c_enhance_settled"),
            312 => {
                // Same action as the dialog's Enhance button
                let n_before = app.cat.photos.len();
                if let Some(d) = app.enhance.take() {
                    super::enhance::start_for_test(app, &d);
                }
                log(&format!("enhance started, photos {n_before}, job {}", app.enhance_job.is_some()));
                at.step = 302;
                at.t = Instant::now();
            }
            302 if el > 2.0 => shot(&mut at, "41_enhance_running"),
            303 if app.enhance_job.is_none() => {
                let orig = at.mark[0] as i64;
                let new = app.current;
                let p = new.and_then(|id| app.cat.get(id));
                let o = app.cat.get(orig);
                log(&format!(
                    "enhance done: photos {} -> {}, new {:?} ({:?} {}x{}), stacked with original = {}, toast {:?}",
                    at.mark[1],
                    app.cat.photos.len(),
                    new,
                    p.map(|p| p.file_name.clone()),
                    p.map(|p| p.meta.width).unwrap_or(0),
                    p.map(|p| p.meta.height).unwrap_or(0),
                    match (p, o) {
                        (Some(a), Some(b)) => a.stack_id != 0 && a.stack_id == b.stack_id,
                        _ => false,
                    },
                    app.toasts.last().map(|t| t.text.clone())
                ));
                app.set_module(Module::Develop);
                at.step = 304;
                at.t = Instant::now();
            }
            304 if el > 6.0 => shot(&mut at, "42_enhance_result"),
            305 => {
                log("done");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                at.step = 100;
            }
            2 => {
                app.set_module(Module::Develop);
                // Quick UI-only check (test switch: DARKROOM_AUTOTEST_UI=1)
                at.step = if std::env::var_os("DARKROOM_AUTOTEST_INPUT").is_some() {
                    200
                } else if std::env::var_os("DARKROOM_AUTOTEST_UI").is_some() {
                    79
                } else {
                    3
                };
                at.t = Instant::now();
            }
            3 if el > 5.0 => shot(&mut at, "02_develop"),
            4 => {
                app.select_all();
                app.prefs.auto_sync = true;
                if let Some(d) = &mut app.devst {
                    d.settings.exposure = 0.4;
                    d.settings.highlights = -60.0;
                    d.settings.shadows = 40.0;
                    d.settings.clarity = 25.0;
                    d.settings.vibrance = 30.0;
                    d.settings.effects.vignette_amount = -25.0;
                    // Value field width check: longest value string
                    d.settings.texture = 100.0;
                    d.settings.dehaze = -100.0;
                    d.settings.whites = 100.0;
                }
                app.commit("autotest edit");
                let cur = app.devst.as_ref().map(|d| d.id);
                for id in app.selected.clone() {
                    if Some(id) != cur {
                        let s = app.cat.get(id).map(|p| p.settings()).unwrap_or_default();
                        log(&format!("auto-sync -> {id}: exp={:.2} hl={} sh={} clar={} vib={}", s.exposure, s.highlights, s.shadows, s.clarity, s.vibrance));
                    }
                }
                app.prefs.auto_sync = false;
                at.step = 5;
                at.t = Instant::now();
            }
            5 if el > 3.0 => shot(&mut at, "03_edited"),
            6 if !at.skip_color => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Color;
                    d.settings.hsl[1].sat = -30.0;
                    d.settings.hsl[5].hue = 25.0;
                    d.settings.grading.shadows = crate::develop::settings::Wheel { hue: 210.0, sat: 25.0, lum: -10.0 };
                    d.settings.grading.highlights = crate::develop::settings::Wheel { hue: 40.0, sat: 20.0, lum: 5.0 };
                }
                app.commit("autotest color");
                at.step = 61;
                at.t = Instant::now();
            }
            61 if el > 3.0 => shot(&mut at, "03b_color"),
            // Detail tab: 1:1 preview + sharpening and noise reduction
            62 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Detail;
                    d.settings.detail.sharpen_amount = 80.0;
                    d.settings.detail.nr_luma = 40.0;
                }
                app.commit("autotest detail");
                at.step = 621;
                at.t = Instant::now();
            }
            621 if el > 4.0 => {
                let dv = app.devst.as_ref().and_then(|d| d.detail_view.as_ref()).map(|v| (v.zoom, v.has_underlay(), v.source.is_some()));
                log(&format!("detail 1:1 view: {dv:?}"));
                shot(&mut at, "03c_detail")
            }
            622 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Optics;
                }
                at.step = 6221;
                at.t = Instant::now();
            }
            6221 if el > 2.0 => shot(&mut at, "03g_optics"),
            6222 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Effects;
                }
                at.step = 6223;
                at.t = Instant::now();
            }
            6223 if el > 2.0 => shot(&mut at, "03h_effects"),
            6224 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Light;
                    d.tool = super::develop::Tool::Privacy;
                    d.settings.privacy.push(crate::develop::settings::PrivacyRegion { center: [0.5, 0.3], radius: [0.12, 0.08], ..Default::default() });
                    d.settings.privacy.push(crate::develop::settings::PrivacyRegion {
                        center: [0.3, 0.7],
                        radius: [0.1, 0.07],
                        kind: crate::develop::settings::PrivacyKind::Blur,
                        ..Default::default()
                    });
                    if let Some(src) = d.viewer.source.clone() {
                        let r = crate::imaging::faces::auto_regions(&src, crate::develop::settings::PrivacyKind::Mosaic, 50.0);
                        log(&format!("face detect: {:?}", r.map(|v| v.len())));
                    }
                }
                app.commit("autotest privacy");
                at.step = 63;
                at.t = Instant::now();
            }
            63 if el > 3.0 => shot(&mut at, "03c_privacy"),
            64 => {
                if let Some(d) = &mut app.devst {
                    d.settings.privacy.clear();
                    d.tool = super::develop::Tool::Spot;
                    if let Some(src) = d.viewer.source.clone() {
                        for dst in [[0.42f32, 0.55f32], [0.6, 0.35]] {
                            let src_pt = crate::develop::pipeline::auto_spot_source(&src, dst, 0.03);
                            d.settings.spots.push(crate::develop::settings::Spot { dst, src: src_pt, radius: 0.03, ..Default::default() });
                        }
                        d.spot_sel = Some(1);
                    }
                }
                app.commit("autotest spot");
                at.step = 65;
                at.t = Instant::now();
            }
            65 if el > 3.0 => shot(&mut at, "03d_spot"),
            66 => {
                if let Some(d) = &mut app.devst {
                    d.tool = super::develop::Tool::None;
                    d.settings.spots.clear();
                }
                app.commit("autotest privacy clear");
                at.step = 6;
                at.t = Instant::now();
                at.skip_color = true;
            }
            6 if at.skip_color => {
                super::develop::shortcut(app, egui::Key::R, egui::Modifiers::NONE);
                at.step = 7;
                at.t = Instant::now();
            }
            7 if el > 2.5 => shot(&mut at, "04_crop"),
            // Straighten tool: Ctrl+drag along a tilted line → angle that makes it level
            8 => {
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    let a = r.center() + egui::vec2(-r.width() * 0.3, -r.width() * 0.3 * 0.105 / 2.0);
                    let b = r.center() + egui::vec2(r.width() * 0.3, r.width() * 0.3 * 0.105 / 2.0);
                    let m = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
                    at.mods = m;
                    ev_path(&mut at.events, &[a, b], m);
                }
                at.mark[0] = app.devst.as_ref().map(|d| d.settings.geometry.angle).unwrap_or(0.0);
                at.step = 801;
                at.t = Instant::now();
            }
            801 if at.events.is_empty() && el > 1.5 => {
                at.mods = egui::Modifiers::NONE;
                let a1 = app.devst.as_ref().map(|d| d.settings.geometry.angle).unwrap_or(0.0);
                log(&format!("straighten: angle {} -> {a1:.2} (기울기 약 3°인 선 → 약 ±3°)", at.mark[0]));
                at.mark[1] = a1;
                shot(&mut at, "04b_crop_straighten")
            }
            802 => {
                if let Some(d) = &mut app.devst {
                    d.settings.geometry = Default::default();
                }
                at.step = 803;
            }
            803 => {
                super::develop::shortcut(app, egui::Key::R, egui::Modifiers::NONE);
                super::develop::shortcut(app, egui::Key::M, egui::Modifiers::SHIFT);
                if let Some(d) = &mut app.devst
                    && let Some(m) = d.settings.masks.last_mut() {
                        m.adj.exposure = 1.0;
                    }
                app.commit("autotest mask");
                at.step = 9;
                at.t = Instant::now();
            }
            9 if el > 3.0 => shot(&mut at, "05_mask"),
            10 => {
                let dirs = crate::lrpreset::default_dirs();
                let files: Vec<_> = dirs.iter().flat_map(|d| crate::lrpreset::collect_files(d, &["xmp", "lrtemplate"])).collect();
                let wms = crate::lrpreset::watermark_dir().map(|d| crate::lrpreset::collect_files(&d, &["lrtemplate"])).unwrap_or_default();
                let t = Instant::now();
                let rep = super::dialogs::run_lr_import(app, &files, &wms, true);
                log(&format!("lr import in {:?}: {}", t.elapsed(), rep.replace('\n', " | ")));
                let pick = app.cat.presets().iter().find(|p| p.name == "FK 1").cloned().or_else(|| app.cat.presets().first().cloned());
                if let (Some(p), Some(d)) = (pick, &mut app.devst) {
                    d.settings.copy_groups_from(&p.settings, &p.groups);
                    d.tool = super::develop::Tool::None;
                    log(&format!("applied preset {}", p.name));
                }
                app.dlg.preset_filter = "FK".into();
                app.commit("autotest preset");
                at.step = 11;
                at.t = Instant::now();
            }
            11 if el > 3.0 => shot(&mut at, "05b_preset"),
            12 => {
                app.set_module(Module::Library);
                at.step = 13;
                at.t = Instant::now();
            }
            13 if el > 4.0 => shot(&mut at, "06_library_after"),
            14 => {
                app.prefs.artist = "OrionNest".into();
                super::dialogs::open_wm_editor(app, "");
                at.step = 15;
                at.t = Instant::now();
            }
            15 if el > 1.5 => shot(&mut at, "07_watermark"),
            16 => {
                app.dlg.wm = None;
                // Synchronous export (watermark + resize + metadata)
                let wm: crate::export::watermark::Watermark = app.cat.kv_get_json::<Vec<crate::export::watermark::Watermark>>("watermarks").and_then(|v| v.into_iter().next()).unwrap_or_else(|| crate::export::watermark::Watermark { text: "© {year} {artist}".into(), ..Default::default() });
                log(&format!("export watermark: {}", wm.name));
                let ex = crate::export::ExportSettings {
                    folder: at.out.join("export").to_string_lossy().to_string(),
                    resize: true,
                    size_a: 2048,
                    sharpen: true,
                    watermark: true,
                    open_folder: false,
                    ..Default::default()
                };
                let reserved = parking_lot::Mutex::new(std::collections::HashSet::new());
                let mut eng = crate::develop::pipeline::Engine::default();
                for id in app.visible.iter().take(2) {
                    let p = app.cat.get(*id).unwrap().clone();
                    let s = p.settings();
                    let t = Instant::now();
                    match crate::export::export_one(&p, &s, &ex, Some(&wm), "OrionNest", "© 2026 OrionNest", 1, &mut eng, &reserved) {
                        Ok(Some(path)) => log(&format!("exported {} in {:?}", path.display(), t.elapsed())),
                        Ok(None) => log("export skipped"),
                        Err(e) => log(&format!("export error {e:#}")),
                    }
                }
                at.step = 17;
            }
            17 => {
                // Print PDF (2×2, file-name captions)
                let items: Vec<_> = app.visible.iter().take(4).filter_map(|id| app.cat.get(*id)).map(|p| (p.clone(), p.settings())).collect();
                let lay = crate::export::print::PrintLayout { rows: 2, cols: 2, caption: crate::export::print::Caption::FileName, dpi: 150, ..Default::default() };
                let t = Instant::now();
                match crate::export::print::run(items, &lay, &at.out.join("print.pdf"), &|_, _| {}) {
                    Ok(f) => log(&format!("print ok {:?} in {:?}", f, t.elapsed())),
                    Err(e) => log(&format!("print error {e:#}")),
                }
                // Export history → filmstrip filter (exported)
                for id in app.visible.clone().iter().take(2) {
                    app.cat.mark_exported(*id);
                }
                app.filter.exported = Some(true);
                app.recompute_visible();
                log(&format!("filter exported: {} visible", app.visible.len()));
                app.filter.exported = Some(false);
                app.recompute_visible();
                log(&format!("filter not exported: {} visible", app.visible.len()));
                app.filter = Default::default();
                app.recompute_visible();
                // Stacks: imported stack count, collapse/expand, auto-stack
                let stacks: std::collections::HashSet<i64> = app.cat.photos.iter().map(|p| p.stack_id).filter(|s| *s != 0).collect();
                let before = app.visible.len();
                let all = app.visible.clone();
                let n = app.cat.auto_stack(&all, 2.0).unwrap_or(0);
                app.recompute_visible();
                let collapsed = app.visible.len();
                app.expanded_stacks.extend(app.cat.photos.iter().map(|p| p.stack_id).filter(|s| *s != 0));
                app.recompute_visible();
                log(&format!("stacks: imported {} · auto(2s) made {n} · visible {before} → collapsed {collapsed} → expanded {}", stacks.len(), app.visible.len()));
                app.set_module(Module::Develop);
                if let Some(lr) = std::env::var_os("DARKROOM_LRCAT") {
                    super::lrcat_import::open_path(app, std::path::PathBuf::from(lr));
                }
                at.step = 18;
                at.t = Instant::now();
            }
            18 if el > 3.0 => shot(&mut at, "08_lrcat_dialog"),
            19 => {
                app.dlg.lrcat = None;
                app.palette = Some(super::tools::Palette { query: tr!("테마", "theme").into(), sel: 0 });
                at.step = 70;
                at.t = Instant::now();
            }
            70 if el > 1.5 => shot(&mut at, "09_palette"),
            71 => {
                app.palette = None;
                app.show_shortcuts = true;
                at.step = 72;
                at.t = Instant::now();
            }
            72 if el > 1.5 => shot(&mut at, "10_shortcuts"),
            73 => {
                app.show_shortcuts = false;
                super::tools::start_slideshow(app);
                at.step = 74;
                at.t = Instant::now();
            }
            74 if el > 3.0 => shot(&mut at, "11_slideshow"),
            75 => {
                app.slideshow = None;
                app.prefs.theme = super::theme::ThemeKind::Light;
                app.pending_theme = Some(super::theme::ThemeKind::Light);
                app.set_module(Module::Library);
                at.step = 76;
                at.t = Instant::now();
            }
            76 if el > 3.0 => shot(&mut at, "12_light_theme"),
            // ── Interaction tests (fake input): spot click/brush, Delete, radial draw, panning ──
            200 if el > 4.0 => {
                super::develop::shortcut(app, egui::Key::Q, egui::Modifiers::NONE);
                at.step = 201;
                at.t = Instant::now();
            }
            201 if el > 1.5 => {
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    let at_ = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
                    ev_click(&mut at.events, at_(0.40, 0.42));
                    ev_drag(&mut at.events, at_(0.55, 0.62), at_(0.72, 0.66), 12);
                    log(&format!("input: spot click + stroke on image {:?}", r));
                }
                at.step = 202;
                at.t = Instant::now();
            }
            202 if at.events.is_empty() && el > 3.0 => {
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    at.events.push_back(vec![egui::Event::PointerMoved(egui::pos2(r.left() + r.width() * 0.3, r.top() + r.height() * 0.25))]);
                }
                if let Some(d) = &app.devst {
                    let kinds: Vec<String> = d.settings.spots.iter().map(|s| if s.path.is_empty() { "원형".to_string() } else { format!("브러시({}점)", s.path.len()) }).collect();
                    log(&format!("spots after input: {} {:?} sel={:?}", d.settings.spots.len(), kinds, d.spot_sel));
                }
                shot(&mut at, "30_spot_input")
            }
            203 => {
                // Overlapping zigzag strokes (checks the merged outline)
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    let at_ = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
                    ev_path(&mut at.events, &[at_(0.20, 0.70), at_(0.30, 0.74), at_(0.22, 0.75), at_(0.32, 0.79), at_(0.24, 0.80)], egui::Modifiers::NONE);
                }
                at.step = 220;
                at.t = Instant::now();
            }
            220 if at.events.is_empty() && el > 3.0 => shot(&mut at, "32_spot_union"),
            221 => {
                // Shift + drag: move the source from anywhere
                if let (Some(d), Some(v)) = (app.devst.as_ref(), app.devst.as_ref().and_then(|d| d.viewer.last_view())) {
                    if let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get(i)) {
                        at.mark = [sp.src[0], sp.src[1], sp.dst[0], sp.dst[1]];
                    }
                    let c = v.canvas.center();
                    at.mods = egui::Modifiers::SHIFT;
                    ev_path(&mut at.events, &[c + egui::vec2(-300.0, 200.0), c + egui::vec2(-220.0, 240.0)], egui::Modifiers::SHIFT);
                }
                at.step = 222;
                at.t = Instant::now();
            }
            222 if at.events.is_empty() && el > 1.5 => {
                at.mods = egui::Modifiers::NONE;
                if let Some(sp) = app.devst.as_ref().and_then(|d| d.spot_sel.and_then(|i| d.settings.spots.get(i))) {
                    log(&format!("shift-drag: src moved {:?}, dst moved {:?}", [sp.src[0] - at.mark[0], sp.src[1] - at.mark[1]], [sp.dst[0] - at.mark[2], sp.dst[1] - at.mark[3]]));
                    at.mark = [sp.src[0], sp.src[1], sp.dst[0], sp.dst[1]];
                }
                // Grab the source handle 12 pt off-center and drag
                if let (Some(d), Some(v)) = (app.devst.as_ref(), app.devst.as_ref().and_then(|d| d.viewer.last_view()))
                    && let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get(i)) {
                        let k = v.base_to_screen(sp.src) + egui::vec2(10.0, 7.0);
                        ev_path(&mut at.events, &[k, k + egui::vec2(-60.0, 0.0)], egui::Modifiers::NONE);
                    }
                at.step = 223;
                at.t = Instant::now();
            }
            223 if at.events.is_empty() && el > 1.5 => {
                if let Some(sp) = app.devst.as_ref().and_then(|d| d.spot_sel.and_then(|i| d.settings.spots.get(i))) {
                    log(&format!("knob-drag (12pt off): src moved {:?}, dst moved {:?}", [sp.src[0] - at.mark[0], sp.src[1] - at.mark[1]], [sp.dst[0] - at.mark[2], sp.dst[1] - at.mark[3]]));
                    at.mark = [sp.src[0], sp.src[1], sp.dst[0], sp.dst[1]];
                }
                for _ in 0..3 {
                    ev_key(&mut at.events, egui::Key::ArrowRight);
                }
                at.step = 224;
                at.t = Instant::now();
            }
            224 if at.events.is_empty() && el > 1.5 => {
                if let Some(sp) = app.devst.as_ref().and_then(|d| d.spot_sel.and_then(|i| d.settings.spots.get(i))) {
                    log(&format!("arrow x3: src moved {:?}, photo still {:?}", [sp.src[0] - at.mark[0], sp.src[1] - at.mark[1]], app.current));
                }
                at.step = 250;
                at.t = Instant::now();
            }
            // ── Does a spot move while zooming with the wheel? ──
            250 if el > 1.0 => {
                if let (Some(d), Some(v)) = (app.devst.as_ref(), app.devst.as_ref().and_then(|d| d.viewer.last_view()))
                    && let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get(i)) {
                        at.mark = [sp.src[0], sp.src[1], sp.dst[0], sp.dst[1]];
                        let p = v.base_to_screen(sp.dst) + egui::vec2(30.0, 20.0);
                        at.events.push_back(vec![egui::Event::PointerMoved(p)]);
                        for _ in 0..6 {
                            at.events.push_back(vec![egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, 1.0), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE }]);
                            at.events.push_back(vec![]);
                        }
                        // Pan by middle-dragging over a spot (the spot must stay put)
                        let q = v.base_to_screen(sp.dst);
                        ev_middle_drag(&mut at.events, q, q + egui::vec2(-120.0, -80.0));
                    }
                at.step = 251;
                at.t = Instant::now();
            }
            251 if at.events.is_empty() && el > 2.0 => {
                if let Some(d) = app.devst.as_ref()
                    && let Some(sp) = d.spot_sel.and_then(|i| d.settings.spots.get(i)) {
                        log(&format!("wheel zoom + middle-drag: zoom {:?} center {:?}, dst moved {:?}, src moved {:?}", d.viewer.zoom, d.viewer.center, [sp.dst[0] - at.mark[2], sp.dst[1] - at.mark[3]], [sp.src[0] - at.mark[0], sp.src[1] - at.mark[1]]));
                    }
                shot(&mut at, "36_spot_wheel")
            }
            // Right after a large pan while zoomed: unrendered areas must show the backdrop (fit render), no blank tiles
            252 => {
                if let Some(d) = &mut app.devst {
                    d.viewer.zoom = Some(1.0);
                    d.viewer.center = [0.3, 0.4];
                }
                at.step = 2521;
                at.t = Instant::now();
            }
            2521 if el > 3.0 => {
                if let Some(d) = &mut app.devst {
                    d.viewer.center = [0.55, 0.6];
                    log(&format!("pan jump: underlay ready = {}", d.viewer.has_underlay()));
                }
                shot(&mut at, "36b_pan_underlay")
            }
            2522 => {
                if let Some(d) = &mut app.devst {
                    d.viewer.zoom = None;
                }
                at.step = 260;
                at.t = Instant::now();
            }
            // Red eye: add by click → add another by drag → Delete removes the selection
            260 if el > 1.0 => {
                let tool0 = app.devst.as_ref().map(|d| d.tool);
                if let Some(d) = &mut app.devst {
                    at.mark[1] = d.spot_sel.map(|i| i as f32).unwrap_or(-1.0);
                    d.tool = super::develop::Tool::RedEye;
                }
                log(&format!("before red eye: spots {:?} sel {:?}", app.devst.as_ref().map(|d| d.settings.spots.len()), app.devst.as_ref().map(|d| d.spot_sel)));
                at.mark[0] = tool0.map(|t| (t == super::develop::Tool::Spot) as u8 as f32).unwrap_or(0.0);
                at.step = 261;
                at.t = Instant::now();
            }
            261 if el > 1.0 => {
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    let at_ = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
                    ev_click(&mut at.events, at_(0.2, 0.2));
                    ev_drag(&mut at.events, at_(0.6, 0.6), at_(0.66, 0.65), 8);
                }
                at.step = 262;
                at.t = Instant::now();
            }
            262 if at.events.is_empty() && el > 2.0 => {
                let n = app.devst.as_ref().map(|d| d.settings.red_eye.len()).unwrap_or(0);
                let r = app.devst.as_ref().and_then(|d| d.settings.red_eye.last()).map(|r| r.radius);
                log(&format!("red eye after click+drag: {n} regions, last radius {r:?}"));
                shot(&mut at, "37_red_eye")
            }
            263 => {
                ev_key(&mut at.events, egui::Key::Delete);
                at.step = 264;
                at.t = Instant::now();
            }
            264 if at.events.is_empty() && el > 1.0 => {
                let n = app.devst.as_ref().map(|d| d.settings.red_eye.len()).unwrap_or(0);
                log(&format!("red eye after Delete: {n} regions, catalog-remove dialog open = {}", app.dlg.confirm_remove));
                if let Some(d) = &mut app.devst {
                    d.settings.red_eye.clear();
                    d.tool = super::develop::Tool::Spot;
                    d.spot_sel = (at.mark[1] >= 0.0).then_some(at.mark[1] as usize);
                }
                at.step = 225;
                at.t = Instant::now();
            }
            225 if el > 1.0 => {
                log(&format!("before Delete: spots {:?} sel {:?} tool {:?}", app.devst.as_ref().map(|d| d.settings.spots.len()), app.devst.as_ref().map(|d| d.spot_sel), app.devst.as_ref().map(|d| d.tool)));
                ev_key(&mut at.events, egui::Key::Delete);
                at.step = 204;
                at.t = Instant::now();
            }
            204 if at.events.is_empty() && el > 1.0 => {
                let n = app.devst.as_ref().map(|d| d.settings.spots.len()).unwrap_or(0);
                log(&format!("after Delete: spots {n}, catalog-remove dialog open = {}", app.dlg.confirm_remove));
                app.dlg.confirm_remove = false;
                super::develop::shortcut(app, egui::Key::M, egui::Modifiers::SHIFT);
                at.step = 205;
                at.t = Instant::now();
            }
            205 if el > 1.5 => {
                if let Some(r) = app.devst.as_ref().and_then(|d| d.viewer.last_view()).map(|v| v.image_rect) {
                    let at_ = |x: f32, y: f32| egui::pos2(r.left() + r.width() * x, r.top() + r.height() * y);
                    log(&format!("before draw: masks {:?}, armed {:?}", app.devst.as_ref().map(|d| d.settings.masks.len()), app.devst.as_ref().map(|d| d.mask_arm.is_some())));
                    ev_drag(&mut at.events, at_(0.30, 0.30), at_(0.45, 0.42), 10);
                }
                at.step = 206;
                at.t = Instant::now();
            }
            206 if at.events.is_empty() && el > 2.0 => {
                let shape = app.devst.as_ref().and_then(|d| d.settings.masks.last()).and_then(|m| m.components.first()).map(|c| format!("{:?}", c.shape));
                log(&format!("after draw: masks {:?}, armed {:?}, shape {shape:?}", app.devst.as_ref().map(|d| d.settings.masks.len()), app.devst.as_ref().map(|d| d.mask_arm.is_some())));
                // Grab the horizontal radius handle 12 pt off-center and drag 40 pt
                if let (Some(d), Some(v)) = (app.devst.as_ref(), app.devst.as_ref().and_then(|d| d.viewer.last_view()))
                    && let Some(crate::develop::settings::MaskShape::Radial { center, radius, .. }) = d.settings.masks.last().and_then(|m| m.components.first()).map(|c| c.shape.clone()) {
                        let hx = v.base_to_screen([center[0] + radius[0], center[1]]);
                        at.mark = [radius[0], radius[1], 0.0, 0.0];
                        ev_path(&mut at.events, &[hx + egui::vec2(9.0, 8.0), hx + egui::vec2(49.0, 8.0)], egui::Modifiers::NONE);
                    }
                at.step = 230;
                at.t = Instant::now();
            }
            230 if at.events.is_empty() && el > 1.5 => {
                let r = app.devst.as_ref().and_then(|d| d.settings.masks.last()).and_then(|m| m.components.first()).and_then(|c| match &c.shape {
                    crate::develop::settings::MaskShape::Radial { radius, .. } => Some(*radius),
                    _ => None,
                });
                log(&format!("radial handle grab (12pt off): radius {:?} -> {:?}", [at.mark[0], at.mark[1]], r));
                if let Some(d) = &mut app.devst {
                    d.viewer.zoom = Some(1.0);
                    at.mark = [d.viewer.center[0], d.viewer.center[1], 0.0, 0.0];
                }
                at.step = 207;
                at.t = Instant::now();
            }
            207 if el > 2.0 => {
                if let Some(v) = app.devst.as_ref().and_then(|d| d.viewer.last_view()) {
                    let c = v.canvas;
                    // Empty spot far from the handles (lower-right quadrant of the canvas)
                    ev_drag(&mut at.events, egui::pos2(c.right() - 120.0, c.bottom() - 120.0), egui::pos2(c.right() - 320.0, c.bottom() - 260.0), 10);
                }
                if let Some(d) = &app.devst {
                    at.mark = [d.viewer.center[0], d.viewer.center[1], 0.0, 0.0];
                }
                at.step = 208;
                at.t = Instant::now();
            }
            208 if at.events.is_empty() && el > 2.0 => {
                let shape = app.devst.as_ref().and_then(|d| d.settings.masks.last()).and_then(|m| m.components.first()).map(|c| format!("{:?}", c.shape));
                let c = app.devst.as_ref().map(|d| d.viewer.center).unwrap_or_default();
                log(&format!("after empty drag (zoomed): center {:?} -> {:?}, radial {shape:?}", [at.mark[0], at.mark[1]], c));
                shot(&mut at, "31_mask_input")
            }
            209 => {
                // ── Card reordering (drag to move) ──
                if let Some(d) = &mut app.devst {
                    d.tool = super::develop::Tool::None;
                    d.viewer.zoom = None;
                    d.settings.masks.clear();
                }
                app.prefs.dev_layout = super::devlayout::DevLayout::default();
                app.prefs.dev_layout.collapsed.clear();
                at.step = 240;
                at.t = Instant::now();
            }
            240 if el > 3.0 => shot(&mut at, "33_layout_default"),
            241 => {
                let head: Option<egui::Rect> = ctx.data(|d| d.get_temp(egui::Id::new(("card_head_rect", super::devlayout::Card::History as u8))));
                let screen = ctx.content_rect();
                if let Some(h) = head {
                    ev_path(&mut at.events, &[h.center(), h.center() + egui::vec2(0.0, 30.0), egui::pos2(screen.center().x, screen.bottom() - 40.0), egui::pos2(screen.center().x, screen.bottom() - 36.0)], egui::Modifiers::NONE);
                }
                at.step = 242;
                at.t = Instant::now();
            }
            242 if at.events.is_empty() && el > 2.5 => {
                let l = &app.prefs.dev_layout;
                log(&format!("layout after drag to bottom: left {:?} right {:?} bottom {:?}", l.left, l.right, l.bottom));
                shot(&mut at, "34_layout_bottom")
            }
            243 => {
                let head: Option<egui::Rect> = ctx.data(|d| d.get_temp(egui::Id::new(("card_head_rect", super::devlayout::Card::Presets as u8))));
                let cw = app.prefs.dev_layout.col_w;
                if let Some(h) = head {
                    ev_path(&mut at.events, &[h.center(), h.center() + egui::vec2(20.0, 0.0), egui::pos2(cw + 50.0, h.center().y + 100.0), egui::pos2(cw + 52.0, h.center().y + 104.0)], egui::Modifiers::NONE);
                }
                at.step = 244;
                at.t = Instant::now();
            }
            244 if at.events.is_empty() && el > 2.5 => {
                let l = &app.prefs.dev_layout;
                log(&format!("layout after drag to new column: left {:?} right {:?} bottom {:?}", l.left, l.right, l.bottom));
                shot(&mut at, "35_layout_two_cols")
            }
            245 => {
                // Column divider +60, outer edge -40
                let l = &app.prefs.dev_layout;
                at.mark = [l.col_w, l.col2_w, 0.0, 0.0];
                let w = l.col_w + l.col2_w + 9.0;
                let full = w - 16.0;
                let usable = full - 9.0;
                let x_div = 8.0 + usable * l.col_w / (l.col_w + l.col2_w) + 4.5;
                let y = ctx.content_rect().center().y;
                ev_path(&mut at.events, &[egui::pos2(x_div, y), egui::pos2(x_div + 20.0, y), egui::pos2(x_div + 60.0, y)], egui::Modifiers::NONE);
                at.step = 246;
                at.t = Instant::now();
            }
            246 if at.events.is_empty() && el > 1.5 => {
                let l = &app.prefs.dev_layout;
                let w = l.col_w + l.col2_w + 9.0;
                let y = ctx.content_rect().center().y + 100.0;
                log(&format!("inner divider drag: col_w {} -> {}, col2_w {} -> {}", at.mark[0], l.col_w, at.mark[1], l.col2_w));
                at.mark = [l.col_w, l.col2_w, 0.0, 0.0];
                ev_path(&mut at.events, &[egui::pos2(w - 2.0, y), egui::pos2(w - 22.0, y), egui::pos2(w - 42.0, y)], egui::Modifiers::NONE);
                at.step = 247;
                at.t = Instant::now();
            }
            247 if at.events.is_empty() && el > 1.5 => {
                let l = &app.prefs.dev_layout;
                log(&format!("outer edge drag: col_w {} -> {}, col2_w {} -> {}", at.mark[0], l.col_w, at.mark[1], l.col2_w));
                shot(&mut at, "37_layout_widths")
            }
            // Layout lock: header drag and width resize must be blocked → unlock and drag a card divider to resize height
            248 => {
                app.prefs.dev_layout = super::devlayout::DevLayout::default();
                app.prefs.dev_layout.collapsed.clear();
                app.prefs.dev_layout.locked = true;
                at.step = 249;
                at.t = Instant::now();
            }
            249 if el > 1.0 => {
                let head: Option<egui::Rect> = ctx.data(|d| d.get_temp(egui::Id::new(("card_head_rect", super::devlayout::Card::History as u8))));
                if let Some(h) = head {
                    ev_drag(&mut at.events, h.center(), h.center() + egui::vec2(900.0, 0.0), 12);
                }
                at.mark[0] = app.prefs.dev_layout.col_w;
                at.step = 2491;
                at.t = Instant::now();
            }
            2491 if at.events.is_empty() && el > 1.0 => {
                let l = &app.prefs.dev_layout;
                log(&format!("locked: right {:?}, left {:?} (변화 없어야 함)", l.right, l.left));
                app.prefs.dev_layout.locked = false;
                // Bottom edge of the photo card (between photo and presets)
                let film: Option<egui::Rect> = ctx.data(|d| d.get_temp(egui::Id::new(("card_head_rect", super::devlayout::Card::Presets as u8))));
                if let Some(h) = film {
                    let p = egui::pos2(h.center().x, h.top() - 3.0);
                    ev_drag(&mut at.events, p, p + egui::vec2(0.0, -120.0), 10);
                }
                at.step = 2492;
                at.t = Instant::now();
            }
            2492 if at.events.is_empty() && el > 1.5 => {
                let l = &app.prefs.dev_layout;
                log(&format!("card split drag: weights {:?} (사진 카드 비율이 줄어야 함)", l.weights));
                shot(&mut at, "38_card_heights")
            }
            2493 => {
                app.prefs.dev_layout = super::devlayout::DevLayout::default();
                at.step = 78;
            }
            77 => {
                at.step = 79;
                at.t = Instant::now();
            }
            // ── UI check: left filmstrip, per-theme HSL, new dialogs ──
            79 if el > 4.0 => shot(&mut at, "13_develop_film_left"),
            80 => {
                app.prefs.theme = super::theme::ThemeKind::Light;
                app.pending_theme = Some(super::theme::ThemeKind::Light);
                app.set_module(Module::Develop);
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Color;
                    d.settings.hsl[2].sat = 40.0;
                    d.settings.hsl[4].lum = -30.0;
                    d.hsl_band = 2;
                }
                app.left_tab_dev = 0;
                at.step = 81;
                at.t = Instant::now();
            }
            81 if el > 4.0 => shot(&mut at, "14_light_hsl"),
            82 => {
                app.prefs.theme = super::theme::ThemeKind::Midnight;
                app.pending_theme = Some(super::theme::ThemeKind::Midnight);
                at.step = 83;
                at.t = Instant::now();
            }
            83 if el > 2.0 => shot(&mut at, "15_midnight_hsl"),
            84 => {
                app.prefs.theme = super::theme::ThemeKind::Dark;
                app.pending_theme = Some(super::theme::ThemeKind::Dark);
                super::dialogs::open_prefs(app, "워터마크");
                at.step = 85;
                at.t = Instant::now();
            }
            85 if el > 1.5 => shot(&mut at, "16_prefs_watermark"),
            86 => {
                super::dialogs::open_prefs(app, "화면");
                at.step = 87;
                at.t = Instant::now();
            }
            87 if el > 1.5 => shot(&mut at, "17_prefs_view"),
            88 => {
                app.dlg.prefs_open = false;
                super::dialogs::open_export(app);
                at.step = 89;
                at.t = Instant::now();
            }
            89 if el > 1.5 => shot(&mut at, "18_export"),
            90 => {
                app.dlg.export = None;
                let mut pr = app.prefs.clone();
                pr.last_import_dir = at.src.to_string_lossy().to_string();
                app.dlg.open_import(&pr);
                at.step = 91;
                at.t = Instant::now();
            }
            91 if el > 2.0 => shot(&mut at, "19_import"),
            92 => {
                app.dlg.import = None;
                super::dialogs::open_wm_editor(app, "");
                at.step = 93;
                at.t = Instant::now();
            }
            93 if el > 1.5 => shot(&mut at, "20_wm_editor"),
            94 => {
                app.dlg.wm = None;
                app.dlg.smart = Some(super::dialogs::SmartEditor::new(None, tr!("별 3개 이상", "3 stars and up").into(), Default::default()));
                at.step = 95;
                at.t = Instant::now();
            }
            95 if el > 1.5 => shot(&mut at, "21_smart"),
            96 => {
                app.dlg.smart = None;
                app.prefs.theme = super::theme::ThemeKind::Light;
                app.pending_theme = Some(super::theme::ThemeKind::Light);
                super::dialogs::open_export(app);
                at.step = 97;
                at.t = Instant::now();
            }
            97 if el > 2.0 => shot(&mut at, "22_export_light"),
            98 => {
                app.dlg.export = None;
                app.prefs.theme = super::theme::ThemeKind::Dark;
                app.pending_theme = Some(super::theme::ThemeKind::Dark);
                at.step = 400;
            }
            // Stacks: select 3 non-adjacent photos in Develop → stack them → check expand/collapse indicators in the grid
            400 => {
                for sid in app.cat.photos.iter().map(|p| p.stack_id).filter(|s| *s != 0).collect::<std::collections::HashSet<_>>() {
                    let ids: Vec<_> = app.cat.photos.iter().filter(|p| p.stack_id == sid).map(|p| p.id).collect();
                    let _ = app.cat.unstack(&ids);
                }
                app.expanded_stacks.clear();
                app.recompute_visible();
                app.set_module(Module::Develop);
                let pick: Vec<_> = app.visible.iter().copied().enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, id)| id).take(3).collect();
                app.select_single(pick[0]);
                app.selected = pick.clone();
                app.sel_set = pick.iter().copied().collect();
                super::tools::stack_selected(app);
                app.recompute_visible();
                let sid = app.cat.get(pick[0]).map(|p| p.stack_id).unwrap_or(0);
                log(&format!("stack from develop: picked {} → stack {} size {}, toast {:?}", pick.len(), sid, app.cat.stack_size(sid), app.toasts.last().map(|t| t.text.clone())));
                app.set_module(Module::Library);
                app.set_lib_view(LibView::Grid);
                app.expanded_stacks.insert(sid);
                app.recompute_visible();
                let pos: Vec<usize> = app.visible.iter().enumerate().filter(|(_, id)| app.cat.get(**id).map(|p| p.stack_id == sid).unwrap_or(false)).map(|(i, _)| i).collect();
                log(&format!("expanded stack positions in grid: {pos:?} (연속이어야 함)"));
                at.mark[0] = sid as f32;
                at.step = 401;
                at.t = Instant::now();
            }
            401 if el > 2.5 => shot(&mut at, "43_stack_open"),
            402 => {
                app.expanded_stacks.clear();
                app.recompute_visible();
                at.step = 403;
                at.t = Instant::now();
            }
            403 if el > 2.0 => {
                at.pending_shot = None;
                shot(&mut at, "44_stack_closed");
                at.step = 4030;
            }
            4030 if at.pending_shot.is_none() => at.step = 4031,
            // Panorama window
            4031 => {
                let pick: Vec<_> = app.visible.iter().copied().take(3).collect();
                app.selected = pick.clone();
                app.sel_set = pick.iter().copied().collect();
                super::tools::open_pano(app);
                log(&format!("pano dialog open: {}", app.pano.is_some()));
                at.step = 4032;
                at.t = Instant::now();
            }
            4032 if el > 1.5 => shot(&mut at, "45_pano_dialog"),
            4033 => {
                app.pano = None;
                // Split before/after: raise exposure so the halves differ
                app.set_module(Module::Develop);
                if let Some(d) = &mut app.devst {
                    d.settings.exposure = 1.0;
                    d.before_after = super::develop::BeforeAfter::Split;
                    d.split_pos = 0.4;
                    d.viewer.zoom = None;
                }
                at.step = 4034;
                at.t = Instant::now();
            }
            4034 if el > 3.0 => shot(&mut at, "46_split"),
            // Split handle cursor: cursor shape over the handle, after dragging, and far away
            4035 => {
                if let Some(v) = app.devst.as_ref().and_then(|d| d.viewer.last_view().map(|v| (v.canvas, d.split_pos))) {
                    let (r, sp) = v;
                    let x = r.left() + r.width() * sp;
                    at.events.push_back(vec![egui::Event::PointerMoved(egui::pos2(x, r.center().y))]);
                    at.mark = [x, r.center().y, r.left(), r.right()];
                }
                at.step = 4036;
                at.t = Instant::now();
            }
            4036 if el > 0.5 => {
                log(&format!("split cursor on handle: {:?}", ctx.output(|o| o.cursor_icon)));
                let (x, y) = (at.mark[0], at.mark[1]);
                ev_drag(&mut at.events, egui::pos2(x, y), egui::pos2(x + 120.0, y), 6);
                at.step = 4037;
                at.t = Instant::now();
            }
            4037 if el > 1.0 => {
                log(&format!("split cursor after drag: {:?} pos {:?}", ctx.output(|o| o.cursor_icon), app.devst.as_ref().map(|d| d.split_pos)));
                // Move left in several steps like a real mouse
                let from = egui::pos2(at.mark[0] + 120.0, at.mark[1]);
                let to = egui::pos2(at.mark[2] + 40.0, at.mark[1] - 80.0);
                for k in 1..=10 {
                    at.events.push_back(vec![egui::Event::PointerMoved(from + (to - from) * (k as f32 / 10.0))]);
                }
                at.step = 4038;
                at.t = Instant::now();
            }
            4038 if el > 0.5 => {
                log(&format!("split cursor far left: {:?}", ctx.output(|o| o.cursor_icon)));
                at.events.push_back(vec![egui::Event::PointerMoved(egui::pos2(at.mark[3] - 40.0, at.mark[1] + 60.0))]);
                at.step = 4039;
                at.t = Instant::now();
            }
            4039 if el > 0.5 => {
                log(&format!("split cursor far right: {:?}", ctx.output(|o| o.cursor_icon)));
                at.step = 4040;
            }
            4040 => {
                if let Some(d) = &mut app.devst {
                    d.before_after = super::develop::BeforeAfter::Off;
                    d.settings.exposure = 0.0;
                }
                // Match total exposure: relative to the current photo
                let pick: Vec<_> = app.visible.iter().copied().take(4).collect();
                app.select_single(pick[0]);
                app.selected = pick.clone();
                app.sel_set = pick.iter().copied().collect();
                let info = |app: &App| -> Vec<String> {
                    pick.iter().map(|id| {
                        let p = app.cat.get(*id).unwrap();
                        let ex = if app.devst.as_ref().map(|d| d.id == *id).unwrap_or(false) { app.devst.as_ref().unwrap().settings.exposure } else { p.settings().exposure };
                        format!("{} {} f/{:?} ISO{:?} → {:+.2}EV", p.file_name, p.meta.exposure_label(), p.meta.fnumber, p.meta.iso, ex)
                    }).collect()
                };
                super::develop::match_exposures(app);
                log(&format!("match exposures: {:?} toast {:?}", info(app), app.toasts.last().map(|t| t.text.clone())));
                at.step = 404;
            }
            // Per-photo view position: zoom/pan A → B → back to A should restore the same spot
            404 => {
                app.set_module(Module::Develop);
                let a = app.visible[0];
                app.select_single(a);
                at.step = 405;
                at.t = Instant::now();
            }
            405 if el > 1.0 => {
                if let Some(d) = &mut app.devst {
                    d.viewer.zoom = Some(1.0);
                    d.viewer.center = [0.31, 0.62];
                }
                at.step = 406;
                at.t = Instant::now();
            }
            406 if el > 1.0 => {
                let b = app.visible[1];
                app.select_single(b);
                at.step = 407;
                at.t = Instant::now();
            }
            407 if el > 1.0 => {
                let zb = app.devst.as_ref().map(|d| d.viewer.zoom);
                let a = app.visible[0];
                app.select_single(a);
                log(&format!("other photo zoom {zb:?}"));
                at.step = 408;
                at.t = Instant::now();
            }
            408 if el > 1.0 => {
                let v = app.devst.as_ref().map(|d| (d.viewer.zoom, d.viewer.center));
                log(&format!("view memory: back to first photo → {v:?} (기대 Some(1.0), [0.31, 0.62])"));
                at.step = 4100;
            }
            // Reference view: second photo as reference, edit the first
            4100 => {
                if let Some(d) = &mut app.devst {
                    d.viewer.zoom = None;
                }
                let (a, b) = (app.visible[0], app.visible[1]);
                super::develop::set_reference(app, b);
                app.select_single(a);
                at.step = 4101;
                at.t = Instant::now();
            }
            4101 if el > 3.0 => shot(&mut at, "47_reference"),
            4102 => {
                app.ref_view = false;
                // Soft proofing + gamut warning
                app.prefs.proof_gamut = true;
                super::proof_ui::toggle(app);
                at.step = 4103;
                at.t = Instant::now();
            }
            4103 if el > 3.0 => {
                let pn = super::proof_ui::profile_name(app);
                log(&format!("proof: on {} profile {:?} name {pn}", app.proof.on, app.prefs.proof_profile));
                shot(&mut at, "48_proof");
            }
            4104 => {
                app.proof.on = false;
                app.prefs.proof_gamut = false;
                super::catalog_ui::open_new(app);
                at.step = 4105;
                at.t = Instant::now();
            }
            4105 if el > 1.0 => shot(&mut at, "49_new_catalog"),
            4106 => {
                app.catui.new_dlg = None;
                let path = crate::catalogs::new_catalog_path(&crate::config::data_dir().join("cats"), "테스트 카탈로그");
                let r = crate::catalogs::create(&path, &app.cat, true);
                let presets = crate::catalog::Catalog::open(&path).map(|c| c.presets().len()).unwrap_or(0);
                // Catalog switching (app restart) is skipped in self-test so no new window is left behind
                log(&format!("new catalog: {r:?} at {} presets {presets} (원본 {}) toast {:?}", path.display(), app.cat.presets().len(), app.toasts.last().map(|t| t.text.clone())));
                super::catalog_ui::start_backup(app, false);
                at.step = 4107;
                at.t = Instant::now();
            }
            4107 if app.catui.backup_rx.is_none() => {
                log(&format!("backup: toast {:?} age {:?}", app.toasts.last().map(|t| t.text.clone()), crate::catalogs::last_backup_age_days(&app.cat.path)));
                super::dialogs::open_prefs(app, "저장소");
                at.step = 4108;
                at.t = Instant::now();
            }
            4108 if el > 1.0 => shot(&mut at, "50_prefs_catalog"),
            4109 => {
                app.dlg.prefs_open = false;
                // People: only when models are available (DARKROOM_AI_DIR)
                if crate::imaging::people::ready() {
                    app.set_module(Module::Library);
                    app.set_source(Source::All);
                    super::people_ui::start_scan(app);
                    at.step = 4110;
                } else {
                    log("people: models not present — skip");
                    at.step = 78;
                }
                at.t = Instant::now();
            }
            4110 if app.people.job.is_none() || el > 120.0 => {
                let per_photo: Vec<(String, usize)> = app.cat.photos.iter().map(|p| (p.file_name.clone(), app.cat.faces.iter().filter(|f| f.photo == p.id).count())).collect();
                log(&format!("people scan: {:.1}s faces {} persons {} scanned {} per photo {per_photo:?} toast {:?}", el, app.cat.faces.len(), app.cat.persons.len(), app.cat.face_scanned.len(), app.toasts.last().map(|t| t.text.clone())));
                // For checking grouping, naming and the people view: group the found faces into one person
                let ids: Vec<(i64, i64)> = app.cat.faces.iter().map(|f| (f.id, crate::imaging::people::NEW_BASE)).collect();
                if !ids.is_empty() {
                    let _ = app.cat.apply_face_groups(&ids);
                    if let Some(pid) = app.cat.persons.last().map(|p| p.id) {
                        let _ = app.cat.rename_person(pid, "테스트 인물");
                        app.set_source(Source::Person(pid));
                        app.recompute_visible();
                        log(&format!("person view: {} photos", app.visible.len()));
                    }
                }
                at.step = 4111;
                at.t = Instant::now();
            }
            4111 if el > 2.0 => shot(&mut at, "51_people"),
            4112 => {
                app.set_source(Source::All);
                at.step = 4120;
            }
            // Lens profile: log the profile chosen per photo
            4120 => {
                app.set_module(Module::Develop);
                let ids: Vec<_> = app.visible.clone();
                let mut out = Vec::new();
                for id in ids {
                    if let Some(p) = app.cat.get(id) {
                        let path = p.path.clone();
                        let lens = crate::imaging::meta::raw_lens_model(&path).or_else(|| crate::imaging::meta::canon_focal_name(&path)).unwrap_or_default();
                        let lid = crate::imaging::meta::canon_lens_type(&path);
                        let lf = crate::develop::lensfun::db().and_then(|db| db.find(&lens, 1.0).map(|l| l.model.clone()));
                        out.push(format!("{} [{lens} #{lid:?}] → lensfun {lf:?}", p.file_name));
                    }
                }
                log(&format!("lens profiles: {out:?}"));
                // Enable lens correction and open the Optics tab
                let first = app.visible[0];
                app.select_single(first);
                at.step = 4117;
                at.t = Instant::now();
            }
            4117 if el > 2.0 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Optics;
                    d.settings.lens.profile_enable = true;
                }
                app.commit("autotest lens profile");
                at.step = 4118;
                at.t = Instant::now();
            }
            4118 if el > 3.0 => shot(&mut at, "50b_lens"),
            4119 => {
                if let Some(d) = &mut app.devst {
                    d.settings.lens.profile_enable = false;
                }
                app.commit("autotest lens off");
                // Point color: add two on the Color tab and select one
                at.step = 4121;
                at.t = Instant::now();
            }
            4121 if el > 2.0 => {
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Color;
                    d.settings.point_colors.push(crate::develop::settings::PointColor { hue: 25.0, sat: 0.45, lum: 0.55, sat_shift: -60.0, lum_shift: 30.0, ..Default::default() });
                    d.settings.point_colors.push(crate::develop::settings::PointColor { hue: 210.0, sat: 0.4, lum: 0.4, hue_shift: 40.0, ..Default::default() });
                    d.pc_sel = 0;
                }
                app.commit("autotest point color");
                at.step = 4122;
                at.t = Instant::now();
            }
            4122 if el > 3.0 => shot(&mut at, "52_point_color"),
            // AI masks (only when models are available)
            4123 => {
                let ok = crate::imaging::aimask::missing_bytes(crate::imaging::aimask::AiKind::Subject) == 0;
                if let Some(d) = &mut app.devst {
                    d.panel_tab = super::develop::PanelTab::Light;
                    d.tool = super::develop::Tool::Mask;
                    if ok {
                        d.ai_request = Some(crate::imaging::aimask::AiKind::Subject);
                    }
                }
                if !ok {
                    log("ai mask: models not present — skip");
                    at.step = 78;
                } else {
                    at.step = 4124;
                }
                at.t = Instant::now();
            }
            4124 if (el > 1.0 && app.ai_mask_job.is_none()) || el > 180.0 => {
                let m = app.devst.as_ref().map(|d| d.settings.masks.iter().map(|m| (m.name.clone(), m.components.iter().map(|c| c.shape.kind_name()).collect::<Vec<_>>())).collect::<Vec<_>>());
                log(&format!("ai mask: {:.1}s masks {m:?} toast {:?}", el, app.toasts.last().map(|t| t.text.clone())));
                if let Some(d) = &mut app.devst
                    && let Some(i) = d.settings.masks.len().checked_sub(1) {
                        d.settings.masks[i].adj.exposure = 0.6;
                        d.mask_sel = Some(i);
                        d.overlay = true;
                    }
                app.commit("autotest ai mask");
                at.step = 4125;
                at.t = Instant::now();
            }
            4125 if el > 3.0 => shot(&mut at, "53_ai_mask"),
            // Erase: one short stroke in the middle
            4126 => {
                if let Some(d) = &mut app.devst {
                    d.overlay = false;
                    d.tool = super::develop::Tool::Spot;
                    let mut sp = crate::develop::settings::Spot { dst: [0.48, 0.45], radius: 0.02, remove: true, heal: false, ..Default::default() };
                    sp.path = vec![[0.48, 0.45], [0.52, 0.47], [0.55, 0.5]];
                    sp.src = sp.dst;
                    d.settings.spots.push(sp);
                    d.spot_sel = Some(d.settings.spots.len() - 1);
                }
                app.commit("autotest remove");
                at.step = 4127;
                at.t = Instant::now();
            }
            4127 if (el > 1.0 && app.fill_job.is_none() && app.devst.as_ref().map(|d| d.settings.spots.iter().all(|s| !s.needs_fill())).unwrap_or(true)) || el > 180.0 => {
                let f = app.devst.as_ref().and_then(|d| d.settings.spots.last().and_then(|s| s.fill.clone()));
                log(&format!("remove fill: {:.1}s {:?} toast {:?}", el, f.map(|f| (f.key, f.bbox)), app.toasts.last().map(|t| t.text.clone())));
                at.step = 4128;
                at.t = Instant::now();
            }
            4128 if el > 3.0 => shot(&mut at, "54_remove"),
            4129 => {
                at.step = 78;
            }
            78 => {
                log("done");
                app.save_prefs();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                at.step = 100;
            }
            _ => {}
        }
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    app.autotest = Some(at);
}
