use super::{
    aggregate_model_usage_entries, apply_token_pricing, finalize_token_priced_messages,
    generate_graph_with_loaded_pricing, load_aggregated_views_with_pricing,
    load_cache_only_pricing_with_diagnostics, load_usage_data_with_pricing, message_cache,
    normalize_model_for_grouping, parse_all_messages_with_health,
    parse_all_messages_with_health_with_env_strategy, parse_all_messages_with_pricing,
    parse_all_messages_with_pricing_with_env_strategy, positive_token_total, pricing,
    retain_for_requested_clients, scanner, select_local_parse_pricing, AggregatedViews,
    AggregationConfig, ClientContribution, ClientCounts, ClientId, DailyTotals, DateRange,
    GraphResult, GroupBy, LocalParseOptions, ReportOptions, SessionContribution, TimeMetricsReport,
    TokenBreakdown, UnifiedMessage, ViewSet, UNKNOWN_WORKSPACE_LABEL,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug)]
struct LocalMessagesForTest {
    messages: Vec<UnifiedMessage>,
    counts: ClientCounts,
    health: super::DataHealth,
}

fn load_local_messages_for_test(
    options: LocalParseOptions,
) -> Result<LocalMessagesForTest, super::LocalReportError> {
    let counts = super::count_local_client_messages(options.clone())?.counts;
    let prepared = super::prepare_local_sources(options.clone())?;
    let mut messages = Vec::new();
    let health =
        super::fold_prepared_local_sources_with_pricing(prepared, None, &mut messages)?.health;
    let messages = super::filter_unified_messages(messages, &options);
    Ok(LocalMessagesForTest {
        messages,
        counts,
        health,
    })
}

struct HomeEnvGuard(Option<OsString>);

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self(original_home)
    }
}

struct TestEnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl TestEnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }

    #[cfg(unix)]
    fn set_os(key: &'static str, value: &OsString) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn make_workspace_message(
    client: &str,
    model_id: &str,
    provider_id: &str,
    session_id: &str,
    cost: f64,
    workspace_key: Option<&str>,
    workspace_label: Option<&str>,
) -> UnifiedMessage {
    let mut msg = UnifiedMessage::new(
        client,
        model_id,
        provider_id,
        session_id,
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        cost,
    );
    msg.set_workspace(
        workspace_key.map(str::to_string),
        workspace_label.map(str::to_string),
    );
    msg
}

#[test]
fn test_session_contribution_serde_round_trip() {
    let contribution = SessionContribution {
        session_id: "019e1e27-af49-7cd1-89b7-7bad1c3f3be2".into(),
        client: "codex".into(),
        provider: "openai".into(),
        model: "gpt-5".into(),
        totals: DailyTotals {
            tokens: 25_298,
            cost: 0.0123,
            messages: 12,
        },
        token_breakdown: TokenBreakdown {
            input: 12_000,
            output: 8_000,
            cache_read: 5_000,
            cache_write: 258,
            reasoning: 40,
        },
        clients: vec![ClientContribution {
            client: "codex".into(),
            model_id: "gpt-5".into(),
            provider_id: "openai".into(),
            tokens: TokenBreakdown {
                input: 12_000,
                output: 8_000,
                cache_read: 5_000,
                cache_write: 258,
                reasoning: 40,
            },
            cost: 0.0123,
            messages: 12,
        }],
        first_seen: 1_715_551_577,
        last_seen: 1_715_551_612,
    };

    let json = serde_json::to_string(&contribution).expect("serialize session contribution");
    let parsed: SessionContribution =
        serde_json::from_str(&json).expect("deserialize session contribution");

    assert_eq!(parsed, contribution);
    assert!(json.contains("\"session_id\":\"019e1e27"));
}

#[allow(clippy::too_many_arguments)]
fn make_message_with_tokens(
    client: &str,
    model_id: &str,
    provider_id: &str,
    session_id: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
) -> UnifiedMessage {
    UnifiedMessage::new(
        client,
        model_id,
        provider_id,
        session_id,
        1_733_011_200_000,
        TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        },
        0.0,
    )
}

fn aggregate_finalized_model_usage_entries(
    mut messages: Vec<UnifiedMessage>,
    group_by: &GroupBy,
) -> Vec<crate::ModelUsage> {
    for msg in &mut messages {
        let model = crate::model_aliases::canonicalize_model_id(&msg.model_id);
        msg.model_id = crate::sessions::intern::intern(&model);
        msg.refresh_derived_fields();
    }
    aggregate_model_usage_entries(messages, group_by)
}

fn write_streaming_fold_fixture(home: &Path) {
    let db_path = home.join(".local/share/opencode/opencode.db");
    let conn = create_opencode_sqlite_db(&db_path);
    insert_opencode_sqlite_message(
        &conn,
        "msg-1",
        "opencode-session",
        "/repo",
        r#"{"id":"msg-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":1,"cache":{"read":2,"write":3}},"time":{"created":1733011200000}}"#,
    );

    let codex_dir = home.join(".codex/sessions");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("codex-session.jsonl"),
        concat!(
            r#"{"timestamp":"2024-12-01T01:00:00Z","type":"session_meta","payload":{"id":"codex-session","source":"interactive","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2024-12-01T01:00:01Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"timestamp":"2024-12-01T01:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2},"last_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2}}}}"#,
            "\n",
        ),
    )
    .unwrap();
}

fn streaming_report_options(home: &Path, clients: Vec<&str>) -> ReportOptions {
    ReportOptions {
        home_dir: Some(home.to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(clients.into_iter().map(str::to_string).collect()),
        since: None,
        until: None,
        year: None,
        group_by: GroupBy::ClientModel,
        scanner_settings: scanner::ScannerSettings::default(),
    }
}

fn streaming_views(options: &ReportOptions, views: ViewSet) -> AggregatedViews {
    load_aggregated_views_with_pricing(options, views, None).unwrap()
}

fn vec_compat_views(options: &ReportOptions, views: ViewSet) -> AggregatedViews {
    let home_dir = options.home_dir.as_deref().unwrap();
    let clients = options.clients.clone().unwrap();
    let messages = parse_all_messages_with_pricing_with_env_strategy(
        home_dir,
        &clients,
        None,
        options.use_env_roots,
        &options.scanner_settings,
    )
    .unwrap();
    let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
        group_by: options.group_by.clone(),
        date_range: DateRange::from_options(options),
        views,
    });
    for message in &messages {
        engine.push(message);
    }
    engine.finish()
}

fn json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap()
}

fn normalized_graph_value(mut graph: GraphResult) -> serde_json::Value {
    graph.meta.generated_at.clear();
    graph.meta.processing_time_ms = 0;
    json_value(&graph)
}

fn normalized_time_metrics_value(mut report: TimeMetricsReport) -> serde_json::Value {
    report.processing_time_ms = 0;
    json_value(&report)
}

#[test]
fn cache_only_pricing_diagnostics_append_missing_cache_in_order() {
    let mut diagnostics = vec![
        "first diagnostic".to_string(),
        "second diagnostic".to_string(),
    ];

    let loaded = load_cache_only_pricing_with_diagnostics(&mut diagnostics, || None);

    assert!(loaded.is_none());
    assert_eq!(
        diagnostics,
        vec![
            "first diagnostic".to_string(),
            "second diagnostic".to_string(),
            format!(
                "{}: cache-only mode and no cached pricing",
                pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
            ),
        ]
    );
}

#[test]
#[serial_test::serial]
fn test_batched_model_monthly_hourly_views_match_single_view_runs() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let batched = streaming_views(
        &options,
        ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
    );
    let model = streaming_views(&options, ViewSet::MODEL)
        .model_report
        .unwrap();
    let monthly = streaming_views(&options, ViewSet::MONTHLY)
        .monthly_report
        .unwrap();
    let hourly = streaming_views(&options, ViewSet::HOURLY)
        .hourly_report
        .unwrap();

    assert_eq!(
        json_value(&batched.model_report.unwrap()),
        json_value(&model)
    );
    assert_eq!(
        json_value(&batched.monthly_report.unwrap()),
        json_value(&monthly)
    );
    assert_eq!(
        json_value(&batched.hourly_report.unwrap()),
        json_value(&hourly)
    );
}

#[test]
#[serial_test::serial]
fn test_batched_graph_and_time_metrics_views_match_single_view_runs() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let batched = streaming_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);
    let graph = streaming_views(&options, ViewSet::GRAPH).graph.unwrap();
    let time_metrics = streaming_views(&options, ViewSet::TIME_METRICS)
        .time_metrics
        .unwrap();

    assert_eq!(
        normalized_graph_value(batched.graph.unwrap()),
        normalized_graph_value(graph)
    );
    assert_eq!(
        normalized_time_metrics_value(batched.time_metrics.unwrap()),
        normalized_time_metrics_value(time_metrics)
    );
}

#[test]
#[serial_test::serial]
fn test_batched_tui_and_model_views_match_individual_outputs() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let report_options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);
    let local_options = LocalParseOptions {
        home_dir: Some(source_home.path().to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    };

    let batched = streaming_views(&report_options, ViewSet::TUI | ViewSet::MODEL);
    let mut tui = load_usage_data_with_pricing(local_options, GroupBy::ClientModel, None).unwrap();
    let model = streaming_views(&report_options, ViewSet::MODEL)
        .model_report
        .unwrap();
    let batched_health = batched.health.to_report();
    let mut batched_tui = batched.tui_usage.unwrap();

    // `AggregatedViews` carries fold health beside its internal materialized
    // views. The public TUI loader projects that health into `UsageData` at
    // its API boundary, so compare the projection separately from aggregate
    // payload parity.
    assert_eq!(batched_health, tui.health);
    batched_tui.health = Default::default();
    tui.health = Default::default();

    assert_eq!(format!("{batched_tui:?}"), format!("{tui:?}"));
    assert_eq!(
        json_value(&batched.model_report.unwrap()),
        json_value(&model)
    );
}

#[test]
#[serial_test::serial]
fn test_batched_requested_client_filter_matches_single_view_run() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["codex"]);

    let batched = streaming_views(&options, ViewSet::MODEL | ViewSet::MONTHLY);
    let model = streaming_views(&options, ViewSet::MODEL)
        .model_report
        .unwrap();
    let batched_model = batched.model_report.unwrap();

    assert_eq!(json_value(&batched_model), json_value(&model));
    assert_eq!(batched_model.entries.len(), 1);
    assert_eq!(batched_model.entries[0].client, "codex");
}

#[test]
#[serial_test::serial]
fn test_streaming_model_monthly_hourly_reports_match_vec_compat() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let streaming = streaming_views(
        &options,
        ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
    );
    let compat = vec_compat_views(
        &options,
        ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
    );

    assert_eq!(
        json_value(&streaming.model_report.unwrap()),
        json_value(&compat.model_report.unwrap())
    );
    assert_eq!(
        json_value(&streaming.monthly_report.unwrap()),
        json_value(&compat.monthly_report.unwrap())
    );
    assert_eq!(
        json_value(&streaming.hourly_report.unwrap()),
        json_value(&compat.hourly_report.unwrap())
    );
}

#[test]
#[serial_test::serial]
fn test_streaming_graph_and_time_metrics_match_vec_compat() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let streaming = streaming_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);
    let compat = vec_compat_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);

    assert_eq!(
        normalized_graph_value(streaming.graph.unwrap()),
        normalized_graph_value(compat.graph.unwrap())
    );
    assert_eq!(
        normalized_time_metrics_value(streaming.time_metrics.unwrap()),
        normalized_time_metrics_value(compat.time_metrics.unwrap())
    );
}

#[test]
#[serial_test::serial]
fn test_streaming_tui_usage_matches_vec_compat() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = LocalParseOptions {
        home_dir: Some(source_home.path().to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    };
    let report_options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let mut streaming = load_usage_data_with_pricing(options, GroupBy::ClientModel, None).unwrap();
    let mut compat = vec_compat_views(&report_options, ViewSet::TUI)
        .tui_usage
        .unwrap();

    // The vec-compat harness exercises aggregation from a bare message list,
    // which intentionally has no source-health envelope. Health propagation
    // is covered by the local loader; normalize it out for payload parity.
    assert!(streaming.health.complete);
    streaming.health = Default::default();
    compat.health = Default::default();

    assert_eq!(format!("{streaming:?}"), format!("{compat:?}"));
}

#[test]
#[serial_test::serial]
fn test_streaming_tui_usage_applies_date_range() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());

    let included = load_usage_data_with_pricing(
        LocalParseOptions {
            home_dir: Some(source_home.path().to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
            since: Some("2024-12-01".to_string()),
            until: Some("2024-12-01".to_string()),
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        },
        GroupBy::ClientModel,
        None,
    )
    .unwrap();
    let excluded = load_usage_data_with_pricing(
        LocalParseOptions {
            home_dir: Some(source_home.path().to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
            since: Some("2024-12-02".to_string()),
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        },
        GroupBy::ClientModel,
        None,
    )
    .unwrap();

    assert!(included.total_tokens > 0);
    assert!(!included.models.is_empty());
    assert_eq!(excluded.total_tokens, 0);
    assert!(excluded.models.is_empty());
    assert!(excluded.daily.is_empty());
    assert!(excluded.hourly.is_empty());
}

#[test]
#[serial_test::serial]
fn test_streaming_requested_client_filter_matches_vec_compat() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["codex"]);

    let streaming = streaming_views(&options, ViewSet::MODEL);
    let compat = vec_compat_views(&options, ViewSet::MODEL);
    let streaming_report = streaming.model_report.unwrap();

    assert_eq!(
        json_value(&streaming_report),
        json_value(&compat.model_report.unwrap())
    );
    assert_eq!(streaming_report.entries.len(), 1);
    assert_eq!(streaming_report.entries[0].client, "codex");
}

#[test]
#[serial_test::serial]
fn test_streaming_warm_cache_matches_cold_streaming_report() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    write_streaming_fold_fixture(source_home.path());
    let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

    let cold = streaming_views(&options, ViewSet::MODEL)
        .model_report
        .unwrap();
    let warm = streaming_views(&options, ViewSet::MODEL)
        .model_report
        .unwrap();

    assert_eq!(json_value(&cold), json_value(&warm));
}

#[allow(clippy::too_many_arguments)]
fn build_opencode_sqlite_payload(
    created_ms: f64,
    completed_ms: f64,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
) -> String {
    format!(
        r#"{{
            "role": "assistant",
            "modelID": "claude-sonnet-4",
            "providerID": "anthropic",
            "cost": {cost},
            "tokens": {{
                "input": {input},
                "output": {output},
                "reasoning": {reasoning},
                "cache": {{ "read": {cache_read}, "write": {cache_write} }}
            }},
            "time": {{ "created": {created_ms}, "completed": {completed_ms} }},
            "mode": "build"
        }}"#
    )
}

fn create_opencode_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (
            id TEXT PRIMARY KEY,
            directory TEXT NOT NULL
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            data TEXT NOT NULL
        );",
    )
    .unwrap();
    conn
}

fn insert_opencode_sqlite_message(
    conn: &rusqlite::Connection,
    row_id: &str,
    session_id: &str,
    directory: &str,
    data: &str,
) {
    conn.execute(
        "INSERT OR IGNORE INTO session (id, directory) VALUES (?1, ?2)",
        rusqlite::params![session_id, directory],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
        rusqlite::params![row_id, session_id, data],
    )
    .unwrap();
}

fn write_single_opencode_sqlite_fixture(home: &Path) {
    let conn = create_opencode_sqlite_db(&home.join(".local/share/opencode/opencode.db"));
    insert_opencode_sqlite_message(
        &conn,
        "msg-1",
        "session-1",
        "",
        r#"{"id":"msg-1","role":"assistant","modelID":"gpt-5.5","providerID":"openai","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
    );
}

fn encode_proto_varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

fn encode_proto_varint_field(field: u64, value: u64) -> Vec<u8> {
    let mut bytes = encode_proto_varint(field << 3);
    bytes.extend(encode_proto_varint(value));
    bytes
}

fn encode_proto_len_field(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = encode_proto_varint((field << 3) | 2);
    bytes.extend(encode_proto_varint(payload.len() as u64));
    bytes.extend_from_slice(payload);
    bytes
}

fn write_single_antigravity_cli_fixture(home: &Path) {
    let db_path = home.join(".gemini/antigravity-cli/conversations/session.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE gen_metadata (idx integer, data blob, size integer);
         CREATE TABLE trajectory_metadata_blob (id text, data blob);",
    )
    .unwrap();

    let mut usage = Vec::new();
    usage.extend(encode_proto_varint_field(1, 12));
    usage.extend(encode_proto_varint_field(5, 2));
    usage.extend(encode_proto_varint_field(9, 4));
    usage.extend(encode_proto_varint_field(10, 1));
    usage.extend(encode_proto_len_field(11, b"response-1"));
    let mut chat_model = encode_proto_len_field(4, &usage);
    chat_model.extend(encode_proto_len_field(21, b"Gemini 3.5 Flash (Medium)"));
    let generation = encode_proto_len_field(1, &chat_model);

    let created_at = encode_proto_varint_field(1, 1_711_200_000);
    let trajectory = encode_proto_len_field(2, &created_at);

    conn.execute(
        "INSERT INTO gen_metadata (idx, data, size) VALUES (0, ?1, 0)",
        rusqlite::params![generation],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
        rusqlite::params![trajectory],
    )
    .unwrap();
}

