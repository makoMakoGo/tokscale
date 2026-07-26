//! Collision-free internal identity keys used by canonical usage accumulators.

use std::{collections::HashSet, hash::Hash, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{records, AttributedUsageRecord, ClientId, GroupBy};

pub const UNKNOWN_WORKSPACE_LABEL: &str = "Unknown workspace";

/// Allocation-free for the empty and singleton cases; a hash table is
/// created only when a second distinct identity is actually observed.
#[derive(Default, Serialize, Deserialize)]
#[serde(bound(
    serialize = "T: Serialize",
    deserialize = "T: Deserialize<'de> + Eq + Hash"
))]
pub(crate) enum IdentitySet<T> {
    #[default]
    Empty,
    One(T),
    // Indirection keeps the allocation-free singleton representation small;
    // the collection exists only after a second distinct identity appears.
    #[allow(clippy::box_collection)]
    Many(Box<HashSet<T>>),
}

impl<T> IdentitySet<T>
where
    T: Eq + Hash,
{
    pub(crate) fn insert(&mut self, value: T) -> bool {
        match self {
            Self::Empty => {
                *self = Self::One(value);
                true
            }
            Self::One(existing) if *existing == value => false,
            Self::One(_) => {
                let Self::One(existing) = std::mem::replace(self, Self::Empty) else {
                    unreachable!("identity set singleton replaced atomically")
                };
                *self = Self::Many(Box::new(HashSet::from([existing, value])));
                true
            }
            Self::Many(values) => values.insert(value),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Many(values) => values.len(),
        }
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        if let Self::Many(values) = self {
            values.shrink_to_fit();
        }
    }
}

