use std::collections::HashSet;
use std::error::Error;
use std::time::Duration;

use crate::events::{Kind, NewEvent};
use chrono::{DateTime, NaiveDate, Utc};
use deadpool_postgres::{Manager, Pool, PoolError, Runtime, TimeoutType};
use monitor_hub_plugin::migrate::{MigrateError, Migration, up};
use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Config, NoTls, Row, Transaction};

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "00001_init.sql",
        sql: include_str!("../sql/schema/00001_init.sql"),
    },
    Migration {
        version: 2,
        name: "00002_invoice_per_enrollment.sql",
        sql: include_str!("../sql/schema/00002_invoice_per_enrollment.sql"),
    },
];

pub const WRITER_LOCK_ID: i64 = 0x616e_6867_7565_7261;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_SIZE: usize = 4;
const POOL_LIMITS: Limits = Limits {
    statement: Duration::from_secs(20),
    lock: Duration::from_secs(5),
};
const KEEPALIVES_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVES_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVES_RETRIES: u32 = 3;
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(60);

const TRY_WRITER_LOCK: &str = "SELECT pg_try_advisory_lock($1)";

const CREATE_STATE: &str = "INSERT INTO monitor_state (device_id) VALUES ($1)
ON CONFLICT (singleton) DO NOTHING";
const DEVICE_ID: &str = "SELECT device_id FROM monitor_state";
const LOCK_STATE: &str = "SELECT 1 FROM monitor_state FOR UPDATE";
const INSERT_EVENT: &str = "INSERT INTO events (id, kind, urgent, title, body, url, occurred_at)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (id) DO NOTHING
RETURNING seq";
const INSERT_SENT_KEY: &str = "INSERT INTO sent_keys (key, sent_at) VALUES ($1, now())
ON CONFLICT (key) DO NOTHING";
const SENT_KEYS: &str = "SELECT key FROM sent_keys";
const UPSERT_INVOICE: &str = "INSERT INTO invoices (id, course_enrollment_id, due_date, status, first_seen_at, lead_days, announced, closed_at, overdue_count, last_overdue_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT (course_enrollment_id, id) DO UPDATE SET
    due_date = EXCLUDED.due_date,
    status = EXCLUDED.status,
    first_seen_at = EXCLUDED.first_seen_at,
    lead_days = EXCLUDED.lead_days,
    announced = EXCLUDED.announced,
    closed_at = EXCLUDED.closed_at,
    overdue_count = EXCLUDED.overdue_count,
    last_overdue_at = EXCLUDED.last_overdue_at";
const UPSERT_ENROLLMENT: &str =
    "INSERT INTO enrollments (course_enrollment_id, information_type, last_payments_check_at)
VALUES ($1, $2, $3)
ON CONFLICT (course_enrollment_id) DO UPDATE SET
    information_type = EXCLUDED.information_type,
    last_payments_check_at = EXCLUDED.last_payments_check_at";
const KNOWN_INVOICES: &str = "SELECT id, course_enrollment_id, due_date, status, first_seen_at, lead_days, announced, closed_at, overdue_count, last_overdue_at
FROM invoices WHERE course_enrollment_id = $1 ORDER BY due_date, id";
const ENROLLMENT: &str = "SELECT information_type, last_payments_check_at FROM enrollments WHERE course_enrollment_id = $1";
const LEAD_TIMES: &str = "SELECT lead_days FROM invoices
WHERE course_enrollment_id = $1 AND lead_days IS NOT NULL
ORDER BY first_seen_at DESC, id DESC LIMIT 6";
const OPEN_PROBLEM: &str =
    "INSERT INTO problems (signature, area, first_seen_at) VALUES ($1, $2, $3)
