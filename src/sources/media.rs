use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui_kit::prelude::*;
use gpui_kit::*;
use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};
use windows::Media::Control::{    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
    TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStream};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MediaArtwork {
    pub id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub subtitle: String,
    pub artwork: Option<MediaArtwork>,
    pub position_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    Next,
    Previous,
    TogglePlayPause,
    Seek(u64),
}

const SOURCE_MARK: char = '\u{2060}';
const FALLBACK: Duration = Duration::from_secs(5);
const MANAGER_RETRY: Duration = Duration::from_secs(5);
const IO_DEADLINE: Duration = Duration::from_millis(800);

static COMMAND_TX: OnceLock<mpsc::Sender<MediaCommand>> = OnceLock::new();
static MEDIA_CLOSED_AT: AtomicI64 = AtomicI64::new(0);

/// 发送媒体控制命令到 worker 线程
pub fn submit(command: MediaCommand) {
    if let Some(tx) = COMMAND_TX.get() {
        let _ = tx.send(command);
    }
}

/// 取当前 Unix 时间戳（毫秒）
fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 媒体弹窗是否在 400ms 内刚关闭
pub fn media_closed_recently() -> bool {
    unix_millis() - MEDIA_CLOSED_AT.load(Ordering::SeqCst) < 400
}

/// 记录媒体弹窗关闭时刻
pub fn note_media_closed() {
    MEDIA_CLOSED_AT.store(unix_millis(), Ordering::SeqCst);
}

/// Await a WinRT async operation via its completion handler. The handler
/// fires on a WinRT threadpool thread and wakes this thread through the
/// channel; no polling, bounded by `IO_DEADLINE`.
fn await_op<TResult: windows::core::RuntimeType + 'static>(
    op: &IAsyncOperation<TResult>,
) -> Option<TResult> {
    let (tx, rx) = mpsc::channel::<()>();
    let handler = AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = tx.send(());
        Ok(())
    });
    if op.SetCompleted(&handler).is_err() {
        return None;
    }
    if rx.recv_timeout(IO_DEADLINE).is_err() {
        return None;
    }
    op.GetResults().ok()
}

/// SMTC objects are agile (free-threaded); shared with the WinRT threadpool
/// threads that invoke event handlers. The windows crate does not encode
/// agility.
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}

struct MediaWatcher {
    manager: Agile<Manager>,
    session: Option<(Agile<Session>, String, [i64; 3])>,
    artwork_cache: Option<(String, String, MediaArtwork)>,
    last: MediaInfo,
}

/// 去掉标题末尾拼接的来源标记
fn split_source(raw: &str) -> &str {
    match raw.split_once(SOURCE_MARK) {
        Some((clean, _)) => clean,
        None => raw,
    }
}

type InfoTx = futures::channel::mpsc::UnboundedSender<MediaInfo>;

