use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use super::queries::{
    build_contribution_graph_for_today, calculate_streaks_for_today, try_add_projection_cost,
    try_add_projection_tokens,
};
use crate::aggregate::{UsageAggregationError, UsageProjectionError};
use crate::projection::{
    AgentEntry, DailyClientInfo, DailyModelInfo, DailyUsage, HourlyModelInfo, HourlyUsage,
    ModelProjection, UsageModelEntry, UsageProjection, UsageTokenBreakdown,
};
use crate::{
    aggregate::keys::{
        workspace_fields, FineHourlyModelKey, FineModelKey, GroupedModelKey, HourlyModelKey,
        IdentitySet,
    },
    AttributedUsageRecord, ClientId, GroupBy,
};

/// Costs cannot represent debt, but a non-finite value is corrupted data and
/// must cross the typed aggregation boundary instead of disappearing as zero.
fn normalized_cost(cost: f64) -> Result<f64, UsageAggregationError> {
    if !cost.is_finite() {
        return Err(UsageAggregationError::new("record cost"));
    }
    Ok(cost.max(0.0))
}

fn add_aggregation_cost(
    target: &mut f64,
    addition: f64,
    field: &'static str,
) -> Result<(), UsageAggregationError> {
    let updated = *target + addition;
    if !updated.is_finite() {
        return Err(UsageAggregationError::new(field));
    }
    *target = updated;
    Ok(())
}

fn add_record_tokens(
    target: &mut UsageTokenBreakdown,
    src: &crate::TokenBreakdown,
) -> Result<(), UsageAggregationError> {
    let addition = UsageTokenBreakdown {
        input: src.input.max(0) as u64,
        output: src.output.max(0) as u64,
        cache_read: src.cache_read.max(0) as u64,
        cache_write: src.cache_write.max(0) as u64,
        reasoning: src.reasoning.max(0) as u64,
    };
    let updated = target
        .checked_add(&addition)
        .ok_or_else(|| UsageAggregationError::new("usage token buckets"))?;
    updated
        .checked_total()
        .ok_or_else(|| UsageAggregationError::new("usage token total"))?;
    *target = updated;
    Ok(())
}

fn check_record_tokens(
    existing: Option<&UsageTokenBreakdown>,
    src: &crate::TokenBreakdown,
) -> Result<(), UsageAggregationError> {
    let mut staged = existing.cloned().unwrap_or_default();
    add_record_tokens(&mut staged, src)
}

fn check_aggregation_cost(
    existing: Option<f64>,
    addition: f64,
    field: &'static str,
) -> Result<(), UsageAggregationError> {
    let mut staged = existing.unwrap_or_default();
    add_aggregation_cost(&mut staged, addition, field)
}

fn record_count_u32(value: i32, field: &'static str) -> Result<u32, UsageAggregationError> {
    u32::try_from(value.max(0)).map_err(|_| UsageAggregationError::new(field))
}

#[cfg(test)]
pub(super) fn test_calendar_fields(timestamp_ms: i64) -> Option<(NaiveDate, NaiveDateTime)> {
    crate::CalendarContext::explicit("UTC")
        .expect("UTC is a valid test calendar")
        .local_date_and_hour(timestamp_ms)
}

#[cfg(test)]
/// Test-only explicit UTC conversion for direct builder fixtures.
pub(super) fn timestamp_to_hour(timestamp_ms: i64) -> Option<NaiveDateTime> {
    test_calendar_fields(timestamp_ms).map(|(_, hour)| hour)
}

/// Write-only lifecycle owner for canonical usage indexing.
///
/// `next_sequence` exists only while messages are folded. Finishing consumes
/// the builder, so a generation can never retain mutable accumulation state.
#[derive(Default)]
pub(crate) struct UsageIndexBuilder {
    pub(super) index: FrozenUsageIndex,
    pub(super) next_sequence: usize,
    projection_totals: ProjectionTotals,
}

#[derive(Default)]
struct ProjectionTotals {
    tokens: UsageTokenBreakdown,
    cost: f64,
    timeline_message_count: u32,
    timeline_turn_count: u32,
}

/// Immutable canonical usage index installed in a generation.
///
/// The materialized maps preserve the single-fold projection performance
/// design while excluding write-only lifecycle state such as the next
/// first-seen sequence.
#[derive(Default, Serialize)]
pub struct FrozenUsageIndex {
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) usage_totals_by_client: HashMap<ClientId, UsageTotalsBucket>,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) model_map: HashMap<FineModelKey, FineModelBucket>,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) agent_map: HashMap<AgentKey, AgentBucket>,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) daily_map: HashMap<NaiveDate, DailyBucket>,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) hourly_map: HashMap<NaiveDateTime, HourlyBucket>,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct UsageTotalsBucket {
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
}

impl UsageTotalsBucket {
    fn push(
        &mut self,
        msg: &AttributedUsageRecord,
        msg_cost: f64,
    ) -> Result<(), UsageAggregationError> {
        add_record_tokens(&mut self.tokens, &msg.tokens)?;
        add_aggregation_cost(&mut self.cost, msg_cost, "usage cost")?;
        Ok(())
    }
}

/// Stores singleton groups inline and allocates only when a second value joins.
pub(super) enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub(super) fn push(&mut self, value: T) {
        if let Self::Many(values) = self {
            values.push(value);
            return;
        }

        let first = match std::mem::replace(self, Self::Many(Vec::with_capacity(2))) {
            Self::One(first) => first,
            Self::Many(_) => unreachable!("singleton branch contains multiple values"),
        };
        let Self::Many(values) = self else {
            unreachable!("singleton replacement did not create a vector")
        };
        values.push(first);
        values.push(value);
    }

    pub(super) fn into_stable_iter_by_key<K: Ord>(
        self,
        by_key: impl FnMut(&T) -> K,
    ) -> std::iter::Chain<std::option::IntoIter<T>, std::vec::IntoIter<T>> {
        match self {
            Self::One(value) => Some(value).into_iter().chain(Vec::new()),
            Self::Many(mut values) => {
                values.sort_by_key(by_key);
                None.into_iter().chain(values)
            }
        }
    }
}

