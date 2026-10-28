//! Connected accounts: Google Calendar and ChatGPT.
//!
//! Both sign in the way desktop apps should: open the system browser, let the provider show its
//! own sign-in and consent pages, and receive the answer on a one-shot listener on 127.0.0.1
//! (OAuth 2.0 for native apps, RFC 8252, with PKCE). HoverPill never sees a password.
//!
//! Credentials live in `accounts.bin` in the app config dir. On Windows the file is encrypted
//! with DPAPI, so only this Windows user can read it; on Linux it's readable by the owner only.

pub mod chatgpt;
pub mod google;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// How long to wait for the user to finish in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

// ---------- the credential vault ----------

/// One encrypted JSON object, one key per account ("google", "chatgpt").
pub struct Vault {
    path: PathBuf,
    data: Mutex<Map<String, Value>>,
}

impl Vault {
    pub fn load(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let path = app.path().app_config_dir()?.join("accounts.bin");
        let data = std::fs::read(&path)
            .ok()
            .and_then(|bytes| unprotect(&bytes))
            .and_then(|plain| serde_json::from_slice::<Map<String, Value>>(&plain).ok())
            .unwrap_or_default();
        Ok(Self { path, data: Mutex::new(data) })
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.data.lock().unwrap().get(key).cloned().and_then(|v| serde_json::from_value(v).ok())
    }

    pub fn set<T: Serialize>(&self, key: &str, value: Option<&T>) {
        let mut data = self.data.lock().unwrap();
        match value.and_then(|v| serde_json::to_value(v).ok()) {
            Some(v) => data.insert(key.to_string(), v),
            None => data.remove(key),
        };
        let plain = serde_json::to_vec(&*data).unwrap_or_default();
        drop(data);
        let Some(bytes) = protect(&plain) else {
            eprintln!("hoverpill: couldn't encrypt account credentials; not saving them");
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("bin.tmp");
        if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &self.path)).is_err() {
            eprintln!("hoverpill: can't save {}", self.path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(windows)]
fn dpapi(input: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let blob_in = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
    let mut blob_out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // CRYPTPROTECT_UI_FORBIDDEN: never show a prompt.
    const UI_FORBIDDEN: u32 = 0x1;
    let ok = unsafe {
        if encrypt {
            CryptProtectData(&blob_in, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut blob_out)
        } else {
            CryptUnprotectData(&blob_in, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut blob_out)
        }
    };
    if ok == 0 || blob_out.pbData.is_null() {
        return None;
    }
    let out = unsafe { std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize).to_vec() };
    unsafe { LocalFree(blob_out.pbData as _) };
    Some(out)
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    dpapi(plain, true)
}

#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> Option<Vec<u8>> {
    dpapi(bytes, false)
}

#[cfg(not(windows))]
fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    Some(plain.to_vec())
}

#[cfg(not(windows))]
fn unprotect(bytes: &[u8]) -> Option<Vec<u8>> {
    Some(bytes.to_vec())
}

// ---------- the browser round trip ----------

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system randomness");
    URL_SAFE_NO_PAD.encode(buf)
}

/// A PKCE verifier and its S256 challenge.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        let verifier = random_token(48);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// A listener on 127.0.0.1 waiting for the provider to send the browser back.
pub struct Loopback {
    listener: TcpListener,
    path: String,
    pub redirect_uri: String,
}

impl Loopback {
    /// `port` 0 picks any free port (Google allows that); OpenAI allows any port but a fixed path.
    pub async fn bind(port: u16, path: &str) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| format!("can't listen for the sign-in: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        Ok(Self { listener, path: path.to_string(), redirect_uri: format!("http://127.0.0.1:{port}{path}") })
    }

