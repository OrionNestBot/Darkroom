//! Background workers: thumbnail pool, source decoder, develop renderer. The UI thread never blocks.

use crate::catalog::PhotoId;
use crate::config;
use crate::develop::image::SourceImage;
use crate::develop::pipeline::{Engine, Histogram, RenderRequest};
use crate::develop::settings::DevelopSettings;
use crate::imaging::decode::{self, Rgba8};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

// ─────────────────────────── Thumbnails ───────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbKind {
    Grid,
    Preview,
}

#[derive(Clone)]
pub struct ThumbReq {
    pub id: PhotoId,
    pub path: PathBuf,
    pub orientation: u16,
    pub ver: i64,
    pub kind: ThumbKind,
    /// Develop settings for edited photos (applied to the thumbnail).
    pub settings: Option<DevelopSettings>,
}

pub struct ThumbResult {
    pub id: PhotoId,
    pub ver: i64,
    pub kind: ThumbKind,
    pub image: Option<Rgba8>,
}

struct ThumbQueue {
    q: Mutex<VecDeque<ThumbReq>>,
    pending: Mutex<HashSet<(PhotoId, i64, ThumbKind)>>,
    cv: Condvar,
}

pub struct ThumbService {
    queue: Arc<ThumbQueue>,
    pub rx: Receiver<ThumbResult>,
}

pub fn cache_path(id: PhotoId, ver: i64, kind: ThumbKind) -> PathBuf {
    let dir = match kind {
        ThumbKind::Grid => config::THUMB_DIR,
        ThumbKind::Preview => config::PREVIEW_DIR,
    };
    config::cache_root().join(dir).join(format!("{id}_{ver}.jpg"))
}

fn remove_old_versions(id: PhotoId, ver: i64, kind: ThumbKind) {
    for v in (ver - 4).max(0)..ver {
        let _ = std::fs::remove_file(cache_path(id, v, kind));
    }
}

/// Generate thumbnail and preview (cache first).
pub fn produce(req: &ThumbReq) -> Option<Rgba8> {
    let path = cache_path(req.id, req.ver, req.kind);
    if let Ok(img) = Rgba8::load_jpeg_file(&path) {
        return Some(img);
    }
    let (size, q) = match req.kind {
        ThumbKind::Grid => (config::THUMB_SIZE, config::THUMB_JPEG_QUALITY),
        ThumbKind::Preview => (config::PREVIEW_SIZE, config::PREVIEW_JPEG_QUALITY),
    };
    // Grid thumbnails are downscaled from the preview cache when it exists (fast)
    let img = if req.kind == ThumbKind::Grid {
        if let Ok(pv) = Rgba8::load_jpeg_file(&cache_path(req.id, req.ver, ThumbKind::Preview)) {
            Some(pv.fit(size))
        } else {
            None
        }
    } else {
        None
    };
    let img = match img {
        Some(i) => i,
        None => match &req.settings {
            Some(s) => {
                let src = decode::decode_source(&req.path, req.orientation).ok()?;
                let pv = decode::render_preview_with(&src, s, config::PREVIEW_SIZE);
                // After an expensive develop, store both
                save_jpeg(&cache_path(req.id, req.ver, ThumbKind::Preview), &pv, config::PREVIEW_JPEG_QUALITY);
                remove_old_versions(req.id, req.ver, ThumbKind::Preview);
                pv.fit(size)
            }
            None => decode::decode_preview(&req.path, size, req.orientation).ok()?,
        },
    };
    save_jpeg(&path, &img, q);
    remove_old_versions(req.id, req.ver, req.kind);
    Some(img)
}

pub fn save_jpeg(path: &Path, img: &Rgba8, q: u8) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(bytes) = img.encode_jpeg(q) {
        let _ = std::fs::write(path, bytes);
    }
}

