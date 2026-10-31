//! Dates as UTC day numbers: whole days since 1970-01-01.
//!
//! Views are stored per day under this number, so a rolling window such as
//! "the last 365 days" is a range over one integer column.

const MS_PER_DAY: u64 = 86_400_000;

/// Longest window the API accepts, about ten years.
pub const MAX_DAYS: u32 = 3660;

/// The UTC day a Unix timestamp in milliseconds falls on.
pub fn from_millis(ms: u64) -> i64 {
    (ms / MS_PER_DAY) as i64
}

/// The first day of a window of `days` days that ends on `today`, inclusive.
/// A one-day window is just `today`.
pub fn window_start(today: i64, days: u32) -> i64 {
    today - i64::from(days) + 1
}

/// The ISO 8601 date (`YYYY-MM-DD`) of a day number.
pub fn to_iso(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Splits a day number into year, month and day of the proleptic Gregorian
/// calendar. This is Howard Hinnant's `civil_from_days`, which shifts the
/// year to start in March so leap days fall at the end of it.
fn civil_from_days(day: i64) -> (i64, u32, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// One entry per day from `from` to `to`, inclusive, taking counts from
/// `counts` (sorted by day) and filling the days it skips with zero.
pub fn fill_series(from: i64, to: i64, counts: &[(i64, u64)]) -> Vec<(i64, u64)> {
    let mut counts = counts.iter().peekable();
    (from..=to)
        .map(|day| {
            while counts.next_if(|(d, _)| *d < day).is_some() {}
            let views = counts.next_if(|(d, _)| *d == day).map_or(0, |(_, v)| *v);
            (day, views)
        })
        .collect()
}

/// Parses the `days` query parameter: a whole number from 1 to [`MAX_DAYS`].
pub fn parse_days(raw: &str) -> Result<u32, String> {
    match raw.trim().parse::<u32>() {
        Ok(days) if (1..=MAX_DAYS).contains(&days) => Ok(days),
        _ => Err(format!("days must be a whole number from 1 to {MAX_DAYS}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse of `civil_from_days`, written independently so the two
    /// can check each other.
    fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = i64::from(m);
        let mp = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    #[test]
    fn epoch_is_day_zero() {
        assert_eq!(from_millis(0), 0);
        assert_eq!(to_iso(0), "1970-01-01");
    }

    #[test]
    fn day_changes_at_utc_midnight() {
        let midnight = 1_793_491_200_000; // 2026-11-01T00:00:00Z
        assert_eq!(to_iso(from_millis(midnight - 1)), "2026-10-31");
        assert_eq!(to_iso(from_millis(midnight)), "2026-11-01");
        assert_eq!(
            from_millis(midnight + MS_PER_DAY - 1),
            from_millis(midnight)
        );
    }

    #[test]
    fn known_dates() {
        assert_eq!(to_iso(-1), "1969-12-31");
        assert_eq!(to_iso(10_957), "2000-01-01");
        assert_eq!(to_iso(11_016), "2000-02-29");
        assert_eq!(to_iso(20_392), "2025-10-31");
        assert_eq!(to_iso(20_758), "2026-11-01");
    }

    #[test]
    fn leap_years() {
        assert_eq!(to_iso(days_from_civil(2024, 2, 28) + 1), "2024-02-29");
        assert_eq!(to_iso(days_from_civil(2026, 2, 28) + 1), "2026-03-01");
        // 1900 is not a leap year, 2000 is.
        assert_eq!(to_iso(days_from_civil(1900, 2, 28) + 1), "1900-03-01");
        assert_eq!(to_iso(days_from_civil(2000, 2, 28) + 1), "2000-02-29");
    }

    #[test]
    fn round_trips_every_day_for_four_centuries() {
        let start = days_from_civil(1900, 1, 1);
        let end = days_from_civil(2300, 1, 1);
        let mut expected = (1900, 1, 1);
        for day in start..end {
            assert_eq!(civil_from_days(day), expected, "day {day}");
            assert_eq!(days_from_civil(expected.0, expected.1, expected.2), day);
            expected = next_date(expected);
        }
    }

    fn next_date((y, m, d): (i64, u32, u32)) -> (i64, u32, u32) {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = match m {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        match (d < len, m < 12) {
            (true, _) => (y, m, d + 1),
            (false, true) => (y, m + 1, 1),
            (false, false) => (y + 1, 1, 1),
        }
    }

    #[test]
    fn windows_include_today() {
        let today = days_from_civil(2026, 11, 1);
        assert_eq!(window_start(today, 1), today);
        assert_eq!(to_iso(window_start(today, 7)), "2026-10-26");
        assert_eq!(to_iso(window_start(today, 30)), "2026-10-03");
        // 365 days back from 2026-11-01 starts on 2025-11-02.
        assert_eq!(to_iso(window_start(today, 365)), "2025-11-02");
        // Across a leap day: 2024-03-01 back 366 days starts on 2023-03-02.
        let march = days_from_civil(2024, 3, 1);
        assert_eq!(to_iso(window_start(march, 366)), "2023-03-02");
    }

    #[test]
    fn series_fills_missing_days() {
        assert_eq!(
            fill_series(10, 14, &[(11, 3), (13, 1)]),
            vec![(10, 0), (11, 3), (12, 0), (13, 1), (14, 0)]
        );
        assert_eq!(fill_series(10, 12, &[]), vec![(10, 0), (11, 0), (12, 0)]);
        assert_eq!(fill_series(10, 10, &[(10, 7)]), vec![(10, 7)]);
    }

    #[test]
    fn series_ignores_days_outside_the_range() {
        assert_eq!(
            fill_series(10, 11, &[(8, 5), (10, 1), (12, 9)]),
            vec![(10, 1), (11, 0)]
        );
    }

    #[test]
    fn days_parameter() {
        assert_eq!(parse_days("365"), Ok(365));
        assert_eq!(parse_days(" 1 "), Ok(1));
        assert_eq!(parse_days("3660"), Ok(MAX_DAYS));
        for bad in ["0", "-5", "3661", "1.5", "", "year"] {
            assert!(parse_days(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
