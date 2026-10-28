import { AnimatePresence, motion } from "motion/react";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { captureScreen, connectChatGpt, notchTyping, on, openChatGptUsage, openLink } from "../lib/bridge";
import type { Chat } from "../lib/useAccounts";
import type {
  AccountStatus,
  CalendarEvent,
  Decision,
  MediaAction,
  MediaState,
  NotificationItem,
  PastDecision,
  PermissionRequest,
  ReceivedNotification,
  PrivacyState,
  Screenshot,
  SystemCard,
  SystemCardKind,
  Tab,
  UpdateOffer,
} from "../lib/types";
import { IS_LINUX } from "../lib/types";
import { Icon, type IconName } from "./icons";
import { CopyButton, Markdown } from "./markdown";
import { formatCountdown, formatElapsed, MAX_DURATION, stopwatchElapsed, timerLeft, type Timers } from "../lib/useTimers";

// ---------- closed ----------

/**
 * The camera/microphone dots. Green for the camera and orange for the microphone, like the
 * indicators on a phone; the open notch says which app has them.
 */
export function PrivacyDots({ privacy, className }: { privacy: PrivacyState; className?: string }) {
  const { camera, microphone } = privacy;
  if (!camera.active && !microphone.active) return null;
  const label = [camera.active && "Camera", microphone.active && "Microphone"].filter(Boolean).join(" and ");
  return (
    <span className={`privacy-dots${className ? ` ${className}` : ""}`} role="img" aria-label={`${label} in use`}>
      {camera.active && <span className="privacy-dot is-camera" />}
      {microphone.active && <span className="privacy-dot is-microphone" />}
    </span>
  );
}

export function IdleView({ privacy }: { privacy: PrivacyState }) {
  return (
    <div className="idle">
      <PrivacyDots privacy={privacy} />
    </div>
  );
}

/**
 * Closed with something playing: the cover on the left, its colour bleeding into the black.
 * A running timer or stopwatch takes the visualizer's place on the right.
 */
export function MediaCompactView({ media, clock, privacy }: { media: MediaState; clock: string | null; privacy: PrivacyState }) {
  return (
    <div className="compact">
      <Artwork media={media} className="compact-art" />
      <span className="compact-end">
        <PrivacyDots privacy={privacy} />
        {clock ? <span className="compact-clock">{clock}</span> : <Visualizer playing={media.playing} />}
      </span>
    </div>
  );
}

/**
 * Closed as a bubble against a screen edge: a running timer or stopwatch, else the cover of
 * what's playing, else a small pill shape.
 */
export function BubbleView({ media, clock }: { media: MediaState | null; clock: string | null }) {
  return (
    <div className="bubble">
      {clock ? (
        <span className={`bubble-clock${clock.length > 5 ? " is-long" : ""}`}>{clock}</span>
      ) : media ? (
        <Artwork media={media} className="bubble-art" />
      ) : (
        <span className="bubble-mark" />
      )}
    </div>
  );
}

/** Closed, nothing playing, a timer or stopwatch running. */
export function TimerCompactView({ clock, privacy }: { clock: string; privacy: PrivacyState }) {
  return (
    <div className="compact">
      <Icon name="timer" className="compact-timer-icon" />
      <span className="compact-end">
        <PrivacyDots privacy={privacy} />
        <span className="compact-clock">{clock}</span>
      </span>
    </div>
  );
}

/** The timer ran out. Stays until you deal with it. */
export function AlarmView({ durationMs, onSnooze, onDone }: { durationMs: number; onSnooze: () => void; onDone: () => void }) {
  return (
    <div className="card-alarm">
      <span className="alarm-icon" aria-hidden>
        <Icon name="timer" />
      </span>
      <div className="card-text">
        <div className="card-title">Time's up</div>
        <div className="card-body">{describeDuration(durationMs)} timer</div>
      </div>
      <button type="button" className="btn btn-quiet alarm-button" onClick={onSnooze}>
        +1 min
      </button>
      <button type="button" className="btn btn-solid alarm-button" onClick={onDone}>
        Done
      </button>
    </div>
  );
}

/**
 * A newer HoverPill is ready. Restart installs it; on a .deb/.rpm install, which can't replace
 * itself, Download opens the release page instead.
 */
export function UpdateView({ offer, onInstall, onLater }: { offer: UpdateOffer; onInstall: () => void; onLater: () => void }) {
  return (
    <div className="card-alarm">
      <span className="alarm-icon" aria-hidden>
        <Icon name="download" />
      </span>
      <div className="card-text">
        <div className="card-title">HoverPill {offer.version} is ready</div>
        <div className="card-body">{offer.mode === "restart" ? "Restart to update." : "Download the new version."}</div>
      </div>
      <button type="button" className="btn btn-quiet alarm-button" onClick={onLater}>
        Later
      </button>
      <button type="button" className="btn btn-solid alarm-button" onClick={onInstall}>
        {offer.mode === "restart" ? "Restart" : "Download"}
      </button>
    </div>
  );
}

