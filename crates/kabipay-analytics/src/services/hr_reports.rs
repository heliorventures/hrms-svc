use chrono::NaiveDate;
use kabipay_common::{context::{ClientClaims,ScopeType},KabiPayError,KabiPayResult,tenant_business_clock::TenantBusinessClock};
use sea_orm::{ConnectionTrait,DatabaseConnection,DbBackend,Statement};
use uuid::Uuid;
use crate::resolvers::hr_report_types::{HrReportKind,HrReportRows,HrReportCsv};

pub struct ReportFilter { pub from_date:NaiveDate,pub to_date:NaiveDate,pub employee_id:Option<Uuid>, pub employee_search:Option<String> }
impl ReportFilter {
 pub fn validate(&self)->KabiPayResult<()> { if self.from_date>self.to_date { return Err(KabiPayError::Validation("fromDate must not exceed toDate".into())); } Ok(()) }
}
pub fn has_all(claims:&ClientClaims,permission:&str)->bool { claims.has_any_permission(&[permission]) && claims.scope_for_permission(permission)==Some(ScopeType::All) }
pub fn pending_domains(claims:&ClientClaims)->Vec<&'static str> { ["leave","timesheet","expense","travel"].into_iter().filter(|domain|has_all(claims,&format!("{domain}:read"))).collect() }
pub fn authorize(claims:&ClientClaims,kind:HrReportKind)->KabiPayResult<()> {
 let allowed=match kind { HrReportKind::AttendancePunctuality=>has_all(claims,"attendance:read"), HrReportKind::LeaveRequests|HrReportKind::LeaveBalances|HrReportKind::CompOffCredits=>has_all(claims,"leave:read"), HrReportKind::PayrollRegister|HrReportKind::UnpaidLeave=>has_all(claims,"payroll:read"), HrReportKind::EmployeeMovements=>has_all(claims,"employee:read"), HrReportKind::TimesheetHours=>has_all(claims,"timesheet:read"), HrReportKind::PendingRequests=>!pending_domains(claims).is_empty() };
 if !allowed { return Err(KabiPayError::Forbidden("report requires its domain read permission with ALL scope".into())); } Ok(())
}
pub struct ReportData { pub columns:Vec<String>,pub rows:Vec<Vec<String>> }
fn count(len:usize)->KabiPayResult<i32> { i32::try_from(len).map_err(|_|KabiPayError::Validation("report exceeds supported row count".into())) }
impl ReportData {
 /// Every catalogue source starts with employee code and name. Apply this only to the full
 /// authorized result, before either preview slicing or CSV serialization.
 pub fn filter_employee(mut self, search: Option<&str>) -> Self {
     let needle = search.unwrap_or_default().trim().to_lowercase();
     if !needle.is_empty() {
         self.rows.retain(|row| row.iter().take(2).any(|value| value.to_lowercase().contains(&needle)));
     }
     self
 }

