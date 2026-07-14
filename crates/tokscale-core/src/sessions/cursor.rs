//! Cursor IDE session parser
//!
//! Parses CSV files from the Cursor usage export API.
//! CSV files are cached locally at ~/.config/tokscale/cursor-cache/*.csv
//! (legacy single-account cache uses usage.csv; additional accounts may use usage.<account>.csv)
//!
//! CSV Formats:
//! - v1 (old): Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you
//! - v2 (new): Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
//! - v3 (latest): Date,Cloud Agent ID,Automation ID,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost

use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
use crate::source_health::{RecordRejectionReason, ScannedSource};
use crate::{provider_identity, TokenBreakdown};
use std::path::Path;

fn account_id_from_cursor_cache_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("usage.csv");

    if file_name == "usage.csv" {
        return "active".to_string();
    }

    if let Some(stem) = file_name
        .strip_prefix("usage.")
        .and_then(|s| s.strip_suffix(".csv"))
    {
        // Keep it simple/ASCII. The CLI already sanitizes file names.
        let cleaned = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        if cleaned.is_empty() {
            return "unknown".to_string();
        }
        return cleaned;
    }

    "unknown".to_string()
}

/// Provider inference from model name
fn infer_provider(model: &str) -> &'static str {
    provider_identity::inferred_provider_from_model(model).unwrap_or("cursor")
}

/// Parse a Cursor usage CSV file
///
/// Handles both formats:
/// - New: Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
/// - Old: Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you
pub fn parse_cursor_file(path: &Path) -> SessionParseResult<ScannedSource> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| SessionParseError::new("read CSV file", error))?;

    let mut scanned = ScannedSource {
        messages: Vec::with_capacity(128),
        ..ScannedSource::default()
    };
    let mut lines = content.lines();

    // Parse header line to determine column indices
    let header = match lines.next() {
        Some(h) => h,
        None => return Ok(scanned),
    };

    // Verify this is a valid Cursor CSV
    if !header.contains("Date") || !header.contains("Model") {
        return Err(SessionParseError::invalid(
            "validate CSV header",
            "missing required Date or Model column",
        ));
    }

    // Detect format by checking for "Kind" column and column count
    let header_fields: Vec<&str> = parse_csv_line(header);
    let has_kind_column = header_fields.iter().any(|f| f.trim() == "Kind");
    let column_count = header_fields.len();

    // Column indices based on format
    let (model_idx, input_cache_write_idx, input_no_cache_idx, cache_read_idx, output_idx) =
        if has_kind_column && column_count >= 11 {
            // v3 format: Date,Cloud Agent ID,Automation ID,Kind,Model,...
            (4, 6, 7, 8, 9)
        } else if has_kind_column {
            // v2 format: Date,Kind,Model,Max Mode,Input (w/ Cache Write),...
            (2, 4, 5, 6, 7)
        } else {
            // v1 format: Date,Model,Input (w/ Cache Write),...
            (1, 2, 3, 4, 5)
        };

    let account_id = account_id_from_cursor_cache_path(path);

    for (row_index, line) in lines.enumerate() {
        let row_number = row_index + 2;
        if line.trim().is_empty() {
            continue;
        }

        // Parse CSV line (simple parsing, handles quoted fields)
        let fields: Vec<&str> = parse_csv_line(line);

        // Need at least enough columns for the format
        let min_fields = output_idx + 1;
        if fields.len() < min_fields {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!(
                        "{} row {row_number}: expected at least {min_fields} columns, found {}",
                        path.display(),
                        fields.len()
                    )
                });
            continue;
        }

        let date_str = fields[0].trim().trim_matches('"');
        let model = fields[model_idx].trim().trim_matches('"');
        let parse_count = |value: &str| {
            value
                .trim()
                .trim_matches('"')
                .parse::<i64>()
                .map_err(|error| SessionParseError::new("decode CSV token count", error))
        };
        let counts = [
            fields[input_cache_write_idx],
            fields[input_no_cache_idx],
            fields[cache_read_idx],
            fields[output_idx],
        ]
        .map(parse_count);
        let [Ok(input_with_cache_write), Ok(input_without_cache_write), Ok(cache_read), Ok(output_tokens)] =
            counts
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!("{} row {row_number}: invalid token count", path.display())
                });
            continue;
        };
        if [
            input_with_cache_write,
            input_without_cache_write,
            cache_read,
            output_tokens,
        ]
        .into_iter()
        .any(|tokens| tokens < 0)
        {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!(
                        "{} row {row_number}: Cursor token counts must be non-negative",
                        path.display()
                    )
                });
            continue;
        }
        if input_with_cache_write < input_without_cache_write {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!(
                        "{} row {row_number}: input with cache write is smaller than input without cache write",
                        path.display()
                    )
                });
            continue;
        }

        // Cache write = input_with_cache_write - input_without_cache_write
        let cache_write = input_with_cache_write - input_without_cache_write;
        // Input tokens = input_without_cache_write
        let input = input_without_cache_write;
        let tokens = TokenBreakdown {
            input: input.max(0),
            output: output_tokens.max(0),
            cache_read: cache_read.max(0),
            cache_write,
            reasoning: 0,
        };
        let Some(token_total) = tokens.checked_total() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!(
                        "{} row {row_number}: Cursor token total exceeds i64",
                        path.display()
                    )
                });
            continue;
        };
        if token_total == 0 {
            continue;
        }

        if model.is_empty() {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel, || {
                    format!(
                        "{} row {row_number}: token-bearing row has an empty model",
                        path.display()
                    )
                });
            continue;
        }

        // Parse timestamp from date string
        let timestamp = parse_date_to_timestamp(date_str);
        if timestamp == 0 {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp, || {
                    format!(
                        "{} row {row_number}: invalid Cursor usage date `{date_str}`",
                        path.display()
                    )
                });
            continue;
        }

        scanned.messages.push(UnifiedMessage::new(
            "cursor",
            model,
            infer_provider(model),
            format!("cursor-{}-{}", account_id, date_str),
            timestamp,
            tokens,
            0.0,
        ));
    }

    Ok(scanned)
}

