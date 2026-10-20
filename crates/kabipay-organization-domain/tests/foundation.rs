use chrono::{TimeZone, Utc};
use kabipay_organization_domain::*;
use serde_json::json;
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
fn at(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, hour, 0, 0).unwrap()
}
fn period() -> EffectivePeriod {
    EffectivePeriod::new(at(8), Some(at(18))).unwrap()
}
fn version(value: u64) -> ResourceVersion {
    ResourceVersion::try_from(value).unwrap()
}

#[test]
fn identity_and_version_parsing_rejects_invalid_serialized_values() {
    assert_eq!(
        TenantId::try_from(Uuid::nil()),
        Err(OrganizationError::InvalidIdentifier)
    );
    assert!(serde_json::from_value::<PrincipalId>(json!(Uuid::nil())).is_err());
    assert_eq!(
        ResourceVersion::try_from(0),
        Err(OrganizationError::InvalidVersion)
    );
    assert_eq!(
        ResourceVersion::try_from(u64::MAX),
        Err(OrganizationError::InvalidVersion)
    );
    assert!(serde_json::from_value::<ResourceVersion>(json!(0)).is_err());
    let tenant = TenantId::try_from(id(1)).unwrap();
    assert_eq!(serde_json::to_value(tenant).unwrap(), json!(id(1)));
}

#[test]
fn capabilities_preserve_canonical_vocabulary_and_reject_ambiguous_input() {
    for valid in [
        "leave:approve",
        "payroll:manage",
        "loan:exit-review",
        "employee_directory:read",
    ] {
        assert_eq!(Capability::parse(valid).unwrap().as_str(), valid);
    }
    for invalid in [
        "",
        "leave",
        "leave:approve:all",
        "Leave:approve",
        " leave:approve",
        "leave:approve ",
        "leave:*",
        "leave:",
    ] {
        assert!(Capability::parse(invalid).is_err(), "{invalid}");
        assert!(serde_json::from_value::<Capability>(json!(invalid)).is_err());
    }
}

#[test]
fn effective_period_is_half_open_and_invalid_deserialization_fails() {
    let p = period();
    assert!(!p.contains(at(7)));
    assert!(p.contains(at(8)));
    assert!(p.contains(at(17)));
    assert!(!p.contains(at(18)));
    assert!(EffectivePeriod::new(at(8), Some(at(8))).is_err());
    assert!(EffectivePeriod::new(at(9), Some(at(8))).is_err());
    assert!(serde_json::from_value::<EffectivePeriod>(json!({"from":at(9),"to":at(8)})).is_err());
    assert!(EffectivePeriod::new(at(8), None).unwrap().contains(at(23)));
}

fn grant() -> CompanyGrant {
    CompanyGrant {
        organization_id: OrganizationId::try_from(id(1)).unwrap(),
        membership_id: MembershipId::try_from(id(2)).unwrap(),
        tenant_id: TenantId::try_from(id(3)).unwrap(),
        principal_id: PrincipalId::try_from(id(4)).unwrap(),
        capability: Capability::parse("employee:manage").unwrap(),
        scope: EmployeeScope::All,
        period: period(),
        version: version(2),
        active: true,
    }
}
fn security() -> GrantSecurityState {
    GrantSecurityState {
        organization_active: true,
        membership_active: true,
        identity_eligible: true,
        shared_version: version(2),
        accepted_version: version(2),
    }
}
fn request(grant: &CompanyGrant) -> GrantRequest<'_> {
    GrantRequest {
        organization_id: grant.organization_id,
        membership_id: grant.membership_id,
        tenant_id: grant.tenant_id,
        principal_id: grant.principal_id,
        capability: &grant.capability,
    }
}

