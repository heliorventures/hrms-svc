//! Write operations for expense claims.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::{
        ClientClaims, ScopeType, PERM_EXPENSE_APPROVE, PERM_EXPENSE_MANAGE, PERM_EXPENSE_PAY,
        PERM_EXPENSE_SUBMIT, PERM_TRAVEL_APPROVE, PERM_TRAVEL_SUBMIT,
    },
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    ExpenseCategoryDto, ExpenseDto, ExpensePolicyDto, SubmitExpenseInput, SubmitTravelRequestInput,
    TravelRequestDto, UpsertExpenseCategoryAdminInput, UpsertExpensePolicyAdminInput,
};
use crate::services::{approval_authority::ExpenseApprovalAuthority, expense_service, travel_request_service};

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn require_exact_scope(
    claims: &ClientClaims,
    permission: &'static str,
    allowed_scopes: &[ScopeType],
    required_scope_label: &'static str,
) -> Result<ScopeType> {
    if !claims.has_any_permission(&[permission]) {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission is required"
        ))
        .into_graphql());
    }
    let scope = claims.scope_for_permission(permission).ok_or_else(|| {
        KabiPayError::Forbidden(format!(
            "{permission} permission requires an explicit valid {required_scope_label} scope"
        ))
        .into_graphql()
    })?;
    if !allowed_scopes.contains(&scope) {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires {required_scope_label} scope"
        ))
        .into_graphql());
    }
    Ok(scope)
}

fn require_self_submission(
    ctx: &Context<'_>,
    permission: &'static str,
) -> Result<(ClientClaims, Uuid)> {
    let claims = require_client_claims(ctx)?;
    require_exact_scope(&claims, permission, &[ScopeType::Self_], "SELF")?;
    let employee_id = claims.employee_id.ok_or_else(|| {
        KabiPayError::Forbidden(format!(
            "{permission} permission requires a JWT-linked employee"
        ))
        .into_graphql()
    })?;
    Ok((claims.clone(), employee_id))
}

fn require_all(ctx: &Context<'_>, permission: &'static str) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    require_exact_scope(claims, permission, &[ScopeType::All], "ALL")?;
    Ok(())
}

