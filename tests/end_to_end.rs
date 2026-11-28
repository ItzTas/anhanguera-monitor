pub mod support;

use std::path::Path;
use std::time::Duration;

use monitor_hub_sdk::proto::{
    Event, EventsRequest, EventsResponse, StatusRequest, StatusResponse, WatchRequest,
    WatchResponse,
};
use serde_json::Value;
use support::database;
use tokio::time::{Instant, timeout_at};
use tonic::client::Grpc;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Streaming};
use tonic_prost::ProstCodec;

use support::fake_ava::FakeAva;
use support::process::{
    SocketDir, monitor_document, monitor_env, process_env, require_sh, spawn_monitor,
    spawn_monitor_with_document,
};

const OPEN_INVOICES: &str = include_str!("../testdata/ava/Invoices.open.json");
const DELIVERY_LIMIT: Duration = Duration::from_secs(30);

async fn grpc(socket: &Path) -> Grpc<Channel> {
    let channel = Endpoint::from_shared(format!("unix:{}", socket.display()))
        .expect("a socket endpoint")
        .connect()
        .await
        .expect("connect to the monitor");
    let mut grpc = Grpc::new(channel);
    grpc.ready().await.expect("the channel is ready");
    grpc
}

async fn events(socket: &Path, after: i64) -> Vec<Event> {
    grpc(socket)
        .await
        .unary(
            Request::new(EventsRequest { after, limit: 100 }),
            PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Events"),
            ProstCodec::<EventsRequest, EventsResponse>::default(),
        )
        .await
        .expect("events")
        .into_inner()
        .events
}

async fn watch(socket: &Path) -> Streaming<WatchResponse> {
    grpc(socket)
        .await
        .server_streaming(
            Request::new(WatchRequest {}),
            PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Watch"),
            ProstCodec::<WatchRequest, WatchResponse>::default(),
        )
        .await
        .expect("open Watch")
        .into_inner()
}

async fn next_watch_signal(stream: &mut Streaming<WatchResponse>, deadline: Instant) -> i64 {
    timeout_at(deadline, stream.message())
        .await
        .expect("a Watch signal in time")
        .expect("an OK Watch signal")
        .expect("the Watch call stays open")
        .latest_seq
}

async fn first_cycle_finished(socket: &Path, deadline: Instant) {
    loop {
        let status = grpc(socket)
            .await
            .unary(
                Request::new(StatusRequest {}),
                PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Status"),
                ProstCodec::<StatusRequest, StatusResponse>::default(),
            )
            .await
            .expect("status")
            .into_inner();
        if status.next_check_at.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "the first cycle did not finish");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn of_kind<'a>(events: &'a [Event], kind: &str) -> Option<&'a Event> {
    events.iter().find(|event| event.kind == kind)
}

async fn events_once_invoiced(
    socket: &Path,
    stream: &mut Streaming<WatchResponse>,
    deadline: Instant,
) -> Vec<Event> {
    loop {
        if next_watch_signal(stream, deadline).await < 2 {
            continue;
        }
        let listed = events(socket, 0).await;
        if of_kind(&listed, "invoice_open").is_some() {
            return listed;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_invoice_reaches_the_hub_through_watch_and_events() {
    if !require_sh() {
        return;
    }
    let Some(db) = database().await else {
        return;
    };
    let ava = FakeAva::start().await;
    let open: Value = serde_json::from_str(OPEN_INVOICES).expect("the fixture is JSON");
    ava.set("Invoices", 200, open);
    let sockets = SocketDir::create();
    let mut spawned = spawn_monitor(&monitor_env(&db.url, &ava.url(), &sockets));

    let mut stream = watch(&spawned.socket).await;
    let baseline = events(&spawned.socket, 0).await;
    let deadline = Instant::now() + DELIVERY_LIMIT;
    let delivered = events_once_invoiced(&spawned.socket, &mut stream, deadline).await;
    let exited = spawned.stop();

    assert!(baseline.is_empty(), "{baseline:?}");
    assert!(of_kind(&delivered, "welcome").is_some(), "{delivered:?}");
    let invoice = of_kind(&delivered, "invoice_open").expect("the invoice_open event");
    assert!(invoice.body.contains("R$ 123,45"), "{:?}", invoice.body);
    assert!(invoice.body.contains("Boleto: "), "{:?}", invoice.body);
    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_on_descriptor_3_starts_the_monitor_with_one_portal_login() {
    if !require_sh() {
        return;
    }
    let Some(db) = database().await else {
        return;
    };
    let ava = FakeAva::quiet().await;
    let sockets = SocketDir::create();
    let mut env = process_env(&sockets);
    env.extend(
        [
            (
                "DATABASE_URL",
                "postgres://anhanguera:decoy@127.0.0.1:9/anhanguera",
            ),
            ("AVA_PASSWORD", "wrong-password"),
            ("AVA_GRAPHQL_URL", "http://127.0.0.1:9/"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    let mut spawned = spawn_monitor_with_document(&env, &monitor_document(&db.url, &ava.url()));

    first_cycle_finished(&spawned.socket, Instant::now() + DELIVERY_LIMIT).await;
    let exited = spawned.stop();

    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
    let requests = ava.requests();
    let count = |operation: &str| {
        requests
            .iter()
            .filter(|request| request.operation == operation)
            .count()
    };
    assert_eq!(count("LoginStudent"), 1);
    assert!(count("StudentId") >= 1);
}
