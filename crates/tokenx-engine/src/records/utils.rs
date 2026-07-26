//! Shared parsing helpers for local usage inputs.

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;

use super::error::{SessionParseError, SessionParseResult};

pub(crate) fn extract_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|val| {
        val.as_i64()
            .or_else(|| val.as_u64().map(|v| v as i64))
            .or_else(|| val.as_str().and_then(|s| s.parse::<i64>().ok()))
    })
}

pub(crate) fn extract_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|val| val.as_str().map(|s| s.to_string()))
}

pub(crate) fn parse_timestamp_value(value: &Value) -> Option<i64> {
    if let Some(ts) = value.as_str() {
        return parse_timestamp_str(ts);
    }

    let numeric = value
        .as_i64()
        .or_else(|| value.as_u64().map(|v| v as i64))?;
    if numeric <= 0 {
        return None;
    }
    if numeric >= 1_000_000_000_000 {
        Some(numeric)
    } else {
        Some(numeric * 1000)
    }
}

pub(crate) fn parse_timestamp_str(value: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(dt.timestamp_millis());
    }

    if let Ok(numeric) = value.parse::<i64>() {
        if numeric <= 0 {
            return None;
        }
        if numeric >= 1_000_000_000_000 {
            return Some(numeric);
        }
        return Some(numeric * 1000);
    }

    None
}

pub(crate) fn parse_epoch_f64_millis(value: f64) -> Option<i64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    let millis = if value >= 1_000_000_000_000.0 {
        value
    } else {
        value * 1000.0
    };
    if !millis.is_finite() || millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    Some(millis as i64)
}

/// Open a SQLite file for read-only access with no mutex (single-threaded parser use).
pub(crate) fn open_readonly_sqlite(path: &Path) -> SessionParseResult<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| SessionParseError::new("open SQLite input read-only", source))
}

pub(crate) fn read_file(path: &Path) -> SessionParseResult<Vec<u8>> {
    std::fs::read(path).map_err(|source| SessionParseError::new("read input file", source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timestamp_value_rejects_zero_and_negative_numbers() {
        assert!(parse_timestamp_value(&serde_json::json!(0)).is_none());
        assert!(parse_timestamp_value(&serde_json::json!(-1000)).is_none());
        assert!(parse_timestamp_value(&serde_json::json!(-1_700_000_000_000_i64)).is_none());
    }

    #[test]
    fn parse_timestamp_value_accepts_positive_numbers() {
        assert_eq!(
            parse_timestamp_value(&serde_json::json!(1_700_000_000_000_i64)),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            parse_timestamp_value(&serde_json::json!(1_700_000_000_i64)),
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn parse_timestamp_str_rejects_zero_and_negative_strings() {
        assert!(parse_timestamp_str("0").is_none());
        assert!(parse_timestamp_str("-5").is_none());
    }

    #[test]
    fn parse_epoch_f64_millis_accepts_seconds_and_milliseconds() {
        assert_eq!(
            parse_epoch_f64_millis(1_700_000_000.123),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse_epoch_f64_millis(1_700_000_000_123.0),
            Some(1_700_000_000_123)
        );
        assert_eq!(parse_epoch_f64_millis(f64::NAN), None);
        assert_eq!(parse_epoch_f64_millis(f64::INFINITY), None);
    }
}
