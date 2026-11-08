// PC clipboard to phone (T-016, D-030). While `nectar-phone-clipboard-sync` is on and the phone
// link runs, the clipboard history listener hands every text copy here (it already skips what
// Windows marks private). After a short pause the newest text goes to every phone with the
// `clipboardOut` permission. Clipboard text is never logged, only its length.

use super::engine::Engine;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

pub const SETTING: &str = "nectar-phone-clipboard-sync";
/// Rapid copies collapse into the last one after this pause.
pub const DEBOUNCE_MS: i64 = 300;
/// Text that came from a phone is not sent back for this long.
const ECHO_MS: i64 = 10 * 60 * 1000;
const ECHO_KEEP: usize = 8;
/// The same text copied again this soon is not sent twice (apps that set the clipboard twice).
const REPEAT_MS: i64 = 10_000;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Skip {
    Blank,
    TooBig,
    Echo,
    Repeat,
}

fn digest(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Which copies may leave the PC: text only, within the size cap, not an echo of phone text.
#[derive(Default)]
pub struct Filter {
    from_phone: VecDeque<(u64, i64)>,
    last_sent: Option<(u64, i64)>,
}

impl Filter {
    pub fn from_phone(&mut self, text: &str, now_ms: i64) {
        // One phone text is noted when it arrives and again when it is put on the clipboard.
        let d = digest(text);
        self.from_phone.retain(|(h, _)| *h != d);
        self.from_phone.push_back((d, now_ms));
        while self.from_phone.len() > ECHO_KEEP {
            self.from_phone.pop_front();
        }
    }

    /// Checks a copy that is about to be sent and remembers it when it may go.
    pub fn check(&mut self, text: &str, now_ms: i64) -> Result<(), Skip> {
        if text.trim().is_empty() {
            return Err(Skip::Blank);
        }
        if !Engine::clip_fits(text) {
            return Err(Skip::TooBig);
        }
        let d = digest(text);
        self.from_phone.retain(|(_, at)| (0..ECHO_MS).contains(&(now_ms - at)));
        if self.from_phone.iter().any(|(h, _)| *h == d) {
            return Err(Skip::Echo);
        }
        if self.last_sent.is_some_and(|(h, at)| h == d && (0..REPEAT_MS).contains(&(now_ms - at))) {
            return Err(Skip::Repeat);
        }
        self.last_sent = Some((d, now_ms));
        Ok(())
    }
}

/// Keeps only the newest copy; it is due once `DEBOUNCE_MS` passed without another one.
#[derive(Default)]
pub struct Debounce {
    pending: Option<(String, i64)>,
    gen: u64,
}

impl Debounce {
    /// Returns the generation the caller hands back to `take` after the pause.
    pub fn push(&mut self, text: String, now_ms: i64) -> u64 {
        self.gen += 1;
        self.pending = Some((text, now_ms + DEBOUNCE_MS));
        self.gen
    }

    pub fn take(&mut self, gen: u64, now_ms: i64) -> Option<String> {
        if gen != self.gen || self.pending.as_ref().is_none_or(|(_, due)| now_ms < *due) {
            return None;
        }
        self.pending.take().map(|(t, _)| t)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Default)]
struct State {
    filter: Filter,
    debounce: Debounce,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);
static APP: OnceLock<AppHandle> = OnceLock::new();

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(State::default))
}

fn now_ms() -> i64 {
    super::client::now_ms()
}

pub fn enabled(app: &AppHandle) -> bool {
    crate::utils::get_setting_str(app, SETTING).as_deref() == Some("true")
}

/// Applies the setting and the phone link state: the clipboard listener runs for this only
/// while both are on. With the setting off, copies still waiting in phone mailboxes are
/// deleted. Call it without holding the phone link runtime lock.
pub fn refresh(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let sync = enabled(app);
    let on = super::running() && sync;
    ACTIVE.store(on, Ordering::SeqCst);
    if !on {
        with_state(|s| s.debounce.clear());
    }
    crate::clipboard_history::set_phone_sync(on);
    if !sync {
        super::drop_clips(app);
    }
}

/// A text copy seen by the clipboard listener; `seq` is its clipboard sequence number.
pub fn on_copy(text: String, seq: u32) {
    if !ACTIVE.load(Ordering::SeqCst) || seq == super::clipboard::last_write_seq() {
        return;
    }
    let gen = with_state(|s| s.debounce.push(text, now_ms()));
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS as u64 + 10)).await;
        flush(gen);
    });
}

