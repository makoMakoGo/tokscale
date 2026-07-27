pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::home(
    ".mux/sessions",
    crate::integrations::SourceMatcher::new(
        crate::integrations::source_matchers::session_usage_json,
    ),
);
pub(crate) static DRIVER: CachedFileDriver =
    CachedFileDriver::new(SOURCE, DecoderId::Mux, decode::parse_mux_file);
