//! Canonical local-generation cache.
//!
//! The cache stores exactly one [`Generation`]. Usage projections are derived
//! after loading and are never persisted beside their authority.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokscale_core::{AcquisitionScope, ClientUniverse, Generation};

const CACHE_MAGIC: [u8; 8] = *b"TOKGEN\0\0";
const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const HEADER_LEN: usize = 8 + 4 + 8 + 8 + 32;

pub const TUI_DEFAULT_GROUP_BY: tokscale_core::GroupBy = tokscale_core::GroupBy::Model;

#[derive(Debug)]
pub enum CacheResult {
    Fresh(Generation),
    Stale(Generation),
    Miss,
}

fn cache_file() -> Result<PathBuf, tokscale_core::paths::ConfigDirUnavailable> {
    crate::paths::try_get_cache_dir().map(|directory| directory.join("tui-generation.bin"))
}

pub fn load_generation_cache(
    expected_universe: &ClientUniverse,
    expected_scope: &AcquisitionScope,
) -> CacheResult {
    let path = match cache_file() {
        Ok(path) => path,
        Err(_) => return CacheResult::Miss,
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return CacheResult::Miss,
    };
    let (timestamp, generation) = match decode_generation(&bytes) {
        Ok(decoded) => decoded,
        Err(_) => return CacheResult::Miss,
    };

    if generation.universe() != expected_universe || generation.scope() != expected_scope {
        return CacheResult::Miss;
    }
    if generation.health().requires_input_retry() {
        return CacheResult::Stale(generation);
    }

    let now = match current_timestamp_ms() {
        Ok(now) => now,
        Err(_) => return CacheResult::Miss,
    };
    match now.checked_sub(timestamp) {
        Some(age) if age <= CACHE_STALE_THRESHOLD_MS => CacheResult::Fresh(generation),
        _ => CacheResult::Stale(generation),
    }
}

pub fn save_generation_cache(generation: &Generation) -> anyhow::Result<()> {
    generation.validate()?;
    let path = cache_file()?;
    let body = bincode::serialize(generation).context("failed to encode canonical generation")?;
    let body_len = u64::try_from(body.len()).context("generation cache body is too large")?;
    let digest: [u8; 32] = Sha256::digest(&body).into();
    let timestamp = current_timestamp_ms()?;

    tokscale_core::fs_atomic::write_atomic_with(&path, |file| {
        use std::io::Write;

        file.write_all(&CACHE_MAGIC)?;
        file.write_all(&CACHE_SCHEMA_VERSION.to_le_bytes())?;
        file.write_all(&timestamp.to_le_bytes())?;
        file.write_all(&body_len.to_le_bytes())?;
        file.write_all(&digest)?;
        file.write_all(&body)?;
        Ok(())
    })
    .with_context(|| format!("failed to persist generation cache `{}`", path.display()))
}

fn decode_generation(bytes: &[u8]) -> anyhow::Result<(u64, Generation)> {
    if bytes.len() < HEADER_LEN {
        anyhow::bail!("generation cache is truncated");
    }
    if bytes[..8] != CACHE_MAGIC {
        anyhow::bail!("generation cache has unknown magic");
    }

    let schema_version = read_u32(&bytes[8..12]);
    if schema_version != CACHE_SCHEMA_VERSION {
        anyhow::bail!("unsupported generation cache schema {schema_version}");
    }
    let timestamp = read_u64(&bytes[12..20]);
    let declared_body_len = read_u64(&bytes[20..28]);
    let body_len =
        usize::try_from(declared_body_len).context("generation cache body length is too large")?;
    let expected_len = HEADER_LEN
        .checked_add(body_len)
        .context("generation cache length overflow")?;
    if bytes.len() != expected_len {
        anyhow::bail!("generation cache body length does not match its envelope");
    }

    let expected_digest: [u8; 32] = bytes[28..60]
        .try_into()
        .expect("fixed generation digest slice");
    let body = &bytes[HEADER_LEN..];
    let actual_digest: [u8; 32] = Sha256::digest(body).into();
    if actual_digest != expected_digest {
        anyhow::bail!("generation cache digest does not match its contents");
    }

    let generation: Generation =
        bincode::deserialize(body).context("failed to decode canonical generation")?;
    generation.validate()?;
    Ok((timestamp, generation))
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed u32 cache field"))
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed u64 cache field"))
}

fn current_timestamp_ms() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("system timestamp exceeds u64::MAX")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use serial_test::serial;
    use tokscale_core::{ClientId, InputFootprint, SourceFingerprint, UsageIndex};

    use super::*;

    struct EnvGuard {
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("TOKSCALE_CONFIG_DIR");
            unsafe {
                std::env::set_var("TOKSCALE_CONFIG_DIR", path.as_os_str());
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var("TOKSCALE_CONFIG_DIR", value),
                    None => std::env::remove_var("TOKSCALE_CONFIG_DIR"),
                }
            }
        }
    }

    fn generation(home: &std::path::Path) -> Generation {
        Generation::new(
            AcquisitionScope {
                resolved_home_dir: home.to_path_buf(),
                since: None,
                until: None,
                year: None,
            },
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            SourceFingerprint::from_bytes([9; 32]),
            UsageIndex::new(),
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 13)]).unwrap(),
            tokscale_core::input_health::HealthSummary::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    #[serial]
    fn cache_round_trips_only_the_canonical_generation() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        let scope = generation.scope().clone();
        let universe = generation.universe().clone();

        save_generation_cache(&generation).unwrap();

        let CacheResult::Fresh(loaded) = load_generation_cache(&universe, &scope) else {
            panic!("saved generation must be fresh");
        };
        assert_eq!(
            loaded.source_fingerprint(),
            SourceFingerprint::from_bytes([9; 32])
        );
        assert_eq!(loaded.input_footprint().total_bytes().unwrap(), 13);
        assert!(loaded
            .project(&tokscale_core::UsageQuery::full(
                loaded.universe(),
                tokscale_core::GroupBy::Model,
            ))
            .is_ok());
    }

    #[test]
    #[serial]
    fn cache_rejects_corruption_and_trailing_shapes() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        save_generation_cache(&generation).unwrap();
        let path = cache_file().unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[HEADER_LEN] ^= 0xff;
        tokscale_core::fs_atomic::write_atomic(&path, &bytes).unwrap();
        assert!(matches!(
            load_generation_cache(generation.universe(), generation.scope()),
            CacheResult::Miss
        ));

        save_generation_cache(&generation).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        tokscale_core::fs_atomic::write_atomic(&path, &bytes).unwrap();
        assert!(matches!(
            load_generation_cache(generation.universe(), generation.scope()),
            CacheResult::Miss
        ));
    }

    #[test]
    #[serial]
    fn cache_identity_requires_exact_scope_and_universe() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        save_generation_cache(&generation).unwrap();

        let other_scope = AcquisitionScope {
            year: Some("2025".into()),
            ..generation.scope().clone()
        };
        assert!(matches!(
            load_generation_cache(generation.universe(), &other_scope),
            CacheResult::Miss
        ));

        let other_universe = ClientUniverse::new([ClientId::Codex]).unwrap();
        assert!(matches!(
            load_generation_cache(&other_universe, generation.scope()),
            CacheResult::Miss
        ));
    }
}