/// 媒体 worker 线程：订阅 SMTC，处理控制命令，每 5s 兜底刷新会话
fn media_worker(tx: InfoTx, cmd_rx: mpsc::Receiver<MediaCommand>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let manager = loop {
        match Manager::RequestAsync() {
            Ok(op) => match op.join() {
                Ok(manager) => break manager,
                Err(e) => {
                    tracing::error!("[media] manager join failed: {e:#?}");
                    thread::sleep(MANAGER_RETRY);
                }
            },
            Err(e) => {
                tracing::error!("[media] manager request failed: {e:#?}");
                thread::sleep(MANAGER_RETRY);
            }
        }
    };
    let shared = Arc::new(Mutex::new(MediaWatcher {
        manager: Agile(manager.clone()),
        session: None,
        artwork_cache: None,
        last: MediaInfo::default(),
    }));
    let handler = {
        let shared = shared.clone();
        let tx = tx.clone();
        TypedEventHandler::<Manager, SessionsChangedEventArgs>::new(move |_, _| {
            refresh_sessions(&shared, &tx);
            Ok(())
        })
    };
    if let Err(e) = manager.SessionsChanged(&handler) {
        tracing::error!("[media] sessions subscription failed: {e:?}");
    }
    refresh_sessions(&shared, &tx);
    loop {
        match cmd_rx.recv_timeout(FALLBACK) {
            Ok(command) => run_command(&shared, command),
            Err(mpsc::RecvTimeoutError::Timeout) => refresh_sessions(&shared, &tx),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// 重选当前会话，必要时切换订阅，再全量重算状态
fn refresh_sessions(shared: &Arc<Mutex<MediaWatcher>>, tx: &InfoTx) {
    let manager = shared.lock().unwrap().manager.0.clone();
    let picked = pick_session(&manager).map(|session| {
        let id = session
            .SourceAppUserModelId()
            .unwrap_or_default()
            .to_string();
        (session, id)
    });
    {
        let mut watcher = shared.lock().unwrap();
        let same = match (&watcher.session, &picked) {
            (Some((_, old, _)), Some((_, new))) => old == new,
            (None, None) => true,
            _ => false,
        };
        if !same {
            if let Some((session, _, tokens)) = watcher.session.take() {
                let _ = session.0.RemoveMediaPropertiesChanged(tokens[0]);
                let _ = session.0.RemovePlaybackInfoChanged(tokens[1]);
                let _ = session.0.RemoveTimelinePropertiesChanged(tokens[2]);
            }
            if let Some((session, id)) = picked {
                let tokens = subscribe_session(shared, tx, &session);
                watcher.session = Some((Agile(session), id, tokens));
            }
        }
    }
    recompute(shared, tx, Scope::Full);
}

/// 订阅会话的元数据、播放、时间线变更事件，返回令牌
fn subscribe_session(
    shared: &Arc<Mutex<MediaWatcher>>,
    tx: &InfoTx,
    session: &Session,
) -> [i64; 3] {
    let props = {
        let shared = shared.clone();
        let tx = tx.clone();
        TypedEventHandler::<Session, MediaPropertiesChangedEventArgs>::new(move |_, _| {
            recompute(&shared, &tx, Scope::Meta);
            Ok(())
        })
    };
    let playback = {
        let shared = shared.clone();
        let tx = tx.clone();
        TypedEventHandler::<Session, PlaybackInfoChangedEventArgs>::new(move |_, _| {
            recompute(&shared, &tx, Scope::State);
            Ok(())
        })
    };
    let timeline = {
        let shared = shared.clone();
        let tx = tx.clone();
        TypedEventHandler::<Session, TimelinePropertiesChangedEventArgs>::new(move |_, _| {
            recompute(&shared, &tx, Scope::State);
            Ok(())
        })
    };
    [
        session.MediaPropertiesChanged(&props).unwrap_or(0),
        session.PlaybackInfoChanged(&playback).unwrap_or(0),
        session.TimelinePropertiesChanged(&timeline).unwrap_or(0),
    ]
}

/// 优先选正在播放的会话，否则回退当前会话
fn pick_session(manager: &Manager) -> Option<Session> {
    manager
        .GetSessions()
        .ok()
        .and_then(|sessions| sessions.into_iter().find(is_playing))
        .or_else(|| manager.GetCurrentSession().ok())
}

/// 按 scope 重算对应字段，有变化才推送 UI
fn recompute(shared: &Arc<Mutex<MediaWatcher>>, tx: &InfoTx, scope: Scope) {
    let (manager, last) = {
        let watcher = shared.lock().unwrap();
        (watcher.manager.0.clone(), watcher.last.clone())
    };
    let mut info = last;
    if let Some(session) = pick_session(&manager) {
        match scope {
            Scope::Meta => {
                if let Some(meta) = poll_meta(shared, &session, true) {
                    info.title = meta.title;
                    info.artist = meta.artist;
                    info.subtitle = meta.subtitle;
                    info.artwork = meta.artwork;
                }
            }
            Scope::State => {
                if let Some(state) = poll_state(&session) {
                    info.playing = state.playing;
                    info.position_ms = state.position_ms;
                    info.end_ms = state.end_ms;
                }
            }
            Scope::Full => {
                if let Some(fresh) = poll_session(shared, &session) {
                    info = fresh;
                }
            }
        }
    }
    let mut watcher = shared.lock().unwrap();
    if watcher.last != info {
        watcher.last = info.clone();
        let _ = tx.unbounded_send(info);
    }
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Meta,
    State,
    Full,
}

struct MediaMeta {
    title: String,
    artist: String,
    subtitle: String,
    artwork: Option<MediaArtwork>,
}

struct MediaState {
    playing: bool,
    position_ms: u64,
    end_ms: u64,
}

/// 读取会话标题与艺人等元数据，封面可复用缓存
fn poll_meta(
    shared: &Arc<Mutex<MediaWatcher>>,
    session: &Session,
    fresh_artwork: bool,
) -> Option<MediaMeta> {
    let op = session.TryGetMediaPropertiesAsync().ok()?;
    let props = await_op(&op)?;
    let raw_title = props.Title().unwrap_or_default().to_string();
    let raw_artist = props.Artist().unwrap_or_default().to_string();
    let title = split_source(&raw_title).to_string();
    let artist = split_source(&raw_artist).to_string();
    let subtitle = props.Subtitle().unwrap_or_default().to_string();
    let cached = if fresh_artwork {
        None
    } else {
        let watcher = shared.lock().unwrap();
        watcher
            .artwork_cache
            .as_ref()
            .filter(|(cached_title, cached_artist, _)| {
                (cached_title, cached_artist) == (&title, &artist)
            })
            .map(|(_, _, art)| art.clone())
    };
    let artwork = match cached {
        Some(art) => Some(art),
        None => {
            let fresh = read_artwork(&props);
            if let Some(art) = fresh.clone() {
                shared.lock().unwrap().artwork_cache = Some((title.clone(), artist.clone(), art));
            }
            fresh
        }
    };
    Some(MediaMeta {
        title,
        artist,
        subtitle,
        artwork,
    })
}

/// 读取播放状态与时间线位置、结束时间
fn poll_state(session: &Session) -> Option<MediaState> {
    let (position_ms, end_ms) = session
        .GetTimelineProperties()
        .map(|tl| {
            let position = tl.Position().map(|t| t.Duration).unwrap_or(0).max(0);
            let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0).max(0);
            (position as u64 / 10_000, end as u64 / 10_000)
        })
        .unwrap_or((0, 0));
    Some(MediaState {
        playing: is_playing(session),
        position_ms,
        end_ms,
    })
}

/// 一次性读取完整 MediaInfo
fn poll_session(shared: &Arc<Mutex<MediaWatcher>>, session: &Session) -> Option<MediaInfo> {
    let meta = poll_meta(shared, session, false)?;
    let state = poll_state(session)?;
    Some(MediaInfo {
        title: meta.title,
        artist: meta.artist,
        playing: state.playing,
        artwork: meta.artwork,
        subtitle: meta.subtitle,
        position_ms: state.position_ms,
        end_ms: state.end_ms,
    })
}

/// 在当前会话上执行控制命令
fn run_command(shared: &Arc<Mutex<MediaWatcher>>, command: MediaCommand) {
    let manager = shared.lock().unwrap().manager.0.clone();
    let Some(session) = pick_session(&manager) else {
        return;
    };
    let op = match command {
        MediaCommand::Next => session.TrySkipNextAsync(),
        MediaCommand::Previous => session.TrySkipPreviousAsync(),
        MediaCommand::TogglePlayPause => session.TryTogglePlayPauseAsync(),
        MediaCommand::Seek(ms) => session.TryChangePlaybackPositionAsync(ms as i64 * 10_000),
    };
    if let Ok(op) = op {
        let _ = op.join();
    }
}

/// 会话播放状态是否为正在播放
fn is_playing(session: &Session) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|status| status == PlaybackStatus::Playing)
}