/// Copies that arrive marked too long by the listener (over its own limit) are only logged.
pub fn on_too_long() {
    if ACTIVE.load(Ordering::SeqCst) {
        crate::diagnostics::log("phone link: copied text over 64 KB not sent to the phone");
    }
}

fn flush(gen: u64) {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let now = now_ms();
    let picked = with_state(|s| {
        let text = s.debounce.take(gen, now)?;
        let len = text.len();
        Some(s.filter.check(&text, now).map(|_| text).map_err(|e| (e, len)))
    });
    match picked {
        Some(Ok(text)) => {
            if let Some(app) = APP.get() {
                super::send_clip(app, &text);
            }
        }
        Some(Err((Skip::TooBig, len))) => crate::diagnostics::log(&format!("phone link: copied text of {len} bytes is too big for one phone message, not sent")),
        _ => {}
    }
}

/// Text that came from a phone (it may land on the clipboard), so copying it is not an echo.
pub fn note_from_phone(text: &str) {
    with_state(|s| s.filter.from_phone(text, now_ms()));
}

#[cfg(test)]
mod tests {
    use super::super::remote::MAX_TEXT;
    use super::*;

    #[test]
    fn blank_and_oversized_text_is_skipped() {
        let mut f = Filter::default();
        assert_eq!(f.check("", 0), Err(Skip::Blank));
        assert_eq!(f.check(" \r\n\t", 0), Err(Skip::Blank));
        assert_eq!(f.check(&"a".repeat(MAX_TEXT + 1), 0), Err(Skip::TooBig));
        assert_eq!(f.check(&"\u{e7}".repeat(MAX_TEXT / 2 + 1), 0), Err(Skip::TooBig), "the cap is in UTF-8 bytes");
        assert_eq!(f.check(&"a".repeat(MAX_TEXT - 100), 0), Ok(()));
    }

    // I-320: text under the byte cap whose sealed message is over `MAX_PLAIN` was dropped later
    // without a log line.
    #[test]
    fn text_whose_sealed_message_is_too_big_is_skipped_here() {
        let mut f = Filter::default();
        assert_eq!(f.check(&"a\n".repeat(MAX_TEXT / 3), 0), Err(Skip::TooBig));
        assert_eq!(f.check(&"a".repeat(MAX_TEXT), 1), Err(Skip::TooBig), "the JSON around it counts too");
    }

    #[test]
    fn text_from_the_phone_is_not_sent_back_until_it_ages_out() {
        let mut f = Filter::default();
        f.from_phone("from phone", 1_000);
        assert_eq!(f.check("from phone", 2_000), Err(Skip::Echo));
        assert_eq!(f.check("from pc", 2_000), Ok(()));
        assert_eq!(f.check("from phone", 1_000 + ECHO_MS), Ok(()), "an old phone text may be copied again");
    }

    #[test]
    fn only_the_last_few_phone_texts_count_as_echoes() {
        let mut f = Filter::default();
        for i in 0..=ECHO_KEEP {
            f.from_phone(&format!("t{i}"), 0);
        }
        assert_eq!(f.check("t0", 1), Ok(()));
        assert_eq!(f.check(&format!("t{ECHO_KEEP}"), 1), Err(Skip::Echo));
    }

    #[test]
    fn the_same_copy_twice_in_a_row_goes_once() {
        let mut f = Filter::default();
        assert_eq!(f.check("x", 0), Ok(()));
        assert_eq!(f.check("x", 500), Err(Skip::Repeat));
        assert_eq!(f.check("y", 600), Ok(()));
        assert_eq!(f.check("x", 700), Ok(()), "x after y is a new copy");
        assert_eq!(f.check("x", 700 + REPEAT_MS), Ok(()));
    }

    #[test]
    fn debounce_sends_only_the_last_copy_after_the_pause() {
        let mut d = Debounce::default();
        let g1 = d.push("a".into(), 0);
        let g2 = d.push("b".into(), 100);
        assert_eq!(d.take(g1, 1_000), None, "superseded");
        assert_eq!(d.take(g2, 100 + DEBOUNCE_MS - 1), None, "too early");
        assert_eq!(d.take(g2, 100 + DEBOUNCE_MS), Some("b".into()));
        assert_eq!(d.take(g2, 2_000), None, "taken once");
        let g3 = d.push("c".into(), 3_000);
        d.clear();
        assert_eq!(d.take(g3, 9_000), None, "turned off");
    }