fn create_hermes_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            model TEXT,
            started_at REAL NOT NULL,
            message_count INTEGER DEFAULT 0,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0,
            cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0,
            billing_provider TEXT,
            estimated_cost_usd REAL,
            actual_cost_usd REAL
        );",
    )
    .unwrap();
    conn
}

fn create_zed_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY,
            summary TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            created_at TEXT,
            folder_paths TEXT,
            folder_paths_order TEXT,
            data_type TEXT NOT NULL,
            data BLOB NOT NULL
        );",
    )
    .unwrap();
    conn
}

fn insert_zed_thread(conn: &rusqlite::Connection, id: &str, model: &str) {
    let payload = format!(
        r#"{{
            "version": "0.3.0",
            "title": "Test thread",
            "updated_at": "2026-05-01T12:30:00Z",
            "request_token_usage": {{
                "turn-1": {{
                    "input_tokens": 42,
                    "output_tokens": 7,
                    "cache_creation_input_tokens": 3,
                    "cache_read_input_tokens": 5
                }}
            }},
            "model": {{
                "provider": "zed.dev",
                "model": "{model}"
            }},
            "imported": false
        }}"#
    );
    conn.execute(
        "INSERT INTO threads (
            id, summary, updated_at, created_at, folder_paths, folder_paths_order, data_type, data
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            id,
            "Test thread",
            "2026-05-01T12:30:00Z",
            "2026-05-01T12:00:00Z",
            Option::<&str>::None,
            Option::<&str>::None,
            "json",
            payload.as_bytes()
        ],
    )
    .unwrap();
}

fn insert_hermes_session(
    conn: &rusqlite::Connection,
    id: &str,
    model: &str,
    message_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    actual_cost_usd: f64,
) {
    conn.execute(
        "INSERT INTO sessions (
            id, source, model, started_at, message_count,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
            billing_provider, estimated_cost_usd, actual_cost_usd
        ) VALUES (?1, 'cli', ?2, 1775001102.0, ?3, ?4, ?5, 0, 0, 0, 'anthropic', NULL, ?6)",
        rusqlite::params![
            id,
            model,
            message_count,
            input_tokens,
            output_tokens,
            actual_cost_usd
        ],
    )
    .unwrap();
}

fn write_pi_fixture(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#,
    )
    .unwrap();
}

fn write_omp_parent_child_fixture(session_root: &Path) {
    let parent_path = session_root.with_extension("jsonl");
    let child_path = session_root.join("0-ReviewFindings.jsonl");
    std::fs::create_dir_all(child_path.parent().unwrap()).unwrap();
    std::fs::write(
        parent_path,
        r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#,
    )
    .unwrap();
    std::fs::write(
        child_path,
        r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#,
    )
    .unwrap();
}

fn scanner_settings_for_zed_threads_dir(threads_dir: PathBuf) -> scanner::ScannerSettings {
    let mut extra_scan_paths = BTreeMap::new();
    extra_scan_paths.insert("zed".to_string(), vec![threads_dir]);
    scanner::ScannerSettings {
        extra_scan_paths,
        ..Default::default()
    }
}

#[test]
fn test_normalize_model_for_grouping() {
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-5-20251101"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-5-20250929"),
        "claude-sonnet-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-20250514"),
        "claude-sonnet-4"
    );
    assert_eq!(
        normalize_model_for_grouping("qwen3.7-max-2026-05-20"),
        "qwen3.7-max"
    );
    assert_eq!(
        normalize_model_for_grouping("qwen/qwen3.7-max-20260520"),
        "qwen3.7-max"
    );
    assert_eq!(
        normalize_model_for_grouping("qwen3.7-max-2605"),
        "qwen3.7-max"
    );
    assert_eq!(
        normalize_model_for_grouping("qwen3.7-max-05-20"),
        "qwen3.7-max"
    );

    assert_eq!(
        normalize_model_for_grouping("claude-opus-4.5"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4.5"),
        "claude-sonnet-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4.6"),
        "claude-opus-4.6"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-6"),
        "claude-opus-4.6"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-7"),
        "claude-opus-4.7"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-6"),
        "claude-sonnet-4.6"
    );
    assert_eq!(
        normalize_model_for_grouping("anthropic/claude-4-6-sonnet"),
        "claude-sonnet-4.6"
    );
    assert_eq!(
        normalize_model_for_grouping("anthropic/claude-4-5-haiku"),
        "claude-haiku-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("anthropic/claude-4-6-opus"),
        "claude-opus-4.6"
    );

    assert_eq!(normalize_model_for_grouping("gpt-5.2"), "gpt-5.2");
    assert_eq!(normalize_model_for_grouping("gpt-5.4(xhigh)"), "gpt-5.4");
    assert_eq!(normalize_model_for_grouping("gpt-5.4(high)"), "gpt-5.4");
    assert_eq!(normalize_model_for_grouping("gpt-5.4(minimal)"), "gpt-5.4");
    assert_eq!(normalize_model_for_grouping("gpt-5.4(auto)"), "gpt-5.4");
    assert_eq!(normalize_model_for_grouping("gpt-5.4(none)"), "gpt-5.4");
    assert_eq!(
        normalize_model_for_grouping("gpt-5.4(weirdgarbage)"),
        "gpt-5.4(weirdgarbage)"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4.5(high)"),
        "claude-sonnet-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("gemini-3-pro(auto)"),
        "gemini-3-pro(auto)"
    );
    assert_eq!(
        normalize_model_for_grouping("gemini-2.5-pro"),
        "gemini-2.5-pro"
    );
    assert_eq!(
        normalize_model_for_grouping("longcat-flash-3b-all-quant-0203-eagle3"),
        "longcat-flash-3b"
    );
    assert_eq!(
        normalize_model_for_grouping("LongCat-Flash-3B-All-Quant-0203-Eagle3"),
        "longcat-flash-3b"
    );

    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-5-high"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-5-thinking-high"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-5-sub2api-pro"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-opus-4-5-20251101-sub2api-pro"),
        "claude-opus-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-5-20250929-thinking"),
        "claude-sonnet-4.5"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-5-high"),
        "claude-sonnet-4.5"
    );

    assert_eq!(
        normalize_model_for_grouping("claude-4-sonnet"),
        "claude-sonnet-4"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-4-opus-thinking"),
        "claude-opus-4"
    );

    assert_eq!(normalize_model_for_grouping("big-pickle"), "big-pickle");
    assert_eq!(normalize_model_for_grouping("grok-code"), "grok-code");

    assert_eq!(
        normalize_model_for_grouping("claude-opus-4.5-20251101"),
        "claude-opus-4.5"
    );

    assert_eq!(normalize_model_for_grouping("glm-4.7-free"), "glm-4.7");
    assert_eq!(normalize_model_for_grouping("glm-4.7 (free)"), "glm-4.7");
    assert_eq!(normalize_model_for_grouping("glm-4.7:free"), "glm-4.7");
    assert_eq!(normalize_model_for_grouping("glm-4.7-free-high"), "glm-4.7");
    assert_eq!(
        normalize_model_for_grouping("glm-4.7-free-sub2api-pro"),
        "glm-4.7"
    );
    assert_eq!(normalize_model_for_grouping("glm-4.7:free-fast"), "glm-4.7");
    assert_eq!(
        normalize_model_for_grouping("glm-4.7 (free)-medium"),
        "glm-4.7"
    );
    assert_eq!(normalize_model_for_grouping("glm-5.1"), "glm-5.1");
    assert_eq!(
        normalize_model_for_grouping("gemini-2.5-pro-free"),
        "gemini-2.5-pro"
    );
    assert_eq!(
        normalize_model_for_grouping("gemini-2.5-pro-free-xhigh"),
        "gemini-2.5-pro-xhigh"
    );
    assert_eq!(
        normalize_model_for_grouping("claude-sonnet-4-free-thinking"),
        "claude-sonnet-4"
    );
    assert_eq!(
        normalize_model_for_grouping("deepseek-v4 (free)"),
        "deepseek-v4"
    );
    assert_eq!(normalize_model_for_grouping("kimi-k2.5:free"), "kimi-k2.5");
    assert_eq!(
        normalize_model_for_grouping("mimo-v2-pro-20260318"),
        "mimo-v2-pro"
    );
    assert_eq!(
        normalize_model_for_grouping("gpt-4o-mini-2024-07-18"),
        "gpt-4o-mini"
    );
    assert_eq!(
        normalize_model_for_grouping("openai/gpt-4o-mini-2024-07-18"),
        "gpt-4o-mini"
    );
    assert_eq!(
        normalize_model_for_grouping("nemotron-3-ultra-free"),
        "nemotron-3-ultra"
    );
    assert_eq!(
        normalize_model_for_grouping("qwen3.7-max-free"),
        "qwen3.7-max"
    );
    assert_eq!(
        normalize_model_for_grouping("mistral-small-2603"),
        "mistral-small"
    );
    assert_eq!(normalize_model_for_grouping("k2p5"), "kimi-k2.5");
    assert_eq!(normalize_model_for_grouping("k2-p5"), "kimi-k2.5");
    assert_eq!(normalize_model_for_grouping("k2p6"), "kimi-k2.6");
    assert_eq!(normalize_model_for_grouping("k2-p6"), "kimi-k2.6");
    assert_eq!(
        normalize_model_for_grouping("kimi-for-coding/k2p5"),
        "kimi-k2.5"
    );
    assert_eq!(
        normalize_model_for_grouping("kimi-for-coding/k2p6"),
        "kimi-k2.6"
    );

    assert_eq!(
        normalize_model_for_grouping("custom:gpt-5.5-xhigh-sub2api-pro"),
        "gpt-5.5-xhigh-sub2api-pro"
    );
    assert_eq!(normalize_model_for_grouping("gpt-5.5-xhigh"), "gpt-5.5");
    assert_eq!(normalize_model_for_grouping("gpt-5.5-fast"), "gpt-5.5");
    assert_eq!(normalize_model_for_grouping("gpt-5-5-0"), "gpt-5-5-0");
    assert_eq!(normalize_model_for_grouping("gpt-5.4-medium"), "gpt-5.4");
    assert_eq!(
        normalize_model_for_grouping("deepseek/deepseek-v4-pro"),
        "deepseek-v4-pro"
    );
    assert_eq!(
        normalize_model_for_grouping("minimaxai/minimax-m2.5"),
        "minimax-m2.5"
    );
    assert_eq!(
        normalize_model_for_grouping("accounts/fireworks/models/deepseek-v3-0324"),
        "deepseek-v3"
    );
    assert_eq!(
        normalize_model_for_grouping("gpt-5.3-codex"),
        "gpt-5.3-codex"
    );
    assert_eq!(
        normalize_model_for_grouping("gpt-5.1-codex-max"),
        "gpt-5.1-codex-max"
    );
    assert_eq!(
        normalize_model_for_grouping("gpt-5.5-codex-fast"),
        "gpt-5.5-codex"
    );
    assert_eq!(
        normalize_model_for_grouping("gpt-5.1-codex-max-xhigh"),
        "gpt-5.1-codex-max"
    );
}

#[test]
fn test_normalize_model_for_grouping_canonicalizes_gpt_5_6_family_efforts() {
    let cases = [
        ("custom:gpt-5.6-sol-high", "gpt-5.6-sol"),
        ("custom:gpt-5.6-sol-xhigh", "gpt-5.6-sol"),
        ("custom:gpt-5.6-sol-max", "gpt-5.6-sol"),
        ("custom:gpt-5.6-terra-xhigh", "gpt-5.6-terra"),
        ("custom:gpt-5.6-terra-max", "gpt-5.6-terra"),
        ("custom:gpt-5.6-luna-medium", "gpt-5.6-luna"),
        ("custom:gpt-5.6-luna-max", "gpt-5.6-luna"),
    ];

    for (raw, expected) in cases {
        assert_eq!(normalize_model_for_grouping(raw), expected);
    }
}

#[test]
fn test_normalize_model_for_grouping_canonicalizes_gpt_5_6_sol_alias() {
    let cases = [
        "gpt-5.6",
        "gpt-5.6-high",
        "gpt-5.6-max",
        "gpt-5.6(max)",
        "custom:gpt-5.6-max",
        "gpt-5.6-sol-max",
        "gpt-5.6-free",
        "gpt-5.6:free",
        "gpt-5.6 (free)",
        "gpt-5.6-2607",
        "gpt-5.6-high-free",
    ];

    for raw in cases {
        let canonical = normalize_model_for_grouping(raw);
        assert_eq!(canonical, "gpt-5.6-sol", "raw model: {raw}");
        assert_eq!(
            normalize_model_for_grouping(&canonical),
            canonical,
            "raw model: {raw}"
        );
    }
}

#[test]
fn test_group_by_from_str_valid_values() {
    assert_eq!(GroupBy::from_str("model").unwrap(), GroupBy::Model);
    assert_eq!(
        GroupBy::from_str("client,model").unwrap(),
        GroupBy::ClientModel
    );
    assert_eq!(
        GroupBy::from_str("client-model").unwrap(),
        GroupBy::ClientModel
    );
    assert_eq!(
        GroupBy::from_str("client,provider,model").unwrap(),
        GroupBy::ClientProviderModel
    );
    assert_eq!(
        GroupBy::from_str("client-provider-model").unwrap(),
        GroupBy::ClientProviderModel
    );
    assert_eq!(
        GroupBy::from_str("workspace,model").unwrap(),
        GroupBy::WorkspaceModel
    );
    assert_eq!(
        GroupBy::from_str("workspace-model").unwrap(),
        GroupBy::WorkspaceModel
    );
    assert_eq!(GroupBy::from_str("session").unwrap(), GroupBy::Session);
    assert_eq!(
        GroupBy::from_str("session,model").unwrap(),
        GroupBy::Session
    );
    assert_eq!(
        GroupBy::from_str("session-model").unwrap(),
        GroupBy::Session
    );
    assert_eq!(
        GroupBy::from_str("client,session").unwrap(),
        GroupBy::ClientSession
    );
    assert_eq!(
        GroupBy::from_str("client,session,model").unwrap(),
        GroupBy::ClientSession
    );
    assert_eq!(
        GroupBy::from_str("client-session-model").unwrap(),
        GroupBy::ClientSession
    );
    assert!(GroupBy::from_str("unknown").is_err());
}

#[test]
fn test_group_by_default_is_client_model() {
    assert_eq!(GroupBy::default(), GroupBy::ClientModel);
}

#[test]
fn test_group_by_display_round_trips_with_from_str() {
    let variants = [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
        GroupBy::Session,
        GroupBy::ClientSession,
    ];

    for variant in variants {
        let rendered = variant.to_string();
        let parsed = GroupBy::from_str(&rendered).unwrap();
        assert_eq!(parsed, variant);
    }
}

#[test]
fn test_group_by_from_str_whitespace_handling() {
    assert_eq!(
        GroupBy::from_str("client, model").unwrap(),
        GroupBy::ClientModel
    );
    assert_eq!(GroupBy::from_str(" model ").unwrap(), GroupBy::Model);
    assert_eq!(
        GroupBy::from_str("client , provider , model").unwrap(),
        GroupBy::ClientProviderModel
    );
    assert_eq!(
        GroupBy::from_str("workspace, model").unwrap(),
        GroupBy::WorkspaceModel
    );
}

#[test]
fn test_model_usage_performance_uses_only_timed_positive_token_messages() {
    let mut timed = make_workspace_message(
        "opencode",
        "gpt-5.4",
        "openai",
        "session-1",
        0.0,
        None,
        None,
    );
    timed.tokens = TokenBreakdown {
        input: 100,
        output: 50,
        cache_read: 25,
        cache_write: 0,
        reasoning: 25,
    };
    timed.duration_ms = Some(400);

    let mut untimed = make_workspace_message(
        "opencode",
        "gpt-5.4",
        "openai",
        "session-2",
        0.0,
        None,
        None,
    );
    untimed.tokens = TokenBreakdown {
        input: 300,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };

    let entries = aggregate_model_usage_entries(vec![timed, untimed], &GroupBy::ClientModel);

    assert_eq!(entries.len(), 1);
    let performance = &entries[0].performance;
    assert_eq!(performance.total_duration_ms, 400);
    assert_eq!(performance.timed_tokens, 200);
    assert_eq!(performance.sample_count, 1);
    assert_eq!(performance.ms_per_1k_tokens, Some(2000.0));
    assert!((performance.token_coverage - 0.4).abs() < f64::EPSILON);
}

#[test]
fn test_model_usage_performance_is_null_without_duration_samples() {
    let entries = aggregate_model_usage_entries(
        vec![make_workspace_message(
            "claude",
            "claude-sonnet-4-5",
            "anthropic",
            "session-1",
            0.0,
            None,
            None,
        )],
        &GroupBy::ClientModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].performance.ms_per_1k_tokens, None);
    assert_eq!(entries[0].performance.total_duration_ms, 0);
    assert_eq!(entries[0].performance.timed_tokens, 0);
    assert_eq!(entries[0].performance.token_coverage, 0.0);
}

#[test]
fn test_workspace_model_grouping_merges_same_workspace_and_model() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4.5",
                "anthropic",
                "session-1",
                1.25,
                Some("/repo-a"),
                Some("repo-a"),
            ),
            make_workspace_message(
                "qwen",
                "claude-sonnet-4.5",
                "anthropic",
                "session-2",
                2.75,
                Some("/repo-a"),
                Some("repo-a"),
            ),
        ],
        &GroupBy::WorkspaceModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "claude-sonnet-4.5");
    assert_eq!(entries[0].workspace_key.as_deref(), Some("/repo-a"));
    assert_eq!(entries[0].workspace_label.as_deref(), Some("repo-a"));
    assert_eq!(entries[0].cost, 4.0);
    assert_eq!(entries[0].message_count, 2);
    assert_eq!(entries[0].merged_clients.as_deref(), Some("claude, qwen"));
}

