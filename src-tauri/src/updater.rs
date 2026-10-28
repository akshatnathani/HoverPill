//! Updates from GitHub Releases, through Tauri's updater. Each release publishes a `latest.json`
//! and signed installers; an update is installed only if its signature matches the public key
//! in tauri.conf.json, so a tampered download is refused.
//!
//! HoverPill checks a minute after it starts and then every few hours (unless that's turned off
//! in Settings), downloads a newer version in the background, and offers it in the notch:
//! "HoverPill 0.5.0 is ready. Restart to update". The tray and Settings can check right away.
//!
//! Windows installs in place (the installer runs, then HoverPill starts again). On Linux only the
//! AppImage can replace itself; a .deb or .rpm install is pointed at the download page instead.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::events;
use crate::settings::SettingsStore;

/// After starting, and then between checks.
const FIRST_CHECK: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// An update that's downloaded and waiting for a restart.
#[derive(Default)]
pub struct Pending(Mutex<Option<(Update, Vec<u8>)>>);

/// The last update a check found, until it's installed. Kept so that Settings can offer it too,
/// even after "Later" put the notch's card away.
#[derive(Default)]
pub struct Latest(Mutex<Option<Offer>>);

/// Sent as `pill:update`: a newer version is ready to install (`restart`), or, where HoverPill
/// can't replace itself, to download by hand (`download`).
#[derive(Clone, Serialize)]
pub struct Offer {
    pub version: String,
    pub mode: &'static str,
}

/// Whether this copy can install an update over itself.
fn can_replace_itself() -> bool {
    cfg!(windows) || std::env::var_os("APPIMAGE").is_some()
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            if app.state::<SettingsStore>().get().updates.auto_check {
                if let Err(e) = check(&app).await {
                    eprintln!("hoverpill: update check failed: {e}");
                }
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// Looks for a newer release; if there is one, gets it ready and offers it in the notch.
async fn check(app: &AppHandle) -> Result<Option<Offer>, String> {
    if let Some((update, _)) = app.state::<Pending>().0.lock().unwrap().as_ref() {
        let offer = Offer { version: update.version.clone(), mode: "restart" };
        remember(app, Some(offer.clone()));
        events::send(app, "pill:update", offer.clone());
        return Ok(Some(offer));
    }
    let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        remember(app, None);
        return Ok(None);
    };
    let version = update.version.clone();
    if !can_replace_itself() {
        let offer = Offer { version, mode: "download" };
        remember(app, Some(offer.clone()));
        events::send(app, "pill:update", offer.clone());
        return Ok(Some(offer));
    }
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    *app.state::<Pending>().0.lock().unwrap() = Some((update, bytes));
    let offer = Offer { version, mode: "restart" };
    remember(app, Some(offer.clone()));
    events::send(app, "pill:update", offer.clone());
    Ok(Some(offer))
}

fn remember(app: &AppHandle, offer: Option<Offer>) {
    *app.state::<Latest>().0.lock().unwrap() = offer;
}

/// For Settings: the update the last check found, if it hasn't been installed yet. Settings asks
/// when it opens, so "Later" in the notch doesn't leave it with nothing to show.
#[tauri::command]
pub fn pending_update(latest: tauri::State<Latest>) -> Option<Offer> {
    latest.0.lock().unwrap().clone()
}

/// "Check for updates" in the tray: like the automatic check, but says so when there's nothing new.
pub fn check_by_hand(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match check(&app).await {
            Ok(Some(_)) => {}
            Ok(None) => events::notify(&app, "HoverPill is up to date", &format!("You have the latest version, {}.", app.package_info().version)),
            Err(e) => events::notify(&app, "Couldn't check for updates", &e),
        }
    });
}

/// For Settings: checks now and says what to do with it, or nothing if this is the latest.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Option<Offer>, String> {
    check(&app).await
}

/// The notch's Restart button: installs the downloaded update and starts the new version.
#[tauri::command]
pub fn install_update(app: AppHandle) -> Result<(), String> {
    let Some((update, bytes)) = app.state::<Pending>().0.lock().unwrap().take() else {
        return Err("There's no update ready to install.".into());
    };
    remember(&app, None);
    // On Windows this runs the installer, which closes HoverPill and starts the new version.
    update.install(bytes).map_err(|e| e.to_string())?;
    app.restart();
}
