//! YouTube publishing infrastructure (YouTube Data API v3).
//!
//! The user creates an OAuth client ("Desktop app") in Google Cloud, pastes its client id and
//! secret into CrStudio once, and signs in with the browser; the refresh token is kept in
//! `~/.crstudio/youtube.json` (readable only by the user on Unix). Uploads use the resumable
//! protocol and run as a background job (status via [`upload_status`]). HTTPS goes through the
//! system `curl` so the core keeps no TLS stack of its own.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::persist::config_dir;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPES: &str =
    "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly https://www.googleapis.com/auth/youtube.force-ssl";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct YoutubeConfig {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub access_token: String,
    /// Unix seconds when `access_token` expires.
    pub expires_at: u64,
    pub channel_title: String,
}

fn config_path() -> PathBuf {
    config_dir().join("youtube.json")
}

pub fn load_config() -> YoutubeConfig {
    std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_config(c: &YoutubeConfig) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = config_path();
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    filmcraft_format::atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// State shown in the publish screen.
pub fn status() -> Value {
    let c = load_config();
    json!({
        "configured": !c.client_id.is_empty() && !c.client_secret.is_empty(),
        "connected": !c.refresh_token.is_empty(),
        "channelTitle": c.channel_title,
        "upload": upload_status(),
    })
}

fn curl(args: &[&str]) -> Result<(Value, String), String> {
    let out = Command::new("curl").args(["-sS", "--max-time", "60", "-D", "-"]).args(args).output().map_err(|e| format!("curl çalıştırılamadı: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (headers, body) = text.rsplit_once("\r\n\r\n").map(|(h, b)| (h.to_string(), b.to_string())).unwrap_or((String::new(), text.clone()));
    let v = serde_json::from_str::<Value>(body.trim()).unwrap_or(Value::Null);
    if !out.status.success() {
        return Err(format!("ağ hatası: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((v, headers))
}

static AUTH: OnceLock<Mutex<String>> = OnceLock::new();

fn set_auth_state(s: &str) {
    let m = AUTH.get_or_init(|| Mutex::new(String::new()));
    *m.lock().unwrap_or_else(PoisonError::into_inner) = s.to_string();
}

pub fn auth_state() -> String {
    AUTH.get().map(|m| m.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap_or_default()
}

/// Start the browser sign-in: listen on a loopback port and return the URL to open. The token
/// exchange finishes on a background thread when Google redirects back.
pub fn begin_connect() -> Result<String, String> {
    let c = load_config();
    if c.client_id.is_empty() || c.client_secret.is_empty() {
        return Err("Önce Google Cloud'daki OAuth istemci kimliğini ve gizli anahtarını girin".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let url = format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
        enc(&c.client_id),
        enc(&redirect),
        enc(SCOPES)
    );
    set_auth_state("Tarayıcıda Google hesabınızla giriş bekleniyor…");
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finish_connect(&listener, &redirect)));
        match result {
            Ok(Ok(title)) => set_auth_state(&format!("Bağlandı: {title}")),
            Ok(Err(e)) => set_auth_state(&format!("Bağlantı başarısız: {e}")),
            Err(_) => set_auth_state("Bağlantı başarısız: beklenmeyen hata"),
        }
    });
    Ok(url)
}

fn finish_connect(listener: &TcpListener, redirect: &str) -> Result<String, String> {
    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let code = line
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.split_once('?'))
        .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("code=")))
        .map(|c| c.replace("%2F", "/"))
        .ok_or("Google yanıtında yetki kodu yok (izin reddedildi mi?)")?;
    let page =
        "<html><meta charset=utf-8><body style='font-family:sans-serif'><h2>CrStudio YouTube bağlantısı tamam.</h2>Bu sekmeyi kapatabilirsiniz.</body></html>";
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
    let mut c = load_config();
    let form = format!(
        "code={}&client_id={}&client_secret={}&redirect_uri={}&grant_type=authorization_code",
        enc(&code),
        enc(&c.client_id),
        enc(&c.client_secret),
        enc(redirect)
    );
    let (v, _) = curl(&["-X", "POST", "-H", "Content-Type: application/x-www-form-urlencoded", "--data", &form, TOKEN_URL])?;
    let refresh = v.get("refresh_token").and_then(Value::as_str).ok_or_else(|| format!("token alınamadı: {v}"))?;
    c.refresh_token = refresh.to_string();
    c.access_token = v.get("access_token").and_then(Value::as_str).unwrap_or("").to_string();
    c.expires_at = now() + v.get("expires_in").and_then(Value::as_u64).unwrap_or(3000).saturating_sub(60);
    save_config(&c)?;
    let title = fetch_channel_title(&mut c).unwrap_or_else(|_| "kanal".into());
    c.channel_title = title.clone();
    save_config(&c)?;
    Ok(title)
}

/// A valid access token (refreshed when needed).
pub fn access_token(c: &mut YoutubeConfig) -> Result<String, String> {
    if !c.access_token.is_empty() && now() < c.expires_at {
        return Ok(c.access_token.clone());
    }
    if c.refresh_token.is_empty() {
        return Err("YouTube hesabı bağlı değil".into());
    }
    let form =
        format!("client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token", enc(&c.client_id), enc(&c.client_secret), enc(&c.refresh_token));
    let (v, _) = curl(&["-X", "POST", "-H", "Content-Type: application/x-www-form-urlencoded", "--data", &form, TOKEN_URL])?;
    let tok = v.get("access_token").and_then(Value::as_str).ok_or_else(|| format!("token yenilenemedi: {v}"))?;
    c.access_token = tok.to_string();
    c.expires_at = now() + v.get("expires_in").and_then(Value::as_u64).unwrap_or(3000).saturating_sub(60);
    save_config(c)?;
    Ok(c.access_token.clone())
}

fn fetch_channel_title(c: &mut YoutubeConfig) -> Result<String, String> {
    let tok = access_token(c)?;
    let auth = format!("Authorization: Bearer {tok}");
    let (v, _) = curl(&["-H", &auth, "https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true"])?;
    v.pointer("/items/0/snippet/title").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "kanal bulunamadı".into())
}

