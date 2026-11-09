// Mic/camera in-use indicator and meeting mode (T-019).
//
// Windows records each capture in the CapabilityAccessManager consent store: an app's
// LastUsedTimeStop is 0 while it holds the microphone or camera. Those keys are watched with
// RegNotifyChangeKeyValue, and a desktop app in use is tied to its process handle, so nothing runs
// while nothing changes. Meeting mode mutes every active capture endpoint and follows mute changes
// made elsewhere through endpoint volume callbacks.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};
use windows::core::{implement, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl};
use windows::Win32::Media::Audio::{
    eCapture, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ,
    REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_QWORD,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, SetEvent, WaitForMultipleObjects, INFINITE, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

pub const INDICATOR_SETTING: &str = "nectar-mic-cam-indicator";
pub const HOTKEY_SETTING: &str = "nectar-mic-mute-hotkey";
/// Free on a stock Windows 11: Win+Alt+K is Windows' own call mute, Win+Alt+M Game Bar's mic,
/// Win+Ctrl+M Magnifier settings, Ctrl+Alt+M a Word comment and Ctrl+Shift+M Teams' own mute.
pub const DEFAULT_HOTKEY: &str = "Win+Ctrl+K";
/// On by default only in the owner's own build (D-036); friends turn it on themselves (Q14).
const HOTKEY_ON_BY_DEFAULT: bool = cfg!(feature = "phone-link");

const CONSENT_STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";
/// Only for a desktop app in use whose process can't be opened (elevated, or past the wait limit):
/// it is checked this often, since an app that crashes never writes LastUsedTimeStop.
const STALE_CHECK_MS: u32 = 10_000;
/// WaitForMultipleObjects takes 64 handles: the wake event and two registry events come first.
const MAX_WATCHED_PROCESSES: usize = 64 - 3;
/// 2000-01-01 as a FILETIME; a smaller LastUsedTimeStart is not a timestamp and is not compared.
const FILETIME_2000: u64 = 125_911_584_000_000_000;
/// One second of FILETIME ticks, slack for a process that starts capturing right away.
const FILETIME_SLACK: u64 = 10_000_000;
/// Marks our own SetMute calls in the endpoint notifications.
const EVENT_CONTEXT: windows::core::GUID = windows::core::GUID::from_u128(0x6e3c_7a1d_52b4_4c1e_9a57_0d9f_3b21_c019);

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivacyState {
    /// Friendly names of the apps using the microphone / camera right now.
    pub mic: Vec<String>,
    pub camera: Vec<String>,
    /// The red mic shows: every active capture endpoint is muted, or Nectar's mute holds. Only reported
    /// while meeting mode is on or Nectar's mute still holds after it was turned off.
    pub muted: bool,
    /// Nectar muted, but a microphone is live again (it refused the mute or was unmuted elsewhere).
    pub partial: bool,
    /// Meeting mode is on (the mute shortcut is set).
    pub meeting: bool,
}

enum Command {
    Toggle,
    Set(bool),
}

static WAKE: AtomicIsize = AtomicIsize::new(0);
static AUDIO_DIRTY: AtomicBool = AtomicBool::new(false);
static TOPOLOGY_DIRTY: AtomicBool = AtomicBool::new(false);
static COMMANDS: Mutex<Vec<Command>> = Mutex::new(Vec::new());
static STATE: Mutex<PrivacyState> = Mutex::new(PrivacyState { mic: Vec::new(), camera: Vec::new(), muted: false, partial: false, meeting: false });
static PACKAGE_NAMES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn start(app: AppHandle) {
    let Ok(wake) = (unsafe { CreateEventW(None, false, false, PCWSTR::null()) }) else { return };
    WAKE.store(wake.0 as isize, Ordering::SeqCst);
    let raw = wake.0 as isize;
    std::thread::spawn(move || worker(app, HANDLE(raw as *mut _)));
}

fn wake() {
    let h = WAKE.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(h as *mut _));
        }
    }
}

fn push(cmd: Command) {
    if let Ok(mut q) = COMMANDS.lock() {
        q.push(cmd);
    }
    wake();
}

