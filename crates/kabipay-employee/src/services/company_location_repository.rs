use super::company_location_service::{normalized_name, validate_assignment_date};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{
    tenant_business_clock::TenantBusinessClock,
    working_calendar::{audit_calendar, lock_calendar},
    KabiPayError, KabiPayResult, PageInput,
};
use kabipay_db_entities::tenant::{
    d0006_org_hierarchy::location,
    d0007_employee_core::employee,
    d0010_time_shift_roster::holiday_calendar,
    d0097_location_working_calendar::{
        employee_location_assignment as assignment, weekly_off_policy_version as policy,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

pub struct SaveLocationCommand {
    pub id: Option<Uuid>,
    pub expected_updated_at: Option<DateTime<Utc>>,
    pub name: String,
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
}

pub async fn list(
    db: &DatabaseConnection,
    tenant: Uuid,
    page: PageInput,
    search: Option<String>,
    active_only: bool,
) -> KabiPayResult<(Vec<location::Model>, u64)> {
    let mut query = location::Entity::find().filter(location::Column::TenantId.eq(tenant));
    if active_only {
        query = query.filter(location::Column::IsDeleted.eq(false));
    }
    if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
        if search.chars().count() > 200 {
            return Err(KabiPayError::Validation(
                "location search is too long".into(),
            ));
        }
        let escaped = search
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        query = query.filter(sea_orm::sea_query::Expr::cust_with_values(
            "name ILIKE ? ESCAPE '\\'",
            [format!("%{escaped}%")],
        ));
    }
    let total = query.clone().count(db).await?;
    let rows = query
        .order_by_asc(location::Column::Name)
        .order_by_asc(location::Column::Id)
        .offset(page.offset())
        .limit(page.limit())
        .all(db)
        .await?;
    Ok((rows, total))
}

pub async fn active_location<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
) -> KabiPayResult<location::Model> {
    location::Entity::find_by_id(id)
        .filter(location::Column::TenantId.eq(tenant))
        .filter(location::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Validation("select an active location in this company".into()))
}

pub async fn save_location(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    command: SaveLocationCommand,
) -> KabiPayResult<location::Model> {
    let name = normalized_name(&command.name)?;
    for (field, limit) in [
        (&command.address, 500),
        (&command.city, 100),
        (&command.state, 100),
        (&command.country, 100),
    ] {
        if field
            .as_ref()
            .is_some_and(|value| value.chars().count() > limit)
        {
            return Err(KabiPayError::Validation(
                "address allows 500 characters; city, state and country allow 100".into(),
            ));
        }
    }
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant).await?;
    let previous = if let Some(id) = command.id {
        let row = active_location(&txn, tenant, id).await?;
        if command.expected_updated_at != Some(row.updated_at) {
            return Err(KabiPayError::Validation(
                "location changed; reload and try again".into(),
            ));
        }
        Some(row)
    } else {
        None
    };
    let duplicate = location::Entity::find()
        .filter(location::Column::TenantId.eq(tenant))
        .filter(location::Column::IsDeleted.eq(false))
        .filter(sea_orm::sea_query::Expr::cust_with_values(
            "lower(regexp_replace(btrim(name), '\\s+', ' ', 'g')) = lower(?)",
            [name.clone()],
        ))
        .all(&txn)
        .await?
        .into_iter()
        .any(|row| Some(row.id) != command.id);
    if duplicate {
        return Err(KabiPayError::Validation(
            "an active location with this name already exists".into(),
        ));
    }
    let now = Utc::now();
    let mut active = previous
        .clone()
        .map(IntoActiveModel::into_active_model)
        .unwrap_or_else(|| location::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant),
            is_deleted: Set(false),
            created_at: Set(now),
            ..Default::default()
        });
    active.name = Set(name);
    active.address = Set(command.address);
    active.city = Set(command.city);
    active.state = Set(command.state);
    active.country = Set(command.country);
    active.updated_at = Set(now);
    let row = if previous.is_some() {
        active.update(&txn).await?
    } else {
        active.insert(&txn).await?
    };
    audit_calendar(
        &txn,
        tenant,
        actor,
        "location",
        row.id,
        "SAVE",
        previous
            .as_ref()
            .map(|r| serde_json::json!({"name":r.name})),
        Some(serde_json::json!({"name":row.name})),
    )
    .await?;
    txn.commit().await?;
    Ok(row)
}

