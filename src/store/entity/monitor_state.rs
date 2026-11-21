use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "monitor_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub singleton: bool,
    pub device_id: String,
    pub acked_seq: i64,
    pub last_success_at: Option<DateTimeUtc>,
    pub payments_failing_since: Option<DateTimeUtc>,
    pub blind_alerted: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
