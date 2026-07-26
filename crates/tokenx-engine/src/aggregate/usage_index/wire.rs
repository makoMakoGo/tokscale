use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime};
use serde::Deserialize;

use super::index::{
    AgentBucket, AgentInstanceKey, AgentKey, DailyBucket, FineModelBucket, FrozenUsageIndex,
    HourlyBucket, UsageTotalsBucket,
};
use crate::aggregate::keys::{FineModelKey, IdentitySet};
use crate::projection::UsageTokenBreakdown;
use crate::{ClientId, ClientUniverse, GroupBy};

/// Deserialization wire shape for [`FrozenUsageIndex`].
///
/// Keep this field order and each `map_as_vec` adapter aligned with the
/// persisted format. Conversion into the installed index is the single
/// deserialization boundary where all retained containers are compacted.
#[derive(Deserialize)]
pub(crate) struct FrozenUsageIndexWire {
    #[serde(with = "map_as_vec")]
    usage_totals_by_client: HashMap<ClientId, UsageTotalsBucket>,
    #[serde(with = "map_as_vec")]
    model_map: HashMap<FineModelKey, FineModelBucket>,
    #[serde(with = "map_as_vec")]
    agent_map: HashMap<AgentKey, AgentBucket>,
    #[serde(with = "map_as_vec")]
    daily_map: HashMap<NaiveDate, DailyBucket>,
    #[serde(with = "map_as_vec")]
    hourly_map: HashMap<NaiveDateTime, HourlyBucket>,
}

impl FrozenUsageIndexWire {
    pub(crate) fn into_index(self) -> FrozenUsageIndex {
        let mut index = FrozenUsageIndex {
            usage_totals_by_client: self.usage_totals_by_client,
            model_map: self.model_map,
            agent_map: self.agent_map,
            daily_map: self.daily_map,
            hourly_map: self.hourly_map,
        };
        index.shrink_to_fit();
        index
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsageIndexValidationError {
    #[error(
        "frozen usage index `{index}` contains client `{client}` outside its generation universe"
    )]
    IndexedClientOutsideUniverse {
        index: &'static str,
        client: ClientId,
    },
    #[error("frozen usage index `{index}` contains client `{actual}` under client `{expected}`")]
    ScopedClientMismatch {
        index: &'static str,
        expected: ClientId,
        actual: ClientId,
    },
    #[error("frozen usage index `{index}` has inconsistent token totals for client `{client}`")]
    TokenTotalsMismatch {
        index: &'static str,
        client: ClientId,
    },
    #[error("frozen usage index `{index}` has overflowing token totals for client `{client}`")]
    TokenTotalsOverflow {
        index: &'static str,
        client: ClientId,
    },
    #[error(
        "frozen usage index `{index}` has a bucket whose stored identity differs from its key"
    )]
    BucketIdentityMismatch { index: &'static str },
    #[error("frozen usage index `{index}` contains a {kind} cost for client `{client}`")]
    InvalidCost {
        index: &'static str,
        client: ClientId,
        kind: InvalidCostKind,
    },
    #[error(
        "frozen usage index cannot safely project `{group_by:?}` while accumulating `{field}`"
    )]
    ProjectionOverflow {
        group_by: GroupBy,
        field: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCostKind {
    NonFinite,
    Negative,
}

impl std::fmt::Display for InvalidCostKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("non-finite"),
            Self::Negative => formatter.write_str("negative"),
        }
    }
}

fn token_breakdowns_match(left: &UsageTokenBreakdown, right: &UsageTokenBreakdown) -> bool {
    left.input == right.input
        && left.output == right.output
        && left.cache_read == right.cache_read
        && left.cache_write == right.cache_write
        && left.reasoning == right.reasoning
}

fn checked_add_for_validation(
    target: &mut UsageTokenBreakdown,
    addition: &UsageTokenBreakdown,
    index: &'static str,
    client: ClientId,
) -> Result<(), UsageIndexValidationError> {
    addition
        .checked_total()
        .ok_or(UsageIndexValidationError::TokenTotalsOverflow { index, client })?;
    *target = target
        .checked_add(addition)
        .ok_or(UsageIndexValidationError::TokenTotalsOverflow { index, client })?;
    Ok(())
}

