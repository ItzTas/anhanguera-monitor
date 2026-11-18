pub mod support;

use std::collections::HashMap;
use std::sync::Arc;

use anhanguera_monitor::ava::AvaError;
use anhanguera_monitor::ava::model::{
    Assessment, AssessmentKind, CalendarEvent, CourseEnrollment, InformationType, Invoice,
    InvoiceHome, InvoiceStatus, Problem, ProblemKind, Quiz, Subject,
};
use anhanguera_monitor::config::{Config, document_from_env};
use anhanguera_monitor::events::HOME_URL;
use anhanguera_monitor::monitor::{CycleOutcome, Monitor, MonitorOptions};
use anhanguera_monitor::store::{Store, StoredEvent};
use chrono::{NaiveDate, TimeDelta};
use tokio_postgres::NoTls;

use support::fake_portal::FakePortal;
use support::{MovableClock, migrated_database, next_signal, pending_signal, utc};

const REFUSE_SITE_CHANGES: &str =
    "CREATE FUNCTION refuse_site_changes() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.kind = 'site_changed' THEN
        RAISE EXCEPTION 'site changes refused';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER refuse_site_changes BEFORE INSERT ON events
FOR EACH ROW EXECUTE FUNCTION refuse_site_changes();";
const ACCEPT_SITE_CHANGES: &str = "DROP TRIGGER refuse_site_changes ON events";
const ENGINEERING: &str = "Engenharia de Software";
const LAW: &str = "Direito";
const BOLETO: &str = "https://boleto.example.test/inv-1.pdf";

fn starting_clock() -> MovableClock {
    MovableClock::starting_at(utc("2026-12-01T15:00:00Z"))
}

fn config(courses: &str) -> Config {
    let environment = HashMap::from([
        ("DATABASE_URL", "postgres://unused"),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "test-password"),
        ("COURSES", courses),
    ]);
    let document = document_from_env(&|name| environment.get(name).map(|v| (*v).to_owned()))
        .expect("a settings document");
    Config::from_document(&document, &|_| Err(std::io::Error::other("no files")))
        .expect("a valid configuration")
}

fn options(courses: &str, clock: &MovableClock) -> MonitorOptions {
    MonitorOptions::from_config(&config(courses), clock.clock())
}

async fn prepared(url: &str, portal: &FakePortal, options: MonitorOptions) -> Monitor<FakePortal> {
    let store = Store::new(url).expect("store");
    let monitor = Monitor::new(store, portal.clone(), options);
    monitor
        .prepare("device-1".to_owned())
        .await
        .expect("prepare");
    monitor
}

async fn events(monitor: &Monitor<FakePortal>) -> Vec<StoredEvent> {
    monitor.store().events_after(0, 1000).await.expect("events")
}

fn of_kind<'a>(events: &'a [StoredEvent], kind: &str) -> Vec<&'a StoredEvent> {
    events.iter().filter(|event| event.kind == kind).collect()
}

fn ids(events: &[&StoredEvent]) -> Vec<String> {
    events.iter().map(|event| event.id.clone()).collect()
}

fn day(raw: &str) -> NaiveDate {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").expect("date")
}

fn enrollment(id: &str, course_name: &str) -> CourseEnrollment {
    CourseEnrollment {
        id: id.to_owned(),
        status: "ACTIVE".to_owned(),
        course_name: course_name.to_owned(),
    }
}

fn open_invoice(id: &str, due: &str) -> Invoice {
    Invoice {
        id: id.to_owned(),
        status: InvoiceStatus::Open,
        title: "Mensalidade".to_owned(),
        amount: "R$ 512,34".to_owned(),
        due_date: day(due),
        alerts: Vec::new(),
    }
}

fn home(invoices: Vec<Invoice>) -> InvoiceHome {
    InvoiceHome {
        information_type: InformationType::PaymentsOnTime,
        billing_day: None,
        invoices,
    }
}

fn subject(id: &str, name: &str, assessments: Vec<Assessment>) -> Subject {
    Subject {
        id: id.to_owned(),
        name: name.to_owned(),
        in_progress: true,
        attendance: None,
        assessments,
    }
}

