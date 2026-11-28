use sea_orm::{ConnectionTrait, DbErr, DeriveMigrationName};
use sea_orm_migration::async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, SchemaManager};

const CREATE_TABLES: &str = "
CREATE TABLE monitor_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    device_id TEXT NOT NULL,
    acked_seq BIGINT NOT NULL DEFAULT 0,
    last_success_at TIMESTAMPTZ,
    payments_failing_since TIMESTAMPTZ,
    blind_alerted BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE events (
    seq BIGSERIAL PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    urgent BOOLEAN NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    url TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE sent_keys (key TEXT PRIMARY KEY, sent_at TIMESTAMPTZ NOT NULL);

CREATE TABLE invoices (
    id TEXT NOT NULL,
    course_enrollment_id TEXT NOT NULL,
    due_date DATE NOT NULL,
    status TEXT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL,
    lead_days BIGINT,
    announced BOOLEAN NOT NULL DEFAULT FALSE,
    closed_at TIMESTAMPTZ,
    overdue_count BIGINT NOT NULL DEFAULT 0,
    last_overdue_at TIMESTAMPTZ,
    PRIMARY KEY (course_enrollment_id, id)
);

CREATE TABLE enrollments (
    course_enrollment_id TEXT PRIMARY KEY,
    information_type TEXT,
    last_payments_check_at TIMESTAMPTZ
);

CREATE TABLE problems (
    signature TEXT PRIMARY KEY,
    area TEXT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL
);
";

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(CREATE_TABLES)
            .await?;
        Ok(())
    }
}
