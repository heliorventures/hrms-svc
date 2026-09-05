//! Root query resolvers for kabipay-expense.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_claims, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::{
        ClientClaims, ScopeType, PERM_EXPENSE_APPROVE, PERM_EXPENSE_MANAGE,
        PERM_EXPENSE_PAY, PERM_EXPENSE_READ, PERM_EXPENSE_SUBMIT, PERM_TRAVEL_APPROVE,
        PERM_TRAVEL_MANAGE, PERM_TRAVEL_READ, PERM_TRAVEL_SUBMIT,
    },
    subgraph::{require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError, KabiPayResult,
};

use crate::resolvers::types::{
    ExpenseCategoryDto, ExpenseDto, ExpensePolicyDto, ExpenseSubmissionHints, TravelRequestDto,
};
use crate::services::{expense_service, travel_request_service};
use uuid::Uuid;

pub(crate) fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

#[cfg(test)]
fn expense_read_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    data_scope_from_claims(claims, PERM_EXPENSE_READ)
}

#[cfg(test)]
fn travel_read_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    data_scope_from_claims(claims, PERM_TRAVEL_READ)
}

fn request_list_scope_from_claims(
    claims: Option<&ClientClaims>,
    permissions: &[&str],
    resource: &str,
) -> KabiPayResult<ScopeType> {
    let claims = claims.ok_or(KabiPayError::Unauthorised)?;
    let assigned: Vec<_> = permissions
        .iter()
        .copied()
        .filter(|permission| claims.has_any_permission(&[*permission]))
        .collect();
    if assigned.is_empty() {
        return Err(KabiPayError::Forbidden(format!(
            "a scoped {resource} permission is required"
        )));
    }
    assigned
        .into_iter()
        .filter_map(|permission| claims.scope_for_permission(permission))
        .max_by_key(|scope| scope.rank())
        .ok_or_else(|| {
            KabiPayError::Forbidden(format!(
                "assigned {resource} permissions require an explicit valid scope"
            ))
        })
}

fn expense_list_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    request_list_scope_from_claims(
        claims,
        &[
            PERM_EXPENSE_READ,
            PERM_EXPENSE_SUBMIT,
            PERM_EXPENSE_APPROVE,
            PERM_EXPENSE_MANAGE,
            PERM_EXPENSE_PAY,
        ],
        "expense",
    )
}

fn travel_list_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    request_list_scope_from_claims(
        claims,
        &[
            PERM_TRAVEL_READ,
            PERM_TRAVEL_SUBMIT,
            PERM_TRAVEL_APPROVE,
            PERM_TRAVEL_MANAGE,
        ],
        "travel",
    )
}

fn expense_submit_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    let scope = data_scope_from_claims(ctx.data_opt::<ClientClaims>(), PERM_EXPENSE_SUBMIT)
        .map_err(KabiPayError::into_graphql)?;
    if scope != ScopeType::Self_ {
        return Err(KabiPayError::Forbidden(format!(
            "{PERM_EXPENSE_SUBMIT} permission requires SELF scope"
        ))
        .into_graphql());
    }
    Ok(scope)
}

fn expense_list_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    expense_list_scope_from_claims(ctx.data_opt::<ClientClaims>())
        .map_err(KabiPayError::into_graphql)
}

fn travel_list_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    travel_list_scope_from_claims(ctx.data_opt::<ClientClaims>())
        .map_err(KabiPayError::into_graphql)
}

fn expense_manage_all_scope_from_claims(
    claims: Option<&ClientClaims>,
) -> KabiPayResult<ScopeType> {
    let scope = data_scope_from_claims(claims, PERM_EXPENSE_MANAGE)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden(format!(
            "{PERM_EXPENSE_MANAGE} permission requires ALL scope"
        )));
    }
    Ok(scope)
}

