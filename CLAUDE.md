# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# anhanguera-monitor

A monitor-hub monitor. It reads a student's account on the Anhanguera student portal (the AVA) and
records events: an invoice to pay, a payment about to fall due, coursework about to close, and an
urgent event whenever it cannot read the portal. The hub reads those events and sends the
notifications. The monitor is written in Rust on the monitor-hub plugin SDK (`monitor-hub-plugin`,
git tag `sdk-rust/v0.3.0`). Protocol-bound names keep the word "plugin": `plugin.json`, the
`plugin` argument, `/plugin/` in the image, the SDK's `Plugin` trait.

Payments come first. Coursework follows, for the courses named in `COURSES`. The academic
calendar is a safety net. When the monitor cannot read the portal it says so: a page it could not
parse is reported, never read as "nothing to pay". It only reads, and writes nothing on the portal.

The image does not run on its own. It holds `/plugin/plugin.json`, `/plugin/settings.json` and
`/plugin/anhanguera-monitor`, and the hub's image build copies that directory in.

**Before creating or moving a module, test file, migration, GraphQL document or any
config/scaffolding file, read `.claude/skills/project-structure/SKILL.md`.** It holds the full
layout and the persistence workflow.

**The API facts live in `docs/site-api.md`**: the endpoint, the login, each operation's shape, the
enums, and how each fact was established. Read it before touching `queries/` or `src/ava/`.

## Code

- **Keep comments short, and write one only where it explains something worth explaining**: a
  guarantee, a rule that comes from outside the code, a pointer to this file. Leave obvious code
  without one, and never write a comment that repeats the code. Attributes such as `#[cfg(test)]`
  are not comments.
- **No `#[allow]` or `#[expect]`, in tests either.** Every integration test file declares
  `pub mod support;`.
- **English everywhere**: identifiers, file names, SQL, logs, error messages, event text and the
  docs.
- Only make `pub` what another module or the integration tests in `tests/` use. Everything else is
  `pub(crate)` or private.
- `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings` stay clean.
- Prefer guard clauses and small functions over `else` branches, and a map over a chain of
  `match`/`if` arms when the code only maps keys to values.
- Run independent async calls concurrently (`tokio::join!`, `try_join!`, `join_all`, buffered
  streams). With partial dependencies, run each dependency chain concurrently.
- All time is UTC (`chrono::Utc`). Calendar dates and reminder hours use `AVA_TIMEZONE`, never the
  process's `TZ`.
- The CPF, the password and the access token never appear in a log, an error, an event, a `Debug`
  output, a test fixture, an argument or the repository. `Secret` prints `[redacted]` and zeroes on
  drop, and the configuration types do not derive `Debug`.
- Fixtures and examples use fictitious values. No test and no command in this repository talks to
  the real portal.
- Nothing in the tree names an owner, author or company, and there is no license file.

## Commands

```bash
cargo build
cargo test                    # see Tests for what needs Postgres, sh or go
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
make check                    # fmt + clippy + test
make up                       # docker compose up -d: development Postgres on 127.0.0.1:5432
make down
docker buildx build --platform linux/amd64,linux/arm64 .
git config core.hooksPath .githooks   # once per clone
```

The pre-commit hook runs `cargo fmt --check` and `cargo test`. The commit-msg hook enforces
Conventional Commits.

## Tests

Some tests need tools from outside the crate. When one is missing they print a `skipping:` notice
on stderr and return. When `CI` is set and not empty they panic instead, so CI never passes by
skipping.

- Postgres: `tests/support/postgres.rs`'s `database()` gives each test its own database. It uses
  `TEST_DATABASE_URL` when set (the compose Postgres works, see `.env.example`), and otherwise
  starts a throwaway cluster with `initdb` and `postgres` from `PATH`, listening on a local TCP
  port. `TEST_DATABASE_URL` must be a `postgres://` URL with a password, like `DATABASE_URL`, and
  its role must be able to create databases. The throwaway cluster trusts every login, so its URL
  carries a password that Postgres ignores. `tests/support::migrated_database` adds the migrations.
- `tests/migration.rs` builds databases the way goose did (versions 1 and 2, above 2, with a gap,
  with an empty `goose_db_version`) and checks that the first migration adopts them without
  losing a row, and that the second drops `goose_db_version`.
