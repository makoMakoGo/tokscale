pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};

const SOURCE: SourceSpec = SourceSpec::home(
    ".mux/sessions",
    crate::integrations::SourceMatcher::new(
        crate::integrations::source_matchers::session_usage_json,
    ),
);
const STABLE_DEDUP_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = STABLE_DEDUP_REVISION + 2;

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new(
    SOURCE,
    DecoderId::Mux,
    DECODER_REVISION,
    decode::parse_mux_file,
);
