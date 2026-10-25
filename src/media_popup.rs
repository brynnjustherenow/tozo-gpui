use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::popups::{self, PopupSlot, Popups};
use crate::sources::media::{self, MediaCommand, MediaStore};

const POPUP_WIDTH: f32 = 320.;
const POPUP_HEIGHT: f32 = 124.;
const BAR_HEIGHT: f64 = 30.0;
/// Popup padding (10) + artwork (96) + artwork margin (10), in logical px.
const SEEK_TRACK_X: f32 = 116.;
const SEEK_TRACK_W: f32 = 184.;

/// 切换媒体弹窗：带防抖与 media 关闭宽限期，已开则关闭
pub fn toggle(cx: &mut App) {
    let popups = cx.global::<Popups>();
    if !popups::grace_ok(popups.media_closed_at) {
        return;
    }
    if popups.media.is_some() {
        close(cx);
        return;
    }
    if media::media_closed_recently() {
        return;
    }
    open(cx);
}

/// 关闭媒体弹窗并清理槽位
pub fn close(cx: &mut App) {
    if let Some(slot) = cx.global_mut::<Popups>().media.take() {
        popups::close_slot(cx, slot);
    }
}

/// 在屏幕顶部居中打开媒体弹窗并武装点击-away 钩子
fn open(cx: &mut App) {
    let Some(monitor) = tozo_platform::primary_monitor() else {
        return;
    };
    let scale = monitor.scale;
    let mon_w = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let x = ((mon_w - POPUP_WIDTH as f64) / 2.).max(0.);
    let y = monitor.rect.top as f64 / scale + BAR_HEIGHT + 4.;

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x as f32), px(y as f32)),
            size: size(px(POPUP_WIDTH), px(POPUP_HEIGHT)),
        })),
        titlebar: Some(TitlebarOptions {
            title: Some("tozo-media".into()),
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
            let media = cx.global::<crate::MediaGlobal>().0.clone();
            cx.new(|cx| MediaPopup::new(media, cx))
        })
        .expect("failed to open media popup")
        .into();

    popups::arm_click_away(cx, "tozo-media", move |cx, hwnd| {
        cx.global_mut::<Popups>().media = Some(PopupSlot { window, hwnd });
    });
}

pub struct MediaPopup {
    media: Entity<MediaStore>,
    decoded: Option<(u64, Arc<RenderImage>)>,
}

impl MediaPopup {
    /// 持有媒体数据实体并订阅其变更，本地缓存解码后的封面
    fn new(media: Entity<MediaStore>, cx: &mut Context<Self>) -> Self {
        cx.observe(&media, |_, _, cx| cx.notify()).detach();
        Self {
            media,
            decoded: None,
        }
    }
}

impl Render for MediaPopup {
    /// 渲染封面、标题/歌手、进度条与播放控制按钮
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let info = self.media.read(cx).info.clone();
        let title = if info.title.is_empty() {
            info.subtitle.clone()
        } else {
            info.title.clone()
        };
        let artist = info.artist.clone();
        let playing = info.playing;
        let position_ms = info.position_ms;
        let end_ms = info.end_ms;
        let artwork = info.artwork.as_ref().and_then(|art| match &self.decoded {
            Some((id, img)) if *id == art.id => Some(img.clone()),
            _ => {
                let img = crate::sources::media::decode_artwork(art);
                if let Some(img) = &img {
                    self.decoded = Some((art.id, img.clone()));
                }
                img
            }
        });

        let ratio = if end_ms > 0 {
            (position_ms as f32 / end_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let toggle_icon = if playing {
            IconName::Pause
        } else {
            IconName::Play
        };

        let has_danmu_source = info.smtc_metadata.is_some();
        let danmu_on = crate::sources::danmu::danmu_enabled();

        // 弹幕开关：开/关 overlay 窗口必须 defer
        let danmu_toggle = has_danmu_source.then(|| {
            div()
                .id("danmu-toggle")
                .flex()
                .items_center()
                .px_1()
                .rounded(px(6.))
                .hover(|style| style.bg(rgba(0xFFFFFF28)))
                .when(danmu_on, |el| el.bg(rgba(0xFFFFFF33)))
                .child(
                    div()
                        .text_size(px(16.))
                        .text_color(rgba(0xFFFFFFEB))
                        .child(IconName::Captions),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| {
                        cx.defer(crate::danmu::toggle);
                        cx.notify();
                    }),
                )
        });

        div()
            .id("media-popup")
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .p(px(10.))
            .rounded(px(14.))
            .bg(rgba(0x18181DEB))
            .font_family("Microsoft YaHei")
            .when_some(artwork, |el, art| {
                el.child(
                    img(ImageSource::Render(art))
                        .size(px(96.))
                        .rounded(px(12.))
                        .mr(px(10.)),
                )
            })
            .child(
                div()
                    .id("media-popup-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("media-title")
                            .text_size(px(14.))
                            .text_color(rgba(0xFFFFFFFF))
                            .truncate()
                            .child(title),
                    )
                    .when(!artist.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgba(0xFFFFFFA0))
                                .truncate()
                                .child(artist),
                        )
                    })
                    .when(end_ms > 0, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h(px(8.))
                                .items_center()
                                .child(seek_track(ratio, end_ms)),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .items_center()
                            .gap_1()
                            .children(danmu_toggle)
                            .child(ctl_button(
                                "prev",
                                IconName::SkipBack,
                                MediaCommand::Previous,
                            ))
                            .child(ctl_button(
                                "toggle",
                                toggle_icon,
                                MediaCommand::TogglePlayPause,
                            ))
                            .child(ctl_button(
                                "next",
                                IconName::SkipForward,
                                MediaCommand::Next,
                            )),
                    ),
            )
            .with_animation(
                "media-popup-in",
                Animation::new(Duration::from_millis(160)).with_easing(quadratic),
                |el, delta| el.opacity(delta),
            )
    }
}

/// 进度条：点击按 x 坐标换算比例并发起 Seek
fn seek_track(ratio: f32, end_ms: u64) -> impl IntoElement {
    div()
        .id("seek-track")
        .w(px(SEEK_TRACK_W))
        .h(px(6.))
        .rounded_full()
        .bg(rgba(0xFFFFFF3C))
        .overflow_hidden()
        .child(
            div()
                .id("seek-fill")
                .w(relative(ratio))
                .h_full()
                .rounded_full()
                .bg(rgba(0xFFFFFFEB)),
        )
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, _| {
            let ratio_f =
                ((event.position.x.as_f32() - SEEK_TRACK_X) / SEEK_TRACK_W).clamp(0.0, 1.0);
            media::submit(MediaCommand::Seek((ratio_f * end_ms as f32) as u64));
        })
}

/// 播放控制按钮：点击提交对应媒体命令
fn ctl_button(id: &'static str, icon: IconName, command: MediaCommand) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .px_1()
        .rounded(px(6.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .child(
            div()
                .text_size(px(16.))
                .text_color(rgba(0xFFFFFFEB))
                .child(icon),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, _| media::submit(command))
}
