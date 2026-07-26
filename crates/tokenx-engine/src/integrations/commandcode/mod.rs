pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::home(
    ".commandcode/projects",
    crate::integrations::SourceMatcher::new(decode::is_usage_transcript_file),
);
pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_required_dependency(
    SOURCE,
    DecoderId::CommandCode,
    decode::commandcode_config_dependency_path,
    decode::parse_commandcode_file,
);
