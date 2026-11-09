#[cfg(feature = "phone-link")]
use crate::phone_link::remote::Deny;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const MAX_HEAD: usize = 8192;
const MAX_BODY: usize = 4096;
/// An ask's full command for the phone (all lines); the notch keeps the short one-line `detail`.
const ASK_FULL: usize = 2000;
/// JSON bytes of the command the hook puts in a `/ask` body, so the body stays under `MAX_BODY`.
const ASK_SEND_BYTES: usize = 3000;
/// JSON bytes of the notch's short `detail` in a `/ask` body (the notch shows 200 characters).
const ASK_DETAIL_BYTES: usize = 400;
/// Tools whose request is mostly content besides the field the notch shows (an old hook sent only the path).
const CONTENT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];
const MAX_CONNECTIONS: usize = 8;
const ASK_WAIT: u64 = 120;
const SETTING: &str = "nectar-agent-api";

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// Ask ids start at a random 48-bit value each run, so an id from before a restart never names a new ask.
static NEXT_ASK: OnceLock<AtomicU64> = OnceLock::new();
static PENDING: Mutex<Option<HashMap<u64, (mpsc::Sender<bool>, PendingAsk)>>> = Mutex::new(None);

/// A permission request waiting for Allow / Deny (shown in the notch and, with `agentAsk`, on the phone).
#[derive(Serialize, Clone, Debug)]
#[cfg_attr(not(feature = "phone-link"), allow(dead_code))]
pub struct PendingAsk {
    pub id: u64,
    pub name: Option<String>,
    pub project: Option<String>,
    pub tool: Option<String>,
    pub detail: Option<String>,
    /// Every line of the command, so the phone never approves a command it shows only in part.
    pub full: Option<String>,
    /// `full` is not the whole command (cut by the hook or here): the phone may only deny.
    pub truncated: bool,
    /// Random per ask; the phone's answer must carry it, so a card from an earlier ask can never answer this one.
    pub key: String,
    #[serde(skip)]
    window: Option<u64>,
    #[serde(skip)]
    session: Option<String>,
    #[serde(skip)]
    host: Option<String>,
}

#[cfg(all(test, feature = "phone-link"))]
impl PendingAsk {
    pub fn sample(id: u64, text: &str) -> Self {
        let long = |n: usize| Some(text.chars().cycle().take(n).collect::<String>());
        PendingAsk { id, name: long(40), project: long(40), tool: long(40), detail: long(200), full: long(ASK_FULL), truncated: false, key: "0".repeat(16), window: None, session: None, host: None }
    }
}

#[derive(Deserialize)]
struct Incoming {
    state: String,
    name: Option<String>,
    project: Option<String>,
    window: Option<u64>,
    detail: Option<String>,
    tool: Option<String>,
    session: Option<String>,
    title: Option<String>,
    /// The Claude desktop app's own session id (`local_…`), when the desktop app runs the agent.
    host: Option<String>,
}

#[derive(Deserialize)]
struct IncomingAsk {
    name: Option<String>,
    project: Option<String>,
    window: Option<u64>,
    session: Option<String>,
    host: Option<String>,
    tool: Option<String>,
    detail: Option<String>,
    /// The whole request for the phone (the hook's `full`); without it `detail` is used.
    full: Option<String>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Serialize, Clone)]
struct Ask {
    id: u64,
    tool: Option<String>,
    detail: Option<String>,
}

#[derive(Serialize, Clone)]
struct SessionEnd {
    name: Option<String>,
    project: Option<String>,
    window: Option<u64>,
    session: Option<String>,
}

#[derive(Serialize, Clone)]
struct Activity {
    state: &'static str,
    name: Option<String>,
    project: Option<String>,
    window: Option<u64>,
    session: Option<String>,
    host: Option<String>,
    detail: Option<String>,
    tool: Option<String>,
    title: Option<String>,
    idle: bool,
    fresh: bool,
    ask: Option<Ask>,
    cancelled: Option<u64>,
}

pub fn data_dir() -> Option<PathBuf> {
    crate::identity::data_dir()
}

fn config_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("agent.json"))
}

fn host_of(raw: Option<String>) -> Option<String> {
    raw.filter(|id| crate::claude_desktop::is_local_id(id))
}

fn normalize(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

fn state_of(raw: &str) -> Option<Option<&'static str>> {
    match normalize(raw).as_str() {
        "working" | "thinking" | "running" | "start" | "prompt" => Some(Some("working")),
        "tool" | "tool_running" => Some(Some("tool")),
        "needs_input" | "input" | "waiting" | "idle_prompt" => Some(Some("needs_input")),
        "done" | "completed" | "complete" | "finished" => Some(Some("done")),
        "error" | "failed" => Some(Some("error")),
        "clear" | "idle" | "stop" => Some(None),
        _ => None,
    }
}

