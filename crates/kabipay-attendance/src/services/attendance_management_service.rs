//! Cursor-paginated tenant attendance queries for self-service and attendance management.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use kabipay_common::{client_data_scope::EmployeeScopeFilter, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0010_time_shift_roster::attendance,
};
use sea_orm::{
    sea_query::Expr, ColumnTrait, Condition, DatabaseConnection, EntityTrait, JoinType,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect,
};
use uuid::Uuid;

const DEFAULT_PAGE_SIZE: u64 = 50;
const MAX_PAGE_SIZE: u64 = 100;
const MAX_CALENDAR_DAYS: i64 = 92;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttendanceCursor {
    work_date: NaiveDate,
    created_at: DateTime<Utc>,
    id: Uuid,
}

impl AttendanceCursor {
    pub fn new(work_date: NaiveDate, created_at: DateTime<Utc>, id: Uuid) -> Self {
        Self {
            work_date,
            created_at,
            id,
        }
    }

    pub fn encode(&self) -> String {
        let value = format!(
            "{}|{}|{}",
            self.work_date,
            self.created_at
                .to_rfc3339_opts(SecondsFormat::Nanos, true),
            self.id
        );
        URL_SAFE_NO_PAD.encode(value)
    }

    pub fn decode(raw: &str) -> KabiPayResult<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw.trim())
            .map_err(|_| KabiPayError::Validation("invalid attendance cursor".into()))?;
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| KabiPayError::Validation("invalid attendance cursor".into()))?;
        let mut parts = value.split('|');
        let work_date = parts
            .next()
            .ok_or_else(|| KabiPayError::Validation("invalid attendance cursor".into()))?
            .parse::<NaiveDate>()
            .map_err(|_| KabiPayError::Validation("invalid attendance cursor".into()))?;
        let created_at = parts
            .next()
            .ok_or_else(|| KabiPayError::Validation("invalid attendance cursor".into()))?
            .parse::<DateTime<Utc>>()
            .map_err(|_| KabiPayError::Validation("invalid attendance cursor".into()))?;
        let id = parts
            .next()
            .ok_or_else(|| KabiPayError::Validation("invalid attendance cursor".into()))?
            .parse::<Uuid>()
            .map_err(|_| KabiPayError::Validation("invalid attendance cursor".into()))?;
        if parts.next().is_some() {
            return Err(KabiPayError::Validation("invalid attendance cursor".into()));
        }
        Ok(Self::new(work_date, created_at, id))
    }
}

