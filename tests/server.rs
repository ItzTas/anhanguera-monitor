pub mod support;

use std::collections::HashMap;
use std::sync::Arc;

use anhanguera_monitor::ava::AvaError;
use anhanguera_monitor::ava::model::CourseEnrollment;
use anhanguera_monitor::config::{Config, document_from_env};
use anhanguera_monitor::events::Kind;
use anhanguera_monitor::monitor::{CycleOutcome, Monitor, MonitorOptions};
use anhanguera_monitor::scheduler::Scheduler;
use anhanguera_monitor::server::AnhangueraMonitor;
use anhanguera_monitor::store::Store;
use chrono::{DateTime, TimeDelta, Utc};
use monitor_hub_sdk::Monitor as _;
use monitor_hub_sdk::proto::{
    CheckRequest, CheckResponse, DescribeRequest, Event, EventKind, EventsRequest, Severity, State,
    StatusRequest, StatusResponse,
};
use prost_types::Timestamp;
use tokio_postgres::{Client, NoTls};
use tonic::{Code, Request};

use support::fake_portal::FakePortal;
use support::{MovableClock, migrated_database, pending_signal, utc};

const UNREACHABLE_DATABASE: &str = "postgres://anhanguera:hunter2@127.0.0.1:1/anhanguera";
const HUB_AHEAD: &str =
    "the hub is ahead of this database: reset the hub's position for this monitor";

fn now() -> DateTime<Utc> {
    utc("2026-12-01T15:00:00Z")
}

fn options() -> MonitorOptions {
    let environment = HashMap::from([
        ("DATABASE_URL", "postgres://unused"),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "test-password"),
        ("COURSES", "Direito"),
    ]);
    let document = document_from_env(&|name| environment.get(name).map(|v| (*v).to_owned()))
        .expect("a settings document");
    let config = Config::from_document(&document, &|_| Err(std::io::Error::other("no files")))
        .expect("a valid configuration");
    MonitorOptions::from_config(&config, MovableClock::starting_at(now()).clock())
}

fn timestamp(at: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: at.timestamp(),
        nanos: i32::try_from(at.timestamp_subsec_nanos()).expect("nanos"),
    }
}

fn unprepared(url: &str, portal: FakePortal) -> Arc<Scheduler<FakePortal>> {
    let monitor = Monitor::new(Store::new(url).expect("store"), portal, options());
    Scheduler::new(Arc::new(monitor))
}

async fn prepared(url: &str, portal: FakePortal) -> Arc<Scheduler<FakePortal>> {
    let scheduler = unprepared(url, portal);
    scheduler
        .monitor()
        .prepare("device-1".to_owned())
        .await
        .expect("prepare");
    scheduler
}

fn served(scheduler: &Arc<Scheduler<FakePortal>>) -> AnhangueraMonitor<FakePortal> {
    AnhangueraMonitor::new(Arc::clone(scheduler))
}

async fn events(
    served: &AnhangueraMonitor<FakePortal>,
    after: i64,
    limit: i32,
) -> Result<Vec<Event>, tonic::Status> {
    served
        .events(Request::new(EventsRequest { after, limit }))
        .await
        .map(|response| response.into_inner().events)
}

async fn page(served: &AnhangueraMonitor<FakePortal>, after: i64, limit: i32) -> Vec<Event> {
    events(served, after, limit).await.expect("events")
}

async fn status(served: &AnhangueraMonitor<FakePortal>) -> StatusResponse {
    served
        .status(Request::new(StatusRequest {}))
        .await
        .expect("status")
        .into_inner()
}

