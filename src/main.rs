#![cfg_attr(feature = "gui", windows_subsystem = "windows")]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::sources::audio::AudioStore;
use crate::sources::clock::ClockStore;
use crate::sources::media::MediaStore;
use crate::sources::net::NetStore;
use crate::sources::ports::PortStore;
use crate::sources::sysmon::SysStore;
use crate::sources::traypanel::TrayPanel;

mod audio_popup;
mod calendar;
mod media_popup;
mod menu;
mod popups;
mod ports_popup;
mod sources;
mod sys_popup;
mod tray;
mod tray_popup;
mod win;
pub struct MediaGlobal(pub Entity<MediaStore>);
impl Global for MediaGlobal {}

pub struct AudioGlobal(pub Entity<AudioStore>);
impl Global for AudioGlobal {}

pub struct PortsGlobal(pub Entity<PortStore>);
impl Global for PortsGlobal {}

pub static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

const BAR_HEIGHT_LOGICAL: f64 = 30.0;

/// 入口：解析主显示器几何，创建透明 bar 窗口并挂载各岛数据源
fn main() {
    init_logging();

    let monitor = tozo_platform::primary_monitor().expect("primary monitor");
    let scale = monitor.scale;
    let width_logical = (monitor.rect.right - monitor.rect.left) as f64 / scale;
    let origin_y = monitor.rect.top as f64 / scale;

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            popups::init(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(origin_y as f32)),
                    size: size(px(width_logical as f32), px(BAR_HEIGHT_LOGICAL as f32)),
                })),
                titlebar: Some(TitlebarOptions {
                    title: Some("tozo-gpui".into()),
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

            let _bar_window = cx
                .open_window(options, |_, cx| {
                    let media = cx.new(|cx| MediaStore::new(cx));
                    cx.set_global(MediaGlobal(media.clone()));
                    let audio = cx.new(|cx| AudioStore::new(cx));
                    cx.set_global(AudioGlobal(audio.clone()));
                    let ports = cx.new(|cx| PortStore::new(cx));
                    cx.set_global(PortsGlobal(ports.clone()));
                    cx.new(|cx| {
                        let tray = cx.new(|cx| TrayPanel::new(cx));
                        Bar {
                            tray,
                            media,
                            clock: cx.new(|cx| ClockStore::new(cx)),
                            sys: cx.new(|cx| SysStore::new(cx)),
                            net: cx.new(|cx| NetStore::new(cx)),
                            audio,
                            ports,
                        }
                    })
                })
                .expect("failed to open bar window");

            let pid = std::process::id();
            cx.spawn(async move |cx| {
                register_appbar(cx, pid, monitor, scale).await;
            })
            .detach();
        });
}

/// 初始化 tracing 日志：文件层按日滚动写 log/ 目录（保留 7 份，参照 freya 版 logging.rs）；
/// 控制台构建或 gui 从终端启动时另加 stderr 回显；panic 钩子重定向到日志
fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    match (console_attached(), file_appender()) {
        (true, Some((appender, dir))) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_timer(LocalTimer)
                        .with_writer(std::io::stderr),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(false)
                        .with_timer(LocalTimer)
                        .with_writer(appender),
                )
                .init();
            tracing::info!("[log] dir: {}", dir.display());
        }
        (true, None) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_timer(LocalTimer)
                        .with_writer(std::io::stderr),
                )
                .init();
            tracing::error!("[log] file appender unavailable, console only");
        }
        (false, Some((appender, dir))) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(false)
                        .with_timer(LocalTimer)
                        .with_writer(appender),
                )
                .init();
            tracing::info!("[log] dir: {}", dir.display());
        }
        (false, None) => {}
    }
}

/// gui 构建尝试附加父进程控制台；控制台构建恒为 true
#[cfg(feature = "gui")]
fn console_attached() -> bool {
    win::attach_parent_console()
}

#[cfg(not(feature = "gui"))]
fn console_attached() -> bool {
    true
}