/// What to upload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UploadRequest {
    pub video_path: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// `private` | `unlisted` | `public`.
    pub privacy: String,
    /// RFC 3339 time to publish (requires `privacy = private`).
    pub publish_at: Option<String>,
    pub thumbnail_path: Option<String>,
    pub category_id: String,
    pub made_for_kids: bool,
    /// The video contains realistic altered or synthetic content.
    pub synthetic_media: bool,
}

static UPLOAD: OnceLock<Mutex<Value>> = OnceLock::new();

fn set_upload(v: Value) {
    let m = UPLOAD.get_or_init(|| Mutex::new(Value::Null));
    *m.lock().unwrap_or_else(PoisonError::into_inner) = v;
}

pub fn upload_status() -> Value {
    UPLOAD.get().map(|m| m.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap_or(Value::Null)
}

/// Start an upload on a background thread.
pub fn start_upload(req: UploadRequest) -> Result<(), String> {
    if !Path::new(&req.video_path).is_file() {
        return Err(format!("Video dosyası yok: {}", req.video_path));
    }
    if load_config().refresh_token.is_empty() {
        return Err("YouTube hesabı bağlı değil".into());
    }
    set_upload(json!({"state": "running", "message": "Yükleniyor…"}));
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| upload(&req)));
        match r {
            Ok(Ok(id)) => set_upload(json!({"state": "done", "videoId": id, "url": format!("https://youtu.be/{id}"), "message": "Yüklendi"})),
            Ok(Err(e)) => set_upload(json!({"state": "error", "message": e})),
            Err(_) => set_upload(json!({"state": "error", "message": "beklenmeyen hata"})),
        }
    });
    Ok(())
}

fn upload(req: &UploadRequest) -> Result<String, String> {
    let mut c = load_config();
    let tok = access_token(&mut c)?;
    let auth = format!("Authorization: Bearer {tok}");
    let mut status = json!({
        "privacyStatus": if req.privacy.is_empty() { "private" } else { req.privacy.as_str() },
        "selfDeclaredMadeForKids": req.made_for_kids,
        "containsSyntheticMedia": req.synthetic_media,
    });
    if let Some(at) = &req.publish_at {
        status["publishAt"] = json!(at);
        status["privacyStatus"] = json!("private");
    }
    let meta = json!({
        "snippet": {
            "title": req.title.chars().take(100).collect::<String>(),
            "description": req.description.chars().take(5000).collect::<String>(),
            "tags": req.tags,
            "categoryId": if req.category_id.is_empty() { "27" } else { req.category_id.as_str() },
            "defaultLanguage": "tr",
            "defaultAudioLanguage": "tr",
        },
        "status": status,
    })
    .to_string();
    let size = std::fs::metadata(&req.video_path).map_err(|e| e.to_string())?.len().to_string();
    let len_h = format!("X-Upload-Content-Length: {size}");
    let (_, headers) = curl(&[
        "-X",
        "POST",
        "-H",
        &auth,
        "-H",
        "Content-Type: application/json; charset=UTF-8",
        "-H",
        "X-Upload-Content-Type: video/*",
        "-H",
        &len_h,
        "--data",
        &meta,
        "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status",
    ])?;
    let location = headers
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string()))
        .ok_or("YouTube yükleme adresi vermedi (kota ya da yetki?)")?;
    set_upload(json!({"state": "running", "message": "Video gönderiliyor…"}));
    let out = Command::new("curl")
        .args(["-sS", "--max-time", "7200", "-X", "PUT", "-H", &auth, "-H", "Content-Type: video/*", "--upload-file", &req.video_path, &location])
        .output()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("yükleme tamamlanmadı: {}", String::from_utf8_lossy(&out.stdout).chars().take(300).collect::<String>()))?
        .to_string();
    if let Some(th) = &req.thumbnail_path
        && Path::new(th).is_file()
    {
        set_upload(json!({"state": "running", "message": "Kapak görseli ayarlanıyor…"}));
        let url = format!("https://www.googleapis.com/upload/youtube/v3/thumbnails/set?videoId={id}");
        let file = format!("@{th}");
        let _ = curl(&["-X", "POST", "-H", &auth, "-H", "Content-Type: image/png", "--data-binary", &file, &url]);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding() {
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn upload_refuses_missing_file() {
        let r = start_upload(UploadRequest { video_path: "/yok/video.mp4".into(), ..Default::default() });
        assert!(r.is_err());
    }
}
