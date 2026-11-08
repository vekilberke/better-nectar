// Trusted device records (Credential Manager, one per device) and the device key ratchet
// with clone detection (design sections 6.2 and 11).

use super::crypto::{self, b64u, Key};
use super::remote::Perms;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const SUPERSEDED_KEPT: usize = 2;

mod key_b64 {
    use super::super::crypto::{b64u, unb64u_key, Key};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(k: &Key, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64u(k))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        let text = String::deserialize(d)?;
        unb64u_key(&text).ok_or_else(|| serde::de::Error::custom("bad key"))
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct EpochKey {
    pub ep: u32,
    #[serde(with = "key_b64")]
    pub key: Key,
    /// When the PC stopped accepting this key (superseded or abandoned), in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<i64>,
}

impl EpochKey {
    pub fn new(ep: u32, key: Key) -> Self {
        EpochKey { ep, key, retired_at: None }
    }
}

impl Drop for EpochKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// What a valid `hs1` proved about the key it used.
#[derive(Debug, PartialEq, Eq)]
pub enum HsCheck {
    /// Accepted; carries the key the session is derived from.
    Ok(Key),
    /// Valid MAC under a superseded epoch, made after it was retired: a second copy of the key exists.
    Clone,
    Invalid,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct KeyRing {
    pub current: EpochKey,
    /// Previous key after the phone confirmed `rekeyed`, until a handshake under `current`.
    #[serde(default)]
    pub pending_prev: Option<EpochKey>,
    /// Key the PC proposed with `rekey` and the phone has not confirmed yet.
    #[serde(default)]
    pub proposed: Option<EpochKey>,
    /// Last epochs that must never be used again (kept only to recognise them).
    #[serde(default)]
    pub superseded: Vec<EpochKey>,
    /// Proposals dropped because a handshake under `current` came first. Either the phone
    /// never got the `rekey` (and never uses these), or it stored one and another copy of
    /// the old key connected meanwhile; a later handshake under one of them proves the latter.
    #[serde(default)]
    pub abandoned: Vec<EpochKey>,
}

impl KeyRing {
    pub fn new(k_dev0: Key) -> Self {
        KeyRing { current: EpochKey::new(0, k_dev0), pending_prev: None, proposed: None, superseded: Vec::new(), abandoned: Vec::new() }
    }

    fn abandon(&mut self, now: i64) {
        if let Some(mut p) = self.proposed.take() {
            p.retired_at = Some(now);
            self.abandoned.push(p);
            while self.abandoned.len() > SUPERSEDED_KEPT {
                self.abandoned.remove(0);
            }
        }
    }

    fn supersede(&mut self, mut old: EpochKey, now: i64) {
        if self.superseded.iter().any(|e| e.ep == old.ep) {
            return;
        }
        old.retired_at = Some(now);
        self.superseded.push(old);
        self.superseded.sort_by_key(|e| e.ep);
        while self.superseded.len() > SUPERSEDED_KEPT {
            self.superseded.remove(0);
        }
    }

    /// Checks `hs1` and updates the ring. The caller persists the ring when it changed.
    /// `now` is the PC's time in seconds.
    pub fn check_hs1(&mut self, sid: &[u8; 16], e_ph: &[u8; 32], ts: u64, ep: u32, tag: &[u8], now: i64) -> HsCheck {
        let valid = |k: &Key| crypto::hs1_verify(k, sid, e_ph, ts, ep, tag);
        if ep == self.current.ep && valid(&self.current.key) {
            // The phone did not get our last rekey, or already confirmed it.
            self.abandon(now);
            if let Some(prev) = self.pending_prev.take() {
                self.supersede(prev, now);
            }
            return HsCheck::Ok(self.current.key);
        }
        if let Some(next) = self.proposed.as_ref().filter(|p| p.ep == ep && valid(&p.key)).cloned() {
            // The phone stored the new key but its `rekeyed` never arrived.
            self.proposed = None;
            let old = std::mem::replace(&mut self.current, next);
            self.supersede(old, now);
            if let Some(prev) = self.pending_prev.take() {
                self.supersede(prev, now);
            }
            return HsCheck::Ok(self.current.key);
        }
        if let Some(prev) = self.pending_prev.as_ref().filter(|p| p.ep == ep && valid(&p.key)) {
            return HsCheck::Ok(prev.key);
        }
        // A handshake made before the key was retired is an old one replayed (the PC forgets
        // seen sids on restart), not proof of a copy.
        if let Some(old) = self.superseded.iter().chain(&self.abandoned).find(|s| s.ep == ep && valid(&s.key)) {
            let after = old.retired_at.is_none_or(|t| i64::try_from(ts).is_ok_and(|ts| ts >= t));
            return if after { HsCheck::Clone } else { HsCheck::Invalid };
        }
        HsCheck::Invalid
    }

