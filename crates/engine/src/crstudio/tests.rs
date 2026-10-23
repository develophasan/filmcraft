use serde_json::json;

use super::*;

fn project(dir_name: &str) -> Session {
    let mut s = Session::default();
    let dir = crate::temp_dir().join(dir_name);
    let _ = std::fs::remove_dir_all(&dir);
    s.execute(
        "crstudio.new",
        json!({"title": "Yedi Uyurlar Efsanesi", "format": "youtube_video", "tone": "mitoloji", "targetDurationSec": 60.0, "projectDir": dir.to_string_lossy(), "channelName": "Efsaneler"}),
    )
    .unwrap();
    s
}

/// A scripted project with procedural visuals (no network) and short synthetic narration files.
fn scripted(dir_name: &str) -> Session {
    let mut s = project(dir_name);
    let v = json!({
        "titleOptions": ["Yedi Uyurlar: 309 Yıllık Uyku"],
        "hookLine": "Yedi genç bir mağarada uyudu ve 309 yıl sonra uyandı.",
        "description": "Yedi Uyurlar efsanesini kaynaklarıyla anlatıyoruz.",
        "tags": ["yedi uyurlar", "efsane", "efes", "mitoloji", "ashab-ı kehf"],
        "thumbnailText": "309 YIL UYKU",
        "pinnedComment": "Sizce mağara Efes'te mi, Tarsus'ta mı?",
        "scenes": [
            {"chapter": "Kanca", "title": "Uyku", "narrationText": "Yedi genç bir mağarada uyudu. Uyandıklarında dünya değişmişti, ama nasıl?", "visualType": "chapter_card", "overlays": ["info_box"], "infoText": "Rivayet birçok kültürde anlatılır"},
            {"chapter": "Efes", "title": "Efes", "narrationText": "Rivayete göre olay Efes yakınlarındaki bir mağarada geçti ve bu mağara bugün de ziyaret ediliyor.", "visualType": "animated_map", "mapPlace": 0, "factKind": "rivayet"},
            {"chapter": "Efes", "title": "İmparator", "narrationText": "Gençler zulümden kaçarak dağa sığındılar ve yorgunlukla derin bir uykuya daldılar.", "visualType": "stock_photo", "overlays": ["speech_bubble"], "infoText": "Rabbimiz göklerin ve yerin Rabbidir"},
            {"chapter": "Uyanış", "title": "Uyanış", "narrationText": "Uyandıklarında yanlarındaki eski paralar onları ele verdi; şehir artık bambaşkaydı.", "visualType": "stock_photo", "sfxCue": "riser"},
            {"chapter": "Son", "title": "Soru", "narrationText": "Sizce bu hikâyenin gerçek bir kökeni var mı? Yorumlarda yazın ve abone olmayı unutmayın.", "visualType": "chapter_card"}
        ]
    });
    s.crstudio.pipeline.research.places = vec![Place { name: "Efes".into(), lat: 37.94, lon: 27.34, note: "Mağara".into() }];
    assert!(agents::apply_script(&mut s.crstudio.pipeline, &v));
    let (w, h) = s.crstudio.pipeline.format.dimensions();
    let dir = s.crstudio.pipeline.project_dir.clone();
    for sc in &mut s.crstudio.pipeline.scenes {
        let png = render::render_card_png(w, h, sc, "", None).unwrap();
        let path = Path::new(&dir).join(format!("card_{}.png", sc.index));
        std::fs::write(&path, png).unwrap();
        sc.visual_candidates = vec![VisualCandidate {
            local_path: path.to_string_lossy().into_owned(),
            kind: "chapter_card".into(),
            license: "Özgün çizim (kendi)".into(),
            width: w,
            height: h,
            score: 1.0,
            ..Default::default()
        }];
        sc.selected_visual = Some(path.to_string_lossy().into_owned());
        let vo = Path::new(&dir).join(format!("vo_{}.wav", sc.index));
        let samples: Vec<f32> = (0..24_000 * 3).map(|i| (i as f32 * 0.03).sin() * 0.2).collect();
        std::fs::write(&vo, wav::encode(&samples, 24_000)).unwrap();
        sc.voiceover_raw_path = Some(vo.to_string_lossy().into_owned());
        sc.voiceover_path = sc.voiceover_raw_path.clone();
        sc.voiceover_duration_sec = Some(3.0);
        sc.voiceover_engine = "record".into();
    }
    s
}

