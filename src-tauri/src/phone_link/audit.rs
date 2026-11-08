// Local audit log `phone-link-audit.jsonl` (design section 12.1): one line per event,
// never message text or clipboard content, only lengths, hosts and file metadata.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "phone-link-audit.jsonl";
const ROTATE_AT: u64 = 5 * 1024 * 1024;
/// Old files kept next to the current one (`.jsonl.1` is the newest).
const KEEP_OLD: u32 = 4;

#[derive(Serialize, Debug, Clone)]
pub struct Entry {
    pub ts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<&'static str>,
    pub result: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

impl Entry {
    pub fn new(ts: i64, op: &str, result: &str) -> Self {
        Entry { ts, device: None, name: None, op: op.to_string(), dir: None, result: result.to_string(), extra: Value::Null }
    }
    pub fn device(mut self, id: &str, name: &str) -> Self {
        self.device = Some(id.to_string());
        self.name = Some(name.to_string());
        self
    }
    pub fn dir(mut self, dir: &'static str) -> Self {
        self.dir = Some(dir);
        self
    }
    pub fn extra(mut self, extra: Value) -> Self {
        self.extra = extra;
        self
    }
}

pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(dir: &Path) -> Self {
        AuditLog { path: dir.join(FILE) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, entry: &Entry) {
        let Ok(mut line) = serde_json::to_string(entry) else { return };
        line.push('\n');
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::metadata(&self.path).map(|m| m.len() + line.len() as u64 > ROTATE_AT).unwrap_or(false) {
            let old = |n: u32| self.path.with_extension(format!("jsonl.{n}"));
            let _ = std::fs::remove_file(old(KEEP_OLD));
            for n in (1..KEEP_OLD).rev() {
                let _ = std::fs::rename(old(n), old(n + 1));
            }
            let _ = std::fs::rename(&self.path, old(1));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_lines_and_rotates() {
        let dir = std::env::temp_dir().join(format!("nectar-audit-{}", crate::phone_link::crypto::hex(&crate::phone_link::crypto::random_bytes::<6>())));
        let log = AuditLog::new(&dir);
        log.write(&Entry::new(1, "text", "ok").device("id", "Phone").dir("p2c").extra(json!({"len": 5})));
        log.write(&Entry::new(2, "pair", "ok"));
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["extra"]["len"], 5);
        assert_eq!(lines[0]["dir"], "p2c");
        assert!(lines[1].get("device").is_none() && lines[1].get("extra").is_none());

        std::fs::write(log.path(), vec![b'x'; ROTATE_AT as usize]).unwrap();
        log.write(&Entry::new(3, "bye", "ok"));
        assert!(dir.join("phone-link-audit.jsonl.1").exists());
        assert_eq!(std::fs::read_to_string(log.path()).unwrap().lines().count(), 1);

        for n in 0..KEEP_OLD + 2 {
            std::fs::write(log.path(), format!("{n}").repeat(ROTATE_AT as usize)).unwrap();
            log.write(&Entry::new(4, "bye", "ok"));
        }
        let newest = std::fs::read(dir.join("phone-link-audit.jsonl.1")).unwrap();
        assert_eq!(newest[0], b'0' + (KEEP_OLD + 1) as u8);
        let oldest = std::fs::read(dir.join(format!("phone-link-audit.jsonl.{KEEP_OLD}"))).unwrap();
        assert_eq!(oldest[0], b'2', "{KEEP_OLD} old files are kept, newest first");
        assert!(!dir.join(format!("phone-link-audit.jsonl.{}", KEEP_OLD + 1)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
