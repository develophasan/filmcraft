//! Agent 6 — Sound & FX: pacing from the real narration, the procedurally composed score
//! (original work, no third-party music) and the transition sound effects.

use std::path::Path;

use super::{CrStudioPipeline, SceneSpec, write_attribution_sidecar};

/// What the sound designer decided, computed outside the pipeline lock and then applied.
pub struct AudioDesign {
    pub scenes: Vec<SceneSpec>,
    pub bgm_style: String,
    pub bgm_path: Option<String>,
    pub summary: String,
}

impl AudioDesign {
    pub fn apply(self, p: &mut CrStudioPipeline) {
        // Keep edits the user made meanwhile; take only the fields this agent owns.
        for (dst, src) in p.scenes.iter_mut().zip(self.scenes) {
            if dst.voiceover_duration_sec.is_none() {
                dst.duration_seconds = src.duration_seconds;
            }
            if dst.sfx_cue.is_empty() {
                dst.sfx_cue = src.sfx_cue;
            }
            dst.sync_audit_note = src.sync_audit_note;
        }
        p.bgm_style = self.bgm_style;
        p.bgm_path = self.bgm_path;
        p.fx_audit_summary = self.summary.clone();
        p.push_log("Ses & Efekt", "ok", self.summary);
    }
}

pub fn bgm_label(id: &str) -> &'static str {
    match id {
        "mystery_dark_drone" => "Karanlık gizem (drone)",
        "investigative_pulse" => "Araştırmacı nabız",
        "historical_suspense" => "Tarihî gerilim",
        "cosmic_synth" => "Kozmik keşif",
        "none" => "Müzik yok",
        "files" => "Müzik kütüphanesi (proje ▸ muzik)",
        _ => "Karanlık gizem (drone)",
    }
}

pub fn sfx_label(id: &str) -> &'static str {
    match id {
        "low_boom" => "Derin darbe",
        "riser" => "Gerilim yükselişi",
        "map_ping" => "Harita sinyali",
        "archive_shutter" => "Arşiv deklanşörü",
        "none" => "Yok",
        _ => "Hava geçişi (whoosh)",
    }
}

/// The music library of a project: audio files in `<project>/muzik`, else in the channel's shared
/// library `~/.crstudio/muzik`, in file-name order (`01-…`, `02-…`). Each needs its licence sidecar
/// (`<file>.attribution`): only licensed tracks go there (CC0 / CC BY, credited in the description).
pub fn music_files(project_dir: &str) -> Vec<String> {
    let own = music_in(&Path::new(project_dir).join("muzik"));
    if own.is_empty() { music_in(&super::persist::config_dir().join("muzik")) } else { own }
}

/// Whether a music library exists (the project's own, else the shared `~/.crstudio/muzik`).
pub fn has_music_library(project_dir: &str) -> bool {
    !music_files(project_dir).is_empty()
}

fn music_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<String> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp3" | "wav" | "m4a" | "aac" | "flac")))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    v.sort();
    v.truncate(16);
    v
}

/// Silence after a scene's narration before the next scene (seconds): a breath between scenes,
/// a longer one where a new chapter begins.
pub fn scene_pause(cur: &SceneSpec, next: Option<&SceneSpec>) -> f64 {
    match next {
        None => 0.0,
        Some(n) if !n.chapter.is_empty() && n.chapter != cur.chapter => 1.8,
        Some(_) => 0.7,
    }
}

