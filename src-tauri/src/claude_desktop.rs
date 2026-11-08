//! Sessions of the Claude desktop app's Code tab. The app opens one by its `local_…` id through its
//! own link, `claude://code/continue?session=local_…`; the id comes from the hook's environment
//! (`CLAUDE_CODE_HOST_SESSION_ID`) or from the app's session files, which name the Claude Code
//! session id (`cliSessionId`) and the title its notifications carry.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The app's AppUserModelID, which its Windows toasts carry.
pub const AUMID: &str = "com.squirrel.AnthropicClaude.claude";

/// Session files older than this belong to no running agent; skipping them keeps a click cheap.
const RECENT: Duration = Duration::from_secs(3 * 24 * 60 * 60);
const MAX_FILES: usize = 60;

pub fn is_local_id(id: &str) -> bool {
    id.strip_prefix("local_")
        .is_some_and(|rest| (1..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

pub fn session_url(id: &str) -> String {
    format!("claude://code/continue?session={id}")
}

pub fn is_claude_aumid(aumid: &str) -> bool {
    aumid.eq_ignore_ascii_case(AUMID)
}

/// The hook process inherits this from Claude Code when the desktop app runs it.
pub fn host_session_from_env() -> Option<String> {
    std::env::var("CLAUDE_CODE_HOST_SESSION_ID").ok().filter(|id| is_local_id(id))
}

fn sessions_root() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Claude").join("claude-code-sessions"))
}

/// `local_*.json` under `<account>/<organization>/`, most recently written first.
fn session_files(root: &Path, now: SystemTime) -> Vec<PathBuf> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    let dirs = |p: &Path| std::fs::read_dir(p).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect::<Vec<_>>();
    for account in dirs(root) {
        for org in dirs(&account) {
            for entry in std::fs::read_dir(&org).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with("local_") || !name.ends_with(".json") {
                    continue;
                }
                let Some(modified) = entry.metadata().ok().and_then(|m| m.modified().ok()) else { continue };
                if now.duration_since(modified).map(|age| age <= RECENT).unwrap_or(true) {
                    files.push((modified, entry.path()));
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().take(MAX_FILES).map(|(_, path)| path).collect()
}

fn local_id_of(session: &serde_json::Value) -> Option<String> {
    session["sessionId"].as_str().filter(|id| is_local_id(id)).map(str::to_string)
}

fn runs_cli_session(session: &serde_json::Value, cli: &str) -> bool {
    ["cliSessionId", "lastAssistantCliSessionId"].iter().any(|key| session[*key].as_str() == Some(cli))
}

/// A toast's title is the session title, or `title · account` when several accounts are signed in.
fn title_matches(session_title: &str, toast_title: &str) -> bool {
    let (session_title, toast_title) = (session_title.trim(), toast_title.trim());
    !session_title.is_empty()
        && (toast_title == session_title || toast_title.strip_prefix(session_title).is_some_and(|rest| rest.starts_with(" \u{b7} ")))
}

/// Local ids of the sessions `pick` accepts, newest first.
fn matching<'a>(root: &Path, now: SystemTime, quick: Option<&'a str>, pick: impl Fn(&serde_json::Value) -> bool + 'a) -> impl Iterator<Item = String> + 'a {
    session_files(root, now).into_iter().filter_map(move |path| {
        let text = std::fs::read_to_string(path).ok()?;
        if quick.is_some_and(|needle| !text.contains(needle)) {
            return None;
        }
        let session: serde_json::Value = serde_json::from_str(&text).ok()?;
        pick(&session).then(|| local_id_of(&session)).flatten()
    })
}

fn by_cli_session_in(root: &Path, now: SystemTime, cli: &str) -> Option<String> {
    if cli.is_empty() || cli.len() > 80 {
        return None;
    }
    matching(root, now, Some(cli), |session| runs_cli_session(session, cli)).next()
}

/// Only an unambiguous title counts: with two matching sessions the toast could be about either.
fn by_toast_title_in(root: &Path, now: SystemTime, title: &str) -> Option<String> {
    if title.trim().is_empty() {
        return None;
    }
    let mut found = matching(root, now, None, |session| !session["isArchived"].as_bool().unwrap_or(false) && session["title"].as_str().is_some_and(|t| title_matches(t, title)));
    let first = found.next()?;
    found.all(|other| other == first).then_some(first)
}

/// The desktop session running the Claude Code session `cli` (the hook's `session_id`).
pub fn by_cli_session(cli: &str) -> Option<String> {
    by_cli_session_in(&sessions_root()?, SystemTime::now(), cli)
}

/// The desktop session a toast is about, by its title; none when several sessions carry it.
pub fn by_toast_title(title: &str) -> Option<String> {
    by_toast_title_in(&sessions_root()?, SystemTime::now(), title)
}

/// The app's own executable (`...\AnthropicClaude\app-<version>\claude.exe`), not the Claude Code CLI.
fn is_app_image(path: &str) -> bool {
    let path = path.to_ascii_lowercase().replace('/', "\\");
    path.ends_with(r"\claude.exe") && path.contains(r"\anthropicclaude\")
}

/// Whether `hwnd` is a window of the Claude desktop app, so the agent behind it runs in the app.
pub fn is_app_window(hwnd: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let hwnd = HWND(hwnd as *mut _);
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != 0 && unsafe { crate::commands::process_image_path(pid) }.is_some_and(|p| is_app_image(&p)) {
        return true;
    }
    unsafe { let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED); }
    crate::utils::get_window_app_user_model_id(hwnd).is_some_and(|id| is_claude_aumid(&id))
}

/// Opens the session in the app; `window` (or the app's own window) is brought forward as well,
/// since Windows may not let the app take the foreground from a link alone.
pub async fn open(id: &str, window: Option<isize>) -> bool {
    if !is_local_id(id) || tauri_plugin_opener::open_url(session_url(id), None::<&str>).is_err() {
        return false;
    }
    let window = match window {
        Some(w) => Some(w),
        None => tauri::async_runtime::spawn_blocking(|| crate::notifications::window_for_app(AUMID)).await.ok().flatten(),
    };
    if let Some(hwnd) = window {
        crate::commands::bring_window_forward(hwnd).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ids_are_checked() {
        assert!(is_local_id("local_089703a0-1645-4c25-afc4-ab8e87015933"));
        assert!(!is_local_id("local_"));
        assert!(!is_local_id("089703a0-1645-4c25-afc4-ab8e87015933"));
        assert!(!is_local_id("local_a&b=c"));
        assert!(!is_local_id("local_a/../b"));
        assert!(!is_local_id(&format!("local_{}", "a".repeat(65))));
        assert_eq!(session_url("local_ab-12"), "claude://code/continue?session=local_ab-12");
    }

    #[test]
    fn cli_session_matches_current_or_last_id() {
        let session = serde_json::json!({ "sessionId": "local_x1", "cliSessionId": "aaa", "lastAssistantCliSessionId": "bbb" });
        assert!(runs_cli_session(&session, "aaa"));
        assert!(runs_cli_session(&session, "bbb"));
        assert!(!runs_cli_session(&session, "ccc"));
        assert_eq!(local_id_of(&session).as_deref(), Some("local_x1"));
        assert_eq!(local_id_of(&serde_json::json!({ "sessionId": "cse_remote" })), None);
    }

    #[test]
    fn toast_titles_match_with_or_without_account() {
        assert!(title_matches("Nectar kurulumu", "Nectar kurulumu"));
        assert!(title_matches("Nectar kurulumu", "Nectar kurulumu \u{b7} Work"));
        assert!(!title_matches("Nectar", "Nectar kurulumu"));
        assert!(!title_matches("", "Claude Code"));
        assert!(!title_matches("Fix bug", "Claude Code"));
    }

    #[test]
    fn local_id_edges() {
        assert!(is_local_id(&format!("local_{}", "a".repeat(64))));
        assert!(is_local_id("local_A-9"));
        assert!(!is_local_id("local_a\n"), "a trailing newline is not part of an id");
        assert!(!is_local_id("local_a b"));
        assert!(!is_local_id("local_\u{e9}"));
        assert!(!is_local_id("Local_ab"));
        assert!(!is_local_id(" local_ab"));
        assert!(!is_local_id("local_a_b"), "underscores are not allowed after the prefix");
        assert!(!is_local_id("local_a%26b"));
        assert!(!is_local_id(""));
    }

    #[test]
    fn session_url_of_a_valid_id_has_no_extra_query() {
        let url = session_url("local_ab-12");
        assert_eq!(url.matches('?').count(), 1);
        assert!(!url.contains('&') && !url.contains('#') && !url.contains(' '));
    }

    #[test]
    fn toast_title_edges() {
        assert!(title_matches("  Fix bug  ", "Fix bug"), "whitespace is trimmed");
        assert!(title_matches("Fix bug", "Fix bug \u{b7} Work"));
        assert!(!title_matches("Fix bug", "Fix bugs"), "a longer word is another title");
        assert!(!title_matches("Fix bug", "Fix bug \u{b7}x"), "the account separator needs its spaces");
        assert!(!title_matches("Fix bug", "fix bug"), "titles are case sensitive");
        assert!(!title_matches("   ", "   "));
        assert!(!title_matches("Fix bug \u{b7} Work", "Fix bug"));
    }

    #[test]
    fn lookups_refuse_empty_or_oversized_input() {
        assert_eq!(by_cli_session(""), None);
        assert_eq!(by_cli_session(&"a".repeat(81)), None);
        assert_eq!(by_toast_title(""), None);
        assert_eq!(by_toast_title("   "), None);
    }

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nectar-claude-desktop-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, rel: &str, session: serde_json::Value, age: Duration) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, session.to_string()).unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - age).unwrap();
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    fn session(id: &str, cli: &str, title: &str) -> serde_json::Value {
        serde_json::json!({ "sessionId": id, "cliSessionId": cli, "title": title, "isArchived": false })
    }

    #[test]
    fn finds_sessions_two_levels_down_only() {
        let root = TempRoot::new("depth");
        root.write("acc/org/local_a.json", session("local_a", "cli-a", "A"), MIN);
        root.write("acc/org/local_b.json", serde_json::json!({ "sessionId": "local_b", "lastAssistantCliSessionId": "cli-b" }), MIN);
        root.write("local_c.json", session("local_c", "cli-c", "C"), MIN);
        root.write("acc/local_d.json", session("local_d", "cli-d", "D"), MIN);
        root.write("acc/org/deeper/local_e.json", session("local_e", "cli-e", "E"), MIN);
        root.write("acc/org/remote_f.json", session("local_f", "cli-f", "F"), MIN);
        root.write("acc/org/local_g.txt", session("local_g", "cli-g", "G"), MIN);
        let now = SystemTime::now();
        assert_eq!(by_cli_session_in(&root.0, now, "cli-a").as_deref(), Some("local_a"));
        assert_eq!(by_cli_session_in(&root.0, now, "cli-b").as_deref(), Some("local_b"));
        for cli in ["cli-c", "cli-d", "cli-e", "cli-f", "cli-g", "cli-x"] {
            assert_eq!(by_cli_session_in(&root.0, now, cli), None, "{cli}");
        }
        assert_eq!(by_cli_session_in(&root.0.join("missing"), now, "cli-a"), None);
    }

    #[test]
    fn skips_old_files_and_prefers_the_newest() {
        let root = TempRoot::new("age");
        root.write("acc/org/local_old.json", session("local_old", "cli-old", "Old"), RECENT + MIN);
        root.write("acc/org/local_was.json", session("local_was", "cli-x", "Was"), 10 * MIN);
        root.write("acc/other/local_now.json", serde_json::json!({ "sessionId": "local_now", "lastAssistantCliSessionId": "cli-x" }), MIN);
        let now = SystemTime::now();
        assert_eq!(by_cli_session_in(&root.0, now, "cli-old"), None);
        assert_eq!(by_cli_session_in(&root.0, now, "cli-x").as_deref(), Some("local_now"));
    }

    #[test]
    fn reads_at_most_the_newest_files() {
        let root = TempRoot::new("cap");
        for i in 0..=MAX_FILES as u32 {
            root.write(&format!("acc/org/local_s{i}.json"), session(&format!("local_s{i}"), &format!("cli-{i}"), "T"), MIN * (i + 1));
        }
        let now = SystemTime::now();
        let last = MAX_FILES - 1;
        assert_eq!(by_cli_session_in(&root.0, now, &format!("cli-{last}")), Some(format!("local_s{last}")));
        assert_eq!(by_cli_session_in(&root.0, now, &format!("cli-{MAX_FILES}")), None);
    }

    #[test]
    fn skips_broken_files_and_invalid_ids() {
        let root = TempRoot::new("broken");
        std::fs::create_dir_all(root.0.join("acc/org")).unwrap();
        std::fs::write(root.0.join("acc/org/local_x.json"), "{ not json cli-x").unwrap();
        root.write("acc/org/local_y.json", session("local_y&z", "cli-y", "Y"), MIN);
        let now = SystemTime::now();
        assert_eq!(by_cli_session_in(&root.0, now, "cli-x"), None);
        assert_eq!(by_cli_session_in(&root.0, now, "cli-y"), None);
    }

    #[test]
    fn toast_title_must_name_one_session() {
        let root = TempRoot::new("title");
        root.write("acc/org/local_a.json", session("local_a", "cli-a", "Fix bug"), MIN);
        root.write("acc/org/local_b.json", serde_json::json!({ "sessionId": "local_b", "title": "Fix bug", "isArchived": true }), MIN);
        root.write("acc/org/local_c.json", session("local_c", "cli-c", "Release"), MIN);
        root.write("acc/org/local_d.json", session("local_d", "cli-d", "Release"), 2 * MIN);
        root.write("acc/org/local_e.json", session("local_e", "cli-e", "Notes"), MIN);
        root.write("acc/org/local_f.json", session("local_f", "cli-f", "Notes \u{b7} Work"), MIN);
        let now = SystemTime::now();
        assert_eq!(by_toast_title_in(&root.0, now, "Fix bug").as_deref(), Some("local_a"), "archived sessions do not count");
        assert_eq!(by_toast_title_in(&root.0, now, "Fix bug \u{b7} Work").as_deref(), Some("local_a"));
        assert_eq!(by_toast_title_in(&root.0, now, "Release"), None, "two sessions share the title");
        assert_eq!(by_toast_title_in(&root.0, now, "Notes").as_deref(), Some("local_e"));
        assert_eq!(by_toast_title_in(&root.0, now, "Notes \u{b7} Work"), None, "a title, or another title with an account suffix");
        assert_eq!(by_toast_title_in(&root.0, now, "Other"), None);
    }

    #[test]
    fn toast_title_seen_twice_for_the_same_session_is_not_ambiguous() {
        let root = TempRoot::new("same");
        root.write("acc/org/local_a.json", session("local_a", "cli-a", "Fix bug"), MIN);
        root.write("acc/org2/local_a.json", session("local_a", "cli-a", "Fix bug"), 2 * MIN);
        assert_eq!(by_toast_title_in(&root.0, SystemTime::now(), "Fix bug").as_deref(), Some("local_a"));
    }

    #[test]
    fn app_image_is_the_desktop_app_not_the_cli() {
        assert!(is_app_image(r"C:\Users\me\AppData\Local\AnthropicClaude\app-4.23118.0\claude.exe"));
        assert!(is_app_image(r"c:\users\me\appdata\local\anthropicclaude\app-1.0.0\CLAUDE.EXE"));
        assert!(is_app_image("C:/Users/me/AppData/Local/AnthropicClaude/app-1/claude.exe"));
        assert!(!is_app_image(r"C:\Users\me\.local\bin\claude.exe"));
        assert!(!is_app_image(r"C:\Users\me\AppData\Roaming\Claude\claude-code\2.1.0\claude.exe"));
        assert!(!is_app_image(r"C:\Users\me\AppData\Local\AnthropicClaude\app-1\notclaude.exe"));
        assert!(!is_app_image(r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal\WindowsTerminal.exe"));
        assert!(!is_app_image(""));
    }

    #[test]
    fn session_json_without_fields_never_matches() {
        let empty = serde_json::json!({});
        assert!(!runs_cli_session(&empty, "aaa"));
        assert!(!runs_cli_session(&serde_json::json!({ "cliSessionId": null }), "aaa"));
        assert!(!runs_cli_session(&serde_json::json!({ "cliSessionId": 5 }), "5"));
        assert_eq!(local_id_of(&empty), None);
        assert_eq!(local_id_of(&serde_json::json!({ "sessionId": "local_a&b" })), None);
    }

    /// Run from a Claude desktop session: `cargo test --bin nectar claude_desktop -- --ignored`.
    #[test]
    #[ignore]
    fn finds_the_running_desktop_session() {
        let cli = std::env::var("CLAUDE_CODE_SESSION_ID").expect("run inside Claude Code");
        assert_eq!(by_cli_session(&cli), host_session_from_env());
    }

    #[test]
    fn claude_aumid() {
        assert!(is_claude_aumid("com.squirrel.AnthropicClaude.claude"));
        assert!(!is_claude_aumid("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
    }
}
