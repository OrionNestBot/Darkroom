//! Dialogs: import, export, watermark editor, smart collections, preferences, copy/sync settings, presets.

use super::app::*;
use super::theme::*;
use super::form;
use super::widgets::{self, slider};
use crate::catalog::{ColorLabel, Flag, SmartRule, SmartRules};
use crate::config;
use crate::develop::settings::{DevelopSettings, SettingGroups};
use crate::export::watermark::{self, Watermark, WmAlign, WmKind, WmSize};
use crate::export::{self, ExportSettings, Format, ResizeMode, SharpenFor, SharpenLevel};
use crate::imaging::decode::Rgba8;
use crate::imaging::meta::{self, PhotoMeta};
use crossbeam_channel::{Receiver, unbounded};
use egui::{Color32, TextureHandle, TextureOptions, Ui, vec2};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Default)]
pub struct Dialogs {
    pub import: Option<ImportDialog>,
    pub export: Option<ExportDialog>,
    pub wm: Option<WmEditor>,
    pub smart: Option<SmartEditor>,
    pub prefs_open: bool,
    pub prefs_page: usize,
    pub copy_settings: Option<SettingGroups>,
    pub sync_settings: Option<SettingGroups>,
    pub save_preset: Option<(String, String, SettingGroups)>,
    pub new_collection: Option<String>,
    pub new_collection_add: bool,
    pub rename_collection: Option<(i64, String)>,
    pub confirm_remove: bool,
    pub keyword_input: String,
    pub focus_search: bool,
    pub grid_cols: usize,
    pub film_follow: bool,
    pub pending_module: Option<Module>,
    pub preset_filter: String,
    pub lr_import: Option<LrImport>,
    pub lrcat: Option<super::lrcat_import::Dialog>,
}

impl Dialogs {
    pub fn any_modal(&self) -> bool {
        self.import.is_some()
            || self.export.is_some()
            || self.wm.is_some()
            || self.smart.is_some()
            || self.prefs_open
            || self.copy_settings.is_some()
            || self.sync_settings.is_some()
            || self.save_preset.is_some()
            || self.new_collection.is_some()
            || self.rename_collection.is_some()
            || self.confirm_remove
            || self.lr_import.is_some()
            || self.lrcat.is_some()
    }

    pub fn open_import(&mut self, prefs: &Prefs) {
        let mut plan = ImportPlan::default();
        if !prefs.last_import_dir.is_empty() {
            plan.source = PathBuf::from(&prefs.last_import_dir);
        }
        self.import = Some(ImportDialog { plan, scan: None, scan_rx: None });
    }
}


pub fn show_all(app: &mut App, ctx: &egui::Context) {
    import_dialog(app, ctx);
    export_dialog(app, ctx);
    wm_editor(app, ctx);
    smart_editor(app, ctx);
    prefs_dialog(app, ctx);
    small_dialogs(app, ctx);
    lr_import_dialog(app, ctx);
    super::lrcat_import::dialog(app, ctx);
}

// ─────────────────────────── Import ───────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImportMode {
    #[default]
    Add,
    Copy,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Organize {
    #[default]
    ByDate,
    Flat,
}

#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub source: PathBuf,
    pub recursive: bool,
    pub mode: ImportMode,
    pub dest: PathBuf,
    pub organize: Organize,
    pub skip_dupes: bool,
    pub preset: Option<i64>,
    pub keywords: String,
}

impl Default for ImportPlan {
    fn default() -> Self {
        let pics = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures")).unwrap_or_default();
        Self {
            source: PathBuf::new(),
            recursive: true,
            mode: ImportMode::Add,
            dest: pics.join("Darkroom"),
            organize: Organize::ByDate,
            skip_dupes: true,
            preset: None,
            keywords: String::new(),
        }
    }
}

pub struct ScanResult {
    pub files: Vec<PathBuf>,
    pub new_files: Vec<PathBuf>,
}

pub struct ImportDialog {
    pub plan: ImportPlan,
    pub scan: Option<ScanResult>,
    scan_rx: Option<Receiver<Vec<PathBuf>>>,
}

pub enum ImportMsg {
    Batch(Vec<(PathBuf, u64, PhotoMeta)>),
    Progress(usize, usize),
    Done(Vec<String>),
}

pub struct ImportProgress {
    pub rx: Receiver<ImportMsg>,
    pub done: usize,
    pub total: usize,
    pub import_id: i64,
    pub preset: Option<(DevelopSettings, SettingGroups)>,
    pub keywords: Vec<String>,
    pub added: usize,
}

pub fn scan_dir(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                if recursive && !name.starts_with('.') && !name.starts_with('$') {
                    stack.push(p);
                }
            } else if p.extension().and_then(|x| x.to_str()).map(config::is_supported_ext).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn read_meta_any(path: &Path) -> PhotoMeta {
    let raw = path.extension().and_then(|e| e.to_str()).map(config::is_raw_ext).unwrap_or(false);
    let m = if raw { meta::read_raw_meta(path).or_else(|| meta::read_exif(path)) } else { meta::read_exif(path) };
    let mut m = m.unwrap_or_default();
    if m.orientation == 0 {
        m.orientation = 1;
    }
    if m.capture_time.is_none() {
        // Fall back to the file modification time when there is no EXIF date
        if let Ok(md) = std::fs::metadata(path)
            && let Ok(t) = md.modified() {
                let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                m.capture_time = Some(unix_to_string(secs));
            }
    }
    m
}

