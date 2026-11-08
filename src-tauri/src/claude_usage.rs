//! Claude plan usage (T-021). Claude Code hands its status line command the 5-hour and weekly
//! limits on stdin (`rate_limits`); `nectar statusline` forwards them to the agent API's `/usage`.
//! The Claude desktop app, whose sessions run no status line, keeps its own samples (percentages
//! only) in `%APPDATA%\Claude\plan-usage-history.json`. Nothing here reads credentials or calls a
//! remote service.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SETTING: &str = "nectar-claude-usage";
const CHAIN_WAIT: Duration = Duration::from_secs(5);
/// Fields of the user's own `statusLine` that stay when ours takes its place.
const KEPT_FIELDS: [&str; 3] = ["padding", "refreshInterval", "hideVimModeIndicator"];
const DESKTOP_MAX_BYTES: u64 = 4 * 1024 * 1024;
const DESKTOP_MAX_AHEAD_MS: u64 = 5 * 60_000;
/// Reset times this close belong to one window (windows are hours apart, posts come from separate requests).
const SAME_WINDOW_SECS: u64 = 600;

/// The last status line usage and when it came (epoch ms).
static LAST: Mutex<Option<(Usage, u64)>> = Mutex::new(None);
/// The desktop app's file is parsed again only when its size or time changes.
static DESKTOP: Mutex<Option<(std::time::SystemTime, u64, Option<DesktopSample>)>> = Mutex::new(None);

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Window {
    pub used_percentage: f64,
    /// Unix epoch seconds.
    pub resets_at: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<Window>,
}

impl Usage {
    fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none()
    }
}

fn window_of(raw: &Value) -> Option<Window> {
    let used = raw.get("used_percentage")?.as_f64().filter(|p| p.is_finite())?;
    let resets = raw.get("resets_at")?.as_f64().filter(|t| t.is_finite() && *t > 0.0)?;
    Some(Window { used_percentage: used.clamp(0.0, 100.0), resets_at: resets.round() as u64 })
}

fn usage_from(limits: &Value) -> Option<Usage> {
    let usage = Usage { five_hour: window_of(&limits["five_hour"]), seven_day: window_of(&limits["seven_day"]) };
    (!usage.is_empty()).then_some(usage)
}

/// The limits in a status line input (`rate_limits`); `None` for API key or 3P sessions, or before the first response.
pub fn usage_of(input: &str) -> Option<Usage> {
    let event: Value = serde_json::from_str(input).ok()?;
    usage_from(&event["rate_limits"])
}

/// Body of a `/usage` request: same shape as `rate_limits`, checked again since anything local can post it.
pub fn usage_of_body(body: &[u8]) -> Option<Usage> {
    usage_from(&serde_json::from_slice::<Value>(body).ok()?)
}

/// The newest sample of the desktop app's history: `fh` / `sd` are the 5-hour / weekly percentages, `t` epoch ms.
#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
pub struct DesktopSample {
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    pub at: u64,
}

/// A sample dated past `now_ms` (plus `DESKTOP_MAX_AHEAD_MS`) was taken while the clock was wrong and
/// would hide every newer one, so it is skipped.
pub fn desktop_sample_of(text: &str, now_ms: u64) -> Option<DesktopSample> {
    let history: Value = serde_json::from_str(text).ok()?;
    let (at, latest) = history["samples"]
        .as_array()?
        .iter()
        .filter_map(|s| Some((s["t"].as_u64()?, s)))
        .filter(|(t, _)| *t <= now_ms.saturating_add(DESKTOP_MAX_AHEAD_MS))
        .max_by_key(|(t, _)| *t)?;
    let pct = |key: &str| latest["u"][key].as_f64().filter(|p| p.is_finite()).map(|p| p.clamp(0.0, 100.0));
    let sample = DesktopSample { five_hour: pct("fh"), seven_day: pct("sd"), at };
    (sample.five_hour.is_some() || sample.seven_day.is_some()).then_some(sample)
}

