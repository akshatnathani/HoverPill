// Shapes shared with the Rust side (src-tauri/src/events.rs). Keep in sync.

export type Rect = { x: number; y: number; width: number; height: number };

/** Never the bottom: that's where the taskbar is. */
export type Edge = "top" | "left" | "right";

/**
 * How to lay the pill out in its window (see `Layout` in notch.rs): as the notch at the top
 * centre, or docked flat against an edge, meeting it around `x`, `y` (CSS px from the window's
 * top-left).
 */
export type Layout = { mode: "notch" } | { mode: "docked"; edge: Edge; x: number; y: number };

/** `shift`: after a drop, how far the window jumped (old origin minus new, CSS px). */
export type DockEvent = { layout: Layout; shift: [number, number] | null };

export type PermissionRule = {
  toolName: string;
  ruleContent?: string;
};

export type PermissionSuggestion = {
  type: string;
  rules?: PermissionRule[];
  behavior?: "allow" | "deny" | "ask";
  destination?: string;
  mode?: string;
  directories?: string[];
};

export type PermissionRequest = {
  id: string;
  tool: string;
  /** Human-readable summary of what Claude wants to do (shell command, file path, ...). */
  command: string;
  cwd?: string;
  permission_suggestions?: PermissionSuggestion[];
};

/** "ask" hands the question back to Claude Code's own terminal prompt. */
export type Decision = "allow" | "deny" | "ask" | "always_allow";

export type NotificationItem = {
  id: string;
  app: string;
  title: string;
  body: string;
  /** data: URL or http(s) URL; falls back to the app's initial. */
  icon?: string;
};

export type MediaState = {
  title: string;
  artist: string;
  playing: boolean;
  artwork?: string;
  /** Readable player name, e.g. "Spotify". */
  app?: string;
};

export type MediaAction = "play_pause" | "next" | "previous";

/** `timer`: closed, nothing playing, timer or stopwatch running. `alarm`: the timer just ended. */
export type View = "idle" | "media" | "timer" | "expanded" | "notification" | "permission" | "alarm" | "update" | "system";

/** An app using the camera or microphone (src-tauri/src/access.rs). */
export type AccessEntry = {
  /** App name to show, e.g. "Google Chrome". */
  app: string;
  /** The package family name or executable path Windows knows it by. */
  id: string;
  /** When it started, Unix ms. */
  startedAt: number;
};

export type AccessState = {
  /** Every app using it right now, oldest first. */
  entries: AccessEntry[];
  /** Any app at all: what the dot follows. */
  active: boolean;
};

/** Camera and microphone use, pushed as `pill:privacy` when either changes. */
export type PrivacyState = { camera: AccessState; microphone: AccessState };

export type VolumeState = { level: number; muted: boolean };
export type BrightnessState = { level: number; supported: boolean };
export type BatteryState = { level: number; charging: boolean; supported: boolean };

/** Volume, brightness and battery, pushed as `pill:system` when one changes. */
export type SystemState = { volume: VolumeState; brightness: BrightnessState; battery: BatteryState };

export type SystemCardKind = "volume" | "brightness" | "charging" | "low-battery";

/** A short card for a volume/brightness change, the charger going in, or a low battery. */
export type SystemCard = {
  kind: SystemCardKind;
  /** 0–100: the new volume/brightness, or the battery level. */
  level: number;
  muted: boolean;
  /** When it appeared, Unix ms; also identifies the card when it's dismissed. */
  at: number;
};

/**
 * A newer HoverPill: downloaded and ready to install with a restart (`restart`), or, where this
 * copy can't replace itself (a .deb or .rpm install), to download by hand (`download`).
 */
export type UpdateOffer = { version: string; mode: "restart" | "download" };

/** Tabs inside the open notch. */
export type Tab = "home" | "media" | "timer" | "notifications" | "claude" | "chatgpt";

/** An upcoming Google Calendar event (src-tauri/src/accounts/google.rs). */
export type CalendarEvent = {
  id: string;
  title: string;
  /** RFC 3339 date-time, or a plain date for all-day events. */
  start: string;
  end: string;
  allDay: boolean;
  location?: string;
  /** A video call link if there is one, else the event's page in Google Calendar. */
  link?: string;
  calendar: string;
  color?: string;
};

export type AccountStatus = {
  connected: boolean;
  email?: string;
  /** Waiting for the user in the browser. */
  busy: boolean;
  error?: string;
  /** ChatGPT only: requests can use the user's ChatGPT plan. */
  planUsage: boolean;
};

export type AccountsStatus = { google: AccountStatus; chatgpt: AccountStatus };

