//! 周期扫描的节流：`events::watch` 每 `status.detector_interval`（2 s）一 tick，但有些扫描一小时
//! 做一次就够——external FINISHED 行过期（agora-j4w.3）、`done/` 归档清理（agora-t36）。
//! 两边共用这一个形状：谁的周期到了谁扫，tick 本身不变快也不变慢。

use std::sync::Mutex;
use std::time::Duration;

/// 每 `every` 至多放行一次。第一次问总是放行——daemon 刚起来的第一 tick 就扫一遍，
/// 停机期间到期的行不必再等一个周期。
pub struct Throttle {
    every: i64,
    last: Mutex<Option<i64>>,
}

impl Throttle {
    pub fn new(every: Duration) -> Self {
        Throttle {
            every: every.as_secs() as i64,
            last: Mutex::new(None),
        }
    }

    /// 到点了就记账并放行。放行即记账、不看调用方扫得成不成：扫描失败会打日志，一个周期后再来
    /// 一次，比每 tick 重试刷屏好。`now` 由调用方给（unix 秒），测试拨表不用真等。
    pub fn due(&self, now: i64) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(prev) if now - prev < self.every => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }

    pub fn every(&self) -> Duration {
        Duration::from_secs(self.every.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_is_due_then_once_per_period() {
        let t = Throttle::new(Duration::from_secs(3600));
        assert!(t.due(1_000));
        assert!(!t.due(1_000 + 3599), "周期未满不放行");
        assert!(t.due(1_000 + 3600), "满一个周期放行");
        assert!(!t.due(1_000 + 3600 + 1), "放行即记账，从放行时刻重新计");
    }
}
