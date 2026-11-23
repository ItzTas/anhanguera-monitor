use std::fmt::Write;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use monitor_hub_sdk::serve;
use tokio::time::{Instant, timeout_at};
use tracing::info;

use crate::ava::{AvaClient, Credentials};
use crate::config::Config;
use crate::monitor::{Monitor, MonitorOptions};
use crate::scheduler::Scheduler;
use crate::server::AnhangueraMonitor;
use crate::store::{LOCK_LOST, Store, WriterLock, acquire_writer_lock};
use crate::timestamp::system_clock;

const START_UP_LIMIT: Duration = Duration::from_secs(45);

struct Started {
    lock: WriterLock,
    monitor: Monitor<AvaClient>,
    first_read_boundary: i64,
    newest_event: i64,
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let started = start_up(&config, START_UP_LIMIT).await?;
    serve_until_stopped(started).await
}

pub fn new_device_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            id.push('-');
        }
        let _ = write!(id, "{byte:02x}");
    }
    id
}

async fn start_up(config: &Config, limit: Duration) -> anyhow::Result<Started> {
    let deadline = Instant::now() + limit;
    let lock = within(
        deadline,
        limit,
        "acquiring the database writer lock",
        acquire_writer_lock(config.database_url.reveal()),
    )
    .await?;
    let applied = within(
        deadline,
        limit,
        "applying the database migrations",
        lock.migrate(),
    )
    .await?;
    info!(migrations = ?applied, "database migrations applied");
    let store = Store::new(config.database_url.reveal()).context("opening the database pool")?;
    let clock = system_clock();
    let (first_read_boundary, newest_event) =
        within(deadline, limit, "reading the last issued event", async {
            tokio::try_join!(store.last_issued_seq(), store.newest_seq())
        })
        .await?;
    let device_id = within(
        deadline,
        limit,
        "preparing the monitor state",
        store.initialize(new_device_id(), clock()),
    )
    .await?;
    let portal = AvaClient::new(
        config.graphql_url.clone(),
        Credentials {
            username: config.username.clone(),
            password: config.password.clone(),
        },
        device_id,
        Arc::clone(&clock),
        config.timezone,
    )
    .context("building the portal client")?;
    let monitor = Monitor::new(store, portal, MonitorOptions::from_config(config, clock));
    Ok(Started {
        lock,
        monitor,
        first_read_boundary,
        newest_event,
    })
}

async fn within<T, E>(
    deadline: Instant,
    limit: Duration,
    step: &'static str,
    work: impl Future<Output = Result<T, E>>,
) -> anyhow::Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let Ok(finished) = timeout_at(deadline, work).await else {
        return Err(anyhow!("the start-up limit of {limit:?} ran out").context(step));
    };
    finished.context(step)
}

async fn serve_until_stopped(started: Started) -> anyhow::Result<()> {
    let Started {
        mut lock,
        monitor,
        first_read_boundary,
        newest_event,
    } = started;
    let notifier = monitor.notifier().clone();
    notifier.events_recorded(newest_event);
    let scheduler = Scheduler::new(Arc::new(monitor));
    let served = AnhangueraMonitor::new(Arc::clone(&scheduler))
        .with_first_read_boundary(first_read_boundary);
    let cycles = tokio::spawn(scheduler.run());
    let ended = tokio::select! {
        outcome = serve(served, notifier) => outcome.context("serving the monitor"),
        () = lock.closed() => Err(lock_lost(&lock)),
    };
    cycles.abort();
    let _ = cycles.await;
    ended
}

fn lock_lost(lock: &WriterLock) -> anyhow::Error {
    let Some(reason) = lock.close_reason() else {
        return anyhow!(LOCK_LOST);
    };
    anyhow::Error::msg(reason).context(LOCK_LOST)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use tokio::net::TcpListener;

    use crate::postgres::database;

    use super::{START_UP_LIMIT, new_device_id, start_up};
    use crate::config::{Config, document_from_env};

    fn config(database_url: &str) -> Config {
        let environment = HashMap::from([
            ("DATABASE_URL", database_url),
            ("AVA_USERNAME", "12345678909"),
            ("AVA_PASSWORD", "test-password"),
            ("COURSES", "Direito"),
        ]);
        let document = document_from_env(&|name| environment.get(name).map(|v| (*v).to_owned()))
            .expect("a settings document");
        Config::from_document(&document, &|_| Err(std::io::Error::other("no files")))
            .expect("a valid configuration")
    }

    #[test]
    fn new_device_id_is_a_v4_uuid() {
        let shape = |id: &str| {
            let groups: Vec<&str> = id.split('-').collect();
            let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
            let hex = id
                .chars()
                .all(|c| c == '-' || c.is_ascii_digit() || ('a'..='f').contains(&c));
            hex && lengths == [8, 4, 4, 4, 12]
                && groups[2].starts_with('4')
                && "89ab".contains(&groups[3][..1])
        };

        let first = new_device_id();
        let second = new_device_id();

        assert!(shape(&first), "{first}");
        assert!(shape(&second), "{second}");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn on_a_new_database_the_first_read_ends_before_the_welcome() {
        let Some(db) = database().await else {
            return;
        };

        let started = start_up(&config(&db.url), START_UP_LIMIT)
            .await
            .expect("start-up");

        let stored = started
            .monitor
            .store()
            .events_after(0, 10)
            .await
            .expect("events");
        let positions: Vec<(i64, &str)> = stored
            .iter()
            .map(|stored| (stored.seq, stored.id.as_str()))
            .collect();
        assert_eq!(started.first_read_boundary, 0);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].0, 1);
    }

    #[tokio::test]
    async fn the_device_id_survives_a_restart() {
        let Some(db) = database().await else {
            return;
        };
        let config = config(&db.url);

        let first = start_up(&config, START_UP_LIMIT).await.expect("start-up");
        let before = first.monitor.store().device_id().await.expect("device id");
        first.lock.release().await.expect("release the first lock");
        let second = start_up(&config, START_UP_LIMIT)
            .await
            .expect("second start-up");
        let after = second.monitor.store().device_id().await.expect("device id");

        assert!(before.is_some());
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn start_up_stops_at_its_limit_and_names_the_step() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let silent = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let config = config(&format!(
            "postgres://anhanguera:hunter2@{address}/anhanguera"
        ));

        let started = Instant::now();
        let Err(error) = start_up(&config, Duration::from_millis(300)).await else {
            panic!("a silent database let the start-up finish");
        };
        let elapsed = started.elapsed();
        silent.abort();

        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        assert_eq!(
            format!("{error:#}"),
            "acquiring the database writer lock: the start-up limit of 300ms ran out"
        );
    }
}
