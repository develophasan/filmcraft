//! Agent 4 — the Visual Hunter: openly licensed images from Wikimedia Commons and Openverse
//! (licence allow-list, author, file page and attribution sidecar for every file), procedural
//! maps and cards from Agent 5, and Claude's relevance review of the thumbnails.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::claude::{self, ClaudeRequest};
use super::director::license_allowed;
use super::{CrStudioPipeline, MapSpec, SceneSpec, VideoFormatPreset, VisualCandidate, render, turkish, with_shared, write_attribution_sidecar};

const UA: &str = "CrStudio/1.0 (personal documentary editor; https://github.com/develophasan)";

pub(super) fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn curl_json(url: &str) -> Option<Value> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let out = std::process::Command::new("curl").args(["-sS", "-L", "--max-time", "15", "-A", UA, url]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

pub(super) fn download(url: &str, dest: &Path) -> bool {
    if cfg!(target_arch = "wasm32") || url.is_empty() {
        return false;
    }
    let ok = std::process::Command::new("curl")
        .args(["-sS", "-L", "--fail", "--max-time", "30", "-A", UA, "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .is_ok_and(|s| s.success());
    ok && std::fs::metadata(dest).map(|m| m.len() > 2048).unwrap_or(false)
}

fn ext_of(url: &str) -> Option<&'static str> {
    let l = url.to_ascii_lowercase();
    let l = l.split('?').next().unwrap_or("");
    if l.ends_with(".png") {
        Some("png")
    } else if l.ends_with(".jpg") || l.ends_with(".jpeg") {
        Some("jpg")
    } else {
        None
    }
}

/// The small preview Claude looks at (`<file>.preview.<ext>`).
pub fn preview_path(local: &str) -> PathBuf {
    let p = Path::new(local);
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "jpg".into());
    p.with_extension(format!("preview.{ext}"))
}

/// Wikimedia Commons search; keeps only files whose licence is on the allow-list.
pub fn fetch_wikimedia(query: &str, dir: &Path, scene_idx: usize, limit: usize) -> Vec<VisualCandidate> {
    let q = if query.trim().is_empty() { "history".to_string() } else { query.trim().to_string() };
    let api = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&generator=search&gsrsearch={}&gsrnamespace=6&gsrlimit={}&prop=imageinfo&iiprop=url|extmetadata|size&iiurlwidth=1920&format=json",
        url_encode(&format!("{q} filetype:bitmap")),
        (limit * 3).min(30)
    );
    let Some(v) = curl_json(&api) else { return Vec::new() };
    let Some(pages) = v.pointer("/query/pages").and_then(Value::as_object) else { return Vec::new() };
    let _ = std::fs::create_dir_all(dir);
    let mut pages: Vec<&Value> = pages.values().collect();
    pages.sort_by_key(|p| p.get("index").and_then(Value::as_u64).unwrap_or(999));
    let mut out = Vec::new();
    for page in pages {
        if out.len() >= limit {
            break;
        }
        let title = page.get("title").and_then(Value::as_str).unwrap_or("").trim_start_matches("File:").to_string();
        let Some(ii) = page.get("imageinfo").and_then(Value::as_array).and_then(|a| a.first()) else { continue };
        let meta = ii.get("extmetadata");
        let field = |k: &str| meta.and_then(|m| m.get(k)).and_then(|x| x.get("value")).and_then(Value::as_str).map(strip_html_tags).unwrap_or_default();
        let license = field("LicenseShortName");
        if license.is_empty() || !license_allowed(&license) {
            continue;
        }
        let author = field("Artist");
        let url = ii.get("thumburl").and_then(Value::as_str).or_else(|| ii.get("url").and_then(Value::as_str)).unwrap_or("");
        let Some(ext) = ext_of(url) else { continue };
        let w = ii.get("thumbwidth").or_else(|| ii.get("width")).and_then(Value::as_u64).unwrap_or(0) as u32;
        let h = ii.get("thumbheight").or_else(|| ii.get("height")).and_then(Value::as_u64).unwrap_or(0) as u32;
        if w < 640 || h < 360 {
            continue;
        }
        let k = out.len() + 1;
        let dest = dir.join(format!("scene_{:02}_wikimedia_{k}.{ext}", scene_idx + 1));
        if !download(url, &dest) {
            continue;
        }
        let small = url.replace("/1920px-", "/480px-");
        let prev = preview_path(&dest.to_string_lossy());
        if small == url || !download(&small, &prev) {
            let _ = std::fs::copy(&dest, &prev);
        }
        let page_url = ii.get("descriptionurl").and_then(Value::as_str).unwrap_or("").to_string();
        write_attribution_sidecar(
            &dest,
            &title,
            if author.is_empty() { "unknown" } else { &author },
            &page_url,
            &license,
            "Wikimedia Commons, downloaded by CrStudio",
        );
        out.push(VisualCandidate {
            id: format!("wiki_{}_{k}", scene_idx + 1),
            title,
            kind: "wikimedia_commons".into(),
            local_path: dest.to_string_lossy().into_owned(),
            source_url: url.to_string(),
            page_url,
            author,
            license,
            width: w,
            height: h,
            score: 0.80,
            relevance: None,
            relevance_note: String::new(),
            notes: "Wikimedia Commons".into(),
        });
    }
    out
}

