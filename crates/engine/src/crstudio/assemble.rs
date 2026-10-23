//! Agent 7 — the Editor: builds the multi-track sequence from the approved scenes, and the
//! vertical Shorts cut.
//!
//! Tracks: V1 pictures (Ken Burns, colour, grain, B-roll sub-cuts), V2 documentary graphics,
//! V3 YouTuber pop-ups (info/warning boxes, bubbles, like/comment prompts…), V4 channel kit
//! (subscribe, "watch next" end screen); C1 captions; A1 narration, A2 SFX (J-cut lead), A3
//! score ducked under the narration. All timing is in `Tick`s; one undo step.

use std::path::Path;
use std::sync::Arc;

use filmcraft_geom::Vec2;
use filmcraft_project::{BinId, CaptionAlign, CaptionStyle, ClipId, ItemId, ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, ParamValue};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::{AgentStatus, CrStudioPipeline, PipelineStage, SceneSpec, VideoFormatPreset, audio, turkish, voice};
use crate::{EngineError, Event, Result, Session};

pub(super) fn ensure_named_bin(s: &mut Session, name: &str) -> Result<BinId> {
    if let Some(existing) = s.project.root.children.iter().find_map(|c| match c {
        filmcraft_project::BinEntry::Bin(b) if b.name == name => Some(b.id),
        _ => None,
    }) {
        return Ok(existing);
    }
    let r = s.execute("file.newBin", json!({"name": name}))?;
    let id = r["bin"].as_u64().ok_or_else(|| EngineError::Other("kutu oluşturulamadı".into()))?;
    Ok(BinId(id))
}

/// The project item already imported from `path` — when the file is still the one it was
/// imported from — or a new import. A narration re-synthesized under the same name must not reuse
/// the old item: its length (and the clip cut to it) would be the old take's.
pub(super) fn import_path(s: &mut Session, path: &str, bin: BinId) -> Result<ItemId> {
    let now = crate::relink::identity_of(s.services.as_ref(), path).ok();
    let mut ids = Vec::new();
    s.project.root.all_items(&mut ids);
    for id in ids.into_iter().rev() {
        if let Some(it) = s.project.item(id)
            && let ItemKind::Media(m) = &it.kind
            && matches!(&m.media, MediaRef::File { path: p } if p == path)
            && m.identity.is_some()
            && m.identity == now
        {
            return Ok(id);
        }
    }
    let bytes = s.services.read_file(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    crate::commands::import_bytes(s, path, bytes.into(), Some(bin))
}

/// Length of an imported media item in seconds (0 when unknown).
fn media_seconds(s: &Session, id: ItemId) -> f64 {
    match s.project.item(id).map(|i| &i.kind) {
        Some(ItemKind::Media(m)) => m.duration().seconds(),
        _ => 0.0,
    }
}

/// Scale (percent) that makes a `src` picture cover a `frame` (no black bars).
fn cover_percent(src: (u32, u32), frame: (u32, u32)) -> f64 {
    if src.0 == 0 || src.1 == 0 {
        return 100.0;
    }
    let s = (f64::from(frame.0) / f64::from(src.0)).max(f64::from(frame.1) / f64::from(src.1));
    (s * 100.0).clamp(1.0, 2000.0)
}

/// Apply a graphics template on its layer (`track` = video track index); when that layer is taken
/// there, fall back to the first free track above the picture rather than dropping the graphic.
fn apply_gfx(s: &mut Session, mut args: Value) -> Result<Value> {
    // the channel look recolours every overlay (YouTuber boxes, tags, end cards)
    if s.crstudio.pipeline.visual_concept == "atlas"
        && let Some(o) = args.as_object_mut()
    {
        o.entry("theme").or_insert_with(|| json!("atlas"));
    }
    match s.execute("graphics.template.apply", args.clone()) {
        Ok(v) => Ok(v),
        Err(_) => {
            let mut a = args;
            if let Some(o) = a.as_object_mut() {
                o.remove("track");
            }
            s.execute("graphics.template.apply", a)
        }
    }
}

/// Scale (percent) that fits a whole `src` picture inside a `frame`.
fn contain_percent(src: (u32, u32), frame: (u32, u32)) -> f64 {
    if src.0 == 0 || src.1 == 0 {
        return 100.0;
    }
    let s = (f64::from(frame.0) / f64::from(src.0)).min(f64::from(frame.1) / f64::from(src.1));
    (s * 100.0).clamp(1.0, 2000.0)
}

fn source_size(scene: &SceneSpec, path: &str, frame: (u32, u32)) -> (u32, u32) {
    scene.visual_candidates.iter().find(|c| c.local_path == path).map(|c| (c.width, c.height)).filter(|(w, h)| *w > 0 && *h > 0).unwrap_or(frame)
}

/// Ken Burns on the clip's Motion: `cover` is the scale that fills the frame; the move zooms
/// and pans around it.
fn camera_motion(s: &mut Session, clip_id: ClipId, duration: Tick, kind: &str, frame: (u32, u32), cover: f64) -> Result<()> {
    let cx = f64::from(frame.0) * 0.5;
    let cy = f64::from(frame.1) * 0.5;
    let dx = f64::from(frame.0) * 0.03;
    let dy = f64::from(frame.1) * 0.02;
    let (k0, k1, p0, p1) = match kind {
        "zoom_out" => (1.16, 1.03, Vec2::new(cx, cy - dy * 0.5), Vec2::new(cx, cy)),
        "pan_right" => (1.12, 1.12, Vec2::new(cx - dx, cy), Vec2::new(cx + dx, cy)),
        "pan_left" => (1.12, 1.12, Vec2::new(cx + dx, cy), Vec2::new(cx - dx, cy)),
        "dramatic_push" => (1.02, 1.20, Vec2::new(cx, cy), Vec2::new(cx, cy - dy)),
        "macro_punch_in" => (1.15, 1.28, Vec2::new(cx + dx * 0.6, cy - dy * 0.6), Vec2::new(cx, cy - dy * 0.2)),
        "vertical_card" => (1.04, 1.34, Vec2::new(cx + dx * 1.5, cy + dy * 0.5), Vec2::new(cx - dx * 1.5, cy - dy * 0.5)),
        _ => (1.02, 1.12, Vec2::new(cx, cy), Vec2::new(cx, cy)),
    };
    let (s0, s1) = (cover * k0, cover * k1);
    let end_t = Tick(duration.0.saturating_sub(1).max(1));
    s.edit_sequence("Kamera hareketi", |q, _, _| {
        if let Some((_, it)) = q.find_item_mut(clip_id)
            && let Some(motion) = it.effects.iter_mut().find(|e| e.effect == "motion")
        {
            if let Some(sp) = motion.params.get_mut("scale") {
                sp.value = ParamValue::Float(s0);
                sp.keyframes = vec![Keyframe::new(Tick::ZERO, ParamValue::Float(s0)), Keyframe::new(end_t, ParamValue::Float(s1))];
            }
            if let Some(pp) = motion.params.get_mut("position") {
                pp.value = ParamValue::Vec2(p0);
                pp.keyframes = vec![Keyframe::new(Tick::ZERO, ParamValue::Vec2(p0)), Keyframe::new(end_t, ParamValue::Vec2(p1))];
            }
        }
        Ok(())
    })
}

fn lumetri(s: &mut Session, clip: ClipId, preset: &str) {
    let _ = s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}));
    let (look, contrast, faded, sharpen, vig, feather) = match preset {
        "cold_tactical_map" => ("Cool Blue", 14.0, 0.0, 12.0, -1.0, 60.0),
        "vintage_archive_film" => ("Warm Film", 10.0, 14.0, 4.0, -1.6, 55.0),
        "cinematic_teal_orange" => ("Teal & Orange", 12.0, 4.0, 8.0, -1.1, 62.0),
        "clean_contrast" => ("None", 10.0, 0.0, 10.0, -0.7, 65.0),
        _ => ("Night", 14.0, 6.0, 8.0, -1.5, 60.0),
    };
    for (param, val) in [
        ("look", json!(look)),
        ("contrast", json!(contrast)),
        ("faded_film", json!(faded)),
        ("sharpen", json!(sharpen)),
        ("vignette_on", json!(true)),
        ("vignette_amount", json!(vig)),
        ("vignette_feather", json!(feather)),
    ] {
        let _ = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "lumetri", "param": param, "value": val}));
    }
}

