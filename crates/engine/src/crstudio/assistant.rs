//! CrStudio Asistan: Claude looks at the playhead frame, the In/Out range and the timeline, and
//! proposes steps (allow-listed CrStudio commands) that the user applies one by one or all at once.
//! Everything it does is a normal, undoable edit.
//!
//! Commands: `crstudio.assistant.ask {text}`, `.state`, `.apply {id?|all?}`, `.dismiss {id?}`,
//! `.cancel`, `.clear`, `.context`, `.overlay {template,startSec,endSec|durationSec,values}`,
//! `.addSfx {kind?|query?,atSec?,gainDb?}`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use filmcraft_project::gtemplate::builtin_templates;
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{assemble, audio, claude, truncate_line, visuals, write_attribution_sidecar};
use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, str_p};
use crate::{EngineError, Result, Session};

pub const MAX_LOG: usize = 300;
pub const MAX_STEPS: usize = 24;
const MAX_TURNS: usize = 6;

/// Commands the assistant may run (prefix match). Everything here is an undoable edit.
const ALLOWED: &[&str] = &[
    "crstudio.assistant.overlay",
    "crstudio.assistant.addSfx",
    "graphics.template.apply",
    "graphics.template.set",
    "clip.",
    "marker",
    "captions.",
    "sequence.applyVideoTransition",
    "sequence.applyAudioTransition",
    "effects.",
    "effect.",
    "essentialSound.",
    "playhead.set",
];

pub const SFX_KINDS: &[&str] = &["whoosh", "low_boom", "riser", "mystery_riser", "map_ping", "archive_shutter"];

pub fn allowed(cmd: &str) -> bool {
    ALLOWED.iter().any(|p| cmd.starts_with(p))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantStep {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub seconds: Option<f64>,
    /// `bekliyor`, `uygulandı`, `hata: …`
    #[serde(default)]
    pub status: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AssistantState {
    pub busy: bool,
    /// Bumped on every change so the UI copies only when something moved.
    pub version: u64,
    pub log: Vec<(String, String)>,
    pub steps: Vec<AssistantStep>,
    /// The last exchanges (user, assistant), sent back as conversation memory.
    pub turns: Vec<(String, String)>,
    next_id: u64,
    #[serde(skip)]
    pub cancel: Arc<AtomicBool>,
}

impl AssistantState {
    pub fn push(&mut self, role: &str, text: &str) {
        self.log.push((role.to_string(), truncate_line(text, 4000)));
        if self.log.len() > MAX_LOG {
            let n = self.log.len() - MAX_LOG;
            self.log.drain(..n);
        }
        self.version = self.version.wrapping_add(1);
    }
}

pub type Shared = Arc<Mutex<AssistantState>>;

pub fn lock(a: &Shared) -> MutexGuard<'_, AssistantState> {
    a.lock().unwrap_or_else(PoisonError::into_inner)
}

fn other(e: impl Into<String>) -> EngineError {
    EngineError::Other(e.into())
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// What the assistant knows about the edit right now.
pub fn context(s: &Session) -> Value {
    let mut v = json!({"playheadSec": r2(s.playhead().seconds())});
    if let Some(q) = s.active_sequence() {
        let items = |tracks: &[filmcraft_project::Track], prefix: &str| -> Vec<Value> {
            tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let list: Vec<Value> = t
                        .items
                        .iter()
                        .take(40)
                        .map(|it| json!({"clip": it.id.0, "name": truncate_line(&it.name, 60), "startSec": r2(it.start.seconds()), "endSec": r2(it.end().seconds())}))
                        .collect();
                    json!({"track": format!("{prefix}{}", i + 1), "items": list, "more": t.items.len().saturating_sub(40)})
                })
                .collect()
        };
        v["sequence"] = json!({
            "width": q.settings.width, "height": q.settings.height, "durationSec": r2(q.duration().seconds()),
            "inSec": q.mark_in.map(|t| r2(t.seconds())), "outSec": q.mark_out.map(|t| r2(t.seconds())),
            "video": items(&q.video_tracks, "V"), "audio": items(&q.audio_tracks, "A"),
            "markers": q.markers.iter().take(40).map(|m| json!({"name": m.name, "sec": r2(m.start.seconds())})).collect::<Vec<_>>(),
        });
        v["selection"] = json!(s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>());
    }
    let p = &s.crstudio.pipeline;
    if !p.title.is_empty() {
        v["crstudio"] = json!({
            "title": p.title, "channel": p.channel_name, "format": p.format.label(),
            "scenes": p.scenes.iter().take(64).map(|sc| json!({"n": sc.index + 1, "title": sc.title, "chapter": sc.chapter, "narration": truncate_line(&sc.narration_text, 140)})).collect::<Vec<_>>(),
        });
    }
    v["templates"] = json!(builtin_templates()
        .into_iter()
        .map(|t| json!({"id": t.id, "name": t.name, "about": truncate_line(&t.description, 90), "controls": t.controls.iter().map(|c| c.id.clone()).collect::<Vec<_>>()}))
        .collect::<Vec<_>>());
    v["sfxKinds"] = json!(SFX_KINDS);
    v
}