    /// Starts a rekey after a session's first authenticated message in each direction.
    pub fn propose(&mut self, k_rk: &Key, session_ep: u32) -> Option<u32> {
        if session_ep != self.current.ep || self.proposed.is_some() {
            return None;
        }
        let next = session_ep.checked_add(1)?;
        let key = crypto::rekey(&self.current.key, k_rk, next);
        self.proposed = Some(EpochKey::new(next, key));
        Some(next)
    }

    /// The phone confirmed `rekeyed` for `ep`.
    pub fn confirm(&mut self, ep: u32, now: i64) -> bool {
        match self.proposed.take() {
            Some(next) if next.ep == ep => {
                let old = std::mem::replace(&mut self.current, next);
                if let Some(prev) = self.pending_prev.replace(old) {
                    self.supersede(prev, now);
                }
                true
            }
            other => {
                self.proposed = other;
                false
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub ua: String,
    pub paired_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
    pub perms: Perms,
    pub topic: String,
    #[serde(with = "key_b64")]
    pub rejoin_key: Key,
    pub phone_uid: String,
    pub keys: KeyRing,
    #[serde(default)]
    pub suspended: bool,
}

impl Drop for DeviceRecord {
    fn drop(&mut self) {
        self.rejoin_key.zeroize();
    }
}

/// Public view for the Settings UI (no keys).
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub ua: String,
    pub paired_at: i64,
    pub last_seen: Option<i64>,
    pub perms: Perms,
    pub suspended: bool,
    pub epoch: u32,
}

impl DeviceRecord {
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            ua: self.ua.clone(),
            paired_at: self.paired_at,
            last_seen: self.last_seen,
            perms: self.perms.clone(),
            suspended: self.suspended,
            epoch: self.keys.current.ep,
        }
    }
}

pub fn new_device_id() -> String {
    b64u(&crypto::random_bytes::<16>())
}

pub fn valid_device_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Storage for device records; Credential Manager in the app, memory in tests.
pub trait DeviceStore: Send {
    fn load_all(&self) -> Vec<DeviceRecord>;
    fn save(&mut self, record: &DeviceRecord) -> Result<(), String>;
    fn remove(&mut self, id: &str) -> bool;
}

pub struct CredentialDevices;

impl DeviceStore for CredentialDevices {
    fn load_all(&self) -> Vec<DeviceRecord> {
        let prefix = super::secrets::device_target("");
        super::secrets::list(&prefix)
            .iter()
            .filter_map(|t| super::secrets::read_json::<DeviceRecord>(t))
            .filter(|r| valid_device_id(&r.id))
            .collect()
    }
    fn save(&mut self, record: &DeviceRecord) -> Result<(), String> {
        if !valid_device_id(&record.id) {
            return Err("bad device id".into());
        }
        super::secrets::write_json(&super::secrets::device_target(&record.id), record)
    }
    fn remove(&mut self, id: &str) -> bool {
        valid_device_id(id) && super::secrets::delete(&super::secrets::device_target(id))
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryDevices(pub Vec<DeviceRecord>);

#[cfg(test)]
impl DeviceStore for MemoryDevices {
    fn load_all(&self) -> Vec<DeviceRecord> {
        self.0.clone()
    }
    fn save(&mut self, record: &DeviceRecord) -> Result<(), String> {
        self.0.retain(|r| r.id != record.id);
        self.0.push(record.clone());
        Ok(())
    }
    fn remove(&mut self, id: &str) -> bool {
        let before = self.0.len();
        self.0.retain(|r| r.id != id);
        before != self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone_link::crypto::{hs1_mac, random_bytes};

    struct Phone {
        ep: u32,
        key: Key,
    }

    impl Phone {
        fn hs1(&self) -> ([u8; 16], [u8; 32], u64, u32, [u8; 32]) {
            let sid = random_bytes::<16>();
            let e = random_bytes::<32>();
            (sid, e, 1000, self.ep, hs1_mac(&self.key, &sid, &e, 1000, self.ep))
        }
    }

    fn check(ring: &mut KeyRing, phone: &Phone) -> HsCheck {
        let (sid, e, ts, ep, tag) = phone.hs1();
        ring.check_hs1(&sid, &e, ts, ep, &tag, 1000)
    }

    #[test]
    fn full_ratchet_cycle() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        let mut phone = Phone { ep: 0, key: k0 };
        assert_eq!(check(&mut ring, &phone), HsCheck::Ok(k0));
        let rk = random_bytes::<32>();
        assert_eq!(ring.propose(&rk, 0), Some(1));
        assert_eq!(ring.propose(&rk, 0), None, "one proposal at a time");
        phone.key = crypto::rekey(&phone.key, &rk, 1);
        phone.ep = 1;
        assert!(!ring.confirm(2, 1000));
        assert!(ring.confirm(1, 1000));
        assert_eq!(ring.current.ep, 1);
        assert_eq!(ring.pending_prev.as_ref().unwrap().ep, 0);
        assert_eq!(check(&mut ring, &phone), HsCheck::Ok(phone.key));
        assert!(ring.pending_prev.is_none());
        assert_eq!(ring.superseded.iter().map(|e| e.ep).collect::<Vec<_>>(), vec![0]);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Clone);
    }

    #[test]
    fn pending_previous_still_works_until_new_epoch_is_used() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        let rk = random_bytes::<32>();
        ring.propose(&rk, 0);
        assert!(ring.confirm(1, 1000));
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Ok(k0));
        assert_eq!(ring.current.ep, 1);
    }

