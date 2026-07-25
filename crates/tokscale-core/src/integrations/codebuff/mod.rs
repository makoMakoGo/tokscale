pub(crate) mod decode;

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputUnit, ParseContext, ParsedUnit, SourceSpec,
};
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;

pub(crate) struct Integration;

const CODEBUFF_RECORD_REJECTION_REVISION: u32 =
    crate::integrations::MODEL_ID_CANONICALIZATION_REVISION + 2;
const SOURCE: SourceSpec = SourceSpec::home(".config/manicode/projects", "chat-messages.json");

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Codebuff
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let mut roots = codebuff_roots(ctx.home_dir);
        roots.extend(adapter_discover::extra_roots_for_client(client, ctx)?);

        adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, roots, SOURCE.pattern())?,
            FingerprintPolicy::PlainFile,
            DecoderSpec::plain(DecoderId::Codebuff, CODEBUFF_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_codebuff_file(path)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
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

fn codebuff_roots(home_dir: &Path) -> Vec<PathBuf> {
    ["manicode", "manicode-dev", "manicode-staging"]
        .into_iter()
        .map(|channel| home_dir.join(".config").join(channel).join("projects"))
        .collect()
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    #[test]
    fn codebuff_adapter_scans_current_channels_and_configured_extras() {
        let home = tempfile::TempDir::new().unwrap();
        let default_file = home.path().join(
            ".config/manicode/projects/proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json",
        );
        let extra_root = home.path().join("extra-codebuff");
        let extra_file = extra_root.join("proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json");
        write_file(&default_file);
        write_file(&extra_file);

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("codebuff".to_string(), vec![extra_root.clone()]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };
        let units = INTEGRATION.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert!(paths.contains(&default_file));
        assert!(paths.contains(&extra_file));
        assert!(units.iter().all(|unit| {
            unit.decoder.version()
                == DecoderVersion::new(DecoderId::Codebuff, CODEBUFF_RECORD_REJECTION_REVISION)
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
