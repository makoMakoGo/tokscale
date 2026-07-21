//! TUI data caching for instant startup.
//!
//! This module provides disk-based caching for TUI data to enable instant UI display
//! on launch. Fresh cache data renders without an immediate background scan; stale
//! or missing cache data still triggers a refresh.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use tokscale_core::{
    sessions, GroupBy, InputInventorySignature, ModelPerformance, TuiAcc, TuiSessionEntry,
};

use tokscale_core::ClientId;

use super::data::{
    AgentUsage, ContributionDay, DailyClientInfo, DailyModelInfo, DailyUsage, GraphData,
    HourlyModelInfo, HourlyUsage, ModelUsage, TokenBreakdown, UsageData,
};

/// Cache staleness threshold: 5 minutes (matches TS implementation)
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const CACHE_SCHEMA_VERSION: u32 = 43;

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheReportScope {
    pub resolved_home_dir: String,
    pub use_env_roots: bool,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::{OsStr, OsString};
    use std::{env, fs};
    use tempfile::TempDir;
    use tokscale_core::TuiSessionTokens;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    fn fixture() -> (
        TempDir,
        EnvVarGuard,
        HashSet<ClientId>,
        CacheReportScope,
        Vec<TuiSessionEntry>,
        BTreeMap<String, u64>,
    ) {
        let temp = TempDir::new().unwrap();
        let guard = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", temp.path().as_os_str());
        let clients = HashSet::from([ClientId::Claude]);
        let scope = CacheReportScope::new("/test/home".into(), false, None, None, None);
        let sessions = vec![TuiSessionEntry {
            client: "claude".into(),
            session_id: "session-1".into(),
            is_main_session: true,
            workspace_key: Some("/workspace".into()),
            workspace_label: Some("workspace".into()),
            models: BTreeSet::from(["claude-sonnet".into()]),
            tokens: TuiSessionTokens {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
                reasoning: 3,
            },
            cost: 0.25,
            message_count: 4,
            turn_count: 2,
            first_seen: 100,
            last_seen: 200,
        }];
        let client_space = BTreeMap::from([("claude".into(), 4096)]);
        (temp, guard, clients, scope, sessions, client_space)
    }

    fn signature() -> InputInventorySignature {
        InputInventorySignature::from_bytes([0x39; 32])
    }

    fn nonempty_accumulator(home: &std::path::Path) -> TuiAcc {
        for (project, workspace, model, input_tokens, hour) in [
            ("project-a", "/work/alpha", "claude-sonnet-4.6", 101, 10),
            ("project-b", "/work/beta", "claude-haiku-4.5", 202, 12),
        ] {
            let project_dir = home.join(".claude/projects").join(project);
            fs::create_dir_all(&project_dir).unwrap();
            fs::write(
                project_dir.join("session.jsonl"),
                format!(
                    r#"{{"type":"assistant","timestamp":"2026-05-27T{hour}:00:00.000Z","cwd":"{workspace}","requestId":"request-{project}","message":{{"id":"message-{project}","model":"{model}","usage":{{"input_tokens":{input_tokens},"output_tokens":11,"cache_read_input_tokens":7,"cache_creation_input_tokens":3}}}}}}"#
                ),
            )
            .unwrap();
        }

        let data_dir = home.join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();
        let connection = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, data TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session (id, parent_id, directory) VALUES (?1, NULL, ?2)",
                rusqlite::params!["open-session", "/work/gamma"],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "open-message",
                    "open-session",
                    r#"{"id":"open-message","role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":303,"output":13,"reasoning":5,"cache":{"read":9,"write":4}},"time":{"created":1779879600000,"completed":1779879602500},"agent":"planner"}"#
                ],
            )
            .unwrap();
        drop(connection);

        let loader =
            crate::tui::data::DataLoader::with_filters(Some(home.to_path_buf()), None, None, None);
        let prepared = loader
            .prepare(&[ClientId::Claude, ClientId::OpenCode])
            .unwrap();
        loader
            .execute_tui_bundle_with_diagnostics(prepared)
            .unwrap()
            .accumulator
    }

    fn assert_projection_eq(actual: &UsageData, expected: &UsageData) {
        let actual = serde_json::to_value(CachedProjectionUsageDataRef::from(actual)).unwrap();
        let expected = serde_json::to_value(CachedProjectionUsageDataRef::from(expected)).unwrap();
        assert_eq!(actual, expected);
    }

    fn refresh_canonical_digest(value: &mut serde_json::Value) {
        let canonical = serde_json::to_vec(&value["canonical"]).unwrap();
        value["canonicalDigest"] = serde_json::Value::from(sha256_hex(&canonical));
    }

    #[test]
    #[serial]
    fn schema_43_bundle_round_trips_sessions_and_metadata() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let accumulator = TuiAcc::new();
        let expected_signature = signature();
        let health = tokscale_core::input_health::HealthReport {
            clean_inputs: 1,
            input_data_bytes: 4096,
            ..Default::default()
        };

        let store = save_tui_bundle_cache(
            &accumulator,
            &sessions,
            &client_space,
            &health,
            &clients,
            &scope,
            expected_signature,
        )
        .unwrap();
        let saved = store
            .load_snapshot(&clients, &GroupBy::Model, &scope)
            .unwrap();
        assert_eq!(saved.sessions, sessions);
        assert_eq!(saved.client_space, client_space);
        assert_eq!(saved.data.health, health);
        assert_eq!(
            saved.input_inventory_signature.process_digest(),
            expected_signature.process_digest()
        );
        let raw: serde_json::Value =
            serde_json::from_reader(File::open(cache_file().unwrap()).unwrap()).unwrap();
        assert_eq!(raw["clientUniverse"], serde_json::json!(["claude"]));
        assert_eq!(raw["clientSpace"], serde_json::json!({"claude": 4096}));
        assert_eq!(raw["sessions"][0]["client"], "claude");
        assert!(raw.get("sourceUniverse").is_none());
        assert!(raw.get("sourceSpace").is_none());
        assert!(raw["sessions"][0].get("source").is_none());
        assert_eq!(raw["health"]["inputDataBytes"], 4096);
        assert_eq!(raw["canonicalDigest"].as_str().unwrap().len(), 64);
        assert!(raw["projections"]["model"].get("health").is_none());

        let CacheResult::Fresh(loaded) = load_cache(&clients, &GroupBy::Model, &scope) else {
            panic!("expected a fresh schema-43 bundle");
        };
        assert_eq!(loaded.sessions, sessions);
        assert_eq!(loaded.client_space, client_space);
        assert_eq!(loaded.data.health, health);
    }

    #[test]
    #[serial]
    fn schema_43_nonempty_bundle_round_trips_all_four_public_groupings() {
        let (temp, _guard, _clients, scope, _sessions, _client_space) = fixture();
        let _pricing_guard = EnvVarGuard::set("TOKSCALE_PRICING_CACHE_ONLY", OsStr::new("1"));
        let accumulator = nonempty_accumulator(temp.path());
        let clients = HashSet::from([ClientId::Claude, ClientId::OpenCode]);
        let sessions = vec![
            TuiSessionEntry {
                client: "claude".into(),
                session_id: "claude-session".into(),
                workspace_key: Some("/work/alpha".into()),
                workspace_label: Some("alpha".into()),
                models: BTreeSet::from(["claude-sonnet-4.6".into()]),
                message_count: 2,
                ..Default::default()
            },
            TuiSessionEntry {
                client: "opencode".into(),
                session_id: "open-session".into(),
                workspace_key: Some("/work/gamma".into()),
                workspace_label: Some("gamma".into()),
                models: BTreeSet::from(["gpt-5.5".into()]),
                message_count: 1,
                ..Default::default()
            },
        ];
        let client_space =
            BTreeMap::from([("claude".to_string(), 8192), ("opencode".to_string(), 4096)]);
        let mut health = tokscale_core::input_health::HealthReport {
            clean_inputs: 2,
            input_data_bytes: 12_288,
            ..Default::default()
        };
        health.record_unavailable_input("unrelated-test-client");

        let model_projection = accumulator.project(&GroupBy::Model);
        assert!(model_projection.models.len() >= 3);
        assert!(!model_projection.daily.is_empty());
        assert!(model_projection.hourly.len() >= 3);
        assert!(model_projection
            .graph
            .as_ref()
            .is_some_and(|graph| !graph.weeks.is_empty()));
        let workspace_projection = accumulator.project(&GroupBy::WorkspaceModel);
        let workspace_keys = workspace_projection
            .models
            .iter()
            .filter_map(|model| model.workspace_key.as_deref())
            .collect::<Vec<_>>();
        assert!(
            workspace_keys.len() >= 3,
            "workspace keys: {workspace_keys:?}"
        );
        assert!(
            workspace_projection
                .models
                .iter()
                .any(|model| { model.workspace_key.as_deref() == Some("/work/gamma") }),
            "workspace keys: {workspace_keys:?}"
        );

        let mut store = save_tui_bundle_cache(
            &accumulator,
            &sessions,
            &client_space,
            &health,
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let raw: serde_json::Value =
            serde_json::from_reader(File::open(cache_file().unwrap()).unwrap()).unwrap();
        let cached_day = &raw["projections"]["model"]["daily"][0];
        assert!(cached_day.get("clientBreakdown").is_some());
        assert!(cached_day.get("sourceBreakdown").is_none());

        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let expected = accumulator.project(&group_by);
            let loaded = match load_cache(&clients, &group_by, &scope) {
                CacheResult::Fresh(loaded) | CacheResult::Stale(loaded) => loaded,
                CacheResult::Miss => panic!("schema-43 bundle must load for {group_by}"),
            };
            assert_projection_eq(&loaded.data, &expected);
            assert_eq!(loaded.data.health, health);
            assert_eq!(loaded.sessions, sessions);
            assert_eq!(loaded.client_space, client_space);
            assert!(loaded.data.error.is_none());
        }
        let claude_only = HashSet::from([ClientId::Claude]);
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let expected = accumulator.project_for_clients(&group_by, &claude_only);
            let actual = store.project(&group_by, &claude_only).unwrap();
            assert_projection_eq(&actual, &expected);
            assert!(actual
                .models
                .iter()
                .all(|model| !model.client.contains("opencode")));
        }
        assert!(store.project(&GroupBy::Session, &clients).is_err());
    }

    #[test]
    #[serial]
    fn legacy_schema_versions_are_explicit_misses() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        for schema_version in [38, 39, 40, 41, 42] {
            save_tui_bundle_cache(
                &TuiAcc::new(),
                &sessions,
                &client_space,
                &Default::default(),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let path = cache_file().unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            value["schemaVersion"] = serde_json::Value::from(schema_version);
            refresh_canonical_digest(&mut value);
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(matches!(
                load_cache(&clients, &GroupBy::Model, &scope),
                CacheResult::Miss
            ));
        }
    }

    #[test]
    #[serial]
    fn missing_or_incomplete_canonical_state_is_an_explicit_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let path = cache_file().unwrap();

        for replacement in [None, Some(serde_json::json!({ "model_map": [] }))] {
            save_tui_bundle_cache(
                &TuiAcc::new(),
                &sessions,
                &client_space,
                &Default::default(),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            match replacement.clone() {
                Some(canonical) => value["canonical"] = canonical,
                None => {
                    value.as_object_mut().unwrap().remove("canonical");
                }
            }
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(matches!(
                load_cache(&clients, &GroupBy::Model, &scope),
                CacheResult::Miss
            ));
        }

        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("canonicalDigest");
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn canonical_content_corruption_is_an_explicit_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["canonical"]["next_sequence"] = serde_json::Value::from(1);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn partial_or_foreign_client_membership_is_a_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let path = cache_file().unwrap();

        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["clientSpace"] = serde_json::json!({"codex": 4096});
        refresh_canonical_digest(&mut value);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));

        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["sessions"][0]["client"] = serde_json::Value::from("codex");
        refresh_canonical_digest(&mut value);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn writer_is_atomic_and_projection_store_pins_the_old_inode() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let mut store = save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let parent = path.parent().unwrap();
        assert!(path.is_file());
        assert!(fs::read_dir(parent).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));

        tokscale_core::fs_atomic::write_atomic(&path, b"not-json").unwrap();
        assert!(store.project(&GroupBy::WorkspaceModel, &clients).is_ok());
    }

    #[test]
    #[serial]
    fn fresh_and_stale_are_derived_from_the_bundle_timestamp() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &Default::default(),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Fresh(_)
        ));

        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["timestamp"] = serde_json::Value::from(0_u64);
        refresh_canonical_digest(&mut value);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Stale(_)
        ));
    }
}

