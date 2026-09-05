//! Tenant-scoped SeaORM queries for benefits.

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0014_benefits::{
    benefit_plan, benefit_type, employee_benefit_enrollment,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use uuid::Uuid;

pub async fn list_types(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<benefit_type::Model>> {
    let limit = limit.clamp(1, 100);
    benefit_type::Entity::find()
        .filter(benefit_type::Column::TenantId.eq(tenant_id))
        .order_by_asc(benefit_type::Column::Code)
        .order_by_asc(benefit_type::Column::Id)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_plans(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    active_only: bool,
    limit: u64,
    offset: u64,
) -> KabiPayResult<Vec<benefit_plan::Model>> {
    let limit = limit.clamp(1, 100);
    let mut q = benefit_plan::Entity::find()
        .filter(benefit_plan::Column::TenantId.eq(tenant_id));
    if active_only {
        q = q.filter(benefit_plan::Column::IsActive.eq(true));
    }
    q.order_by_asc(benefit_plan::Column::Name)
        .order_by_asc(benefit_plan::Column::Id)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn plan_names(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    plan_ids: Vec<Uuid>,
) -> KabiPayResult<std::collections::HashMap<Uuid, String>> {
    if plan_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = benefit_plan::Entity::find()
        .filter(benefit_plan::Column::TenantId.eq(tenant_id))
        .filter(benefit_plan::Column::Id.is_in(plan_ids))
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|row| (row.id, row.name)).collect())
}

const STATUS_ENROLLED: &str = "ENROLLED";

/// Current employee's enrollments (newest first).
pub async fn list_enrollments_for_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<employee_benefit_enrollment::Model>> {
    let limit = limit.clamp(1, 200);
    employee_benefit_enrollment::Entity::find()
        .filter(employee_benefit_enrollment::Column::TenantId.eq(tenant_id))
        .filter(employee_benefit_enrollment::Column::EmployeeId.eq(employee_id))
        .order_by_desc(employee_benefit_enrollment::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Employee self-enroll in an **active** plan (one row per employee + plan pair).
pub async fn enroll_in_benefit_plan(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    benefit_plan_id: Uuid,
) -> KabiPayResult<employee_benefit_enrollment::Model> {
    let plan = benefit_plan::Entity::find_by_id(benefit_plan_id)
        .filter(benefit_plan::Column::TenantId.eq(tenant_id))
        .filter(benefit_plan::Column::IsActive.eq(true))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "benefit_plan",
            id: benefit_plan_id.to_string(),
        })?;

    let dup = employee_benefit_enrollment::Entity::find()
        .filter(employee_benefit_enrollment::Column::TenantId.eq(tenant_id))
        .filter(employee_benefit_enrollment::Column::EmployeeId.eq(employee_id))
        .filter(employee_benefit_enrollment::Column::BenefitPlanId.eq(benefit_plan_id))
        .one(db)
        .await?;
    if dup.is_some() {
        return Err(KabiPayError::Conflict(
            "already enrolled in this benefit plan".into(),
        ));
    }

    let today = Utc::now().date_naive();
    let id = Uuid::new_v4();
    let now = Utc::now();
    let am = employee_benefit_enrollment::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        benefit_plan_id: Set(benefit_plan_id),
        status: Set(STATUS_ENROLLED.into()),
        enrolled_on: Set(Some(today)),
        effective_from: Set(today),
        effective_to: Set(None),
        employee_contribution_amount: Set(plan.employee_contribution),
        employer_contribution_amount: Set(plan.employer_contribution),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    employee_benefit_enrollment::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("employee_benefit_enrollment insert missing".into()))
}
