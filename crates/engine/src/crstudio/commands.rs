//! The `crstudio.*` commands (UI, MCP and the control channel all run these).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::{
    AgentStatus, CrStudioPipeline, DocTone, MapSpec, PipelineStage, VideoFormatPreset, VisualCandidate, agents, assemble, director, persist, render, spawn_job,
    turkish, visuals, voice, with_shared, write_attribution_sidecar, youtube,
};
use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, journal: bool, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

fn other(e: impl Into<String>) -> EngineError {
    EngineError::Other(e.into())
}

fn finite(p: &Value, k: &str) -> Option<f64> {
    f64_p(p, k).filter(|v| v.is_finite())
}

fn scene_index(s: &Session, p: &Value, cmd: &str) -> Result<usize> {
    let idx = u64_p(p, "sceneIndex").map(|v| v as usize).unwrap_or(s.crstudio.pipeline.active_scene_index);
    if idx >= s.crstudio.pipeline.scenes.len() {
        return Err(bad(cmd, format!("sahne {} yok", idx + 1)));
    }
    Ok(idx)
}

fn guard_idle(s: &Session, cmd: &str) -> Result<()> {
    if s.crstudio.worker_running {
        return Err(bad(cmd, "bir iş sürüyor; bitmesini bekleyin ya da iptal edin"));
    }
    Ok(())
}

fn state(s: &Session) -> Value {
    serde_json::to_value(&s.crstudio.pipeline).unwrap_or_default()
}

/// Apply the project-setup parameters shared by `new`/`init`/`runPipeline`.
fn apply_setup(p: &mut CrStudioPipeline, v: &Value) {
    if let Some(f) = str_p(v, "format").and_then(VideoFormatPreset::from_id) {
        p.format = f;
        if finite(v, "targetDurationSec").is_none() {
            p.target_duration_sec = f.default_duration_sec();
        }
    }
    if let Some(t) = str_p(v, "title") {
        p.title = t.trim().chars().take(200).collect();
    }
    if let Some(t) = str_p(v, "tone").and_then(DocTone::from_id) {
        p.tone = t;
        p.bgm_style = t.default_bgm().into();
    }
    if let Some(d) = finite(v, "targetDurationSec") {
        p.target_duration_sec = d.clamp(15.0, p.format.max_duration_sec());
    }
    if let Some(e) = str_p(v, "extraInstructions") {
        p.extra_instructions = e.chars().take(4000).collect();
    }
    if let Some(c) = str_p(v, "channelName") {
        p.channel_name = c.trim().chars().take(60).collect();
    }
    if let Some(r) = str_p(v, "projectsRoot").filter(|r| !r.trim().is_empty()) {
        p.projects_root = r.to_string();
    }
}

fn new_project(s: &mut Session, v: &Value) -> Result<Value> {
    guard_idle(s, "crstudio.new")?;
    let keep = &s.crstudio.pipeline;
    let mut p = CrStudioPipeline {
        projects_root: keep.projects_root.clone(),
        channel_name: keep.channel_name.clone(),
        brand_color: keep.brand_color.clone(),
        voice_engine: keep.voice_engine.clone(),
        voice_reference: keep.voice_reference.clone(),
        claude_timeout_sec: keep.claude_timeout_sec,
        relevance_check: keep.relevance_check,
        director_ai: keep.director_ai,
        ..Default::default()
    };
    apply_setup(&mut p, v);
    if p.title.trim().is_empty() {
        return Err(bad("crstudio.new", "konu/başlık gerekli"));
    }
    if let Some(dir) = str_p(v, "projectDir").filter(|d| !d.trim().is_empty()) {
        p.project_dir = dir.to_string();
    } else {
        p.project_dir = persist::new_project_dir(Path::new(&p.projects_root), &p.title).to_string_lossy().into_owned();
    }
    if !cfg!(target_arch = "wasm32") {
        std::fs::create_dir_all(&p.project_dir).map_err(|e| other(format!("{}: {e}", p.project_dir)))?;
    }
    // real, licensed music whenever the channel has a library (never the synthesized score by default)
    if super::audio::has_music_library(&p.project_dir) {
        p.bgm_style = "files".into();
    }
    p.push_log("CrStudio", "info", format!("Yeni proje: \"{}\" ({}, {:.0} sn)", p.title, p.format.label(), p.target_duration_sec));
    s.crstudio.pipeline = p;
    s.crstudio.shared_worker = None;
    s.crstudio.worker_running = false;
    s.crstudio.last_saved.clear();
    s.crstudio.last_save_check = 0.0;
    super::autosave(s);
    Ok(state(s))
}

fn open_project(s: &mut Session, dir: &Path) -> Result<Value> {
    guard_idle(s, "crstudio.open")?;
    let p = persist::load(dir).map_err(other)?;
    s.crstudio.last_saved = serde_json::to_string(&p).unwrap_or_default();
    s.crstudio.pipeline = p;
    persist::remember(&s.crstudio.pipeline.project_dir);
    // reopen the editing project too, when there is one
    let slug = turkish::slug(&s.crstudio.pipeline.title);
    let fc = Path::new(&s.crstudio.pipeline.project_dir).join(format!("{slug}.fcproj"));
    if fc.is_file() {
        let _ = s.execute("file.open", json!({"path": fc.to_string_lossy()}));
    }
    Ok(state(s))
}

fn start_preproduction(s: &mut Session, v: &Value) -> Result<Value> {
    guard_idle(s, "crstudio.runPipeline")?;
    if str_p(v, "title").is_some() && str_p(v, "title") != Some(s.crstudio.pipeline.title.as_str()) {
        new_project(s, v)?;
    } else {
        apply_setup(&mut s.crstudio.pipeline, v);
    }
    if s.crstudio.pipeline.title.trim().is_empty() {
        return Err(bad("crstudio.runPipeline", "önce konu/başlık girin"));
    }
    if s.crstudio.pipeline.project_dir.is_empty() {
        let root = s.crstudio.pipeline.projects_root.clone();
        s.crstudio.pipeline.project_dir = persist::new_project_dir(Path::new(&root), &s.crstudio.pipeline.title).to_string_lossy().into_owned();
        let _ = std::fs::create_dir_all(&s.crstudio.pipeline.project_dir);
    }
    s.crstudio.pipeline.stage = PipelineStage::RunningAgents;
    let sync = bool_p(v, "sync").unwrap_or(cfg!(target_arch = "wasm32"));
    spawn_job(s, "Ön hazırlık", agents::run_preproduction).map_err(other)?;
    if sync {
        wait_job(s);
    }
    Ok(state(s))
}

/// Block until the current job ends (MCP/tests with `sync: true`).
fn wait_job(s: &mut Session) {
    for _ in 0..(3600 * 20) {
        super::poll(s);
        if !s.crstudio.worker_running {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The first sentence of a narration (caption body), at most ~110 characters.
fn first_sentence(text: &str) -> String {
    let t = text.trim();
    let chars: Vec<(usize, char)> = t.char_indices().collect();
    // a sentence ends at . ! ? — not inside "...", and not after an ordinal ("5. ve 6. yüzyıl")
    let is_end = |k: usize| -> bool {
        let Some(&(i, c)) = chars.get(k) else { return false };
        if !matches!(c, '.' | '!' | '?') || i <= 20 {
            return false;
        }
        let next = chars.get(k + 1).map(|x| x.1);
        if c == '.' && (next == Some('.') || k.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|x| x.1 == '.')) {
            return false;
        }
        let prev_digit = k.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|x| x.1.is_ascii_digit());
        let next_word_lower = chars.iter().skip(k + 1).map(|x| x.1).find(|ch| !ch.is_whitespace()).is_some_and(char::is_lowercase);
        !(c == '.' && prev_digit && next_word_lower)
    };
    let end = (0..chars.len()).find(|&k| is_end(k)).and_then(|k| chars.get(k)).map(|&(i, c)| i + c.len_utf8()).unwrap_or(t.len());
    let s = t.get(..end).unwrap_or(t).trim();
    if s.chars().count() > 110 {
        let cut: String = s.chars().take(108).collect();
        format!("{}…", cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut))
    } else {
        s.to_string()
    }
}

