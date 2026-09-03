//! 时间文本：SQLite 生成的 `YYYY-MM-DDTHH:MM:SSZ` 与 unix 秒互转。
//!
//! 不引日期库：只需要秒级、只有 UTC、格式只有一种。

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `ts` 距今秒数；解析失败当作 None（调用方按"很久以前 / 不知道"处理）。
pub fn age_secs(ts: &str) -> Option<u64> {
    let secs = parse_utc_secs(ts)?;
    Some((now_secs() - secs).max(0) as u64)
}

pub fn parse_utc_secs(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(&ts[0..4])?,
        num(&ts[5..7])?,
        num(&ts[8..10])?,
        num(&ts[11..13])?,
        num(&ts[14..16])?,
        num(&ts[17..19])?,
    );
    // days-from-civil（Howard Hinnant）
    let (y2, mo2) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * mo2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_parse_matches_epoch() {
        assert_eq!(parse_utc_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_secs("2026-09-03T00:00:00Z"), Some(1_788_393_600));
        assert_eq!(parse_utc_secs("garbage"), None);
    }
}
