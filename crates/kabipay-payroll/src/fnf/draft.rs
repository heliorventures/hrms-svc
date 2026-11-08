use super::{
    calculation::SourceKind,
    coordination::financial_error,
    model::{FinancialInput, FinancialReview, FinancialSnapshot},
    source,
};
use kabipay_common::{
    context::ClientClaims, finance_contract::ExitSubject, KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::{
    d0017_onboarding_offboarding::fnf_settlement,
    d0106_loan_exit_coordination::fnf_financial_review,
};
use rust_decimal::Decimal;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, Statement, TransactionTrait,
};
use uuid::Uuid;

pub async fn load<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    fnf: Uuid,
    lock: bool,
) -> KabiPayResult<Option<FinancialReview>> {
    let query = fnf_financial_review::Entity::find()
        .filter(fnf_financial_review::Column::TenantId.eq(tenant))
        .filter(fnf_financial_review::Column::FnfId.eq(fnf));
    let row = (if lock { query.lock_exclusive() } else { query })
        .one(db)
        .await?;
    row.map(|row| {
        Ok(FinancialReview {
            fnf_id: fnf,
            revision: row.revision,
            fingerprint: row.input_hash,
            snapshot: serde_json::from_value(row.snapshot)
                .map_err(|_| KabiPayError::Internal("FNF review requires reconciliation".into()))?,
            finalized: row.finalized_at.is_some(),
        })
    })
    .transpose()
}

pub fn legacy_fingerprint(row: &fnf_settlement::Model) -> KabiPayResult<String> {
    kabipay_loans::command_hash(&(
        row.id,
        row.tenant_id,
        row.separation_id,
        row.leave_encashment,
        row.gratuity_amount,
        row.bonus_payable,
        row.recovery_amount,
        row.net_payable,
        &row.status,
        row.updated_at,
    ))
    .map_err(financial_error)
}

fn verify_legacy(
    row: &fnf_settlement::Model,
    input: &FinancialInput,
    snapshot: &FinancialSnapshot,
) -> KabiPayResult<()> {
    if [
        row.leave_encashment,
        row.gratuity_amount,
        row.bonus_payable,
        row.recovery_amount,
    ]
    .iter()
    .all(Option::is_none)
    {
        return Ok(());
    }
    let review = input.legacy_review.as_ref().ok_or_else(|| {
        KabiPayError::Validation(
            "Review and classify the existing FNF draft figures before replacing them".into(),
        )
    })?;
    if review.original_figures_fingerprint != legacy_fingerprint(row)?
        || review.reason.trim().is_empty()
        || review.reason.len() > 2000
    {
        return Err(KabiPayError::Conflict(
            "Legacy FNF figures changed or have not been reviewed".into(),
        ));
    }
    if review.classify_as_other_recoveries
        && row.recovery_amount.unwrap_or(Decimal::ZERO) != snapshot.other_recoveries
    {
        return Err(KabiPayError::Validation("Preserve the reviewed legacy recovery in other recoveries, or explicitly review its replacement".into()));
    }
    Ok(())
}

pub async fn save(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    subject: &ExitSubject,
    input: &FinancialInput,
    expected_revision: Option<i64>,
    expected_fingerprint: Option<&str>,
) -> KabiPayResult<FinancialReview> {
    super::review::actor(claims, tenant)?;
    let tx = db.begin().await?;
    crate::services::loan_recovery::lock(&tx, tenant).await?;
    let row = source::ensure_draft(&tx, tenant, subject.separation_id).await?;
    if row.status != "DRAFT" {
        return Err(KabiPayError::Conflict(
            "Issued final settlement is immutable".into(),
        ));
    }
    let prior = load(&tx, tenant, row.id, true).await?;
    match &prior {
        Some(prior)
            if prior.finalized
                || expected_revision != Some(prior.revision)
                || expected_fingerprint != Some(prior.fingerprint.as_str()) =>
        {
            return Err(KabiPayError::Conflict(
                "FNF review changed; reload it before saving".into(),
            ))
        }
        None if expected_revision.is_some() || expected_fingerprint.is_some() => {
            return Err(KabiPayError::Conflict(
                "FNF review reference is stale".into(),
            ))
        }
        _ => {}
    }
    let revision = prior
        .as_ref()
        .map_or(0, |value| value.revision)
        .checked_add(1)
        .ok_or_else(|| KabiPayError::Validation("FNF revision limit reached".into()))?;
    let snapshot =
        super::review::calculate(&tx, tenant, claims, subject, row.id, revision, input).await?;
    if prior.is_none() {
        verify_legacy(&row, input, &snapshot)?;
    }
    let fingerprint = kabipay_loans::command_hash(&snapshot).map_err(financial_error)?;
    persist(
        &tx,
        tenant,
        claims.sub,
        &row,
        revision,
        &fingerprint,
        &snapshot,
    )
    .await?;
    tx.commit().await?;
    Ok(FinancialReview {
        fnf_id: row.id,
        revision,
        fingerprint,
        snapshot,
        finalized: false,
    })
}

async fn persist(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    actor: Uuid,
    row: &fnf_settlement::Model,
    revision: i64,
    fingerprint: &str,
    snapshot: &FinancialSnapshot,
) -> KabiPayResult<()> {
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM fnf_payout_source WHERE tenant_id=$1 AND fnf_id=$2",
        [tenant.into(), row.id.into()],
    ))
    .await?;
    for source in &snapshot.input.sources {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO fnf_payout_source(id,tenant_id,fnf_id,source_kind,source_id,amount,already_paid,other_recovery,eligible_for_loan,approval_reference) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            [Uuid::new_v4().into(),tenant.into(),row.id.into(),source.kind.wire().into(),source.source_id.into(),source.amount.into(),source.already_paid.into(),source.other_recovery.into(),source.eligible_for_loan.into(),source.approval_reference.clone().into()])).await?;
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
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE fnf_settlement SET leave_encashment=$3,gratuity_amount=$4,bonus_payable=$5,recovery_amount=$6,net_payable=$7,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND status='DRAFT'",
        [tenant.into(),row.id.into(),component(SourceKind::LeaveEncashment).into(),component(SourceKind::Gratuity).into(),component(SourceKind::Bonus).into(),(snapshot.other_recoveries+snapshot.loan_recovered).into(),snapshot.employee_payout.into()])).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO fnf_financial_review(id,tenant_id,fnf_id,employee_id,revision,value_date,currency,minor_units,input_hash,snapshot,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(tenant_id,fnf_id) DO UPDATE SET revision=EXCLUDED.revision,value_date=EXCLUDED.value_date,currency=EXCLUDED.currency,minor_units=EXCLUDED.minor_units,input_hash=EXCLUDED.input_hash,snapshot=EXCLUDED.snapshot,actor_id=EXCLUDED.actor_id,updated_at=NOW()",
        [Uuid::new_v4().into(),tenant.into(),row.id.into(),snapshot.exposure.employee_id.into(),revision.into(),snapshot.input.value_date.into(),snapshot.input.currency.code.clone().into(),(snapshot.input.currency.minor_units as i32).into(),fingerprint.into(),serde_json::to_value(snapshot)?.into(),actor.into()])).await?;
    Ok(())
}
