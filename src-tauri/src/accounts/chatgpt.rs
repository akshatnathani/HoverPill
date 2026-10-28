//! ChatGPT, through "Sign in with ChatGPT" for open-source apps.
//!
//! The user signs in with their ChatGPT account and allows HoverPill to use their plan
//! (Plus or Pro) for requests; there's no API key and no client secret. On the first sign-in
//! OpenAI registers HoverPill as a client for this machine and hands back a client ID, which is
//! kept and reused. Requests go to the public Responses API with the OAuth access token, always
//! streamed and never stored, and count against the plan's own usage limits.
//!
//! Contract: https://developers.openai.com/siwc/token-sharing-open-source/sign-in

use std::collections::HashMap;
use std::sync::Mutex;

use futures_util::StreamExt;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::{http, now_secs, open_in_browser, random_token, with_query, AccountStatus, Loopback, Pkce, Vault};
use crate::settings::SettingsStore;

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// Only the port may vary; scheme, host and path are fixed by OpenAI.
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_PORT: u16 = 1455;
const APP_NAME: &str = "HoverPill";
const VAULT_KEY: &str = "chatgpt";
/// Where people manage plan usage and connected apps.
pub const USAGE_URL: &str = "https://chatgpt.com/#settings/Usage";

/// How to answer: as well and as fully as ChatGPT itself. The notch renders the usual Markdown
/// (see src/pill/markdown.tsx); it's a small panel, so big headings are discouraged.
const INSTRUCTIONS: &str = "You are ChatGPT, answering inside HoverPill, a small panel on the user's screen. \
Give a genuinely helpful, accurate and complete answer, as you would in ChatGPT: explain the why when it helps, \
include the steps, examples or code the question needs, and match the length to the question, so a simple \
question gets a short answer and a hard one gets a full one. Don't pad or repeat the question. \
Format with Markdown as you normally would: short paragraphs, bullet and numbered lists, **bold** for key terms, \
`inline code`, fenced code blocks, and tables when they genuinely help. The panel is narrow, so prefer lists to \
wide tables, and use at most small headings. \
When a message comes with a screenshot, it is the user's screen right now, shared so you can help with what they're \
doing: ground your answer in it, refer to what's visible by its label or where it is, and give concrete steps \
(what to click, where, and what to type). If something you need isn't visible, say what to open or show.";

#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    /// Issued by OpenAI on the first sign-in; reused for every later one.
    client_id: Option<String>,
    /// Identifies this machine to OpenAI; stays the same across sign-ins.
    host_id: String,
    email: Option<String>,
    refresh_token: Option<String>,
    access_token: Option<String>,
    expires_at: u64,
    scopes: Vec<String>,
}

impl Saved {
    fn signed_in(&self) -> bool {
        self.refresh_token.is_some() || self.access_token.is_some()
    }

    fn plan_usage(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }
}

#[derive(Default)]
struct Inner {
    saved: Saved,
    busy: bool,
    error: Option<String>,
    /// Requests in flight, by the UI's id, so they can be stopped.
    running: HashMap<String, tokio::sync::oneshot::Sender<()>>,
}

pub struct ChatGpt(Mutex<Inner>);

impl ChatGpt {
    pub fn load(vault: &Vault) -> Self {
        let mut saved: Saved = vault.get(VAULT_KEY).unwrap_or_default();
        if saved.host_id.is_empty() {
            saved.host_id = format!("urn:uuid:{}", uuid_v4());
            vault.set(VAULT_KEY, Some(&saved));
        }
        Self(Mutex::new(Inner { saved, ..Inner::default() }))
    }

    pub fn status(&self) -> AccountStatus {
        let s = self.0.lock().unwrap();
        AccountStatus {
            connected: s.saved.signed_in(),
            email: s.saved.email.clone(),
            busy: s.busy,
            error: s.error.clone(),
            plan_usage: s.saved.plan_usage(),
        }
    }

    fn save(&self, app: &AppHandle) {
        let saved = self.0.lock().unwrap().saved.clone();
        app.state::<Vault>().set(VAULT_KEY, Some(&saved));
    }

