import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  DEFAULT_SETTINGS,
  type AccountsStatus,
  type CalendarEvent,
  type ChatChunk,
  type ChatTurn,
  type Decision,
  type DockEvent,
  type Layout,
  type ModelInfo,
  type MediaAction,
  type MediaState,
  type MonitorInfo,
  type NotificationItem,
  type PermissionRequest,
  type PlayerInfo,
  type PrivacyState,
  type Screenshot,
  type SystemState,
  type UpdateOffer,
  type Rect,
  type Settings,
} from "./types";

/** False when the UI is opened in a plain browser (`npm run dev`) for styling work. */
export const inTauri = "__TAURI_INTERNALS__" in window;

export type PillEvents = {
  /** Open (true) or close the notch. */
  "pill:hover": boolean;
  /** The pointer is over the closed notch: fade it so what's underneath shows. */
  "pill:peek": boolean;
  /** The pointer is on the notch itself, not pushing against the top edge (see `pickView`). */
  "pill:on-card": boolean;
  /** A fullscreen app covers the notch's monitor. */
  "pill:fullscreen": boolean;
  /** Typing in the notch ended because another app came to the front (Windows). */
  "pill:typing": boolean;
  /** A newer HoverPill is ready (or, for a .deb/.rpm install, out). */
  "pill:update": UpdateOffer;
  /** The shortcut to open the notch was pressed: open where it was, with the keyboard. */
  "pill:summon": null;
  /** The ChatGPT shortcut was pressed: show the ChatGPT tab, ready to type. */
  "pill:ask": null;
  /** Ctrl is held over the closed notch: it will take a click, to be dragged. */
  "pill:grab": boolean;
  /** Became the notch, docked somewhere else, moved, or was dropped after a drag. */
  "pill:dock": DockEvent;
  /** Linux, mid-drag: the window hit the edge of the work area, so the bubble moves this far (CSS px) inside it to stay under the pointer. */
  "pill:drag-offset": [number, number];
  "pill:permission": PermissionRequest;
  /** Claude stopped waiting (interrupted or timed out); drop the card. */
  "pill:permission-cancel": { id: string };
  "pill:notification": NotificationItem;
  "pill:media": MediaState | null;
  /** Apps using the camera or microphone (Windows; nothing on Linux yet). */
  "pill:privacy": PrivacyState;
  /** Volume, brightness or battery changed. */
  "pill:system": SystemState;
  /** Sent to every window after a save, with the values Rust actually stored. */
  "settings:changed": Settings;
  /** Upcoming Google Calendar events (empty when disconnected or turned off). */
  "pill:calendar": CalendarEvent[];
  /** A piece of a streamed ChatGPT answer, or its end. */
  "pill:chatgpt": ChatChunk;
  /** Google or ChatGPT connected, disconnected, or hit a problem. */
  "accounts:changed": AccountsStatus;
};

export type Subscription = { off: () => void; registered: Promise<void> };

export function on<K extends keyof PillEvents>(event: K, cb: (payload: PillEvents[K]) => void): Subscription {
  if (!inTauri) return { off: () => {}, registered: Promise.resolve() };
  let disposed = false;
  let unlisten: (() => void) | undefined;
  const registered = listen<PillEvents[K]>(event, (e) => cb(e.payload)).then((fn) => {
    if (disposed) fn();
    else unlisten = fn;
  });
  return {
    off: () => {
      disposed = true;
      unlisten?.();
    },
    registered,
  };
}

/** Releases events Rust queued while the UI was loading. Call once every listener is registered. */
export function pillReady() {
  if (inTauri) invoke("pill_ready").catch(console.error);
}

/** `interactive`: the notch should take clicks while the pointer is over it (open, or a card with buttons). */
export function setHitRect(rect: Rect, interactive: boolean) {
  if (inTauri) invoke("set_hit_rect", { rect, interactive }).catch(console.error);
}

/** Where it's laid out right now; changes arrive as `pill:dock`. */
export async function dockLayout(): Promise<Layout> {
  return inTauri ? invoke<Layout>("dock_layout") : demoLayout();
}

