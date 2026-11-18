use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, SecondsFormat, TimeDelta, Utc};
use chrono_tz::Tz;
use futures_util::future::join_all;
use futures_util::stream::{self, StreamExt};
use monitor_hub_plugin::Notifier;
use tracing::{error, warn};

use crate::ava::AvaError;
use crate::ava::model::{CalendarEvent, CourseEnrollment, Problem, Quiz, Subject};
use crate::config::Config;
use crate::coursework::{CourseworkInput, coursework_items, digest, financial_events};
use crate::dates::{format_day, local_today};
use crate::events::{HOME_URL, Kind, NewEvent};
use crate::payments::{PaymentInput, PaymentOptions, PlannedPayment, plan_payments};
use crate::portal::Portal;
use crate::reminders::ReminderRule;
use crate::store::{EnrollmentState, KnownInvoice, RecordBatch, Store, StoreError};
use crate::text::{course_key, one_line};
use crate::timestamp::Clock;

const STATUS_MAX: usize = 300;
const TITLE_MAX: usize = 80;
const LINE_MAX: usize = 300;
const QUIZ_CALLS: usize = 3;
const KEEP_EVENTS: i64 = 200;
const KEEP_KEYS_DAYS: i64 = 400;
pub const DATABASE_UNAVAILABLE: &str = "the database is unavailable";
const BOLETO_UNAVAILABLE: &str = "boleto link unavailable";
const NO_ENROLLMENTS: &str = "the portal listed no course enrollments";

pub struct MonitorOptions {
    pub courses: Vec<String>,
    pub payments: PaymentOptions,
    pub deadlines: ReminderRule,
    pub calendar_categories: Vec<String>,
    pub boleto_link: bool,
    pub failing_alert_after: Duration,
    pub interval: Duration,
    pub tz: Tz,
    pub clock: Clock,
}

