//! Cooldown for the desktop: a tray app that polls the adapters, keeps readings in SQLite, rings
//! when a limit resets and syncs paired phones. The window shows the shared `@cooldown/ui`.
//!
//! - [`backend`]: the engine (no window code; tested on its own).
//! - [`commands`]: what the UI can call.
//! - [`adapters`], [`hook`], [`phone`], [`secrets`], [`settings`], [`views`]: the pieces the engine uses.
//!
//! Closing the window hides it: the tray icon and the alert timer keep running. Quit from the tray.

#![forbid(unsafe_code)]

pub mod adapters;
pub mod backend;
pub mod commands;
pub mod hook;
pub mod phone;
pub mod secrets;
pub mod settings;
pub mod views;

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};
use tauri_plugin_notification::NotificationExt;

use crate::backend::Backend;

/// The event the UI listens for to refetch.
pub const CHANGED_EVENT: &str = "cooldown://changed";

/// Sent when the window is closed to the tray, so the Pairing screen drops its code.
pub const WINDOW_HIDDEN_EVENT: &str = "cooldown://window-hidden";

/// Passed by launch-at-login so the app starts in the tray without a window.
const HIDDEN_ARG: &str = "--hidden";

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Cooldown", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Check limits now", true, None::<&str>)?;
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start at login",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit Cooldown", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &refresh, &autostart, &separator, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Cooldown")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "refresh" => {
                if let Some(backend) = app.try_state::<Arc<Backend>>() {
                    backend.restart_polling();
                }
            }
            "autostart" => {
                let launcher = app.autolaunch();
                let enabled = launcher.is_enabled().unwrap_or(false);
                let _ = if enabled {
                    launcher.disable()
                } else {
                    launcher.enable()
                };
                let _ = autostart.set_checked(launcher.is_enabled().unwrap_or(false));
            }
            "quit" => {
                if let Some(backend) = app.try_state::<Arc<Backend>>() {
                    backend.phone().shutdown();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // A panic in a release build would otherwise leave no trace at all.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_hook(info);
    }));

    let handle = app.handle().clone();
    // The local data folder (Windows: %LOCALAPPDATA%\app.cooldown.desktop), not the roaming one
    // (%APPDATA%): a managed Windows setup can sync the roaming profile to other PCs, and with it
    // the paired-phone list and settings. Both resolve to the same folder on macOS and Linux.
    let data_dir = app.path().app_local_data_dir()?;
    log::info!(
        "Cooldown {} starting, data in {}",
        env!("CARGO_PKG_VERSION"),
        data_dir.display()
    );

    let notifier_handle = handle.clone();
    let changed_handle = handle.clone();
    // Keys are pinned to this machine (see `secrets`). If the store cannot be opened, Windows
    // refuses to save keys instead of falling back to a store that can roam; other platforms fall
    // back to the keyring wrapper. Either way a warning goes to the log.
    let secrets = secrets::open_secret_store();
    let backend = Backend::open(
        &data_dir,
        secrets,
        Box::new(move |title, body| {
            if let Err(e) = notifier_handle
                .notification()
                .builder()
                .title(title)
                .body(body)
                .show()
            {
                log::warn!("could not show the notification {title:?}: {e}");
            }
        }),
        Box::new(move || {
            let _ = changed_handle.emit(CHANGED_EVENT, ());
        }),
    )?;
    app.manage(backend.clone());

    // Launch at login, switched on once in release builds. A user who turns it off stays off.
    if !cfg!(debug_assertions) && !backend.settings().get().autostart_initialized {
        if let Err(e) = app.autolaunch().enable() {
            log::warn!("could not switch on launch at login: {e}");
        }
        let _ = backend
            .settings()
            .update(|s| s.autostart_initialized = true);
    }

    build_tray(&handle)?;

    tauri::async_runtime::spawn(backend.clone().run_polling());
    tauri::async_runtime::spawn(backend.clone().run_alert_timer());
    // Also starts the phone link when phones are paired (its first tick is immediate) and keeps
    // retrying it if starting fails or a later network change shuts it down.
    tauri::async_runtime::spawn(backend.clone().run_network_watch());

    if !std::env::args().any(|arg| arg == HIDDEN_ARG) {
        show_main(&handle);
    }
    Ok(())
}

/// Start the app.
pub fn run() {
    tauri::Builder::default()
        // Must be first: a second launch just brings the running window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        // Release builds on Windows have no console, so warnings go to a file as well:
        // %LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log (the app log dir elsewhere).
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir {
                        file_name: Some("cooldown".into()),
                    }),
                ])
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(2))
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .plugin(tauri_plugin_notification::init())
        .setup(|app| setup(app).inspect_err(|e| log::error!("startup failed: {e}")))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Keep running in the tray so alerts still ring.
                api.prevent_close();
                let _ = window.hide();
                // The pairing code is a one-time secret: it must not wait on a page nobody can
                // see. Close the pairing window and tell the page to drop the code.
                let app = window.app_handle();
                if let Some(backend) = app.try_state::<Arc<Backend>>() {
                    backend.phone().cancel_pairing();
                }
                let _ = app.emit(WINDOW_HIDDEN_EVENT, ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshots,
            commands::get_history,
            commands::get_adapters,
            commands::save_api_key,
            commands::remove_api_key,
            commands::get_hook_status,
            commands::install_hook,
            commands::uninstall_hook,
            commands::get_paired_devices,
            commands::revoke_device,
            commands::create_pairing_payload,
            commands::get_pairing_state,
            commands::confirm_pairing,
            commands::cancel_pairing,
            commands::refresh,
            commands::get_alerts,
            commands::set_alert,
            commands::remove_alert,
            commands::set_source_enabled,
            commands::get_agent_hook_status,
            commands::install_agent_hook,
            commands::uninstall_agent_hook,
            commands::get_copilot_allowance,
            commands::set_copilot_allowance,
            commands::get_notifications,
            commands::set_notifications,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cooldown");
}