fn desktop_sample() -> Option<DesktopSample> {
    let path = PathBuf::from(std::env::var_os("APPDATA")?).join("Claude").join("plan-usage-history.json");
    let meta = std::fs::metadata(&path).ok().filter(|m| m.len() <= DESKTOP_MAX_BYTES)?;
    let (modified, len) = (meta.modified().ok()?, meta.len());
    let mut cache = DESKTOP.lock().ok()?;
    if let Some((m, l, sample)) = cache.as_ref() {
        if *m == modified && *l == len {
            return *sample;
        }
    }
    let sample = std::fs::read_to_string(&path).ok().and_then(|t| desktop_sample_of(&t, now_secs() * 1000));
    *cache = Some((modified, len, sample));
    sample
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct UsageState {
    status_line: Option<Usage>,
    status_line_at: Option<u64>,
    desktop: Option<DesktopSample>,
}

fn state() -> UsageState {
    let last = LAST.lock().ok().and_then(|l| l.clone());
    UsageState { status_line: last.as_ref().map(|(u, _)| u.clone()), status_line_at: last.map(|(_, at)| at), desktop: desktop_sample() }
}

fn merge_window(old: Option<Window>, new: Option<Window>) -> (Option<Window>, bool) {
    match (old, new) {
        (Some(o), Some(n)) if o.resets_at.abs_diff(n.resets_at) <= SAME_WINDOW_SECS => {
            (Some(Window { used_percentage: o.used_percentage.max(n.used_percentage), resets_at: o.resets_at.max(n.resets_at) }), true)
        }
        (Some(o), Some(n)) if n.resets_at < o.resets_at => (Some(o), false),
        (_, Some(n)) => (Some(n), true),
        (o, None) => (o, false),
    }
}

/// Usage only grows within a window and a later reset is a newer window, so an idle session that
/// re-runs its status line with old numbers changes nothing. The flag: the post spoke for a current window.
pub fn merge(old: Option<&Usage>, new: Usage) -> (Usage, bool) {
    let (five_hour, a) = merge_window(old.and_then(|u| u.five_hour), new.five_hour);
    let (seven_day, b) = merge_window(old.and_then(|u| u.seven_day), new.seven_day);
    (Usage { five_hour, seven_day }, a || b)
}

pub fn receive(app: &AppHandle, usage: Usage) {
    let Ok(mut last) = LAST.lock() else { return };
    let (merged, current) = merge(last.as_ref().map(|(u, _)| u), usage);
    let changed = last.as_ref().map(|(u, _)| u) != Some(&merged);
    let at = match last.as_ref() {
        Some((_, at)) if !current => *at,
        _ => now_secs() * 1000,
    };
    *last = Some((merged, at));
    drop(last);
    if changed {
        let _ = app.emit("claude-usage", state());
    }
}

#[tauri::command]
pub fn claude_usage() -> UsageState {
    state()
}

/// Time to a reset for the terminal line: `45m`, `2h13m`, `3d4h`.
fn left(secs: u64) -> String {
    let mins = secs.div_ceil(60);
    match mins {
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h{:02}m", m / 60, m % 60),
        m => format!("{}d{}h", m / (24 * 60), m % (24 * 60) / 60),
    }
}

/// Whole numbers rounded half up, never 100 before the limit is really hit (as `usagePercent` in the notch).
fn shown_percent(percent: f64) -> u32 {
    if percent >= 100.0 { 100 } else { (percent.round() as u32).min(99) }
}

/// The short line Claude Code shows when the user has no status line of their own.
pub fn line_of(usage: Option<&Usage>, model: Option<&str>, now: u64) -> String {
    let live = |w: Option<Window>| w.filter(|w| w.resets_at > now);
    let mut parts = Vec::new();
    if let Some(u) = usage {
        if let Some(w) = live(u.five_hour) {
            parts.push(format!("5h {}% ({})", shown_percent(w.used_percentage), left(w.resets_at - now)));
        }
        if let Some(w) = live(u.seven_day) {
            parts.push(format!("7d {}%", shown_percent(w.used_percentage)));
        }
    }
    if parts.is_empty() {
        return model.unwrap_or_default().to_string();
    }
    parts.join(" · ")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- statusLine in ~/.claude/settings.json ----

fn chain_path() -> Option<PathBuf> {
    crate::agent_api::data_dir().map(|d| d.join("claude-statusline.json"))
}

/// Our `statusLine.command`. Claude Code runs it through Git Bash on Windows, which eats backslashes.
/// A plain path stays bare (it also works when Claude Code falls back to PowerShell); anything a shell
/// could read (space, `&`, `(`, `$`, `'`, ...) gets single quotes, where Git Bash expands nothing.
pub fn line_command(exe: &str) -> String {
    let path = exe.replace('\\', "/");
    if path.chars().all(|c| c.is_alphanumeric() || "_./:-".contains(c)) {
        format!("{path} statusline")
    } else {
        format!("'{}' statusline", path.replace('\'', "'\\''"))
    }
}

fn is_our_command(command: &str) -> bool {
    let c = command.trim().to_ascii_lowercase();
    ["nectar.exe statusline", "nectar.exe' statusline", "nectar.exe\" statusline"].iter().any(|end| c.ends_with(end))
}

fn is_ours(line: &Value) -> bool {
    line["command"].as_str().is_some_and(is_our_command)
}

/// What the chain file (the user's own status line, run after ours) holds after a sync.
#[derive(Debug, PartialEq)]
pub enum Chain {
    Keep,
    Save(Value),
    Clear,
}

/// Puts our status line into `settings` (`ours` = its command) or takes it out (`None`), never
/// dropping the user's own: it moves to the chain file and comes back when ours is removed.
pub fn sync_status_line(settings: &mut Map<String, Value>, ours: Option<&str>, saved: Option<Value>) -> Chain {
    let current = settings.get("statusLine").cloned();
    let Some(command) = ours else {
        if current.as_ref().is_some_and(is_ours) {
            match saved {
                Some(line) => settings.insert("statusLine".into(), line),
                None => settings.remove("statusLine"),
            };
        }
        return Chain::Clear;
    };
    match current {
        Some(line) if is_ours(&line) => {
            settings["statusLine"]["command"] = command.into();
            Chain::Keep
        }
        Some(line) if line["command"].is_string() => {
            let mut mine = serde_json::json!({ "type": "command", "command": command });
            for field in KEPT_FIELDS {
                if let Some(value) = line.get(field) {
                    mine[field] = value.clone();
                }
            }
            settings.insert("statusLine".into(), mine);
            Chain::Save(line)
        }
        Some(_) => Chain::Keep,
        None => {
            settings.insert("statusLine".into(), serde_json::json!({ "type": "command", "command": command }));
            Chain::Clear
        }
    }
}

pub fn saved_line() -> Option<Value> {
    let text = std::fs::read_to_string(chain_path()?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("statusLine").filter(|l| l["command"].is_string()).cloned()
}

/// Writes the chain file before settings.json changes, so the user's line is never only in memory.
pub fn save_line(line: &Value) -> bool {
    let Some(path) = chain_path() else { return false };
    std::fs::write(path, serde_json::json!({ "statusLine": line }).to_string()).is_ok()
}

pub fn clear_line() {
    if let Some(path) = chain_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// `nectar-claude-usage` straight from settings.json, so the CLI (`agent install-hooks`) sees it too. On unless "false".
pub fn enabled_on_disk() -> bool {
    let Some(path) = crate::agent_api::data_dir().map(|d| d.join("settings.json")) else { return true };
    let Some(settings) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return true };
    !matches!(&settings[SETTING], Value::Bool(false)) && settings[SETTING].as_str() != Some("false")
}

// ---- `nectar statusline` ----

/// Git Bash as Claude Code finds it: `CLAUDE_CODE_GIT_BASH_PATH`, next to git on PATH, then the default install folders.
fn find_bash() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(path);
    }
    let from_git = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).find_map(|dir| {
        if !dir.join("git.exe").is_file() {
            return None;
        }
        dir.ancestors().take(3).map(|d| d.join("bin").join("bash.exe")).find(|b| b.is_file())
    });
    from_git.or_else(|| {
        ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files (x86)\\Git\\bin\\bash.exe"].iter().map(PathBuf::from).find(|p| p.is_file())
    })
}

