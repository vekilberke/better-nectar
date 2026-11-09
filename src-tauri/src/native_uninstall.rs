
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegDeleteTreeW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_TOPMOST, MB_YESNO, SW_SHOWNORMAL,
};
use windows::core::PCWSTR;

use crate::identity::APP;
use crate::uninstall_registry::{desktop_dir, legacy_start_menu_dir, start_menu_programs_dir, wide};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TITLE: &str = APP.display;

pub fn requested() -> bool {
    let by_arg = std::env::args().skip(1).any(|a| a == "--uninstall");
    let by_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .is_some_and(|n| n == "uninstall.exe");
    by_arg || by_name
}

fn message(text: &str, flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let text_w = wide(text);
    let title_w = wide(TITLE);
    unsafe { MessageBoxW(None, PCWSTR(text_w.as_ptr()), PCWSTR(title_w.as_ptr()), flags | MB_TOPMOST) }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

fn relaunch_elevated(exe: &Path) -> bool {
    let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let verb_w = wide("runas");
    let args_w = wide("--uninstall");
    let result = unsafe {
        ShellExecuteW(None, PCWSTR(verb_w.as_ptr()), PCWSTR(exe_w.as_ptr()), PCWSTR(args_w.as_ptr()), None, SW_SHOWNORMAL)
    };
    (result.0 as isize) > 32
}

fn run_hidden(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

fn stop_running_nectar() {
    let not_self = format!("PID ne {}", std::process::id());
    let image = format!("IMAGENAME eq {}", APP.exe);
    let filters = ["/FI", image.as_str(), "/FI", not_self.as_str()];

    let still_running = || {
        run_hidden("tasklist", &filters).is_some_and(|out| out.to_lowercase().contains(&APP.exe.to_lowercase()))
    };

    let _ = run_hidden("taskkill", &filters);
    for _ in 0..16 {
        if !still_running() { return; }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let mut forced = vec!["/F"];
    forced.extend_from_slice(&filters);
    let _ = run_hidden("taskkill", &forced);
    for _ in 0..8 {
        if !still_running() { return; }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// The app was force-killed: bring the taskbar back with the user's own auto-hide setting.
fn restore_taskbar_if_left_hidden() {
    if let Some(flag) = crate::identity::taskbar_marker() {
        crate::utils::restore_from_marker(&flag);
    }
}

fn remove_shortcuts(all_users: bool) {
    let mut lnks = Vec::new();
    let restore = format!("Restore Windows taskbar ({}).lnk", APP.display);
    if let Some(d) = start_menu_programs_dir(all_users) {
        lnks.push(d.join(APP.shortcut));
        lnks.push(d.join(&restore));
    }
    if let Some(d) = legacy_start_menu_dir(all_users) { lnks.push(d.join(APP.shortcut)); }
    if let Some(d) = desktop_dir(all_users) { lnks.push(d.join(APP.shortcut)); }
    for lnk in lnks {
        let _ = std::fs::remove_file(lnk);
    }
}

fn remove_registry(all_users: bool) {
    unsafe {
        let root = if all_users { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
        let subkey_w = wide(&crate::identity::uninstall_subkey());
        let _ = RegDeleteTreeW(root, PCWSTR(subkey_w.as_ptr()));
        let install_key = wide(APP.install_key);
        let _ = RegDeleteTreeW(root, PCWSTR(install_key.as_ptr()));

        let run_key = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let approved_key = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
        // tauri-plugin-autostart names the Run value after the product.
        let value = wide(APP.product);
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(run_key.as_ptr()), PCWSTR(value.as_ptr()));
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(approved_key.as_ptr()), PCWSTR(value.as_ptr()));
    }
}

// Files the app itself puts in the install folder. libunwind.dll only exists in older
// hand-copied installs; the release exe links it statically.
fn app_files() -> Vec<String> {
    vec![
        APP.exe.to_string(),
        format!("{}.old", APP.exe),
        "uninstall.exe".into(),
        ".install-scope".into(),
        "WebView2Loader.dll".into(),
        "libunwind.dll".into(),
    ]
}

// True when the folder belongs to Nectar alone and can be removed as a whole
fn is_dedicated_install_dir(dir: &Path) -> bool {
    let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return false;
    };

    // Folder name must identify a Nectar install
    if !name.contains("nectar") {
        return false;
    }

    // Never a drive root
    if dir.parent().is_none() {
        return false;
    }

    // Never the user profile, Desktop or Downloads
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let user_path = Path::new(&userprofile);
        if dir == user_path || dir == user_path.join("Desktop") || dir == user_path.join("Downloads") {
            return false;
        }
    }

    true
}

fn remove_dir_command(install_dir: &Path, run_once: &str) -> String {
    let dir = install_dir.to_string_lossy().replace('\'', "''");
    format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         for ($i = 0; $i -lt 40 -and (Test-Path -LiteralPath '{dir}'); $i++) {{ \
           Remove-Item -LiteralPath '{dir}' -Recurse -Force -ErrorAction SilentlyContinue -ErrorVariable failed; \
           if (Test-Path -LiteralPath '{dir}') {{ Start-Sleep -Milliseconds 500 }} \
         }}; \
         if (Test-Path -LiteralPath '{dir}') {{ \
           Add-Content (Join-Path $env:TEMP 'nectar-uninstall.log') (\"Could not remove {dir}: \" + ($failed | Out-String)); \
           New-ItemProperty -Path '{run_once}' -Name 'NectarCleanup' -Value 'cmd /c rd /s /q \"{dir}\"' -Force -ErrorAction SilentlyContinue | Out-Null \
         }}",
        pid = std::process::id(),
    )
}

fn remove_app_files_command(install_dir: &Path, run_once: &str) -> String {
    let dir = install_dir.to_string_lossy().replace('\'', "''");
    let paths: Vec<String> = app_files()
        .iter()
        .map(|f| install_dir.join(f).to_string_lossy().replace('\'', "''"))
        .collect();
    let files = paths.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(", ");
    let del_args = paths.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(" ");
    format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         $files = @({files}); \
         $left = {{ @($files | Where-Object {{ Test-Path -LiteralPath $_ }}) }}; \
         for ($i = 0; $i -lt 40 -and (& $left).Count; $i++) {{ \
           & $left | ForEach-Object {{ Remove-Item -LiteralPath $_ -Force -ErrorAction SilentlyContinue -ErrorVariable +failed }}; \
           if ((& $left).Count) {{ Start-Sleep -Milliseconds 500 }} \
         }}; \
         if ((& $left).Count) {{ \
           Add-Content (Join-Path $env:TEMP 'nectar-uninstall.log') (\"Could not remove Nectar files from {dir}: \" + ($failed | Out-String)); \
           New-ItemProperty -Path '{run_once}' -Name 'NectarCleanup' -Value 'cmd /c del /f /q {del_args}' -Force -ErrorAction SilentlyContinue | Out-Null \
         }}",
        pid = std::process::id(),
    )
}

/// The NSIS setup runs the uninstaller with `_?=<dir>` before a reinstall and then waits for the
/// main exe to be gone.
fn from_setup() -> bool {
    std::env::args().skip(1).any(|a| a.starts_with("_?="))
}

/// Reinstall over this install: stop the app and give the taskbar back, then move the main exe
/// aside so setup can write the new one. Settings, hooks, task and registry stay; setup rewrites
/// the registry and the next start deletes the `.old` file.
fn run_for_setup(install_dir: &Path) {
    stop_running_nectar();
    restore_taskbar_if_left_hidden();
    let exe = install_dir.join(APP.exe);
    let old = install_dir.join(format!("{}.old", APP.exe));
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(&exe, &old);
}

pub fn run() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(install_dir) = exe.parent().map(Path::to_path_buf) else { return };

    if from_setup() {
        run_for_setup(&install_dir);
        return;
    }

    let all_users = crate::uninstall_registry::install_scope(&install_dir).unwrap_or(false);

    if all_users && !is_elevated() {
        if !relaunch_elevated(&exe) {
            message(&format!("{} was installed for all users, so uninstalling it needs administrator permission.", APP.display), MB_OK | MB_ICONINFORMATION);
        }
        return;
    }

    if message(
        &format!("Uninstall {}?\n\nThis removes the app and its shortcuts. Your settings are kept in case you reinstall.", APP.display),
        MB_YESNO | MB_ICONQUESTION,
    ) != IDYES
    {
        return;
    }

    stop_running_nectar();
    restore_taskbar_if_left_hidden();
    if let Some(dir) = crate::identity::data_dir() {
        crate::notifications::restore_banners(&dir);
    }
    // Hooks and the status line point at this exe; a chained user status line comes back.
    crate::agent_api::set_hooks(false);
    crate::startup::delete_task();
    remove_shortcuts(all_users);
    remove_registry(all_users);
    // Phone link keys are secrets bound to this install; they are not kept like settings.
    #[cfg(feature = "phone-link")]
    crate::phone_link::delete_all_secrets();

    let dedicated = is_dedicated_install_dir(&install_dir);
    let run_once = if all_users {
        r"HKLM:\Software\Microsoft\Windows\CurrentVersion\RunOnce"
    } else {
        r"HKCU:\Software\Microsoft\Windows\CurrentVersion\RunOnce"
    };
    let ps_command = if dedicated {
        remove_dir_command(&install_dir, run_once)
    } else {
        // Shared folder (e.g. D:\Apps): remove only Nectar's own files, never the folder
        remove_app_files_command(&install_dir, run_once)
    };
    let spawn = |flags: u32| {
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps_command])
            .current_dir(std::env::temp_dir())
            .creation_flags(flags)
            .spawn()
    };
    let spawned = spawn(CREATE_NO_WINDOW | 0x0100_0000).or_else(|_| spawn(CREATE_NO_WINDOW));

    if spawned.is_err() {
        let text = if dedicated {
            format!("{} was unregistered, but its folder couldn't be removed automatically. You can delete it yourself:\n\n{}", APP.display, install_dir.display())
        } else {
            format!("{} was unregistered, but its files couldn't be removed automatically. You can delete {} yourself from:\n\n{}", APP.display, app_files().join(", "), install_dir.display())
        };
        message(&text, MB_OK | MB_ICONINFORMATION);
        return;
    }
    message(&format!("{} has been uninstalled.", APP.display), MB_OK | MB_ICONINFORMATION);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_dir_needs_nectar_in_its_name() {
        assert!(is_dedicated_install_dir(Path::new(r"C:\Users\u\AppData\Local\Programs\Nectar")));
        assert!(is_dedicated_install_dir(Path::new(r"D:\Apps\nectar-beta")));
        assert!(!is_dedicated_install_dir(Path::new(r"D:\Apps")));
    }

    #[test]
    fn drive_root_is_never_dedicated() {
        assert!(!is_dedicated_install_dir(Path::new(r"D:\")));
    }

    #[test]
    fn shared_folder_cleanup_only_targets_app_files() {
        let cmd = remove_app_files_command(Path::new(r"D:\Apps"), r"HKCU:\RunOnce");
        assert!(cmd.contains(&format!(r"'D:\Apps\{}'", APP.exe)));
        assert!(cmd.contains(r"'D:\Apps\uninstall.exe'"));
        assert!(cmd.contains(r"'D:\Apps\WebView2Loader.dll'"));
        assert!(cmd.contains(r"'D:\Apps\libunwind.dll'"));
        assert!(!cmd.contains("-Recurse"));
        assert!(!cmd.contains("rd /s"));
    }
}
