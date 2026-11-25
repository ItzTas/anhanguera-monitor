pub mod support;

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::net::TcpListener;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;
use std::time::{Duration, Instant};

use monitor_hub_sdk::settings::{VALIDATE_SETTINGS_ARG, VALIDATE_SETTINGS_LIMIT};
use monitor_hub_sdk::{MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE};
use monitor_hub_sdk_testkit::settings::command_with_document;
use serde_json::{Value, json};
use support::process::{Exited, require_sh, run_limited};

const BASE: [(&str, &str); 4] = [
    ("DATABASE_URL", "postgres://anhanguera:x@db/anhanguera"),
    ("AVA_USERNAME", "12345678909"),
    ("AVA_PASSWORD", "x"),
    ("COURSES", "A"),
];

fn run<V: AsRef<OsStr>>(environment: &[(&str, V)]) -> Exited {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"));
    command
        .arg(VALIDATE_SETTINGS_ARG)
        .env_clear()
        .envs(environment.iter().map(|(name, value)| (name, value)));
    run_limited(command, VALIDATE_SETTINGS_LIMIT)
}

fn base_with_bytes(name: &'static str, value: &[u8]) -> Vec<(&'static str, OsString)> {
    let mut environment: Vec<(&str, OsString)> = BASE
        .into_iter()
        .filter(|(key, _)| *key != name)
        .map(|(key, value)| (key, OsString::from(value)))
        .collect();
    environment.push((name, OsString::from_vec(value.to_vec())));
    environment
}

fn validate_document(document: &Value, environment: &[(&str, &str)]) -> Exited {
    let bytes = serde_json::to_vec(document).expect("the document is JSON");
    let mut document = command_with_document(
        env!("CARGO_BIN_EXE_anhanguera-monitor"),
        &[VALIDATE_SETTINGS_ARG],
        &bytes,
    )
    .expect("write the document");
    document
        .command
        .envs(environment.iter().map(|(name, value)| (name, value)));
    run_limited(document.command, VALIDATE_SETTINGS_LIMIT)
}

fn answer(exited: &Exited) -> serde_json::Value {
    serde_json::from_str(&exited.stdout).expect("stdout is JSON")
}

fn assert_without_the_marker(exited: &Exited) {
    assert!(
        !exited.stdout.contains("s3cr3t-marker"),
        "stdout carries the secret marker"
    );
    assert!(
        !exited.stderr.contains("s3cr3t-marker"),
        "stderr carries the secret marker"
    );
}

#[test]
fn validate_settings_answers_with_paths() {
    let exited = run::<&str>(&[]);

    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":["#,
            r#"{"path":"database_url","message":"database_url or database_url_file is required"},"#,
            r#"{"path":"ava_password","message":"ava_password or ava_password_file is required"},"#,
            r#"{"path":"ava_username","message":"is required"},"#,
            r#"{"path":"courses","message":"at least one course is required"}"#,
            "]}\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));
    assert_eq!(exited.stderr, "");
}

#[test]
fn validate_settings_accepts_a_valid_environment() {
    let exited = run(&BASE);

    assert_eq!(exited.status.code(), Some(0));
    assert_eq!(answer(&exited), serde_json::json!({"problems": []}));
}

#[test]
fn validate_settings_reads_no_file() {
    let mut environment: Vec<(&str, &str)> = BASE
        .into_iter()
        .filter(|(name, _)| *name != "AVA_PASSWORD")
        .collect();
    environment.push(("AVA_PASSWORD_FILE", "/nonexistent/anhanguera-password"));

    let exited = run(&environment);

    assert_eq!(exited.stdout, "{\"problems\":[]}\n");
    assert_eq!(exited.status.code(), Some(0));
}

#[test]
fn validate_settings_lists_conversion_problems_alone() {
    let exited = run(&[("REMINDER_HOUR", "x")]);

    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":[{"path":"reminder_hour","#,
            r#""message":"REMINDER_HOUR is not an integer (\"x\")"}]}"#,
            "\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));

    let exited = run(&[("PAYMENT_REMINDER_DAYS", "7, x"), ("BOLETO_LINK", "on")]);

    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":["#,
            r#"{"path":"payment_reminder_days[1]","#,
            r#""message":"PAYMENT_REMINDER_DAYS is not an integer (\"x\")"},"#,
            r#"{"path":"boleto_link","message":"BOLETO_LINK is not true or false (\"on\")"}"#,
            "]}\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));
}

#[test]
fn a_secret_that_is_not_utf8_is_refused_without_its_value() {
    let exited = run(&base_with_bytes("AVA_PASSWORD", b"s3cr3t-marker\xff"));

    assert_without_the_marker(&exited);
    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":[{"path":"ava_password","#,
            r#""message":"AVA_PASSWORD is not valid UTF-8"}]}"#,
            "\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));
}

#[test]
fn a_value_that_is_not_utf8_meets_the_monitors_rule() {
    let exited = run(&base_with_bytes("AVA_TIMEZONE", b"America/Sao\xffPaulo"));

    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":[{"path":"ava_timezone","#,
            r#""message":"must be an IANA time zone name"}]}"#,
            "\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));
}

#[test]
fn validate_settings_refuses_a_log_level_outside_its_choices() {
    let mut environment = BASE.to_vec();
    environment.push(("LOG_LEVEL", "verbose"));

    let exited = run(&environment);

    assert_eq!(exited.status.code(), Some(1));
    assert_eq!(
        answer(&exited),
        serde_json::json!({"problems": [{
            "path": "log_level",
            "message": "must be one of trace, debug, info, warn, error"
        }]})
    );
}

