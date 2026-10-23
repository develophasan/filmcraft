//! Agent 8 — the Packager: YouTube chapters (YouTube's rules: first at 00:00, at least three,
//! each at least 10 s), the description with sources and image credits, the Turkish subtitle
//! file timed to the scenes, the licence inventory and the thumbnail.

use std::path::Path;

use super::{CrStudioPipeline, render, write_attribution_sidecar};

/// The packager's output, built outside the lock and then applied.
#[derive(Clone, Debug, Default)]
pub struct Package {
    pub chapters: String,
    pub credits: String,
    pub thumbnail_path: Option<String>,
    pub subtitle_paths: Vec<String>,
    pub license_report: Option<String>,
}

impl Package {
    pub fn apply(self, p: &mut CrStudioPipeline) {
        p.youtube_chapters_text = self.chapters;
        p.credits_text = self.credits;
        if self.thumbnail_path.is_some() {
            p.thumbnail_path = self.thumbnail_path;
        }
        p.subtitle_paths = self.subtitle_paths;
        p.copyright_shield_path = self.license_report;
        p.push_log("Paketleyici", "ok", "Bölümler, açıklama kredileri, altyazı ve kapak hazır");
    }
}

pub fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

/// Chapters from the scenes' chapter names, merged until each is at least 10 s.
pub fn chapters(p: &CrStudioPipeline) -> Vec<(f64, String)> {
    if p.format.is_vertical() {
        return Vec::new();
    }
    let mut raw: Vec<(f64, String)> = Vec::new();
    let mut t = 0.0;
    for s in &p.scenes {
        let name = if s.chapter.trim().is_empty() { s.title.trim().to_string() } else { s.chapter.trim().to_string() };
        if raw.last().is_none_or(|(_, n)| *n != name) {
            raw.push((t, name));
        }
        t += s.effective_duration();
    }
    let total = t;
    let mut out: Vec<(f64, String)> = Vec::new();
    for (i, (start, name)) in raw.iter().enumerate() {
        let end = raw.get(i + 1).map_or(total, |n| n.0);
        if let Some(last) = out.last() {
            let last_end = *start;
            if last_end - last.0 < 10.0 {
                // the previous chapter is too short: it absorbs this one
                continue;
            }
        }
        if end - start >= 0.0 {
            out.push((*start, name.clone()));
        }
    }
    // the final chapter must be ≥ 10 s too
    while out.len() >= 2 && out.last().is_some_and(|l| total - l.0 < 10.0) {
        out.pop();
    }
    if let Some(first) = out.first_mut() {
        first.0 = 0.0;
    }
    if out.len() < 3 { Vec::new() } else { out }
}

pub fn chapters_text(p: &CrStudioPipeline) -> String {
    chapters(p).iter().map(|(t, n)| format!("{} {n}", mmss(*t))).collect::<Vec<_>>().join("\n")
}

/// Sources and visual credits for the description.
pub fn credits(p: &CrStudioPipeline) -> String {
    let mut out = String::new();
    if !p.research.sources.is_empty() {
        out.push_str("Kaynaklar:\n");
        for s in p.research.sources.iter().take(20) {
            out.push_str(&format!("• {} — {}\n", s.title, s.url));
        }
        out.push('\n');
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut lines = Vec::new();
    for s in &p.scenes {
        let Some(sel) = &s.selected_visual else { continue };
        if let Some(c) = s.visual_candidates.iter().find(|c| &c.local_path == sel)
            && (c.kind == "wikimedia_commons" || c.kind == "openverse")
            && seen.insert(c.local_path.clone())
        {
            let author = if c.author.is_empty() { "bilinmiyor".to_string() } else { c.author.clone() };
            lines.push(format!("• \"{}\" — {author} — {} — {}", c.title, c.license, if c.page_url.is_empty() { &c.source_url } else { &c.page_url }));
        }
    }
    if !lines.is_empty() {
        out.push_str("Görsel kredileri:\n");
        out.push_str(&lines.join("\n"));
        out.push_str("\n\n");
    }
    let shows = |kind: &str| {
        p.scenes.iter().any(|s| s.selected_visual.as_deref().is_some_and(|sel| s.visual_candidates.iter().any(|c| c.local_path == sel && c.kind == kind)))
    };
    if shows("blender") {
        out.push_str(&format!("Haritalar: {}\n", super::blender::IMAGERY_CREDIT));
    }
    if shows("animated_map") {
        out.push_str("Harita verisi: Natural Earth (kamu malı).\n");
    }
    out.push_str(&music_credits(p));
    out
}

/// The music line(s): each library track with the credit its licence asks for (from its
/// `.attribution` sidecar), or the synthesized score.
pub fn music_credits(p: &CrStudioPipeline) -> String {
    if p.bgm_style == "files" {
        let lines: Vec<String> = super::audio::music_files(&p.project_dir)
            .iter()
            .filter_map(|f| {
                let side = std::fs::read_to_string(format!("{f}.attribution")).ok()?;
                let field = |k: &str| side.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()));
                let license = field("license:")?;
                // the licence line often carries the full credit already ("…" Kevin MacLeod (incompetech.com), Licensed under …)
                Some(match license.split_once(" — ") {
                    Some((_, credit)) => format!("• {credit}"),
                    None => format!("• \"{}\" — {} — {license}", field("title:").unwrap_or_default(), field("author:").unwrap_or_default()),
                })
            })
            .collect();
        if !lines.is_empty() {
            return format!("Müzik:\n{}\n", lines.join("\n"));
        }
    }
    if p.bgm_style == "none" { String::new() } else { "Müzik: CrStudio ile özgün olarak üretildi.\n".to_string() }
}

