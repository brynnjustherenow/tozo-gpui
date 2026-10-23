//! Appbar registration with a systray spy in the same process.
//! 同进程中存在 systray 伪装窗口时的 appbar 注册。
//!
//! WHAT HAPPENED :
//! systray-util's TraySpy creates a fake top-level window with class
//! "Shell_TrayWnd" inside our own process and re-asserts HWND_TOPMOST every
//! 100ms, so it outranks the real explorer taskbar in every FindWindow race.
//! SHAppBarMessage resolves the shell's AppBarService via
//! FindWindow("Shell_TrayWnd"), so both paths below ended up at the spy,
//! whose forwarding breaks the protocol:
//! 1. Same-process callers (this app): shell32 delivers the registered
//!    appbar message with a raw APPBARDATA pointer (valid same-process);
//!    the spy forwards messages > WM_USER via PostMessageW (asynchronous)
//!    and returns 0, so ABM_NEW/ABM_SETPOS never take effect and rc is
//!    never filled. Windows stopped reserving taskbar space for our bar.
//! 2. Other processes: shell32 sends WM_COPYDATA (dwData == 0) wrapping
//!    APPBARMSGDATAV3 with a shlwapi shared-memory handle allocated for OUR
//!    pid. The spy relays the COPYDATA verbatim, but explorer cannot
//!    SHLockShared a handle that was allocated for a different pid, so
//!    other apps' appbar operations silently get no usable reply.
//!
//!  systray-util 的 TraySpy 在本进程创建了一个类名为
//! "Shell_TrayWnd" 的伪造顶层窗口, 并且每 100ms 把自己重新顶到最前,
//! 因此在所有 FindWindow 竞争中都压过真实的 explorer 任务栏。而
//! SHAppBarMessage 内部正是通过 FindWindow("Shell_TrayWnd") 定位 shell
//! 的 AppBarService, 于是下面两条路径的流量全部落到了 spy 手上, 而它
//! 的转发方式会破坏协议:
//! 1. 同进程调用方 (本应用): shell32 以注册消息 + 裸 APPBARDATA 指针投递
//!    (同进程内指针合法); spy 对 > WM_USER 的消息用 PostMessageW 异步
//!    转发并返回 0, 导致 ABM_NEW/ABM_SETPOS 不生效, rc 也从不被回填,
//!    Windows 因此不再为我们的 bar 保留空间。
//! 2. 其他进程: shell32 发送 WM_COPYDATA (dwData == 0), 内嵌
//!    APPBARMSGDATAV3 与一个按【我们进程】pid 分配的 shlwapi 共享内存
//!    句柄。spy 原样转发该 COPYDATA, 但 explorer 无法 SHLockShared 一个
//!    为别的 pid 分配的句柄, 其他程序的 appbar 操作于是静默地拿不到
//!    任何可用结果。
//!
//! HOW IT IS FIXED :
//! Layer 1 (own calls): relay_appbar_message() skips SHAppBarMessage and
//! FindWindow entirely and talks to the real tray directly, using the
//! cross-process protocol every shell32 uses (WM_COPYDATA + SHAllocShared).
//! Layer 2 (system-wide): patch_tray_spy_appbar() subclasses the spy
//! window; relay_inbound_appbar() intercepts inbound appbar COPYDATA and
//! re-shares + relays it correctly; all other messages fall through to the
//! spy's original wndproc, leaving tray-icon interception untouched.
//!
//! 两层修复。第一层 (自己的调用): relay_appbar_message() 完全
//! 绕开 SHAppBarMessage 和 FindWindow, 用所有 shell32 通用的跨进程协议
//! (WM_COPYDATA + SHAllocShared) 直达真实任务栏。第二层 (全系统):
//! patch_tray_spy_appbar() 子类化 spy 窗口; relay_inbound_appbar() 拦截
//! 入站的 appbar COPYDATA 并正确地重分配 + 中继; 其余消息原样落回 spy
//! 原来的 wndproc, 图标拦截行为不受任何影响。
//!
//! WHY THIS WAY:
//! - The relay re-allocates shared memory FOR EXPLORER'S PID because
//!   SHAllocShared handles are only lockable by the pid they were created
//!   for. Verbatim forwarding cannot work; cairoshell hit the same wall and
//!   uses the same re-share recipe (see AppBarManager.QuerySetPos).
//! - Subclassing instead of forking keeps the upstream crate untouched;
//!   the patch is same-process, ~100 lines, and confined to this file.
//! - Both layers are kept: our own calls stay immune to the spy's 100ms
//!   z-order churn, while the subclass heals the path for every other
//!   process on the system.
//!
//!
//! - 中继必须按 explorer 的 pid 重新分配共享内存, 因为 SHAllocShared 的
//!   句柄只能被创建时指定的 pid 锁定, 原样转发注定行不通; cairoshell
//!   踩过同一个坑, 用的正是同一套重分配配方 (见其
//!   AppBarManager.QuerySetPos)。
//! - 选 subclass 而不是 fork: 上游 crate 保持原样, 补丁同进程、约百行、
//!   只落在本文件内。
//! - 两层并存: 自己的调用不受 spy 每 100ms 置顶搅动 z 序的影响, 子类化
//!   则修复了系统内所有其他进程的通路。

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::core::Result;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, FindWindowExW, FindWindowW, GetWindowThreadProcessId, SendMessageW,
    SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, HWND_TOPMOST, SWP_NOACTIVATE, WM_COPYDATA,
};
use windows_core::w;

