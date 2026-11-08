// The PWA's protocol code (phone/src/core, run under Bun by phone/scripts/sim-phone.ts)
// against this engine. Two tests:
// - `pwa_code_pairs_and_runs_commands_against_the_engine`: no network, this test is the
//   relay and the Storage bucket (JSON lines over the child's stdin/stdout). Runs in
//   `cargo test`; prints SKIP when Bun or phone/node_modules is missing.
// - `pwa_code_over_the_real_relay` (ignored): the simulated phone talks to Supabase itself,
//   this test is the PC. PENDING like `e2e` until the owner claim exists (I-104).
//   cargo test --bin nectar phone_link::pwa_interop -- --ignored --nocapture
// Both run the same script: pairing, a command, status, text both ways, a small file both
// ways, the ratchet and a second session, then mailbox text both ways while the phone is closed,
// and PC clipboard copies (T-016) live and through the mailbox (two copies, one slot).

use super::client::{bytea, RelayBlobs};
use super::config::relay;
use super::crypto::{b64u, unb64u};
use super::device::MemoryDevices;
use super::e2e::{apply, relay_lock, remove_device, DeviceSlot, Ws};
use super::engine::tests::TestHost;
use super::engine::{Action, CodeOutcome, Engine};
use super::net::{self, Route};
use super::remote::{Op, Perms};
use super::secrets;
use super::supabase::Rest;
use super::transfer::tests::{temp_dir, MemBlobs};
use super::transfer::{self, Blobs, XferError};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const SIM_UID: &str = "00000000-0000-4000-8000-00000000c0de";

fn phone_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../phone"))
}

/// `bun.exe` from `NECTAR_BUN` or PATH (also behind npm's `bun.cmd` shim).
fn bun() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NECTAR_BUN") {
        return Some(p.into());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|d| [d.join("bun.exe"), d.join("node_modules").join("bun").join("bin").join("bun.exe"), d.join("bun")])
        .find(|p| p.is_file() && p.extension().is_some_and(|e| e == "exe"))
}

struct Sim {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<Value>,
}

