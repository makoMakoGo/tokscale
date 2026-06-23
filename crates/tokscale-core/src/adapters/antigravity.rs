use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, SourceUnitMeta,
};
use crate::clients::{ClientId, PathRoot};
use crate::sessions;

const CLI_RELATIVE_PATH: &str = "antigravity-cli/conversations";
const CLI_PATTERN: &str = "*.db";

pub(crate) struct AntigravityAdapter;

impl LocalSourceAdapter for AntigravityAdapter {
    fn client(&self) -> ClientId {
        ClientId::Antigravity
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let mut units = adapter_discover::discover_default_scanned_units(
            ClientId::Antigravity,
            ctx,
            FingerprintPolicy::None,
        )
        .into_iter()
        .map(|unit| unit.with_meta(SourceUnitMeta::AntigravityCacheJsonl))
        .collect::<Vec<_>>();

        let cli_root = PathRoot::EnvVar {
            var: "GEMINI_CLI_HOME",
            fallback_relative: ".gemini",
        }
        .resolve_with_env_strategy(ctx.home_dir, ctx.use_env_roots);

        units.extend(
            adapter_discover::source_units_from_paths(
                ClientId::Antigravity,
                adapter_discover::scan_roots(
                    [PathBuf::from(cli_root).join(CLI_RELATIVE_PATH)],
                    CLI_PATTERN,
                ),
                FingerprintPolicy::SqliteWithWal,
            )
            .into_iter()
            .map(|unit| unit.with_meta(SourceUnitMeta::AntigravityCliSqlite)),
        );

        units
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::AntigravityCacheJsonl => adapter_cache::load_or_parse_unit_with(
                    unit,
                    ctx,
                    sessions::antigravity::parse_antigravity_file,
                ),
                SourceUnitMeta::AntigravityCliSqlite => adapter_cache::load_or_parse_unit_with(
                    unit,
                    ctx,
                    sessions::antigravity_cli::parse_antigravity_cli_file,
                ),
                SourceUnitMeta::None
                | SourceUnitMeta::OpenCodeSqlite
                | SourceUnitMeta::OpenCodeJson
                | SourceUnitMeta::KiroFile
                | SourceUnitMeta::KiroSqlite
                | SourceUnitMeta::KiroGlobalStorage
                | SourceUnitMeta::Codex { .. } => {
                    unreachable!("unexpected Antigravity source unit meta")
                }
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut seen = HashSet::new();
        for unit in parsed {
            let ParsedUnit {
                unit,
                messages,
                cache_entry,
                invalidate_cache,
            } = unit;
            let path = unit.path.clone();
            let messages = adapter_cache::resolve_messages(messages, ctx);
            sink.extend_messages(
                messages
                    .into_iter()
                    .filter(|message| crate::should_keep_deduped_message(&mut seen, message))
                    .collect(),
            );

            if let Some(entry) = cache_entry {
                ctx.source_cache.insert(entry);
            } else if invalidate_cache {
                ctx.source_cache.remove(&path);
            }
        }
    }
}

pub(crate) static ANTIGRAVITY_ADAPTER: AntigravityAdapter = AntigravityAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::UnitMessageSource;
    use crate::scanner::ScannerSettings;
    use crate::{message_cache, TokenBreakdown, UnifiedMessage};
    use std::path::Path;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    #[test]
    fn discovers_ide_cache_and_cli_databases_as_antigravity_sources() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_path = home
            .path()
            .join(".config/tokscale/antigravity-cache/sessions/session.jsonl");
        let cli_path = home
            .path()
            .join(".gemini/antigravity-cli/conversations/session.db");
        for path in [&cache_path, &cli_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let settings = ScannerSettings::default();
        let units = ANTIGRAVITY_ADAPTER.discover(&scan_context(home.path(), &settings));

        assert_eq!(units.len(), 2);
        assert!(units.iter().any(|unit| {
            unit.client == ClientId::Antigravity
                && unit.path == cache_path
                && unit.fingerprint_policy == FingerprintPolicy::None
                && matches!(unit.meta, SourceUnitMeta::AntigravityCacheJsonl)
        }));
        assert!(units.iter().any(|unit| {
            unit.client == ClientId::Antigravity
                && unit.path == cli_path
                && unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal
                && matches!(unit.meta, SourceUnitMeta::AntigravityCliSqlite)
        }));
    }

    fn parsed_unit(path: &Path, meta: SourceUnitMeta, message: UnifiedMessage) -> ParsedUnit {
        ParsedUnit {
            unit: SourceUnit::plain_file(ClientId::Antigravity, path.to_path_buf()).with_meta(meta),
            messages: UnitMessageSource::Fresh(vec![message]),
            cache_entry: None,
            invalidate_cache: false,
        }
    }

    fn antigravity_message(session_id: &str, dedup_key: Option<u64>) -> UnifiedMessage {
        UnifiedMessage::new_with_dedup(
            "antigravity",
            "gemini-3.1-pro",
            "google",
            session_id,
            1_781_000_000_000,
            TokenBreakdown {
                input: 10,
                output: 2,
                cache_read: 3,
                cache_write: 0,
                reasoning: 1,
            },
            0.0,
            dedup_key,
        )
    }

    #[test]
    fn fold_dedupes_shared_ide_and_cli_response_ids() {
        let dir = tempfile::TempDir::new().unwrap();
        let dedup_key = sessions::antigravity::response_dedup_key("resp-shared");
        let ide = parsed_unit(
            &dir.path().join("ide.jsonl"),
            SourceUnitMeta::AntigravityCacheJsonl,
            antigravity_message("ide-session", Some(dedup_key)),
        );
        let cli = parsed_unit(
            &dir.path().join("cli.db"),
            SourceUnitMeta::AntigravityCliSqlite,
            antigravity_message("cli-session", Some(dedup_key)),
        );
        let mut cache = message_cache::SourceMessageCache::default();
        let mut messages = Vec::new();

        ANTIGRAVITY_ADAPTER.fold(
            vec![ide, cli],
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut messages,
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "ide-session");
    }
}
