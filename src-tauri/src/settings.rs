//! User settings: one JSON file in the app config dir, owned by Rust. The settings window
//! edits them through `set_settings`; every window hears about changes via `settings:changed`.

use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

/// Field names match src/lib/types.ts (`Settings`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub display: Display,
    pub notifications: Notifications,
    pub media: Media,
    pub calendar: Calendar,
    pub chatgpt: ChatGpt,
    pub updates: Updates,
    pub privacy: Privacy,
    pub system: System,
}

/// The camera/microphone dots in the closed notch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Privacy {
    /// A green dot while any app uses the camera.
    pub camera_indicator: bool,
    /// An orange dot while any app uses the microphone.
    pub microphone_indicator: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Self { camera_indicator: true, microphone_indicator: true }
    }
}

/// The volume, brightness and charging cards that replace the system's own pop-up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct System {
    /// A level bar (and mute state) when the volume changes.
    pub show_volume: bool,
    /// A level bar when the screen brightness changes (laptops).
    pub show_brightness: bool,
    /// A card when the charger is plugged in, and a warning when the battery runs low.
    pub show_battery: bool,
}

impl Default for System {
    fn default() -> Self {
        Self { show_volume: true, show_brightness: true, show_battery: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Updates {
    /// Look for a new version now and then, and get it ready to install (see `updater.rs`).
    pub auto_check: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Display {
    /// Monitor name as the OS reports it; `None` follows the primary monitor.
    pub monitor: Option<String>,
    /// Gap between the top of the screen and the notch, in px. 0 attaches it to the edge.
    pub top_offset: f64,
    /// Size multiplier for the whole pill.
    pub scale: f64,
    /// Also open when the pointer rests on the notch; otherwise only pushing the pointer
    /// against the top edge opens it, and pointing at the closed notch fades it out instead.
    pub open_on_hover: bool,
    /// With nothing playing and no card showing, shrink to a thin bar.
    pub compact_idle: bool,
    /// Get out of the way while a fullscreen app covers the notch's monitor.
    pub hide_in_fullscreen: bool,
    /// Install and enable the GNOME Shell helper that spots native Wayland fullscreen apps
    /// (GNOME only, off by default): it writes into the user's extensions directory, which is the
    /// desktop's business until the user asks for it.
    pub gnome_helper: bool,
    /// Reserve a strip at the top of the monitor (like a taskbar) so maximised windows start
    /// below the notch. Windows only.
    pub reserve_space: bool,
    /// Show the pill on every virtual desktop / workspace, not just the one it was opened on.
    /// Linux only (a sticky window), on by default there; ignored on Windows.
    pub all_workspaces: bool,
    /// Keys that open the notch from any app, like "Super+Alt+Space"; empty for none.
    pub shortcut: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Notifications {
    pub enabled: bool,
    /// How long a card stays before collapsing.
    pub duration_secs: f64,
    /// App names (as shown on the card), compared case-insensitively.
    pub blocked_apps: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Media {
    pub enabled: bool,
    /// Player source ids (see `hoverpill_media::Source`).
    pub ignored_players: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Calendar {
    /// Show events on Home and drop reminders (while Google Calendar is connected).
    pub enabled: bool,
    /// Minutes before an event starts to drop a reminder; 0 turns reminders off.
    pub reminder_minutes: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChatGpt {
    /// Model slug; `None` uses the first one the account offers.
    pub model: Option<String>,
    /// Keys that open the ChatGPT tab from any app, like "Ctrl+Alt+G"; empty for none.
    pub shortcut: String,
}

impl Default for ChatGpt {
    fn default() -> Self {
        Self { model: None, shortcut: "Ctrl+Alt+G".into() }
    }
}

pub const TOP_OFFSET_RANGE: (f64, f64) = (0.0, 48.0);
pub const SCALE_RANGE: (f64, f64) = (0.8, 1.4);
pub const DURATION_RANGE: (f64, f64) = (2.0, 15.0);
pub const REMINDER_RANGE: (f64, f64) = (0.0, 60.0);

impl Default for Display {
    fn default() -> Self {
        Self {
            monitor: None,
            top_offset: 0.0,
            scale: 1.0,
            open_on_hover: false,
            compact_idle: true,
            hide_in_fullscreen: true,
            gnome_helper: false,
            reserve_space: false,
            all_workspaces: cfg!(target_os = "linux"),
            shortcut: "Super+Alt+Space".into(),
        }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self { enabled: true, duration_secs: 4.5, blocked_apps: Vec::new() }
    }
}

impl Default for Media {
    fn default() -> Self {
        Self { enabled: true, ignored_players: Vec::new() }
    }
}

impl Default for Calendar {
    fn default() -> Self {
        Self { enabled: true, reminder_minutes: 5.0 }
    }
}

fn clamp((min, max): (f64, f64), value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn tidy(list: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in list.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&item)) {
            out.push(item);
        }
    }
    out
}

impl Settings {
    /// Whatever the file or the UI sent, the rest of the app only ever sees sane values.
    pub fn sanitized(mut self) -> Self {
        let d = Display::default();
        self.display.top_offset = clamp(TOP_OFFSET_RANGE, self.display.top_offset, d.top_offset).round();
        self.display.scale = (clamp(SCALE_RANGE, self.display.scale, d.scale) * 100.0).round() / 100.0;
        self.display.monitor = self.display.monitor.filter(|m| !m.trim().is_empty());
        self.display.shortcut = self.display.shortcut.trim().to_string();
        let n = Notifications::default();
        self.notifications.duration_secs = clamp(DURATION_RANGE, self.notifications.duration_secs, n.duration_secs);
        self.notifications.blocked_apps = tidy(self.notifications.blocked_apps);
        self.media.ignored_players = tidy(self.media.ignored_players);
        self.calendar.reminder_minutes = clamp(REMINDER_RANGE, self.calendar.reminder_minutes, Calendar::default().reminder_minutes).round();
        self.chatgpt.model = self.chatgpt.model.filter(|m| !m.trim().is_empty());
        self.chatgpt.shortcut = self.chatgpt.shortcut.trim().to_string();
        self
    }

    pub fn is_app_blocked(&self, app: &str) -> bool {
        self.notifications.blocked_apps.iter().any(|b| b.eq_ignore_ascii_case(app.trim()))
    }
}

pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<Settings>,
}

impl SettingsStore {
    /// A missing or unreadable file means defaults; a bad file is left alone until the next save.
    pub fn load(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let path = app.path().app_config_dir()?.join("settings.json");
        let current = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| match serde_json::from_str::<Settings>(&text) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("hoverpill: ignoring invalid {}: {e}", path.display());
                    None
                }
            })
            .unwrap_or_default()
            .sanitized();
        Ok(Self { path, current: RwLock::new(current) })
    }

    pub fn get(&self) -> Settings {
        self.current.read().unwrap().clone()
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        let err = |e: std::io::Error| format!("can't save settings: {e}");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(settings).expect("serialising settings")).map_err(err)?;
        std::fs::rename(&tmp, &self.path).map_err(err)
    }
}

/// Pushes settings into the parts of the app that hold their own copy.
pub fn apply(app: &AppHandle, settings: &Settings) {
    // Turning calendar events off clears them from the notch; turning them back on restores them.
    let calendar = if settings.calendar.enabled { app.state::<crate::accounts::google::Google>().events() } else { Vec::new() };
    crate::events::send(app, "pill:calendar", calendar);
    let media = app.state::<hoverpill_media::Media>();
    media.set_ignored(settings.media.ignored_players.iter().cloned());
    crate::events::send(
        app,
        "pill:media",
        if settings.media.enabled { media.current().map(crate::events::MediaState::from) } else { None },
    );
    if let Some(pill) = app.get_webview_window(crate::notch::LABEL) {
        if let Err(e) = crate::notch::place(&pill) {
            eprintln!("hoverpill: placement failed: {e}");
        }
        crate::notch::set_all_workspaces(&pill, settings.display.all_workspaces);
    }
    crate::shortcut::apply(app, &settings.display.shortcut, &settings.chatgpt.shortcut);
    let _ = app.emit("settings:changed", settings);
}

/// Moves the notch to another monitor, as choosing it in Settings would. Dropping the bubble at
/// that monitor's top centre does this.
pub fn set_monitor(app: &AppHandle, name: String) {
    let store = app.state::<SettingsStore>();
    let mut settings = store.get();
    settings.display.monitor = Some(name);
    match store.save(&settings) {
        Ok(()) => {
            *store.current.write().unwrap() = settings.clone();
            apply(app, &settings);
        }
        Err(e) => eprintln!("hoverpill: {e}"),
    }
}

pub const WINDOW_LABEL: &str = "settings";

/// Bring the Settings window forward and give it the keyboard.
///
/// The pill deliberately never takes focus, so Mutter treats HoverPill as a background app and
/// refuses a focus request with a zero timestamp (Tao's). Asking with a timestamp read from the
/// X server is accepted; XWayland also lets us hand the keyboard over directly, the same trick
/// the notch uses when its question box is clicked. Only done in response to a click on the
/// gear, the tray or a second launch, so it can't pull focus away from what someone is doing.
fn focus(win: &tauri::WebviewWindow) {
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
    #[cfg(target_os = "linux")]
    {
        let win = win.clone();
        let _ = win.clone().run_on_main_thread(move || {
            use gdkx11::glib::translate::ToGlibPtr;
            use gtk::prelude::*;
            let Ok(gtk_win) = win.gtk_window() else { return };
            let Some(gdk_win) = gtk_win.window() else { return };
            // Native Wayland has no X window to focus; the Linux preflight settles on X11.
            let Ok(x11_win) = gdk_win.downcast::<gdkx11::X11Window>() else { return };
            // SAFETY: the window is alive on this thread; the call only reads a server timestamp.
            let timestamp = unsafe { gdkx11::ffi::gdk_x11_get_server_time(x11_win.to_glib_none().0) };
            gtk_win.present_with_time(timestamp);
            let xid: x11_dl::xlib::Window = x11_win.xid();
            let Ok(xlib) = x11_dl::xlib::Xlib::open() else { return };
            // SAFETY: a short-lived connection of our own; `XCloseDisplay` flushes the requests.
            unsafe {
                let display = (xlib.XOpenDisplay)(std::ptr::null());
                if display.is_null() {
                    return;
                }
                (xlib.XRaiseWindow)(display, xid);
                (xlib.XSetInputFocus)(display, xid, x11_dl::xlib::RevertToParent, x11_dl::xlib::CurrentTime);
                (xlib.XCloseDisplay)(display);
            }
        });
    }
}

/// Opens the Settings window, or brings it forward if it's already open.
pub fn open_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(WINDOW_LABEL) {
        focus(&win);
        return;
    }
    // Built off the event-loop thread: creating a WebView2 window from inside a menu/IPC
    // handler can deadlock on Windows.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let built = WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::App("settings.html".into()))
            .title("HoverPill Settings")
            .inner_size(720.0, 540.0)
            .min_inner_size(600.0, 440.0)
            .center()
            .build();
        match built {
            Ok(win) => focus(&win),
            Err(e) => {
                crate::events::notify(&app, "Couldn't open Settings", &e.to_string());
            }
        }
    });
}