async fn check(served: &AnhangueraMonitor<FakePortal>) -> CheckResponse {
    served
        .check(Request::new(CheckRequest {}))
        .await
        .expect("check")
        .into_inner()
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

async fn acked(url: &str) -> i64 {
    connect(url)
        .await
        .query_one("SELECT acked_seq FROM monitor_state", &[])
        .await
        .expect("acked_seq")
        .get(0)
}

async fn add_events(url: &str, count: i32) {
    connect(url)
        .await
        .execute(
            "INSERT INTO events (id, kind, urgent, title, body, url, occurred_at)
             SELECT 'event-' || n, 'payment_due', true, 'title ' || n, 'body', 'https://x/' || n, $1
             FROM generate_series(1, $2::int) AS n",
            &[&now(), &count],
        )
        .await
        .expect("insert events");
}

async fn add_event(url: &str, id: &str) {
    connect(url)
        .await
        .execute(
            "INSERT INTO events (id, kind, urgent, title, body, url, occurred_at)
             VALUES ($1, 'payment_due', true, 'title', 'body', 'https://x/late', $2)
             ON CONFLICT (id) DO NOTHING",
            &[&id, &now()],
        )
        .await
        .expect("insert an event");
}

async fn scheduled_after_three_events(
    url: &str,
) -> (
    Arc<Scheduler<FakePortal>>,
    AnhangueraMonitor<FakePortal>,
    Vec<i64>,
) {
    let scheduler = prepared(url, FakePortal::new()).await;
    add_events(url, 2).await;
    let store = scheduler.monitor().store();
    let boundary = store.last_issued_seq().await.expect("last issued seq");
    let served = served(&scheduler).with_first_read_boundary(boundary);
    add_event(url, "event-late").await;
    let stored = store.events_after(0, 100).await.expect("events");
    let all: Vec<i64> = stored.iter().map(|stored| stored.seq).collect();
    assert_eq!(all.len(), 4);
    assert_eq!(all[2], boundary);
    (scheduler, served, all)
}

fn seqs(events: &[Event]) -> Vec<i64> {
    events.iter().map(|event| event.seq).collect()
}

fn assert_increasing(seqs: &[i64]) {
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
}

#[tokio::test]
async fn describe_lists_every_kind() {
    let scheduler = unprepared(UNREACHABLE_DATABASE, FakePortal::new());

    let described = served(&scheduler)
        .describe(Request::new(DescribeRequest {}))
        .await
        .expect("describe")
        .into_inner();

    assert_eq!(described.name, "anhanguera");
    assert_eq!(described.display_name, "Anhanguera");
    assert_eq!(described.version, env!("CARGO_PKG_VERSION"));
    let expected: Vec<EventKind> = Kind::ALL
        .iter()
        .map(|kind| EventKind {
            id: kind.id().to_owned(),
            description: kind.description().to_owned(),
        })
        .collect();
    assert_eq!(expected.len(), 13);
    assert_eq!(described.kinds, expected);
}

#[tokio::test]
async fn events_carry_urgency_and_url() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakePortal::new()).await;
    let occurred = utc("2026-11-30T10:15:30.5Z");
    connect(&db.url)
        .await
        .execute(
            "INSERT INTO events (id, kind, urgent, title, body, url, occurred_at)
             VALUES ('due-1', 'payment_due', true, 'Invoice due', 'R$ 512,34', 'https://x/boleto', $1)",
            &[&occurred],
        )
        .await
        .expect("insert an urgent event");
    let served = served(&scheduler);

    let first = page(&served, 0, 0).await;

    let [welcome, due] = &first[..] else {
        panic!("expected two events: {first:?}");
    };
    assert_eq!(welcome.id, "welcome");
    assert_eq!(welcome.kind, "welcome");
    assert_eq!(welcome.severity, i32::from(Severity::Info));
    assert_eq!(welcome.url, "");
    assert_eq!(welcome.occurred_at, Some(timestamp(now())));
    assert_eq!(
        due,
        &Event {
            seq: due.seq,
            id: "due-1".to_owned(),
            kind: "payment_due".to_owned(),
            severity: Severity::Urgent.into(),
            occurred_at: Some(timestamp(occurred)),
            title: "Invoice due".to_owned(),
            body: "R$ 512,34".to_owned(),
            url: "https://x/boleto".to_owned(),
            payload: None,
        }
    );
    assert!(welcome.seq < due.seq);
    assert_eq!(acked(&db.url).await, 0);

    assert_eq!(page(&served, welcome.seq, 0).await, first[1..]);
    assert_eq!(acked(&db.url).await, welcome.seq);

    assert_eq!(page(&served, 0, 0).await, first);
    assert_eq!(acked(&db.url).await, welcome.seq);
}