- `sh` with `kill` for the tests that start the binary and signal it.
- The conformance command for `tests/conformance.rs`: the binary named by `PLUGIN_CONFORMANCE`,
  or else `go install` of `plugin-conformance` at the SDK's Go tag `sdk-go/v0.4.0`, cached under
  `~/.cache/monitor-hub/plugin-conformance/`.
- The portal is a fake GraphQL server (`tests/support/fake_ava.rs`) that serves the fictitious
  answers in `testdata/ava/`. Cycle tests use a fake `Portal` (`tests/support/fake_portal.rs`).

The binary of `tests/store.rs` takes about 33 s, because two tests pause a TCP proxy
(`tests/support/proxy.rs`) and wait for the lock's 30 s loss limit to run out.

The SDK refuses Unix socket paths longer than 100 bytes, so a long `TMPDIR` breaks the tests that
start the plugin. Run them with a short one (`TMPDIR=/tmp`). The same holds under the hub, where
`TMPDIR` is `/tmp`.

CI (`.github/workflows/docker.yml`) runs `cargo fmt --check`, clippy and `cargo test --locked`
against a `postgres:18-alpine` service, with Go 1.27.2 for the conformance command. On a push to
`main` the `release` job then computes the next version from the Conventional Commits since the last
`v*` tag (`mathieudutour/github-tag-action`): `feat` is minor; `fix`, `perf`, `chore`, `refactor` and
`build` are patch; a breaking `feat!`, `fix!` or `perf!` (or a `BREAKING CHANGE:` footer on those
types) is major, while a breaking `chore!`, `refactor!` or `build!` stays a patch; `docs`, `test`,
`ci` and `style` make no release. It tags `vX.Y.Z` and creates a GitHub Release with the changelog.
A tag pushed with `GITHUB_TOKEN` starts no new run, so the `image` job builds in the same run and
takes the version from the `release` job. `Cargo.toml`'s version is not updated (`publish = false`).
The image for amd64 and arm64 goes to `ghcr.io` on every run that is not a pull request: pushes to
`main`, `v*` tags and manual runs. A release gets the version tags (`2.1.0` and `2.1`), only the
default branch moves `latest`, and every image gets a `sha-` tag. A `v*` tag pushed by hand still
publishes its version tags. The workflow's `concurrency` serializes `main` pushes so two runs never
compute the same tag.

## Configuration

`settings.json` (version 2) declares every setting, with its default, and `src/config.rs` embeds
it. Under monitor-hub, set them on the hub with the `ANHANGUERA__` prefix
(`ANHANGUERA__COURSES` and so on). The hub removes the prefix before it starts the monitor. The
monitor reads only its environment, never a `.env` file. `.env.example` lists every variable
except `AVA_GRAPHQL_URL`, which only the tests set. The password is set by `AVA_PASSWORD` or
`AVA_PASSWORD_FILE`, the database address by `DATABASE_URL` or `DATABASE_URL_FILE`, never both.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` / `DATABASE_URL_FILE` | required, secret | Postgres address with the role's password, connected without TLS. See below. |
| `AVA_USERNAME` | required, secret | The student's CPF. Dots, dashes and spaces are ignored; it must be 11 digits. |
| `AVA_PASSWORD` / `AVA_PASSWORD_FILE` | exactly one, secret | The portal password. The file variant is not editable from the hub. |
| `COURSES` | required | Names of the courses to watch for coursework. Matching ignores case, accents and repeated whitespace. |
| `CHECK_INTERVAL` | `1h` | How often to read the portal. At least `15m`. |
| `PAYMENT_REMINDER_DAYS` | `7,3,1,0` | Days before the due date to remind, each from 0 to 60. |
| `PAYMENT_OVERDUE_REPEAT` | `24h` | How often to repeat the reminder of an overdue invoice. `0` sends one and no repeats. |
| `BOLETO_LINK` | `true` | Put the boleto link in payment events. |
| `BILLING_DAY_NET` | `auto` | `auto` (learned, 3 days until learned), a day count from 1 to 28, or `off`. |
| `DEADLINE_REMINDER_DAYS` | `7,3,1` | Days before a coursework deadline to remind, each from 0 to 60. |
| `CALENDAR_CATEGORIES` | `FINANCIAL,SECRETARIAT,UNIQUE_EVENTS` | Subset of those three; empty means none. |
| `REMINDER_HOUR` | `9` | Local hour, 0 to 23, from which scheduled reminders go out. |
| `FAILING_ALERT_AFTER` | `24h` | How long payments must keep failing before `monitor_blind`. At least `1h`. |
| `AVA_TIMEZONE` | `America/Sao_Paulo` | An IANA name. Nothing reads `TZ`. |
| `LOG_LEVEL` | `info` | `trace`, `debug`, `info`, `warn` or `error`. |
| `AVA_GRAPHQL_URL` | the portal's endpoint | Tests only. Not editable from the hub. |

`DATABASE_URL` is `postgres://user:password@host:port/database?sslmode=disable` (`postgresql://`
works too). Put the host, port, user, password and database in the URL.

