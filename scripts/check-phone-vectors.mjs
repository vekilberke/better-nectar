// Independent check of phone/test-vectors.json with Node's own crypto (OpenSSL), so the Rust
// output is not only compared with itself. XChaCha20-Poly1305 is HChaCha20 (below) plus
// Node's ChaCha20-Poly1305. Run: node scripts/check-phone-vectors.mjs
import { createCipheriv, createHmac, createPrivateKey, createPublicKey, diffieHellman, hkdfSync } from "node:crypto";
import { readFileSync } from "node:fs";

const file = new URL("../phone/test-vectors.json", import.meta.url);
const { inputs: I, outputs: O } = JSON.parse(readFileSync(file, "utf8"));
const h = (s) => Buffer.from(s, "hex");
const u8 = (s) => Buffer.from(s, "utf8");
const cat = (...parts) => Buffer.concat(parts.map((p) => (typeof p === "string" ? u8(p) : p)));
const be32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const be64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(n)); return b; };
const hkdf = (ikm, salt, info, len = 32) => Buffer.from(hkdfSync("sha256", ikm, salt, info, len));
const hmac = (key, ...parts) => { const m = createHmac("sha256", key); parts.forEach((p) => m.update(typeof p === "string" ? u8(p) : p)); return m.digest(); };

const priv = (secret) => createPrivateKey({ key: cat(h("302e020100300506032b656e04220420"), secret), format: "der", type: "pkcs8" });
const pub = (secret) => createPublicKey(priv(secret)).export({ format: "der", type: "spki" }).subarray(12);
const dh = (secret, peer) => diffieHellman({ privateKey: priv(secret), publicKey: createPublicKey({ key: cat(h("302a300506032b656e032100"), peer), format: "der", type: "spki" }) });

