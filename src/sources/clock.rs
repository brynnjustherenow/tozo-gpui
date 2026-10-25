use std::time::Duration;

use chrono::{DateTime, Local, Timelike};
use gpui_kit::prelude::*;
use gpui_kit::*;

pub struct ClockStore {
    pub dt: DateTime<Local>,
}

impl ClockStore {
    /// 启动分钟对齐定时任务，每到整分更新一次时间
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let now = Local::now();
            let next_minute = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .map(|t| t + chrono::TimeDelta::minutes(1))
                .unwrap_or(now + chrono::TimeDelta::minutes(1));
            let wait = (next_minute - now)
                .to_std()
                .unwrap_or(Duration::from_secs(1))
                .max(Duration::from_millis(50));
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.dt = Local::now();
                cx.notify();
            });
        })
        .detach();
        Self { dt: Local::now() }
    }
}

impl Render for ClockStore {
    /// 渲染时钟岛，点击打开日历弹窗
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        pill()
            .id("clock-island")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.defer(|cx| crate::calendar::toggle(cx));
                }),
            )
            .child(self.dt.format("%H:%M").to_string())
    }
}

/// 各岛共用的胶囊底样式
pub(crate) fn pill() -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .px_3()
        .py_1()
        .rounded_full()
        .text_size(px(12.))
        .text_color(rgba(0xFFFFFFEB))
}