/// 读取缩略图流为字节（上限 4MB），哈希作 id
fn read_artwork(
    props: &GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<MediaArtwork> {
    let thumb = props.Thumbnail().ok()?;
    let op = thumb.OpenReadAsync().ok()?;
    let stream = await_op(&op)?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 4 * 1024 * 1024 {
        return None;
    }
    let raw_stream = stream.cast::<IRandomAccessStream>().ok()?;
    let reader = DataReader::CreateDataReader(&raw_stream).ok()?;
    let op = reader.LoadAsync(size).ok()?;
    let loaded = await_op(&op)?;
    let mut bytes = vec![0u8; size as usize];
    let read_ok = (loaded as usize >= bytes.len())
        .then(|| reader.ReadBytes(&mut bytes))
        .is_some();
    let _ = reader.Close();
    if !read_ok || bytes.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash_slice(&bytes, &mut hasher);
    Some(MediaArtwork {
        id: std::hash::Hasher::finish(&mut hasher),
        bytes,
    })
}

const MAX_ISLAND_WIDTH: f32 = 300.;
const ISLAND_TEXT_WIDTH: f32 = 200.;
const ISLAND_TEXT_WIDTH_EXPANDED: f32 = 400.;

/// 按字符估算文本像素宽度，中日韩算全宽
fn est_text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|c| {
            if c.is_whitespace() {
                0.3
            } else if (c as u32) >= 0x2E80 {
                1.
            } else {
                0.55
            }
        })
        .sum::<f32>()
        * size
}

