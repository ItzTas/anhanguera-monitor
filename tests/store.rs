pub mod support;

use std::error::Error;
use std::time::{Duration, Instant};

use anhanguera_monitor::events::NewEvent;
use anhanguera_monitor::migration::{DROP_GOOSE_DB_VERSION, GOOSE_SCHEMA};
use anhanguera_monitor::store::{
    EnrollmentState, KnownInvoice, LOCK_CHECK_EVERY, LOCK_CHECK_LIMIT, RecordBatch, Store,
    StoreError, acquire_writer_lock,
};
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use tokio::net::TcpListener;
use tokio::time::timeout;

use support::proxy::PausingProxy;
use support::schema::table_names;
use support::{admin, end_the_writer_locks_connection, next_signal, pending_signal};

const PASSWORD: &str = "hunter2";
const TABLES_ONLY: &str = "anhanguera_tables_only";
const TODAYS_TABLES: [&str; 7] = [
    "enrollments",
    "events",
    "invoices",
    "monitor_state",
    "problems",
    "seaql_migrations",
    "sent_keys",
];
const LOCK_TIMED_OUT: &str = "connecting to the database for the writer lock timed out";
const REFUSE_EVENTS_AT_COMMIT: &str = "CREATE FUNCTION refuse_events() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused at commit'; END $$;
CREATE CONSTRAINT TRIGGER refuse_at_commit AFTER INSERT ON events DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION refuse_events();";

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z")
        .expect("timestamp")
        .to_utc()
}

fn event(id: &str) -> NewEvent {
    NewEvent {
        id: id.to_owned(),
        kind: "reminder",
        urgent: false,
        title: format!("title {id}"),
        body: format!("body {id}"),
        url: format!("https://example.test/{id}"),
        occurred_at: start(),
    }
}

fn batch_of(events: Vec<NewEvent>) -> RecordBatch {
    RecordBatch {
        events,
        sent_keys: Vec::new(),
        invoices: Vec::new(),
        enrollments: Vec::new(),
    }
}

fn invoice(id: &str, lead_days: i64, first_seen_at: DateTime<Utc>) -> KnownInvoice {
    KnownInvoice {
        id: id.to_owned(),
        course_enrollment_id: "enrollment-1".to_owned(),
        due_date: NaiveDate::from_ymd_opt(2026, 12, 10).expect("date"),
        status: "open".to_owned(),
        first_seen_at,
        lead_days: Some(lead_days),
        announced: false,
        closed_at: None,
        overdue_count: 0,
        last_overdue_at: None,
    }
}

async fn initialized(url: &str) -> Store {
    let store = Store::new(url).expect("store");
    store
        .initialize("device-a".to_owned(), start())
        .await
        .expect("initialize");
    store
}

#[tokio::test]
async fn initialize_creates_state_and_welcome_once() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    assert_eq!(store.device_id().await.expect("device id"), None);
    let first = store
        .initialize("device-a".to_owned(), start())
        .await
        .expect("first");
    let second = store
        .initialize("device-b".to_owned(), start())
        .await
        .expect("second");
    assert_eq!((first.as_str(), second.as_str()), ("device-a", "device-a"));
    assert_eq!(
        store.device_id().await.expect("device id").as_deref(),
        Some("device-a")
    );
    let events = store.events_after(0, 100).await.expect("events");
    assert_eq!(events.len(), 1);
    let welcome = &events[0];
    assert_eq!(
        (welcome.id.as_str(), welcome.kind.as_str(), welcome.urgent),
        ("welcome", "welcome", false)
    );
    assert_eq!(welcome.title, "Anhanguera monitor is running");
}

#[tokio::test]
async fn record_is_idempotent_on_event_id() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let batch = batch_of(vec![event("a")]);
    assert!(store.record(&batch).await.expect("first").is_some());
    assert_eq!(store.record(&batch).await.expect("second"), None);
    let events = store.events_after(0, 100).await.expect("events");
    assert_eq!(events.iter().filter(|stored| stored.id == "a").count(), 1);
}

