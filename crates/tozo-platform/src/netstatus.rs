use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use tokio::sync::watch;
use windows::Networking::Connectivity::{
    NetworkConnectivityLevel, NetworkInformation, NetworkStatusChangedEventHandler,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Coalesce event bursts; the connection profile can lag the event briefly.
const DEBOUNCE: Duration = Duration::from_millis(150);

static NET_RX: OnceLock<watch::Receiver<bool>> = OnceLock::new();

/// 网络连通性流：后台线程订阅变更事件，去抖后推送
/// Whether the machine currently has internet connectivity, refreshed by
/// network change events instead of polling.
pub fn net_stream() -> watch::Receiver<bool> {
    NET_RX
        .get_or_init(|| {
            let (tx, rx) = watch::channel(true);
            let (signal_tx, signal_rx) = mpsc::channel::<()>();
            let _ = thread::Builder::new()
                .name("tozo-net".to_string())
                .spawn(move || {
                    unsafe {
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    }
                    let handler = NetworkStatusChangedEventHandler::new(move |_| {
                        let _ = signal_tx.send(());
                        Ok(())
                    });
                    if let Err(e) = NetworkInformation::NetworkStatusChanged(&handler) {
                        tracing::error!("[netstatus] subscription failed: {e:?}");
                        return;
                    }
                    publish(&tx, check_online());
                    loop {
                        if signal_rx.recv().is_err() {
                            return;
                        }
                        thread::sleep(DEBOUNCE);
                        while signal_rx.try_recv().is_ok() {}
                        publish(&tx, check_online());
                    }
                });
            rx
        })
        .clone()
}

/// 查询当前是否接入互联网
fn check_online() -> bool {
    NetworkInformation::GetInternetConnectionProfile()
        .ok()
        .and_then(|profile| profile.GetNetworkConnectivityLevel().ok())
        .is_some_and(|level| level == NetworkConnectivityLevel::InternetAccess)
}

/// 值变化时才写入 watch 通道
fn publish(tx: &watch::Sender<bool>, value: bool) {
    let _ = tx.send_if_modified(|current| {
        if *current == value {
            false
        } else {
            *current = value;
            true
        }
    });
}
