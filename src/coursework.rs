use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::ava::model::{Assessment, AssessmentKind, CalendarEvent, Quiz, Subject};
use crate::dates::{days_left, format_day, local_today};
use crate::events::{HOME_URL, Kind, NewEvent, due_phrase};
use crate::reminders::{ReminderRule, due_threshold, in_reminder_hours};
use crate::text::{course_key, fnv64, one_line};

const LINE_MAX: usize = 300;
const TITLE_MAX: usize = 80;

pub struct CourseworkInput<'a> {
    pub subjects: &'a [Subject],
    pub quizzes: &'a [(String, Vec<Quiz>)],
    pub calendar: &'a [CalendarEvent],
    pub calendar_categories: &'a [String],
    pub sent: &'a HashSet<String>,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestItem {
    pub deadline: NaiveDate,
    pub line: String,
    pub urgent: bool,
    pub mark_sent: Vec<String>,
}

struct Context<'a> {
    input: &'a CourseworkInput<'a>,
    rule: &'a ReminderRule,
    today: NaiveDate,
}

impl Context<'_> {
    fn events(&self) -> impl Iterator<Item = &CalendarEvent> {
        self.input
            .calendar
            .iter()
            .filter(|event| !event.canceled && event.category != "HOLIDAYS")
    }

    fn matching(&self, subject: &Subject, sub_category: &'static str) -> Vec<&CalendarEvent> {
        let name = course_key(&subject.name);
        self.events()
            .filter(|event| event.sub_category == sub_category && course_key(&event.title) == name)
            .collect()
    }

    fn period_end(&self, subject: &Subject, sub_category: &'static str) -> Option<NaiveDate> {
        self.matching(subject, sub_category)
            .into_iter()
            .filter_map(|event| event.end)
            .filter(|end| *end >= self.today)
            .min()
    }

    fn item(
        &self,
        deadline: NaiveDate,
        tail: &str,
        key: &dyn Fn(u32) -> String,
    ) -> Option<DigestItem> {
        let due = due_threshold(self.rule, deadline, self.input.now, self.input.sent, key)?;
        let left = days_left(self.today, deadline);
        Some(DigestItem {
            deadline,
            line: format!("{} ({}): {tail}", format_day(deadline), due_phrase(left)),
            urgent: left <= 1,
            mark_sent: due.mark_sent,
        })
    }
}

pub fn coursework_items(input: &CourseworkInput, rule: &ReminderRule) -> Vec<DigestItem> {
    if !in_reminder_hours(rule, input.now) {
        return Vec::new();
    }
    let ctx = Context {
        input,
        rule,
        today: local_today(input.now, rule.tz),
    };
    let active = || input.subjects.iter().filter(|subject| subject.in_progress);
    let mut items = Vec::new();
    for subject in active() {
        items.extend(assessment_items(&ctx, subject));
        items.extend(study_period_item(&ctx, subject));
        items.extend(exam_period_items(&ctx, subject));
    }
    for (subject_id, quizzes) in input.quizzes {
        let Some(subject) = active().find(|subject| subject.id == *subject_id) else {
            continue;
        };
        items.extend(
            quizzes
                .iter()
                .filter_map(|quiz| quiz_item(&ctx, subject, quiz)),
        );
    }
    items.extend(calendar_items(&ctx));
    items
}

fn assessment_items(ctx: &Context, subject: &Subject) -> Vec<DigestItem> {
    subject
        .assessments
        .iter()
        .filter(|assessment| assessment.available && !assessment.done && !assessment.attempted)
        .filter_map(|assessment| assessment_item(ctx, subject, assessment))
        .collect()
}