fn require_approval_authority(
    ctx: &Context<'_>,
    permission: &'static str,
) -> Result<ExpenseApprovalAuthority> {
    let claims = require_client_claims(ctx)?;
    ExpenseApprovalAuthority::from_claims(claims, permission)
        .map_err(KabiPayError::into_graphql)
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Create a PENDING expense claim for the signed-in user’s employee record.
    async fn submit_expense(
        &self,
        ctx: &Context<'_>,
        input: SubmitExpenseInput,
    ) -> Result<ExpenseDto> {
        let (claims, jwt_employee_id) = require_self_submission(ctx, PERM_EXPENSE_SUBMIT)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if employee_id != jwt_employee_id {
            return Err(KabiPayError::Forbidden(
                "expense:submit is restricted to the JWT-linked employee".into(),
            )
            .into_graphql());
        }
        let category_id = parse_uuid(&input.expense_category_id, "expenseCategoryId")?;
        let amount =
            expense_service::parse_amount(&input.amount).map_err(KabiPayError::into_graphql)?;
        let opt_travel = if let Some(tid) = &input.travel_request_id {
            Some(parse_uuid(tid, "travelRequestId")?)
        } else {
            None
        };
        let receipt = if let Some(rid) = &input.receipt_file_storage_id {
            Some(parse_uuid(rid, "receiptFileStorageId")?)
        } else {
            None
        };
        let m = expense_service::submit_expense(
            &db,
            tenant_id,
            employee_id,
            claims.sub,
            category_id,
            amount,
            &input.currency,
            input.expense_date,
            &input.title,
            opt_travel,
            receipt,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    async fn approve_expense(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        expected_workflow_step_id: ID,
        approved_amount: Option<String>,
    ) -> Result<ExpenseDto> {
        let authority = require_approval_authority(ctx, PERM_EXPENSE_APPROVE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let expected_step_id = parse_uuid(
            &expected_workflow_step_id,
            "expectedWorkflowStepId",
        )?;
        let approved_dec = match approved_amount {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(
                expense_service::parse_amount(&s).map_err(KabiPayError::into_graphql)?,
            ),
        };
        let m = expense_service::approve_expense(
            &db,
            tenant_id,
            id,
            expected_step_id,
            &authority,
            approved_dec,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    async fn reject_expense(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        expected_workflow_step_id: ID,
        reason: Option<String>,
    ) -> Result<ExpenseDto> {
        let authority = require_approval_authority(ctx, PERM_EXPENSE_APPROVE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let expected_step_id = parse_uuid(
            &expected_workflow_step_id,
            "expectedWorkflowStepId",
        )?;
        let m = expense_service::reject_expense(
            &db,
            tenant_id,
            id,
            expected_step_id,
            &authority,
            reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    /// Create a **PENDING** travel request for the signed-in employee.
    async fn submit_travel_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitTravelRequestInput,
    ) -> Result<TravelRequestDto> {
        let (_, jwt_employee_id) = require_self_submission(ctx, PERM_TRAVEL_SUBMIT)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if employee_id != jwt_employee_id {
            return Err(KabiPayError::Forbidden(
                "travel:submit is restricted to the JWT-linked employee".into(),
            )
            .into_graphql());
        }
        let est = match &input.estimated_amount {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let currency = if input.currency.trim().is_empty() {
            "INR"
        } else {
            input.currency.trim()
        };
        let m = travel_request_service::submit_travel_request(
            &db,
            tenant_id,
            employee_id,
            input.origin_location,
            input.destination_location,
            input.from_date,
            input.to_date,
            &input.purpose,
            est,
            currency,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    async fn approve_travel_request(
        &self,
        ctx: &Context<'_>,
        travel_request_id: ID,
        expected_workflow_step_id: ID,
    ) -> Result<TravelRequestDto> {
        let authority = require_approval_authority(ctx, PERM_TRAVEL_APPROVE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&travel_request_id, "travelRequestId")?;
        let expected_step_id = parse_uuid(
            &expected_workflow_step_id,
            "expectedWorkflowStepId",
        )?;
        let m = travel_request_service::approve_travel_request(
            &db,
            tenant_id,
            id,
            expected_step_id,
            &authority,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    async fn reject_travel_request(
        &self,
        ctx: &Context<'_>,
        travel_request_id: ID,
        expected_workflow_step_id: ID,
        reason: Option<String>,
    ) -> Result<TravelRequestDto> {
        let authority = require_approval_authority(ctx, PERM_TRAVEL_APPROVE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&travel_request_id, "travelRequestId")?;
        let expected_step_id = parse_uuid(
            &expected_workflow_step_id,
            "expectedWorkflowStepId",
        )?;
        let m = travel_request_service::reject_travel_request(
            &db,
            tenant_id,
            id,
            expected_step_id,
            &authority,
            reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    /// Create or update an **`expense_category`** row (**`expense:manage`** required).
    async fn upsert_expense_category_admin(
        &self,
        ctx: &Context<'_>,
        input: UpsertExpenseCategoryAdminInput,
    ) -> Result<ExpenseCategoryDto> {
        require_all(ctx, PERM_EXPENSE_MANAGE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = match &input.id {
            Some(raw) => Some(parse_uuid(raw, "categoryId")?),
            None => None,
        };
        let max_amt = match &input.max_amount_per_claim {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(
                expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?,
            ),
        };
        let m = expense_service::upsert_expense_category(
            &db,
            tenant_id,
            id,
            &input.name,
            &input.code,
            max_amt,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseCategoryDto::from(m))
    }

    /// Soft-delete an **`expense_category`** (**`expense:manage`** required).
    async fn delete_expense_category_admin(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<bool> {
        require_all(ctx, PERM_EXPENSE_MANAGE)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        expense_service::delete_expense_category(&db, tenant_id, claims.sub, id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn upsert_expense_policy_admin(
        &self,
        ctx: &Context<'_>,
        input: UpsertExpensePolicyAdminInput,
    ) -> Result<ExpensePolicyDto> {
        require_all(ctx, PERM_EXPENSE_MANAGE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = match &input.id {
            Some(raw) => Some(parse_uuid(raw, "policyId")?),
            None => None,
        };
        let cat = parse_uuid(&input.expense_category_id, "expenseCategoryId")?;
        let department_id = match &input.department_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "departmentId")?),
        };
        let designation_id = match &input.designation_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "designationId")?),
        };
        let role_id = match &input.role_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "roleId")?),
        };
        let limit_day = match &input.limit_per_day {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let limit_month = match &input.limit_per_month {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let max_claim = match &input.max_amount_per_claim {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };

        let m = expense_service::upsert_expense_policy_admin(
            &db,
            tenant_id,
            id,
            cat,
            &input.applicable_to,
            department_id,
            designation_id,
            role_id,
            limit_day,
            limit_month,
            max_claim,
            input.receipt_required,
            input.approval_required,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpensePolicyDto::from(m))
    }

    async fn delete_expense_policy_admin(
        &self,
        ctx: &Context<'_>,
        expense_policy_id: ID,
    ) -> Result<bool> {
        require_all(ctx, PERM_EXPENSE_MANAGE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_policy_id, "expensePolicyId")?;
        expense_service::delete_expense_policy_admin(&db, tenant_id, id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Update reimbursement bookkeeping after financial approval (**`expense:pay=ALL`** required).
    async fn mark_expense_payment_status(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        payment_status: String,
        payment_reference: Option<String>,
    ) -> Result<ExpenseDto> {
        require_all(ctx, PERM_EXPENSE_PAY)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let pref = payment_reference.as_deref();
        let m = expense_service::mark_expense_payment_status(
            &db,
            tenant_id,
            id,
            &payment_status,
            pref,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::{
        context::{
            ClientClaims, CLIENT_JWT_ISSUER, PERM_EXPENSE_APPROVE, PERM_EXPENSE_MANAGE,
            PERM_EXPENSE_PAY, PERM_EXPENSE_SUBMIT, PERM_TRAVEL_APPROVE, PERM_TRAVEL_SUBMIT,
        },
        subgraph::TenantId,
    };
    use std::collections::HashMap;

    #[derive(Clone, Copy)]
    enum RequiredScope {
        SelfOnly,
        TeamOrAll,
        AllOnly,
    }

    struct MutationContract {
        field: &'static str,
        document: String,
        permission: &'static str,
        sibling_permission: &'static str,
        scope: RequiredScope,
    }

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
        let mut claims = claims("unrelated:write", Some("ALL"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        claims
    }

    async fn execute_mutation(
        claims: ClientClaims,
        document: &str,
    ) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(
            crate::resolvers::query::QueryRoot,
            MutationRoot,
            EmptySubscription,
        )
        .data(TenantId(tenant_id))
        .data(claims)
        .finish()
        .execute(Request::new(document))
        .await
    }

    fn assert_denied_before_db(
        response: &async_graphql::Response,
        permission: &str,
        field: &str,
    ) {
        assert_eq!(
            response.errors.len(),
            1,
            "{field} returned an unexpected response: {response:?}"
        );
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission),
            "{field} denial did not name exact authority {permission}: {message}"
        );
        assert!(
            !message.contains("TenantDbCache") && !message.contains("database"),
            "{field} reached storage before authorization: {message}"
        );
    }

    fn assert_authorized_gate_reaches_db(
        response: &async_graphql::Response,
        permission: &str,
        field: &str,
    ) {
        assert_eq!(
            response.errors.len(),
            1,
            "{field} should pass its gate and stop at missing test DB context: {response:?}"
        );
        let message = &response.errors[0].message;
        assert_eq!(
            message, "internal server error",
            "{field} did not pass the {permission} gate: {message}"
        );
        assert!(
            !message.contains(permission),
            "{field} incorrectly rejected valid {permission} authority: {message}"
        );
    }

    fn mutation_contracts() -> Vec<MutationContract> {
        let id = Uuid::new_v4();
        vec![
            MutationContract {
                field: "submitExpense",
                document: format!(
                    "mutation {{ submitExpense(input: {{ expenseCategoryId: \"{id}\", amount: \"1.00\", currency: \"INR\", expenseDate: \"2026-08-27\", title: \"Taxi\" }}) {{ id }} }}"
                ),
                permission: PERM_EXPENSE_SUBMIT,
                sibling_permission: PERM_TRAVEL_SUBMIT,
                scope: RequiredScope::SelfOnly,
            },
            MutationContract {
                field: "approveExpense",
                document: format!(
                    "mutation {{ approveExpense(expenseId: \"{id}\", expectedWorkflowStepId: \"{id}\") {{ id }} }}"
                ),
                permission: PERM_EXPENSE_APPROVE,
                sibling_permission: PERM_TRAVEL_APPROVE,
                scope: RequiredScope::TeamOrAll,
            },
            MutationContract {
                field: "rejectExpense",
                document: format!(
                    "mutation {{ rejectExpense(expenseId: \"{id}\", expectedWorkflowStepId: \"{id}\", reason: \"invalid\") {{ id }} }}"
                ),
                permission: PERM_EXPENSE_APPROVE,
                sibling_permission: PERM_TRAVEL_APPROVE,
                scope: RequiredScope::TeamOrAll,
            },
            MutationContract {
                field: "submitTravelRequest",
                document: "mutation { submitTravelRequest(input: { fromDate: \"2026-08-27\", toDate: \"2026-08-28\", purpose: \"Client visit\", currency: \"INR\" }) { id } }".into(),
                permission: PERM_TRAVEL_SUBMIT,
                sibling_permission: PERM_EXPENSE_SUBMIT,
                scope: RequiredScope::SelfOnly,
            },
            MutationContract {
                field: "approveTravelRequest",
                document: format!(
                    "mutation {{ approveTravelRequest(travelRequestId: \"{id}\", expectedWorkflowStepId: \"{id}\") {{ id }} }}"
                ),
                permission: PERM_TRAVEL_APPROVE,
                sibling_permission: PERM_EXPENSE_APPROVE,
                scope: RequiredScope::TeamOrAll,
            },
            MutationContract {
                field: "rejectTravelRequest",
                document: format!(
                    "mutation {{ rejectTravelRequest(travelRequestId: \"{id}\", expectedWorkflowStepId: \"{id}\", reason: \"invalid\") {{ id }} }}"
                ),
                permission: PERM_TRAVEL_APPROVE,
                sibling_permission: PERM_EXPENSE_APPROVE,
                scope: RequiredScope::TeamOrAll,
            },
            MutationContract {
                field: "upsertExpenseCategoryAdmin",
                document: "mutation { upsertExpenseCategoryAdmin(input: { name: \"Travel\", code: \"TRAVEL\" }) { id } }".into(),
                permission: PERM_EXPENSE_MANAGE,
                sibling_permission: PERM_EXPENSE_APPROVE,
                scope: RequiredScope::AllOnly,
            },
            MutationContract {
                field: "deleteExpenseCategoryAdmin",
                document: format!(
                    "mutation {{ deleteExpenseCategoryAdmin(expenseCategoryId: \"{id}\") }}"
                ),
                permission: PERM_EXPENSE_MANAGE,
                sibling_permission: PERM_EXPENSE_APPROVE,
                scope: RequiredScope::AllOnly,
            },
            MutationContract {
                field: "upsertExpensePolicyAdmin",
                document: format!(
                    "mutation {{ upsertExpensePolicyAdmin(input: {{ expenseCategoryId: \"{id}\", applicableTo: \"ALL\", receiptRequired: false, approvalRequired: true }}) {{ id }} }}"
                ),
                permission: PERM_EXPENSE_MANAGE,
                sibling_permission: PERM_EXPENSE_PAY,
                scope: RequiredScope::AllOnly,
            },
            MutationContract {
                field: "deleteExpensePolicyAdmin",
                document: format!(
                    "mutation {{ deleteExpensePolicyAdmin(expensePolicyId: \"{id}\") }}"
                ),
                permission: PERM_EXPENSE_MANAGE,
                sibling_permission: PERM_EXPENSE_PAY,
                scope: RequiredScope::AllOnly,
            },
            MutationContract {
                field: "markExpensePaymentStatus",
                document: format!(
                    "mutation {{ markExpensePaymentStatus(expenseId: \"{id}\", paymentStatus: \"PAID\") {{ id }} }}"
                ),
                permission: PERM_EXPENSE_PAY,
                sibling_permission: PERM_EXPENSE_APPROVE,
                scope: RequiredScope::AllOnly,
            },
        ]
    }

    #[tokio::test]
    async fn every_expense_and_travel_mutation_requires_its_exact_permission_and_scope() {
        let contracts = mutation_contracts();
        assert_eq!(contracts.len(), 11, "every mutation field must be enumerated");

        for contract in contracts {
            for denied_claims in [
                claims_without_permissions(),
                claims(contract.sibling_permission, Some("ALL")),
                claims(contract.permission, None),
                claims(contract.permission, Some("INVALID")),
            ] {
                let response = execute_mutation(denied_claims, &contract.document).await;
                assert_denied_before_db(&response, contract.permission, contract.field);
            }

            let denied_scopes: &[&str] = match contract.scope {
                RequiredScope::SelfOnly => &["TEAM", "DEPARTMENT", "ALL"],
                RequiredScope::TeamOrAll => &["SELF", "DEPARTMENT"],
                RequiredScope::AllOnly => &["SELF", "TEAM", "DEPARTMENT"],
            };
            for scope in denied_scopes {
                let response = execute_mutation(
                    claims(contract.permission, Some(scope)),
                    &contract.document,
                )
                .await;
                assert_denied_before_db(&response, contract.permission, contract.field);
            }

            let allowed_scopes: &[&str] = match contract.scope {
                RequiredScope::SelfOnly => &["SELF"],
                RequiredScope::TeamOrAll => &["TEAM", "ALL"],
                RequiredScope::AllOnly => &["ALL"],
            };
            for scope in allowed_scopes {
                let response = execute_mutation(
                    claims(contract.permission, Some(scope)),
                    &contract.document,
                )
                .await;
                assert_authorized_gate_reaches_db(
                    &response,
                    contract.permission,
                    contract.field,
                );
            }
        }
    }

    #[tokio::test]
    async fn self_submission_requires_a_jwt_linked_employee_before_storage_access() {
        for contract in mutation_contracts().into_iter().filter(|contract| {
            matches!(contract.scope, RequiredScope::SelfOnly)
        }) {
            let mut without_employee = claims(contract.permission, Some("SELF"));
            without_employee.employee_id = None;

            let response = execute_mutation(without_employee, &contract.document).await;

            assert_denied_before_db(&response, contract.permission, contract.field);
        }
    }

    #[tokio::test]
    async fn approval_mutations_require_expected_workflow_step_id_before_storage_access() {
        let id = Uuid::new_v4();
        let contracts = [
            (
                PERM_EXPENSE_APPROVE,
                format!("mutation {{ approveExpense(expenseId: \"{id}\") {{ id }} }}"),
            ),
            (
                PERM_EXPENSE_APPROVE,
                format!(
                    "mutation {{ rejectExpense(expenseId: \"{id}\", reason: \"invalid\") {{ id }} }}"
                ),
            ),
            (
                PERM_TRAVEL_APPROVE,
                format!(
                    "mutation {{ approveTravelRequest(travelRequestId: \"{id}\") {{ id }} }}"
                ),
            ),
            (
                PERM_TRAVEL_APPROVE,
                format!(
                    "mutation {{ rejectTravelRequest(travelRequestId: \"{id}\", reason: \"invalid\") {{ id }} }}"
                ),
            ),
        ];

        for (permission, document) in contracts {
            let response = execute_mutation(claims(permission, Some("ALL")), &document).await;

            assert_eq!(
                response.errors.len(),
                1,
                "missing expectedWorkflowStepId returned an unexpected response: {response:?}"
            );
            assert!(
                response.errors[0]
                    .message
                    .contains("expectedWorkflowStepId"),
                "GraphQL must require the concurrency token before storage access: {response:?}"
            );
        }
    }
}
