//! Voice engines for CrStudio narration.
//!
//! The AI voice is a draft: it times the edit and guides the narrator, who records the final take
//! with the teleprompter. Engines:
//! - `piper:<voice>`: Piper (fahrettin / fettah, Turkish), a separate process installed by
//!   `tools/tts/install.sh` under `~/.crstudio/tts`.
//! - `chatterbox`: Chatterbox Multilingual (MIT), a local HTTP server on 127.0.0.1:8765 started on
//!   demand; can clone the narrator's voice from a reference recording.
//! - `macos:<voice>`: the macOS `say` voices (draft only; Apple's licence is personal use).
//! - `kokoro:<id>`: FilmCraft's Kokoro voices (English only).
//!
//! Every result carries the engine label and licence note, kept with the scene so the publish
//! check can tell an AI draft from the narrator's own recording.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{elevenlabs, persist, turkish, wav};

pub const CHATTERBOX_PORT: u16 = 8765;

/// One engine as offered in the voice picker.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceEngineInfo {
    pub id: String,
    pub label: String,
    pub language: String,
    pub license: String,
    pub available: bool,
    pub note: String,
}

/// What a synthesis produced.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceResult {
    pub seconds: f64,
    pub label: String,
    pub license: String,
    /// The spoken text and each character's start time (engines that report it: ElevenLabs).
    pub alignment: Option<(String, Vec<f32>)>,
}

/// Options for one synthesis.
#[derive(Clone, Debug, Default)]
pub struct VoiceOpts {
    /// Reference recording for voice cloning (Chatterbox).
    pub reference: Option<String>,
    /// 0.25–2.0 expressiveness (Chatterbox `exaggeration`).
    pub expressiveness: f64,
}

/// `~/.crstudio/tts`.
pub fn tts_root() -> PathBuf {
    let home = crate::media_browser::std_home_dir().map(PathBuf::from).unwrap_or_else(crate::temp_dir);
    home.join(".crstudio").join("tts")
}

fn venv_python(name: &str) -> PathBuf {
    let root = tts_root().join(name);
    if cfg!(windows) { root.join("Scripts").join("python.exe") } else { root.join("bin").join("python") }
}

fn piper_voice_installed(voice: &str) -> bool {
    tts_root().join("piper").join(format!("{voice}.onnx")).is_file()
}

/// Engines on this computer (installed or not), in picker order.
pub fn engines() -> Vec<VoiceEngineInfo> {
    let piper_ok = venv_python("piper-venv").is_file();
    let cb_ok = venv_python("chatterbox-venv").is_file() && tts_root().join("chatterbox_server.py").is_file();
    let mut v = Vec::new();
    let el = elevenlabs::load();
    v.push(VoiceEngineInfo {
        id: "elevenlabs".into(),
        label: if el.voice_name.is_empty() { "ElevenLabs (klon sesiniz)".into() } else { format!("ElevenLabs – {}", el.voice_name) },
        language: "tr".into(),
        license: "Ücretli plan: ticari lisans (abonelik bitse de kalıcı)".into(),
        available: el.ready(),
        note: if el.ready() { "Profesyonel klonunuzla son kalite ses".into() } else { "API anahtarı ve ses seçimi gerekli".into() },
    });
    for (voice, label) in [("tr_TR-fahrettin-medium", "Piper – Fahrettin (erkek)"), ("tr_TR-fettah-medium", "Piper – Fettah (erkek)")] {
        v.push(VoiceEngineInfo {
            id: format!("piper:{voice}"),
            label: label.into(),
            language: "tr".into(),
            license: "Veri CC0; lessac tabanlı model (kişisel kullanım, taslak)".into(),
            available: piper_ok && piper_voice_installed(voice),
            note: if piper_ok { "Hızlı taslak ses".into() } else { "Kurulum: tools/tts/install.sh".into() },
        });
    }
    v.push(VoiceEngineInfo {
        id: "chatterbox".into(),
        label: "Chatterbox Çok Dilli (Türkçe, ses klonlama)".into(),
        language: "tr".into(),
        license: "MIT (çıktıda görünmez filigran)".into(),
        available: cb_ok,
        note: if cb_ok { "En doğal AI ses; referans kaydınızla klonlar".into() } else { "Kurulum: tools/tts/install.sh".into() },
    });
    if cfg!(target_os = "macos") {
        v.push(VoiceEngineInfo {
            id: "macos:Yelda".into(),
            label: "macOS Yelda (kadın)".into(),
            language: "tr".into(),
            license: "Apple: yalnız kişisel, ticari olmayan kullanım".into(),
            available: Path::new("/usr/bin/say").is_file(),
            note: "Yalnız taslak".into(),
        });
    }
    v.push(VoiceEngineInfo {
        id: "kokoro:af_heart".into(),
        label: "Kokoro Heart (İngilizce)".into(),
        language: "en".into(),
        license: "Apache-2.0".into(),
        available: filmcraft_tts::voices_in(crate::transcript::models_dir().as_deref()).iter().any(|v| v.id == "af_heart" && v.installed),
        note: "Yalnız İngilizce metin".into(),
    });
    v
}

