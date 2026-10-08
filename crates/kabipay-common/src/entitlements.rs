//! Tenant-scoped module access. Snapshots live for one request/sweep, never in a pool cache.
use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use kabipay_db_entities::ops::{feature_flag, module, tenant, tenant_subscription};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Statement, DbBackend};
use uuid::Uuid;

use crate::{context::ClientClaims, tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};

#[derive(Clone, Debug)]
struct ModuleAccess {
    id: Uuid,
    active: bool,
    core: bool,
    disabled: bool,
    subscription: Option<SubscriptionAccess>,
}

#[derive(Clone, Debug)]
struct SubscriptionAccess {
    active: bool,
    starts: Option<NaiveDate>,
    expires: Option<NaiveDate>,
}

#[derive(Clone, Debug)]
pub struct Entitlements {
    tenant_id: Uuid,
    today: NaiveDate,
    modules: HashMap<String, ModuleAccess>,
    dependencies: HashMap<Uuid, Vec<Uuid>>,
}

impl Entitlements {
    /// Load mutable authorization from the control plane, even if a tenant pool is cached.
    pub async fn load<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid) -> KabiPayResult<Self> {
        let tenant = tenant::Entity::find_by_id(tenant_id).one(db).await?
            .filter(|row| !row.is_deleted && row.status == "ACTIVE")
            .ok_or(KabiPayError::TenantSuspended(tenant_id))?;
        let today = TenantBusinessClock::from_configured_name(tenant.timezone.as_deref())?.now_date();
        let subscriptions = tenant_subscription::Entity::find()
            .filter(tenant_subscription::Column::TenantId.eq(tenant_id))
            .filter(tenant_subscription::Column::IsDeleted.eq(false)).all(db).await?;
        let flags = feature_flag::Entity::find()
            .filter(feature_flag::Column::TenantId.eq(tenant_id)).all(db).await?;
        let mut modules = HashMap::new();
        for row in module::Entity::find().all(db).await? {
            let subscription = subscriptions.iter().find(|sub| sub.module_id == row.id)
                .map(|sub| SubscriptionAccess {
                    active: sub.status == "ACTIVE", starts: sub.activated_at, expires: sub.expires_at,
                });
            let flag_name = format!("module:{}", row.code);
            let disabled = flags.iter().any(|flag| flag.feature_name == flag_name && !flag.is_enabled);
            modules.insert(row.code.clone(), ModuleAccess {
                id: row.id, active: row.is_active, core: row.is_core,
                disabled, subscription,
            });
        }
        let mut dependencies: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for row in db.query_all(Statement::from_string(DbBackend::Postgres,
            "SELECT module_id, depends_on_module_id FROM kabipay_ops.module_dependency".to_owned())).await? {
            dependencies.entry(row.try_get("", "module_id")?).or_default()
                .push(row.try_get("", "depends_on_module_id")?);
        }
        Ok(Self { tenant_id, today, modules, dependencies })
    }

    pub fn require(&self, code: &str) -> KabiPayResult<()> {
        if self.allows(code) { Ok(()) } else { Err(KabiPayError::ModuleNotSubscribed(code.into())) }
    }

    pub fn allows(&self, code: &str) -> bool {
        self.modules.get(code).is_some_and(|module| self.allows_module(module, &mut HashSet::new()))
    }

    /// Only known, entitled event domains may expose payloads or be delivered.
    pub fn outbox_aggregates(&self) -> Vec<&'static str> {
        [("leave_request", "LEAVE"), ("expense", "EXPENSE"),
            ("employee_profile_change_request", "EMPLOYEE"),
            ("employee_education", "EMPLOYEE"), ("employee_work_experience", "EMPLOYEE")]
            .into_iter().filter_map(|(aggregate, module)| self.allows(module).then_some(aggregate)).collect()
    }

    fn allows_module(&self, module: &ModuleAccess, path: &mut HashSet<Uuid>) -> bool {
        if !module.active || module.disabled || !path.insert(module.id) { return false; }
        let subscribed = module.core || module.subscription.as_ref().is_some_and(|sub| {
            sub.active && sub.starts.is_none_or(|date| date <= self.today)
                && sub.expires.is_none_or(|date| self.today < date)
                && !matches!((sub.starts, sub.expires), (Some(start), Some(end)) if start >= end)
        });
        let dependencies_available = self.dependencies.get(&module.id).is_none_or(|ids| ids.iter().all(|id| {
            self.modules.values().find(|candidate| candidate.id == *id)
                .is_some_and(|dependency| self.allows_module(dependency, path))
        }));
        path.remove(&module.id);
        subscribed && dependencies_available
    }

    /// Restrict cross-domain reports without turning unavailable metrics into zero.
    pub fn filter_claims(&self, claims: &ClientClaims) -> KabiPayResult<ClientClaims> {
        self.require_tenant(claims.tenant_id)?;
        let mut filtered = claims.clone();
        let available = |permission: &str| self.allows(permission_module(permission));
        filtered.permissions.retain(|permission| available(permission));
        filtered.permission_scopes.retain(|permission, _| available(permission));
        filtered.resource_scopes.retain(|resource, _| available(resource));
        Ok(filtered)
    }

    pub fn require_tenant(&self, tenant_id: Uuid) -> KabiPayResult<()> {
        if self.tenant_id == tenant_id { Ok(()) } else {
            Err(KabiPayError::Forbidden("entitlement tenant does not match authenticated tenant".into()))
        }
    }
}

/// Catalog ownership matches the canonical RBAC migrations; services are not billing products.
pub fn service_module(service: &str) -> KabiPayResult<Option<&'static str>> {
    Ok(Some(match service {
        "kabipay-ops" => return Ok(None),
        "kabipay-attendance" => "ATTENDANCE",
        "kabipay-leave" => "LEAVE",
        "kabipay-payroll" => "PAYROLL",
        "kabipay-tax" => "TAX",
        "kabipay-expense" => "EXPENSE",
        "kabipay-recruitment" => "RECRUITMENT",
        "kabipay-workflow" => "WORKFLOW",
        "kabipay-employee" | "kabipay-notification" | "kabipay-performance" | "kabipay-benefits"
        | "kabipay-lms" | "kabipay-succession" | "kabipay-compensation" | "kabipay-grievance"
        | "kabipay-assets" | "kabipay-survey" | "kabipay-analytics" => "EMPLOYEE",
        _ => return Err(KabiPayError::Internal("service has no module entitlement mapping".into())),
    }))
}

pub fn permission_module(permission: &str) -> &'static str {
    match permission.split(':').next().unwrap_or("") {
        "attendance" | "timesheet" => "ATTENDANCE",
        "leave" | "comp_off" => "LEAVE",
        "payroll" => "PAYROLL", "tax" => "TAX",
        "expense" | "travel" => "EXPENSE",
        "recruitment" => "RECRUITMENT", "workflow" => "WORKFLOW",
        _ => "EMPLOYEE",
    }
}

pub async fn require_current_module<C: ConnectionTrait + Sync>(db: &C, tenant_id: Uuid, code: &str) -> KabiPayResult<()> {
    Entitlements::load(db, tenant_id).await?.require(code)
}

#[cfg(test)]
#[path = "entitlements_tests.rs"]
mod tests;