pub struct MediaStore {
    pub info: MediaInfo,
    content: MediaInfo,
    hovering: bool,
    dying: bool,
    phase: f64,
    artwork_img: Option<(u64, Arc<RenderImage>)>,
}

/// 选岛显示文本：有副标题用副标题，否则标题加艺人
fn island_text(info: &MediaInfo) -> String {
    match (
        info.title.is_empty(),
        info.artist.is_empty(),
        info.subtitle.is_empty(),
    ) {
        (false, false, true) => format!("{} · {}", info.title, info.artist),
        (false, true, true) => info.title.clone(),
        (true, false, true) => info.artist.clone(),
        (true, true, true) => String::new(),
        (_, _, false) => info.subtitle.clone(),
    }
}

/// 解码封面图并翻转 RGBA 为 BGR 供渲染器使用
pub(crate) fn decode_artwork(art: &MediaArtwork) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(&art.bytes).ok()?;
    let mut rgba = decoded.into_rgba8();
    // The DirectX polychrome atlas is DXGI_FORMAT_B8G8R8A8_UNORM, so the
    // renderer reads RenderImage bytes as BGR; decoded RGBA must be swapped.
    // DirectX 的 polychrome 图集是 B8G8R8A8，渲染器按 BGR 读 RenderImage，
    // 解码出的 RGBA 必须翻转红蓝通道。
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new(smallvec::smallvec![frame])))
}

impl MediaStore {
    /// 启动媒体 worker 线程与 33ms 频谱相位计时
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<MediaInfo>();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let _ = COMMAND_TX.set(cmd_tx);
        thread::Builder::new()
            .name("tozo-media".to_string())
            .spawn(move || media_worker(tx, cmd_rx))
            .expect("failed to spawn tozo-media thread");

        cx.spawn(async move |this, cx| {
            while let Some(info) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.info = info;
                    cx.notify();
                });
            }
        })
        .detach();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.info.playing && this.visible_target() {
                    this.phase += 1.0;
                    cx.notify();
                }
            });
        })
        .detach();

        Self {
            info: MediaInfo::default(),
            content: MediaInfo::default(),
            hovering: false,
            dying: false,
            phase: 0.0,
            artwork_img: None,
        }
    }

    /// 岛是否应显示：有封面或文本
    fn visible_target(&self) -> bool {
        self.info.artwork.is_some() || !island_text(&self.info).is_empty()
    }

    /// 取快照封面的显示图，id 变化才重新解码
    fn content_img(&mut self) -> Option<Arc<RenderImage>> {
        let art = self.content.artwork.as_ref()?;
        match &self.artwork_img {
            Some((id, img)) if *id == art.id => Some(img.clone()),
            _ => {
                let img = decode_artwork(art);
                if let Some(img) = &img {
                    self.artwork_img = Some((art.id, img.clone()));
                }
                img
            }
        }
    }
}

impl Render for MediaStore {
    /// 渲染媒体岛：出入场动画、频谱条、悬停跑马灯
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.visible_target();
        let had_content = self.content.artwork.is_some() || !island_text(&self.content).is_empty();
        if target {
            self.content = self.info.clone();
            self.dying = false;
        } else if had_content && !self.dying {
            self.dying = true;
        }
        if !target && !self.dying {
            return div().id("media-empty").size_0().into_any_element();
        }

        let art_img = self.content_img();
        let has_art = art_img.is_some();
        let playing = self.content.playing;
        let text = island_text(&self.content);
        let text_target = est_text_width(&text, 12.)
            .clamp(ISLAND_TEXT_WIDTH, ISLAND_TEXT_WIDTH_EXPANDED);
        let text_overflow = (est_text_width(&text, 12.) - text_target).max(0.);
        let phase = self.phase;
        let hovering = self.hovering;

