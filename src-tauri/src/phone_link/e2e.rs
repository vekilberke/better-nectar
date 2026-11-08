// End-to-end test against the real relay (acceptance criterion 3), ignored by default:
//   cargo test --bin nectar phone_link::e2e -- --ignored --nocapture
// A simulated phone (the PWA protocol in Rust) pairs with the engine over Supabase Realtime:
// typed code, handshake, a whitelisted command, text both ways. Files both ways join in
// phase 4. The test reports PENDING instead of failing while anonymous sign-ins are off
// (I-101) or the PC has not claimed the relay yet (I-104). It borrows ownership with
// `link_owner_rebind` and the PC's stored K_owner; Nectar rebinds back on its next connect.

use super::client::bytea;
use super::config::relay;
use super::crypto::{self, unb64u};
use super::device::MemoryDevices;
use super::engine::tests::{sends, Phone, TestHost};
use super::engine::{Action, CodeOutcome, Engine};
use super::net::{self, Route};
use super::pairing::tests::PhonePairing;
use super::remote::Perms;
use super::secrets;
use super::supabase::{self, AuthSession, Frame, Rest};
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub(super) const WAIT: Duration = Duration::from_secs(15);

pub(super) struct Ws {
    ws: net::WsStream,
    r: u64,
    joins: std::collections::HashMap<String, u64>,
    key: &'static str,
}

impl Ws {
    pub(super) async fn open(key: &'static str) -> Ws {
        let cfg = relay();
        Ws { ws: net::open_ws(&Route::Direct, &cfg.ws_url(), &cfg.host()).await.expect("websocket"), r: 0, joins: Default::default(), key }
    }

    async fn frame(&mut self) -> Frame {
        loop {
            let msg = tokio::time::timeout(WAIT, self.ws.next()).await.expect("frame timeout").expect("socket open").expect("frame");
            if let Message::Text(t) = msg {
                if let Some(f) = Frame::parse(t.as_str()) {
                    return f;
                }
            }
        }
    }

    pub(super) async fn join(&mut self, topic: &str, token: &str) {
        self.r += 1;
        let r = self.r;
        self.joins.insert(topic.into(), r);
        self.ws.send(Message::text(supabase::join(topic, r, true, self.key, Some(token)))).await.unwrap();
        loop {
            let f = self.frame().await;
            if f.reference.as_deref() == Some(r.to_string().as_str()) {
                let (status, resp) = f.reply().expect("join reply");
                assert_eq!(status, "ok", "join {topic} as {}: {resp}", self.key);
                return;
            }
        }
    }

    pub(super) async fn leave(&mut self, topic: &str) {
        if let Some(j) = self.joins.remove(topic) {
            self.r += 1;
            let _ = self.ws.send(Message::text(supabase::leave(topic, self.r, j))).await;
        }
    }

    pub(super) async fn broadcast(&mut self, topic: &str, event: &str, payload: Value) {
        let j = self.joins[topic];
        self.r += 1;
        self.ws.send(Message::text(supabase::broadcast(topic, event, payload, self.r, j))).await.unwrap();
    }

    pub(super) async fn next_broadcast(&mut self) -> (String, String, Value) {
        loop {
            let f = self.frame().await;
            if let Some((t, e, p)) = supabase::broadcast_of(&f) {
                return (t.into(), e.into(), p.clone());
            }
        }
    }
}

pub(super) async fn apply(pc: &mut Ws, token: &str, actions: Vec<Action>) {
    for a in actions {
        match a {
            Action::Send { topic, event, payload } => pc.broadcast(&topic, event, payload).await,
            Action::Join(t) => pc.join(&t, token).await,
            Action::Leave(t) => pc.leave(&t).await,
            // Transfers run in `pwa_interop::apply_with`; relay cleanup is done by the test itself.
            _ => {}
        }
    }
}

