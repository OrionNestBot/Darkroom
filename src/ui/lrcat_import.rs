//! Import screen and progress handling for catalog files (.lrcat).
//! 1) Read the catalog in the background, then show a summary and options  2) Read file metadata in the background, in parallel
//! 3) Write to the Darkroom catalog on the main thread, then finish virtual copies and collections.

use super::app::*;
use super::theme::*;
use crate::catalog::PhotoId;
use crate::develop::settings::DevelopSettings;
use crate::imaging::meta::PhotoMeta;
use crate::lrcat::{LrCatalog, LrCollectionKind, LrPhoto, exif_steps, orientation_steps};
use crossbeam_channel::{Receiver, unbounded};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Options {
    pub develop: bool,
    pub metadata: bool,
    pub keywords: bool,
    pub collections: bool,
    pub history: bool,
    /// Also add photos whose files are missing (offline drives etc.)
    pub include_missing: bool,
    /// Overwrite settings of photos already in Darkroom with the imported ones
    pub overwrite_existing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { develop: true, metadata: true, keywords: true, collections: true, history: true, include_missing: false, overwrite_existing: false }
    }
}

pub struct Loaded {
    pub cat: Arc<LrCatalog>,
    pub exists: Vec<bool>,
}

pub struct Dialog {
    pub path: PathBuf,
    loading: Option<Receiver<Result<Loaded, String>>>,
    loaded: Option<Loaded>,
    error: Option<String>,
    pub opt: Options,
    pub report: Option<String>,
}

pub enum Msg {
    Batch(Vec<(usize, u64, PhotoMeta)>),
    Progress(usize),
    Done,
}

pub struct Progress {
    rx: Receiver<Msg>,
    pub done: usize,
    pub total: usize,
    cat: Arc<LrCatalog>,
    opt: Options,
    idmap: HashMap<i64, PhotoId>,
    import_id: i64,
    added: usize,
    updated: usize,
    /// (imported photo index, existing photo) for photos already in the catalog
    existing: Vec<(usize, PhotoId)>,
}

pub fn open(app: &mut App) {
    let start = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures").join("Lightroom")).filter(|p| p.exists());
    let mut fd = rfd::FileDialog::new().add_filter(tr!("Lightroom 카탈로그", "Lightroom catalog"), &["lrcat"]);
    if let Some(s) = start {
        fd = fd.set_directory(s);
    }
    let Some(path) = fd.pick_file() else { return };
    open_path(app, path);
}

pub fn open_path(app: &mut App, path: PathBuf) {
    let (tx, rx) = unbounded();
    let p2 = path.clone();
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("lrcat".into())
        .spawn(move || {
            let r = crate::lrcat::read(&p2).map_err(|e| format!("{e:#}")).map(|cat| {
                let exists: Vec<bool> = cat.photos.par_iter().map(|p| p.path.exists()).collect();
                Loaded { cat: Arc::new(cat), exists }
            });
            let _ = tx.send(r);
        })
        .expect("lrcat thread");
    app.dlg.lrcat = Some(Dialog { path, loading: Some(rx), loaded: None, error: None, opt: Options::default(), report: None });
}

