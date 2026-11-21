pub mod support;

use anhanguera_monitor::migration::{DROP_GOOSE_DB_VERSION, GOOSE_SCHEMA, Migrator};
use futures_util::future::join_all;
use sea_orm_migration::MigratorTrait;
use tokio_postgres::{Client, NoTls};

use support::schema::{
    apply_up_sections, goose_era_database, rows, schema_snapshot, seed_rows, table_names,
};
use support::{database, orm};

const TODAYS_TABLES: [&str; 7] = [
    "enrollments",
    "events",
    "invoices",
    "monitor_state",
    "problems",
    "seaql_migrations",
    "sent_keys",
];
const KEPT_TABLES: [&str; 6] = [
    "enrollments",
    "events",
    "invoices",
    "monitor_state",
    "problems",
    "sent_keys",
];
const SEAQL_VERSIONS: &str = "SELECT version FROM seaql_migrations ORDER BY version";
const SEAQL_COUNT: &str = "SELECT count(*)::text FROM seaql_migrations";
const GOOSE_VERSIONS: &str = "SELECT version_id::text FROM goose_db_version ORDER BY id";
const SEQUENCE_STATE: &str = "SELECT format('%s %s', last_value, is_called) FROM events_seq_seq";
const INVOICES_KEY: &str = "SELECT pg_get_constraintdef(oid) FROM pg_constraint
WHERE conrelid = 'invoices'::regclass AND contype = 'p'";

async fn admin(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the admin client");
    tokio::spawn(connection);
    client
}

async fn texts(client: &Client, query: &str) -> Vec<String> {
    client
        .query(query, &[])
        .await
        .unwrap_or_else(|err| panic!("{query}: {err:?}"))
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn kept_data(client: &Client) -> (Vec<Vec<String>>, Vec<String>) {
    tokio::join!(
        join_all(KEPT_TABLES.iter().map(|table| rows(client, table))),
        texts(client, SEQUENCE_STATE)
    )
}

fn with_seaql_migrations(mut tables: Vec<String>) -> Vec<String> {
    tables.push("seaql_migrations".to_owned());
    tables.sort();
    tables
}

#[tokio::test]
async fn a_new_database_gets_todays_tables() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(
        texts(&client, SEAQL_VERSIONS).await,
        [GOOSE_SCHEMA, DROP_GOOSE_DB_VERSION]
    );
    assert_eq!(
        texts(&client, INVOICES_KEY).await,
        ["PRIMARY KEY (course_enrollment_id, id)"]
    );
}

#[tokio::test]
async fn the_first_migration_builds_what_the_goose_files_build() {
    let (Some(migrated), Some(by_hand)) = (database().await, database().await) else {
        return;
    };
    let (migrated_client, by_hand_client) = tokio::join!(admin(&migrated.url), admin(&by_hand.url));

    Migrator::up(&orm(&migrated.url).await, Some(1))
        .await
        .expect("apply the first migration");
    apply_up_sections(&by_hand_client, &[1, 2]).await;

    assert_eq!(
        schema_snapshot(&migrated_client, &["seaql_migrations"]).await,
        schema_snapshot(&by_hand_client, &[]).await
    );
}

#[tokio::test]
async fn a_database_at_goose_version_2_is_adopted_without_loss() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2]).await;
    seed_rows(&client, true).await;
    let before = kept_data(&client).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(kept_data(&client).await, before);
    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(
        texts(&client, SEAQL_VERSIONS).await,
        [GOOSE_SCHEMA, DROP_GOOSE_DB_VERSION]
    );
}

#[tokio::test]
async fn a_database_at_goose_version_1_is_caught_up_and_ends_like_a_new_one() {
    let (Some(db), Some(new)) = (database().await, database().await) else {
        return;
    };
    let (client, new_client) = tokio::join!(admin(&db.url), admin(&new.url));
    goose_era_database(&client, &[1], &[1]).await;
    seed_rows(&client, false).await;
    let before = kept_data(&client).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");
    Migrator::up(&orm(&new.url).await, None)
        .await
        .expect("migrate the new database");

    assert_eq!(kept_data(&client).await, before);
    assert_eq!(
        schema_snapshot(&client, &[]).await,
        schema_snapshot(&new_client, &[]).await
    );
}

#[tokio::test]
async fn a_goose_version_above_2_is_adopted() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2, 3]).await;
    seed_rows(&client, true).await;
    let events_before = rows(&client, "events").await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(rows(&client, "events").await, events_before);
}

#[tokio::test]
async fn a_gap_in_goose_db_version_is_refused_and_nothing_is_dropped() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[2]).await;
    seed_rows(&client, true).await;
    let tables_before = table_names(&client).await;
    let goose_before = texts(&client, GOOSE_VERSIONS).await;
    let data_before = kept_data(&client).await;

    let refused = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("a gap in goose_db_version");

    assert!(
        refused.to_string().contains(
            "migration version 1 is not applied but the database is already at version 2"
        ),
        "{refused}"
    );
    assert_eq!(
        table_names(&client).await,
        with_seaql_migrations(tables_before)
    );
    assert_eq!(texts(&client, SEAQL_COUNT).await, ["0"]);
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, goose_before);
    assert_eq!(kept_data(&client).await, data_before);
}

