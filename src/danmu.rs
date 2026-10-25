//! 弹幕 overlay：bar 之下的全屏透明置顶窗口，点击穿透，仅作被动渲染。
//!
//! 与 popups 槽位复用生命周期管理（不含 click-away 钩子——窗口本身穿透），
//! HWND 就绪后延迟 400ms 开启 WS_EX_TRANSPARENT，避免开窗点击穿透落到下层窗口。

use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::danmu::{ActiveDanmu, DanmuScreen, DanmuStore};

const BAR_HEIGHT: f64 = 30.0;
const TITLE: &str = "tozo-danmu";

/// 切换弹幕 overlay（媒体弹窗按钮入口）；已开则关
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if popups.danmu.is_some() {
        close(cx);
        return;
    }
    if !popups::grace_ok(popups.danmu_closed_at) {
        return;
    }
    open(cx);
}

/// 关闭 overlay 并复位使能位
pub fn close(cx: &mut App) {
    crate::sources::danmu::set_danmu_enabled(false);
    if let Some(slot) = cx.global_mut::<Popups>().danmu.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在 bar 下方铺满主显示器打开透明 overlay
fn open(cx: &mut App) {
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let w = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let h = (monitor.rect.bottom - monitor.rect.top) as f64 / scale;
    let y = monitor.rect.top as f64 / scale + BAR_HEIGHT;

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(y as f32)),
            size: size(px(w as f32), px((h - BAR_HEIGHT) as f32)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some(TITLE.into()),
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
        .open_window(options, |_, cx| cx.new(DanmuOverlay::new))
        .expect("failed to open danmu overlay")
        .into();

    crate::sources::danmu::set_danmu_enabled(true);
    cx.global_mut::<Popups>().danmu = Some(PopupSlot { window, hwnd: 0 });
    arm_overlay(cx);
}

/// 等 HWND 就绪后置顶 + 点击穿透，回填槽位句柄
fn arm_overlay(cx: &mut App) {
    let pid = std::process::id();
    cx.spawn(async move |cx| {
        let mut hwnd_value = None;
        for _ in 0..20 {
            if let Some(hwnd) = crate::win::find_hwnd(pid, Some(TITLE), false) {
                hwnd_value = Some(hwnd);
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
        }
        let Some(hwnd_value) = hwnd_value else {
            // 找不到句柄就无法开点击穿透，全屏窗口会吞掉所有点击，必须关掉
            tracing::error!("[danmu] overlay hwnd not found, closing overlay");
            let _ = cx.update(|cx: &mut App| crate::danmu::close(cx));
            return;
        };
        // 留出时间让开窗点击结束，再开穿透
        cx.background_executor()
            .timer(Duration::from_millis(400))
            .await;
        let _ = cx.update(|cx: &mut App| {
            let hwnd = tozo_platform::hwnd(hwnd_value);
            tozo_platform::set_topmost(hwnd, true);
            tozo_platform::set_click_through(hwnd, true);
            if let Some(slot) = cx.global_mut::<Popups>().danmu.as_mut() {
                slot.hwnd = hwnd_value;
            }
        });
    })
    .detach();
}

struct DanmuOverlay {
    store: Entity<DanmuStore>,
}

impl DanmuOverlay {
    fn new(cx: &mut Context<Self>) -> Self {
        let store = cx.new(DanmuStore::new);
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self { store }
    }
}

impl Render for DanmuOverlay {
    /// 全屏透明画布：先画全部阴影再画全部正文，避免后画的阴影盖住先画的正文
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.store.read(cx).screen.clone();
        let width = window.viewport_size().width.as_f32();
        div()
            .id("danmu-overlay")
            .size_full()
            .relative()
            .font_family("Microsoft YaHei")
            .children(
                screen
                    .items
                    .iter()
                    .map(|d| danmu_text(d, &screen, width, true)),
            )
            .children(
                screen
                    .items
                    .iter()
                    .map(|d| danmu_text(d, &screen, width, false)),
            )
    }
}

/// 单条弹幕：按 t_ms 与 spawn_ms 差值换算水平位移，shadow 画下方偏移 1px 的暗色衬底
fn danmu_text(d: &ActiveDanmu, screen: &DanmuScreen, width: f32, shadow: bool) -> impl IntoElement {
    let frac = (screen.t_ms.saturating_sub(d.spawn_ms)) as f32 / d.travel_ms.max(1) as f32;
    let est = d.est_px as f32;
    let x = width - frac * (width + est);
    let y = d.lane as f32 * screen.layout.line_height + screen.layout.top_offset;
    let text = if d.count > 1 {
        format!("{} ×{}", d.content, d.count)
    } else {
        d.content.clone()
    };
    let color = if shadow {
        rgba(0x000000C8)
    } else {
        // RGB888 → 0xRRGGBBAA
        rgba(((d.color & 0xFF_FFFF) << 8) | 0xFF)
    };
    let (dx, dy) = if shadow { (0., 1.) } else { (0., 0.) };
    div()
        .id((if shadow { "danmu-shadow" } else { "danmu" }, d.id as usize))
        .absolute()
        .left(px(x + dx))
        .top(px(y + dy))
        .text_size(px(screen.layout.font_px))
        .text_color(color)
        .whitespace_nowrap()
        .child(text)
}