/** For `npm run dev`: `?dock=left` (or right, top) shows it docked against that edge. */
function demoLayout(): Layout {
  const edge = new URLSearchParams(location.search).get("dock");
  const [w, h] = [window.innerWidth, window.innerHeight];
  if (edge === "left" || edge === "right") return { mode: "docked", edge, x: edge === "left" ? 0 : w, y: h / 2 };
  if (edge === "top") return { mode: "docked", edge, x: w * 0.3, y: 0 };
  return { mode: "notch" };
}

/** Ctrl+press on the pill: from here Rust moves the window with the pointer. */
export function beginDrag() {
  if (inTauri) invoke("begin_drag").catch(console.error);
}

/** The button came up: Rust docks it where it was dropped and answers with `pill:dock`. */
export async function endDrag(): Promise<void> {
  if (inTauri) await invoke("end_drag");
}

/** Installs the downloaded update and restarts into it. Rejects with a readable message. */
export async function installUpdate(): Promise<void> {
  if (inTauri) await invoke("install_update");
}

/** Checks for a newer version now: what to do about it, or null if this is the latest. */
export async function checkForUpdates(): Promise<UpdateOffer | null> {
  return inTauri ? invoke<UpdateOffer | null>("check_for_updates") : null;
}

/**
 * The update the last check found, if it hasn't been installed. Settings asks on open, so an
 * update put off with "Later" in the notch is still offered there.
 */
export async function pendingUpdate(): Promise<UpdateOffer | null> {
  return inTauri ? invoke<UpdateOffer | null>("pending_update") : null;
}

export function openSettings() {
  if (inTauri) invoke("open_settings").catch(console.error);
}

export function resolvePermission(id: string, decision: Decision) {
  if (inTauri) invoke("resolve_permission", { id, decision }).catch(console.error);
}

export function mediaCommand(action: MediaAction) {
  if (inTauri) invoke("media_command", { action }).catch(console.error);
}

/** What's playing right now; later changes arrive as `pill:media`. */
export async function currentMedia(): Promise<MediaState | null> {
  return inTauri ? invoke<MediaState | null>("current_media") : null;
}

/** While true the notch stays open, wherever the pointer goes (the lock button). */
export function pinNotch(pinned: boolean) {
  if (inTauri) invoke("notch_pinned", { pinned }).catch(console.error);
}

/** While true the notch keeps the keyboard (and stays open) so a question can be typed. */
export function notchTyping(active: boolean) {
  if (inTauri) invoke("notch_typing", { active }).catch(console.error);
}

// ---------- calendar ----------

export async function calendarEvents(): Promise<CalendarEvent[]> {
  return inTauri ? invoke<CalendarEvent[]>("calendar_events") : demoEvents();
}

export function refreshCalendar() {
  if (inTauri) invoke("calendar_refresh").catch(console.error);
}

/** For `npm run dev`: a few events relative to now. */
function demoEvents(): CalendarEvent[] {
  const at = (h: number) => new Date(Date.now() + h * 3600_000).toISOString();
  return [
    { id: "a", title: "Design review", start: at(0.6), end: at(1.4), allDay: false, link: "https://meet.google.com/", calendar: "Work", color: "#4E9A5E" },
    { id: "b", title: "Dentist", start: at(5), end: at(6), allDay: false, location: "Sector 17", calendar: "Personal" },
    { id: "c", title: "Team standup", start: at(24), end: at(24.25), allDay: false, calendar: "Work", color: "#4E9A5E" },
  ];
}

// ---------- accounts ----------

const NOT_CONNECTED = { connected: false, busy: false, planUsage: false };

export async function accountsStatus(): Promise<AccountsStatus> {
  return inTauri ? invoke<AccountsStatus>("accounts_status") : { google: NOT_CONNECTED, chatgpt: NOT_CONNECTED };
}

/** Opens the browser to sign in; resolves when the user is done there. Rejects with a readable message. */
export async function connectGoogle(): Promise<void> {
  if (inTauri) await invoke("google_connect");
}

export async function disconnectGoogle(): Promise<void> {
  if (inTauri) await invoke("google_disconnect");
}

