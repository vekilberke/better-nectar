//! Clipboard history for the notch. A message-only window listens for WM_CLIPBOARDUPDATE while the panel is enabled
//! or phone clipboard sync is on (T-016, text only). Text, images and copied files (their paths) are kept, with the
//! app they came from. Items stay in memory unless persistence is on. Clipboard contents are never logged.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use image::codecs::png::{CompressionType, FilterType as PngFilter};
use image::{ExtendedColorType, ImageEncoder, ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowThreadProcessId,
    PostMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY,
    WNDCLASSW,
};

const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;
const CF_HDROP: u32 = 15;
const MAX_FILES: usize = 1000;
/// Paths sent to the notch per item; the rest is only counted.
const VIEW_FILES: usize = 3;
const MAX_CHARS: usize = 100_000;
const PREVIEW_CHARS: usize = 300;
const MIN_LIMIT: usize = 5;
const MAX_LIMIT: usize = 100;
const FILE_NAME: &str = "clipboard_history.json";
const IMAGE_DIR: &str = "clipboard_images";
/// Largest single image taken from the clipboard, as raw bytes (also bounds decoded size).
const MAX_IMAGE_RAW: u64 = 32 * 1024 * 1024;
/// Total size of the stored PNGs; the oldest images go first when it is exceeded.
const MAX_TOTAL_IMAGE_BYTES: usize = 120 * 1024 * 1024;
const THUMB_SIZE: u32 = 240;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "lowercase")]
enum Kind {
    #[default]
    Text,
    Image,
    Files,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Entry {
    id: u64,
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    text: String,
    at: i64,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    hash: u64,
    /// Full-size PNG. Kept in memory, and as a file next to the history when persistence is on.
    #[serde(skip)]
    png: Arc<Vec<u8>>,
    #[serde(skip)]
    thumb: String,
    #[serde(default)]
    files: Vec<String>,
    /// Display name of the app that put the item on the clipboard, empty when unknown.
    #[serde(default)]
    source: String,
}

/// What the notch gets: a shortened preview or a small thumbnail, never the full content.
#[derive(Clone, Serialize)]
pub struct ClipView {
    id: u64,
    kind: Kind,
    preview: String,
    chars: usize,
    at: i64,
    width: u32,
    height: u32,
    thumb: Option<String>,
    files: Vec<String>,
    file_count: usize,
    source: String,
}

struct Store {
    items: Vec<Entry>,
    next_id: u64,
    limit: usize,
    persist: bool,
    loaded: bool,
    enabled: bool,
}

struct NewImage {
    png: Arc<Vec<u8>>,
    thumb: String,
    width: u32,
    height: u32,
    hash: u64,
}

impl Store {
    const fn new() -> Self {
        Store { items: Vec::new(), next_id: 1, limit: 20, persist: false, loaded: false, enabled: false }
    }

    fn insert(&mut self, mut entry: Entry) {
        entry.id = self.next_id;
        self.next_id += 1;
        self.items.insert(0, entry);
        self.items.truncate(self.limit);
    }

    /// Newest first. A repeat of the newest item is ignored, an older repeat moves to the front.
    fn push(&mut self, text: String, at: i64) -> bool {
        let same = |e: &Entry| e.kind == Kind::Text && e.text == text;
        if text.trim().is_empty() || self.items.first().is_some_and(same) {
            return false;
        }
        self.items.retain(|e| !same(e));
        self.insert(Entry { kind: Kind::Text, text, at, ..Default::default() });
        true
    }

    fn push_files(&mut self, files: Vec<String>, at: i64) -> bool {
        let same = |e: &Entry| e.kind == Kind::Files && e.files == files;
        if files.is_empty() || self.items.first().is_some_and(same) {
            return false;
        }
        self.items.retain(|e| !same(e));
        self.insert(Entry { kind: Kind::Files, files, at, ..Default::default() });
        true
    }

    fn push_image(&mut self, image: NewImage, at: i64) -> bool {
        let same = |e: &Entry| e.kind == Kind::Image && e.hash == image.hash && e.width == image.width && e.height == image.height;
        if self.items.first().is_some_and(same) {
            return false;
        }
        self.items.retain(|e| !same(e));
        self.insert(Entry {
            kind: Kind::Image,
            at,
            width: image.width,
            height: image.height,
            hash: image.hash,
            png: image.png,
            thumb: image.thumb,
            ..Default::default()
        });
        self.enforce_image_budget(MAX_TOTAL_IMAGE_BYTES);
        true
    }

    /// Drops the oldest images until the stored PNGs fit. The newest item always stays.
    fn enforce_image_budget(&mut self, cap: usize) {
        let mut total: usize = self.items.iter().map(|e| e.png.len()).sum();
        let mut i = self.items.len();
        while total > cap && i > 1 {
            i -= 1;
            if self.items[i].kind == Kind::Image {
                total -= self.items[i].png.len();
                self.items.remove(i);
            }
        }
    }

