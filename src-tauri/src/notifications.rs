//! Windows toasts in the notch (T-014).
//!
//! Unpackaged Nectar can read toasts through `UserNotificationListener`, but its `NotificationChanged`
//! event needs package identity (it fails with ELEMENT_NOT_FOUND here). Listing toasts is expensive
//! (~150 ms CPU, seconds of wall time with a full Action Center), so instead of polling, the worker
//! sleeps on change notifications (the per-app notification settings in HKCU, which Windows touches
//! for every toast, and the notification database folder) and lists toasts only then, diffing the ids
//! against the last list.
//! Notification text only travels to the notch window in memory; it is never written to disk.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{HSTRING, PCWSTR};
use windows::ApplicationModel::AppDisplayInfo;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject};
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

pub const SETTING: &str = "nectar-notch-notifications";
pub const HIDE_SETTING: &str = "nectar-notch-notifications-hide-toasts";
pub const EXCLUDE_SETTING: &str = "nectar-notch-notifications-exclude";
const LEGACY_SETTING: &str = "nectar-legacy-mode";
const BANNERS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
const BACKUP_FILE: &str = "notification-banners.json";
// A toast that is already this old when Nectar first sees it is history (resume, sync), not news.
const MAX_AGE_MS: i64 = 2 * 60 * 1000;
const MAX_NEW_PER_CHANGE: usize = 5;
const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 600;
const RESCAN_EVERY: Duration = Duration::from_secs(5 * 60);
const FOLLOW_UP_MS: u32 = 1000;

/// Ids of Nectar's own notch cards (phone link); kept out of the range Windows hands out.
const LOCAL_ID_BASE: u32 = 0xF000_0000;
static LOCAL_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(LOCAL_ID_BASE);

/// A card from Nectar itself, shown like a Windows toast.
pub fn show_local(app: &AppHandle, source: &str, title: &str, body: &str) {
    let id = LOCAL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed).max(LOCAL_ID_BASE);
    let n = NotchNotification {
        id,
        app: source.into(),
        aumid: String::new(),
        title: title.into(),
        body: body.into(),
        icon: None,
        time: crate::utils::get_now_ms(),
    };
    let _ = app.emit_to("main", "notch-notification", &n);
}

#[derive(Clone, Serialize, Debug)]
pub struct NotchNotification {
    pub id: u32,
    pub app: String,
    pub aumid: String,
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    pub time: i64,
}

#[derive(Clone, Serialize)]
pub struct NotificationApp {
    pub aumid: String,
    pub name: String,
    pub icon: Option<String>,
}

#[derive(Default)]
pub struct Config {
    pub on: bool,
    pub hide: bool,
    pub exclude: Vec<String>,
}

impl Config {
    fn read(app: &AppHandle) -> Self {
        let flag = |key: &str| crate::utils::get_setting_str(app, key).as_deref() == Some("true");
        let on = flag(SETTING) && !flag(LEGACY_SETTING);
        Config { on, hide: on && flag(HIDE_SETTING), exclude: parse_exclude(crate::utils::get_setting_str(app, EXCLUDE_SETTING).as_deref()) }
    }

    fn excludes(&self, aumid: &str) -> bool {
        self.exclude.iter().any(|e| e.eq_ignore_ascii_case(aumid))
    }
}

/// The exclude list is a JSON array of AppUserModelIDs.
pub fn parse_exclude(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|r| serde_json::from_str::<Vec<String>>(r).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn is_own(aumid: &str, app: &str) -> bool {
    aumid.to_ascii_lowercase().contains("com.kua8.nectar") || app.eq_ignore_ascii_case("nectar")
}

/// Ids that appeared and ids that went away since the last list, both ascending.
pub fn diff_ids(seen: &HashSet<u32>, current: &HashSet<u32>) -> (Vec<u32>, Vec<u32>) {
    let mut added: Vec<u32> = current.difference(seen).copied().collect();
    let mut removed: Vec<u32> = seen.difference(current).copied().collect();
    added.sort_unstable();
    removed.sort_unstable();
    (added, removed)
}

/// After a burst only the newest few are worth a card.
pub fn newest(added: &[u32], max: usize) -> &[u32] {
    &added[added.len().saturating_sub(max)..]
}

fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The first text line of a toast is its title, the rest is the body.
pub fn split_text(parts: Vec<String>) -> (String, String) {
    let mut lines = parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    let title = lines.next().map(|t| clamp_chars(&t, MAX_TITLE)).unwrap_or_default();
    let body = clamp_chars(&lines.collect::<Vec<_>>().join("\n"), MAX_BODY);
    (title, body)
}

/// WinRT DateTime (100 ns ticks since 1601) to Unix milliseconds.
pub fn ticks_to_unix_ms(ticks: i64) -> i64 {
    (ticks - 116_444_736_000_000_000) / 10_000
}

/// Active Focus / Do Not Disturb profile (0 = off). Windows has no public API for it, so this reads
/// the shell's WNF state, as other shell tools do; None when that isn't available.
pub fn quiet_hours_profile() -> Option<u32> {
    type QueryWnf = unsafe extern "system" fn(*const u64, *const std::ffi::c_void, *const std::ffi::c_void, *mut u32, *mut std::ffi::c_void, *mut u32) -> i32;
    const WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED: u64 = 0x0D83_063E_A3BF_1C75;
    unsafe {
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        let ntdll = GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()?;
        let proc = GetProcAddress(ntdll, windows::core::s!("NtQueryWnfStateData"))?;
        let query: QueryWnf = std::mem::transmute(proc);
        let mut stamp = 0u32;
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = query(&WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED, std::ptr::null(), std::ptr::null(), &mut stamp, &mut value as *mut u32 as *mut _, &mut size);
        (status >= 0).then_some(if size == 0 { 0 } else { value })
    }
}

/// Do Not Disturb, presentation mode or Windows quiet time: Windows keeps toasts silent, so does the notch.
fn is_quiet() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME};
    let state = unsafe { SHQueryUserNotificationState() }.ok();
    matches!(state, Some(s) if s == QUNS_PRESENTATION_MODE || s == QUNS_QUIET_TIME) || quiet_hours_profile().is_some_and(|p| p != 0)
}

static WAKE: OnceLock<isize> = OnceLock::new();
static ICONS: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

/// Wakes the worker so a settings change takes effect right away.
pub fn kick() {
    if let Some(&h) = WAKE.get() {
        if h != 0 {
            unsafe { let _ = SetEvent(HANDLE(h as *mut _)); }
        }
    }
}

pub fn start(app: AppHandle) {
    let wake = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map(|h| h.0 as isize).unwrap_or(0);
    let _ = WAKE.set(wake);
    std::thread::spawn(move || worker(app, HANDLE(wake as *mut _)));
}

fn notifications_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Notifications"))
}

struct DirWatch(HANDLE);

impl DirWatch {
    fn new() -> Option<Self> {
        use windows::Win32::Storage::FileSystem::{FindFirstChangeNotificationW, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE};
        let dir = notifications_dir()?;
        let handle = unsafe { FindFirstChangeNotificationW(&HSTRING::from(dir.as_os_str()), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE) }.ok()?;
        (!handle.is_invalid()).then_some(DirWatch(handle))
    }

    /// Re-arms the watch and swallows the writes that belong to the same change.
    fn drain(&self) {
        use windows::Win32::Storage::FileSystem::FindNextChangeNotification;
        for _ in 0..20 {
            unsafe {
                if FindNextChangeNotification(self.0).is_err() || WaitForSingleObject(self.0, 0) != WAIT_OBJECT_0 {
                    return;
                }
            }
        }
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        unsafe { let _ = windows::Win32::Storage::FileSystem::FindCloseChangeNotification(self.0); }
    }
}

/// Windows stamps `LastNotificationAddedTime` under the per-app notification settings for every toast,
/// right away, while the database itself is written lazily.
struct RegWatch {
    key: windows::Win32::System::Registry::HKEY,
    event: HANDLE,
}

impl RegWatch {
    fn new() -> Option<Self> {
        use windows::Win32::System::Registry::{RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY};
        let path: Vec<u16> = BANNERS_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), KEY_NOTIFY, &mut key) }.ok().ok()?;
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.ok()?;
        let watch = RegWatch { key, event };
        watch.arm().then_some(watch)
    }

    fn arm(&self) -> bool {
        use windows::Win32::System::Registry::{RegNotifyChangeKeyValue, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME};
        unsafe { RegNotifyChangeKeyValue(self.key, true, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, Some(self.event), true) }.is_ok()
    }

    fn drain(&self) {
        for _ in 0..20 {
            if !self.arm() || unsafe { WaitForSingleObject(self.event, 0) } != WAIT_OBJECT_0 {
                return;
            }
        }
    }
}

