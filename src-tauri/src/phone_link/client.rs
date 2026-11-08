// Relay client (design sections 2.3, 5.3, 8, 9): proxy-aware connection, anonymous auth with
// a persisted refresh token, owner check and rebind, Realtime channels, heartbeats, token
// refresh, pending relay cleanup and reconnect with backoff. All protocol decisions are in
// the engine; this file only moves bytes and calls RPCs.

use super::audit::AuditLog;
use super::config::{relay, RelayConfig};
use super::crypto::{self, hex, Key};
use super::engine::{Action, CodeOutcome, Engine};
use super::host::{self, AppHost};
use super::net::{self, NetError, Route};
use super::remote::Perms;
use super::secrets;
use super::store;
use super::supabase::{self, AuthSession, Frame, Rest, RestError};
use super::transfer::{self, Blobs, XferError};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

pub type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Cmd {
    StartPairing(Reply<PairingView>),
    CancelPairing,
    SubmitCode { code: String, perms: Perms, reply: Reply<Value> },
    Apply(Vec<Action>),
    Reconnect,
    Shutdown,
    /// A transfer task ended (the saved path for a received file).
    TransferDone { fid: String, result: Result<Option<PathBuf>, XferError> },
    /// An object from a device topic's incoming mailbox.
    MailboxItem { topic: String, fid: String, bytes: Vec<u8> },
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    pub qr_svg: String,
    pub expires_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StateInfo {
    pub state: &'static str,
    pub detail: Option<String>,
}

pub static STATE: Mutex<StateInfo> = Mutex::new(StateInfo { state: "off", detail: None });

pub fn set_state(app: &AppHandle, state: &'static str, detail: Option<String>) {
    let info = StateInfo { state, detail };
    let changed = STATE.lock().map(|mut s| {
        let changed = *s != info;
        *s = info.clone();
        changed
    });
    if changed.unwrap_or(true) {
        let _ = app.emit("phone-link-state", &info);
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn now_s() -> i64 {
    now_ms() / 1000
}

pub fn bytea(bytes: &[u8]) -> String {
    format!("\\x{}", hex(bytes))
}

pub struct Ctx {
    pub app: AppHandle,
    pub engine: Arc<Mutex<Engine>>,
    pub audit: AuditLog,
    pub config_dir: PathBuf,
    /// The client's own command channel, for transfer tasks to report back.
    pub tx: mpsc::UnboundedSender<Cmd>,
    /// Running transfer tasks: fid to (topic, stop flag).
    pub tasks: Mutex<HashMap<String, (String, Arc<AtomicBool>)>>,
    /// Uploads and discards that came while offline, run on the next connection.
    pub deferred: Mutex<Vec<Action>>,
}

impl Ctx {
    fn with_engine<T>(&self, f: impl FnOnce(&mut Engine, &mut AppHost) -> T) -> T {
        let mut host = AppHost { app: &self.app, audit: &self.audit };
        let mut eng = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut eng, &mut host)
    }

    fn track(&self, fid: &str, topic: &str) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut t) = self.tasks.lock() {
            t.insert(fid.to_string(), (topic.to_string(), stop.clone()));
        }
        stop
    }

    fn untrack(&self, fid: &str) {
        if let Ok(mut t) = self.tasks.lock() {
            t.remove(fid);
        }
    }

    /// Stops the transfer tasks of a removed device (all of them with `None`): nothing they
    /// were doing is saved or uploaded afterwards.
    fn cancel(&self, topic: Option<&str>) {
        if let Ok(t) = self.tasks.lock() {
            for (tt, stop) in t.values() {
                if topic.is_none_or(|x| x == tt) {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        }
        if let (Some(topic), Ok(mut d)) = (topic, self.deferred.lock()) {
            d.retain(|a| !matches!(a, Action::Upload(j) if j.topic == topic));
        }
    }

    pub fn add_pending_cleanup(&self, topic: &str) {
        let mut cfg = store::load(&self.config_dir);
        if !cfg.pending_cleanup.iter().any(|t| t == topic) {
            cfg.pending_cleanup.push(topic.to_string());
            let _ = store::save(&self.config_dir, &cfg);
        }
    }
}

enum Failure {
    Stop,
    Retry { state: &'static str, detail: String, wait: Option<Duration> },
}

fn retry(state: &'static str, detail: impl Into<String>) -> Failure {
    Failure::Retry { state, detail: detail.into(), wait: None }
}

fn rest_failure(e: RestError) -> Failure {
    match e {
        RestError::Paused => Failure::Retry { state: "relay_paused", detail: e.to_string(), wait: Some(Duration::from_secs(300)) },
        RestError::Network(d) => retry("blocked_network", d),
        other => retry("error", other.to_string()),
    }
}

/// Only a 4xx reply (a Postgres `raise`, a refused request) proves the RPC did not commit.
fn may_have_committed(e: &RestError) -> bool {
    !matches!(e, RestError::Status { code, .. } if (400..500).contains(code))
}

fn net_failure(e: NetError) -> Failure {
    if e.is_proxy_block() {
        Failure::Retry { state: "blocked_proxy", detail: e.to_string(), wait: Some(Duration::from_secs(300)) }
    } else {
        retry("blocked_network", e.to_string())
    }
}

// Auth (design 5.3): refreshes are serialized in this one task; the new refresh token is in
// Credential Manager before it is used.

async fn ensure_auth(rest: &Rest, current: Option<AuthSession>) -> Result<(AuthSession, bool), Failure> {
    let saved = current.or_else(|| secrets::read_json::<AuthSession>(secrets::SESSION_TARGET));
    if let Some(s) = saved {
        if s.expires_at - now_s() > 120 {
            return Ok((s, false));
        }
        match rest.refresh(&s.refresh_token, now_s()).await {
            Ok(fresh) => {
                secrets::write_json(secrets::SESSION_TARGET, &fresh).map_err(|e| retry("error", e))?;
                return Ok((fresh, false));
            }
            Err(RestError::Status { code, .. }) if (400..500).contains(&code) => {}
            Err(e) => return Err(rest_failure(e)),
        }
    }
    let fresh = match rest.signup_anonymous(now_s()).await {
        Ok(s) => s,
        Err(RestError::Status { message, .. }) if message.to_ascii_lowercase().contains("anonymous") => {
            return Err(Failure::Retry { state: "error", detail: "anonymous_disabled".into(), wait: Some(Duration::from_secs(300)) });
        }
        Err(e) => return Err(rest_failure(e)),
    };
    secrets::write_json(secrets::SESSION_TARGET, &fresh).map_err(|e| retry("error", e))?;
    Ok((fresh, true))
}

fn stored_owner_key() -> Option<Zeroizing<Key>> {
    secrets::read(secrets::OWNER_TARGET).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()).map(Zeroizing::new)
}

/// K_owner, created on first use. A key that did not reach Credential Manager is never used:
/// claiming with it would leave an owner nobody can rebind after a restart.
fn owner_key() -> Result<Zeroizing<Key>, String> {
    if let Some(k) = stored_owner_key() {
        return Ok(k);
    }
    let k = Zeroizing::new(crypto::random_bytes::<32>());
    secrets::write(secrets::OWNER_TARGET, k.as_ref())?;
    match stored_owner_key() {
        Some(back) if *back == *k => Ok(k),
        _ => Err("owner key was not stored".into()),
    }
}

/// Owner check through `link_owner_check`, which has no side effects (an open pairing row survives).
async fn ensure_owner(rest: &Rest, auth: &AuthSession) -> Result<(), Failure> {
    match rest.rpc(&auth.access_token, "link_owner_check", json!({})).await {
        Ok(_) => return Ok(()),
        Err(e) if e.message() == "not owner" => {}
        Err(e) => return Err(rest_failure(e)),
    }
    let had_key = secrets::read(secrets::OWNER_TARGET).is_some();
    let k = owner_key().map_err(|e| Failure::Retry { state: "error", detail: e, wait: Some(Duration::from_secs(60)) })?;
    if had_key {
        let ts = now_s();
        let mac = crypto::owner_mac(&k, &auth.user_id, ts);
        match rest.rpc(&auth.access_token, "link_owner_rebind", json!({ "ts": ts, "mac": bytea(&mac) })).await {
            Ok(_) => return Ok(()),
            Err(e) if e.message() == "bad mac" => {}
            Err(e) => return Err(rest_failure(e)),
        }
    }
    // First run: the claim is open only while the owner claim procedure runs (T-010 log).
    match rest.rpc(&auth.access_token, "link_owner_claim", json!({ "k": bytea(k.as_ref()) })).await {
        Ok(_) => Ok(()),
        Err(e) => Err(Failure::Retry { state: "unclaimed", detail: e.to_string(), wait: Some(Duration::from_secs(60)) }),
    }
}

async fn cleanup_topic(rest: &Rest, token: &str, topic: &str) -> Result<(), RestError> {
    let files = rest.storage_walk(token, topic).await?;
    rest.storage_delete(token, &files).await?;
    rest.rpc(token, "link_remove_device", json!({ "t": topic })).await.map(|_| ())
}

async fn run_pending_cleanup(ctx: &Ctx, rest: &Rest, token: &str) {
    let cfg = store::load(&ctx.config_dir);
    for topic in cfg.pending_cleanup.clone() {
        match cleanup_topic(rest, token, &topic).await {
            Ok(()) => {
                let mut cfg = store::load(&ctx.config_dir);
                cfg.pending_cleanup.retain(|t| t != &topic);
                let _ = store::save(&ctx.config_dir, &cfg);
                let _ = ctx.app.emit("phone-link-cleanup", json!({ "pending": cfg.pending_cleanup.len() }));
            }
            Err(e) => crate::diagnostics::log(&format!("phone link: relay cleanup failed: {e}")),
        }
    }
}

/// Storage for transfers over the relay (design 7.1): `link-upload` for URLs and releases,
/// signed PUTs, reads and deletes under the user token.
#[derive(Clone)]
pub struct RelayBlobs {
    pub rest: Rest,
    pub token: Arc<Mutex<String>>,
}

impl RelayBlobs {
    fn token(&self) -> String {
        self.token.lock().map(|t| t.clone()).unwrap_or_default()
    }
}

pub fn xfer_error(e: &RestError) -> XferError {
    match e {
        RestError::Network(_) | RestError::Paused => XferError::Network,
        RestError::Status { code: 403, .. } => XferError::Denied,
        RestError::Status { code: 429 | 507, .. } => XferError::Busy,
        RestError::Status { code, .. } if *code >= 500 => XferError::Network,
        RestError::Status { .. } => XferError::Invalid,
    }
}

impl Blobs for RelayBlobs {
    async fn authorize(&self, topic: &str, dir: &str, fid: &str, count: u32, kind: &str) -> Result<Vec<String>, XferError> {
        let body = json!({ "topic": topic, "dir": dir, "fid": fid, "count": count, "kind": kind });
        let v = self.rest.function(&self.token(), "link-upload", body).await.map_err(|e| xfer_error(&e))?;
        let uploads = v.get("uploads").and_then(Value::as_array).ok_or(XferError::Network)?;
        let prefix = format!("{topic}/");
        uploads
            .iter()
            .map(|u| {
                let path = u.get("path").and_then(Value::as_str).filter(|p| p.starts_with(&prefix))?;
                self.rest.signed_upload_url(path, u.get("token").and_then(Value::as_str)?)
            })
            .collect::<Option<Vec<String>>>()
            .ok_or(XferError::Network)
    }
    async fn put(&self, url: &str, body: Vec<u8>) -> Result<(), XferError> {
        self.rest.signed_put(url, body).await.map_err(|e| xfer_error(&e))
    }
    async fn get(&self, path: &str) -> Result<Vec<u8>, XferError> {
        self.rest.storage_get(&self.token(), path).await.map_err(|e| xfer_error(&e))
    }
    async fn delete(&self, paths: Vec<String>) -> Result<(), XferError> {
        self.rest.storage_delete(&self.token(), &paths).await.map_err(|e| xfer_error(&e))
    }
    async fn release(&self, topic: &str, dir: &str, fid: &str, kind: &str) -> Result<(), XferError> {
        let body = json!({ "op": "release", "topic": topic, "dir": dir, "fid": fid, "kind": kind });
        self.rest.function(&self.token(), "link-upload", body).await.map(|_| ()).map_err(|e| xfer_error(&e))
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, XferError> {
        let items = self.rest.storage_list(&self.token(), prefix).await.map_err(|e| xfer_error(&e))?;
        Ok(items.into_iter().filter(|(_, file)| *file).filter_map(|(p, _)| p.rsplit('/').next().map(str::to_string)).collect())
    }
}

/// `phone-link-transfer` progress for one transfer (at most once per 4 MiB chunk).
fn progress(app: AppHandle, fid: String, device: String, dir: &'static str, name: String, size: u64, state: &'static str) -> impl FnMut(u64) + Send {
    move |done| {
        let _ = app.emit("phone-link-transfer", json!({ "fid": fid, "deviceId": device, "dir": dir, "name": name, "size": size, "done": done, "state": state }));
    }
}

const MAILBOX_SCAN_MS: i64 = 10 * 60 * 1000;
const MAILBOX_SCAN_MAX: usize = 20;

async fn scan_mailbox(b: RelayBlobs, tx: mpsc::UnboundedSender<Cmd>, topic: String) {
    let Ok(names) = b.list(&transfer::mbox_prefix(&topic, "p2c")).await else { return };
    for fid in names.into_iter().filter(|f| transfer::is_fid(f)).take(MAILBOX_SCAN_MAX) {
        if let Ok(bytes) = b.get(&transfer::mbox_path(&topic, "p2c", &fid)).await {
            if bytes.len() <= transfer::MAILBOX_MAX_BYTES && tx.send(Cmd::MailboxItem { topic: topic.clone(), fid, bytes }).is_err() {
                return;
            }
        }
    }
}

struct Conn {
    ws: net::WsStream,
    next_ref: u64,
    joined: HashMap<String, u64>,
    heartbeat_pending: Option<u64>,
}

impl Conn {
    fn bump(&mut self) -> u64 {
        self.next_ref += 1;
        self.next_ref
    }

    async fn send(&mut self, text: String) -> Result<(), Failure> {
        self.ws.send(Message::text(text)).await.map_err(|e| retry("blocked_network", e.to_string()))
    }

    async fn join(&mut self, topic: &str, token: &str) -> Result<(), Failure> {
        if self.joined.contains_key(topic) {
            return Ok(());
        }
        let r = self.bump();
        self.joined.insert(topic.to_string(), r);
        self.send(supabase::join(topic, r, true, "pc", Some(token))).await
    }

    async fn leave(&mut self, topic: &str) -> Result<(), Failure> {
        let Some(j) = self.joined.remove(topic) else { return Ok(()) };
        let r = self.bump();
        self.send(supabase::leave(topic, r, j)).await
    }

    async fn broadcast(&mut self, topic: &str, event: &str, payload: Value) -> Result<(), Failure> {
        let Some(&j) = self.joined.get(topic) else { return Ok(()) };
        let r = self.bump();
        self.send(supabase::broadcast(topic, event, payload, r, j)).await
    }
}

/// The topic whose current `phx_join` this ok reply answers.
fn join_confirmed(joined: &HashMap<String, u64>, f: &Frame) -> Option<String> {
    let topic = f.topic.strip_prefix("realtime:")?;
    let r: u64 = f.reference.as_deref()?.parse().ok()?;
    (joined.get(topic) == Some(&r)).then(|| topic.to_string())
}

struct Online<'a> {
    ctx: &'a Ctx,
    rest: Rest,
    auth: AuthSession,
    conn: Conn,
    last_status_push: i64,
    token: Arc<Mutex<String>>,
    last_scan: i64,
}

impl Online<'_> {
    async fn apply(&mut self, actions: Vec<Action>) -> Result<(), Failure> {
        let mut rest = actions.into_iter();
        while let Some(action) = rest.next() {
            if let Err(f) = self.apply_one(action).await {
                // The socket failed: the actions after it are kept like any offline work, so an
                // upload or relay cleanup for another device is not lost with it.
                keep_cleanup(self.ctx, rest.collect());
                return Err(f);
            }
        }
        Ok(())
    }

    async fn apply_one(&mut self, action: Action) -> Result<(), Failure> {
        match action {
            Action::Send { topic, event, payload } => self.conn.broadcast(&topic, event, payload).await?,
            Action::Join(topic) => self.conn.join(&topic, &self.auth.access_token).await?,
            Action::Leave(topic) => self.conn.leave(&topic).await?,
            Action::CancelPairing => {
                let _ = self.rest.rpc(&self.auth.access_token, "link_cancel_pairing", json!({})).await;
            }
            Action::RelayCleanup(topic) => {
                self.ctx.cancel(Some(&topic));
                self.ctx.add_pending_cleanup(&topic);
                run_pending_cleanup(self.ctx, &self.rest, &self.auth.access_token).await;
            }
            Action::Receive(job) => {
                let (b, tx) = (self.blobs(), self.ctx.tx.clone());
                let fid = job.plan.fid_hex();
                let report = progress(self.ctx.app.clone(), fid.clone(), job.device_id.clone(), "p2c", job.plan.name.clone(), job.plan.size, "receiving");
                let stop = self.ctx.track(&fid, &job.topic);
                tokio::spawn(async move {
                    let result = transfer::receive_until(&b, &job, report, &stop).await.map(Some);
                    let _ = tx.send(Cmd::TransferDone { fid, result });
                });
            }
            Action::Upload(job) => {
                let (b, tx) = (self.blobs(), self.ctx.tx.clone());
                let (name, size) = job.plan.as_ref().map(|p| (p.name.clone(), p.size)).unwrap_or_default();
                let report = progress(self.ctx.app.clone(), job.fid.clone(), job.device_id.clone(), "c2p", name, size, "sending");
                let stop = self.ctx.track(&job.fid, &job.topic);
                tokio::spawn(async move {
                    let result = transfer::send_until(&b, &job, report, &stop).await.map(|_| None);
                    let _ = tx.send(Cmd::TransferDone { fid: job.fid.clone(), result });
                });
            }
            Action::Discard { topic, dir, fid, chunks, mailbox } => {
                let b = self.blobs();
                tokio::spawn(async move {
                    if let Err(e) = transfer::discard(&b, &topic, dir, &fid, chunks, mailbox).await {
                        crate::diagnostics::log(&format!("phone link: discarding a transfer failed: {e:?}"));
                    }
                });
            }
            Action::ScanMailbox(topic) => {
                tokio::spawn(scan_mailbox(self.blobs(), self.ctx.tx.clone(), topic));
            }
        }
        Ok(())
    }

    fn blobs(&self) -> RelayBlobs {
        RelayBlobs { rest: self.rest.clone(), token: self.token.clone() }
    }

    async fn scan_all(&mut self) -> Result<(), Failure> {
        self.last_scan = now_ms();
        let topics: Vec<Action> = self.ctx.with_engine(|e, _| e.device_topics()).into_iter().map(Action::ScanMailbox).collect();
        self.apply(topics).await
    }

    async fn refresh_if_needed(&mut self, min_left: i64) -> Result<(), Failure> {
        if self.auth.expires_at - now_s() > min_left {
            return Ok(());
        }
        let (fresh, new_user) = ensure_auth(&self.rest, Some(self.auth.clone())).await?;
        if new_user {
            // A new anonymous user has no memberships; reconnect to rebind ownership.
            return Err(retry("connecting", "session replaced"));
        }
        self.auth = fresh;
        if let Ok(mut t) = self.token.lock() {
            *t = self.auth.access_token.clone();
        }
        let topics: Vec<(String, u64)> = self.conn.joined.iter().map(|(t, j)| (t.clone(), *j)).collect();
        for (topic, j) in topics {
            let r = self.conn.bump();
            self.conn.send(supabase::access_token(&topic, &self.auth.access_token, r, j)).await?;
        }
        Ok(())
    }

    async fn on_cmd(&mut self, cmd: Cmd) -> Result<(), Failure> {
        match cmd {
            Cmd::Shutdown => {
                self.ctx.cancel(None);
                let topics: Vec<String> = self.conn.joined.keys().cloned().collect();
                for t in topics {
                    let _ = self.conn.leave(&t).await;
                }
                let _ = self.conn.ws.close(None).await;
                return Err(Failure::Stop);
            }
            Cmd::Reconnect => return Err(retry("connecting", "reconnect")),
            Cmd::Apply(actions) => self.apply(actions).await?,
            Cmd::TransferDone { fid, result } => {
                self.ctx.untrack(&fid);
                let actions = self.ctx.with_engine(|e, h| e.transfer_finished(h, &fid, result, now_ms()));
                self.apply(actions).await?;
            }
            Cmd::MailboxItem { topic, fid, bytes } => {
                let actions = self.ctx.with_engine(|e, h| e.mailbox_item(h, &topic, &fid, &bytes, now_ms()));
                self.apply(actions).await?;
            }
            Cmd::CancelPairing => {
                let actions = self.ctx.with_engine(|e, h| e.cancel_pairing(h, now_ms()));
                self.apply(actions).await?;
            }
            Cmd::StartPairing(reply) => {
                let cfg = relay();
                // Realtime may check the write policy again after a token refresh; keep that out of the
                // offer's lifetime so the write cached by `seen` still covers the welcome (I-161).
                self.refresh_if_needed(super::pairing::OFFER_SECS + 180).await?;
                let start = self.ctx.with_engine(|e, _| e.start_pairing(&cfg.pwa_url, &cfg.project_ref, now_ms()));
                if let Some(old) = &start.replaced_topic {
                    self.conn.leave(old).await?;
                }
                let created = self.rest.rpc(&self.auth.access_token, "link_create_pairing", json!({ "t": start.pair_topic })).await;
                if let Err(e) = created {
                    let actions = self.ctx.with_engine(|e, h| e.cancel_pairing(h, now_ms()));
                    let _ = reply.send(Err(e.to_string()));
                    return self.apply(actions).await;
                }
                self.conn.join(&start.pair_topic, &self.auth.access_token).await?;
                let view = super::pairing::qr_svg_data_uri(&start.url).map(|qr_svg| PairingView { qr_svg, expires_at: start.expires_at });
                let _ = reply.send(view);
            }
            Cmd::SubmitCode { code, perms, reply } => {
                let (outcome, actions) = self.ctx.with_engine(|e, h| e.submit_code(h, &code, now_ms()));
                self.apply(actions).await?;
                let result = match outcome {
                    CodeOutcome::Accepted => self.finish_pairing(perms).await?,
                    CodeOutcome::Wrong { left } => Ok(json!({ "ok": false, "left": left })),
                    CodeOutcome::Aborted(reason) => Ok(json!({ "ok": false, "aborted": reason })),
                    CodeOutcome::NoRequest => Err("no pairing request".to_string()),
                };
                let _ = reply.send(result);
            }
        }
        Ok(())
    }

    async fn finish_pairing(&mut self, perms: Perms) -> Result<Result<Value, String>, Failure> {
        let Some((pair_topic, device_topic, phone_uid, rk)) = self.ctx.with_engine(|e, _| e.accepted_device()) else {
            return Ok(Err("no accepted pairing".into()));
        };
        let args = json!({ "pair_topic": pair_topic, "t": device_topic, "phone": phone_uid, "rk": bytea(&rk) });
        match self.rest.rpc(&self.auth.access_token, "link_create_device", args).await {
            Ok(_) => {
                let done = self.ctx.with_engine(|e, h| e.complete_pairing(h, perms, now_ms()));
                match done {
                    Ok((info, actions)) => {
                        self.apply(actions).await?;
                        Ok(Ok(json!({ "ok": true, "device": info })))
                    }
                    Err(e) => {
                        // The relay topic exists but the device could not be stored: undo it.
                        self.ctx.add_pending_cleanup(&device_topic);
                        Ok(Err(e))
                    }
                }
            }
            Err(e) => {
                // A lost reply may hide a committed topic; `link_remove_device` is a no-op otherwise.
                if may_have_committed(&e) {
                    self.ctx.add_pending_cleanup(&device_topic);
                }
                let actions = self.ctx.with_engine(|en, h| en.fail_pairing(h, &e.to_string(), now_ms()));
                self.apply(actions).await?;
                Ok(Err(e.to_string()))
            }
        }
    }

    async fn on_frame(&mut self, f: Frame) -> Result<(), Failure> {
        if f.topic == "phoenix" {
            if f.reference.as_deref().and_then(|r| r.parse::<u64>().ok()) == self.conn.heartbeat_pending {
                self.conn.heartbeat_pending = None;
            }
            return Ok(());
        }
        if let Some((status, resp)) = f.reply() {
            if status != "ok" {
                crate::diagnostics::log(&format!("phone link: {} replied {status}: {resp}", f.topic));
                return Ok(());
            }
            if let Some(topic) = join_confirmed(&self.conn.joined, &f) {
                let actions = self.ctx.with_engine(|e, _| e.relinked(&topic));
                self.apply(actions).await?;
            }
            return Ok(());
        }
        if f.event == "phx_error" || f.event == "phx_close" {
            if let Some(topic) = f.topic.strip_prefix("realtime:") {
                if self.conn.joined.remove(topic).is_some() && f.event == "phx_error" {
                    return Err(retry("connecting", format!("channel error on {topic}")));
                }
            }
            return Ok(());
        }
        if let Some((topic, event, payload)) = supabase::broadcast_of(&f) {
            let (topic, event, payload) = (topic.to_string(), event.to_string(), payload.clone());
            let actions = self.ctx.with_engine(|e, h| e.on_broadcast(h, &topic, &event, &payload, now_ms()));
            self.apply(actions).await?;
        }
        Ok(())
    }

    async fn on_tick(&mut self) -> Result<(), Failure> {
        let actions = self.ctx.with_engine(|e, h| e.tick(h, now_ms()));
        self.apply(actions).await?;
        let now = now_ms();
        if now - self.last_status_push >= 1000 && host::STATUS_DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.last_status_push = now;
            let actions = self.ctx.with_engine(|e, h| if e.has_sessions() { e.push_status(h) } else { Vec::new() });
            self.apply(actions).await?;
        }
        if now - self.last_scan >= MAILBOX_SCAN_MS {
            self.scan_all().await?;
        }
        self.refresh_if_needed(120).await
    }
}