#[test]
fn test_model_grouping_cleans_fast_variant() {
    let entries = aggregate_finalized_model_usage_entries(
        vec![
            make_workspace_message(
                "opencode",
                "gpt-5.5-fast",
                "openai",
                "session-1",
                3.0,
                None,
                None,
            ),
            make_workspace_message("codex", "gpt-5.5", "openai", "session-2", 2.0, None, None),
        ],
        &GroupBy::Model,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "gpt-5.5");
    assert_eq!(entries[0].cost, 5.0);
    assert_eq!(entries[0].message_count, 2);
}

#[test]
fn test_model_grouping_cleans_hyphenated_date_snapshot() {
    let entries = aggregate_finalized_model_usage_entries(
        vec![
            make_workspace_message(
                "qwen",
                "qwen3.7-max-2026-05-20",
                "qwen",
                "session-1",
                1.25,
                None,
                None,
            ),
            make_workspace_message("qwen", "qwen3.7-max", "qwen", "session-2", 2.75, None, None),
        ],
        &GroupBy::ClientModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "qwen3.7-max");
    assert_eq!(entries[0].cost, 4.0);
    assert_eq!(entries[0].message_count, 2);
}

#[test]
fn test_model_grouping_cleans_anthropic_prefixed_claude_variant() {
    let entries = aggregate_finalized_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "anthropic/claude-4-6-sonnet",
                "anthropic",
                "session-1",
                1.25,
                Some("/repo-a"),
                Some("repo-a"),
            ),
            make_workspace_message(
                "claude",
                "claude-sonnet-4.6",
                "anthropic",
                "session-2",
                2.75,
                Some("/repo-b"),
                Some("repo-b"),
            ),
        ],
        &GroupBy::ClientModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "claude-sonnet-4.6");
    assert_eq!(entries[0].cost, 4.0);
    assert_eq!(entries[0].message_count, 2);
}

#[test]
fn test_model_grouping_uses_finalized_provider_ids() {
    let entries = aggregate_finalized_model_usage_entries(
        vec![
            make_workspace_message(
                "opencode",
                "xiaomi/mimo-v2.5-pro",
                "xiaomi",
                "session-1",
                1.0,
                None,
                None,
            ),
            make_workspace_message(
                "opencode",
                "xiaomi/mimo-v2.5-pro",
                "xiaomi",
                "session-2",
                2.0,
                None,
                None,
            ),
        ],
        &GroupBy::Model,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "mimo-v2.5-pro");
    assert_eq!(entries[0].provider, "xiaomi");
    assert_eq!(entries[0].cost, 3.0);
    assert_eq!(entries[0].message_count, 2);
}

#[test]
fn test_client_provider_model_grouping_uses_finalized_provider_ids() {
    let entries = aggregate_finalized_model_usage_entries(
        vec![
            make_workspace_message(
                "opencode",
                "xiaomi/mimo-v2.5-pro",
                "xiaomi",
                "session-1",
                1.0,
                None,
                None,
            ),
            make_workspace_message(
                "opencode",
                "xiaomi/mimo-v2.5-pro",
                "xiaomi",
                "session-2",
                2.0,
                None,
                None,
            ),
        ],
        &GroupBy::ClientProviderModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].client, "opencode");
    assert_eq!(entries[0].provider, "xiaomi");
    assert_eq!(entries[0].model, "mimo-v2.5-pro");
    assert_eq!(entries[0].cost, 3.0);
}

#[test]
fn test_model_grouping_orders_merged_clients_by_total_tokens() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_message_with_tokens(
                "opencode",
                "gpt-5.5",
                "openai",
                "session-opencode",
                10,
                0,
                0,
                0,
                0,
            ),
            make_message_with_tokens(
                "codex",
                "gpt-5.5",
                "openai",
                "session-codex",
                30,
                0,
                0,
                0,
                0,
            ),
            make_message_with_tokens("pi", "gpt-5.5", "openai", "session-pi", 100, 0, 0, 0, 0),
        ],
        &GroupBy::Model,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].merged_clients.as_deref(),
        Some("pi, codex, opencode")
    );
    assert_eq!(entries[0].client, "pi, codex, opencode");
}

#[test]
fn test_model_grouping_ignores_negative_client_token_contribution() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_message_with_tokens(
                "negative-client",
                "gpt-5.5",
                "openai",
                "session-negative",
                -1_000,
                0,
                0,
                0,
                0,
            ),
            make_message_with_tokens(
                "positive-client",
                "gpt-5.5",
                "openai",
                "session-positive",
                10,
                0,
                0,
                0,
                0,
            ),
        ],
        &GroupBy::Model,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].merged_clients.as_deref(),
        Some("positive-client, negative-client")
    );
    assert_eq!(entries[0].client, "positive-client, negative-client");
}

#[test]
fn test_workspace_model_grouping_separates_different_workspaces() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-1",
                1.0,
                Some("/repo-a"),
                Some("repo-a"),
            ),
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-2",
                2.0,
                Some("/repo-b"),
                Some("repo-b"),
            ),
        ],
        &GroupBy::WorkspaceModel,
    );

    assert_eq!(entries.len(), 2);
    let labels: HashSet<_> = entries
        .iter()
        .map(|entry| entry.workspace_label.as_deref().unwrap())
        .collect();
    assert_eq!(labels, HashSet::from(["repo-a", "repo-b"]));
}

#[test]
fn test_workspace_model_grouping_uses_unknown_bucket_without_workspace_metadata() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-1",
                1.0,
                None,
                None,
            ),
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-2",
                "2.0".parse().unwrap(),
                None,
                None,
            ),
        ],
        &GroupBy::WorkspaceModel,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].workspace_key, None);
    assert_eq!(
        entries[0].workspace_label.as_deref(),
        Some(UNKNOWN_WORKSPACE_LABEL)
    );
    assert_eq!(entries[0].message_count, 2);
    assert_eq!(entries[0].cost, 3.0);
}

#[test]
fn test_workspace_model_grouping_keeps_real_unknown_workspace_separate() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-1",
                1.0,
                Some("unknown-workspace"),
                Some("unknown-workspace"),
            ),
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-2",
                2.0,
                None,
                None,
            ),
        ],
        &GroupBy::WorkspaceModel,
    );

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| {
        entry.workspace_key.as_deref() == Some("unknown-workspace")
            && entry.workspace_label.as_deref() == Some("unknown-workspace")
            && (entry.cost - 1.0).abs() < f64::EPSILON
    }));
    assert!(entries.iter().any(|entry| {
        entry.workspace_key.is_none()
            && entry.workspace_label.as_deref() == Some(UNKNOWN_WORKSPACE_LABEL)
            && (entry.cost - 2.0).abs() < f64::EPSILON
    }));
}

#[test]
fn test_workspace_model_grouping_avoids_separator_key_collisions() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "c",
                "anthropic",
                "session-1",
                1.0,
                Some("a:b"),
                Some("workspace-ab"),
            ),
            make_workspace_message(
                "claude",
                "b:c",
                "anthropic",
                "session-2",
                2.0,
                Some("a"),
                Some("workspace-a"),
            ),
        ],
        &GroupBy::WorkspaceModel,
    );

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| {
        entry.workspace_key.as_deref() == Some("a:b")
            && entry.model == "c"
            && (entry.cost - 1.0).abs() < f64::EPSILON
    }));
    assert!(entries.iter().any(|entry| {
        entry.workspace_key.as_deref() == Some("a")
            && entry.model == "b:c"
            && (entry.cost - 2.0).abs() < f64::EPSILON
    }));
}

#[test]
fn test_session_grouping_merges_same_session_and_model() {
    // Two messages with the same session_id + same model — should collapse
    // into one row regardless of the client that produced them, because
    // GroupBy::Session keys on (session_id, model) only.
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4.5",
                "anthropic",
                "session-shared",
                1.25,
                None,
                None,
            ),
            make_workspace_message(
                "amp",
                "claude-sonnet-4.5",
                "anthropic",
                "session-shared",
                2.75,
                None,
                None,
            ),
        ],
        &GroupBy::Session,
    );

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("session-shared"));
    assert_eq!(entries[0].model, "claude-sonnet-4.5");
    assert!((entries[0].cost - 4.0).abs() < f64::EPSILON);
    assert_eq!(entries[0].message_count, 2);
    assert!(entries[0].workspace_key.is_none());
    assert!(entries[0].workspace_label.is_none());
    // Session grouping does not merge_clients into a comma list.
    assert!(entries[0].merged_clients.is_none());
}

#[test]
fn test_session_grouping_separates_different_sessions() {
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message("codex", "gpt-5", "openai", "session-a", 1.0, None, None),
            make_workspace_message("codex", "gpt-5", "openai", "session-b", 2.0, None, None),
        ],
        &GroupBy::Session,
    );

    assert_eq!(entries.len(), 2);
    let session_ids: HashSet<_> = entries
        .iter()
        .map(|e| e.session_id.as_deref().unwrap())
        .collect();
    assert_eq!(session_ids, HashSet::from(["session-a", "session-b"]));
}

#[test]
fn test_client_session_grouping_keeps_clients_separate() {
    // Same session_id seen by two different clients (unusual in practice
    // but possible if parsers collide on an id space). ClientSession
    // must yield two rows; Session would yield one (covered above).
    let entries = aggregate_model_usage_entries(
        vec![
            make_workspace_message(
                "claude",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-shared",
                1.0,
                None,
                None,
            ),
            make_workspace_message(
                "amp",
                "claude-sonnet-4-5-20250929",
                "anthropic",
                "session-shared",
                3.0,
                None,
                None,
            ),
        ],
        &GroupBy::ClientSession,
    );

    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.session_id.as_deref(), Some("session-shared"));
        assert!(entry.merged_clients.is_none());
    }
    let by_client: HashSet<_> = entries.iter().map(|e| e.client.as_str()).collect();
    assert_eq!(by_client, HashSet::from(["claude", "amp"]));
}

#[test]
fn test_non_session_grouping_does_not_populate_session_id() {
    // Defensive: only Session/ClientSession variants should set the
    // session_id field on ModelUsage — every other group_by must leave
    // it None so the camelCase JSON output omits it via
    // `skip_serializing_if = "Option::is_none"`.
    for group_by in &[
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        let entries = aggregate_model_usage_entries(
            vec![make_workspace_message(
                "codex",
                "gpt-5",
                "openai",
                "session-x",
                1.0,
                None,
                None,
            )],
            group_by,
        );
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].session_id.is_none(),
            "session_id leaked into {:?} grouping",
            group_by
        );
    }
}

#[test]
fn test_retain_for_requested_clients_keeps_original_client_matches() {
    let requested: HashSet<&str> = HashSet::from(["opencode"]);
    assert!(retain_for_requested_clients(
        "opencode",
        "gpt-4o",
        "anthropic",
        &requested
    ));
    assert!(!retain_for_requested_clients(
        "claude",
        "gpt-4o",
        "anthropic",
        &requested
    ));
}

#[test]
fn test_client_count_sink_attributes_cc_mirror_variants_to_claude() {
    let mut sink = super::ClientCountSink::new(DateRange::none());
    let mut message = UnifiedMessage::new(
        "cc-mirror/zai-worker",
        "claude-sonnet-4",
        "zai",
        "mirror-session",
        1_717_977_600_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.01,
    );
    message.message_count = 3;

    super::adapters::MessageSink::push_message(&mut sink, message);

    assert_eq!(sink.counts.get(ClientId::Claude), 3);
}

#[test]
fn test_retain_for_requested_clients_preserves_kilo_split() {
    let kilocode_only: HashSet<&str> = HashSet::from(["kilocode"]);
    assert!(retain_for_requested_clients(
        "kilocode",
        "gpt-5",
        "openai",
        &kilocode_only
    ));
    assert!(!retain_for_requested_clients(
        "kilo",
        "gpt-5",
        "openai",
        &kilocode_only
    ));

    let kilo_only: HashSet<&str> = HashSet::from(["kilo"]);
    assert!(retain_for_requested_clients(
        "kilo", "gpt-5", "openai", &kilo_only
    ));
    assert!(!retain_for_requested_clients(
        "kilocode", "gpt-5", "openai", &kilo_only
    ));
}

fn write_kimi_code_usage_fixture(source_home: &std::path::Path) {
    let kimi_home = source_home.join(".kimi-code");
    std::fs::create_dir_all(&kimi_home).unwrap();
    std::fs::write(
        kimi_home.join("config.toml"),
        r#"[models."openai-pro/gpt-5.5"]
provider = "openai-pro"
model = "gpt-5.5"
max_context_size = 128000
"#,
    )
    .unwrap();

    let session_dir = kimi_home.join("sessions/wd-project/session_1/agents/main");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("wire.jsonl"),
        r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"context.append_loop_event","time":1770983410000,"event":{"type":"step.end","usage":{"inputOther":10,"output":1,"inputCacheRead":0,"inputCacheCreation":0}}}
{"type":"llm.request","kind":"loop","provider":"openai-responses","model":"gpt-5.5","modelAlias":"openai-pro/gpt-5.5","maxTokens":128000,"time":1770983409000}
{"type":"usage.record","time":1770983410000,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":1,"inputCacheRead":0,"inputCacheCreation":0}}
{"type":"usage.record","time":1770983420000,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":20,"output":2,"inputCacheRead":5,"inputCacheCreation":0}}"#,
    )
    .unwrap();
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_kimi_code_usage_records() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_kimi_code_usage_fixture(source_home.path());

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["kimi".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 30);
        assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 3);
        assert_eq!(messages.iter().map(|m| m.tokens.cache_read).sum::<i64>(), 5);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_kimi_code_usage_records() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_kimi_code_usage_fixture(source_home.path());

        let parsed = load_local_messages_for_test(LocalParseOptions {
            home_dir: Some(source_home.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["kimi".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Kimi), 2);
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].provider_id.as_ref(), "openai");
        assert_eq!(parsed.messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(
            parsed.messages.iter().map(|m| m.tokens.input).sum::<i64>(),
            30
        );
        assert_eq!(
            parsed.messages.iter().map(|m| m.tokens.output).sum::<i64>(),
            3
        );
        assert_eq!(
            parsed
                .messages
                .iter()
                .map(|m| m.tokens.cache_read)
                .sum::<i64>(),
            5
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn kimi_unavailable_optional_config_preserves_current_wire_usage_in_production_pipeline() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());
    write_kimi_code_usage_fixture(source_home.path());

    let options = inventory_options(source_home.path(), &["kimi"]);
    let prepared = super::prepare_local_sources(options.clone()).unwrap();
    let wire_path = source_home
        .path()
        .join(".kimi-code/sessions/wd-project/session_1/agents/main/wire.jsonl");
    let parser_version = prepared.groups[0].units[0].parser_version;
    let mut cold_messages = Vec::new();
    let cold_health =
        super::fold_prepared_local_sources_with_pricing(prepared, None, &mut cold_messages)
            .unwrap()
            .health;
    assert_eq!(cold_messages.len(), 2);
    assert_eq!(cold_health.issue_count(), 0);
    assert!(message_cache::SourceMessageCache::load()
        .unwrap()
        .get_meta(&wire_path, parser_version)
        .unwrap()
        .is_some());

    let mut prepared = super::prepare_local_sources(options).unwrap();
    let regular_config_signature = prepared.source_inventory_signature();
    let config_path = source_home.path().join(".kimi-code/config.toml");
    std::fs::remove_file(&config_path).unwrap();
    std::fs::create_dir(&config_path).unwrap();
    let unavailable_config_signature = prepared.refresh_source_inventory_signature().unwrap();
    assert_ne!(regular_config_signature, unavailable_config_signature);
    assert_eq!(prepared.health.failed_sources(), 0);

    let mut messages = Vec::new();
    let health = super::fold_prepared_local_sources_with_pricing(prepared, None, &mut messages)
        .unwrap()
        .health;
    assert_eq!(messages.len(), 2);
    assert!(messages
        .iter()
        .all(|message| message.model_id.as_ref() == "gpt-5.5"));
    assert!(messages
        .iter()
        .all(|message| message.provider_id.as_ref() == "openai"));
    assert_eq!(
        messages
            .iter()
            .map(|message| message.tokens.input)
            .sum::<i64>(),
        30
    );
    assert_eq!(
        messages
            .iter()
            .map(|message| message.tokens.output)
            .sum::<i64>(),
        3
    );
    assert_eq!(
        messages
            .iter()
            .map(|message| message.tokens.cache_read)
            .sum::<i64>(),
        5
    );
    assert_eq!(health.partial_sources(), 1);
    assert_eq!(health.failed_sources(), 0);
    assert!(message_cache::SourceMessageCache::load()
        .unwrap()
        .get_meta(&wire_path, parser_version)
        .unwrap()
        .is_none());
}

