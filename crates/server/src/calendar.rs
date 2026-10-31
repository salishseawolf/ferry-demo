//! Dates in Pacific time, where the ferries run: the offset from UTC
//! (with daylight time), WSF's trip days, days of the week, and the
//! holidays (with their long weekends) when traffic isn't typical.

/// WSF's trip day starts at this hour, Pacific time: its last sailings
/// after midnight belong to the day before.
const DAY_STARTS_H: i64 = 3;

pub const HOUR_MS: i64 = 3_600_000;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// Pacific time's offset from UTC (ms) at `utc_ms`: daylight time from
/// 2 AM on March's second Sunday to 2 AM on November's first.
pub fn pacific_offset_ms(utc_ms: i64) -> i64 {
    let year = civil(utc_ms.div_euclid(DAY_MS)).0;
    // Day numbers of the Sundays, and the changes' instants in UTC.
    let sunday = |month: u32, nth: i64| {
        let first = days_from_civil(year, month, 1);
        let to_sunday = (3 - first).rem_euclid(7); // 1970-01-01 was a Thursday.
        first + to_sunday + 7 * (nth - 1)
    };
    let starts = sunday(3, 2) * DAY_MS + 10 * HOUR_MS; // 2 AM PST
    let ends = sunday(11, 1) * DAY_MS + 9 * HOUR_MS; // 2 AM PDT
    if (starts..ends).contains(&utc_ms) {
        -7 * HOUR_MS
    } else {
        -8 * HOUR_MS
    }
}

/// The trip day at `utc_ms`, as `2026-10-30`.
pub fn trip_date(utc_ms: i64) -> String {
    trip_date_after(utc_ms, 0)
}

