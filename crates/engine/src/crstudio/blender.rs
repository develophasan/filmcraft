//! Blender as an external, headless renderer for the channel's visual identity: map and route
//! shots over real elevation, intros and chapter openers. Blender is never linked; CrStudio runs
//! `blender -b --factory-startup --python crstudio_shot.py -- shot.json` (the script ships inside
//! CrStudio and is written to `~/.crstudio/blender/` on use) and imports the movie it writes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::persist;

const SCRIPT: &str = include_str!("../../../../tools/blender/crstudio_shot.py");
const TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// The channel's visual concepts.
pub const CONCEPTS: &[(&str, &str, &str)] = &[
    ("atlas", "Kadim Atlas", "Uydu dokulu gerçek kabartma bronz tonda, gece siyahı deniz, amber rota ve madalyonlar, kanal fontuyla başlıklar"),
    ("bakir", "Bakır gravür / altın varak", "Gerçek arazi kabartması, altın kıyı ve rota çizgileri, bronz dikilitaş, mum ışığı"),
    ("minyatur", "Minyatür / kâğıt katman", "Katmanlı kâğıt arazi, mürekkep rota, tezhip çerçeve"),
    ("diorama", "Masa üstü diorama", "Kil ve ahşap maket, alan derinliği, oyuncak ölçeği"),
];

pub fn concept_ok(c: &str) -> bool {
    CONCEPTS.iter().any(|(id, _, _)| *id == c)
}

/// Locate Blender: `CRSTUDIO_BLENDER`, the usual macOS / Linux / Windows install places, `PATH`.
pub fn find_blender() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CRSTUDIO_BLENDER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let home = crate::media_browser::std_home_dir().map(PathBuf::from).unwrap_or_default();
    let mut cands: Vec<PathBuf> = vec![
        PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender"),
        home.join("Applications/Blender.app/Contents/MacOS/Blender"),
        PathBuf::from("/usr/bin/blender"),
        PathBuf::from("/usr/local/bin/blender"),
        PathBuf::from("/opt/homebrew/bin/blender"),
        PathBuf::from("/snap/bin/blender"),
    ];
    if cfg!(windows) {
        cands.push(PathBuf::from(r"C:\Program Files\Blender Foundation\Blender\blender.exe"));
    }
    if let Some(found) = cands.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(if cfg!(windows) { "blender.exe" } else { "blender" })).find(|p| p.is_file())
}

/// `blender --version` first line, when found.
pub fn version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").stdin(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(|l| l.trim().to_string())
}

/// Folder for the channel's own fonts (never shipped in the repo; the user drops them here).
pub fn fonts_dir() -> PathBuf {
    persist::config_dir().join("fonts")
}

/// The channel display face for a concept: a file whose name contains the concept's preferred
/// style (`Sulus` for bakır, `Divani` for minyatür, `Rika` for diorama), else any font there.
pub fn channel_font(concept: &str) -> Option<PathBuf> {
    channel_font_in(&fonts_dir(), concept)
}

fn channel_font_in(dir: &Path, concept: &str) -> Option<PathBuf> {
    let mut fonts: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "otf" | "ttf")))
        .collect();
    fonts.sort();
    let want = match concept {
        "minyatur" => "divani",
        "diorama" => "rika",
        _ => "sulus",
    };
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    // only the channel's own faces count (support fonts like Barlow live in the same folder)
    fonts.retain(|p| {
        let n = name(p);
        n.contains("efsane") || n.contains("baslik") || n.contains(want)
    });
    // prefer the OTF of the wanted style, then any of that style, then the first channel font
    fonts
        .iter()
        .find(|p| name(p).contains(want) && name(p).ends_with(".otf"))
        .or_else(|| fonts.iter().find(|p| name(p).contains(want)))
        .or_else(|| fonts.first())
        .cloned()
}

/// The satellite mosaic for Kadim Atlas maps: NASA Blue Marble Next Generation (July 2004, public
/// domain), tile C1 (0–90°E, 0–90°N) cut to 0–60°E × 20–50°N at 2 km/px. Downloaded once (~77 MB)
/// and cut with macOS `sips`; returns None elsewhere or offline (maps then use the bronze relief).
pub const IMAGERY_BBOX: [f64; 4] = [0.0, 20.0, 60.0, 50.0];
pub const IMAGERY_CREDIT: &str = "Zemin: NASA Blue Marble · Yükseklik: SRTM (Mapzen) · Şematik anlatım";

