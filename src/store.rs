mod connection;
mod entity;

use std::collections::HashSet;
use std::error::Error;
use std::time::Duration;

use crate::events::{Kind, NewEvent};
use chrono::{DateTime, NaiveDate, Utc};
use connection::{CONNECT_TIMEOUT, Limits, POOL_LIMITS, database_options, pool};
use entity::{enrollments, events, invoices, monitor_state, problems, sent_keys};
use monitor_hub_plugin::Notifier;
use monitor_hub_plugin::migrate::{MigrateError, Migration, up};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnAcquireErr, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, DbBackend, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    RuntimeErr, Statement, TransactionTrait, TryInsertResult, Value,
};
use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::{Client, Config, NoTls};

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

const NOT_A_POSTGRES_ADDRESS: &str = "DATABASE_URL is not a valid Postgres address";
const KEEPALIVES_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVES_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVES_RETRIES: u32 = 3;
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(60);
const LEAD_TIMES_KEPT: u64 = 6;

const TRY_WRITER_LOCK: &str = "SELECT pg_try_advisory_lock($1)";

// Raw SQL on purpose. LOCK_STATE locks the state row before each event's INSERT, so the order of
// the seq values is the order of the commits. Sent keys take the server's clock, PAYMENTS_FAILED
// reads both columns back in one statement, and the rest read the sequence or use subqueries.
const LOCK_STATE: &str = "SELECT 1 FROM monitor_state FOR UPDATE";
const INSERT_SENT_KEY: &str = "INSERT INTO sent_keys (key, sent_at) VALUES ($1, now())
ON CONFLICT (key) DO NOTHING";
const PAYMENTS_FAILED: &str = "UPDATE monitor_state
SET payments_failing_since = COALESCE(payments_failing_since, $1)
RETURNING payments_failing_since, blind_alerted";
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
    #[error("{0}")]
    InvalidUrl(&'static str),
    #[error("the monitor state was never initialized")]
    NotInitialized,
    #[error("another anhanguera-monitor process holds the database lock")]
    Locked,
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error(transparent)]
    Database(DbErr),
    #[error(transparent)]
    LockConnection(tokio_postgres::Error),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

impl From<DbErr> for StoreError {
    fn from(error: DbErr) -> Self {
        let Some(step) = timed_out_step(&error) else {
            return StoreError::Database(error);
        };
        StoreError::Timeout(step)
    }
}

impl From<tokio_postgres::Error> for StoreError {
    fn from(error: tokio_postgres::Error) -> Self {
        StoreError::LockConnection(error)
    }
}

fn timed_out_step(error: &DbErr) -> Option<&'static str> {
    if matches!(error, DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)) {
        return Some("getting a database connection");
    }
    let timeouts = [
        ("57014", "running a database statement"),
        ("55P03", "waiting for a database lock"),
    ];
    let code = sql_state(error)?;
    timeouts
        .into_iter()
        .find(|(timeout, _)| code == *timeout)
        .map(|(_, step)| step)
}

fn sql_state(error: &DbErr) -> Option<String> {
    let (DbErr::Exec(RuntimeErr::SqlxError(error))
    | DbErr::Query(RuntimeErr::SqlxError(error))
    | DbErr::Conn(RuntimeErr::SqlxError(error))) = error
    else {
        return None;
    };
    Some(error.as_database_error()?.code()?.into_owned())
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
    connection: DatabaseConnection,
    notifier: Notifier,
}

impl Store {
    pub fn new(database_url: &str) -> Result<Store, StoreError> {
        Store::with_limits(database_url, POOL_LIMITS)
    }

    fn with_limits(database_url: &str, limits: Limits) -> Result<Store, StoreError> {
        let options = connection::with_limits(database_options(database_url)?, limits);
        Ok(Store {
            connection: pool(options),
            notifier: Notifier::new(),
        })
    }

    pub(crate) fn with_notifier(self, notifier: Notifier) -> Store {
        Store { notifier, ..self }
    }

