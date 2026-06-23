//! Antigravity CLI SQLite parser.
//!
//! The CLI stores conversation databases under
//! `~/.gemini/antigravity-cli/conversations/*.db`. Usage rows are protobuf
//! blobs without a checked-in `.proto`; this parser reads only the fields needed
//! for token accounting.
//!
//! The field numbers below were reverse-engineered upstream from real
//! Antigravity CLI conversation databases and ported here as a narrow decoder.
//! They were cross-checked against successful sessions where token buckets move
//! consistently across turns (`output + reasoning` tracks total generated
//! tokens, and `cache_read` appears once a cached prefix exists). This is not an
//! official schema contract; if Antigravity changes the protobuf layout, this
//! parser should be updated from a real database sample or replaced with an
//! official `.proto` decoder.
//!
//! Parsed fields:
//!
//! - `gen_metadata.data #1`: chat model message
//!   - `#4`: usage message
//!     - `#1`: fixed system-prompt input tokens
//!     - `#2`: newly processed input tokens
//!     - `#5`: cache-read tokens
//!     - `#9`: output text tokens
//!     - `#10`: reasoning tokens
//!     - `#11`: response id used for deduplication
//!   - `#19`: response model id, for example `gemini-3-flash-a`
//! - `trajectory_metadata_blob.data #1 #1`: workspace file URI
//! - `trajectory_metadata_blob.data #2`: created timestamp
//!
//! Boundary behavior: rows with all usage buckets equal to zero are ignored, so
//! failed generations with no billable usage do not create usage rows. Failed
//! generations with non-zero usage are still counted, because providers can
//! bill failed requests. Rows without `response_id` still parse, but only the
//! per-file adapter path can distinguish them; cross-file duplicate protection
//! depends on `response_id`.

use super::utils::{file_modified_timestamp_ms, open_readonly_sqlite};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::{pricing, provider_identity, TokenBreakdown};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

pub fn parse_antigravity_cli_file(path: &Path) -> Vec<UnifiedMessage> {
    let Some(conn) = open_readonly_sqlite(path) else {
        return Vec::new();
    };
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();
    let (timestamp, workspace_key, workspace_label) = read_trajectory_meta(&conn, path);

    let mut stmt = match conn.prepare("SELECT data FROM gen_metadata ORDER BY idx") {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([], |row| row.get::<_, Vec<u8>>(0)) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut messages = Vec::new();
    let mut seen_response_ids = HashSet::new();
    for blob in rows.flatten() {
        if let Some(mut message) =
            parse_gen_metadata(&blob, &session_id, timestamp, &mut seen_response_ids)
        {
            message.set_workspace(workspace_key.clone(), workspace_label.clone());
            messages.push(message);
        }
    }
    messages
}

fn parse_gen_metadata(
    blob: &[u8],
    session_id: &str,
    session_timestamp: i64,
    seen_response_ids: &mut HashSet<String>,
) -> Option<UnifiedMessage> {
    let chat_model = message_field(blob, 1)?;
    let fields = chat_model_fields(chat_model);
    let usage = fields.usage?;

    let timestamp = fields
        .generation
        .and_then(|gen| message_field(gen, 4))
        .and_then(proto_timestamp_ms)
        .filter(|ms| *ms > 0)
        .unwrap_or(session_timestamp);

    let to_i64 = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    let input = to_i64(varint_field(usage, 1).unwrap_or(0))
        .saturating_add(to_i64(varint_field(usage, 2).unwrap_or(0)));
    let cache_read = to_i64(varint_field(usage, 5).unwrap_or(0));
    let output = to_i64(varint_field(usage, 9).unwrap_or(0));
    let reasoning = to_i64(varint_field(usage, 10).unwrap_or(0));
    if input == 0 && cache_read == 0 && output == 0 && reasoning == 0 {
        return None;
    }

    let response_id = string_field(usage, 11)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string);
    if let Some(response_id) = &response_id {
        if !seen_response_ids.insert(response_id.clone()) {
            return None;
        }
    }

    let model_raw = fields
        .model_raw
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("unknown");
    let model_id = pricing::aliases::resolve_alias(model_raw)
        .unwrap_or(model_raw)
        .to_string();
    let provider_id = provider_identity::inferred_provider_from_model(&model_id)
        .unwrap_or("antigravity")
        .to_string();
    let dedup_key = response_id.as_deref().map(|response_id| {
        crate::sessions::dedup_hash_str(&format!("antigravity-cli:{response_id}"))
    });

    Some(UnifiedMessage::new_with_dedup(
        "antigravity-cli",
        model_id,
        provider_id,
        session_id,
        timestamp,
        TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write: 0,
            reasoning,
        },
        0.0,
        dedup_key,
    ))
}

