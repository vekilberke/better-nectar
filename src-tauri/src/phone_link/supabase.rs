// Supabase client pieces: Realtime (Phoenix protocol, vsn 1.0.0) frames, and Auth, RPC and
// Storage over REST (design section 8).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const HEARTBEAT_SECS: u64 = 25;

#[derive(Debug, Clone, Deserialize)]
pub struct Frame {
    pub topic: String,
    pub event: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

impl Frame {
    pub fn parse(text: &str) -> Option<Frame> {
        serde_json::from_str(text).ok()
    }

    /// For `phx_reply`: the status ("ok" / "error") and the response body.
    pub fn reply(&self) -> Option<(&str, &Value)> {
        if self.event != "phx_reply" {
            return None;
        }
        let status = self.payload.get("status")?.as_str()?;
        Some((status, self.payload.get("response").unwrap_or(&Value::Null)))
    }
}

pub fn channel(topic: &str) -> String {
    format!("realtime:{topic}")
}

pub fn join(topic: &str, reference: u64, private: bool, presence_key: &str, access_token: Option<&str>) -> String {
    let mut payload = json!({
        "config": {
            "broadcast": { "self": false, "ack": true },
            "presence": { "key": presence_key },
            "private": private
        }
    });
    if let Some(token) = access_token {
        payload["access_token"] = json!(token);
    }
    frame(&channel(topic), "phx_join", payload, reference, Some(reference))
}

pub fn broadcast(topic: &str, event: &str, body: Value, reference: u64, join_ref: u64) -> String {
    let payload = json!({ "type": "broadcast", "event": event, "payload": body });
    frame(&channel(topic), "broadcast", payload, reference, Some(join_ref))
}

pub fn heartbeat(reference: u64) -> String {
    frame("phoenix", "heartbeat", json!({}), reference, None)
}

pub fn leave(topic: &str, reference: u64, join_ref: u64) -> String {
    frame(&channel(topic), "phx_leave", json!({}), reference, Some(join_ref))
}

/// Sent on every joined channel after a token refresh.
pub fn access_token(topic: &str, token: &str, reference: u64, join_ref: u64) -> String {
    frame(&channel(topic), "access_token", json!({ "access_token": token }), reference, Some(join_ref))
}

/// The broadcast inside a pushed frame: (topic without `realtime:`, event, payload).
pub fn broadcast_of(f: &Frame) -> Option<(&str, &str, &Value)> {
    if f.event != "broadcast" {
        return None;
    }
    let topic = f.topic.strip_prefix("realtime:")?;
    let event = f.payload.get("event")?.as_str()?;
    Some((topic, event, f.payload.get("payload").unwrap_or(&Value::Null)))
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestError {
    /// Transport failure (no answer from the relay).
    Network(String),
    /// HTTP 540 (or a non-JSON 503) from the project URL: the free project is paused.
    Paused,
    /// Supabase answered with an error; `message` is the Postgres/Auth message.
    Status { code: u16, message: String },
}

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestError::Network(e) => write!(f, "network: {e}"),
            RestError::Paused => write!(f, "relay paused"),
            RestError::Status { code, message } => write!(f, "HTTP {code}: {message}"),
        }
    }
}