pub fn atlas_imagery() -> Option<PathBuf> {
    let dir = persist::config_dir().join("data").join("bluemarble");
    let out = dir.join("atlas_0E60E_20N50N.jpg");
    if out.is_file() {
        return Some(out);
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    let tile = dir.join("C1.jpg");
    if !tile.is_file() {
        let tmp = dir.join("C1.part");
        let ok = Command::new("curl")
            .args(["-sSL", "--max-time", "900", "-o"])
            .arg(&tmp)
            .arg("https://eoimages.gsfc.nasa.gov/images/imagerecords/74000/74092/world.200407.3x21600x21600.C1.jpg")
            .stdin(Stdio::null())
            .status()
            .ok()?
            .success();
        if !ok || std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0) < 10_000_000 {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        std::fs::rename(&tmp, &tile).ok()?;
    }
    // 240 px/° in the tile: x 0..14400 (0–60°E), y 9600..16800 (50°N–20°N); halve to 7200×3600
    let crop = dir.join("crop_atlas.jpg");
    let ok = Command::new("sips")
        .args(["-c", "7200", "14400", "--cropOffset", "9600", "0"])
        .arg(&tile)
        .arg("--out")
        .arg(&crop)
        .stdout(Stdio::null())
        .status()
        .ok()?
        .success()
        && Command::new("sips").args(["-Z", "7200"]).arg(&crop).arg("--out").arg(&out).stdout(Stdio::null()).status().ok()?.success();
    let _ = std::fs::remove_file(&crop);
    (ok && out.is_file()).then_some(out)
}

/// One shot to render.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    pub concept: String,
    /// `map`, `route`, `intro`, `chapter`.
    pub shot: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: f64,
    pub title: String,
    pub subtitle: String,
    pub kicker: String,
    /// Folder written by [`super::terrain::prepare`], or empty for a procedural backdrop.
    pub terrain: String,
    pub quality: String,
    pub font: String,
    pub font_bold: String,
    /// The channel's own display face for big titles (from `~/.crstudio/fonts`), "" = `font_bold`.
    #[serde(default)]
    pub font_display: String,
    /// Places the narration names and when (map shots): the camera follows the story.
    #[serde(default)]
    pub beats: Vec<super::mapstory::Beat>,
    /// Kadim Atlas caption: the scene title, a one-line body and the phrase shown in amber.
    #[serde(default)]
    pub caption_title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub highlight: String,
    /// Source line printed small under map shots.
    #[serde(default)]
    pub credit: String,
    /// Satellite mosaic (equirectangular) and its [lon0, lat0, lon1, lat1].
    #[serde(default)]
    pub imagery: String,
    #[serde(default)]
    pub imagery_bbox: Vec<f64>,
}

impl Shot {
    pub fn sanitize(&mut self) {
        if !concept_ok(&self.concept) {
            self.concept = "bakir".into();
        }
        if !matches!(self.shot.as_str(), "map" | "route" | "intro" | "chapter") {
            self.shot = "map".into();
        }
        self.beats.retain(|b| b.t.is_finite() && b.t >= 0.0);
        self.beats.truncate(8);
        self.width = self.width.clamp(320, 3840);
        self.height = self.height.clamp(320, 3840);
        self.fps = self.fps.clamp(12, 60);
        self.seconds = if self.seconds.is_finite() { self.seconds.clamp(1.0, 90.0) } else { 6.0 };
        if self.quality != "draft" {
            self.quality = "final".into();
        }
        for s in [&mut self.title, &mut self.subtitle, &mut self.kicker] {
            *s = s.chars().filter(|c| !c.is_control()).take(120).collect();
        }
    }

