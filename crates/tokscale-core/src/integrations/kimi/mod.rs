pub(crate) mod decode;

use std::path::Path;

use crate::clients::ClientId;
use crate::integrations::file::{apply_workspace, CachedFileAdapter};
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;
use crate::records::ParsedMessage;

const SOURCE: SourceSpec = SourceSpec::home(".kimi-code/sessions", "wire.jsonl");
pub(crate) const DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 2;

fn enrich(path: &Path, messages: &mut [ParsedMessage]) {
    apply_workspace(messages, decode::kimi_workspace_metadata(path));
}

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new_with_optional_dependency(
    ClientId::Kimi,
    SOURCE,
    DecoderId::Kimi,
    DECODER_REVISION,
    decode::kimi_config_dependency_path,
    decode::parse_kimi_file,
)
.with_workspace_enrichment(enrich);
