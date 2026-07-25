pub(crate) mod decode;

use std::path::Path;

use crate::clients::ClientId;
use crate::integrations::file::{apply_workspace, CachedFileAdapter};
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;
use crate::records::ParsedMessage;

const SOURCE: SourceSpec = SourceSpec::home(".gemini/tmp", "gemini-session");
pub(crate) const DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;

fn enrich(path: &Path, messages: &mut [ParsedMessage]) {
    apply_workspace(messages, decode::gemini_workspace_metadata(path));
}

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Gemini,
    SOURCE,
    DecoderId::Gemini,
    DECODER_REVISION,
    decode::parse_gemini_file,
)
.with_workspace_enrichment(enrich);
