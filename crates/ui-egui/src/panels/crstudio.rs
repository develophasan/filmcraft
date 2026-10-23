//! CrStudio — the Turkish AI documentary studio (`Mode::CrStudio`).
//!
//! Layout: a header with the project, the running job and the Chief Editor's score; a stage rail
//! on the left (Konu → Ajanlar → Sahneler → Seslendirme → Kurgu → Yayın); the stage in the middle;
//! the agents' live feed on the right. Every action runs a `crstudio.*` command, collected while
//! drawing and executed after (so nothing borrows the session twice).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Align, Align2, Color32, CornerRadius, Layout, Margin, RichText, Sense, Stroke, TextureHandle, Vec2, pos2, vec2};
use filmcraft_engine::crstudio::{AgentStatus, CrStudioPipeline, DocTone, PipelineStage, SceneSpec, VideoFormatPreset};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::Mode;
use crate::theme::Tokens;

// ------------------------------------------------------------------------------------------------
// Palette (readable on the dark and the light theme)
// ------------------------------------------------------------------------------------------------

const RED: Color32 = Color32::from_rgb(225, 29, 72);
const GREEN: Color32 = Color32::from_rgb(22, 163, 74);
const AMBER: Color32 = Color32::from_rgb(217, 119, 6);
const BLUE: Color32 = Color32::from_rgb(37, 99, 235);
const GREY: Color32 = Color32::from_rgb(120, 128, 140);

fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn score_color(score: f64) -> Color32 {
    if score >= 85.0 {
        GREEN
    } else if score >= 65.0 {
        AMBER
    } else {
        RED
    }
}

// ------------------------------------------------------------------------------------------------
// UI-only state
// ------------------------------------------------------------------------------------------------

/// The narration player of the Seslendirme stage: which scene plays, when it ends (UI clock),
/// and whether it goes on to the next scene.
#[derive(Clone, Copy, Debug, Default)]
struct VoicePlay {
    scene: usize,
    until: f64,
    all: bool,
}

#[derive(Default)]
struct UiState {
    voice_play: Option<VoicePlay>,
    /// The side views opened from the rail: Üretim Hattı, Sahne panosu.
    prod_open: bool,
    board_open: bool,
    show_advanced: bool,
    show_projects: bool,
    projects: Vec<Value>,
    voices: Option<Value>,
    voices_at: f64,
    record_started: Option<f64>,
    yt_client_id: String,
    yt_client_secret: String,
    yt_video_path: String,
    yt_status: Option<Value>,
    yt_status_at: f64,
    asked_restore: bool,
    teleprompter: bool,
    /// ElevenLabs panel: key being typed, account voices, status.
    el_open: bool,
    el_key: String,
    el_status: Option<Value>,
    el_voices: Vec<Value>,
    /// Pronunciation dictionary editor.
    lex_open: bool,
    lex_text: Option<String>,
    lex_test: String,
    lex_preview: String,
    /// Recording studio (full-screen teleprompter): open, mode (false = scenes, true = clone
    /// reading), scene shown, last clean-up report, clone status cache, own-text editor.
    studio_open: bool,
    studio_clone: bool,
    studio_scene: usize,
    studio_report: Option<Value>,
    clone_status: Option<Value>,
    clone_text: Option<String>,
    clone_text_open: bool,
    /// `crstudio.blender.status` (cached).
    blender: Option<Value>,
}

/// Background thumbnail loader: decoding happens off the UI thread.
struct Thumbs {
    tex: HashMap<String, TextureHandle>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    tx: Sender<(String, Option<(usize, usize, Vec<u8>)>)>,
    rx: Receiver<(String, Option<(usize, usize, Vec<u8>)>)>,
}

impl Default for Thumbs {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tex: HashMap::new(), pending: HashSet::new(), failed: HashSet::new(), tx, rx }
    }
}

type Probe = Option<(std::sync::Arc<filmcraft_engine::MediaPool>, std::sync::Arc<dyn filmcraft_engine::Services>)>;

thread_local! {
    /// The engine's media pool and file services, for movie thumbnails (set by the panel; the
    /// decoding threads get a copy).
    static PROBE: RefCell<Probe> = const { RefCell::new(None) };
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
    static THUMBS: RefCell<Thumbs> = RefCell::new(Thumbs::default());
}

fn decode_thumb(path: &str, max_w: usize, probe: Probe) -> Option<(usize, usize, Vec<u8>)> {
    let movie =
        std::path::Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mov" | "mp4" | "m4v"));
    let (src, at) = if movie {
        // a Blender shot is hundreds of MB: the engine's probe reads the index and one frame
        let (pool, services) = probe?;
        (pool.probe_file(path, services.as_ref()).ok()?, filmcraft_time::Tick::from_seconds_f64(2.5))
    } else {
        let bytes = std::fs::read(path).ok()?;
        (filmcraft_media::open_bytes(path, bytes.into(), &[]).ok()?, filmcraft_time::Tick::ZERO)
    };
    let frame = src.video_frame(filmcraft_media::FrameRequest::full(at)).ok()?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let rgba = frame.to_rgba8();
    let scale = (w as f32 / max_w as f32).max(1.0);
    let (tw, th) = (((w as f32) / scale).round().max(1.0) as usize, ((h as f32) / scale).round().max(1.0) as usize);
    let mut out = vec![0u8; tw * th * 4];
    for y in 0..th {
        for x in 0..tw {
            let sx = ((x as f32 + 0.5) * scale) as usize;
            let sy = ((y as f32 + 0.5) * scale) as usize;
            let si = (sy.min(h - 1) * w + sx.min(w - 1)) * 4;
            let di = (y * tw + x) * 4;
            if let (Some(src), Some(dst)) = (rgba.get(si..si + 4), out.get_mut(di..di + 4)) {
                dst.copy_from_slice(src);
            }
        }
    }
    Some((tw, th, out))
}

fn thumb_key(path: &str) -> String {
    let m = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis());
    format!("{path}@{m}")
}

/// The thumbnail texture for `path`, starting a background decode the first time.
fn thumb(ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
    if path.is_empty() {
        return None;
    }
    let key = thumb_key(path);
    THUMBS.with(|t| {
        let mut t = t.borrow_mut();
        while let Ok((k, img)) = t.rx.try_recv() {
            t.pending.remove(&k);
            match img {
                Some((w, h, px)) => {
                    let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &px);
                    let handle = ctx.load_texture(format!("crs-thumb:{k}"), ci, egui::TextureOptions::LINEAR);
                    if t.tex.len() > 600 {
                        t.tex.clear();
                    }
                    t.tex.insert(k, handle);
                }
                None => {
                    t.failed.insert(k);
                }
            }
            ctx.request_repaint();
        }
        if let Some(h) = t.tex.get(&key) {
            return Some(h.clone());
        }
        if !t.pending.contains(&key) && !t.failed.contains(&key) && t.pending.len() < 6 {
            t.pending.insert(key.clone());
            let tx = t.tx.clone();
            let p = path.to_string();
            let ctx2 = ctx.clone();
            let probe = PROBE.with(|pr| pr.borrow().clone());
            std::thread::spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_thumb(&p, 360, probe))).ok().flatten();
                let _ = tx.send((key, res));
                ctx2.request_repaint();
            });
        }
        None
    })
}

fn paint_thumb(ui: &mut egui::Ui, path: Option<&str>, size: Vec2, placeholder: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(14, 17, 24));
    if let Some(tex) = path.and_then(|p| thumb(ui.ctx(), p)) {
        let ts = tex.size_vec2();
        let s = (rect.width() / ts.x.max(1.0)).max(rect.height() / ts.y.max(1.0));
        let uv_w = (rect.width() / (ts.x * s)).min(1.0);
        let uv_h = (rect.height() / (ts.y * s)).min(1.0);
        let uv = egui::Rect::from_center_size(pos2(0.5, 0.5), vec2(uv_w, uv_h));
        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
    } else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, placeholder, Tokens::ui(11.0), GREY);
    }
    resp
}

// ------------------------------------------------------------------------------------------------
// Actions (run after drawing)
// ------------------------------------------------------------------------------------------------

enum Act {
    Run(&'static str, Value),
    /// Open a scene for editing (from the board).
    Focus(usize),
    Stage(PipelineStage),
    Mode(Mode),
    OpenUrl(String),
    Reveal(String),
    PickUserVisual(usize),
    PickAudio(usize),
    PickReference,
    PickVideoForUpload,
    Status(String),
    /// Kurgu masasına geç ve Asistan panelini aç.
    Assistant,
}

fn run(acts: &mut Vec<Act>, id: &'static str, v: Value) {
    acts.push(Act::Run(id, v));
}

fn apply(app: &mut FilmcraftApp, ctx: &egui::Context, acts: Vec<Act>) {
    for a in acts {
        match a {
            Act::Run(id, v) => match app.session.execute(id, v) {
                Ok(r) => {
                    if id == "crstudio.youtube.connect"
                        && let Some(url) = r["authUrl"].as_str()
                    {
                        crate::links::open(app, ctx, url);
                    }
                    if id == "crstudio.assembleTimeline" || id == "crstudio.createShortsCut" {
                        app.status(format!("CrStudio: kurgu hazır ({} sahne)", r["scenesPlaced"]));
                    }
                }
                Err(e) => app.status(format!("CrStudio: {e}")),
            },
            Act::Stage(s) => app.session.crstudio.pipeline.stage = s,
            Act::Focus(i) => {
                app.session.crstudio.pipeline.active_scene_index = i;
                app.session.crstudio.pipeline.stage = PipelineStage::ReviewScenes;
                UI.with(|u| u.borrow_mut().board_open = false);
            }
            Act::Mode(m) => app.ui.mode = m,
            Act::Assistant => {
                app.ui.mode = Mode::Edit;
                app.show_panel(crate::dock::PanelKind::Assistant);
            }
            Act::OpenUrl(u) => crate::links::open(app, ctx, &u),
            Act::Reveal(p) => {
                if let Some(f) = app.hooks.open_path.as_mut() {
                    let _ = f(&p, true);
                }
            }
            Act::PickUserVisual(i) => {
                if let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("Görsel", &["png", "jpg", "jpeg"])) {
                    let r = app.session.execute("crstudio.addUserVisual", json!({"sceneIndex": i, "path": path}));
                    if let Err(e) = r {
                        app.status(format!("CrStudio: {e}"));
                    }
                }
            }
            Act::PickAudio(i) => {
                if let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("WAV ses", &["wav"]))
                    && let Err(e) = app.session.execute("crstudio.attachVoiceover", json!({"sceneIndex": i, "path": path}))
                {
                    app.status(format!("CrStudio: {e}"));
                }
            }
            Act::PickReference => {
                if let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("Referans ses (WAV, 10-20 sn)", &["wav"])) {
                    let _ = app.session.execute("crstudio.setVoice", json!({"reference": path}));
                }
            }
            Act::PickVideoForUpload => {
                if let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f("Video", &["mp4", "mov", "mkv", "webm"])) {
                    UI.with(|u| u.borrow_mut().yt_video_path = path);
                }
            }
            Act::Status(s) => app.status(s),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Small widgets
// ------------------------------------------------------------------------------------------------

fn pill(ui: &mut egui::Ui, text: &str, fg: Color32) {
    let _ = pill_r(ui, text, fg);
}

fn pill_r(ui: &mut egui::Ui, text: &str, fg: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), Tokens::semibold(11.0), fg);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(14.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(10), alpha(fg, 34));
    ui.painter().galley(rect.min + vec2(7.0, 3.0), galley, fg);
    resp
}

fn primary_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let btn = egui::Button::new(RichText::new(label).font(Tokens::semibold(14.0)).color(Color32::WHITE))
        .fill(if enabled { RED } else { GREY })
        .min_size(vec2(180.0, 36.0))
        .corner_radius(CornerRadius::same(8));
    let resp = ui.add_enabled(enabled, btn);
    app.auto.add(id, resp.rect, label);
    resp.clicked()
}

fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str) -> bool {
    let resp = ui.add(egui::Button::new(RichText::new(label).font(Tokens::ui(12.5))).corner_radius(CornerRadius::same(6)));
    app.auto.add(id, resp.rect, label);
    resp.clicked()
}

fn card<R>(ui: &mut egui::Ui, t: &Tokens, highlight: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(t.tl_header_bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 12))
        .stroke(Stroke::new(if highlight { 1.5 } else { 1.0 }, if highlight { RED } else { t.separator }))
        .show(ui, add)
        .inner
}

fn section_title(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(text).font(Tokens::semibold(15.0)).color(t.text));
    ui.add_space(4.0);
}

fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.label(RichText::new(text).font(Tokens::ui(11.5)).color(t.text_dim));
}

fn now(ui: &egui::Ui) -> f64 {
    ui.input(|i| i.time)
}

/// Start playing scene `from`'s narration (or, with `all`, the first scene from `from` on that has
/// one). Returns false when there is nothing to play.
fn play_voice(app: &mut FilmcraftApp, ui: &egui::Ui, p: &CrStudioPipeline, from: usize, all: bool) -> bool {
    for sc in p.scenes.iter().skip(from) {
        let Some(path) = sc.voiceover_path.as_deref().filter(|v| std::path::Path::new(v).is_file()) else {
            if all {
                continue;
            }
            return false;
        };
        match filmcraft_engine::crstudio::wav::read(std::path::Path::new(path)) {
            Ok(m) => match app.play_mono(m.samples, m.rate) {
                Ok(secs) => {
                    let until = now(ui) + secs + if all { 0.6 } else { 0.1 };
                    UI.with(|u| u.borrow_mut().voice_play = Some(VoicePlay { scene: sc.index, until, all }));
                    return true;
                }
                Err(e) => {
                    app.status(format!("Ses çalınamadı: {e}"));
                    return false;
                }
            },
            Err(e) => {
                app.status(format!("Sahne {} sesi okunamadı: {e}", sc.index + 1));
                if !all {
                    return false;
                }
            }
        }
        if !all {
            return false;
        }
    }
    false
}