fn assessment_item(
    ctx: &Context,
    subject: &Subject,
    assessment: &Assessment,
) -> Option<DigestItem> {
    let sub_category = match assessment.kind {
        AssessmentKind::Assignment => "ASSIGNMENT",
        AssessmentKind::Exam => "EXAM_PERIOD",
    };
    let deadline = [
        Some(assessment.due_date),
        assessment.period_end,
        ctx.period_end(subject, sub_category),
    ]
    .into_iter()
    .flatten()
    .min()?;
    let tail = one_line(
        &format!(
            "{} — {} ({})",
            subject.name, assessment.name, assessment.period_title
        ),
        LINE_MAX,
    );
    ctx.item(deadline, &tail, &|t| {
        format!("deadline:{}:{deadline}:{t}", assessment.id)
    })
}

fn quiz_item(ctx: &Context, subject: &Subject, quiz: &Quiz) -> Option<DigestItem> {
    if quiz.attempted {
        return None;
    }
    let deadline = [Some(quiz.due_date), ctx.period_end(subject, "STUDY_PERIOD")]
        .into_iter()
        .flatten()
        .min()?;
    let tail = one_line(
        &format!("{} — unit quiz: {}", subject.name, quiz.unit_title),
        LINE_MAX,
    );
    ctx.item(deadline, &tail, &|t| {
        format!("quiz:{}:{deadline}:{t}", quiz.id)
    })
}

fn study_period_item(ctx: &Context, subject: &Subject) -> Option<DigestItem> {
    let (completed, quantity) = subject.attendance?;
    if completed >= quantity {
        return None;
    }
    let deadline = ctx.period_end(subject, "STUDY_PERIOD")?;
    let tail = one_line(
        &format!(
            "{} — watch the remaining {} of {quantity} lessons",
            subject.name,
            quantity - completed
        ),
        LINE_MAX,
    );
    ctx.item(deadline, &tail, &|t| {
        format!("study-period:{}:{deadline}:{t}", subject.id)
    })
}

fn exam_period_items(ctx: &Context, subject: &Subject) -> Vec<DigestItem> {
    let opening = ReminderRule {
        thresholds: vec![1],
        ..ctx.rule.clone()
    };
    ctx.matching(subject, "EXAM_PERIOD")
        .into_iter()
        .filter_map(|event| {
            let start = event.start?;
            let key = format!("exam-period:{}:{start}", event.id);
            let due = due_threshold(&opening, start, ctx.input.now, ctx.input.sent, &|_| {
                key.clone()
            })?;
            Some(DigestItem {
                deadline: start,
                line: one_line(
                    &format!(
                        "{}: {} — exam period opens ({})",
                        format_day(start),
                        subject.name,
                        event.description
                    ),
                    LINE_MAX,
                ),
                urgent: false,
                mark_sent: due.mark_sent,
            })
        })
        .collect()
}

fn calendar_date(event: &CalendarEvent) -> Option<NaiveDate> {
    event.end.or(event.event_date)
}

fn configured(input: &CourseworkInput, category: &str) -> bool {
    input
        .calendar_categories
        .iter()
        .any(|name| name == category)
}

fn calendar_items(ctx: &Context) -> Vec<DigestItem> {
    ctx.events()
        .filter(|event| !matches!(event.category.as_str(), "FINANCIAL" | "ACADEMIC_DATES"))
        .filter(|event| configured(ctx.input, &event.category))
        .filter_map(|event| {
            let date = calendar_date(event)?;
            let tail = one_line(&event.title, LINE_MAX);
            ctx.item(date, &tail, &|t| {
                format!("calendar:{}:{date}:{t}", event.id)
            })
        })
        .collect()
}

pub fn financial_events(
    input: &CourseworkInput,
    rule: &ReminderRule,
) -> (Vec<NewEvent>, Vec<String>) {
    if !in_reminder_hours(rule, input.now) || !configured(input, "FINANCIAL") {
        return (Vec::new(), Vec::new());
    }
    let today = local_today(input.now, rule.tz);
    let mut events = Vec::new();
    let mut keys = Vec::new();
    let financial = input
        .calendar
        .iter()
        .filter(|event| !event.canceled && event.category == "FINANCIAL");
    for event in financial {
        let Some(date) = calendar_date(event) else {
            continue;
        };
        let key = |t: u32| format!("calendar-financial:{}:{date}:{t}", event.id);
        let Some(due) = due_threshold(rule, date, input.now, input.sent, &key) else {
            continue;
        };
        events.push(NewEvent {
            id: key(due.threshold),
            kind: Kind::CalendarFinancial.id(),
            urgent: true,
            title: one_line(
                &format!("Financial date {}: {}", format_day(date), event.title),
                TITLE_MAX,
            ),
            body: format!(
                "{} ({})\n{}",
                format_day(date),
                due_phrase(days_left(today, date)),
                one_line(&event.description, LINE_MAX)
            ),
            url: HOME_URL.to_owned(),
            occurred_at: input.now,
        });
        keys.extend(due.mark_sent);
    }
    (events, keys)
}