#[test]
#[serial_test::serial]
fn test_source_cache_refreshes_stale_provider_on_cache_hit() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let path = source_home.path().join(".local/share/opencode/opencode.db");
        let conn = create_opencode_sqlite_db(&path);
        insert_opencode_sqlite_message(
            &conn,
            "msg-1",
            "session-1",
            "",
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        );
        drop(conn);

        let unit = crate::adapters::SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        // Provider deliberately wrong for the model: the cache-hit path
        // must re-run refresh_derived_fields (dates are derived from
        // timestamps since schema v24, so provider identity is the
        // remaining derived field).
        let stale_message = UnifiedMessage::new(
            "opencode",
            "gpt-5.5",
            "anthropic",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        let mut cache = message_cache::SourceMessageCache::load().unwrap();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            fingerprint,
            vec![stale_message],
            None,
        ));
        cache.save_if_dirty().unwrap();

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].provider_id.as_ref(),
            "openai",
            "cache hits must refresh derived provider identity"
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

fn inventory_options(home: &Path, clients: &[&str]) -> LocalParseOptions {
    LocalParseOptions {
        home_dir: Some(home.to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(clients.iter().map(|client| (*client).to_string()).collect()),
        scanner_settings: scanner::ScannerSettings::default(),
        ..LocalParseOptions::default()
    }
}

#[test]
#[serial_test::serial]
fn prepare_local_sources_rejects_invalid_extra_dirs_configuration() {
    let home = tempfile::TempDir::new().unwrap();
    let _extra_dirs_guard = TestEnvGuard::set("TOKSCALE_EXTRA_DIRS", "missing-separator");
    let mut options = inventory_options(home.path(), &["amp"]);
    options.use_env_roots = true;

    let error = super::prepare_local_sources(options)
        .err()
        .expect("invalid extra-dir syntax must fail source preparation");

    assert_eq!(
        error.kind(),
        super::LocalReportErrorKind::InvalidEnvironment
    );
    let message = error.to_string();
    assert!(message.contains("TOKSCALE_EXTRA_DIRS"));
    assert!(message.contains("parse environment variable"));
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn prepare_local_sources_rejects_non_utf8_extra_dirs_configuration() {
    use std::os::unix::ffi::OsStringExt;

    let home = tempfile::TempDir::new().unwrap();
    let value = OsString::from_vec(b"amp:/tmp/non-utf8-\xff".to_vec());
    let _extra_dirs_guard = TestEnvGuard::set_os("TOKSCALE_EXTRA_DIRS", &value);
    let mut options = inventory_options(home.path(), &["amp"]);
    options.use_env_roots = true;

    let error = super::prepare_local_sources(options)
        .err()
        .expect("non-UTF-8 extra-dir configuration must fail source preparation");

    assert_eq!(
        error.kind(),
        super::LocalReportErrorKind::InvalidEnvironment
    );
    let message = error.to_string();
    assert!(message.contains("TOKSCALE_EXTRA_DIRS"));
    assert!(message.contains("read environment variable"));
}

#[test]
#[serial_test::serial]
fn prepare_local_sources_isolates_ordinary_discovery_source_failure() {
    let home = tempfile::TempDir::new().unwrap();
    let goose_root = home.path().join("configured-goose-root");
    let invalid_db_candidate = goose_root.join("data/sessions/sessions.db");
    std::fs::create_dir_all(&invalid_db_candidate).unwrap();
    let _goose_root_guard = TestEnvGuard::set("GOOSE_PATH_ROOT", goose_root.to_str().unwrap());
    let mut options = inventory_options(home.path(), &["goose"]);
    options.use_env_roots = true;

    let prepared = super::prepare_local_sources(options)
        .expect("a source discovery failure must remain isolated as health");

    assert_eq!(prepared.health.failed_sources(), 1);
    let failure = &prepared.health.sources()[0];
    assert_eq!(failure.client, ClientId::Goose);
    assert_eq!(failure.path, invalid_db_candidate);
    assert!(matches!(
        failure.status,
        crate::source_health::SourceStatus::Unavailable { .. }
    ));
}

fn signature_for_test_units(
    requested_clients: &[String],
    client: ClientId,
    units: Vec<crate::adapters::SourceUnit>,
) -> super::SourceInventorySignature {
    let group = prepared_test_group(client, units);
    super::source_inventory_signature(requested_clients, &[group])
}

fn prepared_test_group(
    client: ClientId,
    units: Vec<crate::adapters::SourceUnit>,
) -> crate::adapters::PreparedAdapterSources {
    crate::adapters::PreparedAdapterSources {
        adapter: crate::adapters::adapter_for(client).unwrap(),
        units: units
            .into_iter()
            .map(crate::adapters::SourceUnit::prepare_snapshot)
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
    }
}

fn confirmed_test_group(
    client: ClientId,
    units: Vec<crate::adapters::SourceUnit>,
) -> crate::adapters::ConfirmedAdapterSources {
    let prepared = prepared_test_group(client, units);
    let mut present_files = Vec::new();
    let unit_digests = prepared
        .units
        .iter()
        .map(|unit| {
            unit.prepared_source_input_snapshot()
                .expect("test unit must carry a prepared snapshot")
                .visit_present_files(|identity, size| present_files.push((identity, size)));
            unit.inventory_signature_digest()
        })
        .collect();
    crate::adapters::ConfirmedAdapterSources {
        client,
        unit_digests,
        present_files,
    }
}

#[test]
fn source_data_size_counts_related_inputs_once_by_file_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    let source = dir.path().join("source.jsonl");
    let dependency = dir.path().join("dependency.json");
    std::fs::write(&source, b"12345678").unwrap();
    std::fs::write(&dependency, b"12345").unwrap();

    let with_dependency = crate::adapters::SourceUnit::plain_file(ClientId::Amp, source.clone())
        .with_dependency(dependency)
        .prepare_snapshot()
        .unwrap();
    let duplicate = crate::adapters::SourceUnit::plain_file(ClientId::Amp, source)
        .prepare_snapshot()
        .unwrap();

    assert_eq!(super::source_data_bytes([&with_dependency, &duplicate]), 13);
}

#[test]
fn source_data_size_by_client_deduplicates_within_each_client() {
    let dir = tempfile::TempDir::new().unwrap();
    let shared = dir.path().join("shared.jsonl");
    let amp_only = dir.path().join("amp.jsonl");
    std::fs::write(&shared, b"12345678").unwrap();
    std::fs::write(&amp_only, b"12345").unwrap();

    let amp = confirmed_test_group(
        ClientId::Amp,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, shared.clone()),
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, shared.clone()),
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, amp_only),
        ],
    );
    let codebuddy = confirmed_test_group(
        ClientId::CodeBuddy,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::CodeBuddy,
            shared,
        )],
    );
    let requested = vec![
        "amp".to_string(),
        "codebuddy".to_string(),
        "codex".to_string(),
    ];

    let (by_client, global) = super::confirmed_source_data_bytes(&requested, &[amp, codebuddy]);
    assert_eq!(
        by_client,
        std::collections::BTreeMap::from([
            ("amp".to_string(), 13),
            ("codebuddy".to_string(), 8),
            ("codex".to_string(), 0),
        ])
    );
    assert_eq!(global, 13);
}

#[test]
fn inventory_probe_refreshes_source_data_size_from_metadata() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let source = amp_dir.join("T-first.json");
    std::fs::write(&source, b"12345678").unwrap();

    let mut prepared =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    assert_eq!(prepared.health.source_data_bytes(), 8);

    std::fs::write(&source, b"1234567890123").unwrap();
    prepared.refresh_source_inventory_signature().unwrap();
    assert_eq!(prepared.health.source_data_bytes(), 13);
}

#[test]
fn prepared_inventory_is_stable_sensitive_and_reads_no_source_bytes() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let first = amp_dir.join("T-first.json");
    std::fs::write(&first, r#"{"id":"amp-first"}"#).unwrap();
    message_cache::reset_source_read_stats(&first);

    let first_inventory =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    let first_signature = first_inventory.source_inventory_signature();
    let second_signature = super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
        .unwrap()
        .source_inventory_signature();
    assert_eq!(first_signature, second_signature);
    assert_eq!(
        message_cache::get_source_read_stats(&first),
        message_cache::SourceReadStats::default(),
        "inventory signatures must use metadata only"
    );

    std::fs::write(&first, r#"{"id":"amp-first","grew":true}"#).unwrap();
    let changed = super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
        .unwrap()
        .source_inventory_signature();
    assert_ne!(first_signature, changed);

    std::fs::write(amp_dir.join("T-second.json"), r#"{"id":"amp-second"}"#).unwrap();
    let added = super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
        .unwrap()
        .source_inventory_signature();
    assert_ne!(changed, added);

    let other_client = super::prepare_local_sources(inventory_options(home.path(), &["claude"]))
        .unwrap()
        .source_inventory_signature();
    assert_ne!(added, other_client);
}

#[test]
fn inventory_signature_changes_for_same_size_same_mtime_atomic_replacement() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let source = amp_dir.join("T-first.json");
    let replacement = amp_dir.join("replacement.json");
    std::fs::write(&source, b"aaaaaaaa").unwrap();
    let original_mtime = std::fs::metadata(&source).unwrap().modified().unwrap();
    let before = super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
        .unwrap()
        .source_inventory_signature();

    std::fs::write(&replacement, b"bbbbbbbb").unwrap();
    std::fs::File::open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&replacement, &source).unwrap();

    let after = super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
        .unwrap()
        .source_inventory_signature();
    assert_ne!(before, after);
}

#[test]
fn inventory_probe_revalidates_identity_without_rediscovery_or_source_reads() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let source = amp_dir.join("T-first.json");
    let replacement = amp_dir.join("replacement.json");
    std::fs::write(&source, b"aaaaaaaa").unwrap();
    let original_mtime = std::fs::metadata(&source).unwrap().modified().unwrap();

    super::reset_prepare_discovery_count();
    let mut prepared =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    let stale = prepared.source_inventory_signature();
    assert_eq!(super::prepare_discovery_count(), 1);
    message_cache::reset_source_read_stats(&source);

    std::fs::write(&replacement, b"bbbbbbbb").unwrap();
    std::fs::File::open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&replacement, &source).unwrap();

    let refreshed = prepared.refresh_source_inventory_signature().unwrap();
    assert_ne!(stale, refreshed);
    assert_eq!(super::prepare_discovery_count(), 1);
    assert_eq!(
        message_cache::get_source_read_stats(&source),
        message_cache::SourceReadStats::default(),
        "inventory revalidation must not read or hash source bodies"
    );
}

#[test]
#[serial_test::serial]
fn inventory_probe_isolates_a_source_that_disappears_after_prepare() {
    let home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(home.path());
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let retained = amp_dir.join("T-retained.json");
    let removed = amp_dir.join("T-removed.json");
    let source = |session: &str, input: u64| {
        format!(
            r#"{{"id":"{session}","created":1747800000000,"messages":[{{"role":"assistant","messageId":1,"usage":{{"timestamp":"2026-05-21T04:00:00Z","model":"gpt-5","inputTokens":{input},"outputTokens":2}}}}]}}"#
        )
    };
    std::fs::write(&retained, source("retained", 10)).unwrap();
    std::fs::write(&removed, source("removed", 20)).unwrap();

    let mut prepared =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    std::fs::remove_file(&removed).unwrap();

    prepared
        .refresh_source_inventory_signature()
        .expect("a vanished third-party source must not abort the inventory probe");
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::load_prepared_tui_bundle_with_diagnostics(prepared))
        .unwrap();

    assert_eq!(result.accumulator.project(&GroupBy::Model).total_tokens, 12);
    assert_eq!(result.health.failed_sources(), 1);
    assert_eq!(result.health.sources()[0].path, removed);
}

#[test]
#[serial_test::serial]
fn prepared_diagnostics_returns_signature_revalidated_after_pricing_boundary() {
    let home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(home.path());
    let _pricing_guard = TestEnvGuard::set("TOKSCALE_PRICING_CACHE_ONLY", "1");
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let source = amp_dir.join("T-first.json");
    let replacement = amp_dir.join("replacement.json");
    let original = r#"{"id":"session-a","created":1747800000000,"messages":[{"role":"assistant","messageId":1,"usage":{"timestamp":"2026-05-21T04:00:00Z","model":"gpt-5","inputTokens":10,"outputTokens":2}}]}"#;
    let changed = original
        .replace("session-a", "session-b")
        .replace("10", "11");
    assert_eq!(original.len(), changed.len());
    std::fs::write(&source, original).unwrap();
    let original_mtime = std::fs::metadata(&source).unwrap().modified().unwrap();
    let prepared = super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    let stale_signature = prepared.source_inventory_signature();

    std::fs::write(&replacement, changed).unwrap();
    std::fs::File::open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&replacement, &source).unwrap();

    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::load_prepared_tui_bundle_with_diagnostics(prepared))
        .unwrap();
    let confirmed_signature =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
            .unwrap()
            .source_inventory_signature();
    assert_ne!(stale_signature, result.source_inventory_signature);
    assert_eq!(confirmed_signature, result.source_inventory_signature);
    assert_eq!(result.accumulator.project(&GroupBy::Model).total_tokens, 13);
}

#[test]
#[serial_test::serial]
fn prepared_tui_bundle_source_space_uses_confirmed_inventory() {
    let home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(home.path());
    let _pricing_guard = TestEnvGuard::set("TOKSCALE_PRICING_CACHE_ONLY", "1");
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let source = amp_dir.join("T-first.json");
    let replacement = amp_dir.join("replacement.json");
    let original = r#"{"id":"session-a","created":1747800000000,"messages":[{"role":"assistant","messageId":1,"usage":{"timestamp":"2026-05-21T04:00:00Z","model":"gpt-5","inputTokens":10,"outputTokens":2}}]}"#;
    let changed = r#"{"id":"session-confirmed-after-prepare","created":1747800000000,"messages":[{"role":"assistant","messageId":1,"usage":{"timestamp":"2026-05-21T04:00:00Z","model":"gpt-5","inputTokens":111,"outputTokens":2}}]}"#;
    assert!(changed.len() > original.len());
    std::fs::write(&source, original).unwrap();
    let original_mtime = std::fs::metadata(&source).unwrap().modified().unwrap();
    let prepared = super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    let stale_signature = prepared.source_inventory_signature();

    std::fs::write(&replacement, changed).unwrap();
    std::fs::File::open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&replacement, &source).unwrap();
    let confirmed_bytes = std::fs::metadata(&source).unwrap().len();

    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::load_prepared_tui_bundle_with_diagnostics(prepared))
        .unwrap();
    let confirmed_signature =
        super::prepare_local_sources(inventory_options(home.path(), &["amp"]))
            .unwrap()
            .source_inventory_signature();

    assert_ne!(stale_signature, result.source_inventory_signature);
    assert_eq!(confirmed_signature, result.source_inventory_signature);
    assert_eq!(result.source_space.get("amp"), Some(&confirmed_bytes));
    assert_eq!(result.health.source_data_bytes(), confirmed_bytes);
    assert_eq!(
        result.accumulator.project(&GroupBy::Model).total_tokens,
        113
    );
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(
        result.sessions[0].session_id,
        "session-confirmed-after-prepare"
    );
}

#[test]
fn inventory_signature_tracks_order_paths_related_stamps_and_unit_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    let first = dir.path().join("first.db");
    let second = dir.path().join("second.db");
    let wal = dir.path().join("first.db-wal");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    std::fs::write(&wal, b"wal-one").unwrap();
    let clients = vec!["zed".to_string(), "amp".to_string()];

    let ordered = signature_for_test_units(
        &clients,
        ClientId::Amp,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, first.clone()),
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, second.clone()),
        ],
    );
    let reordered = signature_for_test_units(
        &clients,
        ClientId::Amp,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, second),
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, first.clone()),
        ],
    );
    assert_ne!(ordered, reordered, "unit discovery order is significant");

    let canonical_clients = signature_for_test_units(
        &["amp".to_string(), "zed".to_string()],
        ClientId::Amp,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::Amp,
            first.clone(),
        )],
    );
    let reversed_clients = signature_for_test_units(
        &["zed".to_string(), "amp".to_string()],
        ClientId::Amp,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::Amp,
            first.clone(),
        )],
    );
    assert_eq!(canonical_clients, reversed_clients);

    let sqlite_before = signature_for_test_units(
        &["zed".to_string()],
        ClientId::Zed,
        vec![crate::adapters::SourceUnit::sqlite_with_wal(
            ClientId::Zed,
            first.clone(),
        )],
    );
    std::fs::write(&wal, b"wal-two-and-longer").unwrap();
    let sqlite_after = signature_for_test_units(
        &["zed".to_string()],
        ClientId::Zed,
        vec![crate::adapters::SourceUnit::sqlite_with_wal(
            ClientId::Zed,
            first.clone(),
        )],
    );
    assert_ne!(
        sqlite_before, sqlite_after,
        "related WAL stamp is significant"
    );

    let parser_changed = signature_for_test_units(
        &["amp".to_string()],
        ClientId::Amp,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::Amp, first).with_parser_version(
                message_cache::ParserVersion::new(message_cache::ParserId::Amp, 999),
            ),
        ],
    );
    assert_ne!(canonical_clients, parser_changed);

    let codebuddy_path = dir.path().join("codebuddy.jsonl");
    std::fs::write(&codebuddy_path, b"codebuddy").unwrap();
    let jsonl_meta = signature_for_test_units(
        &["codebuddy".to_string()],
        ClientId::CodeBuddy,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::CodeBuddy, codebuddy_path.clone())
                .with_meta(crate::adapters::SourceUnitMeta::CodeBuddyJsonl),
        ],
    );
    let extension_meta = signature_for_test_units(
        &["codebuddy".to_string()],
        ClientId::CodeBuddy,
        vec![
            crate::adapters::SourceUnit::plain_file(ClientId::CodeBuddy, codebuddy_path.clone())
                .with_meta(crate::adapters::SourceUnitMeta::CodeBuddyExtensionLog {
                    source: crate::adapters::CodeBuddyLogSource::Extension,
                }),
        ],
    );
    assert_ne!(jsonl_meta, extension_meta, "unit subtype is significant");

    let plain_policy = signature_for_test_units(
        &["codebuddy".to_string()],
        ClientId::CodeBuddy,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::CodeBuddy,
            codebuddy_path.clone(),
        )],
    );
    let no_cache_policy = signature_for_test_units(
        &["codebuddy".to_string()],
        ClientId::CodeBuddy,
        vec![crate::adapters::SourceUnit::no_message_cache(
            ClientId::CodeBuddy,
            codebuddy_path,
        )],
    );
    assert_ne!(plain_policy, no_cache_policy, "input policy is significant");

    let amp_group_path = dir.path().join("amp-group.json");
    let codebuddy_group_path = dir.path().join("codebuddy-group.json");
    std::fs::write(&amp_group_path, b"amp").unwrap();
    std::fs::write(&codebuddy_group_path, b"codebuddy").unwrap();
    let amp_group = || {
        prepared_test_group(
            ClientId::Amp,
            vec![crate::adapters::SourceUnit::plain_file(
                ClientId::Amp,
                amp_group_path.clone(),
            )],
        )
    };
    let codebuddy_group = || {
        prepared_test_group(
            ClientId::CodeBuddy,
            vec![crate::adapters::SourceUnit::plain_file(
                ClientId::CodeBuddy,
                codebuddy_group_path.clone(),
            )],
        )
    };
    let group_order = super::source_inventory_signature(
        &["amp".to_string(), "codebuddy".to_string()],
        &[amp_group(), codebuddy_group()],
    );
    let reversed_group_order = super::source_inventory_signature(
        &["amp".to_string(), "codebuddy".to_string()],
        &[codebuddy_group(), amp_group()],
    );
    assert_ne!(
        group_order, reversed_group_order,
        "adapter order is significant"
    );
}

