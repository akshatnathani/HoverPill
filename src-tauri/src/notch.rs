//! The floating window: placement at the top-center of the chosen monitor (or, after a
//! Ctrl+drag, docked flat against the left, right or top edge of a screen; see `dock.rs`),
//! staying on top, and selective click-through.
//!
//! The window is always click-through except while the notch is *open* (or showing a card you
//! must click, like a Claude permission request) and the cursor is over it. A closed notch sits
//! on top of browser tab strips and title bars, so it never takes clicks: they go to whatever is
//! underneath. To open it you push the pointer against the top edge of the screen above it, or
//! (if enabled) rest the pointer on it for a moment.
//!
//! A background thread polls the global cursor because a click-through window receives no
//! mouse events. On Windows that poll is how hover is detected. On Linux the window shapes its
//! own X input region to the notch (see `set_input_region`) and the GTK enter/leave signals are
//! used instead: under XWayland the global cursor freezes the moment the real pointer leaves that
//! region, so polling would strand the notch open. Either way the webview reports the notch's box
//! and whether it's interactive via `set_hit_rect`, and is told to open or close via `pill:hover`.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, State, WebviewWindow};

use crate::appbar;
use crate::dock::{self, DockStore, Docked, Edge};
use crate::events;
#[cfg(target_os = "linux")]
use crate::shellext;
use crate::settings::{self, Display, SettingsStore};

pub const LABEL: &str = "pill";

/// Window size in logical px at scale 1 with no top gap. Must fit the largest notch view (see
/// `SIZES` in src/pill/Pill.tsx) plus its shadow.
const WINDOW_WIDTH: f64 = 720.0;
const WINDOW_HEIGHT: f64 = 340.0;
/// Docked on a side edge the open views are narrow and tall (`SIDE_SIZES` in src/pill/Pill.tsx),
/// so they don't reach far across the screen; the window is shaped to match.
const SIDE_WINDOW_WIDTH: f64 = 420.0;
const SIDE_WINDOW_HEIGHT: f64 = 560.0;

/// ~60 Hz keeps hover feeling instant; each tick is one GetCursorPos/XQueryPointer.
const POLL: Duration = Duration::from_millis(16);
/// Every ~1s: re-assert z-order, follow monitor/DPI changes, refresh cached geometry.
const SLOW_TICK_EVERY: u64 = 60;
/// Slack around the notch (CSS px) so its edge isn't a hair trigger.
const HIT_PADDING: f64 = 4.0;
/// "Pushed against the top edge": within this many physical px of the monitor's top.
const EDGE_PX: f64 = 2.0;
/// How long the pointer must rest on a closed notch to open it (when that's enabled). Long
/// enough that reaching for a browser tab underneath doesn't open it. Measured in real time:
/// `sleep(16ms)` can take twice that on Windows, so counting ticks would drift.
const DWELL: Duration = Duration::from_millis(600);
/// The top-edge trigger is at least this wide either side of the notch's centre (CSS px), so
/// the thin idle bar is still easy to open.
const EDGE_HALF_WIDTH: f64 = 110.0;
/// How often to look for a fullscreen app (~0.3 s at the poll rate).
const FULLSCREEN_CHECK_EVERY: u64 = 20;
/// Height (CSS px, before scale) of the strip "Keep windows below the notch" reserves: the
/// closed notch with something playing. Cards and the open notch still drop over windows.
const RESERVED_HEIGHT: f64 = 32.0;
/// How long the pointer rests on a notch docked off the top centre before it opens. Short: it's
/// out of the way there, not over browser tabs you'd be reaching for.
const DOCKED_DWELL: Duration = Duration::from_millis(150);
/// A drop this close (logical px) either side of a monitor's top centre makes it the notch there
/// again.
const NOTCH_SNAP: f64 = 160.0;
/// While dragging, follow the pointer faster than the hover poll.
const DRAG_POLL: Duration = Duration::from_millis(8);

/// The notch's box in CSS px, relative to the window's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Default)]
pub struct HitState {
    shape: Mutex<Option<(Rect, bool)>>,
    /// Someone is typing in the notch (asking ChatGPT): it keeps the keyboard and stays open.
    typing: AtomicBool,
    /// Linux: the X window that had the keyboard before `grab_keyboard` took it, so it can be
    /// handed back when typing ends.
    #[cfg(target_os = "linux")]
    previous_focus: Mutex<Option<u64>>,
    /// Locked open with the lock button: it stays open, wherever the pointer goes, until unlocked.
    pinned: AtomicBool,
}

/// The lock button in the open notch: keep it open (or let it close again as usual).
#[tauri::command]
pub fn notch_pinned(pinned: bool, state: State<HitState>) {
    state.pinned.store(pinned, Ordering::Relaxed);
}

/// The notch normally never takes keyboard focus. While the user types a question it has to,
/// so this lifts that for as long as the text box is focused, and puts it back afterwards. The
/// ChatGPT shortcut calls it too, to open the notch ready to type (see `shortcut.rs`).
#[tauri::command]
pub fn notch_typing(active: bool, app: AppHandle, state: State<HitState>) {
    state.typing.store(active, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.set_focusable(active);
        if active {
            // On Linux, asking the window manager (tao's `set_focus`, a zero-timestamp
            // `present`) is refused as focus stealing, and the refusal itself puts the
            // keyboard back on the window that had it a moment later. Hand the keyboard over
            // directly, then ask the window manager properly so it keeps it there.
            #[cfg(not(target_os = "linux"))]
            let _ = win.set_focus();
            #[cfg(target_os = "linux")]
            {
                grab_keyboard(&win);
                activate(&win);
            }
        } else {
            #[cfg(target_os = "linux")]
            restore_keyboard(&win);
        }
    }
}

/// Hand the keyboard to the notch while the text box is focused.
///
/// A click alone can't do it on Linux: the window is mapped with the "does not accept input
/// focus" hint (that's what keeps the pill from ever pulling you out of your terminal), and the
/// window manager has already refused to focus us by the time the text box sees the click.
/// Lifting the hint and asking again doesn't help either - tao asks with a zero timestamp, which
/// is refused as focus stealing, and the window manager ignores the changed hint for the focus
/// decision. So once the text box takes focus, hand the keyboard to the X server directly
/// (XWayland passes that on to the compositor); `activate` then asks the window manager with a
/// real timestamp so it keeps it there. Only ever called while typing: the rest of the time the
/// notch must not be focusable.
#[cfg(target_os = "linux")]
fn grab_keyboard(win: &WebviewWindow) {
    use x11_dl::xlib::{CurrentTime, RevertToParent, Window, Xlib};

    let win = win.clone();
    let handle = win.clone();
    let _ = handle.run_on_main_thread(move || {
        use gtk::prelude::*;
        let Ok(gtk_win) = win.gtk_window() else { return };
        let Some(gdk_win) = gtk_win.window() else { return };
        // Only the X11 backend has an X window to focus; under a native Wayland backend the
        // downcast fails and there is nothing to do (the Linux preflight settles on X11 anyway).
        let Ok(x11_win) = gdk_win.downcast::<gdkx11::X11Window>() else { return };
        let xid: Window = x11_win.xid();
        let Ok(xlib) = Xlib::open() else { return };
        // SAFETY: a short-lived connection of our own; `XCloseDisplay` flushes the requests.
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return;
            }
            // Remember who had it, unless that is already us: restoring our own window would
            // leave the keyboard nowhere when the notch closes. The focused window can be a
            // child of the pill's (the webview's), so walk up rather than compare ids.
            let mut previous: Window = 0;
            let mut revert: i32 = 0;
            (xlib.XGetInputFocus)(display, &mut previous, &mut revert);
            if previous > 1 && !is_within(&xlib, display, previous, xid) {
                *win.state::<HitState>().previous_focus.lock().unwrap() = Some(previous as u64);
            }
            (xlib.XSetInputFocus)(display, xid, RevertToParent, CurrentTime);
            (xlib.XCloseDisplay)(display);
        }
    });
}