/// The real-relay tests share one owner and one pairing slot (`link_create_pairing` drops every
/// open offer, `link_owner_rebind` moves ownership), so they run one at a time, also across
/// processes. The OS drops the lock when the process dies (I-164).
pub(super) fn relay_lock() -> std::fs::File {
    let path = std::env::temp_dir().join("nectar-real-relay-test.lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).expect("lock file");
    f.lock().expect("relay test lock");
    f
}

/// The device topic a real-relay test created. `guard` removes it after the test body, also when
/// the body panics, so failed runs do not fill the relay's 10-device cap (I-164).
#[derive(Default)]
pub(super) struct DeviceSlot(RefCell<Option<String>>);

impl DeviceSlot {
    pub(super) fn set(&self, topic: &str) {
        *self.0.borrow_mut() = Some(topic.to_string());
    }

    pub(super) async fn guard<B, R, RF>(&self, body: B, remove: R)
    where
        B: Future<Output = ()>,
        R: FnOnce(String) -> RF,
        RF: Future<Output = Result<(), String>>,
    {
        let outcome = AssertUnwindSafe(body).catch_unwind().await;
        let topic = self.0.borrow_mut().take();
        if let Some(topic) = topic {
            match remove(topic.clone()).await {
                Ok(()) => println!("cleanup: link_remove_device {topic} ok"),
                Err(e) if outcome.is_ok() => panic!("remove device {topic}: {e}"),
                Err(e) => eprintln!("cleanup failed, remove {topic} as the owner: {e}"),
            }
        }
        if let Err(p) = outcome {
            std::panic::resume_unwind(p);
        }
    }
}

/// `DeviceSlot::guard`'s remover for the real relay.
pub(super) fn remove_device<'a>(rest: &'a Rest, token: &'a str) -> impl FnOnce(String) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
    move |t| Box::pin(async move { rest.rpc(token, "link_remove_device", json!({ "t": t })).await.map(|_| ()).map_err(|e| e.to_string()) })
}

fn now_ms() -> i64 {
    super::client::now_ms()
}