#[test]
fn inventory_preparation_rejects_unavailable_primary_snapshots() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing_a = dir.path().join("missing-a.json");
    let error = crate::adapters::SourceUnit::plain_file(ClientId::Amp, missing_a.clone())
        .prepare_snapshot()
        .expect_err("missing primary inputs must fail before inventory hashing");
    assert!(error.to_string().contains(missing_a.to_str().unwrap()));
}

#[cfg(unix)]
#[test]
fn inventory_signature_hashes_native_non_utf8_paths() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::TempDir::new().unwrap();
    let first = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"source-\x80.json".to_vec()));
    let second = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"source-\x81.json".to_vec()));
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());
    std::fs::write(&first, b"same").unwrap();
    std::fs::write(&second, b"same").unwrap();

    let first_signature = signature_for_test_units(
        &["amp".to_string()],
        ClientId::Amp,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::Amp,
            first,
        )],
    );
    let second_signature = signature_for_test_units(
        &["amp".to_string()],
        ClientId::Amp,
        vec![crate::adapters::SourceUnit::plain_file(
            ClientId::Amp,
            second,
        )],
    );
    assert_ne!(first_signature, second_signature);
}

#[test]
fn prepare_discovers_once_and_execute_consumes_the_same_inventory() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    super::reset_prepare_discovery_count();

    let prepared = super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    assert_eq!(super::prepare_discovery_count(), 1);

    std::fs::write(
        amp_dir.join("T-added-after-prepare.json"),
        r#"{
            "id": "added-after-prepare",
            "created": 1747800000000,
            "messages": [{
                "role": "assistant",
                "messageId": 1,
                "usage": {
                    "timestamp": "2026-05-21T04:00:00Z",
                    "model": "claude-opus-4-7",
                    "inputTokens": 10,
                    "outputTokens": 2
                }
            }]
        }"#,
    )
    .unwrap();

    let frozen =
        super::load_prepared_usage_data_with_pricing(prepared, GroupBy::Model, None).unwrap();
    assert_eq!(super::prepare_discovery_count(), 1);
    assert_eq!(frozen.total_tokens, 0);

    let ordinary = super::load_usage_data_with_pricing(
        inventory_options(home.path(), &["amp"]),
        GroupBy::Model,
        None,
    )
    .unwrap();
    assert_eq!(ordinary.total_tokens, 12);
}

#[test]
fn ordinary_and_explicit_prepare_usage_loads_match() {
    let home = tempfile::TempDir::new().unwrap();
    let options = inventory_options(home.path(), &["amp"]);
    let ordinary =
        super::load_usage_data_with_pricing(options.clone(), GroupBy::Model, None).unwrap();
    let prepared = super::prepare_local_sources(options).unwrap();
    let explicit =
        super::load_prepared_usage_data_with_pricing(prepared, GroupBy::Model, None).unwrap();

    assert_eq!(ordinary.total_tokens, explicit.total_tokens);
    assert_eq!(ordinary.total_cost, explicit.total_cost);
    assert_eq!(ordinary.models.len(), explicit.models.len());
}

#[test]
#[serial_test::serial]
fn prepared_aggregation_reclaims_dead_interner_indices_after_materialization() {
    let home = tempfile::TempDir::new().unwrap();
    let amp_dir = home.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    let model = "claude-c5-production-lifecycle-dead-model";
    std::fs::write(
        amp_dir.join("T-c5-lifecycle.json"),
        format!(
            r#"{{
                "id": "c5-lifecycle-session",
                "created": 1747800000000,
                "messages": [{{
                    "role": "assistant",
                    "messageId": 1,
                    "usage": {{
                        "timestamp": "2026-05-21T04:00:00Z",
                        "model": "{model}",
                        "inputTokens": 10,
                        "outputTokens": 2
                    }}
                }}]
            }}"#
        ),
    )
    .unwrap();

    let externally_live = crate::sessions::intern::intern("c5-production-lifecycle-live");
    let prune_before = crate::sessions::intern::prune_count();
    let prepared = super::prepare_local_sources(inventory_options(home.path(), &["amp"])).unwrap();
    let usage =
        super::load_prepared_usage_data_with_pricing(prepared, GroupBy::Model, None).unwrap();

    assert_eq!(usage.models[0].model, model);
    assert_eq!(crate::sessions::intern::prune_count(), prune_before + 1);
    assert_eq!(crate::sessions::intern::indexed_live_count(model), 0);
    assert_eq!(
        crate::sessions::intern::indexed_live_count(&externally_live),
        1
    );
    assert!(Arc::ptr_eq(
        &externally_live,
        &crate::sessions::intern::intern(&externally_live)
    ));
}

#[test]
#[serial_test::serial]
fn test_warm_parse_taking_messages_keeps_outputs_and_cache_stable() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let path = source_home.path().join(".local/share/opencode/opencode.db");
        let conn = create_opencode_sqlite_db(&path);
        insert_opencode_sqlite_message(
            &conn,
            "msg-1",
            "session-1",
            "",
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        );
        drop(conn);
        let unit = crate::adapters::SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);

        let home = source_home.path().to_str().unwrap();
        let clients = ["opencode".to_string()];

        // Cold parse populates the cache; the two warm parses exercise the
        // move-out (take) path and must return identical results without
        // corrupting the saved store (ADR 0008).
        let cold = parse_all_messages_with_pricing(home, &clients, None).unwrap();
        let warm_first = parse_all_messages_with_pricing(home, &clients, None).unwrap();
        let warm_second = parse_all_messages_with_pricing(home, &clients, None).unwrap();

        assert_eq!(cold.len(), 1);
        assert_eq!(cold, warm_first);
        assert_eq!(warm_first, warm_second);

        let mut cache = message_cache::SourceMessageCache::load().unwrap();
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        assert_eq!(
            cache
                .take_messages(&message_cache::CacheReadPlan::new(
                    &path,
                    unit.parser_version,
                    fingerprint.clone(),
                ))
                .expect("saved warm cache shard must remain readable")
                .len(),
            1,
            "warm parses must leave the cached entry intact on disk"
        );
        assert!(matches!(
            cache.take_messages(&message_cache::CacheReadPlan::new(
                std::path::Path::new("/nonexistent/opencode.db"),
                unit.parser_version,
                fingerprint,
            )),
            Err(message_cache::CacheReadFailure {
                reason: message_cache::CacheReadFailureReason::Open { .. },
                ..
            })
        ));
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_opencode_database_open_errors_are_not_cached_as_empty_success() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let path = source_home.path().join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(&path).unwrap();
        let unit = crate::adapters::SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);
        let scanner_settings = scanner::ScannerSettings {
            opencode_db_paths: vec![path.clone()],
            ..scanner::ScannerSettings::default()
        };

        let (first_messages, first_health) = parse_all_messages_with_health_with_env_strategy(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
            false,
            &scanner_settings,
        )
        .unwrap();
        assert!(first_messages.is_empty());
        assert_eq!(first_health.failed_sources(), 1);
        let source = &first_health.sources()[0];
        assert_eq!(source.path, path);
        let failure = source.status.failure().unwrap();
        assert!(
            failure.message.contains("snapshot source metadata")
                || failure
                    .message
                    .contains("read source metadata and file identity")
                || failure
                    .message
                    .contains("open current OpenCode SQLite database")
                || failure.operation.contains("snapshot source metadata")
                || failure
                    .operation
                    .contains("open current OpenCode SQLite database"),
            "failure must identify the failed source operation: {failure:?}"
        );

        let cache = message_cache::SourceMessageCache::load().unwrap();
        assert!(cache
            .get_meta(&path, unit.parser_version)
            .unwrap()
            .is_none());

        std::fs::remove_dir(&path).unwrap();
        let conn = create_opencode_sqlite_db(&path);
        insert_opencode_sqlite_message(
            &conn,
            "msg-1",
            "session-1",
            "",
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        );
        drop(conn);

        let second_messages = parse_all_messages_with_pricing_with_env_strategy(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
            false,
            &scanner_settings,
        )
        .unwrap();
        assert_eq!(second_messages.len(), 1);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_clean_empty_opencode_scan_result_is_not_cached() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let path = source_home.path().join(".local/share/opencode/opencode.db");
        let conn = create_opencode_sqlite_db(&path);
        drop(conn);

        let unit = crate::adapters::SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);

        let first_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert!(first_messages.is_empty());

        let cache = message_cache::SourceMessageCache::load().unwrap();
        assert!(cache
            .get_meta(&path, unit.parser_version)
            .unwrap()
            .is_none());

        message_cache::reset_source_read_stats(&path);
        let second_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert!(second_messages.is_empty());
        assert!(
            message_cache::get_source_read_stats(&path).hash_passes > 0,
            "a clean empty source has no shard and must be scanned again"
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_sqlite_source_cache_invalidates_on_wal_change() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let db_dir = source_home.path().join(".local/share/opencode");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("opencode.db");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
        conn.execute_batch(
            "PRAGMA wal_autocheckpoint=0;
             CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();

        let row_one = r#"{
            "role": "assistant",
            "modelID": "claude-sonnet-4",
            "providerID": "anthropic",
            "tokens": { "input": 100, "output": 50, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
            "time": { "created": 1700000000000.0 }
        }"#;
        let row_two = r#"{
            "role": "assistant",
            "modelID": "claude-sonnet-4",
            "providerID": "anthropic",
            "tokens": { "input": 120, "output": 60, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
            "time": { "created": 1700000001000.0 }
        }"#;

        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["msg-1", "session-1", row_one],
        )
        .unwrap();

        let first_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(first_messages.len(), 1);

        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["msg-2", "session-1", row_two],
        )
        .unwrap();
        assert!(db_path.with_extension("db-wal").exists());

        let refreshed_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(refreshed_messages.len(), 2);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_dedups_across_channel_suffixed_opencode_dbs() {
    // Regression guard: a session that appears in both `opencode.db` and
    // `opencode-<channel>.db` (e.g. the user switches channels mid-session)
    // must only be counted once.
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let db_dir = source_home.path().join(".local/share/opencode");
        std::fs::create_dir_all(&db_dir).unwrap();

        let schema = "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );";
        let row = |input: u64, ts: u64| {
            format!(
                r#"{{
                    "role": "assistant",
                    "modelID": "claude-sonnet-4",
                    "providerID": "anthropic",
                    "tokens": {{ "input": {input}, "output": 10, "reasoning": 0, "cache": {{ "read": 0, "write": 0 }} }},
                    "time": {{ "created": {ts}.0 }}
                }}"#
            )
        };

        let default_db = db_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&default_db).unwrap();
        conn.execute_batch(schema).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "shared-msg",
                "session-shared",
                row(100, 1_700_000_000_000u64)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "latest-only",
                "session-latest",
                row(200, 1_700_000_001_000u64)
            ],
        )
        .unwrap();
        drop(conn);

        let stable_db = db_dir.join("opencode-stable.db");
        let conn = rusqlite::Connection::open(&stable_db).unwrap();
        conn.execute_batch(schema).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "shared-msg",
                "session-shared",
                row(100, 1_700_000_000_000u64)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "stable-only",
                "session-stable",
                row(300, 1_700_000_002_000u64)
            ],
        )
        .unwrap();
        drop(conn);

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(
            messages.len(),
            3,
            "expected 3 unique messages (shared + latest-only + stable-only), got {}",
            messages.len()
        );
        let mut ids: Vec<u64> = messages.iter().filter_map(|m| m.dedup_key).collect();
        ids.sort_unstable();
        let mut expected: Vec<u64> = ["latest-only", "shared-msg", "stable-only"]
            .iter()
            .map(|key| crate::sessions::dedup_hash_str(key))
            .collect();
        expected.sort_unstable();
        assert_eq!(ids, expected);

        let messages_warm = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(
            messages_warm.len(),
            3,
            "warm cache must also dedup shared message across channel dbs"
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_opencode_sqlite_deduplicates_forked_history() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let db_dir = source_home.path().join(".local/share/opencode");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("opencode.db");
        let conn = create_opencode_sqlite_db(&db_path);

        let msg_a = build_opencode_sqlite_payload(
            1_700_000_000_000.0,
            1_700_000_000_500.0,
            100,
            50,
            0,
            10,
            5,
            0.01,
        );
        let msg_b = build_opencode_sqlite_payload(
            1_700_000_001_000.0,
            1_700_000_001_500.0,
            200,
            80,
            10,
            20,
            0,
            0.02,
        );
        let msg_c = build_opencode_sqlite_payload(
            1_700_000_002_000.0,
            1_700_000_002_500.0,
            300,
            120,
            15,
            0,
            0,
            0.03,
        );

        for (id, session_id, payload) in [
            ("root_a", "root", msg_a.as_str()),
            ("root_b", "root", msg_b.as_str()),
            ("fork_a_copy", "fork", msg_a.as_str()),
            ("fork_b_copy", "fork", msg_b.as_str()),
            ("fork_c_new", "fork", msg_c.as_str()),
        ] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, session_id, payload],
            )
            .unwrap();
        }
        drop(conn);

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["opencode".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(messages.len(), 3);
        assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 600);
        assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 250);
        assert_eq!(messages.iter().map(|m| m.cost).sum::<f64>(), 0.0);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_opencode_sqlite_counts_deduplicated_forked_history() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let db_dir = source_home.path().join(".local/share/opencode");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_path = db_dir.join("opencode.db");
        let conn = create_opencode_sqlite_db(&db_path);

        let msg_a = build_opencode_sqlite_payload(
            1_700_000_000_000.0,
            1_700_000_000_500.0,
            100,
            50,
            0,
            10,
            5,
            0.01,
        );
        let msg_b = build_opencode_sqlite_payload(
            1_700_000_001_000.0,
            1_700_000_001_500.0,
            200,
            80,
            10,
            20,
            0,
            0.02,
        );
        let msg_c = build_opencode_sqlite_payload(
            1_700_000_002_000.0,
            1_700_000_002_500.0,
            300,
            120,
            15,
            0,
            0,
            0.03,
        );

        for (id, session_id, payload) in [
            ("root_a", "root", msg_a.as_str()),
            ("root_b", "root", msg_b.as_str()),
            ("fork_a_copy", "fork", msg_a.as_str()),
            ("fork_b_copy", "fork", msg_b.as_str()),
            ("fork_c_new", "fork", msg_c.as_str()),
        ] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, session_id, payload],
            )
            .unwrap();
        }
        drop(conn);

        let parsed = load_local_messages_for_test(LocalParseOptions {
            home_dir: Some(source_home.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::OpenCode), 3);
        assert_eq!(parsed.messages.len(), 3);
        assert_eq!(
            parsed.messages.iter().map(|m| m.tokens.input).sum::<i64>(),
            600
        );
        assert_eq!(
            parsed.messages.iter().map(|m| m.tokens.output).sum::<i64>(),
            250
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

fn write_codex_forked_history_fixture(source_home: &std::path::Path) {
    let codex_dir = source_home.join(".codex/sessions");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("parent.jsonl"),
        concat!(
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"parent-session","source":"interactive","model_provider":"openai","cwd":"/Users/alice/root"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        codex_dir.join("fork.jsonl"),
        concat!(
            r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"fork-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","cwd":"/Users/alice/root-worktree"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:01:02Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:01:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:01:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T10:01:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"total_tokens":143},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":13}}}}"#,
            "\n"
        ),
    )
    .unwrap();
}

fn write_codex_parent_replay_fixture(source_home: &std::path::Path) {
    let codex_dir = source_home.join(".codex/sessions");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("parent.jsonl"),
        concat!(
            r#"{"timestamp":"2026-05-24T20:00:00Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-24T20:00:01Z","type":"turn_context","payload":{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-24T20:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110},"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-24T20:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":13,"total_tokens":143},"last_token_usage":{"input_tokens":30,"output_tokens":3,"total_tokens":33}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    for (filename, child_id, child_turn_id, timestamp) in [
        (
            "child-a.jsonl",
            "019e5c03-1e99-7000-8000-000000000001",
            "019e5c03-6425-7000-8000-000000000001",
            "2026-05-24T21:00:00Z",
        ),
        (
            "child-b.jsonl",
            "019e5c04-1e99-7000-8000-000000000001",
            "019e5c04-6425-7000-8000-000000000001",
            "2026-05-24T22:00:00Z",
        ),
    ] {
        std::fs::write(
            codex_dir.join(filename),
            format!(
                concat!(
                    r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"id":"{child_id}","forked_from_id":"019e5b00-0000-7000-8000-000000000001","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"019e5b00-0000-7000-8000-000000000001","depth":1}}}}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"turn_context","payload":{{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"output_tokens":10,"total_tokens":110}},"last_token_usage":{{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":130,"output_tokens":13,"total_tokens":143}},"last_token_usage":{{"input_tokens":30,"output_tokens":3,"total_tokens":33}}}}}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"task_started","turn_id":"{child_turn_id}"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"turn_context","payload":{{"turn_id":"{child_turn_id}","model":"gpt-5.5","cwd":"/repo"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":140,"output_tokens":14,"total_tokens":154}},"last_token_usage":{{"input_tokens":10,"output_tokens":1,"total_tokens":11}}}}}}}}"#,
                    "\n",
                ),
                timestamp = timestamp,
                child_id = child_id,
                child_turn_id = child_turn_id,
            ),
        )
        .unwrap();
    }
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_codex_deduplicates_forked_history() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_codex_forked_history_fixture(source_home.path());

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.input)
                .sum::<i64>(),
            88
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.cache_read)
                .sum::<i64>(),
            22
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.output)
                .sum::<i64>(),
            33
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_codex_deduplicates_parent_replay_across_forks() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_codex_parent_replay_fixture(source_home.path());

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        // Parent contributes its two turns. The two forks each replay the
        // parent history (skipped) and then emit one own turn that lands on
        // the identical cumulative total (140/14). Sibling forks sharing a
        // cumulative total is the signature of a replayed row, so the
        // fork-parent-scoped dedup key collapses them into one. Real fork
        // fan-out replays the same upstream totals into 10-100+ siblings;
        // two distinct turns reaching a byte-identical cumulative vector by
        // chance does not happen in practice because the cumulative encodes
        // each fork's divergent context size.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 140);
        assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 14);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

