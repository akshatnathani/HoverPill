//! Google Calendar: read-only access to the user's calendars.
//!
//! Signs in with Google's flow for desktop apps (loopback redirect + PKCE), using HoverPill's own
//! OAuth client ("Desktop app"). Its ID and the secret Google issues with it (which for desktop
//! apps isn't actually secret) are built in at compile time from HOVERPILL_GOOGLE_CLIENT_ID /
//! HOVERPILL_GOOGLE_CLIENT_SECRET; a build without them can't connect Google Calendar.
//!
//! Once connected, upcoming events are fetched every few minutes and sent to the notch, which
//! shows them on Home and drops a reminder card shortly before each one starts.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use super::{http, jwt_claims, now_secs, open_in_browser, random_token, with_query, AccountStatus, Loopback, Pkce, Vault};
use crate::settings::SettingsStore;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API: &str = "https://www.googleapis.com/calendar/v3";
const SCOPES: &str = "openid email https://www.googleapis.com/auth/calendar.readonly";
const VAULT_KEY: &str = "google";

/// How far ahead to look, and how often.
const LOOKAHEAD_DAYS: u64 = 14;
const POLL: Duration = Duration::from_secs(5 * 60);
const REMINDER_TICK: Duration = Duration::from_secs(15);
/// Calendars to read at most (the ones ticked in Google Calendar's own sidebar).
const MAX_CALENDARS: usize = 12;

#[derive(Clone, Serialize, Deserialize)]
struct Creds {
    refresh_token: String,
    /// The refresh token only works with the client that issued it.
    client_id: String,
    email: Option<String>,
}

/// An event as the notch shows it. Times are RFC 3339 strings for the UI; `start_epoch` is for
/// reminders (absent for all-day events).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: Option<String>,
    /// A video call link if there is one, else the event's page in Google Calendar.
    pub link: Option<String>,
    pub calendar: String,
    pub color: Option<String>,
    #[serde(skip)]
    pub start_epoch: Option<u64>,
}

#[derive(Default)]
struct Inner {
    creds: Option<Creds>,
    access: Option<(String, u64)>,
    busy: bool,
    error: Option<String>,
    events: Vec<CalendarEvent>,
    reminded: HashSet<String>,
}

pub struct Google(Mutex<Inner>);

impl Google {
    pub fn load(vault: &Vault) -> Self {
        Self(Mutex::new(Inner { creds: vault.get(VAULT_KEY), ..Inner::default() }))
    }

    pub fn status(&self) -> AccountStatus {
        let s = self.0.lock().unwrap();
        AccountStatus {
            connected: s.creds.is_some(),
            email: s.creds.as_ref().and_then(|c| c.email.clone()),
            busy: s.busy,
            error: s.error.clone(),
            plan_usage: false,
        }
    }

    fn set_error(&self, app: &AppHandle, error: Option<String>) {
        self.0.lock().unwrap().error = error;
        super::broadcast(app);
    }

    pub fn events(&self) -> Vec<CalendarEvent> {
        self.0.lock().unwrap().events.clone()
    }
}

/// The OAuth client built into this build, if any.
fn client() -> Option<(String, String)> {
    let id = option_env!("HOVERPILL_GOOGLE_CLIENT_ID").map(str::trim).filter(|s| !s.is_empty())?;
    let secret = option_env!("HOVERPILL_GOOGLE_CLIENT_SECRET").map(str::trim).unwrap_or_default();
    Some((id.to_string(), secret.to_string()))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

async fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse, (Option<String>, String)> {
    let res = http().post(TOKEN_URL).form(form).send().await.map_err(|e| (None, format!("couldn't reach Google: {e}")))?;
    if res.status().is_success() {
        return res.json().await.map_err(|e| (None, format!("unexpected answer from Google: {e}")));
    }
    let err: Option<TokenError> = res.json().await.ok();
    let code = err.as_ref().map(|e| e.error.clone());
    let text = err.map(|e| e.error_description.unwrap_or(e.error)).unwrap_or_else(|| "Google refused the request".into());
    Err((code, text))
}

/// Signs in through the browser. Returns once the user has finished (or given up).
#[tauri::command]
pub async fn google_connect(app: AppHandle) -> Result<(), String> {
    let google = app.state::<Google>();
    let Some((client_id, client_secret)) = client() else {
        let msg = "This build of HoverPill can't sign in to Google.".to_string();
        google.set_error(&app, Some(msg.clone()));
        return Err(msg);
    };
    {
        let mut s = google.0.lock().unwrap();
        if s.busy {
            return Err("Already waiting for Google in the browser.".into());
        }
        s.busy = true;
        s.error = None;
    }
    super::broadcast(&app);
    let result = sign_in(&client_id, &client_secret).await;
    let mut s = google.0.lock().unwrap();
    s.busy = false;
    match result {
        Ok(creds) => {
            app.state::<Vault>().set(VAULT_KEY, Some(&creds));
            s.creds = Some(creds.0);
            s.access = Some(creds.1);
            s.reminded.clear();
            drop(s);
            super::broadcast(&app);
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move { refresh_events(&app2).await });
            Ok(())
        }
        Err(e) => {
            s.error = Some(e.clone());
            drop(s);
            super::broadcast(&app);
            Err(e)
        }
    }
}

