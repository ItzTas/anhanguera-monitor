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
        monitor_hub_plugin::NOT_LAUNCHED_BY_HUB
    );
}

#[test]
fn refuses_to_run_outside_the_hub() {
    assert_refuses(&run(&["plugin"]));
}

#[test]
fn runs_with_no_arguments_like_outside_the_hub() {
    assert_refuses(&run(&[]));
}