fn clean(text: Option<String>, limit: usize) -> Option<String> {
    let cleaned: String = text?.chars().filter(|c| !c.is_control()).take(limit).collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// One line for the notch: every line break becomes a space, so two statements never read as one.
fn flat(text: Option<String>) -> Option<String> {
    text.map(|t| t.replace("\r\n", " ").chars().map(|c| if is_line_break(c) { ' ' } else { c }).collect())
}

/// Keeps line breaks (CRLF, a lone CR and Unicode line separators become LF; tabs two spaces), drops other control characters and cuts to `limit` characters.
fn clean_lines(text: Option<&str>, limit: usize) -> (Option<String>, bool) {
    let Some(text) = text else { return (None, false) };
    let text: String = text.replace("\r\n", "\n").replace('\t', "  ").chars().map(|c| if is_line_break(c) { '\n' } else { c }).collect();
    let kept: Vec<char> = text.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
    let cut = kept.len() > limit;
    let joined: String = kept.into_iter().take(limit).collect();
    let trimmed = joined.trim_start_matches('\n').trim_end().to_string();
    ((!trimmed.is_empty()).then_some(trimmed), cut)
}

/// The phone's view of an ask and whether it is incomplete. An old hook sends only the path
/// for an edit, so its content is unseen and the phone may only deny.
fn ask_full(incoming: &IncomingAsk) -> (Option<String>, bool) {
    let (full, cut) = clean_lines(incoming.full.as_deref().or(incoming.detail.as_deref()), ASK_FULL);
    let unseen = incoming.full.is_none() && incoming.tool.as_deref().is_some_and(|t| CONTENT_TOOLS.contains(&t));
    let truncated = full.is_some() && (cut || incoming.truncated || unseen);
    (full, truncated)
}

/// Cuts `text` so its JSON string form takes at most `budget` bytes.
fn fit_json(text: &str, budget: usize) -> (String, bool) {
    let mut used = 2;
    for (i, c) in text.char_indices() {
        used += match c {
            '"' | '\\' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        };
        if used > budget {
            return (text[..i].to_string(), true);
        }
    }
    (text.to_string(), false)
}

fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut bytes = [0u8; N];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.ok().ok()?;
    Some(bytes)
}

fn random_token() -> Option<String> {
    Some(random_bytes::<24>()?.iter().map(|b| format!("{b:02x}")).collect())
}

fn next_ask_id() -> u64 {
    NEXT_ASK
        .get_or_init(|| {
            let seed = random_bytes::<8>()
                .map(u64::from_le_bytes)
                .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1));
            AtomicU64::new((seed >> 16).max(2))
        })
        .fetch_add(1, Ordering::SeqCst)
}