    /// Fonts for the concept (system fonts; Blender's own font when missing).
    pub fn with_fonts(mut self) -> Self {
        let pick = |cands: &[&str]| cands.iter().find(|p| Path::new(p).is_file()).map(|s| (*s).to_string()).unwrap_or_default();
        let fd = fonts_dir();
        let support = |n: &str| fd.join(n).to_string_lossy().into_owned();
        let (atlas_r, atlas_b) = (support("Barlow-SemiBold.ttf"), support("BarlowCondensed-Bold.ttf"));
        let (regular, bold): (&[&str], &[&str]) = match self.concept.as_str() {
            "atlas" => (
                &[atlas_r.as_str(), "/System/Library/Fonts/Supplemental/Arial.ttf", "/usr/share/fonts/opentype/inter/Inter-Regular.otf"],
                &[atlas_b.as_str(), "/System/Library/Fonts/Supplemental/Arial Narrow Bold.ttf", "/usr/share/fonts/opentype/inter/Inter-SemiBold.otf"],
            ),
            "diorama" => (
                &["/System/Library/Fonts/Supplemental/Arial.ttf", "/usr/share/fonts/opentype/inter/Inter-Regular.otf"],
                &["/System/Library/Fonts/Supplemental/Arial Bold.ttf", "/usr/share/fonts/opentype/inter/Inter-SemiBold.otf"],
            ),
            _ => (
                &["/System/Library/Fonts/Supplemental/Georgia.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf"],
                &["/System/Library/Fonts/Supplemental/Georgia Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf"],
            ),
        };
        self.font = pick(regular);
        self.font_bold = pick(bold);
        self.font_display = channel_font(&self.concept).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        self
    }

