pub mod support;

use std::sync::{Arc, Mutex};

use anhanguera_monitor::ava::model::{Problem, ProblemKind};
use anhanguera_monitor::ava::{AvaClient, AvaError, Credentials, Operation};
use anhanguera_monitor::portal::Portal;
use anhanguera_monitor::secret::Secret;
use anhanguera_monitor::timestamp::Clock;
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use chrono_tz::America::Sao_Paulo;
use serde_json::{Value, json};
use support::fake_ava::FakeAva;

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z")
        .expect("timestamp")
        .to_utc()
}

fn movable_clock() -> (Clock, Arc<Mutex<DateTime<Utc>>>) {
    let now = Arc::new(Mutex::new(start()));
    let reader = Arc::clone(&now);
    let clock: Clock = Arc::new(move || *reader.lock().expect("clock"));
    (clock, now)
}

fn advance(now: &Mutex<DateTime<Utc>>, by: TimeDelta) {
    let mut guard = now.lock().expect("clock");
    *guard += by;
}

fn portal_client(fake: &FakeAva, password: &str, clock: Clock) -> AvaClient {
    let credentials = Credentials {
        username: Secret::new("12345678909".to_owned()),
        password: Secret::new(password.to_owned()),
    };
    AvaClient::new(
        fake.url(),
        credentials,
        "device-1".to_owned(),
        clock,
        Sao_Paulo,
    )
    .expect("client")
}

fn count(fake: &FakeAva, operation: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|request| request.operation == operation)
        .count()
}

#[tokio::test]
async fn login_uses_the_public_basic_header_digits_and_device_id() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);

    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("execute");

    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].operation, "LoginStudent");
    assert_eq!(requests[0].authorization, "Basic cG9ydGFsOnNlY3JldA==");
    assert_eq!(requests[0].variables["username"], "12345678909");
    assert_eq!(requests[0].variables["deviceId"], "device-1");
    assert_eq!(requests[1].operation, "StudentId");
    assert_eq!(requests[1].authorization, "Bearer test-token");
}

#[tokio::test]
async fn token_is_reused_until_close_to_expiry() {
    let fake = FakeAva::start().await;
    let (clock, now) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);

    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("first");
    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("second");
    assert_eq!(count(&fake, "LoginStudent"), 1);

    advance(&now, TimeDelta::seconds(25200) - TimeDelta::minutes(4));
    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("third");
    assert_eq!(count(&fake, "LoginStudent"), 2);
}

#[tokio::test]
async fn expired_session_logs_in_again_and_retries_once() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("first");

    fake.expire_tokens();
    client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect("retried");
    assert_eq!(count(&fake, "LoginStudent"), 2);

    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    fake.set("StudentId", 401, json!({}));

    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("refused");
    assert!(
        matches!(&error, AvaError::Unavailable(text) if text == "the portal refused the session")
    );
    assert_eq!(count(&fake, "StudentId"), 2);
    assert_eq!(count(&fake, "LoginStudent"), 2);
}

#[tokio::test]
async fn rejected_password_backs_off_for_six_hours() {
    let fake = FakeAva::start().await;
    let (clock, now) = movable_clock();
    let client = portal_client(&fake, "wrong", clock);

    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("rejected");
    assert!(matches!(error, AvaError::LoginRejected));
    assert_eq!(
        client.login_backoff_until(),
        Some(start() + TimeDelta::hours(6))
    );
    assert_eq!(fake.requests().len(), 1);

    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("backoff");
    assert!(matches!(error, AvaError::LoginRejected));
    assert_eq!(fake.requests().len(), 1);

    advance(&now, TimeDelta::hours(6) + TimeDelta::minutes(1));
    assert_eq!(client.login_backoff_until(), None);
    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("rejected again");
    assert!(matches!(error, AvaError::LoginRejected));
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn graphql_errors_are_schema_problems() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    fake.set(
        "StudentId",
        200,
        json!({"errors": [{"message": "Cannot query field \"id\""}]}),
    );

    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("schema");
    assert!(matches!(
        error,
        AvaError::Schema(Problem {
            operation: "StudentId",
            kind: ProblemKind::GraphqlError(_),
            ..
        })
    ));
}

#[tokio::test]
async fn server_errors_are_unavailable_and_never_echo_the_password() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    fake.set(
        "LoginStudent",
        500,
        json!({"message": "test-password boom"}),
    );

    let error = client
        .execute(Operation::StudentId, Value::Null)
        .await
        .expect_err("unavailable");
    let AvaError::Unavailable(text) = error else {
        panic!("expected Unavailable");
    };
    assert!(!text.contains("test-password"));
}

#[tokio::test]
async fn portal_methods_decode_fake_answers() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    let today = NaiveDate::from_ymd_opt(2026, 11, 5).expect("date");

    assert_eq!(client.student_id().await.expect("student"), "900000001");
    let courses = client
        .course_enrollments("900000001")
        .await
        .expect("courses");
    assert_eq!(courses.len(), 2);
    let course = &courses[0].id;
    assert!(
        client
            .invoices(course)
            .await
            .expect("invoices")
            .invoices
            .is_empty()
    );
    assert!(
        client
            .boleto_url(course, "invoice-1")
            .await
            .expect("boleto")
            .ends_with(".pdf")
    );
    let subjects = client.subjects(course).await.expect("subjects");
    assert_eq!(subjects.len(), 3);
    assert_eq!(client.quizzes("subject-1").await.expect("quizzes").len(), 2);
    assert_eq!(
        client
            .calendar(course, today)
            .await
            .expect("calendar")
            .len(),
        4
    );
    assert_eq!(client.login_backoff_until(), None);

    let requests = fake.requests();
    let calendar = requests
        .iter()
        .find(|request| request.operation == "Calendar")
        .expect("calendar request");
    assert_eq!(calendar.variables["date"], "2026-11-05");
    assert_eq!(
        calendar.variables["eventCategories"],
        json!([
            "ACADEMIC_DATES",
            "UNIQUE_EVENTS",
            "SECRETARIAT",
            "FINANCIAL",
            "HOLIDAYS"
        ])
    );
}

#[tokio::test]
async fn a_malformed_answer_becomes_a_schema_error() {
    let fake = FakeAva::start().await;
    let (clock, _) = movable_clock();
    let client = portal_client(&fake, "test-password", clock);
    fake.set("StudentId", 200, json!({"data": {}}));

    let error = client.student_id().await.expect_err("error");

    assert!(matches!(error, AvaError::Schema(problem) if problem.field == "id"));
}