/// Unix time to "YYYY-MM-DD HH:MM:SS" (UTC, no local time conversion).
fn unix_to_string(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Gregorian calendar conversion from 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

pub fn start_import(app: &mut App, plan: ImportPlan) {
    if app.import.is_some() {
        app.toast(tr!("이미 가져오는 중입니다", "Already importing"));
        return;
    }
    let known = app.cat.known_paths();
    let files = scan_dir(&plan.source, plan.recursive);
    let files: Vec<PathBuf> = if plan.skip_dupes || plan.mode == ImportMode::Add {
        files.into_iter().filter(|p| !known.contains(&p.to_string_lossy().to_lowercase())).collect()
    } else {
        files
    };
    if files.is_empty() {
        app.toast(tr!("가져올 새 사진이 없습니다", "No new photos to import"));
        return;
    }
    let preset = plan.preset.and_then(|id| app.cat.presets().iter().find(|p| p.id == id).cloned()).map(|p| (p.settings, p.groups));
    let keywords: Vec<String> = plan.keywords.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let import_id = app.cat.next_import_id();
    let (tx, rx) = unbounded();
    let total = files.len();
    app.import = Some(ImportProgress { rx, done: 0, total, import_id, preset, keywords, added: 0 });
    app.prefs.last_import_dir = plan.source.to_string_lossy().to_string();
    let ctx_repaint = app.dlg.film_follow; // placeholder to keep borrowck simple
    let _ = ctx_repaint;
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("import".into())
        .spawn(move || {
            let mut errors = Vec::new();
            let mut done = 0;
            for chunk in files.chunks(48) {
                let res: Vec<Result<(PathBuf, u64, PhotoMeta), String>> = chunk
                    .par_iter()
                    .map(|src| {
                        let m = read_meta_any(src);
                        let path = match plan.mode {
                            ImportMode::Add => src.clone(),
                            ImportMode::Copy | ImportMode::Move => {
                                let sub = match (plan.organize, &m.capture_time) {
                                    (Organize::ByDate, Some(t)) if t.len() >= 10 => PathBuf::from(&t[..4]).join(&t[..10]),
                                    (Organize::ByDate, _) => PathBuf::from(tr!("날짜 없음", "No date")),
                                    (Organize::Flat, _) => PathBuf::new(),
                                };
                                let dir = plan.dest.join(sub);
                                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                                let mut dst = dir.join(src.file_name().unwrap_or_default());
                                let src_size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                                if dst.exists() {
                                    let same = std::fs::metadata(&dst).map(|m| m.len() == src_size).unwrap_or(false);
                                    if !same {
                                        let stem = dst.file_stem().unwrap_or_default().to_string_lossy().to_string();
                                        let ext = dst.extension().unwrap_or_default().to_string_lossy().to_string();
                                        let mut n = 2;
                                        while dst.exists() {
                                            dst = dir.join(format!("{stem}-{n}.{ext}"));
                                            n += 1;
                                        }
                                    } else {
                                        return Ok((dst, src_size, m));
                                    }
                                }
                                if plan.mode == ImportMode::Move {
                                    if std::fs::rename(src, &dst).is_err() {
                                        std::fs::copy(src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
                                        let _ = std::fs::remove_file(src);
                                    }
                                } else {
                                    std::fs::copy(src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
                                }
                                dst
                            }
                        };
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        Ok((path, size, m))
                    })
                    .collect();
                let mut batch = Vec::new();
                for r in res {
                    match r {
                        Ok(v) => batch.push(v),
                        Err(e) => errors.push(e),
                    }
                }
                done += chunk.len();
                let _ = tx.send(ImportMsg::Batch(batch));
                let _ = tx.send(ImportMsg::Progress(done, total));
            }
            let _ = tx.send(ImportMsg::Done(errors));
        })
        .expect("import thread");
    app.set_source(Source::LastImport);
}

pub fn pump_import(app: &mut App) {
    let Some(im) = &mut app.import else { return };
    let mut finished = None;
    let mut batches = Vec::new();
    for msg in im.rx.try_iter() {
        match msg {
            ImportMsg::Batch(b) => batches.push(b),
            ImportMsg::Progress(d, t) => {
                im.done = d;
                im.total = t;
            }
            ImportMsg::Done(errs) => finished = Some(errs),
        }
    }
    let (import_id, preset, kws) = (im.import_id, im.preset.clone(), im.keywords.clone());
    let auto_lens = app.prefs.auto_lens;
    let read_xmp = !app.prefs.skip_xmp_on_import;
    for b in batches {
        let items: Vec<_> = b
            .into_iter()
            .map(|(p, size, m)| {
                let raw = p.extension().and_then(|e| e.to_str()).map(config::is_raw_ext).unwrap_or(false);
                let mut dev = preset.as_ref().map(|(s, g)| {
                    let mut d = DevelopSettings::default_for(raw);
                    d.copy_groups_from(s, g);
                    d
                });
                if auto_lens && raw {
                    dev.get_or_insert_with(|| DevelopSettings::default_for(raw)).lens.profile_enable = true;
                }
                // Sidecar XMP (edited in another app) takes precedence over the import preset
                if read_xmp && dev.is_none()
                    && let Ok(t) = std::fs::read_to_string(p.with_extension("xmp"))
                        && t.contains("camera-raw-settings") {
                            dev = super::tools::settings_from_xmp(&t, raw);
                        }
                (p, size, m, dev, kws.clone())
            })
            .collect();
        let n = items.len();
        match app.cat.add_photos(&items, import_id) {
            Ok(_) => {
                if let Some(im) = &mut app.import {
                    im.added += n;
                }
            }
            Err(e) => app.toast_err(trf!("카탈로그 기록 실패: {e}", "Couldn't write to catalog: {e}")),
        }
        app.visible_dirty = true;
    }
    if let Some(errs) = finished {
        let added = app.import.as_ref().map(|i| i.added).unwrap_or(0);
        app.import = None;
        app.visible_dirty = true;
        if errs.is_empty() {
            app.toast(trf!("{added}장을 가져왔습니다", "Imported {added} photos"));
        } else {
            app.toast_err(trf!("{added}장 가져옴, {}건 실패: {}", "Imported {added} photos, {} failed: {}", errs.len(), errs[0]));
        }
    }
}

fn import_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(dlg) = &mut app.dlg.import else { return };
    if let Some(rx) = &dlg.scan_rx
        && let Ok(files) = rx.try_recv() {
            let known = app.cat.known_paths();
            let new_files = files.iter().filter(|p| !known.contains(&p.to_string_lossy().to_lowercase())).cloned().collect();
            dlg.scan = Some(ScanResult { files, new_files });
            dlg.scan_rx = None;
        }
    let mut go = false;
    let mut cancel = false;
    let presets = app.cat.presets();
    let n_new = dlg.scan.as_ref().map(|s| s.new_files.len()).unwrap_or(0);
    let can_go = n_new > 0 && (dlg.plan.mode == ImportMode::Add || !dlg.plan.dest.as_os_str().is_empty());
    let (_, _, close) = form::modal(
        ctx,
        "import",
        tr!("사진 가져오기", "Import Photos"),
        "",
        vec2(640.0, 620.0),
        |ui| {
            egui::ScrollArea::vertical().id_salt("import_body").auto_shrink([false, true]).show(ui, |ui| {
                let plan = &mut dlg.plan;
                form::card(ui, tr!("원본", "Original"), "", |ui| {
                    let mut src = plan.source.to_string_lossy().to_string();
                    if form::folder_row(ui, tr!("폴더", "Folder"), &mut src) {
                        plan.source = PathBuf::from(src);
                        dlg.scan = None;
                    }
                    if form::switch_row(ui, tr!("하위 폴더 포함", "Include subfolders"), &mut plan.recursive, "") {
                        dlg.scan = None;
                    }
                    if dlg.scan.is_none() && dlg.scan_rx.is_none() && plan.source.is_dir() {
                        let (tx, rx) = unbounded();
                        let (src, rec) = (plan.source.clone(), plan.recursive);
                        let _ = std::thread::Builder::new().name("scan".into()).spawn(move || {
                            let _ = tx.send(scan_dir(&src, rec));
                        });
                        dlg.scan_rx = Some(rx);
                    }
                    form::row(ui, tr!("찾은 사진", "Photos found"), |ui| match (&dlg.scan, &dlg.scan_rx) {
                        (Some(s), _) => {
                            ui.label(egui::RichText::new(trf!("새 사진 {}장", "{} new photos", s.new_files.len())).color(STRONG()));
                            ui.label(egui::RichText::new(trf!("· 전체 {} · 이미 있음 {}", "· {} total · {} already imported", s.files.len(), s.files.len() - s.new_files.len())).color(TEXT_WEAK()));
                        }
                        (None, Some(_)) => {
                            ui.spinner();
                            ui.label(egui::RichText::new(tr!("폴더 검사 중…", "Scanning folder…")).color(TEXT_WEAK()));
                        }
                        _ => {
                            ui.label(egui::RichText::new(tr!("폴더를 고르세요", "Choose a folder")).color(TEXT_DIM()));
                        }
                    });
                });
                form::card(ui, tr!("가져오는 방식", "Import method"), "", |ui| {
                    form::row(ui, tr!("방식", "Method"), |ui| form::seg(ui, &mut plan.mode, &[(ImportMode::Add, tr!("그대로 추가", "Add in place")), (ImportMode::Copy, tr!("복사", "Copy")), (ImportMode::Move, tr!("이동", "Move"))]));
                    form::hint(
                        ui,
                        match plan.mode {
                            ImportMode::Add => tr!("파일을 옮기지 않고 지금 위치를 카탈로그에 등록합니다.", "Registers the current location in the catalog without moving files."),
                            ImportMode::Copy => tr!("아래 폴더로 복사한 뒤 등록합니다 (원본은 그대로).", "Copies to the folder below, then registers (originals untouched)."),
                            ImportMode::Move => tr!("아래 폴더로 옮긴 뒤 등록합니다.", "Moves to the folder below, then registers."),
                        },
                    );
                    if plan.mode != ImportMode::Add {
                        let mut d = plan.dest.to_string_lossy().to_string();
                        if form::folder_row(ui, tr!("대상 폴더", "Destination folder"), &mut d) {
                            plan.dest = PathBuf::from(d);
                        }
                        form::row(ui, tr!("정리", "Organize"), |ui| form::seg(ui, &mut plan.organize, &[(Organize::ByDate, tr!("날짜별 폴더", "By date")), (Organize::Flat, tr!("한 폴더에", "Into one folder"))]));
                        form::switch_row(ui, tr!("중복 건너뛰기", "Skip duplicates"), &mut plan.skip_dupes, tr!("이미 가져온 사진은 다시 복사하지 않음", "Don't copy photos already imported"));
                    }
                });
                form::card(ui, tr!("가져오면서 적용", "Apply during import"), "", |ui| {
                    form::row(ui, tr!("현상 프리셋", "Develop preset"), |ui| {
                        let cur = plan.preset.and_then(|id| presets.iter().find(|p| p.id == id)).map(|p| p.name.clone()).unwrap_or_else(|| tr!("없음", "None").into());
                        egui::ComboBox::from_id_salt("imp_preset").truncate().width(260.0).selected_text(cur).show_ui(ui, |ui| {
                            ui.selectable_value(&mut plan.preset, None, tr!("없음", "None"));
                            for p in presets.iter() {
                                ui.selectable_value(&mut plan.preset, Some(p.id), format!("{} / {}", p.group, p.name));
                            }
                        });
                    });
                    form::row(ui, tr!("키워드", "Keywords"), |ui| ui.add(egui::TextEdit::singleline(&mut plan.keywords).hint_text(tr!("쉼표로 구분", "Comma separated")).desired_width(260.0)));
                    form::switch_row(ui, tr!("렌즈 교정", "Lens Corrections"), &mut app.prefs.auto_lens, tr!("RAW에 렌즈 프로파일 자동 적용", "Apply lens profile automatically to RAW"));
                });
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if form::primary(ui, &trf!("{n_new}장 가져오기", "Import {n_new} photos"), can_go).clicked() {
                    go = true;
                }
                if form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                    cancel = true;
                }
            });
        },
    );
    if go {
        let plan = app.dlg.import.take().unwrap().plan;
        start_import(app, plan);
    } else if close || cancel {
        app.dlg.import = None;
    }
}

// ─────────────────────────── Export ───────────────────────────

pub struct ExportDialog {
    pub s: ExportSettings,
    pub presets: Vec<ExportSettings>,
    pub preset_name: String,
    pub count: usize,
}

pub fn open_export(app: &mut App) {
    if app.module == Module::Develop {
        app.commit_develop_now();
    }
    let count = if app.module == Module::Develop {
        app.selected.len().max(1)
    } else {
        app.targets().len()
    };
    if count == 0 {
        app.toast(tr!("내보낼 사진을 선택하세요", "Select photos to export"));
        return;
    }
    app.dlg.export = Some(ExportDialog {
        s: exp_settings(app),
        presets: app.cat.kv_get_json("export_presets").unwrap_or_default(),
        preset_name: String::new(),
        count,
    });
}

fn export_targets(app: &App) -> Vec<crate::catalog::PhotoId> {
    if app.module == Module::Develop {
        // In develop, export the whole filmstrip selection (or the current photo if none)
        if app.selected.len() > 1 { app.selected.clone() } else { app.current.into_iter().collect() }
    } else {
        app.targets()
    }
}

/// Quick settings by purpose: (name, description, apply).
fn quick_exports() -> Vec<(&'static str, &'static str, fn(&mut ExportSettings))> {
    vec![
        (tr!("인스타그램", "Instagram"), tr!("1080px · sRGB · 위치 제거", "1080px · sRGB · location removed"), |s| {
            s.format = Format::Jpeg;
            s.quality = 92;
            s.resize = true;
            s.resize_mode = ResizeMode::LongEdge;
            s.size_a = 1080;
            s.color_space = export::icc::ColorSpace::Srgb;
            s.sharpen = true;
            s.sharpen_for = SharpenFor::Screen;
            s.sharpen_level = SharpenLevel::Standard;
            s.remove_location = true;
            s.limit_size = false;
        }),
        (tr!("웹 · SNS", "Web · Social"), tr!("2048px · sRGB · 화면 샤프닝", "2048px · sRGB · screen sharpening"), |s| {
            s.format = Format::Jpeg;
            s.quality = 88;
            s.resize = true;
            s.resize_mode = ResizeMode::LongEdge;
            s.size_a = 2048;
            s.color_space = export::icc::ColorSpace::Srgb;
            s.sharpen = true;
            s.sharpen_for = SharpenFor::Screen;
            s.limit_size = false;
        }),
        (tr!("메신저", "Messenger"), tr!("2048px · 1MB 이하", "2048px · under 1MB"), |s| {
            s.format = Format::Jpeg;
            s.quality = 90;
            s.resize = true;
            s.resize_mode = ResizeMode::LongEdge;
            s.size_a = 2048;
            s.limit_size = true;
            s.limit_kb = 1000;
            s.remove_location = true;
        }),
        (tr!("인쇄", "Print"), tr!("원본 · 300ppi · Adobe RGB", "Original · 300ppi · Adobe RGB"), |s| {
            s.format = Format::Jpeg;
            s.quality = 100;
            s.resize = false;
            s.ppi = 300;
            s.color_space = export::icc::ColorSpace::AdobeRgb;
            s.sharpen = true;
            s.sharpen_for = SharpenFor::Matte;
            s.limit_size = false;
        }),
        (tr!("보관", "Archive"), tr!("원본 · 16비트 TIFF", "Original · 16-bit TIFF"), |s| {
            s.format = Format::Tiff;
            s.tiff_16bit = true;
            s.resize = false;
            s.sharpen = false;
            s.metadata = export::metadata::MetaMode::All;
        }),
    ]
}

fn export_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(dlg) = &mut app.dlg.export else { return };
    let mut go = false;
    let mut cancel = false;
    let mut edit_wm: Option<String> = None;
    let wm_list: Vec<Watermark> = app.cat.kv_get_json("watermarks").unwrap_or_default();
    let example = app.current.and_then(|c| app.cat.get(c)).map(|p| format!("{}.{}", export::file_stem(p, &dlg.s, dlg.s.seq_start), dlg.s.format.ext()));
    let count = dlg.count;
    let mut save_preset = false;
    let mut del_preset: Option<String> = None;
    let summary = export_summary(&dlg.s);
    let (_, _, close) = form::modal(
        ctx,
        "export",
        &trf!("내보내기 — {count}장", "Export — {count} photos"),
        "",
        vec2(980.0, 720.0),
        |ui| {
            let s = &mut dlg.s;
            ui.horizontal_top(|ui| {
                // ── Left: presets ──
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    ui.label(egui::RichText::new(tr!("빠른 설정", "Quick settings")).size(11.0).color(TEXT_DIM()));
                    for (name, desc, f) in quick_exports() {
                        let on = s.preset_name == name;
                        let (click, _) = form::list_item(ui, name, desc, on, |_| ());
                        if click {
                            f(s);
                            s.preset_name = name.to_string();
                        }
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(tr!("내 프리셋", "My presets")).size(11.0).color(TEXT_DIM()));
                    if dlg.presets.is_empty() {
                        ui.label(egui::RichText::new(tr!("아래에서 현재 설정을 저장하세요", "Save the current settings below")).size(11.0).color(TEXT_DIM()));
                    }
                    for p in &dlg.presets {
                        let on = s.preset_name == p.preset_name;
                        let (click, del) = form::list_item(ui, &p.preset_name, "", on, |ui| ui.small_button("×").on_hover_text(tr!("삭제", "Delete")).clicked());
                        if del {
                            del_preset = Some(p.preset_name.clone());
                        } else if click {
                            *s = p.clone();
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut dlg.preset_name).hint_text(tr!("새 프리셋 이름", "New preset name")).desired_width(130.0));
                        if form::small(ui, tr!("저장", "Save")).clicked() && !dlg.preset_name.trim().is_empty() {
                            save_preset = true;
                        }
                    });
                });
                ui.add_space(14.0);
                // ── Right: settings cards ──
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("export_body").auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(ui.available_width() - 6.0);
                        form::card(ui, tr!("저장 위치", "Export location"), "", |ui| {
                            form::row(ui, tr!("폴더", "Folder"), |ui| {
                                form::seg(ui, &mut s.same_as_original, &[(false, tr!("지정한 폴더", "Specific folder")), (true, tr!("원본과 같은 폴더", "Same folder as original"))]);
                            });
                            if !s.same_as_original {
                                form::folder_row(ui, "", &mut s.folder);
                            }
                            form::row(ui, tr!("하위 폴더", "Subfolder"), |ui| {
                                widgets::toggle(ui, &mut s.use_subfolder);
                                ui.add_enabled(s.use_subfolder, egui::TextEdit::singleline(&mut s.subfolder).hint_text(tr!("폴더 이름", "Folder name")).desired_width(200.0));
                            });
                            form::row(ui, tr!("같은 이름이 있으면", "If the file exists"), |ui| {
                                form::seg(ui, &mut s.conflict, &[(export::Conflict::Unique, tr!("새 이름", "New name")), (export::Conflict::Overwrite, tr!("덮어쓰기", "Overwrite")), (export::Conflict::Skip, tr!("건너뛰기", "Skip"))]);
                            });
                        });
                        form::card(ui, tr!("파일 이름", "File name"), "", |ui| {
                            let d = if s.rename { "" } else { tr!("원본 파일 이름 그대로", "Keep original file name") };
                            form::switch_row(ui, tr!("이름 바꾸기", "Rename"), &mut s.rename, d);
                            if s.rename {
                                form::row(ui, tr!("형식", "Format"), |ui| {
                                    ui.add(egui::TextEdit::singleline(&mut s.template).desired_width(260.0));
                                    ui.menu_button(tr!("토큰 ▼", "Tokens ▼"), |ui| {
                                        for (t, n) in export::NAME_TOKENS {
                                            if ui.button(format!("{t}  {}", crate::i18n::t(n))).clicked() {
                                                s.template.push_str(t);
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                                form::row(ui, tr!("사용자 텍스트", "Custom text"), |ui| ui.add(egui::TextEdit::singleline(&mut s.custom_text).desired_width(160.0)));
                                form::row(ui, tr!("시작 번호", "Start number"), |ui| ui.add(egui::DragValue::new(&mut s.seq_start).range(0..=999999)));
                            }
                            if let Some(ex) = &example {
                                form::hint(ui, &trf!("예: {ex}", "e.g. {ex}"));
                            }
                        });
                        form::card(ui, tr!("형식 · 화질", "Format · Quality"), "", |ui| {
                            form::row(ui, tr!("형식", "Format"), |ui| {
                                let opts: Vec<(Format, &str)> = Format::ALL.iter().map(|f| (*f, f.name())).collect();
                                form::seg(ui, &mut s.format, &opts);
                            });
                            match s.format {
                                Format::Jpeg => {
                                    form::row(ui, tr!("품질", "Quality"), |ui| ui.add(egui::Slider::new(&mut s.quality, 1..=100)));
                                    form::row(ui, tr!("용량 제한", "Limit file size"), |ui| {
                                        widgets::toggle(ui, &mut s.limit_size);
                                        ui.add_enabled(s.limit_size, egui::DragValue::new(&mut s.limit_kb).range(50..=50000).suffix(" KB"));
                                    });
                                }
                                Format::Tiff => {
                                    form::row(ui, tr!("비트 깊이", "Bit depth"), |ui| form::seg(ui, &mut s.tiff_16bit, &[(false, tr!("8비트", "8-bit")), (true, tr!("16비트", "16-bit"))]));
                                }
                                Format::Png => {}
                            }
                            form::row(ui, tr!("색공간", "Color space"), |ui| {
                                let opts: Vec<(export::icc::ColorSpace, &str)> = export::icc::ColorSpace::ALL.iter().map(|c| (*c, c.name())).collect();
                                form::seg(ui, &mut s.color_space, &opts);
                            });
                            form::hint(ui, tr!("웹·SNS는 sRGB가 안전합니다.", "sRGB is safest for web and social."));
                        });
                        form::card(ui, tr!("크기 · 해상도", "Size · Resolution"), "", |ui| {
                            let d = if s.resize { "" } else { tr!("원본 해상도 그대로", "Keep original resolution") };
                            form::switch_row(ui, tr!("크기 조정", "Resize"), &mut s.resize, d);
                            if s.resize {
                                form::row(ui, tr!("기준", "Fit"), |ui| {
                                    egui::ComboBox::from_id_salt("rmode").truncate().width(110.0).selected_text(s.resize_mode.name()).show_ui(ui, |ui| {
                                        for m in ResizeMode::ALL {
                                            ui.selectable_value(&mut s.resize_mode, m, m.name());
                                        }
                                    });
                                    match s.resize_mode {
                                        ResizeMode::LongEdge | ResizeMode::ShortEdge => {
                                            ui.add(egui::DragValue::new(&mut s.size_a).range(16..=30000).suffix(" px"));
                                        }
                                        ResizeMode::WidthHeight => {
                                            ui.add(egui::DragValue::new(&mut s.size_a).range(16..=30000).prefix(tr!("가로 ", "Width ")).suffix(" px"));
                                            ui.add(egui::DragValue::new(&mut s.size_b).range(16..=30000).prefix(tr!("세로 ", "Height ")).suffix(" px"));
                                        }
                                        ResizeMode::Megapixels => {
                                            ui.add(egui::DragValue::new(&mut s.megapixels).range(0.1..=200.0).speed(0.1).suffix(" MP"));
                                        }
                                        ResizeMode::Percent => {
                                            ui.add(egui::DragValue::new(&mut s.percent).range(1.0..=400.0).suffix(" %"));
                                        }
                                    }
                                });
                                form::switch_row(ui, tr!("확대 안 함", "Don't enlarge"), &mut s.dont_enlarge, tr!("원본보다 작은 사진은 그대로", "Photos smaller than the target stay as they are"));
                            }
                            form::row(ui, tr!("해상도", "Resolution"), |ui| ui.add(egui::DragValue::new(&mut s.ppi).range(1..=2400).suffix(" ppi")));
                        });
                        form::card(ui, tr!("출력 샤프닝", "Output sharpening"), "", |ui| {
                            form::switch_row(ui, tr!("샤프닝", "Sharpening"), &mut s.sharpen, "");
                            if s.sharpen {
                                form::row(ui, tr!("용도", "For"), |ui| form::seg(ui, &mut s.sharpen_for, &[(SharpenFor::Screen, tr!("화면", "Display")), (SharpenFor::Matte, tr!("무광 용지", "Matte paper")), (SharpenFor::Glossy, tr!("유광 용지", "Glossy paper"))]));
                                form::row(ui, tr!("강도", "Amount"), |ui| form::seg(ui, &mut s.sharpen_level, &[(SharpenLevel::Low, tr!("약하게", "Low")), (SharpenLevel::Standard, tr!("표준", "Standard")), (SharpenLevel::High, tr!("강하게", "High"))]));
                            }
                        });
                        form::card(ui, tr!("메타데이터", "Metadata"), "", |ui| {
                            form::row(ui, tr!("포함", "Include"), |ui| {
                                egui::ComboBox::from_id_salt("metam").truncate().width(220.0).selected_text(s.metadata.name()).show_ui(ui, |ui| {
                                    for m in export::metadata::MetaMode::ALL {
                                        ui.selectable_value(&mut s.metadata, m, m.name());
                                    }
                                });
                            });
                            form::switch_row(ui, tr!("위치 정보 제거", "Remove location info"), &mut s.remove_location, tr!("GPS 좌표를 지웁니다", "Removes GPS coordinates"));
                        });
                        form::card(ui, tr!("워터마크", "Watermark"), "", |ui| {
                            form::switch_row(ui, tr!("워터마크", "Watermark"), &mut s.watermark, "");
                            form::row(ui, tr!("사용할 워터마크", "Watermark to use"), |ui| {
                                ui.add_enabled_ui(s.watermark && !wm_list.is_empty(), |ui| {
                                    egui::ComboBox::from_id_salt("wmsel")
                                        .width(200.0)
                                        .selected_text(if s.watermark_name.is_empty() { tr!("선택…", "Choose…") } else { &s.watermark_name })
                                        .show_ui(ui, |ui| {
                                            for w in &wm_list {
                                                ui.selectable_value(&mut s.watermark_name, w.name.clone(), &w.name);
                                            }
                                        });
                                });
                                if form::small(ui, tr!("편집…", "Edit…")).clicked() {
                                    edit_wm = Some(s.watermark_name.clone());
                                }
                                if form::small(ui, tr!("+ 새로", "+ New")).clicked() {
                                    edit_wm = Some("\u{0}new".into());
                                }
                            });
                            if wm_list.is_empty() {
                                form::hint(ui, tr!("저장된 워터마크가 없습니다 — '+ 새로'로 만드세요 (환경설정 › 워터마크에서도 관리)", "No saved watermarks — create one with '+ New' (also managed in Preferences › Watermark)"));
                            }
                        });
                        form::card(ui, tr!("가리기 · 테두리", "Privacy · Border"), "", |ui| {
                            form::switch_row(ui, tr!("얼굴 자동 가리기", "Auto face privacy"), &mut s.auto_faces, tr!("사진마다 얼굴을 찾아 가립니다", "Finds and covers faces in every photo"));
                            if s.auto_faces {
                                form::row(ui, tr!("방식", "Method"), |ui| {
                                    let opts: Vec<(crate::develop::settings::PrivacyKind, &str)> = crate::develop::settings::PrivacyKind::ALL.iter().map(|k| (*k, k.name())).collect();
                                    form::seg(ui, &mut s.faces_kind, &opts);
                                });
                            }
                            form::hint(ui, tr!("현상에서 직접 지정한 가리기 영역은 항상 적용됩니다.", "Privacy regions set in Develop are always applied."));
                            form::row(ui, tr!("테두리 두께", "Border width"), |ui| ui.add(egui::Slider::new(&mut s.border_pct, 0.0..=10.0).suffix(" %").step_by(0.5)).on_hover_text(tr!("긴 변 대비 (0 = 없음)", "Relative to long edge (0 = none)")));
                            if s.border_pct > 0.0 {
                                form::row(ui, tr!("테두리 색", "Border color"), |ui| form::seg(ui, &mut s.border_color, &[([255u8, 255, 255], tr!("흰색", "White")), ([0, 0, 0], tr!("검정", "Black")), ([128, 128, 128], tr!("회색", "Gray"))]));
                            }
                        });
                        form::card(ui, tr!("완료 후", "After export"), "", |ui| {
                            form::switch_row(ui, tr!("폴더 열기", "Open folder"), &mut s.open_folder, tr!("내보내기가 끝나면 탐색기로 엽니다", "Opens Explorer when the export finishes"));
                        });
                    });
                });
            });
        },
        |ui| {
            form::footer_note(ui, &summary);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if form::primary(ui, &trf!("{count}장 내보내기", "Export {count} photos"), true).clicked() {
                    go = true;
                }
                if form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                    cancel = true;
                }
            });
        },
    );
    if save_preset
        && let Some(dlg) = &mut app.dlg.export {
            let mut p = dlg.s.clone();
            p.preset_name = dlg.preset_name.trim().to_string();
            dlg.presets.retain(|x| x.preset_name != p.preset_name);
            dlg.presets.push(p.clone());
            dlg.s.preset_name = p.preset_name;
            dlg.preset_name.clear();
            app.cat.kv_set_json("export_presets", &dlg.presets);
        }
    if let Some(n) = del_preset
        && let Some(dlg) = &mut app.dlg.export {
            dlg.presets.retain(|x| x.preset_name != n);
            if dlg.s.preset_name == n {
                dlg.s.preset_name.clear();
            }
            app.cat.kv_set_json("export_presets", &dlg.presets);
        }
    if let Some(n) = edit_wm {
        if n == "\u{0}new" {
            open_wm_editor(app, "");
            if let Some(ed) = &mut app.dlg.wm {
                ed.wm = Watermark { name: trf!("워터마크 {}", "Watermark {}", ed.list.len() + 1), ..Default::default() };
            }
        } else {
            open_wm_editor(app, &n);
        }
    }
    if go {
        let dlg = app.dlg.export.take().unwrap();
        app.cat.kv_set_json("export_last", &dlg.s);
        let ids = export_targets(app);
        let items: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                let p = app.cat.get(*id)?.clone();
                let s = match &app.devst {
                    Some(d) if d.id == *id => d.settings.clone(),
                    _ => p.settings(),
                };
                Some((p, s))
            })
            .collect();
        let wm = if dlg.s.watermark { wm_list.into_iter().find(|w| w.name == dlg.s.watermark_name) } else { None };
        if dlg.s.watermark && wm.is_none() {
            app.toast_err(tr!("선택한 워터마크를 찾을 수 없어 워터마크 없이 내보냅니다", "Selected watermark not found — exporting without a watermark"));
        }
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let total = items.len();
        let c2 = ctx.clone();
        export::spawn(
            export::ExportJob { items, settings: dlg.s, watermark: wm, artist: app.prefs.artist.clone(), copyright: app.prefs.copyright.clone() },
            tx,
            cancel.clone(),
            move || c2.request_repaint(),
        );
        app.export = Some(ExportProgress { rx, cancel, done: 0, total, current: String::new() });
    } else if close || cancel {
        app.dlg.export = None;
    }
}