#[tokio::test]
async fn record_returns_highest_seq_and_events_come_back_in_order() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let highest = store
        .record(&batch_of(vec![event("a"), event("b"), event("c")]))
        .await
        .expect("record")
        .expect("seq");
    let events = store.events_after(0, 100).await.expect("events");
    let ids: Vec<&str> = events.iter().map(|stored| stored.id.as_str()).collect();
    assert_eq!(ids, ["welcome", "a", "b", "c"]);
    assert_eq!(events.last().expect("last").seq, highest);
    assert_eq!(store.last_issued_seq().await.expect("issued"), highest);
    let after_first = store.events_after(events[1].seq, 1).await.expect("after");
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].id, "b");
}

#[tokio::test]
async fn record_signals_the_seq_it_committed() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let mut signals = store.notifier().subscribe();
    signals.latest_seq();
    // The welcome is already stored: no row, but its insert spends a sequence value.
    let batch = batch_of(vec![event("a"), event("b"), event("welcome")]);
    let keys_only = RecordBatch {
        sent_keys: vec!["key-1".to_owned()],
        ..RecordBatch::default()
    };

    let recorded = store.record(&batch).await.expect("record");
    let signalled = next_signal(&mut signals).await;
    let issued = store.last_issued_seq().await.expect("issued");
    let again = store.record(&batch).await.expect("record again");
    let after_the_conflicts = pending_signal(&mut signals).await;
    store.record(&keys_only).await.expect("record keys");
    let after_the_keys = pending_signal(&mut signals).await;

    let stored = store.events_after(0, 100).await.expect("events");
    assert_eq!(recorded, stored.last().map(|stored| stored.seq));
    assert_eq!(Some(signalled), recorded);
    assert_eq!(issued, signalled + 1);
    assert_eq!(again, None);
    assert_eq!(after_the_conflicts, None);
    assert_eq!(after_the_keys, None);
}

#[tokio::test]
async fn a_record_refused_at_commit_signals_nothing() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    store
        .record(&batch_of(vec![event("committed")]))
        .await
        .expect("record");
    let (client, connection) = tokio_postgres::connect(&db.url, tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(connection);
    client
        .batch_execute(REFUSE_EVENTS_AT_COMMIT)
        .await
        .expect("a trigger that refuses every event at commit");
    let before = store.events_after(0, 100).await.expect("events");
    let mut signals = store.notifier().subscribe();
    let seen = signals.latest_seq();

    let refused = store.record(&batch_of(vec![event("refused")])).await;
    let signalled = pending_signal(&mut signals).await;

    assert!(
        matches!(refused, Err(StoreError::Database(_))),
        "{refused:?}"
    );
    assert_eq!(signalled, None);
    assert_eq!(signals.latest_seq(), seen);
    assert_eq!(store.events_after(0, 100).await.expect("events"), before);
}

#[tokio::test]
async fn record_persists_keys_invoices_and_enrollments() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let mut stored = invoice("inv-1", 5, start());
    let batch = RecordBatch {
        events: Vec::new(),
        sent_keys: vec!["key-1".to_owned()],
        invoices: vec![stored.clone()],
        enrollments: vec![(
            "enrollment-1".to_owned(),
            EnrollmentState {
                information_type: Some("kind".to_owned()),
                last_payments_check_at: Some(start()),
            },
        )],
    };
    assert_eq!(store.record(&batch).await.expect("record"), None);
    assert!(store.sent_keys().await.expect("keys").contains("key-1"));
    assert_eq!(
        store.known_invoices("enrollment-1").await.expect("known"),
        vec![stored.clone()]
    );
    assert_eq!(
        store.enrollment("enrollment-1").await.expect("enrollment"),
        EnrollmentState {
            information_type: Some("kind".to_owned()),
            last_payments_check_at: Some(start()),
        }
    );
    assert_eq!(
        store.enrollment("missing").await.expect("default"),
        EnrollmentState::default()
    );
    stored.status = "paid".to_owned();
    stored.announced = true;
    stored.overdue_count = 2;
    let update = RecordBatch {
        events: Vec::new(),
        sent_keys: vec!["key-1".to_owned()],
        invoices: vec![stored.clone()],
        enrollments: Vec::new(),
    };
    store.record(&update).await.expect("update");
    assert_eq!(
        store.known_invoices("enrollment-1").await.expect("known"),
        vec![stored]
    );
}