#[test]
fn new_project_creates_folder_and_saves() {
    let mut s = project("crstudio_t_new");
    assert_eq!(s.crstudio.pipeline.tone, DocTone::MythLegend);
    assert_eq!(s.crstudio.pipeline.agents.len(), 9);
    assert!(Path::new(&s.crstudio.pipeline.project_dir).is_dir());
    s.crstudio.last_save_check = 0.0;
    autosave(&mut s);
    assert!(Path::new(&s.crstudio.pipeline.project_dir).join(persist::STATE_FILE).is_file());
    assert!(s.execute("crstudio.new", json!({"title": "  "})).is_err());
}

#[test]
fn hostile_scene_params_are_refused() {
    let mut s = scripted("crstudio_t_hostile");
    assert!(s.execute("crstudio.updateScene", json!({"sceneIndex": 99})).is_err());
    s.execute("crstudio.updateScene", json!({"sceneIndex": 0, "durationSeconds": f64::NAN})).ok();
    assert!(s.crstudio.pipeline.scenes[0].duration_seconds.is_finite());
    assert!(s.execute("crstudio.updateScene", json!({"sceneIndex": 0, "selectedVisual": "/etc/passwd"})).is_err());
    assert!(s.execute("crstudio.generateMap", json!({"sceneIndex": 1, "lat": 1e308, "lon": -1e308})).is_ok());
}

#[test]
fn editing_narration_marks_recording_stale() {
    let mut s = scripted("crstudio_t_stale");
    s.execute("crstudio.updateScene", json!({"sceneIndex": 2, "narrationText": "Yeni metin."})).unwrap();
    let sc = &s.crstudio.pipeline.scenes[2];
    assert!(sc.voiceover_stale);
    assert_eq!(sc.text_origin, "user");
}

#[test]
fn assemble_builds_all_layers_and_shorts() {
    let mut s = scripted("crstudio_t_assemble");
    let r = s.execute("crstudio.assembleTimeline", json!({})).unwrap();
    assert_eq!(r["scenesPlaced"], 5);
    assert!(r["overlays"].as_u64().unwrap() >= 2, "{r}");
    let seq = s.active_sequence().unwrap();
    assert!(seq.video_tracks.len() >= 4);
    assert!(!seq.video_tracks[0].items.is_empty());
    assert!(!seq.video_tracks[2].items.is_empty(), "YouTuber overlays on V3");
    assert!(!seq.video_tracks[3].items.is_empty(), "channel kit on V4");
    assert_eq!(seq.audio_tracks[0].items.len(), 5, "narration on A1");
    assert!(!seq.audio_tracks[2].items.is_empty(), "score on A3");
    // running it again does not re-import the same files
    let media = |s: &Session| {
        let mut ids = Vec::new();
        s.project.root.all_items(&mut ids);
        ids.iter().filter(|i| matches!(s.project.item(**i).map(|it| &it.kind), Some(filmcraft_project::ItemKind::Media(_)))).count()
    };
    let before = media(&s);
    s.execute("crstudio.assembleTimeline", json!({})).unwrap();
    assert!(media(&s) <= before + 1, "only the re-rendered score is imported again");
    let sh = s.execute("crstudio.createShortsCut", json!({})).unwrap();
    assert_eq!(sh["width"], 1080);
    assert_eq!(sh["height"], 1920);
    let r = s.crstudio.pipeline.director.clone().unwrap();
    assert!(r.score > 0.0);
}

#[test]
fn director_flags_templates_and_missing_voice() {
    let mut s = project("crstudio_t_director");
    agents::skeleton_storyboard(&mut s.crstudio.pipeline);
    let r = s.execute("crstudio.review", json!({"ai": false})).unwrap();
    assert!(r["blocking"].as_bool().unwrap());
    assert!(r["issues"].as_array().unwrap().iter().any(|i| i["category"] == "senaryo"));
}

#[test]
fn state_round_trips_through_disk() {
    let s = scripted("crstudio_t_roundtrip");
    persist::save(&s.crstudio.pipeline).unwrap();
    let back = persist::load(Path::new(&s.crstudio.pipeline.project_dir)).unwrap();
    assert_eq!(back.scenes.len(), 5);
    assert_eq!(back.scenes[1].map_spec.as_ref().unwrap().location_name, "EFES");
}

