// File and text transfer (design section 7): per-chunk encrypted Storage objects, the
// mailbox for an offline receiver, and the hardening of received files (name sanitizing,
// path confinement, Mark-of-the-Web). The protocol state lives in the engine; this file has
// the pure pieces and the async upload/download runners over the `Blobs` trait.

use super::crypto::{self, b64u, hex, unb64u, Key, DIR_C2P, DIR_P2C};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const CHUNK: u64 = 4 * 1024 * 1024;
/// `link-upload` issues at most this many URLs per fid (Edge Function `MAX_CHUNKS`).
pub const MAX_CHUNKS: u32 = 64;
/// Transfers in flight per device.
pub const MAX_ACTIVE: usize = 4;
pub const MAILBOX_MAX_AGE: i64 = 24 * 3600;
const MAILBOX_FUTURE: i64 = 300;
/// A sealed mailbox object: one inner message (<= 64 KiB) plus framing.
pub const MAILBOX_MAX_BYTES: usize = 128 * 1024;
const RETRIES: u32 = 3;
const MAX_NAME: usize = 120;
const MAX_MIME: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub t: String,
    pub fid: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub sha256: String,
    pub chunks: u32,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FidOnly {
    pub t: String,
    pub fid: String,
}

/// A checked offer: everything decoded and consistent.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub fid: [u8; 16],
    pub key: Key,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub sha256: [u8; 32],
    pub chunks: u32,
}

impl Plan {
    pub fn fid_hex(&self) -> String {
        hex(&self.fid)
    }

    pub fn offer(&self) -> Offer {
        Offer {
            t: "offer".into(),
            fid: self.fid_hex(),
            name: self.name.clone(),
            size: self.size,
            mime: self.mime.clone(),
            sha256: hex(&self.sha256),
            chunks: self.chunks,
            key: b64u(&self.key),
        }
    }

    /// A fresh outbound transfer for a file of `size` bytes with this hash.
    pub fn new_outbound(name: &str, size: u64, mime: &str, sha256: [u8; 32]) -> Option<Plan> {
        let chunks = chunk_count(size)?;
        Some(Plan {
            fid: crypto::random_bytes::<16>(),
            key: crypto::random_bytes::<32>(),
            name: sanitize_name(name),
            size,
            mime: clean_mime(mime),
            sha256,
            chunks,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XferError {
    Denied,
    TooBig,
    Disk,
    Invalid,
    Busy,
    Network,
    /// A chunk did not decrypt, or the size or SHA-256 did not match the offer.
    Corrupt,
    Io,
    Expired,
    /// The download folder cannot hold Mark-of-the-Web (FAT/exFAT).
    Folder,
}

impl XferError {
    pub fn code(self) -> &'static str {
        match self {
            XferError::Denied => "denied",
            XferError::TooBig => "tooBig",
            XferError::Disk => "disk",
            XferError::Invalid => "invalid",
            XferError::Busy => "busy",
            XferError::Network => "network",
            XferError::Corrupt => "corrupt",
            XferError::Io => "io",
            XferError::Expired => "expired",
            XferError::Folder => "folder",
        }
    }

    /// A later attempt may succeed, so the objects stay on the relay for a retry.
    pub fn transient(self) -> bool {
        matches!(self, XferError::Network | XferError::Disk | XferError::Io)
    }
}

pub fn is_fid(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Number of 4 MiB chunks (an empty file is one empty chunk); `None` above `MAX_CHUNKS`.
pub fn chunk_count(size: u64) -> Option<u32> {
    let n = size.div_ceil(CHUNK).max(1);
    (n <= u64::from(MAX_CHUNKS)).then_some(n as u32)
}

pub fn within_limit(size: u64, max_mb: u32) -> bool {
    size <= u64::from(max_mb) * 1024 * 1024
}

fn clean_mime(mime: &str) -> String {
    let m: String = mime.chars().filter(|c| c.is_ascii_graphic()).take(MAX_MIME).collect();
    if m.contains('/') {
        m
    } else {
        "application/octet-stream".into()
    }
}

impl Offer {
    /// Decodes and checks an offer; `None` is `invalid`.
    pub fn plan(&self) -> Option<Plan> {
        if self.t != "offer" || !is_fid(&self.fid) || self.name.is_empty() || self.name.chars().count() > 255 || self.mime.len() > MAX_MIME {
            return None;
        }
        if chunk_count(self.size)? != self.chunks {
            return None;
        }
        let fid: [u8; 16] = crypto::unhex(&self.fid)?.try_into().ok()?;
        if self.sha256.len() != 64 {
            return None;
        }
        let sha256: [u8; 32] = crypto::unhex(&self.sha256)?.try_into().ok()?;
        let key: Key = unb64u(&self.key)?.try_into().ok()?;
        Some(Plan { fid, key, name: sanitize_name(&self.name), size: self.size, mime: clean_mime(&self.mime), sha256, chunks: self.chunks })
    }
}

fn reserved_device_name(stem: &str) -> bool {
    let s = stem.trim_end().to_ascii_uppercase();
    if matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    (s.starts_with("COM") || s.starts_with("LPT")) && s.chars().count() == 4 && s.chars().last().is_some_and(|c| c.is_ascii_digit() || "¹²³".contains(c))
}

/// Types Explorer acts on just by showing them (icon, thumbnail or location lookups that can
/// reach a remote host), so a file from the phone never keeps one of these as its extension.
const SHELL_ACTIVE: &[&str] = &[
    "lnk", "pif", "url", "website", "scf", "library-ms", "searchconnector-ms", "search-ms", "appref-ms", "settingcontent-ms",
    "theme", "themepack", "deskthemepack",
];

fn shell_active(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "desktop.ini" || split_ext(&lower).1.strip_prefix('.').is_some_and(|ext| SHELL_ACTIVE.contains(&ext))
}

/// A received name made safe for `download_dir.join(name)` (design 7.2). Shell-active types
/// get `.txt` appended (I-153).
pub fn sanitize_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .filter(|c| !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF))
        .collect();
    // Leading dots stay (".gitignore"); Windows drops trailing dots and spaces itself.
    name = name.trim_start_matches(' ').trim_end_matches([' ', '.']).to_string();
    if name.is_empty() {
        name = "file".into();
    }
    if reserved_device_name(name.split('.').next().unwrap_or_default()) {
        name = format!("_{name}");
    }
    if shell_active(&name) {
        name.push_str(".txt");
    }
    if name.chars().count() > MAX_NAME {
        // A long "extension" is no extension: cut the whole name instead of dropping it.
        let (base, ext) = match split_ext(&name) {
            (b, e) if e.chars().count() <= 16 => (b, e),
            _ => (name.as_str(), ""),
        };
        let keep = MAX_NAME - ext.chars().count();
        name = format!("{}{}", base.chars().take(keep).collect::<String>().trim_end_matches([' ', '.']), ext);
    }
    name
}

/// ("photo", ".jpg"); a leading dot is part of the stem.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Reserves `dir\name` (or `name (n)`) with `create_new`, so two transfers never get the same file.
pub fn reserve_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let (base, ext) = split_ext(name);
    for n in 0..1000 {
        let file = if n == 0 { name.to_string() } else { format!("{base} ({n}){ext}") };
        let path = dir.join(&file);
        if !confined(dir, &path) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "path escapes the download folder"));
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name"))
}