/// Pace every scene from its narration and render the score for the whole video.
pub fn design(p: &CrStudioPipeline) -> AudioDesign {
    let mut scenes = p.scenes.clone();
    let bgm_style = if !p.bgm_style.trim().is_empty() {
        p.bgm_style.clone()
    } else if has_music_library(&p.project_dir) {
        // real, licensed music beats the synthesized score whenever there is some
        "files".to_string()
    } else {
        p.tone.default_bgm().to_string()
    };
    let mut cuts = Vec::new();
    let mut cursor = 0.0_f64;
    let n = scenes.len();
    for (i, sc) in scenes.iter_mut().enumerate() {
        if sc.voiceover_duration_sec.is_none() {
            sc.duration_seconds = (sc.estimated_seconds() * 10.0).round() / 10.0;
        }
        if sc.sfx_cue.is_empty() {
            // the synthesized cues sound cheap: none unless a scene asks for one on purpose
            sc.sfx_cue = "none".into();
        }
        if sc.sfx_cue == "auto" {
            sc.sfx_cue = match sc.visual_type.as_str() {
                "animated_map" => "map_ping",
                "evidence_highlight" => "archive_shutter",
                "chapter_card" => "low_boom",
                _ => "whoosh",
            }
            .into();
        }
        let d = sc.effective_duration();
        let words = sc.narration_text.split_whitespace().count();
        let wpm = (words as f64 / d.max(1.0) * 60.0).round();
        let pace = if wpm > 175.0 {
            "hızlı"
        } else if wpm < 110.0 {
            "yavaş"
        } else {
            "doğal"
        };
        sc.sync_audit_note = format!("{d:.1} sn · {wpm:.0} kelime/dk ({pace}) · SFX: {}", sfx_label(&sc.sfx_cue));
        cursor += d;
        if i + 1 < n {
            cuts.push(cursor);
        }
    }
    let total = cursor.max(8.0);
    let mut bgm_path = None;
    if !matches!(bgm_style.as_str(), "none" | "files") && !p.project_dir.is_empty() && !cfg!(target_arch = "wasm32") {
        let bytes = generate_ambient_music_wav(total + 2.0, &bgm_style, &cuts);
        let dir = Path::new(&p.project_dir).join("sfx");
        let path = dir.join(format!("score_{bgm_style}.wav"));
        if std::fs::create_dir_all(&dir).is_ok() && filmcraft_format::atomic_write(&path, &bytes).is_ok() {
            write_attribution_sidecar(&path, bgm_label(&bgm_style), "CrStudio score synthesizer", "original work", "Özgün üretim (kendi)", "Procedural score");
            bgm_path = Some(path.to_string_lossy().into_owned());
        }
    }
    let summary = format!("{n} sahne, toplam {total:.0} sn · müzik: {}", bgm_label(&bgm_style));
    AudioDesign { scenes, bgm_style, bgm_path, summary }
}

