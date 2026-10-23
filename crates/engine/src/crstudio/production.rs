//! Üretim Hattı (the production line): what is left to make the video, what each step costs
//! (time, ElevenLabs credits, a hot Mac), and a queue that runs the approved steps one after the
//! other. Every step is a list of ordinary commands, so the queue, the CLI and the agents all run
//! the same thing.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CrStudioPipeline, SceneSpec, turkish};

/// Frames per minute the final-quality Blender render manages on the M1 (measured).
const BLENDER_FPM: f64 = 45.0;
/// Seconds of export per second of video (1080p H.264, measured).
const EXPORT_RATIO: f64 = 3.2;

/// One command waiting in the queue.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QueuedCommand {
    pub label: String,
    pub command: String,
    pub params: Value,
    /// Estimated seconds.
    pub seconds: f64,
}

/// One step of the plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Step {
    pub id: String,
    pub label: String,
    /// What it will do, in a line ("33 sahne · ~4 800 kredi").
    pub detail: String,
    pub seconds: f64,
    /// ElevenLabs credits it spends (estimate).
    pub credits: u64,
    /// Long and hot: Blender, export.
    pub heavy: bool,
    /// Nothing left to do for this step.
    pub done: bool,
    pub commands: Vec<QueuedCommand>,
}

/// A place key for "is this the same place as before".
fn place_key(sc: &SceneSpec) -> Option<String> {
    let m = sc.map_spec.as_ref()?;
    let first = m.location_name.split(',').next().unwrap_or("").trim();
    (!first.is_empty()).then(|| turkish::ascii_fold(&turkish::lower(first)))
}

/// What the Blender pass should render: a map where the story arrives at a new place, a title card
/// at each chapter card; a scene that only repeats the place on screen gets a photo instead.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapPlan {
    /// (scene index, shot) to render.
    pub renders: Vec<(usize, &'static str)>,
    /// Scenes whose map only repeats the place before: they show a photo.
    pub repeats: Vec<usize>,
}

pub fn map_plan(p: &CrStudioPipeline) -> MapPlan {
    let mut plan = MapPlan::default();
    let mut last_place: Option<String> = None;
    for sc in &p.scenes {
        let is_map = sc.visual_type == "animated_map" || (sc.map_spec.is_some() && sc.visual_type != "stock_photo");
        let key = place_key(sc);
        if sc.visual_type == "chapter_card" {
            // a chapter that opens on a new place flies there; otherwise a title card
            if key.is_some() && key != last_place {
                plan.renders.push((sc.index, "map"));
                last_place = key;
            } else {
                plan.renders.push((sc.index, "chapter"));
            }
        } else if is_map && key.is_some() {
            if key == last_place {
                plan.repeats.push(sc.index);
            } else {
                plan.renders.push((sc.index, "map"));
            }
            last_place = key;
        }
    }
    plan
}

/// Whether the scene already shows a Blender render (of any shot: a chapter card flown as a map
/// counts as done).
fn rendered(sc: &SceneSpec, _shot: &str) -> bool {
    let Some(sel) = sc.selected_visual.as_deref() else { return false };
    sc.visual_candidates.iter().any(|c| c.local_path == sel && c.kind == "blender" && !c.id.ends_with("_draft"))
}

/// Characters the narration of `sc` sends to the voice.
fn voice_chars(sc: &SceneSpec) -> usize {
    let t = if sc.voice_text.trim().is_empty() { &sc.narration_text } else { &sc.voice_text };
    t.chars().count()
}

fn needs_voice(sc: &SceneSpec) -> bool {
    !sc.narration_text.trim().is_empty() && (sc.voiceover_path.is_none() || sc.voiceover_stale)
}

fn q(label: impl Into<String>, command: &str, params: Value, seconds: f64) -> QueuedCommand {
    QueuedCommand { label: label.into(), command: command.into(), params, seconds }
}

