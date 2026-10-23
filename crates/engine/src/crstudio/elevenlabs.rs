//! ElevenLabs text-to-speech (paid plan: commercial licence that stays valid after cancelling), used
//! with the narrator's own Professional Voice Clone.
//!
//! Settings live in `~/.crstudio/elevenlabs.json` (mode 0600); the API key never goes on a command
//! line (curl reads the header from a 0600 file). Audio comes back as 24 kHz 16-bit PCM and is
//! written as WAV. Text is normalized for Turkish and run through the pronunciation dictionary by
//! [`super::voice::synthesize`] before it gets here.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{persist, wav};

const API: &str = "https://api.elevenlabs.io/v1";
/// Characters per request (scenes are short; longer text is split at sentence ends).
const CHUNK: usize = 2400;
const RATE: u32 = 24_000;

pub const MODELS: &[(&str, &str)] = &[
    ("eleven_multilingual_v2", "Multilingual v2 (dengeli, sayıları iyi okur)"),
    ("eleven_v3", "Eleven v3 (en ifadeli; klonlarda değişken)"),
    ("eleven_flash_v2_5", "Flash v2.5 (hızlı, yarı kredi)"),
    ("eleven_turbo_v2_5", "Turbo v2.5 (hızlı, yarı kredi)"),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ElevenConfig {
    pub api_key: String,
    pub voice_id: String,
    pub voice_name: String,
    pub model_id: String,
    /// 0–1: lower is more expressive, higher more even.
    pub stability: f64,
    /// 0–1: how closely to stick to the clone.
    pub similarity: f64,
    /// 0–1: style exaggeration (0 keeps the clone's own delivery).
    pub style: f64,
    /// 0.7–1.2.
    pub speed: f64,
}

impl Default for ElevenConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            voice_id: String::new(),
            voice_name: String::new(),
            model_id: "eleven_multilingual_v2".into(),
            stability: 0.5,
            similarity: 0.85,
            style: 0.0,
            speed: 1.0,
        }
    }
}

impl ElevenConfig {
    pub fn sanitize(&mut self) {
        let clamp = |v: f64, lo: f64, hi: f64, d: f64| if v.is_finite() { v.clamp(lo, hi) } else { d };
        self.stability = clamp(self.stability, 0.0, 1.0, 0.5);
        self.similarity = clamp(self.similarity, 0.0, 1.0, 0.85);
        self.style = clamp(self.style, 0.0, 1.0, 0.0);
        self.speed = clamp(self.speed, 0.7, 1.2, 1.0);
        if !MODELS.iter().any(|(m, _)| *m == self.model_id) {
            self.model_id = "eleven_multilingual_v2".into();
        }
        self.api_key = self.api_key.trim().chars().filter(|c| c.is_ascii_graphic()).take(200).collect();
        self.voice_id = self.voice_id.trim().chars().filter(char::is_ascii_alphanumeric).take(64).collect();
        self.voice_name = self.voice_name.trim().chars().take(80).collect();
    }

    pub fn ready(&self) -> bool {
        !self.api_key.is_empty() && !self.voice_id.is_empty()
    }

    /// Status without the key.
    pub fn public(&self) -> Value {
        json!({
            "hasKey": !self.api_key.is_empty(), "keyHint": self.api_key.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>(),
            "voiceId": self.voice_id, "voiceName": self.voice_name, "modelId": self.model_id,
            "stability": self.stability, "similarity": self.similarity, "style": self.style, "speed": self.speed,
            "ready": self.ready(),
            "models": MODELS.iter().map(|(id, l)| json!({"id": id, "label": l})).collect::<Vec<_>>(),
        })
    }
}

fn config_path() -> PathBuf {
    persist::config_dir().join("elevenlabs.json")
}