impl CacheReportScope {
    fn new(
        resolved_home_dir: String,
        use_env_roots: bool,
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> Self {
        Self {
            resolved_home_dir,
            use_env_roots,
            since,
            until,
            year,
        }
    }

    pub fn for_request(
        home_dir: Option<String>,
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> anyhow::Result<Self> {
        let (resolved_home_dir, use_env_roots) = match home_dir {
            Some(home_dir) => (home_dir, false),
            None => (
                dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
                    .to_string_lossy()
                    .into_owned(),
                true,
            ),
        };

        Ok(Self::new(
            resolved_home_dir,
            use_env_roots,
            since,
            until,
            year,
        ))
    }
}

/// Default usage projection selected when the TUI starts. Schema 43 stores all
/// four public projections plus canonical client-aware state, so Group By and
/// Clients are presentation state rather than cache keys.
pub const TUI_DEFAULT_GROUP_BY: GroupBy = GroupBy::Model;

/// Get the cache directory path
/// Uses the canonical cache subdirectory resolved by `tokscale-core`.
fn cache_dir() -> Result<PathBuf, tokscale_core::paths::ConfigDirUnavailable> {
    crate::paths::try_get_cache_dir()
}

/// Get the cache file path
fn cache_file() -> Result<PathBuf, tokscale_core::paths::ConfigDirUnavailable> {
    cache_dir().map(|directory| directory.join("tui-data-cache.json"))
}

/// Serializable version of UsageData
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageData {
    models: Vec<CachedModelUsage>,
    agents: Vec<CachedAgentUsage>,
    daily: Vec<CachedDailyUsage>,
    hourly: Vec<CachedHourlyUsage>,
    graph: Option<CachedGraphData>,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedTokenBreakdown {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedModelUsage {
    model: String,
    provider: String,
    client: String,
    #[serde(default)]
    workspace_key: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    tokens: CachedTokenBreakdown,
    cost: f64,
    #[serde(default)]
    performance: ModelPerformance,
    session_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAgentUsage {
    agent: String,
    clients: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
    message_count: u32,
    #[serde(default)]
    instance_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelInfo {
    provider: String,
    #[serde(default)]
    model_id: String,
    display_name: String,
    color_key: String,
    #[serde(default)]
    workspace_key: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    tokens: CachedTokenBreakdown,
    cost: f64,
    messages: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyClientInfo {
    tokens: CachedTokenBreakdown,
    cost: f64,
    models: Vec<(String, CachedDailyModelInfo)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyUsage {
    date: String, // NaiveDate serialized as string
    tokens: CachedTokenBreakdown,
    cost: f64,
    client_breakdown: Vec<(String, CachedDailyClientInfo)>,
    message_count: u32,
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfo {
    provider: String,
    #[serde(default)]
    model_id: String,
    display_name: String,
    color_key: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyUsage {
    datetime: String, // NaiveDateTime as "YYYY-MM-DD HH:MM:SS"
    tokens: CachedTokenBreakdown,
    cost: f64,
    clients: Vec<String>,
    models: Vec<(String, CachedHourlyModelInfo)>,
    #[serde(default)]
    message_count: u32,
    #[serde(default)]
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDay {
    date: String,
    tokens: u64,
    cost: f64,
    intensity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedGraphData {
    weeks: Vec<Vec<Option<CachedContributionDay>>>,
}

// Borrowed serialization views avoid allocating an owned copy of the aggregate.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedTokenBreakdownRef {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

impl From<&TokenBreakdown> for CachedTokenBreakdownRef {
    fn from(tokens: &TokenBreakdown) -> Self {
        Self {
            input: tokens.input,
            output: tokens.output,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
            reasoning: tokens.reasoning,
        }
    }
}

struct CachedModelsRef<'a>(&'a [ModelUsage]);

impl Serialize for CachedModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedModelUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedModelUsageRef<'a> {
    model: &'a str,
    provider: &'a str,
    client: &'a str,
    workspace_key: Option<&'a str>,
    workspace_label: Option<&'a str>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    performance: &'a ModelPerformance,
    session_count: u32,
}

impl<'a> From<&'a ModelUsage> for CachedModelUsageRef<'a> {
    fn from(model: &'a ModelUsage) -> Self {
        Self {
            model: &model.model,
            provider: &model.provider,
            client: &model.client,
            workspace_key: model.workspace_key.as_deref(),
            workspace_label: model.workspace_label.as_deref(),
            tokens: (&model.tokens).into(),
            cost: model.cost,
            performance: &model.performance,
            session_count: model.session_count,
        }
    }
}

struct CachedAgentsRef<'a>(&'a [AgentUsage]);

impl Serialize for CachedAgentsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedAgentUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedAgentUsageRef<'a> {
    agent: &'a str,
    clients: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    message_count: u32,
    instance_count: u32,
}

impl<'a> From<&'a AgentUsage> for CachedAgentUsageRef<'a> {
    fn from(agent: &'a AgentUsage) -> Self {
        Self {
            agent: &agent.agent,
            clients: &agent.clients,
            tokens: (&agent.tokens).into(),
            cost: agent.cost,
            message_count: agent.message_count,
            instance_count: agent.instance_count,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelInfoRef<'a> {
    provider: &'a str,
    model_id: &'a str,
    display_name: &'a str,
    color_key: &'a str,
    workspace_key: Option<&'a str>,
    workspace_label: Option<&'a str>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    messages: u64,
}

impl<'a> From<&'a DailyModelInfo> for CachedDailyModelInfoRef<'a> {
    fn from(model: &'a DailyModelInfo) -> Self {
        Self {
            provider: &model.provider,
            model_id: &model.model_id,
            display_name: &model.display_name,
            color_key: &model.color_key,
            workspace_key: model.workspace_key.as_deref(),
            workspace_label: model.workspace_label.as_deref(),
            tokens: (&model.tokens).into(),
            cost: model.cost,
            messages: model.messages,
        }
    }
}

struct CachedDailyModelsRef<'a>(&'a BTreeMap<String, DailyModelInfo>);

impl Serialize for CachedDailyModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedDailyModelInfoRef::from(value))),
        )
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyClientInfoRef<'a> {
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    models: CachedDailyModelsRef<'a>,
}

impl<'a> From<&'a DailyClientInfo> for CachedDailyClientInfoRef<'a> {
    fn from(value: &'a DailyClientInfo) -> Self {
        Self {
            tokens: (&value.tokens).into(),
            cost: value.cost,
            models: CachedDailyModelsRef(&value.models),
        }
    }
}

struct CachedDailyClientBreakdownRef<'a>(&'a BTreeMap<String, DailyClientInfo>);

impl Serialize for CachedDailyClientBreakdownRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedDailyClientInfoRef::from(value))),
        )
    }
}

struct CachedDateRef<'a>(&'a chrono::NaiveDate);

impl Serialize for CachedDateRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self.0)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyUsageRef<'a> {
    date: CachedDateRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    client_breakdown: CachedDailyClientBreakdownRef<'a>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a DailyUsage> for CachedDailyUsageRef<'a> {
    fn from(daily: &'a DailyUsage) -> Self {
        Self {
            date: CachedDateRef(&daily.date),
            tokens: (&daily.tokens).into(),
            cost: daily.cost,
            client_breakdown: CachedDailyClientBreakdownRef(&daily.client_breakdown),
            message_count: daily.message_count,
            turn_count: daily.turn_count,
        }
    }
}

struct CachedDailyEntriesRef<'a>(&'a [DailyUsage]);

impl Serialize for CachedDailyEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedDailyUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfoRef<'a> {
    provider: &'a str,
    model_id: &'a str,
    display_name: &'a str,
    color_key: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
}

impl<'a> From<&'a HourlyModelInfo> for CachedHourlyModelInfoRef<'a> {
    fn from(model: &'a HourlyModelInfo) -> Self {
        Self {
            provider: &model.provider,
            model_id: &model.model_id,
            display_name: &model.display_name,
            color_key: &model.color_key,
            tokens: (&model.tokens).into(),
            cost: model.cost,
        }
    }
}