/// An empty key (no randomness) never matches, so the phone then cannot answer that ask.
fn ask_key() -> String {
    random_bytes::<8>().map(|b| b.iter().map(|b| format!("{b:02x}")).collect()).unwrap_or_default()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn reply(stream: &mut TcpStream, status: &str) {
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

fn reply_json(stream: &mut TcpStream, body: &str) {
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn take_pending(id: u64) -> Option<(mpsc::Sender<bool>, PendingAsk)> {
    PENDING.lock().ok().and_then(|mut map| map.as_mut().and_then(|m| m.remove(&id)))
}

pub fn answer(id: u64, allow: bool) {
    if let Some((sender, _)) = take_pending(id) {
        let _ = sender.send(allow);
        // The phone's status lists pending asks; this one is gone now.
        #[cfg(feature = "phone-link")]
        crate::phone_link::host::mark_dirty();
    }
}

#[cfg(feature = "phone-link")]
pub fn pending_asks() -> Vec<PendingAsk> {
    let mut asks: Vec<PendingAsk> =
        PENDING.lock().ok().and_then(|map| map.as_ref().map(|m| m.values().map(|(_, a)| a.clone()).collect())).unwrap_or_default();
    asks.sort_by_key(|a| a.id);
    asks
}

#[cfg(feature = "phone-link")]
/// Takes the ask only when `key` is the one it was created with. A truncated ask (the phone
/// never saw all of it) may only be denied (design appendix B); allowing it leaves it pending.
fn take_pending_keyed(id: u64, key: &str, allow: bool) -> Result<(mpsc::Sender<bool>, PendingAsk), Deny> {
    let mut map = PENDING.lock().map_err(|_| Deny::Failed)?;
    let map = map.as_mut().ok_or(Deny::Failed)?;
    let ask = &map.get(&id).ok_or(Deny::Failed)?.1;
    if ask.key.is_empty() || !same(&ask.key, key) {
        return Err(Deny::Failed);
    }
    if allow && ask.truncated {
        return Err(Deny::Denied);
    }
    map.remove(&id).ok_or(Deny::Failed)
}

/// Answer from a paired phone (`agentAsk`). Also clears the request in the notch.
#[cfg(feature = "phone-link")]
pub fn answer_remote(app: &AppHandle, id: u64, key: &str, allow: bool) -> Result<(), Deny> {
    let (sender, ask) = take_pending_keyed(id, key, allow)?;
    let delivered = sender.send(allow).is_ok();
    let _ = app.emit(
        "agent-activity",
        Activity { state: "working", name: ask.name, project: ask.project, window: ask.window, session: ask.session, host: ask.host, detail: None, tool: None, title: None, idle: false, fresh: false, ask: None, cancelled: Some(id) },
    );
    if delivered { Ok(()) } else { Err(Deny::Failed) }
}

fn wait_for_answer(stream: &mut TcpStream, id: u64, rx: &mpsc::Receiver<bool>) -> Option<bool> {
    let _ = stream.set_nonblocking(true);
    let mut waited = 0;
    while waited < ASK_WAIT {
        if let Ok(allow) = rx.recv_timeout(Duration::from_secs(1)) {
            let _ = stream.set_nonblocking(false);
            return Some(allow);
        }
        waited += 1;
        let mut probe = [0u8; 1];
        if matches!(stream.peek(&mut probe), Ok(0)) {
            break;
        }
    }
    if let Ok(mut map) = PENDING.lock() {
        if let Some(map) = map.as_mut() {
            map.remove(&id);
        }
    }
    let _ = stream.set_nonblocking(false);
    None
}

fn handle(mut stream: TcpStream, app: &AppHandle, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HEAD {
            return reply(&mut stream, "431 Request Header Fields Too Large");
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("");
    let (ask, usage) = match request {
        "POST /activity HTTP/1.1" | "POST /activity HTTP/1.0" => (false, false),
        "POST /ask HTTP/1.1" | "POST /ask HTTP/1.0" => (true, false),
        "POST /usage HTTP/1.1" | "POST /usage HTTP/1.0" => (false, true),
        _ => return reply(&mut stream, "404 Not Found"),
    };
    let (mut auth, mut length) = (String::new(), 0usize);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "authorization" => auth = value.trim().to_string(),
                "content-length" => length = value.trim().parse().unwrap_or(usize::MAX),
                _ => {}
            }
        }
    }
    if !same(&auth, &format!("Bearer {token}")) {
        return reply(&mut stream, "401 Unauthorized");
    }
    if length > MAX_BODY {
        return reply(&mut stream, "413 Payload Too Large");
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);

    if usage {
        let Some(usage) = crate::claude_usage::usage_of_body(&body) else {
            return reply(&mut stream, "400 Bad Request");
        };
        crate::claude_usage::receive(app, usage);
        return reply(&mut stream, "204 No Content");
    }

    if ask {
        let Ok(incoming) = serde_json::from_slice::<IncomingAsk>(&body) else {
            return reply(&mut stream, "400 Bad Request");
        };
        let id = next_ask_id();
        let (tx, rx) = mpsc::channel();
        let (full, truncated) = ask_full(&incoming);
        let (name, project, window, session) = (clean(incoming.name, 40), clean(incoming.project, 40), incoming.window, clean(incoming.session, 80));
        let host = host_of(incoming.host);
        let (tool, detail) = (clean(incoming.tool, 40), clean(flat(incoming.detail), 200));
        let meta = PendingAsk { id, name: name.clone(), project: project.clone(), tool: tool.clone(), detail: detail.clone(), full, truncated, key: ask_key(), window, session: session.clone(), host: host.clone() };
        if let Ok(mut map) = PENDING.lock() {
            map.get_or_insert_with(HashMap::new).insert(id, (tx, meta));
        }
        let _ = app.emit(
            "agent-activity",
            Activity {
                state: "needs_input",
                name: name.clone(),
                project: project.clone(),
                window,
                session: session.clone(),
                host: host.clone(),
                detail: None,
                tool: None,
                title: None,
                idle: false,
                fresh: false,
                ask: Some(Ask { id, tool, detail }),
                cancelled: None,
            },
        );
        return match wait_for_answer(&mut stream, id, &rx) {
            Some(true) => reply_json(&mut stream, r#"{"decision":"allow"}"#),
            Some(false) => reply_json(&mut stream, r#"{"decision":"deny"}"#),
            None => {
                let _ = app.emit("agent-activity", Activity { state: "working", name, project, window, session, host, detail: None, tool: None, title: None, idle: false, fresh: false, ask: None, cancelled: Some(id) });
                reply(&mut stream, "204 No Content");
            }
        };
    }

    let Ok(incoming) = serde_json::from_slice::<Incoming>(&body) else {
        return reply(&mut stream, "400 Bad Request");
    };
    let Some(state) = state_of(&incoming.state) else {
        return reply(&mut stream, "400 Bad Request");
    };
    let session = clean(incoming.session, 80);
    let Some(state) = state else {
        let _ = app.emit(
            "agent-activity-end",
            SessionEnd { name: clean(incoming.name, 40), project: clean(incoming.project, 40), window: incoming.window, session },
        );
        return reply(&mut stream, "204 No Content");
    };
    let activity = Activity {
        state,
        name: clean(incoming.name, 40),
        project: clean(incoming.project, 40),
        window: incoming.window,
        session,
        host: host_of(incoming.host),
        detail: clean(incoming.detail, 200),
        tool: clean(incoming.tool, 40),
        title: clean(incoming.title, 80),
        idle: normalize(&incoming.state) == "idle_prompt",
        fresh: matches!(normalize(&incoming.state).as_str(), "start" | "prompt"),
        ask: None,
        cancelled: None,
    };
    let _ = app.emit("agent-activity", activity);
    reply(&mut stream, "204 No Content");
}

fn hook_entries(exe: &str) -> Vec<(&'static str, Option<&'static str>, serde_json::Value)> {
    let hook = |args: &[&str], timeout: Option<u64>| {
        let mut all = vec!["agent"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--name", "Claude Code", "--hook"]);
        let mut entry = serde_json::json!({ "type": "command", "command": exe, "args": all });
        if let Some(timeout) = timeout {
            entry["timeout"] = timeout.into();
        }
        entry
    };
    vec![
        ("UserPromptSubmit", None, hook(&["start"], None)),
        ("PreToolUse", None, hook(&["tool"], None)),
        ("PostToolUse", None, hook(&["working"], None)),
        ("PostToolUseFailure", None, hook(&["error"], None)),
        ("PermissionRequest", None, hook(&["ask"], Some(ASK_WAIT + 10))),
        ("Notification", Some("idle_prompt"), hook(&["idle-prompt"], None)),
        ("Stop", None, hook(&["done"], None)),
        ("SessionEnd", None, hook(&["clear"], None)),
    ]
}

/// Only this build's exe: another Nectar install's hooks stay untouched.
fn is_ours(group: &serde_json::Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|hook| hook["args"][0].as_str() == Some("agent") && hook["command"].as_str().is_some_and(crate::identity::is_our_exe))
    })
}

fn sync_claude_hooks(enable: bool, status_line: bool) {
    let Some(path) = std::env::var_os("USERPROFILE").map(|d| PathBuf::from(d).join(".claude").join("settings.json")) else { return };
    let text = std::fs::read_to_string(&path).ok();
    let mut root: serde_json::Value = match text.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => match serde_json::from_str(t) {
            Ok(value) => value,
            Err(_) => return,
        },
        _ if enable => serde_json::json!({}),
        _ => return,
    };
    let original = root.clone();
    let Some(exe) = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()) else { return };
    let Some(object) = root.as_object_mut() else { return };

    let hooks = object.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return };
    for event in hooks.values_mut() {
        if let Some(groups) = event.as_array_mut() {
            groups.retain(|group| !is_ours(group));
        }
    }
    if enable {
        for (event, matcher, hook) in hook_entries(&exe) {
            let mut group = serde_json::json!({ "hooks": [hook] });
            if let Some(matcher) = matcher {
                group["matcher"] = matcher.into();
            }
            match hooks.entry(event).or_insert_with(|| serde_json::json!([])).as_array_mut() {
                Some(groups) => groups.push(group),
                None => return,
            }
        }
    }
    hooks.retain(|_, event| event.as_array().map(|groups| !groups.is_empty()).unwrap_or(true));
    if hooks.is_empty() {
        object.remove("hooks");
    }
    let line_command = status_line.then(|| crate::claude_usage::line_command(&exe));
    let chain = crate::claude_usage::sync_status_line(object, line_command.as_deref(), crate::claude_usage::saved_line());
    if let crate::claude_usage::Chain::Save(line) = &chain {
        if !crate::claude_usage::save_line(line) {
            return;
        }
    }
    if root == original {
        if chain == crate::claude_usage::Chain::Clear {
            crate::claude_usage::clear_line();
        }
        return;
    }

    let Ok(mut out) = serde_json::to_string_pretty(&root) else { return };
    out.push('\n');
    if let Some(text) = &text {
        let backup = path.with_file_name("settings.json.nectar-backup");
        if !backup.exists() {
            let _ = std::fs::write(&backup, text);
        }
    }
    let temp = path.with_file_name("settings.json.nectar-tmp");
    let written = std::fs::write(&temp, out).is_ok() && std::fs::rename(&temp, &path).is_ok();
    if !written {
        let _ = std::fs::remove_file(&temp);
    } else if chain == crate::claude_usage::Chain::Clear {
        crate::claude_usage::clear_line();
    }
}

