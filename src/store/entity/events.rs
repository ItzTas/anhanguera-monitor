use sea_orm::entity::prelude::*;

use crate::store::StoredEvent;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    #[sea_orm(unique)]
    pub id: String,
    pub kind: String,
    pub urgent: bool,
    pub title: String,
    pub body: String,
    pub url: String,
    pub occurred_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for StoredEvent {
    fn from(row: Model) -> StoredEvent {
        StoredEvent {
            seq: row.seq,
            id: row.id,
            kind: row.kind,
            urgent: row.urgent,
            title: row.title,
            body: row.body,
            url: row.url,
            occurred_at: row.occurred_at,
        }
    }
}
