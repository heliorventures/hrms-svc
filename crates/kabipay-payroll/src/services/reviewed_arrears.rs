//! Pending accruals are frozen in the preview and consumed only at finalization.
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewedArrear {
    pub id: Uuid,
    pub amount: Decimal,
    pub reason: Option<String>,
}

pub async fn pending<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
) -> KabiPayResult<Vec<ReviewedArrear>> {
    let mut result = super::arrear_service::list_pending_by_employee(db, tenant, employee)
        .await?
        .into_iter()
        .map(|r| ReviewedArrear {
            id: r.id,
            amount: r.amount,
            reason: r.reason,
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|r| r.id);
    for item in &result {
        kabipay_tax::domain::validate_amount(item.amount)?;
        if item.amount <= Decimal::ZERO {
            return Err(KabiPayError::Validation(
                "Pending arrear must be positive".into(),
            ));
        }
    }
    Ok(result)
}

pub async fn verify<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    reviewed: &[ReviewedArrear],
) -> KabiPayResult<()> {
    if pending(db, tenant, employee).await? != reviewed {
        return Err(KabiPayError::Validation(
            "Pending arrears changed; calculate and review payroll again".into(),
        ));
    }
    Ok(())
}
