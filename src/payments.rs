use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Datelike, Months, NaiveDate, SecondsFormat, TimeDelta, Utc};

use crate::ava::model::{InformationType, Invoice, InvoiceHome, InvoiceStatus};
use crate::config::BillingDayNet;
use crate::dates::{days_left, format_day, local_today, month_day_clamped};
use crate::events::{HOME_URL, Kind, NewEvent, due_phrase};
use crate::reminders::{DueReminder, ReminderRule, due_threshold, in_reminder_hours};
use crate::store::{EnrollmentState, KnownInvoice, RecordBatch};
use crate::text::one_line;

const SITE_TEXT_MAX: usize = 300;

#[derive(Clone, Debug)]
pub struct PaymentOptions {
    pub reminders: ReminderRule,
    pub overdue_repeat: Option<Duration>,
    pub billing_day_net: BillingDayNet,
    pub check_interval: Duration,
}

pub struct PaymentInput<'a> {
    pub course_enrollment_id: &'a str,
    pub home: &'a InvoiceHome,
    pub known: &'a [KnownInvoice],
    pub enrollment: &'a EnrollmentState,
    pub lead_times: &'a [i64],
    pub sent: &'a HashSet<String>,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedPayment {
    pub event: NewEvent,
    pub boleto_for: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PaymentPlan {
    pub events: Vec<PlannedPayment>,
    pub batch: RecordBatch,
}

pub fn plan_payments(input: &PaymentInput, options: &PaymentOptions) -> PaymentPlan {
    let ctx = Context {
        input,
        options,
        today: local_today(input.now, options.reminders.tz),
    };
    let listed: HashSet<&str> = input
        .home
        .invoices
        .iter()
        .map(|invoice| invoice.id.as_str())
        .collect();
    let mut plan = PaymentPlan::default();
    for invoice in &input.home.invoices {
        let known = input.known.iter().find(|known| known.id == invoice.id);
        let (step, record) = follow_listed(&ctx, invoice, known);
        plan.keep(step, record, known);
    }
    let gone = input
        .known
        .iter()
        .filter(|known| !listed.contains(known.id.as_str()));
    for known in gone {
        let mut record = known.clone();
        let step = close(&ctx, &mut record);
        plan.keep(step, record, Some(known));
    }
    plan.add(attention(&ctx));
    plan.add(billing_day(&ctx));
    plan.batch.enrollments.push(enrollment_row(&ctx));
    plan
}

pub fn learned_net_days(lead_times: &[i64]) -> (u32, bool) {
    let learned = lead_times.iter().min().filter(|_| lead_times.len() >= 2);
    let Some(shortest) = learned else {
        return (3, false);
    };
    (shortest.saturating_sub(2).clamp(1, 15) as u32, true)
}

struct Context<'a> {
    input: &'a PaymentInput<'a>,
    options: &'a PaymentOptions,
    today: NaiveDate,
}

#[derive(Default)]
struct Step {
    events: Vec<PlannedPayment>,
    sent_keys: Vec<String>,
}

impl Step {
    fn event(planned: PlannedPayment) -> Step {
        Step {
            events: vec![planned],
            sent_keys: Vec::new(),
        }
    }

    fn then(mut self, next: Step) -> Step {
        self.events.extend(next.events);
        self.sent_keys.extend(next.sent_keys);
        self
    }
}

impl PaymentPlan {
    fn add(&mut self, step: Step) {
        self.events.extend(step.events);
        self.batch.sent_keys.extend(step.sent_keys);
    }

    fn keep(&mut self, step: Step, record: KnownInvoice, known: Option<&KnownInvoice>) {
        self.add(step);
        if known == Some(&record) {
            return;
        }
        self.batch.invoices.push(record);
    }
}

fn follow_listed(
    ctx: &Context,
    invoice: &Invoice,
    known: Option<&KnownInvoice>,
) -> (Step, KnownInvoice) {
    let Some(known) = known else {
        return first_seen(ctx, invoice);
    };
    let mut record = refreshed(known, invoice);
    let step = follow_known(ctx, invoice, &mut record);
    (step, record)
}

fn first_seen(ctx: &Context, invoice: &Invoice) -> (Step, KnownInvoice) {
    let mut record = KnownInvoice {
        id: invoice.id.clone(),
        course_enrollment_id: ctx.input.course_enrollment_id.to_owned(),
        due_date: invoice.due_date,
        status: invoice.status.as_str().to_owned(),
        first_seen_at: ctx.input.now,
        lead_days: None,
        announced: false,
        closed_at: None,
        overdue_count: 0,
        last_overdue_at: None,
    };
    if invoice.status.is_settled() {
        return (Step::default(), record);
    }
    record.lead_days = witnessed(ctx).then(|| days_left(ctx.today, invoice.due_date));
    let step = announce(ctx, invoice, &mut record);
    (step, record)
}

fn refreshed(known: &KnownInvoice, invoice: &Invoice) -> KnownInvoice {
    let record = KnownInvoice {
        status: invoice.status.as_str().to_owned(),
        ..known.clone()
    };
    if record.due_date == invoice.due_date {
        return record;
    }
    KnownInvoice {
        due_date: invoice.due_date,
        overdue_count: 0,
        last_overdue_at: None,
        ..record
    }
}

fn follow_known(ctx: &Context, invoice: &Invoice, record: &mut KnownInvoice) -> Step {
    if invoice.status.is_settled() {
        return close(ctx, record);
    }
    if !record.announced {
        return announce(ctx, invoice, record);
    }
    record.closed_at = None;
    if is_overdue(ctx, invoice) {
        return overdue(ctx, invoice, record);
    }
    payment_due(ctx, invoice)
}

