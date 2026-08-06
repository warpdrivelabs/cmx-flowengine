/*
 * @Describe: 可注入时钟 —— 把「当前时间」从内核里抽出来，让时间可控、可测。
 *
 * M1 的 `now()` 自由函数注释就埋了这颗种子：「集中一处便于将来注入可测时钟」。M2.5
 * 兑现它：引擎持 `Arc<dyn Clock>`，生产用 SystemClock（真实 UTC），测试用 TestClock
 * （手动拨快），定时器逻辑因此可以确定性复现，无需真实等待。
 *
 * 设计取舍：引擎本体**不自带后台线程**。时钟只回答「现在几点」，定时器的推进由外部
 * 显式调 `Engine::trigger_due_timers()`（demo 自己起 tokio 轮询）。这让引擎保持纯函数式、
 * 可测，调度策略留给宿主。
 */

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

/// 时钟契约：只回答「当前时刻」。Send + Sync 以便 Arc 跨线程共享。
pub trait Clock: Send + Sync {
    /// 当前 UTC 时刻。
    fn now(&self) -> DateTime<Utc>;
}

/// 生产时钟：直接取系统 UTC。
#[derive(Debug, Clone, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// 测试时钟：内部持一个可变时刻，测试里手动 `advance` / `set`，让定时器到期确定性可控。
///
/// 用 `Arc<Mutex<..>>` 让克隆共享同一时刻——把 TestClock 交给引擎后，测试仍可持一个克隆
/// 拨动时间，引擎侧立即可见。
#[derive(Clone)]
pub struct TestClock {
    at: Arc<Mutex<DateTime<Utc>>>,
}

impl TestClock {
    /// 以给定时刻起步。
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            at: Arc::new(Mutex::new(start)),
        }
    }

    /// 把时钟往前拨 `d`（模拟时间流逝，触发到期定时器）。
    pub fn advance(&self, d: Duration) {
        let mut guard = self.at.lock().expect("TestClock 互斥锁中毒");
        *guard += d;
    }

    /// 直接设定到某一时刻。
    pub fn set(&self, t: DateTime<Utc>) {
        let mut guard = self.at.lock().expect("TestClock 互斥锁中毒");
        *guard = t;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.at.lock().expect("TestClock 互斥锁中毒")
    }
}
