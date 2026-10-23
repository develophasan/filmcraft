//! CrStudio — Türkçe, yapay zekâ destekli YouTube belgesel/efsane stüdyosu.
//!
//! The user gives a topic and details; nine agents work in stages the user watches and steers:
//! 1. **Araştırmacı** — web research with sources (Claude Code CLI, WebSearch/WebFetch).
//! 2. **Senarist** — hook, chapters and scene narration sized to the target duration.
//! 3. **Yönetmen** — storyboard: visual type, search query, camera, graphics, SFX, YouTuber overlays.
//! 4. **Görsel Avcısı** — openly licensed images (Wikimedia Commons, Openverse) with provenance,
//!    checked for relevance by Claude looking at the thumbnails.
//! 5. **Harita & Grafik** — maps, evidence and chapter cards.
//! 6. **Ses & Efekt** — score, SFX and pacing from the real narration durations.
//! 7. **Kurgucu** — the multi-track timeline (`assemble`), plus the vertical Shorts cut.
//! 8. **Paketleyici** — chapters, description with credits, subtitles, thumbnail.
//! 9. **Yayın Yönetmeni** — the publishability score and what to fix where ([`director`]).
//!
//! Every stage is a command (`crstudio.*`), so the UI, MCP and the control channel drive the same
//! code. The state is saved to `crstudio.json` in the project folder ([`persist`]).

pub mod agents;
pub mod assemble;
pub mod assistant;
pub mod audio;
pub mod blender;
pub mod claude;
mod commands;
pub mod director;
pub mod elevenlabs;
pub mod mapstory;
pub mod package;
pub mod persist;
pub mod production;
pub mod recclean;
pub mod render;
pub mod terrain;
pub mod turkish;
pub mod visuals;
pub mod voice;
pub mod voicecheck;
pub mod wav;
pub mod youtube;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::Session;

pub use commands::commands;

pub const MAX_LOGS: usize = 400;
pub const MAX_SCENES: usize = 64;

/// Supported formats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFormatPreset {
    YoutubeShorts,
    #[default]
    YoutubeVideo,
    InstagramReels,
    InstagramVideo,
}

impl VideoFormatPreset {
    pub const ALL: [Self; 4] = [Self::YoutubeVideo, Self::YoutubeShorts, Self::InstagramReels, Self::InstagramVideo];

    pub fn id(self) -> &'static str {
        match self {
            Self::YoutubeShorts => "youtube_shorts",
            Self::YoutubeVideo => "youtube_video",
            Self::InstagramReels => "instagram_reels",
            Self::InstagramVideo => "instagram_video",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "youtube_shorts" | "youtubeshorts" | "shorts" | "yt_shorts" => Some(Self::YoutubeShorts),
            "youtube_video" | "youtubevideo" | "youtube" | "yt_video" | "16_9" | "uzun" => Some(Self::YoutubeVideo),
            "instagram_reels" | "instagram_shorts" | "reels" | "ig_reels" => Some(Self::InstagramReels),
            "instagram_video" | "instagram_post" | "ig_video" | "ig_post" | "4_5" => Some(Self::InstagramVideo),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::YoutubeShorts => "YouTube Shorts",
            Self::YoutubeVideo => "YouTube Uzun Video",
            Self::InstagramReels => "Instagram Reels",
            Self::InstagramVideo => "Instagram Gönderi",
        }
    }

    pub fn aspect_label(self) -> &'static str {
        match self {
            Self::YoutubeShorts | Self::InstagramReels => "9:16 · 1080×1920",
            Self::YoutubeVideo => "16:9 · 1920×1080",
            Self::InstagramVideo => "4:5 · 1080×1350",
        }
    }

    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::YoutubeShorts | Self::InstagramReels => (1080, 1920),
            Self::YoutubeVideo => (1920, 1080),
            Self::InstagramVideo => (1080, 1350),
        }
    }

    pub fn default_duration_sec(self) -> f64 {
        match self {
            Self::YoutubeShorts | Self::InstagramReels => 50.0,
            Self::YoutubeVideo => 480.0,
            Self::InstagramVideo => 60.0,
        }
    }

    /// Longest duration the platform accepts for this format (seconds).
    pub fn max_duration_sec(self) -> f64 {
        match self {
            Self::YoutubeShorts => 180.0,
            Self::InstagramReels => 180.0,
            Self::InstagramVideo => 600.0,
            Self::YoutubeVideo => 3600.0,
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Self::YoutubeShorts | Self::InstagramReels | Self::InstagramVideo)
    }
}

