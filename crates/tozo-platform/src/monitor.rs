use std::mem::size_of;

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, HDC, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub handle: isize,
    pub rect: RECT,
    pub work_area: RECT,
    pub primary: bool,
    pub scale: f64,
}

struct EnumState {
    monitors: Vec<Monitor>,
}

/// 枚举全部显示器（含矩形、工作区、缩放）
pub fn monitors() -> Vec<Monitor> {
    let mut state = Box::new(EnumState {
        monitors: Vec::new(),
    });
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_enum_proc),
            LPARAM(&mut *state as *mut EnumState as isize),
        );
    }
    state.monitors
}

/// 取主显示器，无标记时退回第一个
pub fn primary_monitor() -> Option<Monitor> {
    let all = monitors();
    all.iter()
        .find(|m| m.primary)
        .copied()
        .or_else(|| all.first().copied())
}

/// EnumDisplayMonitors 回调：收集矩形与有效 DPI 缩放
unsafe extern "system" fn monitor_enum_proc(
    handle: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let state = &mut *(lparam.0 as *mut EnumState);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    if GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
        state.monitors.push(Monitor {
            handle: handle.0 as isize,
            rect: info.monitorInfo.rcMonitor,
            work_area: info.monitorInfo.rcWork,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            scale: dpi_x as f64 / 96.0,
        });
    }
    BOOL(1)
}
