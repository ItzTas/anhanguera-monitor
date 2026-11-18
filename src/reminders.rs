use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::dates::{local_today, reminder_instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderRule {
    pub thresholds: Vec<u32>,
    pub hour: u32,
    pub tz: Tz,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueReminder {
    pub threshold: u32,
    pub mark_sent: Vec<String>,
}

pub fn due_threshold(
    rule: &ReminderRule,
    date: NaiveDate,
    now: DateTime<Utc>,
    sent: &HashSet<String>,
    key: &dyn Fn(u32) -> String,
) -> Option<DueReminder> {
    if local_today(now, rule.tz) > date {
        return None;
    }
    let crossed: Vec<u32> = rule
        .thresholds
        .iter()
        .copied()
        .filter(|threshold| reminder_instant(date, *threshold, rule.hour, rule.tz) <= now)
        .collect();
    let closest = crossed.iter().copied().min()?;
    if sent.contains(&key(closest)) {
        return None;
    }
    Some(DueReminder {
        threshold: closest,
        mark_sent: crossed.into_iter().map(key).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Sao_Paulo;

    fn rule() -> ReminderRule {
        ReminderRule {
            thresholds: vec![7, 3, 1, 0],
            hour: 9,
            tz: Sao_Paulo,
        }
    }

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("timestamp")
            .to_utc()
    }

    fn key(t: u32) -> String {
        format!("k{t}")
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 12, 10).expect("date")
    }

    #[test]
    fn nothing_before_the_first_threshold() {
        let now = at("2026-12-02T20:00:00Z");
        assert_eq!(
            due_threshold(&rule(), date(), now, &HashSet::new(), &key),
            None
        );
    }

    #[test]
    fn closest_crossed_threshold_wins_and_marks_the_rest() {
        let now = at("2026-12-09T13:00:00Z");
        let due = due_threshold(&rule(), date(), now, &HashSet::new(), &key).expect("due");
        assert_eq!(due.threshold, 1);
        assert_eq!(due.mark_sent, ["k7", "k3", "k1"]);
    }

    #[test]
    fn waits_for_the_reminder_hour() {
        let only3 = ReminderRule {
            thresholds: vec![3],
            ..rule()
        };
        let none = HashSet::new();
        let before = at("2026-12-07T11:59:00Z");
        let on = at("2026-12-07T12:00:00Z");
        assert_eq!(due_threshold(&only3, date(), before, &none, &key), None);
        assert!(due_threshold(&only3, date(), on, &none, &key).is_some());
    }

    #[test]
    fn already_sent_closest_threshold_sends_nothing() {
        let sent = HashSet::from(["k1".to_owned()]);
        let now = at("2026-12-09T13:00:00Z");
        assert_eq!(due_threshold(&rule(), date(), now, &sent, &key), None);
    }

    #[test]
    fn nothing_after_the_date() {
        let now = at("2026-12-11T13:00:00Z");
        assert_eq!(
            due_threshold(&rule(), date(), now, &HashSet::new(), &key),
            None
        );
    }
}