pub fn dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.dlg.lrcat.take() else { return };
    if let Some(rx) = &d.loading {
        if let Ok(r) = rx.try_recv() {
            match r {
                Ok(l) => d.loaded = Some(l),
                Err(e) => d.error = Some(e),
            }
            d.loading = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let mut go = false;
    let close_b = false;
    let mut close_f = false;
    let known = app.cat.known_paths();
    let busy = app.import.is_some() || app.lrcat_progress.is_some();
    let ready = d.loaded.is_some() && d.report.is_none() && d.error.is_none();
    let finished = d.report.is_some() || d.error.is_some();
    let progress = app.lrcat_progress.as_ref().map(|pr| (pr.done, pr.total));
    let path_text = d.path.display().to_string();
    let (_, _, close) = super::form::modal(
        ctx,
        "lrcat",
        tr!("Lightroom 카탈로그 가져오기", "Import Lightroom Catalog"),
        &path_text,
        egui::vec2(640.0, 640.0),
        |ui| {
            egui::ScrollArea::vertical().id_salt("lrcat_body").auto_shrink([false, true]).show(ui, |ui| {
                if let Some(rep) = &d.report {
                    super::form::card(ui, tr!("완료", "Done"), "", |ui| {
                        ui.label(egui::RichText::new(rep).color(TEXT()));
                    });
                    return;
                }
                if d.loading.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new(tr!("카탈로그 읽는 중… (복사본으로 읽어 원본은 바뀌지 않습니다)", "Reading catalog… (a copy is read, so the original isn't changed)")).color(TEXT_WEAK()));
                    });
                    return;
                }
                if let Some(e) = &d.error {
                    ui.label(egui::RichText::new(trf!("열 수 없음: {e}", "Can't open: {e}")).color(ACCENT));
                    return;
                }
                let Some(l) = &d.loaded else {
                    if let Some((done, total)) = progress {
                        let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(frac).text(mono(trf!("메타데이터 읽는 중 {done}/{total}", "Reading metadata {done}/{total}"))));
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    return;
                };
                let _ = &close_b;
                let cat = &l.cat;
                let n = cat.photos.len();
                let n_exist = l.exists.iter().filter(|x| **x).count();
                let n_known = cat.photos.iter().filter(|p| known.contains(&p.path.to_string_lossy().to_lowercase())).count();
                let n_vc = cat.photos.iter().filter(|p| p.master.is_some()).count();
                let n_edit = cat.edited_count();
                let n_kw: usize = {
                    let mut s = std::collections::HashSet::new();
                    for p in &cat.photos {
                        for k in &p.keywords {
                            s.insert(k.as_str());
                        }
                    }
                    s.len()
                };
                super::form::card(ui, tr!("카탈로그 내용", "Catalog contents"), "", |ui| {
                    let v = |ui: &mut egui::Ui, s: String| ui.label(egui::RichText::new(s).color(TEXT()));
                    super::form::row(ui, tr!("사진", "Photos"), |ui| v(ui, trf!("{n}장 (가상 사본 {n_vc})", "{n} photos ({n_vc} virtual copies)")));
                    super::form::row(ui, tr!("파일", "File"), |ui| v(ui, trf!("있음 {n_exist}장 · 없음 {}장", "{n_exist} present · {} missing", n - n_exist)));
                    super::form::row(ui, tr!("이미 있는 사진", "Already in Darkroom"), |ui| v(ui, trf!("{n_known}장", "{n_known} photos")));
                    super::form::row(ui, tr!("편집된 사진", "Edited photos"), |ui| v(ui, trf!("{n_edit}장", "{n_edit} photos")));
                    super::form::row(ui, tr!("키워드", "Keywords"), |ui| v(ui, trf!("{n_kw}개", "{n_kw}")));
                    super::form::row(ui, tr!("컬렉션", "Collections"), |ui| {
                        v(
                            ui,
                            trf!("{}개 (스마트 {}) · 스택 {}개", "{} ({} smart) · {} stacks",
                                cat.collections.iter().filter(|c| c.kind != LrCollectionKind::Quick).count(),
                                cat.collections.iter().filter(|c| c.kind == LrCollectionKind::Smart).count(),
                                cat.stacks.len()
                            ),
                        )
                    });
                });
                let o = &mut d.opt;
                super::form::card(ui, tr!("가져올 항목", "What to import"), "", |ui| {
                    super::form::switch_row(ui, tr!("현상 설정", "Develop settings"), &mut o.develop, tr!("로컬 마스크 · 자르기 · 회전 포함", "Includes local masks · crop · rotation"));
                    super::form::switch_row(ui, tr!("히스토리", "History"), &mut o.history, tr!("현상 히스토리와 스냅샷", "Develop history and snapshots"));
                    super::form::switch_row(ui, tr!("정보", "Info"), &mut o.metadata, tr!("별점 · 깃발 · 색상 라벨 · 제목 · 캡션", "Rating · flag · color label · title · caption"));
                    super::form::switch_row(ui, tr!("키워드", "Keywords"), &mut o.keywords, "");
                    super::form::switch_row(ui, tr!("컬렉션", "Collections"), &mut o.collections, tr!("일반 · 스마트 · 빠른 컬렉션 (세트는 이름 경로로)", "Regular · smart · Quick Collection (sets as name paths)"));
                });
                super::form::card(ui, tr!("특수한 경우", "Special cases"), "", |ui| {
                    super::form::switch_row(ui, tr!("없는 파일", "Missing files"), &mut o.include_missing, &trf!("{}장도 추가 (나중에 위치 다시 지정)", "Add the {} too (locate them later)", n - n_exist));
                    super::form::switch_row(ui, tr!("덮어쓰기", "Overwrite"), &mut o.overwrite_existing, &trf!("이미 있는 {n_known}장에 Lightroom 설정 적용", "Apply Lightroom settings to the {n_known} photos already here"));
                });
                if !cat.warnings.is_empty() {
                    super::form::card(ui, &trf!("참고 {}건", "{} notes", cat.warnings.len()), "", |ui| {
                        for w in &cat.warnings {
                            ui.label(egui::RichText::new(format!("· {w}")).size(11.0).color(TEXT_WEAK()));
                        }
                    });
                }
            });
        },
        |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if finished {
                    if super::form::primary(ui, tr!("닫기", "Close"), true).clicked() {
                        close_f = true;
                    }
                } else {
                    if super::form::primary(ui, tr!("가져오기", "Import"), ready && !busy).clicked() {
                        go = true;
                    }
                    if super::form::secondary(ui, tr!("취소", "Cancel")).clicked() {
                        close_f = true;
                    }
                }
            });
        },
    );
    if go
        && let Some(l) = d.loaded.take() {
            start(app, l, d.opt);
        }
    if !(close || close_b || close_f) {
        app.dlg.lrcat = Some(d);
    }
}