function hchacha20(key, nonce16) {
  const s = new Uint32Array(16);
  s.set([0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
  for (let i = 0; i < 8; i++) s[4 + i] = key.readUInt32LE(i * 4);
  for (let i = 0; i < 4; i++) s[12 + i] = nonce16.readUInt32LE(i * 4);
  const rotl = (v, c) => ((v << c) | (v >>> (32 - c))) >>> 0;
  const qr = (a, b, c, d) => {
    s[a] = (s[a] + s[b]) >>> 0; s[d] = rotl(s[d] ^ s[a], 16);
    s[c] = (s[c] + s[d]) >>> 0; s[b] = rotl(s[b] ^ s[c], 12);
    s[a] = (s[a] + s[b]) >>> 0; s[d] = rotl(s[d] ^ s[a], 8);
    s[c] = (s[c] + s[d]) >>> 0; s[b] = rotl(s[b] ^ s[c], 7);
  };
  for (let i = 0; i < 10; i++) {
    qr(0, 4, 8, 12); qr(1, 5, 9, 13); qr(2, 6, 10, 14); qr(3, 7, 11, 15);
    qr(0, 5, 10, 15); qr(1, 6, 11, 12); qr(2, 7, 8, 13); qr(3, 4, 9, 14);
  }
  const out = Buffer.alloc(32);
  [0, 1, 2, 3, 12, 13, 14, 15].forEach((w, i) => out.writeUInt32LE(s[w], i * 4));
  return out;
}

function xseal(key, nonce24, aad, pt) {
  const sub = hchacha20(key, nonce24.subarray(0, 16));
  const c = createCipheriv("chacha20-poly1305", sub, cat(Buffer.alloc(4), nonce24.subarray(16)), { authTagLength: 16 });
  c.setAAD(aad);
  const body = Buffer.concat([c.update(pt), c.final()]);
  return cat(nonce24, body, c.getAuthTag());
}

const got = {};
const pcPub = pub(h(I.pcEphSecret));
const phPub = pub(h(I.phEphSecret));
const shared = dh(h(I.pcEphSecret), phPub);
const ps = h(I.ps);
const k0 = hkdf(shared, ps, cat("nectar-link v1 device", pcPub, phPub));
const topic = "nl-" + hkdf(k0, Buffer.alloc(0), "nectar-link v1 topic", 16).toString("hex");
got.pcEphPublic = pcPub.toString("hex");
got.phEphPublic = phPub.toString("hex");
got.pairShared = shared.toString("hex");
got.pairTopic = "nlp-" + hkdf(ps, Buffer.alloc(0), "nectar-link v1 pair-topic", 16).toString("hex");
got.helloKey = hkdf(ps, Buffer.alloc(0), "nectar-link v1 hello").toString("hex");
got.helloCiphertext = xseal(h(got.helloKey), h(I.helloNonce), cat("hello", phPub), u8(I.helloPlaintext)).toString("hex");
got.deviceKey0 = k0.toString("hex");
got.sas = String(hkdf(k0, Buffer.alloc(0), "nectar-link v1 sas", 4).readUInt32BE(0) % 1_000_000).padStart(6, "0");
got.deviceTopic = topic;
got.rejoinKey = hkdf(k0, Buffer.alloc(0), "nectar-link v1 rejoin").toString("hex");
got.welcomeKey = hkdf(k0, Buffer.alloc(0), "nectar-link v1 welcome").toString("hex");
got.welcomeCiphertext = xseal(h(got.welcomeKey), h(I.welcomeNonce), u8("welcome"), u8(I.welcomePlaintext)).toString("hex");

const sid = h(I.sid);
const ePh = pub(h(I.ePhSecret));
const ePc = pub(h(I.ePcSecret));
got.ePhPublic = ePh.toString("hex");
got.ePcPublic = ePc.toString("hex");
got.hs1Mac = hmac(k0, "hs1", sid, ePh, be64(I.ts), be32(I.ep)).toString("hex");
got.hs2Mac = hmac(k0, "hs2", sid, ePh, ePc).toString("hex");
const sShared = dh(h(I.ePcSecret), ePh);
got.sessionShared = sShared.toString("hex");
const okm = hkdf(sShared, k0, cat("nectar-link v1 session", sid, ePh, ePc), 96);
got.kP2c = okm.subarray(0, 32).toString("hex");
got.kC2p = okm.subarray(32, 64).toString("hex");
got.kRk = okm.subarray(64).toString("hex");
got.deviceKey1 = hkdf(k0, okm.subarray(64), cat("nectar-link v1 rekey", be32(1))).toString("hex");
const msgAad = cat("nectar-link v1 msg", topic, sid, Buffer.from([1]), be64(1));
got.msgAad = msgAad.toString("hex");
got.msgCiphertext = xseal(okm.subarray(0, 32), h(I.msgNonce), msgAad, u8(I.msgPlaintext)).toString("hex");
const fid = h(I.fid);
const chunkAad = cat("nectar-link v1 chunk", Buffer.from([2]), fid, be32(0), Buffer.from([1]));
got.chunkAad = chunkAad.toString("hex");
got.chunkCiphertext = xseal(h(I.fileKey), h(I.chunkNonce), chunkAad, u8(I.chunkPlaintext)).toString("hex");
const kMbox = hkdf(k0, Buffer.alloc(0), "nectar-link v1 mailbox c2p");
got.mailboxKeyC2p = kMbox.toString("hex");
const mboxAad = cat("nectar-link v1 mbox", topic, "c2p", fid, be64(I.ts), be32(0));
got.mailboxAad = mboxAad.toString("hex");
got.mailboxCiphertext = xseal(kMbox, h(I.mboxNonce), mboxAad, u8(I.mboxPlaintext)).toString("hex");
got.ownerMac = hmac(h(I.ownerKey), "nectar-link v1 owner", I.uid, String(I.ts)).toString("hex");
got.rejoinMac = hmac(h(got.rejoinKey), "nectar-link v1 rejoin", topic, I.uid, String(I.ts)).toString("hex");

let bad = 0;
for (const [k, v] of Object.entries(O)) {
  if (got[k] !== v) {
    bad++;
    console.error(`mismatch ${k}\n  file: ${v}\n  node: ${got[k]}`);
  }
}
const missing = Object.keys(got).filter((k) => !(k in O));
if (missing.length) { bad++; console.error("not in file:", missing.join(", ")); }
console.log(bad ? `${bad} problem(s)` : `all ${Object.keys(O).length} vectors match`);
process.exit(bad ? 1 : 0);
