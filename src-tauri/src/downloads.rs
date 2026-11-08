//! Downloads in the notch: one thread watches the Downloads folder with ReadDirectoryChangesW and
//! follows browser temp files (.crdownload, .partial, .part) and plain new files until they are done.
//! Browsers expose no total size to other processes, so progress is bytes on disk and speed only.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

pub const SETTING: &str = "nectar-notch-downloads";
pub const FOLDER_SETTING: &str = "nectar-notch-downloads-folder";
const LEGACY_SETTING: &str = "nectar-legacy-mode";

const MIN_BYTES: u64 = 1024;
const TICK_MS: u64 = 1000;
const STALLED_TICK_MS: u64 = 5000;
const STALL_MS: u64 = 30_000;
const ABANDON_MS: u64 = 30 * 60_000;
const STABLE_MS: u64 = 2000;
const VANISH_MS: u64 = 1500;
const DONE_KEEP_MS: u64 = 6000;
const SPEED_WINDOW_MS: u64 = 3000;
const BURST_MS: u64 = 2000;
const BURST_MAX: usize = 3;
const RECENT_MS: u64 = 60_000;
const RETRY_MS: u32 = 60_000;
const TEMP_EXTS: [&str; 3] = [".crdownload", ".partial", ".part"];

/// What a stat of one file in the folder found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Probe {
    Gone,
    Unknown,
    Present { size: u64, modified: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added(String),
    Removed(String),
    Modified(String),
    Renamed(String, String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Temp,
    Plain,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Active,
    Vanishing(u64),
    Done(u64),
}

#[derive(Debug)]
struct Item {
    id: u64,
    file: String,
    kind: Kind,
    bytes: u64,
    started_wall: u64,
    changed: u64,
    samples: VecDeque<(u64, u64)>,
    phase: Phase,
}

impl Item {
    fn name(&self) -> String {
        match (self.kind, self.phase) {
            (Kind::Temp, Phase::Active | Phase::Vanishing(_)) => temp_final(&self.file).unwrap_or(&self.file).to_string(),
            _ => self.file.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DownloadView {
    pub id: u64,
    pub name: String,
    pub bytes: u64,
    pub speed: u64,
    pub done: bool,
}

/// Final name of a browser temp file (`a.zip.crdownload` → `a.zip`), None for any other name.
pub fn temp_final(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    TEMP_EXTS
        .iter()
        .find(|ext| lower.ends_with(*ext) && lower.len() > ext.len())
        .map(|ext| &name[..name.len() - ext.len()])
}

/// Files that are never downloads: Office lock files, hidden dot files, shell files and temp files.
pub fn ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.contains(':')
        || lower.starts_with("~$")
        || lower.starts_with('.')
        || lower == "desktop.ini"
        || lower == "thumbs.db"
        || lower.ends_with(".tmp")
        || lower.ends_with(".temp")
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Bytes per second over the samples kept (oldest to newest); 0 until half a second has passed.
pub fn speed(samples: &VecDeque<(u64, u64)>) -> u64 {
    let (Some(&(t0, b0)), Some(&(t1, b1))) = (samples.front(), samples.back()) else { return 0 };
    let dt = t1.saturating_sub(t0);
    if dt < 500 {
        return 0;
    }
    b1.saturating_sub(b0) * 1000 / dt
}

#[derive(Default)]
pub struct Tracker {
    next_id: u64,
    items: Vec<Item>,
    plain_times: VecDeque<u64>,
    burst_until: u64,
    wall_skew: i64,
}

impl Tracker {
    /// The tracker's clock is monotonic; this ties it to wall time for comparing file times.
    pub fn set_wall(&mut self, wall: u64, now: u64) {
        self.wall_skew = wall as i64 - now as i64;
    }

    fn wall(&self, now: u64) -> u64 {
        (now as i64).saturating_add(self.wall_skew).max(0) as u64
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|it| it.id == id)
    }

    fn live(&self, file: &str) -> Option<u64> {
        self.items.iter().find(|it| !matches!(it.phase, Phase::Done(_)) && same(&it.file, file)).map(|it| it.id)
    }

    fn remove(&mut self, id: u64) {
        self.items.retain(|it| it.id != id);
    }

    fn push(&mut self, file: &str, kind: Kind, bytes: u64, now: u64) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let started_wall = self.wall(now);
        self.items.push(Item { id, file: file.to_string(), kind, bytes, started_wall, changed: now, samples: VecDeque::from([(now, bytes)]), phase: Phase::Active });
        id
    }

    fn adopt(&mut self, file: &str, probe: &impl Fn(&str) -> Probe, now: u64) {
        let bytes = match probe(file) {
            Probe::Gone => return,
            Probe::Unknown => 0,
            Probe::Present { size, .. } => size,
        };
        self.push(file, Kind::Temp, bytes, now);
        let fin = temp_final(file).unwrap_or(file).to_string();
        self.items.retain(|it| !(it.kind == Kind::Plain && it.phase == Phase::Active && same(&it.file, &fin)));
    }

    fn finish(&mut self, id: u64, file: &str, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) {
        let Some(i) = self.index(id) else { return };
        let size = match probe(file) {
            Probe::Present { size, .. } => size,
            Probe::Unknown => self.items[i].bytes,
            Probe::Gone => 0,
        };
        if size < MIN_BYTES {
            self.remove(id);
            return;
        }
        self.items.retain(|it| it.id == id || !same(&it.file, file));
        let Some(i) = self.index(id) else { return };
        let it = &mut self.items[i];
        it.file = file.to_string();
        it.bytes = size;
        it.phase = Phase::Done(now);
        out.push(file.to_string());
    }

    /// The finished file of a temp that went away is there: a move or copy instead of a rename.
    fn try_final(&mut self, id: u64, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) -> bool {
        let Some(i) = self.index(id) else { return false };
        let it = &self.items[i];
        let Some(fin) = temp_final(&it.file).map(str::to_string) else { return false };
        match probe(&fin) {
            Probe::Present { size, modified } if size >= MIN_BYTES && modified + 2000 >= it.started_wall => {
                self.finish(id, &fin, probe, now, out);
                true
            }
            _ => false,
        }
    }

    fn vanished_final(&mut self, file: &str, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) -> bool {
        let found = self.items.iter().find(|it| {
            matches!(it.phase, Phase::Vanishing(_)) && temp_final(&it.file).is_some_and(|fin| same(fin, file))
        });
        match found.map(|it| it.id) {
            Some(id) => self.try_final(id, probe, now, out),
            None => false,
        }
    }

    fn lost(&mut self, id: u64, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) {
        let Some(i) = self.index(id) else { return };
        match self.items[i].kind {
            Kind::Plain => self.remove(id),
            Kind::Temp => {
                if self.items[i].phase == Phase::Active {
                    self.items[i].phase = Phase::Vanishing(now);
                }
                self.try_final(id, probe, now, out);
            }
        }
    }

    fn sample(&mut self, id: u64, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) {
        let Some(i) = self.index(id) else { return };
        let bytes = match probe(&self.items[i].file) {
            Probe::Gone => return self.lost(id, probe, now, out),
            Probe::Unknown => self.items[i].bytes,
            Probe::Present { size, .. } => size,
        };
        let it = &mut self.items[i];
        // A removed temp file that is there again is the same download, followed afresh.
        if matches!(it.phase, Phase::Vanishing(_)) {
            it.phase = Phase::Active;
            it.samples.clear();
            it.changed = now;
        }
        if bytes != it.bytes {
            it.bytes = bytes;
            it.changed = now;
        }
        it.samples.push_back((now, bytes));
        while it.samples.len() > 1 && it.samples.front().is_some_and(|&(t, _)| t + SPEED_WINDOW_MS < now) {
            it.samples.pop_front();
        }
    }

    fn added(&mut self, file: &str, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) {
        if ignored(file) {
            return;
        }
        if temp_final(file).is_some() {
            match self.live(file) {
                Some(id) => self.sample(id, probe, now, out),
                None => self.adopt(file, probe, now),
            }
            return;
        }
        if self.vanished_final(file, probe, now, out) || self.live(file).is_some() {
            return;
        }
        // Firefox makes an empty placeholder with the final name next to its `.part` file.
        if self.items.iter().any(|it| it.kind == Kind::Temp && temp_final(&it.file).is_some_and(|fin| same(fin, file))) {
            return;
        }
        self.plain_times.push_back(now);
        while self.plain_times.front().is_some_and(|&t| t + BURST_MS < now) {
            self.plain_times.pop_front();
        }
        // Many files at once is a copy or an unzip, not downloads.
        if now < self.burst_until || self.plain_times.len() > BURST_MAX {
            self.burst_until = now + BURST_MS;
            self.items.retain(|it| !(it.kind == Kind::Plain && it.phase == Phase::Active));
            return;
        }
        let bytes = match probe(file) {
            Probe::Gone => return,
            Probe::Unknown => 0,
            Probe::Present { size, .. } => size,
        };
        self.push(file, Kind::Plain, bytes, now);
    }

    /// A plain file that grows again while still listed as done (a slow copy that paused) is followed again.
    fn reopen(&mut self, file: &str, probe: &impl Fn(&str) -> Probe, now: u64) {
        let Some(it) = self.items.iter_mut().find(|it| it.kind == Kind::Plain && matches!(it.phase, Phase::Done(_)) && same(&it.file, file)) else { return };
        if let Probe::Present { size, .. } = probe(file) {
            if size != it.bytes {
                it.bytes = size;
                it.changed = now;
                it.samples = VecDeque::from([(now, size)]);
                it.phase = Phase::Active;
            }
        }
    }

    fn renamed(&mut self, old: &str, new: &str, probe: &impl Fn(&str) -> Probe, now: u64, out: &mut Vec<String>) {
        if let Some(id) = self.live(old) {
            let i = self.index(id).unwrap_or_default();
            match self.items[i].kind {
                Kind::Temp if temp_final(new).is_some() => {
                    self.items[i].file = new.to_string();
                    self.items[i].phase = Phase::Active;
                }
                Kind::Temp if ignored(new) => self.remove(id),
                Kind::Temp => self.finish(id, new, probe, now, out),
                Kind::Plain if ignored(new) || temp_final(new).is_some() => self.remove(id),
                Kind::Plain => self.items[i].file = new.to_string(),
            }
            return;
        }
        if ignored(new) {
            return;
        }
        if temp_final(new).is_some() {
            self.adopt(new, probe, now);
            return;
        }
        if self.vanished_final(new, probe, now, out) {
            return;
        }
        // A download that started before the watch (or never grew while watched) just finished.
        if temp_final(old).is_some() {
            let id = self.push(old, Kind::Temp, 0, now);
            self.finish(id, new, probe, now, out);
        }
    }

    /// Applies one folder change; returns the final names of downloads that finished.
    pub fn apply(&mut self, change: &Change, probe: &impl Fn(&str) -> Probe, now: u64) -> Vec<String> {
        let mut out = Vec::new();
        match change {
            Change::Added(file) => self.added(file, probe, now, &mut out),
            Change::Modified(file) => {
                if let Some(id) = self.live(file) {
                    self.sample(id, probe, now, &mut out);
                } else if temp_final(file).is_some() && !ignored(file) {
                    self.adopt(file, probe, now);
                } else if !self.vanished_final(file, probe, now, &mut out) {
                    self.reopen(file, probe, now);
                }
            }
            Change::Removed(file) => {
                if let Some(id) = self.live(file) {
                    self.lost(id, probe, now, &mut out);
                } else if probe(file) == Probe::Gone {
                    self.items.retain(|it| !(matches!(it.phase, Phase::Done(_)) && same(&it.file, file)));
                }
            }
            Change::Renamed(old, new) => self.renamed(old, new, probe, now, &mut out),
        }
        out
    }

    /// After a lost event list: picks up temp files written in the last minute that aren't followed yet.
    pub fn adopt_recent(&mut self, file: &str, probe: &impl Fn(&str) -> Probe, now: u64) {
        if ignored(file) || temp_final(file).is_none() || self.live(file).is_some() {
            return;
        }
        if matches!(probe(file), Probe::Present { modified, .. } if modified + RECENT_MS >= self.wall(now)) {
            self.adopt(file, probe, now);
        }
    }

    /// Re-reads sizes, finishes plain files whose size settled and expires finished and stale entries.
    pub fn tick(&mut self, probe: &impl Fn(&str) -> Probe, now: u64) -> Vec<String> {
        let mut out = Vec::new();
        let ids: Vec<u64> = self.items.iter().map(|it| it.id).collect();
        for id in ids {
            let Some(i) = self.index(id) else { continue };
            match self.items[i].phase {
                Phase::Active => {
                    self.sample(id, probe, now, &mut out);
                    let Some(i) = self.index(id) else { continue };
                    let it = &self.items[i];
                    if it.phase != Phase::Active {
                        continue;
                    }
                    match it.kind {
                        Kind::Plain if now.saturating_sub(it.changed) >= STABLE_MS => {
                            let file = it.file.clone();
                            self.finish(id, &file, probe, now, &mut out);
                        }
                        Kind::Temp if now.saturating_sub(it.changed) >= ABANDON_MS => self.remove(id),
                        _ => {}
                    }
                }
                Phase::Vanishing(since) => {
                    if !self.try_final(id, probe, now, &mut out) && now.saturating_sub(since) >= VANISH_MS {
                        self.remove(id);
                    }
                }
                Phase::Done(at) => {
                    if now.saturating_sub(at) >= DONE_KEEP_MS {
                        self.remove(id);
                    }
                }
            }
        }
        out
    }

    /// Milliseconds until the next tick is useful; None when nothing is followed (wait without a timeout).
    pub fn next_wake(&self, now: u64) -> Option<u64> {
        self.items
            .iter()
            .map(|it| match it.phase {
                Phase::Active if it.kind == Kind::Temp && now.saturating_sub(it.changed) >= STALL_MS => STALLED_TICK_MS,
                Phase::Active => TICK_MS,
                Phase::Vanishing(since) => (since + VANISH_MS).saturating_sub(now).clamp(50, TICK_MS),
                Phase::Done(at) => (at + DONE_KEEP_MS).saturating_sub(now).max(50),
            })
            .min()
    }

    pub fn view(&self) -> Vec<DownloadView> {
        self.items
            .iter()
            .filter(|it| match it.phase {
                Phase::Done(_) => true,
                Phase::Vanishing(_) => false,
                Phase::Active => it.kind == Kind::Temp || it.bytes >= MIN_BYTES,
            })
            .map(|it| DownloadView {
                id: it.id,
                name: it.name(),
                bytes: it.bytes,
                speed: if it.phase == Phase::Active { speed(&it.samples) } else { 0 },
                done: matches!(it.phase, Phase::Done(_)),
            })
            .collect()
    }

    /// The file on disk for an entry the list shows.
    pub fn file_of(&self, id: u64) -> Option<String> {
        self.items.iter().find(|it| it.id == id).map(|it| it.file.clone())
    }
}

const FILE_ACTION_ADDED: u32 = 1;
const FILE_ACTION_REMOVED: u32 = 2;
const FILE_ACTION_MODIFIED: u32 = 3;
const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

/// Reads a FILE_NOTIFY_INFORMATION list; a rename's old and new names become one change.
pub fn parse_changes(buf: &[u8]) -> Vec<Change> {
    let u32_at = |at: usize| buf.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    let mut old: Option<String> = None;
    let mut at = 0usize;
    loop {
        let (Some(next), Some(action), Some(len)) = (u32_at(at), u32_at(at + 4), u32_at(at + 8)) else { break };
        let Some(raw) = buf.get(at + 12..at + 12 + len as usize) else { break };
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let name = String::from_utf16_lossy(&units);
        if action != FILE_ACTION_RENAMED_NEW_NAME {
            if let Some(o) = old.take() {
                out.push(Change::Removed(o));
            }
        }
        match action {
            FILE_ACTION_ADDED => out.push(Change::Added(name)),
            FILE_ACTION_REMOVED => out.push(Change::Removed(name)),
            FILE_ACTION_MODIFIED => out.push(Change::Modified(name)),
            FILE_ACTION_RENAMED_OLD_NAME => old = Some(name),
            FILE_ACTION_RENAMED_NEW_NAME => out.push(match old.take() {
                Some(o) => Change::Renamed(o, name),
                None => Change::Added(name),
            }),
            _ => {}
        }
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    if let Some(o) = old {
        out.push(Change::Removed(o));
    }
    out
}

/// What the commands see: the last list sent and the file behind each entry. The tracker itself
/// stays on the watch thread, so file probes never run under this lock.
struct Shared {
    dir: Option<PathBuf>,
    shown: Vec<DownloadView>,
    files: Vec<(u64, String)>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { dir: None, shown: Vec::new(), files: Vec::new() });
static WAKE: OnceLock<isize> = OnceLock::new();

#[derive(Clone, PartialEq)]
struct Config {
    on: bool,
    dir: Option<PathBuf>,
}

impl Config {
    fn read(app: &AppHandle) -> Self {
        let get = |key: &str| crate::utils::get_setting_str(app, key);
        let on = get(SETTING).as_deref() != Some("false") && get(LEGACY_SETTING).as_deref() != Some("true");
        let dir = if on { get(FOLDER_SETTING).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).map(PathBuf::from).or_else(known_downloads) } else { None };
        Config { on, dir }
    }
}

fn known_downloads() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        text.map(PathBuf::from)
    }
}

