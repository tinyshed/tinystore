//! A sample's time as an error names it: RFC 3339 in UTC, its milliseconds
//! when it has some.

/// `2026-10-11T09:00:00Z`, `2026-10-11T09:00:00.250Z`.
pub(super) fn rfc3339(millis: i64) -> String {
    let (days, of_day) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000));
    let (year, month, day) = crate::clock::civil_from_days(days);
    let (hour, minute, second, milli) = (of_day / 3_600_000, of_day / 60_000 % 60, of_day / 1000 % 60, of_day % 1000);
    let fraction = if milli == 0 { String::new() } else { format!(".{milli:03}") };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_reads_as_its_utc_date() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_795_600_000), "2026-10-12T09:00:00Z");
        assert_eq!(rfc3339(1_791_795_600_250), "2026-10-12T09:00:00.250Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59.999Z");
    }
}