// ---------- volume, brightness, battery ----------

const SYSTEM_ICON: Record<SystemCardKind, IconName> = {
  volume: "volume",
  brightness: "sun",
  charging: "battery",
  "low-battery": "batteryLow",
};

/**
 * A volume or brightness level bar, the charger going in, or the battery running low. It takes
 * the place of the system's own pop-up in the corner of the screen.
 */
export function SystemCardView({ card }: { card: SystemCard }) {
  const icon = card.kind === "volume" && card.muted ? "volumeMuted" : SYSTEM_ICON[card.kind];
  const title =
    card.kind === "charging"
      ? "Charging"
      : card.kind === "low-battery"
        ? "Battery low"
        : card.kind === "brightness"
          ? "Brightness"
          : card.muted
            ? "Muted"
            : "Volume";
  const value = card.kind === "low-battery" ? `${card.level}% left` : `${card.level}%`;
  return (
    <div className={`card-system is-${card.kind}${card.muted ? " is-muted" : ""}`} role="status">
      <span className="system-icon" aria-hidden>
        <Icon name={icon} />
      </span>
      <div className="system-body">
        <div className="system-line">
          <span className="system-title">{title}</span>
          <span className="system-value">{value}</span>
        </div>
        <div className="system-bar" aria-hidden>
          <span style={{ width: `${card.muted ? 0 : card.level}%` }} />
        </div>
      </div>
    </div>
  );
}

// ---------- cards that drop down by themselves ----------

export function NotificationView({ item, durationMs }: { item: NotificationItem; durationMs: number }) {
  return (
    <div className="card-notification">
      <AppIcon app={item.app} icon={item.icon} className="card-icon" />
      <div className="card-text">
        <div className="card-app">{item.app}</div>
        <div className="card-title">{item.title}</div>
        {item.body && <div className="card-body">{item.body}</div>}
      </div>
      <motion.div
        className="card-timer"
        initial={{ scaleX: 1 }}
        animate={{ scaleX: 0 }}
        transition={{ duration: durationMs / 1000, ease: "linear" }}
      />
    </div>
  );
}

export function PermissionView({ request, queued, onDecide }: { request: PermissionRequest; queued: number; onDecide: (d: Decision) => void }) {
  // "Always allow" only makes sense when Claude offered a rule to remember.
  const canAlwaysAllow = request.permission_suggestions?.some((s) => s.behavior === "allow") ?? false;
  return (
    <div className="card-permission">
      <div className="perm-header">
        <Icon name="spark" className="perm-mark" />
        <span>Claude Code wants to use</span>
        <span className="tool-chip">{request.tool}</span>
        {queued > 0 && <span className="perm-queued">{queued} more waiting</span>}
      </div>
      <pre className="perm-command">{request.command}</pre>
      <div className="perm-footer">
        <span className="perm-cwd">{request.cwd}</span>
        <button type="button" className="text-button" onClick={() => onDecide("ask")}>
          <Icon name="terminal" />
          Answer in terminal
        </button>
      </div>
      <div className="perm-actions">
        {canAlwaysAllow && (
          <button type="button" className="btn btn-quiet" onClick={() => onDecide("always_allow")}>
            Always allow
          </button>
        )}
        <button type="button" className="btn btn-quiet" onClick={() => onDecide("deny")}>
          Deny
        </button>
        <button type="button" className="btn btn-solid" onClick={() => onDecide("allow")}>
          Approve
        </button>
      </div>
    </div>
  );
}

// ---------- open: tab bar + tabs ----------

const TABS: { id: Tab; label: string; icon: IconName }[] = [
  { id: "home", label: "Home", icon: "home" },
  { id: "media", label: "Media", icon: "music" },
  { id: "timer", label: "Timer", icon: "timer" },
  { id: "notifications", label: "Notifications", icon: "bell" },
  { id: "claude", label: "Claude", icon: "spark" },
  { id: "chatgpt", label: "ChatGPT", icon: "chat" },
];

export type ExpandedProps = {
  tab: Tab;
  onTab: (t: Tab) => void;
  media: MediaState | null;
  onMedia: (a: MediaAction) => void;
  history: ReceivedNotification[];
  unseen: number;
  onClearHistory: () => void;
  decisions: PastDecision[];
  timers: Timers;
  onSettings: () => void;
  calendar: CalendarEvent[];
  chat: Chat;
  chatgpt: AccountStatus;
  /** When the ChatGPT shortcut opened the notch (ms), or 0. */
  asked: number;
  /** Locked open; the lock button shows on the ChatGPT tab, and on every tab while locked. */
  pinned: boolean;
  onPin: (pinned: boolean) => void;
  /** Who's using the camera or microphone right now. */
  privacy: PrivacyState;
  /** A volume/brightness/charging card, shown inline in the top bar while the notch is open. */
  systemCard: SystemCard | null;
};

