use std::cell::Cell;
use std::rc::Rc;
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
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
    TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStream};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

use gpui_kit::assets::IconName;
use tozo_extra::engines::{try_resolve_path, SmtcSourceMeta};

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
    /// 浏览器扩展注入的来源页信息（bilibili 时 identifier 为 BV 号）
    pub smtc_metadata: Option<SmtcSourceMeta>,
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

/// 拆掉标题/艺人末尾拼接的来源标记，返回（干净文本, 标记后的额外信息）
fn split_source(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once(SOURCE_MARK) {
        Some((clean, extra)) => (clean, Some(extra)),
        None => (raw, None),
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
                    info.smtc_metadata = meta.smtc_metadata;
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
    smtc_metadata: Option<SmtcSourceMeta>,
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
    let (title, title_extra) = split_source(&raw_title);
    let (artist, artist_extra) = split_source(&raw_artist);
    // 扩展把页址注入在艺人标记后，或标题的 "[via …]" 里
    let source_url = artist_extra.map(|url| url.to_string()).or_else(|| {
        title_extra.and_then(|extra| {
            extra
                .split_once("[via ")
                .map(|(_, rest)| rest.trim_end_matches(']').to_string())
        })
    });
    let smtc_metadata = source_url.as_deref().and_then(try_resolve_path);
    let (title, artist) = (title.to_string(), artist.to_string());
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
        smtc_metadata,
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
        smtc_metadata: meta.smtc_metadata,
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
    pub(crate) compact: bool,
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
            compact: false,
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
    /// 渲染媒体岛：出入场动画、频谱条、悬停跑马灯；
    /// compact 分支：无点击弹窗，悬停时蒙版覆盖控件并沿底边弹出进度条
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = self.compact;
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
        let est = est_text_width(&text, 12.);
        let text_target = est.clamp(ISLAND_TEXT_WIDTH, ISLAND_TEXT_WIDTH_EXPANDED);
        // compact 悬停不展开：文本窗口固定在基础宽度，跑马灯按此窗口计算溢出
        let text_window = if compact {
            ISLAND_TEXT_WIDTH
        } else {
            text_target
        };
        let text_overflow = (est - text_window).max(0.);
        let phase = self.phase;
        let hovering = self.hovering;
        let progress = (self.content.end_ms > 0).then(|| {
            let end = self.content.end_ms;
            (
                (self.content.position_ms as f32 / end as f32).clamp(0., 1.),
                end,
            )
        });
        // 进度条轨道的窗口坐标：canvas 每帧刷新、鼠标事件期读取（岛在 bar 内位置动态，无法静态换算）
        let progress_track = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let danmu = self
            .content
            .smtc_metadata
            .is_some()
            .then(crate::sources::danmu::danmu_enabled);

        div()
            .id("media-island")
            .overflow_hidden()
            .when(compact, |el| el.relative())
            .when(!compact, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| {
                        cx.defer(|cx| crate::media_popup::toggle(cx));
                    }),
                )
            })
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
                        .max_w(px(MAX_ISLAND_WIDTH * p
                            + if hovering {
                                text_window - ISLAND_TEXT_WIDTH
                            } else {
                                0.
                            }))
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
                        el = el.child(div().with_animation(
                            ("spec-presence", playing as usize),
                            Animation::new(Duration::from_millis(150)).with_easing(ease_in_out),
                            move |el, delta| {
                                let q = if playing { delta } else { 1.0 - delta };
                                el.opacity(q).child(spectrum_bars(phase, p * q))
                            },
                        ));
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
                                .max_w(px(text_window))
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
                                                + (text_window - ISLAND_TEXT_WIDTH) * delta
                                        } else {
                                            text_window - (text_window - ISLAND_TEXT_WIDTH) * delta
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
                                                        Animation::new(Duration::from_millis(
                                                            cycle_ms,
                                                        ))
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
                                                            el.ml(px(marquee_pad
                                                                - marquee_travel * delta))
                                                        },
                                                    )
                                                    .child(text.clone()),
                                            )
                                        }),
                                ),
                        );
                    }
                    if compact {
                        el = el
                            .child(compact_overlay(hovering, playing, danmu))
                            .when_some(progress, |el, (ratio, end_ms)| {
                                el.child(compact_progress(
                                    hovering,
                                    ratio,
                                    end_ms,
                                    progress_track.clone(),
                                ))
                            });
                    }
                    el
                },
            )
            .into_any_element()
    }
}