/// Timers, expiry and speed run on a monotonic clock, so setting the system clock back cannot stall them.
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn wall_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn clock(tracker: &mut Tracker) -> u64 {
    let now = now_ms();
    tracker.set_wall(wall_ms(), now);
    now
}

fn probe_in(dir: &Path) -> impl Fn(&str) -> Probe + '_ {
    move |name: &str| match std::fs::metadata(dir.join(name)) {
        Ok(meta) if meta.is_file() => Probe::Present {
            size: meta.len(),
            modified: meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0),
        },
        Ok(_) => Probe::Gone,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Probe::Gone,
        Err(_) => Probe::Unknown,
    }
}

/// Sends the list when it changed and each finished file (the notch adds it to the Shelf).
fn publish(app: &AppHandle, dir: &Path, tracker: &Tracker, finished: Vec<String>) {
    let list = tracker.view();
    let files = list.iter().filter_map(|v| tracker.file_of(v.id).map(|f| (v.id, f))).collect();
    let changed = {
        let Ok(mut shared) = SHARED.lock() else { return };
        shared.files = files;
        if list == shared.shown {
            false
        } else {
            shared.shown = list.clone();
            true
        }
    };
    if changed {
        let _ = app.emit("downloads-changed", list);
    }
    for name in finished {
        let path = dir.join(&name);
        let _ = app.emit("download-finished", serde_json::json!({ "path": path.to_string_lossy(), "name": name }));
    }
}

