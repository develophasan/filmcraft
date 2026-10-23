//! Saving and reopening CrStudio projects.
//!
//! Each project lives in its own folder under the CrStudio root (`~/Movies/CrStudio/<slug>-<date>`)
//! with everything it made (scenes, voices, sounds, subtitles) and `crstudio.json`, the pipeline
//! state. The state is saved automatically a moment after it changes; `~/.crstudio/last-project`
//! remembers which project to reopen.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::CrStudioPipeline;

pub const STATE_FILE: &str = "crstudio.json";
/// Projects larger than this are refused when reopened (a corrupt or foreign file).
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;

/// `~/.crstudio` (settings, last project, YouTube tokens).
pub fn config_dir() -> PathBuf {
    let home = crate::media_browser::std_home_dir().map(PathBuf::from).unwrap_or_else(crate::temp_dir);
    home.join(".crstudio")
}

/// Write the state to `<project_dir>/crstudio.json` (atomically).
pub fn save(p: &CrStudioPipeline) -> Result<PathBuf, String> {
    if cfg!(target_arch = "wasm32") || p.project_dir.trim().is_empty() {
        return Err("Proje klasörü yok".into());
    }
    let dir = Path::new(&p.project_dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(STATE_FILE);
    let json = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    filmcraft_format::atomic_write(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Remember `project_dir` as the project to reopen next time.
pub fn remember(project_dir: &str) {
    // unit tests must not replace the user's "last project" in ~/.crstudio
    if cfg!(test) {
        return;
    }
    let cfg = config_dir();
    let _ = std::fs::create_dir_all(&cfg);
    let _ = std::fs::write(cfg.join("last-project"), project_dir.as_bytes());
}

/// Read a project's state from its folder (or from the `crstudio.json` path itself).
pub fn load(path: &Path) -> Result<CrStudioPipeline, String> {
    let file = if path.is_dir() { path.join(STATE_FILE) } else { path.to_path_buf() };
    let meta = std::fs::metadata(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    if meta.len() > MAX_STATE_BYTES {
        return Err(format!("{}: dosya çok büyük", file.display()));
    }
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut p: CrStudioPipeline = serde_json::from_slice(&bytes).map_err(|e| format!("{}: geçersiz CrStudio projesi ({e})", file.display()))?;
    if let Some(parent) = file.parent() {
        p.project_dir = parent.to_string_lossy().into_owned();
    }
    p.sanitize();
    Ok(p)
}

/// The project opened last, if its folder still exists.
pub fn last_project() -> Option<PathBuf> {
    let s = std::fs::read_to_string(config_dir().join("last-project")).ok()?;
    let p = PathBuf::from(s.trim());
    p.join(STATE_FILE).is_file().then_some(p)
}

/// A project found under the CrStudio root.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub title: String,
    pub dir: String,
    pub format: String,
    pub stage: String,
    pub scenes: usize,
    /// Seconds since the Unix epoch of the last save.
    pub modified: u64,
}

/// Projects under `root`, newest first (at most 200).
pub fn list(root: &Path) -> Vec<ProjectEntry> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten().take(2000) {
        let dir = e.path();
        let f = dir.join(STATE_FILE);
        if !f.is_file() {
            continue;
        }
        let modified =
            std::fs::metadata(&f).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        if let Ok(p) = load(&dir) {
            out.push(ProjectEntry {
                title: if p.title.is_empty() { "Adsız proje".into() } else { p.title.clone() },
                dir: dir.to_string_lossy().into_owned(),
                format: p.format.label().into(),
                stage: p.stage.label().into(),
                scenes: p.scenes.len(),
                modified,
            });
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.modified));
    out.truncate(200);
    out
}

/// Today's date as `YYYY-MM-DD` (UTC) without a date library.
pub fn today() -> String {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// A new project folder name under `root` for `title`: `<slug>-<YYYYMMDD>[-n]`.
pub fn new_project_dir(root: &Path, title: &str) -> PathBuf {
    let base = format!("{}-{}", super::turkish::slug(title), today().replace('-', ""));
    let mut dir = root.join(&base);
    let mut n = 2;
    while dir.exists() && n < 1000 {
        dir = root.join(format!("{base}-{n}"));
        n += 1;
    }
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let root = crate::temp_dir().join("crstudio_persist_test");
        let _ = std::fs::remove_dir_all(&root);
        let dir = new_project_dir(&root, "Yedi Uyurlar");
        assert!(dir.to_string_lossy().contains("yedi-uyurlar-"));
        let p = CrStudioPipeline { title: "Yedi Uyurlar".into(), project_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
        save(&p).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back.title, "Yedi Uyurlar");
        assert_eq!(list(&root).len(), 1);
        assert!(load(&root.join("nope")).is_err());
        std::fs::write(dir.join(STATE_FILE), b"{bozuk").unwrap();
        assert!(load(&dir).is_err());
    }

    #[test]
    fn date_is_plausible() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
