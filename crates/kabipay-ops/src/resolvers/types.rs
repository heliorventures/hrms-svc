//! GraphQL DTOs for kabipay-ops (unified ops plane).

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_db_entities::ops::{
    billing_cycle, feature_flag, invoice, module, operator_role, operator_user, payment, tenant,
    tenant_subscription,
};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Tenant")]
pub struct TenantDto {
    pub id: ID,
    pub name: String,
    pub status: String,
    pub plan: Option<String>,
    pub country: Option<String>,
    pub currency: Option<String>,
    pub subdomain: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<tenant::Model> for TenantDto {
    fn from(m: tenant::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            name: m.name,
            status: m.status,
            plan: m.plan,
            country: m.country,
            currency: m.currency,
            subdomain: m.subdomain,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Module")]
pub struct ModuleDto {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub is_active: bool,
    pub display_order: i32,
    pub is_core: bool,
}

impl From<module::Model> for ModuleDto {
    fn from(m: module::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            code: m.code,
            name: m.name,
            category: m.category,
            description: m.description,
            is_active: m.is_active,
            display_order: m.display_order,
            is_core: m.is_core,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantSubscription")]
pub struct TenantSubscriptionDto {
    pub id: ID,
    pub tenant_id: ID,
    pub module_id: ID,
    pub status: String,
    pub activated_at: Option<NaiveDate>,
    pub expires_at: Option<NaiveDate>,
    pub contracted_seats: i32,
    pub current_seat_usage: i32,
    pub overage_policy: String,
}

impl From<tenant_subscription::Model> for TenantSubscriptionDto {
    fn from(m: tenant_subscription::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            module_id: ID(m.module_id.to_string()),
            status: m.status,
            activated_at: m.activated_at,
            expires_at: m.expires_at,
            contracted_seats: m.contracted_seats,
            current_seat_usage: m.current_seat_usage,
            overage_policy: m.overage_policy,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "FeatureFlag")]
pub struct FeatureFlagDto {
    pub id: ID,
    pub tenant_id: ID,
    pub feature_name: String,
    pub is_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<feature_flag::Model> for FeatureFlagDto {
    fn from(m: feature_flag::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            feature_name: m.feature_name,
            is_enabled: m.is_enabled,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ProvisionTenantPayload {
    pub tenant: TenantDto,
    pub schema_name: String,
    pub migrations_ran: bool,
    pub detail: Option<String>,
}

#[derive(InputObject, Debug)]
pub struct ProvisionTenantInput {
    pub name: String,
    pub code: String,
    pub country: Option<String>,
    pub currency: Option<String>,
    pub schema_name_override: Option<String>,
    #[graphql(default = false)]
    pub run_migrations: bool,
}

#[derive(InputObject, Debug)]
pub struct UpsertTenantSubscriptionInput {
    pub tenant_id: ID,
    pub module_id: ID,
    pub status: Option<String>,
    pub contracted_seats: i32,
    pub overage_policy: Option<String>,
    pub activated_at: Option<NaiveDate>,
    pub expires_at: Option<NaiveDate>,
}

#[derive(InputObject, Debug)]
pub struct UpdateTenantInput {
    pub tenant_id: ID,
    pub name: Option<String>,
    pub status: Option<String>,
    pub plan: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "OperatorUser")]
pub struct OperatorUserDto {
    pub id: ID,
    pub email: String,
    pub full_name: String,
    pub phone: Option<String>,
    pub is_active: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<operator_user::Model> for OperatorUserDto {
    fn from(m: operator_user::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            email: m.email,
            full_name: m.full_name,
            phone: m.phone,
            is_active: m.is_active,
            last_login_at: m.last_login_at,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "OperatorRole")]
pub struct OperatorRoleDto {
    pub id: ID,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<operator_role::Model> for OperatorRoleDto {
    fn from(m: operator_role::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            code: m.code,
            name: m.name,
            description: m.description,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "BillingCycle")]
pub struct BillingCycleDto {
    pub id: ID,
    pub tenant_id: ID,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

impl From<billing_cycle::Model> for BillingCycleDto {
    fn from(m: billing_cycle::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            period_start: m.period_start,
            period_end: m.period_end,
            frequency: m.frequency,
            status: m.status,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Invoice")]
pub struct InvoiceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub billing_cycle_id: ID,
    pub invoice_number: String,
    pub subtotal: String,
    pub discount_total: String,
    pub tax_amount: String,
    pub total_amount: String,
    pub currency: String,
    pub status: String,
    pub due_date: Option<NaiveDate>,
    pub sent_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<invoice::Model> for InvoiceDto {
    fn from(m: invoice::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            billing_cycle_id: ID(m.billing_cycle_id.to_string()),
            invoice_number: m.invoice_number,
            subtotal: m.subtotal.to_string(),
            discount_total: m.discount_total.to_string(),
            tax_amount: m.tax_amount.to_string(),
            total_amount: m.total_amount.to_string(),
            currency: m.currency,
            status: m.status,
            due_date: m.due_date,
            sent_at: m.sent_at,
            paid_at: m.paid_at,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Payment")]
pub struct PaymentDto {
    pub id: ID,
    pub invoice_id: ID,
    pub amount: String,
    pub payment_method: Option<String>,
    pub status: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub gateway_ref: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<payment::Model> for PaymentDto {
    fn from(m: payment::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            invoice_id: ID(m.invoice_id.to_string()),
            amount: m.amount.to_string(),
            payment_method: m.payment_method,
            status: m.status,
            paid_at: m.paid_at,
            gateway_ref: m.gateway_ref,
            failure_reason: m.failure_reason,
            created_at: m.created_at,
        }
    }
}

#[derive(InputObject, Debug)]
pub struct CreateOperatorUserInput {
    pub email: String,
    pub password: String,
    pub full_name: String,
    pub phone: Option<String>,
}

#[derive(InputObject, Debug)]
pub struct CreateInvoiceInput {
    pub tenant_id: ID,
    /// When omitted, uses or creates the current calendar month cycle for the tenant.
    pub billing_cycle_id: Option<ID>,
    pub subtotal: String,
    pub discount_total: Option<String>,
    pub tax_amount: Option<String>,
    pub total_amount: String,
    pub currency: String,
    pub status: Option<String>,
    pub due_date: Option<NaiveDate>,
}

#[derive(InputObject, Debug)]
pub struct RecordPaymentInput {
    pub invoice_id: ID,
    pub amount: String,
    pub payment_method: Option<String>,
    pub gateway_ref: Option<String>,
    pub status: Option<String>,
}