 pub fn preview(self,offset:i32,limit:i32)->KabiPayResult<HrReportRows> { if offset<0 || !(1..=100).contains(&limit) { return Err(KabiPayError::Validation("offset must be non-negative and limit between 1 and 100".into())); } Ok(HrReportRows {total_rows:count(self.rows.len())?,columns:self.columns,rows:self.rows.into_iter().skip(offset as usize).take(limit as usize).collect()}) }
 pub fn csv(self,kind:HrReportKind,filter:&ReportFilter)->KabiPayResult<HrReportCsv> {
 let row_count=count(self.rows.len())?;
 let csv=std::iter::once(&self.columns).chain(self.rows.iter()).map(|row|row.iter().map(|value|csv_cell(value)).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join("\r\n")+"\r\n";
 Ok(HrReportCsv{file_name:format!("hr-{kind:?}-{}-{}.csv",filter.from_date,filter.to_date),csv,row_count})
 }
}
fn csv_cell(value:&str)->String { let dangerous=value.trim_start_matches(|c:char|c.is_whitespace() || c.is_control() || c=='\u{feff}').starts_with(['=','+','-','@']) || value.starts_with(['\t','\r','\n']); format!("\"{}{}\"",if dangerous {"'"} else {""},value.replace('"',"\"\"")) }
struct Source { columns:Vec<&'static str>, sql:String }
fn source(kind:HrReportKind,domains:&[&str])->Source {
 let labels="e.employee_code,concat_ws(' ',e.first_name,e.last_name)";
 let employee_join="JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id";
 let predicate="r.tenant_id=$1 AND ($4::uuid IS NULL OR r.employee_id=$4)";
 let (columns,sql)=match kind {
 HrReportKind::AttendancePunctuality => unreachable!("attendance uses canonical Rust timestamp semantics"),
 HrReportKind::LeaveRequests => (vec!["Employee code","Employee","Leave type","From date","To date","Units","Status","Unpaid","Comp-off"],format!("SELECT {labels},t.name,r.from_date,r.to_date,r.days_requested,r.status,NOT t.is_paid,r.uses_comp_off FROM leave_request r {employee_join} JOIN leave_type t ON t.id=r.leave_type_id AND t.tenant_id=r.tenant_id WHERE {predicate} AND NOT r.is_deleted AND r.from_date<=$3 AND r.to_date>=$2 ORDER BY r.from_date,e.employee_code,r.id")),
 HrReportKind::LeaveBalances => (vec!["Employee code","Employee","Leave type","Leave year","Current entitled","Current used","Current pending","Carried forward","Current balance"],format!("SELECT {labels},t.name,r.year,r.entitled_days,r.used_days,r.pending_days,r.carried_forward_days,r.balance_days FROM leave_balance r {employee_join} JOIN leave_type t ON t.id=r.leave_type_id AND t.tenant_id=r.tenant_id WHERE {predicate} AND t.code<>'COMP_OFF' AND r.year BETWEEN extract(year FROM $2::date) AND extract(year FROM $3::date) ORDER BY r.year,e.employee_code,t.code,r.id")),
 HrReportKind::PayrollRegister|HrReportKind::UnpaidLeave => {
 let unpaid=kind==HrReportKind::UnpaidLeave;
 let extra=if unpaid {",u.basic_component_code,u.basic_amount,u.day_divisor,u.unpaid_days,u.amount,u.treatment,u.source_days"}else{""};
 let mut cols=vec!["Employee code","Employee","Payroll month","Gross salary","Deductions","Net salary generated","Payslip status"];
 if unpaid {cols.extend(["Basic component","Stored basic amount","Day divisor","Unpaid days","Unpaid amount","Treatment","Source day snapshot"]);}
 (cols,format!("SELECT {labels},to_char(make_date(c.year,c.month,1),'YYYY-MM'),r.gross_salary,r.total_deductions,r.net_salary,r.status{extra} FROM payslip r {employee_join} JOIN payroll_cycle c ON c.id=r.payroll_cycle_id AND c.tenant_id=r.tenant_id {} WHERE {predicate} AND make_date(c.year,c.month,1)<= $3 AND (make_date(c.year,c.month,1)+interval '1 month')::date>$2 ORDER BY c.year,c.month,e.employee_code,r.id",if unpaid {"JOIN payslip_unpaid_leave u ON u.payslip_id=r.id AND u.tenant_id=r.tenant_id"}else{""}))
 },
 HrReportKind::EmployeeMovements => (vec!["Employee code","Employee","Movement","Effective date","Current department","Current designation"],format!("WITH movements AS (SELECT id employee_id,tenant_id,'Joined' movement,date_of_joining effective_date,id FROM employee WHERE tenant_id=$1 AND NOT is_deleted UNION ALL SELECT employee_id,tenant_id,'Exited',last_working_date,id FROM separation WHERE tenant_id=$1 AND offboarded_at IS NOT NULL AND last_working_date<=$6) SELECT {labels},r.movement,r.effective_date,d.name,g.title FROM movements r {employee_join} LEFT JOIN department d ON d.id=e.department_id AND d.tenant_id=e.tenant_id LEFT JOIN designation g ON g.id=e.designation_id AND g.tenant_id=e.tenant_id WHERE {predicate} AND r.effective_date BETWEEN $2 AND $3 ORDER BY r.effective_date,e.employee_code,r.movement,r.id")),
 HrReportKind::TimesheetHours => (vec!["Employee code","Employee","Work date","Project code","Work description","Hours","Approval status"],format!("SELECT {labels},r.work_date,r.project_code,r.description,r.hours_worked,r.status FROM timesheet_entry r {employee_join} WHERE {predicate} AND NOT r.is_deleted AND r.work_date BETWEEN $2 AND $3 ORDER BY r.work_date,e.employee_code,r.id")),
 HrReportKind::CompOffCredits => (vec!["Employee code","Employee","Approval business date","Credited units","Current reserved","Current used","Current remaining","Current expired","Expiry date"],format!("SELECT {labels},r.approval_business_date,r.earned_units,r.reserved_units,r.used_units,CASE WHEN r.expires_at>$6 THEN greatest(0,r.earned_units-r.reserved_units-r.used_units) ELSE 0 END,CASE WHEN r.expires_at<=$6 THEN greatest(0,r.earned_units-r.reserved_units-r.used_units) ELSE 0 END,r.expires_at FROM comp_off_credit r {employee_join} WHERE {predicate} AND r.approval_business_date BETWEEN $2 AND $3 ORDER BY r.approval_business_date,e.employee_code,r.id")),
 HrReportKind::PendingRequests => {
 let mut selects=Vec::new();
 for (domain,table,date,condition) in [("leave","leave_request","applied_at","status='PENDING' AND NOT is_deleted"),("leave","comp_off_claim","created_at","status='PENDING'"),("timesheet","timesheet_week_batch","submitted_at","status='PENDING'"),("expense","expense","submitted_at","status IN ('PENDING','SUBMITTED') AND NOT is_deleted"),("travel","travel_request","submitted_at","status IN ('PENDING','SUBMITTED')")] {
 if domains.contains(&domain) {selects.push(format!("SELECT id,tenant_id,employee_id,'{table}' domain,{date} submitted_at,status FROM {table} WHERE tenant_id=$1 AND {condition}"));}
 }
 (vec!["Employee code","Employee","Request domain","Submitted date","Status","Request ID"],format!("WITH requests AS ({}) SELECT {labels},r.domain,(r.submitted_at AT TIME ZONE $5)::date,r.status,r.id FROM requests r {employee_join} WHERE {predicate} AND (r.submitted_at AT TIME ZONE $5)::date BETWEEN $2 AND $3 ORDER BY r.submitted_at,e.employee_code,r.domain,r.id",selects.join(" UNION ALL ")))
 }
 };
 Source{columns,sql}
}
/// No LIMIT/OFFSET is applied here: preview slicing and CSV consume the same full filtered source.
pub async fn load(db:&DatabaseConnection,tenant_id:Uuid,claims:&ClientClaims,kind:HrReportKind,filter:&ReportFilter,clock:TenantBusinessClock)->KabiPayResult<ReportData> {
 authorize(claims,kind)?;filter.validate()?;
 if claims.tenant_id!=tenant_id {return Err(KabiPayError::Forbidden("report tenant does not match authenticated tenant".into()));}
 let data = if kind == HrReportKind::AttendancePunctuality {
     super::hr_report_attendance::load(db, tenant_id, filter, clock).await?
 } else {
     load_sql(db, tenant_id, claims, kind, filter, clock).await?
 };
 Ok(data.filter_employee(filter.employee_search.as_deref()))
}
async fn load_sql(db:&DatabaseConnection,tenant_id:Uuid,claims:&ClientClaims,kind:HrReportKind,filter:&ReportFilter,clock:TenantBusinessClock)->KabiPayResult<ReportData> {
 let source=source(kind,&pending_domains(claims));
 // Turn explicitly selected fields into a positional JSON array; preserve decimal text losslessly.
 let aliases=(0..source.columns.len()).map(|i|format!("c{i}")).collect::<Vec<_>>();
 let cells=aliases.iter().map(|c|format!("coalesce({c}::text,'')")).collect::<Vec<_>>().join(",");
 let sql=format!("WITH report({}) AS ({}) SELECT jsonb_build_array({cells}) AS cells FROM report CROSS JOIN (SELECT $1::uuid,$2::date,$3::date,$4::uuid,$5::text,$6::date) bindings",aliases.join(","),source.sql);
 let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,sql,vec![tenant_id.into(),filter.from_date.into(),filter.to_date.into(),filter.employee_id.into(),clock.timezone_name().into(),clock.now_date().into()])).await?;
 let mut data: Vec<Vec<String>>=Vec::with_capacity(rows.len());
 for row in rows {let value:serde_json::Value=row.try_get("","cells")?;data.push(serde_json::from_value(value).map_err(|e|KabiPayError::Validation(format!("invalid report row: {e}")))?);}
 // Sort positional values explicitly so outer-query planner changes cannot reorder preview pages.
 data.sort();
 Ok(ReportData{columns:source.columns.into_iter().map(str::to_owned).collect(),rows:data})
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test]
 fn employee_movements_selects_designation_title_from_entity_schema() {
     use kabipay_db_entities::tenant::d0006_org_hierarchy::{department, designation};
     use sea_orm::IdenStatic;

     let report = source(HrReportKind::EmployeeMovements, &[]);
     let organization_fields = format!(
         "d.{},g.{} FROM movements",
         department::Column::Name.as_str(),
         designation::Column::Title.as_str(),
     );
     assert!(report.sql.contains(&organization_fields),
         "Employee movements must select the department name and designation title");
     assert_eq!(report.columns.last(), Some(&"Current designation"));
 }
 fn filter()->ReportFilter {ReportFilter{from_date:NaiveDate::from_ymd_opt(2026,9,1).unwrap(),to_date:NaiveDate::from_ymd_opt(2026,9,8).unwrap(),employee_id:None,employee_search:None}}
 #[test] fn csv_neutralizes_formulas_and_escapes_multiline_quotes() {
 assert_eq!(csv_cell("=SUM(1,2)"),"\"'=SUM(1,2)\"");
 assert_eq!(csv_cell(" \t@formula"),"\"' \t@formula\"");
 assert_eq!(csv_cell("a,\"b\"\nc"),"\"a,\"\"b\"\"\nc\"");
 for input in ["+cmd","-cmd","@cmd","\tx","\rx","\nx","\u{feff}=cmd","\0=cmd"] {assert!(csv_cell(input).starts_with("\"'"));}
 }
 #[test] fn full_csv_is_independent_of_preview_page() {
 let data=||ReportData{columns:vec!["Value".into()],rows:(0..251).map(|n|vec![n.to_string()]).collect()};
 let page=data().preview(200,50).unwrap();assert_eq!(page.total_rows,251);assert_eq!(page.rows.len(),50);assert_eq!(page.rows[0][0],"200");
 let csv=data().csv(HrReportKind::LeaveRequests,&filter()).unwrap();assert_eq!(csv.row_count,251);assert!(csv.csv.ends_with("\"250\"\r\n"));
 }
 #[test] fn rejects_inverted_range_and_invalid_paging() {
 let mut f=filter();std::mem::swap(&mut f.from_date,&mut f.to_date);assert!(f.validate().is_err());
 assert!(ReportData{columns:vec![],rows:vec![]}.preview(-1,50).is_err());
 assert!(ReportData{columns:vec![],rows:vec![]}.preview(0,101).is_err());
 }
}

