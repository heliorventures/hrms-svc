//! Validated stable cursors used only by performance administration list operations.

use async_graphql::Result;
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::KabiPayError;
use uuid::Uuid;

pub fn page_limit(value: i32) -> Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|limit| (1..=100).contains(limit))
        .ok_or_else(|| KabiPayError::Validation("Limit must be between 1 and 100".into()).into_graphql())
}

pub fn uuid_cursor(cursor: Option<&str>) -> Result<Option<Uuid>> {
    cursor.map(|value| Uuid::parse_str(value).map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())).transpose()
}

pub fn name_cursor(cursor: Option<&str>) -> Result<Option<(String, Uuid)>> {
    cursor.map(|value| {
        let (name, id) = value.rsplit_once('|').ok_or_else(|| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?;
        if name.is_empty() {
            return Err(KabiPayError::Validation("Cursor is invalid".into()).into_graphql());
        }
        Ok((name.to_owned(), Uuid::parse_str(id).map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?))
    }).transpose()
}

pub fn cycle_cursor(cursor: Option<&str>) -> Result<Option<(NaiveDate, Uuid)>> {
    cursor.map(|value| {
        let (start_date, id) = value.rsplit_once('|').ok_or_else(|| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?;
        let start_date = NaiveDate::parse_from_str(start_date, "%Y-%m-%d").map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?;
        Ok((start_date, Uuid::parse_str(id).map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?))
    }).transpose()
}

pub fn feedback_cursor(cursor: Option<&str>) -> Result<Option<(DateTime<Utc>, Uuid)>> {
    cursor.map(|value| {
        let (created_at, id) = value.rsplit_once('|').ok_or_else(|| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?;
        let created_at = DateTime::parse_from_rfc3339(created_at).map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?.with_timezone(&Utc);
        Ok((created_at, Uuid::parse_str(id).map_err(|_| KabiPayError::Validation("Cursor is invalid".into()).into_graphql())?))
    }).transpose()
}