#[test]
fn stage_navigation_sticks_while_a_job_runs() {
    let mut s = project("crs_nav_job");
    let gate = Arc::new(AtomicBool::new(false));
    let g2 = Arc::clone(&gate);
    spawn_job(&mut s, "Deneme", move |shared, _| {
        while !g2.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // the job itself moves on to the next stage at the end
        shared.lock().unwrap_or_else(PoisonError::into_inner).stage = PipelineStage::ReviewScenes;
    })
    .unwrap();
    s.crstudio.pipeline.stage = PipelineStage::Voiceover;
    poll(&mut s);
    assert_eq!(s.crstudio.pipeline.stage, PipelineStage::Voiceover, "the person's navigation is kept");
    s.crstudio.pipeline.stage = PipelineStage::Publish;
    poll(&mut s);
    assert_eq!(s.crstudio.pipeline.stage, PipelineStage::Publish);
    gate.store(true, Ordering::Relaxed);
    for _ in 0..400 {
        if !poll(&mut s) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(s.crstudio.pipeline.stage, PipelineStage::ReviewScenes, "a stage the job moves to wins");
}

#[test]
fn a_rewritten_narration_file_is_imported_again() {
    let mut s = project("crs_reimport");
    let path = crate::temp_dir().join("crs_reimport_vo.wav");
    let ps = path.to_string_lossy().into_owned();
    std::fs::write(&path, wav::encode(&vec![0.1f32; 24_000], 24_000)).unwrap();
    let bin = assemble::ensure_named_bin(&mut s, "02 Seslendirme").unwrap();
    let a = assemble::import_path(&mut s, &ps, bin).unwrap();
    assert_eq!(assemble::import_path(&mut s, &ps, bin).unwrap(), a, "the same file is reused");
    // a new, longer take under the same name
    std::fs::write(&path, wav::encode(&vec![0.1f32; 24_000 * 3], 24_000)).unwrap();
    let b = assemble::import_path(&mut s, &ps, bin).unwrap();
    assert_ne!(a, b);
    let dur = |s: &Session, id| match &s.project.item(id).unwrap().kind {
        filmcraft_project::ItemKind::Media(m) => m.duration().seconds(),
        _ => 0.0,
    };
    assert!((dur(&s, b) - 3.0).abs() < 0.05, "{}", dur(&s, b));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scenes_breathe_and_the_music_library_is_read_in_order() {
    let a = SceneSpec { chapter: "Kaynak".into(), ..SceneSpec::default() };
    let b = SceneSpec { chapter: "Kaynak".into(), ..SceneSpec::default() };
    let c = SceneSpec { chapter: "Efes".into(), ..SceneSpec::default() };
    assert!((audio::scene_pause(&a, Some(&b)) - 0.7).abs() < 1e-9);
    assert!((audio::scene_pause(&b, Some(&c)) - 1.8).abs() < 1e-9);
    assert_eq!(audio::scene_pause(&c, None), 0.0);
    let dir = crate::temp_dir().join("crs_music_lib");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("muzik")).unwrap();
    for f in ["02-b.mp3", "01-a.wav", "notlar.txt", "01-a.wav.attribution"] {
        std::fs::write(dir.join("muzik").join(f), b"x").unwrap();
    }
    let v = audio::music_files(&dir.to_string_lossy());
    let names: Vec<String> = v.iter().filter_map(|p| Path::new(p).file_name().map(|f| f.to_string_lossy().into_owned())).collect();
    assert_eq!(names, ["01-a.wav", "02-b.mp3"]);
    // a project without its own folder falls back to the shared library (whatever it holds)
    let shared = audio::music_files("/yok/böyle/bir/yer");
    assert!(shared.iter().all(|p| !p.contains("crs_music_lib")));
}

#[test]
fn the_production_queue_runs_commands_in_order_and_stops_on_a_failure() {
    let mut s = project("crs_queue");
    let r = s.execute("crstudio.queue.add", json!({"command": "crstudio.plan", "label": "Plan"})).unwrap();
    assert_eq!(r["added"], 1);
    assert!(s.crstudio.pipeline.queue.is_empty(), "a synchronous command runs straight away");
    assert!(s.crstudio.pipeline.logs.iter().any(|l| l.message.contains("Başladı: Plan")));
    // a failing step clears what comes after it
    s.crstudio
        .pipeline
        .queue
        .push(production::QueuedCommand { label: "Hatalı".into(), command: "crstudio.exportAll".into(), params: json!({}), seconds: 0.0 });
    s.crstudio.pipeline.queue.push(production::QueuedCommand { label: "Sonraki".into(), command: "crstudio.plan".into(), params: json!({}), seconds: 0.0 });
    pump_queue(&mut s);
    assert!(s.crstudio.pipeline.queue.is_empty());
    assert!(s.crstudio.pipeline.logs.iter().any(|l| l.message.contains("hat durdu")));
    assert!(!s.crstudio.pipeline.logs.iter().any(|l| l.message.contains("Başladı: Sonraki")));
    // only CrStudio and export commands may be queued
    let r = s.execute("crstudio.queue.add", json!({"command": "file.saveAs", "params": {"path": "/tmp/x"}})).unwrap();
    assert_eq!(r["added"], 0);
}