    fn set_error(&self, app: &AppHandle, error: Option<String>) {
        self.0.lock().unwrap().error = error;
        super::broadcast(app);
    }

    /// Forgets the tokens but keeps the registration and host id, as OpenAI asks.
    fn forget_tokens(&self, app: &AppHandle) {
        {
            let mut s = self.0.lock().unwrap();
            s.saved.refresh_token = None;
            s.saved.access_token = None;
            s.saved.expires_at = 0;
            s.saved.scopes.clear();
        }
        self.save(app);
    }
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    id_token: Option<String>,
    scope: Option<String>,
}

async fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse, (Option<String>, String)> {
    let res = http()
        .post(TOKEN_URL)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| (None, format!("couldn't reach OpenAI: {e}")))?;
    if res.status().is_success() {
        return res.json().await.map_err(|e| (None, format!("unexpected answer from OpenAI: {e}")));
    }
    let body: Value = res.json().await.unwrap_or(Value::Null);
    let code = body["error"].as_str().map(str::to_string);
    let text = body["error_description"].as_str().or(code.as_deref()).unwrap_or("OpenAI refused the request").to_string();
    Err((code, text))
}

/// Checks the ID token's signature against OpenAI's published keys, and its issuer, audience,
/// expiry and nonce. Returns its claims.
async fn verify_id_token(id_token: &str, client_id: &str, nonce: &str) -> Result<Value, String> {
    let fail = |e: &dyn std::fmt::Display| format!("couldn't verify the sign-in: {e}");
    let discovery: Value = http()
        .get(format!("{ISSUER}/.well-known/openid-configuration"))
        .send()
        .await
        .map_err(|e| fail(&e))?
        .json()
        .await
        .map_err(|e| fail(&e))?;
    let jwks_uri = discovery["jwks_uri"].as_str().ok_or_else(|| fail(&"no key set published"))?;
    if !jwks_uri.starts_with(ISSUER) {
        return Err(fail(&"key set is on an unexpected host"));
    }
    let keys: JwkSet = http().get(jwks_uri).send().await.map_err(|e| fail(&e))?.json().await.map_err(|e| fail(&e))?;
    check_id_token(id_token, &keys, client_id, nonce)
}

/// The offline half of `verify_id_token`: signature against the key set, then issuer, audience,
/// expiry and nonce.
fn check_id_token(id_token: &str, keys: &JwkSet, client_id: &str, nonce: &str) -> Result<Value, String> {
    let fail = |e: &dyn std::fmt::Display| format!("couldn't verify the sign-in: {e}");
    let header = decode_header(id_token).map_err(|e| fail(&e))?;
    let jwk = match &header.kid {
        Some(kid) => keys.find(kid),
        None => keys.keys.first(),
    }
    .ok_or_else(|| fail(&"unknown signing key"))?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| fail(&e))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.leeway = 5;
    let claims = decode::<Value>(id_token, &key, &validation).map_err(|e| fail(&e))?.claims;
    if claims["nonce"].as_str() != Some(nonce) {
        return Err(fail(&"nonce mismatch"));
    }
    Ok(claims)
}

/// Signs in through the browser. `reconsent` asks again for every permission, e.g. to turn on
/// plan usage after it was declined.
#[tauri::command]
pub async fn chatgpt_connect(app: AppHandle, reconsent: Option<bool>) -> Result<(), String> {
    let chatgpt = app.state::<ChatGpt>();
    let previous = {
        let mut s = chatgpt.0.lock().unwrap();
        if s.busy {
            return Err("Already waiting for ChatGPT in the browser.".into());
        }
        s.busy = true;
        s.error = None;
        s.saved.clone()
    };
    super::broadcast(&app);
    let result = sign_in(&previous, reconsent.unwrap_or(false)).await;
    {
        let mut s = chatgpt.0.lock().unwrap();
        s.busy = false;
        match &result {
            Ok(saved) => s.saved = saved.clone(),
            Err(e) => s.error = Some(e.clone()),
        }
    }
    if result.is_ok() {
        chatgpt.save(&app);
    }
    super::broadcast(&app);
    result.map(|_| ())
}