/// The steps left, in order.
pub fn plan(p: &CrStudioPipeline) -> Vec<Step> {
    let mut steps = Vec::new();
    let eleven = p.voice_engine == "elevenlabs";
    let total = p.total_seconds();

    // 1. Voice director
    let undirected = p.scenes.iter().filter(|s| !s.narration_text.trim().is_empty() && s.voice_text.trim().is_empty() && needs_voice(s)).count();
    if eleven {
        steps.push(Step {
            id: "direct".into(),
            label: "Ses Yönetmeni".into(),
            detail: if undirected == 0 { "Anlatım yönetildi".into() } else { format!("{undirected} sahnenin anlatımına durak ve ton etiketleri") },
            seconds: 20.0 + undirected as f64 * 1.5,
            done: undirected == 0,
            commands: if undirected == 0 { Vec::new() } else { vec![q("Ses Yönetmeni", "crstudio.directVoice", json!({}), 60.0)] },
            ..Default::default()
        });
    }

    // 2. Narration
    let todo: Vec<&SceneSpec> = p.scenes.iter().filter(|s| needs_voice(s)).collect();
    let chars: usize = todo.iter().map(|s| voice_chars(s)).sum();
    // retakes after the listening check (~10 %)
    let credits = if eleven { (chars as f64 * 1.1).round() as u64 } else { 0 };
    steps.push(Step {
        id: "voice".into(),
        label: "Seslendirme".into(),
        detail: if todo.is_empty() {
            "Tüm sahneler seslendirildi".into()
        } else if eleven {
            format!("{} sahne · {chars} karakter · ~{credits} kredi · dinleme denetimiyle", todo.len())
        } else {
            format!("{} sahne", todo.len())
        },
        seconds: todo.len() as f64 * 7.0,
        credits,
        done: todo.is_empty(),
        commands: if todo.is_empty() {
            Vec::new()
        } else {
            vec![q("Seslendirme", "crstudio.synthesizeVoiceover", json!({"all": true}), todo.len() as f64 * 7.0)]
        },
        ..Default::default()
    });

    // 3. Blender (Kadim Atlas)
    if p.visual_concept == "atlas" && !p.scenes.is_empty() {
        let mp = map_plan(p);
        let mut cmds = Vec::new();
        if mp.repeats.iter().any(|i| p.scenes.get(*i).is_some_and(|s| s.selected_visual.as_deref().is_some_and(|v| v.ends_with("_map.png")))) {
            cmds.push(q("Tekrarlayan haritalara fotoğraf", "crstudio.applyMapPlan", json!({}), 2.0));
        }
        let (mut maps, mut cards, mut frames) = (0, 0, 0.0);
        for (i, shot) in &mp.renders {
            let Some(sc) = p.scenes.get(*i) else { continue };
            if rendered(sc, shot) {
                continue;
            }
            let f = (sc.effective_duration() + 0.8) * 30.0;
            frames += f;
            if *shot == "map" {
                maps += 1;
            } else {
                cards += 1;
            }
            let what = if *shot == "map" { "harita" } else { "bölüm kartı" };
            cmds.push(q(
                format!("Blender · sahne {} {what}", i + 1),
                "crstudio.blender.render",
                json!({"sceneIndex": i, "shot": shot, "quality": "final"}),
                f / BLENDER_FPM * 60.0,
            ));
        }
        let seconds = frames / BLENDER_FPM * 60.0;
        steps.push(Step {
            id: "blender".into(),
            label: "Haritalar ve bölüm kartları (Blender)".into(),
            detail: if cmds.is_empty() {
                "Çizimler hazır".into()
            } else {
                format!("{maps} harita · {cards} bölüm kartı · {} sahne fotoğrafa geçer · Mac ısınır, kapağı açık kalsın", mp.repeats.len())
            },
            seconds,
            heavy: seconds > 120.0,
            done: cmds.is_empty(),
            commands: cmds,
            ..Default::default()
        });
    }

    // 4. Cut
    let ready = !p.scenes.is_empty() && p.scenes.iter().all(|s| !needs_voice(s));
    steps.push(Step {
        id: "cut".into(),
        label: "Kurgu (uzun video + Shorts)".into(),
        detail: if ready { "Kurgu denetimiyle birlikte".into() } else { "Önce seslendirme".into() },
        seconds: 15.0,
        commands: vec![q("Kurgu", "crstudio.assembleTimeline", json!({}), 8.0), q("Shorts kesiti", "crstudio.createShortsCut", json!({}), 5.0)],
        ..Default::default()
    });

    // 5. Publish package: description with chapters and the credits every licence asks for,
    // subtitles, thumbnail
    steps.push(Step {
        id: "package".into(),
        label: "Yayın paketi".into(),
        detail: "Açıklama, bölümler, kaynak ve lisans künyeleri (müzik dahil), altyazı, kapak".into(),
        seconds: 5.0,
        commands: vec![q("Yayın paketi", "crstudio.packageYoutube", json!({}), 5.0)],
        ..Default::default()
    });

    // 6. Export
    let seconds = (total + 60.0) * EXPORT_RATIO;
    steps.push(Step {
        id: "export".into(),
        label: "Dışa aktarma".into(),
        detail: format!("Uzun video (YouTube 1080p) + Shorts · ~{:.0} dk · Mac ısınır", seconds / 60.0),
        seconds,
        heavy: true,
        commands: vec![q("Dışa aktarma", "crstudio.exportAll", json!({}), seconds)],
        ..Default::default()
    });
    steps
}