/// Narrative tone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocTone {
    #[default]
    MythLegend,
    HistoryMystery,
    InvestigativeVox,
    ScienceTech,
    ViralFast,
}

impl DocTone {
    pub const ALL: [Self; 5] = [Self::MythLegend, Self::HistoryMystery, Self::InvestigativeVox, Self::ScienceTech, Self::ViralFast];

    pub fn id(self) -> &'static str {
        match self {
            Self::MythLegend => "myth_legend",
            Self::InvestigativeVox => "investigative_vox",
            Self::HistoryMystery => "history_mystery",
            Self::ScienceTech => "science_tech",
            Self::ViralFast => "viral_fast",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "myth_legend" | "mitoloji" | "efsane" | "myth" | "legend" => Some(Self::MythLegend),
            "investigative_vox" | "vox" | "documentary" | "belgesel" | "arastirma" => Some(Self::InvestigativeVox),
            "history_mystery" | "history" | "mystery" | "tarih" | "gizem" => Some(Self::HistoryMystery),
            "science_tech" | "science" | "tech" | "bilim" | "teknoloji" => Some(Self::ScienceTech),
            "viral_fast" | "viral" | "fast" | "shorts" => Some(Self::ViralFast),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MythLegend => "Mitoloji & Efsane",
            Self::HistoryMystery => "Tarih & Gizem",
            Self::InvestigativeVox => "Araştırmacı Belgesel",
            Self::ScienceTech => "Bilim & Teknoloji",
            Self::ViralFast => "Hızlı & Viral",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::MythLegend => "Hikâye anlatıcısı dili, rivayet/belge ayrımı, minyatür ve gravür ağırlıklı görseller",
            Self::HistoryMystery => "Sinematik arşiv, kronoloji ve gerilim",
            Self::InvestigativeVox => "Harita, kanıt ve veri odaklı anlatım",
            Self::ScienceTech => "Açık, analitik, grafik destekli",
            Self::ViralFast => "Kısa cümleler, hızlı kesmeler, yüksek tempo",
        }
    }

    /// Default background score for the tone.
    pub fn default_bgm(self) -> &'static str {
        match self {
            Self::MythLegend => "historical_suspense",
            Self::HistoryMystery => "mystery_dark_drone",
            Self::InvestigativeVox | Self::ViralFast => "investigative_pulse",
            Self::ScienceTech => "cosmic_synth",
        }
    }
}

/// Workflow stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    #[default]
    Setup,
    RunningAgents,
    ReviewScenes,
    Voiceover,
    InStudio,
    Publish,
}

impl PipelineStage {
    pub const ALL: [Self; 6] = [Self::Setup, Self::RunningAgents, Self::ReviewScenes, Self::Voiceover, Self::InStudio, Self::Publish];

    pub fn label(self) -> &'static str {
        match self {
            Self::Setup => "Konu",
            Self::RunningAgents => "Ajanlar",
            Self::ReviewScenes => "Sahneler",
            Self::Voiceover => "Seslendirme",
            Self::InStudio => "Kurgu",
            Self::Publish => "Yayın",
        }
    }

    pub fn number(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }
}

/// Agent status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Idle,
    Running,
    Done,
    Error,
    Skipped,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentInfo {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub role: String,
    pub status: AgentStatus,
    pub summary: String,
    /// The agent's live activity lines (latest last).
    pub activity: Vec<String>,
}

/// Map specification for map scenes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MapSpec {
    pub location_name: String,
    pub lat: f64,
    pub lon: f64,
    pub from_lat: Option<f64>,
    pub from_lon: Option<f64>,
    pub zoom: f64,
    pub subtitle: String,
    pub accent_hex: String,
    /// More pins (name, lat, lon), e.g. the several places a legend is told about.
    pub extra_pins: Vec<(String, f64, f64)>,
}

impl Default for MapSpec {
    fn default() -> Self {
        Self {
            location_name: "TÜRKİYE".into(),
            lat: 39.0,
            lon: 35.0,
            from_lat: None,
            from_lon: None,
            zoom: 2.5,
            subtitle: String::new(),
            accent_hex: "#ef4444".into(),
            extra_pins: Vec::new(),
        }
    }
}

