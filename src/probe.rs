use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use chrono::NaiveDate;
use chrono_tz::Tz;
use futures_util::future::join_all;
use serde_json::{Map, Value, json};

use crate::ava::model::{
    CourseEnrollment, Problem, decode_boleto_url, decode_calendar, decode_course_enrollments,
    decode_invoices, decode_quizzes, decode_student_id, decode_subjects,
};
use crate::ava::{AvaClient, AvaError, Credentials, Operation};
use crate::config::{self, BillingDayNet, Config};
use crate::dates::local_today;
use crate::payments::learned_net_days;
use crate::portal::CALENDAR_CATEGORIES;
use crate::store::{Store, StoreError};
use crate::text::course_key;
use crate::timestamp::system_clock;

const ENUM_KEYS: [&str; 6] = [
    "status",
    "informationType",
    "type",
    "category",
    "subCategory",
    "tokenType",
];
const DATE_PREFIXES: [&str; 2] = ["dddd-dd-dd", "dd/dd/dddd"];
const OK: &str = "ok";
const ENV_PREFIX: &str = "ANHANGUERA__";
const NO_STATE: &str = "start the monitor once first";
const NO_DATABASE: &str = "cannot reach the database";

pub fn shape(value: &Value) -> Value {
    shape_under(value, None)
}

fn shape_under(value: &Value, key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, inner)| (name.clone(), shape_under(inner, Some(name))))
                .collect(),
        ),
        Value::Array(items) => match items.first() {
            None => json!([]),
            Some(first) => json!([shape_under(first, key), format!("len={}", items.len())]),
        },
        Value::String(text) => Value::from(text_shape(text, key)),
        Value::Number(_) => Value::from("number"),
        Value::Bool(_) => Value::from("boolean"),
        Value::Null => Value::Null,
    }
}

fn text_shape(text: &str, key: Option<&str>) -> String {
    let date_like = DATE_PREFIXES
        .iter()
        .any(|pattern| starts_like(text, pattern))
        && text.chars().all(is_date_char);
    if date_like {
        return text
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect();
    }
    if key.is_some_and(|name| ENUM_KEYS.contains(&name)) && looks_like_enum(text) {
        return text.to_owned();
    }
    "string".to_owned()
}

fn is_date_char(c: char) -> bool {
    c.is_ascii_digit() || "-/:.+TZ ".contains(c)
}

fn starts_like(text: &str, pattern: &str) -> bool {
    let mut chars = text.chars();
    pattern.chars().all(|expected| {
        let Some(found) = chars.next() else {
            return false;
        };
        match expected {
            'd' => found.is_ascii_digit(),
            literal => found == literal,
        }
    })
}

