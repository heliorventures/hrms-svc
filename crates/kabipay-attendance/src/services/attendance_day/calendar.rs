use chrono::{DateTime, Duration, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use kabipay_common::{KabiPayError, KabiPayResult};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttendanceDayWindow {
    pub work_date: NaiveDate,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub boundary_minutes: i32,
    pub policy_version_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyVersion {
    pub id: Uuid,
    pub effective_work_date: NaiveDate,
    pub boundary_minutes: i32,
    pub timezone: String,
}

pub fn parse_boundary_minutes(value: &str) -> KabiPayResult<i32> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]].iter().all(u8::is_ascii_digit)
    {
        return Err(invalid("attendance start time must use HH:MM"));
    }
    let hours = i32::from(bytes[0] - b'0') * 10 + i32::from(bytes[1] - b'0');
    let minutes = i32::from(bytes[3] - b'0') * 10 + i32::from(bytes[4] - b'0');
    if hours > 23 || minutes > 59 {
        return Err(invalid("attendance start time is outside the day"));
    }
    Ok(hours * 60 + minutes)
}

pub(super) fn invalid(message: &str) -> KabiPayError {
    KabiPayError::Validation(message.into())
}

pub(super) fn validate_minutes(minutes: i32) -> KabiPayResult<()> {
    if !(0..1440).contains(&minutes) {
        return Err(invalid("attendance boundary minutes must be between 0 and 1439"));
    }
    Ok(())
}

fn selected(versions: &[PolicyVersion], date: NaiveDate) -> KabiPayResult<&PolicyVersion> {
    versions.iter()
        .filter(|v| v.effective_work_date <= date)
        .max_by_key(|v| v.effective_work_date)
        .ok_or_else(|| invalid("no attendance policy covers this work date"))
}

fn boundary(date: NaiveDate, policy: &PolicyVersion) -> KabiPayResult<DateTime<Utc>> {
    validate_minutes(policy.boundary_minutes)?;
    let timezone = policy.timezone.parse::<Tz>()
        .map_err(|_| invalid("invalid attendance timezone"))?;
    let minutes = u32::try_from(policy.boundary_minutes)
        .map_err(|_| invalid("invalid attendance boundary"))?;
    let mut local = date.and_hms_opt(minutes / 60, minutes % 60, 0)
        .ok_or_else(|| invalid("attendance boundary overflows"))?;
    // IANA transitions include historical second offsets and skipped whole dates.
    // Advance by seconds so the result is the first valid instant, not merely the next valid minute.
    for _ in 0..=172_800 {
        match timezone.from_local_datetime(&local) {
            LocalResult::Single(value) => return Ok(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(a, b) => return Ok(a.min(b).with_timezone(&Utc)),
            LocalResult::None => {
                local = local.checked_add_signed(Duration::seconds(1))
                    .ok_or_else(|| invalid("attendance boundary overflows"))?;
            }
        }
    }
    Err(invalid("attendance timezone gap exceeds supported two-day search"))
}

/// Resolve a day with its predecessor's endpoint as the inherited start.
pub fn resolve_window(versions: &[PolicyVersion], date: NaiveDate) -> KabiPayResult<AttendanceDayWindow> {
    let next_date = date.succ_opt()
        .ok_or_else(|| invalid("attendance date overflows"))?;
    let previous_date = date.pred_opt()
        .ok_or_else(|| invalid("attendance date overflows"))?;
    let current = selected(versions, date)?;
    let previous = if current.effective_work_date == date {
        selected(versions, previous_date).unwrap_or(current)
    } else {
        current
    };
    let starts_at = boundary(date, previous)?;
    let ends_at = boundary(next_date, current)?;
    if starts_at >= ends_at {
        return Err(invalid("attendance window must have increasing endpoints"));
    }
    Ok(AttendanceDayWindow {
        work_date: date,
        starts_at,
        ends_at,
        timezone: current.timezone.clone(),
        boundary_minutes: current.boundary_minutes,
        policy_version_id: current.id,
    })
}

/// UTC-neighbor candidates also cover transition days across timezone changes.
pub fn resolve_current_window(
    versions: &[PolicyVersion],
    now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayWindow> {
    let mut found = None;
    for offset in -3..=3 {
        let Some(date) = now.date_naive().checked_add_signed(Duration::days(offset)) else {
            continue;
        };
        // A skipped local date can have no window. Nearby valid days remain candidates.
        if let Ok(window) = resolve_window(versions, date) {
            if window.starts_at <= now && now < window.ends_at {
                if found.is_some() {
                    return Err(invalid("attendance policy produces overlapping windows"));
                }
                found = Some(window);
            }
        }
    }
    found.ok_or_else(|| invalid("no attendance window contains the current instant"))
}
