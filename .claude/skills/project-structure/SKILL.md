---
name: project-structure
description: >-
  File structure, module layering and persistence conventions for anhanguera-monitor (a Rust
  monitor-hub monitor: tonic over the SDK, SeaORM on sqlx with sea-orm-migration, Postgres, a
  GraphQL client for the Anhanguera student portal). Read this before creating or
  moving a module, integration test, test helper, migration, GraphQL document or any
  config/scaffolding file (Cargo.toml, rust-toolchain.toml, monitor.json, settings.json,
  Dockerfile, compose.yaml, .github/, .githooks/, .env.example), so new code lands where the rest
  already is.
---

# Project structure: anhanguera-monitor

One crate with a library and a binary. `src/main.rs` only handles the process (arguments, logging,
runtime, exit code), `src/app.rs` puts the pieces together, and each subject has its own module.
Everything is in English: identifiers, files, SQL, logs and user-facing text.

## Directory layout

```
.
├── Cargo.toml                 # package anhanguera-monitor, edition 2024, lib + bin
├── Cargo.lock                 # committed; CI and the image build use --locked
├── rust-toolchain.toml        # the SDK's channel at sdk-rust/v0.4.0, with rustfmt and clippy
├── monitor.json               # the manifest the hub reads: name anhanguera, command anhanguera-monitor monitor
├── settings.json              # the settings declaration (version 2); config.rs embeds it
├── src/
│   ├── main.rs                # monitor, probe and validate-settings arguments, logging, runtime, exit code
│   ├── lib.rs                 # module declarations only
│   ├── app.rs                 # start-up: lock, migrations, first-read seq, state, portal client, serve
│   ├── config.rs              # settings document from the environment, validation, Config
│   ├── secret.rs              # Secret: prints [redacted], zeroed on drop
│   ├── timestamp.rs           # Clock
│   ├── dates.rs               # local dates in AVA_TIMEZONE, portal date parsing, reminder instants, event-text dates
│   ├── text.rs                # course name normalization, one_line cut, a short hash
│   ├── events.rs              # event kinds, ids and the event-text helpers
│   ├── portal.rs              # trait Portal and the calendar categories
│   ├── ava.rs                 # AvaClient: login, token, GraphQL calls, rejected-login backoff
│   ├── ava/                   # model and decoders: strict validation of each answer
│   ├── reminders.rs           # thresholds, the reminder hour gate, the closest-threshold rule
│   ├── payments.rs            # plan_payments: invoices, reminders, billing-day net, payment events
│   ├── coursework.rs          # coursework items, the digest, calendar reminders
│   ├── migration.rs           # Migrator: the sea-orm-migration modules, in order
│   ├── migration/             # goose.rs (adopts the goose schema) and one module per migration
│   ├── store.rs               # Store (SeaORM pool), its queries, StoreError
│   ├── store/                 # connection.rs (URL rules, pool), lock.rs (WriterLock), entity/ (one file per table)
│   ├── monitor.rs             # Monitor: areas, run_cycle, problems, blind and login streaks
│   ├── scheduler.rs           # periodic loop, trigger, CycleState
│   ├── server.rs              # AnhangueraMonitor: the monitor contract over the scheduler and the store
│   ├── probe.rs               # the probe command: response shapes and verdicts
│   └── logtest.rs             # tests only: captures log events on the calling thread
├── queries/                   # the GraphQL documents, embedded with include_str!
├── sql/
│   └── schema/                # goose-format files, the input of the first migration
├── testdata/ava/              # fictitious portal answers for the fake portal
├── tests/
│   ├── support/               # shared helpers, one file per subject (see Testing)
│   ├── ava.rs  store.rs  migration.rs  monitor.rs  scheduler.rs  server.rs  settings.rs  probe.rs
│   └── end_to_end.rs  entrypoint.rs  conformance.rs
├── docs/site-api.md           # facts about the portal's GraphQL API and how each was established
├── .env.example               # every variable, for compose, the tests and a local monitor run
├── .githooks/                 # commit-msg (Conventional Commits) + pre-commit (cargo fmt, cargo test)
├── .github/workflows/         # docker.yml: fmt, clippy and tests against Postgres, then the GHCR image
├── .prototools                # proto: pins Go for the conformance test
├── Dockerfile                 # cross-compiled static musl binary, final stage FROM scratch
├── compose.yaml               # development Postgres only, read-only, no capabilities
└── Makefile                   # check, test, up, down
```

