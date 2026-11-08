// Inner messages, per-device permissions and the remote-control whitelist (design 6.4, 6.5).
// Every decrypted inner message goes through `parse_inbound` and `authorize` before
// anything else happens. Unknown message types and operations are `invalid`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::transfer::{FidOnly, Offer};

pub const MAX_TEXT: usize = 64 * 1024;
pub const MAX_URL: usize = 2048;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenUrl {
    #[default]
    Off,
    Ask,
    On,
}

/// A missing field is `false` / `off`, so permissions added later never widen an old device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Perms {
    pub media: bool,
    pub volume: bool,
    pub lock: bool,
    pub status: bool,
    pub media_info: bool,
    pub agent_status: bool,
    pub agent_ask: bool,
    pub clipboard: bool,
    pub files_in: bool,
    pub text_in: bool,
    pub files_out: bool,
    pub text_out: bool,
    /// PC clipboard text goes to the phone when `nectar-phone-clipboard-sync` is on (T-016).
    pub clipboard_out: bool,
    pub open_url: OpenUrl,
}

impl Perms {
    /// Defaults for a new device, decided by the user (T-010 scope 3, D-018..D-020, D-024).
    pub fn new_device() -> Self {
        Perms {
            media: true,
            volume: true,
            lock: true,
            status: true,
            media_info: true,
            agent_status: true,
            agent_ask: true,
            clipboard: true,
            files_in: true,
            text_in: true,
            files_out: true,
            text_out: true,
            clipboard_out: true,
            open_url: OpenUrl::On,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    VolumeSet(f32),
    VolumeMute,
    VolumeUp,
    VolumeDown,
    PcLock,
    UrlOpen(String),
    ClipboardSet(String),
    StatusGet,
    AgentAnswer { id: u64, key: String, allow: bool },
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Op::MediaPlayPause => "media.play_pause",
            Op::MediaNext => "media.next",
            Op::MediaPrevious => "media.previous",
            Op::VolumeSet(_) => "volume.set",
            Op::VolumeMute => "volume.mute",
            Op::VolumeUp => "volume.up",
            Op::VolumeDown => "volume.down",
            Op::PcLock => "pc.lock",
            Op::UrlOpen(_) => "url.open",
            Op::ClipboardSet(_) => "clipboard.set",
            Op::StatusGet => "status.get",
            Op::AgentAnswer { .. } => "agent.answer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deny {
    Denied,
    Invalid,
    Busy,
    Failed,
}

impl Deny {
    pub fn code(self) -> &'static str {
        match self {
            Deny::Denied => "denied",
            Deny::Invalid => "invalid",
            Deny::Busy => "busy",
            Deny::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Cmd { id: u32, op: Op },
    /// A `cmd` whose op or args did not parse; answered with `invalid`.
    BadCmd { id: u32 },
    /// `id` is optional; with it the PC answers with a `res`.
    Text { id: Option<u32>, text: String },
    Offer(Offer),
    Accept(String),
    Done(String),
    /// The phone declined a PC offer or could not save it.
    Reject { fid: String, e: String },
    /// The phone stored a file the PC sent.
    Saved(String),
    Rekeyed(u32),
    Unpair,
    Bye,
    /// Keepalive while the PWA is visible; a session with no inbound message expires.
    Ping,
}

use super::transfer::is_fid;

fn no_args(a: Option<&Value>) -> bool {
    match a {
        None | Some(Value::Null) => true,
        Some(Value::Object(m)) => m.is_empty(),
        _ => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeArgs {
    v: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlArgs {
    u: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipArgs {
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerArgs {
    id: u64,
    key: String,
    allow: bool,
}

fn args<T: serde::de::DeserializeOwned>(a: Option<&Value>) -> Option<T> {
    serde_json::from_value(a?.clone()).ok()
}

/// Only http(s) URLs with a host; returns the normalized URL.
pub fn check_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL {
        return None;
    }
    let url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.to_string())
}

pub fn url_host(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

pub fn parse_op(op: &str, a: Option<&Value>) -> Option<Op> {
    let none = |o: Op| no_args(a).then_some(o);
    match op {
        "media.play_pause" => none(Op::MediaPlayPause),
        "media.next" => none(Op::MediaNext),
        "media.previous" => none(Op::MediaPrevious),
        "volume.set" => {
            let v = args::<VolumeArgs>(a)?.v;
            (v.is_finite() && (0.0..=1.0).contains(&v)).then_some(Op::VolumeSet(v as f32))
        }
        "volume.mute" => none(Op::VolumeMute),
        "volume.up" => none(Op::VolumeUp),
        "volume.down" => none(Op::VolumeDown),
        "pc.lock" => none(Op::PcLock),
        "url.open" => Some(Op::UrlOpen(check_url(&args::<UrlArgs>(a)?.u)?)),
        "clipboard.set" => {
            let text = args::<ClipArgs>(a)?.text;
            (!text.is_empty() && text.len() <= MAX_TEXT && !text.contains('\0')).then_some(Op::ClipboardSet(text))
        }
        "status.get" => none(Op::StatusGet),
        "agent.answer" => {
            let x = args::<AnswerArgs>(a)?;
            let key_ok = (1..=32).contains(&x.key.len()) && x.key.bytes().all(|b| b.is_ascii_hexdigit());
            key_ok.then_some(Op::AgentAnswer { id: x.id, key: x.key, allow: x.allow })
        }
        _ => None,
    }
}

/// Parses a decrypted phone-to-PC inner message. `None` means drop it as invalid.
pub fn parse_inbound(plain: &[u8]) -> Option<Inbound> {
    let v: Value = serde_json::from_slice(plain).ok()?;
    let obj = v.as_object()?;
    let keys_ok = |allowed: &[&str]| obj.keys().all(|k| allowed.contains(&k.as_str()));
    match obj.get("t")?.as_str()? {
        "cmd" => {
            if !keys_ok(&["t", "id", "op", "a"]) {
                return None;
            }
            let id = u32::try_from(obj.get("id")?.as_u64()?).ok()?;
            let op = obj.get("op").and_then(Value::as_str).and_then(|op| parse_op(op, obj.get("a")));
            Some(match op {
                Some(op) => Inbound::Cmd { id, op },
                None => Inbound::BadCmd { id },
            })
        }
        "text" => {
            if !keys_ok(&["t", "id", "text"]) {
                return None;
            }
            let id = match obj.get("id") {
                None => None,
                Some(v) => Some(u32::try_from(v.as_u64()?).ok()?),
            };
            let text = obj.get("text")?.as_str()?;
            (!text.is_empty() && text.len() <= MAX_TEXT).then(|| Inbound::Text { id, text: text.to_string() })
        }
        "offer" => {
            let o: Offer = serde_json::from_value(v.clone()).ok()?;
            (is_fid(&o.fid) && o.chunks >= 1 && o.chunks <= 64 && o.name.chars().count() <= 255).then_some(Inbound::Offer(o))
        }
        "saved" => {
            let f: FidOnly = serde_json::from_value(v.clone()).ok()?;
            (f.t == "saved" && is_fid(&f.fid)).then_some(Inbound::Saved(f.fid))
        }
        "reject" => {
            if !keys_ok(&["t", "fid", "e"]) {
                return None;
            }
            let fid = obj.get("fid")?.as_str()?;
            let e = obj.get("e")?.as_str()?;
            let e_ok = !e.is_empty() && e.len() <= 40 && e.bytes().all(|b| b.is_ascii_alphanumeric());
            (is_fid(fid) && e_ok).then(|| Inbound::Reject { fid: fid.to_string(), e: e.to_string() })
        }
        "accept" | "done" => {
            let f: FidOnly = serde_json::from_value(v.clone()).ok()?;
            if !is_fid(&f.fid) {
                return None;
            }
            Some(if f.t == "accept" { Inbound::Accept(f.fid) } else { Inbound::Done(f.fid) })
        }
        "rekeyed" => {
            if !keys_ok(&["t", "ep"]) {
                return None;
            }
            Some(Inbound::Rekeyed(u32::try_from(obj.get("ep")?.as_u64()?).ok()?))
        }
        "unpair" => keys_ok(&["t"]).then_some(Inbound::Unpair),
        "bye" => keys_ok(&["t"]).then_some(Inbound::Bye),
        "ping" => keys_ok(&["t"]).then_some(Inbound::Ping),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// `url.open` with `openUrl: "ask"`: the PC user confirms first.
    Confirm,
    Deny(Deny),
}

/// The one permission check for every inbound message (design 6.4).
pub fn authorize(perms: &Perms, msg: &Inbound) -> Decision {
    let gate = |ok: bool| if ok { Decision::Allow } else { Decision::Deny(Deny::Denied) };
    match msg {
        Inbound::Cmd { op, .. } => match op {
            Op::MediaPlayPause | Op::MediaNext | Op::MediaPrevious => gate(perms.media),
            Op::VolumeSet(_) | Op::VolumeMute | Op::VolumeUp | Op::VolumeDown => gate(perms.volume),
            Op::PcLock => gate(perms.lock),
            Op::UrlOpen(_) => match perms.open_url {
                OpenUrl::Off => Decision::Deny(Deny::Denied),
                OpenUrl::Ask => Decision::Confirm,
                OpenUrl::On => Decision::Allow,
            },
            Op::ClipboardSet(_) => gate(perms.clipboard),
            Op::StatusGet => gate(perms.status),
            Op::AgentAnswer { .. } => gate(perms.agent_ask),
        },
        Inbound::BadCmd { .. } => Decision::Deny(Deny::Invalid),
        Inbound::Text { .. } => gate(perms.text_in),
        Inbound::Offer(_) => gate(perms.files_in),
        // Must refer to a transfer this session allowed; checked by the transfer code.
        Inbound::Accept(_) | Inbound::Done(_) | Inbound::Reject { .. } | Inbound::Saved(_) => Decision::Allow,
        Inbound::Rekeyed(_) | Inbound::Unpair | Inbound::Bye | Inbound::Ping => Decision::Allow,
    }
}

/// Outbound checks on the PC before anything is sent (design 6.4).
pub fn may_send_text(perms: &Perms) -> bool {
    perms.text_out
}
pub fn may_send_file(perms: &Perms) -> bool {
    perms.files_out
}
pub fn may_sync_clipboard(perms: &Perms) -> bool {
    perms.clipboard_out
}

/// Token bucket. `new` is the per-device command limit: 10 burst, 3/s sustained (design 6.5).
#[derive(Debug, Clone)]
pub struct RateLimit {
    tokens: f64,
    last_ms: i64,
    burst: f64,
    per_sec: f64,
}

impl RateLimit {
    pub fn new(now_ms: i64) -> Self {
        Self::with(10.0, 3.0, now_ms)
    }

    /// `url.open`: each one is a new browser tab, so 3 burst and one every 10 s after that.
    pub fn url(now_ms: i64) -> Self {
        Self::with(3.0, 0.1, now_ms)
    }

    pub fn with(burst: f64, per_sec: f64, now_ms: i64) -> Self {
        RateLimit { tokens: burst, last_ms: now_ms, burst, per_sec }
    }

    pub fn take(&mut self, now_ms: i64) -> bool {
        let dt = (now_ms - self.last_ms).max(0) as f64 / 1000.0;
        self.last_ms = now_ms.max(self.last_ms);
        self.tokens = (self.tokens + dt * self.per_sec).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Option<Inbound> {
        parse_inbound(v.to_string().as_bytes())
    }

    #[test]
    fn new_device_defaults_match_the_user_decisions() {
        let p = Perms::new_device();
        assert!(p.files_in && p.text_in && p.files_out && p.text_out && p.clipboard && p.clipboard_out);
        assert!(p.agent_ask && p.media_info && p.agent_status);
        assert!(p.media && p.volume && p.lock && p.status);
        assert_eq!(p.open_url, OpenUrl::On);
    }

    #[test]
    fn missing_perm_fields_default_to_off() {
        let p: Perms = serde_json::from_value(json!({"media": true})).unwrap();
        assert!(p.media && !p.agent_ask && !p.files_out && !p.clipboard_out);
        assert_eq!(p.open_url, OpenUrl::Off);
        let text = serde_json::to_value(Perms::new_device()).unwrap();
        assert_eq!(text["openUrl"], "on");
        assert_eq!(text["agentAsk"], true);
        assert_eq!(text["clipboardOut"], true);
    }

    #[test]
    fn parses_every_whitelisted_op() {
        let cases = [
            (json!({"t":"cmd","id":1,"op":"media.play_pause"}), Op::MediaPlayPause),
            (json!({"t":"cmd","id":1,"op":"media.next","a":{}}), Op::MediaNext),
            (json!({"t":"cmd","id":1,"op":"media.previous","a":null}), Op::MediaPrevious),
            (json!({"t":"cmd","id":1,"op":"volume.set","a":{"v":0.5}}), Op::VolumeSet(0.5)),
            (json!({"t":"cmd","id":1,"op":"volume.mute"}), Op::VolumeMute),
            (json!({"t":"cmd","id":1,"op":"volume.up"}), Op::VolumeUp),
            (json!({"t":"cmd","id":1,"op":"volume.down"}), Op::VolumeDown),
            (json!({"t":"cmd","id":1,"op":"pc.lock"}), Op::PcLock),
            (json!({"t":"cmd","id":1,"op":"url.open","a":{"u":"https://example.com/a?b=1"}}), Op::UrlOpen("https://example.com/a?b=1".into())),
            (json!({"t":"cmd","id":1,"op":"clipboard.set","a":{"text":"hi"}}), Op::ClipboardSet("hi".into())),
            (json!({"t":"cmd","id":1,"op":"status.get"}), Op::StatusGet),
            (json!({"t":"cmd","id":1,"op":"agent.answer","a":{"id":7,"key":"0a1b","allow":false}}), Op::AgentAnswer { id: 7, key: "0a1b".into(), allow: false }),
        ];
        for (msg, op) in cases {
            assert_eq!(parse(msg.clone()), Some(Inbound::Cmd { id: 1, op }), "{msg}");
        }
    }

    #[test]
    fn rejects_anything_outside_the_whitelist() {
        let bad = |v: Value| assert_eq!(parse(v.clone()), Some(Inbound::BadCmd { id: 9 }), "{v}");
        bad(json!({"t":"cmd","id":9,"op":"shell.run","a":{"cmd":"calc"}}));
        bad(json!({"t":"cmd","id":9,"op":"media.next","a":{"x":1}}));
        bad(json!({"t":"cmd","id":9,"op":"volume.set","a":{"v":1.5}}));
        bad(json!({"t":"cmd","id":9,"op":"volume.set","a":{"v":"0.5"}}));
        bad(json!({"t":"cmd","id":9,"op":"volume.set","a":{"v":0.5,"x":0}}));
        bad(json!({"t":"cmd","id":9,"op":"url.open","a":{"u":"file:///C:/Windows/system32/calc.exe"}}));
        bad(json!({"t":"cmd","id":9,"op":"url.open","a":{"u":"javascript:alert(1)"}}));
        bad(json!({"t":"cmd","id":9,"op":"url.open","a":{"u":"ms-settings:"}}));
        bad(json!({"t":"cmd","id":9,"op":"url.open","a":{"u":"https://user:pw@example.com"}}));
        bad(json!({"t":"cmd","id":9,"op":"url.open","a":{"u": format!("https://e.com/{}", "a".repeat(2100))}}));
        bad(json!({"t":"cmd","id":9,"op":"clipboard.set","a":{"text":""}}));
        bad(json!({"t":"cmd","id":9,"op":"clipboard.set","a":{"text":"a".repeat(MAX_TEXT + 1)}}));
        bad(json!({"t":"cmd","id":9,"op":"agent.answer","a":{"id":-1,"key":"00","allow":true}}));
        bad(json!({"t":"cmd","id":9,"op":"agent.answer","a":{"id":1,"allow":true}}));
        bad(json!({"t":"cmd","id":9,"op":"agent.answer","a":{"id":1,"key":"","allow":true}}));
        bad(json!({"t":"cmd","id":9,"op":"agent.answer","a":{"id":1,"key":"zz","allow":true}}));
        bad(json!({"t":"cmd","id":9}));
        assert_eq!(parse(json!({"t":"cmd","id":9,"op":"media.next","x":1})), None);
        assert_eq!(parse(json!({"t":"cmd","id":-1,"op":"media.next"})), None);
        assert_eq!(parse(json!({"t":"exec","cmd":"calc"})), None);
        assert_eq!(parse(json!({"t":"rekey","ep":1})), None, "rekey only goes PC to phone");
        assert_eq!(parse(json!([1, 2])), None);
        assert!(parse_inbound(b"not json").is_none());
    }

    #[test]
    fn parses_other_inner_types() {
        assert_eq!(parse(json!({"t":"text","text":"hey"})), Some(Inbound::Text { id: None, text: "hey".into() }));
        assert_eq!(parse(json!({"t":"text","id":4,"text":"hey"})), Some(Inbound::Text { id: Some(4), text: "hey".into() }));
        assert_eq!(parse(json!({"t":"text","text":""})), None);
        assert_eq!(parse(json!({"t":"rekeyed","ep":3})), Some(Inbound::Rekeyed(3)));
        assert_eq!(parse(json!({"t":"bye"})), Some(Inbound::Bye));
        assert_eq!(parse(json!({"t":"unpair"})), Some(Inbound::Unpair));
        assert_eq!(parse(json!({"t":"bye","x":1})), None);
        assert_eq!(parse(json!({"t":"ping"})), Some(Inbound::Ping));
        assert_eq!(parse(json!({"t":"ping","x":1})), None);
        let fid = "0123456789abcdef0123456789abcdef";
        assert_eq!(parse(json!({"t":"done","fid":fid})), Some(Inbound::Done(fid.into())));
        assert_eq!(parse(json!({"t":"saved","fid":fid})), Some(Inbound::Saved(fid.into())));
        assert_eq!(parse(json!({"t":"reject","fid":fid,"e":"storage"})), Some(Inbound::Reject { fid: fid.into(), e: "storage".into() }));
        assert_eq!(parse(json!({"t":"reject","fid":fid,"e":"has space"})), None);
        assert_eq!(parse(json!({"t":"reject","fid":fid,"e":"x","more":1})), None);
        assert_eq!(parse(json!({"t":"saved","fid":"NOTHEX"})), None);
        assert_eq!(parse(json!({"t":"accept","fid":"../x"})), None);
        let offer = json!({"t":"offer","fid":fid,"name":"a.jpg","size":10,"mime":"image/jpeg","sha256":"00","chunks":1,"key":"k"});
        assert!(matches!(parse(offer), Some(Inbound::Offer(_))));
        let extra = json!({"t":"offer","fid":fid,"name":"a.jpg","size":10,"mime":"image/jpeg","sha256":"00","chunks":1,"key":"k","path":"C:/"});
        assert_eq!(parse(extra), None);
    }

    #[test]
    fn authorize_checks_each_permission() {
        let none = Perms::default();
        let all = Perms::new_device();
        let ops = [
            Op::MediaPlayPause, Op::VolumeSet(0.1), Op::VolumeMute, Op::PcLock, Op::UrlOpen("https://a.b/".into()),
            Op::ClipboardSet("x".into()), Op::StatusGet, Op::AgentAnswer { id: 1, key: "00".into(), allow: true },
        ];
        for op in ops {
            let msg = Inbound::Cmd { id: 1, op: op.clone() };
            assert_eq!(authorize(&none, &msg), Decision::Deny(Deny::Denied), "{op:?}");
            assert_eq!(authorize(&all, &msg), Decision::Allow, "{op:?}");
        }
        assert_eq!(authorize(&none, &Inbound::Text { id: None, text: "x".into() }), Decision::Deny(Deny::Denied));
        assert_eq!(authorize(&all, &Inbound::Text { id: None, text: "x".into() }), Decision::Allow);
        assert_eq!(authorize(&none, &Inbound::BadCmd { id: 1 }), Decision::Deny(Deny::Invalid));
        assert_eq!(authorize(&all, &Inbound::BadCmd { id: 1 }), Decision::Deny(Deny::Invalid));
        assert_eq!(authorize(&none, &Inbound::Bye), Decision::Allow);
        let ask = Perms { open_url: OpenUrl::Ask, ..Perms::default() };
        assert_eq!(authorize(&ask, &Inbound::Cmd { id: 1, op: Op::UrlOpen("https://a.b/".into()) }), Decision::Confirm);
        let only_media = Perms { media: true, ..Perms::default() };
        assert_eq!(authorize(&only_media, &Inbound::Cmd { id: 1, op: Op::PcLock }), Decision::Deny(Deny::Denied));
        assert!(!may_send_text(&none) && may_send_text(&all) && may_send_file(&all));
    }

    #[test]
    fn rate_limit_burst_and_refill() {
        let mut r = RateLimit::new(0);
        for _ in 0..10 {
            assert!(r.take(0));
        }
        assert!(!r.take(0));
        assert!(r.take(334));
        assert!(!r.take(334));
        assert!(r.take(10_000));
    }

    #[test]
    fn check_url_rejects_non_web_and_malformed_input() {
        for bad in ["", " ", "example.com", "//example.com", "ftp://example.com", "data:text/html,hi", "http://", "https://u@example.com", "https://:pw@example.com", "mailto:a@b.c", r"\\server\share"] {
            assert!(check_url(bad).is_none(), "{bad:?}");
        }
        // WHATWG parsing treats extra slashes as part of the authority marker: this is host "path".
        assert_eq!(check_url("https:///path").unwrap(), "https://path/");
        assert!(check_url(&format!("https://e.com/{}", "a".repeat(MAX_URL))).is_none());
        assert_eq!(check_url("https://example.com/a b").unwrap(), "https://example.com/a%20b");
        assert!(check_url("https://example.com/x\ny").map_or(true, |u| !u.contains(char::is_control)));
    }

    #[test]
    fn authorize_denies_everything_gated_for_a_default_off_device() {
        let off = Perms::default();
        let ops = [
            Op::MediaPlayPause, Op::VolumeUp, Op::VolumeSet(0.1), Op::PcLock, Op::UrlOpen("https://e.com/".into()),
            Op::ClipboardSet("x".into()), Op::StatusGet, Op::AgentAnswer { id: 1, key: "00".into(), allow: true },
        ];
        for op in ops {
            let msg = Inbound::Cmd { id: 1, op };
            assert_eq!(authorize(&off, &msg), Decision::Deny(Deny::Denied), "{msg:?}");
        }
        assert_eq!(authorize(&off, &Inbound::Text { id: None, text: "x".into() }), Decision::Deny(Deny::Denied));
        assert_eq!(authorize(&off, &Inbound::BadCmd { id: 1 }), Decision::Deny(Deny::Invalid));
        let ask = Perms { open_url: OpenUrl::Ask, ..Perms::default() };
        assert_eq!(authorize(&ask, &Inbound::Cmd { id: 1, op: Op::UrlOpen("https://e.com/".into()) }), Decision::Confirm);
        assert!(!may_send_text(&off) && !may_send_file(&off) && !may_sync_clipboard(&off));
    }

    #[test]
    fn rate_limit_refills_and_survives_a_backwards_clock() {
        let mut r = RateLimit::new(1_000);
        for _ in 0..10 {
            assert!(r.take(1_000));
        }
        assert!(!r.take(1_000));
        assert!(!r.take(500), "an earlier timestamp must not refill the bucket");
        assert!(r.take(1_400), "0.4 s at 3/s refills one token");
        assert!(!r.take(1_400));
    }

    #[test]
    fn url_bucket_allows_three_then_one_every_ten_seconds() {
        let mut r = RateLimit::url(0);
        assert!(r.take(0) && r.take(0) && r.take(0));
        assert!(!r.take(5_000));
        assert!(r.take(10_000));
        assert!(!r.take(15_000));
    }

    #[test]
    fn url_helpers() {
        assert_eq!(url_host("https://Example.com:8080/x"), "example.com");
        assert_eq!(check_url("HTTP://example.com").unwrap(), "http://example.com/");
        assert!(check_url("https://").is_none());
    }
}
