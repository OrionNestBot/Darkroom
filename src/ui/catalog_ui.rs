//! Catalog menu: new, open, recent, backup.
//! Switching catalogs restarts the app so all state starts clean with the new catalog.
//! © 2026 OrionNest

use super::app::*;
use super::form;
use super::theme::*;
use crate::catalogs;
use egui::{Ui, vec2};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum BackupEvery {
    Never,
    Daily,
    #[default]
    Weekly,
    Monthly,
}

impl BackupEvery {
    fn days(self) -> Option<f64> {
        match self {
            BackupEvery::Never => None,
            BackupEvery::Daily => Some(1.0),
            BackupEvery::Weekly => Some(7.0),
            BackupEvery::Monthly => Some(30.0),
        }
    }
}

pub struct NewCatalog {
    pub parent: String,
    pub name: String,
    pub carry: bool,
    pub err: Option<String>,
}

#[derive(Default)]
pub struct CatalogUi {
    pub new_dlg: Option<NewCatalog>,
    pub backup_rx: Option<crossbeam_channel::Receiver<Result<PathBuf, String>>>,
    backup_quiet: bool,
    /// Pending switch: close the window on the next frame
    pub closing: bool,
}

/// Cloud-sync folders (a catalog there can be corrupted by sync conflicts or locks)
fn is_cloud(p: &str) -> bool {
    let l = p.to_lowercase();
    ["onedrive", "dropbox", "google drive", "googledrive", "icloud", tr!("네이버 mybox", "Naver MYBOX"), "mybox"].iter().any(|k| l.contains(k))
}

fn default_parent() -> PathBuf {
    let pics = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|p| p.to_path_buf()));
    match pics {
        Some(p) if !is_cloud(&p.to_string_lossy()) => p.join("Darkroom"),
        // If the Pictures folder was moved into a cloud-sync folder, use the home folder instead
        _ => directories::BaseDirs::new().map(|b| b.home_dir().join("Darkroom")).unwrap_or_else(|| PathBuf::from(r"C:\Darkroom")),
    }
}

/// Catalog name menu in the top bar
pub fn menu(app: &mut App, ui: &mut Ui) {
    let cur = app.cat.path.clone();
    if ui.button(tr!("새 카탈로그…", "New catalog…")).clicked() {
        open_new(app);
        ui.close();
    }
    if ui.button(tr!("카탈로그 열기…", "Open catalog…")).clicked() {
        ui.close();
        if let Some(f) = rfd::FileDialog::new().add_filter(tr!("Darkroom 카탈로그", "Darkroom catalog"), &[crate::config::CATALOG_EXT, "db"]).pick_file() {
            switch(app, ui.ctx(), &f);
        }
    }
    let mut recent: Vec<PathBuf> = catalogs::Registry::load().recent.into_iter().filter(|p| *p != cur && p.exists()).collect();
    if !catalogs::is_default(&cur) && !recent.iter().any(|p| catalogs::is_default(p)) {
        recent.push(crate::config::default_catalog_path());
    }
    if !recent.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new(tr!("최근 카탈로그", "Recent catalogs")).size(11.0).color(TEXT_WEAK()));
        for p in recent {
            if ui.button(catalogs::display_name(&p)).on_hover_text(p.display().to_string()).clicked() {
                ui.close();
                switch(app, ui.ctx(), &p);
            }
        }
    }
    ui.separator();
    let age = catalogs::last_backup_age_days(&cur);
    let label = match age {
        Some(d) if d < 1.0 => tr!("지금 백업 (오늘 백업함)", "Back up now (backed up today)").to_string(),
        Some(d) => trf!("지금 백업 ({:.0}일 전 백업)", "Back up now (last backup {:.0} days ago)", d),
        None => tr!("지금 백업 (백업 없음)", "Back up now (no backup yet)").to_string(),
    };
    if ui.add_enabled(app.catui.backup_rx.is_none(), egui::Button::new(label)).clicked() {
        start_backup(app, false);
        ui.close();
    }
    if ui.button(tr!("탐색기에서 보기", "Show in Explorer")).on_hover_text(cur.display().to_string()).clicked() {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", cur.display())).spawn();
        ui.close();
    }
}

pub fn open_new(app: &mut App) {
    app.catui.new_dlg = Some(NewCatalog { parent: default_parent().to_string_lossy().to_string(), name: tr!("새 카탈로그", "New catalog").into(), carry: true, err: None });
}

