pub(crate) mod decode;

use std::path::Path;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::{apply_workspace, CachedFileDriver};
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::records::UsageRecord;

const SOURCE: SourceSpec = SourceSpec::home(
    ".factory/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::settings_json),
);
pub(crate) const RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = RECORD_REJECTION_REVISION + 3;

fn enrich(path: &Path, messages: &mut [UsageRecord]) {
    apply_workspace(messages, decode::droid_workspace_metadata(path));
    decode::classify_droid_main_session(path, messages);
}

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_dependency(
    SOURCE,
    DecoderId::Droid,
    DECODER_REVISION,
    decode::droid_agent_dependency_path,
    decode::parse_droid_file,
)
.with_workspace_enrichment(enrich);