#[tokio::test]
async fn a_second_record_replaces_every_stored_column() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let first = invoice("inv-1", 5, start());
    let enrollment = |information_type: &str, at: DateTime<Utc>| {
        (
            "enrollment-1".to_owned(),
            EnrollmentState {
                information_type: Some(information_type.to_owned()),
                last_payments_check_at: Some(at),
            },
        )
    };
    let batch = RecordBatch {
        invoices: vec![first],
        enrollments: vec![enrollment("PAYMENTS_ON_TIME", start())],
        ..RecordBatch::default()
    };
    store.record(&batch).await.expect("record");
    let later = start() + TimeDelta::days(3);
    let replaced = KnownInvoice {
        id: "inv-1".to_owned(),
        course_enrollment_id: "enrollment-1".to_owned(),
        due_date: NaiveDate::from_ymd_opt(2026, 12, 20).expect("date"),
        status: "PAID".to_owned(),
        first_seen_at: later,
        lead_days: None,
        announced: true,
        closed_at: Some(later),
        overdue_count: 3,
        last_overdue_at: Some(later),
    };
    let update = RecordBatch {
        invoices: vec![replaced.clone()],
        enrollments: vec![enrollment("LATE_PAYMENTS", later)],
        ..RecordBatch::default()
    };

    store.record(&update).await.expect("record again");

    assert_eq!(
        store.known_invoices("enrollment-1").await.expect("known"),
        vec![replaced]
    );
    assert_eq!(
        store.enrollment("enrollment-1").await.expect("enrollment"),
        enrollment("LATE_PAYMENTS", later).1
    );
}

#[tokio::test]
async fn open_problems_come_back_in_signature_order() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    for signature in ["b", "c", "a"] {
        store
            .open_problem(signature, "payments", start())
            .await
            .expect("open");
    }
    assert_eq!(store.open_problems().await.expect("open"), ["a", "b", "c"]);
}

#[tokio::test]
async fn record_refuses_a_database_that_was_never_initialized() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");

    let refused = store.record(&batch_of(vec![event("a")])).await;

    assert!(
        matches!(refused, Err(StoreError::NotInitialized)),
        "{refused:?}"
    );
    assert!(store.events_after(0, 100).await.expect("events").is_empty());
}

#[tokio::test]
async fn an_invoice_id_under_two_enrollments_keeps_one_row_per_enrollment() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let mut first = invoice("inv-1", 5, start());
    let second = KnownInvoice {
        course_enrollment_id: "enrollment-2".to_owned(),
        lead_days: Some(9),
        announced: true,
        overdue_count: 3,
        last_overdue_at: Some(start()),
        ..first.clone()
    };
    let batch = RecordBatch {
        invoices: vec![first.clone(), second.clone()],
        ..RecordBatch::default()
    };
    store.record(&batch).await.expect("record");
    first.overdue_count = 1;
    let update = RecordBatch {
        invoices: vec![first.clone()],
        ..RecordBatch::default()
    };
    store.record(&update).await.expect("update");

    assert_eq!(
        store.known_invoices("enrollment-1").await.expect("first"),
        vec![first]
    );
    assert_eq!(
        store.known_invoices("enrollment-2").await.expect("second"),
        vec![second]
    );
}

#[tokio::test]
async fn lead_times_are_newest_first_and_capped_at_six() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let invoices = (1..=8)
        .map(|lead| {
            invoice(
                &format!("inv-{lead}"),
                lead,
                start() + TimeDelta::days(lead),
            )
        })
        .collect();
    let batch = RecordBatch {
        events: Vec::new(),
        sent_keys: Vec::new(),
        invoices,
        enrollments: Vec::new(),
    };
    store.record(&batch).await.expect("record");
    assert_eq!(
        store.lead_times("enrollment-1").await.expect("leads"),
        [8, 7, 6, 5, 4, 3]
    );
}

