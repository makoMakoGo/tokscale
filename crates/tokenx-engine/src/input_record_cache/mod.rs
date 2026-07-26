#[cfg(not(any(unix, windows)))]
compile_error!("input-record cache requires stable Unix or Windows file identity");

// Input-record cache shards split serialization layout from decoder/input
// semantics. Bump this only when the shard bincode layout changes; decoder
// semantics are invalidated by their generated source contract fingerprint.
const CACHE_FORMAT_VERSION: u32 = 1;
#[cfg(test)]
const UNSUPPORTED_CACHE_FORMAT_VERSION: u32 = CACHE_FORMAT_VERSION - 1;
const SHARD_MAGIC: [u8; 8] = *b"TOKENXR\0";
const SHARD_KEY_FORMAT_VERSION: u32 = 1;
const SHARDS_DIRNAME: &str = "shards";
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARD_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const SHARD_DIGEST_BYTES: usize = 32;
const SHARD_ENVELOPE_BYTES: usize = 8 + 4 + 8 + 8 + SHARD_DIGEST_BYTES + SHARD_DIGEST_BYTES;
const SHARD_HEADER_LEN_OFFSET: usize = 12;
const SHARD_BODY_LEN_OFFSET: usize = 20;
const SHARD_HEADER_DIGEST_OFFSET: usize = 28;
const SHARD_BODY_DIGEST_OFFSET: usize = SHARD_HEADER_DIGEST_OFFSET + SHARD_DIGEST_BYTES;
#[cfg(test)]
const HASH_BUFFER_BYTES: usize = 64 * 1024;

mod decoder;
mod error;
mod input;
mod plan;
mod prune;
mod store;
#[cfg(test)]
mod test_support;
mod wire;

pub(crate) use decoder::{DecoderId, DecoderVariant, DecoderVersion};
pub(crate) use error::{InputRecordCacheError, InputSnapshotError, RelatedInputFailurePolicy};
pub use error::{InputRecordCachePruneError, InputRecordCachePruneStats};
pub(crate) use input::{
    build_codex_incremental_cache, hash_inventory_bytes, hash_inventory_len, hash_inventory_path,
    input_file_identity_from_open_file, CodexIncrementalCache, InputFileIdentity, InputFingerprint,
    InputPolicy, InputSnapshot,
};
pub(crate) use plan::{
    codex_cache_meta_is_consistent, CacheLookupFailure, CacheReadFailure, CacheReadPlan,
    CacheWritePlan, CachedInputMeta,
};
#[cfg(test)]
pub(crate) use plan::{CacheReadFailureReason, CachedInputEntry};
pub use prune::prune_input_record_cache;
pub(crate) use store::InputRecordShardStore;
#[cfg(test)]
pub(crate) use test_support::{
    get_input_read_stats, record_input_bytes, record_input_hash_start, reset_input_read_stats,
    InputReadStats,
};
#[cfg(test)]
pub(crate) use wire::{
    mark_current_key_shard_as_future_format_for_test,
    mark_current_key_shard_as_unsupported_format_for_test, replace_shard_record_count_for_test,
    shard_path_for_test, truncate_shard_after_header_for_test,
};

#[cfg(test)]
mod tests;