/// Called from the mute shortcut.
pub fn toggle_mute() {
    push(Command::Toggle);
}

pub fn on_setting_changed(key: &str) {
    if key == HOTKEY_SETTING {
        crate::hotkeys::reload();
        wake();
    } else if key == INDICATOR_SETTING {
        wake();
    }
}

#[tauri::command]
pub fn get_privacy_state() -> PrivacyState {
    STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn set_mic_muted(muted: bool) {
    push(Command::Set(muted));
}

/// The mute shortcut: missing means the default, an empty string means off.
pub fn hotkey_from_setting(raw: Option<&str>) -> Option<String> {
    match raw {
        None => HOTKEY_ON_BY_DEFAULT.then(|| DEFAULT_HOTKEY.to_string()),
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(s.trim().to_string()),
    }
}

pub fn hotkey_spec(app: &AppHandle) -> Option<String> {
    hotkey_from_setting(crate::utils::get_setting_str(app, HOTKEY_SETTING).as_deref())
}

fn indicator_enabled() -> bool {
    let Some(cache) = crate::state::SETTINGS_CACHE.get() else { return true };
    let Ok(guard) = cache.lock() else { return true };
    match guard.get(INDICATOR_SETTING) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s != "false",
        _ => true,
    }
}

fn worker(app: AppHandle, wake_event: HANDLE) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut usage: Option<UsageWatch> = None;
    let mut audio: Option<AudioWatch> = None;
    let mut found = Usage::default();
    let mut rescan = true;
    // Nectar's own mute (shortcut or click), so the shortcut can always undo it (I-207).
    let mut held = false;
    let mut view = MuteView::default();
    let mut last: Option<PrivacyState> = None;

    loop {
        let indicator = indicator_enabled();
        let meeting = hotkey_spec(&app).is_some();
        if indicator != usage.is_some() {
            usage = if indicator { UsageWatch::open() } else { None };
            rescan = true;
        }
        // Meeting mode turned off while Nectar's mute holds: the mics stay watched, and the red mic
        // shown, until they are unmuted (I-208).
        if (meeting || held) && audio.is_none() {
            audio = AudioWatch::new();
            AUDIO_DIRTY.store(true, Ordering::SeqCst);
        }

        let commands = COMMANDS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        if let Some(a) = audio.as_mut() {
            if TOPOLOGY_DIRTY.swap(false, Ordering::SeqCst) {
                a.refresh();
                AUDIO_DIRTY.store(true, Ordering::SeqCst);
            }
            for cmd in commands {
                let target = match cmd {
                    Command::Toggle => {
                        let states = a.states();
                        toggle_target(&states, mute_held(&states, held))
                    }
                    Command::Set(m) => m,
                };
                a.set_all(target);
                held = target;
                AUDIO_DIRTY.store(true, Ordering::SeqCst);
            }
            if AUDIO_DIRTY.swap(false, Ordering::SeqCst) {
                let states = a.states();
                held = mute_held(&states, held);
                view = MuteView::of(&states, held);
            }
        }
        if !meeting && !held {
            audio = None;
        }
        if audio.is_none() {
            held = false;
            view = MuteView::default();
        }

        if rescan {
            rescan = false;
            found = usage.as_ref().map(UsageWatch::scan).unwrap_or_default();
        }

        let state = PrivacyState { mic: found.mic.clone(), camera: found.camera.clone(), muted: view.muted, partial: view.partial, meeting };
        if last.as_ref() != Some(&state) {
            if let Ok(mut s) = STATE.lock() {
                *s = state.clone();
            }
            let _ = app.emit("privacy-state", &state);
            last = Some(state);
        }

        let mut handles = vec![wake_event];
        let mut registry = 0;
        if let Some(u) = &usage {
            handles.extend(u.events());
            registry = handles.len() - 1;
        }
        // A watched process exiting is a crash or a quit, so the scan runs again (I-209).
        handles.extend(found.processes.iter().map(|p| p.0));
        let timeout = if found.unconfirmed { STALE_CHECK_MS } else { INFINITE };
        let r = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
        let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if i == 0 {
            continue;
        }
        if i <= registry {
            // Re-armed before reading, so a write during the scan is not lost.
            if let Some(u) = &usage {
                u.arm(i - 1);
            }
        } else if i >= handles.len() && r != WAIT_TIMEOUT {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        rescan = true;
    }
}

