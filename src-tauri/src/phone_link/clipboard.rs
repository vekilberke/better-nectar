// Clipboard writes for `clipboard.set` (design 6.5). One thread owns a message-only window
// and pumps its messages, so the clipboard always has a real owner (OpenClipboard(NULL)
// plus EmptyClipboard leaves the owner NULL and SetClipboardData can then fail). It runs
// only while the phone link is on.

use super::remote::Deny;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WNDCLASSW,
};

const CF_UNICODETEXT: u32 = 13;
static OWNER: AtomicIsize = AtomicIsize::new(0);
static START: Mutex<()> = Mutex::new(());
// Sequence number of our last write, so clipboard sync does not send phone text back (T-016).
static LAST_SEQ: AtomicU32 = AtomicU32::new(0);

pub fn last_write_seq() -> u32 {
    LAST_SEQ.load(Ordering::SeqCst)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> windows::Win32::Foundation::LRESULT {
    match msg {
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            windows::Win32::Foundation::LRESULT(0)
        }
        WM_DESTROY => {
            OWNER.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            windows::Win32::Foundation::LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn owner() -> Option<HWND> {
    let _guard = START.lock().ok()?;
    let raw = OWNER.load(Ordering::SeqCst);
    if raw != 0 {
        return Some(HWND(raw as *mut _));
    }
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    std::thread::spawn(move || unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default().into();
        let class = w!("NectarPhoneLinkClipboard");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        );
        let Ok(hwnd) = hwnd else {
            let _ = tx.send(0);
            return;
        };
        OWNER.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = tx.send(hwnd.0 as isize);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    let raw = rx.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
    (raw != 0).then(|| HWND(raw as *mut _))
}

/// Ends the owner thread (phone link turned off).
pub fn stop() {
    let raw = OWNER.load(Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn set_text(text: &str) -> Result<(), Deny> {
    if text.contains('\0') {
        return Err(Deny::Invalid);
    }
    super::clip_sync::note_from_phone(text);
    let hwnd = owner().ok_or(Deny::Failed)?;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(Some(hwnd)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        if !opened {
            return Err(Deny::Busy);
        }
        let result = (|| {
            EmptyClipboard().map_err(|_| Deny::Failed)?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|_| Deny::Failed)?;
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(Deny::Failed);
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_err() {
                let _ = GlobalFree(Some(mem));
                return Err(Deny::Failed);
            }
            LAST_SEQ.store(GetClipboardSequenceNumber(), Ordering::SeqCst);
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
