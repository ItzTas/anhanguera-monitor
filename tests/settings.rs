fn run(environment: &[(&str, &str)]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_anhanguera-monitor"))
        .arg("validate-settings")
        .env_clear()
        .envs(environment.iter().copied())
        .output()
        .expect("run the binary")
}

fn answer(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

#[test]
fn validate_settings_answers_with_paths() {
    let output = run(&[]);

    assert_eq!(output.status.code(), Some(1));
    let problems = answer(&output)["problems"].clone();
    let paths: Vec<&str> = problems
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|p| p["path"].as_str())
        .collect();
    assert!(paths.contains(&"courses"), "{paths:?}");
}

#[test]
fn validate_settings_accepts_a_valid_environment() {
    let output = run(&[
        ("DATABASE_URL", "postgres://x"),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "x"),
        ("COURSES", "A"),
    ]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(answer(&output), serde_json::json!({"problems": []}));
}

#[test]
fn validate_settings_reports_conversion_and_other_problems_together() {
    let output = run(&[("REMINDER_HOUR", "x")]);

    assert_eq!(output.status.code(), Some(1));
    let problems = answer(&output)["problems"].clone();
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
    let output = run(&[
        ("DATABASE_URL", "postgres://x"),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "x"),
        ("COURSES", "A"),
        ("LOG_LEVEL", "verbose"),
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        answer(&output),
        serde_json::json!({"problems": [{
            "path": "log_level",
            "message": "must be one of trace, debug, info, warn, error"
        }]})
    );
}
