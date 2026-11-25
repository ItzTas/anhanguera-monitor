pub mod support;

use std::time::Duration;

use anhanguera_monitor::store::Store;
use monitor_hub_sdk_testkit::conformance::{Conformance, Outcome, run};

use support::fake_ava::FakeAva;
use support::process::{SocketDir, monitor_env};
use support::{migrated_database, utc};

async fn seed_the_welcome(url: &str) {
    Store::new(url)
        .expect("store")
        .initialize(
            "00000000-0000-4000-8000-000000000000".to_owned(),
            utc("2026-11-18T12:00:00Z"),
        )
        .await
        .expect("seed the welcome event");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_monitor_conforms() {
    let Some(db) = migrated_database().await else {
        return;
    };
    seed_the_welcome(&db.url).await;
    let ava = FakeAva::quiet().await;
    let sockets = SocketDir::create();
    let mut conformance = Conformance::new(env!("CARGO_BIN_EXE_anhanguera-monitor"));
    conformance.command.push("monitor".to_owned());
    conformance.env = monitor_env(&db.url, &ava.url(), &sockets);
    conformance.min_events = 1;
    conformance.monitor_dir = Some(env!("CARGO_MANIFEST_DIR").into());
    conformance.ready_timeout = Duration::from_secs(20);

    let outcome = tokio::task::spawn_blocking(move || run(&conformance))
        .await
        .expect("join the conformance run");

    match outcome {
        Outcome::Passed => {}
        Outcome::Skipped(reason) => eprintln!("skipping: {reason}"),
        Outcome::Failed(output) => panic!("{output}"),
        other => panic!("unexpected conformance outcome: {other:?}"),
    }
}
