import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  checkForUpdates,
  claudeHookInstalled,
  getAutostart,
  inTauri,
  installUpdate,
  listMonitors,
  mediaPlayers,
  notifyingApps,
  openLink,
  pendingUpdate,
  recentNotificationApps,
  setAutostart,
  setClaudeHook,
} from "../lib/bridge";
import { useEditableSettings } from "../lib/useSettings";
import { DEFAULT_SETTINGS, IS_LINUX, RANGES, type MonitorInfo, type PlayerInfo, type Settings, type UpdateOffer } from "../lib/types";
import { Row, Section, SelectRow, SliderRow, SwitchRow, ToggleList } from "./controls";
import { AccountsTab } from "./AccountsTab";
import mark from "../../assets/brand/hoverpill-icon.svg";

type Update = (change: (s: Settings) => Settings, debounceMs?: number) => void;

const TABS = [
  { id: "general", label: "General", icon: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm7.4-3a7.4 7.4 0 0 0-.1-1.2l2-1.6-2-3.4-2.4 1a7.6 7.6 0 0 0-2-1.2L14.5 3h-5l-.4 2.6a7.6 7.6 0 0 0-2 1.2l-2.4-1-2 3.4 2 1.6a7.4 7.4 0 0 0 0 2.4l-2 1.6 2 3.4 2.4-1a7.6 7.6 0 0 0 2 1.2l.4 2.6h5l.4-2.6a7.6 7.6 0 0 0 2-1.2l2.4 1 2-3.4-2-1.6c.1-.4.1-.8.1-1.2Z" },
  { id: "display", label: "Display", icon: "M3 5h18v11H3zM8 20h8M12 16v4" },
  { id: "notifications", label: "Notifications", icon: "M18 16V11a6 6 0 1 0-12 0v5l-2 2h16l-2-2ZM10 20a2 2 0 0 0 4 0" },
  { id: "media", label: "Media", icon: "M9 18V6l11-2v12M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm11-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z" },
  { id: "status", label: "Status", icon: "M3 12h4l2.5-6 4 12L16 12h5" },
  { id: "accounts", label: "Accounts", icon: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4.5 20a7.5 7.5 0 0 1 15 0" },
] as const;
type TabId = (typeof TABS)[number]["id"];

const TAB_KEY = "hoverpill.settings.tab";

/** The top-strip reservation uses a Windows shell API; there's no equivalent wired up on Linux yet. */
const IS_WINDOWS = navigator.userAgent.includes("Windows");

function initialTab(): TabId {
  try {
    const saved = localStorage.getItem(TAB_KEY);
    if (TABS.some((t) => t.id === saved)) return saved as TabId;
  } catch {
    // Storage can be unavailable; the first tab is fine.
  }
  return "general";
}

export function SettingsApp() {
  const { settings, update, error } = useEditableSettings();
  const [tab, setTab] = useState<TabId>(initialTab);
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  const select = (id: TabId, focus = false) => {
    setTab(id);
    if (focus) tabRefs.current[id]?.focus();
    try {
      localStorage.setItem(TAB_KEY, id);
    } catch {
      // Not remembering the tab is harmless.
    }
  };

  // Arrow keys move between tabs (WAI-ARIA tabs pattern, automatic activation).
  const onTabKey = (e: KeyboardEvent) => {
    const i = TABS.findIndex((t) => t.id === tab);
    const to = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: TABS.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    select(TABS[(to + TABS.length) % TABS.length].id, true);
  };

  return (
    <div className="app">
      <header className="header">
        <div className="brand">
          <img className="brand-mark" src={mark} alt="" width={28} height={28} />
          HoverPill
        </div>
        <div className="tabs" role="tablist" aria-label="Settings sections" onKeyDown={onTabKey}>
          {TABS.map((t) => (
            <button
              key={t.id}
              ref={(el) => {
                tabRefs.current[t.id] = el;
              }}
              id={`tab-${t.id}`}
              role="tab"
              type="button"
              className="tab"
              aria-selected={tab === t.id}
              aria-controls={`panel-${t.id}`}
              tabIndex={tab === t.id ? 0 : -1}
              onClick={() => select(t.id)}
            >
              <svg viewBox="0 0 24 24" aria-hidden>
                <path d={t.icon} />
              </svg>
              {t.label}
            </button>
          ))}
        </div>
      </header>

      <main className="panel" role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`} tabIndex={0}>
        {error && (
          <div className="banner" role="alert">
            Couldn't save: {error}
          </div>
        )}
        <UpdateBanner />
        {!settings ? (
          <div className="empty">Loading…</div>
        ) : (
          <>
            {tab === "general" && <GeneralTab settings={settings} update={update} />}
            {tab === "display" && <DisplayTab settings={settings} update={update} />}
            {tab === "notifications" && <NotificationsTab settings={settings} update={update} />}
            {tab === "media" && <MediaTab settings={settings} update={update} />}
            {tab === "status" && <StatusTab settings={settings} update={update} />}
            {tab === "accounts" && <AccountsTab settings={settings} update={update} />}
          </>
        )}
      </main>
    </div>
  );
}

/**
 * An update that was found earlier stays on offer here, even after "Later" in the notch put the
 * card away. On a .deb/.rpm install it points at the release page instead of restarting.
 */
function UpdateBanner() {
  const [offer, setOffer] = useState<UpdateOffer | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    pendingUpdate().then(setOffer, console.error);
  }, []);
  if (!offer) return null;
  const download = offer.mode === "download";
  const run = () => {
    if (download) {
      openLink("https://github.com/PreetinderSinghBadesha/HoverPill/releases/latest");
      return;
    }
    setBusy(true);
    setError(null);
    installUpdate().catch((e) => {
      setError(String(e));
      setBusy(false);
    });
  };
  return (
    <div className="banner is-update" role="status">
      <svg className="banner-icon" viewBox="0 0 24 24" aria-hidden>
        <path d="M12 4v11m-4.5-4.5L12 15l4.5-4.5M5 19h14" />
      </svg>
      <div className="banner-text">
        <strong>HoverPill {offer.version} is ready</strong>
        <span>{error ?? (download ? "Download the new version from the release page." : "Restart HoverPill to use it.")}</span>
      </div>
      <button type="button" className="button is-primary" disabled={busy} onClick={run}>
        {download ? "Download" : busy ? "Restarting…" : "Restart to update"}
      </button>
    </div>
  );
}

/** Re-runs `load` every `ms` while the tab is open, so lists pick up apps as they appear. */
function usePolled<T>(load: () => Promise<T>, initial: T, ms = 3000): T {
  const [value, setValue] = useState<T>(initial);
  useEffect(() => {
    let alive = true;
    const run = () => load().then((v) => alive && setValue(v), console.error);
    run();
    const t = window.setInterval(run, ms);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [load, ms]);
  return value;
}

function GeneralTab({ settings, update }: { settings: Settings; update: Update }) {
  const [autostart, setAutostartState] = useState<boolean | null>(null);
  const [autostartError, setAutostartError] = useState<string | null>(null);
  const [version, setVersion] = useState("");

  useEffect(() => {
    getAutostart().then(setAutostartState, console.error);
    if (inTauri) getVersion().then(setVersion, console.error);
  }, []);

  const toggle = (on: boolean) => {
    setAutostartError(null);
    setAutostart(on)
      .then(setAutostartState)
      .catch((e) => {
        setAutostartError(String(e));
        getAutostart().then(setAutostartState, console.error);
      });
  };

  return (
    <>
      <Section title="Startup">
        <SwitchRow
          title="Start at login"
          description={autostartError ?? "Open HoverPill automatically when you sign in."}
          checked={autostart ?? false}
          disabled={autostart === null}
          onChange={toggle}
        />
      </Section>
      <ClaudeSection />
      <UpdatesSection settings={settings} update={update} />
      <Section title="About">
        <div className="about">
          <p>
            HoverPill {version && <span className="muted">v{version}</span>}
          </p>
          <p className="muted">The tray icon has quick actions, including Quit. Opening HoverPill again while it's running brings you here.</p>
        </div>
      </Section>
    </>
  );
}

/** New versions: checked for now and then, and offered in the notch; or checked right now. */
function UpdatesSection({ settings, update }: { settings: Settings; update: Update }) {
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const check = () => {
    setBusy(true);
    setStatus("Checking…");
    checkForUpdates()
      .then((v) =>
        setStatus(
          v
            ? v.mode === "restart"
              ? `HoverPill ${v.version} is ready: restart from the card in the notch.`
              : `HoverPill ${v.version} is out: download it from the card in the notch.`
            : "You have the latest version.",
        ),
      )
      .catch((e) => setStatus(`Couldn't check: ${e}`))
      .finally(() => setBusy(false));
  };
  return (
    <Section title="Updates">
      <SwitchRow
        title="Check for updates automatically"
        description="Now and then HoverPill looks for a new version, downloads it in the background, and offers to restart into it from the notch."
        checked={settings.updates.autoCheck}
        onChange={(autoCheck) => update((s) => ({ ...s, updates: { autoCheck } }))}
      />
      <Row title="Check now" description={status ?? "Look for a new version right away."}>
        <button type="button" className="button" disabled={busy} onClick={check}>
          Check for updates
        </button>
      </Row>
    </Section>
  );
}

/** Claude Code's permission requests: in the notch (the hook is installed) or in the terminal. */
function ClaudeSection() {
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    claudeHookInstalled().then(setInstalled, console.error);
  }, []);

  const change = (on: boolean) => {
    setError(null);
    setClaudeHook(on).then(setInstalled, (e) => setError(String(e)));
  };

  return (
    <Section title="Claude Code">
      <Row
        title="Permission requests in the notch"
        description={
          error ??
          (installed
            ? "On. Claude Code asks here, with Approve, Deny and Always allow. Sessions started before this was turned on still ask in the terminal."
            : "Off. Claude Code asks in the terminal. Turn this on to answer from the notch instead; new Claude Code sessions will use it.")
        }
      >
        <button type="button" className="button" disabled={installed === null} onClick={() => change(!installed)}>
          {installed ? "Turn off" : "Turn on"}
        </button>
      </Row>
    </Section>
  );
}

