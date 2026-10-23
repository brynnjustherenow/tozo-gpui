use systray_util::{Error, StableId, Systray, SystrayIcon, SystrayIconAction};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 启动托盘事件线程：select 循环转发 systray 事件并接收图标操作指令
pub(crate) fn run(
    tx: mpsc::Sender<systray_util::SystrayEvent>,
    mut rx: mpsc::Receiver<(StableId, SystrayIconAction)>,
) -> Result<(Vec<SystrayIcon>, CancellationToken), Error> {
    let mut sys_tray = Systray::new()?;
    let token = CancellationToken::new();
    let cloned_token = token.clone();
    let icons = sys_tray.icons();
    if let Err(err) = std::thread::Builder::new()
        .name("tozo-tray".to_string())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                tracing::error!("[tray] runtime build failed");
                return;
            };
            runtime.block_on(async move {
                loop {
                    tokio::select! {
                        _ = cloned_token.cancelled() => {
                            break;
                        }
                        e = sys_tray.events() => {
                            if let Some(event) = e {
                                let _ = tx.send(event).await;
                            }
                        }
                        e = rx.recv() => {
                            if let Some((id, action)) = e {
                                let _ = sys_tray.send_action(&id, &action);
                            }
                        }
                    }
                }
            });
        })
    {
        tracing::error!("[tray] thread spawn failed: {err}");
    }
    Ok((icons, token))
}