/// Each settings toggle syncs on its own thread; one sync at a time, so no read-modify-write of
/// `~/.claude/settings.json` and the chain file overlaps another.
static SYNC: Mutex<()> = Mutex::new(());

/// `enable` is asked once the lock is held, so syncs started by quick toggles end in the last state.
fn sync_hooks_when(enable: impl FnOnce() -> bool) {
    let is_build_output = std::env::current_exe().is_ok_and(|p| p.components().any(|c| c.as_os_str() == "target"));
    if cfg!(debug_assertions) || is_build_output {
        return;
    }
    let _one_at_a_time = SYNC.lock().unwrap_or_else(|e| e.into_inner());
    let enable = enable();
    if enable && !std::env::var_os("USERPROFILE").is_some_and(|d| PathBuf::from(d).join(".claude").is_dir()) {
        return;
    }
    sync_claude_hooks(enable, enable && crate::claude_usage::enabled_on_disk());
}

pub fn set_hooks(enable: bool) {
    sync_hooks_when(|| enable);
}

/// The welcome screen offers the Claude Code integration only when `~/.claude` exists.
#[tauri::command]
pub fn claude_code_present() -> bool {
    std::env::var_os("USERPROFILE").is_some_and(|d| PathBuf::from(d).join(".claude").is_dir())
}