fn stop_voice(app: &mut FilmcraftApp) {
    if UI.with(|u| u.borrow_mut().voice_play.take()).is_some() {
        app.stop();
    }
}

/// "~12 dk", "~40 sn", "~1 sa 5 dk".
fn eta(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        format!("~{s} sn")
    } else if s < 3600 {
        format!("~{} dk", s.div_ceil(60))
    } else {
        format!("~{} sa {} dk", s / 3600, (s % 3600).div_ceil(60))
    }
}

/// Üretim Hattı: the steps left with their cost, approved one by one or all at once, and the queue.
fn production_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    use filmcraft_engine::crstudio::production;
    let t = app.tokens;
    let steps = production::plan(p);
    let pending: Vec<&production::Step> = steps.iter().filter(|s| !s.done && !s.commands.is_empty()).collect();
    let secs: f64 = pending.iter().map(|s| s.seconds).sum();
    let credits: u64 = pending.iter().map(|s| s.credits).sum();
    let heavy = pending.iter().any(|s| s.heavy);
    ui.label(RichText::new("Üretim Hattı").font(Tokens::semibold(18.0)).color(t.text));
    hint(ui, &t, "Kalan adımlar, ne kadar süreceği ve ne harcayacağıyla. Onayladığınız adımlar sıraya girer ve biri bitince öteki kendiliğinden başlar.");
    ui.add_space(8.0);
    card(ui, &t, !pending.is_empty(), |ui| {
        ui.horizontal_wrapped(|ui| {
            if pending.is_empty() {
                ui.label(RichText::new("Bekleyen adım yok").font(Tokens::semibold(13.0)).color(GREEN));
            } else {
                ui.label(RichText::new(format!("Kalan: {} adım · {}", pending.len(), eta(secs))).font(Tokens::semibold(13.0)).color(t.text));
                if credits > 0 {
                    pill(ui, &format!("~{credits} ElevenLabs kredisi"), BLUE);
                }
                if heavy {
                    pill(ui, "Mac ısınır · uyku engellenir, kapağı açık tutun", AMBER);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ids: Vec<String> = pending.iter().map(|s| s.id.clone()).collect();
                    if primary_button(app, ui, "crstudio.production.approveAll", "✓ Tümünü onayla ve sıraya al", p.queue.is_empty()) {
                        run(acts, "crstudio.queue.add", json!({"steps": ids}));
                    }
                });
            }
        });
    });
    ui.add_space(8.0);
    egui::ScrollArea::vertical().id_salt("crs-production").auto_shrink([false, false]).show(ui, |ui| {
        for (k, st) in steps.iter().enumerate() {
            let queued = st.commands.iter().any(|c| p.queue.iter().any(|q| q.command == c.command && q.params == c.params));
            card(ui, &t, false, |ui| {
                ui.horizontal(|ui| {
                    let (icon, col) = if st.done {
                        ("✓", GREEN)
                    } else if queued {
                        ("⏳", BLUE)
                    } else {
                        ("●", t.text_dim)
                    };
                    ui.label(RichText::new(icon).font(Tokens::semibold(14.0)).color(col));
                    ui.label(RichText::new(format!("{}. {}", k + 1, st.label)).font(Tokens::semibold(13.5)).color(t.text));
                    if !st.done {
                        pill(ui, &eta(st.seconds), t.text_dim);
                        if st.credits > 0 {
                            pill(ui, &format!("~{} kredi", st.credits), BLUE);
                        }
                        if st.heavy {
                            pill(ui, "ağır iş", AMBER);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !st.done && !st.commands.is_empty() && !queued {
                            let b = ui.button("Onayla ve sıraya al");
                            app.auto.add(&format!("crstudio.production.step.{}", st.id), b.rect, &st.label);
                            if b.clicked() {
                                run(acts, "crstudio.queue.add", json!({"steps": [st.id]}));
                            }
                            // Blender: a quick draft first (about a third of the time) to check the shots
                            if st.id == "blender" {
                                let d = ui.button("Önce taslak (hızlı)").on_hover_text("Düşük kalitede hızlı çizer; beğenince son kaliteyle çizdirirsiniz");
                                app.auto.add("crstudio.production.blenderDraft", d.rect, "Önce taslak");
                                if d.clicked() {
                                    for c in &st.commands {
                                        let mut params = c.params.clone();
                                        if c.command == "crstudio.blender.render" {
                                            params["quality"] = json!("draft");
                                        }
                                        run(
                                            acts,
                                            "crstudio.queue.add",
                                            json!({"command": c.command, "params": params, "label": format!("{} (taslak)", c.label)}),
                                        );
                                    }
                                }
                            }
                        } else if queued {
                            ui.label(RichText::new("sırada").font(Tokens::ui(11.5)).color(BLUE));
                        }
                    });
                });
                ui.label(RichText::new(&st.detail).font(Tokens::ui(12.0)).color(t.text_dim));
            });
            ui.add_space(6.0);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("SIRADA").font(Tokens::semibold(10.5)).color(t.text_faint));
        if p.job.running {
            ui.add(egui::ProgressBar::new(p.job.progress as f32).text(RichText::new(format!("Şimdi: {}", p.job.label)).font(Tokens::ui(11.5))).animate(true));
        }
        if p.queue.is_empty() {
            hint(ui, &t, "Sıra boş.");
        } else {
            let left: f64 = p.queue.iter().map(|q| q.seconds).sum();
            for (i, q) in p.queue.iter().enumerate() {
                ui.label(RichText::new(format!("{}. {} · {}", i + 1, q.label, eta(q.seconds))).font(Tokens::ui(12.0)).color(t.text));
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Tahmini bitiş: {}", eta(left))).font(Tokens::ui(12.0)).color(t.text_dim));
                if button(app, ui, "crstudio.production.clear", "Hattı boşalt") {
                    run(acts, "crstudio.queue.clear", json!({}));
                }
            });
        }
    });
}

/// Sahne panosu: every scene's picture at a glance, with what needs a look.
fn board_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    use filmcraft_engine::crstudio::production;
    let t = app.tokens;
    let issues = production::scene_issues(p);
    let serious = issues.iter().filter(|i| i.severity >= 2).count();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Sahne panosu").font(Tokens::semibold(18.0)).color(t.text));
        ui.label(
            RichText::new(format!("{} sahne · {serious} önemli, {} küçük uyarı", p.scenes.len(), issues.len() - serious))
                .font(Tokens::ui(12.0))
                .color(t.text_dim),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let weak: Vec<usize> = {
                let mut v: Vec<usize> =
                    issues.iter().filter(|i| matches!(i.kind.as_str(), "old_map" | "no_visual" | "low_relevance")).map(|i| i.scene).collect();
                v.dedup();
                v
            };
            if !weak.is_empty() && button(app, ui, "crstudio.board.rehunt", &format!("Zayıf {} sahnede görsel ara", weak.len())) {
                for i in &weak {
                    run(
                        acts,
                        "crstudio.queue.add",
                        json!({"command": "crstudio.huntVisuals", "params": {"sceneIndex": i}, "label": format!("Görsel arama · sahne {}", i + 1)}),
                    );
                }
            }
            if p.visual_concept == "atlas" && button(app, ui, "crstudio.board.mapPlan", "Tekrarlayan haritalara fotoğraf") {
                run(acts, "crstudio.applyMapPlan", json!({}));
            }
        });
    });
    hint(ui, &t, "Kırmızı: dışa aktarmadan önce düzeltin. Sarı: bir bakın. Bir sahneye tıklayınca düzenlemeye gider.");
    ui.add_space(8.0);
    let tile = vec2(214.0, 120.0);
    let cols = ((ui.available_width() + 10.0) / (tile.x + 10.0)).floor().max(1.0) as usize;
    egui::ScrollArea::vertical().id_salt("crs-board").auto_shrink([false, false]).show(ui, |ui| {
        for row in p.scenes.chunks(cols) {
            ui.horizontal_top(|ui| {
                for sc in row {
                    let mine: Vec<&production::SceneIssue> = issues.iter().filter(|i| i.scene == sc.index).collect();
                    let worst = mine.iter().map(|i| i.severity).max().unwrap_or(0);
                    ui.allocate_ui_with_layout(vec2(tile.x, tile.y + 54.0), Layout::top_down(Align::Min), |ui| {
                        let sel = sc.selected_visual.as_deref();
                        let movie = sel.is_some_and(|v| v.ends_with(".mov"));
                        let resp = paint_thumb(ui, sel, tile, "görsel yok");
                        let r = resp.rect;
                        if movie {
                            let tag = egui::Rect::from_min_size(pos2(r.max.x - 72.0, r.min.y + 4.0), vec2(68.0, 16.0));
                            ui.painter().rect_filled(tag, CornerRadius::same(4), alpha(AMBER, 230));
                            ui.painter().text(tag.center(), Align2::CENTER_CENTER, "BLENDER", Tokens::semibold(10.0), Color32::BLACK);
                        }
                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(26.0, 18.0)), CornerRadius::same(4), Color32::from_black_alpha(190));
                        ui.painter().text(r.min + vec2(13.0, 9.0), Align2::CENTER_CENTER, (sc.index + 1).to_string(), Tokens::semibold(11.0), Color32::WHITE);
                        if worst > 0 {
                            let col = if worst >= 2 { RED } else { AMBER };
                            ui.painter().rect_stroke(r, CornerRadius::same(6), Stroke::new(2.0, col), egui::StrokeKind::Inside);
                        }
                        app.auto.add(&format!("crstudio.board.scene.{}", sc.index), r, &sc.title);
                        if resp.clicked() {
                            acts.push(Act::Focus(sc.index));
                        }
                        ui.label(RichText::new(clip_text(&sc.title, 30)).font(Tokens::semibold(11.5)).color(t.text));
                        for i in mine.iter().take(2) {
                            ui.label(RichText::new(clip_text(&i.message, 40)).font(Tokens::ui(10.5)).color(if i.severity >= 2 { RED } else { AMBER }));
                        }
                    });
                    ui.add_space(10.0);
                }
            });
            ui.add_space(8.0);
        }
    });
}

// ------------------------------------------------------------------------------------------------
// Entry point
// ------------------------------------------------------------------------------------------------

/// Draw CrStudio in `rect`.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: egui::Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let mut acts: Vec<Act> = Vec::new();

    PROBE.with(|pr| {
        if pr.borrow().is_none() {
            *pr.borrow_mut() = Some((app.session.media.clone(), app.session.services.clone()));
        }
    });
    // reopen the last project once
    let ask = UI.with(|u| {
        let mut u = u.borrow_mut();
        let first = !u.asked_restore;
        u.asked_restore = true;
        first
    });
    if ask && !app.session.crstudio.restored {
        let _ = app.session.execute("crstudio.openLast", json!({}));
    }
    if app.session.crstudio.worker_running || !app.session.crstudio.pipeline.queue.is_empty() {
        filmcraft_engine::crstudio::poll(&mut app.session);
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }

    let p = app.session.crstudio.pipeline.clone();
    let header_h = 54.0;
    let rail_w = 196.0;
    let feed_w = if rect.width() > 1250.0 { 310.0 } else { 0.0 };
    let header = egui::Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
    let rail = egui::Rect::from_min_max(pos2(rect.min.x, header.max.y), pos2(rect.min.x + rail_w, rect.max.y));
    let feed = egui::Rect::from_min_max(pos2(rect.max.x - feed_w, header.max.y), rect.max);
    let body = egui::Rect::from_min_max(pos2(rail.max.x, header.max.y), pos2(feed.min.x, rect.max.y));

    draw_header(app, ui, header, &p, &mut acts);
    draw_rail(app, ui, rail, &p, &mut acts);
    if feed_w > 0.0 {
        draw_feed(app, ui, feed, &p);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(22.0, 16.0))).layout(Layout::top_down(Align::Min)));
    if UI.with(|u| u.borrow().studio_open) {
        recording_studio(app, &mut child, &p, &mut acts);
        apply(app, &ctx, acts);
        return;
    }
    let (prod_open, board_open) = UI.with(|u| {
        let u = u.borrow();
        (u.prod_open, u.board_open)
    });
    if prod_open {
        production_view(app, &mut child, &p, &mut acts);
        apply(app, &ctx, acts);
        return;
    }
    if board_open {
        board_view(app, &mut child, &p, &mut acts);
        apply(app, &ctx, acts);
        return;
    }
    match p.stage {
        PipelineStage::Setup => stage_setup(app, &mut child, &p, &mut acts),
        PipelineStage::RunningAgents => stage_agents(app, &mut child, &p, &mut acts),
        PipelineStage::ReviewScenes => stage_scenes(app, &mut child, &p, &mut acts),
        PipelineStage::Voiceover => stage_voice(app, &mut child, &p, &mut acts),
        PipelineStage::InStudio => stage_studio(app, &mut child, &p, &mut acts),
        PipelineStage::Publish => stage_publish(app, &mut child, &p, &mut acts),
    }
    apply(app, &ctx, acts);
}

