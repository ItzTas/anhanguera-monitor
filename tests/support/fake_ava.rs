use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TESTDATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/ava");
const LOGIN: &str = "LoginStudent";
const ACCEPTED_PASSWORD: &str = "test-password";

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub operation: String,
    pub authorization: String,
    pub variables: Value,
}

pub struct FakeAva {
    portal: Arc<Portal>,
    server: JoinHandle<()>,
}

struct Portal {
    url: String,
    answers: Mutex<HashMap<String, (u16, String)>>,
    requests: Mutex<Vec<RecordedRequest>>,
    expire: AtomicBool,
}

impl FakeAva {
    pub async fn start() -> FakeAva {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake portal");
        let address = listener.local_addr().expect("fake portal address");
        let portal = Arc::new(Portal {
            url: format!("http://{address}/"),
            answers: Mutex::new(HashMap::new()),
            requests: Mutex::new(Vec::new()),
            expire: AtomicBool::new(false),
        });
        let router = Router::new()
            .route("/", post(handle))
            .with_state(Arc::clone(&portal));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve the fake portal");
        });
        FakeAva { portal, server }
    }

    pub async fn quiet() -> FakeAva {
        let ava = FakeAva::start().await;
        ava.set(
            "Invoices",
            200,
            json!({"data": {"getInvoiceHomeModel": {
                "informationType": "PAYMENTS_ON_TIME",
                "billingDay": "",
                "invoiceSections": []
            }}}),
        );
        ava.set(
            "SubjectDeadlines",
            200,
            json!({"data": {"data": {
                "id": "KRT-00000000-0000-0000-0000-000000000001",
                "subjectEnrollments": []
            }}}),
        );
        ava.set("Calendar", 200, json!({"data": {"data": []}}));
        ava
    }

    pub fn url(&self) -> String {
        self.portal.url.clone()
    }

    pub fn set(&self, operation: &str, status: u16, body: Value) {
        self.set_text(operation, status, &body.to_string());
    }

    pub fn set_text(&self, operation: &str, status: u16, body: &str) {
        lock(&self.portal.answers).insert(operation.to_owned(), (status, body.to_owned()));
    }

    pub fn expire_tokens(&self) {
        self.portal.expire.store(true, Ordering::SeqCst);
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.portal.requests).clone()
    }
}

impl Drop for FakeAva {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn handle(State(portal): State<Arc<Portal>>, headers: HeaderMap, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let operation = request["operationName"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let variables = request["variables"].clone();
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    lock(&portal.requests).push(RecordedRequest {
        operation: operation.clone(),
        authorization,
        variables: variables.clone(),
    });
    if operation != LOGIN && portal.expire.swap(false, Ordering::SeqCst) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let answer = lock(&portal.answers).get(&operation).cloned();
    if let Some((status, body)) = answer {
        return reply(status, body);
    }
    if operation == LOGIN && variables["password"] != ACCEPTED_PASSWORD {
        return reply(
            200,
            json!({"errors": [{"message": "Invalid credentials"}]}).to_string(),
        );
    }
    match fs::read_to_string(format!("{TESTDATA}/{operation}.json")) {
        Ok(text) => reply(200, text),
        Err(_) => reply(
            404,
            json!({"errors": [{"message": "no fixture"}]}).to_string(),
        ),
    }
}

fn reply(status: u16, body: String) -> Response {
    let status = StatusCode::from_u16(status).expect("valid status");
    (status, [("content-type", "application/json")], body).into_response()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
