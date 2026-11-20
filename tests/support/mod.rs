pub mod fake_ava;
pub mod fake_portal;
pub mod postgres;
pub mod process;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anhanguera_monitor::store::MIGRATIONS;
use anhanguera_monitor::timestamp::Clock;
use chrono::{DateTime, TimeDelta, Utc};
use monitor_hub_plugin::Signals;
use monitor_hub_plugin::migrate::up;
use tokio::time::timeout;
use tokio_postgres::NoTls;

pub use postgres::{Database, database};

pub const SIGNAL_WAIT: Duration = Duration::from_secs(5);

pub async fn migrated_database() -> Option<Database> {
    let database = database().await?;
    let (mut client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("connect to the test database");
    let driver = tokio::spawn(connection);
    up(&mut client, MIGRATIONS)
        .await
        .expect("migrate the test database");
    drop(client);
    driver
        .await
        .expect("join the migration connection")
        .expect("close the migration connection");
    Some(database)
}

pub fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("bad timestamp {raw:?}"))
        .to_utc()
}

#[derive(Clone)]
pub struct MovableClock {
    at: Arc<Mutex<DateTime<Utc>>>,
}

impl MovableClock {
    pub fn starting_at(at: DateTime<Utc>) -> MovableClock {
        MovableClock {
            at: Arc::new(Mutex::new(at)),
        }
    }

    pub fn clock(&self) -> Clock {
        let at = Arc::clone(&self.at);
        Arc::new(move || *at.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn now(&self) -> DateTime<Utc> {
        *self.at.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn advance(&self, by: TimeDelta) {
        *self.at.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

pub async fn next_signal(signals: &mut Signals) -> i64 {
    timeout(SIGNAL_WAIT, signals.changed())
        .await
        .expect("a signal within 5 seconds")
}

pub async fn pending_signal(signals: &mut Signals) -> Option<i64> {
    timeout(Duration::ZERO, signals.changed()).await.ok()
}