/** Keys for the shortcut that opens the notch. Each must parse in Rust (see the test in shortcut.rs). */
const NO_SHORTCUT = "__none__";
const OPEN_SHORTCUTS = [
  { value: "Super+Alt+Space", label: IS_LINUX ? "Super + Alt + Space" : "Win + Alt + Space" },
  { value: "Ctrl+Alt+N", label: "Ctrl + Alt + N" },
  { value: "Ctrl+Alt+H", label: "Ctrl + Alt + H" },
  { value: NO_SHORTCUT, label: "Off" },
];

function DisplayTab({ settings, update }: { settings: Settings; update: Update }) {
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);
  useEffect(() => {
    listMonitors().then(setMonitors, console.error);
  }, []);

  const d = settings.display;
  const PRIMARY = "__primary__";
  const chosenMissing = d.monitor !== null && !monitors.some((m) => m.name === d.monitor);
  const options = [
    { value: PRIMARY, label: "Primary monitor" },
    ...monitors.map((m) => ({ value: m.name, label: m.label })),
    ...(chosenMissing && d.monitor ? [{ value: d.monitor, label: "Disconnected monitor (using primary)" }] : []),
  ];
  const setDisplay = (patch: Partial<Settings["display"]>, debounceMs = 0) =>
    update((s) => ({ ...s, display: { ...s.display, ...patch } }), debounceMs);

  return (
    <>
      <Section title="Position">
        <SelectRow
          title="Monitor"
          description="The pill sits at the top centre of this screen."
          value={d.monitor ?? PRIMARY}
          options={options}
          onChange={(v) => setDisplay({ monitor: v === PRIMARY ? null : v })}
        />
        <SliderRow
          title="Distance from top"
          description="At 0 the notch is attached to the top edge. Any gap turns it into a floating pill."
          value={d.topOffset}
          {...RANGES.topOffset}
          format={(v) => `${v} px`}
          onChange={(v) => setDisplay({ topOffset: v }, 150)}
        />
      </Section>
      <Section title="Opening">
        <SwitchRow
          title="Open when the pointer rests on the notch"
          description="Push the pointer against the top edge above the notch to open it. With this off, pointing at the closed notch fades it out so you can see what's underneath. Clicks on the closed notch always go through to it."
          checked={d.openOnHover}
          onChange={(openOnHover) => setDisplay({ openOnHover })}
        />
        <SelectRow
          title="Shortcut to open"
          description="Opens the notch from any app, with the keyboard; Esc closes it again. Ctrl + Alt + G opens its ChatGPT tab (change that under Accounts)."
          value={d.shortcut || NO_SHORTCUT}
          options={OPEN_SHORTCUTS}
          onChange={(v) => setDisplay({ shortcut: v === NO_SHORTCUT ? "" : v })}
        />
      </Section>
      <Section title="Staying out of the way">
        <SwitchRow
          title="Shrink when nothing is happening"
          description="With nothing playing and no notification, the notch becomes a thin bar."
          checked={d.compactIdle}
          onChange={(compactIdle) => setDisplay({ compactIdle })}
        />
        <SwitchRow
          title="Hide during fullscreen apps"
          description="Videos, games and presentations get the whole screen. Claude Code requests still appear."
          checked={d.hideInFullscreen}
          onChange={(hideInFullscreen) => setDisplay({ hideInFullscreen })}
        />
        {IS_LINUX && (
          <SwitchRow
            title="GNOME Shell helper for Wayland fullscreen"
            description="Installs and enables a small GNOME Shell extension so apps that talk to Wayland directly (Firefox, GTK4 apps) hide the notch behind them too. It needs one logout to load. X11 apps are detected either way, and a helper you switch off in GNOME's Extensions app stays off."
            checked={d.gnomeHelper}
            onChange={(gnomeHelper) => setDisplay({ gnomeHelper })}
          />
        )}
        <SwitchRow
          title="Keep windows below the notch"
          description={
            IS_WINDOWS
              ? "Reserves a strip across the top of the screen, like a taskbar, so maximised windows start below the notch. You lose that strip's height across the whole width."
              : "Only available on Windows for now."
          }
          checked={d.reserveSpace}
          disabled={!IS_WINDOWS}
          onChange={(reserveSpace) => setDisplay({ reserveSpace })}
        />
        {IS_LINUX && (
          <SwitchRow
            title="Show on every workspace"
            description="Keep the pill visible when you switch workspaces, instead of only on the one it was opened on."
            checked={d.allWorkspaces}
            onChange={(allWorkspaces) => setDisplay({ allWorkspaces })}
          />
        )}
      </Section>
      <Section title="Size">
        <SliderRow
          title="Pill size"
          description="Scales the pill and everything in it."
          value={d.scale}
          {...RANGES.scale}
          format={(v) => `${Math.round(v * 100)}%`}
          onChange={(v) => setDisplay({ scale: v }, 150)}
        />
      </Section>
      <div className="actions">
        <button type="button" className="button" onClick={() => setDisplay(DEFAULT_SETTINGS.display)}>
          Reset display settings
        </button>
      </div>
    </>
  );
}

