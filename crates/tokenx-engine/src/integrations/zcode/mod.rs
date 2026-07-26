pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::home(
    ".zcode/projects",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);
pub(crate) static DRIVER: CachedFileDriver =
    CachedFileDriver::new(SOURCE, DecoderId::Zcode, decode::parse_zcode_file);