struct CachedHourlyModelsRef<'a>(&'a BTreeMap<String, HourlyModelInfo>);

impl Serialize for CachedHourlyModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedHourlyModelInfoRef::from(value))),
        )
    }
}

struct CachedDateTimeRef<'a>(&'a chrono::NaiveDateTime);

impl Serialize for CachedDateTimeRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&self.0.format("%Y-%m-%d %H:%M:%S"))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyUsageRef<'a> {
    datetime: CachedDateTimeRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    clients: &'a BTreeSet<String>,
    models: CachedHourlyModelsRef<'a>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a HourlyUsage> for CachedHourlyUsageRef<'a> {
    fn from(hourly: &'a HourlyUsage) -> Self {
        Self {
            datetime: CachedDateTimeRef(&hourly.datetime),
            tokens: (&hourly.tokens).into(),
            cost: hourly.cost,
            clients: &hourly.clients,
            models: CachedHourlyModelsRef(&hourly.models),
            message_count: hourly.message_count,
            turn_count: hourly.turn_count,
        }
    }
}

struct CachedHourlyEntriesRef<'a>(&'a [HourlyUsage]);

impl Serialize for CachedHourlyEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedHourlyUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDayRef<'a> {
    date: CachedDateRef<'a>,
    tokens: u64,
    cost: f64,
    intensity: f64,
}

impl<'a> From<&'a ContributionDay> for CachedContributionDayRef<'a> {
    fn from(day: &'a ContributionDay) -> Self {
        Self {
            date: CachedDateRef(&day.date),
            tokens: day.tokens,
            cost: day.cost,
            intensity: day.intensity,
        }
    }
}

struct CachedContributionWeekRef<'a>(&'a [Option<ContributionDay>]);

impl Serialize for CachedContributionWeekRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|day| day.as_ref().map(CachedContributionDayRef::from)),
        )
    }
}

struct CachedContributionWeeksRef<'a>(&'a [Vec<Option<ContributionDay>>]);

impl Serialize for CachedContributionWeeksRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(|week| CachedContributionWeekRef(week)))
    }
}

#[derive(Serialize)]
struct CachedGraphDataRef<'a> {
    weeks: CachedContributionWeeksRef<'a>,
}

impl<'a> From<&'a GraphData> for CachedGraphDataRef<'a> {
    fn from(graph: &'a GraphData) -> Self {
        Self {
            weeks: CachedContributionWeeksRef(&graph.weeks),
        }
    }
}

// Owned conversion implementations used by the cache read path.

impl From<CachedTokenBreakdown> for TokenBreakdown {
    fn from(t: CachedTokenBreakdown) -> Self {
        Self {
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            reasoning: t.reasoning,
        }
    }
}

impl From<CachedModelUsage> for ModelUsage {
    fn from(m: CachedModelUsage) -> Self {
        Self {
            model: m.model,
            provider: m.provider,
            client: m.client,
            workspace_key: m.workspace_key,
            workspace_label: m.workspace_label,
            tokens: m.tokens.into(),
            cost: m.cost,
            performance: m.performance,
            session_count: m.session_count,
        }
    }
}

impl From<CachedAgentUsage> for AgentUsage {
    fn from(a: CachedAgentUsage) -> Self {
        Self {
            agent: a.agent,
            clients: a.clients,
            tokens: a.tokens.into(),
            cost: a.cost,
            message_count: a.message_count,
            instance_count: a.instance_count,
        }
    }
}

fn daily_model_info_from_cached(value: CachedDailyModelInfo) -> DailyModelInfo {
    DailyModelInfo {
        provider: value.provider,
        model_id: value.model_id,
        display_name: value.display_name,
        color_key: value.color_key,
        workspace_key: value.workspace_key,
        workspace_label: value.workspace_label,
        tokens: value.tokens.into(),
        cost: value.cost,
        messages: value.messages,
    }
}

impl From<CachedDailyClientInfo> for DailyClientInfo {
    fn from(value: CachedDailyClientInfo) -> Self {
        Self {
            tokens: value.tokens.into(),
            cost: value.cost,
            models: value
                .models
                .into_iter()
                .map(|(key, value)| {
                    let model_info = daily_model_info_from_cached(value);
                    (key, model_info)
                })
                .collect(),
        }
    }
}

fn hourly_model_info_from_cached(value: CachedHourlyModelInfo) -> HourlyModelInfo {
    HourlyModelInfo {
        provider: value.provider,
        model_id: value.model_id,
        display_name: value.display_name,
        color_key: value.color_key,
        tokens: value.tokens.into(),
        cost: value.cost,
    }
}

impl TryFrom<CachedHourlyUsage> for HourlyUsage {
    type Error = chrono::ParseError;

    fn try_from(h: CachedHourlyUsage) -> Result<Self, Self::Error> {
        use chrono::NaiveDateTime;
        Ok(Self {
            datetime: NaiveDateTime::parse_from_str(&h.datetime, "%Y-%m-%d %H:%M:%S")?,
            tokens: h.tokens.into(),
            cost: h.cost,
            clients: h.clients.into_iter().collect(),
            models: h
                .models
                .into_iter()
                .map(|(key, value)| {
                    let model_info = hourly_model_info_from_cached(value);
                    (key, model_info)
                })
                .collect(),
            message_count: h.message_count,
            turn_count: h.turn_count,
        })
    }
}

impl TryFrom<CachedDailyUsage> for DailyUsage {
    type Error = chrono::ParseError;

    fn try_from(d: CachedDailyUsage) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;

        Ok(Self {
            date: NaiveDate::parse_from_str(&d.date, "%Y-%m-%d")?,
            tokens: d.tokens.into(),
            cost: d.cost,
            client_breakdown: d
                .client_breakdown
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            message_count: d.message_count,
            turn_count: d.turn_count,
        })
    }
}

impl TryFrom<CachedContributionDay> for ContributionDay {
    type Error = chrono::ParseError;