#[tokio::test]
async fn open_problem_reports_new_only_once_until_cleared() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    assert!(
        store
            .open_problem("sig", "payments", start())
            .await
            .expect("first")
    );
    assert!(
        !store
            .open_problem("sig", "payments", start())
            .await
            .expect("again")
    );
    assert_eq!(store.open_problems().await.expect("open"), ["sig"]);
    store.clear_problems("other").await.expect("clear other");
    assert_eq!(store.open_problems().await.expect("open"), ["sig"]);
    store.clear_problems("payments").await.expect("clear");
    assert!(store.open_problems().await.expect("open").is_empty());
    assert!(
        store
            .open_problem("sig", "payments", start())
            .await
            .expect("new again")
    );
}

#[tokio::test]
async fn a_closed_problem_opens_as_new_again() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    for signature in ["sig", "other"] {
        assert!(
            store
                .open_problem(signature, "payments", start())
                .await
                .expect("open")
        );
    }

    store.close_problem("sig").await.expect("close");

    assert_eq!(store.open_problems().await.expect("open"), ["other"]);
    assert!(
        store
            .open_problem("sig", "payments", start())
            .await
            .expect("sig again")
    );
    assert!(
        !store
            .open_problem("other", "payments", start())
            .await
            .expect("other again")
    );
}

#[tokio::test]
async fn payments_streak_starts_once_and_success_clears_it() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let first = store.payments_failed(start()).await.expect("first");
    let later = store
        .payments_failed(start() + TimeDelta::hours(1))
        .await
        .expect("later");
    assert_eq!((first.since, first.alerted), (start(), false));
    assert_eq!(later.since, start());
    store.mark_blind_alerted().await.expect("mark");
    let alerted = store
        .payments_failed(start() + TimeDelta::hours(2))
        .await
        .expect("alerted");
    assert!(alerted.alerted);
    let success = start() + TimeDelta::hours(3);
    store.payments_succeeded(success).await.expect("success");
    assert_eq!(store.last_success().await.expect("last"), Some(success));
    let restarted = store
        .payments_failed(start() + TimeDelta::hours(4))
        .await
        .expect("restarted");
    assert_eq!(
        (restarted.since, restarted.alerted),
        (start() + TimeDelta::hours(4), false)
    );
}

#[tokio::test]
async fn prune_keeps_unacked_events() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let ids: Vec<String> = (0..5).map(|n| format!("e{n}")).collect();
    store
        .record(&batch_of(ids.iter().map(|id| event(id)).collect()))
        .await
        .expect("record");
    store.prune(1, 30).await.expect("prune unacked");
    assert_eq!(store.events_after(0, 100).await.expect("events").len(), 6);
    let events = store.events_after(0, 100).await.expect("events");
    store.ack(events[3].seq).await.expect("ack");
    store.prune(1, 30).await.expect("prune acked");
    let left: Vec<String> = store
        .events_after(0, 100)
        .await
        .expect("events")
        .into_iter()
        .map(|stored| stored.id)
        .collect();
    assert_eq!(left, ["e3", "e4"]);
}

#[tokio::test]
async fn prune_drops_only_old_sent_keys() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let batch = RecordBatch {
        events: Vec::new(),
        sent_keys: vec!["fresh".to_owned()],
        invoices: Vec::new(),
        enrollments: Vec::new(),
    };
    store.record(&batch).await.expect("record");
    let (client, connection) = tokio_postgres::connect(&db.url, tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(connection);
    client
        .execute(
            "INSERT INTO sent_keys (key, sent_at) VALUES ('old', now() - interval '40 days')",
            &[],
        )
        .await
        .expect("insert old");
    store.prune(100, 30).await.expect("prune");
    let keys = store.sent_keys().await.expect("keys");
    assert!(keys.contains("fresh"));
    assert!(!keys.contains("old"));
}

#[tokio::test]
async fn newest_seq_ignores_a_burned_sequence_value() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    store
        .record(&batch_of(vec![event("a")]))
        .await
        .expect("record");
    store
        .record(&batch_of(vec![event("a")]))
        .await
        .expect("record again");

    let newest = store.newest_seq().await.expect("newest");
    let stored = store.events_after(0, 100).await.expect("events");

    assert_eq!(Some(newest), stored.last().map(|stored| stored.seq));
    assert_eq!(store.last_issued_seq().await.expect("issued"), newest + 1);
}