/// Whether `window` is `ancestor` or sits inside it. X input focus can be on one of the
/// webview's child windows rather than the toplevel, so an id comparison is not enough.
#[cfg(target_os = "linux")]
unsafe fn is_within(xlib: &x11_dl::xlib::Xlib, display: *mut x11_dl::xlib::Display, mut window: x11_dl::xlib::Window, ancestor: x11_dl::xlib::Window) -> bool {
    use x11_dl::xlib::Window;

    for _ in 0..32 {
        if window == ancestor {
            return true;
        }
        if window <= 1 {
            return false;
        }
        let mut root: Window = 0;
        let mut parent: Window = 0;
        let mut children: *mut Window = std::ptr::null_mut();
        let mut count: u32 = 0;
        if (xlib.XQueryTree)(display, window, &mut root, &mut parent, &mut children, &mut count) == 0 {
            return false;
        }
        if !children.is_null() {
            (xlib.XFree)(children.cast());
        }
        if parent == 0 || parent == root {
            return false;
        }
        window = parent;
    }
    false
}

/// Asks the window manager to make the notch the active window, with a real timestamp.
///
/// `tao`'s `set_focus` presents with `GDK_CURRENT_TIME`, which Mutter reads as focus stealing
/// and refuses, leaving the keyboard with whatever app was in front. A timestamp read from the
/// X server is accepted, so the shortcut-opened notch really is focused and can be typed into,
/// and the X focus from `grab_keyboard` is not handed back a moment later.
#[cfg(target_os = "linux")]
fn activate(win: &WebviewWindow) {
    let win = win.clone();
    let handle = win.clone();
    let _ = handle.run_on_main_thread(move || {
        use gdkx11::glib::translate::ToGlibPtr;
        use gtk::prelude::*;
        let Ok(gtk_win) = win.gtk_window() else { return };
        let Some(gdk_win) = gtk_win.window() else { return };
        let Ok(x11_win) = gdk_win.downcast::<gdkx11::X11Window>() else { return };
        // SAFETY: the window is alive on this thread; the call only reads a server timestamp.
        let timestamp = unsafe { gdkx11::ffi::gdk_x11_get_server_time(x11_win.to_glib_none().0) };
        gtk_win.present_with_time(timestamp);
    });
}

/// Hands the keyboard back after typing (the notch closing, Esc, or another app taking it).
#[cfg(target_os = "linux")]
fn restore_keyboard(win: &WebviewWindow) {
    use x11_dl::xlib::{CurrentTime, RevertToParent, Window, Xlib};

    let win = win.clone();
    let handle = win.clone();
    let _ = handle.run_on_main_thread(move || {
        use gtk::prelude::*;
        let Some(previous) = win.state::<HitState>().previous_focus.lock().unwrap().take() else { return };
        let Ok(gtk_win) = win.gtk_window() else { return };
        let Some(gdk_win) = gtk_win.window() else { return };
        let Ok(x11_win) = gdk_win.downcast::<gdkx11::X11Window>() else { return };
        let Ok(xlib) = Xlib::open() else { return };
        // SAFETY: a short-lived connection of our own; `XCloseDisplay` flushes the request.
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return;
            }
            // Only if we still have it: clicking another window already gave it away, and
            // pulling it back here would steal focus from where the click landed. The focus
            // may be on a child of the pill's rather than the toplevel itself.
            let mut current: Window = 0;
            let mut revert: i32 = 0;
            (xlib.XGetInputFocus)(display, &mut current, &mut revert);
            if is_within(&xlib, display, current, x11_win.xid()) {
                (xlib.XSetInputFocus)(display, previous as Window, RevertToParent, CurrentTime);
            }
            (xlib.XCloseDisplay)(display);
        }
    });
}

/// `interactive`: the notch is open, or shows something that must be clickable. Otherwise it
/// stays click-through even under the cursor.
#[tauri::command]
pub fn set_hit_rect(rect: Rect, interactive: bool, state: State<HitState>) {
    *state.shape.lock().unwrap() = Some((rect, interactive));
}

/// Whether the pointer is over the notch. On Linux this is driven by the window's GTK
/// enter/leave signals (see `set_input_region`): under XWayland the global cursor Rust polls
/// freezes the moment the real pointer leaves the notch's X input region.
#[derive(Default)]
pub struct HoverState(#[cfg_attr(not(target_os = "linux"), allow(dead_code))] pub AtomicBool);

/// The monitor chosen in settings, falling back to the primary one if it's unplugged.
fn chosen_monitor(win: &WebviewWindow, wanted: Option<&str>) -> Result<Monitor, Box<dyn Error>> {
    if let Some(wanted) = wanted {
        if let Some(m) = win.available_monitors()?.into_iter().find(|m| m.name().map(String::as_str) == Some(wanted)) {
            return Ok(m);
        }
    }
    match win.primary_monitor()? {
        Some(m) => Ok(m),
        None => Ok(win.current_monitor()?.ok_or("no monitor found")?),
    }
}

fn find_monitor(win: &WebviewWindow, name: &str) -> Option<Monitor> {
    win.available_monitors().ok()?.into_iter().find(|m| m.name().map(String::as_str) == Some(name))
}

/// How the webview should lay the pill out in its window. Sent as `pill:dock`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum Layout {
    /// Attached to the top edge, centred.
    Notch,
    /// Flat against `edge`; `x`, `y` are the middle of where it meets that edge, in CSS px from
    /// the window's top-left. Opening grows it from there towards the middle of the screen.
    Docked { edge: Edge, x: f64, y: f64 },
}

/// Where everything should be, in physical screen pixels.
struct Target {
    /// Window position and size: horizontally centered at the top of the chosen monitor, sized
    /// for the notch's scale and top gap (both applied in CSS; see src/pill/Pill.tsx). Docked,
    /// against its edge of the work area instead.
    pos: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    /// The chosen monitor's bounds: x, y, width, height.
    monitor: (i32, i32, i32, i32),
    /// The strip to keep windows out of, if "Keep windows below the notch" is on.
    reserve: Option<appbar::Strip>,
    layout: Layout,
}

fn target(win: &WebviewWindow) -> Result<Target, Box<dyn Error>> {
    let display = win.try_state::<SettingsStore>().map(|s| s.get().display).unwrap_or_default();
    if let Some(docked) = win.try_state::<DockStore>().and_then(|d| d.get()) {
        return docked_target(win, &display, &docked);
    }
    let monitor = chosen_monitor(win, display.monitor.as_deref())?;
    let dpi = monitor.scale_factor();
    let width = WINDOW_WIDTH * display.scale;
    let height = display.top_offset + WINDOW_HEIGHT * display.scale;
    let size = PhysicalSize::new((width * dpi).round() as u32, (height * dpi).round() as u32);
    let origin = *monitor.position();
    let (mw, mh) = (monitor.size().width as i32, monitor.size().height as i32);
    let x = origin.x + (mw - size.width as i32) / 2;
    let reserve = display.reserve_space.then(|| {
        let strip = ((display.top_offset + RESERVED_HEIGHT * display.scale) * dpi).round() as i32;
        (origin.x, origin.y, origin.x + mw, origin.y + strip)
    });
    Ok(Target { pos: PhysicalPosition::new(x, origin.y), size, monitor: (origin.x, origin.y, mw, mh), reserve, layout: Layout::Notch })
}

