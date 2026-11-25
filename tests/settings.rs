pub mod support;

use std::process::Command;

use monitor_hub_sdk::settings::{VALIDATE_SETTINGS_ARG, VALIDATE_SETTINGS_LIMIT};
use support::process::{Exited, run_limited};

const BASE: [(&str, &str); 4] = [
    ("DATABASE_URL", "postgres://anhanguera:x@db/anhanguera"),
    ("AVA_USERNAME", "12345678909"),
    ("AVA_PASSWORD", "x"),
    ("COURSES", "A"),
];

fn run(environment: &[(&str, &str)]) -> Exited {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"));
    command
        .arg(VALIDATE_SETTINGS_ARG)
        .env_clear()
        .envs(environment.iter().copied());
    run_limited(command, VALIDATE_SETTINGS_LIMIT)
}

fn answer(exited: &Exited) -> serde_json::Value {
    serde_json::from_str(&exited.stdout).expect("stdout is JSON")
}

#[test]
fn validate_settings_answers_with_paths() {
    let exited = run(&[]);

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
fn validate_settings_reports_conversion_and_other_problems_together() {
    let exited = run(&[("REMINDER_HOUR", "x")]);

    assert_eq!(exited.status.code(), Some(1));
    let problems = answer(&exited)["problems"].clone();
    let paths: Vec<&str> = problems
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|p| p["path"].as_str())
        .collect();
    assert!(paths.contains(&"reminder_hour"), "{paths:?}");
    assert!(paths.contains(&"courses"), "{paths:?}");
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
