pub mod fake_ava;

use anhanguera_monitor::store::MIGRATIONS;
use monitor_hub_plugin::migrate::up;
use monitor_hub_plugin_testkit::{Database, database};
use tokio_postgres::NoTls;

pub async fn migrated_database() -> Option<Database> {
    let database = database().await?;
    let (mut client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("connect to the test database");
    let driver = tokio::spawn(connection);
    up(&mut client, MIGRATIONS)
        .await
        .expect("migrate the test database");
    drop(client);
    driver
        .await
        .expect("join the migration connection")
        .expect("close the migration connection");
    Some(database)
}