impl MapSpec {
    /// Clamp coordinates to valid ranges and drop non-finite values.
    pub fn sanitize(&mut self) {
        let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
        self.lat = fin(self.lat, 39.0).clamp(-85.0, 85.0);
        self.lon = fin(self.lon, 35.0).clamp(-180.0, 180.0);
        self.zoom = fin(self.zoom, 2.5).clamp(1.0, 8.0);
        self.from_lat = self.from_lat.filter(|v| v.is_finite()).map(|v| v.clamp(-85.0, 85.0));
        self.from_lon = self.from_lon.filter(|v| v.is_finite()).map(|v| v.clamp(-180.0, 180.0));
        self.extra_pins.retain(|(_, a, b)| a.is_finite() && b.is_finite());
        self.extra_pins.truncate(8);
    }
}

/// A visual candidate for a scene.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VisualCandidate {
    pub id: String,
    pub title: String,
    /// `wikimedia_commons` | `openverse` | `animated_map` | `evidence_highlight` | `infographic_chart` | `chapter_card` | `user`
    pub kind: String,
    pub local_path: String,
    /// Direct file URL.
    pub source_url: String,
    /// The page that documents the file (licence, author).
    pub page_url: String,
    pub author: String,
    pub license: String,
    pub width: u32,
    pub height: u32,
    pub score: f64,
    /// 0–100 relevance from the Visual Hunter's review (None: not reviewed).
    pub relevance: Option<f64>,
    pub relevance_note: String,
    pub notes: String,
}

/// Numbers for an infographic scene (only drawn when the script supplied them).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChartSpec {
    pub labels: Vec<String>,
    pub values: Vec<f64>,
    pub unit: String,
}

/// One scene of the storyboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SceneSpec {
    pub id: u64,
    pub index: usize,
    pub title: String,
    pub chapter: String,
    pub narration_text: String,
    pub duration_seconds: f64,
    /// `stock_photo` | `animated_map` | `evidence_highlight` | `infographic_chart` | `chapter_card`
    pub visual_type: String,
    pub visual_query: String,
    /// `zoom_in` | `zoom_out` | `pan_right` | `pan_left` | `dramatic_push`
    pub camera_motion: String,
    pub map_spec: Option<MapSpec>,
    pub chart: Option<ChartSpec>,
    pub overlay_template: String,
    pub overlay_title: String,
    pub overlay_subtitle: String,
    /// `whoosh` | `low_boom` | `riser` | `map_ping` | `archive_shutter` | `none`
    pub sfx_cue: String,
    pub video_transition: String,
    pub visual_effect: String,
    pub film_texture: String,
    pub broll_subcuts: usize,
    pub highlight_phrase: String,
    pub sync_audit_note: String,
    /// Clean-up report of the narrator's last take of this scene (grade and findings).
    pub record_report: String,
    /// The narration as directed for the voice (ElevenLabs v3 audio tags like `[pause]`,
    /// `[low voice]`). Empty: the narration text is spoken as is. Captions always use
    /// `narration_text`.
    pub voice_text: String,
    /// What the voice actually said (after the pronunciation dictionary and number reading) and
    /// when each character of it began (seconds), from the TTS alignment. Lets the map and the
    /// cut follow the real delivery instead of a word-count estimate.
    pub voice_spoken: String,
    pub voice_char_times: Vec<f32>,
    /// Listening check of the AI narration (speech-to-text against the script): the stretches that
    /// came out different, `"Gençler → Gesar"`. Empty: checked and clean, or not checked.
    pub voice_check: Vec<String>,
    /// Whether the listening check ran for the current take.
    pub voice_checked: bool,
    /// YouTuber overlays to place in this scene (`info_box`, `warning_box`, `speech_bubble`,
    /// `like_reminder`, `comment_prompt`, `source_credit`, `countdown`, `timeline_tag`, `shock_badge`).
    pub overlays: Vec<String>,
    /// Text for an info/warning box or speech bubble.
    pub info_text: String,
    /// Where the words came from: `claude` | `template` | `user`.
    pub text_origin: String,
    /// `belge` (documented) | `rivayet` (legend/tradition) | `` (neither).
    pub fact_kind: String,
    /// Indices into `research.sources`.
    pub sources: Vec<usize>,
    pub selected_visual: Option<String>,
    pub visual_candidates: Vec<VisualCandidate>,
    /// The mastered narration used on the timeline.
    pub voiceover_path: Option<String>,
    /// The take as recorded/synthesized (mastering always starts from it).
    pub voiceover_raw_path: Option<String>,
    pub voiceover_duration_sec: Option<f64>,
    pub voiceover_source: String,
    /// `record` | `file` | an engine id from [`voice::engines`].
    pub voiceover_engine: String,
    /// The narration text changed after this take was recorded.
    pub voiceover_stale: bool,
    pub approved: bool,
}

