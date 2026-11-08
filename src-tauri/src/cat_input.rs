//! Typing, scrolling and presence signals for the idle notch cat (SondeR cat reactions).
//! The keyboard and mouse hooks only report that input happened, never which key:
//! no key codes or text are read here, stored, logged or sent anywhere.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Poll rate while keys are coming in; also the fastest the paws can alternate.
const TICK_MS: i64 = 120;
/// Key presses needed, without a pause, before the cat starts typing along.
/// Keeps a lone shortcut (Ctrl+C is two key-downs) from triggering it.
const START_KEYS: u32 = 3;
const START_WINDOW_MS: i64 = 1200;
/// The cat keeps its paws on the keys this long after the last press.
const STOP_MS: i64 = 900;
/// The cat bats at the page this long after the last wheel notch.
const SCROLL_STOP_MS: i64 = 1200;
/// No input for this long counts as a break: coming back makes the cat stretch.
const REST_MS: i64 = 45_000;
/// No input for this long sends the cat to sleep until the user is back.
const AWAY_MS: i64 = 180_000;
/// Session input newer than the hook's last input by this much counts as real input (GetTickCount jitter).
const SESSION_SLACK_MS: i64 = 1000;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CatInput {
    Typing { beat: u32 },
    TypingStop,
    Scroll,
    ScrollStop,
    Away,
    Back {
        #[serde(rename = "idleMs")]
        idle_ms: i64,
    },
}

/// One reading of the hook counters. Counts are running totals, times are monotonic ms.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    pub now: i64,
    pub keys: u32,
    pub last_key: i64,
    pub scrolls: u32,
    pub last_scroll: i64,
    pub last_input: i64,
}

/// Pure state machine over input timing.
#[derive(Debug)]
pub struct Detector {
    keys_seen: u32,
    scrolls_seen: u32,
    typing: bool,
    scrolling: bool,
    beat: u32,
    pending_since: Option<i64>,
    pending_keys: u32,
    last_key: i64,
    last_scroll: i64,
    last_input: i64,
    rested: bool,
    away: bool,
}

impl Detector {
    pub fn new(at: Sample) -> Self {
        Self {
            keys_seen: at.keys,
            scrolls_seen: at.scrolls,
            typing: false,
            scrolling: false,
            beat: 0,
            pending_since: None,
            pending_keys: 0,
            last_key: 0,
            last_scroll: 0,
            last_input: at.last_input,
            rested: false,
            away: false,
        }
    }

    pub fn typing(&self) -> bool { self.typing }
    pub fn rested(&self) -> bool { self.rested }
    pub fn scrolling(&self) -> bool { self.scrolling }

