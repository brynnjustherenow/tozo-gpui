use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::ports::{PortEntry, PortStore};

const POPUP_WIDTH: f32 = 360.;
const POPUP_HEIGHT: f32 = 440.;
const POPUP_X_MARGIN: f32 = 12.;
const BAR_HEIGHT: f64 = 30.0;
const MAX_ROWS: usize = 60;

/// 切换端口弹窗：带防抖，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.ports_closed_at) {
        return;
    }
    if popups.ports.is_some() {
        close(cx);
        return;
    }
    open(cx);
}

/// 关闭端口弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().ports.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在屏幕右上角打开端口弹窗并武装点击-away 钩子
fn open(cx: &mut App) {
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let mon_w = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let x = mon_w - POPUP_WIDTH as f64 - POPUP_X_MARGIN as f64;
    let y = monitor.rect.top as f64 / scale + BAR_HEIGHT + 4.;

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x as f32), px(y as f32)),
            size: size(px(POPUP_WIDTH), px(POPUP_HEIGHT)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("tozo-ports".into()),
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
            let ports = cx
                .global::<crate::PortsGlobal>()
                .0
                .clone();
            cx.new(|cx| PortsPopup::new(ports, cx))
        })
        .expect("failed to open ports popup")
        .into();

    popups::arm_click_away(cx, "tozo-ports", move |cx, hwnd| {
        cx.global_mut::<Popups>().ports = Some(PopupSlot { window, hwnd });
    });
}

pub struct PortsPopup {
    ports: Entity<PortStore>,
    names: HashMap<u32, String>,
}

impl PortsPopup {
    /// 订阅端口数据，并起 3s 循环后台解析 PID 到进程名
    fn new(ports: Entity<PortStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ports, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |this, cx| {
            loop {
                let names = cx
                    .background_executor()
                    .spawn(async move { pid_names() })
                    .await;
                if this
                    .update(cx, |popup, cx| {
                        popup.names = names;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(3))
                    .await;
            }
        })
        .detach();
        Self {
            ports,
            names: HashMap::new(),
        }
    }
}

/// 一次性枚举全部进程，构建 PID 到进程名的映射（不取 CPU/内存）
fn pid_names() -> HashMap<u32, String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        false,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.processes()
        .iter()
        .map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().to_string()))
        .collect()
}

impl Render for PortsPopup {
    /// 渲染监听端口列表：端口、地址、进程名列，最多 MAX_ROWS 行
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.ports.read(cx).entries.clone();
        let names = &self.names;
        let count = entries
            .iter()
            .map(|e| e.port)
            .collect::<std::collections::HashSet<_>>()
            .len();

        div()
            .id("ports-popup")
            .size_full()
            .flex()
            .flex_col()
            .p(px(10.))
            .rounded(px(14.))
            .bg(rgba(0x1C1C20A0))
            .font_family("Microsoft YaHei")
            .overflow_hidden()
            .child(
                div()
                    .id("ports-header")
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .items_center()
                    .mb(px(4.))
                    .child(div().size(px(6.)).rounded_full().bg(rgba(0x4ADE80FF)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgba(0xFFFFFF78))
                            .child(format!("监听端口 {count} · TCP/UDP")),
                    ),
            )
            .child(
                div()
                    .id("ports-header-row")
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .items_center()
                    .mb(px(2.))
                    .child(header_cell("proto", "协议", 36.))
                    .child(header_cell("port", "端口", 52.))
                    .child(header_cell("addr", "地址", 120.))
                    .child(header_cell("proc", "进程", 0.)),
            )
            .child(
                div()
                    .id("ports-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .children(
                        entries
                            .into_iter()
                            .take(MAX_ROWS)
                            .map(|entry| port_row(entry, names)),
                    ),
            )
            .with_animation(
                "ports-pop-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}

/// 列表头单元格：定宽或弹性占满剩余宽度
fn header_cell(id: &'static str, label: &'static str, width: f32) -> impl IntoElement {
    let base = div()
        .id(id)
        .text_size(px(11.))
        .text_color(rgba(0xFFFFFF78));
    if width > 0. {
        base.w(px(width))
    } else {
        base.flex_1()
    }
    .child(label)
}

/// 端口行：端口、地址与进程名（pid 为 0 显示 System，未知显示 ?）
fn port_row(entry: PortEntry, names: &HashMap<u32, String>) -> impl IntoElement {
    let name = if entry.pid == 0 {
        "System".to_string()
    } else {
        names
            .get(&entry.pid)
            .cloned()
            .unwrap_or_else(|| "?".to_string())
    };
    div()
        .id(format!("{}:{}:{}", entry.proto.as_str(), entry.port, entry.addr))
        .flex()
        .flex_row()
        .gap(px(8.))
        .items_center()
        .h(px(20.))
        .px(px(4.))
        .rounded(px(4.))
        .hover(|style| style.bg(rgba(0xFFFFFF1E)))
        .child(
            div()
                .w(px(36.))
                .text_size(px(10.))
                .text_color(rgba(0xFFFFFF78))
                .child(entry.proto.as_str()),
        )
        .child(
            div()
                .w(px(52.))
                .text_size(px(11.))
                .font_family("Consolas")
                .text_color(rgba(0x4ADE80EB))
                .child(entry.port.to_string()),
        )
        .child(
            div()
                .w(px(120.))
                .min_w_0()
                .text_size(px(11.))
                .font_family("Consolas")
                .text_color(rgba(0xFFFFFFA0))
                .truncate()
                .child(entry.addr),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(rgba(0xFFFFFFD0))
                .truncate()
                .child(format!("{name} ({})", entry.pid)),
        )
}
