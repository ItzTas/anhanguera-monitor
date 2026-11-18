---
name: project-structure
description: >-
  File structure, module layering and persistence conventions for anhanguera-monitor (a Rust
  monitor-hub plugin: tonic over the SDK, tokio-postgres + deadpool, goose-format migrations,
  Postgres, a GraphQL client for the Anhanguera student portal). Read this before creating or
  moving a module, integration test, test helper, migration, GraphQL document or any
  config/scaffolding file (Cargo.toml, rust-toolchain.toml, plugin.json, settings.json,
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
├── rust-toolchain.toml        # the SDK's channel at sdk-rust/v0.3.0, with rustfmt and clippy
├── plugin.json                # the manifest the hub reads: name anhanguera, command anhanguera-monitor plugin
├── settings.json              # the settings declaration (version 2); config.rs embeds it
├── src/
│   ├── main.rs                # plugin, probe and validate-settings arguments, logging, runtime, exit code
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
│   ├── store.rs               # all SQL, Store (deadpool), MIGRATIONS, WriterLock
│   ├── monitor.rs             # Monitor: areas, run_cycle, problems, blind and login streaks
│   ├── scheduler.rs           # periodic loop, trigger, CycleState
│   ├── server.rs              # AnhangueraPlugin: the plugin contract over the scheduler and the store
│   ├── probe.rs               # the probe command: response shapes and verdicts
│   └── logtest.rs             # tests only: captures log events on the calling thread
├── queries/                   # the GraphQL documents, embedded with include_str!
├── sql/
│   └── schema/                # goose-format migrations, embedded by MIGRATIONS
├── testdata/ava/              # fictitious portal answers for the fake portal
├── tests/
│   ├── support/               # shared helpers, one file per subject (see Testing)
│   ├── ava.rs  store.rs  monitor.rs  scheduler.rs  server.rs  settings.rs  probe.rs
│   └── end_to_end.rs  entrypoint.rs  conformance.rs
├── docs/site-api.md           # facts about the portal's GraphQL API and how each was established
├── .env.example               # every variable, for compose, the tests and a local plugin run
├── .githooks/                 # commit-msg (Conventional Commits) + pre-commit (cargo fmt, cargo test)
├── .github/workflows/         # docker.yml: fmt, clippy and tests against Postgres, then the GHCR image
├── .prototools                # proto: pins Go for the conformance test
├── Dockerfile                 # cross-compiled static musl binary, final stage FROM scratch
├── compose.yaml               # development Postgres only, read-only, no capabilities
└── Makefile                   # check, test, up, down
```

## Modules

- `main.rs` stays thin. It refuses `plugin` outside the hub, starts logging and the runtime, calls
  `app::run`, and turns its error into the single `fatal` log line and exit code 1. `probe` and
  `validate-settings` run without the hub.
- `app.rs` is the only module that knows every other one. Anything that has to happen before the
  handshake goes in `start_up`, inside the 45 s start-up limit, with a step name for its error.
- `monitor.rs` holds the cycle logic and depends on the `Portal` trait, not on `ava::AvaClient`,
  so tests can use a fake portal. `scheduler.rs` decides when cycles run, and `server.rs` maps
  the scheduler and the store onto the plugin contract.
- `payments.rs` and `coursework.rs` are pure planners: they take what the portal said and what the
  store knows, and return the events and keys to record. `monitor.rs` fetches and records.
- A subject module owns its types and its error enum (`thiserror`). Move a helper only when a
  second module needs it.
- `pub` is for what another module or `tests/` uses. The rest is `pub(crate)` or private.
- No comments in the code. `#[cfg(test)] mod tests` at the bottom of a module is where unit tests
  go.

## Persistence

- `sql/schema/0000N_name.sql` are migrations in goose format (`-- +goose Up` and
  `-- +goose Down`). Adding one means adding the file and its entry in `MIGRATIONS` in
  `src/store.rs`. The SDK's `migrate::up` applies them at start-up on the writer lock's connection.
- **A schema change is always a new migration. Never edit one that exists.**
- **Migrations carry schema only.** The `monitor_state` row (with the device id) is written by
  `Store::initialize` at start-up.
- The SQL lives as constants in `src/store.rs`, and only `Store` runs it. A transaction that
  inserts an event locks the state row first, so `seq` order is commit order.
- Times are `TIMESTAMPTZ` and map to `DateTime<Utc>`. Calendar dates are `DATE`. Integers are
  `BIGINT`.
- The GraphQL documents in `queries/` are copied from the portal's own client and trimmed. A change
  to one is a change to what the monitor sends, so check it with `probe` before trusting it.

## Testing

- Write tests that describe what the code does now.
- Unit tests live in their module. Integration tests in `tests/` go through the public API or
  the built binary (`env!("CARGO_BIN_EXE_anhanguera-monitor")`).
- Every integration test file declares `pub mod support;` (not `mod support;`), and the support
  modules carry no `#[allow]`. One file per subject: `fake_ava.rs` (fake GraphQL portal serving
  `testdata/ava`), `fake_portal.rs` (a `Portal` for cycle tests) and `process.rs` (starts the
  binary, reads the handshake, sends signals).
- Database tests get a database from `support::migrated_database()`, or from the test kit's
  `database()` when the test migrates by itself.
- A test that needs something outside the crate (Postgres, `sh`, the conformance command) prints a
  `skipping:` notice and returns when it is missing, and panics when `CI` is set and not empty.
- No test talks to the real portal. Fixtures are fictitious.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and
  `CI=1 cargo test` must pass.

## Tooling

- The toolchain comes from `rust-toolchain.toml`. Keep its channel equal to the SDK's.
- Every dependency is checked on crates.io before it goes into `Cargo.toml`, and every locked
  crate is at least two weeks old.
- The image builds on the build machine's platform with `cargo zigbuild` for
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. The final stage has only `COPY`
  instructions, so no emulation is needed. It carries `plugin.json`, `settings.json` and the
  binary.
- Never change the versions pinned in `.prototools`.
