//! Crush session parser.
//!
//! Crush persists session-level spend in `crush.db`, but the schema available
//! to this parser does not expose reliable token buckets. Tokscale usage rows
//! are token-derived, so Crush contributes no messages until a token-level
//! source is available.

use super::UnifiedMessage;
use std::path::Path;

pub fn parse_crush_sqlite(_db_path: &Path) -> Vec<UnifiedMessage> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    #[test]
    fn test_parse_crush_sqlite_returns_empty_for_missing_db() {
        let messages = parse_crush_sqlite(Path::new("/nonexistent/crush.db"));
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_crush_sqlite_returns_empty_for_cost_only_database() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("crush.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                parent_session_id TEXT,
                message_count INTEGER NOT NULL DEFAULT 0,
                cost REAL NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO sessions (
                id, parent_session_id, message_count, cost, updated_at, created_at
            ) VALUES (
                'root-1', NULL, 4, 12.34, 1742342000, 1742300000
            );
            "#,
        )
        .unwrap();

        let messages = parse_crush_sqlite(&db_path);
        assert!(messages.is_empty());
    }
}
