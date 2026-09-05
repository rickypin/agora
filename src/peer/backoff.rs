//! peer 重连的退避策略（A29 的策略半边；不变量 8）。
//!
//! 参数是 agora-7ku notes（2026-09-02）的输入，落地后回写 docs/spec/architecture.md：
//! 指数退避 + jitter，1 s 起步、30 s 封顶、**永不放弃**；每次连接尝试 5 s 超时。
//!
//! 全是纯函数：不读时钟、不掷骰子。等多久由 `BackoffPolicy::delay` 算，jitter 由调用方注入
//! （生产用 `random_jitter`，测试给定值），真正的等待交给调用方的 `tokio::time::sleep`——
//! 所以单测不需要假时钟，断言的就是返回值本身。

use std::time::Duration;

/// 每次连接尝试的超时。devcenter 教训：`TcpStream::connect` 不带超时把 plan 拖满 45 s 并泄漏
/// 120 s 的阻塞线程（docs/analysis/devcenter/appendix-b-multihost.md）。传输层（agora-7ku.11）按它设。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 指数退避的两个参数。没有"最多几次"——不变量 8 排除"重试 N 次后标 dead"，退避只封顶不终止。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// 第一次失败后等多久，之后每次翻倍。
    pub base: Duration,
    /// 上限。要低：Mac 睡眠是常态场景（MISSION §0.2），醒来后应秒级恢复。
    pub cap: Duration,
}

impl BackoffPolicy {
    /// peer 重连的默认：1 s 起步、30 s 封顶。
    pub const PEER: BackoffPolicy = BackoffPolicy {
        base: Duration::from_secs(1),
        cap: Duration::from_secs(30),
    };

    pub const fn new(base: Duration, cap: Duration) -> Self {
        BackoffPolicy { base, cap }
    }

    /// 连续失败 `failures` 次之后（0 起）等多久：d = min(base · 2^failures, cap)，再按
    /// jitter ∈ [0, 1) 在 [d/2, d] 里取一点。**只往下抖**，所以带 jitter 也永不超上限；
    /// jitter = 0 就是 1, 2, 4, …, 30, 30 的整齐序列。jitter 不在 [0, 1) 里（含 NaN）一律
    /// 夹回去，调用方给坏值也不会让等待变负或超顶。
    pub fn delay(&self, failures: u32, jitter: f64) -> Duration {
        // 2^failures 用 checked_shl / checked_mul 算：位移到 32 或乘法溢出都直接当作到顶，
        // 不然 failures 一大（永不放弃，它会一直涨）这里就 panic。
        let d = 1u32
            .checked_shl(failures)
            .and_then(|f| self.base.checked_mul(f))
            .map_or(self.cap, |d| d.min(self.cap));
        let j = if jitter.is_finite() {
            jitter.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // 用纳秒整数减，jitter = 0 时结果与 d 逐位相等（f64 乘法回来会差一纳秒）。
        let cut = (d.as_nanos() as f64 * j / 2.0) as u64;
        d.saturating_sub(Duration::from_nanos(cut))
    }
}

/// 一个 peer 的退避计数器：策略 + 连续失败次数。作为 `Iterator` 它**永远给 Some**——
/// 这是不变量 8 在类型上的表达，`tests` 里的守卫会拿它转一万圈。
#[derive(Debug, Clone)]
pub struct Backoff {
    policy: BackoffPolicy,
    failures: u32,
}

impl Backoff {
    pub const fn new(policy: BackoffPolicy) -> Self {
        Backoff {
            policy,
            failures: 0,
        }
    }

    pub fn policy(&self) -> BackoffPolicy {
        self.policy
    }

    /// 自上次成功以来连续失败了几次。
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// 又失败了一次：返回这次该等多久（jitter 见 `BackoffPolicy::delay`），计数饱和不回绕。
    pub fn next_delay(&mut self, jitter: f64) -> Duration {
        let d = self.policy.delay(self.failures, jitter);
        self.failures = self.failures.saturating_add(1);
        d
    }