fn ensure_indexed_client(
    universe: &ClientUniverse,
    index: &'static str,
    client: ClientId,
) -> Result<(), UsageIndexValidationError> {
    if universe.contains(client) {
        Ok(())
    } else {
        Err(UsageIndexValidationError::IndexedClientOutsideUniverse { index, client })
    }
}

fn validate_cost(
    index: &'static str,
    client: ClientId,
    cost: f64,
) -> Result<(), UsageIndexValidationError> {
    let kind = if !cost.is_finite() {
        Some(InvalidCostKind::NonFinite)
    } else if cost < 0.0 {
        Some(InvalidCostKind::Negative)
    } else {
        None
    };
    match kind {
        Some(kind) => Err(UsageIndexValidationError::InvalidCost {
            index,
            client,
            kind,
        }),
        None => Ok(()),
    }
}

pub(super) mod map_as_vec {
    use std::{collections::HashMap, fmt, hash::Hash, marker::PhantomData};

    use serde::{
        de::{SeqAccess, Visitor},
        ser::SerializeSeq,
        Deserialize, Deserializer, Serialize, Serializer,
    };

    pub(in crate::aggregate::usage_index) const MAX_INITIAL_CAPACITY: usize = 16 * 1024;

    pub(in crate::aggregate::usage_index) fn initial_capacity(size_hint: Option<usize>) -> usize {
        // A corrupt binary length prefix reaches `size_hint` before the
        // deserializer's byte limit can validate that the entries exist.
        size_hint.unwrap_or_default().min(MAX_INITIAL_CAPACITY)
    }

    pub(in crate::aggregate::usage_index) fn serialize<K, V, S>(
        map: &HashMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        K: Serialize,
        V: Serialize,
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(map.len()))?;
        for (key, value) in map {
            sequence.serialize_element(&(key, value))?;
        }
        sequence.end()
    }

    pub(in crate::aggregate::usage_index) fn deserialize<'de, K, V, D>(
        deserializer: D,
    ) -> Result<HashMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        struct MapAsVecVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for MapAsVecVisitor<K, V>
        where
            K: Deserialize<'de> + Eq + Hash,
            V: Deserialize<'de>,
        {
            type Value = HashMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of canonical usage-index key-value pairs")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut map = HashMap::with_capacity(initial_capacity(sequence.size_hint()));
                while let Some((key, value)) = sequence.next_element()? {
                    if map.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom(
                            "duplicate key in canonical usage-index map",
                        ));
                    }
                }
                Ok(map)
            }
        }

        deserializer.deserialize_seq(MapAsVecVisitor(PhantomData))
    }
}