    pub fn notifier(&self) -> &Notifier {
        &self.notifier
    }

    #[cfg(test)]
    fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }

    // No signal for the welcome: it is written at start-up, before the hub can read, and the
    // start-up signal and the end of the first read announce it.
    pub async fn initialize(
        &self,
        new_device_id: String,
        at: DateTime<Utc>,
    ) -> Result<String, StoreError> {
        let transaction = self.connection.begin().await?;
        let state = monitor_state::ActiveModel {
            device_id: Set(new_device_id),
            ..Default::default()
        };
        let created = monitor_state::Entity::insert(state)
            .on_conflict(
                OnConflict::column(monitor_state::Column::Singleton)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await?
            > 0;
        if created {
            insert_event(&transaction, &welcome(at)).await?;
        }
        let device_id = device_id(&transaction)
            .await?
            .ok_or(StoreError::NotInitialized)?;
        transaction.commit().await?;
        Ok(device_id)
    }

    pub async fn device_id(&self) -> Result<Option<String>, StoreError> {
        device_id(&self.connection).await
    }

    pub async fn sent_keys(&self) -> Result<HashSet<String>, StoreError> {
        let keys: Vec<String> = sent_keys::Entity::find()
            .select_only()
            .column(sent_keys::Column::Key)
            .into_tuple()
            .all(&self.connection)
            .await?;
        Ok(keys.into_iter().collect())
    }

    pub async fn known_invoices(
        &self,
        course_enrollment_id: &str,
    ) -> Result<Vec<KnownInvoice>, StoreError> {
        let rows = invoices::Entity::find()
            .filter(invoices::Column::CourseEnrollmentId.eq(course_enrollment_id))
            .order_by_asc(invoices::Column::DueDate)
            .order_by_asc(invoices::Column::Id)
            .all(&self.connection)
            .await?;
        Ok(rows.into_iter().map(KnownInvoice::from).collect())
    }

    pub async fn enrollment(
        &self,
        course_enrollment_id: &str,
    ) -> Result<EnrollmentState, StoreError> {
        let row = enrollments::Entity::find_by_id(course_enrollment_id.to_owned())
            .one(&self.connection)
            .await?;
        let Some(row) = row else {
            return Ok(EnrollmentState::default());
        };
        Ok(EnrollmentState {
            information_type: row.information_type,
            last_payments_check_at: row.last_payments_check_at,
        })
    }

    pub async fn lead_times(&self, course_enrollment_id: &str) -> Result<Vec<i64>, StoreError> {
        Ok(invoices::Entity::find()
            .select_only()
            .column(invoices::Column::LeadDays)
            .filter(invoices::Column::CourseEnrollmentId.eq(course_enrollment_id))
            .filter(invoices::Column::LeadDays.is_not_null())
            .order_by_desc(invoices::Column::FirstSeenAt)
            .order_by_desc(invoices::Column::Id)
            .limit(LEAD_TIMES_KEPT)
            .into_tuple()
            .all(&self.connection)
            .await?)
    }

    pub async fn record(&self, batch: &RecordBatch) -> Result<Option<i64>, StoreError> {
        let transaction = self.connection.begin().await?;
        let locked = transaction.query_one_raw(statement(LOCK_STATE, [])).await?;
        if locked.is_none() {
            return Err(StoreError::NotInitialized);
        }
        let mut highest = None;
        for event in &batch.events {
            let inserted = insert_event(&transaction, event).await?;
            highest = highest.max(inserted);
        }
        for key in &batch.sent_keys {
            transaction
                .execute_raw(statement(INSERT_SENT_KEY, [key.as_str().into()]))
                .await?;
        }
        for invoice in &batch.invoices {
            upsert_invoice(&transaction, invoice).await?;
        }
        for (id, state) in &batch.enrollments {
            upsert_enrollment(&transaction, id, state).await?;
        }
        transaction.commit().await?;
        let Some(seq) = highest else {
            return Ok(None);
        };
        self.notifier.notify(seq);
        Ok(Some(seq))
    }

    pub async fn open_problem(
        &self,
        signature: &str,
        area: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let problem = problems::ActiveModel {
            signature: Set(signature.to_owned()),
            area: Set(area.to_owned()),
            first_seen_at: Set(at),
        };
        let opened = problems::Entity::insert(problem)
            .on_conflict_do_nothing_on([problems::Column::Signature])
            .exec_without_returning(&self.connection)
            .await?;
        Ok(matches!(opened, TryInsertResult::Inserted(rows) if rows > 0))
    }

    pub async fn clear_problems(&self, area: &str) -> Result<(), StoreError> {
        problems::Entity::delete_many()
            .filter(problems::Column::Area.eq(area))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn close_problem(&self, signature: &str) -> Result<(), StoreError> {
        problems::Entity::delete_many()
            .filter(problems::Column::Signature.eq(signature))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn open_problems(&self) -> Result<Vec<String>, StoreError> {
        Ok(problems::Entity::find()
            .select_only()
            .column(problems::Column::Signature)
            .order_by_asc(problems::Column::Signature)
            .into_tuple()
            .all(&self.connection)
            .await?)
    }

    pub async fn payments_failed(&self, at: DateTime<Utc>) -> Result<PaymentsStreak, StoreError> {
        let row = self
            .connection
            .query_one_raw(statement(PAYMENTS_FAILED, [at.into()]))
            .await?;
        let row = row.ok_or(StoreError::NotInitialized)?;
        Ok(PaymentsStreak {
            since: row.try_get_by_index(0)?,
            alerted: row.try_get_by_index(1)?,
        })
    }

    pub async fn mark_blind_alerted(&self) -> Result<(), StoreError> {
        monitor_state::Entity::update_many()
            .col_expr(monitor_state::Column::BlindAlerted, Expr::value(true))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn payments_succeeded(&self, at: DateTime<Utc>) -> Result<(), StoreError> {
        let cleared: Option<DateTime<Utc>> = None;
        monitor_state::Entity::update_many()
            .col_expr(
                monitor_state::Column::PaymentsFailingSince,
                Expr::value(cleared),
            )
            .col_expr(monitor_state::Column::BlindAlerted, Expr::value(false))
            .col_expr(monitor_state::Column::LastSuccessAt, Expr::value(at))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn events_after(
        &self,
        after: i64,
        limit: i64,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let rows = events::Entity::find()
            .filter(events::Column::Seq.gt(after))
            .order_by_asc(events::Column::Seq)
            .limit(u64::try_from(limit).unwrap_or(0))
            .all(&self.connection)
            .await?;
        Ok(rows.into_iter().map(StoredEvent::from).collect())
    }

    pub async fn ack(&self, seq: i64) -> Result<(), StoreError> {
        monitor_state::Entity::update_many()
            .col_expr(monitor_state::Column::AckedSeq, Expr::value(seq))
            .filter(monitor_state::Column::AckedSeq.lt(seq))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn last_issued_seq(&self) -> Result<i64, StoreError> {
        self.read_seq(LAST_ISSUED_SEQ).await
    }

    pub async fn newest_seq(&self) -> Result<i64, StoreError> {
        self.read_seq(NEWEST_SEQ).await
    }

    pub async fn last_success(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let last: Option<Option<DateTime<Utc>>> = monitor_state::Entity::find()
            .select_only()
            .column(monitor_state::Column::LastSuccessAt)
            .into_tuple()
            .one(&self.connection)
            .await?;
        Ok(last.flatten())
    }

    pub async fn prune(&self, keep_events: i64, keep_keys_days: i64) -> Result<(), StoreError> {
        let keys_days = i32::try_from(keep_keys_days).unwrap_or(i32::MAX);
        let transaction = self.connection.begin().await?;
        transaction
            .execute_raw(statement(PRUNE_EVENTS, [keep_events.into()]))
            .await?;
        transaction
            .execute_raw(statement(PRUNE_SENT_KEYS, [keys_days.into()]))
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn read_seq(&self, sql: &str) -> Result<i64, StoreError> {
        let row = self.connection.query_one_raw(statement(sql, [])).await?;
        let row = row.ok_or_else(|| DbErr::RecordNotFound("a sequence read gave no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
}

fn statement<const N: usize>(sql: &str, values: [Value; N]) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn device_id(connection: &impl ConnectionTrait) -> Result<Option<String>, StoreError> {
    Ok(monitor_state::Entity::find()
        .select_only()
        .column(monitor_state::Column::DeviceId)
        .into_tuple()
        .one(connection)
        .await?)
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
    database_url
        .parse()
        .map_err(|_| StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS))
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
    transaction: &DatabaseTransaction,
    event: &NewEvent,
) -> Result<Option<i64>, StoreError> {
    let row = events::ActiveModel {
        id: Set(event.id.clone()),
        kind: Set(event.kind.to_owned()),
        urgent: Set(event.urgent),
        title: Set(event.title.clone()),
        body: Set(event.body.clone()),
        url: Set(event.url.clone()),
        occurred_at: Set(event.occurred_at),
        ..Default::default()
    };
    let inserted = events::Entity::insert(row)
        .on_conflict_do_nothing_on([events::Column::Id])
        .exec(transaction)
        .await?;
    let TryInsertResult::Inserted(inserted) = inserted else {
        return Ok(None);
    };
    Ok(Some(inserted.last_insert_id))
}

async fn upsert_invoice(
    transaction: &DatabaseTransaction,
    invoice: &KnownInvoice,
) -> Result<(), StoreError> {
    let row = invoices::ActiveModel {
        course_enrollment_id: Set(invoice.course_enrollment_id.clone()),
        id: Set(invoice.id.clone()),
        due_date: Set(invoice.due_date),
        status: Set(invoice.status.clone()),
        first_seen_at: Set(invoice.first_seen_at),
        lead_days: Set(invoice.lead_days),
        announced: Set(invoice.announced),
        closed_at: Set(invoice.closed_at),
        overdue_count: Set(invoice.overdue_count),
        last_overdue_at: Set(invoice.last_overdue_at),
    };
    let upsert = OnConflict::columns([invoices::Column::CourseEnrollmentId, invoices::Column::Id])
        .update_columns([
            invoices::Column::DueDate,
            invoices::Column::Status,
            invoices::Column::FirstSeenAt,
            invoices::Column::LeadDays,
            invoices::Column::Announced,
            invoices::Column::ClosedAt,
            invoices::Column::OverdueCount,
            invoices::Column::LastOverdueAt,
        ])
        .to_owned();
    invoices::Entity::insert(row)
        .on_conflict(upsert)
        .exec_without_returning(transaction)
        .await?;
    Ok(())
}

async fn upsert_enrollment(
    transaction: &DatabaseTransaction,
    course_enrollment_id: &str,
    state: &EnrollmentState,
) -> Result<(), StoreError> {
    let row = enrollments::ActiveModel {
        course_enrollment_id: Set(course_enrollment_id.to_owned()),
        information_type: Set(state.information_type.clone()),
        last_payments_check_at: Set(state.last_payments_check_at),
    };
    let upsert = OnConflict::column(enrollments::Column::CourseEnrollmentId)
        .update_columns([
            enrollments::Column::InformationType,
            enrollments::Column::LastPaymentsCheckAt,
        ])
        .to_owned();
    enrollments::Entity::insert(row)
        .on_conflict(upsert)
        .exec_without_returning(transaction)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};
    use tokio_postgres::NoTls;

    use super::connection::{Limits, POOL_LIMITS, database_options, with_limits};
    use super::{Store, StoreError};
    use crate::logtest::captured;
    use crate::postgres::database;

    const URL: &str = "postgres://anhanguera:hunter2@db.internal:5432/anhanguera";
    const POOL_OPTIONS: &str = "-c tcp_keepalives_idle=30 -c tcp_keepalives_interval=10 -c tcp_keepalives_count=3 -c tcp_user_timeout=60000 -c statement_timeout=20000 -c lock_timeout=5000";
    const NOT_AN_ADDRESS: &str = "DATABASE_URL is not a valid Postgres address";
    const NO_PASSWORD: &str = "DATABASE_URL has no password: add the role's password to the URL";
    const SHORT: Limits = Limits {
        statement: Duration::from_millis(200),
        lock: Duration::from_millis(200),
    };
    const WAIT: Duration = Duration::from_secs(5);

    fn with_query(url: &str, parameter: &str) -> String {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{url}{separator}{parameter}")
    }

    async fn show(connection: &DatabaseConnection, setting: &str) -> String {
        let statement = Statement::from_string(DbBackend::Postgres, format!("SHOW {setting}"));
        connection
            .query_one_raw(statement)
            .await
            .expect("show")
            .expect("a row")
            .try_get_by_index(0)
            .expect("the setting")
    }

    async fn backend_pid(connection: &DatabaseConnection) -> Result<i32, DbErr> {
        let statement = Statement::from_string(DbBackend::Postgres, "SELECT pg_backend_pid()");
        let row = connection.query_one_raw(statement).await?;
        row.expect("a row").try_get_by_index(0)
    }

    #[test]
    fn pool_connections_get_limits_and_server_keepalives() {
        let options = with_limits(database_options(URL).expect("options"), POOL_LIMITS);
        assert_eq!(options.get_options(), Some(POOL_OPTIONS));
    }

    #[test]
    fn the_store_settings_come_after_the_urls_own() {
        let url = with_query(URL, "options=-c%20statement_timeout%3D1&keepalives_idle=5");
        let options = with_limits(database_options(&url).expect("options"), POOL_LIMITS);
        assert_eq!(
            options.get_options(),
            Some(format!("-c statement_timeout=1 {POOL_OPTIONS}").as_str())
        );
    }

    #[test]
    fn database_options_accept_only_the_documented_forms() {
        let accepted = [
            "postgres://anhanguera:hunter2@db.internal:5432/anhanguera?sslmode=disable",
            "postgresql://anhanguera:hunter2@db.internal/anhanguera",
        ];
        for url in accepted {
            let options = database_options(url).expect(url);
            assert_eq!(options.get_host(), "db.internal", "{url}");
            assert_eq!(options.get_database(), Some("anhanguera"), "{url}");
        }
        let ipv6 =
            database_options("postgres://anhanguera:hunter2@[::1]:5432/anhanguera").expect("IPv6");
        assert_eq!(ipv6.get_host(), "::1");
        let dropped = database_options(&with_query(URL, "connect_timeout=5&keepalives=1"))
            .expect("keys sqlx has no equivalent for");
        let plain = database_options(URL).expect("options");
        assert_eq!(format!("{dropped:?}"), format!("{plain:?}"));

        let refused = [
            "host=db user=anhanguera dbname=anhanguera",
            "postgres://anhanguera:hunter2@h1,h2/anhanguera",
            "postgres://anhanguera:hunter2@h1:5432,h2:5432/anhanguera",
            "mysql://anhanguera:hunter2@db/anhanguera",
            "postgres://anhanguera:hunter2@[bad",
            "postgres://anhanguera:hunter2@/anhanguera",
            "postgres:///anhanguera?host=",
            "postgres://anhanguera:hunter2@db/anhanguera?sslmode=sometimes",
            "postgres://anhanguera:hunter2@db/anhanguera?sslpassword=x",
            "postgres://anhanguera:hunter2@db/anhanguera?Password=x",
        ];
        for url in refused {
            let error = database_options(url).expect_err(url);
            assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
            assert_eq!(error.to_string(), NOT_AN_ADDRESS, "{url}");
        }
        let without_password = [
            "postgres://anhanguera@db/anhanguera",
            "postgres://anhanguera:@db/anhanguera",
            "postgres://anhanguera@db/anhanguera?password=",
        ];
        for url in without_password {
            let error = database_options(url).expect_err(url);
            assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
            assert_eq!(error.to_string(), NO_PASSWORD, "{url}");
        }
        let not_honoured = [
            (
                "channel_binding=require",
                "DATABASE_URL's channel_binding needs TLS, and the monitor connects without TLS",
            ),
            (
                "sslnegotiation=direct",
                "DATABASE_URL's sslnegotiation needs TLS, and the monitor connects without TLS",
            ),
            (
                "target_session_attrs=read-write",
                "DATABASE_URL's target_session_attrs needs a session check the monitor does not run",
            ),
        ];
        for (parameter, text) in not_honoured {
            let url = with_query(URL, parameter);
            let error = database_options(&url).expect_err(&url);
            assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
            assert_eq!(error.to_string(), text, "{url}");
        }
    }

    #[test]
    fn url_query_values_never_reach_the_log() {
        let value = "s3cr3t-value";
        let keys = [
            "sslnegotiation",
            "connect_timeout",
            "tcp_user_timeout",
            "keepalives",
            "keepalives_idle",
            "keepalives_interval",
            "keepalives_retries",
            "target_session_attrs",
            "channel_binding",
            "load_balance_hosts",
            "sslpassword",
            "Password",
            "passfile",
        ];
        let mut errors = Vec::new();
        let events = captured(|| {
            for key in keys {
                let parsed = database_options(&with_query(URL, &format!("{key}={value}")));
                errors.extend(parsed.err().map(|error| format!("{error} {error:?}")));
            }
        });
        let leaked: Vec<&(String, String)> = events
            .iter()
            .flatten()
            .filter(|(_, logged)| logged.contains(value))
            .collect();
        assert!(leaked.is_empty(), "{leaked:?}");
        assert!(
            errors.iter().all(|error| !error.contains(value)),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn pool_connections_carry_their_limits_and_keepalives() {
        let Some(db) = database().await else {
            return;
        };
        let store = Store::new(&db.url).expect("store");
        let settings = [
            ("statement_timeout", "20s"),
            ("lock_timeout", "5s"),
            ("tcp_keepalives_idle", "30"),
            ("tcp_keepalives_interval", "10"),
            ("tcp_keepalives_count", "3"),
            ("tcp_user_timeout", "60000"),
        ];
        for (setting, value) in settings {
            assert_eq!(show(store.connection(), setting).await, value, "{setting}");
        }
    }

    #[tokio::test]
    async fn statement_timeouts_are_reported_as_timeouts() {
        let Some(db) = database().await else {
            return;
        };
        let store = Store::with_limits(&db.url, SHORT).expect("store");
        let sleep = Statement::from_string(DbBackend::Postgres, "SELECT pg_sleep(30)");
        let error = tokio::time::timeout(WAIT, store.connection().query_one_raw(sleep))
            .await
            .expect("the server ended the statement within its limit")
            .expect_err("the statement outlived its limit");
        let error = StoreError::from(error);
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_eq!(error.to_string(), "running a database statement timed out");
    }

    #[tokio::test]
    async fn pooled_connections_are_tested_before_use() {
        let Some(db) = database().await else {
            return;
        };
        let store = Store::new(&db.url).expect("store");
        let pooled = backend_pid(store.connection())
            .await
            .expect("a call that leaves one pooled connection");
        let (admin, connection) = tokio_postgres::connect(&db.url, NoTls)
            .await
            .expect("connect");
        tokio::spawn(connection);
        // With a timeout, pg_terminate_backend returns only once the backend has exited.
        let ended: bool = admin
            .query_one("SELECT pg_terminate_backend($1, 5000)", &[&pooled])
            .await
            .expect("end the pooled connection's backend")
            .get(0);
        assert!(ended, "the pooled backend did not exit");

        let next = backend_pid(store.connection()).await;

        assert!(next.as_ref().is_ok_and(|pid| *pid != pooled), "{next:?}");
    }
}
