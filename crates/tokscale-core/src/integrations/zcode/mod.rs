pub(crate) mod decode;

use crate::clients::ClientId;
use crate::integrations::file::CachedFileAdapter;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::home(".zcode/projects", "*.jsonl");
const OVERLAP_NORMALIZATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = OVERLAP_NORMALIZATION_REVISION + 1;

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Zcode,
    SOURCE,
    DecoderId::Zcode,
    DECODER_REVISION,
    decode::parse_zcode_file,
);
