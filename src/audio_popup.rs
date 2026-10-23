use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::audio::{self, AudioState};

const POPUP_WIDTH: f32 = 300.;
const POPUP_BASE: f32 = 78.;
const SECTION_HEADER: f32 = 26.;
const SECTION_DIVIDER: f32 = 15.;
const ROW: f32 = 26.;
const HEIGHT_MIN: f32 = 120.;
const HEIGHT_MAX: f32 = 480.;
const POPUP_X_MARGIN: f32 = 12.;
const BAR_HEIGHT: f64 = 30.0;

/// Volume track x-origin and width in popup-local logical px:
/// padding (12) + mute button (24) + gap (6).
const TRACK_X: f32 = 42.;
const TRACK_W: f32 = 234.;

/// 按输入/输出设备数计算弹窗高度并夹取到上下限
fn popup_height(state: &AudioState) -> f32 {
    let outputs = state.devices.iter().filter(|d| !d.input).count();
    let inputs = state.devices.iter().filter(|d| d.input).count();
    let mut height = POPUP_BASE;
    if outputs > 0 {
        height += SECTION_HEADER + outputs as f32 * ROW;
    }
    if inputs > 0 {
        height += SECTION_HEADER + inputs as f32 * ROW;
    }
    if outputs > 0 && inputs > 0 {
        height += SECTION_DIVIDER;
    }
    height.clamp(HEIGHT_MIN, HEIGHT_MAX)
}

/// 切换音量弹窗：带防抖，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.audio_closed_at) {
        return;
    }
    if popups.audio.is_some() {
        close(cx);
        return;
    }
    open(cx);
}

/// 关闭音量弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().audio.take() {
        popups::close_slot(cx, slot);
    }
}

/// 按当前设备数算高度，在屏幕右上角打开音量弹窗
fn open(cx: &mut App) {
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let mon_w = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let state = cx
        .global::<crate::AudioGlobal>()
        .0
        .read(cx)
        .state
        .clone();
    let height = popup_height(&state);
    let x = mon_w - POPUP_WIDTH as f64 - POPUP_X_MARGIN as f64;
    let y = monitor.rect.top as f64 / scale + BAR_HEIGHT + 4.;

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x as f32), px(y as f32)),
            size: size(px(POPUP_WIDTH), px(height)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("tozo-audio".into()),
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
            let audio = cx.global::<crate::AudioGlobal>().0.clone();
            cx.new(|cx| AudioPopup::new(audio, cx))
        })
        .expect("failed to open audio popup")
        .into();

    popups::arm_click_away(cx, "tozo-audio", move |cx, hwnd| {
        cx.global_mut::<Popups>().audio = Some(PopupSlot { window, hwnd });
    });
}

pub struct AudioPopup {
    audio: gpui::Entity<audio::AudioStore>,
}

impl AudioPopup {
    /// 持有音频数据实体并订阅其变更以触发重绘
    fn new(audio: gpui::Entity<audio::AudioStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&audio, |_, _, cx| cx.notify()).detach();
        Self { audio }
    }
}

impl Render for AudioPopup {
    /// 渲染音量条、静音按钮与输入/输出设备分区列表
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.audio.read(cx).state.clone();
        let volume = state.volume;
        let muted = state.muted;
        let devices = state.devices.clone();

        div()
            .id("audio-popup")
            .size_full()
            .flex()
            .flex_col()
            .p(px(12.))
            .rounded(px(14.))
            .bg(rgba(0x1C1C20A0))
            .font_family("Microsoft YaHei")
            .overflow_hidden()
            .child(section_label("音量"))
            .child(
                div()
                    .id("vol-row")
                    .flex()
                    .flex_row()
                    .items_center()
                    .mb(px(8.))
                    .child(mute_button(volume, muted))
                    .child(volume_bar(volume)),
            )
            .children(device_sections(&devices))
            .with_animation(
                "audio-popup-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}

/// 分区小标题文本
fn section_label(text: &str) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgba(0xFFFFFF78))
        .mb(px(2.))
        .ml(px(2.))
        .child(text.to_string())
}