    #[test]
    fn lost_rekeyed_reply_is_recovered_from_the_next_handshake() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        let rk = random_bytes::<32>();
        ring.propose(&rk, 0);
        let k1 = crypto::rekey(&k0, &rk, 1);
        assert_eq!(check(&mut ring, &Phone { ep: 1, key: k1 }), HsCheck::Ok(k1));
        assert_eq!(ring.current.ep, 1);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Clone);
    }

    #[test]
    fn rekey_not_received_by_phone_drops_the_proposal() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        ring.propose(&random_bytes::<32>(), 0);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Ok(k0));
        assert!(ring.proposed.is_none());
        assert_eq!(ring.current.ep, 0);
    }

    #[test]
    fn copy_connecting_between_rekey_and_rekeyed_is_caught_when_the_real_phone_returns() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        let rk = random_bytes::<32>();
        ring.propose(&rk, 0);
        let k1 = crypto::rekey(&k0, &rk, 1);
        // The real phone stored K1; a copy of K0 connects before the PC sees `rekeyed`.
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Ok(k0));
        assert!(ring.proposed.is_none());
        // The copy's own session ratchets forward to a different epoch-1 key.
        let rk2 = random_bytes::<32>();
        ring.propose(&rk2, 0);
        assert!(ring.confirm(1, 1000));
        assert_eq!(check(&mut ring, &Phone { ep: 1, key: k1 }), HsCheck::Clone, "the real phone's key exposes the copy");
    }

    #[test]
    fn abandoned_proposal_the_phone_never_got_stays_harmless() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        ring.propose(&random_bytes::<32>(), 0);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Ok(k0));
        let rk = random_bytes::<32>();
        assert_eq!(ring.propose(&rk, 0), Some(1));
        assert!(ring.confirm(1, 1000));
        let k1 = crypto::rekey(&k0, &rk, 1);
        assert_eq!(check(&mut ring, &Phone { ep: 1, key: k1 }), HsCheck::Ok(k1));
        assert_eq!(ring.abandoned.len(), 1);
        for _ in 0..3 {
            ring.propose(&random_bytes::<32>(), 1);
            assert_eq!(check(&mut ring, &Phone { ep: 1, key: k1 }), HsCheck::Ok(k1));
        }
        assert_eq!(ring.abandoned.len(), SUPERSEDED_KEPT);
    }

    #[test]
    fn a_handshake_older_than_the_retirement_is_a_replay_not_a_clone() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        let (sid, e) = (random_bytes::<16>(), random_bytes::<32>());
        let captured = hs1_mac(&k0, &sid, &e, 900, 0);
        assert_eq!(ring.check_hs1(&sid, &e, 900, 0, &captured, 900), HsCheck::Ok(k0));
        let rk = random_bytes::<32>();
        ring.propose(&rk, 0);
        assert!(ring.confirm(1, 950));
        let k1 = crypto::rekey(&k0, &rk, 1);
        assert_eq!(check(&mut ring, &Phone { ep: 1, key: k1 }), HsCheck::Ok(k1));
        assert_eq!(ring.superseded[0].retired_at, Some(1000));
        assert_eq!(ring.check_hs1(&sid, &e, 900, 0, &captured, 1100), HsCheck::Invalid, "replayed after a restart");
        assert_eq!(ring.current.ep, 1);
        let (sid2, e2) = (random_bytes::<16>(), random_bytes::<32>());
        let fresh = hs1_mac(&k0, &sid2, &e2, 1100, 0);
        assert_eq!(ring.check_hs1(&sid2, &e2, 1100, 0, &fresh, 1100), HsCheck::Clone, "a new handshake under the old key");
        ring.superseded = vec![EpochKey::new(0, k0)];
        assert_eq!(ring.check_hs1(&sid, &e, 900, 0, &captured, 1100), HsCheck::Clone, "records from before retired_at stay strict");
    }

    #[test]
    fn forged_or_unknown_epochs_are_invalid_not_clone() {
        let k0 = random_bytes::<32>();
        let mut ring = KeyRing::new(k0);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: random_bytes::<32>() }), HsCheck::Invalid);
        assert_eq!(check(&mut ring, &Phone { ep: 7, key: k0 }), HsCheck::Invalid);
        let mut k = k0;
        for ep in 1..=4 {
            let rk = random_bytes::<32>();
            ring.propose(&rk, ep - 1);
            ring.confirm(ep, 1000);
            k = crypto::rekey(&k, &rk, ep);
            assert_eq!(check(&mut ring, &Phone { ep, key: k }), HsCheck::Ok(k));
        }
        assert_eq!(ring.superseded.len(), SUPERSEDED_KEPT);
        assert_eq!(check(&mut ring, &Phone { ep: 0, key: k0 }), HsCheck::Invalid, "only the last epochs are kept");
    }

    #[test]
    fn record_serializes_without_raw_bytes_and_fits_a_credential() {
        let mut ring = KeyRing::new(random_bytes::<32>());
        ring.propose(&random_bytes::<32>(), 0);
        ring.confirm(1, 1000);
        ring.propose(&random_bytes::<32>(), 1);
        let old = |ep, b| EpochKey { ep, key: [b; 32], retired_at: Some(1_762_500_000) };
        ring.superseded = vec![old(0, 1), old(1, 2)];
        ring.abandoned = vec![old(1, 3), old(2, 4)];
        let rec = DeviceRecord {
            id: new_device_id(),
            name: "x".repeat(40),
            ua: "y".repeat(80),
            paired_at: 1,
            last_seen: Some(2),
            perms: Perms::default(),
            topic: format!("nl-{}", "a".repeat(32)),
            rejoin_key: random_bytes::<32>(),
            phone_uid: "00000000-0000-4000-8000-000000000001".into(),
            keys: ring,
            suspended: false,
        };
        let text = serde_json::to_vec(&rec).unwrap();
        assert!(text.len() < 1500, "{} bytes", text.len());
        let back: DeviceRecord = serde_json::from_slice(&text).unwrap();
        assert_eq!(back.keys, rec.keys);
        assert!(valid_device_id(&rec.id));
        assert!(!valid_device_id("../x"));
    }
}
