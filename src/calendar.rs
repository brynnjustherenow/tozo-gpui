use std::time::Duration;

use chrono::{Datelike, Local, NaiveDate};
use gpui_kit::{assets::IconName, base::StyledExt};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};

const POPUP_WIDTH: f32 = 300.;
const POPUP_HEIGHT: f32 = 340.;
const POPUP_X_MARGIN: f32 = 12.;
const BAR_HEIGHT: f64 = 30.0;

const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

/// 切换日历弹窗：带防抖，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.calendar_closed_at) {
        return;
    }
    if popups.calendar.is_some() {
        close(cx);
        return;
    }
    open(cx);
}

/// 关闭日历弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().calendar.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在屏幕右上角打开日历弹窗并武装点击-away 钩子
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
            title: Some("tozo-calendar".into()),
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
        .open_window(options, |_, cx| cx.new(|_| CalendarPop::new()))
        .expect("failed to open calendar popup")
        .into();

    popups::arm_click_away(cx, "tozo-calendar", move |cx, hwnd| {
        cx.global_mut::<Popups>().calendar = Some(PopupSlot { window, hwnd });
    });
}

struct CalendarPop {
    view: NaiveDate,
    selected: NaiveDate,
}

impl CalendarPop {
    /// 初始视图与选中日期均为今天
    fn new() -> Self {
        let today = Local::now().date_naive();
        Self {
            view: today,
            selected: today,
        }
    }

    /// 视图月份偏移 delta 个月，取该月一号的日期
    fn shift_month(&self, delta: i32) -> NaiveDate {
        let (y, m) = (self.view.year(), self.view.month() as i32);
        let total = y * 12 + (m - 1) + delta;
        let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        NaiveDate::from_ymd_opt(ny, nm as u32, 1).unwrap_or(self.view)
    }
}

impl Render for CalendarPop {
    /// 渲染月份导航、星期头与 6×7 月格（周一起始，今天/选中/跨月分色）
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let today = Local::now().date_naive();
        let view = self.view;
        let selected = self.selected;
        let entity = cx.entity();
        let header = format!("{}年{}月", view.year(), view.month());
        let prev = cx.listener(|this, _, _, cx| {
            this.view = this.shift_month(-1);
            cx.notify();
        });
        let next = cx.listener(|this, _, _, cx| {
            this.view = this.shift_month(1);
            cx.notify();
        });

        let first = NaiveDate::from_ymd_opt(view.year(), view.month(), 1).unwrap_or(view);
        let start = first - chrono::TimeDelta::days(first.weekday().num_days_from_monday() as i64);

        div()
            .id("calendar-pop")
            .size_full()
            .flex()
            .flex_col()
            .p(px(10.))
            .rounded(px(14.))
            .bg(rgba(0x1C1C20A0))
            .font_family("Microsoft YaHei")
            .child(
                div()
                    .id("cal-header")
                    .flex()
                    .flex_row()
                    .items_center()
                    .mb(px(4.))
                    .child(nav_button("cal-prev", IconName::ChevronLeft, prev))
                    .child(
                        div()
                            .id("cal-title")
                            .flex_1()
                            .text_center()
                            .text_size(px(13.))
                            .text_color(rgba(0xFFFFFFEB))
                            .child(header),
                    )
                    .child(nav_button("cal-next", IconName::ChevronRight, next)),
            )
            .child(
                div()
                    .id("cal-weekdays")
                    .flex()
                    .flex_row()
                    .children(WEEKDAYS.map(|d| {
                        div()
                            .flex_1()
                            .text_center()
                            .text_size(px(11.))
                            .text_color(rgba(0xFFFFFF6E))
                            .child(d)
                    })),
            )
            .child(
                div()
                    .id("cal-grid")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .children((0..6usize).map(|w| {
                        div()
                            .id(("cal-week", w))
                            .flex()
                            .flex_row()
                            .flex_1()
                            .children(
                                (0..7usize).map(|d| day_cell(entity.clone(), start, w * 7 + d, view, selected, today)),
                            )
                    })),
            )
            .with_animation(
                "cal-pop-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}

/// 月份导航箭头按钮
fn nav_button(
    id: &'static str,
    icon: IconName,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(6.))
        .hover(|style| style.bg(rgba(0xFFFFFF3C)))
        .on_click(on_click)
        .child(
            div()
                .text_size(px(14.))
                .text_color(rgba(0xFFFFFFEB))
                .child(icon),
        )
}

/// 单个日期格：按今天/选中/是否本月着色，点击选中该日期
fn day_cell(
    entity: Entity<CalendarPop>,
    start: NaiveDate,
    offset: usize,
    view: NaiveDate,
    selected: NaiveDate,
    today: NaiveDate,
) -> impl IntoElement {
    let date = start + chrono::TimeDelta::days(offset as i64);
    let in_month = date.month() == view.month() && date.year() == view.year();
    let is_today = date == today;
    let is_selected = date == selected;
    let label = date.day().to_string();

    let text_color = if is_selected {
        rgba(0xFFFFFFFF)
    } else if is_today {
        rgba(0x56D8FFEB)
    } else if in_month {
        rgba(0xFFFFFFD0)
    } else {
        rgba(0xFFFFFF6E)
    };

    div()
        .id(date.format("%Y-%m-%d").to_string())
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(text_color)
        .when(is_selected, |el| el.paddings(px(1.)).bg(rgba(0x007AFFEB))
    .rounded(px(8.)))
        .when(!is_selected, |el| {
            el.rounded(px(8.))
                .hover(|style| style.bg(rgba(0xFFFFFF32)))
        })
        .on_click(move |_, _, cx| {
            entity.update(cx, |this, cx| {
                this.selected = date;
                cx.notify();
            });
        })
        .child(label)
}