// ---------- In-use apps (consent store) ----------

#[derive(Default)]
struct Usage {
    mic: Vec<String>,
    camera: Vec<String>,
    /// One process per desktop app in use, waited on so a crash or quit rescans.
    processes: Vec<Process>,
    /// A desktop app is listed whose process could not be opened, so it is checked again later.
    unconfirmed: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// Package family name, like `MSTeams_8wekyb3d8bbwe`.
    Packaged(String),
    /// Exe path of a desktop app and its LastUsedTimeStart.
    Desktop { path: String, start: u64 },
}

/// Whether a desktop app's capture still has a process behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Liveness {
    Running,
    /// A process with that exe name could not be opened, so it can't be ruled out.
    Unknown,
    /// Crashed or killed while capturing, so the stop time was never written.
    Gone,
}

/// A process handle opened with SYNCHRONIZE; it signals when the process exits.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// In use while the last start has no matching stop.
pub fn in_use(start: u64, stop: u64) -> bool {
    start != 0 && (stop == 0 || stop < start)
}

/// NonPackaged keys are exe paths with `#` for `\`.
pub fn desktop_path(key: &str) -> String {
    key.replace('#', "\\")
}

fn exe_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase()
}

/// Same exe file. A `#` in a real folder name became `\` in the consent store key, so it is mapped the same way.
pub fn same_image(entry: &str, process: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace(['#', '/'], "\\");
    norm(entry) == norm(process)
}

/// The process already ran when the capture started; one started later is a relaunch after a crash.
pub fn ran_at(created: u64, start: u64) -> bool {
    start < FILETIME_2000 || created <= start.saturating_add(FILETIME_SLACK)
}

/// Readable name from a package family name when the package itself can't be asked:
/// `Microsoft.WindowsCamera_8wekyb3d8bbwe` → `Windows Camera`, `MSTeams_8wekyb3d8bbwe` → `MS Teams`.
pub fn package_fallback_name(family: &str) -> String {
    let name = family.rsplit_once('_').map(|(n, _)| n).unwrap_or(family);
    let last = name.rsplit('.').find(|s| !s.is_empty()).unwrap_or(name);
    let chars: Vec<char> = last.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push(' ');
            }
        }
        out.push(c);
    }
    if out.is_empty() {
        family.to_string()
    } else {
        out
    }
}

/// Names in first-seen order without repeats (case-insensitive).
pub fn unique_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    names.into_iter().filter(|n| !n.trim().is_empty() && seen.insert(n.to_lowercase())).collect()
}

struct KeyWatch {
    key: HKEY,
    event: HANDLE,
}

impl KeyWatch {
    fn open(path: &str) -> Option<Self> {
        let wide = HSTRING::from(path);
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &wide, Some(0), KEY_NOTIFY | KEY_READ, &mut key) } != ERROR_SUCCESS {
            return None;
        }
        let event = match unsafe { CreateEventW(None, false, false, PCWSTR::null()) } {
            Ok(e) => e,
            Err(_) => {
                unsafe {
                    let _ = RegCloseKey(key);
                }
                return None;
            }
        };
        let watch = KeyWatch { key, event };
        watch.arm().then_some(watch)
    }

    fn arm(&self) -> bool {
        unsafe { RegNotifyChangeKeyValue(self.key, true, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, Some(self.event), true) }.is_ok()
    }
}

impl Drop for KeyWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.key);
            let _ = CloseHandle(self.event);
        }
    }
}

struct UsageWatch {
    mic: Option<KeyWatch>,
    camera: Option<KeyWatch>,
}

impl UsageWatch {
    fn open() -> Option<Self> {
        let mic = KeyWatch::open(&format!(r"{CONSENT_STORE}\microphone"));
        let camera = KeyWatch::open(&format!(r"{CONSENT_STORE}\webcam"));
        if mic.is_none() && camera.is_none() {
            crate::diagnostics::log("privacy: consent store keys could not be opened");
            return None;
        }
        Some(UsageWatch { mic, camera })
    }