#[test]
fn organization_grant_requires_exact_company_principal_membership_and_capability() {
    let g = grant();
    assert_eq!(
        g.authorize(&request(&g), &security(), at(12)),
        Ok(EmployeeScope::All)
    );
    let mut r = request(&g);
    r.tenant_id = TenantId::try_from(id(30)).unwrap();
    assert_eq!(
        g.authorize(&r, &security(), at(12)),
        Err(OrganizationError::OutsideGrant)
    );
    let mut r = request(&g);
    r.principal_id = PrincipalId::try_from(id(40)).unwrap();
    assert!(g.authorize(&r, &security(), at(12)).is_err());
    let mut r = request(&g);
    r.membership_id = MembershipId::try_from(id(20)).unwrap();
    assert!(g.authorize(&r, &security(), at(12)).is_err());
    let mut r = request(&g);
    r.organization_id = OrganizationId::try_from(id(10)).unwrap();
    assert!(g.authorize(&r, &security(), at(12)).is_err());
    let payroll = Capability::parse("payroll:manage").unwrap();
    let mut r = request(&g);
    r.capability = &payroll;
    assert!(
        g.authorize(&r, &security(), at(12)).is_err(),
        "HR authority does not imply payroll"
    );
}

#[test]
fn revoked_expired_ineligible_and_stale_grants_fail_closed() {
    let g = grant();
    for state in [
        GrantSecurityState {
            organization_active: false,
            ..security()
        },
        GrantSecurityState {
            membership_active: false,
            ..security()
        },
        GrantSecurityState {
            identity_eligible: false,
            ..security()
        },
        GrantSecurityState {
            shared_version: version(3),
            ..security()
        },
        GrantSecurityState {
            accepted_version: version(1),
            ..security()
        },
    ] {
        assert_eq!(
            g.authorize(&request(&g), &state, at(12)),
            Err(OrganizationError::AuthorityUnavailable)
        );
    }
    assert!(g.authorize(&request(&g), &security(), at(18)).is_err());
    let inactive = CompanyGrant {
        active: false,
        ..g.clone()
    };
    assert!(inactive
        .authorize(&request(&g), &security(), at(12))
        .is_err());
    let scoped = CompanyGrant {
        scope: EmployeeScope::Team,
        ..g.clone()
    };
    assert_eq!(
        scoped.authorize(&request(&g), &security(), at(12)),
        Ok(EmployeeScope::Team)
    );
}

fn edge(subject: u128, manager: u128) -> PrimaryReportingEdge {
    PrimaryReportingEdge {
        subject_employment_id: EmploymentId::try_from(id(subject)).unwrap(),
        manager_employment_id: EmploymentId::try_from(id(manager)).unwrap(),
        subject_principal_id: PrincipalId::try_from(id(subject + 100)).unwrap(),
        manager_principal_id: PrincipalId::try_from(id(manager + 100)).unwrap(),
        period: period(),
    }
}

#[test]
fn primary_reporting_accepts_cross_company_chains_but_rejects_cycles_and_multiple_managers() {
    assert!(validate_primary_reporting_graph(&[edge(1, 2), edge(2, 3)], at(12)).is_ok());
    assert_eq!(
        validate_primary_reporting_graph(&[edge(1, 2), edge(2, 1)], at(12)),
        Err(OrganizationError::ReportingCycle)
    );
    assert_eq!(
        validate_primary_reporting_graph(&[edge(1, 2), edge(2, 3), edge(3, 1)], at(12)),
        Err(OrganizationError::ReportingCycle)
    );
    assert_eq!(
        validate_primary_reporting_graph(&[edge(1, 2), edge(1, 3)], at(12)),
        Err(OrganizationError::MultiplePrimaryManagers)
    );
    assert_eq!(
        validate_primary_reporting_graph(&[edge(1, 1)], at(12)),
        Err(OrganizationError::SelfReporting)
    );
    let mut same_person = edge(1, 2);
    same_person.manager_principal_id = same_person.subject_principal_id;
    assert_eq!(
        validate_primary_reporting_graph(&[same_person], at(12)),
        Err(OrganizationError::SelfReporting)
    );
}

#[test]
fn primary_graph_uses_effective_time_and_rejects_inconsistent_person_identity() {
    let mut old = edge(1, 2);
    old.period = EffectivePeriod::new(at(8), Some(at(12))).unwrap();
    let mut new = edge(1, 3);
    new.period = EffectivePeriod::new(at(12), None).unwrap();
    assert!(validate_primary_reporting_graph(&[old, new], at(12)).is_ok());
    let mut inconsistent = edge(2, 3);
    inconsistent.subject_principal_id = PrincipalId::try_from(id(900)).unwrap();
    assert!(validate_primary_reporting_graph(&[edge(1, 2), inconsistent], at(12)).is_err());
}

