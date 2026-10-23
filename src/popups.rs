use std::time::Instant;

use gpui_kit::*;

#[derive(Clone, Copy)]
pub struct PopupSlot {
    pub window: AnyWindowHandle,
    pub hwnd: isize,
}

#[derive(Default)]
pub struct Popups {
    pub menu: Option<PopupSlot>,
    pub menu_closed_at: Option<Instant>,
    pub media: Option<PopupSlot>,
    pub media_closed_at: Option<Instant>,
    pub audio: Option<PopupSlot>,
    pub audio_closed_at: Option<Instant>,
    pub calendar: Option<PopupSlot>,
    pub calendar_closed_at: Option<Instant>,
    pub sys: Option<PopupSlot>,
    pub sys_closed_at: Option<Instant>,
    pub ports: Option<PopupSlot>,
    pub ports_closed_at: Option<Instant>,
    pub tray: Option<PopupSlot>,
    pub tray_closed_at: Option<Instant>,
}

impl Global for Popups {}

/// 注册弹窗注册表全局，并挂窗口关闭回调清理对应槽位
pub fn init(cx: &mut App) {
    cx.set_global(Popups::default());
    cx.on_window_closed(|cx, id| {
        let popups = cx.global_mut::<Popups>();
        if popups.menu.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.menu = None;
            popups.menu_closed_at = Some(Instant::now());
        }
        if popups.media.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.media = None;
            popups.media_closed_at = Some(Instant::now());
            crate::sources::media::note_media_closed();
        }
        if popups.audio.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.audio = None;
            popups.audio_closed_at = Some(Instant::now());
        }
        if popups.calendar.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.calendar = None;
            popups.calendar_closed_at = Some(Instant::now());
        }
        if popups.sys.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.sys = None;
            popups.sys_closed_at = Some(Instant::now());
        }
        if popups.ports.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.ports = None;
            popups.ports_closed_at = Some(Instant::now());
        }
        if popups.tray.as_ref().is_some_and(|p| p.window.window_id() == id) {
            popups.tray = None;
            popups.tray_closed_at = Some(Instant::now());
        }
    })
    .detach();
}

/// 400ms 防抖窗口：刚关闭的弹窗不允许立刻重开
pub fn grace_ok(closed_at: Option<Instant>) -> bool {
    closed_at.is_none_or(|at| at.elapsed() > std::time::Duration::from_millis(400))
}

/// 卸载点击-away 钩子并移除弹窗窗口
pub fn close_slot(cx: &mut App, slot: PopupSlot) {
    tozo_platform::hook::uninstall_click_away_for(slot.hwnd);
    let _ = slot
        .window
        .update(cx, |_, window, _| window.remove_window());
}

/// 弹窗 HWND 发现与 LL 钩子安装的通用流程，找到后经 on_found 存入槽位
pub fn arm_click_away(
    cx: &mut App,
    title: &str,
    on_found: impl FnOnce(&mut App, isize) + 'static,
) {
    let pid = std::process::id();
    let title = title.to_string();
    cx.spawn(async move |cx| {
        let mut hwnd_value = None;
        for _ in 0..10 {
            if let Some(hwnd) = crate::win::find_hwnd(pid, Some(&title), false) {
                hwnd_value = Some(hwnd);
                break;
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
        }
        let Some(hwnd_value) = hwnd_value else {
            tracing::warn!("[popups] hwnd not found for {title}");
            return;
        };
        let _ = cx.update(|cx: &mut App| {
            tozo_platform::hook::install_click_away(hwnd_value, move || {
                tozo_platform::post_close(tozo_platform::hwnd(hwnd_value));
            });
            on_found(cx, hwnd_value);
        });
    })
    .detach();
}
