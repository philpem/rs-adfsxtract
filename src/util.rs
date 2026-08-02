//! Small standalone utilities that don't belong to any one layer.

/// Howard Hinnant's `civil_from_days`: proleptic Gregorian calendar date
/// from a day count relative to 1970-01-01, without pulling in a calendar
/// crate. Handles negative day counts (pre-1970 dates), which do occur for
/// RISC OS's 1900 epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097); // [0, 146096], safe for plain `/` below
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// Formats Unix seconds as `yyyymmddhhnnss`, matching the `.inf` sidecar
/// `DATETIME=` convention.
pub fn format_inf_datetime(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86400);
    let secs_of_day = unix_secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}")
}

/// Formats Unix seconds as an ISO-8601 UTC timestamp, for JSON output.
pub fn format_iso8601(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86400);
    let secs_of_day = unix_secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_epoch_values() {
        assert_eq!(format_inf_datetime(0), "19700101000000");
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn pre_1970_dates_work() {
        // 1900-01-01 00:00:00 UTC is -2208988800 seconds from the Unix epoch.
        assert_eq!(format_inf_datetime(-2_208_988_800), "19000101000000");
    }

    #[test]
    fn known_2024_date() {
        // 2024-03-05 12:34:56 UTC
        assert_eq!(format_inf_datetime(1_709_642_096), "20240305123456");
    }
}