/// Canonical `(client, provider, workspace, session, model)` bucket. Keeps
/// the additive counters plus the two creation-time attributes every
/// grouping re-derives materialized fields from: `first_seen` (arrival order
/// of the bucket's first message, for provider/label attribution and client
/// ordering tie-breaks) and `workspace_label` (the workspace DTO label of
/// that first message).
#[derive(Serialize, Deserialize)]
pub(super) struct FineModelBucket {
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(super) workspace_label: Arc<str>,
    first_seen: usize,
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
    pub(super) contribution_tokens: u64,
}

/// Grouped model bucket materialized by re-folding [`FineModelBucket`]s for
/// one `GroupBy`; converted to [`UsageModelEntry`] by
/// [`materialize_model`].
struct ModelBucket {
    model: Arc<str>,
    providers: IdentitySet<Arc<str>>,
    client: ClientId,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    sessions: IdentitySet<(ClientId, Arc<str>)>,
    // Boxed only for grouping modes that merge clients; keeps client-scoped
    // high-cardinality buckets free of an inline HashMap.
    #[allow(clippy::box_collection)]
    client_totals: Option<Box<HashMap<ClientId, ClientContributionOrder>>>,
}

#[derive(Clone, Copy)]
struct ClientContributionOrder {
    first_seen: usize,
    total_tokens: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(super) enum AgentInstanceKey {
    Explicit(#[serde(deserialize_with = "crate::records::intern::de_intern")] Arc<str>),
    Derived {
        client: ClientId,
        #[serde(deserialize_with = "crate::records::intern::de_intern")]
        session: Arc<str>,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(super) struct AgentKey {
    pub(super) client: ClientId,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    agent: Arc<str>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct AgentBucket {
    pub(super) instances: IdentitySet<AgentInstanceKey>,
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
    message_count: u32,
}

#[derive(Serialize, Deserialize)]
pub(super) struct DailyBucket {
    pub(super) date: NaiveDate,
    pub(super) clients: HashMap<ClientId, DailyClientBucket>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct DailyClientBucket {
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
    pub(super) message_count: u32,
    pub(super) turn_count: u32,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) models: HashMap<FineModelKey, FineDailyModelBucket>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct FineDailyModelBucket {
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub(super) workspace_label: Arc<str>,
    first_seen: usize,
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
    messages: u64,
}

/// Grouped daily model bucket materialized by re-folding
/// [`FineDailyModelBucket`]s for one `GroupBy`; converted to
/// [`DailyModelInfo`] by [`materialize_daily_model`].
struct DailyModelBucket {
    provider: Arc<str>,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    model: Arc<str>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    messages: u64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct HourlyBucket {
    pub(super) datetime: NaiveDateTime,
    pub(super) clients: HashMap<ClientId, HourlyClientBucket>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct HourlyClientBucket {
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
    #[serde(with = "super::wire::map_as_vec")]
    pub(super) models: HashMap<FineHourlyModelKey, FineHourlyModelBucket>,
    pub(super) message_count: u32,
    pub(super) turn_count: u32,
}

#[derive(Serialize, Deserialize)]
pub(super) struct FineHourlyModelBucket {
    first_seen: usize,
    pub(super) tokens: UsageTokenBreakdown,
    pub(super) cost: f64,
}

/// Grouped hourly model bucket materialized by re-folding
/// [`FineHourlyModelBucket`]s for one `GroupBy`; converted to
/// [`HourlyModelInfo`] by [`materialize_hourly_model`].
struct HourlyModelBucket {
    provider: Arc<str>,
    model: Arc<str>,
    tokens: UsageTokenBreakdown,
    cost: f64,
}

fn materialize_model(bucket: ModelBucket) -> Result<UsageModelEntry, UsageProjectionError> {
    let provider = bucket.providers.to_sorted_arc();
    let clients = if let Some(client_totals) = bucket.client_totals {
        let mut clients: Vec<_> = (*client_totals).into_iter().collect();
        clients.sort_by(|(left_client, left), (right_client, right)| {
            right
                .total_tokens
                .cmp(&left.total_tokens)
                .then_with(|| left.first_seen.cmp(&right.first_seen))
                .then_with(|| left_client.cmp(right_client))
        });
        clients.into_iter().map(|(client, _)| client).collect()
    } else {
        vec![bucket.client]
    };
    let model_id = bucket.model;
    Ok(UsageModelEntry {
        display_name: Arc::clone(&model_id),
        model_id,
        provider,
        clients,
        workspace_key: bucket.workspace_key,
        workspace_label: bucket.workspace_label,
        tokens: bucket.tokens,
        cost: bucket.cost,
        session_count: bucket
            .sessions
            .len()
            .try_into()
            .map_err(|_| UsageProjectionError::new("model session count"))?,
    })
}

fn materialize_daily_model(model: DailyModelBucket) -> DailyModelInfo {
    let model_id = model.model;
    DailyModelInfo {
        provider: model.provider,
        display_name: Arc::clone(&model_id),
        model_id,
        workspace_key: model.workspace_key,
        workspace_label: model.workspace_label,
        tokens: model.tokens,
        cost: model.cost,
        messages: model.messages,
    }
}

/// Re-fold one day's fine-grained client models into `group_by`'s daily
/// breakdown. Within each merged group, fine buckets fold in creation order so
/// floating-point sums stay deterministic; the first-created bucket in the group
/// attributes the provider and workspace label (its first message is the
/// group's first message, matching a direct grouped fold).
fn client_is_selected(client: ClientId, selected: Option<&HashSet<ClientId>>) -> bool {
    selected.is_none_or(|selected| selected.contains(&client))
}

fn materialize_daily_usage(
    bucket: &DailyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Result<Option<DailyUsage>, UsageProjectionError> {
    let mut client_breakdown = BTreeMap::new();
    let mut tokens = UsageTokenBreakdown::default();
    let mut cost = 0.0;
    let mut message_count = 0_u32;
    let mut turn_count = 0_u32;

    let mut selected_clients: Vec<_> = bucket
        .clients
        .iter()
        .filter(|(client, _)| client_is_selected(**client, selected))
        .collect();
    selected_clients.sort_by_key(|(client, _)| **client);

    for (client, client_bucket) in selected_clients {
        let models = materialize_daily_client_models(client_bucket, group_by)?;
        client_breakdown.insert(
            *client,
            DailyClientInfo {
                tokens: client_bucket.tokens.clone(),
                cost: client_bucket.cost,
                models,
            },
        );
        try_add_projection_tokens(&mut tokens, &client_bucket.tokens, "daily token totals")?;
        try_add_projection_cost(&mut cost, client_bucket.cost, "daily cost")?;
        message_count = message_count
            .checked_add(client_bucket.message_count)
            .ok_or_else(|| UsageProjectionError::new("daily message count"))?;
        turn_count = turn_count
            .checked_add(client_bucket.turn_count)
            .ok_or_else(|| UsageProjectionError::new("daily turn count"))?;
    }
    Ok((!client_breakdown.is_empty()).then_some(DailyUsage {
        date: bucket.date,
        tokens,
        cost,
        client_breakdown,
        message_count,
        turn_count,
    }))
}

fn materialize_daily_client_models(
    client_bucket: &DailyClientBucket,
    group_by: &GroupBy,
) -> Result<Vec<DailyModelInfo>, UsageProjectionError> {
    let mut grouped_fine_models: HashMap<
        GroupedModelKey,
        OneOrMany<(&FineModelKey, &FineDailyModelBucket)>,
    > = HashMap::new();
    for (fine_key, fine_model) in &client_bucket.models {
        let fine_bucket = (fine_key, fine_model);
        grouped_fine_models
            .entry(fine_key.grouped(group_by))
            .and_modify(|models| models.push(fine_bucket))
            .or_insert(OneOrMany::One(fine_bucket));
    }

    let mut grouped_models = Vec::with_capacity(grouped_fine_models.len());
    for (key, fine_models) in grouped_fine_models {
        let mut grouped_model: Option<DailyModelBucket> = None;
        for (fine_key, fine_model) in
            fine_models.into_stable_iter_by_key(|(_, model)| model.first_seen)
        {
            let grouped_model = grouped_model.get_or_insert_with(|| {
                let (workspace_key, workspace_label) = if *group_by == GroupBy::WorkspaceModel {
                    (
                        fine_key.workspace.to_key(),
                        Some(Arc::clone(&fine_model.workspace_label)),
                    )
                } else {
                    (None, None)
                };
                DailyModelBucket {
                    provider: Arc::clone(&fine_key.provider),
                    workspace_key,
                    workspace_label,
                    model: Arc::clone(&fine_key.model),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                }
            });
            try_add_projection_tokens(
                &mut grouped_model.tokens,
                &fine_model.tokens,
                "daily model token totals",
            )?;
            try_add_projection_cost(&mut grouped_model.cost, fine_model.cost, "daily model cost")?;
            grouped_model.messages = grouped_model
                .messages
                .checked_add(fine_model.messages)
                .ok_or_else(|| UsageProjectionError::new("daily model message count"))?;
        }
        let grouped_model =
            grouped_model.expect("daily target group contains at least one fine model bucket");
        grouped_models.push((key, materialize_daily_model(grouped_model)));
    }
    grouped_models.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(grouped_models.into_iter().map(|(_, model)| model).collect())
}

fn materialize_hourly_model(model: HourlyModelBucket) -> HourlyModelInfo {
    let model_id = model.model;
    HourlyModelInfo {
        provider: model.provider,
        display_name: Arc::clone(&model_id),
        model_id,
        tokens: model.tokens,
        cost: model.cost,
    }
}

/// Re-fold one hour's `(provider, model)` buckets into `group_by`'s hourly
/// model vector. Only ClientProviderModel keeps the provider split; the other
/// groupings merge providers, attributing the first-created bucket's
/// provider (matching a direct grouped fold).
fn materialize_hourly_usage(
    bucket: &HourlyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Result<Option<HourlyUsage>, UsageProjectionError> {
    let mut tokens = UsageTokenBreakdown::default();
    let mut cost = 0.0;
    let mut clients = BTreeSet::new();
    let mut fine_models = Vec::new();
    let mut message_count = 0_u32;
    let mut turn_count = 0_u32;

    let mut selected_clients: Vec<_> = bucket
        .clients
        .iter()
        .filter(|(client, _)| client_is_selected(**client, selected))
        .collect();
    selected_clients.sort_by_key(|(client, _)| **client);

    for (client, client_bucket) in selected_clients {
        clients.insert(*client);
        try_add_projection_tokens(&mut tokens, &client_bucket.tokens, "hourly token totals")?;
        try_add_projection_cost(&mut cost, client_bucket.cost, "hourly cost")?;
        message_count = message_count
            .checked_add(client_bucket.message_count)
            .ok_or_else(|| UsageProjectionError::new("hourly message count"))?;
        turn_count = turn_count
            .checked_add(client_bucket.turn_count)
            .ok_or_else(|| UsageProjectionError::new("hourly turn count"))?;
        fine_models.extend(client_bucket.models.iter());
    }
    if clients.is_empty() {
        return Ok(None);
    }
    fine_models.sort_by_key(|(_, model)| model.first_seen);
    let mut grouped_models: HashMap<HourlyModelKey, HourlyModelBucket> = HashMap::new();
    for (fine_key, fine_model) in fine_models {
        let grouped_model = grouped_models
            .entry(fine_key.grouped(group_by))
            .or_insert_with(|| HourlyModelBucket {
                provider: Arc::clone(&fine_key.provider),
                model: Arc::clone(&fine_key.model),
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
            });
        try_add_projection_tokens(
            &mut grouped_model.tokens,
            &fine_model.tokens,
            "hourly model token totals",
        )?;
        try_add_projection_cost(
            &mut grouped_model.cost,
            fine_model.cost,
            "hourly model cost",
        )?;
    }
    let mut grouped_models: Vec<_> = grouped_models.into_iter().collect();
    grouped_models.sort_by(|(left, _), (right, _)| left.cmp(right));
    let models = grouped_models
        .into_iter()
        .map(|(_, model)| materialize_hourly_model(model))
        .collect();
    Ok(Some(HourlyUsage {
        datetime: bucket.datetime,
        tokens,
        cost,
        clients,
        models,
        message_count,
        turn_count,
    }))
}

impl UsageIndexBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn check_push_local(
        &self,
        msg: &AttributedUsageRecord,
        local_date: Option<NaiveDate>,
        local_hour: Option<NaiveDateTime>,
    ) -> Result<(), UsageAggregationError> {
        self.next_sequence
            .checked_add(1)
            .ok_or_else(|| UsageAggregationError::new("usage indexing sequence"))?;

        let msg_cost = normalized_cost(msg.cost)?;
        let client = msg.client;
        let message_count = record_count_u32(msg.message_count, "record message count")?;
        let model_key = FineModelKey::from_message(msg);
        let contribution = msg
            .tokens
            .checked_total()
            .ok_or_else(|| UsageAggregationError::new("record token total"))?
            .max(0) as u64;

        check_record_tokens(Some(&self.projection_totals.tokens), &msg.tokens)
            .map_err(|_| UsageAggregationError::new("global token totals"))?;
        check_aggregation_cost(Some(self.projection_totals.cost), msg_cost, "global cost")?;
        if local_date.is_some() {
            self.projection_totals
                .timeline_message_count
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("global timeline message count"))?;
            self.projection_totals
                .timeline_turn_count
                .checked_add(u32::from(msg.is_turn_start))
                .ok_or_else(|| UsageAggregationError::new("global timeline turn count"))?;
        }

        let totals = self.index.usage_totals_by_client.get(&client);
        check_record_tokens(totals.map(|bucket| &bucket.tokens), &msg.tokens)?;
        check_aggregation_cost(totals.map(|bucket| bucket.cost), msg_cost, "usage cost")?;

        let model = self.index.model_map.get(&model_key);
        check_record_tokens(model.map(|bucket| &bucket.tokens), &msg.tokens)?;
        check_aggregation_cost(model.map(|bucket| bucket.cost), msg_cost, "model cost")?;
        model
            .map_or(0, |bucket| bucket.contribution_tokens)
            .checked_add(contribution)
            .ok_or_else(|| UsageAggregationError::new("client token contribution"))?;

        if let Some(agent) = msg.agent.as_ref() {
            let agent_key = AgentKey {
                client,
                agent: Arc::clone(agent),
            };
            let agent = self.index.agent_map.get(&agent_key);
            check_record_tokens(agent.map(|bucket| &bucket.tokens), &msg.tokens)?;
            check_aggregation_cost(agent.map(|bucket| bucket.cost), msg_cost, "agent cost")?;
            agent
                .map_or(0, |bucket| bucket.message_count)
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("agent message count"))?;
        }

        if let Some(date) = local_date {
            let daily_client = self
                .index
                .daily_map
                .get(&date)
                .and_then(|day| day.clients.get(&client));
            check_record_tokens(daily_client.map(|bucket| &bucket.tokens), &msg.tokens)?;
            check_aggregation_cost(
                daily_client.map(|bucket| bucket.cost),
                msg_cost,
                "daily cost",
            )?;
            daily_client
                .map_or(0, |bucket| bucket.message_count)
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("daily message count"))?;
            daily_client
                .map_or(0, |bucket| bucket.turn_count)
                .checked_add(u32::from(msg.is_turn_start))
                .ok_or_else(|| UsageAggregationError::new("daily turn count"))?;

            let daily_model = daily_client.and_then(|bucket| bucket.models.get(&model_key));
            check_record_tokens(daily_model.map(|bucket| &bucket.tokens), &msg.tokens)?;
            check_aggregation_cost(
                daily_model.map(|bucket| bucket.cost),
                msg_cost,
                "daily model cost",
            )?;
            daily_model
                .map_or(0, |bucket| bucket.messages)
                .checked_add(msg.message_count.max(0) as u64)
                .ok_or_else(|| UsageAggregationError::new("daily model message count"))?;
        }

        if let Some(hour) = local_hour {
            let hourly_client = self
                .index
                .hourly_map
                .get(&hour)
                .and_then(|bucket| bucket.clients.get(&client));
            check_record_tokens(hourly_client.map(|bucket| &bucket.tokens), &msg.tokens)?;
            check_aggregation_cost(
                hourly_client.map(|bucket| bucket.cost),
                msg_cost,
                "hourly cost",
            )?;
            hourly_client
                .map_or(0, |bucket| bucket.message_count)
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("hourly message count"))?;
            hourly_client
                .map_or(0, |bucket| bucket.turn_count)
                .checked_add(u32::from(msg.is_turn_start))
                .ok_or_else(|| UsageAggregationError::new("hourly turn count"))?;

            let hourly_model_key = FineHourlyModelKey::from_message(msg);
            let hourly_model =
                hourly_client.and_then(|bucket| bucket.models.get(&hourly_model_key));
            check_record_tokens(hourly_model.map(|bucket| &bucket.tokens), &msg.tokens)?;
            check_aggregation_cost(
                hourly_model.map(|bucket| bucket.cost),
                msg_cost,
                "hourly model cost",
            )?;
        }

        Ok(())
    }

    pub(crate) fn push_local(
        &mut self,
        msg: &AttributedUsageRecord,
        local_date: Option<NaiveDate>,
        local_hour: Option<NaiveDateTime>,
    ) -> Result<(), UsageAggregationError> {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| UsageAggregationError::new("usage indexing sequence"))?;

        let msg_cost = normalized_cost(msg.cost)?;
        let client = msg.client;
        let message_count = record_count_u32(msg.message_count, "record message count")?;

        self.index
            .usage_totals_by_client
            .entry(client)
            .or_default()
            .push(msg, msg_cost)?;

        let model_entry = self
            .index
            .model_map
            .entry(FineModelKey::from_message(msg))
            .or_insert_with(|| FineModelBucket {
                workspace_label: workspace_fields(msg).1,
                first_seen: sequence,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                contribution_tokens: 0,
            });

        add_record_tokens(&mut model_entry.tokens, &msg.tokens)?;
        add_aggregation_cost(&mut model_entry.cost, msg_cost, "model cost")?;
        let contribution = msg
            .tokens
            .checked_total()
            .ok_or_else(|| UsageAggregationError::new("record token total"))?
            .max(0) as u64;
        model_entry.contribution_tokens = model_entry
            .contribution_tokens
            .checked_add(contribution)
            .ok_or_else(|| UsageAggregationError::new("client token contribution"))?;
        if let Some(agent) = msg.agent.as_ref() {
            let agent_entry = self
                .index
                .agent_map
                .entry(AgentKey {
                    client,
                    agent: Arc::clone(agent),
                })
                .or_insert_with(|| AgentBucket {
                    instances: IdentitySet::default(),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    message_count: 0,
                });
            add_record_tokens(&mut agent_entry.tokens, &msg.tokens)?;
            add_aggregation_cost(&mut agent_entry.cost, msg_cost, "agent cost")?;
            agent_entry.message_count = agent_entry
                .message_count
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("agent message count"))?;
            let instance_key = msg.agent_instance.as_ref().map_or_else(
                || AgentInstanceKey::Derived {
                    client,
                    session: Arc::clone(&msg.session_id),
                },
                |instance| AgentInstanceKey::Explicit(Arc::clone(instance)),
            );
            agent_entry.instances.insert(instance_key);
        }

        if let Some(date) = local_date {
            let daily_entry = self
                .index
                .daily_map
                .entry(date)
                .or_insert_with(|| DailyBucket {
                    date,
                    clients: HashMap::new(),
                });

            let client_entry =
                daily_entry
                    .clients
                    .entry(client)
                    .or_insert_with(|| DailyClientBucket {
                        tokens: UsageTokenBreakdown::default(),
                        cost: 0.0,
                        message_count: 0,
                        turn_count: 0,
                        models: HashMap::new(),
                    });
            add_record_tokens(&mut client_entry.tokens, &msg.tokens)?;
            add_aggregation_cost(&mut client_entry.cost, msg_cost, "daily cost")?;
            client_entry.message_count = client_entry
                .message_count
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("daily message count"))?;
            if msg.is_turn_start {
                client_entry.turn_count = client_entry
                    .turn_count
                    .checked_add(1)
                    .ok_or_else(|| UsageAggregationError::new("daily turn count"))?;
            }

            let model_info = client_entry
                .models
                .entry(FineModelKey::from_message(msg))
                .or_insert_with(|| FineDailyModelBucket {
                    workspace_label: workspace_fields(msg).1,
                    first_seen: sequence,
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                });
            add_record_tokens(&mut model_info.tokens, &msg.tokens)?;
            add_aggregation_cost(&mut model_info.cost, msg_cost, "daily model cost")?;
            model_info.messages = model_info
                .messages
                .checked_add(msg.message_count.max(0) as u64)
                .ok_or_else(|| UsageAggregationError::new("daily model message count"))?;
        }

        if let Some(bucket) = local_hour {
            let hourly_entry =
                self.index
                    .hourly_map
                    .entry(bucket)
                    .or_insert_with(|| HourlyBucket {
                        datetime: bucket,
                        clients: HashMap::new(),
                    });
            let client_entry =
                hourly_entry
                    .clients
                    .entry(client)
                    .or_insert_with(|| HourlyClientBucket {
                        tokens: UsageTokenBreakdown::default(),
                        cost: 0.0,
                        models: HashMap::new(),
                        message_count: 0,
                        turn_count: 0,
                    });
            add_record_tokens(&mut client_entry.tokens, &msg.tokens)?;
            add_aggregation_cost(&mut client_entry.cost, msg_cost, "hourly cost")?;
            client_entry.message_count = client_entry
                .message_count
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("hourly message count"))?;
            if msg.is_turn_start {
                client_entry.turn_count = client_entry
                    .turn_count
                    .checked_add(1)
                    .ok_or_else(|| UsageAggregationError::new("hourly turn count"))?;
            }
            let hmodel = client_entry
                .models
                .entry(FineHourlyModelKey::from_message(msg))
                .or_insert_with(|| FineHourlyModelBucket {
                    first_seen: sequence,
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                });
            add_record_tokens(&mut hmodel.tokens, &msg.tokens)?;
            add_aggregation_cost(&mut hmodel.cost, msg_cost, "hourly model cost")?;
        }

        add_record_tokens(&mut self.projection_totals.tokens, &msg.tokens)
            .map_err(|_| UsageAggregationError::new("global token totals"))?;
        add_aggregation_cost(&mut self.projection_totals.cost, msg_cost, "global cost")?;
        if local_date.is_some() {
            self.projection_totals.timeline_message_count = self
                .projection_totals
                .timeline_message_count
                .checked_add(message_count)
                .ok_or_else(|| UsageAggregationError::new("global timeline message count"))?;
            self.projection_totals.timeline_turn_count = self
                .projection_totals
                .timeline_turn_count
                .checked_add(u32::from(msg.is_turn_start))
                .ok_or_else(|| UsageAggregationError::new("global timeline turn count"))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn push(
        &mut self,
        msg: &AttributedUsageRecord,
    ) -> Result<(), UsageAggregationError> {
        let (local_date, local_hour) = test_calendar_fields(msg.timestamp).unzip();
        self.check_push_local(msg, local_date, local_hour)?;
        self.push_local(msg, local_date, local_hour)
    }

    pub(crate) fn finish(mut self) -> FrozenUsageIndex {
        self.index.shrink_to_fit();
        self.index
    }
}

impl FrozenUsageIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) fn shrink_to_fit(&mut self) {
        self.usage_totals_by_client.shrink_to_fit();
        self.model_map.shrink_to_fit();
        self.agent_map.shrink_to_fit();
        self.daily_map.shrink_to_fit();
        self.hourly_map.shrink_to_fit();

        for agent in self.agent_map.values_mut() {
            agent.instances.shrink_to_fit();
        }
        for daily in self.daily_map.values_mut() {
            daily.clients.shrink_to_fit();
            for client in daily.clients.values_mut() {
                client.models.shrink_to_fit();
            }
        }
        for hourly in self.hourly_map.values_mut() {
            hourly.clients.shrink_to_fit();
            for client in hourly.clients.values_mut() {
                client.models.shrink_to_fit();
            }
        }
    }

    /// Prove that every additive path used by a complete projection is safe.
    ///
    /// Validation has already established non-negative finite costs and the
    /// token relationships between fine and client buckets. Folding the
    /// coarsest totals is therefore sufficient for every grouping and client
    /// subset without allocating a materialized projection.
    pub(super) fn validate_projection_arithmetic(&self) -> Result<(), UsageProjectionError> {
        let mut total_tokens = UsageTokenBreakdown::default();
        let mut total_cost = 0.0;
        for totals in self.usage_totals_by_client.values() {
            try_add_projection_tokens(&mut total_tokens, &totals.tokens, "total token breakdown")?;
            try_add_projection_cost(&mut total_cost, totals.cost, "total cost")?;
        }

        let mut model_tokens = UsageTokenBreakdown::default();
        let mut model_cost = 0.0;
        let mut contribution_tokens = 0_u64;
        for model in self.model_map.values() {
            try_add_projection_tokens(&mut model_tokens, &model.tokens, "model token totals")?;
            try_add_projection_cost(&mut model_cost, model.cost, "model cost")?;
            contribution_tokens = contribution_tokens
                .checked_add(model.contribution_tokens)
                .ok_or_else(|| UsageProjectionError::new("client token contribution"))?;
        }
        let _: u32 = self
            .model_map
            .len()
            .try_into()
            .map_err(|_| UsageProjectionError::new("model session count"))?;

        for agent in self.agent_map.values() {
            let _: u32 = agent
                .instances
                .len()
                .try_into()
                .map_err(|_| UsageProjectionError::new("agent instance count"))?;
            agent
                .tokens
                .checked_total()
                .ok_or_else(|| UsageProjectionError::new("agent token total"))?;
        }

        let mut cross_day_tokens = UsageTokenBreakdown::default();
        let mut cross_day_cost = 0.0;
        let mut cross_day_message_count = 0_u32;
        let mut cross_day_turn_count = 0_u32;
        for day in self.daily_map.values() {
            let mut day_tokens = UsageTokenBreakdown::default();
            let mut day_cost = 0.0;
            let mut day_message_count = 0_u32;
            let mut day_turn_count = 0_u32;
            for client in day.clients.values() {
                let mut model_cost = 0.0;
                let mut model_messages = 0_u64;
                for model in client.models.values() {
                    try_add_projection_cost(&mut model_cost, model.cost, "daily model cost")?;
                    model_messages = model_messages
                        .checked_add(model.messages)
                        .ok_or_else(|| UsageProjectionError::new("daily model message count"))?;
                }
                try_add_projection_tokens(&mut day_tokens, &client.tokens, "daily token totals")?;
                try_add_projection_cost(&mut day_cost, client.cost, "daily cost")?;
                day_message_count = day_message_count
                    .checked_add(client.message_count)
                    .ok_or_else(|| UsageProjectionError::new("daily message count"))?;
                day_turn_count = day_turn_count
                    .checked_add(client.turn_count)
                    .ok_or_else(|| UsageProjectionError::new("daily turn count"))?;
            }
            try_add_projection_tokens(
                &mut cross_day_tokens,
                &day_tokens,
                "cross-day token totals",
            )?;
            try_add_projection_cost(&mut cross_day_cost, day_cost, "cross-day cost")?;
            cross_day_message_count = cross_day_message_count
                .checked_add(day_message_count)
                .ok_or_else(|| UsageProjectionError::new("cross-day message count"))?;
            cross_day_turn_count = cross_day_turn_count
                .checked_add(day_turn_count)
                .ok_or_else(|| UsageProjectionError::new("cross-day turn count"))?;
        }

        let mut cross_hour_tokens = UsageTokenBreakdown::default();
        let mut cross_hour_cost = 0.0;
        let mut cross_hour_message_count = 0_u32;
        let mut cross_hour_turn_count = 0_u32;
        for hour in self.hourly_map.values() {
            let mut hour_tokens = UsageTokenBreakdown::default();
            let mut hour_cost = 0.0;
            let mut hour_model_cost = 0.0;
            let mut hour_message_count = 0_u32;
            let mut hour_turn_count = 0_u32;
            for client in hour.clients.values() {
                for model in client.models.values() {
                    try_add_projection_cost(&mut hour_model_cost, model.cost, "hourly model cost")?;
                }
                try_add_projection_tokens(&mut hour_tokens, &client.tokens, "hourly token totals")?;
                try_add_projection_cost(&mut hour_cost, client.cost, "hourly cost")?;
                hour_message_count = hour_message_count
                    .checked_add(client.message_count)
                    .ok_or_else(|| UsageProjectionError::new("hourly message count"))?;
                hour_turn_count = hour_turn_count
                    .checked_add(client.turn_count)
                    .ok_or_else(|| UsageProjectionError::new("hourly turn count"))?;
            }
            try_add_projection_tokens(
                &mut cross_hour_tokens,
                &hour_tokens,
                "cross-hour token totals",
            )?;
            try_add_projection_cost(&mut cross_hour_cost, hour_cost, "cross-hour cost")?;
            cross_hour_message_count = cross_hour_message_count
                .checked_add(hour_message_count)
                .ok_or_else(|| UsageProjectionError::new("cross-hour message count"))?;
            cross_hour_turn_count = cross_hour_turn_count
                .checked_add(hour_turn_count)
                .ok_or_else(|| UsageProjectionError::new("cross-hour turn count"))?;
        }

        Ok(())
    }

    /// Re-fold canonical model buckets into one grouping.
    fn refold_models(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
    ) -> Result<Vec<(GroupedModelKey, ModelBucket)>, UsageProjectionError> {
        let mut grouped_fine_models: HashMap<
            GroupedModelKey,
            OneOrMany<(&FineModelKey, &FineModelBucket)>,
        > = HashMap::new();
        for (fine_key, fine_model) in &self.model_map {
            if !client_is_selected(fine_key.client, selected) {
                continue;
            }
            let fine_bucket = (fine_key, fine_model);
            grouped_fine_models
                .entry(fine_key.grouped(group_by))
                .and_modify(|models| models.push(fine_bucket))
                .or_insert(OneOrMany::One(fine_bucket));
        }

        let mut model_buckets = Vec::with_capacity(grouped_fine_models.len());
        for (key, fine_models) in grouped_fine_models {
            let merge_clients = key.merges_clients();
            let mut model_entry: Option<ModelBucket> = None;
            for (fine_key, fine_model) in
                fine_models.into_stable_iter_by_key(|(_, model)| model.first_seen)
            {
                let model_entry = model_entry.get_or_insert_with(|| {
                    let (workspace_key, workspace_label) = if *group_by == GroupBy::WorkspaceModel {
                        (
                            fine_key.workspace.to_key(),
                            Some(Arc::clone(&fine_model.workspace_label)),
                        )
                    } else {
                        (None, None)
                    };
                    ModelBucket {
                        model: Arc::clone(&fine_key.model),
                        providers: IdentitySet::default(),
                        client: fine_key.client,
                        workspace_key,
                        workspace_label,
                        tokens: UsageTokenBreakdown::default(),
                        cost: 0.0,
                        sessions: IdentitySet::default(),
                        client_totals: merge_clients.then(|| Box::new(HashMap::new())),
                    }
                });

                if merge_clients {
                    let totals = model_entry
                        .client_totals
                        .as_mut()
                        .expect("merge-client grouping has client totals")
                        .entry(fine_key.client)
                        .or_insert_with(|| ClientContributionOrder {
                            first_seen: fine_model.first_seen,
                            total_tokens: 0,
                        });
                    totals.total_tokens = totals
                        .total_tokens
                        .checked_add(fine_model.contribution_tokens)
                        .ok_or_else(|| UsageProjectionError::new("client token contribution"))?;
                }

                model_entry.providers.insert(Arc::clone(&fine_key.provider));

                try_add_projection_tokens(
                    &mut model_entry.tokens,
                    &fine_model.tokens,
                    "model token totals",
                )?;
                try_add_projection_cost(&mut model_entry.cost, fine_model.cost, "model cost")?;

                model_entry
                    .sessions
                    .insert((fine_key.client, Arc::clone(&fine_key.session)));
            }
            model_buckets.push((
                key,
                model_entry.expect("target group contains at least one fine model bucket"),
            ));
        }
        Ok(model_buckets)
    }

    /// Materialize models and their aggregate totals without date-dependent
    /// timeline, graph, or streak work.
    pub fn project_models(
        &self,
        group_by: &GroupBy,
    ) -> Result<ModelProjection, UsageProjectionError> {
        self.project_models_selected(group_by, None)
    }

    /// Materialize models and totals for a client subset without acquiring or
    /// mutating canonical generation state.
    pub fn project_models_for_clients(
        &self,
        group_by: &GroupBy,
        selected: &HashSet<ClientId>,
    ) -> Result<ModelProjection, UsageProjectionError> {
        self.project_models_selected(group_by, Some(selected))
    }

    fn project_models_selected(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
    ) -> Result<ModelProjection, UsageProjectionError> {
        // The client universe is intentionally tiny compared with the
        // canonical model map. Stable-folding only client totals keeps full
        // and selected projections on the same deterministic cost semantics
        // without duplicating a second global source of truth.
        let mut client_totals: Vec<_> = self
            .usage_totals_by_client
            .iter()
            .filter(|(client, _)| client_is_selected(**client, selected))
            .collect();
        client_totals.sort_by_key(|(client, _)| **client);

        let mut total_token_breakdown = UsageTokenBreakdown::default();
        let mut total_cost = 0.0;
        for (_, totals) in client_totals {
            try_add_projection_tokens(
                &mut total_token_breakdown,
                &totals.tokens,
                "total token breakdown",
            )?;
            try_add_projection_cost(&mut total_cost, totals.cost, "total cost")?;
        }

        let mut keyed_models = self
            .refold_models(group_by, selected)?
            .into_iter()
            .map(|(key, bucket)| materialize_model(bucket).map(|model| (key, model)))
            .collect::<Result<Vec<_>, _>>()?;
        keyed_models.sort_by(|(a_key, a), (b_key, b)| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| a.model_id.cmp(&b.model_id))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.clients.cmp(&b.clients))
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.workspace_key.cmp(&b.workspace_key))
                .then_with(|| a_key.cmp(b_key))
        });

        Ok(ModelProjection {
            models: keyed_models.into_iter().map(|(_, model)| model).collect(),
            total_tokens: total_token_breakdown
                .checked_total()
                .ok_or_else(|| UsageProjectionError::new("total tokens"))?,
            total_cost: total_cost.max(0.0),
        })
    }

    /// Materialize one grouping's [`UsageProjection`] from the canonical fold
    /// state. Borrowing, so the same accumulator can be projected repeatedly
    /// with different groupings without rescanning local clients.
    pub fn project_usage(
        &self,
        group_by: &GroupBy,
        effective_date: NaiveDate,
    ) -> Result<UsageProjection, UsageProjectionError> {
        self.project_usage_selected(group_by, None, effective_date)
    }

    /// Materialize a usage projection for a session-local subset of the
    /// clients that produced this accumulator. This is a pure projection: it
    /// never scans, reparses, or reprices input data.
    pub fn project_usage_for_clients(
        &self,
        group_by: &GroupBy,
        selected: &HashSet<ClientId>,
        effective_date: NaiveDate,
    ) -> Result<UsageProjection, UsageProjectionError> {
        self.project_usage_selected(group_by, Some(selected), effective_date)
    }

    fn project_usage_selected(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
        effective_date: NaiveDate,
    ) -> Result<UsageProjection, UsageProjectionError> {
        let ModelProjection {
            models,
            total_tokens,
            total_cost,
        } = self.project_models_selected(group_by, selected)?;

        let mut agents_with_totals = self
            .agent_map
            .iter()
            .filter_map(|(key, agent)| {
                if !client_is_selected(key.client, selected) {
                    return None;
                }
                Some(
                    agent
                        .instances
                        .len()
                        .try_into()
                        .map_err(|_| UsageProjectionError::new("agent instance count"))
                        .and_then(|instance_count| {
                            let token_total = agent
                                .tokens
                                .checked_total()
                                .ok_or_else(|| UsageProjectionError::new("agent token total"))?;
                            Ok((
                                AgentEntry {
                                    agent: Arc::clone(&key.agent),
                                    client: key.client,
                                    tokens: agent.tokens.clone(),
                                    cost: agent.cost,
                                    message_count: agent.message_count,
                                    instance_count,
                                },
                                token_total,
                            ))
                        }),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        agents_with_totals.sort_by(|(a, a_total), (b, b_total)| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| b_total.cmp(a_total))
                .then_with(|| a.agent.cmp(&b.agent))
                .then_with(|| a.client.cmp(&b.client))
        });
        let agents = agents_with_totals
            .into_iter()
            .map(|(agent, _)| agent)
            .collect();

        let mut daily = self
            .daily_map
            .values()
            .map(|bucket| materialize_daily_usage(bucket, group_by, selected))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut daily_tokens = UsageTokenBreakdown::default();
        let mut daily_message_count = 0_u32;
        let mut daily_turn_count = 0_u32;
        for day in &daily {
            try_add_projection_tokens(&mut daily_tokens, &day.tokens, "cross-day token totals")?;
            daily_message_count = daily_message_count
                .checked_add(day.message_count)
                .ok_or_else(|| UsageProjectionError::new("cross-day message count"))?;
            daily_turn_count = daily_turn_count
                .checked_add(day.turn_count)
                .ok_or_else(|| UsageProjectionError::new("cross-day turn count"))?;
        }
        daily.sort_by_key(|usage| std::cmp::Reverse(usage.date));

        let mut hourly = self
            .hourly_map
            .values()
            .map(|bucket| materialize_hourly_usage(bucket, group_by, selected))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut hourly_tokens = UsageTokenBreakdown::default();
        let mut hourly_message_count = 0_u32;
        let mut hourly_turn_count = 0_u32;
        for hour in &hourly {
            try_add_projection_tokens(&mut hourly_tokens, &hour.tokens, "cross-hour token totals")?;
            hourly_message_count = hourly_message_count
                .checked_add(hour.message_count)
                .ok_or_else(|| UsageProjectionError::new("cross-hour message count"))?;
            hourly_turn_count = hourly_turn_count
                .checked_add(hour.turn_count)
                .ok_or_else(|| UsageProjectionError::new("cross-hour turn count"))?;
        }
        hourly.sort_by_key(|usage| std::cmp::Reverse(usage.datetime));

        let graph = build_contribution_graph_for_today(&daily, effective_date)?;
        let (current_streak, longest_streak) = calculate_streaks_for_today(&daily, effective_date)?;

        Ok(UsageProjection {
            group_by: *group_by,
            models,
            agents,
            daily,
            hourly,
            graph,
            total_tokens,
            total_cost,
            current_streak,
            longest_streak,
        })
    }
}