fn witnessed(ctx: &Context) -> bool {
    let window = delta(ctx.options.check_interval.saturating_mul(2));
    ctx.input
        .enrollment
        .last_payments_check_at
        .is_some_and(|last| ctx.input.now - last < window)
}

fn announce(ctx: &Context, invoice: &Invoice, record: &mut KnownInvoice) -> Step {
    record.announced = true;
    let open = invoice_event(
        ctx,
        Kind::InvoiceOpen,
        format!("invoice-open:{}", invoice.id),
        due_title(invoice.due_date),
        invoice,
    );
    let covered = crossed_reminder(ctx, invoice)
        .map(|due| due.mark_sent)
        .unwrap_or_default();
    let step = Step {
        events: vec![open],
        sent_keys: covered,
    };
    if !is_overdue(ctx, invoice) {
        return step;
    }
    step.then(overdue(ctx, invoice, record))
}

fn payment_due(ctx: &Context, invoice: &Invoice) -> Step {
    if !in_reminder_hours(&ctx.options.reminders, ctx.input.now) {
        return Step::default();
    }
    let Some(due) = crossed_reminder(ctx, invoice) else {
        return Step::default();
    };
    let event = invoice_event(
        ctx,
        Kind::PaymentDue,
        due_key(invoice, due.threshold),
        due_title(invoice.due_date),
        invoice,
    );
    Step {
        events: vec![event],
        sent_keys: due.mark_sent,
    }
}

fn crossed_reminder(ctx: &Context, invoice: &Invoice) -> Option<DueReminder> {
    due_threshold(
        &ctx.options.reminders,
        invoice.due_date,
        ctx.input.now,
        ctx.input.sent,
        &|threshold| due_key(invoice, threshold),
    )
}

fn due_key(invoice: &Invoice, threshold: u32) -> String {
    format!(
        "payment-due:{}:{}:{threshold}",
        invoice.id, invoice.due_date
    )
}

fn is_overdue(ctx: &Context, invoice: &Invoice) -> bool {
    invoice.status == InvoiceStatus::Overdue || ctx.today > invoice.due_date
}

fn overdue(ctx: &Context, invoice: &Invoice, record: &mut KnownInvoice) -> Step {
    if !overdue_reminder_due(ctx, record) {
        return Step::default();
    }
    let id = format!(
        "payment-overdue:{}:{}:{}",
        invoice.id, invoice.due_date, record.overdue_count
    );
    record.overdue_count += 1;
    record.last_overdue_at = Some(ctx.input.now);
    let title = format!("{} is overdue", due_title(invoice.due_date));
    Step::event(invoice_event(ctx, Kind::PaymentOverdue, id, title, invoice))
}

fn overdue_reminder_due(ctx: &Context, record: &KnownInvoice) -> bool {
    if !in_reminder_hours(&ctx.options.reminders, ctx.input.now) {
        return false;
    }
    if record.overdue_count == 0 {
        return true;
    }
    let Some(repeat) = ctx.options.overdue_repeat else {
        return false;
    };
    record
        .last_overdue_at
        .is_some_and(|last| ctx.input.now - last >= delta(repeat))
}

fn close(ctx: &Context, record: &mut KnownInvoice) -> Step {
    if !record.announced || record.closed_at.is_some() {
        return Step::default();
    }
    if !in_reminder_hours(&ctx.options.reminders, ctx.input.now) {
        return Step::default();
    }
    record.closed_at = Some(ctx.input.now);
    let day = format_day(record.due_date);
    let closed_at = ctx.input.now.to_rfc3339_opts(SecondsFormat::Secs, true);
    Step::event(notice(
        ctx,
        Kind::InvoiceClosed,
        format!(
            "invoice-closed:{}:{}:{closed_at}",
            record.id, record.due_date
        ),
        format!("Invoice due {day} is no longer open"),
        format!(
            "Invoice due {day} is no longer open on the portal. If you did not pay it, check the Financeiro page."
        ),
    ))
}

fn attention(ctx: &Context) -> Step {
    let current = ctx.input.home.information_type;
    let raw = current.as_str();
    let unchanged = ctx.input.enrollment.information_type.as_deref() == Some(raw);
    if unchanged || !current.needs_attention() {
        return Step::default();
    }
    let words = attention_words(current);
    Step::event(notice(
        ctx,
        Kind::PaymentAttention,
        format!(
            "payment-attention:{}:{raw}:{}",
            ctx.input.course_enrollment_id, ctx.today
        ),
        format!("Payment attention: {words}"),
        format!(
            "The portal now reports {words} ({raw}) for this enrollment. Check the Financeiro page."
        ),
    ))
}

fn attention_words(value: InformationType) -> &'static str {
    const WORDS: [(InformationType, &str); 5] = [
        (InformationType::DocumentBlock, "a document block"),
        (InformationType::GenericBlock, "a general block"),
        (
            InformationType::ReEnrollmentNotPaid,
            "an unpaid re-enrollment",
        ),
        (InformationType::Negotiation, "a negotiation"),
        (
            InformationType::ReEnrollmentNegotiation,
            "a re-enrollment negotiation",
        ),
    ];
    WORDS
        .iter()
        .find(|(key, _)| *key == value)
        .map_or(value.as_str(), |&(_, words)| words)
}

fn enrollment_row(ctx: &Context) -> (String, EnrollmentState) {
    let state = EnrollmentState {
        information_type: Some(ctx.input.home.information_type.as_str().to_owned()),
        last_payments_check_at: Some(ctx.input.now),
    };
    (ctx.input.course_enrollment_id.to_owned(), state)
}