fn write_codex_twin_token_count_fixture(source_home: &std::path::Path) {
    // Single session with two turns whose `last_token_usage` deltas are
    // byte-identical but emitted at different timestamps. The fork-dedup
    // key includes the cumulative total, so both turns must survive even
    // when a user happens to send two turns producing the same per-turn
    // delta.
    let codex_dir = source_home.join(".codex/sessions");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("twin-deltas.jsonl"),
        concat!(
            r#"{"timestamp":"2026-04-30T11:00:00Z","type":"session_meta","payload":{"id":"twin-session","source":"interactive","model_provider":"openai","cwd":"/Users/alice/root"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T11:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T11:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-30T11:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n"
        ),
    )
    .unwrap();
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_codex_keeps_twin_token_counts_at_distinct_timestamps() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_codex_twin_token_count_fixture(source_home.path());

        let messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(
            messages.len(),
            2,
            "two turns with identical token deltas at distinct timestamps must both survive dedup",
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.input)
                .sum::<i64>(),
            16,
            "input tokens normalize cache_read out of input: 2 turns × (10 - 2) = 16",
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.output)
                .sum::<i64>(),
            6,
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.tokens.cache_read)
                .sum::<i64>(),
            4,
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_codex_counts_deduplicated_forked_history() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        write_codex_forked_history_fixture(source_home.path());

        let parsed = load_local_messages_for_test(LocalParseOptions {
            home_dir: Some(source_home.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["codex".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Codex), 3);
        assert_eq!(parsed.messages.len(), 3);
        assert_eq!(
            parsed
                .messages
                .iter()
                .map(|message| message.tokens.input)
                .sum::<i64>(),
            88
        );
        assert_eq!(
            parsed
                .messages
                .iter()
                .map(|message| message.tokens.cache_read)
                .sum::<i64>(),
            22
        );
        assert_eq!(
            parsed
                .messages
                .iter()
                .map(|message| message.tokens.output)
                .sum::<i64>(),
            33
        );
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_cache_reparses_from_zero_when_incremental_prefix_is_stale() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let fresh_cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let codex_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let path = codex_dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let initial_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(initial_messages.len(), 1);
        assert_eq!(initial_messages[0].model_id.as_ref(), "gpt-5.4");
        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .and_then(|meta| meta.codex_incremental)
            .is_some());

        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let warm_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        std::env::set_var("HOME", fresh_cache_home.path());
        let fresh_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(warm_messages, fresh_messages);
        assert_eq!(warm_messages.len(), 2);
        assert!(warm_messages
            .iter()
            .all(|message| message.model_id.as_ref() == "gpt-5.5"));
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_untimestamped_token_row_is_partial_without_cache_shard() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let codex_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let path = codex_dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let (messages, health) = parse_all_messages_with_health(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert!(messages.is_empty());
        assert_eq!(health.partial_sources(), 1);
        assert_eq!(health.failed_sources(), 0);
        assert_eq!(health.rejected_records(), 1);
        let source = &health.sources()[0];
        assert_eq!(source.path, path);
        assert!(matches!(
            source.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert_eq!(
            source.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
        let failure = source.status.failure().unwrap();
        assert_eq!(failure.operation, "validate Codex token-count event");
        assert!(
            failure.message.contains("timestamp is missing"),
            "{failure:?}"
        );

        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_none());
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_malformed_json_suffix_keeps_prefix_without_cache_shard() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let codex_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let path = codex_dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999""#,
                "\n"
            ),
        )
        .unwrap();

        let (messages, health) = parse_all_messages_with_health(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(health.partial_sources(), 1);
        assert_eq!(health.failed_sources(), 0);
        assert_eq!(health.rejected_records(), 1);
        let source = &health.sources()[0];
        assert_eq!(source.path, path);
        assert!(matches!(
            source.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert_eq!(
            source.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        let failure = source.status.failure().unwrap();
        assert_eq!(failure.operation, "decode Codex JSONL entry");
        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_none());
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_invalid_utf8_suffix_keeps_prefix_without_cache_shard() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let session_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("session.jsonl");

        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
        file.write_all(&[0xff, b'\n']).unwrap();
        file.flush().unwrap();

        let (messages, health) = parse_all_messages_with_health(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(health.partial_sources(), 1);
        assert_eq!(health.failed_sources(), 0);
        assert_eq!(health.rejected_records(), 0);
        let source = &health.sources()[0];
        assert_eq!(source.path, path);
        assert!(matches!(
            source.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert!(source.rejections.is_empty());
        let failure = source.status.failure().unwrap();
        assert_eq!(failure.operation, "read Codex JSONL line");

        let cache = message_cache::SourceMessageCache::load().unwrap();
        assert!(cache
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_none());
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_unknown_model_prefix_is_partial_then_parses_when_completed() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let fresh_cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let session_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let (initial_messages, initial_health) = parse_all_messages_with_health(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert!(initial_messages.is_empty());
        assert_eq!(initial_health.partial_sources(), 1);
        assert_eq!(initial_health.failed_sources(), 0);
        assert_eq!(initial_health.rejected_records(), 1);
        let source = &initial_health.sources()[0];
        assert_eq!(source.path, path);
        assert!(matches!(
            source.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert_eq!(
            source.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        let failure = source.status.failure().unwrap();
        assert_eq!(failure.operation, "resolve Codex token-count model");
        assert!(
            failure.message.contains("model was never identified"),
            "{failure:?}"
        );
        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_none());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(
            concat!(
                r#"{"timestamp":"2026-04-27T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        let resumed_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        std::env::set_var("HOME", fresh_cache_home.path());
        let fresh_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(resumed_messages, fresh_messages);
        assert_eq!(resumed_messages.len(), 1);
        assert_eq!(resumed_messages[0].model_id.as_ref(), "gpt-5.5");

        std::env::set_var("HOME", cache_home.path());
        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_some());
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_codex_cache_skips_non_newline_terminated_resume_prefix() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let fresh_cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", cache_home.path());

    {
        let session_dir = source_home.path().join(".codex/sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#
            ),
        )
        .unwrap();

        let initial_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(initial_messages.len(), 1);
        assert!(message_cache::SourceMessageCache::load()
            .unwrap()
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION
                )
            )
            .unwrap()
            .is_none());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(
            concat!(
                "\n",
                r#"{"timestamp":"2026-04-27T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        let warm_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        std::env::set_var("HOME", fresh_cache_home.path());
        let fresh_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["codex".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(warm_messages, fresh_messages);
        assert_eq!(warm_messages.len(), 2);
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[serial_test::serial]
fn test_source_cache_does_not_reuse_priced_cost_without_pricing_service() {
    let temp_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", temp_home.path());
    {
        write_kimi_code_usage_fixture(source_home.path());

        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-5.5".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                cache_read_input_token_cost: Some(0.0005),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let repriced_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["kimi".to_string()],
            Some(&pricing),
        )
        .unwrap();
        assert_eq!(repriced_messages.len(), 2);
        assert!(repriced_messages.iter().all(|message| message.cost > 0.0));

        let cached_messages = parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["kimi".to_string()],
            None,
        )
        .unwrap();

        assert_eq!(cached_messages.len(), 2);
        assert!(cached_messages.iter().all(|message| message.cost == 0.0));
    }

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn test_apply_token_pricing_clears_existing_cost_without_pricing() {
    let mut msg = UnifiedMessage::new_with_agent(
        "roocode",
        "gpt-4o",
        "provider",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.42,
        Some("planner".to_string()),
    );

    apply_token_pricing(&mut msg, None);

    assert_eq!(msg.cost, 0.0);
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_prices_canonical_gpt_5_6_factory_model() {
    let cache_home = tempfile::TempDir::new().unwrap();
    let source_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());
    let session_dir = source_home.path().join(".factory/sessions/workspace");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("factory-session.settings.json"),
        r#"{
            "model": "custom:gpt-5.6-max",
            "reasoningEffort": "max",
            "providerLock": "openai",
            "providerLockTimestamp": "2026-07-11T13:38:03.820Z",
            "tokenUsage": {
                "inputTokens": 10,
                "outputTokens": 5,
                "thinkingTokens": 2
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        session_dir.join("factory-sol.settings.json"),
        r#"{
            "model": "custom:gpt-5.6-sol-xhigh",
            "reasoningEffort": "xhigh",
            "providerLock": "openai",
            "providerLockTimestamp": "2026-07-11T13:39:03.820Z",
            "tokenUsage": {
                "inputTokens": 10,
                "outputTokens": 5,
                "thinkingTokens": 2
            }
        }"#,
    )
    .unwrap();

    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5.6-sol".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());
    let cold_messages = parse_all_messages_with_pricing(
        source_home.path().to_str().unwrap(),
        &["droid".to_string()],
        Some(&pricing),
    )
    .unwrap();
    let warm_messages = parse_all_messages_with_pricing(
        source_home.path().to_str().unwrap(),
        &["droid".to_string()],
        Some(&pricing),
    )
    .unwrap();

    assert_eq!(cold_messages, warm_messages);
    assert_eq!(warm_messages.len(), 2);
    assert!(warm_messages
        .iter()
        .all(|message| message.model_id.as_ref() == "gpt-5.6-sol"));
    assert!(warm_messages
        .iter()
        .all(|message| message.provider_id.as_ref() == "openai"));
    assert!(warm_messages
        .iter()
        .all(|message| message.tokens.reasoning == 2));
    assert!(warm_messages.iter().all(|message| message.cost == 0.024));
}

#[test]
fn test_finalize_token_priced_messages_drops_rows_without_positive_tokens() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4o".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut messages = vec![
        UnifiedMessage::new(
            "gemini",
            "gpt-4o",
            "openai",
            "zero",
            1_733_011_200_000,
            TokenBreakdown::default(),
            0.42,
        ),
        UnifiedMessage::new(
            "gemini",
            "gpt-4o",
            "openai",
            "negative",
            1_733_011_200_000,
            TokenBreakdown {
                input: -10,
                output: -5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.42,
        ),
        UnifiedMessage::new(
            "gemini",
            "gpt-4o",
            "openai",
            "mixed",
            1_733_011_200_000,
            TokenBreakdown {
                input: -10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.42,
        ),
    ];

    finalize_token_priced_messages(&mut messages, Some(&pricing));

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].session_id.as_ref(), "mixed");
    assert_eq!(messages[0].tokens.input, 0);
    assert_eq!(messages[0].tokens.output, 5);
    assert_eq!(messages[0].cost, 0.01);
}

#[test]
fn test_finalize_token_priced_messages_canonicalizes_provider() {
    let mut messages = vec![
        UnifiedMessage::new(
            "pi",
            "gpt-5.5",
            "",
            "missing-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "mux",
            "some-model",
            "fireworks",
            "canonical-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "opencode",
            "grok-code-fast-1",
            "xai-oauth",
            "xai-oauth-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "opencode",
            "grok-code-fast-1",
            "grok-oauth",
            "grok-oauth-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "claude",
            "kimi-for-coding",
            "moonshotai",
            "moonshot-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "copilot",
            "claude-sonnet-4.5",
            "github-copilot",
            "copilot-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "codex",
            "gpt-5.2",
            "azure",
            "azure-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "google-antigravity",
            "gemini-2.5-pro",
            "vertex",
            "vertex-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "opencode",
            "glm-5.1",
            "open.bigmodel.cn",
            "bigmodel-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "claude",
            "hy3-preview-agent",
            "",
            "hy3-missing-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "file",
            "Jamba-1.5-Large",
            "unknown",
            "jamba-unknown-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "file",
            "perplexity/llama-3",
            "",
            "route-prefix-must-not-drive-provider",
            1_733_011_200_000,
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
    ];

    finalize_token_priced_messages(&mut messages, None);

    assert_eq!(messages[0].provider_id.as_ref(), "openai");
    assert_eq!(messages[1].provider_id.as_ref(), "fireworks");
    assert_eq!(messages[2].provider_id.as_ref(), "xai");
    assert_eq!(messages[3].provider_id.as_ref(), "xai");
    assert_eq!(messages[4].provider_id.as_ref(), "kimi");
    assert_eq!(messages[5].provider_id.as_ref(), "microsoft");
    assert_eq!(messages[6].provider_id.as_ref(), "microsoft");
    assert_eq!(messages[7].provider_id.as_ref(), "google");
    assert_eq!(messages[8].provider_id.as_ref(), "zai");
    assert_eq!(messages[9].provider_id.as_ref(), "tencent");
    assert_eq!(messages[9].model_id.as_ref(), "hy3-preview-agent");
    assert_eq!(messages[10].provider_id.as_ref(), "ai21");
    assert_eq!(messages[10].model_id.as_ref(), "jamba-1.5-large");
    assert_eq!(messages[11].provider_id.as_ref(), "meta");
    assert_eq!(messages[11].model_id.as_ref(), "llama-3");
}

#[test]
fn test_finalize_token_priced_messages_preserves_custom_provider_literal_tag() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "venice/claude-sonnet-4.5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    litellm.insert(
        "anthropic/claude-sonnet-4.5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(1.0),
            output_cost_per_token: Some(2.0),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut messages = vec![UnifiedMessage::new(
        "claude",
        "claude-sonnet-4.5",
        "venice",
        "custom-provider",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    )];

    finalize_token_priced_messages(&mut messages, Some(&pricing));

    assert_eq!(messages[0].provider_id.as_ref(), "venice");
    assert_eq!(messages[0].cost, 0.2);
}

#[test]
fn test_finalize_token_priced_messages_preserves_owl_provider_identity() {
    let mut messages = vec![UnifiedMessage::new(
        "opencode",
        "gpt-5.2",
        "openai-owl",
        "owl-provider",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    )];

    finalize_token_priced_messages(&mut messages, None);

    assert_eq!(messages[0].provider_id.as_ref(), "owl");
}

#[test]
#[should_panic(expected = "token count exceeds i64::MAX while aggregating usage")]
fn test_positive_token_total_rejects_overflow() {
    let tokens = TokenBreakdown {
        input: i64::MAX,
        output: i64::MAX,
        cache_read: i64::MAX,
        cache_write: i64::MAX,
        reasoning: i64::MAX,
    };

    let _ = positive_token_total(&tokens);
}

#[test]
#[should_panic(expected = "token total exceeds i64::MAX")]
fn test_token_breakdown_total_rejects_overflow() {
    let tokens = TokenBreakdown {
        input: i64::MAX,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };

    let _ = tokens.total();
}

#[test]
#[should_panic(expected = "token count exceeds i64::MAX while aggregating usage")]
fn test_model_aggregation_rejects_overflowing_bucket_fold() {
    let message = || {
        UnifiedMessage::new(
            "antigravity-cli",
            "gemini-3-pro",
            "google",
            "overflow-session",
            1_700_000_000_000,
            TokenBreakdown {
                input: i64::MAX,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        )
    };

    let _ = aggregate_model_usage_entries(vec![message(), message()], &GroupBy::Model);
}

#[test]
fn test_apply_token_pricing_overrides_cost_when_pricing_exists() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4o".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "codex",
        "gpt-4o",
        "provider",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.02);
}

#[test]
fn test_apply_token_pricing_resolves_canonical_longcat_model() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "longcat-flash-3b".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "claudecode",
        "longcat-flash-3b",
        "meituan",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.02);
}

#[test]
fn test_apply_token_pricing_uses_same_price_for_zed_and_other_clients() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4-5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let tokens = TokenBreakdown {
        input: 10,
        output: 5,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    let mut zed_msg = UnifiedMessage::new(
        "zed",
        "claude-sonnet-4-5",
        crate::sessions::zed::ZED_HOSTED_PROVIDER,
        "session-1",
        1_733_011_200_000,
        tokens.clone(),
        0.0,
    );
    let mut claude_msg = UnifiedMessage::new(
        "claudecode",
        "claude-sonnet-4-5",
        crate::sessions::zed::ZED_HOSTED_PROVIDER,
        "session-1",
        1_733_011_200_000,
        tokens,
        0.0,
    );

    apply_token_pricing(&mut zed_msg, Some(&pricing));
    apply_token_pricing(&mut claude_msg, Some(&pricing));

    assert_eq!(zed_msg.cost, claude_msg.cost);
    assert!((zed_msg.cost - 0.020).abs() < 1e-12);
}

#[test]
fn test_apply_token_pricing_custom_zed_price_is_final_price() {
    let mut custom = HashMap::new();
    custom.insert(
        "claude-sonnet-4-5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.003),
            output_cost_per_token: Some(0.004),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new_with_custom(
        pricing::custom::CustomPricing::from_models(custom),
        HashMap::new(),
        HashMap::new(),
    );

    let mut msg = UnifiedMessage::new(
        "zed",
        "claude-sonnet-4-5",
        crate::sessions::zed::ZED_HOSTED_PROVIDER,
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert!((msg.cost - 0.050).abs() < 1e-12);
}

#[test]
fn test_apply_token_pricing_uses_upstream_provider_for_zed_byok() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4-5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "zed",
        "claude-sonnet-4-5",
        "anthropic",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert!((msg.cost - 0.020).abs() < 1e-12);
}

#[test]
fn test_apply_token_pricing_uses_reasoning_for_gemini() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gemini-2.5-pro".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "gemini",
        "gemini-2.5-pro",
        "google",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 7,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.034);
}

#[test]
fn test_apply_token_pricing_uses_cache_read_pricing_for_gemini() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gemini-2.5-pro".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            cache_read_input_token_cost: Some(0.0001),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "gemini",
        "gemini-2.5-pro",
        "google",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 7,
            cache_write: 0,
            reasoning: 3,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.0267);
}

#[test]
fn test_finalize_token_pricing_cleans_free_variant_before_lookup() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "nemotron-3-ultra".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let msg = UnifiedMessage::new(
        "opencode",
        "nemotron-3-ultra-free",
        "nvidia",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );
    let mut messages = vec![msg];

    finalize_token_priced_messages(&mut messages, Some(&pricing));

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].model_id.as_ref(), "nemotron-3-ultra");
    assert!(messages[0].cost > 0.0);
}