#[tokio::test]
async fn test_databases_are_reached_over_tcp() {
    let Some(database) = support::database().await else {
        return;
    };
    let (client, connection) = tokio_postgres::connect(&database.url, tokio_postgres::NoTls)
        .await
        .expect("connect to the test database");
    tokio::spawn(connection);
    let row = client
        .query_one("SELECT inet_server_addr() IS NOT NULL", &[])
        .await
        .expect("read the server address");
    assert!(row.get::<_, bool>(0));
}

fn everything_printed(error: &(dyn Error + 'static)) -> String {
    let mut printed = format!("{error:?}");
    let mut current = Some(error);
    while let Some(error) = current {
        printed.push('\n');
        printed.push_str(&error.to_string());
        current = error.source();
    }
    printed
}

fn assert_hides_the_login(error: &StoreError, url: &str) {
    let printed = everything_printed(error);
    assert!(!printed.contains(PASSWORD), "{printed}");
    assert!(!printed.contains(url), "{printed}");
}

fn with_login(url: &str, login: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("a postgres:// URL");
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let host_start = rest[..authority_end].rfind('@').map_or(0, |at| at + 1);
    format!("{scheme}://{login}@{}", &rest[host_start..])
}

fn with_query(url: &str, parameter: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{parameter}")
}

#[tokio::test]
async fn invalid_url_never_echoes_the_address() {
    let refused = [
        ("host=db user=anhanguera dbname=anhanguera", "db"),
        ("postgres://anhanguera:hunter2@h1,h2/anhanguera", "h1,h2"),
        (
            "postgres://anhanguera:hunter2@h1:5432,h2:5432/anhanguera",
            "h1:5432",
        ),
        ("mysql://anhanguera:hunter2@db/anhanguera", "db"),
        ("postgres://anhanguera:hunter2@[bad", "[bad"),
        ("postgres://anhanguera:hunter2@/anhanguera", "anhanguera"),
        ("postgres:///anhanguera?host=", "anhanguera"),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?sslmode=sometimes",
            "db.internal",
        ),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?sslpassword=x",
            "db.internal",
        ),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?Password=x",
            "db.internal",
        ),
        (
            "postgres://anhanguera@db.internal/anhanguera",
            "db.internal",
        ),
        (
            "postgres://anhanguera:@db.internal/anhanguera",
            "db.internal",
        ),
        (
            "postgres://anhanguera@db.internal/anhanguera?password=",
            "db.internal",
        ),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?channel_binding=require",
            "db.internal",
        ),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?sslnegotiation=direct",
            "db.internal",
        ),
        (
            "postgres://anhanguera:hunter2@db.internal/anhanguera?target_session_attrs=read-write",
            "db.internal",
        ),
    ];
    for (url, host) in refused {
        let Err(store) = Store::new(url) else {
            panic!("the store accepted {url}")
        };
        let Err(lock) = acquire_writer_lock(url).await else {
            panic!("the writer lock accepted {url}")
        };
        for error in [store, lock] {
            assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
            assert_hides_the_login(&error, url);
            let printed = everything_printed(&error);
            assert!(!printed.contains(host), "{url}: {printed}");
        }
    }
}

#[tokio::test]
async fn refused_connections_never_show_the_password() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let url = format!("postgres://anhanguera:{PASSWORD}@{address}/anhanguera");
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let (lock, read) = tokio::join!(acquire_writer_lock(&url), store.device_id());
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(13), "took {elapsed:?}");
    let lock = lock.err().expect("a closed port gave the lock");
    let read = read.expect_err("a closed port answered");
    assert_eq!(lock.to_string(), LOCK_TIMED_OUT);
    assert_eq!(read.to_string(), "getting a database connection timed out");
    for error in [&lock, &read] {
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_hides_the_login(error, &url);
        let printed = everything_printed(error);
        assert!(!printed.contains(&address.to_string()), "{printed}");
    }
}

#[tokio::test]
async fn rejected_logins_never_show_the_password() {
    let Some(db) = support::database().await else {
        return;
    };
    // The test cluster trusts every role it knows, so an unknown role is what it rejects.
    let url = with_login(&db.url, &format!("intruder:{PASSWORD}"));
    let store = Store::new(&url).expect("store");

    let (lock, read) = tokio::join!(acquire_writer_lock(&url), store.device_id());

    let lock = lock.err().expect("an unknown role took the lock");
    let read = read.expect_err("an unknown role was let in");
    for error in [&lock, &read] {
        assert!(matches!(error, StoreError::Database(_)), "{error:?}");
        assert_hides_the_login(error, &url);
    }
}

