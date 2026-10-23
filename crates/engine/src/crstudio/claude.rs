//! Running the local Claude Code CLI (`claude -p`) for CrStudio's agents.
//!
//! The CLI is found on `PATH` (and in the usual install folders on macOS, Linux and Windows), runs
//! with `--output-format stream-json --verbose` so every step the agent takes (text, tool calls)
//! is reported live through a callback, and is killed when it exceeds its time limit or the user
//! cancels. Nothing here panics: every failure is an `Err(String)` the caller can show.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Longest single line read from the CLI (a stream-json event); longer lines are cut.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// One thing the agent did, as reported while it runs.
#[derive(Clone, Debug, PartialEq)]
pub enum ClaudeEvent {
    /// Assistant prose (the first part of a message).
    Text(String),
    /// A tool call: tool name and a short description of its input.
    Tool { name: String, detail: String },
    /// The final result.
    Done { text: String, is_error: bool, cost_usd: f64, seconds: f64 },
}

/// What to run.
#[derive(Clone, Debug)]
pub struct ClaudeRequest {
    pub prompt: String,
    /// `--allowedTools` (e.g. `WebSearch`, `WebFetch`, `Read`, `mcp__crstudio`).
    pub allowed_tools: Vec<String>,
    /// `--mcp-config` file.
    pub mcp_config: Option<PathBuf>,
    pub max_turns: Option<u32>,
    pub timeout: Duration,
    /// Working directory (the project folder, so `Read` can open its thumbnails).
    pub cwd: Option<PathBuf>,
    /// `--append-system-prompt`.
    pub system: Option<String>,
}

impl ClaudeRequest {
    pub fn new(prompt: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            prompt: prompt.into(),
            allowed_tools: Vec::new(),
            mcp_config: None,
            max_turns: None,
            timeout: Duration::from_secs(timeout_secs.clamp(10, 3600)),
            cwd: None,
            system: None,
        }
    }
}

fn exe_names() -> &'static [&'static str] {
    if cfg!(windows) { &["claude.cmd", "claude.exe", "claude"] } else { &["claude"] }
}