const SYSTEM: &str = "Sen CrStudio video düzenleyicisinin Türkçe kurgu asistanısın. Kullanıcı bir YouTube belgeseli kurguluyor. \
Görevin isteği anlamak ve CrStudio komutlarından oluşan, kullanıcının tek tek onaylayacağı adımlar önermek. Kendin hiçbir şey uygulamazsın. \
YALNIZCA şu JSON'u döndür (başka metin yok): {\"reply\": \"kısa Türkçe açıklama\", \"steps\": [{\"label\": \"adımın Türkçe kısa adı\", \"command\": \"komut\", \"params\": {...}, \"seconds\": başlangıç_saniyesi}]}.\n\
Kullanabileceğin komutlar:\n\
- crstudio.assistant.overlay {\"template\": \"builtin:…\", \"startSec\": s, \"endSec\": s, \"values\": {kontrol_id: değer}}: bir aralığa grafik/kuşak/balon/uyarı/abone ol vb. koyar (her zaman en üst boş ize). Renkler \"#RRGGBB\" metni.\n\
- crstudio.assistant.addSfx {\"kind\": \"whoosh|low_boom|riser|mystery_riser|map_ping|archive_shutter\"} veya {\"query\": \"İngilizce arama, ör. door creak\"}, \"atSec\": s, \"gainDb\": -9}: efekt sesi ekler (query: Openverse'ten CC0/CC BY ses indirir).\n\
- clip.gain {\"clips\": [id], \"setDb\": dB}, markers.add {\"seconds\": s, \"name\": \"…\"}, playhead.set {\"seconds\": s}.\n\
Kurallar: Zamanlar saniye. Aralık verilmemişse In/Out'u, o da yoksa oynatma kafasından itibaren makul bir süreyi kullan. \
Yalnızca bağlamdaki şablon kimliklerini ve kontrol adlarını kullan. Reklam/sponsor kuşaklarında builtin:yt-ad-band kullan ve label alanına \"REKLAM\" yaz (YouTube ücretli tanıtım bildirimi gerekir; reply'da hatırlat). \
Kare görseli verilmişse Read ile bakıp içeriğe göre karar ver. Karedeki bir nesneyi silme (inpainting) henüz yok: bunu dürüstçe söyle ve alternatif öner (üstünü bilgi kutusuyla kapatmak, kırpma/zoom, sahneyi değiştirmek). \
Emin olmadığın şeyi uydurma; gerekirse steps boş bırakıp reply'da soru sor.";

