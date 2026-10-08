//! Entities for migration 0078 comp-off policy, claims, credits and leave allocations.

use crate::tenant::prelude::*;

macro_rules! relationless { () => { #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {} }; }

pub mod comp_off_policy {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "comp_off_policy")]
    pub struct Model { #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid, pub tenant_id: Uuid, pub designation_id: Option<Uuid>, pub employee_id: Option<Uuid>, pub leave_type_id: Uuid, pub enabled: bool, pub validity_days: i32, pub claim_deadline_days: i32, pub monthly_earning_limit: Option<Decimal>, pub yearly_earning_limit: Option<Decimal>, pub max_unused_balance: Option<Decimal>, pub allow_approved_leave_cancellation: bool, pub created_at: DateTimeUtc, pub updated_at: DateTimeUtc }
    impl ActiveModelBehavior for ActiveModel {} relationless!();
}
pub mod comp_off_claim {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "comp_off_claim")]
    pub struct Model { #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid, pub tenant_id: Uuid, pub employee_id: Uuid, pub worked_date: NaiveDate, pub units: Decimal, pub status: String, pub reason: Option<String>, pub rejection_reason: Option<String>, pub policy_id: Uuid, pub policy_validity_days: i32, pub policy_claim_deadline_days: i32, pub policy_monthly_earning_limit: Option<Decimal>, pub policy_yearly_earning_limit: Option<Decimal>, pub policy_max_unused_balance: Option<Decimal>, pub approved_by: Option<Uuid>, pub approved_at: Option<DateTimeUtc>, pub created_at: DateTimeUtc, pub updated_at: DateTimeUtc }
    impl ActiveModelBehavior for ActiveModel {} relationless!();
}
pub mod comp_off_credit {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "comp_off_credit")]
    pub struct Model { #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid, pub tenant_id: Uuid, pub employee_id: Uuid, pub claim_id: Uuid, pub earned_units: Decimal, pub reserved_units: Decimal, pub used_units: Decimal, pub approved_at: DateTimeUtc, pub approval_business_date: NaiveDate, pub expires_at: NaiveDate, pub created_at: DateTimeUtc, pub updated_at: DateTimeUtc }
    impl ActiveModelBehavior for ActiveModel {} relationless!();
}
pub mod comp_off_allocation {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "comp_off_allocation")]
    pub struct Model { #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid, pub tenant_id: Uuid, pub credit_id: Uuid, pub leave_request_id: Uuid, pub leave_date: NaiveDate, pub units: Decimal, pub status: String, pub created_at: DateTimeUtc, pub updated_at: DateTimeUtc }
    impl ActiveModelBehavior for ActiveModel {} relationless!();
}