    /// Stable short hash of everything that changes the picture (the cache key).
    pub fn key(&self) -> String {
        let s = serde_json::to_string(self).unwrap_or_default();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // the terrain folder's data and the renderer script itself count too
        let meta = std::fs::read(Path::new(&self.terrain).join("meta.json")).unwrap_or_default();
        for b in meta.iter().chain(SCRIPT.as_bytes()) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

fn script_path() -> Result<PathBuf, String> {
    let dir = persist::config_dir().join("blender");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join("crstudio_shot.py");
    if std::fs::read_to_string(&p).ok().as_deref() != Some(SCRIPT) {
        std::fs::write(&p, SCRIPT).map_err(|e| e.to_string())?;
    }
    Ok(p)
}

/// Render `shot` into `out_dir` (cached by content). `progress` gets 0–1.
pub fn render(shot: &Shot, out_dir: &Path, cancel: &Arc<AtomicBool>, progress: &mut dyn FnMut(f64, &str)) -> Result<PathBuf, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Web sürümünde yok".into());
    }
    let bin = find_blender().ok_or("Blender bulunamadı. blender.org'dan indirip Uygulamalar klasörüne kurun (ücretsiz).")?;
    let mut shot = shot.clone();
    shot.sanitize();
    let key = shot.key();
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    for ext in ["mov", "mp4"] {
        let cached = out_dir.join(format!("{}_{}_{key}.{ext}", shot.concept, shot.shot));
        if cached.is_file() && std::fs::metadata(&cached).map(|m| m.len() > 1024).unwrap_or(false) {
            progress(1.0, "önbellekten");
            return Ok(cached);
        }
    }
    let out = out_dir.join(format!("{}_{}_{key}.mov", shot.concept, shot.shot));
    let mut params = serde_json::to_value(&shot).map_err(|e| e.to_string())?;
    params["out"] = json!(out.to_string_lossy());
    let pj = out_dir.join(format!("{key}.json"));
    std::fs::write(&pj, serde_json::to_vec_pretty(&params).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let script = script_path()?;
    let mut child = Command::new(&bin)
        .args(["-b", "--factory-startup", "--python"])
        .arg(&script)
        .arg("--")
        .arg(&pj)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Blender çalıştırılamadı: {e}"))?;
    let stdout = child.stdout.take().ok_or("Blender çıktısı okunamadı")?;
    // stderr is drained on its own thread so the pipe never fills up
    let stderr = child.stderr.take();
    let err_tail = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let et = Arc::clone(&err_tail);
    let err_thread = std::thread::spawn(move || {
        if let Some(e) = stderr {
            for line in BufReader::new(e).lines().map_while(Result::ok) {
                let mut v = et.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                v.push(line);
                if v.len() > 40 {
                    v.remove(0);
                }
            }
        }
    });
    let total = (shot.seconds * f64::from(shot.fps)).round().max(1.0);
    let start = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut last_lines: Vec<String> = Vec::new();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("iptal edildi".into());
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Blender zaman aşımına uğradı".into());
        }
        while let Ok(line) = rx.try_recv() {
            if let Some(f) = frame_of(&line) {
                progress((f / total).clamp(0.0, 0.99), &format!("kare {f:.0}/{total:.0}"));
            }
            last_lines.push(line);
            if last_lines.len() > 40 {
                last_lines.remove(0);
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let _ = reader.join();
    let _ = err_thread.join();
    let produced = [out.clone(), out.with_extension("mp4")].into_iter().find(|p| p.is_file() && std::fs::metadata(p).map(|m| m.len() > 1024).unwrap_or(false));
    match produced {
        Some(p) if status.success() => {
            progress(1.0, "bitti");
            Ok(p)
        }
        _ => {
            let errs = err_tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            let tail: Vec<String> = last_lines
                .iter()
                .chain(errs.iter())
                .filter(|l| l.contains("Error") || l.contains("error") || l.contains("Traceback") || l.starts_with("  "))
                .take(12)
                .cloned()
                .collect();
            let log = out_dir.join(format!("{key}.log"));
            let _ = std::fs::write(&log, last_lines.iter().chain(errs.iter()).cloned().collect::<Vec<_>>().join("\n"));
            Err(format!(
                "Blender çekimi üretemedi ({}). Ayrıntı: {}{}",
                status,
                log.display(),
                if tail.is_empty() { String::new() } else { format!("\n{}", tail.join("\n")) }
            ))
        }
    }
}

/// `Fra:12 Mem:…` → 12.
fn frame_of(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix("Fra:")?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<f64>().ok()
}

pub fn status() -> Value {
    match find_blender() {
        Some(p) => json!({"found": true, "path": p.to_string_lossy(), "version": version(&p).unwrap_or_default(),
                          "channelFont": channel_font("bakir").map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                          "fontsDir": fonts_dir().to_string_lossy(),
                          "concepts": CONCEPTS.iter().map(|(id, l, d)| json!({"id": id, "label": l, "about": d})).collect::<Vec<_>>()}),
        None => json!({"found": false, "concepts": CONCEPTS.iter().map(|(id, l, d)| json!({"id": id, "label": l, "about": d})).collect::<Vec<_>>(),
                       "hint": "Blender'ı blender.org'dan indirip Uygulamalar klasörüne kurun"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_font_follows_the_concept() {
        let dir = std::env::temp_dir().join(format!("crs-fonts-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(channel_font_in(&dir, "bakir"), None);
        for n in ["Barlow-SemiBold.ttf", "EfsanelerBaslik-Divani.otf", "EfsanelerBaslik-Sulus.ttf", "EfsanelerBaslik-Sulus.otf", "notes.txt"] {
            let _ = std::fs::write(dir.join(n), b"x");
        }
        let name = |c: &str| channel_font_in(&dir, c).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        assert_eq!(name("bakir").as_deref(), Some("EfsanelerBaslik-Sulus.otf"));
        assert_eq!(name("minyatur").as_deref(), Some("EfsanelerBaslik-Divani.otf"));
        // no Rik'a: falls back to the first font
        assert_eq!(name("diorama").as_deref(), Some("EfsanelerBaslik-Divani.otf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shot_is_sanitized_and_keyed() {
        let mut s = Shot { concept: "x".into(), shot: "??".into(), width: 99_999, fps: 0, seconds: f64::NAN, title: "a\u{7}b".into(), ..Default::default() };
        s.sanitize();
        assert_eq!(s.concept, "bakir");
        assert_eq!(s.shot, "map");
        assert_eq!(s.width, 3840);
        assert_eq!(s.fps, 12);
        assert_eq!(s.seconds, 6.0);
        assert_eq!(s.title, "ab");
        let k1 = s.key();
        s.title = "c".into();
        assert_ne!(k1, s.key());
        assert_eq!(frame_of("Fra:42 Mem:12M"), Some(42.0));
        assert_eq!(frame_of("Saved: x"), None);
        assert!(SCRIPT.contains("def shot_map"));
    }
}