// shlwapi shell shared memory: the cross-process carrier for APPBARDATA.
// Handles are only lockable by the pid passed to SHAllocShared, which is
// why relays must re-allocate for the receiver's pid.
// shlwapi 的 shell 共享内存：APPBARDATA 的跨进程载体。句柄只能被
// SHAllocShared 时指定的 pid 锁定，所以中继必须按接收方 pid 重新分配。
#[link(name = "shlwapi")]
extern "system" {
    /// 按目标进程 pid 分配 shell 共享内存
    fn SHAllocShared(pvdata: *const core::ffi::c_void, dwsize: u32, dwprocessid: u32) -> HANDLE;
    /// 锁定共享内存，得到本进程可用的视图
    fn SHLockShared(hdata: HANDLE, dwprocessid: u32) -> *mut core::ffi::c_void;
    /// 解锁共享内存视图
    fn SHUnlockShared(pvview: *mut core::ffi::c_void) -> windows::core::BOOL;
    /// 释放共享内存句柄
    fn SHFreeShared(hdata: HANDLE, dwprocessid: u32) -> windows::core::BOOL;
}

pub const APPBAR_CALLBACK_MSG: u32 = 0x8000;

pub const ABN_STATECHANGE: usize = 0;
pub const ABN_POSCHANGED: usize = 1;
pub const ABN_FULLSCREENAPP: usize = 2;

static ACTIVE: OnceLock<(isize, RECT, i32)> = OnceLock::new();

// Cross-process appbar wire format, reverse-engineered and proven by
// cairoshell/ManagedShell (matches what shell32 sends to the tray window
// from another process). Layout is asserted at compile time below.
// 跨进程 appbar 线格式，由 cairoshell/ManagedShell 逆向并验证（即 shell32
// 从其它进程发给任务栏窗口的格式）。下方有编译期尺寸断言。
#[derive(Clone)]
#[repr(C)]
struct AppbarDataV2 {
    cb_size: u32,
    hwnd: u32,
    u_callback_message: u32,
    u_edge: u32,
    rc: RECT,
    l_param: i64,
}

#[repr(C)]
struct AppbarMsgDataV3 {
    abd: AppbarDataV2,
    dw_message: i32,
    dw_padding1: i32,
    h_shared_memory: isize,
    dw_source_process_id: i32,
    dw_padding3: i32,
}

const _: () = assert!(size_of::<AppbarDataV2>() == 40);
const _: () = assert!(size_of::<AppbarMsgDataV3>() == 64);

