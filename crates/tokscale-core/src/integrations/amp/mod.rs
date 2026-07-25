pub(crate) mod decode;

use crate::clients::ClientId;
use crate::integrations::file::CachedFileAdapter;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::local_share("amp/threads", "T-*.json");
pub(crate) const DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 2;

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Amp,
    SOURCE,
    DecoderId::Amp,
    DECODER_REVISION,
    decode::parse_amp_file,
);