fn draw_header(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: egui::Rect, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 0.0, t.header_bg);
    ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.max.x, r.max.y)], Stroke::new(1.0, t.separator));
    crate::brand::paint_logo_mark(ui.painter(), egui::Rect::from_min_size(r.min + vec2(18.0, 11.0), vec2(32.0, 32.0)));
    ui.painter().text(r.min + vec2(58.0, 14.0), Align2::LEFT_TOP, "CrStudio", Tokens::semibold(17.0), t.text);
    let sub = if p.title.is_empty() { "Yeni proje".to_string() } else { format!("{} · {} · {}", p.title, p.format.label(), p.tone.label()) };
    ui.painter().text(r.min + vec2(58.0, 35.0), Align2::LEFT_TOP, crate::panels::crstudio::clip_text(&sub, 90), Tokens::ui(11.5), t.text_dim);

    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(16.0, 10.0))).layout(Layout::right_to_left(Align::Center)));
    if let Some(d) = &p.director {
        let txt = format!("Yayınlanabilirlik {:.0}/100", d.score);
        let resp = right.add(
            egui::Button::new(RichText::new(txt).font(Tokens::semibold(12.5)).color(Color32::WHITE))
                .fill(score_color(d.score))
                .corner_radius(CornerRadius::same(14)),
        );
        app.auto.add("crstudio.header.score", resp.rect, "Yayınlanabilirlik");
        if resp.on_hover_text(d.verdict.clone()).clicked() {
            acts.push(Act::Stage(PipelineStage::Publish));
        }
    }
    if p.claude_cli_available {
        pill(&mut right, "● Claude bağlı", GREEN);
    } else {
        pill(&mut right, "● Claude bulunamadı", RED);
    }
    if p.job.running {
        right.add_space(8.0);
        if button(app, &mut right, "crstudio.job.cancel", "İptal") {
            run(acts, "crstudio.cancel", json!({}));
        }
        right.add(
            egui::ProgressBar::new(p.job.progress as f32).desired_width(220.0).text(RichText::new(p.job.label.clone()).font(Tokens::ui(11.0))).animate(true),
        );
    } else if !p.job.error.is_empty() {
        right.label(RichText::new(clip_text(&p.job.error, 70)).font(Tokens::ui(11.5)).color(RED));
    }
}

/// Shorten text for one line.
pub fn clip_text(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

fn stage_ready(p: &CrStudioPipeline, s: PipelineStage) -> bool {
    match s {
        PipelineStage::Setup | PipelineStage::RunningAgents => true,
        _ => !p.scenes.is_empty(),
    }
}

fn draw_rail(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: egui::Rect, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 0.0, t.app_bg);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(12.0, 16.0))).layout(Layout::top_down(Align::Min)));
    c.label(RichText::new("AŞAMALAR").font(Tokens::semibold(10.5)).color(t.text_faint));
    c.add_space(8.0);
    let cur = p.stage.number();
    for st in PipelineStage::ALL {
        let n = st.number();
        let ready = stage_ready(p, st);
        let active = st == p.stage;
        let done = n < cur;
        let (rect, resp) = c.allocate_exact_size(vec2(c.available_width(), 40.0), if ready { Sense::click() } else { Sense::hover() });
        app.auto.add(&format!("crstudio.stage.{n}"), rect, st.label());
        if active {
            c.painter().rect_filled(rect, CornerRadius::same(8), alpha(RED, 40));
        } else if resp.hovered() && ready {
            c.painter().rect_filled(rect, CornerRadius::same(8), t.hover);
        }
        let dot = pos2(rect.min.x + 18.0, rect.center().y);
        let col = if active {
            RED
        } else if done {
            GREEN
        } else if ready {
            t.text_dim
        } else {
            t.text_faint
        };
        c.painter().circle_filled(dot, 11.0, alpha(col, if active || done { 255 } else { 60 }));
        c.painter().text(
            dot,
            Align2::CENTER_CENTER,
            if done && !active { "✓".to_string() } else { n.to_string() },
            Tokens::semibold(11.0),
            if active || done { Color32::WHITE } else { t.text },
        );
        c.painter().text(
            pos2(rect.min.x + 38.0, rect.center().y),
            Align2::LEFT_CENTER,
            st.label(),
            if active { Tokens::semibold(13.5) } else { Tokens::ui(13.0) },
            if ready { t.text } else { t.text_faint },
        );
        if resp.clicked() && ready {
            if st == PipelineStage::InStudio && p.assembled_sequence.is_some() && p.stage == PipelineStage::InStudio {
                acts.push(Act::Mode(Mode::Edit));
            } else {
                acts.push(Act::Stage(st));
            }
        }
        c.add_space(2.0);
    }
    c.add_space(10.0);
    // the two working views beside the stages
    let (prod_open, board_open) = UI.with(|u| {
        let u = u.borrow();
        (u.prod_open, u.board_open)
    });
    let queued = p.queue.len();
    let prod_label = if queued > 0 { format!("⚙ Üretim Hattı · {queued} iş") } else { "⚙ Üretim Hattı".to_string() };
    let issues = filmcraft_engine::crstudio::production::scene_issues(p).iter().filter(|i| i.severity >= 2).count();
    let board_label = if issues > 0 { format!("▦ Sahne panosu · {issues} uyarı") } else { "▦ Sahne panosu".to_string() };
    for (id, label, open, which) in [("crstudio.rail.production", prod_label, prod_open, 0), ("crstudio.rail.board", board_label, board_open, 1)] {
        let b = c.add_sized(
            vec2(c.available_width(), 30.0),
            egui::Button::new(RichText::new(label.clone()).font(Tokens::semibold(12.5)).color(if open { Color32::WHITE } else { t.text }))
                .fill(if open { alpha(RED, 200) } else { t.tl_header_bg })
                .corner_radius(CornerRadius::same(8)),
        );
        app.auto.add(id, b.rect, &label);
        if b.clicked() {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                if which == 0 {
                    u.prod_open = !open;
                    u.board_open = false;
                } else {
                    u.board_open = !open;
                    u.prod_open = false;
                }
            });
        }
        c.add_space(4.0);
    }
    let mut expert = !app.ui.simple_mode;
    let e = c
        .checkbox(&mut expert, RichText::new("Uzman modu").font(Tokens::ui(12.0)))
        .on_hover_text("Editörün İçe Aktar ve Dışa Aktar ekranlarını da üst çubukta gösterir");
    app.auto.add("crstudio.rail.expert", e.rect, "Uzman modu");
    app.ui.simple_mode = !expert;
    c.add_space(12.0);
    c.separator();
    c.add_space(8.0);
    if button(app, &mut c, "crstudio.rail.new", "＋ Yeni proje") {
        acts.push(Act::Stage(PipelineStage::Setup));
        UI.with(|u| u.borrow_mut().show_projects = false);
        app.session.crstudio.pipeline.scenes.clear();
        app.session.crstudio.pipeline.title.clear();
        app.session.crstudio.pipeline.project_dir.clear();
        app.session.crstudio.pipeline.director = None;
    }
    if button(app, &mut c, "crstudio.rail.projects", "Projelerim…") {
        let list = app.session.execute("crstudio.list", json!({})).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        UI.with(|u| {
            let mut u = u.borrow_mut();
            u.projects = list;
            u.show_projects = true;
        });
        acts.push(Act::Stage(PipelineStage::Setup));
    }
    if !p.project_dir.is_empty() && button(app, &mut c, "crstudio.rail.folder", "Proje klasörünü aç") {
        acts.push(Act::Reveal(p.project_dir.clone()));
    }
    if p.assembled_sequence.is_some() && button(app, &mut c, "crstudio.rail.editor", "Kurgu masasına geç →") {
        acts.push(Act::Mode(Mode::Edit));
    }
}

fn draw_feed(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: egui::Rect, p: &CrStudioPipeline) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 0.0, t.app_bg);
    ui.painter().line_segment([pos2(r.min.x, r.min.y), pos2(r.min.x, r.max.y)], Stroke::new(1.0, t.separator));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(12.0, 14.0))).layout(Layout::top_down(Align::Min)));
    c.label(RichText::new("AJANLAR · CANLI").font(Tokens::semibold(10.5)).color(t.text_faint));
    c.add_space(6.0);
    egui::ScrollArea::vertical().id_salt("crs-feed").auto_shrink([false, false]).show(&mut c, |c| {
        for a in &p.agents {
            let (col, badge) = match a.status {
                AgentStatus::Idle => (t.text_faint, "bekliyor"),
                AgentStatus::Running => (AMBER, "çalışıyor"),
                AgentStatus::Done => (GREEN, "tamam"),
                AgentStatus::Error => (RED, "sorun"),
                AgentStatus::Skipped => (GREY, "atlandı"),
            };
            c.horizontal(|c| {
                let (dot, _) = c.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                c.painter().circle_filled(dot.center(), 4.5, col);
                c.label(RichText::new(format!("{}. {}", a.index, a.name)).font(Tokens::semibold(12.0)).color(t.text));
                c.with_layout(Layout::right_to_left(Align::Center), |c| {
                    c.label(RichText::new(badge).font(Tokens::ui(10.5)).color(col));
                });
            });
            if !a.summary.is_empty() && a.status != AgentStatus::Idle {
                c.label(RichText::new(clip_text(&a.summary, 120)).font(Tokens::ui(11.0)).color(t.text_dim));
            }
            if a.status == AgentStatus::Running {
                for line in a.activity.iter().rev().take(4).rev() {
                    c.label(RichText::new(clip_text(line, 90)).font(Tokens::ui(10.5)).color(t.text_dim));
                }
            }
            c.add_space(6.0);
        }
        c.separator();
        c.label(RichText::new("GÜNLÜK").font(Tokens::semibold(10.5)).color(t.text_faint));
        for log in p.logs.iter().rev().take(60) {
            let col = match log.level.as_str() {
                "ok" => GREEN,
                "error" => RED,
                _ => t.text_dim,
            };
            c.label(RichText::new(format!("{} · {}", log.agent, clip_text(&log.message, 140))).font(Tokens::ui(10.5)).color(col));
        }
    });
}

// ------------------------------------------------------------------------------------------------
// 1. Konu
// ------------------------------------------------------------------------------------------------