/// Locate the `claude` CLI: `CRSTUDIO_CLAUDE`, then `PATH`, then the usual install folders.
pub fn find_claude() -> Option<PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    if let Some(p) = std::env::var_os("CRSTUDIO_CLAUDE").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in exe_names() {
                let c = dir.join(name);
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(home) = crate::media_browser::std_home_dir() {
        let home = PathBuf::from(home);
        extra.push(home.join(".local/bin"));
        extra.push(home.join(".claude/local"));
        extra.push(home.join(".npm-global/bin"));
        extra.push(home.join("AppData/Roaming/npm"));
        extra.push(home.join(".local/share/claude"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin"));
    extra.push(PathBuf::from("/usr/local/bin"));
    for dir in extra {
        for name in exe_names() {
            let c = dir.join(name);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}

/// A readable one-line summary of a tool call's input.
fn tool_detail(name: &str, input: &Value) -> String {
    let pick = |k: &str| input.get(k).and_then(Value::as_str).map(|s| short(s, 90));
    match name {
        "WebSearch" => pick("query").map(|q| format!("arıyor: {q}")),
        "WebFetch" => pick("url").map(|u| format!("okuyor: {u}")),
        "Read" => pick("file_path").map(|p| format!("açıyor: {p}")),
        _ => None,
    }
    .unwrap_or_else(|| {
        let raw = input.to_string();
        short(&raw, 110)
    })
}

/// Parse one stream-json line into events.
pub fn parse_line(line: &str) -> Vec<ClaudeEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str)
                                && !t.trim().is_empty()
                            {
                                out.push(ClaudeEvent::Text(t.to_string()));
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                            let detail = tool_detail(&name, b.get("input").unwrap_or(&Value::Null));
                            out.push(ClaudeEvent::Tool { name, detail });
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            let text = v.get("result").and_then(Value::as_str).unwrap_or("").to_string();
            let is_error =
                v.get("is_error").and_then(Value::as_bool).unwrap_or(false) || v.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success");
            let cost_usd = v.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let seconds = v.get("duration_ms").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
            out.push(ClaudeEvent::Done { text, is_error, cost_usd, seconds });
        }
        _ => {}
    }
    out
}

/// Run the CLI and return the final result text. `on_event` sees every step as it happens.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(req: &ClaudeRequest, cancel: &Arc<AtomicBool>, on_event: &mut dyn FnMut(&ClaudeEvent)) -> Result<String, String> {
    let bin = find_claude().ok_or_else(|| "Claude Code CLI (`claude`) bulunamadı. Kurulum: https://docs.claude.com/claude-code".to_string())?;
    let mut cmd = Command::new(&bin);
    cmd.arg("-p").arg(&req.prompt).args(["--output-format", "stream-json", "--verbose"]);
    if !req.allowed_tools.is_empty() {
        cmd.arg("--allowedTools").arg(req.allowed_tools.join(","));
    }
    if let Some(m) = &req.mcp_config {
        cmd.arg("--mcp-config").arg(m);
    }
    if let Some(n) = req.max_turns {
        cmd.arg("--max-turns").arg(n.to_string());
    }
    if let Some(sys) = &req.system {
        cmd.arg("--append-system-prompt").arg(sys);
    }
    if let Some(dir) = &req.cwd
        && Path::new(dir).is_dir()
    {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("NO_COLOR", "1");
    let mut child = cmd.spawn().map_err(|e| format!("Claude CLI başlatılamadı ({}): {e}", bin.display()))?;
    let stdout = child.stdout.take().ok_or("Claude CLI çıktısı alınamadı")?;
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if buf.len() > MAX_LINE {
                        buf.truncate(MAX_LINE);
                    }
                    if tx.send(buf.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let err_text = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(se) = stderr {
        let sink = Arc::clone(&err_text);
        std::thread::spawn(move || {
            let reader = BufReader::new(se);
            for line in reader.lines().map_while(Result::ok) {
                let mut g = sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if g.len() < 8000 {
                    g.push_str(&line);
                    g.push('\n');
                }
            }
        });
    }
    let start = Instant::now();
    let mut final_text: Option<(String, bool)> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("İptal edildi".into());
        }
        if start.elapsed() > req.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Claude {} sn içinde bitmedi (zaman aşımı; Ayarlar'dan artırılabilir)", req.timeout.as_secs()));
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                for ev in parse_line(line.trim()) {
                    if let ClaudeEvent::Done { text, is_error, .. } = &ev {
                        final_text = Some((text.clone(), *is_error));
                    }
                    on_event(&ev);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| format!("Claude CLI beklenemedi: {e}"))?;
    match final_text {
        Some((text, false)) => Ok(text),
        Some((text, true)) => Err(if text.is_empty() { "Claude hata döndürdü".into() } else { short(&text, 400) }),
        None => {
            let err = err_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner).trim().to_string();
            Err(if err.is_empty() { format!("Claude CLI sonuç vermeden kapandı ({status})") } else { short(&err, 600) })
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn run(_req: &ClaudeRequest, _cancel: &Arc<AtomicBool>, _on_event: &mut dyn FnMut(&ClaudeEvent)) -> Result<String, String> {
    Err("Web sürümünde yerel Claude CLI kullanılamaz".into())
}

/// Pull a JSON object (or array) out of a model answer that may wrap it in prose or ``` fences.
pub fn extract_json(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    if let Some(fence) = trimmed.find("```") {
        let after = trimmed.get(fence + 3..).unwrap_or("");
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(end) = after.find("```")
            && let Ok(v) = serde_json::from_str::<Value>(after.get(..end).unwrap_or("").trim())
        {
            return Some(v);
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(s), Some(e)) = (trimmed.find(open), trimmed.rfind(close))
            && e > s
            && let Some(slice) = trimmed.get(s..=e)
            && let Ok(v) = serde_json::from_str::<Value>(slice)
        {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_json_events() {
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Arıyorum"},{"type":"tool_use","name":"WebSearch","input":{"query":"Yedi Uyurlar Efes"}}]}}"#;
        let evs = parse_line(tool);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1], ClaudeEvent::Tool { name: "WebSearch".into(), detail: "arıyor: Yedi Uyurlar Efes".into() });
        let done = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"a\":1}","total_cost_usd":0.01,"duration_ms":1500}"#;
        assert_eq!(parse_line(done), vec![ClaudeEvent::Done { text: "{\"a\":1}".into(), is_error: false, cost_usd: 0.01, seconds: 1.5 }]);
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn extracts_fenced_json() {
        assert_eq!(extract_json("İşte:\n```json\n{\"x\":2}\n```\nbitti").unwrap()["x"], 2);
        assert_eq!(extract_json("a {\"y\":3} b").unwrap()["y"], 3);
        assert!(extract_json("yok").is_none());
    }
}