        div()
            .id("media-island")
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.defer(|cx| crate::media_popup::toggle(cx));
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.hovering != *hovered {
                    this.hovering = *hovered;
                    cx.notify();
                }
            }))
            .with_animation(
                ("presence", target as usize),
                Animation::new(Duration::from_millis(220)).with_easing(ease_in_out),
                move |el, delta| {
                    let p = if target { delta } else { 1.0 - delta };
                    let mut el = el
                        .max_w(px(
                            MAX_ISLAND_WIDTH * p
                                + if hovering {
                                    text_target - ISLAND_TEXT_WIDTH
                                } else {
                                    0.
                                },
                        ))
                        .flex()
                        .flex_row()
                        .items_center()
                        .opacity(p)
                        .rounded_full()
                        .bg(rgba(0x1C1C20A0))
                        .p(px(2.));
                    if let Some(art_img) = art_img.as_ref() {
                        el = el.child(
                            img(ImageSource::Render(art_img.clone()))
                                .size(px(22.))
                                .rounded(px(6.))
                                .ml(px(4.)),
                        );
                    }
                    if has_art {
                        el = el.child(
                            div().with_animation(
                                ("spec-presence", playing as usize),
                                Animation::new(Duration::from_millis(150))
                                    .with_easing(ease_in_out),
                                move |el, delta| {
                                    let q = if playing { delta } else { 1.0 - delta };
                                    el.opacity(q).child(spectrum_bars(phase, p * q))
                                },
                            ),
                        );
                    }
                    if !text.is_empty() {
                        let text = text.clone();
                        let marquee = hovering && text_overflow > 4.;
                        let marquee_pad = 12.;
                        let marquee_travel = text_overflow + 2. * marquee_pad;
                        let travel_ms = (marquee_travel / 25.) * 1000.;
                        let cycle_ms = (2. * travel_ms + 2. * 1000.).max(2400.) as u64;
                        let dwell_frac = 1000. / cycle_ms as f32;
                        el = el.child(
                            div()
                                .max_w(px(text_target))
                                .overflow_hidden()
                                .ml(px(8.))
                                .mr(px(10.))
                                .with_animation(
                                    ("hover", hovering as usize),
                                    Animation::new(Duration::from_millis(150))
                                        .with_easing(ease_in_out),
                                    move |el, delta| {
                                        let t = if hovering {
                                            ISLAND_TEXT_WIDTH
                                                + (text_target - ISLAND_TEXT_WIDTH) * delta
                                        } else {
                                            text_target
                                                - (text_target - ISLAND_TEXT_WIDTH) * delta
                                        };
                                        el.max_w(px(t))
                                    },
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(rgba(0xFFFFFFEB))
                                        .when(!marquee, |el| el.truncate().child(text.clone()))
                                        .when(marquee, move |el| {
                                            el.whitespace_nowrap().child(
                                                div()
                                                    .with_animation(
                                                        "marquee",
                                                        Animation::new(
                                                            Duration::from_millis(cycle_ms),
                                                        )
                                                        .repeat()
                                                        .with_easing(move |t| {
                                                            let d = dwell_frac;
                                                            let leg = 0.5 - d;
                                                            if t < d {
                                                                0.
                                                            } else if t < 0.5 {
                                                                (t - d) / leg
                                                            } else if t < 0.5 + d {
                                                                1.
                                                            } else {
                                                                1. - (t - 0.5 - d) / leg
                                                            }
                                                        }),
                                                        move |el, delta| {
                                                            el.ml(px(
                                                                marquee_pad
                                                                    - marquee_travel * delta,
                                                            ))
                                                        },
                                                    )
                                                    .child(text.clone()),
                                            )
                                        }),
                                ),
                        );
                    }
                    el
                },
            )
            .into_any_element()
    }
}

/// 用正弦模拟构建 4 根频谱条
fn spectrum_bars(phase: f64, presence: f32) -> impl IntoElement {
    div()
        .id("spectrum")
        .flex()
        .flex_row()
        .items_center()
        .ml(px(4. * presence))
        .children((0..4usize).map(|i| {
            let x = i as f64;
            let wave = 0.6 * (phase * (0.21 + 0.07 * x) + x * 1.7).sin()
                + 0.4 * (phase * (0.47 + 0.11 * x) + x * 0.8).sin();
            let height = (3. + 10. * (0.5 + 0.5 * wave)) * presence as f64;
            div()
                .id(("bar", i))
                .w(px(2. * presence))
                .h(px(height as f32))
                .ml(px(1.5 * presence))
                .rounded(px(1.))
                .bg(rgba(0xFFFFFFEB))
        }))
}
