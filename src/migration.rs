mod m20261128_000001_init;

use sea_orm_migration::{MigrationTrait, MigratorTrait};

pub const INIT: &str = "m20261128_000001_init";

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261128_000001_init::Migration)]
    }
}
