<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/hoverpill-wordmark-dark.png">
  <img src="assets/brand/hoverpill-wordmark-light.png" alt="HoverPill" width="390">
</picture>

HoverPill is a smart notch for Windows and Linux, styled after the iPhone's Dynamic Island. A black pill floats at the top-center of your primary monitor. It springs open when you hover over it, when Claude Code needs permission, when a notification arrives, or when media is playing. Everywhere else, clicks pass straight through to the windows underneath.

[![CI](https://github.com/PreetinderSinghBadesha/HoverPill/actions/workflows/ci.yml/badge.svg)](https://github.com/PreetinderSinghBadesha/HoverPill/actions/workflows/ci.yml)

Built with Tauri 2 (Rust), React, and Motion. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and the roadmap.

## Install

Download the latest release from [GitHub Releases](https://github.com/PreetinderSinghBadesha/HoverPill/releases/latest). Each release lists what's new, and earlier versions are on the [releases page](https://github.com/PreetinderSinghBadesha/HoverPill/releases).

**Windows:** run `HoverPill_<version>_x64-setup.exe`. It isn't code-signed yet, so SmartScreen may stop it: choose **More info**, then **Run anyway**.

**Linux:** each release also has a `.deb` (Debian, Ubuntu), an `.rpm` (Fedora, RHEL, openSUSE) and an AppImage (any distribution). Install the `.deb` or `.rpm` with your package manager, or make the AppImage executable and run it:

```bash
chmod +x HoverPill_<version>_amd64.AppImage
./HoverPill_<version>_amd64.AppImage
```

On a Wayland session HoverPill runs through XWayland (`GDK_BACKEND=x11`), because a normal Wayland client can't position itself at the top edge or read the pointer. Native Wayland is on the roadmap.

### Build the installer yourself

```bash
npm run tauri build
```

On Windows this produces `src-tauri/target/release/bundle/nsis/HoverPill_<version>_x64-setup.exe`, a 3 MB per-user installer. It needs no admin rights, adds HoverPill to the Start menu, and puts an uninstaller under Windows Settings › Apps.

On Linux it produces a `.deb`, an `.rpm` and an AppImage under `src-tauri/target/release/bundle/`. The release build uses about 40 MB of memory.

While the installed HoverPill is running, `npm run tauri dev` can't start a second copy. Only one HoverPill runs at a time, so the dev build just opens the installed copy's Settings and exits. Quit the installed one from its tray icon first.

## Run from source

Prerequisites:

- Rust (stable) and Node 20+.
- **Windows:** WebView2, which ships with Windows 11.
- **Linux:** `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libgtk-3-dev`, `libx11-dev`, `libdbus-1-dev`, and a compositing window manager.

```bash
npm install
npm run tauri dev
```

**Using the notch.** It sits flat against the top edge of the screen, like a laptop's camera notch.

- **When it's closed, clicks go straight through it** to whatever is underneath, so browser tabs and title bars stay usable.
  - **On Linux** this is different: to let a real mouse hover the notch under XWayland, the closed notch keeps the pointer over its own box, so hovering it opens it and a click on it lands on the notch instead of the window below. Everywhere else on the window still passes clicks through. On Windows a closed notch always passes clicks through.
- **To open it,** push the pointer against the top edge of the screen above the notch, or press **Win + Alt + Space** from any app (Esc closes it and gives the keyboard back; pick other keys, or none, in Settings › Display).
- **Staying out of the way:**
  - Pointing at the closed notch fades it out so you can see what's underneath.
  - With nothing happening, it shrinks to a thin bar.
  - It hides while a fullscreen app (video, game, presentation, F11 browser) covers its screen.
    - **On Linux** apps running through X11/XWayland are watched directly. For apps talking to the compositor directly (native Wayland — Firefox on Ubuntu does this by default, `MOZ_ENABLE_WAYLAND=0` forces it into X11 mode), turn on the GNOME Shell helper in Settings › Display: it installs a small extension and asks you to log out once, after which fullscreen Wayland apps hide the notch on GNOME too. It is off by default, switching it off removes the extension again, and a helper you disable yourself in GNOME's Extensions app stays disabled. (KDE/wlroots remain X11-only.)
  - "Keep windows below the notch" in Settings reserves a strip at the top, like a taskbar, so maximised windows start below it. Windows only.
  - If you prefer opening the notch by resting the pointer on it for about half a second, turn that on in Settings. It replaces the fade.
- **Moving it:** hold **Ctrl** and drag the notch (on Linux you can also press and hold it still for a moment, then drag: under Wayland the pill can't see Ctrl while another app has the keyboard). While you hold it, it's a bubble; let go and it sticks flat to the nearest left, right or top edge of that screen (never the bottom, where the taskbar is), and stays there after a restart. On a side edge it stands upright. Rest the pointer on it to open the full notch from that edge. To put it back, drag it to the middle of a screen's top edge.
- **Inside the open notch,** tabs switch between:
  - **Home:** what's playing, the latest notification, and a calendar
  - **Media:** large cover art and controls
  - **Timer:** a countdown (presets from 1 minute to 1 hour, ±1 minute) and a stopwatch with laps. While either runs, the closed notch shows the time. When a timer ends, a "Time's up" card drops down with a chime and +1 min / Done buttons, even over fullscreen apps.
  - **Notifications:** everything received this session
  - **Claude:** your recent permission decisions
  - **ChatGPT:** ask a question and the answer streams in, using your ChatGPT plan (sign in under Settings › Accounts). The screen button next to the question box sends a screenshot of the notch's screen (without the notch) with your next question, so ChatGPT can see what you're doing and tell you what to click or type; on Linux it uses the desktop's Screenshot portal, so it works under Wayland too. The lock button at the top keeps the notch open while you read or work alongside it, until you unlock it. **Ctrl + Alt + G** opens this tab from any app, ready to type; Esc or a click elsewhere closes it. Pick other keys, or turn the shortcut off, in Settings › Accounts.

  The gear button opens Settings.

The pill has no taskbar entry. Use the **tray icon** to open **Settings…**, to quit, to turn on **Start at login**, to **Check for updates**, or to fire demo events (a Claude permission request or a notification). Launching HoverPill again while it's running also opens Settings.

**Updates.** HoverPill checks for a new release now and then (turn that off in Settings › General), downloads it in the background, and offers it in the notch: "HoverPill 0.5.0 is ready. Restart to update". Updates are signed, and one that doesn't match HoverPill's key is refused. On Linux the AppImage updates itself; a `.deb` or `.rpm` install is pointed at the download page instead.

**Settings** has four tabs, and changes apply immediately:

| Tab | What's there |
|---|---|
| General | Start at login, version |
| Display | Which monitor, distance from the top edge (0 = attached notch, more = floating pill), size (80–140%), open on rest, shrink when idle, hide during fullscreen, keep windows below the notch, show on every workspace (Linux) |
| Notifications | On/off, how long each card stays (2–15 s), per-app on/off: apps that recently sent notifications, plus every other app Windows lets notify |
| Media | On/off, per-player on/off (e.g. hide a browser so the pill shows Spotify) |

Settings are stored in `settings.json` in the app's config folder (`%APPDATA%\dev.hoverpill.app` on Windows, `~/.config/dev.hoverpill.app` on Linux).

**Start at login:**

- How it's stored: a per-user Run entry in the registry on Windows, an entry in `~/.config/autostart` on Linux.
- Release builds only. Run `npm run tauri build` and enable it from the installed or built app. Debug builds refuse to turn it on, because they need the dev server and would show a blank pill at login.
- Only one copy of HoverPill can run at a time. Launching it again shows "already running" in the pill.

Media works without any setup. Whatever is playing (Spotify, a browser tab, VLC, ...) appears in the pill, and hovering shows the controls. If a player doesn't show up, see what the backend detects:

```bash
cargo run --manifest-path src-tauri/Cargo.toml -p hoverpill-media --example now_playing 30
```

Notifications from other apps (Discord, WhatsApp, Gmail in your browser, ...) drop the pill down for a few seconds. Windows may ask once for notification access.

- **Windows:** to stop the normal Windows banners from appearing as well, open Settings › System › Notifications, pick each app, and turn off **Show notification banners**. Keep the app's notifications themselves on: HoverPill reads them from Notification Center.
- **Linux:** turn on your desktop's Do Not Disturb.

**Gmail and other websites** notify through your browser, so Windows reports them as the browser (e.g. Brave), and they're listed under it. The notification API doesn't expose which site sent them. To give Gmail its own entry, install it as an app from the browser's menu.

To see what the listener picks up (and which apps Windows lets notify):

```bash
cargo run --manifest-path src-tauri/Cargo.toml -p hoverpill-notify --example listen 60
```

### Connect Claude Code

1. With HoverPill running, open the tray menu and choose **Install Claude Code hook**.
   This adds an `http` `PermissionRequest` hook to `~/.claude/settings.json`. The rest of the file is left alone, and the previous version is saved as `settings.json.hoverpill-backup`.
2. Start a new Claude Code session. When Claude needs permission, the pill drops down with the command. Click **Approve** or **Deny**, or **Answer in terminal** to get Claude's normal prompt.

If HoverPill isn't running, Claude Code shows its usual terminal prompt. To undo the setup, choose **Remove Claude Code hook** from the tray menu.

To work on the UI in a normal browser, run `npm run dev` and open http://localhost:1420. Hover the pill, or press `1` (permission), `2` (notification), or `3` (media).

## Releasing

1. Bump the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`, and commit.
2. Tag and push: `git tag v0.3.0 && git push origin v0.3.0`.
3. The [Release workflow](.github/workflows/release.yml) builds the Windows installer and the Linux `.deb`, `.rpm` and AppImage, and attaches them all to one draft release for that tag. Add the notes on GitHub, then publish it.

The workflow refuses a tag that doesn't match the version in `tauri.conf.json`. It signs the installers for the in-app updater with the `TAURI_SIGNING_PRIVATE_KEY` repository secret (the matching public key is in `tauri.conf.json`) and attaches a `latest.json` the updater reads. Google Calendar signs in with HoverPill's own OAuth client, built in at compile time from the `HOVERPILL_GOOGLE_CLIENT_ID` and `HOVERPILL_GOOGLE_CLIENT_SECRET` repository secrets; a build without them can't connect Google Calendar.

[CI](.github/workflows/ci.yml) type-checks and builds the interface, then runs clippy with warnings as errors and the tests on Windows **and** Linux, on every push and pull request. Dependabot opens weekly pull requests for npm, Cargo and Actions updates.

## Layout

```
src/                     React UI
  pill/Pill.tsx          spring-animated notch, view switching, hit-rect reporting
  pill/views.tsx         closed states, notification/permission cards, the tabbed open notch
  pill/icons.tsx         stroke icon set
  lib/useArtworkColor.ts accent colour sampled from the album art
  lib/usePillState.ts    event-driven state and view priority
  lib/bridge.ts          Tauri events and commands (no-ops in a plain browser)
src-tauri/src/
  notch.rs               placement, always-on-top, click-through / X input region
  broker/mod.rs          Claude Code permission broker (local HTTP server, pending decisions)
  broker/summary.rs      tool_input → readable card text
  broker/install.rs      add/remove the hook in ~/.claude/settings.json
  crates/media/          now playing + transport controls (GSMTC on Windows, MPRIS on Linux)
  crates/notify/         other apps' notifications (UserNotificationListener / D-Bus monitor)
  events.rs              shared payloads and commands
  tray.rs                tray menu: start at login, hook install, demo events, quit
  autostart.rs           start-at-login toggle (tauri-plugin-autostart)
  settings.rs            settings model, storage, Settings window, lists for the UI
src/settings/            the tabbed Settings window (settings.html)
```

## Status

| | |
|---|---|
| ✅ Done | Floating window, selective click-through, spring animations, all views, Claude permission broker, media controller, notification stream, never steals focus, start at login, single instance, tabbed Settings window, Linux (XWayland) support with `.deb` and AppImage builds |
| 🧪 Untested | Everything under a native Wayland session |
| 🔜 Next | Wayland layer-shell (see the architecture doc) |
