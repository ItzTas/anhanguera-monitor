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

- **Do not write comments in the code.** No `//`, `///` or `//!`. Names explain what the code
  does. Attributes such as `#[cfg(test)]` are not comments.
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

- Postgres: the SDK test kit's `database()` gives each test its own database. It uses
  `TEST_DATABASE_URL` when set (the compose Postgres works, see `.env.example`), and otherwise
  starts a throwaway cluster with `initdb` and `postgres` from `PATH`.
  `tests/support::migrated_database` adds the migrations.
- `sh` with `kill` for the tests that start the binary and signal it.
- The conformance command for `tests/conformance.rs`: the binary named by `PLUGIN_CONFORMANCE`,
  or else `go install` of `plugin-conformance` at the SDK's Go tag `sdk-go/v0.4.0`, cached under
  `~/.cache/monitor-hub/plugin-conformance/`.
- The portal is a fake GraphQL server (`tests/support/fake_ava.rs`) that serves the fictitious
  answers in `testdata/ava/`. Cycle tests use a fake `Portal` (`tests/support/fake_portal.rs`).

The SDK refuses Unix socket paths longer than 100 bytes, so a long `TMPDIR` breaks the tests that
start the plugin. Run them with a short one (`TMPDIR=/tmp`). The same holds under the hub, where
`TMPDIR` is `/tmp`.

CI (`.github/workflows/docker.yml`) runs `cargo fmt --check`, clippy and `cargo test --locked`
against a `postgres:18-alpine` service, with Go 1.27.2 for the conformance command. Then it builds
the image for amd64 and arm64 and publishes it to `ghcr.io` on every run that is not a pull
request: pushes to `main`, `v*` tags and manual runs. A `v*` tag gets the version tags (`2.1.0` and
`2.1`), only the default branch moves `latest`, and every image gets a `sha-` tag.

## Configuration

`settings.json` (version 2) declares every setting, with its default, and `src/config.rs` embeds
it. Under monitor-hub, set them on the hub with the `ANHANGUERA__` prefix
(`ANHANGUERA__COURSES` and so on). The hub removes the prefix before it starts the monitor. The
monitor reads only its environment, never a `.env` file. `.env.example` lists every variable
except `AVA_GRAPHQL_URL`, which only the tests set. The password is set by `AVA_PASSWORD` or
`AVA_PASSWORD_FILE`, the database address by `DATABASE_URL` or `DATABASE_URL_FILE`, never both.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` / `DATABASE_URL_FILE` | required, secret | Postgres address, connected without TLS. |
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

The hub also passes `PATH`, `TZ`, `TMPDIR` and `MONITOR_HUB_DATA_DIR`. The monitor ignores `TZ`
and keeps all its state in Postgres. `anhanguera-monitor validate-settings` checks the
environment and prints `{"problems": [{"path", "message"}]}`; it exits 0 when there are none.

## Process contract

- The hub starts `anhanguera-monitor plugin` with its cookie variable. Started any other way, the
  binary prints the SDK's `NOT_LAUNCHED_BY_HUB` text on stderr and exits 1. `probe` and
  `validate-settings` are the two commands that run without the hub.
- Stdout carries only the handshake line. Logs go to stderr as JSON lines.
- Start-up takes the database writer lock, applies the migrations, opens the pool, reads the newest
  issued `seq` and the newest stored event, and creates the state row with the device id, all
  before the handshake. Each connection attempt has 10 s, and the whole sequence has 45 s, because
  the hub kills a plugin that has not answered in 60 s. Start-up does not call the portal.
- Exit code 0 means a clean stop (SIGTERM or SIGINT), with no `fatal` line. When the monitor exits
  1 because of a failed start-up step or a lost database lock, it writes exactly one JSON `fatal`
  line at `error` level, whose `error` field names the step (for example `acquiring the database
  writer lock: ...`) or starts with `the database lock was lost`. A configuration error prints its
  message on stderr and exits 1. The hub restarts the plugin with a backoff.
- A second process on the same database cannot take the writer lock and migrates nothing.
- `Describe` names the plugin `anhanguera` with these event kinds: `welcome`, `invoice_open`,
  `payment_due`, `payment_overdue`, `invoice_closed`, `payment_attention`,
  `billing_day_approaching`, `calendar_financial`, `coursework_digest`, `site_changed`,
  `monitor_blind`, `login_failed` and `course_not_found`. Urgent or info is set per event.
  `Status` answers `FAILING` with the hub-ahead text while the last `Events` call found the hub
  ahead of the database. Otherwise it answers `CHECKING` during a cycle, `FAILING` with a one-line
  message (cut at 300 characters) when the last cycle did not finish or the monitor has something
  to report (a site change, no enrollments, a course not found, a rejected password, an
  unavailable boleto link), and `IDLE` otherwise. `Check` starts a cycle now, or refuses with
  `a check is already running`.
- The monitor serves `Watch` through the SDK's `serve_with_notifier`. It signals at start with the
  newest stored `seq`, after each committed batch of events, when a cycle starts and ends, when the
  hub-ahead state changes, and when the first read ends. A signal never makes the monitor call the
  portal. Signals carry the `seq` of a stored event, never the sequence's own value.
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
  with `the database is unavailable`, without the address or the password.
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
     status lists the available names.
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
- `payment_due`, `payment_overdue`, `billing_day_approaching`, `calendar_financial` and the
  coursework digest wait for `REMINDER_HOUR` in `AVA_TIMEZONE`. Every other kind goes out as soon
  as a cycle sees it. When several thresholds of one item are due together, only
  the closest is sent and the others are marked done. If an invoice's due date changes, its
  reminders restart from the new date.
- `invoice_closed`: an invoice that was to pay is settled or gone. Its id is
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

- A schema change is a new file `sql/schema/0000N_name.sql` in goose format (`-- +goose Up` and
  `-- +goose Down`) and a new entry in `MIGRATIONS` in `src/store.rs`. **Never edit a migration
  that already exists.** The SDK's `migrate::up` applies them on the writer lock's connection.
- Tables: `monitor_state` (one row: device id, acknowledged `seq`, last payments success, the
  payments failure streak start, whether `monitor_blind` went out), `events` (`seq` from a
  `BIGSERIAL`, a unique `id`), `sent_keys`, `invoices` (one row per enrollment and invoice id, so
  an invoice listed under two enrollments keeps two rows: due date, status, first seen, learned lead
  days, overdue repeat bookkeeping), `enrollments` (last `informationType`, last payments check) and
  `problems` (open `site_changed` signatures). The password and the token are never stored.
- All SQL lives in `src/store.rs`. Every transaction that inserts an event locks the state row
  first, so the order of `seq` values is the order of the commits.
- Pool connections run with a 20 s `statement_timeout`, a 5 s `lock_timeout`, TCP keepalives and a
  60 s `tcp_user_timeout`. The writer lock's connection has no statement timeout, because it runs
  the migrations.

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