#[cfg(test)]
mod employee_search_tests {
    use super::*;

    fn source_rows() -> ReportData {
        ReportData {
            columns: vec!["Employee code".into(), "Employee".into(), "Description".into()],
            rows: vec![
                vec!["EMP-042".into(), "Ana Rivera".into(), "Project one".into()],
                vec!["EMP-099".into(), "Bob Jones".into(), "Ana project".into()],
                vec!["EMP-100".into(), "ANA Singh".into(), "Project two".into()],
            ],
        }
    }

    #[test]
    fn preview_and_full_export_search_only_employee_code_or_name() {
        let filter = ReportFilter {
            from_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            to_date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            employee_id: None,
            employee_search: Some("  aNa  ".into()),
        };
        let page = source_rows().filter_employee(filter.employee_search.as_deref())
            .preview(1, 1).unwrap();
        assert_eq!(page.total_rows, 2);
        assert_eq!(page.rows[0][1], "ANA Singh");
        let export = source_rows().filter_employee(filter.employee_search.as_deref())
            .csv(HrReportKind::TimesheetHours, &filter).unwrap();
        assert_eq!(export.row_count, 2);
        assert!(export.csv.contains("Ana Rivera"));
        assert!(export.csv.contains("ANA Singh"));
        assert!(!export.csv.contains("Bob Jones"));
        assert_eq!(source_rows().filter_employee(Some("emp-042")).rows.len(), 1);
        assert_eq!(source_rows().filter_employee(Some("  ")).rows.len(), 3);
        assert_eq!(source_rows().filter_employee(Some("Project")).rows.len(), 0);
    }
}