/// The window flush against the docked edge of the work area (clear of the taskbar), centred on
/// the notch's spot along it, with room for the full panel to open from that edge towards the
/// middle of the screen. Near a corner the window stays on screen and the notch sits off its
/// centre.
fn docked_target(win: &WebviewWindow, display: &Display, docked: &Docked) -> Result<Target, Box<dyn Error>> {
    let monitor = match docked.monitor.as_deref().and_then(|name| find_monitor(win, name)) {
        Some(m) => m,
        None => chosen_monitor(win, display.monitor.as_deref())?,
    };
    let dpi = monitor.scale_factor();
    let unit = display.scale * dpi;
    let (width, height) = match docked.edge {
        Edge::Top => (WINDOW_WIDTH, WINDOW_HEIGHT),
        Edge::Left | Edge::Right => (SIDE_WINDOW_WIDTH, SIDE_WINDOW_HEIGHT),
    };
    let size = PhysicalSize::new((width * unit).round() as u32, (height * unit).round() as u32);
    let (w, h) = (size.width as f64, size.height as f64);
    let area = monitor.work_area();
    let (ax, ay) = (area.position.x as f64, area.position.y as f64);
    let (aw, ah) = (area.size.width as f64, area.size.height as f64);
    // The window's top-left, and where the notch meets the edge inside it.
    let (x, y, px, py) = match docked.edge {
        Edge::Left | Edge::Right => {
            let centre = ay + docked.along * ah;
            let y = within(centre - h / 2.0, ay, ay + ah - h).round();
            let left = docked.edge == Edge::Left;
            (if left { ax } else { ax + aw - w }, y, if left { 0.0 } else { w }, centre - y)
        }
        Edge::Top => {
            let centre = ax + docked.along * aw;
            let x = within(centre - w / 2.0, ax, ax + aw - w).round();
            (x, ay, centre - x, 0.0)
        }
    };
    let origin = *monitor.position();
    Ok(Target {
        pos: PhysicalPosition::new(x as i32, y as i32),
        size,
        monitor: (origin.x, origin.y, monitor.size().width as i32, monitor.size().height as i32),
        reserve: None,
        layout: Layout::Docked { edge: docked.edge, x: px / dpi, y: py / dpi },
    })
}

/// `value` kept between `lo` and `hi`, or `lo` if the range is empty (a window taller than the
/// work area sits at its top).
fn within(value: f64, lo: f64, hi: f64) -> f64 {
    if hi < lo {
        lo
    } else {
        value.clamp(lo, hi)
    }
}

/// The bounds (physical px: x, y, width, height) of the monitor the notch is on.
pub fn monitor_bounds(win: &WebviewWindow) -> Result<(i32, i32, i32, i32), String> {
    target(win).map(|t| t.monitor).map_err(|e| e.to_string())
}

/// The union of every monitor: the whole desktop the compositor can show in a screenshot.
pub fn desktop_bounds(win: &WebviewWindow) -> Result<(i32, i32, i32, i32), String> {
    let monitors = win.available_monitors().map_err(|e| e.to_string())?;
    let left = monitors.iter().map(|m| m.position().x).min().ok_or("no monitors")?;
    let top = monitors.iter().map(|m| m.position().y).min().ok_or("no monitors")?;
    let right = monitors.iter().map(|m| m.position().x + m.size().width as i32).max().ok_or("no monitors")?;
    let bottom = monitors.iter().map(|m| m.position().y + m.size().height as i32).max().ok_or("no monitors")?;
    Ok((left, top, right - left, bottom - top))
}

/// Set while the notch is parked off-screen for a screenshot, so `place` leaves it there.
static PARKED_FOR_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Leaves the notch out of screen captures, or puts it back. Only while HoverPill takes its own
/// screenshot for ChatGPT: other screenshots and recordings show the notch as usual. Windows
/// hides the window from capture directly; on Linux the compositor has no such switch, and its
/// portal screenshot ignores both `_NET_WM_WINDOW_OPACITY` and `set_opacity`, so the window is
/// parked just past the edge of the desktop for the instant the shot is taken instead.
pub fn exclude_from_capture(win: &WebviewWindow, exclude: bool) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
        let hwnd = native_handle(win);
        // The window's own thread sets its affinity; wait for it so the capture comes after.
        let (done, wait) = std::sync::mpsc::channel();
        let _ = win.run_on_main_thread(move || {
            unsafe { SetWindowDisplayAffinity(hwnd as _, if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }) };
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_millis(500));
        // Give the compositor a frame or two to redraw without it.
        if exclude {
            std::thread::sleep(Duration::from_millis(80));
        }
    }
    #[cfg(target_os = "linux")]
    {
        if exclude {
            // Just past the right edge of every monitor, so it is off the composited screen
            // without losing focus or its place in the dock. `place` is held off meanwhile.
            if let Ok((dx, dy, dw, _dh)) = desktop_bounds(win) {
                PARKED_FOR_CAPTURE.store(true, Ordering::Relaxed);
                let _ = win.set_position(PhysicalPosition::new(dx + dw, dy));
                // Let the move be applied (and the notch leave the frame) before the shot.
                std::thread::sleep(Duration::from_millis(120));
            }
        } else {
            PARKED_FOR_CAPTURE.store(false, Ordering::Relaxed);
            let _ = place(win);
            std::thread::sleep(Duration::from_millis(80));
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (win, exclude);
    }
}

/// The native window handle as an integer (0 where there isn't a Win32 one).
fn native_handle(win: &WebviewWindow) -> isize {
    #[cfg(windows)]
    return win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    #[cfg(not(windows))]
    {
        let _ = win;
        0
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    name: String,
    label: String,
    primary: bool,
}

#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Result<Vec<MonitorInfo>, String> {
    let primary = app.primary_monitor().map_err(|e| e.to_string())?.and_then(|m| m.name().cloned());
    let monitors = app.available_monitors().map_err(|e| e.to_string())?;
    Ok(monitors
        .into_iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let name = m.name()?.clone();
            let size = m.size();
            let is_primary = primary.as_ref() == Some(&name);
            let label = format!(
                "Display {} — {}×{}{}",
                i + 1,
                size.width,
                size.height,
                if is_primary { " (primary)" } else { "" }
            );
            Some(MonitorInfo { name, label, primary: is_primary })
        })
        .collect())
}

/// Moves/resizes the window only if it isn't already where it should be, so this is cheap to
/// call periodically (monitors get plugged in, resolution and DPI scale change). Leaves it alone
/// while it's being dragged.
pub fn place(win: &WebviewWindow) -> Result<(), Box<dyn Error>> {
    if dragging(win) || PARKED_FOR_CAPTURE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let t = target(win)?;
    move_to(win, &t)?;
    announce(win, t.layout, None);
    Ok(())
}

fn move_to(win: &WebviewWindow, t: &Target) -> Result<(), Box<dyn Error>> {
    // Position first: moving onto a monitor with a different DPI rescales the window.
    if win.outer_position()? != t.pos {
        win.set_position(t.pos)?;
    }
    if win.inner_size()? != t.size {
        // Linux: lift the fixed min == max hints a non-resizable GTK window carries before
        // asking for a smaller size. Queued in this order, the event loop applies both.
        #[cfg(target_os = "linux")]
        win.set_resizable(true)?;
        win.set_size(t.size)?;
    }
    appbar::sync(native_handle(win), t.reserve);
    Ok(())
}

#[derive(Clone, Serialize)]
struct DockEvent {
    layout: Layout,
    /// After a drop: how far the window jumped, in CSS px (old origin minus new). The bubble
    /// moves back by this much first, so it slides from where it was let go instead of jumping.
    shift: Option<(f64, f64)>,
}

static LAST_LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);

/// Tells the webview how to lay out, when that changed or after a drop.
fn announce(win: &WebviewWindow, layout: Layout, shift: Option<(f64, f64)>) {
    let mut last = LAST_LAYOUT.lock().unwrap();
    if shift.is_none() && *last == Some(layout) {
        return;
    }
    *last = Some(layout);
    events::send(win.app_handle(), "pill:dock", DockEvent { layout, shift });
}

/// The layout right now, for a webview that's just loaded.
#[tauri::command]
pub fn dock_layout(app: AppHandle) -> Result<Layout, String> {
    let win = app.get_webview_window(LABEL).ok_or("no pill window")?;
    target(&win).map(|t| t.layout).map_err(|e| e.to_string())
}

// ---------- dragging ----------