/// Render a Blender shot for a scene and offer it as the scene's picture.
fn blender_job(
    shared: &std::sync::Arc<std::sync::Mutex<CrStudioPipeline>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    idx: usize,
    kind: String,
    quality: String,
) {
    let snap = with_shared(shared, |p| p.clone());
    let Some(sc) = snap.scenes.get(idx).cloned() else { return };
    let (w, h) = snap.format.dimensions();
    let dir = Path::new(&snap.project_dir).join("blender");
    let report = |msg: String, err: bool| {
        with_shared(shared, |p| {
            p.push_log("Harita & Grafik", if err { "error" } else { "ok" }, msg.clone());
            if err {
                p.job.error = msg;
            }
        })
    };
    let chapter_no = {
        let mut seen: Vec<&str> = Vec::new();
        for s in &snap.scenes {
            if !seen.contains(&s.chapter.as_str()) {
                seen.push(&s.chapter);
            }
            if s.index == idx {
                break;
            }
        }
        seen.len()
    };
    let mut shot = super::blender::Shot {
        concept: snap.visual_concept.clone(),
        shot: kind.clone(),
        width: w,
        height: h,
        fps: 30,
        // the whole scene plus a handle for the outgoing transition (a shorter shot would hold its last frame)
        seconds: sc.effective_duration().clamp(3.0, 60.0) + 0.8,
        quality,
        ..Default::default()
    };
    // Kadim Atlas title cards drift over the story's real land (the video's first map place)
    if snap.visual_concept == "atlas" && matches!(kind.as_str(), "intro" | "chapter") {
        let home = sc.map_spec.clone().or_else(|| snap.scenes.iter().find_map(|s| s.map_spec.clone()));
        if let Some(mut home) = home {
            home.extra_pins.clear();
            home.from_lat = None;
            home.from_lon = None;
            home.zoom = home.zoom.max(5.5);
            let grid = if shot.quality == "draft" { 360 } else { 640 };
            if let Ok(t) = super::terrain::prepare(&home, f64::from(w) / f64::from(h.max(1)), grid, &dir.join("arazi_kart")) {
                shot.terrain = t.dir.to_string_lossy().into_owned();
            }
            if let Some(img) = super::blender::atlas_imagery() {
                shot.imagery = img.to_string_lossy().into_owned();
                shot.imagery_bbox = super::blender::IMAGERY_BBOX.to_vec();
            }
        }
    }
    match kind.as_str() {
        "intro" => {
            shot.kicker = snap.channel_name.clone();
            shot.title = snap.title.clone();
            shot.subtitle = snap.tone.label().to_string();
            shot.seconds = 6.0;
        }
        "chapter" => {
            shot.kicker = format!("BÖLÜM {}", turkish::upper(&super::turkish::number_words(chapter_no as u64)));
            shot.title = if sc.chapter.is_empty() { sc.title.clone() } else { sc.chapter.clone() };
            shot.subtitle = sc.overlay_subtitle.clone();
        }
        _ => {
            let spec = sc.map_spec.clone().or_else(|| {
                snap.research.places.first().map(|pl| MapSpec {
                    location_name: pl.name.clone(),
                    lat: pl.lat,
                    lon: pl.lon,
                    subtitle: pl.note.clone(),
                    zoom: 7.0,
                    ..Default::default()
                })
            });
            let Some(spec) = spec else {
                report(format!("Sahne {}: harita için yer bilgisi yok (önce haritayı ayarlayın)", idx + 1), true);
                return;
            };
            // follow the story: the places the narration names, in order and on time
            let (spec, beats) = if spec.from_lat.is_some() { (spec, Vec::new()) } else { super::mapstory::plan(&snap.scenes, idx, &spec) };
            if spec.from_lat.is_some() {
                shot.shot = "route".into();
            }
            shot.beats = beats;
            shot.title = spec.location_name.clone();
            shot.subtitle = spec.subtitle.clone();
            // Kadim Atlas caption: chapter tag, scene title, one line with the highlight in amber
            shot.kicker = turkish::upper(&sc.chapter);
            shot.caption_title = if sc.title.trim().is_empty() { sc.overlay_title.clone() } else { sc.title.clone() };
            shot.body = if sc.overlay_subtitle.chars().count() >= 30 { sc.overlay_subtitle.clone() } else { first_sentence(&sc.narration_text) };
            // the amber phrase must be short and actually in the line
            shot.highlight = if sc.highlight_phrase.chars().count() <= 40 { sc.highlight_phrase.clone() } else { String::new() };
            shot.credit = super::blender::IMAGERY_CREDIT.to_string();
            if shot.concept == "atlas" {
                with_shared(shared, |p| p.job.label = "Uydu dokusu hazırlanıyor".into());
                if let Some(img) = super::blender::atlas_imagery() {
                    shot.imagery = img.to_string_lossy().into_owned();
                    shot.imagery_bbox = super::blender::IMAGERY_BBOX.to_vec();
                }
            }
            with_shared(shared, |p| p.job.label = "Yükseklik verisi hazırlanıyor".into());
            let grid = if shot.quality == "draft" { 360 } else { 640 };
            match super::terrain::prepare(&spec, f64::from(w) / f64::from(h.max(1)), grid, &dir.join(format!("arazi_{:02}", idx + 1))) {
                Ok(t) => shot.terrain = t.dir.to_string_lossy().into_owned(),
                Err(e) => {
                    report(format!("Sahne {}: {e}; temsilî arazi kullanılıyor", idx + 1), false);
                }
            }
        }
    }
    let shot = shot.with_fonts();
    let label = format!("Blender · sahne {} · {}", idx + 1, shot.shot);
    with_shared(shared, |p| {
        p.job.label = label.clone();
        p.job.progress = 0.02;
    });
    let res = super::blender::render(&shot, &dir, cancel, &mut |f, note| {
        with_shared(shared, |p| {
            p.job.progress = f;
            p.job.label = format!("{label} · {note}");
        })
    });
    match res {
        Ok(path) => {
            let ps = path.to_string_lossy().into_owned();
            let license = if shot.terrain.is_empty() {
                "Özgün 3B çizim (Blender)".to_string()
            } else {
                format!("Özgün 3B çizim (Blender) · {}", super::terrain::LICENSE)
                    + if shot.imagery.is_empty() { "" } else { " · Uydu: NASA Blue Marble (kamu malı)" }
            };
            write_attribution_sidecar(&path, &shot.title, "CrStudio + Blender", "original work", &license, "Rendered shot");
            with_shared(shared, |p| {
                if let Some(t) = p.scenes.get_mut(idx) {
                    t.visual_candidates.retain(|c| c.local_path != ps);
                    t.visual_candidates.insert(
                        0,
                        VisualCandidate {
                            // a draft is told apart so the production line still asks for the final
                            id: if shot.quality == "draft" {
                                format!("blender_{}_{}_draft", idx + 1, shot.shot)
                            } else {
                                format!("blender_{}_{}", idx + 1, shot.shot)
                            },
                            title: format!("Blender: {} ({})", shot.title, shot.shot),
                            kind: "blender".into(),
                            local_path: ps.clone(),
                            source_url: "original work".into(),
                            author: "CrStudio".into(),
                            license,
                            width: shot.width,
                            height: shot.height,
                            score: 0.99,
                            relevance: Some(100.0),
                            ..Default::default()
                        },
                    );
                    t.selected_visual = Some(ps.clone());
                }
                p.push_log("Harita & Grafik", "ok", format!("Sahne {} · Blender çekimi hazır ({})", idx + 1, shot.shot));
            });
        }
        Err(e) => report(format!("Sahne {}: {e}", idx + 1), true),
    }
}

