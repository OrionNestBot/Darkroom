//! Catalog (SQLite). All photo records are loaded into memory at startup so filtering and sorting are instant;
//! changes are written to the database immediately (WAL mode).

use crate::develop::settings::DevelopSettings;
use crate::imaging::meta::PhotoMeta;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type PhotoId = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}

impl Flag {
    fn to_i(self) -> i64 {
        match self {
            Flag::None => 0,
            Flag::Pick => 1,
            Flag::Reject => -1,
        }
    }
    fn from_i(v: i64) -> Self {
        match v {
            1 => Flag::Pick,
            -1 => Flag::Reject,
            _ => Flag::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub enum ColorLabel {
    #[default]
    None,
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    pub const ALL: [ColorLabel; 6] = [
        ColorLabel::None,
        ColorLabel::Red,
        ColorLabel::Yellow,
        ColorLabel::Green,
        ColorLabel::Blue,
        ColorLabel::Purple,
    ];
    pub fn to_i(self) -> i64 {
        self as i64
    }
    pub fn from_i(v: i64) -> Self {
        *Self::ALL.get(v as usize).unwrap_or(&ColorLabel::None)
    }
    pub fn name(self) -> &'static str {
        match self {
            ColorLabel::None => tr!("없음", "None"),
            ColorLabel::Red => tr!("빨강", "Red"),
            ColorLabel::Yellow => tr!("노랑", "Yellow"),
            ColorLabel::Green => tr!("초록", "Green"),
            ColorLabel::Blue => tr!("파랑", "Blue"),
            ColorLabel::Purple => tr!("보라", "Purple"),
        }
    }
    /// Label display color (a data color for classifying photos, an exception to the neutral UI chrome rule).
    pub fn rgb(self) -> [u8; 3] {
        match self {
            ColorLabel::None => [80, 80, 80],
            ColorLabel::Red => [0xC3, 0x2B, 0x21],
            ColorLabel::Yellow => [0xD9, 0xB2, 0x2A],
            ColorLabel::Green => [0x4C, 0x9A, 0x4A],
            ColorLabel::Blue => [0x3A, 0x6F, 0xC4],
            ColorLabel::Purple => [0x86, 0x4C, 0xB0],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Photo {
    pub id: PhotoId,
    pub path: PathBuf,
    pub folder: PathBuf,
    pub file_name: String,
    pub ext: String,
    pub file_size: u64,
    pub is_raw: bool,
    pub meta: PhotoMeta,
    pub rating: u8,
    pub flag: Flag,
    pub label: ColorLabel,
    pub title: String,
    pub caption: String,
    /// Source photo id if this is a virtual copy
    pub master: Option<PhotoId>,
    pub copy_name: String,
    pub develop: Option<DevelopSettings>,
    pub import_id: i64,
    #[allow(dead_code)] // kept for future sort options
    pub imported_at: i64,
    pub keywords: Vec<String>,
    /// Thumbnail version (bumped on edit to invalidate the cache)
    pub thumb_ver: i64,
    pub missing: bool,
    /// Last export time (0 = never)
    pub exported_at: i64,
    /// Last develop edit time (0 = never)
    pub edited_at: i64,
    /// Stack: 0 = none, otherwise the stack id (id of the top photo). stack_pos 0 = top
    pub stack_id: i64,
    pub stack_pos: i32,
}

impl Photo {
    pub fn has_edits(&self) -> bool {
        self.develop.as_ref().map(|d| !d.is_default_for(self.is_raw)).unwrap_or(false)
    }
    pub fn settings(&self) -> DevelopSettings {
        self.develop.clone().unwrap_or_else(|| DevelopSettings::default_for(self.is_raw))
    }
    pub fn display_name(&self) -> String {
        if self.master.is_some() {
            format!("{} ({})", self.file_name, if self.copy_name.is_empty() { tr!("사본", "Copy") } else { &self.copy_name })
        } else {
            self.file_name.clone()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
    /// Smart collection rules (None for a regular collection)
    pub smart: Option<SmartRules>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SmartRules {
    /// true = all conditions must match, false = any
    pub match_all: bool,
    pub rules: Vec<SmartRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SmartRule {
    RatingAtLeast(u8),
    RatingAtMost(u8),
    FlagIs(Flag),
    LabelIs(ColorLabel),
    KeywordContains(String),
    TextContains(String),
    CameraContains(String),
    LensContains(String),
    CapturedAfter(String),
    CapturedBefore(String),
    FileTypeIs(String),
    HasEdits(bool),
    IsoAtLeast(u32),
    IsoAtMost(u32),
    FocalAtLeast(f32),
    FocalAtMost(f32),
    InFolder(String),
    HasKeywords(bool),
    CapturedWithinDays(u32),
    EditedWithinDays(u32),
}

impl SmartRule {
    pub fn matches(&self, p: &Photo) -> bool {
        let lc = |s: &Option<String>| s.as_deref().unwrap_or("").to_lowercase();
        match self {
            SmartRule::RatingAtLeast(r) => p.rating >= *r,
            SmartRule::RatingAtMost(r) => p.rating <= *r,
            SmartRule::FlagIs(f) => p.flag == *f,
            SmartRule::LabelIs(l) => p.label == *l,
            SmartRule::KeywordContains(k) => {
                let k = k.to_lowercase();
                p.keywords.iter().any(|x| x.to_lowercase().contains(&k))
            }
            SmartRule::TextContains(t) => text_match(p, t),
            SmartRule::CameraContains(c) => {
                format!("{} {}", lc(&p.meta.make), lc(&p.meta.model)).contains(&c.to_lowercase())
            }
            SmartRule::LensContains(c) => lc(&p.meta.lens).contains(&c.to_lowercase()),
            SmartRule::CapturedAfter(d) => p.meta.capture_time.as_deref().map(|t| t >= d.as_str()).unwrap_or(false),
            SmartRule::CapturedBefore(d) => p.meta.capture_time.as_deref().map(|t| t <= d.as_str()).unwrap_or(false),
            SmartRule::FileTypeIs(e) => p.ext.eq_ignore_ascii_case(e),
            SmartRule::HasEdits(b) => p.has_edits() == *b,
            SmartRule::IsoAtLeast(v) => p.meta.iso.map(|i| i >= *v).unwrap_or(false),
            SmartRule::IsoAtMost(v) => p.meta.iso.map(|i| i <= *v).unwrap_or(false),
            SmartRule::FocalAtLeast(v) => p.meta.focal.map(|f| f >= *v).unwrap_or(false),
            SmartRule::FocalAtMost(v) => p.meta.focal.map(|f| f <= *v).unwrap_or(false),
            SmartRule::InFolder(f) => p.folder.to_string_lossy().to_lowercase().contains(&f.to_lowercase()),
            SmartRule::HasKeywords(b) => !p.keywords.is_empty() == *b,
            SmartRule::CapturedWithinDays(d) => {
                let cutoff = days_ago_string(*d);
                p.meta.capture_time.as_deref().map(|t| t >= cutoff.as_str()).unwrap_or(false)
            }
            SmartRule::EditedWithinDays(d) => p.edited_at > 0 && p.edited_at >= now() - *d as i64 * 86400,
        }
    }
}

impl SmartRules {
    pub fn matches(&self, p: &Photo) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        if self.match_all { self.rules.iter().all(|r| r.matches(p)) } else { self.rules.iter().any(|r| r.matches(p)) }
    }
}

/// Every space-separated word must appear in the file name, title, caption, keywords, camera or lens.
pub fn text_match(p: &Photo, q: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {} {} {}",
        p.file_name,
        p.title,
        p.caption,
        p.keywords.join(" "),
        p.meta.make.as_deref().unwrap_or(""),
        p.meta.model.as_deref().unwrap_or(""),
        p.meta.lens.as_deref().unwrap_or("")
    )
    .to_lowercase();
    q.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub label: String,
    pub settings: DevelopSettings,
    #[allow(dead_code)] // for showing history timestamps
    pub ts: i64,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: i64,
    pub name: String,
    pub settings: DevelopSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub id: i64,
    pub group: String,
    pub name: String,
    pub settings: DevelopSettings,
    pub groups: crate::develop::settings::SettingGroups,
}

pub struct Catalog {
    conn: Connection,
    pub photos: Vec<Photo>,
    index: HashMap<PhotoId, usize>,
    pub collections: Vec<Collection>,
    pub collection_members: HashMap<i64, Vec<PhotoId>>,
    pub quick_collection: HashSet<PhotoId>,
    pub last_import: i64,
    #[allow(dead_code)] // for catalog backup / relocation
    pub path: PathBuf,
    preset_cache: std::cell::RefCell<Option<std::sync::Arc<Vec<Preset>>>>,
    /// People: detected faces, persons, and photos already scanned for faces
    pub faces: Vec<FaceRec>,
    pub persons: Vec<Person>,
    pub face_scanned: HashSet<PhotoId>,
}

/// A detected face. person: 0 = unassigned, -1 = excluded from grouping (removed by the user), >0 = person id
#[derive(Clone, Debug)]
pub struct FaceRec {
    pub id: i64,
    pub photo: PhotoId,
    pub bbox: [f32; 4],
    pub score: f32,
    pub emb: Vec<f32>,
    pub person: i64,
}

#[derive(Clone, Debug)]
pub struct Person {
    pub id: i64,
    pub name: String,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS photos(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL,
  file_size INTEGER NOT NULL DEFAULT 0,
  meta TEXT NOT NULL DEFAULT '{}',
  rating INTEGER NOT NULL DEFAULT 0,
  flag INTEGER NOT NULL DEFAULT 0,
  label INTEGER NOT NULL DEFAULT 0,
  title TEXT NOT NULL DEFAULT '',
  caption TEXT NOT NULL DEFAULT '',
  master_id INTEGER REFERENCES photos(id) ON DELETE CASCADE,
  copy_name TEXT NOT NULL DEFAULT '',
  develop TEXT,
  import_id INTEGER NOT NULL DEFAULT 0,
  imported_at INTEGER NOT NULL DEFAULT 0,
  thumb_ver INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS photos_path ON photos(path);
CREATE TABLE IF NOT EXISTS keywords(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS photo_keywords(
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  keyword_id INTEGER NOT NULL REFERENCES keywords(id) ON DELETE CASCADE,
  PRIMARY KEY(photo_id, keyword_id)
);
CREATE TABLE IF NOT EXISTS collections(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, parent_id INTEGER, smart TEXT
);
CREATE TABLE IF NOT EXISTS collection_photos(
  collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  position INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(collection_id, photo_id)
);
CREATE TABLE IF NOT EXISTS quick_collection(photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS history(
  id INTEGER PRIMARY KEY,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  label TEXT NOT NULL, settings TEXT NOT NULL, ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS history_photo ON history(photo_id, id);
CREATE TABLE IF NOT EXISTS snapshots(
  id INTEGER PRIMARY KEY,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  name TEXT NOT NULL, settings TEXT NOT NULL, ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS presets(
  id INTEGER PRIMARY KEY, grp TEXT NOT NULL, name TEXT NOT NULL, data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS persons(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS faces(
  id INTEGER PRIMARY KEY,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
  score REAL NOT NULL, emb BLOB NOT NULL, thumb BLOB,
  person_id INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS faces_photo ON faces(photo_id);
CREATE TABLE IF NOT EXISTS face_scanned(photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS ai_masks(key TEXT PRIMARY KEY, png BLOB NOT NULL, created INTEGER NOT NULL DEFAULT 0);
"#;

/// Date d days before today as "YYYY-MM-DD 00:00:00" (UTC)
pub fn days_ago_string(d: u32) -> String {
    let t = now() - d as i64 * 86400;
    let days = t.div_euclid(86400);
    // Days since 1970-01-01 -> Gregorian date (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{dd:02} 00:00:00")
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        // Schema additions (compatible with older catalogs)
        let has_col = |conn: &Connection, col: &str| -> bool {
            conn.prepare("SELECT name FROM pragma_table_info('photos')")
                .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().any(|n| n == col)))
                .unwrap_or(false)
        };
        if !has_col(&conn, "exported_at") {
            conn.execute_batch("ALTER TABLE photos ADD COLUMN exported_at INTEGER NOT NULL DEFAULT 0;")?;
        }
        if !has_col(&conn, "edited_at") {
            conn.execute_batch("ALTER TABLE photos ADD COLUMN edited_at INTEGER NOT NULL DEFAULT 0;")?;
        }
        if !has_col(&conn, "stack_id") {
            conn.execute_batch("ALTER TABLE photos ADD COLUMN stack_id INTEGER NOT NULL DEFAULT 0; ALTER TABLE photos ADD COLUMN stack_pos INTEGER NOT NULL DEFAULT 0;")?;
        }
        let mut c = Catalog {
            conn,
            photos: Vec::new(),
            index: HashMap::new(),
            collections: Vec::new(),
            collection_members: HashMap::new(),
            quick_collection: HashSet::new(),
            last_import: 0,
            path: path.to_path_buf(),
            preset_cache: Default::default(),
            faces: Vec::new(),
            persons: Vec::new(),
            face_scanned: HashSet::new(),
        };
        c.load_all()?;
        Ok(c)
    }

    fn load_all(&mut self) -> Result<()> {
        let mut kw: HashMap<PhotoId, Vec<String>> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT pk.photo_id, k.name FROM photo_keywords pk JOIN keywords k ON k.id = pk.keyword_id")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows.flatten() {
                kw.entry(row.0).or_default().push(row.1);
            }
        }
        let mut st = self.conn.prepare(
            "SELECT id, path, file_size, meta, rating, flag, label, title, caption, master_id, copy_name, develop, import_id, imported_at, thumb_ver FROM photos ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            let path: String = r.get(1)?;
            let meta: String = r.get(3)?;
            let dev: Option<String> = r.get(11)?;
            Ok((
                r.get::<_, i64>(0)?,
                path,
                r.get::<_, i64>(2)?,
                meta,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, String>(10)?,
                dev,
                r.get::<_, i64>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, i64>(14)?,
            ))
        })?;
        let mut photos = Vec::new();
        for row in rows {
            let (id, path, size, meta, rating, flag, label, title, caption, master, copy_name, dev, import_id, imported_at, tv) = row?;
            let p = PathBuf::from(&path);
            let mut kws = kw.remove(&id).unwrap_or_default();
            kws.sort_by_key(|s| s.to_lowercase());
            photos.push(make_photo(
                id,
                p,
                size as u64,
                serde_json::from_str(&meta).unwrap_or_default(),
                rating as u8,
                Flag::from_i(flag),
                ColorLabel::from_i(label),
                title,
                caption,
                master,
                copy_name,
                dev.and_then(|d| DevelopSettings::from_json(&d)),
                import_id,
                imported_at,
                kws,
                tv,
            ));
        }
        drop(st);
        self.photos = photos;
        self.reindex();
        let ex: Vec<(i64, i64, i64)> = {
            let mut st = self.conn.prepare("SELECT id, exported_at, edited_at FROM photos WHERE exported_at > 0 OR edited_at > 0")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.flatten().collect()
        };
        for (id, t, e) in ex {
            if let Some(p) = self.get_mut(id) {
                p.exported_at = t;
                p.edited_at = e;
            }
        }
        let st: Vec<(i64, i64, i32)> = {
            let mut st = self.conn.prepare("SELECT id, stack_id, stack_pos FROM photos WHERE stack_id <> 0")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.flatten().collect()
        };
        for (id, sid, pos) in st {
            if let Some(p) = self.get_mut(id) {
                p.stack_id = sid;
                p.stack_pos = pos;
            }
        }
        self.load_collections()?;
        let qc: Vec<i64> = {
            let mut st = self.conn.prepare("SELECT photo_id FROM quick_collection")?;
            st.query_map([], |r| r.get(0))?.flatten().collect()
        };
        self.quick_collection = qc.into_iter().collect();
        self.last_import = self.photos.iter().map(|p| p.import_id).max().unwrap_or(0);
        self.load_people()?;
        Ok(())
    }

    fn load_people(&mut self) -> Result<()> {
        let mut st = self.conn.prepare("SELECT id, name FROM persons ORDER BY id")?;
        self.persons = st.query_map([], |r| Ok(Person { id: r.get(0)?, name: r.get(1)? }))?.flatten().collect();
        let mut st = self.conn.prepare("SELECT id, photo_id, x, y, w, h, score, emb, person_id FROM faces ORDER BY id")?;
        self.faces = st
            .query_map([], |r| {
                let b: Vec<u8> = r.get(7)?;
                Ok(FaceRec {
                    id: r.get(0)?,
                    photo: r.get(1)?,
                    bbox: [r.get::<_, f64>(2)? as f32, r.get::<_, f64>(3)? as f32, r.get::<_, f64>(4)? as f32, r.get::<_, f64>(5)? as f32],
                    score: r.get::<_, f64>(6)? as f32,
                    emb: b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
                    person: r.get(8)?,
                })
            })?
            .flatten()
            .collect();
        let mut st = self.conn.prepare("SELECT photo_id FROM face_scanned")?;
        self.face_scanned = st.query_map([], |r| r.get(0))?.flatten().collect();
        Ok(())
    }

    // ── People ──

    /// Face records for one photo (replaced when detection runs again)
    pub fn add_faces(&mut self, photo: PhotoId, hits: &[crate::imaging::people::FaceHit]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM faces WHERE photo_id=?1", params![photo])?;
        let mut new = Vec::new();
        for h in hits {
            let emb: Vec<u8> = h.emb.iter().flat_map(|v| v.to_le_bytes()).collect();
            tx.execute(
                "INSERT INTO faces(photo_id, x, y, w, h, score, emb, thumb, person_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)",
                params![photo, h.bbox[0] as f64, h.bbox[1] as f64, h.bbox[2] as f64, h.bbox[3] as f64, h.score as f64, emb, h.thumb],
            )?;
            new.push(FaceRec { id: tx.last_insert_rowid(), photo, bbox: h.bbox, score: h.score, emb: h.emb.clone(), person: 0 });
        }
        tx.execute("INSERT OR IGNORE INTO face_scanned(photo_id) VALUES(?1)", params![photo])?;
        tx.commit()?;
        self.faces.retain(|f| f.photo != photo);
        self.faces.extend(new);
        self.face_scanned.insert(photo);
        Ok(())
    }

    /// Store an AI mask bitmap (keyed by content hash; unchanged if identical)
    pub fn save_ai_mask(&self, key: &str, png: &[u8]) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO ai_masks(key, png, created) VALUES(?1, ?2, ?3)", params![key, png, now()])?;
        Ok(())
    }

    pub fn face_thumb(&self, face: i64) -> Option<Vec<u8>> {
        self.conn.query_row("SELECT thumb FROM faces WHERE id=?1", params![face], |r| r.get::<_, Option<Vec<u8>>>(0)).ok().flatten()
    }

    /// Apply grouping results. Temporary ids (>= NEW_BASE) become new persons. Returns the number of new persons
    pub fn apply_face_groups(&mut self, changes: &[(i64, i64)]) -> Result<usize> {
        let mut map: HashMap<i64, i64> = HashMap::new();
        let tx = self.conn.savepoint()?;
        for (_, p) in changes {
            if *p >= crate::imaging::people::NEW_BASE && !map.contains_key(p) {
                tx.execute("INSERT INTO persons(name) VALUES('')", [])?;
                map.insert(*p, tx.last_insert_rowid());
            }
        }
        for (f, p) in changes {
            let pid = map.get(p).copied().unwrap_or(*p);
            tx.execute("UPDATE faces SET person_id=?1 WHERE id=?2", params![pid, f])?;
        }
        tx.commit()?;
        let mut ids: Vec<i64> = map.values().copied().collect();
        ids.sort();
        for id in ids {
            self.persons.push(Person { id, name: String::new() });
        }
        for (f, p) in changes {
            let pid = map.get(p).copied().unwrap_or(*p);
            if let Some(r) = self.faces.iter_mut().find(|r| r.id == *f) {
                r.person = pid;
            }
        }
        self.drop_empty_persons()?;
        Ok(map.len())
    }

    /// Change the person of a set of faces (remove = -1)
    pub fn set_face_person(&mut self, faces: &[i64], person: i64) -> Result<()> {
        let tx = self.conn.savepoint()?;
        for f in faces {
            tx.execute("UPDATE faces SET person_id=?1 WHERE id=?2", params![person, f])?;
        }
        tx.commit()?;
        for r in self.faces.iter_mut().filter(|r| faces.contains(&r.id)) {
            r.person = person;
        }
        self.drop_empty_persons()
    }

    pub fn rename_person(&mut self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE persons SET name=?1 WHERE id=?2", params![name.trim(), id])?;
        if let Some(p) = self.persons.iter_mut().find(|p| p.id == id) {
            p.name = name.trim().to_string();
        }
        Ok(())
    }

    /// Move all faces of `from` into `into` (`from` is deleted)
    pub fn merge_person(&mut self, from: i64, into: i64) -> Result<()> {
        let ids: Vec<i64> = self.faces.iter().filter(|f| f.person == from).map(|f| f.id).collect();
        self.set_face_person(&ids, into)
    }

    /// Dissolve a person: its faces become excluded from grouping
    pub fn dissolve_person(&mut self, id: i64) -> Result<()> {
        let ids: Vec<i64> = self.faces.iter().filter(|f| f.person == id).map(|f| f.id).collect();
        self.set_face_person(&ids, -1)
    }

    fn drop_empty_persons(&mut self) -> Result<()> {
        let used: HashSet<i64> = self.faces.iter().map(|f| f.person).collect();
        let gone: Vec<i64> = self.persons.iter().filter(|p| !used.contains(&p.id)).map(|p| p.id).collect();
        for id in &gone {
            self.conn.execute("DELETE FROM persons WHERE id=?1", params![id])?;
        }
        self.persons.retain(|p| !gone.contains(&p.id));
        Ok(())
    }

    pub fn person_photos(&self, id: i64) -> HashSet<PhotoId> {
        self.faces.iter().filter(|f| f.person == id).map(|f| f.photo).collect()
    }

    /// Representative face of a person (largest, highest score)
    pub fn person_cover(&self, id: i64) -> Option<i64> {
        self.faces.iter().filter(|f| f.person == id).max_by(|a, b| (a.bbox[2] * a.score).total_cmp(&(b.bbox[2] * b.score))).map(|f| f.id)
    }

    /// Delete all face records (to detect from scratch)
    pub fn clear_people(&mut self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM faces; DELETE FROM persons; DELETE FROM face_scanned;")?;
        self.faces.clear();
        self.persons.clear();
        self.face_scanned.clear();
        Ok(())
    }

    fn load_collections(&mut self) -> Result<()> {
        let mut st = self.conn.prepare("SELECT id, name, parent_id, smart FROM collections ORDER BY name COLLATE NOCASE")?;
        self.collections = st
            .query_map([], |r| {
                let smart: Option<String> = r.get(3)?;
                Ok(Collection {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    parent: r.get(2)?,
                    smart: smart.and_then(|s| serde_json::from_str(&s).ok()),
                })
            })?
            .flatten()
            .collect();
        drop(st);
        let mut st = self.conn.prepare("SELECT collection_id, photo_id FROM collection_photos ORDER BY position")?;
        let mut m: HashMap<i64, Vec<PhotoId>> = HashMap::new();
        for (c, p) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?.flatten() {
            m.entry(c).or_default().push(p);
        }
        self.collection_members = m;
        Ok(())
    }

    fn reindex(&mut self) {
        self.index = self.photos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    }

    pub fn get(&self, id: PhotoId) -> Option<&Photo> {
        self.index.get(&id).map(|i| &self.photos[*i])
    }

    pub fn get_mut(&mut self, id: PhotoId) -> Option<&mut Photo> {
        self.index.get(&id).copied().map(move |i| &mut self.photos[i])
    }

    #[allow(dead_code)] // for folder relinking and metadata re-read
    pub fn contains_path(&self, path: &Path) -> bool {
        // case-insensitive comparison (Windows)
        let key = path.to_string_lossy().to_lowercase();
        self.photos.iter().any(|p| p.master.is_none() && p.path.to_string_lossy().to_lowercase() == key)
    }

    pub fn known_paths(&self) -> HashSet<String> {
        self.photos
            .iter()
            .filter(|p| p.master.is_none())
            .map(|p| p.path.to_string_lossy().to_lowercase())
            .collect()
    }

    pub fn next_import_id(&mut self) -> i64 {
        self.last_import += 1;
        self.last_import
    }

    /// Add new photos in bulk (one transaction).
    pub fn add_photos(&mut self, items: &[(PathBuf, u64, PhotoMeta, Option<DevelopSettings>, Vec<String>)], import_id: i64) -> Result<Vec<PhotoId>> {
        let ts = now();
        let tx = self.conn.savepoint()?;
        let mut ids = Vec::new();
        {
            let mut st = tx.prepare(
                "INSERT INTO photos(path, file_size, meta, rating, develop, import_id, imported_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (path, size, meta, dev, _) in items {
                let rating = meta.rating.unwrap_or(0).min(5) as i64;
                st.execute(params![
                    path.to_string_lossy(),
                    *size as i64,
                    serde_json::to_string(meta)?,
                    rating,
                    dev.as_ref().map(|d| d.to_json()),
                    import_id,
                    ts
                ])?;
                ids.push(tx.last_insert_rowid());
            }
        }
        tx.commit()?;
        for (i, (path, size, meta, dev, _)) in items.iter().enumerate() {
            let rating = meta.rating.unwrap_or(0).min(5) as u8;
            self.photos.push(make_photo(
                ids[i],
                path.clone(),
                *size,
                meta.clone(),
                rating,
                Flag::None,
                ColorLabel::None,
                String::new(),
                String::new(),
                None,
                String::new(),
                dev.clone(),
                import_id,
                ts,
                Vec::new(),
                0,
            ));
        }
        self.reindex();
        // Keywords after the index update, so the in-memory records reflect them too
        for (i, (_, _, _, _, kws)) in items.iter().enumerate() {
            if !kws.is_empty() {
                let _ = self.add_keywords(&[ids[i]], kws);
            }
        }
        Ok(ids)
    }

    #[allow(dead_code)] // for folder relinking and metadata re-read
    pub fn update_meta(&mut self, id: PhotoId, meta: &PhotoMeta) -> Result<()> {
        self.conn
            .execute("UPDATE photos SET meta=?1 WHERE id=?2", params![serde_json::to_string(meta)?, id])?;
        if let Some(p) = self.get_mut(id) {
            p.meta = meta.clone();
        }
        Ok(())
    }

    pub fn set_rating(&mut self, ids: &[PhotoId], r: u8) -> Result<()> {
        self.batch_update(ids, "rating", r as i64)?;
        for id in ids {
            if let Some(p) = self.get_mut(*id) {
                p.rating = r;
            }
        }
        Ok(())
    }

    pub fn set_flag(&mut self, ids: &[PhotoId], f: Flag) -> Result<()> {
        self.batch_update(ids, "flag", f.to_i())?;
        for id in ids {
            if let Some(p) = self.get_mut(*id) {
                p.flag = f;
            }
        }
        Ok(())
    }

    pub fn set_label(&mut self, ids: &[PhotoId], l: ColorLabel) -> Result<()> {
        self.batch_update(ids, "label", l.to_i())?;
        for id in ids {
            if let Some(p) = self.get_mut(*id) {
                p.label = l;
            }
        }
        Ok(())
    }

    fn batch_update(&mut self, ids: &[PhotoId], col: &str, v: i64) -> Result<()> {
        let tx = self.conn.savepoint()?;
        {
            let mut st = tx.prepare(&format!("UPDATE photos SET {col}=?1 WHERE id=?2"))?;
            for id in ids {
                st.execute(params![v, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_text(&mut self, id: PhotoId, title: &str, caption: &str) -> Result<()> {
        self.conn
            .execute("UPDATE photos SET title=?1, caption=?2 WHERE id=?3", params![title, caption, id])?;
        if let Some(p) = self.get_mut(id) {
            p.title = title.to_string();
            p.caption = caption.to_string();
        }
        Ok(())
    }

    pub fn set_develop(&mut self, id: PhotoId, s: &DevelopSettings) -> Result<()> {
        let t = now();
        self.conn.execute(
            "UPDATE photos SET develop=?1, thumb_ver=thumb_ver+1, edited_at=?3 WHERE id=?2",
            params![s.to_json(), id, t],
        )?;
        if let Some(p) = self.get_mut(id) {
            p.develop = Some(s.clone());
            p.thumb_ver += 1;
            p.edited_at = t;
        }
        Ok(())
    }

    /// Save during develop (keeps the thumbnail version; it is bumped once when leaving the photo).
    pub fn save_develop_quiet(&mut self, id: PhotoId, s: &DevelopSettings) -> Result<()> {
        let t = now();
        self.conn.execute("UPDATE photos SET develop=?1, edited_at=?3 WHERE id=?2", params![s.to_json(), id, t])?;
        if let Some(p) = self.get_mut(id) {
            p.develop = Some(s.clone());
            p.edited_at = t;
        }
        Ok(())
    }

    /// Group several updates in one transaction (inner methods use savepoints, so this can nest).
    pub fn begin_batch(&mut self) {
        let _ = self.conn.execute_batch("BEGIN");
    }

    pub fn end_batch(&mut self) {
        let _ = self.conn.execute_batch("COMMIT");
    }

    pub fn set_copy_name(&mut self, id: PhotoId, name: &str) -> Result<()> {
        self.conn.execute("UPDATE photos SET copy_name=?1 WHERE id=?2", params![name, id])?;
        if let Some(p) = self.get_mut(id) {
            p.copy_name = name.to_string();
        }
        Ok(())
    }

    // ───────── Stacks ─────────

    /// Stack photos in the given order (first photo on top). Photos already in a stack bring their stack members along
    pub fn stack(&mut self, ids: &[PhotoId]) -> Result<i64> {
        if ids.len() < 2 {
            return Ok(0);
        }
        let mut all: Vec<PhotoId> = Vec::new();
        for &id in ids {
            let sid = self.get(id).map(|p| p.stack_id).unwrap_or(0);
            if sid != 0 {
                let mut mem: Vec<(i32, PhotoId)> = self.photos.iter().filter(|p| p.stack_id == sid).map(|p| (p.stack_pos, p.id)).collect();
                mem.sort();
                for (_, m) in mem {
                    if !all.contains(&m) {
                        all.push(m);
                    }
                }
            } else if !all.contains(&id) {
                all.push(id);
            }
        }
        let top = ids[0];
        if let Some(i) = all.iter().position(|x| *x == top) {
            all.remove(i);
        }
        all.insert(0, top);
        self.write_stack(&all)?;
        Ok(top)
    }

    fn write_stack(&mut self, order: &[PhotoId]) -> Result<()> {
        let sid = order[0];
        self.begin_batch();
        for (pos, id) in order.iter().enumerate() {
            self.conn.execute("UPDATE photos SET stack_id=?1, stack_pos=?2 WHERE id=?3", params![sid, pos as i32, id])?;
            if let Some(p) = self.get_mut(*id) {
                p.stack_id = sid;
                p.stack_pos = pos as i32;
            }
        }
        self.end_batch();
        Ok(())
    }

    /// Remove from stack (dissolves the stack if only one member remains)
    pub fn unstack(&mut self, ids: &[PhotoId]) -> Result<()> {
        let mut touched: HashSet<i64> = HashSet::new();
        for &id in ids {
            let sid = self.get(id).map(|p| p.stack_id).unwrap_or(0);
            if sid == 0 {
                continue;
            }
            touched.insert(sid);
            self.conn.execute("UPDATE photos SET stack_id=0, stack_pos=0 WHERE id=?1", params![id])?;
            if let Some(p) = self.get_mut(id) {
                p.stack_id = 0;
                p.stack_pos = 0;
            }
        }
        for sid in touched {
            let mut mem: Vec<(i32, PhotoId)> = self.photos.iter().filter(|p| p.stack_id == sid).map(|p| (p.stack_pos, p.id)).collect();
            mem.sort();
            let order: Vec<PhotoId> = mem.into_iter().map(|m| m.1).collect();
            if order.len() <= 1 {
                for id in order {
                    self.conn.execute("UPDATE photos SET stack_id=0, stack_pos=0 WHERE id=?1", params![id])?;
                    if let Some(p) = self.get_mut(id) {
                        p.stack_id = 0;
                        p.stack_pos = 0;
                    }
                }
            } else {
                self.write_stack(&order)?;
            }
        }
        Ok(())
    }

    /// Dissolve every stack that contains the selected photos
    pub fn dissolve_stacks(&mut self, ids: &[PhotoId]) -> Result<()> {
        let sids: HashSet<i64> = ids.iter().filter_map(|i| self.get(*i)).map(|p| p.stack_id).filter(|s| *s != 0).collect();
        let mem: Vec<PhotoId> = self.photos.iter().filter(|p| sids.contains(&p.stack_id)).map(|p| p.id).collect();
        self.unstack(&mem)
    }

    /// Move this photo to the top of its stack
    pub fn set_stack_top(&mut self, id: PhotoId) -> Result<()> {
        let sid = self.get(id).map(|p| p.stack_id).unwrap_or(0);
        if sid == 0 {
            return Ok(());
        }
        let mut mem: Vec<(i32, PhotoId)> = self.photos.iter().filter(|p| p.stack_id == sid).map(|p| (p.stack_pos, p.id)).collect();
        mem.sort();
        let mut order: Vec<PhotoId> = mem.into_iter().map(|m| m.1).filter(|x| *x != id).collect();
        order.insert(0, id);
        // The stack id changes to the new top photo id, so update members still using the old id
        self.write_stack(&order)
    }

    pub fn stack_size(&self, sid: i64) -> usize {
        if sid == 0 { 0 } else { self.photos.iter().filter(|p| p.stack_id == sid).count() }
    }

    /// Auto-stack photos whose capture times are within `gap` seconds of each other (skips stacked photos and virtual copies). Returns the number of stacks created
    pub fn auto_stack(&mut self, ids: &[PhotoId], gap: f64) -> Result<usize> {
        let mut v: Vec<(f64, PhotoId)> = ids
            .iter()
            .filter_map(|i| self.get(*i))
            .filter(|p| p.stack_id == 0 && p.master.is_none())
            .filter_map(|p| p.meta.capture_time.as_deref().and_then(capture_secs).map(|t| (t, p.id)))
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut groups: Vec<Vec<PhotoId>> = Vec::new();
        let mut cur: Vec<PhotoId> = Vec::new();
        let mut last = f64::NEG_INFINITY;
        for (t, id) in v {
            if t - last > gap && !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            cur.push(id);
            last = t;
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
        let mut n = 0;
        for g in groups.into_iter().filter(|g| g.len() >= 2) {
            self.write_stack(&g)?;
            n += 1;
        }
        Ok(n)
    }

    /// Record imported edit times (e.g. touchTime from a .lrcat catalog)
    pub fn set_edited_at(&mut self, id: PhotoId, t: i64) {
        let _ = self.conn.execute("UPDATE photos SET edited_at=?1 WHERE id=?2", params![t, id]);
        if let Some(p) = self.get_mut(id) {
            p.edited_at = t;
        }
    }

    pub fn mark_exported(&mut self, id: PhotoId) {
        let t = now();
        let _ = self.conn.execute("UPDATE photos SET exported_at=?1 WHERE id=?2", params![t, id]);
        if let Some(p) = self.get_mut(id) {
            p.exported_at = t;
        }
    }

    pub fn bump_thumb(&mut self, id: PhotoId) {
        let _ = self.conn.execute("UPDATE photos SET thumb_ver=thumb_ver+1 WHERE id=?1", params![id]);
        if let Some(p) = self.get_mut(id) {
            p.thumb_ver += 1;
        }
    }

    // ── Keywords ──
    pub fn add_keywords(&mut self, ids: &[PhotoId], kws: &[String]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        for k in kws {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            tx.execute("INSERT OR IGNORE INTO keywords(name) VALUES (?1)", params![k])?;
            let kid: i64 = tx.query_row("SELECT id FROM keywords WHERE name=?1", params![k], |r| r.get(0))?;
            for id in ids {
                tx.execute("INSERT OR IGNORE INTO photo_keywords(photo_id, keyword_id) VALUES (?1, ?2)", params![id, kid])?;
            }
        }
        tx.commit()?;
        for id in ids {
            if let Some(p) = self.get_mut(*id) {
                for k in kws {
                    let k = k.trim().to_string();
                    if !k.is_empty() && !p.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
                        p.keywords.push(k);
                    }
                }
                p.keywords.sort_by_key(|s| s.to_lowercase());
            }
        }
        Ok(())
    }

    pub fn remove_keyword(&mut self, ids: &[PhotoId], k: &str) -> Result<()> {
        let kid: Option<i64> = self
            .conn
            .query_row("SELECT id FROM keywords WHERE name=?1", params![k], |r| r.get(0))
            .optional()?;
        if let Some(kid) = kid {
            let tx = self.conn.savepoint()?;
            for id in ids {
                tx.execute("DELETE FROM photo_keywords WHERE photo_id=?1 AND keyword_id=?2", params![id, kid])?;
            }
            tx.commit()?;
        }
        for id in ids {
            if let Some(p) = self.get_mut(*id) {
                p.keywords.retain(|x| !x.eq_ignore_ascii_case(k));
            }
        }
        Ok(())
    }

    /// All keywords with usage counts.
    pub fn keyword_counts(&self) -> Vec<(String, usize)> {
        let mut m: HashMap<String, (String, usize)> = HashMap::new();
        for p in &self.photos {
            for k in &p.keywords {
                let e = m.entry(k.to_lowercase()).or_insert((k.clone(), 0));
                e.1 += 1;
            }
        }
        let mut v: Vec<_> = m.into_values().collect();
        v.sort_by_key(|(k, _)| k.to_lowercase());
        v
    }

    // ── Collections ──
    pub fn create_collection(&mut self, name: &str, smart: Option<SmartRules>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO collections(name, smart) VALUES (?1, ?2)",
            params![name, smart.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default())],
        )?;
        let id = self.conn.last_insert_rowid();
        self.load_collections()?;
        Ok(id)
    }

    pub fn update_smart(&mut self, id: i64, name: &str, smart: &SmartRules) -> Result<()> {
        self.conn.execute(
            "UPDATE collections SET name=?1, smart=?2 WHERE id=?3",
            params![name, serde_json::to_string(smart)?, id],
        )?;
        self.load_collections()
    }

    pub fn rename_collection(&mut self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE collections SET name=?1 WHERE id=?2", params![name, id])?;
        self.load_collections()
    }

    pub fn delete_collection(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM collections WHERE id=?1", params![id])?;
        self.load_collections()
    }

    pub fn add_to_collection(&mut self, cid: i64, ids: &[PhotoId]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let base: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position),0) FROM collection_photos WHERE collection_id=?1",
            params![cid],
            |r| r.get(0),
        )?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO collection_photos(collection_id, photo_id, position) VALUES (?1, ?2, ?3)",
                params![cid, id, base + 1 + i as i64],
            )?;
        }
        tx.commit()?;
        self.load_collections()
    }

    pub fn remove_from_collection(&mut self, cid: i64, ids: &[PhotoId]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        for id in ids {
            tx.execute("DELETE FROM collection_photos WHERE collection_id=?1 AND photo_id=?2", params![cid, id])?;
        }
        tx.commit()?;
        self.load_collections()
    }

    pub fn toggle_quick(&mut self, ids: &[PhotoId]) -> Result<()> {
        let all_in = ids.iter().all(|i| self.quick_collection.contains(i));
        let tx = self.conn.savepoint()?;
        for id in ids {
            if all_in {
                tx.execute("DELETE FROM quick_collection WHERE photo_id=?1", params![id])?;
            } else {
                tx.execute("INSERT OR IGNORE INTO quick_collection(photo_id) VALUES (?1)", params![id])?;
            }
        }
        tx.commit()?;
        for id in ids {
            if all_in {
                self.quick_collection.remove(id);
            } else {
                self.quick_collection.insert(*id);
            }
        }
        Ok(())
    }

    // ── Virtual copies / removal ──
    pub fn create_virtual_copy(&mut self, id: PhotoId) -> Result<PhotoId> {
        let src = self.get(id).cloned().ok_or_else(|| anyhow::anyhow!("{}", trf!("사진 없음", "No photo")))?;
        let master = src.master.unwrap_or(src.id);
        let n = self.photos.iter().filter(|p| p.master == Some(master)).count() + 1;
        let copy_name = trf!("사본 {n}", "Copy {n}");
        self.conn.execute(
            "INSERT INTO photos(path, file_size, meta, rating, flag, label, master_id, copy_name, develop, import_id, imported_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                src.path.to_string_lossy(),
                src.file_size as i64,
                serde_json::to_string(&src.meta)?,
                src.rating as i64,
                src.flag.to_i(),
                src.label.to_i(),
                master,
                copy_name,
                src.develop.as_ref().map(|d| d.to_json()),
                src.import_id,
                now()
            ],
        )?;
        let nid = self.conn.last_insert_rowid();
        let mut p = src.clone();
        p.id = nid;
        p.master = Some(master);
        p.copy_name = copy_name;
        p.thumb_ver = 0;
        let kws = p.keywords.clone();
        p.keywords.clear();
        self.photos.push(p);
        self.reindex();
        let _ = self.add_keywords(&[nid], &kws);
        Ok(nid)
    }

    /// Remove from the catalog (files stay on disk).
    pub fn remove_photos(&mut self, ids: &[PhotoId]) -> Result<()> {
        let set: HashSet<PhotoId> = ids.iter().copied().collect();
        // Removing a source photo also removes its virtual copies
        let all: HashSet<PhotoId> = self
            .photos
            .iter()
            .filter(|p| set.contains(&p.id) || p.master.map(|m| set.contains(&m)).unwrap_or(false))
            .map(|p| p.id)
            .collect();
        let tx = self.conn.savepoint()?;
        for id in &all {
            tx.execute("DELETE FROM photos WHERE id=?1", params![id])?;
        }
        tx.commit()?;
        self.photos.retain(|p| !all.contains(&p.id));
        for v in self.collection_members.values_mut() {
            v.retain(|p| !all.contains(p));
        }
        self.quick_collection.retain(|p| !all.contains(p));
        self.faces.retain(|f| !all.contains(&f.photo));
        self.face_scanned.retain(|p| !all.contains(p));
        let _ = self.drop_empty_persons();
        self.reindex();
        Ok(())
    }

    // ── History / snapshots ──
    pub fn history(&self, id: PhotoId) -> Vec<HistoryEntry> {
        let Ok(mut st) = self.conn.prepare("SELECT id, label, settings, ts FROM history WHERE photo_id=?1 ORDER BY id") else {
            return Vec::new();
        };
        st.query_map(params![id], |r| {
            let s: String = r.get(2)?;
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, s, r.get::<_, i64>(3)?))
        })
        .map(|rows| {
            rows.flatten()
                .filter_map(|(hid, label, s, ts)| {
                    DevelopSettings::from_json(&s).map(|settings| HistoryEntry { id: hid, label, settings, ts })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn push_history(&mut self, id: PhotoId, label: &str, s: &DevelopSettings) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO history(photo_id, label, settings, ts) VALUES (?1, ?2, ?3, ?4)",
            params![id, label, s.to_json(), now()],
        )?;
        let hid = self.conn.last_insert_rowid();
        // Prune old entries
        self.conn.execute(
            "DELETE FROM history WHERE photo_id=?1 AND id NOT IN (SELECT id FROM history WHERE photo_id=?1 ORDER BY id DESC LIMIT ?2)",
            params![id, crate::config::MAX_HISTORY_PER_PHOTO as i64],
        )?;
        Ok(hid)
    }

    pub fn replace_last_history(&mut self, hid: i64, label: &str, s: &DevelopSettings) -> Result<()> {
        self.conn.execute(
            "UPDATE history SET label=?1, settings=?2, ts=?3 WHERE id=?4",
            params![label, s.to_json(), now(), hid],
        )?;
        Ok(())
    }

    /// Delete entries after the given one (when editing again from an earlier history step).
    pub fn truncate_history_after(&mut self, id: PhotoId, hid: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE photo_id=?1 AND id>?2", params![id, hid])?;
        Ok(())
    }

    pub fn clear_history(&mut self, id: PhotoId) -> Result<()> {
        self.conn.execute("DELETE FROM history WHERE photo_id=?1", params![id])?;
        Ok(())
    }

    pub fn snapshots(&self, id: PhotoId) -> Vec<Snapshot> {
        let Ok(mut st) = self.conn.prepare("SELECT id, name, settings FROM snapshots WHERE photo_id=?1 ORDER BY ts") else {
            return Vec::new();
        };
        st.query_map(params![id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map(|rows| {
                rows.flatten()
                    .filter_map(|(sid, name, s)| DevelopSettings::from_json(&s).map(|settings| Snapshot { id: sid, name, settings }))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn add_snapshot(&mut self, id: PhotoId, name: &str, s: &DevelopSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snapshots(photo_id, name, settings, ts) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, s.to_json(), now()],
        )?;
        Ok(())
    }

    pub fn delete_snapshot(&mut self, sid: i64) -> Result<()> {
        self.conn.execute("DELETE FROM snapshots WHERE id=?1", params![sid])?;
        Ok(())
    }

    // ── Presets (cached in memory; the list is drawn every frame, so the database is not re-read) ──
    pub fn presets(&self) -> std::sync::Arc<Vec<Preset>> {
        if let Some(c) = self.preset_cache.borrow().as_ref() {
            return c.clone();
        }
        let list: Vec<Preset> = (|| -> Option<Vec<Preset>> {
            let mut st = self.conn.prepare("SELECT id, grp, name, data FROM presets ORDER BY grp COLLATE NOCASE, name COLLATE NOCASE").ok()?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
                .ok()?;
            Some(
                rows.flatten()
                    .filter_map(|(id, group, name, data)| {
                        #[derive(Deserialize)]
                        struct D {
                            settings: DevelopSettings,
                            groups: crate::develop::settings::SettingGroups,
                        }
                        serde_json::from_str::<D>(&data).ok().map(|d| Preset { id, group, name, settings: d.settings, groups: d.groups })
                    })
                    .collect(),
            )
        })()
        .unwrap_or_default();
        let arc = std::sync::Arc::new(list);
        *self.preset_cache.borrow_mut() = Some(arc.clone());
        arc
    }

    pub fn save_preset(&mut self, group: &str, name: &str, s: &DevelopSettings, g: &crate::develop::settings::SettingGroups) -> Result<()> {
        self.save_presets(&[(group.to_string(), name.to_string(), s.clone(), g.clone())])
    }

    /// Save several presets in one transaction.
    pub fn save_presets(&mut self, items: &[(String, String, DevelopSettings, crate::develop::settings::SettingGroups)]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        {
            let mut st = tx.prepare("INSERT INTO presets(grp, name, data) VALUES (?1, ?2, ?3)")?;
            for (group, name, s, g) in items {
                let data = serde_json::json!({ "settings": s, "groups": g }).to_string();
                st.execute(params![group, name, data])?;
            }
        }
        tx.commit()?;
        *self.preset_cache.borrow_mut() = None;
        Ok(())
    }

    pub fn delete_preset(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM presets WHERE id=?1", params![id])?;
        *self.preset_cache.borrow_mut() = None;
        Ok(())
    }

    pub fn delete_preset_group(&mut self, group: &str) -> Result<()> {
        self.conn.execute("DELETE FROM presets WHERE grp=?1", params![group])?;
        *self.preset_cache.borrow_mut() = None;
        Ok(())
    }

    // ── Key-value store (export and watermark presets, UI state) ──
    /// Copy presets, preferences, watermarks and export settings to another catalog file (when creating a new catalog).
    pub fn copy_settings_to(&self, dst: &Path) -> Result<()> {
        self.conn.execute("ATTACH DATABASE ?1 AS dst", [dst.to_string_lossy().to_string()])?;
        let r = self.conn.execute_batch(
            "INSERT OR REPLACE INTO dst.kv SELECT key, value FROM main.kv
               WHERE key IN ('prefs','watermarks','export_presets','export_last','print_layout','enhance_opts');
             INSERT INTO dst.presets(grp, name, data) SELECT grp, name, data FROM main.presets;",
        );
        self.conn.execute_batch("DETACH DATABASE dst")?;
        r?;
        Ok(())
    }

    pub fn kv_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        );
    }

    pub fn kv_get_json<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.kv_get(key).and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn kv_set_json<T: Serialize>(&self, key: &str, v: &T) {
        if let Ok(s) = serde_json::to_string(v) {
            self.kv_set(key, &s);
        }
    }

    /// Photo count per folder (source photos only).
    pub fn folders(&self) -> Vec<(PathBuf, usize)> {
        let mut m: HashMap<PathBuf, usize> = HashMap::new();
        for p in &self.photos {
            *m.entry(p.folder.clone()).or_default() += 1;
        }
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by_key(|a| a.0.to_string_lossy().to_lowercase());
        v
    }

    /// Apply a file move or rename.
    #[allow(dead_code)] // for folder relinking and metadata re-read
    pub fn update_path(&mut self, id: PhotoId, new_path: &Path) -> Result<()> {
        let old = self.get(id).map(|p| p.path.clone());
        if let Some(old) = old {
            self.conn.execute(
                "UPDATE photos SET path=?1 WHERE path=?2",
                params![new_path.to_string_lossy(), old.to_string_lossy()],
            )?;
            for p in self.photos.iter_mut().filter(|p| p.path == old) {
                p.path = new_path.to_path_buf();
                p.folder = new_path.parent().map(Path::to_path_buf).unwrap_or_default();
                p.file_name = new_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            }
        }
        Ok(())
    }

    pub fn check_missing(&mut self) {
        for p in &mut self.photos {
            p.missing = !p.path.exists();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn make_photo(
    id: PhotoId,
    path: PathBuf,
    size: u64,
    meta: PhotoMeta,
    rating: u8,
    flag: Flag,
    label: ColorLabel,
    title: String,
    caption: String,
    master: Option<PhotoId>,
    copy_name: String,
    develop: Option<DevelopSettings>,
    import_id: i64,
    imported_at: i64,
    keywords: Vec<String>,
    thumb_ver: i64,
) -> Photo {
    let ext = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    Photo {
        id,
        folder: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        file_name: path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        is_raw: crate::config::is_raw_ext(&ext),
        ext,
        path,
        file_size: size,
        meta,
        rating,
        flag,
        label,
        title,
        caption,
        master,
        copy_name,
        develop,
        import_id,
        imported_at,
        keywords,
        thumb_ver,
        missing: false,
        exported_at: 0,
        edited_at: 0,
        stack_id: 0,
        stack_pos: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_crud() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        let mut c = Catalog::open(&db).unwrap();
        let imp = c.next_import_id();
        let ids = c
            .add_photos(
                &[
                    (PathBuf::from(r"C:\a\x.jpg"), 10, PhotoMeta::default(), None, vec!["여행".into()]),
                    (PathBuf::from(r"C:\a\y.cr3"), 20, PhotoMeta::default(), None, vec![]),
                ],
                imp,
            )
            .unwrap();
        c.set_rating(&ids, 4).unwrap();
        c.set_flag(&[ids[0]], Flag::Pick).unwrap();
        c.add_keywords(&[ids[1]], &["바다".into()]).unwrap();
        let mut s = DevelopSettings::default();
        s.exposure = 1.0;
        c.set_develop(ids[1], &s).unwrap();
        let col = c.create_collection("베스트", None).unwrap();
        c.add_to_collection(col, &ids).unwrap();
        let vc = c.create_virtual_copy(ids[1]).unwrap();
        c.push_history(ids[1], "노출", &s).unwrap();
        drop(c);
        let mut c = Catalog::open(&db).unwrap();
        assert_eq!(c.photos.len(), 3);
        assert_eq!(c.get(ids[0]).unwrap().rating, 4);
        assert_eq!(c.get(ids[0]).unwrap().flag, Flag::Pick);
        assert_eq!(c.get(ids[0]).unwrap().keywords, vec!["여행".to_string()]);
        assert!(c.get(ids[1]).unwrap().is_raw);
        assert_eq!(c.get(vc).unwrap().settings().exposure, 1.0);
        assert_eq!(c.collection_members[&col].len(), 2);
        assert_eq!(c.history(ids[1]).len(), 1);
        c.remove_photos(&[ids[1]]).unwrap();
        assert_eq!(c.photos.len(), 1, "가상 사본도 함께 제거");
        assert!(c.contains_path(Path::new(r"c:\A\X.JPG")));
    }

    #[test]
    fn smart_rules() {
        let mut p = make_photo(
            1,
            PathBuf::from("C:/x/a.cr3"),
            0,
            PhotoMeta { iso: Some(3200), ..Default::default() },
            3,
            Flag::Pick,
            ColorLabel::Red,
            String::new(),
            String::new(),
            None,
            String::new(),
            None,
            0,
            0,
            vec!["Paris".into()],
            0,
        );
        let r = SmartRules {
            match_all: true,
            rules: vec![SmartRule::RatingAtLeast(3), SmartRule::IsoAtLeast(1600), SmartRule::KeywordContains("par".into())],
        };
        assert!(r.matches(&p));
        p.rating = 2;
        assert!(!r.matches(&p));
    }
}

/// "YYYY-MM-DD HH:MM:SS(.fff)" -> approximate seconds (monotonic, for comparing gaps within a year)
pub fn capture_secs(t: &str) -> Option<f64> {
    let t = t.trim();
    let (d, tm) = t.split_once([' ', 'T'])?;
    let mut ds = d.split(['-', ':']).map(|x| x.parse::<f64>().ok());
    let (y, mo, da) = (ds.next()??, ds.next()??, ds.next()??);
    let mut ts = tm.split(':');
    let h: f64 = ts.next()?.parse().ok()?;
    let mi: f64 = ts.next()?.parse().ok()?;
    let se: f64 = ts.next().and_then(|x| x.trim_end_matches('Z').split('+').next().and_then(|v| v.parse().ok())).unwrap_or(0.0);
    Some((((y * 372.0 + mo * 31.0 + da) * 24.0 + h) * 60.0 + mi) * 60.0 + se)
}

#[cfg(test)]
mod stack_tests {
    use super::*;

    #[test]
    fn capture_secs_orders() {
        let a = capture_secs("2024-12-01 18:17:52").unwrap();
        let b = capture_secs("2024-12-01 18:17:55.5").unwrap();
        assert!((b - a - 3.5).abs() < 1e-6);
        assert!(capture_secs("2024-12-02 00:00:00").unwrap() > b);
    }
}
