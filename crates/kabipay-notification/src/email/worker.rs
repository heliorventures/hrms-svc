//! Isolated supervised email channel. SMTP failure never gates API readiness.
use super::{
    delivery_configuration,
    delivery_transport::{DeliveryRecipient, EmailRoutes, RouteFailure},
    destination::{
        resolve_smtp_destination, DestinationError, SystemSmtpResolver, ValidatedSmtpDestination,
    },
    keyring::configured_keyring,
    smtp::{
        send_email_attempt, FenceError, SmtpCredential, SmtpErrorCode, SmtpOutcome, SmtpTlsMode,
    },
    store::*,
    templates::{self, EmailSender, RenderedEmail},
};
use futures_util::FutureExt;
use kabipay_common::{
    db::{resolve_required_tenant_db, TenantDbCache, TenantDbConfig},
    important_email::*,
    KabiPayError, KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{watch, Semaphore};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct WorkerLimits {
    pub concurrency: usize,
    pub tick_interval: Duration,
    pub operation_timeout: Duration,
}
impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            concurrency: 8,
            tick_interval: Duration::from_secs(2),
            operation_timeout: Duration::from_secs(85),
        }
    }
}
/// One loop per allowlisted tenant, sharing a fair global semaphore. A timeout
/// or panic affects only its tenant; cancellation is observed even while queued.
pub async fn supervise_tenants<F, Fut>(
    tenants: Vec<Uuid>,
    limits: WorkerLimits,
    shutdown: watch::Receiver<bool>,
    tick: F,
) where
    F: Fn(Uuid) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    if tenants.len() > 100
        || limits.concurrency == 0
        || limits.concurrency > 8
        || limits.tick_interval.is_zero()
        || limits.operation_timeout.is_zero()
    {
        return;
    }
    let semaphore = Arc::new(Semaphore::new(limits.concurrency));
    let tick = Arc::new(tick);
    let mut jobs = tokio::task::JoinSet::new();
    for tenant in tenants {
        let semaphore = semaphore.clone();
        let tick = tick.clone();
        let mut stop = shutdown.clone();
        jobs.spawn(async move {
            let mut failures=0_u32;
            loop {
                if *stop.borrow() { break; }
                let permit=tokio::select! { biased;
                    _=stop.changed()=>break,
                    permit=semaphore.acquire()=>match permit { Ok(p)=>p,Err(_)=>break }
                };
                let operation=std::panic::AssertUnwindSafe(
                    tokio::time::timeout(limits.operation_timeout,tick(tenant))
                ).catch_unwind();
                let result=tokio::select! { biased;
                    _=stop.changed()=>{drop(permit);break;},
                    result=operation=>result
                };
                let succeeded=matches!(result,Ok(Ok(true)));
                failures=if succeeded { 0 } else { failures.saturating_add(1) };
                if !succeeded {
                    tracing::warn!(code="EMAIL_TENANT_WORK_DEFERRED",tenant=%tenant,"email channel work deferred");
                }
                // Release global capacity before a failing tenant backs off.
                drop(permit);
                let delay=limits.tick_interval.saturating_mul(1_u32<<failures.min(10)).min(Duration::from_secs(30));
                tokio::select! { biased;
                    _=stop.changed()=>break,
                    _=tokio::time::sleep(delay)=>{}
                }
            }
        });
    }
    while jobs.join_next().await.is_some() {}
}
#[derive(Clone)]
pub struct WorkerContext {
    pub ops: DatabaseConnection,
    pub cache: TenantDbCache,
    pub fallback: TenantDbConfig,
}
#[derive(Clone, Copy, Debug)]
pub enum PreparationFailure {
    Skip(DeliveryCode),
    Retry(DeliveryCode),
}
pub trait DeliveryBackend: Send + Sync {
    type Prepared: Send + Sync;
    fn prepare(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
    ) -> impl Future<Output = Result<Self::Prepared, PreparationFailure>> + Send;
    fn identity(prepared: &Self::Prepared) -> EmailRecipientIdentity;
    fn submit(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
        prepared: &Self::Prepared,
    ) -> impl Future<Output = SmtpOutcome> + Send;
}
/// The network preparation future is dropped safely on cancellation (no SMTP
/// exists yet). During submission, cancellation leaves a durable uncertain fence.
pub async fn process_attempt<B: DeliveryBackend>(
    db: &DatabaseConnection,
    attempt: &DeliveryAttempt,
    backend: &B,
    mut shutdown: watch::Receiver<bool>,
) -> KabiPayResult<()> {
    let prepared = backend.prepare(db, attempt);
    tokio::pin!(prepared);
    let deadline = tokio::time::sleep(Duration::from_secs(45));
    tokio::pin!(deadline);
    let mut heartbeat_tick = tokio::time::interval(Duration::from_secs(15));
    heartbeat_tick.tick().await;
    let result = loop {
        tokio::select! {
         biased;
         _=shutdown.changed()=>{let _=retry_email_attempt(db,attempt,SmtpErrorCode::Unavailable.as_str(),false).await;return Ok(());},
         _=&mut deadline=>break Err(PreparationFailure::Retry(DeliveryCode::RoutingUnavailable)),
         _=heartbeat_tick.tick()=>{
          let renewed=tokio::time::timeout(Duration::from_secs(3),heartbeat(db,attempt)).await;
          if !matches!(renewed,Ok(Ok(true))){return Ok(());}
         },
         result=&mut prepared=>break result,
        }
    };
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(PreparationFailure::Skip(code)) => {
            skip_email_attempt(db, attempt, code).await?;
            return Ok(());
        }
        Err(PreparationFailure::Retry(code)) => {
            retry_email_attempt(db, attempt, code.as_str(), false).await?;
            return Ok(());
        }
    };
    if reserve_identity(db, attempt, &B::identity(&prepared)).await? != IdentityReservation::Ready {
        return Ok(());
    }
    let outcome = tokio::select! {biased;_=shutdown.changed()=>{
     // Accepted-but-unrecorded or in-flight DATA is never returned to Pending.
     let uncertain=complete_email_attempt(db,attempt,SmtpOutcome::Uncertain(SmtpErrorCode::Uncertain)).await?;
     if !uncertain{retry_email_attempt(db,attempt,SmtpErrorCode::Unavailable.as_str(),false).await?;}
     return Ok(());
    },outcome=backend.submit(db,attempt,&prepared)=>outcome};
    complete_email_attempt(db, attempt, outcome).await?;
    Ok(())
}
struct ProductionBackend {
    context: WorkerContext,
}
pub struct PreparedEmail {
    routes: EmailRoutes,
    recipient: DeliveryRecipient,
    credential: SmtpCredential,
    destination: ValidatedSmtpDestination,
    mode: SmtpTlsMode,
    message: RenderedEmail,
}
fn route_failure(error: RouteFailure) -> PreparationFailure {
    match error {
        RouteFailure::Unavailable => PreparationFailure::Retry(DeliveryCode::RoutingUnavailable),
        RouteFailure::Obsolete => PreparationFailure::Skip(DeliveryCode::Obsolete),
        RouteFailure::Forbidden => PreparationFailure::Skip(DeliveryCode::Forbidden),
        RouteFailure::RecipientUnavailable => {
            PreparationFailure::Skip(DeliveryCode::RecipientUnavailable)
        }
    }
}
async fn active_tenant(ops: &DatabaseConnection, tenant: Uuid) -> KabiPayResult<Option<String>> {
    let row=ops.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT name FROM kabipay_ops.tenant WHERE id=$1 AND status='ACTIVE' AND NOT is_deleted",[tenant.into()])).await?;
    row.map(|row| row.try_get("", "name").map_err(KabiPayError::from))
        .transpose()
}
async fn current_company(
    context: &WorkerContext,
    attempt: &DeliveryAttempt,
) -> Result<String, PreparationFailure> {
    let retry = || PreparationFailure::Retry(DeliveryCode::RoutingUnavailable);
    let company = active_tenant(&context.ops, attempt.tenant)
        .await
        .map_err(|_| retry())?
        .ok_or(PreparationFailure::Skip(DeliveryCode::Disabled))?;
    let entitlements =
        kabipay_common::entitlements::Entitlements::load(&context.ops, attempt.tenant)
            .await
            .map_err(|_| retry())?;
    if entitlements.require("EMPLOYEE").is_err() {
        return Err(PreparationFailure::Skip(DeliveryCode::Disabled));
    }
    if let EmailRecipientRef::OrganizationPrincipal {
        source_tenant_id, ..
    } = attempt.event.recipient
    {
        context
            .cache
            .require_tenant(source_tenant_id)
            .map_err(|_| PreparationFailure::Skip(DeliveryCode::Forbidden))?;
        if active_tenant(&context.ops, source_tenant_id)
            .await
            .map_err(|_| retry())?
            .is_none()
        {
            return Err(PreparationFailure::Skip(DeliveryCode::Forbidden));
        }
    }
    Ok(company)
}
impl DeliveryBackend for ProductionBackend {
    type Prepared = PreparedEmail;
    async fn prepare(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
    ) -> Result<PreparedEmail, PreparationFailure> {
        let retry = PreparationFailure::Retry;
        let config = delivery_configuration::load(db, attempt.tenant)
            .await
            .map_err(|_| retry(DeliveryCode::RoutingUnavailable))?;
        config
            .admission(attempt)
            .map_err(PreparationFailure::Skip)?;
        let company = current_company(&self.context, attempt).await?;
        let origin = kabipay_common::db::company_public_origin(attempt.tenant)
            .map_err(|_| retry(DeliveryCode::RoutingUnavailable))?
            .ok_or(retry(DeliveryCode::RoutingUnavailable))?;
        let keys = configured_keyring()
            .await
            .map_err(|_| retry(DeliveryCode::KeyUnavailable))?;
        let secret = super::crypto::open_email_credential(
            attempt.tenant,
            config.settings.id,
            config.credential.as_ref().ok_or(PreparationFailure::Skip(
                DeliveryCode::CredentialUnavailable,
            ))?,
            &keys,
        )
        .map_err(|_| retry(DeliveryCode::CredentialUnavailable))?;
        let routes = EmailRoutes::configured()
            .await
            .map_err(|_| retry(DeliveryCode::RoutingUnavailable))?;
        let recipient = routes.resolve(attempt).await.map_err(route_failure)?;
        let sender = EmailSender::new(
            config.settings.sender_name,
            config.settings.sender_email,
            config.settings.account_email.clone(),
        )
        .map_err(|_| PreparationFailure::Skip(DeliveryCode::CredentialUnavailable))?;
        let message = match &recipient {
            DeliveryRecipient::Business(recipient) => {
                templates::render_email(&attempt.event, &company, &origin, &sender, recipient)
            }
            DeliveryRecipient::Test(_) => {
                templates::render_saved_account_test(&attempt.event, &company, &origin, &sender)
            }
        }
        .map_err(|_| PreparationFailure::Skip(DeliveryCode::RecipientUnavailable))?;
        let credential = SmtpCredential::new(config.settings.account_email, secret)
            .map_err(|_| PreparationFailure::Skip(DeliveryCode::CredentialUnavailable))?;
        let destination = resolve_smtp_destination(
            &config.settings.smtp_host,
            config.settings.smtp_port as u16,
            &SystemSmtpResolver,
        )
        .await
        .map_err(|error| match error {
            DestinationError::DnsUnavailable | DestinationError::NoAddress => {
                retry(DeliveryCode::RoutingUnavailable)
            }
            _ => PreparationFailure::Skip(DeliveryCode::DestinationRefused),
        })?;
        let mode = if config.settings.smtp_port == 465 {
            SmtpTlsMode::Implicit
        } else {
            SmtpTlsMode::RequiredStartTls
        };
        Ok(PreparedEmail {
            routes,
            recipient,
            credential,
            destination,
            mode,
            message,
        })
    }
    fn identity(prepared: &PreparedEmail) -> EmailRecipientIdentity {
        prepared.recipient.identity()
    }
    async fn submit(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
        prepared: &PreparedEmail,
    ) -> SmtpOutcome {
        send_email_attempt(
            &prepared.destination,
            prepared.mode,
            &prepared.credential,
            &prepared.message,
            || async {
                // Recheck eligibility/address next to body submission, after TLS/AUTH/DATA.
                current_company(&self.context, attempt)
                    .await
                    .map_err(|failure| match failure {
                        PreparationFailure::Retry(_) => FenceError::Unavailable,
                        PreparationFailure::Skip(_) => FenceError::Stale,
                    })?;
                let fresh = prepared.routes.resolve(attempt).await.map_err(|failure| {
                    if matches!(failure, RouteFailure::Unavailable) {
                        FenceError::Unavailable
                    } else {
                        FenceError::Stale
                    }
                })?;
                if !prepared.recipient.unchanged(&fresh) {
                    return Err(FenceError::Stale);
                }
                match mark_submitting(db, attempt).await {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(FenceError::Stale),
                    Err(_) => Err(FenceError::Unavailable),
                }
            },
        )
        .await
    }
}
pub fn email_worker_enabled(value: Option<&str>) -> bool {
    value == Some("true")
}
pub async fn run_email_worker(context: WorkerContext, shutdown: watch::Receiver<bool>) {
    if !email_worker_enabled(std::env::var("KABIPAY_EMAIL_WORKER_ENABLED").ok().as_deref()) {
        return;
    }
    let tenants = context.cache.configured_tenant_ids();
    let stop = shutdown.clone();
    supervise_tenants(tenants, WorkerLimits::default(), shutdown, move |tenant| {
        let context = context.clone();
        let stop = stop.clone();
        async move {
            let work = async {
                let db = resolve_required_tenant_db(
                    tenant,
                    &context.ops,
                    &context.cache,
                    &context.fallback,
                )
                .await?;
                // Missing/bad keys hold an enabled tenant before new leases are acquired.
                // Disabled work still drains to Skipped without any key/network dependency.
                let settings = super::settings::load_email_settings(&db, tenant).await?;
                if settings.enabled && settings.credential_present {
                    configured_keyring().await?.active_id(tenant)?;
                }
                let attempts = claim_email_batch(&db, tenant, 1).await?;
                let backend = ProductionBackend {
                    context: context.clone(),
                };
                for attempt in attempts {
                    process_attempt(&db, &attempt, &backend, stop.clone()).await?;
                }
                purge_terminal(&db, tenant).await?;
                Ok::<_, KabiPayError>(())
            }
            .await;
            work.is_ok()
        }
    })
    .await;
}