#[tokio::test]
async fn a_url_that_requires_tls_is_refused_at_once() {
    let Some(db) = support::database().await else {
        return;
    };
    let url = with_query(
        &with_login(&db.url, &format!("anhanguera:{PASSWORD}")),
        "sslmode=require",
    );
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let (lock, read) = tokio::join!(acquire_writer_lock(&url), store.device_id());
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    let lock = lock
        .err()
        .expect("an address that requires TLS took the lock");
    let read = read.expect_err("an address that requires TLS was let in");
    for error in [&lock, &read] {
        assert!(matches!(error, StoreError::Database(_)), "{error:?}");
        let printed = everything_printed(error);
        assert!(printed.contains("TLS"), "{printed}");
        assert_hides_the_login(error, &url);
    }
}

#[tokio::test]
async fn silent_servers_time_out_without_showing_the_address() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let url = format!("postgres://anhanguera:{PASSWORD}@{address}/anhanguera");
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let (lock, read) = tokio::join!(acquire_writer_lock(&url), store.device_id());
    let elapsed = started.elapsed();
    silent.abort();

    assert!(elapsed < Duration::from_secs(13), "took {elapsed:?}");
    let lock = lock.err().expect("a silent server gave the lock");
    let read = read.expect_err("a silent server answered");
    assert_eq!(lock.to_string(), LOCK_TIMED_OUT);
    for error in [&lock, &read] {
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_hides_the_login(error, &url);
        let printed = everything_printed(error);
        assert!(!printed.contains(&address.to_string()), "{printed}");
    }
}

fn assert_reason_hides_the_address(reason: &str, url: &str) {
    let host_and_port = url
        .split_once('@')
        .and_then(|(_, rest)| rest.split('/').next())
        .expect("a host and port in the URL");
    assert!(!reason.contains(url), "{reason}");
    assert!(!reason.contains(host_and_port), "{reason}");
}

#[tokio::test]
async fn migrate_applies_the_migrations_once() {
    let Some(db) = support::database().await else {
        return;
    };
    let lock = acquire_writer_lock(&db.url).await.expect("lock");

    let first = lock.migrate().await.expect("migrate");
    let again = lock.migrate().await.expect("migrate again");

    assert_eq!(first, [GOOSE_SCHEMA, DROP_GOOSE_DB_VERSION]);
    assert_eq!(again, Vec::<String>::new());
    assert_eq!(table_names(&admin(&db.url).await).await, TODAYS_TABLES);
}

#[tokio::test]
async fn a_role_that_cannot_create_tables_starts_on_a_migrated_database() {
    let Some(db) = support::migrated_database().await else {
        return;
    };
    admin(&db.url)
        .await
        .batch_execute(&format!(
            "DO $$ BEGIN CREATE ROLE {TABLES_ONLY} LOGIN PASSWORD '{PASSWORD}';
             EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;
             REVOKE CREATE ON SCHEMA public FROM PUBLIC;
             GRANT SELECT, INSERT, UPDATE, DELETE
                 ON enrollments, events, invoices, monitor_state, problems, sent_keys
                 TO {TABLES_ONLY};
             GRANT USAGE, SELECT ON SEQUENCE events_seq_seq TO {TABLES_ONLY};
             GRANT SELECT ON seaql_migrations TO {TABLES_ONLY};"
        ))
        .await
        .expect("a role with table rights only");
    let url = with_login(&db.url, &format!("{TABLES_ONLY}:{PASSWORD}"));
    let lock = acquire_writer_lock(&url).await.expect("lock as that role");

    let applied = lock.migrate().await;

    assert_eq!(applied.expect("migrate as that role"), Vec::<String>::new());
}

