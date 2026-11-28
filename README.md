# anhanguera-monitor

A monitor for monitor-hub that watches a student's account on the Anhanguera student portal (the
AVA). It records events, and the hub turns them into notifications.

A tuition invoice is easy to miss: the portal does not warn that an invoice exists or that it
falls due soon. The monitor fills that gap. It only reads, and it never pays or changes anything on
the portal.

## What it does

- **Payments, first.** It tells you when an invoice to pay appears, with the link to the boleto,
  reminds you on a schedule before the due date, and keeps reminding while the invoice is overdue.
  It also tells you when an invoice is no longer open, and it warns when the due day is near and
  the portal shows no invoice.
- **Coursework**, for the courses you list. Assignments, exams, unit quizzes and the study period
  get reminders before they close, grouped into one message per cycle. It also announces an exam
  period one day before it opens.
- **Calendar.** Dated events from the academic calendar (financial, secretariat, one-off) get
  reminders too.
- **It says so when it cannot see.** If the portal changes in a way the monitor cannot read, if the login
  stops working, or if payments cannot be checked for a day, it sends an urgent event saying so.

Scheduled reminders and the payment confirmation wait for a configured hour (9 by default, in the
portal's time zone), so nothing arrives at 3 a.m. These are `payment_due`, `payment_overdue`,
`billing_day_approaching`, `calendar_financial`, the coursework digest and `invoice_closed` (a
boleto payment often clears overnight). Everything else goes out as soon as a cycle sees it:
`invoice_open`, `payment_attention`, `site_changed`, `monitor_blind`, `login_failed`,
`course_not_found` and `welcome`.

## Configuration

Every setting is declared in `settings.json`, with its default. The monitor reads its
configuration as one JSON document. A hub that hands the document over writes it, at every start,
on the descriptor named by `MONITOR_HUB_CONFIG_FD`. Without that descriptor, the monitor builds
the same document from the variables below. Under monitor-hub, set them on the hub with the
`ANHANGUERA__` prefix, for example `ANHANGUERA__COURSES`. The hub removes the prefix before it
starts the monitor. `.env.example` lists every variable.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` / `DATABASE_URL_FILE` | required | Postgres address with the role's password, connected without TLS. |
| `AVA_USERNAME` | required | The student's CPF. Dots, dashes and spaces are ignored. |
| `AVA_PASSWORD` / `AVA_PASSWORD_FILE` | one of them | The portal password. |
| `COURSES` | required | Names of the courses to watch for coursework. Case and accents are ignored. |
| `CHECK_INTERVAL` | `1h` | How often to read the portal. At least `15m`. |
| `PAYMENT_REMINDER_DAYS` | `7,3,1,0` | Days before the due date to remind, each from 0 to 60. |
| `PAYMENT_OVERDUE_REPEAT` | `24h` | How often to repeat the reminder of an overdue invoice. `0` sends one. |
| `BOLETO_LINK` | `true` | Include the boleto link in payment events. |
| `BILLING_DAY_NET` | `auto` | Days before the due day to warn when no invoice shows: `auto`, 1 to 28, or `off`. |
| `DEADLINE_REMINDER_DAYS` | `7,3,1` | Days before a coursework deadline to remind, each from 0 to 60. |
| `CALENDAR_CATEGORIES` | `FINANCIAL,SECRETARIAT,UNIQUE_EVENTS` | Which calendar events to report. Empty takes the default; `,` reports none. |
| `REMINDER_HOUR` | `9` | Local hour, 0 to 23, from which reminders go out. |
| `FAILING_ALERT_AFTER` | `24h` | How long payments must keep failing before the urgent alert. At least `1h`. |
| `AVA_TIMEZONE` | `America/Sao_Paulo` | An IANA time zone name. |
| `LOG_LEVEL` | `info` | `trace`, `debug`, `info`, `warn` or `error`. |

The password and the CPF are secrets: they never reach a log, an event or the database. After a
rejected login the monitor waits six hours before trying again, so a wrong password cannot lock the
account.

## Running under monitor-hub

The image holds `/monitor/monitor.json`, `/monitor/settings.json` and the binary, and the hub
copies `/monitor/` to `/opt/monitor-hub/monitors/anhanguera/`. The monitor runs only when the hub
starts it with `anhanguera-monitor monitor`. Started by hand, it prints the SDK's not-launched
message and exits 1. When `MONITOR_HUB_CONFIG_FD` is set, the monitor reads the configuration
document from that descriptor once, at start, and closes it. The hub's own deploy guide
(`docs/deploy.md` in the monitor-hub repository) covers how the hub's image gets its monitors.

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t anhanguera-monitor .
```

It keeps its state in Postgres, so give it its own database. It takes a lock on that database, so a
second copy on the same database refuses to start. If the monitor loses the lock, because its
connection to the database failed, was replaced or stopped answering for 30 seconds, the process
stops, and the next start is a new login on the portal.

The address is `postgres://user:password@host:port/database?sslmode=disable` (`postgresql://` works
too) and must carry the role's password, in the address or in a `password` query key. The monitor
connects without TLS, so `sslmode=require` and the stricter modes fail at once. It refuses, with a
message that never repeats the address, a URL without a password, the key=value form, several
hosts, a URL without a host, and a query key it does not know. `CLAUDE.md` lists the exact forms.

### The database role

The first start creates the tables, so the monitor's role needs `CREATE` on its schema for that
start. Later starts send no DDL and work with `SELECT`, `INSERT`, `UPDATE` and `DELETE` on the
tables, `USAGE` and `SELECT` on `events_seq_seq`, and `SELECT` on `seaql_migrations`. The hub's log
shows `database migrations applied` with the migration's name on the first start, and with `[]` on
every later one.

## Releases

When a push to `main` passes the tests, the workflow works out the next version from the Conventional
Commits since the last `v*` tag:

- `feat` bumps the minor version.
- `fix`, `perf`, `chore`, `refactor` and `build` bump the patch version.
- A breaking `feat!`, `fix!` or `perf!` (or a `BREAKING CHANGE:` footer on those types) bumps the major
  version. A breaking `chore!`, `refactor!` or `build!` is still a patch.
- `docs`, `test`, `ci` and `style` make no release.

When a release is due, the same run tags `vX.Y.Z`, creates a GitHub Release with the changelog and
publishes the image as `X.Y.Z`, `X.Y`, `latest` and `sha-…`. `Cargo.toml`'s version is not updated,
because the crate is not published.

## Checking what the portal answers

`probe` runs every query once and prints only the shape of the answers, never a value, id, name or
token. Start the monitor once first, then run it inside the hub's container:

```bash
docker exec monitor-hub /opt/monitor-hub/monitors/anhanguera/anhanguera-monitor probe
```

Inside the container it reads the `ANHANGUERA__` variables set on the hub and ignores the hub's own
`DATABASE_URL`. Run anywhere else, with no `ANHANGUERA__` variable set, it reads the plain names.
It reads only variables, never the configuration document a hub hands to the monitor.

It prints one JSON line with the shape and a verdict for each operation. It exits 0 when every
verdict is `ok`, 2 when some are not, and 1 when it could not run. Compare the shapes with
`docs/site-api.md`.

## Development

```bash
make check      # fmt, clippy, tests
make up         # a development Postgres on 127.0.0.1:5432
```

The tests start their own Postgres unless `TEST_DATABASE_URL` is set, and talk to a fake portal
only. Use a short `TMPDIR` (the SDK limits Unix socket paths to 100 bytes). When `CI` is set, a
test that cannot run fails instead of skipping. The conformance test uses the SDK's
`monitor-conformance` command, built with Go. See `CLAUDE.md` for the details.
