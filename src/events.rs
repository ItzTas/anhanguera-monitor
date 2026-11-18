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
        let entries = [
            (
                Kind::Welcome,
                ("welcome", "The monitor started and is connected"),
            ),
            (
                Kind::InvoiceOpen,
                ("invoice_open", "An invoice to pay appeared on the portal"),
            ),
            (
                Kind::PaymentDue,
                ("payment_due", "An open invoice is close to its due date"),
            ),
            (
                Kind::PaymentOverdue,
                ("payment_overdue", "An invoice is past its due date"),
            ),
            (
                Kind::InvoiceClosed,
                (
                    "invoice_closed",
                    "An invoice is no longer open on the portal",
                ),
            ),
            (
                Kind::PaymentAttention,
                ("payment_attention", "The portal flags a payment problem"),
            ),
            (
                Kind::BillingDayApproaching,
                (
                    "billing_day_approaching",
                    "The monthly billing day is coming and no invoice is open",
                ),
            ),
            (
                Kind::CalendarFinancial,
                (
                    "calendar_financial",
                    "A financial date from the academic calendar is close",
                ),
            ),
            (
                Kind::CourseworkDigest,
                ("coursework_digest", "Coursework with a deadline is pending"),
            ),
            (
                Kind::SiteChanged,
                (
                    "site_changed",
                    "The portal changed in a way the monitor does not understand",
                ),
            ),
            (
                Kind::MonitorBlind,
                (
                    "monitor_blind",
                    "The monitor cannot read part of the portal",
                ),
            ),
            (
                Kind::LoginFailed,
                ("login_failed", "The portal rejected the login"),
            ),
            (
                Kind::CourseNotFound,
                (
                    "course_not_found",
                    "The configured course was not found on the portal",
                ),
            ),
        ];
        entries
            .into_iter()
            .find(|(kind, _)| *kind == self)
            .map(|(_, entry)| entry)
            .unwrap_or_default()
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
