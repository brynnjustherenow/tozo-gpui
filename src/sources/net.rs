use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use windows::Networking::Connectivity::{
    NetworkConnectivityLevel, NetworkInformation, NetworkStatusChangedEventHandler,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::clock::pill;

/// Coalesce event bursts; the connection profile can lag the event briefly.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct NetStore {
    pub online: bool,
}

impl NetStore {
    /// 起 MTA 线程订阅 NetworkStatusChanged 事件，防抖后把在线状态转发给前台实体
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<bool>();
        thread::Builder::new()
            .name("tozo-net".to_string())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let (signal_tx, signal_rx) = mpsc::channel::<()>();
                let handler = NetworkStatusChangedEventHandler::new(move |_| {
                    let _ = signal_tx.send(());
                    Ok(())
                });
                if let Err(err) = NetworkInformation::NetworkStatusChanged(&handler) {
                    tracing::error!("[net] subscription failed: {err:?}");
                    return;
                }
                let _ = tx.unbounded_send(check_online());
                loop {
                    if signal_rx.recv().is_err() {
                        return;
                    }
                    thread::sleep(DEBOUNCE);
                    while signal_rx.try_recv().is_ok() {}
                    let _ = tx.unbounded_send(check_online());
                }
            })
            .expect("failed to spawn tozo-net thread");

        cx.spawn(async move |this, cx| {
            while let Some(online) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if this.online != online {
                        this.online = online;
                        cx.notify();
                    }
                });
            }
        })
        .detach();

        Self { online: true }
    }
}

impl Render for NetStore {
    /// 渲染网络岛，按在线状态切换 wifi 图标与颜色
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, color) = if self.online {
            (IconName::Wifi, rgba(0xFFFFFFEB))
        } else {
            (IconName::WifiOff, rgba(0xFFFFFF6E))
        };
        pill().child(
            div()
                .flex()
                .items_center()
                .text_size(px(14.))
                .text_color(color)
                .child(icon),
        )
    }
}

/// 当前连接配置是否具备 InternetAccess 级别
fn check_online() -> bool {
    NetworkInformation::GetInternetConnectionProfile()
        .ok()
        .and_then(|profile| profile.GetNetworkConnectivityLevel().ok())
        .is_some_and(|level| level == NetworkConnectivityLevel::InternetAccess)
}