/// Ctrl+drag in progress: where the pointer and the window were when it began.
#[derive(Default)]
pub struct DragState(Mutex<Option<Drag>>);

#[derive(Clone)]
struct Drag {
    cursor: PhysicalPosition<f64>,
    window: PhysicalPosition<i32>,
    /// Linux: the window's size and every monitor's work area (x, y, width, height), read once
    /// when the drag begins. GNOME keeps a window the app moves itself wholly inside a work area
    /// (it won't go under the top bar or past a screen edge), so the drag loop clamps the window
    /// the same way and moves the bubble inside it instead (see `follow_cursor`).
    #[cfg(target_os = "linux")]
    size: PhysicalSize<u32>,
    #[cfg(target_os = "linux")]
    work_areas: Vec<(i32, i32, i32, i32)>,
    #[cfg(target_os = "linux")]
    scale: f64,
}

fn dragging(win: &WebviewWindow) -> bool {
    win.try_state::<DragState>().is_some_and(|d| d.0.lock().unwrap().is_some())
}

/// The webview saw Ctrl+press on the notch and has turned it into a bubble under the pointer.
/// From here the hit-test loop moves the window with the pointer until the button comes up.
#[tauri::command]
pub fn begin_drag(app: AppHandle, state: State<DragState>) -> Result<(), String> {
    let win = app.get_webview_window(LABEL).ok_or("no pill window")?;
    let cursor = win.cursor_position().map_err(|e| e.to_string())?;
    let window = win.outer_position().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    let drag = {
        let work_areas = win
            .available_monitors()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|m| {
                let a = m.work_area();
                (a.position.x, a.position.y, a.size.width as i32, a.size.height as i32)
            })
            .collect();
        let size = win.outer_size().map_err(|e| e.to_string())?;
        let scale = win.scale_factor().map_err(|e| e.to_string())?;
        Drag { cursor, window, size, work_areas, scale }
    };
    #[cfg(not(target_os = "linux"))]
    let drag = Drag { cursor, window };
    *state.0.lock().unwrap() = Some(drag);
    // While it's on the move there's no notch at the top to keep windows clear of.
    appbar::sync(native_handle(&win), None);
    Ok(())
}

/// The webview saw the button come up. On Windows the hit-test loop also watches the button
/// itself, in case that event never arrives; whichever is first settles the drop.
#[tauri::command]
pub fn end_drag(app: AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        finish_drag(&win);
    }
}

