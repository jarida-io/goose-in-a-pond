//! Column readers and writers the idea engine's stores share. Times are RFC 3339 to the second
//! in UTC, so they compare as text.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::Row;

pub(crate) use crate::sqlite_interests::{parse_time, stamp};

pub(crate) fn maybe_stamp(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(stamp)
}

pub(crate) fn time_of(row: &SqliteRow, column: &str) -> Result<DateTime<Utc>> {
    let raw: String = row.try_get(column)?;
    parse_time(&raw)
}

pub(crate) fn maybe_time_of(row: &SqliteRow, column: &str) -> Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.try_get(column)?;
    raw.as_deref().map(parse_time).transpose()
}

pub(crate) fn json_of<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).context("write a JSON column")
}

/// A JSON column; one this build cannot read is its default, not an error.
pub(crate) fn json_in<T: serde::de::DeserializeOwned + Default>(
    row: &SqliteRow,
    column: &str,
) -> Result<T> {
    let raw: Option<String> = row.try_get(column)?;
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

/// A column whose words a `CHECK` constrains, read with the type's own `parse`.
pub(crate) fn word_in<T>(
    row: &SqliteRow,
    column: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    let raw: String = row.try_get(column)?;
    parse(&raw).with_context(|| format!("unreadable {column} {raw:?}"))
}

pub(crate) fn count_in(row: &SqliteRow, column: &str) -> Result<u32> {
    let raw: i64 = row.try_get(column)?;
    Ok(raw.clamp(0, i64::from(u32::MAX)) as u32)
}

pub(crate) fn flag_in(row: &SqliteRow, column: &str) -> Result<bool> {
    Ok(row.try_get::<i64, _>(column)? != 0)
}

#[cfg(test)]
pub(crate) mod testing {
    use chrono::{DateTime, TimeZone, Utc};
    use sqlx::{Pool, Sqlite};

    /// A migrated in-memory database with two members, `jerry` and `liz`.
    pub(crate) async fn pool() -> Pool<Sqlite> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        for member in ["jerry", "liz"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(member)
                .bind(member)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    /// A whole second, so a time read back equals the one written.
    pub(crate) fn noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }
}