fn assignment(id: &str, due: &str) -> Assessment {
    Assessment {
        id: id.to_owned(),
        kind: AssessmentKind::Assignment,
        available: true,
        done: false,
        attempted: false,
        name: "Atividade 1".to_owned(),
        period_title: "Unidade 1".to_owned(),
        period_end: None,
        due_date: day(due),
    }
}

fn quiz(id: &str, due: &str) -> Quiz {
    Quiz {
        id: id.to_owned(),
        unit_title: "Unidade 2".to_owned(),
        name: "Questionário".to_owned(),
        due_date: day(due),
        attempted: false,
    }
}

fn financial(id: &str, end: &str) -> CalendarEvent {
    CalendarEvent {
        id: id.to_owned(),
        category: "FINANCIAL".to_owned(),
        sub_category: "RE_ENROLLMENT".to_owned(),
        title: "Rematrícula".to_owned(),
        description: "Prazo da rematrícula".to_owned(),
        start: None,
        end: Some(day(end)),
        event_date: None,
        canceled: false,
    }
}

fn site_change(operation: &'static str, field: &str) -> AvaError {
    AvaError::Schema(Problem {
        operation,
        field: field.to_owned(),
        kind: ProblemKind::Missing,
    })
}

fn unavailable() -> AvaError {
    AvaError::Unavailable("the portal answered HTTP 503".to_owned())
}

async fn run_sql(url: &str, sql: &str) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the test database");
    tokio::spawn(connection);
    client.batch_execute(sql).await.expect("run the SQL");
}

fn one_enrollment_with_an_open_invoice() -> FakePortal {
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment("ce-1", ENGINEERING)]));
    portal.set_invoices("ce-1", Ok(home(vec![open_invoice("inv-1", "2026-12-10")])));
    portal
}

#[tokio::test]
async fn first_cycle_records_welcome_and_an_open_invoice() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = one_enrollment_with_an_open_invoice();
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);

    let recorded = events(&monitor).await;
    let all: Vec<&StoredEvent> = recorded.iter().collect();
    assert_eq!(ids(&all), ["welcome", "invoice-open:inv-1"]);
    let open = &recorded[1];
    assert_eq!(open.kind, "invoice_open");
    assert!(open.urgent);
    assert_eq!(open.title, "Invoice due 10 Dec 2026");
    assert_eq!(open.occurred_at, clock.now());
    assert_eq!(monitor.status_message(), None);
}

#[tokio::test]
async fn coursework_problem_does_not_block_payments() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = one_enrollment_with_an_open_invoice();
    portal.set_subjects(
        "ce-1",
        Err(site_change("SubjectDeadlines", "data.data.subjects")),
    );
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    let outcome = monitor.run_cycle().await;

    assert!(
        matches!(outcome, CycleOutcome::Incomplete(_)),
        "{outcome:?}"
    );
    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "invoice_open")),
        ["invoice-open:inv-1"]
    );
    let changed = of_kind(&recorded, "site_changed");
    assert_eq!(
        ids(&changed),
        ["site-changed:SubjectDeadlines:data.data.subjects:missing:2026-12-01"]
    );
    assert!(changed[0].urgent);
    assert_eq!(
        changed[0].title,
        "The portal changed and the plugin needs a new version"
    );
    assert!(
        changed[0].body.contains("coursework"),
        "{}",
        changed[0].body
    );
    assert!(changed[0].body.contains("SubjectDeadlines"));
    assert!(changed[0].body.contains("data.data.subjects"));
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("SubjectDeadlines"), "{status}");

    clock.advance(TimeDelta::days(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(of_kind(&recorded, "site_changed").len(), 1);
    assert_eq!(of_kind(&recorded, "invoice_open").len(), 1);
}

#[tokio::test]
async fn site_change_is_reported_again_after_recovery() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment("ce-1", ENGINEERING)]));
    portal.set_invoices("ce-1", Err(site_change("Invoices", "billingDay")));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    monitor.run_cycle().await;
    clock.advance(TimeDelta::days(1));
    portal.set_invoices("ce-1", Ok(home(Vec::new())));

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);
    assert_eq!(monitor.status_message(), None);

    clock.advance(TimeDelta::days(1));
    portal.set_invoices("ce-1", Err(site_change("Invoices", "billingDay")));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "site_changed")),
        [
            "site-changed:Invoices:billingDay:missing:2026-12-01",
            "site-changed:Invoices:billingDay:missing:2026-12-03",
        ]
    );
}

