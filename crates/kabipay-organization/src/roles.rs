use crate::{
    models::RoleCapability,
    policy::{validate_capabilities, DELEGATE, MANAGE},
    store::*,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;
async fn save_capabilities(
    tx: &DatabaseTransaction,
    org: Uuid,
    role: Uuid,
    caps: &[RoleCapability],
) -> KabiPayResult<()> {
    for cap in caps {
        tx.execute(sql("INSERT INTO kabipay_ops.organization_role_capability(organization_id,role_id,capability_code,employee_scope) VALUES($1,$2,$3,$4)",vec![org.into(),role.into(),cap.code.clone().into(),cap.employee_scope.code().into()])).await?;
    }
    Ok(())
}
pub(crate) async fn create(
    tx: &DatabaseTransaction,
    org: Uuid,
    name: &str,
    caps: &[RoleCapability],
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_name(name, 100)?;
    validate_capabilities(caps)?;
    let role = Uuid::new_v4();
    tx.execute(sql("INSERT INTO kabipay_ops.organization_role(id,organization_id,name,status) VALUES($1,$2,$3,'ACTIVE')",vec![role.into(),org.into(),name.into()])).await?;
    save_capabilities(tx, org, role, caps).await?;
    Ok((role, "ACTIVE", 1))
}
pub(crate) async fn replace(
    tx: &DatabaseTransaction,
    org: Uuid,
    actor: Uuid,
    role: Uuid,
    version: i64,
    caps: &[RoleCapability],
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(role)?;
    let central = validate_capabilities(caps)?;
    if version <= 0 || version == i64::MAX {
        return Err(conflict());
    }
    if tx.query_one(sql("SELECT id FROM kabipay_ops.principal_role_binding WHERE organization_id=$1 AND role_id=$2 AND principal_id=$3 AND status IN ('ACTIVE','PENDING')",vec![org.into(),role.into(),actor.into()])).await?.is_some(){return Err(denied());}
    let updated=tx.execute(sql("UPDATE kabipay_ops.organization_role SET version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND version=$3 AND status='ACTIVE'",vec![role.into(),org.into(),version.into()])).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict());
    }
    tx.execute(sql("DELETE FROM kabipay_ops.organization_role_capability WHERE organization_id=$1 AND role_id=$2",vec![org.into(),role.into()])).await?;
    save_capabilities(tx, org, role, caps).await?;
    let status = if central { "ACTIVE" } else { "PENDING" };
    tx.execute(sql("UPDATE kabipay_ops.principal_role_binding SET status=$3,version=version+1,updated_at=NOW() WHERE organization_id=$1 AND role_id=$2 AND status IN ('ACTIVE','PENDING')",vec![org.into(),role.into(),status.into()])).await?;
    tx.execute(sql("UPDATE kabipay_ops.role_binding_company s SET expected_grant_version=b.version,activation_status=$3 FROM kabipay_ops.principal_role_binding b WHERE s.binding_id=b.id AND s.organization_id=b.organization_id AND b.organization_id=$1 AND b.role_id=$2 AND b.status=$3 AND s.activation_status<>'REVOKED'",vec![org.into(),role.into(),status.into()])).await?;
    Ok((role, "ACTIVE", version + 1))
}
pub(crate) async fn grant(
    tx: &DatabaseTransaction,
    org: Uuid,
    actor: Uuid,
    subject: Uuid,
    role: Uuid,
    companies: &[Uuid],
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(subject)?;
    valid_id(role)?;
    if subject == actor || companies.is_empty() || companies.len() > 100 {
        return Err(denied());
    }
    let unique: std::collections::HashSet<_> = companies.iter().collect();
    if unique.len() != companies.len() {
        return Err(KabiPayError::Validation("duplicate company scope".into()));
    }
    one(tx,"SELECT p.id FROM kabipay_ops.principal p JOIN kabipay_ops.principal_login_link l ON l.principal_id=p.id JOIN kabipay_ops.organization_company m ON m.tenant_id=l.source_tenant_id WHERE p.id=$1 AND p.status='ACTIVE' AND l.status='VERIFIED' AND l.issuer='kabipay-client' AND m.organization_id=$2 AND m.status='ACTIVE' AND m.effective_from<=NOW() AND (m.effective_to IS NULL OR NOW()<m.effective_to) LIMIT 1",vec![subject.into(),org.into()]).await?;
    one(tx,"SELECT id FROM kabipay_ops.organization_role WHERE id=$1 AND organization_id=$2 AND status='ACTIVE'",vec![role.into(),org.into()]).await?;
    let caps=tx.query_all(sql("SELECT capability_code FROM kabipay_ops.organization_role_capability WHERE organization_id=$1 AND role_id=$2",vec![org.into(),role.into()])).await?;
    if caps.is_empty() {
        return Err(denied());
    }
    let mut central = true;
    for cap in caps {
        let code: String = cap.try_get("", "capability_code")?;
        central &= matches!(code.as_str(), MANAGE | DELEGATE);
    }
    for company in companies {
        valid_id(*company)?;
        one(tx,"SELECT id FROM kabipay_ops.organization_company WHERE id=$1 AND organization_id=$2 AND status='ACTIVE' AND effective_from<=NOW() AND (effective_to IS NULL OR NOW()<effective_to)",vec![(*company).into(),org.into()]).await?;
    }
    let id = Uuid::new_v4();
    let status = if central { "ACTIVE" } else { "PENDING" };
    tx.execute(sql("INSERT INTO kabipay_ops.principal_role_binding(id,organization_id,principal_id,role_id,status,effective_from,granted_by) VALUES($1,$2,$3,$4,$5,NOW(),$6)",vec![id.into(),org.into(),subject.into(),role.into(),status.into(),actor.into()])).await?;
    for company in companies {
        tx.execute(sql("INSERT INTO kabipay_ops.role_binding_company(organization_id,binding_id,organization_company_id,activation_status) VALUES($1,$2,$3,$4)",vec![org.into(),id.into(),(*company).into(),status.into()])).await?;
    }
    Ok((id, status, 1))
}
pub(crate) async fn revoke(
    tx: &DatabaseTransaction,
    org: Uuid,
    actor: Uuid,
    binding: Uuid,
    version: i64,
) -> KabiPayResult<(Uuid, &'static str, i64)> {
    valid_id(binding)?;
    if version <= 0 || version == i64::MAX {
        return Err(conflict());
    }
    let updated=tx.execute(sql("UPDATE kabipay_ops.principal_role_binding SET status='REVOKED',version=version+1,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND principal_id<>$3 AND version=$4 AND status IN ('ACTIVE','PENDING','SUSPENDED')",vec![binding.into(),org.into(),actor.into(),version.into()])).await?;
    if updated.rows_affected() != 1 {
        return Err(conflict());
    }
    tx.execute(sql("UPDATE kabipay_ops.role_binding_company SET activation_status='REVOKED',expected_grant_version=$3 WHERE organization_id=$1 AND binding_id=$2",vec![org.into(),binding.into(),(version+1).into()])).await?;
    Ok((binding, "REVOKED", version + 1))
}
