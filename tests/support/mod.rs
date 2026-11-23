pub mod fake_ava;
pub mod fake_portal;
pub mod postgres;
pub mod process;
pub mod proxy;
pub mod schema;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anhanguera_monitor::migration::Migrator;
use anhanguera_monitor::store::WRITER_LOCK_ID;
use anhanguera_monitor::timestamp::Clock;
use chrono::{DateTime, TimeDelta, Utc};
use monitor_hub_plugin::Signals;
use sea_orm::{ConnectOptions, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use tokio::time::timeout;
use tokio_postgres::{Client, NoTls};

pub use postgres::{Database, database};

pub const SIGNAL_WAIT: Duration = Duration::from_secs(5);
const WRITER_LOCK_HOLDERS: &str = "SELECT pid FROM pg_locks
WHERE locktype = 'advisory'
  AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
  AND objsubid = 1
  AND ((classid::bigint << 32) | objid::bigint) = $1";

pub async fn orm(url: &str) -> DatabaseConnection {
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    sea_orm::Database::connect(options)
        .await
        .expect("connect to the test database through SeaORM")
}

pub async fn admin(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the admin client");
    tokio::spawn(connection);
    client
}

pub async fn end_the_writer_locks_connection(client: &Client) {
    let holders: Vec<i32> = client
        .query(WRITER_LOCK_HOLDERS, &[&WRITER_LOCK_ID])
        .await
        .expect("list the writer lock's holders")
        .iter()
        .map(|row| row.get(0))
        .collect();
    let [holder] = holders[..] else {
        panic!("the writer lock has {} holders", holders.len());
    };
    // With a timeout, pg_terminate_backend returns only once the backend has exited.
    let ended: bool = client
        .query_one("SELECT pg_terminate_backend($1, 5000)", &[&holder])
        .await
        .expect("end the writer lock's backend")
        .get(0);
    assert!(ended, "the writer lock's backend did not exit");
}

pub async fn migrated_database() -> Option<Database> {
    let database = database().await?;
    let connection = orm(&database.url).await;
    Migrator::up(&connection, None)
        .await
        .expect("migrate the test database");
    connection
        .close()
        .await
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
