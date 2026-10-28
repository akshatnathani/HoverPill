//! Volume, screen brightness and battery, pushed to the notch so changing them shows a level
//! bar or a charging card there instead of the system's own pop-up in the corner.
//!
//! Windows reads the default audio endpoint (WASAPI), the built-in panel's brightness (WMI's
//! `WmiMonitorBrightness`, which only laptop and tablet panels answer) and the battery
//! (GetSystemPowerStatus).
//! Linux reads the default PulseAudio/PipeWire sink (`pactl`, or `wpctl`), the backlight under
//! `/sys/class/backlight` and the battery under `/sys/class/power_supply`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::events::send;

/// Output volume, 0–100 and whether it's muted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeState {
    pub level: u8,
    pub muted: bool,
}

/// Screen brightness, 0–100. `supported` is false on desktops and external monitors.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrightnessState {
    pub level: u8,
    pub supported: bool,
}

/// Battery level and whether the machine is plugged in. `supported` is false without a battery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatteryState {
    pub level: u8,
    pub charging: bool,
    pub supported: bool,
}

/// Everything at once; sent as `pill:system` whenever one of them changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemState {
    pub volume: VolumeState,
    pub brightness: BrightnessState,
    pub battery: BatteryState,
}

#[cfg(windows)]
mod platform {
    use super::*;

    use std::time::Instant;

