//! Collision-free internal identity keys used by report and TUI accumulators.

use std::{collections::HashSet, hash::Hash, sync::Arc};

use crate::{sessions, GroupBy, UnifiedMessage};

pub const UNKNOWN_WORKSPACE_LABEL: &str = "Unknown workspace";
const UNKNOWN_WORKSPACE_GROUP_KEY: &str = "\0unknown-workspace";

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

    pub(crate) fn extend(&mut self, other: Self) {
        if matches!(self, Self::Empty) {
            *self = other;
            return;
        }
        match other {
            Self::Empty => {}
            Self::One(value) => {
                self.insert(value);
            }
            Self::Many(values) => {
                for value in *values {
                    self.insert(value);
                }
            }
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

    fn legacy_group_key(&self) -> &str {
        match self {
            Self::Known(key) => key,
            Self::Unknown => UNKNOWN_WORKSPACE_GROUP_KEY,
        }
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

    /// Whether a different structured key can produce the same historical
    /// public key text. Delimiter-free composite components are injective by
    /// separator count; workspace length-prefixing is injective except for the
    /// deliberate Unknown sentinel and a literal workspace with that value.
    pub(crate) fn may_alias_legacy_key(&self) -> bool {
        match self {
            Self::Model(_) => false,
            Self::ClientModel { client, model } => client.contains(':') || model.contains(':'),
            Self::ClientProviderModel {
                client,
                provider,
                model,
            } => client.contains(':') || provider.contains(':') || model.contains(':'),
            Self::WorkspaceModel { workspace, .. } => match workspace {
                WorkspaceKey::Unknown => true,
                WorkspaceKey::Known(key) => key.as_ref() == UNKNOWN_WORKSPACE_GROUP_KEY,
            },
            Self::SessionModel { session, model } => session.contains(':') || model.contains(':'),
            Self::ClientSessionModel {
                client,
                session,
                model,
            } => client.contains(':') || session.contains(':') || model.contains(':'),
        }
    }

    /// Preserve the existing public/cache key text. This string is created at
    /// output materialization, never for a hot-path map lookup.
    pub(crate) fn public_key(&self) -> String {
        match self {
            Self::Model(model) => model.to_string(),
            Self::ClientModel { client, model } => format!("{client}:{model}"),
            Self::ClientProviderModel {
                client,
                provider,
                model,
            } => format!("{client}:{provider}:{model}"),
            Self::WorkspaceModel { workspace, model } => {
                let workspace = workspace.legacy_group_key();
                format!("{}:{workspace}:{model}", workspace.len())
            }
            Self::SessionModel { session, model } => format!("{session}:{model}"),
            Self::ClientSessionModel {
                client,
                session,
                model,
            } => format!("{client}:{session}:{model}"),
        }
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

    pub(crate) fn public_key(&self) -> String {
        match self {
            Self::Model(model) => model.to_string(),
            Self::ProviderModel { provider, model } => format!("{provider}:{model}"),
        }
    }

    pub(crate) fn may_alias_legacy_key(&self) -> bool {
        match self {
            Self::Model(_) => false,
            Self::ProviderModel { provider, model } => {
                provider.contains(':') || model.contains(':')
            }
        }
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
        assert_eq!(left.public_key(), right.public_key());
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
        assert_eq!(left.public_key(), right.public_key());
        assert!(left.may_alias_legacy_key());
        assert!(right.may_alias_legacy_key());
    }

    #[test]
    fn legacy_alias_classifier_separates_injective_and_ambiguous_keys() {
        let msg = message();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::Session,
            GroupBy::ClientSession,
        ] {
            assert!(!GroupedModelKey::from_message(&group_by, &msg).may_alias_legacy_key());
        }

        let mut ambiguous = message();
        ambiguous.session_id = Arc::from("session:child");
        assert!(GroupedModelKey::from_message(&GroupBy::Session, &ambiguous).may_alias_legacy_key());
        assert!(
            GroupedModelKey::from_message(&GroupBy::ClientSession, &ambiguous)
                .may_alias_legacy_key()
        );
        ambiguous = message();
        ambiguous.provider_id = Arc::from("provider:route");
        assert!(
            GroupedModelKey::from_message(&GroupBy::ClientProviderModel, &ambiguous)
                .may_alias_legacy_key()
        );

        let unknown = {
            let mut message = message();
            message.workspace_key = None;
            GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &message)
        };
        let literal_sentinel = {
            let mut message = message();
            message.workspace_key = Some(Arc::from(UNKNOWN_WORKSPACE_GROUP_KEY));
            GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &message)
        };
        let ordinary = GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &msg);
        assert!(unknown.may_alias_legacy_key());
        assert!(literal_sentinel.may_alias_legacy_key());
        assert!(!ordinary.may_alias_legacy_key());
        assert_eq!(unknown.public_key(), literal_sentinel.public_key());
        assert_ne!(unknown.public_key(), ordinary.public_key());
    }

    #[test]
    fn safe_and_risky_delimiter_keys_cannot_share_public_text() {
        let safe = message();
        let mut client_risky = message();
        client_risky.client = Arc::from("client:child");
        let mut provider_risky = message();
        provider_risky.provider_id = Arc::from("provider:route");
        let mut session_risky = message();
        session_risky.session_id = Arc::from("session:child");
        let cases = [
            (GroupBy::ClientModel, &client_risky, 1),
            (GroupBy::ClientProviderModel, &provider_risky, 2),
            (GroupBy::Session, &session_risky, 1),
            (GroupBy::ClientSession, &session_risky, 2),
        ];

        for (group_by, risky_message, separator_count) in cases {
            let safe_key = GroupedModelKey::from_message(&group_by, &safe);
            let risky_key = GroupedModelKey::from_message(&group_by, risky_message);
            assert!(!safe_key.may_alias_legacy_key());
            assert!(risky_key.may_alias_legacy_key());
            assert_eq!(safe_key.public_key().matches(':').count(), separator_count);
            assert!(risky_key.public_key().matches(':').count() > separator_count);
            assert_ne!(safe_key.public_key(), risky_key.public_key());
        }
    }

    #[test]
    fn unknown_workspace_does_not_alias_literal_legacy_sentinel() {
        let mut unknown = message();
        unknown.workspace_key = None;
        let mut literal = message();
        literal.workspace_key = Some(Arc::from(UNKNOWN_WORKSPACE_GROUP_KEY));

        assert_ne!(
            GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &unknown),
            GroupedModelKey::from_message(&GroupBy::WorkspaceModel, &literal)
        );
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