#[tokio::test]
async fn events_applies_the_limit() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakePortal::new()).await;
    add_events(&db.url, 149).await;
    let served = served(&scheduler);

    let default = page(&served, 0, 0).await;
    assert_eq!(default.len(), 100);
    assert_increasing(&seqs(&default));
    assert_eq!(default[0].id, "welcome");

    let seven = page(&served, 0, 7).await;
    assert_eq!(seqs(&seven), seqs(&default)[..7]);

    assert_eq!(page(&served, 0, -3).await.len(), 100);
}

#[tokio::test]
async fn events_never_returns_more_than_500() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakePortal::new()).await;
    add_events(&db.url, 600).await;
    let served = served(&scheduler);

    let full = page(&served, 0, 1000).await;
    assert_eq!(full.len(), 500);
    assert_increasing(&seqs(&full));
    assert_eq!(page(&served, 0, i32::MAX).await, full);
    assert_eq!(page(&served, 0, 500).await, full);

    let rest = page(&served, full[499].seq, 500).await;
    assert_eq!(rest.len(), 101);
    assert!(rest[0].seq > full[499].seq);
}

#[tokio::test]
async fn events_after_the_newest_seq_answers_empty_and_never_acks_past_it() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakePortal::new()).await;
    add_events(&db.url, 2).await;
    let store = scheduler.monitor().store();
    let newest = store.last_issued_seq().await.expect("newest seq");
    let served = served(&scheduler);

    assert_eq!(page(&served, newest + 50, 0).await, []);
    assert_eq!(acked(&db.url).await, 0);

    assert_eq!(page(&served, newest, 0).await, []);
    assert_eq!(acked(&db.url).await, newest);
}

#[tokio::test]
async fn status_fails_while_the_hub_is_ahead_of_the_database() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![enrollment()]));
    let scheduler = prepared(&db.url, portal).await;
    assert_eq!(
        scheduler.monitor().run_cycle().await,
        CycleOutcome::Completed
    );
    let newest = scheduler
        .monitor()
        .store()
        .last_issued_seq()
        .await
        .expect("newest seq");
    let served = served(&scheduler);
    assert_eq!(status(&served).await.state, i32::from(State::Idle));

    assert_eq!(page(&served, newest + 5, 0).await, []);
    let ahead = status(&served).await;
    assert_eq!(ahead.state, i32::from(State::Failing));
    assert_eq!(ahead.message, HUB_AHEAD);

    assert_eq!(page(&served, newest, 0).await, []);
    let in_line = status(&served).await;
    assert_eq!(in_line.state, i32::from(State::Idle));
    assert_eq!(in_line.message, "");
}

#[tokio::test]
async fn the_hub_falling_ahead_and_back_in_line_signals_the_status() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakePortal::new()).await;
    let newest = scheduler
        .monitor()
        .store()
        .last_issued_seq()
        .await
        .expect("newest seq");
    let served = served(&scheduler);
    let mut signals = scheduler.monitor().notifier().subscribe();
    signals.latest_seq();

    assert_eq!(page(&served, newest + 5, 0).await, []);
    assert_eq!(pending_signal(&mut signals).await, Some(0));

    assert_eq!(page(&served, newest + 5, 0).await, []);
    assert_eq!(pending_signal(&mut signals).await, None);

    assert_eq!(page(&served, newest, 0).await, []);
    assert_eq!(pending_signal(&mut signals).await, Some(0));
}

#[tokio::test]
async fn the_first_read_holds_only_the_events_from_before_the_start() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let (_, served, all) = scheduled_after_three_events(&db.url).await;

    assert_eq!(seqs(&page(&served, 0, 500).await), all[..3]);
    assert_eq!(seqs(&page(&served, all[2], 500).await), all[3..]);
}

#[tokio::test]
async fn the_first_read_ends_with_its_first_short_page() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let (_, served, all) = scheduled_after_three_events(&db.url).await;

    assert_eq!(seqs(&page(&served, 0, 2).await), all[..2]);
    assert_eq!(seqs(&page(&served, all[1], 2).await), all[2..3]);
    assert_eq!(seqs(&page(&served, all[2], 2).await), all[3..]);
}

