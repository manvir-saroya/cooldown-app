//! The commands the UI calls (`invoke`). One per `SnapshotSource` method in
//! `packages/ui/src/data/SnapshotSource.tsx`. Errors are plain sentences for the UI.
//!
//! No command returns an API key: keys go in, never come out.

use std::sync::Arc;
use std::time::Duration;

use cooldown_core::{AlertIntent, Snapshot};
use tauri::State;

use crate::backend::Backend;
use crate::hook;
use crate::settings::Notifications;
use crate::views::{
    AdapterInfo, HistorySeries, HookStatus, PairedDevice, PairingPayload, PairingState,
};

type Shared<'a> = State<'a, Arc<Backend>>;

/// Run blocking work (credential store, file edits) off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
pub fn get_snapshots(backend: Shared<'_>) -> Result<Vec<Snapshot>, String> {
    backend.snapshots()
}

#[tauri::command]
pub fn get_history(
    backend: Shared<'_>,
    snapshot_id: String,
    range: String,
) -> Result<HistorySeries, String> {
    backend.history(&snapshot_id, &range)
}

#[tauri::command]
pub fn get_adapters(backend: Shared<'_>) -> Vec<AdapterInfo> {
    backend.adapters()
}

#[tauri::command]
pub async fn save_api_key(
    backend: Shared<'_>,
    adapter_id: String,
    key: String,
    team_id: Option<String>,
) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.save_key(&adapter_id, &key, team_id.as_deref())).await
}

#[tauri::command]
pub async fn remove_api_key(backend: Shared<'_>, adapter_id: String) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.remove_key(&adapter_id)).await
}

#[tauri::command]
pub async fn get_hook_status() -> Result<HookStatus, String> {
    blocking(hook::status).await
}

#[tauri::command]
pub async fn install_hook(backend: Shared<'_>) -> Result<(), String> {
    blocking(hook::install).await?;
    backend.restart_polling();
    Ok(())
}

#[tauri::command]
pub async fn uninstall_hook() -> Result<(), String> {
    blocking(hook::uninstall).await.map(|_| ())
}

#[tauri::command]
pub async fn get_agent_hook_status(agent: String) -> Result<HookStatus, String> {
    let agent = hook::parse_agent(&agent)?;
    blocking(move || hook::status_for(agent)).await
}

#[tauri::command]
pub async fn install_agent_hook(backend: Shared<'_>, agent: String) -> Result<(), String> {
    let agent = hook::parse_agent(&agent)?;
    blocking(move || hook::install_for(agent)).await?;
    backend.restart_polling();
    Ok(())
}

#[tauri::command]
pub async fn uninstall_agent_hook(agent: String) -> Result<(), String> {
    let agent = hook::parse_agent(&agent)?;
    blocking(move || hook::uninstall_for(agent))
        .await
        .map(|_| ())
}

#[tauri::command]
pub fn get_paired_devices(backend: Shared<'_>) -> Result<Vec<PairedDevice>, String> {
    backend.phone().devices()
}

#[tauri::command]
pub fn revoke_device(backend: Shared<'_>, device_id: String) -> Result<(), String> {
    backend.phone().revoke(&device_id)
}

#[tauri::command]
pub async fn create_pairing_payload(backend: Shared<'_>) -> Result<PairingPayload, String> {
    let backend = Arc::clone(&backend);
    backend.start_phone_link().await?;
    let server = backend
        .phone()
        .server()
        .ok_or("The phone link did not start.")?;
    backend.phone().open_pairing(&server)
}

#[tauri::command]
pub fn get_pairing_state(backend: Shared<'_>) -> PairingState {
    backend.phone().state()
}

#[tauri::command]
pub fn confirm_pairing(backend: Shared<'_>, accept: bool) -> Result<(), String> {
    backend.phone().confirm_pairing(accept)
}

#[tauri::command]
pub fn cancel_pairing(backend: Shared<'_>) {
    backend.phone().cancel_pairing();
}

#[tauri::command]
pub async fn refresh(backend: Shared<'_>) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    backend.refresh(Duration::from_secs(20)).await;
    Ok(())
}

#[tauri::command]
pub fn get_alerts(backend: Shared<'_>) -> Result<Vec<AlertIntent>, String> {
    backend.alerts()
}

// Off the main thread: a change rewrites settings.json (with fsync) while phones are paired.
#[tauri::command]
pub async fn set_alert(backend: Shared<'_>, intent: AlertIntent) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.set_alert(intent)).await
}

#[tauri::command]
pub fn get_copilot_allowance(backend: Shared<'_>) -> Option<f64> {
    backend.copilot_allowance()
}

#[tauri::command]
pub async fn set_copilot_allowance(backend: Shared<'_>, usd: Option<f64>) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.set_copilot_allowance(usd)).await
}

#[tauri::command]
pub async fn set_source_enabled(
    backend: Shared<'_>,
    adapter_id: String,
    enabled: bool,
) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.set_source_enabled(&adapter_id, enabled)).await
}

#[tauri::command]
pub fn get_notifications(backend: Shared<'_>) -> Notifications {
    backend.notifications()
}

#[tauri::command]
pub async fn set_notifications(
    backend: Shared<'_>,
    notifications: Notifications,
) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.set_notifications(notifications)).await
}

#[tauri::command]
pub async fn remove_alert(backend: Shared<'_>, id: String) -> Result<(), String> {
    let backend = Arc::clone(&backend);
    blocking(move || backend.remove_alert(&id)).await
}