impl Default for SceneSpec {
    fn default() -> Self {
        Self {
            id: 1,
            index: 0,
            title: "Sahne 1".into(),
            chapter: String::new(),
            narration_text: String::new(),
            duration_seconds: 6.0,
            visual_type: "stock_photo".into(),
            visual_query: String::new(),
            camera_motion: "zoom_in".into(),
            map_spec: None,
            chart: None,
            overlay_template: String::new(),
            overlay_title: String::new(),
            overlay_subtitle: String::new(),
            sfx_cue: "whoosh".into(),
            video_transition: "film_dissolve".into(),
            visual_effect: "cinematic_teal_orange".into(),
            film_texture: "35mm_grain".into(),
            broll_subcuts: 1,
            highlight_phrase: String::new(),
            sync_audit_note: String::new(),
            record_report: String::new(),
            voice_text: String::new(),
            voice_spoken: String::new(),
            voice_char_times: Vec::new(),
            voice_check: Vec::new(),
            voice_checked: false,
            overlays: Vec::new(),
            info_text: String::new(),
            text_origin: "claude".into(),
            fact_kind: String::new(),
            sources: Vec::new(),
            selected_visual: None,
            visual_candidates: Vec::new(),
            voiceover_path: None,
            voiceover_raw_path: None,
            voiceover_duration_sec: None,
            voiceover_source: String::new(),
            voiceover_engine: String::new(),
            voiceover_stale: false,
            approved: false,
        }
    }
}

impl SceneSpec {
    /// Duration on the timeline: the narration's length when there is one, else the estimate.
    pub fn effective_duration(&self) -> f64 {
        let est = if self.duration_seconds.is_finite() { self.duration_seconds } else { 6.0 };
        self.voiceover_duration_sec.filter(|d| d.is_finite() && *d >= 0.5).map(|d| d + 0.35).unwrap_or(est).clamp(1.5, 300.0)
    }

    /// Estimated narration length from the word count.
    pub fn estimated_seconds(&self) -> f64 {
        let words = self.narration_text.split_whitespace().count() as f64;
        let pauses = self.narration_text.chars().filter(|c| matches!(c, '.' | ',' | '?' | '!' | ';' | ':' | '—')).count() as f64;
        (words / turkish::WORDS_PER_SECOND + pauses * 0.18).clamp(2.0, 60.0)
    }
}

/// Publishing metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SocialMetadata {
    pub alternative_titles: Vec<String>,
    pub description: String,
    pub tags: Vec<String>,
    pub thumbnail_text: String,
    pub hook_line: String,
    /// Pinned-comment question to drive comments.
    pub pinned_comment: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineLog {
    pub agent: String,
    pub level: String,
    pub message: String,
}

/// A source the Researcher found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceRef {
    pub title: String,
    pub url: String,
    pub publisher: String,
}

/// A fact with its kind and source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Fact {
    pub text: String,
    /// `belge` | `rivayet`.
    pub kind: String,
    pub source: Option<usize>,
}

/// A place the story is about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub note: String,
}

/// What the Researcher brought back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResearchPack {
    pub summary: String,
    pub facts: Vec<Fact>,
    pub sources: Vec<SourceRef>,
    pub places: Vec<Place>,
    /// `claude` | `template`.
    pub origin: String,
}

/// The current long-running job, for the progress bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JobInfo {
    pub label: String,
    /// 0–1.
    pub progress: f64,
    pub running: bool,
    pub error: String,
}