## Modules

- `main.rs` stays thin. It refuses `monitor` outside the hub, starts logging and the runtime, calls
  `app::run`, and turns its error into the single `fatal` log line and exit code 1. `probe` and
  `validate-settings` run without the hub.
- `app.rs` is the only module that knows every other one. Anything that has to happen before the
  handshake goes in `start_up`, inside the 45 s start-up limit, with a step name for its error.
- `monitor.rs` holds the cycle logic and depends on the `Portal` trait, not on `ava::AvaClient`,
  so tests can use a fake portal. `scheduler.rs` decides when cycles run, and `server.rs` maps
  the scheduler and the store onto the monitor contract.
- `payments.rs` and `coursework.rs` are pure planners: they take what the portal said and what the
  store knows, and return the events and keys to record. `monitor.rs` fetches and records.
- A subject module owns its types and its error enum (`thiserror`). Move a helper only when a
  second module needs it.
- `pub` is for what another module or `tests/` uses. The rest is `pub(crate)` or private.
- Comments are short and only where they explain something worth explaining: a guarantee, a rule
  from outside, a pointer to `CLAUDE.md`. None where the code is obvious, and never one that
  repeats the code. `#[cfg(test)] mod tests` at the bottom of a module is where unit tests
  go.

## Persistence

- The monitor reaches Postgres through SeaORM on sqlx. The entities are in `src/store/entity/`, one
  per table. The migrations are `sea-orm-migration` modules in `src/migration/`, run in the order
  of `Migrator::migrations`, and `seaql_migrations` records the ones applied.
- The first migration, `m20261120_000001_goose_schema`, adopts the schema that goose built for
  0.1.0. It reads `goose_db_version` in the current schema and runs the Up section of each
  `sql/schema/` file that table has not recorded. A database without the table is new and gets
  every file. It refuses a gap (`migration version 1 is not applied but the database is already at
  version 2`) and a table with no rows (`goose_db_version has no rows, not even the version 0 row
  goose expects`), accepts versions above 2, and never writes a goose row.
- The second, `m20261120_000002_drop_goose_db_version`, drops `goose_db_version` from that same
  schema, qualified by the schema and without `CASCADE`. If another object depends on the table,
  the drop fails, nothing is dropped, and start-up stops with Postgres's error. The device id and
  every row pass through the two migrations unchanged.
- **Upgrading from 0.1.0.** The first start of this version creates `seaql_migrations` and drops
  `goose_db_version`. The monitor's role therefore needs `CREATE` on the current schema and must
  own `goose_db_version`, which 0.1.0 never needed after its own first start. Without `CREATE` the
  start fails with `applying the database migrations: Execution Error: error returned from
  database: permission denied for schema public`, and without ownership with `... must be owner of
  table goose_db_version`. Either way it changes nothing, but under the hub it becomes a restart
  loop, and no try gets as far as calling the portal. Before the first start, run these as the
  monitor's role:

  ```sql
  SELECT has_schema_privilege(current_user, current_schema(), 'CREATE');
  SELECT tablename, tableowner FROM pg_tables
  WHERE schemaname = current_schema() AND tablename = 'goose_db_version';
  ```

  The first must return `t`, and the second must list the table with the monitor's role as
  `tableowner`. Later starts need the rights 0.1.0 needed on the tables and on `events_seq_seq`,
  and `SELECT` on `seaql_migrations`. Without that table's rights the start reads the migrations
  as pending, tries to create the table and fails with the same `permission denied for schema
  public`, which points at the wrong right. It happens when another role ran the first start:
  grant it. Take a `pg_dump` of the database before the first start, because there is no way back.
- 0.1.0 no longer runs on a database that went through the migrations. Started on one, it exits 1
  with `applying the database migrations: migration 00001_init.sql failed: db error: ERROR:
  relation "monitor_state" already exists` and changes no data. It does leave a new
  `goose_db_version` table with one version-0 row, which this version ignores and never drops,
  because the drop migration is already recorded. Drop it by hand.