#[test]
fn multiple_employments_cannot_hide_a_person_reporting_cycle() {
    let first = edge(1, 2);
    let mut second = edge(3, 4);
    second.subject_principal_id = first.manager_principal_id;
    second.manager_principal_id = first.subject_principal_id;
    assert_eq!(
        validate_primary_reporting_graph(&[first, second], at(12)),
        Err(OrganizationError::ReportingCycle)
    );
}
#[test]
fn transition_union_allows_manager_switches_but_not_cycles_during_partial_installation() {
    assert!(validate_reporting_transition_graph(&[edge(1, 2), edge(1, 3)], at(12)).is_ok());
    assert_eq!(
        validate_reporting_transition_graph(&[edge(1, 2), edge(1, 3), edge(2, 1)], at(12)),
        Err(OrganizationError::ReportingCycle)
    );
}

fn header_input() -> BusinessEventHeaderInput {
    BusinessEventHeaderInput {
        event_id: id(10),
        event_type: "LeaveApproved".into(),
        schema_version: 1,
        organization_id: id(1),
        organization_membership_id: id(2),
        source_tenant_id: id(3),
        aggregate_type: "leave_request".into(),
        aggregate_id: id(4),
        source_version: 3,
        actor: EventActor::Person {
            principal_id: PrincipalId::try_from(id(5)).unwrap(),
            login_tenant_id: TenantId::try_from(id(6)).unwrap(),
            authority_ref: id(7),
            authority_version: version(2),
        },
        command_id: Some(id(8)),
        correlation_id: id(9),
        causation_id: None,
        occurred_at: at(12),
    }
}

#[test]
fn canonical_event_preserves_source_actor_version_and_id_without_embedding_a_login_token() {
    let header = BusinessEventHeader::try_from(header_input()).unwrap();
    let event = BusinessEvent {
        header,
        payload: json!({"status":"APPROVED"}),
    };
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["source_tenant_id"], json!(id(3)));
    assert_eq!(value["source_version"], json!(3));
    assert_eq!(value["event_id"], json!(id(10)));
    assert_eq!(value["actor"]["login_tenant_id"], json!(id(6)));
    assert_eq!(value["payload"]["status"], "APPROVED");
    assert!(value.get("authorization").is_none());
}

#[test]
fn malformed_event_headers_and_system_attribution_are_rejected_during_deserialization() {
    let base = serde_json::to_value(header_input()).unwrap();
    for (field, value) in [
        ("event_id", json!(Uuid::nil())),
        ("schema_version", json!(0)),
        ("schema_version", json!(2)),
        ("source_version", json!(0)),
        ("aggregate_id", json!(Uuid::nil())),
        ("correlation_id", json!(Uuid::nil())),
        ("event_type", json!(" ")),
        ("command_id", json!(Uuid::nil())),
        ("aggregate_type", json!("employee row")),
    ] {
        let mut input = base.clone();
        input[field] = value;
        assert!(
            serde_json::from_value::<BusinessEventHeader>(input).is_err(),
            "{field}"
        );
    }
    let mut input = header_input();
    input.actor = EventActor::System {
        service: " ".into(),
    };
    assert!(BusinessEventHeader::try_from(input).is_err());
    let mut input = header_input();
    input.actor = EventActor::System {
        service: "kabipay-employee".into(),
    };
    assert!(BusinessEventHeader::try_from(input).is_ok());
}

#[test]
fn a_valid_event_still_requires_its_authenticated_company_binding() {
    let header = BusinessEventHeader::try_from(header_input()).unwrap();
    assert!(header
        .require_source(TenantId::try_from(id(3)).unwrap())
        .is_ok());
    assert_eq!(
        header.require_source(TenantId::try_from(id(99)).unwrap()),
        Err(OrganizationError::WrongTenant)
    );
}

#[test]
fn long_primary_chains_are_validated_without_recursive_stack_growth() {
    let edges: Vec<_> = (1..10_000)
        .map(|subject| edge(subject, subject + 1))
        .collect();
    assert!(validate_primary_reporting_graph(&edges, at(12)).is_ok());
}
