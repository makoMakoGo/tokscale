pub(crate) mod decode;

use crate::clients::ClientId;
use crate::integrations::file::CachedFileAdapter;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::home(".mux/sessions", "session-usage.json");
const STABLE_DEDUP_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = STABLE_DEDUP_REVISION + 2;

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Mux,
    SOURCE,
    DecoderId::Mux,
    DECODER_REVISION,
    decode::parse_mux_file,
);
