use std::{sync::Arc, time::Duration};

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::sysmon::{ProcSnapshot, ProcStore};

const POPUP_WIDTH: f32 = 360.;
const POPUP_HEIGHT: f32 = 440.;
const POPUP_X_MARGIN: f32 = 12.;
const BAR_HEIGHT: f64 = 30.0;

/// 切换进程弹窗：带防抖，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.sys_closed_at) {
        return;
    }
    if popups.sys.is_some() {
        close(cx);
        return;
    }
    open(cx);
}

/// 关闭进程弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().sys.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在屏幕右上角打开进程弹窗并武装点击-away 钩子
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
            title: Some("tozo-sys".into()),
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
        .open_window(options, |_, cx| cx.new(|cx| SysPopup::new(cx)))
        .expect("failed to open sys popup")
        .into();

    popups::arm_click_away(cx, "tozo-sys", move |cx, hwnd| {
        cx.global_mut::<Popups>().sys = Some(PopupSlot { window, hwnd });
    });
}

#[derive(Clone, Copy, PartialEq)]
enum SortKey {
    Cpu,
    Mem,
}

pub struct SysPopup {
    procs: Entity<ProcStore>,
    sort: SortKey,
    desc: bool,
    hovered: Option<u32>,
}

impl SysPopup {
    /// 建进程数据实体并订阅其变更以触发重绘
    fn new(cx: &mut Context<Self>) -> Self {
        let procs = cx.new(|cx| ProcStore::new(cx));
        cx.observe(&procs, |_, _, cx| cx.notify()).detach();
        Self {
            procs,
            sort: SortKey::Cpu,
            desc: true,
            hovered: None,
        }
    }
}

impl Render for SysPopup {
    /// 渲染进程列表：按当前排序键排序，uniform_list 虚拟化滚动（滚轮翻全部进程）
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = self.procs.read(cx).procs.clone();
        rows.sort_by(|a, b| match self.sort {
            SortKey::Cpu => b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Mem => b.mem_bytes.cmp(&a.mem_bytes),
        });
        if !self.desc {
            rows.reverse();
        }
        let rows = Arc::new(rows);
        let hovered = self.hovered;
        let entity = cx.entity();

        div()
            .id("sys-popup")
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
                    .id("proc-header")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .mb(px(4.))
                    .child(
                        div()
                            .id("col-name")
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(rgba(0xFFFFFF78))
                            .child("进程"),
                    )
                    .child(sort_header(
                        "sort-cpu",
                        "CPU",
                        self.sort == SortKey::Cpu,
                        self.desc,
                        cx.listener(|this, _, _, cx| {
                            if this.sort == SortKey::Cpu {
                                this.desc = !this.desc;
                            } else {
                                this.sort = SortKey::Cpu;
                                this.desc = true;
                            }
                            cx.notify();
                        }),
                    ))
                    .child(sort_header(
                        "sort-mem",
                        "RAM",
                        self.sort == SortKey::Mem,
                        self.desc,
                        cx.listener(|this, _, _, cx| {
                            if this.sort == SortKey::Mem {
                                this.desc = !this.desc;
                            } else {
                                this.sort = SortKey::Mem;
                                this.desc = true;
                            }
                            cx.notify();
                        }),
                    ))
                    .child(div().w(px(16.))),
            )
            .child(
                uniform_list(
                    "proc-list",
                    rows.len(),
                    move |range, _window, _cx| {
                        range
                            .map(|ix| proc_row(rows[ix].clone(), hovered, entity.clone()))
                            .collect()
                    },
                )
                .flex_1()
                .w_full(),
            )
            .with_animation(
                "sys-pop-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}

/// 列头单元格：显示排序方向，点击切换排序键
fn sort_header(
    id: &'static str,
    label: &'static str,
    active: bool,
    desc: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let text = if active {
        if desc {
            format!("{label} ▾")
        } else {
            format!("{label} ▴")
        }
    } else {
        label.to_string()
    };
    div()
        .id(id)
        .w(px(64.))
        .text_right()
        .text_size(px(11.))
        .text_color(rgba(if active { 0xFFFFFFEB } else { 0xFFFFFF78 }))
        .rounded(px(4.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .on_click(on_click)
        .child(text)
}

/// 进程行：名称、CPU、RAM 与 hover 显示的杀进程按钮
fn proc_row(proc: ProcSnapshot, hovered: Option<u32>, entity: Entity<SysPopup>) -> impl IntoElement {
    let is_hovered = hovered == Some(proc.pid);
    let pid = proc.pid;
    let name_display = if proc.name.is_empty() {
        format!("({pid})")
    } else {
        proc.name
    };
    div()
        .id(("proc", pid))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(20.))
        .rounded(px(4.))
        .when(is_hovered, |el| el.bg(rgba(0xFFFFFF28)))
        .on_hover(move |is_hovered: &bool, _, cx| {
            entity.update(cx, |this, cx| {
                if *is_hovered {
                    if this.hovered != Some(pid) {
                        this.hovered = Some(pid);
                        cx.notify();
                    }
                } else if this.hovered == Some(pid) {
                    this.hovered = None;
                    cx.notify();
                }
            });
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(rgba(0xFFFFFFD0))
                .truncate()
                .child(name_display),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .text_size(px(11.))
                .font_family("Consolas")
                .text_color(rgba(if proc.cpu >= 10. { 0xFFD87AEB } else { 0xFFFFFFD0 }))
                .child(format!("{:.1}%", proc.cpu)),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .text_size(px(11.))
                .font_family("Consolas")
                .text_color(rgba(0xFFFFFFD0))
                .child(fmt_mem(proc.mem_bytes)),
        )
        .child(
            div()
                .id(("kill", pid))
                .w(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_size(px(11.))
                .text_color(rgba(if is_hovered { 0xFF6E6EEB } else { 0xFF6E6E00 }))
                .hover(|style| style.bg(rgba(0xFF6E6E3C)))
                .on_click(move |_, _, _| {
                    crate::sources::sysmon::kill_process(pid);
                })
                .child(IconName::X),
        )
}

/// 内存值自适应 M/G 的单位格式化
fn fmt_mem(bytes: u64) -> String {
    let mb = bytes as f64 / 1024.0 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1}G", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{:.0}M", mb)
    } else {
        format!("{:.1}M", mb)
    }
}
