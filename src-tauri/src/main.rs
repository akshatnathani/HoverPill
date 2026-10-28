// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod access;
mod accounts;
mod appbar;
mod autostart;
mod broker;
mod dock;
mod events;
mod notch;
mod screen;
mod settings;
#[cfg(target_os = "linux")]
mod shellext;
mod shortcut;
mod system;
mod tray;
mod updater;

use tauri::Manager;

use settings::{RecentApps, SettingsStore};

fn main() {
    #[cfg(target_os = "linux")]
    notch::linux_preflight();

    // If this turns out to be a second copy, it will hand over to the running one, which then
    // opens Settings. Windows only lets the process the user just launched take the foreground,
    // so pass that permission on; otherwise Settings opens behind everything else.
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
        AllowSetForegroundWindow(ASFW_ANY);
    }

    tauri::Builder::default()
        // Must be first: a second copy (e.g. launched by hand after starting at login) exits
        // here instead of opening another pill and fighting over the broker port. Launching
        // HoverPill again is a natural way to look for its settings, so open them.
        // A second copy is either the user launching HoverPill again (look for its settings) or
        // one of the GNOME keybindings we install (which run `hoverpill --summon=...`).
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !shortcut::summon_from_args(app, &args) {
                settings::open_window(app);
            }
        }))
        .plugin(autostart::plugin())
        .plugin(shortcut::plugin())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(notch::HitState::default())
        .manage(notch::HoverState::default())
        .manage(notch::DragState::default())
        .manage(screen::Screens::default())
        .manage(updater::Pending::default())
        .manage(updater::Latest::default())
        .manage(events::Outbox::default())
        .invoke_handler(tauri::generate_handler![
            notch::set_hit_rect,
            notch::list_monitors,
            events::resolve_permission,
            events::media_command,
            events::current_media,
            events::pill_ready,
            events::copy_text,
            screen::capture_screen,
            tray::claude_hook_installed,
            tray::set_claude_hook,
            updater::check_for_updates,
            updater::pending_update,
            updater::install_update,
            settings::get_settings,
            settings::open_settings,
            settings::set_settings,
            settings::recent_notification_apps,
            settings::notifying_apps,
            settings::media_players,
            autostart::get_autostart,
            autostart::set_autostart,
            notch::notch_typing,
            notch::notch_pinned,
            notch::begin_drag,
            notch::end_drag,
            notch::dock_layout,
            accounts::accounts_status,
            accounts::open_link,
            accounts::google::google_connect,
            accounts::google::google_disconnect,
            accounts::google::calendar_events,
            accounts::google::calendar_refresh,
            accounts::chatgpt::chatgpt_connect,
            accounts::chatgpt::chatgpt_disconnect,
            accounts::chatgpt::chatgpt_models,
            accounts::chatgpt::chatgpt_ask,
            accounts::chatgpt::chatgpt_stop,
            accounts::chatgpt::chatgpt_open_usage,
        ])
        .setup(|app| {
            // Placement, media and notifications all read settings, so load them first.
            app.manage(SettingsStore::load(app.handle())?);
            app.manage(RecentApps::load(app.handle())?);
            app.manage(dock::DockStore::load(app.handle())?);
            let vault = accounts::Vault::load(app.handle())?;
            app.manage(accounts::google::Google::load(&vault));
            app.manage(accounts::chatgpt::ChatGpt::load(&vault));
            app.manage(vault);

            let pill = app
                .get_webview_window(notch::LABEL)
                .ok_or("pill window missing from tauri.conf.json")?;
            // Linux: a non-resizable GTK window gets fixed min == max size hints, and GTK then
            // clamps every `set_size` to the width it was created with (720), even though the
            // docked side layout only needs 420. The extra width made the notch stop short of
            // the screen edge when docked right. Tao re-applies the config's `resizable: false`
            // on the window's first configure event, so lift it from a handler connected after
            // Tao's one (which runs later for the same event). With decorations off, the user
            // still can't resize it by hand; this only lets the placement code shrink it.
            #[cfg(target_os = "linux")]
            if let Ok(gtk_win) = pill.gtk_window() {
                use gtk::prelude::*;
                gtk_win.connect_configure_event(|gtk_win, _| {
                    if !gtk_win.is_resizable() {
                        gtk_win.set_resizable(true);
                    }
                    false
                });
            }
            notch::place(&pill)?;
            // Apply the saved all-desktops choice at startup; `settings::apply` only runs it when
            // the setting changes. On Linux this is a no-op until the GTK window is realized, so
            // the hit-test loop re-applies it (see `STICKY_STATE` in notch.rs).
            let all_workspaces = app.state::<SettingsStore>().get().display.all_workspaces;
            notch::set_all_workspaces(&pill, all_workspaces);
            // Show first: on Linux the GTK window has no gdk surface until it's realized, and
            // set_ignore_cursor_events would panic on the missing surface. Windows is fine either
            // way.
            pill.show()?;
            pill.set_ignore_cursor_events(true)?;
            // Apply the saved Wayland-helper choice: the GNOME extension that
            // reports fullscreen state for native Wayland apps exists only
            // while the user has it switched on; see `shellext.rs`.
            // The hit-test loop falls back to X11 when it isn't installed.
            #[cfg(target_os = "linux")]
            shellext::sync(app.handle(), app.state::<SettingsStore>().get().display.gnome_helper);
            notch::spawn_hit_test_loop(pill);

            app.manage(broker::Broker::load(app.handle())?);
            broker::start(app.handle().clone());

            let handle = app.handle().clone();
            let media = hoverpill_media::Media::spawn(move |now| {
                let enabled = handle.state::<SettingsStore>().get().media.enabled;
                events::send(&handle, "pill:media", now.filter(|_| enabled).map(events::MediaState::from));
            });
            media.set_ignored(app.state::<SettingsStore>().get().media.ignored_players);
            app.manage(media);

            let handle = app.handle().clone();
            hoverpill_notify::spawn(move |event| match event {
                hoverpill_notify::Event::Notification(n) => {
                    handle.state::<RecentApps>().record([n.app.as_str()]);
                    let settings = handle.state::<SettingsStore>().get();
                    if settings.notifications.enabled && !settings.is_app_blocked(&n.app) {
                        events::send(&handle, "pill:notification", events::NotificationItem::from(n));
                    }
                }
                // Already waiting when we started: not shown, but those apps belong in the list.
                hoverpill_notify::Event::Seen(apps) => handle.state::<RecentApps>().record(apps.iter().map(String::as_str)),
                hoverpill_notify::Event::Unavailable(reason) => events::notify(&handle, "Notifications unavailable", &reason),
            });

            let settings = app.state::<SettingsStore>().get();
            shortcut::apply(app.handle(), &settings.display.shortcut, &settings.chatgpt.shortcut);
            // Started by a shortcut while HoverPill wasn't running: the keybinding launched us
            // with `--summon=...`, and the notification listener and webview are now set up.
            let args: Vec<String> = std::env::args().skip(1).collect();
            shortcut::summon_from_args(app.handle(), &args);
            accounts::google::spawn(app.handle().clone());
            updater::spawn(app.handle().clone());
            access::spawn_worker(app.handle().clone());
            system::spawn_worker(app.handle().clone());
            #[cfg(target_os = "linux")]
            exit_on_signal(app.handle().clone());
            autostart::refresh(app.handle());
            tray::init(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building HoverPill")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Give the reserved top strip back, or Windows keeps it until it notices we're gone.
                appbar::release();
                // And the GNOME shortcuts, so none stay behind after HoverPill is gone.
                shortcut::remove_desktop_shortcuts();
            }
        });
}

/// Linux: logging out (or `kill`) sends SIGTERM, which would end the process without Tauri's
/// `Exit` event, and so without removing the GNOME shortcuts. Turn SIGTERM and SIGINT into a
/// normal exit instead. The handler only sets a flag; a thread notices it and exits.
#[cfg(target_os = "linux")]
fn exit_on_signal(app: tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let _ = std::thread::Builder::new().name("hoverpill-signals".into()).spawn(move || loop {
        if STOP.load(Ordering::SeqCst) {
            app.exit(0);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    });
}
