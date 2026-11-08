// PC side effects for the engine: the whitelist mapped onto existing Nectar code, the status
// object (design 6.5), events and the audit log.

use super::audit::{AuditLog, Entry};
use super::device::DeviceInfo;
use super::engine::Host;
use super::remote::{Deny, Op, Perms};
use super::store;
use super::transfer::{self, XferError};
use crate::types::SystemCommand;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

/// Set when something in the status object changed; the client pushes at most once a second.
pub static STATUS_DIRTY: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Cache {
    muted: bool,
    agent: Option<Value>,
    media: Option<Value>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { muted: false, agent: None, media: None });
/// `url.open` requests waiting for Open / Ignore on the PC (`openUrl: "ask"`).
static URL_REQUESTS: Mutex<Option<HashMap<String, (String, String)>>> = Mutex::new(None);

pub fn on_volume_change(payload: &str) {
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        if let (Some(m), Ok(mut c)) = (v.get("is_muted").and_then(Value::as_bool), CACHE.lock()) {
            c.muted = m;
        }
    }
    STATUS_DIRTY.store(true, Ordering::Relaxed);
}

pub fn on_agent_activity(payload: &str) {
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        let agent = json!({
            "state": v.get("state").cloned().unwrap_or(Value::Null),
            "name": v.get("name").cloned().unwrap_or(Value::Null),
            "project": v.get("project").cloned().unwrap_or(Value::Null),
        });
        if let Ok(mut c) = CACHE.lock() {
            c.agent = Some(agent);
        }
    }
    STATUS_DIRTY.store(true, Ordering::Relaxed);
}

/// `media-update` fires about once a second while music plays (the position moves); only the
/// fields the status object carries make it dirty.
pub fn on_media_update(payload: &str) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return };
    let key = json!([v.get("has_media"), v.get("is_playing"), v.get("title"), v.get("artist")]);
    let changed = CACHE.lock().map(|mut c| {
        let changed = c.media.as_ref() != Some(&key);
        c.media = Some(key);
        changed
    });
    if changed.unwrap_or(true) {
        mark_dirty();
    }
}

pub fn mark_dirty() {
    STATUS_DIRTY.store(true, Ordering::Relaxed);
}

fn battery() -> Value {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut p = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut p) }.is_err() || p.BatteryFlag == 128 || p.BatteryLifePercent == 255 {
        return Value::Null;
    }
    json!({ "percent": p.BatteryLifePercent, "charging": p.ACLineStatus == 1 })
}

pub fn is_locked() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, UOI_NAME,
    };
    unsafe {
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) else { return true };
        let mut name = [0u16; 64];
        let ok = GetUserObjectInformationW(HANDLE(desk.0), UOI_NAME, Some(name.as_mut_ptr() as *mut _), (name.len() * 2) as u32, None).is_ok();
        let _ = CloseDesktop(desk);
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        ok && !String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("default")
    }
}

/// The status object for one device; fields appear only with their permission.
pub fn status(perms: &Perms) -> Value {
    let media = crate::services::LAST_MEDIA.lock().ok().and_then(|m| m.clone()).filter(|m| m.has_media).map(|m| {
        if perms.media_info {
            let cut = |s: &str| s.chars().take(200).collect::<String>();
            json!({ "playing": m.is_playing, "title": cut(&m.title), "artist": cut(&m.artist) })
        } else {
            json!({ "playing": m.is_playing })
        }
    });
    let (muted, agent) = CACHE.lock().map(|c| (c.muted, c.agent.clone())).unwrap_or((false, None));
    let level = crate::state::CURRENT_VOLUME.load(Ordering::Relaxed) as f64 / 100.0;
    let mut out = json!({
        "battery": battery(),
        "media": media,
        "volume": { "level": level, "muted": muted },
        "agent": if perms.agent_status { agent.unwrap_or(Value::Null) } else { Value::Null },
        "locked": is_locked(),
    });
    if perms.agent_ask {
        let (asks, more) = fit_asks(crate::agent_api::pending_asks());
        out["asks"] = json!(asks);
        if more > 0 {
            out["asksMore"] = json!(more);
        }
    }
    out
}

const MAX_PHONE_ASKS: usize = 8;
/// Serialized bytes of the asks in one status, leaving room under the sealed-message limit for the rest.
const ASKS_BUDGET: usize = 48 * 1024;

/// The oldest asks that fit in `ASKS_BUDGET` (at most `MAX_PHONE_ASKS`) and how many are left out;
/// a status over the limit would be dropped by the phone and freeze it.
fn fit_asks(asks: Vec<crate::agent_api::PendingAsk>) -> (Vec<Value>, usize) {
    let total = asks.len();
    let mut used = 2;
    let mut kept = Vec::new();
    for ask in asks.into_iter().take(MAX_PHONE_ASKS) {
        let value = json!(ask);
        let size = value.to_string().len() + 1;
        if used + size > ASKS_BUDGET {
            break;
        }
        used += size;
        kept.push(value);
    }
    let more = total - kept.len();
    (kept, more)
}

