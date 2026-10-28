//! Camera and microphone use, for the little dots in the closed notch and the app names in the
//! open one.
//!
//! Windows keeps a record of every app that uses a camera or microphone: the
//! `CapabilityAccessManager\ConsentStore` keys under HKCU are written when an app starts using a
//! device (`LastUsedTimeStart`) and when it stops (`LastUsedTimeStop`). Packaged apps get a key
//! named after their package family name; desktop apps are under `NonPackaged`, named after their
//! executable. That is the same information the camera/microphone icons in Windows' own taskbar
//! use, and it names the app.
//!
//! Linux reads the sound server for the microphone: every recording stream (`pactl list
//! source-outputs`) names its app, and streams that record the speakers rather than a microphone
//! (screen recorders, visualisers) or only meter its level are left out. For the camera it asks
//! which processes have a `/dev/video*` node open, woken by inotify when one is opened or
//! closed. When PipeWire holds the camera on an app's behalf (the camera portal), the app is
//! read from PipeWire's own video streams.

#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

/// An app currently using a capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessEntry {
    /// App name to show, e.g. "Camera" or "Google Chrome".
    pub app: String,
    /// What Windows calls it: the package family name, or the executable's path.
    pub id: String,
    /// When it started, Unix ms.
    pub started_at: u64,
}

/// The apps using one capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessState {
    pub entries: Vec<AccessEntry>,
    /// Any app at all: what the dot follows.
    pub active: bool,
}

/// Camera and microphone together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyState {
    pub camera: AccessState,
    pub microphone: AccessState,
}

#[cfg(windows)]
mod platform {
    use super::*;

    use std::time::Duration;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    use crate::events::send;

    /// Comfortably quicker than someone notices a video call starting; a registry read is cheap.
    const POLL: Duration = Duration::from_millis(700);

    fn capability(name: &str) -> AccessState {
        // HKCU, because the per-user record is the one that names the app; the machine-wide
        // policy key below `HKLM` only says whether the capability is allowed at all.
        let path = format!(
            r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{name}"
        );
        let Ok(store) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(&path, KEY_READ) else {
            // No key: nothing has ever used it, or this Windows version doesn't keep the record.
            return AccessState::default();
        };

        let mut entries = Vec::new();
        // Packaged (Store/MSIX) apps: the key is the package family name.
        for id in store.enum_keys().flatten() {
            if id.eq_ignore_ascii_case("NonPackaged") {
                continue;
            }
            if let Ok(key) = store.open_subkey_with_flags(&id, KEY_READ) {
                if let Some(entry) = entry(&key, &id, None) {
                    entries.push(entry);
                }
            }
        }
        // Desktop apps: `NonPackaged\<executable path, with # for \>`.
        if let Ok(desktop) = store.open_subkey_with_flags("NonPackaged", KEY_READ) {
            for id in desktop.enum_keys().flatten() {
                if let Ok(key) = desktop.open_subkey_with_flags(&id, KEY_READ) {
                    if let Some(entry) = entry(&key, &id, Some(&id)) {
                        entries.push(entry);
                    }
                }
            }
        }

        entries.sort_by_key(|e| e.started_at);
        AccessState { active: !entries.is_empty(), entries }
    }

    /// One app's record, if it is using the capability *right now*: started and not stopped.
    fn entry(key: &RegKey, id: &str, path: Option<&str>) -> Option<AccessEntry> {
        let started = timestamp(key, "LastUsedTimeStart")?;
        // Some builds leave the stop value out while an app is active; absent means "still on".
        let stopped = timestamp(key, "LastUsedTimeStop").unwrap_or(0);
        if started == 0 || stopped != 0 {
            return None;
        }
        let app = path
            .map(friendly_path)
            .or_else(|| key.get_value::<String, _>("DisplayName").ok().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| friendly_package(id));
        Some(AccessEntry { app, id: id.to_string(), started_at: filetime_ms(started) })
    }