/// Save the current state and restart with another catalog
pub fn switch(app: &mut App, ctx: &egui::Context, p: &Path) {
    if p == app.cat.path {
        app.toast(tr!("이미 열려 있는 카탈로그입니다", "This catalog is already open"));
        return;
    }
    app.save_prefs();
    app.save_session();
    if app.module == Module::Develop {
        app.commit_develop_now();
    }
    // (app.autotest is temporarily taken out during self-test, so check the environment variable)
    if std::env::var_os("DARKROOM_AUTOTEST").is_some() {
        // In self-test, don't actually restart; only check the recorded switch
        app.toast(trf!("전환 예약: {}", "Switch scheduled: {}", catalogs::display_name(p)));
        return;
    }
    match catalogs::switch_to(p) {
        Ok(()) => {
            app.catui.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Err(e) => app.toast_err(trf!("카탈로그 전환 실패: {e}", "Couldn't switch catalog: {e}")),
    }
}

pub fn new_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.catui.new_dlg.take() else { return };
    let path = catalogs::new_catalog_path(Path::new(&d.parent), &d.name);
    let ok = !catalogs::clean_name(&d.name).is_empty() && !d.parent.trim().is_empty();
    let (_, (go, cancel), close) = form::modal(
        ctx,
        "new_catalog",
        tr!("새 카탈로그", "New catalog"),
        tr!("사진 목록·편집 내역을 따로 관리할 카탈로그를 만듭니다", "Create a catalog that keeps its own photo list and edit history"),
        vec2(560.0, 420.0),
        |ui| {
            form::card(ui, "", "", |ui| {
                form::row(ui, tr!("이름", "Name"), |ui| ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(280.0)));
                form::folder_row(ui, tr!("위치", "Location"), &mut d.parent);
                form::hint(ui, &trf!("만들어질 파일: {}\n미리보기 캐시는 같은 폴더의 \"{} Previews\"에 생깁니다.", "File to create: {}\nThe preview cache will be created in \"{} Previews\" in the same folder.", path.display(), catalogs::clean_name(&d.name)));
                if is_cloud(&d.parent) {
                    ui.label(egui::RichText::new(tr!("클라우드 동기화 폴더(OneDrive 등)에는 카탈로그를 두지 마세요 — 동기화 중 파일이 잠기거나 충돌해 손상될 수 있습니다. 백업 폴더로만 쓰는 것을 권장합니다.", "Don't keep a catalog in a cloud-synced folder (OneDrive etc.) — files can be locked or conflicted while syncing and get damaged. Use it only for backups.")).size(11.0).color(ACCENT));
                }
                form::switch_row(ui, tr!("설정 가져가기", "Bring settings along"), &mut d.carry, tr!("현재 카탈로그의 현상 프리셋·환경설정·워터마크·내보내기 설정을 옮겨 담음", "Copy develop presets, preferences, watermarks and export settings from the current catalog"));
            });
            if let Some(e) = &d.err {
                ui.label(egui::RichText::new(e).color(ACCENT));
            }
        },
        |ui| {
            let mut r = (false, false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                r.0 = form::primary(ui, tr!("만들고 열기", "Create and open"), ok).clicked();
                r.1 = form::secondary(ui, tr!("취소", "Cancel")).clicked();
            });
            r
        },
    );
    if cancel || close {
        return;
    }
    if go {
        match catalogs::create(&path, &app.cat, d.carry) {
            Ok(()) => {
                switch(app, ctx, &path);
                return;
            }
            Err(e) => d.err = Some(trf!("만들 수 없습니다: {e}", "Can't create: {e}")),
        }
    }
    app.catui.new_dlg = Some(d);
}

pub fn start_backup(app: &mut App, quiet: bool) {
    if app.catui.backup_rx.is_some() {
        return;
    }
    let p = app.cat.path.clone();
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send(catalogs::backup(&p).map_err(|e| format!("{e:#}")));
    });
    app.catui.backup_rx = Some(rx);
    app.catui.backup_quiet = quiet;
}

/// At startup: run a background backup if the backup interval has passed
pub fn auto_backup(app: &mut App) {
    let Some(days) = app.prefs.backup_every.days() else { return };
    if app.cat.photos.is_empty() {
        return;
    }
    if catalogs::last_backup_age_days(&app.cat.path).map(|a| a >= days).unwrap_or(true) {
        start_backup(app, true);
    }
}

