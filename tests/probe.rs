pub mod support;

use std::process::{Command, Output};

use anhanguera_monitor::app::new_device_id;
use anhanguera_monitor::store::Store;
use chrono::Utc;
use serde_json::Value;

use support::fake_ava::FakeAva;
use support::migrated_database;

const OPEN_INVOICES: &str = include_str!("../testdata/ava/Invoices.open.json");

fn collect_strings(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) => into.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, into)),
        Value::Object(map) => map.values().for_each(|item| collect_strings(item, into)),
        _ => {}
    }
}

fn fixture_strings() -> Vec<String> {
    let folder = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/ava");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).expect("read the fixtures") {
        let path = entry.expect("a fixture entry").path();
        let text = std::fs::read_to_string(&path).expect("read a fixture");
        let value: Value = serde_json::from_str(&text).expect("a JSON fixture");
        collect_strings(&value, &mut found);
    }
    found
        .into_iter()
        .filter(|text| text.len() > 3)
        .filter(|text| !text.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|text| !text.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .collect()
}

async fn probe(env: Vec<(&'static str, String)>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"))
            .arg("probe")
            .env_clear()
            .envs(env)
            .output()
            .expect("run the probe")
    })
    .await
    .expect("join the probe")
}

fn probe_env(database_url: &str, graphql_url: &str) -> Vec<(&'static str, String)> {
    [
        ("ANHANGUERA__AVA_USERNAME", "12345678909"),
        ("ANHANGUERA__AVA_PASSWORD", "test-password"),
        (
            "ANHANGUERA__COURSES",
            "Analise e desenvolvimento de sistemas",
        ),
        ("ANHANGUERA__DATABASE_URL", database_url),
        ("AVA_GRAPHQL_URL", graphql_url),
    ]
    .into_iter()
    .map(|(name, value)| (name, value.to_owned()))
    .collect()
}

#[tokio::test]
async fn probe_prints_shapes_with_prefixed_variables() {
    let Some(db) = migrated_database().await else {
        return;
    };
    Store::new(&db.url)
        .expect("open the store")
        .initialize(new_device_id(), Utc::now())
        .await
        .expect("initialize the state");
    let ava = FakeAva::start().await;
    let open: Value = serde_json::from_str(OPEN_INVOICES).expect("the fixture is JSON");
    ava.set("Invoices", 200, open);

    let output = probe(probe_env(&db.url, &ava.url())).await;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for secret in ["12345678909", "test-password", db.url.as_str()] {
        assert!(!stdout.contains(secret), "stdout leaked a secret");
        assert!(!stderr.contains(secret), "stderr leaked a secret");
    }
    for hidden in fixture_strings() {
        assert!(!stdout.contains(&hidden), "stdout leaked a fixture value");
    }
    let document: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let operations = document["operations"].as_object().expect("operations");
    let names: Vec<&str> = operations.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "BoletoPdf",
            "Calendar",
            "CourseEnrollments",
            "Invoices",
            "StudentId",
            "SubjectDeadlines",
            "UnitAssignments",
        ]
    );
    for (name, entry) in operations {
        assert_eq!(entry["verdict"], "ok", "{name}");
    }
    assert_eq!(
        operations["BoletoPdf"]["shape"]["data"]["data"]["url"],
        "string"
    );
    assert_eq!(
        document["billing_day_net"],
        serde_json::json!([
            {"days": 3, "learned": false},
            {"days": 3, "learned": false},
        ])
    );
}

#[tokio::test]
async fn probe_reports_a_problem_with_exit_code_2() {
    let Some(db) = migrated_database().await else {
        return;
    };
    Store::new(&db.url)
        .expect("open the store")
        .initialize(new_device_id(), Utc::now())
        .await
        .expect("initialize the state");
    let ava = FakeAva::start().await;
    ava.set(
        "Calendar",
        200,
        serde_json::json!({"data": {"data": "nope"}}),
    );

    let output = probe(probe_env(&db.url, &ava.url())).await;

    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_ne!(document["operations"]["Calendar"]["verdict"], "ok");
    assert_eq!(document["operations"]["Invoices"]["verdict"], "ok");
}

#[tokio::test]
async fn probe_refuses_without_device_id() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let ava = FakeAva::start().await;

    let output = probe(probe_env(&db.url, &ava.url())).await;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "start the plugin once first"
    );
    assert!(ava.requests().is_empty());
}

#[tokio::test]
async fn probe_exits_1_when_the_portal_rejects_the_login() {
    let Some(db) = migrated_database().await else {
        return;
    };
    Store::new(&db.url)
        .expect("open the store")
        .initialize(new_device_id(), Utc::now())
        .await
        .expect("initialize the state");
    let ava = FakeAva::start().await;
    let mut env = probe_env(&db.url, &ava.url());
    env.push(("ANHANGUERA__AVA_PASSWORD", "wrong-password".to_owned()));

    let output = probe(env).await;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("wrong-password"));
    assert!(!stderr.trim().is_empty());
}

#[tokio::test]
async fn probe_exits_1_when_settings_are_missing() {
    let output = probe(vec![]).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}
