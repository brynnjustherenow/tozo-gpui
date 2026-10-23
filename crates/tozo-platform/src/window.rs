use std::ffi::c_void;
use std::mem::size_of;
use std::sync::OnceLock;

use windows::core::{Error, BOOL};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, GetWindowRect, PostMessageW, SetWindowLongPtrW, SetWindowPos,
    GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_CLOSE,
    WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

static MAIN_HWND: OnceLock<isize> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    None,
    Mica,
    Acrylic,
}

/// 记录主窗口句柄
pub fn set_main_hwnd(hwnd: isize) {
    let _ = MAIN_HWND.set(hwnd);
}

/// isize 转为 HWND
pub fn hwnd(value: isize) -> HWND {
    HWND(value as *mut c_void)
}

/// 取主窗口句柄
pub fn main_hwnd() -> Option<HWND> {
    MAIN_HWND.get().map(|v| HWND(*v as *mut c_void))
}

/// 用 SetWindowPos 设置或取消窗口置顶
pub fn set_topmost(hwnd: HWND, enable: bool) {
    let insert_after = if enable { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// 设置点击穿透（WS_EX_TRANSPARENT 与 WS_EX_LAYERED）
pub fn set_click_through(hwnd: HWND, enable: bool) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bits = (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
        let new_style = if enable { style | bits } else { style & !bits };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
    }
}

/// 应用 DWM 系统背景材质，失败回退 Win10 亚克力
pub fn apply_backdrop(hwnd: HWND, backdrop: Backdrop) -> bool {
    let value: u32 = match backdrop {
        Backdrop::None => 1,
        Backdrop::Mica => 2,
        Backdrop::Acrylic => 3,
    };
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &value as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        )
    };
    if result.is_ok() {
        return true;
    }
    apply_win10_acrylic(hwnd)
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 3;

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
    data: *mut c_void,
    size: u32,
}

#[link(name = "user32", kind = "raw-dylib")]
extern "system" {
    /// user32 未导出接口：设置窗口组合属性
    fn SetWindowCompositionAttribute(hwnd: HWND, data: *mut WindowCompositionAttribData) -> BOOL;
}

/// Win10 回退路径：经 AccentPolicy 启用亚克力模糊
fn apply_win10_acrylic(hwnd: HWND) -> bool {
    let mut policy = AccentPolicy {
        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        flags: 0,
        color: 0x99000000,
        animation: 0,
    };
    let mut data = WindowCompositionAttribData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as *mut c_void,
        size: size_of::<AccentPolicy>() as u32,
    };
    unsafe { SetWindowCompositionAttribute(hwnd, &mut data) }.as_bool()
}

/// 取窗口屏幕坐标矩形（左、上、右、下）
pub fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let mut rect = RECT::default();
    let ok = unsafe { GetWindowRect(hwnd, &mut rect) };
    if ok.is_ok() {
        Some((rect.left, rect.top, rect.right, rect.bottom))
    } else {
        None
    }
}

/// 取鼠标光标屏幕坐标
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    Some((pt.x, pt.y))
}

/// 投递 WM_CLOSE 异步关闭窗口
pub fn post_close(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
    }
}

/// 取当前线程最近一次 Win32 错误
pub(crate) fn last_error() -> Error {
    Error::from_thread()
}
