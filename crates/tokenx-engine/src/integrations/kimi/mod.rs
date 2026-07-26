pub(crate) mod decode;

use std::path::Path;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::{apply_workspace, CachedFileDriver};
use crate::integrations::SourceSpec;
use crate::records::UsageRecord;

const SOURCE: SourceSpec = SourceSpec::home(
    ".kimi-code/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::wire_jsonl),
);
fn enrich(path: &Path, messages: &mut [UsageRecord]) {
    apply_workspace(messages, decode::kimi_workspace_metadata(path));
}

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_dependency(
    SOURCE,
    DecoderId::Kimi,
    decode::kimi_config_dependency_path,
    decode::parse_kimi_file,
)
.with_workspace_enrichment(enrich);