/// compact 悬停覆盖层：“中深两浅”钟形蒙版盖全岛——gpui 线性渐变仅支持
/// 两个 stop，用左右两半拼接：各自从岛缘渐隐、在中心汇于峰值
/// （rgba(0x10101499)），看不出蒙版/岛边界；中央横排上一首/播放暂停/
/// 下一首（+弹幕开关），随 hover 淡入淡出，两侧文本保持可读
fn compact_overlay(hovering: bool, playing: bool, danmu: Option<bool>) -> impl IntoElement {
    let toggle = if playing {
        IconName::Pause
    } else {
        IconName::Play
    };
    div()
        .id("compact-overlay")
        .absolute()
        .inset_0()
        .with_animation(
            ("compact-overlay", hovering as usize),
            Animation::new(Duration::from_millis(150)).with_easing(ease_in_out),
            move |el, delta| {
                let q = if hovering { delta } else { 1.0 - delta };
                el.opacity(q)
            },
        )
        .child(
            div()
                .id("compact-scrim-l")
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(0.5))
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(rgba(0x10101400), 0.),
                    linear_color_stop(rgba(0x10101499), 1.),
                )),
        )
        .child(
            div()
                .id("compact-scrim-r")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(relative(0.5))
                .bg(linear_gradient(
                    270.,
                    linear_color_stop(rgba(0x10101400), 0.),
                    linear_color_stop(rgba(0x10101499), 1.),
                )),
        )
        .child(
            div()
                .id("compact-controls")
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .size_full()
                .gap(px(6.))
                .child(compact_ctl(
                    "c-prev",
                    IconName::SkipBack,
                    MediaCommand::Previous,
                ))
                .child(compact_ctl(
                    "c-toggle",
                    toggle,
                    MediaCommand::TogglePlayPause,
                ))
                .child(compact_ctl(
                    "c-next",
                    IconName::SkipForward,
                    MediaCommand::Next,
                ))
                .when_some(danmu, |el, on| el.child(compact_danmu_ctl(on))),
        )
}

/// compact 弹幕开关：仅弹幕源可用时显示，开启时高亮；开/关 overlay 窗口 defer 到帧外
fn compact_danmu_ctl(on: bool) -> impl IntoElement {
    div()
        .id("c-danmu")
        .flex()
        .items_center()
        .p(px(2.))
        .rounded(px(4.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .when(on, |el| el.bg(rgba(0xFFFFFF33)))
        .child(
            div()
                .text_size(px(14.))
                .text_color(rgba(0xFFFFFFEB))
                .child(IconName::Captions),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.defer(|cx| {
                crate::danmu::toggle(cx);
                let media = cx.global::<crate::MediaGlobal>().0.clone();
                media.update(cx, |_, cx| cx.notify());
            });
        })
}

/// compact 播放控件按钮：点击直接提交媒体命令
fn compact_ctl(id: &'static str, icon: IconName, command: MediaCommand) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .p(px(2.))
        .rounded(px(4.))
        .hover(|style| style.bg(rgba(0xFFFFFF28)))
        .child(
            div()
                .text_size(px(14.))
                .text_color(rgba(0xFFFFFFEB))
                .child(icon),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, _| submit(command))
}

/// compact 底边进度条：贴岛底边、左右内缩避开圆角，随 hover 淡入淡出。
/// 命中区加高到 8px，点击/按住拖动按 x 换算 Seek
fn compact_progress(
    hovering: bool,
    ratio: f32,
    end_ms: u64,
    track: Rc<Cell<Option<Bounds<Pixels>>>>,
) -> impl IntoElement {
    let down = track.clone();
    let drag = track.clone();
    div()
        .id("compact-progress")
        .absolute()
        .left(px(6.))
        .right(px(6.))
        .bottom_0()
        .h(px(8.))
        .flex()
        .items_center()
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, _| {
            if let Some(b) = down.get() {
                submit(MediaCommand::Seek(seek_ms(event.position.x, &b, end_ms)));
            }
        })
        .on_mouse_move(move |event: &MouseMoveEvent, _, _| {
            if event.pressed_button == Some(MouseButton::Left) {
                if let Some(b) = drag.get() {
                    submit(MediaCommand::Seek(seek_ms(event.position.x, &b, end_ms)));
                }
            }
        })
        .child(
            div()
                .id("compact-progress-visual")
                .relative()
                .w_full()
                .h(px(3.))
                .rounded_full()
                .bg(rgba(0xFFFFFF3C))
                .with_animation(
                    ("compact-progress", hovering as usize),
                    Animation::new(Duration::from_millis(150)).with_easing(ease_in_out),
                    move |el, delta| {
                        let q = if hovering { delta } else { 1.0 - delta };
                        el.opacity(q)
                    },
                )
                // 轨道实际 x 范围由内嵌 canvas 每帧上报（避开命中区左右 6px 内边距）
                .child(
                    canvas(move |bounds, _, _| track.set(Some(bounds)), |_, _, _, _| {})
                        .absolute()
                        .inset_0(),
                )
                .child(
                    div()
                        .id("compact-progress-fill")
                        .h_full()
                        .rounded_full()
                        .w(relative(ratio))
                        .bg(rgba(0xFFFFFFEB)),
                ),
        )
}

/// 按轨道窗口坐标把鼠标 x 换算成 Seek 毫秒
fn seek_ms(x: Pixels, bounds: &Bounds<Pixels>, end_ms: u64) -> u64 {
    let ratio = ((x.as_f32() - bounds.origin.x.as_f32()) / bounds.size.width.as_f32().max(1.))
        .clamp(0., 1.);
    (ratio * end_ms as f32) as u64
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