/// Everything before the socket is up. Dropping it mid-way is safe: the refresh token and
/// `K_owner` are stored before use, and pending cleanup is idempotent.
async fn setup(ctx: &Ctx) -> Result<(Rest, AuthSession, net::WsStream), Failure> {
    let cfg: &RelayConfig = relay();
    let url = format!("{}/", cfg.url);
    let (route, source) = tokio::task::spawn_blocking(move || net::discover_proxy(&url))
        .await
        .unwrap_or((Route::Direct, net::ProxySource::None));
    crate::diagnostics::log(&format!("phone link: route {route} ({source:?})"));
    let http = net::http_client(&route).map_err(|e| retry("error", e))?;
    let rest = Rest { http, base: cfg.url.clone(), anon_key: cfg.anon_key.clone() };
    let (auth, _) = ensure_auth(&rest, None).await?;
    ensure_owner(&rest, &auth).await?;
    run_pending_cleanup(ctx, &rest, &auth.access_token).await;
    let ws = net::open_ws(&route, &cfg.ws_url(), &cfg.host()).await.map_err(net_failure)?;
    Ok((rest, auth, ws))
}

async fn connect_once(ctx: &Ctx, rx: &mut mpsc::UnboundedReceiver<Cmd>) -> Failure {
    // Commands are served while connecting too, so turning the feature off stops the network
    // work at once instead of after the client went online.
    let pending = setup(ctx);
    tokio::pin!(pending);
    let (rest, auth, ws) = loop {
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                None => return Failure::Stop,
                Some(cmd) => if let Some(f) = offline_cmd(ctx, cmd) {
                    return f;
                },
            },
            r = &mut pending => match r {
                Ok(v) => break v,
                Err(f) => return f,
            },
        }
    };
    let token = Arc::new(Mutex::new(auth.access_token.clone()));
    let mut online = Online {
        ctx,
        rest,
        auth,
        conn: Conn { ws, next_ref: 0, joined: HashMap::new(), heartbeat_pending: None },
        last_status_push: 0,
        token,
        last_scan: 0,
    };
    let topics = ctx.with_engine(|e, _| e.topics());
    for t in topics {
        if let Err(f) = online.conn.join(&t, &online.auth.access_token).await {
            return f;
        }
    }
    set_state(&ctx.app, "online", None);
    if let Err(f) = online.scan_all().await {
        return f;
    }
    let deferred = ctx.deferred.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
    if let Err(f) = online.apply(deferred).await {
        return f;
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(supabase::HEARTBEAT_SECS));
    heartbeat.tick().await;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let step = tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(cmd) => online.on_cmd(cmd).await,
                None => Err(Failure::Stop),
            },
            msg = online.conn.ws.next() => match msg {
                Some(Ok(Message::Text(t))) => match Frame::parse(t.as_str()) {
                    Some(f) => online.on_frame(f).await,
                    None => Ok(()),
                },
                Some(Ok(Message::Close(c))) => Err(retry("connecting", format!("closed by relay: {c:?}"))),
                Some(Ok(_)) => Ok(()),
                Some(Err(e)) => Err(retry("blocked_network", e.to_string())),
                None => Err(retry("connecting", "socket closed")),
            },
            _ = heartbeat.tick() => {
                if online.conn.heartbeat_pending.is_some() {
                    Err(retry("connecting", "heartbeat timeout"))
                } else {
                    let r = online.conn.bump();
                    online.conn.heartbeat_pending = Some(r);
                    online.conn.send(supabase::heartbeat(r)).await
                }
            },
            _ = tick.tick() => online.on_tick().await,
        };
        if let Err(f) = step {
            return f;
        }
    }
}