// ─────────────────────────── Watermark editor ───────────────────────────

pub struct WmEditor {
    pub wm: Watermark,
    pub list: Vec<Watermark>,
    base: Option<Rgba8>,
    tex: Option<TextureHandle>,
    last: Option<Watermark>,
    font_filter: String,
    ctx: watermark::TokenCtx,
}

pub fn open_wm_editor(app: &mut App, name: &str) {
    let list: Vec<Watermark> = app.cat.kv_get_json("watermarks").unwrap_or_default();
    let mut wm = list.iter().find(|w| w.name == name).cloned().or_else(|| list.first().cloned()).unwrap_or_default();
    if wm.text.contains("{artist}") && app.prefs.artist.is_empty() && list.is_empty() {
        // Without an artist name, drop the name (set it under Preferences > General > Artist to use {artist})
        wm.text = format!("© {}", watermark::current_year());
    }
    // Preview background: current photo's preview cache, else its thumbnail, else gray
    let base = app.current.and_then(|c| app.cat.get(c)).and_then(|p| {
        let pv = super::workers::cache_path(p.id, p.thumb_ver, super::workers::ThumbKind::Preview);
        let th = super::workers::cache_path(p.id, p.thumb_ver, super::workers::ThumbKind::Grid);
        Rgba8::load_jpeg_file(&pv).or_else(|_| Rgba8::load_jpeg_file(&th)).ok().map(|i| i.fit(1200))
    });
    let ctx = app.current.and_then(|c| app.cat.get(c)).map(|p| export::token_ctx(p, &app.prefs.artist, &app.prefs.copyright)).unwrap_or_default();
    app.dlg.wm = Some(WmEditor { wm, list, base, tex: None, last: None, font_filter: String::new(), ctx });
}

