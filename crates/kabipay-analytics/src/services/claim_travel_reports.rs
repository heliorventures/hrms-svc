//! Bound database predicates are shared by counts, ordered pages and bounded exports.

use kabipay_common::{
    context::ClientClaims, tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult,
};
use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DbBackend, IsolationLevel, QueryResult,
    Statement, TransactionTrait, Value,
};
use uuid::Uuid;

use super::claim_travel_report_filters::{literal_substring, ClaimTravelFilter};
use super::hr_reports::{authorize, render_csv};
use crate::resolvers::hr_report_types::{
    ClaimTravelReportOption, ClaimTravelReportOptions, HrReportCsv, HrReportKind, HrReportRows,
};

pub use super::claim_travel_report_filters::is_claim_travel;
pub const EXPORT_LIMIT: usize = 10_000;

pub async fn load_options(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    kind: HrReportKind,
    search: Option<&str>,
    limit: i32,
) -> KabiPayResult<ClaimTravelReportOptions> {
    check_authority(claims, tenant, kind)?;
    source(kind)?;
    let pattern = literal_substring(search.map(str::trim).filter(|s| !s.is_empty()));
    let limit = limit.clamp(1, 200);
    let mut groups = Vec::new();
    for table in ["department", "location", "expense_category"] {
        if table == "expense_category" && kind == HrReportKind::TravelRequests {
            groups.push(vec![]);
            continue;
        }
        let sql = format!(
            r"SELECT id,name FROM {table} WHERE tenant_id=$1 AND NOT is_deleted AND ($2::text IS NULL OR name ILIKE $2 ESCAPE E'\\') ORDER BY lower(name),id LIMIT $3"
        );
        let result = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                vec![tenant.into(), pattern.clone().into(), limit.into()],
            ))
            .await?;
        let options = result
            .into_iter()
            .map(|row| -> KabiPayResult<_> {
                let id: Uuid = row.try_get("", "id")?;
                Ok(ClaimTravelReportOption {
                    id: async_graphql::ID(id.to_string()),
                    name: row.try_get("", "name")?,
                })
            })
            .collect::<KabiPayResult<Vec<_>>>()?;
        groups.push(options);
    }
    let mut groups = groups.into_iter();
    Ok(ClaimTravelReportOptions {
        departments: groups.next().unwrap_or_default(),
        locations: groups.next().unwrap_or_default(),
        expense_categories: groups.next().unwrap_or_default(),
    })
}

struct Source {
    columns: Vec<&'static str>,
    fields: &'static str,
    from: &'static str,
    predicate: &'static str,
    order: &'static str,
}

