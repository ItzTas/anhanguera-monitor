pub mod support;

use anhanguera_monitor::migration::{INIT, Migrator};
use sea_orm_migration::MigratorTrait;
use tokio_postgres::{Client, NoTls};

use support::schema::table_names;
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
const SEAQL_VERSIONS: &str = "SELECT version FROM seaql_migrations ORDER BY version";
const SEAQL_COUNT: &str = "SELECT count(*)::text FROM seaql_migrations";
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
    assert_eq!(texts(&client, SEAQL_VERSIONS).await, [INIT]);
    assert_eq!(
        texts(&client, INVOICES_KEY).await,
        ["PRIMARY KEY (course_enrollment_id, id)"]
    );
}

#[tokio::test]
async fn a_failed_migration_keeps_none_of_its_tables() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    // sea-orm-migration's own table, whose record insert runs after the tables are created, in
    // the same transaction.
    client
        .batch_execute(
            "CREATE TABLE seaql_migrations (
                version varchar PRIMARY KEY,
                applied_at bigint NOT NULL
            );
            CREATE FUNCTION refuse_record() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                RAISE EXCEPTION 'migration record refused by the test';
            END
            $$;
            CREATE TRIGGER refuse_record BEFORE INSERT ON seaql_migrations
                FOR EACH ROW EXECUTE FUNCTION refuse_record();",
        )
        .await
        .expect("refuse the migration record");

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("the trigger refuses the record");

    assert!(
        failed
            .to_string()
            .contains("migration record refused by the test"),
        "{failed}"
    );
    assert_eq!(table_names(&client).await, ["seaql_migrations"]);
    assert_eq!(texts(&client, SEAQL_COUNT).await, ["0"]);
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
async fn the_tables_go_to_the_first_schema_on_the_search_path() {
    let Some(db) = database().await else {
        return;
    };
    let setup = admin(&db.url).await;
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

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(
        texts(&client, "SELECT current_schema()::text").await,
        ["anhanguera"]
    );
    assert_eq!(table_names(&client).await, TODAYS_TABLES);
}