/// Self-test helper: import directly without the dialog.
pub fn autostart(app: &mut App, path: &std::path::Path) -> Result<(), String> {
    let cat = crate::lrcat::read(path).map_err(|e| format!("{e:#}"))?;
    let exists: Vec<bool> = cat.photos.iter().map(|p| p.path.exists()).collect();
    start(app, Loaded { cat: Arc::new(cat), exists }, Options::default());
    Ok(())
}

fn start(app: &mut App, l: Loaded, opt: Options) {
    let known: HashMap<String, PhotoId> = app.cat.photos.iter().filter(|p| p.master.is_none()).map(|p| (p.path.to_string_lossy().to_lowercase(), p.id)).collect();
    let cat = l.cat;
    let mut existing = Vec::new();
    let mut todo: Vec<usize> = Vec::new();
    for (i, p) in cat.photos.iter().enumerate() {
        if p.master.is_some() {
            continue; // Virtual copies are handled after their masters
        }
        if let Some(id) = known.get(&p.path.to_string_lossy().to_lowercase()) {
            existing.push((i, *id));
        } else if l.exists[i] || opt.include_missing {
            todo.push(i);
        }
    }
    let (tx, rx) = unbounded();
    let total = todo.len();
    let c2 = cat.clone();
    let exists = l.exists.clone();
    std::thread::Builder::new()
        .stack_size(crate::config::WORKER_STACK)
        .name("lrcat-import".into())
        .spawn(move || {
            let mut done = 0;
            for chunk in todo.chunks(64) {
                let batch: Vec<(usize, u64, PhotoMeta)> = chunk
                    .par_iter()
                    .map(|&i| {
                        let p = &c2.photos[i];
                        if exists[i] {
                            let m = super::dialogs::read_meta_any(&p.path);
                            let size = std::fs::metadata(&p.path).map(|m| m.len()).unwrap_or(0);
                            (i, size, m)
                        } else {
                            let m = PhotoMeta { capture_time: p.capture_time.clone(), orientation: 1, ..Default::default() };
                            (i, 0, m)
                        }
                    })
                    .collect();
                done += batch.len();
                let _ = tx.send(Msg::Batch(batch));
                let _ = tx.send(Msg::Progress(done));
            }
            let _ = tx.send(Msg::Done);
        })
        .expect("lrcat import thread");
    let import_id = app.cat.next_import_id();
    app.lrcat_progress = Some(Progress { rx, done: 0, total, cat, opt, idmap: HashMap::new(), import_id, added: 0, updated: 0, existing });
}