#[test]
fn test_finalize_token_pricing_cleans_date_variant_before_lookup() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4o-mini".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let msg = UnifiedMessage::new(
        "copilot",
        "gpt-4o-mini-2024-07-18",
        "openai",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );
    let mut messages = vec![msg];

    finalize_token_priced_messages(&mut messages, Some(&pricing));

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].model_id.as_ref(), "gpt-4o-mini");
    assert!(messages[0].cost > 0.0);
}

#[test]
fn test_finalize_token_pricing_cleans_repeated_date_variant_before_lookup() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4o-mini".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut messages = vec![
        UnifiedMessage::new(
            "copilot",
            "gpt-4o-mini-2024-07-18",
            "openai",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
        UnifiedMessage::new(
            "copilot",
            "gpt-4o-mini-2024-07-18",
            "openai",
            "session-2",
            1_733_011_201_000,
            TokenBreakdown {
                input: 20,
                output: 10,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        ),
    ];

    finalize_token_priced_messages(&mut messages, Some(&pricing));

    assert_eq!(messages.len(), 2);
    assert!(messages
        .iter()
        .all(|message| message.model_id.as_ref() == "gpt-4o-mini"));
    assert!(messages.iter().all(|message| message.cost > 0.0));
}

#[test]
fn test_apply_token_pricing_prefers_provider_aware_match() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "xai/grok-code-fast-1-0825".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure_ai/grok-code-fast-1".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "opencode",
        "grok-code",
        "azure",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_uses_nested_reseller_exact_match() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure/openai/gpt-4".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "opencode",
        "gpt-4",
        "azure",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_clears_cost_without_exact_pricing() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "fireworks_ai/accounts/fireworks/models/deepseek-r1-0528-distill-qwen3-8b".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.0000002),
            output_cost_per_token: Some(0.0000002),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "deepseek/deepseek-v4-pro".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.000001),
            output_cost_per_token: Some(0.000002),
            ..Default::default()
        },
    );

    let pricing = pricing::PricingService::new(litellm, openrouter);
    let mut msg = UnifiedMessage::new(
        "opencode",
        "accounts/fireworks/models/deepseek-v4-pro",
        "fireworks",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.123,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.0);
}

#[test]
fn test_apply_token_pricing_prefers_provider_specific_exact_match_over_plain_exact() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gemini-2.5-pro".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "google/gemini-2.5-pro".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            cache_creation_input_token_cost: Some(0.01),
            ..Default::default()
        },
    );

    let pricing = pricing::PricingService::new(litellm, openrouter);

    let mut msg = UnifiedMessage::new(
        "opencode",
        "gemini-2.5-pro",
        "google",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 3,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.05);
}

#[test]
fn test_apply_token_pricing_normalizes_openai_codex_provider() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-5.2-preview".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    litellm.insert(
        "google/gpt-5.2-preview-max".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.1),
            output_cost_per_token: Some(0.2),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "openclaw",
        "gpt-5.2",
        "openai-codex",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_normalizes_openai_pro_provider() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-5.2-preview".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "kimi",
        "gpt-5.2",
        "openai-pro",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_prices_owl_gpt_as_openai() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-5.2-preview".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "opencode",
        "gpt-5.2",
        "openai-owl",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_prices_owl_claude_as_anthropic() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "anthropic/claude-sonnet-4-5".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "opencode",
        "claude-sonnet-4-5",
        "openai-owlc",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_prices_owl_minimax_as_minimax() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "minimax/minimax-m2.1".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "opencode",
        "MiniMax-M2.1",
        "friend.owl",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_prices_claude_code_gpt_5_3_codex() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5.3-codex".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.00000175),
            output_cost_per_token: Some(0.000014),
            cache_read_input_token_cost: Some(0.000000175),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "claude",
        "gpt-5.3-codex",
        "openai",
        "session-1",
        1_776_000_000_000,
        TokenBreakdown {
            input: 1_000_000,
            output: 100_000,
            cache_read: 50_000,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    let expected = 1.75 + 1.4 + 0.00875;
    assert!((msg.cost - expected).abs() < 1e-12);
}

#[test]
fn test_apply_token_pricing_prices_claude_code_minimax_model() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "minimax/minimax-m2.1".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(litellm, HashMap::new());

    let mut msg = UnifiedMessage::new(
        "claude",
        "MiniMax-M2.1",
        "minimax",
        "session-1",
        1_776_000_000_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    assert_eq!(msg.cost, 0.2);
}

#[test]
fn test_apply_token_pricing_prices_canonical_kimi_k2_6() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "moonshotai/kimi-k2.6".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(9.5e-7),
            output_cost_per_token: Some(0.000004),
            ..Default::default()
        },
    );
    let pricing = pricing::PricingService::new(HashMap::new(), openrouter);

    let mut msg = UnifiedMessage::new(
        "kimi",
        "kimi-k2.6",
        "kimi",
        "session-1",
        1_776_000_000_000,
        TokenBreakdown {
            input: 1_000_000,
            output: 250_000,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(&pricing));

    let expected = 1_000_000.0 * 9.5e-7 + 250_000.0 * 0.000004;
    assert!((msg.cost - expected).abs() < 1e-12);
    assert!(msg.cost > 0.0);
}

#[test]
fn test_select_local_parse_pricing_prefers_fresh_service_for_new_models() {
    let mut fresh_litellm = HashMap::new();
    fresh_litellm.insert(
        "gpt-5.4".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.000002),
            output_cost_per_token: Some(0.00001),
            ..Default::default()
        },
    );
    let fresh = Arc::new(pricing::PricingService::new(fresh_litellm, HashMap::new()));
    let stale = pricing::PricingService::new(HashMap::new(), HashMap::new());
    let selected = select_local_parse_pricing(Ok(Arc::clone(&fresh)), || Some(stale)).unwrap();

    let mut msg = UnifiedMessage::new(
        "opencode",
        "gpt-5.4",
        "openai",
        "session-1",
        1_733_011_200_000,
        TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
    );

    apply_token_pricing(&mut msg, Some(selected.as_ref()));

    assert!(msg.cost > 0.0);
}

#[test]
fn test_select_local_parse_pricing_falls_back_to_stale_cache_on_fetch_error() {
    let mut stale_litellm = HashMap::new();
    stale_litellm.insert(
        "gpt-5.2".into(),
        pricing::ModelPricing {
            input_cost_per_token: Some(0.00000175),
            output_cost_per_token: Some(0.000014),
            ..Default::default()
        },
    );
    let stale = pricing::PricingService::new(stale_litellm, HashMap::new());

    let selected =
        select_local_parse_pricing(Err("network failed".to_string()), || Some(stale)).unwrap();

    assert!(selected.lookup_with_source("gpt-5.2", None).is_some());
}

#[test]
fn test_select_local_parse_pricing_does_not_evaluate_stale_fallback_on_fresh_success() {
    let fresh = Arc::new(pricing::PricingService::new(HashMap::new(), HashMap::new()));
    let mut stale_called = false;

    let selected = select_local_parse_pricing(Ok(Arc::clone(&fresh)), || {
        stale_called = true;
        None
    })
    .unwrap();

    assert!(Arc::ptr_eq(&selected, &fresh));
    assert!(!stale_called);
}

#[test]
#[serial_test::serial]
fn test_parse_all_messages_with_pricing_keeps_gateway_message_under_real_client_filter() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let conn =
        create_opencode_sqlite_db(&temp_dir.path().join(".local/share/opencode/opencode.db"));
    insert_opencode_sqlite_message(
        &conn,
        "msg-1",
        "session-1",
        "",
        r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"hf:deepseek-ai/DeepSeek-V3-0324","providerID":"unknown","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
    );

    let pricing = pricing::PricingService::new(HashMap::new(), HashMap::new());
    let messages = parse_all_messages_with_pricing(
        temp_dir.path().to_str().unwrap(),
        &["opencode".to_string()],
        Some(&pricing),
    )
    .unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].client.as_ref(), "opencode");
    assert_eq!(messages[0].model_id.as_ref(), "deepseek-v3");
    assert_eq!(messages[0].provider_id.as_ref(), "deepseek");
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_preserves_gateway_message_client_counts() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let conn =
        create_opencode_sqlite_db(&temp_dir.path().join(".local/share/opencode/opencode.db"));
    insert_opencode_sqlite_message(
        &conn,
        "msg-1",
        "session-1",
        "",
        r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
    );

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::OpenCode), 1);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "opencode");
    assert_eq!(parsed.messages[0].model_id.as_ref(), "deepseek-v3");
    assert_eq!(parsed.messages[0].provider_id.as_ref(), "fireworks");
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_honors_scanner_settings_opencode_db_paths() {
    // Regression guard: local message loading must forward
    // `options.scanner_settings` into OpenCode adapter discovery. Users with
    // `scanner.opencodeDbPaths` pointing at an OPENCODE_DB outside the
    // XDG data dir would see no rows through the clients/wrapped
    // command paths even though model/monthly/graph reports honored
    // the same config.
    let temp_dir = tempfile::TempDir::new().unwrap();
    // Deliberately do not create ~/.local/share/opencode so nothing
    // is auto-discoverable; the only db the scanner can find must
    // come from `scanner_settings`.
    let outside_dir = temp_dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let external_db = outside_dir.join("opencode.db");

    let conn = rusqlite::Connection::open(&external_db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
         CREATE TABLE message (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             data TEXT NOT NULL
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "ext-msg-1",
            "ext-session",
            r#"{
                "role": "assistant",
                "modelID": "claude-sonnet-4",
                "providerID": "anthropic",
                "tokens": { "input": 42, "output": 7, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": { "created": 1700000000000.0 }
            }"#
        ],
    )
    .unwrap();
    drop(conn);

    // Without scanner_settings: no rows (nothing auto-discoverable).
    let parsed_default = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();
    assert_eq!(parsed_default.counts.get(ClientId::OpenCode), 0);
    assert!(parsed_default.messages.is_empty());

    // With scanner_settings pointing at the external db: the user
    // row must show up.
    let parsed_with_settings = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            opencode_db_paths: vec![external_db.clone()],
            ..Default::default()
        },
    })
    .unwrap();
    assert_eq!(
        parsed_with_settings.counts.get(ClientId::OpenCode),
        1,
        "scanner.opencodeDbPaths must reach the local message loading path"
    );
    assert_eq!(parsed_with_settings.messages.len(), 1);
    assert_eq!(parsed_with_settings.messages[0].client.as_ref(), "opencode");
    assert_eq!(
        parsed_with_settings.messages[0].model_id.as_ref(),
        "claude-sonnet-4"
    );
}

#[test]
#[serial_test::serial]
fn test_missing_configured_opencode_database_is_an_explicit_error() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(temp_dir.path());
    let missing_db = temp_dir.path().join("missing/custom-current.db");

    let loaded = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        scanner_settings: scanner::ScannerSettings {
            opencode_db_paths: vec![missing_db.clone()],
            ..Default::default()
        },
        ..LocalParseOptions::default()
    })
    .unwrap();

    assert!(loaded.messages.is_empty());
    assert_eq!(loaded.health.failed_sources(), 1);
    let source = &loaded.health.sources()[0];
    assert_eq!(source.path, missing_db);
    let failure = source.status.failure().unwrap();
    assert!(
        failure
            .message
            .contains("read source metadata and file identity")
            || failure
                .message
                .contains("open current OpenCode SQLite database")
            || failure.operation.contains("snapshot source metadata"),
        "failure must identify the failed source operation: {failure:?}"
    );

    let unit = crate::adapters::SourceUnit::sqlite_with_wal(ClientId::OpenCode, missing_db.clone())
        .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);
    assert!(message_cache::SourceMessageCache::load()
        .unwrap()
        .get_meta(&missing_db, unit.parser_version)
        .unwrap()
        .is_none());
}

#[test]
#[serial_test::serial]
fn time_metrics_report_preserves_source_health() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(temp_dir.path());
    let missing_db = temp_dir.path().join("missing/custom-current.db");

    let report = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::get_time_metrics_report(ReportOptions {
            home_dir: Some(temp_dir.path().to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string()]),
            scanner_settings: scanner::ScannerSettings {
                opencode_db_paths: vec![missing_db.clone()],
                ..Default::default()
            },
            ..ReportOptions::default()
        }))
        .expect("a broken third-party source must not abort time-metrics");

    assert_eq!(report.metrics.session_count, 0);
    assert!(!report.health.complete);
    assert_eq!(report.health.failed_sources, 1);
    assert_eq!(report.health.issues[0].source, "opencode");
    assert_eq!(report.health.issues[0].issue, "source-unavailable");
}

#[test]
#[serial_test::serial]
fn local_client_counts_preserve_source_health() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(temp_dir.path());
    let missing_db = temp_dir.path().join("missing/client-counts.db");

    let report = super::count_local_client_messages(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        scanner_settings: scanner::ScannerSettings {
            opencode_db_paths: vec![missing_db.clone()],
            ..Default::default()
        },
        ..LocalParseOptions::default()
    })
    .expect("a broken third-party source must not abort client counts");

    assert_eq!(report.counts.get(ClientId::OpenCode), 0);
    assert!(!report.health.complete);
    assert_eq!(report.health.failed_sources, 1);
    assert_eq!(report.health.issues[0].source, "opencode");
    assert_eq!(report.health.issues[0].issue, "source-unavailable");
}

#[test]
#[serial_test::serial]
fn public_raw_message_report_preserves_source_health_and_metadata() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(temp_dir.path());
    let missing_db = temp_dir.path().join("missing/raw-report.db");

    let report = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::parse_local_unified_messages_with_pricing(
            LocalParseOptions {
                home_dir: Some(temp_dir.path().to_string_lossy().into_owned()),
                use_env_roots: false,
                clients: Some(vec!["opencode".to_string()]),
                scanner_settings: scanner::ScannerSettings {
                    opencode_db_paths: vec![missing_db.clone()],
                    ..Default::default()
                },
                ..LocalParseOptions::default()
            },
            None,
        ))
        .expect("a broken source must produce a degraded raw-message report");

    assert!(report.data.is_empty());
    assert!(!report.health.complete);
    assert_eq!(report.health.failed_sources, 1);
    assert_eq!(report.health.issues[0].source, "opencode");
    assert_eq!(report.health.issues[0].issue, "source-unavailable");
    assert_ne!(
        report.metadata.source_inventory_signature.as_bytes(),
        &[0_u8; 32]
    );
}

