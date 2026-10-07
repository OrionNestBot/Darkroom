//! App state, top-level layout, event handling and shortcuts.

use super::develop::DevState;
use super::dialogs::Dialogs;
use super::theme::*;
use super::viewer::Viewer;
use super::workers::{DevEvent, DevService, ThumbKind, ThumbReq, ThumbResult, ThumbService};
use crate::catalog::{Catalog, ColorLabel, Flag, PhotoId, SmartRules};
use crate::config;
use crate::develop::settings::{DevelopSettings, SettingGroups};
use crate::export::{ExportEvent, ExportSettings};
use egui::{Align2, Key, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Module {
    Library,
    Develop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LibView {
    Grid,
    Loupe,
    Compare,
    Survey,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Source {
    All,
    Quick,
    LastImport,
    Missing,
    Folder(PathBuf),
    Collection(i64),
    /// People (persons grouped by face)
    Person(i64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum RatingOp {
    #[default]
    AtLeast,
    Exactly,
    AtMost,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum FlagFilter {
    #[default]
    All,
    Picked,
    Unflagged,
    Rejected,
    NotRejected,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum KindFilter {
    #[default]
    All,
    Raw,
    Jpeg,
    Heif,
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Filter {
    pub text: String,
    pub rating: u8,
    pub rating_op: RatingOp,
    pub flag: FlagFilter,
    pub labels: Vec<ColorLabel>,
    pub kind: KindFilter,
    pub edited: Option<bool>,
    #[serde(default)]
    pub exported: Option<bool>,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        *self != Filter::default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum SortKey {
    #[default]
    CaptureTime,
    ImportOrder,
    FileName,
    Rating,
    Edited,
}

impl SortKey {
    pub const ALL: [SortKey; 5] = [SortKey::CaptureTime, SortKey::ImportOrder, SortKey::FileName, SortKey::Rating, SortKey::Edited];
    pub fn name(self) -> &'static str {
        match self {
            SortKey::CaptureTime => tr!("촬영 시간", "Capture time"),
            SortKey::ImportOrder => tr!("가져온 순서", "Import order"),
            SortKey::FileName => tr!("파일명", "File name"),
            SortKey::Rating => tr!("별점", "Rating"),
            SortKey::Edited => tr!("편집 여부", "Edited"),
        }
    }
}

/// Filmstrip position
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilmPos {
    #[default]
    Left,
    Bottom,
    Hidden,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub artist: String,
    pub copyright: String,
    pub grid_cell: f32,
    pub show_left: bool,
    pub show_right: bool,
    pub show_film: bool,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub last_import_dir: String,
    /// Develop: apply changes to all selected photos immediately
    pub auto_sync: bool,
    pub theme: super::theme::ThemeKind,
    /// UI scale (0 = default 1.0)
    pub ui_scale: f32,
    /// Automatically apply lens profile correction to newly imported RAWs
    pub auto_lens: bool,
    /// On import, read settings from a same-name XMP sidecar (photos edited in other photo editors)
    pub skip_xmp_on_import: bool,
    pub film_pos: FilmPos,
    /// Develop panel card layout
    pub dev_layout: super::devlayout::DevLayout,
    /// Catalog auto-backup interval
    pub backup_every: super::catalog_ui::BackupEvery,
    /// Soft proofing: profile path, intent, paper simulation, gamut warning
    pub proof_profile: String,
    pub proof_intent: crate::export::proof::Intent,
    pub proof_paper: bool,
    pub proof_gamut: bool,
}

/// Last session state (resumed on restart): module, view, source, selection, current photo, develop zoom
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub module: Option<Module>,
    pub lib_view: Option<LibView>,
    pub source: Option<Source>,
    pub current: Option<PhotoId>,
    pub selected: Vec<PhotoId>,
    pub expanded_stacks: Vec<i64>,
    pub dev_zoom: Option<f32>,
    pub dev_center: [f32; 2],
    pub panel_tab: u8,
    /// Per-photo view position (most recent first, up to 500)
    pub views: Vec<(PhotoId, Option<f32>, [f32; 2])>,
}

pub struct Toast {
    pub text: String,
    pub at: Instant,
    pub error: bool,
}

pub struct ExportProgress {
    pub rx: crossbeam_channel::Receiver<ExportEvent>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub done: usize,
    pub total: usize,
    pub current: String,
}

pub struct App {
    pub cat: Catalog,
    pub thumbs: ThumbService,
    pub dev: DevService,
    pub tex: lru::LruCache<PhotoId, (i64, TextureHandle)>,
    pub preview_tex: lru::LruCache<PhotoId, (i64, TextureHandle)>,
    inbox: VecDeque<ThumbResult>,
    pub failed_thumbs: HashSet<(PhotoId, i64)>,
    pub module: Module,
    pub lib_view: LibView,
    pub source: Source,
    pub filter: Filter,
    pub visible: Vec<PhotoId>,
    pub visible_dirty: bool,
    pub selected: Vec<PhotoId>,
    pub sel_set: HashSet<PhotoId>,
    pub current: Option<PhotoId>,
    pub anchor: Option<PhotoId>,
    pub scroll_to_current: bool,
    pub loupe: Option<Viewer>,
    pub compare: Option<(Viewer, Viewer)>,
    pub devst: Option<DevState>,
    pub clipboard: Option<(DevelopSettings, SettingGroups)>,
    pub prefs: Prefs,
    pub dlg: Dialogs,
    pub toasts: Vec<Toast>,
    pub export: Option<ExportProgress>,
    pub import: Option<super::dialogs::ImportProgress>,
    pub lrcat_progress: Option<super::lrcat_import::Progress>,
    pub rename: Option<super::tools::RenameDialog>,
    pub slideshow: Option<super::tools::Slideshow>,
    pub show_shortcuts: bool,
    pub palette: Option<super::tools::Palette>,
    pub pending_theme: Option<super::theme::ThemeKind>,
    pub hdr_job: Option<super::tools::HdrJob>,
    pub pano: Option<super::tools::PanoDialog>,
    pub pano_job: Option<super::tools::PanoJob>,
    pub enhance: Option<super::enhance::EnhanceDialog>,
    pub enhance_job: Option<super::enhance::EnhanceJob>,
    pub print: Option<super::tools::PrintDialog>,
    /// Expanded stacks (other stacks show only their top photo)
    pub expanded_stacks: HashSet<i64>,
    /// Per-photo last view state (zoom, center), restored when reopened
    pub view_mem: HashMap<PhotoId, (Option<f32>, [f32; 2])>,
    /// Left panel tab (Library / Develop)
    pub left_tab_lib: usize,
    pub left_tab_dev: usize,
    pub frame_ms: f32,
    pub catui: super::catalog_ui::CatalogUi,
    /// Reference view: the photo pinned on the left and whether the view is on
    pub dev_ref: Option<PhotoId>,
    pub ref_view: bool,
    pub proof: super::proof_ui::ProofUi,
    pub people: super::people_ui::PeopleUi,
    pub ai_mask_job: Option<super::develop::AiMaskJob>,
    pub fill_job: Option<super::develop::FillJob>,
    pub autotest: Option<super::autotest::AutoTest>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let repaint: super::workers::Repaint = Arc::new(move || ctx.request_repaint());
        let (path, cat_warn) = crate::catalogs::startup_path();
        config::set_catalog_path(path.clone());
        crate::catalogs::note_opened(&path);
        if !crate::catalogs::is_default(&path) {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — {}", crate::catalogs::display_name(&path), config::APP_NAME)));
        }
        let mut cat = Catalog::open(&path).unwrap_or_else(|e| {
            // If the catalog is corrupted, back it up and start a new one
            let bak = path.with_extension(format!("broken-{}.db", crate::catalog::now()));
            let _ = std::fs::rename(&path, &bak);
            eprintln!("카탈로그 열기 실패: {e}");
            Catalog::open(&path).expect(tr!("카탈로그 생성 실패", "Couldn't create catalog"))
        });
        cat.check_missing();
        crate::imaging::ai::cleanup_pending();
        std::thread::spawn(crate::lrcat::cleanup_stale_temp);
        let mut prefs: Prefs = cat.kv_get_json("prefs").unwrap_or(Prefs {
            grid_cell: config::GRID_CELL_DEFAULT,
            show_left: true,
            show_right: true,
            show_film: true,
            ..Default::default()
        });
        if prefs.grid_cell < config::GRID_CELL_MIN {
            prefs.grid_cell = config::GRID_CELL_DEFAULT;
        }
        let filter = cat.kv_get_json("filter").unwrap_or_default();
        super::theme::apply_theme(&cc.egui_ctx, prefs.theme);
        if prefs.ui_scale > 0.0 {
            cc.egui_ctx.set_zoom_factor(prefs.ui_scale);
        }
        let mut app = Self {
            thumbs: ThumbService::new(repaint.clone()),
            dev: DevService::new(repaint),
            tex: lru::LruCache::new(NonZeroUsize::new(config::THUMB_TEXTURE_CACHE).unwrap()),
            preview_tex: lru::LruCache::new(NonZeroUsize::new(6).unwrap()),
            inbox: VecDeque::new(),
            failed_thumbs: HashSet::new(),
            module: Module::Library,
            lib_view: LibView::Grid,
            source: Source::All,
            filter,
            visible: Vec::new(),
            visible_dirty: true,
            selected: Vec::new(),
            sel_set: HashSet::new(),
            current: None,
            anchor: None,
            scroll_to_current: false,
            loupe: None,
            compare: None,
            devst: None,
            clipboard: None,
            prefs,
            dlg: Dialogs::default(),
            toasts: Vec::new(),
            export: None,
            import: None,
            lrcat_progress: None,
            rename: None,
            slideshow: None,
            show_shortcuts: false,
            palette: None,
            pending_theme: None,
            hdr_job: None,
            pano: None,
            pano_job: None,
            enhance: None,
            enhance_job: None,
            print: None,
            expanded_stacks: HashSet::new(),
            view_mem: HashMap::new(),
            left_tab_lib: 0,
            left_tab_dev: 0,
            frame_ms: 0.0,
            catui: Default::default(),
            dev_ref: None,
            ref_view: false,
            proof: Default::default(),
            people: Default::default(),
            ai_mask_job: None,
            fill_job: None,
            autotest: super::autotest::AutoTest::from_env(),
            cat,
        };
        app.recompute_visible();
        if let Some(first) = app.visible.first().copied() {
            app.select_single(first);
        }
        // Restore the last session state (skipped for self-test runs)
        if app.autotest.is_none() || std::env::var_os("DARKROOM_SESSION").as_deref() == Some(std::ffi::OsStr::new("check")) {
            app.restore_session();
        }
        if let Some(w) = cat_warn {
            app.toast_err(w);
        }
        super::catalog_ui::auto_backup(&mut app);
        app
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), at: Instant::now(), error: false });
    }

    pub fn toast_err(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), at: Instant::now(), error: true });
    }

    pub fn save_prefs(&self) {
        self.cat.kv_set_json("prefs", &self.prefs);
        self.cat.kv_set_json("filter", &self.filter);
    }

    /// Save the current session state (autosave and on exit)
    pub fn save_session(&self) {
        if self.autotest.is_some() && std::env::var_os("DARKROOM_SESSION").is_none() {
            return;
        }
        let d = self.devst.as_ref();
        let s = Session {
            module: Some(self.module),
            lib_view: Some(self.lib_view),
            source: Some(self.source.clone()),
            current: self.current,
            selected: self.selected.iter().copied().take(5000).collect(),
            expanded_stacks: self.expanded_stacks.iter().copied().collect(),
            dev_zoom: d.and_then(|d| d.viewer.zoom),
            dev_center: d.map(|d| d.viewer.center).unwrap_or([0.5, 0.5]),
            panel_tab: d.map(|d| super::develop::panel_tab_index(d.panel_tab)).unwrap_or(0),
            views: self.view_mem.iter().take(500).map(|(k, (z, c))| (*k, *z, *c)).collect(),
        };
        self.cat.kv_set_json("session", &s);
    }

    /// Restore the last session state (as much as possible if it is missing or photos were removed)
    fn restore_session(&mut self) {
        let Some(s) = self.cat.kv_get_json::<Session>("session") else { return };
        if let Some(src) = s.source {
            let ok = match &src {
                Source::Folder(f) => self.cat.photos.iter().any(|p| p.folder == *f),
                Source::Collection(c) => self.cat.collections.iter().any(|x| x.id == *c),
                _ => true,
            };
            if ok {
                self.source = src;
            }
        }
        self.expanded_stacks = s.expanded_stacks.into_iter().collect();
        self.view_mem = s.views.iter().map(|(k, z, c)| (*k, (*z, *c))).collect();
        self.recompute_visible();
        let vis: HashSet<PhotoId> = self.visible.iter().copied().collect();
        let cur = s.current.filter(|c| vis.contains(c)).or(self.visible.first().copied());
        if let Some(c) = cur {
            self.select_single(c);
            let sel: Vec<PhotoId> = s.selected.into_iter().filter(|x| vis.contains(x)).collect();
            if sel.len() > 1 && sel.contains(&c) {
                self.sel_set = sel.iter().copied().collect();
                self.selected = sel;
            }
        }
        if let Some(v) = s.lib_view
            && v != LibView::Compare {
                self.set_lib_view(v);
            }
        if s.module == Some(Module::Develop) && self.current.is_some() {
            self.set_module(Module::Develop);
            if let Some(d) = &mut self.devst {
                d.viewer.zoom = s.dev_zoom;
                d.viewer.center = s.dev_center;
                d.panel_tab = super::develop::panel_tab_from(s.panel_tab);
            }
        }
        self.scroll_to_current = true;
        self.dlg.film_follow = true;
    }

    // ───────────── Visible photo list ─────────────

    pub fn source_matches(&self, p: &crate::catalog::Photo) -> bool {
        match &self.source {
            Source::All => true,
            Source::Quick => self.cat.quick_collection.contains(&p.id),
            Source::LastImport => p.import_id == self.cat.last_import && p.import_id != 0,
            Source::Missing => p.missing,
            Source::Folder(f) => p.folder == *f,
            Source::Collection(cid) => match self.cat.collections.iter().find(|c| c.id == *cid) {
                Some(c) => match &c.smart {
                    Some(r) => r.matches(p),
                    None => self.cat.collection_members.get(cid).map(|m| m.contains(&p.id)).unwrap_or(false),
                },
                None => false,
            },
            Source::Person(pid) => self.cat.faces.iter().any(|f| f.person == *pid && f.photo == p.id),
        }
    }

    pub fn filter_matches(&self, p: &crate::catalog::Photo) -> bool {
        let f = &self.filter;
        if !f.text.trim().is_empty() && !crate::catalog::text_match(p, &f.text) {
            return false;
        }
        let ok_rating = match f.rating_op {
            RatingOp::AtLeast => p.rating >= f.rating,
            RatingOp::Exactly => p.rating == f.rating,
            RatingOp::AtMost => p.rating <= f.rating,
        };
        if !ok_rating {
            return false;
        }
        let ok_flag = match f.flag {
            FlagFilter::All => true,
            FlagFilter::Picked => p.flag == Flag::Pick,
            FlagFilter::Unflagged => p.flag == Flag::None,
            FlagFilter::Rejected => p.flag == Flag::Reject,
            FlagFilter::NotRejected => p.flag != Flag::Reject,
        };
        if !ok_flag {
            return false;
        }
        if !f.labels.is_empty() && !f.labels.contains(&p.label) {
            return false;
        }
        let e = p.ext.to_ascii_lowercase();
        let ok_kind = match f.kind {
            KindFilter::All => true,
            KindFilter::Raw => p.is_raw,
            KindFilter::Jpeg => e == "jpg" || e == "jpeg",
            KindFilter::Heif => config::HEIF_EXTS.contains(&e.as_str()),
            KindFilter::Other => !p.is_raw && !matches!(e.as_str(), "jpg" | "jpeg" | "heic" | "heif" | "hif"),
        };
        if !ok_kind {
            return false;
        }
        if let Some(ed) = f.edited
            && p.has_edits() != ed {
                return false;
            }
        if let Some(ex) = f.exported
            && (p.exported_at > 0) != ex {
                return false;
            }
        true
    }

    pub fn recompute_visible(&mut self) {
        // Collect people in one pass instead of scanning face lists per photo
        let person_set = if let Source::Person(pid) = &self.source { Some(self.cat.person_photos(*pid)) } else { None };
        let mut v: Vec<&crate::catalog::Photo> = self
            .cat
            .photos
            .iter()
            .filter(|p| person_set.as_ref().map(|s| s.contains(&p.id)).unwrap_or_else(|| self.source_matches(p)) && self.filter_matches(p))
            .collect();
        match self.prefs.sort {
            SortKey::CaptureTime => v.sort_by(|a, b| {
                a.meta.capture_time.as_deref().unwrap_or("").cmp(b.meta.capture_time.as_deref().unwrap_or("")).then(a.file_name.cmp(&b.file_name)).then(a.id.cmp(&b.id))
            }),
            SortKey::ImportOrder => v.sort_by_key(|p| p.id),
            SortKey::FileName => v.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()).then(a.id.cmp(&b.id))),
            SortKey::Rating => v.sort_by(|a, b| a.rating.cmp(&b.rating).then(a.id.cmp(&b.id))),
            SortKey::Edited => v.sort_by(|a, b| a.has_edits().cmp(&b.has_edits()).then(a.id.cmp(&b.id))),
        }
        if self.prefs.sort_desc {
            v.reverse();
        }
        // Stacks: gathered at the first member's position; collapsed shows only the top photo (lowest stack_pos),
        // expanded shows all filtered members in stack order, grouped together even if they were apart
        let mut members: HashMap<i64, Vec<&crate::catalog::Photo>> = HashMap::new();
        for p in &v {
            if p.stack_id != 0 {
                members.entry(p.stack_id).or_default().push(p);
            }
        }
        for m in members.values_mut() {
            m.sort_by_key(|p| (p.stack_pos, p.id));
        }
        let mut out: Vec<&crate::catalog::Photo> = Vec::with_capacity(v.len());
        let mut done: HashSet<i64> = HashSet::new();
        for p in v {
            if p.stack_id == 0 {
                out.push(p);
            } else if done.insert(p.stack_id) {
                let m = &members[&p.stack_id];
                if self.expanded_stacks.contains(&p.stack_id) {
                    out.extend(m.iter().copied());
                } else {
                    out.push(m[0]);
                }
            }
        }
        self.visible = out.into_iter().map(|p| p.id).collect();
        self.visible_dirty = false;
        let vis: HashSet<PhotoId> = self.visible.iter().copied().collect();
        self.selected.retain(|i| vis.contains(i));
        self.sel_set = self.selected.iter().copied().collect();
        if self.current.map(|c| !vis.contains(&c)).unwrap_or(true) {
            self.current = self.selected.first().copied().or(self.visible.first().copied());
            if let Some(c) = self.current
                && self.selected.is_empty() {
                    self.selected = vec![c];
                    self.sel_set = [c].into_iter().collect();
                }
        }
    }

    pub fn set_source(&mut self, s: Source) {
        self.source = s;
        self.recompute_visible();
        self.scroll_to_current = true;
    }

    // ───────────── Selection ─────────────

    pub fn select_single(&mut self, id: PhotoId) {
        self.selected = vec![id];
        self.sel_set = [id].into_iter().collect();
        self.set_current(id);
        self.anchor = Some(id);
    }

    pub fn set_current(&mut self, id: PhotoId) {
        if self.current == Some(id) {
            return;
        }
        self.current = Some(id);
        self.scroll_to_current = true;
        if self.module == Module::Develop {
            self.open_develop(id);
        } else if self.lib_view == LibView::Loupe {
            self.open_loupe(id);
        }
        self.prefetch_neighbors();
    }

    pub fn toggle_select(&mut self, id: PhotoId) {
        if self.sel_set.remove(&id) {
            self.selected.retain(|x| *x != id);
            if self.current == Some(id)
                && let Some(f) = self.selected.first().copied() {
                    self.set_current(f);
                }
        } else {
            self.selected.push(id);
            self.sel_set.insert(id);
            self.set_current(id);
        }
        self.anchor = Some(id);
    }

    pub fn range_select(&mut self, id: PhotoId) {
        let a = self.anchor.unwrap_or(id);
        let (Some(ia), Some(ib)) = (self.visible.iter().position(|x| *x == a), self.visible.iter().position(|x| *x == id)) else {
            return self.select_single(id);
        };
        let (lo, hi) = (ia.min(ib), ia.max(ib));
        self.selected = self.visible[lo..=hi].to_vec();
        self.sel_set = self.selected.iter().copied().collect();
        self.set_current(id);
    }

    pub fn select_all(&mut self) {
        self.selected = self.visible.clone();
        self.sel_set = self.selected.iter().copied().collect();
    }

    /// Targets: the selected items (or the current photo if none).
    /// For multi-photo operations (stack, HDR, enhance): all selected photos regardless of view, or the current photo if only one
    pub fn multi_targets(&self) -> Vec<PhotoId> {
        if self.selected.len() > 1 { self.selected.clone() } else { self.current.into_iter().collect() }
    }

    pub fn targets(&self) -> Vec<PhotoId> {
        if self.module == Module::Develop || self.lib_view == LibView::Loupe {
            // In Develop/Loupe, only the current photo
            return self.current.into_iter().collect();
        }
        if self.selected.is_empty() { self.current.into_iter().collect() } else { self.selected.clone() }
    }

    pub fn step(&mut self, delta: i64) {
        if self.visible.is_empty() {
            return;
        }
        let i = self.current.and_then(|c| self.visible.iter().position(|x| *x == c)).unwrap_or(0) as i64;
        let ni = (i + delta).clamp(0, self.visible.len() as i64 - 1) as usize;
        let id = self.visible[ni];
        self.select_single(id);
    }

    fn prefetch_neighbors(&self) {
        if self.module != Module::Develop && self.lib_view != LibView::Loupe {
            return;
        }
        let Some(c) = self.current else { return };
        let Some(i) = self.visible.iter().position(|x| *x == c) else { return };
        for j in [i + 1, i.wrapping_sub(1)] {
            if let Some(id) = self.visible.get(j)
                && let Some(p) = self.cat.get(*id) {
                    self.dev.prefetch(p.path.clone(), p.meta.orientation);
                }
        }
    }

    // ───────────── Thumbnail textures ─────────────

    pub fn thumb(&mut self, id: PhotoId) -> Option<egui::TextureId> {
        let p = self.cat.get(id)?;
        let ver = p.thumb_ver;
        let hit = self.tex.get(&id).map(|(v, t)| (*v, t.id()));
        if let Some((v, tid)) = hit
            && v == ver {
                return Some(tid);
            }
        if !self.failed_thumbs.contains(&(id, ver)) && !p.missing {
            let req = ThumbReq {
                id,
                path: p.path.clone(),
                orientation: p.meta.orientation,
                ver,
                kind: ThumbKind::Grid,
                settings: if p.has_edits() { p.develop.clone() } else { None },
            };
            self.thumbs.request(req);
        }
        hit.map(|(_, t)| t)
    }

    pub fn request_preview(&mut self, id: PhotoId) -> Option<TextureHandle> {
        let p = self.cat.get(id)?;
        let ver = p.thumb_ver;
        if let Some((v, t)) = self.preview_tex.get(&id)
            && *v == ver {
                return Some(t.clone());
            }
        if !p.missing {
            self.thumbs.request(ThumbReq {
                id,
                path: p.path.clone(),
                orientation: p.meta.orientation,
                ver,
                kind: ThumbKind::Preview,
                settings: if p.has_edits() { p.develop.clone() } else { None },
            });
        }
        self.tex.get(&id).map(|(_, t)| t.clone())
    }

    fn pump(&mut self, ctx: &egui::Context) {
        self.inbox.extend(self.thumbs.rx.try_iter());
        let mut n = 0;
        while n < config::MAX_TEXTURE_UPLOADS_PER_FRAME {
            let Some(r) = self.inbox.pop_front() else { break };
            n += 1;
            match r.image {
                Some(img) => {
                    let ci = egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.data);
                    let t = ctx.load_texture(format!("t{}", r.id), ci, TextureOptions::LINEAR);
                    match r.kind {
                        ThumbKind::Grid => {
                            self.tex.put(r.id, (r.ver, t));
                        }
                        ThumbKind::Preview => {
                            self.preview_tex.put(r.id, (r.ver, t.clone()));
                            for v in self.viewers_mut() {
                                if v.id == r.id {
                                    v.placeholder = Some(t.clone());
                                }
                            }
                        }
                    }
                }
                None => {
                    self.failed_thumbs.insert((r.id, r.ver));
                }
            }
        }
        if !self.inbox.is_empty() {
            ctx.request_repaint();
        }
        let evs: Vec<DevEvent> = self.dev.rx.try_iter().collect();
        for ev in evs {
            if let DevEvent::PreviewsReady { id, ver, thumb } = &ev {
                let ci = egui::ColorImage::from_rgba_unmultiplied([thumb.w as usize, thumb.h as usize], &thumb.data);
                let t = ctx.load_texture(format!("t{id}"), ci, TextureOptions::LINEAR);
                self.tex.put(*id, (*ver, t));
                self.preview_tex.pop(id);
                continue;
            }
            for v in self.viewers_mut() {
                v.handle(ctx, &ev);
            }
            if let Some(d) = &mut self.devst {
                d.on_event(&ev);
            }
        }
        // Export progress
        let mut finished = None;
        let mut exported = Vec::new();
        if let Some(ex) = &mut self.export {
            for ev in ex.rx.try_iter() {
                match ev {
                    ExportEvent::Exported { id } => exported.push(id),
                    ExportEvent::Progress { done, total, current } => {
                        ex.done = done;
                        ex.total = total;
                        ex.current = current;
                    }
                    ExportEvent::Finished { ok, failed, skipped, dest, cancelled } => finished = Some((ok, failed, skipped, dest, cancelled)),
                }
            }
        }
        for id in exported {
            self.cat.mark_exported(id);
        }
        if let Some((ok, failed, skipped, _dest, cancelled)) = finished {
            self.export = None;
            let mut msg = trf!("내보내기 {}: {ok}장 완료", "Export {}: {ok} photos done", if cancelled { tr!("취소됨", "cancelled") } else { tr!("끝", "finished") });
            if skipped > 0 {
                msg += &trf!(", {skipped}장 건너뜀", ", {skipped} skipped");
            }
            if failed.is_empty() {
                self.toast(msg);
            } else {
                msg += &trf!(", {}장 실패 — {}", ", {} failed — {}", failed.len(), failed[0].1);
                self.toast_err(msg);
            }
        }
        super::dialogs::pump_import(self);
        super::lrcat_import::pump(self);
        super::tools::pump_hdr(self);
        super::tools::pump_pano(self);
        super::catalog_ui::pump(self);
        super::people_ui::pump(self);
        // AI masks: start jobs on panel requests, show progress in the panel, recompute pasted masks
        if let Some(k) = self.devst.as_mut().and_then(|d| d.ai_request.take()) {
            super::develop::start_ai_mask(self, k, None);
        }
        super::develop::pump_ai_mask(self);
        super::develop::refresh_ai_masks(self);
        super::develop::pump_fill(self);
        if self.fill_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let (busy, st) = match &self.ai_mask_job {
            Some(j) => (Some(j.kind), Some(j.status.clone())),
            None => (None, None),
        };
        if let Some(d) = &mut self.devst {
            d.ai_busy = busy;
            d.ai_status = st;
        }
        if self.ai_mask_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        super::enhance::pump(self);
    }

    pub fn viewers_mut(&mut self) -> Vec<&mut Viewer> {
        let mut v: Vec<&mut Viewer> = Vec::new();
        if let Some(l) = &mut self.loupe {
            v.push(l);
        }
        if let Some((a, b)) = &mut self.compare {
            v.push(a);
            v.push(b);
        }
        if let Some(d) = &mut self.devst {
            v.push(&mut d.viewer);
            if let Some(b) = &mut d.before {
                v.push(b);
            }
            if let Some(b) = &mut d.detail_view {
                v.push(b);
            }
        }
        if let Some(e) = &mut self.enhance
            && let Some(b) = e.viewer_mut() {
                v.push(b);
            }
        v
    }

    // ───────────── Module switching ─────────────

    pub fn open_loupe(&mut self, id: PhotoId) {
        let Some(p) = self.cat.get(id) else { return };
        let (path, o) = (p.path.clone(), p.meta.orientation);
        let mut v = Viewer::new(id, path, o, 0, &self.dev);
        v.placeholder = self.request_preview(id);
        if let Some((z, c)) = self.view_mem.get(&id) {
            v.zoom = *z;
            v.center = *c;
        }
        self.loupe = Some(v);
    }

    pub fn set_lib_view(&mut self, view: LibView) {
        if self.module == Module::Develop {
            self.leave_develop();
            self.module = Module::Library;
        }
        self.lib_view = view;
        self.compare = None;
        if view != LibView::Loupe {
            self.loupe = None;
        }
        match view {
            LibView::Loupe => {
                if let Some(c) = self.current {
                    self.open_loupe(c);
                }
            }
            LibView::Compare => {
                let a = self.current;
                let b = self.selected.iter().copied().find(|x| Some(*x) != a).or_else(|| {
                    let i = a.and_then(|c| self.visible.iter().position(|x| *x == c))?;
                    self.visible.get(i + 1).copied()
                });
                if let (Some(a), Some(b)) = (a, b) {
                    let pa = self.cat.get(a).unwrap();
                    let pb = self.cat.get(b).unwrap();
                    let mut va = Viewer::new(a, pa.path.clone(), pa.meta.orientation, 0, &self.dev);
                    let mut vb = Viewer::new(b, pb.path.clone(), pb.meta.orientation, 1, &self.dev);
                    va.placeholder = self.request_preview(a);
                    vb.placeholder = self.request_preview(b);
                    self.compare = Some((va, vb));
                } else {
                    self.lib_view = LibView::Grid;
                    self.toast(tr!("비교하려면 사진 두 장 이상이 필요합니다", "Comparing needs two or more photos"));
                }
            }
            _ => {}
        }
        self.scroll_to_current = true;
    }

    pub fn set_module(&mut self, m: Module) {
        if self.module == m {
            return;
        }
        match m {
            Module::Develop => {
                let Some(c) = self.current.or(self.visible.first().copied()) else {
                    self.toast(tr!("현상할 사진을 선택하세요", "Select a photo to develop"));
                    return;
                };
                self.loupe = None;
                self.compare = None;
                self.module = Module::Develop;
                self.current = Some(c);
                self.open_develop(c);
                self.prefetch_neighbors();
            }
            Module::Library => {
                self.leave_develop();
                self.module = Module::Library;
                if self.lib_view == LibView::Loupe
                    && let Some(c) = self.current {
                        self.open_loupe(c);
                    }
            }
        }
    }

    // ───────────── Batch metadata changes ─────────────

    pub fn apply_rating(&mut self, r: u8) {
        let t = self.targets();
        if let Err(e) = self.cat.set_rating(&t, r) {
            self.toast_err(format!("{e}"));
        }
        self.after_meta_change();
    }

    pub fn apply_flag(&mut self, f: Flag) {
        let t = self.targets();
        let _ = self.cat.set_flag(&t, f);
        self.after_meta_change();
    }

    pub fn apply_label(&mut self, l: ColorLabel) {
        let t = self.targets();
        // Applying the same label again clears it (toggle)
        let all_same = t.iter().all(|i| self.cat.get(*i).map(|p| p.label == l).unwrap_or(false));
        let _ = self.cat.set_label(&t, if all_same { ColorLabel::None } else { l });
        self.after_meta_change();
    }

    fn after_meta_change(&mut self) {
        if self.filter.is_active() || !matches!(self.source, Source::All | Source::Folder(_)) {
            // Some photos may drop out of the filter; keep the current position
            let keep = self.current;
            self.recompute_visible();
            if let Some(k) = keep
                && !self.visible.contains(&k) {
                    self.scroll_to_current = true;
                }
        }
    }

    pub fn virtual_copy(&mut self) {
        let Some(c) = self.current else { return };
        if self.module == Module::Develop {
            self.commit_develop_now();
        }
        match self.cat.create_virtual_copy(c) {
            Ok(nid) => {
                self.recompute_visible();
                self.select_single(nid);
                self.toast(tr!("가상 사본을 만들었습니다", "Virtual copy created"));
            }
            Err(e) => self.toast_err(format!("{e}")),
        }
    }

    pub fn remove_selected(&mut self) {
        let t = self.targets();
        if t.is_empty() {
            return;
        }
        if self.module == Module::Develop {
            self.devst = None;
            self.module = Module::Library;
        }
        let next = {
            let idx = self.visible.iter().position(|x| t.contains(x)).unwrap_or(0);
            self.visible.iter().skip(idx).find(|x| !t.contains(x)).copied().or_else(|| self.visible.iter().rev().find(|x| !t.contains(x)).copied())
        };
        match self.cat.remove_photos(&t) {
            Ok(()) => {
                self.loupe = None;
                self.compare = None;
                self.recompute_visible();
                if let Some(n) = next {
                    self.current = None;
                    self.select_single(n);
                }
                self.toast(trf!("카탈로그에서 {}장 제거 (원본 파일은 그대로)", "Removed {} photos from the catalog (original files untouched)", t.len()));
            }
            Err(e) => self.toast_err(format!("{e}")),
        }
    }

    pub fn show_in_explorer(&self) {
        if let Some(p) = self.current.and_then(|c| self.cat.get(c)) {
            let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.path.display())).spawn();
        }
    }

    pub fn smart_matches_count(&self, r: &SmartRules) -> usize {
        self.cat.photos.iter().filter(|p| r.matches(p)).count()
    }

    // ───────────── Shortcuts ─────────────

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // The command palette opens even when a text field has focus
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::K)) {
            self.palette = if self.palette.is_some() { None } else { Some(super::tools::Palette { query: String::new(), sel: 0 }) };
            return;
        }
        if ctx.egui_wants_keyboard_input() || self.dlg.any_modal() || self.palette.is_some() || self.rename.is_some() || self.print.is_some() {
            return;
        }
        let (mods, keys): (egui::Modifiers, Vec<Key>) = ctx.input(|i| {
            (
                i.modifiers,
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed: true, .. } => Some(*key),
                        _ => None,
                    })
                    .collect(),
            )
        });
        let cmd = mods.command;
        for k in keys {
            if self.module == Module::Develop && super::develop::shortcut(self, k, mods) {
                continue;
            }
            match (k, cmd, mods.shift, mods.alt) {
                (Key::I, true, true, _) => self.dlg.open_import(&self.prefs),
                (Key::E, true, true, _) => super::dialogs::open_export(self),
                (Key::C, true, true, _) => self.dlg.copy_settings = Some(SettingGroups::all()),
                (Key::V, true, true, _) => self.paste_settings(),
                (Key::V, true, false, true) => self.paste_previous(),
                (Key::Quote, true, _, _) => self.virtual_copy(),
                (Key::A, true, false, _) => self.select_all(),
                (Key::D, true, false, _) => {
                    if let Some(c) = self.current {
                        self.selected = vec![c];
                        self.sel_set = [c].into_iter().collect();
                    }
                }
                (Key::F, true, false, _) => self.dlg.focus_search = true,
                (Key::Comma, true, false, _) => self.dlg.prefs_open = true,
                (Key::F2, false, ..) => super::tools::open_rename(self),
                (Key::P, true, false, _) => super::tools::open_print(self),
                (Key::S, true, false, _) => super::tools::save_xmp(self),
                (Key::I, true, false, true) => super::enhance::open(self),
                (Key::M, true, false, false) => super::tools::open_pano(self),
                (Key::M, true, true, true) => super::develop::match_exposures(self),
                (Key::G, true, false, _) => super::tools::stack_selected(self),
                (Key::G, true, true, _) => super::tools::unstack_selected(self),
                (Key::S, false, false, false) => super::tools::toggle_stack(self),
                (Key::F11, false, ..) => super::tools::start_slideshow(self),
                (Key::Slash, false, true, _) | (Key::Questionmark, false, ..) => self.show_shortcuts = !self.show_shortcuts,
                (Key::G, false, false, false) => self.set_lib_view(LibView::Grid),
                (Key::E, false, false, false) => self.set_lib_view(LibView::Loupe),
                (Key::C, false, false, false) => self.set_lib_view(LibView::Compare),
                (Key::N, false, false, false) => self.set_lib_view(LibView::Survey),
                (Key::D, false, false, false) => self.set_module(Module::Develop),
                (Key::Num0, false, ..) => self.apply_rating(0),
                (Key::Num1, false, ..) => self.apply_rating(1),
                (Key::Num2, false, ..) => self.apply_rating(2),
                (Key::Num3, false, ..) => self.apply_rating(3),
                (Key::Num4, false, ..) => self.apply_rating(4),
                (Key::Num5, false, ..) => self.apply_rating(5),
                (Key::Num6, false, ..) => self.apply_label(ColorLabel::Red),
                (Key::Num7, false, ..) => self.apply_label(ColorLabel::Yellow),
                (Key::Num8, false, ..) => self.apply_label(ColorLabel::Green),
                (Key::Num9, false, ..) => self.apply_label(ColorLabel::Blue),
                (Key::P, false, ..) => self.apply_flag(Flag::Pick),
                (Key::X, false, ..) => self.apply_flag(Flag::Reject),
                (Key::U, false, ..) => self.apply_flag(Flag::None),
                (Key::B, false, ..) => {
                    let t = self.targets();
                    let _ = self.cat.toggle_quick(&t);
                    if self.source == Source::Quick {
                        self.recompute_visible();
                    }
                }
                (Key::ArrowRight, false, false, _) => self.step(1),
                (Key::ArrowLeft, false, false, _) => self.step(-1),
                (Key::ArrowDown, false, false, _) if self.module == Module::Library && self.lib_view == LibView::Grid => {
                    let cols = self.dlg.grid_cols.max(1) as i64;
                    self.step(cols)
                }
                (Key::ArrowUp, false, false, _) if self.module == Module::Library && self.lib_view == LibView::Grid => {
                    let cols = self.dlg.grid_cols.max(1) as i64;
                    self.step(-cols)
                }
                (Key::Home, ..) => {
                    if let Some(f) = self.visible.first().copied() {
                        self.select_single(f)
                    }
                }
                (Key::End, ..) => {
                    if let Some(f) = self.visible.last().copied() {
                        self.select_single(f)
                    }
                }
                (Key::Enter, false, ..) if self.module == Module::Library && self.lib_view == LibView::Grid => self.set_lib_view(LibView::Loupe),
                (Key::Escape, ..) if self.module == Module::Library && self.lib_view != LibView::Grid => self.set_lib_view(LibView::Grid),
                (Key::Space, false, ..) | (Key::Z, false, ..) if self.module == Module::Library => {
                    if self.lib_view == LibView::Grid {
                        self.set_lib_view(LibView::Loupe);
                    } else if let Some(l) = &mut self.loupe {
                        let at = ctx.input(|i| i.pointer.hover_pos());
                        l.toggle_zoom(at);
                    }
                }
                (Key::Delete, ..) | (Key::Backspace, false, ..) => self.dlg.confirm_remove = !self.targets().is_empty(),
                (Key::F6, false, ..) => {
                    self.prefs.film_pos = match self.prefs.film_pos {
                        FilmPos::Left => FilmPos::Bottom,
                        FilmPos::Bottom => FilmPos::Hidden,
                        FilmPos::Hidden => FilmPos::Left,
                    };
                    self.prefs.show_film = true;
                    let fp = self.prefs.film_pos;
                    self.prefs.dev_layout.set_film(fp);
                    self.toast(match self.prefs.film_pos {
                        FilmPos::Left => tr!("미리보기 줄: 왼쪽", "Filmstrip: left"),
                        FilmPos::Bottom => tr!("미리보기 줄: 아래", "Filmstrip: bottom"),
                        FilmPos::Hidden => tr!("미리보기 줄: 숨김", "Filmstrip: hidden"),
                    });
                }
                (Key::Tab, false, false, _) => {
                    let show = !(self.prefs.show_left && self.prefs.show_right);
                    self.prefs.show_left = show;
                    self.prefs.show_right = show;
                }
                (Key::Tab, false, true, _) => {
                    let show = !(self.prefs.show_left && self.prefs.show_right && self.prefs.show_film);
                    self.prefs.show_left = show;
                    self.prefs.show_right = show;
                    self.prefs.show_film = show;
                }
                _ => {}
            }
        }
    }

    // ───────────── Layout ─────────────

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top")
            .exact_size(40.0)
            .frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(egui::RichText::new("DARKROOM").monospace().size(13.0).strong().color(TEXT()));
                    ui.label(egui::RichText::new("■").size(9.0).color(ACCENT));
                    ui.add_space(6.0);
                    let cname = crate::catalogs::display_name(&self.cat.path);
                    let cr = ui
                        .menu_button(egui::RichText::new(format!("{cname}    ")).size(11.5).color(TEXT_WEAK()), |ui| super::catalog_ui::menu(self, ui))
                        .response
                        .on_hover_text(trf!("카탈로그: {}", "Catalog: {}", self.cat.path.display()));
                    // Expand indicator (drawn manually since the font lacks ▾)
                    let c = egui::pos2(cr.rect.right() - 11.0, cr.rect.center().y + 1.0);
                    ui.painter().add(egui::Shape::convex_polygon(vec![c + egui::vec2(-3.5, -2.0), c + egui::vec2(3.5, -2.0), c + egui::vec2(0.0, 2.5)], TEXT_WEAK(), egui::Stroke::NONE));
                    ui.add_space(14.0);
                    for (m, name, key) in [(Module::Library, tr!("라이브러리", "Library"), "G"), (Module::Develop, tr!("현상", "Develop"), "D")] {
                        let sel = self.module == m;
                        let txt = egui::RichText::new(name).size(13.0).color(if sel { STRONG() } else { TEXT_WEAK() });
                        let r = ui.add(egui::Button::new(txt).frame(false)).on_hover_text(key);
                        if sel {
                            let rr = r.rect;
                            ui.painter().line_segment(
                                [egui::pos2(rr.left() + 4.0, rr.bottom() + 3.0), egui::pos2(rr.right() - 4.0, rr.bottom() + 3.0)],
                                egui::Stroke::new(2.0, ACCENT),
                            );
                        }
                        if r.clicked() {
                            if m == Module::Library {
                                self.set_module(Module::Library);
                            } else {
                                self.set_module(Module::Develop);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⚙", |ui| {
                            if ui.button(tr!("환경설정…   Ctrl+,", "Preferences…   Ctrl+,")).clicked() {
                                super::dialogs::open_prefs(self, "일반");
                                ui.close();
                            }
                            if ui.button(tr!("워터마크 만들기 · 편집…", "Create · edit watermarks…")).clicked() {
                                super::dialogs::open_prefs(self, "워터마크");
                                ui.close();
                            }
                            if ui.button(tr!("화면 · 테마 · 배치…", "Display · theme · layout…")).clicked() {
                                super::dialogs::open_prefs(self, "화면");
                                ui.close();
                            }
                            ui.separator();
                            if ui.button(tr!("단축키   ?", "Shortcuts   ?")).clicked() {
                                self.show_shortcuts = true;
                                ui.close();
                            }
                            if ui.button(tr!("명령 찾기   Ctrl+K", "Command palette   Ctrl+K")).clicked() {
                                self.palette = Some(Default::default());
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text(tr!("설정 · 워터마크 · 단축키", "Settings · watermarks · shortcuts"));
                        if ui.button(tr!("내보내기…", "Export…")).on_hover_text("Ctrl+Shift+E").clicked() {
                            super::dialogs::open_export(self);
                        }
                        ui.menu_button(tr!("가져오기…", "Import…"), |ui| {
                            if ui.button(tr!("사진 가져오기…  Ctrl+Shift+I", "Import photos…  Ctrl+Shift+I")).clicked() {
                                self.dlg.open_import(&self.prefs);
                                ui.close();
                            }
                            if ui.button(tr!("Lightroom 카탈로그(.lrcat) 가져오기…", "Import Lightroom catalog (.lrcat)…")).clicked() {
                                ui.close();
                                super::lrcat_import::open(self);
                            }
                            if ui.button(tr!("Lightroom 프리셋 가져오기…", "Import Lightroom presets…")).clicked() {
                                ui.close();
                                super::dialogs::open_lr_import(self);
                            }
                        });
                        let mut cancel = false;
                        if let Some(ex) = &self.export {
                            if ui.small_button("×").on_hover_text(tr!("내보내기 취소", "Cancel export")).clicked() {
                                cancel = true;
                            }
                            let frac = if ex.total > 0 { ex.done as f32 / ex.total as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(frac).desired_width(140.0).text(mono(trf!("내보내기 {}/{}", "Export {}/{}", ex.done, ex.total))));
                        }
                        if cancel
                            && let Some(ex) = &self.export {
                                ex.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        super::enhance::status(self, ui);
                        if let Some(j) = &self.pano_job {
                            ui.spinner();
                            ui.label(mono(&j.status).color(TEXT_WEAK()));
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        }
                        if let Some(j) = &self.hdr_job {
                            ui.spinner();
                            ui.label(mono(&j.status).color(TEXT_WEAK()));
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        }
                        if let Some(pr) = &self.lrcat_progress {
                            let frac = if pr.total > 0 { pr.done as f32 / pr.total as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(frac).desired_width(160.0).text(mono(trf!("LR 카탈로그 {}/{}", "LR catalog {}/{}", pr.done, pr.total))));
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                        }
                        if let Some(im) = &self.import {
                            let frac = if im.total > 0 { im.done as f32 / im.total as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(frac).desired_width(140.0).text(mono(trf!("가져오기 {}/{}", "Import {}/{}", im.done, im.total))));
                        }
                    });
                });
            });
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.at.elapsed().as_secs_f32() < if t.error { 8.0 } else { 3.5 });
        if self.toasts.is_empty() {
            return;
        }
        let screen = ctx.content_rect();
        let mut y = screen.bottom() - 120.0;
        for t in self.toasts.iter().rev() {
            let id = egui::Id::new(("toast", t.at));
            egui::Area::new(id).fixed_pos(egui::pos2(screen.center().x, y)).pivot(Align2::CENTER_BOTTOM).order(egui::Order::Tooltip).show(ctx, |ui| {
                egui::Frame::new()
                    .fill(PANEL2())
                    .stroke(egui::Stroke::new(1.0, if t.error { ACCENT } else { BORDER() }))
                    .corner_radius(5.0)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(&t.text).color(TEXT()));
                    });
            });
            y -= 40.0;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        if let Some(k) = self.pending_theme.take() {
            super::theme::apply_theme(&ctx, k);
        }
        if self.visible_dirty {
            self.recompute_visible();
        }
        // Slideshow covers the whole screen
        if super::tools::slideshow_ui(self, &ctx) {
            super::autotest::tick(self, &ctx);
            return;
        }
        self.shortcuts(&ctx);
        self.top_bar(ui);
        if self.prefs.show_film && self.prefs.film_pos == FilmPos::Bottom && self.module != Module::Develop {
            super::library::filmstrip(self, ui);
        }
        match self.module {
            Module::Library => super::library::show(self, ui),
            Module::Develop => super::develop::show(self, ui),
        }
        super::dialogs::show_all(self, &ctx);
        super::tools::rename_dialog(self, &ctx);
        // AI noise reduction button in the Develop panel
        if ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("open_enhance"))).unwrap_or(false) {
            super::enhance::open(self);
        }
        super::enhance::dialog(self, &ctx);
        super::tools::pano_dialog(self, &ctx);
        super::catalog_ui::new_dialog(self, &ctx);
        super::people_ui::rename_dialog(self, &ctx);
        super::tools::print_dialog(self, &ctx);
        super::tools::shortcuts_window(self, &ctx);
        super::tools::palette_ui(self, &ctx);
        self.draw_toasts(&ctx);
        super::autotest::tick(self, &ctx);
        self.frame_ms = t0.elapsed().as_secs_f32() * 1000.0;
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.save_prefs();
        self.save_session();
        if self.module == Module::Develop {
            self.commit_develop_now();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_prefs();
        self.save_session();
        if self.module == Module::Develop {
            self.leave_develop();
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        super::autotest::inject(self, raw_input);
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        BG().to_normalized_gamma_f32()
    }
}

pub fn about_text() -> String {
    trf!("{} {} — RAW 사진 관리·현상\n{} · {}", "{} {} — RAW photo management & developing\n{} · {}", config::APP_NAME, config::APP_VERSION, config::COPYRIGHT, config::REPO_URL)
}

pub fn fmt_size(b: u64) -> String {
    if b > 1 << 20 { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) } else { format!("{:.0} KB", b as f64 / 1024.0) }
}

pub fn exp_settings(app: &App) -> ExportSettings {
    app.cat.kv_get_json("export_last").unwrap_or_default()
}

