pub(crate) mod decode;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderRoute, DecoderSpec, DiscoveryContext,
    FingerprintPolicy, FoldContext, InputDiscoveryError, InputUnit, ParseContext, ParsedUnit,
    SourceSpec,
};
#[cfg(test)]
use crate::message_cache::{DecoderId, DecoderVersion};

const KIRO_RECORD_REJECTION_REVISION: u32 = 5;
const SOURCE: SourceSpec = SourceSpec::home(".kiro/sessions/cli", "*.json");

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Kiro
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let mut units = adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, [SOURCE.resolve(ctx.home_dir)], SOURCE.pattern())?,
            FingerprintPolicy::PlainFile,
            DecoderSpec::kiro_file(KIRO_RECORD_REJECTION_REVISION),
        )?
        .into_iter()
        .map(configure_kiro_file_unit)
        .collect::<Vec<_>>();

        if let Some(db_path) = kiro_db_path(client, ctx.home_dir)? {
            units.push(InputUnit::sqlite_with_wal(
                db_path,
                DecoderSpec::kiro_sqlite(KIRO_RECORD_REJECTION_REVISION),
            ));
        }

        units.extend(adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(
                client,
                kiro_global_storage_roots(ctx.home_dir),
                "kiro-globalstorage",
            )?,
            FingerprintPolicy::PlainFile,
            DecoderSpec::kiro_global_storage(KIRO_RECORD_REJECTION_REVISION),
        )?);

        units.extend(kiro_extra_units(client, ctx)?);
        dedup_units_by_canonical_path(client, &mut units)?;
        units.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder.route() {
                DecoderRoute::KiroFile => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, decode::parse_kiro_file)
                }
                DecoderRoute::KiroSqlite => {
                    adapter_cache::parse_uncached_unit(unit, ctx, decode::parse_kiro_sqlite)
                }
                DecoderRoute::KiroGlobalStorage => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, decode::parse_kiro_file)
                }
                DecoderRoute::None
                | DecoderRoute::AntigravityCliSqlite
                | DecoderRoute::OpenCodeSqlite
                | DecoderRoute::CodeBuddyJsonl
                | DecoderRoute::CodeBuddyExtensionLog { .. }
                | DecoderRoute::Codex => unreachable!("unexpected Kiro input unit meta"),
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        match unit.decoder.route() {
            DecoderRoute::KiroFile | DecoderRoute::KiroGlobalStorage => {
                adapter_cache::plan_cache_hit(unit, input_cache)
            }
            DecoderRoute::KiroSqlite => Ok(crate::integrations::CacheHitPlan::Miss(unit)),
            _ => unreachable!("unexpected Kiro input unit meta"),
        }
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

fn kiro_db_path(client: ClientId, home_dir: &Path) -> Result<Option<PathBuf>, InputDiscoveryError> {
    let xdg_path = home_dir.join(".local/share/kiro-cli/data.sqlite3");
    let mut paths = Vec::new();
    adapter_discover::push_existing_file(client, xdg_path, &mut paths)?;
    if let Some(path) = paths.pop() {
        return Ok(Some(path));
    }

    let macos_path = home_dir.join("Library/Application Support/kiro-cli/data.sqlite3");
    adapter_discover::push_existing_file(client, macos_path, &mut paths)?;
    Ok(paths.pop())
}

fn kiro_global_storage_roots(home_dir: &Path) -> Vec<PathBuf> {
    vec![
        home_dir.join("Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent"),
        home_dir.join("Library/Application Support/kiro/User/globalStorage/kiro.kiroagent"),
        home_dir.join(".config/Kiro/User/globalStorage/kiro.kiroagent"),
        home_dir.join(".config/kiro/User/globalStorage/kiro.kiroagent"),
        home_dir.join("AppData/Roaming/Kiro/User/globalStorage/kiro.kiroagent"),
        home_dir.join("AppData/Roaming/kiro/User/globalStorage/kiro.kiroagent"),
    ]
}