/// The complete, saved state of a CrStudio project.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CrStudioPipeline {
    pub format: VideoFormatPreset,
    pub title: String,
    pub tone: DocTone,
    pub target_duration_sec: f64,
    pub extra_instructions: String,
    /// The CrStudio root (all projects).
    pub projects_root: String,
    /// This project's folder.
    pub project_dir: String,
    pub stage: PipelineStage,
    pub agents: Vec<AgentInfo>,
    pub scenes: Vec<SceneSpec>,
    pub active_scene_index: usize,
    pub research: ResearchPack,
    pub research_summary: String,
    pub raw_script: String,
    pub social_meta: SocialMetadata,
    pub logs: VecDeque<PipelineLog>,
    pub claude_cli_available: bool,
    /// Seconds each Claude agent may run.
    pub claude_timeout_sec: u64,
    /// Let Claude look at visual candidates and score their relevance.
    pub relevance_check: bool,
    /// Ask Claude for the Chief Editor's content review too.
    pub director_ai: bool,
    pub voice_engine: String,
    /// Reference recording for voice cloning (Chatterbox).
    pub voice_reference: Option<String>,
    pub tts_voice: String,
    pub bgm_style: String,
    pub bgm_volume_db: f64,
    pub bgm_auto_duck: bool,
    pub bgm_path: Option<String>,
    pub fx_audit_summary: String,
    pub broll_pacing_enabled: bool,
    pub j_l_cut_enabled: bool,
    pub film_grain_enabled: bool,
    /// `cinematic_doc` | `viral_word_highlight`
    pub caption_style_mode: String,
    pub voice_polish_enabled: bool,
    /// Place the synthesized sound-effect cues (whoosh, boom, riser…). Off: they sound cheap; a
    /// scene's cue stays in the storyboard but is not cut in.
    pub synth_sfx: bool,
    pub brand_kit_enabled: bool,
    pub channel_name: String,
    /// Channel accent colour (`#rrggbb`) used by the YouTuber overlays.
    pub brand_color: String,
    /// Place YouTuber overlays (like, comment prompt, info boxes) automatically.
    pub youtuber_overlays: bool,
    pub thumbnail_path: Option<String>,
    pub youtube_chapters_text: String,
    pub credits_text: String,
    pub copyright_shield_path: Option<String>,
    pub subtitle_paths: Vec<String>,
    pub shorts_sequence: Option<u64>,
    pub assembled_sequence: Option<u64>,
    pub recording_scene_index: Option<usize>,
    pub recording_rate: u32,
    pub director: Option<director::DirectorReport>,
    pub job: JobInfo,
    /// Üretim Hattı: approved commands waiting to run, one after the other.
    pub queue: Vec<production::QueuedCommand>,
    /// Advance through stages that need no decision without waiting.
    pub autopilot: bool,
    /// The channel's visual concept for Blender shots (`bakir`, `minyatur`, `diorama`).
    pub visual_concept: String,
}

impl Default for CrStudioPipeline {
    fn default() -> Self {
        Self {
            format: VideoFormatPreset::YoutubeVideo,
            title: String::new(),
            tone: DocTone::MythLegend,
            target_duration_sec: 480.0,
            extra_instructions: String::new(),
            projects_root: default_projects_root(),
            project_dir: String::new(),
            stage: PipelineStage::Setup,
            agents: default_agents(),
            scenes: Vec::new(),
            active_scene_index: 0,
            research: ResearchPack::default(),
            research_summary: String::new(),
            raw_script: String::new(),
            social_meta: SocialMetadata::default(),
            logs: VecDeque::new(),
            claude_cli_available: claude::find_claude().is_some(),
            claude_timeout_sec: 900,
            relevance_check: true,
            director_ai: true,
            voice_engine: String::new(),
            voice_reference: None,
            tts_voice: String::new(),
            bgm_style: DocTone::MythLegend.default_bgm().into(),
            bgm_volume_db: -14.0,
            bgm_auto_duck: true,
            bgm_path: None,
            fx_audit_summary: String::new(),
            broll_pacing_enabled: true,
            j_l_cut_enabled: true,
            film_grain_enabled: true,
            caption_style_mode: "cinematic_doc".into(),
            voice_polish_enabled: true,
            synth_sfx: false,
            brand_kit_enabled: true,
            channel_name: "CRSTUDIO".into(),
            brand_color: "#e11d48".into(),
            youtuber_overlays: true,
            thumbnail_path: None,
            youtube_chapters_text: String::new(),
            credits_text: String::new(),
            copyright_shield_path: None,
            subtitle_paths: Vec::new(),
            shorts_sequence: None,
            assembled_sequence: None,
            recording_scene_index: None,
            recording_rate: 48_000,
            director: None,
            job: JobInfo::default(),
            queue: Vec::new(),
            autopilot: false,
            visual_concept: "atlas".into(),
        }
    }
}

impl CrStudioPipeline {
    pub fn push_log(&mut self, agent: &str, level: &str, message: impl Into<String>) {
        self.logs.push_back(PipelineLog { agent: agent.to_string(), level: level.to_string(), message: message.into() });
        while self.logs.len() > MAX_LOGS {
            self.logs.pop_front();
        }
    }

    pub fn set_agent_status(&mut self, index: u32, status: AgentStatus, summary: impl Into<String>) {
        if let Some(a) = self.agents.iter_mut().find(|a| a.index == index) {
            a.status = status;
            let s = summary.into();
            if !s.is_empty() {
                a.summary = s;
            }
        }
    }

