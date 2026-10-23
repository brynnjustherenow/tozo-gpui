//! render all trays in a panel
//!

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use systray_util::{StableId, SystrayEvent, SystrayIcon, SystrayIconAction};
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;

use crate::{sources::clock::pill, tray::holder::run};

pub(crate) const MAX_PANEL_ICONS: usize = 6;

#[derive(Clone)]
struct TrayEntry {
    icon: SystrayIcon,
    img: Option<Arc<RenderImage>>,
    hidden: bool,
}

pub struct TrayGlobal(pub Entity<TraysState>);
impl Global for TrayGlobal {}

/// 把托盘图标图像转为 RenderImage，翻转红蓝通道
fn entry_img(icon: &SystrayIcon) -> Option<Arc<RenderImage>> {
    let mut image = icon.icon_image.clone()?;
    // GetDIBits yields BGR; systray-util swaps it to RGB internally; the
    // DirectX renderer reads RenderImage as BGR (B8G8R8A8 atlas), so swap it
    // back here. Exactly one local flip; the one inside the crate cannot be
    // skipped without forking.
    // GetDIBits 输出 BGR；systray-util 内部翻成 RGB；DirectX 渲染器按 BGR
    // 读 RenderImage（B8G8R8A8 图集），所以这里再翻回去。本地仅此一次翻
    // 转，crate 内部那次不 fork 无法去掉。
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(image);
    Some(Arc::new(RenderImage::new(smallvec::smallvec![frame])))
}

/// 是否系统音量托盘图标（按 tooltip 或 sndtray.exe 判断）
fn is_volume_icon(icon: &SystrayIcon) -> bool {
    let tooltip = icon.tooltip.to_lowercase();
    if ["扬声器", "音量", "volume", "speakers"]
        .iter()
        .any(|prefix| tooltip.starts_with(prefix))
    {
        return true;
    }
    icon.window_handle
        .and_then(window_exe_name)
        .is_some_and(|exe| exe.eq_ignore_ascii_case("sndtray.exe"))
}

/// 由窗口句柄取所属进程的 exe 文件名
fn window_exe_name(hwnd: isize) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 512];
    let mut len = buffer.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len)
    };
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;
    let path = String::from_utf16_lossy(&buffer[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_string)
}

pub struct TrayPanel {
    store: Entity<TraysState>,
    _subscription: Subscription,
}

impl TrayPanel {
    /// 建托盘状态与全局，转发托盘事件到 store
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let store = cx.new(|_| TraysState::new(tx, cmd_rx, cmd_tx));
        cx.set_global(TrayGlobal(store.clone()));
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.store.update(cx, |state, cx| state.apply(event, cx));
                });
            }
        })
        .detach();
        let _subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            _subscription,
        }
    }
}

impl Render for TrayPanel {
    /// 渲染托盘面板，最多显示 6 个图标，溢出显示展开按钮
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (visible, hovered_id) = {
            let state = self.store.read(cx);
            (state.visible_entries(), state.hovered.clone())
        };
        if visible.is_empty() {
            return div().id("tray-empty").size_0().into_any_element();
        }
        let panel_count = visible.len().min(MAX_PANEL_ICONS);
        let has_overflow = visible.len() > MAX_PANEL_ICONS;
        let store = self.store.clone();
        let mut items: Vec<AnyElement> = visible[..panel_count]
            .iter()
            .map(|(idx, id, icon_img)| {
                tray_icon_view(
                    ("tray-icon", *idx),
                    22.,
                    icon_img.clone(),
                    hovered_id.as_ref() == Some(id),
                    id.clone(),
                    store.clone(),
                )
            })
            .collect();
        if has_overflow {
            items.push(
                div()
                    .id("tray-more")
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .hover(|style| style.bg(rgba(0xFFFFFF1A)))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.defer(crate::tray_popup::toggle);
                    }))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgba(0xFFFFFFEB))
                            .child(IconName::ChevronDown),
                    )
                    .into_any_element(),
            );
        }
        pill()
            .px_2()
            .py(px(2.))
            .gap_1()
            .ml_3()
            .children(items)
            .into_any_element()
    }
}

