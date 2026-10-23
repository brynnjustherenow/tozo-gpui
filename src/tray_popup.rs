use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::traypanel::{tray_icon_view, TrayGlobal, TraysState, MAX_PANEL_ICONS};

const CELL: f32 = 28.;
const GAP: f32 = 4.;
const PAD: f32 = 12.;
const COLS: usize = 6;
const X_MARGIN: f32 = 8.;
const BAR_HEIGHT: f64 = 30.;

/// 切换托盘溢出弹窗：带防抖，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.tray_closed_at) {
        return;
    }
    if popups.tray.is_some() {
        close(cx);
        return;
    }
    open(cx);
}

/// 关闭托盘弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().tray.take() {
        popups::close_slot(cx, slot);
    }
}

/// 无溢出图标时不动作；否则按溢出数量算网格尺寸，在左上角开窗
fn open(cx: &mut App) {
    let store = cx.global::<TrayGlobal>().0.clone();
    let overflow_count = store
        .read(cx)
        .visible_entries()
        .len()
        .saturating_sub(MAX_PANEL_ICONS);
    if overflow_count == 0 {
        return;
    }
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let cols = COLS.min(overflow_count);
    let rows = overflow_count.div_ceil(cols);
    let width = 2. * PAD + cols as f32 * CELL + (cols - 1) as f32 * GAP;
    let height = 2. * PAD + rows as f32 * CELL + (rows - 1) as f32 * GAP;
    let x = X_MARGIN;
    let y = monitor.rect.top as f64 / scale + BAR_HEIGHT + 4.;

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x), px(y as f32)),
            size: size(px(width), px(height)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("tozo-tray".into()),
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
        .open_window(options, |_, cx| {
            cx.new(|cx| {
                let subscription = cx.observe(&store, |_, _, cx| cx.notify());
                TrayPop {
                    store: store.clone(),
                    _subscription: subscription,
                }
            })
        })
        .expect("failed to open tray popup")
        .into();

    popups::arm_click_away(cx, "tozo-tray", move |cx, hwnd| {
        cx.global_mut::<Popups>().tray = Some(PopupSlot { window, hwnd });
    });
}

struct TrayPop {
    store: Entity<TraysState>,
    _subscription: Subscription,
}

impl Render for TrayPop {
    /// 渲染溢出托盘图标的网格视图
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (overflow, hovered_id) = {
            let state = self.store.read(cx);
            (
                state
                    .visible_entries()
                    .into_iter()
                    .skip(MAX_PANEL_ICONS)
                    .collect::<Vec<_>>(),
                state.hovered.clone(),
            )
        };
        let store = self.store.clone();
        let icons: Vec<AnyElement> = overflow
            .iter()
            .map(|(idx, id, icon_img)| {
                tray_icon_view(
                    ("tray-pop-icon", *idx),
                    CELL,
                    icon_img.clone(),
                    hovered_id.as_ref() == Some(id),
                    id.clone(),
                    store.clone(),
                )
            })
            .collect();
        div()
            .id("tray-pop")
            .size_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_start()
            .gap(px(GAP))
            .p(px(PAD))
            .rounded(px(14.))
            .bg(rgba(0x1C1C20A0))
            .children(icons)
            .with_animation(
                "tray-pop-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}