/// Received files and texts no notch has taken yet (at startup the first mailbox scan can
/// finish before the shelf listens). Memory only, newest last.
static RECEIVED: Mutex<(u64, VecDeque<Value>)> = Mutex::new((0, VecDeque::new()));
const MAX_RECEIVED: usize = 20;

fn shelf_on(app: &AppHandle) -> bool {
    let get = |k: &str| crate::utils::get_setting_str(app, k).unwrap_or_default();
    get("nectar-panel-shelf-enabled") != "false" && get("nectar-legacy-mode") != "true"
}

/// Hands a received item to the notch shelf: queued until a shelf takes it, and announced
/// with `phone-link-received`. Without a shelf a notification says what came, and text also
/// goes to the clipboard when the device may set it.
fn deliver(app: &AppHandle, mut payload: Value, clipboard: bool) {
    if shelf_on(app) {
        let seq = RECEIVED.lock().map(|mut q| {
            // Starts at the clock so shelf ids saved by an earlier run never match a new item.
            if q.0 == 0 {
                q.0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_millis() as u64);
            }
            q.0 += 1;
            let seq = q.0;
            payload["seq"] = json!(seq);
            q.1.push_back(payload.clone());
            while q.1.len() > MAX_RECEIVED {
                q.1.pop_front();
            }
            seq
        });
        if seq.is_ok() {
            let _ = app.emit("phone-link-received", payload);
        }
        return;
    }
    let device = payload["name"].as_str().unwrap_or_default();
    let source = crate::i18n::tr(app, "phone.notice.app");
    if let Some(text) = payload["text"].as_str() {
        let copied = clipboard && super::clipboard::set_text(text).is_ok();
        let title = crate::i18n::tr(app, "phone.notice.text").replace("{name}", device);
        let body = if copied { crate::i18n::tr(app, "phone.notice.textCopied") } else { text.chars().take(120).collect() };
        crate::notifications::show_local(app, &source, &title, &body);
    } else if let Some(paths) = payload["paths"].as_array() {
        let names: Vec<String> = paths
            .iter()
            .filter_map(Value::as_str)
            .map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        let title = crate::i18n::tr(app, "phone.notice.file").replace("{name}", device);
        crate::notifications::show_local(app, &source, &title, &names.join(", "));
    }
}

/// Everything received that no notch has taken yet; the shelf calls this on mount and after
/// each `phone-link-received`.
pub fn take_received() -> Vec<Value> {
    RECEIVED.lock().map(|mut q| q.1.drain(..).collect()).unwrap_or_default()
}

pub fn take_url_request(id: &str) -> Option<(String, String)> {
    URL_REQUESTS.lock().ok()?.as_mut()?.remove(id)
}

pub struct AppHost<'a> {
    pub app: &'a AppHandle,
    pub audit: &'a AuditLog,
}

fn system(cmd: SystemCommand) -> Result<Value, Deny> {
    crate::state::COMMAND_SENDER.get().ok_or(Deny::Failed)?.send(cmd).map_err(|_| Deny::Failed)?;
    mark_dirty();
    Ok(Value::Null)
}

