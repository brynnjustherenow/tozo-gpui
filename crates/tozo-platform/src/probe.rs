use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WINDOWPOS;

const PROBE_ID: usize = 0x7A09;

/// 带毫秒时间戳写调试日志
fn log(msg: String) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    tracing::debug!("[{ms}] {msg}");
}

/// 安装窗口子类探测过程
pub fn install(hwnd: HWND) {
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(probe_proc), PROBE_ID, 0);
    }
    log("probe installed".into());
}

/// 移除窗口子类探测过程
pub fn uninstall(hwnd: HWND) {
    unsafe {
        let _ = RemoveWindowSubclass(hwnd, Some(probe_proc), PROBE_ID);
    }
}

/// 子类过程：记录关键窗口消息，appbar 位移时重断言位置
unsafe extern "system" fn probe_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        0x001A => log(format!("WM_SETTINGCHANGE wparam={wparam:?}")),
        0x0046 => {
            let wp = &*(lparam.0 as *const WINDOWPOS);
            log(format!(
                "WM_WINDOWPOSCHANGING -> ({}, {}) {}x{} flags={:#x}",
                wp.x, wp.y, wp.cx, wp.cy, wp.flags.0
            ));
        }
        0x0047 => {
            let wp = &*(lparam.0 as *const WINDOWPOS);
            log(format!(
                "WM_WINDOWPOSCHANGED -> ({}, {}) {}x{} flags={:#x}",
                wp.x, wp.y, wp.cx, wp.cy, wp.flags.0
            ));
        }
        0x8000 => {
            log(format!("WM_APP (ABN) wparam={wparam:?}"));
            if wparam.0 as usize == crate::appbar::ABN_POSCHANGED {
                let approved = crate::appbar::assert_position();
                log(format!("ABN_POSCHANGED handled, re-asserted to {approved:?}"));
            }
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}
