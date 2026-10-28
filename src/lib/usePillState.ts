import { useEffect, useReducer, useRef } from "react";
import { calendarEvents, currentMedia, inTauri, notchTyping, on, pillReady } from "./bridge";
import type {
  AccessState,
  CalendarEvent,
  Decision,
  MediaState,
  NotificationItem,
  PastDecision,
  PermissionRequest,
  PrivacyState,
  ReceivedNotification,
  SystemCard,
  SystemCardKind,
  SystemState,
  Tab,
  UpdateOffer,
  View,
} from "./types";

const HOVER_LEAVE_MS = 180;
/** A burst (a busy group chat) shouldn't take minutes to play through: keep the newest few cards. */
const MAX_QUEUED_NOTIFICATIONS = 4;
const MAX_HISTORY = 20;
const MAX_DECISIONS = 8;
const TAB_KEY = "hoverpill.notch.tab";

/** How long a system card stays up: level bars are a glance, a low battery is worth more. */
const SYSTEM_CARD_MS: Record<SystemCardKind, number> = {
  volume: 2000,
  brightness: 2000,
  charging: 2500,
  "low-battery": 6000,
};

const NO_ACCESS: AccessState = { entries: [], active: false };
const NO_PRIVACY: PrivacyState = { camera: NO_ACCESS, microphone: NO_ACCESS };

type State = {
  /** The notch is open (pushed to the top edge, or rested on). */
  open: boolean;
  /** The pointer is over the closed notch. */
  peek: boolean;
  /** A fullscreen app covers the notch's monitor. */
  fullscreen: boolean;
  tab: Tab;
  media: MediaState | null;
  /** Cards still waiting to drop down. */
  queue: NotificationItem[];
  /** Everything received this session, newest first, for the Notifications tab. */
  history: ReceivedNotification[];
  /** Received since the Notifications tab was last looked at. */
  unseen: number;
  permissions: PermissionRequest[];
  decisions: PastDecision[];
  /** Upcoming Google Calendar events, soonest first. */
  calendar: CalendarEvent[];
  /** When a shortcut opened it (ms), until it closes again; 0 otherwise. */
  asked: number;
  /** Locked open with the lock button. */
  pinned: boolean;
  /** A newer version, offered in a card until it's installed or put off. */
  update: UpdateOffer | null;
  /** The pointer is on the notch itself rather than pushing against the top edge. */
  onCard: boolean;
  /** This opening came from reaching for the update card, so the card stays (see `pickView`). */
  cardHeld: boolean;
  /** Camera and microphone use right now. */
  privacy: PrivacyState;
  /** The latest volume, brightness and battery reading (null until the first arrives). */
  system: SystemState | null;
  /** A short card for what just changed, until it's dismissed. */
  systemCard: SystemCard | null;
};

type Action =
  | { type: "open"; value: boolean }
  | { type: "peek"; value: boolean }
  | { type: "onCard"; value: boolean }
  | { type: "fullscreen"; value: boolean }
  | { type: "tab"; value: Tab }
  | { type: "media"; value: MediaState | null }
  | { type: "notify"; value: NotificationItem }
  | { type: "dismissNotification"; id: string }
  | { type: "clearHistory" }
  | { type: "permission"; value: PermissionRequest }
  | { type: "decide"; id: string; decision: Decision }
  | { type: "cancelPermission"; id: string }
  | { type: "calendar"; value: CalendarEvent[] }
  | { type: "ask" }
  | { type: "summon" }
  | { type: "pin"; value: boolean }
  | { type: "update"; value: UpdateOffer | null }
  | { type: "privacy"; value: PrivacyState }
  | { type: "system"; value: SystemState }
  | { type: "dismissSystem"; at: number };