/// The path is a direct child of `dir` (no separators, no stream, no `..`).
pub fn confined(dir: &Path, path: &Path) -> bool {
    path.parent() == Some(dir)
        && path.file_name().is_some_and(|f| {
            let f = f.to_string_lossy();
            !f.contains(['/', '\\', ':']) && f != "." && f != ".."
        })
}

/// `:Zone.Identifier` with `ZoneId=3`, so SmartScreen and Office treat the file as downloaded.
pub fn write_motw(path: &Path) -> std::io::Result<()> {
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\n")
}

pub fn read_motw(path: &Path) -> Option<String> {
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    std::fs::read_to_string(PathBuf::from(ads)).ok()
}

/// Free bytes on the volume of `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(dir.as_os_str()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// Whether the volume of `dir` has alternate data streams, which Mark-of-the-Web needs.
/// `None` when it cannot be told.
pub fn has_streams(dir: &Path) -> Option<bool> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};
    const FILE_NAMED_STREAMS: u32 = 0x0004_0000;
    let mut root = [0u16; 261];
    unsafe { GetVolumePathNameW(&HSTRING::from(dir.as_os_str()), &mut root) }.ok()?;
    let mut flags = 0u32;
    unsafe { GetVolumeInformationW(windows::core::PCWSTR(root.as_ptr()), None, None, None, Some(&mut flags), None) }.ok()?;
    Some(flags & FILE_NAMED_STREAMS != 0)
}

pub fn blob_path(topic: &str, dir: &str, fid: &str, i: u32) -> String {
    format!("{topic}/blob/{dir}/{fid}/{i}")
}

pub fn mbox_path(topic: &str, dir: &str, fid: &str) -> String {
    format!("{topic}/mbox/{dir}/{fid}")
}

pub fn mbox_prefix(topic: &str, dir: &str) -> String {
    format!("{topic}/mbox/{dir}")
}

pub fn seal_chunk(plan: &Plan, dir: u8, i: u32, data: &[u8]) -> Vec<u8> {
    crypto::seal(&plan.key, &crypto::chunk_aad(dir, &plan.fid, i, i + 1 == plan.chunks), data)
}

/// Decrypts chunk `i` and checks it has the length the offer implies.
pub fn open_chunk(plan: &Plan, dir: u8, i: u32, blob: &[u8]) -> Option<Vec<u8>> {
    let plain = crypto::open(&plan.key, &crypto::chunk_aad(dir, &plan.fid, i, i + 1 == plan.chunks), blob)?;
    let expected = if i + 1 == plan.chunks { plan.size.checked_sub(u64::from(i) * CHUNK)? } else { CHUNK };
    (plain.len() as u64 == expected).then_some(plain)
}

/// Size and SHA-256 of a local file, read in chunks.
pub fn hash_file(path: &Path) -> std::io::Result<(u64, [u8; 32])> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, h.finalize().into()))
}