fn source(kind: HrReportKind) -> KabiPayResult<Source> {
    match kind {
        HrReportKind::ExpenseClaims => Ok(Source {
            columns: vec!["Employee code", "Employee", "Current department", "Current location", "Expense date", "Submitted date", "Category", "Title", "Claimed amount", "Approved amount", "Currency", "Approval status", "Payment status", "Payment reference", "Linked travel", "Supporting file"],
            fields: "e.employee_code,concat_ws(' ',e.first_name,e.last_name),d.name,l.name,r.expense_date,(r.submitted_at AT TIME ZONE $12)::date,c.name,r.title,r.amount::text,r.approved_amount::text,r.currency,r.status,r.payment_status,r.payment_reference,concat_ws(' → ',t.origin_location,t.destination_location),(r.receipt_file_storage_id IS NOT NULL)",
            from: "expense r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id LEFT JOIN department d ON d.id=e.department_id AND d.tenant_id=e.tenant_id LEFT JOIN location l ON l.id=e.location_id AND l.tenant_id=e.tenant_id LEFT JOIN expense_category c ON c.id=r.expense_category_id AND c.tenant_id=r.tenant_id LEFT JOIN travel_request t ON t.id=r.travel_request_id AND t.tenant_id=r.tenant_id",
            predicate: "NOT r.is_deleted AND r.expense_date BETWEEN $2 AND $3 AND ($7::uuid IS NULL OR r.expense_category_id=$7) AND ($9::text IS NULL OR r.payment_status=$9)",
            order: "r.expense_date DESC,r.submitted_at DESC,r.id",
        }),
        HrReportKind::TravelRequests => Ok(Source {
            columns: vec!["Employee code", "Employee", "Current department", "Current location", "Origin", "Destination", "From date", "To date", "Purpose", "Estimated amount", "Currency", "Approval status", "Submitted date", "Supporting file"],
            fields: "e.employee_code,concat_ws(' ',e.first_name,e.last_name),d.name,l.name,r.origin_location,r.destination_location,r.from_date,r.to_date,r.purpose,r.estimated_amount::text,r.currency,r.status,(r.submitted_at AT TIME ZONE $12)::date,(r.supporting_file_storage_id IS NOT NULL)",
            from: "travel_request r JOIN employee e ON e.id=r.employee_id AND e.tenant_id=r.tenant_id LEFT JOIN department d ON d.id=e.department_id AND d.tenant_id=e.tenant_id LEFT JOIN location l ON l.id=e.location_id AND l.tenant_id=e.tenant_id",
            predicate: r"r.from_date<=$3 AND r.to_date>=$2 AND ($11::text IS NULL OR r.origin_location ILIKE $11 ESCAPE E'\\' OR r.destination_location ILIKE $11 ESCAPE E'\\')",
            order: "r.from_date DESC,r.submitted_at DESC,r.id",
        }),
        _ => Err(KabiPayError::Validation("an expense or travel report kind is required".into())),
    }
}

const COMMON_PREDICATE: &str = r"r.tenant_id=$1 AND ($4::uuid IS NULL OR r.employee_id=$4) AND ($5::uuid IS NULL OR e.department_id=$5) AND ($6::uuid IS NULL OR e.location_id=$6) AND ($8::text IS NULL OR r.status=$8) AND ($10::text IS NULL OR e.employee_code ILIKE $10 ESCAPE E'\\' OR concat_ws(' ',e.first_name,e.last_name) ILIKE $10 ESCAPE E'\\')";
const BINDINGS: &str = "CROSS JOIN (SELECT $1::uuid,$2::date,$3::date,$4::uuid,$5::uuid,$6::uuid,$7::uuid,$8::text,$9::text,$10::text,$11::text,$12::text) bindings";

fn statement(
    sql: String,
    tenant: Uuid,
    filter: &ClaimTravelFilter,
    clock: &TenantBusinessClock,
) -> Statement {
    let values: Vec<Value> = vec![
        tenant.into(),
        filter.base.from_date.into(),
        filter.base.to_date.into(),
        filter.base.employee_id.into(),
        filter.department_id.into(),
        filter.location_id.into(),
        filter.expense_category_id.into(),
        filter.approval_status.clone().into(),
        filter.payment_status.clone().into(),
        literal_substring(filter.base.employee_search.as_deref()).into(),
        literal_substring(filter.route_search.as_deref()).into(),
        clock.timezone_name().into(),
    ];
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn validate_owned_ids(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    filter: &ClaimTravelFilter,
) -> KabiPayResult<()> {
    for (table, id) in [
        ("department", filter.department_id),
        ("location", filter.location_id),
        ("expense_category", filter.expense_category_id),
        ("employee", filter.base.employee_id),
    ] {
        if let Some(id) = id {
            let found = db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!("SELECT id FROM {table} WHERE tenant_id=$1 AND id=$2"),
                    [tenant.into(), id.into()],
                ))
                .await?;
            if found.is_none() {
                return Err(KabiPayError::Validation(
                    "report filter selection is unavailable in this company".into(),
                ));
            }
        }
    }
    Ok(())
}

fn check_authority(claims: &ClientClaims, tenant: Uuid, kind: HrReportKind) -> KabiPayResult<()> {
    authorize(claims, kind)?;
    if claims.tenant_id != tenant {
        return Err(KabiPayError::Forbidden(
            "report tenant does not match authenticated tenant".into(),
        ));
    }
    Ok(())
}