function withTab(state: State, tab: Tab): State {
  return { ...state, tab, unseen: tab === "notifications" ? 0 : state.unseen };
}

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "onCard":
      return { ...state, onCard: action.value };
    case "open":
      if (!action.value) return { ...state, open: false, asked: 0, cardHeld: false };
      // Decided once, as it opens: the pointer came onto the update card to use its buttons. A push
      // against the top edge, a shortcut or the lock opens the notch as usual instead.
      if (!state.open && state.update && state.onCard && !state.asked) return { ...state, open: true, cardHeld: true };
      // Opening while cards are queued means "show me": jump to the list and drop the queue.
      // Not when the ChatGPT shortcut is what opened it.
      if (state.queue.length && !state.asked) return { ...withTab(state, "notifications"), open: true, queue: [] };
      return { ...withTab(state, state.tab), open: true };
    case "peek":
      return { ...state, peek: action.value };
    case "fullscreen":
      return { ...state, fullscreen: action.value };
    case "tab":
      return withTab(state, action.value);
    case "media":
      return { ...state, media: action.value };
    case "notify": {
      const received = { ...action.value, receivedAt: Date.now() };
      const showingList = state.open && state.tab === "notifications";
      return {
        ...state,
        queue: state.open ? state.queue : [...state.queue, action.value].slice(-MAX_QUEUED_NOTIFICATIONS),
        history: [received, ...state.history].slice(0, MAX_HISTORY),
        unseen: showingList ? 0 : state.unseen + 1,
      };
    }
    case "dismissNotification":
      return { ...state, queue: state.queue.filter((n) => n.id !== action.id) };
    case "clearHistory":
      return { ...state, history: [], unseen: 0 };
    case "permission":
      return { ...state, permissions: [...state.permissions, action.value] };
    case "decide": {
      const request = state.permissions.find((p) => p.id === action.id);
      if (!request) return state;
      return {
        ...state,
        permissions: state.permissions.filter((p) => p.id !== action.id),
        decisions: [{ request, decision: action.decision, at: Date.now() }, ...state.decisions].slice(0, MAX_DECISIONS),
      };
    }
    case "cancelPermission":
      return { ...state, permissions: state.permissions.filter((p) => p.id !== action.id) };
    case "calendar":
      return { ...state, calendar: action.value };
    case "ask":
      // The shortcut toggles: a second press puts it away. Only a notch the shortcut actually
      // opened counts, though: Rust grabs the keyboard before the event arrives, and the grab
      // can open the notch a tick earlier (a `pill:hover`), which must not look like a second
      // press and close it again. It also keeps this working when the notch happens to be
      // sitting on the ChatGPT tab already.
      if (state.asked && state.open && state.tab === "chatgpt") return { ...state, open: false, asked: 0 };
      return { ...withTab(state, "chatgpt"), open: true, queue: [], asked: Date.now(), cardHeld: false };
    case "summon":
      // Same toggle, but it keeps whichever section was open.
      if (state.asked && state.open) return { ...state, open: false, asked: 0 };
      return { ...reducer({ ...state, onCard: false }, { type: "open", value: true }), onCard: state.onCard, asked: Date.now() };
    case "pin":
      return { ...state, pinned: action.value };
    case "update":
      return { ...state, update: action.value };
    case "privacy":
      return { ...state, privacy: action.value };
    case "system": {
      const card = changedCard(state.system, action.value);
      // A newer change replaces whatever card was up; an unchanged reading leaves it alone.
      return { ...state, system: action.value, systemCard: card ?? state.systemCard };
    }
    case "dismissSystem":
      return state.systemCard?.at === action.at ? { ...state, systemCard: null } : state;
  }
}

/**
 * What just changed, for a short card. Plugging in wins over a level bar, and a low battery is
 * only announced once, when it crosses below 20% while unplugged. The first reading after
 * starting isn't a change, so nothing drops down for what was already true.
 */
function changedCard(before: SystemState | null, after: SystemState): SystemCard | null {
  if (!before) return null;
  const at = Date.now();
  const battery = after.battery;
  if (battery.supported && !before.battery.charging && battery.charging) {
    return { kind: "charging", level: battery.level, muted: false, at };
  }
  const low = (s: SystemState) => s.battery.supported && !s.battery.charging && s.battery.level < 20;
  if (!low(before) && low(after)) {
    return { kind: "low-battery", level: battery.level, muted: false, at };
  }
  if (before.volume.level !== after.volume.level || before.volume.muted !== after.volume.muted) {
    return { kind: "volume", level: after.volume.level, muted: after.volume.muted, at };
  }
  if (after.brightness.supported && before.brightness.level !== after.brightness.level) {
    return { kind: "brightness", level: after.brightness.level, muted: false, at };
  }
  return null;
}

function savedTab(): Tab {
  try {
    const t = localStorage.getItem(TAB_KEY);
    if (t === "home" || t === "media" || t === "timer" || t === "notifications" || t === "claude" || t === "chatgpt") return t;
  } catch {
    // Storage can be unavailable; Home is fine.
  }
  return "home";
}

const initial = (): State => ({
  open: false,
  peek: false,
  fullscreen: false,
  tab: savedTab(),
  media: null,
  queue: [],
  history: [],
  unseen: 0,
  permissions: [],
  decisions: [],
  calendar: [],
  asked: 0,
  pinned: false,
  update: null,
  onCard: false,
  cardHeld: false,
  privacy: NO_PRIVACY,
  system: null,
  systemCard: null,
});

/**
 * Priority: things waiting on you (a Claude request, then a finished timer or an update), then a
 * short system card while closed, then the open notch, then a notification card, then what's
 * playing, then a running timer or stopwatch.
 */
export function pickView(
  s: State,
  timers: { ringing: boolean; ticking: boolean },
  systemCard: SystemCard | null = s.systemCard,
): View {
  if (s.permissions.length) return "permission";
  if (timers.ringing) return "alarm";
  // Like the alarm card it has buttons, so it stays while you reach for Restart/Later (the open
  // notch would otherwise replace it). Opened any other way, the notch opens as usual and the
  // card comes back when it closes.
  if (s.update && (!s.open || s.cardHeld)) return "update";
  // Closed only: when the notch is open the same change shows inline in the top bar instead.
  if (!s.open && systemCard) return "system";
  if (s.open) return "expanded";
  if (s.queue.length) return "notification";
  if (s.media) return "media";
  if (timers.ticking) return "timer";
  return "idle";
}

