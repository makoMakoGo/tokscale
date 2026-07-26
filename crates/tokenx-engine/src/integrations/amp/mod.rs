pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::local_share(
    "amp/threads",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::amp_thread),
);
pub(crate) static DRIVER: CachedFileDriver =
    CachedFileDriver::new(SOURCE, DecoderId::Amp, decode::parse_amp_file);