#[tokio::test]
async fn site_change_is_retried_after_a_failed_write() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment("ce-1", ENGINEERING)]));
    portal.set_invoices("ce-1", Err(site_change("Invoices", "billingDay")));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;
    run_sql(&db.url, REFUSE_SITE_CHANGES).await;

    monitor.run_cycle().await;

    assert!(of_kind(&events(&monitor).await, "site_changed").is_empty());

    run_sql(&db.url, ACCEPT_SITE_CHANGES).await;
    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "site_changed")),
        ["site-changed:Invoices:billingDay:missing:2026-12-01"]
    );
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("Invoices:billingDay:missing"), "{status}");
}

#[tokio::test]
async fn no_enrollments_raise_the_blind_alarm() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(Vec::new()));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    let first = monitor.run_cycle().await;

    let CycleOutcome::Incomplete(reason) = first else {
        panic!("expected an incomplete cycle, got {first:?}");
    };
    assert!(reason.contains("no course enrollments"), "{reason}");
    for _ in 0..25 {
        clock.advance(TimeDelta::hours(1));
        assert!(matches!(
            monitor.run_cycle().await,
            CycleOutcome::Incomplete(_)
        ));
    }

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "monitor_blind")),
        ["monitor-blind:2026-12-01T15:00:00Z"]
    );
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("no course enrollments"), "{status}");
}

#[tokio::test]
async fn blind_alarm_after_the_failing_limit_once_per_streak() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment("ce-1", ENGINEERING)]));
    portal.set_invoices("ce-1", Err(unavailable()));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    for _ in 0..=25 {
        assert!(matches!(
            monitor.run_cycle().await,
            CycleOutcome::Incomplete(_)
        ));
        clock.advance(TimeDelta::hours(1));
    }

    let recorded = events(&monitor).await;
    let blind = of_kind(&recorded, "monitor_blind");
    assert_eq!(ids(&blind), ["monitor-blind:2026-12-01T15:00:00Z"]);
    assert!(blind[0].urgent);
    assert_eq!(blind[0].title, "Could not check payments for 24 hours");
    assert_eq!(blind[0].body, "Check the portal by hand.");
    assert_eq!(blind[0].occurred_at, utc("2026-12-02T15:00:00Z"));

    portal.set_invoices("ce-1", Ok(home(Vec::new())));
    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);
    clock.advance(TimeDelta::hours(1));
    portal.set_invoices("ce-1", Err(unavailable()));
    let second_streak = clock.now();
    for _ in 0..24 {
        monitor.run_cycle().await;
        clock.advance(TimeDelta::hours(1));
    }

    assert_eq!(of_kind(&events(&monitor).await, "monitor_blind").len(), 1);

    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "monitor_blind")),
        [
            "monitor-blind:2026-12-01T15:00:00Z".to_owned(),
            format!(
                "monitor-blind:{}",
                second_streak.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            ),
        ]
    );
}

#[tokio::test]
async fn rejected_login_reports_once_per_streak() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment("ce-1", ENGINEERING)]));
    portal.set_student_id(Err(AvaError::LoginRejected));
    portal.set_login_backoff_until(Some(utc("2026-12-01T21:00:00Z")));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    assert!(matches!(
        monitor.run_cycle().await,
        CycleOutcome::Incomplete(_)
    ));
    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    let failed = of_kind(&recorded, "login_failed");
    assert_eq!(ids(&failed), ["login-failed:2026-12-01T15:00:00Z"]);
    assert!(failed[0].urgent);
    assert!(failed[0].body.contains("6 hours"), "{}", failed[0].body);
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("password"), "{status}");
    assert!(status.contains("1 Dec 2026 18:00"), "{status}");

    portal.set_student_id(Ok("student-1".to_owned()));
    portal.set_login_backoff_until(None);
    clock.advance(TimeDelta::hours(1));
    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);
    assert_eq!(monitor.status_message(), None);

    portal.set_student_id(Err(AvaError::LoginRejected));
    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "login_failed")),
        [
            "login-failed:2026-12-01T15:00:00Z",
            "login-failed:2026-12-01T18:00:00Z",
        ]
    );
}

