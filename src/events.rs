use chrono::{DateTime, Utc};

pub const HOME_URL: &str = "https://www.anhanguera.com/aluno/home";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEvent {
    pub id: String,
    pub kind: &'static str,
    pub urgent: bool,
    pub title: String,
    pub body: String,
    pub url: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Welcome,
    InvoiceOpen,
    PaymentDue,
    PaymentOverdue,
    InvoiceClosed,
    PaymentAttention,
    BillingDayApproaching,
    CalendarFinancial,
    CourseworkDigest,
    SiteChanged,
    MonitorBlind,
    LoginFailed,
    CourseNotFound,
}

impl Kind {
    pub const ALL: [Kind; 13] = [
        Kind::Welcome,
        Kind::InvoiceOpen,
        Kind::PaymentDue,
        Kind::PaymentOverdue,
        Kind::InvoiceClosed,
        Kind::PaymentAttention,
        Kind::BillingDayApproaching,
        Kind::CalendarFinancial,
        Kind::CourseworkDigest,
        Kind::SiteChanged,
        Kind::MonitorBlind,
        Kind::LoginFailed,
        Kind::CourseNotFound,
    ];

    fn entry(self) -> (&'static str, &'static str) {
        const ENTRIES: [(&str, &str); 13] = [
            ("welcome", "The monitor started and is connected"),
            ("invoice_open", "An invoice to pay appeared on the portal"),
            ("payment_due", "An open invoice is close to its due date"),
            ("payment_overdue", "An invoice is past its due date"),
            (
                "invoice_closed",
                "An invoice is no longer open on the portal",
            ),
            ("payment_attention", "The portal flags a payment problem"),
            (
                "billing_day_approaching",
                "The monthly billing day is coming and no invoice is open",
            ),
            (
                "calendar_financial",
                "A financial date from the academic calendar is close",
            ),
            ("coursework_digest", "Coursework with a deadline is pending"),
            (
                "site_changed",
                "The portal changed in a way the monitor does not understand",
            ),
            (
                "monitor_blind",
                "The monitor cannot read part of the portal",
            ),
            ("login_failed", "The portal rejected the login"),
            (
                "course_not_found",
                "The configured course was not found on the portal",
            ),
        ];
        ENTRIES[self as usize]
    }

    pub fn id(self) -> &'static str {
        self.entry().0
    }

    pub fn description(self) -> &'static str {
        self.entry().1
    }
}

pub fn due_phrase(days_left: i64) -> String {
    match days_left {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        -1 => "1 day ago".to_owned(),
        n if n < 0 => format!("{} days ago", -n),
        n => format!("in {n} days"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_kind_has_a_unique_id() {
        let ids: HashSet<_> = Kind::ALL.iter().map(|k| k.id()).collect();
        assert_eq!(ids.len(), 13);
        assert!(Kind::ALL.iter().all(|k| !k.id().is_empty()));
        assert!(Kind::ALL.iter().all(|k| !k.description().is_empty()));
        assert_eq!(Kind::Welcome.id(), "welcome");
        assert_eq!(Kind::CourseNotFound.id(), "course_not_found");
    }

    #[test]
    fn due_phrase_reads_naturally() {
        let phrases: Vec<_> = [0, 1, 5, -1, -3].into_iter().map(due_phrase).collect();
        assert_eq!(
            phrases,
            ["today", "tomorrow", "in 5 days", "1 day ago", "3 days ago"]
        );
    }
}