export function usePillState(notificationMs: number) {
  const [state, dispatch] = useReducer(reducer, undefined, initial);
  const leaveTimer = useRef<number | undefined>(undefined);

  // Open/close arrives from Rust: while the window is click-through the DOM sees no mouse
  // events at all. Closing is debounced so the spring doesn't stutter when the cursor grazes
  // the edge.
  const setOpen = (value: boolean) => {
    window.clearTimeout(leaveTimer.current);
    if (value) dispatch({ type: "open", value });
    else leaveTimer.current = window.setTimeout(() => dispatch({ type: "open", value }), HOVER_LEAVE_MS);
  };

  useEffect(() => {
    const subs = [
      on("pill:hover", setOpen),
      on("pill:ask", () => dispatch({ type: "ask" })),
      on("pill:summon", () => dispatch({ type: "summon" })),
      on("pill:update", (value) => dispatch({ type: "update", value })),
      on("pill:peek", (value) => dispatch({ type: "peek", value })),
      on("pill:on-card", (value) => dispatch({ type: "onCard", value })),
      on("pill:fullscreen", (value) => dispatch({ type: "fullscreen", value })),
      on("pill:media", (value) => dispatch({ type: "media", value })),
      on("pill:privacy", (value) => dispatch({ type: "privacy", value })),
      on("pill:system", (value) => dispatch({ type: "system", value })),
      on("pill:notification", (value) => dispatch({ type: "notify", value })),
      on("pill:permission", (value) => dispatch({ type: "permission", value })),
      on("pill:permission-cancel", ({ id }) => dispatch({ type: "cancelPermission", id })),
      on("pill:calendar", (value) => dispatch({ type: "calendar", value })),
    ];
    Promise.all(subs.map((s) => s.registered)).then(pillReady);
    // Anything that started playing before the UI loaded was emitted before we listened.
    currentMedia()
      .then((value) => value && dispatch({ type: "media", value }))
      .catch(console.error);
    calendarEvents()
      .then((value) => dispatch({ type: "calendar", value }))
      .catch(console.error);
    return () => subs.forEach((s) => s.off());
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem(TAB_KEY, state.tab);
    } catch {
      // Not remembering the tab is harmless.
    }
  }, [state.tab]);

  // A shortcut opens the notch by grabbing the keyboard in Rust, so it stays up with the
  // pointer elsewhere. Closing it again with the same shortcut has to release that, or the
  // hit-test loop keeps it open forever.
  useEffect(() => {
    if (!state.open) notchTyping(false);
  }, [state.open]);

  // Each card drops down for `notificationMs`, then the next one (or the resting state) follows.
  const head = state.queue[0];
  useEffect(() => {
    if (!head) return;
    const t = window.setTimeout(() => dispatch({ type: "dismissNotification", id: head.id }), notificationMs);
    return () => window.clearTimeout(t);
  }, [head?.id, notificationMs]);

  // A system card is a glance: take it away again, unless a newer one replaced it meanwhile.
  const systemCard = state.systemCard;
  useEffect(() => {
    if (!systemCard) return;
    const t = window.setTimeout(() => dispatch({ type: "dismissSystem", at: systemCard.at }), SYSTEM_CARD_MS[systemCard.kind]);
    return () => window.clearTimeout(t);
  }, [systemCard?.at, systemCard?.kind]);

  // Browser-only demo keys, so the UI can be iterated on with `npm run dev`.
  useEffect(() => {
    if (inTauri) return;
    const onKey = (e: KeyboardEvent) => {
      const id = crypto.randomUUID();
      if (e.key === "1")
        dispatch({ type: "permission", value: { id, tool: "Bash", command: "npm install --save-dev vitest", cwd: "D:/projects/HoverPill" } });
      if (e.key === "2") dispatch({ type: "notify", value: { id, app: "Discord", title: "#general", body: "anyone up for a match tonight?" } });
      if (e.key === "3")
        dispatch({ type: "media", value: state.media ? null : { title: "Midnight City", artist: "M83", playing: true, app: "Spotify" } });
      if (e.key === "4")
        dispatch({
          type: "privacy",
          value: state.privacy.camera.active
            ? NO_PRIVACY
            : {
                camera: { active: true, entries: [{ app: "Google Chrome", id: "chrome.exe", startedAt: Date.now() - 120_000 }] },
                microphone: { active: true, entries: [{ app: "Zoom", id: "zoom.exe", startedAt: Date.now() - 60_000 }] },
              },
        });
      if (e.key === "5") {
        const seed = state.system ?? {
          volume: { level: 45, muted: false },
          brightness: { level: 80, supported: true },
          battery: { level: 64, charging: false, supported: true },
        };
        // Seed first when there's no reading yet: the first reading never shows a card.
        if (!state.system) dispatch({ type: "system", value: seed });
        dispatch({ type: "system", value: { ...seed, volume: { level: (seed.volume.level + 15) % 100, muted: false } } });
      }
      if (e.key === "o") dispatch({ type: "open", value: !state.open });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [state.media, state.open, state.privacy, state.system]);

  return { state, dispatch, setOpen };
}