- A schema change is a new module in `src/migration/` and a new entry at the end of
  `Migrator::migrations`. **Never edit a migration that already exists, nor the files in
  `sql/schema/`**: they are the first migration's input.
- Start-up runs the migrations on the writer lock's connection, in one transaction that first
  checks that the connection still holds the lock. The monitor reads the pending migrations
  without creating `seaql_migrations`, and sends DDL only when one is pending, so a role with
  table rights only still starts once nothing is pending. Each migration's own transaction
  becomes a savepoint inside the lock's, and a run that fails records nothing. No migration can
  leave that transaction, so a statement Postgres refuses inside a transaction block, such as
  `CREATE INDEX CONCURRENTLY`, cannot be a migration: it makes start-up fail.
- The SQL lives in the store (`src/store.rs` and `src/store/`) and in the migrations
  (`src/migration/`, which reads `sql/schema/`). Most queries go through the entities. These keep
  raw SQL, the same text as before SeaORM: `LOCK_STATE`, `INSERT_SENT_KEY`, `PAYMENTS_FAILED`,
  `LAST_ISSUED_SEQ`, `NEWEST_SEQ`, `PRUNE_EVENTS` and `PRUNE_SENT_KEYS`. Every transaction that
  inserts an event locks the state row first, so the order of `seq` values is the order of the
  commits.
- `Store::record` signals the hub with the new event's `seq` right after the commit that inserted
  it, never before. A failed commit, or a record without a new event, signals nothing.
  `Store::initialize` writes the welcome event without a signal.
- Tables: `monitor_state` (one row: device id, acknowledged `seq`, last payments success, the
  payments failure streak start, whether `monitor_blind` went out), `events` (`seq` from a
  `BIGSERIAL`, a unique `id`), `sent_keys`, `invoices` (one row per enrollment and invoice id, so
  an invoice listed under two enrollments keeps two rows: due date, status, first seen, learned lead
  days, overdue repeat bookkeeping), `enrollments` (last `informationType`, last payments check) and
  `problems` (open `site_changed` signatures). The password and the token are never stored.
  Times are `TIMESTAMPTZ` and map to `DateTime<Utc>`, calendar dates are `DATE`, integers are
  `BIGINT`.
- Migrations carry schema only. `Store::initialize` writes the `monitor_state` row, with the
  device id, at start-up.
- The GraphQL documents in `queries/` are copied from the portal's own client and trimmed. A change
  to one is a change to what the monitor sends, so check it with `probe` before trusting it.
- The pool has 4 connections. Each one runs with a 20 s `statement_timeout` and a 5 s
  `lock_timeout`, asks the server for TCP keepalives (`tcp_keepalives_idle=30`,
  `tcp_keepalives_interval=10`, `tcp_keepalives_count=3`, `tcp_user_timeout=60000`, in ms), and
  is tested before each use. Getting a connection has 10 s. The pool keeps no minimum, idle limit
  or lifetime, so sqlx runs no maintenance task and `Store::new` works outside a runtime. A
  connection the server closed while idle (`idle_session_timeout`, a restart,
  `pg_terminate_backend`) is replaced at its next use, so the role no longer needs
  `idle_session_timeout` at 0. The lock's connection is queried every 0.5 s, so it is never idle for
  longer than that.
- The writer lock (`pg_try_advisory_lock`) has a connection of its own, outside the pool, with the
  same keepalives, no statement or lock timeout because the migrations run there, and
  `client_min_messages=warning`. It is never tested before use: a failed test would quietly open a
  new connection without the lock. Every 0.5 s, a query with 30 s to answer checks in `pg_locks`
  that this connection still holds the lock. sqlx pings the connection each time it goes back to
  that pool, with no limit of its own, so the wait for the connection gets the same 30 s. The limit
  is that long because a false loss stops the process, and the restart logs in to the portal and
  loses the 6 hours of back-off after a rejected login. The monitor gives up on a silent connection
  after about 30.5 s, and the server frees a vanished monitor's lock only after about 60 s, so the
  lock cannot pass to a second copy while the first could still write.