/// 渲染单个托盘图标，处理悬停、左键与右键事件
pub(crate) fn tray_icon_view(
    id: impl Into<ElementId>,
    cell: f32,
    icon_img: Option<Arc<RenderImage>>,
    hovered: bool,
    stable_id: StableId,
    store: Entity<TraysState>,
) -> AnyElement {
    let hover_id = stable_id.clone();
    let click_id = stable_id.clone();
    let right_id = stable_id;
    let hover_store = store.clone();
    let click_store = store.clone();
    let right_store = store;
    div()
        .id(id)
        .size(px(cell))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when(hovered, |el| el.bg(rgba(0xFFFFFF1A)))
        .on_hover(move |is_hovered: &bool, _, cx| {
            hover_store.update(cx, |state, cx| {
                let action = if *is_hovered {
                    SystrayIconAction::HoverEnter
                } else {
                    SystrayIconAction::HoverLeave
                };
                let _ = state.cmd.try_send((hover_id.clone(), action));
                if *is_hovered {
                    if state.hovered.as_ref() != Some(&hover_id) {
                        state.hovered = Some(hover_id.clone());
                        cx.notify();
                    }
                } else if state.hovered.as_ref() == Some(&hover_id) {
                    state.hovered = None;
                    cx.notify();
                }
            });
        })
        .on_click(move |event: &ClickEvent, _, cx| {
            let action = if event.click_count() >= 2 {
                SystrayIconAction::LeftDoubleClick
            } else {
                SystrayIconAction::LeftClick
            };
            let _ = click_store.read(cx).cmd.try_send((click_id.clone(), action));
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            cx.stop_propagation();
            let _ = right_store
                .read(cx)
                .cmd
                .try_send((right_id.clone(), SystrayIconAction::RightClick));
        })
        .when_some(icon_img, |el, icon_img| {
            el.child(img(ImageSource::Render(icon_img)).size(px(16.)))
        })
        .into_any_element()
}

#[derive(Clone)]
pub struct TraysState {
    cancel: CancellationToken,
    cmd: Sender<(StableId, SystrayIconAction)>,
    trays: Vec<TrayEntry>,
    pub(crate) hovered: Option<StableId>,
}

impl Drop for TraysState {
    /// 析构时取消托盘采集任务
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl TraysState {
    /// 初始化全部托盘图标，音量图标默认隐藏
    fn new(
        tx: Sender<SystrayEvent>,
        rx: mpsc::Receiver<(StableId, SystrayIconAction)>,
        cmd: Sender<(StableId, SystrayIconAction)>,
    ) -> Self {
        let (icons, cancel) = match run(tx, rx) {
            Ok(result) => result,
            Err(err) => {
                tracing::error!("[tray] init failed: {err:?}");
                (Vec::new(), CancellationToken::new())
            }
        };
        let trays = icons
            .into_iter()
            .map(|icon| TrayEntry {
                hidden: is_volume_icon(&icon),
                img: entry_img(&icon),
                icon,
            })
            .collect();
        Self {
            cancel,
            cmd,
            trays,
            hovered: None,
        }
    }

    /// 列出未隐藏且可见图标的序号、稳定 id 与图像
    pub(crate) fn visible_entries(&self) -> Vec<(usize, StableId, Option<Arc<RenderImage>>)> {
        self.trays
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.hidden && entry.icon.is_visible)
            .map(|(idx, entry)| (idx, entry.icon.stable_id.clone(), entry.img.clone()))
            .collect()
    }

    /// 应用托盘增删改事件并触发刷新
    fn apply(&mut self, event: SystrayEvent, cx: &mut Context<Self>) {
        match event {
            SystrayEvent::IconAdd(icon) | SystrayEvent::IconUpdate(icon) => self.upsert(icon),
            SystrayEvent::IconRemove(id) => {
                self.trays.retain(|entry| entry.icon.stable_id != id);
                if self.hovered.as_ref() == Some(&id) {
                    self.hovered = None;
                }
            }
        }
        cx.notify();
    }

    /// 插入或更新托盘图标，图像未变时复用
    fn upsert(&mut self, icon: SystrayIcon) {
        let id = icon.stable_id.clone();
        match self.trays.iter_mut().find(|entry| entry.icon.stable_id == id) {
            Some(entry) => {
                let keep =
                    entry.img.is_some() && entry.icon.icon_image_hash == icon.icon_image_hash;
                if !keep {
                    entry.img = entry_img(&icon);
                }
                entry.hidden = is_volume_icon(&icon);
                entry.icon = icon;
            }
            None => self.trays.push(TrayEntry {
                hidden: is_volume_icon(&icon),
                img: entry_img(&icon),
                icon,
            }),
        }
    }
}
