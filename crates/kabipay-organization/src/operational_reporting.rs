//! Source-defined operational facts. No inferred productivity or daily leave proration.
use crate::{
    execution::Execution,
    identity::SourceIdentity,
    reporting::{companies, coverage, progress_bounds, reauthorize, tenants, unavailable},
};
use chrono::NaiveDate;
use kabipay_common::KabiPayResult;
use sea_orm::TransactionTrait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;
const ATTENDANCE:&str="SELECT source_tenant_id,employee_id,work_date,status,toString(records) AS records,toString(unknown_duration_records) AS unknown_duration_records,toString(observed_seconds) AS observed_seconds,toString(overtime_seconds) AS overtime_seconds,toString(late_minutes) AS late_minutes,toString(early_exit_minutes) AS early_exit_minutes FROM organization.attendance_daily WHERE organization_id={organization:UUID} AND source_tenant_id IN {tenants:Array(UUID)} AND work_date>={from:Date} AND work_date<={to:Date} ORDER BY source_tenant_id,work_date,employee_id,status LIMIT 10001 FORMAT JSONEachRow";
const LEAVE:&str="SELECT source_tenant_id,request_id,employee_id,from_date,to_date,toString(requested_centidays) AS requested_centidays,status FROM organization.leave_totals WHERE organization_id={organization:UUID} AND source_tenant_id IN {tenants:Array(UUID)} AND from_date<={to:Date} AND to_date>={from:Date} ORDER BY source_tenant_id,from_date,request_id LIMIT 10001 FORMAT JSONEachRow";
const PROJECT:&str="SELECT source_tenant_id,employee_id,work_date,project_code,toString(logged_seconds) AS logged_seconds,toString(approved_seconds) AS approved_seconds FROM organization.project_progress_daily WHERE organization_id={organization:UUID} AND source_tenant_id IN {tenants:Array(UUID)} AND work_date>={from:Date} AND work_date<={to:Date} ORDER BY source_tenant_id,work_date,employee_id,project_code LIMIT 10001 FORMAT JSONEachRow";