enum Ended {
    Changed,
    Failed,
}

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
    if wake == 0 || WAKE.set(wake).is_err() {
        return;
    }
    std::thread::spawn(move || worker(app, HANDLE(wake as *mut _)));
}

fn worker(app: AppHandle, wake: HANDLE) {
    loop {
        let cfg = Config::read(&app);
        let ended = match (cfg.on, &cfg.dir) {
            (true, Some(dir)) => Some(watch(&app, dir, &cfg, wake)),
            _ => None,
        };
        if let Ok(mut shared) = SHARED.lock() {
            shared.dir = None;
        }
        publish(&app, Path::new(""), &Tracker::default(), Vec::new());
        match ended {
            Some(Ended::Changed) => continue,
            Some(Ended::Failed) => unsafe { WaitForSingleObject(wake, RETRY_MS) },
            None => unsafe { WaitForSingleObject(wake, INFINITE) },
        };
    }
}

fn watch(app: &AppHandle, dir: &Path, cfg: &Config, wake: HANDLE) -> Ended {
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(dir.as_os_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    };
    let Ok(handle) = handle else { return Ended::Failed };
    let Ok(event) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
        unsafe { let _ = CloseHandle(handle); }
        return Ended::Failed;
    };
    if let Ok(mut shared) = SHARED.lock() {
        shared.dir = Some(dir.to_path_buf());
    }
    let probe = probe_in(dir);
    let mut buf = vec![0u32; 16 * 1024];
    let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
    let arm = |buf: &mut Vec<u32>, ov: &mut OVERLAPPED| unsafe {
        ReadDirectoryChangesW(
            handle,
            buf.as_mut_ptr() as *mut _,
            (buf.len() * 4) as u32,
            false,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE,
            None,
            Some(ov as *mut _),
            None,
        )
        .is_ok()
    };
    let mut tracker = Tracker::default();
    let mut pending = arm(&mut buf, &mut ov);
    let mut due: Option<u64> = None;
    let ended = loop {
        if !pending {
            break Ended::Failed;
        }
        let now = clock(&mut tracker);
        if due.is_some_and(|d| now >= d) {
            due = None;
            let finished = tracker.tick(&probe, now);
            publish(app, dir, &tracker, finished);
        }
        // Folder events can come faster than the tick, so a due tick is never pushed back.
        if let Some(wait) = tracker.next_wake(now) {
            if due.is_none_or(|d| now + wait < d) {
                due = Some(now + wait);
            }
        }
        let timeout = due.map(|d| d.saturating_sub(now).min(u32::MAX as u64 - 1) as u32).unwrap_or(INFINITE);
        // The lowest signalled index wins and folder events can keep coming, so the settings wake goes first.
        let woke = unsafe { WaitForMultipleObjects(&[wake, event], false, timeout) };
        if woke == WAIT_OBJECT_0 {
            if Config::read(app) != *cfg {
                break Ended::Changed;
            }
        } else if woke.0 == WAIT_OBJECT_0.0 + 1 {
            let mut n = 0u32;
            if unsafe { GetOverlappedResult(handle, &ov, &mut n, false) }.is_err() {
                pending = false;
                continue;
            }
            let now = clock(&mut tracker);
            let finished = if n == 0 {
                // The event list overflowed: look at the folder instead.
                let names: Vec<String> = std::fs::read_dir(dir)
                    .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                    .unwrap_or_default();
                for name in &names {
                    tracker.adopt_recent(name, &probe, now);
                }
                tracker.tick(&probe, now)
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, (n as usize).min(buf.len() * 4)) };
                parse_changes(bytes).iter().flat_map(|c| tracker.apply(c, &probe, now)).collect()
            };
            publish(app, dir, &tracker, finished);
            unsafe { let _ = ResetEvent(event); }
            pending = arm(&mut buf, &mut ov);
        } else if woke != WAIT_TIMEOUT {
            break Ended::Failed;
        }
    };
    unsafe {
        if pending {
            let _ = CancelIoEx(handle, Some(&ov as *const _));
            let mut n = 0u32;
            let _ = GetOverlappedResult(handle, &ov, &mut n, true);
        }
        let _ = CloseHandle(handle);
        let _ = CloseHandle(event);
    }
    ended
}

