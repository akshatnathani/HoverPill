import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type MouseEvent, type PointerEvent } from "react";
import { AnimatePresence, motion, useMotionValue, type Transition } from "motion/react";
import {
  beginDrag,
  dockLayout,
  endDrag,
  inTauri,
  installUpdate,
  mediaCommand,
  notchTyping,
  on,
  openLink,
  openSettings,
  pinNotch,
  resolvePermission,
  setHitRect,
} from "../lib/bridge";
import { pickView, usePillState } from "../lib/usePillState";
import { formatCountdown, formatElapsed, stopwatchElapsed, timerLeft, useTimers } from "../lib/useTimers";
import { useLiveSettings } from "../lib/useSettings";
import { useArtworkColor } from "../lib/useArtworkColor";
import { useAccounts, useChat } from "../lib/useAccounts";
import { IS_LINUX, type Layout, type PrivacyState, type SystemCardKind, type View } from "../lib/types";
import {
  AlarmView,
  BubbleView,
  ExpandedView,
  IdleView,
  MediaCompactView,
  NotificationView,
  PermissionView,
  SystemCardView,
  TimerCompactView,
  UpdateView,
  releaseTypingAfterBlur,
} from "./views";

/**
 * Target size per view, in CSS px. The window is fixed (see notch.rs); only the notch morphs.
 * `radius` is the bottom corners; attached to the top edge the top corners are square.
 */
type Size = { width: number; height: number; radius: number };

const SIZES: Record<View, Size> = {
  idle: { width: 190, height: 30, radius: 12 },
  media: { width: 300, height: 32, radius: 14 },
  timer: { width: 210, height: 32, radius: 14 },
  alarm: { width: 440, height: 78, radius: 24 },
  update: { width: 440, height: 78, radius: 24 },
  notification: { width: 420, height: 78, radius: 24 },
  permission: { width: 520, height: 260, radius: 26 },
  system: { width: 320, height: 78, radius: 24 },
  expanded: { width: 680, height: 300, radius: 30 },
};

/**
 * Docked on the left or right edge the open views are narrow and tall instead, so they don't
 * reach far across the screen. Must fit `SIDE_WINDOW_WIDTH`/`HEIGHT` in notch.rs.
 */
const SIDE_SIZES: Partial<Record<View, Size>> = {
  alarm: { width: 260, height: 124, radius: 22 },
  update: { width: 260, height: 124, radius: 22 },
  notification: { width: 260, height: 132, radius: 22 },
  permission: { width: 280, height: 340, radius: 24 },
  system: { width: 260, height: 120, radius: 22 },
  expanded: { width: 380, height: 500, radius: 30 },
};

/** Idle with "shrink when idle" on: a thin bar that barely overlaps anything. */
const IDLE_BAR: Size = { width: 120, height: 6, radius: 3 };

/** What it is while Ctrl+dragged: a round bubble under the pointer. */
const BUBBLE = 52;
const BUBBLE_SIZE: Size = { width: BUBBLE, height: BUBBLE, radius: BUBBLE / 2 };

/** The closed views. Docked on a side edge these stand upright, their contents turned sideways. */
const CLOSED: View[] = ["idle", "media", "timer"];

/** Keeps the dragged bubble this far inside the window. */
const INSET = 6;
/** Linux: how long a press must stay still to pick the notch up (see `onPointerDown`). */
const HOLD_TO_DRAG_MS = 400;
/** CSS px a press may wander and still count as held still. */
const HOLD_SLOP = 6;
/** Keeps a docked notch (and its ears) this far inside the window along its edge. */
const EDGE_ROOM = 18;

/** Pointed at while closed: faint enough to read the browser tab underneath, still findable. */
const PEEK_OPACITY = 0.12;

// Slightly under-damped so the notch overshoots a touch as it drops, then settles.
const SPRING: Transition = { type: "spring", stiffness: 420, damping: 32, mass: 0.9 };

const clamp = (value: number, lo: number, hi: number) => (hi < lo ? lo : Math.min(hi, Math.max(lo, value)));

