//! Transfer amounts respect salary already paid, without changing net earnings or tax reports.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0090_payroll_period_configuration::payslip_statement;
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::{collections::HashMap, str::FromStr};
use uuid::Uuid;

pub async fn remaining_by_payslip<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, Decimal>> {
    let rows = payslip_statement::Entity::find()
        .filter(payslip_statement::Column::TenantId.eq(tenant))
        .filter(payslip_statement::Column::PayslipId.is_in(ids.to_vec()))
        .all(db)
        .await?;
    rows.into_iter()
        .map(|row| {
            let value = row.statement["remaining_payable"]
                .as_str()
                .and_then(|value| Decimal::from_str(value).ok())
                .filter(|value| !value.is_sign_negative())
                .ok_or_else(|| {
                    KabiPayError::Validation(
                        "salary settlement is unresolved; transfer export is blocked".into(),
                    )
                })?;
            Ok((row.payslip_id, value))
        })
        .collect()
}