pub fn digest(
    mut items: Vec<DigestItem>,
    now: DateTime<Utc>,
    tz: Tz,
) -> Option<(NewEvent, Vec<String>)> {
    if items.is_empty() {
        return None;
    }
    items.sort_by(|a, b| (a.deadline, &a.line).cmp(&(b.deadline, &b.line)));
    let mut keys: Vec<String> = items
        .iter()
        .flat_map(|item| item.mark_sent.iter().cloned())
        .collect();
    keys.sort();
    let count = items.len();
    let noun = if count == 1 { "deadline" } else { "deadlines" };
    let body = items
        .iter()
        .map(|item| one_line(&item.line, LINE_MAX))
        .collect::<Vec<_>>()
        .join("\n");
    let event = NewEvent {
        id: format!(
            "coursework:{}:{}",
            local_today(now, tz),
            fnv64(&keys.join("\n"))
        ),
        kind: Kind::CourseworkDigest.id(),
        urgent: items.iter().any(|item| item.urgent),
        title: format!("Coursework: {count} {noun}"),
        body,
        url: HOME_URL.to_owned(),
        occurred_at: now,
    };
    Some((event, keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Sao_Paulo;

    fn day(raw: &str) -> NaiveDate {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").expect("date")
    }

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("timestamp")
            .to_utc()
    }

    fn rule() -> ReminderRule {
        ReminderRule {
            thresholds: vec![7, 3, 1],
            hour: 9,
            tz: Sao_Paulo,
        }
    }

    fn subject(in_progress: bool) -> Subject {
        Subject {
            id: "s1".to_owned(),
            name: "Cálculo Diferencial".to_owned(),
            in_progress,
            attendance: None,
            assessments: Vec::new(),
        }
    }

    fn assessment(kind: AssessmentKind) -> Assessment {
        Assessment {
            id: "a1".to_owned(),
            kind,
            available: true,
            done: false,
            attempted: false,
            name: "Atividade 1".to_owned(),
            period_title: "Unidade 2".to_owned(),
            period_end: Some(day("2026-12-12")),
            due_date: day("2026-12-22"),
        }
    }

    fn quiz(attempted: bool) -> Quiz {
        Quiz {
            id: "q1".to_owned(),
            unit_title: "Unidade 1".to_owned(),
            name: "Quiz".to_owned(),
            due_date: day("2026-12-22"),
            attempted,
        }
    }

    fn event(id: &str, category: &str, sub_category: &str, title: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.to_owned(),
            category: category.to_owned(),
            sub_category: sub_category.to_owned(),
            title: title.to_owned(),
            description: "details".to_owned(),
            start: None,
            end: None,
            event_date: None,
            canceled: false,
        }
    }

    fn period(sub_category: &str, title: &str, end: &str) -> CalendarEvent {
        CalendarEvent {
            end: Some(day(end)),
            ..event(
                &format!("p-{sub_category}"),
                "ACADEMIC_DATES",
                sub_category,
                title,
            )
        }
    }

    fn categories() -> Vec<String> {
        ["FINANCIAL", "SECRETARIAT", "UNIQUE_EVENTS"]
            .map(str::to_owned)
            .to_vec()
    }

    fn items(
        subjects: &[Subject],
        quizzes: &[(String, Vec<Quiz>)],
        calendar: &[CalendarEvent],
        now: &str,
    ) -> Vec<DigestItem> {
        let categories = categories();
        let sent = HashSet::new();
        let input = CourseworkInput {
            subjects,
            quizzes,
            calendar,
            calendar_categories: &categories,
            sent: &sent,
            now: at(now),
        };
        coursework_items(&input, &rule())
    }

    fn with_assessment(kind: AssessmentKind, edit: impl FnOnce(&mut Assessment)) -> Subject {
        let mut value = assessment(kind);
        edit(&mut value);
        Subject {
            assessments: vec![value],
            ..subject(true)
        }
    }

    #[test]
    fn pending_assessment_uses_the_earliest_deadline() {
        let subjects = [Subject {
            assessments: vec![assessment(AssessmentKind::Assignment)],
            ..subject(true)
        }];
        let calendar = [period("ASSIGNMENT", "calculo diferencial", "2026-12-15")];
        let result = items(&subjects, &[], &calendar, "2026-12-05T12:00:00Z");
        assert_eq!(result.len(), 1);
        assert!(
            result[0].line.starts_with("12 Dec 2026 (in 7 days)"),
            "{}",
            result[0].line
        );
        assert_eq!(
            result[0].line,
            "12 Dec 2026 (in 7 days): Cálculo Diferencial — Atividade 1 (Unidade 2)"
        );
        assert_eq!(result[0].mark_sent, ["deadline:a1:2026-12-12:7"]);
        assert!(!result[0].urgent);
    }

    #[test]
    fn done_attempted_and_not_available_assessments_are_skipped() {
        let now = "2026-12-05T12:00:00Z";
        let done = with_assessment(AssessmentKind::Assignment, |a| a.done = true);
        let attempted = with_assessment(AssessmentKind::Exam, |a| a.attempted = true);
        let closed = with_assessment(AssessmentKind::Assignment, |a| a.available = false);
        assert!(items(&[done, attempted, closed], &[], &[], now).is_empty());
    }

    #[test]
    fn quiz_deadline_is_capped_by_the_study_period() {
        let subjects = [subject(true)];
        let quizzes = [("s1".to_owned(), vec![quiz(false)])];
        let calendar = [period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12")];
        let result = items(&subjects, &quizzes, &calendar, "2026-12-05T12:00:00Z");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].line,
            "12 Dec 2026 (in 7 days): Cálculo Diferencial — unit quiz: Unidade 1"
        );
        assert_eq!(result[0].mark_sent, ["quiz:q1:2026-12-12:7"]);

        let attempted = [("s1".to_owned(), vec![quiz(true)])];
        assert!(items(&subjects, &attempted, &calendar, "2026-12-05T12:00:00Z").is_empty());
    }

    #[test]
    fn study_period_reminds_only_with_unwatched_lessons() {
        let calendar = [period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12")];
        let now = "2026-12-05T12:00:00Z";
        let pending = [Subject {
            attendance: Some((3, 10)),
            ..subject(true)
        }];
        let result = items(&pending, &[], &calendar, now);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].line,
            "12 Dec 2026 (in 7 days): Cálculo Diferencial — watch the remaining 7 of 10 lessons"
        );
        assert_eq!(result[0].mark_sent, ["study-period:s1:2026-12-12:7"]);

        let watched = [Subject {
            attendance: Some((10, 10)),
            ..subject(true)
        }];
        assert!(items(&watched, &[], &calendar, now).is_empty());
    }

    #[test]
    fn exam_period_announces_the_day_before() {
        let mut opening = period("EXAM_PERIOD", "Cálculo Diferencial", "2026-12-20");
        opening.start = Some(day("2026-12-10"));
        opening.description = "first sitting".to_owned();
        let subjects = [subject(true)];
        let calendar = [opening];
        assert!(items(&subjects, &[], &calendar, "2026-12-08T12:00:00Z").is_empty());
        let result = items(&subjects, &[], &calendar, "2026-12-09T12:00:00Z");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].line,
            "10 Dec 2026: Cálculo Diferencial — exam period opens (first sitting)"
        );
        assert_eq!(
            result[0].mark_sent,
            ["exam-period:p-EXAM_PERIOD:2026-12-10"]
        );
        assert!(!result[0].urgent);
    }

    #[test]
    fn two_exam_periods_each_announce_themselves() {
        let mut first = period("EXAM_PERIOD", "Cálculo Diferencial", "2026-12-20");
        first.start = Some(day("2026-12-10"));
        let mut second = event(
            "second",
            "ACADEMIC_DATES",
            "EXAM_PERIOD",
            "Cálculo Diferencial",
        );
        second.start = Some(day("2026-12-10"));
        second.end = Some(day("2026-12-30"));
        let result = items(
            &[subject(true)],
            &[],
            &[first, second],
            "2026-12-09T12:00:00Z",
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn subjects_not_in_progress_are_ignored() {
        let mut idle = with_assessment(AssessmentKind::Assignment, |_| {});
        idle.in_progress = false;
        idle.attendance = Some((0, 10));
        let quizzes = [("s1".to_owned(), vec![quiz(false)])];
        let calendar = [period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12")];
        assert!(items(&[idle], &quizzes, &calendar, "2026-12-05T12:00:00Z").is_empty());
    }

    #[test]
    fn calendar_matching_ignores_case_and_accents() {
        let subjects = [Subject {
            attendance: Some((0, 4)),
            ..subject(true)
        }];
        let calendar = [period(
            "STUDY_PERIOD",
            "  CALCULO   diferencial ",
            "2026-12-12",
        )];
        assert_eq!(
            items(&subjects, &[], &calendar, "2026-12-05T12:00:00Z").len(),
            1
        );
    }

    #[test]
    fn canceled_and_holiday_events_are_ignored() {
        let subjects = [Subject {
            attendance: Some((0, 4)),
            ..subject(true)
        }];
        let mut canceled = period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12");
        canceled.canceled = true;
        let mut holiday = period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12");
        holiday.category = "HOLIDAYS".to_owned();
        assert!(items(&subjects, &[], &[canceled, holiday], "2026-12-05T12:00:00Z").is_empty());
    }

    #[test]
    fn calendar_events_of_configured_categories_reach_the_digest() {
        let mut secretariat = event("c1", "SECRETARIAT", "", "Enrollment renewal");
        secretariat.end = Some(day("2026-12-12"));
        let mut unique = event("c2", "UNIQUE_EVENTS", "", "Graduation");
        unique.event_date = Some(day("2026-12-12"));
        let result = items(&[], &[], &[secretariat, unique], "2026-12-05T12:00:00Z");
        let lines: Vec<&str> = result.iter().map(|item| item.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "12 Dec 2026 (in 7 days): Enrollment renewal",
                "12 Dec 2026 (in 7 days): Graduation"
            ]
        );
        assert_eq!(result[0].mark_sent, ["calendar:c1:2026-12-12:7"]);
    }

    #[test]
    fn unconfigured_calendar_categories_are_ignored() {
        let mut secretariat = event("c1", "SECRETARIAT", "", "Enrollment renewal");
        secretariat.end = Some(day("2026-12-12"));
        let only = ["UNIQUE_EVENTS".to_owned()];
        let sent = HashSet::new();
        let calendar = [secretariat];
        let input = CourseworkInput {
            subjects: &[],
            quizzes: &[],
            calendar: &calendar,
            calendar_categories: &only,
            sent: &sent,
            now: at("2026-12-05T12:00:00Z"),
        };
        assert!(coursework_items(&input, &rule()).is_empty());
        assert!(financial_events(&input, &rule()).0.is_empty());
    }

    #[test]
    fn reminders_wait_for_the_reminder_hour() {
        let subjects = [Subject {
            attendance: Some((0, 4)),
            ..subject(true)
        }];
        let calendar = [period("STUDY_PERIOD", "Cálculo Diferencial", "2026-12-12")];
        assert!(items(&subjects, &[], &calendar, "2026-12-05T06:00:00Z").is_empty());
        assert_eq!(
            items(&subjects, &[], &calendar, "2026-12-05T12:00:00Z").len(),
            1
        );

        let mut fin = event("f1", "FINANCIAL", "", "Tuition");
        fin.end = Some(day("2026-12-10"));
        let categories = categories();
        let sent = HashSet::new();
        let calendar = [fin];
        let input = |now| CourseworkInput {
            subjects: &[],
            quizzes: &[],
            calendar: &calendar,
            calendar_categories: &categories,
            sent: &sent,
            now: at(now),
        };
        let night = financial_events(&input("2026-12-09T06:00:00Z"), &rule());
        assert!(night.0.is_empty() && night.1.is_empty());
        assert_eq!(
            financial_events(&input("2026-12-09T12:00:00Z"), &rule())
                .0
                .len(),
            1
        );
    }

    fn digest_item(deadline: &str, line: &str, urgent: bool, key: &str) -> DigestItem {
        DigestItem {
            deadline: day(deadline),
            line: line.to_owned(),
            urgent,
            mark_sent: vec![key.to_owned()],
        }
    }

    #[test]
    fn digest_is_one_event_with_sorted_lines_and_stable_id() {
        let now = at("2026-12-05T12:00:00Z");
        let late = digest_item("2026-12-12", "late", false, "k-late");
        let early = digest_item("2026-12-08", "early", false, "k-early");
        let (event, keys) =
            digest(vec![late.clone(), early.clone()], now, Sao_Paulo).expect("digest");
        assert_eq!(event.kind, "coursework_digest");
        assert_eq!(event.title, "Coursework: 2 deadlines");
        assert_eq!(event.body, "early\nlate");
        assert_eq!(event.url, HOME_URL);
        assert_eq!(event.occurred_at, now);
        assert!(!event.urgent);
        assert_eq!(keys, ["k-early", "k-late"]);
        assert_eq!(
            event.id,
            format!("coursework:2026-12-05:{}", fnv64("k-early\nk-late"))
        );

        let (again, _) = digest(vec![early.clone(), late.clone()], now, Sao_Paulo).expect("digest");
        assert_eq!(again.id, event.id);

        let soon = digest_item("2026-12-06", "soon", true, "k-soon");
        let (urgent, _) = digest(vec![late, soon], now, Sao_Paulo).expect("digest");
        assert!(urgent.urgent);

        let (single, _) = digest(vec![early], now, Sao_Paulo).expect("digest");
        assert_eq!(single.title, "Coursework: 1 deadline");
        assert!(digest(Vec::new(), now, Sao_Paulo).is_none());
    }

    #[test]
    fn financial_calendar_events_are_separate_and_urgent() {
        let mut fin = event("f1", "FINANCIAL", "", "Tuition discount ends");
        fin.end = Some(day("2026-12-10"));
        fin.description = "pay before".to_owned();
        let categories = categories();
        let sent = HashSet::new();
        let calendar = [fin];
        let input = CourseworkInput {
            subjects: &[],
            quizzes: &[],
            calendar: &calendar,
            calendar_categories: &categories,
            sent: &sent,
            now: at("2026-12-09T12:00:00Z"),
        };
        let (events, keys) = financial_events(&input, &rule());
        assert_eq!(events.len(), 1);
        let first = &events[0];
        assert_eq!(first.id, "calendar-financial:f1:2026-12-10:1");
        assert_eq!(first.kind, "calendar_financial");
        assert!(first.urgent);
        assert_eq!(
            first.title,
            "Financial date 10 Dec 2026: Tuition discount ends"
        );
        assert_eq!(first.body, "10 Dec 2026 (tomorrow)\npay before");
        assert_eq!(first.url, HOME_URL);
        assert_eq!(
            keys,
            [
                "calendar-financial:f1:2026-12-10:7",
                "calendar-financial:f1:2026-12-10:3",
                "calendar-financial:f1:2026-12-10:1"
            ]
        );
        assert!(coursework_items(&input, &rule()).is_empty());
    }
}