ON CONFLICT (signature) DO NOTHING";
const CLEAR_PROBLEMS: &str = "DELETE FROM problems WHERE area = $1";
const CLOSE_PROBLEM: &str = "DELETE FROM problems WHERE signature = $1";
const OPEN_PROBLEMS: &str = "SELECT signature FROM problems ORDER BY signature";
const PAYMENTS_FAILED: &str = "UPDATE monitor_state
SET payments_failing_since = COALESCE(payments_failing_since, $1)
RETURNING payments_failing_since, blind_alerted";
const MARK_BLIND_ALERTED: &str = "UPDATE monitor_state SET blind_alerted = TRUE";
const PAYMENTS_SUCCEEDED: &str = "UPDATE monitor_state
SET payments_failing_since = NULL, blind_alerted = FALSE, last_success_at = $1";
const LAST_SUCCESS: &str = "SELECT last_success_at FROM monitor_state";
const EVENTS_AFTER: &str = "SELECT seq, id, kind, urgent, title, body, url, occurred_at FROM events WHERE seq > $1 ORDER BY seq LIMIT $2";
const ACK_EVENTS: &str = "UPDATE monitor_state SET acked_seq = $1 WHERE acked_seq < $1";
const LAST_ISSUED_SEQ: &str = "SELECT GREATEST(
    (SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM events_seq_seq),
    (SELECT COALESCE(max(seq), 0) FROM events)
)";
const NEWEST_SEQ: &str = "SELECT COALESCE(max(seq), 0) FROM events";
const PRUNE_EVENTS: &str = "DELETE FROM events
WHERE seq <= (SELECT acked_seq FROM monitor_state)
  AND seq NOT IN (SELECT seq FROM events ORDER BY seq DESC LIMIT $1)";
const PRUNE_SENT_KEYS: &str =
    "DELETE FROM sent_keys WHERE sent_at < now() - make_interval(days => $1::INT)";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("DATABASE_URL is not a valid Postgres address")]
    InvalidUrl,
    #[error("the monitor state was never initialized")]
    NotInitialized,
    #[error("another anhanguera-monitor process holds the database lock")]
    Locked,
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error(transparent)]
    Pool(PoolError),
    #[error(transparent)]
    Database(tokio_postgres::Error),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