/** `reconsent` asks for every permission again, e.g. to turn on plan usage after it was declined. */
export async function connectChatGpt(reconsent = false): Promise<void> {
  if (inTauri) await invoke("chatgpt_connect", { reconsent });
}

export async function disconnectChatGpt(): Promise<void> {
  if (inTauri) await invoke("chatgpt_disconnect");
}

export async function chatGptModels(): Promise<ModelInfo[]> {
  return inTauri ? invoke<ModelInfo[]>("chatgpt_models") : [{ id: "demo-model", label: "Demo model" }];
}

/** Sends the conversation; the answer streams back on `pill:chatgpt` under `id`. */
export function askChatGpt(id: string, turns: ChatTurn[]) {
  // Screenshots go by id; Rust has the image. The thumbnails stay here.
  const sent = turns.map(({ role, text, screen }) => ({ role, text, screen }));
  if (inTauri) invoke("chatgpt_ask", { id, turns: sent }).catch(console.error);
}

/** Captures the notch's screen (without the notch) for the next question. Rejects with a message. */
export async function captureScreen(): Promise<Screenshot> {
  if (inTauri) return invoke<Screenshot>("capture_screen");
  throw new Error("Screen capture needs the app.");
}

export function stopChatGpt(id: string) {
  if (inTauri) invoke("chatgpt_stop", { id }).catch(console.error);
}

/** Puts text on the clipboard (through Rust: the notch isn't focused, so the browser API refuses). */
export async function copyText(text: string): Promise<void> {
  if (inTauri) await invoke("copy_text", { text });
  else await navigator.clipboard.writeText(text);
}

/** Opens an https link (a calendar event, a video call) in the default browser. */
export function openLink(url: string) {
  if (inTauri) invoke("open_link", { url }).catch(console.error);
  else window.open(url, "_blank", "noopener");
}

/** ChatGPT's own settings page, where plan usage is shown. */
export function openChatGptUsage() {
  if (inTauri) invoke("chatgpt_open_usage").catch(console.error);
}

// ---------- settings ----------

export async function getSettings(): Promise<Settings> {
  return inTauri ? invoke<Settings>("get_settings") : structuredClone(DEFAULT_SETTINGS);
}

/** Resolves to what was stored after Rust clamped it. */
export async function saveSettings(settings: Settings): Promise<Settings> {
  return inTauri ? invoke<Settings>("set_settings", { settings }) : settings;
}

export async function listMonitors(): Promise<MonitorInfo[]> {
  return inTauri ? invoke<MonitorInfo[]>("list_monitors") : [{ name: "DISPLAY1", label: "Display 1 — 1920×1080 (primary)", primary: true }];
}

export async function recentNotificationApps(): Promise<string[]> {
  return inTauri ? invoke<string[]>("recent_notification_apps") : ["Discord", "WhatsApp", "Gmail"];
}

/** Every app Windows lets send notifications, whether or not it has sent one yet (empty on Linux). */
export async function notifyingApps(): Promise<string[]> {
  return inTauri ? invoke<string[]>("notifying_apps") : ["Brave", "Discord", "Google Chrome", "Slack", "WhatsApp"];
}

export async function mediaPlayers(): Promise<PlayerInfo[]> {
  return inTauri ? invoke<PlayerInfo[]>("media_players") : [{ id: "Spotify.exe", label: "Spotify" }, { id: "chrome", label: "Chrome" }];
}

/** Whether Claude Code's permission requests come to the notch (its hook is installed). */
export async function claudeHookInstalled(): Promise<boolean> {
  return inTauri ? invoke<boolean>("claude_hook_installed") : false;
}

/** Installs or removes the hook; resolves to whether it's installed. Rejects with a readable message. */
export async function setClaudeHook(installed: boolean): Promise<boolean> {
  return inTauri ? invoke<boolean>("set_claude_hook", { installed }) : installed;
}

export async function getAutostart(): Promise<boolean> {
  return inTauri ? invoke<boolean>("get_autostart") : false;
}

/** Resolves to the resulting state; rejects with a readable message (e.g. in debug builds). */
export async function setAutostart(enabled: boolean): Promise<boolean> {
  return inTauri ? invoke<boolean>("set_autostart", { enabled }) : enabled;
}
