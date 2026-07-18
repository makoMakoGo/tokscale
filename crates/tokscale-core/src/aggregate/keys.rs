//! Collision-free internal identity keys used by report and TUI accumulators.

use std::{collections::HashSet, hash::Hash, sync::Arc};

use crate::{sessions, GroupBy, UnifiedMessage};

pub const UNKNOWN_WORKSPACE_LABEL: &str = "Unknown workspace";

fn push_len_prefixed(output: &mut String, value: &str) {
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
}

const STORAGE_KEY_VERSION: &str = "v1|";

/// Allocation-free for the empty and singleton cases; a hash table is
/// created only when a second distinct identity is actually observed.
#[derive(Default)]
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
    pub(crate) fn one(value: T) -> Self {
        Self::One(value)
    }

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

    pub(crate) fn into_vec(self) -> Vec<T> {
        match self {
            Self::Empty => panic!("identity set must contain a value before materialization"),
            Self::One(value) => vec![value],
            Self::Many(values) => (*values).into_iter().collect(),
        }
    }
}

impl<T> IdentitySet<T>
where
    T: AsRef<str> + Eq + Hash,
{
    pub(crate) fn into_sorted_string(self) -> String {
        match self {
            Self::Empty => panic!("identity set must contain a value before materialization"),
            Self::One(value) => value.as_ref().to_owned(),
            Self::Many(values) => {
                let mut values: Vec<_> = (*values).into_iter().collect();
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
                output
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum WorkspaceKey {
    Known(Arc<str>),
    Unknown,
}

impl WorkspaceKey {
    fn from_message(msg: &UnifiedMessage) -> Self {
        msg.workspace_key
            .as_ref()
            .map_or(Self::Unknown, |key| Self::Known(Arc::clone(key)))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum GroupedModelKey {
    Model(Arc<str>),
    ClientModel {
        client: Arc<str>,
        model: Arc<str>,
    },
    ClientProviderModel {
        client: Arc<str>,
        provider: Arc<str>,
        model: Arc<str>,
    },
    WorkspaceModel {
        workspace: WorkspaceKey,
        model: Arc<str>,
    },
    SessionModel {
        session: Arc<str>,
        model: Arc<str>,
    },
    ClientSessionModel {
        client: Arc<str>,
        session: Arc<str>,
        model: Arc<str>,
    },
}

impl GroupedModelKey {
    /// Match the grouping mode before touching unrelated identity fields.
    pub(crate) fn from_message(group_by: &GroupBy, msg: &UnifiedMessage) -> Self {
        match group_by {
            GroupBy::Model => Self::Model(Arc::clone(&msg.model_id)),
            GroupBy::ClientModel => Self::ClientModel {
                client: Arc::clone(&msg.client),
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::ClientProviderModel => Self::ClientProviderModel {
                client: Arc::clone(&msg.client),
                provider: Arc::clone(&msg.provider_id),
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::WorkspaceModel => Self::WorkspaceModel {
                workspace: WorkspaceKey::from_message(msg),
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::Session => Self::SessionModel {
                session: Arc::clone(&msg.session_id),
                model: Arc::clone(&msg.model_id),
            },
            GroupBy::ClientSession => Self::ClientSessionModel {
                client: Arc::clone(&msg.client),
                session: Arc::clone(&msg.session_id),
                model: Arc::clone(&msg.model_id),
            },
        }
    }

    pub(crate) fn merges_clients(&self) -> bool {
        matches!(self, Self::Model(_) | Self::WorkspaceModel { .. })
    }

    /// Stable, collision-free key for DTO maps that cannot retain the
    /// structured enum directly. Every component is byte-length-prefixed and
    /// each enum variant, including known versus unknown workspace, has its
    /// own tag.
    pub(crate) fn map_key(&self) -> String {
        let mut output = String::from(STORAGE_KEY_VERSION);
        match self {
            Self::Model(model) => {
                output.push_str("m|");
                push_len_prefixed(&mut output, model);
            }
            Self::ClientModel { client, model } => {
                output.push_str("cm|");
                push_len_prefixed(&mut output, client);
                push_len_prefixed(&mut output, model);
            }
            Self::ClientProviderModel {
                client,
                provider,
                model,
            } => {
                output.push_str("cpm|");
                push_len_prefixed(&mut output, client);
                push_len_prefixed(&mut output, provider);
                push_len_prefixed(&mut output, model);
            }
            Self::WorkspaceModel { workspace, model } => {
                match workspace {
                    WorkspaceKey::Known(workspace) => {
                        output.push_str("wmk|");
                        push_len_prefixed(&mut output, workspace);
                    }
                    WorkspaceKey::Unknown => output.push_str("wmu|"),
                }
                push_len_prefixed(&mut output, model);
            }
            Self::SessionModel { session, model } => {
                output.push_str("sm|");
                push_len_prefixed(&mut output, session);
                push_len_prefixed(&mut output, model);
            }
            Self::ClientSessionModel {
                client,
                session,
                model,
            } => {
                output.push_str("csm|");
                push_len_prefixed(&mut output, client);
                push_len_prefixed(&mut output, session);
                push_len_prefixed(&mut output, model);
            }
        }
        output
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum HourlyModelKey {
    Model(Arc<str>),
    ProviderModel { provider: Arc<str>, model: Arc<str> },
}

impl HourlyModelKey {
    pub(crate) fn from_message(group_by: &GroupBy, msg: &UnifiedMessage) -> Self {
        if *group_by == GroupBy::ClientProviderModel {
            Self::ProviderModel {
                provider: Arc::clone(&msg.provider_id),
                model: Arc::clone(&msg.model_id),
            }
        } else {
            Self::Model(Arc::clone(&msg.model_id))
        }
    }

    pub(crate) fn map_key(&self) -> String {
        let mut output = String::from(STORAGE_KEY_VERSION);
        match self {
            Self::Model(model) => {
                output.push_str("m|");
                push_len_prefixed(&mut output, model);
            }
            Self::ProviderModel { provider, model } => {
                output.push_str("pm|");
                push_len_prefixed(&mut output, provider);
                push_len_prefixed(&mut output, model);
            }
        }
        output
    }
}

/// Workspace DTO fields are derived only while creating a workspace bucket.
pub(crate) fn workspace_fields(msg: &UnifiedMessage) -> (Option<Arc<str>>, Arc<str>) {
    match (&msg.workspace_key, &msg.workspace_label) {
        (Some(key), Some(label)) => (Some(Arc::clone(key)), Arc::clone(label)),
        (Some(key), None) => (
            Some(Arc::clone(key)),
            sessions::workspace_label_from_key(key)
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(UNKNOWN_WORKSPACE_LABEL)),
        ),
        _ => (None, Arc::from(UNKNOWN_WORKSPACE_LABEL)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::TokenBreakdown;

    fn message() -> UnifiedMessage {
        UnifiedMessage {
            client: Arc::from("client"),
            model_id: Arc::from("model"),
            provider_id: Arc::from("provider"),
            session_id: Arc::from("session"),
            is_main_session: true,
            workspace_key: Some(Arc::from("workspace")),
            workspace_label: Some(Arc::from("Workspace")),
            timestamp: 0,
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            duration_ms: None,
            message_count: 1,
            agent: None,
            agent_instance: None,
            dedup_key: None,
            is_turn_start: false,
        }
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
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::Session, &msg),
            GroupedModelKey::SessionModel { .. }
        ));
        assert!(matches!(
            GroupedModelKey::from_message(&GroupBy::ClientSession, &msg),
            GroupedModelKey::ClientSessionModel { .. }
        ));
    }

    #[test]
    fn structured_keys_do_not_alias_separator_collisions() {
        let mut left = message();
        left.client = Arc::from("a:b");
        left.model_id = Arc::from("c");
        let mut right = message();
        right.client = Arc::from("a");
        right.model_id = Arc::from("b:c");

        let left = GroupedModelKey::from_message(&GroupBy::ClientModel, &left);
        let right = GroupedModelKey::from_message(&GroupBy::ClientModel, &right);
        assert_ne!(left, right);
        assert_ne!(left.map_key(), right.map_key());
        assert_eq!(left.map_key(), "v1|cm|3:a:b1:c");
        assert_eq!(right.map_key(), "v1|cm|1:a3:b:c");
    }

    #[test]
    fn keys_compare_string_values_instead_of_arc_addresses() {
        let mut left = message();
        left.client = Arc::from(String::from("same-client"));
        left.model_id = Arc::from(String::from("same-model"));
        let mut right = message();
        right.client = Arc::from(String::from("same-client"));
        right.model_id = Arc::from(String::from("same-model"));
        assert!(!Arc::ptr_eq(&left.client, &right.client));
        assert!(!Arc::ptr_eq(&left.model_id, &right.model_id));

        assert_eq!(
            GroupedModelKey::from_message(&GroupBy::ClientModel, &left),
            GroupedModelKey::from_message(&GroupBy::ClientModel, &right)
        );
    }

    #[test]
    fn hourly_structured_keys_do_not_alias_separator_collisions() {
        let mut left = message();
        left.provider_id = Arc::from("a:b");
        left.model_id = Arc::from("c");
        let mut right = message();
        right.provider_id = Arc::from("a");
        right.model_id = Arc::from("b:c");

        let left = HourlyModelKey::from_message(&GroupBy::ClientProviderModel, &left);
        let right = HourlyModelKey::from_message(&GroupBy::ClientProviderModel, &right);
        assert_ne!(left, right);
        assert_ne!(left.map_key(), right.map_key());
        assert_eq!(left.map_key(), "v1|pm|3:a:b1:c");
        assert_eq!(right.map_key(), "v1|pm|1:a3:b:c");
    }

    #[test]
    fn map_keys_tag_every_grouping_variant() {
        let msg = message();
        let cases = [
            (GroupBy::Model, "v1|m|"),
            (GroupBy::ClientModel, "v1|cm|"),
            (GroupBy::ClientProviderModel, "v1|cpm|"),
            (GroupBy::WorkspaceModel, "v1|wmk|"),
            (GroupBy::Session, "v1|sm|"),
            (GroupBy::ClientSession, "v1|csm|"),
        ];
        for (group_by, prefix) in cases {
            assert!(GroupedModelKey::from_message(&group_by, &msg)
                .map_key()
                .starts_with(prefix));
        }
    }

    #[test]
    fn map_key_lengths_count_utf8_bytes() {
        let mut msg = message();
        msg.model_id = Arc::from("雪");

        assert_eq!(
            GroupedModelKey::from_message(&GroupBy::Model, &msg).map_key(),
            "v1|m|3:雪"
        );
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
        assert!(unknown.map_key().starts_with("v1|wmu|"));
        assert!(literal.map_key().starts_with("v1|wmk|"));
        assert_ne!(unknown.map_key(), literal.map_key());
    }

    #[test]
    fn unrelated_grouping_does_not_clone_workspace_or_session() {
        let msg = message();
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
