//! bilibili 弹幕数据源：跟随 SMTC 播放进度拉取分段弹幕，计算车道布局供 overlay 渲染。
//!
//! - 网络拉取在专用 tokio runtime（2 线程）跑 reqwest，结果经 std mpsc 回投，UI 线程零阻塞；
//! - DanmuStore 每 16ms 一次 watcher（drain 事件 / 换道 / 淘汰），overlay 窗口关闭
//!   → 实体销毁 → `this.update` 失败 → 泵自动退出。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;

use tozo_extra::engines::danmu_cache::{DanmuCache, FETCH_COOLDOWN_SECS};
use tozo_extra::engines::{bilibili, Danmu};

use crate::sources::media::MediaInfo;

/// 弹幕显示配置
#[derive(Clone, Debug, PartialEq)]
pub struct DanmuConfig {
    pub font_px: f32,
    pub region_percent: u32,
    pub speed_px_ms: f32,
    pub lane_gap_px: f32,
}

impl Default for DanmuConfig {
    fn default() -> Self {
        Self {
            font_px: 18.,
            region_percent: 25,
            speed_px_ms: 0.22,
            lane_gap_px: 40.,
        }
    }
}

impl DanmuConfig {
    pub fn sanitize(mut self) -> Self {
        self.font_px = self.font_px.clamp(10., 48.);
        self.region_percent = self.region_percent.clamp(25, 100);
        self.speed_px_ms = self.speed_px_ms.clamp(0.05, 2.);
        self.lane_gap_px = self.lane_gap_px.clamp(0., 200.);
        self
    }

    pub fn line_height(&self) -> f32 {
        (self.font_px * 4. / 3.).round()
    }
}

static CONFIG: OnceLock<RwLock<DanmuConfig>> = OnceLock::new();

fn config_cell() -> &'static RwLock<DanmuConfig> {
    CONFIG.get_or_init(|| RwLock::new(DanmuConfig::default()))
}

/// 更新弹幕配置（预留：设置界面入口）
#[allow(dead_code)]
pub fn set_danmu_config(cfg: DanmuConfig) {
    *config_cell().write().unwrap_or_else(|e| e.into_inner()) = cfg.sanitize();
}

pub fn danmu_config() -> DanmuConfig {
    config_cell().read().map(|c| c.clone()).unwrap_or_default()
}

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_danmu_enabled(value: bool) {
    ENABLED.store(value, Ordering::Relaxed);
}

pub fn danmu_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

const DANMU_TOP_OFFSET: f32 = 16.;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct DanmuLayout {
    pub font_px: f32,
    pub line_height: f32,
    pub top_offset: f32,
}

fn screen_size() -> (f32, f32) {
    tozo_platform::primary_monitor()
        .map(|m| {
            (
                (m.rect.right - m.rect.left) as f32 / m.scale as f32,
                (m.rect.bottom - m.rect.top) as f32 / m.scale as f32,
            )
        })
        .unwrap_or((1920., 1080.))
}

/// 估算弹幕显示像素宽：字符数 × 字号，重复弹幕附加 " ×N"
fn display_est_px(content: &str, count: u32, font_px: f32) -> u32 {
    let mut chars = content.chars().count();
    if count > 1 {
        chars += 2 + count.to_string().len();
    }
    (chars as f32 * font_px).round() as u32
}

/// 弹幕从右侧进入到完全离开左侧的耗时（ms）
fn travel_ms(est_px: u32, screen_w: f32, speed_px_ms: f32) -> u64 {
    ((screen_w + est_px as f32) / speed_px_ms) as u64
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActiveDanmu {
    pub id: i64,
    pub content: String,
    pub color: u32,
    pub fontsize: u8,
    pub spawn_ms: u64,
    pub count: u32,
    pub lane: u32,
    pub travel_ms: u64,
    pub est_px: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct DanmuScreen {
    pub t_ms: u64,
    pub items: Vec<ActiveDanmu>,
    pub layout: DanmuLayout,
}

enum FetchEvent {
    CidOk { bv: String, aid: i64, cid: i64 },
    CidErr { bv: String },
    SegmentOk { index: u32, list: Vec<Danmu> },
    SegmentErr { index: u32 },
}

/// 发送端随 overlay 重建：旧通道的接收端已销毁，发送静默失败即可
static FETCH_TX: Mutex<Option<mpsc::Sender<FetchEvent>>> = Mutex::new(None);

fn fetch_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("danmu fetch runtime")
    })
}