fn stage_setup(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let (show_projects, projects) = UI.with(|u| {
        let u = u.borrow();
        (u.show_projects, u.projects.clone())
    });
    egui::ScrollArea::vertical().id_salt("crs-setup").auto_shrink([false, false]).show(ui, |ui| {
        if show_projects {
            card(ui, &t, false, |ui| {
                ui.horizontal(|ui| {
                    section_title(ui, &t, "Projelerim");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Kapat").clicked() {
                            UI.with(|u| u.borrow_mut().show_projects = false);
                        }
                    });
                });
                if projects.is_empty() {
                    hint(ui, &t, "Henüz kayıtlı proje yok.");
                }
                for pr in projects.iter().take(40) {
                    ui.horizontal(|ui| {
                        let title = pr["title"].as_str().unwrap_or("");
                        if ui.link(RichText::new(title).font(Tokens::semibold(13.0))).clicked() {
                            run(acts, "crstudio.open", json!({"dir": pr["dir"]}));
                            UI.with(|u| u.borrow_mut().show_projects = false);
                        }
                        ui.label(
                            RichText::new(format!("{} · {} · {} sahne", pr["format"].as_str().unwrap_or(""), pr["stage"].as_str().unwrap_or(""), pr["scenes"]))
                                .font(Tokens::ui(11.0))
                                .color(t.text_dim),
                        );
                    });
                }
            });
            ui.add_space(14.0);
        }

        ui.label(RichText::new("Bugün hangi hikâyeyi anlatıyoruz?").font(Tokens::semibold(22.0)).color(t.text));
        hint(
            ui,
            &t,
            "Konuyu ve istediklerinizi yazın. Ajanlar araştırır, senaryoyu yazar, görselleri bulur ve haritaları çizer; siz her aşamada seçip onaylarsınız.",
        );
        ui.add_space(12.0);

        let w = ui.available_width().min(900.0);
        let pl = &mut app.session.crstudio.pipeline;
        let title_resp = ui.add(
            egui::TextEdit::singleline(&mut pl.title)
                .hint_text("Örn: Yedi Uyurlar efsanesi — Efes'ten Afşin'e mağaranın sırrı")
                .font(Tokens::ui(17.0))
                .desired_width(w)
                .margin(Margin::symmetric(12, 10)),
        );
        app.auto.add("crstudio.setup.title", title_resp.rect, "Konu");
        ui.add_space(8.0);
        let pl = &mut app.session.crstudio.pipeline;
        let notes = ui.add(
            egui::TextEdit::multiline(&mut pl.extra_instructions)
                .hint_text("Detaylar (isteğe bağlı): hangi açıdan anlatılsın, mutlaka geçmesi gerekenler, hedef izleyici, kaçınılacaklar…")
                .desired_rows(3)
                .desired_width(w),
        );
        app.auto.add("crstudio.setup.notes", notes.rect, "Detaylar");
        ui.add_space(16.0);

        section_title(ui, &t, "Format");
        ui.horizontal_wrapped(|ui| {
            for f in VideoFormatPreset::ALL {
                let sel = app.session.crstudio.pipeline.format == f;
                let (rect, resp) = ui.allocate_exact_size(vec2(200.0, 86.0), Sense::click());
                app.auto.add(&format!("crstudio.format.{}", f.id()), rect, f.label());
                ui.painter().rect(
                    rect,
                    CornerRadius::same(10),
                    if sel {
                        alpha(RED, 36)
                    } else if resp.hovered() {
                        t.hover
                    } else {
                        t.tl_header_bg
                    },
                    Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { RED } else { t.separator }),
                    egui::StrokeKind::Inside,
                );
                let (aw, ah) = match f {
                    VideoFormatPreset::YoutubeShorts | VideoFormatPreset::InstagramReels => (20.0, 34.0),
                    VideoFormatPreset::YoutubeVideo => (40.0, 23.0),
                    VideoFormatPreset::InstagramVideo => (26.0, 32.0),
                };
                let ir = egui::Rect::from_center_size(pos2(rect.min.x + 30.0, rect.center().y), vec2(aw, ah));
                ui.painter().rect_stroke(ir, CornerRadius::same(4), Stroke::new(2.0, if sel { RED } else { t.text_dim }), egui::StrokeKind::Inside);
                ui.painter().text(pos2(rect.min.x + 58.0, rect.min.y + 30.0), Align2::LEFT_CENTER, f.label(), Tokens::semibold(13.0), t.text);
                ui.painter().text(pos2(rect.min.x + 58.0, rect.min.y + 52.0), Align2::LEFT_CENTER, f.aspect_label(), Tokens::ui(11.0), t.text_dim);
                if resp.clicked() {
                    let pl = &mut app.session.crstudio.pipeline;
                    pl.format = f;
                    pl.target_duration_sec = f.default_duration_sec();
                }
            }
        });
        ui.add_space(14.0);

        section_title(ui, &t, "Anlatım tonu");
        ui.horizontal_wrapped(|ui| {
            for tone in DocTone::ALL {
                let sel = app.session.crstudio.pipeline.tone == tone;
                let resp = ui.add(
                    egui::Button::new(RichText::new(tone.label()).font(Tokens::ui(12.5)).color(if sel { Color32::WHITE } else { t.text }))
                        .fill(if sel { RED } else { t.tl_header_bg })
                        .corner_radius(CornerRadius::same(14)),
                );
                app.auto.add(&format!("crstudio.tone.{}", tone.id()), resp.rect, tone.label());
                if resp.on_hover_text(tone.description()).clicked() {
                    let pl = &mut app.session.crstudio.pipeline;
                    pl.tone = tone;
                    pl.bgm_style = tone.default_bgm().into();
                }
            }
        });
        hint(ui, &t, app.session.crstudio.pipeline.tone.description());
        ui.add_space(14.0);

        section_title(ui, &t, "Süre ve kanal");
        ui.horizontal(|ui| {
            let pl = &mut app.session.crstudio.pipeline;
            let max = pl.format.max_duration_sec().min(1800.0);
            ui.label("Hedef süre");
            let mut mins = pl.target_duration_sec;
            let r = ui.add(egui::Slider::new(&mut mins, 15.0..=max).step_by(5.0).custom_formatter(|v, _| {
                let s = v.round() as u64;
                format!("{}:{:02} dk", s / 60, s % 60)
            }));
            pl.target_duration_sec = mins;
            app.auto.add("crstudio.setup.duration", r.rect, "Hedef süre");
            ui.add_space(20.0);
            ui.label("Kanal adı");
            let pl = &mut app.session.crstudio.pipeline;
            let r = ui.add(egui::TextEdit::singleline(&mut pl.channel_name).desired_width(180.0));
            app.auto.add("crstudio.setup.channel", r.rect, "Kanal adı");
        });
        let est = {
            let pl = &app.session.crstudio.pipeline;
            let words = pl.target_duration_sec * 2.3 * 0.95;
            let per = if pl.format.is_vertical() { 13.0 } else { 20.0 };
            ((words / per).round() as usize).clamp(4, 64)
        };
        hint(ui, &t, &format!("≈ {est} sahne · ≈ {:.0} kelime anlatım", app.session.crstudio.pipeline.target_duration_sec * 2.3));
        ui.add_space(10.0);

        let adv = UI.with(|u| u.borrow().show_advanced);
        if ui.selectable_label(adv, if adv { "▾ Gelişmiş ayarlar" } else { "▸ Gelişmiş ayarlar" }).clicked() {
            UI.with(|u| u.borrow_mut().show_advanced = !adv);
        }
        if adv {
            card(ui, &t, false, |ui| {
                let pl = &mut app.session.crstudio.pipeline;
                ui.checkbox(&mut pl.relevance_check, "Görsel alaka denetimi (Claude görselleri inceler)");
                ui.checkbox(&mut pl.director_ai, "Yayın Yönetmeni içerik değerlendirmesinde Claude'u kullansın");
                ui.checkbox(&mut pl.youtuber_overlays, "YouTuber eklentilerini otomatik yerleştir");
                ui.checkbox(&mut pl.brand_kit_enabled, "Abone ol ve 'Bunu da izle' bitiş ekranı");
                ui.horizontal(|ui| {
                    ui.label("Claude süre sınırı (sn)");
                    let mut v = pl.claude_timeout_sec as f64;
                    ui.add(egui::Slider::new(&mut v, 120.0..=3600.0).step_by(60.0));
                    pl.claude_timeout_sec = v as u64;
                });
                ui.horizontal(|ui| {
                    ui.label("Projeler klasörü");
                    ui.add(egui::TextEdit::singleline(&mut pl.projects_root).desired_width(380.0));
                });
            });
        }
        ui.add_space(18.0);
        let can = !app.session.crstudio.pipeline.title.trim().is_empty() && !app.session.crstudio.worker_running;
        ui.horizontal(|ui| {
            if primary_button(app, ui, "crstudio.setup.start", "Ajanları başlat  →", can) {
                let pl = &app.session.crstudio.pipeline;
                let fresh = pl.project_dir.is_empty() || !pl.scenes.is_empty();
                let params = json!({
                    "title": pl.title, "format": pl.format.id(), "tone": pl.tone.id(),
                    "targetDurationSec": pl.target_duration_sec, "extraInstructions": pl.extra_instructions, "channelName": pl.channel_name,
                });
                if fresh {
                    run(acts, "crstudio.new", params.clone());
                }
                run(acts, "crstudio.runPipeline", json!({}));
            }
            if !p.claude_cli_available {
                ui.label(RichText::new("Claude Code CLI bulunamadı: ajanlar yalnız boş bir iskelet üretebilir.").font(Tokens::ui(11.5)).color(RED));
            }
        });
    });
}

// ------------------------------------------------------------------------------------------------
// 2. Ajanlar
// ------------------------------------------------------------------------------------------------

fn stage_agents(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.horizontal(|ui| {
        if p.job.running {
            ui.add(egui::Spinner::new().size(18.0));
        }
        ui.label(
            RichText::new(if p.job.running { format!("Ajanlar çalışıyor: {}", p.job.label) } else { "Ajanlar bitirdi".into() })
                .font(Tokens::semibold(18.0))
                .color(t.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !p.job.running && !p.scenes.is_empty() && primary_button(app, ui, "crstudio.agents.next", "Sahneleri incele →", true) {
                acts.push(Act::Stage(PipelineStage::ReviewScenes));
            }
        });
    });
    ui.add(egui::ProgressBar::new(p.job.progress as f32).desired_width(ui.available_width()).show_percentage());
    ui.add_space(12.0);
    egui::ScrollArea::vertical().id_salt("crs-agents").auto_shrink([false, false]).show(ui, |ui| {
        let cols = if ui.available_width() > 900.0 { 3 } else { 2 };
        let w = (ui.available_width() - 14.0 * (cols as f32 - 1.0)) / cols as f32;
        for row in p.agents.chunks(cols) {
            ui.horizontal(|ui| {
                for a in row {
                    // a top-down column per card (the row is horizontal; its children must not be)
                    ui.allocate_ui_with_layout(vec2(w, 170.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(w);
                        ui.set_max_width(w);
                        card(ui, &t, a.status == AgentStatus::Running, |ui| {
                            ui.set_min_height(140.0);
                            let (col, badge) = match a.status {
                                AgentStatus::Idle => (t.text_faint, "Sırada"),
                                AgentStatus::Running => (AMBER, "Çalışıyor"),
                                AgentStatus::Done => (GREEN, "Tamam"),
                                AgentStatus::Error => (RED, "Sorun var"),
                                AgentStatus::Skipped => (GREY, "Atlandı"),
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{}  {}", a.index, a.name)).font(Tokens::semibold(14.0)).color(t.text));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| pill(ui, badge, col));
                            });
                            ui.label(RichText::new(&a.role).font(Tokens::ui(11.0)).color(t.text_faint));
                            ui.add_space(4.0);
                            if !a.summary.is_empty() {
                                ui.label(RichText::new(clip_text(&a.summary, 160)).font(Tokens::ui(12.0)).color(col));
                            }
                            for line in a.activity.iter().rev().take(4).rev() {
                                ui.label(RichText::new(clip_text(line, 70)).font(Tokens::ui(11.0)).color(t.text_dim));
                            }
                        });
                    });
                }
            });
            ui.add_space(10.0);
        }
        if !p.research.summary.is_empty() {
            card(ui, &t, false, |ui| {
                section_title(ui, &t, "Araştırma özeti");
                ui.label(RichText::new(&p.research.summary).font(Tokens::ui(12.5)).color(t.text));
                for (i, s) in p.research.sources.iter().enumerate().take(15) {
                    if ui.link(RichText::new(format!("[{}] {}", i + 1, s.title)).font(Tokens::ui(11.5))).clicked() {
                        acts.push(Act::OpenUrl(s.url.clone()));
                    }
                }
            });
        }
    });
}

// ------------------------------------------------------------------------------------------------
// 3. Sahneler
// ------------------------------------------------------------------------------------------------

fn origin_badge(ui: &mut egui::Ui, sc: &SceneSpec) {
    match sc.text_origin.as_str() {
        "template" => pill(ui, "taslak metin", RED),
        "user" => pill(ui, "sizin metniniz", BLUE),
        _ => {}
    }
    match sc.fact_kind.as_str() {
        "rivayet" => pill(ui, "rivayet", AMBER),
        "belge" if sc.sources.is_empty() => pill(ui, "kaynaksız", RED),
        "belge" => pill(ui, "kaynaklı", GREEN),
        _ => {}
    }
}

fn stage_scenes(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let n = p.scenes.len();
    let approved = p.scenes.iter().filter(|s| s.approved).count();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("Sahneler · {n} sahne · {:.0}:{:02.0} dk", (p.total_seconds() / 60.0).floor(), p.total_seconds() % 60.0))
                .font(Tokens::semibold(18.0))
                .color(t.text),
        );
        ui.label(RichText::new(format!("{approved}/{n} onaylı")).font(Tokens::ui(12.0)).color(t.text_dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(app, ui, "crstudio.scenes.approveAll", "Onayla ve seslendirmeye geç →", !app.session.crstudio.worker_running) {
                run(acts, "crstudio.approveAll", json!({"approved": true}));
            }
            if button(app, ui, "crstudio.scenes.huntAll", "Tüm görselleri yeniden ara") {
                run(acts, "crstudio.huntVisuals", json!({}));
            }
        });
    });
    ui.add_space(8.0);
    let active = p.active_scene_index.min(n.saturating_sub(1));
    let list_w = 290.0_f32.min(ui.available_width() * 0.34);
    let h = ui.available_height();
    ui.horizontal_top(|ui| {
        // Scene list
        ui.allocate_ui_with_layout(vec2(list_w, h), Layout::top_down(Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("crs-scene-list").auto_shrink([false, false]).show(ui, |ui| {
                let mut last_chapter = String::new();
                for sc in &p.scenes {
                    if sc.chapter != last_chapter && !sc.chapter.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(filmcraft_engine::crstudio::turkish::upper(&sc.chapter)).font(Tokens::semibold(10.5)).color(t.text_faint));
                        last_chapter = sc.chapter.clone();
                    }
                    let sel = sc.index == active;
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
                    app.auto.add(&format!("crstudio.scene.{}", sc.index), rect, &sc.title);
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(8),
                        if sel {
                            alpha(RED, 40)
                        } else if resp.hovered() {
                            t.hover
                        } else {
                            Color32::TRANSPARENT
                        },
                    );
                    let tr = egui::Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(88.0, 50.0));
                    let mut tu = ui.new_child(egui::UiBuilder::new().max_rect(tr));
                    paint_thumb(&mut tu, sc.selected_visual.as_deref(), tr.size(), "—");
                    ui.painter().text(
                        pos2(tr.max.x + 8.0, rect.min.y + 14.0),
                        Align2::LEFT_CENTER,
                        clip_text(&format!("{}. {}", sc.index + 1, sc.title), 26),
                        Tokens::semibold(12.0),
                        t.text,
                    );
                    let status = format!(
                        "{:.1} sn · {} · {}",
                        sc.effective_duration(),
                        if sc.voiceover_path.is_some() { if sc.voiceover_engine == "record" { "kayıt ✓" } else { "AI ses" } } else { "ses yok" },
                        if sc.approved { "onaylı" } else { "bekliyor" }
                    );
                    ui.painter().text(pos2(tr.max.x + 8.0, rect.min.y + 34.0), Align2::LEFT_CENTER, status, Tokens::ui(10.5), t.text_dim);
                    if sc.text_origin == "template" {
                        ui.painter().circle_filled(pos2(rect.max.x - 10.0, rect.min.y + 12.0), 4.0, RED);
                    }
                    if resp.clicked() {
                        app.session.crstudio.pipeline.active_scene_index = sc.index;
                    }
                }
                ui.add_space(6.0);
                if button(app, ui, "crstudio.scenes.add", "＋ Sahne ekle") {
                    run(acts, "crstudio.addScene", json!({"after": active}));
                }
            });
        });
        ui.separator();
        // Scene editor
        let Some(sc) = p.scenes.get(active).cloned() else { return };
        let edit_w = ui.available_width();
        ui.allocate_ui_with_layout(vec2(edit_w, h), Layout::top_down(Align::Min), |ui| {
            ui.set_max_width(edit_w);
            let clip = ui.clip_rect().intersect(ui.max_rect());
            ui.set_clip_rect(clip);
            egui::ScrollArea::vertical().id_salt("crs-scene-edit").auto_shrink([false, false]).show(ui, |ui| scene_editor(app, ui, p, &sc, acts));
        });
    });
}