/// Keeps the window where it was relative to the pointer when the drag began, so the bubble
/// stays under it.
///
/// On Linux the window can't always get there: GNOME keeps it inside the work area. So it's
/// clamped there up front, and however far the pointer has gone past that is sent to the webview
/// as `pill:drag-offset` (CSS px), which moves the bubble inside the window to stay under the
/// pointer. `last_offset` is what was sent last, so it's only sent when it changes.
fn follow_cursor(win: &WebviewWindow, drag: &Drag, last_offset: &mut (i32, i32)) {
    let Some(c) = pointer_position(win) else { return };
    let x = drag.window.x + (c.x - drag.cursor.x).round() as i32;
    let y = drag.window.y + (c.y - drag.cursor.y).round() as i32;
    #[cfg(target_os = "linux")]
    let (x, y) = {
        let (cx, cy) = (x, y);
        let (x, y) = clamp_to_work_area(&drag.work_areas, (c.x, c.y), (x, y), drag.size);
        let offset = (cx - x, cy - y);
        if offset != *last_offset {
            *last_offset = offset;
            let _ = win.emit("pill:drag-offset", (offset.0 as f64 / drag.scale, offset.1 as f64 / drag.scale));
        }
        (x, y)
    };
    #[cfg(not(target_os = "linux"))]
    let _ = last_offset;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// `pos` moved just enough to put a window of `size` wholly inside the work area of the monitor
/// under `cursor` (or the nearest one), the way GNOME would place it anyway.
#[cfg(target_os = "linux")]
fn clamp_to_work_area(
    areas: &[(i32, i32, i32, i32)],
    cursor: (f64, f64),
    pos: (i32, i32),
    size: PhysicalSize<u32>,
) -> (i32, i32) {
    let distance = |&&(ax, ay, aw, ah): &&(i32, i32, i32, i32)| {
        let dx = (ax as f64 - cursor.0).max(cursor.0 - (ax + aw) as f64).max(0.0);
        let dy = (ay as f64 - cursor.1).max(cursor.1 - (ay + ah) as f64).max(0.0);
        dx * dx + dy * dy
    };
    let Some(&(ax, ay, aw, ah)) = areas.iter().min_by(|a, b| distance(a).total_cmp(&distance(b))) else {
        return pos;
    };
    let (w, h) = (size.width as i32, size.height as i32);
    // `within` keeps a window bigger than the area at its top-left.
    let x = within(pos.0 as f64, ax as f64, (ax + aw - w) as f64) as i32;
    let y = within(pos.1 as f64, ay as f64, (ay + ah - h) as f64) as i32;
    (x, y)
}

/// The pointer's screen position. On Linux this asks the X root directly: `cursor_position()`
/// goes through the same X server, but a root query also tells us the button state and keeps the
/// drag loop independent of which window the pointer happens to be over.
fn pointer_position(win: &WebviewWindow) -> Option<PhysicalPosition<f64>> {
    #[cfg(target_os = "linux")]
    if let Some((x, y, _)) = x11::pointer() {
        return Some(PhysicalPosition::new(x, y));
    }
    win.cursor_position().ok()
}

fn finish_drag(win: &WebviewWindow) {
    if win.state::<DragState>().0.lock().unwrap().take().is_none() {
        return;
    }
    if let Err(e) = settle(win) {
        eprintln!("hoverpill: couldn't place the dropped pill: {e}");
        let _ = place(win);
    }
}

/// Docks the dropped bubble on the nearest edge (left, right or top) of the monitor under the
/// pointer, or makes it the notch again at a top centre, and saves that.
fn settle(win: &WebviewWindow) -> Result<(), Box<dyn Error>> {
    let cursor = win.cursor_position()?;
    let before = win.outer_position()?;
    let under = |m: &Monitor| {
        let (p, s) = (m.position(), m.size());
        cursor.x >= p.x as f64 && cursor.x < (p.x + s.width as i32) as f64 && cursor.y >= p.y as f64 && cursor.y < (p.y + s.height as i32) as f64
    };
    let monitor = match win.available_monitors()?.into_iter().find(under) {
        Some(m) => m,
        None => win.current_monitor()?.ok_or("no monitor under the pointer")?,
    };
    let area = monitor.work_area();
    let work = (area.position.x, area.position.y, area.size.width as i32, area.size.height as i32);
    let bounds = (monitor.position().x, monitor.position().y, monitor.size().width as i32, monitor.size().height as i32);
    let name = monitor.name().cloned();
    match dock::drop_target(work, bounds, (cursor.x, cursor.y), NOTCH_SNAP * monitor.scale_factor()) {
        None => {
            win.state::<DockStore>().set(None);
            // Dropped at another monitor's top centre: the notch moves there, as if chosen in Settings.
            let display = win.state::<SettingsStore>().get().display;
            let current = chosen_monitor(win, display.monitor.as_deref()).ok().and_then(|m| m.name().cloned());
            if let Some(name) = name.filter(|n| Some(n) != current.as_ref()) {
                settings::set_monitor(win.app_handle(), name);
            }
        }
        Some((edge, along)) => win.state::<DockStore>().set(Some(Docked { monitor: name, edge, along })),
    }
    let t = target(win)?;
    move_to(win, &t)?;
    let dpi = monitor.scale_factor();
    announce(win, t.layout, Some(((before.x - t.pos.x) as f64 / dpi, (before.y - t.pos.y) as f64 / dpi)));
    Ok(())
}

/// Cached so the 60 Hz loop makes one cross-thread call (cursor) instead of several.
struct Geometry {
    origin: PhysicalPosition<i32>,
    /// Only Linux needs the window's own size (see `bounds`).
    #[cfg(target_os = "linux")]
    size: PhysicalSize<u32>,
    scale: f64,
    monitor: (i32, i32, i32, i32),
    /// Docked somewhere other than the top centre.
    docked: bool,
}

impl Geometry {
    fn read(win: &WebviewWindow) -> Option<Self> {
        let t = target(win).ok()?;
        Some(Self {
            origin: win.inner_position().ok()?,
            #[cfg(target_os = "linux")]
            size: win.inner_size().ok()?,
            scale: win.scale_factor().ok()?,
            monitor: t.monitor,
            docked: matches!(t.layout, Layout::Docked { .. }),
        })
    }

    /// The window's own box on screen: x, y, width, height, in the same pixels as `origin`.
    /// Linux checks whether a fullscreen window reaches over this rather than over the whole
    /// monitor, so the test stays inside X11's coordinate space (see `x11::fullscreen_app_on`).
    #[cfg(target_os = "linux")]
    fn bounds(&self) -> (i32, i32, i32, i32) {
        (self.origin.x, self.origin.y, self.size.width as i32, self.size.height as i32)
    }
}

/// Where the cursor is relative to the notch.
#[derive(Default, Clone, Copy)]
struct Pointer {
    /// Over the notch's box.
    over: bool,
    /// Pushed against the top edge of the screen, horizontally within the notch. The notch may
    /// sit lower than the edge (a top gap), so this isn't implied by `over`.
    at_edge: bool,
    interactive: bool,
}

fn read_pointer(win: &WebviewWindow, geo: &Geometry) -> Pointer {
    let Some((r, interactive)) = *win.state::<HitState>().shape.lock().unwrap() else {
        return Pointer::default();
    };
    let Ok(cursor) = win.cursor_position() else {
        return Pointer { interactive, ..Pointer::default() };
    };
    let x = (cursor.x - geo.origin.x as f64) / geo.scale;
    let y = (cursor.y - geo.origin.y as f64) / geo.scale;
    let within_x = x >= r.x - HIT_PADDING && x <= r.x + r.width + HIT_PADDING;
    // The idle bar is narrow, so the top-edge zone is at least EDGE_HALF_WIDTH either side of
    // its centre: easy to hit without aiming.
    let centre = r.x + r.width / 2.0;
    let within_edge_zone = (x - centre).abs() <= (r.width / 2.0 + HIT_PADDING).max(EDGE_HALF_WIDTH);
    // Pushing against the pill's own top edge.
    let at_window_top = cursor.y - geo.origin.y as f64 <= EDGE_PX && cursor.y >= geo.origin.y as f64;
    // On Linux the window manager (e.g. GNOME) keeps the pill below the top bar, so its top edge
    // is lower than the screen's. The README promises opening by pushing against "the top edge of
    // the screen", so also fire from the monitor's own top edge there.
    #[cfg(target_os = "linux")]
    let at_edge = within_edge_zone
        && (at_window_top || (cursor.y - geo.monitor.1 as f64 <= EDGE_PX && cursor.y >= geo.monitor.1 as f64));
    #[cfg(not(target_os = "linux"))]
    let at_edge = within_edge_zone && at_window_top;
    // Docked elsewhere, it opens by resting on it instead (the top-edge zone is the notch's).
    let at_edge = at_edge && !geo.docked;
    Pointer {
        over: within_x && y >= r.y - HIT_PADDING && y <= r.y + r.height + HIT_PADDING,
        at_edge,
        interactive,
    }
}

/// Linux/XWayland only: shape the window's X *input* region to the notch's own box so the X
/// server actually delivers pointer motion to us there.
///
/// A fully click-through window receives no cursor updates, so under XWayland a real mouse can
/// never be seen near the top: `cursor_position()` just returns wherever the pointer last left an
/// X surface. Shaping the input region (XShape) to the notch's box makes that box a real X
/// surface for the pointer, so hovering it and pushing its top edge work with a real mouse, while
/// everywhere else in the window stays click-through. `None` sets an empty region (fully
/// click-through). GTK must be touched on the main thread, so this hops there; the rect is in
/// window coordinates (GDK scales to device pixels itself, matching `read_pointer`).
#[cfg(target_os = "linux")]
fn set_input_region(win: &WebviewWindow, rect: Option<Rect>) {
    let win = win.clone();
    let handle = win.clone();
    let _ = handle.run_on_main_thread(move || {
        use gtk::prelude::*;
        let Ok(gtk_win) = win.gtk_window() else { return };
        let Some(gdk_win) = gtk_win.window() else { return };
        if !HOVER_SIGNALS_CONNECTED.swap(true, Ordering::Relaxed) {
            // GTK's own crossing events are the authoritative hover signal on Linux: X delivers
            // Enter/LeaveNotify from real pointer motion at the input-region boundary, whereas the
            // cursor Tauri polls (`XQueryPointer`) freezes the moment the real pointer leaves it.
            let app = win.app_handle().clone();
            gtk_win.connect_enter_notify_event(move |_, _| {
                if let Some(s) = app.try_state::<HoverState>() {
                    s.0.store(true, Ordering::Relaxed);
                }
                gtk::glib::Propagation::Proceed
            });
            let app = win.app_handle().clone();
            gtk_win.connect_leave_notify_event(move |_, _| {
                if let Some(s) = app.try_state::<HoverState>() {
                    s.0.store(false, Ordering::Relaxed);
                }
                gtk::glib::Propagation::Proceed
            });
        }
        let region = match rect {
            Some(r) => gtk::gdk::cairo::Region::create_rectangle(&gtk::gdk::cairo::RectangleInt::new(
                r.x.round() as i32,
                r.y.round() as i32,
                r.width.round().max(1.0) as i32,
                r.height.round().max(1.0) as i32,
            )),
            None => gtk::gdk::cairo::Region::create(),
        };
        gdk_win.input_shape_combine_region(&region, 0, 0);
    });
}

#[cfg(target_os = "linux")]
static HOVER_SIGNALS_CONNECTED: AtomicBool = AtomicBool::new(false);

/// The all-workspaces state we last applied on Linux (-1 = not applied yet), so the hit-test loop
/// keeps re-asking until the GTK window exists (`stick` is a no-op before it's realized).
#[cfg(target_os = "linux")]
static STICKY_STATE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

/// Keeps the pill on every virtual desktop / workspace. Linux only: the window manager is asked to
/// make the window sticky (`_NET_WM_STATE_STICKY` via GDK). Windows has no documented way to do
/// this, so there the pill stays on the desktop it was opened on.
pub fn set_all_workspaces(win: &WebviewWindow, all: bool) {
    #[cfg(target_os = "linux")]
    {
        let win = win.clone();
        let handle = win.clone();
        let _ = handle.run_on_main_thread(move || {
            use gtk::prelude::*;
            // Nothing to do until the GTK window is realized; the loop calls us again until then.
            let Ok(gtk_win) = win.gtk_window() else { return };
            let Some(gdk_win) = gtk_win.window() else { return };
            if all {
                gdk_win.stick();
            } else {
                gdk_win.unstick();
            }
            STICKY_STATE.store(if all { 1 } else { 0 }, Ordering::Relaxed);
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (win, all);
    }
}

/// X11 (Linux/XWayland) helpers for spotting a fullscreen app. The window manager flags a
/// fullscreen window with `_NET_WM_STATE_FULLSCREEN` and lists managed windows in
/// `_NET_CLIENT_LIST`. libX11 is used directly (through `x11-dl`) because GDK only exposes
/// properties of our own window.
#[cfg(target_os = "linux")]
mod x11 {
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::os::raw::{c_long, c_void};

    use x11_dl::xlib::{Display, Window, Xlib};

    /// One Xlib connection, kept for the life of the hit-test thread. Xlib connections must not be
    /// shared between threads, and only that thread ever touches this one.
    struct Connection {
        xlib: Xlib,
        display: *mut Display,
    }

    impl Connection {
        fn open() -> Option<Self> {
            let xlib = Xlib::open().ok()?;
            // SAFETY: `XOpenDisplay` on a thread that keeps the connection to itself.
            let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            (!display.is_null()).then_some(Self { xlib, display })
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            // SAFETY: `display` came from `XOpenDisplay` on this thread and is still open.
            unsafe { (self.xlib.XCloseDisplay)(self.display) };
        }
    }

    thread_local! {
        static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
    }

    /// Reads a property as its 32-bit values: both `WINDOW` and `ATOM` properties are stored as
    /// `long`s. Empty if the property is missing or an unexpected type.
    fn read_property(conn: &Connection, window: Window, name: &str) -> Vec<u64> {
        let Ok(name) = CString::new(name) else { return Vec::new() };
        // SAFETY: pointers below are either null or owned by Xlib; the buffer is freed with XFree.
        unsafe {
            let atom = (conn.xlib.XInternAtom)(conn.display, name.as_ptr(), 0);
            if atom == 0 {
                return Vec::new();
            }
            let mut actual_type = 0u64;
            let mut format = 0i32;
            let mut count = 0u64;
            let mut after = 0u64;
            let mut data: *mut u8 = std::ptr::null_mut();
            let status = (conn.xlib.XGetWindowProperty)(
                conn.display, window, atom, 0, 1024, 0, 0,
                &mut actual_type, &mut format, &mut count, &mut after, &mut data,
            );
            if status != 0 || data.is_null() {
                return Vec::new();
            }
            let mut values = Vec::new();
            if format == 32 {
                let longs = data as *const c_long;
                for i in 0..count as usize {
                    values.push(*longs.add(i) as u64);
                }
            }
            (conn.xlib.XFree)(data as *mut c_void);
            values
        }
    }

    fn intern(conn: &Connection, name: &str) -> u64 {
        let Ok(name) = CString::new(name) else { return 0 };
        // SAFETY: `conn.display` is our open connection.
        unsafe { (conn.xlib.XInternAtom)(conn.display, name.as_ptr(), 0) }
    }

    /// Whether `window`'s frame reaches over `region` (x, y, width, height) on the X root.
    fn covers(conn: &Connection, window: Window, (rx, ry, rw, rh): (i32, i32, i32, i32)) -> bool {
        // SAFETY: all pointers are to locals or `conn.display`; Xlib only fills them in.
        unsafe {
            let mut root = 0u64;
            let mut x = 0i32;
            let mut y = 0i32;
            let mut w = 0u32;
            let mut h = 0u32;
            let mut border = 0u32;
            let mut depth = 0u32;
            let status = (conn.xlib.XGetGeometry)(
                conn.display, window, &mut root, &mut x, &mut y, &mut w, &mut h, &mut border, &mut depth,
            );
            if status == 0 {
                return false;
            }
            // `x`/`y` are relative to the parent, which a reparenting WM makes a frame window.
            let mut ox = 0i32;
            let mut oy = 0i32;
            let mut child = 0u64;
            (conn.xlib.XTranslateCoordinates)(conn.display, window, root, 0, 0, &mut ox, &mut oy, &mut child);
            let (w, h) = (w as i32, h as i32);
            ox <= rx && oy <= ry && ox + w >= rx + rw && oy + h >= ry + rh
        }
    }

    /// The pointer on the X root, and whether the primary button is down, as XWayland last saw
    /// them. Used while dragging by the hit-test thread, which owns this connection. X only
    /// learns about the pointer while it is over an X surface, so the drag loop also widens the
    /// pill's input region to its whole window (see the drag branch in `spawn_hit_test_loop`).
    pub fn pointer() -> Option<(f64, f64, bool)> {
        CONNECTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Connection::open();
            }
            let conn = slot.as_ref()?;
            // SAFETY: pointers are to locals or `conn.display`; Xlib only fills them in.
            unsafe {
                let root = (conn.xlib.XDefaultRootWindow)(conn.display);
                let (mut root_return, mut child_return) = (0u64, 0u64);
                let (mut root_x, mut root_y) = (0i32, 0i32);
                let (mut win_x, mut win_y) = (0i32, 0i32);
                let mut mask = 0u32;
                let ok = (conn.xlib.XQueryPointer)(
                    conn.display, root, &mut root_return, &mut child_return, &mut root_x, &mut root_y, &mut win_x, &mut win_y, &mut mask,
                );
                (ok != 0).then_some((root_x as f64, root_y as f64, mask & x11_dl::xlib::Button1Mask != 0))
            }
        })
    }

    /// `true` when a window marked fullscreen covers `pill`, the pill window's own box on the X
    /// root. Reading just `_NET_ACTIVE_WINDOW` isn't enough: some window managers (Ubuntu's split shell,
    /// for one) leave it pointing at a dead helper window while an app is fullscreen, and the
    /// fullscreen app need not be the focused one. So walk `_NET_CLIENT_LIST` and accept any
    /// window that is flagged fullscreen, isn't hidden on another workspace, and reaches over the
    /// pill. Comparing against the pill's box rather than the monitor keeps the test inside X11's
    /// own coordinates, so DPI scaling can't make it miss.
    pub fn fullscreen_app_on(pill: (i32, i32, i32, i32)) -> bool {
        CONNECTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Connection::open();
            }
            let Some(conn) = slot.as_ref() else { return false };
            let fullscreen = intern(conn, "_NET_WM_STATE_FULLSCREEN");
            if fullscreen == 0 {
                return false;
            }
            let hidden = intern(conn, "_NET_WM_STATE_HIDDEN");
            let root = unsafe { (conn.xlib.XDefaultRootWindow)(conn.display) };
            let mut windows = read_property(conn, root, "_NET_CLIENT_LIST");
            if windows.is_empty() {
                windows = read_property(conn, root, "_NET_CLIENT_LIST_STACKING");
            }
            // Fall back to the active window for managers that don't publish a client list.
            if let Some(active) = read_property(conn, root, "_NET_ACTIVE_WINDOW").first() {
                if !windows.contains(active) {
                    windows.push(*active);
                }
            }
            windows.into_iter().any(|window| {
                let window = window as Window;
                let state = read_property(conn, window, "_NET_WM_STATE");
                state.contains(&fullscreen) && !state.contains(&hidden) && covers(conn, window, pill)
            })
        })
    }
}