/// Saved as (creds, access token) so the vault stores only the long-lived part.
struct Signed(Creds, (String, u64));

impl Serialize for Signed {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

async fn sign_in(client_id: &str, client_secret: &str) -> Result<Signed, String> {
    let loopback = Loopback::bind(0, "/").await?;
    let pkce = Pkce::new();
    let state = random_token(24);
    let url = with_query(
        AUTH_URL,
        &[
            ("client_id", client_id),
            ("redirect_uri", &loopback.redirect_uri),
            ("response_type", "code"),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            // A refresh token, so HoverPill keeps working without asking again.
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    );
    open_in_browser(&url)?;
    let redirect_uri = loopback.redirect_uri.clone();
    let params = loopback.wait("Google Calendar").await?;
    if params.get("state") != Some(&state) {
        return Err("The sign-in answer didn't match. Try again.".into());
    }
    if let Some(error) = params.get("error") {
        return Err(if error == "access_denied" { "Google access wasn't allowed.".into() } else { format!("Google said: {error}") });
    }
    let code = params.get("code").ok_or("Google didn't send a sign-in code.")?;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }
    let tokens = token_request(&form).await.map_err(|(_, e)| e)?;
    let refresh_token = tokens.refresh_token.ok_or("Google didn't send a refresh token. Remove HoverPill's access in your Google Account and connect again.")?;
    let email = tokens.id_token.as_deref().and_then(jwt_claims).and_then(|c| c.get("email").and_then(Value::as_str).map(str::to_string));
    Ok(Signed(
        Creds { refresh_token, client_id: client_id.to_string(), email },
        (tokens.access_token, now_secs() + tokens.expires_in),
    ))
}

#[tauri::command]
pub async fn google_disconnect(app: AppHandle) -> Result<(), String> {
    let google = app.state::<Google>();
    let token = {
        let mut s = google.0.lock().unwrap();
        let token = s.creds.take().map(|c| c.refresh_token);
        s.access = None;
        s.events.clear();
        s.error = None;
        token
    };
    app.state::<Vault>().set::<Creds>(VAULT_KEY, None);
    crate::events::send(&app, "pill:calendar", Vec::<CalendarEvent>::new());
    super::broadcast(&app);
    // Tell Google too, so the grant disappears from the user's Google Account. Best effort.
    if let Some(token) = token {
        let _ = http().post(REVOKE_URL).form(&[("token", token.as_str())]).send().await;
    }
    Ok(())
}

/// A usable access token, refreshed if it's about to expire.
async fn access_token(app: &AppHandle) -> Result<String, String> {
    let google = app.state::<Google>();
    let (creds, current) = {
        let s = google.0.lock().unwrap();
        (s.creds.clone().ok_or("not connected")?, s.access.clone())
    };
    if let Some((token, expires)) = current {
        if expires > now_secs() + 60 {
            return Ok(token);
        }
    }
    let secret = client().filter(|(id, _)| *id == creds.client_id).map(|(_, s)| s).unwrap_or_default();
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", creds.refresh_token.as_str()), ("client_id", creds.client_id.as_str())];
    if !secret.is_empty() {
        form.push(("client_secret", secret.as_str()));
    }
    match token_request(&form).await {
        Ok(t) => {
            google.0.lock().unwrap().access = Some((t.access_token.clone(), now_secs() + t.expires_in));
            Ok(t.access_token)
        }
        // The user removed access, or the grant expired: only a new sign-in helps.
        Err((Some(code), _)) if code == "invalid_grant" => {
            {
                let mut s = google.0.lock().unwrap();
                s.creds = None;
                s.access = None;
                s.events.clear();
            }
            app.state::<Vault>().set::<Creds>(VAULT_KEY, None);
            crate::events::send(app, "pill:calendar", Vec::<CalendarEvent>::new());
            let msg = "Google Calendar was disconnected. Connect again to see your events.".to_string();
            google.set_error(app, Some(msg.clone()));
            Err(msg)
        }
        Err((_, e)) => Err(e),
    }
}

async fn get_json(token: &str, url: &str) -> Result<Value, String> {
    let res = http().get(url).bearer_auth(token).send().await.map_err(|e| format!("couldn't reach Google Calendar: {e}"))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("request failed");
        return Err(format!("Google Calendar: {msg}"));
    }
    Ok(body)
}