async fn sign_in(previous: &Saved, reconsent: bool) -> Result<Saved, String> {
    // OpenAI allows any port; prefer the usual one, fall back to any free one.
    let loopback = match Loopback::bind(CALLBACK_PORT, CALLBACK_PATH).await {
        Ok(l) => l,
        Err(_) => Loopback::bind(0, CALLBACK_PATH).await?,
    };
    let pkce = Pkce::new();
    let state = random_token(24);
    let nonce = random_token(24);
    let client_id = previous.client_id.clone().unwrap_or_else(|| "dynamic_agent_client".into());
    let mut params: Vec<(&str, &str)> = vec![
        ("client_id", &client_id),
        ("response_type", "code"),
        ("redirect_uri", &loopback.redirect_uri),
        ("scope", SCOPES),
        ("resource", RESOURCE),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &pkce.challenge),
        ("ext_agent_host_id", &previous.host_id),
    ];
    if previous.client_id.is_none() {
        params.push(("agent_name_hint", APP_NAME));
    }
    if let Some(email) = &previous.email {
        params.push(("login_hint", email));
    }
    if reconsent {
        params.push(("prompt", "consent"));
    }
    open_in_browser(&with_query(AUTHORIZE_URL, &params))?;
    let redirect_uri = loopback.redirect_uri.clone();
    let reply = loopback.wait("ChatGPT").await?;
    if reply.get("state") != Some(&state) {
        return Err("The sign-in answer didn't match. Try again.".into());
    }
    if let Some(error) = reply.get("error") {
        return Err(if error == "access_denied" { "ChatGPT access wasn't allowed.".into() } else { format!("OpenAI said: {error}") });
    }
    let code = reply.get("code").ok_or("OpenAI didn't send a sign-in code.")?;
    // The first sign-in issues a client ID; later ones must keep the same one.
    let issued = match (&previous.client_id, reply.get("client_id")) {
        (Some(saved), Some(got)) if saved != got => return Err("OpenAI answered for a different registration. Try again.".into()),
        (Some(saved), _) => saved.clone(),
        (None, Some(got)) => got.clone(),
        (None, None) => return Err("OpenAI didn't register HoverPill. Try again.".into()),
    };
    // Kept before the exchange, so a retry after a failed exchange reuses the registration.
    let mut saved = Saved { client_id: Some(issued.clone()), ..previous.clone() };
    let tokens = token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", &issued),
        ("code", code),
        ("code_verifier", &pkce.verifier),
        ("redirect_uri", &redirect_uri),
        ("resource", RESOURCE),
    ])
    .await
    .map_err(|(code, e)| if code.as_deref() == Some("invalid_grant") { "The sign-in code expired. Try again.".to_string() } else { e })?;
    let id_token = tokens.id_token.as_deref().ok_or("OpenAI didn't send an ID token.")?;
    let claims = verify_id_token(id_token, &issued, &nonce).await?;
    saved.email = claims["email"].as_str().map(str::to_string).or(saved.email);
    saved.access_token = Some(tokens.access_token);
    saved.refresh_token = tokens.refresh_token.or(saved.refresh_token);
    saved.expires_at = now_secs() + tokens.expires_in;
    saved.scopes = tokens.scope.unwrap_or_default().split_whitespace().map(str::to_string).collect();
    Ok(saved)
}

#[tauri::command]
pub fn chatgpt_disconnect(app: AppHandle) {
    let chatgpt = app.state::<ChatGpt>();
    chatgpt.forget_tokens(&app);
    {
        let mut s = chatgpt.0.lock().unwrap();
        s.error = None;
        s.saved.email = None;
    }
    chatgpt.save(&app);
    super::broadcast(&app);
}

