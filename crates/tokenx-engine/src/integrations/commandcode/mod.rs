pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};

const SOURCE: SourceSpec = SourceSpec::home(
    ".commandcode/projects",
    crate::integrations::SourceMatcher::new(decode::is_usage_transcript_file),
);
const RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = RECORD_REJECTION_REVISION + 2;

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_required_dependency(
    SOURCE,
    DecoderId::CommandCode,
    DECODER_REVISION,
    decode::commandcode_config_dependency_path,
    decode::parse_commandcode_file,
);