#[tokio::test]
#[ignore]
async fn pairing_and_session_over_the_real_relay() {
    let _lock = relay_lock();
    let cfg = relay();
    let rest = Rest { http: net::http_client(&Route::Direct).unwrap(), base: cfg.url.clone(), anon_key: cfg.anon_key.clone() };
    let settings = rest.auth_settings().await.expect("auth settings");
    if settings["external"]["anonymous_users"].as_bool() != Some(true) {
        println!("PENDING: anonymous sign-ins are off on the relay (I-101); nothing tested end to end.");
        return;
    }
    let Some(k_owner) = secrets::read(secrets::OWNER_TARGET).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) else {
        println!("PENDING: no K_owner in Credential Manager; run the owner claim procedure first (I-104).");
        return;
    };
    let now = now_ms() / 1000;
    let pc_auth: AuthSession = rest.signup_anonymous(now).await.expect("pc signup");
    let mac = crypto::owner_mac(&k_owner, &pc_auth.user_id, now);
    if let Err(e) = rest.rpc(&pc_auth.access_token, "link_owner_rebind", json!({ "ts": now, "mac": bytea(&mac) })).await {
        println!("PENDING: owner rebind refused ({e}); the stored K_owner does not match the relay.");
        return;
    }
    println!("pc user {} is owner", pc_auth.user_id);
    let slot = DeviceSlot::default();
    let body = async {

        let mut engine = Engine::new(Box::new(MemoryDevices::default()), "E2E-PC");
        let mut host = TestHost::default();
        let start = engine.start_pairing(&cfg.pwa_url, &cfg.project_ref, now_ms());
        rest.rpc(&pc_auth.access_token, "link_create_pairing", json!({ "t": start.pair_topic })).await.expect("create pairing");
        // The connect-time owner check must leave the open pairing row alone (the phone joins it below).
        rest.rpc(&pc_auth.access_token, "link_owner_check", json!({})).await.expect("owner check");
        let mut pc = Ws::open("pc").await;
        pc.join(&start.pair_topic, &pc_auth.access_token).await;

        let phone_auth = rest.signup_anonymous(now).await.expect("phone signup");
        let pairing = PhonePairing::from_qr(&start.url, &phone_auth.user_id);
        let mut ph = Ws::open("phone").await;
        ph.join(&start.pair_topic, &phone_auth.access_token).await;
        ph.broadcast(&start.pair_topic, "hello", pairing.hello("E2E phone")).await;

        let (topic, event, payload) = pc.next_broadcast().await;
        assert_eq!((topic.as_str(), event.as_str()), (start.pair_topic.as_str(), "hello"));
        let seen = engine.on_broadcast(&mut host, &topic, &event, &payload, now_ms());
        assert_eq!(host.events.last().unwrap().0, "phone-link-pair-request");
        // The PC's first write on the pairing topic must happen while the offer is open (I-161).
        apply(&mut pc, &pc_auth.access_token, seen).await;
        let (_, event, _) = ph.next_broadcast().await;
        assert_eq!(event, "seen");
        println!("typed code {} accepted", pairing.code());
        assert_eq!(engine.submit_code(&mut host, &pairing.code(), now_ms()).0, CodeOutcome::Accepted);
        let (pair_topic, device_topic, uid, rk) = engine.accepted_device().unwrap();
        slot.set(&device_topic);
        rest.rpc(&pc_auth.access_token, "link_create_device", json!({ "pair_topic": pair_topic, "t": device_topic, "phone": uid, "rk": bytea(&rk) }))
            .await
            .expect("create device");
        let (info, actions) = engine.complete_pairing(&mut host, Perms::new_device(), now_ms()).unwrap();
        apply(&mut pc, &pc_auth.access_token, actions).await;

        let (_, event, welcome) = ph.next_broadcast().await;
        assert_eq!(event, "welcome");
        let k0 = pairing.k_dev0();
        let w: Value = serde_json::from_slice(
            &crypto::open(&crypto::welcome_key(&k0), b"welcome", &unb64u(welcome["c"].as_str().unwrap()).unwrap()).expect("welcome decrypts"),
        )
        .unwrap();
        assert_eq!(w["id"], info.id);
        ph.leave(&pair_topic).await;
        ph.join(&device_topic, &phone_auth.access_token).await;
        println!("paired: device topic joined by the phone");

        let mut phone = Phone::new(&device_topic, k0);
        ph.broadcast(&device_topic, "hs1", phone.hs1(now_ms() / 1000)).await;
        let (t, e, p) = pc.next_broadcast().await;
        assert_eq!(e, "hs1");
        let out = engine.on_broadcast(&mut host, &t, &e, &p, now_ms());
        apply(&mut pc, &pc_auth.access_token, out).await;
        let (_, e, hs2) = ph.next_broadcast().await;
        assert_eq!(e, "hs2");
        assert!(phone.on_hs2(&hs2), "hs2 verifies");
        println!("handshake ok");

        let mut seen = Vec::new();
        let msg = phone.seal(json!({ "t": "cmd", "id": 1, "op": "status.get" }));
        ph.broadcast(&device_topic, "m", msg).await;
        let msg = phone.seal(json!({ "t": "text", "id": 2, "text": "hello from the phone" }));
        ph.broadcast(&device_topic, "m", msg).await;
        for _ in 0..2 {
            let (t, e, p) = pc.next_broadcast().await;
            let out = engine.on_broadcast(&mut host, &t, &e, &p, now_ms());
            apply(&mut pc, &pc_auth.access_token, out).await;
        }
        assert_eq!(host.texts, vec!["hello from the phone".to_string()]);
        let out = engine.send_text(&info.id, "hello from the pc", &mut host, now_ms()).unwrap();
        apply(&mut pc, &pc_auth.access_token, out).await;
        while seen.iter().filter(|v: &&Value| v["t"] == "res").count() < 2 || !seen.iter().any(|v| v["t"] == "text") {
            let (_, e, p) = tokio::time::timeout(WAIT, ph.next_broadcast()).await.expect("phone receives");
            if e == "m" {
                if let Some(v) = phone.open(&p) {
                    seen.push(v);
                }
            }
        }
        assert!(seen.iter().any(|v| v["t"] == "res" && v["id"] == 1 && v["ok"] == true));
        assert!(seen.iter().any(|v| v["t"] == "text" && v["text"] == "hello from the pc"));
        println!("command and text both ways ok ({} messages)", seen.len());
        let _ = sends(&[], "m");

    };
    slot.guard(body, remove_device(&rest, &pc_auth.access_token)).await;
    println!("Nectar rebinds ownership on its next connect");
}

#[tokio::test]
async fn device_slot_removes_the_topic_also_when_the_test_panics() {
    let removed = RefCell::new(Vec::new());
    let remover = |t: String| {
        removed.borrow_mut().push(t);
        async { Ok(()) }
    };
    let slot = DeviceSlot::default();
    let run = AssertUnwindSafe(slot.guard(
        async {
            slot.set("nl-a");
            panic!("assert failed after create_device");
        },
        remover,
    ))
    .catch_unwind()
    .await;
    assert!(run.is_err(), "the test's panic is passed on");
    slot.guard(async { slot.set("nl-b") }, |t: String| {
        removed.borrow_mut().push(t);
        async { Ok(()) }
    })
    .await;
    slot.guard(async {}, |_: String| -> std::future::Ready<Result<(), String>> { unreachable!("no topic, nothing to remove") }).await;
    assert_eq!(*removed.borrow(), vec!["nl-a".to_string(), "nl-b".to_string()]);
    let failed = AssertUnwindSafe(slot.guard(async { slot.set("nl-c") }, |_: String| async { Err("400".to_string()) })).catch_unwind().await;
    assert!(failed.is_err(), "a failed cleanup fails a passing test");
}