/// If the user rotation in the imported catalog differs from the file's EXIF orientation, move the difference into the develop 90-degree rotation.
fn develop_for(p: &LrPhoto, meta: &PhotoMeta, is_raw: bool, opt: &Options) -> Option<DevelopSettings> {
    let mut d = if opt.develop { p.develop.clone() } else { None };
    if let Some(lr) = orientation_steps(&p.orientation) {
        let ex = exif_steps(meta.orientation);
        let extra = (lr + 4 - ex) % 4;
        if extra != 0 {
            let s = d.get_or_insert_with(|| DevelopSettings::default_for(is_raw));
            s.geometry.rotate90 = (s.geometry.rotate90 + extra) % 4;
        }
    }
    d
}

fn apply_meta(app: &mut App, id: PhotoId, p: &LrPhoto, opt: &Options) {
    if opt.metadata {
        // Virtual copies inherit their master's values, so write defaults explicitly too
        let _ = app.cat.set_rating(&[id], p.rating);
        let _ = app.cat.set_flag(&[id], p.flag);
        let _ = app.cat.set_label(&[id], p.label);
        if !p.title.is_empty() || !p.caption.is_empty() {
            let _ = app.cat.set_text(id, &p.title, &p.caption);
        }
    }
    if p.touched > 0 {
        app.cat.set_edited_at(id, p.touched);
    }
    if opt.history {
        let _ = app.cat.clear_history(id);
        for (label, s) in &p.history {
            let _ = app.cat.push_history(id, label, s);
        }
        for (name, s) in &p.snapshots {
            let _ = app.cat.add_snapshot(id, name, s);
        }
    }
}

