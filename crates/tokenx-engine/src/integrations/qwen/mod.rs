pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::home(
    ".qwen/projects",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);
pub(crate) static DRIVER: CachedFileDriver =
    CachedFileDriver::new(SOURCE, DecoderId::Qwen, decode::parse_qwen_file);
