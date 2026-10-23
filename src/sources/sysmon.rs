use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use sysinfo::{Networks, ProcessesToUpdate, System};

use super::clock::pill;

#[derive(Clone, Debug, PartialEq)]
pub struct ProcSnapshot {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem_bytes: u64,
}

pub struct ProcStore {
    pub procs: Vec<ProcSnapshot>,
}

impl ProcStore {
    /// 每 1.5s 刷新进程列表，CPU 按任务管理器口径除以逻辑核数
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let mut sys = System::new();
            sys.refresh_cpu_usage();
            let cores = sys.cpus().len().max(1) as f32;
            sys.refresh_processes(ProcessesToUpdate::All, true);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                sys.refresh_processes(ProcessesToUpdate::All, true);
                let procs: Vec<ProcSnapshot> = sys
                    .processes()
                    .iter()
                    .map(|(pid, p)| ProcSnapshot {
                        pid: pid.as_u32(),
                        name: p.name().to_string_lossy().to_string(),
                        // sysinfo 进程 cpu 按每核 100% 计，任务管理器口径需除以逻辑核数
                        cpu: p.cpu_usage() / cores,
                        mem_bytes: p.memory(),
                    })
                    .collect();
                if this
                    .update(cx, |store, cx| {
                        store.procs = procs;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self { procs: Vec::new() }
    }
}

/// 以 PROCESS_TERMINATE 权限打开进程并结束，返回是否成功
pub fn kill_process(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) else {
            return false;
        };
        let ok = TerminateProcess(handle, 1).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        ok
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemSnapshot {
    pub cpu_percent: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

pub struct SysStore {
    snapshot: SystemSnapshot,
    sampler: Sampler,
}

impl SysStore {
    /// 起每秒一次的采样循环，更新全局 CPU/内存/网速快照
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            snapshot: SystemSnapshot::default(),
            sampler: Sampler::default(),
        };
        this.sampler.refresh();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = this.update(cx, |this, cx| {
                    this.snapshot = this.sampler.poll();
                    cx.notify();
                });
            }
        })
        .detach();
        this
    }
}

impl Render for SysStore {
    /// 渲染 CPU/RAM/网速岛，点击打开进程弹窗
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = &self.snapshot;
        let mem_percent = if snap.mem_total_bytes > 0 {
            snap.mem_used_bytes as f64 / snap.mem_total_bytes as f64 * 100.0
        } else {
            0.0
        };
        let text = format!(
            "CPU {:>3.0}%  RAM {:>3.0}%  ↓{} ↑{}",
            snap.cpu_percent,
            mem_percent,
            fmt_bps(snap.rx_bps),
            fmt_bps(snap.tx_bps),
        );
        pill()
            .id("sys-island")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.defer(|cx| crate::sys_popup::toggle(cx));
                }),
            )
            .font_family("Consolas")
            .child(text)
    }
}

/// 网速值自适应 B/s 到 GB/s 的单位格式化
fn fmt_bps(v: f64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut val = v;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{val:.0} {}", UNITS[unit])
    } else {
        format!("{val:.1} {}", UNITS[unit])
    }
}

#[derive(Default)]
struct Sampler {
    sys: System,
    networks: Networks,
    last_rx: u64,
    last_tx: u64,
    last_at: Option<Instant>,
}

impl Sampler {
    /// 刷新全局 CPU、内存与各网卡累计流量计数
    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.networks.refresh(true);
    }

    /// 汇总一次系统快照，按与上次的间隔差分计算上下行速率
    fn poll(&mut self) -> SystemSnapshot {
        self.refresh();
        let (rx, tx) = self.networks.iter().fold((0u64, 0u64), |(r, t), (_, data)| {
            (r + data.received(), t + data.transmitted())
        });
        let now = Instant::now();
        let (rx_bps, tx_bps) = match self.last_at {
            Some(at) if rx >= self.last_rx && tx >= self.last_tx => {
                let dt = now.duration_since(at).as_secs_f64().max(1e-3);
                (
                    (rx - self.last_rx) as f64 * 8.0 / dt,
                    (tx - self.last_tx) as f64 * 8.0 / dt,
                )
            }
            _ => (0.0, 0.0),
        };
        self.last_rx = rx;
        self.last_tx = tx;
        self.last_at = Some(now);
        SystemSnapshot {
            cpu_percent: self.sys.global_cpu_usage(),
            mem_used_bytes: self.sys.used_memory(),
            mem_total_bytes: self.sys.total_memory(),
            rx_bps,
            tx_bps,
        }
    }
}
