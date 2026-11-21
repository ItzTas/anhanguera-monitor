use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "enrollments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub course_enrollment_id: String,
    pub information_type: Option<String>,
    pub last_payments_check_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