    /// `LastUsedTimeStart`/`Stop` are FILETIMEs; Windows has written them both as a binary value
    /// and as a QWORD over the years, and both are eight little-endian bytes.
    fn timestamp(key: &RegKey, name: &str) -> Option<u64> {
        let raw = key.get_raw_value(name).ok()?;
        let bytes: [u8; 8] = raw.bytes.as_ref().try_into().ok()?;
        Some(u64::from_le_bytes(bytes))
    }

    /// FILETIME (100 ns since 1601) to Unix milliseconds.
    pub(super) fn filetime_ms(filetime: u64) -> u64 {
        (filetime / 10_000).saturating_sub(11_644_473_600_000)
    }

    /// `C:#Program Files#Google#Chrome#Application#chrome.exe` → "Chrome".
    pub(super) fn friendly_path(id: &str) -> String {
        let path = id.replace('#', r"\");
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
        split_words(name)
    }

    /// `Microsoft.WindowsCamera_8wekyb3d8bbwe` → "Windows Camera".
    pub(super) fn friendly_package(id: &str) -> String {
        let name = id.split('_').next().unwrap_or(id);
        let name = name.rsplit('.').next().unwrap_or(name);
        let name = name.strip_prefix("Microsoft.").unwrap_or(name);
        split_words(name)
    }

    /// `WindowsCamera` → "Windows Camera"; leaves names that already have spaces alone.
    fn split_words(name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 4);
        for (i, c) in name.char_indices() {
            if i > 0 && c.is_uppercase() {
                let prev = name[..i].chars().next_back().unwrap_or(' ');
                if prev.is_lowercase() || prev.is_ascii_digit() {
                    out.push(' ');
                }
            }
            out.push(if i == 0 { c.to_ascii_uppercase() } else { c });
        }
        out
    }

    pub fn read() -> Result<PrivacyState, String> {
        Ok(PrivacyState { camera: capability("webcam"), microphone: capability("microphone") })
    }

    pub fn spawn_worker(app: AppHandle) {
        std::thread::Builder::new()
            .name("hoverpill-access".into())
            .spawn(move || {
                let mut last = PrivacyState::default();
                loop {
                    match read() {
                        Ok(state) => {
                            if state != last {
                                send(&app, "pill:privacy", state.clone());
                                last = state;
                            }
                        }
                        Err(e) => eprintln!("hoverpill: reading camera/microphone use failed: {e}"),
                    }
                    std::thread::sleep(POLL);
                }
            })
            .expect("spawning the camera/microphone thread");
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    use std::collections::HashMap;
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use crate::events::send;

    /// Read both again this often even without an event, in case one was missed.
    const SAFETY_EVERY: Duration = Duration::from_secs(10);
    /// While PipeWire holds the camera for apps, another app can join or leave without the
    /// device being opened or closed again, so look at its streams this often.
    const SHARED_CAMERA_EVERY: Duration = Duration::from_secs(2);
    /// After an event, wait this long for the rest of the burst (an app opens several nodes).
    const SETTLE: Duration = Duration::from_millis(150);

    /// The processes that hold a camera for others.
    const MEDIA_SERVERS: [&str; 3] = ["pipewire", "wireplumber", "pipewire-media-session"];

    enum Wake {
        Microphone,
        Camera,
    }

    /// One app using a capability, before it gets its start time.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct User {
        pub app: String,
        pub id: String,
    }

    /// Who a PipeWire/PulseAudio stream belongs to: its app name, else the executable, else the
    /// node's name, made readable (see `readable`).
    fn stream_user(props: &Value) -> Option<User> {
        let text = |name: &str| props[name].as_str().map(str::trim).filter(|s| !s.is_empty());
        let binary = text("application.process.binary");
        let app = text("application.name").or(binary).or_else(|| text("node.name"))?;
        Some(User { app: readable(app), id: binary.unwrap_or(app).to_string() })
    }

