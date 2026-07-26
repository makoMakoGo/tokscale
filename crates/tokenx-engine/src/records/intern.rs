//! Global weak string interner for high-repetition identity fields.
//!
//! The pool never owns interned values strongly: messages and aggregation
//! buckets determine their lifetime. Hash buckets are only an index; every
//! lookup confirms full byte equality, so hash collisions cannot alias values.

use serde::Deserialize;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[cfg(test)]
use std::cell::Cell;

type HashValue = u64;

fn default_hash(value: &str) -> HashValue {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

struct WeakInterner<H = fn(&str) -> HashValue> {
    buckets: HashMap<HashValue, WeakBucket>,
    hash: H,
}

enum WeakBucket {
    One(Weak<str>),
    Collisions(Vec<Weak<str>>),
}

impl WeakBucket {
    fn prune(&mut self) -> bool {
        match self {
            Self::One(value) => value.upgrade().is_some(),
            Self::Collisions(values) => {
                values.retain(|value| value.upgrade().is_some());
                match values.len() {
                    0 => false,
                    1 => {
                        let value = values.pop().expect("one weak identity remains");
                        *self = Self::One(value);
                        true
                    }
                    _ => {
                        values.shrink_to_fit();
                        true
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        match self {
            Self::One(_) => 1,
            Self::Collisions(values) => values.len(),
        }
    }

    #[cfg(test)]
    fn is_collision_vector(&self) -> bool {
        matches!(self, Self::Collisions(_))
    }
}

impl Default for WeakInterner {
    fn default() -> Self {
        Self {
            buckets: HashMap::new(),
            hash: default_hash,
        }
    }
}

impl<H> WeakInterner<H>
where
    H: Fn(&str) -> HashValue,
{
    #[cfg(test)]
    fn with_hasher(hash: H) -> Self {
        Self {
            buckets: HashMap::new(),
            hash,
        }
    }

    fn intern(&mut self, value: &str) -> Arc<str> {
        match self.buckets.entry((self.hash)(value)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let shared: Arc<str> = Arc::from(value);
                entry.insert(WeakBucket::One(Arc::downgrade(&shared)));
                shared
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let bucket = entry.get_mut();
                match bucket {
                    WeakBucket::One(weak) => {
                        if let Some(shared) = weak.upgrade() {
                            if shared.as_ref() == value {
                                return shared;
                            }
                            let new_shared: Arc<str> = Arc::from(value);
                            *bucket = WeakBucket::Collisions(vec![
                                Arc::downgrade(&shared),
                                Arc::downgrade(&new_shared),
                            ]);
                            new_shared
                        } else {
                            let shared: Arc<str> = Arc::from(value);
                            *weak = Arc::downgrade(&shared);
                            shared
                        }
                    }
                    WeakBucket::Collisions(values) => {
                        let mut matching = None;
                        values.retain(|weak| {
                            let Some(shared) = weak.upgrade() else {
                                return false;
                            };
                            if matching.is_none() && shared.as_ref() == value {
                                matching = Some(shared);
                            }
                            true
                        });
                        if let Some(shared) = matching {
                            if values.len() == 1 {
                                let only = values.pop().expect("matching weak identity remains");
                                *bucket = WeakBucket::One(only);
                            }
                            return shared;
                        }

                        let shared: Arc<str> = Arc::from(value);
                        if values.is_empty() {
                            *bucket = WeakBucket::One(Arc::downgrade(&shared));
                        } else {
                            values.push(Arc::downgrade(&shared));
                        }
                        shared
                    }
                }
            }
        }
    }

    fn prune_dead(&mut self) {
        self.buckets.retain(|_, bucket| bucket.prune());
        self.buckets.shrink_to_fit();
    }

    #[cfg(test)]
    fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    #[cfg(test)]
    fn weak_count(&self) -> usize {
        self.buckets.values().map(WeakBucket::len).sum()
    }

    #[cfg(test)]
    fn collision_vector_count(&self) -> usize {
        self.buckets
            .values()
            .filter(|bucket| bucket.is_collision_vector())
            .count()
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.buckets.capacity()
    }
}

static POOL: OnceLock<Mutex<WeakInterner>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static PRUNE_COUNT: Cell<usize> = const { Cell::new(0) };
}

pub fn intern(value: &str) -> Arc<str> {
    POOL.get_or_init(|| Mutex::new(WeakInterner::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .intern(value)
}

/// Remove weak entries whose values are no longer owned by messages or
/// aggregation buckets. Local aggregation calls this after public outputs have
/// been materialized, or after partial accumulators are dropped on error.
pub(crate) fn prune_dead() {
    POOL.get_or_init(|| Mutex::new(WeakInterner::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .prune_dead();
    #[cfg(test)]
    PRUNE_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
pub(crate) fn indexed_live_count(value: &str) -> usize {
    let pool = POOL.get_or_init(|| Mutex::new(WeakInterner::default()));
    let interner = pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    interner
        .buckets
        .get(&default_hash(value))
        .map_or(0, |bucket| {
            let values: &[Weak<str>] = match bucket {
                WeakBucket::One(value) => std::slice::from_ref(value),
                WeakBucket::Collisions(values) => values,
            };
            values
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|shared| shared.as_ref() == value)
                .count()
        })
}

#[cfg(test)]
pub(crate) fn prune_count() -> usize {
    PRUNE_COUNT.with(Cell::get)
}

/// Deserialize a string field through the interner (cache loads and JSON
/// parses go through here, so live corpus copies share allocations).
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

pub fn de_intern_btree_set<'de, D>(
    deserializer: D,
) -> Result<std::collections::BTreeSet<Arc<str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values =
        <std::collections::BTreeSet<std::borrow::Cow<'_, str>>>::deserialize(deserializer)?;
    Ok(values.into_iter().map(|value| intern(&value)).collect())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    use super::*;

    fn collide(_: &str) -> HashValue {
        7
    }

    #[test]
    fn shares_one_allocation_while_value_is_live() {
        let mut interner = WeakInterner::with_hasher(default_hash);
        let first = interner.intern("claude-fable-5");
        let second = interner.intern("claude-fable-5");
        assert!(Arc::ptr_eq(&first, &second));
        let other = interner.intern("claude-fable-5[1m]");
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn forced_hash_collision_does_not_alias_distinct_values() {
        let mut interner = WeakInterner::with_hasher(collide);
        let first = interner.intern("alpha");
        let second = interner.intern("beta");
        assert_eq!(first.as_ref(), "alpha");
        assert_eq!(second.as_ref(), "beta");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(interner.bucket_count(), 1);
        assert_eq!(interner.weak_count(), 2);
        assert_eq!(interner.collision_vector_count(), 1);
    }

    #[test]
    fn high_cardinality_unique_hashes_do_not_allocate_collision_vectors() {
        let mut interner = WeakInterner::with_hasher(default_hash);
        let values: Vec<_> = (0..4_096)
            .map(|index| interner.intern(&format!("unique-{index}")))
            .collect();

        assert_eq!(interner.bucket_count(), values.len());
        assert_eq!(interner.weak_count(), values.len());
        assert_eq!(interner.collision_vector_count(), 0);
    }

    #[test]
    fn prune_removes_a_fully_dead_bucket() {
        let mut interner = WeakInterner::with_hasher(default_hash);
        let values: Vec<_> = (0..4_096)
            .map(|index| interner.intern(&format!("temporary-{index}")))
            .collect();
        assert!(interner.capacity() >= interner.bucket_count());
        drop(values);
        interner.prune_dead();
        assert_eq!(interner.bucket_count(), 0);
        assert_eq!(interner.weak_count(), 0);
        assert_eq!(interner.capacity(), 0);
    }

    #[test]
    fn touched_collision_bucket_drops_dead_peer_and_keeps_live_peer() {
        let mut interner = WeakInterner::with_hasher(collide);
        let live = interner.intern("live");
        drop(interner.intern("dead"));

        let again = interner.intern("live");
        assert!(Arc::ptr_eq(&live, &again));
        assert_eq!(interner.bucket_count(), 1);
        assert_eq!(interner.weak_count(), 1);
        assert_eq!(interner.collision_vector_count(), 0);
    }

    #[test]
    fn prune_compacts_a_surviving_collision_bucket() {
        let mut interner = WeakInterner::with_hasher(collide);
        let live = interner.intern("live");
        let dead: Vec<_> = (0..1_024)
            .map(|index| interner.intern(&format!("dead-{index}")))
            .collect();
        drop(dead);

        interner.prune_dead();
        assert_eq!(interner.bucket_count(), 1);
        assert_eq!(interner.weak_count(), 1);
        assert_eq!(interner.collision_vector_count(), 0);
        assert!(Arc::ptr_eq(&live, &interner.intern("live")));
    }

    #[test]
    fn concurrent_requests_share_one_live_value() {
        const THREADS: usize = 8;
        let interner = Arc::new(Mutex::new(WeakInterner::with_hasher(default_hash)));
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let interner = Arc::clone(&interner);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    interner
                        .lock()
                        .expect("test interner mutex poisoned")
                        .intern("shared")
                })
            })
            .collect();
        let values: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("interner thread panicked"))
            .collect();

        assert!(values
            .iter()
            .skip(1)
            .all(|value| Arc::ptr_eq(&values[0], value)));
    }
}