pub fn guess_mime(name: &str) -> &'static str {
    let ext = split_ext(name).1.to_ascii_lowercase();
    match ext.as_str() {
        ".jpg" | ".jpeg" => "image/jpeg",
        ".png" => "image/png",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".heic" => "image/heic",
        ".pdf" => "application/pdf",
        ".txt" | ".log" | ".md" => "text/plain",
        ".mp4" => "video/mp4",
        ".mov" => "video/quicktime",
        ".mp3" => "audio/mpeg",
        ".zip" => "application/zip",
        ".json" => "application/json",
        ".csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

// Mailbox (design 7.4).

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxObject {
    pub ep: u32,
    pub created: u64,
    pub c: String,
}

pub fn seal_mailbox(k_dev: &Key, ep: u32, topic: &str, dir: &str, fid: &[u8; 16], created: u64, inner: &Value) -> Vec<u8> {
    let k = crypto::mailbox_key(k_dev, dir);
    let c = crypto::seal(&k, &crypto::mailbox_aad(topic, dir, fid, created, ep), inner.to_string().as_bytes());
    serde_json::to_vec(&MailboxObject { ep, created, c: b64u(&c) }).unwrap_or_default()
}

/// Opens an incoming mailbox object with the receiver's key for its epoch; checks the 24 h window.
pub fn open_mailbox(key_for: impl Fn(u32) -> Option<Key>, topic: &str, dir: &str, fid: &[u8; 16], bytes: &[u8], now: i64) -> Option<Vec<u8>> {
    if bytes.len() > MAILBOX_MAX_BYTES {
        return None;
    }
    let obj: MailboxObject = serde_json::from_slice(bytes).ok()?;
    let created = i64::try_from(obj.created).ok()?;
    if now - created > MAILBOX_MAX_AGE || created - now > MAILBOX_FUTURE {
        return None;
    }
    let k_dev = key_for(obj.ep)?;
    let k = crypto::mailbox_key(&k_dev, dir);
    crypto::open(&k, &crypto::mailbox_aad(topic, dir, fid, obj.created, obj.ep), &unb64u(&obj.c)?)
}

// Storage access, implemented over the relay in `client.rs` and in memory for tests.

pub trait Blobs: Send + Sync {
    /// Signed upload URLs from `link-upload` (`kind` is "blob" or "mailbox").
    fn authorize(&self, topic: &str, dir: &str, fid: &str, count: u32, kind: &str) -> impl Future<Output = Result<Vec<String>, XferError>> + Send;
    fn put(&self, url: &str, body: Vec<u8>) -> impl Future<Output = Result<(), XferError>> + Send;
    fn get(&self, path: &str) -> impl Future<Output = Result<Vec<u8>, XferError>> + Send;
    fn delete(&self, paths: Vec<String>) -> impl Future<Output = Result<(), XferError>> + Send;
    fn release(&self, topic: &str, dir: &str, fid: &str, kind: &str) -> impl Future<Output = Result<(), XferError>> + Send;
    /// Object names (the last path segment) directly under `prefix`.
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, XferError>> + Send;
}

async fn retry<T, F, Fut>(mut f: F) -> Result<T, XferError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, XferError>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Err(XferError::Network) if attempt + 1 < RETRIES => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500 * u64::from(attempt))).await;
            }
            r => return r,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReceiveJob {
    pub device_id: String,
    pub topic: String,
    pub plan: Plan,
    pub download_dir: PathBuf,
    /// Delivered through the mailbox: the mailbox object and its grant go too.
    pub mailbox: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    File(PathBuf),
    /// Nothing but the mailbox object (offline text).
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendJob {
    pub device_id: String,
    pub topic: String,
    pub fid: String,
    pub plan: Option<Plan>,
    pub source: Source,
    /// Sealed mailbox object, uploaded after the chunks when the phone is offline.
    pub mailbox: Option<Vec<u8>>,
}

/// Every object of a transfer and its grants, for the receiver to remove (design 7.1).
pub async fn discard<B: Blobs>(b: &B, topic: &str, dir: &str, fid: &str, chunks: u32, mailbox: bool) -> Result<(), XferError> {
    let mut paths: Vec<String> = (0..chunks).map(|i| blob_path(topic, dir, fid, i)).collect();
    if mailbox {
        paths.push(mbox_path(topic, dir, fid));
    }
    if !paths.is_empty() {
        retry(|| b.delete(paths.clone())).await?;
    }
    if chunks > 0 {
        let _ = retry(|| b.release(topic, dir, fid, "blob")).await;
    }
    if mailbox {
        let _ = retry(|| b.release(topic, dir, fid, "mailbox")).await;
    }
    Ok(())
}

/// Phone to PC: downloads, decrypts and verifies each chunk into `<name>.<fid>.part`, checks
/// the SHA-256, moves it to a reserved name, writes Mark-of-the-Web, then deletes the objects
/// and releases the grants. Returns the saved path.
pub async fn receive<B: Blobs>(b: &B, job: &ReceiveJob, progress: impl FnMut(u64) + Send) -> Result<PathBuf, XferError> {
    receive_until(b, job, progress, &AtomicBool::new(false)).await
}

/// Creates a part file that did not exist (never truncates a file already there).
fn create_part(dir: &Path, name: &str, tag: &str) -> Result<(PathBuf, std::fs::File), XferError> {
    for n in 0..10 {
        let file = if n == 0 { format!("{name}.{tag}.part") } else { format!("{name}.{tag}-{n}.part") };
        let path = dir.join(file);
        if !confined(dir, &path) {
            return Err(XferError::Invalid);
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(XferError::Io),
        }
    }
    Err(XferError::Io)
}

/// `receive` that stops (and keeps nothing) once `cancel` is set, e.g. when the device is removed.
pub async fn receive_until<B: Blobs>(b: &B, job: &ReceiveJob, mut progress: impl FnMut(u64) + Send, cancel: &AtomicBool) -> Result<PathBuf, XferError> {
    let plan = &job.plan;
    let fid = plan.fid_hex();
    let dir = &job.download_dir;
    std::fs::create_dir_all(dir).map_err(|_| XferError::Io)?;
    if free_space(dir).is_some_and(|f| f < plan.size + 64 * 1024 * 1024) {
        return Err(XferError::Disk);
    }
    let (part, file) = create_part(dir, &plan.name, &fid[..8])?;
    let stopped = || cancel.load(Ordering::Relaxed);
    let result = async {
        let mut file = file;
        // Fail before any byte is written when the volume cannot carry Mark-of-the-Web.
        write_motw(&part).map_err(|_| XferError::Io)?;
        let mut hash = Sha256::new();
        let mut done = 0u64;
        for i in 0..plan.chunks {
            if stopped() {
                return Err(XferError::Denied);
            }
            let path = blob_path(&job.topic, "p2c", &fid, i);
            let blob = retry(|| b.get(&path)).await?;
            let plain = open_chunk(plan, DIR_P2C, i, &blob).ok_or(XferError::Corrupt)?;
            hash.update(&plain);
            file.write_all(&plain).map_err(|_| XferError::Io)?;
            done += plain.len() as u64;
            progress(done);
        }
        file.flush().map_err(|_| XferError::Io)?;
        drop(file);
        if done != plan.size || <[u8; 32]>::from(hash.finalize()) != plan.sha256 {
            return Err(XferError::Corrupt);
        }
        if stopped() {
            return Err(XferError::Denied);
        }
        let target = reserve_path(dir, &plan.name).map_err(|_| XferError::Io)?;
        if std::fs::rename(&part, &target).is_err() {
            let _ = std::fs::remove_file(&target);
            return Err(XferError::Io);
        }
        if read_motw(&target).is_none() {
            write_motw(&target).map_err(|_| XferError::Io)?;
        }
        Ok(target)
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    let saved = result?;
    let _ = discard(b, &job.topic, "p2c", &fid, plan.chunks, job.mailbox).await;
    Ok(saved)
}

/// PC to phone: encrypts and uploads each chunk (after checking the file did not change size),
/// then the mailbox object when the phone is offline.
pub async fn send<B: Blobs>(b: &B, job: &SendJob, progress: impl FnMut(u64) + Send) -> Result<(), XferError> {
    send_until(b, job, progress, &AtomicBool::new(false)).await
}

/// `send` that stops before the next object once `cancel` is set.
pub async fn send_until<B: Blobs>(b: &B, job: &SendJob, mut progress: impl FnMut(u64) + Send, cancel: &AtomicBool) -> Result<(), XferError> {
    let stopped = || cancel.load(Ordering::Relaxed);
    if let (Some(plan), Source::File(path)) = (&job.plan, &job.source) {
        let mut file = std::fs::File::open(path).map_err(|_| XferError::Io)?;
        if file.metadata().map(|m| m.len()).map_err(|_| XferError::Io)? != plan.size {
            return Err(XferError::Corrupt);
        }
        let urls = retry(|| b.authorize(&job.topic, "c2p", &job.fid, plan.chunks, "blob")).await?;
        if urls.len() != plan.chunks as usize {
            return Err(XferError::Network);
        }
        let mut done = 0u64;
        let mut buf = vec![0u8; CHUNK as usize];
        for (i, url) in urls.iter().enumerate() {
            let i = i as u32;
            let len = (plan.size - u64::from(i) * CHUNK).min(CHUNK) as usize;
            file.seek(SeekFrom::Start(u64::from(i) * CHUNK)).map_err(|_| XferError::Io)?;
            file.read_exact(&mut buf[..len]).map_err(|_| XferError::Io)?;
            if stopped() {
                return Err(XferError::Denied);
            }
            let blob = seal_chunk(plan, DIR_C2P, i, &buf[..len]);
            retry(|| b.put(url, blob.clone())).await?;
            done += len as u64;
            progress(done);
        }
    }
    if let Some(obj) = &job.mailbox {
        if stopped() {
            return Err(XferError::Denied);
        }
        let urls = retry(|| b.authorize(&job.topic, "c2p", &job.fid, 1, "mailbox")).await?;
        let url = urls.first().ok_or(XferError::Network)?;
        retry(|| b.put(url, obj.clone())).await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::{Arc, Mutex};

    /// The relay's bucket in memory: upload URLs are `mem://<path>`.
    #[derive(Clone, Default)]
    pub struct MemBlobs {
        pub objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        pub released: Arc<Mutex<Vec<String>>>,
        pub fail_gets: Arc<Mutex<HashSet<String>>>,
    }

    impl Blobs for MemBlobs {
        async fn authorize(&self, topic: &str, dir: &str, fid: &str, count: u32, kind: &str) -> Result<Vec<String>, XferError> {
            Ok(if kind == "mailbox" {
                vec![format!("mem://{}", mbox_path(topic, dir, fid))]
            } else {
                (0..count).map(|i| format!("mem://{}", blob_path(topic, dir, fid, i))).collect()
            })
        }
        async fn put(&self, url: &str, body: Vec<u8>) -> Result<(), XferError> {
            self.objects.lock().unwrap().insert(url.trim_start_matches("mem://").to_string(), body);
            Ok(())
        }
        async fn get(&self, path: &str) -> Result<Vec<u8>, XferError> {
            if self.fail_gets.lock().unwrap().contains(path) {
                return Err(XferError::Network);
            }
            self.objects.lock().unwrap().get(path).cloned().ok_or(XferError::Network)
        }
        async fn delete(&self, paths: Vec<String>) -> Result<(), XferError> {
            let mut o = self.objects.lock().unwrap();
            for p in paths {
                o.remove(&p);
            }
            Ok(())
        }
        async fn release(&self, _topic: &str, dir: &str, fid: &str, kind: &str) -> Result<(), XferError> {
            self.released.lock().unwrap().push(format!("{dir}/{fid}/{kind}"));
            Ok(())
        }
        async fn list(&self, prefix: &str) -> Result<Vec<String>, XferError> {
            let p = format!("{}/", prefix.trim_end_matches('/'));
            Ok(self.objects.lock().unwrap().keys().filter_map(|k| k.strip_prefix(&p)).filter(|r| !r.contains('/')).map(str::to_string).collect())
        }
    }

    pub fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nectar-{tag}-{}", hex(&crypto::random_bytes::<6>())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
    }

    /// Uploads `data` as the phone would (p2c chunks) and returns the plan.
    pub fn phone_upload(b: &MemBlobs, topic: &str, name: &str, data: &[u8]) -> Plan {
        let plan = Plan::new_outbound(name, data.len() as u64, "application/octet-stream", Sha256::digest(data).into()).unwrap();
        for i in 0..plan.chunks {
            let start = (u64::from(i) * CHUNK) as usize;
            let end = (start + CHUNK as usize).min(data.len());
            let blob = seal_chunk(&plan, DIR_P2C, i, &data[start..end]);
            b.objects.lock().unwrap().insert(blob_path(topic, "p2c", &plan.fid_hex(), i), blob);
        }
        plan
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_name("..\\..\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_name("a/b/../c.txt"), "c.txt");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name("   "), "file");
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_name("com1.tar.gz"), "_com1.tar.gz");
        assert_eq!(sanitize_name("LPT9"), "_LPT9");
        assert_eq!(sanitize_name("COM\u{00B9}.txt"), "_COM\u{00B9}.txt");
        assert_eq!(sanitize_name("console.txt"), "console.txt");
        assert_eq!(sanitize_name("trailing. . "), "trailing");
        assert_eq!(sanitize_name("a<b>c:d\"e|f?g*h\u{0007}.txt"), "abcdefgh.txt");
        assert_eq!(sanitize_name("invoice\u{202E}gpj.exe"), "invoicegpj.exe", "bidi override dropped");
        assert_eq!(sanitize_name("file.txt:Zone.Identifier"), "file.txtZone.Identifier");
        let long = format!("{}.jpeg", "x".repeat(300));
        let s = sanitize_name(&long);
        assert_eq!(s.chars().count(), 120);
        assert!(s.ends_with(".jpeg"));
    }


    #[test]
    fn shell_active_types_from_the_phone_become_text_files() {
        for (raw, safe) in [
            ("photo.LNK", "photo.LNK.txt"),
            ("x.url", "x.url.txt"),
            ("a.b.Library-ms", "a.b.Library-ms.txt"),
            ("s.searchConnector-ms", "s.searchConnector-ms.txt"),
            ("e.scf", "e.scf.txt"),
            ("Desktop.ini", "Desktop.ini.txt"),
            ("x.lnk. . ", "x.lnk.txt"),
        ] {
            assert_eq!(sanitize_name(raw), safe, "{raw}");
        }
        assert_eq!(sanitize_name("notes.txt"), "notes.txt");
        assert_eq!(sanitize_name("lnk"), "lnk");
        let long = sanitize_name(&format!("{}.lnk", "x".repeat(300)));
        assert!(long.ends_with(".lnk.txt") || long.ends_with(".txt"), "{long}");
        assert!(long.chars().count() <= 120 && !long.to_lowercase().ends_with(".lnk"));
    }

    #[test]
    fn names_with_spaced_or_odd_device_stems_and_empty_bases_stay_safe() {
        assert_eq!(sanitize_name("CON .txt"), "_CON .txt");
        assert_eq!(sanitize_name("conin$.log"), "_conin$.log");
        assert_eq!(sanitize_name("con."), "_con");
        assert_eq!(sanitize_name("Aux"), "_Aux");
        assert_eq!(sanitize_name("COM10.txt"), "COM10.txt");
        assert_eq!(sanitize_name("\u{200B}\u{202E}"), "file");
        assert_eq!(sanitize_name(r"C:\Users\x\..\..\boot.ini"), "boot.ini");
        assert_eq!(sanitize_name("dir/"), "file");
        let dots = format!("{}.txt", ". ".repeat(100));
        let s = sanitize_name(&dots);
        assert!(s.chars().count() <= 120 && s.ends_with(".txt") && !s.is_empty());
        let long_ext = format!("a.{}", "e".repeat(200));
        let s = sanitize_name(&long_ext);
        assert!(!s.is_empty() && s.chars().count() <= 120, "a very long \"extension\" still gives a legal name");
        let report = format!("report.{}", "x".repeat(130));
        let s = sanitize_name(&report);
        assert_eq!(s.chars().count(), 120);
        assert!(s.starts_with("report.xxx"), "the whole name is cut, not dropped to its stem: {s}");
    }

    #[test]
    fn reserved_paths_stay_inside_and_never_collide() {
        let dir = temp_dir("reserve");
        let a = reserve_path(&dir, "a.txt").unwrap();
        let b = reserve_path(&dir, "a.txt").unwrap();
        let c = reserve_path(&dir, "a.txt").unwrap();
        assert_eq!(a.file_name().unwrap(), "a.txt");
        assert_eq!(b.file_name().unwrap(), "a (1).txt");
        assert_eq!(c.file_name().unwrap(), "a (2).txt");
        assert!(confined(&dir, &a));
        assert!(!confined(&dir, &dir.join("..").join("x")));
        assert!(!confined(&dir, &dir.join("sub").join("x")));
        assert!(!confined(&dir, &dir.join("x:stream")));
        assert!(reserve_path(&dir, "..\\x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offers_are_checked() {
        let plan = Plan::new_outbound("a.bin", 9 * 1024 * 1024, "application/x", [7; 32]).unwrap();
        assert_eq!(plan.chunks, 3);
        assert_eq!(plan.offer().plan().unwrap(), plan);
        let mut o = plan.offer();
        o.chunks = 2;
        assert!(o.plan().is_none(), "chunk count must match the size");
        let mut o = plan.offer();
        o.key = "short".into();
        assert!(o.plan().is_none());
        let mut o = plan.offer();
        o.sha256 = "ab".into();
        assert!(o.plan().is_none());
        let mut o = plan.offer();
        o.fid = "XYZ".into();
        assert!(o.plan().is_none());
        assert_eq!(chunk_count(0), Some(1));
        assert_eq!(chunk_count(CHUNK), Some(1));
        assert_eq!(chunk_count(CHUNK + 1), Some(2));
        assert_eq!(chunk_count(200 * 1024 * 1024), Some(50));
        assert_eq!(chunk_count(64 * CHUNK + 1), None);
        assert!(within_limit(100 * 1024 * 1024, 100));
        assert!(!within_limit(100 * 1024 * 1024 + 1, 100));
        assert_eq!(clean_mime("text/plain; charset=utf-8"), "text/plain;charset=utf-8");
        assert_eq!(clean_mime("garbage"), "application/octet-stream");
    }

    #[test]
    fn chunks_bind_index_direction_and_last_flag() {
        let plan = Plan::new_outbound("x", CHUNK + 5, "a/b", [0; 32]).unwrap();
        let c0 = seal_chunk(&plan, DIR_C2P, 0, &vec![1; CHUNK as usize]);
        let c1 = seal_chunk(&plan, DIR_C2P, 1, &[2; 5]);
        assert!(open_chunk(&plan, DIR_C2P, 0, &c0).is_some());
        assert!(open_chunk(&plan, DIR_C2P, 1, &c1).is_some());
        assert!(open_chunk(&plan, DIR_C2P, 1, &c0).is_none(), "reordered");
        assert!(open_chunk(&plan, DIR_P2C, 0, &c0).is_none(), "other direction");
        let mut short = plan.clone();
        short.chunks = 1;
        short.size = CHUNK;
        assert!(open_chunk(&short, DIR_C2P, 0, &c0).is_none(), "a non-last chunk cannot end the file");
    }

    #[test]
    fn receive_saves_verifies_marks_and_cleans_up() {
        let b = MemBlobs::default();
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let data: Vec<u8> = (0..(CHUNK as usize + 1000)).map(|i| (i % 251) as u8).collect();
        let plan = phone_upload(&b, topic, "../holiday photo.jpg", &data);
        let dir = temp_dir("recv");
        std::fs::write(dir.join("holiday photo.jpg"), b"already here").unwrap();
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan: plan.clone(), download_dir: dir.clone(), mailbox: false };
        let mut seen = Vec::new();
        let path = rt().block_on(receive(&b, &job, |d| seen.push(d))).unwrap();
        assert_eq!(path.file_name().unwrap(), "holiday photo (1).jpg");
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(read_motw(&path).unwrap(), "[ZoneTransfer]\r\nZoneId=3\r\n");
        assert_eq!(seen, vec![CHUNK, data.len() as u64]);
        assert!(b.objects.lock().unwrap().is_empty(), "objects deleted");
        assert_eq!(*b.released.lock().unwrap(), vec![format!("p2c/{}/blob", plan.fid_hex())]);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no .part left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn receive_never_truncates_or_deletes_an_existing_part_file() {
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let dir = temp_dir("recv-part");
        let b = MemBlobs::default();
        let mut plan = phone_upload(&b, topic, "report.docx", b"hello");
        plan.sha256 = [0; 32];
        let theirs = dir.join(format!("report.docx.{}.part", &plan.fid_hex()[..8]));
        std::fs::write(&theirs, b"user data").unwrap();
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        assert_eq!(rt().block_on(receive(&b, &job, |_| {})), Err(XferError::Corrupt));
        assert_eq!(std::fs::read(&theirs).unwrap(), b"user data");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only the user's file is left");

        let b = MemBlobs::default();
        let plan = phone_upload(&b, topic, "report.docx", b"fine");
        let part = dir.join(format!("report.docx.{}.part", &plan.fid_hex()[..8]));
        std::fs::write(&part, b"also theirs").unwrap();
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        let path = rt().block_on(receive(&b, &job, |_| {})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"fine");
        assert_eq!(std::fs::read(&part).unwrap(), b"also theirs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_transfer_keeps_no_file_and_uploads_nothing_more() {
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let dir = temp_dir("recv-cancel");
        let b = MemBlobs::default();
        let plan = phone_upload(&b, topic, "a.txt", b"hello");
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        let stop = AtomicBool::new(true);
        assert_eq!(rt().block_on(receive_until(&b, &job, |_| {}, &stop)), Err(XferError::Denied));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no file and no .part");
        assert!(!b.objects.lock().unwrap().is_empty(), "the relay cleanup of the removed device takes the objects");

        let src = dir.join("out.bin");
        std::fs::write(&src, b"data").unwrap();
        let plan = Plan::new_outbound("out.bin", 4, "a/b", [0; 32]).unwrap();
        let b = MemBlobs::default();
        let job = SendJob { device_id: "d".into(), topic: topic.into(), fid: plan.fid_hex(), plan: Some(plan), source: Source::File(src), mailbox: Some(vec![1]) };
        assert_eq!(rt().block_on(send_until(&b, &job, |_| {}, &stop)), Err(XferError::Denied));
        assert!(b.objects.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn receive_refuses_a_wrong_hash_or_a_swapped_chunk() {
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let dir = temp_dir("recv-bad");
        let b = MemBlobs::default();
        let mut plan = phone_upload(&b, topic, "a.txt", b"hello");
        plan.sha256 = [0; 32];
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        assert_eq!(rt().block_on(receive(&b, &job, |_| {})), Err(XferError::Corrupt));

        let b = MemBlobs::default();
        let data = vec![5u8; CHUNK as usize * 2];
        let plan = phone_upload(&b, topic, "b.bin", &data);
        {
            let mut o = b.objects.lock().unwrap();
            let (p0, p1) = (blob_path(topic, "p2c", &plan.fid_hex(), 0), blob_path(topic, "p2c", &plan.fid_hex(), 1));
            let (c0, c1) = (o[&p0].clone(), o[&p1].clone());
            o.insert(p0, c1);
            o.insert(p1, c0);
        }
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        assert_eq!(rt().block_on(receive(&b, &job, |_| {})), Err(XferError::Corrupt));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing kept from a bad transfer");
        assert!(!b.objects.lock().unwrap().is_empty(), "a failed download leaves the objects for the engine to discard");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_uploads_chunks_the_phone_can_open_and_the_mailbox_object() {
        let b = MemBlobs::default();
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let dir = temp_dir("send");
        let src = dir.join("report.pdf");
        let data: Vec<u8> = (0..(CHUNK as usize * 2 + 7)).map(|i| (i % 13) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let (size, sha) = hash_file(&src).unwrap();
        let plan = Plan::new_outbound("report.pdf", size, guess_mime("report.pdf"), sha).unwrap();
        let k_dev = [9u8; 32];
        let obj = seal_mailbox(&k_dev, 3, topic, "c2p", &plan.fid, 1000, &serde_json::to_value(plan.offer()).unwrap());
        let job = SendJob { device_id: "d".into(), topic: topic.into(), fid: plan.fid_hex(), plan: Some(plan.clone()), source: Source::File(src.clone()), mailbox: Some(obj) };
        rt().block_on(send(&b, &job, |_| {})).unwrap();
        {
            let o = b.objects.lock().unwrap();
            let mut got = Vec::new();
            for i in 0..plan.chunks {
                got.extend(open_chunk(&plan, DIR_C2P, i, &o[&blob_path(topic, "c2p", &plan.fid_hex(), i)]).unwrap());
            }
            assert_eq!(got, data);
            let mbox = &o[&mbox_path(topic, "c2p", &plan.fid_hex())];
            let inner = open_mailbox(|ep| (ep == 3).then_some(k_dev), topic, "c2p", &plan.fid, mbox, 1000).unwrap();
            let offer: Offer = serde_json::from_slice(&inner).unwrap();
            assert_eq!(offer.plan().unwrap(), plan);
        }
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(rt().block_on(send(&b, &job, |_| {})), Err(XferError::Corrupt), "a file that changed size is not sent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mailbox_objects_are_bound_and_expire() {
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let k = [4u8; 32];
        let fid = [1u8; 16];
        let obj = seal_mailbox(&k, 0, topic, "p2c", &fid, 10_000, &serde_json::json!({"t":"text","text":"hi"}));
        let key = |ep: u32| (ep == 0).then_some(k);
        assert!(open_mailbox(key, topic, "p2c", &fid, &obj, 10_000).is_some());
        assert!(open_mailbox(key, topic, "c2p", &fid, &obj, 10_000).is_none(), "direction");
        assert!(open_mailbox(key, topic, "p2c", &[2; 16], &obj, 10_000).is_none(), "fid");
        assert!(open_mailbox(key, "nl-ffffffffffffffffffffffffffffffff", "p2c", &fid, &obj, 10_000).is_none(), "topic");
        assert!(open_mailbox(key, topic, "p2c", &fid, &obj, 10_000 + MAILBOX_MAX_AGE + 1).is_none(), "older than 24 h");
        assert!(open_mailbox(key, topic, "p2c", &fid, &obj, 10_000 - 301).is_none(), "from the future");
        assert!(open_mailbox(|_| None, topic, "p2c", &fid, &obj, 10_000).is_none(), "unknown epoch");
        let mut v: serde_json::Value = serde_json::from_slice(&obj).unwrap();
        v["created"] = serde_json::json!(10_001);
        assert!(open_mailbox(key, topic, "p2c", &fid, &serde_json::to_vec(&v).unwrap(), 10_001).is_none(), "created is in the AAD");
    }

    #[test]
    fn mem_blobs_list_only_direct_children() {
        let b = MemBlobs::default();
        rt().block_on(async {
            b.put("mem://t/mbox/p2c/aa", vec![]).await.unwrap();
            b.put("mem://t/mbox/p2c/bb", vec![]).await.unwrap();
            b.put("mem://t/mbox/c2p/cc", vec![]).await.unwrap();
            let mut l = b.list("t/mbox/p2c").await.unwrap();
            l.sort();
            assert_eq!(l, vec!["aa", "bb"]);
        });
    }

    #[test]
    fn name_edge_cases_stay_safe() {
        assert_eq!(sanitize_name("CON .txt"), "_CON .txt", "a space before the dot is still a device name");
        assert_eq!(sanitize_name("aux."), "_aux");
        assert_eq!(sanitize_name("<>:\"|?*"), "file");
        assert_eq!(sanitize_name("a\u{200B}b\u{FEFF}.txt"), "ab.txt", "zero-width characters dropped");
        assert_eq!(sanitize_name("C:\\Users\\x\\y.txt"), "y.txt");
        assert_eq!(sanitize_name("dir/"), "file", "a trailing separator leaves no name");
        assert_eq!(sanitize_name(".gitignore"), ".gitignore", "a leading dot stays");
        assert_eq!(sanitize_name("  .env. "), ".env");
        let long_ext = format!("{}.{}", "n".repeat(200), "e".repeat(40));
        let s = sanitize_name(&long_ext);
        assert!(s.chars().count() <= 120 && !s.ends_with('.') && !s.ends_with(' '));
        let dotty = format!("{}. . .txt", "a".repeat(117));
        let s = sanitize_name(&dotty);
        assert!(s.chars().count() <= 120 && s.ends_with(".txt"));
        let dir = temp_dir("reserve-dot");
        let a = reserve_path(&dir, ".gitignore").unwrap();
        let b = reserve_path(&dir, ".gitignore").unwrap();
        assert_eq!(a.file_name().unwrap(), ".gitignore");
        assert_eq!(b.file_name().unwrap(), ".gitignore (1)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offer_limits_and_unknown_fields() {
        let ok = Plan::new_outbound("a.bin", 64 * CHUNK, "a/b", [1; 32]).unwrap();
        assert_eq!(ok.chunks, MAX_CHUNKS);
        assert!(ok.offer().plan().is_some());
        assert!(Plan::new_outbound("a.bin", 64 * CHUNK + 1, "a/b", [1; 32]).is_none());
        let mut o = ok.offer();
        o.name = String::new();
        assert!(o.plan().is_none(), "empty name");
        o.name = "n".repeat(256);
        assert!(o.plan().is_none(), "name over 255");
        let mut v = serde_json::to_value(ok.offer()).unwrap();
        v["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Offer>(v).is_err(), "unknown fields are refused");
        assert!(within_limit(0, 1) && !within_limit(1, 0));
    }

    #[test]
    fn an_empty_file_arrives_as_an_empty_file() {
        let b = MemBlobs::default();
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let plan = phone_upload(&b, topic, "empty.txt", b"");
        assert_eq!(plan.chunks, 1);
        let dir = temp_dir("recv-empty");
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan: plan.clone(), download_dir: dir.clone(), mailbox: true };
        let path = rt().block_on(receive(&b, &job, |_| {})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        assert!(read_motw(&path).is_some());
        assert!(b.objects.lock().unwrap().is_empty());
        let rel = b.released.lock().unwrap().clone();
        assert!(rel.contains(&format!("p2c/{}/blob", plan.fid_hex())) && rel.contains(&format!("p2c/{}/mailbox", plan.fid_hex())), "mailbox item releases both grants");
        let (size, sha) = hash_file(&path).unwrap();
        assert_eq!((size, sha), (0, Sha256::digest(b"").into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_network_failure_is_transient_and_leaves_no_part_file() {
        let b = MemBlobs::default();
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let plan = phone_upload(&b, topic, "n.bin", b"abc");
        b.fail_gets.lock().unwrap().insert(blob_path(topic, "p2c", &plan.fid_hex(), 0));
        let dir = temp_dir("recv-net");
        let job = ReceiveJob { device_id: "d".into(), topic: topic.into(), plan, download_dir: dir.clone(), mailbox: false };
        let r = rt().block_on(receive(&b, &job, |_| {}));
        assert_eq!(r, Err(XferError::Network));
        assert!(r.unwrap_err().transient());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(b.objects.lock().unwrap().len(), 1, "the object stays for a retry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mailbox_window_edges() {
        let topic = "nl-0123456789abcdef0123456789abcdef";
        let k = [6u8; 32];
        let fid = [3u8; 16];
        let obj = seal_mailbox(&k, 0, topic, "p2c", &fid, 50_000, &serde_json::json!({"t":"text","text":"x"}));
        let key = |_| Some(k);
        assert!(open_mailbox(key, topic, "p2c", &fid, &obj, 50_000 + MAILBOX_MAX_AGE).is_some(), "exactly 24 h is still fine");
        assert!(open_mailbox(key, topic, "p2c", &fid, &obj, 50_000 - 300).is_some(), "300 s ahead is allowed");
        assert!(open_mailbox(key, topic, "p2c", &fid, &vec![b' '; MAILBOX_MAX_BYTES + 1], 50_000).is_none());
        assert!(open_mailbox(key, topic, "p2c", &fid, b"{\"ep\":0,\"created\":-1,\"c\":\"\"}", 50_000).is_none(), "negative created");
        assert!(open_mailbox(key, topic, "p2c", &fid, b"{\"ep\":0,\"created\":50000,\"c\":\"AA\",\"x\":1}", 50_000).is_none(), "unknown field");
    }}
