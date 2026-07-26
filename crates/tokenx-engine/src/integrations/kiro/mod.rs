pub(crate) mod decode;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::clients::ClientId;
#[cfg(test)]
use crate::input_record_cache::{DecoderId, DecoderVersion};
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".kiro/sessions/cli",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::json),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut units = source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, [SOURCE.resolve(ctx.home_dir)], SOURCE.matcher())?,
            FingerprintPolicy::PlainFile,
            DecoderKind::kiro_file(),
        )?
        .into_iter()
        .map(configure_kiro_file_unit)
        .collect::<Vec<_>>();

        if let Some(db_path) = kiro_db_path(client, ctx.home_dir)? {
            units.push(DiscoveredInput::sqlite_with_wal(
                db_path,
                DecoderKind::kiro_sqlite(),
            ));
        }

        units.extend(source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(
                ctx,
                kiro_global_storage_roots(ctx.home_dir),
                crate::integrations::SourceMatcher::new(
                    crate::integrations::source_matchers::kiro_global_storage,
                ),
            )?,
            FingerprintPolicy::PlainFile,
            DecoderKind::kiro_global_storage(),
        )?);

        units.extend(kiro_extra_units(client, ctx)?);
        dedup_units_by_canonical_path(client, &mut units)?;
        units.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder {
                DecoderKind::KiroFile => {
                    pipeline_cache::load_or_scan_unit_with(unit, ctx, decode::parse_kiro_file)
                }
                DecoderKind::KiroSqlite => {
                    pipeline_cache::parse_uncached_unit(unit, ctx, decode::parse_kiro_sqlite)
                }
                DecoderKind::KiroGlobalStorage => {
                    pipeline_cache::load_or_scan_unit_with(unit, ctx, decode::parse_kiro_file)
                }
                _ => unreachable!("unexpected Kiro decoder"),
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        match unit.decoder {
            DecoderKind::KiroFile | DecoderKind::KiroGlobalStorage => {
                pipeline_cache::plan_cache_hit(unit, input_cache)
            }
            DecoderKind::KiroSqlite => Ok(crate::integrations::CacheHitPlan::Miss(
                unit.into_bypass_execution(),
            )),
            _ => unreachable!("unexpected Kiro decoder"),
        }
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

fn kiro_db_path(client: ClientId, home_dir: &Path) -> Result<Option<PathBuf>, InputDiscoveryError> {
    let mut paths = Vec::new();
    for path in kiro_db_candidates(home_dir) {
        source_discovery::push_existing_file(client, path, &mut paths)?;
    }
    Ok(paths.pop())
}

fn kiro_db_candidates(home_dir: &Path) -> Vec<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        vec![home_dir.join(".local/share/kiro-cli/data.sqlite3")]
    }

    #[cfg(target_os = "macos")]
    {
        vec![home_dir.join("Library/Application Support/kiro-cli/data.sqlite3")]
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
    {
        let _ = home_dir;
        Vec::new()
    }
}

fn kiro_global_storage_roots(home_dir: &Path) -> Vec<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        vec![
            home_dir.join(".config/Kiro/User/globalStorage/kiro.kiroagent"),
            home_dir.join(".config/kiro/User/globalStorage/kiro.kiroagent"),
        ]
    }

    #[cfg(target_os = "macos")]
    {
        vec![
            home_dir.join("Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent"),
            home_dir.join("Library/Application Support/kiro/User/globalStorage/kiro.kiroagent"),
        ]
    }

    #[cfg(target_os = "windows")]
    {
        vec![
            home_dir.join("AppData/Roaming/Kiro/User/globalStorage/kiro.kiroagent"),
            home_dir.join("AppData/Roaming/kiro/User/globalStorage/kiro.kiroagent"),
        ]
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        let _ = home_dir;
        Vec::new()
    }
}

fn configure_kiro_file_unit(unit: DiscoveredInput) -> DiscoveredInput {
    let sidecar = unit.path.with_extension("jsonl");
    unit.with_optional_dependency(sidecar)
}