    fn events(&self) -> impl Iterator<Item = HANDLE> + '_ {
        self.mic.iter().chain(self.camera.iter()).map(|w| w.event)
    }

    fn arm(&self, index: usize) {
        if let Some(w) = self.mic.iter().chain(self.camera.iter()).nth(index) {
            w.arm();
        }
    }

    fn scan(&self) -> Usage {
        let mic = self.mic.as_ref().map(|w| entries_in_use(w.key)).unwrap_or_default();
        let camera = self.camera.as_ref().map(|w| entries_in_use(w.key)).unwrap_or_default();
        let mut processes = Vec::new();
        let mut checked: HashMap<(String, u64), Liveness> = HashMap::new();
        let mut running: Option<Vec<(u32, String)>> = None;
        let mut liveness = |path: &str, start: u64| {
            *checked.entry((path.to_lowercase(), start)).or_insert_with(|| {
                let running = running.get_or_insert_with(running_processes);
                process_liveness(path, start, running, &mut processes)
            })
        };
        let (mic, mic_unknown) = resolve(&mic, &mut liveness);
        let (camera, camera_unknown) = resolve(&camera, &mut liveness);
        Usage { mic, camera, processes, unconfirmed: mic_unknown || camera_unknown }
    }
}

/// Friendly names, and whether a desktop app among them could not be checked.
fn resolve(entries: &[Entry], liveness: &mut impl FnMut(&str, u64) -> Liveness) -> (Vec<String>, bool) {
    let mut unknown = false;
    let names: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Packaged(family) => Some(package_name(family)),
            Entry::Desktop { path, start } => match liveness(path, *start) {
                Liveness::Gone => None,
                l => {
                    unknown |= l == Liveness::Unknown;
                    Some(unsafe { crate::commands::friendly_process_name(path) })
                }
            },
        })
        .collect();
    (unique_names(names), unknown)
}

/// Looks for the process behind a desktop app's capture: same exe path, already running when the
/// capture started. The oldest such process is kept in `watched` (for an app with many processes,
/// like a browser, that is the main one).
fn process_liveness(path: &str, start: u64, running: &[(u32, String)], watched: &mut Vec<Process>) -> Liveness {
    let name = exe_name(path);
    let mut unknown = false;
    let mut best: Option<(u64, Process)> = None;
    for (pid, _) in running.iter().filter(|(_, exe)| *exe == name) {
        let Some((process, image, created)) = open_process(*pid) else {
            unknown = true;
            continue;
        };
        if same_image(path, &image) && ran_at(created, start) && best.as_ref().is_none_or(|(c, _)| created < *c) {
            best = Some((created, process));
        }
    }
    match best {
        Some((_, process)) if watched.len() < MAX_WATCHED_PROCESSES => {
            watched.push(process);
            Liveness::Running
        }
        Some(_) => Liveness::Unknown,
        None if unknown => Liveness::Unknown,
        None => Liveness::Gone,
    }
}

