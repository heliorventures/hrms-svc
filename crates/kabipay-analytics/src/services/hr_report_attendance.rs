use std::collections::{BTreeMap,HashMap};
use chrono::{DateTime,Days,NaiveDate,NaiveTime,Utc};
use kabipay_common::{tenant_business_clock::TenantBusinessClock,KabiPayResult};
use kabipay_db_entities::tenant::{d0010_time_shift_roster::attendance,d0007_employee_core::employee};
use sea_orm::{ColumnTrait,DatabaseConnection,EntityTrait,QueryFilter,Select};
use uuid::Uuid;
use super::hr_reports::{ReportData,ReportFilter};

// Matches attendance_duration::canonical_instants, including rejecting ambiguous local times,
// overnight legacy checkout and equal legacy times as incomplete.
fn instants(date:NaiveDate,check_in:Option<NaiveTime>,check_out:Option<NaiveTime>,start:Option<DateTime<Utc>>,end:Option<DateTime<Utc>>,clock:TenantBusinessClock)->(Option<DateTime<Utc>>,Option<DateTime<Utc>>) {
 let legacy_start=check_in.and_then(|t|clock.to_utc(date,t).ok());
 let legacy_end=match(check_in,check_out){(Some(a),Some(b)) if a!=b=>{let date=if b>a{Some(date)}else{date.checked_add_days(Days::new(1))};date.and_then(|d|clock.to_utc(d,b).ok())},_=>None};
 (start.or(legacy_start),end.or(legacy_end))
}
#[derive(Default)]
struct Day {first:Option<(DateTime<Utc>,Uuid,Option<i32>)>,minutes:i64,incomplete:bool}
impl Day {
 fn add(&mut self,id:Uuid,start:Option<DateTime<Utc>>,end:Option<DateTime<Utc>>,late:Option<i32>) {
 if let Some(start)=start {if self.first.is_none_or(|(old,old_id,_)|(start,id)<(old,old_id)){self.first=Some((start,id,late));}}
 match(start,end){(Some(a),Some(b)) if b>a=>self.minutes+=(b-a).num_minutes(),_=>self.incomplete=true}
 }
 fn classification(&self)->&'static str {match self.first.and_then(|(_,_,late)|late){None=>"Unknown",Some(n) if n>0=>"Late",_=>"On time"}}
}
fn employee_lookup_queries(
    tenant_id: Uuid,
    employee_ids: impl IntoIterator<Item = Uuid>,
) -> Vec<Select<employee::Entity>> {
    // Attendance can contain many segments per employee and arbitrarily many employees.
    // Deduplicate first, then bound each SQL statement independently of report volume.
    // The tenant predicate consumes one additional parameter in each statement.
    const EMPLOYEE_IDS_PER_QUERY: usize = 1_000;
    let unique_ids: Vec<_> = employee_ids.into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    unique_ids.chunks(EMPLOYEE_IDS_PER_QUERY)
        .map(|ids| employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::Id.is_in(ids.iter().copied())))
        .collect()
}