#[tokio::test]
async fn boleto_link_failure_keeps_the_payment_event() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = one_enrollment_with_an_open_invoice();
    portal.set_boleto(Err(unavailable()));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);

    let recorded = events(&monitor).await;
    let open = of_kind(&recorded, "invoice_open");
    assert_eq!(ids(&open), ["invoice-open:inv-1"]);
    assert_eq!(open[0].url, HOME_URL);
    assert!(!open[0].body.contains("Boleto"), "{}", open[0].body);
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("boleto link unavailable"), "{status}");

    portal.set_boleto(Ok(BOLETO.to_owned()));
    clock.advance(TimeDelta::days(2));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    let due = of_kind(&recorded, "payment_due");
    assert_eq!(ids(&due), ["payment-due:inv-1:2026-12-10:7"]);
    assert_eq!(due[0].url, BOLETO);
    assert_eq!(monitor.status_message(), None);
}

#[tokio::test]
async fn boleto_link_is_attached_when_available() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = one_enrollment_with_an_open_invoice();
    portal.set_boleto(Ok(BOLETO.to_owned()));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    let open = of_kind(&recorded, "invoice_open");
    assert_eq!(open[0].url, BOLETO);
    assert!(
        open[0].body.ends_with(&format!("\nBoleto: {BOLETO}")),
        "{}",
        open[0].body
    );
    assert!(portal.calls().contains(&"boleto_url:ce-1:inv-1".to_owned()));

    let mut without_links = options(ENGINEERING, &clock);
    without_links.boleto_link = false;
    let quiet = Monitor::new(
        Store::new(&db.url).expect("store"),
        portal.clone(),
        without_links,
    );
    portal.set_invoices(
        "ce-1",
        Ok(home(vec![
            open_invoice("inv-1", "2026-12-10"),
            open_invoice("inv-2", "2026-12-20"),
        ])),
    );
    clock.advance(TimeDelta::hours(1));
    quiet.run_cycle().await;

    let recorded = events(&monitor).await;
    let second = of_kind(&recorded, "invoice_open");
    assert_eq!(second[1].id, "invoice-open:inv-2");
    assert_eq!(second[1].url, HOME_URL);
    assert!(!portal.calls().contains(&"boleto_url:ce-1:inv-2".to_owned()));
}

#[tokio::test]
async fn missing_course_is_reported_once_and_listed_in_status() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![
        enrollment("ce-1", ENGINEERING),
        enrollment("ce-2", LAW),
    ]));
    let monitor = prepared(
        &db.url,
        &portal,
        options("Engenharia de Software, Medicina", &clock),
    )
    .await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);
    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    let missing = of_kind(&recorded, "course_not_found");
    assert_eq!(ids(&missing), ["course-not-found:medicina"]);
    assert!(missing[0].urgent);
    assert!(
        missing[0].title.contains("Medicina"),
        "{}",
        missing[0].title
    );
    assert!(missing[0].body.contains(ENGINEERING), "{}", missing[0].body);
    assert!(missing[0].body.contains(LAW), "{}", missing[0].body);
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("Medicina"), "{status}");
    assert!(status.contains(ENGINEERING), "{status}");
    assert!(status.contains(LAW), "{status}");
}

#[tokio::test]
async fn an_empty_course_list_reports_no_missing_course() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(Vec::new()));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    monitor.run_cycle().await;

    assert!(of_kind(&events(&monitor).await, "course_not_found").is_empty());
    let status = monitor.status_message().expect("a status message");
    assert!(!status.contains("not found"), "{status}");

    portal.set_enrollments(Ok(vec![enrollment("ce-2", LAW)]));
    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    let missing = of_kind(&recorded, "course_not_found");
    assert_eq!(ids(&missing), ["course-not-found:engenharia de software"]);
    assert_eq!(missing[0].occurred_at, clock.now());
    let status = monitor.status_message().expect("a status message");
    assert!(status.contains("not found"), "{status}");
}

#[tokio::test]
async fn payments_cover_enrollments_outside_courses() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![
        enrollment("ce-1", ENGINEERING),
        enrollment("ce-2", LAW),
    ]));
    portal.set_invoices("ce-2", Ok(home(vec![open_invoice("inv-2", "2026-12-10")])));
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "invoice_open")),
        ["invoice-open:inv-2"]
    );
    let calls = portal.calls();
    assert!(calls.contains(&"invoices:ce-1".to_owned()), "{calls:?}");
    assert!(calls.contains(&"subjects:ce-1".to_owned()), "{calls:?}");
    assert!(!calls.contains(&"subjects:ce-2".to_owned()), "{calls:?}");
}