    /// Append a live activity line to an agent (kept short).
    pub fn agent_activity(&mut self, index: u32, line: impl Into<String>) {
        if let Some(a) = self.agents.iter_mut().find(|a| a.index == index) {
            a.activity.push(line.into());
            let n = a.activity.len();
            if n > 30 {
                a.activity.drain(..n - 30);
            }
        }
    }

    /// Repair a state read from disk or set over MCP: indices, counts and numbers in range.
    pub fn sanitize(&mut self) {
        if !self.target_duration_sec.is_finite() {
            self.target_duration_sec = self.format.default_duration_sec();
        }
        self.target_duration_sec = self.target_duration_sec.clamp(15.0, self.format.max_duration_sec());
        self.scenes.truncate(MAX_SCENES);
        let nsrc = self.research.sources.len();
        for (i, s) in self.scenes.iter_mut().enumerate() {
            s.index = i;
            s.id = i as u64 + 1;
            if !s.duration_seconds.is_finite() {
                s.duration_seconds = 6.0;
            }
            s.duration_seconds = s.duration_seconds.clamp(1.0, 300.0);
            s.voiceover_duration_sec = s.voiceover_duration_sec.filter(|d| d.is_finite() && *d > 0.0);
            s.broll_subcuts = s.broll_subcuts.clamp(1, 3);
            s.sources.retain(|x| *x < nsrc);
            s.visual_candidates.truncate(24);
            if let Some(m) = s.map_spec.as_mut() {
                m.sanitize();
            }
        }
        if self.active_scene_index >= self.scenes.len() {
            self.active_scene_index = 0;
        }
        if self.agents.len() != default_agents().len() {
            self.agents = default_agents();
        }
        if !self.bgm_volume_db.is_finite() {
            self.bgm_volume_db = -18.0;
        }
        self.bgm_volume_db = self.bgm_volume_db.clamp(-40.0, 0.0);
        self.claude_timeout_sec = self.claude_timeout_sec.clamp(60, 3600);
        if self.projects_root.is_empty() {
            self.projects_root = default_projects_root();
        }
        // A saved "running" state means the app closed mid-run.
        if self.stage == PipelineStage::RunningAgents {
            self.stage = if self.scenes.is_empty() { PipelineStage::Setup } else { PipelineStage::ReviewScenes };
        }
        self.job.running = false;
        for a in &mut self.agents {
            if a.status == AgentStatus::Running {
                a.status = AgentStatus::Error;
                a.summary = "Yarıda kaldı".into();
            }
        }
    }

    pub fn project_path(&self, rel: &str) -> PathBuf {
        Path::new(&self.project_dir).join(rel)
    }

    pub fn total_seconds(&self) -> f64 {
        self.scenes.iter().map(SceneSpec::effective_duration).sum()
    }
}

/// Session-level holder for the pipeline and its background worker.
#[derive(Default)]
pub struct CrStudioState {
    pub pipeline: CrStudioPipeline,
    pub shared_worker: Option<Arc<Mutex<CrStudioPipeline>>>,
    pub worker_running: bool,
    pub cancel: Arc<AtomicBool>,
    /// JSON last written to disk (autosave compares against it).
    pub last_saved: String,
    pub last_save_check: f64,
    /// The UI already asked to reopen the last project (once per session).
    pub restored: bool,
    /// The Asistan conversation and its proposed steps (shared with its worker thread).
    pub assistant: assistant::Shared,
    /// A clone-reading take in progress: (sample rate, paragraph index, level test only).
    pub clone_rec: Option<(u32, usize, bool)>,
    /// The worker's stage as last adopted: while a job runs, the stage the person navigates to
    /// stays theirs unless the job itself moves on (agents finishing → scene review).
    pub job_stage: PipelineStage,
}

impl Clone for CrStudioState {
    fn clone(&self) -> Self {
        Self {
            pipeline: self.pipeline.clone(),
            shared_worker: None,
            worker_running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            last_saved: String::new(),
            last_save_check: 0.0,
            restored: self.restored,
            assistant: Default::default(),
            clone_rec: None,
            job_stage: self.job_stage,
        }
    }
}

