pub(crate) mod decode;

use std::path::Path;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::{apply_workspace, CachedFileDriver};
use crate::integrations::SourceSpec;
use crate::records::UsageRecord;

const SOURCE: SourceSpec = SourceSpec::home(
    ".factory/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::settings_json),
);
fn enrich(path: &Path, messages: &mut [UsageRecord]) {
    apply_workspace(messages, decode::droid_workspace_metadata(path));
    decode::classify_droid_main_session(path, messages);
}

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_dependency(
    SOURCE,
    DecoderId::Droid,
    decode::droid_agent_dependency_path,
    decode::parse_droid_file,
)
.with_workspace_enrichment(enrich);
