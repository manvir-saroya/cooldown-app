//! Cooldown for Android: the phone side. Pairs with the PC over the LAN (QR + Noise), keeps the
//! latest synced limits, rings bells with exact alarms and feeds the home-screen widget. The UI is
//! the shared `@cooldown/ui` in phone mode (`src/PhoneSnapshotSource.ts`).
//!
//! - [`phone`]: pairing, sync, reconnect, bells (no Android code).
//! - [`identity`]: the phone key at rest, wrapped by the Android Keystore.
//! - [`widget`]: what the home-screen widget is given, and what a choice of windows may look like.
//! - [`background`] and [`jni_entry`]: one sync round with the app closed, called by the Kotlin
//!   WorkManager job through JNI.
//! - Kotlin `NativePlugin` (in `gen/android`): Keystore wrapping, alarms (with boot restore),
//!   widget and its settings, background job schedule, QR scanning, permissions, Wi-Fi multicast
//!   lock. Reached through [`native_call`].
//!
//! The crate also builds on desktop (CI runs on Windows); there the native pieces report that they
//! need Android and the key is stored unwrapped (development only).

// Denied, not forbidden, for one reason: `jni_entry` exports a JNI function and needs `no_mangle`.
#![deny(unsafe_code)]

pub mod background;
pub mod identity;
pub mod jni_entry;
pub mod notices;
pub mod phone;
pub mod widget;

use std::sync::Arc;
use std::time::Duration;

use cooldown_core::{parse_alert_id, reset_title, AlertIntent, Snapshot};
use cooldown_sync::CloseReason;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, watch};
use zeroize::Zeroizing;

use crate::background::{APP_WAITS_FOR_ROUND, REGISTRY};
use crate::identity::KeyWrapper;
use crate::notices::PhoneNotifications;
use crate::phone::{PairStart, PairedPc, Phone, PHONE_NAME};

/// Emitted whenever anything the UI shows may have changed, including every new sync. The UI
/// refetches; the data itself is not sent along.
pub const CHANGED_EVENT: &str = "cooldown://changed";

/// How often a disconnected phone tries to reach its PC while the app runs.
const RECONNECT_EVERY: Duration = Duration::from_secs(60);

/// How often the link state is checked for the UI.
const LINK_CHECK: Duration = Duration::from_secs(5);

/// The phone, once it has started (key unwrapped, stores open).
struct Ready(watch::Receiver<Option<Arc<Phone>>>);

type ReadyState<'a> = State<'a, Ready>;

/// Wait (briefly) for the phone to finish starting.
async fn phone(ready: &Ready) -> Result<Arc<Phone>, String> {
    let mut rx = ready.0.clone();
    let waited = tokio::time::timeout(Duration::from_secs(15), rx.wait_for(Option::is_some)).await;
    match waited {
        Ok(Ok(phone)) => phone
            .clone()
            .ok_or_else(|| "Cooldown is still starting.".to_owned()),
        _ => Err("Cooldown is still starting. Try again in a moment.".into()),
    }
}

// ------------------------------------------------------------------ native bridge

/// The Kotlin side (`app.cooldown.phone.NativePlugin`), registered on Android only.
#[cfg(target_os = "android")]
struct Native(tauri::plugin::PluginHandle<tauri::Wry>);

fn native_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("cooldown-native")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle = _api.register_android_plugin("app.cooldown.phone", "NativePlugin")?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}

/// Call a `@Command` on the Kotlin plugin. Blocking: use [`native`] from async code.
fn native_call(_app: &AppHandle, _command: &str, _args: Value) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        let native = _app
            .try_state::<Native>()
            .ok_or("The native plugin is not loaded.")?;
        native
            .0
            .run_mobile_plugin::<Value>(_command, _args)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("This needs Android.".into())
    }
}

/// [`native_call`] from async code, off the async workers.
async fn native(app: &AppHandle, command: &'static str, args: Value) -> Result<Value, String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || native_call(&app, command, args))
        .await
        .map_err(|e| e.to_string())?
}

/// Keystore wrapping through the Kotlin plugin.
struct KeystoreWrapper(AppHandle);

#[derive(Deserialize)]
struct BlobReply {
    blob: String,
}

#[derive(Deserialize)]
struct RawReply {
    raw: Zeroizing<String>,
}

