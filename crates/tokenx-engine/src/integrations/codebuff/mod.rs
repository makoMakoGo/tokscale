pub(crate) mod decode;

use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

pub(crate) struct Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".config/manicode/projects",
    crate::integrations::SourceMatcher::new(
        crate::integrations::source_matchers::chat_messages_json,
    ),
);

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut roots = codebuff_roots(ctx.home_dir);
        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);

        source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::Codebuff),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_codebuff_file(path)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        pipeline_cache::plan_cache_hit(unit, input_cache)
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

fn codebuff_roots(home_dir: &Path) -> Vec<PathBuf> {
    ["manicode", "manicode-dev", "manicode-staging"]
        .into_iter()
        .map(|channel| home_dir.join(".config").join(channel).join("projects"))
        .collect()
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    #[test]
    fn codebuff_driver_scans_current_channels_and_configured_extras() {
        let home = tempfile::TempDir::new().unwrap();
        let default_file = home.path().join(
            ".config/manicode/projects/proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json",
        );
        let extra_root = home.path().join("extra-codebuff");
        let extra_file = extra_root.join("proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json");
        write_file(&default_file);
        write_file(&extra_file);

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(ClientId::Codebuff, vec![extra_root.clone()]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::Codebuff,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };
        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert!(paths.contains(&default_file));
        assert!(paths.contains(&extra_file));
        assert!(units.iter().all(|unit| {
            unit.decoder.version() == DecoderVersion::current(DecoderId::Codebuff)
        }));
    }

    #[test]
    fn codebuff_roots_use_current_channel_directories() {
        assert_eq!(
            codebuff_roots(Path::new("/home/alice")),
            vec![
                PathBuf::from("/home/alice/.config/manicode/projects"),
                PathBuf::from("/home/alice/.config/manicode-dev/projects"),
                PathBuf::from("/home/alice/.config/manicode-staging/projects"),
            ]
        );
    }
}