/// A usable access token, refreshed when it's close to expiring.
async fn access_token(app: &AppHandle) -> Result<String, String> {
    let chatgpt = app.state::<ChatGpt>();
    let saved = chatgpt.0.lock().unwrap().saved.clone();
    if !saved.signed_in() {
        return Err("Sign in with ChatGPT in HoverPill's Settings first.".into());
    }
    if let Some(token) = saved.access_token.as_ref().filter(|_| saved.expires_at > now_secs() + 60) {
        return Ok(token.clone());
    }
    let (Some(client_id), Some(refresh)) = (saved.client_id.as_deref(), saved.refresh_token.as_deref()) else {
        return Err("Sign in with ChatGPT again.".into());
    };
    match token_request(&[("grant_type", "refresh_token"), ("client_id", client_id), ("refresh_token", refresh), ("resource", RESOURCE)]).await {
        Ok(t) => {
            {
                // The access token, its expiry, the scopes and the rotated refresh token change together.
                let mut s = chatgpt.0.lock().unwrap();
                s.saved.access_token = Some(t.access_token.clone());
                s.saved.expires_at = now_secs() + t.expires_in;
                if let Some(r) = t.refresh_token {
                    s.saved.refresh_token = Some(r);
                }
                if let Some(scope) = t.scope {
                    s.saved.scopes = scope.split_whitespace().map(str::to_string).collect();
                }
            }
            chatgpt.save(app);
            Ok(t.access_token)
        }
        Err((Some(code), _))
            if matches!(
                code.as_str(),
                "invalid_grant" | "invalid_refresh_token" | "token_expired" | "refresh_token_expired" | "refresh_token_invalidated" | "refresh_token_reused"
            ) =>
        {
            chatgpt.forget_tokens(app);
            let msg = "ChatGPT was disconnected. Sign in again.".to_string();
            chatgpt.set_error(app, Some(msg.clone()));
            Err(msg)
        }
        // A network hiccup or an outage: keep the credentials and let the user retry.
        Err((_, e)) => Err(e),
    }
}

#[derive(Serialize)]
pub struct ModelInfo {
    id: String,
    label: String,
}

/// Models this account can use, for the picker in Settings.
#[tauri::command]
pub async fn chatgpt_models(app: AppHandle) -> Result<Vec<ModelInfo>, String> {
    let token = access_token(&app).await?;
    let body: Value = http()
        .get(format!("{RESOURCE}/models"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("couldn't reach OpenAI: {e}"))?
        .json()
        .await
        .map_err(|e| format!("unexpected answer from OpenAI: {e}"))?;
    let items = body["data"].as_array().or(body["models"].as_array()).cloned().unwrap_or_default();
    Ok(items
        .iter()
        .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
        .filter_map(|m| {
            let id = m["slug"].as_str().or(m["id"].as_str())?.to_string();
            let label = m["display_name"].as_str().or(m["title"].as_str()).unwrap_or(&id).to_string();
            Some(ModelInfo { id, label })
        })
        .collect())
}

#[derive(Deserialize)]
pub struct Turn {
    role: String,
    text: String,
    /// A screenshot sent with this question (an id from `capture_screen`).
    #[serde(default)]
    screen: Option<String>,
}

/// Streamed back to the notch on `pill:chatgpt`.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Chunk {
    Delta { id: String, text: String },
    Done { id: String },
    Error { id: String, message: String, usage: bool },
}

/// Sends a conversation and streams the answer to the notch. `id` names this request so the UI
/// can match chunks to it and stop it.
#[tauri::command]
pub async fn chatgpt_ask(app: AppHandle, id: String, turns: Vec<Turn>) -> Result<(), String> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    app.state::<ChatGpt>().0.lock().unwrap().running.insert(id.clone(), stop_tx);
    let outcome = tokio::select! {
        r = ask(&app, &id, turns) => r,
        _ = stop_rx => Ok(()),
    };
    app.state::<ChatGpt>().0.lock().unwrap().running.remove(&id);
    let chunk = match outcome {
        Ok(()) => Chunk::Done { id },
        Err((message, usage)) => Chunk::Error { id, message, usage },
    };
    let _ = app.emit("pill:chatgpt", chunk);
    Ok(())
}

#[tauri::command]
pub fn chatgpt_stop(app: AppHandle, id: String) {
    if let Some(stop) = app.state::<ChatGpt>().0.lock().unwrap().running.remove(&id) {
        let _ = stop.send(());
    }
}

