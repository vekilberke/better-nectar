// Pairing offer and state machine (design section 5): QR with the pairing secret in the
// URL fragment, encrypted hello, typed 6-digit code, abort on a second device.

use super::crypto::{self, b64u, unb64u, Key, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

pub const OFFER_SECS: i64 = 300;
pub const MAX_WRONG_CODES: u32 = 3;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub name: String,
    pub ua: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AbortReason {
    SecondDevice,
    WrongCode,
    Expired,
    Cancelled,
}

#[derive(Debug, PartialEq)]
pub enum HelloResult {
    /// First valid hello: show the code input.
    Request(PairRequest),
    /// Retry of the same hello, or junk that does not decrypt.
    Ignore,
    Abort(AbortReason),
}

#[derive(Debug, PartialEq)]
pub enum CodeResult {
    Ok,
    Wrong { left: u32 },
    Abort(AbortReason),
    NoRequest,
}

struct Hello {
    ph_pub: [u8; 32],
    uid: String,
    name: String,
    ua: String,
    k_dev0: Zeroizing<Key>,
    sas: Zeroizing<String>,
}

/// Everything needed to call `link_create_device` and then finish the pairing.
pub struct Accepted {
    pub pair_topic: String,
    pub device_topic: String,
    pub phone_uid: String,
    pub rejoin_key: Key,
    pub k_dev0: Zeroizing<Key>,
    pub name: String,
    pub ua: String,
}

pub struct Offer {
    ps: Zeroizing<Key>,
    pc: KeyPair,
    pub pair_topic: String,
    pub expires_at: i64,
    pc_name: String,
    hello: Option<Hello>,
    wrong: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloWire {
    e: String,
    c: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloInner {
    name: String,
    uid: String,
    ua: String,
}

pub fn clean_text(text: &str, limit: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(limit).collect::<String>().trim().to_string()
}

pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

impl Offer {
    pub fn new(pc_name: &str, now: i64) -> Self {
        Self::with_secrets(pc_name, now, crypto::random_bytes::<32>(), KeyPair::generate())
    }

    pub fn with_secrets(pc_name: &str, now: i64, ps: Key, pc: KeyPair) -> Self {
        Offer {
            pair_topic: crypto::pair_topic(&ps),
            ps: Zeroizing::new(ps),
            pc,
            expires_at: now + OFFER_SECS,
            pc_name: clean_text(pc_name, 40),
            hello: None,
            wrong: 0,
        }
    }

    /// `https://<pwa>/#v1.<b64u(json)>`; everything after `#` stays on the phone.
    pub fn qr_url(&self, pwa_url: &str, relay_ref: &str) -> String {
        let payload = json!({
            "r": relay_ref,
            "s": b64u(self.ps.as_ref()),
            "p": b64u(&self.pc.public),
            "n": self.pc_name,
            "x": self.expires_at,
        });
        format!("{pwa_url}#v1.{}", b64u(payload.to_string().as_bytes()))
    }

    /// The phone waiting for its code, if a hello arrived.
    pub fn request(&self) -> Option<PairRequest> {
        self.hello.as_ref().map(|h| PairRequest { name: h.name.clone(), ua: h.ua.clone() })
    }

    pub fn expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }

    pub fn on_hello(&mut self, payload: &Value, now: i64) -> HelloResult {
        if self.expired(now) {
            return HelloResult::Abort(AbortReason::Expired);
        }
        let Ok(wire) = serde_json::from_value::<HelloWire>(payload.clone()) else { return HelloResult::Ignore };
        let Some(ph_pub) = unb64u(&wire.e).and_then(|v| <[u8; 32]>::try_from(v).ok()) else { return HelloResult::Ignore };
        let Some(sealed) = unb64u(&wire.c) else { return HelloResult::Ignore };
        let Some(plain) = crypto::open(&crypto::hello_key(&self.ps), &crypto::hello_aad(&ph_pub), &sealed) else {
            return HelloResult::Ignore;
        };
        if let Some(existing) = &self.hello {
            return if existing.ph_pub == ph_pub { HelloResult::Ignore } else { HelloResult::Abort(AbortReason::SecondDevice) };
        }
        let Ok(inner) = serde_json::from_slice::<HelloInner>(&plain) else { return HelloResult::Ignore };
        if !is_uuid(&inner.uid) {
            return HelloResult::Ignore;
        }
        let Some(shared) = self.pc.agree(&ph_pub) else { return HelloResult::Ignore };
        let k_dev0 = crypto::device_key(&shared, &self.ps, &self.pc.public, &ph_pub);
        let request = PairRequest { name: clean_text(&inner.name, 40), ua: clean_text(&inner.ua, 80) };
        self.hello = Some(Hello {
            ph_pub,
            uid: inner.uid,
            name: request.name.clone(),
            ua: request.ua.clone(),
            sas: Zeroizing::new(crypto::sas(&k_dev0)),
            k_dev0: Zeroizing::new(k_dev0),
        });
        HelloResult::Request(request)
    }

    pub fn submit_code(&mut self, code: &str, now: i64) -> CodeResult {
        if self.expired(now) {
            return CodeResult::Abort(AbortReason::Expired);
        }
        let Some(hello) = &self.hello else { return CodeResult::NoRequest };
        let typed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let ok = typed.len() == 6
            && typed.bytes().zip(hello.sas.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        if ok {
            return CodeResult::Ok;
        }
        self.wrong += 1;
        if self.wrong >= MAX_WRONG_CODES {
            CodeResult::Abort(AbortReason::WrongCode)
        } else {
            CodeResult::Wrong { left: MAX_WRONG_CODES - self.wrong }
        }
    }

    /// After a correct code. Consumes the offer; `ps` is wiped when it drops.
    pub fn accept(self) -> Option<Accepted> {
        let hello = self.hello?;
        let k = *hello.k_dev0;
        Some(Accepted {
            pair_topic: self.pair_topic.clone(),
            device_topic: crypto::device_topic(&k),
            phone_uid: hello.uid.clone(),
            rejoin_key: crypto::rejoin_key(&k),
            k_dev0: Zeroizing::new(k),
            name: hello.name.clone(),
            ua: hello.ua.clone(),
        })
    }
}

/// `welcome` payload for the phone (design 5.2 step 6).
pub fn welcome(k_dev0: &Key, device_id: &str, pc_name: &str, perms: &super::remote::Perms) -> Value {
    let inner = json!({ "id": device_id, "pc": clean_text(pc_name, 40), "perms": perms });
    json!({ "c": b64u(&crypto::seal(&crypto::welcome_key(k_dev0), b"welcome", inner.to_string().as_bytes())) })
}

/// QR as an SVG data URI (the settings CSP already allows `img-src data:`).
pub fn qr_svg_data_uri(text: &str) -> Result<String, String> {
    use base64::Engine;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).map_err(|e| e.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The phone side of pairing, as the PWA will do it.
    pub struct PhonePairing {
        pub eph: KeyPair,
        pub ps: Key,
        pub pc_pub: [u8; 32],
        pub uid: String,
    }

    impl PhonePairing {
        pub fn from_qr(url: &str, uid: &str) -> Self {
            let frag = url.split_once("#v1.").unwrap().1;
            let v: Value = serde_json::from_slice(&unb64u(frag).unwrap()).unwrap();
            PhonePairing {
                eph: KeyPair::generate(),
                ps: unb64u(v["s"].as_str().unwrap()).unwrap().try_into().unwrap(),
                pc_pub: unb64u(v["p"].as_str().unwrap()).unwrap().try_into().unwrap(),
                uid: uid.into(),
            }
        }
        pub fn hello(&self, name: &str) -> Value {
            let inner = json!({ "name": name, "uid": self.uid, "ua": "Android Chrome" });
            let c = crypto::seal(&crypto::hello_key(&self.ps), &crypto::hello_aad(&self.eph.public), inner.to_string().as_bytes());
            json!({ "e": b64u(&self.eph.public), "c": b64u(&c) })
        }
        pub fn k_dev0(&self) -> Key {
            let shared = self.eph.agree(&self.pc_pub).unwrap();
            crypto::device_key(&shared, &self.ps, &self.pc_pub, &self.eph.public)
        }
        pub fn code(&self) -> String {
            crypto::sas(&self.k_dev0())
        }
    }

    const UID: &str = "00000000-0000-4000-8000-0000000000aa";

    #[test]
    fn happy_path() {
        let mut offer = Offer::new("TEST-PC", 1000);
        let url = offer.qr_url("https://example.github.io/phone/", "abcdefghijklmnopqrst");
        assert!(url.starts_with("https://example.github.io/phone/#v1."));
        let phone = PhonePairing::from_qr(&url, UID);
        assert_eq!(crypto::pair_topic(&phone.ps), offer.pair_topic);
        let req = offer.on_hello(&phone.hello("Galaxy S26"), 1001);
        assert_eq!(req, HelloResult::Request(PairRequest { name: "Galaxy S26".into(), ua: "Android Chrome".into() }));
        assert_eq!(offer.on_hello(&phone.hello("Galaxy S26"), 1002), HelloResult::Ignore, "same device retry");
        assert_eq!(offer.submit_code(&phone.code(), 1003), CodeResult::Ok);
        let acc = offer.accept().unwrap();
        assert_eq!(*acc.k_dev0, phone.k_dev0());
        assert_eq!(acc.phone_uid, UID);
        assert_eq!(acc.device_topic, crypto::device_topic(&phone.k_dev0()));
        let w = welcome(&acc.k_dev0, "AAAAAAAAAAAAAAAAAAAAAA", "PC", &super::super::remote::Perms::new_device());
        let plain = crypto::open(&crypto::welcome_key(&phone.k_dev0()), b"welcome", &unb64u(w["c"].as_str().unwrap()).unwrap()).unwrap();
        let v: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(v["perms"]["openUrl"], "on");
    }

    #[test]
    fn second_device_aborts() {
        let mut offer = Offer::new("PC", 0);
        let url = offer.qr_url("https://x/", "r");
        let a = PhonePairing::from_qr(&url, UID);
        let b = PhonePairing::from_qr(&url, "00000000-0000-4000-8000-0000000000bb");
        assert!(matches!(offer.on_hello(&a.hello("A"), 1), HelloResult::Request(_)));
        assert_eq!(offer.on_hello(&b.hello("B"), 2), HelloResult::Abort(AbortReason::SecondDevice));
    }

    #[test]
    fn wrong_codes_expiry_and_junk() {
        let mut offer = Offer::new("PC", 0);
        assert_eq!(offer.submit_code("123456", 1), CodeResult::NoRequest);
        let url = offer.qr_url("https://x/", "r");
        let phone = PhonePairing::from_qr(&url, UID);
        assert_eq!(offer.on_hello(&json!({"e": "x", "c": "y"}), 1), HelloResult::Ignore);
        let stranger = PhonePairing { ps: crypto::random_bytes::<32>(), ..PhonePairing::from_qr(&url, UID) };
        assert_eq!(offer.on_hello(&stranger.hello("X"), 1), HelloResult::Ignore, "no QR, no valid hello");
        let bad_uid = PhonePairing::from_qr(&url, "not-a-uuid");
        assert_eq!(offer.on_hello(&bad_uid.hello("X"), 1), HelloResult::Ignore);
        assert!(matches!(offer.on_hello(&phone.hello("A"), 1), HelloResult::Request(_)));
        let wrong = if phone.code() == "000000" { "111111" } else { "000000" };
        assert_eq!(offer.submit_code(wrong, 2), CodeResult::Wrong { left: 2 });
        assert_eq!(offer.submit_code("12", 2), CodeResult::Wrong { left: 1 });
        assert_eq!(offer.submit_code(wrong, 2), CodeResult::Abort(AbortReason::WrongCode));
        let mut late = Offer::new("PC", 0);
        assert_eq!(late.on_hello(&json!({}), OFFER_SECS), HelloResult::Abort(AbortReason::Expired));
        assert_eq!(late.submit_code("123456", OFFER_SECS + 1), CodeResult::Abort(AbortReason::Expired));
    }

    #[test]
    fn qr_renders_and_names_are_cleaned() {
        let offer = Offer::new("A\u{0007}B", 0);
        let url = offer.qr_url("https://example.github.io/phone/", "abcdefghijklmnopqrst");
        assert!(url.len() < 400, "{}", url.len());
        assert!(qr_svg_data_uri(&url).unwrap().starts_with("data:image/svg+xml;base64,"));
        let frag = url.split_once("#v1.").unwrap().1;
        let v: Value = serde_json::from_slice(&unb64u(frag).unwrap()).unwrap();
        assert_eq!(v["n"], "AB");
        assert!(is_uuid(UID) && !is_uuid("00000000-0000-4000-8000-0000000000AA"));
    }
}