#[tokio::test]
async fn writer_lock_admits_one_holder() {
    let Some(db) = support::database().await else {
        return;
    };
    let first = acquire_writer_lock(&db.url).await.expect("first lock");

    let second = acquire_writer_lock(&db.url).await;
    first.release().await.expect("release the first lock");
    let third = acquire_writer_lock(&db.url).await;

    let Err(refused) = second else {
        panic!("a second holder took the lock");
    };
    assert!(matches!(refused, StoreError::Locked), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        "another anhanguera-monitor process holds the database lock"
    );
    third.expect("the lock once the first released it");
}

#[tokio::test]
async fn writer_lock_notices_its_connection_ending() {
    let Some(db) = support::database().await else {
        return;
    };
    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");

    end_the_writer_locks_connection(&admin(&db.url).await).await;
    timeout(Duration::from_secs(1), lock.closed())
        .await
        .expect("closed resolves once the backend ends");

    let reason = lock.close_reason().expect("a reason");
    assert!(
        reason.starts_with("the lock's connection failed: "),
        "{reason}"
    );
    assert_reason_hides_the_address(&reason, &db.url);
}

#[tokio::test]
async fn a_lock_whose_connection_ended_refuses_to_migrate() {
    let Some(db) = support::database().await else {
        return;
    };
    let lock = acquire_writer_lock(&db.url).await.expect("lock");
    let client = admin(&db.url).await;
    end_the_writer_locks_connection(&client).await;

    let error = lock
        .migrate()
        .await
        .expect_err("a lock without its connection migrated");

    let StoreError::LockLost(reason) = &error else {
        panic!("{error:?}");
    };
    assert!(
        reason.starts_with("the lock's connection failed: "),
        "{reason}"
    );
    assert_reason_hides_the_address(reason, &db.url);
    assert!(
        error
            .to_string()
            .starts_with("the database lock was lost: the lock's connection failed: "),
        "{error}"
    );
    assert_eq!(table_names(&client).await, Vec::<String>::new());
}

#[tokio::test]
async fn writer_lock_notices_a_connection_that_stops_answering() {
    let Some(db) = support::database().await else {
        return;
    };
    let (proxy, url) = PausingProxy::start(&db.url).await;
    let mut lock = acquire_writer_lock(&url)
        .await
        .expect("lock through the proxy");

    proxy.pause();
    let started = Instant::now();
    timeout(
        LOCK_CHECK_LIMIT + LOCK_CHECK_EVERY + Duration::from_secs(2),
        lock.closed(),
    )
    .await
    .expect("closed resolves once the connection stops answering");

    let elapsed = started.elapsed();
    assert!(elapsed >= LOCK_CHECK_LIMIT, "{elapsed:?}");
    assert_eq!(
        lock.close_reason().as_deref(),
        Some("the lock's connection did not answer within 30s")
    );
}

#[tokio::test]
async fn writer_lock_notices_an_idle_connection_that_stops_answering() {
    let Some(db) = support::database().await else {
        return;
    };
    let (proxy, url) = PausingProxy::start(&db.url).await;
    let mut lock = acquire_writer_lock(&url)
        .await
        .expect("lock through the proxy");
    // The pause comes between checks, while the connection sits idle in the pool.
    let settled = timeout(2 * LOCK_CHECK_EVERY + LOCK_CHECK_EVERY / 4, lock.closed()).await;
    assert!(
        settled.is_err(),
        "closed resolved: {:?}",
        lock.close_reason()
    );

    proxy.pause();
    let started = Instant::now();
    timeout(
        LOCK_CHECK_LIMIT + LOCK_CHECK_EVERY + Duration::from_secs(2),
        lock.closed(),
    )
    .await
    .expect("closed resolves once the idle connection stops answering");

    let elapsed = started.elapsed();
    assert!(elapsed >= LOCK_CHECK_LIMIT, "{elapsed:?}");
    assert_eq!(
        lock.close_reason().as_deref(),
        Some("the lock's connection did not answer within 30s")
    );
}

#[tokio::test]
async fn writer_lock_stays_while_its_connection_answers() {
    let Some(db) = support::database().await else {
        return;
    };
    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");

    let waited = timeout(3 * LOCK_CHECK_EVERY, lock.closed()).await;

    assert!(
        waited.is_err(),
        "closed resolved: {:?}",
        lock.close_reason()
    );
    assert_eq!(lock.close_reason(), None);
}
