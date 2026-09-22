use super::*;

fn participant(employee_id: Uuid, manager_employee_id: Option<Uuid>) -> performance_participant::Model {
    performance_participant::Model {
        id: Uuid::new_v4(), tenant_id: Uuid::new_v4(), review_cycle_id: Uuid::new_v4(),
        employee_id, manager_employee_id, department_id: None, designation_id: None,
        work_location_id: None, appraisal_template_id: Uuid::new_v4(), status: "SELF_REVIEW".into(),
        is_excluded: false, exclusion_reason: None, response_revision: 2, self_submitted_at: None,
        manager_submitted_at: None, acknowledged_at: None, acknowledgement_comment: None,
        final_rating: None, performance_band: None, manager_rating: None,
        manager_performance_band: None, calibration_provenance: None,
        created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(),
    }
}

fn claims(permission: &str, scope: &str, employee_id: Uuid) -> kabipay_common::context::ClientClaims {
    let mut claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
        "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(), "iss": "kabipay-client",
        "iat": 0, "exp": 9999999999i64,
        "permissions": [permission], "permission_scopes": {permission: scope}
    })).expect("test claims must deserialize");
    claims.employee_id = Some(employee_id);
    claims
}

#[test]
fn rejects_malformed_or_replayed_pagination_cursors() {
    assert!(parse_uuid_cursor(Some("not-a-uuid")).is_err());
    assert!(parse_name_cursor(Some("missing-id")).is_err());
    assert!(parse_cycle_cursor(Some("2026-02-30|00000000-0000-0000-0000-000000000000")).is_err());
    assert!(parse_feedback_cursor(Some("not-a-time|00000000-0000-0000-0000-000000000000")).is_err());
}

#[test]
fn kpi_actuals_require_the_participant_role_for_the_open_stage() {
    let employee_id = Uuid::new_v4();
    let manager_id = Uuid::new_v4();
    let review = participant(employee_id, Some(manager_id));
    let self_claims = claims("performance:self", "SELF", employee_id);
    assert!(require_kpi_actual_authority(&self_claims, &review, "SELF_REVIEW").is_ok());
    assert!(require_kpi_actual_authority(&self_claims, &review, "MANAGER_REVIEW").is_err());
    let other_self = claims("performance:self", "SELF", Uuid::new_v4());
    assert!(require_kpi_actual_authority(&other_self, &review, "SELF_REVIEW").is_err());
    let manager_claims = claims("performance:evaluate", "TEAM", manager_id);
    assert!(require_kpi_actual_authority(&manager_claims, &review, "MANAGER_REVIEW").is_ok());
}