#[tokio::test]
async fn an_empty_goose_db_version_is_refused_and_nothing_is_dropped() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[]).await;
    client
        .batch_execute("DELETE FROM goose_db_version")
        .await
        .expect("empty goose_db_version");
    seed_rows(&client, true).await;
    let tables_before = table_names(&client).await;
    let data_before = kept_data(&client).await;

    let refused = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("an empty goose_db_version");

    assert!(
        refused
            .to_string()
            .contains("goose_db_version has no rows, not even the version 0 row goose expects"),
        "{refused}"
    );
    assert_eq!(
        table_names(&client).await,
        with_seaql_migrations(tables_before)
    );
    assert_eq!(texts(&client, SEAQL_COUNT).await, ["0"]);
    assert_eq!(kept_data(&client).await, data_before);
}

#[tokio::test]
async fn a_failed_catch_up_applies_nothing_and_drops_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1], &[1]).await;
    seed_rows(&client, false).await;
    client
        .batch_execute("ALTER TABLE invoices DROP CONSTRAINT invoices_pkey")
        .await
        .expect("drop the invoices key by hand");
    let data_before = kept_data(&client).await;

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("version 2 needs the invoices key");

    assert!(
        failed
            .to_string()
            .contains(r#"constraint "invoices_pkey" of relation "invoices" does not exist"#),
        "{failed}"
    );
    assert!(texts(&client, INVOICES_KEY).await.is_empty());
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, ["0", "1"]);
    assert!(
        table_names(&client)
            .await
            .contains(&"goose_db_version".to_owned())
    );
    assert_eq!(texts(&client, SEAQL_COUNT).await, ["0"]);
    assert_eq!(kept_data(&client).await, data_before);
}

#[tokio::test]
async fn a_new_database_whose_catch_up_fails_keeps_none_of_it() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    client
        .batch_execute(
            "CREATE FUNCTION refuse_alter_table() RETURNS event_trigger LANGUAGE plpgsql AS $$
            BEGIN
                RAISE EXCEPTION 'ALTER TABLE refused by the test';
            END
            $$;
            CREATE EVENT TRIGGER refuse_alter_table ON ddl_command_start
                WHEN TAG IN ('ALTER TABLE') EXECUTE FUNCTION refuse_alter_table();",
        )
        .await
        .expect("create the event trigger");

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("the event trigger refuses version 2");

    assert!(
        failed
            .to_string()
            .contains("ALTER TABLE refused by the test"),
        "{failed}"
    );
    assert_eq!(table_names(&client).await, ["seaql_migrations"]);
    assert_eq!(texts(&client, SEAQL_COUNT).await, ["0"]);
}

#[tokio::test]
async fn an_object_that_depends_on_goose_db_version_stops_the_drop() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2]).await;
    seed_rows(&client, true).await;
    client
        .batch_execute("CREATE VIEW goose_seen AS SELECT * FROM goose_db_version")
        .await
        .expect("create the view");
    let events_before = rows(&client, "events").await;
    let goose_before = texts(&client, GOOSE_VERSIONS).await;

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("the view stops the drop");

    let message = failed.to_string();
    assert!(
        message.contains("goose_db_version")
            && message.contains("because other objects depend on it"),
        "{message}"
    );
    assert_eq!(texts(&client, SEAQL_VERSIONS).await, [GOOSE_SCHEMA]);
    assert!(
        table_names(&client)
            .await
            .contains(&"goose_db_version".to_owned())
    );
    assert_eq!(
        texts(
            &client,
            "SELECT viewname::text FROM pg_views WHERE schemaname = current_schema()"
        )
        .await,
        ["goose_seen"]
    );
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, goose_before);
    assert_eq!(rows(&client, "events").await, events_before);
}

#[tokio::test]
async fn running_the_migrations_again_changes_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    let orm = orm(&db.url).await;
    let recorded =
        "SELECT format('%s %s', version, applied_at) FROM seaql_migrations ORDER BY version";
    Migrator::up(&orm, None).await.expect("migrate");
    let before = texts(&client, recorded).await;

    Migrator::up(&orm, None).await.expect("migrate again");

    assert!(
        Migrator::get_pending_migrations(&orm)
            .await
            .expect("pending migrations")
            .is_empty()
    );
    assert_eq!(texts(&client, recorded).await, before);
}

#[tokio::test]
async fn goose_db_version_in_another_schema_on_the_search_path_is_left_alone() {
    let Some(db) = database().await else {
        return;
    };
    let setup = admin(&db.url).await;
    goose_era_database(&setup, &[], &[1, 2]).await;
    setup
        .batch_execute("CREATE SCHEMA anhanguera")
        .await
        .expect("create the anhanguera schema");
    let name = texts(&setup, "SELECT current_database()::text").await;
    setup
        .batch_execute(&format!(
            "ALTER DATABASE \"{}\" SET search_path = anhanguera, public",
            name[0]
        ))
        .await
        .expect("put anhanguera first on the search path");
    let client = admin(&db.url).await;
    let goose_before = rows(&client, "public.goose_db_version").await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(
        texts(&client, "SELECT current_schema()::text").await,
        ["anhanguera"]
    );
    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(rows(&client, "public.goose_db_version").await, goose_before);
}
