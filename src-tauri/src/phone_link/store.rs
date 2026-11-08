// `phone-link.json` in the app config dir (design section 11): non-secret phone link
// config owned by Rust. Not part of settings.json, so export/import never touch it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "phone-link.json";
pub const MAX_IN_MB: u32 = 200;
pub const MAX_OUT_MB: u32 = 100;
const PROCESSED_KEEP_SECS: i64 = 48 * 3600;

fn default_in() -> u32 {
    100
}
fn default_out() -> u32 {
    50
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub download_dir: Option<String>,
    #[serde(default = "default_in")]
    pub max_in_mb: u32,
    #[serde(default = "default_out")]
    pub max_out_mb: u32,
    /// Relay topics whose cleanup (blobs + `link_remove_device`) has not succeeded yet.
    #[serde(default)]
    pub pending_cleanup: Vec<String>,
    /// Mailbox fids already processed, with the time they were seen (kept 48 h).
    #[serde(default)]
    pub processed_fids: BTreeMap<String, i64>,
    /// Per device: clipboard slot fids (T-016) that may still hold a copy on the relay.
    #[serde(default)]
    pub clip_fids: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            download_dir: None,
            max_in_mb: default_in(),
            max_out_mb: default_out(),
            pending_cleanup: Vec::new(),
            processed_fids: BTreeMap::new(),
            clip_fids: BTreeMap::new(),
        }
    }
}

const CLIP_FIDS_KEEP: usize = 9;

impl Config {
    pub fn normalized(mut self) -> Self {
        self.max_in_mb = self.max_in_mb.clamp(1, MAX_IN_MB);
        self.max_out_mb = self.max_out_mb.clamp(1, MAX_OUT_MB);
        self.pending_cleanup.retain(|t| t.len() == 35 && t.starts_with("nl-"));
        self.pending_cleanup.dedup();
        for fids in self.clip_fids.values_mut() {
            fids.retain(|f| super::transfer::is_fid(f));
            let extra = fids.len().saturating_sub(CLIP_FIDS_KEEP);
            fids.drain(..extra);
        }
        self.clip_fids.retain(|_, fids| !fids.is_empty());
        self
    }

    pub fn prune_fids(&mut self, now: i64) {
        self.processed_fids.retain(|_, seen| now - *seen < PROCESSED_KEEP_SECS);
    }

    pub fn download_dir(&self) -> PathBuf {
        self.download_dir.as_ref().map(PathBuf::from).unwrap_or_else(default_download_dir)
    }
}

pub fn default_download_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Downloads").join("Nectar"))
        .unwrap_or_else(|| PathBuf::from("Nectar"))
}

pub fn load(dir: &Path) -> Config {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Config>(&t).ok())
        .unwrap_or_default()
        .normalized()
}

pub fn save(dir: &Path, cfg: &Config) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(FILE)).map_err(|e| e.to_string())
}

/// Download folder rules (design section 11): an existing local directory that is not a
/// drive root, a system folder, a Startup folder or one of Nectar's own folders.
pub fn check_download_dir(path: &Path, own_dirs: &[PathBuf]) -> Result<PathBuf, &'static str> {
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") || text.starts_with("//") {
        return Err("unc");
    }
    if !path.is_absolute() {
        return Err("relative");
    }
    let canon = std::fs::canonicalize(path).map_err(|_| "missing")?;
    if !canon.is_dir() {
        return Err("missing");
    }
    let plain = PathBuf::from(canon.to_string_lossy().trim_start_matches(r"\\?\").to_string());
    if plain.parent().is_none() || plain.components().count() <= 1 {
        return Err("root");
    }
    let lower = plain.to_string_lossy().to_ascii_lowercase();
    if lower.contains(r"\start menu\programs\startup") {
        return Err("system");
    }
    let mut blocked: Vec<PathBuf> = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData", "ProgramW6432"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    blocked.extend(own_dirs.iter().cloned());
    for b in blocked {
        let b = std::fs::canonicalize(&b)
            .map(|c| PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\").to_string()))
            .unwrap_or(b);
        let b = b.to_string_lossy().to_ascii_lowercase();
        let b = b.trim_end_matches('\\');
        if !b.is_empty() && (lower == b || lower.starts_with(&format!("{b}\\"))) {
            return Err("system");
        }
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("nectar-store-{}", crate::phone_link::crypto::hex(&crate::phone_link::crypto::random_bytes::<6>())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_clamping_and_roundtrip() {
        let dir = temp();
        assert_eq!(load(&dir), Config::default());
        std::fs::write(dir.join(FILE), r#"{"maxInMb": 999, "maxOutMb": 0, "pendingCleanup": ["nl-x", "nl-0123456789abcdef0123456789abcdef"]}"#).unwrap();
        let cfg = load(&dir);
        assert_eq!((cfg.max_in_mb, cfg.max_out_mb), (MAX_IN_MB, 1));
        assert_eq!(cfg.pending_cleanup, vec!["nl-0123456789abcdef0123456789abcdef".to_string()]);
        let mut cfg = Config::default();
        cfg.processed_fids.insert("a".into(), 0);
        cfg.processed_fids.insert("b".into(), PROCESSED_KEEP_SECS);
        cfg.prune_fids(PROCESSED_KEEP_SECS + 1);
        assert_eq!(cfg.processed_fids.keys().collect::<Vec<_>>(), vec!["b"]);
        save(&dir, &cfg).unwrap();
        assert_eq!(load(&dir), cfg);
        std::fs::write(dir.join(FILE), "garbage").unwrap();
        assert_eq!(load(&dir), Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clip_fids_keep_valid_fids_and_the_newest_few() {
        let fid = |i: usize| format!("{i:032x}");
        let mut cfg = Config::default();
        cfg.clip_fids.insert("dev".into(), (0..12).map(fid).chain(["not a fid".to_string()]).collect());
        cfg.clip_fids.insert("empty".into(), vec!["x".into()]);
        let cfg = cfg.normalized();
        assert_eq!(cfg.clip_fids.get("dev"), Some(&(3..12).map(fid).collect::<Vec<_>>()));
        assert!(!cfg.clip_fids.contains_key("empty"));
    }

    #[test]
    fn download_dir_rules() {
        let dir = temp();
        assert!(check_download_dir(&dir, &[]).is_ok());
        assert_eq!(check_download_dir(Path::new(r"C:\"), &[]), Err("root"));
        assert_eq!(check_download_dir(Path::new(r"\\server\share"), &[]), Err("unc"));
        assert_eq!(check_download_dir(Path::new("relative"), &[]), Err("relative"));
        assert_eq!(check_download_dir(&dir.join("missing"), &[]), Err("missing"));
        if let Some(win) = std::env::var_os("SystemRoot") {
            assert_eq!(check_download_dir(&PathBuf::from(win).join("System32"), &[]), Err("system"));
        }
        assert_eq!(check_download_dir(&dir, &[dir.clone()]), Err("system"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