impl From<tokio_postgres::Error> for StoreError {
    fn from(error: tokio_postgres::Error) -> Self {
        let timeouts = [
            (SqlState::QUERY_CANCELED, "running a database statement"),
            (SqlState::LOCK_NOT_AVAILABLE, "waiting for a database lock"),
        ];
        let step = error.code().and_then(|code| {
            timeouts
                .into_iter()
                .find(|(timeout, _)| timeout == code)
                .map(|(_, step)| step)
        });
        let Some(step) = step else {
            return StoreError::Database(error);
        };
        StoreError::Timeout(step)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEvent {
    pub seq: i64,
    pub id: String,
    pub kind: String,
    pub urgent: bool,
    pub title: String,
    pub body: String,
    pub url: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownInvoice {
    pub id: String,
    pub course_enrollment_id: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub first_seen_at: DateTime<Utc>,
    pub lead_days: Option<i64>,
    pub announced: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub overdue_count: i64,
    pub last_overdue_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrollmentState {
    pub information_type: Option<String>,
    pub last_payments_check_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default)]
pub struct RecordBatch {
    pub events: Vec<NewEvent>,
    pub sent_keys: Vec<String>,
    pub invoices: Vec<KnownInvoice>,
    pub enrollments: Vec<(String, EnrollmentState)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentsStreak {
    pub since: DateTime<Utc>,
    pub alerted: bool,
}

#[derive(Clone, Copy)]
struct Limits {
    statement: Duration,
    lock: Duration,
}

pub struct WriterLock {
    client: Client,
    connection: Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
    close_reason: Option<String>,
}

pub async fn acquire_writer_lock(database_url: &str) -> Result<WriterLock, StoreError> {
    let config = lock_config(database_url)?;
    let (client, connection) = tokio::time::timeout(CONNECT_TIMEOUT, config.connect(NoTls))
        .await
        .map_err(|_| StoreError::Timeout("connecting to the database for the writer lock"))??;
    let connection = tokio::spawn(connection);
    let locked: bool = client
        .query_one(TRY_WRITER_LOCK, &[&WRITER_LOCK_ID])
        .await?
        .try_get(0)?;
    if !locked {
        return Err(StoreError::Locked);
    }
    Ok(WriterLock {
        client,
        connection: Some(connection),
        close_reason: None,
    })
}

impl WriterLock {
    pub async fn migrate(&mut self) -> Result<Vec<i64>, StoreError> {
        Ok(up(&mut self.client, MIGRATIONS).await?)
    }

    pub async fn closed(&mut self) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let ended = connection.await;
        self.close_reason = reason_for(ended);
        self.connection = None;
    }

    pub fn close_reason(&self) -> Option<String> {
        self.close_reason.clone()
    }
}

#[derive(Clone)]
pub struct Store {
    pool: Pool,
}

impl Store {
    pub fn new(database_url: &str) -> Result<Store, StoreError> {
        Ok(Store::with_config(pool_config(database_url, POOL_LIMITS)?))
    }

    fn with_config(config: Config) -> Store {
        let manager = Manager::new(config, NoTls);
        let pool = Pool::builder(manager)
            .max_size(POOL_SIZE)
            .runtime(Runtime::Tokio1)
            .create_timeout(Some(CONNECT_TIMEOUT))
            .wait_timeout(Some(CONNECT_TIMEOUT))
            .build()
            .expect("the pool has a runtime for its timeouts");
        Store { pool }
    }

    pub async fn initialize(
        &self,
        new_device_id: String,
        at: DateTime<Utc>,
    ) -> Result<String, StoreError> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        let created = transaction.execute(CREATE_STATE, &[&new_device_id]).await? > 0;
        if created {
            insert_event(&transaction, &welcome(at)).await?;
        }
        let row = transaction.query_one(DEVICE_ID, &[]).await?;
        transaction.commit().await?;
        Ok(row.try_get(0)?)
    }

    pub async fn device_id(&self) -> Result<Option<String>, StoreError> {
        let row = self.client().await?.query_opt(DEVICE_ID, &[]).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(row.try_get(0)?))
    }

    pub async fn sent_keys(&self) -> Result<HashSet<String>, StoreError> {
        let rows = self.client().await?.query(SENT_KEYS, &[]).await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()?)
    }

    pub async fn known_invoices(
        &self,
        course_enrollment_id: &str,
    ) -> Result<Vec<KnownInvoice>, StoreError> {
        let rows = self
            .client()
            .await?
            .query(KNOWN_INVOICES, &[&course_enrollment_id])
            .await?;
        Ok(rows.iter().map(known_invoice).collect::<Result<_, _>>()?)
    }

    pub async fn enrollment(
        &self,
        course_enrollment_id: &str,
    ) -> Result<EnrollmentState, StoreError> {
        let row = self
            .client()
            .await?
            .query_opt(ENROLLMENT, &[&course_enrollment_id])
            .await?;
        let Some(row) = row else {
            return Ok(EnrollmentState::default());
        };
        Ok(EnrollmentState {
            information_type: row.try_get(0)?,
            last_payments_check_at: row.try_get(1)?,
        })
    }