/// Openverse search (CC0, public domain mark, CC BY, CC BY-SA only).
pub fn fetch_openverse(query: &str, dir: &Path, scene_idx: usize, limit: usize) -> Vec<VisualCandidate> {
    let q = if query.trim().is_empty() { "history".to_string() } else { query.trim().to_string() };
    let api = format!("https://api.openverse.org/v1/images/?q={}&license=cc0,pdm,by,by-sa&page_size={}&mature=false", url_encode(&q), (limit * 2).min(20));
    let Some(v) = curl_json(&api) else { return Vec::new() };
    let Some(results) = v.get("results").and_then(Value::as_array) else { return Vec::new() };
    let _ = std::fs::create_dir_all(dir);
    let mut out = Vec::new();
    for r in results {
        if out.len() >= limit {
            break;
        }
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let lic_code = s("license");
        let license = match lic_code.as_str() {
            "cc0" => "CC0".to_string(),
            "pdm" => "Public domain".to_string(),
            "by" => format!("CC BY {}", s("license_version")),
            "by-sa" => format!("CC BY-SA {}", s("license_version")),
            _ => continue,
        };
        let url = s("url");
        let Some(ext) = ext_of(&url) else { continue };
        let w = r.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
        let h = r.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
        if (w > 0 && w < 640) || (h > 0 && h < 360) {
            continue;
        }
        let k = out.len() + 1;
        let dest = dir.join(format!("scene_{:02}_openverse_{k}.{ext}", scene_idx + 1));
        if !download(&url, &dest) {
            continue;
        }
        let prev = preview_path(&dest.to_string_lossy());
        if !download(&s("thumbnail"), &prev) {
            let _ = std::fs::copy(&dest, &prev);
        }
        let title = s("title");
        let author = s("creator");
        let page_url = s("foreign_landing_url");
        write_attribution_sidecar(&dest, &title, if author.is_empty() { "unknown" } else { &author }, &page_url, &license, "Openverse, downloaded by CrStudio");
        out.push(VisualCandidate {
            id: format!("ov_{}_{k}", scene_idx + 1),
            title,
            kind: "openverse".into(),
            local_path: dest.to_string_lossy().into_owned(),
            source_url: url,
            page_url,
            author,
            license,
            width: w,
            height: h,
            score: 0.70,
            relevance: None,
            relevance_note: String::new(),
            notes: "Openverse".into(),
        });
    }
    out
}

fn source_line(p: &CrStudioPipeline, scene: &SceneSpec) -> String {
    scene
        .sources
        .first()
        .and_then(|i| p.research.sources.get(*i))
        .map(|s| if s.publisher.is_empty() { s.title.clone() } else { format!("{} ({})", s.title, s.publisher) })
        .unwrap_or_default()
}