The database layer adds these lines to the log. None carries the address or a password.

- When a migration is pending, sea-orm-migration logs `Applying all pending migrations`, then
  `Applying migration '<name>'` and `Migration '<name>' has been applied` for each one it runs.
  Those lines show even when a later migration fails and the run records nothing. The monitor then
  logs `database migrations applied` with the names it applied, which is `[]` on every start with
  nothing pending, alone. Postgres's notices, such as the migration table's `already exists,
  skipping`, stay out because of `client_min_messages=warning`; warnings and errors still show.
- `ping on idle connection returned error` (`info`, sqlx) appears each time the pool replaces a
  connection the server closed while it was idle. There is one line per replaced connection, and
  it has no address.
- `there is no transaction in progress` (`warn`, `sqlx::postgres::notice`) follows a commit that
  Postgres refused, because SeaORM sends a `ROLLBACK` after it.

## Testing

- Write tests that describe what the code does now.
- Unit tests live in their module. Integration tests in `tests/` go through the public API or
  the built binary (`env!("CARGO_BIN_EXE_anhanguera-monitor")`).
- Every integration test file declares `pub mod support;` (not `mod support;`), and the support
  modules carry no `#[allow]`. One file per subject: `fake_ava.rs` (fake GraphQL portal serving
  `testdata/ava`), `fake_portal.rs` (a `Portal` for cycle tests), `postgres.rs` (the throwaway
  Postgres), `process.rs` (starts the binary, reads the handshake, sends signals), `proxy.rs` (a
  TCP proxy that can stop answering) and `schema.rs` (goose-era databases, schema snapshots).
- Database tests get a database from `support::migrated_database()`, which migrates through
  `Migrator`, or from `support::database()` when the test migrates by itself. The Postgres comes
  from `support/postgres.rs` over TCP, and `support/schema.rs` builds goose-era databases and
  compares schemas. `tests/migration.rs` covers the adoption.
- A test that needs something outside the crate (Postgres, `sh`, the conformance command) prints a
  `skipping:` notice and returns when it is missing, and panics when `CI` is set and not empty.
- No test talks to the real portal. Fixtures are fictitious.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and
  `CI=1 cargo test` must pass.

## Tooling

- The toolchain comes from `rust-toolchain.toml`. Keep its channel equal to the SDK's.
- `sea-orm` and `sea-orm-migration` are pinned with `=` at 2.0.4, with default features off, and
  `chrono`, `chrono-tz` and `unicode-normalization` are pinned with `=` too. sqlx gets no TLS feature:
  `cargo tree -e features -i sqlx-core | grep -c _tls` stays 0.
- After any change to `Cargo.toml`, set these back with `cargo update -p <crate> --precise
  <version>`, one crate per command: `sqlx` 0.9.0, `nix` 0.31.3, `uuid` 1.29.0, `winnow` 1.0.4,
  `toml_edit` 0.25.17+spec-1.1.0, `toml_datetime` 1.1.2+spec-1.1.0, `regex` 1.13.1,
  `regex-automata` 0.4.18 and `regex-syntax` 0.8.11.
- `libsqlite3-sys` is in `Cargo.lock` and never compiled, and so are `sqlx-sqlite`, `sqlx-mysql`
  and the `arrow-*` crates. The sqlx features that sea-orm's defaults turn on carry weak
  `sqlx-sqlite?/...` edges, and cargo keeps the packages those edges name in the lock
  (cargo#10801). `cargo tree --locked -e normal --target all -i libsqlite3-sys` prints nothing.
  An audit that reads the lock still reports RUSTSEC-2026-0459 against `libsqlite3-sys` 0.37.0;
  look again when sqlx 0.9.1 is two weeks old, after 2026-12-02.
- `tokio-postgres` is a dev-dependency only.
- Every dependency is checked on crates.io before it goes into `Cargo.toml`, and every locked
  crate is at least two weeks old.
- The image builds on the build machine's platform with `cargo zigbuild` for
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. The final stage has only `COPY`
  instructions, so no emulation is needed. It carries `monitor.json`, `settings.json` and the
  binary.
- Never change the versions pinned in `.prototools`.