fn synth_job(
    shared: &std::sync::Arc<std::sync::Mutex<CrStudioPipeline>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    only: Option<usize>,
    force: bool,
) {
    let snap = with_shared(shared, |p| p.clone());
    let engine = if snap.voice_engine.is_empty() { voice::default_engine().unwrap_or_default() } else { snap.voice_engine.clone() };
    if engine.is_empty() {
        with_shared(shared, |p| {
            p.job.error = "Kurulu Türkçe ses motoru yok (tools/tts/install.sh) — sahneleri kendi sesinizle kaydedebilirsiniz".into();
            p.push_log("Seslendirme", "error", p.job.error.clone());
        });
        return;
    }
    let opts = voice::VoiceOpts { reference: snap.voice_reference.clone(), expressiveness: 0.55 };
    let todo: Vec<usize> = snap
        .scenes
        .iter()
        .filter(|sc| only.is_none_or(|i| i == sc.index))
        .filter(|sc| force || only.is_some() || sc.voiceover_path.is_none() || sc.voiceover_stale)
        .filter(|sc| sc.voiceover_engine != "record" || force || only.is_some())
        .map(|sc| sc.index)
        .collect();
    let n = todo.len();
    // names written mid-sentence anywhere in the script are names everywhere (listening check)
    let names = super::voicecheck::proper_names(snap.scenes.iter().map(|s| s.narration_text.as_str()));
    for (k, i) in todo.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some(sc) = snap.scenes.get(i) else { continue };
        with_shared(shared, |p| {
            p.job.label = format!("Seslendirme {}/{n}", k + 1);
            p.job.progress = k as f64 / n.max(1) as f64;
        });
        let out = Path::new(&snap.project_dir).join("voiceovers").join(format!("scene_{:02}_ai.wav", i + 1));
        // the directed version (with v3 audio tags) when there is one
        let text = if sc.voice_text.trim().is_empty() { &sc.narration_text } else { &sc.voice_text };
        let mut res = voice::synthesize(&engine, text, &out, &opts);
        // listening check: transcribe the take and compare it with the script; a misread ordinary
        // word gets the scene read again (up to three takes, the best one is kept)
        let mut check: Option<Vec<String>> = None;
        if engine == "elevenlabs" && res.is_ok() {
            let best_path = out.with_extension("best.wav");
            let mut best: Option<(usize, voice::VoiceResult, Vec<String>)> = None;
            for take in 0..3 {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(cur) = res.clone() else { break };
                let heard = match super::elevenlabs::transcribe(&out) {
                    Ok(h) => h,
                    Err(e) => {
                        with_shared(shared, |p| p.push_log("Seslendirme", "warn", format!("Sahne {}: dinleme denetimi yapılamadı: {e}", i + 1)));
                        break;
                    }
                };
                let misses = super::voicecheck::compare_with_names(text, &heard, &names);
                let score = super::voicecheck::score(&misses);
                let labels: Vec<String> = misses.iter().map(super::voicecheck::Miss::label).take(12).collect();
                let better = best.as_ref().is_none_or(|(b, _, _)| score < *b);
                if better {
                    let _ = std::fs::copy(&out, &best_path);
                    best = Some((score, cur, labels));
                }
                if !misses.iter().any(|m| m.common) || take == 2 {
                    break;
                }
                let bad: Vec<String> = misses.iter().filter(|m| m.common).map(super::voicecheck::Miss::label).take(3).collect();
                with_shared(shared, |p| {
                    p.job.label = format!("Seslendirme {}/{n} · yeniden okutuluyor", k + 1);
                    p.push_log("Seslendirme", "warn", format!("Sahne {}: yanlış okundu ({}) — tekrar", i + 1, bad.join(", ")));
                });
                res = voice::synthesize(&engine, text, &out, &opts);
                if res.is_err() {
                    break;
                }
            }
            if let Some((_, r, labels)) = best {
                if best_path.is_file() {
                    let _ = std::fs::rename(&best_path, &out);
                }
                res = Ok(r);
                check = Some(labels);
            }
            let _ = std::fs::remove_file(&best_path);
        }
        with_shared(shared, |p| match res {
            Ok(r) => {
                if let Some(t) = p.scenes.get_mut(i) {
                    let raw = out.to_string_lossy().into_owned();
                    t.voiceover_raw_path = Some(raw.clone());
                    t.voiceover_path = Some(raw);
                    t.voiceover_duration_sec = Some(r.seconds);
                    t.voiceover_source = format!("Taslak AI ses · {}", r.label);
                    t.voiceover_engine = engine.clone();
                    t.voiceover_stale = false;
                    let (sp, times) = r.alignment.clone().unwrap_or_default();
                    t.voice_spoken = sp;
                    t.voice_char_times = times;
                    t.voice_checked = check.is_some();
                    t.voice_check = check.clone().unwrap_or_default();
                }
                write_attribution_sidecar(&out, "Narration draft", &r.label, "generated", &r.license, "AI draft narration");
                let note = match &check {
                    Some(c) if c.is_empty() => " · dinleme denetimi temiz".to_string(),
                    Some(c) => format!(" · dinleme denetimi: {}", c.join("; ")),
                    None => String::new(),
                };
                p.push_log("Seslendirme", "ok", format!("Sahne {} · {:.1} sn{note}", i + 1, r.seconds));
            }
            Err(e) => {
                p.push_log("Seslendirme", "error", format!("Sahne {}: {e}", i + 1));
                p.job.error = e;
            }
        });
    }
    with_shared(shared, |p| {
        let a = super::audio::design(p);
        a.apply(p);
        p.director = Some(director::review(p));
    });
}