    fn try_from(c: CachedContributionDay) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;
        Ok(Self {
            date: NaiveDate::parse_from_str(&c.date, "%Y-%m-%d")?,
            tokens: c.tokens,
            cost: c.cost,
            intensity: c.intensity,
        })
    }
}

impl TryFrom<CachedGraphData> for GraphData {
    type Error = chrono::ParseError;

    fn try_from(g: CachedGraphData) -> Result<Self, Self::Error> {
        let weeks: Result<Vec<Vec<Option<ContributionDay>>>, _> = g
            .weeks
            .into_iter()
            .map(|week| {
                week.into_iter()
                    .map(|day| day.map(|d| d.try_into()).transpose())
                    .collect()
            })
            .collect();
        Ok(Self { weeks: weeks? })
    }
}

#[derive(Debug)]
enum CacheDataError {
    InvalidDate(chrono::ParseError),
    AgentTokenOverflow,
}

impl std::fmt::Display for CacheDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDate(err) => err.fmt(f),
            Self::AgentTokenOverflow => {
                f.write_str("cached TUI agent token buckets exceed u64::MAX")
            }
        }
    }
}

impl std::error::Error for CacheDataError {}

impl From<chrono::ParseError> for CacheDataError {
    fn from(err: chrono::ParseError) -> Self {
        Self::InvalidDate(err)
    }
}

impl TryFrom<CachedUsageData> for UsageData {
    type Error = CacheDataError;

    fn try_from(u: CachedUsageData) -> Result<Self, Self::Error> {
        let daily: Result<Vec<DailyUsage>, _> = u.daily.into_iter().map(|d| d.try_into()).collect();
        let hourly: Result<Vec<HourlyUsage>, _> =
            u.hourly.into_iter().map(|h| h.try_into()).collect();
        let graph: Option<Result<GraphData, _>> = u.graph.map(|g| g.try_into());

        Ok(Self {
            health: Default::default(),
            models: u.models.into_iter().map(|m| m.into()).collect(),
            agents: normalize_cached_agents(u.agents)?,
            daily: daily?,
            hourly: hourly?,
            graph: graph.transpose()?,
            total_tokens: u.total_tokens,
            total_cost: u.total_cost,
            error: None,
            current_streak: u.current_streak,
            longest_streak: u.longest_streak,
        })
    }
}

fn normalize_cached_agents(
    agents: Vec<CachedAgentUsage>,
) -> Result<Vec<AgentUsage>, CacheDataError> {
    let mut merged: BTreeMap<String, AgentUsage> = BTreeMap::new();
    let mut clients_by_agent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for cached in agents {
        let normalized_agent = normalize_cached_agent_name(&cached.agent, &cached.clients);
        let entry = merged
            .entry(normalized_agent.clone())
            .or_insert_with(|| AgentUsage {
                agent: normalized_agent.clone(),
                clients: String::new(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                message_count: 0,
                instance_count: 0,
            });

        let tokens: TokenBreakdown = cached.tokens.into();
        entry.tokens = entry
            .tokens
            .checked_add(&tokens)
            .ok_or(CacheDataError::AgentTokenOverflow)?;
        entry.cost += cached.cost;
        entry.message_count = entry.message_count.saturating_add(cached.message_count);
        entry.instance_count = entry.instance_count.saturating_add(cached.instance_count);

        let client_set = clients_by_agent.entry(normalized_agent).or_default();
        for client in cached
            .clients
            .split(", ")
            .filter(|client| !client.is_empty())
        {
            client_set.insert(client.to_string());
        }
    }

    let mut agents = merged.into_values().collect::<Vec<_>>();
    for agent in &mut agents {
        if let Some(clients) = clients_by_agent.get(&agent.agent) {
            agent.clients = clients.iter().cloned().collect::<Vec<_>>().join(", ");
        }
    }
    Ok(agents)
}

fn normalize_cached_agent_name(agent: &str, clients: &str) -> String {
    let has_client = |name: &str| clients.split(", ").any(|client| client == name);
    if has_client("opencode") {
        sessions::normalize_opencode_agent_name(agent)
    } else if has_client("copilot") {
        sessions::normalize_copilot_agent_name(agent)
    } else {
        sessions::normalize_agent_name(agent)
    }
}

/// Since schema 38, `modelId` is the authoritative model identity (ADR 0026),
/// but the cached field still deserializes with `#[serde(default)]`. A cache
/// written without it would load with an empty id and merge unrelated entries
/// under the empty key downstream, so treat it as a miss and rescan.
fn cached_models_missing_identity(data: &UsageData) -> bool {
    data.daily
        .iter()
        .flat_map(|day| day.client_breakdown.values())
        .flat_map(|client| client.models.values())
        .any(|model| model.model_id.is_empty())
        || data
            .hourly
            .iter()
            .flat_map(|hour| hour.models.values())
            .any(|model| model.model_id.is_empty())
}

/// Determine whether the cached client key exactly matches the current TUI request.
fn cache_clients_match_exact(
    client_universe: &HashSet<ClientId>,
    cached_clients: &[String],
) -> bool {
    let enabled: HashSet<&str> = client_universe
        .iter()
        .map(|client| client.as_str())
        .collect();
    let cached: HashSet<&str> = cached_clients.iter().map(String::as_str).collect();

    cached.len() == cached_clients.len() && enabled == cached
}

fn cache_client_space_matches_exact(
    client_universe: &HashSet<ClientId>,
    client_space: &BTreeMap<String, u64>,
) -> bool {
    client_space.len() == client_universe.len()
        && client_universe
            .iter()
            .all(|client| client_space.contains_key(client.as_str()))
}

fn cache_session_clients_are_enabled(
    client_universe: &HashSet<ClientId>,
    sessions: &[TuiSessionEntry],
) -> bool {
    sessions.iter().all(|session| {
        client_universe
            .iter()
            .any(|client| client.as_str() == session.client.as_str())
    })
}

/// A complete, generation-consistent TUI cache snapshot.
///
/// `projection_store` owns the same file handle that was used to deserialize
/// `data` and `sessions`. Keeping that handle open pins the cache inode even if
/// another process atomically replaces the canonical cache path later.
#[derive(Debug)]
pub struct LoadedTuiCache {
    pub data: UsageData,
    pub sessions: Vec<TuiSessionEntry>,
    pub client_space: BTreeMap<String, u64>,
    pub projection_store: ProjectionStore,
    pub input_inventory_signature: InputInventorySignature,
}

/// Result of loading the schema-43 TUI bundle.
pub enum CacheResult {
    Fresh(LoadedTuiCache),
    Stale(LoadedTuiCache),
    Miss,
}

/// Lazy reader for the four public TUI usage projections.
///
/// The store never reopens the path. Each projection seek starts from the
/// beginning of the pinned inode and a serde seed ignores all unrelated JSON
/// subtrees without materializing them.
pub struct ProjectionStore {
    file: File,
    health: tokscale_core::input_health::HealthReport,
    universe: HashSet<ClientId>,
    canonical: Option<TuiAcc>,
}

impl std::fmt::Debug for ProjectionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionStore")
            .field("file", &self.file)
            .field("universe", &self.universe)
            .field("canonical_loaded", &self.canonical.is_some())
            .finish_non_exhaustive()
    }
}