pub fn default_agents() -> Vec<AgentInfo> {
    let a = |index: u32, id: &str, name: &str, role: &str| AgentInfo {
        index,
        id: id.into(),
        name: name.into(),
        role: role.into(),
        status: AgentStatus::Idle,
        summary: "Beklemede".into(),
        activity: Vec::new(),
    };
    vec![
        a(1, "researcher", "Araştırmacı", "Web'de kaynak bulur; belgelenmiş bilgiyi rivayetten ayırır, yerleri ve koordinatları çıkarır"),
        a(2, "scriptwriter", "Senarist", "Kanca, bölümler ve hedef süreye göre ölçülü sahne anlatımları yazar"),
        a(3, "director", "Yönetmen", "Storyboard: görsel türü, arama sorgusu, kamera, grafik, ses efekti ve YouTuber eklentileri"),
        a(4, "visual_hunter", "Görsel Avcısı", "Wikimedia Commons ve Openverse'ten açık lisanslı görseller; alaka denetimi"),
        a(5, "maps_graphics", "Harita & Grafik", "Haritalar, bölüm kartları, kanıt ve veri kartları"),
        a(6, "sound_fx", "Ses & Efekt", "Müzik, ses efektleri, geçişler ve tempo denetimi"),
        a(7, "editor", "Kurgucu", "Çok katmanlı timeline, YouTuber eklentileri, altyazı ve Shorts kesiti"),
        a(8, "packager", "Paketleyici", "Bölümler, açıklama ve krediler, altyazı dosyası, kapak görseli"),
        a(9, "chief_editor", "Yayın Yönetmeni", "Yayınlanabilirlik puanı ve hangi aşamada neyin düzeltileceği"),
    ]
}

pub fn default_projects_root() -> String {
    if cfg!(target_arch = "wasm32") {
        return "/tmp/CrStudio".into();
    }
    if let Some(home) = crate::media_browser::std_home_dir() {
        let base = Path::new(&home);
        let movies = base.join("Movies");
        let dir = if movies.is_dir() { movies.join("CrStudio") } else { base.join("Videos").join("CrStudio") };
        return dir.to_string_lossy().into_owned();
    }
    crate::temp_dir().join("CrStudio").to_string_lossy().into_owned()
}

/// Poll the background worker into the session and autosave.
/// Called by `Session::poll_persistence` every UI frame.
pub fn poll(s: &mut Session) -> bool {
    let mut running = false;
    if let Some(shared) = s.crstudio.shared_worker.clone() {
        let snap = shared.lock().unwrap_or_else(PoisonError::into_inner).clone();
        running = snap.job.running;
        // free navigation while a job runs: keep the stage on screen unless the job moved it
        let on_screen = s.crstudio.pipeline.stage;
        let moved = snap.stage != s.crstudio.job_stage;
        s.crstudio.job_stage = snap.stage;
        // the queue lives on the UI side (added to while the job runs)
        let queue = std::mem::take(&mut s.crstudio.pipeline.queue);
        s.crstudio.pipeline = snap;
        s.crstudio.pipeline.queue = queue;
        if !moved {
            s.crstudio.pipeline.stage = on_screen;
        }
        s.crstudio.worker_running = running;
        if !running {
            s.crstudio.shared_worker = None;
            let job = s.crstudio.pipeline.job.clone();
            if !job.error.is_empty() && !s.crstudio.pipeline.queue.is_empty() {
                // a failed step stops the line: nothing after it would make sense
                s.crstudio.pipeline.queue.clear();
                s.crstudio.pipeline.push_log("Üretim Hattı", "error", format!("Hat durdu: {} — {}", job.label, job.error));
                notify("Üretim hattı durdu", &format!("{}: {}", job.label, job.error));
            } else if s.crstudio.pipeline.queue.is_empty() {
                notify("CrStudio", &if job.error.is_empty() { format!("{} bitti", job.label) } else { format!("{}: {}", job.label, job.error) });
            }
        }
    }
    pump_queue(s);
    autosave(s);
    running || s.crstudio.worker_running
}

/// Start the next queued command when nothing runs (synchronous ones run straight through).
pub fn pump_queue(s: &mut Session) {
    for _ in 0..16 {
        if s.crstudio.worker_running || s.crstudio.pipeline.queue.is_empty() {
            return;
        }
        let c = s.crstudio.pipeline.queue.remove(0);
        match s.execute(&c.command, c.params.clone()) {
            Ok(_) => s.crstudio.pipeline.push_log("Üretim Hattı", "info", format!("Başladı: {}", c.label)),
            Err(e) => {
                s.crstudio.pipeline.queue.clear();
                s.crstudio.pipeline.push_log("Üretim Hattı", "error", format!("{}: {e} — hat durdu", c.label));
                notify("Üretim hattı durdu", &format!("{}: {e}", c.label));
                return;
            }
        }
        if !s.crstudio.worker_running && s.crstudio.pipeline.queue.is_empty() {
            s.crstudio.pipeline.push_log("Üretim Hattı", "ok", "Sıradaki işler bitti");
            notify("CrStudio", "Üretim hattındaki işler bitti");
        }
    }
}