/// The queue entries of the plan steps `ids` (in plan order; done steps add nothing).
pub fn commands_for(p: &CrStudioPipeline, ids: &[String]) -> Vec<QueuedCommand> {
    plan(p).into_iter().filter(|s| ids.contains(&s.id) && !s.done).flat_map(|s| s.commands).collect()
}

/// Give the scenes that only repeat a place on screen a photo: their own best one, else the best
/// of the same place's other scenes (copied with its licence). Returns how many changed.
pub fn apply_map_plan(p: &mut CrStudioPipeline) -> usize {
    let mp = map_plan(p);
    let mut used: Vec<String> = p.scenes.iter().filter_map(|s| s.selected_visual.clone()).collect();
    let mut changed = 0;
    for i in mp.repeats {
        let key = p.scenes.get(i).and_then(place_key);
        let photo = |s: &SceneSpec, used: &[String]| {
            let mut c: Vec<_> = s
                .visual_candidates
                .iter()
                .filter(|c| matches!(c.kind.as_str(), "wikimedia_commons" | "openverse" | "user") && !used.contains(&c.local_path))
                .filter(|c| c.relevance.is_none_or(|r| r >= 40.0))
                .cloned()
                .collect();
            c.sort_by(|a, b| b.relevance.unwrap_or(50.0).total_cmp(&a.relevance.unwrap_or(50.0)));
            c.into_iter().next()
        };
        let own = p.scenes.get(i).and_then(|s| photo(s, &used));
        let pick = own.or_else(|| {
            // the same place's other scenes, nearest first
            let mut others: Vec<&SceneSpec> = p.scenes.iter().filter(|s| s.index != i && place_key(s) == key).collect();
            others.sort_by_key(|s| s.index.abs_diff(i));
            others.into_iter().find_map(|s| photo(s, &used))
        });
        if let (Some(c), Some(sc)) = (pick, p.scenes.get_mut(i)) {
            used.push(c.local_path.clone());
            if !sc.visual_candidates.iter().any(|x| x.local_path == c.local_path) {
                sc.visual_candidates.insert(0, c.clone());
            }
            sc.selected_visual = Some(c.local_path);
            changed += 1;
        }
    }
    changed
}

/// Something on a scene worth a look before the export (the scene board's badges).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SceneIssue {
    pub scene: usize,
    /// `old_map` | `no_visual` | `low_relevance` | `duplicate` | `no_voice` | `voice_diff` | `long_scene`
    pub kind: String,
    pub message: String,
    /// 2: fix before export, 1: worth a look.
    pub severity: u8,
}

