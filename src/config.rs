//! App-wide settings and constants; every tunable value lives here.
//! © 2026 OrionNest

pub const APP_NAME: &str = "Darkroom";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COPYRIGHT: &str = "© 2026 OrionNest";
/// Source repository (shown in the About screen; fallback host for AI files).
pub const REPO_URL: &str = "https://github.com/OrionNestBot/Darkroom";

// ── Storage locations ──
/// Catalog and caches live under %LOCALAPPDATA%\<DATA_DIR_NAME>\.
pub const DATA_DIR_NAME: &str = "Darkroom";
/// Default catalog file name inside the data folder.
pub const CATALOG_FILE: &str = "catalog.db";
/// Thumbnail cache folder (relative to the cache root).
pub const THUMB_DIR: &str = "cache/thumbs";
/// Preview cache folder (relative to the cache root).
pub const PREVIEW_DIR: &str = "cache/previews";

// ── Thumbnail / preview cache ──
/// Grid thumbnail long edge (px); sized for the largest grid cell on high-DPI screens.
pub const THUMB_SIZE: u32 = 384;
/// Library loupe preview long edge (px).
pub const PREVIEW_SIZE: u32 = 2048;
/// JPEG quality of cached thumbnails.
pub const THUMB_JPEG_QUALITY: u8 = 82;
/// JPEG quality of cached previews.
pub const PREVIEW_JPEG_QUALITY: u8 = 88;
/// Maximum thumbnail textures kept on the GPU (LRU).
pub const THUMB_TEXTURE_CACHE: usize = 1500;
/// Maximum texture uploads per frame (keeps scrolling smooth).
pub const MAX_TEXTURE_UPLOADS_PER_FRAME: usize = 24;
/// Thumbnail worker threads (0 = logical cores - 1).
pub const THUMB_WORKERS: usize = 0;

// ── Develop engine ──
/// Decoded source images kept in memory (current + next/previous prefetch).
pub const SOURCE_CACHE: usize = 3;
/// Long edge (px) of the smallest pyramid level.
pub const PYRAMID_MIN_EDGE: usize = 768;
/// Draft render scale while dragging a slider (1.0 = full screen resolution).
pub const DRAFT_SCALE: f32 = 0.5;
/// Tone curve / LUT resolution.
pub const LUT_SIZE: usize = 4096;
/// Brush mask raster resolution (long edge).
pub const BRUSH_MASK_RES: usize = 2048;
/// Default RAW exposure offset (EV); most RAW files render darker than the camera JPEG.
pub const RAW_BASELINE_EV: f32 = 0.6;

/// Worker thread stack size.
pub const WORKER_STACK: usize = 16 * 1024 * 1024;

// ── History / undo ──
/// Edits closer together than this (ms) merge into one history step.
pub const HISTORY_COALESCE_MS: u128 = 600;
/// Maximum history steps kept per photo.
pub const MAX_HISTORY_PER_PHOTO: usize = 500;

// ── UI ──────────────────────────────────────────────────
/// Accent color (RGB).
pub const ACCENT: [u8; 3] = [0xC3, 0x2B, 0x21];
/// Minimum grid cell size (px).
pub const GRID_CELL_MIN: f32 = 110.0;
/// Maximum grid cell size (px).
pub const GRID_CELL_MAX: f32 = 360.0;
/// Default grid cell size (px).
pub const GRID_CELL_DEFAULT: f32 = 180.0;
/// Filmstrip height (px).
pub const FILMSTRIP_HEIGHT: f32 = 96.0;
/// Default left panel width (px).
pub const LEFT_PANEL_WIDTH: f32 = 240.0;
/// Default right panel width (px).
pub const RIGHT_PANEL_WIDTH: f32 = 320.0;
/// UI fonts, tried in order.
pub const UI_FONT_PATHS: &[&str] = &[r"C:\Windows\Fonts\malgun.ttf", r"C:\Windows\Fonts\segoeui.ttf"];
/// Monospace fonts, tried in order.
pub const MONO_FONT_PATHS: &[&str] = &[r"C:\Windows\Fonts\consola.ttf", r"C:\Windows\Fonts\cascadiamono.ttf"];
/// System font folder.
pub const SYSTEM_FONT_DIR: &str = r"C:\Windows\Fonts";

// ── Icon ──
/// Application icon.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

// ── Supported file extensions ──
/// RAW file extensions.
pub const RAW_EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "dng", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "erf", "3fr",
    "iiq", "mos", "mrw", "kdc", "dcr", "drhdr",
];
/// Raster image extensions.
pub const RASTER_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff"];
/// HEIF image extensions.
pub const HEIF_EXTS: &[&str] = &["heic", "heif", "hif"];

pub fn is_supported_ext(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    RAW_EXTS.contains(&e.as_str()) || RASTER_EXTS.contains(&e.as_str()) || HEIF_EXTS.contains(&e.as_str())
}