impl FrozenUsageIndex {
    /// Validate persisted index semantics against the generation that owns it.
    ///
    /// This checks every stored client identity and the additive token
    /// relationships that can be proven from the materialized indexes without
    /// retaining a second raw-message table.
    pub fn validate(&self, universe: &ClientUniverse) -> Result<(), UsageIndexValidationError> {
        const CLIENT_TOTALS: &str = "usage_totals_by_client";
        const MODELS: &str = "model_map";
        const AGENTS: &str = "agent_map";
        const DAILY: &str = "daily_map";
        const DAILY_MODELS: &str = "daily_map.models";
        const HOURLY: &str = "hourly_map";
        const HOURLY_MODELS: &str = "hourly_map.models";

        for (client, totals) in &self.usage_totals_by_client {
            ensure_indexed_client(universe, CLIENT_TOTALS, *client)?;
            validate_cost(CLIENT_TOTALS, *client, totals.cost)?;
            totals.tokens.checked_total().ok_or(
                UsageIndexValidationError::TokenTotalsOverflow {
                    index: CLIENT_TOTALS,
                    client: *client,
                },
            )?;
        }

        let mut model_totals_by_client = HashMap::new();
        for (key, model) in &self.model_map {
            ensure_indexed_client(universe, MODELS, key.client)?;
            validate_cost(MODELS, key.client, model.cost)?;
            checked_add_for_validation(
                model_totals_by_client.entry(key.client).or_default(),
                &model.tokens,
                MODELS,
                key.client,
            )?;
        }
        for (client, totals) in &self.usage_totals_by_client {
            let Some(model_totals) = model_totals_by_client.remove(client) else {
                return Err(UsageIndexValidationError::TokenTotalsMismatch {
                    index: MODELS,
                    client: *client,
                });
            };
            if !token_breakdowns_match(&totals.tokens, &model_totals) {
                return Err(UsageIndexValidationError::TokenTotalsMismatch {
                    index: MODELS,
                    client: *client,
                });
            }
        }
        if let Some((client, _)) = model_totals_by_client.into_iter().next() {
            return Err(UsageIndexValidationError::TokenTotalsMismatch {
                index: MODELS,
                client,
            });
        }

        for (key, agent) in &self.agent_map {
            ensure_indexed_client(universe, AGENTS, key.client)?;
            validate_cost(AGENTS, key.client, agent.cost)?;
            agent
                .tokens
                .checked_total()
                .ok_or(UsageIndexValidationError::TokenTotalsOverflow {
                    index: AGENTS,
                    client: key.client,
                })?;
            let validate_instance =
                |instance: &AgentInstanceKey| -> Result<(), UsageIndexValidationError> {
                    let AgentInstanceKey::Derived { client, .. } = instance else {
                        return Ok(());
                    };
                    ensure_indexed_client(universe, AGENTS, *client)?;
                    if *client != key.client {
                        return Err(UsageIndexValidationError::ScopedClientMismatch {
                            index: AGENTS,
                            expected: key.client,
                            actual: *client,
                        });
                    }
                    Ok(())
                };
            match &agent.instances {
                IdentitySet::Empty => {}
                IdentitySet::One(instance) => validate_instance(instance)?,
                IdentitySet::Many(instances) => {
                    for instance in instances.iter() {
                        validate_instance(instance)?;
                    }
                }
            }
        }

        for (date, daily) in &self.daily_map {
            if *date != daily.date {
                return Err(UsageIndexValidationError::BucketIdentityMismatch { index: DAILY });
            }
            for (client, client_bucket) in &daily.clients {
                ensure_indexed_client(universe, DAILY, *client)?;
                validate_cost(DAILY, *client, client_bucket.cost)?;
                client_bucket.tokens.checked_total().ok_or(
                    UsageIndexValidationError::TokenTotalsOverflow {
                        index: DAILY,
                        client: *client,
                    },
                )?;
                let mut model_totals = UsageTokenBreakdown::default();
                for (model_key, model) in &client_bucket.models {
                    ensure_indexed_client(universe, DAILY_MODELS, model_key.client)?;
                    if model_key.client != *client {
                        return Err(UsageIndexValidationError::ScopedClientMismatch {
                            index: DAILY_MODELS,
                            expected: *client,
                            actual: model_key.client,
                        });
                    }
                    validate_cost(DAILY_MODELS, *client, model.cost)?;
                    checked_add_for_validation(
                        &mut model_totals,
                        &model.tokens,
                        DAILY_MODELS,
                        *client,
                    )?;
                }
                if !token_breakdowns_match(&client_bucket.tokens, &model_totals) {
                    return Err(UsageIndexValidationError::TokenTotalsMismatch {
                        index: DAILY_MODELS,
                        client: *client,
                    });
                }
            }
        }

        for (datetime, hourly) in &self.hourly_map {
            if *datetime != hourly.datetime {
                return Err(UsageIndexValidationError::BucketIdentityMismatch { index: HOURLY });
            }
            for (client, client_bucket) in &hourly.clients {
                ensure_indexed_client(universe, HOURLY, *client)?;
                validate_cost(HOURLY, *client, client_bucket.cost)?;
                client_bucket.tokens.checked_total().ok_or(
                    UsageIndexValidationError::TokenTotalsOverflow {
                        index: HOURLY,
                        client: *client,
                    },
                )?;
                let mut model_totals = UsageTokenBreakdown::default();
                for model in client_bucket.models.values() {
                    validate_cost(HOURLY_MODELS, *client, model.cost)?;
                    checked_add_for_validation(
                        &mut model_totals,
                        &model.tokens,
                        HOURLY_MODELS,
                        *client,
                    )?;
                }
                if !token_breakdowns_match(&client_bucket.tokens, &model_totals) {
                    return Err(UsageIndexValidationError::TokenTotalsMismatch {
                        index: HOURLY_MODELS,
                        client: *client,
                    });
                }
            }
        }

        self.validate_projection_arithmetic().map_err(|error| {
            UsageIndexValidationError::ProjectionOverflow {
                group_by: GroupBy::Model,
                field: error.field(),
            }
        })?;

        Ok(())
    }
}