fn scene_editor(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, sc: &SceneSpec, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let i = sc.index;
    // wrapped rows (thumbnails, chips) must wrap at the editor's edge, not the window's
    let edge = ui.available_width();
    ui.set_max_width(edge);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Sahne {} · {}", i + 1, sc.title)).font(Tokens::semibold(17.0)).color(t.text));
        origin_badge(ui, sc);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Sil").clicked() {
                run(acts, "crstudio.removeScene", json!({"sceneIndex": i}));
            }
            if ui.small_button("↓").on_hover_text("Aşağı taşı").clicked() {
                run(acts, "crstudio.moveScene", json!({"sceneIndex": i, "to": i + 1}));
            }
            if i > 0 && ui.small_button("↑").on_hover_text("Yukarı taşı").clicked() {
                run(acts, "crstudio.moveScene", json!({"sceneIndex": i, "to": i - 1}));
            }
            let mut ok = sc.approved;
            if ui.checkbox(&mut ok, "Onaylı").changed() {
                run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "approved": ok}));
            }
        });
    });
    ui.add_space(6.0);
    // Narration
    let mut text = sc.narration_text.clone();
    let r = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(ui.available_width()).font(Tokens::ui(15.0)));
    app.auto.add("crstudio.scene.narration", r.rect, "Anlatım");
    if r.changed() {
        run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "narrationText": text}));
    }
    hint(
        ui,
        &t,
        &format!(
            "{} kelime · ≈ {:.1} sn{}",
            sc.narration_text.split_whitespace().count(),
            sc.estimated_seconds(),
            if sc.sync_audit_note.is_empty() { String::new() } else { format!(" · {}", sc.sync_audit_note) }
        ),
    );
    if !sc.sources.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Kaynak:").font(Tokens::ui(11.5)).color(t.text_dim));
            for s in &sc.sources {
                if let Some(src) = p.research.sources.get(*s)
                    && ui.link(RichText::new(clip_text(&src.title, 50)).font(Tokens::ui(11.5))).clicked()
                {
                    acts.push(Act::OpenUrl(src.url.clone()));
                }
            }
        });
    }
    ui.add_space(10.0);

    // Visual
    ui.horizontal(|ui| {
        section_title(ui, &t, "Görsel");
        egui::ComboBox::from_id_salt(("crs-vtype", i)).selected_text(vtype_label(&sc.visual_type)).show_ui(ui, |ui| {
            for v in ["stock_photo", "animated_map", "chapter_card", "evidence_highlight", "infographic_chart"] {
                if ui.selectable_label(sc.visual_type == v, vtype_label(v)).clicked() {
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "visualType": v}));
                }
            }
        });
    });
    let sel = sc.selected_visual.clone().unwrap_or_default();
    let big_w = (ui.available_width() * 0.62).min(560.0);
    ui.horizontal_top(|ui| {
        paint_thumb(ui, Some(&sel), vec2(big_w, big_w * 9.0 / 16.0), "Görsel yok");
        ui.vertical(|ui| {
            if let Some(c) = sc.visual_candidates.iter().find(|c| c.local_path == sel) {
                ui.label(RichText::new(clip_text(&c.title, 60)).font(Tokens::semibold(12.5)).color(t.text));
                ui.label(
                    RichText::new(format!("Lisans: {}", c.license))
                        .font(Tokens::ui(11.5))
                        .color(if filmcraft_engine::crstudio::director::license_allowed(&c.license) { GREEN } else { RED }),
                );
                if !c.author.is_empty() {
                    ui.label(RichText::new(format!("Yazar: {}", clip_text(&c.author, 60))).font(Tokens::ui(11.5)).color(t.text_dim));
                }
                if let Some(r) = c.relevance {
                    ui.label(RichText::new(format!("Alaka: {r:.0}/100  {}", c.relevance_note)).font(Tokens::ui(11.5)).color(score_color(r)));
                }
                if !c.page_url.is_empty() && ui.link("Kaynak sayfası").clicked() {
                    acts.push(Act::OpenUrl(c.page_url.clone()));
                }
            }
            ui.add_space(8.0);
            let mut q = sc.visual_query.clone();
            ui.label(RichText::new("Arama (İngilizce daha iyi sonuç verir)").font(Tokens::ui(11.0)).color(t.text_dim));
            let r = ui.add(egui::TextEdit::singleline(&mut q).desired_width(260.0));
            if r.lost_focus() && q != sc.visual_query {
                run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "visualQuery": q}));
            }
            if button(app, ui, "crstudio.scene.hunt", "Bu sahne için yeniden ara") {
                run(acts, "crstudio.huntVisuals", json!({"sceneIndex": i, "query": q}));
            }
            if button(app, ui, "crstudio.scene.userVisual", "Kendi görselimi ekle…") {
                acts.push(Act::PickUserVisual(i));
            }
        });
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for c in sc.visual_candidates.iter().take(10) {
            ui.vertical(|ui| {
                let resp = paint_thumb(ui, Some(&c.local_path), vec2(150.0, 84.0), "…");
                if c.local_path == sel {
                    ui.painter().rect_stroke(resp.rect, CornerRadius::same(6), Stroke::new(2.5, RED), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "selectedVisual": c.local_path}));
                }
                let tag = match c.kind.as_str() {
                    "wikimedia_commons" => "Wikimedia",
                    "openverse" => "Openverse",
                    "animated_map" => "Harita",
                    "user" => "Benim",
                    "blender" => "Blender",
                    _ => "Kart",
                };
                let rel = c.relevance.map(|r| format!(" · {r:.0}")).unwrap_or_default();
                ui.label(RichText::new(format!("{tag}{rel}")).font(Tokens::ui(10.5)).color(c.relevance.map_or(t.text_dim, score_color)));
            });
        }
    });

    // Blender shots in the channel's visual concept
    ui.add_space(8.0);
    card(ui, &t, false, |ui| {
        let bs = UI.with(|u| {
            let mut u = u.borrow_mut();
            if u.blender.is_none() {
                u.blender = app.session.execute("crstudio.blender.status", json!({})).ok();
            }
            u.blender.clone().unwrap_or(Value::Null)
        });
        ui.horizontal(|ui| {
            section_title(ui, &t, "Blender çekimi");
            if bs["found"].as_bool().unwrap_or(false) {
                pill(ui, &clip_text(bs["version"].as_str().unwrap_or("Blender"), 24), GREEN);
            } else {
                pill(ui, "Blender kurulu değil", AMBER);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Konsept:").font(Tokens::ui(11.5)).color(t.text_dim));
            for c in bs["concepts"].as_array().cloned().unwrap_or_default() {
                let id = c["id"].as_str().unwrap_or("").to_string();
                let sel = p.visual_concept == id;
                let r = ui.selectable_label(sel, c["label"].as_str().unwrap_or("")).on_hover_text(c["about"].as_str().unwrap_or(""));
                app.auto.add(&format!("crstudio.concept.{id}"), r.rect, &id);
                if r.clicked() {
                    run(acts, "crstudio.setConcept", json!({"concept": id}));
                }
            }
        });
        let ok = bs["found"].as_bool().unwrap_or(false) && !app.session.crstudio.worker_running;
        ui.horizontal_wrapped(|ui| {
            for (shot, label) in [("map", "Harita / rota"), ("chapter", "Bölüm açılışı"), ("intro", "Kanal introsu")] {
                let r = ui.add_enabled(ok, egui::Button::new(label));
                app.auto.add(&format!("crstudio.scene.blender.{shot}"), r.rect, label);
                if r.on_hover_text("Blender arka planda üretir; bitince bu sahnenin görseli olur (önce taslak kalite için Shift)").clicked() {
                    let draft = ui.input(|i| i.modifiers.shift);
                    run(acts, "crstudio.blender.render", json!({"sceneIndex": i, "shot": shot, "quality": if draft { "draft" } else { "final" }}));
                }
            }
        });
        if !bs["found"].as_bool().unwrap_or(false) {
            hint(ui, &t, "Blender'ı blender.org'dan indirip Uygulamalar klasörüne kurun (ücretsiz); CrStudio onu arka planda, pencere açmadan çalıştırır.");
        }
    });

    // Map
    if sc.visual_type == "animated_map" || sc.map_spec.is_some() {
        ui.add_space(8.0);
        card(ui, &t, false, |ui| {
            section_title(ui, &t, "Harita");
            let ms = sc.map_spec.clone().unwrap_or_default();
            let (mut name, mut lat, mut lon, mut zoom) = (ms.location_name.clone(), ms.lat, ms.lon, ms.zoom);
            ui.horizontal(|ui| {
                ui.label("Yer");
                ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
                ui.label("Enlem");
                ui.add(egui::DragValue::new(&mut lat).speed(0.05).range(-85.0..=85.0));
                ui.label("Boylam");
                ui.add(egui::DragValue::new(&mut lon).speed(0.05).range(-180.0..=180.0));
                ui.label("Yakınlık");
                ui.add(egui::DragValue::new(&mut zoom).speed(0.05).range(1.0..=8.0));
                if button(app, ui, "crstudio.scene.map", "Haritayı çiz") {
                    run(acts, "crstudio.generateMap", json!({"sceneIndex": i, "locationName": name, "lat": lat, "lon": lon, "zoom": zoom}));
                }
            });
        });
    }

    // Overlays
    ui.add_space(8.0);
    card(ui, &t, false, |ui| {
        section_title(ui, &t, "Ekran yazıları ve YouTuber eklentileri");
        ui.horizontal(|ui| {
            ui.label("Başlık");
            let mut ot = sc.overlay_title.clone();
            if ui.add(egui::TextEdit::singleline(&mut ot).desired_width(260.0)).changed() {
                run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "overlayTitle": ot}));
            }
            ui.label("Alt satır");
            let mut os = sc.overlay_subtitle.clone();
            if ui.add(egui::TextEdit::singleline(&mut os).desired_width(260.0)).changed() {
                run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "overlaySubtitle": os}));
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (id, label) in [
                ("info_box", "Bilgi kutusu"),
                ("warning_box", "Uyarı kutusu"),
                ("speech_bubble", "Konuşma balonu"),
                ("comment_prompt", "Yorumda belirt"),
                ("like_reminder", "Beğen"),
                ("source_credit", "Kaynak"),
                ("timeline_tag", "Tarih etiketi"),
                ("countdown", "Geri sayım"),
                ("shock_badge", "Vurgu etiketi"),
            ] {
                let on = sc.overlays.iter().any(|o| o == id);
                let resp = ui.add(
                    egui::Button::new(RichText::new(label).font(Tokens::ui(11.5)).color(if on { Color32::WHITE } else { t.text }))
                        .fill(if on { BLUE } else { t.tl_header_bg })
                        .corner_radius(CornerRadius::same(12)),
                );
                app.auto.add(&format!("crstudio.overlay.{id}"), resp.rect, label);
                if resp.clicked() {
                    let mut list: Vec<String> = sc.overlays.clone();
                    if on {
                        list.retain(|o| o != id);
                    } else {
                        list.push(id.into());
                    }
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "overlays": list}));
                }
            }
        });
        if !sc.overlays.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Kutu/balon metni");
                let mut it = sc.info_text.clone();
                if ui.add(egui::TextEdit::singleline(&mut it).desired_width(420.0)).changed() {
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "infoText": it}));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Kamera");
            for (id, l) in
                [("zoom_in", "Yakınlaş"), ("zoom_out", "Uzaklaş"), ("pan_right", "Sağa kay"), ("pan_left", "Sola kay"), ("dramatic_push", "Dramatik itme")]
            {
                if ui.selectable_label(sc.camera_motion == id, l).clicked() {
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "cameraMotion": id}));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Ses efekti");
            for (id, l) in [
                ("whoosh", "Whoosh"),
                ("low_boom", "Darbe"),
                ("riser", "Yükseliş"),
                ("map_ping", "Harita sinyali"),
                ("archive_shutter", "Deklanşör"),
                ("none", "Yok"),
            ] {
                if ui.selectable_label(sc.sfx_cue == id, l).clicked() {
                    run(acts, "crstudio.updateScene", json!({"sceneIndex": i, "sfxCue": id}));
                }
            }
        });
    });
}

fn vtype_label(v: &str) -> &'static str {
    match v {
        "animated_map" => "Harita",
        "chapter_card" => "Bölüm kartı",
        "evidence_highlight" => "Alıntı/belge kartı",
        "infographic_chart" => "Veri grafiği",
        _ => "Fotoğraf/arşiv görseli",
    }
}

// ------------------------------------------------------------------------------------------------
// 4. Seslendirme
// ------------------------------------------------------------------------------------------------

