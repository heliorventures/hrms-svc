//! Source-owned membership fences. Only the verified private lifecycle adapter
//! may call install; callers must roll back their transaction on any error.
use crate::{statement, unavailable};
use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::{MembershipAcknowledgement, MembershipInstallation};
use kabipay_organization_security::command_body_hash;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;

fn conflict() -> KabiPayError {
    KabiPayError::Conflict(
        "Organization membership revision changed; reconcile before retrying".into(),
    )
}
fn operational_status(desired: &str) -> KabiPayResult<&str> {
    match desired {
        "ACTIVE" => Ok("ACTIVE"),
        "SUSPENDED" => Ok("SUSPENDED"),
        "DETACHING" => Ok("DETACHED"),
        _ => Err(unavailable()),
    }
}
fn validate(value: &MembershipInstallation, command: Uuid) -> KabiPayResult<()> {
    if [
        value.organization_id,
        value.membership_id,
        value.tenant_id,
        command,
    ]
    .iter()
    .any(Uuid::is_nil)
        || value.version <= 0
    {
        return Err(unavailable());
    }
    operational_status(&value.status)?;
    let mut canonical = value.clone();
    canonical.configuration_hash.clear();
    if value.configuration_hash.len() != 64
        || command_body_hash(&serde_json::to_vec(&canonical)?) != value.configuration_hash
    {
        return Err(unavailable());
    }
    Ok(())
}
fn acknowledgement(
    value: &MembershipInstallation,
    command_id: Uuid,
) -> KabiPayResult<MembershipAcknowledgement> {
    Ok(MembershipAcknowledgement {
        membership_id: value.membership_id,
        version: value.version,
        status: operational_status(&value.status)?.into(),
        configuration_hash: value.configuration_hash.clone(),
        command_id,
    })
}

/// Acquire before any receipt, resource, assignment or bootstrap-job row locks.
/// The SHARE lock survives until the caller commits/rolls back its transaction;
/// lifecycle installation drains these accepted operations with an UPDATE lock.
pub async fn require_active(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    membership: Uuid,
    organization: Uuid,
    version: i64,
) -> KabiPayResult<()> {
    if [tenant, membership, organization].iter().any(Uuid::is_nil) || version <= 0 {
        return Err(unavailable());
    }
    let row=tx.query_one(statement("SELECT organization_id,status,version FROM organization_membership_installation WHERE tenant_id=$1 AND membership_id=$2 FOR SHARE",vec![tenant.into(),membership.into()])).await?.ok_or_else(unavailable)?;
    if row.try_get::<Uuid>("", "organization_id")? != organization
        || row.try_get::<String>("", "status")? != "ACTIVE"
        || row.try_get::<i64>("", "version")? != version
    {
        return Err(unavailable());
    }
    Ok(())
}

/// Lifecycle adapters call this immediately after BEGIN, before command receipt
/// and actor locks. Business adapters acquire require_active first. Keeping the
/// fence before every shared resource avoids actor/fence lock inversion.
pub async fn lock_installation(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    membership: Uuid,
) -> KabiPayResult<()> {
    if tenant.is_nil() || membership.is_nil() {
        return Err(unavailable());
    }
    // Serialize first installations and overlapping company ownership changes.
    tx.execute(statement(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        vec![format!("organization-membership-company:{tenant}").into()],
    ))
    .await?;
    tx.query_one(statement("SELECT membership_id FROM organization_membership_installation WHERE tenant_id=$1 AND membership_id=$2 FOR UPDATE",vec![tenant.into(),membership.into()])).await?;
    Ok(())
}