fn kiro_extra_units(
    client: ClientId,
    ctx: &DiscoveryContext<'_>,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let mut cli_paths = Vec::new();
    let mut sqlite_paths = Vec::new();
    let mut global_storage_paths = Vec::new();

    for root in source_discovery::extra_roots_for_client(client, ctx)? {
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

    let mut units = source_discovery::input_units_from_paths(
        client,
        cli_paths,
        FingerprintPolicy::PlainFile,
        DecoderKind::kiro_file(),
    )?
    .into_iter()
    .map(configure_kiro_file_unit)
    .collect::<Vec<_>>();
    units.extend(source_discovery::input_units_from_paths(
        client,
        sqlite_paths,
        FingerprintPolicy::SqliteWithWal,
        DecoderKind::kiro_sqlite(),
    )?);
    units.extend(source_discovery::input_units_from_paths(
        client,
        global_storage_paths,
        FingerprintPolicy::PlainFile,
        DecoderKind::kiro_global_storage(),
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
    units: &mut Vec<DiscoveredInput>,
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

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    fn kiro_global_unit(path: PathBuf) -> DiscoveredInput {
        DiscoveredInput::plain_file(path, DecoderKind::kiro_global_storage())
    }

    fn kiro_file_unit(path: PathBuf) -> DiscoveredInput {
        let sidecar = path.with_extension("jsonl");
        DiscoveredInput::plain_file(path, DecoderKind::kiro_file())
            .with_optional_dependency(sidecar)
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn kiro_driver_discovers_linux_file_sqlite_and_global_storage_inputs() {
        let home = tempfile::TempDir::new().unwrap();
        let file_path = home.path().join(".kiro/sessions/cli/session.json");
        let db_path = home.path().join(".local/share/kiro-cli/data.sqlite3");
        let global_path = home
            .path()
            .join(".config/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/execution.chat");
        for path in [&file_path, &db_path, &global_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = DiscoveryContext {
            client: ClientId::Kiro,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(units.len(), 3);
        assert!(units
            .iter()
            .any(|unit| unit.path == file_path && matches!(unit.decoder, DecoderKind::KiroFile)));
        assert!(units
            .iter()
            .any(|unit| unit.path == db_path && matches!(unit.decoder, DecoderKind::KiroSqlite)));
        assert!(units.iter().any(|unit| {
            unit.path == global_path && matches!(unit.decoder, DecoderKind::KiroGlobalStorage)
        }));
        let file_unit = units
            .iter()
            .find(|unit| matches!(unit.decoder, DecoderKind::KiroFile))
            .unwrap();
        assert_eq!(
            file_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: file_path.with_extension("jsonl"),
                related_failure_policy:
                    crate::input_record_cache::RelatedInputFailurePolicy::PreservePrimary,
            }
        );
        let global_unit = units
            .iter()
            .find(|unit| matches!(unit.decoder, DecoderKind::KiroGlobalStorage))
            .unwrap();
        assert_eq!(global_unit.fingerprint_policy, FingerprintPolicy::PlainFile);
        for unit in &units {
            let decoder_id = match unit.decoder {
                DecoderKind::KiroFile => DecoderId::KiroFile,
                DecoderKind::KiroSqlite => DecoderId::KiroSqlite,
                DecoderKind::KiroGlobalStorage => DecoderId::KiroGlobalStorage,
                _ => unreachable!(),
            };
            assert_eq!(unit.decoder.version(), DecoderVersion::current(decoder_id));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kiro_linux_built_in_candidates_exclude_macos_and_windows_layouts() {
        let home = Path::new("/home/alice");
        assert_eq!(
            kiro_db_candidates(home),
            vec![home.join(".local/share/kiro-cli/data.sqlite3")]
        );
        let roots = kiro_global_storage_roots(home);
        assert_eq!(roots.len(), 2);
        assert!(roots
            .iter()
            .all(|root| root.starts_with(home.join(".config"))));
        assert!(roots.iter().all(|root| {
            let root = root.to_string_lossy();
            !root.contains("Library") && !root.contains("AppData")
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kiro_macos_built_in_candidates_exclude_linux_and_windows_layouts() {
        let home = Path::new("/Users/alice");
        assert_eq!(
            kiro_db_candidates(home),
            vec![home.join("Library/Application Support/kiro-cli/data.sqlite3")]
        );
        let roots = kiro_global_storage_roots(home);
        assert_eq!(roots.len(), 2);
        assert!(roots
            .iter()
            .all(|root| root.starts_with(home.join("Library/Application Support"))));
        assert!(roots.iter().all(|root| {
            let root = root.to_string_lossy();
            !root.contains(".config") && !root.contains("AppData")
        }));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn kiro_windows_built_in_candidates_exclude_linux_and_macos_layouts() {
        let home = Path::new(r"C:\Users\alice");
        assert!(kiro_db_candidates(home).is_empty());
        let roots = kiro_global_storage_roots(home);
        assert_eq!(roots.len(), 2);
        assert!(roots
            .iter()
            .all(|root| root.starts_with(home.join("AppData/Roaming"))));
        assert!(roots.iter().all(|root| {
            let root = root.to_string_lossy();
            !root.contains(".config") && !root.contains("Library")
        }));
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
        extra_scan_paths.insert(ClientId::Kiro, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::Kiro,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        assert_eq!(units.len(), 3);

        let cli_unit = units.iter().find(|unit| unit.path == cli_path).unwrap();
        assert!(matches!(cli_unit.decoder, DecoderKind::KiroFile));
        assert_eq!(
            cli_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: cli_sidecar,
                related_failure_policy:
                    crate::input_record_cache::RelatedInputFailurePolicy::PreservePrimary,
            }
        );
        assert_eq!(
            cli_unit.decoder.version(),
            DecoderVersion::current(DecoderId::KiroFile)
        );

        let sqlite_unit = units.iter().find(|unit| unit.path == sqlite_path).unwrap();
        assert!(matches!(sqlite_unit.decoder, DecoderKind::KiroSqlite));
        assert_eq!(
            sqlite_unit.fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
        assert_eq!(
            sqlite_unit.decoder.version(),
            DecoderVersion::current(DecoderId::KiroSqlite)
        );

        let global_unit = units.iter().find(|unit| unit.path == global_path).unwrap();
        assert!(matches!(
            global_unit.decoder,
            DecoderKind::KiroGlobalStorage
        ));
        assert_eq!(
            global_unit.decoder.version(),
            DecoderVersion::current(DecoderId::KiroGlobalStorage)
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
        let units = paths.into_iter().map(kiro_global_unit).collect::<Vec<_>>();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let mut cache = crate::input_record_cache::InputRecordShardStore::default();
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Kiro);
        let mut ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundUsageSink::new(binding, &mut messages);

        DRIVER.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Kiro));
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(ctx.health().failed_inputs(), 0);
        assert_eq!(ctx.health().partial_inputs(), 0);
    }

    #[test]
    fn malformed_kiro_cli_header_is_unavailable_at_the_driver_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "not json").unwrap();

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![kiro_file_unit(path)]),
            &ParseContext::uncancelled(None),
        );

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

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![unit]),
            &ParseContext::uncancelled(None),
        );

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

        let mut cache = crate::input_record_cache::InputRecordShardStore::default();
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Kiro);
        let mut ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        DRIVER.fold(parsed, &mut ctx, &mut sink).unwrap();

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
        let mut cache = crate::input_record_cache::InputRecordShardStore::default();
        cache.insert(
            crate::input_record_cache::CachedInputEntry::new_with_version(
                &path,
                unit.decoder.version(),
                unit.input_policy().fingerprint().unwrap(),
                vec![crate::records::UsageRecord::new(
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
            ),
        );
        std::fs::write(&sidecar, "new sidecar only").unwrap();

        let planned = DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
            .unwrap();

        assert!(matches!(
            planned,
            crate::integrations::CacheHitPlan::Miss(_)
        ));
    }
}
