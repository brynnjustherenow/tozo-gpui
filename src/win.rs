use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    GWL_EXSTYLE,
};

const WS_EX_TOPMOST: i32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: i32 = 0x0000_0080;

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    color: u32,
    animation: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attribute: u32,
    data: *mut core::ffi::c_void,
    size: u32,
}

#[link(name = "user32", kind = "raw-dylib")]
extern "system" {
    fn SetWindowCompositionAttribute(hwnd: HWND, data: *mut WindowCompositionAttribData) -> BOOL;
}

/// GUI 子系统下尝试附加父进程控制台并把 std 句柄指到 CONOUT$，返回是否成功
#[cfg(feature = "gui")]
pub fn attach_parent_console() -> bool {
    use std::os::windows::io::AsRawHandle;

    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF6;

    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        fn AttachConsole(dwprocessid: u32) -> i32;
        fn SetStdHandle(nstdhandle: u32, hhandle: *mut core::ffi::c_void) -> i32;
    }

    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return false;
    }
    let Ok(con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") else {
        return false;
    };
    let handle = con.as_raw_handle();
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    std::mem::forget(con);
    true
}

/// 用 SetWindowCompositionAttribute 关闭窗口的亚克力等合成效果，返回是否成功
pub fn accent_disable(hwnd: HWND) -> bool {
    let mut policy = AccentPolicy {
        state: 0,
        flags: 0,
        color: 0,
        animation: 0,
    };
    let mut data = WindowCompositionAttribData {
        attribute: 19,
        data: &mut policy as *mut AccentPolicy as *mut core::ffi::c_void,
        size: std::mem::size_of::<AccentPolicy>() as u32,
    };
    unsafe { SetWindowCompositionAttribute(hwnd, &mut data) }.as_bool()
}

struct FindCtx {
    pid: u32,
    title: Option<String>,
    require_popup_style: bool,
    found: AtomicIsize,
}

/// EnumWindows 回调：按 pid/标题/弹窗扩展样式匹配，命中即停止枚举
unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &*(lparam.0 as *const FindCtx) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != ctx.pid {
        return BOOL(1);
    }
    if let Some(title) = &ctx.title {
        let mut buf = [0u16; 128];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        let actual = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if &actual != title {
            return BOOL(1);
        }
    }
    if ctx.require_popup_style {
        let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) };
        if ex & (WS_EX_TOOLWINDOW | WS_EX_TOPMOST) != (WS_EX_TOOLWINDOW | WS_EX_TOPMOST) {
            return BOOL(1);
        }
    }
    ctx.found.store(hwnd.0 as isize, Ordering::SeqCst);
    BOOL(0)
}

/// 枚举顶层窗口，找到本进程内符合条件的 HWND
pub fn find_hwnd(pid: u32, title: Option<&str>, require_popup_style: bool) -> Option<isize> {
    let ctx = FindCtx {
        pid,
        title: title.map(str::to_string),
        require_popup_style,
        found: AtomicIsize::new(0),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&ctx as *const FindCtx as isize));
    }
    let value = ctx.found.load(Ordering::SeqCst);
    (value != 0).then_some(value)
}