#[derive(Clone, Debug)]
pub struct AttendancePage<T> {
    pub rows: Vec<T>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

#[derive(Clone, Debug)]
pub struct ManagedAttendanceRow {
    pub attendance: attendance::Model,
    pub employee_name: String,
    pub employee_code: String,
}

/// Minimal server-side employee data used only to canonicalize an authorized search.
#[derive(Clone, Debug, FromQueryResult)]
struct EmployeeSearchProjection {
    id: Uuid,
    first_name: String,
    last_name: String,
    employee_code: String,
}

pub fn page_size(first: Option<i32>) -> KabiPayResult<u64> {
    match first {
        None => Ok(DEFAULT_PAGE_SIZE),
        Some(value) if value < 1 => Err(KabiPayError::Validation(
            "first must be between 1 and 100".into(),
        )),
        Some(value) => {
            let value = u64::try_from(value).map_err(|_| {
                KabiPayError::Validation("first must be between 1 and 100".into())
            })?;
            if value > MAX_PAGE_SIZE {
                return Err(KabiPayError::Validation(
                    "first must be between 1 and 100".into(),
                ));
            }
            Ok(value)
        }
    }
}

pub fn validate_date_range(from_date: NaiveDate, to_date: NaiveDate) -> KabiPayResult<()> {
    if to_date < from_date {
        return Err(KabiPayError::Validation(
            "toDate must be on or after fromDate".into(),
        ));
    }
    let day_count = to_date.signed_duration_since(from_date).num_days() + 1;
    if day_count > MAX_CALENDAR_DAYS {
        return Err(KabiPayError::Validation(
            "attendance date range cannot exceed 92 calendar days".into(),
        ));
    }
    Ok(())
}

fn cursor_from_attendance(row: &attendance::Model) -> AttendanceCursor {
    AttendanceCursor::new(row.work_date, row.created_at, row.id)
}

fn cursor_filter(cursor: &AttendanceCursor) -> Condition {
    Condition::any()
        .add(attendance::Column::WorkDate.lt(cursor.work_date))
        .add(
            Condition::all()
                .add(attendance::Column::WorkDate.eq(cursor.work_date))
                .add(attendance::Column::CreatedAt.lt(cursor.created_at)),
        )
        .add(
            Condition::all()
                .add(attendance::Column::WorkDate.eq(cursor.work_date))
                .add(attendance::Column::CreatedAt.eq(cursor.created_at))
                .add(attendance::Column::Id.lt(cursor.id)),
        )
}

#[cfg(test)]
fn is_after_cursor(
    work_date: NaiveDate,
    created_at: DateTime<Utc>,
    id: Uuid,
    cursor: &AttendanceCursor,
) -> bool {
    (work_date, created_at, id) < (cursor.work_date, cursor.created_at, cursor.id)
}

fn decode_after(after: Option<&str>) -> KabiPayResult<Option<AttendanceCursor>> {
    after
        .filter(|value| !value.trim().is_empty())
        .map(AttendanceCursor::decode)
        .transpose()
}

fn normalize_employee_search(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            token
                .chars()
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn matching_authorized_employee_ids(
    employees: &[EmployeeSearchProjection],
    scope: &EmployeeScopeFilter,
    employee_id: Option<Uuid>,
    employee_search: &str,
) -> Vec<Uuid> {
    let needle = normalize_employee_search(employee_search);
    if needle.is_empty() {
        return Vec::new();
    }

    employees
        .iter()
        .filter(|employee| scope.allows_employee(employee.id))
        .filter(|employee| employee_id.is_none_or(|id| employee.id == id))
        .filter(|employee| {
            let full_name = normalize_employee_search(&format!(
                "{} {}",
                employee.first_name, employee.last_name
            ));
            let employee_code = normalize_employee_search(&employee.employee_code);
            full_name.contains(&needle) || employee_code.contains(&needle)
        })
        .map(|employee| employee.id)
        .collect()
}

fn page_from_rows<T>(mut rows: Vec<T>, limit: u64, cursor_for: impl Fn(&T) -> AttendanceCursor) -> AttendancePage<T> {
    let has_next_page = rows.len() > limit as usize;
    if has_next_page {
        rows.pop();
    }
    let end_cursor = rows.last().map(|row| cursor_for(row).encode());
    AttendancePage {
        rows,
        end_cursor,
        has_next_page,
    }
}

pub async fn list_my_attendance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    from_date: NaiveDate,
    to_date: NaiveDate,
    first: Option<i32>,
    after: Option<&str>,
) -> KabiPayResult<AttendancePage<attendance::Model>> {
    validate_date_range(from_date, to_date)?;
    let limit = page_size(first)?;
    let after = decode_after(after)?;
    let mut query = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.gte(from_date))
        .filter(attendance::Column::WorkDate.lte(to_date));
    if let Some(cursor) = after.as_ref() {
        query = query.filter(cursor_filter(cursor));
    }
    let rows = query
        .order_by_desc(attendance::Column::WorkDate)
        .order_by_desc(attendance::Column::CreatedAt)
        .order_by_desc(attendance::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(page_from_rows(rows, limit, cursor_from_attendance))
}

pub async fn list_managed_attendance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: &EmployeeScopeFilter,
    from_date: NaiveDate,
    to_date: NaiveDate,
    employee_search: Option<&str>,
    employee_id: Option<Uuid>,
    first: Option<i32>,
    after: Option<&str>,
) -> KabiPayResult<AttendancePage<ManagedAttendanceRow>> {
    validate_date_range(from_date, to_date)?;
    let limit = page_size(first)?;
    let after = decode_after(after)?;
    if matches!(scope, EmployeeScopeFilter::Empty)
        || matches!(scope, EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty())
    {
        return Ok(AttendancePage {
            rows: Vec::new(),
            end_cursor: None,
            has_next_page: false,
        });
    }

    let matching_employee_ids = if let Some(search) = employee_search
        .map(normalize_employee_search)
        .filter(|search| !search.is_empty())
    {
        let mut employee_query = employee::Entity::find()
            .select_only()
            .column(employee::Column::Id)
            .column(employee::Column::FirstName)
            .column(employee::Column::LastName)
            .column(employee::Column::EmployeeCode)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .filter(employee::Column::Status.eq("ACTIVE"));

        match scope {
            EmployeeScopeFilter::Unrestricted => {}
            EmployeeScopeFilter::Empty => {
                return Ok(AttendancePage {
                    rows: Vec::new(),
                    end_cursor: None,
                    has_next_page: false,
                });
            }
            EmployeeScopeFilter::EmployeeIds(ids) => {
                employee_query =
                    employee_query.filter(employee::Column::Id.is_in(ids.clone()));
            }
        }
        if let Some(employee_id) = employee_id {
            employee_query = employee_query.filter(employee::Column::Id.eq(employee_id));
        }

        let employees = employee_query
            .into_model::<EmployeeSearchProjection>()
            .all(db)
            .await
            .map_err(KabiPayError::from)?;
        let employee_ids =
            matching_authorized_employee_ids(&employees, scope, employee_id, &search);
        if employee_ids.is_empty() {
            return Ok(AttendancePage {
                rows: Vec::new(),
                end_cursor: None,
                has_next_page: false,
            });
        }
        Some(employee_ids)
    } else {
        None
    };

    let mut query = attendance::Entity::find()
        .join(
            JoinType::InnerJoin,
            attendance::Entity::belongs_to(employee::Entity)
                .from(attendance::Column::EmployeeId)
                .to(employee::Column::Id)
                .on_condition(|left, right| {
                    Condition::all().add(
                        Expr::col((left, attendance::Column::TenantId))
                            .equals((right, employee::Column::TenantId)),
                    )
                })
                .into(),
        )
        .select_also(employee::Entity)
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.eq("ACTIVE"));

    match scope {
        EmployeeScopeFilter::Unrestricted => {}
        EmployeeScopeFilter::Empty => {
            return Ok(AttendancePage {
                rows: Vec::new(),
                end_cursor: None,
                has_next_page: false,
            });
        }
        EmployeeScopeFilter::EmployeeIds(ids) => {
            query = query.filter(attendance::Column::EmployeeId.is_in(ids.clone()));
        }
    }

    query = query
        .filter(attendance::Column::WorkDate.gte(from_date))
        .filter(attendance::Column::WorkDate.lte(to_date));
    if let Some(cursor) = after.as_ref() {
        query = query.filter(cursor_filter(cursor));
    }
    if let Some(employee_id) = employee_id {
        query = query.filter(attendance::Column::EmployeeId.eq(employee_id));
    }
    if let Some(employee_ids) = matching_employee_ids {
        query = query.filter(attendance::Column::EmployeeId.is_in(employee_ids));
    }

    let rows = query
        .order_by_desc(attendance::Column::WorkDate)
        .order_by_desc(attendance::Column::CreatedAt)
        .order_by_desc(attendance::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let rows = rows
        .into_iter()
        .map(|(attendance, employee)| {
            let employee = employee.ok_or_else(|| {
                KabiPayError::Internal("active employee join returned no employee row".into())
            })?;
            Ok(ManagedAttendanceRow {
                employee_name: format!("{} {}", employee.first_name, employee.last_name)
                    .trim()
                    .to_string(),
                employee_code: employee.employee_code,
                attendance,
            })
        })
        .collect::<KabiPayResult<Vec<_>>>()?;
    Ok(page_from_rows(rows, limit, |row| {
        cursor_from_attendance(&row.attendance)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone, Utc};
    use uuid::Uuid;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date is valid")
    }

    fn timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 24, 10, 30, 0)
            .single()
            .expect("test timestamp is valid")
    }

    fn uuid() -> Uuid {
        Uuid::from_u128(42)
    }

    #[test]
    fn cursor_round_trips_complete_sort_key() {
        let key = AttendanceCursor::new(date(2026, 8, 24), timestamp(), uuid());

        assert_eq!(AttendanceCursor::decode(&key.encode()).unwrap(), key);
    }

    #[test]
    fn rejects_malformed_cursor_input() {
        assert!(AttendanceCursor::decode("not-a-valid-cursor").is_err());
    }

    #[test]
    fn cursor_accepts_only_rows_after_the_complete_descending_sort_key() {
        let cursor = AttendanceCursor::new(date(2026, 8, 24), timestamp(), Uuid::from_u128(42));

        assert!(is_after_cursor(
            date(2026, 8, 23),
            timestamp(),
            Uuid::from_u128(999),
            &cursor,
        ));
        assert!(is_after_cursor(
            date(2026, 8, 24),
            Utc.with_ymd_and_hms(2026, 8, 24, 10, 29, 59)
                .single()
                .expect("test timestamp is valid"),
            Uuid::from_u128(999),
            &cursor,
        ));
        assert!(is_after_cursor(
            date(2026, 8, 24),
            timestamp(),
            Uuid::from_u128(41),
            &cursor,
        ));
        assert!(!is_after_cursor(
            date(2026, 8, 24),
            timestamp(),
            Uuid::from_u128(42),
            &cursor,
        ));
        assert!(!is_after_cursor(
            date(2026, 8, 25),
            timestamp(),
            Uuid::from_u128(1),
            &cursor,
        ));
    }

    #[test]
    fn rejects_ranges_over_ninety_two_days() {
        assert!(validate_date_range(date(2026, 5, 1), date(2026, 8, 2)).is_err());
    }

    #[test]
    fn date_range_accepts_ninety_two_days_and_rejects_reversed_or_ninety_three_days() {
        assert!(validate_date_range(date(2026, 5, 3), date(2026, 8, 2)).is_ok());
        assert!(validate_date_range(date(2026, 8, 2), date(2026, 5, 3)).is_err());
        assert!(validate_date_range(date(2026, 5, 2), date(2026, 8, 2)).is_err());
    }

    #[test]
    fn page_size_defaults_to_fifty_and_caps_at_one_hundred() {
        assert_eq!(page_size(None).unwrap(), 50);
        assert_eq!(page_size(Some(100)).unwrap(), 100);
        assert!(page_size(Some(101)).is_err());
    }

    #[test]
    fn page_size_rejects_zero_negative_and_values_over_one_hundred() {
        assert!(page_size(Some(0)).is_err());
        assert!(page_size(Some(-1)).is_err());
        assert!(page_size(Some(101)).is_err());
    }

    #[test]
    fn sentinel_page_truncates_the_extra_row_and_uses_the_last_returned_cursor() {
        let first = Uuid::from_u128(3);
        let second = Uuid::from_u128(2);
        let sentinel = Uuid::from_u128(1);
        let page = page_from_rows(vec![first, second, sentinel], 2, |id| {
            AttendanceCursor::new(date(2026, 8, 24), timestamp(), *id)
        });

        assert!(page.has_next_page);
        assert_eq!(page.rows, vec![first, second]);
        assert!(!page.rows.contains(&sentinel));
        assert_eq!(
            page.end_cursor,
            Some(AttendanceCursor::new(date(2026, 8, 24), timestamp(), second).encode())
        );
    }

    #[test]
    fn employee_search_matches_unicode_case_without_database_collation() {
        let employee_id = Uuid::from_u128(100);
        let rows = vec![employee_projection(employee_id, "ÉLODIE", "Durand", "EMP-100")];

        assert_eq!(
            matching_authorized_employee_ids(
                &rows,
                &EmployeeScopeFilter::Unrestricted,
                None,
                "élodie",
            ),
            vec![employee_id]
        );
    }

    #[test]
    fn employee_search_matches_irregular_stored_whitespace() {
        let employee_id = Uuid::from_u128(101);
        let rows = vec![employee_projection(
            employee_id,
            "  Ana\t",
            "\n  María  ",
            "EMP-101",
        )];

        assert_eq!(
            matching_authorized_employee_ids(
                &rows,
                &EmployeeScopeFilter::Unrestricted,
                None,
                "ana maría",
            ),
            vec![employee_id]
        );
    }

    #[test]
    fn employee_search_normalizes_employee_codes_with_the_same_rules() {
        let employee_id = Uuid::from_u128(102);
        let rows = vec![employee_projection(employee_id, "Code", "Owner", "  HR\tÉ-42  ")];

        assert_eq!(
            matching_authorized_employee_ids(
                &rows,
                &EmployeeScopeFilter::Unrestricted,
                None,
                "hr é-42",
            ),
            vec![employee_id]
        );
    }

    #[test]
    fn employee_search_returns_no_ids_when_no_authorized_projection_matches() {
        let rows = vec![employee_projection(
            Uuid::from_u128(103),
            "Mina",
            "Patel",
            "EMP-103",
        )];

        assert!(matching_authorized_employee_ids(
            &rows,
            &EmployeeScopeFilter::Unrestricted,
            None,
            "not present",
        )
        .is_empty());
    }

    #[test]
    fn employee_scope_filters_projection_before_canonical_matching() {
        let allowed_id = Uuid::from_u128(104);
        let excluded_id = Uuid::from_u128(105);
        let rows = vec![
            employee_projection(excluded_id, "Élodie", "Outside", "EMP-105"),
            employee_projection(allowed_id, "Élodie", "Inside", "EMP-104"),
        ];

        assert_eq!(
            matching_authorized_employee_ids(
                &rows,
                &EmployeeScopeFilter::EmployeeIds(vec![allowed_id]),
                None,
                "élodie",
            ),
            vec![allowed_id]
        );
    }

    #[test]
    fn employee_search_normalizes_unicode_case_and_irregular_whitespace() {
        assert_eq!(
            normalize_employee_search("  ÉLODIE\t\n  DUPONT  "),
            "élodie dupont"
        );
    }

    fn employee_projection(
        id: Uuid,
        first_name: &str,
        last_name: &str,
        employee_code: &str,
    ) -> EmployeeSearchProjection {
        EmployeeSearchProjection {
            id,
            first_name: first_name.into(),
            last_name: last_name.into(),
            employee_code: employee_code.into(),
        }
    }
}