fn prompt(user: &str, ctx: &Value, frame: Option<&Path>, turns: &[(String, String)]) -> String {
    let mut out = String::new();
    if !turns.is_empty() {
        out.push_str("Önceki konuşma:\n");
        for (u, a) in turns {
            out.push_str(&format!("Kullanıcı: {u}\nAsistan: {a}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("Kurgu bağlamı (JSON):\n{}\n\n", serde_json::to_string(ctx).unwrap_or_default()));
    if let Some(f) = frame {
        out.push_str(&format!("Oynatma kafasındaki kare: {} (Read ile bakabilirsin)\n\n", f.display()));
    }
    out.push_str(&format!("İstek: {user}\n"));
    out
}

fn work_dir(s: &Session) -> PathBuf {
    let d = &s.crstudio.pipeline.project_dir;
    let base = if d.is_empty() { crate::temp_dir().join("crstudio-asistan") } else { Path::new(d).join("asistan") };
    let _ = std::fs::create_dir_all(&base);
    base
}

/// Parse Claude's answer into a reply and checked steps.
pub fn parse_answer(raw: &str, next_id: &mut u64) -> (String, Vec<AssistantStep>) {
    let Some(v) = claude::extract_json(raw) else {
        return (truncate_line(raw, 2000), Vec::new());
    };
    let reply = v.get("reply").and_then(Value::as_str).unwrap_or("").to_string();
    let mut steps = Vec::new();
    for st in v.get("steps").and_then(Value::as_array).into_iter().flatten().take(MAX_STEPS) {
        let command = st.get("command").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if command.is_empty() {
            continue;
        }
        *next_id = next_id.wrapping_add(1);
        let params = st.get("params").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        let seconds = st
            .get("seconds")
            .and_then(Value::as_f64)
            .or_else(|| params.get("startSec").and_then(Value::as_f64))
            .or_else(|| params.get("atSec").and_then(Value::as_f64))
            .or_else(|| params.get("seconds").and_then(Value::as_f64))
            .filter(|v| v.is_finite() && *v >= 0.0);
        let label = st.get("label").and_then(Value::as_str).map(|l| truncate_line(l, 160)).unwrap_or_else(|| command.clone());
        let status = if allowed(&command) { "bekliyor".to_string() } else { format!("hata: {command} asistana açık değil") };
        steps.push(AssistantStep { id: format!("a{next_id}"), label, command, params, seconds, status });
    }
    (reply, steps)
}

fn ask(s: &mut Session, v: &Value) -> Result<Value> {
    let text = str_p(v, "text").map(str::trim).unwrap_or("").chars().take(4000).collect::<String>();
    if text.is_empty() {
        return Err(bad("crstudio.assistant.ask", "boş istek"));
    }
    let shared = s.crstudio.assistant.clone();
    {
        let a = lock(&shared);
        if a.busy {
            return Err(bad("crstudio.assistant.ask", "asistan hâlâ çalışıyor"));
        }
    }
    let ctx = context(s);
    let dir = work_dir(s);
    // the frame under the playhead, so Claude can see it
    let frame = dir.join("kare.png");
    let _ = std::fs::remove_file(&frame);
    let frame = s.execute("file.exportFrame", json!({"path": frame.to_string_lossy()})).ok().map(|_| frame).filter(|f| f.is_file());
    let turns = {
        let mut a = lock(&shared);
        a.push("user", &text);
        a.busy = true;
        a.cancel.store(false, Ordering::Relaxed);
        a.turns.clone()
    };
    let mut req = claude::ClaudeRequest::new(prompt(&text, &ctx, frame.as_deref(), &turns), 300);
    req.system = Some(SYSTEM.to_string());
    req.allowed_tools = vec!["Read".into()];
    req.max_turns = Some(6);
    req.cwd = Some(dir);
    let sync = bool_p(v, "sync").unwrap_or(false);
    let job = move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let cancel = lock(&shared).cancel.clone();
            if claude::find_claude().is_none() {
                return Err("Claude Code CLI bulunamadı (claude). Kurulu ve oturum açık olmalı.".to_string());
            }
            let sh = shared.clone();
            claude::run(&req, &cancel, &mut |ev| {
                if let claude::ClaudeEvent::Tool { name, detail } = ev {
                    lock(&sh).push("tool", &format!("{name} {}", truncate_line(detail, 120)));
                }
            })
        }));
        let mut a = lock(&shared);
        a.busy = false;
        match res {
            Ok(Ok(raw)) => {
                let mut id = a.next_id;
                let (reply, steps) = parse_answer(&raw, &mut id);
                a.next_id = id;
                let reply = if reply.is_empty() && steps.is_empty() { "(boş yanıt)".to_string() } else { reply };
                a.push("assistant", &reply);
                if !steps.is_empty() {
                    a.push("tool", &format!("{} adım önerildi: ✓ ile tek tek ya da \"Hepsini uygula\" ile uygulayın.", steps.len()));
                }
                a.steps.retain(|st| st.status != "bekliyor");
                a.steps.extend(steps);
                a.turns.push((truncate_line(&text, 400), truncate_line(&reply, 600)));
                if a.turns.len() > MAX_TURNS {
                    a.turns.remove(0);
                }
            }
            Ok(Err(e)) => a.push("error", &e),
            Err(_) => a.push("error", "asistan beklenmedik biçimde durdu"),
        }
    };
    if sync || cfg!(target_arch = "wasm32") {
        job();
    } else {
        std::thread::spawn(job);
    }
    Ok(json!({"started": true, "frame": frame.map(|f| f.to_string_lossy().into_owned())}))
}