impl KeyWrapper for KeystoreWrapper {
    fn wrap(&self, raw: &[u8; 32]) -> Result<String, String> {
        let encoded = identity::encode_raw(raw);
        let reply = native_call(&self.0, "wrapKey", json!({ "raw": encoded.as_str() }))?;
        serde_json::from_value::<BlobReply>(reply)
            .map(|r| r.blob)
            .map_err(|_| "The keystore gave an unexpected answer.".into())
    }

    fn unwrap(&self, blob: &str) -> Result<Zeroizing<[u8; 32]>, String> {
        let reply = native_call(&self.0, "unwrapKey", json!({ "blob": blob.trim() }))?;
        let reply: RawReply = serde_json::from_value(reply)
            .map_err(|_| "The keystore gave an unexpected answer.".to_owned())?;
        identity::decode_raw(&reply.raw)
    }
}

// ------------------------------------------------------------------ bells and widget

/// The alarms the phone should hold: every enabled bell still in the future.
pub fn alarm_list(alerts: &[AlertIntent], now_ms: i64) -> Vec<Value> {
    alerts
        .iter()
        .filter(|a| a.enabled)
        .filter_map(|a| {
            let at_ms = (a.fire_at.unix_timestamp_nanos() / 1_000_000) as i64;
            (at_ms > now_ms).then(|| {
                let title = match parse_alert_id(&a.id) {
                    Some(p) => reset_title(&p.provider, p.window),
                    None => "Your limit is back".to_owned(),
                };
                json!({
                    "id": a.id,
                    "atMs": at_ms,
                    "title": title,
                    "body": "Your usage limit has reset. You can work again.",
                })
            })
        })
        .collect()
}

pub(crate) fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Hand the newest data to the alarms and the widget. Failures are logged, not fatal. With no PC
/// paired the alarms are cleared and the widget goes back to asking for a pairing.
async fn apply_to_device(app: &AppHandle, phone: &Phone) {
    let latest = phone.latest();
    let alarms = phone.alarms(now_ms());
    if let Err(e) = native(app, "syncAlarms", json!({ "alarms": alarms })).await {
        eprintln!("cooldown: alarms not updated: {e}");
    }
    let widget = match &latest {
        Some(sync) if !phone.pcs_by_recency().is_empty() => widget::widget_json(
            &sync.payload.snapshots,
            &sync.payload.alerts,
            Some(sync.received_at_ms),
        ),
        _ => String::new(),
    };
    if let Err(e) = native(app, "writeWidgetData", json!({ "json": widget })).await {
        eprintln!("cooldown: widget not updated: {e}");
    }
}

/// Keep the 15-minute background sync scheduled while a PC is paired and cancel it otherwise.
async fn schedule_background_sync(app: &AppHandle, phone: &Phone) {
    let enabled = !phone.pcs_by_recency().is_empty();
    if let Err(e) = native(app, "scheduleSync", json!({ "enabled": enabled })).await {
        eprintln!("cooldown: background sync not scheduled: {e}");
    }
}

// ------------------------------------------------------------------ connecting

/// Reach a paired PC, the most recently seen first: each one's last address, then a discovery
/// probe for all of them.
async fn connect(app: &AppHandle, phone: &Phone) -> Result<(), String> {
    if phone.pcs_by_recency().is_empty() {
        return Err("No PC is paired yet.".into());
    }
    if phone.connect_cached().await {
        return Ok(());
    }
    let _ = native(app, "multicastLock", json!({ "hold": true })).await;
    let found = phone.connect_discovered().await;
    let _ = native(app, "multicastLock", json!({ "hold": false })).await;
    if found {
        Ok(())
    } else {
        Err("Your PC was not found on this network. Is Cooldown running on it?".into())
    }
}

// ------------------------------------------------------------------ commands

#[tauri::command]
async fn get_snapshots(ready: ReadyState<'_>) -> Result<Vec<Snapshot>, String> {
    Ok(phone(&ready).await?.snapshots())
}

#[tauri::command]
async fn get_alerts(ready: ReadyState<'_>) -> Result<Vec<AlertIntent>, String> {
    Ok(phone(&ready).await?.alerts())
}

#[tauri::command]
async fn set_alert(ready: ReadyState<'_>, id: String, enabled: bool) -> Result<(), String> {
    phone(&ready).await?.set_alert(&id, enabled)
}

#[tauri::command]
async fn get_notifications(ready: ReadyState<'_>) -> Result<PhoneNotifications, String> {
    Ok(phone(&ready).await?.notices().settings())
}

