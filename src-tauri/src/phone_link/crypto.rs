// Phone link crypto (design section 3, 5, 6, 7). X25519, HKDF-SHA256, HMAC-SHA256 and
// XChaCha20-Poly1305 from RustCrypto; the PWA uses @noble/* and checks the same vectors
// (phone/test-vectors.json).

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub type Key = [u8; 32];
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;

/// Direction bytes inside AADs. `p2c` = phone to PC.
pub const DIR_P2C: u8 = 1;
pub const DIR_C2P: u8 = 2;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out).expect("OS random generator failed");
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64u(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text).ok()
}

pub fn unb64u_key(text: &str) -> Option<Key> {
    unb64u(text)?.try_into().ok()
}

#[allow(dead_code)]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn hkdf_into(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .expect("hkdf output length");
}

pub fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> Key {
    let mut out = [0u8; 32];
    hkdf_into(ikm, salt, info, &mut out);
    out
}

fn mac_of(key: &[u8], parts: &[&[u8]]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    for part in parts {
        mac.update(part);
    }
    mac
}

pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    mac_of(key, parts).finalize().into_bytes().into()
}

/// Constant-time MAC check.
pub fn hmac_verify(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    mac_of(key, parts).verify_slice(tag).is_ok()
}

pub fn seal_with_nonce(key: &Key, nonce: &[u8; NONCE_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let ct = cipher
        .encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext, aad })
        .expect("xchacha encrypt");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    out
}

/// `nonce24 || ciphertext || tag` with a fresh random nonce.
pub fn seal(key: &Key, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    seal_with_nonce(key, &random_bytes::<NONCE_LEN>(), aad, plaintext)
}

pub fn open(key: &Key, aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < NONCE_LEN + TAG_LEN {
        return None;
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .ok()
}

pub struct KeyPair {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Self {
        Self::from_secret(random_bytes::<32>())
    }

    pub fn from_secret(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret).to_bytes();
        KeyPair { secret, public }
    }

    /// X25519. Rejects low-order peer keys (all-zero shared secret).
    pub fn agree(&self, peer: &[u8; 32]) -> Option<Zeroizing<Key>> {
        let shared = self.secret.diffie_hellman(&PublicKey::from(*peer));
        if !shared.was_contributory() {
            return None;
        }
        Some(Zeroizing::new(shared.to_bytes()))
    }
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

// Pairing (section 5).

pub fn pair_topic(ps: &Key) -> String {
    let mut id = [0u8; 16];
    hkdf_into(ps, b"", b"nectar-link v1 pair-topic", &mut id);
    format!("nlp-{}", hex(&id))
}

pub fn hello_key(ps: &Key) -> Key {
    hkdf32(ps, b"", b"nectar-link v1 hello")
}

pub fn hello_aad(ph_pub: &[u8; 32]) -> Vec<u8> {
    cat(&[b"hello", ph_pub])
}

pub fn device_key(shared: &Key, ps: &Key, pc_pub: &[u8; 32], ph_pub: &[u8; 32]) -> Key {
    hkdf32(shared, ps, &cat(&[b"nectar-link v1 device", pc_pub, ph_pub]))
}

pub fn sas(k_dev0: &Key) -> String {
    let mut four = [0u8; 4];
    hkdf_into(k_dev0, b"", b"nectar-link v1 sas", &mut four);
    format!("{:06}", u32::from_be_bytes(four) % 1_000_000)
}

pub fn device_topic(k_dev0: &Key) -> String {
    let mut id = [0u8; 16];
    hkdf_into(k_dev0, b"", b"nectar-link v1 topic", &mut id);
    format!("nl-{}", hex(&id))
}

pub fn rejoin_key(k_dev0: &Key) -> Key {
    hkdf32(k_dev0, b"", b"nectar-link v1 rejoin")
}

pub fn welcome_key(k_dev0: &Key) -> Key {
    hkdf32(k_dev0, b"", b"nectar-link v1 welcome")
}

// Relay RPC MACs. The ts is decimal text, as the SQL computes it (I-105).

pub fn owner_mac(k_owner: &Key, uid: &str, ts: i64) -> [u8; 32] {
    hmac(k_owner, &[b"nectar-link v1 owner", uid.as_bytes(), ts.to_string().as_bytes()])
}

pub fn rejoin_mac(k_rejoin: &Key, topic: &str, uid: &str, ts: i64) -> [u8; 32] {
    hmac(k_rejoin, &[b"nectar-link v1 rejoin", topic.as_bytes(), uid.as_bytes(), ts.to_string().as_bytes()])
}

// Session handshake (section 6.1).

pub fn hs1_parts<'a>(sid: &'a [u8; 16], e_ph: &'a [u8; 32], ts: &'a [u8; 8], ep: &'a [u8; 4]) -> [&'a [u8]; 5] {
    [b"hs1", sid, e_ph, ts, ep]
}