pub fn pump(app: &mut App) {
    let Some(pr) = &mut app.lrcat_progress else { return };
    let mut batches = Vec::new();
    let mut finished = false;
    for m in pr.rx.try_iter() {
        match m {
            Msg::Batch(b) => batches.push(b),
            Msg::Progress(d) => pr.done = d,
            Msg::Done => finished = true,
        }
    }
    if batches.is_empty() && !finished {
        return;
    }
    let mut pr = app.lrcat_progress.take().unwrap();
    let cat = pr.cat.clone();
    let opt = pr.opt;
    for b in batches {
        let items: Vec<_> = b
            .iter()
            .map(|(i, size, m)| {
                let p = &cat.photos[*i];
                let raw = p.path.extension().and_then(|e| e.to_str()).map(crate::config::is_raw_ext).unwrap_or(false);
                let mut m = m.clone();
                m.rating = None; // Use the imported rating
                let dev = develop_for(p, &m, raw, &opt);
                (p.path.clone(), *size, m, dev, if opt.keywords { p.keywords.clone() } else { Vec::new() })
            })
            .collect();
        match app.cat.add_photos(&items, pr.import_id) {
            Ok(ids) => {
                app.cat.begin_batch();
                for ((i, _, _), id) in b.iter().zip(ids) {
                    pr.idmap.insert(cat.photos[*i].local_id, id);
                    apply_meta(app, id, &cat.photos[*i], &opt);
                    pr.added += 1;
                }
                app.cat.end_batch();
            }
            Err(e) => app.toast_err(trf!("카탈로그 기록 실패: {e}", "Couldn't write to catalog: {e}")),
        }
        app.visible_dirty = true;
    }
    if !finished {
        app.lrcat_progress = Some(pr);
        return;
    }
    // Existing photo: change only with the overwrite option, but always link collections
    for (i, id) in pr.existing.clone() {
        let p = &cat.photos[i];
        pr.idmap.insert(p.local_id, id);
        if opt.overwrite_existing {
            let (raw, meta) = match app.cat.get(id) {
                Some(x) => (x.is_raw, x.meta.clone()),
                None => continue,
            };
            if let Some(d) = develop_for(p, &meta, raw, &opt) {
                let _ = app.cat.set_develop(id, &d);
            }
            apply_meta(app, id, p, &opt);
            if opt.keywords && !p.keywords.is_empty() {
                let _ = app.cat.add_keywords(&[id], &p.keywords);
            }
            pr.updated += 1;
        }
    }
    // Virtual copies
    let mut vcs = 0;
    for p in cat.photos.iter().filter(|p| p.master.is_some()) {
        let Some(&mid) = p.master.and_then(|m| pr.idmap.get(&m)) else { continue };
        let Ok(nid) = app.cat.create_virtual_copy(mid) else { continue };
        let (raw, meta) = app.cat.get(nid).map(|x| (x.is_raw, x.meta.clone())).unwrap_or_default();
        let d = develop_for(p, &meta, raw, &opt).unwrap_or_else(|| DevelopSettings::default_for(raw));
        let _ = app.cat.set_develop(nid, &d);
        if !p.copy_name.is_empty() {
            let _ = app.cat.set_copy_name(nid, &p.copy_name);
        }
        apply_meta(app, nid, p, &opt);
        pr.idmap.insert(p.local_id, nid);
        vcs += 1;
    }
    // Collections
    let mut cols = 0;
    if opt.collections {
        for c in &cat.collections {
            let ids: Vec<PhotoId> = c.images.iter().filter_map(|i| pr.idmap.get(i).copied()).collect();
            match c.kind {
                LrCollectionKind::Quick => {
                    let add: Vec<PhotoId> = ids.into_iter().filter(|i| !app.cat.quick_collection.contains(i)).collect();
                    if !add.is_empty() {
                        let _ = app.cat.toggle_quick(&add);
                    }
                }
                LrCollectionKind::Normal | LrCollectionKind::Smart => {
                    let name = if c.path.is_empty() { c.name.clone() } else { format!("{} / {}", c.path, c.name) };
                    if let Ok(cid) = app.cat.create_collection(&name, c.smart.clone()) {
                        if c.kind == LrCollectionKind::Normal && !ids.is_empty() {
                            let _ = app.cat.add_to_collection(cid, &ids);
                        }
                        cols += 1;
                    }
                }
            }
        }
    }
    // Stacks (only those with two or more imported members)
    let mut stacks = 0;
    for st in &cat.stacks {
        let ids: Vec<PhotoId> = st.iter().filter_map(|i| pr.idmap.get(i).copied()).collect();
        if ids.len() >= 2 && ids.iter().all(|i| app.cat.get(*i).map(|p| p.stack_id == 0).unwrap_or(false)) && app.cat.stack(&ids).is_ok() {
            stacks += 1;
        }
    }
    app.cat.check_missing();
    app.visible_dirty = true;
    let skipped = cat.photos.iter().filter(|p| p.master.is_none()).count() - pr.added - pr.existing.len();
    let mut rep = trf!("새로 추가 {}장 · 가상 사본 {vcs}장 · 컬렉션 {cols}개", "Added {} photos · {vcs} virtual copies · {cols} collections", pr.added);
    if stacks > 0 {
        rep += &trf!(" · 스택 {stacks}개", " · {stacks} stacks");
    }
    if pr.updated > 0 {
        rep += &trf!(" · 기존 사진 갱신 {}장", " · updated {} existing photos", pr.updated);
    }
    if !pr.existing.is_empty() && !opt.overwrite_existing {
        rep += &trf!("\n이미 있던 {}장은 그대로 두었습니다", "\nLeft {} existing photos as they were", pr.existing.len());
    }
    if skipped > 0 {
        rep += &trf!("\n파일이 없어 건너뜀 {skipped}장", "\nSkipped {skipped} missing files");
    }
    app.toast(trf!("Lightroom 카탈로그: {}장 가져옴", "Lightroom catalog: imported {} photos", pr.added + vcs));
    if let Some(d) = &mut app.dlg.lrcat {
        d.report = Some(rep);
    } else {
        app.dlg.lrcat = Some(Dialog { path: PathBuf::new(), loading: None, loaded: None, error: None, opt, report: Some(rep) });
    }
}