/// Runs the user's own status line command with the same input and returns what it printed.
fn run_chained(command: &str, input: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = match find_bash() {
        Some(bash) => {
            let mut c = Command::new(bash);
            c.arg("-c").arg(command);
            c
        }
        None => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            c
        }
    };
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW).spawn().ok()?;
    // Git Bash execs commands as new processes whose parent is already gone, so only a job reaches them all.
    let job = Job::around(&child);
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(CHAIN_WAIT).ok();
    if out.is_none() {
        // The shell's children hold Claude Code's pipe too, so a slow line would block it past our own exit.
        if let Some(job) = &job {
            job.terminate();
        }
        let _ = child.kill();
    }
    out.map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// A job object holding the chained command and everything it starts.
struct Job(*mut std::ffi::c_void);

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> *mut std::ffi::c_void;
    fn AssignProcessToJobObject(job: *mut std::ffi::c_void, process: *mut std::ffi::c_void) -> i32;
    fn TerminateJobObject(job: *mut std::ffi::c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

impl Job {
    fn around(child: &std::process::Child) -> Option<Job> {
        use std::os::windows::io::AsRawHandle;
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return None;
        }
        let job = Job(job);
        (unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } != 0).then_some(job)
    }

    fn terminate(&self) {
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn run_statusline() -> i32 {
    let input = crate::agent_api::hook_input();
    let usage = usage_of(&input);
    if let Some(usage) = &usage {
        if let Ok(body) = serde_json::to_string(usage) {
            let _ = crate::agent_api::post("/usage", body, 1);
        }
    }
    let chained = saved_line().and_then(|l| l["command"].as_str().map(str::to_string)).filter(|c| !is_our_command(c));
    let out = match chained {
        Some(command) => run_chained(&command, &input).unwrap_or_default(),
        None => {
            let model = serde_json::from_str::<Value>(&input).ok().and_then(|e| e["model"]["display_name"].as_str().map(str::to_string));
            line_of(usage.as_ref(), model.as_deref(), now_secs())
        }
    };
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str = r#"{"session_id":"s","model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":42.4,"resets_at":1762600000},"seven_day":{"used_percentage":18,"resets_at":1763000000.4}}}"#;

    #[test]
    fn reads_both_windows_from_status_line_input() {
        let usage = usage_of(INPUT).unwrap();
        assert_eq!(usage.five_hour, Some(Window { used_percentage: 42.4, resets_at: 1762600000 }));
        assert_eq!(usage.seven_day, Some(Window { used_percentage: 18.0, resets_at: 1763000000 }));
    }

    #[test]
    fn missing_or_broken_limits_give_nothing() {
        assert_eq!(usage_of(r#"{"session_id":"s"}"#), None);
        assert_eq!(usage_of(r#"{"rate_limits":null}"#), None);
        assert_eq!(usage_of(r#"{"rate_limits":{"five_hour":{"used_percentage":"x","resets_at":1}}}"#), None);
        assert_eq!(usage_of(r#"{"rate_limits":{"five_hour":{"used_percentage":5,"resets_at":0}}}"#), None);
        assert_eq!(usage_of("not json"), None);
        let one = usage_of(r#"{"rate_limits":{"seven_day":{"used_percentage":250,"resets_at":9}}}"#).unwrap();
        assert_eq!((one.five_hour, one.seven_day.map(|w| w.used_percentage)), (None, Some(100.0)));
    }

    #[test]
    fn desktop_history_gives_its_newest_sample() {
        let text = r#"{"version":2,"samples":[{"t":300,"org":"a","u":{"fh":11,"sd":38}},{"t":100,"org":"a","u":{"fh":80,"sd":37}},{"t":200,"u":{"fh":4}}]}"#;
        assert_eq!(desktop_sample_of(text, 1_000), Some(DesktopSample { five_hour: Some(11.0), seven_day: Some(38.0), at: 300 }));
        assert_eq!(desktop_sample_of(r#"{"samples":[{"t":1,"u":{"fh":140}}]}"#, 1_000), Some(DesktopSample { five_hour: Some(100.0), seven_day: None, at: 1 }));
        assert_eq!(desktop_sample_of(r#"{"samples":[{"t":1,"u":{}}]}"#, 1_000), None);
        assert_eq!(desktop_sample_of(r#"{"samples":[]}"#, 1_000), None);
        assert_eq!(desktop_sample_of(r#"{"samples":[{"u":{"fh":3}}]}"#, 1_000), None);
        assert_eq!(desktop_sample_of("[]", 1_000), None);
    }

    #[test]
    fn desktop_sample_from_a_clock_that_ran_ahead_is_skipped() {
        let now = 10_000_000;
        let ahead = now + DESKTOP_MAX_AHEAD_MS;
        let text = format!(r#"{{"samples":[{{"t":{},"u":{{"fh":100}}}},{{"t":{now},"u":{{"fh":12}}}}]}}"#, ahead + 1);
        assert_eq!(desktop_sample_of(&text, now), Some(DesktopSample { five_hour: Some(12.0), seven_day: None, at: now }));
        let text = format!(r#"{{"samples":[{{"t":{ahead},"u":{{"fh":100}}}},{{"t":{now},"u":{{"fh":12}}}}]}}"#);
        assert_eq!(desktop_sample_of(&text, now).map(|s| s.at), Some(ahead));
        assert_eq!(desktop_sample_of(&format!(r#"{{"samples":[{{"t":{},"u":{{"fh":100}}}}]}}"#, ahead + 1), now), None);
    }

    #[test]
    fn usage_body_round_trips() {
        let usage = usage_of(INPUT).unwrap();
        let body = serde_json::to_vec(&usage).unwrap();
        assert_eq!(usage_of_body(&body), Some(usage));
        assert_eq!(usage_of_body(b"{}"), None);
    }

    #[test]
    fn terminal_line_shows_live_windows_or_the_model() {
        let usage = usage_of(INPUT).unwrap();
        assert_eq!(line_of(Some(&usage), Some("Opus"), 1762600000 - 2 * 3600 - 13 * 60), "5h 42% (2h13m) · 7d 18%");
        assert_eq!(line_of(Some(&usage), None, 1762600000), "7d 18%");
        assert_eq!(line_of(None, Some("Opus"), 0), "Opus");
        assert_eq!(line_of(None, None, 0), "");
        assert_eq!(left(59), "1m");
        assert_eq!(left(3 * 86400 + 4 * 3600), "3d4h");
    }

    #[test]
    fn our_command_uses_forward_slashes_and_quotes_spaces() {
        assert_eq!(line_command("C:\\Users\\a.b\\AppData\\Local\\Programs\\Nectar\\nectar.exe"), "C:/Users/a.b/AppData/Local/Programs/Nectar/nectar.exe statusline");
        assert_eq!(line_command("C:\\Program Files\\Nectar\\nectar.exe"), "'C:/Program Files/Nectar/nectar.exe' statusline");
        assert!(is_our_command(&line_command("C:\\Program Files\\Nectar\\nectar.exe")));
        assert!(!is_our_command("powershell -NoProfile -File ~/.claude/statusline-command.ps1"));
    }

    fn settings(line: Option<Value>) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("model".into(), "opus".into());
        if let Some(line) = line {
            map.insert("statusLine".into(), line);
        }
        map
    }

    #[test]
    fn user_line_is_chained_kept_and_restored() {
        let ours = line_command("C:\\N\\nectar.exe");
        let user = serde_json::json!({ "type": "command", "command": "powershell -File ~/.claude/s.ps1", "padding": 1, "refreshInterval": 5 });
        let mut map = settings(Some(user.clone()));

        assert_eq!(sync_status_line(&mut map, Some(&ours), None), Chain::Save(user.clone()));
        assert_eq!(map["statusLine"], serde_json::json!({ "type": "command", "command": ours, "padding": 1, "refreshInterval": 5 }));

        let moved = line_command("C:\\M\\nectar.exe");
        assert_eq!(sync_status_line(&mut map, Some(&moved), Some(user.clone())), Chain::Keep);
        assert_eq!(map["statusLine"]["command"], moved);

        assert_eq!(sync_status_line(&mut map, None, Some(user.clone())), Chain::Clear);
        assert_eq!(map["statusLine"], user);
        assert_eq!(map["model"], "opus");
    }

    #[test]
    fn no_user_line_means_ours_alone_and_removal_leaves_none() {
        let ours = line_command("C:\\N\\nectar.exe");
        let mut map = settings(None);
        assert_eq!(sync_status_line(&mut map, Some(&ours), None), Chain::Clear);
        assert_eq!(map["statusLine"]["command"], ours);
        assert_eq!(sync_status_line(&mut map, None, None), Chain::Clear);
        assert!(!map.contains_key("statusLine"));
    }

    #[test]
    fn removal_never_touches_a_line_that_is_not_ours() {
        let user = serde_json::json!({ "type": "command", "command": "bash ~/line.sh" });
        let mut map = settings(Some(user.clone()));
        assert_eq!(sync_status_line(&mut map, None, Some(serde_json::json!({ "command": "old" }))), Chain::Clear);
        assert_eq!(map["statusLine"], user);
        let mut odd = settings(Some(Value::String("weird".into())));
        assert_eq!(sync_status_line(&mut odd, Some("x nectar.exe statusline"), None), Chain::Keep);
        assert_eq!(odd["statusLine"], "weird");
    }
    #[test]
    fn reset_time_unit_boundaries() {
        assert_eq!(left(60 * 60), "1h00m");
        assert_eq!(left(60 * 60 + 1), "1h01m");
        assert_eq!(left(24 * 3600 - 60), "23h59m");
        assert_eq!(left(24 * 3600 - 1), "1d0h");
        assert_eq!(left(24 * 3600), "1d0h");
        assert_eq!(left(0), "0m");
    }

    #[test]
    fn terminal_line_skips_a_window_that_has_reset() {
        let usage = usage_of(INPUT).unwrap();
        // five-hour reset passed, weekly still live
        assert_eq!(line_of(Some(&usage), Some("Opus"), 1762600001), "7d 18%");
        // both passed: falls back to the model
        assert_eq!(line_of(Some(&usage), Some("Opus"), 1763000001), "Opus");
    }

    #[test]
    fn body_with_extra_or_wrong_typed_fields_is_handled() {
        assert!(usage_of_body(br#"{"five_hour":{"used_percentage":5,"resets_at":10,"extra":1},"spend_limit":{}}"#).is_some());
        assert_eq!(usage_of_body(br#"{"five_hour":{"used_percentage":-5,"resets_at":10}}"#).unwrap().five_hour.unwrap().used_percentage, 0.0);
        assert_eq!(usage_of_body(br#"{"five_hour":{"used_percentage":null,"resets_at":10}}"#), None);
        assert_eq!(usage_of_body(b"[]"), None);
        assert_eq!(usage_of_body(b""), None);
    }

    #[test]
    fn our_command_detection_is_case_and_space_tolerant() {
        assert!(is_our_command("  C:/X/Nectar.EXE statusline  "));
        assert!(!is_our_command("C:/X/nectar.exe statusline --other"));
        assert!(!is_our_command("echo nectar.exe statusline-ish"));
    }

    fn usage(five: Option<(f64, u64)>, seven: Option<(f64, u64)>) -> Usage {
        let w = |p: Option<(f64, u64)>| p.map(|(used_percentage, resets_at)| Window { used_percentage, resets_at });
        Usage { five_hour: w(five), seven_day: w(seven) }
    }

    #[test]
    fn within_one_window_usage_only_grows() {
        let stored = usage(Some((60.0, 1000)), Some((30.0, 9000)));
        assert_eq!(merge(Some(&stored), usage(Some((20.0, 1000)), Some((35.0, 9000)))), (usage(Some((60.0, 1000)), Some((35.0, 9000))), true));
        // reset times from separate requests may differ by a little: still the same window
        assert_eq!(merge(Some(&stored), usage(Some((61.0, 1030)), None)), (usage(Some((61.0, 1030)), Some((30.0, 9000))), true));
    }

    #[test]
    fn an_idle_session_reposting_an_older_window_changes_nothing() {
        let stored = usage(Some((60.0, 100_000)), Some((30.0, 900_000)));
        assert_eq!(merge(Some(&stored), usage(Some((20.0, 80_000)), Some((25.0, 800_000)))), (stored.clone(), false));
        assert_eq!(merge(Some(&stored), usage(None, Some((25.0, 800_000)))), (stored, false));
    }

    #[test]
    fn a_later_reset_is_a_new_window_even_with_a_lower_number() {
        let stored = usage(Some((90.0, 100_000)), None);
        assert_eq!(merge(Some(&stored), usage(Some((3.0, 118_000)), Some((40.0, 900_000)))), (usage(Some((3.0, 118_000)), Some((40.0, 900_000))), true));
        assert_eq!(merge(None, usage(None, Some((40.0, 900_000)))), (usage(None, Some((40.0, 900_000))), true));
    }

    #[test]
    fn terminal_line_rounds_like_the_notch() {
        let line = |p: f64| line_of(Some(&usage(None, Some((p, 100)))), None, 0);
        assert_eq!([line(42.5), line(99.5), line(99.99), line(100.0), line(0.4)], ["7d 43%", "7d 99%", "7d 99%", "7d 100%", "7d 0%"]);
    }

    #[test]
    fn our_command_single_quotes_paths_with_shell_characters() {
        assert_eq!(line_command("C:\\Users\\A&B\\Nectar\\nectar.exe"), "'C:/Users/A&B/Nectar/nectar.exe' statusline");
        assert_eq!(line_command("C:\\Users\\O'Neil $x\\nectar.exe"), "'C:/Users/O'\\''Neil $x/nectar.exe' statusline");
        assert_eq!(line_command("C:\\Users\\Çağrı\\nectar.exe"), "C:/Users/Çağrı/nectar.exe statusline");
        assert!(is_our_command(&line_command("C:\\Users\\A(1)!\\nectar.exe")));
        assert!(is_our_command("\"C:/Program Files/Nectar/nectar.exe\" statusline"));
    }

    #[test]
    fn a_line_without_a_command_is_left_alone_and_not_chained() {
        let odd = serde_json::json!({ "type": "command" });
        let mut map = settings(Some(odd.clone()));
        assert_eq!(sync_status_line(&mut map, Some("x nectar.exe statusline"), None), Chain::Keep);
        assert_eq!(map["statusLine"], odd);
    }
}
