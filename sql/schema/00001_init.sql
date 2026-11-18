-- +goose Up
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
    id TEXT PRIMARY KEY,
    course_enrollment_id TEXT NOT NULL,
    due_date DATE NOT NULL,
    status TEXT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL,
    lead_days BIGINT,
    announced BOOLEAN NOT NULL DEFAULT FALSE,
    closed_at TIMESTAMPTZ,
    overdue_count BIGINT NOT NULL DEFAULT 0,
    last_overdue_at TIMESTAMPTZ
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

-- +goose Down
DROP TABLE problems;
DROP TABLE enrollments;
DROP TABLE invoices;
DROP TABLE sent_keys;
DROP TABLE events;
DROP TABLE monitor_state;
