//! Centralized periodic task scheduler.
//!
//! All periodic polling in tozo is registered here instead of spawning one
//! thread per feature; registered tasks run sequentially on a single
//! background thread. Event-driven sources (SMTC media, network status)
//! subscribe to OS events on their own threads and do not belong here.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

/// Upper bound on the sleep between runs; also bounds how quickly newly
/// registered tasks are adopted.
const MAX_IDLE: Duration = Duration::from_millis(250);

/// Extra guard after a minute boundary so clock truncation never shows the
/// previous minute.
const MINUTE_GUARD: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Schedule {
    /// Re-run every duration, measured from the previous run's completion
    /// (no drift, no overlap).
    Interval(Duration),
    /// Run shortly after each wall-clock minute boundary.
    MinuteAligned,
}

#[derive(Clone, Debug)]
pub struct TaskHandle {
    cancelled: Arc<AtomicBool>,
}

impl TaskHandle {
    /// 标记任务为已取消
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// 查询任务是否已被取消
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

type TaskFn = Box<dyn FnMut() + Send + 'static>;

struct Task {
    name: &'static str,
    schedule: Schedule,
    next_run: Instant,
    cancelled: Arc<AtomicBool>,
    run: TaskFn,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Vec<Task>>,
    started: AtomicBool,
}

#[derive(Default)]
pub struct Scheduler {
    shared: Shared,
}

static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

/// 获取全局调度器单例
pub fn scheduler() -> &'static Scheduler {
    SCHEDULER.get_or_init(Scheduler::default)
}

/// 立即启动轮询线程（注册首个任务时也会懒启动）
/// Start the poll thread eagerly. It also starts lazily when the first task
/// is registered.
pub fn start() {
    scheduler().ensure_started();
}

impl Scheduler {
    /// 按调度周期运行 produce，值变化时才唤醒订阅者
    /// Run `produce` on `schedule` and publish its return value. Consumers
    /// holding the returned receiver are only woken when the value actually
    /// changed.
    pub fn poll<T>(
        &self,
        name: &'static str,
        schedule: Schedule,
        init: T,
        mut produce: impl FnMut() -> T + Send + 'static,
    ) -> watch::Receiver<T>
    where
        T: Clone + Send + Sync + PartialEq + 'static,
    {
        let (tx, rx) = watch::channel(init);
        let run = move || {
            let value = produce();
            let _ = tx.send_if_modified(|current| {
                if *current == value {
                    false
                } else {
                    *current = value;
                    true
                }
            });
        };
        self.register(name, schedule, Box::new(run));
        rx
    }

    /// 按调度周期运行 f，不产生数据流
    /// Run `f` on `schedule` without a data stream.
    pub fn tick(
        &self,
        name: &'static str,
        schedule: Schedule,
        mut f: impl FnMut() + Send + 'static,
    ) -> TaskHandle {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let run = move || {
            if !flag.load(Ordering::Relaxed) {
                f();
            }
        };
        self.register(name, schedule, Box::new(run));
        TaskHandle { cancelled }
    }

    /// 将任务加入待接入队列并确保轮询线程已启动
    fn register(&self, name: &'static str, schedule: Schedule, run: TaskFn) {
        self.shared.pending.lock().unwrap().push(Task {
            name,
            schedule,
            next_run: Instant::now(),
            cancelled: Arc::new(AtomicBool::new(false)),
            run,
        });
        self.ensure_started();
    }

    /// 确保轮询线程已启动，仅首个调用者会触发
    fn ensure_started(&self) {
        if !self.shared.started.swap(true, Ordering::SeqCst) {
            let _ = thread::Builder::new()
                .name("tozo-poll".to_string())
                .spawn(poll_loop);
        }
    }
}

/// 轮询线程主循环：收编任务、到期执行、休眠至最近的下次运行
fn poll_loop() {
    let shared = &scheduler().shared;
    let mut tasks: Vec<Task> = Vec::new();
    loop {
        {
            let mut pending = shared.pending.lock().unwrap();
            if !pending.is_empty() {
                for task in pending.drain(..) {
                    match tasks.iter_mut().find(|t| t.name == task.name) {
                        Some(slot) => *slot = task,
                        None => tasks.push(task),
                    }
                }
            }
        }

        let now = Instant::now();
        for task in tasks.iter_mut() {
            if task.cancelled.load(Ordering::Relaxed) || task.next_run > now {
                continue;
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| (task.run)()));
            if let Err(payload) = outcome {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic>".to_string());
                tracing::error!("[scheduler] task {} panicked: {message}", task.name);
            }
            task.next_run = Instant::now() + next_delay(task.schedule);
        }
        tasks.retain(|t| !t.cancelled.load(Ordering::Relaxed));

        let now = Instant::now();
        let delay = tasks
            .iter()
            .map(|t| t.next_run)
            .min()
            .map(|next| next.saturating_duration_since(now))
            .unwrap_or(MAX_IDLE);
        thread::sleep(delay.min(MAX_IDLE));
    }
}

/// 计算任务本次运行后距下次运行的延迟
fn next_delay(schedule: Schedule) -> Duration {
    match schedule {
        Schedule::Interval(d) => d,
        Schedule::MinuteAligned => delay_until_next_minute(),
    }
}

/// 计算距下一个分钟边界再加保护余量的延迟
pub(crate) fn delay_until_next_minute() -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let sub_second = Duration::from_nanos(now.subsec_nanos() as u64);
    let to_boundary = Duration::from_secs(60 - now.as_secs() % 60) - sub_second;
    to_boundary + MINUTE_GUARD
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// 验证分钟对齐延迟总是落在边界保护之后
    #[test]
    fn minute_aligned_delay_lands_after_boundary_guard() {
        let delay = delay_until_next_minute();
        assert!(delay >= Duration::from_millis(149));
        assert!(delay <= Duration::from_secs(60) + MINUTE_GUARD);
    }

    /// 验证 poll 周期执行并持续更新 watch 流
    #[test]
    fn poll_runs_repeatedly_and_updates_stream() {
        let counter = Arc::new(AtomicU32::new(0));
        let bump = counter.clone();
        let rx = scheduler().poll(
            "test-count",
            Schedule::Interval(Duration::from_millis(10)),
            0u32,
            move || bump.fetch_add(1, Ordering::SeqCst) + 1,
        );
        thread::sleep(Duration::from_millis(300));
        let value = *rx.borrow();
        assert!(value >= 3, "expected several ticks, got {value}");
    }
}