impl Sim {
    fn spawn(mode: &str, url: &str) -> Option<Sim> {
        let Some(bun) = bun() else {
            println!("SKIP: bun not found (set NECTAR_BUN)");
            return None;
        };
        if !phone_dir().join("node_modules").join("@noble").join("curves").is_dir() {
            println!("SKIP: run `bun install` in phone/ first");
            return None;
        }
        let mut child = Command::new(bun)
            .args(["scripts/sim-phone.ts", mode, url])
            .current_dir(phone_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start bun");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        Some(Sim { child, stdin, lines })
    }

    fn write(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn deliver(&mut self, actions: Vec<Action>) {
        for a in actions {
            if let Action::Send { topic, event, payload } = a {
                self.write(json!({ "op": "broadcast", "topic": topic, "event": event, "payload": payload }));
            }
        }
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn now_ms() -> i64 {
    super::client::now_ms()
}

/// Carries out transfer actions the way the client does, in line; returns what is left for
/// the transport (sends, joins, leaves).
async fn run_transfers<B: Blobs>(engine: &mut Engine, host: &mut TestHost, b: &B, actions: Vec<Action>) -> Vec<Action> {
    let mut queue: VecDeque<Action> = actions.into();
    let mut rest = Vec::new();
    while let Some(a) = queue.pop_front() {
        match a {
            Action::Receive(job) => {
                let fid = job.plan.fid_hex();
                let r = transfer::receive(b, &job, |_| {}).await.map(Some);
                queue.extend(engine.transfer_finished(host, &fid, r, now_ms()));
            }
            Action::Upload(job) => {
                let r = transfer::send(b, &job, |_| {}).await.map(|_| None);
                queue.extend(engine.transfer_finished(host, &job.fid, r, now_ms()));
            }
            Action::Discard { topic, dir, fid, chunks, mailbox } => {
                let _ = transfer::discard(b, &topic, dir, &fid, chunks, mailbox).await;
            }
            Action::ScanMailbox(topic) => {
                for fid in b.list(&transfer::mbox_prefix(&topic, "p2c")).await.unwrap_or_default() {
                    if let Ok(bytes) = b.get(&transfer::mbox_path(&topic, "p2c", &fid)).await {
                        queue.extend(engine.mailbox_item(host, &topic, &fid, &bytes, now_ms()));
                    }
                }
            }
            other => rest.push(other),
        }
    }
    rest
}

/// One `{op:"blob"}` call of the simulated phone against the in-memory bucket.
async fn blob_call(b: &MemBlobs, v: &Value) -> Result<Value, XferError> {
    let a = &v["args"];
    let s = |i: usize| a[i].as_str().unwrap_or_default().to_string();
    match v["call"].as_str().unwrap_or_default() {
        "authorize" => b.authorize(&s(0), &s(1), &s(2), a[3].as_u64().unwrap_or(0) as u32, &s(4)).await.map(|u| json!(u)),
        "put" => b.put(&s(0), unb64u(&s(1)).ok_or(XferError::Invalid)?).await.map(|_| Value::Null),
        "get" => b.get(&s(0)).await.map(|bytes| json!(b64u(&bytes))),
        "delete" => {
            let paths = a[0].as_array().map(|x| x.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default();
            b.delete(paths).await.map(|_| Value::Null)
        }
        "release" => b.release(&s(0), &s(1), &s(2), &s(3)).await.map(|_| Value::Null),
        "list" => b.list(&s(0)).await.map(|l| json!(l)),
        _ => Err(XferError::Invalid),
    }
}

/// The PC's side of the script once the phone's file arrived: a file back to the phone.
fn pc_file_to_phone(engine: &mut Engine, host: &mut TestHost, device_id: &str, dir: &Path) -> Vec<Action> {
    let src = dir.join("from the pc.txt");
    std::fs::write(&src, b"file from the pc").unwrap();
    let (size, sha) = transfer::hash_file(&src).unwrap();
    let (_, out) = engine.send_file(host, device_id, src, "from the pc.txt", size, sha, now_ms()).expect("send file");
    out
}

fn received_file(host: &TestHost) -> Option<Value> {
    host.events.iter().find(|(e, p)| e == "phone-link-received" && p.get("paths").is_some()).map(|(_, p)| p.clone())
}

/// What the simulated phone reported, checked the same way in both tests.
#[derive(Default)]
struct Report {
    res: Vec<(String, Value)>,
    text: Option<String>,
    file: Option<(String, String)>,
    mailbox: Option<String>,
    clip_live: Option<String>,
    clip_mailbox: Option<(String, u64)>,
    ep: Option<u64>,
    done: bool,
}

impl Report {
    fn record(&mut self, v: &Value) {
        match v["op"].as_str() {
            Some("res") => self.res.push((v["cmd"].as_str().unwrap_or_default().into(), v["r"].clone())),
            Some("text") => self.text = v["text"].as_str().map(str::to_string),
            Some("file") => self.file = Some((v["name"].as_str().unwrap_or_default().into(), v["text"].as_str().unwrap_or_default().into())),
            Some("mailbox") => self.mailbox = v["text"].as_str().map(str::to_string),
            Some("clip-live") => self.clip_live = v["text"].as_str().map(str::to_string),
            Some("clip-mailbox") => self.clip_mailbox = Some((v["text"].as_str().unwrap_or_default().into(), v["clips"].as_u64().unwrap_or_default())),
            Some("ep") => self.ep = v["ep"].as_u64(),
            Some("done") => self.done = true,
            Some("error") => panic!("simulated phone failed: {v}"),
            _ => {}
        }
    }

    fn check(&self, host: &TestHost) {
        assert!(self.done, "phone did not finish");
        let cmds: Vec<&str> = self.res.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(cmds, ["media.play_pause", "status.get", "text", "file", "status.get#2", "queued-text"]);
        for (cmd, r) in &self.res {
            assert_eq!(r["ok"], true, "{cmd}: {r}");
        }
        assert_eq!(self.res[5].1["d"]["queued"], true, "the closed phone queued its text");
        assert_eq!(self.text.as_deref(), Some("hello from the pc"));
        assert_eq!(self.file.as_ref().map(|(n, t)| (n.as_str(), t.as_str())), Some(("from the pc.txt", "file from the pc")));
        assert_eq!(self.mailbox.as_deref(), Some("mailbox from the pc"));
        assert_eq!(self.clip_live.as_deref(), Some("live clip from the pc"));
        assert_eq!(self.clip_mailbox.as_ref().map(|(t, n)| (t.as_str(), *n)), Some(("clip from the pc", 1)), "only the newest copy, replacing the live one");
        let clips: Vec<&str> = host.audit.iter().filter(|e| e.op == "clip").map(|e| e.result.as_str()).collect();
        // The live copy comes again on the second session if the phone sent nothing CLIP_ACK_SECS
        // after it; a phone that reopens within CLIP_RESEND_SECS of the slot upload also gets that copy live.
        assert!(matches!(clips.as_slice(), ["ok", "queued", "queued"] | ["ok", "queued", "queued", "ok"] | ["ok", "ok", "queued", "queued"] | ["ok", "ok", "queued", "queued", "ok"]), "{clips:?}");
        assert_eq!(self.ep, Some(1), "the phone ratcheted to epoch 1");
        assert!(host.executed.contains(&Op::MediaPlayPause) && host.executed.contains(&Op::StatusGet));
        assert_eq!(host.texts, vec!["hello from the pwa".to_string(), "mailbox from the pwa".to_string()]);
        let received = received_file(host).expect("file received");
        let path = PathBuf::from(received["paths"][0].as_str().unwrap());
        assert_eq!(path.file_name().unwrap(), "sim note.txt");
        assert_eq!(std::fs::read(&path).unwrap(), b"file from the pwa");
        assert!(transfer::read_motw(&path).is_some(), "Mark-of-the-Web on the received file");
        let saved: Vec<&str> = host.events.iter().filter(|(e, p)| e == "phone-link-transfer" && p["state"] == "saved").map(|(_, p)| p["dir"].as_str().unwrap()).collect();
        assert_eq!(saved, ["p2c", "c2p"], "both files confirmed");
        let files: Vec<(&str, &str)> = host.audit.iter().filter(|e| e.op == "file").map(|e| (e.dir.unwrap_or(""), e.result.as_str())).collect();
        assert_eq!(files, [("p2c", "ok"), ("c2p", "ok")]);
        assert!(host.audit.iter().all(|e| !e.extra.to_string().contains("hello from") && !e.extra.to_string().contains("mailbox from") && !e.extra.to_string().contains("clip from")), "no text in the audit log");
        let starts = host.events.iter().filter(|(e, p)| e == "phone-link-session" && p["state"] == "start").count();
        assert_eq!(starts, 3, "three sessions: the second under the ratcheted key, the third after the phone reopened");
    }
}

#[test]
fn pwa_code_pairs_and_runs_commands_against_the_engine() {
    let cfg = relay();
    let mut engine = Engine::new(Box::new(MemoryDevices::default()), "INTEROP-PC");
    let dir = temp_dir("interop");
    let mut host = TestHost { download_dir: Some(dir.clone()), ..TestHost::default() };
    let blobs = MemBlobs::default();
    let rt = transfer::tests::rt();
    let start = engine.start_pairing(&cfg.pwa_url, &cfg.project_ref, now_ms());
    let Some(mut sim) = Sim::spawn("--stdio", &start.url) else { return };
    let mut report = Report::default();
    let mut device: Option<(String, String)> = None;
    let (mut sent_text, mut sent_file) = (false, false);
    while !report.done {
        let v = sim.lines.recv_timeout(Duration::from_secs(30)).expect("simulated phone output");
        match v["op"].as_str() {
            Some("send") => {
                let (topic, event) = (v["topic"].as_str().unwrap(), v["event"].as_str().unwrap());
                let out = engine.on_broadcast(&mut host, topic, event, &v["payload"], now_ms());
                let out = rt.block_on(run_transfers(&mut engine, &mut host, &blobs, out));
                sim.deliver(out);
            }
            Some("blob") => {
                let r = rt.block_on(blob_call(&blobs, &v));
                sim.write(match r {
                    Ok(r) => json!({ "op": "blobres", "id": v["id"], "ok": true, "r": r }),
                    Err(e) => json!({ "op": "blobres", "id": v["id"], "ok": false, "e": e.code() }),
                });
            }
            Some("code") => {
                let (outcome, _) = engine.submit_code(&mut host, v["code"].as_str().unwrap(), now_ms());
                assert_eq!(outcome, CodeOutcome::Accepted, "the PWA shows the PC's code");
                let (_, device_topic, uid, _) = engine.accepted_device().unwrap();
                assert_eq!(uid, SIM_UID);
                let (info, out) = engine.complete_pairing(&mut host, Perms::new_device(), now_ms()).unwrap();
                assert!(out.contains(&Action::Join(device_topic.clone())));
                device = Some((info.id, device_topic));
                sim.deliver(out);
            }
            Some("queued") => {
                // The phone is closed: read its mailbox, then queue a text for it.
                let (id, topic) = device.clone().unwrap();
                let out = rt.block_on(run_transfers(&mut engine, &mut host, &blobs, vec![Action::ScanMailbox(topic)]));
                sim.deliver(out);
                let _ = engine.end_session(&id, &mut host);
                let out = engine.send_text(&id, "mailbox from the pc", &mut host, now_ms()).unwrap();
                assert!(out.iter().any(|a| matches!(a, Action::Upload(j) if j.mailbox.is_some())), "no session: the text goes to the mailbox");
                let out = rt.block_on(run_transfers(&mut engine, &mut host, &blobs, out));
                sim.deliver(out);
                for copy in ["old clip from the pc", "clip from the pc"] {
                    let out = engine.send_clip(copy, &mut host, now_ms());
                    let out = rt.block_on(run_transfers(&mut engine, &mut host, &blobs, out));
                    sim.deliver(out);
                }
                sim.write(json!({ "op": "go" }));
            }
            _ => report.record(&v),
        }
        if let Some((id, _)) = device.clone() {
            if !sent_text && !host.texts.is_empty() {
                sent_text = true;
                let mut out = engine.send_text(&id, "hello from the pc", &mut host, now_ms()).unwrap();
                out.extend(engine.send_clip("live clip from the pc", &mut host, now_ms()));
                sim.deliver(out);
            }
            if !sent_file && received_file(&host).is_some() {
                sent_file = true;
                let out = pc_file_to_phone(&mut engine, &mut host, &id, &dir);
                let out = rt.block_on(run_transfers(&mut engine, &mut host, &blobs, out));
                sim.deliver(out);
            }
        }
    }
    report.check(&host);
    assert_eq!(engine.devices()[0].epoch, 1, "the PC confirmed the ratchet");
    assert!(blobs.objects.lock().unwrap().is_empty(), "every transfer object was removed from the bucket: {:?}", blobs.objects.lock().unwrap().keys().collect::<Vec<_>>());
    assert!(engine.transfers().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The PC's `RelayBlobs` against real Storage and `link-upload`, on a test topic shared by two
/// anonymous users (no owner needed). Prepare it with `phone/scripts/transfer-smoke.ts setup
/// <state.json>` plus the SQL it prints, then:
///   NECTAR_SMOKE_STATE=<state.json> cargo test --bin nectar phone_link::pwa_interop::rust_storage -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn rust_storage_over_the_real_relay() {
    let Some(path) = std::env::var_os("NECTAR_SMOKE_STATE") else {
        println!("PENDING: set NECTAR_SMOKE_STATE (see the comment above this test)");
        return;
    };
    let state: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let topic = state["topic"].as_str().unwrap().to_string();
    let cfg = relay();
    let rest = Rest { http: net::http_client(&Route::Direct).unwrap(), base: cfg.url.clone(), anon_key: cfg.anon_key.clone() };
    let as_user = |who: &str| RelayBlobs { rest: rest.clone(), token: std::sync::Arc::new(std::sync::Mutex::new(state[who]["accessToken"].as_str().unwrap().to_string())) };
    let (pc, phone) = (as_user("pc"), as_user("phone"));
    let dir = temp_dir("relay-storage");

    // Phone to PC: the phone's upload (signed PUTs), then the PC's receive with Mark-of-the-Web.
    let data: Vec<u8> = (0..(transfer::CHUNK as usize + 999)).map(|i| (i % 241) as u8).collect();
    let plan = transfer::Plan::new_outbound("rust smoke.bin", data.len() as u64, "application/octet-stream", <sha2::Sha256 as sha2::Digest>::digest(&data).into()).unwrap();
    let urls = phone.authorize(&topic, "p2c", &plan.fid_hex(), plan.chunks, "blob").await.expect("phone gets upload URLs");
    for (i, url) in urls.iter().enumerate() {
        let start = i * transfer::CHUNK as usize;
        let end = (start + transfer::CHUNK as usize).min(data.len());
        phone.put(url, transfer::seal_chunk(&plan, super::crypto::DIR_P2C, i as u32, &data[start..end])).await.expect("signed PUT");
    }
    let job = transfer::ReceiveJob { device_id: "d".into(), topic: topic.clone(), plan: plan.clone(), download_dir: dir.clone(), mailbox: false };
    let saved = transfer::receive(&pc, &job, |_| {}).await.expect("PC receives over the relay");
    assert_eq!(std::fs::read(&saved).unwrap(), data);
    assert!(transfer::read_motw(&saved).is_some());
    assert!(pc.get(&transfer::blob_path(&topic, "p2c", &plan.fid_hex(), 0)).await.is_err(), "chunks deleted after the receive");
    println!("p2c: {} bytes received, Mark-of-the-Web written, objects deleted", data.len());

    // PC to phone: chunks and a mailbox offer from `transfer::send`, read back as the phone.
    let src = dir.join("pc file.txt");
    std::fs::write(&src, b"pc to phone over the relay").unwrap();
    let (size, sha) = transfer::hash_file(&src).unwrap();
    let out = transfer::Plan::new_outbound("pc file.txt", size, "text/plain", sha).unwrap();
    let k_dev = [7u8; 32];
    let obj = transfer::seal_mailbox(&k_dev, 0, &topic, "c2p", &out.fid, (now_ms() / 1000) as u64, &serde_json::to_value(out.offer()).unwrap());
    let job = transfer::SendJob { device_id: "d".into(), topic: topic.clone(), fid: out.fid_hex(), plan: Some(out.clone()), source: transfer::Source::File(src), mailbox: Some(obj) };
    transfer::send(&pc, &job, |_| {}).await.expect("PC uploads over the relay");
    let listed = phone.list(&transfer::mbox_prefix(&topic, "c2p")).await.unwrap();
    assert_eq!(listed, vec![out.fid_hex()]);
    let bytes = phone.get(&transfer::mbox_path(&topic, "c2p", &out.fid_hex())).await.unwrap();
    let inner = transfer::open_mailbox(|ep| (ep == 0).then_some(k_dev), &topic, "c2p", &out.fid, &bytes, now_ms() / 1000).expect("mailbox opens");
    let offer: transfer::Offer = serde_json::from_slice(&inner).unwrap();
    let blob = phone.get(&transfer::blob_path(&topic, "c2p", &out.fid_hex(), 0)).await.unwrap();
    assert_eq!(transfer::open_chunk(&offer.plan().unwrap(), super::crypto::DIR_C2P, 0, &blob).unwrap(), b"pc to phone over the relay");
    transfer::discard(&phone, &topic, "c2p", &out.fid_hex(), 1, true).await.expect("phone discards");
    assert!(phone.list(&transfer::mbox_prefix(&topic, "c2p")).await.unwrap().is_empty());
    assert_eq!(pc.authorize(&topic, "p2c", &plan.fid_hex(), 1, "blob").await, Err(XferError::Denied), "the PC may not upload phone-to-PC");
    let _ = std::fs::remove_dir_all(&dir);
    println!("c2p: chunks and mailbox offer read back by the phone, then discarded; direction enforced");
}

#[tokio::test]
#[ignore]
async fn pwa_code_over_the_real_relay() {
    let _lock = relay_lock();
    let cfg = relay();
    let rest = Rest { http: net::http_client(&Route::Direct).unwrap(), base: cfg.url.clone(), anon_key: cfg.anon_key.clone() };
    let settings = rest.auth_settings().await.expect("auth settings");
    if settings["external"]["anonymous_users"].as_bool() != Some(true) {
        println!("PENDING: anonymous sign-ins are off on the relay (I-101).");
        return;
    }
    let Some(k_owner) = secrets::read(secrets::OWNER_TARGET).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) else {
        println!("PENDING: no K_owner in Credential Manager; run the owner claim procedure first (I-104).");
        return;
    };
    let now = now_ms() / 1000;
    let pc_auth = rest.signup_anonymous(now).await.expect("pc signup");
    let mac = super::crypto::owner_mac(&k_owner, &pc_auth.user_id, now);
    if let Err(e) = rest.rpc(&pc_auth.access_token, "link_owner_rebind", json!({ "ts": now, "mac": bytea(&mac) })).await {
        println!("PENDING: owner rebind refused ({e}); the stored K_owner does not match the relay.");
        return;
    }
    let token = pc_auth.access_token.clone();
    let slot = DeviceSlot::default();
    let body = async {
        let blobs = RelayBlobs { rest: rest.clone(), token: std::sync::Arc::new(std::sync::Mutex::new(token.clone())) };
        let mut engine = Engine::new(Box::new(MemoryDevices::default()), "RELAY-PC");
        let dir = temp_dir("relay-interop");
        let mut host = TestHost { download_dir: Some(dir.clone()), ..TestHost::default() };
        let start = engine.start_pairing(&cfg.pwa_url, &cfg.project_ref, now_ms());
        rest.rpc(&token, "link_create_pairing", json!({ "t": start.pair_topic })).await.expect("create pairing");
        let mut pc = Ws::open("pc").await;
        pc.join(&start.pair_topic, &token).await;
        let Some(mut sim) = Sim::spawn("--relay", &start.url) else { return };

        // Child lines arrive on a std channel; poll it between websocket frames.
        let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
        let std_lines = std::mem::replace(&mut sim.lines, mpsc::channel().1);
        std::thread::spawn(move || {
            while let Ok(v) = std_lines.recv() {
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        let mut report = Report::default();
        let mut device: Option<(String, String)> = None;
        let (mut sent_text, mut sent_file) = (false, false);
        let mut code: Option<String> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        while !report.done {
            assert!(tokio::time::Instant::now() < deadline, "relay interop timed out");
            enum Ev {
                Line(Value),
                Frame(String, String, Value),
                Idle,
            }
            let ev = tokio::select! {
                // Shorter than the 15 s frame timeout inside Ws::frame, so an idle stretch (the simulated
                // phone still starting under load) yields Idle instead of racing that timeout into a panic.
                v = lines.recv() => Ev::Line(v.expect("simulated phone exited")),
                r = tokio::time::timeout(Duration::from_secs(5), pc.next_broadcast()) => match r {
                    Ok((t, e, p)) => Ev::Frame(t, e, p),
                    Err(_) => Ev::Idle,
                }
            };
            match ev {
                // The PWA shows the code before its hello has crossed the relay; a person types it
                // later, so wait for the pairing request first.
                Ev::Line(v) if v["op"] == "code" => code = Some(v["code"].as_str().unwrap().to_string()),
                Ev::Line(v) if v["op"] == "queued" => {
                    let (id, topic) = device.clone().unwrap();
                    let out = run_transfers(&mut engine, &mut host, &blobs, vec![Action::ScanMailbox(topic)]).await;
                    apply(&mut pc, &token, out).await;
                    let end = engine.end_session(&id, &mut host);
                    apply(&mut pc, &token, end).await;
                    let out = engine.send_text(&id, "mailbox from the pc", &mut host, now_ms()).unwrap();
                    let out = run_transfers(&mut engine, &mut host, &blobs, out).await;
                    apply(&mut pc, &token, out).await;
                    for copy in ["old clip from the pc", "clip from the pc"] {
                        let out = engine.send_clip(copy, &mut host, now_ms());
                        let out = run_transfers(&mut engine, &mut host, &blobs, out).await;
                        apply(&mut pc, &token, out).await;
                    }
                    sim.write(json!({ "op": "go" }));
                }
                Ev::Line(v) => report.record(&v),
                Ev::Frame(t, e, p) => {
                    let out = engine.on_broadcast(&mut host, &t, &e, &p, now_ms());
                    let out = run_transfers(&mut engine, &mut host, &blobs, out).await;
                    apply(&mut pc, &token, out).await;
                }
                Ev::Idle => {}
            }
            if code.is_some() && host.events.iter().any(|e| e.0 == "phone-link-pair-request") {
                let (outcome, _) = engine.submit_code(&mut host, &code.take().unwrap(), now_ms());
                assert_eq!(outcome, CodeOutcome::Accepted);
                let (pair_topic, device_topic, uid, rk) = engine.accepted_device().unwrap();
                slot.set(&device_topic);
                rest.rpc(&token, "link_create_device", json!({ "pair_topic": pair_topic, "t": device_topic, "phone": uid, "rk": bytea(&rk) }))
                    .await
                    .expect("create device");
                let (info, out) = engine.complete_pairing(&mut host, Perms::new_device(), now_ms()).unwrap();
                device = Some((info.id, device_topic));
                apply(&mut pc, &token, out).await;
            }
            if let Some((id, _)) = device.clone() {
                if !sent_text && !host.texts.is_empty() {
                    sent_text = true;
                    let mut out = engine.send_text(&id, "hello from the pc", &mut host, now_ms()).unwrap();
                    out.extend(engine.send_clip("live clip from the pc", &mut host, now_ms()));
                    apply(&mut pc, &token, out).await;
                }
                if !sent_file && received_file(&host).is_some() {
                    sent_file = true;
                    let out = pc_file_to_phone(&mut engine, &mut host, &id, &dir);
                    let out = run_transfers(&mut engine, &mut host, &blobs, out).await;
                    apply(&mut pc, &token, out).await;
                }
            }
        }
        report.check(&host);
        if let Some((_, topic)) = device {
            let left = rest.storage_walk(&token, &topic).await.unwrap_or_default();
            assert!(left.is_empty(), "objects left on the relay: {left:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        println!("relay interop ok (files and mailbox both ways)");
    };
    slot.guard(body, remove_device(&rest, &token)).await;
    println!("Nectar rebinds ownership on its next connect");
}
