pub mod support;

use std::path::Path;
use std::time::Duration;

use anhanguera_monitor::migration::{DROP_GOOSE_DB_VERSION, GOOSE_SCHEMA};
use monitor_hub_sdk::proto::{Event, EventsRequest, EventsResponse, WatchRequest, WatchResponse};
use serde_json::Value;
use support::{admin, database};
use tokio::time::{Instant, timeout_at};
use tonic::client::Grpc;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Streaming};
use tonic_prost::ProstCodec;

use support::fake_ava::FakeAva;
use support::process::{SocketDir, plugin_env, require_sh, spawn_plugin};
use support::schema::{goose_era_database, seed_rows};

const OPEN_INVOICES: &str = include_str!("../testdata/ava/Invoices.open.json");
const DELIVERY_LIMIT: Duration = Duration::from_secs(30);

async fn grpc(socket: &Path) -> Grpc<Channel> {
    let channel = Endpoint::from_shared(format!("unix:{}", socket.display()))
        .expect("a socket endpoint")
        .connect()
        .await
        .expect("connect to the plugin");
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
    let mut plugin = spawn_plugin(&plugin_env(&db.url, &ava.url(), &sockets));

    let mut stream = watch(&plugin.socket).await;
    let baseline = events(&plugin.socket, 0).await;
    let deadline = Instant::now() + DELIVERY_LIMIT;
    let delivered = events_once_invoiced(&plugin.socket, &mut stream, deadline).await;
    let exited = plugin.stop();

    assert!(baseline.is_empty(), "{baseline:?}");
    assert!(of_kind(&delivered, "welcome").is_some(), "{delivered:?}");
    let invoice = of_kind(&delivered, "invoice_open").expect("the invoice_open event");
    assert!(invoice.body.contains("R$ 123,45"), "{:?}", invoice.body);
    assert!(invoice.body.contains("Boleto: "), "{:?}", invoice.body);
    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_left_by_0_1_keeps_its_events_device_and_positions() {
    if !require_sh() {
        return;
    }
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2]).await;
    seed_rows(&client, true).await;
    let seeded: Vec<(i64, String)> = client
        .query("SELECT seq, id FROM events ORDER BY seq", &[])
        .await
        .expect("the seeded events")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let device = "SELECT device_id FROM monitor_state";
    let seeded_device: String = client
        .query_one(device, &[])
        .await
        .expect("the seeded device id")
        .get(0);
    let ava = FakeAva::quiet().await;
    let sockets = SocketDir::create();
    let mut plugin = spawn_plugin(&plugin_env(&db.url, &ava.url(), &sockets));

    let listed: Vec<(i64, String)> = events(&plugin.socket, 0)
        .await
        .into_iter()
        .map(|event| (event.seq, event.id))
        .collect();
    let exited = plugin.stop();

    assert_eq!(listed, seeded);
    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
    let device_id: String = client
        .query_one(device, &[])
        .await
        .expect("the device id")
        .get(0);
    assert_eq!(device_id, seeded_device);
    let versions: Vec<String> = client
        .query("SELECT version FROM seaql_migrations ORDER BY version", &[])
        .await
        .expect("the recorded migrations")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(versions, [GOOSE_SCHEMA, DROP_GOOSE_DB_VERSION]);
    let goose_gone: bool = client
        .query_one("SELECT to_regclass('goose_db_version') IS NULL", &[])
        .await
        .expect("look for goose_db_version")
        .get(0);
    assert!(goose_gone);
    let logins = ava
        .requests()
        .iter()
        .filter(|request| request.operation == "LoginStudent")
        .count();
    assert!(logins <= 1, "{logins} logins");
}
