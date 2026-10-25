use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, Popups, PopupSlot};

/// 关闭右键菜单（若开着）
pub fn close_menu(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().menu.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在光标处打开右键菜单（带防抖与屏幕边界夹取），重复调用则关闭
pub fn open(cx: &mut App) {
    let popups_ref = cx.global::<Popups>();
    if !popups::grace_ok(popups_ref.menu_closed_at) {
        return;
    }
    if popups_ref.menu.is_some() {
        close_menu(cx);
        return;
    }
    let Some((mx, my)) = tozo_platform::cursor_pos() else {
        return;
    };
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let mon_w = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let mon_h = (monitor.rect.bottom - monitor.rect.top) as f64 / scale;
    let w = 176.0f32;
    let h = 108.0f32;
    let x = ((mx as f64 - monitor.rect.left as f64) / scale)
        .min(mon_w - w as f64 - 8.)
        .max(0.);
    let y = (((my as f64 - monitor.rect.top as f64) / scale) + 2.)
        .min(mon_h - h as f64 - 8.)
        .max(0.);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x as f32), px(y as f32)),
            size: size(px(w), px(h)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("tozo-gpui-menu".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_resizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };

    let window: AnyWindowHandle = cx
        .open_window(options, |_, cx| cx.new(|_| Menu))
        .expect("failed to open menu window")
        .into();

    popups::arm_click_away(cx, "tozo-gpui-menu", move |cx, hwnd| {
        cx.global_mut::<Popups>().menu = Some(PopupSlot { window, hwnd });
    });
}

struct Menu;

impl Render for Menu {
    /// 渲染菜单项：退出应用与关闭菜单
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("menu-root")
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap_1()
            .px_1()
            .rounded_xl()
            .bg(rgba(0x1C1C20E6))
            .font_family("Microsoft YaHei")
            .child(
                div()
                    .id("menu-compact")
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(rgba(0xFFFFFFEB))
                    .hover(|style| style.bg(rgba(0xFFFFFF2E)))
                    .on_click(cx.listener(|_, _, window, cx| {
                        let media = cx.global::<crate::MediaGlobal>().0.clone();
                        media.update(cx, |store, cx| {
                            store.compact = !store.compact;
                            cx.notify();
                        });
                        window.remove_window();
                    }))
                    .child("紧凑媒体岛"),
            )
            .child(
                div()
                    .id("menu-quit")
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(rgba(0xFFFFFFEB))
                    .hover(|style| style.bg(rgba(0xFFFFFF2E)))
                    .on_click(cx.listener(|_, _, _, cx| crate::quit(cx)))
                    .child("退出 tozo-gpui"),
            )
            .child(
                div()
                    .id("menu-close")
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(rgba(0xFFFFFFEB))
                    .hover(|style| style.bg(rgba(0xFFFFFF2E)))
                    .on_click(cx.listener(|_, _, window, _| window.remove_window()))
                    .child("关闭菜单"),
            )
    }
}
