// Prevents console window from appearing on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod canvas_feed;
mod commands;
mod hotkey;
mod notifications;
mod process;
mod tray;
mod tts_text;

use audio::{AudioState, WakeListenerState};
use commands::{audio_cmd, desktop_cmd, server_cmd, window_cmd};
use process::ServerProcess;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tauri_plugin_window_state::StateFlags;
use tokio::time::Instant;

#[cfg(target_os = "macos")]
fn build_macos_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    // App menu (the menu titled with the app name on macOS)
    let app_menu = Submenu::with_items(
        app,
        "Goose In A Pond",
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    // Edit menu — required for WebView clipboard (Cut/Copy/Paste) to work on macOS
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    // View menu — app-specific shortcuts
    let toggle_canvas = MenuItem::with_id(
        app,
        "toggle-canvas",
        "Toggle Canvas Overlay",
        true,
        Some("CmdOrCtrl+Shift+G"),
    )?;
    let voice_mode = MenuItem::with_id(
        app,
        "voice-mode",
        "Switch to Voice Mode",
        true,
        Some("CmdOrCtrl+Shift+V"),
    )?;
    let view_menu = Submenu::with_items(app, "View", true, &[&toggle_canvas, &voice_mode])?;

    Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu])
}

const HEALTH_CHECK_INTERVAL_SECS: u64 = 10;

#[derive(Serialize, Clone)]
struct RecoveryStatusPayload {
    active: bool,
    attempt: u32,
    next_retry_secs: u64,
    last_error: Option<String>,
}

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        // ── Plugins ─────────────────────────────────────────────────────────
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::POSITION | StateFlags::SIZE)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_shell::init())
        // ── Managed state ────────────────────────────────────────────────────
        .manage(ServerProcess::new())
        .manage(AudioState::new())
        .manage(WakeListenerState::new())
        .manage(hotkey::HotkeyState::new())
        // ── Commands ─────────────────────────────────────────────────────────
        .invoke_handler(tauri::generate_handler![
            server_cmd::get_server_url,
            server_cmd::set_server_url,
            server_cmd::server_health,
            server_cmd::ensure_server_running,
            window_cmd::show_canvas,
            window_cmd::hide_canvas,
            window_cmd::toggle_canvas,
            window_cmd::canvas_visible,
            window_cmd::position_canvas,
            audio_cmd::start_recording,
            audio_cmd::stop_recording,
            audio_cmd::abort_recording,
            audio_cmd::record_with_vad,
            audio_cmd::play_ping,
            audio_cmd::run_voice_pipeline,
            audio_cmd::start_wake_listener,
            audio_cmd::stop_wake_listener,
            desktop_cmd::enable_autostart,
            desktop_cmd::disable_autostart,
            desktop_cmd::is_autostart_enabled,
            desktop_cmd::set_hotkey,
            desktop_cmd::open_privacy_mic,
        ])
        // ── App lifecycle ─────────────────────────────────────────────────────
        .setup(|app| {
            let handle = app.handle().clone();

            // ── Main window (created here so we can attach an initialization script)
            // The init script runs before any page JS, ensuring window.__GIAP_SERVER_URL__
            // is available when api.ts executes at module load time.
            let default_url = "http://127.0.0.1:4000";
            let init_script = format!(r#"window.__GIAP_SERVER_URL__ = "{default_url}";"#);
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Goose In A Pond")
            .inner_size(1280.0, 860.0)
            .min_inner_size(900.0, 600.0)
            .resizable(true)
            .decorations(true)
            .center()
            .initialization_script(&init_script)
            .build()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

            // macOS native menu bar (App + Edit + View)
            #[cfg(target_os = "macos")]
            match build_macos_menu(&handle) {
                Ok(menu) => {
                    if let Err(e) = app.set_menu(menu) {
                        tracing::warn!("Failed to set macOS menu bar: {e}");
                    }
                }
                Err(e) => tracing::warn!("Failed to build macOS menu: {e}"),
            }

            // Build system tray
            tray::build_tray(&handle)?;

            // Connect to (or spawn) pond-server in background
            let handle_server = handle.clone();
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let resource_dir_health = resource_dir.clone();

            tauri::async_runtime::spawn(async move {
                let server = handle_server.state::<ServerProcess>();
                match server.ensure_running(&resource_dir).await {
                    Ok(url) => {
                        tracing::info!("pond-server ready at {url}");
                        tray::set_tray_tooltip(&handle_server, "Connected");
                        inject_server_url(&handle_server, &url);

                        // CRITICAL: emit `server-status: true` here so the
                        // frontend AppContext fires its handshake immediately.
                        // Previously this emit only happened in the periodic
                        // health-check loop (every HEALTH_CHECK_INTERVAL_SECS),
                        // which is why the very first launch came up blank
                        // until the periodic tick eventually fired — the user
                        // had to "close and reopen" to see anything.
                        let _ = handle_server.emit("server-status", true);

                        // Start notification poller now that we have a live server URL
                        let handle_notif = handle_server.clone();
                        let notif_url = url.clone();
                        tauri::async_runtime::spawn(async move {
                            notifications::run(handle_notif, notif_url).await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("pond-server unavailable: {e}");
                        tray::set_tray_tooltip(&handle_server, "Disconnected");
                        let _ = handle_server.emit("server-offline", &e);
                        let _ = handle_server.emit(
                            "server-recovery",
                            RecoveryStatusPayload {
                                active: true,
                                attempt: 1,
                                next_retry_secs: HEALTH_CHECK_INTERVAL_SECS,
                                last_error: Some(e),
                            },
                        );
                    }
                }
            });

            // Register global canvas hotkey
            if let Err(e) = hotkey::register_canvas_hotkey(&handle) {
                tracing::warn!("Failed to register hotkey: {e}");
            }
            if let Err(e) = hotkey::register_summon_hotkey(&handle) {
                tracing::warn!("Failed to register summon hotkey: {e}");
            }

            // Periodic health check every 10s
            let handle_health = handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut consecutive_recovery_failures: u32 = 0;
                let mut next_recovery_attempt = Instant::now();
                let mut last_recovery_error: Option<String> = None;

                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(HEALTH_CHECK_INTERVAL_SECS)).await;
                    let server = handle_health.state::<ServerProcess>();
                    let url = server.get_url();
                    if server.health_check(&url).await {
                        consecutive_recovery_failures = 0;
                        next_recovery_attempt = Instant::now();
                        last_recovery_error = None;
                        tray::set_tray_tooltip(&handle_health, "Connected");
                        let _ = handle_health.emit("server-status", true);
                        let _ = handle_health.emit(
                            "server-recovery",
                            RecoveryStatusPayload {
                                active: false,
                                attempt: 0,
                                next_retry_secs: 0,
                                last_error: None,
                            },
                        );
                        continue;
                    }

                    let now = Instant::now();
                    if now < next_recovery_attempt {
                        let retry_in = next_recovery_attempt
                            .saturating_duration_since(now)
                            .as_secs();

                        tray::set_tray_tooltip(&handle_health, "Disconnected");
                        let _ = handle_health.emit("server-status", false);
                        let _ = handle_health.emit(
                            "server-recovery",
                            RecoveryStatusPayload {
                                active: true,
                                attempt: consecutive_recovery_failures,
                                next_retry_secs: retry_in,
                                last_error: last_recovery_error.clone(),
                            },
                        );
                        continue;
                    }

                    tracing::warn!("pond-server health check failed, attempting recovery");
                    match server.ensure_running(&resource_dir_health).await {
                        Ok(recovered_url) => {
                            consecutive_recovery_failures = 0;
                            next_recovery_attempt = Instant::now();
                            last_recovery_error = None;
                            tray::set_tray_tooltip(&handle_health, "Connected");
                            inject_server_url(&handle_health, &recovered_url);
                            let _ = handle_health.emit("server-status", true);
                            let _ = handle_health.emit(
                                "server-recovery",
                                RecoveryStatusPayload {
                                    active: false,
                                    attempt: 0,
                                    next_retry_secs: 0,
                                    last_error: None,
                                },
                            );
                        }
                        Err(e) => {
                            consecutive_recovery_failures = consecutive_recovery_failures.saturating_add(1);
                            let backoff_secs = calculate_recovery_backoff_secs(consecutive_recovery_failures);
                            let error_message = e.clone();
                            next_recovery_attempt = Instant::now()
                                + tokio::time::Duration::from_secs(backoff_secs);
                            last_recovery_error = Some(error_message.clone());

                            tray::set_tray_tooltip(&handle_health, "Disconnected");
                            let _ = handle_health.emit("server-status", false);
                            let _ = handle_health.emit("server-offline", &e);
                            let _ = handle_health.emit(
                                "server-recovery",
                                RecoveryStatusPayload {
                                    active: true,
                                    attempt: consecutive_recovery_failures,
                                    next_retry_secs: backoff_secs,
                                    last_error: Some(error_message),
                                },
                            );
                            tracing::warn!(
                                "pond-server recovery failed (attempt {}), next retry in {}s",
                                consecutive_recovery_failures,
                                backoff_secs
                            );
                        }
                    }
                }
            });

            Ok(())
        })
        // Handle macOS View menu items — emit events dispatched by AppContext
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle-canvas" => { let _ = app.emit("canvas-toggle", ()); }
            "voice-mode"    => { let _ = app.emit("switch-to-voice", ()); }
            _ => {}
        })
        // Keep app alive in tray when main window is closed
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Failed to build Goose In A Pond desktop app")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit) {
                let server = app.state::<ServerProcess>();
                server.shutdown();
            }
        });
}