fn require_expense_configuration(ctx: &Context<'_>) -> Result<()> {
    expense_manage_all_scope_from_claims(ctx.data_opt::<ClientClaims>())
        .map(|_| ())
        .map_err(KabiPayError::into_graphql)
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn expense_health(&self) -> &'static str {
        "ok"
    }

    async fn expense_categories(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ExpenseCategoryDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        expense_list_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = expense_service::list_categories(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpenseCategoryDto::from).collect())
    }

    async fn expenses(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ExpenseDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = expense_list_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = expense_service::list_expenses(&db, tenant_id, limit, &filt)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpenseDto::from).collect())
    }

    /// Travel / trip requests for the caller's exact `travel:read` data scope.
    async fn travel_requests(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TravelRequestDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = travel_list_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = travel_request_service::list_travel_requests(&db, tenant_id, limit, &filt)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TravelRequestDto::from).collect())
    }

    async fn expense_submission_hints(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<ExpenseSubmissionHints> {
        let tenant_id = require_tenant_id(ctx)?;
        expense_submit_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let cid = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        let cat =
            expense_service::get_expense_category(&db, tenant_id, cid).await?;

        let h = expense_service::resolve_expense_submit_constraints(
            &db,
            tenant_id,
            cid,
            employee_id,
            cat.max_amount_per_claim,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseSubmissionHints {
            expense_category_id,
            max_amount_per_claim: h.max_amount_per_claim.map(|d| d.to_string()),
            receipt_required: h.receipt_required,
            limit_per_month: h.limit_per_month.map(|d| d.to_string()),
            limit_per_day: h.limit_per_day.map(|d| d.to_string()),
        })
    }

    /// Scoped expense policies for a category (**`expense:manage`**).
    async fn expense_policies_for_admin(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<Vec<ExpensePolicyDto>> {
        require_expense_configuration(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        let rows = expense_service::list_expense_policies_for_category(&db, tenant_id, cid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpensePolicyDto::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use kabipay_common::client_data_scope::EmployeeScopeFilter;
    use kabipay_common::context::{
        ClientClaims, ScopeType, CLIENT_JWT_ISSUER, PERM_EXPENSE_MANAGE, PERM_TRAVEL_READ,
    };
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| HashMap::from([(permission.to_string(), scope.to_string())]))
            .unwrap_or_default();
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    fn claims_without_permissions() -> ClientClaims {
        let mut claims = claims("unrelated:read", Some("ALL"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        claims
    }

    async fn execute_query(claims: ClientClaims, query: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(query))
            .await
    }

    fn assert_permission_denied_before_db(
        response: &async_graphql::Response,
        expected_message: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(expected_message),
            "unexpected denial: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    #[test]
    fn expense_and_travel_read_gates_require_their_own_exact_scopes() {
        for (gate, permission) in [
            (
                expense_read_scope_from_claims
                    as fn(Option<&ClientClaims>) -> KabiPayResult<ScopeType>,
                PERM_EXPENSE_READ,
            ),
            (
                travel_read_scope_from_claims
                    as fn(Option<&ClientClaims>) -> KabiPayResult<ScopeType>,
                PERM_TRAVEL_READ,
            ),
        ] {
            for (wire_scope, expected) in [
                ("SELF", ScopeType::Self_),
                ("TEAM", ScopeType::Team),
                ("ALL", ScopeType::All),
            ] {
                assert_eq!(
                    gate(Some(&claims(permission, Some(wire_scope))))
                        .expect("valid exact read scope"),
                    expected
                );
            }

            assert!(matches!(
                gate(Some(&claims(permission, None))),
                Err(KabiPayError::Forbidden(_))
            ));
        }

        assert!(matches!(
            expense_read_scope_from_claims(Some(&claims(PERM_TRAVEL_READ, Some("ALL")))),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(matches!(
            travel_read_scope_from_claims(Some(&claims(PERM_EXPENSE_READ, Some("ALL")))),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[test]
    fn approval_scope_can_open_its_matching_request_list_without_read_permission() {
        assert_eq!(
            expense_list_scope_from_claims(Some(&claims(PERM_EXPENSE_APPROVE, Some("TEAM"))))
                .expect("expense approver needs the scoped approval list"),
            ScopeType::Team
        );
        assert_eq!(
            travel_list_scope_from_claims(Some(&claims(PERM_TRAVEL_APPROVE, Some("ALL"))))
                .expect("travel approver needs the scoped approval list"),
            ScopeType::All
        );
        assert!(matches!(
            expense_list_scope_from_claims(Some(&claims(PERM_EXPENSE_APPROVE, None))),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(matches!(
            travel_list_scope_from_claims(Some(&claims(PERM_EXPENSE_APPROVE, Some("ALL")))),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[test]
    fn expense_policy_admin_requires_exact_all_manage_scope() {
        assert!(expense_manage_all_scope_from_claims(Some(&claims(
            PERM_EXPENSE_MANAGE,
            Some("ALL"),
        )))
        .is_ok());

        for scope in [None, Some("INVALID"), Some("SELF"), Some("TEAM"), Some("DEPARTMENT")] {
            assert!(matches!(
                expense_manage_all_scope_from_claims(Some(&claims(PERM_EXPENSE_MANAGE, scope))),
                Err(KabiPayError::Forbidden(_))
            ));
        }

        assert!(matches!(
            expense_manage_all_scope_from_claims(Some(&claims(PERM_EXPENSE_READ, Some("ALL")))),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[test]
    fn expense_and_travel_target_filters_enforce_self_recursive_team_and_all() {
        let viewer_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();

        let self_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id]);
        let team_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]);

        assert!(self_filter.allows_employee(viewer_id));
        assert!(!self_filter.allows_employee(descendant_id));
        assert!(team_filter.allows_employee(viewer_id));
        assert!(team_filter.allows_employee(descendant_id));
        assert!(!team_filter.allows_employee(outside_id));
        assert!(EmployeeScopeFilter::Unrestricted.allows_employee(outside_id));
    }

    #[tokio::test]
    async fn expense_submission_hints_and_configuration_use_their_exact_permissions() {
        let category_id = Uuid::new_v4();
        let fields = vec![
            (
                format!(
                    "{{ expenseSubmissionHints(expenseCategoryId: \"{category_id}\") {{ __typename }} }}"
                ),
                PERM_EXPENSE_SUBMIT,
                PERM_EXPENSE_READ,
                true,
            ),
            (
                format!(
                    "{{ expensePoliciesForAdmin(expenseCategoryId: \"{category_id}\") {{ __typename }} }}"
                ),
                PERM_EXPENSE_MANAGE,
                PERM_EXPENSE_READ,
                true,
            ),
        ];

        for (query, required_permission, sibling_permission, requires_scope) in fields {
            for denied_claims in [
                claims_without_permissions(),
                claims(sibling_permission, Some("ALL")),
            ] {
                let response = execute_query(denied_claims, &query).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!("{required_permission} permission required"),
                );
            }

            if requires_scope {
                for scope in [None, Some("INVALID")] {
                    let response = execute_query(claims(required_permission, scope), &query).await;
                    assert_permission_denied_before_db(
                        &response,
                        &format!(
                            "{required_permission} permission requires an explicit valid scope"
                        ),
                    );
                }
            }

            if required_permission == PERM_EXPENSE_MANAGE {
                for scope in ["SELF", "TEAM", "DEPARTMENT"] {
                    let response =
                        execute_query(claims(required_permission, Some(scope)), &query).await;
                    assert_permission_denied_before_db(
                        &response,
                        &format!("{required_permission} permission requires ALL scope"),
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn request_lists_accept_matching_scoped_approval_authority_before_db_access() {
        for (query, permission, unrelated_permission, resource) in [
            (
                "{ expenseCategories { __typename } }",
                PERM_EXPENSE_APPROVE,
                PERM_TRAVEL_APPROVE,
                "expense",
            ),
            (
                "{ expenses { __typename } }",
                PERM_EXPENSE_APPROVE,
                PERM_TRAVEL_APPROVE,
                "expense",
            ),
            (
                "{ travelRequests { __typename } }",
                PERM_TRAVEL_APPROVE,
                PERM_EXPENSE_APPROVE,
                "travel",
            ),
        ] {
            let denied = execute_query(claims(unrelated_permission, Some("ALL")), query).await;
            assert_permission_denied_before_db(
                &denied,
                &format!("a scoped {resource} permission is required"),
            );

            let malformed = execute_query(claims(permission, None), query).await;
            assert_permission_denied_before_db(
                &malformed,
                &format!(
                    "assigned {resource} permissions require an explicit valid scope"
                ),
            );

            let accepted = execute_query(claims(permission, Some("TEAM")), query).await;
            assert!(
                accepted
                    .errors
                    .iter()
                    .all(|error| !error.message.contains("permission")),
                "matching action permission was rejected: {accepted:?}"
            );
        }
    }
}