impl Drop for RegWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Registry::RegCloseKey(self.key);
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
        }
    }
}

/// Both change signals; either one means "list the toasts again".
#[derive(Default)]
struct Watches {
    reg: Option<RegWatch>,
    dir: Option<DirWatch>,
}

impl Watches {
    fn open() -> Self {
        Watches { reg: RegWatch::new(), dir: DirWatch::new() }
    }

    fn any(&self) -> bool {
        self.reg.is_some() || self.dir.is_some()
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.reg.iter().map(|w| w.event).chain(self.dir.iter().map(|w| w.0)).collect()
    }

    fn drain(&self) {
        if let Some(w) = &self.reg {
            w.drain();
        }
        if let Some(w) = &self.dir {
            w.drain();
        }
    }
}

/// Index of the signalled handle, None on timeout.
fn wait(handles: &[HANDLE], timeout_ms: u32) -> Option<usize> {
    let handles: Vec<HANDLE> = handles.iter().copied().filter(|h| !h.is_invalid() && !h.0.is_null()).collect();
    if handles.is_empty() {
        std::thread::sleep(Duration::from_millis(timeout_ms as u64));
        return None;
    }
    let r = unsafe { WaitForMultipleObjects(&handles, false, timeout_ms) };
    let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    (i < handles.len()).then_some(i)
}

fn worker(app: AppHandle, wake: HANDLE) {
    unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED); }
    let mut listener: Option<UserNotificationListener> = None;
    let mut seen: Option<HashSet<u32>> = None;
    let mut watch: Option<Watches> = None;
    let mut was_on = false;
    let mut need_scan = true;
    let mut from_watch = false;
    let mut follow_up = false;
    let mut last_scan = Instant::now();
    loop {
        // Quitting puts the banners back; don't take them away again on the way out.
        if crate::state::SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let cfg = Config::read(&app);
        if cfg.on && listener.is_none() {
            listener = UserNotificationListener::Current().ok();
        }
        let allowed = cfg.on && listener.as_ref().and_then(|l| l.GetAccessStatus().ok()) == Some(UserNotificationListenerAccessStatus::Allowed);
        banners::sync(&app, (allowed && cfg.hide).then_some(&cfg));

        if !allowed {
            if was_on {
                let _ = app.emit_to("main", "notch-notifications-clear", ());
            }
            was_on = false;
            seen = None;
            watch = None;
            if !cfg.on {
                listener = None;
            }
            need_scan = true;
            wait(&[wake], 10_000);
            continue;
        }
        was_on = true;
        let watches = watch.get_or_insert_with(Watches::open);
        if need_scan || last_scan.elapsed() >= RESCAN_EVERY || !watches.any() {
            let found = listener.as_ref().is_some_and(|l| scan(&app, l, &cfg, &mut seen));
            // The registry changes ~0.5 s before the toast can be listed; look once more if it wasn't there yet.
            follow_up = from_watch && !found;
            last_scan = Instant::now();
        }
        let mut handles = vec![wake];
        handles.extend(watches.handles());
        // Without any change signal, fall back to a slow poll.
        let timeout = if follow_up { FOLLOW_UP_MS } else if watches.any() { 30_000 } else { 5_000 };
        let signalled = wait(&handles, timeout);
        need_scan = signalled.is_some() || std::mem::take(&mut follow_up);
        from_watch = signalled.is_some_and(|i| i > 0);
        if from_watch {
            // A toast touches several values in a row; let them settle and count them as one change.
            std::thread::sleep(Duration::from_millis(200));
            watches.drain();
        }
    }
}

/// Lists the toasts and emits the new ones. Returns true when the list had new ids (or was the first one).
fn scan(app: &AppHandle, listener: &UserNotificationListener, cfg: &Config, seen: &mut Option<HashSet<u32>>) -> bool {
    let Ok(list) = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()) else { return false };
    let mut items: HashMap<u32, UserNotification> = HashMap::new();
    for i in 0..list.Size().unwrap_or(0) {
        if let Ok(n) = list.GetAt(i) {
            if let Ok(id) = n.Id() {
                items.insert(id, n);
            }
        }
    }
    let current: HashSet<u32> = items.keys().copied().collect();
    // The first list after turning on is what was already there; only later ones are new.
    let Some(prev) = seen.replace(current.clone()) else { return true };
    let (added, removed) = diff_ids(&prev, &current);
    if !removed.is_empty() {
        let _ = app.emit_to("main", "notch-notification-removed", &removed);
    }
    if added.is_empty() {
        return false;
    }
    if is_quiet() {
        return true;
    }
    let now = crate::utils::get_now_ms();
    for id in newest(&added, MAX_NEW_PER_CHANGE) {
        let Some(n) = items.get(id).and_then(read_notification) else { continue };
        if is_own(&n.aumid, &n.app) || cfg.excludes(&n.aumid) || now - n.time > MAX_AGE_MS {
            continue;
        }
        let _ = app.emit_to("main", "notch-notification", &n);
    }
    true
}

fn read_notification(n: &UserNotification) -> Option<NotchNotification> {
    let id = n.Id().ok()?;
    let info = n.AppInfo().ok();
    let aumid = info.as_ref().and_then(|a| a.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();
    let display = info.as_ref().and_then(|a| a.DisplayInfo().ok());
    let app = display
        .as_ref()
        .and_then(|d| d.DisplayName().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| aumid.clone());
    let parts = n
        .Notification()
        .and_then(|t| t.Visual())
        .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
        .and_then(|b| b.GetTextElements())
        .map(|texts| (0..texts.Size().unwrap_or(0)).filter_map(|i| texts.GetAt(i).ok()?.Text().ok().map(|s| s.to_string())).collect::<Vec<_>>())
        .unwrap_or_default();
    let (title, body) = split_text(parts);
    let time = n.CreationTime().map(|t| ticks_to_unix_ms(t.UniversalTime)).unwrap_or_else(|_| crate::utils::get_now_ms());
    let icon = app_icon(&aumid, display.as_ref());
    Some(NotchNotification { id, app, aumid, title, body, icon, time })
}

fn png_data_url(bytes: &[u8]) -> Option<String> {
    use base64::Engine;
    let png = if bytes.starts_with(b"\x89PNG") {
        bytes.to_vec()
    } else {
        let img = image::load_from_memory(bytes).ok()?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        out.into_inner()
    };
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

fn logo(display: &AppDisplayInfo) -> Option<String> {
    use windows::Storage::Streams::DataReader;
    let stream = display.GetLogo(windows::Foundation::Size { Width: 64.0, Height: 64.0 }).ok()?.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > 4 * 1024 * 1024 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    let loaded = reader.LoadAsync(size as u32).ok()?.get().ok()?;
    let mut bytes = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    png_data_url(&bytes)
}

/// Packaged apps hand out a logo; classic desktop apps don't, so their icon comes from the shell.
fn app_icon(aumid: &str, display: Option<&AppDisplayInfo>) -> Option<String> {
    let cache = ICONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(aumid).cloned()) {
        return hit;
    }
    let icon = display.and_then(logo).or_else(|| crate::commands::icon_from_aumid(aumid));
    if let Ok(mut c) = cache.lock() {
        c.insert(aumid.to_string(), icon.clone());
    }
    icon
}

fn access_label(status: Option<UserNotificationListenerAccessStatus>) -> &'static str {
    match status {
        Some(UserNotificationListenerAccessStatus::Allowed) => "allowed",
        Some(UserNotificationListenerAccessStatus::Denied) => "denied",
        Some(_) => "unspecified",
        None => "unavailable",
    }
}

fn com_mta() {
    unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED); }
}

#[tauri::command]
pub async fn notch_notifications_access() -> String {
    tauri::async_runtime::spawn_blocking(|| {
        com_mta();
        access_label(UserNotificationListener::Current().and_then(|l| l.GetAccessStatus()).ok()).to_string()
    })
    .await
    .unwrap_or_else(|_| "unavailable".into())
}

/// Shows the Windows consent prompt when the user hasn't decided yet; Nectar never flips the setting itself.
/// The request has to start on the UI thread (Windows shows no prompt otherwise); only the wait runs off it.
#[tauri::command]
pub async fn notch_notifications_request_access(app: AppHandle) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let started = app.run_on_main_thread(move || {
        let _ = tx.send(UserNotificationListener::Current().and_then(|l| l.RequestAccessAsync()).ok());
    });
    let status = match started {
        Ok(()) => tauri::async_runtime::spawn_blocking(move || {
            com_mta();
            let op = rx.recv_timeout(Duration::from_secs(10)).ok().flatten();
            access_label(op.and_then(|op| op.get().ok())).to_string()
        })
        .await
        .unwrap_or_else(|_| "unavailable".into()),
        Err(_) => "unavailable".into(),
    };
    kick();
    status
}