    /// Some apps name their streams with their reverse-DNS app ID: `org.gnome.Snapshot` →
    /// "Snapshot", `com.discordapp.Discord` → "Discord". Anything that reads as a name already
    /// (spaces, or fewer than three dot-separated parts) is left as it is.
    pub(super) fn readable(name: &str) -> String {
        let parts: Vec<&str> = name.split('.').collect();
        let id_like = parts.len() >= 3
            && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        if !id_like {
            return name.to_string();
        }
        // The last part is the app; split CamelCase so `TextEditor` reads "Text Editor".
        let last = parts[parts.len() - 1];
        let mut out = String::with_capacity(last.len() + 4);
        for (i, c) in last.char_indices() {
            if i > 0 && c.is_uppercase() && last[..i].chars().next_back().is_some_and(|p| p.is_lowercase()) {
                out.push(' ');
            }
            out.push(c);
        }
        out
    }

    // ---------- microphone ----------

    fn pactl_json(what: &str) -> Option<Value> {
        let out = Command::new("pactl").args(["-f", "json", "list", what]).stderr(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    }

    /// `None` when the sound server can't be asked; the last answer stands then.
    fn microphone_users() -> Option<Vec<User>> {
        Some(recording_apps(&pactl_json("sources")?, &pactl_json("source-outputs")?))
    }

    /// A real microphone (or a filter on one), not the "monitor" of a speaker.
    fn is_microphone(source: &Value) -> bool {
        let props = &source["properties"];
        if let Some(class) = props["media.class"].as_str() {
            // PipeWire: monitors show up as the sink they belong to.
            return class.starts_with("Audio/Source");
        }
        // PulseAudio has no media class.
        props["device.class"].as_str() != Some("monitor")
            && matches!(source["monitor_of_sink"].as_str(), None | Some("") | Some("n/a"))
    }

    /// The apps recording from a microphone right now, one entry per app.
    pub(super) fn recording_apps(sources: &Value, outputs: &Value) -> Vec<User> {
        let mics: Vec<u64> =
            sources.as_array().into_iter().flatten().filter(|s| is_microphone(s)).filter_map(|s| s["index"].as_u64()).collect();
        let mut users: Vec<User> = Vec::new();
        for stream in outputs.as_array().into_iter().flatten() {
            let props = &stream["properties"];
            let flag = |name: &str| props[name].as_str() == Some("true");
            let on_mic = stream["source"].as_u64().is_some_and(|s| mics.contains(&s));
            // Paused streams, streams that capture the speakers, and level meters (Settings'
            // input level, pavucontrol) aren't listening to you.
            if !on_mic || stream["corked"].as_bool() == Some(true) || flag("stream.capture.sink") || flag("resample.peaks") {
                continue;
            }
            let Some(user) = stream_user(props) else { continue };
            if !users.iter().any(|u| u.app == user.app) {
                users.push(user);
            }
        }
        users
    }

    /// `pactl subscribe` prints a line for every change on the sound server; recording streams
    /// coming and going are the ones that matter here.
    fn watch_microphone(tx: mpsc::Sender<Wake>) {
        let _ = std::thread::Builder::new().name("hoverpill-mic-events".into()).spawn(move || {
            let Ok(mut child) =
                Command::new("pactl").arg("subscribe").stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            else {
                return;
            };
            let Some(stdout) = child.stdout.take() else { return };
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if (line.contains("source-output") || line.contains("'source'") || line.contains("server"))
                    && tx.send(Wake::Microphone).is_err()
                {
                    break;
                }
            }
            let _ = child.kill();
        });
    }

    // ---------- camera ----------