impl IdentitySet<Arc<str>> {
    pub(crate) fn to_sorted_arc(&self) -> Arc<str> {
        match self {
            Self::Empty => panic!("identity set must contain a value before materialization"),
            Self::One(value) => Arc::clone(value),
            Self::Many(values) => {
                let mut values: Vec<_> = values.iter().collect();
                values.sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
                let value_bytes = values.iter().fold(0_usize, |total, value| {
                    total
                        .checked_add(value.as_ref().len())
                        .expect("joined identity length exceeds usize::MAX")
                });
                let separator_bytes = values
                    .len()
                    .checked_sub(1)
                    .expect("multi-identity set contains at least two values")
                    .checked_mul(2)
                    .expect("joined identity separator length exceeds usize::MAX");
                let capacity = value_bytes
                    .checked_add(separator_bytes)
                    .expect("joined identity length exceeds usize::MAX");
                let mut output = String::with_capacity(capacity);
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push_str(", ");
                    }
                    output.push_str(value.as_ref());
                }
                records::intern::intern(&output)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) enum WorkspaceKey {
    Known(#[serde(deserialize_with = "crate::records::intern::de_intern")] Arc<str>),
    Unknown,
}

impl WorkspaceKey {
    fn from_message(msg: &AttributedUsageRecord) -> Self {
        msg.workspace_key
            .as_ref()
            .map_or(Self::Unknown, |key| Self::Known(Arc::clone(key)))
    }

    /// The DTO workspace key: `Some` for a known workspace, `None` for the
    /// unknown bucket.
    pub(crate) fn to_key(&self) -> Option<Arc<str>> {
        match self {
            Self::Known(key) => Some(Arc::clone(key)),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum GroupedModelKey {
    Model(Arc<str>),
    ClientModel {
        client: ClientId,
        model: Arc<str>,
    },
    ClientProviderModel {
        client: ClientId,
        provider: Arc<str>,
        model: Arc<str>,
    },
    WorkspaceModel {
        workspace: WorkspaceKey,
        model: Arc<str>,
    },
}

impl GroupedModelKey {
    /// Match the grouping mode before touching unrelated identity fields.
    #[cfg(test)]
    pub(crate) fn from_message(group_by: &GroupBy, msg: &AttributedUsageRecord) -> Self {
        match group_by {
            GroupBy::Model => Self::Model(Arc::clone(&msg.model_id)),
            GroupBy::ClientModel => Self::ClientModel {
                client: msg.client,
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::ClientProviderModel => Self::ClientProviderModel {
                client: msg.client,
                provider: Arc::clone(&msg.provider_id),
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::WorkspaceModel => Self::WorkspaceModel {
                workspace: WorkspaceKey::from_message(msg),
                model: Arc::clone(&msg.model_id),
            },
        }
    }

    pub(crate) fn merges_clients(&self) -> bool {
        matches!(self, Self::Model(_) | Self::WorkspaceModel { .. })
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum HourlyModelKey {
    Model(Arc<str>),
    ProviderModel { provider: Arc<str>, model: Arc<str> },
}

/// Finest-granularity model identity for every public `GroupBy` dimension,
/// plus session identity so projections retain an exact distinct-session
/// count. The usage accumulator keys its canonical buckets with this so
/// switching groupings is an in-memory re-fold instead of a rescan.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct FineModelKey {
    pub(crate) client: ClientId,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(crate) provider: Arc<str>,
    pub(crate) workspace: WorkspaceKey,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(crate) session: Arc<str>,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(crate) model: Arc<str>,
}

impl FineModelKey {
    pub(crate) fn from_message(msg: &AttributedUsageRecord) -> Self {
        Self {
            client: msg.client,
            provider: Arc::clone(&msg.provider_id),
            workspace: WorkspaceKey::from_message(msg),
            session: Arc::clone(&msg.session_id),
            model: Arc::clone(&msg.model_id),
        }
    }

    /// Collapse to one grouping's bucket identity.
    pub(crate) fn grouped(&self, group_by: &GroupBy) -> GroupedModelKey {
        match group_by {
            GroupBy::Model => GroupedModelKey::Model(Arc::clone(&self.model)),
            GroupBy::ClientModel => GroupedModelKey::ClientModel {
                client: self.client,
                model: Arc::clone(&self.model),
            },
            GroupBy::ClientProviderModel => GroupedModelKey::ClientProviderModel {
                client: self.client,
                provider: Arc::clone(&self.provider),
                model: Arc::clone(&self.model),
            },
            GroupBy::WorkspaceModel => GroupedModelKey::WorkspaceModel {
                workspace: self.workspace.clone(),
                model: Arc::clone(&self.model),
            },
        }
    }
}

/// Finest-granularity hourly model identity: `(provider, model)`. Only the
/// ClientProviderModel grouping keeps the provider split; every other
/// grouping re-folds providers back into the bare model.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct FineHourlyModelKey {
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(crate) provider: Arc<str>,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(crate) model: Arc<str>,
}

impl FineHourlyModelKey {
    pub(crate) fn from_message(msg: &AttributedUsageRecord) -> Self {
        Self {
            provider: Arc::clone(&msg.provider_id),
            model: Arc::clone(&msg.model_id),
        }
    }

    /// Collapse to one grouping's hourly bucket identity.
    pub(crate) fn grouped(&self, group_by: &GroupBy) -> HourlyModelKey {
        if *group_by == GroupBy::ClientProviderModel {
            HourlyModelKey::ProviderModel {
                provider: Arc::clone(&self.provider),
                model: Arc::clone(&self.model),
            }
        } else {
            HourlyModelKey::Model(Arc::clone(&self.model))
        }
    }
}

/// Workspace DTO fields are derived only while creating a workspace bucket.
pub(crate) fn workspace_fields(msg: &AttributedUsageRecord) -> (Option<Arc<str>>, Arc<str>) {
    match (&msg.workspace_key, &msg.workspace_label) {
        (Some(key), Some(label)) => (Some(Arc::clone(key)), Arc::clone(label)),
        (Some(key), None) => (
            Some(Arc::clone(key)),
            records::workspace_label_from_key(key)
                .as_deref()
                .map(records::intern::intern)
                .unwrap_or_else(|| records::intern::intern(UNKNOWN_WORKSPACE_LABEL)),
        ),
        _ => (None, records::intern::intern(UNKNOWN_WORKSPACE_LABEL)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{ClientId, TokenBreakdown};

    fn message() -> AttributedUsageRecord {
        let mut message = AttributedUsageRecord::new(
            ClientId::Codex,
            "model",
            "provider",
            "session",
            0,
            TokenBreakdown::default(),
            0.0,
        );
        message.set_workspace(Some("workspace".to_string()), Some("Workspace".to_string()));
        message
    }

    #[test]
    fn creates_all_grouping_variants() {
        let msg = message();
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::Model, &msg),
            GroupedModelKey::Model(_)
        ));
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::ClientModel, &msg),
            GroupedModelKey::ClientModel { .. }
        ));
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::ClientProviderModel, &msg),
            GroupedModelKey::ClientProviderModel { .. }
        ));
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &msg),
            GroupedModelKey::WorkspaceModel { .. }
        ));
    }

    #[test]
    fn structured_keys_include_typed_client_identity() {
        let mut left = message();
        left.client = ClientId::Amp;
        left.model_id = Arc::from("c");
        let mut right = message();
        right.client = ClientId::Codex;
        right.model_id = Arc::from("c");

        let left = GroupedModelKey::from_message(&GroupBy::ClientModel, &left);
        let right = GroupedModelKey::from_message(&GroupBy::ClientModel, &right);
        assert_ne!(left, right);
    }

    #[test]
    fn model_keys_compare_string_values_instead_of_arc_addresses() {
        let mut left = message();
        left.model_id = Arc::from(String::from("same-model"));
        let mut right = message();
        right.model_id = Arc::from(String::from("same-model"));
        assert!(!Arc::ptr_eq(&left.model_id, &right.model_id));

        assert_eq!(
            GroupedModelKey::from_message(&GroupBy::ClientModel, &left),
            GroupedModelKey::from_message(&GroupBy::ClientModel, &right)
        );
    }

    #[test]
    fn fine_model_key_persists_client_as_stable_catalog_id() {
        let key = FineModelKey::from_message(&message());
        let encoded = serde_json::to_value(&key).unwrap();
        assert_eq!(encoded["client"], "codex");

        let restored: FineModelKey = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored.client, ClientId::Codex);
    }

    #[test]
    fn hourly_structured_keys_do_not_alias_separator_collisions() {
        let mut left = message();
        left.provider_id = Arc::from("a:b");
        left.model_id = Arc::from("c");
        let mut right = message();
        right.provider_id = Arc::from("a");
        right.model_id = Arc::from("b:c");

        let left = FineHourlyModelKey::from_message(&left).grouped(&GroupBy::ClientProviderModel);
        let right = FineHourlyModelKey::from_message(&right).grouped(&GroupBy::ClientProviderModel);
        assert_ne!(left, right);
    }

    #[test]
    fn unknown_workspace_has_a_distinct_variant_tag() {
        let mut unknown = message();
        unknown.workspace_key = None;
        let mut literal = message();
        literal.workspace_key = Some(Arc::from(""));

        let unknown = GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &unknown);
        let literal = GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &literal);
        assert_ne!(unknown, literal);
    }

    #[test]
    fn unrelated_grouping_does_not_clone_workspace_or_session() {
        let mut msg = message();
        msg.workspace_key = Some(Arc::from(String::from("unique-workspace")));
        msg.session_id = Arc::from(String::from("unique-session"));
        let workspace = Arc::clone(msg.workspace_key.as_ref().expect("workspace"));
        let session = Arc::clone(&msg.session_id);
        let workspace_before = Arc::strong_count(&workspace);
        let session_before = Arc::strong_count(&session);

        let key = GroupedModelKey::from_message(&GroupBy::Model, &msg);
        assert_eq!(Arc::strong_count(&workspace), workspace_before);
        assert_eq!(Arc::strong_count(&session), session_before);
        drop(key);
    }

    #[test]
    fn identity_set_allocates_a_hash_table_only_for_distinct_peers() {
        let mut identities = IdentitySet::default();
        assert!(identities.insert(Arc::<str>::from("one")));
        assert!(matches!(identities, IdentitySet::One(_)));
        assert!(!identities.insert(Arc::<str>::from("one")));
        assert!(matches!(identities, IdentitySet::One(_)));
        assert!(identities.insert(Arc::<str>::from("two")));
        assert!(matches!(identities, IdentitySet::Many(_)));
        assert_eq!(identities.len(), 2);

        assert!(std::mem::size_of::<IdentitySet<Arc<str>>>() <= 3 * std::mem::size_of::<usize>());
    }
}