#[tauri::command]
pub fn notch_notifications_open_privacy() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    unsafe {
        let _ = ShellExecuteW(None, windows::core::w!("open"), windows::core::w!("ms-settings:privacy-notifications"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

#[tauri::command]
pub async fn notch_notification_dismiss(id: u32) {
    if id >= LOCAL_ID_BASE {
        return;
    }
    let _ = tauri::async_runtime::spawn_blocking(move || {
        com_mta();
        if let Ok(l) = UserNotificationListener::Current() {
            let _ = l.RemoveNotification(id);
        }
    })
    .await;
}

/// Words of an AppUserModelID, to match classic apps such as `Microsoft.Office.OUTLOOK.EXE.15` by exe name.
pub fn aumid_words(aumid: &str) -> Vec<String> {
    aumid
        .split(|c: char| matches!(c, '.' | '!' | '_' | '\\' | '/' | ' ' | '-'))
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

pub(crate) fn window_for_app(aumid: &str) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        let tool = (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0;
        if IsWindowVisible(hwnd).as_bool() && !tool && GetWindowTextLengthW(hwnd) > 0 {
            list.push(hwnd);
        }
        windows::core::BOOL(1)
    }
    let mut windows: Vec<HWND> = Vec::new();
    unsafe { let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut Vec<HWND> as isize)); }
    let mine = std::process::id();
    let windows: Vec<HWND> = windows
        .into_iter()
        .filter(|&h| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            pid != mine
        })
        .collect();
    if let Some(h) = windows.iter().find(|&&h| crate::utils::get_window_app_user_model_id(h).is_some_and(|id| id.eq_ignore_ascii_case(aumid))) {
        return Some(h.0 as isize);
    }
    let words = aumid_words(aumid);
    windows
        .iter()
        .find(|&&h| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            unsafe { crate::commands::process_image_path(pid) }
                .and_then(|p| Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()))
                .is_some_and(|stem| words.contains(&stem))
        })
        .map(|h| h.0 as isize)
}

/// Windows' own notifiers (security, devices, calls, Action Center) always keep their banners.
fn is_system_aumid(aumid: &str) -> bool {
    aumid.starts_with("Windows.") || aumid.starts_with("Microsoft.Windows.")
}