pub async fn retire_location(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    id: Uuid,
    expected_updated_at: DateTime<Utc>,
    today: NaiveDate,
) -> KabiPayResult<location::Model> {
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant).await?;
    let row = active_location(&txn, tenant, id).await?;
    if row.updated_at != expected_updated_at {
        return Err(KabiPayError::Validation(
            "location changed; reload and try again".into(),
        ));
    }
    let assigned = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::LocationId.eq(id))
        .count(&txn)
        .await?
        > 0;
    let versions = policy::Entity::find()
        .filter(policy::Column::TenantId.eq(tenant))
        .filter(policy::Column::LocationId.eq(id))
        .filter(policy::Column::SupersededAt.is_null())
        .all(&txn)
        .await?;
    let current = versions
        .iter()
        .filter(|r| r.effective_from <= today)
        .max_by_key(|r| r.effective_from);
    let configured = current.is_some_and(|r| !r.inherits_default)
        || versions
            .iter()
            .any(|r| r.effective_from > today && !r.inherits_default);
    let holidays = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant))
        .filter(holiday_calendar::Column::LocationId.eq(id))
        .count(&txn)
        .await?
        > 0;
    if assigned || configured || holidays {
        return Err(KabiPayError::Validation("move assigned employees and remove active or scheduled policies and holiday calendars before retiring this location".into()));
    }
    let mut active = row.clone().into_active_model();
    active.is_deleted = Set(true);
    active.deleted_at = Set(Some(Utc::now()));
    active.deleted_by = Set(Some(actor));
    active.updated_at = Set(Utc::now());
    let retired = active.update(&txn).await?;
    audit_calendar(
        &txn,
        tenant,
        actor,
        "location",
        id,
        "RETIRE",
        Some(serde_json::json!({"name":row.name})),
        Some(serde_json::json!({"retired":true})),
    )
    .await?;
    txn.commit().await?;
    Ok(retired)
}

pub async fn assign_employee_location(
    db: &DatabaseConnection,
    tenant: Uuid,
    employee_id: Uuid,
    actor: Uuid,
    location_id: Option<Uuid>,
    effective_date: NaiveDate,
    expected_revision: i64,
    clock: TenantBusinessClock,
) -> KabiPayResult<assignment::Model> {
    validate_assignment_date(effective_date, clock)?;
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant).await?;
    if let Some(id) = location_id {
        active_location(&txn, tenant, id).await?;
    }
    let target = employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant))
        .filter(employee::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("employee does not belong to this company".into())
        })?;
    let latest = assignment::Entity::find()
        .filter(assignment::Column::TenantId.eq(tenant))
        .filter(assignment::Column::EmployeeId.eq(employee_id))
        .order_by_desc(assignment::Column::EffectiveFrom)
        .one(&txn)
        .await?;
    let revision = latest.as_ref().map_or(0, |row| row.revision);
    if expected_revision != revision {
        return Err(KabiPayError::Validation(
            "employee location changed; reload and try again".into(),
        ));
    }
    let now = Utc::now();
    let same_day = latest.filter(|r| r.effective_from == effective_date);
    let mut active = same_day
        .clone()
        .map(IntoActiveModel::into_active_model)
        .unwrap_or_else(|| assignment::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant),
            employee_id: Set(employee_id),
            effective_from: Set(effective_date),
            created_at: Set(now),
            ..Default::default()
        });
    active.location_id = Set(location_id);
    active.revision = Set(revision + 1);
    active.changed_by = Set(actor);
    active.updated_at = Set(now);
    let row = if same_day.is_some() {
        active.update(&txn).await?
    } else {
        active.insert(&txn).await?
    };
    let old_location = target.location_id;
    let mut emp = target.into_active_model();
    emp.location_id = Set(location_id);
    emp.updated_at = Set(now);
    emp.update(&txn).await?;
    use kabipay_db_entities::tenant::d0097_location_working_calendar::working_calendar_profile as profile;
    if let Some(profile) = profile::Entity::find_by_id(tenant).one(&txn).await? {
        let next_revision = profile.revision + 1;
        let mut active = profile.into_active_model();
        active.revision = Set(next_revision);
        active.updated_at = Set(now);
        active.update(&txn).await?;
    }
    audit_calendar(&txn,tenant,actor,"employee_location",employee_id,"ASSIGN",Some(serde_json::json!({"locationId":old_location,"revision":revision})),Some(serde_json::json!({"locationId":location_id,"effectiveFrom":effective_date,"revision":row.revision}))).await?;
    txn.commit().await?;
    Ok(row)
}