#[test]
#[serial_test::serial]
fn test_opencode_auto_discovery_error_reaches_public_loader() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let data_root = temp_dir.path().join(".local/share/opencode");
    std::fs::create_dir_all(data_root.parent().unwrap()).unwrap();
    std::fs::write(&data_root, "not a directory").unwrap();

    let loaded = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_string_lossy().into_owned()),
        use_env_roots: false,
        clients: Some(vec!["opencode".to_string()]),
        ..LocalParseOptions::default()
    })
    .unwrap();

    assert!(loaded.messages.is_empty());
    assert_eq!(loaded.health.failed_sources(), 1);
    let failure = loaded.health.sources()[0].status.failure().unwrap();
    assert!(
        failure
            .message
            .contains("failed to read OpenCode data directory"),
        "{failure:?}"
    );
    assert!(
        failure.message.contains(data_root.to_str().unwrap()),
        "{failure:?}"
    );
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_honors_scanner_extra_scan_paths_for_hermes_profile_db() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let profile_dir = temp_dir.path().join(".hermes/profiles/director_planning");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile_db = profile_dir.join("state.db");
    let conn = create_hermes_sqlite_db(&profile_db);
    insert_hermes_session(
        &conn,
        "hermes-extra-session",
        "claude-sonnet-4",
        2,
        100,
        25,
        0.07,
    );
    drop(conn);

    let parsed_default = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["hermes".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();
    assert_eq!(parsed_default.counts.get(ClientId::Hermes), 0);
    assert!(parsed_default.messages.is_empty());

    let mut extra_scan_paths = std::collections::BTreeMap::new();
    extra_scan_paths.insert("hermes".to_string(), vec![profile_dir]);
    let parsed_with_settings = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["hermes".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(parsed_with_settings.counts.get(ClientId::Hermes), 2);
    assert_eq!(parsed_with_settings.messages.len(), 1);
    assert_eq!(parsed_with_settings.messages[0].client.as_ref(), "hermes");
    assert_eq!(
        parsed_with_settings.messages[0].agent.as_deref(),
        Some("Hermes Agent")
    );
    assert_eq!(
        parsed_with_settings.messages[0].session_id.as_ref(),
        "hermes-extra-session"
    );
    assert_eq!(
        parsed_with_settings.messages[0].model_id.as_ref(),
        "claude-sonnet-4"
    );
    assert_eq!(parsed_with_settings.messages[0].tokens.input, 100);
    assert_eq!(parsed_with_settings.messages[0].tokens.output, 25);
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_honors_scanner_extra_scan_paths_for_zed_threads_db() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let windows_threads_dir = temp_dir.path().join("AppData/Local/Zed/threads");
    std::fs::create_dir_all(&windows_threads_dir).unwrap();
    let threads_db = windows_threads_dir.join("threads.db");
    let conn = create_zed_sqlite_db(&threads_db);
    insert_zed_thread(&conn, "zed-extra-thread", "claude-sonnet-4-5");
    drop(conn);

    let parsed_default = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["zed".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();
    assert_eq!(parsed_default.counts.get(ClientId::Zed), 0);
    assert!(parsed_default.messages.is_empty());

    let mut extra_scan_paths = std::collections::BTreeMap::new();
    extra_scan_paths.insert("zed".to_string(), vec![windows_threads_dir]);
    let parsed_with_settings = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["zed".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(parsed_with_settings.counts.get(ClientId::Zed), 1);
    assert_eq!(parsed_with_settings.messages.len(), 1);
    assert_eq!(parsed_with_settings.messages[0].client.as_ref(), "zed");
    assert_eq!(
        parsed_with_settings.messages[0].session_id.as_ref(),
        "zed-extra-thread"
    );
    assert_eq!(
        parsed_with_settings.messages[0].model_id.as_ref(),
        "claude-sonnet-4.5"
    );
    assert_eq!(parsed_with_settings.messages[0].tokens.input, 42);
    assert_eq!(parsed_with_settings.messages[0].tokens.output, 7);
}

#[test]
#[serial_test::serial]
fn test_default_graph_includes_antigravity_cli_database_rows() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    write_single_antigravity_cli_fixture(temp_dir.path());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let graph = rt
        .block_on(generate_graph_with_loaded_pricing(
            ReportOptions {
                home_dir: Some(temp_dir.path().to_string_lossy().to_string()),
                use_env_roots: false,
                clients: None,
                since: None,
                until: None,
                year: None,
                group_by: GroupBy::default(),
                scanner_settings: scanner::ScannerSettings::default(),
            },
            None,
        ))
        .unwrap();

    assert_eq!(graph.summary.clients, vec!["antigravity"]);
    assert_eq!(graph.summary.models, vec!["gemini-3.5-flash"]);
    assert_eq!(graph.summary.total_tokens, 19);
    assert_eq!(graph.contributions.len(), 1);
    assert_eq!(graph.contributions[0].clients[0].client, "antigravity");
    assert_eq!(
        graph.contributions[0].clients[0].model_id,
        "gemini-3.5-flash"
    );
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_dedups_zed_threads_across_default_and_extra_dbs() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Place threads.db at the default platform path so the scanner finds it
    // as `zed_db` AND we also pass it via extraScanPaths.
    let default_threads_dir = temp_dir.path().join(".local/share/zed/threads");
    std::fs::create_dir_all(&default_threads_dir).unwrap();
    let default_db = default_threads_dir.join("threads.db");
    let conn = create_zed_sqlite_db(&default_db);
    insert_zed_thread(&conn, "shared-zed-thread", "claude-sonnet-4-5");
    drop(conn);

    // Point extraScanPaths.zed at the same directory — dedup should prevent
    // the thread from appearing twice.
    let mut extra_scan_paths = std::collections::BTreeMap::new();
    extra_scan_paths.insert("zed".to_string(), vec![default_threads_dir.clone()]);
    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["zed".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        },
    })
    .unwrap();

    // Should see exactly 1 message, not 2 (deduped by canonicalize).
    assert_eq!(parsed.counts.get(ClientId::Zed), 1);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].session_id.as_ref(), "shared-zed-thread");
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_zed_extra_scan_paths_nonexistent_dir_is_silent() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let mut extra_scan_paths = std::collections::BTreeMap::new();
    extra_scan_paths.insert(
        "zed".to_string(),
        vec![temp_dir.path().join("does/not/exist")],
    );
    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["zed".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Zed), 0);
    assert!(parsed.messages.is_empty());
}

#[test]
#[serial_test::serial]
fn test_driver_uses_zed_adapter_when_only_zed_requested() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let zed_threads_dir = temp_dir.path().join("zed-fixture/threads");
    std::fs::create_dir_all(&zed_threads_dir).unwrap();
    let zed_db = zed_threads_dir.join("threads.db");
    let zed_conn = create_zed_sqlite_db(&zed_db);
    insert_zed_thread(&zed_conn, "zed-only-thread", "claude-sonnet-4-5");
    drop(zed_conn);

    write_single_opencode_sqlite_fixture(temp_dir.path());

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["zed".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner_settings_for_zed_threads_dir(zed_threads_dir),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Zed), 1);
    assert_eq!(
        parsed.counts.get(ClientId::OpenCode),
        0,
        "an explicit Zed-only request must not scan OpenCode SQLite"
    );
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "zed");
    assert_eq!(parsed.messages[0].session_id.as_ref(), "zed-only-thread");
}

#[test]
#[serial_test::serial]
fn test_driver_uses_simple_file_adapter_when_only_amp_requested() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let amp_dir = temp_dir.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    std::fs::write(
        amp_dir.join("T-simple.json"),
        r#"{
            "id": "amp-thread",
            "created": 1747800000000,
            "messages": [
                {
                    "role": "assistant",
                    "messageId": 1,
                    "usage": {
                        "timestamp": "2026-05-21T04:00:00Z",
                        "model": "claude-opus-4-7",
                        "inputTokens": 10,
                        "outputTokens": 2
                    }
                }
            ]
        }"#,
    )
    .unwrap();

    write_single_opencode_sqlite_fixture(temp_dir.path());

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["amp".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Amp), 1);
    assert_eq!(
        parsed.counts.get(ClientId::OpenCode),
        0,
        "an explicit Amp-only request must not scan OpenCode SQLite"
    );
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "amp");
    assert_eq!(parsed.messages[0].model_id.as_ref(), "claude-opus-4.7");
}

#[test]
#[serial_test::serial]
fn test_driver_uses_custom_file_adapter_when_only_codebuff_requested() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let cache_home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeEnvGuard::set(cache_home.path());

    let codebuff_dir = temp_dir
        .path()
        .join(".config/manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
    std::fs::create_dir_all(&codebuff_dir).unwrap();
    std::fs::write(
        codebuff_dir.join("chat-messages.json"),
        r#"[
            { "variant": "user", "content": "hi" },
            { "variant": "ai",
              "timestamp": "2025-12-20T12:00:05.000Z",
              "metadata": {
                "model": "claude-sonnet-4-20250514",
                "usage": { "inputTokens": 10, "outputTokens": 5 }
              }
            }
        ]"#,
    )
    .unwrap();

    write_single_opencode_sqlite_fixture(temp_dir.path());

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["codebuff".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Codebuff), 1);
    assert_eq!(
        parsed.counts.get(ClientId::OpenCode),
        0,
        "an explicit Codebuff-only request must not scan OpenCode SQLite"
    );
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "codebuff");
    assert_eq!(parsed.messages[0].model_id.as_ref(), "claude-sonnet-4");
}

#[test]
#[serial_test::serial]
fn test_driver_uses_pi_and_omp_adapters_when_requested() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let pi_path = temp_dir
        .path()
        .join(".pi/agent/sessions/project/pi-session.jsonl");
    write_pi_fixture(&pi_path);

    let omp_session_root = temp_dir
        .path()
        .join(".omp/agent/sessions/project/root-session");
    write_omp_parent_child_fixture(&omp_session_root);

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["pi".to_string(), "omp".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Pi), 1);
    assert_eq!(parsed.counts.get(ClientId::Omp), 2);
    assert!(parsed
        .messages
        .iter()
        .any(|message| message.client.as_ref() == "pi"
            && message.session_id.as_ref() == "pi_ses_001"));
    assert!(parsed.messages.iter().any(|message| {
        message.client.as_ref() == "omp"
            && message.session_id.as_ref() == "child-session"
            && message.agent.as_deref() == Some("OMP Reviewer")
    }));
}

#[test]
#[serial_test::serial]
fn test_driver_all_clients_includes_each_adapter_without_duplicate() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let zed_threads_dir = temp_dir.path().join("zed-fixture/threads");
    std::fs::create_dir_all(&zed_threads_dir).unwrap();
    let zed_db = zed_threads_dir.join("threads.db");
    let zed_conn = create_zed_sqlite_db(&zed_db);
    insert_zed_thread(&zed_conn, "zed-all-thread", "claude-sonnet-4-5");
    drop(zed_conn);

    write_single_opencode_sqlite_fixture(temp_dir.path());

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(Vec::new()),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner_settings_for_zed_threads_dir(zed_threads_dir),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Zed), 1);
    assert_eq!(parsed.counts.get(ClientId::OpenCode), 1);
    assert_eq!(
        parsed
            .messages
            .iter()
            .filter(|message| message.client.as_ref() == "zed")
            .count(),
        1
    );
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_dedups_hermes_sessions_across_default_and_extra_dbs() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    let default_dir = temp_dir.path().join(".hermes");
    std::fs::create_dir_all(&default_dir).unwrap();
    let default_db = default_dir.join("state.db");
    let default_conn = create_hermes_sqlite_db(&default_db);
    insert_hermes_session(
        &default_conn,
        "shared-hermes-session",
        "claude-sonnet-4",
        2,
        100,
        25,
        0.07,
    );
    drop(default_conn);

    let profile_dir = temp_dir.path().join(".hermes/profiles/director_planning");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile_db = profile_dir.join("state.db");
    let profile_conn = create_hermes_sqlite_db(&profile_db);
    insert_hermes_session(
        &profile_conn,
        "shared-hermes-session",
        "claude-sonnet-4",
        9,
        999,
        999,
        9.99,
    );
    drop(profile_conn);

    let mut extra_scan_paths = std::collections::BTreeMap::new();
    extra_scan_paths.insert("hermes".to_string(), vec![profile_db]);
    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["hermes".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Hermes), 2);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(
        parsed.messages[0].session_id.as_ref(),
        "shared-hermes-session"
    );
    assert_eq!(parsed.messages[0].tokens.input, 100);
    assert_eq!(parsed.messages[0].tokens.output, 25);
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_claude_filter_ignores_scanner_settings_opencode_db_paths() {
    // Regression guard for the scanner client-filter bypass: even
    // when `scanner.opencodeDbPaths` pins an external opencode db,
    // a `--clients claude` request must NOT pull in OpenCode rows.
    // Before the fix, the merge ran outside the OpenCode-enabled
    // guard so user-pinned dbs leaked through both `messages` and
    // `counts` (the latter is computed before the message-level
    // client filter, so even the post-filter pipeline could not
    // hide a leaked count).
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Claude session: one assistant message, the only thing the
    // filter should accept.
    let claude_dir = temp_dir.path().join(".claude/projects/myproject");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("conversation.jsonl"),
        r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
"#,
    )
    .unwrap();

    // External opencode.db that the user has pinned via
    // scanner.opencodeDbPaths. Without the fix, this would leak
    // into the Claude-only result.
    let outside_dir = temp_dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let external_db = outside_dir.join("opencode.db");
    let conn = rusqlite::Connection::open(&external_db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
         CREATE TABLE message (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             data TEXT NOT NULL
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "leaked-opencode",
            "should-not-show-up",
            r#"{
                "role": "assistant",
                "modelID": "claude-sonnet-4",
                "providerID": "anthropic",
                "tokens": { "input": 9999, "output": 9999, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": { "created": 1700000000000.0 }
            }"#
        ],
    )
    .unwrap();
    drop(conn);

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["claude".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings {
            opencode_db_paths: vec![external_db.clone()],
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(
        parsed.counts.get(ClientId::OpenCode),
        0,
        "OpenCode count must stay zero under a Claude-only filter even \
         when scanner.opencodeDbPaths is set"
    );
    assert_eq!(
        parsed.counts.get(ClientId::Claude),
        1,
        "Claude message must still be counted"
    );
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "claude");
    assert!(
        parsed
            .messages
            .iter()
            .all(|m| m.client.as_ref() != "opencode"),
        "no OpenCode messages may leak into a Claude-only result, got {:?}",
        parsed.messages
    );
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_claude_transcripts_count_only_usage_metadata() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let transcripts_dir = temp_dir.path().join(".claude/transcripts");
    std::fs::create_dir_all(&transcripts_dir).unwrap();
    std::fs::write(
        transcripts_dir.join("ses_123456789012345678901234567.jsonl"),
        r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"assistant","timestamp":"2026-04-01T10:00:01.000Z","requestId":"req_wrapper","message":{"id":"msg_wrapper","model":"claude-sonnet-4","usage":{"input_tokens":123,"output_tokens":45,"cache_read_input_tokens":67,"cache_creation_input_tokens":8}}}
"#,
    )
    .unwrap();
    std::fs::write(
        transcripts_dir.join("ses_765432109876543210987654321.jsonl"),
        r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"tool_use","timestamp":"2026-04-01T10:00:01.000Z","message":{"content":"Run tool"}}
{"type":"tool_result","timestamp":"2026-04-01T10:00:02.000Z","message":{"content":"Tool result"}}
"#,
    )
    .unwrap();

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["claude".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Claude), 1);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "claude");
    assert_eq!(
        parsed.messages[0].session_id.as_ref(),
        "ses_123456789012345678901234567"
    );
    assert_eq!(parsed.messages[0].model_id.as_ref(), "claude-sonnet-4");
    assert_eq!(parsed.messages[0].tokens.input, 123);
    assert_eq!(parsed.messages[0].tokens.output, 45);
    assert_eq!(parsed.messages[0].tokens.cache_read, 67);
    assert_eq!(parsed.messages[0].tokens.cache_write, 8);
}

#[test]
#[serial_test::serial]
fn test_local_message_loader_amp_reads_current_thread_files() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let amp_dir = temp_dir.path().join(".local/share/amp/threads");
    std::fs::create_dir_all(&amp_dir).unwrap();
    std::fs::write(
        amp_dir.join("T-current.json"),
        r#"{
            "id": "current-thread",
            "created": 1747800000000,
            "messages": [
                {
                    "role": "assistant",
                    "messageId": 1,
                    "usage": {
                        "timestamp": "2026-05-21T04:00:00Z",
                        "model": "claude-opus-4-7",
                        "inputTokens": 10,
                        "outputTokens": 2
                    }
                }
            ]
        }"#,
    )
    .unwrap();

    let parsed = load_local_messages_for_test(LocalParseOptions {
        home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
        use_env_roots: false,
        clients: Some(vec!["amp".to_string()]),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner::ScannerSettings::default(),
    })
    .unwrap();

    assert_eq!(parsed.counts.get(ClientId::Amp), 1);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].client.as_ref(), "amp");
    assert_eq!(parsed.messages[0].model_id.as_ref(), "claude-opus-4.7");
    assert_eq!(parsed.messages[0].provider_id.as_ref(), "anthropic");
    assert_eq!(parsed.messages[0].tokens.input, 10);
    assert_eq!(parsed.messages[0].tokens.output, 2);
}