#[derive(Default)]
struct ChatModelFields<'a> {
    usage: Option<&'a [u8]>,
    generation: Option<&'a [u8]>,
    model_raw: Option<&'a str>,
}

fn chat_model_fields(chat_model: &[u8]) -> ChatModelFields<'_> {
    let mut fields = ChatModelFields::default();
    let mut reader = ProtoReader::new(chat_model);
    while let Some((field, wire)) = reader.next_field() {
        match (field, wire) {
            (4, Wire::Len(bytes)) => fields.usage = Some(bytes),
            (9, Wire::Len(bytes)) => fields.generation = Some(bytes),
            (19, Wire::Len(bytes)) => {
                if let Ok(model_raw) = std::str::from_utf8(bytes) {
                    fields.model_raw = Some(model_raw);
                }
            }
            _ => {}
        }

        if fields.usage.is_some() && fields.generation.is_some() && fields.model_raw.is_some() {
            break;
        }
    }
    fields
}

fn read_trajectory_meta(conn: &Connection, path: &Path) -> (i64, Option<String>, Option<String>) {
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT data FROM trajectory_metadata_blob LIMIT 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .ok();

    let mut timestamp = None;
    let mut workspace_key = None;
    let mut workspace_label = None;
    if let Some(blob) = &blob {
        timestamp = session_created_ms(blob).filter(|ms| *ms > 0);
        if let Some(uri) = message_field(blob, 1).and_then(|folder| string_field(folder, 1)) {
            if let Some(path_str) = file_uri_to_path(uri) {
                workspace_key = normalize_workspace_key(&path_str);
                workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
            }
        }
    }

    (
        timestamp.unwrap_or_else(|| file_modified_timestamp_ms(path)),
        workspace_key,
        workspace_label,
    )
}

fn session_created_ms(blob: &[u8]) -> Option<i64> {
    proto_timestamp_ms(message_field(blob, 2)?)
}