fn billing_day(ctx: &Context) -> Step {
    let (Some(days), Some(billing_day)) = (net_days(ctx), ctx.input.home.billing_day) else {
        return Step::default();
    };
    if !in_reminder_hours(&ctx.options.reminders, ctx.input.now) {
        return Step::default();
    }
    let due = next_due_date(ctx.today, billing_day);
    if month_has_invoice(ctx, due) {
        return Step::default();
    }
    let key = format!(
        "billing-day:{}:{}",
        ctx.input.course_enrollment_id,
        due.format("%Y-%m")
    );
    let rule = ReminderRule {
        thresholds: vec![days],
        hour: ctx.options.reminders.hour,
        tz: ctx.options.reminders.tz,
    };
    if due_threshold(&rule, due, ctx.input.now, ctx.input.sent, &|_| key.clone()).is_none() {
        return Step::default();
    }
    let event = notice(
        ctx,
        Kind::BillingDayApproaching,
        key.clone(),
        format!("No invoice yet for {}", format_day(due)),
        format!(
            "Due day {} is {} and the portal shows no invoice. Check the Financeiro page.",
            due.day(),
            due_phrase(days_left(ctx.today, due))
        ),
    );
    Step {
        events: vec![event],
        sent_keys: vec![key],
    }
}

fn net_days(ctx: &Context) -> Option<u32> {
    match ctx.options.billing_day_net {
        BillingDayNet::Auto => Some(learned_net_days(ctx.input.lead_times).0),
        BillingDayNet::Fixed(days) => Some(days),
        BillingDayNet::Off => None,
    }
}

fn next_due_date(today: NaiveDate, billing_day: u32) -> NaiveDate {
    let this_month = month_day_clamped(today.year(), today.month(), billing_day);
    if this_month >= today {
        return this_month;
    }
    let next = today.checked_add_months(Months::new(1)).unwrap_or(today);
    month_day_clamped(next.year(), next.month(), billing_day)
}

fn month_has_invoice(ctx: &Context, due: NaiveDate) -> bool {
    let in_month = |date: NaiveDate| date.year() == due.year() && date.month() == due.month();
    ctx.input.known.iter().any(|known| in_month(known.due_date))
        || ctx
            .input
            .home
            .invoices
            .iter()
            .any(|invoice| in_month(invoice.due_date))
}

fn due_title(date: NaiveDate) -> String {
    format!("Invoice due {}", format_day(date))
}

fn invoice_body(ctx: &Context, invoice: &Invoice) -> String {
    let due = invoice.due_date;
    let heading = format!(
        "{} — {}",
        one_line(&invoice.title, SITE_TEXT_MAX),
        one_line(&invoice.amount, SITE_TEXT_MAX)
    );
    let when = format!(
        "Due {} ({})",
        format_day(due),
        due_phrase(days_left(ctx.today, due))
    );
    let alerts = invoice
        .alerts
        .iter()
        .map(|alert| one_line(alert, SITE_TEXT_MAX));
    [heading, when]
        .into_iter()
        .chain(alerts)
        .collect::<Vec<_>>()
        .join("\n")
}

fn invoice_event(
    ctx: &Context,
    kind: Kind,
    id: String,
    title: String,
    invoice: &Invoice,
) -> PlannedPayment {
    PlannedPayment {
        event: new_event(ctx, kind, id, title, invoice_body(ctx, invoice)),
        boleto_for: Some(invoice.id.clone()),
    }
}

fn notice(ctx: &Context, kind: Kind, id: String, title: String, body: String) -> PlannedPayment {
    PlannedPayment {
        event: new_event(ctx, kind, id, title, body),
        boleto_for: None,
    }
}

fn new_event(ctx: &Context, kind: Kind, id: String, title: String, body: String) -> NewEvent {
    NewEvent {
        id,
        kind: kind.id(),
        urgent: kind != Kind::InvoiceClosed,
        title,
        body,
        url: HOME_URL.to_owned(),
        occurred_at: ctx.input.now,
    }
}