fn apply(s: &mut Session, v: &Value) -> Result<Value> {
    let shared = s.crstudio.assistant.clone();
    let all = bool_p(v, "all").unwrap_or(false);
    let id = str_p(v, "id").map(str::to_string);
    if !all && id.is_none() {
        return Err(bad("crstudio.assistant.apply", "`id` ya da `all: true` gerekli"));
    }
    let todo: Vec<AssistantStep> = lock(&shared).steps.iter().filter(|st| st.status == "bekliyor" && (all || Some(&st.id) == id.as_ref())).cloned().collect();
    if todo.is_empty() {
        return Err(bad("crstudio.assistant.apply", "uygulanacak adım yok"));
    }
    let mut results = Vec::new();
    for st in todo {
        let r =
            if allowed(&st.command) { s.execute(&st.command, st.params.clone()).map_err(|e| e.to_string()) } else { Err("izin verilmeyen komut".to_string()) };
        let status = match &r {
            Ok(_) => "uygulandı".to_string(),
            Err(e) => format!("hata: {}", truncate_line(e, 200)),
        };
        let mut a = lock(&shared);
        if let Some(x) = a.steps.iter_mut().find(|x| x.id == st.id) {
            x.status = status.clone();
        }
        match &r {
            Ok(_) => a.push("ok", &format!("✓ {}", st.label)),
            Err(e) => a.push("error", &format!("✗ {}: {e}", st.label)),
        }
        results.push(json!({"id": st.id, "status": status}));
    }
    Ok(json!({"results": results}))
}

/// Place a graphics template over a time range on the top free video track.
fn overlay(s: &mut Session, v: &Value) -> Result<Value> {
    let cmd = "crstudio.assistant.overlay";
    let mut tpl = str_p(v, "template").unwrap_or("").trim().to_string();
    if tpl.is_empty() {
        return Err(bad(cmd, "`template` gerekli"));
    }
    if !tpl.contains(':') {
        tpl = format!("builtin:{tpl}");
    }
    let start = f64_p(v, "startSec").filter(|x| x.is_finite() && *x >= 0.0).unwrap_or_else(|| s.playhead().seconds());
    let dur = f64_p(v, "endSec")
        .filter(|e| e.is_finite())
        .map(|e| e - start)
        .or_else(|| f64_p(v, "durationSec"))
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(5.0)
        .clamp(0.2, 3600.0);
    let mut args = json!({"template": tpl, "time": Tick::from_seconds_f64(start).0, "durationSeconds": dur});
    if let Some(vals) = v.get("values").filter(|x| x.is_object()) {
        args["values"] = vals.clone();
    }
    let ad_band = tpl.ends_with("yt-ad-band");
    let r = s.execute("graphics.template.apply", args)?;
    // a bottom band hides bottom captions: lift the caption tracks above it
    if ad_band {
        let low: Vec<usize> = s
            .active_sequence()
            .map(|q| q.caption_tracks.iter().enumerate().filter(|(_, t)| t.style.margin < AD_BAND_CAPTION_MARGIN).map(|(i, _)| i).collect())
            .unwrap_or_default();
        for i in low {
            let _ = s.execute("captions.setStyle", json!({"track": format!("C{}", i + 1), "margin": AD_BAND_CAPTION_MARGIN}));
        }
    }
    Ok(r)
}

