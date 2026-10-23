use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, MSLLHOOKSTRUCT, WH_MOUSE_LL,
};

type ClickAwayCallback = Arc<dyn Fn() + Send + Sync + 'static>;

struct ClickAwayRegistration {
    protected_hwnd: isize,
    callback: ClickAwayCallback,
}

static REGISTRATIONS: Mutex<Vec<ClickAwayRegistration>> = Mutex::new(Vec::new());
static HOOK: AtomicIsize = AtomicIsize::new(0);

const WM_LBUTTONDOWN: usize = 0x0201;
const WM_RBUTTONDOWN: usize = 0x0204;
const WM_NCLBUTTONDOWN: usize = 0x00A1;
const WM_NCRBUTTONDOWN: usize = 0x00A4;

/// 判断点击是否落在受保护窗口矩形内
fn click_inside(info: &MSLLHOOKSTRUCT, protected_hwnd: isize) -> bool {
    protected_hwnd != 0
        && crate::window::window_rect(crate::window::hwnd(protected_hwnd)).is_some_and(
            |(l, t, r, b)| info.pt.x >= l && info.pt.x < r && info.pt.y >= t && info.pt.y < b,
        )
}

/// LL 鼠标钩子回调：触发点击离开并按需卸载钩子
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let message = wparam.0 as usize;
        if matches!(
            message,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN
        ) {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let mut fired: Vec<ClickAwayCallback> = Vec::new();
            {
                let mut registrations = REGISTRATIONS.lock().unwrap();
                registrations.retain(|r| {
                    if click_inside(info, r.protected_hwnd) {
                        true
                    } else {
                        fired.push(r.callback.clone());
                        false
                    }
                });
                if registrations.is_empty() {
                    uninstall_system_hook();
                }
            }
            for callback in fired {
                callback();
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 注册全局低级鼠标钩子（WH_MOUSE_LL）
fn install_system_hook() {
    if HOOK.load(Ordering::SeqCst) == 0 {
        unsafe {
            if let Ok(hook) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
                HOOK.store(hook.0 as isize, Ordering::SeqCst);
            }
        }
    }
}

/// 卸载全局鼠标钩子
fn uninstall_system_hook() {
    let handle = HOOK.swap(0, Ordering::SeqCst);
    if handle != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(handle as *mut c_void));
        }
    }
}

/// 注册点击离开监听，多窗口可并存，须在主线程调用
/// Install a global low-level mouse hook that runs `on_click_away` when a
/// click happens outside `protected_hwnd`. Multiple windows can register at
/// the same time. Must be called from a thread with a message loop (the main
/// thread).
pub fn install_click_away(protected_hwnd: isize, on_click_away: impl Fn() + Send + Sync + 'static) {
    let mut registrations = REGISTRATIONS.lock().unwrap();
    registrations.retain(|r| r.protected_hwnd != protected_hwnd);
    registrations.push(ClickAwayRegistration {
        protected_hwnd,
        callback: Arc::new(on_click_away),
    });
    install_system_hook();
}

/// 移除指定窗口的点击离开监听
/// Remove the click-away registration protecting `protected_hwnd`. The system
/// hook is removed once no registrations remain.
pub fn uninstall_click_away_for(protected_hwnd: isize) {
    let mut registrations = REGISTRATIONS.lock().unwrap();
    registrations.retain(|r| r.protected_hwnd != protected_hwnd);
    if registrations.is_empty() {
        uninstall_system_hook();
    }
}