/// The complete lines at the front of `buf`, decoded, leaving any partial line behind. Network
/// chunks can end in the middle of a character (anything outside ASCII is several bytes), so
/// bytes are only decoded once their whole line has arrived.
fn take_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let Some(last) = buf.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
    let rest = buf.split_off(last + 1);
    let lines = String::from_utf8_lossy(buf).lines().map(str::to_string).collect();
    *buf = rest;
    lines
}

/// Errors carry a message for the user, and whether it's the plan's usage limit.
async fn ask(app: &AppHandle, id: &str, turns: Vec<Turn>) -> Result<(), (String, bool)> {
    let plain = |m: String| (m, false);
    if !app.state::<ChatGpt>().status().plan_usage && app.state::<ChatGpt>().status().connected {
        return Err(plain("ChatGPT plan usage isn't turned on for HoverPill. Turn it on in Settings, Accounts.".into()));
    }
    let token = access_token(app).await.map_err(plain)?;
    let model = match app.state::<SettingsStore>().get().chatgpt.model {
        Some(m) => m,
        None => chatgpt_models(app.clone()).await.map_err(plain)?.into_iter().next().map(|m| m.id).ok_or_else(|| plain("No ChatGPT models are available to this account.".into()))?,
    };
    let screens = app.state::<crate::screen::Screens>();
    let input: Vec<Value> = turns
        .into_iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| match t.screen.as_deref().filter(|_| t.role != "assistant").and_then(|id| screens.get(id)) {
            Some(image) => json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": t.text }, { "type": "input_image", "image_url": image }],
            }),
            None => json!({ "role": if t.role == "assistant" { "assistant" } else { "user" }, "content": t.text }),
        })
        .collect();
    let res = http()
        .post(format!("{RESOURCE}/responses"))
        .bearer_auth(token)
        .json(&json!({ "model": model, "instructions": INSTRUCTIONS, "input": input, "store": false, "stream": true }))
        .send()
        .await
        .map_err(|e| plain(format!("couldn't reach OpenAI: {e}")))?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        return Err(explain(status, body["error"]["code"].as_str(), body["error"]["message"].as_str().or(body["detail"].as_str())));
    }

    // Server-sent events: `data: {...}` lines, one JSON event each.
    let mut stream = res.bytes_stream();
    let mut buf = Vec::new();
    let mut completed = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| plain(format!("the answer was cut off: {e}")))?;
        buf.extend_from_slice(&chunk);
        for line in take_lines(&mut buf) {
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            if data == "[DONE]" {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
            match event["type"].as_str().unwrap_or_default() {
                "response.output_text.delta" => {
                    let text = event["delta"].as_str().unwrap_or_default().to_string();
                    let _ = app.emit("pill:chatgpt", Chunk::Delta { id: id.to_string(), text });
                }
                "response.completed" => completed = true,
                "response.failed" | "error" => {
                    let err = if event["type"] == "error" { &event } else { &event["response"]["error"] };
                    return Err(explain(0, err["code"].as_str(), err["message"].as_str()));
                }
                "response.incomplete" => return Err(plain("The answer stopped early. Try asking again.".into())),
                _ => {}
            }
        }
    }
    if completed {
        Ok(())
    } else {
        Err(plain("The answer was cut off. Try asking again.".into()))
    }
}

/// Turns OpenAI's error codes into what the user should know or do.
fn explain(status: u16, code: Option<&str>, message: Option<&str>) -> (String, bool) {
    let text = match code {
        Some("subscription_sharing_usage_limit_exceeded") => return ("You've reached your ChatGPT plan's usage limit. Check Usage in ChatGPT settings.".into(), true),
        Some("subscription_sharing_user_not_eligible") => "This ChatGPT account can't share its plan with apps. Plus and Pro can.".to_string(),
        Some("subscription_sharing_usage_unavailable") | Some("subscription_sharing_user_unavailable") => "ChatGPT plan usage is unavailable right now. Try again in a little while.".to_string(),
        Some("subscription_sharing_unsupported_capability") => "That request isn't supported with a ChatGPT plan. Try another model in Settings.".to_string(),
        Some("subscription_sharing_invalid_user") => "ChatGPT didn't accept this sign-in. Try signing in again in Settings.".to_string(),
        _ => match status {
            401 => "ChatGPT didn't accept this sign-in. Try signing in again in Settings.".to_string(),
            403 => format!("ChatGPT refused the request{}", message.map(|m| format!(": {m}")).unwrap_or_default()),
            503 => "ChatGPT is unavailable right now. Try again in a little while.".to_string(),
            _ => message.map(str::to_string).unwrap_or_else(|| "ChatGPT couldn't answer. Try again.".into()),
        },
    };
    (text, false)
}

