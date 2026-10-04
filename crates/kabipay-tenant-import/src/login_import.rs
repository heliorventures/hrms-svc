//! Optional login provisioning is independent of employee and financial imports.
use crate::{contract::ImportEmployee, employee_import::NewCredential, options::ImportOptions};
use anyhow::{bail, Result};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{role, user},
    d0007_employee_core::employee,
};
use kabipay_employee::services::employee_service::{
    provision_login_in_transaction, NewLoginAccount,
};
use rand::{distributions::Alphanumeric, Rng};
use sea_orm::{ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter};
use uuid::Uuid;

pub async fn save(
    txn: &DatabaseTransaction,
    row: &ImportEmployee,
    options: &ImportOptions,
    id: Uuid,
    credentials: &mut Vec<NewCredential>,
) -> Result<&'static str> {
    let employee = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(options.tenant_id))
        .filter(employee::Column::Id.eq(id))
        .one(txn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("EMPLOYEE_UNRESOLVED"))?;
    let code = row
        .employee
        .code
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("EMPLOYEE_CODE_REQUIRED"))?;
    let supplied = options.login_by_employee_code.get(code);
    if let Some(user_id) = employee.user_id {
        let login = user::Entity::find()
            .filter(user::Column::TenantId.eq(options.tenant_id))
            .filter(user::Column::Id.eq(user_id))
            .one(txn)
            .await?
            .ok_or_else(|| anyhow::anyhow!("LINKED_LOGIN_UNRESOLVED"))?;
        if login.is_deleted || !login.is_active {
            bail!("LINKED_LOGIN_INACTIVE_REQUIRES_REVIEW");
        }
        if supplied.is_some_and(|username| username != &login.username) {
            bail!("LOGIN_MANIFEST_DISAGREES_WITH_EXISTING_LINK");
        }
        return Ok("UNCHANGED");
    }
    let Some(username) = supplied else {
        bail!("LOGIN_MANIFEST_NOT_SUPPLIED");
    };
    let roles = role::Entity::find()
        .filter(role::Column::TenantId.eq(options.tenant_id))
        .filter(role::Column::Name.eq("EMPLOYEE"))
        .filter(role::Column::IsDeleted.eq(false))
        .all(txn)
        .await?;
    if roles.len() != 1 {
        bail!("EMPLOYEE_ROLE_AMBIGUOUS");
    }
    let secret: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    let account = NewLoginAccount {
        username: username.clone(),
        email: None,
        password_hash: kabipay_common::password::hash(&secret)?,
        role_ids: vec![roles[0].id],
    };
    provision_login_in_transaction(txn, options.tenant_id, id, account).await?;
    credentials.push(NewCredential {
        employee_code: code.into(),
        username: username.clone(),
        temporary_password: secret,
    });
    Ok("CREATED")
}