    fn camera_nodes() -> Vec<std::path::PathBuf> {
        let Ok(dir) = fs::read_dir("/dev") else { return Vec::new() };
        dir.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("video"))).collect()
    }

    /// Processes with a camera node open: (pid, executable name). Only our own user's processes
    /// can be looked at, which covers every app on this desktop.
    fn camera_holders() -> Vec<(u32, String)> {
        let me = std::process::id();
        let mut holders = Vec::new();
        let Ok(procs) = fs::read_dir("/proc") else { return holders };
        for entry in procs.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if pid == me {
                continue;
            }
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else { continue };
            let holds = fds.flatten().any(|fd| {
                fs::read_link(fd.path()).is_ok_and(|target| target.as_os_str().as_encoded_bytes().starts_with(b"/dev/video"))
            });
            if holds {
                holders.push((pid, process_name(pid)));
            }
        }
        holders
    }

    /// The executable's file name (the thread name in `comm` can be "Isolated Web Co" or cut
    /// off at 15 characters), falling back to `comm` where the executable can't be read.
    fn process_name(pid: u32) -> String {
        fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|n| n.trim_end_matches(" (deleted)").to_string())
            .or_else(|| fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()))
            .unwrap_or_default()
    }

    /// `google-chrome` → "Google Chrome", `firefox-bin` → "Firefox", `python3.12` → "Python3".
    pub(super) fn friendly(binary: &str) -> String {
        // Version suffixes (`python3.12`) and extensions (`.bin`, `.AppImage`) go.
        let name = binary.split('.').next().unwrap_or(binary);
        let name = name.trim_end_matches("-bin").trim_end_matches("-stable");
        name.split(['-', '_'])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The apps PipeWire is streaming a camera to: its running video input streams.
    pub(super) fn pipewire_video_apps(dump: &Value) -> Vec<User> {
        let mut users: Vec<User> = Vec::new();
        for object in dump.as_array().into_iter().flatten() {
            let info = &object["info"];
            let props = &info["props"];
            if props["media.class"].as_str() != Some("Stream/Input/Video") || info["state"].as_str() != Some("running") {
                continue;
            }
            let Some(user) = stream_user(props) else { continue };
            if !users.iter().any(|u| u.app == user.app) {
                users.push(user);
            }
        }
        users
    }

    /// The apps using a camera, and whether PipeWire is holding it for them.
    fn camera_users() -> (Vec<User>, bool) {
        let mut users: Vec<User> = Vec::new();
        let mut shared = false;
        for (_, binary) in camera_holders() {
            if MEDIA_SERVERS.contains(&binary.as_str()) {
                shared = true;
                continue;
            }
            let user = User { app: friendly(&binary), id: binary };
            if !users.iter().any(|u| u.app == user.app) {
                users.push(user);
            }
        }
        if shared {
            let out = Command::new("pw-dump").stderr(Stdio::null()).output().ok();
            let apps = out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).map(|d| pipewire_video_apps(&d)).unwrap_or_default();
            if apps.is_empty() {
                // Open, but we can't tell for whom. Say so rather than show nothing: the light is on.
                users.push(User { app: "Camera".into(), id: "pipewire".into() });
            }
            for user in apps {
                if !users.iter().any(|u| u.app == user.app) {
                    users.push(user);
                }
            }
        }
        (users, shared)
    }

    /// Wakes the worker whenever a camera node is opened or closed, and when one appears or
    /// goes away. Opening a node doesn't start the camera, so watching it is harmless.
    fn watch_cameras(tx: mpsc::Sender<Wake>) {
        let _ = std::thread::Builder::new().name("hoverpill-camera-events".into()).spawn(move || {
            // SAFETY: plain inotify calls on a descriptor this thread owns; paths are C strings.
            unsafe {
                let fd = libc::inotify_init1(libc::IN_CLOEXEC);
                if fd < 0 {
                    return;
                }
                let watch = |path: &std::path::Path, mask: u32| {
                    if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
                        libc::inotify_add_watch(fd, c.as_ptr(), mask);
                    }
                };
                let use_mask = libc::IN_OPEN | libc::IN_CLOSE_WRITE | libc::IN_CLOSE_NOWRITE;
                let add_cameras = || camera_nodes().iter().for_each(|node| watch(node, use_mask));
                watch(std::path::Path::new("/dev"), libc::IN_CREATE | libc::IN_DELETE);
                add_cameras();
                let mut buf = [0u8; 4096];
                loop {
                    let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                    if n <= 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    // Events for /dev itself: only new or removed cameras matter. Re-adding a watch
                    // that exists is a no-op.
                    let mut camera_event = false;
                    let mut offset = 0usize;
                    let header = std::mem::size_of::<libc::inotify_event>();
                    while offset + header <= n as usize {
                        let event = std::ptr::read_unaligned(buf.as_ptr().add(offset).cast::<libc::inotify_event>());
                        let name_bytes = &buf[offset + header..(offset + header + event.len as usize).min(n as usize)];
                        let created_or_removed = event.mask & (libc::IN_CREATE | libc::IN_DELETE) != 0;
                        if !created_or_removed || name_bytes.starts_with(b"video") {
                            camera_event = true;
                        }
                        if created_or_removed && name_bytes.starts_with(b"video") {
                            add_cameras();
                        }
                        offset += header + event.len as usize;
                    }
                    if camera_event && tx.send(Wake::Camera).is_err() {
                        break;
                    }
                }
                libc::close(fd);
            }
        });
    }

    // ---------- the worker ----------

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    /// Keeps each app's start time from when it was first seen, for as long as it stays.
    fn with_start_times(users: Vec<User>, since: &mut HashMap<String, u64>) -> AccessState {
        let now = now_ms();
        since.retain(|id, _| users.iter().any(|u| &u.id == id));
        let mut entries: Vec<AccessEntry> = users
            .into_iter()
            .map(|u| AccessEntry { started_at: *since.entry(u.id.clone()).or_insert(now), app: u.app, id: u.id })
            .collect();
        entries.sort_by_key(|e| e.started_at);
        AccessState { active: !entries.is_empty(), entries }
    }

    fn due(at: Option<Instant>, every: Duration) -> bool {
        at.is_none_or(|t| t.elapsed() >= every)
    }

    pub fn spawn_worker(app: AppHandle) {
        std::thread::Builder::new()
            .name("hoverpill-access".into())
            .spawn(move || {
                let (tx, rx) = mpsc::channel();
                // Kept so the channel stays open if the watcher threads couldn't start.
                let _keep_alive = tx.clone();
                watch_microphone(tx.clone());
                watch_cameras(tx);

                let mut state = PrivacyState::default();
                let mut last = PrivacyState::default();
                let (mut mic_since, mut camera_since) = (HashMap::new(), HashMap::new());
                let (mut mic_at, mut camera_at) = (None, None);
                let mut shared_camera = false;
                loop {
                    let (mut mic, mut camera) = (false, false);
                    if let Ok(first) = rx.recv_timeout(Duration::from_millis(500)) {
                        std::thread::sleep(SETTLE);
                        for wake in std::iter::once(first).chain(rx.try_iter()) {
                            match wake {
                                Wake::Microphone => mic = true,
                                Wake::Camera => camera = true,
                            }
                        }
                    }
                    if mic || due(mic_at, SAFETY_EVERY) {
                        if let Some(users) = microphone_users() {
                            state.microphone = with_start_times(users, &mut mic_since);
                        }
                        mic_at = Some(Instant::now());
                    }
                    let camera_every = if shared_camera { SHARED_CAMERA_EVERY } else { SAFETY_EVERY };
                    if camera || due(camera_at, camera_every) {
                        let (users, shared) = camera_users();
                        shared_camera = shared;
                        state.camera = with_start_times(users, &mut camera_since);
                        camera_at = Some(Instant::now());
                    }
                    if state != last {
                        send(&app, "pill:privacy", state.clone());
                        last = state.clone();
                    }
                }
            })
            .expect("spawning the camera/microphone thread");
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::*;

    /// Nothing to watch on this platform.
    pub fn spawn_worker(_app: AppHandle) {}
}