fn rgb_edit(ui: &mut Ui, c: &mut [u8; 3]) -> bool {
    let mut col = Color32::from_rgb(c[0], c[1], c[2]);
    let r = ui.color_edit_button_srgba(&mut col);
    if r.changed() {
        *c = [col.r(), col.g(), col.b()];
        true
    } else {
        false
    }
}

fn wm_editor(app: &mut App, ctx: &egui::Context) {
    let Some(ed) = &mut app.dlg.wm else { return };
    let mut saved: Option<Vec<Watermark>> = None;
    let mut closing = false;
    // Refresh preview
    if ed.last.as_ref() != Some(&ed.wm) {
        let mut img = ed.base.clone().unwrap_or(Rgba8 { w: 900, h: 600, data: vec![90; 900 * 600 * 4] });
        watermark::apply_rgba8(&mut img.data, img.w as usize, img.h as usize, &ed.wm, &ed.ctx);
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.data);
        match &mut ed.tex {
            Some(t) if t.size() == [img.w as usize, img.h as usize] => t.set(ci, TextureOptions::LINEAR),
            _ => ed.tex = Some(ctx.load_texture("wm_preview", ci, TextureOptions::LINEAR)),
        }
        ed.last = Some(ed.wm.clone());
    }
    let is_saved = ed.list.contains(&ed.wm);
    let can_save = !ed.wm.name.trim().is_empty();
    let mut act = 0u8;
    let (_, _, close) = form::modal(
        ctx,
        "wm_editor",
        tr!("워터마크 편집", "Edit Watermark"),
        tr!("크기·위치는 사진 대비 비율이라 내보내는 해상도와 상관없이 같게 들어갑니다.", "Size and position are relative to the photo, so they look the same at any export resolution."),
        vec2(1160.0, 760.0),
        |ui| {
            ui.horizontal_top(|ui| {
                // Preview
                // Left-anchored column: a centered layout would shrink around its middle and push the right column out of the dialog
                let pw = (ui.available_width() - 384.0 - 12.0 - 2.0 * ui.spacing().item_spacing.x).max(300.0);
                ui.allocate_ui_with_layout(vec2(pw, 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.set_width(pw);
                    if let Some(t) = &ed.tex {
                        let size = t.size_vec2();
                        let k = (pw / size.x).min(560.0 / size.y);
                        let (r, _) = ui.allocate_exact_size(size * k, egui::Sense::hover());
                        ui.painter().rect_filled(r.expand(1.0), 4.0, BORDER());
                        ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    ui.label(egui::RichText::new(tr!("현재 사진으로 미리보기", "Preview with current photo")).size(11.0).color(TEXT_DIM()));
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(384.0);
                    egui::ScrollArea::vertical().id_salt("wm_body").auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(372.0);
                        let w = &mut ed.wm;
                        form::card(ui, "", "", |ui| {
                            form::row(ui, tr!("불러오기", "Load"), |ui| {
                                egui::ComboBox::from_id_salt("wm_list").truncate().width(200.0).selected_text(if ed.list.is_empty() { tr!("저장된 것 없음", "Nothing saved") } else { tr!("목록에서 선택…", "Choose from list…") }).show_ui(ui, |ui| {
                                    for x in &ed.list {
                                        if ui.selectable_label(x.name == w.name, &x.name).clicked() {
                                            *w = x.clone();
                                        }
                                    }
                                });
                            });
                            form::row(ui, tr!("이름", "Name"), |ui| ui.add(egui::TextEdit::singleline(&mut w.name).desired_width(200.0)));
                            form::row(ui, tr!("종류", "Type"), |ui| form::seg(ui, &mut w.kind, &[(WmKind::Text, tr!("텍스트", "Text")), (WmKind::Graphic, tr!("이미지", "Image"))]));
                        });
                        if w.kind == WmKind::Text {
                            form::card(ui, tr!("텍스트", "Text"), "", |ui| {
                                ui.add(egui::TextEdit::multiline(&mut w.text).desired_rows(2).desired_width(ui.available_width()));
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(egui::RichText::new(tr!("넣기", "Insert")).size(11.0).color(TEXT_DIM()));
                                    for (t, n) in watermark::TOKENS {
                                        if ui.small_button(crate::i18n::t(n)).on_hover_text(*t).clicked() {
                                            w.text.push_str(t);
                                        }
                                    }
                                });
                            });
                            form::card(ui, tr!("글꼴", "Font"), "", |ui| {
                                form::row(ui, tr!("글꼴", "Font"), |ui| {
                                    egui::ComboBox::from_id_salt("wm_font").truncate().width(200.0).selected_text(&w.font_name).show_ui(ui, |ui| {
                                        ui.add(egui::TextEdit::singleline(&mut ed.font_filter).hint_text(tr!("검색", "Search")));
                                        let f = ed.font_filter.to_lowercase();
                                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                                            for font in crate::fonts::system_fonts().iter().filter(|x| f.is_empty() || x.name.to_lowercase().contains(&f)) {
                                                if ui.selectable_label(font.name == w.font_name, &font.name).clicked() {
                                                    w.font_name = font.name.clone();
                                                    w.font_path = font.path.to_string_lossy().to_string();
                                                    w.font_index = font.index;
                                                }
                                            }
                                        });
                                    });
                                });
                                form::row(ui, tr!("정렬", "Align"), |ui| form::seg(ui, &mut w.align, &[(WmAlign::Left, tr!("왼쪽", "Left")), (WmAlign::Center, tr!("가운데", "Center")), (WmAlign::Right, tr!("오른쪽", "Right"))]));
                                form::row(ui, tr!("색상", "Color"), |ui| rgb_edit(ui, &mut w.color));
                                slider(ui, tr!("불투명도", "Opacity"), &mut w.text_opacity, 0.0, 100.0, 100.0, 0, "wto");
                                slider(ui, tr!("줄 간격", "Line spacing"), &mut w.line_spacing, 50.0, 300.0, 100.0, 0, "wls");
                                slider(ui, tr!("자간", "Letter spacing"), &mut w.letter_spacing, -50.0, 200.0, 0.0, 0, "wlsp");
                            });
                            form::card(ui, tr!("그림자 · 외곽선", "Shadow · Outline"), "", |ui| {
                                form::switch_row(ui, tr!("그림자", "Shadow"), &mut w.shadow, "");
                                if w.shadow {
                                    slider(ui, tr!("불투명도", "Opacity"), &mut w.shadow_opacity, 0.0, 100.0, 40.0, 0, "wso");
                                    slider(ui, tr!("거리", "Distance"), &mut w.shadow_offset, 0.0, 100.0, 10.0, 0, "wsoff");
                                    slider(ui, tr!("번짐", "Blur"), &mut w.shadow_radius, 0.0, 100.0, 20.0, 0, "wsr");
                                    slider(ui, tr!("각도", "Angle"), &mut w.shadow_angle, -180.0, 180.0, -45.0, 0, "wsa");
                                }
                                form::switch_row(ui, tr!("외곽선", "Outline"), &mut w.stroke, "");
                                if w.stroke {
                                    form::row(ui, tr!("외곽선 색", "Outline color"), |ui| rgb_edit(ui, &mut w.stroke_color));
                                    slider(ui, tr!("두께", "Thickness"), &mut w.stroke_width, 0.0, 100.0, 10.0, 0, "wsw");
                                    slider(ui, tr!("불투명도", "Opacity"), &mut w.stroke_opacity, 0.0, 100.0, 80.0, 0, "wsop");
                                }
                            });
                        } else {
                            form::card(ui, tr!("이미지", "Image"), tr!("투명 배경 PNG를 권장합니다.", "A PNG with transparent background is recommended."), |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(egui::TextEdit::singleline(&mut w.image_path).desired_width(240.0));
                                    if form::small(ui, tr!("찾아보기…", "Browse…")).clicked()
                                        && let Some(f) = rfd::FileDialog::new().add_filter(tr!("이미지", "Image"), &["png", "jpg", "jpeg"]).pick_file() {
                                            w.image_path = f.to_string_lossy().to_string();
                                        }
                                });
                            });
                        }
                        form::card(ui, tr!("배치", "Placement"), "", |ui| {
                            slider(ui, tr!("불투명도", "Opacity"), &mut w.opacity, 0.0, 100.0, 85.0, 0, "wop");
                            form::row(ui, tr!("크기", "Size"), |ui| form::seg(ui, &mut w.size_mode, &[(WmSize::Proportional, tr!("비율", "Aspect")), (WmSize::Fit, tr!("맞춤", "Fit")), (WmSize::Fill, tr!("채우기", "Fill"))]));
                            if w.size_mode == WmSize::Proportional {
                                slider(ui, tr!("너비 %", "Width %"), &mut w.size, 1.0, 100.0, 22.0, 0, "wsz");
                            }
                            slider(ui, tr!("가로 여백", "Horizontal margin"), &mut w.inset_h, 0.0, 100.0, 4.0, 0, "wih");
                            slider(ui, tr!("세로 여백", "Vertical margin"), &mut w.inset_v, 0.0, 100.0, 4.0, 0, "wiv");
                            form::row(ui, tr!("위치", "Location"), |ui| {
                                // 3×3 position picker
                                let (rect, _) = ui.allocate_exact_size(vec2(84.0, 62.0), egui::Sense::hover());
                                ui.painter().rect_filled(rect, 5.0, WIDGET());
                                for r in 0..3u8 {
                                    for c in 0..3u8 {
                                        let a = r * 3 + c;
                                        let cell = egui::Rect::from_min_size(rect.min + vec2(c as f32 * 28.0, r as f32 * 20.67), vec2(28.0, 20.67));
                                        let resp = ui.interact(cell, ui.id().with(("anchor", a)), egui::Sense::click());
                                        let on = w.anchor == a;
                                        ui.painter().circle_filled(cell.center(), if on { 5.0 } else { 3.0 }, if on { ACCENT } else if resp.hovered() { TEXT() } else { TEXT_DIM() });
                                        if resp.clicked() {
                                            w.anchor = a;
                                        }
                                    }
                                }
                                ui.add_space(12.0);
                                if form::small(ui, "⟲").on_hover_text(tr!("왼쪽으로 90°", "90° left")).clicked() {
                                    w.rotation = (w.rotation + 3) % 4;
                                }
                                if form::small(ui, "⟳").on_hover_text(tr!("오른쪽으로 90°", "90° right")).clicked() {
                                    w.rotation = (w.rotation + 1) % 4;
                                }
                                ui.label(mono(format!("{}°", w.rotation as u32 * 90)).color(TEXT_WEAK()));
                            });
                        });
                    });
                });
            });
        },
        |ui| {
            if form::secondary(ui, tr!("삭제", "Delete")).on_hover_text(tr!("이 이름의 워터마크를 목록에서 지웁니다", "Removes the watermark with this name from the list")).clicked() {
                act = 1;
            }
            if form::secondary(ui, tr!("새로 만들기", "Create new")).clicked() {
                act = 2;
            }
            if form::secondary(ui, tr!("Lightroom에서…", "From Lightroom…")).on_hover_text(tr!("Lightroom 워터마크(.lrtemplate) 가져오기", "Import Lightroom watermark (.lrtemplate)")).clicked() {
                act = 3;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if form::primary(ui, if is_saved { tr!("저장됨", "Saved") } else { tr!("저장", "Save") }, !is_saved && can_save).clicked() {
                    act = 4;
                }
                if form::secondary(ui, tr!("닫기", "Close")).clicked() {
                    closing = true;
                }
            });
        },
    );
    if let Some(ed) = &mut app.dlg.wm {
        match act {
            1 => {
                let n = ed.wm.name.clone();
                ed.list.retain(|x| x.name != n);
                saved = Some(ed.list.clone());
            }
            2 => ed.wm = Watermark { name: trf!("워터마크 {}", "Watermark {}", ed.list.len() + 1), ..Default::default() },
            3 => {
                let start = crate::lrpreset::watermark_dir().unwrap_or_default();
                if let Some(fs) = rfd::FileDialog::new().set_directory(start).add_filter(tr!("Lightroom 워터마크", "Lightroom watermark"), &["lrtemplate"]).pick_files() {
                    for f in fs {
                        if let Ok(t) = std::fs::read_to_string(&f)
                            && let Ok((nw, _)) = crate::lrpreset::parse_lr_watermark(&t, &f.file_stem().unwrap_or_default().to_string_lossy()) {
                                ed.list.retain(|x| x.name != nw.name);
                                ed.list.push(nw.clone());
                                ed.wm = nw;
                            }
                    }
                    saved = Some(ed.list.clone());
                }
            }
            4 => {
                let n = ed.wm.name.clone();
                ed.list.retain(|x| x.name != n);
                ed.list.push(ed.wm.clone());
                ed.list.sort_by(|a, b| a.name.cmp(&b.name));
                saved = Some(ed.list.clone());
            }
            _ => {}
        }
    }
    if let Some(list) = saved {
        app.cat.kv_set_json("watermarks", &list);
        let name = app.dlg.wm.as_ref().map(|e| e.wm.name.clone()).unwrap_or_default();
        if let Some(ex) = &mut app.dlg.export {
            ex.s.watermark_name = name;
            ex.s.watermark = true;
        }
        app.toast(tr!("워터마크를 저장했습니다", "Watermark saved"));
    }
    if close || closing {
        app.dlg.wm = None;
    }
}

