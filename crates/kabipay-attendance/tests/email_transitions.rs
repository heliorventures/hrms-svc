// Complements owned expiry and workflow rehearsals.
#[test]
fn only_successfully_written_expiry_appends_missed_punch_email_before_return() {
    let source = include_str!("../src/services/attendance_day_runtime.rs");
    let start = source.find("pub(crate) async fn expire_employee").unwrap();
    let body = &source[start..source.find("pub struct ExpirySweepResult").unwrap()];
    assert!(body
        .find("emit_missed_punch_email")
        .is_some_and(|i| i > body.find("rows_affected() != 1").unwrap()
            && i < body.find("expired += 1").unwrap()));
}

#[test]
fn submit_timesheet_week_uses_one_owned_email_emitter() {
    let source = include_str!("../src/services/timesheet_batch_service.rs");
    let start = source.find("pub async fn submit_timesheet_week(").unwrap();
    let body = &source[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body.contains("timesheet_email::"));
}

#[test]
fn approve_timesheet_week_batch_uses_one_owned_email_emitter() {
    let source = include_str!("../src/services/timesheet_batch_service.rs");
    let start = source
        .find("pub async fn approve_timesheet_week_batch(")
        .unwrap();
    let body = &source[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body.contains("timesheet_email::"));
}

#[test]
fn reject_timesheet_week_batch_uses_one_owned_email_emitter() {
    let source = include_str!("../src/services/timesheet_batch_service.rs");
    let start = source
        .find("pub async fn reject_timesheet_week_batch(")
        .unwrap();
    let body = &source[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body.contains("timesheet_email::"));
}

#[test]
fn decide_organization_timesheet_uses_one_owned_email_emitter() {
    let source = include_str!("../src/services/timesheet_batch_service.rs");
    let start = source
        .find("pub async fn decide_organization_timesheet(")
        .unwrap();
    let body = &source[start..];
    let end = body[1..]
        .find("\npub async fn ")
        .map_or(body.len(), |i| i + 1);
    let body = &body[..end];
    assert!(body.contains("timesheet_email::"));
}
