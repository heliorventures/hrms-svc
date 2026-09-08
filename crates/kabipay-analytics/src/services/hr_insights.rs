use std::collections::BTreeMap;
use kabipay_common::{context::ClientClaims,tenant_business_clock::TenantBusinessClock,KabiPayError,KabiPayResult};
use sea_orm::{prelude::Decimal,ConnectionTrait,DatabaseConnection,DbBackend,Statement};
use uuid::Uuid;
use crate::resolvers::hr_report_types::{HrInsights,HrMonthlyPayroll,HrReportKind};
use super::hr_reports::{self,ReportFilter,has_all};

pub async fn load(db:&DatabaseConnection,tenant_id:Uuid,claims:&ClientClaims,filter:&ReportFilter,clock:TenantBusinessClock)->KabiPayResult<HrInsights> {
 filter.validate()?;
 if claims.tenant_id!=tenant_id || !has_all(claims,"analytics:read") {return Err(KabiPayError::Forbidden("analytics requires authenticated tenant and analytics:read ALL".into()));}
 let mut result=HrInsights::default();
 if has_all(claims,"attendance:read") {
 let data=hr_reports::load(db,tenant_id,claims,HrReportKind::AttendancePunctuality,filter,clock).await?;
 let(mut on_time,mut late,mut unknown,mut incomplete)=(0,0,0,0);
 for row in data.rows {match row[7].as_str(){"On time"=>on_time+=1,"Late"=>late+=1,_=>unknown+=1};if row[5]=="true" {incomplete+=1;}}
 result.on_time_days=Some(on_time);result.late_days=Some(late);result.unknown_punctuality_days=Some(unknown);result.incomplete_days=Some(incomplete);
 }
 if has_all(claims,"employee:read") {
 let data=hr_reports::load(db,tenant_id,claims,HrReportKind::EmployeeMovements,filter,clock).await?;
 result.joiners=Some(data.rows.iter().filter(|r|r[2]=="Joined").count() as i32);
 result.exits=Some(data.rows.iter().filter(|r|r[2]=="Exited").count() as i32);
 let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT count(*)::integer AS count FROM employee WHERE tenant_id=$1 AND NOT is_deleted AND upper(trim(status)) IN ('ACTIVE','PROBATION','ON_LEAVE')",[tenant_id.into()])).await?.ok_or_else(||KabiPayError::Validation("headcount unavailable".into()))?;
 result.active_headcount=Some(row.try_get("","count")?);
 }
 if has_all(claims,"payroll:read") {
 let data=hr_reports::load(db,tenant_id,claims,HrReportKind::PayrollRegister,filter,clock).await?;
 let mut months:BTreeMap<String,(Decimal,i32)>=BTreeMap::new();
 let mut total=Decimal::ZERO;
 for row in &data.rows {let net=row[5].parse::<Decimal>().map_err(|_|KabiPayError::Validation("invalid stored net salary".into()))?;total=total.checked_add(net).ok_or_else(||KabiPayError::Validation("payroll total overflow".into()))?;let entry=months.entry(row[2].clone()).or_insert((Decimal::ZERO,0));entry.0=entry.0.checked_add(net).ok_or_else(||KabiPayError::Validation("monthly payroll total overflow".into()))?;entry.1+=1;}
 result.net_salary_generated=Some(total.to_string());result.generated_payslips=Some(i32::try_from(data.rows.len()).map_err(|_|KabiPayError::Validation("payslip count overflow".into()))?);
 result.monthly_payroll=Some(months.into_iter().map(|(month,(net,payslips))|HrMonthlyPayroll{month,net_salary_generated:net.to_string(),payslips}).collect());
 }
 result.included_pending_domains=hr_reports::pending_domains(claims).into_iter().map(str::to_owned).collect();
 if !result.included_pending_domains.is_empty() {let data=hr_reports::load(db,tenant_id,claims,HrReportKind::PendingRequests,filter,clock).await?;result.pending_requests=Some(i32::try_from(data.rows.len()).map_err(|_|KabiPayError::Validation("pending count overflow".into()))?);}
 Ok(result)
}