// ─────────────────────────── Smart collections ───────────────────────────

pub struct SmartEditor {
    pub id: Option<i64>,
    pub name: String,
    pub rules: SmartRules,
}

impl SmartEditor {
    pub fn new(id: Option<i64>, name: String, mut rules: SmartRules) -> Self {
        if rules.rules.is_empty() {
            rules.match_all = true;
            rules.rules.push(SmartRule::RatingAtLeast(3));
        }
        Self { id, name, rules }
    }
}

fn rule_name(r: &SmartRule) -> &'static str {
    match r {
        SmartRule::RatingAtLeast(_) => tr!("별점 ≥", "Rating ≥"),
        SmartRule::RatingAtMost(_) => tr!("별점 ≤", "Rating ≤"),
        SmartRule::FlagIs(_) => tr!("깃발", "Flag"),
        SmartRule::LabelIs(_) => tr!("색상 라벨", "Color label"),
        SmartRule::KeywordContains(_) => tr!("키워드 포함", "Keyword contains"),
        SmartRule::TextContains(_) => tr!("텍스트 검색", "Text search"),
        SmartRule::CameraContains(_) => tr!("카메라", "Camera"),
        SmartRule::LensContains(_) => tr!("렌즈", "Lens"),
        SmartRule::CapturedAfter(_) => tr!("촬영일 이후", "Captured after"),
        SmartRule::CapturedBefore(_) => tr!("촬영일 이전", "Captured before"),
        SmartRule::FileTypeIs(_) => tr!("파일 형식", "File type"),
        SmartRule::HasEdits(_) => tr!("편집 여부", "Edited"),
        SmartRule::IsoAtLeast(_) => "ISO ≥",
        SmartRule::IsoAtMost(_) => "ISO ≤",
        SmartRule::FocalAtLeast(_) => tr!("초점거리 ≥", "Focal length ≥"),
        SmartRule::FocalAtMost(_) => tr!("초점거리 ≤", "Focal length ≤"),
        SmartRule::InFolder(_) => tr!("폴더 경로 포함", "Folder path contains"),
        SmartRule::HasKeywords(_) => tr!("키워드 유무", "Has keywords"),
        SmartRule::CapturedWithinDays(_) => tr!("최근 N일 내 촬영", "Captured in last N days"),
        SmartRule::EditedWithinDays(_) => tr!("최근 N일 내 편집", "Edited in last N days"),
    }
}

fn rule_templates() -> Vec<SmartRule> {
    vec![
        SmartRule::RatingAtLeast(3),
        SmartRule::RatingAtMost(2),
        SmartRule::FlagIs(Flag::Pick),
        SmartRule::LabelIs(ColorLabel::Red),
        SmartRule::KeywordContains(String::new()),
        SmartRule::TextContains(String::new()),
        SmartRule::CameraContains(String::new()),
        SmartRule::LensContains(String::new()),
        SmartRule::CapturedAfter("2025-01-01".into()),
        SmartRule::CapturedBefore("2026-12-31".into()),
        SmartRule::FileTypeIs("CR3".into()),
        SmartRule::HasEdits(true),
        SmartRule::IsoAtLeast(1600),
        SmartRule::IsoAtMost(400),
        SmartRule::FocalAtLeast(70.0),
        SmartRule::FocalAtMost(35.0),
        SmartRule::InFolder(String::new()),
        SmartRule::HasKeywords(false),
        SmartRule::CapturedWithinDays(30),
        SmartRule::EditedWithinDays(7),
    ]
}

fn smart_editor(app: &mut App, ctx: &egui::Context) {
    let Some(ed) = &mut app.dlg.smart else { return };
    let mut save = false;
    let mut cancel = false;
    let count = app.cat.photos.iter().filter(|p| ed.rules.matches(p)).count();
    let can_save = !ed.name.trim().is_empty();
    let (_, _, close) = form::modal(
        ctx,
        "smart",
        if ed.id.is_some() { tr!("스마트 컬렉션 편집", "Edit Smart Collection") } else { tr!("새 스마트 컬렉션", "New Smart Collection") },
        tr!("조건에 맞는 사진이 자동으로 모입니다.", "Photos matching the rules gather automatically."),
        vec2(660.0, 560.0),
        |ui| {
            form::card(ui, "", "", |ui| {
                form::row(ui, tr!("이름", "Name"), |ui| ui.add(egui::TextEdit::singleline(&mut ed.name).desired_width(280.0)));
                form::row(ui, tr!("일치", "Match"), |ui| form::seg(ui, &mut ed.rules.match_all, &[(true, tr!("모든 조건", "All rules")), (false, tr!("하나라도", "Any rule"))]));
            });
            form::card(ui, tr!("조건", "Rules"), "", |ui| {
                egui::ScrollArea::vertical().id_salt("smart_rules").max_height(260.0).show(ui, |ui| {
                    let mut remove = None;
                    for (i, r) in ed.rules.rules.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("rk", i)).width(140.0).selected_text(rule_name(r)).show_ui(ui, |ui| {
                                for t in rule_templates() {
                                    if ui.selectable_label(rule_name(&t) == rule_name(r), rule_name(&t)).clicked() {
                                        *r = t;
                                    }
                                }
                            });
                            match r {
                                SmartRule::RatingAtLeast(v) | SmartRule::RatingAtMost(v) => {
                                    ui.add(egui::Slider::new(v, 0..=5));
                                }
                                SmartRule::FlagIs(f) => {
                                    form::seg(ui, f, &[(Flag::Pick, tr!("선택", "Pick")), (Flag::None, tr!("없음", "None")), (Flag::Reject, tr!("거부", "Reject"))]);
                                }
                                SmartRule::LabelIs(l) => {
                                    egui::ComboBox::from_id_salt(("rl", i)).selected_text(l.name()).show_ui(ui, |ui| {
                                        for x in ColorLabel::ALL {
                                            ui.selectable_value(l, x, x.name());
                                        }
                                    });
                                }
                                SmartRule::KeywordContains(s)
                                | SmartRule::TextContains(s)
                                | SmartRule::CameraContains(s)
                                | SmartRule::LensContains(s)
                                | SmartRule::FileTypeIs(s)
                                | SmartRule::InFolder(s) => {
                                    ui.add(egui::TextEdit::singleline(s).desired_width(220.0));
                                }
                                SmartRule::CapturedAfter(s) | SmartRule::CapturedBefore(s) => {
                                    ui.add(egui::TextEdit::singleline(s).hint_text("YYYY-MM-DD").desired_width(110.0));
                                }
                                SmartRule::HasEdits(b) => {
                                    form::seg(ui, b, &[(true, tr!("편집됨", "Edited")), (false, tr!("미편집", "Unedited"))]);
                                }
                                SmartRule::IsoAtLeast(v) | SmartRule::IsoAtMost(v) => {
                                    ui.add(egui::DragValue::new(v).range(25..=409600));
                                }
                                SmartRule::FocalAtLeast(v) | SmartRule::FocalAtMost(v) => {
                                    ui.add(egui::DragValue::new(v).range(1.0..=2000.0).suffix(" mm"));
                                }
                                SmartRule::HasKeywords(b) => {
                                    form::seg(ui, b, &[(true, tr!("있음", "Yes")), (false, tr!("없음", "None"))]);
                                }
                                SmartRule::CapturedWithinDays(d) | SmartRule::EditedWithinDays(d) => {
                                    ui.add(egui::DragValue::new(d).range(1..=3650).suffix(tr!(" 일", " days")));
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text(tr!("조건 삭제", "Delete rule")).clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(i) = remove {
                        ed.rules.rules.remove(i);
                    }
                });
                if form::small(ui, tr!("+ 조건 추가", "+ Add rule")).clicked() {
                    ed.rules.rules.push(SmartRule::RatingAtLeast(1));
                }
            });
        },
        |ui| {
            form::footer_note(ui, &trf!("지금 일치하는 사진 {count}장", "{count} photos match now"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if form::primary(ui, tr!("저장", "Save"), can_save).clicked() {
                    save = true;
                }
                if form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                    cancel = true;
                }
            });
        },
    );
    if save {
        let ed = app.dlg.smart.take().unwrap();
        let res = match ed.id {
            Some(id) => app.cat.update_smart(id, ed.name.trim(), &ed.rules).map(|_| id),
            None => app.cat.create_collection(ed.name.trim(), Some(ed.rules)),
        };
        match res {
            Ok(id) => app.set_source(Source::Collection(id)),
            Err(e) => app.toast_err(format!("{e}")),
        }
    } else if close || cancel {
        app.dlg.smart = None;
    }
}

// ─────────────────────────── Preferences ───────────────────────────

const PREF_PAGES: [(&str, &str); 8] = [
    ("일반", "작가 정보"),
    ("화면", "테마 · 배율 · 배치"),
    ("가져오기", "자동 적용"),
    ("워터마크", "만들기 · 편집"),
    ("내보내기 프리셋", "저장한 설정"),
    ("저장소", "카탈로그 · 캐시"),
    ("단축키", "키보드"),
    ("정보", ""),
];

/// Open preferences on a specific page (e.g. watermark).
pub fn open_prefs(app: &mut App, page: &str) {
    app.dlg.prefs_open = true;
    if let Some(i) = PREF_PAGES.iter().position(|(n, _)| *n == page) {
        app.dlg.prefs_page = i;
    }
}