/// The real explorer tray window, skipping same-process impostors.
/// systray-util's TraySpy hosts a "Shell_TrayWnd"-classed window inside this
/// process and wins the FindWindow race, which makes SHAppBarMessage deliver
/// appbar traffic to the spy (whose forwarding breaks the protocol), so any
/// direct appbar call must target the real tray explicitly.
///
/// 找到真实的 explorer 任务栏窗口，跳过本进程的伪装者。systray-util 的
/// TraySpy 在本进程内建了一个 "Shell_TrayWnd" 类的窗口并常态赢得
/// FindWindow 竞争，导致 SHAppBarMessage 的 appbar 流量全部落到 spy 上
/// （而它的转发会破坏协议），所以一切直连调用都必须显式定位真实任务栏。
fn real_tray_window() -> Option<(HWND, u32)> {
    let class = w!("Shell_TrayWnd");
    let self_pid = std::process::id();
    let mut candidate = unsafe { FindWindowW(class, None) }.unwrap_or_default();
    while !candidate.is_invalid() {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(candidate, Some(&mut pid)) };
        if pid != 0 && pid != self_pid {
            return Some((candidate, pid));
        }
        candidate =
            unsafe { FindWindowExW(None, Some(candidate), class, None) }.unwrap_or_default();
    }
    None
}

/// Relay one appbar operation to the real tray over freshly allocated
/// shared memory (allocated for the tray's pid, which is required for the
/// tray to lock it), returning the shell's reply and updated APPBARDATA.
///
/// 把一次 appbar 操作中继给真实任务栏：用共享内存承载 APPBARDATA（必须按
/// 任务栏的 pid 重新分配，否则任务栏锁不开），同步等待回包，返回 shell
/// 的应答和被任务栏修改过的 APPBARDATA（如 ABM_SETPOS 批准的矩形）。
fn relay_shared(abd: &AppbarDataV2, dw_message: u32) -> Option<(usize, AppbarDataV2)> {
    let (tray, tray_pid) = real_tray_window()?;
    unsafe {
        let shared = SHAllocShared(std::ptr::null(), size_of::<AppbarDataV2>() as u32, tray_pid);
        if shared.is_invalid() {
            return None;
        }
        let result = (|| {
            let view = SHLockShared(shared, tray_pid);
            if view.is_null() {
                return None;
            }
            (view as *mut AppbarDataV2).write(abd.clone());
            let _ = SHUnlockShared(view);

            let mut v3 = AppbarMsgDataV3 {
                abd: abd.clone(),
                dw_message: dw_message as i32,
                dw_padding1: 0,
                h_shared_memory: shared.0 as isize,
                dw_source_process_id: tray_pid as i32,
                dw_padding3: 0,
            };
            let copy = COPYDATASTRUCT {
                dwData: 0,
                cbData: size_of::<AppbarMsgDataV3>() as u32,
                lpData: &mut v3 as *mut AppbarMsgDataV3 as *mut core::ffi::c_void,
            };
            let lresult = SendMessageW(
                tray,
                WM_COPYDATA,
                Some(WPARAM(abd.hwnd as usize)),
                Some(LPARAM(&copy as *const COPYDATASTRUCT as isize)),
            );

            let view = SHLockShared(shared, tray_pid);
            if view.is_null() {
                return Some((lresult.0 as usize, abd.clone()));
            }
            let updated = (view as *const AppbarDataV2).read();
            let _ = SHUnlockShared(view);
            Some((lresult.0 as usize, updated))
        })();
        let _ = SHFreeShared(shared, tray_pid);
        result
    }
}

/// Relay an appbar operation straight to the real explorer tray window via
/// the cross-process shell protocol (WM_COPYDATA + shlwapi shared memory,
/// as consumed by explorer's AppBarService), bypassing the FindWindow
/// resolution that the in-process tray spy hijacks. This is layer 1 of the
/// fix and covers OUR OWN register/setpos/remove calls.
///
/// 把 appbar 操作经跨进程 shell 协议（WM_COPYDATA + shlwapi 共享内存，
/// 即 explorer AppBarService 消费的格式）直达真实任务栏，绕开被本进程
/// tray spy 劫持的 FindWindow 解析。这是修复的第一层，覆盖我们自己的
/// register/setpos/remove 调用。
fn relay_appbar_message(dw_message: u32, abd: &mut APPBARDATA) -> Option<usize> {
    let v2 = AppbarDataV2 {
        cb_size: size_of::<AppbarDataV2>() as u32,
        hwnd: abd.hWnd.0 as usize as u32,
        u_callback_message: abd.uCallbackMessage,
        u_edge: abd.uEdge,
        rc: abd.rc,
        l_param: abd.lParam.0 as i64,
    };
    let (result, updated) = relay_shared(&v2, dw_message)?;
    abd.rc = updated.rc;
    abd.lParam = LPARAM(updated.l_param as isize);
    Some(result)
}

