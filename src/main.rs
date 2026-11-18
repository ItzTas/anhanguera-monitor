use std::error::Error;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use anhanguera_monitor::config;
use monitor_hub_plugin::{NOT_LAUNCHED_BY_HUB, launched_by_hub, logging};
use tokio::runtime::{Builder, Runtime};
use tracing::error;

const RUNTIME_SHUTDOWN_LIMIT: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    if arguments == ["validate-settings"] {
        return validate_settings();
    }
    if arguments == ["plugin"] && launched_by_hub() {
        return run_plugin();
    }
    eprintln!("{NOT_LAUNCHED_BY_HUB}");
    ExitCode::FAILURE
}

fn validate_settings() -> ExitCode {
    let problems = match config::document_from_env(&|name| std::env::var(name).ok()) {
        Ok(document) => config::validate(&document),
        Err(problems) => problems,
    };
    let answer = serde_json::json!({
        "problems": problems
            .iter()
            .map(|p| serde_json::json!({"path": p.path, "message": p.message}))
            .collect::<Vec<_>>(),
    });
    if writeln!(std::io::stdout(), "{answer}").is_err() {
        return ExitCode::from(2);
    }
    if problems.is_empty() {
        return ExitCode::SUCCESS;
    }
    ExitCode::FAILURE
}

fn run_plugin() -> ExitCode {
    if let Err(err) = logging::init("info") {
        eprintln!("cannot start logging: {err}");
        return ExitCode::FAILURE;
    }
    let Some(runtime) = runtime() else {
        return ExitCode::FAILURE;
    };
    let result = runtime.block_on(anhanguera_monitor::app::run());
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_LIMIT);
    let Err(err) = result else {
        return ExitCode::SUCCESS;
    };
    error!(error = &*err as &dyn Error, "fatal");
    ExitCode::FAILURE
}

fn runtime() -> Option<Runtime> {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .inspect_err(|err| error!(error = err as &dyn Error, "cannot start the async runtime"))
        .ok()
}
