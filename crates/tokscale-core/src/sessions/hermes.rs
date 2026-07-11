//! Hermes Agent session parser
//!
//! Parses aggregated session rows from Hermes Agent's SQLite state database:
//! - `~/.hermes/state.db`
//! - `$HERMES_HOME/state.db`
//!
//! App-reported estimated and actual costs are ignored. Tokscale reports cost
//! only from token usage and its own pricing table.

use super::error::{SessionParseError, SessionParseResult};
use super::utils::open_readonly_sqlite;
use super::UnifiedMessage;
use crate::{provider_identity, TokenBreakdown};
use std::path::Path;

const HERMES_AGENT_NAME: &str = "Hermes Agent";

fn timestamp_secs_to_ms(timestamp: f64) -> i64 {
    if timestamp > 1e12 {
        timestamp as i64
    } else {
        (timestamp * 1000.0) as i64
    }
}

fn resolved_provider(billing_provider: Option<String>, model_id: &str) -> String {
    billing_provider
        .filter(|provider| !provider.trim().is_empty())
        .and_then(|provider| provider_identity::canonical_provider(provider.trim()))
        .or_else(|| provider_identity::inferred_provider_from_model(model_id).map(str::to_string))
        .unwrap_or_else(|| "hermes".to_string())
}

pub fn parse_hermes_sqlite(db_path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let conn = open_readonly_sqlite(db_path)?;

    let query = r#"
        SELECT
            id,
            model,
            billing_provider,
            started_at,
            message_count,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens
        FROM sessions
        WHERE model IS NOT NULL
          AND TRIM(model) != ''
          AND (
            COALESCE(input_tokens, 0) > 0 OR
            COALESCE(output_tokens, 0) > 0 OR
            COALESCE(cache_read_tokens, 0) > 0 OR
            COALESCE(cache_write_tokens, 0) > 0 OR
            COALESCE(reasoning_tokens, 0) > 0
          )
    "#;

    let mut stmt = conn
        .prepare(query)
        .map_err(|error| SessionParseError::new("prepare Hermes session query", error))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, Option<i32>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                row.get::<_, Option<i64>>(9)?.unwrap_or(0),
            ))
        })
        .map_err(|error| SessionParseError::new("execute Hermes session query", error))?;

    rows.map(|row| {
        let (
            session_id,
            model_id,
            billing_provider,
            started_at,
            message_count,
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        ) = row.map_err(|error| SessionParseError::new("decode Hermes session row", error))?;
        let provider = resolved_provider(billing_provider, &model_id);
        let mut msg = UnifiedMessage::new_with_agent(
            "hermes",
            model_id,
            provider,
            session_id.clone(),
            timestamp_secs_to_ms(started_at),
            TokenBreakdown {
                input: input.max(0),
                output: output.max(0),
                cache_read: cache_read.max(0),
                cache_write: cache_write.max(0),
                reasoning: reasoning.max(0),
            },
            0.0,
            Some(HERMES_AGENT_NAME.to_string()),
        );
        msg.message_count = message_count.max(0);
        msg.dedup_key = Some(crate::sessions::dedup_hash_str(&session_id));
        Ok(msg)
    })
    .collect()
}
