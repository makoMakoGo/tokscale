pub(crate) mod decode;

use crate::input_record_cache::DecoderId;
use crate::integrations::file::CachedFileDriver;
use crate::integrations::SourceSpec;

const SOURCE: SourceSpec = SourceSpec::home(
    ".grok/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::updates_jsonl),
);
pub(crate) const RELATED_METADATA_SIBLINGS: &[&str] = &["summary.json", "events.jsonl"];

pub(crate) static DRIVER: CachedFileDriver = CachedFileDriver::new_with_optional_siblings(
    SOURCE,
    DecoderId::Grok,
    RELATED_METADATA_SIBLINGS,
    decode::parse_grok_updates_file,
);
