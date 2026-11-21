mod goose;
mod m20261120_000001_goose_schema;
mod m20261120_000002_drop_goose_db_version;

use sea_orm_migration::{MigrationTrait, MigratorTrait};

pub const GOOSE_SCHEMA: &str = "m20261120_000001_goose_schema";
pub const DROP_GOOSE_DB_VERSION: &str = "m20261120_000002_drop_goose_db_version";

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261120_000001_goose_schema::Migration),
            Box::new(m20261120_000002_drop_goose_db_version::Migration),
        ]
    }
}