/// Full-screen recording studio: a teleprompter for the scenes (or for clone reading), one-key
/// recording (Space), automatic clean-up with a report, and the voice-clone library.
fn recording_studio(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let tnow = now(ui);
    let (clone_mode, mut scene_i) = UI.with(|u| {
        let u = u.borrow();
        (u.studio_clone, u.studio_scene)
    });
    let status = UI.with(|u| {
        let mut u = u.borrow_mut();
        if u.clone_status.is_none() {
            u.clone_status = app.session.execute("crstudio.clone.status", json!({})).ok();
        }
        u.clone_status.clone().unwrap_or(Value::Null)
    });
    let n = p.scenes.len();
    scene_i = scene_i.min(n.saturating_sub(1));
    let scene_rec = p.recording_scene_index.is_some();
    let clone_rec = app.session.crstudio.clone_rec.is_some();
    let recording = scene_rec || clone_rec;
    let rec_started = UI.with(|u| u.borrow().record_started);
    let refresh = |u: &mut UiState| {
        u.clone_status = None;
        u.voices = None;
    };

    // header: mode, close
    ui.horizontal(|ui| {
        ui.label(RichText::new("Kayıt stüdyosu").font(Tokens::semibold(18.0)).color(t.text));
        ui.add_space(12.0);
        for (flag, label) in [(false, "Sahneler"), (true, "Klon okuması")] {
            let r = ui.add_enabled(
                !recording,
                egui::Button::new(RichText::new(label).color(if clone_mode == flag { Color32::WHITE } else { t.text }))
                    .fill(if clone_mode == flag { RED } else { t.tl_header_bg })
                    .corner_radius(CornerRadius::same(12)),
            );
            app.auto.add(&format!("crstudio.studio.mode.{}", if flag { "clone" } else { "scenes" }), r.rect, label);
            if r.clicked() {
                UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.studio_clone = flag;
                    u.studio_report = None;
                });
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui.add_enabled(!recording, egui::Button::new("Kapat ✕"));
            app.auto.add("crstudio.studio.close", r.rect, "Kapat");
            if r.clicked() {
                UI.with(|u| u.borrow_mut().studio_open = false);
            }
        });
    });
    ui.add_space(4.0);
    let minutes = status["minutes"].as_f64().unwrap_or(0.0).max(0.0) + 0.0;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Klon malzemesi: {minutes:.1} dk")).font(Tokens::ui(12.0)).color(t.text_dim));
        let frac = (minutes / 120.0).clamp(0.0, 1.0) as f32;
        ui.add(egui::ProgressBar::new(frac).desired_width(220.0).text(if minutes < 30.0 {
            format!("en az 30 dk · %{:.0}", minutes / 30.0 * 100.0)
        } else {
            "30 dk tamam · ideal 2 saat".to_string()
        }));
        hint(ui, &t, "Sahne kayıtları da kütüphaneye eklenir.");
    });
    ui.add_space(10.0);

    // the text to read
    let (title, text, can_prev, can_next) = if clone_mode {
        let cur = status["cursor"].as_u64().unwrap_or(0) as usize;
        let total = status["readingCount"].as_u64().unwrap_or(0) as usize;
        (format!("Okuma {} / {}", cur + 1, total.max(1)), status["text"].as_str().unwrap_or("").to_string(), cur > 0, cur + 1 < total)
    } else if let Some(sc) = p.scenes.get(scene_i) {
        (format!("Sahne {} / {} · {}", scene_i + 1, n, sc.title), sc.narration_text.clone(), scene_i > 0, scene_i + 1 < n)
    } else {
        ("Sahne yok".to_string(), "Önce bir proje oluşturun ya da Klon okuması moduna geçin.".to_string(), false, false)
    };
    let h = (ui.available_height() - 190.0).max(160.0);
    card(ui, &t, recording, |ui| {
        ui.set_min_height(h);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&title).font(Tokens::semibold(13.0)).color(t.text_dim));
            if !clone_mode
                && let Some(sc) = p.scenes.get(scene_i)
                && !sc.record_report.is_empty()
            {
                let col = if sc.record_report.starts_with("iyi") {
                    GREEN
                } else if sc.record_report.starts_with("orta") {
                    AMBER
                } else {
                    RED
                };
                pill(ui, &clip_text(&sc.record_report, 70), col);
            }
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical().id_salt("crs-prompter").max_height(h - 30.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&text).font(Tokens::ui(34.0)).color(if recording { Color32::WHITE } else { t.text })).wrap());
        });
    });
    ui.add_space(10.0);

    // controls
    let typing = ui.ctx().egui_wants_keyboard_input();
    let (space, left, right) = ui.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
    let mut toggle = space && !typing;
    let mut go: i64 = 0;
    ui.horizontal(|ui| {
        if ui.add_enabled(can_prev && !recording, egui::Button::new("← Önceki")).clicked() {
            go = -1;
        }
        let label = if recording {
            format!("■ Bitir  ({:.0} sn)   [Boşluk]", rec_started.map(|s| tnow - s).unwrap_or(0.0))
        } else {
            "● Kaydet   [Boşluk]".to_string()
        };
        let b = ui.add_sized(
            vec2(260.0, 44.0),
            egui::Button::new(RichText::new(label).font(Tokens::semibold(16.0)).color(Color32::WHITE)).fill(if recording { RED } else { alpha(RED, 210) }),
        );
        app.auto.add("crstudio.studio.record", b.rect, "Kaydet / Bitir");
        toggle |= b.clicked();
        if ui.add_enabled(can_next && !recording, egui::Button::new("Sonraki →")).clicked() {
            go = 1;
        }
        ui.add_space(16.0);
        let tb = ui.add_enabled(!recording, egui::Button::new("Seviye testi (5 sn)"));
        app.auto.add("crstudio.studio.levelTest", tb.rect, "Seviye testi");
        if tb.on_hover_text("5 saniye normal sesinizle konuşun; seviye ve oda gürültüsü ölçülür, kayıt saklanmaz.").clicked() {
            match app.session.execute("crstudio.clone.recordStart", json!({"test": true})) {
                Ok(_) => UI.with(|u| u.borrow_mut().record_started = Some(tnow)),
                Err(e) => app.status(format!("Kayıt: {e}")),
            }
        }
        if !clone_mode
            && let Some(sc) = p.scenes.get(scene_i).filter(|sc| sc.voiceover_engine == "record")
            && ui.button("Dinle").on_hover_text("Temizlenmiş kaydı sistem oynatıcısında açar").clicked()
            && let Some(path) = sc.voiceover_path.clone()
            && let Some(f) = app.hooks.open_path.as_mut()
        {
            let _ = f(&path, false);
        }
    });
    if recording {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    if !recording && (left || right) && !typing {
        go = if left && can_prev {
            -1
        } else if right && can_next {
            1
        } else {
            0
        };
    }
    // the level test stops itself after 5 s
    let test_running = app.session.crstudio.clone_rec.is_some_and(|(_, _, test)| test);
    if test_running && rec_started.is_some_and(|s| tnow - s >= 5.0) {
        toggle = true;
    }
    if toggle {
        if clone_rec {
            match app.session.execute("crstudio.clone.recordStop", json!({})) {
                Ok(r) => UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.studio_report = Some(r);
                    u.record_started = None;
                    refresh(&mut u);
                }),
                Err(e) => {
                    app.status(format!("Kayıt: {e}"));
                    UI.with(|u| u.borrow_mut().record_started = None);
                }
            }
        } else if scene_rec {
            match app.session.execute("crstudio.recordVoiceoverStop", json!({})) {
                Ok(r) => UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.studio_report = Some(r);
                    u.record_started = None;
                    refresh(&mut u);
                }),
                Err(e) => {
                    app.status(format!("Kayıt: {e}"));
                    UI.with(|u| u.borrow_mut().record_started = None);
                }
            }
        } else {
            let r = if clone_mode {
                app.session.execute("crstudio.clone.recordStart", json!({}))
            } else {
                app.session.execute("crstudio.recordVoiceoverStart", json!({"sceneIndex": scene_i}))
            };
            match r {
                Ok(_) => UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.record_started = Some(tnow);
                    u.studio_report = None;
                }),
                Err(e) => app.status(format!("Kayıt: {e}")),
            }
        }
    }
    if go != 0 {
        if clone_mode {
            let _ = app.session.execute("crstudio.clone.goto", json!({"delta": go}));
            UI.with(|u| refresh(&mut u.borrow_mut()));
        } else {
            let to = (scene_i as i64 + go).clamp(0, n.saturating_sub(1) as i64) as usize;
            UI.with(|u| u.borrow_mut().studio_scene = to);
        }
        UI.with(|u| u.borrow_mut().studio_report = None);
    }

    // last take's report
    if let Some(r) = UI.with(|u| u.borrow().studio_report.clone()) {
        let rep = &r["report"];
        let grade = rep["grade"].as_str().unwrap_or("");
        let col = match grade {
            "iyi" => GREEN,
            "orta" => AMBER,
            _ => RED,
        };
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            pill(ui, if r["test"].as_bool().unwrap_or(false) { "seviye testi" } else { "son kayıt" }, GREY);
            ui.label(
                RichText::new(format!(
                    "{} · konuşma {:.0} dB · oda gürültüsü {:.0} dB · {:.1} sn → {:.1} sn",
                    grade,
                    rep["rawSpeechDb"].as_f64().unwrap_or(0.0),
                    rep["floorDb"].as_f64().unwrap_or(0.0),
                    rep["rawSeconds"].as_f64().unwrap_or(0.0),
                    rep["cleanSeconds"].as_f64().unwrap_or(0.0)
                ))
                .font(Tokens::semibold(12.5))
                .color(col),
            );
        });
        for w in rep["warnings"].as_array().cloned().unwrap_or_default() {
            ui.label(RichText::new(format!("• {}", w.as_str().unwrap_or(""))).font(Tokens::ui(12.0)).color(AMBER));
        }
        if grade == "iyi" && !r["test"].as_bool().unwrap_or(false) {
            hint(ui, &t, "Temiz kayıt. Bekleme kısaltma, nefes/gürültü azaltma ve seviye ayarı uygulandı.");
        }
    }

    // clone tools
    if clone_mode {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!recording, egui::Button::new("Son kaydı sil")).clicked() {
                match app.session.execute("crstudio.clone.removeLast", json!({})) {
                    Ok(_) => app.status("Son klon kaydı silindi"),
                    Err(e) => app.status(format!("Klon: {e}")),
                }
                UI.with(|u| refresh(&mut u.borrow_mut()));
            }
            let open = UI.with(|u| u.borrow().clone_text_open);
            if ui.button(if open { "Kendi metnimi gizle" } else { "Kendi okuma metnimi ekle…" }).clicked() {
                UI.with(|u| u.borrow_mut().clone_text_open = !open);
            }
            let ex = ui.add_enabled(!recording && minutes > 0.0, egui::Button::new("ElevenLabs için dışa aktar"));
            app.auto.add("crstudio.studio.cloneExport", ex.rect, "Dışa aktar");
            if ex.on_hover_text("İyi/orta kalitedeki kayıtları 10 dakikalık FLAC parçalara birleştirir (~/.crstudio/klon/elevenlabs)").clicked() {
                match app.session.execute("crstudio.clone.export", json!({})) {
                    Ok(v) => {
                        app.status(format!("{} parça hazır", v["parts"].as_array().map_or(0, Vec::len)));
                        if let Some(d) = v["dir"].as_str() {
                            acts.push(Act::Reveal(d.to_string()));
                        }
                    }
                    Err(e) => app.status(format!("Klon: {e}")),
                }
            }
        });
        if UI.with(|u| u.borrow().clone_text_open) {
            hint(ui, &t, "Belgesel tonunda okuyacağınız kendi metinleriniz (paragrafları boş satırla ayırın). Hazır metinlerden önce gelir.");
            let mut txt = UI.with(|u| u.borrow().clone_text.clone()).unwrap_or_else(|| status["customText"].as_str().unwrap_or("").to_string());
            ui.add(egui::TextEdit::multiline(&mut txt).desired_rows(6).desired_width(ui.available_width()));
            UI.with(|u| u.borrow_mut().clone_text = Some(txt.clone()));
            if ui.button("Metni kaydet").clicked() {
                match app.session.execute("crstudio.clone.setText", json!({"text": txt})) {
                    Ok(v) => app.status(format!("{} okuma paragrafı", v["paragraphs"])),
                    Err(e) => app.status(format!("Klon: {e}")),
                }
                UI.with(|u| refresh(&mut u.borrow_mut()));
            }
        }
    }
}

/// ElevenLabs: API key, the narrator's cloned voice, model and delivery settings.
fn elevenlabs_card(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let t = app.tokens;
    let (open, status) = UI.with(|u| {
        let mut u = u.borrow_mut();
        if u.el_status.is_none() {
            u.el_status = app.session.execute("crstudio.elevenlabs.status", json!({})).ok();
        }
        (u.el_open, u.el_status.clone().unwrap_or(Value::Null))
    });
    let ready = status["ready"].as_bool().unwrap_or(false);
    card(ui, &t, false, |ui| {
        ui.horizontal(|ui| {
            let arrow = if open { "▾" } else { "▸" };
            let r = ui.add(egui::Button::new(RichText::new(format!("{arrow} ElevenLabs – kendi klon sesiniz")).font(Tokens::semibold(13.0))).frame(false));
            app.auto.add("crstudio.voice.elevenlabs", r.rect, "ElevenLabs");
            if r.clicked() {
                UI.with(|u| u.borrow_mut().el_open = !open);
            }
            if ready {
                pill(ui, &format!("hazır · {}", status["voiceName"].as_str().unwrap_or("")), GREEN);
            } else if status["hasKey"].as_bool().unwrap_or(false) {
                pill(ui, "ses seçin", AMBER);
            } else {
                pill(ui, "ayarlı değil", GREY);
            }
        });
        if !open {
            return;
        }
        hint(
            ui,
            &t,
            "Ücretli planda üretilen sesin ticari lisansı abonelik bitse de kalıcıdır. En gerçekçi sonuç: 1–3 saatlik temiz kayıtla Profesyonel Ses Klonu (Creator planı). Anahtar yalnızca bu Mac'te (~/.crstudio, yalnız size açık) saklanır.",
        );
        let mut msg: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label("API anahtarı");
            let mut key = UI.with(|u| u.borrow().el_key.clone());
            let hint_text = if status["hasKey"].as_bool().unwrap_or(false) {
                format!("kayıtlı (…{})", status["keyHint"].as_str().unwrap_or(""))
            } else {
                "sk_…".to_string()
            };
            let r = ui.add(egui::TextEdit::singleline(&mut key).password(true).hint_text(hint_text).desired_width(260.0));
            app.auto.add("crstudio.voice.elevenlabs.key", r.rect, "API anahtarı");
            UI.with(|u| u.borrow_mut().el_key = key.clone());
            if ui.button("Kaydet ve sesleri getir").clicked() {
                let mut args = json!({});
                if !key.trim().is_empty() {
                    args["apiKey"] = json!(key.trim());
                }
                match app.session.execute("crstudio.elevenlabs.configure", args).and_then(|_| app.session.execute("crstudio.elevenlabs.voices", json!({}))) {
                    Ok(v) => {
                        let list = v["voices"].as_array().cloned().unwrap_or_default();
                        msg = Some(format!("{} ses bulundu", list.len()));
                        UI.with(|u| {
                            let mut u = u.borrow_mut();
                            u.el_voices = list;
                            u.el_key.clear();
                        });
                    }
                    Err(e) => msg = Some(e.to_string()),
                }
            }
        });
        let voices = UI.with(|u| u.borrow().el_voices.clone());
        let mut set: Option<Value> = None;
        if !voices.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Ses:");
                for v in voices.iter().take(40) {
                    let id = v["id"].as_str().unwrap_or("");
                    let name = v["name"].as_str().unwrap_or("");
                    let own = v["own"].as_bool().unwrap_or(false);
                    let sel = status["voiceId"].as_str() == Some(id);
                    let label = if own { format!("★ {name}") } else { name.to_string() };
                    if ui.selectable_label(sel, label).on_hover_text(v["category"].as_str().unwrap_or("")).clicked() {
                        set = Some(json!({"voiceId": id, "voiceName": name}));
                    }
                }
            });
        } else if !status["voiceId"].as_str().unwrap_or("").is_empty() {
            ui.label(RichText::new(format!("Seçili ses: {}", status["voiceName"].as_str().unwrap_or(""))).font(Tokens::ui(11.5)).color(t.text_dim));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Model:");
            for m in status["models"].as_array().cloned().unwrap_or_default() {
                let id = m["id"].as_str().unwrap_or("").to_string();
                if ui.selectable_label(status["modelId"].as_str() == Some(id.as_str()), m["label"].as_str().unwrap_or("")).clicked() {
                    set = Some(json!({"modelId": id}));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (k, label, lo, hi, tip) in [
                ("stability", "Tutarlılık", 0.0, 1.0, "Düşük: daha duygulu ama değişken · Yüksek: daha düz ve tutarlı (belgesel için 0,45–0,6)"),
                ("similarity", "Benzerlik", 0.0, 1.0, "Klonunuza ne kadar sadık kalsın (0,8–0,9). Çok yüksek, kayıttaki gürültüyü de taklit edebilir."),
                ("style", "Stil", 0.0, 1.0, "Abartı; klonun kendi tonunu korumak için 0–0,2"),
                ("speed", "Hız", 0.7, 1.2, "1,0 doğal hız"),
            ] {
                let mut x = status[k].as_f64().unwrap_or(0.5);
                ui.label(label).on_hover_text(tip);
                let r = ui.add(egui::Slider::new(&mut x, lo..=hi).step_by(0.05).fixed_decimals(2));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    set = Some(json!({ k: x }));
                }
            }
        });
        if let Some(args) = set {
            match app.session.execute("crstudio.elevenlabs.configure", args) {
                Ok(_) => UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.el_status = None;
                    u.voices = None;
                }),
                Err(e) => msg = Some(e.to_string()),
            }
        }
        if let Some(m) = msg {
            app.status(format!("ElevenLabs: {m}"));
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.el_status = None;
                u.voices = None;
            });
        }
    });
}