pub fn load() -> ElevenConfig {
    let mut c: ElevenConfig = std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    c.sanitize();
    c
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    filmcraft_format::atomic_write(path, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn save(c: &ElevenConfig) -> Result<(), String> {
    let mut c = c.clone();
    c.sanitize();
    write_private(&config_path(), &serde_json::to_vec_pretty(&c).map_err(|e| e.to_string())?)
}

/// The header file curl reads (`-H @file`), so the key is not visible in the process list.
fn header_file(c: &ElevenConfig) -> Result<PathBuf, String> {
    let p = persist::config_dir().join("elevenlabs.headers");
    write_private(&p, format!("xi-api-key: {}\nAccept: */*\n", c.api_key).as_bytes())?;
    Ok(p)
}

/// The error message in an ElevenLabs error body.
fn api_error(body: &[u8], code: &str) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let msg = v["detail"]["message"].as_str().or_else(|| v["detail"].as_str()).or_else(|| v["message"].as_str()).unwrap_or("").to_string();
    let hint = match code {
        "401" => " (API anahtarı geçersiz)",
        "402" | "429" => " (kredi ya da istek sınırı doldu)",
        "404" => " (ses bulunamadı; ses kimliğini kontrol edin)",
        "000" => " (bağlantı kurulamadı; internet bağlantısını kontrol edin)",
        _ => "",
    };
    format!("ElevenLabs HTTP {code}{hint}{}", if msg.is_empty() { String::new() } else { format!(": {msg}") })
}

/// `GET /voices`: the account's voices (own clones first).
pub fn voices(c: &ElevenConfig) -> Result<Vec<Value>, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Web sürümünde yok".into());
    }
    if c.api_key.is_empty() {
        return Err("Önce ElevenLabs API anahtarını girin".into());
    }
    let hdr = header_file(c)?;
    let tmp = persist::config_dir().join("elevenlabs.voices.json");
    let code =
        curl(&["-sS", "--max-time", "30", "-o", &tmp.to_string_lossy(), "-w", "%{http_code}", "-H", &format!("@{}", hdr.display()), &format!("{API}/voices")])?;
    let body = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    if code != "200" {
        return Err(api_error(&body, &code));
    }
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("ElevenLabs yanıtı okunamadı: {e}"))?;
    let mut list: Vec<Value> = v["voices"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(500)
                .map(|x| {
                    json!({"id": x["voice_id"].as_str().unwrap_or(""), "name": x["name"].as_str().unwrap_or(""),
                       "category": x["category"].as_str().unwrap_or(""),
                       "own": matches!(x["category"].as_str(), Some("cloned" | "professional" | "generated"))})
                })
                .filter(|x| !x["id"].as_str().unwrap_or("").is_empty())
                .collect()
        })
        .unwrap_or_default();
    list.sort_by_key(|x| (!x["own"].as_bool().unwrap_or(false), x["name"].as_str().unwrap_or("").to_lowercase()));
    Ok(list)
}

fn curl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("curl").args(args).output().map_err(|e| format!("curl çalıştırılamadı: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(format!("ElevenLabs'e ulaşılamadı: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Speech-to-text (`POST /speech-to-text`, Scribe): what a listener would hear in `wav`.
pub fn transcribe(wav: &Path) -> Result<String, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Web sürümünde yok".into());
    }
    let c = load();
    if c.api_key.is_empty() {
        return Err("ElevenLabs API anahtarı yok".into());
    }
    if !wav.is_file() {
        return Err(format!("{}: ses dosyası yok", wav.display()));
    }
    let hdr = header_file(&c)?;
    let dir = wav.parent().map(Path::to_path_buf).unwrap_or_else(crate::temp_dir);
    let tmp = dir.join(".el_stt.json");
    let code = curl(&[
        "-sS",
        "--max-time",
        "180",
        "-X",
        "POST",
        "-H",
        &format!("@{}", hdr.display()),
        "-F",
        "model_id=scribe_v1",
        "-F",
        "language_code=tur",
        "-F",
        "tag_audio_events=false",
        "-F",
        &format!("file=@{}", wav.display()),
        "-o",
        &tmp.to_string_lossy(),
        "-w",
        "%{http_code}",
        &format!("{API}/speech-to-text"),
    ]);
    let body = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let code = code?;
    if code != "200" {
        return Err(api_error(&body, &code));
    }
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("ElevenLabs yanıtı okunamadı: {e}"))?;
    Ok(v["text"].as_str().unwrap_or("").chars().take(20_000).collect())
}