fn grain(s: &mut Session, clip: ClipId, texture: &str, enabled: bool) {
    if !enabled || texture == "none" || texture.is_empty() {
        return;
    }
    let (amount, color) = match texture {
        "vhs_archive" => (7.0, true),
        "crt_classified" => (5.5, false),
        _ => (3.5, false),
    };
    let _ = s.execute("effects.apply", json!({"clips": [clip.0], "effect": "noise"}));
    let _ = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "noise", "param": "amount", "value": amount}));
    let _ = s.execute("effects.setParam", json!({"clip": clip.0, "effect": "noise", "param": "color", "value": color}));
}

/// Caption chunks weighted by characters and punctuation, timed across `[start, start+dur]`.
fn captions(s: &mut Session, text: &str, start: Tick, dur: Tick, viral: bool) {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return;
    }
    let max_words = if viral { 3 } else { 7 };
    let mut chunks: Vec<String> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let shown = if viral && (w.chars().any(|c| c.is_ascii_digit()) || w.chars().count() >= 8 || i == 0) { turkish::upper(w) } else { (*w).to_string() };
        cur.push(shown);
        let clause = w.ends_with(['.', ',', '?', '!', ':', ';']);
        if cur.len() >= max_words || (cur.len() >= 2 && clause) {
            chunks.push(cur.join(" "));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        chunks.push(cur.join(" "));
    }
    let weights: Vec<i64> = chunks
        .iter()
        .map(|c| {
            let bonus = if c.ends_with(['.', '?', '!']) {
                4
            } else if c.ends_with([',', ';', ':']) {
                2
            } else {
                0
            };
            c.chars().count().max(4) as i64 + bonus
        })
        .collect();
    let total: i64 = weights.iter().sum::<i64>().max(1);
    let mut t = start;
    let end = start + dur;
    for (c, w) in chunks.iter().zip(&weights) {
        let d = Tick((dur.0 as i128 * i128::from(*w) / i128::from(total)) as i64).max(Tick::from_seconds_f64(0.3));
        let d = if t + d > end { Tick((end.0 - t.0).max(1)) } else { d };
        let _ = s.execute("captions.add", json!({"seconds": t.seconds(), "durationSeconds": d.seconds(), "text": c}));
        t += d;
    }
}

