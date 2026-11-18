pub mod support;

use std::time::Duration;

use anhanguera_monitor::store::Store;
use monitor_hub_plugin_testkit::conformance::{Conformance, Outcome, run};
use serde_json::json;

use support::fake_ava::FakeAva;
use support::process::{SocketDir, plugin_env};
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

async fn quiet_portal() -> FakeAva {
    let ava = FakeAva::start().await;
    ava.set(
        "Invoices",
        200,
        json!({"data": {"getInvoiceHomeModel": {
            "informationType": "PAYMENTS_ON_TIME",
            "billingDay": "",
            "invoiceSections": []
        }}}),
    );
    ava.set(
        "SubjectDeadlines",
        200,
        json!({"data": {"data": {
            "id": "KRT-00000000-0000-0000-0000-000000000001",
            "subjectEnrollments": []
        }}}),
    );
    ava.set("Calendar", 200, json!({"data": {"data": []}}));
    ava
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plugin_conforms() {
    let Some(db) = migrated_database().await else {
        return;
    };
    seed_the_welcome(&db.url).await;
    let ava = quiet_portal().await;
    let sockets = SocketDir::create();
    let mut conformance = Conformance::new(env!("CARGO_BIN_EXE_anhanguera-monitor"));
    conformance.command.push("plugin".to_owned());
    conformance.env = plugin_env(&db.url, &ava.url(), &sockets);
    conformance.min_events = 1;
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
