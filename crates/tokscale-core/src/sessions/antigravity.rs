use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
use crate::source_health::{RecordRejectionReason, ScannedSource, SourceFailure};
use crate::{provider_identity, TokenBreakdown};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub(crate) fn response_dedup_key(response_id: &str) -> u64 {
    crate::sessions::dedup_hash_str(&format!("antigravity:{response_id}"))
}

pub fn parse_antigravity_file(path: &Path) -> SessionParseResult<ScannedSource> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::new("read Antigravity JSONL file", error))?;

    Ok(parse_antigravity_reader(BufReader::new(file), path))
}

fn parse_antigravity_reader<R: BufRead>(mut reader: R, path: &Path) -> ScannedSource {
    let mut scanned = ScannedSource::default();
    let mut session_model: Option<String> = None;
    let mut line = String::with_capacity(4096);
    let mut line_number = 0usize;

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => line_number += 1,
            Err(error) => {
                scanned.interrupted = Some(SourceFailure::new(
                    "read Antigravity JSONL line",
                    format!("{} line {}: {error}", path.display(), line_number + 1),
                ));
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(_error) => {
                session_model = None;
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let row_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        match row_type {
            "session_meta" => {
                match value
                    .get("modelId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(model_id) => session_model = Some(model_id.to_string()),
                    None => {
                        session_model = None;
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MalformedRecord);
                    }
                }
            }
            "usage" => match parse_usage_row(&value, session_model.as_deref()) {
                Ok(Some(message)) => scanned.messages.push(message),
                Ok(None) => {}
                Err(error) => {
                    let reason = antigravity_rejection_reason(&error);
                    scanned.rejections.record(reason);
                }
            },
            _ => {}
        }
    }

    scanned
}

fn antigravity_rejection_reason(error: &SessionParseError) -> RecordRejectionReason {
    let detail = error.to_string();
    if detail.contains("modelId") {
        RecordRejectionReason::MissingModel
    } else if detail.contains("providerId") {
        RecordRejectionReason::MissingProvider
    } else if detail.contains("timestamp") {
        RecordRejectionReason::MissingTimestamp
    } else {
        RecordRejectionReason::MalformedRecord
    }
}

fn parse_usage_row(
    value: &Value,
    fallback_model: Option<&str>,
) -> SessionParseResult<Option<UnifiedMessage>> {
    let tokens = TokenBreakdown {
        input: parse_nonnegative_i64(value.get("input"), "input")?,
        output: parse_nonnegative_i64(value.get("output"), "output")?,
        cache_read: parse_nonnegative_i64(value.get("cacheRead"), "cacheRead")?,
        cache_write: parse_nonnegative_i64(value.get("cacheWrite"), "cacheWrite")?,
        reasoning: parse_nonnegative_i64(value.get("reasoning"), "reasoning")?,
    };
    let token_total = tokens.checked_total().ok_or_else(|| {
        SessionParseError::invalid(
            "validate Antigravity usage row",
            "usage token total exceeds i64::MAX",
        )
    })?;
    if token_total == 0 {
        return Ok(None);
    }

    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                "usage row is missing a non-empty sessionId",
            )
        })?
        .to_string();
    let timestamp = parse_nonnegative_i64(value.get("timestamp"), "timestamp")?;
    if timestamp <= 0 {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            "usage row is missing a positive timestamp",
        ));
    }

    let model_id = value
        .get("modelId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_string())
        .or_else(|| fallback_model.map(|text| text.trim().to_string()))
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                "usage row and session metadata are missing modelId",
            )
        })?;
    let model_id = if let Some(resolved) = resolve_antigravity_placeholder(&model_id) {
        resolved.to_string()
    } else {
        model_id
    };

    let provider_id = value
        .get("providerId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_string())
        .or_else(|| infer_provider(&model_id).map(str::to_string))
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                format!(
                    "usage row is missing providerId and model `{model_id}` has no known provider"
                ),
            )
        })?;

    let dedup_key = value
        .get("responseId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(response_dedup_key);

    Ok(Some(UnifiedMessage::new_with_dedup(
        "antigravity",
        model_id,
        provider_id,
        session_id,
        timestamp,
        tokens,
        0.0,
        dedup_key,
    )))
}