/** A streamed answer from ChatGPT, matched to its question by `id`. */
export type ChatChunk =
  | { kind: "delta"; id: string; text: string }
  | { kind: "done"; id: string }
  | { kind: "error"; id: string; message: string; usage: boolean };

/** `screen`: a screenshot sent with the question (its id in Rust); `preview`: its thumbnail. */
export type ChatTurn = { role: "user" | "assistant"; text: string; screen?: string; preview?: string };

/** A captured screen, kept in Rust until it's sent: its id and a thumbnail `data:` URL. */
export type Screenshot = { id: string; preview: string };

export type ModelInfo = { id: string; label: string };

export type ReceivedNotification = NotificationItem & { receivedAt: number };

export type PastDecision = { request: PermissionRequest; decision: Decision; at: number };

// Mirrors src-tauri/src/settings.rs. Rust clamps everything on save; these ranges are for the UI.
export type Settings = {
  display: {
    /** Monitor name from `list_monitors`; null follows the primary monitor. */
    monitor: string | null;
    topOffset: number;
    scale: number;
    /** Also open when the pointer rests on the notch (not just when pushed to the top edge). */
    openOnHover: boolean;
    /** With nothing playing and no card showing, shrink to a thin bar. */
    compactIdle: boolean;
    /** Hide while a fullscreen app covers the notch's monitor. */
    hideInFullscreen: boolean;
    /** Install and enable the GNOME Shell helper for native Wayland fullscreen apps (GNOME only). */
    gnomeHelper: boolean;
    /** Reserve a strip at the top so maximised windows start below the notch (Windows only). */
    reserveSpace: boolean;
    /** Show the pill on every virtual desktop / workspace, not just the one it was opened on. */
    allWorkspaces: boolean;
    /** Keys that open the notch from any app, like "Super+Alt+Space"; empty for none. */
    shortcut: string;
  };
  notifications: {
    enabled: boolean;
    durationSecs: number;
    blockedApps: string[];
  };
  media: {
    enabled: boolean;
    ignoredPlayers: string[];
  };
  calendar: {
    /** Show events on Home and drop reminders while Google Calendar is connected. */
    enabled: boolean;
    /** Minutes before an event to drop a reminder; 0 turns reminders off. */
    reminderMinutes: number;
  };
  updates: {
    /** Look for a new version now and then, and get it ready to install. */
    autoCheck: boolean;
  };
  privacy: {
    /** A green dot while any app uses the camera. */
    cameraIndicator: boolean;
    /** An orange dot while any app uses the microphone. */
    microphoneIndicator: boolean;
  };
  system: {
    /** A level bar (and mute state) when the volume changes. */
    showVolume: boolean;
    /** A level bar when the screen brightness changes (laptops). */
    showBrightness: boolean;
    /** A card when the charger goes in, and a warning when the battery runs low. */
    showBattery: boolean;
  };
  chatgpt: {
    /** Model slug; null uses the first one the account offers. */
    model: string | null;
    /** Keys that open the ChatGPT tab from any app, like "Ctrl+Alt+G"; empty for none. */
    shortcut: string;
  };
};

/** Mirrors the platform-aware default in src-tauri/src/settings.rs (`Display::default`). */
export const IS_LINUX = typeof navigator !== "undefined" && navigator.userAgent.includes("Linux");

export const DEFAULT_SETTINGS: Settings = {
  display: {
    monitor: null,
    topOffset: 0,
    scale: 1,
    openOnHover: false,
    compactIdle: true,
    hideInFullscreen: true,
    gnomeHelper: false,
    reserveSpace: false,
    allWorkspaces: IS_LINUX,
    shortcut: "Super+Alt+Space",
  },
  notifications: { enabled: true, durationSecs: 4.5, blockedApps: [] },
  media: { enabled: true, ignoredPlayers: [] },
  calendar: { enabled: true, reminderMinutes: 5 },
  chatgpt: { model: null, shortcut: "Ctrl+Alt+G" },
  updates: { autoCheck: true },
  privacy: { cameraIndicator: true, microphoneIndicator: true },
  system: { showVolume: true, showBrightness: true, showBattery: true },
};

export const RANGES = {
  topOffset: { min: 0, max: 48, step: 1 },
  scale: { min: 0.8, max: 1.4, step: 0.05 },
  durationSecs: { min: 2, max: 15, step: 0.5 },
  reminderMinutes: { min: 0, max: 60, step: 1 },
} as const;

export type MonitorInfo = { name: string; label: string; primary: boolean };
export type PlayerInfo = { id: string; label: string };
