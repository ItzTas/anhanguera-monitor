pub mod model;

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use reqwest::{Certificate, StatusCode};
use serde_json::{Value, json};

use crate::secret::Secret;
use crate::timestamp::Clock;
use model::{Problem, ProblemKind};

const BASIC_AUTHORIZATION: &str = "Basic cG9ydGFsOnNlY3JldA==";
const ORIGIN: &str = "https://www.anhanguera.com";
const REFERER: &str = "https://www.anhanguera.com/";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const REFUSED: &str = "the portal refused the session";
const NOT_JSON: &str = "the portal answered something that is not JSON";

fn login_backoff() -> TimeDelta {
    TimeDelta::hours(6)
}

fn expiry_margin() -> TimeDelta {
    TimeDelta::minutes(5)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    LoginStudent,
    StudentId,
    CourseEnrollments,
    SubjectDeadlines,
    UnitAssignments,
    Calendar,
    Invoices,
    BoletoPdf,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Operation::LoginStudent => "LoginStudent",
            Operation::StudentId => "StudentId",
            Operation::CourseEnrollments => "CourseEnrollments",
            Operation::SubjectDeadlines => "SubjectDeadlines",
            Operation::UnitAssignments => "UnitAssignments",
            Operation::Calendar => "Calendar",
            Operation::Invoices => "Invoices",
            Operation::BoletoPdf => "BoletoPdf",
        }
    }

    pub fn document(self) -> &'static str {
        match self {
            Operation::LoginStudent => include_str!("../queries/LoginStudent.graphql"),
            Operation::StudentId => include_str!("../queries/StudentId.graphql"),
            Operation::CourseEnrollments => include_str!("../queries/CourseEnrollments.graphql"),
            Operation::SubjectDeadlines => include_str!("../queries/SubjectDeadlines.graphql"),
            Operation::UnitAssignments => include_str!("../queries/UnitAssignments.graphql"),
            Operation::Calendar => include_str!("../queries/Calendar.graphql"),
            Operation::Invoices => include_str!("../queries/Invoices.graphql"),
            Operation::BoletoPdf => include_str!("../queries/BoletoPdf.graphql"),
        }
    }
}

pub struct Credentials {
    pub username: Secret,
    pub password: Secret,
}

#[derive(Debug, thiserror::Error)]
pub enum AvaError {
    #[error("portal unavailable: {0}")]
    Unavailable(String),
    #[error("the portal rejected the login")]
    LoginRejected,
    #[error("unexpected portal answer: {}", .0.signature())]
    Schema(Problem),
}

struct Token {
    value: Secret,
    expires_at: DateTime<Utc>,
}

pub struct AvaClient {
    http: reqwest::Client,
    url: String,
    credentials: Credentials,
    device_id: String,
    clock: Clock,
    session: tokio::sync::Mutex<Option<Token>>,
    rejected_at: Mutex<Option<DateTime<Utc>>>,
}