/// Watches camera and microphone use and pushes `pill:privacy` whenever it changes.
pub fn spawn_worker(app: AppHandle) {
    platform::spawn_worker(app);
}

#[cfg(all(test, windows))]
mod tests {
    use super::platform;

    #[test]
    fn names_read_well() {
        assert_eq!(platform::friendly_path(r"C:#Program Files#Google#Chrome#Application#chrome.exe"), "Chrome");
        assert_eq!(platform::friendly_package("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "Windows Camera");
        assert_eq!(platform::friendly_package("Microsoft.SkypeApp_kzf8qxf38zg5c"), "Skype App");
        assert_eq!(platform::friendly_package("SpotifyAB.SpotifyMusic_zpdnekdrzrea0"), "Spotify Music");
    }

    #[test]
    fn filetimes_become_unix_milliseconds() {
        // 2024-01-01T00:00:00Z, as a FILETIME.
        assert_eq!(platform::filetime_ms(133_485_408_000_000_000), 1_704_067_200_000);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::platform::{friendly, pipewire_video_apps, readable, recording_apps, User};
    use serde_json::json;

    fn user(app: &str, id: &str) -> User {
        User { app: app.into(), id: id.into() }
    }

    #[test]
    fn only_streams_from_a_microphone_count() {
        // As PipeWire's pactl reports them: 58 is the speakers' monitor, 59 the microphone.
        let sources = json!([
            { "index": 58, "monitor_source": "alsa_output.analog-stereo", "properties": { "media.class": "Audio/Sink", "device.class": "monitor" } },
            { "index": 59, "monitor_source": "", "properties": { "media.class": "Audio/Source", "device.class": "sound" } },
        ]);
        let outputs = json!([
            // A call app on the microphone.
            { "source": 59, "corked": false, "properties": { "application.name": "Zoom", "application.process.binary": "zoom" } },
            // The same app with a second stream: still one entry.
            { "source": 59, "corked": false, "properties": { "application.name": "Zoom", "application.process.binary": "zoom" } },
            // A screen recorder capturing the speakers.
            { "source": 58, "corked": false, "properties": { "application.name": "OBS", "stream.capture.sink": "true" } },
            // Paused.
            { "source": 59, "corked": true, "properties": { "application.name": "Recorder" } },
            // Settings' input level meter.
            { "source": 59, "corked": false, "properties": { "application.name": "Settings", "resample.peaks": "true" } },
            // No app name: the binary, then the node.
            { "source": 59, "corked": false, "properties": { "node.name": "pw-record" } },
        ]);
        assert_eq!(recording_apps(&sources, &outputs), vec![user("Zoom", "zoom"), user("pw-record", "pw-record")]);
    }

    #[test]
    fn pulseaudio_monitors_are_told_apart_without_a_media_class() {
        let sources = json!([
            { "index": 1, "monitor_of_sink": "alsa_output.analog-stereo", "properties": { "device.class": "monitor" } },
            { "index": 2, "monitor_of_sink": "n/a", "properties": { "device.class": "sound" } },
        ]);
        let outputs = json!([
            { "source": 1, "corked": false, "properties": { "application.name": "Visualiser" } },
            { "source": 2, "corked": false, "properties": { "application.name": "Firefox", "application.process.binary": "firefox" } },
        ]);
        assert_eq!(recording_apps(&sources, &outputs), vec![user("Firefox", "firefox")]);
    }

    #[test]
    fn camera_apps_through_pipewire() {
        let dump = json!([
            { "info": { "state": "suspended", "props": { "media.class": "Video/Source", "node.name": "v4l2_input" } } },
            { "info": { "state": "running", "props": { "media.class": "Stream/Input/Video", "application.name": "org.gnome.Snapshot" } } },
            { "info": { "state": "paused", "props": { "media.class": "Stream/Input/Video", "application.name": "Cheese" } } },
        ]);
        assert_eq!(pipewire_video_apps(&dump), vec![user("Snapshot", "org.gnome.Snapshot")]);
    }

    #[test]
    fn app_ids_read_as_names() {
        assert_eq!(readable("org.gnome.Snapshot"), "Snapshot");
        assert_eq!(readable("com.discordapp.Discord"), "Discord");
        assert_eq!(readable("org.gnome.TextEditor"), "Text Editor");
        // Already names: left alone.
        assert_eq!(readable("Google Chrome"), "Google Chrome");
        assert_eq!(readable("Camera"), "Camera");
        assert_eq!(readable("pw-record"), "pw-record");
        assert_eq!(readable("python3.14"), "python3.14");
        assert_eq!(readable("Mozilla Firefox 1.2.3"), "Mozilla Firefox 1.2.3");
    }

    #[test]
    fn executables_read_well() {
        assert_eq!(friendly("google-chrome"), "Google Chrome");
        assert_eq!(friendly("firefox-bin"), "Firefox");
        assert_eq!(friendly("zoom"), "Zoom");
        assert_eq!(friendly("obs"), "Obs");
        assert_eq!(friendly("python3.12"), "Python3");
        assert_eq!(friendly("Slack.AppImage"), "Slack");
    }
}
