pub mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_postgres::Client;

use support::fake_ava::FakeAva;
use support::process::{
    SocketDir, monitor_env, process_env, require_sh, run_to_exit, run_to_exit_with_document,
    spawn_monitor,
};
use support::schema::table_names;
use support::{admin, database, end_the_writer_locks_connection};

const EXIT_LIMIT: Duration = Duration::from_secs(30);
const RECORDED_VERSIONS: &str =
    "SELECT format('%s %s', version, applied_at) FROM seaql_migrations ORDER BY version";
const PGPASS_LINE: &str = "pgpass-line-7d1e";

fn run(arguments: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"))
        .args(arguments)
        .env_clear()
        .output()
        .expect("run the binary")
}

fn assert_refuses(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        monitor_hub_sdk::NOT_LAUNCHED_BY_HUB
    );
}

fn the_fatal_error(stderr: &str) -> String {
    let fatal: Vec<Value> = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["@message"] == "fatal")
        .collect();
    let [fatal] = &fatal[..] else {
        panic!("expected one fatal line:\n{stderr}");
    };
    assert_eq!(fatal["@level"], "error", "{stderr}");
    fatal["error"]
        .as_str()
        .unwrap_or_else(|| panic!("a fatal line without an error:\n{stderr}"))
        .to_owned()
}

fn host_and_port(url: &str) -> &str {
    url.split_once('@')
        .and_then(|(_, rest)| rest.split('/').next())
        .expect("a host and port in the URL")
}

async fn texts(client: &Client, query: &str) -> Vec<String> {
    client
        .query(query, &[])
        .await
        .unwrap_or_else(|err| panic!("{query}: {err:?}"))
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[test]
fn refuses_to_run_outside_the_hub() {
    assert_refuses(&run(&["monitor"]));
}

#[test]
fn runs_with_no_arguments_like_outside_the_hub() {
    assert_refuses(&run(&[]));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_process_refuses_to_start() {
    if !require_sh() {
        return;
    }
    let Some(db) = database().await else {
        return;
    };
    let (first_portal, second_portal) = tokio::join!(FakeAva::quiet(), FakeAva::start());
    let (first_sockets, second_sockets) = (SocketDir::create(), SocketDir::create());
    let mut first = spawn_monitor(&monitor_env(&db.url, &first_portal.url(), &first_sockets));
    let client = admin(&db.url).await;
    let before = texts(&client, RECORDED_VERSIONS).await;

    let second = run_to_exit(
        &monitor_env(&db.url, &second_portal.url(), &second_sockets),
        EXIT_LIMIT,
    );

    assert_eq!(second.status.code(), Some(1), "{}", second.stderr);
    assert_eq!(second.stdout, "");
    assert_eq!(
        the_fatal_error(&second.stderr),
        "acquiring the database writer lock: another anhanguera-monitor process holds the database lock"
    );
    assert_eq!(texts(&client, RECORDED_VERSIONS).await, before);
    assert_eq!(before.len(), 2, "{before:?}");
    assert!(second_portal.requests().is_empty());
    let stopped = first.stop();
    assert_eq!(stopped.status.code(), Some(0), "{}", stopped.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_database_lock_exits_1() {
    let Some(db) = database().await else {
        return;
    };
    let portal = FakeAva::quiet().await;
    let sockets = SocketDir::create();
    let mut spawned = spawn_monitor(&monitor_env(&db.url, &portal.url(), &sockets));

    end_the_writer_locks_connection(&admin(&db.url).await).await;
    let exited = spawned.wait_exit(Duration::from_secs(5));

    assert_eq!(exited.status.code(), Some(1), "{}", exited.stderr);
    let error = the_fatal_error(&exited.stderr);
    assert!(
        error.starts_with("the database lock was lost: the lock's connection"),
        "{error}"
    );
    assert!(!exited.stderr.contains(&db.url), "{}", exited.stderr);
    assert!(
        !exited.stderr.contains(host_and_port(&db.url)),
        "{}",
        exited.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_url_without_a_password_stops_the_start_before_pgpass_is_read() {
    let Some(db) = database().await else {
        return;
    };
    let mut url = reqwest::Url::parse(&db.url).expect("the test database's URL");
    url.set_password(None).expect("drop the password");
    let portal = FakeAva::quiet().await;
    let sockets = SocketDir::create();
    let pgpass = format!("{}/pgpass", sockets.path());
    // One field short, so a reader of the file would log the whole line as malformed.
    fs::write(&pgpass, format!("*:*:*:{PGPASS_LINE}\n")).expect("write the pgpass file");
    fs::set_permissions(&pgpass, fs::Permissions::from_mode(0o600))
        .expect("make the pgpass file private");
    let mut env = monitor_env(url.as_str(), &portal.url(), &sockets);
    env.push(("PGPASSFILE".to_owned(), pgpass));

    let exited = run_to_exit(&env, EXIT_LIMIT);

    assert_eq!(exited.status.code(), Some(1), "{}", exited.stderr);
    assert_eq!(exited.stdout, "");
    assert_eq!(
        the_fatal_error(&exited.stderr),
        "acquiring the database writer lock: DATABASE_URL has no password: add the role's password to the URL"
    );
    assert!(!exited.stderr.contains(PGPASS_LINE), "{}", exited.stderr);
    let tables = table_names(&admin(&db.url).await).await;
    assert!(tables.is_empty(), "{tables:?}");
    assert!(portal.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configuration_error_in_the_document_stops_the_start_before_the_portal() {
    if !require_sh() {
        return;
    }
    let ava = FakeAva::start().await;
    let sockets = SocketDir::create();
    let document = json!({
        "database_url": "postgres://anhanguera:s3cr3t-marker@127.0.0.1:9/anhanguera",
        "ava_username": "1234",
        "ava_password": "s3cr3t-marker",
        "courses": [],
        "reminder_hour": 24,
        "ava_graphql_url": ava.url(),
    });

    let exited = run_to_exit_with_document(&process_env(&sockets), &document, EXIT_LIMIT);

    assert!(
        !exited.stdout.contains("s3cr3t-marker"),
        "stdout carries the secret marker"
    );
    assert!(
        !exited.stderr.contains("s3cr3t-marker"),
        "stderr carries the secret marker"
    );
    assert_eq!(exited.status.code(), Some(1));
    assert_eq!(exited.stdout, "");
    assert_eq!(
        exited.stderr,
        concat!(
            "ava_username: must have 11 digits once dots, dashes and spaces are dropped; ",
            "courses: at least one course is required; ",
            "reminder_hour: must be from 0 to 23\n"
        )
    );
    assert!(ava.requests().is_empty());
}
