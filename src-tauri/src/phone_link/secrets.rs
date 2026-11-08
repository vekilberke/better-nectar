// Phone link secrets in Windows Credential Manager (design section 11). Written with
// CredWriteW and CRED_PERSIST_LOCAL_MACHINE, so they never roam with a domain profile
// (the keyring crate writes CRED_PERSIST_ENTERPRISE).

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use zeroize::Zeroizing;

pub const PREFIX: &str = "Nectar Phone Link/";
/// CRED_MAX_CREDENTIAL_BLOB_SIZE.
pub const MAX_BLOB: usize = 5 * 512;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn write(target: &str, blob: &[u8]) -> Result<(), String> {
    if blob.len() > MAX_BLOB {
        return Err(format!("credential too large ({} bytes)", blob.len()));
    }
    let mut name = wide(target);
    let mut user = wide("Nectar");
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&cred, 0) }.map_err(|e| e.to_string())
}

pub fn read(target: &str) -> Option<Zeroizing<Vec<u8>>> {
    let name = wide(target);
    let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) }.ok()?;
    if ptr.is_null() {
        return None;
    }
    let blob = unsafe {
        let c = &*ptr;
        let out = if c.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        CredFree(ptr as *const _);
        out
    };
    Some(Zeroizing::new(blob))
}

pub fn delete(target: &str) -> bool {
    let name = wide(target);
    unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) }.is_ok()
}

/// Target names that start with `prefix` (no wildcard in `prefix`).
pub fn list(prefix: &str) -> Vec<String> {
    let filter = wide(&format!("{prefix}*"));
    let mut count = 0u32;
    let mut items: *mut *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredEnumerateW(PCWSTR(filter.as_ptr()), None, &mut count, &mut items) }.is_err() || items.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    unsafe {
        for i in 0..count as usize {
            let c = *items.add(i);
            if !c.is_null() {
                if let Ok(name) = (*c).TargetName.to_string() {
                    if name.starts_with(prefix) {
                        out.push(name);
                    }
                }
            }
        }
        CredFree(items as *const _);
    }
    out
}

/// Removes every phone link credential (uninstall, design section 9).
pub fn delete_all() -> usize {
    list(PREFIX).iter().filter(|t| delete(t)).count()
}

pub fn device_target(id: &str) -> String {
    format!("{PREFIX}device/{id}")
}
pub const OWNER_TARGET: &str = "Nectar Phone Link/owner";
pub const SESSION_TARGET: &str = "Nectar Phone Link/session";

pub fn write_json<T: serde::Serialize>(target: &str, value: &T) -> Result<(), String> {
    let text = Zeroizing::new(serde_json::to_vec(value).map_err(|e| e.to_string())?);
    write(target, &text)
}

pub fn read_json<T: serde::de::DeserializeOwned>(target: &str) -> Option<T> {
    serde_json::from_slice(&read(target)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_list_delete_roundtrip() {
        let prefix = format!("Nectar Phone Link Test {}/", crate::phone_link::crypto::hex(&crate::phone_link::crypto::random_bytes::<6>()));
        let a = format!("{prefix}device/a");
        let b = format!("{prefix}device/b");
        write(&a, b"alpha").unwrap();
        write(&b, &[7u8; 300]).unwrap();
        assert_eq!(read(&a).unwrap().as_slice(), b"alpha");
        write(&a, b"beta").unwrap();
        assert_eq!(read(&a).unwrap().as_slice(), b"beta");
        let mut names = list(&prefix);
        names.sort();
        assert_eq!(names, vec![a.clone(), b.clone()]);
        assert!(delete(&a) && delete(&b));
        assert!(read(&a).is_none());
        assert!(!delete(&a));
        assert!(list(&prefix).is_empty());
        assert!(write(&a, &vec![0u8; MAX_BLOB + 1]).is_err());
    }

    #[test]
    fn credentials_are_local_machine() {
        let target = format!("Nectar Phone Link Test {}/persist", crate::phone_link::crypto::hex(&crate::phone_link::crypto::random_bytes::<6>()));
        write(&target, b"x").unwrap();
        let name = wide(&target);
        let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) }.unwrap();
        let persist = unsafe { (*ptr).Persist };
        unsafe { CredFree(ptr as *const _) };
        delete(&target);
        assert_eq!(persist, CRED_PERSIST_LOCAL_MACHINE);
    }
}