pub fn scene_issues(p: &CrStudioPipeline) -> Vec<SceneIssue> {
    let mut out = Vec::new();
    let mut push = |scene: usize, kind: &str, message: String, severity: u8| out.push(SceneIssue { scene, kind: kind.into(), message, severity });
    for sc in &p.scenes {
        let sel = sc.selected_visual.as_deref();
        match sel {
            None => push(sc.index, "no_visual", "Görsel yok".into(), 2),
            Some(v) => {
                if p.visual_concept == "atlas" && v.ends_with("_map.png") {
                    push(sc.index, "old_map", "Eski düz harita (Kadim Atlas değil)".into(), 2);
                }
                if let Some(c) = sc.visual_candidates.iter().find(|c| c.local_path == v)
                    && let Some(r) = c.relevance.filter(|r| *r < 40.0)
                {
                    push(sc.index, "low_relevance", format!("Görselin alakası düşük ({r:.0}/100)"), 1);
                }
                if let Some(other) = p.scenes.iter().find(|o| o.index != sc.index && o.selected_visual.as_deref() == Some(v)) {
                    push(sc.index, "duplicate", format!("Aynı görsel sahne {}'de de var", other.index + 1), 1);
                }
            }
        }
        if needs_voice(sc) {
            push(sc.index, "no_voice", if sc.voiceover_stale { "Metin değişti, ses eski".into() } else { "Seslendirme yok".into() }, 2);
        } else if !sc.voice_check.is_empty() {
            push(sc.index, "voice_diff", format!("Dinleme farkı: {}", sc.voice_check.join("; ")), 1);
        }
        if sc.effective_duration() > 25.0 {
            push(sc.index, "long_scene", format!("Uzun sahne ({:.0} sn): bölmek ya da ikinci görsel iyi olur", sc.effective_duration()), 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crstudio::{MapSpec, VisualCandidate};

    fn scene(i: usize, kind: &str, place: Option<&str>) -> SceneSpec {
        SceneSpec {
            index: i,
            visual_type: kind.into(),
            narration_text: "Bir cümle. İki cümle.".into(),
            map_spec: place.map(|p| MapSpec { location_name: p.into(), ..Default::default() }),
            ..SceneSpec::default()
        }
    }

    #[test]
    fn a_map_only_where_the_story_reaches_a_new_place() {
        let mut p = CrStudioPipeline { visual_concept: "atlas".into(), ..Default::default() };
        p.scenes = vec![
            scene(0, "stock_photo", None),
            scene(1, "chapter_card", Some("TARSUS, MERSİN")),
            scene(2, "animated_map", Some("Tarsus, Mersin")),
            scene(3, "animated_map", Some("TARSUS")),
            scene(4, "chapter_card", None),
            scene(5, "animated_map", Some("MARDİN")),
        ];
        let mp = map_plan(&p);
        assert_eq!(mp.renders, vec![(1, "map"), (4, "chapter"), (5, "map")]);
        assert_eq!(mp.repeats, vec![2, 3]);
    }

    #[test]
    fn repeats_get_a_photo_of_the_same_place_with_its_licence() {
        let mut p = CrStudioPipeline { visual_concept: "atlas".into(), ..Default::default() };
        let mut a = scene(0, "animated_map", Some("Tarsus"));
        a.visual_candidates.push(VisualCandidate {
            kind: "wikimedia_commons".into(),
            local_path: "/x/hamam.jpg".into(),
            license: "CC BY-SA 4.0".into(),
            relevance: Some(80.0),
            ..Default::default()
        });
        a.visual_candidates.push(VisualCandidate {
            kind: "wikimedia_commons".into(),
            local_path: "/x/hamam2.jpg".into(),
            license: "CC0".into(),
            relevance: Some(70.0),
            ..Default::default()
        });
        let mut b = scene(1, "animated_map", Some("Tarsus"));
        b.selected_visual = Some("/x/scene_02_map.png".into());
        p.scenes = vec![a, b];
        assert_eq!(apply_map_plan(&mut p), 1);
        let sel = p.scenes[1].selected_visual.clone().unwrap_or_default();
        assert_eq!(sel, "/x/hamam.jpg");
        assert_eq!(p.scenes[1].visual_candidates[0].license, "CC BY-SA 4.0");
    }

    #[test]
    fn the_plan_counts_credits_and_skips_finished_work() {
        let mut p = CrStudioPipeline { voice_engine: "elevenlabs".into(), visual_concept: "atlas".into(), ..Default::default() };
        p.scenes = vec![scene(0, "stock_photo", None), scene(1, "stock_photo", None)];
        p.scenes[1].voiceover_path = Some("/x/v.wav".into());
        let steps = plan(&p);
        let voice = steps.iter().find(|s| s.id == "voice").unwrap();
        assert_eq!(voice.credits, (21.0f64 * 1.1).round() as u64);
        assert!(!voice.done);
        let ids = vec!["voice".to_string(), "cut".to_string()];
        let cmds = commands_for(&p, &ids);
        assert_eq!(
            cmds.iter().map(|c| c.command.as_str()).collect::<Vec<_>>(),
            ["crstudio.synthesizeVoiceover", "crstudio.assembleTimeline", "crstudio.createShortsCut"]
        );
        assert!(steps.iter().any(|s| s.id == "export" && s.heavy));
    }

    #[test]
    fn the_board_flags_old_maps_duplicates_and_missing_voice() {
        let mut p = CrStudioPipeline { visual_concept: "atlas".into(), ..Default::default() };
        let mut a = scene(0, "stock_photo", None);
        a.selected_visual = Some("/x/scene_01_map.png".into());
        a.voiceover_path = Some("/x/a.wav".into());
        let mut b = scene(1, "stock_photo", None);
        b.selected_visual = Some("/x/p.jpg".into());
        let mut c = scene(2, "stock_photo", None);
        c.selected_visual = Some("/x/p.jpg".into());
        c.voiceover_path = Some("/x/c.wav".into());
        p.scenes = vec![a, b, c];
        let kinds: Vec<(usize, String)> = scene_issues(&p).into_iter().map(|i| (i.scene, i.kind)).collect();
        assert!(kinds.contains(&(0, "old_map".into())));
        assert!(kinds.contains(&(1, "duplicate".into())) && kinds.contains(&(2, "duplicate".into())));
        assert!(kinds.contains(&(1, "no_voice".into())));
        assert!(!kinds.iter().any(|(s, k)| *s == 2 && k == "no_voice"));
    }
}