impl ThumbService {
    pub fn new(repaint: Repaint) -> Self {
        let queue = Arc::new(ThumbQueue { q: Mutex::new(VecDeque::new()), pending: Mutex::new(HashSet::new()), cv: Condvar::new() });
        let (tx, rx) = unbounded();
        let n = if config::THUMB_WORKERS == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(1).clamp(2, 12)
        } else {
            config::THUMB_WORKERS
        };
        for i in 0..n {
            let queue = queue.clone();
            let tx: Sender<ThumbResult> = tx.clone();
            let repaint = repaint.clone();
            std::thread::Builder::new()
                .stack_size(crate::config::WORKER_STACK)
                .name(format!("thumb{i}"))
                .spawn(move || loop {
                    let req = {
                        let mut q = queue.q.lock();
                        while q.is_empty() {
                            queue.cv.wait(&mut q);
                        }
                        // LIFO: most recently visible first
                        q.pop_back().unwrap()
                    };
                    let image = produce(&req);
                    queue.pending.lock().remove(&(req.id, req.ver, req.kind));
                    let _ = tx.send(ThumbResult { id: req.id, ver: req.ver, kind: req.kind, image });
                    repaint();
                })
                .expect("thumb thread");
        }
        Self { queue, rx }
    }

    pub fn request(&self, req: ThumbReq) {
        let key = (req.id, req.ver, req.kind);
        {
            let mut p = self.queue.pending.lock();
            if p.contains(&key) {
                // If already queued, only raise its priority
                let mut q = self.queue.q.lock();
                if let Some(pos) = q.iter().position(|r| (r.id, r.ver, r.kind) == key) {
                    let r = q.remove(pos).unwrap();
                    q.push_back(r);
                }
                return;
            }
            p.insert(key);
        }
        self.queue.q.lock().push_back(req);
        self.queue.cv.notify_one();
    }

    /// Drop stale off-screen requests (when the queue gets too long).
    pub fn trim(&self, keep: usize) {
        let mut q = self.queue.q.lock();
        if q.len() > keep {
            let drop_n = q.len() - keep;
            let mut p = self.queue.pending.lock();
            for r in q.drain(..drop_n) {
                p.remove(&(r.id, r.ver, r.kind));
            }
        }
    }
}

// ─────────────────────────── Develop ───────────────────────────

pub enum DevCmd {
    Open { id: PhotoId, path: PathBuf, orientation: u16 },
    Prefetch { path: PathBuf, orientation: u16 },
    Render(Box<RenderJob>),
    MakePreviews { id: PhotoId, ver: i64, path: PathBuf, settings: DevelopSettings },
}

pub struct RenderJob {
    pub id: PhotoId,
    pub path: PathBuf,
    pub settings: DevelopSettings,
    pub w: usize,
    pub h: usize,
    pub region: [f32; 4],
    pub draft: bool,
    pub clipping: bool,
    pub overlay: Option<usize>,
    pub generation: u64,
    /// 0 = main view, 1 = compare (before) view, 2 = detail panel 1:1 preview, 3 = enhance dialog
    pub slot: u8,
    /// Full render used as the background while zoomed (processed after the screen render).
    pub underlay: bool,
}