fn spawn_fetch(future: impl std::future::Future<Output = FetchEvent> + Send + 'static) {
    let sender = FETCH_TX.lock().unwrap().clone();
    fetch_runtime().spawn(async move {
        let event = future.await;
        if let Some(sender) = sender {
            let _ = sender.send(event);
        }
    });
}

async fn fetch_cid_task(bv: String) -> FetchEvent {
    match bilibili::fetch_cid(&bv).await {
        Ok((aid, cid)) => FetchEvent::CidOk { bv, aid, cid },
        Err(e) => {
            tracing::warn!("[danmu] cid fetch failed: {e}");
            FetchEvent::CidErr { bv }
        }
    }
}

async fn fetch_segment_task(aid: i64, cid: i64, index: u32) -> FetchEvent {
    match bilibili::fetch_segment(aid, cid, index).await {
        Ok(list) => FetchEvent::SegmentOk { index, list },
        Err(e) => {
            tracing::warn!("[danmu] segment {index} fetch failed: {e}");
            FetchEvent::SegmentErr { index }
        }
    }
}

/// 弹幕 watcher：吃媒体信息与回投事件，产出每帧的 DanmuScreen（freya 版 DanmuWatcher 移植）
struct DanmuWatcher {
    fetch_rx: mpsc::Receiver<FetchEvent>,
    last_media: Option<MediaInfo>,
    base_pos: u64,
    base_at: Option<Instant>,
    playing: bool,
    bv: Option<String>,
    pending_cid: Option<String>,
    cid_failed_at: Option<Instant>,
    cache: Option<DanmuCache>,
    pending_segments: HashSet<u32>,
    active: Vec<ActiveDanmu>,
    screen: DanmuScreen,
    cfg: DanmuConfig,
    lanes: u32,
    screen_w: f32,
    screen_h: f32,
}

impl DanmuWatcher {
    fn new(fetch_rx: mpsc::Receiver<FetchEvent>) -> Self {
        let mut watcher = Self {
            fetch_rx,
            last_media: None,
            base_pos: 0,
            base_at: None,
            playing: false,
            bv: None,
            pending_cid: None,
            cid_failed_at: None,
            cache: None,
            pending_segments: HashSet::new(),
            active: Vec::new(),
            screen: DanmuScreen::default(),
            cfg: DanmuConfig::default(),
            lanes: 1,
            screen_w: 1920.,
            screen_h: 1080.,
        };
        watcher.refresh_geom();
        watcher
    }

    fn refresh_geom(&mut self) {
        let (w, h) = screen_size();
        self.screen_w = w;
        self.screen_h = h;
        let region = self.screen_h * self.cfg.region_percent as f32 / 100.;
        self.lanes = (region / self.cfg.line_height()).floor().max(1.) as u32;
    }

    fn blank(&mut self) -> DanmuScreen {
        self.active.clear();
        self.screen = DanmuScreen::default();
        self.screen.clone()
    }

    fn apply(&mut self, event: FetchEvent) {
        match event {
            FetchEvent::CidOk { bv, aid, cid } => {
                if self.bv.as_deref() == Some(bv.as_str()) && self.cache.is_none() {
                    self.cache = Some(DanmuCache::new(aid, cid));
                    self.cid_failed_at = None;
                }
                self.pending_cid = None;
            }
            FetchEvent::CidErr { bv } => {
                if self.bv.as_deref() == Some(bv.as_str()) {
                    self.cid_failed_at = Some(Instant::now());
                }
                if self.pending_cid.as_deref() == Some(bv.as_str()) {
                    self.pending_cid = None;
                }
            }
            FetchEvent::SegmentOk { index, list } => {
                if let Some(cache) = self.cache.as_mut() {
                    cache.insert(index, list);
                }
                self.pending_segments.remove(&index);
            }
            FetchEvent::SegmentErr { index } => {
                if let Some(cache) = self.cache.as_mut() {
                    cache.fail(index);
                }
                self.pending_segments.remove(&index);
            }
        }
    }

    /// 找一条尾部已离开屏幕的弹幕车道，没有则 None
    fn free_lane(&self, t: u64) -> Option<u32> {
        let mut best: Option<(u64, u32)> = None;
        for lane in 0..self.lanes {
            let slack = match self
                .active
                .iter()
                .filter(|a| a.lane == lane)
                .max_by_key(|a| a.spawn_ms)
            {
                None => u64::MAX,
                Some(last) => {
                    let need =
                        ((last.est_px as f32 + self.cfg.lane_gap_px) / self.cfg.speed_px_ms) as u64;
                    let dt = t.saturating_sub(last.spawn_ms);
                    if dt < need {
                        continue;
                    }
                    dt - need
                }
            };
            if best.is_none_or(|(s, _)| slack > s) {
                best = Some((slack, lane));
            }
        }
        best.map(|(_, lane)| lane)
    }