fn configure_kiro_file_unit(unit: InputUnit) -> InputUnit {
    let sidecar = unit.path.with_extension("jsonl");
    unit.with_optional_dependency(sidecar)
}

fn kiro_extra_units(
    client: ClientId,
    ctx: &DiscoveryContext<'_>,
) -> Result<Vec<InputUnit>, InputDiscoveryError> {
    let mut cli_paths = Vec::new();
    let mut sqlite_paths = Vec::new();
    let mut global_storage_paths = Vec::new();

    for root in adapter_discover::extra_roots_for_client(client, ctx)? {
        match std::fs::metadata(&root) {
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(InputDiscoveryError::new(
                    &root,
                    "read extra scan root metadata",
                    source,
                ));
            }
        }

        for entry in WalkDir::new(&root) {
            let entry = entry.map_err(|source| {
                InputDiscoveryError::new(&root, "walk extra scan root", source)
            })?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            if is_kiro_global_storage_input(&path) {
                global_storage_paths.push(path);
            } else if path.file_name().is_some_and(|name| name == "data.sqlite3") {
                sqlite_paths.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                cli_paths.push(path);
            }
        }
    }

    let mut units = adapter_discover::input_units_from_paths(
        client,
        cli_paths,
        FingerprintPolicy::PlainFile,
        DecoderSpec::kiro_file(KIRO_RECORD_REJECTION_REVISION),
    )?
    .into_iter()
    .map(configure_kiro_file_unit)
    .collect::<Vec<_>>();
    units.extend(adapter_discover::input_units_from_paths(
        client,
        sqlite_paths,
        FingerprintPolicy::SqliteWithWal,
        DecoderSpec::kiro_sqlite(KIRO_RECORD_REJECTION_REVISION),
    )?);
    units.extend(adapter_discover::input_units_from_paths(
        client,
        global_storage_paths,
        FingerprintPolicy::PlainFile,
        DecoderSpec::kiro_global_storage(KIRO_RECORD_REJECTION_REVISION),
    )?);
    Ok(units)
}

fn is_kiro_global_storage_input(path: &Path) -> bool {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    let has_storage_layout = components.windows(2).any(|pair| {
        pair[0].eq_ignore_ascii_case("globalStorage")
            && pair[1].eq_ignore_ascii_case("kiro.kiroagent")
    });
    if !has_storage_layout {
        return false;
    }

    path.extension().is_none()
        || path
            .extension()
            .is_some_and(|extension| extension == "chat" || extension == "json")
}

