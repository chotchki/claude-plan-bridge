/// Today's date as `YYYY-MM-DD` in UTC, computed from `SystemTime` without
/// pulling in `chrono`.
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = ymd_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert unix seconds (UTC) to (year, month, day) using Howard Hinnant's
/// civil_from_days algorithm.
pub fn ymd_from_unix(secs: u64) -> (i64, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ymd_examples() {
        // 1970-01-01 = unix 0
        assert_eq!(ymd_from_unix(0), (1970, 1, 1));
        // 2026-05-16 00:00:00 UTC = 1778889600 (verified externally)
        let (y, m, d) = ymd_from_unix(1_778_889_600);
        assert_eq!((y, m, d), (2026, 5, 16));
    }
}
