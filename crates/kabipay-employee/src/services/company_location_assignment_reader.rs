use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

#[derive(Debug, FromQueryResult)]
pub struct AssignmentState {
    pub location_id: Option<Uuid>,
    pub location_name: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub revision: i64,
}

/// Read the displayed location and its concurrency token in one PostgreSQL snapshot.
/// Separate employee/assignment queries can pair an old location with a new revision.
pub async fn read_assignment(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    employee: Uuid,
) -> KabiPayResult<AssignmentState> {
    let state = AssignmentState::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r"SELECT e.location_id, l.name AS location_name, a.effective_from,
                  COALESCE(a.revision, 0)::bigint AS revision
          FROM employee e
          LEFT JOIN location l ON l.id = e.location_id
            AND l.tenant_id = e.tenant_id AND NOT l.is_deleted
          LEFT JOIN LATERAL (
            SELECT effective_from, revision FROM employee_location_assignment
            WHERE tenant_id = e.tenant_id AND employee_id = e.id
            ORDER BY effective_from DESC LIMIT 1
          ) a ON TRUE
          WHERE e.tenant_id = $1 AND e.id = $2 AND NOT e.is_deleted",
        [tenant.into(), employee.into()],
    ))
    .one(db)
    .await?
    .ok_or_else(|| KabiPayError::Validation("employee does not belong to this company".into()))?;
    if state.location_id.is_some() && state.location_name.is_none() {
        return Err(KabiPayError::Validation(
            "select an active location in this company".into(),
        ));
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ReaderProxy {
        statements: Arc<Mutex<Vec<Statement>>>,
        location: Option<Uuid>,
        name: Option<String>,
        revision: i64,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ReaderProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            let mut statements = self.statements.lock().unwrap();
            statements.push(statement);
            if statements.len() > 1 {
                return Err(DbErr::Custom(
                    "assignment state must use one snapshot".into(),
                ));
            }
            Ok(vec![ProxyRow::new(BTreeMap::from([
                ("location_id".into(), self.location.into()),
                ("location_name".into(), self.name.clone().into()),
                ("effective_from".into(), Option::<NaiveDate>::None.into()),
                ("revision".into(), self.revision.into()),
            ]))])
        }

        async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
            Err(DbErr::Custom("assignment reader must not write".into()))
        }
    }

    #[tokio::test]
    async fn company_location_assignment_read_pairs_location_and_revision_in_one_statement() {
        let tenant = Uuid::new_v4();
        let employee = Uuid::new_v4();
        for (location, name, revision) in [
            (Some(Uuid::new_v4()), Some("Pune".to_owned()), 7_i64),
            (None, None, 0),
        ] {
            let statements = Arc::new(Mutex::new(Vec::new()));
            let db = Database::connect_proxy(
                DbBackend::Postgres,
                Arc::new(Box::new(ReaderProxy {
                    statements: statements.clone(),
                    location,
                    name: name.clone(),
                    revision,
                })),
            )
            .await
            .unwrap();
            let state = read_assignment(&db, tenant, employee).await.unwrap();
            assert_eq!(state.location_id, location);
            assert_eq!(state.location_name, name);
            assert_eq!(state.revision, revision);
            let statements = statements.lock().unwrap();
            assert_eq!(statements.len(), 1);
            let query = &statements[0];
            assert!(query.sql.contains("LEFT JOIN LATERAL"));
            assert!(query
                .sql
                .contains("tenant_id = e.tenant_id AND employee_id = e.id"));
            assert_eq!(
                query.values.as_ref().unwrap().0,
                vec![tenant.into(), employee.into()]
            );
        }
    }
}