/// Split at sentence ends so each request stays under [`CHUNK`] characters.
fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for sentence in text.split_inclusive(['.', '!', '?', '…']) {
        if cur.chars().count() + sentence.chars().count() > CHUNK && !cur.trim().is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if sentence.chars().count() > CHUNK {
            // one huge "sentence": cut at spaces
            for w in sentence.split_inclusive(' ') {
                if cur.chars().count() + w.chars().count() > CHUNK && !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push_str(w);
            }
        } else {
            cur.push_str(sentence);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(40).collect()
}

/// Request body for one chunk.
pub fn request_body(c: &ElevenConfig, text: &str, previous: &str, next: &str) -> Value {
    let mut b = json!({
        "text": text,
        "model_id": c.model_id,
        "voice_settings": {"stability": c.stability, "similarity_boost": c.similarity, "style": c.style, "use_speaker_boost": true, "speed": c.speed},
    });
    // the v2.5 models take an explicit language
    if c.model_id.ends_with("v2_5") {
        b["language_code"] = json!("tr");
    }
    // neighbouring text keeps the intonation continuous across chunks
    if !previous.is_empty() && c.model_id != "eleven_v3" {
        b["previous_text"] = json!(previous);
    }
    if !next.is_empty() && c.model_id != "eleven_v3" {
        b["next_text"] = json!(next);
    }
    b
}

/// Synthesize `text` (already normalized) into the WAV file `out`. Returns what was said and
/// when each character began (the `with-timestamps` alignment), when the API sent it.
pub fn synthesize(text: &str, out: &Path) -> Result<Option<(String, Vec<f32>)>, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Web sürümünde yok".into());
    }
    let c = load();
    if !c.ready() {
        return Err("ElevenLabs ayarlı değil: API anahtarı ve ses seçin (Seslendirme ▸ ElevenLabs)".into());
    }
    let hdr = header_file(&c)?;
    let parts = chunks(text);
    if parts.is_empty() {
        return Err("Seslendirilecek metin boş".into());
    }
    let dir = out.parent().map(Path::to_path_buf).unwrap_or_else(crate::temp_dir);
    let mut samples: Vec<f32> = Vec::new();
    let mut spoken = String::new();
    let mut times: Vec<f32> = Vec::new();
    let mut aligned = true;
    for (k, part) in parts.iter().enumerate() {
        let prev = if k > 0 { parts.get(k - 1).map(String::as_str).unwrap_or("") } else { "" };
        let next = parts.get(k + 1).map(String::as_str).unwrap_or("");
        let body_path = dir.join(format!(".el_req_{k}.json"));
        let pcm_path = dir.join(format!(".el_out_{k}.pcm"));
        std::fs::write(&body_path, serde_json::to_vec(&request_body(&c, part, prev, next)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let url = format!("{API}/text-to-speech/{}/with-timestamps?output_format=pcm_{RATE}", c.voice_id);
        let code = curl(&[
            "-sS",
            "--max-time",
            "180",
            "-X",
            "POST",
            "-H",
            &format!("@{}", hdr.display()),
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &format!("@{}", body_path.display()),
            "-o",
            &pcm_path.to_string_lossy(),
            "-w",
            "%{http_code}",
            &url,
        ]);
        let _ = std::fs::remove_file(&body_path);
        let bytes = std::fs::read(&pcm_path).unwrap_or_default();
        let _ = std::fs::remove_file(&pcm_path);
        let code = code?;
        if code != "200" {
            return Err(api_error(&bytes, &code));
        }
        let Some((pcm, chars, starts)) = parse_timestamped(&bytes) else {
            return Err("ElevenLabs: yanıt okunamadı (zaman damgalı ses beklendi)".into());
        };
        let offset = samples.len() as f32 / RATE as f32;
        if chars.chars().count() == starts.len() && aligned {
            if !spoken.is_empty() {
                spoken.push(' ');
                times.push(offset);
            }
            spoken.push_str(&chars);
            times.extend(starts.iter().map(|t| t + offset));
        } else {
            aligned = false;
        }
        samples.extend(pcm16_to_f32(&pcm));
        if samples.len() > (RATE as usize) * 60 * 30 {
            return Err("ElevenLabs: ses beklenenden uzun".into());
        }
    }
    if samples.is_empty() {
        return Err("ElevenLabs boş ses döndürdü".into());
    }
    std::fs::write(out, wav::encode(&samples, RATE)).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok((aligned && !spoken.is_empty()).then_some((spoken, times)))
}

/// `with-timestamps` response → (PCM bytes, the characters spoken, each one's start in seconds).
pub fn parse_timestamped(bytes: &[u8]) -> Option<(Vec<u8>, String, Vec<f32>)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let pcm = filmcraft_project::gtemplate::base64_decode(v.get("audio_base64")?.as_str()?)?;
    let al = v.get("alignment").or_else(|| v.get("normalized_alignment"));
    let chars: String =
        al.and_then(|a| a.get("characters")).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let starts: Vec<f32> = al
        .and_then(|a| a.get("character_start_times_seconds"))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|t| t.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0) as f32).collect())
        .unwrap_or_default();
    Some((pcm, chars, starts))
}

