pub(crate) mod decode;

use crate::clients::ClientId;
use crate::integrations::file::CachedFileAdapter;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::home(".grok/sessions", "updates.jsonl");
const TOTAL_ONLY_IMPUTATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const RECORD_REJECTION_REVISION: u32 = TOTAL_ONLY_IMPUTATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = RECORD_REJECTION_REVISION + 1;
pub(crate) const RELATED_METADATA_SIBLINGS: &[&str] = &["summary.json", "events.jsonl"];

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new_with_optional_siblings(
    ClientId::Grok,
    SOURCE,
    DecoderId::Grok,
    DECODER_REVISION,
    RELATED_METADATA_SIBLINGS,
    decode::parse_grok_updates_file,
);
