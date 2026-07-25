pub(crate) mod decode;

use crate::clients::ClientId;
use crate::integrations::file::CachedFileAdapter;
use crate::integrations::{SourceSpec, MODEL_ID_CANONICALIZATION_REVISION};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::home(".commandcode/projects", "commandcode-session");
const RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const DECODER_REVISION: u32 = RECORD_REJECTION_REVISION + 2;

pub(crate) static INTEGRATION: CachedFileAdapter = CachedFileAdapter::new_with_required_dependency(
    ClientId::CommandCode,
    SOURCE,
    DecoderId::CommandCode,
    DECODER_REVISION,
    decode::commandcode_config_dependency_path,
    decode::parse_commandcode_file,
);