    fn views(&self) -> Vec<ClipView> {
        self.items
            .iter()
            .map(|e| {
                let mut view = ClipView {
                    id: e.id,
                    kind: e.kind,
                    preview: String::new(),
                    chars: 0,
                    at: e.at,
                    width: 0,
                    height: 0,
                    thumb: None,
                    files: Vec::new(),
                    file_count: 0,
                    source: e.source.clone(),
                };
                match e.kind {
                    Kind::Text => {
                        // Leading blank lines would leave the card empty.
                        view.preview = e.text.trim_start().chars().take(PREVIEW_CHARS).collect();
                        view.chars = e.text.chars().count();
                    }
                    Kind::Image => {
                        view.width = e.width;
                        view.height = e.height;
                        view.thumb = Some(e.thumb.clone());
                    }
                    Kind::Files => {
                        view.files = e.files.iter().take(VIEW_FILES).cloned().collect();
                        view.file_count = e.files.len();
                    }
                }
                view
            })
            .collect()
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());
// Held across every change plus its file write, so configure, clipboard updates and edits never interleave
// and an older snapshot cannot overwrite a newer one. Always taken before STORE.
static GATE: Mutex<()> = Mutex::new(());
// Newest configure call applied, so a call that arrives late is dropped.
static LAST_GEN: AtomicU64 = AtomicU64::new(0);

fn gate() -> std::sync::MutexGuard<'static, ()> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}
static APP: OnceLock<AppHandle> = OnceLock::new();
static WANT: AtomicBool = AtomicBool::new(false);
// The two users of the listener; it runs while either wants it. WANT_LOCK orders their start/stop.
static HISTORY_ON: AtomicBool = AtomicBool::new(false);
static PHONE_ON: AtomicBool = AtomicBool::new(false);
static WANT_LOCK: Mutex<()> = Mutex::new(());
static ALIVE: AtomicBool = AtomicBool::new(false);
static LISTENER: AtomicIsize = AtomicIsize::new(0);
// Sequence number of our own write, so putting an item back is not recorded again.
static SELF_SEQ: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

fn file_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(FILE_NAME))
}

fn image_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(IMAGE_DIR))
}

