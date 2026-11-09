// Phone link (docs/phone-link-design.md). Runs only while `nectar-phone-link` is "true":
// a background relay client, no listening socket, all secrets in Credential Manager.

pub mod audit;
pub mod client;
pub mod clip_sync;
pub mod clipboard;
#[cfg_attr(not(test), allow(dead_code))]
pub mod config;
#[cfg_attr(not(test), allow(dead_code))]
pub mod crypto;
pub mod device;
pub mod engine;
pub mod host;
pub mod net;
pub mod pairing;
#[cfg_attr(not(test), allow(dead_code))]
pub mod remote;
pub mod secrets;
#[cfg_attr(not(test), allow(dead_code))]
pub mod store;
#[cfg_attr(not(test), allow(dead_code))]
pub mod supabase;
#[cfg_attr(not(test), allow(dead_code))]
pub mod transfer;

#[cfg(test)]
mod e2e;
#[cfg(test)]
mod pwa_interop;
#[cfg(test)]
mod spike;

use audit::AuditLog;
use client::{now_ms, Cmd, Ctx};
use device::{CredentialDevices, DeviceInfo};
use engine::Engine;
use host::AppHost;
use remote::Perms;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::{mpsc, oneshot};

pub const SETTING: &str = "nectar-phone-link";

struct Runtime {
    engine: Arc<Mutex<Engine>>,
    tx: mpsc::UnboundedSender<Cmd>,
    listeners: Vec<tauri::EventId>,
    task: tauri::async_runtime::JoinHandle<()>,
}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);
/// The client task of a runtime that was turned off and may still be shutting down.
static STOPPING: Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = Mutex::new(None);

fn config_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok()
}

fn audit_log(app: &AppHandle) -> AuditLog {
    AuditLog::new(&app.path().app_data_dir().or_else(|_| app.path().app_config_dir()).unwrap_or_else(|_| PathBuf::from(".")))
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

pub fn enabled(app: &AppHandle) -> bool {
    crate::utils::get_setting_str(app, SETTING).as_deref() == Some("true")
}

pub fn start(app: AppHandle) {
    if enabled(&app) {
        start_runtime(&app);
        clip_sync::refresh(&app);
    }
}

/// Called when `nectar-phone-link` changes (Settings or an external edit of settings.json).
pub fn on_setting_changed(app: &AppHandle, value: &Value) {
    let on = value.as_str() == Some("true") || value.as_bool() == Some(true);
    if on {
        start_runtime(app);
    } else {
        stop_runtime(app);
    }
    clip_sync::refresh(app);
}

fn start_runtime(app: &AppHandle) {
    let Ok(mut slot) = RUNTIME.lock() else { return };
    if slot.is_some() {
        return;
    }
    let Some(config_dir) = config_dir(app) else { return };
    let engine = Arc::new(Mutex::new(Engine::new(Box::new(CredentialDevices), &pc_name())));
    let (tx, rx) = mpsc::unbounded_channel();
    let listeners = vec![
        app.listen_any("volume-change", |e| host::on_volume_change(e.payload())),
        app.listen_any("agent-activity", |e| host::on_agent_activity(e.payload())),
        app.listen_any("media-update", |e| host::on_media_update(e.payload())),
    ];
    let ctx = Ctx { app: app.clone(), engine: engine.clone(), audit: audit_log(app), config_dir, tx: tx.clone(), tasks: Default::default(), deferred: Default::default() };
    let previous = STOPPING.lock().ok().and_then(|mut s| s.take());
    let task = tauri::async_runtime::spawn(async move {
        // Never two clients on the same credentials: the old one ends before this one starts.
        if let Some(old) = previous {
            old.abort();
            let _ = old.await;
            ctx.engine.lock().unwrap_or_else(|p| p.into_inner()).reload_devices();
        }
        client::run(ctx, rx).await
    });
    *slot = Some(Runtime { engine, tx, listeners, task });
}

fn stop_runtime(app: &AppHandle) {
    let Some(rt) = RUNTIME.lock().ok().and_then(|mut s| s.take()) else { return };
    let _ = rt.tx.send(Cmd::Shutdown);
    if let Ok(mut s) = STOPPING.lock() {
        *s = Some(rt.task);
    }
    for id in rt.listeners {
        app.unlisten(id);
    }
    clipboard::stop();
}

fn send(cmd: Cmd) -> Result<(), String> {
    let guard = RUNTIME.lock().map_err(|e| e.to_string())?;
    let rt = guard.as_ref().ok_or("phone link is off")?;
    rt.tx.send(cmd).map_err(|_| "phone link is stopping".to_string())
}

async fn ask<T>(make: impl FnOnce(client::Reply<T>) -> Cmd) -> Result<T, String> {
    let (reply, rx) = oneshot::channel();
    send(make(reply))?;
    tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|_| "phone link stopped".to_string())?
}