/// The narrator's pronunciation dictionary (`~/.crstudio/telaffuz.tsv`).
pub fn lexicon_path() -> PathBuf {
    persist::config_dir().join("telaffuz.tsv")
}

pub const LEXICON_TEMPLATE: &str = "# Telaffuz sözlüğü: her satıra  yazılış<TAB>okunuş  ya da  yazılış = okunuş\n# Tüm ses motorlarında, seslendirmeden önce uygulanır. Örnekler:\nDecius = Desiyus\nAshab-ı Kehf = Ashabıkehf\n";

pub fn lexicon_text() -> String {
    std::fs::read_to_string(lexicon_path()).unwrap_or_else(|_| LEXICON_TEMPLATE.to_string())
}

pub fn save_lexicon_text(text: &str) -> Result<usize, String> {
    let text: String = text.chars().take(200_000).collect();
    let n = turkish::parse_lexicon(&text).len();
    let p = lexicon_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    filmcraft_format::atomic_write(&p, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(n)
}

pub fn load_lexicon() -> Vec<(String, String)> {
    turkish::parse_lexicon(&lexicon_text())
}

/// The first available Turkish engine (Chatterbox, then Piper, then macOS), if any.
pub fn default_engine() -> Option<String> {
    let all = engines();
    for pref in ["elevenlabs", "chatterbox", "piper:tr_TR-fahrettin-medium", "piper:tr_TR-fettah-medium", "macos:Yelda"] {
        if all.iter().any(|e| e.id == pref && e.available) {
            return Some(pref.to_string());
        }
    }
    None
}

/// Synthesize `text` with `engine` into the WAV file `out`.
pub fn synthesize(engine: &str, text: &str, out: &Path, opts: &VoiceOpts) -> Result<VoiceResult, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Web sürümünde yerel ses motoru yok".into());
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("Seslendirilecek metin boş".into());
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let info = engines().into_iter().find(|e| e.id == engine);
    let license = info.as_ref().map(|i| i.license.clone()).unwrap_or_default();
    let label = info.as_ref().map(|i| i.label.clone()).unwrap_or_else(|| engine.to_string());
    let spoken = if engine.starts_with("kokoro:") { text.to_string() } else { turkish::normalize_for_speech(&turkish::apply_lexicon(text, &load_lexicon())) };
    let mut alignment = None;
    if engine == "elevenlabs" {
        alignment = elevenlabs::synthesize(&spoken, out)?;
    } else if let Some(voice) = engine.strip_prefix("piper:") {
        piper(voice, &spoken, out)?;
    } else if engine == "chatterbox" {
        chatterbox(&spoken, out, opts)?;
    } else if let Some(voice) = engine.strip_prefix("macos:") {
        macos_say(voice, &spoken, out)?;
    } else if let Some(voice) = engine.strip_prefix("kokoro:") {
        kokoro(voice, &spoken, out)?;
    } else {
        return Err(format!("Bilinmeyen ses motoru: {engine}"));
    }
    let seconds = wav::duration(out).ok_or_else(|| format!("{label}: ses dosyası okunamadı"))?;
    Ok(VoiceResult { seconds, label, license, alignment })
}