/// Fetches the next two weeks from every calendar ticked in Google Calendar, and sends them on.
pub async fn refresh_events(app: &AppHandle) {
    let google = app.state::<Google>();
    if google.0.lock().unwrap().creds.is_none() {
        return;
    }
    match fetch_events(app).await {
        Ok(events) => {
            {
                let mut s = google.0.lock().unwrap();
                s.events = events.clone();
                s.error = None;
            }
            if app.state::<SettingsStore>().get().calendar.enabled {
                crate::events::send(app, "pill:calendar", events);
            }
            super::broadcast(app);
        }
        Err(e) => {
            eprintln!("hoverpill: calendar: {e}");
            google.set_error(app, Some(e));
        }
    }
}

async fn fetch_events(app: &AppHandle) -> Result<Vec<CalendarEvent>, String> {
    let token = access_token(app).await?;
    let list = get_json(&token, &format!("{API}/users/me/calendarList?minAccessRole=reader")).await?;
    let calendars: Vec<&Value> = list["items"]
        .as_array()
        .map(|items| items.iter().filter(|c| c["selected"].as_bool().unwrap_or(false) || c["primary"].as_bool().unwrap_or(false)).collect())
        .unwrap_or_default();
    // From the start of today (UTC is fine: a few hours early), so today's earlier events show too.
    let now = now_secs();
    let time_min = rfc3339(now - now % 86_400);
    let time_max = rfc3339(now + LOOKAHEAD_DAYS * 86_400);
    let mut events = Vec::new();
    for cal in calendars.into_iter().take(MAX_CALENDARS) {
        let Some(id) = cal["id"].as_str() else { continue };
        let url = with_query(
            &format!("{API}/calendars/{}/events", encode_path(id)),
            &[("timeMin", &time_min), ("timeMax", &time_max), ("singleEvents", "true"), ("orderBy", "startTime"), ("maxResults", "50")],
        );
        let body = match get_json(&token, &url).await {
            Ok(b) => b,
            // One unreadable calendar shouldn't hide the others.
            Err(e) => {
                eprintln!("hoverpill: calendar {id}: {e}");
                continue;
            }
        };
        let name = cal["summaryOverride"].as_str().or(cal["summary"].as_str()).unwrap_or("Calendar").to_string();
        let color = cal["backgroundColor"].as_str().map(str::to_string);
        for e in body["items"].as_array().into_iter().flatten() {
            if e["status"] == "cancelled" {
                continue;
            }
            // Skip events the user declined.
            let declined = e["attendees"].as_array().into_iter().flatten().any(|a| a["self"] == true && a["responseStatus"] == "declined");
            if declined {
                continue;
            }
            let (start, all_day) = match (e["start"]["dateTime"].as_str(), e["start"]["date"].as_str()) {
                (Some(t), _) => (t.to_string(), false),
                (None, Some(d)) => (d.to_string(), true),
                _ => continue,
            };
            let end = e["end"]["dateTime"].as_str().or(e["end"]["date"].as_str()).unwrap_or(&start).to_string();
            let call = e["hangoutLink"]
                .as_str()
                .or_else(|| e["conferenceData"]["entryPoints"].as_array()?.iter().find(|p| p["entryPointType"] == "video")?["uri"].as_str());
            events.push(CalendarEvent {
                id: format!("{id}/{}", e["id"].as_str().unwrap_or_default()),
                title: e["summary"].as_str().unwrap_or("(No title)").to_string(),
                start_epoch: if all_day { None } else { parse_rfc3339(&start) },
                start,
                end,
                all_day,
                location: e["location"].as_str().map(str::to_string),
                link: call.or(e["htmlLink"].as_str()).map(str::to_string),
                calendar: name.clone(),
                color: color.clone(),
            });
        }
    }
    events.sort_by(|a, b| a.start_epoch.unwrap_or(0).cmp(&b.start_epoch.unwrap_or(0)).then(a.start.cmp(&b.start)));
    Ok(events)
}