#[tauri::command]
pub fn downloads_list() -> Vec<DownloadView> {
    SHARED.lock().map(|s| s.shown.clone()).unwrap_or_default()
}

/// Opens a finished download, or shows any listed one in Explorer.
#[tauri::command]
pub fn downloads_open(id: u64, reveal: bool) -> Result<(), String> {
    let (path, done) = {
        let shared = SHARED.lock().map_err(|e| e.to_string())?;
        let dir = shared.dir.clone().ok_or("not watching")?;
        let file = shared.files.iter().find(|(i, _)| *i == id).map(|(_, f)| f.clone()).ok_or("unknown download")?;
        let done = shared.shown.iter().any(|v| v.id == id && v.done);
        (dir.join(file), done)
    };
    if reveal || !done {
        tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
    } else {
        tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A fake folder: name → (size, modified).
    struct Folder(RefCell<HashMap<String, (u64, u64)>>);

    impl Folder {
        fn new() -> Self {
            Folder(RefCell::new(HashMap::new()))
        }
        fn put(&self, name: &str, size: u64, modified: u64) {
            self.0.borrow_mut().insert(name.to_string(), (size, modified));
        }
        fn rm(&self, name: &str) {
            self.0.borrow_mut().remove(name);
        }
        fn mv(&self, from: &str, to: &str) {
            let v = self.0.borrow_mut().remove(from).unwrap();
            self.0.borrow_mut().insert(to.to_string(), v);
        }
        fn probe(&self) -> impl Fn(&str) -> Probe + '_ {
            move |name| match self.0.borrow().get(name) {
                Some(&(size, modified)) => Probe::Present { size, modified },
                None => Probe::Gone,
            }
        }
    }

    fn added(n: &str) -> Change { Change::Added(n.into()) }
    fn modified(n: &str) -> Change { Change::Modified(n.into()) }
    fn removed(n: &str) -> Change { Change::Removed(n.into()) }
    fn renamed(a: &str, b: &str) -> Change { Change::Renamed(a.into(), b.into()) }

    const MB: u64 = 1024 * 1024;

    #[test]
    fn temp_names_map_to_their_final_name() {
        assert_eq!(temp_final("setup.exe.crdownload"), Some("setup.exe"));
        assert_eq!(temp_final("Report.PDF.CRDOWNLOAD"), Some("Report.PDF"));
        assert_eq!(temp_final("movie.mkv.part"), Some("movie.mkv"));
        assert_eq!(temp_final("a.zip.partial"), Some("a.zip"));
        assert_eq!(temp_final("Unconfirmed 123456.crdownload"), Some("Unconfirmed 123456"));
        assert_eq!(temp_final("notes.txt"), None);
        assert_eq!(temp_final(".part"), None);
        assert!(ignored("~$report.docx") && ignored("desktop.ini") && ignored("x.tmp") && ignored("a.zip:Zone.Identifier"));
        assert!(!ignored("report.docx"));
    }

    #[test]
    fn speed_is_bytes_per_second_over_the_window() {
        assert_eq!(speed(&VecDeque::new()), 0);
        assert_eq!(speed(&VecDeque::from([(0, 0)])), 0);
        assert_eq!(speed(&VecDeque::from([(0, 0), (200, 1000)])), 0);
        assert_eq!(speed(&VecDeque::from([(0, 0), (1000, 2 * MB), (2000, 4 * MB)])), 2 * MB);
        assert_eq!(speed(&VecDeque::from([(0, 5 * MB), (2000, 4 * MB)])), 0);
    }

    #[test]
    fn chrome_download_starts_grows_is_renamed_and_finishes() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("Unconfirmed 812.crdownload", 0, 1000);
        assert!(t.apply(&added("Unconfirmed 812.crdownload"), &f.probe(), 1000).is_empty());
        assert_eq!(t.view()[0].name, "Unconfirmed 812");

        f.mv("Unconfirmed 812.crdownload", "video.mp4.crdownload");
        t.apply(&renamed("Unconfirmed 812.crdownload", "video.mp4.crdownload"), &f.probe(), 1100);
        let v = t.view();
        assert_eq!((v.len(), v[0].name.as_str(), v[0].done), (1, "video.mp4", false));

        f.put("video.mp4.crdownload", 2 * MB, 2100);
        t.tick(&f.probe(), 2100);
        f.put("video.mp4.crdownload", 4 * MB, 3100);
        t.tick(&f.probe(), 3100);
        let v = t.view();
        assert_eq!(v[0].bytes, 4 * MB);
        assert_eq!(v[0].speed, 4 * MB * 1000 / 2100, "4 MB since the first sample, 2.1 s ago");
        f.put("video.mp4.crdownload", 6 * MB, 4100);
        t.tick(&f.probe(), 4100);
        assert_eq!(t.view()[0].speed, 2 * MB, "samples older than the window are dropped");
        assert_eq!(t.next_wake(4100), Some(TICK_MS));

        f.mv("video.mp4.crdownload", "video.mp4");
        let done = t.apply(&renamed("video.mp4.crdownload", "video.mp4"), &f.probe(), 4500);
        assert_eq!(done, vec!["video.mp4".to_string()]);
        let v = t.view();
        assert_eq!((v.len(), v[0].name.as_str(), v[0].done, v[0].speed), (1, "video.mp4", true, 0));
        assert_eq!(t.file_of(v[0].id).as_deref(), Some("video.mp4"));

        // The rename is followed by a write event for the final file: no second download.
        assert!(t.apply(&modified("video.mp4"), &f.probe(), 4600).is_empty());
        assert_eq!(t.view().len(), 1);

        assert_eq!(t.next_wake(4600), Some(DONE_KEEP_MS - 100));
        t.tick(&f.probe(), 4500 + DONE_KEEP_MS);
        assert!(t.view().is_empty());
        assert_eq!(t.next_wake(4500 + DONE_KEEP_MS), None);
    }

    #[test]
    fn cancelled_download_disappears() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("big.iso.crdownload", 3 * MB, 1000);
        t.apply(&added("big.iso.crdownload"), &f.probe(), 1000);
        f.rm("big.iso.crdownload");
        assert!(t.apply(&removed("big.iso.crdownload"), &f.probe(), 2000).is_empty());
        assert!(t.view().is_empty(), "a vanishing temp is hidden");
        assert!(t.tick(&f.probe(), 2000 + VANISH_MS).is_empty());
        assert!(t.items.is_empty());
    }

    #[test]
    fn temp_that_disappears_during_a_tick_counts_as_cancelled() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.rm("a.zip.crdownload");
        t.tick(&f.probe(), 2000);
        t.tick(&f.probe(), 2000 + VANISH_MS);
        assert!(t.items.is_empty());
    }

    #[test]
    fn temp_removed_after_its_final_file_was_written_is_done() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.put("a.zip", 3 * MB, 1900);
        f.rm("a.zip.crdownload");
        assert_eq!(t.apply(&removed("a.zip.crdownload"), &f.probe(), 2000), vec!["a.zip".to_string()]);
    }

    #[test]
    fn old_file_with_the_final_name_does_not_finish_a_cancelled_download() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip", 3 * MB, 10);
        f.put("a.zip.crdownload", 3 * MB, 100_000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 100_000);
        f.rm("a.zip.crdownload");
        assert!(t.apply(&removed("a.zip.crdownload"), &f.probe(), 101_000).is_empty());
        assert!(t.tick(&f.probe(), 101_000 + VANISH_MS).is_empty());
        assert!(t.items.is_empty());
    }

    #[test]
    fn firefox_part_file_with_placeholder_finishes_once() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("song.flac", 0, 1000);
        t.apply(&added("song.flac"), &f.probe(), 1000);
        f.put("song.flac.part", 0, 1000);
        t.apply(&added("song.flac.part"), &f.probe(), 1010);
        assert_eq!(t.view().len(), 1, "the empty placeholder is not a download");
        f.put("song.flac.part", 5 * MB, 4000);
        t.tick(&f.probe(), 4000);
        f.rm("song.flac");
        t.apply(&removed("song.flac"), &f.probe(), 4500);
        f.mv("song.flac.part", "song.flac");
        let done = t.apply(&renamed("song.flac.part", "song.flac"), &f.probe(), 4500);
        assert_eq!(done, vec!["song.flac".to_string()]);
        assert_eq!(t.tick(&f.probe(), 7000), Vec::<String>::new());
        assert_eq!(t.view().len(), 1);
    }

    #[test]
    fn plain_new_file_finishes_when_its_size_settles() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("scan.pdf", 2 * MB, 1000);
        t.apply(&added("scan.pdf"), &f.probe(), 1000);
        assert!(t.tick(&f.probe(), 2000).is_empty());
        f.put("scan.pdf", 3 * MB, 2500);
        assert!(t.tick(&f.probe(), 2500).is_empty());
        assert!(t.tick(&f.probe(), 4000).is_empty());
        assert_eq!(t.tick(&f.probe(), 4500), vec!["scan.pdf".to_string()]);
    }

    #[test]
    fn small_and_temporary_files_are_ignored() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("tiny.txt", 200, 1000);
        t.apply(&added("tiny.txt"), &f.probe(), 1000);
        assert!(t.view().is_empty(), "under 1 KB is not shown");
        assert!(t.tick(&f.probe(), 4000).is_empty());
        assert!(t.items.is_empty());

        for name in ["~$doc.docx", "x.tmp", "desktop.ini"] {
            f.put(name, MB, 5000);
            t.apply(&added(name), &f.probe(), 5000);
        }
        assert!(t.items.is_empty());

        f.put("small.bin.crdownload", 100, 6000);
        t.apply(&added("small.bin.crdownload"), &f.probe(), 6000);
        f.mv("small.bin.crdownload", "small.bin");
        assert!(t.apply(&renamed("small.bin.crdownload", "small.bin"), &f.probe(), 6500).is_empty());
        assert!(t.items.is_empty());
    }

    #[test]
    fn a_burst_of_plain_files_is_a_copy_not_downloads() {
        let f = Folder::new();
        let mut t = Tracker::default();
        for i in 0..6 {
            let name = format!("photo{i}.jpg");
            f.put(&name, MB, 1000);
            t.apply(&added(&name), &f.probe(), 1000 + i * 10);
        }
        assert!(t.items.is_empty());
        assert!(t.tick(&f.probe(), 5000).is_empty());

        f.put("later.pdf", MB, 10_000);
        t.apply(&added("later.pdf"), &f.probe(), 10_000);
        assert_eq!(t.view().len(), 1);
    }

    #[test]
    fn renaming_an_existing_file_is_not_a_download() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("b.pdf", MB, 1000);
        t.apply(&renamed("a.pdf", "b.pdf"), &f.probe(), 1000);
        t.apply(&modified("b.pdf"), &f.probe(), 1000);
        assert!(t.items.is_empty());
    }

    #[test]
    fn download_started_before_the_watch_finishes_on_its_rename() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("old.zip", 2 * MB, 1000);
        assert_eq!(t.apply(&renamed("old.zip.crdownload", "old.zip"), &f.probe(), 1000), vec!["old.zip".to_string()]);
    }

    #[test]
    fn untracked_temp_is_adopted_when_written_and_stale_ones_are_not_picked_up() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("stale.zip.crdownload", MB, 0);
        t.adopt_recent("stale.zip.crdownload", &f.probe(), 10 * RECENT_MS);
        assert!(t.items.is_empty());
        f.put("fresh.zip.crdownload", MB, 10 * RECENT_MS - 1000);
        t.adopt_recent("fresh.zip.crdownload", &f.probe(), 10 * RECENT_MS);
        assert_eq!(t.view().len(), 1);
        t.apply(&modified("stale.zip.crdownload"), &f.probe(), 10 * RECENT_MS);
        assert_eq!(t.view().len(), 2);
    }

    #[test]
    fn stalled_download_ticks_slower_and_is_dropped_when_abandoned() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("p.zip.crdownload", MB, 0);
        t.apply(&added("p.zip.crdownload"), &f.probe(), 0);
        t.tick(&f.probe(), STALL_MS);
        assert_eq!(t.next_wake(STALL_MS), Some(STALLED_TICK_MS));
        assert_eq!(t.view()[0].speed, 0);
        t.tick(&f.probe(), ABANDON_MS);
        assert!(t.items.is_empty());
    }

    #[test]
    fn temp_recreated_while_vanishing_comes_back() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.rm("a.zip.crdownload");
        t.apply(&removed("a.zip.crdownload"), &f.probe(), 2000);
        f.put("a.zip.crdownload", MB, 2200);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 2200);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes, v[0].done), (1, MB, false));
        f.put("a.zip.crdownload", 2 * MB, 3200);
        t.tick(&f.probe(), 2000 + VANISH_MS);
        t.tick(&f.probe(), 3200);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes, v[0].speed), (1, 2 * MB, MB), "followed again, speed from the new file only");
    }

    #[test]
    fn size_event_for_a_vanishing_temp_that_is_back_revives_it() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.rm("a.zip.crdownload");
        t.apply(&removed("a.zip.crdownload"), &f.probe(), 2000);
        f.put("a.zip.crdownload", MB, 2500);
        t.apply(&modified("a.zip.crdownload"), &f.probe(), 2500);
        t.tick(&f.probe(), 2000 + VANISH_MS);
        assert_eq!(t.view().len(), 1);
    }

    #[test]
    fn vanish_timeout_is_not_pushed_back_by_more_events() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.rm("a.zip.crdownload");
        t.apply(&removed("a.zip.crdownload"), &f.probe(), 2000);
        t.apply(&modified("a.zip.crdownload"), &f.probe(), 3000);
        t.tick(&f.probe(), 2000 + VANISH_MS);
        assert!(t.items.is_empty());
    }

    #[test]
    fn plain_file_written_again_while_listed_as_done_is_followed_again() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("copy.iso", 2 * MB, 1000);
        t.apply(&added("copy.iso"), &f.probe(), 1000);
        assert_eq!(t.tick(&f.probe(), 3000), vec!["copy.iso".to_string()]);
        t.apply(&modified("copy.iso"), &f.probe(), 3500);
        assert!(t.view()[0].done, "no new bytes: stays done");
        f.put("copy.iso", 3 * MB, 4000);
        t.apply(&modified("copy.iso"), &f.probe(), 4000);
        let v = t.view();
        assert_eq!((v.len(), v[0].done, v[0].bytes), (1, false, 3 * MB));
        assert!(t.tick(&f.probe(), 5000).is_empty());
        assert_eq!(t.tick(&f.probe(), 6000), vec!["copy.iso".to_string()]);
    }

    #[test]
    fn finished_temp_download_is_not_reopened_by_later_writes() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 2 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.mv("a.zip.crdownload", "a.zip");
        t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 2000);
        f.put("a.zip", 3 * MB, 2500);
        t.apply(&modified("a.zip"), &f.probe(), 2500);
        assert!(t.view()[0].done);
    }

    #[test]
    fn file_times_use_the_wall_clock_and_timers_the_monotonic_one() {
        let wall = 1_800_000_000_000;
        let f = Folder::new();
        let mut t = Tracker::default();
        t.set_wall(wall, 1000);
        f.put("old.zip", 3 * MB, wall - 3_600_000);
        f.put("old.zip.crdownload", 3 * MB, wall);
        t.apply(&added("old.zip.crdownload"), &f.probe(), 1000);
        f.put("new.zip.crdownload", 3 * MB, wall);
        t.apply(&added("new.zip.crdownload"), &f.probe(), 1000);

        t.set_wall(wall + 1000, 2000);
        f.rm("old.zip.crdownload");
        assert!(t.apply(&removed("old.zip.crdownload"), &f.probe(), 2000).is_empty(), "final file older than the start");
        f.put("new.zip", 3 * MB, wall + 900);
        f.rm("new.zip.crdownload");
        assert_eq!(t.apply(&removed("new.zip.crdownload"), &f.probe(), 2000), vec!["new.zip".to_string()]);

        // The clock is set back an hour: the finished entry still goes after its 6 s.
        t.tick(&f.probe(), 2000 + VANISH_MS);
        assert_eq!(t.items.len(), 1, "the cancelled one is gone");
        t.set_wall(wall - 3_600_000, 4000);
        assert_eq!(t.next_wake(4000), Some(DONE_KEEP_MS - 2000));
        t.tick(&f.probe(), 2000 + DONE_KEEP_MS);
        assert!(t.view().is_empty());

        t.set_wall(wall, 10_000);
        f.put("recent.zip.crdownload", MB, wall - 5000);
        t.adopt_recent("recent.zip.crdownload", &f.probe(), 10_000);
        f.put("stale.zip.crdownload", MB, wall - 2 * RECENT_MS);
        t.adopt_recent("stale.zip.crdownload", &f.probe(), 10_000);
        assert_eq!(t.view().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["recent.zip"]);
    }

    #[test]
    fn finished_name_downloaded_again_leaves_one_entry() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.mv("a.zip.crdownload", "a.zip");
        t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 2000);
        f.put("a.zip.crdownload", 2 * MB, 3000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 3000);
        f.mv("a.zip.crdownload", "a.zip");
        assert_eq!(t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 4000), vec!["a.zip".to_string()]);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes), (1, 2 * MB));
    }

    #[test]
    fn temp_renamed_to_an_ignored_name_is_dropped() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.mv("a.zip.crdownload", "a.tmp");
        assert!(t.apply(&renamed("a.zip.crdownload", "a.tmp"), &f.probe(), 1500).is_empty());
        assert!(t.items.is_empty());
    }

    #[test]
    fn temp_names_match_case_insensitively_through_a_rename() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("Movie.MKV.CRDOWNLOAD", 3 * MB, 1000);
        t.apply(&added("Movie.MKV.CRDOWNLOAD"), &f.probe(), 1000);
        t.apply(&modified("movie.mkv.crdownload"), &f.probe(), 1500);
        assert_eq!(t.view().len(), 1);
        f.mv("Movie.MKV.CRDOWNLOAD", "Movie.MKV");
        assert_eq!(t.apply(&renamed("movie.mkv.crdownload", "Movie.MKV"), &f.probe(), 2000), vec!["Movie.MKV".to_string()]);
    }

    #[test]
    fn two_parallel_downloads_finish_independently() {
        let f = Folder::new();
        let mut t = Tracker::default();
        for n in ["a.zip", "b.zip"] {
            f.put(&format!("{n}.crdownload"), 2 * MB, 1000);
            t.apply(&added(&format!("{n}.crdownload")), &f.probe(), 1000);
        }
        f.mv("a.zip.crdownload", "a.zip");
        t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 2000);
        let v = t.view();
        assert_eq!((v.len(), v.iter().filter(|d| d.done).count()), (2, 1));
        assert_eq!(t.next_wake(2000), Some(TICK_MS), "the running one keeps the tick fast");
    }

    #[test]
    fn locked_temp_keeps_its_last_size() {
        let mut t = Tracker::default();
        let known = |_: &str| Probe::Present { size: 2 * MB, modified: 1000 };
        t.apply(&added("a.zip.crdownload"), &known, 1000);
        t.tick(&|_: &str| Probe::Unknown, 2000);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes), (1, 2 * MB));
    }

    #[test]
    fn truncated_or_oversized_notify_entries_do_not_panic() {
        let buf = notify(&[(FILE_ACTION_ADDED, "a.zip.crdownload"), (FILE_ACTION_MODIFIED, "a.zip.crdownload")]);
        for cut in 0..buf.len() {
            let _ = parse_changes(&buf[..cut]);
        }
        let mut bad = notify(&[(FILE_ACTION_ADDED, "x")]);
        bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_changes(&bad).is_empty());
        let mut loop_back = notify(&[(FILE_ACTION_ADDED, "x"), (FILE_ACTION_ADDED, "y")]);
        loop_back[0..4].copy_from_slice(&0x7fff_fff0u32.to_le_bytes());
        assert_eq!(parse_changes(&loop_back), vec![added("x")]);
    }

    fn notify(entries: &[(u32, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (i, (action, name)) in entries.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            let len = units.len() * 2;
            let size = (12 + len + 3) & !3;
            let next = if i + 1 == entries.len() { 0 } else { size as u32 };
            buf.extend_from_slice(&next.to_le_bytes());
            buf.extend_from_slice(&action.to_le_bytes());
            buf.extend_from_slice(&(len as u32).to_le_bytes());
            for u in units {
                buf.extend_from_slice(&u.to_le_bytes());
            }
            buf.resize(buf.len() + size - 12 - len, 0);
        }
        buf
    }

    #[test]
    fn revived_temp_still_finishes_by_rename_with_its_new_size() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 3 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.rm("a.zip.crdownload");
        t.apply(&removed("a.zip.crdownload"), &f.probe(), 2000);
        f.put("a.zip.crdownload", 2 * MB, 2300);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 2300);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes, v[0].done), (1, 2 * MB, false), "an added event revives it too");
        f.put("a.zip.crdownload", 5 * MB, 3000);
        f.mv("a.zip.crdownload", "a.zip");
        assert_eq!(t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 3000), vec!["a.zip".to_string()]);
        let v = t.view();
        assert_eq!((v.len(), v[0].bytes, v[0].done), (1, 5 * MB, true));
        // The revived entry must not be dropped by the old vanish deadline.
        t.tick(&f.probe(), 2000 + VANISH_MS + 10);
        assert_eq!(t.view().len(), 1);
    }

    #[test]
    fn plain_file_truncated_after_done_is_dropped_without_a_second_finish() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("copy.iso", 2 * MB, 1000);
        t.apply(&added("copy.iso"), &f.probe(), 1000);
        assert_eq!(t.tick(&f.probe(), 3000), vec!["copy.iso".to_string()]);
        f.put("copy.iso", 10, 3500);
        t.apply(&modified("copy.iso"), &f.probe(), 3500);
        assert!(t.view().is_empty(), "under 1 KB is hidden while followed");
        assert!(t.tick(&f.probe(), 3500 + STABLE_MS).is_empty());
        assert!(t.items.is_empty());
    }

    #[test]
    fn finished_file_deleted_while_listed_is_dropped() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("scan.pdf", 2 * MB, 1000);
        t.apply(&added("scan.pdf"), &f.probe(), 1000);
        assert_eq!(t.tick(&f.probe(), 3000), vec!["scan.pdf".to_string()]);
        f.rm("scan.pdf");
        t.apply(&removed("scan.pdf"), &f.probe(), 4000);
        assert!(t.view().is_empty());

        f.put("a.zip.crdownload", 2 * MB, 5000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 5000);
        f.mv("a.zip.crdownload", "a.zip");
        assert_eq!(t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 5500), vec!["a.zip".to_string()]);
        f.rm("a.zip");
        t.apply(&removed("a.zip"), &f.probe(), 6000);
        assert!(t.view().is_empty());
        assert_eq!(t.next_wake(6000), None);
    }

    #[test]
    fn removal_of_a_finished_file_that_is_still_there_keeps_it() {
        let f = Folder::new();
        let mut t = Tracker::default();
        f.put("a.zip.crdownload", 2 * MB, 1000);
        t.apply(&added("a.zip.crdownload"), &f.probe(), 1000);
        f.mv("a.zip.crdownload", "a.zip");
        t.apply(&renamed("a.zip.crdownload", "a.zip"), &f.probe(), 1500);
        t.apply(&removed("a.zip"), &f.probe(), 2000);
        let v = t.view();
        assert_eq!((v.len(), v[0].done), (1, true));
    }

    #[test]
    fn wall_clock_behind_the_monotonic_one_does_not_underflow() {
        let f = Folder::new();
        let mut t = Tracker::default();
        t.set_wall(100, 5_000_000);
        assert_eq!(t.wall(1000), 0);
        f.put("a.zip.crdownload", MB, 0);
        t.adopt_recent("a.zip.crdownload", &f.probe(), 1000);
        assert_eq!(t.view().len(), 1);
    }

    #[test]
    fn notify_buffer_pairs_renames() {
        let buf = notify(&[
            (FILE_ACTION_ADDED, "Unconfirmed 1.crdownload"),
            (FILE_ACTION_RENAMED_OLD_NAME, "Unconfirmed 1.crdownload"),
            (FILE_ACTION_RENAMED_NEW_NAME, "über.zip.crdownload"),
            (FILE_ACTION_MODIFIED, "über.zip.crdownload"),
            (FILE_ACTION_RENAMED_OLD_NAME, "gone.txt"),
            (FILE_ACTION_REMOVED, "x.part"),
            (FILE_ACTION_RENAMED_NEW_NAME, "moved-in.pdf"),
        ]);
        assert_eq!(
            parse_changes(&buf),
            vec![
                added("Unconfirmed 1.crdownload"),
                renamed("Unconfirmed 1.crdownload", "über.zip.crdownload"),
                modified("über.zip.crdownload"),
                removed("gone.txt"),
                removed("x.part"),
                added("moved-in.pdf"),
            ]
        );
        assert!(parse_changes(&buf[..5]).is_empty());
    }
}