fn dedup_units_by_canonical_path(
    _client: ClientId,
    units: &mut Vec<InputUnit>,
) -> Result<(), InputDiscoveryError> {
    let mut seen = HashSet::new();
    let mut keys = Vec::with_capacity(units.len());
    for unit in units.iter() {
        keys.push(std::fs::canonicalize(&unit.path).map_err(|source| {
            InputDiscoveryError::new(&unit.path, "canonicalize discovered input", source)
        })?);
    }
    let mut index = 0;
    units.retain(|_| {
        let keep = seen.insert(keys[index].clone());
        index += 1;
        keep
    });
    Ok(())
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;

    fn kiro_global_unit(path: PathBuf) -> InputUnit {
        InputUnit::plain_file(
            path,
            DecoderSpec::kiro_global_storage(KIRO_RECORD_REJECTION_REVISION),
        )
    }

    fn kiro_file_unit(path: PathBuf) -> InputUnit {
        let sidecar = path.with_extension("jsonl");
        InputUnit::plain_file(path, DecoderSpec::kiro_file(KIRO_RECORD_REJECTION_REVISION))
            .with_optional_dependency(sidecar)
    }

    #[test]
    fn kiro_adapter_discovers_file_sqlite_and_global_storage_inputs() {
        let home = tempfile::TempDir::new().unwrap();
        let file_path = home.path().join(".kiro/sessions/cli/session.json");
        let db_path = home.path().join(".local/share/kiro-cli/data.sqlite3");
        let global_path = home.path().join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/execution.chat",
        );
        for path in [&file_path, &db_path, &global_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 3);
        assert!(units
            .iter()
            .any(|unit| unit.path == file_path
                && matches!(unit.decoder.route(), DecoderRoute::KiroFile)));
        assert!(units
            .iter()
            .any(|unit| unit.path == db_path
                && matches!(unit.decoder.route(), DecoderRoute::KiroSqlite)));
        assert!(units.iter().any(|unit| {
            unit.path == global_path
                && matches!(unit.decoder.route(), DecoderRoute::KiroGlobalStorage)
        }));
        let file_unit = units
            .iter()
            .find(|unit| matches!(unit.decoder.route(), DecoderRoute::KiroFile))
            .unwrap();
        assert_eq!(
            file_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: file_path.with_extension("jsonl"),
                related_failure_policy:
                    crate::message_cache::RelatedInputFailurePolicy::PreservePrimary,
            }
        );
        let global_unit = units
            .iter()
            .find(|unit| matches!(unit.decoder.route(), DecoderRoute::KiroGlobalStorage))
            .unwrap();
        assert_eq!(global_unit.fingerprint_policy, FingerprintPolicy::PlainFile);
        for unit in &units {
            let decoder_id = match unit.decoder.route() {
                DecoderRoute::KiroFile => DecoderId::KiroFile,
                DecoderRoute::KiroSqlite => DecoderId::KiroSqlite,
                DecoderRoute::KiroGlobalStorage => DecoderId::KiroGlobalStorage,
                _ => unreachable!(),
            };
            assert_eq!(
                unit.decoder.version(),
                DecoderVersion::new(decoder_id, KIRO_RECORD_REJECTION_REVISION)
            );
        }
    }

    #[test]
    fn kiro_extra_root_discovers_each_current_input_layout() {
        let home = tempfile::TempDir::new().unwrap();
        let extra_root = home.path().join("external-profile");
        let cli_path = extra_root.join(".kiro/sessions/cli/session.json");
        let cli_sidecar = cli_path.with_extension("jsonl");
        let sqlite_path = extra_root.join(".local/share/kiro-cli/data.sqlite3");
        let global_path = extra_root.join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/session.json",
        );
        for path in [&cli_path, &cli_sidecar, &sqlite_path, &global_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("kiro".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();
        assert_eq!(units.len(), 3);

        let cli_unit = units.iter().find(|unit| unit.path == cli_path).unwrap();
        assert_eq!(cli_unit.decoder.route(), DecoderRoute::KiroFile);
        assert_eq!(
            cli_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: cli_sidecar,
                related_failure_policy:
                    crate::message_cache::RelatedInputFailurePolicy::PreservePrimary,
            }
        );
        assert_eq!(
            cli_unit.decoder.version(),
            DecoderVersion::new(DecoderId::KiroFile, KIRO_RECORD_REJECTION_REVISION)
        );

        let sqlite_unit = units.iter().find(|unit| unit.path == sqlite_path).unwrap();
        assert_eq!(sqlite_unit.decoder.route(), DecoderRoute::KiroSqlite);
        assert_eq!(
            sqlite_unit.fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
        assert_eq!(
            sqlite_unit.decoder.version(),
            DecoderVersion::new(DecoderId::KiroSqlite, KIRO_RECORD_REJECTION_REVISION)
        );

        let global_unit = units.iter().find(|unit| unit.path == global_path).unwrap();
        assert_eq!(global_unit.decoder.route(), DecoderRoute::KiroGlobalStorage);
        assert_eq!(
            global_unit.decoder.version(),
            DecoderVersion::new(DecoderId::KiroGlobalStorage, KIRO_RECORD_REJECTION_REVISION)
        );
    }

    #[test]
    fn kiro_global_storage_keeps_good_inputs_around_a_bad_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("globalStorage/kiro.kiroagent/workspace-a");
        std::fs::create_dir_all(&root).unwrap();
        let good_payload = |session: &str, timestamp: i64| {
            serde_json::json!({
                "session_id": session,
                "model": "claude-sonnet-4-5",
                "timestamp": timestamp,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string()
        };
        let paths = [
            root.join("01-good.chat"),
            root.join("02-bad.chat"),
            root.join("03-good.chat"),
        ];
        std::fs::write(&paths[0], good_payload("good-1", 1_770_000_000_000)).unwrap();
        std::fs::write(&paths[1], "not json").unwrap();
        std::fs::write(&paths[2], good_payload("good-2", 1_770_000_002_000)).unwrap();
        let units = paths.into_iter().map(kiro_global_unit).collect();
        let parsed = INTEGRATION.parse_checked(units, &ParseContext { pricing: None });
        let mut cache = crate::message_cache::InputMessageCache::default();
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Kiro);
        let mut ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundMessageSink::new(binding, &mut messages);

        INTEGRATION.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Kiro));
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(ctx.health().failed_inputs(), 0);
        assert_eq!(ctx.health().partial_inputs(), 0);
    }

    #[test]
    fn malformed_kiro_cli_header_is_unavailable_at_the_adapter_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "not json").unwrap();

        let parsed =
            INTEGRATION.parse_checked(vec![kiro_file_unit(path)], &ParseContext { pricing: None });

        let health = &parsed[0].health;
        let failure = health.status.failure().unwrap();
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Unavailable { .. }
        ));
        assert_eq!(failure.operation, "decode Kiro session header");
        assert!(health.rejections.is_empty());
    }

    #[test]
    fn unreadable_kiro_cli_sidecar_is_partial_and_is_not_cached() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        let sidecar = path.with_extension("jsonl");
        std::fs::write(
            &path,
            r#"{
                "session_id":"session-sidecar-read",
                "session_state":{
                    "rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},
                    "conversation_metadata":{"user_turn_metadatas":[
                        {"input_token_count":13,"output_token_count":5,"end_timestamp":1770983427}
                    ]}
                }
            }"#,
        )
        .unwrap();
        std::fs::create_dir(&sidecar).unwrap();
        let unit = kiro_file_unit(path.clone());
        let decoder_version = unit.decoder.version();

        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });

        let health = &parsed[0].health;
        let failure = health.status.failure().unwrap();
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert!(matches!(
            failure.operation.as_str(),
            "open Kiro JSONL sidecar" | "read Kiro JSONL sidecar line"
        ));
        assert!(failure.message.contains(&sidecar.display().to_string()));
        assert!(parsed[0].cache_write.is_none());

        let mut cache = crate::message_cache::InputMessageCache::default();
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Kiro);
        let mut ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundMessageSink::new(binding, &mut messages);
        INTEGRATION.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::Kiro);
        assert_eq!(messages[0].tokens.input, 13);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(ctx.health().partial_inputs(), 1);
        assert!(cache.get_meta(&path, decoder_version).unwrap().is_none());
    }

    #[test]
    fn kiro_cli_cache_fingerprint_tracks_the_sidecar() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        let sidecar = path.with_extension("jsonl");
        std::fs::write(&path, r#"{"session_id":"session-1"}"#).unwrap();
        std::fs::write(&sidecar, "old sidecar").unwrap();
        let unit = kiro_file_unit(path.clone());
        let mut cache = crate::message_cache::InputMessageCache::default();
        cache.insert(crate::message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![crate::records::ParsedMessage::new(
                "cached-model",
                "cached-provider",
                "cached-session",
                1,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));
        std::fs::write(&sidecar, "new sidecar only").unwrap();

        let planned = INTEGRATION.plan_cache_hit(unit, &cache).unwrap();

        assert!(matches!(
            planned,
            crate::integrations::CacheHitPlan::Miss(_)
        ));
    }
}