impl Host for AppHost<'_> {
    fn execute(&mut self, device: &DeviceInfo, op: &Op) -> Result<Value, Deny> {
        match op {
            Op::MediaPlayPause => system(SystemCommand::MediaPlayPause),
            Op::MediaNext => system(SystemCommand::MediaNext),
            Op::MediaPrevious => system(SystemCommand::MediaPrevious),
            Op::VolumeSet(v) => system(SystemCommand::SetVolume(*v)),
            Op::VolumeMute => system(SystemCommand::VolumeMute),
            Op::VolumeUp => system(SystemCommand::VolumeUp),
            Op::VolumeDown => system(SystemCommand::VolumeDown),
            Op::PcLock => {
                unsafe { windows::Win32::System::Shutdown::LockWorkStation() }.map_err(|_| Deny::Failed)?;
                Ok(Value::Null)
            }
            Op::UrlOpen(url) => tauri_plugin_opener::open_url(url, None::<&str>).map(|_| Value::Null).map_err(|_| Deny::Failed),
            Op::ClipboardSet(text) => super::clipboard::set_text(text).map(|_| Value::Null),
            Op::StatusGet => Ok(status(&device.perms)),
            Op::AgentAnswer { id, key, allow } => crate::agent_api::answer_remote(self.app, *id, key, *allow).map(|_| Value::Null),
        }
    }

    fn confirm_url(&mut self, device: &DeviceInfo, url: &str) {
        let id = super::crypto::hex(&super::crypto::random_bytes::<8>());
        if let Ok(mut map) = URL_REQUESTS.lock() {
            let map = map.get_or_insert_with(HashMap::new);
            if map.len() >= 20 {
                map.clear();
            }
            map.insert(id.clone(), (url.to_string(), device.id.clone()));
        }
        let _ = self.app.emit(
            "phone-link-url-request",
            json!({ "id": id, "deviceId": device.id, "name": device.name, "url": url, "host": super::remote::url_host(url) }),
        );
    }

    fn status(&self, perms: &Perms) -> Value {
        let mut out = status(perms);
        // The phone checks a file against this before uploading it to the mailbox.
        if perms.files_in {
            out["maxInMb"] = json!(self.config().max_in_mb);
        }
        out
    }

    fn text_received(&mut self, device: &DeviceInfo, text: &str) {
        // The shelf may copy it later; that copy is not sent back to the phone.
        super::clip_sync::note_from_phone(text);
        deliver(self.app, json!({ "deviceId": device.id, "name": device.name, "text": text }), device.perms.clipboard);
    }

    fn emit(&mut self, event: &str, payload: Value) {
        // Design 6.1 / 6.2: a new session and a clone suspension also show in the notch.
        let name = payload.get("name").and_then(Value::as_str).unwrap_or_default();
        let source = crate::i18n::tr(self.app, "phone.notice.app");
        if event == "phone-link-device-suspended" {
            crate::notifications::show_local(self.app, &source, name, &crate::i18n::tr(self.app, "phone.suspended"));
        } else if event == "phone-link-session" && payload["state"] == "start" && payload["toast"] == true {
            let title = crate::i18n::tr(self.app, "phone.notice.connected").replace("{name}", name);
            crate::notifications::show_local(self.app, &source, &title, "");
        }
        if event == "phone-link-received" {
            return deliver(self.app, payload, false);
        }
        let _ = self.app.emit(event, payload);
    }

    fn audit(&mut self, entry: Entry) {
        self.audit.write(&entry);
    }

    fn inbound_check(&mut self, size: u64) -> Result<PathBuf, XferError> {
        let cfg = self.config();
        if !transfer::within_limit(size, cfg.max_in_mb) {
            return Err(XferError::TooBig);
        }
        let dir = cfg.download_dir();
        std::fs::create_dir_all(&dir).map_err(|_| XferError::Io)?;
        if transfer::has_streams(&dir) == Some(false) {
            return Err(XferError::Folder);
        }
        if transfer::free_space(&dir).is_some_and(|free| free < size + 64 * 1024 * 1024) {
            return Err(XferError::Disk);
        }
        Ok(dir)
    }

    fn max_out_bytes(&self) -> u64 {
        u64::from(self.config().max_out_mb) * 1024 * 1024
    }

    fn processed(&self, fid: &str) -> bool {
        self.config().processed_fids.contains_key(fid)
    }

    fn mark_processed(&mut self, fid: &str, now: i64) {
        let Ok(dir) = self.app.path().app_config_dir() else { return };
        let mut cfg = store::load(&dir);
        cfg.prune_fids(now);
        cfg.processed_fids.insert(fid.to_string(), now);
        let _ = store::save(&dir, &cfg);
    }

    fn pc_locked(&self) -> bool {
        is_locked()
    }

    fn clip_fids(&self, device: &str) -> Vec<String> {
        self.config().clip_fids.get(device).cloned().unwrap_or_default()
    }

    fn set_clip_fids(&mut self, device: &str, fids: &[String]) {
        let Ok(dir) = self.app.path().app_config_dir() else { return };
        let mut cfg = store::load(&dir);
        if fids.is_empty() {
            cfg.clip_fids.remove(device);
        } else {
            cfg.clip_fids.insert(device.to_string(), fids.to_vec());
        }
        let _ = store::save(&dir, &cfg.normalized());
    }
}

impl AppHost<'_> {
    fn config(&self) -> store::Config {
        self.app.path().app_config_dir().map(|d| store::load(&d)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_api::PendingAsk;

    #[test]
    fn asks_fit_the_message_limit_in_the_worst_case() {
        let asks: Vec<_> = (1..=20).map(|id| PendingAsk::sample(id, "\u{1F600}")).collect();
        let (kept, more) = fit_asks(asks);
        assert!(!kept.is_empty() && kept.len() < MAX_PHONE_ASKS);
        assert_eq!(kept.len() + more, 20);
        let title = "\u{1F600}".repeat(200);
        let status = json!({
            "battery": { "percent": 100, "charging": true },
            "media": { "playing": true, "title": title, "artist": title },
            "volume": { "level": 1.0, "muted": false },
            "agent": { "state": "needs_input", "name": "x".repeat(400), "project": "x".repeat(400), "title": "x".repeat(400) },
            "locked": false,
            "asks": kept,
            "asksMore": more,
        });
        let inner = json!({ "t": "evt", "op": "status", "d": status }).to_string();
        assert!(inner.len() <= super::super::engine::MAX_PLAIN, "{}", inner.len());

        let (kept, more) = fit_asks((1..=3).map(|id| PendingAsk::sample(id, "a")).collect());
        assert_eq!((kept.len(), more), (3, 0));
    }
}