fn update_scene(s: &mut Session, v: &Value) -> Result<Value> {
    guard_idle(s, "crstudio.updateScene")?;
    let idx = scene_index(s, v, "crstudio.updateScene")?;
    let nsrc = s.crstudio.pipeline.research.sources.len();
    let Some(sc) = s.crstudio.pipeline.scenes.get_mut(idx) else { return Err(bad("crstudio.updateScene", "sahne yok")) };
    let text = |k: &str, max: usize| str_p(v, k).map(|x| x.chars().take(max).collect::<String>());
    if let Some(t) = text("title", 200) {
        sc.title = t;
    }
    if let Some(t) = text("chapter", 120) {
        sc.chapter = t;
    }
    if let Some(t) = text("voiceText", 4000) {
        if t != sc.voice_text {
            sc.voiceover_stale = sc.voiceover_path.is_some();
        }
        sc.voice_text = t;
    }
    if let Some(t) = text("narrationText", 4000)
        && t != sc.narration_text
    {
        sc.narration_text = t;
        sc.text_origin = "user".into();
        if sc.voiceover_path.is_some() {
            if sc.voiceover_engine == "record" || sc.voiceover_engine == "file" {
                sc.voiceover_stale = true;
            } else {
                sc.voiceover_path = None;
                sc.voiceover_raw_path = None;
                sc.voiceover_duration_sec = None;
                sc.voiceover_source.clear();
            }
        }
        sc.duration_seconds = (sc.estimated_seconds() * 10.0).round() / 10.0;
    }
    if let Some(d) = finite(v, "durationSeconds") {
        sc.duration_seconds = d.clamp(1.0, 300.0);
    }
    if let Some(t) = str_p(v, "visualType").filter(|t| ["stock_photo", "animated_map", "evidence_highlight", "infographic_chart", "chapter_card"].contains(t)) {
        sc.visual_type = t.to_string();
    }
    if let Some(t) = text("visualQuery", 300) {
        sc.visual_query = t;
    }
    if let Some(t) = str_p(v, "cameraMotion").filter(|t| ["zoom_in", "zoom_out", "pan_right", "pan_left", "dramatic_push"].contains(t)) {
        sc.camera_motion = t.to_string();
    }
    if let Some(t) = str_p(v, "selectedVisual") {
        if !sc.visual_candidates.iter().any(|c| c.local_path == t) {
            return Err(bad("crstudio.updateScene", "seçilen görsel bu sahnenin adaylarından biri değil"));
        }
        sc.selected_visual = Some(t.to_string());
    }
    for (k, slot) in [
        ("overlayTemplate", &mut sc.overlay_template),
        ("overlayTitle", &mut sc.overlay_title),
        ("overlaySubtitle", &mut sc.overlay_subtitle),
        ("infoText", &mut sc.info_text),
        ("highlightPhrase", &mut sc.highlight_phrase),
    ] {
        if let Some(t) = str_p(v, k) {
            *slot = t.chars().take(300).collect();
        }
    }
    if let Some(t) = str_p(v, "sfxCue").filter(|t| ["whoosh", "low_boom", "riser", "map_ping", "archive_shutter", "none"].contains(t)) {
        sc.sfx_cue = t.to_string();
    }
    for (k, slot) in [("videoTransition", &mut sc.video_transition), ("visualEffect", &mut sc.visual_effect), ("filmTexture", &mut sc.film_texture)] {
        if let Some(t) = str_p(v, k) {
            *slot = t.chars().take(40).collect();
        }
    }
    if let Some(arr) = v.get("overlays").and_then(Value::as_array) {
        sc.overlays = arr.iter().filter_map(Value::as_str).filter(|o| agents::OVERLAYS.contains(o)).take(4).map(str::to_string).collect();
    }
    if let Some(arr) = v.get("sources").and_then(Value::as_array) {
        sc.sources = arr.iter().filter_map(Value::as_u64).map(|x| x as usize).filter(|x| *x < nsrc).collect();
    }
    if let Some(n) = u64_p(v, "brollSubcuts") {
        sc.broll_subcuts = (n as usize).clamp(1, 3);
    }
    if let Some(b) = bool_p(v, "approved") {
        sc.approved = b;
    }
    if let Some(m) = v.get("mapSpec")
        && let Ok(mut ms) = serde_json::from_value::<MapSpec>(m.clone())
    {
        ms.sanitize();
        sc.map_spec = Some(ms);
    }
    let out = serde_json::to_value(&*sc).unwrap_or_default();
    s.crstudio.pipeline.active_scene_index = idx;
    Ok(out)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("crstudio.presets", "CrStudio: formatlar ve tonlar", "{}", false, |_, _| {
            Ok(json!({
                "formats": VideoFormatPreset::ALL.iter().map(|f| json!({"id": f.id(), "label": f.label(), "aspect": f.aspect_label(), "defaultDurationSec": f.default_duration_sec(), "maxDurationSec": f.max_duration_sec()})).collect::<Vec<_>>(),
                "tones": DocTone::ALL.iter().map(|t| json!({"id": t.id(), "label": t.label(), "description": t.description()})).collect::<Vec<_>>(),
                "overlays": agents::OVERLAYS,
            }))
        }),
        spec("crstudio.status", "CrStudio: durum", "{}", false, |s, _| {
            super::poll(s);
            Ok(state(s))
        }),
        spec("crstudio.new", "CrStudio: yeni proje", r#"{"title":str,"format":str?,"tone":str?,"targetDurationSec":f64?,"extraInstructions":str?,"channelName":str?,"projectDir":str?}"#, true, new_project),
        spec("crstudio.init", "CrStudio: yeni proje (eski ad)", r#"{"title":str,"format":str?,"tone":str?,"targetDurationSec":f64?,"projectDir":str?}"#, true, new_project),
        spec("crstudio.open", "CrStudio: projeyi aç", r#"{"dir":str}"#, true, |s, v| {
            let dir = str_p(v, "dir").ok_or_else(|| bad("crstudio.open", "`dir` gerekli"))?.to_string();
            open_project(s, Path::new(&dir))
        }),
        spec("crstudio.openLast", "CrStudio: son projeyi aç", "{}", false, |s, _| {
            s.crstudio.restored = true;
            if !s.crstudio.pipeline.project_dir.is_empty() {
                return Ok(json!({"opened": false}));
            }
            match persist::last_project() {
                Some(dir) => open_project(s, &dir).map(|_| json!({"opened": true, "dir": dir.to_string_lossy()})),
                None => Ok(json!({"opened": false})),
            }
        }),
        spec("crstudio.list", "CrStudio: projeler", "{}", false, |s, _| Ok(json!(persist::list(Path::new(&s.crstudio.pipeline.projects_root))))),
        spec("crstudio.save", "CrStudio: kaydet", "{}", false, |s, _| {
            let path = persist::save(&s.crstudio.pipeline).map_err(other)?;
            persist::remember(&s.crstudio.pipeline.project_dir);
            Ok(json!({"path": path.to_string_lossy()}))
        }),
        spec("crstudio.settings", "CrStudio: ayarlar", r#"{"claudeTimeoutSec":u64?,"relevanceCheck":bool?,"directorAi":bool?,"autopilot":bool?,"brandColor":str?,"channelName":str?,"youtuberOverlays":bool?,"brandKitEnabled":bool?,"bgmStyle":str?,"bgmVolumeDb":f64?,"bgmAutoDuck":bool?,"captionStyleMode":str?,"filmGrainEnabled":bool?,"brollPacingEnabled":bool?,"jLCutEnabled":bool?,"voicePolishEnabled":bool?,"synthSfx":bool?,"projectsRoot":str?}"#, true, |s, v| {
            let p = &mut s.crstudio.pipeline;
            if let Some(t) = u64_p(v, "claudeTimeoutSec") {
                p.claude_timeout_sec = t.clamp(60, 3600);
            }
            for (k, slot) in [
                ("relevanceCheck", &mut p.relevance_check),
                ("directorAi", &mut p.director_ai),
                ("autopilot", &mut p.autopilot),
                ("youtuberOverlays", &mut p.youtuber_overlays),
                ("brandKitEnabled", &mut p.brand_kit_enabled),
                ("bgmAutoDuck", &mut p.bgm_auto_duck),
                ("filmGrainEnabled", &mut p.film_grain_enabled),
                ("brollPacingEnabled", &mut p.broll_pacing_enabled),
                ("jLCutEnabled", &mut p.j_l_cut_enabled),
                ("voicePolishEnabled", &mut p.voice_polish_enabled),
                ("synthSfx", &mut p.synth_sfx),
            ] {
                if let Some(b) = bool_p(v, k) {
                    *slot = b;
                }
            }
            if let Some(c) = str_p(v, "brandColor").filter(|c| filmcraft_color::parse_hex(c).is_some()) {
                p.brand_color = c.to_string();
            }
            if let Some(c) = str_p(v, "channelName") {
                p.channel_name = c.chars().take(60).collect();
            }
            if let Some(b) = str_p(v, "bgmStyle").filter(|b| ["mystery_dark_drone", "investigative_pulse", "historical_suspense", "cosmic_synth", "files", "none"].contains(b)) {
                p.bgm_style = b.to_string();
                // keep the Sound & FX agent's line true to the choice
                let summary = format!("{} sahne, toplam {:.0} sn · müzik: {}", p.scenes.len(), p.total_seconds(), super::audio::bgm_label(b));
                p.set_agent_status(6, AgentStatus::Done, summary);
            }
            if let Some(d) = finite(v, "bgmVolumeDb") {
                p.bgm_volume_db = d.clamp(-40.0, 0.0);
            }
            if let Some(c) = str_p(v, "captionStyleMode").filter(|c| ["cinematic_doc", "viral_word_highlight"].contains(c)) {
                p.caption_style_mode = c.to_string();
            }
            if let Some(r) = str_p(v, "projectsRoot").filter(|r| !r.trim().is_empty()) {
                p.projects_root = r.to_string();
            }
            Ok(state(s))
        }),
        spec("crstudio.runPipeline", "CrStudio: ajanları çalıştır", r#"{"title":str?,"format":str?,"tone":str?,"targetDurationSec":f64?,"extraInstructions":str?,"sync":bool?}"#, true, start_preproduction),
        spec("crstudio.cancel", "CrStudio: işi iptal et", "{}", false, |s, _| {
            s.crstudio.cancel.store(true, Ordering::Relaxed);
            Ok(json!({"cancelled": s.crstudio.worker_running}))
        }),
        spec("crstudio.setStage", "CrStudio: aşamaya git", r#"{"stage":str}"#, false, |s, v| {
            let st = str_p(v, "stage").unwrap_or("");
            let stage = match st {
                "setup" | "konu" => PipelineStage::Setup,
                "agents" | "running_agents" | "ajanlar" => PipelineStage::RunningAgents,
                "review" | "review_scenes" | "sahneler" => PipelineStage::ReviewScenes,
                "voiceover" | "seslendirme" => PipelineStage::Voiceover,
                "studio" | "in_studio" | "kurgu" => PipelineStage::InStudio,
                "publish" | "yayin" => PipelineStage::Publish,
                _ => return Err(bad("crstudio.setStage", "bilinmeyen aşama")),
            };
            if stage != PipelineStage::Setup && stage != PipelineStage::RunningAgents && s.crstudio.pipeline.scenes.is_empty() {
                return Err(bad("crstudio.setStage", "önce ajanları çalıştırın"));
            }
            s.crstudio.pipeline.stage = stage;
            Ok(json!({"stage": stage}))
        }),
        spec("crstudio.updateScene", "CrStudio: sahneyi düzenle", r#"{"sceneIndex":usize,"title":str?,"chapter":str?,"narrationText":str?,"voiceText":str?,"durationSeconds":f64?,"visualType":str?,"visualQuery":str?,"cameraMotion":str?,"selectedVisual":str?,"overlayTemplate":str?,"overlayTitle":str?,"overlaySubtitle":str?,"infoText":str?,"overlays":[str]?,"sfxCue":str?,"sources":[usize]?,"approved":bool?,"mapSpec":obj?}"#, true, update_scene),
        spec("crstudio.addScene", "CrStudio: sahne ekle", r#"{"after":usize?,"narrationText":str?}"#, true, |s, v| {
            guard_idle(s, "crstudio.addScene")?;
            let p = &mut s.crstudio.pipeline;
            if p.scenes.len() >= super::MAX_SCENES {
                return Err(bad("crstudio.addScene", "sahne sınırına ulaşıldı"));
            }
            let at = u64_p(v, "after").map(|a| (a as usize + 1).min(p.scenes.len())).unwrap_or(p.scenes.len());
            let chapter = p.scenes.get(at.saturating_sub(1)).map(|s| s.chapter.clone()).unwrap_or_default();
            let mut sc = super::SceneSpec { narration_text: str_p(v, "narrationText").unwrap_or("").chars().take(4000).collect(), text_origin: "user".into(), chapter, ..Default::default() };
            sc.duration_seconds = sc.estimated_seconds();
            p.scenes.insert(at, sc);
            p.sanitize();
            p.active_scene_index = at;
            Ok(json!({"sceneIndex": at, "scenes": p.scenes.len()}))
        }),
        spec("crstudio.removeScene", "CrStudio: sahneyi sil", r#"{"sceneIndex":usize}"#, true, |s, v| {
            guard_idle(s, "crstudio.removeScene")?;
            let idx = scene_index(s, v, "crstudio.removeScene")?;
            let p = &mut s.crstudio.pipeline;
            p.scenes.remove(idx);
            p.sanitize();
            Ok(json!({"scenes": p.scenes.len()}))
        }),
        spec("crstudio.moveScene", "CrStudio: sahneyi taşı", r#"{"sceneIndex":usize,"to":usize}"#, true, |s, v| {
            guard_idle(s, "crstudio.moveScene")?;
            let idx = scene_index(s, v, "crstudio.moveScene")?;
            let p = &mut s.crstudio.pipeline;
            let to = u64_p(v, "to").map(|t| (t as usize).min(p.scenes.len().saturating_sub(1))).unwrap_or(idx);
            let sc = p.scenes.remove(idx);
            p.scenes.insert(to, sc);
            p.sanitize();
            p.active_scene_index = to;
            Ok(json!({"sceneIndex": to}))
        }),
        spec("crstudio.addUserVisual", "CrStudio: kendi görselini ekle", r#"{"sceneIndex":usize,"path":str}"#, true, |s, v| {
            let idx = scene_index(s, v, "crstudio.addUserVisual")?;
            let path = str_p(v, "path").ok_or_else(|| bad("crstudio.addUserVisual", "`path` gerekli"))?.to_string();
            let img = render::decode_image(Path::new(&path)).ok_or_else(|| bad("crstudio.addUserVisual", "görsel açılamadı"))?;
            let Some(sc) = s.crstudio.pipeline.scenes.get_mut(idx) else { return Err(bad("crstudio.addUserVisual", "sahne yok")) };
            sc.visual_candidates.insert(0, VisualCandidate { id: format!("user_{}_{}", idx + 1, sc.visual_candidates.len()), title: Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), kind: "user".into(), local_path: path.clone(), license: "Kendi görselim".into(), author: "Ben".into(), width: img.w as u32, height: img.h as u32, score: 1.0, relevance: Some(100.0), ..Default::default() });
            sc.selected_visual = Some(path);
            Ok(json!({"sceneIndex": idx}))
        }),
        spec("crstudio.huntVisuals", "CrStudio: görselleri yeniden ara", r#"{"sceneIndex":usize?,"query":str?}"#, true, |s, v| {
            guard_idle(s, "crstudio.huntVisuals")?;
            if let Some(i) = u64_p(v, "sceneIndex").map(|x| x as usize) {
                if i >= s.crstudio.pipeline.scenes.len() {
                    return Err(bad("crstudio.huntVisuals", "sahne yok"));
                }
                if let Some(q) = str_p(v, "query")
                    && let Some(sc) = s.crstudio.pipeline.scenes.get_mut(i)
                {
                    sc.visual_query = q.chars().take(300).collect();
                    sc.selected_visual = None;
                }
                spawn_job(s, "Görsel arama", move |sh, c| {
                    let snap = with_shared(sh, |p| p.clone());
                    let Some(mut sc) = snap.scenes.get(i).cloned() else { return };
                    visuals::hunt_scene(&snap, &mut sc);
                    let mut one = snap.clone();
                    one.scenes = vec![sc.clone()];
                    if snap.relevance_check && !c.load(Ordering::Relaxed)
                        && let Some(rev) = visuals::relevance_review(&one, sh, c)
                        && let Some(r) = rev.into_iter().next()
                    {
                        sc = r;
                    }
                    with_shared(sh, |p| {
                        if let Some(slot) = p.scenes.get_mut(i) {
                            slot.visual_candidates = sc.visual_candidates;
                            slot.selected_visual = sc.selected_visual;
                        }
                        p.push_log("Görsel Avcısı", "ok", format!("Sahne {} için görseller yenilendi", i + 1));
                    });
                })
                .map_err(other)?;
            } else {
                spawn_job(s, "Görsel arama", |sh, c| {
                    let snap = with_shared(sh, |p| p.clone());
                    let scenes = visuals::hunt_all(&snap, sh, c);
                    with_shared(sh, |p| p.scenes = scenes);
                })
                .map_err(other)?;
            }
            Ok(json!({"started": true}))
        }),
        spec("crstudio.generateMap", "CrStudio: harita çiz", r#"{"sceneIndex":usize,"locationName":str?,"lat":f64?,"lon":f64?,"fromLat":f64?,"fromLon":f64?,"zoom":f64?,"subtitle":str?}"#, true, |s, v| {
            let idx = scene_index(s, v, "crstudio.generateMap")?;
            let (w, h) = s.crstudio.pipeline.format.dimensions();
            let dir = s.crstudio.pipeline.project_dir.clone();
            let Some(sc) = s.crstudio.pipeline.scenes.get_mut(idx) else { return Err(bad("crstudio.generateMap", "sahne yok")) };
            let mut ms = sc.map_spec.clone().unwrap_or_default();
            if let Some(l) = str_p(v, "locationName") {
                ms.location_name = l.chars().take(80).collect();
            }
            if let Some(x) = finite(v, "lat") {
                ms.lat = x;
            }
            if let Some(x) = finite(v, "lon") {
                ms.lon = x;
            }
            ms.from_lat = finite(v, "fromLat").or(ms.from_lat);
            ms.from_lon = finite(v, "fromLon").or(ms.from_lon);
            if let Some(z) = finite(v, "zoom") {
                ms.zoom = z;
            }
            if let Some(t) = str_p(v, "subtitle") {
                ms.subtitle = t.chars().take(120).collect();
            }
            ms.sanitize();
            let (png, real) = render::render_map_png(w, h, &ms, &sc.title)?;
            let path = Path::new(&dir).join("scenes").join(format!("scene_{:02}", idx + 1)).join(format!("scene_{:02}_map_custom.png", idx + 1));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            filmcraft_format::atomic_write(&path, &png).map_err(|e| other(e.to_string()))?;
            let ps = path.to_string_lossy().into_owned();
            sc.visual_candidates.retain(|c| c.local_path != ps);
            sc.visual_candidates.insert(0, VisualCandidate { id: format!("map_custom_{}", idx + 1), title: format!("Harita: {}", ms.location_name), kind: "animated_map".into(), local_path: ps.clone(), license: if real { "Natural Earth (kamu malı) + özgün çizim".into() } else { "Özgün çizim (kendi)".into() }, author: "CrStudio".into(), width: w, height: h, score: 1.0, ..Default::default() });
            sc.selected_visual = Some(ps.clone());
            sc.visual_type = "animated_map".into();
            sc.overlay_template = "builtin:doc-map-pin".into();
            sc.map_spec = Some(ms);
            Ok(json!({"sceneIndex": idx, "path": ps, "realGeography": real}))
        }),
        spec("crstudio.approveAll", "CrStudio: tüm sahneleri onayla", r#"{"approved":bool?}"#, true, |s, v| {
            guard_idle(s, "crstudio.approveAll")?;
            let ok = bool_p(v, "approved").unwrap_or(true);
            let p = &mut s.crstudio.pipeline;
            p.scenes.iter_mut().for_each(|sc| sc.approved = ok);
            if ok && !p.scenes.is_empty() {
                p.stage = PipelineStage::Voiceover;
                p.push_log("CrStudio", "ok", "Sahneler onaylandı; seslendirme aşamasına geçildi");
            }
            Ok(json!({"approved": ok, "stage": p.stage}))
        }),
        spec("crstudio.voices", "CrStudio: ses motorları", "{}", false, |s, _| {
            Ok(json!({"engines": voice::engines(), "selected": s.crstudio.pipeline.voice_engine, "default": voice::default_engine(), "chatterboxRunning": voice::chatterbox_running()}))
        }),
        spec("crstudio.setVoice", "CrStudio: ses motorunu seç", r#"{"engine":str?,"reference":str?}"#, true, |s, v| {
            if let Some(e) = str_p(v, "engine") {
                s.crstudio.pipeline.voice_engine = e.to_string();
            }
            if let Some(r) = str_p(v, "reference") {
                s.crstudio.pipeline.voice_reference = (!r.is_empty()).then(|| r.to_string());
            }
            Ok(json!({"engine": s.crstudio.pipeline.voice_engine, "reference": s.crstudio.pipeline.voice_reference}))
        }),
        spec("crstudio.synthesizeVoiceover", "CrStudio: taslak AI seslendirme", r#"{"sceneIndex":usize?,"all":bool?,"force":bool?,"engine":str?,"sync":bool?}"#, true, |s, v| {
            guard_idle(s, "crstudio.synthesizeVoiceover")?;
            if let Some(e) = str_p(v, "engine") {
                s.crstudio.pipeline.voice_engine = e.to_string();
            }
            let only = if bool_p(v, "all").unwrap_or(false) { None } else { u64_p(v, "sceneIndex").map(|x| x as usize) };
            if let Some(i) = only
                && i >= s.crstudio.pipeline.scenes.len()
            {
                return Err(bad("crstudio.synthesizeVoiceover", "sahne yok"));
            }
            let force = bool_p(v, "force").unwrap_or(false);
            spawn_job(s, "Seslendirme", move |sh, c| synth_job(sh, c, only, force)).map_err(other)?;
            if bool_p(v, "sync").unwrap_or(false) {
                wait_job(s);
            }
            Ok(json!({"started": true}))
        }),
        spec("crstudio.recordVoiceoverStart", "CrStudio: mikrofon kaydını başlat", r#"{"sceneIndex":usize?}"#, false, |s, v| {
            guard_idle(s, "crstudio.recordVoiceoverStart")?;
            let idx = scene_index(s, v, "crstudio.recordVoiceoverStart")?;
            let dev = s.prefs.voice_over.source.clone();
            let fmt = s.voiceover.start_direct(&dev, 48_000).map_err(other)?;
            s.crstudio.pipeline.recording_scene_index = Some(idx);
            s.crstudio.pipeline.recording_rate = fmt.sample_rate.max(8000);
            Ok(json!({"recording": true, "sceneIndex": idx, "sampleRate": fmt.sample_rate}))
        }),
        spec("crstudio.recordVoiceoverStop", "CrStudio: mikrofon kaydını bitir", "{}", false, |s, _| {
            let Some(idx) = s.crstudio.pipeline.recording_scene_index.take() else { return Err(bad("crstudio.recordVoiceoverStop", "kayıt yok")) };
            let rate = s.crstudio.pipeline.recording_rate.max(8000);
            let samples = s.voiceover.stop_direct(rate).map_err(other)?;
            if samples.len() < (rate as usize) / 4 {
                return Err(bad("crstudio.recordVoiceoverStop", "kayıt boş ya da çok kısa (mikrofon izni ve giriş cihazını kontrol edin)"));
            }
            let dir = Path::new(&s.crstudio.pipeline.project_dir).join("voiceovers");
            let _ = std::fs::create_dir_all(&dir);
            // the raw take is kept next to the cleaned one
            let raw_path = dir.join(format!("scene_{:02}_kayit_ham.wav", idx + 1));
            let raw_ps = raw_path.to_string_lossy().into_owned();
            s.services.write_file(&raw_ps, &super::wav::encode(&samples, rate)).map_err(|e| other(format!("{raw_ps}: {e}")))?;
            let (clean, report) = super::recclean::clean(&samples, rate);
            let path = dir.join(format!("scene_{:02}_kayit.wav", idx + 1));
            let bytes = super::wav::encode(&clean, rate);
            let ps = path.to_string_lossy().into_owned();
            s.services.write_file(&ps, &bytes).map_err(|e| other(format!("{ps}: {e}")))?;
            write_attribution_sidecar(&path, "Narration", "Kendi sesim", "original work", "Kendi kaydım", "Recorded in CrStudio");
            let dur = clean.len() as f64 / f64::from(rate);
            let text = s.crstudio.pipeline.scenes.get(idx).map(|sc| sc.narration_text.clone()).unwrap_or_default();
            // every take also feeds the voice-clone library
            let _ = super::recclean::add_piece(&clean, rate, &text, "sahne", &report);
            if let Some(sc) = s.crstudio.pipeline.scenes.get_mut(idx) {
                sc.record_report = super::recclean::summary(&report) + &report.warnings.iter().map(|w| format!(" · {w}")).collect::<String>();
                sc.voiceover_raw_path = Some(ps.clone());
                sc.voiceover_path = Some(ps.clone());
                sc.voiceover_duration_sec = Some(dur);
                sc.voiceover_source = "Kendi kaydım".into();
                sc.voice_spoken.clear();
                sc.voice_char_times.clear();
                sc.voice_check.clear();
                sc.voice_checked = false;
                sc.voiceover_engine = "record".into();
                sc.voiceover_stale = false;
            }
            Ok(json!({"sceneIndex": idx, "path": ps, "durationSec": dur, "report": report}))
        }),
        spec("crstudio.attachVoiceover", "CrStudio: ses dosyası bağla", r#"{"sceneIndex":usize,"path":str}"#, true, |s, v| {
            let idx = scene_index(s, v, "crstudio.attachVoiceover")?;
            let path = str_p(v, "path").ok_or_else(|| bad("crstudio.attachVoiceover", "`path` gerekli"))?.to_string();
            let dur = super::wav::duration(Path::new(&path)).ok_or_else(|| bad("crstudio.attachVoiceover", "WAV dosyası okunamadı"))?;
            if let Some(sc) = s.crstudio.pipeline.scenes.get_mut(idx) {
                sc.voiceover_raw_path = Some(path.clone());
                sc.voiceover_path = Some(path.clone());
                sc.voiceover_duration_sec = Some(dur);
                sc.voiceover_source = "Harici dosya".into();
                sc.voice_spoken.clear();
                sc.voice_char_times.clear();
                sc.voice_check.clear();
                sc.voice_checked = false;
                sc.voiceover_engine = "file".into();
                sc.voiceover_stale = false;
            }
            Ok(json!({"sceneIndex": idx, "durationSec": dur}))
        }),
        spec("crstudio.auditEffects", "CrStudio: ses ve efekt denetimi", r#"{"bgmStyle":str?}"#, true, |s, v| {
            guard_idle(s, "crstudio.auditEffects")?;
            if let Some(b) = str_p(v, "bgmStyle").filter(|b| ["mystery_dark_drone", "investigative_pulse", "historical_suspense", "cosmic_synth", "files", "none"].contains(b)) {
                s.crstudio.pipeline.bgm_style = b.to_string();
            }
            spawn_job(s, "Ses ve efekt", |sh, _| {
                let snap = with_shared(sh, |p| p.clone());
                let a = super::audio::design(&snap);
                with_shared(sh, |p| a.apply(p));
            })
            .map_err(other)?;
            Ok(json!({"started": true}))
        }),
        spec("crstudio.packageYoutube", "CrStudio: yayın paketi", r#"{"thumbnailText":str?}"#, true, |s, v| {
            guard_idle(s, "crstudio.packageYoutube")?;
            if let Some(t) = str_p(v, "thumbnailText") {
                s.crstudio.pipeline.social_meta.thumbnail_text = t.chars().take(60).collect();
            }
            spawn_job(s, "Yayın paketi", |sh, _| {
                let snap = with_shared(sh, |p| p.clone());
                let pkg = super::package::build(&snap);
                with_shared(sh, |p| {
                    pkg.apply(p);
                    p.director = Some(director::review(p));
                });
            })
            .map_err(other)?;
            Ok(json!({"started": true}))
        }),
        spec("crstudio.description", "CrStudio: YouTube açıklaması", "{}", false, |s, _| Ok(json!({"description": super::package::full_description(&s.crstudio.pipeline)}))),
        spec("crstudio.assembleTimeline", "CrStudio: kurguya gönder", "{}", true, assemble::assemble_timeline),
        spec("crstudio.createShortsCut", "CrStudio: Shorts kesiti", "{}", true, assemble::create_shorts_cut),
        spec("crstudio.review", "CrStudio: Yayın Yönetmeni değerlendirmesi", r#"{"ai":bool?,"sync":bool?}"#, true, |s, v| {
            if !bool_p(v, "ai").unwrap_or(s.crstudio.pipeline.director_ai) {
                let r = director::review(&s.crstudio.pipeline);
                s.crstudio.pipeline.set_agent_status(9, AgentStatus::Done, format!("Yayınlanabilirlik {:.0}/100", r.score));
                s.crstudio.pipeline.director = Some(r.clone());
                return Ok(serde_json::to_value(r).unwrap_or_default());
            }
            guard_idle(s, "crstudio.review")?;
            spawn_job(s, "Yayın Yönetmeni", agents::run_director).map_err(other)?;
            if bool_p(v, "sync").unwrap_or(false) {
                wait_job(s);
            }
            Ok(json!({"started": true}))
        }),
        spec("crstudio.plan", "CrStudio: üretim hattı planı", "{}", false, |s, _| {
            let steps = super::production::plan(&s.crstudio.pipeline);
            Ok(json!({"steps": steps, "queue": s.crstudio.pipeline.queue, "running": s.crstudio.worker_running}))
        }),
        spec("crstudio.queue.add", "CrStudio: üretim hattına ekle", r#"{"steps":[str]?,"command":str?,"params":obj?,"label":str?}"#, true, |s, v| {
            let mut add: Vec<super::production::QueuedCommand> = Vec::new();
            if let Some(ids) = v["steps"].as_array() {
                let ids: Vec<String> = ids.iter().filter_map(Value::as_str).map(str::to_string).collect();
                add.extend(super::production::commands_for(&s.crstudio.pipeline, &ids));
            }
            if let Some(cmd) = str_p(v, "command").filter(|c| c.starts_with("crstudio.") || c.starts_with("export.")) {
                add.push(super::production::QueuedCommand {
                    label: str_p(v, "label").unwrap_or(cmd).chars().take(120).collect(),
                    command: cmd.to_string(),
                    params: v.get("params").cloned().unwrap_or_else(|| json!({})),
                    seconds: 0.0,
                });
            }
            let n = add.len();
            let q = &mut s.crstudio.pipeline.queue;
            q.extend(add);
            q.truncate(200);
            s.crstudio.pipeline.push_log("Üretim Hattı", "info", format!("{n} iş sıraya eklendi"));
            super::pump_queue(s);
            Ok(json!({"added": n, "queue": s.crstudio.pipeline.queue}))
        }),
        spec("crstudio.queue.clear", "CrStudio: üretim hattını boşalt", "{}", true, |s, _| {
            let n = s.crstudio.pipeline.queue.len();
            s.crstudio.pipeline.queue.clear();
            Ok(json!({"cleared": n}))
        }),
        spec("crstudio.sceneIssues", "CrStudio: sahne panosu uyarıları", "{}", false, |s, _| {
            Ok(json!({"issues": super::production::scene_issues(&s.crstudio.pipeline)}))
        }),
        spec("crstudio.applyMapPlan", "CrStudio: tekrarlayan haritalara fotoğraf", "{}", true, |s, _| {
            guard_idle(s, "crstudio.applyMapPlan")?;
            let n = super::production::apply_map_plan(&mut s.crstudio.pipeline);
            s.crstudio.pipeline.push_log("Harita & Grafik", "ok", format!("Aynı yeri tekrarlayan {n} sahneye fotoğraf konuldu"));
            Ok(json!({"changed": n}))
        }),
        spec("crstudio.exportAll", "CrStudio: uzun video ve Shorts'u dışa aktar", "{}", true, |s, _| {
            let p = &s.crstudio.pipeline;
            if p.project_dir.is_empty() {
                return Err(bad("crstudio.exportAll", "proje klasörü yok"));
            }
            let dir = Path::new(&p.project_dir).join("yayin");
            let _ = std::fs::create_dir_all(&dir);
            let slug = super::turkish::slug(&p.title);
            let jobs: Vec<(u64, String, &str)> = [
                (p.assembled_sequence, format!("{slug}-uzun.mp4"), "YouTube 1080p Full HD"),
                (p.shorts_sequence, format!("{slug}-shorts.mp4"), "Social Vertical 1080×1920"),
            ]
            .into_iter()
            .filter_map(|(seq, name, preset)| seq.map(|q| (q, name, preset)))
            .collect();
            if jobs.is_empty() {
                return Err(bad("crstudio.exportAll", "önce kurguyu kurun"));
            }
            let total = p.total_seconds();
            let mut added = Vec::new();
            for (seq, name, preset) in jobs {
                let path = dir.join(&name).to_string_lossy().into_owned();
                let r = s.execute("export.queue.add", json!({"preset": preset, "path": path, "sequences": [seq], "range": "entire", "start": true}))?;
                added.push(json!({"path": path, "result": r}));
            }
            // the export runs outside the CrStudio worker: keep the Mac awake for its estimated length
            #[cfg(target_os = "macos")]
            {
                let secs = ((total + 60.0) * 4.0).clamp(300.0, 4.0 * 3600.0) as u64;
                let _ = std::process::Command::new("caffeinate")
                    .args(["-i", "-t", &secs.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            let _ = total;
            s.crstudio.pipeline.push_log("Üretim Hattı", "info", format!("Dışa aktarma sıraya alındı ({} dosya, yayin klasörü)", added.len()));
            Ok(json!({"exports": added}))
        }),
        spec("crstudio.directVoice", "CrStudio: anlatımı yönet (Ses Yönetmeni)", r#"{"sync":bool?}"#, true, |s, v| {
            guard_idle(s, "crstudio.directVoice")?;
            spawn_job(s, "Ses Yönetmeni", agents::run_voice_director).map_err(other)?;
            if bool_p(v, "sync").unwrap_or(false) {
                wait_job(s);
            }
            Ok(json!({"started": true}))
        }),
        spec("crstudio.ttsServer", "CrStudio: Chatterbox sunucusu", r#"{"action":"start|stop|status"}"#, false, |_, v| match str_p(v, "action").unwrap_or("status") {
            "start" => {
                std::thread::spawn(|| {
                    let _ = voice::ensure_chatterbox();
                });
                Ok(json!({"starting": true}))
            }
            "stop" => {
                voice::stop_chatterbox();
                Ok(json!({"stopped": true}))
            }
            _ => Ok(json!({"running": voice::chatterbox_running()})),
        }),
        spec("crstudio.youtube.status", "CrStudio: YouTube durumu", "{}", false, |_, _| Ok(youtube::status())),
        spec("crstudio.youtube.configure", "CrStudio: YouTube API bilgileri", r#"{"clientId":str,"clientSecret":str}"#, false, |_, v| {
            let mut c = youtube::load_config();
            c.client_id = str_p(v, "clientId").unwrap_or("").trim().to_string();
            c.client_secret = str_p(v, "clientSecret").unwrap_or("").trim().to_string();
            youtube::save_config(&c).map_err(other)?;
            Ok(youtube::status())
        }),
        spec("crstudio.youtube.connect", "CrStudio: YouTube hesabını bağla", "{}", false, |_, _| {
            let url = youtube::begin_connect().map_err(other)?;
            Ok(json!({"authUrl": url}))
        }),
        spec("crstudio.youtube.upload", "CrStudio: YouTube'a yükle", r#"{"videoPath":str,"privacy":str?,"publishAt":str?,"title":str?,"syntheticMedia":bool?}"#, false, |s, v| {
            let p = &s.crstudio.pipeline;
            let req = youtube::UploadRequest {
                video_path: str_p(v, "videoPath").unwrap_or("").to_string(),
                title: str_p(v, "title").map(str::to_string).unwrap_or_else(|| p.title.clone()),
                description: super::package::full_description(p),
                tags: p.social_meta.tags.clone(),
                privacy: str_p(v, "privacy").unwrap_or("private").to_string(),
                publish_at: str_p(v, "publishAt").map(str::to_string),
                thumbnail_path: p.thumbnail_path.clone(),
                category_id: "27".into(),
                made_for_kids: false,
                synthetic_media: bool_p(v, "syntheticMedia").unwrap_or(false),
            };
            if let Some(r) = &p.director
                && r.blocking
                && !bool_p(v, "force").unwrap_or(false)
            {
                return Err(bad("crstudio.youtube.upload", "Yayın Yönetmeni kritik eksik buldu; önce düzeltin (ya da force:true)"));
            }
            youtube::start_upload(req).map_err(other)?;
            Ok(json!({"started": true}))
        }),
        spec("crstudio.projectFolder", "CrStudio: proje klasörü", "{}", false, |s, _| {
            let dir = PathBuf::from(&s.crstudio.pipeline.project_dir);
            Ok(json!({"dir": dir.to_string_lossy(), "exists": dir.is_dir()}))
        }),
        spec("crstudio.clone.status", "CrStudio: klon kütüphanesi", "{}", false, |s, _| {
            let l = super::recclean::load_library();
            let list = super::recclean::reading_list(&l);
            let cursor = l.cursor.min(list.len().saturating_sub(1));
            Ok(json!({
                "pieces": l.pieces.len(),
                "minutes": super::recclean::total_seconds(&l) / 60.0,
                "weak": l.pieces.iter().filter(|p| p.grade == "zayıf").count(),
                "fromScenes": l.pieces.iter().filter(|p| p.source == "sahne").count(),
                "readingCount": list.len(),
                "cursor": cursor,
                "text": list.get(cursor).cloned().unwrap_or_default(),
                "customText": l.custom_text,
                "recording": s.crstudio.clone_rec.is_some(),
                "dir": super::recclean::clone_dir().to_string_lossy(),
                "last": l.pieces.iter().rev().take(8).map(|p| json!({"seconds": p.seconds, "grade": p.grade, "source": p.source, "text": super::truncate_line(&p.text, 60)})).collect::<Vec<_>>(),
            }))
        }),
        spec("crstudio.clone.goto", "CrStudio: klon okuma metni", r#"{"index":usize?,"delta":i64?}"#, false, |_, v| {
            let mut l = super::recclean::load_library();
            let n = super::recclean::reading_list(&l).len().max(1);
            let cur = l.cursor.min(n - 1) as i64;
            let to = u64_p(v, "index").map(|i| i as i64).unwrap_or_else(|| cur.saturating_add(v.get("delta").and_then(Value::as_i64).unwrap_or(1)));
            l.cursor = to.clamp(0, n as i64 - 1) as usize;
            super::recclean::save_library(&l).map_err(other)?;
            Ok(json!({"cursor": l.cursor}))
        }),
        spec("crstudio.clone.setText", "CrStudio: kendi okuma metnim", r#"{"text":str}"#, false, |_, v| {
            let mut l = super::recclean::load_library();
            l.custom_text = str_p(v, "text").unwrap_or("").chars().take(500_000).collect();
            l.cursor = 0;
            super::recclean::save_library(&l).map_err(other)?;
            Ok(json!({"paragraphs": super::recclean::reading_list(&l).len()}))
        }),
        spec("crstudio.clone.recordStart", "CrStudio: klon okumasını kaydet", r#"{"test":bool?}"#, false, |s, v| {
            if s.crstudio.pipeline.recording_scene_index.is_some() || s.crstudio.clone_rec.is_some() {
                return Err(bad("crstudio.clone.recordStart", "zaten kayıt yapılıyor"));
            }
            let l = super::recclean::load_library();
            let dev = s.prefs.voice_over.source.clone();
            let fmt = s.voiceover.start_direct(&dev, 48_000).map_err(other)?;
            s.crstudio.clone_rec = Some((fmt.sample_rate.max(8000), l.cursor, bool_p(v, "test").unwrap_or(false)));
            Ok(json!({"recording": true, "sampleRate": fmt.sample_rate}))
        }),
        spec("crstudio.clone.recordStop", "CrStudio: klon okumasını bitir", "{}", false, |s, _| {
            let Some((rate, idx, test)) = s.crstudio.clone_rec.take() else { return Err(bad("crstudio.clone.recordStop", "kayıt yok")) };
            let samples = s.voiceover.stop_direct(rate).map_err(other)?;
            if samples.len() < (rate as usize) / 4 {
                return Err(bad("crstudio.clone.recordStop", "kayıt boş ya da çok kısa (mikrofon izni ve giriş cihazını kontrol edin)"));
            }
            let (clean, report) = super::recclean::clean(&samples, rate);
            if test {
                return Ok(json!({"test": true, "report": report, "summary": super::recclean::summary(&report)}));
            }
            let mut l = super::recclean::load_library();
            let list = super::recclean::reading_list(&l);
            let text = list.get(idx).cloned().unwrap_or_default();
            let piece = super::recclean::add_piece(&clean, rate, &text, "okuma", &report).map_err(other)?;
            // move on after a usable take
            l = super::recclean::load_library();
            if report.grade != "zayıf" {
                l.cursor = (idx + 1).min(list.len().saturating_sub(1));
                super::recclean::save_library(&l).map_err(other)?;
            }
            Ok(json!({"report": report, "summary": super::recclean::summary(&report), "piece": piece}))
        }),
        spec("crstudio.clone.removeLast", "CrStudio: son klon kaydını sil", "{}", false, |_, _| {
            let mut l = super::recclean::load_library();
            let Some(p) = l.pieces.pop() else { return Err(bad("crstudio.clone.removeLast", "kütüphane boş")) };
            let _ = std::fs::remove_file(&p.path);
            super::recclean::save_library(&l).map_err(other)?;
            Ok(json!({"removed": p.path}))
        }),
        spec("crstudio.clone.export", "CrStudio: klon parçalarını dışa aktar", "{}", false, |_, _| {
            let parts = super::recclean::export_parts().map_err(other)?;
            Ok(json!({"parts": parts.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(), "dir": super::recclean::clone_dir().join("elevenlabs").to_string_lossy()}))
        }),
        spec("crstudio.blender.status", "CrStudio: Blender durumu", "{}", false, |s, _| {
            let mut v = super::blender::status();
            v["concept"] = json!(s.crstudio.pipeline.visual_concept);
            Ok(v)
        }),
        spec("crstudio.setConcept", "CrStudio: görsel konsept", r#"{"concept":"bakir|minyatur|diorama"}"#, true, |s, v| {
            let c = str_p(v, "concept").unwrap_or("");
            if !super::blender::concept_ok(c) {
                return Err(bad("crstudio.setConcept", format!("bilinmeyen konsept `{c}`")));
            }
            s.crstudio.pipeline.visual_concept = c.to_string();
            Ok(json!({"concept": c}))
        }),
        spec("crstudio.blender.render", "CrStudio: Blender çekimi üret", r#"{"sceneIndex":usize?,"shot":"map|route|intro|chapter"?,"quality":"draft|final"?}"#, false, |s, v| {
            guard_idle(s, "crstudio.blender.render")?;
            let idx = scene_index(s, v, "crstudio.blender.render")?;
            let kind = str_p(v, "shot").filter(|k| matches!(*k, "map" | "route" | "intro" | "chapter")).unwrap_or("map").to_string();
            let quality = if str_p(v, "quality") == Some("draft") { "draft" } else { "final" }.to_string();
            if super::blender::find_blender().is_none() {
                return Err(other("Blender bulunamadı. blender.org'dan indirip Uygulamalar klasörüne kurun (ücretsiz)."));
            }
            spawn_job(s, "Blender çekimi", move |sh, c| blender_job(sh, c, idx, kind, quality)).map_err(other)?;
            Ok(json!({"started": true, "sceneIndex": idx}))
        }),
        spec("crstudio.elevenlabs.status", "CrStudio: ElevenLabs durumu", "{}", false, |_, _| Ok(super::elevenlabs::load().public())),
        spec(
            "crstudio.elevenlabs.configure",
            "CrStudio: ElevenLabs ayarları",
            r#"{"apiKey":str?,"voiceId":str?,"voiceName":str?,"modelId":str?,"stability":f64?,"similarity":f64?,"style":f64?,"speed":f64?,"forget":bool?}"#,
            false,
            |_, v| {
                let mut c = super::elevenlabs::load();
                if bool_p(v, "forget").unwrap_or(false) {
                    c = super::elevenlabs::ElevenConfig::default();
                }
                if let Some(k) = str_p(v, "apiKey").filter(|k| !k.trim().is_empty()) {
                    c.api_key = k.to_string();
                }
                if let Some(x) = str_p(v, "voiceId") {
                    c.voice_id = x.to_string();
                }
                if let Some(x) = str_p(v, "voiceName") {
                    c.voice_name = x.to_string();
                }
                if let Some(x) = str_p(v, "modelId") {
                    c.model_id = x.to_string();
                }
                for (k, slot) in [("stability", &mut c.stability), ("similarity", &mut c.similarity), ("style", &mut c.style), ("speed", &mut c.speed)] {
                    if let Some(x) = finite(v, k) {
                        *slot = x;
                    }
                }
                super::elevenlabs::save(&c).map_err(other)?;
                Ok(super::elevenlabs::load().public())
            },
        ),
        spec("crstudio.elevenlabs.voices", "CrStudio: ElevenLabs sesleri", "{}", false, |_, _| {
            let list = super::elevenlabs::voices(&super::elevenlabs::load()).map_err(other)?;
            Ok(json!({"voices": list}))
        }),
        spec("crstudio.lexicon.get", "CrStudio: telaffuz sözlüğü", "{}", false, |_, _| {
            let text = voice::lexicon_text();
            Ok(json!({"text": text, "entries": turkish::parse_lexicon(&text).len(), "path": voice::lexicon_path().to_string_lossy()}))
        }),
        spec("crstudio.lexicon.set", "CrStudio: telaffuz sözlüğünü kaydet", r#"{"text":str}"#, false, |_, v| {
            let text = str_p(v, "text").ok_or_else(|| bad("crstudio.lexicon.set", "`text` gerekli"))?;
            let n = voice::save_lexicon_text(text).map_err(other)?;
            Ok(json!({"entries": n}))
        }),
        spec("crstudio.speechText", "CrStudio: seslendirilecek metni göster", r#"{"text":str}"#, false, |_, v| {
            let text = str_p(v, "text").unwrap_or("");
            Ok(json!({"spoken": turkish::normalize_for_speech(&turkish::apply_lexicon(text, &voice::load_lexicon()))}))
        }),
    ]
    .into_iter()
    .chain(super::assistant::commands())
    .collect()
}

#[cfg(test)]
mod first_sentence_tests {
    use super::first_sentence;

    #[test]
    fn ordinals_and_ellipses_do_not_end_the_sentence() {
        assert_eq!(
            first_sentence("1926... 1928. Kazılarda 5. ve 6. yüzyıla ait birkaç yüz mezar çıkar. Bir kısmında yazıtlar var."),
            "1926... 1928. Kazılarda 5. ve 6. yüzyıla ait birkaç yüz mezar çıkar."
        );
        assert_eq!(first_sentence("Rivayete göre kemikler Marsilya'ya götürülür. Ama bu tarihî değil."), "Rivayete göre kemikler Marsilya'ya götürülür.");
        assert_eq!(first_sentence("Kısa."), "Kısa.");
        assert_eq!(first_sentence(""), "");
    }
}