impl MonitorOptions {
    pub fn from_config(config: &Config, clock: Clock) -> MonitorOptions {
        let rule = |thresholds: &[u32]| ReminderRule {
            thresholds: thresholds.to_vec(),
            hour: config.reminder_hour,
            tz: config.timezone,
        };
        MonitorOptions {
            courses: config.courses.clone(),
            payments: PaymentOptions {
                reminders: rule(&config.payment_reminder_days),
                overdue_repeat: config.payment_overdue_repeat,
                billing_day_net: config.billing_day_net,
                check_interval: config.check_interval,
            },
            deadlines: rule(&config.deadline_reminder_days),
            calendar_categories: config.calendar_categories.clone(),
            boleto_link: config.boleto_link,
            failing_alert_after: config.failing_alert_after,
            interval: config.check_interval,
            tz: config.timezone,
            clock,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CycleOutcome {
    Completed,
    Incomplete(String),
}

pub struct Monitor<P> {
    store: Store,
    portal: P,
    options: MonitorOptions,
    notifier: Notifier,
    memory: Mutex<Memory>,
}

#[derive(Default)]
struct Memory {
    login_streak: Option<LoginStreak>,
    no_enrollments: bool,
    missing_courses: Vec<String>,
    boleto_unavailable: bool,
    problems: Vec<String>,
}

#[derive(Clone, Copy)]
struct LoginStreak {
    since: DateTime<Utc>,
    alerted: bool,
}

struct Area {
    key: String,
    label: String,
}

impl Area {
    fn login() -> Area {
        Area {
            key: "login".to_owned(),
            label: "login".to_owned(),
        }
    }

    fn payments(enrollment: &CourseEnrollment) -> Area {
        Area::of("payments", enrollment)
    }

    fn coursework(enrollment: &CourseEnrollment) -> Area {
        Area::of("coursework", enrollment)
    }

    fn of(name: &str, enrollment: &CourseEnrollment) -> Area {
        Area {
            key: format!("{name}:{}", enrollment.id),
            label: one_line(&format!("{name} for {}", enrollment.course_name), TITLE_MAX),
        }
    }
}

struct PaymentState {
    known: Vec<KnownInvoice>,
    enrollment: EnrollmentState,
    lead_times: Vec<i64>,
}

struct Coursework {
    subjects: Vec<Subject>,
    quizzes: Vec<(String, Vec<Quiz>)>,
    calendar: Vec<CalendarEvent>,
}

impl<P: Portal> Monitor<P> {
    pub fn new(store: Store, portal: P, options: MonitorOptions) -> Monitor<P> {
        Monitor {
            store,
            portal,
            options,
            notifier: Notifier::new(),
            memory: Mutex::default(),
        }
    }

    pub async fn prepare(&self, device_id: String) -> Result<String, StoreError> {
        self.store.initialize(device_id, self.now()).await
    }

    pub async fn run_cycle(&self) -> CycleOutcome {
        let now = self.now();
        let (listed, sent) = tokio::join!(self.enrollments(now), self.store.sent_keys());
        let failures = match (listed, sent) {
            (Ok(enrollments), Ok(sent)) => self.areas(&enrollments, &sent, now).await,
            (listed, sent) => self.blind_cycle(listed.err(), sent.err(), now).await,
        };
        self.refresh_problems().await;
        self.prune().await;
        outcome(failures)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn notifier(&self) -> &Notifier {
        &self.notifier
    }

    pub fn interval(&self) -> Duration {
        self.options.interval
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.options.clock)()
    }

    pub fn status_message(&self) -> Option<String> {
        let mut parts = Vec::new();
        let memory = self.memory();
        if !memory.problems.is_empty() {
            parts.push(format!(
                "the portal changed: {}",
                memory.problems.join(", ")
            ));
        }
        if memory.no_enrollments {
            parts.push(NO_ENROLLMENTS.to_owned());
        }
        parts.extend(memory.missing_courses.iter().cloned());
        let boleto_unavailable = memory.boleto_unavailable;
        drop(memory);
        if let Some(until) = self.portal.login_backoff_until() {
            parts.push(format!(
                "the portal rejected the password; the next login is after {}",
                local_time(until, self.options.tz)
            ));
        }
        if boleto_unavailable {
            parts.push(BOLETO_UNAVAILABLE.to_owned());
        }
        if parts.is_empty() {
            return None;
        }
        Some(one_line(&parts.join("; "), STATUS_MAX))
    }

    async fn enrollments(&self, now: DateTime<Utc>) -> Result<Vec<CourseEnrollment>, String> {
        match self.list_enrollments().await {
            Ok(enrollments) => Ok(enrollments),
            Err(failure) => Err(self.portal_failed(&Area::login(), vec![failure], now).await),
        }
    }

    async fn list_enrollments(&self) -> Result<Vec<CourseEnrollment>, AvaError> {
        let student_id = self.portal.student_id().await?;
        self.memory().login_streak = None;
        self.portal.course_enrollments(&student_id).await
    }

    async fn blind_cycle(
        &self,
        listed: Option<String>,
        sent: Option<StoreError>,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let unread = sent.map(|failure| store_failed("the cycle", &failure));
        let tracked = self.track_payments(false, now).await;
        listed.into_iter().chain(unread).chain(tracked).collect()
    }

    async fn areas(
        &self,
        enrollments: &[CourseEnrollment],
        sent: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let login_area = Area::login();
        let (payments, coursework, courses, login) = tokio::join!(
            self.payments(enrollments, sent, now),
            self.coursework(enrollments, sent, now),
            self.missing_courses(enrollments, now),
            self.clear(&login_area),
        );
        payments
            .into_iter()
            .chain(coursework)
            .chain(courses)
            .chain(login.err())
            .collect()
    }

    async fn payments(
        &self,
        enrollments: &[CourseEnrollment],
        sent: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        self.memory().no_enrollments = enrollments.is_empty();
        if enrollments.is_empty() {
            let tracked = self.track_payments(false, now).await;
            let unlisted = format!("payments: {NO_ENROLLMENTS}");
            return std::iter::once(unlisted).chain(tracked).collect();
        }
        let areas: Vec<_> = enrollments
            .iter()
            .map(|enrollment| self.payment_area(enrollment, sent, now))
            .collect();
        let checked = join_all(areas).await;
        let mut lookups = Vec::new();
        let mut failures = Vec::new();
        for result in checked {
            match result {
                Ok(found) => lookups.extend(found),
                Err(reason) => failures.push(reason),
            }
        }
        {
            let mut memory = self.memory();
            memory.boleto_unavailable = boleto_unavailable(memory.boleto_unavailable, &lookups);
        }
        let tracked = self.track_payments(failures.is_empty(), now).await;
        failures.extend(tracked);
        failures
    }

    async fn payment_area(
        &self,
        enrollment: &CourseEnrollment,
        sent: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> Result<Vec<bool>, String> {
        let area = Area::payments(enrollment);
        let (home, state) = tokio::join!(
            self.portal.invoices(&enrollment.id),
            self.payment_state(&enrollment.id),
        );
        let home = match home {
            Ok(home) => home,
            Err(failure) => return Err(self.portal_failed(&area, vec![failure], now).await),
        };
        let state = state.map_err(|failure| store_failed(&area.label, &failure))?;
        let plan = plan_payments(
            &PaymentInput {
                course_enrollment_id: &enrollment.id,
                home: &home,
                known: &state.known,
                enrollment: &state.enrollment,
                lead_times: &state.lead_times,
                sent,
                now,
            },
            &self.options.payments,
        );
        let mut batch = plan.batch;
        let (events, lookups) = self.with_boleto_links(&enrollment.id, plan.events).await;
        batch.events = events;
        self.record(&batch)
            .await
            .map_err(|failure| store_failed(&area.label, &failure))?;
        self.clear(&area).await?;
        Ok(lookups)
    }

    async fn payment_state(&self, course_enrollment_id: &str) -> Result<PaymentState, StoreError> {
        let (known, enrollment, lead_times) = tokio::try_join!(
            self.store.known_invoices(course_enrollment_id),
            self.store.enrollment(course_enrollment_id),
            self.store.lead_times(course_enrollment_id),
        )?;
        Ok(PaymentState {
            known,
            enrollment,
            lead_times,
        })
    }

    async fn with_boleto_links(
        &self,
        course_enrollment_id: &str,
        planned: Vec<PlannedPayment>,
    ) -> (Vec<NewEvent>, Vec<bool>) {
        if !self.options.boleto_link {
            let events = planned.into_iter().map(|payment| payment.event).collect();
            return (events, Vec::new());
        }
        let invoices: BTreeSet<String> = planned
            .iter()
            .filter_map(|payment| payment.boleto_for.clone())
            .collect();
        let fetches: Vec<_> = invoices
            .into_iter()
            .map(|invoice_id| async move {
                let link = self.boleto_link(course_enrollment_id, &invoice_id).await;
                (invoice_id, link)
            })
            .collect();
        let links: HashMap<String, Option<String>> = join_all(fetches).await.into_iter().collect();
        let lookups = links.values().map(Option::is_some).collect();
        let events = planned
            .into_iter()
            .map(|payment| {
                let link = payment
                    .boleto_for
                    .and_then(|invoice_id| links.get(&invoice_id).cloned().flatten());
                with_link(payment.event, link)
            })
            .collect();
        (events, lookups)
    }

    async fn boleto_link(&self, course_enrollment_id: &str, invoice_id: &str) -> Option<String> {
        let link = self
            .portal
            .boleto_url(course_enrollment_id, invoice_id)
            .await;
        link.inspect_err(|failure| {
            warn!(
                error = failure as &dyn Error,
                "could not fetch the boleto link"
            );
        })
        .ok()
    }

    async fn track_payments(&self, complete: bool, now: DateTime<Utc>) -> Option<String> {
        self.payments_streak(complete, now)
            .await
            .err()
            .map(|failure| store_failed("payments", &failure))
    }

    async fn payments_streak(&self, complete: bool, now: DateTime<Utc>) -> Result<(), StoreError> {
        if complete {
            return self.store.payments_succeeded(now).await;
        }
        let streak = self.store.payments_failed(now).await?;
        let limit = TimeDelta::from_std(self.options.failing_alert_after).unwrap_or(TimeDelta::MAX);
        if streak.alerted || now - streak.since < limit {
            return Ok(());
        }
        let blind = blind_event(streak.since, self.options.failing_alert_after, now);
        self.record(&events_batch(vec![blind])).await?;
        self.store.mark_blind_alerted().await
    }

    async fn coursework(
        &self,
        enrollments: &[CourseEnrollment],
        sent: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let wanted: HashSet<String> = self
            .options
            .courses
            .iter()
            .map(|course| course_key(course))
            .collect();
        let watched: Vec<&CourseEnrollment> = enrollments
            .iter()
            .filter(|enrollment| wanted.contains(&course_key(&enrollment.course_name)))
            .collect();
        let today = local_today(now, self.options.tz);
        let reads: Vec<_> = watched
            .iter()
            .map(|enrollment| self.coursework_area(enrollment, today, now))
            .collect();
        let fetched = join_all(reads).await;
        let mut read = Vec::new();
        let mut areas = Vec::new();
        let mut failures = Vec::new();
        for (enrollment, result) in watched.into_iter().zip(fetched) {
            match result {
                Ok(coursework) => {
                    read.push(coursework);
                    areas.push(Area::coursework(enrollment));
                }
                Err(reason) => failures.push(reason),
            }
        }
        let batch = self.coursework_batch(&read, sent, now);
        if let Err(failure) = self.record(&batch).await {
            failures.extend(areas.iter().map(|area| store_failed(&area.label, &failure)));
            return failures;
        }
        let clears: Vec<_> = areas.iter().map(|area| self.clear(area)).collect();
        let cleared = join_all(clears).await;
        failures.extend(cleared.into_iter().filter_map(Result::err));
        failures
    }

    async fn coursework_area(
        &self,
        enrollment: &CourseEnrollment,
        today: NaiveDate,
        now: DateTime<Utc>,
    ) -> Result<Coursework, String> {
        match self.read_coursework(&enrollment.id, today).await {
            Ok(coursework) => Ok(coursework),
            Err(failures) => Err(self
                .portal_failed(&Area::coursework(enrollment), failures, now)
                .await),
        }
    }

    async fn read_coursework(
        &self,
        course_enrollment_id: &str,
        today: NaiveDate,
    ) -> Result<Coursework, Vec<AvaError>> {
        let (subjects, calendar) = tokio::join!(
            self.portal.subjects(course_enrollment_id),
            self.portal.calendar(course_enrollment_id, today),
        );
        let (subjects, calendar) = match (subjects, calendar) {
            (Ok(subjects), Ok(calendar)) => (subjects, calendar),
            (subjects, calendar) => {
                return Err(subjects.err().into_iter().chain(calendar.err()).collect());
            }
        };
        let quizzes = self.read_quizzes(&subjects).await?;
        Ok(Coursework {
            subjects,
            quizzes,
            calendar,
        })
    }

    async fn read_quizzes(
        &self,
        subjects: &[Subject],
    ) -> Result<Vec<(String, Vec<Quiz>)>, Vec<AvaError>> {
        let calls: Vec<_> =
            subjects
                .iter()
                .filter(|subject| subject.in_progress)
                .map(|subject| async move {
                    (subject.id.clone(), self.portal.quizzes(&subject.id).await)
                })
                .collect();
        let answers: Vec<(String, Result<Vec<Quiz>, AvaError>)> = stream::iter(calls)
            .buffer_unordered(QUIZ_CALLS)
            .collect()
            .await;
        let mut quizzes = Vec::new();
        let mut failures = Vec::new();
        for (subject_id, answer) in answers {
            match answer {
                Ok(found) => quizzes.push((subject_id, found)),
                Err(failure) => failures.push(failure),
            }
        }
        if failures.is_empty() {
            return Ok(quizzes);
        }
        Err(failures)
    }

    fn coursework_batch(
        &self,
        read: &[Coursework],
        sent: &HashSet<String>,
        now: DateTime<Utc>,
    ) -> RecordBatch {
        let mut items = Vec::new();
        let mut batch = RecordBatch::default();
        for coursework in read {
            let input = CourseworkInput {
                subjects: &coursework.subjects,
                quizzes: &coursework.quizzes,
                calendar: &coursework.calendar,
                calendar_categories: &self.options.calendar_categories,
                sent,
                now,
            };
            items.extend(coursework_items(&input, &self.options.deadlines));
            let (events, keys) = financial_events(&input, &self.options.deadlines);
            batch.events.extend(events);
            batch.sent_keys.extend(keys);
        }
        let mut event_ids = HashSet::new();
        batch
            .events
            .retain(|event| event_ids.insert(event.id.clone()));
        let mut item_keys = HashSet::new();
        items.retain(|item| item_keys.insert(item.mark_sent.clone()));
        let Some((event, keys)) = digest(items, now, self.options.tz) else {
            return batch;
        };
        batch.events.push(event);
        batch.sent_keys.extend(keys);
        batch
    }

    async fn missing_courses(
        &self,
        enrollments: &[CourseEnrollment],
        now: DateTime<Utc>,
    ) -> Option<String> {
        let available: BTreeSet<&str> = enrollments
            .iter()
            .map(|enrollment| enrollment.course_name.as_str())
            .collect();
        let found: HashSet<String> = available.iter().map(|name| course_key(name)).collect();
        let missing: Vec<&String> = self
            .options
            .courses
            .iter()
            .filter(|course| !found.contains(&course_key(course)))
            .collect();
        let listed = available_names(&available);
        self.memory().missing_courses = missing
            .iter()
            .map(|course| format!("course \"{course}\" not found (available: {listed})"))
            .collect();
        let events: Vec<NewEvent> = missing
            .iter()
            .map(|course| course_not_found_event(course, &listed, now))
            .collect();
        self.record(&events_batch(events))
            .await
            .err()
            .map(|failure| store_failed("courses", &failure))
    }

    async fn portal_failed(
        &self,
        area: &Area,
        failures: Vec<AvaError>,
        now: DateTime<Utc>,
    ) -> String {
        let notes: Vec<_> = failures
            .iter()
            .map(|failure| self.note(area, failure, now))
            .collect();
        join_all(notes).await;
        failures
            .iter()
            .map(|failure| format!("{}: {failure}", area.label))
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn note(&self, area: &Area, failure: &AvaError, now: DateTime<Utc>) {
        warn!(
            area = %area.label,
            error = failure as &dyn Error,
            "could not read the portal"
        );
        let noted = match failure {
            AvaError::Schema(problem) => self.site_changed(area, problem, now).await,
            AvaError::LoginRejected => self.login_rejected(now).await,
            AvaError::Unavailable(_) => Ok(()),
        };
        if let Err(failure) = noted {
            error!(
                area = %area.label,
                error = &failure as &dyn Error,
                "could not record the portal problem"
            );
        }
    }

    async fn site_changed(
        &self,
        area: &Area,
        problem: &Problem,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let signature = problem.signature();
        if !self.store.open_problem(&signature, &area.key, now).await? {
            return Ok(());
        }
        let today = local_today(now, self.options.tz);
        let event = site_changed_event(area, problem, &signature, today, now);
        let Err(failure) = self.record(&events_batch(vec![event])).await else {
            return Ok(());
        };
        warn!(
            area = %area.label,
            error = &failure as &dyn Error,
            "could not record the site change; the next cycle tries again"
        );
        self.store.close_problem(&signature).await
    }

    async fn login_rejected(&self, now: DateTime<Utc>) -> Result<(), StoreError> {
        let streak = *self.memory().login_streak.get_or_insert(LoginStreak {
            since: now,
            alerted: false,
        });
        if streak.alerted {
            return Ok(());
        }
        self.record(&events_batch(vec![login_failed_event(streak.since, now)]))
            .await?;
        let mut memory = self.memory();
        let current = memory
            .login_streak
            .as_mut()
            .filter(|current| current.since == streak.since);
        if let Some(current) = current {
            current.alerted = true;
        }
        Ok(())
    }

    async fn clear(&self, area: &Area) -> Result<(), String> {
        self.store
            .clear_problems(&area.key)
            .await
            .map_err(|failure| store_failed(&area.label, &failure))
    }

    async fn record(&self, batch: &RecordBatch) -> Result<(), StoreError> {
        if is_empty(batch) {
            return Ok(());
        }
        let Some(seq) = self.store.record(batch).await? else {
            return Ok(());
        };
        self.notifier.notify(seq);
        Ok(())
    }

    async fn refresh_problems(&self) {
        let problems = match self.store.open_problems().await {
            Ok(problems) => problems,
            Err(failure) => {
                error!(
                    error = &failure as &dyn Error,
                    "could not read the open portal problems"
                );
                return;
            }
        };
        self.memory().problems = problems;
    }

    async fn prune(&self) {
        if let Err(failure) = self.store.prune(KEEP_EVENTS, KEEP_KEYS_DAYS).await {
            error!(
                error = &failure as &dyn Error,
                "could not prune old events and reminder keys"
            );
        }
    }

    fn memory(&self) -> MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn outcome(failures: Vec<String>) -> CycleOutcome {
    let mut reasons: Vec<String> = Vec::new();
    for failure in failures {
        if !reasons.contains(&failure) {
            reasons.push(failure);
        }
    }
    if reasons.is_empty() {
        return CycleOutcome::Completed;
    }
    CycleOutcome::Incomplete(one_line(&reasons.join("; "), STATUS_MAX))
}

fn store_failed(area: &str, failure: &StoreError) -> String {
    error!(
        area = %area,
        error = failure as &dyn Error,
        "could not use the database"
    );
    format!("{area}: {DATABASE_UNAVAILABLE}")
}

fn boleto_unavailable(previous: bool, lookups: &[bool]) -> bool {
    if lookups.contains(&false) {
        return true;
    }
    if lookups.is_empty() {
        return previous;
    }
    false
}

fn with_link(event: NewEvent, link: Option<String>) -> NewEvent {
    let Some(url) = link else {
        return event;
    };
    NewEvent {
        body: format!("{}\nBoleto: {url}", event.body),
        url,
        ..event
    }
}

fn is_empty(batch: &RecordBatch) -> bool {
    batch.events.is_empty()
        && batch.sent_keys.is_empty()
        && batch.invoices.is_empty()
        && batch.enrollments.is_empty()
}

fn events_batch(events: Vec<NewEvent>) -> RecordBatch {
    RecordBatch {
        events,
        ..RecordBatch::default()
    }
}

fn instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn local_time(at: DateTime<Utc>, tz: Tz) -> String {
    let local = at.with_timezone(&tz);
    format!(
        "{} {}",
        format_day(local.date_naive()),
        local.format("%H:%M")
    )
}

fn span(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if !minutes.is_multiple_of(60) {
        return format!("{minutes} minutes");
    }
    let hours = minutes / 60;
    if hours == 1 {
        return "1 hour".to_owned();
    }
    format!("{hours} hours")
}

fn available_names(available: &BTreeSet<&str>) -> String {
    if available.is_empty() {
        return "none".to_owned();
    }
    available.iter().copied().collect::<Vec<_>>().join(", ")
}

fn urgent_event(
    kind: Kind,
    id: String,
    title: String,
    body: String,
    now: DateTime<Utc>,
) -> NewEvent {
    NewEvent {
        id,
        kind: kind.id(),
        urgent: true,
        title: one_line(&title, TITLE_MAX),
        body,
        url: HOME_URL.to_owned(),
        occurred_at: now,
    }
}

fn site_changed_event(
    area: &Area,
    problem: &Problem,
    signature: &str,
    today: NaiveDate,
    now: DateTime<Utc>,
) -> NewEvent {
    urgent_event(
        Kind::SiteChanged,
        format!("site-changed:{signature}:{today}"),
        "The portal changed and the plugin needs a new version".to_owned(),
        format!(
            "Area: {}\nOperation: {}\nField: {}",
            area.label,
            problem.operation,
            one_line(&problem.field, LINE_MAX)
        ),
        now,
    )
}

fn blind_event(since: DateTime<Utc>, after: Duration, now: DateTime<Utc>) -> NewEvent {
    urgent_event(
        Kind::MonitorBlind,
        format!("monitor-blind:{}", instant(since)),
        format!("Could not check payments for {}", span(after)),
        "Check the portal by hand.".to_owned(),
        now,
    )
}

fn login_failed_event(since: DateTime<Utc>, now: DateTime<Utc>) -> NewEvent {
    urgent_event(
        Kind::LoginFailed,
        format!("login-failed:{}", instant(since)),
        "The portal rejected the password".to_owned(),
        "The plugin waits 6 hours before trying again. Check the AVA password in the hub settings."
            .to_owned(),
        now,
    )
}

fn course_not_found_event(course: &str, available: &str, now: DateTime<Utc>) -> NewEvent {
    urgent_event(
        Kind::CourseNotFound,
        format!("course-not-found:{}", course_key(course)),
        format!("Course not found: {course}"),
        one_line(
            &format!("No course on the portal matches \"{course}\". Available: {available}."),
            LINE_MAX,
        ),
        now,
    )
}