    /// Waits for the redirect and returns its query parameters. Other requests the browser
    /// makes (a favicon) are answered and ignored.
    pub async fn wait(self, provider: &str) -> Result<HashMap<String, String>, String> {
        let wait = async {
            loop {
                let (mut stream, _) = self.listener.accept().await.map_err(|e| e.to_string())?;
                let mut buf = vec![0u8; 16 * 1024];
                let mut len = 0;
                let target = loop {
                    let n = stream.read(&mut buf[len..]).await.map_err(|e| e.to_string())?;
                    if n == 0 {
                        break None;
                    }
                    len += n;
                    let mut headers = [httparse::EMPTY_HEADER; 32];
                    let mut req = httparse::Request::new(&mut headers);
                    match req.parse(&buf[..len]) {
                        Ok(httparse::Status::Complete(_)) => break req.path.map(str::to_string),
                        Ok(httparse::Status::Partial) if len < buf.len() => continue,
                        _ => break None,
                    }
                };
                let Some(target) = target else { continue };
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                if path != self.path {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                }
                let params = parse_query(query);
                let failed = params.contains_key("error");
                let page = done_page(provider, failed);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                return Ok(params);
            }
        };
        tokio::time::timeout(SIGN_IN_TIMEOUT, wait).await.map_err(|_| "Sign-in timed out. Try again.".to_string())?
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// What the browser tab shows once it has handed the answer back.
fn done_page(provider: &str, failed: bool) -> String {
    let (title, body) = if failed {
        (format!("{provider} wasn't connected"), "You can close this tab and try again from HoverPill's Settings.".to_string())
    } else {
        (format!("{provider} is connected"), "You can close this tab and go back to what you were doing.".to_string())
    };
    format!(
        "<!doctype html><meta charset=utf-8><title>HoverPill</title>\
         <body style=\"margin:0;display:grid;place-items:center;min-height:100vh;background:#f2f5ee;color:#16261b;font:16px/1.5 'Segoe UI',system-ui,sans-serif\">\
         <div style=\"text-align:center;max-width:32ch\"><h1 style=\"font-weight:600;font-size:24px;margin:0 0 8px\">{title}</h1><p style=\"margin:0;color:#3b4a3f\">{body}</p></div>"
    )
}

/// Opens a URL in the default browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (verb, target) = (wide("open"), wide(url));
        let result = unsafe {
            ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
        };
        // ShellExecute returns a value > 32 on success.
        if result as isize > 32 {
            Ok(())
        } else {
            Err("couldn't open the browser".into())
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ()).map_err(|e| format!("couldn't open the browser: {e}"))
    }
}

/// Builds `base?k=v&…` with every value percent-encoded.
pub fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse(base).expect("static URL");
    url.query_pairs_mut().extend_pairs(params);
    url.into()
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("HoverPill/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("HTTP client")
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads the payload of a JWT without checking its signature. Only for tokens that came straight
/// from the provider's token endpoint over TLS (Google says that's enough), and only for display.
pub fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

// ---------- status for the Settings window ----------

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub connected: bool,
    pub email: Option<String>,
    /// A sign-in or refresh is under way.
    pub busy: bool,
    /// The last thing that went wrong, in words for the user.
    pub error: Option<String>,
    /// ChatGPT only: requests can use the user's ChatGPT plan.
    pub plan_usage: bool,
}

#[derive(Clone, Serialize)]
pub struct AccountsStatus {
    pub google: AccountStatus,
    pub chatgpt: AccountStatus,
}

pub fn status(app: &AppHandle) -> AccountsStatus {
    AccountsStatus { google: app.state::<google::Google>().status(), chatgpt: app.state::<chatgpt::ChatGpt>().status() }
}

/// Tells every window (Settings, the notch) that an account changed.
pub fn broadcast(app: &AppHandle) {
    let _ = app.emit("accounts:changed", status(app));
}

#[tauri::command]
pub fn accounts_status(app: AppHandle) -> AccountsStatus {
    status(&app)
}

/// Opens a calendar event or a video call. Only https links, so the UI can't launch anything else.
#[tauri::command]
pub fn open_link(url: String) -> Result<(), String> {
    match reqwest::Url::parse(&url) {
        Ok(u) if u.scheme() == "https" => open_in_browser(u.as_str()),
        _ => Err("only https links can be opened".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query() {
        let q = parse_query("code=4%2F0Ab&state=x+y&scope=openid%20email");
        assert_eq!(q["code"], "4/0Ab");
        assert_eq!(q["state"], "x y");
        assert_eq!(q["scope"], "openid email");
    }

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let p = Pkce::new();
        assert_eq!(p.challenge, URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes())));
        assert!(p.verifier.len() >= 43);
    }
}