pub fn hs1_mac(k_dev: &Key, sid: &[u8; 16], e_ph: &[u8; 32], ts: u64, ep: u32) -> [u8; 32] {
    hmac(k_dev, &hs1_parts(sid, e_ph, &ts.to_be_bytes(), &ep.to_be_bytes()))
}

pub fn hs1_verify(k_dev: &Key, sid: &[u8; 16], e_ph: &[u8; 32], ts: u64, ep: u32, tag: &[u8]) -> bool {
    hmac_verify(k_dev, &hs1_parts(sid, e_ph, &ts.to_be_bytes(), &ep.to_be_bytes()), tag)
}

pub fn hs2_mac(k_dev: &Key, sid: &[u8; 16], e_ph: &[u8; 32], e_pc: &[u8; 32]) -> [u8; 32] {
    hmac(k_dev, &[b"hs2", sid, e_ph, e_pc])
}

pub fn hs2_verify(k_dev: &Key, sid: &[u8; 16], e_ph: &[u8; 32], e_pc: &[u8; 32], tag: &[u8]) -> bool {
    hmac_verify(k_dev, &[b"hs2", sid, e_ph, e_pc], tag)
}

pub struct SessionKeys {
    pub p2c: Zeroizing<Key>,
    pub c2p: Zeroizing<Key>,
    pub rk: Zeroizing<Key>,
}

pub fn session_keys(shared: &Key, k_dev: &Key, sid: &[u8; 16], e_ph: &[u8; 32], e_pc: &[u8; 32]) -> SessionKeys {
    let mut okm = Zeroizing::new([0u8; 96]);
    hkdf_into(shared, k_dev, &cat(&[b"nectar-link v1 session", sid, e_ph, e_pc]), okm.as_mut());
    let part = |i: usize| {
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&okm[i * 32..i * 32 + 32]);
        k
    };
    SessionKeys { p2c: part(0), c2p: part(1), rk: part(2) }
}

/// Device key ratchet (section 6.2).
pub fn rekey(k_dev: &Key, k_rk: &Key, next_ep: u32) -> Key {
    hkdf32(k_dev, k_rk, &cat(&[b"nectar-link v1 rekey", &next_ep.to_be_bytes()]))
}

// Envelopes (sections 6.3, 7.1, 7.4).

pub fn msg_aad(topic: &str, sid: &[u8; 16], dir: u8, n: u64) -> Vec<u8> {
    cat(&[b"nectar-link v1 msg", topic.as_bytes(), sid, &[dir], &n.to_be_bytes()])
}

pub fn chunk_aad(dir: u8, fid: &[u8; 16], index: u32, last: bool) -> Vec<u8> {
    cat(&[b"nectar-link v1 chunk", &[dir], fid, &index.to_be_bytes(), &[last as u8]])
}

pub fn mailbox_key(k_dev: &Key, dir: &str) -> Key {
    hkdf32(k_dev, b"", format!("nectar-link v1 mailbox {dir}").as_bytes())
}

pub fn mailbox_aad(topic: &str, dir: &str, fid: &[u8; 16], created: u64, ep: u32) -> Vec<u8> {
    cat(&[b"nectar-link v1 mbox", topic.as_bytes(), dir.as_bytes(), fid, &created.to_be_bytes(), &ep.to_be_bytes()])
}

#[cfg(test)]
pub(crate) mod vectors {
    use super::*;
    use serde_json::{json, Value};

    const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../phone/test-vectors.json");

    fn h(text: &str) -> Vec<u8> {
        unhex(text).unwrap()
    }
    fn k(text: &str) -> Key {
        h(text).try_into().unwrap()
    }