/// Runs `f` on the live engine, or on one loaded from Credential Manager when the feature is off.
fn with_engine<T>(app: &AppHandle, f: impl FnOnce(&mut Engine, &mut AppHost) -> (T, Vec<engine::Action>)) -> T {
    let audit = audit_log(app);
    let mut host = AppHost { app, audit: &audit };
    let live = RUNTIME.lock().ok().and_then(|s| s.as_ref().map(|rt| rt.engine.clone()));
    let (out, actions) = match &live {
        Some(engine) => {
            let mut eng = engine.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut eng, &mut host)
        }
        None => {
            let mut eng = Engine::new(Box::new(CredentialDevices), &pc_name());
            f(&mut eng, &mut host)
        }
    };
    if actions.is_empty() {
        return out;
    }
    if live.is_some() && send(Cmd::Apply(actions.clone())).is_ok() {
        return out;
    }
    if let Some(dir) = config_dir(app) {
        let mut cfg = store::load(&dir);
        for a in actions {
            if let engine::Action::RelayCleanup(t) = a {
                if !cfg.pending_cleanup.contains(&t) {
                    cfg.pending_cleanup.push(t);
                }
            }
        }
        let _ = store::save(&dir, &cfg);
    }
    out
}

/// Uninstall: removes every phone link credential (design section 9).
pub fn delete_all_secrets() -> usize {
    secrets::delete_all()
}

// Tauri commands (design section 11).

#[tauri::command]
pub async fn phone_link_status(app: AppHandle) -> Value {
    let state = client::STATE.lock().map(|s| s.clone()).unwrap_or(client::StateInfo { state: "off", detail: None });
    let cfg = config_dir(&app).map(|d| store::load(&d)).unwrap_or_default();
    let rc = config::relay();
    let (devices, sessions, view) = with_engine(&app, |e, _| ((e.devices(), e.sessions(), e.pairing_view(&rc.pwa_url, &rc.project_ref)), Vec::new()));
    let pairing = view.is_some();
    // The open offer, so a Settings tab that was closed during pairing can show it again.
    let pairing_view = view.and_then(|(url, expires_at, request)| {
        pairing::qr_svg_data_uri(&url).ok().map(|qr| json!({ "qrSvg": qr, "expiresAt": expires_at, "request": request }))
    });
    json!({
        "pairingView": pairing_view,
        "enabled": enabled(&app),
        "state": state.state,
        "detail": state.detail,
        "devices": devices,
        "sessions": sessions,
        "pairing": pairing,
        "pendingCleanup": cfg.pending_cleanup.len(),
        "downloadDir": cfg.download_dir().to_string_lossy(),
        "maxInMb": cfg.max_in_mb,
        "maxOutMb": cfg.max_out_mb,
        "defaultPerms": Perms::new_device(),
        "clipSync": clip_sync::enabled(&app),
    })
}

#[tauri::command]
pub async fn phone_link_devices(app: AppHandle) -> Vec<DeviceInfo> {
    with_engine(&app, |e, _| (e.devices(), Vec::new()))
}

#[tauri::command]
pub async fn phone_link_sessions(app: AppHandle) -> Vec<engine::SessionInfo> {
    with_engine(&app, |e, _| (e.sessions(), Vec::new()))
}

#[tauri::command]
pub async fn phone_link_start_pairing() -> Result<client::PairingView, String> {
    ask(Cmd::StartPairing).await
}

#[tauri::command]
pub fn phone_link_cancel_pairing() -> Result<(), String> {
    send(Cmd::CancelPairing)
}

#[tauri::command]
pub async fn phone_link_submit_code(code: String, perms: Perms) -> Result<Value, String> {
    ask(|reply| Cmd::SubmitCode { code, perms, reply }).await
}

#[tauri::command]
pub async fn phone_link_set_permissions(app: AppHandle, id: String, perms: Perms) -> Result<(), String> {
    with_engine(&app, |e, h| match e.set_perms(&id, perms, now_ms(), h) {
        Ok(actions) => (Ok(()), actions),
        Err(err) => (Err(err), Vec::new()),
    })
}

#[tauri::command]
pub async fn phone_link_end_session(app: AppHandle, id: String) {
    with_engine(&app, |e, h| ((), e.end_session(&id, h)))
}

#[tauri::command]
pub async fn phone_link_revoke(app: AppHandle, id: String) {
    with_engine(&app, |e, h| ((), e.revoke(h, &id, "ok", now_ms())))
}

#[tauri::command]
pub async fn phone_link_revoke_all(app: AppHandle) {
    with_engine(&app, |e, h| ((), e.revoke_all(h, now_ms())))
}

fn running() -> bool {
    RUNTIME.lock().map(|s| s.is_some()).unwrap_or(false)
}

/// Sends a file (path) or text to a phone (design 7.3). Over a live session it is offered
/// right away; otherwise it goes to the phone's mailbox. Returns `{ fid?, queued }`.
#[tauri::command]
pub async fn phone_link_send(app: AppHandle, device_id: String, path: Option<String>, text: Option<String>) -> Result<Value, String> {
    if !running() {
        return Err("phone link is off".into());
    }
    if let Some(path) = path {
        return send_file(&app, &device_id, PathBuf::from(path)).await;
    }
    let text = text.ok_or("nothing to send")?;
    with_engine(&app, |e, h| match e.send_text(&device_id, &text, h, now_ms()) {
        Ok(actions) => {
            let queued = actions.iter().any(|a| matches!(a, engine::Action::Upload(_)));
            (Ok(json!({ "queued": queued })), actions)
        }
        Err(err) => (Err(err), Vec::new()),
    })
}