/// Handle, full exe path and creation time (FILETIME).
fn open_process(pid: u32) -> Option<(Process, String, u64)> {
    use windows::Win32::Foundation::FILETIME;
    unsafe {
        let process = Process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid).ok()?);
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
        let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        Some((process, String::from_utf16_lossy(&buf[..len as usize]), created))
    }
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = [0u16; 512];
    for index in 0.. {
        let mut len = buf.len() as u32;
        let r = unsafe { RegEnumKeyExW(key, index, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if r != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

fn qword(key: HKEY, sub: &str, value: &str) -> u64 {
    let sub = HSTRING::from(sub);
    let value = HSTRING::from(value);
    let mut data = 0u64;
    let mut size = std::mem::size_of::<u64>() as u32;
    let r = unsafe { RegGetValueW(key, &sub, &value, RRF_RT_REG_QWORD, None, Some(&mut data as *mut u64 as *mut _), Some(&mut size)) };
    if r == ERROR_SUCCESS {
        data
    } else {
        0
    }
}

/// LastUsedTimeStart while in use.
fn used(key: HKEY, sub: &str) -> Option<u64> {
    let start = qword(key, sub, "LastUsedTimeStart");
    in_use(start, qword(key, sub, "LastUsedTimeStop")).then_some(start)
}

fn entries_in_use(key: HKEY) -> Vec<Entry> {
    let mut out = Vec::new();
    for name in subkeys(key) {
        if name.eq_ignore_ascii_case("NonPackaged") {
            let mut np = HKEY::default();
            if unsafe { RegOpenKeyExW(key, &HSTRING::from(name.as_str()), Some(0), KEY_READ, &mut np) } != ERROR_SUCCESS {
                continue;
            }
            for app in subkeys(np) {
                if let Some(start) = used(np, &app) {
                    out.push(Entry::Desktop { path: desktop_path(&app), start });
                }
            }
            unsafe {
                let _ = RegCloseKey(np);
            }
        } else if used(key, &name).is_some() {
            out.push(Entry::Packaged(name));
        }
    }
    out
}

/// Process ids and lower-case exe names.
fn running_processes() -> Vec<(u32, String)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            out.push((entry.th32ProcessID, String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase()));
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn package_name(family: &str) -> String {
    if let Some(name) = PACKAGE_NAMES.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(family).cloned())) {
        return name;
    }
    let name = package_display_name(family).unwrap_or_else(|| package_fallback_name(family));
    if let Ok(mut m) = PACKAGE_NAMES.lock() {
        m.get_or_insert_with(HashMap::new).insert(family.to_string(), name.clone());
    }
    name
}

fn package_display_name(family: &str) -> Option<String> {
    use windows::Management::Deployment::PackageManager;
    let manager = PackageManager::new().ok()?;
    let packages = manager.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family)).ok()?;
    let package = packages.First().ok()?.Current().ok()?;
    let name = package.DisplayName().ok()?.to_string();
    let name = name.trim();
    (!name.is_empty() && !name.starts_with("ms-resource:")).then(|| name.to_string())
}

// ---------- Meeting mode (capture endpoint mute) ----------

/// Muted only when there is at least one endpoint and every one of them is muted.
pub fn all_muted(states: impl IntoIterator<Item = bool>) -> bool {
    let mut any = false;
    for muted in states {
        if !muted {
            return false;
        }
        any = true;
    }
    any
}

/// The mute states below are per capture endpoint; `None` could not be read and is left out.
fn known(states: &[Option<bool>]) -> impl Iterator<Item = bool> + '_ {
    states.iter().flatten().copied()
}

/// Nectar's mute ends once no microphone is muted any more (all unmuted elsewhere, or none took it).
pub fn mute_held(states: &[Option<bool>], held: bool) -> bool {
    held && known(states).any(|m| m)
}