/// Template + control values for a YouTuber overlay id.
fn overlay_template(id: &str, scene: &SceneSpec, p: &CrStudioPipeline) -> Option<(&'static str, Value)> {
    let text = if scene.info_text.is_empty() { scene.overlay_subtitle.clone() } else { scene.info_text.clone() };
    let source = scene.sources.first().and_then(|i| p.research.sources.get(*i)).map(|s| s.title.clone()).unwrap_or_default();
    Some(match id {
        "info_box" => ("builtin:yt-info-box", json!({"title": "BİLİYOR MUYDUNUZ?", "text": text})),
        "warning_box" => ("builtin:yt-warning-box", json!({"title": "DİKKAT", "text": text})),
        "speech_bubble" => ("builtin:yt-speech-bubble", json!({"text": text})),
        "comment_prompt" => {
            ("builtin:yt-comment-prompt", json!({"question": if text.is_empty() { "Sizce hangisi doğru? Yorumlarda yazın!".to_string() } else { text }}))
        }
        "like_reminder" => ("builtin:yt-like-reminder", json!({"text": "Beğenmeyi unutma!"})),
        "source_credit" => ("builtin:yt-source-credit", json!({"source": if source.is_empty() { text } else { format!("Kaynak: {source}") }})),
        "timeline_tag" => ("builtin:yt-timeline-tag", json!({"date": text})),
        "countdown" => ("builtin:yt-countdown", json!({"number": if text.is_empty() { format!("{}", scene.index + 1) } else { text }})),
        "shock_badge" => ("builtin:yt-shock-badge", json!({"text": if text.is_empty() { "İNANILMAZ!".to_string() } else { turkish::upper(&text) }})),
        _ => return None,
    })
}

fn doc_graphic_values(tpl: &str, scene: &SceneSpec) -> Value {
    let title = if scene.overlay_title.is_empty() { &scene.title } else { &scene.overlay_title };
    let sub = if scene.overlay_subtitle.is_empty() { super::truncate_line(&scene.narration_text, 60) } else { scene.overlay_subtitle.clone() };
    match tpl {
        "builtin:doc-map-pin" => {
            let coords = scene.map_spec.as_ref().map(|m| format!("{:.2}°, {:.2}°  •  {}", m.lat, m.lon, m.subtitle)).unwrap_or_else(|| sub.clone());
            json!({"location": turkish::upper(title), "coordinates": coords})
        }
        "builtin:doc-highlight-quote" => json!({"quote": title, "source": sub}),
        "builtin:doc-chapter-card" => {
            json!({"chapter": turkish::upper(if scene.chapter.is_empty() { "BÖLÜM" } else { &scene.chapter }), "title": turkish::upper(title), "subtitle": sub})
        }
        "builtin:shorts-dynamic-hook" => json!({"tag": "BUNU BİLİYOR MUYDUN?", "headline": turkish::upper(title)}),
        "builtin:atlas-lower-third" => json!({
            "kicker": turkish::upper(if scene.chapter.is_empty() { "EFSANELER" } else { &scene.chapter }),
            "title": turkish::upper(title),
            "body": sub
        }),
        _ => json!({"name": title, "role": sub}),
    }
}