/// Inject the server URL as `window.__GIAP_SERVER_URL__` so that api.ts
/// can construct the correct base URL regardless of what hostname/port
/// pond-server is running on.
fn inject_server_url(app: &tauri::AppHandle, url: &str) {
    if let Some(win) = app.get_webview_window("main") {
        let script = format!(
            r#"window.__GIAP_SERVER_URL__ = "{}"; console.log("[GIAP] Server:", window.__GIAP_SERVER_URL__);"#,
            url
        );
        let _ = win.eval(&script);
    }
}

fn calculate_recovery_backoff_secs(consecutive_failures: u32) -> u64 {
    const BASE_SECONDS: u64 = 5;
    const MAX_SECONDS: u64 = 300;

    if consecutive_failures == 0 {
        return 0;
    }

    let shift = (consecutive_failures - 1).min(6);
    let multiplier = 1u64 << shift;
    (BASE_SECONDS.saturating_mul(multiplier)).min(MAX_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::calculate_recovery_backoff_secs;

    #[test]
    fn recovery_backoff_starts_at_five_seconds() {
        assert_eq!(calculate_recovery_backoff_secs(1), 5);
    }

    #[test]
    fn recovery_backoff_doubles_until_cap() {
        assert_eq!(calculate_recovery_backoff_secs(2), 10);
        assert_eq!(calculate_recovery_backoff_secs(3), 20);
        assert_eq!(calculate_recovery_backoff_secs(4), 40);
        assert_eq!(calculate_recovery_backoff_secs(5), 80);
        assert_eq!(calculate_recovery_backoff_secs(6), 160);
        assert_eq!(calculate_recovery_backoff_secs(7), 300);
        assert_eq!(calculate_recovery_backoff_secs(8), 300);
    }
}
