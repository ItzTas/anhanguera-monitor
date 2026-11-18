pub mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anhanguera_monitor::ava::model::CourseEnrollment;
use anhanguera_monitor::config::{Config, document_from_env};
use anhanguera_monitor::monitor::{Monitor, MonitorOptions};
use anhanguera_monitor::scheduler::Scheduler;
use anhanguera_monitor::store::Store;
use tokio::time::{Instant, sleep};

use support::fake_portal::FakePortal;
use support::{MovableClock, migrated_database, utc};

const PATIENCE: Duration = Duration::from_secs(5);

fn options(clock: &MovableClock) -> MonitorOptions {
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
    MonitorOptions::from_config(&config, clock.clock())
}

async fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_first_cycle_runs_at_start_and_a_trigger_runs_the_next() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let clock = MovableClock::starting_at(utc("2026-12-01T15:00:00Z"));
    let portal = FakePortal::new();
    portal.set_enrollments(Ok(vec![CourseEnrollment {
        id: "enrollment-1".to_owned(),
        status: "ACTIVE".to_owned(),
        course_name: "Direito".to_owned(),
    }]));
    let monitor = Monitor::new(
        Store::new(&db.url).expect("store"),
        portal.clone(),
        options(&clock),
    );
    monitor
        .prepare("device-1".to_owned())
        .await
        .expect("prepare");
    let scheduler = Scheduler::new(Arc::new(monitor));
    let cycles = || {
        portal
            .calls()
            .iter()
            .filter(|call| *call == "student_id")
            .count()
    };

    let running = tokio::spawn(Arc::clone(&scheduler).run());

    eventually("the first cycle ends", || {
        cycles() == 1 && !scheduler.state().running
    })
    .await;
    let first = scheduler.state();
    assert_eq!(first.last_started, Some(clock.now()));
    assert_eq!(first.last_finished, Some(clock.now()));
    assert_eq!(first.problem, None);

    sleep(Duration::from_millis(200)).await;
    assert_eq!(cycles(), 1);

    assert!(scheduler.trigger());
    eventually("the triggered cycle ends", || {
        cycles() == 2 && !scheduler.state().running
    })
    .await;
    running.abort();
}