pub enum DevEvent {
    Opened { id: PhotoId, source: Arc<SourceImage> },
    Failed { id: PhotoId, error: String },
    Rendered { id: PhotoId, generation: u64, slot: u8, underlay: bool, region: [f32; 4], #[allow(dead_code)] draft: bool, image: Rgba8, hist: Histogram, ms: f32 },
    PreviewsReady { id: PhotoId, ver: i64, thumb: Rgba8 },
}

type SourceCache = Arc<Mutex<lru::LruCache<PathBuf, Arc<SourceImage>>>>;

pub struct DevService {
    decode_tx: Sender<DevCmd>,
    render_tx: Sender<DevCmd>,
    pub rx: Receiver<DevEvent>,
}

impl DevService {
    pub fn new(repaint: Repaint) -> Self {
        let cache: SourceCache = Arc::new(Mutex::new(lru::LruCache::new(NonZeroUsize::new(config::SOURCE_CACHE).unwrap())));
        let (ev_tx, ev_rx) = unbounded::<DevEvent>();
        let (dec_tx, dec_rx) = unbounded::<DevCmd>();
        let (ren_tx, ren_rx) = unbounded::<DevCmd>();

        // Decoder thread
        {
            let cache = cache.clone();
            let ev_tx = ev_tx.clone();
            let ren_tx = ren_tx.clone();
            let repaint = repaint.clone();
            std::thread::Builder::new()
                .stack_size(crate::config::WORKER_STACK)
                .name("decoder".into())
                .spawn(move || {
                    let mut prefetch: VecDeque<(PathBuf, u16)> = VecDeque::new();
                    loop {
                        let cmd = if prefetch.is_empty() {
                            match dec_rx.recv() {
                                Ok(c) => Some(c),
                                Err(_) => break,
                            }
                        } else {
                            dec_rx.try_recv().ok()
                        };
                        match cmd {
                            Some(DevCmd::Open { id, path, orientation }) => {
                                // Open requests take priority over queued prefetches
                                let hit = cache.lock().get(&path).cloned();
                                let res = match hit {
                                    Some(s) => Ok(s),
                                    None => decode::decode_source(&path, orientation).map(Arc::new),
                                };
                                match res {
                                    Ok(src) => {
                                        cache.lock().put(path.clone(), src.clone());
                                        let _ = ev_tx.send(DevEvent::Opened { id, source: src });
                                        let _ = ren_tx.send(DevCmd::Prefetch { path, orientation }); // wake the renderer
                                    }
                                    Err(e) => {
                                        let _ = ev_tx.send(DevEvent::Failed { id, error: format!("{e:#}") });
                                    }
                                }
                                repaint();
                            }
                            Some(DevCmd::Prefetch { path, orientation }) => {
                                if !prefetch.iter().any(|(p, _)| *p == path) {
                                    prefetch.push_back((path, orientation));
                                }
                            }
                            Some(_) => {}
                            None => {
                                if let Some((path, o)) = prefetch.pop_front()
                                    && !cache.lock().contains(&path)
                                        && let Ok(src) = decode::decode_source(&path, o) {
                                            cache.lock().put(path, Arc::new(src));
                                        }
                            }
                        }
                    }
                })
                .expect("decoder thread");
        }

        // Render thread
        {
            let cache = cache.clone();
            let repaint = repaint.clone();
            std::thread::Builder::new()
                .stack_size(crate::config::WORKER_STACK)
                .name("render".into())
                .spawn(move || {
                    let mut engine = Engine::default();
                    // [slots 0-3 screen, slots 0-3 underlay]; earlier entries first
                    let mut pending: [Option<Box<RenderJob>>; 8] = Default::default();
                    let idx = |j: &RenderJob| j.slot as usize % 4 + if j.underlay { 4 } else { 0 };
                    let mut previews: VecDeque<(PhotoId, i64, PathBuf, DevelopSettings)> = VecDeque::new();
                    loop {
                        // Wait, then drain all queued commands (only the latest render per slot is kept)
                        let first = if pending.iter().all(Option::is_none) && previews.is_empty() {
                            match ren_rx.recv() {
                                Ok(c) => Some(c),
                                Err(_) => break,
                            }
                        } else {
                            None
                        };
                        for cmd in first.into_iter().chain(ren_rx.try_iter()) {
                            match cmd {
                                DevCmd::Render(job) => {
                                    let s = idx(&job);
                                    pending[s] = Some(job);
                                }
                                DevCmd::MakePreviews { id, ver, path, settings } => {
                                    previews.retain(|p| p.0 != id);
                                    previews.push_back((id, ver, path, settings));
                                }
                                _ => {}
                            }
                        }
                        let mut did = false;
                        for s in 0..8 {
                            // Underlay renders run after all screen renders (a new screen request goes first)
                            if s >= 4 && (did || pending[..4].iter().any(Option::is_some)) {
                                break;
                            }
                            let ready = pending[s].as_ref().and_then(|j| cache.lock().get(&j.path).cloned());
                            if let Some(src) = ready {
                                let job = pending[s].take().unwrap();
                                let t = std::time::Instant::now();
                                let scale = if job.draft { config::DRAFT_SCALE } else { 1.0 };
                                let w = ((job.w as f32 * scale) as usize).max(1);
                                let h = ((job.h as f32 * scale) as usize).max(1);
                                // Keep the thread alive if a render panics (unexpected settings or file): rebuild the engine and skip this render
                                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    engine.render(
                                        &src,
                                        &RenderRequest {
                                            settings: &job.settings,
                                            out_w: w,
                                            out_h: h,
                                            region: job.region,
                                            draft: job.draft,
                                            clipping: job.clipping,
                                            mask_overlay: job.overlay,
                                            keep_float: false,
                                        },
                                    )
                                }));
                                let Ok(mut out) = r else {
                                    engine = Engine::default();
                                    continue;
                                };
                                // Soft proofing applies to the display only (the histogram also uses the proofed colors)
                                if job.settings.view_proof != 0 && crate::export::proof::apply_rgba(job.settings.view_proof, &mut out.rgba, [255, 40, 40]) {
                                    out.hist = crate::develop::pipeline::Histogram::from_rgba(&out.rgba);
                                }
                                let _ = ev_tx.send(DevEvent::Rendered {
                                    id: job.id,
                                    generation: job.generation,
                                    slot: job.slot,
                                    underlay: job.underlay,
                                    region: job.region,
                                    draft: job.draft,
                                    image: Rgba8 { w: out.w as u32, h: out.h as u32, data: out.rgba },
                                    hist: out.hist,
                                    ms: t.elapsed().as_secs_f32() * 1000.0,
                                });
                                repaint();
                                did = true;
                            }
                        }
                        if did {
                            continue;
                        }
                        // Update thumbnails/previews only when no screen render is pending
                        if let Some((id, ver, path, settings)) = previews.pop_front() {
                            let src = cache.lock().get(&path).cloned();
                            if let Some(src) = src {
                                let Ok(pv) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode::render_preview_with(&src, &settings, config::PREVIEW_SIZE))) else { continue };
                                save_jpeg(&cache_path(id, ver, ThumbKind::Preview), &pv, config::PREVIEW_JPEG_QUALITY);
                                remove_old_versions(id, ver, ThumbKind::Preview);
                                let th = pv.fit(config::THUMB_SIZE);
                                save_jpeg(&cache_path(id, ver, ThumbKind::Grid), &th, config::THUMB_JPEG_QUALITY);
                                remove_old_versions(id, ver, ThumbKind::Grid);
                                let _ = ev_tx.send(DevEvent::PreviewsReady { id, ver, thumb: th });
                                repaint();
                            }
                            continue;
                        }
                        if pending.iter().any(Option::is_some) {
                            // Waiting on source decoding: block until the next command (including the decoder's wake-up)
                            match ren_rx.recv() {
                                Ok(DevCmd::Render(job)) => {
                                    let s = idx(&job);
                                    pending[s] = Some(job);
                                }
                                Ok(DevCmd::MakePreviews { id, ver, path, settings }) => previews.push_back((id, ver, path, settings)),
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    }
                })
                .expect("render thread");
        }
        Self { decode_tx: dec_tx, render_tx: ren_tx, rx: ev_rx }
    }

    pub fn open(&self, id: PhotoId, path: PathBuf, orientation: u16) {
        let _ = self.decode_tx.send(DevCmd::Open { id, path, orientation });
    }

    pub fn prefetch(&self, path: PathBuf, orientation: u16) {
        let _ = self.decode_tx.send(DevCmd::Prefetch { path, orientation });
    }

    pub fn render(&self, job: RenderJob) {
        let _ = self.render_tx.send(DevCmd::Render(Box::new(job)));
    }

    pub fn make_previews(&self, id: PhotoId, ver: i64, path: PathBuf, settings: DevelopSettings) {
        let _ = self.render_tx.send(DevCmd::MakePreviews { id, ver, path, settings });
    }
}