/// The shortcut unmutes while Nectar's mute holds or every microphone is muted, and mutes otherwise.
/// Following the intent, not only the devices, keeps it from muting forever when one device won't take it.
pub fn toggle_target(states: &[Option<bool>], held: bool) -> bool {
    !(held || all_muted(known(states)))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MuteView {
    pub muted: bool,
    pub partial: bool,
}

impl MuteView {
    /// Red mic when every microphone is muted, or while Nectar's mute holds; amber (`partial`) when
    /// Nectar's mute holds but a microphone is live, so there is still something to click.
    pub fn of(states: &[Option<bool>], held: bool) -> Self {
        let all = all_muted(known(states));
        let held = mute_held(states, held);
        MuteView { muted: all || held, partial: held && !all }
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct MuteCallback {
    muted: AtomicBool,
}

impl IAudioEndpointVolumeCallback_Impl for MuteCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        // Level changes (apps' auto gain) arrive here too; only a mute flip is worth a wake-up.
        if let Some(d) = unsafe { data.as_ref() } {
            let muted = d.bMuted.as_bool();
            if self.muted.swap(muted, Ordering::SeqCst) != muted {
                AUDIO_DIRTY.store(true, Ordering::SeqCst);
                wake();
            }
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback;

fn topology_changed() {
    TOPOLOGY_DIRTY.store(true, Ordering::SeqCst);
    wake();
}

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        topology_changed();
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        topology_changed();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        topology_changed();
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        if flow == eCapture {
            topology_changed();
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &windows::Win32::Foundation::PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

struct Endpoint {
    volume: IAudioEndpointVolume,
    callback: Option<IAudioEndpointVolumeCallback>,
}

/// Every active capture endpoint, not only the default one: a meeting app set to a headset while
/// the laptop mic is the default would otherwise stay live.
struct AudioWatch {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    endpoints: Vec<Endpoint>,
}

impl AudioWatch {
    fn new() -> Option<Self> {
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
        let client: IMMNotificationClient = DeviceCallback.into();
        if let Err(e) = unsafe { enumerator.RegisterEndpointNotificationCallback(&client) } {
            crate::diagnostics::log(&format!("privacy: device notifications unavailable: {e}"));
        }
        let mut watch = AudioWatch { enumerator, client, endpoints: Vec::new() };
        watch.refresh();
        Some(watch)
    }

    fn refresh(&mut self) {
        self.release();
        let Ok(list) = (unsafe { self.enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }) else { return };
        for i in 0..unsafe { list.GetCount() }.unwrap_or(0) {
            let Ok(device) = (unsafe { list.Item(i) }) else { continue };
            let Ok(volume) = (unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }) else { continue };
            let muted = unsafe { volume.GetMute() }.map(|b| b.as_bool()).unwrap_or(false);
            let callback: IAudioEndpointVolumeCallback = MuteCallback { muted: AtomicBool::new(muted) }.into();
            let callback = unsafe { volume.RegisterControlChangeNotify(&callback) }.is_ok().then_some(callback);
            self.endpoints.push(Endpoint { volume, callback });
        }
    }

    fn release(&mut self) {
        for e in self.endpoints.drain(..) {
            if let Some(cb) = &e.callback {
                unsafe {
                    let _ = e.volume.UnregisterControlChangeNotify(cb);
                }
            }
        }
    }

    fn states(&self) -> Vec<Option<bool>> {
        self.endpoints.iter().map(|e| unsafe { e.volume.GetMute() }.ok().map(|b| b.as_bool())).collect()
    }

    fn set_all(&self, muted: bool) {
        for e in &self.endpoints {
            unsafe {
                let _ = e.volume.SetMute(muted, &EVENT_CONTEXT);
            }
        }
    }
}

impl Drop for AudioWatch {
    fn drop(&mut self) {
        self.release();
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_use_needs_a_start_without_a_later_stop() {
        assert!(!in_use(0, 0), "never used");
        assert!(in_use(134_000, 0), "started, not stopped");
        assert!(!in_use(134_000, 134_500), "stopped after the start");
        assert!(in_use(134_500, 134_000), "restarted after the last stop");
    }

    #[test]
    fn desktop_keys_become_paths_and_exe_names() {
        let path = desktop_path("C:#Program Files#Google#Chrome#Application#chrome.exe");
        assert_eq!(path, r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        assert_eq!(exe_name(&path), "chrome.exe");
        assert_eq!(exe_name("Zoom.EXE"), "zoom.exe");
    }

    #[test]
    fn package_family_names_read_as_words() {
        assert_eq!(package_fallback_name("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "Windows Camera");
        assert_eq!(package_fallback_name("MSTeams_8wekyb3d8bbwe"), "MS Teams");
        assert_eq!(package_fallback_name("Microsoft.MicrosoftOfficeHub_8wekyb3d8bbwe"), "Microsoft Office Hub");
        assert_eq!(package_fallback_name("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe"), "Windows Sound Recorder");
        assert_eq!(package_fallback_name("zoom"), "zoom");
        assert_eq!(package_fallback_name("Microsoft.XboxGamingOverlay_8wekyb3d8bbwe"), "Xbox Gaming Overlay");
    }

    #[test]
    fn names_are_unique_and_keep_their_order() {
        let names = unique_names(["Zoom".to_string(), "Google Chrome".to_string(), "zoom".to_string(), " ".to_string()]);
        assert_eq!(names, vec!["Zoom".to_string(), "Google Chrome".to_string()]);
    }

    #[test]
    fn muted_means_every_endpoint_and_at_least_one() {
        assert!(!all_muted([]), "no microphone at all");
        assert!(all_muted([true]));
        assert!(all_muted([true, true]));
        assert!(!all_muted([true, false]), "a live headset mic is not muted");
    }

    #[test]
    fn hotkey_setting_defaults_and_turns_off_when_empty() {
        assert_eq!(hotkey_from_setting(None).as_deref(), HOTKEY_ON_BY_DEFAULT.then_some(DEFAULT_HOTKEY));
        assert_eq!(hotkey_from_setting(Some("")), None);
        assert_eq!(hotkey_from_setting(Some("  ")), None);
        assert_eq!(hotkey_from_setting(Some(" Ctrl+Alt+F9 ")).as_deref(), Some("Ctrl+Alt+F9"));
        assert!(crate::launchers::parse_shortcut(DEFAULT_HOTKEY).is_some());
    }

    /// Reads this PC's consent store, package names and capture endpoints (nothing is changed).
    /// `cargo test --bin nectar privacy::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_read_only_scan() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let watch = UsageWatch::open().expect("consent store");
        let usage = watch.scan();
        println!("mic in use: {:?}
camera in use: {:?}
unconfirmed: {}", usage.mic, usage.camera, usage.unconfirmed);
        for family in ["MSTeams_8wekyb3d8bbwe", "Microsoft.WindowsCamera_8wekyb3d8bbwe"] {
            println!("{family} -> {:?}", package_display_name(family));
        }
        println!("chrome -> {}", unsafe { crate::commands::friendly_process_name(r"C:\Program Files\Google\Chrome\Application\chrome.exe") });
        let audio = AudioWatch::new().expect("audio");
        println!("capture endpoint mute states: {:?}", audio.states());
        let me = std::env::current_exe().unwrap().to_string_lossy().to_string();
        let mut watched = Vec::new();
        let running = running_processes();
        let now = filetime_now();
        let elsewhere = format!(r"C:\elsewhere\{}", exe_name(&me));
        println!("this test process: {:?} (watched {})", process_liveness(&me, now, &running, &mut watched), watched.len());
        println!("started after the capture: {:?}", process_liveness(&me, FILETIME_2000 + 1, &running, &mut Vec::new()));
        println!("other path, same exe name: {:?}", process_liveness(&elsewhere, now, &running, &mut Vec::new()));
    }

    #[test]
    fn a_watched_process_signals_when_it_exits() {
        let mut child = std::process::Command::new("cmd.exe").args(["/c", "ping -n 2 127.0.0.1 >nul"]).spawn().expect("cmd");
        let (process, image, created) = open_process(child.id()).expect("open own child");
        assert!(exe_name(&image) == "cmd.exe" && created > FILETIME_2000);
        assert_eq!(unsafe { WaitForMultipleObjects(&[process.0], false, 0) }, WAIT_TIMEOUT, "still running");
        assert_eq!(unsafe { WaitForMultipleObjects(&[process.0], false, 10_000) }, WAIT_OBJECT_0, "exit wakes the wait");
        let _ = child.wait();
    }

    fn filetime_now() -> u64 {
        let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
        (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
    }

    fn desktop(path: &str) -> Entry {
        Entry::Desktop { path: path.into(), start: 134_000 }
    }

    #[test]
    fn stale_desktop_entries_are_dropped() {
        let (names, unknown) = resolve(&[desktop(r"C:\x\gone.exe")], &mut |_, _| Liveness::Gone);
        assert!(names.is_empty());
        assert!(!unknown, "nothing left to check again");
    }

    #[test]
    fn only_desktop_apps_that_cannot_be_opened_keep_the_timer() {
        let (names, unknown) = resolve(&[desktop(r"C:\Apps\Zoom.exe")], &mut |_, _| Liveness::Running);
        assert_eq!(names.len(), 1);
        assert!(!unknown, "a watched process needs no timer");
        let (names, unknown) = resolve(&[desktop(r"C:\Apps\Zoom.exe")], &mut |_, _| Liveness::Unknown);
        assert_eq!(names.len(), 1, "an elevated app stays listed");
        assert!(unknown);
    }

    #[test]
    fn the_exact_exe_counts_not_its_name() {
        let entry = desktop_path("C:#Users#me#AppData#Local#Discord#app-1.0.9001#Discord.exe");
        assert!(same_image(&entry, r"c:\users\me\appdata\local\discord\app-1.0.9001\discord.exe"));
        assert!(!same_image(&entry, r"C:\Users\me\AppData\Local\Discord\app-1.0.9002\Discord.exe"), "another version");
        assert!(same_image(&desktop_path("C:#Code#C#Tools#rec.exe"), r"C:\Code\C#Tools\rec.exe"), "# in a folder name");
    }

    #[test]
    fn a_process_started_after_the_capture_is_a_relaunch() {
        let start = 134_377_551_868_306_419;
        assert!(ran_at(start - 5_000_000_000, start), "running long before");
        assert!(ran_at(start + 5_000_000, start), "half a second of slack");
        assert!(!ran_at(start + 600_000_000, start), "started a minute later");
        assert!(ran_at(u64::MAX, 134_000), "not a timestamp: not compared");
    }

    #[test]
    fn shortcut_follows_nectars_mute_not_only_the_devices() {
        let live = [Some(false), Some(false)];
        let stuck = [Some(true), Some(false)];
        assert!(toggle_target(&live, false), "nothing muted: mute");
        assert!(!toggle_target(&stuck, true), "one device refused the mute: the next press unmutes");
        assert!(toggle_target(&stuck, false), "one mic muted elsewhere: mute the rest");
        assert!(!toggle_target(&[Some(true), Some(true)], false), "all muted elsewhere: unmute");
        assert!(!toggle_target(&[Some(true), None], false), "an unreadable device is left out");
    }

    #[test]
    fn nectars_mute_shows_amber_while_a_mic_is_live() {
        assert_eq!(MuteView::of(&[Some(true), Some(true)], true), MuteView { muted: true, partial: false });
        assert_eq!(MuteView::of(&[Some(true), Some(false)], true), MuteView { muted: true, partial: true });
        assert_eq!(MuteView::of(&[Some(true), Some(false)], false), MuteView::default(), "muted elsewhere, not all");
        assert_eq!(MuteView::of(&[Some(true), Some(true)], false), MuteView { muted: true, partial: false }, "muted elsewhere");
        assert_eq!(MuteView::of(&[Some(false)], true), MuteView::default(), "nothing took the mute");
        assert_eq!(MuteView::of(&[None], true), MuteView::default());
        assert!(!mute_held(&[Some(false), Some(false)], true), "unmuted everywhere: Nectar's mute ends");
        assert!(mute_held(&[Some(false), Some(true)], true));
        assert!(!mute_held(&[Some(true)], false));
    }

    #[test]
    fn edge_cases_of_the_pure_helpers() {
        assert!(!in_use(5, 5), "stop equal to start is stopped");
        assert!(in_use(u64::MAX, 0));
        assert_eq!(exe_name(r"C:\"), "", "trailing separator");
        assert_eq!(exe_name("C:/x/Zoom.exe"), "zoom.exe", "forward slashes");
        assert_eq!(desktop_path("noslash.exe"), "noslash.exe");
        assert_eq!(package_fallback_name(""), "");
        assert_eq!(package_fallback_name("_8wekyb3d8bbwe"), "_8wekyb3d8bbwe");
        assert_eq!(package_fallback_name("Vendor.App_abc"), "App");
        assert_eq!(unique_names(Vec::<String>::new()), Vec::<String>::new());
        assert!(!all_muted([false]));
        assert_eq!(hotkey_from_setting(Some("	")), None);
    }

    #[test]
    fn packaged_entries_need_no_process_check() {
        let mut asked = false;
        let (names, unknown) = resolve(&[Entry::Packaged("MSTeams_8wekyb3d8bbwe".into())], &mut |_, _| {
            asked = true;
            Liveness::Gone
        });
        assert_eq!(names.len(), 1);
        assert!(!unknown && !asked, "packaged apps need no process check");
    }
}