impl ProjectionStore {
    pub fn project(
        &mut self,
        group_by: &GroupBy,
        selected_clients: &HashSet<ClientId>,
    ) -> anyhow::Result<UsageData> {
        projection_field(group_by)?;
        if !selected_clients.is_subset(&self.universe) {
            anyhow::bail!("TUI client selection is outside the cached client universe");
        }
        if selected_clients != &self.universe {
            if self.canonical.is_none() {
                self.file.seek(SeekFrom::Start(0))?;
                let accumulator = {
                    let mut deserializer =
                        serde_json::Deserializer::from_reader(BufReader::new(&mut self.file));
                    let accumulator = CanonicalBundleSeed.deserialize(&mut deserializer)?;
                    deserializer.end()?;
                    accumulator
                };
                self.canonical = Some(accumulator);
            }
            let mut data = self
                .canonical
                .as_ref()
                .expect("canonical accumulator was loaded")
                .project_for_clients(group_by, selected_clients);
            data.health = self.health.clone();
            return Ok(data);
        }
        self.file.seek(SeekFrom::Start(0))?;

        let cached = {
            let mut deserializer =
                serde_json::Deserializer::from_reader(BufReader::new(&mut self.file));
            let cached = ProjectionBundleSeed { group_by }.deserialize(&mut deserializer)?;
            deserializer.end()?;
            cached
        };
        let mut data: UsageData = cached.try_into()?;
        data.health = self.health.clone();
        if cached_models_missing_identity(&data) {
            anyhow::bail!("cached TUI projection is missing authoritative model identity");
        }
        Ok(data)
    }

    pub fn load_snapshot(
        self,
        client_universe: &HashSet<ClientId>,
        group_by: &GroupBy,
        report_scope: &CacheReportScope,
    ) -> anyhow::Result<LoadedTuiCache> {
        let Self {
            file,
            health: _,
            universe: _,
            canonical: _,
        } = self;
        load_bundle_from_file(file, client_universe, group_by, report_scope)
            .map(|parsed| parsed.loaded)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedTuiBundleRef<'a> {
    schema_version: u32,
    timestamp: u64,
    client_universe: &'a [&'a str],
    report_scope: &'a CacheReportScope,
    input_inventory_signature: &'a InputInventorySignature,
    health: &'a tokscale_core::input_health::HealthReport,
    sessions: &'a [TuiSessionEntry],
    client_space: &'a BTreeMap<String, u64>,
    canonical_digest: &'a str,
    canonical: &'a RawValue,
    projections: CachedProjectionSetRef<'a>,
}

struct CachedProjectionSetRef<'a>(&'a TuiAcc);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedProjectionUsageDataRef<'a> {
    models: CachedModelsRef<'a>,
    agents: CachedAgentsRef<'a>,
    daily: CachedDailyEntriesRef<'a>,
    hourly: CachedHourlyEntriesRef<'a>,
    graph: Option<CachedGraphDataRef<'a>>,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

impl<'a> From<&'a UsageData> for CachedProjectionUsageDataRef<'a> {
    fn from(data: &'a UsageData) -> Self {
        Self {
            models: CachedModelsRef(&data.models),
            agents: CachedAgentsRef(&data.agents),
            daily: CachedDailyEntriesRef(&data.daily),
            hourly: CachedHourlyEntriesRef(&data.hourly),
            graph: data.graph.as_ref().map(CachedGraphDataRef::from),
            total_tokens: data.total_tokens,
            total_cost: data.total_cost,
            current_streak: data.current_streak,
            longest_streak: data.longest_streak,
        }
    }
}

impl Serialize for CachedProjectionSetRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;

        let model = self.0.project(&GroupBy::Model);
        map.serialize_entry("model", &CachedProjectionUsageDataRef::from(&model))?;
        drop(model);

        let client_model = self.0.project(&GroupBy::ClientModel);
        map.serialize_entry(
            "clientModel",
            &CachedProjectionUsageDataRef::from(&client_model),
        )?;
        drop(client_model);

        let client_provider_model = self.0.project(&GroupBy::ClientProviderModel);
        map.serialize_entry(
            "clientProviderModel",
            &CachedProjectionUsageDataRef::from(&client_provider_model),
        )?;
        drop(client_provider_model);

        let workspace_model = self.0.project(&GroupBy::WorkspaceModel);
        map.serialize_entry(
            "workspaceModel",
            &CachedProjectionUsageDataRef::from(&workspace_model),
        )?;

        map.end()
    }
}

fn projection_field(group_by: &GroupBy) -> anyhow::Result<&'static str> {
    match group_by {
        GroupBy::Model => Ok("model"),
        GroupBy::ClientModel => Ok("clientModel"),
        GroupBy::ClientProviderModel => Ok("clientProviderModel"),
        GroupBy::WorkspaceModel => Ok("workspaceModel"),
        GroupBy::Session | GroupBy::ClientSession => {
            anyhow::bail!("session groupings are not public TUI usage projections")
        }
    }
}

struct ProjectionSetSeed<'a> {
    group_by: &'a GroupBy,
}

impl<'de> DeserializeSeed<'de> for ProjectionSetSeed<'_> {
    type Value = CachedUsageData;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectionSetVisitor {
            selected_field: projection_field(self.group_by).map_err(serde::de::Error::custom)?,
        })
    }
}

struct ProjectionSetVisitor {
    selected_field: &'static str,
}

impl<'de> Visitor<'de> for ProjectionSetVisitor {
    type Value = CachedUsageData;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the four public TUI usage projections")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut selected = None;
        let mut present = 0_u8;

        while let Some(field) = map.next_key::<String>()? {
            let bit = match field.as_str() {
                "model" => 1,
                "clientModel" => 2,
                "clientProviderModel" => 4,
                "workspaceModel" => 8,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
            };
            if present & bit != 0 {
                return Err(serde::de::Error::custom(format!(
                    "duplicate TUI projection `{field}`"
                )));
            }
            present |= bit;

            if field == self.selected_field {
                selected = Some(map.next_value()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }

        if present != 0b1111 {
            return Err(serde::de::Error::custom(
                "cached TUI bundle is missing one or more public projections",
            ));
        }
        selected.ok_or_else(|| {
            serde::de::Error::custom(format!(
                "cached TUI bundle is missing projection `{}`",
                self.selected_field
            ))
        })
    }
}

struct ParsedTuiBundle {
    schema_version: u32,
    timestamp: u64,
    client_universe: Vec<String>,
    report_scope: CacheReportScope,
    input_inventory_signature: InputInventorySignature,
    health: tokscale_core::input_health::HealthReport,
    sessions: Vec<TuiSessionEntry>,
    client_space: BTreeMap<String, u64>,
    data: CachedUsageData,
}

struct FullBundleSeed<'a> {
    group_by: &'a GroupBy,
}