    /// Feeds one reading. Returns the events to send and how long to wait before the next
    /// step (`None`: nothing is due, wait for input).
    pub fn step(&mut self, s: Sample) -> (Vec<CatInput>, Option<i64>) {
        let now = s.now;
        let mut out = Vec::new();

        if s.last_input > self.last_input {
            if self.rested {
                out.push(CatInput::Back { idle_ms: s.last_input - self.last_input });
            }
            self.rested = false;
            self.away = false;
            self.last_input = s.last_input;
        }
        let idle = now - self.last_input;
        if idle >= REST_MS { self.rested = true; }
        if self.rested && !self.away && idle >= AWAY_MS {
            self.away = true;
            out.push(CatInput::Away);
        }

        let new_keys = s.keys.wrapping_sub(self.keys_seen);
        self.keys_seen = s.keys;
        if new_keys > 0 {
            let gap = s.last_key - self.last_key;
            let stale = self.pending_since.is_none_or(|since| gap > STOP_MS || (!self.typing && s.last_key - since > START_WINDOW_MS));
            if stale {
                self.pending_since = Some(s.last_key);
                self.pending_keys = new_keys;
            } else {
                self.pending_keys = self.pending_keys.saturating_add(new_keys);
            }
            self.last_key = s.last_key;
            if self.typing || self.pending_keys >= START_KEYS {
                self.typing = true;
                self.beat = self.beat.wrapping_add(1);
                out.push(CatInput::Typing { beat: self.beat });
            }
        }
        if self.pending_since.is_some() && now - self.last_key >= STOP_MS {
            if self.typing { out.push(CatInput::TypingStop); }
            self.typing = false;
            self.pending_since = None;
            self.pending_keys = 0;
        }

        if s.scrolls.wrapping_sub(self.scrolls_seen) > 0 {
            self.scrolls_seen = s.scrolls;
            self.last_scroll = s.last_scroll;
            if !self.scrolling {
                self.scrolling = true;
                out.push(CatInput::Scroll);
            }
        }
        if self.scrolling && now - self.last_scroll >= SCROLL_STOP_MS {
            self.scrolling = false;
            out.push(CatInput::ScrollStop);
        }

        let presence = if !self.rested {
            Some(self.last_input + REST_MS - now)
        } else if !self.away {
            Some(self.last_input + AWAY_MS - now)
        } else {
            None
        };
        let typing = self.pending_since.map(|_| TICK_MS);
        let scrolling = self.scrolling.then(|| self.last_scroll + SCROLL_STOP_MS - now);
        let next = [presence, typing, scrolling].into_iter().flatten().min().map(|ms| ms.max(1));
        (out, next)
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static KEY_COUNT: AtomicU32 = AtomicU32::new(0);
static LAST_KEY_MS: AtomicI64 = AtomicI64::new(0);
static SCROLL_COUNT: AtomicU32 = AtomicU32::new(0);
static LAST_SCROLL_MS: AtomicI64 = AtomicI64::new(0);
static LAST_INPUT_MS: AtomicI64 = AtomicI64::new(0);
/// Set while the user is on a break, so the next mouse move wakes the worker once.
static RESTING: AtomicBool = AtomicBool::new(false);
static SCROLL_ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<std::thread::Thread> = OnceLock::new();
static WATCHERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static EPOCH: OnceLock<Instant> = OnceLock::new();

fn now_ms() -> i64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as i64
}

fn wake_worker() {
    if let Some(worker) = WORKER.get() { worker.unpark(); }
}

/// Called from the low-level keyboard hook for a physical key-down. Only the time is kept.
pub fn note_key() {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    let now = now_ms();
    LAST_KEY_MS.store(now, Ordering::Relaxed);
    LAST_INPUT_MS.store(now, Ordering::Relaxed);
    KEY_COUNT.fetch_add(1, Ordering::Relaxed);
    wake_worker();
}

/// Called from the low-level mouse hook on a wheel notch.
pub fn note_scroll() {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    let now = now_ms();
    LAST_SCROLL_MS.store(now, Ordering::Relaxed);
    LAST_INPUT_MS.store(now, Ordering::Relaxed);
    SCROLL_COUNT.fetch_add(1, Ordering::Relaxed);
    // Only the first notch of a scroll needs the worker, it times the stop itself.
    if !SCROLL_ACTIVE.load(Ordering::Relaxed) { wake_worker(); }
}

/// Called from the low-level mouse hook on movement.
pub fn note_pointer() {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
    if RESTING.swap(false, Ordering::Relaxed) { wake_worker(); }
}

fn sample() -> Sample {
    Sample {
        now: now_ms(),
        keys: KEY_COUNT.load(Ordering::Relaxed),
        last_key: LAST_KEY_MS.load(Ordering::Relaxed),
        scrolls: SCROLL_COUNT.load(Ordering::Relaxed),
        last_scroll: LAST_SCROLL_MS.load(Ordering::Relaxed),
        last_input: LAST_INPUT_MS.load(Ordering::Relaxed),
    }
}

/// When the session's last input was, on the `now_ms` clock, from a GetTickCount reading and its
/// LASTINPUTINFO time. Wraps with the 49-day tick counter.
pub fn session_input_at(now: i64, tick: u32, last: u32) -> i64 {
    now - tick.wrapping_sub(last) as i64
}

/// The low-level hooks get no input while an elevated window has the focus (UIPI), but the
/// session-wide last-input time does. Checked only once the hooks look idle (rest/away deadlines).
fn sync_session_input() {
    let hook_last = LAST_INPUT_MS.load(Ordering::Relaxed);
    let now = now_ms();
    if now - hook_last < REST_MS { return; }
    let (tick, last) = unsafe {
        let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if !GetLastInputInfo(&mut info).as_bool() { return; }
        (GetTickCount(), info.dwTime)
    };
    let at = session_input_at(now, tick, last);
    if at > hook_last + SESSION_SLACK_MS && at <= now {
        LAST_INPUT_MS.fetch_max(at, Ordering::Relaxed);
    }
}

fn run_worker(app: AppHandle) {
    let mut det: Option<Detector> = None;
    loop {
        if !ENABLED.load(Ordering::Relaxed) {
            det = None;
            RESTING.store(false, Ordering::Relaxed);
            SCROLL_ACTIVE.store(false, Ordering::Relaxed);
            std::thread::park();
            continue;
        }
        sync_session_input();
        let d = det.get_or_insert_with(|| Detector::new(sample()));
        let (events, next) = d.step(sample());
        RESTING.store(d.rested(), Ordering::Relaxed);
        SCROLL_ACTIVE.store(d.scrolling(), Ordering::Relaxed);
        if !events.is_empty() {
            let labels = WATCHERS.lock().map(|w| w.clone()).unwrap_or_default();
            let live: Vec<&String> = labels.iter().filter(|l| app.get_webview_window(l).is_some()).collect();
            if live.len() != labels.len() {
                if let Ok(mut w) = WATCHERS.lock() {
                    w.retain(|l| app.get_webview_window(l).is_some());
                    if w.is_empty() { ENABLED.store(false, Ordering::Relaxed); }
                }
            }
            for event in &events {
                for label in &live { let _ = app.emit_to(label.as_str(), "cat-input", event); }
            }
        }
        match next {
            // Every key press unparks the thread; sleeping keeps the typing tick at its rate.
            Some(ms) if d.typing() || ms <= TICK_MS => std::thread::sleep(Duration::from_millis(ms as u64)),
            Some(ms) => std::thread::park_timeout(Duration::from_millis(ms as u64)),
            None => std::thread::park(),
        }
    }
}

/// The idle cat asks for input signals only while it is on screen; each notch window registers itself.
#[tauri::command]
pub fn set_cat_input_watch(app: AppHandle, window: tauri::WebviewWindow, enabled: bool) {
    let label = window.label().to_string();
    let on = {
        let Ok(mut w) = WATCHERS.lock() else { return };
        w.retain(|l| *l != label);
        if enabled { w.push(label); }
        !w.is_empty()
    };
    if on && !ENABLED.load(Ordering::Relaxed) {
        LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
    }
    ENABLED.store(on, Ordering::Relaxed);
    if WORKER.get().is_none() {
        if !on { return; }
        let handle = app.clone();
        if let Ok(t) = std::thread::Builder::new().name("cat-input".into()).spawn(move || run_worker(handle)) {
            let _ = WORKER.set(t.thread().clone());
        }
    }
    wake_worker();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Feed { d: Detector, s: Sample }

    impl Feed {
        fn new() -> Self { Self { d: Detector::new(Sample::default()), s: Sample::default() } }
        fn at(&mut self, now: i64) -> (Vec<CatInput>, Option<i64>) {
            self.s.now = now;
            self.d.step(self.s)
        }
        fn keys(&mut self, now: i64, n: u32) -> Vec<CatInput> {
            self.s.keys = self.s.keys.wrapping_add(n);
            self.s.last_key = now;
            self.s.last_input = now;
            self.at(now).0
        }
        fn wheel(&mut self, now: i64) -> Vec<CatInput> {
            self.s.scrolls += 1;
            self.s.last_scroll = now;
            self.s.last_input = now;
            self.at(now).0
        }
        fn mouse(&mut self, now: i64) -> Vec<CatInput> {
            self.s.last_input = now;
            self.at(now).0
        }
    }

    #[test]
    fn a_single_key_or_shortcut_does_not_start_typing() {
        let mut f = Feed::new();
        assert!(f.keys(100, 1).is_empty());
        assert!(f.keys(110, 1).is_empty());
        assert!(f.at(1200).0.is_empty());
        assert!(!f.d.typing());
    }

    #[test]
    fn a_burst_starts_typing_and_each_tick_with_keys_is_one_beat() {
        let mut f = Feed::new();
        f.keys(100, 1);
        f.keys(250, 1);
        assert_eq!(f.keys(400, 1), vec![CatInput::Typing { beat: 1 }]);
        assert!(f.d.typing());
        // Several keys between two ticks make one beat (throttled); a tick without keys makes none.
        assert_eq!(f.keys(520, 3), vec![CatInput::Typing { beat: 2 }]);
        assert!(f.at(640).0.is_empty());
    }

    #[test]
    fn typing_stops_shortly_after_the_last_key() {
        let mut f = Feed::new();
        for t in [0, 100, 200] { f.keys(t, 1); }
        assert!(f.d.typing());
        assert!(f.at(200 + STOP_MS - 1).0.is_empty());
        assert_eq!(f.at(200 + STOP_MS).0, vec![CatInput::TypingStop]);
        assert!(!f.d.typing());
        assert!(f.at(200 + STOP_MS + TICK_MS).0.is_empty());
    }

    #[test]
    fn slow_isolated_presses_never_start_typing() {
        let mut f = Feed::new();
        for i in 0..10 {
            assert!(f.keys(i * (STOP_MS + 100), 1).is_empty());
        }
        assert!(!f.d.typing());
    }

    #[test]
    fn ticks_only_while_keys_are_pending() {
        let mut f = Feed::new();
        f.keys(10, 1);
        assert_eq!(f.at(10).1, Some(TICK_MS));
        assert_eq!(f.at(10 + STOP_MS).1, Some(10 + REST_MS - (10 + STOP_MS)));
    }

    #[test]
    fn scrolling_is_one_start_and_one_stop() {
        let mut f = Feed::new();
        assert_eq!(f.wheel(0), vec![CatInput::Scroll]);
        assert!(f.wheel(300).is_empty());
        let (ev, next) = f.at(400);
        assert!(ev.is_empty());
        assert_eq!(next, Some(300 + SCROLL_STOP_MS - 400));
        assert_eq!(f.at(300 + SCROLL_STOP_MS).0, vec![CatInput::ScrollStop]);
    }

    #[test]
    fn a_break_then_input_reports_back_and_a_long_absence_reports_away() {
        let mut f = Feed::new();
        let (ev, next) = f.at(REST_MS);
        assert!(ev.is_empty());
        assert!(f.d.rested());
        assert_eq!(next, Some(AWAY_MS - REST_MS));
        assert_eq!(f.at(AWAY_MS), (vec![CatInput::Away], None));
        assert!(f.at(AWAY_MS + 5000).0.is_empty());
        let back = AWAY_MS + 9000;
        assert_eq!(f.mouse(back), vec![CatInput::Back { idle_ms: back }]);
        assert!(!f.d.rested());
    }

    #[test]
    fn short_pauses_are_not_a_break() {
        let mut f = Feed::new();
        f.at(REST_MS - 1);
        assert!(f.mouse(REST_MS).is_empty());
        assert!(!f.d.rested());
    }

    #[test]
    fn session_input_time_maps_onto_the_monotonic_clock_across_tick_wraparound() {
        assert_eq!(session_input_at(100_000, 50_000, 40_000), 90_000);
        assert_eq!(session_input_at(100_000, 5, u32::MAX - 994), 99_000);
    }

    #[test]
    fn input_seen_only_by_the_session_clock_keeps_the_user_present() {
        // Typing in an elevated window: the hooks are blind, the worker pushes last_input forward
        // at the rest deadline, so no break is counted and no stretch follows.
        let mut f = Feed::new();
        let seen = REST_MS - 3000;
        f.s.last_input = seen;
        let (ev, next) = f.at(REST_MS);
        assert!(ev.is_empty());
        assert!(!f.d.rested());
        assert_eq!(next, Some(seen)); // next rest deadline: seen + REST_MS, from now = REST_MS
    }

    #[test]
    fn events_carry_no_key_data() {
        assert_eq!(serde_json::to_string(&CatInput::Typing { beat: 4 }).unwrap(), r#"{"kind":"typing","beat":4}"#);
        assert_eq!(serde_json::to_string(&CatInput::TypingStop).unwrap(), r#"{"kind":"typing-stop"}"#);
        assert_eq!(serde_json::to_string(&CatInput::Back { idle_ms: 5 }).unwrap(), r#"{"kind":"back","idleMs":5}"#);
    }

    #[test]
    fn steady_typing_never_stops_and_restarts_cleanly_after_a_pause() {
        let mut f = Feed::new();
        for t in [0, 100, 200] { f.keys(t, 1); }
        for i in 1..40 {
            let ev = f.keys(200 + i * TICK_MS, 1);
            assert!(ev.iter().all(|e| matches!(e, CatInput::Typing { .. })), "tick {i}: {ev:?}");
            assert!(f.d.typing());
        }
        let last = 200 + 39 * TICK_MS;
        assert_eq!(f.at(last + STOP_MS).0, vec![CatInput::TypingStop]);
        // A lone key after the pause must not resume typing; a new burst must.
        assert!(f.keys(last + 5000, 1).is_empty());
        assert!(f.keys(last + 5100, 1).is_empty());
        assert!(matches!(f.keys(last + 5200, 1).as_slice(), [CatInput::Typing { .. }]));
    }

    #[test]
    fn typing_after_a_break_reports_back_first_and_survives_counter_wraparound() {
        let mut f = Feed::new();
        f.at(AWAY_MS);
        f.s.keys = u32::MAX - 1;
        f.d.keys_seen = u32::MAX - 1;
        let t = AWAY_MS + 1000;
        f.keys(t, 1);
        f.keys(t + 100, 1);
        let ev = f.keys(t + 200, 1);
        assert_eq!(f.s.keys, 1, "counter wrapped");
        assert!(matches!(ev.as_slice(), [CatInput::Typing { .. }]), "{ev:?}");
        assert!(!f.d.rested());
    }

    #[test]
    fn scrolling_and_typing_report_independently() {
        let mut f = Feed::new();
        assert_eq!(f.wheel(0), vec![CatInput::Scroll]);
        for t in [100, 200] { f.keys(t, 1); }
        assert!(matches!(f.keys(300, 1).as_slice(), [CatInput::Typing { .. }]));
        assert!(f.d.typing() && f.d.scrolling());
        assert_eq!(f.at(SCROLL_STOP_MS).0, vec![CatInput::TypingStop, CatInput::ScrollStop]);
    }

    #[test]
    fn a_stale_older_input_time_never_rewinds_presence() {
        let mut f = Feed::new();
        f.mouse(30_000);
        f.s.last_input = 10_000;
        let (ev, next) = f.at(40_000);
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(next, Some(30_000 + REST_MS - 40_000));
    }
}
