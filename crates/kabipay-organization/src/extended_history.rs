//! Revalidate purpose-limited history before it crosses the browser boundary.
use crate::reporting::unavailable;
use chrono::NaiveDate;
use kabipay_common::KabiPayResult;
use serde_json::Value;
use uuid::Uuid;
fn id(v: &Value) -> Option<Uuid> {
    v.as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .filter(|id| !id.is_nil())
}
fn date(v: &Value) -> Option<NaiveDate> {
    v.as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}
fn quantity(v: &Value, nullable: bool) -> bool {
    nullable && v.is_null() || v.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
}
fn status(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 40
            && s.bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_uppercase() || i > 0 && (b == b'_' || b.is_ascii_digit()))
    })
}
pub(crate) fn validate(
    kind: &str,
    aggregate: &str,
    aggregate_id: Uuid,
    version: u64,
    deleted: bool,
    p: &Value,
) -> KabiPayResult<bool> {
    let employee = id(&p["employee_id"]).is_some();
    let valid = match kind {
        "attendance.changed" => {
            aggregate == "attendance"
                && employee
                && date(&p["work_date"]).is_some()
                && status(&p["status"])
                && [
                    "observed_seconds",
                    "overtime_seconds",
                    "late_minutes",
                    "early_exit_minutes",
                ]
                .iter()
                .all(|key| quantity(&p[key], true))
        }
        "leave.changed" => {
            aggregate == "leave_request"
                && employee
                && date(&p["from_date"])
                    .zip(date(&p["to_date"]))
                    .is_some_and(|(a, b)| a <= b)
                && quantity(&p["requested_centidays"], false)
                && status(&p["status"])
        }
        "project_time.changed" => {
            aggregate == "timesheet_entry"
                && employee
                && date(&p["work_date"]).is_some()
                && (p["project_code"].is_null()
                    || p["project_code"]
                        .as_str()
                        .is_some_and(|s| s.chars().count() <= 255))
                && quantity(&p["logged_seconds"], false)
                && quantity(&p["approved_seconds"], false)
        }
        "administration.changed" => {
            let actions: &[&str] = match aggregate {
                "employee" => &[
                    "employee_create",
                    "employee_update",
                    "employee_employment",
                    "employee_login_provision",
                ],
                "user" => &[
                    "account_create",
                    "account_update",
                    "account_access",
                    "account_roles",
                ],
                "workflow_definition" => &["workflow_save", "workflow_activation"],
                "separation" => &[
                    "separation_create",
                    "separation_reject",
                    "separation_cancel_approval",
                ],
                "clearance_checklist" => &["clearance_update"],
                "payroll_job" => &["payroll_job_status", "payroll_job_cancel_before_execution"],
                _ => &[],
            };
            p["resource_type"] == aggregate
                && id(&p["resource_id"]) == Some(aggregate_id)
                && p["action"].as_str().is_some_and(|s| actions.contains(&s))
                && (p["status"].is_null()
                    || p["status"].as_str().is_some_and(|s| {
                        [
                            "RECORDED",
                            "QUEUED",
                            "RUNNING",
                            "SUCCEEDED",
                            "FAILED",
                            "CANCELLED",
                        ]
                        .contains(&s)
                    }))
        }
        "legacy_audit.recorded" => {
            aggregate == "legacy_audit"
                && id(&p["source_audit_id"]) == Some(aggregate_id)
                && version == 1
                && !deleted
                && p["source_entity_type"].as_str().is_some_and(|s| {
                    [
                        "LEAVE_REQUEST",
                        "employee_profile_change_request",
                        "employee_education",
                        "employee_work_experience",
                        "company_location",
                        "working_calendar_profile",
                        "holiday",
                        "roster",
                        "UNMAPPED",
                    ]
                    .contains(&s)
                })
                && p["source_action"].as_str().is_some_and(|s| {
                    [
                        "approved",
                        "rejected",
                        "submitted",
                        "created",
                        "updated",
                        "deleted",
                        "APPROVED",
                        "REJECTED",
                        "CREATE",
                        "UPDATE",
                        "DELETE",
                        "CANCEL_APPROVED_COMP_OFF_LEAVE",
                        "UNMAPPED",
                    ]
                    .contains(&s)
                })
                && ["source_entity_id", "source_user_id"]
                    .iter()
                    .all(|key| p[key].is_null() || id(&p[key]).is_some())
                && ["before_status", "after_status"].iter().all(|key| {
                    p[key].is_null()
                        || p[key].as_str().is_some_and(|s| {
                            [
                                "PENDING",
                                "APPROVED",
                                "REJECTED",
                                "CANCELLED",
                                "VERIFIED",
                                "DRAFT",
                                "SUBMITTED",
                                "ACTIVE",
                                "INACTIVE",
                            ]
                            .contains(&s)
                        })
                })
                && p["source_sha256"].as_str().is_some_and(|s| {
                    s.len() == 64
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        }
        _ => return Ok(false),
    };
    if valid {
        Ok(true)
    } else {
        Err(unavailable())
    }
}