    pub async fn lead_times(&self, course_enrollment_id: &str) -> Result<Vec<i64>, StoreError> {
        let rows = self
            .client()
            .await?
            .query(LEAD_TIMES, &[&course_enrollment_id])
            .await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()?)
    }

    pub async fn record(&self, batch: &RecordBatch) -> Result<Option<i64>, StoreError> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        let locked = transaction.query_opt(LOCK_STATE, &[]).await?;
        if locked.is_none() {
            return Err(StoreError::NotInitialized);
        }
        let mut highest = None;
        for event in &batch.events {
            let inserted = insert_event(&transaction, event).await?;
            highest = highest.max(inserted);
        }
        for key in &batch.sent_keys {
            transaction.execute(INSERT_SENT_KEY, &[key]).await?;
        }
        for invoice in &batch.invoices {
            upsert_invoice(&transaction, invoice).await?;
        }
        for (id, state) in &batch.enrollments {
            transaction
                .execute(
                    UPSERT_ENROLLMENT,
                    &[id, &state.information_type, &state.last_payments_check_at],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(highest)
    }

    pub async fn open_problem(
        &self,
        signature: &str,
        area: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let opened = self
            .client()
            .await?
            .execute(OPEN_PROBLEM, &[&signature, &area, &at])
            .await?;
        Ok(opened > 0)
    }

    pub async fn clear_problems(&self, area: &str) -> Result<(), StoreError> {
        self.client()
            .await?
            .execute(CLEAR_PROBLEMS, &[&area])
            .await?;
        Ok(())
    }

    pub async fn close_problem(&self, signature: &str) -> Result<(), StoreError> {
        self.client()
            .await?
            .execute(CLOSE_PROBLEM, &[&signature])
            .await?;
        Ok(())
    }

    pub async fn open_problems(&self) -> Result<Vec<String>, StoreError> {
        let rows = self.client().await?.query(OPEN_PROBLEMS, &[]).await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()?)
    }

    pub async fn payments_failed(&self, at: DateTime<Utc>) -> Result<PaymentsStreak, StoreError> {
        let row = self
            .client()
            .await?
            .query_opt(PAYMENTS_FAILED, &[&at])
            .await?;
        let row = row.ok_or(StoreError::NotInitialized)?;
        Ok(PaymentsStreak {
            since: row.try_get(0)?,
            alerted: row.try_get(1)?,
        })
    }

    pub async fn mark_blind_alerted(&self) -> Result<(), StoreError> {
        self.client()
            .await?
            .execute(MARK_BLIND_ALERTED, &[])
            .await?;
        Ok(())
    }

    pub async fn payments_succeeded(&self, at: DateTime<Utc>) -> Result<(), StoreError> {
        self.client()
            .await?
            .execute(PAYMENTS_SUCCEEDED, &[&at])
            .await?;
        Ok(())
    }

    pub async fn events_after(
        &self,
        after: i64,
        limit: i64,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let rows = self
            .client()
            .await?
            .query(EVENTS_AFTER, &[&after, &limit])
            .await?;
        Ok(rows.iter().map(stored_event).collect::<Result<_, _>>()?)
    }

    pub async fn ack(&self, seq: i64) -> Result<(), StoreError> {
        self.client().await?.execute(ACK_EVENTS, &[&seq]).await?;
        Ok(())
    }

    pub async fn last_issued_seq(&self) -> Result<i64, StoreError> {
        let row = self.client().await?.query_one(LAST_ISSUED_SEQ, &[]).await?;
        Ok(row.try_get(0)?)
    }

    pub async fn newest_seq(&self) -> Result<i64, StoreError> {
        let row = self.client().await?.query_one(NEWEST_SEQ, &[]).await?;
        Ok(row.try_get(0)?)
    }

    pub async fn last_success(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let row = self.client().await?.query_opt(LAST_SUCCESS, &[]).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(row.try_get(0)?)
    }

    pub async fn prune(&self, keep_events: i64, keep_keys_days: i64) -> Result<(), StoreError> {
        let keys_days = i32::try_from(keep_keys_days).unwrap_or(i32::MAX);
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction.execute(PRUNE_EVENTS, &[&keep_events]).await?;
        transaction.execute(PRUNE_SENT_KEYS, &[&keys_days]).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, StoreError> {
        self.pool.get().await.map_err(pool_error)
    }
}

fn pool_config(database_url: &str, limits: Limits) -> Result<Config, StoreError> {
    let mut config = lock_config(database_url)?;
    let limits = format!(
        "-c statement_timeout={} -c lock_timeout={}",
        limits.statement.as_millis(),
        limits.lock.as_millis()
    );
    let options = after_existing(config.get_options(), limits);
    config.options(options);
    Ok(config)
}

