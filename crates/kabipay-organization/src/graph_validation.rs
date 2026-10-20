//! Validate desired and possibly still installed source graphs. Incomplete
//! installations cannot hide a cycle behind a pending replacement.
use crate::store::*;
use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_domain::{
    validate_primary_reporting_graph, validate_reporting_transition_graph, EffectivePeriod,
    PrimaryReportingEdge,
};
use kabipay_organization_protocol::RelationshipInstallation;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;
fn edge(
    subject: Uuid,
    manager: Uuid,
    sp: Uuid,
    mp: Uuid,
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
) -> KabiPayResult<PrimaryReportingEdge> {
    Ok(PrimaryReportingEdge {
        subject_employment_id: subject.try_into().map_err(|_| denied())?,
        manager_employment_id: manager.try_into().map_err(|_| denied())?,
        subject_principal_id: sp.try_into().map_err(|_| denied())?,
        manager_principal_id: mp.try_into().map_err(|_| denied())?,
        period: EffectivePeriod::new(from, to).map_err(|_| denied())?,
    })
}
fn invalid() -> KabiPayError {
    KabiPayError::Validation(
        "primary reporting overlaps or forms a cycle in the current or reserved installation graph"
            .into(),
    )
}
pub(crate) fn validate_snapshots(
    desired: &[PrimaryReportingEdge],
    installed: &[PrimaryReportingEdge],
    legacy: &[PrimaryReportingEdge],
    now: DateTime<Utc>,
) -> KabiPayResult<()> {
    let mut boundaries = std::collections::BTreeSet::from([now]);
    for e in desired.iter().chain(installed) {
        let value = serde_json::to_value(e.period)?;
        let from: DateTime<Utc> = serde_json::from_value(value["from"].clone())?;
        let to: Option<DateTime<Utc>> = serde_json::from_value(value["to"].clone())?;
        if from >= now {
            boundaries.insert(from);
        }
        if let Some(to) = to {
            if to >= now {
                boundaries.insert(to);
            }
        }
    }
    for at in boundaries {
        let mut final_edges: Vec<_> = desired
            .iter()
            .filter(|e| e.period.contains(at))
            .cloned()
            .collect();
        let mut source_edges: Vec<_> = installed
            .iter()
            .filter(|e| e.period.contains(at))
            .cloned()
            .collect();
        for e in legacy {
            if !final_edges
                .iter()
                .any(|d| d.subject_employment_id == e.subject_employment_id)
            {
                final_edges.push(e.clone());
            }
            if !source_edges
                .iter()
                .any(|d| d.subject_employment_id == e.subject_employment_id)
            {
                source_edges.push(e.clone());
            }
        }
        validate_primary_reporting_graph(&final_edges, at).map_err(|_| invalid())?;
        validate_primary_reporting_graph(&source_edges, at).map_err(|_| invalid())?;
        // Duplicate identical old/new edges are harmless and must not imply two
        // distinct primary managers in this potential transition graph.
        source_edges.extend(final_edges);
        validate_reporting_transition_graph(&source_edges, at).map_err(|_| invalid())?;
    }
    Ok(())
}
pub(crate) async fn validate_graph(tx: &DatabaseTransaction, org: Uuid) -> KabiPayResult<()> {
    if tx.query_one(sql("SELECT id FROM kabipay_ops.organization_company WHERE organization_id=$1 AND status='DETACHING' LIMIT 1",vec![org.into()])).await?.is_some(){return Err(KabiPayError::ConflictRule{code:"ORGANIZATION_LIFECYCLE_CONFIRMATION_REQUIRED",message:"Confirm source company retirement before changing the reporting graph.".into()});}

    let missing=tx.query_one(sql("SELECT m.id FROM kabipay_ops.organization_company m LEFT JOIN kabipay_ops.organization_directory_sync s ON s.membership_id=m.id AND s.organization_id=m.organization_id WHERE m.organization_id=$1 AND m.status IN ('ACTIVE','SUSPENDED','DETACHING') AND (s.status IS DISTINCT FROM 'COMPLETE' OR (m.status='ACTIVE' AND s.membership_version<>m.version)) LIMIT 1",vec![org.into()])).await?;
    if missing.is_some() {
        return Err(KabiPayError::BusinessRule{code:"ORGANIZATION_DIRECTORY_SYNC_REQUIRED",message:"Complete directory synchronization for every joined company before changing the reporting graph. Resume and synchronize an incomplete suspended company, or detach it.".into()});
    }
    let rows=tx.query_all(sql("SELECT a.*,s.principal_id AS subject_principal,m.principal_id AS manager_principal FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref s ON s.id=a.subject_employment_ref_id AND s.organization_id=a.organization_id JOIN kabipay_ops.employment_ref m ON m.id=a.manager_employment_ref_id AND m.organization_id=a.organization_id JOIN kabipay_ops.organization_company c ON c.id=a.organization_company_id WHERE a.organization_id=$1 AND a.relationship_type='PRIMARY' AND c.status<>'DETACHED' ORDER BY a.id LIMIT 10001",vec![org.into()])).await?;
    if rows.len() > 10000 {
        return Err(KabiPayError::Validation(
            "reporting graph validation limit exceeded".into(),
        ));
    }
    let mut desired = Vec::new();
    let mut installed = Vec::new();
    for r in rows {
        let status: String = r.try_get("", "status")?;
        if matches!(status.as_str(), "ACTIVE" | "PENDING") {
            desired.push(edge(
                r.try_get("", "subject_employment_ref_id")?,
                r.try_get("", "manager_employment_ref_id")?,
                r.try_get("", "subject_principal")?,
                r.try_get("", "manager_principal")?,
                r.try_get("", "effective_from")?,
                r.try_get("", "effective_to")?,
            )?);
        }
        if let Some(value) =
            r.try_get::<Option<serde_json::Value>>("", "installed_configuration")?
        {
            let snapshot: RelationshipInstallation = serde_json::from_value(value)?;
            if snapshot.status == "ACTIVE" {
                installed.push(edge(
                    snapshot.subject_employment_ref_id,
                    snapshot.manager_employment_ref_id,
                    snapshot.subject_principal_id,
                    snapshot.manager_principal_id,
                    snapshot.effective_from,
                    snapshot.effective_to,
                )?);
            }
        }
    }
    let rows=tx.query_all(sql("SELECT s.id,s.principal_id,s.legacy_manager_employee_id,m.id AS manager_ref,m.principal_id AS manager_principal FROM kabipay_ops.employment_ref s JOIN kabipay_ops.organization_company c ON c.id=s.organization_company_id LEFT JOIN kabipay_ops.employment_ref m ON m.organization_id=s.organization_id AND m.organization_company_id=s.organization_company_id AND m.source_employee_id=s.legacy_manager_employee_id WHERE s.organization_id=$1 AND s.legacy_manager_employee_id IS NOT NULL AND c.status<>'DETACHED' ORDER BY s.id LIMIT 10001",vec![org.into()])).await?;
    if rows.len() > 10000 {
        return Err(KabiPayError::Validation(
            "reporting graph validation limit exceeded".into(),
        ));
    }
    let mut legacy = Vec::new();
    for r in rows {
        let manager: Option<Uuid> = r.try_get("", "manager_ref")?;
        legacy.push(edge(r.try_get("","id")?,manager.ok_or_else(||KabiPayError::Validation("legacy manager directory reference is incomplete; synchronize the company directory".into()))?,r.try_get("","principal_id")?,r.try_get("","manager_principal")?,DateTime::<Utc>::MIN_UTC,None)?);
    }
    validate_snapshots(&desired, &installed, &legacy, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(a: u128, b: u128) -> PrimaryReportingEdge {
        edge(
            Uuid::from_u128(a),
            Uuid::from_u128(b),
            Uuid::from_u128(a),
            Uuid::from_u128(b),
            Utc::now() - chrono::Duration::days(1),
            None,
        )
        .unwrap()
    }
    #[test]
    fn canonical_primary_replaces_legacy_for_validation() {
        let now = Utc::now();
        assert!(validate_snapshots(&[e(1, 3)], &[e(1, 3)], &[e(1, 2), e(2, 1)], now).is_ok());
    }
    #[test]
    fn legacy_cycles_cannot_be_hidden_before_source_installation() {
        let now = Utc::now();
        assert!(validate_snapshots(&[e(1, 3)], &[], &[e(1, 2), e(2, 1)], now).is_err());
    }
    #[test]
    fn partial_installation_cannot_hide_transition_cycle() {
        let now = Utc::now();
        assert!(validate_snapshots(&[e(1, 3), e(2, 1)], &[e(1, 2), e(2, 3)], &[], now).is_err());
    }
    #[test]
    fn atomic_same_assignment_replacement_has_safe_reserved_graph() {
        let now = Utc::now();
        assert!(validate_snapshots(&[e(1, 3)], &[e(1, 2)], &[], now).is_ok());
    }
}
