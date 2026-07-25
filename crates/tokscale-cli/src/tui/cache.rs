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
use tokscale_core::usage_views::ContributionGrade;
use tokscale_core::{GroupBy, InputInventorySignature, TuiAcc, TuiSessionEntry};

use tokscale_core::ClientId;

use super::data::{
    AgentUsage, ContributionDay, DailyClientCommon, DailyModelInfo, DailyModelProjection,
    DailyUsageCommon, GraphData, HourlyModelInfo, HourlyModelProjection, HourlyUsageCommon,
    ModelUsage, TokenBreakdown, UsageCommonData, UsageData, UsageGroupedData,
};

/// Cache staleness threshold: 5 minutes (matches TS implementation)
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const CACHE_SCHEMA_VERSION: u32 = 51;

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
    use tokscale_core::{DateRange, TokenBreakdown, TuiSessionTokens, UnifiedMessage};

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
        let scope = CacheReportScope::new("/test/home".into(), None, None, None);
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

    fn health_for_client_space(
        client_space: &BTreeMap<String, u64>,
    ) -> tokscale_core::input_health::HealthReport {
        tokscale_core::input_health::HealthReport {
            input_data_bytes: client_space.values().copied().sum(),
            ..Default::default()
        }
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
        let mut actual = actual.clone();
        actual.health = Default::default();
        actual.error = None;
        let mut expected = expected.clone();
        expected.health = Default::default();
        expected.error = None;
        assert_eq!(format!("{actual:#?}"), format!("{expected:#?}"));
    }

    fn refresh_canonical_digest(value: &mut serde_json::Value) {
        let canonical = serde_json::to_vec(&value["canonical"]).unwrap();
        value["canonicalDigest"] = serde_json::Value::from(sha256_hex(&canonical));
    }

    fn same_named_cross_client_agent_accumulator() -> TuiAcc {
        let tokens = TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 2,
            cache_write: 1,
            reasoning: 3,
        };
        let messages = [
            UnifiedMessage::new_with_agent(
                "opencode",
                "gpt-5.5",
                "openai",
                "open-session",
                1_779_876_000_000,
                tokens.clone(),
                0.25,
                Some("Builder".into()),
            ),
            UnifiedMessage::new_with_agent(
                "roocode",
                "gpt-5.5",
                "openai",
                "roo-session",
                1_779_876_100_000,
                tokens,
                0.5,
                Some("Builder".into()),
            ),
        ];
        tokscale_core::build_tui_accumulator(&messages, DateRange::none())
    }

    #[test]
    #[serial]
    fn bundle_round_trips_sessions_and_metadata() {
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
        assert!(raw["common"].get("health").is_none());
        assert!(raw["common"].get("models").is_none());
        assert!(raw["projections"]["model"].get("agents").is_none());
        assert_eq!(raw["common"]["graph"], serde_json::json!({ "weeks": [] }));

        let CacheResult::Fresh(loaded) = load_cache(&clients, &GroupBy::Model, &scope) else {
            panic!("expected a fresh cache bundle");
        };
        assert_eq!(loaded.sessions, sessions);
        assert_eq!(loaded.client_space, client_space);
        assert_eq!(loaded.data.health, health);
    }

    #[test]
    #[serial]
    fn bundle_rejects_data_size_outside_client_space_invariant() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let invalid_health = tokscale_core::input_health::HealthReport::default();

        let error = save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &invalid_health,
            &clients,
            &scope,
            signature(),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("TUI Data Size 0 does not match client-space total 4096"));

        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["health"]["inputDataBytes"] = serde_json::Value::from(1_u64);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn bundle_keeps_same_named_agents_separate_across_clients() {
        let (_temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let accumulator = same_named_cross_client_agent_accumulator();
        let clients = HashSet::from([ClientId::OpenCode, ClientId::RooCode]);
        let client_space = BTreeMap::from([
            ("opencode".to_string(), 1024),
            ("roocode".to_string(), 2048),
        ]);

        save_tui_bundle_cache(
            &accumulator,
            &[],
            &client_space,
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();

        let CacheResult::Fresh(loaded) = load_cache(&clients, &GroupBy::Model, &scope) else {
            panic!("expected a fresh cache bundle");
        };
        assert_eq!(loaded.data.agents.len(), 2);
        let identities = loaded
            .data
            .agents
            .iter()
            .map(|agent| (agent.client.as_str(), agent.agent.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            identities,
            BTreeSet::from([("opencode", "Builder"), ("roocode", "Builder")])
        );

        let raw: serde_json::Value =
            serde_json::from_reader(File::open(cache_file().unwrap()).unwrap()).unwrap();
        assert!(raw["common"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent.get("client").is_some() && agent.get("clients").is_none()));
    }

    #[test]
    #[serial]
    fn missing_or_null_projection_graph_is_an_explicit_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let path = cache_file().unwrap();

        for graph in [None, Some(serde_json::Value::Null)] {
            save_tui_bundle_cache(
                &TuiAcc::new(),
                &sessions,
                &client_space,
                &health_for_client_space(&client_space),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            match graph.clone() {
                Some(graph) => value["common"]["graph"] = graph,
                None => {
                    value["common"].as_object_mut().unwrap().remove("graph");
                }
            }
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
    fn nonempty_bundle_round_trips_all_four_public_groupings() {
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
        assert!(!model_projection.graph.weeks.is_empty());
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
        let cached_common_day = &raw["common"]["daily"][0];
        assert!(cached_common_day.get("clients").is_some());
        assert!(cached_common_day.get("clientBreakdown").is_none());
        assert!(cached_common_day["clients"][0][1].get("models").is_none());
        let cached_grouped_day = &raw["projections"]["model"]["daily"][0];
        assert!(cached_grouped_day.get("clientModels").is_some());
        assert!(cached_grouped_day.get("tokens").is_none());
        let cached_common_hour = &raw["common"]["hourly"][0];
        assert!(cached_common_hour.get("models").is_none());
        let cached_grouped_hour = &raw["projections"]["model"]["hourly"][0];
        assert!(cached_grouped_hour.get("models").is_some());
        assert!(cached_grouped_hour.get("clients").is_none());
        let cached_graph_days = raw["common"]["graph"]["weeks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|week| week.as_array().unwrap())
            .filter_map(serde_json::Value::as_object)
            .collect::<Vec<_>>();
        assert!(cached_graph_days.iter().all(|day| {
            day.get("grade")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && !day.contains_key("intensity")
        }));
        assert!(cached_graph_days.iter().any(|day| day["grade"] == "empty"));
        assert!(cached_graph_days.iter().any(|day| day["grade"] != "empty"));
        assert!(raw["projections"]["model"].get("graph").is_none());
        assert!(
            !raw.to_string().contains("\"colorKey\""),
            "the cache must derive model colors from modelId"
        );

        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let expected = accumulator.project(&group_by);
            let loaded = match load_cache(&clients, &group_by, &scope) {
                CacheResult::Fresh(loaded) | CacheResult::Stale(loaded) => loaded,
                CacheResult::Miss => panic!("cache bundle must load for {group_by}"),
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
    }

    #[test]
    #[serial]
    fn bundle_rejects_common_and_grouped_shape_mismatches() {
        type Mutation = fn(&mut serde_json::Value);

        fn mismatch_daily_date(value: &mut serde_json::Value) {
            value["projections"]["model"]["daily"][0]["date"] =
                serde_json::Value::from("2026-05-28");
        }

        fn mismatch_daily_clients(value: &mut serde_json::Value) {
            value["projections"]["model"]["daily"][0]["clientModels"][0][0] =
                serde_json::Value::from("foreign-client");
        }

        fn mismatch_hourly_datetime(value: &mut serde_json::Value) {
            value["projections"]["model"]["hourly"][0]["datetime"] =
                serde_json::Value::from("2026-05-28 00:00:00");
        }

        fn unknown_contribution_grade(value: &mut serde_json::Value) {
            let day = value["common"]["graph"]["weeks"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .flat_map(|week| week.as_array_mut().unwrap())
                .find(|day| day.is_object())
                .unwrap();
            day["grade"] = serde_json::Value::from("unknown");
        }

        fn active_day_with_empty_grade(value: &mut serde_json::Value) {
            let day = value["common"]["graph"]["weeks"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .flat_map(|week| week.as_array_mut().unwrap())
                .find(|day| day["tokens"].as_u64().is_some_and(|tokens| tokens > 0))
                .expect("fixture must contain an active contribution day");
            day["grade"] = serde_json::Value::from("empty");
        }

        fn empty_day_with_active_grade(value: &mut serde_json::Value) {
            let day = value["common"]["graph"]["weeks"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .flat_map(|week| week.as_array_mut().unwrap())
                .find(|day| day["tokens"].as_u64() == Some(0))
                .expect("fixture must contain an empty contribution day");
            day["grade"] = serde_json::Value::from("peak");
        }

        let (temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let _pricing_guard = EnvVarGuard::set("TOKSCALE_PRICING_CACHE_ONLY", OsStr::new("1"));
        let accumulator = nonempty_accumulator(temp.path());
        let clients = HashSet::from([ClientId::Claude, ClientId::OpenCode]);
        let client_space =
            BTreeMap::from([("claude".to_string(), 8192), ("opencode".to_string(), 4096)]);
        let path = cache_file().unwrap();

        for (boundary, mutate) in [
            ("daily date", mismatch_daily_date as Mutation),
            ("daily client keys", mismatch_daily_clients as Mutation),
            ("hourly datetime", mismatch_hourly_datetime as Mutation),
            (
                "unknown contribution grade",
                unknown_contribution_grade as Mutation,
            ),
            (
                "active day with empty contribution grade",
                active_day_with_empty_grade as Mutation,
            ),
            (
                "empty day with active contribution grade",
                empty_day_with_active_grade as Mutation,
            ),
        ] {
            save_tui_bundle_cache(
                &accumulator,
                &[],
                &client_space,
                &health_for_client_space(&client_space),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            mutate(&mut value);
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(
                matches!(
                    load_cache(&clients, &GroupBy::Model, &scope),
                    CacheResult::Miss
                ),
                "{boundary} mismatch must invalidate the cache bundle"
            );
        }
    }

    #[test]
    #[serial]
    fn bundle_rejects_corrupt_inactive_projections_during_default_load() {
        type Mutation = fn(&mut serde_json::Value);

        fn mismatch_client_model_date(value: &mut serde_json::Value) {
            value["projections"]["clientModel"]["daily"][0]["date"] =
                serde_json::Value::from("2026-05-28");
        }

        fn remove_workspace_daily_model_id(value: &mut serde_json::Value) {
            value["projections"]["workspaceModel"]["daily"][0]["clientModels"][0][1][0][1]
                .as_object_mut()
                .unwrap()
                .remove("modelId");
        }

        let (temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let _pricing_guard = EnvVarGuard::set("TOKSCALE_PRICING_CACHE_ONLY", OsStr::new("1"));
        let accumulator = nonempty_accumulator(temp.path());
        let clients = HashSet::from([ClientId::Claude, ClientId::OpenCode]);
        let client_space =
            BTreeMap::from([("claude".to_string(), 8192), ("opencode".to_string(), 4096)]);
        let path = cache_file().unwrap();

        for (boundary, mutate) in [
            (
                "inactive daily shape",
                mismatch_client_model_date as Mutation,
            ),
            (
                "inactive authoritative model identity",
                remove_workspace_daily_model_id as Mutation,
            ),
        ] {
            save_tui_bundle_cache(
                &accumulator,
                &[],
                &client_space,
                &health_for_client_space(&client_space),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            mutate(&mut value);
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(
                matches!(
                    load_cache(&clients, &GroupBy::Model, &scope),
                    CacheResult::Miss
                ),
                "{boundary} corruption must invalidate the cache bundle at startup"
            );
        }
    }

    #[test]
    #[serial]
    fn bundle_rejects_model_clients_outside_universe_in_active_and_inactive_projections() {
        let (_temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let accumulator = same_named_cross_client_agent_accumulator();
        let clients = HashSet::from([ClientId::OpenCode, ClientId::RooCode]);
        let client_space = BTreeMap::from([
            ("opencode".to_string(), 1024),
            ("roocode".to_string(), 2048),
        ]);
        let path = cache_file().unwrap();

        for (boundary, projection) in [("active", "model"), ("inactive", "workspaceModel")] {
            save_tui_bundle_cache(
                &accumulator,
                &[],
                &client_space,
                &health_for_client_space(&client_space),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            let model = &mut value["projections"][projection]["models"][0];
            assert!(
                model["client"].as_str().unwrap().contains(", "),
                "fixture must exercise a model bucket that merges Clients"
            );
            model["client"] = serde_json::Value::from("opencode, foreign-client");
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(
                matches!(
                    load_cache(&clients, &GroupBy::Model, &scope),
                    CacheResult::Miss
                ),
                "{boundary} model Client corruption must invalidate the cache bundle"
            );
        }
    }

    #[test]
    #[serial]
    fn bundle_rejects_missing_required_projection_fields() {
        type Mutation = fn(&mut serde_json::Value);

        fn remove_model_display_name(value: &mut serde_json::Value) {
            value["projections"]["model"]["models"][0]
                .as_object_mut()
                .unwrap()
                .remove("displayName");
        }

        fn remove_agent_instance_count(value: &mut serde_json::Value) {
            value["common"]["agents"][0]
                .as_object_mut()
                .unwrap()
                .remove("instanceCount");
        }

        fn remove_hourly_message_count(value: &mut serde_json::Value) {
            value["common"]["hourly"][0]
                .as_object_mut()
                .unwrap()
                .remove("messageCount");
        }

        fn remove_hourly_turn_count(value: &mut serde_json::Value) {
            value["common"]["hourly"][0]
                .as_object_mut()
                .unwrap()
                .remove("turnCount");
        }

        fn remove_daily_model_id(value: &mut serde_json::Value) {
            value["projections"]["model"]["daily"][0]["clientModels"][0][1][0][1]
                .as_object_mut()
                .unwrap()
                .remove("modelId");
        }

        let (temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let _pricing_guard = EnvVarGuard::set("TOKSCALE_PRICING_CACHE_ONLY", OsStr::new("1"));
        let accumulator = nonempty_accumulator(temp.path());
        let clients = HashSet::from([ClientId::Claude, ClientId::OpenCode]);
        let client_space =
            BTreeMap::from([("claude".to_string(), 8192), ("opencode".to_string(), 4096)]);
        let path = cache_file().unwrap();

        for (field, mutate) in [
            ("model display name", remove_model_display_name as Mutation),
            (
                "Agent instance count",
                remove_agent_instance_count as Mutation,
            ),
            (
                "hourly message count",
                remove_hourly_message_count as Mutation,
            ),
            ("hourly turn count", remove_hourly_turn_count as Mutation),
            ("daily model id", remove_daily_model_id as Mutation),
        ] {
            save_tui_bundle_cache(
                &accumulator,
                &[],
                &client_space,
                &health_for_client_space(&client_space),
                &clients,
                &scope,
                signature(),
            )
            .unwrap();
            let mut value: serde_json::Value =
                serde_json::from_reader(File::open(&path).unwrap()).unwrap();
            mutate(&mut value);
            tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
                .unwrap();

            assert!(
                matches!(
                    load_cache(&clients, &GroupBy::Model, &scope),
                    CacheResult::Miss
                ),
                "missing {field} must invalidate the cache bundle"
            );
        }
    }

    #[test]
    #[serial]
    fn bundle_rejects_duplicate_client_agent_identity() {
        let (_temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let accumulator = same_named_cross_client_agent_accumulator();
        let clients = HashSet::from([ClientId::OpenCode, ClientId::RooCode]);
        let client_space = BTreeMap::from([
            ("opencode".to_string(), 1024),
            ("roocode".to_string(), 2048),
        ]);

        save_tui_bundle_cache(
            &accumulator,
            &[],
            &client_space,
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        let duplicate = value["common"]["agents"][0].clone();
        value["common"]["agents"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn bundle_rejects_agent_outside_client_universe() {
        let (_temp, _guard, _fixture_clients, scope, _sessions, _client_space) = fixture();
        let accumulator = same_named_cross_client_agent_accumulator();
        let clients = HashSet::from([ClientId::OpenCode, ClientId::RooCode]);
        let client_space = BTreeMap::from([
            ("opencode".to_string(), 1024),
            ("roocode".to_string(), 2048),
        ]);

        save_tui_bundle_cache(
            &accumulator,
            &[],
            &client_space,
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["common"]["agents"][0]["client"] = serde_json::Value::from("foreign-client");
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn unsupported_schema_version_is_an_explicit_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["schemaVersion"] = serde_json::Value::from(CACHE_SCHEMA_VERSION - 1);
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
    fn missing_or_incomplete_canonical_state_is_an_explicit_miss() {
        let (_temp, _guard, clients, scope, sessions, client_space) = fixture();
        let path = cache_file().unwrap();

        for replacement in [None, Some(serde_json::json!({ "model_map": [] }))] {
            save_tui_bundle_cache(
                &TuiAcc::new(),
                &sessions,
                &client_space,
                &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
            &clients,
            &scope,
            signature(),
        )
        .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        value["canonical"]
            .as_object_mut()
            .unwrap()
            .remove("usage_totals_by_client");
        refresh_canonical_digest(&mut value);
        tokscale_core::fs_atomic::write_atomic(&path, &serde_json::to_vec(&value).unwrap())
            .unwrap();

        assert!(
            matches!(
                load_cache(&clients, &GroupBy::Model, &scope),
                CacheResult::Miss
            ),
            "missing canonical Client totals must invalidate the cache bundle"
        );

        save_tui_bundle_cache(
            &TuiAcc::new(),
            &sessions,
            &client_space,
            &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
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
            &health_for_client_space(&client_space),
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
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> Self {
        Self {
            resolved_home_dir,
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
        let resolved_home_dir = match home_dir {
            Some(home_dir) => home_dir,
            None => dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
                .to_string_lossy()
                .into_owned(),
        };

        Ok(Self::new(resolved_home_dir, since, until, year))
    }
}

/// Default usage projection selected when the TUI starts. The cache stores one
/// Common part, all four Grouped parts, and canonical client-aware state, so
/// Group By and Clients are presentation state rather than cache keys.
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

/// Serializable group-agnostic projection stored once per generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageCommonData {
    agents: Vec<CachedAgentUsage>,
    daily: Vec<CachedDailyUsageCommon>,
    hourly: Vec<CachedHourlyUsageCommon>,
    graph: CachedGraphData,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

/// Serializable fields reshaped by one Group By projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageGroupedData {
    models: Vec<CachedModelUsage>,
    daily: Vec<CachedDailyModelProjection>,
    hourly: Vec<CachedHourlyModelProjection>,
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
    model_id: String,
    display_name: String,
    provider: String,
    client: String,
    #[serde(default)]
    workspace_key: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    tokens: CachedTokenBreakdown,
    cost: f64,
    session_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAgentUsage {
    agent: String,
    client: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
    message_count: u32,
    instance_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelInfo {
    provider: String,
    model_id: String,
    display_name: String,
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
struct CachedDailyClientCommon {
    tokens: CachedTokenBreakdown,
    cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyUsageCommon {
    date: String, // NaiveDate serialized as string
    tokens: CachedTokenBreakdown,
    cost: f64,
    clients: Vec<(String, CachedDailyClientCommon)>,
    message_count: u32,
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelProjection {
    date: String,
    client_models: Vec<(String, Vec<(String, CachedDailyModelInfo)>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfo {
    provider: String,
    model_id: String,
    display_name: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyUsageCommon {
    datetime: String, // NaiveDateTime as "YYYY-MM-DD HH:MM:SS"
    tokens: CachedTokenBreakdown,
    cost: f64,
    clients: Vec<String>,
    message_count: u32,
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelProjection {
    datetime: String,
    models: Vec<(String, CachedHourlyModelInfo)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDay {
    date: String,
    tokens: u64,
    cost: f64,
    grade: CachedContributionGrade,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CachedContributionGrade {
    Empty,
    Low,
    Medium,
    High,
    Peak,
}

impl From<ContributionGrade> for CachedContributionGrade {
    fn from(grade: ContributionGrade) -> Self {
        match grade {
            ContributionGrade::Empty => Self::Empty,
            ContributionGrade::Low => Self::Low,
            ContributionGrade::Medium => Self::Medium,
            ContributionGrade::High => Self::High,
            ContributionGrade::Peak => Self::Peak,
        }
    }
}

impl From<CachedContributionGrade> for ContributionGrade {
    fn from(grade: CachedContributionGrade) -> Self {
        match grade {
            CachedContributionGrade::Empty => Self::Empty,
            CachedContributionGrade::Low => Self::Low,
            CachedContributionGrade::Medium => Self::Medium,
            CachedContributionGrade::High => Self::High,
            CachedContributionGrade::Peak => Self::Peak,
        }
    }
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
    model_id: &'a str,
    display_name: &'a str,
    provider: &'a str,
    client: &'a str,
    workspace_key: Option<&'a str>,
    workspace_label: Option<&'a str>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    session_count: u32,
}

impl<'a> From<&'a ModelUsage> for CachedModelUsageRef<'a> {
    fn from(model: &'a ModelUsage) -> Self {
        Self {
            model_id: &model.model_id,
            display_name: &model.display_name,
            provider: &model.provider,
            client: &model.client,
            workspace_key: model.workspace_key.as_deref(),
            workspace_label: model.workspace_label.as_deref(),
            tokens: (&model.tokens).into(),
            cost: model.cost,
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
    client: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    message_count: u32,
    instance_count: u32,
}

impl<'a> From<&'a AgentUsage> for CachedAgentUsageRef<'a> {
    fn from(agent: &'a AgentUsage) -> Self {
        Self {
            agent: &agent.agent,
            client: &agent.client,
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
struct CachedDailyClientCommonRef {
    tokens: CachedTokenBreakdownRef,
    cost: f64,
}

impl From<&DailyClientCommon> for CachedDailyClientCommonRef {
    fn from(value: &DailyClientCommon) -> Self {
        Self {
            tokens: (&value.tokens).into(),
            cost: value.cost,
        }
    }
}

struct CachedDailyClientsRef<'a>(&'a BTreeMap<String, DailyClientCommon>);

impl Serialize for CachedDailyClientsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedDailyClientCommonRef::from(value))),
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
struct CachedDailyUsageCommonRef<'a> {
    date: CachedDateRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    clients: CachedDailyClientsRef<'a>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a DailyUsageCommon> for CachedDailyUsageCommonRef<'a> {
    fn from(daily: &'a DailyUsageCommon) -> Self {
        Self {
            date: CachedDateRef(&daily.date),
            tokens: (&daily.tokens).into(),
            cost: daily.cost,
            clients: CachedDailyClientsRef(&daily.clients),
            message_count: daily.message_count,
            turn_count: daily.turn_count,
        }
    }
}

struct CachedDailyCommonEntriesRef<'a>(&'a [DailyUsageCommon]);

impl Serialize for CachedDailyCommonEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedDailyUsageCommonRef::from))
    }
}

struct CachedDailyClientModelsRef<'a>(&'a BTreeMap<String, BTreeMap<String, DailyModelInfo>>);

impl Serialize for CachedDailyClientModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(client, models)| (client, CachedDailyModelsRef(models))),
        )
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelProjectionRef<'a> {
    date: CachedDateRef<'a>,
    client_models: CachedDailyClientModelsRef<'a>,
}

impl<'a> From<&'a DailyModelProjection> for CachedDailyModelProjectionRef<'a> {
    fn from(projection: &'a DailyModelProjection) -> Self {
        Self {
            date: CachedDateRef(&projection.date),
            client_models: CachedDailyClientModelsRef(&projection.client_models),
        }
    }
}

struct CachedDailyModelProjectionsRef<'a>(&'a [DailyModelProjection]);

impl Serialize for CachedDailyModelProjectionsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedDailyModelProjectionRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfoRef<'a> {
    provider: &'a str,
    model_id: &'a str,
    display_name: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
}

impl<'a> From<&'a HourlyModelInfo> for CachedHourlyModelInfoRef<'a> {
    fn from(model: &'a HourlyModelInfo) -> Self {
        Self {
            provider: &model.provider,
            model_id: &model.model_id,
            display_name: &model.display_name,
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
struct CachedHourlyUsageCommonRef<'a> {
    datetime: CachedDateTimeRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    clients: &'a BTreeSet<String>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a HourlyUsageCommon> for CachedHourlyUsageCommonRef<'a> {
    fn from(hourly: &'a HourlyUsageCommon) -> Self {
        Self {
            datetime: CachedDateTimeRef(&hourly.datetime),
            tokens: (&hourly.tokens).into(),
            cost: hourly.cost,
            clients: &hourly.clients,
            message_count: hourly.message_count,
            turn_count: hourly.turn_count,
        }
    }
}

struct CachedHourlyCommonEntriesRef<'a>(&'a [HourlyUsageCommon]);

impl Serialize for CachedHourlyCommonEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedHourlyUsageCommonRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelProjectionRef<'a> {
    datetime: CachedDateTimeRef<'a>,
    models: CachedHourlyModelsRef<'a>,
}

impl<'a> From<&'a HourlyModelProjection> for CachedHourlyModelProjectionRef<'a> {
    fn from(projection: &'a HourlyModelProjection) -> Self {
        Self {
            datetime: CachedDateTimeRef(&projection.datetime),
            models: CachedHourlyModelsRef(&projection.models),
        }
    }
}

struct CachedHourlyModelProjectionsRef<'a>(&'a [HourlyModelProjection]);

impl Serialize for CachedHourlyModelProjectionsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedHourlyModelProjectionRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDayRef<'a> {
    date: CachedDateRef<'a>,
    tokens: u64,
    cost: f64,
    grade: CachedContributionGrade,
}

impl<'a> From<&'a ContributionDay> for CachedContributionDayRef<'a> {
    fn from(day: &'a ContributionDay) -> Self {
        Self {
            date: CachedDateRef(&day.date),
            tokens: day.tokens,
            cost: day.cost,
            grade: day.grade.into(),
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
            model_id: m.model_id,
            display_name: m.display_name,
            provider: m.provider,
            client: m.client,
            workspace_key: m.workspace_key,
            workspace_label: m.workspace_label,
            tokens: m.tokens.into(),
            cost: m.cost,
            session_count: m.session_count,
        }
    }
}

impl From<CachedAgentUsage> for AgentUsage {
    fn from(a: CachedAgentUsage) -> Self {
        Self {
            agent: a.agent,
            client: a.client,
            tokens: a.tokens.into(),
            cost: a.cost,
            message_count: a.message_count,
            instance_count: a.instance_count,
        }
    }
}

impl From<CachedDailyClientCommon> for DailyClientCommon {
    fn from(value: CachedDailyClientCommon) -> Self {
        Self {
            tokens: value.tokens.into(),
            cost: value.cost,
        }
    }
}

fn daily_model_info_from_cached(value: CachedDailyModelInfo) -> DailyModelInfo {
    DailyModelInfo {
        provider: value.provider,
        model_id: value.model_id,
        display_name: value.display_name,
        workspace_key: value.workspace_key,
        workspace_label: value.workspace_label,
        tokens: value.tokens.into(),
        cost: value.cost,
        messages: value.messages,
    }
}

fn collect_unique_string_map<V>(
    entries: impl IntoIterator<Item = (String, V)>,
    context: &'static str,
) -> Result<BTreeMap<String, V>, CacheDataError> {
    let mut values = BTreeMap::new();
    for (key, value) in entries {
        if values.insert(key.clone(), value).is_some() {
            return Err(CacheDataError::DuplicateKey { context, key });
        }
    }
    Ok(values)
}

fn hourly_model_info_from_cached(value: CachedHourlyModelInfo) -> HourlyModelInfo {
    HourlyModelInfo {
        provider: value.provider,
        model_id: value.model_id,
        display_name: value.display_name,
        tokens: value.tokens.into(),
        cost: value.cost,
    }
}

impl TryFrom<CachedDailyUsageCommon> for DailyUsageCommon {
    type Error = CacheDataError;

    fn try_from(value: CachedDailyUsageCommon) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;
        Ok(Self {
            date: NaiveDate::parse_from_str(&value.date, "%Y-%m-%d")?,
            tokens: value.tokens.into(),
            cost: value.cost,
            clients: collect_unique_string_map(
                value
                    .clients
                    .into_iter()
                    .map(|(key, value)| (key, value.into())),
                "daily Common Client",
            )?,
            message_count: value.message_count,
            turn_count: value.turn_count,
        })
    }
}

impl TryFrom<CachedDailyModelProjection> for DailyModelProjection {
    type Error = CacheDataError;

    fn try_from(value: CachedDailyModelProjection) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;
        let client_models = value
            .client_models
            .into_iter()
            .map(|(client, models)| {
                let models = collect_unique_string_map(
                    models
                        .into_iter()
                        .map(|(key, model)| (key, daily_model_info_from_cached(model))),
                    "daily Grouped model",
                )?;
                Ok((client, models))
            })
            .collect::<Result<Vec<_>, CacheDataError>>()?;
        Ok(Self {
            date: NaiveDate::parse_from_str(&value.date, "%Y-%m-%d")?,
            client_models: collect_unique_string_map(client_models, "daily Grouped Client")?,
        })
    }
}

impl TryFrom<CachedHourlyUsageCommon> for HourlyUsageCommon {
    type Error = CacheDataError;

    fn try_from(value: CachedHourlyUsageCommon) -> Result<Self, Self::Error> {
        use chrono::NaiveDateTime;
        let mut clients = BTreeSet::new();
        for client in value.clients {
            if !clients.insert(client.clone()) {
                return Err(CacheDataError::DuplicateKey {
                    context: "hourly Common Client",
                    key: client,
                });
            }
        }
        Ok(Self {
            datetime: NaiveDateTime::parse_from_str(&value.datetime, "%Y-%m-%d %H:%M:%S")?,
            tokens: value.tokens.into(),
            cost: value.cost,
            clients,
            message_count: value.message_count,
            turn_count: value.turn_count,
        })
    }
}

impl TryFrom<CachedHourlyModelProjection> for HourlyModelProjection {
    type Error = CacheDataError;

    fn try_from(value: CachedHourlyModelProjection) -> Result<Self, Self::Error> {
        use chrono::NaiveDateTime;
        Ok(Self {
            datetime: NaiveDateTime::parse_from_str(&value.datetime, "%Y-%m-%d %H:%M:%S")?,
            models: collect_unique_string_map(
                value
                    .models
                    .into_iter()
                    .map(|(key, model)| (key, hourly_model_info_from_cached(model))),
                "hourly Grouped model",
            )?,
        })
    }
}

impl TryFrom<CachedContributionDay> for ContributionDay {
    type Error = CacheDataError;

    fn try_from(c: CachedContributionDay) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;
        let grade = ContributionGrade::from(c.grade);
        if (c.tokens == 0) != (grade == ContributionGrade::Empty) {
            return Err(CacheDataError::InvalidContributionGrade {
                tokens: c.tokens,
                grade,
            });
        }
        Ok(Self {
            date: NaiveDate::parse_from_str(&c.date, "%Y-%m-%d")?,
            tokens: c.tokens,
            cost: c.cost,
            grade,
        })
    }
}

impl TryFrom<CachedGraphData> for GraphData {
    type Error = CacheDataError;

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
    InvalidContributionGrade {
        tokens: u64,
        grade: ContributionGrade,
    },
    DuplicateKey {
        context: &'static str,
        key: String,
    },
    ProjectionShape(tokscale_core::usage_views::UsageProjectionShapeError),
}

impl std::fmt::Display for CacheDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDate(err) => err.fmt(f),
            Self::InvalidContributionGrade { tokens, grade } => write!(
                f,
                "cached TUI contribution day has incompatible tokens {tokens} and grade {grade:?}"
            ),
            Self::DuplicateKey { context, key } => {
                write!(f, "cached TUI {context} contains duplicate key `{key}`")
            }
            Self::ProjectionShape(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for CacheDataError {}

impl From<chrono::ParseError> for CacheDataError {
    fn from(err: chrono::ParseError) -> Self {
        Self::InvalidDate(err)
    }
}

impl From<tokscale_core::usage_views::UsageProjectionShapeError> for CacheDataError {
    fn from(err: tokscale_core::usage_views::UsageProjectionShapeError) -> Self {
        Self::ProjectionShape(err)
    }
}

fn reject_duplicate_daily_dates<T>(
    rows: &[T],
    date_of: impl Fn(&T) -> chrono::NaiveDate,
    context: &'static str,
) -> Result<(), CacheDataError> {
    let mut dates = BTreeSet::new();
    for row in rows {
        let date = date_of(row);
        if !dates.insert(date) {
            return Err(CacheDataError::DuplicateKey {
                context,
                key: date.to_string(),
            });
        }
    }
    Ok(())
}

fn reject_duplicate_hourly_datetimes<T>(
    rows: &[T],
    datetime_of: impl Fn(&T) -> chrono::NaiveDateTime,
    context: &'static str,
) -> Result<(), CacheDataError> {
    let mut datetimes = BTreeSet::new();
    for row in rows {
        let datetime = datetime_of(row);
        if !datetimes.insert(datetime) {
            return Err(CacheDataError::DuplicateKey {
                context,
                key: datetime.to_string(),
            });
        }
    }
    Ok(())
}

impl TryFrom<CachedUsageCommonData> for UsageCommonData {
    type Error = CacheDataError;

    fn try_from(value: CachedUsageCommonData) -> Result<Self, Self::Error> {
        let daily = value
            .daily
            .into_iter()
            .map(DailyUsageCommon::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        reject_duplicate_daily_dates(&daily, |row| row.date, "daily Common rows")?;
        let hourly = value
            .hourly
            .into_iter()
            .map(HourlyUsageCommon::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        reject_duplicate_hourly_datetimes(&hourly, |row| row.datetime, "hourly Common rows")?;
        let mut agent_identities = HashSet::with_capacity(value.agents.len());
        let mut agents = Vec::with_capacity(value.agents.len());
        for agent in value.agents {
            let identity = (agent.client.clone(), agent.agent.clone());
            if !agent_identities.insert(identity.clone()) {
                return Err(CacheDataError::DuplicateKey {
                    context: "Common Agent identity",
                    key: format!("{}/{}", identity.0, identity.1),
                });
            }
            agents.push(agent.into());
        }
        Ok(Self {
            agents,
            daily,
            hourly,
            graph: value.graph.try_into()?,
            total_tokens: value.total_tokens,
            total_cost: value.total_cost,
            current_streak: value.current_streak,
            longest_streak: value.longest_streak,
        })
    }
}

impl TryFrom<CachedUsageGroupedData> for UsageGroupedData {
    type Error = CacheDataError;

    fn try_from(value: CachedUsageGroupedData) -> Result<Self, Self::Error> {
        let daily = value
            .daily
            .into_iter()
            .map(DailyModelProjection::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        reject_duplicate_daily_dates(&daily, |row| row.date, "daily Grouped rows")?;
        let hourly = value
            .hourly
            .into_iter()
            .map(HourlyModelProjection::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        reject_duplicate_hourly_datetimes(&hourly, |row| row.datetime, "hourly Grouped rows")?;
        Ok(Self {
            models: value.models.into_iter().map(ModelUsage::from).collect(),
            daily,
            hourly,
        })
    }
}

fn usage_data_from_cached(
    common: CachedUsageCommonData,
    grouped: CachedUsageGroupedData,
) -> Result<UsageData, CacheDataError> {
    Ok(UsageData::from_projection_parts(
        common.try_into()?,
        grouped.try_into()?,
    )?)
}

/// `modelId` is the authoritative model identity (ADR 0010).
/// Reject an explicitly empty id so unrelated entries cannot merge under the
/// empty key downstream.
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

fn grouped_models_missing_identity(data: &UsageGroupedData) -> bool {
    data.daily
        .iter()
        .flat_map(|day| day.client_models.values())
        .flat_map(|models| models.values())
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

fn validate_data_size_matches_client_space(
    client_space: &BTreeMap<String, u64>,
    health: &tokscale_core::input_health::HealthReport,
) -> anyhow::Result<()> {
    let total = client_space
        .values()
        .copied()
        .try_fold(0_u64, u64::checked_add)
        .ok_or_else(|| anyhow::anyhow!("TUI client-space total exceeds u64::MAX"))?;
    if health.input_data_bytes != total {
        anyhow::bail!(
            "TUI Data Size {} does not match client-space total {total}",
            health.input_data_bytes
        );
    }
    Ok(())
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

fn cache_usage_clients_are_enabled(client_universe: &HashSet<ClientId>, data: &UsageData) -> bool {
    let enabled: HashSet<&str> = client_universe
        .iter()
        .map(|client| client.as_str())
        .collect();

    data.models
        .iter()
        .flat_map(ModelUsage::client_keys)
        .all(|client| enabled.contains(client))
        && data
            .agents
            .iter()
            .all(|agent| enabled.contains(agent.client.as_str()))
        && data.daily.iter().all(|day| {
            day.client_breakdown
                .keys()
                .all(|client| enabled.contains(client.as_str()))
        })
        && data.hourly.iter().all(|hour| {
            hour.clients
                .iter()
                .all(|client| enabled.contains(client.as_str()))
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

/// Result of loading the current TUI bundle schema.
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
        let mut data = usage_data_from_cached(cached.common, cached.grouped)?;
        data.health = self.health.clone();
        if !cache_usage_clients_are_enabled(&self.universe, &data) {
            anyhow::bail!("cached TUI usage contains a Client outside the cached universe");
        }
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
    common: CachedCommonProjectionRef<'a>,
    projections: CachedProjectionSetRef<'a>,
}

struct CachedCommonProjectionRef<'a>(&'a TuiAcc);
struct CachedProjectionSetRef<'a>(&'a TuiAcc);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageCommonDataRef<'a> {
    agents: CachedAgentsRef<'a>,
    daily: CachedDailyCommonEntriesRef<'a>,
    hourly: CachedHourlyCommonEntriesRef<'a>,
    graph: CachedGraphDataRef<'a>,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

impl<'a> From<&'a UsageCommonData> for CachedUsageCommonDataRef<'a> {
    fn from(data: &'a UsageCommonData) -> Self {
        Self {
            agents: CachedAgentsRef(&data.agents),
            daily: CachedDailyCommonEntriesRef(&data.daily),
            hourly: CachedHourlyCommonEntriesRef(&data.hourly),
            graph: CachedGraphDataRef::from(&data.graph),
            total_tokens: data.total_tokens,
            total_cost: data.total_cost,
            current_streak: data.current_streak,
            longest_streak: data.longest_streak,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageGroupedDataRef<'a> {
    models: CachedModelsRef<'a>,
    daily: CachedDailyModelProjectionsRef<'a>,
    hourly: CachedHourlyModelProjectionsRef<'a>,
}

impl<'a> From<&'a UsageGroupedData> for CachedUsageGroupedDataRef<'a> {
    fn from(data: &'a UsageGroupedData) -> Self {
        Self {
            models: CachedModelsRef(&data.models),
            daily: CachedDailyModelProjectionsRef(&data.daily),
            hourly: CachedHourlyModelProjectionsRef(&data.hourly),
        }
    }
}

impl Serialize for CachedCommonProjectionRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let common = self.0.project_common();
        CachedUsageCommonDataRef::from(&common).serialize(serializer)
    }
}

impl Serialize for CachedProjectionSetRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;

        let model = self.0.project_grouped(&GroupBy::Model);
        map.serialize_entry("model", &CachedUsageGroupedDataRef::from(&model))?;
        drop(model);

        let client_model = self.0.project_grouped(&GroupBy::ClientModel);
        map.serialize_entry(
            "clientModel",
            &CachedUsageGroupedDataRef::from(&client_model),
        )?;
        drop(client_model);

        let client_provider_model = self.0.project_grouped(&GroupBy::ClientProviderModel);
        map.serialize_entry(
            "clientProviderModel",
            &CachedUsageGroupedDataRef::from(&client_provider_model),
        )?;
        drop(client_provider_model);

        let workspace_model = self.0.project_grouped(&GroupBy::WorkspaceModel);
        map.serialize_entry(
            "workspaceModel",
            &CachedUsageGroupedDataRef::from(&workspace_model),
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
    }
}

fn projection_descriptor(field: &str) -> Option<(&'static str, u8)> {
    match field {
        "model" => Some(("model", 1)),
        "clientModel" => Some(("clientModel", 2)),
        "clientProviderModel" => Some(("clientProviderModel", 4)),
        "workspaceModel" => Some(("workspaceModel", 8)),
        _ => None,
    }
}

struct SelectedProjectionSetSeed<'a> {
    group_by: &'a GroupBy,
}

impl<'de> DeserializeSeed<'de> for SelectedProjectionSetSeed<'_> {
    type Value = CachedUsageGroupedData;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SelectedProjectionSetVisitor {
            selected_field: projection_field(self.group_by).map_err(serde::de::Error::custom)?,
        })
    }
}

struct SelectedProjectionSetVisitor {
    selected_field: &'static str,
}

impl<'de> Visitor<'de> for SelectedProjectionSetVisitor {
    type Value = CachedUsageGroupedData;

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
            let Some((field, bit)) = projection_descriptor(&field) else {
                map.next_value::<IgnoredAny>()?;
                continue;
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

/// Retain only the fields inspected by `UsageData::validate_projection_parts`.
/// This lets startup validate all four projections without retaining four
/// complete model payloads at once.
fn grouped_projection_shape_skeleton(data: &UsageGroupedData) -> UsageGroupedData {
    UsageGroupedData {
        models: Vec::new(),
        daily: data
            .daily
            .iter()
            .map(|day| DailyModelProjection {
                date: day.date,
                client_models: day
                    .client_models
                    .keys()
                    .map(|client| (client.clone(), BTreeMap::new()))
                    .collect(),
            })
            .collect(),
        hourly: data
            .hourly
            .iter()
            .map(|hour| HourlyModelProjection {
                datetime: hour.datetime,
                models: BTreeMap::new(),
            })
            .collect(),
    }
}

struct ValidatedProjectionSet {
    selected: UsageGroupedData,
    validation_shapes: Vec<ValidatedProjectionShape>,
}

struct ValidatedProjectionShape {
    field: &'static str,
    shape: UsageGroupedData,
    model_clients: BTreeSet<String>,
}

struct ValidatedProjectionSetSeed<'a> {
    group_by: &'a GroupBy,
}

impl<'de> DeserializeSeed<'de> for ValidatedProjectionSetSeed<'_> {
    type Value = ValidatedProjectionSet;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ValidatedProjectionSetVisitor {
            selected_field: projection_field(self.group_by).map_err(serde::de::Error::custom)?,
        })
    }
}

struct ValidatedProjectionSetVisitor {
    selected_field: &'static str,
}

impl<'de> Visitor<'de> for ValidatedProjectionSetVisitor {
    type Value = ValidatedProjectionSet;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the four valid public TUI usage projections")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut selected = None;
        let mut validation_shapes = Vec::with_capacity(4);
        let mut present = 0_u8;

        while let Some(field) = map.next_key::<String>()? {
            let Some((field, bit)) = projection_descriptor(&field) else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            if present & bit != 0 {
                return Err(serde::de::Error::custom(format!(
                    "duplicate TUI projection `{field}`"
                )));
            }
            present |= bit;

            let cached: CachedUsageGroupedData = map.next_value()?;
            let grouped: UsageGroupedData = cached.try_into().map_err(serde::de::Error::custom)?;
            if grouped_models_missing_identity(&grouped) {
                return Err(serde::de::Error::custom(format!(
                    "cached TUI projection `{field}` is missing authoritative model identity"
                )));
            }
            let model_clients = grouped
                .models
                .iter()
                .flat_map(ModelUsage::client_keys)
                .map(str::to_owned)
                .collect();
            validation_shapes.push(ValidatedProjectionShape {
                field,
                shape: grouped_projection_shape_skeleton(&grouped),
                model_clients,
            });
            if field == self.selected_field {
                selected = Some(grouped);
            }
        }

        if present != 0b1111 {
            return Err(serde::de::Error::custom(
                "cached TUI bundle is missing one or more public projections",
            ));
        }
        Ok(ValidatedProjectionSet {
            selected: selected.ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "cached TUI bundle is missing projection `{}`",
                    self.selected_field
                ))
            })?,
            validation_shapes,
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
    common: UsageCommonData,
    grouped: UsageGroupedData,
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
                "usage_totals_by_client" => 1,
                "model_map" => 2,
                "agent_map" => 4,
                "daily_map" => 8,
                "hourly_map" => 16,
                "next_sequence" => 32,
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

        if present != 0b11_1111 {
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
        formatter.write_str("a current-schema TUI cache bundle")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version: Option<u32> = None;
        let mut timestamp = None;
        let mut client_universe: Option<Vec<String>> = None;
        let mut report_scope = None;
        let mut input_inventory_signature = None;
        let mut health = None;
        let mut sessions = None;
        let mut client_space = None;
        let mut expected_canonical_digest: Option<String> = None;
        let mut actual_canonical_digest: Option<String> = None;
        let mut common: Option<CachedUsageCommonData> = None;
        let mut grouped: Option<ValidatedProjectionSet> = None;

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
                "common" => set_once(&mut common, map.next_value()?, "common")?,
                "projections" => {
                    if grouped.is_some() {
                        return Err(serde::de::Error::duplicate_field("projections"));
                    }
                    grouped = Some(map.next_value_seed(ValidatedProjectionSetSeed {
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
        let common: UsageCommonData = required(common, "common")?
            .try_into()
            .map_err(serde::de::Error::custom)?;
        let grouped = required(grouped, "projections")?;
        let cached_client_universe = required(client_universe, "clientUniverse")?;
        let enabled_clients: HashSet<&str> =
            cached_client_universe.iter().map(String::as_str).collect();
        for validation in &grouped.validation_shapes {
            UsageData::validate_projection_parts(&common, &validation.shape).map_err(|error| {
                serde::de::Error::custom(format!(
                    "cached TUI projection `{}` does not match Common: {error}",
                    validation.field
                ))
            })?;
            if !validation
                .model_clients
                .iter()
                .all(|client| enabled_clients.contains(client.as_str()))
            {
                return Err(serde::de::Error::custom(format!(
                    "cached TUI projection `{}` contains a model Client outside the cached universe",
                    validation.field
                )));
            }
        }
        Ok(ParsedTuiBundle {
            schema_version: required(schema_version, "schemaVersion")?,
            timestamp: required(timestamp, "timestamp")?,
            client_universe: cached_client_universe,
            report_scope: required(report_scope, "reportScope")?,
            input_inventory_signature: required(
                input_inventory_signature,
                "inputInventorySignature",
            )?,
            health: required(health, "health")?,
            sessions: required(sessions, "sessions")?,
            client_space: required(client_space, "clientSpace")?,
            common,
            grouped: grouped.selected,
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

struct CachedProjectionParts {
    common: CachedUsageCommonData,
    grouped: CachedUsageGroupedData,
}

impl<'de> DeserializeSeed<'de> for ProjectionBundleSeed<'_> {
    type Value = CachedProjectionParts;

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
    type Value = CachedProjectionParts;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a current-schema TUI cache bundle")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version: Option<u32> = None;
        let mut common = None;
        let mut grouped = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "schemaVersion" => {
                    set_once(&mut schema_version, map.next_value()?, "schemaVersion")?
                }
                "common" => set_once(&mut common, map.next_value()?, "common")?,
                "projections" => {
                    if grouped.is_some() {
                        return Err(serde::de::Error::duplicate_field("projections"));
                    }
                    grouped = Some(map.next_value_seed(SelectedProjectionSetSeed {
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
        Ok(CachedProjectionParts {
            common: required(common, "common")?,
            grouped: required(grouped, "projections")?,
        })
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
        formatter.write_str("a current-schema TUI cache bundle with canonical projection state")
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
    validate_data_size_matches_client_space(&parsed.client_space, &parsed.health)?;
    if !cache_session_clients_are_enabled(client_universe, &parsed.sessions) {
        anyhow::bail!("cached TUI Sessions contain a client outside the client universe");
    }

    let mut data = UsageData::from_projection_parts(parsed.common, parsed.grouped)?;
    data.health = parsed.health.clone();
    if !cache_usage_clients_are_enabled(client_universe, &data) {
        anyhow::bail!("cached TUI usage contains a Client outside the client universe");
    }
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
/// projection plus Sessions. Unsupported or corrupt files are explicit
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

/// Atomically persist one complete current-schema TUI bundle.
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
    validate_data_size_matches_client_space(client_space, health)?;
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
        common: CachedCommonProjectionRef(accumulator),
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