    /// 连上了：从头来。
    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

impl Iterator for Backoff {
    type Item = Duration;

    /// 永不 None：退避可封顶、不可终止（不变量 8）。
    fn next(&mut self) -> Option<Duration> {
        Some(self.next_delay(random_jitter()))
    }
}

/// 系统随机源给的 jitter ∈ [0, 1)。随机源不可用就不抖（退避照常工作，只是几个 peer 可能同拍）。
pub fn random_jitter() -> f64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        return 0.0;
    }
    // 取高 53 位，是 f64 能精确表示的整数范围；除以 2^53 落在 [0, 1)。
    (u64::from_le_bytes(b) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn sequence_doubles_from_one_second_and_caps_at_thirty() {
        let seq: Vec<Duration> = (0..8).map(|n| BackoffPolicy::PEER.delay(n, 0.0)).collect();
        assert_eq!(
            seq,
            [S(1), S(2), S(4), S(8), S(16), S(30), S(30), S(30)],
            "1, 2, 4, …封顶 30 s"
        );
        // 计数再大也是顶，不 panic、不回绕成 1 s。
        assert_eq!(BackoffPolicy::PEER.delay(31, 0.0), S(30));
        assert_eq!(BackoffPolicy::PEER.delay(32, 0.0), S(30));
        assert_eq!(BackoffPolicy::PEER.delay(u32::MAX, 0.0), S(30));
    }

    #[test]
    fn jitter_stays_within_half_to_full_and_never_exceeds_the_cap() {
        let p = BackoffPolicy::PEER;
        for n in 0..40u32 {
            let full = p.delay(n, 0.0);
            for j in [
                0.0,
                0.1,
                0.5,
                0.999,
                1.0,
                1.7,
                -3.0,
                f64::NAN,
                f64::INFINITY,
            ] {
                let d = p.delay(n, j);
                assert!(d <= p.cap, "failures={n} jitter={j}: {d:?} 超过上限");
                assert!(d <= full, "failures={n} jitter={j}: jitter 只许往下抖");
                assert!(d >= full / 2, "failures={n} jitter={j}: 最多抖掉一半");
            }
        }
        // 半程 jitter 正好砍一半：确认公式方向。
        assert_eq!(p.delay(0, 1.0), Duration::from_millis(500));
        assert_eq!(p.delay(10, 1.0), S(15));
    }

    #[test]
    fn never_gives_up() {
        // 不变量 8：Iterator 一万圈没有一次 None，每次都 ≤ 上限；计数饱和在 u32::MAX 也照常。
        let mut b = Backoff::new(BackoffPolicy::PEER);
        for _ in 0..10_000 {
            let d = b.next().expect("退避永不放弃");
            assert!(d <= BackoffPolicy::PEER.cap);
        }
        b.failures = u32::MAX;
        assert_eq!(b.next_delay(0.0), S(30));
        assert!(b.next().is_some());
        assert_eq!(b.failures(), u32::MAX, "计数饱和不回绕");
        assert!(random_jitter() < 1.0 && random_jitter() >= 0.0);
    }

    #[test]
    fn reset_starts_over_after_a_successful_connect() {
        let mut b = Backoff::new(BackoffPolicy::PEER);
        assert_eq!(b.next_delay(0.0), S(1));
        assert_eq!(b.next_delay(0.0), S(2));
        assert_eq!(b.next_delay(0.0), S(4));
        assert_eq!(b.failures(), 3);
        b.reset();
        assert_eq!(b.failures(), 0);
        assert_eq!(b.next_delay(0.0), S(1));
    }

    #[test]
    fn every_connect_attempt_has_a_five_second_timeout() {
        // 守卫（规则 8）：谁把超时删了或调到几十秒，先来这里改数字、说清为什么。
        assert_eq!(CONNECT_TIMEOUT, S(5));
        assert!(
            CONNECT_TIMEOUT < BackoffPolicy::PEER.cap,
            "一次尝试不该比退避上限还久"
        );
    }
}
