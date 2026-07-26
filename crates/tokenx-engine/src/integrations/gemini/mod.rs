pub(crate) mod decode;

use std::path::Path;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::{apply_workspace, CachedFileDriver};
use crate::integrations::SourceSpec;
use crate::records::UsageRecord;

fn should_descend_into_project(path: &Path, depth: usize) -> bool {
    depth != 1 || decode::is_current_project_dir(path)
}

const SOURCE: SourceSpec = SourceSpec::home(
    ".gemini/tmp",
    crate::integrations::SourceMatcher::with_directory_filter(
        decode::is_current_project_session,
        should_descend_into_project,
    ),
);
fn enrich(path: &Path, messages: &mut [UsageRecord]) {
    apply_workspace(messages, decode::gemini_workspace_metadata(path));
}

pub(crate) static DRIVER: CachedFileDriver =
    CachedFileDriver::new(SOURCE, DecoderId::Gemini, decode::parse_gemini_file)
        .with_workspace_enrichment(enrich);
