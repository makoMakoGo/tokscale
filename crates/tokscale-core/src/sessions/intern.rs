//! Global string interner for high-repetition identity fields.
//!
//! 255K messages share a few hundred distinct clients/models/providers and a
//! few tens of thousands of session ids; owning a fresh `String` per field per
//! message multiplied every corpus copy (ADR 0008). `intern` returns a shared
//! `Arc<str>` so each distinct value is allocated once per process.

use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

static POOL: OnceLock<Mutex<HashSet<Arc<str>>>> = OnceLock::new();

pub fn intern(value: &str) -> Arc<str> {
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match pool.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = guard.get(value) {
        return Arc::clone(existing);
    }
    let shared: Arc<str> = Arc::from(value);
    guard.insert(Arc::clone(&shared));
    shared
}

/// Deserialize a string field through the interner (cache loads and JSON
/// parses go through here, so every corpus copy shares one allocation per
/// distinct value).
pub fn de_intern<'de, D>(deserializer: D) -> Result<Arc<str>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <std::borrow::Cow<'_, str>>::deserialize(deserializer)?;
    Ok(intern(&value))
}

pub fn de_intern_opt<'de, D>(deserializer: D) -> Result<Option<Arc<str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<std::borrow::Cow<'_, str>>>::deserialize(deserializer)?;
    Ok(value.map(|value| intern(&value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_shares_one_allocation_per_distinct_value() {
        let first = intern("claude-fable-5");
        let second = intern("claude-fable-5");
        assert!(Arc::ptr_eq(&first, &second));
        let other = intern("claude-fable-5[1m]");
        assert!(!Arc::ptr_eq(&first, &other));
    }
}
