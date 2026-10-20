//! Current source-installed reporting presentation; foreign principals never get local employee IDs.
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use std::collections::HashMap;
use uuid::Uuid;
#[derive(Debug)]
pub struct ManagerDisplay {
    pub local_employee_id: Option<Uuid>,
    pub principal_id: Uuid,
    pub tenant_id: Uuid,
    pub display_name: String,
}
pub async fn current_managers(
    db: &DatabaseConnection,
    tenant: Uuid,
    ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, ManagerDisplay>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let slots = (2..=ids.len() + 1)
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![tenant.into()];
    values.extend(ids.into_iter().map(Into::into));
    let sql=format!("SELECT a.subject_employee_id,a.manager_employee_id,a.manager_principal_id,a.manager_tenant_id,r.display_name FROM management_assignment a LEFT JOIN actor_ref r ON r.tenant_id=a.tenant_id AND r.principal_id=a.manager_principal_id WHERE a.tenant_id=$1 AND a.subject_employee_id IN ({slots}) AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp())");
    let mut result = HashMap::new();
    for row in db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await?
    {
        let id = row.try_get("", "subject_employee_id")?;
        let manager = ManagerDisplay {
            local_employee_id: row.try_get("", "manager_employee_id")?,
            principal_id: row.try_get("", "manager_principal_id")?,
            tenant_id: row.try_get("", "manager_tenant_id")?,
            display_name: row.try_get("", "display_name")?,
        };
        if result.insert(id, manager).is_some() {
            return Err(KabiPayError::Conflict(
                "Employee has overlapping current organization reporting relationships".into(),
            ));
        }
    }
    Ok(result)
}

pub async fn organization_managed(db: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<bool> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT organization_reporting_owned($1) AS managed",
            [tenant.into()],
        ))
        .await?
        .ok_or_else(|| {
            KabiPayError::Internal("Organization reporting ownership unavailable".into())
        })?;
    Ok(row.try_get("", "managed")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow};
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    #[derive(Debug)]
    struct Installed {
        row: Mutex<Option<ProxyRow>>,
    }
    #[sea_orm::entity::prelude::async_trait::async_trait]
    impl ProxyDatabaseTrait for Installed {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            assert!(statement.sql.contains("a.tenant_id=$1"));
            assert!(statement.sql.contains("a.subject_employee_id IN ($2)"));
            assert!(statement.sql.contains("effective_from<=clock_timestamp()"));
            assert!(statement.sql.contains("effective_to>clock_timestamp()"));
            Ok(self.row.lock().unwrap().take().into_iter().collect())
        }
        async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
            panic!("display must never write")
        }
    }
    #[tokio::test]
    async fn current_foreign_manager_is_named_without_a_local_employee_uuid() {
        let (tenant, subject, principal, foreign) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let row = ProxyRow::new(BTreeMap::from([
            ("subject_employee_id".into(), subject.into()),
            ("manager_employee_id".into(), Option::<Uuid>::None.into()),
            ("manager_principal_id".into(), principal.into()),
            ("manager_tenant_id".into(), foreign.into()),
            ("display_name".into(), "Verified foreign manager".into()),
        ]));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(Installed {
                row: Mutex::new(Some(row)),
            })),
        )
        .await
        .unwrap();
        let managers = current_managers(&db, tenant, &[subject]).await.unwrap();
        let manager = managers.get(&subject).unwrap();
        assert_eq!(manager.local_employee_id, None);
        assert_eq!(manager.principal_id, principal);
        assert_eq!(manager.tenant_id, foreign);
        assert_eq!(manager.display_name, "Verified foreign manager");
    }
}