- The password goes in the address or in a `password` query key. A URL without one, or with an
  empty one, is refused with `DATABASE_URL has no password: add the role's password to the URL`.
  Without a password in the URL, sqlx would read `PGPASSWORD`, `PGPASSFILE` or `~/.pgpass`, and it
  logs a malformed `.pgpass` line whole at `warn`. A line missing a field, such as `*:*:*:secret`,
  puts the password where sqlx expects the user name, so the log would show it. With the refusal in
  place, the monitor never uses `PGPASSWORD` and never reads a `.pgpass` file. A database that
  trusts the role still needs a password in the URL, and Postgres ignores it.
- Whatever else the URL leaves out, sqlx takes from the `PG*` variables (`PGPORT`, `PGUSER`,
  `PGDATABASE`, `PGOPTIONS`, `PGSSLMODE` and the rest), then the user from the system account.
  Under the hub the `PG*` variables only arrive when set as `ANHANGUERA__PG*`.
- The monitor connects without TLS. `sslmode=disable`, `allow` and `prefer` connect in plain text.
  `require`, `verify-ca` and `verify-full` fail at once, with sqlx's message that it was built
  without TLS support.
- These are refused with `DATABASE_URL is not a valid Postgres address`, which never repeats the
  address: the key=value form (`host=db user=anhanguera`), several hosts (`h1,h2`), a URL with no
  host (unless a `host` or `hostaddr` query key names one), any scheme other than the two above,
  and any query key the monitor does not know. sqlx would log an unknown key together with its
  value, and that value can be a secret (`sslpassword`, a misspelled `Password`). Key names are
  case sensitive.
- The monitor passes on the query keys sqlx reads: `sslmode`, `options`, `application_name`,
  `host`, `hostaddr`, `port`, `dbname`, `user`, `password`, the certificate keys (`sslrootcert`,
  `sslcert`, `sslkey` and their dashed forms) and `statement-cache-capacity`.
- It accepts and ignores the libpq keys that 0.1.0 took and sqlx has no equivalent for:
  `connect_timeout`, `tcp_user_timeout`, `keepalives`, `keepalives_idle`, `keepalives_interval`,
  `keepalives_retries` and `load_balance_hosts`, plus `channel_binding=disable` or `prefer`,
  `sslnegotiation=postgres` and `target_session_attrs=any`. Any other value of those last three
  stops the start with a message that names the key and never its value: `DATABASE_URL's
  channel_binding needs TLS, and the monitor connects without TLS`, the same for `sslnegotiation`,
  and `DATABASE_URL's target_session_attrs needs a session check the monitor does not run`.
- An IPv6 host goes in brackets: `postgres://user:password@[::1]:5432/database`.
- A refused address stops the start with one `fatal` line, for example `acquiring the database
  writer lock: DATABASE_URL has no password: add the role's password to the URL`. The `probe`
  command prints the same text on stderr and exits 1.

The hub also passes `PATH`, `TZ`, `TMPDIR` and `MONITOR_HUB_DATA_DIR`. The monitor ignores `TZ`
and keeps all its state in Postgres. `anhanguera-monitor validate-settings` checks the
environment and prints `{"problems": [{"path", "message"}]}`; it exits 0 when there are none.

