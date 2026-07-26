pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};

const SOURCE: SourceSpec = SourceSpec::home(
    ".zcode/projects",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);
const OVERLAP_NORMALIZATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = OVERLAP_NORMALIZATION_REVISION + 1;

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new(
    SOURCE,
    DecoderId::Zcode,
    DECODER_REVISION,
    decode::parse_zcode_file,
);
