use chrono::{
    DateTime, Datelike, Days, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;

pub fn parse_portal_date(raw: &str, tz: Tz) -> Option<NaiveDate> {
    let raw = raw.trim();
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Some(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%d/%m/%Y") {
        return Some(date);
    }
    let instant = DateTime::parse_from_rfc3339(raw).ok()?;
    Some(instant.with_timezone(&tz).date_naive())
}

pub fn parse_period_end(period_dates: &str) -> Option<NaiveDate> {
    let (_, end) = period_dates.trim().split_once('-')?;
    NaiveDate::parse_from_str(end.trim(), "%d/%m/%Y").ok()
}

pub fn local_today(now: DateTime<Utc>, tz: Tz) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

pub fn reminder_instant(date: NaiveDate, days_before: u32, hour: u32, tz: Tz) -> DateTime<Utc> {
    let day = date
        .checked_sub_days(Days::new(u64::from(days_before)))
        .unwrap_or(date);
    let time = NaiveTime::from_hms_opt(hour, 0, 0).unwrap_or(NaiveTime::MIN);
    let mut local = day.and_time(time);
    loop {
        match tz.from_local_datetime(&local) {
            LocalResult::Single(instant) => return instant.with_timezone(&Utc),
            LocalResult::Ambiguous(earliest, _) => return earliest.with_timezone(&Utc),
            LocalResult::None => local += Duration::minutes(15),
        }
    }
}

pub fn days_left(today: NaiveDate, date: NaiveDate) -> i64 {
    (date - today).num_days()
}

pub fn format_day(date: NaiveDate) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {}",
        date.day(),
        MONTHS[date.month0() as usize],
        date.year()
    )
}

pub fn month_day_clamped(year: i32, month: u32, day: u32) -> NaiveDate {
    (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::America::Sao_Paulo;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn portal_dates_resolve_to_local_dates() {
        let cases = [
            ("2026-12-10", Some(d(2026, 12, 10))),
            ("10/12/2026", Some(d(2026, 12, 10))),
            ("2026-12-10T02:59:59Z", Some(d(2026, 12, 9))),
            ("2026-12-09T23:59:59-03:00", Some(d(2026, 12, 9))),
            ("2026-12-10T12:00:00.000Z", Some(d(2026, 12, 10))),
            ("", None),
            ("10-12-2026", None),
            ("2026/12/10", None),
        ];
        for (raw, want) in cases {
            assert_eq!(parse_portal_date(raw, Sao_Paulo), want, "{raw:?}");
        }
    }

    #[test]
    fn period_end_is_the_second_date() {
        assert_eq!(
            parse_period_end(" 03/11/2026 - 12/12/2026 "),
            Some(d(2026, 12, 12))
        );
        assert_eq!(parse_period_end("12/12/2026"), None);
        assert_eq!(parse_period_end("x"), None);
    }

    #[test]
    fn reminder_instant_is_local_hour_days_before() {
        let want = Utc.with_ymd_and_hms(2026, 12, 7, 12, 0, 0).unwrap();
        assert_eq!(reminder_instant(d(2026, 12, 10), 3, 9, Sao_Paulo), want);
    }

    #[test]
    fn format_day_is_unambiguous_english() {
        assert_eq!(format_day(d(2026, 12, 10)), "10 Dec 2026");
        assert_eq!(format_day(d(2027, 1, 5)), "5 Jan 2027");
    }

    #[test]
    fn month_day_clamped_handles_short_months() {
        assert_eq!(month_day_clamped(2026, 11, 31), d(2026, 11, 30));
        assert_eq!(month_day_clamped(2027, 2, 30), d(2027, 2, 28));
    }

    #[test]
    fn days_left_and_today() {
        assert_eq!(days_left(d(2026, 12, 7), d(2026, 12, 10)), 3);
        let now = Utc.with_ymd_and_hms(2026, 12, 10, 2, 0, 0).unwrap();
        assert_eq!(local_today(now, Sao_Paulo), d(2026, 12, 9));
    }
}