/// Find and make every candidate for one scene, then pick the best for its visual type.
pub fn hunt_scene(p: &CrStudioPipeline, scene: &mut SceneSpec) {
    let format: VideoFormatPreset = p.format;
    let (w, h) = format.dimensions();
    let dir = Path::new(&p.project_dir).join("scenes").join(format!("scene_{:02}", scene.index + 1));
    let _ = std::fs::create_dir_all(&dir);
    let mut cands: Vec<VisualCandidate> = scene.visual_candidates.iter().filter(|c| c.kind == "user").cloned().collect();

    // Web images (3 Wikimedia + 2 Openverse).
    let mut web = fetch_wikimedia(&scene.visual_query, &dir, scene.index, 3);
    if web.len() < 3 {
        let folded = turkish::ascii_fold(&scene.visual_query);
        if folded != scene.visual_query {
            web.extend(fetch_wikimedia(&folded, &dir, scene.index, 3 - web.len()));
        }
    }
    web.extend(fetch_openverse(&scene.visual_query, &dir, scene.index, 2));
    // nothing for a too specific query ("Ottoman miniature serpent garden"): ask simpler ones
    if web.is_empty() {
        let place = scene.map_spec.as_ref().map(|m| m.location_name.clone()).unwrap_or_default();
        for q in simpler_queries(&scene.visual_query, &p.title, &place) {
            web.extend(fetch_wikimedia(&q, &dir, scene.index, 3));
            if !web.is_empty() {
                scene.visual_query = q;
                break;
            }
        }
    }
    let bg = web.first().map(|c| PathBuf::from(&c.local_path));

    // Map for map scenes (and as an alternative when a place is known).
    let map_spec: Option<MapSpec> = scene.map_spec.clone().or_else(|| {
        p.research.places.first().map(|pl| MapSpec {
            location_name: pl.name.clone(),
            lat: pl.lat,
            lon: pl.lon,
            subtitle: pl.note.clone(),
            ..Default::default()
        })
    });
    if let Some(ms) = &map_spec
        && let Ok((png, real)) = render::render_map_png(w, h, ms, &scene.title)
    {
        let path = dir.join(format!("scene_{:02}_map.png", scene.index + 1));
        if filmcraft_format::atomic_write(&path, &png).is_ok() {
            write_attribution_sidecar(
                &path,
                &ms.location_name,
                "CrStudio map engine; Natural Earth data",
                "original work",
                "Public domain data / original work",
                "Map",
            );
            cands.push(VisualCandidate {
                id: format!("map_{}", scene.index + 1),
                title: format!("Harita: {}", ms.location_name),
                kind: "animated_map".into(),
                local_path: path.to_string_lossy().into_owned(),
                source_url: "original work".into(),
                author: "CrStudio".into(),
                license: if real { "Natural Earth (kamu malı) + özgün çizim".into() } else { "Özgün çizim (kendi)".into() },
                width: w,
                height: h,
                score: if scene.visual_type == "animated_map" { 0.98 } else { 0.5 },
                notes: if real { "Gerçek kıyı ve sınırlar".into() } else { "Coğrafi veri indirilemedi; şematik harita".into() },
                ..Default::default()
            });
        }
    }
    // Card (chapter / quote / chart), over a darkened photo when there is one.
    if let Ok(png) = render::render_card_png(w, h, scene, &source_line(p, scene), bg.as_deref()) {
        let path = dir.join(format!("scene_{:02}_card.png", scene.index + 1));
        if filmcraft_format::atomic_write(&path, &png).is_ok() {
            write_attribution_sidecar(&path, &scene.title, "CrStudio", "original work", "Özgün çizim (kendi)", "Graphic card");
            let fits = matches!(scene.visual_type.as_str(), "chapter_card" | "evidence_highlight" | "infographic_chart");
            cands.push(VisualCandidate {
                id: format!("card_{}", scene.index + 1),
                title: format!("Kart: {}", scene.title),
                kind: scene.visual_type.clone(),
                local_path: path.to_string_lossy().into_owned(),
                source_url: "original work".into(),
                author: "CrStudio".into(),
                license: "Özgün çizim (kendi)".into(),
                width: w,
                height: h,
                score: if fits { 0.97 } else { 0.4 },
                ..Default::default()
            });
        }
    }
    cands.extend(web);
    scene.visual_candidates = cands;
    choose_best(scene);
}

/// Words that make a picture query narrower without naming its subject.
const STYLE_WORDS: [&str; 22] = [
    "ottoman",
    "persian",
    "medieval",
    "ancient",
    "miniature",
    "painting",
    "manuscript",
    "illustration",
    "landscape",
    "light",
    "photo",
    "old",
    "historic",
    "historical",
    "traditional",
    "art",
    "detail",
    "view",
    "illuminated",
    "byzantine",
    "islamic",
    "mosaic",
];