/// The full YouTube description: the writer's text, chapters, credits, hashtags.
pub fn full_description(p: &CrStudioPipeline) -> String {
    let mut d = p.social_meta.description.trim().to_string();
    let ch = if p.youtube_chapters_text.is_empty() { chapters_text(p) } else { p.youtube_chapters_text.clone() };
    if !ch.is_empty() {
        d.push_str("\n\nBölümler:\n");
        d.push_str(&ch);
    }
    let cr = if p.credits_text.is_empty() { credits(p) } else { p.credits_text.clone() };
    d.push_str("\n\n");
    d.push_str(&cr);
    let tags: Vec<String> = p.social_meta.tags.iter().take(3).map(|t| format!("#{}", t.split_whitespace().collect::<String>())).collect();
    if !tags.is_empty() {
        d.push('\n');
        d.push_str(&tags.join(" "));
    }
    d.chars().take(5000).collect()
}

fn srt_time(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

/// Turkish subtitles timed to the scenes (chunks of up to 7 words, 4 in vertical formats).
pub fn srt(p: &CrStudioPipeline) -> String {
    let mut out = String::new();
    let mut n = 1usize;
    let mut cursor = 0.0;
    let per = if p.format.is_vertical() { 4 } else { 7 };
    for s in &p.scenes {
        let dur = s.effective_duration();
        let words: Vec<&str> = s.narration_text.split_whitespace().collect();
        let chunks: Vec<String> = words.chunks(per).map(|c| c.join(" ")).collect();
        let total_chars: usize = chunks.iter().map(|c| c.chars().count().max(1)).sum::<usize>().max(1);
        let mut t = cursor;
        for c in &chunks {
            let d = dur * c.chars().count().max(1) as f64 / total_chars as f64;
            out.push_str(&format!("{n}\n{} --> {}\n{c}\n\n", srt_time(t), srt_time((t + d - 0.04).max(t + 0.2))));
            n += 1;
            t += d;
        }
        cursor += dur;
    }
    out
}

/// Build everything (files are written into the project folder).
pub fn build(p: &CrStudioPipeline) -> Package {
    let mut pkg = Package { chapters: chapters_text(p), credits: credits(p), ..Default::default() };
    if p.project_dir.is_empty() || cfg!(target_arch = "wasm32") {
        return pkg;
    }
    let dir = Path::new(&p.project_dir);
    let _ = std::fs::create_dir_all(dir.join("yayin"));
    if let Ok(png) = render::render_youtube_thumbnail_png(1280, 720, p) {
        let path = dir.join("yayin").join("kapak_1280x720.png");
        if filmcraft_format::atomic_write(&path, &png).is_ok() {
            write_attribution_sidecar(&path, &p.title, "CrStudio", "original work", "Özgün (kendi)", "Thumbnail");
            pkg.thumbnail_path = Some(path.to_string_lossy().into_owned());
        }
    }
    let srt_path = dir.join("yayin").join("altyazi_tr.srt");
    if filmcraft_format::atomic_write(&srt_path, srt(p).as_bytes()).is_ok() {
        pkg.subtitle_paths.push(srt_path.to_string_lossy().into_owned());
    }
    let desc_path = dir.join("yayin").join("aciklama.txt");
    let mut tmp = p.clone();
    tmp.youtube_chapters_text = pkg.chapters.clone();
    tmp.credits_text = pkg.credits.clone();
    let _ = filmcraft_format::atomic_write(&desc_path, full_description(&tmp).as_bytes());
    let mut report = format!("CrStudio lisans envanteri — {}\n\n", p.title);
    for s in &p.scenes {
        let sel = s.selected_visual.as_deref().unwrap_or("-");
        let c = s.visual_candidates.iter().find(|c| c.local_path == sel);
        report.push_str(&format!(
            "Sahne {:02} · {}\n  görsel: {}\n  lisans: {}\n  yazar: {}\n  kaynak: {}\n  ses: {}\n\n",
            s.index + 1,
            s.title,
            sel,
            c.map_or("-", |c| c.license.as_str()),
            c.map_or("-", |c| c.author.as_str()),
            c.map_or("-", |c| if c.page_url.is_empty() { c.source_url.as_str() } else { c.page_url.as_str() }),
            if s.voiceover_source.is_empty() { "-" } else { &s.voiceover_source },
        ));
    }
    report.push_str("Not: Bu dosya bir envanterdir, hukuki güvence değildir. CC BY / CC BY-SA görseller için krediler açıklamada yer almalıdır.\n");
    let rp = dir.join("yayin").join("lisans_envanteri.txt");
    if filmcraft_format::atomic_write(&rp, report.as_bytes()).is_ok() {
        pkg.license_report = Some(rp.to_string_lossy().into_owned());
    }
    pkg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crstudio::SceneSpec;

    fn scene(i: usize, chapter: &str, secs: f64) -> SceneSpec {
        SceneSpec {
            index: i,
            chapter: chapter.into(),
            title: format!("S{i}"),
            duration_seconds: secs,
            narration_text: "bir iki üç".into(),
            ..Default::default()
        }
    }

    #[test]
    fn chapters_follow_youtube_rules() {
        let p = CrStudioPipeline {
            scenes: vec![
                scene(0, "Kanca", 5.0),
                scene(1, "Giriş", 12.0),
                scene(2, "Mağara", 8.0),
                scene(3, "Mağara", 8.0),
                scene(4, "Uyanış", 15.0),
                scene(5, "Son", 4.0),
            ],
            ..Default::default()
        };
        let ch = chapters(&p);
        assert!(ch.len() >= 3, "{ch:?}");
        assert_eq!(ch[0].0, 0.0);
        let total: f64 = p.scenes.iter().map(|s| s.effective_duration()).sum();
        for (i, (t, _)) in ch.iter().enumerate() {
            let end = ch.get(i + 1).map_or(total, |n| n.0);
            assert!(end - t >= 10.0, "chapter {i} too short: {ch:?}");
        }
        let shorts = CrStudioPipeline { format: crate::crstudio::VideoFormatPreset::YoutubeShorts, ..p };
        assert!(chapters(&shorts).is_empty());
    }

    #[test]
    fn srt_is_well_formed() {
        let p = CrStudioPipeline { scenes: vec![scene(0, "a", 4.0)], ..Default::default() };
        let s = srt(&p);
        assert!(s.starts_with("1\n00:00:00,000 --> "));
        assert_eq!(mmss(3725.0), "1:02:05");
    }

    #[test]
    fn the_library_music_is_credited_as_its_licence_asks() {
        let dir = crate::temp_dir().join("crs_music_credit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("muzik")).unwrap();
        std::fs::write(dir.join("muzik").join("01-Babylon.mp3"), b"x").unwrap();
        std::fs::write(
            dir.join("muzik").join("01-Babylon.mp3.attribution"),
            "asset:        01-Babylon.mp3\ntitle:        Babylon\nauthor:       Kevin MacLeod (incompetech.com)\nlicense:      CC BY 4.0 — \"Babylon\" Kevin MacLeod (incompetech.com), Licensed under Creative Commons: By Attribution 4.0 License\n",
        )
        .unwrap();
        let p = CrStudioPipeline { bgm_style: "files".into(), project_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
        let m = music_credits(&p);
        assert!(m.contains("\"Babylon\" Kevin MacLeod (incompetech.com), Licensed under Creative Commons"), "{m}");
        assert!(!credits(&p).contains("özgün olarak üretildi"));
        let synth = CrStudioPipeline { bgm_style: "historical_suspense".into(), ..Default::default() };
        assert!(music_credits(&synth).contains("özgün"));
    }
}