fn launch_app(aumid: &str) {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, ShellExecuteW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if aumid.is_empty() || aumid.starts_with("Windows.SystemToast") {
        return;
    }
    let target = HSTRING::from(format!(r"shell:AppsFolder\{aumid}"));
    // Only launch what the shell knows; an unknown id would open an Explorer error instead.
    if unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&target, None) }.is_err() {
        return;
    }
    unsafe {
        let _ = ShellExecuteW(None, windows::core::w!("open"), &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Best effort: a window of the app that sent the toast, else the app itself. A Claude desktop
/// toast opens the session its title names.
#[tauri::command]
pub async fn notch_notification_open(aumid: String, title: Option<String>) {
    if crate::claude_desktop::is_claude_aumid(&aumid) {
        if let Some(title) = title {
            let local = tauri::async_runtime::spawn_blocking(move || crate::claude_desktop::by_toast_title(&title)).await.ok().flatten();
            if let Some(id) = local {
                if crate::claude_desktop::open(&id, None).await {
                    return;
                }
            }
        }
    }
    let found = tauri::async_runtime::spawn_blocking({
        let aumid = aumid.clone();
        move || {
            com_mta();
            let hwnd = window_for_app(&aumid);
            if hwnd.is_none() {
                launch_app(&aumid);
            }
            hwnd
        }
    })
    .await
    .ok()
    .flatten();
    if let Some(hwnd) = found {
        crate::commands::bring_window_forward(hwnd).await;
    }
}

/// Apps with toasts in Action Center plus apps Windows keeps notification settings for, for the exclude list.
#[tauri::command]
pub async fn notch_notification_apps() -> Vec<NotificationApp> {
    tauri::async_runtime::spawn_blocking(|| {
        com_mta();
        let mut apps: BTreeMap<String, NotificationApp> = BTreeMap::new();
        if let Ok(list) = UserNotificationListener::Current().and_then(|l| l.GetNotificationsAsync(NotificationKinds::Toast)).and_then(|op| op.get()) {
            for i in 0..list.Size().unwrap_or(0) {
                let Ok(n) = list.GetAt(i) else { continue };
                let Some(info) = n.AppInfo().ok() else { continue };
                let Some(aumid) = info.AppUserModelId().ok().map(|s| s.to_string()).filter(|s| !s.is_empty()) else { continue };
                if apps.contains_key(&aumid.to_ascii_lowercase()) {
                    continue;
                }
                let display = info.DisplayInfo().ok();
                let name = display.as_ref().and_then(|d| d.DisplayName().ok()).map(|s| s.to_string()).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| aumid.clone());
                let icon = app_icon(&aumid, display.as_ref());
                apps.insert(aumid.to_ascii_lowercase(), NotificationApp { aumid, name, icon });
            }
        }
        for aumid in banners::list_apps(BANNERS_KEY) {
            if is_system_aumid(&aumid) || apps.contains_key(&aumid.to_ascii_lowercase()) {
                continue;
            }
            let name = aumid.rsplit(['!', '\\']).next().unwrap_or(&aumid).to_string();
            apps.insert(aumid.to_ascii_lowercase(), NotificationApp { name, icon: crate::commands::icon_from_aumid(&aumid), aumid });
        }
        let mut out: Vec<NotificationApp> = apps.into_values().filter(|a| !is_own(&a.aumid, &a.name)).collect();
        out.sort_by_key(|a| a.name.to_lowercase());
        out
    })
    .await
    .unwrap_or_default()
}

/// Puts back every banner Nectar turned off (quit, uninstall). The worker turns them off again on the next start.
pub fn restore_banners(config_dir: &Path) {
    banners::restore_all(BANNERS_KEY, &config_dir.join(BACKUP_FILE));
}

pub fn restore_banners_for(app: &AppHandle) {
    if let Ok(dir) = app.path().app_config_dir() {
        restore_banners(&dir);
    }
}

/// Hiding Windows banners through the user's own per-app setting (`ShowBanner` = 0), keeping toasts in
/// Notification Center. The value each app had before is saved first and written back exactly.
mod banners {
    use super::*;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        KEY_SET_VALUE, REG_DWORD, REG_VALUE_TYPE,
    };

    const VALUE: &str = "ShowBanner";

    #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
    #[serde(tag = "type", content = "value")]
    pub enum Prior {
        Missing,
        Dword(u32),
    }

    pub type Backup = BTreeMap<String, Prior>;

    static STATE: Mutex<Option<Backup>> = Mutex::new(None);
    static CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// System toasts (security, devices, calls) keep their banners; so do Nectar's own and excluded apps.
    pub fn wanted(apps: &[String], cfg: &Config) -> BTreeSet<String> {
        apps.iter()
            .filter(|a| !super::is_system_aumid(a) && !is_own(a, "") && !cfg.excludes(a))
            .cloned()
            .collect()
    }

    /// Which apps to turn off now and which to give back.
    pub fn plan(backup: &Backup, wanted: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
        let apply = wanted.iter().filter(|a| !backup.contains_key(*a)).cloned().collect();
        let restore = backup.keys().filter(|a| !wanted.contains(*a)).cloned().collect();
        (apply, restore)
    }

    fn open(base: &str, app: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
        let path = wide(&format!(r"{base}\{app}"));
        let mut key = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), access, &mut key) }.is_ok().then_some(key)
    }

    /// None when the app key can't be read or `ShowBanner` isn't a DWORD; such apps are left alone.
    pub fn read(base: &str, app: &str) -> Option<Prior> {
        let key = open(base, app, KEY_READ)?;
        let name = wide(VALUE);
        let mut buf = [0u8; 4];
        let mut size = buf.len() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let r = unsafe { RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, Some(&mut kind), Some(buf.as_mut_ptr()), Some(&mut size)) };
        unsafe { let _ = RegCloseKey(key); }
        if r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            Some(Prior::Missing)
        } else {
            (r.is_ok() && kind == REG_DWORD && size == 4).then(|| Prior::Dword(u32::from_le_bytes(buf)))
        }
    }

    pub fn write(base: &str, app: &str, value: Prior) -> bool {
        let Some(key) = open(base, app, KEY_SET_VALUE) else { return false };
        let name = wide(VALUE);
        let ok = match value {
            Prior::Dword(v) => unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_DWORD, Some(&v.to_le_bytes())) }.is_ok(),
            Prior::Missing => {
                unsafe { let _ = RegDeleteValueW(key, PCWSTR(name.as_ptr())); }
                true
            }
        };
        unsafe { let _ = RegCloseKey(key); }
        ok
    }

    pub fn list_apps(base: &str) -> Vec<String> {
        let path = wide(base);
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), KEY_READ, &mut key) }.is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let r = unsafe { RegEnumKeyExW(key, i, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
            if r.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        unsafe { let _ = RegCloseKey(key); }
        // An app id with backslashes becomes nested keys; the empty parent is not an app.
        out.retain(|app| has_values(base, app));
        out
    }

    fn has_values(base: &str, app: &str) -> bool {
        use windows::Win32::System::Registry::RegQueryInfoKeyW;
        let Some(key) = open(base, app, KEY_READ) else { return false };
        let mut values = 0u32;
        let ok = unsafe { RegQueryInfoKeyW(key, None, None, None, None, None, None, Some(&mut values), None, None, None, None) }.is_ok();
        unsafe { let _ = RegCloseKey(key); }
        ok && values > 0
    }

    /// Turns `wanted` apps off and gives every other saved app its old value back. Returns true when the backup changed.
    /// The originals are handed to `persist` before any 0 is written, so a kill in between can't lose them; when
    /// `persist` fails nothing new is hidden.
    pub fn apply_plan(base: &str, backup: &mut Backup, wanted: &BTreeSet<String>, persist: &mut dyn FnMut(&Backup) -> bool) -> bool {
        let (apply, restore) = plan(backup, wanted);
        let mut changed = false;
        for app in restore {
            let Some(&prior) = backup.get(&app) else { continue };
            let done = match read(base, &app) {
                // Still Nectar's 0: put the original back. A locked key keeps its entry for the next try.
                Some(Prior::Dword(0)) => write(base, &app, prior),
                // Gone, or changed in Windows Settings while hidden: the user's value stays.
                _ => true,
            };
            if done {
                backup.remove(&app);
                changed = true;
            }
        }
        let priors: Vec<(String, Prior)> = apply.into_iter().filter_map(|app| read(base, &app).map(|p| (app, p))).collect();
        if !priors.is_empty() {
            backup.extend(priors.iter().cloned());
            if !persist(backup) {
                for (app, _) in &priors {
                    backup.remove(app);
                }
                return changed;
            }
            changed = true;
        }
        for (app, _) in priors {
            if !write(base, &app, Prior::Dword(0)) {
                backup.remove(&app);
            }
        }
        changed
    }

    /// None when the file exists but can't be read or parsed: its originals are unknown, so nothing may be hidden
    /// (the next pass would save Nectar's own 0 as the original) and the file is kept.
    pub fn load(path: &Path) -> Option<Backup> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Backup::new()),
            Err(_) => None,
        }
    }

    /// Written to a temp file and renamed over the old one, so a kill mid-write leaves the previous backup intact.
    pub fn save(path: &Path, backup: &Backup) -> bool {
        use std::io::Write;
        if backup.is_empty() {
            return match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            };
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(backup) else { return false };
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::File::create(&tmp).and_then(|mut f| {
            f.write_all(json.as_bytes())?;
            f.sync_all()
        });
        // std::fs::rename replaces an existing file on Windows (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
        if written.is_ok() && std::fs::rename(&tmp, path).is_ok() {
            return true;
        }
        let _ = std::fs::remove_file(&tmp);
        false
    }

    pub fn sync(app: &AppHandle, cfg: Option<&Config>) {
        let Ok(dir) = app.path().app_config_dir() else { return };
        let path = dir.join(BACKUP_FILE);
        let Ok(mut state) = STATE.lock() else { return };
        // Checked under the lock: once restore_all ran (quit, uninstall), nothing hides banners again.
        if CLOSED.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if state.is_none() {
            // A corrupt backup: hide and restore nothing until the file is readable again (or removed by hand).
            let Some(loaded) = load(&path) else { return };
            *state = Some(loaded);
        }
        let Some(backup) = state.as_mut() else { return };
        let wanted = match cfg {
            Some(cfg) => wanted(&list_apps(BANNERS_KEY), cfg),
            None if backup.is_empty() => return,
            None => BTreeSet::new(),
        };
        if apply_plan(BANNERS_KEY, backup, &wanted, &mut |b| save(&path, b)) {
            save(&path, backup);
        }
    }

    pub fn restore_all(base: &str, path: &Path) {
        let mut state = STATE.lock().ok();
        CLOSED.store(true, std::sync::atomic::Ordering::SeqCst);
        let in_memory = state.as_mut().and_then(|s| s.take()).filter(|b| !b.is_empty());
        let Some(mut backup) = in_memory.or_else(|| load(path)) else { return };
        apply_plan(base, &mut backup, &BTreeSet::new(), &mut |_| true);
        save(path, &backup);
        if let Some(s) = state.as_mut() {
            **s = Some(backup);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_new_and_gone_ids() {
        let seen: HashSet<u32> = [1, 2, 3].into();
        let current: HashSet<u32> = [2, 3, 7, 5].into();
        assert_eq!(diff_ids(&seen, &current), (vec![5, 7], vec![1]));
        assert_eq!(diff_ids(&current, &current), (vec![], vec![]));
    }

    #[test]
    fn a_burst_keeps_only_the_newest() {
        assert_eq!(newest(&[1, 2, 3, 4, 5, 6, 7], 5), &[3, 4, 5, 6, 7]);
        assert_eq!(newest(&[4], 5), &[4]);
        assert!(newest(&[], 5).is_empty());
    }

    #[test]
    fn first_line_is_the_title() {
        let (title, body) = split_text(vec![" Ayşe ".into(), "".into(), "Lunch?".into(), "See you at 1".into()]);
        assert_eq!(title, "Ayşe");
        assert_eq!(body, "Lunch?\nSee you at 1");
        assert_eq!(split_text(vec![]), (String::new(), String::new()));
        let (_, long) = split_text(vec!["t".into(), "ğ".repeat(MAX_BODY + 10)]);
        assert_eq!(long.chars().count(), MAX_BODY + 1);
    }

    #[test]
    fn winrt_ticks_to_unix_ms() {
        assert_eq!(ticks_to_unix_ms(116_444_736_000_000_000), 0);
        assert_eq!(ticks_to_unix_ms(116_444_736_000_000_000 + 15_000_000), 1500);
    }

    #[test]
    fn exclude_list_parses_json() {
        assert_eq!(parse_exclude(Some(r#"["Chrome", " ", "Microsoft.Office.OUTLOOK.EXE.15"]"#)), vec!["Chrome", "Microsoft.Office.OUTLOOK.EXE.15"]);
        assert!(parse_exclude(Some("not json")).is_empty());
        assert!(parse_exclude(None).is_empty());
        let cfg = Config { on: true, hide: true, exclude: vec!["chrome".into()] };
        assert!(cfg.excludes("Chrome"));
        assert!(!cfg.excludes("Edge"));
    }

    #[test]
    fn own_toasts_are_recognised() {
        assert!(is_own("com.kua8.nectar", ""));
        assert!(is_own("x", "Nectar"));
        assert!(!is_own("Chrome", "Google Chrome"));
    }

    #[test]
    fn aumid_words_match_exe_names() {
        assert!(aumid_words("Microsoft.Office.OUTLOOK.EXE.15").contains(&"outlook".to_string()));
        assert!(aumid_words("Chrome").contains(&"chrome".to_string()));
        assert!(!aumid_words("Microsoft.Office.OUTLOOK.EXE.15").contains(&"15".to_string()));
    }

    #[test]
    fn banner_plan_keeps_system_own_and_excluded_apps() {
        let apps: Vec<String> = ["Chrome", "Windows.SystemToast.Calling", "com.kua8.nectar", "Slack", "Microsoft.Windows.Explorer", "Windows.Defender.SecurityCenter", "Windows.ActionCenter.SmartOptOut"].map(String::from).into();
        let cfg = Config { on: true, hide: true, exclude: vec!["slack".into()] };
        let wanted = banners::wanted(&apps, &cfg);
        assert_eq!(wanted.into_iter().collect::<Vec<_>>(), vec!["Chrome".to_string()]);

        let mut backup = banners::Backup::new();
        backup.insert("Old".into(), banners::Prior::Missing);
        backup.insert("Chrome".into(), banners::Prior::Dword(1));
        let wanted: BTreeSet<String> = ["Chrome".to_string(), "New".to_string()].into();
        assert_eq!(banners::plan(&backup, &wanted), (vec!["New".to_string()], vec!["Old".to_string()]));
    }

    // Uses a scratch key under HKCU\Software\NectarTest, never the real notification settings, and removes it.
    #[test]
    fn banners_are_restored_exactly() {
        use windows::Win32::System::Registry::{RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_DWORD, REG_OPTION_NON_VOLATILE};
        let base = format!(r"Software\NectarTest\Banners-{}", std::process::id());
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        // AppB has other values but no ShowBanner; Parent only holds a nested key, like an app id with backslashes.
        for (app, name, value) in [("AppA", "ShowBanner", Some(1u32)), ("AppB", "LastNotificationAddedTime", Some(5)), ("AppC", "ShowBanner", Some(0)), (r"Parent\Child", "ShowBanner", None)] {
            let path = wide(&format!(r"{base}\{app}"));
            let mut key = HKEY::default();
            unsafe {
                assert!(RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_ALL_ACCESS, None, &mut key, None).is_ok());
                if let Some(v) = value {
                    let name = wide(name);
                    let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_DWORD, Some(&v.to_le_bytes()));
                }
                let _ = windows::Win32::System::Registry::RegCloseKey(key);
            }
        }
        let mut apps = banners::list_apps(&base);
        apps.sort();
        assert_eq!(apps, vec!["AppA", "AppB", "AppC"]);

        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = apps.iter().cloned().collect();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        for app in &apps {
            assert_eq!(banners::read(&base, app), Some(banners::Prior::Dword(0)));
        }
        // Applying again must not overwrite the saved originals with Nectar's own zeros.
        assert!(!banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.get("AppA"), Some(&banners::Prior::Dword(1)));

        // Excluding AppB later gives only AppB back.
        let fewer: BTreeSet<String> = ["AppA".to_string(), "AppC".to_string()].into();
        banners::apply_plan(&base, &mut backup, &fewer, &mut |_| true);
        assert_eq!(banners::read(&base, "AppB"), Some(banners::Prior::Missing));

        let dir = std::env::temp_dir().join(format!("nectar-banners-{}", std::process::id()));
        let file = dir.join("notification-banners.json");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(&file, serde_json::to_string(&backup).unwrap()).unwrap();
        banners::restore_all(&base, &file);
        assert_eq!(banners::read(&base, "AppA"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "AppB"), Some(banners::Prior::Missing));
        assert_eq!(banners::read(&base, "AppC"), Some(banners::Prior::Dword(0)));
        assert!(!file.exists());

        let _ = std::fs::remove_dir_all(&dir);
        let root = wide(r"Software\NectarTest");
        unsafe { let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(root.as_ptr())); }
    }

    #[test]
    fn diff_edge_cases() {
        let empty: HashSet<u32> = HashSet::new();
        let some: HashSet<u32> = [4, 9].into();
        assert_eq!(diff_ids(&empty, &empty), (vec![], vec![]));
        assert_eq!(diff_ids(&empty, &some), (vec![4, 9], vec![]));
        assert_eq!(diff_ids(&some, &empty), (vec![], vec![4, 9]));
        assert!(newest(&[1, 2, 3], 0).is_empty());
        assert_eq!(newest(&[1, 2, 3], 3), &[1, 2, 3]);
        assert_eq!(newest(&[1, 2, 3], 100), &[1, 2, 3]);
    }

    #[test]
    fn text_limits_are_exact_and_char_based() {
        assert_eq!(clamp_chars("abc", 3), "abc");
        assert_eq!(clamp_chars("abcd", 3), "abc…");
        // Multi-byte characters must not be cut in the middle.
        assert_eq!(clamp_chars("ğüşiöç", 4), "ğüşi…");
        let (title, body) = split_text(vec!["x".repeat(MAX_TITLE), "y".repeat(MAX_BODY)]);
        assert_eq!(title.chars().count(), MAX_TITLE);
        assert_eq!(body.chars().count(), MAX_BODY);
        let (title, _) = split_text(vec!["x".repeat(MAX_TITLE + 1)]);
        assert_eq!(title.chars().count(), MAX_TITLE + 1);
        assert!(title.ends_with('…'));
        assert_eq!(split_text(vec!["  ".into(), "\n".into()]), (String::new(), String::new()));
        assert_eq!(split_text(vec!["only title".into()]), ("only title".to_string(), String::new()));
    }

    #[test]
    fn ticks_before_the_unix_epoch_are_negative() {
        assert_eq!(ticks_to_unix_ms(116_444_736_000_000_000 - 10_000), -1);
    }

    #[test]
    fn own_toast_detection_edge_cases() {
        assert!(is_own("COM.KUA8.NECTAR!App", ""));
        assert!(is_own("", "NECTAR"));
        assert!(!is_own("", "Nectarine"));
        assert!(!is_own("", ""));
    }

    #[test]
    fn exclude_list_rejects_wrong_json_shapes() {
        assert!(parse_exclude(Some("")).is_empty());
        assert!(parse_exclude(Some(r#"{"a":1}"#)).is_empty());
        assert!(parse_exclude(Some("[1,2]")).is_empty());
        assert_eq!(parse_exclude(Some(r#"["  Slack  "]"#)), vec!["Slack"]);
    }

    #[test]
    fn banner_wanted_is_case_insensitive_for_excludes_and_handles_empty() {
        let cfg = Config { on: true, hide: true, exclude: vec!["SLACK".into()] };
        assert!(banners::wanted(&[], &cfg).is_empty());
        let apps: Vec<String> = ["slack", "Teams", "Teams"].map(String::from).into();
        assert_eq!(banners::wanted(&apps, &cfg).into_iter().collect::<Vec<_>>(), vec!["Teams".to_string()]);
    }

    #[test]
    fn banner_plan_is_empty_when_nothing_changes() {
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = BTreeSet::new();
        assert_eq!(banners::plan(&backup, &wanted), (vec![], vec![]));
        backup.insert("A".into(), banners::Prior::Dword(1));
        let wanted: BTreeSet<String> = ["A".to_string()].into();
        assert_eq!(banners::plan(&backup, &wanted), (vec![], vec![]));
        // Turning the feature off wants nothing: everything saved is given back.
        assert_eq!(banners::plan(&backup, &BTreeSet::new()), (vec![], vec!["A".to_string()]));
    }

    #[test]
    fn banner_backup_survives_a_json_round_trip() {
        let mut backup = banners::Backup::new();
        backup.insert("A".into(), banners::Prior::Missing);
        backup.insert("B".into(), banners::Prior::Dword(0));
        backup.insert("C".into(), banners::Prior::Dword(u32::MAX));
        let json = serde_json::to_string(&backup).unwrap();
        let back: banners::Backup = serde_json::from_str(&json).unwrap();
        assert_eq!(back, backup);
    }

    // Every test gets its own top-level scratch key, so deleting all of it cannot race the other tests.
    const SCRATCH_ROOT: &str = r"Software\NectarTestB";

    fn scratch_app(base: &str, app: &str, value: Option<u32>) {
        use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_DWORD, REG_OPTION_NON_VOLATILE};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let path = wide(&format!(r"{base}\{app}"));
        let mut key = HKEY::default();
        unsafe {
            assert!(RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_ALL_ACCESS, None, &mut key, None).is_ok());
            // Always give the key one value so list_apps counts it as an app.
            let other = wide("Other");
            let _ = RegSetValueExW(key, PCWSTR(other.as_ptr()), Some(0), REG_DWORD, Some(&1u32.to_le_bytes()));
            if let Some(v) = value {
                let name = wide("ShowBanner");
                let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_DWORD, Some(&v.to_le_bytes()));
            }
            let _ = RegCloseKey(key);
        }
    }

    fn drop_scratch(path: &str) {
        use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
        let path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr())); }
    }

    // Scratch keys only (own root, so the parallel test above cannot delete them); real settings are never touched.
    #[test]
    fn banner_restore_handles_an_app_that_vanished() {
        let base = format!(r"{SCRATCH_ROOT}-Vanish-{}", std::process::id());
        scratch_app(&base, "Gone", Some(1));
        scratch_app(&base, "Stays", None);
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["Gone".to_string(), "Stays".to_string()].into();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.get("Gone"), Some(&banners::Prior::Dword(1)));
        assert_eq!(backup.get("Stays"), Some(&banners::Prior::Missing));

        // The app is uninstalled while hidden; restoring must not panic or keep the entry forever.
        drop_scratch(&format!(r"{base}\Gone"));
        assert!(banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true));
        assert!(backup.is_empty());
        assert_eq!(banners::read(&base, "Gone"), None);
        assert_eq!(banners::read(&base, "Stays"), Some(banners::Prior::Missing));
        drop_scratch(&base);
    }

    #[test]
    fn banner_apply_skips_apps_whose_key_cannot_be_opened() {
        let base = format!(r"{SCRATCH_ROOT}-Skip-{}", std::process::id());
        scratch_app(&base, "Real", Some(1));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["Real".to_string(), "Ghost".to_string()].into();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.len(), 1);
        assert!(!backup.contains_key("Ghost"));
        assert_eq!(banners::read(&base, "Ghost"), None);
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "Real"), Some(banners::Prior::Dword(1)));
        drop_scratch(&base);
    }

    #[test]
    fn banner_restore_all_with_a_corrupt_or_missing_file_does_nothing() {
        let base = format!(r"{SCRATCH_ROOT}-Corrupt-{}", std::process::id());
        scratch_app(&base, "App", Some(1));
        let dir = std::env::temp_dir().join(format!("nectar-banners-corrupt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        banners::restore_all(&base, &dir.join("none.json"));
        assert_eq!(banners::read(&base, "App"), Some(banners::Prior::Dword(1)));
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        banners::restore_all(&base, &bad);
        assert_eq!(banners::read(&base, "App"), Some(banners::Prior::Dword(1)));
        assert!(bad.exists(), "a corrupt backup is kept, not replaced by an empty one");
        let _ = std::fs::remove_dir_all(&dir);
        drop_scratch(&base);
    }

    #[test]
    fn banner_originals_are_persisted_before_any_zero_is_written() {
        let base = format!(r"{SCRATCH_ROOT}-Ahead-{}", std::process::id());
        scratch_app(&base, "A", Some(1));
        scratch_app(&base, "B", None);
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
        let mut calls = 0;
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |b| {
            calls += 1;
            assert_eq!(b.get("A"), Some(&banners::Prior::Dword(1)));
            assert_eq!(b.get("B"), Some(&banners::Prior::Missing));
            // Nothing is hidden yet when the backup is handed over.
            assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
            assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Missing));
            true
        }));
        assert_eq!(calls, 1);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(0)));
        // A pass with nothing new to hide doesn't write the file again.
        banners::apply_plan(&base, &mut backup, &wanted, &mut |_| panic!("no new originals"));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Missing));
        drop_scratch(&base);
    }

    #[test]
    fn banner_values_of_another_type_are_left_alone() {
        use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ};
        let base = format!(r"{SCRATCH_ROOT}-Type-{}", std::process::id());
        scratch_app(&base, "Odd", None);
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let path = wide(&format!(r"{base}\Odd"));
        let name = wide("ShowBanner");
        let data: Vec<u8> = wide("1").iter().flat_map(|c| c.to_le_bytes()).collect();
        let mut key = HKEY::default();
        unsafe {
            assert!(RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), KEY_SET_VALUE, &mut key).is_ok());
            let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(&data));
            let _ = RegCloseKey(key);
        }
        assert_eq!(banners::read(&base, "Odd"), None);
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["Odd".to_string()].into();
        assert!(!banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert!(backup.is_empty());
        drop_scratch(&base);
    }

    #[test]
    fn banner_restore_all_writes_back_values_from_the_backup_file() {
        let base = format!(r"{SCRATCH_ROOT}-FromFile-{}", std::process::id());
        // Simulates a crash: banners are at 0 and only the file remembers the originals.
        scratch_app(&base, "A", Some(0));
        scratch_app(&base, "B", Some(0));
        let mut backup = banners::Backup::new();
        backup.insert("A".into(), banners::Prior::Dword(1));
        backup.insert("B".into(), banners::Prior::Missing);
        let dir = std::env::temp_dir().join(format!("nectar-banners-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("notification-banners.json");
        std::fs::write(&file, serde_json::to_string(&backup).unwrap()).unwrap();
        banners::restore_all(&base, &file);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Missing));
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
        drop_scratch(&base);
    }
    #[test]
    fn diff_with_empty_sides_and_extreme_ids() {
        let none: HashSet<u32> = HashSet::new();
        let some: HashSet<u32> = [u32::MAX, 0, 9, 3].into();
        assert_eq!(diff_ids(&none, &some), (vec![0, 3, 9, u32::MAX], vec![]));
        assert_eq!(diff_ids(&some, &none), (vec![], vec![0, 3, 9, u32::MAX]));
        assert_eq!(diff_ids(&none, &none), (vec![], vec![]));
        let disjoint_a: HashSet<u32> = [1, 2].into();
        let disjoint_b: HashSet<u32> = [3, 4].into();
        assert_eq!(diff_ids(&disjoint_a, &disjoint_b), (vec![3, 4], vec![1, 2]));
    }

    #[test]
    fn newest_with_zero_and_oversized_limits() {
        assert!(newest(&[1, 2, 3], 0).is_empty());
        assert_eq!(newest(&[1, 2, 3], 3), &[1, 2, 3]);
        assert_eq!(newest(&[1, 2, 3], usize::MAX), &[1, 2, 3]);
    }

    #[test]
    fn text_split_drops_blank_parts_and_keeps_a_lone_title() {
        assert_eq!(split_text(vec!["Only title".into()]), ("Only title".to_string(), String::new()));
        assert_eq!(split_text(vec!["  ".into(), "".into()]), (String::new(), String::new()));
        let (title, body) = split_text(vec!["\n".into(), " T ".into(), " ".into(), "B1".into(), "B2".into()]);
        assert_eq!((title.as_str(), body.as_str()), ("T", "B1\nB2"));
        let (long_title, _) = split_text(vec!["é".repeat(MAX_TITLE + 3)]);
        assert_eq!(long_title.chars().count(), MAX_TITLE + 1);
        assert!(long_title.ends_with('…'));
        let (exact, _) = split_text(vec!["é".repeat(MAX_TITLE)]);
        assert_eq!(exact.chars().count(), MAX_TITLE);
    }

    #[test]
    fn banner_prior_json_shape_is_stable() {
        // The backup file outlives app versions, so its shape must not change by accident.
        assert_eq!(serde_json::to_string(&banners::Prior::Missing).unwrap(), r#"{"type":"Missing"}"#);
        assert_eq!(serde_json::to_string(&banners::Prior::Dword(1)).unwrap(), r#"{"type":"Dword","value":1}"#);
        let back: banners::Backup = serde_json::from_str(r#"{"App":{"type":"Dword","value":4294967295},"B":{"type":"Missing"}}"#).unwrap();
        assert_eq!(back.get("App"), Some(&banners::Prior::Dword(u32::MAX)));
        assert_eq!(back.get("B"), Some(&banners::Prior::Missing));
    }

    #[test]
    fn banner_zero_and_max_originals_come_back_exactly() {
        let base = format!(r"{SCRATCH_ROOT}-Extremes-{}", std::process::id());
        scratch_app(&base, "AlreadyOff", Some(0));
        scratch_app(&base, "Big", Some(u32::MAX));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["AlreadyOff".to_string(), "Big".to_string()].into();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.get("AlreadyOff"), Some(&banners::Prior::Dword(0)));
        assert_eq!(backup.get("Big"), Some(&banners::Prior::Dword(u32::MAX)));
        assert_eq!(banners::read(&base, "Big"), Some(banners::Prior::Dword(0)));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "AlreadyOff"), Some(banners::Prior::Dword(0)));
        assert_eq!(banners::read(&base, "Big"), Some(banners::Prior::Dword(u32::MAX)));
        assert!(backup.is_empty());
        drop_scratch(&base);
    }

    #[test]
    fn banner_original_is_never_overwritten_by_a_later_pass() {
        let base = format!(r"{SCRATCH_ROOT}-Keep-{}", std::process::id());
        scratch_app(&base, "App", Some(1));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["App".to_string()].into();
        banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true);
        // Several more passes with the app still wanted (the registry now holds our 0).
        for _ in 0..3 {
            assert!(!banners::apply_plan(&base, &mut backup, &wanted, &mut |_| panic!("must not persist again")));
        }
        assert_eq!(backup.get("App"), Some(&banners::Prior::Dword(1)));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "App"), Some(banners::Prior::Dword(1)));
        drop_scratch(&base);
    }

    #[test]
    fn banner_hide_restore_hide_picks_up_a_changed_original() {
        let base = format!(r"{SCRATCH_ROOT}-Cycle-{}", std::process::id());
        scratch_app(&base, "App", Some(1));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["App".to_string()].into();
        banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true);
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert!(backup.is_empty());
        // The user changes the setting while Nectar is not hiding anything; the next hide must save the new value.
        assert!(banners::write(&base, "App", banners::Prior::Dword(7)));
        banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true);
        assert_eq!(backup.get("App"), Some(&banners::Prior::Dword(7)));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "App"), Some(banners::Prior::Dword(7)));
        drop_scratch(&base);
    }

    #[test]
    fn banner_apps_leaving_the_wanted_set_are_restored_one_by_one() {
        let base = format!(r"{SCRATCH_ROOT}-Partial-{}", std::process::id());
        scratch_app(&base, "A", Some(1));
        scratch_app(&base, "B", Some(1));
        let mut backup = banners::Backup::new();
        let both: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
        banners::apply_plan(&base, &mut backup, &both, &mut |_| true);
        // B joins the exclude list: only B gets its banner back.
        let only_a: BTreeSet<String> = ["A".to_string()].into();
        assert!(banners::apply_plan(&base, &mut backup, &only_a, &mut |_| true));
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(0)));
        assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Dword(1)));
        assert_eq!(backup.keys().collect::<Vec<_>>(), vec!["A"]);
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        drop_scratch(&base);
    }

    #[test]
    fn banner_restore_all_twice_is_harmless_and_drops_vanished_entries_from_the_file() {
        let base = format!(r"{SCRATCH_ROOT}-Twice-{}", std::process::id());
        scratch_app(&base, "Here", Some(0));
        let mut backup = banners::Backup::new();
        backup.insert("Here".into(), banners::Prior::Dword(1));
        backup.insert("Uninstalled".into(), banners::Prior::Dword(1));
        let dir = std::env::temp_dir().join(format!("nectar-banners-twice-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("notification-banners.json");
        std::fs::write(&file, serde_json::to_string(&backup).unwrap()).unwrap();
        banners::restore_all(&base, &file);
        assert_eq!(banners::read(&base, "Here"), Some(banners::Prior::Dword(1)));
        assert!(!file.exists(), "nothing left to remember, so the file goes away");
        // The user then changes the value; a second restore must not undo it.
        assert!(banners::write(&base, "Here", banners::Prior::Dword(5)));
        banners::restore_all(&base, &file);
        assert_eq!(banners::read(&base, "Here"), Some(banners::Prior::Dword(5)));
        let _ = std::fs::remove_dir_all(&dir);
        drop_scratch(&base);
    }

    #[test]
    fn banner_list_apps_skips_empty_parents_and_missing_bases() {
        let base = format!(r"{SCRATCH_ROOT}-List-{}", std::process::id());
        assert!(banners::list_apps(&base).is_empty());
        scratch_app(&base, "Plain.App", None);
        scratch_app(&base, r"Nested\Child", None);
        let apps = banners::list_apps(&base);
        assert!(apps.contains(&"Plain.App".to_string()));
        assert!(!apps.contains(&"Nested".to_string()), "a parent key without values is not an app");
        drop_scratch(&base);
        assert!(banners::list_apps(&base).is_empty());
    }

    #[test]
    fn banner_backup_file_is_saved_atomically_and_corrupt_files_load_as_unknown() {
        let dir = std::env::temp_dir().join(format!("nectar-banners-atomic-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("notification-banners.json");
        assert_eq!(banners::load(&file), Some(banners::Backup::new()));
        let mut backup = banners::Backup::new();
        backup.insert("A".into(), banners::Prior::Dword(1));
        assert!(banners::save(&file, &backup));
        backup.insert("B".into(), banners::Prior::Missing);
        assert!(banners::save(&file, &backup), "replaces the existing file");
        assert_eq!(banners::load(&file), Some(backup.clone()));
        assert!(!dir.join("notification-banners.json.tmp").exists());
        std::fs::write(&file, r#"{"A":{"type":"Dw"#).unwrap();
        assert_eq!(banners::load(&file), None);
        assert!(banners::save(&file, &banners::Backup::new()));
        assert!(!file.exists());
        assert!(banners::save(&file, &banners::Backup::new()), "removing a missing file is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn banner_nothing_is_hidden_when_the_backup_cannot_be_saved() {
        let base = format!(r"{SCRATCH_ROOT}-NoSave-{}", std::process::id());
        scratch_app(&base, "A", Some(1));
        scratch_app(&base, "B", None);
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
        assert!(!banners::apply_plan(&base, &mut backup, &wanted, &mut |_| false));
        assert!(backup.is_empty());
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Missing));
        // The next pass that can save hides them with the real originals.
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.get("A"), Some(&banners::Prior::Dword(1)));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "B"), Some(banners::Prior::Missing));
        drop_scratch(&base);
    }

    #[test]
    fn banner_restore_keeps_a_value_the_user_changed_while_hidden() {
        let base = format!(r"{SCRATCH_ROOT}-UserChange-{}", std::process::id());
        scratch_app(&base, "On", None);
        scratch_app(&base, "Off", Some(1));
        scratch_app(&base, "Untouched", Some(1));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["On".to_string(), "Off".to_string(), "Untouched".to_string()].into();
        banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true);
        // In Windows Settings the user turns On's banner back on and deletes Off's value.
        assert!(banners::write(&base, "On", banners::Prior::Dword(1)));
        assert!(banners::write(&base, "Off", banners::Prior::Missing));
        assert!(banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true));
        assert!(backup.is_empty());
        assert_eq!(banners::read(&base, "On"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "Off"), Some(banners::Prior::Missing));
        assert_eq!(banners::read(&base, "Untouched"), Some(banners::Prior::Dword(1)));
        drop_scratch(&base);
    }

    // Round 3: the list the worker sees over time, as the scan diff applies it.
    #[test]
    fn scan_sequence_seeds_then_reports_only_new_ids_and_ids_can_return() {
        let set = |ids: &[u32]| ids.iter().copied().collect::<HashSet<u32>>();
        let mut seen: Option<HashSet<u32>> = None;
        // Same steps as scan(): the first list only seeds.
        let mut step = |current: HashSet<u32>| match seen.replace(current.clone()) {
            None => None,
            Some(prev) => Some(diff_ids(&prev, &current)),
        };
        assert_eq!(step(set(&[1, 2, 3])), None);
        assert_eq!(step(set(&[1, 2, 3])), Some((vec![], vec![])));
        assert_eq!(step(set(&[1, 2, 3, 7, 5])), Some((vec![5, 7], vec![])));
        assert_eq!(step(set(&[5, 7])), Some((vec![], vec![1, 2, 3])));
        // A removed id that shows up again counts as new again.
        assert_eq!(step(set(&[1, 5, 7])), Some((vec![1], vec![])));
        // Everything cleared in Notification Center.
        assert_eq!(step(set(&[])), Some((vec![], vec![1, 5, 7])));
    }

    #[test]
    fn newest_of_a_burst_matches_the_diff_order_and_leaves_the_input_alone() {
        let seen: HashSet<u32> = [1].into();
        let current: HashSet<u32> = (1..=12).collect();
        let (added, _) = diff_ids(&seen, &current);
        assert_eq!(added.len(), 11);
        let kept = newest(&added, 5);
        assert_eq!(kept, &[8, 9, 10, 11, 12]);
        assert_eq!(added.len(), 11);
        assert_eq!(newest(&added, added.len()), &added[..]);
        assert_eq!(newest(&added, 1), &[12]);
    }

    #[test]
    fn split_text_keeps_title_when_the_body_is_only_blank_lines_and_trims_each_part() {
        let (t, b) = split_text(vec!["  Title  ".into(), " \r\n ".into(), "\tline one\t".into(), "".into(), "line two".into()]);
        assert_eq!(t, "Title");
        assert_eq!(b, "line one\nline two");
        let (t, b) = split_text(vec!["Only".into(), "   ".into()]);
        assert_eq!((t.as_str(), b.as_str()), ("Only", ""));
        assert_eq!(split_text(vec![]), (String::new(), String::new()));
    }

    #[test]
    fn banner_backup_with_an_unknown_value_type_or_wrong_shape_is_unreadable_not_empty() {
        let dir = std::env::temp_dir().join(format!("nectar-banners-shape-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("notification-banners.json");
        for bad in [r#"{"A":{"type":"Sz","value":"x"}}"#, r#"[]"#, r#"{"A":1}"#, r#"{"A":{"type":"Dword"}}"#, r#"{"A":{"type":"Dword","value":-1}}"#, r#"{"A":{"type":"Dword","value":4294967296}}"#, ""] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(banners::load(&file), None, "{bad:?} must not load as an empty backup");
        }
        std::fs::write(&file, r#"{"A":{"type":"Dword","value":1}}"#).unwrap();
        assert_eq!(banners::load(&file).and_then(|b| b.get("A").copied()), Some(banners::Prior::Dword(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn banner_app_ids_with_symbols_and_unicode_round_trip_through_json_and_the_registry() {
        let base = format!(r"{SCRATCH_ROOT}-Names-{}", std::process::id());
        let ids = ["Vendor.App_8wekyb3d8bbwe!App", "Ünïcode Ağ Uygulaması", "With Space.exe", "a.b.c"];
        for id in ids {
            scratch_app(&base, id, Some(1));
        }
        let wanted: BTreeSet<String> = ids.iter().map(|s| s.to_string()).collect();
        let mut backup = banners::Backup::new();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        let json = serde_json::to_string(&backup).unwrap();
        let mut back: banners::Backup = serde_json::from_str(&json).unwrap();
        assert_eq!(back, backup);
        for id in ids {
            assert_eq!(banners::read(&base, id), Some(banners::Prior::Dword(0)), "{id}");
        }
        assert!(banners::apply_plan(&base, &mut back, &BTreeSet::new(), &mut |_| true));
        for id in ids {
            assert_eq!(banners::read(&base, id), Some(banners::Prior::Dword(1)), "{id}");
        }
        drop_scratch(&base);
    }

    #[test]
    fn banner_an_app_that_already_had_banners_off_stays_off_and_is_forgotten() {
        let base = format!(r"{SCRATCH_ROOT}-AlreadyOff-{}", std::process::id());
        scratch_app(&base, "Quiet", Some(0));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["Quiet".to_string()].into();
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| true));
        assert_eq!(backup.get("Quiet"), Some(&banners::Prior::Dword(0)));
        assert_eq!(banners::read(&base, "Quiet"), Some(banners::Prior::Dword(0)));
        assert!(banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true));
        assert!(backup.is_empty());
        assert_eq!(banners::read(&base, "Quiet"), Some(banners::Prior::Dword(0)), "the user's own 0 is written back, not removed");
        drop_scratch(&base);
    }

    #[test]
    fn banner_second_pass_with_the_same_wanted_set_is_a_no_op_and_does_not_persist() {
        let base = format!(r"{SCRATCH_ROOT}-Idempotent-{}", std::process::id());
        scratch_app(&base, "A", Some(1));
        let mut backup = banners::Backup::new();
        let wanted: BTreeSet<String> = ["A".to_string()].into();
        let mut saves = 0;
        assert!(banners::apply_plan(&base, &mut backup, &wanted, &mut |_| { saves += 1; true }));
        assert!(!banners::apply_plan(&base, &mut backup, &wanted, &mut |_| { saves += 1; true }));
        assert_eq!(saves, 1);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(0)));
        banners::apply_plan(&base, &mut backup, &BTreeSet::new(), &mut |_| true);
        assert_eq!(banners::read(&base, "A"), Some(banners::Prior::Dword(1)));
        drop_scratch(&base);
    }

    #[test]
    fn banner_restore_all_never_touches_apps_missing_from_the_backup() {
        let base = format!(r"{SCRATCH_ROOT}-Only-{}", std::process::id());
        scratch_app(&base, "Mine", Some(1));
        scratch_app(&base, "UserOff", Some(0));
        let dir = std::env::temp_dir().join(format!("nectar-banners-only-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("notification-banners.json");
        let mut backup = banners::Backup::new();
        banners::apply_plan(&base, &mut backup, &["Mine".to_string()].into(), &mut |_| true);
        assert!(banners::save(&file, &backup));
        banners::restore_all(&base, &file);
        assert_eq!(banners::read(&base, "Mine"), Some(banners::Prior::Dword(1)));
        assert_eq!(banners::read(&base, "UserOff"), Some(banners::Prior::Dword(0)), "an app Nectar never hid keeps the user's 0");
        assert!(!file.exists(), "nothing left to restore removes the backup file");
        let _ = std::fs::remove_dir_all(&dir);
        drop_scratch(&base);
    }
}

#[cfg(test)]
mod probe {
    use super::*;

    // Feasibility probe (T-014): `cargo test --bin nectar notifications::probe -- --ignored --nocapture --test-threads 1`.
    // Prints counts, app names and timings only, never notification text.
    #[test]
    #[ignore]
    fn listener_access_probe() {
        com_mta();
        let listener = match UserNotificationListener::Current() {
            Ok(l) => l,
            Err(e) => {
                println!("Current() failed: {e:?}");
                return;
            }
        };
        println!("GetAccessStatus: {}", access_label(listener.GetAccessStatus().ok()));
        println!("RequestAccessAsync: {}", access_label(listener.RequestAccessAsync().and_then(|op| op.get()).ok()));
        let t = Instant::now();
        match listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()) {
            Ok(list) => {
                let n = list.Size().unwrap_or(0);
                println!("GetNotificationsAsync: {n} toasts in {:?}", t.elapsed());
                let mut apps: BTreeMap<String, (usize, bool)> = BTreeMap::new();
                for i in 0..n {
                    let Ok(un) = list.GetAt(i) else { continue };
                    let name = un.AppInfo().and_then(|a| a.DisplayInfo()).and_then(|d| d.DisplayName()).map(|s| s.to_string()).unwrap_or_default();
                    let has_logo = un.AppInfo().and_then(|a| a.DisplayInfo()).ok().as_ref().and_then(logo).is_some();
                    let e = apps.entry(name).or_insert((0, has_logo));
                    e.0 += 1;
                }
                for (name, (count, has_logo)) in apps {
                    println!("  {name}: {count} (package logo: {has_logo})");
                }
            }
            Err(e) => println!("GetNotificationsAsync failed: {e:?}"),
        }
        println!("SHQueryUserNotificationState: {:?}, WNF quiet hours profile: {:?}", unsafe { windows::Win32::UI::Shell::SHQueryUserNotificationState() }.map(|s| s.0), quiet_hours_profile());
        let handler = windows::Foundation::TypedEventHandler::new(|_, _| Ok(()));
        match listener.NotificationChanged(&handler) {
            Ok(token) => {
                println!("NotificationChanged: subscribed");
                let _ = listener.RemoveNotificationChanged(token);
            }
            Err(e) => println!("NotificationChanged failed: {e:?}"),
        }
    }

    // Shows one harmless test toast under PowerShell's app id, checks that the folder watch fires and the
    // diff finds it, then removes it again with RemoveNotification.
    #[test]
    #[ignore]
    fn new_toast_probe() {
        com_mta();
        let listener = UserNotificationListener::Current().unwrap();
        let ids = || -> HashSet<u32> {
            let list = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()).unwrap();
            (0..list.Size().unwrap_or(0)).filter_map(|i| list.GetAt(i).ok()?.Id().ok()).collect()
        };
        let before = ids();
        let watches = Watches::open();
        println!("registry watch: {}, folder watch: {}", watches.reg.is_some(), watches.dir.is_some());
        let script = r#"
$null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
$null = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text>Nectar test</text><text>T-014 probe, removed automatically</text></binding></visual></toast>')
$id = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($id).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
"#;
        let sent = Instant::now();
        let status = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).status().unwrap();
        println!("toast shown: {status:?} after {:?}", sent.elapsed());
        let fired = wait(&watches.handles(), 10_000);
        println!("watch fired: {:?} (0 = registry, 1 = folder) after {:?}", fired, sent.elapsed());
        std::thread::sleep(Duration::from_millis(200));
        watches.drain();
        let after = ids();
        let (added, removed) = diff_ids(&before, &after);
        println!("diff after {:?}: added {:?}, removed {}", sent.elapsed(), added, removed.len());
        for id in &added {
            let n = listener.GetNotification(*id).ok().and_then(|n| read_notification(&n));
            if let Some(n) = n {
                println!("  id {} app {:?} aumid {:?} title_len {} body_len {} icon {} age {} ms", n.id, n.app, n.aumid, n.title.len(), n.body.len(), n.icon.is_some(), crate::utils::get_now_ms() - n.time);
            }
            let _ = listener.RemoveNotification(*id);
        }
        std::thread::sleep(Duration::from_millis(500));
        let gone = ids();
        println!("removed again: {}", added.iter().all(|id| !gone.contains(id)));
        println!("removal signalled a watch: {:?}", wait(&watches.handles(), 0));
    }
}