static SPY_ORIGINAL_PROC: AtomicIsize = AtomicIsize::new(0);
static SPY_PATCHED: AtomicBool = AtomicBool::new(false);

// Replacement wndproc installed on the spy window (layer 2). Appbar
// COPYDATA gets the correct relay; everything else goes back to the spy's
// original proc so tray-icon interception is untouched.
// 安装在 spy 窗口上的替换 wndproc（修复第二层）。appbar COPYDATA 走正确
// 中继；其余消息原样交回 spy 原来的 wndproc，图标拦截行为不受影响。
/// spy 窗口的替换 wndproc：appbar 消息走中继，其余回落原过程
extern "system" fn spy_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        if let Some(result) = unsafe { relay_inbound_appbar(lparam.0 as *const COPYDATASTRUCT) } {
            return result;
        }
    }
    let original = SPY_ORIGINAL_PROC.load(Ordering::SeqCst);
    unsafe {
        CallWindowProcW(
            std::mem::transmute::<isize, windows::Win32::UI::WindowsAndMessaging::WNDPROC>(
                original,
            ),
            hwnd,
            msg,
            wparam,
            lparam,
        )
    }
}

/// Correctly relay an appbar operation that another process sent to the
/// tray-spy window: the inbound shared memory was allocated for our pid, so
/// the real tray cannot lock it as-is. Re-share it for the tray's pid,
/// forward, then write the tray's answer back into the caller's block.
///
/// 修复其他进程发到 spy 窗口的 appbar 操作：入站共享内存是按我们进程的
/// pid 分配的，真实任务栏按原样是锁不开的。这里为任务栏的 pid 重新分
/// 配、转发，再把任务栏的回写结果拷回原调用者的共享块，并返回应答的
/// LRESULT，让调用方的 shell32 像 shell 正常应答一样读到结果。
unsafe fn relay_inbound_appbar(copy: *const COPYDATASTRUCT) -> Option<LRESULT> {
    if (*copy).dwData != 0 || (*copy).cbData != size_of::<AppbarMsgDataV3>() as u32 {
        return None;
    }
    let v3 = (*copy).lpData as *const AppbarMsgDataV3;
    let v3 = &*v3;
    let src_handle = HANDLE(v3.h_shared_memory as *mut core::ffi::c_void);
    let src_pid = v3.dw_source_process_id as u32;
    let old_view = SHLockShared(src_handle, src_pid);
    if old_view.is_null() {
        return None;
    }
    let old_abd = (old_view as *const AppbarDataV2).read();
    let outcome = relay_shared(&old_abd, v3.dw_message as u32);
    let result = match outcome {
        Some((lresult, updated)) => {
            (old_view as *mut AppbarDataV2).write(updated);
            Some(LRESULT(lresult as isize))
        }
        None => None,
    };
    let _ = SHUnlockShared(old_view);
    result
}