fn looks_like_enum(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

struct Call {
    operation: &'static str,
    shape: Option<Value>,
    verdict: String,
    unreachable: bool,
}

type Decoder<T> = fn(&Value, Tz) -> Result<T, Problem>;

async fn call<T>(
    client: &AvaClient,
    operation: Operation,
    variables: Value,
    decode: Decoder<T>,
) -> (Call, Option<T>) {
    let name = operation.name();
    let body = match client.execute(operation, variables).await {
        Ok(body) => body,
        Err(failure) => return (failed(name, &failure), None),
    };
    let shaped = Some(shape(&body));
    match decode(&body, client.tz()) {
        Ok(decoded) => (answered(name, shaped, OK.to_owned()), Some(decoded)),
        Err(problem) => (answered(name, shaped, problem.signature()), None),
    }
}

fn answered(operation: &'static str, shape: Option<Value>, verdict: String) -> Call {
    Call {
        operation,
        shape,
        verdict,
        unreachable: false,
    }
}

fn failed(operation: &'static str, failure: &AvaError) -> Call {
    let (verdict, unreachable) = match failure {
        AvaError::Schema(problem) => (problem.signature(), false),
        AvaError::LoginRejected => ("login_rejected".to_owned(), true),
        AvaError::Unavailable(_) => ("unavailable".to_owned(), true),
    };
    Call {
        operation,
        shape: None,
        verdict,
        unreachable,
    }
}

struct Survey {
    calls: Vec<Call>,
    billing: Vec<Value>,
}

async fn survey(client: &AvaClient, store: &Store, config: &Config) -> Result<Survey, String> {
    let (call, student_id) =
        self::call(client, Operation::StudentId, Value::Null, decode_student_id).await;
    if call.unreachable {
        return Err(format!(
            "cannot reach the portal or log in ({})",
            call.verdict
        ));
    }
    let mut calls = vec![call];
    let Some(student_id) = student_id else {
        return Ok(Survey {
            calls,
            billing: Vec::new(),
        });
    };
    let variables = json!({"studentId": student_id});
    let (call, enrollments) = self::call(
        client,
        Operation::CourseEnrollments,
        variables,
        decode_course_enrollments,
    )
    .await;
    calls.push(call);
    let enrollments = enrollments.unwrap_or_default();
    let wanted: HashSet<String> = config.courses.iter().map(|name| course_key(name)).collect();
    let today = local_today((system_clock())(), config.timezone);
    let chains = enrollments.iter().map(|enrollment| {
        let watched = wanted.contains(&course_key(&enrollment.course_name));
        enrollment_chains(
            client,
            store,
            config.billing_day_net,
            enrollment,
            watched,
            today,
        )
    });
    let mut billing = Vec::new();
    for chain in join_all(chains).await {
        let (found, entry) = chain?;
        calls.extend(found);
        billing.push(entry);
    }
    Ok(Survey { calls, billing })
}

async fn enrollment_chains(
    client: &AvaClient,
    store: &Store,
    net: BillingDayNet,
    enrollment: &CourseEnrollment,
    watched: bool,
    today: NaiveDate,
) -> Result<(Vec<Call>, Value), String> {
    let (payments, coursework, billing) = tokio::join!(
        payment_calls(client, &enrollment.id),
        coursework_calls(client, &enrollment.id, watched, today),
        billing_entry(store, net, &enrollment.id),
    );
    let calls = payments.into_iter().chain(coursework).collect();
    Ok((calls, billing?))
}

async fn payment_calls(client: &AvaClient, enrollment_id: &str) -> Vec<Call> {
    let variables = json!({"courseEnrollmentId": enrollment_id});
    let (invoices_call, home) = call(client, Operation::Invoices, variables, decode_invoices).await;
    let mut calls = vec![invoices_call];
    let Some(home) = home else {
        return calls;
    };
    let lookups = home
        .invoices
        .iter()
        .filter(|invoice| !invoice.status.is_settled())
        .map(|invoice| {
            let variables = json!({
                "courseEnrollmentId": enrollment_id,
                "invoiceId": invoice.id,
            });
            call(client, Operation::BoletoPdf, variables, decode_boleto_url)
        });
    calls.extend(join_all(lookups).await.into_iter().map(|(call, _)| call));
    calls
}

async fn coursework_calls(
    client: &AvaClient,
    enrollment_id: &str,
    watched: bool,
    today: NaiveDate,
) -> Vec<Call> {
    if !watched {
        return Vec::new();
    }
    let by_enrollment = json!({"courseEnrollmentId": enrollment_id});
    let calendar_variables = json!({
        "courseEnrollmentId": enrollment_id,
        "date": today.format("%Y-%m-%d").to_string(),
        "eventCategories": CALENDAR_CATEGORIES,
    });
    let ((subjects_call, subjects), (calendar_call, _)) = tokio::join!(
        call(
            client,
            Operation::SubjectDeadlines,
            by_enrollment,
            decode_subjects
        ),
        call(
            client,
            Operation::Calendar,
            calendar_variables,
            decode_calendar
        ),
    );
    let mut calls = vec![subjects_call, calendar_call];
    let lookups = subjects
        .unwrap_or_default()
        .into_iter()
        .filter(|subject| subject.in_progress)
        .map(|subject| {
            let variables = json!({"subjectEnrollmentId": subject.id});
            async move {
                call(
                    client,
                    Operation::UnitAssignments,
                    variables,
                    decode_quizzes,
                )
                .await
            }
        });
    calls.extend(join_all(lookups).await.into_iter().map(|(call, _)| call));
    calls
}

async fn billing_entry(
    store: &Store,
    net: BillingDayNet,
    enrollment_id: &str,
) -> Result<Value, String> {
    let (days, learned) = match net {
        BillingDayNet::Fixed(days) => (Some(days), false),
        BillingDayNet::Off => (None, false),
        BillingDayNet::Auto => {
            let lead_times = store
                .lead_times(enrollment_id)
                .await
                .map_err(|_| NO_DATABASE.to_owned())?;
            let (days, learned) = learned_net_days(&lead_times);
            (Some(days), learned)
        }
    };
    Ok(json!({"days": days, "learned": learned}))
}

fn operations(calls: Vec<Call>) -> Map<String, Value> {
    let mut merged: BTreeMap<&str, (Option<Value>, String)> = BTreeMap::new();
    for call in calls {
        let slot = merged
            .entry(call.operation)
            .or_insert_with(|| (call.shape, call.verdict.clone()));
        if slot.1 == OK {
            slot.1 = call.verdict;
        }
    }
    merged
        .into_iter()
        .map(|(name, (shape, verdict))| {
            (name.to_owned(), json!({"shape": shape, "verdict": verdict}))
        })
        .collect()
}

fn environment_lookup() -> impl Fn(&str) -> Option<OsString> {
    let under_hub = std::env::vars_os()
        .any(|(name, _)| name.as_encoded_bytes().starts_with(ENV_PREFIX.as_bytes()));
    let prefix = match under_hub {
        true => ENV_PREFIX,
        false => "",
    };
    move |name| std::env::var_os(format!("{prefix}{name}")).filter(|value| !value.is_empty())
}

fn load_config() -> Result<Config, String> {
    let document = config::document_from_env(environment_lookup()).map_err(|err| {
        let lines: Vec<String> = err
            .problems
            .iter()
            .map(|problem| format!("{}: {}", problem.path, problem.message))
            .collect();
        lines.join("\n")
    })?;
    Config::from_document(&document, &|path| std::fs::read_to_string(path))
        .map_err(|err| err.to_string())
}

fn refused_database(error: StoreError) -> String {
    let StoreError::InvalidUrl(reason) = error else {
        return NO_DATABASE.to_owned();
    };
    reason.to_owned()
}

async fn execute() -> Result<(Value, bool), String> {
    let config = load_config()?;
    let store = Store::new(config.database_url.reveal()).map_err(refused_database)?;
    let device_id = store
        .device_id()
        .await
        .map_err(|_| NO_DATABASE.to_owned())?
        .ok_or_else(|| NO_STATE.to_owned())?;
    let credentials = Credentials {
        username: config.username.clone(),
        password: config.password.clone(),
    };
    let client = AvaClient::new(
        config.graphql_url.clone(),
        credentials,
        device_id,
        system_clock(),
        config.timezone,
    )
    .map_err(|err| err.to_string())?;
    let Survey { calls, billing } = survey(&client, &store, &config).await?;
    let operations = operations(calls);
    let all_ok = operations.values().all(|entry| entry["verdict"] == OK);
    Ok((
        json!({"operations": operations, "billing_day_net": billing}),
        all_ok,
    ))
}

pub fn run() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let Ok(runtime) = runtime else {
        eprintln!("cannot start the async runtime");
        return ExitCode::FAILURE;
    };
    let (document, all_ok) = match runtime.block_on(execute()) {
        Ok(result) => result,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    if writeln!(std::io::stdout(), "{document}").is_err() {
        return ExitCode::FAILURE;
    }
    if all_ok {
        return ExitCode::SUCCESS;
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OPEN_INVOICES: &str = include_str!("../testdata/ava/Invoices.open.json");
    const ENROLLMENTS: &str = include_str!("../testdata/ava/CourseEnrollments.json");

    fn shown(raw: &str) -> String {
        let value: Value = serde_json::from_str(raw).expect("the fixture is JSON");
        shape(&value).to_string()
    }

    #[test]
    fn shape_hides_every_value() {
        let invoices = shown(OPEN_INVOICES);
        let enrollments = shown(ENROLLMENTS);
        for hidden in [
            "invoice-1",
            "Mensalidade",
            "Dezembro",
            "123,45",
            "Pague",
            "2026",
        ] {
            assert!(!invoices.contains(hidden), "{hidden} leaked: {invoices}");
        }
        for hidden in ["KRT-", "Análise", "Example Other Course"] {
            assert!(
                !enrollments.contains(hidden),
                "{hidden} leaked: {enrollments}"
            );
        }
        assert!(invoices.contains("\"OPEN\""));
        assert!(invoices.contains("\"9999-99-99\""));
        assert!(invoices.contains("\"len=1\""));
        assert!(enrollments.contains("\"len=2\""));
    }

    #[test]
    fn shape_keeps_enum_looking_text_only_under_known_enum_keys() {
        let shaped = shape(&json!({
            "name": "MATEMATICA",
            "status": "IN_PROGRESS",
            "tags": ["ABC"],
            "category": ["ACADEMIC_DATES"],
        }));
        assert_eq!(shaped["name"], "string");
        assert_eq!(shaped["status"], "IN_PROGRESS");
        assert_eq!(shaped["tags"], json!(["string", "len=1"]));
        assert_eq!(shaped["category"], json!(["ACADEMIC_DATES", "len=1"]));
    }

    #[test]
    fn shape_masks_dates_under_any_key_and_types_the_rest() {
        let shaped = shape(&json!({
            "when": "2026-12-10",
            "slashed": "10/12/2026 08:30",
            "count": 3,
            "flag": true,
            "nothing": null,
            "empty": [],
        }));
        assert_eq!(shaped["when"], "9999-99-99");
        assert_eq!(shaped["slashed"], "99/99/9999 99:99");
        assert_eq!(shaped["count"], "number");
        assert_eq!(shaped["flag"], "boolean");
        assert_eq!(shaped["nothing"], Value::Null);
        assert_eq!(shaped["empty"], json!([]));
    }

    #[test]
    fn shape_masks_whole_dates_and_periods() {
        let shaped = shape(&json!({
            "a": "2026-12-10T02:59:59Z",
            "b": "2026-12-09T23:59:59-03:00",
            "c": "2026-12-10T12:00:00.000Z",
            "periodDates": "03/11/2026 - 12/12/2026",
        }));
        assert_eq!(shaped["a"], "9999-99-99T99:99:99Z");
        assert_eq!(shaped["b"], "9999-99-99T99:99:99-99:99");
        assert_eq!(shaped["c"], "9999-99-99T99:99:99.999Z");
        assert_eq!(shaped["periodDates"], "99/99/9999 - 99/99/9999");
    }

    #[test]
    fn shape_hides_text_that_only_starts_with_a_date() {
        let shaped = shape(&json!({
            "a": "10/12/2026 Prova de Joao",
            "b": "2026-12-10abc-secret",
            "c": "2026-12-10 Joao Silva",
        }));
        assert_eq!(shaped["a"], "string");
        assert_eq!(shaped["b"], "string");
        assert_eq!(shaped["c"], "string");
    }
}