fn rows_sql(source: &Source, limit: usize, offset: usize) -> String {
    let aliases: Vec<_> = (0..source.columns.len()).map(|i| format!("c{i}")).collect();
    let cells = aliases
        .iter()
        .map(|c| format!("coalesce({c}::text,'')"))
        .collect::<Vec<_>>()
        .join(",");
    format!("WITH report({},sort_order) AS (SELECT {},row_number() OVER (ORDER BY {}) FROM {} {BINDINGS} WHERE {COMMON_PREDICATE} AND {} ORDER BY {} LIMIT {limit} OFFSET {offset}) SELECT jsonb_build_array({cells}) AS cells FROM report ORDER BY sort_order", aliases.join(","), source.fields, source.order, source.from, source.predicate, source.order)
}

fn decode(rows: Vec<QueryResult>) -> KabiPayResult<Vec<Vec<String>>> {
    rows.into_iter()
        .map(|row| {
            let cells: serde_json::Value = row.try_get("", "cells")?;
            serde_json::from_value(cells)
                .map_err(|_| KabiPayError::Internal("report rows could not be read".into()))
        })
        .collect()
}

pub async fn load_page(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    kind: HrReportKind,
    filter: &ClaimTravelFilter,
    offset: i32,
    limit: i32,
    clock: TenantBusinessClock,
) -> KabiPayResult<HrReportRows> {
    check_authority(claims, tenant, kind)?;
    filter.validate(kind)?;
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(KabiPayError::Validation(
            "offset must be non-negative and limit between 1 and 100".into(),
        ));
    }
    let source = source(kind)?;
    let txn = db
        .begin_with_config(
            Some(IsolationLevel::RepeatableRead),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    validate_owned_ids(&txn, tenant, filter).await?;
    let sql = format!(
        "SELECT count(*) AS total FROM {} {BINDINGS} WHERE {COMMON_PREDICATE} AND {}",
        source.from, source.predicate
    );
    let count = txn
        .query_one(statement(sql, tenant, filter, &clock))
        .await?
        .ok_or_else(|| KabiPayError::Internal("report count is unavailable".into()))?;
    let total: i64 = count.try_get("", "total")?;
    let total_rows = i32::try_from(total)
        .map_err(|_| KabiPayError::Validation("report exceeds supported row count".into()))?;
    let rows = decode(
        txn.query_all(statement(
            rows_sql(&source, limit as usize, offset as usize),
            tenant,
            filter,
            &clock,
        ))
        .await?,
    )?;
    txn.commit().await?;
    Ok(HrReportRows {
        columns: source.columns.into_iter().map(str::to_owned).collect(),
        rows,
        total_rows,
    })
}

pub async fn load_csv(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    kind: HrReportKind,
    filter: &ClaimTravelFilter,
    clock: TenantBusinessClock,
) -> KabiPayResult<HrReportCsv> {
    check_authority(claims, tenant, kind)?;
    filter.validate(kind)?;
    let source = source(kind)?;
    let txn = db
        .begin_with_config(
            Some(IsolationLevel::RepeatableRead),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    validate_owned_ids(&txn, tenant, filter).await?;
    let rows = decode(
        txn.query_all(statement(
            rows_sql(&source, EXPORT_LIMIT + 1, 0),
            tenant,
            filter,
            &clock,
        ))
        .await?,
    )?;
    txn.commit().await?;
    validate_export_count(rows.len())?;
    let columns = source
        .columns
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(HrReportCsv {
        file_name: format!(
            "hr-{kind:?}-{}-{}.csv",
            filter.base.from_date, filter.base.to_date
        ),
        csv: render_csv(&columns, &rows),
        row_count: rows.len() as i32,
    })
}

pub fn validate_export_count(count: usize) -> KabiPayResult<()> {
    if count > EXPORT_LIMIT {
        return Err(KabiPayError::Validation(
            "Report has more than 10,000 rows. Narrow the filters and try again.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "claim_travel_report_tests.rs"]
mod tests;
