//! Direct account administration shares the lock used by role revocation.
use super::*;
use sea_orm::DatabaseTransaction;

#[cfg(test)]
#[path = "employee_login_administration_tests.rs"]
mod tests;

async fn finish<T>(transaction: DatabaseTransaction, result: KabiPayResult<T>) -> KabiPayResult<T> {
    match result {
        Ok(value) => {
            transaction.commit().await?;
            Ok(value)
        }
        Err(error) => {
            transaction.rollback().await?;
            Err(error)
        }
    }
}

pub async fn create_with_login_for_actor(
    db: &DatabaseConnection,
    tenant: Uuid,
    data: NewEmployee,
    account: NewLoginAccount,
    actor: Option<Uuid>,
) -> KabiPayResult<employee::Model> {
    if data.user_id.is_some() {
        return Err(KabiPayError::Validation(
            "userId cannot be supplied when loginAccount is used".into(),
        ));
    }
    rbac_admin_service::require_nonempty_role_assignment(&account.role_ids)?;
    canonical_employment_status(&data.status)?;
    let transaction = db.begin().await?;
    let result = async {
        if let Some(actor) = actor {
            rbac_admin_service::require_fresh_direct_rbac_admin(&transaction, tenant, actor)
                .await?;
        }
        create_with_login_in_transaction(&transaction, tenant, data, account).await
    }
    .await;
    finish(transaction, result).await
}

pub async fn provision_login_for_actor(
    db: &DatabaseConnection,
    tenant: Uuid,
    employee_id: Uuid,
    account: NewLoginAccount,
    actor: Option<Uuid>,
) -> KabiPayResult<employee::Model> {
    rbac_admin_service::require_nonempty_role_assignment(&account.role_ids)?;
    let transaction = db.begin().await?;
    let result = async {
        if let Some(actor) = actor {
            rbac_admin_service::require_fresh_direct_rbac_admin(&transaction, tenant, actor)
                .await?;
        }
        provision_login_in_transaction(&transaction, tenant, employee_id, account).await
    }
    .await;
    finish(transaction, result).await
}

pub async fn reset_linked_user_password_for_actor(
    db: &DatabaseConnection,
    tenant: Uuid,
    employee_id: Uuid,
    password_hash: String,
    actor: Option<Uuid>,
) -> KabiPayResult<()> {
    let transaction = db.begin().await?;
    let result = async {
        if let Some(actor) = actor {
            rbac_admin_service::require_fresh_direct_rbac_admin(&transaction, tenant, actor)
                .await?;
        }
        reset_linked_user_password_in_transaction(&transaction, tenant, employee_id, password_hash)
            .await
    }
    .await;
    finish(transaction, result).await
}