/// Whether the foreground window covers the whole monitor, i.e. a fullscreen video, game or
/// presentation (or a browser in F11). The desktop and the taskbar don't count, and neither
/// does a maximised window: with an auto-hiding taskbar those cover the whole monitor too, but
/// they're ordinary windows (IsZoomed), not fullscreen apps.
#[cfg(windows)]
fn fullscreen_app_on(win: &WebviewWindow, geo: &Geometry) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow, GetWindowRect, IsZoomed};
    let (mx, my, mw, mh) = geo.monitor;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() || fg as isize == native_handle(win) || IsZoomed(fg) != 0 {
            return false;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        if matches!(String::from_utf16_lossy(&class[..len]).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return false;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(fg, &mut r) != 0 && r.left <= mx && r.top <= my && r.right >= mx + mw && r.bottom >= my + mh
    }
}

#[cfg(target_os = "linux")]
fn fullscreen_app_on(win: &WebviewWindow, geo: &Geometry) -> bool {
    // Native Wayland apps first (the companion extension), then plain X11
    // clients, which are what the X11 module can see.
    shellext::fullscreen_on(monitor_index(win)) || x11::fullscreen_app_on(geo.bounds())
}

/// Index of the pill's monitor in GNOME's monitor list — the shell extension
/// identifies monitors by index, and GDK under XWayland enumerates the same
/// outputs in the same order as the window manager. `usize::MAX` when the
/// monitor can't be matched, to which the extension answers "no".
#[cfg(target_os = "linux")]
fn monitor_index(win: &WebviewWindow) -> usize {
    let Ok(monitors) = win.available_monitors() else { return usize::MAX };
    // `target()` picks the very monitor the notch is on, so match by its name.
    let wanted = win
        .try_state::<SettingsStore>()
        .map(|store| store.get().display.monitor)
        .unwrap_or_default();
    let Ok(chosen) = chosen_monitor(win, wanted.as_deref()) else { return usize::MAX };
    let Some(name) = chosen.name() else { return usize::MAX };
    let name = name.clone();
    monitors.iter().position(|monitor| monitor.name().is_some_and(|other| other == &name)).unwrap_or(usize::MAX)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn fullscreen_app_on(_win: &WebviewWindow, _geo: &Geometry) -> bool {
    false
}

pub fn spawn_hit_test_loop(win: WebviewWindow) {
    std::thread::spawn(move || {
        let mut geo = Geometry::read(&win);
        let mut open = false;
        #[cfg(not(target_os = "linux"))]
        let mut accepting_input = false;
        let mut resting_since: Option<Instant> = None;
        let mut display = Display::default();
        let mut fullscreen = false;
        let mut peek = false;
        let mut on_card = false;
        let mut focus = FocusGuard::default();
        // Ctrl is held over the closed notch, so it takes the click (see `pill:grab`).
        let mut grab = false;
        // Just dropped under the pointer: don't open until the pointer has moved off it.
        let mut wait_for_leave = false;
        let mut was_dragging = false;
        // How far the bubble has been moved inside the window during this drag (see `follow_cursor`).
        let mut drag_offset = (0, 0);
        // Linux: when the current drag started, so a stale button reading can't end it
        // immediately (XWayland can lose the pointerup while the window moves under it).
        #[cfg(target_os = "linux")]
        let mut drag_since: Option<Instant> = None;
        // Linux: the whole window has been made input-opaque for the duration of a drag.
        #[cfg(target_os = "linux")]
        let mut drag_region_full = false;
        // Typing (Windows): when it started, and whether the notch has been the foreground window
        // since. See `typing_over`.
        #[cfg(windows)]
        let mut typing_since: Option<(Instant, bool)> = None;
        // Linux: follow the reported notch box as the X input region; start fully click-through.
        #[cfg(target_os = "linux")]
        let mut last_region: Option<Rect> = None;
        #[cfg(target_os = "linux")]
        set_input_region(&win, None);
        #[cfg(not(target_os = "linux"))]
        let _ = win.set_ignore_cursor_events(true);
        let mut ticks: u64 = 0;
        loop {
            // Dragging: the window follows the pointer and nothing else happens meanwhile.
            let drag = win.state::<DragState>().0.lock().unwrap().clone();
            if let Some(drag) = drag {
                #[cfg(target_os = "linux")]
                let started = *drag_since.get_or_insert_with(Instant::now);
                std::thread::sleep(DRAG_POLL);
                if open {
                    open = false;
                    let _ = win.emit("pill:hover", false);
                }
                // XWayland stops updating the X pointer while it is over a native Wayland
                // window, so keep the whole (window-sized) pill under the pointer while it
                // moves: the bubble is small, and the old notch-sized input region was easy to
                // leave behind, which stranded the drag with no pointerup.
                #[cfg(target_os = "linux")]
                if !drag_region_full {
                    drag_region_full = true;
                    set_input_region(&win, Some(Rect { x: 0.0, y: 0.0, width: 100_000.0, height: 100_000.0 }));
                }
                follow_cursor(&win, &drag, &mut drag_offset);
                #[cfg(not(target_os = "linux"))]
                if !accepting_input {
                    accepting_input = true;
                    let _ = win.set_ignore_cursor_events(false);
                }
                // The webview's pointerup can go missing while the window moves under it; the
                // button itself has the final word. Give X a moment to see the press first.
                #[cfg(windows)]
                if !primary_button_down() {
                    finish_drag(&win);
                }
                #[cfg(target_os = "linux")]
                if started.elapsed() > Duration::from_millis(150) && !primary_button_down() {
                    finish_drag(&win);
                }
                was_dragging = true;
                continue;
            }
            #[cfg(target_os = "linux")]
            {
                drag_since = None;
            }
            // Dropped (here or by the webview's `end_drag`): it has moved, and it's under the pointer.
            if was_dragging {
                was_dragging = false;
                drag_offset = (0, 0);
                geo = Geometry::read(&win);
                wait_for_leave = true;
                // Back to shaping only the notch, and let the next tick re-apply the reported box.
                #[cfg(target_os = "linux")]
                {
                    drag_region_full = false;
                    last_region = None;
                }
            }

            let tick = ticks;
            ticks = ticks.wrapping_add(1);
            std::thread::sleep(POLL);

            if tick.is_multiple_of(SLOW_TICK_EVERY) {
                if let Err(e) = place(&win) {
                    eprintln!("hoverpill: placement failed: {e}");
                }
                reassert_topmost(&win);
                geo = Geometry::read(&win);
                if let Some(store) = win.try_state::<SettingsStore>() {
                    display = store.get().display;
                }
                // Linux: apply once the GTK window is realized, and re-apply on every change. The
                // startup call in `setup` runs before the window exists, so this is the real one.
                #[cfg(target_os = "linux")]
                if STICKY_STATE.load(Ordering::Relaxed) != i8::from(display.all_workspaces) {
                    set_all_workspaces(&win, display.all_workspaces);
                }
            }

            if tick.is_multiple_of(FULLSCREEN_CHECK_EVERY) {
                let now = display.hide_in_fullscreen && geo.as_ref().is_some_and(|g| fullscreen_app_on(&win, g));
                if now != fullscreen {
                    fullscreen = now;
                    let _ = win.emit("pill:fullscreen", fullscreen);
                }
            }

            let p = geo.as_ref().map(|g| read_pointer(&win, g)).unwrap_or_default();
            // Linux: the window's GTK enter/leave signal is authoritative (see `HoverState`); the
            // polled cursor sticks at the input region's edge once the real pointer leaves it.
            #[cfg(target_os = "linux")]
            let p = {
                let mut p = p;
                p.over = win.state::<HoverState>().0.load(Ordering::Relaxed);
                p
            };
            let docked = geo.as_ref().is_some_and(|g| g.docked);
            let ctrl = ctrl_down();
            if !p.over {
                wait_for_leave = false;
            }
            // Ctrl over the closed notch: it takes the click, so Ctrl+drag picks it up
            // instead of Ctrl+clicking whatever is underneath.
            let want_grab = ctrl && p.over && !p.interactive && !fullscreen;
            if want_grab != grab {
                grab = want_grab;
                let _ = win.emit("pill:grab", grab);
            }
            if !p.over || p.interactive || ctrl || wait_for_leave {
                resting_since = None;
            } else if resting_since.is_none() {
                resting_since = Some(Instant::now());
            }
            let rested = resting_since.is_some_and(|t| t.elapsed() >= if docked { DOCKED_DWELL } else { DWELL });

            // Interactive (open, or a card with buttons): stays open while the pointer is over it.
            // Closed: only the top-edge push or a long rest opens it; it never takes clicks.
            // Behind a fullscreen app, nothing opens it (video players put their controls at the
            // top edge too).
            // Under XWayland the pill owns its own input region (see `set_input_region`), so a real
            // cursor over the closed pill is a genuine hover: open on it directly. On Windows the
            // pill stays click-through when closed, so hovering it can't be detected and the
            // top-edge push / dwell are the only triggers.
            #[cfg(target_os = "linux")]
            let hover_open = p.over && !wait_for_leave;
            #[cfg(not(target_os = "linux"))]
            let hover_open = false;

            #[allow(unused_mut)]
            let mut typing = win.state::<HitState>().typing.load(Ordering::Relaxed);
            #[cfg(windows)]
            if typing_over(&win, typing, &mut typing_since) {
                typing = false;
                win.state::<HitState>().typing.store(false, Ordering::Relaxed);
                let _ = win.set_focusable(false);
                let _ = win.emit("pill:typing", false);
            }
            let pinned = win.state::<HitState>().pinned.load(Ordering::Relaxed);
            // On the notch itself rather than pushing against the top edge: reaching for a card's
            // buttons. Sent before `pill:hover`, so the webview knows why it's opening.
            let want_on_card = p.over && !p.at_edge;
            if want_on_card != on_card {
                on_card = want_on_card;
                let _ = win.emit("pill:on-card", on_card);
            }
            let want_open = if typing || pinned {
                true
            } else if p.interactive {
                p.over || p.at_edge
            } else if fullscreen {
                false
            } else if docked {
                // Off the top centre it's out of the way, so resting on it briefly opens it.
                rested || hover_open
            } else {
                p.at_edge || hover_open || (display.open_on_hover && rested)
            };
            if want_open != open {
                open = want_open;
                if open {
                    focus.remember(&win);
                }
                let _ = win.emit("pill:hover", open);
            }

            // Pointing at the closed notch fades it out so whatever is underneath shows.
            let want_peek = p.over && !p.interactive && !open && !docked && !grab;
            if want_peek != peek {
                peek = want_peek;
                let _ = win.emit("pill:peek", peek);
            }

            // Linux: keep the X input region on the reported notch box so a real cursor is seen
            // over the pill (and its clicks land on us) while everything else stays click-through.
            // While a fullscreen app covers the monitor the pill is hidden and must not intercept
            // anything, so hand the whole input region back — unless a card is up that needs clicks.
            #[cfg(target_os = "linux")]
            {
                let rect = win.state::<HitState>().shape.lock().unwrap().map(|(r, _)| r);
                let target = if fullscreen && !p.interactive { None } else { rect };
                if target != last_region || tick.is_multiple_of(SLOW_TICK_EVERY) {
                    last_region = target;
                    set_input_region(&win, target);
                }
            }
            // Windows: the cursor is always readable, so a plain all-or-nothing toggle is fine.
            #[cfg(not(target_os = "linux"))]
            {
                // Docked off the top centre it takes the pointer even closed: it's small and out of
                // the way there, and pointing at it never clicks through to what's underneath.
                let want_input = (p.over && (p.interactive || (docked && !fullscreen))) || grab;
                if want_input != accepting_input {
                    accepting_input = want_input;
                    let _ = win.set_ignore_cursor_events(!want_input);
                }
            }
            if !typing {
                focus.give_back_if_taken(&win);
            }
        }
    });
}

/// The pill must never keep keyboard focus: clicking Approve shouldn't pull you out of your
/// terminal. `"focusable": false` (WS_EX_NOACTIVATE on Windows, no input hint on X11) prevents
/// most activation, but WebView2 lives in a separate process and its child windows can still
/// activate us when clicked. So remember who had the foreground when the cursor arrived, and
/// hand it straight back if we take it.
#[derive(Default)]
struct FocusGuard {
    /// HWND as an integer so the guard can live on the polling thread.
    #[cfg(windows)]
    previous: Option<isize>,
}

#[cfg(windows)]
impl FocusGuard {
    fn remember(&mut self, win: &WebviewWindow) {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let Ok(ours) = win.hwnd() else { return };
        let foreground = unsafe { GetForegroundWindow() };
        if !foreground.is_null() && !std::ptr::eq(foreground, ours.0) {
            self.previous = Some(foreground as isize);
        }
    }

    fn give_back_if_taken(&mut self, win: &WebviewWindow) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow};
        let Ok(ours) = win.hwnd() else { return };
        if !std::ptr::eq(unsafe { GetForegroundWindow() }, ours.0) {
            return;
        }
        if let Some(previous) = self.previous {
            // We're the foreground window right now, so Windows lets us pass it on.
            unsafe {
                if IsWindow(previous as _) != 0 {
                    SetForegroundWindow(previous as _);
                }
            }
        }
    }
}