/// Simpler picture queries to try when the script's query found nothing: the query without style
/// words, its last two words, the subject of the video (its title's first words), the place.
pub fn simpler_queries(query: &str, title: &str, place: &str) -> Vec<String> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let core: Vec<&str> = words.iter().copied().filter(|w| !STYLE_WORDS.contains(&w.to_lowercase().as_str())).collect();
    let mut out: Vec<String> = Vec::new();
    let mut push = |q: String| {
        let q = q.trim().to_string();
        if !q.is_empty() && q != query && !out.contains(&q) {
            out.push(q);
        }
    };
    push(core.join(" "));
    if core.len() > 2
        && let Some(tail) = core.get(core.len() - 2..)
    {
        push(tail.join(" "));
    }
    // the video's subject: the title before its colon ("Şahmeran: Yılanların Kraliçesi" → "Şahmeran")
    push(title.split([':', '—', '-']).next().unwrap_or("").to_string());
    push(place.split(',').next().unwrap_or("").to_string());
    out.truncate(4);
    out
}

/// Pick the selected visual: the user's choice if still present, else the highest score
/// (relevance when reviewed).
pub fn choose_best(scene: &mut SceneSpec) {
    if let Some(sel) = &scene.selected_visual
        && scene.visual_candidates.iter().any(|c| &c.local_path == sel && c.kind == "user")
    {
        return;
    }
    let value = |c: &VisualCandidate| match c.relevance {
        Some(r) => r / 100.0,
        None => c.score,
    };
    scene.visual_candidates.sort_by(|a, b| value(b).partial_cmp(&value(a)).unwrap_or(std::cmp::Ordering::Equal));
    scene.selected_visual = scene.visual_candidates.first().map(|c| c.local_path.clone());
}

/// Hunt every scene on up to four threads, reporting progress.
pub fn hunt_all(p: &CrStudioPipeline, shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>) -> Vec<SceneSpec> {
    let scenes = Arc::new(Mutex::new(p.scenes.clone()));
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let n = p.scenes.len();
    let threads = if cfg!(target_arch = "wasm32") { 1 } else { 4.min(n.max(1)) };
    let mut handles = Vec::new();
    for _ in 0..threads {
        let (scenes, next, done, cancel, shared) = (Arc::clone(&scenes), Arc::clone(&next), Arc::clone(&done), Arc::clone(cancel), Arc::clone(shared));
        let p = p.clone();
        let work = move || {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(mut sc) = scenes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(i).cloned() else { break };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hunt_scene(&p, &mut sc)));
                let count = sc.visual_candidates.len();
                let title = sc.title.clone();
                if let Some(slot) = scenes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i) {
                    *slot = sc;
                }
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                with_shared(&shared, |q| {
                    q.agent_activity(4, format!("🖼 Sahne {} · {title}: {count} aday", i + 1));
                    q.job.progress = 0.45 + 0.25 * d as f64 / n.max(1) as f64;
                });
            }
        };
        if threads == 1 {
            work();
        } else {
            handles.push(std::thread::spawn(work));
        }
    }
    for h in handles {
        let _ = h.join();
    }
    scenes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

fn relevance_prompt(batch: &[(usize, String, String, Vec<(String, String)>)]) -> String {
    let items: Vec<Value> = batch
        .iter()
        .map(|(i, narr, query, cands)| json!({"sahne": i, "anlatim": narr, "arama": query, "adaylar": cands.iter().map(|(id, path)| json!({"id": id, "dosya": path})).collect::<Vec<_>>()}))
        .collect();
    format!(
        r#"Sen CrStudio'nun görsel alaka denetçisisin. Her sahne için aday görselleri Read aracıyla aç ve sahnenin anlatımına ne kadar uyduğunu puanla.
Sahneler: {items}
Puanlama: 90-100 konuyu doğrudan gösteriyor; 60-89 ilgili ve kullanılabilir; 30-59 zayıf/genel; 0-29 alakasız, yanıltıcı ya da başka bir yer/dönem.
Tanınabilir gerçek bir kişi ya da marka logosu varsa "uyari" alanına yaz. Metin ağırlıklı, bulanık ya da çok küçük görsellerin puanını düşür.
Yalnız şu JSON'u döndür: {{"scores":[{{"id":"aday id","score":0,"note":"en fazla 12 kelime","uyari":""}}]}}"#,
        items = Value::Array(items)
    )
}

