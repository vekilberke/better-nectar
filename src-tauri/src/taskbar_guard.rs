//! A small second process that brings the Windows taskbar back when the app dies without cleaning
//! up (force kill, panic abort): `<exe> --taskbar-guard <pid>`. `<exe> restore-taskbar` does the
//! same by hand.

use std::os::windows::process::CommandExt;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// Long enough for a tray Restart to create the single-instance mutex again.
const RESTART_GRACE: Duration = Duration::from_secs(3);

/// Restore only when the app left its marker and no new instance took over.
fn should_restore(marker_exists: bool, instance_running: bool) -> bool {
    marker_exists && !instance_running
}

fn instance_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name = crate::uninstall_registry::wide(crate::identity::APP.mutex);
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, windows::core::PCWSTR(name.as_ptr())) } {
        Ok(handle) => {
            let _ = unsafe { CloseHandle(handle) };
            true
        }
        Err(_) => false,
    }
}

fn wait_for_exit(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return false };
    unsafe {
        WaitForSingleObject(process, INFINITE);
        let _ = CloseHandle(process);
    }
    true
}

fn restore() -> bool {
    crate::identity::taskbar_marker().is_some_and(|path| crate::utils::restore_from_marker(&path))
}

/// Starts the guard for this process. Its wait costs no CPU while the app runs.
pub fn spawn() {
    if cfg!(debug_assertions) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe)
        .args(["--taskbar-guard", &std::process::id().to_string()])
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
}

pub fn run_cli(args: &[String]) -> Option<i32> {
    match args.get(1).map(String::as_str) {
        Some("--taskbar-guard") => {
            let pid = args.get(2).and_then(|p| p.parse::<u32>().ok())?;
            if !wait_for_exit(pid) {
                return Some(1);
            }
            std::thread::sleep(RESTART_GRACE);
            let marker = crate::identity::taskbar_marker().is_some_and(|p| p.exists());
            if should_restore(marker, instance_running()) {
                restore();
            }
            Some(0)
        }
        Some("restore-taskbar") => {
            if !restore() {
                crate::utils::show_taskbar_default();
            }
            Some(0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_restores_only_an_abandoned_taskbar() {
        assert!(should_restore(true, false));
        assert!(!should_restore(true, true), "tray Restart: the new instance owns the taskbar");
        assert!(!should_restore(false, false), "clean quit removed the marker");
        assert!(!should_restore(false, true));
    }

    #[test]
    fn cli_ignores_other_commands() {
        assert_eq!(run_cli(&["x".into(), "agent".into()]), None);
        assert_eq!(run_cli(&["x".into(), "--taskbar-guard".into(), "abc".into()]), None);
    }
}
