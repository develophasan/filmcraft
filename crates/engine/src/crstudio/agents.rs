//! The pre-production agents (1–6 and 8–9) and the background job that runs them in order.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::claude::{self, ClaudeEvent, ClaudeRequest};
use super::{
    AgentStatus, ChartSpec, CrStudioPipeline, Fact, MapSpec, PipelineStage, Place, ResearchPack, SceneSpec, SocialMetadata, SourceRef, director, turkish,
    with_shared,
};

/// Words per scene the Scriptwriter aims at.
fn words_per_scene(p: &CrStudioPipeline) -> f64 {
    if p.format.is_vertical() { 13.0 } else { 20.0 }
}

/// Scene count for the target duration.
pub fn scene_count(p: &CrStudioPipeline) -> usize {
    let words = p.target_duration_sec * turkish::WORDS_PER_SECOND * 0.95;
    ((words / words_per_scene(p)).round() as usize).clamp(4, super::MAX_SCENES)
}

fn tone_guidance(p: &CrStudioPipeline) -> &'static str {
    match p.tone {
        super::DocTone::MythLegend => {
            "Bir hikâye anlatıcısı gibi, sıcak ve merak uyandıran bir dille anlat. Belgelenmiş bilgiyi ('kayıtlara göre', 'arkeolojik bulgular') rivayetten ('rivayete göre', 'anlatılır ki') açıkça ayır. Farklı kültürlerdeki versiyonları karşılaştır."
        }
        super::DocTone::HistoryMystery => "Sinematik, gerilimli, kronolojik ilerleyen bir anlatım; her bölüm bir soruyla bitsin.",
        super::DocTone::InvestigativeVox => "Araştırmacı gazeteci dili; harita, belge ve verilerle kanıtla, iddiaları kaynaklarla bağla.",
        super::DocTone::ScienceTech => "Açık, öğretici, analojilerle anlatan bir dil; sayıları ve grafikleri kullan.",
        super::DocTone::ViralFast => "Çok kısa cümleler, her 3-4 saniyede yeni bir bilgi, şaşırtıcı açılış.",
    }
}

fn log_event(shared: &Arc<Mutex<CrStudioPipeline>>, agent: u32, name: &str, ev: &ClaudeEvent) {
    let line = match ev {
        ClaudeEvent::Text(t) => format!("💬 {}", super::truncate_line(t, 140)),
        ClaudeEvent::Tool { name, detail } => match name.as_str() {
            "WebSearch" => format!("🔎 {detail}"),
            "WebFetch" => format!("🌐 {detail}"),
            "Read" => format!("🖼 {detail}"),
            _ => format!("🛠 {name}: {detail}"),
        },
        ClaudeEvent::Done { seconds, is_error, .. } => {
            if *is_error {
                "✗ Claude hata ile bitti".to_string()
            } else {
                format!("✓ Bitti ({seconds:.0} sn)")
            }
        }
    };
    with_shared(shared, |p| {
        p.agent_activity(agent, line.clone());
        if matches!(ev, ClaudeEvent::Tool { .. }) {
            p.push_log(name, "info", line);
        }
    });
}

fn run_agent(shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>, agent: u32, name: &str, req: &ClaudeRequest) -> Result<Value, String> {
    let mut cb = |ev: &ClaudeEvent| log_event(shared, agent, name, ev);
    let text = claude::run(req, cancel, &mut cb)?;
    claude::extract_json(&text).ok_or_else(|| format!("Claude yanıtı JSON değil: {}", super::truncate_line(&text, 200)))
}

// ------------------------------------------------------------------------------------------------
// Agent 1: Researcher
// ------------------------------------------------------------------------------------------------

pub fn research_prompt(p: &CrStudioPipeline) -> String {
    format!(
        r#"Sen CrStudio'nun Araştırmacı ajanısın. Türkçe bir YouTube videosu için kaynaklı araştırma yapacaksın.
Konu: "{title}"
Ek notlar: {extra}
Ton: {tone}

Görev:
1. WebSearch ve WebFetch ile konuyu araştır. Güvenilir kaynaklara öncelik ver (akademik yayınlar, müzeler, ansiklopediler, resmi kurumlar, Vikipedi yalnız başlangıç noktası).
2. En az 6, en fazla 15 bilgi topla. Her bilginin türünü belirt: "belge" (kayıt/arkeoloji/kaynakla doğrulanabilir) ya da "rivayet" (efsane, halk anlatısı, dini/kültürel anlatı).
3. Hikâyenin geçtiği yerleri gerçek koordinatlarıyla çıkar (farklı rivayetlerde farklı yerler varsa hepsini).
4. Videoyu ilginç kılacak 3 farklı anlatı açısı öner.

Yalnız şu JSON'u döndür (başka metin yazma):
{{"summary":"3-4 cümlelik özet","facts":[{{"text":"bilgi","kind":"belge|rivayet","source":0}}],"sources":[{{"title":"sayfa başlığı","url":"https://...","publisher":"yayıncı"}}],"places":[{{"name":"Efes, İzmir","lat":37.94,"lon":27.34,"note":"neden önemli"}}],"angles":["açı 1","açı 2","açı 3"]}}
"source" alanı sources dizisindeki sıra numarasıdır. Yalnız gerçekten açtığın sayfaları kaynak olarak yaz."#,
        title = p.title,
        extra = if p.extra_instructions.trim().is_empty() { "Yok" } else { p.extra_instructions.trim() },
        tone = p.tone.label(),
    )
}

