pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};

const SOURCE: SourceSpec = SourceSpec::home(
    ".grok/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::updates_jsonl),
);
const TOTAL_ONLY_IMPUTATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const RECORD_REJECTION_REVISION: u32 = TOTAL_ONLY_IMPUTATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = RECORD_REJECTION_REVISION + 1;
pub(crate) const RELATED_METADATA_SIBLINGS: &[&str] = &["summary.json", "events.jsonl"];

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_siblings(
    SOURCE,
    DecoderId::Grok,
    DECODER_REVISION,
    RELATED_METADATA_SIBLINGS,
    decode::parse_grok_updates_file,
);