export function ExpandedView(props: ExpandedProps) {
  const now = useNow(15_000);
  return (
    <div className="expanded">
      <div className="topbar">
        <div className="tabs" role="tablist" aria-label="Notch sections">
          {TABS.map((t) => {
            const active = t.id === props.tab;
            const badge = t.id === "notifications" && props.unseen > 0 ? props.unseen : 0;
            const ticking = t.id === "timer" && (props.timers.timer.status === "running" || props.timers.stopwatch.startedAt !== null);
            return (
              <button
                key={t.id}
                type="button"
                role="tab"
                aria-selected={active}
                aria-label={t.label}
                className={`tab${active ? " is-active" : ""}`}
                onClick={() => props.onTab(t.id)}
              >
                <Icon name={t.icon} />
                {active && <span className="tab-label">{t.label}</span>}
                {badge > 0 && <span className="tab-badge">{badge > 9 ? "9+" : badge}</span>}
                {ticking && !active && <span className="tab-dot" aria-label="running" />}
              </button>
            );
          })}
        </div>
        <div className="topbar-end">
          <PrivacyDots privacy={props.privacy} className="topbar-dots" />
          {props.systemCard && <SystemMeter card={props.systemCard} />}
          <span className="clock">{now.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</span>
          {(props.tab === "chatgpt" || props.pinned) && (
            <button
              type="button"
              className={`round-button${props.pinned ? " is-on" : ""}`}
              aria-label={props.pinned ? "Unlock: close the notch as usual" : "Lock: keep the notch open"}
              aria-pressed={props.pinned}
              title={props.pinned ? "Unlock" : "Keep open"}
              onClick={() => props.onPin(!props.pinned)}
            >
              <Icon name={props.pinned ? "lock" : "unlock"} />
            </button>
          )}
          <button type="button" className="round-button" aria-label="Open settings" title="Settings" onClick={props.onSettings}>
            <Icon name="gear" />
          </button>
        </div>
      </div>

      <AnimatePresence mode="wait" initial={false}>
        <motion.div
          key={props.tab}
          className="tab-panel"
          role="tabpanel"
          initial={{ opacity: 0, y: 4 }}
          animate={{ opacity: 1, y: 0, transition: { duration: 0.16 } }}
          exit={{ opacity: 0, transition: { duration: 0.08 } }}
        >
          {props.tab === "home" && <HomeTab {...props} now={now} />}
          {props.tab === "media" && <MediaTab media={props.media} onMedia={props.onMedia} />}
          {props.tab === "timer" && <TimerTab timers={props.timers} />}
          {props.tab === "notifications" && <NotificationsTab history={props.history} onClear={props.onClearHistory} now={now} />}
          {props.tab === "claude" && <ClaudeTab decisions={props.decisions} now={now} />}
          {props.tab === "chatgpt" && <ChatGptTab chat={props.chat} account={props.chatgpt} onSettings={props.onSettings} asked={props.asked} />}
        </motion.div>
      </AnimatePresence>
    </div>
  );
}

function HomeTab({ media, onMedia, history, now, onTab, calendar, privacy }: ExpandedProps & { now: Date }) {
  const latest = history[0];
  const next = nextEvent(calendar, now);
  return (
    <div className="home">
      <div className="home-main">
        {media ? (
          <div className="home-player">
            <Artwork media={media} className="home-art" />
            <div className="home-meta">
              <div className="track-title">{media.title}</div>
              <div className="track-artist">{media.artist}</div>
              <Transport media={media} onMedia={onMedia} size="small" />
            </div>
          </div>
        ) : (
          <EmptyState icon="music" title="Nothing playing">
            Play something in Spotify, a browser or any media app.
          </EmptyState>
        )}
        {(privacy.camera.active || privacy.microphone.active) && <PrivacyRow privacy={privacy} />}
        {next ? (
          <UpNext event={next} now={now} />
        ) : latest && (
          <button type="button" className="home-latest" onClick={() => onTab("notifications")}>
            <AppIcon app={latest.app} icon={latest.icon} className="latest-icon" />
            <span className="latest-text">
              <span className="latest-title">{latest.title}</span>
              <span className="latest-body">{latest.body || latest.app}</span>
            </span>
            <span className="latest-time">{ago(latest.receivedAt, now)}</span>
          </button>
        )}
      </div>
      <Calendar today={now} events={calendar} />
    </div>
  );
}

// ---------- Google Calendar on Home ----------

/** All-day events come as a plain date; read it as a local date, not UTC midnight. */
function eventDate(value: string, allDay: boolean) {
  if (allDay) {
    const [y, m, d] = value.split("-").map(Number);
    return new Date(y, m - 1, d);
  }
  return new Date(value);
}

/** The first event that hasn't finished yet. */
function nextEvent(events: CalendarEvent[], now: Date) {
  return events.find((e) => eventDate(e.end, e.allDay) > now);
}

function dayLabel(d: Date, now: Date) {
  const days = Math.round((new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() - new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Tomorrow";
  return d.toLocaleDateString([], { weekday: "long" });
}

function whenText(e: CalendarEvent, now: Date) {
  const start = eventDate(e.start, e.allDay);
  if (e.allDay) return `${dayLabel(start, now)}, all day`;
  const end = eventDate(e.end, false);
  const time = (d: Date) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (start <= now) return `Now, until ${time(end)}`;
  const mins = Math.round((start.getTime() - now.getTime()) / 60_000);
  if (mins < 60) return `In ${mins} min, at ${time(start)}`;
  return `${dayLabel(start, now)}, ${time(start)}`;
}

const CALL_HOSTS = /(^|\.)(meet\.google\.com|zoom\.us|teams\.microsoft\.com|teams\.live\.com|webex\.com)$/;

function isCall(link: string) {
  try {
    return CALL_HOSTS.test(new URL(link).hostname);
  } catch {
    return false;
  }
}

function UpNext({ event, now }: { event: CalendarEvent; now: Date }) {
  const call = event.link && isCall(event.link);
  return (
    <div className="up-next">
      <span className="up-next-mark" style={event.color ? { background: event.color } : undefined} aria-hidden />
      <span className="up-next-text">
        <span className="up-next-title">{event.title}</span>
        <span className="up-next-when">
          {whenText(event, now)}
          {event.location ? `, ${event.location}` : ""}
        </span>
      </span>
      {event.link && (
        <button type="button" className={`btn ${call ? "btn-solid" : "btn-quiet"} up-next-button`} onClick={() => openLink(event.link!)}>
          {call && <Icon name="video" />}
          {call ? "Join" : "Open"}
        </button>
      )}
    </div>
  );
}

function MediaTab({ media, onMedia }: { media: MediaState | null; onMedia: (a: MediaAction) => void }) {
  if (!media)
    return (
      <EmptyState icon="music" title="Nothing playing">
        Play something in Spotify, a browser or any media app and it shows up here.
      </EmptyState>
    );
  return (
    <div className="player">
      <div className="player-art-wrap">
        <Artwork media={media} className="player-art" />
      </div>
      <div className="player-meta">
        {media.app && <div className="player-app">Playing in {media.app}</div>}
        <div className="player-title">{media.title}</div>
        <div className="player-artist">{media.artist}</div>
        <div className="player-row">
          <Transport media={media} onMedia={onMedia} size="large" />
          <Visualizer playing={media.playing} />
        </div>
      </div>
    </div>
  );
}

function NotificationsTab({ history, onClear, now }: { history: ReceivedNotification[]; onClear: () => void; now: Date }) {
  if (!history.length)
    return (
      <EmptyState icon="bell" title="No notifications yet">
        Notifications from your apps collect here as they arrive.
      </EmptyState>
    );
  return (
    <div className="list-wrap">
      <div className="list-head">
        <span>
          {history.length} {history.length === 1 ? "notification" : "notifications"}
        </span>
        <button type="button" className="text-button" onClick={onClear}>
          Clear all
        </button>
      </div>
      <ul className="list">
        {history.map((n) => (
          <li key={n.id} className="list-item">
            <AppIcon app={n.app} icon={n.icon} className="list-icon" />
            <div className="list-text">
              <div className="list-line">
                <span className="list-app">{n.app}</span>
                <span className="list-time">{ago(n.receivedAt, now)}</span>
              </div>
              <div className="list-title">{n.title}</div>
              {n.body && <div className="list-body">{n.body}</div>}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

const DECISION_TEXT: Record<Decision, string> = { allow: "Approved", deny: "Denied", ask: "Sent to terminal", always_allow: "Always allowed" };

function ClaudeTab({ decisions, now }: { decisions: PastDecision[]; now: Date }) {
  if (!decisions.length)
    return (
      <EmptyState icon="spark" title="No requests yet">
        When Claude Code asks for permission, the request drops down from the notch. To connect it, choose Install Claude Code hook in
        the tray menu.
      </EmptyState>
    );
  return (
    <div className="list-wrap">
      <div className="list-head">
        <span>Recent requests</span>
      </div>
      <ul className="list">
        {decisions.map((d) => (
          <li key={d.request.id + d.at} className="list-item decision">
            <span className={`decision-mark is-${d.decision}`} aria-hidden />
            <div className="list-text">
              <div className="list-line">
                <span className="tool-chip">{d.request.tool}</span>
                <span className={`decision-text is-${d.decision}`}>{DECISION_TEXT[d.decision]}</span>
                <span className="list-time">{ago(d.at, now)}</span>
              </div>
              <code className="decision-command">{d.request.command}</code>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ---------- ChatGPT tab ----------

function ChatGptTab({ chat, account, onSettings, asked }: { chat: Chat; account: AccountStatus; onSettings: () => void; asked: number }) {
  if (!account.connected)
    return (
      <EmptyState icon="chat" title="Ask ChatGPT from the notch">
        Sign in with your ChatGPT account and questions use your Plus or Pro plan, no API key needed.{" "}
        <button type="button" className="text-button inline" disabled={account.busy} onClick={() => connectChatGpt().catch(console.error)}>
          {account.busy ? "Waiting for the browser…" : "Sign in with ChatGPT"}
        </button>
      </EmptyState>
    );
  if (!account.planUsage)
    return (
      <EmptyState icon="chat" title="Plan usage is off">
        HoverPill can only answer when it may use your ChatGPT plan.{" "}
        <button type="button" className="text-button inline" disabled={account.busy} onClick={() => connectChatGpt(true).catch(console.error)}>
          Turn it on
        </button>
      </EmptyState>
    );
  return <ChatPanel chat={chat} onSettings={onSettings} asked={asked} />;
}

// Panels currently mounted, so a remount (React does so in development, and the account status
// arriving swaps the sign-in view for the panel) doesn't hand the keyboard back in between.
let openChatPanels = 0;

/**
 * X hands the notch its keyboard focus a moment after the shortcut, so the blur that comes first
 * isn't the user leaving. Wait for the focus to settle and only let go if it really went away.
 */
export function releaseTypingAfterBlur() {
  window.setTimeout(() => {
    if (!document.hasFocus()) notchTyping(false);
  }, 400);
}

function ChatPanel({ chat, onSettings, asked }: { chat: Chat; onSettings: () => void; asked: number }) {
  const [draft, setDraft] = useState("");
  // The screen captured for the next question, and a capture in progress or its error.
  const [shot, setShot] = useState<Screenshot | null>(null);
  const [capture, setCapture] = useState<"idle" | "busy" | string>("idle");
  const input = useRef<HTMLTextAreaElement>(null);
  const log = useRef<HTMLDivElement>(null);
  const { turns, pending, busy } = chat;

  // Keep the newest words in view as the answer streams in.
  useLayoutEffect(() => {
    log.current?.scrollTo({ top: log.current.scrollHeight });
  }, [turns.length, pending?.text, pending?.error]);

  // Leaving the tab (or the notch folding up) gives the keyboard back, once nothing has replaced
  // this panel in the same tick.
  useEffect(() => {
    openChatPanels += 1;
    return () => {
      openChatPanels -= 1;
      if (openChatPanels > 0) return;
      window.setTimeout(() => {
        if (openChatPanels === 0) notchTyping(false);
      }, 0);
    };
  }, []);

  // Opened by the ChatGPT shortcut just now: straight into the question box, keyboard and all.
  // The box's focus event claims the keyboard too, but it doesn't fire when the box is already
  // focused (this panel mounting again), so claim it here as well.
  useEffect(() => {
    if (!asked || Date.now() - asked >= 2000) return;
    input.current?.focus();
    notchTyping(true);
  }, [asked]);

  // Another app came to the front, so typing here is over (Windows; see `typing_over` in notch.rs).
  useEffect(() => on("pill:typing", (typing) => !typing && input.current?.blur()).off, []);

  const send = () => {
    if ((!draft.trim() && !shot) || busy) return;
    chat.ask(draft, shot ?? undefined);
    setDraft("");
    setShot(null);
  };

  const share = () => {
    setCapture("busy");
    captureScreen().then(
      (s) => {
        setShot(s);
        setCapture("idle");
        input.current?.focus();
      },
      (e) => setCapture(String(e)),
    );
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
    if (e.key === "Escape") {
      input.current?.blur();
      notchTyping(false);
    }
  };

  return (
    <div className="chat">
      <div className="chat-log" ref={log}>
        {turns.length === 0 && !pending && <p className="chat-hint">Ask anything. Answers use your ChatGPT plan and aren't saved.</p>}
        {turns.map((t, i) =>
          t.role === "user" ? (
            <div key={i} className="chat-turn is-user">
              <p>{t.text}</p>
              {t.preview && <img className="chat-turn-shot" src={t.preview} alt="Your screen, sent with this question" />}
            </div>
          ) : (
            <div key={i} className="chat-turn is-assistant">
              <Markdown text={t.text} />
              <CopyButton text={t.text} label="Copy answer" className="answer-copy" />
            </div>
          ),
        )}
        {pending && (
          <div className="chat-turn is-assistant">
            <Markdown text={pending.text} trailing={!pending.error && <span className="chat-caret" aria-hidden />} />
          </div>
        )}
        {pending?.error && (
          <p className="chat-error" role="alert">
            {pending.error}{" "}
            {pending.usage ? (
              <button type="button" className="text-button inline" onClick={openChatGptUsage}>
                See usage
              </button>
            ) : (
              <button type="button" className="text-button inline" onClick={onSettings}>
                Settings
              </button>
            )}
          </p>
        )}
      </div>
      {(shot || capture !== "idle") && (
        <div className="chat-shot">
          {shot ? (
            <>
              <img src={shot.preview} alt="" />
              <span>Your screen goes with your next question</span>
              <button type="button" className="round-button is-small" aria-label="Don't send the screen" title="Remove" onClick={() => setShot(null)}>
                <Icon name="close" />
              </button>
            </>
          ) : capture === "busy" ? (
            <span>Capturing your screen…</span>
          ) : (
            <span className="chat-shot-error" role="alert">
              {capture}{" "}
              <button type="button" className="text-button inline" onClick={() => setCapture("idle")}>
                OK
              </button>
            </span>
          )}
        </div>
      )}
      <div className="chat-input">
        <button
          type="button"
          className={`round-button${shot ? " is-on" : ""}`}
          aria-label="Share your screen with the next question"
          title="Share your screen"
          disabled={capture === "busy"}
          onClick={share}
        >
          <Icon name="screen" />
        </button>
        <textarea
          ref={input}
          rows={1}
          value={draft}
          placeholder="Ask ChatGPT"
          aria-label="Ask ChatGPT"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKey}
          onFocus={() => notchTyping(true)}
          // On Windows Rust ends typing when another app comes to the front: a blur here can be
          // focus moving within the notch (letting go of Alt, for one), which mustn't close it.
          onBlur={IS_LINUX ? releaseTypingAfterBlur : undefined}
        />
        {busy ? (
          <button type="button" className="round-button" aria-label="Stop the answer" title="Stop" onClick={chat.stop}>
            <Icon name="stop" />
          </button>
        ) : (
          <button type="button" className="round-button is-send" aria-label="Send" title="Send" disabled={!draft.trim() && !shot} onClick={send}>
            <Icon name="send" />
          </button>
        )}
        <button type="button" className="round-button" aria-label="Clear the conversation" title="Clear" disabled={!turns.length && !pending} onClick={chat.clear}>
          <Icon name="clear" />
        </button>
      </div>
    </div>
  );
}

// ---------- Timer tab ----------

const PRESETS_MIN = [1, 3, 5, 10, 15, 25, 45, 60];

function TimerTab({ timers }: { timers: Timers }) {
  // Opens on whichever one is in use; otherwise the countdown.
  const [mode, setMode] = useState<"timer" | "stopwatch">(() =>
    timers.stopwatch.startedAt !== null && timers.timer.status !== "running" ? "stopwatch" : "timer",
  );
  return (
    <div className="timer-tab">
      <div className="segmented" role="tablist" aria-label="Timer or stopwatch">
        {(["timer", "stopwatch"] as const).map((m) => (
          <button key={m} type="button" role="tab" aria-selected={mode === m} className={`segment${mode === m ? " is-active" : ""}`} onClick={() => setMode(m)}>
            {m === "timer" ? "Timer" : "Stopwatch"}
          </button>
        ))}
      </div>
      {mode === "timer" ? <CountdownPane timers={timers} /> : <StopwatchPane timers={timers} />}
    </div>
  );
}

function CountdownPane({ timers }: { timers: Timers }) {
  const { timer, now, timerActions: act } = timers;
  const left = timerLeft(timer, now);
  const idle = timer.status === "idle" || timer.status === "done";
  const progress = idle ? 0 : 1 - left / timer.durationMs;
  return (
    <div className="timer-pane">
      <div className="timer-main">
        <div className="big-clock" aria-live="off">
          {formatCountdown(left)}
        </div>
        <div className="timer-progress" aria-hidden>
          <span style={{ transform: `scaleX(${progress})` }} />
        </div>
        <div className="timer-caption">
          {timer.status === "running" && `${describeDuration(timer.durationMs)} timer, ends at ${new Date(timer.endsAt ?? now).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`}
          {timer.status === "paused" && `Paused, ${describeDuration(timer.durationMs)} timer`}
          {idle && "Pick a length, then start"}
        </div>
        <div className="timer-controls">
          {idle ? (
            <>
              <button type="button" className="round-button is-medium" aria-label="One minute less" onClick={() => act.setDuration(timer.durationMs - 60_000)} disabled={timer.durationMs <= 60_000}>
                <Icon name="minus" />
              </button>
              <button type="button" className="pill-button is-solid" onClick={act.start}>
                <Icon name="play" />
                Start
              </button>
              <button type="button" className="round-button is-medium" aria-label="One minute more" onClick={() => act.setDuration(timer.durationMs + 60_000)} disabled={timer.durationMs >= MAX_DURATION}>
                <Icon name="plus" />
              </button>
            </>
          ) : (
            <>
              <button type="button" className="pill-button" onClick={act.reset}>
                Reset
              </button>
              {timer.status === "running" ? (
                <button type="button" className="pill-button is-solid" onClick={act.pause}>
                  <Icon name="pause" />
                  Pause
                </button>
              ) : (
                <button type="button" className="pill-button is-solid" onClick={act.start}>
                  <Icon name="play" />
                  Resume
                </button>
              )}
            </>
          )}
        </div>
      </div>
      <div className="presets" role="group" aria-label="Timer length">
        {PRESETS_MIN.map((min) => (
          <button
            key={min}
            type="button"
            className={`preset${timer.durationMs === min * 60_000 ? " is-active" : ""}`}
            disabled={!idle}
            onClick={() => act.setDuration(min * 60_000)}
          >
            {min < 60 ? `${min} min` : "1 hour"}
          </button>
        ))}
      </div>
    </div>
  );
}

function StopwatchPane({ timers }: { timers: Timers }) {
  const { stopwatch, now, stopwatchActions: act } = timers;
  const elapsed = stopwatchElapsed(stopwatch, now);
  const running = stopwatch.startedAt !== null;
  const started = running || elapsed > 0;
  // Newest first, each with its own length and the running total.
  const laps = stopwatch.laps.map((at, i) => ({ n: i + 1, at, split: at - (stopwatch.laps[i - 1] ?? 0) })).reverse();
  return (
    <div className="timer-pane">
      <div className="timer-main">
        <div className="big-clock">{formatElapsed(elapsed, true)}</div>
        <div className="timer-caption">{running ? "Running" : started ? "Paused" : "Start, then add laps as you go"}</div>
        <div className="timer-controls">
          {running ? (
            <button type="button" className="pill-button" onClick={act.lap}>
              Lap
            </button>
          ) : (
            started && (
              <button type="button" className="pill-button" onClick={act.reset}>
                Reset
              </button>
            )
          )}
          {running ? (
            <button type="button" className="pill-button is-solid" onClick={act.pause}>
              <Icon name="pause" />
              Pause
            </button>
          ) : (
            <button type="button" className="pill-button is-solid" onClick={act.start}>
              <Icon name="play" />
              {started ? "Resume" : "Start"}
            </button>
          )}
        </div>
      </div>
      {laps.length ? (
        <ol className="laps" aria-label="Laps">
          {laps.map((l) => (
            <li key={l.n} className="lap">
              <span className="lap-n">Lap {l.n}</span>
              <span className="lap-split">{formatElapsed(l.split, true)}</span>
              <span className="lap-at">{formatElapsed(l.at, true)}</span>
            </li>
          ))}
        </ol>
      ) : (
        <div className="laps-empty">Laps show up here.</div>
      )}
    </div>
  );
}

/** "5-minute", "1-hour", "1 h 30 min" (for captions and the "Time's up" card). */
function describeDuration(ms: number) {
  const min = Math.round(ms / 60_000);
  if (min < 60) return `${min}-minute`;
  if (min % 60 === 0) return `${min / 60}-hour`;
  return `${Math.floor(min / 60)} h ${min % 60} min`;
}

// ---------- pieces ----------

function Transport({ media, onMedia, size }: { media: MediaState; onMedia: (a: MediaAction) => void; size: "small" | "large" }) {
  return (
    <div className={`transport is-${size}`}>
      <button type="button" className="transport-button" aria-label="Previous" onClick={() => onMedia("previous")}>
        <Icon name="prev" />
      </button>
      <button type="button" className="transport-button is-main" aria-label={media.playing ? "Pause" : "Play"} onClick={() => onMedia("play_pause")}>
        <Icon name={media.playing ? "pause" : "play"} />
      </button>
      <button type="button" className="transport-button" aria-label="Next" onClick={() => onMedia("next")}>
        <Icon name="next" />
      </button>
    </div>
  );
}

function Calendar({ today, events }: { today: Date; events: CalendarEvent[] }) {
  // Days with an event get a dot (multi-day all-day events mark their first day).
  const busy = new Set(events.map((e) => eventDate(e.start, e.allDay).toDateString()));
  const [offset, setOffset] = useState(0);
  const first = new Date(today.getFullYear(), today.getMonth() + offset, 1);
  const daysInMonth = new Date(first.getFullYear(), first.getMonth() + 1, 0).getDate();
  const lead = first.getDay();
  const cells = Array.from({ length: 42 }, (_, i) => new Date(first.getFullYear(), first.getMonth(), i - lead + 1));
  const rows = Math.ceil((lead + daysInMonth) / 7);
  const weekdays = Array.from({ length: 7 }, (_, i) =>
    new Date(2026, 1, 1 + i).toLocaleDateString([], { weekday: "narrow" }),
  );
  const isToday = (d: Date) => d.toDateString() === today.toDateString();

  return (
    <div className="calendar">
      <div className="calendar-head">
        <span className="calendar-month">{first.toLocaleDateString([], { month: "long", year: "numeric" })}</span>
        <div className="calendar-nav">
          {offset !== 0 && (
            <button type="button" className="text-button" onClick={() => setOffset(0)}>
              Today
            </button>
          )}
          <button type="button" className="round-button is-small" aria-label="Previous month" onClick={() => setOffset((o) => o - 1)}>
            <Icon name="chevronLeft" />
          </button>
          <button type="button" className="round-button is-small" aria-label="Next month" onClick={() => setOffset((o) => o + 1)}>
            <Icon name="chevronRight" />
          </button>
        </div>
      </div>
      <div className="calendar-grid" role="grid">
        {weekdays.map((w, i) => (
          <span key={`w${i}`} className="calendar-weekday">
            {w}
          </span>
        ))}
        {cells.slice(0, rows * 7).map((d) => (
          <span
            key={d.toISOString()}
            className={`calendar-day${d.getMonth() !== first.getMonth() ? " is-outside" : ""}${isToday(d) ? " is-today" : ""}${busy.has(d.toDateString()) ? " has-event" : ""}`}
          >
            {d.getDate()}
          </span>
        ))}
      </div>
    </div>
  );
}

/** Home, while something is using the camera or microphone: which app, and since when. */
function PrivacyRow({ privacy }: { privacy: PrivacyState }) {
  const uses = [
    { kind: "camera" as const, label: "Camera", access: privacy.camera },
    { kind: "microphone" as const, label: "Microphone", access: privacy.microphone },
  ].filter((use) => use.access.active);
  return (
    <div className="privacy-row" role="status">
      {uses.map(({ kind, label, access }) => {
        const names = [...new Set(access.entries.map((e) => e.app))].join(", ");
        const started = access.entries[0]?.startedAt;
        return (
          <div key={kind} className={`privacy-use is-${kind}`}>
            <span className={`privacy-dot is-${kind}`} aria-hidden />
            <span className="privacy-label">{label}</span>
            <span className="privacy-app">{names}</span>
            {started && (
              <span className="privacy-time">
                since {new Date(started).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** The same change as a card, squeezed into the top bar while the notch is open. */
function SystemMeter({ card }: { card: SystemCard }) {
  const icon = card.kind === "volume" && card.muted ? "volumeMuted" : SYSTEM_ICON[card.kind];
  const name =
    card.kind === "charging" ? "Charging" : card.kind === "low-battery" ? "Battery low" : card.kind === "brightness" ? "Brightness" : "Volume";
  const text = card.muted ? "Muted" : `${card.level}%`;
  return (
    <span className={`topbar-meter is-${card.kind}${card.muted ? " is-muted" : ""}`} role="status" title={`${name} ${text}`}>
      <Icon name={icon} />
      <span className="topbar-meter-bar" aria-hidden>
        <span style={{ width: `${card.muted ? 0 : card.level}%` }} />
      </span>
      <span className="topbar-meter-text">{text}</span>
    </span>
  );
}

function EmptyState({ icon, title, children }: { icon: IconName; title: string; children: ReactNode }) {
  return (
    <div className="empty">
      <Icon name={icon} className="empty-icon" />
      <div className="empty-title">{title}</div>
      <div className="empty-text">{children}</div>
    </div>
  );
}

function Artwork({ media, className }: { media: MediaState; className: string }) {
  return media.artwork ? <img className={`artwork ${className}`} src={media.artwork} alt="" /> : <div className={`artwork is-blank ${className}`} />;
}

function AppIcon({ app, icon, className }: { app: string; icon?: string; className: string }) {
  if (icon) return <img className={`app-icon ${className}`} src={icon} alt="" />;
  const hue = [...app].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 0);
  return (
    <span className={`app-icon is-letter ${className}`} style={{ background: `hsl(${hue} 55% 46%)` }} aria-hidden>
      {app.charAt(0).toUpperCase()}
    </span>
  );
}

/** Decorative bars in the cover's colour. Real levels would need loopback audio capture. */
function Visualizer({ playing }: { playing: boolean }) {
  return (
    <div className={`visualizer${playing ? " is-playing" : ""}`} aria-hidden>
      {[0, 1, 2, 3, 4].map((i) => (
        <span key={i} style={{ animationDelay: `${i * -0.19}s` }} />
      ))}
    </div>
  );
}

function useNow(everyMs: number) {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = window.setInterval(() => setNow(new Date()), everyMs);
    return () => window.clearInterval(t);
  }, [everyMs]);
  return now;
}

function ago(at: number, now: Date) {
  const s = Math.max(0, Math.round((now.getTime() - at) / 1000));
  if (s < 60) return "now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  return h < 24 ? `${h}h` : `${Math.round(h / 24)}d`;
}