/// Install an exact live-policy configuration inside the existing verified,
/// tenant-bound command/receipt transaction. This never commits or sends ACKs;
/// transport sends the returned ACK only after the source commit is confirmed.
pub async fn install(
    tx: &DatabaseTransaction,
    value: &MembershipInstallation,
    command_id: Uuid,
) -> KabiPayResult<MembershipAcknowledgement> {
    validate(value, command_id)?;
    let status = operational_status(&value.status)?;
    lock_installation(tx, value.tenant_id, value.membership_id).await?;
    // A command ID is tenant-wide, including concurrent first installations
    // for different memberships. Serialize before inspecting its receipt.
    tx.execute(statement(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        vec![format!(
            "organization-membership-command:{}:{}",
            value.tenant_id, command_id
        )
        .into()],
    ))
    .await?;
    if let Some(receipt)=tx.query_one(statement("SELECT membership_id,organization_id,version,desired_status,configuration_hash FROM organization_membership_lifecycle_receipt WHERE tenant_id=$1 AND command_id=$2 FOR UPDATE",vec![value.tenant_id.into(),command_id.into()])).await? {
        if receipt.try_get::<Uuid>("","membership_id")?!=value.membership_id || receipt.try_get::<Uuid>("","organization_id")?!=value.organization_id || receipt.try_get::<i64>("","version")?!=value.version || receipt.try_get::<String>("","desired_status")?!=value.status || receipt.try_get::<String>("","configuration_hash")?!=value.configuration_hash {return Err(conflict());}
    }
    let existing=tx.query_one(statement("SELECT organization_id,status,desired_status,version,configuration_hash FROM organization_membership_installation WHERE tenant_id=$1 AND membership_id=$2 FOR UPDATE",vec![value.tenant_id.into(),value.membership_id.into()])).await?;
    let mut same = false;
    if let Some(existing) = existing {
        let version: i64 = existing.try_get("", "version")?;
        if existing.try_get::<Uuid>("", "organization_id")? != value.organization_id
            || version > value.version
        {
            return Err(conflict());
        }
        if version == value.version {
            if existing.try_get::<String>("", "configuration_hash")? != value.configuration_hash
                || existing.try_get::<String>("", "desired_status")? != value.status
                || existing.try_get::<String>("", "status")? != status
            {
                return Err(conflict());
            }
            same = true;
        }
    }
    if !same {
        // The UPDATE lock above has drained every previously accepted command.
        // New commands wait, then observe this revision and operational status.
        tx.execute(statement("INSERT INTO organization_membership_installation(tenant_id,membership_id,organization_id,status,desired_status,version,configuration_hash,retirement_transaction_id) VALUES($1,$2,$3,$4,$5,$6,$7,CASE WHEN $4='DETACHED' THEN txid_current() ELSE NULL END) ON CONFLICT(tenant_id,membership_id) DO UPDATE SET status=excluded.status,desired_status=excluded.desired_status,version=excluded.version,configuration_hash=excluded.configuration_hash,retirement_transaction_id=excluded.retirement_transaction_id,installed_at=clock_timestamp() WHERE organization_membership_installation.organization_id=excluded.organization_id",vec![value.tenant_id.into(),value.membership_id.into(),value.organization_id.into(),status.into(),value.status.clone().into(),value.version.into(),value.configuration_hash.clone().into()])).await?;
        match value.status.as_str() {
            "ACTIVE" => {
                // Membership activation never silently revives old grant ACKs.
                tx.execute(statement("UPDATE organization_grant_installation SET status='SUSPENDED' WHERE tenant_id=$1 AND organization_id=$2 AND membership_version<>$3 AND status='ACTIVE'",vec![value.tenant_id.into(),value.organization_id.into(),value.version.into()])).await?;
                tx.execute(statement("INSERT INTO organization_fact_bootstrap(tenant_id,organization_id,phase,after_id,status) VALUES($1,$2,0,NULL,'PENDING') ON CONFLICT(tenant_id,organization_id) DO UPDATE SET phase=0,after_id=NULL,status='PENDING',updated_at=clock_timestamp()",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
            }
            "SUSPENDED" => {
                tx.execute(statement("UPDATE organization_grant_installation SET status='SUSPENDED' WHERE tenant_id=$1 AND organization_id=$2 AND status='ACTIVE'",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
                tx.execute(statement("INSERT INTO organization_fact_bootstrap(tenant_id,organization_id,status) VALUES($1,$2,'PAUSED') ON CONFLICT(tenant_id,organization_id) DO UPDATE SET status='PAUSED',updated_at=clock_timestamp()",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
            }
            "DETACHING" => retire(tx, value).await?,
            _ => return Err(unavailable()),
        }
    }
    tx.execute(statement("INSERT INTO organization_membership_lifecycle_receipt(tenant_id,command_id,membership_id,organization_id,version,desired_status,status,configuration_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant_id,command_id) DO NOTHING",vec![value.tenant_id.into(),command_id.into(),value.membership_id.into(),value.organization_id.into(),value.version.into(),value.status.clone().into(),status.into(),value.configuration_hash.clone().into()])).await?;
    acknowledgement(value, command_id)
}

async fn retire(tx: &DatabaseTransaction, value: &MembershipInstallation) -> KabiPayResult<()> {
    if tx.query_one(statement("SELECT membership_id FROM organization_membership_installation WHERE tenant_id=$1 AND membership_id<>$2 AND status IN ('ACTIVE','SUSPENDED','DETACHING') LIMIT 1",vec![value.tenant_id.into(),value.membership_id.into()])).await?.is_some() {
        return Err(KabiPayError::Conflict("Reconcile overlapping organization ownership before retiring company routing".into()));
    }
    // Preserve all source decisions, actors, capabilities and receipts. Only
    // current routing/authority/job state is retired; no DELETE is issued here.
    // Legacy Leave locks employee before canonical assignments. Local review
    // workflows lock cycle before participant/employee FK state. Drain both in
    // their existing order, not assignment -> employee (which can deadlock).
    // Lock all old-org subjects, including future relationships: the effective
    // interval may cross a boundary while waiting for a legacy transaction.
    tx.query_all(statement("SELECT c.id FROM review_cycle c WHERE c.tenant_id=$1 AND EXISTS(SELECT 1 FROM performance_participant p JOIN management_assignment a ON a.tenant_id=p.tenant_id AND a.subject_employee_id=p.employee_id WHERE p.tenant_id=$1 AND p.review_cycle_id=c.id AND a.organization_id=$2) ORDER BY c.id FOR UPDATE",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.query_all(statement("SELECT e.id FROM employee e WHERE e.tenant_id=$1 AND EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=$1 AND a.organization_id=$2 AND (a.subject_employee_id=e.id OR (a.manager_tenant_id=$1 AND a.manager_employee_id=e.id))) ORDER BY e.id FOR UPDATE",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.query_all(statement("SELECT p.id FROM performance_participant p WHERE p.tenant_id=$1 AND p.manager_submitted_at IS NULL AND EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=p.tenant_id AND a.organization_id=$2 AND a.subject_employee_id=p.employee_id) ORDER BY p.id FOR UPDATE",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.query_all(statement("SELECT id FROM management_assignment WHERE tenant_id=$1 AND organization_id=$2 ORDER BY id FOR UPDATE",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    let retired_at: DateTime<Utc> = tx
        .query_one(statement("SELECT clock_timestamp() AS at", vec![]))
        .await?
        .ok_or_else(unavailable)?
        .try_get("", "at")?;
    if tx.query_one(statement("SELECT a.id FROM management_assignment a LEFT JOIN employee manager ON manager.tenant_id=a.tenant_id AND manager.id=a.manager_employee_id AND NOT manager.is_deleted WHERE a.tenant_id=$1 AND a.organization_id=$2 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=$3 AND (a.effective_to IS NULL OR $3<a.effective_to) AND a.manager_tenant_id=a.tenant_id AND (manager.id IS NULL OR manager.id=a.subject_employee_id) LIMIT 1",vec![value.tenant_id.into(),value.organization_id.into(),retired_at.into()])).await?.is_some(){return Err(KabiPayError::Conflict("Reconcile the actual local manager reference before retiring organization ownership".into()));}
    let previous: Option<String> = tx
        .query_one(statement(
            "SELECT current_setting('kabipay.organization_retirement',true) AS value",
            vec![],
        ))
        .await?
        .ok_or_else(unavailable)?
        .try_get("", "value")?;
    let context = format!(
        "{}:{}:{}",
        value.tenant_id, value.organization_id, value.membership_id
    );
    tx.execute(statement(
        "SELECT set_config('kabipay.organization_retirement',$1,true)",
        vec![context.into()],
    ))
    .await?;
    // Foreign current managers become NULL. Same-company canonical managers are
    // real employee IDs already checked above, never fabricated local actors.
    tx.execute(statement("UPDATE employee subject SET reporting_manager_id=CASE WHEN a.manager_tenant_id=a.tenant_id THEN a.manager_employee_id ELSE NULL END,updated_at=clock_timestamp() FROM management_assignment a WHERE a.tenant_id=$1 AND a.organization_id=$2 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=$3 AND (a.effective_to IS NULL OR $3<a.effective_to) AND subject.tenant_id=a.tenant_id AND subject.id=a.subject_employee_id AND NOT subject.is_deleted",vec![value.tenant_id.into(),value.organization_id.into(),retired_at.into()])).await?;
    // Pending reviews must not fall back to an old pre-organization manager
    // snapshot after canonical ownership ends. Submitted manager history stays
    // immutable; foreign current managers intentionally restore NULL routing.
    tx.execute(statement("UPDATE performance_participant p SET manager_employee_id=subject.reporting_manager_id,updated_at=clock_timestamp() FROM employee subject WHERE p.tenant_id=$1 AND p.tenant_id=subject.tenant_id AND p.employee_id=subject.id AND p.manager_submitted_at IS NULL AND NOT subject.is_deleted AND EXISTS(SELECT 1 FROM management_assignment a WHERE a.tenant_id=p.tenant_id AND a.organization_id=$2 AND a.subject_employee_id=p.employee_id AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=$3 AND (a.effective_to IS NULL OR $3<a.effective_to))",vec![value.tenant_id.into(),value.organization_id.into(),retired_at.into()])).await?;
    tx.execute(statement("UPDATE management_assignment SET status='ENDED',updated_at=clock_timestamp() WHERE tenant_id=$1 AND organization_id=$2 AND status<>'ENDED'",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.execute(statement("UPDATE organization_grant_installation SET status='REVOKED' WHERE tenant_id=$1 AND organization_id=$2",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.execute(statement("INSERT INTO organization_fact_bootstrap(tenant_id,organization_id,status) VALUES($1,$2,'STOPPED') ON CONFLICT(tenant_id,organization_id) DO UPDATE SET status='STOPPED',updated_at=clock_timestamp()",vec![value.tenant_id.into(),value.organization_id.into()])).await?;
    tx.execute(statement(
        "SELECT set_config('kabipay.organization_retirement',$1,true)",
        vec![previous.unwrap_or_default().into()],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configuration(status: &str) -> MembershipInstallation {
        let mut value = MembershipInstallation {
            organization_id: Uuid::new_v4(),
            membership_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            status: status.into(),
            version: 1,
            configuration_hash: String::new(),
        };
        value.configuration_hash =
            command_body_hash(&serde_json::to_vec(&value).expect("configuration"));
        value
    }
    #[test]
    fn configuration_is_exact_and_detach_ack_is_terminal_without_revision_change() {
        let value = configuration("DETACHING");
        let command = Uuid::new_v4();
        validate(&value, command).expect("valid configuration");
        let ack = acknowledgement(&value, command).expect("ack");
        assert_eq!(ack.status, "DETACHED");
        assert_eq!(ack.version, value.version);
        assert_eq!(ack.configuration_hash, value.configuration_hash);
        let mut invalid = value.clone();
        invalid.status = "ACTIVE".into();
        assert!(validate(&invalid, command).is_err());
        assert!(validate(&configuration("DETACHED"), command).is_err());
        assert!(validate(&value, Uuid::nil()).is_err());
    }

    /// Explicitly opt in with an isolated localhost PostgreSQL database. Applies
    /// the real 0117 SQL in a fresh schema; never touches existing source tables.
    #[tokio::test]
    #[ignore = "requires ORGANIZATION_LIFECYCLE_TEST_DATABASE_URL isolated PostgreSQL"]
    async fn postgres_lifecycle_drains_preserves_and_retires() {
        use sea_orm::{ConnectOptions, Database, DatabaseConnection, TransactionTrait};
        use std::time::Duration;
        async fn connection(url: &str, schema: &str) -> DatabaseConnection {
            let mut options = ConnectOptions::new(url);
            options.max_connections(1).sqlx_logging(false);
            let db = Database::connect(options)
                .await
                .expect("isolated connection");
            db.execute_unprepared(&format!("SET search_path TO {schema},public"))
                .await
                .expect("fixed test schema");
            db
        }
        let url = std::env::var("ORGANIZATION_LIFECYCLE_TEST_DATABASE_URL")
            .expect("explicit synthetic URL");
        assert!(url.starts_with("postgres://acceptance:synthetic-only-password@127.0.0.1:15436/organization_acceptance"), "only the dedicated synthetic acceptance database is permitted");
        let schema = format!("lifecycle_{}", Uuid::new_v4().simple());
        let db = connection(&url, "public").await;
        db.execute_unprepared(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema},public"
        ))
        .await
        .expect("new isolated schema");
        db.execute_unprepared("CREATE TABLE organization_grant_installation(tenant_id uuid,organization_id uuid,membership_version bigint); CREATE TABLE organization_fact_bootstrap(tenant_id uuid,organization_id uuid,phase integer NOT NULL DEFAULT 0,after_id uuid,status text NOT NULL DEFAULT 'PENDING' CHECK(status IN ('PENDING','COMPLETE')),updated_at timestamptz DEFAULT now(),PRIMARY KEY(tenant_id,organization_id)); CREATE TABLE employee(tenant_id uuid,id uuid,reporting_manager_id uuid,is_deleted boolean DEFAULT false,updated_at timestamptz DEFAULT now(),PRIMARY KEY(tenant_id,id)); CREATE TABLE management_assignment(id uuid PRIMARY KEY,tenant_id uuid,organization_id uuid,subject_employee_id uuid,manager_employee_id uuid,manager_tenant_id uuid,relationship_type text,status text,effective_from timestamptz,effective_to timestamptz,updated_at timestamptz DEFAULT now());").await.expect("representative source prerequisites");
        db.execute_unprepared("CREATE TABLE review_cycle(id uuid PRIMARY KEY,tenant_id uuid NOT NULL); CREATE TABLE performance_participant(id uuid PRIMARY KEY,tenant_id uuid NOT NULL,review_cycle_id uuid NOT NULL REFERENCES review_cycle(id),employee_id uuid NOT NULL,manager_employee_id uuid,manager_submitted_at timestamptz,updated_at timestamptz DEFAULT now(),FOREIGN KEY(tenant_id,employee_id) REFERENCES employee(tenant_id,id),FOREIGN KEY(tenant_id,manager_employee_id) REFERENCES employee(tenant_id,id));").await.expect("review routing prerequisites");
        let xml = include_str!("../../../../hrms-database/changelog/migrations/0117_organization_membership_lifecycle/organization_membership_lifecycle.xml");
        let sql = xml
            .split("<![CDATA[")
            .nth(1)
            .expect("actual DDL")
            .split("]]>")
            .next()
            .expect("DDL end")
            .replace("${schema}", &schema);
        db.execute_unprepared(&sql)
            .await
            .expect("actual migration0117");
        db.execute_unprepared("CREATE FUNCTION manager_guard() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.reporting_manager_id IS DISTINCT FROM OLD.reporting_manager_id AND organization_reporting_owned(NEW.tenant_id) AND NOT organization_reporting_retirement_allowed(NEW.tenant_id) THEN RAISE EXCEPTION 'organization owns manager'; END IF; RETURN NEW; END $$; CREATE TRIGGER manager_guard BEFORE UPDATE ON employee FOR EACH ROW EXECUTE FUNCTION manager_guard()").await.expect("source ownership guard contract");
        let mut value = configuration("ACTIVE");
        let command = Uuid::new_v4();
        let tx = db.begin().await.expect("activate");
        install(&tx, &value, command).await.expect("active install");
        tx.commit().await.expect("active commit");
        db.execute(statement("INSERT INTO organization_grant_installation(tenant_id,organization_id,membership_version) VALUES($1,$2,1)",vec![value.tenant_id.into(),value.organization_id.into()])).await.expect("installed grant");
        let manager = Uuid::new_v4();
        let own = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        for id in [manager, own, foreign] {
            db.execute(statement(
                "INSERT INTO employee(tenant_id,id,reporting_manager_id) VALUES($1,$2,$3)",
                vec![
                    value.tenant_id.into(),
                    id.into(),
                    (if id == foreign { Some(manager) } else { None }).into(),
                ],
            ))
            .await
            .expect("employee");
        }
        for (subject, manager_tenant, manager_employee, future) in [
            (own, value.tenant_id, Some(manager), false),
            (foreign, Uuid::new_v4(), None, false),
            (manager, value.tenant_id, Some(own), true),
        ] {
            db.execute(statement("INSERT INTO management_assignment(id,tenant_id,organization_id,subject_employee_id,manager_tenant_id,manager_employee_id,relationship_type,status,effective_from) VALUES($1,$2,$3,$4,$5,$6,'PRIMARY','ACTIVE',clock_timestamp()+CASE WHEN $7 THEN interval '1 day' ELSE interval '-1 day' END)",vec![Uuid::new_v4().into(),value.tenant_id.into(),value.organization_id.into(),subject.into(),manager_tenant.into(),manager_employee.into(),future.into()])).await.expect("canonical assignments");
        }
        let cycle = Uuid::new_v4();
        let history_cycle = Uuid::new_v4();
        for id in [cycle, history_cycle] {
            db.execute(statement(
                "INSERT INTO review_cycle(id,tenant_id) VALUES($1,$2)",
                vec![id.into(), value.tenant_id.into()],
            ))
            .await
            .expect("review cycle");
        }
        let own_review = Uuid::new_v4();
        let foreign_review = Uuid::new_v4();
        let submitted_review = Uuid::new_v4();
        let future_review = Uuid::new_v4();
        for (id, employee, review_cycle, old_manager, submitted) in [
            (own_review, own, cycle, foreign, false),
            (foreign_review, foreign, cycle, manager, false),
            (submitted_review, own, history_cycle, foreign, true),
            (future_review, manager, cycle, own, false),
        ] {
            db.execute(statement("INSERT INTO performance_participant(id,tenant_id,review_cycle_id,employee_id,manager_employee_id,manager_submitted_at) VALUES($1,$2,$3,$4,$5,CASE WHEN $6 THEN clock_timestamp() ELSE NULL END)",vec![id.into(),value.tenant_id.into(),review_cycle.into(),employee.into(),old_manager.into(),submitted.into()])).await.expect("review manager snapshot");
        }
        let held = db.begin().await.expect("accepted business transaction");
        require_active(
            &held,
            value.tenant_id,
            value.membership_id,
            value.organization_id,
            1,
        )
        .await
        .expect("shared fence");
        value.status = "SUSPENDED".into();
        value.version = 2;
        value.configuration_hash.clear();
        value.configuration_hash =
            command_body_hash(&serde_json::to_vec(&value).expect("canonical"));
        let second = connection(&url, &schema).await;
        let suspended = value.clone();
        let mut pending = tokio::spawn(async move {
            let tx = second.begin().await.expect("lifecycle");
            let ack = install(&tx, &suspended, Uuid::new_v4())
                .await
                .expect("suspend");
            tx.commit().await.expect("suspend commit");
            ack
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut pending)
                .await
                .is_err(),
            "exclusive lifecycle must wait for accepted operation"
        );
        held.commit().await.expect("drain accepted operation");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .expect("drained")
                .expect("task")
                .status,
            "SUSPENDED"
        );
        let tx = db.begin().await.expect("suspended checks");
        assert!(require_active(
            &tx,
            value.tenant_id,
            value.membership_id,
            value.organization_id,
            2
        )
        .await
        .is_err());
        assert_eq!(
            tx.query_one(statement(
                "SELECT count(*) AS n FROM management_assignment WHERE status='ACTIVE'",
                vec![]
            ))
            .await
            .expect("assignments")
            .expect("count")
            .try_get::<i64>("", "n")
            .expect("count"),
            3
        );
        tx.rollback().await.expect("rollback check");
        let state=db.query_one(statement("SELECT organization_reporting_owned($1) AS owned,organization_membership_active($1,$2) AS active,(SELECT status FROM organization_fact_bootstrap) AS bootstrap,(SELECT status FROM organization_grant_installation) AS grant",vec![value.tenant_id.into(),value.organization_id.into()])).await.expect("suspended state").expect("state");
        assert!(state.try_get::<bool>("", "owned").expect("owned"));
        assert!(!state.try_get::<bool>("", "active").expect("active"));
        assert_eq!(
            state.try_get::<String>("", "bootstrap").expect("bootstrap"),
            "PAUSED"
        );
        assert_eq!(
            state.try_get::<String>("", "grant").expect("grant"),
            "SUSPENDED"
        );
        value.status = "DETACHING".into();
        value.version = 3;
        value.configuration_hash.clear();
        value.configuration_hash =
            command_body_hash(&serde_json::to_vec(&value).expect("canonical"));
        let detach_command = Uuid::new_v4();
        let local_review = connection(&url, &schema).await;
        let review_tx = local_review.begin().await.expect("legacy review");
        review_tx
            .query_one(statement(
                "SELECT id FROM review_cycle WHERE id=$1 FOR UPDATE",
                vec![cycle.into()],
            ))
            .await
            .expect("legacy cycle first");
        review_tx
            .query_one(statement(
                "SELECT id FROM performance_participant WHERE id=$1 FOR UPDATE",
                vec![own_review.into()],
            ))
            .await
            .expect("legacy participant");
        let local_leave = connection(&url, &schema).await;
        let leave_tx = local_leave.begin().await.expect("legacy leave");
        leave_tx
            .query_one(statement(
                "SELECT id FROM employee WHERE id=$1 FOR UPDATE",
                vec![own.into()],
            ))
            .await
            .expect("legacy employee first");
        let retirement = connection(&url, &schema).await;
        let retiring = value.clone();
        let mut pending = tokio::spawn(async move {
            let tx = retirement.begin().await.expect("detach");
            let ack = install(&tx, &retiring, detach_command)
                .await
                .expect("retire");
            tx.commit().await.expect("detach commit");
            ack
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut pending)
                .await
                .is_err(),
            "retirement waits for legacy cycle lock"
        );
        tokio::time::timeout(
            Duration::from_secs(2),
            review_tx.query_one(statement(
                "SELECT id FROM employee WHERE id=$1 FOR KEY SHARE",
                vec![manager.into()],
            )),
        )
        .await
        .expect("cycle owner may finish employee FK lookup")
        .expect("employee lookup");
        tokio::time::timeout(
            Duration::from_secs(2),
            review_tx.query_one(statement(
                "SELECT id FROM management_assignment WHERE subject_employee_id=$1 FOR SHARE",
                vec![own.into()],
            )),
        )
        .await
        .expect("retirement must not lock assignment before review cycle")
        .expect("review assignment read");
        review_tx.commit().await.expect("legacy review completes");
        tokio::time::timeout(
            Duration::from_secs(2),
            leave_tx.query_one(statement(
                "SELECT id FROM management_assignment WHERE subject_employee_id=$1 FOR SHARE",
                vec![own.into()],
            )),
        )
        .await
        .expect("retirement must not lock assignment before legacy employee")
        .expect("leave assignment read");
        leave_tx.commit().await.expect("legacy leave completes");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .expect("legacy operations drain without deadlock")
                .expect("retirement task")
                .status,
            "DETACHED"
        );
        let own_manager = db
            .query_one(statement(
                "SELECT reporting_manager_id FROM employee WHERE id=$1",
                vec![own.into()],
            ))
            .await
            .expect("local manager")
            .expect("employee")
            .try_get::<Option<Uuid>>("", "reporting_manager_id")
            .expect("manager id");
        assert_eq!(own_manager, Some(manager));
        for (id, expected) in [
            (own_review, Some(manager)),
            (foreign_review, None),
            (submitted_review, Some(foreign)),
            (future_review, Some(own)),
        ] {
            let actual = db
                .query_one(statement(
                    "SELECT manager_employee_id FROM performance_participant WHERE id=$1",
                    vec![id.into()],
                ))
                .await
                .expect("review snapshot")
                .expect("participant")
                .try_get::<Option<Uuid>>("", "manager_employee_id")
                .expect("manager snapshot");
            assert_eq!(actual,expected,"pending current review routing is restored; submitted and future snapshots remain intact");
        }
        let state=db.query_one(statement("SELECT organization_reporting_owned($1) AS owned,(SELECT status FROM organization_grant_installation) AS grant,(SELECT count(*) FROM organization_membership_lifecycle_receipt) AS receipts",vec![value.tenant_id.into()])).await.expect("retired state").expect("state");
        assert!(!state
            .try_get::<bool>("", "owned")
            .expect("released ownership"));
        assert_eq!(
            state.try_get::<String>("", "grant").expect("revoked grant"),
            "REVOKED"
        );
        assert_eq!(
            state
                .try_get::<i64>("", "receipts")
                .expect("permanent receipts"),
            3
        );
        for id in [foreign, manager] {
            assert_eq!(
                db.query_one(statement(
                    "SELECT reporting_manager_id FROM employee WHERE id=$1",
                    vec![id.into()]
                ))
                .await
                .expect("routing")
                .expect("employee")
                .try_get::<Option<Uuid>>("", "reporting_manager_id")
                .expect("manager"),
                None,
                "foreign current clears; future assignment is not copied"
            );
        }
        assert_eq!(
            db.query_one(statement(
                "SELECT status FROM organization_fact_bootstrap",
                vec![]
            ))
            .await
            .expect("bootstrap")
            .expect("job")
            .try_get::<String>("", "status")
            .expect("status"),
            "STOPPED"
        );
        assert_eq!(
            db.query_one(statement(
                "SELECT count(*) AS n FROM management_assignment WHERE status<>'ENDED'",
                vec![]
            ))
            .await
            .expect("ended")
            .expect("count")
            .try_get::<i64>("", "n")
            .expect("count"),
            0
        );
        let tx = db.begin().await.expect("replay");
        assert_eq!(
            install(&tx, &value, detach_command)
                .await
                .expect("exact replay")
                .version,
            3
        );
        let mut stale = value.clone();
        stale.version = 2;
        stale.configuration_hash.clear();
        stale.configuration_hash =
            command_body_hash(&serde_json::to_vec(&stale).expect("canonical"));
        assert!(
            install(&tx, &stale, detach_command).await.is_err(),
            "command ID cannot be reused for a different canonical revision"
        );
        assert!(install(&tx, &stale, Uuid::new_v4()).await.is_err());
        tx.rollback().await.expect("rollback checks");
        // A copied GUC from an old retirement cannot authorize another transaction.
        db.execute(statement(
            "SELECT set_config('kabipay.organization_retirement',$1,false)",
            vec![format!(
                "{}:{}:{}",
                value.tenant_id, value.organization_id, value.membership_id
            )
            .into()],
        ))
        .await
        .expect("stale tuple");
        assert!(!db
            .query_one(statement(
                "SELECT organization_reporting_retirement_allowed($1) AS allowed",
                vec![value.tenant_id.into()]
            ))
            .await
            .expect("stale context")
            .expect("bool")
            .try_get::<bool>("", "allowed")
            .expect("bool"));
        db.execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .expect("remove only fresh synthetic schema");
    }
}