function NotificationsTab({ settings, update }: { settings: Settings; update: Update }) {
  const recent = usePolled(recentNotificationApps, [] as string[]);
  const [installed, setInstalled] = useState<string[] | null>(null);
  useEffect(() => {
    notifyingApps().then(setInstalled, () => setInstalled([]));
  }, []);

  const n = settings.notifications;
  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  const blocked = (app: string) => n.blockedApps.some((b) => same(b, app));
  // Blocked apps stay listed even if they've gone quiet, so they can be turned back on.
  const recentApps = [...recent, ...n.blockedApps.filter((b) => !recent.some((r) => same(r, b)) && !installed?.some((i) => same(i, b)))];
  const otherApps = (installed ?? []).filter((app) => !recentApps.some((r) => same(r, app)));
  const toItems = (apps: string[]) => apps.map((app) => ({ key: app, label: app, on: !blocked(app) }));
  const toggle = (app: string, on: boolean) =>
    setNotifications({ blockedApps: on ? n.blockedApps.filter((b) => !same(b, app)) : [...n.blockedApps, app] });

  const setNotifications = (patch: Partial<Settings["notifications"]>, debounceMs = 0) =>
    update((s) => ({ ...s, notifications: { ...s.notifications, ...patch } }), debounceMs);

  return (
    <>
      <Section title="Notifications">
        <SwitchRow
          title="Show notifications in the pill"
          description="Mirror notifications from other apps."
          checked={n.enabled}
          onChange={(enabled) => setNotifications({ enabled })}
        />
        <SliderRow
          title="Show each notification for"
          description="Hovering the pill keeps a notification open."
          value={n.durationSecs}
          {...RANGES.durationSecs}
          format={(v) => `${v.toFixed(1)} s`}
          onChange={(durationSecs) => setNotifications({ durationSecs }, 150)}
        />
      </Section>
      <Section title="Recently sent notifications">
        <ToggleList
          items={toItems(recentApps)}
          empty={<>Apps show up here after they send a notification. Turn one off to keep it out of the pill.</>}
          onToggle={toggle}
        />
      </Section>
      {installed === null ? (
        <Note>Looking up the other apps on this PC that can send notifications…</Note>
      ) : (
        otherApps.length > 0 && (
          <Section title="Other apps that can send notifications">
            <ToggleList items={toItems(otherApps)} empty={null} onToggle={toggle} />
          </Section>
        )
      )}
      <Note>
        Gmail and other websites send notifications through your browser, so they're listed under the browser (for example
        Brave). To give a site its own entry, install it as an app from the browser's menu, then it notifies under its own name.
      </Note>
      {!n.enabled && <Note>Notifications are off, so these app choices have no effect right now.</Note>}
    </>
  );
}