fn remove_saved(app: &AppHandle) {
    if let Some(path) = file_path(app) {
        let _ = std::fs::remove_file(path);
    }
    if let Some(dir) = image_dir(app) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The hash is part of the name so an id that gets reused never points at an older picture.
fn image_file_name(e: &Entry) -> String {
    format!("{}-{:016x}.png", e.id, e.hash)
}

fn save(app: &AppHandle, items: &[Entry]) {
    let Some(path) = file_path(app) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Images go to their own files so the JSON stays small.
    if let Some(dir) = image_dir(app) {
        let images: Vec<&Entry> = items.iter().filter(|e| e.kind == Kind::Image).collect();
        if images.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
        } else {
            let _ = std::fs::create_dir_all(&dir);
            for e in &images {
                let file = dir.join(image_file_name(e));
                // Same id with other content (ids restart after a crash) must not reuse a stale file.
                if std::fs::metadata(&file).map(|m| m.len() != e.png.len() as u64).unwrap_or(true) {
                    let tmp = file.with_extension("png.tmp");
                    if std::fs::write(&tmp, e.png.as_slice()).is_ok() && std::fs::rename(&tmp, &file).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
            if let Ok(read) = std::fs::read_dir(&dir) {
                for f in read.flatten() {
                    let keep = f
                        .file_name()
                        .to_str()
                        .is_some_and(|name| images.iter().any(|e| image_file_name(e) == name));
                    if !keep {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
            }
        }
    }
    if let Ok(json) = serde_json::to_string(items) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn load(app: &AppHandle) -> Vec<Entry> {
    let saved: Vec<Entry> = file_path(app)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let dir = image_dir(app);
    saved
        .into_iter()
        .filter_map(|mut e| {
            if e.kind == Kind::Image {
                let bytes = std::fs::read(dir.as_ref()?.join(image_file_name(&e))).ok()?;
                let (w, h, rgba) = decode_png(&bytes)?;
                e.thumb = make_thumb(w, h, &rgba)?;
                e.width = w;
                e.height = h;
                e.png = Arc::new(bytes);
            }
            Some(e)
        })
        .collect()
}

fn announce(views: Vec<ClipView>) {
    if let Some(app) = APP.get() {
        let _ = app.emit("clipboard-history-changed", views);
    }
}

/// Turn the listener on or off and apply the size and persistence settings.
pub fn configure(app: &AppHandle, generation: u64, enabled: bool, size: usize, persist: bool) {
    let _ = APP.set(app.clone());
    let _gate = gate();
    if generation < LAST_GEN.load(Ordering::SeqCst) {
        return;
    }
    LAST_GEN.store(generation, Ordering::SeqCst);
    let (changed, views, snapshot) = {
        let Ok(mut store) = STORE.lock() else { return };
        let mut changed = false;
        let limit = size.clamp(MIN_LIMIT, MAX_LIMIT);
        store.limit = limit;
        if store.items.len() > limit {
            store.items.truncate(limit);
            changed = true;
        }
        if enabled && persist && !store.loaded {
            store.loaded = true;
            if store.items.is_empty() {
                let mut saved = load(app);
                saved.truncate(limit);
                store.items = saved;
                store.enforce_image_budget(MAX_TOTAL_IMAGE_BYTES);
                let saved = std::mem::take(&mut store.items);
                store.next_id = saved.iter().map(|e| e.id).max().unwrap_or(0) + 1;
                store.items = saved;
                changed = true;
            }
        }
        if !persist || !enabled {
            remove_saved(app);
        }
        let persist_turned_on = persist && !store.persist;
        store.persist = persist;
        store.enabled = enabled;
        if !enabled && !store.items.is_empty() {
            store.items.clear();
            changed = true;
        }
        if !enabled {
            store.loaded = false;
        }
        let snapshot = (enabled && persist && (changed || persist_turned_on)).then(|| store.items.clone());
        (changed, store.views(), snapshot)
    };
    if let Some(items) = snapshot {
        save(app, &items);
    }
    if changed {
        announce(views);
    }
    HISTORY_ON.store(enabled, Ordering::SeqCst);
    apply_want();
}

/// Phone clipboard sync turned on or off (`phone_link::clip_sync`).
#[cfg(feature = "phone-link")]
pub fn set_phone_sync(on: bool) {
    PHONE_ON.store(on, Ordering::SeqCst);
    apply_want();
}

fn apply_want() {
    let _lock = WANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if HISTORY_ON.load(Ordering::SeqCst) || PHONE_ON.load(Ordering::SeqCst) {
        start();
    } else {
        stop();
    }
}

fn start() {
    WANT.store(true, Ordering::SeqCst);
    if !ALIVE.swap(true, Ordering::SeqCst) {
        std::thread::spawn(run);
    }
}

fn stop() {
    WANT.store(false, Ordering::SeqCst);
    let raw = LISTENER.load(Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Sleeps in short steps and returns early once the listener is no longer wanted.
fn sleep_while_wanted(seconds: u64) {
    for _ in 0..seconds * 4 {
        if !WANT.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn run() {
    let mut ran = unsafe { listen() };
    // Setting up the listener can fail while the system is busy, so keep trying, slower each time, while it is wanted.
    let mut delay = 2;
    while !ran && WANT.load(Ordering::SeqCst) {
        sleep_while_wanted(delay);
        if !WANT.load(Ordering::SeqCst) {
            break;
        }
        ran = unsafe { listen() };
        delay = (delay * 2).min(30);
    }
    LISTENER.store(0, Ordering::SeqCst);
    ALIVE.store(false, Ordering::SeqCst);
    // Turned back on while the old window was closing.
    if WANT.load(Ordering::SeqCst) {
        start();
    }
}

unsafe fn listen() -> bool {
    let instance = GetModuleHandleW(None).unwrap_or_default().into();
    let class = w!("NectarClipboardListener");
    let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance, lpszClassName: class, ..Default::default() };
    // Registering twice fails once the class exists, which is fine.
    RegisterClassW(&wc);
    let Ok(hwnd) = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        PCWSTR::null(),
        WINDOW_STYLE::default(),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(instance),
        None,
    ) else {
        return false;
    };
    if AddClipboardFormatListener(hwnd).is_err() {
        let _ = DestroyWindow(hwnd);
        return false;
    }
    LISTENER.store(hwnd.0 as isize, Ordering::SeqCst);
    if !WANT.load(Ordering::SeqCst) {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    true
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            on_update(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // A write must not pick up a window that is already going away.
            let _ = LISTENER.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst);
            let _ = RemoveClipboardFormatListener(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

enum Captured {
    Text(String),
    /// Text over `MAX_CHARS`: skipped, only reported to phone sync.
    TooLong,
    Png(Vec<u8>),
    Dib(Vec<u8>),
    Files(Vec<String>),
}

fn on_update(hwnd: HWND) {
    let seq = unsafe { GetClipboardSequenceNumber() };
    // Putting a history item back is a copy too, for the phone, but not a new history entry.
    let history = HISTORY_ON.load(Ordering::SeqCst) && seq != SELF_SEQ.load(Ordering::SeqCst);
    let phone = PHONE_ON.load(Ordering::SeqCst);
    if !history && !phone {
        return;
    }
    let Some(captured) = (unsafe { read_clip(hwnd, history) }) else { return };
    #[cfg(feature = "phone-link")]
    if phone {
        match &captured {
            Captured::Text(text) => crate::phone_link::clip_sync::on_copy(text.clone(), seq),
            Captured::TooLong => crate::phone_link::clip_sync::on_too_long(),
            _ => {}
        }
    }
    if !history {
        return;
    }
    let source = unsafe { owner_app() };
    // Decoding and thumbnailing happen before the gate so a big image does not block other edits.
    let image = match &captured {
        Captured::Text(_) | Captured::TooLong | Captured::Files(_) => None,
        Captured::Png(bytes) => match build_from_png(bytes) {
            Some(img) => Some(img),
            None => return,
        },
        Captured::Dib(bytes) => match build_from_dib(bytes) {
            Some(img) => Some(img),
            None => return,
        },
    };
    let Some(app) = APP.get() else { return };
    let _gate = gate();
    let (views, snapshot) = {
        let Ok(mut store) = STORE.lock() else { return };
        let pushed = match (captured, image) {
            (Captured::Text(text), _) => store.enabled && store.push(text, now_ms()),
            (Captured::Files(files), _) => store.enabled && store.push_files(files, now_ms()),
            (_, Some(image)) => store.enabled && store.push_image(image, now_ms()),
            _ => false,
        };
        if !pushed {
            return;
        }
        if let Some(first) = store.items.first_mut() {
            first.source = source;
        }
        (store.views(), store.persist.then(|| store.items.clone()))
    };
    if let Some(items) = snapshot {
        save(app, &items);
    }
    announce(views);
}

/// The app that owns the clipboard now, by its friendly name. Empty for our own writes or when unknown.
unsafe fn owner_app() -> String {
    let Ok(owner) = GetClipboardOwner() else { return String::new() };
    if owner.0.is_null() {
        return String::new();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(owner, Some(&mut pid));
    if pid == 0 || pid == std::process::id() {
        return String::new();
    }
    crate::commands::process_image_path(pid)
        .map(|path| crate::commands::friendly_process_name(&path))
        .unwrap_or_default()
}

unsafe fn open(hwnd: Option<HWND>) -> bool {
    for _ in 0..8 {
        if OpenClipboard(hwnd).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

/// A DWORD format that password managers and Windows' own history honor. Zero means keep this out.
unsafe fn flag_is_zero(name: PCWSTR) -> bool {
    let format = RegisterClipboardFormatW(name);
    if format == 0 || IsClipboardFormatAvailable(format).is_err() {
        return false;
    }
    let Ok(handle) = GetClipboardData(format) else { return false };
    let mem = HGLOBAL(handle.0);
    let ptr = GlobalLock(mem) as *const u32;
    if ptr.is_null() {
        return false;
    }
    let zero = GlobalSize(mem) >= 4 && *ptr == 0;
    let _ = GlobalUnlock(mem);
    zero
}

/// Copies a clipboard format out as bytes. Must be called with the clipboard open.
unsafe fn read_bytes(format: u32) -> Option<Vec<u8>> {
    if IsClipboardFormatAvailable(format).is_err() {
        return None;
    }
    let handle = GetClipboardData(format).ok()?;
    let mem = HGLOBAL(handle.0);
    let size = GlobalSize(mem);
    if size == 0 || size as u64 > MAX_IMAGE_RAW {
        return None;
    }
    let ptr = GlobalLock(mem) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
    let _ = GlobalUnlock(mem);
    Some(bytes)
}

unsafe fn read_files_open() -> Option<Vec<String>> {
    if IsClipboardFormatAvailable(CF_HDROP).is_err() {
        return None;
    }
    let handle = GetClipboardData(CF_HDROP).ok()?;
    let hdrop = HDROP(handle.0);
    let count = DragQueryFileW(hdrop, u32::MAX, None) as usize;
    let mut files = Vec::with_capacity(count.min(MAX_FILES));
    for i in 0..count.min(MAX_FILES) as u32 {
        let len = DragQueryFileW(hdrop, i, None) as usize;
        let mut buf = vec![0u16; len + 1];
        DragQueryFileW(hdrop, i, Some(&mut buf));
        files.push(String::from_utf16_lossy(&buf[..len]));
    }
    (!files.is_empty()).then_some(files)
}

enum TextRead {
    Absent,
    TooLong,
    Text(String),
}

unsafe fn read_text_open() -> TextRead {
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
        return TextRead::Absent;
    }
    let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else { return TextRead::Absent };
    let mem = HGLOBAL(handle.0);
    let ptr = GlobalLock(mem) as *const u16;
    if ptr.is_null() {
        return TextRead::Absent;
    }
    let max = GlobalSize(mem) / 2;
    let mut len = 0;
    while len < max && *ptr.add(len) != 0 {
        len += 1;
    }
    let text = if len <= MAX_CHARS {
        TextRead::Text(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)))
    } else {
        TextRead::TooLong
    };
    let _ = GlobalUnlock(mem);
    text
}

/// What Ctrl+V would paste: copied files, then text, otherwise an image. Files alone and images only with `images`.
unsafe fn read_clip(hwnd: HWND, images: bool) -> Option<Captured> {
    let png_format = RegisterClipboardFormatW(w!("PNG"));
    let has = |f: u32| f != 0 && IsClipboardFormatAvailable(f).is_ok();
    let any_image = images && (has(png_format) || has(CF_DIBV5) || has(CF_DIB));
    if !((images && has(CF_HDROP)) || has(CF_UNICODETEXT) || any_image) {
        return None;
    }
    // Presence alone means "do not monitor".
    let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    if has(exclude) {
        return None;
    }
    if !open(Some(hwnd)) {
        return None;
    }
    let captured = (|| {
        if flag_is_zero(w!("CanIncludeInClipboardHistory")) || flag_is_zero(w!("CanUploadToCloudClipboard")) {
            return None;
        }
        // Copied files (Explorer) come first, as a paste would.
        if let Some(files) = read_files_open() {
            return Some(Captured::Files(files));
        }
        match read_text_open() {
            TextRead::Text(text) if !text.trim().is_empty() => return Some(Captured::Text(text)),
            // Text wins over an image, so oversized text is skipped, not recorded as a picture.
            TextRead::TooLong => return Some(Captured::TooLong),
            _ => {}
        }
        if !images {
            return None;
        }
        if has(png_format) {
            if let Some(bytes) = read_bytes(png_format) {
                return Some(Captured::Png(bytes));
            }
        }
        read_bytes(CF_DIBV5).or_else(|| read_bytes(CF_DIB)).map(Captured::Dib)
    })();
    let _ = CloseClipboard();
    captured
}

/// Wait briefly for the listener window when it is being restarted, so a copy-back does not fail.
fn listener_window() -> Result<HWND, String> {
    for _ in 0..100 {
        let raw = LISTENER.load(Ordering::SeqCst);
        if raw != 0 {
            return Ok(HWND(raw as *mut _));
        }
        if !WANT.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err("clipboard history is not running".into())
}

/// Replaces the clipboard with the given formats. Our own write is not recorded again.
/// Without the history listener (panel off) the clipboard gets no owner window, which is fine for these formats.
unsafe fn write_formats(formats: &[(u32, &[u8])]) -> Result<(), String> {
    let hwnd = listener_window().ok();
    if !open(hwnd) {
        return Err("clipboard is busy".into());
    }
    let result = (|| {
        EmptyClipboard().map_err(|e| e.to_string())?;
        for (format, bytes) in formats {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
            let ptr = GlobalLock(mem) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("could not lock memory".to_string());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(*format, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e.to_string());
            }
        }
        SELF_SEQ.store(GetClipboardSequenceNumber(), Ordering::SeqCst);
        Ok(())
    })();
    let _ = CloseClipboard();
    result
}

unsafe fn write_text(text: &str) -> Result<(), String> {
    let bytes: Vec<u8> = text.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect();
    write_formats(&[(CF_UNICODETEXT, &bytes)])
}

/// A DROPFILES block (CF_HDROP): 20 byte header, then wide paths each ending in NUL, then one more NUL.
fn dropfiles_bytes(files: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&20u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for unit in files.iter().flat_map(|f| f.encode_utf16().chain(std::iter::once(0))).chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

unsafe fn write_files(files: &[String]) -> Result<(), String> {
    let drop = dropfiles_bytes(files);
    let effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
    let copy = 1u32.to_le_bytes();
    if effect == 0 {
        return write_formats(&[(CF_HDROP, &drop)]);
    }
    write_formats(&[(CF_HDROP, &drop), (effect, &copy)])
}

unsafe fn write_image(png: &[u8]) -> Result<(), String> {
    let (w, h, rgba) = decode_png(png).ok_or("image could not be read")?;
    let dib = rgba_to_dib(w, h, &rgba);
    let png_format = RegisterClipboardFormatW(w!("PNG"));
    if png_format == 0 {
        return write_formats(&[(CF_DIB, &dib)]);
    }
    write_formats(&[(CF_DIB, &dib), (png_format, png)])
}

// ---- image helpers (pure) ----

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Decodes a PNG, refusing anything whose decoded size would exceed the image limit.
fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let reader = image::ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let (w, h) = reader.into_dimensions().ok()?;
    if w == 0 || h == 0 || w as u64 * h as u64 * 4 > MAX_IMAGE_RAW {
        return None;
    }
    let img = image::load_from_memory_with_format(bytes, ImageFormat::Png).ok()?.to_rgba8();
    Some((w, h, img.into_raw()))
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    // Fast compression: screenshots are large and this runs on the listener thread.
    image::codecs::png::PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::Adaptive)
        .write_image(rgba, w, h, ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

/// A small data URL for the panel: JPEG when the image is opaque, PNG when it has transparency.
fn make_thumb(w: u32, h: u32, rgba: &[u8]) -> Option<String> {
    let (tw, th, pixels) = if w <= THUMB_SIZE && h <= THUMB_SIZE {
        (w, h, rgba.to_vec())
    } else {
        let scale = THUMB_SIZE as f64 / w.max(h) as f64;
        let tw = ((w as f64 * scale).round() as u32).max(1);
        let th = ((h as f64 * scale).round() as u32).max(1);
        let src = RgbaImage::from_raw(w, h, rgba.to_vec())?;
        (tw, th, image::imageops::thumbnail(&src, tw, th).into_raw())
    };
    let translucent = pixels.chunks_exact(4).any(|p| p[3] < 255);
    let mut out = Vec::new();
    let mime = if translucent {
        image::codecs::png::PngEncoder::new(&mut out).write_image(&pixels, tw, th, ExtendedColorType::Rgba8).ok()?;
        "image/png"
    } else {
        let rgb: Vec<u8> = pixels.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .write_image(&rgb, tw, th, ExtendedColorType::Rgb8)
            .ok()?;
        "image/jpeg"
    };
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(out)))
}

fn finish_image(w: u32, h: u32, rgba: &[u8], png: Vec<u8>) -> Option<NewImage> {
    Some(NewImage { thumb: make_thumb(w, h, rgba)?, hash: fnv1a(rgba), width: w, height: h, png: Arc::new(png) })
}

fn build_from_png(bytes: &[u8]) -> Option<NewImage> {
    let (w, h, rgba) = decode_png(bytes)?;
    finish_image(w, h, &rgba, bytes.to_vec())
}

fn build_from_dib(bytes: &[u8]) -> Option<NewImage> {
    let (w, h, rgba) = dib_to_rgba(bytes)?;
    let png = encode_png(w, h, &rgba)?;
    finish_image(w, h, &rgba, png)
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 255;
    }
    let bits = mask.count_ones();
    let v = (value & mask) >> mask.trailing_zeros();
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        (v * 255 / ((1u32 << bits) - 1)) as u8
    }
}

/// Reads a packed DIB (CF_DIB / CF_DIBV5): 24 bit, or 32 bit with or without bit masks.
fn dib_to_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if dib.len() < 40 {
        return None;
    }
    let header = le32(dib, 0) as usize;
    let width = le32(dib, 4) as i32;
    let height = le32(dib, 8) as i32;
    let bpp = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = le32(dib, 16);
    let colors = le32(dib, 32) as usize;
    if header < 40 || header > dib.len() || width <= 0 || height == 0 {
        return None;
    }
    let (w, h) = (width as u32, height.unsigned_abs());
    if w as u64 * h as u64 * 4 > MAX_IMAGE_RAW {
        return None;
    }
    let bitfields = match (bpp, compression) {
        (24, 0) | (32, 0) => false,
        (32, 3) => true,
        _ => return None,
    };
    let (mut rm, mut gm, mut bm, mut am) = (0xFF0000, 0xFF00, 0xFF, 0xFF000000);
    let mut offset = header + colors.min(256) * 4;
    if bitfields {
        if dib.len() < 52 {
            return None;
        }
        (rm, gm, bm) = (le32(dib, 40), le32(dib, 44), le32(dib, 48));
        am = if header >= 56 { le32(dib, 52) } else { 0 };
        if header == 40 {
            offset += 12;
        }
    }
    let bytes_per_pixel = (bpp / 8) as usize;
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    let rows = h as usize;
    if dib.len() < offset.checked_add(stride.checked_mul(rows)?)? {
        return None;
    }
    let top_down = height < 0;
    let mut rgba = Vec::with_capacity(w as usize * rows * 4);
    for y in 0..rows {
        let row = if top_down { y } else { rows - 1 - y };
        let line = &dib[offset + row * stride..];
        for x in 0..w as usize {
            let px = &line[x * bytes_per_pixel..];
            if bpp == 24 {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            } else {
                let v = le32(px, 0);
                rgba.extend_from_slice(&[channel(v, rm), channel(v, gm), channel(v, bm), channel(v, am)]);
            }
        }
    }
    // Many apps leave the alpha byte of a 32 bit DIB at zero; treat that as fully opaque.
    if bpp == 32 && rgba.chunks_exact(4).all(|p| p[3] == 0) {
        rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Some((w, h, rgba))
}

/// A 32 bit bottom-up DIB for CF_DIB.
fn rgba_to_dib(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + rgba.len());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for row in (0..h as usize).rev() {
        for p in rgba[row * w as usize * 4..(row + 1) * w as usize * 4].chunks_exact(4) {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

fn mutate(f: impl FnOnce(&mut Store) -> bool) {
    let Some(app) = APP.get() else { return };
    let _gate = gate();
    let (views, snapshot) = {
        let Ok(mut store) = STORE.lock() else { return };
        if !f(&mut store) {
            return;
        }
        (store.views(), store.persist.then(|| store.items.clone()))
    };
    if let Some(items) = snapshot {
        save(app, &items);
    }
    announce(views);
}

#[tauri::command(async)]
pub fn clipboard_history_configure(app: AppHandle, generation: u64, enabled: bool, size: usize, persist: bool) {
    configure(&app, generation, enabled, size, persist);
}

#[tauri::command(async)]
pub fn clipboard_history_get() -> Vec<ClipView> {
    STORE.lock().map(|s| s.views()).unwrap_or_default()
}

#[tauri::command(async)]
pub fn clipboard_history_copy(id: u64) -> Result<(), String> {
    let (kind, text, png, files) = STORE
        .lock()
        .map_err(|e| e.to_string())?
        .items
        .iter()
        .find(|e| e.id == id)
        .map(|e| (e.kind, e.text.clone(), Arc::clone(&e.png), e.files.clone()))
        .ok_or("item no longer exists")?;
    unsafe {
        match kind {
            Kind::Text => write_text(&text)?,
            Kind::Image => write_image(&png)?,
            Kind::Files => write_files(&files)?,
        }
    };
    mutate(|store| {
        let Some(pos) = store.items.iter().position(|e| e.id == id) else { return false };
        if pos == 0 {
            return false;
        }
        let entry = store.items.remove(pos);
        store.items.insert(0, entry);
        true
    });
    Ok(())
}

#[tauri::command(async)]
pub fn clipboard_history_delete(id: u64) {
    mutate(|store| {
        let before = store.items.len();
        store.items.retain(|e| e.id != id);
        store.items.len() != before
    });
}

/// Puts files on the clipboard as Explorer's Copy does (used by the shelf).
#[tauri::command(async)]
pub fn clipboard_copy_files(paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Err("nothing to copy".into());
    }
    unsafe { write_files(&paths) }
}

#[tauri::command(async)]
pub fn clipboard_history_clear() {
    mutate(|store| {
        let had = !store.items.is_empty();
        store.items.clear();
        had
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(limit: usize) -> Store {
        Store { items: Vec::new(), next_id: 1, limit, persist: false, loaded: false, enabled: false }
    }

    #[test]
    fn newest_first_and_capped() {
        let mut s = store(3);
        for t in ["a", "b", "c", "d"] {
            assert!(s.push(t.into(), 0));
        }
        let texts: Vec<_> = s.items.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["d", "c", "b"]);
    }

    #[test]
    fn consecutive_duplicate_is_ignored() {
        let mut s = store(5);
        assert!(s.push("x".into(), 0));
        assert!(!s.push("x".into(), 1));
        assert_eq!(s.items.len(), 1);
    }

    #[test]
    fn older_duplicate_moves_to_front() {
        let mut s = store(5);
        s.push("a".into(), 0);
        s.push("b".into(), 0);
        assert!(s.push("a".into(), 0));
        let texts: Vec<_> = s.items.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["a", "b"]);
    }

    #[test]
    fn blank_text_is_ignored() {
        let mut s = store(5);
        assert!(!s.push("  \n\t".into(), 0));
        assert!(s.items.is_empty());
    }

    #[test]
    fn preview_is_shortened() {
        let mut s = store(5);
        s.push("é".repeat(PREVIEW_CHARS + 50), 0);
        let view = &s.views()[0];
        assert_eq!(view.preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(view.chars, PREVIEW_CHARS + 50);
    }

    #[test]
    fn preview_starts_at_the_first_visible_character() {
        let mut s = store(5);
        s.push(format!("{}hello", "
".repeat(PREVIEW_CHARS + 10)), 0);
        let view = &s.views()[0];
        assert!(view.preview.starts_with("hello"));
        assert_eq!(view.chars, PREVIEW_CHARS + 15);
    }

    #[test]
    fn ids_never_repeat_and_duplicate_gets_a_new_id() {
        let mut s = store(5);
        s.push("a".into(), 0);
        let first = s.items[0].id;
        s.push("b".into(), 0);
        s.push("a".into(), 0);
        assert_ne!(s.items[0].id, first);
        let mut ids: Vec<_> = s.items.iter().map(|e| e.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), s.items.len());
    }

    #[test]
    fn duplicate_at_the_cap_does_not_evict_others() {
        let mut s = store(3);
        for t in ["a", "b", "c"] {
            s.push(t.into(), 0);
        }
        assert!(s.push("a".into(), 0));
        let texts: Vec<_> = s.items.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["a", "c", "b"]);
    }

    #[test]
    fn whitespace_variants_are_distinct_but_kept_verbatim() {
        let mut s = store(5);
        s.push("x".into(), 0);
        assert!(s.push("x ".into(), 0));
        assert_eq!(s.items[0].text, "x ");
    }

    #[test]
    fn limit_of_one_keeps_only_the_newest() {
        let mut s = store(1);
        s.push("a".into(), 0);
        s.push("b".into(), 0);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].text, "b");
    }

    #[test]
    fn views_report_char_count_not_bytes_and_keep_timestamp() {
        let mut s = store(5);
        s.push("日本語".into(), 42);
        let v = &s.views()[0];
        assert_eq!(v.chars, 3);
        assert_eq!(v.at, 42);
        assert_eq!(v.preview, "日本語");
    }

    #[test]
    fn lowering_the_limit_then_pushing_trims_to_the_new_limit() {
        let mut s = store(5);
        for t in ["a", "b", "c", "d", "e"] {
            s.push(t.into(), 0);
        }
        s.limit = 2;
        s.push("f".into(), 0);
        let texts: Vec<_> = s.items.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["f", "e"]);
    }

    #[test]
    fn saved_entries_round_trip_through_json() {
        let mut s = store(5);
        let text = "line1\nline2 \"quoted\" \u{1F600}";
        s.push(text.into(), 7);
        s.push("b".into(), 8);
        let json = serde_json::to_string(&s.items).unwrap();
        let back: Vec<Entry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].text, "b");
        assert_eq!(back[1].text, text);
        assert_eq!(back[1].at, 7);
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;

    fn store(limit: usize) -> Store {
        Store { items: Vec::new(), next_id: 1, limit, persist: false, loaded: false, enabled: false }
    }

    fn fake(hash: u64, bytes: usize) -> NewImage {
        NewImage { png: vec![0; bytes].into(), thumb: "data:".into(), width: 2, height: 2, hash }
    }

    fn pixels(w: u32, h: u32) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn dib_round_trips_through_rgba() {
        let rgba: Vec<u8> = pixels(3, 2).chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let dib = rgba_to_dib(3, 2, &rgba);
        let (w, h, back) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(back, rgba);
    }

    #[test]
    fn dib_with_zero_alpha_is_treated_as_opaque() {
        let mut rgba = pixels(2, 2);
        rgba.chunks_exact_mut(4).for_each(|p| p[3] = 0);
        let (_, _, back) = dib_to_rgba(&rgba_to_dib(2, 2, &rgba)).unwrap();
        assert!(back.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn dib_24_bit_with_row_padding() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&(-2i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[3, 2, 1, 0]);
        dib.extend_from_slice(&[6, 5, 4, 0]);
        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (1, 2));
        assert_eq!(rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn dib_with_bit_masks_without_alpha_mask_is_opaque() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&3u32.to_le_bytes());
        dib.extend_from_slice(&[0u8; 20]);
        for mask in [0xFF0000u32, 0xFF00, 0xFF] {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[10, 20, 30, 128]);
        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(rgba, [30, 20, 10, 255]);
    }

    #[test]
    fn broken_or_oversized_dibs_are_rejected() {
        assert!(dib_to_rgba(&[0; 10]).is_none());
        let mut dib = rgba_to_dib(2, 2, &pixels(2, 2));
        dib.truncate(50);
        assert!(dib_to_rgba(&dib).is_none());
        let mut huge = rgba_to_dib(1, 1, &pixels(1, 1));
        huge[4..8].copy_from_slice(&100_000i32.to_le_bytes());
        huge[8..12].copy_from_slice(&100_000i32.to_le_bytes());
        assert!(dib_to_rgba(&huge).is_none());
    }

    #[test]
    fn png_round_trip_and_hash_are_stable() {
        let rgba = pixels(4, 3);
        let png = encode_png(4, 3, &rgba).unwrap();
        let (w, h, back) = decode_png(&png).unwrap();
        assert_eq!((w, h, &back), (4, 3, &rgba));
        assert_eq!(fnv1a(&rgba), fnv1a(&back));
        assert!(decode_png(b"not a png").is_none());
    }

    #[test]
    fn thumbnails_are_bounded_and_pick_a_format() {
        let opaque: Vec<u8> = pixels(600, 300).chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        assert!(make_thumb(600, 300, &opaque).unwrap().starts_with("data:image/jpeg;base64,"));
        let mut clear = opaque.clone();
        clear[3] = 0;
        assert!(make_thumb(600, 300, &clear).unwrap().starts_with("data:image/png;base64,"));
        let small = make_thumb(10, 10, &pixels(10, 10)).unwrap();
        assert!(small.starts_with("data:image/"));
    }

    #[test]
    fn repeated_image_is_deduplicated_like_text() {
        let mut s = store(5);
        assert!(s.push_image(fake(1, 4), 0));
        assert!(!s.push_image(fake(1, 4), 1));
        s.push("t".into(), 2);
        assert!(s.push_image(fake(1, 4), 3));
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.items[0].kind, Kind::Image);
    }

    #[test]
    fn text_and_image_share_the_cap_and_text_never_matches_an_image() {
        let mut s = store(2);
        s.push("a".into(), 0);
        s.push_image(fake(1, 4), 0);
        s.push("b".into(), 0);
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.items[0].text, "b");
        assert_eq!(s.items[1].kind, Kind::Image);
    }

    #[test]
    fn image_budget_drops_oldest_images_but_keeps_the_newest() {
        let mut s = store(10);
        s.push_image(fake(1, 60), 0);
        s.push("keep".into(), 0);
        s.push_image(fake(2, 60), 0);
        s.push_image(fake(3, 60), 0);
        s.enforce_image_budget(100);
        let hashes: Vec<_> = s.items.iter().filter(|e| e.kind == Kind::Image).map(|e| e.hash).collect();
        assert_eq!(hashes, [3]);
        assert!(s.items.iter().any(|e| e.text == "keep"));
        s.enforce_image_budget(1);
        assert_eq!(s.items[0].hash, 3);
    }

    #[test]
    fn image_views_carry_a_thumbnail_and_size() {
        let mut s = store(5);
        s.push_image(NewImage { png: vec![1].into(), thumb: "data:x".into(), width: 7, height: 9, hash: 5 }, 3);
        let v = &s.views()[0];
        assert_eq!((v.kind, v.width, v.height, v.at), (Kind::Image, 7, 9, 3));
        assert_eq!(v.thumb.as_deref(), Some("data:x"));
        assert!(v.preview.is_empty());
    }

    #[test]
    fn old_saved_text_entries_without_a_kind_still_load() {
        let back: Vec<Entry> = serde_json::from_str(r#"[{"id":4,"text":"hi","at":9}]"#).unwrap();
        assert_eq!(back[0].kind, Kind::Text);
        assert_eq!(back[0].text, "hi");
    }

    #[test]
    fn files_are_deduplicated_and_views_carry_a_few_paths_and_the_count() {
        let mut s = store(5);
        let files: Vec<String> = (0..5).map(|i| format!("C:\\Temp\\f{i}.png")).collect();
        assert!(s.push_files(files.clone(), 1));
        assert!(!s.push_files(files.clone(), 2));
        assert!(!s.push_files(Vec::new(), 2));
        s.push("t".into(), 3);
        assert!(s.push_files(files.clone(), 4));
        assert_eq!(s.items.len(), 2);
        let v = &s.views()[0];
        assert_eq!((v.kind, v.file_count, v.files.len()), (Kind::Files, 5, VIEW_FILES));
        assert_eq!(v.files[0], files[0]);
    }

    #[test]
    fn dropfiles_block_has_header_and_double_nul() {
        let bytes = dropfiles_bytes(&["C:\\a".into(), "b".into()]);
        assert_eq!(&bytes[..4], &20u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &1u32.to_le_bytes());
        let wide: Vec<u16> = bytes[20..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let expected: Vec<u16> = "C:\\a\0b\0\0".encode_utf16().collect();
        assert_eq!(wide, expected);
    }

    #[test]
    fn saved_entries_without_files_or_source_still_load() {
        let json = r#"[{"id":4,"kind":"files","files":["C:\\x.txt"],"at":9,"source":"Explorer"},{"id":5,"text":"hi","at":9}]"#;
        let back: Vec<Entry> = serde_json::from_str(json).unwrap();
        assert_eq!(back[0].kind, Kind::Files);
        assert_eq!(back[0].files, ["C:\\x.txt"]);
        assert_eq!(back[0].source, "Explorer");
        assert!(back[1].files.is_empty() && back[1].source.is_empty());
    }

    #[test]
    fn saved_image_entries_keep_the_bytes_out_of_the_json() {
        let mut s = store(5);
        s.push_image(fake(8, 1000), 1);
        let json = serde_json::to_string(&s.items).unwrap();
        assert!(json.len() < 200);
        assert!(json.contains("\"image\""));
    }
}

#[cfg(test)]
mod image_edge_tests {
    use super::*;

    fn header(size: u32, w: i32, h: i32, bpp: u16, compression: u32) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&size.to_le_bytes());
        d.extend_from_slice(&w.to_le_bytes());
        d.extend_from_slice(&h.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&bpp.to_le_bytes());
        d.extend_from_slice(&compression.to_le_bytes());
        d.resize(size as usize, 0);
        d
    }

    #[test]
    fn dibv5_bitfields_header_reads_alpha_mask() {
        let mut dib = header(124, 1, 1, 32, 3);
        for (i, mask) in [0xFF0000u32, 0xFF00, 0xFF, 0xFF000000].iter().enumerate() {
            dib[40 + i * 4..44 + i * 4].copy_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[10, 20, 30, 128]);
        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(rgba, [30, 20, 10, 128]);
    }

    #[test]
    fn top_down_32_bit_keeps_row_order() {
        let mut dib = header(40, 1, -2, 32, 0);
        dib.extend_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(rgba, [3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn unsupported_depths_and_bad_dimensions_are_rejected_not_panicking() {
        for bpp in [1u16, 4, 8, 16] {
            let mut dib = header(40, 2, 2, bpp, 0);
            dib.extend_from_slice(&[0; 64]);
            assert!(dib_to_rgba(&dib).is_none(), "bpp {bpp}");
        }
        let mut zero_w = header(40, 0, 2, 32, 0);
        zero_w.extend_from_slice(&[0; 16]);
        assert!(dib_to_rgba(&zero_w).is_none());
        let mut zero_h = header(40, 2, 0, 32, 0);
        zero_h.extend_from_slice(&[0; 16]);
        assert!(dib_to_rgba(&zero_h).is_none());
        let mut min_h = header(40, 2, i32::MIN, 32, 0);
        min_h.extend_from_slice(&[0; 16]);
        assert!(dib_to_rgba(&min_h).is_none());
        let mut bad_hdr = header(40, 1, 1, 32, 0);
        bad_hdr[0..4].copy_from_slice(&4000u32.to_le_bytes());
        bad_hdr.extend_from_slice(&[0; 4]);
        assert!(dib_to_rgba(&bad_hdr).is_none());
    }

    #[test]
    fn thumbnail_never_exceeds_the_bound_and_keeps_aspect() {
        let rgba = vec![200u8; 1000 * 10 * 4];
        let url = make_thumb(1000, 10, &rgba).unwrap();
        let b64 = url.split(',').nth(1).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!(img.width(), THUMB_SIZE);
        assert!(img.height() >= 1 && img.height() <= 3);
    }

    #[test]
    fn budget_with_only_the_newest_image_over_cap_keeps_it() {
        let mut s = Store { items: Vec::new(), next_id: 1, limit: 5, persist: false, loaded: false, enabled: false };
        s.push_image(NewImage { png: vec![0; 500].into(), thumb: String::new(), width: 1, height: 1, hash: 1 }, 0);
        s.enforce_image_budget(10);
        assert_eq!(s.items.len(), 1);
    }

    #[test]
    fn same_pixels_different_size_are_not_duplicates() {
        let mut s = Store { items: Vec::new(), next_id: 1, limit: 5, persist: false, loaded: false, enabled: false };
        let mk = |w| NewImage { png: vec![0].into(), thumb: String::new(), width: w, height: 1, hash: 9 };
        assert!(s.push_image(mk(2), 0));
        assert!(s.push_image(mk(4), 0));
        assert_eq!(s.items.len(), 2);
    }
}