fn run_with_stdin(mut cmd: Command, input: &str, timeout: Duration) -> Result<(), String> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("başlatılamadı: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(input.as_bytes());
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut se) = child.stderr.take() {
                    let _ = se.read_to_string(&mut err);
                }
                let tail: String = err.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
                return Err(format!("çıkış {status}: {}", tail.trim()));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("zaman aşımı".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn piper(voice: &str, text: &str, out: &Path) -> Result<(), String> {
    let py = venv_python("piper-venv");
    if !py.is_file() {
        return Err("Piper kurulu değil (tools/tts/install.sh)".into());
    }
    if !piper_voice_installed(voice) {
        return Err(format!("Piper sesi indirilmemiş: {voice}"));
    }
    let mut cmd = Command::new(py);
    cmd.args(["-m", "piper", "--data-dir"]).arg(tts_root().join("piper")).args(["-m", voice, "-f"]).arg(out);
    run_with_stdin(cmd, text, Duration::from_secs(300)).map_err(|e| format!("Piper: {e}"))
}

fn macos_say(voice: &str, text: &str, out: &Path) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/say");
    cmd.args(["-v", voice, "--file-format=WAVE", "--data-format=LEF32@24000", "-o"]).arg(out).arg("-f").arg("-");
    run_with_stdin(cmd, text, Duration::from_secs(300)).map_err(|e| format!("macOS say: {e}"))
}

fn kokoro(voice: &str, text: &str, out: &Path) -> Result<(), String> {
    let models = crate::transcript::models_dir();
    let v = filmcraft_tts::voice_in(voice, models.as_deref()).map_err(|e| format!("Kokoro: {e}"))?;
    let audio = v.synthesize(text, &filmcraft_tts::Params { semitones: 0.0, pace: 1.0 }).map_err(|e| format!("Kokoro: {e}"))?;
    let bytes = wav::encode(&audio.samples, audio.sample_rate);
    filmcraft_format::atomic_write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))
}

// ------------------------------------------------------------------------------------------------
// Chatterbox local server
// ------------------------------------------------------------------------------------------------

static SERVER: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();

/// A tiny HTTP/1.1 exchange with the local server. Returns (status, body).
fn http_local(method: &str, path: &str, body: Option<&str>, timeout: Duration) -> Result<(u16, String), String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], CHATTERBOX_PORT));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, body))
}

/// True when the Chatterbox server answers.
pub fn chatterbox_running() -> bool {
    http_local("GET", "/health", None, Duration::from_secs(2)).is_ok_and(|(c, _)| c == 200)
}