pub fn apply_research(p: &mut CrStudioPipeline, v: &Value) -> bool {
    let sources: Vec<SourceRef> = v
        .get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().take(40).filter_map(|x| serde_json::from_value(x.clone()).ok()).collect())
        .unwrap_or_default();
    let n = sources.len();
    let facts: Vec<Fact> = v
        .get("facts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(40)
                .filter_map(|x| {
                    let text = x.get("text").and_then(Value::as_str)?.trim().to_string();
                    (!text.is_empty()).then(|| Fact {
                        text,
                        kind: x.get("kind").and_then(Value::as_str).unwrap_or("belge").to_string(),
                        source: x.get("source").and_then(Value::as_u64).map(|i| i as usize).filter(|i| *i < n),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let places: Vec<Place> = v
        .get("places")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(12)
                .filter_map(|x| {
                    let lat = x.get("lat").and_then(Value::as_f64).filter(|l| l.is_finite() && l.abs() <= 90.0)?;
                    let lon = x.get("lon").and_then(Value::as_f64).filter(|l| l.is_finite() && l.abs() <= 180.0)?;
                    Some(Place {
                        name: x.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        lat,
                        lon,
                        note: x.get("note").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if facts.is_empty() {
        return false;
    }
    let summary = v.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
    p.research_summary = summary.clone();
    p.research = ResearchPack { summary, facts, sources, places, origin: "claude".into() };
    if let Some(angles) = v.get("angles").and_then(Value::as_array) {
        let a: Vec<String> = angles.iter().filter_map(Value::as_str).map(str::to_string).collect();
        if !a.is_empty() {
            p.research.summary.push_str("\n\nAnlatı açıları: ");
            p.research.summary.push_str(&a.join(" · "));
        }
    }
    true
}

// ------------------------------------------------------------------------------------------------
// Agents 2 + 3: Scriptwriter and Director (one structured answer)
// ------------------------------------------------------------------------------------------------

pub fn script_prompt(p: &CrStudioPipeline) -> String {
    let count = scene_count(p);
    let wps = words_per_scene(p);
    let research = json!({
        "summary": p.research.summary,
        "facts": p.research.facts,
        "sources": p.research.sources.iter().enumerate().map(|(i, s)| json!({"i": i, "title": s.title})).collect::<Vec<_>>(),
        "places": p.research.places,
    });
    format!(
        r#"Sen CrStudio'nun Senarist ve Yönetmen ajanlarısın. Aşağıdaki araştırmayla Türkçe bir YouTube videosunun senaryosunu ve storyboard'unu hazırla.
Başlık/konu: "{title}"
Format: {fmt} ({aspect}), hedef süre {dur:.0} sn → tam {count} sahne, her sahne yaklaşık {wps:.0} kelime (seslendirme hızı ~2,3 kelime/sn).
Ton: {tone}. {guide}
Kanal: {channel}
Ek notlar: {extra}
Araştırma (JSON): {research}

Kurallar:
- İlk sahne güçlü bir kanca: ilk 5 saniyede merak uyandıran soru ya da beklenmedik bilgi. Kanca sahnesi kısa olsun.
- Sahneleri bölümlere ayır (chapter); uzun videoda 4-8 bölüm. Her bölümün ilk sahnesi "chapter_card" olabilir.
- Yalnız araştırmadaki bilgileri kullan; uydurma tarih, isim, alıntı, sayı yazma. Sahnenin bilgisi bir kaynağa dayanıyorsa "sources" ile belirt, rivayetse "factKind":"rivayet".
- "visualQuery" İngilizce ve Wikimedia Commons'ta bulunabilecek somut bir şey olsun (ör. "Ephesus cave of the Seven Sleepers", "Ottoman miniature cave", "Byzantine mosaic sleeping youths").
- visualType: "stock_photo" (çoğunluk), "animated_map" (yer değişimi), "chapter_card", "evidence_highlight" (yalnız gerçek bir alıntı ya da belge cümlesi varsa), "infographic_chart" (yalnız gerçek sayılar varsa; "chart" alanını doldur).
- cameraMotion: zoom_in | zoom_out | pan_right | pan_left | dramatic_push. sfxCue: whoosh | low_boom | riser | map_ping | archive_shutter | none.
- overlays: YouTuber eklentileri, gerektiği yerde: "info_box" (ek bilgi), "warning_box" (yanlış bilinen bir şey), "speech_bubble" (bir karakterin sözü), "comment_prompt" (izleyiciye soru; videonun ortasında ve sonunda), "like_reminder" (videonun ~%40'ında bir kez), "source_credit" (kaynak gösterilecek sahne), "timeline_tag" (tarih geçen sahne), "countdown" (sıralama). Kutu metni "infoText" alanına (en fazla 90 karakter).
- Haritada yer: "mapPlace": araştırmadaki places dizisinin sıra numarası.
- Son sahne izleyiciye soru sorsun ve abone olmaya davet etsin (abartısız).
- Anlatımda web sitesi adı geçmesin ("Wikipedia'ya göre" gibi); kaynağı yayın, kurum ya da araştırmacı adıyla an ("Kültür Bakanlığı kayıtlarına göre", "Mavi Rehber'e göre").
- Her sahne en az iki cümle olsun; tek cümlelik çok kısa sahneleri birleştir.

Yalnız şu JSON'u döndür:
{{"titleOptions":["...","...","..."],"hookLine":"...","description":"YouTube açıklaması: ilk 2 satır merak uyandırsın, sonra 2-3 paragraf özet","tags":["10-15 etiket"],"thumbnailText":"KAPAKTA 2-4 KELİME","pinnedComment":"sabitlenecek yorum sorusu",
"scenes":[{{"chapter":"Bölüm adı","title":"Sahne başlığı","narrationText":"seslendirme metni","factKind":"belge|rivayet|","sources":[0],"visualType":"stock_photo","visualQuery":"...","cameraMotion":"zoom_in","overlayTitle":"ekranda kısa başlık","overlaySubtitle":"alt bilgi","sfxCue":"whoosh","overlays":[],"infoText":"","mapPlace":null,"chart":null}}]}}"#,
        title = p.title,
        fmt = p.format.label(),
        aspect = p.format.aspect_label(),
        dur = p.target_duration_sec,
        tone = p.tone.label(),
        guide = tone_guidance(p),
        channel = p.channel_name,
        extra = if p.extra_instructions.trim().is_empty() { "Yok" } else { p.extra_instructions.trim() },
        research = research,
    )
}

const VISUAL_TYPES: [&str; 5] = ["stock_photo", "animated_map", "evidence_highlight", "infographic_chart", "chapter_card"];
const CAMERA: [&str; 5] = ["zoom_in", "zoom_out", "pan_right", "pan_left", "dramatic_push"];
const SFX: [&str; 6] = ["whoosh", "low_boom", "riser", "map_ping", "archive_shutter", "none"];
pub const OVERLAYS: [&str; 9] =
    ["info_box", "warning_box", "speech_bubble", "comment_prompt", "like_reminder", "source_credit", "timeline_tag", "countdown", "shock_badge"];

fn pick<'a>(v: Option<&'a str>, allowed: &[&'a str], default: &'a str) -> String {
    v.filter(|x| allowed.contains(x)).unwrap_or(default).to_string()
}

pub fn default_transition_and_effect(idx: usize, visual_type: &str) -> (&'static str, &'static str) {
    if idx == 0 {
        return ("dip_to_black", "dark_mystery_vignette");
    }
    match visual_type {
        "animated_map" => ("cross_zoom", "cold_tactical_map"),
        "evidence_highlight" => ("light_sweep", "vintage_archive_film"),
        "infographic_chart" => ("luma_fade", "clean_contrast"),
        "chapter_card" => ("dip_to_black", "dark_mystery_vignette"),
        _ => ("film_dissolve", "cinematic_teal_orange"),
    }
}

fn overlay_template_for(visual_type: &str, vertical: bool, idx: usize) -> &'static str {
    match visual_type {
        "animated_map" => "builtin:doc-map-pin",
        "evidence_highlight" => "builtin:doc-highlight-quote",
        "chapter_card" => "builtin:doc-chapter-card",
        _ if idx == 0 && vertical => "builtin:shorts-dynamic-hook",
        _ => "builtin:lower-third-rule",
    }
}

pub fn apply_script(p: &mut CrStudioPipeline, v: &Value) -> bool {
    let Some(arr) = v.get("scenes").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return false;
    };
    let nsrc = p.research.sources.len();
    let places = p.research.places.clone();
    let vertical = p.format.is_vertical();
    let mut scenes = Vec::new();
    for item in arr.iter().take(super::MAX_SCENES) {
        let narration = item.get("narrationText").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if narration.is_empty() {
            continue;
        }
        let idx = scenes.len();
        let s = |k: &str| item.get(k).and_then(Value::as_str).map(|x| x.trim().to_string()).unwrap_or_default();
        let vtype = pick(item.get("visualType").and_then(Value::as_str), &VISUAL_TYPES, "stock_photo");
        let chart: Option<ChartSpec> = item
            .get("chart")
            .and_then(|c| serde_json::from_value::<ChartSpec>(c.clone()).ok())
            .filter(|c| !c.values.is_empty() && c.values.len() == c.labels.len() && c.values.iter().all(|x| x.is_finite()));
        let vtype = if vtype == "infographic_chart" && chart.is_none() { "stock_photo".to_string() } else { vtype };
        let map_spec = item
            .get("mapPlace")
            .and_then(Value::as_u64)
            .and_then(|i| places.get(i as usize))
            .map(|pl| MapSpec {
                location_name: turkish::upper(&pl.name),
                lat: pl.lat,
                lon: pl.lon,
                subtitle: pl.note.clone(),
                extra_pins: places.iter().filter(|o| o.name != pl.name).take(4).map(|o| (o.name.clone(), o.lat, o.lon)).collect(),
                ..Default::default()
            })
            .or_else(|| {
                (vtype == "animated_map").then(|| {
                    places.first().map_or_else(MapSpec::default, |pl| MapSpec {
                        location_name: turkish::upper(&pl.name),
                        lat: pl.lat,
                        lon: pl.lon,
                        subtitle: pl.note.clone(),
                        ..Default::default()
                    })
                })
            });
        let vtype = if vtype == "animated_map" && map_spec.is_none() { "stock_photo".to_string() } else { vtype };
        let (tr, eff) = default_transition_and_effect(idx, &vtype);
        let overlays: Vec<String> = item
            .get("overlays")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).filter(|o| OVERLAYS.contains(o)).map(str::to_string).take(3).collect())
            .unwrap_or_default();
        let mut sc = SceneSpec {
            id: idx as u64 + 1,
            index: idx,
            title: {
                let t = s("title");
                if t.is_empty() { format!("Sahne {}", idx + 1) } else { t }
            },
            chapter: s("chapter"),
            narration_text: narration.clone(),
            visual_type: vtype.clone(),
            visual_query: {
                let q = s("visualQuery");
                if q.is_empty() { turkish::ascii_fold(&p.title) } else { q }
            },
            camera_motion: pick(item.get("cameraMotion").and_then(Value::as_str), &CAMERA, "zoom_in"),
            map_spec,
            chart,
            overlay_template: overlay_template_for(&vtype, vertical, idx).into(),
            overlay_title: s("overlayTitle"),
            overlay_subtitle: s("overlaySubtitle"),
            sfx_cue: pick(item.get("sfxCue").and_then(Value::as_str), &SFX, "whoosh"),
            video_transition: tr.into(),
            visual_effect: eff.into(),
            film_texture: if vtype == "evidence_highlight" { "vhs_archive".into() } else { "35mm_grain".into() },
            overlays,
            info_text: super::truncate_line(&s("infoText"), 110),
            text_origin: "claude".into(),
            fact_kind: {
                let k = s("factKind");
                if k == "belge" || k == "rivayet" { k } else { String::new() }
            },
            sources: item
                .get("sources")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_u64).map(|x| x as usize).filter(|x| *x < nsrc).collect())
                .unwrap_or_default(),
            highlight_phrase: super::truncate_line(&narration, 60),
            ..Default::default()
        };
        sc.duration_seconds = (sc.estimated_seconds() * 10.0).round() / 10.0;
        sc.broll_subcuts = if sc.duration_seconds >= 7.0 && idx > 0 && sc.visual_type == "stock_photo" { 2 } else { 1 };
        scenes.push(sc);
    }
    if scenes.len() < 2 {
        return false;
    }
    let strs =
        |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
    let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    p.social_meta = SocialMetadata {
        alternative_titles: strs("titleOptions"),
        description: get("description"),
        tags: strs("tags").into_iter().take(20).collect(),
        thumbnail_text: get("thumbnailText"),
        hook_line: get("hookLine"),
        pinned_comment: get("pinnedComment"),
    };
    p.raw_script = scenes.iter().map(|s| s.narration_text.as_str()).collect::<Vec<_>>().join("\n\n");
    p.scenes = scenes;
    p.active_scene_index = 0;
    true
}

/// When Claude cannot run: a clearly marked skeleton the user fills in (never presented as a
/// finished script; every scene is `text_origin = "template"` and blocks publishing).
pub fn skeleton_storyboard(p: &mut CrStudioPipeline) {
    let count = scene_count(p).min(12);
    let title = if p.title.trim().is_empty() { "Yeni video".to_string() } else { p.title.trim().to_string() };
    let beats = [
        ("Kanca", "(Taslak) İzleyiciyi ilk beş saniyede yakalayacak bir soru yazın: ", "chapter_card"),
        ("Giriş", "(Taslak) Konuyu ve neden önemli olduğunu anlatın: ", "stock_photo"),
        ("Yer", "(Taslak) Hikâyenin geçtiği yeri ve zamanı anlatın: ", "stock_photo"),
        ("Olay", "(Taslak) Hikâyenin ana olayını anlatın: ", "stock_photo"),
        ("Kanıt", "(Taslak) Kaynaklarda ne yazdığını aktarın: ", "stock_photo"),
        ("Yorumlar", "(Taslak) Farklı anlatıları ve yorumları karşılaştırın: ", "stock_photo"),
        ("Bugün", "(Taslak) Bugüne yansımasını anlatın: ", "stock_photo"),
        ("Kapanış", "(Taslak) İzleyiciye bir soru sorup abone olmaya davet edin: ", "stock_photo"),
    ];
    let mut scenes = Vec::new();
    for i in 0..count {
        let (chap, text, vt) = beats.get(i * beats.len() / count.max(1)).copied().unwrap_or(("Bölüm", "(Taslak) ", "stock_photo"));
        let (tr, eff) = default_transition_and_effect(i, vt);
        scenes.push(SceneSpec {
            id: i as u64 + 1,
            index: i,
            title: format!("{chap} {}", i + 1),
            chapter: chap.into(),
            narration_text: format!("{text}{title}."),
            duration_seconds: (p.target_duration_sec / count as f64).clamp(4.0, 20.0),
            visual_type: vt.into(),
            visual_query: turkish::ascii_fold(&title),
            overlay_template: overlay_template_for(vt, p.format.is_vertical(), i).into(),
            overlay_title: turkish::upper(&title),
            video_transition: tr.into(),
            visual_effect: eff.into(),
            text_origin: "template".into(),
            ..Default::default()
        });
    }
    p.scenes = scenes;
    p.active_scene_index = 0;
    if p.social_meta.thumbnail_text.is_empty() {
        p.social_meta.thumbnail_text = turkish::upper(&title.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
    }
}

// ------------------------------------------------------------------------------------------------
// The pre-production job
// ------------------------------------------------------------------------------------------------

fn stopped(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// Agents 1–6, 8 and 9 in order. Runs on the job thread; all slow work happens outside the lock.
pub fn run_preproduction(shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>) {
    let snapshot = with_shared(shared, |p| {
        p.stage = PipelineStage::RunningAgents;
        for a in &mut p.agents {
            a.status = AgentStatus::Idle;
            a.summary = "Sırada".into();
            a.activity.clear();
        }
        p.claude_cli_available = claude::find_claude().is_some();
        p.push_log("CrStudio", "info", format!("\"{}\" için ajanlar başladı", p.title));
        p.clone()
    });
    let timeout = snapshot.claude_timeout_sec;
    let cwd = (!snapshot.project_dir.is_empty()).then(|| std::path::PathBuf::from(&snapshot.project_dir));

    // 1. Researcher
    with_shared(shared, |p| {
        p.set_agent_status(1, AgentStatus::Running, "Web'de kaynak arıyor…");
        p.job.label = "Araştırma".into();
        p.job.progress = 0.05;
    });
    let mut req = ClaudeRequest::new(research_prompt(&snapshot), timeout);
    req.allowed_tools = vec!["WebSearch".into(), "WebFetch".into()];
    req.max_turns = Some(40);
    req.cwd = cwd.clone();
    let research = run_agent(shared, cancel, 1, "Araştırmacı", &req);
    if stopped(cancel) {
        return;
    }
    let research_ok = with_shared(shared, |p| match &research {
        Ok(v) if apply_research(p, v) => {
            let (nf, ns) = (p.research.facts.len(), p.research.sources.len());
            p.set_agent_status(1, AgentStatus::Done, format!("{nf} bilgi, {ns} kaynak, {} yer", p.research.places.len()));
            p.push_log("Araştırmacı", "ok", format!("{nf} bilgi ve {ns} kaynak bulundu"));
            true
        }
        Ok(_) => {
            p.set_agent_status(1, AgentStatus::Error, "Yanıtta kullanılabilir bilgi yok");
            p.push_log("Araştırmacı", "error", "Claude yanıtında bilgi yok");
            false
        }
        Err(e) => {
            p.set_agent_status(1, AgentStatus::Error, e.clone());
            p.push_log("Araştırmacı", "error", e.clone());
            false
        }
    });

    // 2 + 3. Scriptwriter and Director
    let snapshot = with_shared(shared, |p| {
        p.set_agent_status(2, AgentStatus::Running, "Senaryoyu yazıyor…");
        p.set_agent_status(3, AgentStatus::Running, "Storyboard planlıyor…");
        p.job.label = "Senaryo ve storyboard".into();
        p.job.progress = 0.25;
        p.clone()
    });
    let script = if research_ok || snapshot.claude_cli_available {
        let mut req = ClaudeRequest::new(script_prompt(&snapshot), timeout);
        req.max_turns = Some(4);
        req.cwd = cwd.clone();
        run_agent(shared, cancel, 2, "Senarist", &req)
    } else {
        Err("Claude kullanılamıyor".into())
    };
    if stopped(cancel) {
        return;
    }
    with_shared(shared, |p| match &script {
        Ok(v) if apply_script(p, v) => {
            let n = p.scenes.len();
            let words: usize = p.scenes.iter().map(|s| s.narration_text.split_whitespace().count()).sum();
            p.set_agent_status(2, AgentStatus::Done, format!("{n} sahne, {words} kelime (~{:.0} sn)", p.total_seconds()));
            p.set_agent_status(3, AgentStatus::Done, "Storyboard hazır");
            p.push_log("Senarist", "ok", format!("{n} sahnelik senaryo yazıldı"));
        }
        other => {
            let why = match other {
                Err(e) => e.clone(),
                Ok(_) => "Claude yanıtında sahne yok".into(),
            };
            skeleton_storyboard(p);
            p.set_agent_status(2, AgentStatus::Error, format!("{why} — taslak iskelet oluşturuldu, metinleri siz yazmalısınız"));
            p.set_agent_status(3, AgentStatus::Error, "İskelet storyboard");
            p.push_log("Senarist", "error", why);
        }
    });

    // 2b. Voice director: pauses and tone tags for the narration (ElevenLabs v3)
    run_voice_director(shared, cancel);
    if stopped(cancel) {
        return;
    }

    // 4 + 5. Visual Hunter, Maps & Graphics
    let snapshot = with_shared(shared, |p| {
        p.set_agent_status(4, AgentStatus::Running, "Açık lisanslı görseller aranıyor…");
        p.set_agent_status(5, AgentStatus::Running, "Haritalar ve kartlar çiziliyor…");
        p.job.label = "Görseller ve haritalar".into();
        p.job.progress = 0.45;
        p.clone()
    });
    let scenes = super::visuals::hunt_all(&snapshot, shared, cancel);
    if stopped(cancel) {
        return;
    }
    let total: usize = scenes.iter().map(|s| s.visual_candidates.len()).sum();
    let snapshot = with_shared(shared, |p| {
        p.scenes = scenes;
        p.set_agent_status(5, AgentStatus::Done, "Harita ve kartlar hazır");
        p.job.progress = 0.7;
        p.push_log("Görsel Avcısı", "ok", format!("{total} görsel adayı hazırlandı"));
        p.clone()
    });

    // 4b. Relevance review by Claude looking at the thumbnails
    if snapshot.relevance_check && snapshot.claude_cli_available {
        with_shared(shared, |p| p.agent_activity(4, "🖼 Claude görselleri inceliyor (alaka denetimi)…"));
        let reviewed = super::visuals::relevance_review(&snapshot, shared, cancel);
        if stopped(cancel) {
            return;
        }
        with_shared(shared, |p| {
            if let Some(scenes) = reviewed {
                p.scenes = scenes;
                p.set_agent_status(4, AgentStatus::Done, format!("{total} aday; alaka denetimi yapıldı"));
            } else {
                p.set_agent_status(4, AgentStatus::Done, format!("{total} aday (alaka denetimi yapılamadı)"));
            }
        });
    } else {
        with_shared(shared, |p| p.set_agent_status(4, AgentStatus::Done, format!("{total} aday")));
    }

    // 6. Sound & FX (score rendered outside the lock)
    let snapshot = with_shared(shared, |p| {
        p.set_agent_status(6, AgentStatus::Running, "Tempo ve müzik…");
        p.job.label = "Ses ve efekt".into();
        p.job.progress = 0.85;
        p.clone()
    });
    let audio = super::audio::design(&snapshot);
    with_shared(shared, |p| {
        audio.apply(p);
        p.set_agent_status(6, AgentStatus::Done, p.fx_audit_summary.clone());
    });

    // 8. Packager
    let snapshot = with_shared(shared, |p| {
        p.set_agent_status(8, AgentStatus::Running, "Bölümler, açıklama, kapak…");
        p.clone()
    });
    let pkg = super::package::build(&snapshot);
    with_shared(shared, |p| {
        pkg.apply(p);
        p.set_agent_status(8, AgentStatus::Done, "Paket taslağı hazır");
    });

    // 9. Chief Editor (rules; the content review runs on request / at publish)
    with_shared(shared, |p| {
        let r = director::review(p);
        p.set_agent_status(9, AgentStatus::Done, format!("Yayınlanabilirlik {:.0}/100 — {}", r.score, r.verdict));
        p.director = Some(r);
        p.stage = PipelineStage::ReviewScenes;
        p.job.progress = 1.0;
        p.push_log("CrStudio", "ok", "Ön hazırlık bitti. Sahneleri inceleyin, görselleri seçin ve onaylayın.");
    });
}

/// Prompt for the voice director: every scene's narration directed for ElevenLabs v3 (audio tags
/// and pauses), the words unchanged.
pub fn voice_direction_prompt(p: &CrStudioPipeline) -> String {
    let scenes: Vec<Value> = p.scenes.iter().map(|s| json!({"i": s.index, "chapter": s.chapter, "text": s.narration_text})).collect();
    format!(
        r#"Sen CrStudio'nun Ses Yönetmenisin. Belgesel anlatıcısının (ElevenLabs eleven_v3, Türkçe) her sahneyi nasıl okuyacağını yönet.
Video: "{title}" — ton: {tone}. Anlatım ağır, dramatik ve gizemli olmalı: kısa vuruşlar, sorulardan önce ve sonra duraklar.

Kurallar:
- Kelimeleri DEĞİŞTİRME, ekleme ya da silme yapma. Yalnız ses etiketleri, "..." ve noktalama ekle.
- Etiketler (yalnız bunlar): [pause] [long pause] [slowly] [low voice] [dramatic] [curious]. Etiketleri cümle başına koy, aşırıya kaçma (sahne başına 2-5 etiket).
- Önemli bir sözden ya da sayıdan önce "..." ile nefes bırak; sorulardan sonra [pause] ya da [long pause].
- Bölümün ilk sahnesine [slowly] ya da [dramatic] ile gir; belge/sayı veren cümleler sakin, rivayet ve sır anları [low voice].
Örnek (onaylanmış üslup):
metin: "Yedi genç bir mağaraya girdi. Ve uyudular. Uyandıklarında 309 yıl geçmişti. Peki bu rakam gerçekten tutuyor mu?"
yönetmenli: "[dramatic] [slowly] Yedi genç... bir mağaraya girdi. [pause] Ve uyudular. [long pause] [low voice] Uyandıklarında... [pause] 309 yıl geçmişti. [long pause] Peki bu rakam... [pause] gerçekten tutuyor mu?"

Sahneler (JSON): {scenes}

Yalnız şu JSON'u döndür: {{"voiceTexts":[{{"i":0,"voiceText":"..."}}]}}"#,
        title = p.title,
        tone = p.tone.label(),
        scenes = Value::Array(scenes),
    )
}

/// Apply the voice director's answer: a directed text is kept only when its words are exactly the
/// scene's narration (a director that rewrote the script is ignored for that scene).
pub fn apply_voice_direction(p: &mut CrStudioPipeline, v: &Value) -> usize {
    let mut n = 0;
    for item in v["voiceTexts"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let (Some(i), Some(vt)) = (item["i"].as_u64(), item["voiceText"].as_str()) else { continue };
        let Some(sc) = p.scenes.get_mut(i as usize) else { continue };
        let vt: String = vt.chars().take(4000).collect();
        if vt.trim().is_empty() || !super::voicecheck::compare(&vt, &sc.narration_text).is_empty() {
            continue;
        }
        if sc.voice_text != vt {
            sc.voiceover_stale = sc.voiceover_path.is_some();
            sc.voice_text = vt;
        }
        n += 1;
    }
    n
}

/// The voice director as a job (also run after the script in pre-production).
pub fn run_voice_director(shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>) {
    let snapshot = with_shared(shared, |p| {
        p.job.label = "Ses Yönetmeni: anlatımı yönetiyor".into();
        p.clone()
    });
    if snapshot.scenes.is_empty() || claude::find_claude().is_none() {
        with_shared(shared, |p| p.push_log("Ses Yönetmeni", "warn", "Claude yok ya da sahne yok; anlatım yönetilmedi"));
        return;
    }
    let mut req = ClaudeRequest::new(voice_direction_prompt(&snapshot), snapshot.claude_timeout_sec.min(900));
    req.max_turns = Some(3);
    let r = run_agent(shared, cancel, 2, "Ses Yönetmeni", &req);
    if stopped(cancel) {
        return;
    }
    with_shared(shared, |p| match r {
        Ok(v) => {
            let n = apply_voice_direction(p, &v);
            let total = p.scenes.len();
            p.push_log(
                "Ses Yönetmeni",
                if n == total { "ok" } else { "warn" },
                format!("{n}/{total} sahnenin anlatımı yönetildi (duraklar ve ton etiketleri)"),
            );
        }
        Err(e) => p.push_log("Ses Yönetmeni", "error", e),
    });
}

/// The Chief Editor's full review (rules + Claude) as a job.
pub fn run_director(shared: &Arc<Mutex<CrStudioPipeline>>, cancel: &Arc<AtomicBool>) {
    let snapshot = with_shared(shared, |p| {
        p.set_agent_status(9, AgentStatus::Running, "Videoyu değerlendiriyor…");
        p.job.label = "Yayın Yönetmeni değerlendiriyor".into();
        p.clone()
    });
    let rules = director::review(&snapshot);
    let mut report = rules.clone();
    if snapshot.director_ai && claude::find_claude().is_some() {
        let mut req = ClaudeRequest::new(director::ai_prompt(&snapshot, &rules), snapshot.claude_timeout_sec.min(600));
        req.max_turns = Some(3);
        match run_agent(shared, cancel, 9, "Yayın Yönetmeni", &req) {
            Ok(v) => report = director::merge_ai_review(rules, &v),
            Err(e) => with_shared(shared, |p| p.push_log("Yayın Yönetmeni", "error", format!("İçerik değerlendirmesi yapılamadı: {e}"))),
        }
    }
    with_shared(shared, |p| {
        p.set_agent_status(9, AgentStatus::Done, format!("Yayınlanabilirlik {:.0}/100 — {}", report.score, report.verdict));
        p.push_log("Yayın Yönetmeni", if report.blocking { "error" } else { "ok" }, format!("Puan {:.0}/100: {}", report.score, report.verdict));
        p.director = Some(report);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_count_follows_duration() {
        let mut p = CrStudioPipeline { target_duration_sec: 480.0, ..Default::default() };
        let long = scene_count(&p);
        assert!((40..=64).contains(&long), "{long}");
        p.format = super::super::VideoFormatPreset::YoutubeShorts;
        p.target_duration_sec = 50.0;
        assert!((6..=12).contains(&scene_count(&p)));
    }

    #[test]
    fn script_json_is_validated() {
        let mut p = CrStudioPipeline::default();
        p.research.places = vec![Place { name: "Efes".into(), lat: 37.94, lon: 27.34, note: "mağara".into() }];
        p.research.sources = vec![SourceRef { title: "a".into(), url: "https://a".into(), publisher: String::new() }];
        let v = json!({"scenes": [
            {"title":"Kanca","narrationText":"Yedi genç neden üç yüz yıl uyudu?","visualType":"bogus","cameraMotion":"spin","sfxCue":"x","overlays":["info_box","hack"],"sources":[0,7]},
            {"title":"Efes","narrationText":"Rivayete göre mağara Efes yakınındaydı.","visualType":"animated_map","mapPlace":0,"factKind":"rivayet"},
            {"title":"Veri","narrationText":"Sayılar.","visualType":"infographic_chart"}
        ], "tags":["efsane"]});
        assert!(apply_script(&mut p, &v));
        assert_eq!(p.scenes.len(), 3);
        assert_eq!(p.scenes[0].visual_type, "stock_photo");
        assert_eq!(p.scenes[0].camera_motion, "zoom_in");
        assert_eq!(p.scenes[0].overlays, vec!["info_box".to_string()]);
        assert_eq!(p.scenes[0].sources, vec![0]);
        assert_eq!(p.scenes[1].map_spec.as_ref().unwrap().location_name, "EFES");
        assert_eq!(p.scenes[2].visual_type, "stock_photo", "no chart data -> no fake infographic");
        assert!(!apply_script(&mut p, &json!({"scenes": []})));
    }

    #[test]
    fn skeleton_is_marked_as_template() {
        let mut p = CrStudioPipeline { title: "Yedi Uyurlar".into(), ..Default::default() };
        skeleton_storyboard(&mut p);
        assert!(!p.scenes.is_empty());
        assert!(p.scenes.iter().all(|s| s.text_origin == "template" && s.narration_text.starts_with("(Taslak)")));
        let r = director::review(&p);
        assert!(r.blocking);
    }
}

#[cfg(test)]
mod voice_direction_tests {
    use super::*;
    use crate::crstudio::SceneSpec;

    #[test]
    fn a_directed_text_is_kept_only_when_the_words_are_unchanged() {
        let mut p = CrStudioPipeline::default();
        for (i, t) in ["Yedi genç bir mağaraya girdi. Ve uyudular.", "Şahmeran yılanların şahıdır."].iter().enumerate() {
            p.scenes.push(SceneSpec { index: i, narration_text: (*t).into(), ..SceneSpec::default() });
        }
        let v = json!({"voiceTexts": [
            {"i": 0, "voiceText": "[dramatic] [slowly] Yedi genç... bir mağaraya girdi. [pause] Ve uyudular."},
            {"i": 1, "voiceText": "[low voice] Şahmeran... yılanların kraliçesidir."},
            {"i": 9, "voiceText": "yok"}
        ]});
        assert_eq!(apply_voice_direction(&mut p, &v), 1);
        assert!(p.scenes[0].voice_text.contains("[pause]"));
        assert!(p.scenes[1].voice_text.is_empty(), "a rewritten text is ignored");
    }
}