fn exact_quantity(v: &Value, nullable: bool) -> bool {
    nullable && v.is_null()
        || v.as_str().is_some_and(|s| {
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u128>().is_ok()
        })
}
fn row_tenant(row: &Value, kind: &str, from: NaiveDate, to: NaiveDate) -> KabiPayResult<Uuid> {
    let (keys, quantities, nullable): (&[&str], &[&str], &[&str]) = match kind {
        "attendance" => (
            &[
                "source_tenant_id",
                "employee_id",
                "work_date",
                "status",
                "records",
                "unknown_duration_records",
                "observed_seconds",
                "overtime_seconds",
                "late_minutes",
                "early_exit_minutes",
            ],
            &["records", "unknown_duration_records"],
            &[
                "observed_seconds",
                "overtime_seconds",
                "late_minutes",
                "early_exit_minutes",
            ],
        ),
        "leave" => (
            &[
                "source_tenant_id",
                "request_id",
                "employee_id",
                "from_date",
                "to_date",
                "requested_centidays",
                "status",
            ],
            &["requested_centidays"],
            &[],
        ),
        "project_time" => (
            &[
                "source_tenant_id",
                "employee_id",
                "work_date",
                "project_code",
                "logged_seconds",
                "approved_seconds",
            ],
            &["logged_seconds", "approved_seconds"],
            &[],
        ),
        _ => return Err(unavailable()),
    };
    let object = row.as_object().ok_or_else(unavailable)?;
    if object.len() != keys.len()
        || !object.keys().all(|k| keys.contains(&k.as_str()))
        || !quantities
            .iter()
            .all(|key| exact_quantity(&row[key], false))
        || !nullable.iter().all(|key| exact_quantity(&row[key], true))
    {
        return Err(unavailable());
    }
    let id = |key: &str| {
        row[key]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| !id.is_nil())
    };
    let tenant = id("source_tenant_id").ok_or_else(unavailable)?;
    if id("employee_id").is_none() || kind == "leave" && id("request_id").is_none() {
        return Err(unavailable());
    }
    let date = |key: &str| {
        row[key]
            .as_str()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    };
    let in_range = if kind == "leave" {
        date("from_date")
            .zip(date("to_date"))
            .is_some_and(|(a, b)| a <= b && a <= to && b >= from)
    } else {
        date("work_date").is_some_and(|d| from <= d && d <= to)
    };
    if !in_range
        || kind != "project_time"
            && !row["status"].as_str().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= 40
                    && s.bytes().enumerate().all(|(i, b)| {
                        b.is_ascii_uppercase() || i > 0 && (b == b'_' || b.is_ascii_digit())
                    })
            })
        || kind == "project_time"
            && !(row["project_code"].is_null()
                || row["project_code"]
                    .as_str()
                    .is_some_and(|s| s.chars().count() <= 255))
    {
        return Err(unavailable());
    }
    Ok(tenant)
}
impl Execution {
    pub async fn operational_progress(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        membership_ids: Option<Vec<Uuid>>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> KabiPayResult<Value> {
        progress_bounds(from, to)?;
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        let tx = self.store.db.begin().await?;
        let selected = companies(&tx, actor, org, membership_ids, "analytics:read").await?;
        let mut states = BTreeMap::new();
        let start = from.and_hms_opt(0, 0, 0).ok_or_else(unavailable)?.and_utc();
        for company in &selected {
            states.insert(
                company.tenant,
                coverage(&tx, org, company.tenant, start).await?,
            );
        }
        let mut facts: BTreeMap<(Uuid, &str), Vec<Value>> = BTreeMap::new();
        let mut available = self.reporting.is_some();
        if let Some(client) = self.reporting.as_ref() {
            let params = [
                ("organization", org.to_string()),
                ("tenants", tenants(&selected)),
                ("from", from.to_string()),
                ("to", to.to_string()),
            ];
            for (kind, query) in [
                ("attendance", ATTENDANCE),
                ("leave", LEAVE),
                ("project_time", PROJECT),
            ] {
                match client.query(query, &params).await {
                    Ok(rows) if rows.len() <= 10000 => {
                        for mut row in rows {
                            let tenant = row_tenant(&row, kind, from, to)?;
                            if !states.contains_key(&tenant) {
                                return Err(unavailable());
                            }
                            row.as_object_mut()
                                .ok_or_else(unavailable)?
                                .remove("source_tenant_id");
                            facts.entry((tenant, kind)).or_default().push(row);
                        }
                    }
                    _ => {
                        available = false;
                        break;
                    }
                }
            }
        }
        let mut partial = false;
        let mut result = Vec::new();
        for company in &selected {
            let mut state = states.remove(&company.tenant).ok_or_else(unavailable)?;
            if !available {
                state = json!({"state":"UNAVAILABLE","reason_code":"REPORTING_SINK_UNAVAILABLE_OR_LIMIT_EXCEEDED"});
            }
            let ready = available && state["state"] == "READY";
            partial |= !ready;
            let values = |key| {
                if ready {
                    json!(facts
                        .get(&(company.tenant, key))
                        .cloned()
                        .unwrap_or_default())
                } else {
                    Value::Null
                }
            };
            result.push(json!({"membership_id":company.membership,"tenant_id":company.tenant,"company_name":company.name,"freshness":state,"attendance":values("attendance"),"leave":values("leave"),"project_time":values("project_time")}));
        }
        self.identities
            .require_eligible(actor.tenant(), actor.user(), &self.service_issuer)
            .await?;
        reauthorize(&tx, actor, org, &selected, "analytics:read").await?;
        // An outage, marker change or quarantine during the sink read cannot be advertised READY.
        for company in &selected {
            if coverage(&tx, org, company.tenant, start).await?["state"] != "READY" {
                for row in &mut result {
                    if row["membership_id"] == company.membership.to_string() {
                        row["freshness"] = coverage(&tx, org, company.tenant, start).await?;
                        for key in ["attendance", "leave", "project_time"] {
                            row[key] = Value::Null;
                        }
                        partial = true;
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(json!({"schema_version":1,"companies":result,"partial":partial}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_quantities_preserve_null_and_large_values() {
        assert!(exact_quantity(
            &json!("340282366920938463463374607431768211455"),
            false
        ));
        for v in [
            json!("-1"),
            json!("1.5"),
            json!(2),
            json!("340282366920938463463374607431768211456"),
        ] {
            assert!(!exact_quantity(&v, false));
        }
        assert!(exact_quantity(&Value::Null, true));
        assert!(!exact_quantity(&Value::Null, false));
    }
    #[test]
    fn leave_ranges_keep_whole_request_quantity_and_reject_private_fields() {
        let from = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        let mut row = json!({"source_tenant_id":Uuid::new_v4(),"request_id":Uuid::new_v4(),"employee_id":Uuid::new_v4(),"from_date":"2026-10-17","to_date":"2026-10-20","requested_centidays":"400","status":"APPROVED"});
        assert!(row_tenant(&row, "leave", from, from).is_ok());
        row["reason"] = json!("private");
        assert!(row_tenant(&row, "leave", from, from).is_err());
    }
}