fn after_existing(existing: Option<&str>, settings: String) -> String {
    let Some(existing) = existing.filter(|options| !options.trim().is_empty()) else {
        return settings;
    };
    format!("{existing} {settings}")
}

fn lock_config(database_url: &str) -> Result<Config, StoreError> {
    let mut config = parse(database_url)?;
    config
        .keepalives(true)
        .keepalives_idle(KEEPALIVES_IDLE)
        .keepalives_interval(KEEPALIVES_INTERVAL)
        .keepalives_retries(KEEPALIVES_RETRIES)
        .tcp_user_timeout(TCP_USER_TIMEOUT);
    Ok(config)
}

fn parse(database_url: &str) -> Result<Config, StoreError> {
    database_url.parse().map_err(|_| StoreError::InvalidUrl)
}

fn reason_for(ended: Result<Result<(), tokio_postgres::Error>, JoinError>) -> Option<String> {
    match ended {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(chain_text(&error)),
        Err(error) => Some(chain_text(&error)),
    }
}

pub(crate) fn chain_text(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(current) = cause {
        let shown = current.to_string();
        if !text.ends_with(&shown) {
            text.push_str(": ");
            text.push_str(&shown);
        }
        cause = current.source();
    }
    text
}

fn pool_error(error: PoolError) -> StoreError {
    let PoolError::Timeout(kind) = error else {
        return StoreError::Pool(error);
    };
    StoreError::Timeout(match kind {
        TimeoutType::Wait => "waiting for a free database connection",
        TimeoutType::Create => "connecting to the database",
        TimeoutType::Recycle => "checking a pooled database connection",
    })
}

fn welcome(at: DateTime<Utc>) -> NewEvent {
    NewEvent {
        id: "welcome".to_owned(),
        kind: Kind::Welcome.id(),
        urgent: false,
        title: "Anhanguera monitor is running".to_owned(),
        body: "The monitor is connected and will announce new invoices and reminders here."
            .to_owned(),
        url: String::new(),
        occurred_at: at,
    }
}

async fn insert_event(
    transaction: &Transaction<'_>,
    event: &NewEvent,
) -> Result<Option<i64>, StoreError> {
    let row = transaction
        .query_opt(
            INSERT_EVENT,
            &[
                &event.id,
                &event.kind,
                &event.urgent,
                &event.title,
                &event.body,
                &event.url,
                &event.occurred_at,
            ],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(row.try_get(0)?))
}

async fn upsert_invoice(
    transaction: &Transaction<'_>,
    invoice: &KnownInvoice,
) -> Result<(), StoreError> {
    transaction
        .execute(
            UPSERT_INVOICE,
            &[
                &invoice.id,
                &invoice.course_enrollment_id,
                &invoice.due_date,
                &invoice.status,
                &invoice.first_seen_at,
                &invoice.lead_days,
                &invoice.announced,
                &invoice.closed_at,
                &invoice.overdue_count,
                &invoice.last_overdue_at,
            ],
        )
        .await?;
    Ok(())
}

fn known_invoice(row: &Row) -> Result<KnownInvoice, tokio_postgres::Error> {
    Ok(KnownInvoice {
        id: row.try_get(0)?,
        course_enrollment_id: row.try_get(1)?,
        due_date: row.try_get(2)?,
        status: row.try_get(3)?,
        first_seen_at: row.try_get(4)?,
        lead_days: row.try_get(5)?,
        announced: row.try_get(6)?,
        closed_at: row.try_get(7)?,
        overdue_count: row.try_get(8)?,
        last_overdue_at: row.try_get(9)?,
    })
}

fn stored_event(row: &Row) -> Result<StoredEvent, tokio_postgres::Error> {
    Ok(StoredEvent {
        seq: row.try_get(0)?,
        id: row.try_get(1)?,
        kind: row.try_get(2)?,
        urgent: row.try_get(3)?,
        title: row.try_get(4)?,
        body: row.try_get(5)?,
        url: row.try_get(6)?,
        occurred_at: row.try_get(7)?,
    })
}