impl RestError {
    pub fn message(&self) -> &str {
        match self {
            RestError::Status { message, .. } => message,
            _ => "",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuthSession {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub user_id: String,
}

#[derive(Deserialize)]
struct AuthResponse {
    access_token: String,
    refresh_token: String,
    expires_in: Option<i64>,
    expires_at: Option<i64>,
    user: AuthUser,
}

#[derive(Deserialize)]
struct AuthUser {
    id: String,
}

pub fn parse_auth(body: &str, now: i64) -> Option<AuthSession> {
    let r: AuthResponse = serde_json::from_str(body).ok()?;
    let expires_at = r.expires_at.unwrap_or(now + r.expires_in.unwrap_or(3600));
    Some(AuthSession { access_token: r.access_token, refresh_token: r.refresh_token, expires_at, user_id: r.user.id })
}

/// Error text from a Supabase error body (PostgREST `message`, Auth `msg` / `error_description`).
pub fn error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    ["message", "msg", "error_description", "error"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(200).collect())
}

pub fn classify(code: u16, body: &str) -> Result<(), RestError> {
    match code {
        200..=299 => Ok(()),
        540 => Err(RestError::Paused),
        503 if !body.trim_start().starts_with('{') => Err(RestError::Paused),
        _ => Err(RestError::Status { code, message: error_message(body) }),
    }
}

#[derive(Clone)]
pub struct Rest {
    pub http: reqwest::Client,
    pub base: String,
    pub anon_key: String,
}

impl Rest {
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<String, RestError> {
        let resp = req.header("apikey", &self.anon_key).send().await.map_err(|e| RestError::Network(e.to_string()))?;
        let code = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| RestError::Network(e.to_string()))?;
        classify(code, &text).map(|_| text)
    }

    pub async fn auth_settings(&self) -> Result<Value, RestError> {
        let text = self.send(self.http.get(format!("{}/auth/v1/settings", self.base))).await?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn signup_anonymous(&self, now: i64) -> Result<AuthSession, RestError> {
        let text = self.send(self.http.post(format!("{}/auth/v1/signup", self.base)).header("content-type", "application/json").body(json!({}).to_string())).await?;
        parse_auth(&text, now).ok_or(RestError::Status { code: 200, message: "bad auth response".into() })
    }

    pub async fn refresh(&self, refresh_token: &str, now: i64) -> Result<AuthSession, RestError> {
        let url = format!("{}/auth/v1/token?grant_type=refresh_token", self.base);
        let text = self.send(self.http.post(url).header("content-type", "application/json").body(json!({ "refresh_token": refresh_token }).to_string())).await?;
        parse_auth(&text, now).ok_or(RestError::Status { code: 200, message: "bad auth response".into() })
    }

    pub async fn rpc(&self, token: &str, name: &str, args: Value) -> Result<Value, RestError> {
        let url = format!("{}/rest/v1/rpc/{name}", self.base);
        let text = self.send(self.http.post(url).bearer_auth(token).header("content-type", "application/json").body(args.to_string())).await?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Object paths directly under `prefix` in the link-blobs bucket (folders end without a file id).
    pub async fn storage_list(&self, token: &str, prefix: &str) -> Result<Vec<(String, bool)>, RestError> {
        let url = format!("{}/storage/v1/object/list/link-blobs", self.base);
        let text = self
            .send(self.http.post(url).bearer_auth(token).header("content-type", "application/json").body(json!({ "prefix": prefix, "limit": 1000, "offset": 0 }).to_string()))
            .await?;
        let items: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|i| {
                let name = i.get("name")?.as_str()?;
                let is_file = i.get("id").is_some_and(|v| !v.is_null());
                Some((format!("{}/{name}", prefix.trim_end_matches('/')), is_file))
            })
            .collect())
    }

    /// Every object path under `prefix`, walking folders (bounded depth).
    pub async fn storage_walk(&self, token: &str, prefix: &str) -> Result<Vec<String>, RestError> {
        let mut files = Vec::new();
        let mut folders = vec![(prefix.to_string(), 0)];
        while let Some((dir, depth)) = folders.pop() {
            for (path, is_file) in self.storage_list(token, &dir).await? {
                if is_file {
                    files.push(path);
                } else if depth < 4 {
                    folders.push((path, depth + 1));
                }
            }
        }
        Ok(files)
    }

    /// Edge Function call (`/functions/v1/<name>`) with the user token.
    pub async fn function(&self, token: &str, name: &str, body: Value) -> Result<Value, RestError> {
        let url = format!("{}/functions/v1/{name}", self.base);
        let text = self.send(self.http.post(url).bearer_auth(token).header("content-type", "application/json").body(body.to_string())).await?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// One object of the link-blobs bucket, read under the caller's RLS (members only).
    pub async fn storage_get(&self, token: &str, path: &str) -> Result<Vec<u8>, RestError> {
        let url = format!("{}/storage/v1/object/authenticated/link-blobs/{path}", self.base);
        let resp = self.http.get(url).bearer_auth(token).header("apikey", &self.anon_key).send().await.map_err(|e| RestError::Network(e.to_string()))?;
        let code = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| RestError::Network(e.to_string()))?;
        classify(code, &String::from_utf8_lossy(&body[..body.len().min(2048)]))?;
        Ok(body.to_vec())
    }

    /// Upload URL for a path and token returned by `link-upload`.
    pub fn signed_upload_url(&self, path: &str, token: &str) -> Option<String> {
        let safe = |s: &str, extra: &[u8]| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b));
        (safe(path, b"-_/") && safe(token, b"-_.")).then(|| format!("{}/storage/v1/object/upload/sign/link-blobs/{path}?token={token}", self.base))
    }

    /// PUT to a signed upload URL (the token in the URL authorizes it; upsert was granted).
    pub async fn signed_put(&self, url: &str, body: Vec<u8>) -> Result<(), RestError> {
        let req = self.http.put(url).header("x-upsert", "true").header("content-type", "application/octet-stream").body(body);
        self.send(req).await.map(|_| ())
    }

    pub async fn storage_delete(&self, token: &str, paths: &[String]) -> Result<(), RestError> {
        for batch in paths.chunks(100) {
            let url = format!("{}/storage/v1/object/link-blobs", self.base);
            self.send(self.http.delete(url).bearer_auth(token).header("content-type", "application/json").body(json!({ "prefixes": batch }).to_string())).await?;
        }
        Ok(())
    }
}