fn proto_timestamp_ms(timestamp: &[u8]) -> Option<i64> {
    let seconds = i64::try_from(varint_field(timestamp, 1)?).ok()?;
    let nanos = i64::try_from(varint_field(timestamp, 2).unwrap_or(0)).ok()?;
    if !(0..=999_999_999).contains(&nanos) {
        return None;
    }
    seconds.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let decoded = percent_decode(uri.strip_prefix("file://")?);
    let bytes = decoded.as_bytes();
    if bytes.first() == Some(&b'/') {
        if bytes.len() >= 3 && bytes[2] == b':' {
            Some(decoded[1..].to_string())
        } else {
            Some(decoded)
        }
    } else {
        Some(format!("//{decoded}"))
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

enum Wire<'a> {
    Varint(u64),
    Len(&'a [u8]),
    Fixed,
}

struct ProtoReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_varint(&mut self) -> Option<u64> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    fn next_field(&mut self) -> Option<(u64, Wire<'a>)> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let tag = self.read_varint()?;
        let field = tag >> 3;
        let wire = match tag & 0x7 {
            0 => Wire::Varint(self.read_varint()?),
            1 => {
                self.pos = self
                    .pos
                    .checked_add(8)
                    .filter(|pos| *pos <= self.buf.len())?;
                Wire::Fixed
            }
            2 => {
                let len = self.read_varint()? as usize;
                let end = self
                    .pos
                    .checked_add(len)
                    .filter(|pos| *pos <= self.buf.len())?;
                let bytes = &self.buf[self.pos..end];
                self.pos = end;
                Wire::Len(bytes)
            }
            5 => {
                self.pos = self
                    .pos
                    .checked_add(4)
                    .filter(|pos| *pos <= self.buf.len())?;
                Wire::Fixed
            }
            _ => return None,
        };
        Some((field, wire))
    }
}

fn message_field(buf: &[u8], field: u64) -> Option<&[u8]> {
    let mut reader = ProtoReader::new(buf);
    while let Some((found, wire)) = reader.next_field() {
        if found == field {
            if let Wire::Len(bytes) = wire {
                return Some(bytes);
            }
        }
    }
    None
}

fn varint_field(buf: &[u8], field: u64) -> Option<u64> {
    let mut reader = ProtoReader::new(buf);
    while let Some((found, wire)) = reader.next_field() {
        if found == field {
            if let Wire::Varint(value) = wire {
                return Some(value);
            }
        }
    }
    None
}

fn string_field(buf: &[u8], field: u64) -> Option<&str> {
    message_field(buf, field).and_then(|bytes| std::str::from_utf8(bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn enc_varint(field: u64, value: u64) -> Vec<u8> {
        let mut out = encode_varint(field << 3);
        out.extend(encode_varint(value));
        out
    }

    fn enc_len(field: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = encode_varint((field << 3) | 2);
        out.extend(encode_varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
        out
    }

    fn timestamp_message(seconds: u64, nanos: u64) -> Vec<u8> {
        let mut timestamp = Vec::new();
        timestamp.extend(enc_varint(1, seconds));
        timestamp.extend(enc_varint(2, nanos));
        timestamp
    }

    fn gen_metadata_with_timestamp(response_id: &[u8], timestamp: Option<(u64, u64)>) -> Vec<u8> {
        let mut usage = Vec::new();
        usage.extend(enc_varint(1, 1132));
        usage.extend(enc_varint(2, 500));
        usage.extend(enc_varint(5, 16000));
        usage.extend(enc_varint(9, 300));
        usage.extend(enc_varint(10, 40));
        usage.extend(enc_len(11, response_id));

        let mut chat_model = Vec::new();
        chat_model.extend(enc_len(4, &usage));
        if let Some((seconds, nanos)) = timestamp {
            let gen_time = enc_len(4, &timestamp_message(seconds, nanos));
            chat_model.extend(enc_len(9, &gen_time));
        }
        chat_model.extend(enc_len(19, b"gemini-3-flash-a"));
        enc_len(1, &chat_model)
    }

    fn gen_metadata(response_id: &[u8]) -> Vec<u8> {
        gen_metadata_with_timestamp(response_id, None)
    }

    fn trajectory_meta() -> Vec<u8> {
        let workspace = enc_len(1, b"file:///C:/Users/Frank/obsidian-vault");
        let mut created = Vec::new();
        created.extend(enc_varint(1, 1_781_502_653));
        created.extend(enc_varint(2, 0));
        let mut blob = Vec::new();
        blob.extend(enc_len(1, &workspace));
        blob.extend(enc_len(2, &created));
        blob
    }

    #[test]
    fn parses_tokens_model_and_workspace_from_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-test.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE gen_metadata (idx integer, data blob, size integer);
             CREATE TABLE trajectory_metadata_blob (id text, data blob);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (0, ?1, 0)",
            params![gen_metadata(b"resp-1")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
            params![trajectory_meta()],
        )
        .unwrap();
        drop(conn);

        let messages = parse_antigravity_cli_file(&path);

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.client.as_ref(), "antigravity-cli");
        assert_eq!(message.model_id.as_ref(), "gemini-3-flash-preview");
        assert_eq!(message.provider_id.as_ref(), "google");
        assert_eq!(message.tokens.input, 1632);
        assert_eq!(message.tokens.cache_read, 16000);
        assert_eq!(message.tokens.output, 300);
        assert_eq!(message.tokens.reasoning, 40);
        assert_eq!(message.timestamp, 1_781_502_653_000);
        assert_eq!(
            message.workspace_key.as_deref(),
            Some("C:/Users/Frank/obsidian-vault")
        );
        assert_eq!(message.workspace_label.as_deref(), Some("obsidian-vault"));
    }

    #[test]
    fn per_generation_timestamp_overrides_session_fallback() {
        let mut seen = HashSet::new();
        let message = parse_gen_metadata(
            &gen_metadata_with_timestamp(b"with-time", Some((1_781_000_000, 250_000_000))),
            "session",
            111_000,
            &mut seen,
        )
        .unwrap();

        assert_eq!(message.timestamp, 1_781_000_000_250);

        let mut seen_without_time = HashSet::new();
        let fallback_message = parse_gen_metadata(
            &gen_metadata(b"without-time"),
            "session",
            111_000,
            &mut seen_without_time,
        )
        .unwrap();
        assert_eq!(fallback_message.timestamp, 111_000);
    }

    #[test]
    fn invalid_generation_timestamp_falls_back_to_session_time() {
        let mut seen = HashSet::new();
        let message = parse_gen_metadata(
            &gen_metadata_with_timestamp(b"bad-nanos", Some((1_781_000_000, 1_000_000_000))),
            "session",
            222_000,
            &mut seen,
        )
        .unwrap();

        assert_eq!(message.timestamp, 222_000);

        let mut overflow = Vec::new();
        overflow.extend(enc_varint(1, i64::MAX as u64));
        overflow.extend(enc_varint(2, 0));
        assert_eq!(proto_timestamp_ms(&overflow), None);
    }

    #[test]
    fn overlarge_varint_token_counts_are_clamped_not_wrapped() {
        let mut usage = Vec::new();
        usage.extend(enc_varint(1, u64::MAX));
        usage.extend(enc_varint(2, 10));
        usage.extend(enc_varint(9, u64::MAX));
        usage.extend(enc_len(11, b"resp-overflow"));

        let mut chat_model = Vec::new();
        chat_model.extend(enc_len(4, &usage));
        chat_model.extend(enc_len(19, b"gemini-3-flash-a"));
        let blob = enc_len(1, &chat_model);

        let mut seen = HashSet::new();
        let message = parse_gen_metadata(&blob, "session", 1_000, &mut seen).unwrap();
        assert_eq!(message.tokens.input, i64::MAX);
        assert_eq!(message.tokens.output, i64::MAX);
    }

    #[test]
    fn dedupes_repeated_response_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dupes.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE gen_metadata (idx integer, data blob, size integer);")
            .unwrap();
        for idx in 0..2 {
            conn.execute(
                "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, 0)",
                params![idx, gen_metadata(b"resp-1")],
            )
            .unwrap();
        }
        drop(conn);

        let messages = parse_antigravity_cli_file(&path);

        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn file_uri_to_path_handles_windows_posix_and_unc() {
        assert_eq!(
            file_uri_to_path("file:///C:/Users/Frank/obsidian-vault").as_deref(),
            Some("C:/Users/Frank/obsidian-vault")
        );
        assert_eq!(
            file_uri_to_path("file:///home/frank/project").as_deref(),
            Some("/home/frank/project")
        );
        assert_eq!(
            file_uri_to_path("file://server/share/code").as_deref(),
            Some("//server/share/code")
        );
        assert_eq!(
            file_uri_to_path("file:///D:/My%20Project").as_deref(),
            Some("D:/My Project")
        );
    }
}