/// Make sure every scene with a narration take has a fresh master (raw → master, idempotent).
fn master_voices(p: &mut CrStudioPipeline) {
    for sc in &mut p.scenes {
        let Some(raw) = sc.voiceover_raw_path.clone().or_else(|| sc.voiceover_path.clone()) else { continue };
        let raw_p = Path::new(&raw);
        if !raw_p.is_file() {
            sc.voiceover_path = None;
            sc.voiceover_duration_sec = None;
            continue;
        }
        if !p.voice_polish_enabled {
            sc.voiceover_path = Some(raw.clone());
            sc.voiceover_duration_sec = super::wav::duration(raw_p).or(sc.voiceover_duration_sec);
            continue;
        }
        let master = raw_p.with_file_name(format!(
            "{}_master.wav",
            raw_p.file_stem().map(|s| s.to_string_lossy().trim_end_matches("_master").to_string()).unwrap_or_default()
        ));
        let fresh = master.is_file() && std::fs::metadata(&master).and_then(|m| m.modified()).ok() >= std::fs::metadata(raw_p).and_then(|m| m.modified()).ok();
        if sc.voiceover_raw_path.is_none() {
            sc.voiceover_raw_path = Some(raw.clone());
        }
        if fresh {
            sc.voiceover_path = Some(master.to_string_lossy().into_owned());
            sc.voiceover_duration_sec = super::wav::duration(&master).or(sc.voiceover_duration_sec);
        } else if let Ok(d) = voice::master(raw_p, &master) {
            sc.voiceover_path = Some(master.to_string_lossy().into_owned());
            sc.voiceover_duration_sec = Some(d);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    Main,
    Shorts,
}

fn build(s: &mut Session, cut: Cut) -> Result<Value> {
    if s.crstudio.worker_running {
        return Err(EngineError::Other("Ajanlar çalışırken kurgu kurulamaz".into()));
    }
    if s.crstudio.pipeline.scenes.is_empty() {
        return Err(EngineError::Other("Kurgulanacak sahne yok".into()));
    }
    master_voices(&mut s.crstudio.pipeline);
    let p = s.crstudio.pipeline.clone();
    let format = if cut == Cut::Shorts { VideoFormatPreset::YoutubeShorts } else { p.format };
    let frame = format.dimensions();
    let vertical = format.is_vertical();
    let title = if p.title.trim().is_empty() { "CrStudio".to_string() } else { p.title.trim().to_string() };
    let channel = if p.channel_name.trim().is_empty() { "CRSTUDIO".to_string() } else { turkish::upper(p.channel_name.trim()) };
    let viral = vertical || p.caption_style_mode == "viral_word_highlight";

    // Scenes in this cut.
    let picked: Vec<usize> = if cut == Cut::Shorts {
        let mut acc = 0.0;
        let mut v = Vec::new();
        for sc in &p.scenes {
            if acc + sc.effective_duration() > 58.0 && !v.is_empty() {
                break;
            }
            acc += sc.effective_duration();
            v.push(sc.index);
        }
        v
    } else {
        (0..p.scenes.len()).collect()
    };
    let n = picked.len();
    let seq_name = match cut {
        Cut::Main => format!("{title} ({})", format.aspect_label()),
        Cut::Shorts => format!("{title} — Shorts"),
    };

    let result = s.edit_group(if cut == Cut::Main { "CrStudio kurgusu" } else { "CrStudio Shorts kesiti" }, |s| {
        if cut == Cut::Main {
            let pr = Arc::make_mut(&mut s.project);
            pr.name = title.clone();
        }
        let vis_bin = ensure_named_bin(s, "01 Görseller ve Haritalar")?;
        let vo_bin = ensure_named_bin(s, "02 Seslendirme")?;
        let sfx_bin = ensure_named_bin(s, "03 Ses Efektleri ve Müzik")?;
        let r = s.execute(
            "file.newSequence",
            json!({"name": seq_name, "width": frame.0, "height": frame.1, "fps": 30.0, "sampleRate": 48000, "video": 4, "audio": 3}),
        )?;
        let seq_id = r["sequence"].as_u64().map(ItemId).or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
        if let Some(q) = Arc::make_mut(&mut s.project).sequence_mut(seq_id) {
            for (i, name) in ["V1 · Görüntü", "V2 · Belgesel grafikleri", "V3 · YouTuber eklentileri", "V4 · Kanal kimliği"].iter().enumerate() {
                if let Some(t) = q.video_tracks.get_mut(i) {
                    t.name = (*name).into();
                }
            }
            for (i, name) in ["A1 · Seslendirme", "A2 · Ses efektleri", "A3 · Müzik"].iter().enumerate() {
                if let Some(t) = q.audio_tracks.get_mut(i) {
                    t.name = (*name).into();
                }
            }
        }
        let _ = s.execute("captions.newTrack", json!({"name": "Altyazı", "format": "Subtitle", "language": "tr"}));
        let track = |s: &Session, video: bool, i: usize| -> Option<u64> {
            s.active_sequence().and_then(|q| if video { q.video_tracks.get(i).map(|t| t.id.0) } else { q.audio_tracks.get(i).map(|t| t.id.0) })
        };
        let v1 = track(s, true, 0).ok_or(EngineError::NoSequence)?;
        // graphics go by track index (V2 = 1, V3 = 2, V4 = 3); see `apply_gfx`
        let (v2, v3, v4) = (1u64, 2u64, 3u64);
        let a1 = track(s, false, 0).ok_or(EngineError::NoSequence)?;
        let a2 = track(s, false, 1).unwrap_or(a1);
        let a3 = track(s, false, 2).unwrap_or(a1);

        let mut cursor = Tick::ZERO;
        let mut cuts: Vec<f64> = Vec::new();
        let mut v1_clips: Vec<(ClipId, String)> = Vec::new();
        let mut vo_clips: Vec<ClipId> = Vec::new();
        let mut sfx_clips: Vec<ClipId> = Vec::new();
        let mut overlays_placed = 0usize;
        let mut like_done = false;
        // the cut checks itself: every narration whole, every rendered shot filling its slot
        let mut qa: Vec<String> = Vec::new();
        let total_secs: f64 = picked.iter().filter_map(|i| p.scenes.get(*i)).map(SceneSpec::effective_duration).sum();

        for (k, &si) in picked.iter().enumerate() {
            let Some(scene) = p.scenes.get(si) else { continue };
            // the long cut breathes between scenes (and longer between chapters); Shorts run tight
            let pause = if cut == Cut::Main { audio::scene_pause(scene, picked.get(k + 1).and_then(|j| p.scenes.get(*j))) } else { 0.0 };
            let dur = Tick::from_seconds_f64(scene.effective_duration() + pause);
            let at = cursor;

            // V1 pictures
            // our own rendered cards and maps already carry their titles and are drawn 16:9
            let drawn = scene.selected_visual.as_deref().and_then(|p| scene.visual_candidates.iter().find(|c| c.local_path == p)).is_some_and(|c| {
                c.id.starts_with("card_") || c.id.starts_with("map_") || matches!(c.kind.as_str(), "chapter_card" | "evidence_highlight" | "infographic_chart" | "animated_map" | "blender")
            });
            // a rendered Blender shot already moves: no Ken Burns, no grade, no grain
            let rendered = scene.selected_visual.as_deref().and_then(|p| scene.visual_candidates.iter().find(|c| c.local_path == p)).is_some_and(|c| c.kind == "blender");
            if let Some(path) = scene.selected_visual.clone().filter(|p| Path::new(p).is_file()) {
                let item = import_path(s, &path, vis_bin)?;
                // the decoded file's own size (candidate metadata can be the original's, not the download's)
                let size_of = |s: &Session, it: ItemId, path: &str| s.project.source_size(it).filter(|(w, h)| *w > 0 && *h > 0).unwrap_or_else(|| source_size(scene, path, frame));
                // drawn 16:9 cards in a vertical cut are fitted whole, not cropped
                let fit = |src: (u32, u32)| if drawn && cut == Cut::Shorts { contain_percent(src, frame) } else { cover_percent(src, frame) };
                let cover = fit(size_of(s, item, &path));
                let second = scene
                    .visual_candidates
                    .iter()
                    .find(|c| c.local_path != path && (c.kind == "wikimedia_commons" || c.kind == "openverse") && c.relevance.is_none_or(|r| r >= 55.0) && Path::new(&c.local_path).is_file())
                    .map(|c| c.local_path.clone());
                let split = p.broll_pacing_enabled && scene.broll_subcuts >= 2 && scene.effective_duration() >= 6.0 && (cut == Cut::Main || !drawn) && !rendered;
                let parts: Vec<(String, Tick, Tick, String)> = if split {
                    let a = Tick((dur.0 as i128 * 56 / 100) as i64);
                    let b_path = second.unwrap_or_else(|| path.clone());
                    vec![(path.clone(), at, a, scene.camera_motion.clone()), (b_path, at + a, Tick(dur.0 - a.0), "macro_punch_in".into())]
                } else {
                    vec![(path.clone(), at, dur, if cut == Cut::Shorts { "macro_punch_in".into() } else { scene.camera_motion.clone() })]
                };
                for (pi, (ppath, pat, pdur, motion)) in parts.into_iter().enumerate() {
                    let it = if ppath == path { item } else { import_path(s, &ppath, vis_bin)? };
                    let placed = s.execute("timeline.place", json!({"item": it.0, "track": v1, "time": pat.0, "duration": pdur.0, "video": true, "audio": false}))?;
                    if let Some(cid) = placed["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).map(ClipId) {
                        let src = size_of(s, it, &ppath);
                        let cov = fit(src);
                        if drawn && cut == Cut::Shorts {
                            // vertical cut of a 16:9 card: a blurred, darkened full-frame copy behind
                            // (V1) and the whole card on top (V2)
                            let _ = camera_motion(s, cid, pdur, "slow_push", frame, cover_percent(src, frame));
                            let _ = s.execute("effects.apply", json!({"clips": [cid.0], "effect": "gaussian_blur"}));
                            let _ = s.execute("effects.setParam", json!({"clip": cid.0, "effect": "gaussian_blur", "param": "blurriness", "value": 60.0}));
                            let _ = s.execute("effects.apply", json!({"clips": [cid.0], "effect": "lumetri"}));
                            let _ = s.execute("effects.setParam", json!({"clip": cid.0, "effect": "lumetri", "param": "exposure", "value": -1.2}));
                            if let Ok(top) = s.execute("timeline.place", json!({"item": it.0, "track": "V2", "time": pat.0, "duration": pdur.0, "video": true, "audio": false}))
                                && let Some(tc) = top["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).map(ClipId)
                            {
                                if rendered {
                                    // a Blender shot moves by itself; shown whole so its captions stay in frame
                                    let _ = s.execute("effects.setParam", json!({"clip": tc.0, "effect": "motion", "param": "scale", "value": cov}));
                                } else {
                                    // a still card: a clear, steady push (a slow one reads as a frozen frame)
                                    let _ = camera_motion(s, tc, pdur, "vertical_card", frame, cov);
                                }
                            }
                        } else if rendered {
                            let _ = s.execute("effects.setParam", json!({"clip": cid.0, "effect": "motion", "param": "scale", "value": cov}));
                            // a shot rendered for the scene without its pause: slow it a touch to fill
                            // the slot (never hold a frozen last frame)
                            let media = media_seconds(s, it);
                            let slot = pdur.seconds();
                            if media > 1.0 && media < slot {
                                let pct = (media / slot * 100.0).clamp(75.0, 100.0);
                                let _ = s.execute("clip.speedDuration", json!({"clips": [cid.0], "speed": pct, "ripple": false}));
                                if media / (pct / 100.0) < slot - 0.05 {
                                    qa.push(format!("Sahne {}: Blender çekimi ({media:.1} sn) sahneyi doldurmuyor ({slot:.1} sn); yeniden çizin", si + 1));
                                }
                            }
                        } else {
                            let _ = camera_motion(s, cid, pdur, &motion, frame, if pi == 0 { cover } else { cov });
                            lumetri(s, cid, &scene.visual_effect);
                            grain(s, cid, &scene.film_texture, p.film_grain_enabled);
                        }
                        v1_clips.push((cid, if pi == 0 { scene.video_transition.clone() } else { "cross_dissolve".into() }));
                    }
                }
            }

            // V2 documentary graphic (first ≤5 s); a drawn card already shows its title
            if !drawn {
                let tpl = if p.visual_concept == "atlas" && (scene.overlay_template.is_empty() || scene.overlay_template == "builtin:lower-third-rule") {
                    "builtin:atlas-lower-third".to_string()
                } else if scene.overlay_template.is_empty() {
                    "builtin:lower-third-rule".to_string()
                } else {
                    scene.overlay_template.clone()
                };
                let gd = Tick::from_seconds_f64(scene.effective_duration().clamp(2.0, 5.0));
                let _ = apply_gfx(s, json!({"template": tpl, "track": v2, "time": at.0, "duration": gd.0, "values": doc_graphic_values(&tpl, scene)}));
            }

            // V3 YouTuber overlays
            if p.youtuber_overlays {
                let mut ids: Vec<String> = scene.overlays.clone();
                if !like_done && cut == Cut::Main && total_secs > 0.0 && cursor.seconds() >= total_secs * 0.38 && !ids.iter().any(|o| o == "like_reminder") {
                    ids.push("like_reminder".into());
                }
                if k + 1 == n && !ids.iter().any(|o| o == "comment_prompt") && !p.social_meta.pinned_comment.is_empty() {
                    ids.push("comment_prompt".into());
                }
                let slot = scene.effective_duration() / ids.len().max(1) as f64;
                // the Shorts end card owns the last seconds: overlays stop before it (no overlap)
                let cta_from = (p.brand_kit_enabled && cut == Cut::Shorts && k + 1 == n).then(|| (at + dur).seconds() - 3.5f64.min(scene.effective_duration()));
                for (oi, id) in ids.iter().enumerate() {
                    if id == "like_reminder" {
                        like_done = true;
                    }
                    let mut sc2 = scene.clone();
                    if id == "comment_prompt" && sc2.info_text.is_empty() {
                        sc2.info_text = p.social_meta.pinned_comment.clone();
                    }
                    if let Some((tid, vals)) = overlay_template(id, &sc2, &p) {
                        let start = at + Tick::from_seconds_f64(0.5 + slot * oi as f64);
                        let mut d_s = (slot - 0.6).clamp(1.5, 4.5);
                        if let Some(cf) = cta_from {
                            d_s = d_s.min(cf - 0.2 - start.seconds());
                            if d_s < 1.0 {
                                continue;
                            }
                        }
                        let d = Tick::from_seconds_f64(d_s);
                        if apply_gfx(s, json!({"template": tid, "track": v3, "time": start.0, "duration": d.0, "values": vals})).is_ok() {
                            overlays_placed += 1;
                        }
                    }
                }
            }

            // V4 channel kit
            if p.brand_kit_enabled {
                if k == 1 && cut == Cut::Main {
                    let _ = apply_gfx(
                        s,
                        json!({"template": "builtin:youtube-subscribe-cta", "track": v4, "time": (at + Tick::from_seconds_f64(0.8)).0, "duration": Tick::from_seconds_f64(4.0).0,
                               "values": {"channel": channel, "cta": "ABONE OL", "note": "BİLDİRİMLERİ AÇ"}}),
                    );
                }
                if k + 1 == n {
                    let (tid, vals, d) = if cut == Cut::Shorts {
                        ("builtin:youtube-subscribe-cta", json!({"channel": channel, "cta": "ABONE OL", "note": "DEVAMI UZUN VİDEODA"}), 3.5)
                    } else {
                        ("builtin:yt-watch-next", json!({"heading": "BUNU DA İZLE", "channel": channel}), 15.0_f64.min(scene.effective_duration().max(5.0)))
                    };
                    let start = at + dur - Tick::from_seconds_f64(d.min(scene.effective_duration()));
                    let _ = apply_gfx(s, json!({"template": tid, "track": v4, "time": start.0, "duration": Tick::from_seconds_f64(d).0, "values": vals}));
                }
            }

            // A1 narration
            if let Some(vo) = scene.voiceover_path.clone().filter(|v| Path::new(v).is_file())
                && let Ok(item) = import_path(s, &vo, vo_bin)
            {
                let placed = s.execute("timeline.place", json!({"item": item.0, "audioTrack": a1, "time": at.0, "video": false, "audio": true}))?;
                if let Some(c) = placed["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).map(ClipId) {
                    vo_clips.push(c);
                }
                let on_timeline = media_seconds(s, item);
                if let Some(want) = scene.voiceover_duration_sec.filter(|w| (w - on_timeline).abs() > 0.3) {
                    qa.push(format!("Sahne {}: seslendirme {on_timeline:.1} sn yerleşti, dosya {want:.1} sn", si + 1));
                }
            }

            // A2 SFX (J-cut: 0.25 s before the cut)
            if p.synth_sfx && !scene.sfx_cue.is_empty() && scene.sfx_cue != "none" && !p.project_dir.is_empty() {
                let path = Path::new(&p.project_dir).join("sfx").join(format!("sfx_{}.wav", scene.sfx_cue));
                if !path.is_file() {
                    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
                    let _ = filmcraft_format::atomic_write(&path, &audio::generate_sfx_wav(&scene.sfx_cue));
                }
                let ps = path.to_string_lossy().into_owned();
                if let Ok(item) = import_path(s, &ps, sfx_bin) {
                    let t = if p.j_l_cut_enabled && k > 0 { Tick(at.0.saturating_sub(Tick::from_seconds_f64(0.25).0).max(0)) } else { at };
                    if let Ok(placed) = s.execute("timeline.place", json!({"item": item.0, "audioTrack": a2, "time": t.0, "video": false, "audio": true}))
                        && let Some(c) = placed["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).map(ClipId)
                    {
                        sfx_clips.push(c);
                        let _ = s.execute("clip.gain", json!({"clips": [c.0], "setDb": -9.0}));
                    }
                }
            }

            // C1 captions
            captions(s, &scene.narration_text, at, dur, viral);

            // Chapter marker at chapter starts
            let is_chapter_start = k == 0 || p.scenes.get(si.wrapping_sub(1)).is_none_or(|prev| prev.chapter != scene.chapter);
            if cut == Cut::Main && is_chapter_start {
                let name = if scene.chapter.is_empty() { scene.title.clone() } else { scene.chapter.clone() };
                let _ = s.edit_sequence("Bölüm işareti", |q, ctx, _| {
                    q.markers.push(Marker { id: MarkerId(ctx.alloc()), start: at, duration: Tick::ZERO, name, comment: String::new(), color: Label::Cerulean, kind: MarkerKind::Chapter });
                    Ok(())
                });
            }

            cursor = at + dur;
            if k + 1 < n {
                cuts.push(cursor.seconds());
            }
        }

        // Transitions
        let mut transitions = 0usize;
        for (i, (cid, tr)) in v1_clips.iter().enumerate() {
            let eff = if i == 0 { "dip_to_black" } else if tr.is_empty() { "film_dissolve" } else { tr.as_str() };
            if s.execute("sequence.applyVideoTransition", json!({"clip": cid.0, "edge": "in", "effect": eff, "frames": if i == 0 { 18 } else { 14 }})).is_ok() {
                transitions += 1;
            }
        }
        if let Some((last, _)) = v1_clips.last() {
            let _ = s.execute("sequence.applyVideoTransition", json!({"clip": last.0, "edge": "out", "effect": "dip_to_black", "frames": 24}));
        }
        for c in vo_clips.iter().skip(1) {
            let _ = s.execute("sequence.applyAudioTransition", json!({"clip": c.0, "edge": "in", "effect": "constant_power", "frames": if p.j_l_cut_enabled { 10 } else { 6 }}));
        }

        // Caption style
        if let Some(q) = Arc::make_mut(&mut s.project).sequence_mut(seq_id)
            && let Some(ct) = q.caption_tracks.first_mut()
        {
            ct.style = CaptionStyle {
                font: "Inter".into(),
                size: if viral { 60.0 } else { 42.0 },
                align: CaptionAlign::Center,
                color: if viral { [250, 204, 21, 255] } else { [255, 255, 255, 255] },
                background: true,
                background_color: [10, 12, 18, 210],
                ..Default::default()
            };
        }

        // Essential Sound + ducked score
        if !vo_clips.is_empty() {
            let ids: Vec<u64> = vo_clips.iter().map(|c| c.0).collect();
            let _ = s.execute("essentialSound.setType", json!({"clips": ids, "type": "dialogue"}));
        }
        if !sfx_clips.is_empty() {
            let ids: Vec<u64> = sfx_clips.iter().map(|c| c.0).collect();
            let _ = s.execute("essentialSound.setType", json!({"clips": ids, "type": "sfx"}));
        }
        let total = cursor.seconds().max(4.0);
        let library = if p.bgm_style == "files" { audio::music_files(&p.project_dir) } else { Vec::new() };
        if !library.is_empty() {
            // the project's own (licensed) music, track after track until the cut is covered,
            // crossfaded, faded out at the end, ducked under the narration
            let mut t = 0.0_f64;
            let mut music: Vec<u64> = Vec::new();
            for (k, path) in library.iter().cycle().take(32).enumerate() {
                if t >= total - 0.5 {
                    break;
                }
                let Ok(item) = import_path(s, path, sfx_bin) else { continue };
                let len = media_seconds(s, item);
                if len < 5.0 {
                    if k >= library.len() {
                        break;
                    }
                    continue;
                }
                let d = len.min(total - t);
                let placed = s.execute(
                    "timeline.place",
                    json!({"item": item.0, "audioTrack": a3, "time": Tick::from_seconds_f64(t).0, "duration": Tick::from_seconds_f64(d).0, "video": false, "audio": true}),
                )?;
                if let Some(c) = placed["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64) {
                    if !music.is_empty() {
                        let _ = s.execute("sequence.applyAudioTransition", json!({"clip": c, "edge": "in", "effect": "constant_power", "frames": 60}));
                    }
                    music.push(c);
                }
                t += d;
            }
            if let Some(last) = music.last() {
                let _ = s.execute("sequence.applyAudioTransition", json!({"clip": last, "edge": "out", "effect": "constant_power", "frames": 90}));
            }
            if !music.is_empty() {
                let _ = s.execute("clip.gain", json!({"clips": music, "setDb": p.bgm_volume_db}));
                let _ = s.execute("essentialSound.setType", json!({"clips": music, "type": "music"}));
                if p.bgm_auto_duck && !vo_clips.is_empty() {
                    // music comes up in the pauses between scenes and settles under the voice
                    let _ = s.execute("essentialSound.set", json!({"clips": music, "values": {"ducking.enabled": true, "ducking.reduceDb": -10.0, "ducking.fadeS": 0.5}}));
                    let _ = s.execute("essentialSound.generateDucking", json!({"clips": music}));
                }
            }
        } else if !matches!(p.bgm_style.as_str(), "none" | "files") && !p.project_dir.is_empty() {
            let path = Path::new(&p.project_dir).join("sfx").join(format!("score_{}_{}.wav", p.bgm_style, if cut == Cut::Main { "uzun" } else { "shorts" }));
            let bytes = audio::generate_ambient_music_wav(total + 1.0, &p.bgm_style, &cuts);
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
            if filmcraft_format::atomic_write(&path, &bytes).is_ok() {
                super::write_attribution_sidecar(&path, audio::bgm_label(&p.bgm_style), "CrStudio score synthesizer", "original work", "Özgün üretim (kendi)", "Score");
                let ps = path.to_string_lossy().into_owned();
                // a fresh import (the file changed)
                if let Ok(item) = crate::commands::import_bytes(s, &ps, bytes.into(), Some(sfx_bin)) {
                    let placed = s.execute("timeline.place", json!({"item": item.0, "audioTrack": a3, "time": 0, "duration": Tick::from_seconds_f64(total).0, "video": false, "audio": true}))?;
                    if let Some(bg) = placed["clips"].as_array().and_then(|a| a.first()).and_then(Value::as_u64) {
                        let _ = s.execute("clip.gain", json!({"clips": [bg], "setDb": p.bgm_volume_db}));
                        let _ = s.execute("essentialSound.setType", json!({"clips": [bg], "type": "music"}));
                        if p.bgm_auto_duck && !vo_clips.is_empty() {
                            let _ = s.execute("essentialSound.set", json!({"clips": [bg], "values": {"ducking.enabled": true, "ducking.reduceDb": -12.0, "ducking.fadeS": 0.6}}));
                            let _ = s.execute("essentialSound.generateDucking", json!({"clips": [bg]}));
                        }
                    }
                }
            }
        }
        s.set_playhead(Tick::ZERO);
        s.events.push(Event::OpenSequence(seq_id));
        Ok(json!({
            "sequence": seq_id.0,
            "scenesPlaced": n,
            "totalDurationSeconds": total,
            "videoClips": v1_clips.len(),
            "voiceoverClips": vo_clips.len(),
            "sfxClips": sfx_clips.len(),
            "overlays": overlays_placed,
            "videoTransitions": transitions,
            "qa": qa,
            "width": frame.0,
            "height": frame.1,
        }))
    })?;

    let seq = result["sequence"].as_u64();
    let pl = &mut s.crstudio.pipeline;
    match cut {
        Cut::Main => {
            pl.assembled_sequence = seq;
            pl.stage = PipelineStage::InStudio;
            pl.set_agent_status(
                7,
                AgentStatus::Done,
                format!("{} sahne, {} eklenti, {:.0} sn", n, result["overlays"], result["totalDurationSeconds"].as_f64().unwrap_or(0.0)),
            );
            pl.push_log("Kurgucu", "ok", format!("Kurgu hazır: {n} sahne, {} YouTuber eklentisi, {} geçiş", result["overlays"], result["videoTransitions"]));
        }
        Cut::Shorts => {
            pl.shorts_sequence = seq;
            pl.push_log("Kurgucu", "ok", format!("Shorts kesiti hazır: {n} sahne, {:.0} sn", result["totalDurationSeconds"].as_f64().unwrap_or(0.0)));
        }
    }
    match result["qa"].as_array().filter(|a| !a.is_empty()) {
        Some(list) => {
            for w in list.iter().filter_map(Value::as_str) {
                pl.push_log("Kurgu denetimi", "warn", w.to_string());
            }
        }
        None => pl.push_log("Kurgu denetimi", "ok", "Seslendirmeler tam, çekimler sahneleri dolduruyor".to_string()),
    }
    let r = super::director::review(pl);
    pl.director = Some(r);
    // save after either cut, so reopening the project brings back both sequences
    if !pl.project_dir.is_empty() {
        let file = Path::new(&pl.project_dir).join(format!("{}.fcproj", turkish::slug(&pl.title))).to_string_lossy().into_owned();
        let _ = s.execute("file.saveAs", json!({"path": file}));
    }
    Ok(result)
}

/// `crstudio.assembleTimeline`.
pub fn assemble_timeline(s: &mut Session, _p: &Value) -> Result<Value> {
    build(s, Cut::Main)
}

/// `crstudio.createShortsCut`: up to ~58 s from the start (the hook and first beats), vertical.
pub fn create_shorts_cut(s: &mut Session, _p: &Value) -> Result<Value> {
    build(s, Cut::Shorts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_scale() {
        assert!((cover_percent((1920, 1080), (1920, 1080)) - 100.0).abs() < 1e-9);
        assert!((cover_percent((1920, 1080), (1080, 1920)) - 177.777).abs() < 0.01);
        assert_eq!(cover_percent((0, 0), (1080, 1920)), 100.0);
    }
}
