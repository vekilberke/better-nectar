//! Hybrid dock (T-018): the dock reserves no strip and floats over maximized windows, and a lone tap of
//! AltGr (or Right Ctrl) hides or shows it. The keyboard hook feeds physical key events into a tap
//! detector that keeps only the trigger and modifier state: no other key is stored, logged or sent.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

pub const KEY_SETTING: &str = "nectar-dock-hybrid-key";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapKey {
    AltGr,
    RightCtrl,
}

impl TapKey {
    pub fn vk(self) -> u32 {
        match self { TapKey::AltGr => VK_RMENU, TapKey::RightCtrl => VK_RCONTROL }
    }

    /// `None` = the shortcut is off. Unknown values fall back to AltGr, the default.
    pub fn from_setting(value: Option<&str>) -> Option<TapKey> {
        match value.unwrap_or("altgr") {
            "off" => None,
            "rctrl" => Some(TapKey::RightCtrl),
            _ => Some(TapKey::AltGr),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Trigger,
    /// Left Ctrl while AltGr is the trigger: either a real key or the one Windows adds to AltGr.
    LeftCtrl,
    Modifier(u8),
    Other,
    /// The Left Ctrl Windows adds to AltGr (scan-code mark).
    AddedCtrl,
}

const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
/// Windows sends AltGr as LCtrl + RAlt and marks the LCtrl it adds with this scan-code bit (0x21D).
const FAKE_CTRL_SCAN: u32 = 0x200;

pub fn classify(vk: u32, scan: u32, tap: TapKey) -> Key {
    match (vk, tap) {
        (VK_RMENU, TapKey::AltGr) | (VK_RCONTROL, TapKey::RightCtrl) => Key::Trigger,
        (VK_LCONTROL, TapKey::AltGr) if scan & FAKE_CTRL_SCAN != 0 => Key::AddedCtrl,
        (VK_LCONTROL, TapKey::AltGr) => Key::LeftCtrl,
        (VK_SHIFT | VK_LSHIFT, _) => Key::Modifier(1),
        (VK_RSHIFT, _) => Key::Modifier(2),
        (VK_CONTROL | VK_LCONTROL, _) => Key::Modifier(4),
        (VK_RCONTROL, _) => Key::Modifier(8),
        (VK_MENU | VK_LMENU, _) => Key::Modifier(16),
        (VK_RMENU, _) => Key::Modifier(32),
        (VK_LWIN, _) => Key::Modifier(64),
        (VK_RWIN, _) => Key::Modifier(128),
        _ => Key::Other,
    }
}

/// Pure state machine: a tap is the trigger pressed and released with no other key and no modifier
/// held in between. Held long enough to auto-repeat, it is not a tap either.
#[derive(Debug, Default)]
pub struct TapDetector {
    held: bool,
    spoiled: bool,
    mods: u8,
    /// Time of a Left Ctrl press not yet matched to AltGr (AltGr's added LCtrl shares its time).
    ctrl_at: Option<u32>,
    /// AltGr's added Left Ctrl is down.
    added_ctrl: bool,
    /// The trigger went down without AltGr's added Left Ctrl: Right Alt on a layout without AltGr.
    plain_alt: bool,
}

/// Virtual key behind each `Key::Modifier` bit, for checking the real key state.
const MODIFIER_VKS: [(u8, u32); 8] = [
    (1, VK_LSHIFT), (2, VK_RSHIFT), (4, VK_LCONTROL), (8, VK_RCONTROL),
    (16, VK_LMENU), (32, VK_RMENU), (64, VK_LWIN), (128, VK_RWIN),
];

impl TapDetector {
    /// Before a fresh trigger press (or AltGr's added LCtrl): drops modifier state whose key-up the hook missed (Win+L,
    /// Ctrl+Alt+Del, an elevated window), using the real key state from `physically_down(vk)`. A hold whose
    /// trigger is no longer down lost its key-up too; a trigger still down is auto-repeat.
    pub fn sync(&mut self, trigger_vk: u32, physically_down: impl Fn(u32) -> bool) {
        if self.held {
            if physically_down(trigger_vk) { return; }
            self.held = false;
            self.spoiled = false;
        }
        for (bit, vk) in MODIFIER_VKS {
            if self.mods & bit != 0 && !physically_down(vk) { self.mods &= !bit; }
        }
        if !physically_down(VK_LCONTROL) { self.ctrl_at = None; }
    }

    /// After a tap: it was a lone Alt for the focused app, which would open its menu.
    pub fn lone_alt(&self) -> bool {
        self.plain_alt
    }

    /// One key-down or key-up of a physical key at hook time `time`. True on a lone tap of the trigger.
    pub fn feed(&mut self, key: Key, down: bool, time: u32) -> bool {
        match key {
            Key::AddedCtrl => {
                self.added_ctrl = down;
                false
            }
            Key::Trigger if !down => {
                let tap = self.held && !self.spoiled;
                self.held = false;
                self.spoiled = false;
                tap
            }
            Key::Trigger => {
                if self.held {
                    self.spoiled = true;
                } else {
                    let matched = self.ctrl_at == Some(time);
                    if matched {
                        self.ctrl_at = None;
                    }
                    self.plain_alt = !matched && !self.added_ctrl;
                    self.held = true;
                    self.spoiled = self.mods != 0 || self.ctrl_at.is_some();
                }
                false
            }
            Key::LeftCtrl => {
                if !down {
                    self.ctrl_at = None;
                } else if self.held {
                    self.spoiled = true;
                } else {
                    self.ctrl_at = Some(time);
                }
                false
            }
            Key::Modifier(bit) => {
                if down { self.mods |= bit; } else { self.mods &= !bit; }
                if self.held { self.spoiled = true; }
                false
            }
            Key::Other => {
                if self.held { self.spoiled = true; }
                false
            }
        }
    }
}

/// 0 = off, 1 = AltGr, 2 = Right Ctrl.
static TRIGGER: AtomicU8 = AtomicU8::new(0);
/// Some dock window is in hybrid mode. The hook does nothing else while this is false.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Hidden with the key. Not saved: every start shows the dock.
static HIDDEN: AtomicBool = AtomicBool::new(false);
static DETECTOR: Mutex<TapDetector> = Mutex::new(TapDetector { held: false, spoiled: false, mods: 0, ctrl_at: None, added_ctrl: false, plain_alt: false });
/// A mouse button or the wheel was used since the last key event (spoils a tap in progress).
static MOUSE_USED: AtomicBool = AtomicBool::new(false);
static APP: OnceLock<AppHandle> = OnceLock::new();

fn trigger() -> Option<TapKey> {
    match TRIGGER.load(Ordering::Relaxed) {
        1 => Some(TapKey::AltGr),
        2 => Some(TapKey::RightCtrl),
        _ => None,
    }
}

fn fullscreen_or_shell() -> bool {
    crate::state::CURRENT_FOREGROUND_FULLSCREEN.load(Ordering::Relaxed)
        || crate::state::CURRENT_FOREGROUND_SHELL_ON_DOCK.load(Ordering::Relaxed)
}

fn key_physically_down(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// Called from the low-level keyboard hook for every physical (not injected) key event.
/// True = swallow this event (the hook has already re-sent it behind the mask key).
pub fn on_key(vk: u32, scan: u32, down: bool, up: bool, time: u32) -> bool {
    if !ACTIVE.load(Ordering::Relaxed) || !(down || up) { return false; }
    let Some(tap) = trigger() else { return false };
    let Ok(mut detector) = DETECTOR.try_lock() else { return false };
    if MOUSE_USED.swap(false, Ordering::Relaxed) {
        detector.feed(Key::Other, true, time);
    }
    let key = classify(vk, scan, tap);
    // Also on AltGr's added LCtrl, before it marks LCtrl as down.
    if down && matches!(key, Key::Trigger | Key::AddedCtrl) {
        detector.sync(tap.vk(), key_physically_down);
    }
    if !detector.feed(key, down, time) { return false; }
    // Fullscreen or a shell surface already hides the dock: a flip would show up only later, and the
    // app in front gets its key unchanged (I-192).
    if fullscreen_or_shell() { return false; }
    let mask = tap == TapKey::AltGr && detector.lone_alt();
    drop(detector);
    // A lone Right Alt (layout without AltGr) would open the focused app's menu: tap the mask key
    // first and send the release after it, so Windows never sees a lone Alt.
    if mask { send_masked_alt_release(); }
    let hidden = !HIDDEN.fetch_xor(true, Ordering::SeqCst);
    emit(hidden);
    mask
}

/// Injected key-downs (remappers like PowerToys or AutoHotkey re-send keys this way) only spoil a tap.
pub fn on_injected_key(vk: u32, scan: u32) {
    if !ACTIVE.load(Ordering::Relaxed) { return; }
    let Some(tap) = trigger() else { return };
    if classify(vk, scan, tap) == Key::Trigger { return; }
    if let Ok(mut detector) = DETECTOR.try_lock() {
        detector.feed(Key::Other, true, 0);
    }
}

/// From the mouse hook: a button or the wheel, so Right Ctrl+click or AltGr+wheel is not a tap.
pub fn on_mouse_button() {
    if ACTIVE.load(Ordering::Relaxed) { MOUSE_USED.store(true, Ordering::Relaxed); }
}

fn send_masked_alt_release() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY};
    let key = |vk: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let mask = crate::services::MASK_VK;
    let inputs = [
        key(mask, KEYBD_EVENT_FLAGS(0)),
        key(mask, KEYEVENTF_KEYUP),
        key(VK_RMENU as u16, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32); }
}

fn emit(hidden: bool) {
    // The dock's click-through is only re-evaluated on mouse moves; a resting cursor must not click the hidden dock.
    crate::services::recheck_pointer();
    let Some(app) = APP.get().cloned() else { return };
    // Don't block input on webview IPC.
    tauri::async_runtime::spawn(async move {
        let _ = app.emit("dock-hybrid-hidden", hidden);
    });
}

fn reset_hidden() {
    if HIDDEN.swap(false, Ordering::SeqCst) { emit(false); }
}

/// The dock in `label` is hybrid and hidden with the key: no edge reveal, no clicks.
pub fn suppressed(label: &str) -> bool {
    HIDDEN.load(Ordering::Relaxed) && crate::state::get_flag(crate::state::dock_hybrid(), label)
}

fn refresh_trigger(app: &AppHandle) {
    let key = TapKey::from_setting(crate::utils::get_setting_str(app, KEY_SETTING).as_deref());
    let code = match key { Some(TapKey::AltGr) => 1, Some(TapKey::RightCtrl) => 2, None => 0 };
    if TRIGGER.swap(code, Ordering::Relaxed) != code {
        // Key events went unseen while it was off, and the old state belongs to another key.
        if let Ok(mut d) = DETECTOR.lock() { *d = TapDetector::default(); }
    }
    if key.is_none() { reset_hidden(); }
}

/// Recomputes whether any dock is hybrid. Leaving hybrid everywhere shows the dock again next time.
pub fn update_active() {
    let any = crate::state::dock_hybrid().lock().map(|m| m.values().any(|v| *v)).unwrap_or(false);
    let was = ACTIVE.swap(any, Ordering::SeqCst);
    if any && !was {
        if let Ok(mut d) = DETECTOR.lock() { *d = TapDetector::default(); }
    }
    if !any { reset_hidden(); }
}

pub fn on_setting_changed(app: &AppHandle) {
    refresh_trigger(app);
}

/// Each dock window says whether it is in hybrid mode (its mode can differ per monitor).
/// Returns whether it should start hidden.
#[tauri::command]
pub fn set_dock_hybrid(app: AppHandle, window: tauri::WebviewWindow, active: bool) -> bool {
    let _ = APP.set(app.clone());
    refresh_trigger(&app);
    crate::state::set_flag(crate::state::dock_hybrid(), window.label(), active);
    update_active();
    active && HIDDEN.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LCTRL_FAKE_SCAN: u32 = 0x21D;
    const LCTRL_SCAN: u32 = 0x1D;
    const RALT_SCAN: u32 = 0x38;

    /// Feeds the detector like `on_key`: keys physically down are tracked so a missed key-up can be modelled.
    struct Kb { d: TapDetector, tap: TapKey, t: u32, toggles: u32, physical: Vec<u32>, lone_alt: bool }

    impl Kb {
        fn altgr() -> Self { Self { d: TapDetector::default(), tap: TapKey::AltGr, t: 1000, toggles: 0, physical: Vec::new(), lone_alt: false } }
        fn rctrl() -> Self { Self { tap: TapKey::RightCtrl, ..Self::altgr() } }
        fn ev(&mut self, vk: u32, scan: u32, down: bool) -> &mut Self {
            let key = classify(vk, scan, self.tap);
            if down && matches!(key, Key::Trigger | Key::AddedCtrl) {
                let physical = self.physical.clone();
                self.d.sync(self.tap.vk(), |v| physical.contains(&v));
            }
            if down { if !self.physical.contains(&vk) { self.physical.push(vk); } } else { self.physical.retain(|v| *v != vk); }
            if self.d.feed(key, down, self.t) { self.toggles += 1; self.lone_alt = self.d.lone_alt(); }
            self
        }
        /// The key goes up where the hook never sees it (secure desktop, elevated window, key setting off).
        fn lose_up(&mut self, vk: u32) -> &mut Self { self.physical.retain(|v| *v != vk); self }
        fn mouse(&mut self) -> &mut Self { if self.d.feed(Key::Other, true, self.t) { self.toggles += 1; } self }
        fn later(&mut self, ms: u32) -> &mut Self { self.t = self.t.wrapping_add(ms); self }
        /// What a Turkish-layout keyboard sends for AltGr: LCtrl (scan 0x21D) + RAlt at the same time.
        fn altgr_down(&mut self) -> &mut Self { self.ev(VK_LCONTROL, LCTRL_FAKE_SCAN, true).ev(VK_RMENU, RALT_SCAN, true) }
        fn altgr_up(&mut self) -> &mut Self { self.ev(VK_LCONTROL, LCTRL_FAKE_SCAN, false).ev(VK_RMENU, RALT_SCAN, false) }
        fn key(&mut self, vk: u32) -> &mut Self { self.ev(vk, 0x10, true).later(30).ev(vk, 0x10, false) }
    }

    #[test]
    fn a_lone_altgr_tap_toggles_and_the_next_tap_toggles_back() {
        let mut k = Kb::altgr();
        k.altgr_down().later(80).altgr_up();
        assert_eq!(k.toggles, 1);
        k.later(500).altgr_down().later(60).altgr_up();
        assert_eq!(k.toggles, 2);
    }

    #[test]
    fn altgr_chords_for_turkish_characters_never_toggle() {
        // AltGr+Q = @, AltGr+E = €, AltGr+7/8/9/0 = { [ ] }
        for vk in [0x51, 0x45, 0x37, 0x38, 0x39, 0x30, 0xDC] {
            let mut k = Kb::altgr();
            k.altgr_down().later(40).key(vk).later(40).altgr_up();
            assert_eq!(k.toggles, 0, "vk {vk:#x}");
        }
    }

    #[test]
    fn several_characters_typed_under_one_altgr_hold_never_toggle() {
        let mut k = Kb::altgr();
        k.altgr_down().later(40).key(0x37).later(40).key(0x30).later(40).altgr_up();
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn the_chord_does_not_spoil_the_next_lone_tap() {
        let mut k = Kb::altgr();
        k.altgr_down().later(40).key(0x51).later(40).altgr_up();
        k.later(300).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn rollover_from_a_previous_key_is_a_chord() {
        // 'a' still down when AltGr goes down, released while AltGr is held.
        let mut k = Kb::altgr();
        k.ev(0x41, 0x1E, true).later(20).altgr_down().later(20).ev(0x41, 0x1E, false).later(40).altgr_up();
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn a_held_altgr_that_auto_repeats_is_not_a_tap() {
        let mut k = Kb::altgr();
        k.altgr_down();
        for _ in 0..5 { k.later(33).altgr_down(); }
        k.later(30).altgr_up();
        assert_eq!(k.toggles, 0);
        k.later(300).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn real_left_ctrl_held_with_right_alt_is_a_chord() {
        let mut k = Kb::altgr();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).later(120).ev(VK_RMENU, RALT_SCAN, true).later(60)
            .ev(VK_RMENU, RALT_SCAN, false).later(30).ev(VK_LCONTROL, LCTRL_SCAN, false);
        assert_eq!(k.toggles, 0);
        // Ctrl released: the next lone tap works again.
        k.later(300).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn left_ctrl_pressed_during_altgr_is_a_chord() {
        let mut k = Kb::altgr();
        k.altgr_down().later(40).ev(VK_LCONTROL, LCTRL_SCAN, true).later(30).ev(VK_LCONTROL, LCTRL_SCAN, false).later(30).altgr_up();
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn left_ctrl_alt_chord_never_toggles() {
        let mut k = Kb::altgr();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).later(50).ev(VK_LMENU, RALT_SCAN, true).later(60)
            .ev(VK_LMENU, RALT_SCAN, false).ev(VK_LCONTROL, LCTRL_SCAN, false);
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn added_left_ctrl_without_the_scan_mark_is_matched_by_its_time() {
        // Some drivers/remote sessions: LCtrl with the plain scan code, same time as RAlt.
        let mut k = Kb::altgr();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).ev(VK_RMENU, RALT_SCAN, true).later(70)
            .ev(VK_LCONTROL, LCTRL_SCAN, false).ev(VK_RMENU, RALT_SCAN, false);
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn a_modifier_held_before_altgr_is_a_chord() {
        for vk in [VK_LSHIFT, VK_RSHIFT, VK_LWIN, VK_RCONTROL, VK_LMENU] {
            let mut k = Kb::altgr();
            k.ev(vk, 0x2A, true).later(100).altgr_down().later(50).altgr_up().later(20).ev(vk, 0x2A, false);
            assert_eq!(k.toggles, 0, "vk {vk:#x}");
        }
    }

    #[test]
    fn a_modifier_released_before_the_tap_does_not_block_it() {
        let mut k = Kb::altgr();
        k.key(VK_LSHIFT).later(200).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn keyup_without_keydown_does_nothing() {
        let mut k = Kb::altgr();
        k.altgr_up();
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn right_ctrl_trigger_taps_and_ignores_altgr() {
        let mut k = Kb::rctrl();
        k.ev(VK_RCONTROL, 0x11D, true).later(60).ev(VK_RCONTROL, 0x11D, false);
        assert_eq!(k.toggles, 1);
        k.later(200).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
        k.later(200).ev(VK_RCONTROL, 0x11D, true).later(20).key(0x43).later(20).ev(VK_RCONTROL, 0x11D, false);
        assert_eq!(k.toggles, 1, "RCtrl+C is a chord");
    }

    #[test]
    fn a_tap_with_the_tick_counter_wrapping_still_counts() {
        let mut k = Kb::altgr();
        k.t = u32::MAX - 10;
        k.altgr_down().later(40).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn fake_ctrl_is_only_ignored_for_the_altgr_trigger() {
        assert_eq!(classify(VK_LCONTROL, LCTRL_FAKE_SCAN, TapKey::AltGr), Key::AddedCtrl);
        assert_eq!(classify(VK_LCONTROL, LCTRL_SCAN, TapKey::AltGr), Key::LeftCtrl);
        assert_eq!(classify(VK_LCONTROL, LCTRL_FAKE_SCAN, TapKey::RightCtrl), Key::Modifier(4));
        assert_eq!(classify(VK_RMENU, RALT_SCAN, TapKey::RightCtrl), Key::Modifier(32));
        assert_eq!(classify(0x41, 0x1E, TapKey::AltGr), Key::Other);
    }

    #[test]
    fn only_the_fixed_dock_reserves_a_strip() {
        assert!(crate::commands::dock_mode_reserves_strip("fixed"));
        for mode in ["hybrid", "smart", "peek"] {
            assert!(!crate::commands::dock_mode_reserves_strip(mode), "{mode}");
        }
    }

    #[test]
    fn a_missed_modifier_release_must_not_kill_the_tap() {
        // Win+L: the Win key-up happens on the secure desktop and never reaches the hook.
        let mut k = Kb::altgr();
        k.ev(VK_LWIN, 0x5B, true).lose_up(VK_LWIN);
        k.later(5000).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn a_missed_left_ctrl_release_must_not_kill_the_tap() {
        // Ctrl+Alt+Del: the LCtrl key-up goes to the secure desktop.
        let mut k = Kb::altgr();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).lose_up(VK_LCONTROL);
        k.later(5000).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1);
    }

    #[test]
    fn a_modifier_still_held_stays_a_chord_after_the_check() {
        let mut k = Kb::altgr();
        k.ev(VK_LSHIFT, 0x2A, true).later(100).altgr_down().later(50).altgr_up();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).later(100).ev(VK_RMENU, RALT_SCAN, true).later(50).ev(VK_RMENU, RALT_SCAN, false);
        assert_eq!(k.toggles, 0);
    }

    #[test]
    fn a_mouse_click_or_wheel_during_the_hold_is_a_chord() {
        let mut k = Kb::rctrl();
        k.ev(VK_RCONTROL, 0x11D, true).later(40).mouse().later(40).ev(VK_RCONTROL, 0x11D, false);
        assert_eq!(k.toggles, 0, "RCtrl+click / RCtrl+wheel");
        let mut k = Kb::altgr();
        k.mouse().later(100).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1, "a click before the tap does not spoil it");
    }

    #[test]
    fn a_tap_on_a_layout_without_altgr_is_a_lone_alt() {
        let mut k = Kb::altgr();
        k.ev(VK_RMENU, RALT_SCAN, true).later(60).ev(VK_RMENU, RALT_SCAN, false);
        assert_eq!(k.toggles, 1);
        assert!(k.lone_alt, "plain Right Alt needs the mask key");
        let mut k = Kb::altgr();
        k.altgr_down().later(60).altgr_up();
        assert_eq!(k.toggles, 1);
        assert!(!k.lone_alt, "AltGr already carries Ctrl");
        let mut k = Kb::altgr();
        k.ev(VK_LCONTROL, LCTRL_SCAN, true).ev(VK_RMENU, RALT_SCAN, true).later(70)
            .ev(VK_LCONTROL, LCTRL_SCAN, false).ev(VK_RMENU, RALT_SCAN, false);
        assert_eq!(k.toggles, 1);
        assert!(!k.lone_alt, "time-matched added LCtrl");
    }

    #[test]
    fn a_missed_trigger_release_must_not_cost_a_tap() {
        // Win+L while AltGr is held: the AltGr key-up goes to the secure desktop (I-191).
        let mut k = Kb::altgr();
        k.altgr_down().later(50).lose_up(VK_RMENU).lose_up(VK_LCONTROL);
        k.later(5000).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 1, "the stale hold is dropped on the next press");
        k.later(500).altgr_down().later(50).altgr_up();
        assert_eq!(k.toggles, 2);
    }

    #[test]
    fn a_missed_right_ctrl_release_must_not_cost_a_tap() {
        let mut k = Kb::rctrl();
        k.ev(VK_RCONTROL, 0x11D, true).later(30).key(0x43).lose_up(VK_RCONTROL);
        k.later(5000).ev(VK_RCONTROL, 0x11D, true).later(50).ev(VK_RCONTROL, 0x11D, false);
        assert_eq!(k.toggles, 1, "the spoiled stale hold is dropped too");
    }

    #[test]
    fn key_setting_defaults_to_altgr() {
        assert_eq!(TapKey::from_setting(None), Some(TapKey::AltGr));
        assert_eq!(TapKey::from_setting(Some("altgr")), Some(TapKey::AltGr));
        assert_eq!(TapKey::from_setting(Some("rctrl")), Some(TapKey::RightCtrl));
        assert_eq!(TapKey::from_setting(Some("off")), None);
        assert_eq!(TapKey::from_setting(Some("bogus")), Some(TapKey::AltGr));
    }
}
