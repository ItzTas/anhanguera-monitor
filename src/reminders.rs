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