#[tokio::test]
async fn overdue_reminders_keep_counting_when_two_enrollments_list_one_invoice() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = MovableClock::starting_at(utc("2026-12-11T15:00:00Z"));
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![
        enrollment("ce-1", ENGINEERING),
        enrollment("ce-2", LAW),
    ]));
    for id in ["ce-1", "ce-2"] {
        portal.set_invoices(id, Ok(home(vec![open_invoice("inv-1", "2026-12-10")])));
    }
    let monitor = prepared(&db.url, &portal, options(ENGINEERING, &clock)).await;

    for _ in 0..5 {
        assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);
        clock.advance(TimeDelta::hours(24));
    }

    let recorded = events(&monitor).await;
    assert_eq!(
        ids(&of_kind(&recorded, "invoice_open")),
        ["invoice-open:inv-1"]
    );
    let expected: Vec<String> = (0..5)
        .map(|count| format!("payment-overdue:inv-1:2026-12-10:{count}"))
        .collect();
    assert_eq!(ids(&of_kind(&recorded, "payment_overdue")), expected);
}

#[tokio::test]
async fn coursework_reminders_arrive_as_one_digest() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![
        enrollment("ce-1", ENGINEERING),
        enrollment("ce-2", LAW),
    ]));
    portal.set_subjects(
        "ce-1",
        Ok(vec![subject(
            "se-1",
            "Algoritmos",
            vec![assignment("as-1", "2026-12-04")],
        )]),
    );
    portal.set_subjects(
        "ce-2",
        Ok(vec![subject(
            "se-2",
            "Direito Civil",
            vec![assignment("as-2", "2026-12-04")],
        )]),
    );
    portal.set_quizzes("se-1", Ok(vec![quiz("qz-1", "2026-12-08")]));
    portal.set_calendar("ce-1", Ok(vec![financial("cal-1", "2026-12-04")]));
    let monitor = prepared(
        &db.url,
        &portal,
        options("Engenharia de Software, Direito", &clock),
    )
    .await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Completed);

    let recorded = events(&monitor).await;
    let digests = of_kind(&recorded, "coursework_digest");
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0].title, "Coursework: 3 deadlines");
    let body = &digests[0].body;
    assert_eq!(body.lines().count(), 3, "{body}");
    assert!(body.contains("Algoritmos"), "{body}");
    assert!(body.contains("Direito Civil"), "{body}");
    assert!(body.contains("unit quiz"), "{body}");
    assert_eq!(
        ids(&of_kind(&recorded, "calendar_financial")),
        ["calendar-financial:cal-1:2026-12-04:3"]
    );

    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    let recorded = events(&monitor).await;
    assert_eq!(of_kind(&recorded, "coursework_digest").len(), 1);
    assert_eq!(of_kind(&recorded, "calendar_financial").len(), 1);
}

#[tokio::test]
async fn every_recorded_event_signals_the_notifier() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = starting_clock();
    let portal = one_enrollment_with_an_open_invoice();
    portal.set_subjects(
        "ce-1",
        Ok(vec![subject(
            "se-1",
            "Algoritmos",
            vec![assignment("as-1", "2026-12-04")],
        )]),
    );
    let monitor = Arc::new(
        prepared(
            &db.url,
            &portal,
            options("Engenharia de Software, Medicina", &clock),
        )
        .await,
    );
    let mut signals = monitor.notifier().subscribe();
    signals.latest_seq();

    let cycle = Arc::clone(&monitor);
    tokio::spawn(async move { cycle.run_cycle().await })
        .await
        .expect("the cycle task");

    let recorded = events(&monitor).await;
    let kinds: Vec<&str> = recorded.iter().map(|event| event.kind.as_str()).collect();
    for kind in ["invoice_open", "coursework_digest", "course_not_found"] {
        assert!(kinds.contains(&kind), "{kinds:?}");
    }
    let newest = recorded.iter().map(|event| event.seq).max().expect("seq");
    assert_eq!(next_signal(&mut signals).await, newest);

    clock.advance(TimeDelta::hours(1));
    monitor.run_cycle().await;

    assert_eq!(pending_signal(&mut signals).await, None);
}