/// Simple CSV line parser that handles quoted fields
fn parse_csv_line(line: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let bytes = line.as_bytes();

    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                fields.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }

    // Add the last field
    if start <= line.len() {
        fields.push(&line[start..]);
    }

    fields
}

/// Parse a date string to Unix milliseconds timestamp
fn parse_date_to_timestamp(date_str: &str) -> i64 {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};

    // Try ISO 8601 format with milliseconds: "2025-02-05T12:00:00.123Z"
    if let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y-%m-%dT%H:%M:%S%.3fZ") {
        return Utc.from_utc_datetime(&dt).timestamp_millis();
    }

    // Try ISO 8601 format with time: "2025-02-05T12:00:00Z"
    if let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y-%m-%dT%H:%M:%SZ") {
        return Utc.from_utc_datetime(&dt).timestamp_millis();
    }

    // Try ISO 8601 format with milliseconds without Z: "2025-02-05T12:00:00.123"
    if let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y-%m-%dT%H:%M:%S%.3f") {
        return Utc.from_utc_datetime(&dt).timestamp_millis();
    }

    // Try ISO 8601 format with time without Z: "2025-02-05T12:00:00"
    if let Ok(dt) = NaiveDateTime::parse_from_str(date_str, "%Y-%m-%dT%H:%M:%S") {
        return Utc.from_utc_datetime(&dt).timestamp_millis();
    }

    // Date-only format: "2025-02-05" - use noon UTC (12:00:00Z)
    // Noon keeps the local date stable for all timezones from UTC-12 to UTC+14,
    // so filtering by local day boundaries won't shift the record to an adjacent day.
    if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        let dt = date.and_hms_opt(12, 0, 0).unwrap();
        return Utc.from_utc_datetime(&dt).timestamp_millis();
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_cursor_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_cursor_file(path).unwrap().messages
    }

    #[test]
    fn rejects_csv_without_required_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        std::fs::write(&path, "Kind,Input,Output\nchat,1,2\n").unwrap();

        let error = super::parse_cursor_file(&path).unwrap_err();
        assert_eq!(error.operation(), "validate CSV header");
    }

    #[test]
    fn missing_csv_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.csv");

        let error = super::parse_cursor_file(&path).unwrap_err();
        assert_eq!(error.operation(), "read CSV file");
    }

    #[test]
    fn mixed_rows_reject_bad_record_and_keep_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        std::fs::write(
            &path,
            "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you\n\
             2025-02-01,gpt-4o,10,8,1,2,11,$0,$0\n\
             2025-02-02,gpt-4o,not-a-number,8,1,2,11,$0,$0\n\
             2025-02-03,gpt-4o,20,16,2,4,22,$0,$0\n",
        )
        .unwrap();

        let scanned = super::parse_cursor_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_row_tokens_are_malformed_instead_of_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        std::fs::write(
            &path,
            "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you\n\
             2025-02-01,gpt-4o,10,8,1,2,11,$0,$0\n\
             2025-02-02,gpt-4o,10,-8,1,2,11,$0,$0\n\
             2025-02-03,gpt-4o,20,16,2,4,22,$0,$0\n",
        )
        .unwrap();

        let scanned = super::parse_cursor_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn inconsistent_cache_write_inputs_are_malformed_and_later_row_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        std::fs::write(
            &path,
            "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you\n\
             2025-02-01,gpt-4o,10,20,1,2,23,$0,$0\n\
             2025-02-02,gpt-4o,20,16,2,4,22,$0,$0\n",
        )
        .unwrap();

        let scanned = super::parse_cursor_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 16);
        assert_eq!(scanned.messages[0].tokens.cache_write, 4);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_row_tokens_are_malformed_and_later_row_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.csv");
        std::fs::write(
            &path,
            "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you\n\
             2025-02-01,gpt-4o,9223372036854775807,9223372036854775807,0,1,0,$0,$0\n\
             2025-02-02,gpt-4o,20,16,2,4,22,$0,$0\n",
        )
        .unwrap();

        let scanned = super::parse_cursor_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 16);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_infer_provider() {
        assert_eq!(infer_provider("claude-3-sonnet"), "anthropic");
        assert_eq!(infer_provider("gpt-4o"), "openai");
        assert_eq!(infer_provider("gemini-pro"), "google");
        assert_eq!(infer_provider("deepseek-coder"), "deepseek");
        assert_eq!(infer_provider("llama-3"), "meta");
        assert_eq!(infer_provider("unknown-model"), "cursor");
    }

    #[test]
    fn test_parse_csv_line() {
        let line = "2025-02-01,gpt-4o,10,5,0,15,30,$0.10,$0.10";
        let fields = parse_csv_line(line);
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0], "2025-02-01");
        assert_eq!(fields[1], "gpt-4o");
        assert_eq!(fields[8], "$0.10");
    }

    #[test]
    fn test_parse_date_to_timestamp() {
        // ISO with milliseconds and Z (new Cursor format)
        let ts = parse_date_to_timestamp("2025-11-13T18:36:05.846Z");
        assert!(ts > 0);

        // ISO with Z
        let ts = parse_date_to_timestamp("2025-02-05T12:00:00Z");
        assert!(ts > 0);

        // Date only
        let ts = parse_date_to_timestamp("2025-02-05");
        assert!(ts > 0);

        // Invalid
        let ts = parse_date_to_timestamp("invalid");
        assert_eq!(ts, 0);
    }

    #[test]
    fn test_parse_cursor_csv_sample_old_format() {
        let csv = "Date,Model,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost,Cost to you
2025-02-01,gpt-4o,10,5,0,15,30,$0.10,$0.10
2025-02-02,gpt-4o-mini,0,0,0,5,5,$0.05,$0.05";

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("usage.csv");
        std::fs::write(&file_path, csv).unwrap();

        let messages = parse_cursor_file(&file_path);
        assert_eq!(messages.len(), 2);

        assert_eq!(messages[0].client.as_ref(), "cursor");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-4o");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.input, 5);
        assert_eq!(messages[0].tokens.output, 15);
        assert_eq!(messages[0].tokens.cache_write, 5); // 10 - 5
        assert_eq!(messages[0].cost, 0.0);

        assert_eq!(messages[1].model_id.as_ref(), "gpt-4o-mini");
    }

    #[test]
    fn test_parse_cursor_csv_sample_new_format() {
        // Cursor API column layout with internally consistent cache-write totals.
        let csv = r#"Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
"2025-11-13T18:36:05.846Z","Included","auto","No","28342","775","105891","21282","156290","0.19"
"2025-11-13T13:35:04.658Z","On-Demand","gpt-5-codex","No","8263","8263","66964","1612","76839","0.03""#;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("usage.csv");
        std::fs::write(&file_path, csv).unwrap();

        let messages = parse_cursor_file(&file_path);
        assert_eq!(messages.len(), 2);

        // First message: auto model
        assert_eq!(messages[0].client.as_ref(), "cursor");
        assert_eq!(messages[0].model_id.as_ref(), "auto");
        assert_eq!(messages[0].provider_id.as_ref(), "cursor"); // unknown model -> cursor
        assert_eq!(messages[0].tokens.input, 775);
        assert_eq!(messages[0].tokens.output, 21282);
        assert_eq!(messages[0].tokens.cache_read, 105891);
        assert_eq!(messages[0].tokens.cache_write, 28342 - 775); // 27567
        assert_eq!(messages[0].cost, 0.0);

        // Second message: gpt-5-codex
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5-codex");
        assert_eq!(messages[1].provider_id.as_ref(), "openai"); // gpt -> openai
        assert_eq!(messages[1].tokens.input, 8263);
        assert_eq!(messages[1].tokens.cache_read, 66964);
    }

    #[test]
    fn test_parse_cursor_csv_sample_v3_format() {
        // v3 format includes Cloud Agent ID and Automation ID columns
        let csv = r#"Date,Cloud Agent ID,Automation ID,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
"2026-04-09T20:01:10.528Z","bc-a380fb49-e1a5-414e-817d-6a85b6cdc51c","cc30782e-26cc-4359-bc22-7567efe282be","Included","composer-2","Yes","343446","343446","29045760","915201","30304407","Included"
"2026-04-09T18:02:13.576Z","bc-19a9b74b-2af3-46e2-9f61-3ba1cdac46c8","1a0df38f-1474-4dfe-896b-70b841d4a833","On-Demand","composer-2","Yes","43478","43478","420864","7957","472299","0.11"
"2026-04-09T07:39:09.091Z","bc-49262501-0ee0-49f9-b856-a5b0466deddb","","Errored, No Charge","composer-2","Yes","104504","104504","985600","3666","1093770","-""#;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("usage.csv");
        std::fs::write(&file_path, csv).unwrap();

        let messages = parse_cursor_file(&file_path);
        assert_eq!(messages.len(), 3);

        // First message: "Included" cost should be 0
        assert_eq!(messages[0].client.as_ref(), "cursor");
        assert_eq!(messages[0].model_id.as_ref(), "composer-2");
        assert_eq!(messages[0].cost, 0.0);
        assert_eq!(messages[0].tokens.cache_read, 29045760);

        // Second message: app-reported cost is ignored.
        assert_eq!(messages[1].model_id.as_ref(), "composer-2");
        assert_eq!(messages[1].cost, 0.0);

        // Third message: "-" cost should be 0 (Errored, No Charge)
        assert_eq!(messages[2].model_id.as_ref(), "composer-2");
        assert_eq!(messages[2].cost, 0.0);
    }
}