/// Apps seen sending notifications (blocked ones too), newest first, so the Settings window
/// can offer them for blocking without anyone typing names. Saved, so the list survives
/// restarts.
pub struct RecentApps {
    path: PathBuf,
    list: Mutex<Vec<String>>,
}

const RECENT_APPS_MAX: usize = 40;

impl RecentApps {
    pub fn load(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let path = app.path().app_config_dir()?.join("seen-apps.json");
        let list = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Ok(Self { path, list: Mutex::new(list) })
    }

    /// Moves `apps` to the front. Saves only when an app is new, so ordinary traffic from known
    /// apps doesn't write to disk.
    pub fn record<'a>(&self, apps: impl IntoIterator<Item = &'a str>) {
        let mut list = self.list.lock().unwrap();
        let mut new_app = false;
        for app in apps.into_iter().map(str::trim).filter(|a| !a.is_empty()) {
            let before = list.len();
            list.retain(|a| !a.eq_ignore_ascii_case(app));
            new_app |= list.len() == before;
            list.insert(0, app.to_string());
        }
        list.truncate(RECENT_APPS_MAX);
        if new_app {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&self.path, serde_json::to_string(&*list).unwrap_or_default()) {
                eprintln!("hoverpill: can't save {}: {e}", self.path.display());
            }
        }
    }
}