/// Caption margin (fraction of the frame height) that clears `yt-ad-band` (top edge at y 1004/1080).
const AD_BAND_CAPTION_MARGIN: f32 = 0.075;

fn procedural_sfx(dir: &Path, kind: &str) -> Result<(PathBuf, String)> {
    let path = dir.join(format!("sfx_{kind}.wav"));
    if !path.is_file() {
        filmcraft_format::atomic_write(&path, &audio::generate_sfx_wav(kind)).map_err(|e| other(format!("{}: {e}", path.display())))?;
    }
    Ok((path, audio::sfx_label(kind).to_string()))
}

/// Openverse audio search (CC0, public domain, CC BY), short clips only.
fn openverse_sfx(dir: &Path, query: &str) -> Option<(PathBuf, String)> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let api = format!("https://api.openverse.org/v1/audio/?q={}&license=cc0,pdm,by&page_size=12&mature=false", visuals::url_encode(query));
    let v = visuals::curl_json(&api)?;
    for r in v.get("results").and_then(Value::as_array)? {
        let st = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let ms = r.get("duration").and_then(Value::as_u64).unwrap_or(0);
        if ms == 0 || ms > 30_000 {
            continue;
        }
        let url = st("url");
        let lower = url.to_ascii_lowercase();
        let Some(ext) = ["mp3", "wav", "ogg", "flac"].into_iter().find(|e| lower.split('?').next().unwrap_or("").ends_with(e)) else { continue };
        let license = match st("license").as_str() {
            "cc0" => "CC0".to_string(),
            "pdm" => "Public domain".to_string(),
            "by" => format!("CC BY {}", st("license_version")),
            _ => continue,
        };
        let id: String = st("id").chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect();
        let dest = dir.join(format!("ov_{id}.{ext}"));
        if !dest.is_file() && !visuals::download(&url, &dest) {
            continue;
        }
        let title = st("title");
        let author = st("creator");
        write_attribution_sidecar(
            &dest,
            &title,
            if author.is_empty() { "unknown" } else { &author },
            &st("foreign_landing_url"),
            &license,
            "Openverse audio, CrStudio Asistan",
        );
        return Some((dest, format!("{title} — {author} ({license})")));
    }
    None
}