pub fn is_raw_ext(ext: &str) -> bool {
    RAW_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// File extension for user-created catalogs (SQLite).
pub const CATALOG_EXT: &str = "drcat";
/// Number of automatic catalog backups to keep.
pub const BACKUP_KEEP: usize = 5;
/// Length of the recent catalogs list.
pub const RECENT_CATALOGS: usize = 8;

static CATALOG_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Sets the current catalog path once at startup.
pub fn set_catalog_path(p: std::path::PathBuf) {
    let _ = CATALOG_PATH.set(p);
}

pub fn default_catalog_path() -> std::path::PathBuf {
    data_dir().join(CATALOG_FILE)
}

pub fn catalog_path() -> std::path::PathBuf {
    CATALOG_PATH.get().cloned().unwrap_or_else(default_catalog_path)
}

/// Thumbnail/preview cache root: the data folder for the default catalog, otherwise a
/// "<name> Previews" folder next to the catalog.
pub fn cache_root() -> std::path::PathBuf {
    let c = catalog_path();
    if c == default_catalog_path() {
        return data_dir();
    }
    let stem = c.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "catalog".into());
    c.parent().map(|d| d.join(format!("{stem} Previews"))).unwrap_or_else(data_dir)
}

// ── People (faces) ──
/// Minimum detector score (YuNet) to accept a face; real faces usually score above 0.9, masks/animals/dolls lower.
pub const FACE_MIN_SCORE: f32 = 0.85;
/// Ignore faces smaller than this fraction of the long edge.
pub const FACE_MIN_SIZE: f32 = 0.02;
/// Feature similarity (SFace cosine) to treat two faces as the same person; stricter than the usual 0.363.
pub const FACE_SAME_PERSON: f32 = 0.45;
/// Preview size (long edge) used for AI masks; also the resulting mask resolution.
pub const AI_MASK_EDGE: u32 = 1600;
/// Preview size (long edge) used for face detection.
pub const FACE_SCAN_EDGE: u32 = 1600;

/// Camera/lens combinations whose embedded lens corrections are applied by default: (camera model, lens name, flags:
/// d = distortion, c = chromatic aberration, v = vignetting). Empty fields match anything. DNG opcodes always apply.
pub const EMBEDDED_AUTO: &[(&str, &str, &str)] = &[
    // Olympus lenses (both the rawler name and the EXIF name)
    ("", "M.Zuiko Digital ED 12-40mm F2.8 Pro", "d"),
    ("", "OLYMPUS M.12-40mm F2.8", "d"),
    ("", "M.Zuiko Digital ED 14-42mm F3.5-5.6 EZ", "d"),
    ("", "OLYMPUS M.14-42mm F3.5-5.6 EZ", "d"),
    ("", "M.Zuiko Digital ED 12-100mm F4.0 IS Pro", "d"),
    ("", "OLYMPUS M.12-100mm F4.0", "d"),
    ("", "XF35mmF2 R WR", "d"),
    ("", "XF16-55mmF2.8 R LM WR", "dv"),
    ("X100V", "", "v"),
    // Sony zoom compacts (RX10/RX100) use embedded distortion and CA correction; the fixed-lens RX1R models do not.
    ("DSC-RX10", "", "dc"),
    ("DSC-RX10M2", "", "dc"),
    ("DSC-RX10M3", "", "dc"),
    ("DSC-RX10M4", "", "dc"),
    ("DSC-RX100", "", "dc"),
    ("DSC-RX100M2", "", "dc"),
    ("DSC-RX100M3", "", "dc"),
    ("DSC-RX100M4", "", "dc"),
    ("DSC-RX100M5", "", "dc"),
    ("DSC-RX100M5A", "", "dc"),
    ("DSC-RX100M6", "", "dc"),
    ("DSC-RX100M7", "", "dc"),
];

/// Camera calibration (primaries) sliders: if the fitted forward matrix primaries are farther than this (xy) from the
/// published color matrix primaries, compute from the published primaries instead.
pub const CAL_PRIMARY_DIST: f64 = 2.0;

/// Strength of the default vignetting correction for Nikon Z (relative to lens data, at the camera's "normal" setting;
/// low/high scale proportionally). 0 = off, because a fixed fraction of lensfun data does not match across lenses.
pub const NIKON_VC_NORMAL: f64 = 0.0;

/// Radius exponent of the Nikon Z embedded vignetting (0x06) polynomial (r^p, r^2p, ...); tuned by comparing rendered output.
pub const NIKON_VIG_POW: f64 = 2.0;

/// Radius scale of the Nikon Z embedded distortion coefficients (relative to half the diagonal); tuned by comparing rendered output.
pub const NIKON_DIST_SCALE: f64 = 1.05;

/// Per-model low-ISO exposure step: (camera, below this ISO, EV). Camera-generated DNGs change BaselineExposure by
/// 1 EV at that ISO, so the same rule is applied to the original RAW files.
pub const LOW_ISO_STEP: &[(&str, u32, f64)] = &[("Pentax K-1 Mark II", 200, -1.0)];

/// Fallback download hosts for AI executables/models: the file name is appended to each entry; the SHA-256 must match.
/// Uploading files to the `ai-files` release keeps downloads working without an app update if the original host disappears.
pub const ASSET_MIRRORS: &[&str] = &["https://github.com/OrionNestBot/Darkroom/releases/download/ai-files/"];

pub fn data_dir() -> std::path::PathBuf {
    // Test switch: DARKROOM_DATA_DIR overrides the data folder.
    if let Some(d) = std::env::var_os("DARKROOM_DATA_DIR") {
        return std::path::PathBuf::from(d);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(DATA_DIR_NAME)
}