/// Debounced PC clipboard text from `clip_sync` to every phone allowed to get it.
fn send_clip(app: &AppHandle, text: &str) {
    if !running() {
        return;
    }
    with_engine(app, |e, h| ((), e.send_clip(text, h, now_ms())));
}

/// `nectar-phone-clipboard-sync` is off: copies waiting in phone mailboxes are deleted.
fn drop_clips(app: &AppHandle) {
    if running() {
        with_engine(app, |e, h| ((), e.drop_clips(h)));
    }
}

async fn send_file(app: &AppHandle, device_id: &str, path: PathBuf) -> Result<Value, String> {
    let meta = std::fs::metadata(&path).map_err(|_| "missing".to_string())?;
    if !meta.is_file() {
        return Err("notFile".into());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("missing")?;
    let max = with_engine(app, |_, h| (engine::Host::max_out_bytes(h), Vec::new()));
    if meta.len() > max {
        return Err("tooBig".into());
    }
    let hashed = path.clone();
    let (size, sha) = tauri::async_runtime::spawn_blocking(move || transfer::hash_file(&hashed))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "unreadable".to_string())?;
    with_engine(app, |e, h| match e.send_file(h, device_id, path, &name, size, sha, now_ms()) {
        Ok((fid, actions)) => {
            let queued = actions.iter().any(|a| matches!(a, engine::Action::Upload(j) if j.mailbox.is_some()));
            (Ok(json!({ "fid": fid, "queued": queued })), actions)
        }
        Err(err) => (Err(err), Vec::new()),
    })
}

/// Settings › Phone: pick a file and send it to one phone.
#[tauri::command]
pub async fn phone_link_send_pick(app: AppHandle, device_id: String) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let Some(picked) = app.dialog().file().blocking_pick_file() else { return Ok(None) };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    phone_link_send(app, device_id, Some(path.to_string_lossy().into_owned()), None).await.map(Some)
}

/// Phones the shelf can send to: `[]` when the feature is off.
#[tauri::command]
pub async fn phone_link_targets(app: AppHandle) -> Vec<Value> {
    if !running() {
        return Vec::new();
    }
    with_engine(&app, |e, _| {
        let live: Vec<String> = e.sessions().into_iter().map(|s| s.device_id).collect();
        let list = e
            .devices()
            .into_iter()
            .filter(|d| !d.suspended && (d.perms.files_out || d.perms.text_out))
            .map(|d| json!({ "id": d.id, "name": d.name, "files": d.perms.files_out, "text": d.perms.text_out, "online": live.contains(&d.id) }))
            .collect();
        (list, Vec::new())
    })
}

/// Transfers in flight (Settings › Phone).
#[tauri::command]
pub async fn phone_link_transfers(app: AppHandle) -> Vec<engine::TransferInfo> {
    with_engine(&app, |e, _| (e.transfers(), Vec::new()))
}

/// Received files and texts waiting for the notch shelf.
#[tauri::command]
pub fn phone_link_take_received() -> Vec<Value> {
    host::take_received()
}

#[tauri::command]
pub fn phone_link_url_answer(id: String, open: bool) -> Result<(), String> {
    let (url, _device) = host::take_url_request(&id).ok_or("request expired")?;
    if open {
        tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn phone_link_reconnect() -> Result<(), String> {
    send(Cmd::Reconnect)
}

#[tauri::command]
pub async fn phone_link_set_download_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let Some(picked) = app.dialog().file().blocking_pick_folder() else { return Ok(None) };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    let mut own: Vec<PathBuf> = Vec::new();
    own.extend(app.path().app_config_dir().ok());
    own.extend(app.path().app_data_dir().ok());
    own.extend(app.path().app_local_data_dir().ok());
    own.extend(std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)));
    let checked = store::check_download_dir(&path, &own).map_err(|e| e.to_string())?;
    let dir = config_dir(&app).ok_or("no config dir")?;
    let mut cfg = store::load(&dir);
    cfg.download_dir = Some(checked.to_string_lossy().into_owned());
    store::save(&dir, &cfg)?;
    Ok(cfg.download_dir)
}

#[tauri::command]
pub fn phone_link_set_limits(app: AppHandle, max_in_mb: u32, max_out_mb: u32) -> Result<Value, String> {
    let dir = config_dir(&app).ok_or("no config dir")?;
    let mut cfg = store::load(&dir);
    cfg.max_in_mb = max_in_mb;
    cfg.max_out_mb = max_out_mb;
    let cfg = cfg.normalized();
    store::save(&dir, &cfg)?;
    Ok(json!({ "maxInMb": cfg.max_in_mb, "maxOutMb": cfg.max_out_mb }))
}

#[tauri::command]
pub fn phone_link_open_audit_log(app: AppHandle) -> Result<(), String> {
    let log = audit_log(&app);
    let path = log.path().to_path_buf();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, "").map_err(|e| e.to_string())?;
    }
    tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
}