    #[test]
    fn echo_window_edges_and_echo_beats_repeat() {
        let mut f = Filter::default();
        f.from_phone("p", 0);
        assert_eq!(f.check("p", ECHO_MS - 1), Err(Skip::Echo));
        assert_eq!(f.check("p", ECHO_MS), Ok(()), "aged out exactly at the window");
        // A skipped copy is not remembered as sent, so it does not start a repeat window.
        let mut f = Filter::default();
        f.from_phone("q", 0);
        assert_eq!(f.check("q", 1), Err(Skip::Echo));
        assert_eq!(f.check(&"a".repeat(MAX_TEXT + 1), 2), Err(Skip::TooBig));
        assert_eq!(f.check("a", 3), Ok(()));
        // Surrounding whitespace is part of the copy (only blank text is dropped).
        assert_eq!(f.check("  a  ", 4), Ok(()));
    }

    #[test]
    fn a_too_early_take_keeps_the_copy_for_the_flush_that_is_due() {
        let mut d = Debounce::default();
        let g = d.push("a".into(), 0);
        assert_eq!(d.take(g, DEBOUNCE_MS - 1), None);
        assert_eq!(d.take(g, DEBOUNCE_MS), Some("a".into()));
        // A flush scheduled for an older copy must not take a newer one early.
        let old = d.push("b".into(), 1_000);
        let new = d.push("c".into(), 1_200);
        assert_eq!(d.take(old, 1_000 + DEBOUNCE_MS), None);
        assert_eq!(d.take(new, 1_200 + DEBOUNCE_MS), Some("c".into()));
    }

    // host.rs `text_received` and clipboard.rs `set_text` both note one phone text (I-304).
    #[test]
    fn a_phone_text_noted_twice_uses_one_echo_place() {
        let mut f = Filter::default();
        for i in 0..ECHO_KEEP {
            f.from_phone(&format!("t{i}"), 0);
            f.from_phone(&format!("t{i}"), 0);
        }
        assert_eq!(f.check("t0", 1), Err(Skip::Echo));
    }

    #[test]
    fn noting_a_phone_text_again_restarts_its_echo_window() {
        let mut f = Filter::default();
        f.from_phone("p", 0);
        f.from_phone("p", ECHO_MS - 10);
        assert_eq!(f.check("p", ECHO_MS + 10), Err(Skip::Echo));
    }

    #[test]
    fn a_clock_that_went_back_does_not_panic_or_repeat_forever() {
        let mut f = Filter::default();
        assert_eq!(f.check("x", 10_000), Ok(()));
        assert_eq!(f.check("x", 5_000), Ok(()), "a copy dated before the last one is a new copy");
        f.from_phone("p", 10_000);
        assert_eq!(f.check("p", 5_000), Ok(()));
    }

    #[test]
    fn multibyte_text_at_the_byte_cap_passes_and_one_byte_more_is_skipped() {
        let mut f = Filter::default();
        // The cap is the sealed message: the text plus its JSON frame, in UTF-8 bytes.
        let cap = (0..MAX_TEXT).rev().find(|&n| Engine::clip_fits(&"a".repeat(n))).unwrap();
        let ok = format!("{}{}", "\u{e7}".repeat(cap / 2), "a".repeat(cap % 2));
        assert_eq!(ok.len(), cap);
        assert_eq!(f.check(&ok, 0), Ok(()));
        assert_eq!(f.check(&format!("{ok}a"), 1), Err(Skip::TooBig));
    }

    // Tester round 3 (T-016).
    #[test]
    fn a_b_a_within_the_repeat_window_sends_a_again() {
        let mut f = Filter::default();
        assert_eq!(f.check("a", 0), Ok(()));
        assert_eq!(f.check("b", 1_000), Ok(()));
        assert_eq!(f.check("a", 2_000), Ok(()), "only the copy just sent counts as a repeat");
        assert_eq!(f.check("a", 3_000), Err(Skip::Repeat));
    }

    #[test]
    fn a_cleared_debounce_sends_nothing_even_for_the_current_generation() {
        let mut d = Debounce::default();
        let gen = d.push("x".into(), 0);
        d.clear();
        assert_eq!(d.take(gen, DEBOUNCE_MS + 1), None);
        let gen = d.push("y".into(), 10_000);
        assert_eq!(d.take(gen, 10_000 + DEBOUNCE_MS), Some("y".into()), "due exactly at the pause");
    }
}
