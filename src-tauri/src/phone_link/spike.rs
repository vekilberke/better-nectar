// Phase 0 connectivity spike (design section 13). Talks to the real relay, so it is ignored by
// default. Run: scripts/phone-link-spike.ps1, or
//   cargo test --bin nectar phone_link::spike -- --ignored --nocapture
// Optional: NECTAR_SPIKE_PROXY=host:port forces an extra proxy path.

use super::config::relay;
use super::net::{self, ProxyAddr, Route};
use super::supabase::{self, Frame};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

const STEP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Default)]
struct PathResult {
    label: String,
    rest: Option<Result<String, String>>,
    ws: Option<Result<String, String>>,
}

impl PathResult {
    fn transport_ok(&self) -> bool {
        matches!(self.rest, Some(Ok(_))) && matches!(self.ws, Some(Ok(_)))
    }
}

async fn rest_check(route: &Route) -> Result<String, String> {
    let cfg = relay();
    let t = Instant::now();
    let client = net::http_client(route)?;
    let resp = client
        .get(cfg.rest_url("/auth/v1/settings"))
        .header("apikey", &cfg.anon_key)
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("HTTP {status}, body: {e}"))?;
    let body: Value = serde_json::from_str(&text).map_err(|e| format!("HTTP {status}, body: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let anon = body["external"]["anonymous_users"].as_bool();
    Ok(format!("HTTP {status} in {} ms, anonymous sign-in: {anon:?}", t.elapsed().as_millis()))
}

async fn next_frame(ws: &mut net::WsStream) -> Result<Frame, String> {
    loop {
        let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
            .await
            .map_err(|_| "timed out waiting for a frame".to_string())?
            .ok_or("socket closed")?
            .map_err(|e| e.to_string())?;
        match msg {
            Message::Text(t) => {
                if let Some(f) = Frame::parse(t.as_str()) {
                    return Ok(f);
                }
            }
            Message::Close(c) => return Err(format!("closed by relay: {c:?}")),
            _ => {}
        }
    }
}

async fn wait_reply(ws: &mut net::WsStream, reference: u64) -> Result<(String, Value), String> {
    let want = reference.to_string();
    loop {
        let f = next_frame(ws).await?;
        if f.reference.as_deref() == Some(want.as_str()) {
            if let Some((status, resp)) = f.reply() {
                return Ok((status.to_string(), resp.clone()));
            }
        }
    }
}

async fn ws_check(route: &Route) -> Result<String, String> {
    let cfg = relay();
    let t = Instant::now();
    let mut ws = net::open_ws(route, &cfg.ws_url(), &cfg.host()).await.map_err(|e| e.to_string())?;
    let upgraded = t.elapsed().as_millis();

    let topic = format!("nectar-spike-{:x}", std::process::id() as u64 ^ t.elapsed().as_nanos() as u64);
    let mut join: Value = serde_json::from_str(&supabase::join(&topic, 1, false, "", None)).unwrap();
    join["payload"]["config"]["broadcast"]["self"] = json!(true);
    ws.send(Message::text(join.to_string())).await.map_err(|e| e.to_string())?;
    let (status, resp) = wait_reply(&mut ws, 1).await?;
    let joined = t.elapsed().as_millis();
    if status != "ok" {
        let _ = ws.close(None).await;
        // A refused join still proves Phoenix frames flow over this path.
        return Ok(format!("upgrade {upgraded} ms, join refused ({status}: {resp}) in {joined} ms; transport OK"));
    }

    ws.send(Message::text(supabase::broadcast(&topic, "spike", json!({ "n": 1 }), 2, 1)))
        .await
        .map_err(|e| e.to_string())?;
    let mut acked = false;
    let mut echoed = false;
    while !(acked && echoed) {
        let f = next_frame(&mut ws).await?;
        if f.reference.as_deref() == Some("2") && f.reply().map(|r| r.0 == "ok").unwrap_or(false) {
            acked = true;
        }
        if f.event == "broadcast" && f.payload["event"] == "spike" {
            echoed = true;
        }
    }
    let round_trip = t.elapsed().as_millis();

    ws.send(Message::text(supabase::heartbeat(3))).await.map_err(|e| e.to_string())?;
    let (hb, _) = wait_reply(&mut ws, 3).await?;
    let _ = ws.send(Message::text(supabase::leave(&topic, 4, 1))).await;
    let _ = ws.close(None).await;
    Ok(format!(
        "upgrade {upgraded} ms, join ok {joined} ms, broadcast ack+echo {round_trip} ms, heartbeat {hb}"
    ))
}

/// Loopback-only CONNECT proxy so the tunnel code path is exercised against the real relay
/// even on a network without a proxy. Exists only inside this test.
async fn loopback_connect_proxy() -> ProxyAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
                    match client.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let target = text
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("CONNECT "))
                    .and_then(|l| l.split_whitespace().next())
                    .map(str::to_string);
                let Some(target) = target else { return };
                let Ok(mut upstream) = TcpStream::connect(target).await else {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                    return;
                };
                if client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.is_ok() {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });
    ProxyAddr { host: "127.0.0.1".into(), port }
}

async fn run_path(label: String, route: Route) -> PathResult {
    let rest = rest_check(&route).await;
    let ws = ws_check(&route).await;
    PathResult { label, rest: Some(rest), ws: Some(ws) }
}

#[tokio::test]
#[ignore = "talks to the real relay; run with --ignored"]
async fn relay_connectivity() {
    let cfg = relay();
    let target = format!("{}/", cfg.url);
    let (discovered, source) = net::discover_proxy(&target);
    println!("\n== Nectar phone link, phase 0 connectivity spike ==");
    println!("relay:            {}", cfg.host());
    println!("proxy discovery:  {discovered} (source: {source:?})");

    let mut paths = vec![("direct".to_string(), Route::Direct)];
    if let Route::Proxy(p) = &discovered {
        paths.push((format!("system proxy {p}"), discovered.clone()));
    }
    if let Ok(forced) = std::env::var("NECTAR_SPIKE_PROXY") {
        match net::parse_proxy_list(&forced) {
            Some(p) => paths.push((format!("NECTAR_SPIKE_PROXY {p}"), Route::Proxy(p))),
            None => println!("NECTAR_SPIKE_PROXY={forced:?} is not host:port, ignored"),
        }
    }
    let loopback = loopback_connect_proxy().await;
    paths.push(("loopback CONNECT tunnel (code path)".to_string(), Route::Proxy(loopback)));

    let mut results = Vec::new();
    for (label, route) in paths {
        let r = run_path(label, route).await;
        println!("\n[{}]", r.label);
        println!("  REST GET /auth/v1/settings: {:?}", r.rest.as_ref().unwrap());
        println!("  Realtime websocket:         {:?}", r.ws.as_ref().unwrap());
        println!("  => {}", if r.transport_ok() { "OK" } else { "FAILED" });
        results.push(r);
    }

    let system_ok = results
        .iter()
        .filter(|r| !r.label.starts_with("loopback"))
        .any(PathResult::transport_ok);
    println!(
        "\nverdict: {}",
        if system_ok {
            "the relay is reachable from this network"
        } else {
            "NO working path from this network; see the runbook in docs/board/tasks/T-010-phone-link.md"
        }
    );
    assert!(system_ok, "no working path to the relay");
}