impl<'de> DeserializeSeed<'de> for FullBundleSeed<'_> {
    type Value = ParsedTuiBundle;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(FullBundleVisitor {
            group_by: self.group_by,
        })
    }
}

struct FullBundleVisitor<'a> {
    group_by: &'a GroupBy,
}

struct CanonicalShapeSeed;

impl<'de> DeserializeSeed<'de> for CanonicalShapeSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CanonicalShapeVisitor)
    }
}

struct CanonicalShapeVisitor;

impl<'de> Visitor<'de> for CanonicalShapeVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("canonical TUI projection state")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut present = 0_u8;
        while let Some(field) = map.next_key::<String>()? {
            let bit = match field.as_str() {
                "model_map" => 1,
                "agent_map" => 2,
                "daily_map" => 4,
                "hourly_map" => 8,
                "next_sequence" => 16,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
            };
            if present & bit != 0 {
                return Err(serde::de::Error::custom(format!(
                    "duplicate canonical TUI field `{field}`"
                )));
            }
            present |= bit;
            map.next_value::<IgnoredAny>()?;
        }

        if present != 0b1_1111 {
            return Err(serde::de::Error::custom(
                "cached TUI bundle has incomplete canonical projection state",
            ));
        }
        Ok(())
    }
}

fn validate_canonical_shape<E>(raw: &RawValue) -> Result<(), E>
where
    E: serde::de::Error,
{
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    CanonicalShapeSeed
        .deserialize(&mut deserializer)
        .map_err(E::custom)?;
    deserializer.end().map_err(E::custom)
}

impl<'de> Visitor<'de> for FullBundleVisitor<'_> {
    type Value = ParsedTuiBundle;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a schema-43 TUI cache bundle")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version: Option<u32> = None;
        let mut timestamp = None;
        let mut client_universe = None;
        let mut report_scope = None;
        let mut input_inventory_signature = None;
        let mut health = None;
        let mut sessions = None;
        let mut client_space = None;
        let mut expected_canonical_digest: Option<String> = None;
        let mut actual_canonical_digest: Option<String> = None;
        let mut data = None;

        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "schemaVersion" => {
                    set_once(&mut schema_version, map.next_value()?, "schemaVersion")?
                }
                "timestamp" => set_once(&mut timestamp, map.next_value()?, "timestamp")?,
                "clientUniverse" => {
                    set_once(&mut client_universe, map.next_value()?, "clientUniverse")?
                }
                "reportScope" => set_once(&mut report_scope, map.next_value()?, "reportScope")?,
                "inputInventorySignature" => set_once(
                    &mut input_inventory_signature,
                    map.next_value()?,
                    "inputInventorySignature",
                )?,
                "health" => set_once(&mut health, map.next_value()?, "health")?,
                "sessions" => set_once(&mut sessions, map.next_value()?, "sessions")?,
                "clientSpace" => set_once(&mut client_space, map.next_value()?, "clientSpace")?,
                "canonicalDigest" => set_once(
                    &mut expected_canonical_digest,
                    map.next_value()?,
                    "canonicalDigest",
                )?,
                "canonical" => {
                    if actual_canonical_digest.is_some() {
                        return Err(serde::de::Error::duplicate_field("canonical"));
                    }
                    let raw: Box<RawValue> = map.next_value()?;
                    validate_canonical_shape::<A::Error>(&raw)?;
                    actual_canonical_digest = Some(sha256_hex(raw.get().as_bytes()));
                }
                "projections" => {
                    if data.is_some() {
                        return Err(serde::de::Error::duplicate_field("projections"));
                    }
                    data = Some(map.next_value_seed(ProjectionSetSeed {
                        group_by: self.group_by,
                    })?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let expected_canonical_digest = required(expected_canonical_digest, "canonicalDigest")?;
        let actual_canonical_digest = required(actual_canonical_digest, "canonical")?;
        if actual_canonical_digest != expected_canonical_digest {
            return Err(serde::de::Error::custom(
                "cached TUI canonical projection digest does not match its contents",
            ));
        }
        Ok(ParsedTuiBundle {
            schema_version: required(schema_version, "schemaVersion")?,
            timestamp: required(timestamp, "timestamp")?,
            client_universe: required(client_universe, "clientUniverse")?,
            report_scope: required(report_scope, "reportScope")?,
            input_inventory_signature: required(
                input_inventory_signature,
                "inputInventorySignature",
            )?,
            health: required(health, "health")?,
            sessions: required(sessions, "sessions")?,
            client_space: required(client_space, "clientSpace")?,
            data: required(data, "projections")?,
        })
    }
}

fn set_once<T, E>(slot: &mut Option<T>, value: T, field: &'static str) -> Result<(), E>
where
    E: serde::de::Error,
{
    if slot.replace(value).is_some() {
        return Err(E::duplicate_field(field));
    }
    Ok(())
}

fn required<T, E>(value: Option<T>, field: &'static str) -> Result<T, E>
where
    E: serde::de::Error,
{
    value.ok_or_else(|| E::missing_field(field))
}

struct ProjectionBundleSeed<'a> {
    group_by: &'a GroupBy,
}

impl<'de> DeserializeSeed<'de> for ProjectionBundleSeed<'_> {
    type Value = CachedUsageData;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectionBundleVisitor {
            group_by: self.group_by,
        })
    }
}

struct ProjectionBundleVisitor<'a> {
    group_by: &'a GroupBy,
}

impl<'de> Visitor<'de> for ProjectionBundleVisitor<'_> {
    type Value = CachedUsageData;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a schema-43 TUI cache bundle")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version: Option<u32> = None;
        let mut data = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "schemaVersion" => {
                    set_once(&mut schema_version, map.next_value()?, "schemaVersion")?
                }
                "projections" => {
                    if data.is_some() {
                        return Err(serde::de::Error::duplicate_field("projections"));
                    }
                    data = Some(map.next_value_seed(ProjectionSetSeed {
                        group_by: self.group_by,
                    })?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let schema_version = required(schema_version, "schemaVersion")?;
        if schema_version != CACHE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported TUI cache schema {schema_version}"
            )));
        }
        required(data, "projections")
    }
}