/// Start the Chatterbox server if it is not running, and wait until it answers (model load).
pub fn ensure_chatterbox() -> Result<(), String> {
    if chatterbox_running() {
        return Ok(());
    }
    let py = venv_python("chatterbox-venv");
    let script = tts_root().join("chatterbox_server.py");
    if !py.is_file() || !script.is_file() {
        return Err("Chatterbox kurulu değil (tools/tts/install.sh)".into());
    }
    {
        let slot = SERVER.get_or_init(|| Mutex::new(None));
        let mut g = slot.lock().unwrap_or_else(PoisonError::into_inner);
        let alive = g.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        if !alive {
            let log = std::fs::File::create(tts_root().join("chatterbox-server.log")).ok();
            let mut cmd = Command::new(py);
            cmd.arg(&script).arg("--port").arg(CHATTERBOX_PORT.to_string()).stdin(Stdio::null());
            match log.and_then(|f| f.try_clone().ok().map(|f2| (f, f2))) {
                Some((a, b)) => {
                    cmd.stdout(a).stderr(b);
                }
                None => {
                    cmd.stdout(Stdio::null()).stderr(Stdio::null());
                }
            }
            *g = Some(cmd.spawn().map_err(|e| format!("Chatterbox sunucusu başlatılamadı: {e}"))?);
        }
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(180) {
        if chatterbox_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Chatterbox sunucusu 3 dakikada açılmadı (~/.crstudio/tts/chatterbox-server.log)".into())
}

/// Stop a Chatterbox server this app started.
pub fn stop_chatterbox() {
    if let Some(slot) = SERVER.get() {
        let mut g = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mut c) = g.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn chatterbox(text: &str, out: &Path, opts: &VoiceOpts) -> Result<(), String> {
    ensure_chatterbox()?;
    let mut req = json!({
        "text": text,
        "out": out.to_string_lossy(),
        "language": "tr",
        "exaggeration": if opts.expressiveness > 0.0 { opts.expressiveness.clamp(0.25, 2.0) } else { 0.5 },
        "cfg": 0.5,
    });
    if let Some(r) = &opts.reference
        && Path::new(r).is_file()
    {
        req["voice_ref"] = json!(r);
    }
    let (code, body) = http_local("POST", "/tts", Some(&req.to_string()), Duration::from_secs(900)).map_err(|e| format!("Chatterbox: {e}"))?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if code != 200 || v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!("Chatterbox: {}", v.get("error").and_then(Value::as_str).unwrap_or("bilinmeyen hata")));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Narration mastering (idempotent: always from the raw take)
// ------------------------------------------------------------------------------------------------

/// Trim leading/trailing silence (keeping 65 ms), gentle compression and −1.2 dBFS peak
/// normalization of `raw`, written to `master`. Running it twice gives the same file.
pub fn master(raw: &Path, master: &Path) -> Result<f64, String> {
    let m = wav::read(raw)?;
    let sr = m.rate.max(8000) as usize;
    let mono = m.samples;
    if mono.len() < sr / 4 {
        filmcraft_format::atomic_write(master, &wav::encode(&mono, m.rate)).map_err(|e| e.to_string())?;
        return Ok(mono.len() as f64 / sr as f64);
    }
    let thresh = 0.01_f32;
    let pad = sr * 65 / 1000;
    let first = mono.iter().position(|s| s.abs() > thresh).unwrap_or(0);
    let last = mono.iter().rposition(|s| s.abs() > thresh).unwrap_or(mono.len().saturating_sub(1));
    let a = first.saturating_sub(pad);
    let b = (last + pad + 1).min(mono.len());
    let slice = if b > a + sr / 3 { mono.get(a..b).unwrap_or(&mono) } else { &mono[..] };
    let mut out: Vec<f32> = slice.iter().map(|x| (x * 1.3).tanh() / 1.3_f32.tanh()).collect();
    let peak = out.iter().fold(0.01_f32, |p, x| p.max(x.abs()));
    let gain = (0.87 / peak).clamp(0.3, 4.0);
    let fade = (sr * 15 / 1000).max(1);
    let n = out.len();
    for (i, s) in out.iter_mut().enumerate() {
        let fi = (i as f32 / fade as f32).min(1.0);
        let fo = ((n.saturating_sub(1 + i)) as f32 / fade as f32).min(1.0);
        *s = (*s * gain * fi * fo).clamp(-0.98, 0.98);
    }
    filmcraft_format::atomic_write(master, &wav::encode(&out, m.rate)).map_err(|e| format!("{}: {e}", master.display()))?;
    Ok(out.len() as f64 / sr as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mastering_is_idempotent() {
        let dir = crate::temp_dir().join("crstudio_master_test");
        let _ = std::fs::create_dir_all(&dir);
        let raw = dir.join("raw.wav");
        let mut s = vec![0.0f32; 4800];
        s.extend((0..24000).map(|i| (i as f32 * 0.05).sin() * 0.3));
        s.extend(vec![0.0f32; 4800]);
        std::fs::write(&raw, wav::encode(&s, 24000)).unwrap();
        let m1 = dir.join("m1.wav");
        let m2 = dir.join("m2.wav");
        let d1 = master(&raw, &m1).unwrap();
        let d2 = master(&raw, &m2).unwrap();
        assert_eq!(d1, d2);
        assert_eq!(std::fs::read(&m1).unwrap(), std::fs::read(&m2).unwrap());
        assert!(d1 < 1.2, "silence trimmed: {d1}");
    }

    #[test]
    fn unknown_engine_is_an_error() {
        let out = crate::temp_dir().join("crstudio_voice_unknown.wav");
        assert!(synthesize("nope", "merhaba", &out, &VoiceOpts::default()).is_err());
        assert!(synthesize("piper:x", "  ", &out, &VoiceOpts::default()).is_err());
    }
}