#[tauri::command]
pub fn chatgpt_open_usage() -> Result<(), String> {
    open_in_browser(USAGE_URL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn host_ids_are_v4_uuids() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }

    /// A throwaway 2048-bit RSA key, used only to sign tokens in these tests.
    const TEST_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEogIBAAKCAQEAla/+X4mCzU3PfhnyoTdBKcUPlWy5RRBuFEr/envAmKWPefVI\nLGae/JXhhc1IdE3UAeVDGLCBk8FBJr+6qp4Xw/bEqUfiusWyLWsGiigZh0uLgQTA\nASEOA7VnoD0rQwYJCi2UxFtKfLbkNFJ+Lbjb5hyDUS22PN6SoiLQQBiWuJlvKmel\nqRQBWZDxnBCKHzAggAkZc7+pFqC9crEokhU6peq64UldyDX/zRoggpb+wEVpmUcI\nYCYT0KI+GWQ8vDp0bwdh7pKrYD6kSQ2O99lETLcaTcd9F0F/5+9pL0Ff+M4Ijo6Y\na3vwV7MYhwX5OtYRY+XF2t3zl6ZKNCicm8R/gwIDAQABAoIBACQ4lpqiqUqpufxf\nJgWZ28RSQY+cZAaQz7pMXZ7TZErrwRi3YFQSLwWa9RwODSrLjHI6+m32hBxvZqcM\nPQy9Pa33QCZgVT5xyUhRPh9fTbId9y9sqVN8nqoKEv3E33uT8csLNZUALDmafgUl\nMxA4S8nWe3ZA0/jNuPjP6MmhyZL4ex62F+oY1uSOEPMYkvY2rGhDrM58292JGuja\ntuNfJRE7hdUAlhB9mHx7gBHYilu7kVCbzh9sVRKu3kRXieYRAC6Hw+uq8NY9XztL\nOvD9ngfrmvpz0mEtDQJC0gPj968pj+8KcgjeYVthBVpzwqkCf0A0q8zbVt0fheWQ\nHwgQQXUCgYEAxUzghcs/5c1BeWfxRIQkvY9SNuSobAMKdZTGtdnvC+riGXwqUlbZ\n+Gy1WMoz2AhhPxfnjK5dCn62SBQDRG1v0qvg9xvSm0AKcQTcORl3vATJXUfRH9cO\nQZSyo4/PHZJQJ7p+SzqCtoDV2Bm1KNxIcHz7MRwBwTbr5KoZ+TKsZ90CgYEAwji/\nRFiZJXYhhOOiSy+3lmNmOf5cM+zefPop5LGkoTmhwnibXfrBiv7LdJJFwIGKL32a\nCY2qpdDWSkhoG/cncuIay5eUyL9mhIjQ84X9hw/ECxKW15jI4kYqHVK0M4wC9zd7\nF77gu9UyCavTohFKm440fZabHRovohTR8KSAvt8CgYBMDsa+zQtk2ytSUe+AKjC/\nonBfGOaUPNfbW+7+S3ULcxCWInduJm0Ah2ehQtGZhiF24RAx+0WCtjYuswhPaKCi\n5f9jQyZ9EsLzrpv2pwqxQGcfvn+vdofYYCo9Q0Lkc3LS3eGxvmyFTkUheUeM6N5D\n/uoWlGVYxoNJMwmwNwifQQKBgDD4+Bl2sZ8Fi2wjrc+Bh6wvAh7LaVpadrtSpO+a\nb2WLz6LmoF5xFQ5EMu8A1v+0ajCRXh7cjtPitbXjTgUHK/ZG1vTREt4PjP0MNovB\nFG1S517XYBua1cRgZswgB3ob6r+FSM9y/N2/4y8XGXT7ITHaCJhGOnR7HtmioFEq\nWaBHAoGAdgrSUlpcvvTz03W1WgDbR4z1CLelVQLOLS/kpnx9NkIRk8FyUC0OZ3EZ\nbytzfMhOziOYr4SpoyFqInE5ACud7jE/MmFMoOqbltuQtuJTCngGkRE6X7bh69Ig\nc/dWyD0Ve1OINvfuyJzmgfKQmO6Idc0akQRmf2cFCDNy2GE2hZ4=\n-----END RSA PRIVATE KEY-----";
    const TEST_N: &str = "la_-X4mCzU3PfhnyoTdBKcUPlWy5RRBuFEr_envAmKWPefVILGae_JXhhc1IdE3UAeVDGLCBk8FBJr-6qp4Xw_bEqUfiusWyLWsGiigZh0uLgQTAASEOA7VnoD0rQwYJCi2UxFtKfLbkNFJ-Lbjb5hyDUS22PN6SoiLQQBiWuJlvKmelqRQBWZDxnBCKHzAggAkZc7-pFqC9crEokhU6peq64UldyDX_zRoggpb-wEVpmUcIYCYT0KI-GWQ8vDp0bwdh7pKrYD6kSQ2O99lETLcaTcd9F0F_5-9pL0Ff-M4Ijo6Ya3vwV7MYhwX5OtYRY-XF2t3zl6ZKNCicm8R_gw";

    fn signed_token(claims: Value) -> String {
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some("test-key".into());
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_KEY.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    fn test_keys() -> JwkSet {
        serde_json::from_value(json!({ "keys": [{ "kty": "RSA", "kid": "test-key", "alg": "RS256", "use": "sig", "n": TEST_N, "e": "AQAB" }] })).unwrap()
    }

    fn claims(aud: &str, nonce: &str) -> Value {
        json!({ "iss": ISSUER, "aud": aud, "sub": "user-1", "email": "a@example.com", "nonce": nonce, "exp": now_secs() + 600 })
    }

    #[test]
    fn a_character_split_across_chunks_survives() {
        let event = "data: {\"delta\":\"café ✓\"}\n";
        let bytes = event.as_bytes();
        // Cut inside the "é" (two bytes) and again inside the "✓" (three).
        let cut = event.find('é').unwrap() + 1;
        let mut buf = bytes[..cut].to_vec();
        assert!(take_lines(&mut buf).is_empty());
        buf.extend_from_slice(&bytes[cut..bytes.len() - 4]);
        assert!(take_lines(&mut buf).is_empty());
        buf.extend_from_slice(&bytes[bytes.len() - 4..]);
        assert_eq!(take_lines(&mut buf), vec![event.trim_end().to_string()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn keeps_the_partial_line_for_later() {
        let mut buf = b"data: one\r\ndata: two\ndata: thr".to_vec();
        assert_eq!(take_lines(&mut buf), vec!["data: one", "data: two"]);
        assert_eq!(buf, b"data: thr");
    }

    #[test]
    fn accepts_a_valid_id_token() {
        let token = signed_token(claims("client-1", "n-1"));
        let got = check_id_token(&token, &test_keys(), "client-1", "n-1").unwrap();
        assert_eq!(got["email"], "a@example.com");
    }

    #[test]
    fn rejects_wrong_audience_nonce_or_signature() {
        let keys = test_keys();
        assert!(check_id_token(&signed_token(claims("someone-else", "n-1")), &keys, "client-1", "n-1").is_err());
        assert!(check_id_token(&signed_token(claims("client-1", "other")), &keys, "client-1", "n-1").is_err());
        // Tamper with the payload: the signature no longer matches.
        let token = signed_token(claims("client-1", "n-1"));
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims("client-1", "n-1").to_string().replace("user-1", "user-2"));
        parts[1] = &forged;
        assert!(check_id_token(&parts.join("."), &keys, "client-1", "n-1").is_err());
    }

    #[test]
    fn usage_limit_is_flagged() {
        assert!(explain(429, Some("subscription_sharing_usage_limit_exceeded"), None).1);
        assert!(!explain(503, None, None).1);
    }
}