/// Polls for events, and drops a reminder card before each one starts.
pub fn spawn(app: AppHandle) {
    let poller = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_events(&poller).await;
            tokio::time::sleep(POLL).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(REMINDER_TICK).await;
            remind(&app);
        }
    });
}

fn remind(app: &AppHandle) {
    let settings = app.state::<SettingsStore>().get().calendar;
    if !settings.enabled || settings.reminder_minutes <= 0.0 {
        return;
    }
    let lead = (settings.reminder_minutes * 60.0) as u64;
    let now = now_secs();
    let due: Vec<CalendarEvent> = {
        let google = app.state::<Google>();
        let mut s = google.0.lock().unwrap();
        let due: Vec<CalendarEvent> = s
            .events
            .iter()
            .filter(|e| e.start_epoch.is_some_and(|t| now + lead >= t && now < t) && !s.reminded.contains(&e.id))
            .cloned()
            .collect();
        s.reminded.extend(due.iter().map(|e| e.id.clone()));
        due
    };
    for e in due {
        let mins = e.start_epoch.unwrap_or(now).saturating_sub(now).div_ceil(60);
        let when = if mins <= 1 { "Starts in a minute".to_string() } else { format!("Starts in {mins} minutes") };
        let body = match &e.location {
            Some(place) => format!("{when}, {place}"),
            None => when,
        };
        crate::events::send(
            app,
            "pill:notification",
            crate::events::NotificationItem { id: crate::events::next_id("calendar"), app: "Calendar".into(), title: e.title, body, icon: None },
        );
    }
}

/// What the notch shows when it loads; later changes arrive as `pill:calendar`.
#[tauri::command]
pub fn calendar_events(google: tauri::State<Google>, settings: tauri::State<SettingsStore>) -> Vec<CalendarEvent> {
    if settings.get().calendar.enabled {
        google.events()
    } else {
        Vec::new()
    }
}

/// Asks for fresh events now (e.g. after changing settings).
#[tauri::command]
pub async fn calendar_refresh(app: AppHandle) {
    refresh_events(&app).await;
}

fn encode_path(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------- RFC 3339 without a date library ----------

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// UTC time as `2026-10-18T09:30:00Z`.
fn rfc3339(epoch: u64) -> String {
    let (y, m, d) = civil_from_days((epoch / 86_400) as i64);
    let s = epoch % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// Seconds since the epoch for `2026-10-18T10:30:00+05:30` / `…Z` / with fractional seconds.
fn parse_rfc3339(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let rest = s.get(19..)?.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let oh: i64 = o.get(1..3)?.parse().ok()?;
            let om: i64 = o.get(4..6)?.parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
    };
    let t = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset;
    u64::try_from(t).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trip() {
        let t = 1_792_312_200; // 2026-10-18T08:30:00Z
        assert_eq!(rfc3339(t), "2026-10-18T08:30:00Z");
        assert_eq!(parse_rfc3339("2026-10-18T08:30:00Z"), Some(t));
        assert_eq!(parse_rfc3339("2026-10-18T14:00:00+05:30"), Some(t));
        assert_eq!(parse_rfc3339("2026-10-18T04:30:00.000-04:00"), Some(t));
    }

    #[test]
    fn encodes_calendar_ids() {
        assert_eq!(encode_path("en.indian#holiday@group.v.calendar.google.com"), "en.indian%23holiday%40group.v.calendar.google.com");
    }
}
