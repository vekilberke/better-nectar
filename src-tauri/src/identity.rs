//! Names that tell this build apart from another Nectar install. The shared build is "Better
//! Nectar"; the owner's own build (`--features phone-link`, `tauri.own.conf.json`) keeps the
//! upstream names so its settings, logon task, credentials and Claude hooks stay where they are.

pub struct Identity {
    /// Tauri `identifier`, also the `%APPDATA%` folder.
    pub identifier: &'static str,
    /// Tauri `productName`: NSIS uses it for the uninstall key, the shortcut and the Run value.
    pub product: &'static str,
    /// Shown in message boxes, the tray and Apps & features.
    pub display: &'static str,
    pub publisher: &'static str,
    /// File name of the main binary (Tauri `mainBinaryName` + `.exe`).
    pub exe: &'static str,
    pub mutex: &'static str,
    pub open_event: &'static str,
    pub task: &'static str,
    pub caldav_target: &'static str,
    pub ics_target: &'static str,
    pub install_key: &'static str,
    pub uninstall_name: &'static str,
    pub shortcut: &'static str,
}

pub const LEGACY: Identity = Identity {
    identifier: "com.kua8.nectar",
    product: "nectar",
    display: "Nectar",
    publisher: "kua8",
    exe: "nectar.exe",
    mutex: "NectarSingleInstance",
    open_event: "NectarOpenSettings",
    task: "Nectar",
    caldav_target: "Nectar CalDAV",
    ics_target: "Nectar Calendar Link",
    install_key: r"Software\kua8\nectar",
    uninstall_name: "Nectar",
    shortcut: "Nectar.lnk",
};

#[cfg_attr(feature = "phone-link", allow(dead_code))]
pub const SHARED: Identity = Identity {
    identifier: "io.github.vekilberke.betternectar",
    product: "Better Nectar",
    display: "Better Nectar",
    publisher: "vekilberke",
    exe: "better-nectar.exe",
    mutex: "BetterNectarSingleInstance",
    open_event: "BetterNectarOpenSettings",
    task: "Better Nectar",
    caldav_target: "Better Nectar CalDAV",
    ics_target: "Better Nectar Calendar Link",
    install_key: r"Software\vekilberke\Better Nectar",
    uninstall_name: "Better Nectar",
    shortcut: "Better Nectar.lnk",
};

#[cfg(feature = "phone-link")]
pub const APP: Identity = LEGACY;
#[cfg(not(feature = "phone-link"))]
pub const APP: Identity = SHARED;

pub fn data_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|dir| std::path::PathBuf::from(dir).join(APP.identifier))
}

#[cfg(not(feature = "phone-link"))]
pub fn legacy_data_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|dir| std::path::PathBuf::from(dir).join(LEGACY.identifier))
}

pub fn taskbar_marker() -> Option<std::path::PathBuf> {
    data_dir().map(|dir| dir.join("taskbar_hidden.flag"))
}

pub fn uninstall_subkey() -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}", APP.uninstall_name)
}

/// One-time read of a credential saved under the upstream name (settings copied from Nectar):
/// stores it under this build's name and leaves the old one for the other install.
pub fn adopt_legacy_secret(current: &keyring::Entry, legacy_service: &str, user: &str) -> Option<String> {
    if APP.identifier == LEGACY.identifier {
        return None;
    }
    let secret = keyring::Entry::new(legacy_service, user).ok()?.get_password().ok()?;
    let _ = current.set_password(&secret);
    Some(secret)
}

/// True when `path` (a command or a path, quoted or not) names exactly this build's exe.
pub fn is_our_exe(path: &str) -> bool {
    let trimmed = path.trim().trim_matches(|c| c == '"' || c == '\'');
    let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    name.eq_ignore_ascii_case(APP.exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn shared_identity_matches_tauri_conf() {
        let c = conf(include_str!("../tauri.conf.json"));
        assert_eq!(c["identifier"], SHARED.identifier);
        assert_eq!(c["productName"], SHARED.product);
        assert_eq!(format!("{}.exe", c["mainBinaryName"].as_str().unwrap()), SHARED.exe);
        assert_eq!(c["bundle"]["publisher"], SHARED.publisher);
        // Tauri NSIS: Uninstall\${PRODUCTNAME} and Software\${MANUFACTURER}\${PRODUCTNAME}.
        assert_eq!(SHARED.uninstall_name, SHARED.product);
        assert_eq!(SHARED.install_key, format!(r"Software\{}\{}", SHARED.publisher, SHARED.product));
        assert_eq!(SHARED.shortcut, format!("{}.lnk", SHARED.product));
    }

    #[test]
    fn own_overlay_keeps_the_legacy_identity() {
        let c = conf(include_str!("../tauri.own.conf.json"));
        assert_eq!(c["identifier"], LEGACY.identifier);
        assert_eq!(c["productName"], LEGACY.product);
        assert_eq!(format!("{}.exe", c["mainBinaryName"].as_str().unwrap()), LEGACY.exe);
    }

    #[test]
    fn legacy_names_are_byte_for_byte_the_old_literals() {
        assert_eq!(LEGACY.mutex, "NectarSingleInstance");
        assert_eq!(LEGACY.open_event, "NectarOpenSettings");
        assert_eq!(LEGACY.task, "Nectar");
        assert_eq!(LEGACY.product, "nectar");
        assert_eq!(LEGACY.caldav_target, "Nectar CalDAV");
        assert_eq!(LEGACY.ics_target, "Nectar Calendar Link");
        assert_eq!(LEGACY.install_key, r"Software\kua8\nectar");
        assert_eq!(LEGACY.uninstall_name, "Nectar");
        assert_eq!(LEGACY.shortcut, "Nectar.lnk");
        assert_eq!(LEGACY.identifier, "com.kua8.nectar");
    }

    #[test]
    fn no_shared_name_collides_with_upstream() {
        let pairs = [
            (SHARED.identifier, LEGACY.identifier),
            (SHARED.exe, LEGACY.exe),
            (SHARED.mutex, LEGACY.mutex),
            (SHARED.open_event, LEGACY.open_event),
            (SHARED.task, LEGACY.task),
            (SHARED.product, LEGACY.product),
            (SHARED.caldav_target, LEGACY.caldav_target),
            (SHARED.ics_target, LEGACY.ics_target),
            (SHARED.install_key, LEGACY.install_key),
            (SHARED.uninstall_name, LEGACY.uninstall_name),
            (SHARED.shortcut, LEGACY.shortcut),
        ];
        for (a, b) in pairs {
            assert!(!a.eq_ignore_ascii_case(b), "{a} collides");
        }
    }

    #[test]
    fn exe_match_is_exact() {
        let other = if APP.exe == LEGACY.exe { SHARED.exe } else { LEGACY.exe };
        assert!(is_our_exe(&format!(r"C:\Program Files\X\{}", APP.exe)));
        assert!(is_our_exe(&format!("\"C:/X/{}\"", APP.exe.to_uppercase())));
        assert!(!is_our_exe(&format!(r"C:\X\{other}")));
        assert!(!is_our_exe("C:/X/nectar.exe.bak"));
    }
}