    use windows::core::{w, BSTR, GUID, PCWSTR};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
    };
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::Variant::{VariantClear, VariantToUInt32, VARIANT};
    use windows::Win32::System::Wmi::{
        IEnumWbemClassObject, IWbemClassObject, IWbemContext, IWbemLocator, IWbemServices, WbemLocator,
        WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
    };

    /// Fast enough that pressing a volume key feels immediate; the reads are cheap COM gets.
    const POLL: Duration = Duration::from_millis(400);
    /// After WMI failed, or answered that there's no built-in panel (a desktop), ask again this
    /// rarely rather than every tick.
    const BRIGHTNESS_RETRY: Duration = Duration::from_secs(30);

    struct Backend {
        endpoint: Option<IAudioEndpointVolume>,
        brightness: Brightness,
    }

    impl Backend {
        fn new() -> Self {
            // This thread lives as long as the app, so the apartment never needs tearing down.
            // Without it, volume (and a device that appears later) is simply unavailable.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Self { endpoint: default_endpoint(), brightness: Brightness::default() }
        }

        fn read(&mut self) -> SystemState {
            // The default device can come and go (headphones, a dock): re-open it when it does.
            if self.endpoint.is_none() {
                self.endpoint = default_endpoint();
            }
            SystemState {
                volume: self.endpoint.as_ref().and_then(volume).unwrap_or_default(),
                brightness: self.brightness.read(),
                battery: battery(),
            }
        }
    }

    /// The speaker/headphones Windows is currently playing through.
    fn default_endpoint() -> Option<IAudioEndpointVolume> {
        // The MMDeviceEnumerator class; the bindings don't carry the CLSID itself.
        const MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MM_DEVICE_ENUMERATOR, None::<&windows::core::IUnknown>, CLSCTX_ALL).ok()?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            device.Activate(CLSCTX_ALL, None).ok()
        }
    }

    fn volume(endpoint: &IAudioEndpointVolume) -> Option<VolumeState> {
        unsafe {
            let level = endpoint.GetMasterVolumeLevelScalar().ok()? * 100.0;
            let muted = endpoint.GetMute().ok()?.as_bool();
            Some(VolumeState { level: level.round().clamp(0.0, 100.0) as u8, muted })
        }
    }

    /// The built-in panel's brightness through WMI (`root\WMI`, `WmiMonitorBrightness`), the same
    /// level Windows' own brightness slider shows. Only laptop and tablet panels have it: a
    /// desktop's monitors answer with no instances, which is `supported: false`. (External
    /// monitors could be asked over DDC/CI, but each read takes tens of milliseconds on the
    /// monitor's bus, and Windows doesn't show its own pop-up for them anyway.)
    #[derive(Default)]
    struct Brightness {
        services: Option<IWbemServices>,
        /// Don't ask before this: WMI failed, or there's no panel to ask about.
        wait_until: Option<Instant>,
    }

    impl Brightness {
        fn read(&mut self) -> BrightnessState {
            if self.wait_until.is_some_and(|t| Instant::now() < t) {
                return BrightnessState::default();
            }
            self.wait_until = None;
            if self.services.is_none() {
                self.services = connect_wmi();
            }
            let Some(services) = &self.services else {
                self.wait_until = Some(Instant::now() + BRIGHTNESS_RETRY);
                return BrightnessState::default();
            };
            match current_brightness(services) {
                Ok(Some(level)) => BrightnessState { level: level.min(100) as u8, supported: true },
                Ok(None) => {
                    self.wait_until = Some(Instant::now() + BRIGHTNESS_RETRY);
                    BrightnessState::default()
                }
                Err(e) => {
                    eprintln!("hoverpill: reading screen brightness failed: {e}");
                    // The WMI service may have restarted: connect afresh next time.
                    self.services = None;
                    self.wait_until = Some(Instant::now() + BRIGHTNESS_RETRY);
                    BrightnessState::default()
                }
            }
        }
    }

    fn connect_wmi() -> Option<IWbemServices> {
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER).ok()?;
            let empty = BSTR::new();
            let services = locator
                .ConnectServer(&BSTR::from("ROOT\\WMI"), &empty, &empty, &empty, 0, &empty, None::<&IWbemContext>)
                .ok()?;
            // Calls on the proxy run as this user. The process-wide `CoInitializeSecurity` isn't
            // ours to call (the webview owns COM in this process), so set it on the proxy.
            // 10 = RPC_C_AUTHN_WINNT, 0 = RPC_C_AUTHZ_NONE.
            CoSetProxyBlanket(
                &services,
                10,
                0,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .ok()?;
            Some(services)
        }
    }

    /// The first active panel's level, 0–100, or `None` when there's no panel.
    fn current_brightness(services: &IWbemServices) -> windows::core::Result<Option<u32>> {
        unsafe {
            let rows: IEnumWbemClassObject = services.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active = TRUE"),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None::<&IWbemContext>,
            )?;
            let mut row: [Option<IWbemClassObject>; 1] = [None];
            let mut returned = 0u32;
            // S_FALSE with nothing returned is "no rows", not an error.
            rows.Next(WBEM_INFINITE, &mut row, &mut returned).ok()?;
            let Some(row) = row[0].take().filter(|_| returned == 1) else {
                return Ok(None);
            };
            let mut value = VARIANT::default();
            row.Get(w!("CurrentBrightness"), 0, &mut value, None, None)?;
            let level = VariantToUInt32(&value);
            let _ = VariantClear(&mut value);
            Ok(Some(level?))
        }
    }

    fn battery() -> BatteryState {
        let mut status = SYSTEM_POWER_STATUS::default();
        if unsafe { GetSystemPowerStatus(&mut status) }.is_ok() {
            // 128 means "no system battery"; 255 is "unknown" percentage.
            let supported = status.BatteryFlag != 128 && status.BatteryLifePercent != 255;
            if supported {
                return BatteryState {
                    level: status.BatteryLifePercent.min(100),
                    // 1 = plugged in (AC), 0 = running on the battery.
                    charging: status.ACLineStatus == 1,
                    supported: true,
                };
            }
        }
        BatteryState::default()
    }

    pub fn spawn_worker(app: AppHandle) {
        std::thread::Builder::new()
            .name("hoverpill-system".into())
            .spawn(move || {
                let mut backend = Backend::new();
                let mut last: Option<SystemState> = None;
                loop {
                    let state = backend.read();
                    if last.as_ref() != Some(&state) {
                        send(&app, "pill:system", state.clone());
                        last = Some(state);
                    }
                    std::thread::sleep(POLL);
                }
            })
            .expect("spawning the volume/brightness/battery thread");
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Instant;

    /// How long a quiet spell lasts before everything is read again as a safety net (the mixer
    /// subscription normally wakes the loop on a change).
    const TICK: Duration = Duration::from_millis(500);
    const VOLUME_EVERY: Duration = Duration::from_secs(3);
    const BRIGHTNESS_EVERY: Duration = Duration::from_secs(1);
    /// A few small sysfs reads; quick enough that the charging card follows the plug.
    const BATTERY_EVERY: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy)]
    enum Mixer {
        Pactl,
        Wpctl,
    }

    struct Backend {
        mixer: Option<Mixer>,
        backlight: Option<(PathBuf, u64)>,
        battery: Option<PathBuf>,
    }

    impl Backend {
        fn new() -> Self {
            Self { mixer: None, backlight: find_backlight(), battery: find_battery() }
        }
    }

    /// PipeWire and PulseAudio both answer `pactl`; a bare WirePlumber has `wpctl`.
    fn read_volume(mixer: &mut Option<Mixer>) -> Option<VolumeState> {
        let order = match mixer {
            Some(Mixer::Wpctl) => [Mixer::Wpctl, Mixer::Pactl],
            _ => [Mixer::Pactl, Mixer::Wpctl],
        };
        for tool in order {
            let state = match tool {
                Mixer::Pactl => pactl_volume(),
                Mixer::Wpctl => wpctl_volume(),
            };
            if state.is_some() {
                *mixer = Some(tool);
                return state;
            }
        }
        None
    }

    fn pactl_volume() -> Option<VolumeState> {
        let volume = run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?;
        let mute = run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])?;
        pactl_state(&volume, &mute)
    }

    fn pactl_state(volume: &str, mute: &str) -> Option<VolumeState> {
        // "Volume: front-left: 45875 /  70% / -9.29 dB, ..." — the first percentage is the level.
        let level = volume.split('%').next()?.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse::<u8>().ok()?;
        // "Mute: yes" / "Mute: no".
        Some(VolumeState { level: level.min(100), muted: mute.trim().to_ascii_lowercase().ends_with("yes") })
    }

    fn wpctl_volume() -> Option<VolumeState> {
        wpctl_state(&run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?)
    }

    fn wpctl_state(out: &str) -> Option<VolumeState> {
        // "Volume: 0.65" or "Volume: 0.65 [MUTED]".
        let level = out.split_whitespace().nth(1)?.parse::<f32>().ok()?;
        Some(VolumeState { level: (level * 100.0).round().clamp(0.0, 100.0) as u8, muted: out.contains("[MUTED]") })
    }

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn find_backlight() -> Option<(PathBuf, u64)> {
        for entry in fs::read_dir("/sys/class/backlight").ok()? {
            let dir = entry.ok()?.path();
            let max = fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse::<u64>().ok()?;
            if max > 0 {
                return Some((dir, max));
            }
        }
        None
    }

    fn read_brightness(backlight: &Option<(PathBuf, u64)>) -> BrightnessState {
        let Some((dir, max)) = backlight else { return BrightnessState::default() };
        // The raw backlight value the desktop last wrote; good enough for a level bar.
        let level = fs::read_to_string(dir.join("brightness"))
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| (v.min(*max) * 100 / max).min(100) as u8)
            .unwrap_or(0);
        BrightnessState { level, supported: true }
    }

    fn find_battery() -> Option<PathBuf> {
        for entry in fs::read_dir("/sys/class/power_supply").ok()? {
            let dir = entry.ok()?.path();
            let kind = fs::read_to_string(dir.join("type")).unwrap_or_default();
            if kind.trim() == "Battery" && dir.join("capacity").exists() {
                return Some(dir);
            }
        }
        None
    }

    /// The mains adapter, when the machine reports one. USB-C supplies are left out: a phone
    /// plugged into a port would otherwise look like a charger.
    fn on_ac() -> Option<bool> {
        for entry in fs::read_dir("/sys/class/power_supply").ok()? {
            let dir = entry.ok()?.path();
            if fs::read_to_string(dir.join("type")).ok().is_some_and(|kind| kind.trim() == "Mains") {
                if let Ok(online) = fs::read_to_string(dir.join("online")) {
                    return Some(online.trim() == "1");
                }
            }
        }
        None
    }

    fn read_battery(battery: &Option<PathBuf>) -> BatteryState {
        let Some(dir) = battery else { return BatteryState::default() };
        let Some(level) = fs::read_to_string(dir.join("capacity")).ok().and_then(|v| v.trim().parse::<u8>().ok()) else {
            return BatteryState::default();
        };
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        battery_state(level, &status, on_ac())
    }

    /// The adapter is the better answer where there is one: a full battery on mains still says
    /// "Not charging", and that isn't the same as being unplugged. Status alone otherwise.
    fn battery_state(level: u8, status: &str, adapter: Option<bool>) -> BatteryState {
        let charging = adapter.unwrap_or_else(|| matches!(status.trim(), "Charging" | "Full"));
        BatteryState { level: level.min(100), charging, supported: true }
    }

    /// `pactl subscribe` prints a line per change on the sound server; every one is a reason to
    /// re-read the default sink, without polling `pactl` twice a second. If it isn't available,
    /// the loop's own timer covers it.
    fn subscribe(tx: mpsc::Sender<()>) {
        let _ = std::thread::Builder::new().name("hoverpill-mixer".into()).spawn(move || {
            let Ok(mut child) =
                Command::new("pactl").args(["subscribe"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            else {
                return;
            };
            let Some(stdout) = child.stdout.take() else { return };
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if (line.contains("sink") || line.contains("server")) && tx.send(()).is_err() {
                    break;
                }
            }
        });
    }

    fn due(at: Option<Instant>, every: Duration) -> bool {
        at.is_none_or(|t| t.elapsed() >= every)
    }

    pub fn spawn_worker(app: AppHandle) {
        std::thread::Builder::new()
            .name("hoverpill-system".into())
            .spawn(move || {
                let mut backend = Backend::new();
                let (tx, rx) = mpsc::channel();
                // Kept alive here so the channel never reports Disconnected if the subscriber
                // thread could not start; the timer alone still reads everything then.
                let _keep_alive = tx.clone();
                subscribe(tx);

                let mut state = SystemState::default();
                let mut last: Option<SystemState> = None;
                let (mut volume_at, mut brightness_at, mut battery_at) = (None, None, None);
                loop {
                    // A mixer event wakes us early; otherwise this is just the safety-net tick.
                    let changed = matches!(rx.recv_timeout(TICK), Ok(()));
                    if changed || due(volume_at, VOLUME_EVERY) {
                        if let Some(volume) = read_volume(&mut backend.mixer) {
                            state.volume = volume;
                        }
                        volume_at = Some(Instant::now());
                    }
                    if due(brightness_at, BRIGHTNESS_EVERY) {
                        state.brightness = read_brightness(&backend.backlight);
                        brightness_at = Some(Instant::now());
                    }
                    if due(battery_at, BATTERY_EVERY) {
                        state.battery = read_battery(&backend.battery);
                        battery_at = Some(Instant::now());
                    }
                    if last.as_ref() != Some(&state) {
                        send(&app, "pill:system", state.clone());
                        last = Some(state.clone());
                    }
                }
            })
            .expect("spawning the volume/brightness/battery thread");
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pactl_output_reads() {
            let volume = "Volume: front-left: 45875 /  70% / -9.29 dB,   front-right: 45875 /  70% / -9.29 dB\n";
            assert_eq!(pactl_state(volume, "Mute: no\n"), Some(VolumeState { level: 70, muted: false }));
            assert_eq!(pactl_state(volume, "Mute: yes\n"), Some(VolumeState { level: 70, muted: true }));
            // Above 100% (amplified sinks) still shows a full bar.
            assert_eq!(pactl_state("Volume: front-left: 99999 / 150%\n", "Mute: no\n").unwrap().level, 100);
            assert_eq!(pactl_state("no volume here", "Mute: no\n"), None);
        }

        #[test]
        fn wpctl_output_reads() {
            assert_eq!(wpctl_state("Volume: 0.65\n"), Some(VolumeState { level: 65, muted: false }));
            assert_eq!(wpctl_state("Volume: 1.00 [MUTED]\n"), Some(VolumeState { level: 100, muted: true }));
            assert_eq!(wpctl_state("Volume: 0.655\n").unwrap().level, 66);
            assert_eq!(wpctl_state("muted\n"), None);
        }

        #[test]
        fn brightness_is_a_percentage_of_the_maximum() {
            let dir = temp_dir("brightness");
            fs::write(dir.join("brightness"), "19172\n").unwrap();
            assert_eq!(read_brightness(&Some((dir.clone(), 62451))), BrightnessState { level: 30, supported: true });
            // A value above the maximum (the desktop hasn't settled yet) stays at 100.
            fs::write(dir.join("brightness"), "70000\n").unwrap();
            assert_eq!(read_brightness(&Some((dir.clone(), 62451))).level, 100);
            // No backlight at all: a desktop, or an external monitor.
            assert_eq!(read_brightness(&None), BrightnessState::default());
            let _ = fs::remove_dir_all(dir);
        }

        #[test]
        fn battery_prefers_the_adapter() {
            // Plugged in at 100%, the battery still says "Not charging".
            assert_eq!(
                battery_state(100, "Not charging\n", Some(true)),
                BatteryState { level: 100, charging: true, supported: true }
            );
            assert!(!battery_state(64, "Discharging\n", Some(false)).charging);
            // Without an adapter entry the status is all there is to go on.
            assert!(battery_state(64, "Charging\n", None).charging);
            assert!(battery_state(64, "Full\n", None).charging);
            assert_eq!(battery_state(150, "Discharging\n", None).level, 100);
        }

        fn temp_dir(what: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("hoverpill-test-{}-{what}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            dir
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::*;

    pub fn spawn_worker(_app: AppHandle) {}
}

/// Watches volume, brightness and battery and pushes `pill:system` whenever one changes.
pub fn spawn_worker(app: AppHandle) {
    platform::spawn_worker(app);
}