export function Pill() {
  const settings = useLiveSettings();
  const { scale, topOffset } = settings.display;
  const attached = topOffset === 0;
  const durationMs = settings.notifications.durationSecs * 1000;
  const { state, dispatch, setOpen } = usePillState(durationMs);
  const timers = useTimers();
  const accounts = useAccounts();
  const chat = useChat();
  const { timer, stopwatch, now } = timers;
  const ringing = timer.status === "done";
  // What the closed notch shows ticking: the countdown wins over the stopwatch.
  const clock =
    timer.status === "running" || timer.status === "paused"
      ? formatCountdown(timerLeft(timer, now))
      : stopwatch.startedAt !== null
        ? formatElapsed(stopwatchElapsed(stopwatch, now))
        : null;
  const enabled = (kind: SystemCardKind) =>
    kind === "volume" ? settings.system.showVolume : kind === "brightness" ? settings.system.showBrightness : settings.system.showBattery;
  // Rust always sends both; the settings decide what the notch shows.
  const systemCard = state.systemCard && enabled(state.systemCard.kind) ? state.systemCard : null;
  const privacy: PrivacyState = {
    camera: settings.privacy.cameraIndicator ? state.privacy.camera : { entries: [], active: false },
    microphone: settings.privacy.microphoneIndicator ? state.privacy.microphone : { entries: [], active: false },
  };
  const dots = privacy.camera.active || privacy.microphone.active;
  const view = pickView(state, { ringing, ticking: clock !== null }, systemCard);
  const accent = useArtworkColor(state.media?.artwork);
  const ref = useRef<HTMLDivElement>(null);

  // ---------- where it lives: the notch, a bubble, or under the pointer ----------

  // Unknown until Rust says; nothing is drawn till then, so a bubble doesn't slide in from a corner.
  const [known, setLayout] = useState<Layout | null>(null);
  const layout: Layout = known ?? { mode: "notch" };
  /** While Ctrl+dragging: the bubble's centre, CSS px from the window's top-left. */
  const [drag, setDrag] = useState<{ x: number; y: number } | null>(null);
  const [grab, setGrab] = useState(false);
  const windowSize = useWindowSize();
  const dragging = useRef(false);
  /** Where the bubble started (CSS px in the window), so `pill:drag-offset` can move it from there. */
  const dragStart = useRef({ x: 0, y: 0 });
  const swallowClick = useRef(false);
  // Its offset when laid out freely (scaler px). Animated with its size by the same spring
  // (`animate` below), so a panel opening from a bubble keeps that side still as it grows.
  const x = useMotionValue(0);
  const y = useMotionValue(0);
  const scaleRef = useRef(scale);
  scaleRef.current = scale;

  useEffect(() => {
    dockLayout().then(setLayout).catch(console.error);
    const subs = [
      on("pill:dock", ({ layout, shift }) => {
        if (layout.mode === "notch") {
          x.jump(0);
          y.jump(0);
        } else if (shift) {
          // Dropped, and the window jumped to its new spot: put the bubble back where it was let
          // go, then let it slide to the edge from there.
          x.jump(x.get() + shift[0] / scaleRef.current);
          y.jump(y.get() + shift[1] / scaleRef.current);
        }
        setLayout(layout);
        if (shift) {
          dragging.current = false;
          setDrag(null);
        }
      }),
      on("pill:grab", setGrab),
      on("pill:drag-offset", ([dx, dy]) => {
        if (!dragging.current) return;
        const reach = (BUBBLE * scaleRef.current) / 2 + INSET;
        const start = dragStart.current;
        setDrag({
          x: clamp(start.x + dx, reach, window.innerWidth - reach),
          y: clamp(start.y + dy, reach, window.innerHeight - reach),
        });
      }),
    ];
    return () => subs.forEach((s) => s.off());
  }, [x, y]);

  // Laid out freely in the window instead of centred at the top.
  const free = drag !== null || layout.mode === "docked";
  const edge = drag ? null : layout.mode === "docked" ? layout.edge : "top";
  const side = edge === "left" || edge === "right";
  // Closed against the left or right edge: the notch turned on its side.
  const upright = side && CLOSED.includes(view);
  const flat =
    view === "idle" && settings.display.compactIdle && !dots ? IDLE_BAR : (side && SIDE_SIZES[view]) || SIZES[view];
  const size = drag ? BUBBLE_SIZE : upright ? { ...flat, width: flat.height, height: flat.width } : flat;
  // Behind a fullscreen app only what's waiting on you (a Claude request, a finished timer) shows,
  // or ChatGPT when its shortcut was pressed.
  const hidden = state.fullscreen && view !== "permission" && view !== "alarm" && !state.asked && !state.pinned;
  // Closed (and a passing notification) never takes clicks, so browser tabs underneath stay usable.
  const interactive = !hidden && !drag && (view === "expanded" || view === "permission" || view === "alarm" || view === "update");
  // Pointing at the closed notch fades it, unless resting is how you open it.
  const faded = state.peek && !interactive && !settings.display.openOnHover && !free;

  // Where its top-left goes, in the scaler's (unscaled) px. Docked, it stays flat against its
  // edge and grows from there towards the middle of the screen.
  let tx = 0;
  let ty = 0;
  if (drag) {
    tx = drag.x / scale - size.width / 2;
    ty = drag.y / scale - size.height / 2;
  } else if (layout.mode === "docked") {
    const [px, py] = [layout.x / scale, layout.y / scale];
    const along = (middle: number, length: number, room: number) => clamp(middle - length / 2, EDGE_ROOM, room - length - EDGE_ROOM);
    if (layout.edge === "top") {
      tx = along(px, size.width, windowSize.w / scale);
    } else {
      tx = layout.edge === "left" ? px : px - size.width;
      ty = along(py, size.height, windowSize.h / scale);
    }
  }

  // Report the notch's on-screen box to Rust every time it changes size (every frame of the
  // spring), so click-through always matches what's drawn. Scale, top gap, window resizes and a
  // bubble sliding move it without resizing it, so those re-report too.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const report = () => {
      const r = el.getBoundingClientRect();
      setHitRect({ x: r.x, y: r.y, width: r.width, height: r.height }, interactive);
    };
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    window.addEventListener("resize", report);
    const moves = [x.on("change", report), y.on("change", report)];
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", report);
      moves.forEach((off) => off());
    };
  }, [scale, topOffset, interactive, free, known === null, x, y]);

  // Opened by the ChatGPT shortcut, it has the keyboard until Esc or a click elsewhere. On
  // Windows Rust notices the click elsewhere (another app comes to the front); on Linux the
  // window losing focus is the sign.
  useEffect(() => {
    if (!state.asked) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && notchTyping(false);
    const onBlur = () => releaseTypingAfterBlur();
    window.addEventListener("keydown", onKey);
    if (IS_LINUX) window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onBlur);
    };
  }, [state.asked]);

  // ---------- Ctrl+drag ----------

  /** Linux: a press that may become a pick-up if it's held still (see `onPointerDown`). */
  const hold = useRef<{ timer: number; x: number; y: number } | null>(null);
  const cancelHold = () => {
    if (hold.current) window.clearTimeout(hold.current.timer);
    hold.current = null;
  };
  useEffect(() => cancelHold, []);

  const startDrag = (clientX: number, clientY: number, pointerId: number) => {
    const el = ref.current;
    if (!el || dragging.current) return;
    try {
      el.setPointerCapture(pointerId);
    } catch {
      // The pointer is already up; the drag loop's own button check ends it at once.
    }
    // Start the bubble from where the notch is drawn, so it shrinks out of it. React lays it out
    // freely before motion next writes the offset, so it never shows in the wrong frame.
    if (!free) {
      const box = el.getBoundingClientRect();
      x.jump(box.left / scale);
      y.jump(box.top / scale);
    }
    // Under the pointer, but wholly inside the window.
    const reach = (BUBBLE * scale) / 2 + INSET;
    dragStart.current = { x: clamp(clientX, reach, window.innerWidth - reach), y: clamp(clientY, reach, window.innerHeight - reach) };
    setDrag(dragStart.current);
    dragging.current = true;
    swallowClick.current = true;
    beginDrag();
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (!inTauri || e.button !== 0 || dragging.current) return;
    if (e.ctrlKey || grab) {
      e.preventDefault();
      e.stopPropagation();
      startDrag(e.clientX, e.clientY, e.pointerId);
      return;
    }
    // Linux: under Wayland the pill never has the keyboard, so it doesn't see Ctrl while another
    // app has focus. Pressing and holding still picks it up instead. A click, or a press that
    // moves first (selecting text), stays what it was. Fields you type in are left alone.
    if (!IS_LINUX || (e.target as Element).closest("input, textarea, select, [contenteditable]")) return;
    cancelHold();
    const { clientX, clientY, pointerId } = e;
    hold.current = {
      x: clientX,
      y: clientY,
      timer: window.setTimeout(() => {
        hold.current = null;
        window.getSelection()?.removeAllRanges();
        startDrag(clientX, clientY, pointerId);
      }, HOLD_TO_DRAG_MS),
    };
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const h = hold.current;
    if (h && Math.hypot(e.clientX - h.x, e.clientY - h.y) > HOLD_SLOP) cancelHold();
  };

  const onPointerUp = () => {
    cancelHold();
    if (!dragging.current) return;
    endDrag()
      .catch(console.error)
      // Rust answers with `pill:dock`; if that never comes, don't stay stuck as a dragged bubble.
      .finally(() =>
        window.setTimeout(() => {
          if (dragging.current) {
            dragging.current = false;
            setDrag(null);
          }
        }, 500),
      );
  };

  // The press that picked it up mustn't also click a button inside it.
  const onClickCapture = (e: MouseEvent) => {
    if (!swallowClick.current) return;
    swallowClick.current = false;
    e.preventDefault();
    e.stopPropagation();
  };

  const permission = state.permissions[0];
  const notification = state.queue[0];
  // Square where it meets the screen edge, rounded everywhere else.
  const radius = Math.min(size.radius, size.width / 2, size.height / 2);
  const flush = free ? edge : attached ? "top" : null;
  const corner = (...edges: (typeof edge)[]) => (edges.includes(flush) ? 0 : radius);
  // Ears where it meets the edge, like a display cut-out; a floating pill or the bubble has none.
  const ears = flush !== null;

  if (!known) return <div className="stage" />;

  return (
    <div className={`stage${free ? " is-free" : ""}`} style={{ paddingTop: free ? 0 : topOffset }}>
      {/* Scaled as a whole so the spring keeps animating unscaled sizes; the hit rect is measured after scaling. */}
      <div className="scaler" style={{ transform: `scale(${scale})` }}>
        <motion.div
          ref={ref}
          className={[
            "notch",
            ears ? "is-attached" : "is-floating",
            accent ? "has-accent" : "",
            dots ? "has-privacy" : "",
            side ? "is-narrow" : "",
            drag ? "is-dragging" : grab ? "is-grab" : "",
          ]
            .filter(Boolean)
            .join(" ")}
          data-edge={flush ?? undefined}
          style={{ ...({ ...(accent ? { "--accent": accent } : {}), "--ear": `${Math.min(12, upright ? size.width : size.height)}px` } as CSSProperties), x, y }}
          initial={false}
          animate={{
            x: tx,
            y: ty,
            width: size.width,
            height: size.height,
            borderTopLeftRadius: corner("top", "left"),
            borderTopRightRadius: corner("top", "right"),
            borderBottomLeftRadius: corner("left"),
            borderBottomRightRadius: corner("right"),
            opacity: hidden ? 0 : faded ? PEEK_OPACITY : 1,
          }}
          transition={{ ...SPRING, opacity: { duration: 0.15, ease: "easeOut" } }}
          onPointerDownCapture={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onLostPointerCapture={onPointerUp}
          onClickCapture={onClickCapture}
          // In Tauri, opening is decided by the Rust cursor poller; these only drive the browser preview.
          onMouseEnter={inTauri ? undefined : () => setOpen(true)}
          onMouseLeave={inTauri ? undefined : () => setOpen(false)}
        >
          {ears && (
            <>
              <span className="ear is-left" aria-hidden />
              <span className="ear is-right" aria-hidden />
            </>
          )}
          <div className="notch-clip">
            <AnimatePresence initial={false}>
              <motion.div
                key={
                  drag
                    ? "bubble"
                    : view === "permission"
                      ? `p-${permission?.id}`
                      : view === "notification"
                        ? `n-${notification?.id}`
                        : view === "system"
                          ? `s-${systemCard?.at}`
                          : `${view}-${upright ? edge : "flat"}`
                }
                className={`content${upright ? ` is-sideways on-${edge}` : ""}`}
                // Upright, the content keeps its usual shape and is turned to fit.
                style={{ width: upright ? flat.width : size.width, height: upright ? flat.height : size.height }}
                initial={{ opacity: 0, filter: "blur(6px)" }}
                animate={{ opacity: 1, filter: "blur(0px)", transition: { duration: 0.2, delay: 0.07 } }}
                exit={{ opacity: 0, filter: "blur(4px)", transition: { duration: 0.1 } }}
              >
                {drag && <BubbleView media={state.media} clock={clock} />}
                {!drag && view === "idle" && <IdleView privacy={privacy} />}
                {!drag && view === "media" && state.media && <MediaCompactView media={state.media} clock={clock} privacy={privacy} />}
                {!drag && view === "timer" && clock && <TimerCompactView clock={clock} privacy={privacy} />}
                {!drag && view === "alarm" && <AlarmView durationMs={timer.durationMs} onSnooze={timers.timerActions.snooze} onDone={timers.timerActions.reset} />}
                {!drag && view === "system" && systemCard && <SystemCardView card={systemCard} />}
                {!drag && view === "notification" && notification && <NotificationView item={notification} durationMs={durationMs} />}
                {!drag && view === "update" && state.update && (
                  <UpdateView
                    offer={state.update}
                    onLater={() => dispatch({ type: "update", value: null })}
                    onInstall={() => {
                      const offer = state.update!;
                      dispatch({ type: "update", value: null });
                      if (offer.mode === "download") openLink("https://github.com/PreetinderSinghBadesha/HoverPill/releases/latest");
                      else installUpdate().catch(console.error);
                    }}
                  />
                )}
                {!drag && view === "permission" && permission && (
                  <PermissionView
                    request={permission}
                    queued={state.permissions.length - 1}
                    onDecide={(decision) => {
                      resolvePermission(permission.id, decision);
                      dispatch({ type: "decide", id: permission.id, decision });
                    }}
                  />
                )}
                {!drag && view === "expanded" && (
                  <ExpandedView
                    tab={state.tab}
                    onTab={(value) => dispatch({ type: "tab", value })}
                    media={state.media}
                    onMedia={(action) => {
                      mediaCommand(action);
                      // Optimistic: the real state arrives back via `pill:media` once the OS confirms.
                      if (action === "play_pause" && state.media)
                        dispatch({ type: "media", value: { ...state.media, playing: !state.media.playing } });
                    }}
                    history={state.history}
                    unseen={state.unseen}
                    onClearHistory={() => dispatch({ type: "clearHistory" })}
                    decisions={state.decisions}
                    timers={timers}
                    onSettings={openSettings}
                    calendar={state.calendar}
                    chat={chat}
                    chatgpt={accounts.chatgpt}
                    asked={state.asked}
                    pinned={state.pinned}
                    onPin={(value) => {
                      pinNotch(value);
                      dispatch({ type: "pin", value });
                    }}
                    privacy={privacy}
                    systemCard={systemCard}
                  />
                )}
              </motion.div>
            </AnimatePresence>
          </div>
        </motion.div>
      </div>
    </div>
  );
}

function useWindowSize() {
  const [size, setSize] = useState({ w: window.innerWidth, h: window.innerHeight });
  useEffect(() => {
    const update = () => setSize({ w: window.innerWidth, h: window.innerHeight });
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  return size;
}