#[tauri::command]
pub fn recent_notification_apps(recent: State<RecentApps>) -> Vec<String> {
    recent.list.lock().unwrap().clone()
}

/// Every app the OS lets send notifications (Windows; empty on Linux), for apps that haven't
/// sent one since HoverPill started. Reading the Start menu takes a moment, so it runs on a
/// worker thread.
#[tauri::command]
pub async fn notifying_apps() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(hoverpill_notify::notifying_apps).await.unwrap_or_default()
}

#[derive(Serialize)]
pub struct PlayerInfo {
    id: String,
    label: String,
}

/// Players the OS knows about right now, plus ignored ones that aren't running.
#[tauri::command]
pub fn media_players(media: State<hoverpill_media::Media>, store: State<SettingsStore>) -> Vec<PlayerInfo> {
    let mut sources = media.sources();
    for id in store.get().media.ignored_players {
        if !sources.iter().any(|s| s.id == id) {
            sources.push(hoverpill_media::Source::new(id));
        }
    }
    sources.sort_by_key(|a| a.label.to_lowercase());
    sources.into_iter().map(|s| PlayerInfo { id: s.id, label: s.label }).collect()
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    open_window(&app);
}

#[tauri::command]
pub fn get_settings(store: State<SettingsStore>) -> Settings {
    store.get()
}

