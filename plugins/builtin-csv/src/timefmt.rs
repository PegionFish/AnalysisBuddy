//! 时间解析（sdk-plugins.md §3.2 `time_format` 四态）。

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::config::TimeFormat;

/// 解析时间文本 → UTC 毫秒（i64）。
pub fn parse_time(s: &str, fmt: &TimeFormat) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    match fmt {
        TimeFormat::EpochMs => t.parse::<i64>().ok(),
        TimeFormat::Iso8601 => parse_iso8601(t),
        TimeFormat::Custom(pattern) => parse_custom(t, pattern),
        TimeFormat::Auto => {
            // 纯整数 → epoch 毫秒直读；否则按 ISO8601。
            if t.chars().all(|c| c.is_ascii_digit()) {
                t.parse::<i64>().ok()
            } else {
                parse_iso8601(t)
            }
        }
    }
}

/// ISO8601 / RFC3339（含时区，缺省视为 UTC）。
fn parse_iso8601(s: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    // 无时区：按 UTC 解析（毫秒级小数可选）。
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp_millis())
}

/// custom：chrono strftime 子集；先按有时区解析，失败再按无时区（视为 UTC）。
fn parse_custom(s: &str, pattern: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_str(s, pattern) {
        return Some(dt.timestamp_millis());
    }
    let naive = NaiveDateTime::parse_from_str(s, pattern).ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TimeFormat;

    const MS: i64 = 1785542400123; // 2026-08-01T00:00:00.123Z
    const MS0: i64 = 1785542400000; // 2026-08-01T00:00:00Z

    #[test]
    fn epoch_ms() {
        assert_eq!(parse_time("1785542400123", &TimeFormat::EpochMs), Some(MS));
        assert_eq!(
            parse_time(" 1785542400123 ", &TimeFormat::EpochMs),
            Some(MS)
        );
        assert_eq!(parse_time("abc", &TimeFormat::EpochMs), None);
        assert_eq!(parse_time("-1", &TimeFormat::EpochMs), Some(-1));
    }

    #[test]
    fn iso8601_with_timezone() {
        assert_eq!(
            parse_time("2026-08-01T00:00:00.123Z", &TimeFormat::Iso8601),
            Some(MS)
        );
        assert_eq!(
            parse_time("2026-08-01T08:00:00.123+08:00", &TimeFormat::Iso8601),
            Some(MS)
        );
        assert_eq!(
            parse_time("2026-08-01T00:00:00.123+00:00", &TimeFormat::Iso8601),
            Some(MS)
        );
        assert_eq!(
            parse_time("2026-08-01T00:00:00Z", &TimeFormat::Iso8601),
            Some(MS0)
        );
    }

    #[test]
    fn iso8601_without_timezone_assumed_utc() {
        assert_eq!(
            parse_time("2026-08-01T00:00:00.123", &TimeFormat::Iso8601),
            Some(MS)
        );
    }

    #[test]
    fn iso8601_invalid() {
        assert_eq!(
            parse_time("2026-13-01T00:00:00", &TimeFormat::Iso8601),
            None
        );
        assert_eq!(parse_time("hello", &TimeFormat::Iso8601), None);
        assert_eq!(parse_time("", &TimeFormat::Iso8601), None);
    }

    #[test]
    fn custom_pattern() {
        let fmt = TimeFormat::Custom("%Y/%m/%d %H:%M:%S%.f".into());
        assert_eq!(parse_time("2026/08/01 00:00:00.123", &fmt), Some(MS));
        let fmt = TimeFormat::Custom("%Y%m%d %H:%M:%S".into());
        assert_eq!(parse_time("20260801 00:00:00", &fmt), Some(MS0));
        assert_eq!(parse_time("not a date", &fmt), None);
    }

    #[test]
    fn auto_mode() {
        assert_eq!(parse_time("1785542400123", &TimeFormat::Auto), Some(MS));
        assert_eq!(
            parse_time("2026-08-01T00:00:00.123Z", &TimeFormat::Auto),
            Some(MS)
        );
        assert_eq!(
            parse_time("2026-08-01T08:00:00.123+08:00", &TimeFormat::Auto),
            Some(MS)
        );
        assert_eq!(parse_time("nope", &TimeFormat::Auto), None);
    }
}
