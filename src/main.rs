use std::error::Error;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use anhanguera_monitor::config::{self, Config};
use monitor_hub_sdk::settings::{self, VALIDATE_SETTINGS_ARG};
use monitor_hub_sdk::{NOT_LAUNCHED_BY_HUB, launched_by_hub, logging};
use tokio::runtime::{Builder, Runtime};
use tracing::error;

const RUNTIME_SHUTDOWN_LIMIT: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    if arguments == [VALIDATE_SETTINGS_ARG] {
        return settings::answer(&config::validate_settings());
    }
    if arguments == ["probe"] {
        return anhanguera_monitor::probe::run();
    }
    if arguments == ["monitor"] && launched_by_hub() {
        return run_monitor();
    }
    eprintln!("{NOT_LAUNCHED_BY_HUB}");
    ExitCode::FAILURE
}

fn run_monitor() -> ExitCode {
    let config = match Config::load() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = logging::init(&config.log_level) {
        eprintln!("cannot start logging: {err}");
        return ExitCode::FAILURE;
    }
    let Some(runtime) = runtime() else {
        return ExitCode::FAILURE;
    };
    let result = runtime.block_on(anhanguera_monitor::app::run(config));
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