/// The pronunciation dictionary and a "how will it be read?" tester.
fn lexicon_card(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let t = app.tokens;
    let open = UI.with(|u| u.borrow().lex_open);
    card(ui, &t, false, |ui| {
        let arrow = if open { "▾" } else { "▸" };
        let r = ui.add(egui::Button::new(RichText::new(format!("{arrow} Telaffuz sözlüğü ve okunuş testi")).font(Tokens::semibold(13.0))).frame(false));
        app.auto.add("crstudio.voice.lexicon", r.rect, "Telaffuz sözlüğü");
        if r.clicked() {
            UI.with(|u| u.borrow_mut().lex_open = !open);
        }
        if !open {
            return;
        }
        hint(
            ui,
            &t,
            "Sayılar, MS/MÖ, II. gibi sıra sayıları ve kısaltmalar otomatik açılır. Yabancı adları burada düzeltin: her satıra  Decius = Desiyus. Tüm ses motorlarında geçerlidir.",
        );
        let mut text = UI.with(|u| u.borrow().lex_text.clone()).unwrap_or_else(|| {
            app.session.execute("crstudio.lexicon.get", json!({})).ok().and_then(|v| v["text"].as_str().map(str::to_string)).unwrap_or_default()
        });
        let r = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(6).desired_width(ui.available_width()).code_editor());
        app.auto.add("crstudio.voice.lexicon.text", r.rect, "Sözlük");
        UI.with(|u| u.borrow_mut().lex_text = Some(text.clone()));
        if ui.button("Sözlüğü kaydet").clicked() {
            match app.session.execute("crstudio.lexicon.set", json!({"text": text})) {
                Ok(v) => app.status(format!("Telaffuz sözlüğü kaydedildi: {} kayıt", v["entries"])),
                Err(e) => app.status(format!("Sözlük: {e}")),
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut test = UI.with(|u| u.borrow().lex_test.clone());
            let r = ui.add(
                egui::TextEdit::singleline(&mut test)
                    .hint_text("Deneme: MS 249-251'de Decius, II. Theodosius, 9-26. ayetler")
                    .desired_width((ui.available_width() - 110.0).max(120.0)),
            );
            app.auto.add("crstudio.voice.lexicon.test", r.rect, "Okunuş testi");
            UI.with(|u| u.borrow_mut().lex_test = test.clone());
            if ui.button("Nasıl okunur?").clicked() {
                // the saved dictionary is what the voices use
                let _ = app.session.execute("crstudio.lexicon.set", json!({"text": text}));
                let spoken = app
                    .session
                    .execute("crstudio.speechText", json!({"text": test}))
                    .ok()
                    .and_then(|v| v["spoken"].as_str().map(str::to_string))
                    .unwrap_or_default();
                UI.with(|u| u.borrow_mut().lex_preview = spoken);
            }
        });
        let preview = UI.with(|u| u.borrow().lex_preview.clone());
        if !preview.is_empty() {
            ui.label(RichText::new(format!("→ {preview}")).font(Tokens::ui(13.0)).color(GREEN));
        }
    });
}

fn stage_voice(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let tnow = now(ui);
    let voices = UI.with(|u| {
        let mut u = u.borrow_mut();
        if u.voices.is_none() || tnow - u.voices_at > 5.0 {
            u.voices = app.session.execute("crstudio.voices", json!({})).ok();
            u.voices_at = tnow;
        }
        u.voices.clone().unwrap_or(Value::Null)
    });
    let missing = p.scenes.iter().filter(|s| s.voiceover_path.is_none() || s.voiceover_stale).count();
    let recorded = p.scenes.iter().filter(|s| s.voiceover_engine == "record").count();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Seslendirme").font(Tokens::semibold(18.0)).color(t.text));
        ui.label(RichText::new(format!("{recorded}/{} sahne kendi sesinizle · {missing} eksik", p.scenes.len())).font(Tokens::ui(12.0)).color(t.text_dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(app, ui, "crstudio.voice.assemble", "Kurguya gönder →", !app.session.crstudio.worker_running) {
                run(acts, "crstudio.assembleTimeline", json!({}));
            }
            if button(app, ui, "crstudio.voice.studio", "● Kayıt stüdyosu") {
                UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.studio_open = true;
                    u.studio_clone = false;
                });
            }
        });
    });
    hint(
        ui,
        &t,
        "Önerilen akış: önce taslak AI sesle zamanlamayı oluşturun, sonra her sahneyi teleprompter ile kendi sesinizle kaydedin; kayıt taslağın yerine geçer.",
    );
    ui.add_space(8.0);
    card(ui, &t, false, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Taslak AI ses:").font(Tokens::semibold(12.5)));
            let selected = p.voice_engine.clone();
            let default = voices["default"].as_str().unwrap_or("").to_string();
            let current = if selected.is_empty() { default.clone() } else { selected.clone() };
            if let Some(list) = voices["engines"].as_array() {
                for e in list {
                    let id = e["id"].as_str().unwrap_or("");
                    let ok = e["available"].as_bool().unwrap_or(false);
                    let label = format!(
                        "{}{}",
                        e["label"].as_str().unwrap_or(id),
                        if ok {
                            ""
                        } else if id == "elevenlabs" {
                            " (ayarlı değil)"
                        } else {
                            " (kurulu değil)"
                        }
                    );
                    let resp = ui.add_enabled(
                        ok,
                        egui::Button::new(RichText::new(label).font(Tokens::ui(11.5)).color(if current == id { Color32::WHITE } else { t.text }))
                            .fill(if current == id { RED } else { t.tl_header_bg })
                            .corner_radius(CornerRadius::same(12)),
                    );
                    app.auto.add(&format!("crstudio.voice.engine.{id}"), resp.rect, id);
                    if resp.on_hover_text(format!("{} · {}", e["license"].as_str().unwrap_or(""), e["note"].as_str().unwrap_or(""))).clicked() {
                        run(acts, "crstudio.setVoice", json!({"engine": id}));
                    }
                }
            }
        });
        if p.voice_engine == "chatterbox" || (p.voice_engine.is_empty() && voices["default"] == "chatterbox") {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Klon referansı: {}", p.voice_reference.as_deref().unwrap_or("yok (varsayılan ses)")))
                        .font(Tokens::ui(11.5))
                        .color(t.text_dim),
                );
                if ui.small_button("Kendi ses örneğimi seç…").clicked() {
                    acts.push(Act::PickReference);
                }
                pill(ui, if voices["chatterboxRunning"].as_bool().unwrap_or(false) { "sunucu açık" } else { "ilk kullanımda açılır (~1 dk)" }, GREY);
            });
        }
        ui.horizontal(|ui| {
            if button(app, ui, "crstudio.voice.synthMissing", &format!("Eksik {missing} sahneyi AI ile seslendir")) {
                run(acts, "crstudio.synthesizeVoiceover", json!({"all": true}));
            }
            if button(app, ui, "crstudio.voice.synthAll", "Tümünü yeniden üret") {
                run(acts, "crstudio.synthesizeVoiceover", json!({"all": true, "force": true}));
            }
            let mut tp = UI.with(|u| u.borrow().teleprompter);
            if ui.checkbox(&mut tp, "Teleprompter (büyük yazı)").changed() {
                UI.with(|u| u.borrow_mut().teleprompter = tp);
            }
        });
    });
    ui.add_space(8.0);
    elevenlabs_card(app, ui);
    ui.add_space(8.0);
    lexicon_card(app, ui);
    ui.add_space(8.0);
    let rec_idx = p.recording_scene_index;
    let rec_started = UI.with(|u| u.borrow().record_started);
    let teleprompter = UI.with(|u| u.borrow().teleprompter);
    // the narration player: one scene, or every scene in order
    let playing = UI.with(|u| u.borrow().voice_play);
    if let Some(vp) = playing {
        if now(ui) >= vp.until {
            UI.with(|u| u.borrow_mut().voice_play = None);
            if !(vp.all && play_voice(app, ui, p, vp.scene + 1, true)) {
                app.stop();
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let playing = UI.with(|u| u.borrow().voice_play);
    let with_voice = p.scenes.iter().filter(|s| s.voiceover_path.is_some()).count();
    card(ui, &t, playing.is_some(), |ui| {
        ui.horizontal(|ui| match playing {
            Some(vp) => {
                let b = ui.add(egui::Button::new(RichText::new("■ Durdur").color(Color32::WHITE)).fill(RED));
                app.auto.add("crstudio.voice.playStop", b.rect, "Durdur");
                if b.clicked() {
                    stop_voice(app);
                }
                let title = p.scenes.get(vp.scene).map(|s| s.title.clone()).unwrap_or_default();
                let left = (vp.until - now(ui)).max(0.0);
                ui.label(
                    RichText::new(format!("{} Sahne {}/{} · {title} · {left:.0} sn", if vp.all { "▶ Tümü ·" } else { "▶" }, vp.scene + 1, p.scenes.len()))
                        .font(Tokens::semibold(12.5)),
                );
            }
            None => {
                let b = ui
                    .add_enabled(with_voice > 0, egui::Button::new(RichText::new(format!("▶ Tümünü dinle ({with_voice} sahne)")).font(Tokens::semibold(12.5))));
                app.auto.add("crstudio.voice.playAll", b.rect, "Tümünü dinle");
                if b.clicked() {
                    play_voice(app, ui, p, 0, true);
                }
                hint(ui, &t, "Sahnelerin seslendirmelerini sırayla çalar; tek sahne için kartındaki ▶ düğmesini kullanın.");
            }
        });
    });
    ui.add_space(6.0);
    egui::ScrollArea::vertical().id_salt("crs-voice").auto_shrink([false, false]).show(ui, |ui| {
        for sc in &p.scenes {
            let recording = rec_idx == Some(sc.index);
            let this_plays = playing.is_some_and(|v| v.scene == sc.index);
            card(ui, &t, recording || this_plays, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}. {}", sc.index + 1, sc.title)).font(Tokens::semibold(13.0)).color(t.text));
                    let (txt, col) = match (&sc.voiceover_path, sc.voiceover_engine.as_str(), sc.voiceover_stale) {
                        (_, _, true) => ("metin değişti: yeniden kaydedin".to_string(), AMBER),
                        (None, _, _) => ("ses yok".to_string(), RED),
                        (Some(_), "record", _) => (format!("kendi kaydınız · {:.1} sn", sc.voiceover_duration_sec.unwrap_or(0.0)), GREEN),
                        (Some(_), "file", _) => (format!("dosya · {:.1} sn", sc.voiceover_duration_sec.unwrap_or(0.0)), GREEN),
                        (Some(_), _, _) => (format!("taslak AI · {:.1} sn", sc.voiceover_duration_sec.unwrap_or(0.0)), BLUE),
                    };
                    pill(ui, &txt, col);
                    if sc.voice_checked && !sc.voiceover_stale {
                        if sc.voice_check.is_empty() {
                            pill(ui, "dinlendi ✓", GREEN);
                        } else {
                            let _ = pill_r(ui, &format!("dinleme: {} fark", sc.voice_check.len()), AMBER).on_hover_text(sc.voice_check.join("\n"));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if sc.voiceover_path.is_some() {
                            let b = ui.small_button(if this_plays { "■" } else { "▶ Dinle" });
                            app.auto.add(&format!("crstudio.voice.play.{}", sc.index), b.rect, "Dinle");
                            if b.clicked() {
                                if this_plays {
                                    stop_voice(app);
                                } else {
                                    play_voice(app, ui, p, sc.index, false);
                                }
                            }
                        }
                        if ui.small_button("Dosya bağla…").clicked() {
                            acts.push(Act::PickAudio(sc.index));
                        }
                        if ui.small_button("AI taslak").clicked() {
                            run(acts, "crstudio.synthesizeVoiceover", json!({"sceneIndex": sc.index}));
                        }
                        if recording {
                            let secs = rec_started.map(|s| now(ui) - s).unwrap_or(0.0);
                            let b = ui.add(egui::Button::new(RichText::new(format!("■ Bitir ({secs:.0} sn)")).color(Color32::WHITE)).fill(RED));
                            app.auto.add("crstudio.voice.recStop", b.rect, "Kaydı bitir");
                            if b.clicked() {
                                run(acts, "crstudio.recordVoiceoverStop", json!({}));
                                UI.with(|u| u.borrow_mut().record_started = None);
                            }
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                        } else if rec_idx.is_none() {
                            let b = ui.add(egui::Button::new(RichText::new("● Kaydet").color(Color32::WHITE)).fill(alpha(RED, 220)));
                            app.auto.add(&format!("crstudio.voice.rec.{}", sc.index), b.rect, "Kaydet");
                            if b.clicked() {
                                run(acts, "crstudio.recordVoiceoverStart", json!({"sceneIndex": sc.index}));
                                let tn = now(ui);
                                UI.with(|u| u.borrow_mut().record_started = Some(tn));
                            }
                        }
                    });
                });
                let size = if teleprompter && recording {
                    30.0
                } else if teleprompter {
                    18.0
                } else {
                    13.5
                };
                ui.label(RichText::new(&sc.narration_text).font(Tokens::ui(size)).color(t.text));
            });
            ui.add_space(6.0);
        }
    });
}