    /// 每 16ms 一次：媒体变化 → 拉分段 → 游标推进 → 换道合并 → 淘汰过期
    fn poll(&mut self, info: &MediaInfo) -> DanmuScreen {
        while let Ok(event) = self.fetch_rx.try_recv() {
            self.apply(event);
        }
        if !danmu_enabled() {
            return self.blank();
        }

        let cfg = danmu_config();
        if cfg != self.cfg {
            self.cfg = cfg;
            self.active.clear();
            self.refresh_geom();
        }

        if self.last_media.as_ref() != Some(info) {
            self.last_media = Some(info.clone());
            self.playing = info.playing;
            self.base_pos = info.position_ms;
            self.base_at = Some(Instant::now());
            let bv = info
                .smtc_metadata
                .as_ref()
                .map(|meta| meta.identifier.clone());
            if bv != self.bv {
                self.bv = bv;
                self.cache = None;
                self.pending_segments.clear();
                self.active.clear();
                self.refresh_geom();
            }
        }

        let Some(bv) = self.bv.clone() else {
            return self.blank();
        };
        if self.cache.is_none() {
            if self.pending_cid.is_some() {
                return self.blank();
            }
            if let Some(at) = self.cid_failed_at {
                if at.elapsed().as_secs() < FETCH_COOLDOWN_SECS {
                    return self.blank();
                }
            }
            self.pending_cid = Some(bv.clone());
            spawn_fetch(fetch_cid_task(bv));
            return self.blank();
        }

        let t = match self.base_at {
            Some(at) if self.playing => self.base_pos + at.elapsed().as_millis() as u64,
            _ => self.base_pos,
        };

        let mut spawned = Vec::new();
        if let Some(cache) = self.cache.as_mut() {
            for index in cache.wanted(t) {
                if self.pending_segments.insert(index) {
                    let (aid, cid) = cache.ids();
                    spawn_fetch(fetch_segment_task(aid, cid, index));
                }
            }
            spawned = cache.advance(t);
        }

        for d in spawned {
            if let Some(idx) = self.active.iter().position(|a| a.content == d.content) {
                let lane = self.free_lane(t);
                let a = &mut self.active[idx];
                a.count += 1;
                a.spawn_ms = d.progress_ms as u64;
                a.est_px = display_est_px(&a.content, a.count, self.cfg.font_px);
                a.travel_ms = travel_ms(a.est_px, self.screen_w, self.cfg.speed_px_ms);
                if let Some(lane) = lane {
                    a.lane = lane;
                }
                continue;
            }
            let est_px = display_est_px(&d.content, 1, self.cfg.font_px);
            let Some(lane) = self.free_lane(t) else {
                continue;
            };
            self.active.push(ActiveDanmu {
                id: d.id,
                content: d.content,
                color: d.color,
                fontsize: d.fontsize,
                spawn_ms: d.progress_ms as u64,
                count: 1,
                lane,
                travel_ms: travel_ms(est_px, self.screen_w, self.cfg.speed_px_ms),
                est_px,
            });
        }
        self.active
            .retain(|a| t >= a.spawn_ms && t - a.spawn_ms <= a.travel_ms);

        self.screen = DanmuScreen {
            t_ms: t,
            items: self.active.clone(),
            layout: DanmuLayout {
                font_px: self.cfg.font_px,
                line_height: self.cfg.line_height(),
                top_offset: DANMU_TOP_OFFSET,
            },
        };
        self.screen.clone()
    }
}

/// overlay 窗口的弹幕实体：每 16ms 泵 watcher，屏幕变化才 notify
pub struct DanmuStore {
    watcher: DanmuWatcher,
    pub screen: DanmuScreen,
}

impl DanmuStore {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // 每次开窗重建回投通道；在途的旧 fetch 发送到已死通道，静默丢弃
        let (tx, rx) = mpsc::channel();
        *FETCH_TX.lock().unwrap() = Some(tx);

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            // overlay 关闭 → 实体销毁 → update 失败 → 泵退出
            if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                break;
            }
        })
        .detach();

        Self {
            watcher: DanmuWatcher::new(rx),
            screen: DanmuScreen::default(),
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let info = cx.global::<crate::MediaGlobal>().0.read(cx).info.clone();
        let prev = std::mem::take(&mut self.screen);
        self.screen = self.watcher.poll(&info);
        if self.screen != prev {
            cx.notify();
        }
    }
}