impl AvaClient {
    pub fn new(
        url: String,
        credentials: Credentials,
        device_id: String,
        clock: Clock,
    ) -> Result<AvaClient, AvaError> {
        let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .map(|root| Certificate::from_der(root))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                AvaError::Unavailable("the trusted certificates are unusable".to_owned())
            })?;
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .gzip(true)
            .tls_certs_only(roots)
            .build()
            .map_err(|_| AvaError::Unavailable("the HTTP client could not be built".to_owned()))?;
        Ok(AvaClient {
            http,
            url,
            credentials,
            device_id,
            clock,
            session: tokio::sync::Mutex::new(None),
            rejected_at: Mutex::new(None),
        })
    }

    pub fn login_backoff_until(&self) -> Option<DateTime<Utc>> {
        let now = (self.clock)();
        let rejected_at = *self.rejected_at.lock().unwrap_or_else(|e| e.into_inner());
        rejected_at
            .map(|at| at + login_backoff())
            .filter(|until| now < *until)
    }

    pub async fn execute(&self, operation: Operation, variables: Value) -> Result<Value, AvaError> {
        if let Some(body) = self.attempt(operation, &variables).await? {
            return Ok(body);
        }
        *self.session.lock().await = None;
        self.attempt(operation, &variables)
            .await?
            .ok_or_else(|| AvaError::Unavailable(REFUSED.to_owned()))
    }

    async fn attempt(
        &self,
        operation: Operation,
        variables: &Value,
    ) -> Result<Option<Value>, AvaError> {
        let token = self.token().await?;
        let authorization = format!("Bearer {}", token.reveal());
        let (status, body) = self.send(operation, variables, &authorization).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(AvaError::Unavailable(format!(
                "the portal answered HTTP {}",
                status.as_u16()
            )));
        }
        let body = body.ok_or_else(|| AvaError::Unavailable(NOT_JSON.to_owned()))?;
        let Some(errors) = graphql_errors(&body) else {
            return Ok(Some(body));
        };
        if errors.iter().any(is_authentication_error) {
            return Ok(None);
        }
        Err(AvaError::Schema(Problem {
            operation: operation.name(),
            field: "errors".to_owned(),
            kind: ProblemKind::GraphqlError(first_message(errors)),
        }))
    }

    async fn token(&self) -> Result<Secret, AvaError> {
        let mut session = self.session.lock().await;
        let now = (self.clock)();
        if let Some(token) = session
            .as_ref()
            .filter(|token| token.expires_at - expiry_margin() > now)
        {
            return Ok(token.value.clone());
        }
        let token = self.login().await?;
        let value = token.value.clone();
        *session = Some(token);
        Ok(value)
    }

    async fn login(&self) -> Result<Token, AvaError> {
        if self.login_backoff_until().is_some() {
            return Err(AvaError::LoginRejected);
        }
        let variables = json!({
            "username": self.credentials.username.reveal(),
            "password": self.credentials.password.reveal(),
            "deviceId": self.device_id,
        });
        let (status, body) = self
            .send(Operation::LoginStudent, &variables, BASIC_AUTHORIZATION)
            .await?;
        if !status.is_success() {
            return Err(self.reject());
        }
        let body = body.ok_or_else(|| AvaError::Unavailable(NOT_JSON.to_owned()))?;
        if graphql_errors(&body).is_some() {
            return Err(self.reject());
        }
        let login = &body["data"];
        let missing = |field: &str| {
            AvaError::Schema(Problem {
                operation: Operation::LoginStudent.name(),
                field: field.to_owned(),
                kind: ProblemKind::Missing,
            })
        };
        let access_token = login["accessToken"]
            .as_str()
            .ok_or_else(|| missing("accessToken"))?;
        let expires_in = login["expiresIn"]
            .as_i64()
            .ok_or_else(|| missing("expiresIn"))?;
        Ok(Token {
            value: Secret::new(access_token.to_owned()),
            expires_at: (self.clock)() + TimeDelta::seconds(expires_in),
        })
    }

    fn reject(&self) -> AvaError {
        *self.rejected_at.lock().unwrap_or_else(|e| e.into_inner()) = Some((self.clock)());
        AvaError::LoginRejected
    }

    async fn send(
        &self,
        operation: Operation,
        variables: &Value,
        authorization: &str,
    ) -> Result<(StatusCode, Option<Value>), AvaError> {
        let payload = json!({
            "operationName": operation.name(),
            "query": operation.document(),
            "variables": variables,
        });
        let unreachable = |error: reqwest::Error| {
            let reason = match error.is_timeout() {
                true => "the portal timed out",
                false => "the portal could not be reached",
            };
            AvaError::Unavailable(reason.to_owned())
        };
        let response = self
            .http
            .post(&self.url)
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .header("origin", ORIGIN)
            .header("referer", REFERER)
            .body(payload.to_string())
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        if status.is_server_error() {
            return Err(AvaError::Unavailable(format!(
                "the portal answered HTTP {}",
                status.as_u16()
            )));
        }
        let bytes = response.bytes().await.map_err(unreachable)?;
        Ok((status, serde_json::from_slice(&bytes).ok()))
    }
}

fn graphql_errors(body: &Value) -> Option<&Vec<Value>> {
    body.get("errors")?
        .as_array()
        .filter(|errors| !errors.is_empty())
}

fn is_authentication_error(error: &Value) -> bool {
    matches!(
        error["extensions"]["code"].as_str(),
        Some("UNAUTHENTICATED" | "UNAUTHORIZED")
    )
}

fn first_message(errors: &[Value]) -> String {
    errors
        .first()
        .and_then(|error| error["message"].as_str())
        .unwrap_or_default()
        .to_owned()
}