// ------------------------------------------------------------------------------------------------
// 5. Kurgu
// ------------------------------------------------------------------------------------------------

fn stage_studio(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.label(RichText::new("Kurgu").font(Tokens::semibold(18.0)).color(t.text));
    ui.add_space(8.0);
    card(ui, &t, true, |ui| {
        if p.assembled_sequence.is_some() {
            ui.label(
                RichText::new(
                    "Kurgu hazır: görüntü, belgesel grafikleri, YouTuber eklentileri, kanal kimliği, altyazı, seslendirme, efektler ve müzik timeline'da.",
                )
                .font(Tokens::ui(13.0))
                .color(t.text),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if primary_button(app, ui, "crstudio.studio.open", "Kurgu masasını aç →", true) {
                    acts.push(Act::Mode(Mode::Edit));
                }
                if button(app, ui, "crstudio.studio.assistant", "Asistanla düzenle") {
                    acts.push(Act::Assistant);
                }
                if button(app, ui, "crstudio.studio.rebuild", "Kurguyu yeniden oluştur") {
                    run(acts, "crstudio.assembleTimeline", json!({}));
                }
                if button(
                    app,
                    ui,
                    "crstudio.studio.shorts",
                    if p.shorts_sequence.is_some() { "Shorts kesitini yenile" } else { "Shorts kesiti oluştur (9:16)" },
                ) {
                    run(acts, "crstudio.createShortsCut", json!({}));
                }
            });
            ui.add_space(8.0);
            hint(
                ui,
                &t,
                "Asistan: In/Out ile bir aralık işaretleyin ya da oynatma kafasını bir kareye getirin, ne istediğinizi yazın — reklam kuşağı, ses efekti, bilgi kutusu, işaret… Claude adımları önerir, siz onaylarsınız.",
            );
        } else {
            ui.label("Henüz kurgu oluşturulmadı.");
            if primary_button(app, ui, "crstudio.studio.build", "Kurguyu oluştur", true) {
                run(acts, "crstudio.assembleTimeline", json!({}));
            }
        }
    });
    ui.add_space(10.0);
    card(ui, &t, false, |ui| {
        section_title(ui, &t, "Kurgu ayarları");
        let pl = &mut app.session.crstudio.pipeline;
        ui.checkbox(&mut pl.broll_pacing_enabled, "Uzun sahnelerde B-roll alt kesmesi");
        ui.checkbox(&mut pl.film_grain_enabled, "Film greni");
        ui.checkbox(&mut pl.j_l_cut_enabled, "J-cut ses geçişleri");
        ui.checkbox(&mut pl.youtuber_overlays, "YouTuber eklentileri");
        ui.checkbox(&mut pl.brand_kit_enabled, "Abone ol + 'Bunu da izle' bitiş ekranı");
        ui.checkbox(&mut pl.voice_polish_enabled, "Seslendirme cilası (sessizlik kırpma + seviye)");
        ui.horizontal(|ui| {
            ui.label("Altyazı");
            for (id, l) in [("cinematic_doc", "Sinematik"), ("viral_word_highlight", "Vurgulu (viral)")] {
                if ui.selectable_label(pl.caption_style_mode == id, l).clicked() {
                    pl.caption_style_mode = id.into();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Müzik");
            for (id, l) in [
                ("historical_suspense", "Tarihî gerilim"),
                ("mystery_dark_drone", "Karanlık gizem"),
                ("investigative_pulse", "Araştırmacı nabız"),
                ("cosmic_synth", "Kozmik"),
                ("none", "Yok"),
            ] {
                if ui.selectable_label(pl.bgm_style == id, l).clicked() {
                    pl.bgm_style = id.into();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Müzik seviyesi");
            ui.add(egui::Slider::new(&mut pl.bgm_volume_db, -36.0..=-6.0).suffix(" dB"));
        });
    });
}

// ------------------------------------------------------------------------------------------------
// 6. Yayın
// ------------------------------------------------------------------------------------------------

fn stage_publish(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: &CrStudioPipeline, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Yayın").font(Tokens::semibold(18.0)).color(t.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button(app, ui, "crstudio.publish.review", "Yayın Yönetmeni değerlendirsin (Claude)") {
                run(acts, "crstudio.review", json!({"ai": true}));
            }
            if button(app, ui, "crstudio.publish.package", "Paketi yenile (kapak, bölümler, altyazı)") {
                run(acts, "crstudio.packageYoutube", json!({}));
            }
        });
    });
    ui.add_space(6.0);
    egui::ScrollArea::vertical().id_salt("crs-publish").auto_shrink([false, false]).show(ui, |ui| {
        // Chief Editor
        card(ui, &t, true, |ui| match &p.director {
            None => {
                ui.label("Henüz değerlendirme yok.");
                if button(app, ui, "crstudio.publish.reviewRules", "Hızlı değerlendirme") {
                    run(acts, "crstudio.review", json!({"ai": false}));
                }
            }
            Some(d) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:.0}", d.score)).font(Tokens::semibold(44.0)).color(score_color(d.score)));
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Yayınlanabilirlik").font(Tokens::ui(12.0)).color(t.text_dim));
                        ui.label(RichText::new(&d.verdict).font(Tokens::semibold(15.0)).color(t.text));
                        if let Some(a) = d.ai_score {
                            ui.label(RichText::new(format!("İçerik (Claude): {a:.0}/100")).font(Tokens::ui(11.5)).color(t.text_dim));
                        }
                    });
                });
                for c in &d.categories {
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(200.0, 16.0), egui::Label::new(RichText::new(&c.label).font(Tokens::ui(11.5))));
                        ui.add(
                            egui::ProgressBar::new((c.score / 100.0) as f32).desired_width(240.0).fill(score_color(c.score)).text(format!("{:.0}", c.score)),
                        );
                    });
                }
                for n in &d.ai_notes {
                    ui.label(RichText::new(format!("• {n}")).font(Tokens::ui(12.0)).color(t.text));
                }
                ui.add_space(6.0);
                for (k, is) in d.issues.iter().enumerate().take(40) {
                    let col = match is.severity.as_str() {
                        "kritik" => RED,
                        "önemli" => AMBER,
                        _ => BLUE,
                    };
                    ui.horizontal_wrapped(|ui| {
                        pill(ui, &is.severity, col);
                        ui.label(RichText::new(&is.message).font(Tokens::semibold(12.0)).color(t.text));
                        ui.label(RichText::new(&is.fix).font(Tokens::ui(11.5)).color(t.text_dim));
                        let resp = ui.small_button(format!("{} aşamasına git →", is.stage.label()));
                        app.auto.add(&format!("crstudio.issue.{k}"), resp.rect, "Git");
                        if resp.clicked() {
                            acts.push(Act::Stage(is.stage));
                            if let Some(sc) = is.scene {
                                app.session.crstudio.pipeline.active_scene_index = sc;
                            }
                        }
                    });
                }
            }
        });
        ui.add_space(10.0);
        // Package
        card(ui, &t, false, |ui| {
            section_title(ui, &t, "Paket");
            ui.horizontal_top(|ui| {
                paint_thumb(ui, p.thumbnail_path.as_deref(), vec2(320.0, 180.0), "Kapak yok");
                ui.vertical(|ui| {
                    ui.label(RichText::new("Başlık seçenekleri").font(Tokens::semibold(12.5)));
                    for (k, alt) in p.social_meta.alternative_titles.iter().enumerate() {
                        let resp = ui.selectable_label(p.title == *alt, alt);
                        app.auto.add(&format!("crstudio.publish.title.{k}"), resp.rect, alt);
                        if resp.clicked() {
                            app.session.crstudio.pipeline.title = alt.clone();
                        }
                    }
                    ui.add_space(6.0);
                    let mut tt = p.social_meta.thumbnail_text.clone();
                    ui.horizontal(|ui| {
                        ui.label("Kapak yazısı");
                        if ui.add(egui::TextEdit::singleline(&mut tt).desired_width(200.0)).lost_focus() && tt != p.social_meta.thumbnail_text {
                            run(acts, "crstudio.packageYoutube", json!({"thumbnailText": tt}));
                        }
                    });
                    if !p.social_meta.tags.is_empty() {
                        ui.label(RichText::new(format!("Etiketler: {}", p.social_meta.tags.join(", "))).font(Tokens::ui(11.5)).color(t.text_dim));
                    }
                    if !p.social_meta.pinned_comment.is_empty() {
                        ui.label(RichText::new(format!("Sabit yorum: {}", p.social_meta.pinned_comment)).font(Tokens::ui(11.5)).color(t.text_dim));
                    }
                });
            });
            ui.add_space(6.0);
            let desc = filmcraft_engine::crstudio::package::full_description(p);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Açıklama").font(Tokens::semibold(12.5)));
                if ui.small_button("Kopyala").clicked() {
                    ui.ctx().copy_text(desc.clone());
                    acts.push(Act::Status("Açıklama panoya kopyalandı".into()));
                }
                if !p.project_dir.is_empty() && ui.small_button("Yayın klasörünü aç").clicked() {
                    acts.push(Act::Reveal(format!("{}/yayin", p.project_dir)));
                }
            });
            let mut d2 = desc.clone();
            ui.add(egui::TextEdit::multiline(&mut d2).desired_rows(8).desired_width(ui.available_width()).interactive(false));
        });
        ui.add_space(10.0);
        // Export + YouTube
        card(ui, &t, false, |ui| {
            section_title(ui, &t, "Dışa aktar ve YouTube");
            ui.horizontal(|ui| {
                if button(app, ui, "crstudio.publish.export", "Dışa aktarma ekranını aç") {
                    acts.push(Act::Mode(Mode::Export));
                }
            });
            ui.add_space(6.0);
            let tnow = now(ui);
            let yt = UI.with(|u| {
                let mut u = u.borrow_mut();
                if u.yt_status.is_none() || tnow - u.yt_status_at > 2.0 {
                    u.yt_status = app.session.execute("crstudio.youtube.status", json!({})).ok();
                    u.yt_status_at = tnow;
                }
                u.yt_status.clone().unwrap_or(Value::Null)
            });
            if !yt["configured"].as_bool().unwrap_or(false) {
                hint(ui, &t, "Kanalınızı açınca Google Cloud'da bir OAuth istemcisi (Masaüstü uygulaması) oluşturup kimliğini buraya girin.");
                UI.with(|u| {
                    let mut u = u.borrow_mut();
                    ui.horizontal(|ui| {
                        ui.label("İstemci kimliği");
                        ui.add(egui::TextEdit::singleline(&mut u.yt_client_id).desired_width(300.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Gizli anahtar");
                        ui.add(egui::TextEdit::singleline(&mut u.yt_client_secret).password(true).desired_width(300.0));
                    });
                });
                if ui.button("Kaydet").clicked() {
                    let (id, sec) = UI.with(|u| {
                        let u = u.borrow();
                        (u.yt_client_id.clone(), u.yt_client_secret.clone())
                    });
                    run(acts, "crstudio.youtube.configure", json!({"clientId": id, "clientSecret": sec}));
                    UI.with(|u| u.borrow_mut().yt_status = None);
                }
            } else if !yt["connected"].as_bool().unwrap_or(false) {
                if button(app, ui, "crstudio.publish.ytConnect", "YouTube hesabını bağla (tarayıcıda açılır)") {
                    run(acts, "crstudio.youtube.connect", json!({}));
                }
            } else {
                ui.label(RichText::new(format!("Bağlı kanal: {}", yt["channelTitle"].as_str().unwrap_or(""))).color(GREEN));
                let path = UI.with(|u| u.borrow().yt_video_path.clone());
                ui.horizontal(|ui| {
                    ui.label(if path.is_empty() { "Video: seçilmedi".to_string() } else { format!("Video: {path}") });
                    if ui.small_button("Seç…").clicked() {
                        acts.push(Act::PickVideoForUpload);
                    }
                });
                let blocked = p.director.as_ref().is_some_and(|d| d.blocking);
                if blocked {
                    ui.label(RichText::new("Yayın Yönetmeni kritik eksik buldu; yükleme kilitli.").color(RED));
                }
                if primary_button(app, ui, "crstudio.publish.upload", "Özel olarak yükle", !path.is_empty() && !blocked) {
                    run(acts, "crstudio.youtube.upload", json!({"videoPath": path, "privacy": "private"}));
                }
                if let Some(m) = yt["upload"]["message"].as_str() {
                    ui.label(RichText::new(m).color(t.text_dim));
                }
            }
        });
    });
}