/// 构造 log/ 目录下的按日滚动 appender：TOZO_LOG_DIR 优先，其次 exe 目录，失败退回 temp
fn file_appender() -> Option<(tracing_appender::rolling::RollingFileAppender, std::path::PathBuf)> {
    use tracing_appender::rolling::{Builder, Rotation};

    fn build(dir: &std::path::Path) -> Option<tracing_appender::rolling::RollingFileAppender> {
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix("tozo-gpui")
            .filename_suffix("log")
            .max_log_files(7)
            .build(dir)
            .ok()
    }

    if let Some(dir) = std::env::var_os("TOZO_LOG_DIR") {
        let dir = std::path::PathBuf::from(dir);
        if let Some(appender) = build(&dir) {
            return Some((appender, dir));
        }
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| dir.join("log"));
    if let Some(dir) = exe_dir {
        if let Some(appender) = build(&dir) {
            return Some((appender, dir));
        }
    }
    let dir = std::env::temp_dir().join("tozo-gpui").join("log");
    build(&dir).map(|appender| (appender, dir))
}

/// 日志时间戳用本地时区（chrono 实现，参照 freya 版）
struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

/// 等待 bar 窗口 HWND 就绪后注册 appbar 并申请屏幕顶部占位
async fn register_appbar(cx: &mut AsyncApp, pid: u32, monitor: tozo_platform::Monitor, scale: f64) {
    if std::env::var_os("TOZO_NO_APPBAR").is_some() {
        tracing::info!("[spike] TOZO_NO_APPBAR set, skipping appbar");
        return;
    }
    let mut hwnd_value = None;
    for _ in 0..20 {
        if let Some(hwnd) = win::find_hwnd(pid, Some("tozo-gpui"), false) {
            hwnd_value = Some(hwnd);
            break;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    let Some(hwnd_value) = hwnd_value else {
        tracing::error!("[spike] main hwnd not found");
        return;
    };
    MAIN_HWND.store(hwnd_value, Ordering::SeqCst);
    let _ = cx.update(|_cx: &mut App| {
        tozo_platform::appbar::patch_tray_spy_appbar();
        let hwnd = tozo_platform::hwnd(hwnd_value);
        let _ = tozo_platform::apply_backdrop(hwnd, tozo_platform::Backdrop::None);
        let accent_reset = crate::win::accent_disable(hwnd);
        tracing::info!("[spike] accent_disable={accent_reset}");
        match tozo_platform::appbar::register(hwnd) {
            Ok(()) => {
                let height = (BAR_HEIGHT_LOGICAL * scale).round() as i32;
                let approved = tozo_platform::appbar::set_top_position(hwnd, &monitor.rect, height);
                tracing::info!(
                    "[spike] appbar approved=({},{},{},{}) window_rect={:?}",
                    approved.left,
                    approved.top,
                    approved.right,
                    approved.bottom,
                    tozo_platform::window_rect(hwnd)
                );
            }
            Err(err) => tracing::error!("[spike] appbar register failed: {err:?}"),
        }
    });
}

/// 退出前注销 appbar 与菜单，再结束应用
pub fn quit(cx: &mut App) {
    let hwnd_value = MAIN_HWND.load(Ordering::SeqCst);
    if hwnd_value != 0 {
        tozo_platform::appbar::remove(tozo_platform::hwnd(hwnd_value));
    }
    menu::close_menu(cx);
    cx.quit();
}

struct Bar {
    tray: Entity<TrayPanel>,
    media: Entity<MediaStore>,
    clock: Entity<ClockStore>,
    sys: Entity<SysStore>,
    net: Entity<NetStore>,
    audio: Entity<AudioStore>,
    ports: Entity<PortStore>,
}

impl Render for Bar {
    /// 渲染任务栏：左托盘、中媒体、右侧各状态岛，右键弹菜单
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("bar")
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .font_family("Microsoft YaHei")
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, _, cx| menu::open(cx)),
            )
            .child(self.tray.clone())
            .child(div().flex_1())
            .child(self.media.clone())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .pr_3()
                    .child(self.ports.clone())
                    .child(self.sys.clone())
                    .child(self.net.clone())
                    .child(self.audio.clone())
                    .child(self.clock.clone()),
            )
    }
}