    /// Recomputes every vector from the fixed inputs.
    pub fn compute() -> Value {
        let ps = k("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let pc_eph = KeyPair::from_secret(k("a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf"));
        let ph_eph = KeyPair::from_secret(k("c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf"));
        let shared = pc_eph.agree(&ph_eph.public).unwrap();
        assert_eq!(*shared, *ph_eph.agree(&pc_eph.public).unwrap());
        let k_dev0 = device_key(&shared, &ps, &pc_eph.public, &ph_eph.public);
        let hello_nonce: [u8; 24] = h("101112131415161718191a1b1c1d1e1f2021222324252627").try_into().unwrap();
        let hello_pt = br#"{"name":"Pixel","uid":"00000000-0000-4000-8000-000000000001","ua":"test"}"#;
        let hello_c = seal_with_nonce(&hello_key(&ps), &hello_nonce, &hello_aad(&ph_eph.public), hello_pt);
        let welcome_nonce: [u8; 24] = h("303132333435363738393a3b3c3d3e3f4041424344454647").try_into().unwrap();
        let welcome_pt = br#"{"id":"AAECAwQFBgcICQoLDA0ODw","pc":"PC","perms":{}}"#;
        let welcome_c = seal_with_nonce(&welcome_key(&k_dev0), &welcome_nonce, b"welcome", welcome_pt);

        let sid: [u8; 16] = h("505152535455565758595a5b5c5d5e5f").try_into().unwrap();
        let e_ph = KeyPair::from_secret(k("e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"));
        let e_pc = KeyPair::from_secret(k("202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"));
        let ts: u64 = 1_762_500_000;
        let s_shared = e_pc.agree(&e_ph.public).unwrap();
        let sk = session_keys(&s_shared, &k_dev0, &sid, &e_ph.public, &e_pc.public);
        let k_dev1 = rekey(&k_dev0, &sk.rk, 1);
        let topic = device_topic(&k_dev0);
        let msg_nonce: [u8; 24] = h("606162636465666768696a6b6c6d6e6f7071727374757677").try_into().unwrap();
        let msg_pt = br#"{"t":"cmd","id":1,"op":"media.play_pause"}"#;
        let msg_c = seal_with_nonce(&sk.p2c, &msg_nonce, &msg_aad(&topic, &sid, DIR_P2C, 1), msg_pt);
        let fid: [u8; 16] = h("808182838485868788898a8b8c8d8e8f").try_into().unwrap();
        let chunk_nonce: [u8; 24] = h("a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7").try_into().unwrap();
        let k_file = k("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff000102030405060708090a0b0c0d0e0f");
        let chunk_c = seal_with_nonce(&k_file, &chunk_nonce, &chunk_aad(DIR_C2P, &fid, 0, true), b"hello file");
        let k_mbox = mailbox_key(&k_dev0, "c2p");
        let mbox_nonce: [u8; 24] = h("b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7").try_into().unwrap();
        let mbox_c = seal_with_nonce(&k_mbox, &mbox_nonce, &mailbox_aad(&topic, "c2p", &fid, ts, 0), b"{}");
        let k_rejoin = rejoin_key(&k_dev0);
        let k_owner = k("1111111111111111111111111111111111111111111111111111111111111111");
        let uid = "00000000-0000-4000-8000-000000000002";

        json!({
            "note": "Generated by cargo test (phone_link::crypto). Inputs are fixed; outputs must match in Rust and the PWA. Regenerate with NECTAR_WRITE_VECTORS=1.",
            "inputs": {
                "ps": hex(&ps),
                "pcEphSecret": "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf",
                "phEphSecret": "c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
                "helloNonce": hex(&hello_nonce),
                "helloPlaintext": String::from_utf8_lossy(hello_pt),
                "welcomeNonce": hex(&welcome_nonce),
                "welcomePlaintext": String::from_utf8_lossy(welcome_pt),
                "sid": hex(&sid),
                "ePhSecret": "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
                "ePcSecret": "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
                "ts": ts,
                "ep": 0,
                "msgNonce": hex(&msg_nonce),
                "msgPlaintext": String::from_utf8_lossy(msg_pt),
                "fid": hex(&fid),
                "chunkNonce": hex(&chunk_nonce),
                "fileKey": hex(&k_file),
                "chunkPlaintext": "hello file",
                "mboxNonce": hex(&mbox_nonce),
                "mboxPlaintext": "{}",
                "ownerKey": hex(&k_owner),
                "uid": uid
            },
            "outputs": {
                "pcEphPublic": hex(&pc_eph.public),
                "phEphPublic": hex(&ph_eph.public),
                "pairShared": hex(shared.as_ref()),
                "pairTopic": pair_topic(&ps),
                "helloKey": hex(&hello_key(&ps)),
                "helloCiphertext": hex(&hello_c),
                "deviceKey0": hex(&k_dev0),
                "sas": sas(&k_dev0),
                "deviceTopic": topic,
                "rejoinKey": hex(&k_rejoin),
                "welcomeKey": hex(&welcome_key(&k_dev0)),
                "welcomeCiphertext": hex(&welcome_c),
                "ePhPublic": hex(&e_ph.public),
                "ePcPublic": hex(&e_pc.public),
                "hs1Mac": hex(&hs1_mac(&k_dev0, &sid, &e_ph.public, ts, 0)),
                "hs2Mac": hex(&hs2_mac(&k_dev0, &sid, &e_ph.public, &e_pc.public)),
                "sessionShared": hex(s_shared.as_ref()),
                "kP2c": hex(sk.p2c.as_ref()),
                "kC2p": hex(sk.c2p.as_ref()),
                "kRk": hex(sk.rk.as_ref()),
                "deviceKey1": hex(&k_dev1),
                "msgAad": hex(&msg_aad(&topic, &sid, DIR_P2C, 1)),
                "msgCiphertext": hex(&msg_c),
                "chunkAad": hex(&chunk_aad(DIR_C2P, &fid, 0, true)),
                "chunkCiphertext": hex(&chunk_c),
                "mailboxKeyC2p": hex(&k_mbox),
                "mailboxAad": hex(&mailbox_aad(&topic, "c2p", &fid, ts, 0)),
                "mailboxCiphertext": hex(&mbox_c),
                "ownerMac": hex(&owner_mac(&k_owner, uid, ts as i64)),
                "rejoinMac": hex(&rejoin_mac(&k_rejoin, &topic, uid, ts as i64))
            }
        })
    }

    #[test]
    fn test_vectors_match_the_shared_file() {
        let fresh = compute();
        if std::env::var_os("NECTAR_WRITE_VECTORS").is_some() {
            let mut text = serde_json::to_string_pretty(&fresh).unwrap();
            text.push('\n');
            std::fs::write(FILE, text).unwrap();
        }
        let stored: Value = serde_json::from_str(&std::fs::read_to_string(FILE).expect("phone/test-vectors.json")).unwrap();
        assert_eq!(stored, fresh, "phone/test-vectors.json is out of date");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let key = random_bytes::<32>();
        let sealed = seal(&key, b"aad", b"secret");
        assert_eq!(open(&key, b"aad", &sealed).unwrap(), b"secret");
        assert!(open(&key, b"other", &sealed).is_none());
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&key, b"aad", &bad).is_none());
        assert!(open(&key, b"aad", &sealed[..20]).is_none());
        assert!(open(&random_bytes::<32>(), b"aad", &sealed).is_none());
        assert_ne!(seal(&key, b"aad", b"secret")[..24], sealed[..24], "nonces must differ");
    }

    #[test]
    fn x25519_agrees_and_rejects_low_order() {
        let a = KeyPair::generate();
        let b = KeyPair::generate();
        assert_eq!(*a.agree(&b.public).unwrap(), *b.agree(&a.public).unwrap());
        assert!(a.agree(&[0u8; 32]).is_none());
    }

    #[test]
    fn hmac_verify_is_exact() {
        let tag = hmac(b"k", &[b"a", b"b"]);
        assert!(hmac_verify(b"k", &[b"ab"], &tag));
        assert!(!hmac_verify(b"k", &[b"ab"], &tag[..31]));
        assert!(!hmac_verify(b"x", &[b"ab"], &tag));
    }

    #[test]
    fn derived_ids_have_the_relay_formats() {
        let ps = random_bytes::<32>();
        let pt = pair_topic(&ps);
        assert!(pt.starts_with("nlp-") && pt.len() == 36 && pt[4..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let dt = device_topic(&ps);
        assert!(dt.starts_with("nl-") && dt.len() == 35);
        let code = sas(&ps);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn hex_and_b64u() {
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert_eq!(unhex("00ff10").unwrap(), vec![0, 255, 16]);
        assert!(unhex("0").is_none() && unhex("zz").is_none());
        assert_eq!(unb64u(&b64u(b"\xff\xfe")).unwrap(), b"\xff\xfe");
        assert!(unb64u("a+b/").is_none());
        assert!(unb64u_key(&b64u(&[1u8; 31])).is_none());
    }

    #[test]
    fn handshake_macs_bind_every_field() {
        let k = random_bytes::<32>();
        let sid = random_bytes::<16>();
        let e = random_bytes::<32>();
        let tag = hs1_mac(&k, &sid, &e, 100, 3);
        assert!(hs1_verify(&k, &sid, &e, 100, 3, &tag));
        assert!(!hs1_verify(&k, &sid, &e, 101, 3, &tag));
        assert!(!hs1_verify(&k, &sid, &e, 100, 4, &tag));
        let e2 = random_bytes::<32>();
        let t2 = hs2_mac(&k, &sid, &e, &e2);
        assert!(hs2_verify(&k, &sid, &e, &e2, &t2));
        assert!(!hs2_verify(&k, &sid, &e2, &e, &t2));
    }
}