#[derive(Debug, PartialEq)]
enum Offline {
    Cleanup(String),
    Defer(Action),
    /// Needs the socket. After `Engine::link_lost` the engine seals nothing for a device, so
    /// only a message sealed just before the loss was noticed ends up here.
    Drop(Action),
}

/// Offline, relay cleanup is persisted and uploads and discards wait for the next connection
/// (in memory: after a restart the engine has forgotten those transfers anyway).
fn offline_route(a: Action) -> Offline {
    match a {
        Action::RelayCleanup(t) => Offline::Cleanup(t),
        Action::Upload(_) | Action::Discard { .. } => Offline::Defer(a),
        other => Offline::Drop(other),
    }
}

fn keep_cleanup(ctx: &Ctx, actions: Vec<Action>) {
    for a in actions {
        match offline_route(a) {
            Offline::Cleanup(t) => {
                ctx.cancel(Some(&t));
                ctx.add_pending_cleanup(&t);
            }
            Offline::Defer(a) => {
                if let Ok(mut d) = ctx.deferred.lock() {
                    d.push(a);
                }
            }
            Offline::Drop(Action::Send { event, .. }) => crate::diagnostics::log(&format!("phone link: offline, dropped a {event} message")),
            Offline::Drop(_) => {}
        }
    }
}

