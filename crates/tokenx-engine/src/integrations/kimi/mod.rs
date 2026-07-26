pub(crate) mod decode;

use std::path::Path;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::{apply_workspace, CachedFileDriver};
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::records::UsageRecord;

const SOURCE: SourceSpec = SourceSpec::home(
    ".kimi-code/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::wire_jsonl),
);
pub(crate) const DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 2;

fn enrich(path: &Path, messages: &mut [UsageRecord]) {
    apply_workspace(messages, decode::kimi_workspace_metadata(path));
}

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_dependency(
    SOURCE,
    DecoderId::Kimi,
    DECODER_REVISION,
    decode::kimi_config_dependency_path,
    decode::parse_kimi_file,
)
.with_workspace_enrichment(enrich);