pub fn generate_sfx_wav(kind: &str) -> Vec<u8> {
    let sr = 48_000u32;
    let dur_sec = match kind {
        "low_boom" => 1.8,
        "riser" | "mystery_riser" => 1.5,
        "map_ping" => 1.1,
        "archive_shutter" => 0.65,
        _ => 0.85,
    };
    let n = (dur_sec * f64::from(sr)) as usize;
    let mut samples = Vec::with_capacity(n);
    let mut phase = 0.0_f32;
    for i in 0..n {
        let t = i as f32 / sr as f32;
        let norm = i as f32 / n.max(1) as f32;
        let noise = ((i as u32).wrapping_mul(1_664_525).wrapping_add(1_013_904_223) as f32 / u32::MAX as f32) * 2.0 - 1.0;
        let s = match kind {
            "low_boom" => {
                // Deep dark documentary sub-bass impact + eerie minor fifth tail
                let freq = 86.0 * (-t * 2.6).exp() + 34.0;
                phase += 2.0 * std::f32::consts::PI * freq / sr as f32;
                let sub = phase.sin() * 0.72 + (phase * 1.5).sin() * 0.22;
                let rumble = noise * (-t * 6.0).exp() * 0.18;
                let env = (-t * 2.1).exp() * (t * 45.0).min(1.0);
                (sub + rumble) * env * 0.48
            }
            "riser" | "mystery_riser" => {
                // Eerie metallic/harmonic suspense riser
                let freq = 85.0 + 580.0 * norm * norm;
                phase += 2.0 * std::f32::consts::PI * freq / sr as f32;
                let detuned = (phase * 1.015).sin() * 0.45 + (phase * 1.498).sin() * 0.25;
                let env = norm.powf(1.9) * ((1.0 - norm) * 16.0).min(1.0);
                (phase.sin() * 0.5 + detuned + noise * 0.15 * norm) * env * 0.30
            }
            "map_ping" => {
                // Tactical sonar ping + subtle harmonic echo
                let f1 = 1318.5; // E6
                let f2 = 1975.5; // B6
                let main_env = (-t * 6.0).exp() * (t * 90.0).min(1.0);
                let echo_env = if t > 0.28 { (-(t - 0.28) * 7.5).exp() * 0.35 } else { 0.0 };
                let env = main_env + echo_env;
                ((2.0 * std::f32::consts::PI * f1 * t).sin() * 0.72 + (2.0 * std::f32::consts::PI * f2 * t).sin() * 0.28) * env * 0.28
            }
            "archive_shutter" => {
                // Vintage archive document flash / mechanical microfiche click
                let click1 = if t < 0.04 { (-t * 90.0).exp() * noise * 0.8 } else { 0.0 };
                let click2 = if t > 0.09 && t < 0.16 { (-(t - 0.09) * 75.0).exp() * noise * 0.6 } else { 0.0 };
                let flash_tone = (2.0 * std::f32::consts::PI * 240.0 * t).sin() * (-t * 8.0).exp() * 0.25;
                (click1 + click2 + flash_tone) * 0.42
            }
            _ => {
                // Cinematic low-mid air whoosh
                let freq = 115.0 + 340.0 * (norm * std::f32::consts::PI).sin();
                phase += 2.0 * std::f32::consts::PI * freq / sr as f32;
                let env = (norm * std::f32::consts::PI).sin().powf(1.6);
                (phase.sin() * 0.68 + noise * 0.32) * env * 0.26
            }
        };
        samples.push(s);
    }
    crate::voiceover::write_wav_f32(&samples, sr)
}