pub fn pump(app: &mut App) {
    let Some(rx) = &app.catui.backup_rx else { return };
    let r = match rx.try_recv() {
        Ok(r) => r,
        Err(crossbeam_channel::TryRecvError::Empty) => return,
        Err(crossbeam_channel::TryRecvError::Disconnected) => Err(tr!("백업 작업이 중단되었습니다", "The backup was interrupted").into()),
    };
    app.catui.backup_rx = None;
    match r {
        Ok(p) => {
            if !app.catui.backup_quiet {
                app.toast(trf!("카탈로그를 백업했습니다: {}", "Catalog backed up: {}", p.parent().map(|d| d.display().to_string()).unwrap_or_default()));
            }
        }
        Err(e) => app.toast_err(trf!("카탈로그 백업 실패: {e}", "Catalog backup failed: {e}")),
    }
}

/// Catalog card on the preferences storage tab
pub fn prefs_card(app: &mut App, ui: &mut Ui) {
    let cur = app.cat.path.clone();
    form::card(ui, tr!("카탈로그", "Catalog"), tr!("사진 목록·별점·키워드·현상 설정과 작업 내역이 담긴 파일. 원본 사진은 카탈로그 밖 제자리에 있습니다.", "The file holding your photo list, ratings, keywords, develop settings and history. Original photos stay where they are, outside the catalog."), |ui| {
        form::row(ui, tr!("이름", "Name"), |ui| ui.label(egui::RichText::new(catalogs::display_name(&cur)).color(TEXT())));
        // Ellipsize long paths so they don't widen the dialog (full path on hover)
        form::row(ui, tr!("파일", "File"), |ui| ui.add(egui::Label::new(mono(cur.display().to_string()).color(TEXT())).truncate()).on_hover_text(cur.display().to_string()));
        let (db, cache) = catalogs::sizes(&cur);
        form::row(ui, tr!("크기", "Size"), |ui| ui.label(mono(trf!("카탈로그 {} · 미리보기 캐시 {}", "Catalog {} · preview cache {}", fmt_size(db), fmt_size(cache))).color(TEXT())));
        form::row(ui, tr!("자동 백업", "Automatic backup"), |ui| {
            form::seg(ui, &mut app.prefs.backup_every, &[(BackupEvery::Never, tr!("안 함", "Never")), (BackupEvery::Daily, tr!("매일", "Daily")), (BackupEvery::Weekly, tr!("매주", "Weekly")), (BackupEvery::Monthly, tr!("매월", "Monthly"))]);
        });
        form::hint(ui, &trf!("시작할 때 주기가 지났으면 카탈로그 옆 Backups 폴더에 복사본을 만듭니다 (최근 {}개 보관).", "At startup, if the interval has passed, a copy is made in the Backups folder next to the catalog (keeps the last {}).", crate::config::BACKUP_KEEP));
        if !app.cat.face_scanned.is_empty() {
            form::row(ui, tr!("인물", "People"), |ui| {
                ui.label(mono(trf!("얼굴 {}개 · 인물 {}명 · 찾아 본 사진 {}장", "{} faces · {} people · {} photos scanned", app.cat.faces.len(), app.cat.persons.len(), app.cat.face_scanned.len())).color(TEXT()));
                if form::small(ui, tr!("인물 기록 지우기", "Clear people data")).on_hover_text(tr!("찾은 얼굴과 이름을 모두 지움 (사진·키워드는 그대로)", "Deletes all found faces and names (photos and keywords stay)")).clicked() {
                    match app.cat.clear_people() {
                        Ok(()) => {
                            if matches!(app.source, Source::Person(_)) {
                                app.set_source(Source::All);
                            }
                            app.toast(tr!("인물 기록을 지웠습니다", "People data cleared"));
                        }
                        Err(e) => app.toast_err(trf!("지우기 실패: {e}", "Remove failed: {e}")),
                    }
                }
            });
        }
        form::row(ui, "", |ui| {
            if form::small(ui, tr!("새 카탈로그…", "New catalog…")).clicked() {
                open_new(app);
            }
            if form::small(ui, tr!("지금 백업", "Back up now")).clicked() {
                start_backup(app, false);
            }
            if form::small(ui, tr!("탐색기에서 보기", "Show in Explorer")).clicked() {
                let _ = std::process::Command::new("explorer").arg(format!("/select,{}", cur.display())).spawn();
            }
        });
    });
}