#[cfg(not(windows))]
impl FocusGuard {
    fn remember(&mut self, _win: &WebviewWindow) {}
    fn give_back_if_taken(&mut self, _win: &WebviewWindow) {}
}

/// Whether typing in the notch is over (Windows): another app came to the front, or the notch
/// never managed to get the keyboard. Decided here rather than by the webview's focus events,
/// because those fire for focus moving around *inside* the notch too: letting go of Alt after
/// the ChatGPT shortcut, for one, moves focus to the window's menu handling, and the notch shut
/// the moment it opened.
#[cfg(windows)]
fn typing_over(win: &WebviewWindow, typing: bool, since: &mut Option<(Instant, bool)>) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    if !typing {
        *since = None;
        return false;
    }
    let (started, had_focus) = since.get_or_insert((Instant::now(), false));
    let front = unsafe { GetForegroundWindow() } as isize;
    if front == native_handle(win) {
        *had_focus = true;
        return false;
    }
    // Moved on to another app, or the keyboard never came (Windows can refuse it).
    let over = *had_focus || started.elapsed() > Duration::from_millis(1500);
    if over {
        *since = None;
    }
    over && front != 0
}

/// Whether Ctrl is held. Windows only: on Linux the notch's own input region already takes the
/// click, and the webview sees Ctrl on it.
#[cfg(windows)]
fn ctrl_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    unsafe { GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0 }
}

