use chrono::NaiveDate;
use chrono_tz::Tz;
use serde_json::Value;

use crate::dates::{parse_period_end, parse_portal_date};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub operation: &'static str,
    pub field: String,
    pub kind: ProblemKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    GraphqlError(String),
    Missing,
    WrongType,
    UnknownValue(String),
    UnparseableDate(String),
}

impl Problem {
    pub fn signature(&self) -> String {
        let tag = match &self.kind {
            ProblemKind::GraphqlError(_) => "graphql_error",
            ProblemKind::Missing => "missing",
            ProblemKind::WrongType => "wrong_type",
            ProblemKind::UnknownValue(_) => "unknown_value",
            ProblemKind::UnparseableDate(_) => "unparseable_date",
        };
        format!("{}:{}:{tag}", self.operation, self.field)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CourseEnrollment {
    pub id: String,
    pub status: String,
    pub course_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InformationType {
    PaymentsOnTime,
    None,
    DocumentBlock,
    GenericBlock,
    ReEnrollmentNotPaid,
    Negotiation,
    ReEnrollmentNegotiation,
}

const INFORMATION_TYPES: [(&str, InformationType); 7] = [
    ("PAYMENTS_ON_TIME", InformationType::PaymentsOnTime),
    ("NONE", InformationType::None),
    ("DOCUMENT_BLOCK", InformationType::DocumentBlock),
    ("GENERIC_BLOCK", InformationType::GenericBlock),
    (
        "RE_ENROLLMENT_NOT_PAID",
        InformationType::ReEnrollmentNotPaid,
    ),
    ("NEGOTIATION", InformationType::Negotiation),
    (
        "RE_ENROLLMENT_NEGOTIATION",
        InformationType::ReEnrollmentNegotiation,
    ),
];

impl InformationType {
    pub fn as_str(self) -> &'static str {
        name_of(&INFORMATION_TYPES, self)
    }

    pub fn needs_attention(self) -> bool {
        !matches!(
            self,
            InformationType::PaymentsOnTime | InformationType::None
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceStatus {
    Open,
    Paid,
    Cancelled,
    Blocked,
    Overdue,
    Pending,
    Failed,
    Underpaid,
    Overpaid,
    Refunded,
    WroteOff,
    Available,
}

const INVOICE_STATUSES: [(&str, InvoiceStatus); 12] = [
    ("OPEN", InvoiceStatus::Open),
    ("PAID", InvoiceStatus::Paid),
    ("CANCELLED", InvoiceStatus::Cancelled),
    ("BLOCKED", InvoiceStatus::Blocked),
    ("OVERDUE", InvoiceStatus::Overdue),
    ("PENDING", InvoiceStatus::Pending),
    ("FAILED", InvoiceStatus::Failed),
    ("UNDERPAID", InvoiceStatus::Underpaid),
    ("OVERPAID", InvoiceStatus::Overpaid),
    ("REFUNDED", InvoiceStatus::Refunded),
    ("WROTE_OFF", InvoiceStatus::WroteOff),
    ("AVAILABLE", InvoiceStatus::Available),
];

impl InvoiceStatus {
    pub fn as_str(self) -> &'static str {
        name_of(&INVOICE_STATUSES, self)
    }

    pub fn is_settled(self) -> bool {
        matches!(
            self,
            InvoiceStatus::Paid
                | InvoiceStatus::Cancelled
                | InvoiceStatus::Refunded
                | InvoiceStatus::WroteOff
                | InvoiceStatus::Overpaid
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoice {
    pub id: String,
    pub status: InvoiceStatus,
    pub title: String,
    pub amount: String,
    pub due_date: NaiveDate,
    pub alerts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceHome {
    pub information_type: InformationType,
    pub billing_day: Option<u32>,
    pub invoices: Vec<Invoice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssessmentKind {
    Assignment,
    Exam,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub id: String,
    pub kind: AssessmentKind,
    pub available: bool,
    pub done: bool,
    pub attempted: bool,
    pub name: String,
    pub period_title: String,
    pub period_end: Option<NaiveDate>,
    pub due_date: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub id: String,
    pub name: String,
    pub in_progress: bool,
    pub attendance: Option<(u32, u32)>,
    pub assessments: Vec<Assessment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quiz {
    pub id: String,
    pub unit_title: String,
    pub name: String,
    pub due_date: NaiveDate,
    pub attempted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String,
    pub category: String,
    pub sub_category: String,
    pub title: String,
    pub description: String,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub event_date: Option<NaiveDate>,
    pub canceled: bool,
}

type Decoded<T> = Result<T, Problem>;

fn name_of<T: Copy + PartialEq>(table: &[(&'static str, T)], value: T) -> &'static str {
    table
        .iter()
        .find(|(_, candidate)| *candidate == value)
        .map_or("", |(name, _)| name)
}

fn lookup<T: Copy>(table: &[(&str, T)], raw: &str) -> Option<T> {
    table
        .iter()
        .find(|(name, _)| *name == raw)
        .map(|(_, value)| *value)
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        return key.to_owned();
    }
    format!("{path}.{key}")
}

fn fail(operation: &'static str, field: String, kind: ProblemKind) -> Problem {
    Problem {
        operation,
        field,
        kind,
    }
}

fn skip(operation: &'static str, field: &str, value: &str) {
    tracing::warn!(
        operation,
        field,
        value,
        "skipped a portal value it does not know"
    );
}

fn required<'a>(
    operation: &'static str,
    parent: &'a Value,
    path: &str,
    key: &str,
) -> Decoded<&'a Value> {
    parent
        .get(key)
        .filter(|value| !value.is_null())
        .ok_or_else(|| fail(operation, join(path, key), ProblemKind::Missing))
}

fn required_str<'a>(
    operation: &'static str,
    parent: &'a Value,
    path: &str,
    key: &str,
) -> Decoded<&'a str> {
    required(operation, parent, path, key)?
        .as_str()
        .ok_or_else(|| fail(operation, join(path, key), ProblemKind::WrongType))
}

fn required_array<'a>(
    operation: &'static str,
    parent: &'a Value,
    path: &str,
    key: &str,
) -> Decoded<&'a [Value]> {
    required(operation, parent, path, key)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| fail(operation, join(path, key), ProblemKind::WrongType))
}

fn required_date(
    operation: &'static str,
    parent: &Value,
    path: &str,
    key: &str,
    tz: Tz,
) -> Decoded<NaiveDate> {
    let raw = required_str(operation, parent, path, key)?;
    parse_portal_date(raw, tz).ok_or_else(|| {
        fail(
            operation,
            join(path, key),
            ProblemKind::UnparseableDate(raw.to_owned()),
        )
    })
}

fn root_array<'a>(operation: &'static str, body: &'a Value) -> Decoded<&'a [Value]> {
    required_array(operation, body, "", "data")
}

fn owned(value: &str) -> String {
    value.to_owned()
}

pub fn decode_student_id(body: &Value, _tz: Tz) -> Decoded<String> {
    const OPERATION: &str = "StudentId";
    let data = required(OPERATION, body, "", "data")?;
    required_str(OPERATION, data, "", "id").map(owned)
}

pub fn decode_course_enrollments(body: &Value, _tz: Tz) -> Decoded<Vec<CourseEnrollment>> {
    const OPERATION: &str = "CourseEnrollments";
    root_array(OPERATION, body)?
        .iter()
        .map(|entry| {
            let course = required(OPERATION, entry, "", "course")?;
            Ok(CourseEnrollment {
                id: owned(required_str(OPERATION, entry, "", "id")?),
                status: owned(required_str(OPERATION, entry, "", "status")?),
                course_name: owned(required_str(OPERATION, course, "course", "name")?),
            })
        })
        .collect()
}

pub fn decode_boleto_url(body: &Value, _tz: Tz) -> Decoded<String> {
    const OPERATION: &str = "BoletoPdf";
    let data = required(OPERATION, body, "", "data")?;
    required_str(OPERATION, data, "", "url").map(owned)
}

pub fn decode_invoices(body: &Value, tz: Tz) -> Decoded<InvoiceHome> {
    const OPERATION: &str = "Invoices";
    let data = required(OPERATION, body, "", "data")?;
    let home = required(OPERATION, data, "", "getInvoiceHomeModel")?;
    let raw_type = required_str(OPERATION, home, "", "informationType")?;
    let information_type = lookup(&INFORMATION_TYPES, raw_type).ok_or_else(|| {
        fail(
            OPERATION,
            "informationType".to_owned(),
            ProblemKind::UnknownValue(owned(raw_type)),
        )
    })?;
    let billing_day = decode_billing_day(home)?;
    let mut invoices = Vec::new();
    for section in required_array(OPERATION, home, "", "invoiceSections")? {
        let entries = required_array(OPERATION, section, "invoiceSections[]", "invoices")?;
        for entry in entries {
            invoices.push(decode_invoice(entry, tz)?);
        }
    }
    Ok(InvoiceHome {
        information_type,
        billing_day,
        invoices,
    })
}

fn decode_billing_day(home: &Value) -> Decoded<Option<u32>> {
    const OPERATION: &str = "Invoices";
    let raw = required_str(OPERATION, home, "", "billingDay")?.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<u32>()
        .ok()
        .filter(|day| (1..=31).contains(day))
        .map(Some)
        .ok_or_else(|| {
            fail(
                OPERATION,
                "billingDay".to_owned(),
                ProblemKind::UnknownValue(owned(raw)),
            )
        })
}

fn decode_invoice(entry: &Value, tz: Tz) -> Decoded<Invoice> {
    const OPERATION: &str = "Invoices";
    const PATH: &str = "invoiceSections[].invoices[]";
    let raw_status = required_str(OPERATION, entry, PATH, "status")?;
    let status = lookup(&INVOICE_STATUSES, raw_status).ok_or_else(|| {
        fail(
            OPERATION,
            join(PATH, "status"),
            ProblemKind::UnknownValue(owned(raw_status)),
        )
    })?;
    let short_title = entry
        .get("shortTitle")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty());
    let title = match short_title {
        Some(title) => title,
        None => required_str(OPERATION, entry, PATH, "title")?,
    };
    let alerts = ["paymentAlert", "invoiceAlert"]
        .iter()
        .filter_map(|key| entry.get("details")?.get(*key)?.get("message")?.as_str())
        .filter(|message| !message.is_empty())
        .map(owned)
        .collect();
    Ok(Invoice {
        id: owned(required_str(OPERATION, entry, PATH, "id")?),
        status,
        title: owned(title),
        amount: owned(required_str(OPERATION, entry, PATH, "formattedAmount")?),
        due_date: required_date(OPERATION, entry, PATH, "currentDueDate", tz)?,
        alerts,
    })
}

pub fn decode_subjects(body: &Value, tz: Tz) -> Decoded<Vec<Subject>> {
    const OPERATION: &str = "SubjectDeadlines";
    let data = required(OPERATION, body, "", "data")?;
    let mut subjects = Vec::new();
    for entry in required_array(OPERATION, data, "", "subjectEnrollments")? {
        subjects.extend(decode_subject(entry, tz)?);
    }
    Ok(subjects)
}

fn decode_subject(entry: &Value, tz: Tz) -> Decoded<Option<Subject>> {
    const OPERATION: &str = "SubjectDeadlines";
    const PATH: &str = "subjectEnrollments[]";
    const STATUSES: [(&str, bool); 3] =
        [("PASSED", false), ("FAILED", false), ("IN_PROGRESS", true)];
    let raw_status = required_str(OPERATION, entry, PATH, "status")?;
    let Some(in_progress) = lookup(&STATUSES, raw_status) else {
        skip(OPERATION, &join(PATH, "status"), raw_status);
        return Ok(None);
    };
    let subject = required(OPERATION, entry, PATH, "subject")?;
    let mut assessments = Vec::new();
    if in_progress {
        for assignment in required_array(OPERATION, entry, PATH, "assignments")? {
            assessments.extend(decode_assessment(assignment, tz)?);
        }
    }
    Ok(Some(Subject {
        id: owned(required_str(OPERATION, entry, PATH, "id")?),
        name: owned(required_str(
            OPERATION,
            subject,
            "subjectEnrollments[].subject",
            "name",
        )?),
        in_progress,
        attendance: decode_attendance(entry),
        assessments,
    }))
}

fn decode_attendance(entry: &Value) -> Option<(u32, u32)> {
    let details = entry.get("progress")?.get("details")?.as_array()?;
    let attendance = details
        .iter()
        .find(|detail| detail.get("type").and_then(Value::as_str) == Some("ATTENDANCE"))?;
    let quantity = u32::try_from(attendance.get("quantity")?.as_u64()?).ok()?;
    let completed = attendance
        .get("completed")
        .and_then(Value::as_u64)
        .and_then(|completed| u32::try_from(completed).ok())
        .unwrap_or(0);
    Some((completed, quantity))
}

fn decode_assessment(entry: &Value, tz: Tz) -> Decoded<Option<Assessment>> {
    const OPERATION: &str = "SubjectDeadlines";
    const PATH: &str = "subjectEnrollments[].assignments[]";
    const DETAIL: &str = "subjectEnrollments[].assignments[].detail";
    const STATUSES: [(&str, (bool, bool)); 3] = [
        ("AVAILABLE", (true, false)),
        ("DONE", (false, true)),
        ("NOT_AVAILABLE", (false, false)),
    ];
    const KINDS: [(&str, AssessmentKind); 2] = [
        ("ASSIGNMENT", AssessmentKind::Assignment),
        ("EXAM", AssessmentKind::Exam),
    ];
    let raw_status = required_str(OPERATION, entry, PATH, "status")?;
    let Some((available, done)) = lookup(&STATUSES, raw_status) else {
        skip(OPERATION, &join(PATH, "status"), raw_status);
        return Ok(None);
    };
    let detail = required(OPERATION, entry, PATH, "detail")?;
    let config = required(OPERATION, detail, DETAIL, "config")?;
    let config_path = join(DETAIL, "config");
    let raw_kind = required_str(OPERATION, config, &config_path, "type")?;
    let Some(kind) = lookup(&KINDS, raw_kind) else {
        skip(OPERATION, &join(&config_path, "type"), raw_kind);
        return Ok(None);
    };
    let display = required(OPERATION, detail, DETAIL, "display")?;
    let display_path = join(DETAIL, "display");
    let period_end = decode_period_end(display, &display_path);
    let attempts = required_array(OPERATION, detail, DETAIL, "attempts")?;
    Ok(Some(Assessment {
        id: owned(required_str(OPERATION, entry, PATH, "id")?),
        kind,
        available,
        done,
        attempted: !attempts.is_empty(),
        name: owned(required_str(OPERATION, display, &display_path, "name")?),
        period_title: owned(required_str(
            OPERATION,
            display,
            &display_path,
            "periodTitle",
        )?),
        period_end,
        due_date: required_date(OPERATION, detail, DETAIL, "dueDate", tz)?,
    }))
}

fn decode_period_end(display: &Value, display_path: &str) -> Option<NaiveDate> {
    let period_end = display
        .get("periodDates")
        .and_then(Value::as_str)
        .and_then(parse_period_end);
    if period_end.is_none() {
        tracing::warn!(
            operation = "SubjectDeadlines",
            field = join(display_path, "periodDates"),
            "could not read the period dates"
        );
    }
    period_end
}

pub fn decode_quizzes(body: &Value, tz: Tz) -> Decoded<Vec<Quiz>> {
    const OPERATION: &str = "UnitAssignments";
    const PATH: &str = "assignments[]";
    const DETAIL: &str = "assignments[].detail";
    let mut quizzes = Vec::new();
    for unit in root_array(OPERATION, body)? {
        let unit_title = required_str(OPERATION, unit, "", "title")?;
        for entry in required_array(OPERATION, unit, "", "assignments")? {
            let detail = required(OPERATION, entry, PATH, "detail")?;
            let config = required(OPERATION, detail, DETAIL, "config")?;
            let config_path = join(DETAIL, "config");
            if required_str(OPERATION, config, &config_path, "type")? != "QUIZ" {
                continue;
            }
            let attempts = required_array(OPERATION, detail, DETAIL, "attempts")?;
            quizzes.push(Quiz {
                id: owned(required_str(OPERATION, detail, DETAIL, "assignmentId")?),
                unit_title: owned(unit_title),
                name: owned(required_str(OPERATION, config, &config_path, "name")?),
                due_date: required_date(OPERATION, detail, DETAIL, "dueDate", tz)?,
                attempted: !attempts.is_empty(),
            });
        }
    }
    Ok(quizzes)
}

pub fn decode_calendar(body: &Value, tz: Tz) -> Decoded<Vec<CalendarEvent>> {
    const OPERATION: &str = "Calendar";
    let mut events = Vec::new();
    for entry in root_array(OPERATION, body)? {
        match decode_event(entry, tz) {
            Some(event) => events.push(event),
            None => tracing::warn!(
                operation = OPERATION,
                field = "data[]",
                "skipped a calendar event it could not read"
            ),
        }
    }
    Ok(events)
}

fn decode_event(entry: &Value, tz: Tz) -> Option<CalendarEvent> {
    let text = |key: &str| entry.get(key)?.as_str().map(owned);
    let date = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .and_then(|raw| parse_portal_date(raw, tz))
    };
    let (start, end, event_date) = (date("startDate"), date("endDate"), date("eventDate"));
    if start.is_none() && end.is_none() && event_date.is_none() {
        return None;
    }
    Some(CalendarEvent {
        id: text("eventId")?,
        category: text("category")?,
        sub_category: text("subCategory")?,
        title: text("title")?,
        description: text("description")?,
        start,
        end,
        event_date,
        canceled: entry.get("isCanceled")?.as_bool()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono_tz::America::Sao_Paulo;
    use serde_json::{Value, json};

    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).expect("fixture is JSON")
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("date")
    }

    fn invoices_body(invoice: Value) -> Value {
        json!({"data": {"getInvoiceHomeModel": {
            "informationType": "PAYMENTS_ON_TIME",
            "billingDay": "10",
            "invoiceSections": [{"title": "Mensalidades", "invoices": [invoice]}],
        }}})
    }

    fn invoice(overrides: Value) -> Value {
        let mut base = json!({
            "id": "invoice-1",
            "status": "OPEN",
            "title": "Mensalidade",
            "shortTitle": "",
            "formattedAmount": "R$ 1,00",
            "currentDueDate": "2026-12-10",
            "details": null,
        });
        for (key, value) in overrides.as_object().expect("object") {
            base[key] = value.clone();
        }
        base
    }

    fn subjects_fixture() -> Vec<Subject> {
        let body = fixture(include_str!("../../testdata/ava/SubjectDeadlines.json"));
        decode_subjects(&body, Sao_Paulo).expect("subjects")
    }

    #[test]
    fn paid_invoices_decode_with_billing_day() {
        let body = fixture(include_str!("../../testdata/ava/Invoices.json"));

        let home = decode_invoices(&body, Sao_Paulo).expect("home");

        assert_eq!(home.information_type, InformationType::PaymentsOnTime);
        assert_eq!(home.billing_day, Some(10));
        assert!(home.invoices.is_empty());
    }

    #[test]
    fn open_invoice_decodes_title_amount_date_and_alerts() {
        let body = fixture(include_str!("../../testdata/ava/Invoices.open.json"));

        let home = decode_invoices(&body, Sao_Paulo).expect("home");

        assert_eq!(home.invoices.len(), 1);
        let invoice = &home.invoices[0];
        assert_eq!(invoice.id, "invoice-1");
        assert_eq!(invoice.status, InvoiceStatus::Open);
        assert_eq!(invoice.title, "Dezembro");
        assert_eq!(invoice.amount, "R$ 123,45");
        assert_eq!(invoice.due_date, date(2026, 12, 10));
        assert_eq!(invoice.alerts, vec!["Pague até o vencimento".to_owned()]);
    }

    #[test]
    fn empty_short_title_falls_back_to_title() {
        let body = invoices_body(invoice(json!({})));

        let home = decode_invoices(&body, Sao_Paulo).expect("home");

        assert_eq!(home.invoices[0].title, "Mensalidade");
    }

    #[test]
    fn due_date_formats_are_all_accepted() {
        let raws = [
            "2026-12-10",
            "10/12/2026",
            "2026-12-10T12:00:00Z",
            "2026-12-10T09:00:00-03:00",
        ];
        for raw in raws {
            let body = invoices_body(invoice(json!({"currentDueDate": raw})));

            let home = decode_invoices(&body, Sao_Paulo).expect(raw);

            assert_eq!(home.invoices[0].due_date, date(2026, 12, 10), "{raw}");
        }
    }

    #[test]
    fn unknown_invoice_status_is_a_problem() {
        let body = invoices_body(invoice(json!({"status": "SOMETHING_NEW"})));

        let problem = decode_invoices(&body, Sao_Paulo).expect_err("problem");

        assert_eq!(
            problem,
            Problem {
                operation: "Invoices",
                field: "invoiceSections[].invoices[].status".to_owned(),
                kind: ProblemKind::UnknownValue("SOMETHING_NEW".to_owned()),
            }
        );
    }

    #[test]
    fn unknown_information_type_is_a_problem() {
        let mut body = fixture(include_str!("../../testdata/ava/Invoices.json"));
        body["data"]["getInvoiceHomeModel"]["informationType"] = json!("SOMETHING_NEW");

        let problem = decode_invoices(&body, Sao_Paulo).expect_err("problem");

        assert_eq!(problem.field, "informationType");
        assert_eq!(
            problem.kind,
            ProblemKind::UnknownValue("SOMETHING_NEW".to_owned())
        );
    }

    #[test]
    fn missing_invoice_sections_is_a_problem() {
        let mut body = fixture(include_str!("../../testdata/ava/Invoices.json"));
        body["data"]["getInvoiceHomeModel"]
            .as_object_mut()
            .expect("object")
            .remove("invoiceSections");

        let problem = decode_invoices(&body, Sao_Paulo).expect_err("problem");

        assert_eq!(problem.field, "invoiceSections");
        assert_eq!(problem.kind, ProblemKind::Missing);
    }

    #[test]
    fn unparseable_due_date_is_a_problem() {
        let body = invoices_body(invoice(json!({"currentDueDate": "10 de dezembro"})));

        let problem = decode_invoices(&body, Sao_Paulo).expect_err("problem");

        assert_eq!(problem.field, "invoiceSections[].invoices[].currentDueDate");
        assert_eq!(
            problem.kind,
            ProblemKind::UnparseableDate("10 de dezembro".to_owned())
        );
    }

    #[test]
    fn billing_day_empty_is_none_and_out_of_range_is_a_problem() {
        let mut body = fixture(include_str!("../../testdata/ava/Invoices.json"));
        body["data"]["getInvoiceHomeModel"]["billingDay"] = json!("");
        assert_eq!(
            decode_invoices(&body, Sao_Paulo).expect("home").billing_day,
            None
        );

        body["data"]["getInvoiceHomeModel"]["billingDay"] = json!("32");
        let problem = decode_invoices(&body, Sao_Paulo).expect_err("problem");
        assert_eq!(problem.field, "billingDay");
    }

    #[test]
    fn information_type_needs_attention_except_on_time_and_none() {
        assert!(!InformationType::PaymentsOnTime.needs_attention());
        assert!(!InformationType::None.needs_attention());
        assert!(InformationType::DocumentBlock.needs_attention());
        assert_eq!(
            InformationType::ReEnrollmentNegotiation.as_str(),
            "RE_ENROLLMENT_NEGOTIATION"
        );
    }

    #[test]
    fn invoice_status_settled_set() {
        assert!(InvoiceStatus::Paid.is_settled());
        assert!(InvoiceStatus::WroteOff.is_settled());
        assert!(!InvoiceStatus::Open.is_settled());
        assert!(!InvoiceStatus::Overdue.is_settled());
        assert_eq!(InvoiceStatus::WroteOff.as_str(), "WROTE_OFF");
    }

    #[test]
    fn course_enrollments_decode() {
        let body = fixture(include_str!("../../testdata/ava/CourseEnrollments.json"));

        let courses = decode_course_enrollments(&body, Sao_Paulo).expect("courses");

        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].id, "KRT-00000000-0000-0000-0000-000000000001");
        assert_eq!(courses[0].status, "ATTENDING");
        assert_eq!(
            courses[0].course_name,
            "Análise e Desenvolvimento de Sistemas"
        );
    }

    #[test]
    fn student_id_and_boleto_url_decode() {
        let student = fixture(include_str!("../../testdata/ava/StudentId.json"));
        let boleto = fixture(include_str!("../../testdata/ava/BoletoPdf.json"));

        assert_eq!(
            decode_student_id(&student, Sao_Paulo).expect("id"),
            "900000001"
        );
        assert_eq!(
            decode_boleto_url(&boleto, Sao_Paulo).expect("url"),
            "https://boleto.example.test/invoice-1.pdf"
        );
        let problem = decode_boleto_url(&json!({"data": {}}), Sao_Paulo).expect_err("problem");
        assert_eq!(problem.field, "url");
    }

    #[test]
    fn subjects_keep_attendance_and_pending_flags() {
        let subjects = subjects_fixture();

        let first = &subjects[0];
        assert_eq!(first.id, "subject-1");
        assert_eq!(first.name, "EXAMPLE SUBJECT ONE");
        assert!(first.in_progress);
        assert_eq!(first.attendance, Some((0, 20)));
        assert_eq!(first.assessments.len(), 3);

        let pending = &first.assessments[0];
        assert_eq!(pending.kind, AssessmentKind::Assignment);
        assert!(pending.available && !pending.done && !pending.attempted);
        assert_eq!(pending.name, "Trabalho");
        assert_eq!(pending.period_title, "Período de trabalho");
        assert_eq!(pending.period_end, Some(date(2026, 11, 30)));
        assert_eq!(pending.due_date, date(2026, 12, 20));

        let done = &first.assessments[1];
        assert!(done.done && done.attempted && !done.available);

        let exam = &first.assessments[2];
        assert_eq!(exam.kind, AssessmentKind::Exam);
        assert!(!exam.available && !exam.done);

        assert!(!subjects[1].in_progress);
        assert!(subjects[1].assessments.is_empty());
    }

    #[test]
    fn unknown_subject_status_is_skipped_not_a_problem() {
        let mut body = fixture(include_str!("../../testdata/ava/SubjectDeadlines.json"));
        body["data"]["subjectEnrollments"][1]["status"] = json!("SOMETHING_NEW");

        let subjects = decode_subjects(&body, Sao_Paulo).expect("subjects");

        assert_eq!(subjects.len(), 2);
        assert_eq!(subjects[1].id, "subject-3");
    }

    #[test]
    fn unknown_assessment_status_or_type_is_skipped() {
        let mut body = fixture(include_str!("../../testdata/ava/SubjectDeadlines.json"));
        body["data"]["subjectEnrollments"][0]["assignments"][0]["status"] = json!("SOMETHING_NEW");
        body["data"]["subjectEnrollments"][0]["assignments"][1]["detail"]["config"]["type"] =
            json!("SURVEY");

        let subjects = decode_subjects(&body, Sao_Paulo).expect("subjects");

        assert_eq!(subjects[0].assessments.len(), 1);
        assert_eq!(subjects[0].assessments[0].kind, AssessmentKind::Exam);
    }

    #[test]
    fn unparseable_period_dates_leave_period_end_empty() {
        let mut body = fixture(include_str!("../../testdata/ava/SubjectDeadlines.json"));
        body["data"]["subjectEnrollments"][0]["assignments"][0]["detail"]["display"]["periodDates"] =
            json!("em breve");

        let subjects = decode_subjects(&body, Sao_Paulo).expect("subjects");

        assert_eq!(subjects[0].assessments[0].period_end, None);
    }

    #[test]
    fn unparseable_assessment_due_date_is_a_problem() {
        let mut body = fixture(include_str!("../../testdata/ava/SubjectDeadlines.json"));
        body["data"]["subjectEnrollments"][0]["assignments"][0]["detail"]["dueDate"] =
            json!("soon");

        let problem = decode_subjects(&body, Sao_Paulo).expect_err("problem");

        assert_eq!(
            problem.field,
            "subjectEnrollments[].assignments[].detail.dueDate"
        );
        assert_eq!(
            problem.kind,
            ProblemKind::UnparseableDate("soon".to_owned())
        );
    }

    #[test]
    fn quizzes_keep_only_quiz_type() {
        let body = fixture(include_str!("../../testdata/ava/UnitAssignments.json"));

        let quizzes = decode_quizzes(&body, Sao_Paulo).expect("quizzes");

        assert_eq!(quizzes.len(), 2);
        assert_eq!(quizzes[0].id, "quiz-1");
        assert_eq!(quizzes[0].unit_title, "Unidade 1");
        assert_eq!(quizzes[0].name, "Exercício da Unidade 1");
        assert_eq!(quizzes[0].due_date, date(2026, 12, 20));
        assert!(!quizzes[0].attempted);
        assert!(quizzes[1].attempted);
    }

    #[test]
    fn calendar_skips_events_without_dates_and_keeps_canceled_flag() {
        let mut body = fixture(include_str!("../../testdata/ava/Calendar.json"));
        let events = body["data"].as_array_mut().expect("array");
        for key in ["eventDate", "startDate", "endDate"] {
            events[0][key] = Value::Null;
        }
        events[1].as_object_mut().expect("object").remove("title");
        events[2]["isCanceled"] = json!(true);

        let decoded = decode_calendar(&body, Sao_Paulo).expect("events");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].id, "event-3");
        assert!(decoded[0].canceled);
        assert_eq!(decoded[0].start, Some(date(2026, 8, 3)));
        assert_eq!(decoded[0].end, Some(date(2026, 12, 10)));
        assert_eq!(decoded[0].event_date, Some(date(2026, 12, 10)));
        assert!(!decoded[1].canceled);
    }

    #[test]
    fn calendar_that_is_not_an_array_is_a_problem() {
        let problem = decode_calendar(&json!({"data": {}}), Sao_Paulo).expect_err("problem");

        assert_eq!(problem.operation, "Calendar");
        assert_eq!(problem.kind, ProblemKind::WrongType);
    }

    #[test]
    fn problem_signature_drops_the_inner_text() {
        let problem = Problem {
            operation: "Invoices",
            field: "invoiceSections[].invoices[].status".to_owned(),
            kind: ProblemKind::UnknownValue("SECRET TEXT".to_owned()),
        };

        assert_eq!(
            problem.signature(),
            "Invoices:invoiceSections[].invoices[].status:unknown_value"
        );
    }
}