fn frame(topic: &str, event: &str, payload: Value, reference: u64, join_ref: Option<u64>) -> String {
    let mut f = json!({ "topic": topic, "event": event, "payload": payload, "ref": reference.to_string() });
    if let Some(j) = join_ref {
        f["join_ref"] = json!(j.to_string());
    }
    f.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_frame_shape() {
        let f: Value = serde_json::from_str(&join("nl-ab", 1, true, "pc", Some("jwt"))).unwrap();
        assert_eq!(f["topic"], "realtime:nl-ab");
        assert_eq!(f["event"], "phx_join");
        assert_eq!(f["ref"], "1");
        assert_eq!(f["join_ref"], "1");
        assert_eq!(f["payload"]["config"]["private"], true);
        assert_eq!(f["payload"]["config"]["broadcast"]["ack"], true);
        assert_eq!(f["payload"]["access_token"], "jwt");
        let anon: Value = serde_json::from_str(&join("t", 2, false, "", None)).unwrap();
        assert!(anon["payload"].get("access_token").is_none());
    }

    #[test]
    fn broadcast_and_heartbeat_frames() {
        let b: Value = serde_json::from_str(&broadcast("t", "m", json!({"c": "x"}), 5, 1)).unwrap();
        assert_eq!(b["payload"]["type"], "broadcast");
        assert_eq!(b["payload"]["event"], "m");
        assert_eq!(b["payload"]["payload"]["c"], "x");
        assert_eq!(b["join_ref"], "1");
        let h: Value = serde_json::from_str(&heartbeat(9)).unwrap();
        assert_eq!(h["topic"], "phoenix");
        assert!(h.get("join_ref").is_none());
    }

    #[test]
    fn auth_and_error_parsing() {
        let body = r#"{"access_token":"a","refresh_token":"r","expires_in":3600,"token_type":"bearer","user":{"id":"u1","is_anonymous":true}}"#;
        let s = parse_auth(body, 100).unwrap();
        assert_eq!((s.access_token.as_str(), s.refresh_token.as_str(), s.expires_at, s.user_id.as_str()), ("a", "r", 3700, "u1"));
        let with_at = r#"{"access_token":"a","refresh_token":"r","expires_at":5,"user":{"id":"u"}}"#;
        assert_eq!(parse_auth(with_at, 0).unwrap().expires_at, 5);
        assert!(parse_auth("{}", 0).is_none());
        assert_eq!(error_message(r#"{"code":"P0001","message":"not owner"}"#), "not owner");
        assert_eq!(error_message(r#"{"msg":"Anonymous sign-ins are disabled"}"#), "Anonymous sign-ins are disabled");
        assert_eq!(classify(204, ""), Ok(()));
        assert_eq!(classify(540, "{}"), Err(RestError::Paused));
        assert_eq!(classify(503, "<html>"), Err(RestError::Paused));
        assert_eq!(classify(400, r#"{"message":"bad mac"}"#).unwrap_err().message(), "bad mac");
    }

    #[test]
    fn broadcast_and_access_token_frames() {
        let f = Frame::parse(r#"{"topic":"realtime:nl-ab","event":"broadcast","payload":{"type":"broadcast","event":"hs1","payload":{"x":1}},"ref":null}"#).unwrap();
        let (topic, event, payload) = broadcast_of(&f).unwrap();
        assert_eq!((topic, event, payload["x"].as_i64()), ("nl-ab", "hs1", Some(1)));
        let a: Value = serde_json::from_str(&access_token("nl-ab", "jwt", 3, 1)).unwrap();
        assert_eq!(a["event"], "access_token");
        assert_eq!(a["payload"]["access_token"], "jwt");
        assert_eq!(a["topic"], "realtime:nl-ab");
    }

    #[test]
    fn signed_upload_urls_are_built_from_safe_parts_only() {
        let rest = Rest { http: reqwest::Client::new(), base: "https://r.supabase.co".into(), anon_key: "k".into() };
        assert_eq!(rest.signed_upload_url("nl-a/blob/p2c/ab/0", "x.y-z_1").as_deref(), Some("https://r.supabase.co/storage/v1/object/upload/sign/link-blobs/nl-a/blob/p2c/ab/0?token=x.y-z_1"));
        assert!(rest.signed_upload_url("../x", "t").is_none());
        assert!(rest.signed_upload_url("a/b", "t&x=1").is_none());
        assert!(rest.signed_upload_url("", "t").is_none());
    }

    #[test]
    fn parses_replies() {
        let ok = Frame::parse(r#"{"topic":"realtime:t","event":"phx_reply","payload":{"status":"ok","response":{}},"ref":"1"}"#).unwrap();
        assert_eq!(ok.reply().unwrap().0, "ok");
        assert_eq!(ok.reference.as_deref(), Some("1"));
        let err = Frame::parse(r#"{"topic":"realtime:t","event":"phx_reply","payload":{"status":"error","response":{"reason":"Unauthorized"}},"ref":"1"}"#).unwrap();
        let (status, resp) = err.reply().unwrap();
        assert_eq!(status, "error");
        assert_eq!(resp["reason"], "Unauthorized");
        let push = Frame::parse(r#"{"topic":"realtime:t","event":"broadcast","payload":{},"ref":null}"#).unwrap();
        assert!(push.reply().is_none());
        assert!(Frame::parse("not json").is_none());
    }
}