/// Returns what was actually stored (after clamping), so the UI can snap to it.
#[tauri::command]
pub fn set_settings(app: AppHandle, store: State<SettingsStore>, settings: Settings) -> Result<Settings, String> {
    let settings = settings.sanitized();
    // The Wayland helper writes into the user's GNOME extensions, so only a change of that
    // setting should touch them: any other save must leave a helper the user removed alone.
    #[cfg(target_os = "linux")]
    let helper_changed = store.get().display.gnome_helper != settings.display.gnome_helper;
    store.save(&settings)?;
    *store.current.write().unwrap() = settings.clone();
    apply(&app, &settings);
    #[cfg(target_os = "linux")]
    if helper_changed {
        crate::shellext::sync(&app, settings.display.gnome_helper);
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_fills_in_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "display": { "scale": 1.2 } }"#).unwrap();
        assert_eq!(s.display.scale, 1.2);
        assert_eq!(s.display.top_offset, 0.0);
        assert!(!s.display.open_on_hover && s.display.compact_idle && s.display.hide_in_fullscreen);
        assert!(!s.display.reserve_space);
        // The GNOME helper touches the desktop's own extensions directory, so it waits to be asked
        // for, whatever the platform.
        assert!(!s.display.gnome_helper);
        assert_eq!(s.display.all_workspaces, cfg!(target_os = "linux"));
        assert!(s.notifications.enabled);
        assert_eq!(s.chatgpt.shortcut, "Ctrl+Alt+G");
        assert_eq!(s.display.shortcut, "Super+Alt+Space");
        assert!(s.updates.auto_check);
        // Added after 0.4.1: an older file keeps the dots and the level cards on.
        assert!(s.privacy.camera_indicator && s.privacy.microphone_indicator);
        assert!(s.system.show_volume && s.system.show_brightness && s.system.show_battery);
    }

    #[test]
    fn a_google_client_from_older_settings_is_dropped() {
        let s: Settings =
            serde_json::from_str(r#"{ "calendar": { "enabled": false, "clientId": "1-abc.apps.googleusercontent.com", "clientSecret": "x" } }"#)
                .unwrap();
        assert!(!s.calendar.enabled);
        let saved = serde_json::to_string(&s).unwrap();
        assert!(!saved.contains("clientId") && !saved.contains("clientSecret"));
    }

    #[test]
    fn sanitize_clamps_and_dedupes() {
        let mut s = Settings::default();
        s.display.scale = 9.0;
        s.display.top_offset = -5.0;
        s.display.monitor = Some("  ".into());
        s.notifications.duration_secs = f64::NAN;
        s.notifications.blocked_apps = vec!["Discord".into(), " discord ".into(), "".into(), "Slack".into()];
        let s = s.sanitized();
        assert_eq!(s.display.scale, SCALE_RANGE.1);
        assert_eq!(s.display.top_offset, 0.0);
        assert_eq!(s.display.monitor, None);
        assert_eq!(s.notifications.duration_secs, 4.5);
        assert_eq!(s.notifications.blocked_apps, vec!["Discord", "Slack"]);
    }

    #[test]
    fn blocking_ignores_case() {
        let mut s = Settings::default();
        s.notifications.blocked_apps = vec!["WhatsApp".into()];
        assert!(s.is_app_blocked("whatsapp"));
        assert!(!s.is_app_blocked("Discord"));
    }
}