/// Generate a royalty-free, multi-layered mysterious documentary ambient background score WAV
/// with deep sub-bass drones, suspense heartbeat pulse, minor harmonic pads, and tension swells at scene cuts.
pub fn generate_ambient_music_wav(duration_sec: f64, bgm_style: &str, cut_seconds: &[f64]) -> Vec<u8> {
    let sr = 24_000u32;
    let dur = duration_sec.clamp(5.0, 600.0);
    let n = (dur * f64::from(sr)) as usize;
    let mut samples = Vec::with_capacity(n);

    // Dark documentary chord progression roots (in Hz):
    // D minor (36.71 Hz) -> Bb major7 (29.14 Hz) -> G minor (49.00 Hz) -> A Phrygian/sus (27.50 Hz)
    let chord_roots: [(f32, f32, f32, f32); 4] = match bgm_style {
        "historical_suspense" => [
            (36.71, 73.42, 110.00, 174.61), // Dm
            (32.70, 65.41, 98.00, 155.56),  // Cm/Eb
            (29.14, 58.27, 87.31, 146.83),  // BbMaj7
            (27.50, 55.00, 87.31, 138.59),  // A7b9 Phrygian
        ],
        "cosmic_synth" => [
            (32.70, 65.41, 98.00, 196.00),  // C minor 9
            (43.65, 87.31, 130.81, 196.00), // Fm7
            (38.89, 77.78, 116.54, 174.61), // EbMaj7
            (36.71, 73.42, 110.00, 164.81), // Gsus4/D
        ],
        _ => [
            // `mystery_dark_drone` & `investigative_pulse` (Netflix/Vox/Johnny Harris style dark D-minor mystery)
            (36.71, 73.42, 110.00, 146.83), // D1, D2, A2, D3
            (38.89, 77.78, 116.54, 174.61), // Eb1 (Phrygian dark tension), Eb2, Bb2, F3
            (29.14, 58.27, 110.00, 164.81), // Bb0, Bb1, A2, E3 (mysterious lydian/minor clash)
            (27.50, 55.00, 82.41, 138.59),  // A0, A1, E2, C#3
        ],
    };

    let pulse_bpm = match bgm_style {
        "investigative_pulse" => 84.0_f32,
        "historical_suspense" => 60.0_f32,
        "cosmic_synth" => 66.0_f32,
        _ => 70.0_f32,
    };
    let beat_period = 60.0 / pulse_bpm;

    for i in 0..n {
        let t = i as f32 / sr as f32;
        let fade_in = (t / 2.2).clamp(0.0, 1.0);
        let fade_out = ((dur as f32 - t) / 2.8).clamp(0.0, 1.0);

        // Smooth 8-second chord morphing
        let bar_pos = t / 8.0;
        let c_idx = (bar_pos.floor() as usize) % 4;
        let next_idx = (c_idx + 1) % 4;
        let frac = bar_pos.fract();
        // Smoothstep crossfade in the last 1.5s of each 8s bar
        let blend = ((frac - 0.80) / 0.20).clamp(0.0, 1.0);
        let blend = blend * blend * (3.0 - 2.0 * blend);

        let c0 = chord_roots[c_idx];
        let c1 = chord_roots[next_idx];
        let f_sub = c0.0 * (1.0 - blend) + c1.0 * blend;
        let f_bass = c0.1 * (1.0 - blend) + c1.1 * blend;
        let f_fifth = c0.2 * (1.0 - blend) + c1.2 * blend;
        let f_color = c0.3 * (1.0 - blend) + c1.3 * blend;

        // 1. Deep mysterious sub-bass & warm bowed low drone
        let lfo_breath = 0.72 + 0.28 * (2.0 * std::f32::consts::PI * 0.14 * t).sin();
        let sub_drone = (2.0 * std::f32::consts::PI * f_sub * t).sin() * 0.42 + (2.0 * std::f32::consts::PI * f_bass * t).sin() * 0.28;

        // 2. Haunting detuned minor pad + eerie high mystery overtone
        let pad = (2.0 * std::f32::consts::PI * f_fifth * t).sin() * 0.16
            + (2.0 * std::f32::consts::PI * (f_color * 1.003) * t).sin() * 0.11
            + (2.0 * std::f32::consts::PI * (f_color * 1.997) * t).sin() * 0.05 * (t * 0.35).sin().abs();

        // 3. Suspense heartbeat / investigative sub-pulse ("lub-dub" documentary tension)
        let beat_phase = (t % beat_period) / beat_period;
        let pulse1 = (-beat_phase * 11.0).exp() * (beat_phase * 55.0).min(1.0);
        let pulse2 = if beat_phase > 0.28 { (-(beat_phase - 0.28) * 14.0).exp() * ((beat_phase - 0.28) * 60.0).min(1.0) * 0.55 } else { 0.0 };
        let pulse_env = pulse1 + pulse2;
        let pulse_weight = if bgm_style == "investigative_pulse" { 0.36 } else { 0.24 };
        let tension_pulse = (2.0 * std::f32::consts::PI * 52.0 * t).sin() * pulse_env * pulse_weight;

        // 4. Scene-transition tension swell right around each cut on the timeline
        let mut cut_swell = 0.0_f32;
        let near = cut_seconds.partition_point(|c| (*c as f32) < t - 1.2);
        for &ct in cut_seconds.get(near..(near + 3).min(cut_seconds.len())).unwrap_or(&[]) {
            let dt = (t - ct as f32).abs();
            if dt < 1.2 {
                let w = 1.0 - (dt / 1.2);
                cut_swell = cut_swell.max(w * w);
            }
        }
        let swell_shimmer = (2.0 * std::f32::consts::PI * (f_color * 1.5) * t).sin() * cut_swell * 0.14;

        let mix = ((sub_drone + pad) * lfo_breath + tension_pulse + swell_shimmer) * fade_in * fade_out * 0.26;
        samples.push(mix.clamp(-0.95, 0.95));
    }
    crate::voiceover::write_wav_f32(&samples, sr)
}
