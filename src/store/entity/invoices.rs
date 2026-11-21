use sea_orm::entity::prelude::*;

use crate::store::KnownInvoice;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "invoices")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub course_enrollment_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub due_date: Date,
    pub status: String,
    pub first_seen_at: DateTimeUtc,
    pub lead_days: Option<i64>,
    pub announced: bool,
    pub closed_at: Option<DateTimeUtc>,
    pub overdue_count: i64,
    pub last_overdue_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for KnownInvoice {
    fn from(row: Model) -> KnownInvoice {
        KnownInvoice {
            id: row.id,
            course_enrollment_id: row.course_enrollment_id,
            due_date: row.due_date,
            status: row.status,
            first_seen_at: row.first_seen_at,
            lead_days: row.lead_days,
            announced: row.announced,
            closed_at: row.closed_at,
            overdue_count: row.overdue_count,
            last_overdue_at: row.last_overdue_at,
        }
    }
}