fn infer_provider(model: &str) -> Option<&'static str> {
    provider_identity::inferred_provider_from_model(model)
}

fn resolve_antigravity_placeholder(model_id: &str) -> Option<&'static str> {
    match model_id.to_lowercase().as_str() {
        "model_placeholder_m26" => Some("claude-opus-4.6"),
        "model_placeholder_m35" => Some("claude-sonnet-4.6"),
        "model_placeholder_m36" | "model_placeholder_m37" => Some("gemini-3.1-pro"),
        "model_placeholder_m47" => Some("gemini-3-flash-preview"),
        "model_openai_gpt_oss_120b_medium" => Some("gpt-oss-120b-medium"),
        _ => None,
    }
}

fn parse_nonnegative_i64(value: Option<&Value>, field: &str) -> SessionParseResult<i64> {
    let Some(value) = value else {
        return Ok(0);
    };
    let parsed = if let Some(number) = value.as_i64() {
        number
    } else if let Some(number) = value.as_u64() {
        i64::try_from(number).map_err(|_| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                format!("{field} exceeds i64::MAX"),
            )
        })?
    } else if let Some(text) = value.as_str() {
        text.parse::<i64>().map_err(|error| {
            SessionParseError::new("decode Antigravity usage token field", error)
        })?
    } else {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            format!("{field} must be an integer or decimal integer string"),
        ));
    };
    if parsed < 0 {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            format!("{field} must be non-negative"),
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufRead, Read};

    fn parse_antigravity_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_antigravity_file(path).unwrap().messages
    }

    #[test]
    fn malformed_jsonl_is_reported() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), "{not-json}\n").unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn directory_source_is_reported_as_interrupted_read() {
        let directory = tempfile::TempDir::new().unwrap();

        let scanned = super::parse_antigravity_file(directory.path()).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(
            scanned.interrupted.as_ref().unwrap().operation,
            "read Antigravity JSONL line"
        );
    }

    #[test]
    fn parse_usage_row_with_meta_fallback() {
        let input = r#"{"type":"session_meta","sessionId":"abc","modelId":"claude-sonnet-4.6"}
{"type":"usage","sessionId":"abc","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1,"responseId":"resp-1"}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "antigravity");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].tokens.input, 12);
        assert_eq!(messages[0].tokens.reasoning, 1);
        assert_eq!(messages[0].dedup_key, Some(response_dedup_key("resp-1")));
    }

    #[test]
    fn bad_usage_row_is_rejected_without_hiding_later_usage() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            path.path(),
            concat!(
                r#"{"type":"session_meta","modelId":"gemini-3.1-pro"}"#,
                "\n",
                r#"{"type":"usage","sessionId":"good-1","timestamp":1780000000000,"input":10,"output":2}"#,
                "\n",
                r#"{"type":"usage","sessionId":"bad","timestamp":1780000001000,"input":"not-a-number"}"#,
                "\n",
                r#"{"type":"usage","sessionId":"good-2","timestamp":1780000002000,"input":20,"output":3}"#,
            ),
        )
        .unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_state_line_clears_model_and_later_metadata_resyncs() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            path.path(),
            concat!(
                r#"{"type":"usage","sessionId":"good-1","modelId":"gemini-3.1-pro","timestamp":1780000000000,"input":10}"#,
                "\n",
                r#"{"type":"session_meta","modelId":"broken"#,
                "\n",
                r#"{"type":"usage","sessionId":"old-model-must-not-leak","timestamp":1780000001000,"input":20}"#,
                "\n",
                r#"{"type":"session_meta","modelId":"claude-sonnet-4.6"}"#,
                "\n",
                r#"{"type":"usage","sessionId":"good-2","timestamp":1780000002000,"input":30}"#,
            ),
        )
        .unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[1].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.rejections.total(), 2);
        let keys = scanned
            .rejections
            .entries()
            .map(|entry| entry.key)
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["malformed-record", "missing-model"]);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn invalid_session_metadata_clears_old_model_and_records_rejection() {
        for invalid_model in ["null", "42", r#"\"\""#] {
            let path = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(
                path.path(),
                format!(
                    concat!(
                        "{{\"type\":\"session_meta\",\"modelId\":\"gemini-3.1-pro\"}}\n",
                        "{{\"type\":\"session_meta\",\"modelId\":{}}}\n",
                        "{{\"type\":\"usage\",\"sessionId\":\"must-not-use-old-model\",\"timestamp\":1780000000000,\"input\":10}}\n",
                        "{{\"type\":\"session_meta\",\"modelId\":\"claude-sonnet-4.6\"}}\n",
                        "{{\"type\":\"usage\",\"sessionId\":\"recovered\",\"timestamp\":1780000001000,\"input\":20}}\n"
                    ),
                    invalid_model
                ),
            )
            .unwrap();

            let scanned = super::parse_antigravity_file(path.path()).unwrap();

            assert_eq!(scanned.messages.len(), 1, "modelId={invalid_model}");
            assert_eq!(scanned.messages[0].model_id.as_ref(), "claude-sonnet-4.6");
            assert_eq!(scanned.rejections.total(), 2, "modelId={invalid_model}");
        }
    }

    #[test]
    fn unknown_model_without_explicit_provider_is_rejected() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            path.path(),
            r#"{"type":"usage","sessionId":"unknown","modelId":"model_placeholder_m84","timestamp":1780000000000,"input":10}"#,
        )
        .unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-provider"
        );
    }

    #[test]
    fn zero_usage_is_ignored_before_identity_validation() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), r#"{"type":"usage","input":0,"output":0}"#).unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 0);
    }

    #[test]
    fn overflowing_usage_total_is_rejected_without_panicking() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            path.path(),
            format!(
                concat!(
                    "{{\"type\":\"usage\",\"sessionId\":\"overflow\",\"modelId\":\"gemini-3.1-pro\",\"timestamp\":1780000000000,\"input\":{},\"output\":1}}\n",
                    "{{\"type\":\"usage\",\"sessionId\":\"good\",\"modelId\":\"gemini-3.1-pro\",\"timestamp\":1780000001000,\"input\":10}}\n"
                ),
                i64::MAX
            ),
        )
        .unwrap();

        let scanned = super::parse_antigravity_file(path.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "good");
        assert_eq!(scanned.rejections.total(), 1);
    }

    struct InterruptAfterFirstLine {
        first_line: Option<String>,
    }

    impl Read for InterruptAfterFirstLine {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            unreachable!("parse_antigravity_reader uses BufRead::read_line")
        }
    }

    impl BufRead for InterruptAfterFirstLine {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            unreachable!("read_line is overridden")
        }

        fn consume(&mut self, _amount: usize) {
            unreachable!("read_line is overridden")
        }

        fn read_line(&mut self, output: &mut String) -> io::Result<usize> {
            match self.first_line.take() {
                Some(line) => {
                    let len = line.len();
                    output.push_str(&line);
                    Ok(len)
                }
                None => Err(io::Error::other("injected read interruption")),
            }
        }
    }

    #[test]
    fn read_interruption_keeps_confirmed_prefix_and_marks_partial() {
        let reader = InterruptAfterFirstLine {
            first_line: Some(
                concat!(
                    r#"{"type":"usage","sessionId":"confirmed","modelId":"gemini-3.1-pro","timestamp":1780000000000,"input":10}"#,
                    "\n"
                )
                .to_string(),
            ),
        };

        let scanned = parse_antigravity_reader(reader, Path::new("injected.jsonl"));

        assert_eq!(scanned.messages.len(), 1);
        assert!(scanned.interrupted.is_some());
    }

    #[test]
    fn parse_usage_row_resolves_placeholder_model_alias() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"MODEL_PLACEHOLDER_M26","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn parse_usage_row_preserves_real_model_alias_candidates() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"gemini-3-flash-c","timestamp":1711200000000,"input":12,"output":4}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-c");
    }

    #[test]
    fn parse_usage_row_preserves_unmapped_models_with_explicit_provider() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"model_placeholder_m84","providerId":"antigravity","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1}
{"type":"usage","sessionId":"abc","modelId":"model_placeholder_m16","providerId":"antigravity","timestamp":1711200000001,"input":8,"output":3,"cacheRead":0,"cacheWrite":0,"reasoning":0}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "model_placeholder_m84");
        assert_eq!(messages[0].provider_id.as_ref(), "antigravity");
        assert_eq!(messages[1].model_id.as_ref(), "model_placeholder_m16");
        assert_eq!(messages[1].provider_id.as_ref(), "antigravity");
    }
}