#[test]
fn validate_settings_reads_the_document_on_descriptor_3() {
    if !require_sh() {
        return;
    }
    let document = json!({
        "database_url": "postgres://x",
        "ava_username": "1234",
        "ava_password": "x",
        "courses": ["A"],
    });

    let exited = validate_document(&document, &[]);

    assert_eq!(
        exited.stdout,
        concat!(
            r#"{"problems":[{"path":"ava_username","#,
            r#""message":"must have 11 digits once dots, dashes and spaces are dropped"}]}"#,
            "\n"
        )
    );
    assert_eq!(exited.status.code(), Some(1));
    assert_eq!(exited.stderr, "");
}

#[test]
fn the_document_wins_over_the_environment() {
    if !require_sh() {
        return;
    }
    let document = json!({
        "database_url": "postgres://anhanguera:x@db/anhanguera",
        "ava_username": "12345678909",
        "ava_password": "x",
        "courses": ["A"],
    });

    let exited = validate_document(
        &document,
        &[("REMINDER_HOUR", "99"), ("AVA_USERNAME", "1234")],
    );

    assert_eq!(exited.stdout, "{\"problems\":[]}\n");
    assert_eq!(exited.status.code(), Some(0));
}

fn assert_answers_at_once_without_problems(started: Instant, exited: &Exited) {
    let took = started.elapsed();
    assert_eq!(exited.stdout, "{\"problems\":[]}\n");
    assert_eq!(exited.status.code(), Some(0));
    assert_eq!(exited.stderr, "");
    assert!(took < Duration::from_secs(1), "the validator took {took:?}");
}

fn assert_never_reached(listener: &TcpListener) {
    listener
        .set_nonblocking(true)
        .expect("make the listener non-blocking");
    let reached = listener.accept().map(|_| ()).map_err(|err| err.kind());
    assert_eq!(reached, Err(ErrorKind::WouldBlock));
}

#[test]
fn validate_settings_neither_logs_in_nor_reaches_the_database() {
    if !require_sh() {
        return;
    }
    let postgres = TcpListener::bind("127.0.0.1:0").expect("bind in place of Postgres");
    let portal = TcpListener::bind("127.0.0.1:0").expect("bind in place of the portal");
    let database_url = format!(
        "postgres://anhanguera:x@127.0.0.1:{}/anhanguera",
        postgres.local_addr().expect("the Postgres port").port()
    );
    let graphql_url = format!(
        "http://127.0.0.1:{}/",
        portal.local_addr().expect("the portal port").port()
    );
    let mut environment: Vec<(&str, &str)> = BASE
        .into_iter()
        .filter(|(name, _)| *name != "DATABASE_URL")
        .collect();
    environment.extend([
        ("DATABASE_URL", database_url.as_str()),
        ("AVA_GRAPHQL_URL", graphql_url.as_str()),
        (MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE),
    ]);
    let document = json!({
        "database_url": database_url,
        "ava_username": "12345678909",
        "ava_password": "x",
        "courses": ["A"],
        "ava_graphql_url": graphql_url,
    });

    let started = Instant::now();
    let from_environment = run(&environment);
    assert_answers_at_once_without_problems(started, &from_environment);

    let started = Instant::now();
    let from_document = validate_document(&document, &[]);
    assert_answers_at_once_without_problems(started, &from_document);

    assert_never_reached(&postgres);
    assert_never_reached(&portal);
}

#[test]
fn validate_settings_never_prints_a_secret() {
    if !require_sh() {
        return;
    }
    let expected = concat!(
        r#"{"problems":["#,
        r#"{"path":"ava_password","message":"set ava_password or ava_password_file, not both"},"#,
        r#"{"path":"ava_username","message":"must have 11 digits once dots, dashes and spaces are dropped"}"#,
        "]}\n"
    );
    let environment = [
        (
            "DATABASE_URL",
            "postgres://anhanguera:s3cr3t-marker@127.0.0.1:9/anhanguera",
        ),
        ("AVA_USERNAME", "abc-s3cr3t-marker"),
        ("AVA_PASSWORD", "s3cr3t-marker"),
        ("AVA_PASSWORD_FILE", "/run/s3cr3t-marker"),
        ("COURSES", "A"),
    ];
    let document = json!({
        "database_url": "postgres://anhanguera:s3cr3t-marker@127.0.0.1:9/anhanguera",
        "ava_username": "abc-s3cr3t-marker",
        "ava_password": "s3cr3t-marker",
        "ava_password_file": "/run/s3cr3t-marker",
        "courses": ["A"],
    });

    for exited in [run(&environment), validate_document(&document, &[])] {
        assert_without_the_marker(&exited);
        assert_eq!(exited.stdout, expected);
        assert_eq!(exited.status.code(), Some(1));
    }

    let wrong_type = json!({
        "database_url": ["s3cr3t-marker"],
        "ava_username": "12345678909",
        "ava_password": "x",
        "courses": ["A"],
    });

    let exited = validate_document(&wrong_type, &[]);

    assert_without_the_marker(&exited);
    assert_eq!(
        exited.stdout,
        "{\"problems\":[{\"path\":\"database_url\",\"message\":\"must be string\"}]}\n"
    );
    assert_eq!(exited.status.code(), Some(1));
}