/// After `nectar-agent-api` or `nectar-claude-usage` changed (or at start): syncs to the settings as they are when it runs.
pub fn resync_hooks() {
    sync_hooks_when(|| crate::utils::setting_flag(SETTING));
}

pub fn start(app: AppHandle) {
    let enabled = crate::utils::get_setting_str(&app, SETTING).as_deref() == Some("true");
    if !enabled {
        return;
    }
    std::thread::spawn(resync_hooks);
    let Some(token) = random_token() else { return };
    let Ok(listener) = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))) else { return };
    let Ok(addr) = listener.local_addr() else { return };
    let Some(path) = config_path() else { return };
    let config = serde_json::json!({ "port": addr.port(), "token": token }).to_string();
    if std::fs::write(&path, config).is_err() {
        return;
    }

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if !stream.peer_addr().map(|a| a.ip().is_loopback()).unwrap_or(false) {
                continue;
            }
            if ACTIVE.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                ACTIVE.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let app = app.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                handle(stream, &app, &token);
                ACTIVE.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
}

fn project_name() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    dir.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn ancestor_window() -> Option<u64> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER};

    let mut parents: HashMap<u32, (u32, String)> = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase()));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }

    let mut chain = Vec::new();
    let mut pid = std::process::id();
    while let Some((parent, _)) = parents.get(&pid) {
        match parents.get(parent) {
            Some((_, name)) if name != "explorer.exe" && !chain.contains(parent) => chain.push(*parent),
            _ => break,
        }
        pid = *parent;
    }

    struct Search {
        chain: Vec<u32>,
        found: Vec<(usize, u64)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if IsWindowVisible(hwnd).as_bool() && GetWindow(hwnd, GW_OWNER).map(|o| o.0.is_null()).unwrap_or(true) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Some(rank) = search.chain.iter().position(|&p| p == pid) {
                search.found.push((rank, hwnd.0 as usize as u64));
            }
        }
        BOOL(1)
    }
    let mut search = Search { chain, found: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found.into_iter().min_by_key(|(rank, _)| *rank).map(|(_, hwnd)| hwnd)
}

fn flags(args: &[String]) -> (Option<String>, Option<String>) {
    let (mut name, mut project) = (None, None);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--name" | "-n" => name = rest.next().cloned(),
            "--project" | "-p" => project = rest.next().cloned(),
            _ => {}
        }
    }
    (name, project.or_else(project_name))
}

pub(crate) fn post(path: &str, body: String, wait: u64) -> Option<String> {
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config_path()?).ok()?).ok()?;
    let port = config.get("port")?.as_u64()? as u16;
    let token = config.get("token")?.as_str()?;

    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_secs(1)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(wait))).ok()?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

#[derive(Default)]
struct Extra {
    detail: Option<String>,
    tool: Option<String>,
    session: Option<String>,
    title: Option<String>,
}

fn send_with(args: &[String], extra: Extra) -> Option<()> {
    let state = args.first()?.clone();
    let (name, project) = flags(&args[1..]);
    let body = serde_json::json!({
        "state": state,
        "name": name,
        "project": project,
        "window": ancestor_window(),
        "detail": extra.detail,
        "tool": extra.tool,
        "session": extra.session,
        "title": extra.title,
        "host": crate::claude_desktop::host_session_from_env(),
    })
    .to_string();
    post("/activity", body, 2)?.starts_with("HTTP/1.1 204").then_some(())
}

fn tool_name_of(input: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(input).ok()?;
    event["tool_name"].as_str().map(str::to_string)
}

/// Short description of what a tool call targets: the file name, else the first line of the command, URL or pattern.
fn tool_detail_of(input: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(input).ok()?;
    let tool_input = &event["tool_input"];
    if let Some(path) = ["file_path", "notebook_path"].iter().find_map(|key| tool_input[*key].as_str()) {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned());
        return name.or_else(|| Some(path.to_string())).filter(|n| !n.is_empty());
    }
    let text = ["command", "url", "pattern", "query", "description", "prompt", "path"].iter().find_map(|key| tool_input[*key].as_str())?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(120).collect())
}

