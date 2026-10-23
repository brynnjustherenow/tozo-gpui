use std::time::{Duration, Instant};

use sysinfo::{Networks, System};
use tokio::sync::watch;

use crate::scheduler::{scheduler, Schedule};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemSnapshot {
    pub cpu_percent: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub rx_bps: f64,
    pub tx_bps: f64,
}

/// 启动系统采样任务，每秒推送一次快照
pub fn system_stream() -> watch::Receiver<SystemSnapshot> {
    let mut sampler = Sampler::default();
    sampler.refresh();
    scheduler().poll(
        "system",
        Schedule::Interval(Duration::from_secs(1)),
        SystemSnapshot::default(),
        move || sampler.poll(),
    )
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
    /// 刷新 CPU、内存与网卡计数
    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.networks.refresh(true);
    }

    /// 采样并计算 CPU、内存与收发速率快照
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 测试快照内存总量合理
    fn snapshot_has_sane_totals() {
        let mut sampler = Sampler::default();
        sampler.refresh();
        let snapshot = sampler.poll();
        assert!(snapshot.mem_total_bytes > 0);
        assert!(snapshot.mem_used_bytes <= snapshot.mem_total_bytes);
    }

    #[test]
    /// 测试首次速率为零且后续非负
    fn rates_are_zero_on_first_poll_then_nonnegative() {
        let mut sampler = Sampler::default();
        sampler.refresh();
        let first = sampler.poll();
        assert_eq!((first.rx_bps, first.tx_bps), (0.0, 0.0));
        let second = sampler.poll();
        assert!(second.rx_bps >= 0.0 && second.tx_bps >= 0.0);
    }
}