fn add_sfx(s: &mut Session, v: &Value) -> Result<Value> {
    let cmd = "crstudio.assistant.addSfx";
    let at = f64_p(v, "atSec").filter(|x| x.is_finite() && *x >= 0.0).unwrap_or_else(|| s.playhead().seconds());
    let gain = f64_p(v, "gainDb").filter(|x| x.is_finite()).unwrap_or(-9.0).clamp(-40.0, 6.0);
    let dir = work_dir(s).join("sfx");
    std::fs::create_dir_all(&dir).map_err(|e| other(format!("{}: {e}", dir.display())))?;
    let kind = str_p(v, "kind").map(str::trim).filter(|k| !k.is_empty());
    let query = str_p(v, "query").map(str::trim).filter(|q| !q.is_empty());
    let (path, label) = match (kind, query) {
        (Some(k), _) if SFX_KINDS.contains(&k) => procedural_sfx(&dir, k)?,
        (_, Some(q)) => match openverse_sfx(&dir, q) {
            Some(x) => x,
            None => {
                let (p, l) = procedural_sfx(&dir, "whoosh")?;
                (p, format!("{l} (\"{q}\" için lisanslı ses bulunamadı, yerine üretilmiş efekt)"))
            }
        },
        (Some(k), None) => return Err(bad(cmd, format!("bilinmeyen efekt `{k}`; şunlardan biri: {}", SFX_KINDS.join(", ")))),
        (None, None) => procedural_sfx(&dir, "whoosh")?,
    };
    let bin = assemble::ensure_named_bin(s, "03 Ses Efektleri ve Müzik")?;
    let item = assemble::import_path(s, &path.to_string_lossy(), bin)?;
    let t = Tick::from_seconds_f64(at);
    // a free audio track around `at` (5 s), else a new one
    let track = s
        .active_sequence()
        .ok_or(EngineError::NoSequence)?
        .audio_tracks
        .iter()
        .position(|tr| !tr.locked && !tr.items.iter().any(|i| i.start.0 < t.0 + Tick::from_seconds_f64(5.0).0 && i.end().0 > t.0))
        .map(|i| json!(format!("A{}", i + 1)))
        .unwrap_or(json!("new"));
    let placed = s.execute("timeline.place", json!({"item": item.0, "audioTrack": track, "time": t.0, "video": false, "audio": true}))?;
    let clips: Vec<u64> = placed["clips"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    if !clips.is_empty() {
        let _ = s.execute("clip.gain", json!({"clips": clips, "setDb": gain}));
        let _ = s.execute("essentialSound.setType", json!({"clips": clips, "type": "sfx"}));
    }
    Ok(json!({"path": path.to_string_lossy(), "label": label, "clips": clips, "atSec": at}))
}

fn spec(id: &'static str, label: &'static str, params: &'static str, journal: bool, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("crstudio.assistant.ask", "Asistan: iste", r#"{"text":str,"sync":bool?}"#, false, ask),
        spec("crstudio.assistant.state", "Asistan: durum", "{}", false, |s, _| Ok(serde_json::to_value(&*lock(&s.crstudio.assistant)).unwrap_or_default())),
        spec("crstudio.assistant.context", "Asistan: bağlam", "{}", false, |s, _| Ok(context(s))),
        spec("crstudio.assistant.apply", "Asistan: adımı uygula", r#"{"id":str?,"all":bool?}"#, true, apply),
        spec("crstudio.assistant.dismiss", "Asistan: adımı kaldır", r#"{"id":str?}"#, false, |s, v| {
            let mut a = lock(&s.crstudio.assistant);
            match str_p(v, "id") {
                Some(id) => a.steps.retain(|st| st.id != id),
                None => a.steps.retain(|st| st.status == "bekliyor"),
            }
            a.version = a.version.wrapping_add(1);
            Ok(json!({"steps": a.steps.len()}))
        }),
        spec("crstudio.assistant.cancel", "Asistan: durdur", "{}", false, |s, _| {
            let a = lock(&s.crstudio.assistant);
            a.cancel.store(true, Ordering::Relaxed);
            Ok(json!({"busy": a.busy}))
        }),
        spec("crstudio.assistant.clear", "Asistan: temizle", "{}", false, |s, _| {
            let mut a = lock(&s.crstudio.assistant);
            if a.busy {
                return Err(bad("crstudio.assistant.clear", "asistan çalışıyor"));
            }
            a.log.clear();
            a.steps.clear();
            a.turns.clear();
            a.version = a.version.wrapping_add(1);
            Ok(json!({}))
        }),
        spec(
            "crstudio.assistant.overlay",
            "Asistan: aralığa grafik koy",
            r#"{"template":str,"startSec":f64?,"endSec":f64?,"durationSec":f64?,"values":obj?}"#,
            true,
            overlay,
        ),
        spec("crstudio.assistant.addSfx", "Asistan: efekt sesi ekle", r#"{"kind":str?,"query":str?,"atSec":f64?,"gainDb":f64?}"#, true, add_sfx),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_parsing_checks_commands_and_times() {
        let raw = r#"Tamam. {"reply":"Reklam kuşağı hazır","steps":[
            {"label":"Kuşak","command":"crstudio.assistant.overlay","params":{"template":"yt-ad-band","startSec":12,"endSec":20}},
            {"label":"Sil","command":"file.close","params":{}},
            {"label":"x","command":"","params":{}},
            {"label":"Ses","command":"crstudio.assistant.addSfx","params":{"kind":"whoosh","atSec":-4}}]}"#;
        let mut id = 0;
        let (reply, steps) = parse_answer(raw, &mut id);
        assert_eq!(reply, "Reklam kuşağı hazır");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].seconds, Some(12.0));
        assert_eq!(steps[0].status, "bekliyor");
        assert!(steps[1].status.starts_with("hata"), "file.close is not allowed");
        assert_eq!(steps[2].seconds, None, "negative time dropped");
        assert_ne!(steps[0].id, steps[2].id);
        let (r, s) = parse_answer("düz metin", &mut id);
        assert_eq!(r, "düz metin");
        assert!(s.is_empty());
    }

    #[test]
    fn ad_band_overlay_and_sfx_land_on_the_timeline_and_undo() {
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"name": "Test"})).unwrap();
        let before = s.active_sequence().map(|q| q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>()).unwrap_or(0);
        let r = s
            .execute(
                "crstudio.assistant.overlay",
                json!({"template": "yt-ad-band", "startSec": 2.0, "endSec": 9.0, "values": {"label": "REKLAM", "brand": "Örnek Marka"}}),
            )
            .unwrap();
        assert!(r.is_object() || r.is_number(), "{r}");
        let q = s.active_sequence().unwrap();
        let it = q.video_tracks.iter().flat_map(|t| t.items.iter()).last().unwrap();
        assert!((it.duration.seconds() - 7.0).abs() < 0.1, "band covers the range: {}", it.duration.seconds());
        assert!((it.start.seconds() - 2.0).abs() < 0.1);
        s.execute("captions.add", json!({"seconds": 3.0, "durationSeconds": 2.0, "text": "altyazı"})).unwrap();
        s.execute("crstudio.assistant.overlay", json!({"template": "yt-ad-band", "startSec": 10.0, "endSec": 12.0})).unwrap();
        let q = s.active_sequence().unwrap();
        assert!(q.caption_tracks.iter().all(|t| t.style.margin >= AD_BAND_CAPTION_MARGIN), "captions lifted above the band");
        assert!(s.execute("crstudio.assistant.overlay", json!({"template": "builtin:nope"})).is_err());
        let sfx = s.execute("crstudio.assistant.addSfx", json!({"kind": "riser", "atSec": 3.0})).unwrap();
        assert!(!sfx["clips"].as_array().unwrap().is_empty());
        assert!(s.execute("crstudio.assistant.addSfx", json!({"kind": "bogus"})).is_err());
        s.execute("edit.undo", json!({})).ok();
        let after = s.active_sequence().map(|q| q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>()).unwrap_or(0);
        assert!(after > before);
    }

    #[test]
    fn apply_runs_only_pending_allowed_steps() {
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"name": "Test"})).unwrap();
        {
            let mut a = lock(&s.crstudio.assistant);
            let mut id = 0;
            let (_, steps) = parse_answer(
                r#"{"reply":"","steps":[{"label":"İşaret","command":"markers.add","params":{"seconds":1.0,"name":"burada"}},{"label":"Kapat","command":"file.close"}]}"#,
                &mut id,
            );
            a.steps = steps;
        }
        let r = s.execute("crstudio.assistant.apply", json!({"all": true})).unwrap();
        assert_eq!(r["results"].as_array().unwrap().len(), 1, "the disallowed step is never run");
        assert!(s.execute("crstudio.assistant.apply", json!({"all": true})).is_err(), "nothing left");
        assert!(s.execute("crstudio.assistant.ask", json!({"text": "  "})).is_err());
        assert!(context(&s)["templates"].as_array().unwrap().iter().any(|t| t["id"] == "builtin:yt-ad-band"));
    }
}