/// Session title from a UserPromptSubmit hook: the first non-empty line of the prompt, cut to about 60 characters.
fn prompt_title_of(input: &str) -> Option<String> {
    const LIMIT: usize = 60;
    let event: serde_json::Value = serde_json::from_str(input).ok()?;
    let line = event["prompt"].as_str()?.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).find(|l| !l.is_empty())?;
    if line.chars().count() <= LIMIT {
        return Some(line);
    }
    let cut: String = line.chars().take(LIMIT - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

fn session_of(input: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(input).ok()?;
    event["session_id"].as_str().map(str::to_string)
}

pub(crate) fn hook_input() -> String {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let mut input = String::new();
    if !stdin.is_terminal() {
        let _ = stdin.lock().read_to_string(&mut input);
    }
    input
}

/// What a permission request shows: the notch's short `detail` (the command, path or URL) and,
/// for the phone, `full`: that field followed by every other field that changes what runs or is
/// written (an edit's content), so Allow on the phone never covers content the user did not see.
fn ask_texts(tool_input: &serde_json::Value) -> (Option<String>, Option<String>) {
    const SHOWN: [&str; 5] = ["command", "file_path", "url", "pattern", "description"];
    const QUIET: [&str; 3] = ["description", "timeout", "run_in_background"];
    let Some(fields) = tool_input.as_object() else {
        let raw = (!tool_input.is_null()).then(|| tool_input.to_string());
        return (raw.clone(), raw);
    };
    let Some(key) = SHOWN.iter().find(|key| fields.get(**key).is_some_and(|v| v.is_string())) else {
        let raw = (!fields.is_empty()).then(|| tool_input.to_string());
        return (raw.clone(), raw);
    };
    let detail = fields[*key].as_str().unwrap_or_default().to_string();
    let mut full = detail.clone();
    for (name, value) in fields {
        if name == *key || QUIET.contains(&name.as_str()) || value.is_null() {
            continue;
        }
        let text = value.as_str().map(str::to_string).unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default());
        full.push_str(&format!("\n\n{name}:\n{text}"));
    }
    (Some(detail), Some(full))
}

fn ask(args: &[String]) {
    let (name, project) = flags(args);
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let event: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    let session = event["session_id"].as_str();
    let tool_input = &event["tool_input"];
    if event["tool_name"].as_str() == Some("AskUserQuestion") {
        let question = tool_input["questions"][0]["question"].as_str().map(str::to_string);
        let mut forwarded = vec!["needs-input".to_string()];
        forwarded.extend(args.iter().cloned());
        let _ = send_with(&forwarded, Extra { detail: question, session: session.map(str::to_string), ..Default::default() });
        return;
    }
    let (detail, full) = ask_texts(tool_input);
    let detail = detail.map(|d| fit_json(&d, ASK_DETAIL_BYTES).0);
    let (full, truncated) = match full {
        Some(f) => {
            let (f, cut) = fit_json(&f, ASK_SEND_BYTES);
            (Some(f), cut)
        }
        None => (None, false),
    };
    let body = serde_json::json!({
        "name": name,
        "project": project,
        "window": ancestor_window(),
        "session": session,
        "host": crate::claude_desktop::host_session_from_env(),
        "tool": event["tool_name"].as_str(),
        "detail": detail,
        "full": full,
        "truncated": truncated,
    })
    .to_string();

    let Some(response) = post("/ask", body, ASK_WAIT + 5) else { return };
    let decision = response.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok());
    let behavior = match decision.as_ref().and_then(|d| d["decision"].as_str()) {
        Some("allow") => serde_json::json!({ "behavior": "allow" }),
        Some("deny") => serde_json::json!({ "behavior": "deny", "message": "Denied from Nectar" }),
        _ => return,
    };
    println!("{}", serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": behavior } }));
}

pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) == Some("statusline") {
        return Some(crate::claude_usage::run_statusline());
    }
    if args.get(1).map(String::as_str) != Some("agent") {
        return None;
    }
    match args.get(2).map(String::as_str) {
        Some("install-hooks") => {
            set_hooks(true);
            return Some(0);
        }
        Some("remove-hooks") => {
            set_hooks(false);
            return Some(0);
        }
        _ => {}
    }
    if args.get(2).map(String::as_str) == Some("ask") {
        ask(&args[3..]);
        return Some(0);
    }
    let state = args.get(2).map(|s| normalize(s));
    let state = state.as_deref();
    let with_tool = matches!(state, Some("tool" | "error"));
    let input = if args.iter().any(|a| a == "--hook") || state == Some("tool") { hook_input() } else { String::new() };
    let extra = Extra {
        detail: if with_tool { tool_detail_of(&input) } else { None },
        tool: if with_tool { tool_name_of(&input) } else { None },
        session: session_of(&input),
        title: if matches!(state, Some("start" | "prompt")) { prompt_title_of(&input) } else { None },
    };
    let sent = send_with(&args[2..], extra);
    Some(if sent.is_some() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tool_name_from_hook_input() {
        assert_eq!(tool_name_of(r#"{"tool_name":"Bash","tool_input":{}}"#).as_deref(), Some("Bash"));
        assert_eq!(tool_name_of("{}"), None);
        assert_eq!(tool_name_of("not json"), None);
    }

    #[test]
    fn start_and_prompt_map_to_working_and_normalize_folds_case_and_dashes() {
        assert_eq!(state_of("start"), Some(Some("working")));
        assert_eq!(state_of("Prompt"), Some(Some("working")));
        assert_eq!(normalize("  Idle-Prompt "), "idle_prompt");
        assert_eq!(state_of(""), None);
    }

    #[test]
    fn tool_detail_prefers_file_name_then_first_command_line() {
        assert_eq!(tool_detail_of(r#"{"tool_name":"Edit","tool_input":{"file_path":"C:\\a\\src\\App.tsx"}}"#).as_deref(), Some("App.tsx"));
        assert_eq!(tool_detail_of(r#"{"tool_input":{"command":"\n  cargo build\nnext"}}"#).as_deref(), Some("cargo build"));
        assert_eq!(tool_detail_of(r#"{"tool_input":{"pattern":"foo","path":"src"}}"#).as_deref(), Some("foo"));
        assert_eq!(tool_detail_of(r#"{"tool_input":{}}"#), None);
        assert_eq!(tool_detail_of("not json"), None);
    }

    #[test]
    fn prompt_title_is_first_line_and_about_sixty_chars() {
        assert_eq!(prompt_title_of(r#"{"prompt":"\n  Fix   the notch\nsecond line"}"#).as_deref(), Some("Fix the notch"));
        let long = format!(r#"{{"prompt":"{}"}}"#, "word ".repeat(30));
        let title = prompt_title_of(&long).unwrap();
        assert!(title.chars().count() <= 60 && title.ends_with('…'));
        assert_eq!(prompt_title_of(r#"{"prompt":"   "}"#), None);
        assert_eq!(prompt_title_of("{}"), None);
    }

    #[test]
    fn tool_detail_and_title_handle_unicode_and_empty_paths() {
        assert_eq!(tool_detail_of(r#"{"tool_input":{"file_path":""}}"#), None);
        let cmd = format!(r#"{{"tool_input":{{"command":"{}"}}}}"#, "é".repeat(200));
        assert_eq!(tool_detail_of(&cmd).unwrap().chars().count(), 120);
        let long = format!(r#"{{"prompt":"{}"}}"#, "ö".repeat(100));
        assert_eq!(prompt_title_of(&long).unwrap().chars().count(), 60);
    }

    #[test]
    fn old_clients_without_title_still_parse() {
        let incoming: Incoming = serde_json::from_str(r#"{"state":"working","name":"Claude Code"}"#).unwrap();
        assert!(incoming.title.is_none() && incoming.session.is_none());
    }

    #[test]
    fn reads_session_id_from_hook_input() {
        assert_eq!(session_of(r#"{"session_id":"abc","tool_name":"Bash"}"#).as_deref(), Some("abc"));
        assert_eq!(session_of("{}"), None);
    }

    #[test]
    fn clear_states_end_a_session_and_unknown_states_are_rejected() {
        assert_eq!(state_of(" Stop "), Some(None));
        assert_eq!(state_of("idle"), Some(None));
        assert_eq!(state_of("tool-running"), Some(Some("tool")));
        assert_eq!(state_of("needs-input"), Some(Some("needs_input")));
        assert_eq!(state_of("idle-prompt"), Some(Some("needs_input")));
        assert_eq!(state_of("bogus"), None);
    }

    #[test]
    fn ask_command_keeps_every_line_and_says_when_it_was_cut() {
        let (full, cut) = clean_lines(Some("\r\ncd C:\\proj\r\nRemove-Item -Recurse $HOME\\Documents\u{7}\n"), ASK_FULL);
        assert_eq!(full.as_deref(), Some("cd C:\\proj\nRemove-Item -Recurse $HOME\\Documents"));
        assert!(!cut);
        let (full, cut) = clean_lines(Some(&"x".repeat(ASK_FULL + 1)), ASK_FULL);
        assert_eq!(full.map(|f| f.len()), Some(ASK_FULL));
        assert!(cut);
        assert_eq!(clean_lines(Some(" \n "), ASK_FULL), (None, false));
        // PowerShell ends a statement at a lone CR: it must stay a line break, not glue two statements.
        let (full, _) = clean_lines(Some("Get-ChildItem .\\\rrm -r -fo $HOME\u{2028}x\u{85}y"), ASK_FULL);
        assert_eq!(full.as_deref(), Some("Get-ChildItem .\\\nrm -r -fo $HOME\nx\ny"));
        assert_eq!(flat(Some("a\rb\r\nc".into())).as_deref(), Some("a b c"));
        assert_eq!(clean_lines(None, ASK_FULL), (None, false));
    }

    #[test]
    fn hook_cuts_the_ask_command_to_a_json_byte_budget() {
        assert_eq!(fit_json("ls -la", 100), ("ls -la".to_string(), false));
        let (cut, was_cut) = fit_json(&"\\".repeat(100), 22);
        assert!(was_cut);
        assert_eq!(serde_json::to_string(&cut).unwrap().len(), 22);
        let (cut, was_cut) = fit_json(&"ş".repeat(100), 21);
        assert!(was_cut);
        assert!(serde_json::to_string(&cut).unwrap().len() <= 21);
    }

    #[test]
    fn clean_strips_control_chars_trims_and_limits() {
        assert_eq!(clean(Some("  a
b	 ".into()), 40).as_deref(), Some("ab"));
        assert_eq!(clean(Some("
 
".into()), 40), None);
        assert_eq!(clean(Some("abcdef".into()), 3).as_deref(), Some("abc"));
        assert_eq!(clean(None, 3), None);
    }

    #[test]
    fn hooks_of_another_nectar_install_are_not_ours() {
        let group = |exe: &str| serde_json::json!({ "hooks": [{ "type": "command", "command": exe, "args": ["agent", "done"] }] });
        let mine = format!(r"C:\Users\a\AppData\Local\X\{}", crate::identity::APP.exe);
        let other_exe = if crate::identity::APP.exe == crate::identity::LEGACY.exe { crate::identity::SHARED.exe } else { crate::identity::LEGACY.exe };
        let other = format!(r"C:\Users\a\AppData\Local\Y\{other_exe}");
        assert!(is_ours(&group(&mine)));
        assert!(!is_ours(&group(&other)));
        assert!(!is_ours(&serde_json::json!({ "hooks": [{ "type": "command", "command": mine, "args": ["statusline"] }] })));
    }

    #[test]
    fn ask_ids_do_not_restart_and_keys_are_random() {
        let a = next_ask_id();
        assert!(a > 1 && a < 1 << 53, "fits a JS safe integer: {a}");
        assert!(next_ask_id() > a);
        let (k1, k2) = (ask_key(), ask_key());
        assert_eq!(k1.len(), 16);
        assert_ne!(k1, k2);
    }

    #[cfg(feature = "phone-link")]
    #[test]
    fn an_answer_with_another_asks_key_does_not_take_it() {
        let (tx, _rx) = mpsc::channel();
        let id = next_ask_id();
        let ask = PendingAsk { id, name: None, project: None, tool: None, detail: None, full: None, truncated: false, key: "00112233aabbccdd".into(), window: None, session: None, host: None };
        PENDING.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, (tx, ask));
        assert!(take_pending_keyed(id, "ffffffffffffffff", false).is_err());
        assert!(take_pending_keyed(id, "", false).is_err());
        assert!(take_pending_keyed(id + 1, "00112233aabbccdd", false).is_err());
        assert!(take_pending_keyed(id, "00112233aabbccdd", true).is_ok());
        assert!(take_pending(id).is_none());
    }

    #[cfg(feature = "phone-link")]
    #[test]
    fn a_truncated_ask_can_only_be_denied() {
        let (tx, rx) = mpsc::channel();
        let id = next_ask_id();
        let key = "0123456789abcdef";
        let ask = PendingAsk { id, name: None, project: None, tool: Some("Write".into()), detail: None, full: Some("x.ps1".into()), truncated: true, key: key.into(), window: None, session: None, host: None };
        PENDING.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, (tx, ask));
        assert_eq!(take_pending_keyed(id, key, true).err(), Some(Deny::Denied));
        assert!(PENDING.lock().unwrap().as_ref().unwrap().contains_key(&id), "a refused allow leaves the ask waiting");
        let (sender, _) = take_pending_keyed(id, key, false).unwrap();
        sender.send(false).unwrap();
        assert_eq!(rx.recv().unwrap(), false);
    }

    #[test]
    fn the_phone_sees_an_edits_content_or_may_only_deny() {
        let input = serde_json::json!({ "file_path": "C:\\x\\Startup\\x.ps1", "content": "rm -r ~" });
        let (detail, full) = ask_texts(&input);
        assert_eq!(detail.as_deref(), Some("C:\\x\\Startup\\x.ps1"));
        assert_eq!(full.as_deref(), Some("C:\\x\\Startup\\x.ps1\n\ncontent:\nrm -r ~"));
        let input = serde_json::json!({ "command": "ls", "description": "List", "timeout": 5 });
        assert_eq!(ask_texts(&input), (Some("ls".into()), Some("ls".into())));
        let input = serde_json::json!({ "file_path": "a.txt", "edits": [{ "old_string": "a", "new_string": "b" }] });
        assert!(ask_texts(&input).1.unwrap().contains("\"new_string\": \"b\""));

        let old_hook = |tool: &str| IncomingAsk { name: None, project: None, window: None, session: None, host: None, tool: Some(tool.into()), detail: Some("x.ps1".into()), full: None, truncated: false };
        assert!(ask_full(&old_hook("Write")).1, "an old hook sent only the path");
        assert!(!ask_full(&old_hook("Bash")).1);
        let new_hook = IncomingAsk { full: Some("x.ps1\n\ncontent:\nhi".into()), ..old_hook("Write") };
        assert_eq!(ask_full(&new_hook), (Some("x.ps1\n\ncontent:\nhi".into()), false));
    }

    #[test]
    fn host_session_is_optional_and_checked() {
        let old: Incoming = serde_json::from_str(r#"{"state":"working","session":"abc"}"#).unwrap();
        assert_eq!(host_of(old.host), None);
        let new: Incoming = serde_json::from_str(r#"{"state":"working","host":"local_089703a0-1645"}"#).unwrap();
        assert_eq!(host_of(new.host).as_deref(), Some("local_089703a0-1645"));
        let bad: IncomingAsk = serde_json::from_str(r#"{"host":"local_x&source=evil"}"#).unwrap();
        assert_eq!(host_of(bad.host), None);
    }
}