/// 组装输出/输入设备分区，两组都有时插入分隔线
fn device_sections(devices: &[audio::AudioDevice]) -> Vec<AnyElement> {
    let outputs: Vec<audio::AudioDevice> = devices.iter().filter(|d| !d.input).cloned().collect();
    let inputs: Vec<audio::AudioDevice> = devices.iter().filter(|d| d.input).cloned().collect();
    let both = !outputs.is_empty() && !inputs.is_empty();
    let mut sections = Vec::new();
    if !outputs.is_empty() {
        sections.push(device_section("输出设备", &outputs).into_any_element());
    }
    if both {
        sections.push(
            div()
                .id("divider")
                .w_full()
                .h(px(1.))
                .bg(rgba(0xFFFFFF12))
                .mx(px(7.))
                .my(px(7.))
                .into_any_element(),
        );
    }
    if !inputs.is_empty() {
        sections.push(device_section("输入设备", &inputs).into_any_element());
    }
    sections
}

/// 单个设备分区：标题加设备行列表
fn device_section(title: &'static str, devices: &[audio::AudioDevice]) -> impl IntoElement {
    div()
        .id(title)
        .flex()
        .flex_col()
        .child(section_label(title))
        .children(devices.iter().cloned().map(device_row))
}

/// 设备行：默认设备带勾选高亮，点击切换默认设备
fn device_row(device: audio::AudioDevice) -> impl IntoElement {
    let id = device.id.clone();
    let is_default = device.is_default;
    div()
        .id(device.id.clone())
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .rounded(px(6.))
        .px(px(6.))
        .py(px(3.))
        .my(px(1.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .on_mouse_down(MouseButton::Left, move |_, _, _| {
            audio::submit(audio::Command::SetDefault(id.clone()));
        })
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgba(if is_default { 0x56D8FFEB } else { 0x56D8FF00 }))
                .mr(px(6.))
                .child(IconName::Check),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.))
                .text_color(rgba(if is_default { 0xFFFFFFEB } else { 0xFFFFFFA0 }))
                .truncate()
                .child(device.name),
        )
}

/// 静音按钮：按音量/静音状态切图标，点击切换静音
fn mute_button(volume: f32, muted: bool) -> impl IntoElement {
    let icon = if muted {
        IconName::VolumeX
    } else if volume < 0.5 {
        IconName::Volume1
    } else {
        IconName::Volume2
    };
    let color = if muted {
        rgba(0xFFFFFF6E)
    } else {
        rgba(0xFFFFFFEB)
    };
    div()
        .id("mute-btn")
        .flex()
        .items_center()
        .p(px(3.))
        .mr(px(6.))
        .rounded(px(6.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .on_mouse_down(MouseButton::Left, move |_, _, _| {
            audio::submit(audio::Command::SetMute(!muted));
        })
        .child(
            div()
                .text_size(px(14.))
                .text_color(color)
                .child(icon),
        )
}

/// 音量条：按下或按住拖动时按 x 坐标换算音量值
fn volume_bar(volume: f32) -> impl IntoElement {
    div()
        .id("vol-track")
        .flex_1()
        .h(px(18.))
        .flex()
        .items_center()
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, _| {
            let value = ((event.position.x.as_f32() - TRACK_X) / TRACK_W).clamp(0., 1.);
            audio::submit(audio::Command::SetVolume(value));
        })
        .on_mouse_move(move |event: &MouseMoveEvent, _, _| {
            if event.pressed_button == Some(MouseButton::Left) {
                let value = ((event.position.x.as_f32() - TRACK_X) / TRACK_W).clamp(0., 1.);
                audio::submit(audio::Command::SetVolume(value));
            }
        })
        .child(
            div()
                .id("vol-track-visual")
                .w_full()
                .h(px(6.))
                .rounded_full()
                .bg(rgba(0xFFFFFF3C))
                .overflow_hidden()
                .child(
                    div()
                        .id("vol-fill")
                        .w(relative(volume.clamp(0., 1.)))
                        .h_full()
                        .rounded_full()
                        .bg(rgba(0xFFFFFFEB)),
                ),
        )
}
