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

Reminders wait for a configured hour (9 by default, in the portal's time zone), so nothing arrives
at 3 a.m. Two things go out at once: a new invoice, and the alerts about the monitor itself.

## Configuration

Every setting is declared in `settings.json`. Under monitor-hub, set them on the hub with the
`ANHANGUERA__` prefix, for example `ANHANGUERA__COURSES`. The hub removes the prefix before it
starts the monitor. `.env.example` lists every variable.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` / `DATABASE_URL_FILE` | required | Postgres address, connected without TLS. |
| `AVA_USERNAME` | required | The student's CPF. Dots, dashes and spaces are ignored. |
| `AVA_PASSWORD` / `AVA_PASSWORD_FILE` | one of them | The portal password. |
| `COURSES` | required | Names of the courses to watch for coursework. Case and accents are ignored. |
| `CHECK_INTERVAL` | `1h` | How often to read the portal. At least `15m`. |
| `PAYMENT_REMINDER_DAYS` | `7,3,1,0` | Days before the due date to remind, each from 0 to 60. |
| `PAYMENT_OVERDUE_REPEAT` | `24h` | How often to repeat the reminder of an overdue invoice. `0` sends one. |
| `BOLETO_LINK` | `true` | Include the boleto link in payment events. |
| `BILLING_DAY_NET` | `auto` | Days before the due day to warn when no invoice shows: `auto`, 1 to 28, or `off`. |
| `DEADLINE_REMINDER_DAYS` | `7,3,1` | Days before a coursework deadline to remind, each from 0 to 60. |
| `CALENDAR_CATEGORIES` | `FINANCIAL,SECRETARIAT,UNIQUE_EVENTS` | Which calendar events to report. |
| `REMINDER_HOUR` | `9` | Local hour, 0 to 23, from which reminders go out. |
| `FAILING_ALERT_AFTER` | `24h` | How long payments must keep failing before the urgent alert. At least `1h`. |
| `AVA_TIMEZONE` | `America/Sao_Paulo` | An IANA time zone name. |
| `LOG_LEVEL` | `info` | `trace`, `debug`, `info`, `warn` or `error`. |

The password and the CPF are secrets: they never reach a log, an event or the database. After a
rejected login the monitor waits six hours before trying again, so a wrong password cannot lock the
account.

## Running under monitor-hub

The image holds `/plugin/plugin.json`, `/plugin/settings.json` and the binary, and the hub's image
build copies that directory in. The monitor runs only when the hub starts it with `plugin`. Started
by hand, it prints the SDK's not-launched message and exits 1.

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t anhanguera-monitor .
```

It keeps its state in Postgres, so give it its own database. It takes a lock on that database, so a
second copy on the same database refuses to start.

## Checking what the portal answers

`probe` runs every query once and prints only the shape of the answers, never a value, id, name or
token. Start the monitor once first, then run it inside the hub's container:

```bash
docker exec monitor-hub /opt/monitor-hub/plugins/anhanguera/anhanguera-monitor probe
```

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
`plugin-conformance` command, built with Go. See `CLAUDE.md` for the details.