/// Commands while offline: network work is refused, cleanup is persisted for later.
fn offline_cmd(ctx: &Ctx, cmd: Cmd) -> Option<Failure> {
    match cmd {
        Cmd::Shutdown => {
            ctx.cancel(None);
            return Some(Failure::Stop);
        }
        Cmd::Reconnect => return Some(retry("connecting", "reconnect")),
        Cmd::StartPairing(reply) => {
            let _ = reply.send(Err("offline".into()));
        }
        Cmd::SubmitCode { reply, .. } => {
            let _ = reply.send(Err("offline".into()));
        }
        Cmd::CancelPairing => {
            let _ = ctx.with_engine(|e, h| e.cancel_pairing(h, now_ms()));
        }
        Cmd::Apply(actions) => keep_cleanup(ctx, actions),
        Cmd::TransferDone { fid, result } => {
            ctx.untrack(&fid);
            let actions = ctx.with_engine(|e, h| e.transfer_finished(h, &fid, result, now_ms()));
            keep_cleanup(ctx, actions);
        }
        Cmd::MailboxItem { .. } => {}
    }
    None
}

pub async fn run(ctx: Ctx, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut backoff = 1u64;
    loop {
        set_state(&ctx.app, "connecting", None);
        let started = now_ms();
        let failure = connect_once(&ctx, &mut rx).await;
        let (state, detail, wait) = match failure {
            Failure::Stop => break,
            Failure::Retry { state, detail, wait } => (state, detail, wait),
        };
        let actions = ctx.with_engine(|e, h| e.link_lost(h, now_ms()));
        keep_cleanup(&ctx, actions);
        if now_ms() - started > 60_000 {
            backoff = 1;
        }
        crate::diagnostics::log(&format!("phone link: {state}: {detail}"));
        let shown = if state == "connecting" { "connecting" } else { state };
        set_state(&ctx.app, shown, Some(detail));
        let jitter = crypto::random_bytes::<1>()[0] as u64 % 1000;
        let wait = wait.unwrap_or(Duration::from_millis(backoff * 1000 + jitter));
        backoff = (backoff * 2).min(60);
        let deadline = tokio::time::Instant::now() + wait;
        let mut stop = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                cmd = rx.recv() => match cmd {
                    None => { stop = true; break; }
                    Some(cmd) => match offline_cmd(&ctx, cmd) {
                        Some(Failure::Stop) => { stop = true; break; }
                        Some(_) => { backoff = 1; break; }
                        None => {}
                    },
                },
            }
        }
        if stop {
            break;
        }
    }
    set_state(&ctx.app, "off", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clear_refusal_skips_relay_cleanup() {
        let refused = RestError::Status { code: 400, message: "too many devices".into() };
        assert!(!may_have_committed(&refused));
        assert!(may_have_committed(&RestError::Network("timed out".into())));
        assert!(may_have_committed(&RestError::Paused));
        assert!(may_have_committed(&RestError::Status { code: 502, message: "bad gateway".into() }));
    }

    #[test]
    fn only_the_current_join_reply_relinks() {
        let joined = HashMap::from([("dev".to_string(), 7u64)]);
        let reply = |topic: &str, r: &str| Frame { topic: topic.into(), event: "phx_reply".into(), payload: json!({"status":"ok"}), reference: Some(r.into()) };
        assert_eq!(join_confirmed(&joined, &reply("realtime:dev", "7")).as_deref(), Some("dev"));
        assert_eq!(join_confirmed(&joined, &reply("realtime:dev", "8")), None, "a broadcast ack");
        assert_eq!(join_confirmed(&joined, &reply("realtime:other", "7")), None);
    }

    #[test]
    fn nothing_sent_after_a_lost_socket_is_dropped() {
        use crate::phone_link::engine::tests::{connect, paired, NOW};
        let (mut eng, mut host, mut phone) = paired(Perms::new_device());
        connect(&mut eng, &mut host, &mut phone, NOW + 10_000);
        let id = eng.devices()[0].id.clone();
        let (up, _) = eng.send_file(&mut host, &id, PathBuf::from("C:/x.bin"), "x.bin", 10, [0; 32], NOW + 11_000).unwrap();
        let m = phone.seal(json!({"t":"accept","fid":up}));
        eng.on_broadcast(&mut host, &phone.topic.clone(), "m", &m, NOW + 12_000);
        let stale = eng.send_text(&id, "sealed before the loss", &mut host, NOW + 12_500).unwrap();
        assert!(stale.iter().all(|a| matches!(offline_route(a.clone()), Offline::Drop(Action::Send { .. }))));

        eng.link_lost(&mut host, NOW + 12_800);
        let mut actions = eng.send_text(&id, "hi", &mut host, NOW + 13_000).unwrap();
        actions.extend(eng.send_file(&mut host, &id, PathBuf::from("C:/y.bin"), "y.bin", 10, [0; 32], NOW + 13_500).unwrap().1);
        actions.extend(eng.transfer_finished(&mut host, &up, Ok(None), NOW + 14_000));
        assert_eq!(actions.len(), 3);
        assert!(actions.into_iter().all(|a| matches!(offline_route(a), Offline::Defer(Action::Upload(_)))));
    }
}