fn delta(duration: Duration) -> TimeDelta {
    TimeDelta::from_std(duration).unwrap_or(TimeDelta::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Sao_Paulo;

    const CE: &str = "ce-1";
    const WITH_LINK: [&str; 3] = ["invoice_open", "payment_due", "payment_overdue"];

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("timestamp")
            .to_utc()
    }

    fn day(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").expect("date")
    }

    fn options() -> PaymentOptions {
        PaymentOptions {
            reminders: ReminderRule {
                thresholds: vec![7, 3, 1, 0],
                hour: 9,
                tz: Sao_Paulo,
            },
            overdue_repeat: Some(Duration::from_secs(24 * 3600)),
            billing_day_net: BillingDayNet::Auto,
            check_interval: Duration::from_secs(3600),
        }
    }

    struct Fixture(InvoiceHome);

    fn fixture() -> Fixture {
        Fixture(InvoiceHome {
            information_type: InformationType::PaymentsOnTime,
            billing_day: Some(10),
            invoices: Vec::new(),
        })
    }

    impl Fixture {
        fn invoice(mut self, id: &str, status: InvoiceStatus, due: &str) -> Fixture {
            self.0.invoices.push(Invoice {
                id: id.to_owned(),
                status,
                title: "Dezembro".to_owned(),
                amount: "R$ 123,45".to_owned(),
                due_date: day(due),
                alerts: Vec::new(),
            });
            self
        }

        fn alerts(mut self, alerts: &[&str]) -> Fixture {
            if let Some(last) = self.0.invoices.last_mut() {
                last.alerts = alerts.iter().map(|alert| (*alert).to_owned()).collect();
            }
            self
        }

        fn information_type(mut self, value: InformationType) -> Fixture {
            self.0.information_type = value;
            self
        }

        fn billing_day(mut self, billing_day: Option<u32>) -> Fixture {
            self.0.billing_day = billing_day;
            self
        }

        fn build(self) -> InvoiceHome {
            self.0
        }
    }

    fn open(due: &str) -> InvoiceHome {
        fixture().invoice("inv-1", InvoiceStatus::Open, due).build()
    }

    fn empty() -> InvoiceHome {
        fixture().build()
    }

    struct World {
        options: PaymentOptions,
        known: Vec<KnownInvoice>,
        enrollment: EnrollmentState,
        sent: HashSet<String>,
        lead_times: Vec<i64>,
    }

    impl World {
        fn new() -> World {
            World::with(options())
        }

        fn with(options: PaymentOptions) -> World {
            World {
                options,
                known: Vec::new(),
                enrollment: EnrollmentState::default(),
                sent: HashSet::new(),
                lead_times: Vec::new(),
            }
        }

        fn plan(&self, home: &InvoiceHome, now: &str) -> PaymentPlan {
            let input = PaymentInput {
                course_enrollment_id: CE,
                home,
                known: &self.known,
                enrollment: &self.enrollment,
                lead_times: &self.lead_times,
                sent: &self.sent,
                now: at(now),
            };
            let plan = plan_payments(&input, &self.options);
            assert!(plan.batch.events.is_empty());
            for planned in &plan.events {
                let event = &planned.event;
                assert!(event.title.chars().count() <= 80, "{}", event.title);
                assert_eq!(event.url, HOME_URL);
                assert_eq!(event.occurred_at, at(now));
                assert_eq!(event.urgent, event.kind != "invoice_closed", "{}", event.id);
                assert_eq!(
                    planned.boleto_for.is_some(),
                    WITH_LINK.contains(&event.kind),
                    "{}",
                    event.id
                );
            }
            plan
        }

        fn cycle(&mut self, home: &InvoiceHome, now: &str) -> Vec<PlannedPayment> {
            let plan = self.plan(home, now);
            self.apply(plan.batch);
            plan.events
        }

        fn ids(&mut self, home: &InvoiceHome, now: &str) -> Vec<String> {
            self.cycle(home, now)
                .into_iter()
                .map(|planned| planned.event.id)
                .collect()
        }

        fn apply(&mut self, batch: RecordBatch) {
            self.sent.extend(batch.sent_keys);
            for invoice in batch.invoices {
                self.known.retain(|known| known.id != invoice.id);
                self.known.push(invoice);
            }
            for (id, state) in batch.enrollments {
                assert_eq!(id, CE);
                self.enrollment = state;
            }
        }

        fn invoice(&self, id: &str) -> &KnownInvoice {
            self.known
                .iter()
                .find(|known| known.id == id)
                .expect("known invoice")
        }
    }

    fn ids(planned: &[PlannedPayment]) -> Vec<&str> {
        planned
            .iter()
            .map(|planned| planned.event.id.as_str())
            .collect()
    }

    #[test]
    fn new_invoice_is_announced_once() {
        let home = open("2026-12-10");
        let mut world = World::new();
        let first = world.cycle(&home, "2026-11-20T15:00:00Z");
        assert_eq!(ids(&first), ["invoice-open:inv-1"]);
        assert_eq!(first[0].event.kind, "invoice_open");
        assert_eq!(first[0].event.title, "Invoice due 10 Dec 2026");
        assert_eq!(first[0].boleto_for.as_deref(), Some("inv-1"));
        assert!(world.invoice("inv-1").announced);
        assert!(world.cycle(&home, "2026-11-20T15:00:00Z").is_empty());
        assert!(world.cycle(&home, "2026-11-21T15:00:00Z").is_empty());
    }

    #[test]
    fn invoice_text_names_title_amount_due_date_and_alerts() {
        let long = "x".repeat(400);
        let home = fixture()
            .invoice("inv-1", InvoiceStatus::Open, "2026-12-10")
            .alerts(&["Pague até\n  o vencimento", long.as_str()])
            .build();
        let events = World::new().cycle(&home, "2026-11-20T15:00:00Z");
        assert_eq!(
            events[0].event.body,
            format!(
                "Dezembro — R$ 123,45\nDue 10 Dec 2026 (in 20 days)\nPague até o vencimento\n{}…",
                "x".repeat(299)
            )
        );
    }

    #[test]
    fn reminders_follow_thresholds_at_the_reminder_hour() {
        let home = open("2026-12-10");
        let mut world = World::new();
        world.cycle(&home, "2026-11-20T15:00:00Z");
        assert!(world.cycle(&home, "2026-12-03T11:00:00Z").is_empty());
        let due = world.cycle(&home, "2026-12-03T12:00:00Z");
        assert_eq!(ids(&due), ["payment-due:inv-1:2026-12-10:7"]);
        assert_eq!(due[0].event.kind, "payment_due");
        assert_eq!(due[0].event.title, "Invoice due 10 Dec 2026");
        assert_eq!(
            due[0].event.body,
            "Dezembro — R$ 123,45\nDue 10 Dec 2026 (in 7 days)"
        );
        assert_eq!(due[0].boleto_for.as_deref(), Some("inv-1"));
        assert!(world.cycle(&home, "2026-12-03T13:00:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-07T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:3"]
        );
        assert_eq!(
            world.ids(&home, "2026-12-09T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:1"]
        );
        let today = world.cycle(&home, "2026-12-10T12:00:00Z");
        assert_eq!(ids(&today), ["payment-due:inv-1:2026-12-10:0"]);
        assert_eq!(
            today[0].event.body,
            "Dezembro — R$ 123,45\nDue 10 Dec 2026 (today)"
        );
    }

    #[test]
    fn open_event_covers_already_crossed_thresholds() {
        let home = open("2026-12-10");
        let mut world = World::new();
        assert_eq!(
            world.ids(&home, "2026-12-08T15:00:00Z"),
            ["invoice-open:inv-1"]
        );
        assert!(world.sent.contains("payment-due:inv-1:2026-12-10:7"));
        assert!(world.sent.contains("payment-due:inv-1:2026-12-10:3"));
        assert!(!world.sent.contains("payment-due:inv-1:2026-12-10:1"));
        assert!(world.cycle(&home, "2026-12-08T20:00:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-09T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:1"]
        );
    }

    #[test]
    fn late_start_after_due_date_sends_open_and_one_overdue() {
        let home = fixture()
            .invoice("inv-1", InvoiceStatus::Overdue, "2026-12-10")
            .build();
        let mut world = World::new();
        let events = world.cycle(&home, "2026-12-14T15:00:00Z");
        assert_eq!(
            ids(&events),
            ["invoice-open:inv-1", "payment-overdue:inv-1:2026-12-10:0"]
        );
        assert_eq!(events[1].event.kind, "payment_overdue");
        assert_eq!(events[1].event.title, "Invoice due 10 Dec 2026 is overdue");
        assert_eq!(
            events[1].event.body,
            "Dezembro — R$ 123,45\nDue 10 Dec 2026 (4 days ago)"
        );
        assert_eq!(events[1].boleto_for.as_deref(), Some("inv-1"));
        assert!(world.cycle(&home, "2026-12-14T16:00:00Z").is_empty());

        let known = open("2026-12-10");
        let mut down = World::new();
        down.cycle(&known, "2026-11-20T15:00:00Z");
        assert_eq!(
            down.ids(&known, "2026-12-14T15:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:0"]
        );
        assert!(down.cycle(&known, "2026-12-14T16:00:00Z").is_empty());
    }

    #[test]
    fn overdue_repeats_every_interval_and_stops_when_paid() {
        let home = open("2026-12-10");
        let paid = fixture()
            .invoice("inv-1", InvoiceStatus::Paid, "2026-12-10")
            .build();
        let mut world = World::new();
        assert_eq!(
            world.ids(&home, "2026-12-11T15:00:00Z"),
            ["invoice-open:inv-1", "payment-overdue:inv-1:2026-12-10:0"]
        );
        assert!(world.cycle(&home, "2026-12-12T12:00:00Z").is_empty());
        assert!(world.cycle(&home, "2026-12-12T14:59:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-12T15:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:1"]
        );
        assert!(world.cycle(&home, "2026-12-12T20:00:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-13T15:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:2"]
        );
        let closed = world.cycle(&paid, "2026-12-13T18:00:00Z");
        assert_eq!(
            ids(&closed),
            ["invoice-closed:inv-1:2026-12-10:2026-12-13T18:00:00Z"]
        );
        assert_eq!(closed[0].event.kind, "invoice_closed");
        assert_eq!(
            closed[0].event.title,
            "Invoice due 10 Dec 2026 is no longer open"
        );
        assert_eq!(
            closed[0].event.body,
            "Invoice due 10 Dec 2026 is no longer open on the portal. If you did not pay it, check the Financeiro page."
        );
        assert!(world.cycle(&paid, "2026-12-14T15:00:00Z").is_empty());
        assert!(world.cycle(&empty(), "2026-12-15T15:00:00Z").is_empty());
    }

    #[test]
    fn overdue_repeat_zero_sends_a_single_overdue() {
        let home = open("2026-12-10");
        let mut world = World::with(PaymentOptions {
            overdue_repeat: None,
            ..options()
        });
        assert_eq!(
            world.ids(&home, "2026-12-11T15:00:00Z"),
            ["invoice-open:inv-1", "payment-overdue:inv-1:2026-12-10:0"]
        );
        for now in ["2026-12-12T15:00:00Z", "2026-12-20T15:00:00Z"] {
            assert!(world.cycle(&home, now).is_empty(), "{now}");
        }
        assert_eq!(world.invoice("inv-1").overdue_count, 1);
    }

    #[test]
    fn open_status_with_past_due_date_counts_as_overdue() {
        let home = open("2026-12-10");
        let mut world = World::new();
        world.cycle(&home, "2026-11-20T15:00:00Z");
        assert_eq!(
            world.ids(&home, "2026-12-10T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:0"]
        );
        assert!(world.cycle(&home, "2026-12-11T11:59:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-11T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:0"]
        );
    }

    #[test]
    fn overdue_status_replaces_due_reminders() {
        let open_home = open("2026-12-10");
        let overdue = fixture()
            .invoice("inv-1", InvoiceStatus::Overdue, "2026-12-10")
            .build();
        let mut world = World::new();
        world.cycle(&open_home, "2026-11-20T15:00:00Z");
        assert_eq!(
            world.ids(&overdue, "2026-12-09T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:0"]
        );
        assert_eq!(
            world.ids(&overdue, "2026-12-10T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:1"]
        );
    }

    #[test]
    fn reappearing_invoice_resumes_without_new_open_event() {
        let home = open("2026-12-10");
        let mut world = World::new();
        assert_eq!(
            world.ids(&home, "2026-11-20T15:00:00Z"),
            ["invoice-open:inv-1"]
        );
        assert_eq!(
            world.ids(&empty(), "2026-11-25T15:00:00Z"),
            ["invoice-closed:inv-1:2026-12-10:2026-11-25T15:00:00Z"]
        );
        assert_eq!(
            world.invoice("inv-1").closed_at,
            Some(at("2026-11-25T15:00:00Z"))
        );
        assert!(world.cycle(&home, "2026-11-28T15:00:00Z").is_empty());
        assert_eq!(world.invoice("inv-1").closed_at, None);
        assert!(world.invoice("inv-1").announced);
        assert_eq!(
            world.ids(&home, "2026-12-03T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:7"]
        );
        assert_eq!(
            world.ids(&empty(), "2026-12-04T15:00:00Z"),
            ["invoice-closed:inv-1:2026-12-10:2026-12-04T15:00:00Z"]
        );

        let paid = fixture()
            .invoice("inv-1", InvoiceStatus::Paid, "2026-12-10")
            .build();
        let mut settled = World::new();
        settled.cycle(&home, "2026-11-20T15:00:00Z");
        assert_eq!(
            settled.ids(&paid, "2026-11-25T15:00:00Z"),
            ["invoice-closed:inv-1:2026-12-10:2026-11-25T15:00:00Z"]
        );
        assert_eq!(
            settled.ids(&home, "2026-12-08T15:00:00Z"),
            ["payment-due:inv-1:2026-12-10:3"]
        );
        assert_eq!(settled.invoice("inv-1").closed_at, None);
        assert_eq!(
            settled.ids(&paid, "2026-12-08T18:00:00Z"),
            ["invoice-closed:inv-1:2026-12-10:2026-12-08T18:00:00Z"]
        );
    }

    #[test]
    fn open_at_night_covers_crossed_thresholds_until_the_next_one() {
        let home = open("2026-12-10");
        let mut world = World::new();
        assert_eq!(
            world.ids(&home, "2026-12-08T06:00:00Z"),
            ["invoice-open:inv-1"]
        );
        assert!(world.sent.contains("payment-due:inv-1:2026-12-10:3"));
        for now in ["2026-12-08T12:00:00Z", "2026-12-08T20:00:00Z"] {
            assert!(world.cycle(&home, now).is_empty(), "{now}");
        }
        assert_eq!(
            world.ids(&home, "2026-12-09T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:1"]
        );
    }

    #[test]
    fn invoice_closed_at_night_waits_for_the_reminder_hour() {
        let home = open("2026-12-10");
        let paid = fixture()
            .invoice("inv-1", InvoiceStatus::Paid, "2026-12-10")
            .build();
        for after in [paid, empty()] {
            let mut world = World::new();
            world.cycle(&home, "2026-12-05T15:00:00Z");
            for now in ["2026-12-06T06:00:00Z", "2026-12-06T11:59:00Z"] {
                assert!(world.cycle(&after, now).is_empty(), "{now}");
                assert_eq!(world.invoice("inv-1").closed_at, None, "{now}");
            }
            assert_eq!(
                world.ids(&after, "2026-12-06T12:00:00Z"),
                ["invoice-closed:inv-1:2026-12-10:2026-12-06T12:00:00Z"]
            );
            assert!(world.cycle(&after, "2026-12-06T18:00:00Z").is_empty());
        }
    }

    #[test]
    fn overdue_found_at_night_waits_for_the_reminder_hour() {
        let home = open("2026-12-10");
        let mut world = World::new();
        assert_eq!(
            world.ids(&home, "2026-12-14T06:00:00Z"),
            ["invoice-open:inv-1"]
        );
        assert!(world.cycle(&home, "2026-12-14T11:59:00Z").is_empty());
        assert_eq!(
            world.ids(&home, "2026-12-14T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:0"]
        );
    }

    #[test]
    fn reminder_crossed_while_away_waits_for_the_reminder_hour() {
        let home = open("2026-12-10");
        let mut down = World::new();
        down.cycle(&home, "2026-11-20T15:00:00Z");
        assert!(down.cycle(&home, "2026-12-08T06:00:00Z").is_empty());
        assert!(down.cycle(&home, "2026-12-08T11:59:00Z").is_empty());
        assert_eq!(
            down.ids(&home, "2026-12-08T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:3"]
        );

        let mut away = World::new();
        away.cycle(&home, "2026-11-20T15:00:00Z");
        away.cycle(&empty(), "2026-11-25T15:00:00Z");
        assert!(away.cycle(&home, "2026-12-08T06:00:00Z").is_empty());
        assert_eq!(away.invoice("inv-1").closed_at, None);
        assert_eq!(
            away.ids(&home, "2026-12-08T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:3"]
        );
    }

    #[test]
    fn billing_day_net_crossed_while_down_waits_for_the_reminder_hour() {
        let mut world = World::new();
        assert!(world.cycle(&empty(), "2026-12-08T06:00:00Z").is_empty());
        assert!(!world.sent.contains("billing-day:ce-1:2026-12"));
        assert_eq!(
            world.ids(&empty(), "2026-12-08T12:00:00Z"),
            ["billing-day:ce-1:2026-12"]
        );
    }

    #[test]
    fn attention_information_type_is_reported_once_per_change() {
        let negotiation = fixture()
            .information_type(InformationType::Negotiation)
            .billing_day(None)
            .build();
        let on_time = fixture().billing_day(None).build();
        let mut world = World::new();
        let events = world.cycle(&negotiation, "2026-12-01T15:00:00Z");
        assert_eq!(
            ids(&events),
            ["payment-attention:ce-1:NEGOTIATION:2026-12-01"]
        );
        assert_eq!(events[0].event.kind, "payment_attention");
        assert_eq!(events[0].event.title, "Payment attention: a negotiation");
        assert_eq!(
            events[0].event.body,
            "The portal now reports a negotiation (NEGOTIATION) for this enrollment. Check the Financeiro page."
        );
        assert!(world.cycle(&negotiation, "2026-12-02T15:00:00Z").is_empty());
        assert!(world.cycle(&on_time, "2026-12-03T15:00:00Z").is_empty());
        assert_eq!(
            world.enrollment,
            EnrollmentState {
                information_type: Some("PAYMENTS_ON_TIME".to_owned()),
                last_payments_check_at: Some(at("2026-12-03T15:00:00Z")),
            }
        );
        assert_eq!(
            world.ids(&negotiation, "2026-12-04T15:00:00Z"),
            ["payment-attention:ce-1:NEGOTIATION:2026-12-04"]
        );

        let none = fixture()
            .information_type(InformationType::None)
            .billing_day(None)
            .build();
        assert!(
            World::new()
                .cycle(&on_time, "2026-12-01T15:00:00Z")
                .is_empty()
        );
        assert!(World::new().cycle(&none, "2026-12-01T15:00:00Z").is_empty());
    }

    #[test]
    fn attention_names_each_value_in_plain_words() {
        let cases = [
            (InformationType::DocumentBlock, "a document block"),
            (InformationType::GenericBlock, "a general block"),
            (
                InformationType::ReEnrollmentNotPaid,
                "an unpaid re-enrollment",
            ),
            (InformationType::Negotiation, "a negotiation"),
            (
                InformationType::ReEnrollmentNegotiation,
                "a re-enrollment negotiation",
            ),
        ];
        for (value, words) in cases {
            let home = fixture().information_type(value).billing_day(None).build();
            let events = World::new().cycle(&home, "2026-12-01T15:00:00Z");
            assert_eq!(events.len(), 1, "{value:?}");
            assert_eq!(events[0].event.title, format!("Payment attention: {words}"));
        }
    }

    #[test]
    fn billing_day_net_fires_when_no_invoice_for_the_month() {
        let mut world = World::new();
        assert!(world.cycle(&empty(), "2026-12-07T11:59:00Z").is_empty());
        let events = world.cycle(&empty(), "2026-12-07T12:00:00Z");
        assert_eq!(ids(&events), ["billing-day:ce-1:2026-12"]);
        assert_eq!(events[0].event.kind, "billing_day_approaching");
        assert_eq!(events[0].event.title, "No invoice yet for 10 Dec 2026");
        assert_eq!(
            events[0].event.body,
            "Due day 10 is in 3 days and the portal shows no invoice. Check the Financeiro page."
        );
        assert!(world.sent.contains("billing-day:ce-1:2026-12"));
        assert!(world.cycle(&empty(), "2026-12-08T12:00:00Z").is_empty());
    }

    #[test]
    fn billing_day_net_is_quiet_when_the_month_already_had_an_invoice() {
        let mut world = World::new();
        world.cycle(&open("2026-12-10"), "2026-11-20T15:00:00Z");
        world.cycle(&empty(), "2026-11-25T15:00:00Z");
        assert!(world.invoice("inv-1").closed_at.is_some());
        assert!(world.cycle(&empty(), "2026-12-07T12:00:00Z").is_empty());
        assert!(world.cycle(&empty(), "2026-12-09T12:00:00Z").is_empty());
    }

    #[test]
    fn billing_day_net_uses_learned_lead_times() {
        let mut world = World::new();
        world.lead_times = vec![10, 12];
        assert!(world.cycle(&empty(), "2026-12-02T11:59:00Z").is_empty());
        let events = world.cycle(&empty(), "2026-12-02T12:00:00Z");
        assert_eq!(ids(&events), ["billing-day:ce-1:2026-12"]);
        assert_eq!(
            events[0].event.body,
            "Due day 10 is in 8 days and the portal shows no invoice. Check the Financeiro page."
        );
    }

    #[test]
    fn billing_day_net_can_be_fixed_or_off() {
        let mut fixed = World::with(PaymentOptions {
            billing_day_net: BillingDayNet::Fixed(5),
            ..options()
        });
        assert!(fixed.cycle(&empty(), "2026-12-05T11:59:00Z").is_empty());
        assert_eq!(
            fixed.ids(&empty(), "2026-12-05T12:00:00Z"),
            ["billing-day:ce-1:2026-12"]
        );

        let mut off = World::with(PaymentOptions {
            billing_day_net: BillingDayNet::Off,
            ..options()
        });
        assert!(off.cycle(&empty(), "2026-12-09T12:00:00Z").is_empty());

        let no_day = fixture().billing_day(None).build();
        assert!(
            World::new()
                .cycle(&no_day, "2026-12-09T12:00:00Z")
                .is_empty()
        );
    }

    #[test]
    fn billing_day_clamps_to_month_end() {
        let november = fixture().billing_day(Some(31)).build();
        let mut world = World::new();
        assert!(world.cycle(&november, "2026-11-27T11:59:00Z").is_empty());
        let events = world.cycle(&november, "2026-11-27T12:00:00Z");
        assert_eq!(ids(&events), ["billing-day:ce-1:2026-11"]);
        assert_eq!(events[0].event.title, "No invoice yet for 30 Nov 2026");
        assert_eq!(
            events[0].event.body,
            "Due day 30 is in 3 days and the portal shows no invoice. Check the Financeiro page."
        );

        let february = fixture().billing_day(Some(30)).build();
        let mut world = World::new();
        assert!(world.cycle(&february, "2027-02-25T11:59:00Z").is_empty());
        let events = world.cycle(&february, "2027-02-25T12:00:00Z");
        assert_eq!(ids(&events), ["billing-day:ce-1:2027-02"]);
        assert_eq!(events[0].event.title, "No invoice yet for 28 Feb 2027");
    }

    #[test]
    fn billing_day_net_looks_into_the_next_month() {
        let home = fixture().billing_day(Some(1)).build();
        let mut world = World::new();
        assert!(world.cycle(&home, "2026-12-29T11:59:00Z").is_empty());
        let events = world.cycle(&home, "2026-12-29T12:00:00Z");
        assert_eq!(ids(&events), ["billing-day:ce-1:2027-01"]);
        assert_eq!(events[0].event.title, "No invoice yet for 1 Jan 2027");
    }

    #[test]
    fn lead_time_is_only_recorded_when_the_appearance_was_witnessed() {
        let home = open("2026-12-10");

        let mut cold = World::new();
        cold.cycle(&home, "2026-11-20T15:00:00Z");
        assert_eq!(cold.invoice("inv-1").lead_days, None);

        let mut witnessed = World::new();
        witnessed.cycle(&empty(), "2026-11-20T15:00:00Z");
        witnessed.cycle(&home, "2026-11-20T16:59:00Z");
        let invoice = witnessed.invoice("inv-1");
        assert_eq!(invoice.lead_days, Some(20));
        assert_eq!(invoice.first_seen_at, at("2026-11-20T16:59:00Z"));

        let mut stale = World::new();
        stale.cycle(&empty(), "2026-11-20T15:00:00Z");
        stale.cycle(&home, "2026-11-20T17:00:00Z");
        assert_eq!(stale.invoice("inv-1").lead_days, None);

        let mut later = World::new();
        later.cycle(&empty(), "2026-11-20T15:00:00Z");
        later.cycle(&home, "2026-11-20T16:00:00Z");
        later.cycle(&home, "2026-11-20T17:00:00Z");
        assert_eq!(later.invoice("inv-1").lead_days, Some(20));
        assert_eq!(
            later.invoice("inv-1").first_seen_at,
            at("2026-11-20T16:00:00Z")
        );
    }

    #[test]
    fn learned_net_days_bounds() {
        assert_eq!(learned_net_days(&[]), (3, false));
        assert_eq!(learned_net_days(&[12]), (3, false));
        assert_eq!(learned_net_days(&[10, 12]), (8, true));
        assert_eq!(learned_net_days(&[20, 20]), (15, true));
        assert_eq!(learned_net_days(&[2, 9]), (1, true));
    }

    #[test]
    fn settled_invoice_seen_first_is_remembered_without_an_event() {
        let paid = fixture()
            .invoice("inv-1", InvoiceStatus::Paid, "2026-12-10")
            .build();
        let mut world = World::new();
        world.cycle(&empty(), "2026-12-05T14:00:00Z");
        assert!(world.cycle(&paid, "2026-12-05T15:00:00Z").is_empty());
        assert_eq!(
            world.invoice("inv-1"),
            &KnownInvoice {
                id: "inv-1".to_owned(),
                course_enrollment_id: CE.to_owned(),
                due_date: day("2026-12-10"),
                status: "PAID".to_owned(),
                first_seen_at: at("2026-12-05T15:00:00Z"),
                lead_days: None,
                announced: false,
                closed_at: None,
                overdue_count: 0,
                last_overdue_at: None,
            }
        );
        assert!(world.cycle(&empty(), "2026-12-06T15:00:00Z").is_empty());
        assert!(world.cycle(&empty(), "2026-12-07T12:00:00Z").is_empty());
    }

    #[test]
    fn unannounced_invoice_is_announced_when_it_turns_to_pay() {
        let paid = fixture()
            .invoice("inv-1", InvoiceStatus::Paid, "2026-12-10")
            .build();
        let home = open("2026-12-10");
        let mut world = World::new();
        world.cycle(&paid, "2026-12-05T15:00:00Z");
        world.cycle(&paid, "2026-12-08T14:00:00Z");
        assert_eq!(
            world.ids(&home, "2026-12-08T15:00:00Z"),
            ["invoice-open:inv-1"]
        );
        assert!(world.sent.contains("payment-due:inv-1:2026-12-10:3"));
        let invoice = world.invoice("inv-1");
        assert!(invoice.announced);
        assert_eq!(invoice.lead_days, None);
        assert_eq!(invoice.first_seen_at, at("2026-12-05T15:00:00Z"));
        assert_eq!(
            world.ids(&home, "2026-12-09T12:00:00Z"),
            ["payment-due:inv-1:2026-12-10:1"]
        );

        let mut late = World::new();
        late.cycle(&paid, "2026-12-05T15:00:00Z");
        assert_eq!(
            late.ids(&home, "2026-12-12T15:00:00Z"),
            ["invoice-open:inv-1", "payment-overdue:inv-1:2026-12-10:0"]
        );
    }

    #[test]
    fn due_date_change_restarts_reminders_and_overdue_count() {
        let home = open("2026-12-10");
        let moved = open("2026-12-20");
        let mut world = World::new();
        world.cycle(&home, "2026-11-20T15:00:00Z");
        world.cycle(&home, "2026-12-10T12:00:00Z");
        assert_eq!(
            world.ids(&home, "2026-12-11T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-10:0"]
        );
        assert!(world.cycle(&moved, "2026-12-11T15:00:00Z").is_empty());
        let invoice = world.invoice("inv-1");
        assert_eq!(invoice.due_date, day("2026-12-20"));
        assert_eq!(invoice.overdue_count, 0);
        assert_eq!(invoice.last_overdue_at, None);
        assert_eq!(
            world.ids(&moved, "2026-12-13T12:00:00Z"),
            ["payment-due:inv-1:2026-12-20:7"]
        );
        world.cycle(&moved, "2026-12-20T12:00:00Z");
        assert_eq!(
            world.ids(&moved, "2026-12-21T12:00:00Z"),
            ["payment-overdue:inv-1:2026-12-20:0"]
        );
    }
}