function MediaTab({ settings, update }: { settings: Settings; update: Update }) {
  const players = usePolled(mediaPlayers, [] as PlayerInfo[]);
  const m = settings.media;
  const setMedia = (patch: Partial<Settings["media"]>) => update((s) => ({ ...s, media: { ...s.media, ...patch } }));

  return (
    <>
      <Section title="Media">
        <SwitchRow
          title="Show media in the pill"
          description="Track, artwork and playback controls for whatever is playing."
          checked={m.enabled}
          onChange={(enabled) => setMedia({ enabled })}
        />
      </Section>
      <Section title="Players">
        <ToggleList
          items={players.map((p) => ({ key: p.id, label: p.label, on: !m.ignoredPlayers.includes(p.id) }))}
          empty={<>Players show up here while they're open. Turn one off (a browser, say) and the pill shows the next one instead.</>}
          onToggle={(id, on) =>
            setMedia({ ignoredPlayers: on ? m.ignoredPlayers.filter((p) => p !== id) : [...m.ignoredPlayers, id] })
          }
        />
      </Section>
      {!m.enabled && <Note>Media is off, so these player choices have no effect right now.</Note>}
    </>
  );
}

function Note({ children }: { children: ReactNode }) {
  return <p className="note">{children}</p>;
}

/** The dots in the closed notch, and the cards that replace the system's own volume pop-up. */
function StatusTab({ settings, update }: { settings: Settings; update: Update }) {
  const p = settings.privacy;
  const s = settings.system;
  const setPrivacy = (patch: Partial<Settings["privacy"]>) => update((v) => ({ ...v, privacy: { ...v.privacy, ...patch } }));
  const setSystem = (patch: Partial<Settings["system"]>) => update((v) => ({ ...v, system: { ...v.system, ...patch } }));

  return (
    <>
      <Section title="Camera and microphone">
        <SwitchRow
          title="Green dot when the camera is on"
          description="A dot in the closed notch while any app uses the camera. Open the notch to see which app, and since when."
          checked={p.cameraIndicator}
          onChange={(cameraIndicator) => setPrivacy({ cameraIndicator })}
        />
        <SwitchRow
          title="Orange dot when the microphone is on"
          description="The same for the microphone, in orange."
          checked={p.microphoneIndicator}
          onChange={(microphoneIndicator) => setPrivacy({ microphoneIndicator })}
        />
      </Section>
      <Section title="Volume, brightness and battery">
        <SwitchRow
          title="Volume changes"
          description="A level bar, with the mute state, when the volume changes."
          checked={s.showVolume}
          onChange={(showVolume) => setSystem({ showVolume })}
        />
        <SwitchRow
          title="Brightness changes"
          description="A level bar on laptops when the screen brightness changes."
          checked={s.showBrightness}
          onChange={(showBrightness) => setSystem({ showBrightness })}
        />
        <SwitchRow
          title="Charging and low battery"
          description="A short card when the charger goes in, and a warning when the battery drops below 20%."
          checked={s.showBattery}
          onChange={(showBattery) => setSystem({ showBattery })}
        />
      </Section>
      {IS_LINUX && (
        <Note>
          On Linux the microphone dot follows recording streams in PipeWire (or PulseAudio), and the camera dot follows
          apps that have the camera open. Volume, brightness and battery come from the sound server, the backlight and
          the battery.
        </Note>
      )}
    </>
  );
}
