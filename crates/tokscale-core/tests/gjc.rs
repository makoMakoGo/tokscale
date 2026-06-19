//! GJC end-to-end token-pricing integration test.
//!
//! GJC may store embedded `usage.cost.total` values, but Tokscale local report
//! cost is token-derived. Both embedded-cost and no-cost messages must be
//! priced from their token buckets by the PricingService.

use std::collections::HashMap;
use std::io::Write;

use tokscale_core::pricing::{litellm::ModelPricing, PricingService};
use tokscale_core::scanner::ScannerSettings;
use tokscale_core::{parse_local_unified_messages_with_pricing, LocalParseOptions};

/// Build a minimal `PricingService` that knows about one model.
/// input_cost = 0.001 per token, output_cost = 0.002 per token.
/// With 100 input tokens and 50 output tokens:
///   recomputed = 100 * 0.001 + 50 * 0.002 = 0.100 + 0.100 = 0.200
fn make_pricing_service() -> PricingService {
    let mut litellm_data: HashMap<String, ModelPricing> = HashMap::new();
    litellm_data.insert(
        "gjc-priceable-model".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    PricingService::new(litellm_data, HashMap::new())
}

/// The expected token-derived cost:
///   100 * 0.001 + 50 * 0.002 = 0.200
const EXPECTED_TOKEN_PRICED_COST: f64 = 100.0 * 0.001 + 50.0 * 0.002;

/// Embedded app cost is ignored; both messages get token pricing.
///
/// - Message A: `gjc-priceable-model` WITH `usage.cost.total = 0.3`
///   -> reported cost must equal EXPECTED_TOKEN_PRICED_COST
/// - Message B: `gjc-priceable-model` WITHOUT a cost object
///   -> reported cost must also equal EXPECTED_TOKEN_PRICED_COST
#[tokio::test]
async fn test_gjc_embedded_cost_is_ignored_end_to_end() {
    // Build a temporary home directory with the gjc session file.
    let home_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let home_path = home_dir.path();

    // Place the session file at <home>/.gjc/agent/sessions/<slug>/sess.jsonl
    let slug = "test-project-slug";
    let session_dir = home_path
        .join(".gjc")
        .join("agent")
        .join("sessions")
        .join(slug);
    std::fs::create_dir_all(&session_dir).expect("failed to create session dir");

    let session_file = session_dir.join("sess.jsonl");

    // Two JSONL lines:
    // Session header + message A (with embedded cost 0.3) + message B (no cost).
    //
    // Message A: id=msg_A, 100 input, 50 output, cost.total=0.3
    //   → embedded_cost() returns 0.3  → Hermes guard: cost > 0, skip reprice
    //   → final cost == 0.3
    //
    // Message B: id=msg_B, 100 input, 50 output, NO cost object
    //   → embedded_cost() returns 0.0  → Hermes guard: cost == 0, reprice
    //   → final cost == 100*0.001 + 50*0.002 == 0.200
    let jsonl = concat!(
        // Session header
        r#"{"type":"session","id":"gjc_g7_session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work/test-project-slug"}"#,
        "\n",
        // Message A: embedded cost 0.3 — must survive repricing
        r#"{"type":"message","id":"msg_A","parentId":null,"timestamp":"2026-01-01T00:01:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","api":"anthropic","timestamp":1767225661000,"usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150,"cost":{"input":0.1,"output":0.2,"cacheRead":0.0,"cacheWrite":0.0,"total":0.3}}}}"#,
        "\n",
        // Message B: no cost object — must be repriced by PricingService
        r#"{"type":"message","id":"msg_B","parentId":"msg_A","timestamp":"2026-01-01T00:02:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","api":"anthropic","timestamp":1767225721000,"usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150}}}"#,
        "\n",
    );

    {
        let mut f = std::fs::File::create(&session_file).expect("failed to create session file");
        f.write_all(jsonl.as_bytes())
            .expect("failed to write JSONL");
        f.flush().expect("failed to flush");
    }

    let pricing = make_pricing_service();

    // use_env_roots: false ensures we only scan home-derived paths (no env vars).
    let options = LocalParseOptions {
        home_dir: Some(home_path.to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["gjc".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: ScannerSettings::default(),
    };

    let messages = parse_local_unified_messages_with_pricing(options, Some(&pricing))
        .await
        .expect("parse failed");

    assert_eq!(
        messages.len(),
        2,
        "expected exactly 2 messages; got {}: {:#?}",
        messages.len(),
        messages
    );

    // Sort by timestamp for deterministic order (A before B).
    let mut sorted = messages.clone();
    sorted.sort_by_key(|m| m.timestamp);

    let msg_a = &sorted[0]; // timestamp 1767225661000
    let msg_b = &sorted[1]; // timestamp 1767225721000

    // Both messages should be gjc client with the right model.
    assert_eq!(msg_a.client.as_ref(), "gjc");
    assert_eq!(msg_a.model_id.as_ref(), "gjc-priceable-model");
    assert_eq!(msg_b.client.as_ref(), "gjc");
    assert_eq!(msg_b.model_id.as_ref(), "gjc-priceable-model");

    assert!(
        (msg_a.cost - EXPECTED_TOKEN_PRICED_COST).abs() < 1e-10,
        "message A cost should be token-priced {EXPECTED_TOKEN_PRICED_COST} but got {}",
        msg_a.cost
    );

    assert!(
        (msg_b.cost - EXPECTED_TOKEN_PRICED_COST).abs() < 1e-10,
        "message B cost should be token-priced {EXPECTED_TOKEN_PRICED_COST} but got {}",
        msg_b.cost
    );

    assert!(
        msg_b.cost > 0.0,
        "token-priced cost for message B must be > 0, got {}",
        msg_b.cost
    );
}

/// G8: workspace key derives from the session `cwd` header.
///
/// gjc project dirs are dash-encoded cwds (e.g. `--work--pi--`). The parser
/// prefers the session header `cwd` (a real path) for the workspace key, and
/// `normalize_workspace_key` / `workspace_label_from_key` produce a sensible
/// key + label. This also exercises the public normalization helpers directly
/// for the decode + graceful-fallback contract.
#[tokio::test]
async fn test_gjc_workspace_key_from_dashed_slug() {
    use tokscale_core::sessions::{normalize_workspace_key, workspace_label_from_key};

    // The session header carries a real cwd; the on-disk slug is dash-encoded.
    let decoded = "/work/pi";
    let key = normalize_workspace_key(decoded).expect("cwd should normalize");
    assert_eq!(key, "/work/pi");
    assert_eq!(workspace_label_from_key(&key).as_deref(), Some("pi"));

    // Trailing slash + duplicate separators collapse; empty input is None.
    assert_eq!(
        normalize_workspace_key("/work//pi/").as_deref(),
        Some("/work/pi")
    );
    assert_eq!(normalize_workspace_key("   "), None);

    // End-to-end: a session whose cwd header is set drives the message
    // workspace key/label, taking precedence over the on-disk dash-slug dir.
    let home_dir = tempfile::TempDir::new().unwrap();
    let home_path = home_dir.path();
    let session_dir = home_path
        .join(".gjc")
        .join("agent")
        .join("sessions")
        .join("--work--pi--"); // dash-encoded slug on disk
    std::fs::create_dir_all(&session_dir).unwrap();
    let jsonl = concat!(
        r#"{"type":"session","id":"gjc_g8","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work/pi"}"#,
        "\n",
        r#"{"type":"message","id":"m1","timestamp":"2026-01-01T00:01:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","timestamp":1767225661000,"usage":{"input":10,"output":5,"cost":{"total":0.01}}}}"#,
        "\n",
    );
    {
        let mut f = std::fs::File::create(session_dir.join("sess.jsonl")).unwrap();
        f.write_all(jsonl.as_bytes()).unwrap();
        f.flush().unwrap();
    }
    let options = LocalParseOptions {
        home_dir: Some(home_path.to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["gjc".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: ScannerSettings::default(),
    };
    let messages =
        parse_local_unified_messages_with_pricing(options, Some(&make_pricing_service()))
            .await
            .expect("parse failed");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].workspace_key.as_deref(), Some("/work/pi"));
    assert_eq!(messages[0].workspace_label.as_deref(), Some("pi"));
}

/// G9a: recursive glob discovers depth-1 and depth-2 transcripts.
///
/// gjc emits depth-1 session files `<slug>/<id>.jsonl` AND depth-2 per-pass
/// sub-agent children `<slug>/<session>/N-*.jsonl`. Both must be discovered and
/// their (distinct) messages counted.
#[tokio::test]
async fn test_gjc_recursive_glob_depth1_and_depth2() {
    let home_dir = tempfile::TempDir::new().unwrap();
    let home_path = home_dir.path();
    let slug_dir = home_path
        .join(".gjc")
        .join("agent")
        .join("sessions")
        .join("proj");
    std::fs::create_dir_all(&slug_dir).unwrap();

    // depth-1: <slug>/d1.jsonl with one assistant message (distinct id/tokens).
    let d1 = concat!(
        r#"{"type":"session","id":"s_depth1","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work/proj"}"#,
        "\n",
        r#"{"type":"message","id":"d1_m1","timestamp":"2026-01-01T00:01:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","timestamp":1767225661000,"usage":{"input":10,"output":5,"cost":{"total":0.01}}}}"#,
        "\n",
    );
    {
        let mut f = std::fs::File::create(slug_dir.join("d1.jsonl")).unwrap();
        f.write_all(d1.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    // depth-2: <slug>/<session>/0-Pass.jsonl with a DIFFERENT message.
    let depth2_dir = slug_dir.join("s_depth1");
    std::fs::create_dir_all(&depth2_dir).unwrap();
    let d2 = concat!(
        r#"{"type":"session","id":"s_depth2","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work/proj"}"#,
        "\n",
        r#"{"type":"message","id":"d2_m1","timestamp":"2026-01-01T00:03:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","timestamp":1767225841000,"usage":{"input":20,"output":7,"cost":{"total":0.02}}}}"#,
        "\n",
    );
    {
        let mut f = std::fs::File::create(depth2_dir.join("0-Pass.jsonl")).unwrap();
        f.write_all(d2.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    let options = LocalParseOptions {
        home_dir: Some(home_path.to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["gjc".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: ScannerSettings::default(),
    };
    let messages =
        parse_local_unified_messages_with_pricing(options, Some(&make_pricing_service()))
            .await
            .expect("parse failed");
    // Both the depth-1 and the distinct depth-2 message are discovered.
    assert_eq!(
        messages.len(),
        2,
        "expected depth1 + depth2 messages: {messages:#?}"
    );
    let mut ids: Vec<u64> = messages.iter().filter_map(|m| m.dedup_key).collect();
    ids.sort_unstable();
    let mut expected: Vec<u64> = ["s_depth1:d1_m1", "s_depth2:d2_m1"]
        .iter()
        .map(|key| tokscale_core::sessions::dedup_hash_str(key))
        .collect();
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

/// G9b: message-level dedup collapses a replayed parent message id across files.
///
/// Per Architect N6, real depth-2 children are distinct sub-agent sessions;
/// this is a defensive regression guard: if a depth-2 child replays a parent's
/// message id (same session id + message id → same dedup_key), it is counted
/// ONCE via should_keep_deduped_message.
#[tokio::test]
async fn test_gjc_message_dedup_across_replayed_files() {
    let home_dir = tempfile::TempDir::new().unwrap();
    let home_path = home_dir.path();
    let slug_dir = home_path
        .join(".gjc")
        .join("agent")
        .join("sessions")
        .join("proj");
    std::fs::create_dir_all(&slug_dir).unwrap();

    // Same session id "S" and same message id "SHARED" in BOTH files → same
    // dedup_key "S:SHARED" → counted once.
    let shared_msg = r#"{"type":"message","id":"SHARED","timestamp":"2026-01-01T00:01:00.000Z","message":{"role":"assistant","model":"gjc-priceable-model","provider":"anthropic","timestamp":1767225661000,"usage":{"input":10,"output":5,"cost":{"total":0.01}}}}"#;
    let header =
        r#"{"type":"session","id":"S","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work/proj"}"#;
    let content = format!("{header}\n{shared_msg}\n");

    {
        let mut f = std::fs::File::create(slug_dir.join("parent.jsonl")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
    }
    let child_dir = slug_dir.join("S");
    std::fs::create_dir_all(&child_dir).unwrap();
    {
        // child replays the SAME session+message id
        let mut f = std::fs::File::create(child_dir.join("0-replay.jsonl")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    let options = LocalParseOptions {
        home_dir: Some(home_path.to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["gjc".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: ScannerSettings::default(),
    };
    let messages =
        parse_local_unified_messages_with_pricing(options, Some(&make_pricing_service()))
            .await
            .expect("parse failed");
    assert_eq!(
        messages.len(),
        1,
        "replayed message id must be deduped to one: {messages:#?}"
    );
    assert_eq!(
        messages[0].dedup_key,
        Some(tokscale_core::sessions::dedup_hash_str("S:SHARED"))
    );
}
