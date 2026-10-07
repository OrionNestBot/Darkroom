//! Catalog list (current and recent) and backups.
//! A catalog is one SQLite file (.drcat) plus a "<name> Previews" cache folder next to it.
//! Original photos are not stored in the catalog, only their paths.
//! © 2026 OrionNest

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config;

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
pub struct Registry {
    /// Catalog to open on next launch (default catalog if none)
    pub current: Option<PathBuf>,
    pub recent: Vec<PathBuf>,
    /// UI language ("ko" default, "en"); applies to the whole app regardless of catalog
    #[serde(default)]
    pub lang: String,
}

fn reg_path() -> PathBuf {
    config::data_dir().join("catalogs.json")
}

impl Registry {
    pub fn load() -> Self {
        std::fs::read_to_string(reg_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config::data_dir())?;
        std::fs::write(reg_path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn touch(&mut self, p: &Path) {
        self.recent.retain(|r| r != p);
        self.recent.insert(0, p.to_path_buf());
        self.recent.truncate(config::RECENT_CATALOGS);
    }
}

/// Catalog to start with, plus a notice message if the requested one was not found
pub fn startup_path() -> (PathBuf, Option<String>) {
    let reg = Registry::load();
    match reg.current {
        Some(p) if p.exists() => (p, None),
        Some(p) => (config::default_catalog_path(), Some(trf!("카탈로그를 찾지 못해 기본 카탈로그로 열었습니다: {}", "Catalog not found — opened the default catalog instead: {}", p.display()))),
        None => (config::default_catalog_path(), None),
    }
}

pub fn is_default(p: &Path) -> bool {
    p == config::default_catalog_path()
}

pub fn display_name(p: &Path) -> String {
    if is_default(p) {
        tr!("기본 카탈로그", "Default catalog").to_string()
    } else {
        p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
}

/// Cleans out characters not allowed in names
pub fn clean_name(name: &str) -> String {
    name.trim().chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect::<String>().trim_end_matches('.').to_string()
}

/// New catalog location: <parent>\<name>\<name>.drcat
pub fn new_catalog_path(parent: &Path, name: &str) -> PathBuf {
    let n = clean_name(name);
    parent.join(&n).join(format!("{n}.{}", config::CATALOG_EXT))
}

/// Creates an empty catalog. With `carry`, copies the current catalog's presets, preferences, watermarks and export settings.
pub fn create(path: &Path, from: &crate::catalog::Catalog, carry: bool) -> Result<()> {
    if path.exists() {
        return Err(anyhow!("{}", trf!("같은 이름의 카탈로그가 이미 있습니다", "A catalog with the same name already exists")));
    }
    drop(crate::catalog::Catalog::open(path)?);
    if carry {
        from.copy_settings_to(path)?;
    }
    Ok(())
}

/// Records it as the next-launch catalog and relaunches in a new window (the caller closes the current one).
pub fn switch_to(p: &Path) -> Result<()> {
    let mut reg = Registry::load();
    reg.current = if is_default(p) { None } else { Some(p.to_path_buf()) };
    reg.touch(p);
    reg.save()?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("DARKROOM_AUTOTEST") {
            cmd.env_remove(k);
        }
    }
    cmd.spawn()?;
    Ok(())
}

/// Adds the current catalog to the recent list when it is first opened
pub fn note_opened(p: &Path) {
    let mut reg = Registry::load();
    if reg.recent.first().map(|r| r == p).unwrap_or(false) {
        return;
    }
    reg.touch(p);
    let _ = reg.save();
}

pub fn backup_dir(cat: &Path) -> PathBuf {
    cat.parent().map(|d| d.join("Backups")).unwrap_or_else(|| config::data_dir().join("Backups"))
}

/// Catalog backup to Backups\<date time>\<file name>, using SQLite VACUUM INTO, which is safe while the catalog is in use.
/// Only the newest `BACKUP_KEEP` backups are kept.
pub fn backup(cat: &Path) -> Result<PathBuf> {
    let conn = rusqlite::Connection::open_with_flags(cat, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let stamp: String = conn.query_row("SELECT strftime('%Y-%m-%d %H%M%S','now','localtime')", [], |r| r.get(0))?;
    let root = backup_dir(cat);
    let dir = root.join(&stamp);
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(cat.file_name().ok_or_else(|| anyhow!("{}", trf!("카탈로그 이름 없음", "Catalog has no name")))?);
    conn.execute("VACUUM INTO ?1", [out.to_string_lossy().to_string()])?;
    prune_backups(&root);
    Ok(out)
}

/// Deletes only "YYYY-MM-DD HHMMSS" folders made by auto backup, oldest first
fn prune_backups(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name().map(|n| {
                    let n = n.to_string_lossy();
                    n.len() == 17 && n.as_bytes()[4] == b'-' && n.as_bytes()[10] == b' '
                }) == Some(true)
        })
        .collect();
    dirs.sort();
    while dirs.len() > config::BACKUP_KEEP {
        let d = dirs.remove(0);
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Most recent backup time (Unix seconds), from folder modification time
pub fn last_backup_age_days(cat: &Path) -> Option<f64> {
    let rd = std::fs::read_dir(backup_dir(cat)).ok()?;
    let newest = rd.flatten().filter_map(|e| e.metadata().ok()?.modified().ok()).max()?;
    Some(newest.elapsed().ok()?.as_secs_f64() / 86400.0)
}

/// Size of the catalog file + cache folder
pub fn sizes(cat: &Path) -> (u64, u64) {
    let db = std::fs::metadata(cat).map(|m| m.len()).unwrap_or(0);
    let mut cache = 0u64;
    for d in [config::THUMB_DIR, config::PREVIEW_DIR] {
        if let Ok(rd) = std::fs::read_dir(config::cache_root().join(d)) {
            cache += rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>();
        }
    }
    (db, cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(clean_name(" 여행: 2026/봄 "), "여행_ 2026_봄");
        let p = new_catalog_path(Path::new("D:/Photos"), "가족");
        assert!(p.ends_with("가족/가족.drcat"));
    }

    #[test]
    fn create_carry_and_backup() {
        let td = tempfile::tempdir().unwrap();
        let src = crate::catalog::Catalog::open(&td.path().join("a.db")).unwrap();
        src.kv_set_json("prefs", &serde_json::json!({"artist": "T"}));
        src.kv_set_json("session", &serde_json::json!({"x": 1}));
        let mut src = src;
        let s = crate::develop::settings::DevelopSettings::default();
        src.save_preset("내 그룹", "따뜻하게", &s, &Default::default()).unwrap();
        let dst = new_catalog_path(td.path(), "새 카탈로그");
        create(&dst, &src, true).unwrap();
        let c = crate::catalog::Catalog::open(&dst).unwrap();
        assert_eq!(c.kv_get_json::<serde_json::Value>("prefs").unwrap()["artist"], "T");
        assert!(c.kv_get("session").is_none(), "작업 상태는 옮기지 않음");
        assert_eq!(c.presets().len(), 1);
        assert!(create(&dst, &src, false).is_err());
        // An open catalog (WAL, not yet checkpointed) must still be backed up correctly
        c.kv_set_json("prefs", &serde_json::json!({"artist": "U"}));
        let t = std::time::Instant::now();
        let b = backup(&dst).unwrap();
        assert!(t.elapsed().as_secs() < 3, "백업이 막히면 안 됨");
        drop(c);
        assert!(b.exists());
        let bc = crate::catalog::Catalog::open(&b).unwrap();
        assert_eq!(bc.presets().len(), 1);
    }
}
