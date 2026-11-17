//! Calendar date from Unix milliseconds, without a date-time dependency.

/// Formats Unix time in milliseconds as a UTC `YYYY-MM-DD` date.
///
/// # Examples
/// ```
/// assert_eq!(harness_context::civil_date(0), "1970-01-01");
/// ```
#[must_use]
pub fn civil_date(unix_ms: i64) -> String {
    // Howard Hinnant's days-to-civil algorithm (proleptic Gregorian calendar).
    let z = unix_ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(1_763_337_600_000), "2025-11-17");
        assert_eq!(civil_date(1_709_164_800_000), "2024-02-29");
        assert_eq!(civil_date(1_709_164_800_000 + 86_399_999), "2024-02-29");
    }

    #[test]
    fn dates_before_the_epoch() {
        assert_eq!(civil_date(-1), "1969-12-31");
        assert_eq!(civil_date(-86_400_000), "1969-12-31");
    }
}