/// Claude looks at the web candidates' previews and scores their relevance; returns the updated
/// scenes (None when the review could not run).
pub fn relevance_review(p: &CrStudioPipeline, shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>) -> Option<Vec<SceneSpec>> {
    let mut scenes = p.scenes.clone();
    let work: Vec<(usize, String, String, Vec<(String, String)>)> = scenes
        .iter()
        .filter(|s| s.visual_type == "stock_photo")
        .filter_map(|s| {
            let cands: Vec<(String, String)> = s
                .visual_candidates
                .iter()
                .filter(|c| c.kind == "wikimedia_commons" || c.kind == "openverse")
                .take(3)
                .map(|c| (c.id.clone(), preview_path(&c.local_path).to_string_lossy().into_owned()))
                .filter(|(_, path)| Path::new(path).is_file())
                .collect();
            (!cands.is_empty()).then(|| (s.index, super::truncate_line(&s.narration_text, 200), s.visual_query.clone(), cands))
        })
        .collect();
    if work.is_empty() {
        return None;
    }
    let mut any = false;
    let batches: Vec<_> = work.chunks(8).collect();
    let nb = batches.len();
    for (bi, batch) in batches.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        with_shared(shared, |q| q.agent_activity(4, format!("🖼 Alaka denetimi: grup {}/{nb}", bi + 1)));
        let mut req = ClaudeRequest::new(relevance_prompt(batch), p.claude_timeout_sec.min(900));
        req.allowed_tools = vec!["Read".into()];
        req.max_turns = Some(40);
        req.cwd = Some(PathBuf::from(&p.project_dir));
        let sh = Arc::clone(shared);
        let mut cb = |ev: &claude::ClaudeEvent| {
            if let claude::ClaudeEvent::Tool { detail, .. } = ev {
                with_shared(&sh, |q| q.agent_activity(4, format!("🖼 {detail}")));
            }
        };
        let Ok(text) = claude::run(&req, cancel, &mut cb) else { continue };
        let Some(v) = claude::extract_json(&text) else { continue };
        let Some(scores) = v.get("scores").and_then(Value::as_array) else { continue };
        for s in scores {
            let (Some(id), Some(score)) = (s.get("id").and_then(Value::as_str), s.get("score").and_then(Value::as_f64)) else { continue };
            let note = s.get("note").and_then(Value::as_str).unwrap_or("");
            let warn = s.get("uyari").and_then(Value::as_str).unwrap_or("");
            for sc in &mut scenes {
                if let Some(c) = sc.visual_candidates.iter_mut().find(|c| c.id == id) {
                    c.relevance = Some(score.clamp(0.0, 100.0));
                    c.relevance_note = if warn.is_empty() { note.to_string() } else { format!("{note} ⚠ {warn}") };
                    any = true;
                }
            }
        }
    }
    if !any {
        return None;
    }
    for sc in &mut scenes {
        // Cards and maps keep their own score; a weak web image loses to the card.
        if sc.visual_type == "stock_photo" {
            for c in &mut sc.visual_candidates {
                if c.kind == "chapter_card" || c.kind == "stock_photo" {
                    c.score = 0.45;
                }
            }
        }
        choose_best(sc);
    }
    Some(scenes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_best_prefers_relevance() {
        let mut s = SceneSpec {
            visual_candidates: vec![
                VisualCandidate { id: "a".into(), local_path: "a".into(), score: 0.9, relevance: Some(20.0), ..Default::default() },
                VisualCandidate { id: "b".into(), local_path: "b".into(), score: 0.5, relevance: Some(85.0), ..Default::default() },
                VisualCandidate { id: "c".into(), local_path: "c".into(), score: 0.6, ..Default::default() },
            ],
            ..Default::default()
        };
        choose_best(&mut s);
        assert_eq!(s.selected_visual.as_deref(), Some("b"));
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_html_tags("<a href='x'>Ali  Veli</a>"), "Ali Veli");
        assert_eq!(ext_of("https://x/y/1920px-a.JPG"), Some("jpg"));
        assert_eq!(ext_of("https://x/y.svg"), None);
        assert!(preview_path("/a/b/scene.jpg").to_string_lossy().ends_with("scene.preview.jpg"));
    }
}

#[cfg(test)]
mod simpler_query_tests {
    use super::simpler_queries;

    #[test]
    fn a_narrow_query_falls_back_to_its_subject() {
        let q = simpler_queries("Ottoman miniature serpent garden", "Şahmeran: Yılanların Kraliçesi", "ŞAHMERAN HAMAMI, TARSUS");
        assert_eq!(q, ["serpent garden", "Şahmeran", "ŞAHMERAN HAMAMI"]);
        let q = simpler_queries("Çukurova plain Adana Ceyhan landscape", "Şahmeran", "");
        assert_eq!(q.first().map(String::as_str), Some("Çukurova plain Adana Ceyhan"));
        assert!(simpler_queries("", "", "").is_empty());
    }
}