## Process contract

- The hub starts `anhanguera-monitor plugin` with its cookie variable. Started any other way, the
  binary prints the SDK's `NOT_LAUNCHED_BY_HUB` text on stderr and exits 1. `probe` and
  `validate-settings` are the two commands that run without the hub.
- Stdout carries only the handshake line. Logs go to stderr as JSON lines.
- Start-up takes the database writer lock, applies the migrations with `sea-orm-migration` on the
  lock's connection, opens the pool, reads the newest issued `seq` and the newest stored event, and
  creates the state row with the device id, all before the handshake. Each connection attempt has
  10 s, and sqlx retries a refused connection within them. The whole sequence has 45 s, because the
  hub kills a plugin that has not answered in 60 s. Start-up does not call the portal, so nothing
  of it logs in. Against a database that refuses connections the start ends after about 10 s with
  `acquiring the database writer lock: connecting to the database for the writer lock timed out`
  (0.1.0 failed at once). The longest stall after the lock's connection drops ends the process
  after about 30 s. Both stay under the 45 s start-up limit and the 60 s the hub allows.
- Exit code 0 means a clean stop (SIGTERM or SIGINT), with no `fatal` line. When the monitor exits
  1 because of a failed start-up step or a lost database lock, it writes exactly one JSON `fatal`
  line at `error` level, whose `error` field names the step (for example `acquiring the database
  writer lock: ...`) or starts with `the database lock was lost: the lock's connection`. A
  configuration error prints its message on stderr and exits 1. The hub restarts the plugin with
  a backoff.
- A query on the lock's connection every 0.5 s, with 30 s to answer, notices a lost lock (see
  Persistence). It reads `pg_locks` and checks that this connection still holds the lock. The
  reason after `the database lock was lost: ` is `the lock's connection failed: ...`, `the lock's
  connection did not answer within 30s` or `the lock's connection was replaced`. A lock lost
  before the migrations stops the start the same way, under the step `applying the database
  migrations`, and nothing is migrated.
- A second process on the same database cannot take the writer lock and migrates nothing.
- `Describe` names the plugin `anhanguera` with these event kinds: `welcome`, `invoice_open`,
  `payment_due`, `payment_overdue`, `invoice_closed`, `payment_attention`,
  `billing_day_approaching`, `calendar_financial`, `coursework_digest`, `site_changed`,
  `monitor_blind`, `login_failed` and `course_not_found`. Urgent or info is set per event.
  `Status` answers `FAILING` with the hub-ahead text while the last `Events` call found the hub
  ahead of the database. Otherwise it answers `CHECKING` during a cycle, `FAILING` with a one-line
  message (cut at 300 characters) when the last cycle did not finish or the monitor has something
  to report (a site change, no enrollments, a course not found, a rejected password, an
  unavailable boleto link), and `IDLE` otherwise. When both apply, the message gives the monitor's
  note first and then the cycle's reason, joined by `; `. `Check` starts a cycle now, or refuses
  with `a check is already running`.
- The monitor serves `Watch` through the SDK's `serve_with_notifier`. It signals at start with the
  newest stored `seq`, after each committed batch of events, when a cycle starts and ends, when the
  hub-ahead state changes, and when the first read ends. The signal of a batch comes from
  `Store::record`, right after the commit and with the highest new `seq` of the batch, so a failed
  commit, or a batch without a new event, signals nothing. The welcome event has no signal of its
  own: the signal at start and the one at the end of the first read cover it. A signal never makes
  the monitor call the portal. Signals carry the `seq` of a stored event, never the sequence's own
  value.
- `Events(after, limit)` returns the events with a `seq` above `after`, in order. A `limit` of 0
  means 100, and the maximum is 500. Each call acknowledges every event up to `after`, and writes
  the state row only when that position moves forward.
- The first read stops at the newest `seq` that existed at start-up. Until one `Events` call
  returns a page shorter than its limit, `Events` leaves out every newer event, so what the first
  cycle records is not absorbed into the hub's baseline. After that call nothing is left out and
  the monitor signals the hub. On a new database the `welcome` event is newer than that `seq`, so
  the hub announces it. The limit belongs to one process: if the monitor restarts after its first
  cycle recorded events and before the hub's first read finished, the new process counts those
  events as history.
