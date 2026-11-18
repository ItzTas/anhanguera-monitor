pub mod support;

use anhanguera_monitor::events::NewEvent;
use anhanguera_monitor::store::{EnrollmentState, KnownInvoice, RecordBatch, Store};
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};

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
