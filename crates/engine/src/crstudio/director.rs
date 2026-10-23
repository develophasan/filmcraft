//! Agent 9 — the Chief Editor ("Yayın Yönetmeni").
//!
//! Reviews the whole project after every stage and before export: a 0–100 publishability score,
//! a score per category, and for every gap the stage to go back to and what to do there. The
//! score comes from measurable rules (durations, licences, chapters, voice, overlays) so it is
//! repeatable and cannot be talked up; Claude's editorial review, when it ran, is folded in as a
//! separate "İçerik" category with its own notes.

use serde::{Deserialize, Serialize};

use super::{CrStudioPipeline, PipelineStage};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CategoryScore {
    pub id: String,
    pub label: String,
    /// 0–100.
    pub score: f64,
    pub weight: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirectorIssue {
    /// `kritik` (blocks publishing) | `önemli` | `öneri`.
    pub severity: String,
    pub category: String,
    /// Stage to go back to.
    pub stage: PipelineStage,
    pub scene: Option<usize>,
    pub message: String,
    pub fix: String,
}

impl Default for DirectorIssue {
    fn default() -> Self {
        Self { severity: "öneri".into(), category: String::new(), stage: PipelineStage::ReviewScenes, scene: None, message: String::new(), fix: String::new() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirectorReport {
    /// 0–100 overall publishability.
    pub score: f64,
    pub verdict: String,
    pub categories: Vec<CategoryScore>,
    pub issues: Vec<DirectorIssue>,
    /// Claude's editorial notes (empty when only the rules ran).
    pub ai_notes: Vec<String>,
    /// Claude's 0–100 content score, if it ran.
    pub ai_score: Option<f64>,
    pub blocking: bool,
}

/// Licences CrStudio accepts for downloaded visuals and sounds.
pub fn license_allowed(license: &str) -> bool {
    let l = license.to_lowercase();
    if l.contains("nc") && (l.contains("cc") || l.contains("non")) || l.contains("-nd") || l.contains(" nd") || l.contains("fair use") {
        return false;
    }
    l.contains("public domain")
        || l.starts_with("pd")
        || l.contains("cc0")
        || l.contains("cc by")
        || l.contains("cc-by")
        || l.contains("mit")
        || l.contains("apache")
        || l.contains("original")
        || l.contains("kendi")
        || l.contains("özgün")
        || l.contains("kamu malı")
        || l.contains("natural earth")
}

struct Acc {
    issues: Vec<DirectorIssue>,
}

impl Acc {
    fn add(&mut self, sev: &str, cat: &str, stage: PipelineStage, scene: Option<usize>, msg: impl Into<String>, fix: impl Into<String>) {
        self.issues.push(DirectorIssue { severity: sev.into(), category: cat.into(), stage, scene, message: msg.into(), fix: fix.into() });
    }
}

fn penalty(issues: &[DirectorIssue], cat: &str) -> f64 {
    issues
        .iter()
        .filter(|i| i.category == cat)
        .map(|i| match i.severity.as_str() {
            "kritik" => 45.0,
            "önemli" => 18.0,
            _ => 6.0,
        })
        .sum::<f64>()
}

/// Measure the project and produce the report (rules only; merge Claude's review with
/// [`merge_ai_review`]).
pub fn review(p: &CrStudioPipeline) -> DirectorReport {
    let mut a = Acc { issues: Vec::new() };
    let n = p.scenes.len();
    let vertical = p.format.is_vertical();
    let total: f64 = p.scenes.iter().map(|s| s.effective_duration()).sum();

    // --- Hook & story
    if n == 0 {
        a.add("kritik", "hook", PipelineStage::Setup, None, "Henüz sahne yok.", "Konu ekranından ajanları başlatın.");
    }
    if let Some(first) = p.scenes.first() {
        let words = first.narration_text.split_whitespace().count();
        if words > 32 {
            a.add(
                "önemli",
                "hook",
                PipelineStage::ReviewScenes,
                Some(0),
                format!("Açılış cümlesi uzun ({words} kelime)."),
                "İlk 5 saniyede merak uyandıran tek, kısa bir cümle yazın.",
            );
        }
        if !first.narration_text.contains('?') && !first.narration_text.contains('!') && p.social_meta.hook_line.trim().is_empty() {
            a.add(
                "öneri",
                "hook",
                PipelineStage::ReviewScenes,
                Some(0),
                "Açılışta soru ya da çarpıcı bir iddia yok.",
                "Kancayı soru ya da beklenmedik bir bilgiyle açın.",
            );
        }
    }
    if p.title.chars().count() > 70 {
        a.add(
            "öneri",
            "youtube",
            PipelineStage::Publish,
            None,
            "Başlık 70 karakteri aşıyor; aramada kesilir.",
            "Başlığı kısaltın ya da alternatif başlıklardan birini seçin.",
        );
    }

    // --- Script & sources
    let template_scenes: Vec<usize> = p.scenes.iter().filter(|s| s.text_origin == "template").map(|s| s.index).collect();
    if !template_scenes.is_empty() {
        a.add(
            "kritik",
            "senaryo",
            PipelineStage::Setup,
            template_scenes.first().copied(),
            format!("{} sahnenin metni yedek şablondan geliyor (Claude yazmadı).", template_scenes.len()),
            "Ajanları yeniden çalıştırın ya da bu sahnelerin metnini kendiniz yazın.",
        );
    }
    if p.research.sources.is_empty() && n > 0 {
        a.add("önemli", "senaryo", PipelineStage::Setup, None, "Araştırmada kaynak yok.", "Araştırma ajanını web erişimiyle yeniden çalıştırın.");
    }
    let unsourced = p.scenes.iter().filter(|s| s.fact_kind == "belge" && s.sources.is_empty()).count();
    if unsourced > 0 {
        a.add(
            "önemli",
            "senaryo",
            PipelineStage::ReviewScenes,
            None,
            format!("{unsourced} sahne belgesel bilgi veriyor ama kaynağı yok."),
            "Kaynak ekleyin ya da anlatımı 'rivayete göre' diye yumuşatın.",
        );
    }

    // --- Visuals & licences
    for s in &p.scenes {
        match &s.selected_visual {
            None => a.add(
                "kritik",
                "gorsel",
                PipelineStage::ReviewScenes,
                Some(s.index),
                format!("Sahne {}: görsel seçilmemiş.", s.index + 1),
                "Görsel adaylarından birini seçin ya da yeniden arayın.",
            ),
            Some(path) => {
                if let Some(c) = s.visual_candidates.iter().find(|c| &c.local_path == path) {
                    if !license_allowed(&c.license) {
                        a.add(
                            "kritik",
                            "telif",
                            PipelineStage::ReviewScenes,
                            Some(s.index),
                            format!("Sahne {}: görsel lisansı uygun değil ({}).", s.index + 1, c.license),
                            "Kamu malı, CC0 ya da CC BY lisanslı bir görsel seçin.",
                        );
                    }
                    if let Some(r) = c.relevance
                        && r < 45.0
                    {
                        a.add(
                            "önemli",
                            "gorsel",
                            PipelineStage::ReviewScenes,
                            Some(s.index),
                            format!("Sahne {}: görsel konuyla zayıf ilişkili ({r:.0}/100).", s.index + 1),
                            "Alaka puanı yüksek başka bir aday seçin.",
                        );
                    }
                    if (c.kind == "wikimedia_commons" || c.kind == "openverse") && c.author.trim().is_empty() {
                        a.add(
                            "önemli",
                            "telif",
                            PipelineStage::Publish,
                            Some(s.index),
                            format!("Sahne {}: görselin yazarı bilinmiyor.", s.index + 1),
                            "Kredisi belli bir görsel seçin.",
                        );
                    }
                }
            }
        }
    }
    let procedural = p.scenes.iter().filter(|s| s.selected_visual.as_deref().is_some_and(|v| v.contains("_doc_graphic") || v.contains("_map"))).count();
    if n >= 4 && procedural * 10 > n * 7 {
        a.add(
            "önemli",
            "gorsel",
            PipelineStage::ReviewScenes,
            None,
            "Sahnelerin çoğu kart/harita; gerçek görüntü az.",
            "Arşiv görseli ya da kendi çekiminizi ekleyerek çeşitlendirin.",
        );
    }

    // --- Voice
    let missing_vo = p.scenes.iter().filter(|s| s.voiceover_path.is_none()).count();
    if missing_vo > 0 && n > 0 {
        a.add(
            "kritik",
            "ses",
            PipelineStage::Voiceover,
            None,
            format!("{missing_vo} sahnenin seslendirmesi yok."),
            "Taslak AI ses üretin ya da kendi kaydınızı alın.",
        );
    }
    let stale = p.scenes.iter().filter(|s| s.voiceover_stale).count();
    if stale > 0 {
        a.add(
            "önemli",
            "ses",
            PipelineStage::Voiceover,
            None,
            format!("{stale} sahnede metin değişti, kayıt eski metni söylüyor."),
            "Bu sahneleri yeniden kaydedin.",
        );
    }
    let cloned = p.scenes.iter().filter(|s| s.voiceover_path.is_some() && s.voiceover_engine == "elevenlabs").count();
    if cloned > 0 {
        a.add(
            "öneri",
            "ses",
            PipelineStage::Publish,
            None,
            format!("{cloned} sahnede ElevenLabs klon sesi var."),
            "YouTube'a yüklerken 'yapay zekâ kullanımı / değiştirilmiş içerik' bildirimini işaretleyin; kanca ve kapanış cümlelerini kendi sesinizle kaydetmek etkiyi artırır.",
        );
    }
    let ai_voice = p.scenes.iter().filter(|s| s.voiceover_path.is_some() && !matches!(s.voiceover_engine.as_str(), "record" | "file" | "elevenlabs")).count();
    if ai_voice > 0 {
        a.add(
            "önemli",
            "ses",
            PipelineStage::Voiceover,
            None,
            format!("{ai_voice} sahnede taslak AI ses var."),
            "Teleprompter ile kendi sesinizle kaydedin; vurgu ve tonlama izlenmeyi artırır.",
        );
    }

    // --- Pacing
    for s in &p.scenes {
        let d = s.effective_duration();
        if d > 22.0 {
            a.add(
                "öneri",
                "tempo",
                PipelineStage::ReviewScenes,
                Some(s.index),
                format!("Sahne {} uzun ({d:.0} sn).", s.index + 1),
                "İkiye bölün ya da B-roll alt kesmesi ekleyin.",
            );
        }
    }
    if p.target_duration_sec > 0.0 && total > 0.0 {
        let ratio = total / p.target_duration_sec;
        if !(0.75..=1.3).contains(&ratio) {
            a.add(
                "önemli",
                "tempo",
                PipelineStage::ReviewScenes,
                None,
                format!("Süre hedeften sapıyor: {total:.0} sn (hedef {:.0} sn).", p.target_duration_sec),
                "Sahne ekleyin/çıkarın ya da metinleri uzatın/kısaltın.",
            );
        }
    }
    if vertical && total > 180.0 {
        a.add("kritik", "youtube", PipelineStage::ReviewScenes, None, "Shorts 3 dakikayı aşamaz.", "Sahneleri azaltın.");
    }

    // --- YouTube packaging
    if !vertical && total >= 60.0 {
        let chapters = p.youtube_chapters_text.lines().filter(|l| !l.trim().is_empty()).count();
        if chapters < 3 {
            a.add("öneri", "youtube", PipelineStage::Publish, None, "Bölüm (chapter) listesi yok ya da 3'ten az.", "Paketleme adımını çalıştırın.");
        }
    }
    if p.thumbnail_path.is_none() {
        a.add("önemli", "youtube", PipelineStage::Publish, None, "Kapak görseli (thumbnail) yok.", "Paketleme adımında kapak üretin.");
    }
    let tags = p.social_meta.tags.len();
    if tags < 5 {
        a.add("öneri", "youtube", PipelineStage::Publish, None, format!("Etiket sayısı az ({tags})."), "5–15 etiket ekleyin.");
    }
    if p.social_meta.description.trim().len() < 120 {
        a.add("öneri", "youtube", PipelineStage::Publish, None, "Açıklama kısa.", "İlk iki satırda konuyu ve merakı özetleyin; kaynak ve kredileri ekleyin.");
    }
    if !p.brand_kit_enabled {
        a.add("öneri", "youtube", PipelineStage::Voiceover, None, "Abone ol / bitiş ekranı kapalı.", "Kanal kimliğini açın.");
    }
    if p.assembled_sequence.is_none() && n > 0 {
        a.add("önemli", "kurgu", PipelineStage::Voiceover, None, "Kurgu henüz oluşturulmadı.", "'Kurguya gönder' ile timeline'ı kurun.");
    }

    let cats: [(&str, &str, f64); 8] = [
        ("hook", "Kanca ve açılış", 1.2),
        ("senaryo", "Senaryo ve kaynaklar", 1.4),
        ("gorsel", "Görsel kalite ve alaka", 1.2),
        ("telif", "Telif ve lisans", 1.5),
        ("ses", "Ses", 1.0),
        ("tempo", "Tempo ve süre", 0.9),
        ("youtube", "YouTube uyumu", 0.8),
        ("kurgu", "Kurgu", 0.6),
    ];
    let mut categories: Vec<CategoryScore> = cats
        .iter()
        .map(|(id, label, w)| CategoryScore { id: (*id).into(), label: (*label).into(), score: (100.0 - penalty(&a.issues, id)).clamp(0.0, 100.0), weight: *w })
        .collect();
    let wsum: f64 = categories.iter().map(|c| c.weight).sum::<f64>().max(1e-9);
    let mut score = categories.iter().map(|c| c.score * c.weight).sum::<f64>() / wsum;
    let blocking = a.issues.iter().any(|i| i.severity == "kritik");
    if blocking {
        score = score.min(59.0);
    }
    a.issues.sort_by_key(|i| match i.severity.as_str() {
        "kritik" => 0,
        "önemli" => 1,
        _ => 2,
    });
    categories.retain(|c| c.weight > 0.0);
    DirectorReport { score: score.round(), verdict: verdict(score, blocking), categories, issues: a.issues, ai_notes: Vec::new(), ai_score: None, blocking }
}

fn verdict(score: f64, blocking: bool) -> String {
    if blocking {
        "Yayına hazır değil: kritik eksikler var".into()
    } else if score >= 85.0 {
        "Yayına hazır".into()
    } else if score >= 70.0 {
        "Küçük düzeltmelerle yayınlanabilir".into()
    } else {
        "Önemli iyileştirmeler gerekiyor".into()
    }
}

/// Fold Claude's editorial review (`{"contentScore":0-100,"notes":[..],"suggestions":[{"stage","scene","message","fix","severity"}]}`)
/// into a rules report.
pub fn merge_ai_review(mut r: DirectorReport, v: &serde_json::Value) -> DirectorReport {
    if let Some(cs) = v.get("contentScore").and_then(serde_json::Value::as_f64).filter(|x| x.is_finite()) {
        let cs = cs.clamp(0.0, 100.0);
        r.ai_score = Some(cs);
        r.categories.push(CategoryScore { id: "icerik".into(), label: "İçerik (Claude değerlendirmesi)".into(), score: cs, weight: 1.3 });
        let wsum: f64 = r.categories.iter().map(|c| c.weight).sum::<f64>().max(1e-9);
        let mut score = r.categories.iter().map(|c| c.score * c.weight).sum::<f64>() / wsum;
        if r.blocking {
            score = score.min(59.0);
        }
        r.score = score.round();
        r.verdict = verdict(r.score, r.blocking);
    }
    if let Some(notes) = v.get("notes").and_then(serde_json::Value::as_array) {
        r.ai_notes = notes.iter().filter_map(|n| n.as_str()).take(12).map(str::to_string).collect();
    }
    if let Some(sugs) = v.get("suggestions").and_then(serde_json::Value::as_array) {
        for s in sugs.iter().take(12) {
            let stage = match s.get("stage").and_then(serde_json::Value::as_str).unwrap_or("") {
                "konu" | "setup" | "senaryo" | "arastirma" => PipelineStage::Setup,
                "ses" | "seslendirme" | "voiceover" => PipelineStage::Voiceover,
                "kurgu" | "studio" | "edit" => PipelineStage::InStudio,
                "yayin" | "paket" | "publish" => PipelineStage::Publish,
                _ => PipelineStage::ReviewScenes,
            };
            let sev = s.get("severity").and_then(serde_json::Value::as_str).unwrap_or("öneri");
            r.issues.push(DirectorIssue {
                severity: if matches!(sev, "kritik" | "önemli") { sev.to_string() } else { "öneri".into() },
                category: "icerik".into(),
                stage,
                scene: s.get("scene").and_then(serde_json::Value::as_u64).map(|x| x as usize).filter(|x| *x < 1000),
                message: s.get("message").and_then(serde_json::Value::as_str).unwrap_or("").chars().take(300).collect(),
                fix: s.get("fix").and_then(serde_json::Value::as_str).unwrap_or("").chars().take(300).collect(),
            });
        }
    }
    r
}

/// The prompt for Claude's editorial review.
pub fn ai_prompt(p: &CrStudioPipeline, rules: &DirectorReport) -> String {
    let scenes: Vec<serde_json::Value> = p
        .scenes
        .iter()
        .map(|s| {
            serde_json::json!({
                "sahne": s.index,
                "baslik": s.title,
                "anlatim": s.narration_text,
                "sure": (s.effective_duration() * 10.0).round() / 10.0,
                "gorselTuru": s.visual_type,
                "eklentiler": s.overlays,
                "tur": s.fact_kind,
            })
        })
        .collect();
    let findings: Vec<String> = rules.issues.iter().map(|i| format!("[{}] {}", i.severity, i.message)).collect();
    format!(
        r#"Sen CrStudio'nun Yayın Yönetmenisin: Türkçe YouTube belgesel/efsane kanalları için deneyimli, dürüst bir genel yayın yönetmeni.
Aşağıdaki videoyu yayınlanabilirlik açısından değerlendir. Övgü değil, gerçekçi ve uygulanabilir eleştiri istiyorum.
Başlık: {title}
Format: {fmt} · Ton: {tone} · Hedef süre: {target:.0} sn · Gerçek süre: {total:.0} sn
Kanca cümlesi: {hook}
Sahneler (JSON): {scenes}
Kural tabanlı ölçümlerin bulduğu eksikler: {findings}

Yalnız şu JSON'u döndür:
{{"contentScore": 0-100, "notes": ["en fazla 6 kısa genel not"], "suggestions": [{{"severity":"kritik|önemli|öneri","stage":"senaryo|sahneler|ses|kurgu|yayin","scene": sahne_no_veya_null,"message":"sorun","fix":"somut çözüm"}}]}}
Ölçütler: ilk 5 saniyenin merak gücü, anlatı akışı ve bölüm geçişleri, bilgi doğruluğu ve rivayet/belge ayrımı, izleyici tutma (tempo, tekrar), başlık-kapak vaadi ile içeriğin uyumu, YouTube'un orijinal olmayan/seri üretim içerik politikasına uygunluk."#,
        title = p.title,
        fmt = p.format.label(),
        tone = p.tone.label(),
        target = p.target_duration_sec,
        total = p.scenes.iter().map(|s| s.effective_duration()).sum::<f64>(),
        hook = p.social_meta.hook_line,
        scenes = serde_json::Value::Array(scenes),
        findings = findings.join(" | "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licences() {
        assert!(license_allowed("Public domain"));
        assert!(license_allowed("CC BY-SA 4.0"));
        assert!(license_allowed("CC0"));
        assert!(!license_allowed("CC BY-NC 4.0"));
        assert!(!license_allowed("CC BY-ND 2.0"));
        assert!(!license_allowed("unknown"));
        assert!(license_allowed("Natural Earth (kamu malı) + özgün çizim"), "the map renderer's own licence line");
        assert!(license_allowed("Özgün çizim (kendi)"));
    }

    #[test]
    fn empty_project_is_blocked() {
        let r = review(&CrStudioPipeline::default());
        assert!(r.blocking);
        assert!(r.score <= 59.0);
        assert!(r.issues.iter().any(|i| i.severity == "kritik"));
    }

    #[test]
    fn ai_review_merges() {
        let r = review(&CrStudioPipeline::default());
        let v = serde_json::json!({"contentScore": 80, "notes": ["İyi kanca"], "suggestions": [{"severity":"önemli","stage":"ses","scene":2,"message":"m","fix":"f"}]});
        let m = merge_ai_review(r, &v);
        assert_eq!(m.ai_score, Some(80.0));
        assert_eq!(m.ai_notes, vec!["İyi kanca".to_string()]);
        assert!(m.issues.iter().any(|i| i.stage == PipelineStage::Voiceover && i.scene == Some(2)));
    }
}
