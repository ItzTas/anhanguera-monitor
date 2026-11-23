use sea_orm::{ConnectionTrait, DbErr, DeriveMigrationName, Statement};
use sea_orm_migration::async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, SchemaManager};

const CURRENT_SCHEMA: &str = "SELECT quote_ident(current_schema())";

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let schema: String = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                CURRENT_SCHEMA,
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("the current schema query returned no row".to_owned()))?
            .try_get_by_index(0)?;
        // Qualified with the adoption's own schema, so another schema's table is never dropped.
        // No CASCADE: an object that depends on the table makes the DROP fail and the transaction
        // undo it.
        db.execute_unprepared(&format!("DROP TABLE IF EXISTS {schema}.goose_db_version"))
            .await?;
        Ok(())
    }
}
