//! US Pacific-time helpers for YouTube's daily quota reset.
//!
//! YouTube Data API quota is billed per Google Cloud **project** and resets at
//! **midnight Pacific Time** — the project's `quota_date` rolls over then. The
//! [crate::services::quota::QuotaGovernor] must align its day boundary to
//! Pacific or its accounting drifts a full day out of phase with Google's.
//!
//! We deliberately avoid pulling in `chrono-tz` (and its embedded zone tables)
//! for this single rule and instead derive the US Pacific UTC offset from the
//! federal DST schedule directly: PDT (UTC−7) from 02:00 on the 2nd Sunday of
//! March to 02:00 on the 1st Sunday of November, PST (UTC−8) otherwise.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};

/// UTC offset in hours (always negative) for US Pacific at the given instant.
fn pacific_offset_hours(utc: DateTime<Utc>) -> i64 {
    // Approximate the local wall-clock with the standard (−8) offset purely to
    // decide which side of a DST switch we're on. The switch happens at 02:00
    // local on a Sunday, so this approximation only mislabels the single
    // ambiguous hour around each transition — harmless for day-boundary math.
    let local_guess = (utc - Duration::hours(8)).naive_utc();
    let year = local_guess.year();
    let dst_start = nth_weekday(year, 3, Weekday::Sun, 2) // 2nd Sunday of March
        .and_hms_opt(2, 0, 0)
        .unwrap();
    let dst_end = nth_weekday(year, 11, Weekday::Sun, 1) // 1st Sunday of November
        .and_hms_opt(2, 0, 0)
        .unwrap();
    if local_guess >= dst_start && local_guess < dst_end {
        -7
    } else {
        -8
    }
}

/// The Nth `weekday` of `month` in `year` (n is 1-based).
fn nth_weekday(year: i32, month: u32, weekday: Weekday, n: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    let offset = (7 + weekday.num_days_from_sunday() as i64
        - first.weekday().num_days_from_sunday() as i64)
        % 7;
    let day = 1 + offset as u32 + (n - 1) * 7;
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// The Pacific-local calendar date for a UTC instant — the `quota_date` Google
/// bills against.
pub fn pacific_date(utc: DateTime<Utc>) -> NaiveDate {
    (utc + Duration::hours(pacific_offset_hours(utc))).date_naive()
}

/// The next midnight Pacific (the next quota reset) as a UTC instant.
pub fn next_reset(utc: DateTime<Utc>) -> DateTime<Utc> {
    let off = pacific_offset_hours(utc);
    let local = utc + Duration::hours(off);
    let next_local_midnight = (local.date_naive() + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap();
    // Re-evaluate the offset at the target instant: the boundary may fall on the
    // far side of a DST switch (e.g. checking late on the night the clocks move).
    let approx = Utc.from_utc_datetime(&next_local_midnight) - Duration::hours(off);
    let off2 = pacific_offset_hours(approx);
    Utc.from_utc_datetime(&next_local_midnight) - Duration::hours(off2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[test]
    fn winter_is_pst() {
        // Jan 15 2026 12:00 UTC → PST (−8) → 04:00 local, same date.
        assert_eq!(pacific_offset_hours(utc(2026, 1, 15, 12)), -8);
        assert_eq!(
            pacific_date(utc(2026, 1, 15, 12)),
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
    }

    #[test]
    fn summer_is_pdt() {
        // Jul 15 2026 12:00 UTC → PDT (−7) → 05:00 local.
        assert_eq!(pacific_offset_hours(utc(2026, 7, 15, 12)), -7);
    }

    #[test]
    fn dst_boundaries_2026() {
        // DST starts 2nd Sun of March 2026 = Mar 8; ends 1st Sun of Nov 2026 = Nov 1.
        assert_eq!(
            nth_weekday(2026, 3, Weekday::Sun, 2),
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap()
        );
        assert_eq!(
            nth_weekday(2026, 11, Weekday::Sun, 1),
            NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
        );
    }

    #[test]
    fn reset_is_a_pacific_midnight_in_utc() {
        // PST midnight = 08:00 UTC; PDT midnight = 07:00 UTC.
        let r = next_reset(utc(2026, 1, 15, 12));
        assert_eq!(r, utc(2026, 1, 16, 8));
        let r = next_reset(utc(2026, 7, 15, 12));
        assert_eq!(r, utc(2026, 7, 16, 7));
    }

    #[test]
    fn reset_is_in_the_future() {
        let now = Utc::now();
        assert!(next_reset(now) > now);
        assert!(next_reset(now) <= now + Duration::hours(25));
    }
}