/// Subclass systray-util's in-process "Shell_TrayWnd" spy window so that
/// appbar traffic from other processes (WM_COPYDATA dwData == 0) is relayed
/// to the real tray with properly re-shared memory instead of the spy's
/// naive pass-through, which the tray cannot process. This is layer 2 of
/// the fix and heals the system-wide appbar path while the spy lives.
///
/// Subclass（而非 fork）systray-util 在本进程伪造的 "Shell_TrayWnd" spy
/// 窗口：其他进程的 appbar 流量（WM_COPYDATA dwData == 0）改走正确重分
/// 配的中继，而不是 spy 那种任务栏无法处理的裸转发。这是修复的第二
/// 层，spy 存活期间修复全系统的 appbar 通路。同进程 subclass 合法且改
/// 动面最小；调用方在 register_appbar 里、窗口体系就绪后调用一次。
pub fn patch_tray_spy_appbar() {
    if SPY_PATCHED.load(Ordering::SeqCst) {
        return;
    }
    let class = w!("Shell_TrayWnd");
    let self_pid = std::process::id();
    for _ in 0..10 {
        let mut spy = HWND::default();
        let mut candidate = unsafe { FindWindowW(class, None) }.unwrap_or_default();
        while !candidate.is_invalid() {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(candidate, Some(&mut pid)) };
            if pid == self_pid {
                spy = candidate;
                break;
            }
            candidate =
                unsafe { FindWindowExW(None, Some(candidate), class, None) }.unwrap_or_default();
        }
        if !spy.is_invalid() {
            let previous = unsafe { set_wnd_proc(spy, spy_wnd_proc as *const () as usize) };
            if previous != 0 {
                SPY_ORIGINAL_PROC.store(previous, Ordering::SeqCst);
                SPY_PATCHED.store(true, Ordering::SeqCst);
            } else {
                tracing::error!("[appbar] failed to subclass tray spy window");
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    tracing::warn!("[appbar] tray spy window not found, appbar relay patch skipped");
}

/// 替换窗口过程并返回原过程地址
unsafe fn set_wnd_proc(hwnd: HWND, proc: usize) -> isize {
    SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as isize)
}

// All appbar entry points go through here: try the direct relay first
// (deterministic, immune to the spy's FindWindow races); if it is not
// available (e.g. explorer's tray not found, spy absent), fall back to the
// stock SHAppBarMessage, which is correct whenever nobody impersonates the
// tray window.
// 所有 appbar 入口都走这里：先走直连中继（确定性高，不受 spy 的
// FindWindow 竞态影响）；中继不可用（如找不到 explorer 任务栏、spy 不
// 存在）时退回原生 SHAppBarMessage，在没有伪装者的环境下它本来就是对的。
/// appbar 统一入口：优先直连中继，不可用时退回原生调用
fn shell_appbar_message(dw_message: u32, abd: &mut APPBARDATA) -> usize {
    if let Some(result) = relay_appbar_message(dw_message, abd) {
        return result;
    }
    unsafe { SHAppBarMessage(dw_message, abd as *mut APPBARDATA) }
}

/// 注册窗口为 appbar（ABM_NEW）
pub fn register(hwnd: HWND) -> Result<()> {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: APPBAR_CALLBACK_MSG,
        ..Default::default()
    };
    let ok = shell_appbar_message(ABM_NEW, &mut abd);
    if ok == 0 {
        return Err(crate::window::last_error());
    }
    Ok(())
}

/// 记录位置状态并申请顶部 appbar 矩形（ABM_SETPOS）
pub fn set_top_position(hwnd: HWND, monitor_rect: &RECT, height: i32) -> RECT {
    let _ = ACTIVE.set((hwnd.0 as isize, *monitor_rect, height));
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uEdge: ABE_TOP,
        rc: RECT {
            left: monitor_rect.left,
            top: monitor_rect.top,
            right: monitor_rect.right,
            bottom: monitor_rect.top + height,
        },
        ..Default::default()
    };
    shell_appbar_message(ABM_SETPOS, &mut abd);
    abd.rc
}

/// 注销 appbar 注册（ABM_REMOVE）
pub fn remove(hwnd: HWND) {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    };
    shell_appbar_message(ABM_REMOVE, &mut abd);
}

/// 重新申请位置并按批准矩形置顶窗口
pub fn assert_position() -> Option<RECT> {
    let (hwnd_value, monitor_rect, height) = *ACTIVE.get()?;
    let hwnd = HWND(hwnd_value as *mut core::ffi::c_void);
    let approved = set_top_position(hwnd, &monitor_rect, height);
    let width = approved.right - approved.left;
    let height = approved.bottom - approved.top;
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            approved.left,
            approved.top,
            width,
            height,
            SWP_NOACTIVATE,
        );
    }
    Some(approved)
}