- When the database is down, `Events` answers `UNAVAILABLE` and `Status` answers `FAILING`, both
  with `the database is unavailable`, without the address or the password. Against a database that
  refuses connections, sqlx keeps retrying for the 10 s allowed to get a connection, so `Events`
  and `Status` take up to 10 s to answer, under the hub's 30 s limit per call. A cycle then takes
  about 40 s, because the state reads and writes it makes one after another (sent keys, the
  payments streak, the open problems and the pruning) each wait those 10 s. Its failure (`the
  cycle: the database is unavailable; payments: the database is unavailable`) shows in `Status`
  about 40 s after the cycle starts, and `Status` answers `CHECKING` until then.
- When `the hub is ahead of this database: reset the hub's position for this plugin` appears at
  `error` level, the hub asked for events after a `seq` this database never issued: the database
  was reset or replaced and the hub's stored position was not. `Events` returns nothing until the
  two agree. Reset both together. The line is logged once per process.

## Flow of a cycle

The scheduler runs a cycle at start, then one interval after each cycle ends. `Check` starts one
early.

1. The cycle logs in if needed, gets the student id and lists every course enrollment, whatever its
   status. The token lives in memory only. The device id (a UUID v4 made on first start and kept
   in the database) never changes, so the account does not collect a new device per login. After a
   rejected login there is no new attempt for 6 hours, unless the process restarts.
2. The areas run concurrently and independently, so a problem in one never blocks another:
   - **Payments**, for every enrollment, not only the configured courses. Invoices, then the
     boleto link for each invoice that needs one.
   - **Coursework**, for the enrollments whose course name matches `COURSES`: subject deadlines,
     the calendar, and one unit-quiz call per in-progress subject (three at a time).
   - **Courses**: a configured name with no match records one `course_not_found` event, and the
     status lists the available names. An empty course list skips this check, because the hub
     keeps that event's id forever and the list may come back.
3. Each area validates its answers strictly before it records anything. A problem stops that area
   for that enrollment. It becomes one `site_changed` event, keyed by a signature of operation,
   field and kind, and it repeats only after a full successful cycle of the area cleared it. If
   recording that event fails, the monitor closes the signature again so the next cycle retries.
4. Each area records its events, its sent-notification keys and its bookkeeping in one
   transaction, then the monitor signals `Watch`.

Invoices to pay have the status `OPEN`, `OVERDUE` or `FAILED`; unknown statuses count as to pay.
Settled ones are `PAID`, `CANCELLED`, `REFUNDED`, `WROTE_OFF` and `OVERPAID`.

- `invoice_open`: the first cycle that sees an unannounced invoice to pay. It is never gated by the
  reminder hour, and it covers the thresholds it makes redundant, so there is no second message
  that morning.
- `payment_due`, `payment_overdue`, `billing_day_approaching`, `calendar_financial`, the
  coursework digest and `invoice_closed` wait for `REMINDER_HOUR` in `AVA_TIMEZONE`. Every other
  kind goes out as soon as a cycle sees it. When several thresholds of one item are due together,
  only the closest is sent and the others are marked done. If an invoice's due date changes, its
  reminders restart from the new date.
- `invoice_closed`: an invoice that was to pay is settled or gone. A boleto payment often clears
  overnight, so the first cycle in reminder hours sends it and sets `closedAt`. Its id is
  `invoice-closed:<invoiceId>:<dueDate>:<closedAt>`.
- `payment_attention`: `informationType` changed to something other than `PAYMENTS_ON_TIME` or
  `NONE`.
- `billing_day_approaching`: the net for an invoice that did not appear. Once per month per
  enrollment, `BILLING_DAY_NET` days before the next due day, when no invoice with a due date in
  that month was seen. With `auto`, the lead time is learned from invoices whose appearance the
  monitor witnessed: 3 days until two exist, then the smallest of the last six minus 2, kept
  between 1 and 15.