pub async fn load(db:&DatabaseConnection,tenant_id:Uuid,filter:&ReportFilter,clock:TenantBusinessClock)->KabiPayResult<ReportData>{
 let mut query=attendance::Entity::find().filter(attendance::Column::TenantId.eq(tenant_id)).filter(attendance::Column::WorkDate.between(filter.from_date,filter.to_date));
 if let Some(id)=filter.employee_id{query=query.filter(attendance::Column::EmployeeId.eq(id));}
 let rows=query.all(db).await?;
 let mut employees: HashMap<Uuid, employee::Model> = HashMap::new();
 for lookup in employee_lookup_queries(tenant_id, rows.iter().map(|r| r.employee_id)) {
     employees.extend(lookup.all(db).await?.into_iter().map(|e| (e.id, e)));
 }
 let mut days:BTreeMap<(NaiveDate,String,Uuid),Day>=BTreeMap::new();
 for row in rows {let Some(employee)=employees.get(&row.employee_id) else {continue};let (start,end)=instants(row.work_date,row.check_in_time,row.check_out_time,row.check_in_at,row.check_out_at,clock);days.entry((row.work_date,employee.employee_code.clone(),row.employee_id)).or_default().add(row.id,start,end,row.late_minutes);}
 let rows=days.into_iter().map(|((date,code,id),day)|{let employee=&employees[&id];vec![code,format!("{} {}",employee.first_name,employee.last_name),date.to_string(),day.first.map(|(at,_,_)|at.to_rfc3339()).unwrap_or_default(),day.minutes.to_string(),day.incomplete.to_string(),day.first.and_then(|(_,_,late)|late).map(|n|n.to_string()).unwrap_or_default(),day.classification().into()]}).collect();
 Ok(ReportData{columns:["Employee code","Employee","Work date","First punch (UTC)","Completed minutes","Incomplete","Recorded late minutes","Punctuality"].into_iter().map(str::to_owned).collect(),rows})
}
#[cfg(test)]mod tests{
 use super::*;
 fn utc(s:&str)->DateTime<Utc>{s.parse().unwrap()}
 #[test]fn first_punch_classifies_one_employee_day_and_null_is_unknown(){
 let mut day=Day::default();day.add(Uuid::from_u128(2),Some(utc("2026-09-01T10:00:00Z")),None,Some(20));day.add(Uuid::from_u128(1),Some(utc("2026-09-01T08:00:00Z")),Some(utc("2026-09-01T09:00:00Z")),None);
 assert_eq!(day.classification(),"Unknown");assert_eq!(day.minutes,60);assert!(day.incomplete);
 let mut on_time=Day::default();on_time.add(Uuid::nil(),Some(utc("2026-09-01T08:00:00Z")),Some(utc("2026-09-01T09:00:00Z")),Some(0));assert_eq!(on_time.classification(),"On time");
 }
 #[test]fn legacy_overnight_and_equal_times_match_attendance_duration(){let clock=TenantBusinessClock::from_name("Asia/Kolkata").unwrap();let date=NaiveDate::from_ymd_opt(2026,9,1).unwrap();let a=Some("22:00:00".parse().unwrap());let b=Some("02:00:00".parse().unwrap());let(start,end)=instants(date,a,b,None,None,clock);assert_eq!((end.unwrap()-start.unwrap()).num_hours(),4);assert!(instants(date,a,a,None,None,clock).1.is_none());}
}

#[cfg(test)]
mod employee_lookup_tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait, Value};
    use std::collections::BTreeSet;

    #[test]
    fn large_attendance_lookup_bounds_each_statement_and_preserves_all_employees() {
        let tenant_id = Uuid::from_u128(900_000);
        // Covers both duplicate attendance segments and more distinct employees than the
        // PostgreSQL per-statement bind limit; deduplication alone cannot satisfy this case.
        let distinct: Vec<_> = (1..=70_000).map(Uuid::from_u128).collect();
        let queries = employee_lookup_queries(
            tenant_id,
            distinct.iter().chain(distinct.iter()).copied(),
        );
        let mut requested = BTreeSet::new();
        let mut total_employee_bindings = 0;
        for query in queries {
            let statement = query.build(DbBackend::Postgres);
            let values = statement.values.unwrap().0;
            assert!(values.len() <= 1_001, "lookup has {} parameters", values.len());
            assert_eq!(values[0], Value::from(tenant_id));
            for value in values.into_iter().skip(1) {
                let Value::Uuid(Some(id)) = value else { panic!("expected employee UUID") };
                requested.insert(*id);
                total_employee_bindings += 1;
            }
        }
        assert_eq!(requested, distinct.into_iter().collect());
        assert_eq!(total_employee_bindings, 70_000);
    }

    #[test]
    fn empty_attendance_does_not_query_the_employee_directory() {
        assert!(employee_lookup_queries(Uuid::nil(), []).is_empty());
    }
}