/// A macOS notification (nothing elsewhere). The text goes in as script arguments, never as code.
pub fn notify(title: &str, body: &str) {
    if cfg!(test) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let title: String = title.chars().take(80).collect();
        let body: String = body.chars().take(200).collect();
        let _ = std::process::Command::new("osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)", "-e", "end run", &title, &body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, body);
    }
}

/// Keep the Mac awake while a long job runs (`caffeinate -i`); dropped when the job ends.
struct Awake(Option<std::process::Child>);

impl Awake {
    fn start() -> Self {
        #[cfg(target_os = "macos")]
        let child = std::process::Command::new("caffeinate")
            .arg("-i")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        #[cfg(not(target_os = "macos"))]
        let child = None;
        Self(child)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Save `crstudio.json` when the state changed (checked at most every 2 seconds).
pub fn autosave(s: &mut Session) {
    if cfg!(target_arch = "wasm32") || s.crstudio.pipeline.project_dir.is_empty() {
        return;
    }
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    if now - s.crstudio.last_save_check < 2.0 {
        return;
    }
    s.crstudio.last_save_check = now;
    let Ok(json) = serde_json::to_string(&s.crstudio.pipeline) else {
        return;
    };
    if json != s.crstudio.last_saved && persist::save(&s.crstudio.pipeline).is_ok() {
        persist::remember(&s.crstudio.pipeline.project_dir);
        s.crstudio.last_saved = json;
    }
}

/// Start `work` on a background thread with a shared copy of the pipeline. The UI shows the
/// shared copy live; when `work` returns (or panics) the job ends and the result stays.
pub fn spawn_job(s: &mut Session, label: &str, work: impl FnOnce(&Arc<Mutex<CrStudioPipeline>>, &Arc<AtomicBool>) + Send + 'static) -> Result<(), String> {
    if s.crstudio.worker_running {
        return Err("Başka bir iş sürüyor; bitmesini bekleyin ya da iptal edin".into());
    }
    s.crstudio.cancel.store(false, Ordering::Relaxed);
    s.crstudio.pipeline.job = JobInfo { label: label.to_string(), progress: 0.0, running: true, error: String::new() };
    let shared = Arc::new(Mutex::new(s.crstudio.pipeline.clone()));
    s.crstudio.job_stage = s.crstudio.pipeline.stage;
    s.crstudio.shared_worker = Some(Arc::clone(&shared));
    s.crstudio.worker_running = true;
    let cancel = Arc::clone(&s.crstudio.cancel);
    let label = label.to_string();
    let run = move || {
        // the Mac must not sleep under a render or a narration batch
        let _awake = Awake::start();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&shared, &cancel)));
        let mut g = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if res.is_err() {
            g.job.error = format!("{label}: beklenmeyen hata, iş durduruldu");
            let msg = g.job.error.clone();
            g.push_log("CrStudio", "error", msg);
            for a in &mut g.agents {
                if a.status == AgentStatus::Running {
                    a.status = AgentStatus::Error;
                }
            }
            if g.stage == PipelineStage::RunningAgents {
                g.stage = if g.scenes.is_empty() { PipelineStage::Setup } else { PipelineStage::ReviewScenes };
            }
        }
        if cancel.load(Ordering::Relaxed) && g.job.error.is_empty() {
            g.job.error = "İptal edildi".into();
        }
        g.job.running = false;
        g.job.progress = 1.0;
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    Ok(())
}

/// Lock the shared pipeline and change it (a short critical section; never do slow work inside).
pub fn with_shared<R>(shared: &Arc<Mutex<CrStudioPipeline>>, f: impl FnOnce(&mut CrStudioPipeline) -> R) -> R {
    let mut g = shared.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut g)
}

/// Write an attribution sidecar next to a generated or downloaded asset.
pub fn write_attribution_sidecar(file_path: &Path, title: &str, author: &str, source: &str, license: &str, notes: &str) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let file_name = file_path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "asset".into());
    let sidecar = PathBuf::from(format!("{}.attribution", file_path.display()));
    let content = format!(
        "asset:        {file_name}\ntitle:        {title}\nauthor:       {author}\nsource:       {source}\nlicense:      {license}\nadded:        {} by CrStudio\nnotes:        {notes}\n",
        persist::today()
    );
    let _ = std::fs::write(sidecar, content);
}

/// Shorten `s` to `max_chars`, adding `…`.
pub fn truncate_line(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