- Coursework: pending assessments and exams that are `AVAILABLE`, unit quizzes without attempts, the
  study period while lessons are unwatched, and exam periods one day before they open, all with
  `DEADLINE_REMINDER_DAYS`. Everything that becomes due in a cycle goes into one
  `coursework_digest` event (urgent when any item has one day or less left). Financial calendar
  events are separate `calendar_financial` events. Other calendar categories join the digest.
- When the boleto link fails, the payment event goes out without it and the status says so. That is
  not a site change.
- A plain failure (network, timeout, HTTP 5xx) is not a site change: the status says `FAILING` and
  the next cycle retries. When payments have not fully succeeded for `FAILING_ALERT_AFTER`, one
  `monitor_blind` event goes out per streak. A rejected login records one `login_failed` event per
  streak. A course list with no enrollments counts as a payments failure.
- Event ids are built from stable portal ids and dates, because the hub drops a second event with
  the same id. Those ids reach the hub and its database, never the repository.
- After each cycle the store prunes: events already acknowledged and older than the newest 200, and
  sent keys older than 400 days.

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

## Dependencies

- `sea-orm` and `sea-orm-migration` are pinned with `=` at 2.0.4, with default features off.
  sqlx gets no TLS feature: `cargo tree -e features -i sqlx-core | grep -c _tls` stays 0.
- After any change to `Cargo.toml`, set these back with `cargo update -p <crate> --precise
  <version>`, one crate per command: `sqlx` 0.9.0, `nix` 0.31.3, `uuid` 1.29.0, `winnow` 1.0.4,
  `toml_edit` 0.25.17+spec-1.1.0, `toml_datetime` 1.1.2+spec-1.1.0, `regex` 1.13.1,
  `regex-automata` 0.4.18 and `regex-syntax` 0.8.11. Check on crates.io that each new lock entry
  is at least two weeks old, and move a newer one back with `--precise`.
- `libsqlite3-sys` is in `Cargo.lock` and never compiled, and so are `sqlx-sqlite`, `sqlx-mysql`
  and the `arrow-*` crates. The sqlx features that sea-orm's defaults turn on carry weak
  `sqlx-sqlite?/...` edges, and cargo keeps the packages those edges name in the lock
  (cargo#10801). `cargo tree --locked -e normal --target all -i libsqlite3-sys` prints nothing. An
  audit that reads the lock still reports RUSTSEC-2026-0459 against `libsqlite3-sys` 0.37.0; look
  again when sqlx 0.9.1 is two weeks old, after 2026-12-02.
- `tokio-postgres` is a dev-dependency only: the tests' admin clients and the throwaway Postgres
  use it.

## The probe command

`anhanguera-monitor probe` exists to compare what the portal really answers with
`docs/site-api.md`, and it is the first live check of the queries in `queries/`. Run it inside the
hub's container, with the plugin already started once:

```bash
docker exec monitor-hub /opt/monitor-hub/plugins/anhanguera/anhanguera-monitor probe
```

- It does not need the hub cookie. It reads the same settings as the monitor. When any variable
  whose name starts with `ANHANGUERA__` is set, as inside the hub's container, it reads only the
  prefixed names, so the hub's own `DATABASE_URL` never reaches it. With no such variable (a local
  run) it reads only the plain names. It never reads credentials from arguments.
- It reads the device id from the database (read-only, no migration, no writer lock). With no
  state row it stops with `start the plugin once first`.
- It runs every operation once per enrollment (invoices, a boleto link for each invoice to pay) and
  per configured course (subject deadlines, calendar, unit quizzes), validates the answers with the
  same decoders as the monitor, and prints one JSON line on stdout:
  `{"operations": {"<Operation>": {"shape": ..., "verdict": ...}}, "billing_day_net": [{"days", "learned"}]}`.
  The shape holds field names, types, `number`/`boolean`/`string`, dates with every digit replaced
  by `9`, and an upper-case enum value only under a known enum key (`status`, `informationType`,
  `type`, `category`, `subCategory`, `tokenType`). It never prints a value, id, name, URL or
  token. The verdict is `ok` or the problem's signature (`login_rejected`, `unavailable`).
- Exit code 0 when every verdict is `ok`, 2 when some are not, and 1 when it could not run (a
  configuration error, no state row, an unreachable database, an unreachable portal or a rejected
  login), with the reason on stderr.
- It records nothing.