#[tauri::command]
async fn set_notifications(
    ready: ReadyState<'_>,
    notifications: PhoneNotifications,
) -> Result<(), String> {
    phone(&ready).await?.notices().set_settings(notifications)
}

#[tauri::command]
async fn paired_pcs(ready: ReadyState<'_>) -> Result<Vec<PairedPc>, String> {
    Ok(phone(&ready).await?.pcs())
}

#[tauri::command]
async fn unpair(app: AppHandle, ready: ReadyState<'_>, pc_id: String) -> Result<(), String> {
    let phone = phone(&ready).await?;
    phone.unpair(&pc_id)?;
    apply_to_device(&app, &phone).await;
    schedule_background_sync(&app, &phone).await;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

#[tauri::command]
async fn pair_begin(ready: ReadyState<'_>, qr: String) -> Result<PairStart, String> {
    phone(&ready).await?.begin_pairing(&qr).await
}

#[tauri::command]
async fn pair_finish(app: AppHandle, ready: ReadyState<'_>) -> Result<(), String> {
    let phone = phone(&ready).await?;
    phone.finish_pairing().await?;
    schedule_background_sync(&app, &phone).await;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

#[tauri::command]
async fn reconnect(app: AppHandle, ready: ReadyState<'_>) -> Result<(), String> {
    let phone = phone(&ready).await?;
    if phone.connected() {
        return Ok(());
    }
    let result = connect(&app, &phone).await;
    let _ = app.emit(CHANGED_EVENT, ());
    result
}

#[derive(Deserialize)]
struct ScanReply {
    text: String,
}

/// Scan the PC's pairing QR with the camera.
#[tauri::command]
async fn scan_qr(app: AppHandle) -> Result<String, String> {
    let reply = native(&app, "scanQr", json!({})).await?;
    serde_json::from_value::<ScanReply>(reply)
        .map(|r| r.text)
        .map_err(|_| "The scanner gave an unexpected answer.".into())
}

/// `{ bottom }`: the navigation bar's height in CSS pixels, which the WebView does not report to CSS.
#[tauri::command]
async fn navigation_inset(app: AppHandle) -> Result<Value, String> {
    native(&app, "navigationInset", json!({})).await
}

/// `{ notifications, exactAlarms, localNetwork }`.
#[tauri::command]
async fn alarm_permissions(app: AppHandle) -> Result<Value, String> {
    native(&app, "permissions", json!({})).await
}

/// Ask for what is missing (notifications, local network, exact alarms), same shape back.
#[tauri::command]
async fn request_permissions(app: AppHandle) -> Result<Value, String> {
    native(&app, "askPermissions", json!({})).await
}

/// Ask the launcher to place the widget: `{ requested }`, false when the launcher cannot.
#[tauri::command]
async fn pin_widget(app: AppHandle) -> Result<Value, String> {
    native(&app, "pinWidget", json!({})).await
}

/// `{ windows, canPin }`: the snapshot ids the widget shows (`null` = every window) and whether
/// the launcher can place the widget from inside the app.
#[tauri::command]
async fn widget_config(app: AppHandle) -> Result<Value, String> {
    native(&app, "getWidgetConfig", json!({})).await
}

/// Choose the windows the widget shows (`null` = every window). Same shape back as `widget_config`.
#[tauri::command]
async fn set_widget_config(app: AppHandle, windows: Option<Vec<String>>) -> Result<Value, String> {
    let windows = widget::normalize_selection(windows)?;
    native(&app, "setWidgetConfig", json!({ "windows": windows })).await
}

// ------------------------------------------------------------------ startup

/// Open the phone: unwrap (or create) its key, open its stores. Blocking.
fn open_phone(app: AppHandle) -> Result<Phone, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let keypair = if cfg!(target_os = "android") {
        identity::load_or_create(&dir, &KeystoreWrapper(app))?
    } else {
        identity::load_or_create(&dir, &identity::PassThrough)?
    };
    Phone::open(&dir, PHONE_NAME, keypair)
}

/// Start the phone off the main thread (the Keystore call goes through Kotlin), then run the sync
/// and reconnect loops.
fn start(app: AppHandle, ready: watch::Sender<Option<Arc<Phone>>>) {
    tauri::async_runtime::spawn(async move {
        let opener = app.clone();
        let opened = tauri::async_runtime::spawn_blocking(move || {
            // A background sync round may have the files open: let it finish first.
            REGISTRY.app_starting(APP_WAITS_FOR_ROUND);
            open_phone(opener)
        })
        .await;
        let phone = match opened {
            Ok(Ok(phone)) => Arc::new(phone),
            Ok(Err(e)) => {
                REGISTRY.app_failed();
                return eprintln!("cooldown: could not start: {e}");
            }
            Err(e) => {
                REGISTRY.app_failed();
                return eprintln!("cooldown: could not start: {e}");
            }
        };
        REGISTRY.app_ready(phone.clone());
        let _ = ready.send(Some(phone.clone()));
        apply_to_device(&app, &phone).await;
        schedule_background_sync(&app, &phone).await;
        let _ = app.emit(CHANGED_EVENT, ());

        // New syncs: keep them, ring their bells, refresh the widget, tell the UI.
        let sync_app = app.clone();
        let sync_phone = phone.clone();
        tauri::async_runtime::spawn(async move {
            let mut syncs = sync_phone.subscribe();
            while syncs.changed().await.is_ok() {
                let latest = syncs.borrow_and_update().clone();
                if let Some(sync) = latest {
                    sync_phone.persist_latest(&sync);
                    apply_to_device(&sync_app, &sync_phone).await;
                    let _ = sync_app.emit(CHANGED_EVENT, ());
                }
            }
        });

        // A PC that removes this phone takes its readings and bells with it, at once: cancel its
        // alarms, blank the widget, tell the UI. Any other end of a session just refreshes the UI.
        let end_app = app.clone();
        let end_phone = phone.clone();
        tauri::async_runtime::spawn(async move {
            let mut ended = end_phone.subscribe_ended();
            loop {
                match ended.recv().await {
                    Ok(end) if end.reason == CloseReason::Revoked => {
                        end_phone.pc_revoked(&end.pc);
                        apply_to_device(&end_app, &end_phone).await;
                        schedule_background_sync(&end_app, &end_phone).await;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
                let _ = end_app.emit(CHANGED_EVENT, ());
            }
        });

        // Tell the UI when the link comes up or drops, without waiting for a sync.
        let watch_app = app.clone();
        let watch_phone = phone.clone();
        tauri::async_runtime::spawn(async move {
            let mut was = watch_phone.connected();
            loop {
                tokio::time::sleep(LINK_CHECK).await;
                let now = watch_phone.connected();
                if now != was {
                    was = now;
                    let _ = watch_app.emit(CHANGED_EVENT, ());
                }
            }
        });

        // Stay connected while the app runs.
        loop {
            if !phone.pcs_by_recency().is_empty() && !phone.connected() {
                let _ = connect(&app, &phone).await;
            }
            tokio::time::sleep(RECONNECT_EVERY).await;
        }
    });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = watch::channel(None);
    app.manage(Ready(rx));
    start(app.handle().clone(), tx);
    Ok(())
}

/// Start the app (the Android entry point).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(native_plugin())
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            get_snapshots,
            get_alerts,
            set_alert,
            paired_pcs,
            unpair,
            pair_begin,
            pair_finish,
            reconnect,
            scan_qr,
            alarm_permissions,
            navigation_inset,
            request_permissions,
            pin_widget,
            widget_config,
            set_widget_config,
            get_notifications,
            set_notifications,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cooldown");
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn intent(id: &str, at: time::OffsetDateTime, enabled: bool) -> AlertIntent {
        AlertIntent {
            id: id.into(),
            fire_at: at,
            enabled,
        }
    }

    #[test]
    fn only_enabled_future_bells_become_alarms() {
        let now = datetime!(2026-10-16 12:00:00 UTC);
        let now_ms = (now.unix_timestamp_nanos() / 1_000_000) as i64;
        let alarms = alarm_list(
            &[
                intent(
                    "reset:claude:default:session_5h",
                    now + time::Duration::hours(1),
                    true,
                ),
                intent(
                    "reset:codex:default:weekly",
                    now + time::Duration::hours(2),
                    false,
                ),
                intent(
                    "reset:xai:default:balance",
                    now - time::Duration::minutes(1),
                    true,
                ),
                intent("garbage", now + time::Duration::hours(3), true),
            ],
            now_ms,
        );
        assert_eq!(alarms.len(), 2);
        assert_eq!(alarms[0]["title"], "Claude 5-hour limit is back");
        assert_eq!(alarms[0]["atMs"], now_ms + 3_600_000);
        assert_eq!(alarms[1]["title"], "Your limit is back");
    }
}