fn prefs_dialog(app: &mut App, ctx: &egui::Context) {
    if !app.dlg.prefs_open {
        return;
    }
    let mut clear_cache = false;
    let mut remove_ai = false;
    let mut edit_wm: Option<String> = None;
    let mut wm_list: Vec<Watermark> = app.cat.kv_get_json("watermarks").unwrap_or_default();
    let mut wm_changed = false;
    let mut exp_presets: Vec<ExportSettings> = app.cat.kv_get_json("export_presets").unwrap_or_default();
    let mut exp_changed = false;
    let (_, done, close) = form::modal(
        ctx,
        "prefs",
        tr!("환경설정", "Preferences"),
        "",
        vec2(820.0, 600.0),
        |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    form::nav(ui, &PREF_PAGES.map(|(a, b)| (crate::i18n::t(a), crate::i18n::t(b))), &mut app.dlg.prefs_page);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("prefs_body").auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(ui.available_width() - 6.0);
                        match app.dlg.prefs_page {
                            0 => {
                                form::card(ui, "언어 · Language", "", |ui| {
                                    let mut en = crate::i18n::en();
                                    if form::seg(ui, &mut en, &[(false, "한국어"), (true, "English")]) {
                                        crate::i18n::set_en(en);
                                        let mut reg = crate::catalogs::Registry::load();
                                        reg.lang = if en { "en".into() } else { "ko".into() };
                                        if let Err(e) = reg.save() {
                                            app.toast_err(trf!("저장 실패: {e}", "Couldn't save: {e}"));
                                        }
                                    }
                                });
                                form::card(ui, tr!("작가 정보", "Artist info"), tr!("내보낸 사진의 메타데이터와 워터마크의 {artist}·{copyright} 토큰에 들어갑니다.", "Used in exported photo metadata and in watermark {artist}·{copyright} tokens."), |ui| {
                                    form::row(ui, tr!("작가", "Artist"), |ui| ui.add(egui::TextEdit::singleline(&mut app.prefs.artist).hint_text(tr!("이름", "Name")).desired_width(300.0)));
                                    form::row(ui, tr!("저작권", "Copyright"), |ui| ui.add(egui::TextEdit::singleline(&mut app.prefs.copyright).hint_text(tr!("© 2026 이름", "© 2026 Name")).desired_width(300.0)));
                                });
                                form::card(ui, tr!("현상", "Develop"), "", |ui| {
                                    form::switch_row(ui, tr!("자동 동기화", "Auto Sync"), &mut app.prefs.auto_sync, tr!("여러 장 선택 시 값을 바꾸면 모두에 적용", "When several photos are selected, changes apply to all of them"));
                                });
                            }
                            1 => {
                                form::card(ui, tr!("테마", "Theme"), tr!("무채색 바탕 + 강조색 하나만 씁니다.", "Neutral backgrounds with a single accent color."), |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for k in super::theme::ThemeKind::ALL {
                                            if theme_tile(ui, k, app.prefs.theme == k).clicked() && app.prefs.theme != k {
                                                app.prefs.theme = k;
                                                super::theme::apply_theme(ctx, k);
                                            }
                                        }
                                    });
                                });
                                form::card(ui, tr!("크기", "Size"), "", |ui| {
                                    form::row(ui, tr!("화면 배율", "UI scale"), |ui| {
                                        let mut cur = if app.prefs.ui_scale > 0.0 { app.prefs.ui_scale } else { 1.0 };
                                        let cur0 = cur;
                                        let opts = [(0.9f32, "90%"), (1.0, "100%"), (1.1, "110%"), (1.25, "125%"), (1.5, "150%")];
                                        // Snap to the nearest value, then select it
                                        if let Some(best) = opts.iter().min_by(|a, b| (a.0 - cur).abs().total_cmp(&(b.0 - cur).abs())) {
                                            cur = best.0;
                                        }
                                        if form::seg(ui, &mut cur, &opts) || (cur - cur0).abs() > 0.001 && app.prefs.ui_scale == 0.0 {
                                            app.prefs.ui_scale = cur;
                                            ctx.set_zoom_factor(cur);
                                        }
                                    });
                                });
                                form::card(ui, tr!("배치", "Placement"), tr!("와이드 모니터에서는 미리보기 줄을 왼쪽에 두면 사진이 더 크게 보입니다.", "On wide monitors, putting the filmstrip on the left makes photos larger."), |ui| {
                                    form::row(ui, tr!("미리보기 줄", "Filmstrip"), |ui| {
                                        if form::seg(ui, &mut app.prefs.film_pos, &[(FilmPos::Left, tr!("왼쪽", "Left")), (FilmPos::Bottom, tr!("아래", "Bottom")), (FilmPos::Hidden, tr!("숨김", "Hidden"))]) {
                                            let fp = app.prefs.film_pos;
                                            app.prefs.dev_layout.set_film(fp);
                                        }
                                    });
                                    form::hint(ui, tr!("단축키 F6으로 위치를 바꿀 수 있습니다. 현상 화면의 카드(사진·프리셋·히스토리…)는 머리글을 끌어 원하는 곳으로 옮길 수 있습니다 (왼쪽 최대 2열 · 오른쪽 1열 · 아래 1줄).", "Press F6 to change its position. Cards in Develop (Photos·Presets·History…) can be moved by dragging their headers (left up to 2 columns · right 1 column · bottom 1 row)."));
                                    form::row(ui, tr!("현상 화면 배치", "Develop layout"), |ui| {
                                        if form::small(ui, tr!("기본 배치로 되돌리기", "Restore default layout")).clicked() {
                                            app.prefs.dev_layout = super::devlayout::DevLayout::default();
                                            app.prefs.film_pos = FilmPos::Left;
                                        }
                                    });
                                    form::switch_row(ui, tr!("배치 잠금", "Lock layout"), &mut app.prefs.dev_layout.locked, tr!("카드 위치·패널 폭·카드 높이를 고정 (접기·펼치기는 됨)", "Fix card positions·panel widths·card heights (collapse/expand still works)"));
                                    form::switch_row(ui, tr!("왼쪽 패널", "Left panel"), &mut app.prefs.show_left, tr!("라이브러리 소스 · 현상 프리셋/히스토리 (Tab)", "Library sources · develop presets/history (Tab)"));
                                    form::switch_row(ui, tr!("오른쪽 패널", "Right panel"), &mut app.prefs.show_right, tr!("정보 · 현상 조정", "Info · develop adjustments"));
                                });
                            }
                            2 => {
                                form::card(ui, tr!("가져올 때 자동 적용", "Auto-apply on import"), "", |ui| {
                                    form::switch_row(ui, tr!("렌즈 교정", "Lens Corrections"), &mut app.prefs.auto_lens, tr!("새 RAW에 렌즈 프로파일 교정 적용 (lensfun 공개 자료·카메라 내장 자료)", "Apply lens profile corrections to new RAW files (open lensfun data, camera built-in data)"));
                                    let mut rx = !app.prefs.skip_xmp_on_import;
                                    if form::switch_row(ui, tr!("XMP 사이드카", "XMP sidecar"), &mut rx, tr!("같은 이름의 .xmp가 있으면 그 현상 설정 읽기", "Read develop settings from a .xmp with the same name")) {
                                        app.prefs.skip_xmp_on_import = !rx;
                                    }
                                });
                                form::card(ui, tr!("Lightroom에서 옮겨 오기", "Migrate from Lightroom"), "", |ui| {
                                    ui.horizontal(|ui| {
                                        if form::small(ui, tr!("카탈로그(.lrcat) 가져오기…", "Import catalog (.lrcat)…")).clicked() {
                                            super::lrcat_import::open(app);
                                            app.dlg.prefs_open = false;
                                        }
                                        if form::small(ui, tr!("현상 프리셋 · 워터마크 가져오기…", "Import develop presets · watermarks…")).clicked() {
                                            open_lr_import(app);
                                            app.dlg.prefs_open = false;
                                        }
                                    });
                                });
                            }
                            3 => {
                                form::card(ui, tr!("워터마크", "Watermark"), tr!("내보내기 창의 '워터마크'에서 고르거나, 여기서 만들고 고칩니다.", "Pick one under 'Watermark' in the export window, or create and edit them here."), |ui| {
                                    ui.horizontal(|ui| {
                                        if form::small(ui, tr!("+ 새 워터마크", "+ New watermark")).clicked() {
                                            edit_wm = Some(String::new());
                                        }
                                        if form::small(ui, tr!("Lightroom 워터마크 가져오기…", "Import Lightroom watermark…")).on_hover_text(".lrtemplate").clicked() {
                                            let start = crate::lrpreset::watermark_dir().unwrap_or_default();
                                            if let Some(fs) = rfd::FileDialog::new().set_directory(start).add_filter(tr!("Lightroom 워터마크", "Lightroom watermark"), &["lrtemplate"]).pick_files() {
                                                for f in fs {
                                                    if let Ok(t) = std::fs::read_to_string(&f)
                                                        && let Ok((nw, _)) = crate::lrpreset::parse_lr_watermark(&t, &f.file_stem().unwrap_or_default().to_string_lossy()) {
                                                            wm_list.retain(|x| x.name != nw.name);
                                                            wm_list.push(nw);
                                                            wm_changed = true;
                                                        }
                                                }
                                            }
                                        }
                                    });
                                    ui.add_space(4.0);
                                    if wm_list.is_empty() {
                                        ui.label(egui::RichText::new(tr!("저장된 워터마크가 없습니다", "No saved watermarks")).color(TEXT_DIM()));
                                    }
                                    let mut del = None;
                                    for (i, w) in wm_list.iter().enumerate() {
                                        let sub = match w.kind {
                                            WmKind::Text => trf!("텍스트 · {}", "Text · {}", w.text.lines().next().unwrap_or("")),
                                            WmKind::Graphic => trf!("그래픽 · {}", "Graphic · {}", std::path::Path::new(&w.image_path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
                                        };
                                        let (click, (e, d)) = form::list_item(ui, &w.name, &sub, false, |ui| {
                                            let d = form::small(ui, tr!("삭제", "Delete")).clicked();
                                            let e = form::small(ui, tr!("편집", "Edit")).clicked();
                                            (e, d)
                                        });
                                        if click || e {
                                            edit_wm = Some(w.name.clone());
                                        }
                                        if d {
                                            del = Some(i);
                                        }
                                    }
                                    if let Some(i) = del {
                                        wm_list.remove(i);
                                        wm_changed = true;
                                    }
                                });
                            }
                            4 => {
                                form::card(ui, tr!("내보내기 프리셋", "Export presets"), tr!("내보내기 창 왼쪽 '내 프리셋'에 나타납니다. 저장은 내보내기 창에서 합니다.", "Shown under 'My presets' on the left of the export window. Save them from the export window."), |ui| {
                                    if exp_presets.is_empty() {
                                        ui.label(egui::RichText::new(tr!("저장된 프리셋이 없습니다", "No saved presets")).color(TEXT_DIM()));
                                    }
                                    let mut del = None;
                                    for (i, p) in exp_presets.iter().enumerate() {
                                        let (_, d) = form::list_item(ui, &p.preset_name, &export_summary(p), false, |ui| form::small(ui, tr!("삭제", "Delete")).clicked());
                                        if d {
                                            del = Some(i);
                                        }
                                    }
                                    if let Some(i) = del {
                                        exp_presets.remove(i);
                                        exp_changed = true;
                                    }
                                });
                            }
                            5 => {
                                super::catalog_ui::prefs_card(app, ui);
                                form::card(ui, tr!("앱 데이터", "App data"), tr!("기본 카탈로그·AI 파일·카탈로그 목록이 있는 폴더 (원본 사진은 건드리지 않습니다).", "Folder with the default catalog, AI files and catalog list (original photos are never touched)."), |ui| {
                                    form::row(ui, tr!("폴더", "Folder"), |ui| ui.add(egui::Label::new(mono(config::data_dir().display().to_string()).color(TEXT())).truncate()).on_hover_text(config::data_dir().display().to_string()));
                                    form::row(ui, "", |ui| {
                                        if form::small(ui, tr!("탐색기에서 열기", "Open in Explorer")).clicked() {
                                            let _ = std::process::Command::new("explorer").arg(config::data_dir()).spawn();
                                        }
                                    });
                                });
                                form::card(ui, tr!("AI 파일", "AI files"), tr!("향상(노이즈 감소·해상도)·AI 마스크·지우기·인물에 쓰는 ONNX Runtime과 모델. 기능마다 처음 쓸 때 받고, 모든 처리는 이 PC 안에서 합니다.", "ONNX Runtime and models for Enhance (denoise·resolution), AI masks, Remove and People. Each is downloaded the first time it's used; all processing stays on this PC."), |ui| {
                                    let b = crate::imaging::ai::installed_bytes();
                                    form::row(ui, tr!("사용량", "Usage"), |ui| ui.label(mono(if b == 0 { tr!("없음", "None").to_string() } else { format!("{:.0}MB", b as f64 / 1_048_576.0) }).color(TEXT())));
                                    if b > 0 {
                                        form::row(ui, "", |ui| {
                                            if form::small(ui, tr!("AI 파일 지우기", "Delete AI files")).clicked() {
                                                remove_ai = true;
                                            }
                                        });
                                    }
                                });
                                form::card(ui, tr!("렌즈 자료 (lensfun)", "Lens data (lensfun)"), tr!("렌즈 교정에 쓰는 공개 자료 (lensfun.github.io, CC BY-SA 3.0)로, 프로그램에 들어 있습니다. lensfun의 새 XML 파일을 추가 폴더에 넣으면 같은 이름의 내장 파일 대신 쓰고, 새 이름이면 더합니다.", "Open lens database used for lens correction (lensfun.github.io, CC BY-SA 3.0), built into the program. Put newer lensfun XML files in the extra folder: a file with the same name replaces the built-in one, a new name is added."), |ui| {
                                    use crate::develop::lensfun;
                                    let n = lensfun::db().map(|d| d.lenses.len()).unwrap_or(0);
                                    form::row(ui, tr!("자료", "Data"), |ui| ui.label(mono(trf!("렌즈 {}개 · 내장 {}", "{} lenses · built-in {}", n, lensfun::BUNDLED_VERSION)).color(TEXT())));
                                    let extra = lensfun::user_files().len();
                                    form::row(ui, tr!("추가 파일", "Extra files"), |ui| ui.label(mono(if extra == 0 { tr!("없음", "None").to_string() } else { trf!("{}개", "{}", extra) }).color(TEXT())));
                                    form::row(ui, "", |ui| {
                                        if form::small(ui, tr!("추가 폴더 열기", "Open extra folder")).clicked() {
                                            let d = lensfun::dir();
                                            let _ = std::fs::create_dir_all(&d);
                                            let _ = std::process::Command::new("explorer").arg(d).spawn();
                                        }
                                        if form::small(ui, tr!("다시 읽기", "Reload")).clicked() {
                                            lensfun::reload();
                                        }
                                    });
                                });
                                form::card(ui, tr!("카메라 지원", "Camera support"), tr!("새 카메라가 나와도 프로그램을 업데이트하지 않고 쓸 수 있게: 목록에 없는 기종은 같은 회사의 가장 비슷한 기종 설정으로 엽니다. 정확하게 열려면 그 기종의 rawler 카메라 정의(.toml)를 카메라 폴더에, 색을 맞춘 DCP 프로파일을 프로파일 폴더에 넣으세요 (다시 시작하면 읽음).", "So new cameras work without updating the program: a model missing from the list opens with the settings of the most similar model of the same maker. For exact results put the model's rawler camera definition (.toml) in the cameras folder and a matching DCP profile in the profiles folder (read on restart)."), |ui| {
                                    let count = |d: &std::path::Path, ext: &str| std::fs::read_dir(d).map(|r| r.flatten().filter(|e| e.path().extension().map(|x| x.eq_ignore_ascii_case(ext)).unwrap_or(false)).count()).unwrap_or(0);
                                    let cam_dir = config::data_dir().join("cameras");
                                    let prof_dir = crate::develop::dcp::user_profile_dir();
                                    form::row(ui, tr!("카메라 정의", "Camera definitions"), |ui| ui.label(mono(trf!("추가 {}개", "{} extra", count(&cam_dir, "toml"))).color(TEXT())));
                                    form::row(ui, tr!("프로파일", "Profiles"), |ui| ui.label(mono(trf!("추가 {}개", "{} extra", count(&prof_dir, "dcp"))).color(TEXT())));
                                    form::row(ui, "", |ui| {
                                        for (label, d) in [(tr!("카메라 폴더 열기", "Open cameras folder"), cam_dir.clone()), (tr!("프로파일 폴더 열기", "Open profiles folder"), prof_dir.clone())] {
                                            if form::small(ui, label).clicked() {
                                                let _ = std::fs::create_dir_all(&d);
                                                let _ = std::process::Command::new("explorer").arg(d).spawn();
                                            }
                                        }
                                    });
                                });
                                form::card(ui, tr!("미리보기 캐시", "Preview cache"), tr!("썸네일·미리보기를 지우면 필요할 때 다시 만듭니다.", "Thumbnails and previews are rebuilt when needed after clearing."), |ui| {
                                    form::row(ui, tr!("사용량", "Usage"), |ui| ui.label(mono(cache_size_text()).color(TEXT())));
                                    form::row(ui, "", |ui| {
                                        if form::small(ui, tr!("캐시 비우기", "Clear cache")).clicked() {
                                            clear_cache = true;
                                        }
                                    });
                                });
                            }
                            6 => {
                                form::card(ui, tr!("단축키", "Shortcuts"), tr!("어디서든 ?를 눌러도 볼 수 있습니다. Ctrl+K: 명령 찾기", "Press ? anywhere to see these. Ctrl+K: command palette"), |ui| {
                                    let mut last = "";
                                    for (g, k, d) in super::tools::SHORTCUTS {
                                        if *g != last {
                                            ui.add_space(4.0);
                                            ui.label(egui::RichText::new(crate::i18n::t(g)).size(11.0).color(TEXT_DIM()));
                                            last = g;
                                        }
                                        form::row(ui, crate::i18n::t(k), |ui| ui.label(egui::RichText::new(crate::i18n::t(d)).color(TEXT())));
                                    }
                                });
                            }
                            _ => {
                                form::card(ui, config::APP_NAME, "", |ui| {
                                    ui.label(egui::RichText::new(about_text()).color(TEXT()));
                                    ui.label(egui::RichText::new(tr!("RAW 색 처리: DNG 명세의 색 모델 · 카메라 프로파일·룩은 자체 제작", "RAW color processing: the DNG specification color model · camera profiles and looks are Darkroom's own")).size(11.0).color(TEXT_WEAK()));
                                    ui.label(egui::RichText::new(tr!("AI 향상: ONNX Runtime (Microsoft, MIT) · NAFNet (Chen et al., megvii, MIT) · Real-ESRGAN (Wang et al., BSD-3-Clause)", "AI Enhance: ONNX Runtime (Microsoft, MIT) · NAFNet (Chen et al., megvii, MIT) · Real-ESRGAN (Wang et al., BSD-3-Clause)")).size(11.0).color(TEXT_WEAK()));
                                    ui.label(egui::RichText::new(tr!("렌즈 자료: lensfun (lensfun.github.io, CC BY-SA 3.0)", "Lens data: lensfun (lensfun.github.io, CC BY-SA 3.0)")).size(11.0).color(TEXT_WEAK()));
                                });
                            }
                        }
                    });
                });
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| form::primary(ui, tr!("완료", "Done"), true).clicked()).inner
        },
    );
    if wm_changed {
        app.cat.kv_set_json("watermarks", &wm_list);
    }
    if exp_changed {
        app.cat.kv_set_json("export_presets", &exp_presets);
    }
    if remove_ai {
        match crate::imaging::ai::remove_all() {
            Ok(()) => app.toast(tr!("AI 파일을 지웠습니다", "AI files deleted")),
            Err(e) => app.toast_err(trf!("AI 파일 지우기 실패: {e}", "Couldn't delete AI files: {e}")),
        }
    }
    if clear_cache {
        for d in [config::THUMB_DIR, config::PREVIEW_DIR] {
            let _ = std::fs::remove_dir_all(config::cache_root().join(d));
        }
        app.tex.clear();
        app.preview_tex.clear();
        app.failed_thumbs.clear();
        app.toast(tr!("캐시를 비웠습니다", "Cache cleared"));
    }
    if let Some(n) = edit_wm {
        if n.is_empty() {
            open_wm_editor(app, "");
            if let Some(ed) = &mut app.dlg.wm {
                ed.wm = Watermark { name: trf!("워터마크 {}", "Watermark {}", ed.list.len() + 1), ..Default::default() };
            }
        } else {
            open_wm_editor(app, &n);
        }
    }
    if done || close {
        app.dlg.prefs_open = false;
        app.save_prefs();
    }
}