/// The trip day `days` after the one at `utc_ms`. By day number, not by
/// adding hours, so a change of clocks between them doesn't matter.
pub fn trip_date_after(utc_ms: i64, days: i64) -> String {
    let local = utc_ms + pacific_offset_ms(utc_ms) - DAY_STARTS_H * HOUR_MS;
    let (y, m, d) = civil(local.div_euclid(DAY_MS) + days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 of a date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number, as year, month and day.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The day number (days since 1970-01-01) of the Pacific date at `utc_ms`.
pub fn pacific_day(utc_ms: i64) -> i64 {
    (utc_ms + pacific_offset_ms(utc_ms)).div_euclid(DAY_MS)
}

/// Minutes past Pacific midnight at `utc_ms`.
pub fn pacific_minute(utc_ms: i64) -> i64 {
    (utc_ms + pacific_offset_ms(utc_ms)).rem_euclid(DAY_MS) / 60_000
}

/// The day of the week of a day number, Monday 0 to Sunday 6.
pub fn weekday(day: i64) -> u32 {
    (day + 3).rem_euclid(7) as u32 // 1970-01-01 was a Thursday.
}

/// Whether `minute` past midnight on day number `day` is a busy time to
/// travel, when WSF asks drivers to come 30 to 60 minutes early: weekday
/// commutes (6 to 9 AM, 3 to 7 PM), Friday afternoons (1 to 8 PM), and
/// weekends (9 AM to 7 PM). Holidays are busy all day (see `holiday`).
pub fn busy(day: i64, minute: i64) -> bool {
    let hour = minute / 60;
    match weekday(day) {
        5 | 6 => (9..19).contains(&hour),
        4 => (6..9).contains(&hour) || (13..20).contains(&hour),
        _ => (6..9).contains(&hour) || (15..19).contains(&hour),
    }
}

/// The holiday a day number falls in, if any: a US federal or Washington
/// state holiday (as observed: a Saturday's on the Friday, a Sunday's on
/// the Monday), or the long weekend around one (a Monday's from the
/// Saturday, a Friday's to the Sunday, Thanksgiving's from the Wednesday
/// to the Sunday).
pub fn holiday(day: i64) -> Option<&'static str> {
    let (year, ..) = civil(day);
    // The nth (from 1; 0 the last) given weekday of a month.
    let nth = |month: u32, wd: u32, n: i64| {
        if n == 0 {
            let next = if month == 12 {
                days_from_civil(year + 1, 1, 1)
            } else {
                days_from_civil(year, month + 1, 1)
            };
            let last = next - 1;
            last - (i64::from(weekday(last)) - i64::from(wd)).rem_euclid(7)
        } else {
            let first = days_from_civil(year, month, 1);
            first + (i64::from(wd) - i64::from(weekday(first))).rem_euclid(7) + 7 * (n - 1)
        }
    };
    let observed = |d: i64| match weekday(d) {
        5 => d - 1,
        6 => d + 1,
        _ => d,
    };
    let thanksgiving = nth(11, 3, 4);
    let holidays: [(&str, i64); 11] = [
        ("New Year's Day", observed(days_from_civil(year, 1, 1))),
        ("Martin Luther King Jr. Day", nth(1, 0, 3)),
        ("Presidents' Day", nth(2, 0, 3)),
        ("Memorial Day", nth(5, 0, 0)),
        ("Juneteenth", observed(days_from_civil(year, 6, 19))),
        ("Independence Day", observed(days_from_civil(year, 7, 4))),
        ("Labor Day", nth(9, 0, 1)),
        ("Veterans Day", observed(days_from_civil(year, 11, 11))),
        ("Thanksgiving", thanksgiving),
        ("Native American Heritage Day", thanksgiving + 1),
        ("Christmas Day", observed(days_from_civil(year, 12, 25))),
    ];
    // Next year's New Year's Day can be observed on this year's Dec 31.
    let next_new_year = observed(days_from_civil(year + 1, 1, 1));
    if day == next_new_year {
        return Some("New Year's Day");
    }
    for (name, d) in holidays {
        let (from, to) = match name {
            "Thanksgiving" => (d - 1, d + 3),
            "Native American Heritage Day" => (d, d),
            _ => match weekday(d) {
                0 => (d - 2, d),
                4 => (d, d + 2),
                _ => (d, d),
            },
        };
        if (from..=to).contains(&day) {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_trip_day_in_pacific_time() {
        // 2026-10-30 16:00 UTC is 9 AM PDT.
        let utc = days_from_civil(2026, 10, 30) * DAY_MS + 16 * HOUR_MS;
        assert_eq!(pacific_offset_ms(utc), -7 * HOUR_MS);
        assert_eq!(trip_date(utc), "2026-10-30");
        // 1 AM PDT on the 31st still sails on the 30th's schedule; 3 AM doesn't.
        let one_am = days_from_civil(2026, 10, 31) * DAY_MS + 8 * HOUR_MS;
        assert_eq!(trip_date(one_am), "2026-10-30");
        assert_eq!(trip_date(one_am + 2 * HOUR_MS), "2026-10-31");
        // Standard time from 2 AM PDT on Sunday, November 1, 2026.
        let change = days_from_civil(2026, 11, 1) * DAY_MS + 9 * HOUR_MS;
        assert_eq!(pacific_offset_ms(change - 1), -7 * HOUR_MS);
        assert_eq!(pacific_offset_ms(change), -8 * HOUR_MS);
        // Daylight time from 2 AM PST on Sunday, March 8, 2026.
        let spring = days_from_civil(2026, 3, 8) * DAY_MS + 10 * HOUR_MS;
        assert_eq!(pacific_offset_ms(spring - 1), -8 * HOUR_MS);
        assert_eq!(pacific_offset_ms(spring), -7 * HOUR_MS);
        assert_eq!(civil(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(civil(0), (1970, 1, 1));
    }

    #[test]
    fn knows_holidays_and_their_long_weekends() {
        let day = |m, d| days_from_civil(2026, m, d);
        assert_eq!(weekday(day(10, 30)), 4); // a Friday
        assert_eq!(holiday(day(10, 30)), None);
        // Thanksgiving, Thursday Nov 26, 2026: Wednesday to Sunday.
        for d in 25..=29 {
            assert!(holiday(day(11, d)).is_some(), "Nov {d}");
        }
        assert_eq!(holiday(day(11, 24)), None);
        assert_eq!(holiday(day(11, 26)), Some("Thanksgiving"));
        // Labor Day, Monday Sep 7: the weekend before too.
        assert_eq!(holiday(day(9, 5)), Some("Labor Day"));
        assert_eq!(holiday(day(9, 8)), None);
        // July 4, 2026 is a Saturday: observed Friday the 3rd, with the
        // weekend.
        assert_eq!(holiday(day(7, 3)), Some("Independence Day"));
        assert_eq!(holiday(day(7, 5)), Some("Independence Day"));
        // Memorial Day: the last Monday in May, May 25.
        assert_eq!(holiday(day(5, 25)), Some("Memorial Day"));
        assert_eq!(holiday(day(5, 18)), None);
        assert_eq!(pacific_minute(day(10, 30) * DAY_MS + 16 * HOUR_MS), 9 * 60);
        // Friday Oct 30: busy at 5 PM, not at 11 AM; a Tuesday at 2 PM
        // isn't, Saturday at noon is.
        assert!(busy(day(10, 30), 17 * 60) && !busy(day(10, 30), 11 * 60));
        assert!(!busy(day(10, 27), 14 * 60) && busy(day(10, 31), 12 * 60));
    }

    #[test]
    fn counts_trip_days_across_a_change_of_clocks() {
        // Sunday 2026-11-01 has 25 hours. Saturday 10 PM PDT:
        let saturday_night = 1_793_509_200_000;
        assert_eq!(trip_date(saturday_night), "2026-10-31");
        assert_eq!(trip_date_after(saturday_night, 1), "2026-11-01");
        assert_eq!(trip_date_after(saturday_night, 2), "2026-11-02");
        // An hour before the trip day ends (2 AM PST on the 2nd).
        assert_eq!(trip_date(1_793_613_600_000), "2026-11-01");
        assert_eq!(trip_date_after(1_793_613_600_000, 1), "2026-11-02");
    }
}
