pub mod support;

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

use monitor_hub_sdk::settings::{VALIDATE_SETTINGS_ARG, VALIDATE_SETTINGS_LIMIT};
use support::process::{Exited, run_limited};

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

fn answer(exited: &Exited) -> serde_json::Value {
    serde_json::from_str(&exited.stdout).expect("stdout is JSON")
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

    assert!(
        !exited.stdout.contains("s3cr3t-marker"),
        "stdout carries the secret marker"
    );
    assert!(
        !exited.stderr.contains("s3cr3t-marker"),
        "stderr carries the secret marker"
    );
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