/// Little-endian signed 16-bit PCM → f32 samples.
pub fn pcm16_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| match b {
            [lo, hi] => f32::from(i16::from_le_bytes([*lo, *hi])) / 32768.0,
            _ => 0.0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_sanitized_and_key_hidden() {
        let mut c = ElevenConfig {
            api_key: " sk_abc123 \n".into(),
            voice_id: "Ab-12/..".into(),
            stability: f64::NAN,
            speed: 9.0,
            model_id: "nope".into(),
            ..Default::default()
        };
        c.sanitize();
        assert_eq!(c.api_key, "sk_abc123");
        assert_eq!(c.voice_id, "Ab12");
        assert_eq!(c.stability, 0.5);
        assert_eq!(c.speed, 1.2);
        assert_eq!(c.model_id, "eleven_multilingual_v2");
        let p = c.public();
        assert!(!p.to_string().contains("sk_abc123"), "the key never leaves the file");
        assert_eq!(p["keyHint"], "c123");
    }

    #[test]
    fn chunks_split_at_sentences() {
        let long = "Bir cümle. ".repeat(500);
        let parts = chunks(&long);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK));
        assert_eq!(chunks("  ").len(), 0);
        let one = "a".repeat(CHUNK * 2 + 5);
        assert!(!chunks(&one).is_empty());
    }

    #[test]
    fn body_and_pcm() {
        let c = ElevenConfig { model_id: "eleven_flash_v2_5".into(), voice_id: "x".into(), ..Default::default() };
        let b = request_body(&c, "Merhaba", "Önce", "");
        assert_eq!(b["language_code"], "tr");
        assert_eq!(b["previous_text"], "Önce");
        assert!(b.get("next_text").is_none());
        assert_eq!(pcm16_to_f32(&[0x00, 0x40, 0xff]), vec![0.5], "odd trailing byte ignored");
        assert!(api_error(br#"{"detail":{"message":"quota"}}"#, "402").contains("quota"));
        // with-timestamps: base64 PCM + per-character starts
        let (pcm, chars, t) =
            parse_timestamped(br#"{"audio_base64":"AEA=","alignment":{"characters":["E","f","e","s"],"character_start_times_seconds":[0.0,0.1,0.2,0.3]}}"#)
                .unwrap();
        assert_eq!(pcm, vec![0x00, 0x40]);
        assert_eq!(chars, "Efes");
        assert_eq!(t.len(), 4);
        assert!(parse_timestamped(b"not json").is_none());
    }
}
