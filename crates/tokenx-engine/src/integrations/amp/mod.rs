pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};

const SOURCE: SourceSpec = SourceSpec::local_share(
    "amp/threads",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::amp_thread),
);
pub(crate) const DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 2;

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new(
    SOURCE,
    DecoderId::Amp,
    DECODER_REVISION,
    decode::parse_amp_file,
);