/// Theme swatch tile.
fn theme_tile(ui: &mut Ui, k: super::theme::ThemeKind, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(132.0, 86.0), egui::Sense::click());
    let p = ui.painter();
    let pal = super::theme::preview_colors(k);
    p.rect_filled(rect, 8.0, pal[0]);
    // Mini window: left panel, canvas, right panel
    let inner = rect.shrink(8.0);
    let top = egui::Rect::from_min_size(inner.min, vec2(inner.width(), 8.0));
    p.rect_filled(top, 2.0, pal[1]);
    let body = egui::Rect::from_min_max(egui::pos2(inner.left(), inner.top() + 11.0), egui::pos2(inner.right(), inner.bottom() - 18.0));
    p.rect_filled(egui::Rect::from_min_size(body.min, vec2(20.0, body.height())), 2.0, pal[1]);
    p.rect_filled(egui::Rect::from_min_max(egui::pos2(body.right() - 26.0, body.top()), body.max), 2.0, pal[1]);
    p.rect_filled(egui::Rect::from_min_max(egui::pos2(body.left() + 23.0, body.top()), egui::pos2(body.right() - 29.0, body.bottom())), 2.0, pal[2]);
    p.circle_filled(egui::pos2(body.right() - 13.0, body.top() + 6.0), 2.5, ACCENT);
    p.text(egui::pos2(inner.left(), inner.bottom() - 6.0), egui::Align2::LEFT_CENTER, k.name(), egui::FontId::proportional(11.5), pal[3]);
    let stroke = if selected { egui::Stroke::new(2.0, ACCENT) } else if resp.hovered() { egui::Stroke::new(1.0, TEXT_WEAK()) } else { egui::Stroke::new(1.0, BORDER()) };
    p.rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
    resp
}

