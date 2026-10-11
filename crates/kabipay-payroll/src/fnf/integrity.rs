use super::{
    calculation::{ReviewedPayoutSource, SourceKind},
    model::FinancialSnapshot,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0017_onboarding_offboarding::fnf_settlement, d0106_loan_exit_coordination::fnf_payout_source,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QuerySelect};
use uuid::Uuid;

fn stale() -> KabiPayError {
    KabiPayError::Conflict(
        "Persisted FNF sources or figures changed; calculate and review the settlement again"
            .into(),
    )
}

pub(super) async fn verify(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    row: &fnf_settlement::Model,
    snapshot: &FinancialSnapshot,
) -> KabiPayResult<()> {
    let records = fnf_payout_source::Entity::find()
        .filter(fnf_payout_source::Column::TenantId.eq(tenant))
        .filter(fnf_payout_source::Column::FnfId.eq(row.id))
        .lock_exclusive()
        .all(tx)
        .await?;
    let expected = snapshot
        .input
        .sources
        .iter()
        .map(|source| {
            (
                (source.kind.wire().to_string(), source.source_id),
                source.clone(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let persisted = records
        .into_iter()
        .map(|source| {
            let kind: SourceKind =
                serde_json::from_value(serde_json::Value::String(source.source_kind.clone()))
                    .map_err(|_| stale())?;
            Ok((
                (source.source_kind, source.source_id),
                ReviewedPayoutSource {
                    kind,
                    source_id: source.source_id,
                    amount: source.amount,
                    already_paid: source.already_paid,
                    other_recovery: source.other_recovery,
                    eligible_for_loan: source.eligible_for_loan,
                    approval_reference: source.approval_reference,
                },
            ))
        })
        .collect::<KabiPayResult<std::collections::BTreeMap<_, _>>>()?;
    if expected != persisted {
        return Err(stale());
    }
    let component = |kind| {
        snapshot
            .input
            .sources
            .iter()
            .filter(|source| source.kind == kind)
            .map(|source| source.amount)
            .sum::<Decimal>()
    };
    if row.leave_encashment != Some(component(SourceKind::LeaveEncashment))
        || row.gratuity_amount != Some(component(SourceKind::Gratuity))
        || row.bonus_payable != Some(component(SourceKind::Bonus))
        || row.recovery_amount != Some(snapshot.other_recoveries + snapshot.loan_recovered)
        || row.net_payable != Some(snapshot.employee_payout)
    {
        return Err(stale());
    }
    Ok(())
}
