// PC side of the phone link protocol, independent of the network (design sections 5, 6, 9).
// The relay client feeds it broadcasts and carries out the returned actions; side effects
// on the PC go through `Host`. Tests drive it with a simulated phone.

use super::audit::Entry;
use super::crypto::{self, b64u, unb64u, Key, KeyPair, DIR_C2P, DIR_P2C};
use super::device::{new_device_id, DeviceInfo, DeviceRecord, DeviceStore, HsCheck, KeyRing};
use super::pairing::{self, AbortReason, Accepted, CodeResult, HelloResult};
use super::remote::{self, authorize, Decision, Deny, Inbound, Op, Perms, RateLimit};
use super::transfer::{self, Offer, Plan, ReceiveJob, SendJob, Source, XferError, MAX_ACTIVE};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

pub const HS_WINDOW_SECS: i64 = 300;
const SID_MEMORY_SECS: i64 = 600;
/// The largest plaintext either side seals; the phone drops anything bigger without a word.
pub const MAX_PLAIN: usize = 64 * 1024;
const MAX_CIPHERTEXT: usize = MAX_PLAIN + crypto::NONCE_LEN + crypto::TAG_LEN;
const TOAST_EVERY_SECS: i64 = 600;
/// The PWA sends `ping` every 60 s while it is visible; a silent session ends after this.
pub const SESSION_IDLE_SECS: i64 = 150;
/// A transfer waiting on the other side (accept, upload, save) is dropped after this.
const XFER_WAIT_SECS: i64 = 2 * 3600;
/// Nothing in flight lives longer than this, whatever its state.
const XFER_MAX_SECS: i64 = 3 * 3600;
/// `pc.lock` runs at most once in this time per device.
const LOCK_COOLDOWN_SECS: i64 = 300;
/// After the PC is unlocked from a phone lock, no device may lock it again for this long, so
/// the user can always reach Settings > Phone > Remove.
const UNLOCK_GRACE_SECS: i64 = 60;
/// Repeated refusals of the same kind from one device take one audit line per window.
const REFUSAL_WINDOW_SECS: i64 = 60;
/// A session that starts this soon after a clipboard slot upload ended also gets that copy
/// live: the phone may have listed its mailbox just before the upload landed.
const CLIP_RESEND_SECS: i64 = 30;
/// A clipboard slot fid is replaced after this long. The relay sweeps an object 24 h after it
/// was created and an overwrite keeps that time, so the newest copy still gets 18 h.
const CLIP_FID_SECS: i64 = 6 * 3600;
/// Older slot fids remembered for deletion; beyond this the relay's 24 h sweep takes them.
const CLIP_STALE_KEEP: usize = 8;
/// A phone message this long after a live clipboard copy shows the phone was there to get it;
/// one sent sooner may have crossed the copy.
const CLIP_ACK_SECS: i64 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send { topic: String, event: &'static str, payload: Value },
    Join(String),
    Leave(String),
    /// `link_cancel_pairing` on the relay.
    CancelPairing,
    /// Delete the topic's blobs and call `link_remove_device`; persisted until it succeeds.
    RelayCleanup(String),
    /// Download a phone-to-PC transfer into the download folder; reported back with `transfer_finished`.
    Receive(ReceiveJob),
    /// Upload a PC-to-phone transfer (and its mailbox object); reported back with `transfer_finished`.
    Upload(SendJob),
    /// Delete a transfer's objects and release its grants.
    Discard { topic: String, dir: &'static str, fid: String, chunks: u32, mailbox: bool },
    /// List the topic's incoming mailbox and feed each object to `mailbox_item`.
    ScanMailbox(String),
}

pub trait Host {
    fn execute(&mut self, device: &DeviceInfo, op: &Op) -> Result<Value, Deny>;
    fn confirm_url(&mut self, device: &DeviceInfo, url: &str);
    fn status(&self, perms: &Perms) -> Value;
    fn text_received(&mut self, device: &DeviceInfo, text: &str);
    fn emit(&mut self, event: &str, payload: Value);
    fn audit(&mut self, entry: Entry);
    /// Size limit and free disk space for an incoming file; the download folder on success.
    fn inbound_check(&mut self, size: u64) -> Result<PathBuf, XferError>;
    fn max_out_bytes(&self) -> u64;
    /// Mailbox fids already handled (persisted 48 h, design 7.4).
    fn processed(&self, fid: &str) -> bool;
    fn mark_processed(&mut self, fid: &str, now: i64);
    /// The interactive desktop is the lock screen (or another secure desktop).
    fn pc_locked(&self) -> bool;
    /// Clipboard slot fids that may still hold a copy for the phone, kept across restarts.
    fn clip_fids(&self, device: &str) -> Vec<String>;
    fn set_clip_fids(&mut self, device: &str, fids: &[String]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XState {
    /// Phone to PC: accepted, the phone is uploading.
    AwaitDone,
    Receiving,
    /// PC to phone: offered, waiting for `accept`.
    AwaitAccept,
    Sending,
    /// PC to phone: uploaded and `done` sent, waiting for `saved`.
    Delivered,
}

struct Xfer {
    device_id: String,
    dir: &'static str,
    state: XState,
    /// `None` for a mailbox text.
    plan: Option<Plan>,
    source: Option<PathBuf>,
    download_dir: Option<PathBuf>,
    mailbox: bool,
    created: i64,
    text_len: usize,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TransferInfo {
    pub fid: String,
    pub device_id: String,
    pub dir: &'static str,
    pub name: String,
    pub size: u64,
    pub state: &'static str,
}

fn state_label(s: XState) -> &'static str {
    match s {
        XState::AwaitDone => "incoming",
        XState::Receiving => "receiving",
        XState::AwaitAccept => "offered",
        XState::Sending => "sending",
        XState::Delivered => "delivering",
    }
}

struct Session {
    sid: [u8; 16],
    ep: u32,
    keys: crypto::SessionKeys,
    send_n: u64,
    recv_n: u64,
    got_in: bool,
    sent_out: bool,
    rekey_sent: bool,
    started_at: i64,
    last_in: i64,
    last_status: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub device_id: String,
    pub name: String,
    pub ua: String,
    pub started_at: i64,
}

#[derive(Debug, PartialEq)]
pub enum CodeOutcome {
    Accepted,
    Wrong { left: u32 },
    Aborted(AbortReason),
    NoRequest,
}

pub struct PairingStart {
    pub pair_topic: String,
    pub url: String,
    pub expires_at: i64,
    pub replaced_topic: Option<String>,
}

/// Per-device limits: commands and handshakes share `cmd`; `url.open` and incoming texts and
/// offers have their own buckets on top.
struct Limits {
    cmd: RateLimit,
    url: RateLimit,
    xfer: RateLimit,
    last_lock: Option<i64>,
}

impl Limits {
    fn new(now_ms: i64) -> Self {
        Limits { cmd: RateLimit::new(now_ms), url: RateLimit::url(now_ms), xfer: RateLimit::new(now_ms), last_lock: None }
    }
}

/// The phone's mailbox slot for PC clipboard text (T-016): one fid per phone, overwritten in
/// place (the relay's upload URLs upsert), so only the newest copy waits. A new fid is used
/// once the phone may have read the old one, and the old object is deleted as soon as a newer
/// copy is stored, so copies hold one mailbox place.
#[derive(Default)]
struct ClipSlot {
    /// The fid copies go to; `None` gives the next copy a new one.
    fid: Option<[u8; 16]>,
    /// When `fid` was first written.
    since: i64,
    /// An upload of `fid` is running; a newer copy waits in `next`.
    busy: bool,
    next: Option<(String, i64)>,
    /// The phone may have read `fid` (a session started or the relay link was lost): it is
    /// given up once the running upload ends.
    retire: bool,
    /// The copy written or being written to `fid`, and when its upload ended.
    last: Option<(String, i64)>,
    stored: Option<i64>,
    /// Earlier fids that may still hold an older copy, deleted once a newer copy is stored.
    stale: Vec<[u8; 16]>,
    /// The newest copy sent over a live session (text, at, sent) that the phone has not
    /// answered since: it may have gone (closed, killed, socket lost) before the PC noticed.
    live: Option<(String, i64, i64)>,
    /// Copies may no longer go to the phone: the running upload's object is deleted when it ends.
    dropped: bool,
}

impl ClipSlot {
    fn give_up_fid(&mut self) {
        if let Some(f) = self.fid.take() {
            self.stale.push(f);
            let extra = self.stale.len().saturating_sub(CLIP_STALE_KEEP);
            self.stale.drain(..extra);
        }
        self.stored = None;
    }

    fn fids(&self) -> Vec<String> {
        self.stale.iter().chain(self.fid.as_ref()).map(|f| crypto::hex(f)).collect()
    }
}

/// An open coalescing window for one kind of refusal from one device.
struct Refusals {
    since: i64,
    repeated: u32,
    entry: Entry,
}

pub struct Engine {
    store: Box<dyn DeviceStore>,
    devices: Vec<DeviceRecord>,
    sessions: HashMap<String, Session>,
    /// Sessions ended by `link_lost`. The phone still holds them, so once its topic is joined
    /// again a `relink` sealed under the old keys makes it start a new session.
    lost: HashMap<String, Session>,
    seen_sids: VecDeque<([u8; 16], i64)>,
    limits: HashMap<String, Limits>,
    /// Set by a phone `pc.lock` until the PC is seen unlocked again.
    lock_watch: Option<i64>,
    unlocked_at: Option<i64>,
    refusals: HashMap<(String, String, String), Refusals>,
    toasts: HashMap<String, i64>,
    offer: Option<pairing::Offer>,
    accepted: Option<Accepted>,
    pc_name: String,
    xfers: HashMap<String, Xfer>,
    /// Topics whose mailbox held an item left for later because the device was at `MAX_ACTIVE`.
    rescan: HashSet<String>,
    clip_slots: HashMap<String, ClipSlot>,
    /// The `at` of the last clipboard copy (ms); the next one is always later.
    clip_at: i64,
}

fn secs(now_ms: i64) -> i64 {
    now_ms.div_euclid(1000)
}

fn key16(text: &str) -> Option<[u8; 16]> {
    unb64u(text)?.try_into().ok()
}

fn key32(text: &str) -> Option<[u8; 32]> {
    unb64u(text)?.try_into().ok()
}

impl Engine {
    pub fn new(store: Box<dyn DeviceStore>, pc_name: &str) -> Self {
        let devices = store.load_all();
        Engine {
            store,
            devices,
            sessions: HashMap::new(),
            lost: HashMap::new(),
            seen_sids: VecDeque::new(),
            limits: HashMap::new(),
            lock_watch: None,
            unlocked_at: None,
            refusals: HashMap::new(),
            toasts: HashMap::new(),
            offer: None,
            accepted: None,
            pc_name: pairing::clean_text(pc_name, 40),
            xfers: HashMap::new(),
            rescan: HashSet::new(),
            clip_slots: HashMap::new(),
            clip_at: 0,
        }
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        let mut list: Vec<DeviceInfo> = self.devices.iter().map(DeviceRecord::info).collect();
        list.sort_by_key(|d| d.paired_at);
        list
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .filter_map(|(id, s)| {
                let d = self.devices.iter().find(|d| &d.id == id)?;
                Some(SessionInfo { device_id: id.clone(), name: d.name.clone(), ua: d.ua.clone(), started_at: s.started_at })
            })
            .collect()
    }

    pub fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Device topics of devices that are not suspended.
    pub fn device_topics(&self) -> Vec<String> {
        self.devices.iter().filter(|d| !d.suspended).map(|d| d.topic.clone()).collect()
    }

    /// Topics the relay client keeps joined.
    pub fn topics(&self) -> Vec<String> {
        let mut t = self.device_topics();
        if let Some(o) = &self.offer {
            t.push(o.pair_topic.clone());
        }
        t
    }

    /// The open offer for a Settings window that was not showing it: QR URL, expiry, request.
    pub fn pairing_view(&self, pwa_url: &str, relay_ref: &str) -> Option<(String, i64, Option<pairing::PairRequest>)> {
        self.offer.as_ref().map(|o| (o.qr_url(pwa_url, relay_ref), o.expires_at, o.request()))
    }

    /// Re-reads the device records after another engine instance may have written them.
    pub fn reload_devices(&mut self) {
        self.devices = self.store.load_all();
    }

    fn persist(&mut self, id: &str) {
        if let Some(rec) = self.devices.iter().find(|d| d.id == id) {
            if let Err(e) = self.store.save(rec) {
                crate::diagnostics::log(&format!("phone link: saving device failed: {e}"));
            }
        }
    }

    fn device_index_by_topic(&self, topic: &str) -> Option<usize> {
        self.devices.iter().position(|d| d.topic == topic)
    }

    // Pairing.

    pub fn start_pairing(&mut self, pwa_url: &str, relay_ref: &str, now_ms: i64) -> PairingStart {
        let replaced_topic = self.offer.take().map(|o| o.pair_topic);
        self.accepted = None;
        let offer = pairing::Offer::new(&self.pc_name, secs(now_ms));
        let start = PairingStart {
            pair_topic: offer.pair_topic.clone(),
            url: offer.qr_url(pwa_url, relay_ref),
            expires_at: offer.expires_at,
            replaced_topic,
        };
        self.offer = Some(offer);
        start
    }

    fn abort_pairing(&mut self, host: &mut dyn Host, reason: AbortReason, now: i64) -> Vec<Action> {
        self.accepted = None;
        let Some(offer) = self.offer.take() else { return Vec::new() };
        host.emit("phone-link-pair-aborted", json!({ "reason": reason }));
        host.audit(Entry::new(now, "pair", "aborted").extra(json!({ "reason": reason })));
        vec![Action::Leave(offer.pair_topic), Action::CancelPairing]
    }

    pub fn cancel_pairing(&mut self, host: &mut dyn Host, now_ms: i64) -> Vec<Action> {
        self.abort_pairing(host, AbortReason::Cancelled, secs(now_ms))
    }

    pub fn submit_code(&mut self, host: &mut dyn Host, code: &str, now_ms: i64) -> (CodeOutcome, Vec<Action>) {
        let now = secs(now_ms);
        let Some(offer) = self.offer.as_mut() else { return (CodeOutcome::NoRequest, Vec::new()) };
        match offer.submit_code(code, now) {
            CodeResult::Ok => {
                let offer = self.offer.take().expect("offer present");
                self.accepted = offer.accept();
                (CodeOutcome::Accepted, Vec::new())
            }
            CodeResult::Wrong { left } => (CodeOutcome::Wrong { left }, Vec::new()),
            CodeResult::NoRequest => (CodeOutcome::NoRequest, Vec::new()),
            CodeResult::Abort(reason) => {
                let actions = self.abort_pairing(host, reason, now);
                (CodeOutcome::Aborted(reason), actions)
            }
        }
    }

    /// Arguments for `link_create_device` after a correct code.
    pub fn accepted_device(&self) -> Option<(String, String, String, Key)> {
        self.accepted.as_ref().map(|a| (a.pair_topic.clone(), a.device_topic.clone(), a.phone_uid.clone(), a.rejoin_key))
    }

    /// The relay refused `link_create_device`.
    pub fn fail_pairing(&mut self, host: &mut dyn Host, error: &str, now_ms: i64) -> Vec<Action> {
        let Some(acc) = self.accepted.take() else { return Vec::new() };
        host.emit("phone-link-pair-aborted", json!({ "reason": "relay", "detail": error }));
        host.audit(Entry::new(secs(now_ms), "pair", "failed").extra(json!({ "error": error })));
        vec![Action::Leave(acc.pair_topic), Action::CancelPairing]
    }

    /// After `link_create_device` succeeded: store the device and welcome the phone.
    pub fn complete_pairing(&mut self, host: &mut dyn Host, perms: Perms, now_ms: i64) -> Result<(DeviceInfo, Vec<Action>), String> {
        let acc = self.accepted.take().ok_or("no accepted pairing")?;
        let now = secs(now_ms);
        let id = new_device_id();
        let record = DeviceRecord {
            id: id.clone(),
            name: if acc.name.is_empty() { "Phone".into() } else { acc.name.clone() },
            ua: acc.ua.clone(),
            paired_at: now,
            last_seen: None,
            perms: perms.clone(),
            topic: acc.device_topic.clone(),
            rejoin_key: acc.rejoin_key,
            phone_uid: acc.phone_uid.clone(),
            keys: KeyRing::new(*acc.k_dev0),
            suspended: false,
        };
        self.store.save(&record)?;
        let info = record.info();
        self.devices.retain(|d| d.topic != record.topic);
        self.devices.push(record);
        host.audit(Entry::new(now, "pair", "ok").device(&info.id, &info.name).extra(json!({ "ua": info.ua, "perms": perms })));
        host.emit("phone-link-devices", json!(self.devices()));
        let actions = vec![
            Action::Send { topic: acc.pair_topic.clone(), event: "welcome", payload: pairing::welcome(&acc.k_dev0, &id, &self.pc_name, &perms) },
            Action::Leave(acc.pair_topic.clone()),
            Action::Join(acc.device_topic.clone()),
        ];
        Ok((info, actions))
    }

    // Devices.

    pub fn set_perms(&mut self, id: &str, perms: Perms, now_ms: i64, host: &mut dyn Host) -> Result<Vec<Action>, String> {
        let rec = self.devices.iter_mut().find(|d| d.id == id).ok_or("unknown device")?;
        rec.perms = perms.clone();
        let (name, topic) = (rec.name.clone(), rec.topic.clone());
        self.persist(id);
        host.audit(Entry::new(secs(now_ms), "perms", "ok").device(id, &name).extra(json!(perms)));
        let mut out = Vec::new();
        if self.sessions.contains_key(id) {
            out.extend(self.send_inner(id, &topic, json!({ "t": "evt", "op": "perms", "d": perms })));
        }
        if !remote::may_sync_clipboard(&perms) {
            out.extend(self.drop_clip_slot(host, id));
        }
        Ok(out)
    }

    pub fn end_session(&mut self, id: &str, host: &mut dyn Host) -> Vec<Action> {
        let Some(topic) = self.devices.iter().find(|d| d.id == id).map(|d| d.topic.clone()) else { return Vec::new() };
        let out = self.send_inner(id, &topic, json!({ "t": "bye" })).into_iter().collect();
        if self.sessions.remove(id).is_some() {
            host.emit("phone-link-session", json!({ "deviceId": id, "state": "end" }));
        }
        out
    }

    /// The relay socket is gone. Every session ends (the phone hears nothing more on it and
    /// starts a new one once the PC is back), so until then texts and files take the mailbox
    /// and finished transfers are not answered into a dead session.
    pub fn link_lost(&mut self, host: &mut dyn Host, now_ms: i64) -> Vec<Action> {
        for (id, s) in self.sessions.drain() {
            host.emit("phone-link-session", json!({ "deviceId": id, "state": "end", "reason": "offline" }));
            self.lost.insert(id, s);
        }
        // The phone may read the mailbox while this PC is away.
        let ids: Vec<String> = self.clip_slots.keys().cloned().collect();
        let mut out = Vec::new();
        for id in ids {
            self.retire_clip_slot(&id);
            out.extend(self.flush_live_clip(host, &id, secs(now_ms)));
        }
        out
    }

    /// The relay confirmed the join of `topic` after a lost socket. Without this the phone
    /// would keep pinging a session the PC no longer has and hear nothing until its next request.
    pub fn relinked(&mut self, topic: &str) -> Vec<Action> {
        let Some(d) = self.devices.iter().find(|d| d.topic == topic && !d.suspended) else { return Vec::new() };
        let Some(mut s) = self.lost.remove(&d.id) else { return Vec::new() };
        Self::seal_into(&mut s, topic, json!({ "t": "relink" })).into_iter().collect()
    }

    pub fn revoke(&mut self, host: &mut dyn Host, id: &str, reason: &str, now_ms: i64) -> Vec<Action> {
        let Some(pos) = self.devices.iter().position(|d| d.id == id) else { return Vec::new() };
        let mut out = self.end_session(id, host);
        let rec = self.devices.remove(pos);
        self.lost.remove(id);
        self.store.remove(id);
        self.limits.remove(id);
        // The relay cleanup removes every object of the topic.
        self.clip_slots.remove(id);
        host.set_clip_fids(id, &[]);
        self.xfers.retain(|_, x| x.device_id != id);
        host.audit(Entry::new(secs(now_ms), "revoke", reason).device(&rec.id, &rec.name));
        host.emit("phone-link-devices", json!(self.devices()));
        out.push(Action::Leave(rec.topic.clone()));
        out.push(Action::RelayCleanup(rec.topic.clone()));
        out
    }

    pub fn revoke_all(&mut self, host: &mut dyn Host, now_ms: i64) -> Vec<Action> {
        let ids: Vec<String> = self.devices.iter().map(|d| d.id.clone()).collect();
        ids.iter().flat_map(|id| self.revoke(host, id, "ok", now_ms)).collect()
    }

    // Sending.

    fn send_inner(&mut self, id: &str, topic: &str, inner: Value) -> Option<Action> {
        Self::seal_into(self.sessions.get_mut(id)?, topic, inner)
    }

    fn seal_into(s: &mut Session, topic: &str, inner: Value) -> Option<Action> {
        let plain = inner.to_string();
        if plain.len() > MAX_PLAIN {
            return None;
        }
        s.send_n += 1;
        let aad = crypto::msg_aad(topic, &s.sid, DIR_C2P, s.send_n);
        let c = crypto::seal(&s.keys.c2p, &aad, plain.as_bytes());
        s.sent_out = true;
        Some(Action::Send { topic: topic.to_string(), event: "m", payload: json!({ "sid": b64u(&s.sid), "n": s.send_n, "c": b64u(&c) }) })
    }

    /// PC to phone text (permission `textOut`). Without a live session it goes to the
    /// phone's mailbox (an `Upload` action in the result).
    pub fn send_text(&mut self, id: &str, text: &str, host: &mut dyn Host, now_ms: i64) -> Result<Vec<Action>, String> {
        let now = secs(now_ms);
        let rec = self.devices.iter().find(|d| d.id == id).ok_or("unknown device")?;
        if rec.suspended || !remote::may_send_text(&rec.perms) {
            return Err("denied".into());
        }
        if text.is_empty() || text.len() > remote::MAX_TEXT {
            return Err("invalid".into());
        }
        let (topic, name) = (rec.topic.clone(), rec.name.clone());
        let inner = json!({ "t": "text", "text": text });
        if self.sessions.contains_key(id) {
            let action = self.send_inner(id, &topic, inner).ok_or("offline")?;
            host.audit(Entry::new(now, "text", "ok").device(id, &name).dir("c2p").extra(json!({ "len": text.len() })));
            let mut out = vec![action];
            out.extend(self.maybe_rekey(id, &topic));
            return Ok(out);
        }
        if self.active(id) >= MAX_ACTIVE {
            return Err("busy".into());
        }
        let fid = crypto::random_bytes::<16>();
        let object = transfer::seal_mailbox(&rec.keys.current.key, rec.keys.current.ep, &topic, "c2p", &fid, now as u64, &inner);
        let fid = crypto::hex(&fid);
        self.xfers.insert(
            fid.clone(),
            Xfer { device_id: id.into(), dir: "c2p", state: XState::Sending, plan: None, source: None, download_dir: None, mailbox: true, created: now, text_len: text.len() },
        );
        Ok(vec![Action::Upload(SendJob { device_id: id.into(), topic, fid, plan: None, source: Source::None, mailbox: Some(object) })])
    }

    /// PC clipboard text (T-016) to every phone with `clipboardOut`: over a live session, or
    /// into the phone's single clipboard mailbox slot, replacing what waits there.
    pub fn send_clip(&mut self, text: &str, host: &mut dyn Host, now_ms: i64) -> Vec<Action> {
        // Milliseconds that never go back, so the phone can keep the newest copy by `at`.
        let at = now_ms.max(self.clip_at + 1);
        if !Self::clip_fits(text) {
            return Vec::new();
        }
        self.clip_at = at;
        let ids: Vec<String> = self.devices.iter().filter(|d| !d.suspended && remote::may_sync_clipboard(&d.perms)).map(|d| d.id.clone()).collect();
        ids.iter().flat_map(|id| self.deliver_clip(host, id, text, at, secs(now_ms), false)).collect()
    }

    /// `late`: a copy made before the session it goes over; the phone offers it and does not
    /// write it, since its clipboard may hold something newer by then.
    fn clip_inner(text: &str, at: i64, late: bool) -> Value {
        let mut v = json!({ "t": "clip", "text": text, "at": at });
        if late {
            v["late"] = json!(true);
        }
        v
    }

    /// Whether clipboard text fits one sealed message in every form it is sent in.
    pub fn clip_fits(text: &str) -> bool {
        !text.is_empty() && text.len() <= remote::MAX_TEXT && Self::clip_inner(text, i64::MAX, true).to_string().len() <= MAX_PLAIN
    }

    fn deliver_clip(&mut self, host: &mut dyn Host, id: &str, text: &str, at: i64, now: i64, late: bool) -> Vec<Action> {
        let Some(rec) = self.devices.iter().find(|d| d.id == id && !d.suspended && remote::may_sync_clipboard(&d.perms)) else { return Vec::new() };
        let (topic, name, key, ep) = (rec.topic.clone(), rec.name.clone(), rec.keys.current.key, rec.keys.current.ep);
        if self.sessions.contains_key(id) {
            let Some(action) = self.send_inner(id, &topic, Self::clip_inner(text, at, late)) else { return Vec::new() };
            Self::clip_slot(&mut self.clip_slots, host, id).live = Some((text.to_string(), at, now));
            host.audit(Entry::new(now, "clip", "ok").device(id, &name).dir("c2p").extra(json!({ "len": text.len() })));
            let mut out = vec![action];
            out.extend(self.maybe_rekey(id, &topic));
            return out;
        }
        let slot = Self::clip_slot(&mut self.clip_slots, host, id);
        slot.live = None;
        if slot.busy {
            slot.next = Some((text.to_string(), at));
            return Vec::new();
        }
        if slot.fid.is_some() && now - slot.since >= CLIP_FID_SECS {
            slot.give_up_fid();
        }
        let fid = match slot.fid {
            Some(f) => f,
            None => {
                let f = crypto::random_bytes::<16>();
                slot.fid = Some(f);
                slot.since = now;
                host.set_clip_fids(id, &slot.fids());
                f
            }
        };
        slot.busy = true;
        slot.last = Some((text.to_string(), at));
        let object = transfer::seal_mailbox(&key, ep, &topic, "c2p", &fid, secs(at) as u64, &Self::clip_inner(text, at, false));
        vec![Action::Upload(SendJob { device_id: id.into(), topic, fid: crypto::hex(&fid), plan: None, source: Source::None, mailbox: Some(object) })]
    }

    fn clip_slot<'a>(slots: &'a mut HashMap<String, ClipSlot>, host: &dyn Host, id: &str) -> &'a mut ClipSlot {
        slots.entry(id.to_string()).or_insert_with(|| ClipSlot {
            // Slots of an earlier run may still hold a copy.
            stale: host.clip_fids(id).iter().filter_map(|f| crypto::unhex(f)?.try_into().ok()).collect(),
            ..ClipSlot::default()
        })
    }

    /// The session ended with no phone message since the last live copy: the copy may never
    /// have arrived, so it goes to the mailbox. Call it once the session is gone.
    fn flush_live_clip(&mut self, host: &mut dyn Host, id: &str, now: i64) -> Vec<Action> {
        let Some((text, at, _)) = self.clip_slots.get_mut(id).and_then(|s| s.live.take()) else { return Vec::new() };
        self.deliver_clip(host, id, &text, at, now, false)
    }

    /// A phone message: a live copy sent long enough before it has arrived.
    fn clip_answered(&mut self, id: &str, now: i64) {
        if let Some(slot) = self.clip_slots.get_mut(id) {
            if slot.live.as_ref().is_some_and(|(_, _, sent)| now - sent >= CLIP_ACK_SECS) {
                slot.live = None;
            }
        }
    }

    /// Clipboard copies may no longer go to this phone (permission or setting withdrawn): every
    /// copy waiting for it in the mailbox is deleted, so it cannot read text copied before.
    fn drop_clip_slot(&mut self, host: &mut dyn Host, id: &str) -> Vec<Action> {
        let Some(topic) = self.devices.iter().find(|d| d.id == id).map(|d| d.topic.clone()) else { return Vec::new() };
        let fids = match self.clip_slots.get_mut(id) {
            Some(slot) if slot.busy => {
                // The running upload may land after a delete; `clip_finished` deletes it all.
                slot.dropped = true;
                slot.next = None;
                slot.live = None;
                return Vec::new();
            }
            Some(_) => self.clip_slots.remove(id).map(|s| s.fids()).unwrap_or_default(),
            None => host.clip_fids(id),
        };
        if fids.is_empty() {
            return Vec::new();
        }
        host.set_clip_fids(id, &[]);
        fids.into_iter().map(|fid| Action::Discard { topic: topic.clone(), dir: "c2p", fid, chunks: 0, mailbox: true }).collect()
    }

    /// `nectar-phone-clipboard-sync` is off: nothing copied may wait for any phone.
    pub fn drop_clips(&mut self, host: &mut dyn Host) -> Vec<Action> {
        let ids: Vec<String> = self.devices.iter().map(|d| d.id.clone()).collect();
        ids.iter().flat_map(|id| self.drop_clip_slot(host, id)).collect()
    }

    /// The phone may have read the slot: the next offline copy gets a new fid.
    fn retire_clip_slot(&mut self, id: &str) {
        match self.clip_slots.get_mut(id) {
            Some(slot) if slot.busy => slot.retire = true,
            Some(slot) => slot.give_up_fid(),
            None => {}
        }
    }

    /// A session started, so the phone reads its mailbox now. A copy whose upload is running
    /// or just ended may land after the phone listed it and would then be deleted unread (its
    /// fid is processed), so the newest such copy goes over the new session as well.
    fn clip_session_started(&mut self, id: &str, now: i64) -> Option<(String, i64)> {
        let slot = self.clip_slots.get_mut(id)?;
        let recent = slot.busy || slot.stored.is_some_and(|t| now - t <= CLIP_RESEND_SECS);
        // A copy whose permission was withdrawn during its upload is not sent again.
        let queued = slot.next.take().or_else(|| slot.last.clone().filter(|_| !slot.dropped)).filter(|_| recent);
        // A live copy the phone never answered: it may not have arrived on the old session.
        let live = slot.live.take().map(|(text, at, _)| (text, at));
        self.retire_clip_slot(id);
        queued.into_iter().chain(live).max_by_key(|(_, at)| *at)
    }

    /// The end of a clipboard slot upload, or `None` when `fid` is not one.
    fn clip_finished(&mut self, host: &mut dyn Host, fid: &str, result: &Result<Option<PathBuf>, XferError>, now: i64) -> Option<Vec<Action>> {
        let id = self.clip_slots.iter().find(|(_, s)| s.busy && s.fid.is_some_and(|f| crypto::hex(&f) == fid)).map(|(id, _)| id.clone())?;
        let (topic, name) = self.devices.iter().find(|d| d.id == id).map(|d| (d.topic.clone(), d.name.clone()))?;
        let slot = self.clip_slots.get_mut(&id)?;
        slot.busy = false;
        let next = slot.next.take();
        let len = slot.last.as_ref().map_or(0, |(t, _)| t.len());
        let mut out = Vec::new();
        if slot.dropped {
            // Withdrawn during the upload: nothing of this slot may stay readable.
            let fids = slot.fids();
            self.clip_slots.remove(&id);
            host.set_clip_fids(&id, &[]);
            out.extend(fids.into_iter().map(|fid| Action::Discard { topic: topic.clone(), dir: "c2p", fid, chunks: 0, mailbox: true }));
            host.audit(Entry::new(now, "clip", "dropped").device(&id, &name).dir("c2p").extra(json!({ "len": len, "mailbox": true })));
            if let Some((text, at)) = next {
                out.extend(self.deliver_clip(host, &id, &text, at, now, false));
            }
            return Some(out);
        }
        if result.is_ok() {
            slot.stored = Some(now);
            // The newest copy is stored: older objects for this phone go.
            out.extend(slot.stale.drain(..).map(|f| Action::Discard { topic: topic.clone(), dir: "c2p", fid: crypto::hex(&f), chunks: 0, mailbox: true }));
            if slot.retire {
                slot.give_up_fid();
            }
        } else {
            // The object may be missing, older, or still on its way: the next copy takes a new
            // fid, and this one is deleted once that copy is stored.
            slot.give_up_fid();
        }
        slot.retire = false;
        if !out.is_empty() {
            host.set_clip_fids(&id, &slot.fids());
        }
        let label = match result {
            Ok(_) => "queued",
            Err(e) => e.code(),
        };
        host.audit(Entry::new(now, "clip", label).device(&id, &name).dir("c2p").extra(json!({ "len": len, "mailbox": true })));
        if let Some((text, at)) = next {
            out.extend(self.deliver_clip(host, &id, &text, at, now, false));
        }
        Some(out)
    }

    // Transfers (design section 7).

    fn active(&self, device: &str) -> usize {
        self.xfers.values().filter(|x| x.device_id == device).count()
    }

    /// Scans for mailboxes that had to wait on a full device and now have room.
    fn rescans(&mut self) -> Vec<Action> {
        let ready: Vec<String> = self
            .rescan
            .iter()
            .filter(|t| self.devices.iter().find(|d| &d.topic == *t).is_none_or(|d| self.active(&d.id) < MAX_ACTIVE))
            .cloned()
            .collect();
        ready
            .into_iter()
            .filter_map(|t| {
                self.rescan.remove(&t);
                self.device_index_by_topic(&t).map(|_| Action::ScanMailbox(t))
            })
            .collect()
    }

    fn emit_xfer(host: &mut dyn Host, fid: &str, x: &Xfer, state: &str, e: Option<&str>) {
        let (name, size) = x.plan.as_ref().map(|p| (p.name.clone(), p.size)).unwrap_or_default();
        host.emit("phone-link-transfer", json!({ "fid": fid, "deviceId": x.device_id, "dir": x.dir, "name": name, "size": size, "done": 0, "state": state, "e": e }));
    }

    fn file_audit(plan: &Plan, mailbox: bool) -> Value {
        json!({ "name": plan.name, "size": plan.size, "sha256": crypto::hex(&plan.sha256), "mailbox": mailbox })
    }

    pub fn transfers(&self) -> Vec<TransferInfo> {
        let mut list: Vec<(i64, TransferInfo)> = self
            .xfers
            .iter()
            .map(|(fid, x)| {
                let (name, size) = x.plan.as_ref().map(|p| (p.name.clone(), p.size)).unwrap_or_default();
                (x.created, TransferInfo { fid: fid.clone(), device_id: x.device_id.clone(), dir: x.dir, name, size, state: state_label(x.state) })
            })
            .collect();
        list.sort_by_key(|(c, _)| *c);
        list.into_iter().map(|(_, t)| t).collect()
    }

    /// Mailbox key lookup for a phone-to-PC object: the epochs the phone may hold now.
    fn mailbox_key(&self, idx: usize, ep: u32) -> Option<crypto::Key> {
        let k = &self.devices[idx].keys;
        [Some(&k.current), k.proposed.as_ref(), k.pending_prev.as_ref()].into_iter().flatten().find(|e| e.ep == ep).map(|e| e.key)
    }

    /// Checks shared by live and mailbox offers (permission, shape, load, size and disk).
    fn check_offer(&self, host: &mut dyn Host, id: &str, offer: &Offer, decision: &Decision) -> Result<(Plan, PathBuf), XferError> {
        if *decision != Decision::Allow {
            return Err(XferError::Denied);
        }
        let plan = offer.plan().ok_or(XferError::Invalid)?;
        if self.xfers.contains_key(&plan.fid_hex()) {
            return Err(XferError::Invalid);
        }
        if self.active(id) >= MAX_ACTIVE {
            return Err(XferError::Busy);
        }
        let dir = host.inbound_check(plan.size)?;
        Ok((plan, dir))
    }

    /// A live offer also draws from the device's transfer bucket, so junk offers cannot flood
    /// the audit log; mailbox offers are capped by the relay (5 per direction).
    fn check_live_offer(&mut self, host: &mut dyn Host, id: &str, offer: &Offer, decision: &Decision, now_ms: i64) -> Result<(Plan, PathBuf), XferError> {
        if *decision == Decision::Allow && !self.limits(id, now_ms).xfer.take(now_ms) {
            return Err(XferError::Busy);
        }
        self.check_offer(host, id, offer, decision)
    }

    /// PC to phone file (permission `filesOut`): an offer over the live session, or the
    /// chunks plus a mailbox offer when the phone is not connected. Returns the fid.
    #[allow(clippy::too_many_arguments)]
    pub fn send_file(&mut self, host: &mut dyn Host, id: &str, path: PathBuf, name: &str, size: u64, sha256: [u8; 32], now_ms: i64) -> Result<(String, Vec<Action>), String> {
        let now = secs(now_ms);
        let rec = self.devices.iter().find(|d| d.id == id).ok_or("unknown device")?;
        if rec.suspended || !remote::may_send_file(&rec.perms) {
            return Err("denied".into());
        }
        if size > host.max_out_bytes() {
            return Err("tooBig".into());
        }
        if self.active(id) >= MAX_ACTIVE {
            return Err("busy".into());
        }
        let plan = Plan::new_outbound(name, size, transfer::guess_mime(name), sha256).ok_or("tooBig")?;
        let (topic, dev_name) = (rec.topic.clone(), rec.name.clone());
        let fid = plan.fid_hex();
        let offer = serde_json::to_value(plan.offer()).map_err(|e| e.to_string())?;
        let live = self.sessions.contains_key(id);
        let mut out = Vec::new();
        let x = if live {
            out.extend(self.send_inner(id, &topic, offer));
            out.extend(self.maybe_rekey(id, &topic));
            Xfer { device_id: id.into(), dir: "c2p", state: XState::AwaitAccept, plan: Some(plan.clone()), source: Some(path), download_dir: None, mailbox: false, created: now, text_len: 0 }
        } else {
            let rec = self.devices.iter().find(|d| d.id == id).ok_or("unknown device")?;
            let object = transfer::seal_mailbox(&rec.keys.current.key, rec.keys.current.ep, &topic, "c2p", &plan.fid, now as u64, &offer);
            out.push(Action::Upload(SendJob { device_id: id.into(), topic, fid: fid.clone(), plan: Some(plan.clone()), source: Source::File(path.clone()), mailbox: Some(object) }));
            Xfer { device_id: id.into(), dir: "c2p", state: XState::Sending, plan: Some(plan.clone()), source: Some(path), download_dir: None, mailbox: true, created: now, text_len: 0 }
        };
        host.audit(Entry::new(now, "offer", "ok").device(id, &dev_name).dir("c2p").extra(Self::file_audit(&plan, !live)));
        Self::emit_xfer(host, &fid, &x, state_label(x.state), None);
        self.xfers.insert(fid.clone(), x);
        Ok((fid, out))
    }

    /// The result of a `Receive` or `Upload` action.
    pub fn transfer_finished(&mut self, host: &mut dyn Host, fid: &str, result: Result<Option<PathBuf>, XferError>, now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        if let Some(out) = self.clip_finished(host, fid, &result, now) {
            return out;
        }
        let Some(x) = self.xfers.get(fid) else { return Vec::new() };
        if !matches!(x.state, XState::Receiving | XState::Sending) {
            return Vec::new();
        }
        let (id, dir, mailbox) = (x.device_id.clone(), x.dir, x.mailbox);
        let Some(rec) = self.devices.iter().find(|d| d.id == id) else {
            self.xfers.remove(fid);
            return Vec::new();
        };
        let (topic, name) = (rec.topic.clone(), rec.name.clone());
        let mut out = Vec::new();
        match (dir, result) {
            ("p2c", Ok(path)) => {
                let x = self.xfers.remove(fid).expect("xfer");
                let plan = x.plan.as_ref().expect("p2c plan");
                if x.mailbox {
                    host.mark_processed(fid, now);
                }
                host.audit(Entry::new(now, "file", "ok").device(&id, &name).dir("p2c").extra(Self::file_audit(plan, x.mailbox)));
                let paths: Vec<String> = path.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                host.emit("phone-link-received", json!({ "deviceId": id, "name": name, "paths": paths }));
                Self::emit_xfer(host, fid, &x, "saved", None);
                if !x.mailbox {
                    out.extend(self.send_inner(&id, &topic, json!({ "t": "saved", "fid": fid })));
                }
            }
            ("p2c", Err(e)) => {
                let x = self.xfers.remove(fid).expect("xfer");
                let plan = x.plan.as_ref().expect("p2c plan");
                host.audit(Entry::new(now, "file", e.code()).device(&id, &name).dir("p2c").extra(Self::file_audit(plan, x.mailbox)));
                Self::emit_xfer(host, fid, &x, "failed", Some(e.code()));
                // A mailbox item that failed for a passing reason stays for the next scan.
                if !(x.mailbox && e.transient()) {
                    if x.mailbox {
                        host.mark_processed(fid, now);
                    }
                    out.push(Action::Discard { topic: topic.clone(), dir: "p2c", fid: fid.into(), chunks: plan.chunks, mailbox: x.mailbox });
                }
                if !x.mailbox {
                    out.extend(self.send_inner(&id, &topic, json!({ "t": "reject", "fid": fid, "e": e.code() })));
                }
            }
            (_, Ok(_)) if mailbox => {
                let x = self.xfers.remove(fid).expect("xfer");
                match &x.plan {
                    Some(plan) => host.audit(Entry::new(now, "file", "queued").device(&id, &name).dir("c2p").extra(Self::file_audit(plan, true))),
                    None => host.audit(Entry::new(now, "text", "queued").device(&id, &name).dir("c2p").extra(json!({ "len": x.text_len, "mailbox": true }))),
                }
                Self::emit_xfer(host, fid, &x, "queued", None);
            }
            (_, Ok(_)) => {
                let done = self.send_inner(&id, &topic, json!({ "t": "done", "fid": fid }));
                let x = self.xfers.get_mut(fid).expect("xfer");
                match (done, x.plan.clone()) {
                    (Some(done), _) => {
                        x.state = XState::Delivered;
                        Self::emit_xfer(host, fid, self.xfers.get(fid).expect("xfer"), "delivering", None);
                        out.push(done);
                    }
                    // The session ended during the upload: the chunks are there, so a mailbox
                    // offer is all the phone needs to fetch them later.
                    (None, Some(plan)) => {
                        let rec = self.devices.iter().find(|d| d.id == id).expect("device");
                        let offer = serde_json::to_value(plan.offer()).unwrap_or_default();
                        let object = transfer::seal_mailbox(&rec.keys.current.key, rec.keys.current.ep, &topic, "c2p", &plan.fid, now as u64, &offer);
                        x.mailbox = true;
                        out.push(Action::Upload(SendJob { device_id: id.clone(), topic: topic.clone(), fid: fid.into(), plan: Some(plan), source: Source::None, mailbox: Some(object) }));
                    }
                    (None, None) => {
                        self.xfers.remove(fid);
                    }
                }
            }
            (_, Err(e)) => {
                let x = self.xfers.remove(fid).expect("xfer");
                let extra = match &x.plan {
                    Some(plan) => Self::file_audit(plan, x.mailbox),
                    None => json!({ "len": x.text_len, "mailbox": true }),
                };
                host.audit(Entry::new(now, if x.plan.is_some() { "file" } else { "text" }, e.code()).device(&id, &name).dir("c2p").extra(extra));
                Self::emit_xfer(host, fid, &x, "failed", Some(e.code()));
                let chunks = x.plan.as_ref().map_or(0, |p| p.chunks);
                out.push(Action::Discard { topic: topic.clone(), dir: "c2p", fid: fid.into(), chunks, mailbox: x.mailbox });
                if !x.mailbox {
                    out.extend(self.send_inner(&id, &topic, json!({ "t": "reject", "fid": fid, "e": e.code() })));
                }
            }
        }
        out.extend(self.rescans());
        out
    }

    /// One object from a device's incoming mailbox (`<topic>/mbox/p2c/<fid>`), design 7.4.
    pub fn mailbox_item(&mut self, host: &mut dyn Host, topic: &str, fid: &str, bytes: &[u8], now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        let Some(idx) = self.device_index_by_topic(topic) else { return Vec::new() };
        if self.devices[idx].suspended || !transfer::is_fid(fid) || self.xfers.contains_key(fid) {
            return Vec::new();
        }
        let discard = |chunks: u32| vec![Action::Discard { topic: topic.into(), dir: "p2c", fid: fid.into(), chunks, mailbox: true }];
        if host.processed(fid) {
            return discard(0);
        }
        let Some(fid_b) = crypto::unhex(fid).and_then(|v| <[u8; 16]>::try_from(v).ok()) else { return Vec::new() };
        let plain = transfer::open_mailbox(|ep| self.mailbox_key(idx, ep), topic, "p2c", &fid_b, bytes, now);
        let (id, name) = (self.devices[idx].id.clone(), self.devices[idx].name.clone());
        let info = self.devices[idx].info();
        let msg = plain.as_deref().and_then(remote::parse_inbound);
        let decision = msg.as_ref().map(|m| authorize(&self.devices[idx].perms, m));
        match (msg, decision) {
            (Some(Inbound::Text { text, .. }), Some(d)) => {
                let ok = d == Decision::Allow;
                if ok {
                    host.text_received(&info, &text);
                }
                let entry = Entry::new(now, "text", if ok { "ok" } else { "denied" }).device(&id, &name).dir("p2c").extra(json!({ "len": text.len(), "mailbox": true }));
                if ok {
                    host.audit(entry);
                } else {
                    self.refused(host, entry);
                }
                host.mark_processed(fid, now);
                discard(0)
            }
            (Some(Inbound::Offer(offer)), Some(d)) => match self.check_offer(host, &id, &offer, &d) {
                Ok((plan, dir)) if plan.fid_hex() == fid => {
                    let job = ReceiveJob { device_id: id.clone(), topic: topic.into(), plan: plan.clone(), download_dir: dir.clone(), mailbox: true };
                    let x = Xfer { device_id: id, dir: "p2c", state: XState::Receiving, plan: Some(plan), source: None, download_dir: Some(dir), mailbox: true, created: now, text_len: 0 };
                    Self::emit_xfer(host, fid, &x, "receiving", None);
                    self.xfers.insert(fid.into(), x);
                    vec![Action::Receive(job)]
                }
                // Full (or low on disk) right now: the item stays for a later scan, which for a
                // full device comes as soon as one of its transfers ends.
                Err(e) if e == XferError::Busy || e.transient() => {
                    if e == XferError::Busy {
                        self.rescan.insert(topic.to_string());
                    }
                    Vec::new()
                }
                other => {
                    let e = other.err().unwrap_or(XferError::Invalid);
                    self.refused(host, Entry::new(now, "offer", e.code()).device(&id, &name).dir("p2c").extra(json!({ "name": transfer::sanitize_name(&offer.name), "size": offer.size, "mailbox": true })));
                    host.mark_processed(fid, now);
                    discard(offer.chunks.min(transfer::MAX_CHUNKS))
                }
            },
            _ => {
                host.mark_processed(fid, now);
                discard(0)
            }
        }
    }

    fn limits(&mut self, id: &str, now_ms: i64) -> &mut Limits {
        self.limits.entry(id.to_string()).or_insert_with(|| Limits::new(now_ms))
    }

    /// Writes a refusal, or counts it when the same device had the same refusal for the same
    /// operation within `REFUSAL_WINDOW_SECS`; the count goes out as one line when the window ends.
    fn refused(&mut self, host: &mut dyn Host, entry: Entry) {
        let key = (entry.device.clone().unwrap_or_default(), entry.op.clone(), entry.result.clone());
        if let Some(r) = self.refusals.get_mut(&key) {
            if (0..REFUSAL_WINDOW_SECS).contains(&(entry.ts - r.since)) {
                r.repeated += 1;
                return;
            }
        }
        if let Some(r) = self.refusals.remove(&key) {
            Self::flush_refusals(host, r, entry.ts);
        }
        host.audit(entry.clone());
        self.refusals.insert(key, Refusals { since: entry.ts, repeated: 0, entry });
    }

    fn flush_refusals(host: &mut dyn Host, r: Refusals, now: i64) {
        if r.repeated == 0 {
            return;
        }
        let mut line = Entry::new(now, &r.entry.op, &r.entry.result).extra(json!({ "repeated": r.repeated, "since": r.since }));
        line.device = r.entry.device;
        line.name = r.entry.name;
        line.dir = r.entry.dir;
        host.audit(line);
    }

    /// Notes the end of a phone lock; checked every tick and before each `pc.lock`.
    fn watch_unlock(&mut self, host: &dyn Host, now: i64) {
        // LockWorkStation switches desktops asynchronously; give it a moment before reading.
        if self.lock_watch.is_some_and(|t| now - t >= 2) && !host.pc_locked() {
            self.lock_watch = None;
            self.unlocked_at = Some(now);
        }
    }

    /// Per-operation limits on top of the command bucket. `pc.lock`: once per
    /// `LOCK_COOLDOWN_SECS` per device, never while locked or in the grace after an unlock.
    fn op_allowed(&mut self, host: &dyn Host, id: &str, op: &Op, now_ms: i64) -> bool {
        let now = secs(now_ms);
        match op {
            Op::PcLock => {
                self.watch_unlock(host, now);
                let grace = self.unlocked_at.is_some_and(|t| (0..UNLOCK_GRACE_SECS).contains(&(now - t)));
                let cooling = self.limits(id, now_ms).last_lock.is_some_and(|t| (0..LOCK_COOLDOWN_SECS).contains(&(now - t)));
                !(grace || cooling || self.lock_watch.is_some() || host.pc_locked())
            }
            Op::UrlOpen(_) => self.limits(id, now_ms).url.take(now_ms),
            _ => true,
        }
    }

    pub fn push_status(&mut self, host: &mut dyn Host) -> Vec<Action> {
        let targets: Vec<(String, String, Perms)> = self
            .devices
            .iter()
            .filter(|d| d.perms.status && self.sessions.contains_key(&d.id))
            .map(|d| (d.id.clone(), d.topic.clone(), d.perms.clone()))
            .collect();
        let mut out = Vec::new();
        for (id, topic, perms) in targets {
            out.extend(self.status_evt(host, &id, &topic, &perms));
            out.extend(self.maybe_rekey(&id, &topic));
        }
        out
    }

    /// Status event for one session, only when it differs from the last one that session got.
    fn status_evt(&mut self, host: &mut dyn Host, id: &str, topic: &str, perms: &Perms) -> Option<Action> {
        let text = host.status(perms).to_string();
        let s = self.sessions.get_mut(id)?;
        if s.last_status.as_deref() == Some(text.as_str()) {
            return None;
        }
        let status: Value = serde_json::from_str(&text).ok()?;
        s.last_status = Some(text);
        self.send_inner(id, topic, json!({ "t": "evt", "op": "status", "d": status }))
    }

    fn maybe_rekey(&mut self, id: &str, topic: &str) -> Vec<Action> {
        let Some(s) = self.sessions.get(id) else { return Vec::new() };
        if !(s.got_in && s.sent_out) || s.rekey_sent {
            return Vec::new();
        }
        let (ep, rk) = (s.ep, *s.keys.rk);
        let Some(rec) = self.devices.iter_mut().find(|d| d.id == id) else { return Vec::new() };
        let next = rec.keys.propose(&rk, ep);
        if let Some(s) = self.sessions.get_mut(id) {
            s.rekey_sent = true;
        }
        let Some(next) = next else { return Vec::new() };
        self.persist(id);
        self.send_inner(id, topic, json!({ "t": "rekey", "ep": next })).into_iter().collect()
    }

    // Receiving.

    pub fn tick(&mut self, host: &mut dyn Host, now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        while self.seen_sids.front().is_some_and(|(_, t)| now - t > SID_MEMORY_SECS) {
            self.seen_sids.pop_front();
        }
        self.watch_unlock(host, now);
        let closed: Vec<_> = self.refusals.iter().filter(|(_, r)| now - r.since >= REFUSAL_WINDOW_SECS).map(|(k, _)| k.clone()).collect();
        for key in closed {
            if let Some(r) = self.refusals.remove(&key) {
                Self::flush_refusals(host, r, now);
            }
        }
        let idle: Vec<String> = self.sessions.iter().filter(|(_, s)| now - s.last_in > SESSION_IDLE_SECS).map(|(id, _)| id.clone()).collect();
        let mut out = Vec::new();
        for id in idle {
            self.sessions.remove(&id);
            host.emit("phone-link-session", json!({ "deviceId": id, "state": "end", "reason": "idle" }));
            out.extend(self.flush_live_clip(host, &id, now));
        }
        let stale: Vec<String> = self
            .xfers
            .iter()
            .filter(|(_, x)| {
                let waiting = matches!(x.state, XState::AwaitDone | XState::AwaitAccept | XState::Delivered);
                now - x.created > XFER_MAX_SECS || (waiting && now - x.created > XFER_WAIT_SECS)
            })
            .map(|(f, _)| f.clone())
            .collect();
        for fid in stale {
            let Some(x) = self.xfers.remove(&fid) else { continue };
            Self::emit_xfer(host, &fid, &x, "failed", Some(XferError::Expired.code()));
            let name = self.devices.iter().find(|d| d.id == x.device_id).map(|d| d.name.clone()).unwrap_or_default();
            let (kind, extra) = match &x.plan {
                Some(plan) => ("file", Self::file_audit(plan, x.mailbox)),
                None => ("text", json!({ "len": x.text_len, "mailbox": x.mailbox })),
            };
            host.audit(Entry::new(now, kind, XferError::Expired.code()).device(&x.device_id, &name).dir(x.dir).extra(extra));
            let chunks = x.plan.as_ref().map_or(0, |p| p.chunks);
            if x.dir == "p2c" && x.mailbox {
                host.mark_processed(&fid, now);
            }
            // Nothing was uploaded for an offer the phone never accepted.
            if x.state == XState::AwaitAccept {
                continue;
            }
            if let Some(rec) = self.devices.iter().find(|d| d.id == x.device_id) {
                out.push(Action::Discard { topic: rec.topic.clone(), dir: x.dir, fid, chunks, mailbox: x.mailbox });
            }
        }
        out.extend(self.rescans());
        if self.offer.as_ref().is_some_and(|o| o.expired(now)) {
            out.extend(self.abort_pairing(host, AbortReason::Expired, now));
        }
        out
    }

    pub fn on_broadcast(&mut self, host: &mut dyn Host, topic: &str, event: &str, payload: &Value, now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        if self.offer.as_ref().is_some_and(|o| o.pair_topic == topic) {
            if event != "hello" {
                return Vec::new();
            }
            let result = self.offer.as_mut().expect("offer").on_hello(payload, now);
            return match result {
                HelloResult::Request(req) => {
                    host.emit("phone-link-pair-request", json!(req));
                    // Realtime checks the write policy on our first broadcast and keeps the answer
                    // for the channel. `link_create_device` closes the offer before the welcome,
                    // so write once while it is open (I-161). The phone ignores this event.
                    vec![Action::Send { topic: topic.to_string(), event: "seen", payload: json!({}) }]
                }
                HelloResult::Ignore => Vec::new(),
                HelloResult::Abort(reason) => self.abort_pairing(host, reason, now),
            };
        }
        let Some(idx) = self.device_index_by_topic(topic) else { return Vec::new() };
        match event {
            "hs1" => self.on_hs1(host, idx, payload, now),
            "m" => self.on_message(host, idx, payload, now_ms),
            _ => Vec::new(),
        }
    }

    fn on_hs1(&mut self, host: &mut dyn Host, idx: usize, p: &Value, now: i64) -> Vec<Action> {
        if self.devices[idx].suspended {
            return Vec::new();
        }
        let obj = match p.as_object() {
            Some(o) if o.keys().all(|k| matches!(k.as_str(), "sid" | "ep" | "e" | "ts" | "m")) => o,
            _ => return Vec::new(),
        };
        let parsed = (|| {
            let sid = key16(obj.get("sid")?.as_str()?)?;
            let ep = u32::try_from(obj.get("ep")?.as_u64()?).ok()?;
            let e_ph = key32(obj.get("e")?.as_str()?)?;
            let ts = obj.get("ts")?.as_u64()?;
            let tag = unb64u(obj.get("m")?.as_str()?)?;
            Some((sid, ep, e_ph, ts, tag))
        })();
        let Some((sid, ep, e_ph, ts, tag)) = parsed else { return Vec::new() };
        let Ok(ts_signed) = i64::try_from(ts) else { return Vec::new() };
        if now.abs_diff(ts_signed) > HS_WINDOW_SECS as u64 || self.seen_sids.iter().any(|(s, _)| *s == sid) {
            return Vec::new();
        }
        let before = self.devices[idx].keys.clone();
        let check = self.devices[idx].keys.check_hs1(&sid, &e_ph, ts, ep, &tag, now);
        let (id, name, topic) = {
            let d = &self.devices[idx];
            (d.id.clone(), d.name.clone(), d.topic.clone())
        };
        let k_dev = match check {
            // Handshakes draw from the device's command bucket, so a new session never refills it.
            HsCheck::Ok(_) if !self.limits(&id, now * 1000).cmd.take(now * 1000) => {
                self.devices[idx].keys = before;
                return Vec::new();
            }
            HsCheck::Ok(k) => k,
            HsCheck::Invalid => return Vec::new(),
            HsCheck::Clone => {
                self.devices[idx].suspended = true;
                self.persist(&id);
                self.sessions.remove(&id);
                self.lost.remove(&id);
                host.audit(Entry::new(now, "clone", "suspended").device(&id, &name).extra(json!({ "ep": ep })));
                host.emit("phone-link-device-suspended", json!({ "deviceId": id, "name": name }));
                host.emit("phone-link-devices", json!(self.devices()));
                return vec![Action::Leave(topic)];
            }
        };
        let e_pc = KeyPair::generate();
        let Some(shared) = e_pc.agree(&e_ph) else {
            self.devices[idx].keys = before;
            return Vec::new();
        };
        self.seen_sids.push_back((sid, now));
        let keys = crypto::session_keys(&shared, &k_dev, &sid, &e_ph, &e_pc.public);
        let hs2 = json!({ "sid": b64u(&sid), "e": b64u(&e_pc.public), "m": b64u(&crypto::hs2_mac(&k_dev, &sid, &e_ph, &e_pc.public)) });
        self.devices[idx].last_seen = Some(now);
        self.persist(&id);
        self.lost.remove(&id);
        self.sessions.insert(
            id.clone(),
            Session {
                sid,
                ep,
                keys,
                send_n: 0,
                recv_n: 0,
                got_in: false,
                sent_out: false,
                rekey_sent: false,
                started_at: now,
                last_in: now,
                last_status: None,
            },
        );
        let toast = self.toasts.get(&id).is_none_or(|t| now - t >= TOAST_EVERY_SECS);
        if toast {
            self.toasts.insert(id.clone(), now);
        }
        let clip = self.clip_session_started(&id, now);
        host.emit("phone-link-session", json!({ "deviceId": id, "name": name, "state": "start", "toast": toast }));
        host.audit(Entry::new(now, "session", "ok").device(&id, &name).extra(json!({ "ep": ep })));
        let mut out = vec![Action::Send { topic: topic.clone(), event: "hs2", payload: hs2 }];
        if self.devices[idx].perms.status {
            let perms = self.devices[idx].perms.clone();
            out.extend(self.status_evt(host, &id, &topic, &perms));
        }
        out.extend(self.reoffer(&id, &topic));
        if let Some((text, at)) = clip {
            out.extend(self.deliver_clip(host, &id, &text, at, now, true));
        }
        // The phone may have queued items while this PC was offline.
        out.push(Action::ScanMailbox(topic));
        out
    }

    /// A new session replaces the old one, and the phone forgets live offers it had not started
    /// downloading. Every live PC-to-phone file still open is offered again on the new session
    /// (`done` too when the upload already ended); the phone ignores an offer it is already
    /// downloading and answers `saved` for one it already has.
    fn reoffer(&mut self, id: &str, topic: &str) -> Vec<Action> {
        let mut open: Vec<(i64, String, Value, bool)> = self
            .xfers
            .iter()
            .filter(|(_, x)| x.device_id == id && x.dir == "c2p" && !x.mailbox && matches!(x.state, XState::AwaitAccept | XState::Sending | XState::Delivered))
            .filter_map(|(fid, x)| {
                let offer = serde_json::to_value(x.plan.as_ref()?.offer()).ok()?;
                Some((x.created, fid.clone(), offer, x.state == XState::Delivered))
            })
            .collect();
        open.sort_by_key(|(c, ..)| *c);
        let mut out = Vec::new();
        for (_, fid, offer, delivered) in open {
            out.extend(self.send_inner(id, topic, offer));
            if delivered {
                out.extend(self.send_inner(id, topic, json!({ "t": "done", "fid": fid })));
            }
        }
        out
    }

    fn on_message(&mut self, host: &mut dyn Host, idx: usize, p: &Value, now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        let (id, topic) = (self.devices[idx].id.clone(), self.devices[idx].topic.clone());
        let Some(s) = self.sessions.get_mut(&id) else { return Vec::new() };
        let parsed = (|| {
            let obj = p.as_object()?;
            if !obj.keys().all(|k| matches!(k.as_str(), "sid" | "n" | "c")) {
                return None;
            }
            let sid = key16(obj.get("sid")?.as_str()?)?;
            let n = obj.get("n")?.as_u64()?;
            let c = obj.get("c")?.as_str()?;
            if c.len() > MAX_CIPHERTEXT * 4 / 3 + 4 {
                return None;
            }
            Some((sid, n, unb64u(c)?))
        })();
        let Some((sid, n, c)) = parsed else { return Vec::new() };
        if sid != s.sid || n <= s.recv_n || c.len() > MAX_CIPHERTEXT {
            return Vec::new();
        }
        let Some(plain) = crypto::open(&s.keys.p2c, &crypto::msg_aad(&topic, &sid, DIR_P2C, n), &c) else { return Vec::new() };
        s.recv_n = n;
        s.got_in = true;
        s.last_in = now;
        let Some(msg) = remote::parse_inbound(&plain) else { return Vec::new() };
        // A bye comes from a page going away, whatever it got before.
        if !matches!(msg, Inbound::Bye) {
            self.clip_answered(&id, now);
        }
        let mut out = self.dispatch(host, idx, msg, now_ms);
        if self.sessions.contains_key(&id) {
            out.extend(self.maybe_rekey(&id, &topic));
        }
        out
    }

    fn res(&mut self, id: &str, topic: &str, req: u32, result: Result<Value, Deny>) -> Option<Action> {
        let body = match result {
            Ok(d) => json!({ "t": "res", "id": req, "ok": true, "d": d }),
            Err(e) => json!({ "t": "res", "id": req, "ok": false, "e": e.code() }),
        };
        self.send_inner(id, topic, body)
    }

    fn dispatch(&mut self, host: &mut dyn Host, idx: usize, msg: Inbound, now_ms: i64) -> Vec<Action> {
        let now = secs(now_ms);
        let rec = &self.devices[idx];
        let (id, topic, name) = (rec.id.clone(), rec.topic.clone(), rec.name.clone());
        let info = rec.info();
        let decision = authorize(&rec.perms, &msg);
        let mut out = Vec::new();
        match msg {
            Inbound::Cmd { id: req, op } => {
                let limited = !self.limits(&id, now_ms).cmd.take(now_ms);
                let mut extra = match &op {
                    Op::UrlOpen(u) => json!({ "host": remote::url_host(u) }),
                    Op::ClipboardSet(t) => json!({ "len": t.len() }),
                    Op::VolumeSet(v) => json!({ "v": v }),
                    Op::AgentAnswer { id, allow, .. } => json!({ "ask": id, "allow": allow }),
                    _ => Value::Null,
                };
                let result = if limited {
                    Err(Deny::Busy)
                } else {
                    match decision {
                        Decision::Allow | Decision::Confirm if !self.op_allowed(host, &id, &op, now_ms) => Err(Deny::Busy),
                        Decision::Allow => {
                            let r = host.execute(&info, &op);
                            if op == Op::PcLock && r.is_ok() {
                                self.limits(&id, now_ms).last_lock = Some(now);
                                self.lock_watch = Some(now);
                            }
                            r
                        }
                        Decision::Confirm => {
                            if let Op::UrlOpen(u) = &op {
                                host.confirm_url(&info, u);
                            }
                            if let Value::Object(m) = &mut extra {
                                m.insert("ask".into(), json!(true));
                            }
                            Ok(json!({ "pending": true }))
                        }
                        Decision::Deny(d) => Err(d),
                    }
                };
                let label = match &result {
                    Ok(_) => "ok",
                    Err(e) => e.code(),
                };
                let entry = Entry::new(now, op.name(), label).device(&id, &name).dir("p2c").extra(extra);
                if result.is_err() {
                    self.refused(host, entry);
                } else if op != Op::StatusGet {
                    host.audit(entry);
                }
                out.extend(self.res(&id, &topic, req, result));
            }
            Inbound::BadCmd { id: req } => {
                self.refused(host, Entry::new(now, "cmd", "invalid").device(&id, &name).dir("p2c"));
                out.extend(self.res(&id, &topic, req, Err(Deny::Invalid)));
            }
            Inbound::Text { id: req, text } => {
                let result = match decision {
                    Decision::Allow if !self.limits(&id, now_ms).xfer.take(now_ms) => Err(Deny::Busy),
                    Decision::Allow => Ok(Value::Null),
                    _ => Err(Deny::Denied),
                };
                let entry = Entry::new(now, "text", result.as_ref().map_or_else(|e| e.code(), |_| "ok")).device(&id, &name).dir("p2c").extra(json!({ "len": text.len() }));
                if result.is_ok() {
                    host.text_received(&info, &text);
                    host.audit(entry);
                } else {
                    self.refused(host, entry);
                }
                if let Some(req) = req {
                    out.extend(self.res(&id, &topic, req, result));
                }
            }
            Inbound::Offer(offer) => match self.check_live_offer(host, &id, &offer, &decision, now_ms) {
                Ok((plan, dir)) => {
                    let fid = plan.fid_hex();
                    host.audit(Entry::new(now, "offer", "ok").device(&id, &name).dir("p2c").extra(Self::file_audit(&plan, false)));
                    let x = Xfer { device_id: id.clone(), dir: "p2c", state: XState::AwaitDone, plan: Some(plan), source: None, download_dir: Some(dir), mailbox: false, created: now, text_len: 0 };
                    Self::emit_xfer(host, &fid, &x, "incoming", None);
                    self.xfers.insert(fid.clone(), x);
                    out.extend(self.send_inner(&id, &topic, json!({ "t": "accept", "fid": fid })));
                }
                Err(e) => {
                    self.refused(host, Entry::new(now, "offer", e.code()).device(&id, &name).dir("p2c").extra(json!({ "name": transfer::sanitize_name(&offer.name), "size": offer.size })));
                    out.extend(self.send_inner(&id, &topic, json!({ "t": "reject", "fid": offer.fid, "e": e.code() })));
                }
            },
            Inbound::Done(fid) => {
                if let Some(x) = self.xfers.get_mut(&fid).filter(|x| x.device_id == id && x.dir == "p2c" && x.state == XState::AwaitDone) {
                    x.state = XState::Receiving;
                    let (Some(plan), Some(dir)) = (x.plan.clone(), x.download_dir.clone()) else { return out };
                    Self::emit_xfer(host, &fid, x, "receiving", None);
                    out.push(Action::Receive(ReceiveJob { device_id: id.clone(), topic: topic.clone(), plan, download_dir: dir, mailbox: false }));
                }
            }
            Inbound::Accept(fid) => {
                if let Some(x) = self.xfers.get_mut(&fid).filter(|x| x.device_id == id && x.dir == "c2p" && x.state == XState::AwaitAccept) {
                    x.state = XState::Sending;
                    let (Some(plan), Some(path)) = (x.plan.clone(), x.source.clone()) else { return out };
                    Self::emit_xfer(host, &fid, x, "sending", None);
                    out.push(Action::Upload(SendJob { device_id: id.clone(), topic: topic.clone(), fid: fid.clone(), plan: Some(plan), source: Source::File(path), mailbox: None }));
                }
            }
            Inbound::Reject { fid, e } => {
                if self.xfers.get(&fid).is_some_and(|x| x.device_id == id && x.dir == "c2p" && !x.mailbox) {
                    let x = self.xfers.remove(&fid).expect("xfer");
                    if let Some(plan) = &x.plan {
                        host.audit(Entry::new(now, "file", "rejected").device(&id, &name).dir("c2p").extra(json!({ "name": plan.name, "size": plan.size, "e": e })));
                        if x.state != XState::AwaitAccept {
                            out.push(Action::Discard { topic: topic.clone(), dir: "c2p", fid: fid.clone(), chunks: plan.chunks, mailbox: false });
                        }
                    }
                    Self::emit_xfer(host, &fid, &x, "failed", Some(&e));
                } else if self.xfers.get(&fid).is_some_and(|x| x.device_id == id && x.dir == "p2c" && x.state == XState::AwaitDone) {
                    // The phone gave up on its upload (error or timeout): free the slot and its objects.
                    let x = self.xfers.remove(&fid).expect("xfer");
                    if let Some(plan) = &x.plan {
                        host.audit(Entry::new(now, "file", "cancelled").device(&id, &name).dir("p2c").extra(json!({ "name": plan.name, "size": plan.size, "e": e })));
                        out.push(Action::Discard { topic: topic.clone(), dir: "p2c", fid: fid.clone(), chunks: plan.chunks, mailbox: false });
                    }
                    Self::emit_xfer(host, &fid, &x, "failed", Some(&e));
                }
            }
            Inbound::Saved(fid) => {
                if self.xfers.get(&fid).is_some_and(|x| x.device_id == id && x.dir == "c2p" && x.state == XState::Delivered) {
                    let x = self.xfers.remove(&fid).expect("xfer");
                    if let Some(plan) = &x.plan {
                        host.audit(Entry::new(now, "file", "ok").device(&id, &name).dir("c2p").extra(Self::file_audit(plan, false)));
                    }
                    Self::emit_xfer(host, &fid, &x, "saved", None);
                }
            }
            Inbound::Ping => {}
            Inbound::Rekeyed(ep) => {
                if self.devices[idx].keys.confirm(ep, now) {
                    self.persist(&id);
                }
            }
            Inbound::Bye => {
                if self.sessions.remove(&id).is_some() {
                    host.emit("phone-link-session", json!({ "deviceId": id, "state": "end" }));
                    out.extend(self.flush_live_clip(host, &id, now));
                }
            }
            Inbound::Unpair => {
                out.extend(self.revoke(host, &id, "unpair", now_ms));
            }
        }
        out.extend(self.rescans());
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::phone_link::device::MemoryDevices;
    use crate::phone_link::pairing::tests::PhonePairing;
    use crate::phone_link::remote::OpenUrl;

    #[derive(Default)]
    pub struct TestHost {
        pub events: Vec<(String, Value)>,
        pub audit: Vec<Entry>,
        pub executed: Vec<Op>,
        pub texts: Vec<String>,
        pub urls_to_confirm: Vec<String>,
        pub volume: u32,
        pub download_dir: Option<PathBuf>,
        pub processed: std::collections::HashSet<String>,
        pub locked: bool,
        pub clip_fids: HashMap<String, Vec<String>>,
    }

    pub const TEST_MAX_IN: u64 = 100 * 1024 * 1024;
    pub const TEST_MAX_OUT: u64 = 50 * 1024 * 1024;

    impl Host for TestHost {
        fn execute(&mut self, _d: &DeviceInfo, op: &Op) -> Result<Value, Deny> {
            self.executed.push(op.clone());
            self.locked |= *op == Op::PcLock;
            Ok(if *op == Op::StatusGet { json!({ "locked": false }) } else { Value::Null })
        }
        fn confirm_url(&mut self, _d: &DeviceInfo, url: &str) {
            self.urls_to_confirm.push(url.into());
        }
        fn status(&self, perms: &Perms) -> Value {
            json!({ "locked": false, "volume": self.volume, "agent": if perms.agent_status { json!({"state": "working"}) } else { Value::Null } })
        }
        fn text_received(&mut self, _d: &DeviceInfo, text: &str) {
            self.texts.push(text.into());
        }
        fn emit(&mut self, event: &str, payload: Value) {
            self.events.push((event.into(), payload));
        }
        fn audit(&mut self, entry: Entry) {
            self.audit.push(entry);
        }
        fn inbound_check(&mut self, size: u64) -> Result<PathBuf, XferError> {
            if size > TEST_MAX_IN {
                return Err(XferError::TooBig);
            }
            Ok(self.download_dir.clone().unwrap_or_else(|| std::env::temp_dir().join("nectar-test-downloads")))
        }
        fn max_out_bytes(&self) -> u64 {
            TEST_MAX_OUT
        }
        fn processed(&self, fid: &str) -> bool {
            self.processed.contains(fid)
        }
        fn mark_processed(&mut self, fid: &str, _now: i64) {
            self.processed.insert(fid.to_string());
        }
        fn pc_locked(&self) -> bool {
            self.locked
        }
        fn clip_fids(&self, device: &str) -> Vec<String> {
            self.clip_fids.get(device).cloned().unwrap_or_default()
        }
        fn set_clip_fids(&mut self, device: &str, fids: &[String]) {
            self.clip_fids.insert(device.into(), fids.to_vec());
        }
    }

    /// The phone side of a session, as the PWA will speak it.
    pub struct Phone {
        pub topic: String,
        pub ep: u32,
        pub key: Key,
        sid: [u8; 16],
        e: Option<KeyPair>,
        keys: Option<crypto::SessionKeys>,
        send_n: u64,
        recv_n: u64,
    }

    impl Phone {
        pub fn new(topic: &str, k_dev0: Key) -> Self {
            Phone { topic: topic.into(), ep: 0, key: k_dev0, sid: [0; 16], e: None, keys: None, send_n: 0, recv_n: 0 }
        }
        pub fn hs1(&mut self, ts: i64) -> Value {
            self.sid = crypto::random_bytes::<16>();
            let e = KeyPair::generate();
            let m = crypto::hs1_mac(&self.key, &self.sid, &e.public, ts as u64, self.ep);
            let v = json!({ "sid": b64u(&self.sid), "ep": self.ep, "e": b64u(&e.public), "ts": ts, "m": b64u(&m) });
            self.e = Some(e);
            self.send_n = 0;
            self.recv_n = 0;
            v
        }
        pub fn on_hs2(&mut self, p: &Value) -> bool {
            let e = self.e.take().unwrap();
            let e_pc = key32(p["e"].as_str().unwrap()).unwrap();
            let tag = unb64u(p["m"].as_str().unwrap()).unwrap();
            if key16(p["sid"].as_str().unwrap()).unwrap() != self.sid || !crypto::hs2_verify(&self.key, &self.sid, &e.public, &e_pc, &tag) {
                return false;
            }
            let shared = e.agree(&e_pc).unwrap();
            self.keys = Some(crypto::session_keys(&shared, &self.key, &self.sid, &e.public, &e_pc));
            true
        }
        pub fn seal(&mut self, inner: Value) -> Value {
            self.send_n += 1;
            let k = self.keys.as_ref().unwrap();
            let c = crypto::seal(&k.p2c, &crypto::msg_aad(&self.topic, &self.sid, DIR_P2C, self.send_n), inner.to_string().as_bytes());
            json!({ "sid": b64u(&self.sid), "n": self.send_n, "c": b64u(&c) })
        }
        /// Decrypts a PC message and applies `rekey` like the PWA does.
        pub fn open(&mut self, p: &Value) -> Option<Value> {
            let n = p["n"].as_u64()?;
            if n <= self.recv_n {
                return None;
            }
            let k = self.keys.as_ref()?;
            let c = unb64u(p["c"].as_str()?)?;
            let plain = crypto::open(&k.c2p, &crypto::msg_aad(&self.topic, &self.sid, DIR_C2P, n), &c)?;
            self.recv_n = n;
            serde_json::from_slice(&plain).ok()
        }
        pub fn apply_rekey(&mut self, ep: u32) {
            let rk = *self.keys.as_ref().unwrap().rk;
            self.key = crypto::rekey(&self.key, &rk, ep);
            self.ep = ep;
        }
    }

    pub const NOW: i64 = 1_762_500_000_000;
    const UID: &str = "00000000-0000-4000-8000-0000000000aa";

    pub fn sends(actions: &[Action], event: &str) -> Vec<Value> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send { event: e, payload, .. } if *e == event => Some(payload.clone()),
                _ => None,
            })
            .collect()
    }

    /// Pairs a simulated phone and returns it with the engine and host.
    pub fn paired(perms: Perms) -> (Engine, TestHost, Phone) {
        let mut eng = Engine::new(Box::new(MemoryDevices::default()), "TEST-PC");
        let mut host = TestHost::default();
        let start = eng.start_pairing("https://example.github.io/phone/", "abcdefghijklmnopqrst", NOW);
        let pp = PhonePairing::from_qr(&start.url, UID);
        let seen = eng.on_broadcast(&mut host, &start.pair_topic, "hello", &pp.hello("Galaxy S26"), NOW + 1000);
        assert_eq!(seen, vec![Action::Send { topic: start.pair_topic.clone(), event: "seen", payload: json!({}) }]);
        assert!(eng.on_broadcast(&mut host, &start.pair_topic, "hello", &pp.hello("Galaxy S26"), NOW + 1500).is_empty(), "a repeated hello is ignored");
        assert_eq!(host.events.last().unwrap().0, "phone-link-pair-request");
        let (outcome, _) = eng.submit_code(&mut host, &pp.code(), NOW + 2000);
        assert_eq!(outcome, CodeOutcome::Accepted);
        let (pair_topic, device_topic, uid, rk) = eng.accepted_device().unwrap();
        assert_eq!((pair_topic.as_str(), uid.as_str()), (start.pair_topic.as_str(), UID));
        assert_eq!(rk, crypto::rejoin_key(&pp.k_dev0()));
        let (info, actions) = eng.complete_pairing(&mut host, perms, NOW + 3000).unwrap();
        let welcome = sends(&actions, "welcome").pop().unwrap();
        let plain = crypto::open(&crypto::welcome_key(&pp.k_dev0()), b"welcome", &unb64u(welcome["c"].as_str().unwrap()).unwrap()).unwrap();
        let w: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(w["id"], info.id);
        assert!(actions.contains(&Action::Join(device_topic.clone())));
        assert!(actions.contains(&Action::Leave(start.pair_topic.clone())));
        (eng, host, Phone::new(&device_topic, pp.k_dev0()))
    }

    pub fn connect(eng: &mut Engine, host: &mut TestHost, phone: &mut Phone, at: i64) -> Vec<Action> {
        let hs1 = phone.hs1(at / 1000);
        let out = eng.on_broadcast(host, &phone.topic.clone(), "hs1", &hs1, at);
        let hs2 = sends(&out, "hs2").pop().expect("hs2");
        assert!(phone.on_hs2(&hs2));
        out
    }

    fn cmd(eng: &mut Engine, host: &mut TestHost, phone: &mut Phone, inner: Value, at: i64) -> Vec<Value> {
        let m = phone.seal(inner);
        let out = eng.on_broadcast(host, &phone.topic.clone(), "m", &m, at);
        sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect()
    }

    #[test]
    fn pairing_session_command_and_ratchet() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let first: Vec<Value> = sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(first[0]["op"], "status", "status is pushed on connect");
        assert_eq!(eng.sessions().len(), 1);

        let replies = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":1,"op":"media.play_pause"}), NOW + 11_000);
        assert_eq!(replies[0], json!({"t":"res","id":1,"ok":true,"d":null}));
        assert_eq!(replies[1]["t"], "rekey");
        assert_eq!(host.executed, vec![Op::MediaPlayPause]);
        let ep = replies[1]["ep"].as_u64().unwrap() as u32;
        assert_eq!(ep, 1);
        phone.apply_rekey(ep);
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"rekeyed","ep":ep}), NOW + 12_000);
        assert_eq!(eng.devices[0].keys.current.ep, 1);

        // New session under the new epoch; the old key is now a clone signal.
        connect(&mut eng, &mut host, &mut phone, NOW + 20_000);
        assert!(eng.devices[0].keys.pending_prev.is_none());
        let mut clone = Phone::new(&phone.topic, eng.devices[0].keys.superseded[0].key);
        let hs1 = clone.hs1((NOW + 21_000) / 1000);
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, NOW + 21_000);
        assert!(out.contains(&Action::Leave(phone.topic.clone())));
        assert!(eng.devices[0].suspended);
        assert!(host.events.iter().any(|(e, _)| e == "phone-link-device-suspended"));
        let hs1 = phone.hs1((NOW + 22_000) / 1000);
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, NOW + 22_000).is_empty(), "suspended device gets no session");
        assert!(eng.topics().is_empty());
    }

    #[test]
    fn copy_connecting_during_a_rekey_is_suspended_when_the_real_phone_returns() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let k0 = phone.key;
        let replies = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":1,"op":"media.next"}), NOW + 11_000);
        let ep = replies[1]["ep"].as_u64().unwrap() as u32;
        phone.apply_rekey(ep);
        // The phone's `rekeyed` is lost; a copy of K0 connects first and replaces the session.
        let mut copy = Phone::new(&phone.topic, k0);
        connect(&mut eng, &mut host, &mut copy, NOW + 12_000);
        assert!(!eng.devices[0].suspended);
        let hs1 = phone.hs1((NOW + 13_000) / 1000);
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, NOW + 13_000);
        assert!(out.contains(&Action::Leave(phone.topic.clone())));
        assert!(eng.devices[0].suspended);
        assert!(!eng.has_sessions());
        assert!(host.events.iter().any(|(e, _)| e == "phone-link-device-suspended"));
    }

    #[test]
    fn silent_sessions_expire_and_pings_keep_them() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let t = NOW + 10_000 + SESSION_IDLE_SECS * 1000;
        eng.tick(&mut host, t - 60_000);
        assert!(eng.has_sessions());
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"ping"}), t - 30_000);
        eng.tick(&mut host, t + 1000);
        assert!(eng.has_sessions(), "the ping renewed the session");
        eng.tick(&mut host, t - 30_000 + SESSION_IDLE_SECS * 1000 + 1000);
        assert!(!eng.has_sessions());
        let (ev, p) = host.events.last().unwrap();
        assert_eq!((ev.as_str(), p["state"].as_str(), p["reason"].as_str()), ("phone-link-session", Some("end"), Some("idle")));
        assert!(eng.push_status(&mut host).is_empty(), "nothing is pushed to an expired session");
    }

    #[test]
    fn status_is_pushed_only_when_it_changes() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        assert!(eng.push_status(&mut host).is_empty(), "same status as on connect");
        host.volume = 40;
        let got: Vec<Value> = sends(&eng.push_status(&mut host), "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(got[0]["d"]["volume"], 40);
        assert!(eng.push_status(&mut host).is_empty());
    }

    #[test]
    fn huge_handshake_timestamp_is_dropped_without_overflow() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let mut hs1 = phone.hs1(NOW / 1000);
        for ts in [json!(9_223_372_036_854_775_808u64), json!(u64::MAX), json!(0)] {
            hs1["ts"] = ts;
            assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, NOW).is_empty());
        }
    }

    #[test]
    fn open_offer_can_be_shown_again() {
        let mut eng = Engine::new(Box::new(MemoryDevices::default()), "PC");
        let mut host = TestHost::default();
        assert!(eng.pairing_view("https://x/", "r").is_none());
        let start = eng.start_pairing("https://x/", "r", NOW);
        let (url, exp, req) = eng.pairing_view("https://x/", "r").unwrap();
        assert_eq!((url.as_str(), exp, req), (start.url.as_str(), start.expires_at, None));
        let p = PhonePairing::from_qr(&start.url, UID);
        eng.on_broadcast(&mut host, &start.pair_topic, "hello", &p.hello("Pixel"), NOW + 1000);
        assert_eq!(eng.pairing_view("https://x/", "r").unwrap().2.unwrap().name, "Pixel");
    }

    #[test]
    fn handshake_rejects_replay_stale_and_forged() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let at = NOW + 10_000;
        let hs1 = phone.hs1(at / 1000);
        assert!(!eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, at).is_empty());
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, at + 1000).is_empty(), "sid replay");
        let stale = phone.hs1(at / 1000 - 301);
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &stale, at).is_empty());
        let mut forged = phone.hs1(at / 1000);
        forged["m"] = json!(b64u(&[0u8; 32]));
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &forged, at).is_empty());
        let mut extra = phone.hs1(at / 1000);
        extra["x"] = json!(1);
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &extra, at).is_empty());
        assert!(eng.on_broadcast(&mut host, "nl-unknown", "hs1", &phone.hs1(at / 1000), at).is_empty());
    }

    #[test]
    fn messages_reject_replay_and_tampering() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let m = phone.seal(json!({"t":"cmd","id":1,"op":"volume.mute"}));
        assert!(!eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 11_000).is_empty());
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 11_500).is_empty(), "replayed n");
        let mut bad = phone.seal(json!({"t":"cmd","id":2,"op":"volume.mute"}));
        bad["n"] = json!(99);
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &bad, NOW + 12_000).is_empty(), "n is in the AAD");
        let mut other_sid = phone.seal(json!({"t":"bye"}));
        other_sid["sid"] = json!(b64u(&[9u8; 16]));
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &other_sid, NOW + 12_000).is_empty());
        assert_eq!(host.executed, vec![Op::VolumeMute]);
    }

    #[test]
    fn permissions_are_enforced_and_audited() {
        let perms = Perms { media: true, open_url: OpenUrl::Ask, ..Perms::default() };
        let (mut eng, mut host, mut phone) = paired(perms);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        assert!(sends(&out, "m").is_empty(), "no status without the status permission");
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":1,"op":"pc.lock"}), NOW + 11_000);
        assert_eq!(r[0]["e"], "denied");
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":2,"op":"url.open","a":{"u":"https://example.com/x"}}), NOW + 11_100);
        assert_eq!(r[0]["d"]["pending"], true);
        assert_eq!(host.urls_to_confirm, vec!["https://example.com/x".to_string()]);
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":3,"op":"shell.run"}), NOW + 11_200);
        assert_eq!(r[0]["e"], "invalid");
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"text","id":4,"text":"secret words"}), NOW + 11_300);
        assert_eq!(r[0]["e"], "denied");
        assert!(host.texts.is_empty() && host.executed.is_empty());
        let fid = "0123456789abcdef0123456789abcdef";
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"offer","fid":fid,"name":"a.jpg","size":1,"mime":"image/jpeg","sha256":"00","chunks":1,"key":"k"}), NOW + 11_400);
        assert_eq!(r[0], json!({"t":"reject","fid":fid,"e":"denied"}));
        let ops: Vec<(&str, &str)> = host.audit.iter().map(|e| (e.op.as_str(), e.result.as_str())).collect();
        assert!(ops.contains(&("pc.lock", "denied")) && ops.contains(&("url.open", "ok")) && ops.contains(&("text", "denied")));
        let text_entry = host.audit.iter().find(|e| e.op == "text").unwrap();
        assert_eq!(text_entry.extra, json!({"len": 12}), "audit keeps the length, never the text");
        let url_entry = host.audit.iter().find(|e| e.op == "url.open").unwrap();
        assert_eq!(url_entry.extra["host"], "example.com");
        assert!(eng.send_text(&eng.devices()[0].id, "x", &mut host, NOW).is_err(), "textOut off");
    }

    #[test]
    fn text_both_ways_rate_limit_and_unpair() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"text","id":1,"text":"from phone"}), NOW + 11_000);
        assert_eq!(r[0]["ok"], true);
        assert_eq!(host.texts, vec!["from phone".to_string()]);
        let id = eng.devices()[0].id.clone();
        let out = eng.send_text(&id, "from pc", &mut host, NOW + 11_500).unwrap();
        let got: Vec<Value> = sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(got[0], json!({"t":"text","text":"from pc"}));
        let mut busy = 0;
        for i in 0..15 {
            let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":100 + i,"op":"volume.up"}), NOW + 12_000);
            if r[0]["e"] == "busy" {
                busy += 1;
            }
        }
        assert!(busy >= 5, "{busy}");
        let m = phone.seal(json!({"t":"unpair"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 13_000);
        assert!(out.contains(&Action::RelayCleanup(phone.topic.clone())));
        assert!(eng.devices().is_empty());
        assert!(host.audit.iter().any(|e| e.op == "revoke" && e.result == "unpair"));
    }

    #[test]
    fn a_new_handshake_does_not_refill_the_command_bucket() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let at = NOW + 10_000;
        let mut ran = 0;
        for round in 0..3 {
            let hs1 = phone.hs1(at / 1000);
            let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, at);
            let Some(hs2) = sends(&out, "hs2").pop() else { break };
            assert!(phone.on_hs2(&hs2));
            for i in 0..10 {
                let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":round * 10 + i,"op":"volume.up"}), at);
                if r.first().is_some_and(|r| r["ok"] == true) {
                    ran += 1;
                }
            }
        }
        assert!(ran < 10, "{ran} commands ran within one second");
        let later = at + 5000;
        connect(&mut eng, &mut host, &mut phone, later);
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":99,"op":"volume.up"}), later);
        assert_eq!(r[0]["ok"], true, "the bucket refills over time");
    }

    fn res_of(replies: &[Value]) -> Value {
        replies.iter().find(|v| v["t"] == "res").cloned().expect("res")
    }

    #[test]
    fn a_pc_lock_burst_locks_once_and_an_unlock_gets_a_grace() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let mut busy = 0;
        for i in 0..20 {
            let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":i,"op":"pc.lock"}), NOW + 11_000 + i as i64 * 400));
            busy += usize::from(r["e"] == "busy");
        }
        assert_eq!(host.executed.iter().filter(|o| **o == Op::PcLock).count(), 1, "LockWorkStation ran once");
        assert_eq!(busy, 19);
        // Still refused after an unlock inside the cooldown.
        host.locked = false;
        eng.tick(&mut host, NOW + 30_000);
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":50,"op":"pc.lock"}), NOW + 40_000));
        assert_eq!(r["e"], "busy");
        // The user stays locked past the cooldown, then unlocks: a grace before the next lock.
        host.locked = true;
        let t = NOW + 11_000 + LOCK_COOLDOWN_SECS * 1000 + 60_000;
        connect(&mut eng, &mut host, &mut phone, t);
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":51,"op":"pc.lock"}), t + 1000));
        assert_eq!(r["e"], "busy", "never while locked");
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":52,"op":"volume.up"}), t + 1500));
        assert_eq!(r["ok"], true);
        // A phone lock is watched until the PC is unlocked.
        eng.lock_watch = Some(secs(t));
        host.locked = false;
        eng.tick(&mut host, t + 5000);
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":53,"op":"pc.lock"}), t + 5000 + (UNLOCK_GRACE_SECS - 1) * 1000));
        assert_eq!(r["e"], "busy", "grace after the unlock");
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":54,"op":"pc.lock"}), t + 5000 + UNLOCK_GRACE_SECS * 1000));
        assert_eq!(r["ok"], true);
        assert_eq!(host.executed.iter().filter(|o| **o == Op::PcLock).count(), 2);
        let refusals: Vec<&Entry> = host.audit.iter().filter(|e| e.op == "pc.lock" && e.result == "busy").collect();
        assert!(refusals.len() < 6, "refusals are coalesced: {}", refusals.len());
    }

    #[test]
    fn url_open_has_its_own_slower_bucket() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let mut ok = 0;
        for i in 0..8 {
            let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":i,"op":"url.open","a":{"u":"https://example.com/"}}), NOW + 11_000 + i as i64 * 400));
            ok += usize::from(r["ok"] == true);
        }
        assert_eq!(ok, 3);
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":20,"op":"volume.up"}), NOW + 15_000));
        assert_eq!(r["ok"], true, "media and volume keep their own rate");
        let r = res_of(&cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":21,"op":"url.open","a":{"u":"https://example.com/"}}), NOW + 25_000));
        assert_eq!(r["ok"], true, "one more after ten seconds");
    }

    #[test]
    fn junk_refusals_are_coalesced_and_audited_with_a_clean_name() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let before = host.audit.len();
        for i in 0..100 {
            cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":i,"op":"shell.run"}), NOW + 11_000 + i as i64 * 10);
        }
        let name: String = "\u{0007}\u{202E}x".repeat(85);
        let mut rejected = 0;
        for i in 0..100 {
            let offer = json!({"t":"offer","fid":format!("{:032x}", i + 1),"name":name,"size":10,"mime":"a/b","sha256":"00","chunks":3,"key":"k"});
            let r = cmd(&mut eng, &mut host, &mut phone, offer, NOW + 12_000 + i as i64 * 10);
            rejected += r.iter().filter(|v| v["t"] == "reject").count();
        }
        assert_eq!(rejected, 100, "every junk offer still gets its reject");
        let lines: Vec<(String, String)> = host.audit[before..].iter().map(|e| (e.op.clone(), e.result.clone())).collect();
        assert_eq!(lines, vec![("cmd".into(), "invalid".into()), ("offer".into(), "invalid".into()), ("offer".into(), "busy".into())]);
        let logged = host.audit[before + 1].extra["name"].as_str().unwrap();
        assert!(logged.chars().count() <= 120 && !logged.chars().any(|c| c.is_control() || c == '\u{202E}'), "{logged:?}");
        eng.tick(&mut host, NOW + 80_000);
        let summary: Vec<&Entry> = host.audit.iter().filter(|e| e.extra.get("repeated").is_some()).collect();
        assert_eq!(summary.len(), 3);
        let invalid_cmds = summary.iter().find(|e| e.op == "cmd").unwrap();
        assert_eq!(invalid_cmds.extra["repeated"], 99);
        assert_eq!(invalid_cmds.device.as_deref(), Some(eng.devices()[0].id.as_str()));
    }

    #[test]
    fn a_replayed_hs1_after_a_restart_does_not_suspend_the_device() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let at = NOW + 10_000;
        let captured = phone.hs1(at / 1000);
        let hs2 = sends(&eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &captured, at), "hs2").pop().unwrap();
        assert!(phone.on_hs2(&hs2));
        let replies = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":1,"op":"media.next"}), at + 1000);
        let ep = replies.iter().find(|v| v["t"] == "rekey").unwrap()["ep"].as_u64().unwrap() as u32;
        phone.apply_rekey(ep);
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"rekeyed","ep":ep}), at + 2000);
        // Nectar restarts: a new engine over the same records, with no memory of seen sids.
        let mut eng = Engine::new(Box::new(MemoryDevices(eng.devices.clone())), "TEST-PC");
        connect(&mut eng, &mut host, &mut phone, at + 60_000);
        assert_eq!(eng.devices[0].keys.superseded[0].ep, 0);
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &captured, at + 90_000);
        assert!(out.is_empty(), "the old handshake is dropped");
        assert!(!eng.devices[0].suspended);
        assert_eq!(eng.sessions().len(), 1, "the real session stays");
    }

    #[test]
    fn pairing_aborts_and_expiry() {
        let mut eng = Engine::new(Box::new(MemoryDevices::default()), "PC");
        let mut host = TestHost::default();
        let start = eng.start_pairing("https://x/", "r", NOW);
        let a = PhonePairing::from_qr(&start.url, UID);
        let b = PhonePairing::from_qr(&start.url, "00000000-0000-4000-8000-0000000000bb");
        eng.on_broadcast(&mut host, &start.pair_topic, "hello", &a.hello("A"), NOW + 1000);
        let out = eng.on_broadcast(&mut host, &start.pair_topic, "hello", &b.hello("B"), NOW + 2000);
        assert!(out.contains(&Action::CancelPairing));
        assert_eq!(host.events.last().unwrap().1["reason"], "second-device");
        assert_eq!(eng.submit_code(&mut host, &a.code(), NOW + 3000).0, CodeOutcome::NoRequest);

        let start = eng.start_pairing("https://x/", "r", NOW);
        assert!(eng.tick(&mut host, NOW + 299_000).is_empty());
        assert!(eng.tick(&mut host, NOW + 300_000).contains(&Action::Leave(start.pair_topic)));
        assert!(eng.pairing_view("https://x/", "r").is_none());

        let start = eng.start_pairing("https://x/", "r", NOW);
        let replaced = eng.start_pairing("https://x/", "r", NOW);
        assert_eq!(replaced.replaced_topic.as_deref(), Some(start.pair_topic.as_str()));
        let p = PhonePairing::from_qr(&replaced.url, UID);
        eng.on_broadcast(&mut host, &replaced.pair_topic, "hello", &p.hello("A"), NOW + 1000);
        assert_eq!(eng.submit_code(&mut host, &p.code(), NOW + 2000).0, CodeOutcome::Accepted);
        assert!(eng.fail_pairing(&mut host, "no offer", NOW + 3000).contains(&Action::CancelPairing));
        assert!(eng.complete_pairing(&mut host, Perms::new_device(), NOW).is_err());
    }

    #[test]
    fn revoke_and_perms_update() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let out = eng.set_perms(&id, Perms::default(), NOW + 11_000, &mut host).unwrap();
        let got: Vec<Value> = sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(got[0]["op"], "perms");
        let r = cmd(&mut eng, &mut host, &mut phone, json!({"t":"cmd","id":1,"op":"media.next"}), NOW + 12_000);
        assert_eq!(r[0]["e"], "denied");
        let out = eng.revoke(&mut host, &id, "ok", NOW + 13_000);
        let got: Vec<Value> = sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(got[0]["t"], "bye");
        assert!(out.contains(&Action::RelayCleanup(phone.topic.clone())));
        let hs1 = phone.hs1((NOW + 14_000) / 1000);
        assert!(eng.on_broadcast(&mut host, &phone.topic.clone(), "hs1", &hs1, NOW + 14_000).is_empty(), "revoked: no handshake");
    }

    // Transfers.

    use crate::phone_link::transfer::tests::{phone_upload, rt, temp_dir, MemBlobs};
    use crate::phone_link::transfer::{blob_path, mbox_path, open_chunk, open_mailbox, seal_mailbox, CHUNK};
    use sha2::Digest;

    fn receive_jobs(out: &[Action]) -> Vec<ReceiveJob> {
        out.iter().filter_map(|a| if let Action::Receive(j) = a { Some(j.clone()) } else { None }).collect()
    }

    fn upload_jobs(out: &[Action]) -> Vec<SendJob> {
        out.iter().filter_map(|a| if let Action::Upload(j) = a { Some(j.clone()) } else { None }).collect()
    }

    fn opened(phone: &mut Phone, out: &[Action]) -> Vec<Value> {
        sends(out, "m").iter().filter_map(|p| phone.open(p)).collect()
    }

    #[test]
    fn files_both_ways_over_a_live_session() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-files");
        host.download_dir = Some(dir.clone());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let blobs = MemBlobs::default();

        // Phone to PC: offer, accept, upload, done, receive, saved.
        let data: Vec<u8> = (0..(CHUNK as usize + 17)).map(|i| (i % 7) as u8).collect();
        let plan = phone_upload(&blobs, &phone.topic, "IMG_0001.jpg", &data);
        let fid = plan.fid_hex();
        let r = cmd(&mut eng, &mut host, &mut phone, serde_json::to_value(plan.offer()).unwrap(), NOW + 11_000);
        assert_eq!(r[0], json!({"t":"accept","fid":fid}));
        let m = phone.seal(json!({"t":"done","fid":fid}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000);
        let job = receive_jobs(&out).pop().expect("receive job");
        assert!(!job.mailbox);
        let path = rt().block_on(transfer::receive(&blobs, &job, |_| {})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
        let out = eng.transfer_finished(&mut host, &fid, Ok(Some(path.clone())), NOW + 13_000);
        assert!(opened(&mut phone, &out).contains(&json!({"t":"saved","fid":fid})));
        let received = host.events.iter().find(|(e, _)| e == "phone-link-received").unwrap();
        assert_eq!(received.1["paths"][0], path.to_string_lossy().as_ref());
        let entry = host.audit.iter().find(|e| e.op == "file" && e.dir == Some("p2c")).unwrap();
        assert_eq!(entry.result, "ok");
        assert_eq!(entry.extra["sha256"], crypto::hex(&sha2::Sha256::digest(&data)));
        assert_eq!(entry.extra["name"], "IMG_0001.jpg");
        assert!(eng.transfers().is_empty());

        // PC to phone: offer, accept, upload, done, saved.
        let src = dir.join("notes.txt");
        std::fs::write(&src, b"from the pc").unwrap();
        let (size, sha) = transfer::hash_file(&src).unwrap();
        let id = eng.devices()[0].id.clone();
        let (fid, out) = eng.send_file(&mut host, &id, src.clone(), "notes.txt", size, sha, NOW + 14_000).unwrap();
        let offer: Offer = serde_json::from_value(opened(&mut phone, &out).into_iter().find(|v| v["t"] == "offer").unwrap()).unwrap();
        let plan = offer.plan().unwrap();
        assert_eq!((plan.fid_hex(), plan.name.as_str(), plan.mime.as_str()), (fid.clone(), "notes.txt", "text/plain"));
        assert_eq!(eng.transfers()[0].state, "offered");
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 15_000);
        let job = upload_jobs(&out).pop().expect("upload job");
        assert!(job.mailbox.is_none());
        rt().block_on(transfer::send(&blobs, &job, |_| {})).unwrap();
        let out = eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 16_000);
        assert!(opened(&mut phone, &out).contains(&json!({"t":"done","fid":fid})));
        let blob = blobs.objects.lock().unwrap()[&blob_path(&phone.topic, "c2p", &fid, 0)].clone();
        assert_eq!(open_chunk(&plan, crypto::DIR_C2P, 0, &blob).unwrap(), b"from the pc");
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"saved","fid":fid}), NOW + 17_000);
        assert!(host.events.iter().any(|(e, p)| e == "phone-link-transfer" && p["fid"] == fid.as_str() && p["state"] == "saved"));
        assert!(host.audit.iter().any(|e| e.op == "file" && e.dir == Some("c2p") && e.result == "ok"));
        assert!(eng.transfers().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offers_are_refused_for_permission_size_shape_and_load() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let offer = |size: u64| serde_json::to_value(transfer::Plan::new_outbound("a.bin", size, "a/b", [1; 32]).unwrap().offer()).unwrap();
        let r = cmd(&mut eng, &mut host, &mut phone, offer(TEST_MAX_IN + 1), NOW + 11_000);
        assert_eq!(r[0]["e"], "tooBig");
        let mut bad = offer(10);
        bad["chunks"] = json!(2);
        assert_eq!(cmd(&mut eng, &mut host, &mut phone, bad, NOW + 11_100)[0]["e"], "invalid");
        for i in 0..MAX_ACTIVE {
            assert_eq!(cmd(&mut eng, &mut host, &mut phone, offer(10 + i as u64), NOW + 11_200 + i as i64 * 400)[0]["t"], "accept");
        }
        assert_eq!(cmd(&mut eng, &mut host, &mut phone, offer(99), NOW + 13_000)[0]["e"], "busy");
        let id = eng.devices()[0].id.clone();
        eng.set_perms(&id, Perms { files_in: false, ..Perms::new_device() }, NOW + 13_400, &mut host).unwrap();
        let replies = cmd(&mut eng, &mut host, &mut phone, offer(5), NOW + 13_500);
        assert_eq!(replies.iter().find(|v| v["t"] == "reject").unwrap()["e"], "denied");
        // `done` for a transfer that was never accepted, `accept` for one the PC never offered.
        let stray = "0123456789abcdef0123456789abcdef";
        let m = phone.seal(json!({"t":"done","fid":stray}));
        assert!(receive_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 13_600)).is_empty());
        let m = phone.seal(json!({"t":"accept","fid":stray}));
        assert!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 13_700)).is_empty());
        // Waiting transfers expire.
        eng.tick(&mut host, NOW + 13_000 + 2 * 3600 * 1000 + 2000);
        assert!(eng.transfers().is_empty());
        assert_eq!(eng.send_file(&mut host, &id, PathBuf::from("x"), "x", TEST_MAX_OUT + 1, [0; 32], NOW).unwrap_err(), "tooBig");
    }

    #[test]
    fn a_corrupt_upload_is_refused_and_discarded() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-corrupt");
        host.download_dir = Some(dir.clone());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let blobs = MemBlobs::default();
        let mut plan = phone_upload(&blobs, &phone.topic, "a.txt", b"abc");
        plan.sha256 = [9; 32];
        let fid = plan.fid_hex();
        cmd(&mut eng, &mut host, &mut phone, serde_json::to_value(plan.offer()).unwrap(), NOW + 11_000);
        let m = phone.seal(json!({"t":"done","fid":fid}));
        let job = receive_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000)).pop().unwrap();
        let r = rt().block_on(transfer::receive(&blobs, &job, |_| {}));
        assert_eq!(r, Err(XferError::Corrupt));
        let out = eng.transfer_finished(&mut host, &fid, r.map(Some), NOW + 13_000);
        assert!(opened(&mut phone, &out).contains(&json!({"t":"reject","fid":fid,"e":"corrupt"})));
        assert!(out.contains(&Action::Discard { topic: phone.topic.clone(), dir: "p2c", fid: fid.clone(), chunks: 1, mailbox: false }));
        assert!(!host.events.iter().any(|(e, _)| e == "phone-link-received"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_phone_can_decline_a_pc_offer() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (fid, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"reject","fid":fid,"e":"storage"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000);
        assert!(!out.iter().any(|a| matches!(a, Action::Discard { .. })), "nothing was uploaded yet");
        assert!(eng.transfers().is_empty());
        assert!(host.audit.iter().any(|e| e.op == "file" && e.result == "rejected"));
    }

    #[test]
    fn a_full_device_leaves_mailbox_offers_for_later() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-mbox-busy");
        host.download_dir = Some(dir.clone());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let topic = phone.topic.clone();
        let mut fids = Vec::new();
        for i in 0..MAX_ACTIVE {
            let o = serde_json::to_value(transfer::Plan::new_outbound("a.bin", 10 + i as u64, "a/b", [1; 32]).unwrap().offer()).unwrap();
            fids.push(o["fid"].as_str().unwrap().to_string());
            assert_eq!(cmd(&mut eng, &mut host, &mut phone, o, NOW + 11_000 + i as i64 * 400)[0]["t"], "accept");
        }
        let plan = transfer::Plan::new_outbound("late.jpg", 5, "image/jpeg", [2; 32]).unwrap();
        let obj = seal_mailbox(&phone.key, 0, &topic, "p2c", &plan.fid, (NOW / 1000) as u64 + 13, &serde_json::to_value(plan.offer()).unwrap());
        let out = eng.mailbox_item(&mut host, &topic, &plan.fid_hex(), &obj, NOW + 13_000);
        assert!(out.is_empty(), "neither received nor discarded: {out:?}");
        assert!(!host.processed.contains(&plan.fid_hex()));

        // The phone gives up on one upload: its slot and objects are freed and the mailbox is read again.
        let m = phone.seal(json!({"t":"reject","fid":fids[0],"e":"network"}));
        let out = eng.on_broadcast(&mut host, &topic, "m", &m, NOW + 14_000);
        assert!(out.contains(&Action::Discard { topic: topic.clone(), dir: "p2c", fid: fids[0].clone(), chunks: 1, mailbox: false }));
        assert!(out.contains(&Action::ScanMailbox(topic.clone())));
        assert_eq!(eng.transfers().len(), MAX_ACTIVE - 1);
        let out = eng.mailbox_item(&mut host, &topic, &plan.fid_hex(), &obj, NOW + 15_000);
        assert_eq!(receive_jobs(&out).len(), 1);

        // Expired transfers take their objects with them; a mailbox one is not fetched again.
        let out = eng.tick(&mut host, NOW + 13_000 + 2 * 3600 * 1000);
        assert_eq!(out.iter().filter(|a| matches!(a, Action::Discard { dir: "p2c", mailbox: false, chunks: 1, .. })).count(), MAX_ACTIVE - 1);
        let out = eng.tick(&mut host, NOW + 15_000 + 3 * 3600 * 1000 + 2000);
        assert!(out.contains(&Action::Discard { topic: topic.clone(), dir: "p2c", fid: plan.fid_hex(), chunks: 1, mailbox: true }));
        assert!(host.processed.contains(&plan.fid_hex()));
        assert!(eng.transfers().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn a_late_or_stray_phone_reject_does_not_touch_a_running_download() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-late-reject");
        host.download_dir = Some(dir.clone());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let blobs = MemBlobs::default();
        let plan = phone_upload(&blobs, &phone.topic, "a.txt", b"abc");
        let fid = plan.fid_hex();
        cmd(&mut eng, &mut host, &mut phone, serde_json::to_value(plan.offer()).unwrap(), NOW + 11_000);
        let m = phone.seal(json!({"t":"done","fid":fid}));
        assert_eq!(receive_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000)).len(), 1);
        // The PC is already downloading: a reject that crosses `done` must not drop the transfer or delete its chunks.
        let m = phone.seal(json!({"t":"reject","fid":fid,"e":"network"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_500);
        assert!(!out.iter().any(|a| matches!(a, Action::Discard { .. })));
        assert_eq!(eng.transfers().len(), 1);
        assert!(!host.audit.iter().any(|e| e.result == "cancelled"));
        // A reject for a fid nobody knows is ignored.
        let m = phone.seal(json!({"t":"reject","fid":"0123456789abcdef0123456789abcdef","e":"network"}));
        assert!(!eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 13_000).iter().any(|a| matches!(a, Action::Discard { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_pc_upload_that_outlives_the_session_goes_to_the_mailbox() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (fid, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        assert_eq!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000)).len(), 1);
        eng.end_session(&id, &mut host);
        let out = eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 13_000);
        let job = upload_jobs(&out).pop().expect("mailbox offer");
        assert!(matches!(job.source, Source::None), "the chunks are not uploaded again");
        let fid_b: [u8; 16] = crypto::unhex(&fid).unwrap().try_into().unwrap();
        let inner = open_mailbox(|ep| (ep == 0).then_some(phone.key), &phone.topic, "c2p", &fid_b, &job.mailbox.unwrap(), NOW / 1000 + 13).unwrap();
        let offer: Value = serde_json::from_slice(&inner).unwrap();
        assert_eq!((offer["t"].as_str(), offer["fid"].as_str()), (Some("offer"), Some(fid.as_str())));
        eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 14_000);
        assert!(eng.transfers().is_empty());
        assert!(host.audit.iter().any(|e| e.op == "file" && e.result == "queued"));
    }

    #[test]
    fn a_lost_relay_socket_ends_sessions_so_sends_use_the_mailbox() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-link-lost");
        host.download_dir = Some(dir.clone());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (up, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":up}));
        assert_eq!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000)).len(), 1);
        let blobs = MemBlobs::default();
        let plan = phone_upload(&blobs, &phone.topic, "a.jpg", b"phone bytes");
        let down = plan.fid_hex();
        cmd(&mut eng, &mut host, &mut phone, serde_json::to_value(plan.offer()).unwrap(), NOW + 12_500);
        let m = phone.seal(json!({"t":"done","fid":down}));
        assert_eq!(receive_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_600)).len(), 1);

        eng.link_lost(&mut host, NOW + 12_700);
        assert!(!eng.has_sessions());
        assert!(host.events.iter().any(|(e, p)| e == "phone-link-session" && p["state"] == "end" && p["reason"] == "offline"));

        // Text and files while the socket is down go to the mailbox, not into a dead session.
        let out = eng.send_text(&id, "while offline", &mut host, NOW + 13_000).unwrap();
        assert!(sends(&out, "m").is_empty());
        let job = upload_jobs(&out).pop().expect("mailbox text");
        let fid_b: [u8; 16] = crypto::unhex(&job.fid).unwrap().try_into().unwrap();
        let inner = open_mailbox(|ep| (ep == 0).then_some(phone.key), &phone.topic, "c2p", &fid_b, &job.mailbox.unwrap(), NOW / 1000 + 13).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&inner).unwrap()["text"], "while offline");
        let (_, out) = eng.send_file(&mut host, &id, PathBuf::from("C:/y.bin"), "y.bin", 10, [0; 32], NOW + 13_500).unwrap();
        assert!(sends(&out, "m").is_empty());
        let job = upload_jobs(&out).pop().expect("mailbox file");
        assert!(job.mailbox.is_some() && matches!(job.source, Source::File(_)));

        // Transfers that end while offline: no `done` or `saved` into the dead session.
        let out = eng.transfer_finished(&mut host, &up, Ok(None), NOW + 14_000);
        assert!(sends(&out, "m").is_empty());
        let job = upload_jobs(&out).pop().expect("mailbox offer for the finished upload");
        assert!(job.mailbox.is_some() && matches!(job.source, Source::None));
        let saved = dir.join("a.jpg");
        std::fs::write(&saved, b"phone bytes").unwrap();
        let out = eng.transfer_finished(&mut host, &down, Ok(Some(saved)), NOW + 14_500);
        assert!(sends(&out, "m").is_empty());
        assert!(host.events.iter().any(|(e, _)| e == "phone-link-received"));

        // The phone's next handshake starts a fresh session; the queued upload is not offered live.
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 30_000);
        assert!(eng.has_sessions());
        assert!(!opened(&mut phone, &out).iter().any(|v| v["t"] == "offer" && v["fid"] == up.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rejoined_topic_tells_the_phone_to_start_a_new_session() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let topic = phone.topic.clone();
        assert!(eng.relinked(&topic).is_empty(), "no lost session, nothing to say");
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.link_lost(&mut host, NOW + 11_000);
        eng.link_lost(&mut host, NOW + 11_000);

        let out = eng.relinked(&topic);
        assert_eq!(opened(&mut phone, &out), vec![json!({"t":"relink"})]);
        assert!(eng.relinked(&topic).is_empty(), "said once");

        // The phone answers with a new handshake, which carries status, and texts go live again.
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 20_000);
        assert!(opened(&mut phone, &out).iter().any(|v| v["op"] == "status"));
        let id = eng.devices()[0].id.clone();
        let out = eng.send_text(&id, "live", &mut host, NOW + 21_000).unwrap();
        assert_eq!(opened(&mut phone, &out)[0]["text"], "live");

        // A handshake that came first, or a revoke while offline, leaves nothing to relink.
        eng.link_lost(&mut host, NOW + 20_000);
        connect(&mut eng, &mut host, &mut phone, NOW + 30_000);
        assert!(eng.relinked(&topic).is_empty());
        eng.link_lost(&mut host, NOW + 31_000);
        eng.revoke(&mut host, &id, "ok", NOW + 40_000);
        assert!(eng.relinked(&topic).is_empty());
    }

    #[test]
    fn relink_opens_after_a_message_lost_with_the_socket() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let topic = phone.topic.clone();
        let id = eng.devices()[0].id.clone();
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        // Sealed in the moment before the loss was noticed: the phone never gets it.
        let lost_text = eng.send_text(&id, "gone", &mut host, NOW + 11_000).unwrap();
        assert_eq!(sends(&lost_text, "m").len(), 1);
        eng.link_lost(&mut host, NOW + 11_500);
        let out = eng.relinked(&topic);
        assert_eq!(opened(&mut phone, &out), vec![json!({"t":"relink"})], "a counter gap does not hide the relink");
    }

    #[test]
    fn a_suspended_device_is_not_relinked() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let topic = phone.topic.clone();
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.link_lost(&mut host, NOW + 11_000);
        eng.devices[0].suspended = true;
        assert!(eng.relinked(&topic).is_empty());
        eng.devices[0].suspended = false;
        assert!(!eng.relinked(&topic).is_empty(), "the entry is kept for a device that is not suspended");
    }

    #[test]
    fn link_lost_is_idempotent_and_the_old_session_is_deaf() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        eng.link_lost(&mut host, NOW);
        assert!(host.events.iter().all(|(e, _)| e != "phone-link-session"), "no sessions, nothing to end");

        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.link_lost(&mut host, NOW + 11_000);
        eng.link_lost(&mut host, NOW + 11_000);
        let ends = host.events.iter().filter(|(e, p)| e == "phone-link-session" && p["state"] == "end").count();
        assert_eq!(ends, 1, "repeated retries must not repeat the end event");

        // A phone that still talks on the old session gets no answer and nothing runs.
        let before = host.executed.len();
        let m = phone.seal(json!({"t":"cmd","id":7,"op":"media.play_pause"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 11_000);
        assert!(out.is_empty());
        assert_eq!(host.executed.len(), before);
        assert!(!eng.has_sessions());
    }

    #[test]
    fn offline_sends_keep_their_limits_and_permissions() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        eng.link_lost(&mut host, NOW + 11_000);

        assert_eq!(eng.send_text(&id, "", &mut host, NOW + 11_000).unwrap_err(), "invalid");
        assert_eq!(eng.send_text(&id, &"x".repeat(remote::MAX_TEXT + 1), &mut host, NOW + 11_000).unwrap_err(), "invalid");
        for i in 0..MAX_ACTIVE {
            let out = eng.send_text(&id, &format!("queued {i}"), &mut host, NOW + 12_000).unwrap();
            assert_eq!(upload_jobs(&out).len(), 1);
        }
        assert_eq!(eng.send_text(&id, "one too many", &mut host, NOW + 13_000).unwrap_err(), "busy");
        let too_many = eng.send_file(&mut host, &id, PathBuf::from("C:/z.bin"), "z.bin", 10, [0; 32], NOW + 13_000);
        assert_eq!(too_many.unwrap_err(), "busy");

        // A queued item that is delivered later frees its slot again.
        let fid = eng.xfers.keys().next().cloned().unwrap();
        let out = eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 14_000);
        assert!(sends(&out, "m").is_empty());
        assert!(host.audit.iter().any(|e| e.op == "text" && e.result == "queued"));
        assert!(eng.send_text(&id, "fits again", &mut host, NOW + 15_000).is_ok());

        let mut off = Perms::new_device();
        off.text_out = false;
        off.files_out = false;
        let (mut eng, mut host, _) = paired(off);
        let id = eng.devices()[0].id.clone();
        eng.link_lost(&mut host, NOW);
        assert_eq!(eng.send_text(&id, "hi", &mut host, NOW + 11_000).unwrap_err(), "denied");
        assert_eq!(eng.send_file(&mut host, &id, PathBuf::from("C:/a.bin"), "a.bin", 10, [0; 32], NOW + 11_000).unwrap_err(), "denied");
    }

    #[test]
    fn revoking_while_offline_only_cleans_up() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let topic = phone.topic.clone();
        eng.link_lost(&mut host, NOW + 11_000);
        let out = eng.send_text(&id, "queued then revoked", &mut host, NOW + 11_000).unwrap();
        let fid = upload_jobs(&out).pop().expect("mailbox text").fid;

        let out = eng.revoke(&mut host, &id, "ok", NOW + 12_000);
        assert!(sends(&out, "m").is_empty(), "no bye into a dead session");
        assert!(out.iter().any(|a| matches!(a, Action::RelayCleanup(t) if *t == topic)));
        assert!(eng.devices().is_empty());
        assert_eq!(eng.send_text(&id, "late", &mut host, NOW + 13_000).unwrap_err(), "unknown device");
        // The queued item's upload may still finish later; the engine no longer knows the device.
        assert!(eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 14_000).is_empty());
    }

    #[test]
    fn a_new_phone_session_gets_the_open_pc_offers_again() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (fid, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        assert_eq!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000)).len(), 1);

        // Mid-upload the phone starts a new session (it dropped the offer it had not started).
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 160_000);
        let inner = opened(&mut phone, &out);
        let offer = inner.iter().find(|v| v["t"] == "offer").expect("offered again on the new session");
        assert_eq!(offer["fid"], fid.as_str());
        assert!(!inner.iter().any(|v| v["t"] == "done"), "not uploaded yet");
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        assert!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 161_000)).is_empty(), "the running upload is not started twice");
        let out = eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 200_000);
        assert!(opened(&mut phone, &out).contains(&json!({"t":"done","fid":fid})), "done goes out on the new session");

        // `done` lost with yet another session: offer and done are sent again.
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 400_000);
        let inner = opened(&mut phone, &out);
        let pos = |t: &str| inner.iter().position(|v| v["t"] == t && v["fid"] == fid.as_str());
        assert!(pos("offer").unwrap() < pos("done").unwrap());
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"saved","fid":fid}), NOW + 401_000);
        assert!(eng.transfers().is_empty());
        assert!(host.audit.iter().any(|e| e.op == "file" && e.dir == Some("c2p") && e.result == "ok"));

        // An unknown `done` answered with `reject expired` frees what the PC still holds.
        let (fid, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/y.bin"), "y.bin", 10, [0; 32], NOW + 402_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 403_000);
        eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 404_000);
        let m = phone.seal(json!({"t":"reject","fid":fid,"e":"expired"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 405_000);
        assert!(out.iter().any(|a| matches!(a, Action::Discard { dir: "c2p", .. })));
        assert!(eng.transfers().is_empty());
    }

    #[test]
    fn expired_transfers_are_audited() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (fid, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":fid}));
        eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000);
        eng.transfer_finished(&mut host, &fid, Ok(None), NOW + 13_000);
        eng.tick(&mut host, NOW + 11_000 + 2 * 3600 * 1000 + 2000);
        assert!(eng.transfers().is_empty());
        let e = host.audit.iter().find(|e| e.op == "file" && e.result == "expired").expect("expiry audited");
        assert_eq!((e.dir, e.extra["name"].as_str(), e.extra["mailbox"].as_bool()), (Some("c2p"), Some("x.bin"), Some(false)));
    }

    #[test]
    fn mailbox_both_ways_with_replay_and_expiry() {
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let dir = temp_dir("eng-mbox");
        host.download_dir = Some(dir.clone());
        let id = eng.devices()[0].id.clone();
        let topic = phone.topic.clone();
        let blobs = MemBlobs::default();

        // PC to an offline phone: text and a file go to the c2p mailbox.
        let out = eng.send_text(&id, "while you were away", &mut host, NOW).unwrap();
        let job = upload_jobs(&out).pop().expect("mailbox upload");
        rt().block_on(transfer::send(&blobs, &job, |_| {})).unwrap();
        let obj = blobs.objects.lock().unwrap()[&mbox_path(&topic, "c2p", &job.fid)].clone();
        let fid_b: [u8; 16] = crypto::unhex(&job.fid).unwrap().try_into().unwrap();
        let inner = open_mailbox(|ep| (ep == 0).then_some(phone.key), &topic, "c2p", &fid_b, &obj, NOW / 1000).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&inner).unwrap(), json!({"t":"text","text":"while you were away"}));
        eng.transfer_finished(&mut host, &job.fid, Ok(None), NOW);
        let src = dir.join("a.pdf");
        std::fs::write(&src, b"pdf").unwrap();
        let (size, sha) = transfer::hash_file(&src).unwrap();
        let (fid, out) = eng.send_file(&mut host, &id, src, "a.pdf", size, sha, NOW).unwrap();
        let job = upload_jobs(&out).pop().unwrap();
        assert!(job.mailbox.is_some() && job.plan.is_some());
        rt().block_on(transfer::send(&blobs, &job, |_| {})).unwrap();
        eng.transfer_finished(&mut host, &fid, Ok(None), NOW);
        assert!(host.events.iter().any(|(e, p)| e == "phone-link-transfer" && p["fid"] == fid.as_str() && p["state"] == "queued"));
        let queued: Vec<&str> = host.audit.iter().filter(|e| e.result == "queued").map(|e| e.op.as_str()).collect();
        assert_eq!(queued, vec!["text", "file"]);
        assert!(host.audit.iter().all(|e| !e.extra.to_string().contains("while you were away")), "audit never keeps text");

        // Phone to an offline PC: a text and a file in the p2c mailbox.
        let now_s = (NOW / 1000) as u64;
        let tfid = [3u8; 16];
        let text_obj = seal_mailbox(&phone.key, 0, &topic, "p2c", &tfid, now_s, &json!({"t":"text","text":"note"}));
        let out = eng.mailbox_item(&mut host, &topic, &crypto::hex(&tfid), &text_obj, NOW);
        assert_eq!(host.texts, vec!["note".to_string()]);
        assert!(out.contains(&Action::Discard { topic: topic.clone(), dir: "p2c", fid: crypto::hex(&tfid), chunks: 0, mailbox: true }));
        let out = eng.mailbox_item(&mut host, &topic, &crypto::hex(&tfid), &text_obj, NOW + 1000);
        assert_eq!(host.texts.len(), 1, "a replayed mailbox item is not delivered twice");
        assert!(matches!(out.as_slice(), [Action::Discard { chunks: 0, .. }]));

        let data = b"photo bytes".to_vec();
        let plan = phone_upload(&blobs, &topic, "p.jpg", &data);
        let obj = seal_mailbox(&phone.key, 0, &topic, "p2c", &plan.fid, now_s, &serde_json::to_value(plan.offer()).unwrap());
        blobs.objects.lock().unwrap().insert(mbox_path(&topic, "p2c", &plan.fid_hex()), obj.clone());
        let out = eng.mailbox_item(&mut host, &topic, &plan.fid_hex(), &obj, NOW);
        let job = receive_jobs(&out).pop().expect("mailbox receive");
        assert!(job.mailbox);
        let path = rt().block_on(transfer::receive(&blobs, &job, |_| {})).unwrap();
        eng.transfer_finished(&mut host, &plan.fid_hex(), Ok(Some(path.clone())), NOW);
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert!(host.processed.contains(&plan.fid_hex()));
        let released = blobs.released.lock().unwrap().clone();
        assert!(released.contains(&format!("p2c/{}/blob", plan.fid_hex())) && released.contains(&format!("p2c/{}/mailbox", plan.fid_hex())));
        assert!(!blobs.objects.lock().unwrap().keys().any(|k| k.contains("/p2c/")), "every p2c object removed");

        // An item older than 24 h, or under a key the PC never had, is dropped unread.
        let old = seal_mailbox(&phone.key, 0, &topic, "p2c", &[4; 16], now_s - 25 * 3600, &json!({"t":"text","text":"old"}));
        eng.mailbox_item(&mut host, &topic, &crypto::hex(&[4u8; 16]), &old, NOW);
        let forged = seal_mailbox(&[0; 32], 0, &topic, "p2c", &[5; 16], now_s, &json!({"t":"text","text":"forged"}));
        eng.mailbox_item(&mut host, &topic, &crypto::hex(&[5u8; 16]), &forged, NOW);
        assert_eq!(host.texts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn open_clip(phone: &Phone, job: &SendJob, now_s: i64) -> Value {
        let fid_b: [u8; 16] = crypto::unhex(&job.fid).unwrap().try_into().unwrap();
        let inner = open_mailbox(|ep| (ep == phone.ep).then_some(phone.key), &phone.topic, "c2p", &fid_b, job.mailbox.as_ref().unwrap(), now_s).unwrap();
        serde_json::from_slice(&inner).unwrap()
    }

    #[test]
    fn clipboard_text_goes_live_only_to_phones_allowed_to_get_it() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let out = eng.send_clip("copied on pc", &mut host, NOW + 11_000);
        let got: Vec<Value> = sends(&out, "m").iter().filter_map(|p| phone.open(p)).collect();
        assert_eq!(got[0], json!({"t":"clip","text":"copied on pc","at":NOW + 11_000}));
        let audit = host.audit.iter().find(|e| e.op == "clip").expect("audited");
        assert_eq!((audit.result.as_str(), audit.dir, audit.extra["len"].as_u64()), ("ok", Some("c2p"), Some(12)));
        assert!(!serde_json::to_string(&host.audit).unwrap().contains("copied on pc"), "never the content");

        let id = eng.devices()[0].id.clone();
        eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 12_000, &mut host).unwrap();
        assert!(eng.send_clip("not allowed", &mut host, NOW + 13_000).is_empty());
        // Missing from an older device record: off.
        eng.set_perms(&id, serde_json::from_value(json!({"textOut": true})).unwrap(), NOW + 14_000, &mut host).unwrap();
        assert!(eng.send_clip("not allowed", &mut host, NOW + 15_000).is_empty());
    }

    #[test]
    fn oversized_clipboard_text_is_not_sent() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        assert!(eng.send_clip("", &mut host, NOW + 11_000).is_empty());
        assert!(eng.send_clip(&"a".repeat(remote::MAX_TEXT + 1), &mut host, NOW + 11_000).is_empty());
        // Within the text cap but the escaped JSON would not fit one sealed message.
        assert!(eng.send_clip(&"\"".repeat(remote::MAX_TEXT - 10), &mut host, NOW + 11_000).is_empty());
        assert!(!Engine::clip_fits(&"\n".repeat(remote::MAX_TEXT / 2)), "escaped JSON over MAX_PLAIN");
        assert!(Engine::clip_fits(&"a".repeat(remote::MAX_TEXT - 100)));
        assert!(!sends(&eng.send_clip(&"a".repeat(remote::MAX_TEXT - 100), &mut host, NOW + 11_000), "m").is_empty());
    }

    #[test]
    fn offline_clipboard_text_replaces_the_one_waiting_in_the_mailbox() {
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let now_s = NOW / 1000;
        let first = upload_jobs(&eng.send_clip("one", &mut host, NOW)).pop().expect("mailbox upload");
        assert!(first.plan.is_none() && matches!(first.source, Source::None));
        assert_eq!(open_clip(&phone, &first, now_s), json!({"t":"clip","text":"one","at":NOW}));
        assert!(eng.transfers().is_empty(), "the slot does not take a transfer place");

        // Copies during the upload wait; only the newest goes after it, into the same object.
        assert!(eng.send_clip("two", &mut host, NOW + 1_000).is_empty());
        assert!(eng.send_clip("three", &mut host, NOW + 2_000).is_empty());
        let next = upload_jobs(&eng.transfer_finished(&mut host, &first.fid, Ok(None), NOW + 3_000)).pop().expect("newest copy");
        assert_eq!(next.fid, first.fid);
        assert_eq!(open_clip(&phone, &next, now_s + 3)["text"], "three");
        assert!(eng.transfer_finished(&mut host, &next.fid, Ok(None), NOW + 4_000).is_empty());
        let later = upload_jobs(&eng.send_clip("four", &mut host, NOW + 5_000)).pop().unwrap();
        assert_eq!(later.fid, first.fid, "overwritten in place, never queued");
        assert_eq!(host.audit.iter().filter(|e| e.op == "clip" && e.result == "queued").count(), 2);

        // A failed upload drops the slot; the next copy starts a new one.
        eng.transfer_finished(&mut host, &later.fid, Err(XferError::Network), NOW + 6_000);
        assert!(host.audit.iter().any(|e| e.op == "clip" && e.result == "network"));
        let fresh = upload_jobs(&eng.send_clip("five", &mut host, NOW + 7_000)).pop().unwrap();
        assert_ne!(fresh.fid, first.fid);
        // Once the new copy is stored, the object the failed upload may have left goes.
        let out = eng.transfer_finished(&mut host, &fresh.fid, Ok(None), NOW + 8_000);
        assert_eq!(out, vec![clip_discard(&phone, &first.fid)]);
    }

    fn clip_discard(phone: &Phone, fid: &str) -> Action {
        Action::Discard { topic: phone.topic.clone(), dir: "c2p", fid: fid.into(), chunks: 0, mailbox: true }
    }

    fn discards(out: &[Action]) -> Vec<String> {
        out.iter()
            .filter_map(|a| match a {
                Action::Discard { fid, .. } => Some(fid.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_new_slot_fid_deletes_the_old_object_once_the_newer_copy_is_stored() {
        // Phone closed all day while the PC's relay link drops between copies (sleep, Wi-Fi):
        // the copies must not pile up in the 5-item mailbox and block explicit sends.
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let mut prev = upload_jobs(&eng.send_clip("copy 0", &mut host, NOW)).pop().unwrap();
        assert!(eng.transfer_finished(&mut host, &prev.fid, Ok(None), NOW + 500).is_empty());
        for i in 1..6 {
            let t = NOW + i * 60_000;
            eng.link_lost(&mut host, t - 1_000);
            let out = eng.send_clip(&format!("copy {i}"), &mut host, t);
            let job = upload_jobs(&out).pop().unwrap();
            assert_ne!(job.fid, prev.fid, "the phone may have read the old one");
            assert!(discards(&out).is_empty(), "the old copy stays until the newer one is stored");
            assert_eq!(eng.transfer_finished(&mut host, &job.fid, Ok(None), t + 500), vec![clip_discard(&phone, &prev.fid)]);
            prev = job;
        }
    }

    #[test]
    fn a_retired_slot_with_a_waiting_copy_deletes_the_old_object_after_the_new_one() {
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        assert!(eng.send_clip("b", &mut host, NOW + 1_000).is_empty());
        eng.link_lost(&mut host, NOW + 1_500);
        let b = upload_jobs(&eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 2_000)).pop().unwrap();
        assert_ne!(b.fid, a.fid);
        assert_eq!(eng.transfer_finished(&mut host, &b.fid, Ok(None), NOW + 3_000), vec![clip_discard(&phone, &a.fid)]);
    }

    #[test]
    fn a_slot_fid_is_replaced_before_the_relay_sweep_takes_its_object() {
        // The relay sweeps an object 24 h after it was created and an overwrite keeps that time.
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 1_000);
        let same = upload_jobs(&eng.send_clip("b", &mut host, NOW + (CLIP_FID_SECS - 60) * 1000)).pop().unwrap();
        assert_eq!(same.fid, a.fid);
        eng.transfer_finished(&mut host, &same.fid, Ok(None), NOW + (CLIP_FID_SECS - 59) * 1000);
        let fresh = upload_jobs(&eng.send_clip("c", &mut host, NOW + CLIP_FID_SECS * 1000)).pop().unwrap();
        assert_ne!(fresh.fid, a.fid);
        assert_eq!(eng.transfer_finished(&mut host, &fresh.fid, Ok(None), NOW + CLIP_FID_SECS * 1000 + 500), vec![clip_discard(&phone, &a.fid)]);
    }

    #[test]
    fn slot_fids_of_an_earlier_run_are_deleted_after_the_first_stored_copy() {
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("before restart", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 500);
        assert_eq!(host.clip_fids.get(&id), Some(&vec![a.fid.clone()]), "kept across restarts");

        let mut eng = Engine::new(Box::new(MemoryDevices(eng.devices.clone())), "TEST-PC");
        let b = upload_jobs(&eng.send_clip("after restart", &mut host, NOW + 60_000)).pop().unwrap();
        assert_ne!(b.fid, a.fid);
        assert_eq!(eng.transfer_finished(&mut host, &b.fid, Ok(None), NOW + 61_000), vec![clip_discard(&phone, &a.fid)]);
        assert_eq!(host.clip_fids.get(&id), Some(&vec![b.fid.clone()]));
        eng.revoke(&mut host, &id, "ok", NOW + 62_000);
        assert!(host.clip_fids.get(&id).is_none_or(|f| f.is_empty()), "forgotten with the phone");
    }

    #[test]
    fn a_session_that_starts_during_a_slot_upload_gets_the_newest_copy_live() {
        // The phone lists its mailbox while it connects; a copy landing after that would be
        // deleted unread (its fid is processed), so it also goes over the new session.
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("old", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 500);
        let b = upload_jobs(&eng.send_clip("in flight", &mut host, NOW + 60_000)).pop().unwrap();
        assert!(eng.send_clip("newest", &mut host, NOW + 60_500).is_empty());
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 61_000);
        let pos = |ev: &str| out.iter().position(|x| matches!(x, Action::Send { event, .. } if *event == ev));
        assert!(pos("hs2") < pos("m"), "hs2 goes first");
        let clips: Vec<Value> = opened(&mut phone, &out).into_iter().filter(|v| v["t"] == "clip").collect();
        assert_eq!(clips, vec![json!({"t":"clip","text":"newest","at":NOW + 60_500,"late":true})]);
        // The waiting copy went live, so nothing more is uploaded; the next offline copy takes a new fid.
        assert!(upload_jobs(&eng.transfer_finished(&mut host, &b.fid, Ok(None), NOW + 62_000)).is_empty());
    }

    #[test]
    fn a_session_right_after_a_slot_upload_gets_that_copy_live_but_not_an_old_one() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("just stored", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 1_000);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + (1 + CLIP_RESEND_SECS) * 1000);
        let clips: Vec<Value> = opened(&mut phone, &out).into_iter().filter(|v| v["t"] == "clip").collect();
        assert_eq!(clips, vec![json!({"t":"clip","text":"just stored","at":NOW,"late":true})]);

        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("stored long ago", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 1_000);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + (2 + CLIP_RESEND_SECS) * 1000);
        assert!(opened(&mut phone, &out).iter().all(|v| v["t"] != "clip"), "the phone reads it from the mailbox");
    }

    #[test]
    fn clipboard_copies_are_stamped_in_milliseconds_that_never_go_back() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let ats: Vec<i64> = [NOW + 11_000, NOW + 11_300, NOW + 9_000]
            .iter()
            .map(|t| opened(&mut phone, &eng.send_clip(&format!("c{t}"), &mut host, *t))[0]["at"].as_i64().unwrap())
            .collect();
        assert_eq!(ats, vec![NOW + 11_000, NOW + 11_300, NOW + 11_301], "a clock set back does not make a copy look older");
    }

    #[test]
    fn a_session_or_a_lost_link_gives_the_clipboard_slot_a_new_fid() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 500);
        // The phone connects (and reads its mailbox): a copy during the session goes live.
        connect(&mut eng, &mut host, &mut phone, NOW + 1_000);
        let out = eng.send_clip("live", &mut host, NOW + 2_000);
        assert!(upload_jobs(&out).is_empty());
        assert_eq!(opened(&mut phone, &out)[0]["text"], "live");
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"ping"}), NOW + 5_000);
        let topic = phone.topic.clone();
        let m = phone.seal(json!({"t":"bye"}));
        eng.on_broadcast(&mut host, &topic, "m", &m, NOW + 6_000);
        let b = upload_jobs(&eng.send_clip("b", &mut host, NOW + 7_000)).pop().unwrap();
        assert_ne!(b.fid, a.fid, "the phone may have read the old slot");

        // The relay link drops during an upload: that upload still reports, then a new fid.
        assert!(eng.send_clip("c", &mut host, NOW + 8_000).is_empty());
        eng.link_lost(&mut host, NOW + 8_500);
        let c = upload_jobs(&eng.transfer_finished(&mut host, &b.fid, Ok(None), NOW + 9_000)).pop().expect("the waiting copy");
        assert_ne!(c.fid, b.fid, "the waiting copy starts a new slot");
        eng.transfer_finished(&mut host, &c.fid, Ok(None), NOW + 10_000);
        let d = upload_jobs(&eng.send_clip("d", &mut host, NOW + 11_000)).pop().unwrap();
        assert_eq!(d.fid, c.fid);
    }

    #[test]
    fn revoking_a_phone_forgets_its_clipboard_slot() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        eng.revoke(&mut host, &id, "ok", NOW + 100);
        assert!(eng.transfer_finished(&mut host, &a.fid, Err(XferError::Denied), NOW + 200).is_empty());
        assert!(eng.send_clip("b", &mut host, NOW + 300).is_empty());
    }

    // Tester round 2 (T-016).

    #[test]
    fn a_copy_waiting_for_the_slot_is_dropped_when_the_permission_goes_off() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        assert!(eng.send_clip("b", &mut host, NOW + 100).is_empty());
        eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 200, &mut host).unwrap();
        let out = eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 300);
        assert!(upload_jobs(&out).is_empty(), "the waiting copy must not go after the permission was withdrawn");
    }

    #[test]
    fn a_finished_slot_upload_of_a_revoked_phone_is_ignored_even_with_a_waiting_copy() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        assert!(eng.send_clip("b", &mut host, NOW + 100).is_empty());
        eng.revoke(&mut host, &id, "ok", NOW + 200);
        assert!(eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 300).is_empty());
        assert!(host.clip_fids.get(&id).is_none_or(|f| f.is_empty()));
    }

    #[test]
    fn a_suspended_phone_gets_no_clipboard_text() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        eng.devices[0].suspended = true;
        assert!(eng.send_clip("x", &mut host, NOW).is_empty());
    }

    #[test]
    fn withdrawing_the_permission_discards_the_copy_waiting_in_the_mailbox() {
        let (mut eng, mut host, phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("secret", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 100);
        let out = eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 200, &mut host).unwrap();
        assert_eq!(out.iter().filter(|a| matches!(a, Action::Discard { .. })).collect::<Vec<_>>(), vec![&clip_discard(&phone, &a.fid)]);
        assert!(host.clip_fids.get(&id).is_none_or(|f| f.is_empty()));
        assert!(discards(&eng.set_perms(&id, Perms::new_device(), NOW + 300, &mut host).unwrap()).is_empty());
        let b = upload_jobs(&eng.send_clip("allowed again", &mut host, NOW + 400)).pop().unwrap();
        assert_ne!(b.fid, a.fid);
    }

    #[test]
    fn withdrawing_the_permission_during_a_slot_upload_discards_its_object_when_it_ends() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("a", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 100);
        eng.link_lost(&mut host, NOW + 150);
        let b = upload_jobs(&eng.send_clip("b", &mut host, NOW + 200)).pop().unwrap();
        assert!(eng.send_clip("c", &mut host, NOW + 300).is_empty());
        let out = eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 400, &mut host).unwrap();
        assert!(discards(&out).is_empty(), "the running upload may land after a delete");
        let out = eng.transfer_finished(&mut host, &b.fid, Ok(None), NOW + 500);
        assert!(upload_jobs(&out).is_empty(), "the waiting copy is dropped");
        let mut gone = discards(&out);
        gone.sort();
        let mut want = vec![a.fid.clone(), b.fid.clone()];
        want.sort();
        assert_eq!(gone, want);
        assert!(host.clip_fids.get(&id).is_none_or(|f| f.is_empty()));
    }

    #[test]
    fn a_copy_withdrawn_during_its_upload_is_not_sent_live_after_the_permission_comes_back() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("withdrawn", &mut host, NOW)).pop().unwrap();
        eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 100, &mut host).unwrap();
        eng.set_perms(&id, Perms::new_device(), NOW + 200, &mut host).unwrap();
        assert!(eng.send_clip("allowed again", &mut host, NOW + 250).is_empty(), "waits for the running upload");
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 300);
        let texts: Vec<Value> = clips_in(&mut phone, &out).into_iter().map(|c| c["text"].clone()).collect();
        assert_eq!(texts, vec![json!("allowed again")]);
        assert_eq!(discards(&eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 400)), vec![a.fid.clone()]);
    }

    #[test]
    fn turning_clipboard_sync_off_discards_every_waiting_copy_also_from_an_earlier_run() {
        let (mut eng, mut host, _phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        let a = upload_jobs(&eng.send_clip("before restart", &mut host, NOW)).pop().unwrap();
        eng.transfer_finished(&mut host, &a.fid, Ok(None), NOW + 100);
        let mut eng = Engine::new(Box::new(MemoryDevices(eng.devices.clone())), "TEST-PC");
        assert_eq!(discards(&eng.drop_clips(&mut host)), vec![a.fid.clone()]);
        assert!(host.clip_fids.get(&id).is_none_or(|f| f.is_empty()));
        assert!(eng.drop_clips(&mut host).is_empty());
    }

    fn clips_in(phone: &mut Phone, out: &[Action]) -> Vec<Value> {
        opened(phone, out).into_iter().filter(|v| v["t"] == "clip").collect()
    }

    #[test]
    fn a_live_copy_the_phone_never_answered_comes_again_on_its_next_session() {
        // The phone was swiped out of recents: the PC keeps the session until it idles out.
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let out = eng.send_clip("copied after the phone left", &mut host, NOW + 100_000);
        assert!(upload_jobs(&out).is_empty(), "the PC still has the session");
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 120_000);
        // Late: made before this session, so the phone offers it instead of writing its clipboard (I-319).
        assert_eq!(clips_in(&mut phone, &out), vec![json!({"t":"clip","text":"copied after the phone left","at":NOW + 100_000,"late":true})]);
        // Unanswered again: it comes once more on the session after that.
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 130_000);
        assert_eq!(clips_in(&mut phone, &out).len(), 1);
    }

    #[test]
    fn a_live_copy_the_phone_answered_afterwards_is_not_sent_again() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("seen", &mut host, NOW + 100_000);
        // A ping that crossed the copy proves nothing.
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"ping"}), NOW + 101_000);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 102_000);
        assert_eq!(clips_in(&mut phone, &out).len(), 1);
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"ping"}), NOW + 106_000);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 110_000);
        assert!(clips_in(&mut phone, &out).is_empty());
        let out = eng.tick(&mut host, NOW + (110 + SESSION_IDLE_SECS + 1) * 1000);
        assert!(!eng.has_sessions());
        assert!(upload_jobs(&out).is_empty());
    }

    #[test]
    fn an_unanswered_live_copy_goes_to_the_mailbox_when_the_session_idles_out() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("old", &mut host, NOW + 90_000);
        eng.send_clip("newest", &mut host, NOW + 100_000);
        let out = eng.tick(&mut host, NOW + (10 + SESSION_IDLE_SECS + 1) * 1000);
        assert!(!eng.has_sessions());
        let jobs = upload_jobs(&out);
        assert_eq!(jobs.len(), 1, "only the newest copy");
        assert_eq!(open_clip(&phone, &jobs[0], NOW / 1000 + 200), json!({"t":"clip","text":"newest","at":NOW + 100_000}));
        eng.transfer_finished(&mut host, &jobs[0].fid, Ok(None), NOW + 200_000);
        // Read from the mailbox on the next session, not sent live a second time.
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 400_000);
        assert!(clips_in(&mut phone, &out).is_empty());
    }

    #[test]
    fn an_unanswered_live_copy_goes_to_the_mailbox_when_the_phone_says_bye() {
        // The PWA says bye when its page goes away; the copy may have reached a hidden page
        // whose inbox is now gone. The bye itself does not count as an answer.
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("while hidden", &mut host, NOW + 20_000);
        let m = phone.seal(json!({"t":"bye"}));
        let out = eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 50_000);
        assert!(!eng.has_sessions());
        let job = upload_jobs(&out).pop().expect("the copy waits in the mailbox");
        assert_eq!(open_clip(&phone, &job, NOW / 1000 + 50)["text"], "while hidden");

        // Answered before the bye: nothing more to do.
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("seen", &mut host, NOW + 20_000);
        cmd(&mut eng, &mut host, &mut phone, json!({"t":"ping"}), NOW + 30_000);
        let m = phone.seal(json!({"t":"bye"}));
        assert!(upload_jobs(&eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 40_000)).is_empty());
    }

    #[test]
    fn a_lost_relay_link_puts_an_unanswered_live_copy_in_the_mailbox() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("just before the loss", &mut host, NOW + 20_000);
        let job = upload_jobs(&eng.link_lost(&mut host, NOW + 21_000)).pop().expect("deferred until the link is back");
        assert_eq!(open_clip(&phone, &job, NOW / 1000 + 21)["text"], "just before the loss");
        // The phone comes back while that upload runs: the copy also goes over the new session.
        let topic = phone.topic.clone();
        eng.relinked(&topic);
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 30_000);
        assert_eq!(clips_in(&mut phone, &out).len(), 1);
    }

    #[test]
    fn a_live_copy_is_not_resent_once_the_permission_is_withdrawn() {
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        let id = eng.devices()[0].id.clone();
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        eng.send_clip("x", &mut host, NOW + 20_000);
        eng.set_perms(&id, Perms { clipboard_out: false, ..Perms::new_device() }, NOW + 21_000, &mut host).unwrap();
        let out = connect(&mut eng, &mut host, &mut phone, NOW + 30_000);
        assert!(clips_in(&mut phone, &out).is_empty());
        assert!(upload_jobs(&eng.tick(&mut host, NOW + (30 + SESSION_IDLE_SECS + 1) * 1000)).is_empty());
    }
}