fn cache_size_text() -> String {
    let mut total = 0u64;
    let mut files = 0usize;
    for d in [config::THUMB_DIR, config::PREVIEW_DIR] {
        let mut stack = vec![config::cache_root().join(d)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                match e.metadata() {
                    Ok(m) if m.is_dir() => stack.push(e.path()),
                    Ok(m) => {
                        total += m.len();
                        files += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    trf!("{} · 파일 {files}개", "{} · {files} files", fmt_size(total))
}

/// One-line summary of the export settings.
pub fn export_summary(s: &ExportSettings) -> String {
    let mut v = vec![match s.format {
        Format::Jpeg => format!("JPEG {}", s.quality),
        Format::Tiff => if s.tiff_16bit { tr!("TIFF 16비트", "TIFF 16-bit").into() } else { tr!("TIFF 8비트", "TIFF 8-bit").into() },
        Format::Png => "PNG".into(),
    }];
    if s.resize {
        v.push(match s.resize_mode {
            ResizeMode::LongEdge => trf!("긴 변 {}px", "Long edge {}px", s.size_a),
            ResizeMode::ShortEdge => trf!("짧은 변 {}px", "Short edge {}px", s.size_a),
            ResizeMode::WidthHeight => format!("{}×{}px", s.size_a, s.size_b),
            ResizeMode::Megapixels => format!("{:.1}MP", s.megapixels),
            ResizeMode::Percent => format!("{:.0}%", s.percent),
        });
    } else {
        v.push(tr!("원본 크기", "Original size").into());
    }
    v.push(s.color_space.name().to_string());
    if s.limit_size && s.format == Format::Jpeg {
        v.push(trf!("{}KB 이하", "Under {}KB", s.limit_kb));
    }
    if s.watermark {
        v.push(tr!("워터마크", "Watermark").into());
    }
    v.join(" · ")
}

// ─────────────────────────── Small dialogs ───────────────────────────

/// Setting group picker: two-column checkboxes plus All/None.
fn groups_ui(ui: &mut Ui, g: &mut SettingGroups) {
    ui.horizontal(|ui| {
        if form::small(ui, tr!("모두 선택", "Select all")).clicked() {
            for (_, v) in g.fields_mut() {
                *v = true;
            }
        }
        if form::small(ui, tr!("모두 해제", "Select none")).clicked() {
            for (_, v) in g.fields_mut() {
                *v = false;
            }
        }
    });
    ui.add_space(4.0);
    egui::Grid::new(ui.id().with("groups")).num_columns(2).spacing(vec2(28.0, 6.0)).show(ui, |ui| {
        for (i, (n, v)) in g.fields_mut().into_iter().enumerate() {
            ui.checkbox(v, n);
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
}

/// OK/Cancel footer (primary button label, enabled) returning (ok, cancel).
fn ok_cancel(ui: &mut Ui, ok: &str, enabled: bool) -> (bool, bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let o = form::primary(ui, ok, enabled).clicked();
        let c = form::secondary(ui, tr!("취소", "Cancel")).clicked();
        (o, c)
    })
    .inner
}

fn small_dialogs(app: &mut App, ctx: &egui::Context) {
    // Copy settings
    if let Some(mut g) = app.dlg.copy_settings.take() {
        let (_, (go, cancel), close) = form::modal(ctx, "copy_settings", tr!("현상 설정 복사", "Copy Develop Settings"), tr!("복사할 항목을 고르세요. 붙여넣기: Ctrl+Shift+V", "Choose what to copy. Paste: Ctrl+Shift+V"), vec2(520.0, 520.0), |ui| form::card(ui, "", "", |ui| groups_ui(ui, &mut g)), |ui| ok_cancel(ui, tr!("복사", "Copy"), true));
        if go {
            app.copy_settings(g);
        } else if !(close || cancel) {
            app.dlg.copy_settings = Some(g);
        }
    }
    // Sync
    if let Some(mut g) = app.dlg.sync_settings.take() {
        let n = app.selected.len();
        let (_, (go, cancel), close) = form::modal(
            ctx,
            "sync_settings",
            tr!("설정 동기화", "Sync Settings"),
            &trf!("현재 사진의 선택한 항목을 나머지 {}장에 적용합니다.", "Applies the selected items of the current photo to the other {} photos.", n.saturating_sub(1)),
            vec2(520.0, 520.0),
            |ui| form::card(ui, "", "", |ui| groups_ui(ui, &mut g)),
            |ui| ok_cancel(ui, tr!("동기화", "Sync"), true),
        );
        if go {
            app.sync_settings(g);
        } else if !(close || cancel) {
            app.dlg.sync_settings = Some(g);
        }
    }
    // Save preset
    if let Some((mut group, mut name, mut g)) = app.dlg.save_preset.take() {
        let ok = !name.trim().is_empty();
        let groups: Vec<String> = {
            let mut v: Vec<String> = app.cat.presets().iter().map(|p| p.group.clone()).collect();
            v.dedup();
            v
        };
        let (_, (go, cancel), close) = form::modal(
            ctx,
            "save_preset",
            tr!("새 현상 프리셋", "New Develop Preset"),
            "",
            vec2(560.0, 620.0),
            |ui| {
                form::card(ui, "", "", |ui| {
                    form::row(ui, tr!("이름", "Name"), |ui| ui.add(egui::TextEdit::singleline(&mut name).desired_width(260.0)));
                    form::row(ui, tr!("그룹", "Group"), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut group).desired_width(200.0));
                        ui.menu_button("▼", |ui| {
                            for gname in &groups {
                                if ui.button(gname).clicked() {
                                    group = gname.clone();
                                    ui.close();
                                }
                            }
                        });
                    });
                });
                form::card(ui, tr!("포함할 항목", "Include"), "", |ui| groups_ui(ui, &mut g));
            },
            |ui| ok_cancel(ui, tr!("저장", "Save"), ok),
        );
        if go {
            let s = app.devst.as_ref().map(|d| d.settings.clone()).unwrap_or_default();
            match app.cat.save_preset(group.trim(), name.trim(), &s, &g) {
                Ok(()) => app.toast(trf!("프리셋 '{}' 저장", "Saved preset '{}'", name.trim())),
                Err(e) => app.toast_err(format!("{e}")),
            }
        } else if !(close || cancel) {
            app.dlg.save_preset = Some((group, name, g));
        }
    }
    // New collection
    if let Some(mut name) = app.dlg.new_collection.take() {
        let mut enter = false;
        let ok = !name.trim().is_empty();
        let n_sel = app.targets().len();
        let mut add = app.dlg.new_collection_add;
        let (_, (go, cancel), close) = form::modal(
            ctx,
            "new_collection",
            tr!("새 컬렉션", "New Collection"),
            "",
            vec2(480.0, 280.0),
            |ui| {
                form::card(ui, "", "", |ui| {
                    form::row(ui, tr!("이름", "Name"), |ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(240.0));
                        r.request_focus();
                        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    });
                    form::switch_row(ui, tr!("선택한 사진 넣기", "Include selected photos"), &mut add, &trf!("{n_sel}장", "{n_sel} photos"));
                });
            },
            |ui| ok_cancel(ui, tr!("만들기", "Create"), ok),
        );
        app.dlg.new_collection_add = add;
        if (go || enter) && ok {
            match app.cat.create_collection(name.trim(), None) {
                Ok(id) => {
                    if app.dlg.new_collection_add {
                        let t = app.targets();
                        let _ = app.cat.add_to_collection(id, &t);
                    }
                    app.dlg.new_collection_add = false;
                }
                Err(e) => app.toast_err(format!("{e}")),
            }
        } else if !(close || cancel) {
            app.dlg.new_collection = Some(name);
        }
    }
    // Rename
    if let Some((id, mut name)) = app.dlg.rename_collection.take() {
        let ok = !name.trim().is_empty();
        let (_, (go, cancel), close) = form::modal(
            ctx,
            "rename_collection",
            tr!("컬렉션 이름 바꾸기", "Rename Collection"),
            "",
            vec2(460.0, 240.0),
            |ui| form::card(ui, "", "", |ui| form::row(ui, tr!("이름", "Name"), |ui| ui.add(egui::TextEdit::singleline(&mut name).desired_width(240.0)))),
            |ui| ok_cancel(ui, tr!("확인", "OK"), ok),
        );
        if go {
            let _ = app.cat.rename_collection(id, name.trim());
        } else if !(close || cancel) {
            app.dlg.rename_collection = Some((id, name));
        }
    }
    // Confirm removal
    if app.dlg.confirm_remove {
        let n = app.targets().len();
        let (_, (go, cancel), close) = form::modal(
            ctx,
            "confirm_remove",
            tr!("카탈로그에서 제거", "Remove from Catalog"),
            "",
            vec2(460.0, 230.0),
            |ui| {
                ui.label(egui::RichText::new(trf!("{n}장을 카탈로그에서 제거합니다.", "Removes {n} photos from the catalog.")).color(TEXT()));
                ui.label(egui::RichText::new(tr!("디스크의 원본 파일은 지우지 않습니다. 편집 내용은 사라집니다.", "Original files on disk are not deleted. Edits will be lost.")).size(11.5).color(TEXT_WEAK()));
            },
            |ui| ok_cancel(ui, tr!("제거", "Remove"), true),
        );
        if go {
            app.dlg.confirm_remove = false;
            app.remove_selected();
        } else if close || cancel {
            app.dlg.confirm_remove = false;
        }
    }
    let _ = widgets::vec_len;
}

// ─────────────────────────── Preset import (XMP / .lrtemplate) ───────────────────────────

pub struct LrImport {
    pub files: Vec<PathBuf>,
    pub wm_files: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
    pub skip_dupes: bool,
    pub report: Option<String>,
}

pub fn open_lr_import(app: &mut App) {
    let sources = crate::lrpreset::default_dirs();
    let files = sources.iter().flat_map(|d| crate::lrpreset::collect_files(d, &["xmp", "lrtemplate"])).collect();
    let wm_files = crate::lrpreset::watermark_dir().map(|d| crate::lrpreset::collect_files(&d, &["lrtemplate"])).unwrap_or_default();
    app.dlg.lr_import = Some(LrImport { files, wm_files, sources, skip_dupes: true, report: None });
}

/// Run the preset and watermark import. Returns a summary string.
pub fn run_lr_import(app: &mut App, files: &[PathBuf], wm_files: &[PathBuf], skip_dupes: bool) -> String {
    let existing: std::collections::HashSet<(String, String)> = app.cat.presets().iter().map(|p| (p.group.clone(), p.name.clone())).collect();
    let mut items = Vec::new();
    let mut skipped_dupe = 0;
    let mut failed: Vec<String> = Vec::new();
    let mut warn_count: std::collections::BTreeMap<String, usize> = Default::default();
    for f in files {
        match crate::lrpreset::import_file(f) {
            Ok(p) => {
                if skip_dupes && existing.contains(&(p.group.clone(), p.name.clone())) {
                    skipped_dupe += 1;
                    continue;
                }
                for w in &p.warnings {
                    // Color temperature approximation warnings vary by value, so they are counted per message
                    let key = w.clone();
                    *warn_count.entry(key).or_default() += 1;
                }
                items.push((p.group, p.name, p.settings, p.groups));
            }
            Err(e) => failed.push(format!("{} — {e}", f.file_name().unwrap_or_default().to_string_lossy())),
        }
    }
    let n = items.len();
    if let Err(e) = app.cat.save_presets(&items) {
        return trf!("저장 실패: {e}", "Couldn't save: {e}");
    }
    let mut wms: Vec<Watermark> = app.cat.kv_get_json("watermarks").unwrap_or_default();
    let mut wm_added = 0;
    for f in wm_files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        match crate::lrpreset::parse_lr_watermark(&text, &f.file_stem().unwrap_or_default().to_string_lossy()) {
            Ok((w, warns)) => {
                if wms.iter().any(|x| x.name == w.name) {
                    continue;
                }
                for wn in warns {
                    *warn_count.entry(trf!("워터마크 '{}': {wn}", "Watermark '{}': {wn}", w.name)).or_default() += 1;
                }
                wms.push(w);
                wm_added += 1;
            }
            Err(e) => failed.push(format!("{} — {e}", f.file_name().unwrap_or_default().to_string_lossy())),
        }
    }
    if wm_added > 0 {
        app.cat.kv_set_json("watermarks", &wms);
    }
    let mut r = trf!("현상 프리셋 {n}개, 워터마크 {wm_added}개를 가져왔습니다.", "Imported {n} develop presets and {wm_added} watermarks.");
    if skipped_dupe > 0 {
        r += &trf!("\n이미 있는 프리셋 {skipped_dupe}개는 건너뜀.", "\nSkipped {skipped_dupe} presets that already exist.");
    }
    if !warn_count.is_empty() {
        r += tr!("\n\n참고:", "\n\nNotes:");
        for (w, c) in &warn_count {
            r += &trf!("\n · {w} ({c}개)", "\n · {w} ({c})");
        }
    }
    if !failed.is_empty() {
        r += &trf!("\n\n가져오지 못함 {}개:", "\n\nCouldn't import {}:", failed.len());
        for f in failed.iter().take(8) {
            r += &format!("\n · {f}");
        }
    }
    r
}

fn lr_import_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.dlg.lr_import.take() else { return };
    let mut go = false;
    let mut cancel = false;
    let done = st.report.is_some();
    let can = !st.files.is_empty() || !st.wm_files.is_empty();
    let (_, _, close) = form::modal(
        ctx,
        "lr_import",
        tr!("Lightroom 프리셋 · 워터마크 가져오기", "Import Lightroom Presets · Watermarks"),
        "",
        vec2(640.0, 560.0),
        |ui| {
            egui::ScrollArea::vertical().id_salt("lrimp").auto_shrink([false, true]).show(ui, |ui| {
                if let Some(rep) = &st.report {
                    form::card(ui, tr!("결과", "Result"), "", |ui| {
                        ui.label(egui::RichText::new(rep).color(TEXT()));
                    });
                    return;
                }
                form::card(ui, tr!("찾은 파일", "Files found"), tr!("설치된 Lightroom·Camera Raw 프리셋 폴더를 자동으로 찾았습니다.", "Found the installed Lightroom·Camera Raw preset folders automatically."), |ui| {
                    form::row(ui, tr!("현상 프리셋", "Develop preset"), |ui| ui.label(egui::RichText::new(trf!("{}개", "{}", st.files.len())).color(STRONG())));
                    form::row(ui, tr!("워터마크", "Watermark"), |ui| ui.label(egui::RichText::new(trf!("{}개", "{}", st.wm_files.len())).color(STRONG())));
                    if st.sources.is_empty() {
                        form::hint(ui, tr!("설치된 Lightroom 프리셋 폴더를 찾지 못했습니다 — 파일이나 폴더를 직접 추가하세요.", "Couldn't find installed Lightroom preset folders — add files or folders yourself."));
                    }
                    for s in &st.sources {
                        form::hint(ui, &s.display().to_string());
                    }
                    form::row(ui, "", |ui| {
                        if form::small(ui, tr!("파일 추가…", "Add files…")).clicked()
                            && let Some(fs) = rfd::FileDialog::new().add_filter(tr!("Lightroom 프리셋", "Lightroom presets"), &["xmp", "lrtemplate"]).pick_files() {
                                st.files.extend(fs);
                            }
                        if form::small(ui, tr!("폴더 추가…", "Add folder…")).clicked()
                            && let Some(d) = rfd::FileDialog::new().pick_folder() {
                                st.files.extend(crate::lrpreset::collect_files(&d, &["xmp", "lrtemplate"]));
                            }
                        if form::small(ui, tr!("목록 비우기", "Clear list")).clicked() {
                            st.files.clear();
                            st.wm_files.clear();
                        }
                    });
                });
                form::card(ui, tr!("옵션", "Options"), "", |ui| {
                    form::switch_row(ui, tr!("중복 건너뛰기", "Skip duplicates"), &mut st.skip_dupes, tr!("같은 그룹·이름의 프리셋은 가져오지 않음", "Don't import presets with the same group and name"));
                    form::hint(ui, tr!("기본·톤 커브·HSL·컬러 그레이딩·디테일·효과·캘리브레이션·프로파일·렌즈 교정, .lrtemplate의 로컬 마스크를 옮깁니다. AI 마스크는 옮기지 않습니다.", "Brings over basic·tone curve·HSL·color grading·detail·effects·calibration·profile·lens corrections, and local masks from .lrtemplate. AI masks aren't carried over."));
                });
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if done {
                    if form::primary(ui, tr!("닫기", "Close"), true).clicked() {
                        cancel = true;
                    }
                } else {
                    if form::primary(ui, tr!("가져오기", "Import"), can).clicked() {
                        go = true;
                    }
                    if form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                        cancel = true;
                    }
                }
            });
        },
    );
    if go {
        st.files.sort();
        st.files.dedup();
        let (f, w) = (st.files.clone(), st.wm_files.clone());
        st.report = Some(run_lr_import(app, &f, &w, st.skip_dupes));
    }
    if !(close || cancel) {
        app.dlg.lr_import = Some(st);
    }
}