#[cfg(not(windows))]
fn ctrl_down() -> bool {
    false
}

/// Whether the primary mouse button is held. `GetAsyncKeyState` reads the physical buttons, so
/// with the buttons swapped in Mouse settings the primary one is the right-hand button.
#[cfg(windows)]
fn primary_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    let button = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
    unsafe { GetAsyncKeyState(button as i32) as u16 & 0x8000 != 0 }
}

/// Linux: the X server's button mask. Only meaningful while the pointer is over an X surface;
/// the drag loop keeps the pill's whole window input-opaque for exactly that reason.
#[cfg(target_os = "linux")]
fn primary_button_down() -> bool {
    x11::pointer().is_some_and(|(_, _, down)| down)
}

/// Other "topmost" windows (borderless-fullscreen video players, the taskbar after it's clicked)
/// can end up above us; pushing ourselves back to the top of the topmost band fixes that.
/// Exclusive-fullscreen games bypass the desktop compositor, and nothing can draw over them.
#[cfg(windows)]
fn reassert_topmost(win: &WebviewWindow) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    };
    if let Ok(hwnd) = win.hwnd() {
        unsafe {
            SetWindowPos(
                hwnd.0 as _,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}

#[cfg(not(windows))]
fn reassert_topmost(win: &WebviewWindow) {
    let _ = win.set_always_on_top(true);
}

/// Native Wayland doesn't let a client position itself, stay above other windows, or read the
/// global cursor position, so run under XWayland until a wlr-layer-shell backend exists
/// (see docs/ARCHITECTURE.md). Must run before GTK initialises.
#[cfg(target_os = "linux")]
pub fn linux_preflight() {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    // 1920×1200 with GNOME's 29 px top bar, and a second screen to its right.
    const AREAS: [(i32, i32, i32, i32); 2] = [(0, 29, 1920, 1171), (1920, 0, 2560, 1440)];
    const SIZE: PhysicalSize<u32> = PhysicalSize { width: 720, height: 340 };

    #[test]
    fn a_dragged_window_stays_inside_the_work_area() {
        // Left edge and under the top bar: held at the work area's corner.
        assert_eq!(clamp_to_work_area(&AREAS, (20.0, 10.0), (-340, -10), SIZE), (0, 29));
        // Bottom of the first screen.
        assert_eq!(clamp_to_work_area(&AREAS, (900.0, 1190.0), (540, 1170), SIZE), (540, 860));
        // Well inside: left alone.
        assert_eq!(clamp_to_work_area(&AREAS, (900.0, 600.0), (540, 580), SIZE), (540, 580));
    }

    #[test]
    fn the_monitor_under_the_pointer_decides() {
        // Pointer on the second screen: its work area, which has no top bar.
        assert_eq!(clamp_to_work_area(&AREAS, (2000.0, 5.0), (1640, -15), SIZE), (1920, 0));
    }
}