struct CanonicalBundleSeed;

impl<'de> DeserializeSeed<'de> for CanonicalBundleSeed {
    type Value = TuiAcc;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CanonicalBundleVisitor)
    }
}

struct CanonicalBundleVisitor;

impl<'de> Visitor<'de> for CanonicalBundleVisitor {
    type Value = TuiAcc;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a schema-43 TUI cache bundle with canonical projection state")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version: Option<u32> = None;
        let mut canonical = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "schemaVersion" => {
                    set_once(&mut schema_version, map.next_value()?, "schemaVersion")?
                }
                "canonical" => set_once(&mut canonical, map.next_value()?, "canonical")?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let schema_version = required(schema_version, "schemaVersion")?;
        if schema_version != CACHE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported TUI cache schema {schema_version}"
            )));
        }
        required(canonical, "canonical")
    }
}

struct ParsedLoad {
    loaded: LoadedTuiCache,
    timestamp: u64,
}

fn load_bundle_from_file(
    mut file: File,
    client_universe: &HashSet<ClientId>,
    group_by: &GroupBy,
    report_scope: &CacheReportScope,
) -> anyhow::Result<ParsedLoad> {
    projection_field(group_by)?;
    file.seek(SeekFrom::Start(0))?;
    let parsed = {
        let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(&mut file));
        let parsed = FullBundleSeed { group_by }.deserialize(&mut deserializer)?;
        deserializer.end()?;
        parsed
    };

    if parsed.schema_version != CACHE_SCHEMA_VERSION {
        anyhow::bail!("unsupported TUI cache schema {}", parsed.schema_version);
    }
    if &parsed.report_scope != report_scope {
        anyhow::bail!("cached TUI report scope does not match the request");
    }
    if !cache_clients_match_exact(client_universe, &parsed.client_universe) {
        anyhow::bail!("cached TUI client universe does not match the request");
    }
    if !cache_client_space_matches_exact(client_universe, &parsed.client_space) {
        anyhow::bail!("cached TUI client-space keys do not match the client universe");
    }
    if !cache_session_clients_are_enabled(client_universe, &parsed.sessions) {
        anyhow::bail!("cached TUI Sessions contain a client outside the client universe");
    }

    let mut data: UsageData = parsed.data.try_into()?;
    data.health = parsed.health.clone();
    if cached_models_missing_identity(&data) {
        anyhow::bail!("cached TUI projection is missing authoritative model identity");
    }
    file.seek(SeekFrom::Start(0))?;

    Ok(ParsedLoad {
        loaded: LoadedTuiCache {
            data,
            sessions: parsed.sessions,
            client_space: parsed.client_space,
            projection_store: ProjectionStore {
                file,
                health: parsed.health,
                universe: client_universe.clone(),
                canonical: None,
            },
            input_inventory_signature: parsed.input_inventory_signature,
        },
        timestamp: parsed.timestamp,
    })
}

/// Load one complete TUI snapshot while materializing only the requested usage
/// projection plus Sessions. Unsupported/legacy/corrupt files are explicit
/// misses; the caller remains responsible for reporting a refresh failure.
pub fn load_cache(
    client_universe: &HashSet<ClientId>,
    group_by: &GroupBy,
    report_scope: &CacheReportScope,
) -> CacheResult {
    let cache_path = match cache_file() {
        Ok(path) => path,
        Err(_) => return CacheResult::Miss,
    };
    let file = match File::open(cache_path) {
        Ok(file) => file,
        Err(_) => return CacheResult::Miss,
    };
    let parsed = match load_bundle_from_file(file, client_universe, group_by, report_scope) {
        Ok(parsed) => parsed,
        Err(_) => return CacheResult::Miss,
    };

    if parsed.loaded.data.health.requires_input_retry() {
        return CacheResult::Stale(parsed.loaded);
    }

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => return CacheResult::Miss,
    };
    match now.checked_sub(parsed.timestamp) {
        Some(age) if age <= CACHE_STALE_THRESHOLD_MS => CacheResult::Fresh(parsed.loaded),
        _ => CacheResult::Stale(parsed.loaded),
    }
}

/// Atomically persist one complete schema-43 TUI bundle.
///
/// Projection serialization borrows the canonical accumulator and materializes
/// one grouping at a time, so the four projections never coexist in memory.
/// The returned store pins the temporary file's inode before the atomic rename,
/// so later reads cannot race with another writer replacing the canonical path.
pub fn save_tui_bundle_cache(
    accumulator: &TuiAcc,
    sessions: &[TuiSessionEntry],
    client_space: &BTreeMap<String, u64>,
    health: &tokscale_core::input_health::HealthReport,
    client_universe: &HashSet<ClientId>,
    report_scope: &CacheReportScope,
    input_inventory_signature: InputInventorySignature,
) -> anyhow::Result<ProjectionStore> {
    if !cache_client_space_matches_exact(client_universe, client_space) {
        anyhow::bail!("TUI client-space keys do not match the client universe");
    }
    if !cache_session_clients_are_enabled(client_universe, sessions) {
        anyhow::bail!("TUI Sessions contain a client outside the client universe");
    }

    let cache_path = cache_file()?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let mut clients_vec: Vec<&str> = client_universe
        .iter()
        .map(|client| client.as_str())
        .collect();
    clients_vec.sort_unstable();
    let canonical_json = serde_json::to_string(accumulator)?;
    let canonical_digest = sha256_hex(canonical_json.as_bytes());
    let canonical = RawValue::from_string(canonical_json)?;

    let cached = CachedTuiBundleRef {
        schema_version: CACHE_SCHEMA_VERSION,
        timestamp,
        client_universe: &clients_vec,
        report_scope,
        input_inventory_signature: &input_inventory_signature,
        health,
        sessions,
        client_space,
        canonical_digest: &canonical_digest,
        canonical: canonical.as_ref(),
        projections: CachedProjectionSetRef(accumulator),
    };

    let mut pinned_file = None;
    tokscale_core::fs_atomic::write_atomic_with(&cache_path, |file| {
        let mut writer = BufWriter::new(&mut *file);
        serde_json::to_writer(&mut writer, &cached).map_err(std::io::Error::other)?;
        writer.flush()?;
        drop(writer);
        pinned_file = Some(file.try_clone()?);
        Ok(())
    })
    .with_context(|| format!("failed to persist TUI cache `{}`", cache_path.display()))?;

    let mut file = pinned_file
        .ok_or_else(|| anyhow::anyhow!("atomic TUI cache writer did not retain its file handle"))?;
    file.seek(SeekFrom::Start(0))?;
    Ok(ProjectionStore {
        file,
        health: health.clone(),
        universe: client_universe.clone(),
        canonical: None,
    })
}