#[tokio::test]
async fn the_end_of_the_first_read_signals_the_events_it_held() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let (scheduler, served, all) = scheduled_after_three_events(&db.url).await;
    add_event(&db.url, "event-late").await;
    let issued = scheduler
        .monitor()
        .store()
        .last_issued_seq()
        .await
        .expect("last issued seq");
    assert_eq!(issued, all[3] + 1);
    let mut signals = scheduler.monitor().notifier().subscribe();
    signals.latest_seq();

    assert_eq!(seqs(&page(&served, 0, 2).await), all[..2]);
    assert_eq!(pending_signal(&mut signals).await, None);
    assert_eq!(seqs(&page(&served, all[1], 2).await), all[2..3]);
    assert_eq!(pending_signal(&mut signals).await, Some(all[3]));
    assert_eq!(seqs(&page(&served, all[2], 2).await), all[3..]);
    assert_eq!(pending_signal(&mut signals).await, None);
}

#[tokio::test]
async fn on_a_new_database_the_first_read_is_empty() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = unprepared(&db.url, FakePortal::new());
    let served = served(&scheduler).with_first_read_boundary(0);
    scheduler
        .monitor()
        .prepare("device-1".to_owned())
        .await
        .expect("prepare");

    assert_eq!(page(&served, 0, 500).await, []);
    let second = page(&served, 0, 500).await;
    let ids: Vec<&str> = second.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(ids, ["welcome"]);
}

#[tokio::test]
async fn database_failures_never_show_the_password() {
    let scheduler = unprepared(UNREACHABLE_DATABASE, FakePortal::new());
    let served = served(&scheduler);

    let failure = events(&served, 0, 0)
        .await
        .expect_err("an unreachable database gave events");
    assert_eq!(failure.code(), Code::Unavailable);
    assert_eq!(failure.message(), "the database is unavailable");

    let status = status(&served).await;
    assert_eq!(status.state, i32::from(State::Failing));
    assert_eq!(status.message, "the database is unavailable");
    assert_eq!(status.last_success_at, None);

    let accepted = check(&served).await;
    assert!(accepted.accepted);
    for shown in [format!("{failure:?}"), format!("{status:?}")] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("127.0.0.1"), "{shown}");
    }
}

#[tokio::test]
async fn status_shows_what_the_monitor_cannot_do() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let portal = FakePortal::new();
    let scheduler = prepared(&db.url, portal.clone()).await;
    let served = served(&scheduler);

    assert_eq!(
        scheduler.monitor().run_cycle().await,
        CycleOutcome::Incomplete("payments: the portal listed no course enrollments".to_owned())
    );
    let blind = status(&served).await;
    assert_eq!(blind.state, i32::from(State::Failing));
    assert_eq!(blind.message, "the portal listed no course enrollments");

    portal.set_enrollments(Ok(vec![enrollment()]));
    assert_eq!(
        scheduler.monitor().run_cycle().await,
        CycleOutcome::Completed
    );
    let seeing = status(&served).await;
    assert_eq!(seeing.state, i32::from(State::Idle));
    assert_eq!(seeing.message, "");
    assert_eq!(seeing.last_success_at, Some(timestamp(now())));
}

#[tokio::test]
async fn status_shows_the_reason_of_an_incomplete_cycle() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let portal = FakePortal::new();
    portal.set_enrollments(Err(AvaError::Unavailable("down".to_owned())));
    let scheduler = prepared(&db.url, portal).await;
    let served = served(&scheduler);
    let cycles = tokio::spawn(Arc::clone(&scheduler).run());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while scheduler.state().problem.is_none() || scheduler.state().running {
        assert!(tokio::time::Instant::now() < deadline, "the cycle ended");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cycles.abort();

    let failing = status(&served).await;
    let interval = TimeDelta::from_std(scheduler.monitor().interval()).expect("interval");

    assert_eq!(failing.state, i32::from(State::Failing));
    assert_eq!(Some(failing.message), scheduler.state().problem);
    assert_eq!(
        failing.next_check_at,
        scheduler
            .state()
            .last_finished
            .map(|finished| timestamp(finished + interval))
    );
}

fn enrollment() -> CourseEnrollment {
    CourseEnrollment {
        id: "enrollment-1".to_owned(),
        status: "ACTIVE".to_owned(),
        course_name: "Direito".to_owned(),
    }
}
