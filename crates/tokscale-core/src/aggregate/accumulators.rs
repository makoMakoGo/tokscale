//! Per-view accumulators derived from the old fold sites
//! (`aggregate_model_usage_entries`, `MonthAggregator`/month fold,
//! `HourAggregator`/hour fold, and the daily graph fold). Time-metrics views
//! still need their existing two-pass projection.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    aggregate::keys::{workspace_fields, GroupedModelKey, IdentitySet},
    checked_token_add, checked_token_sum, positive_token_total,
    sessionize::SessionTimeEvent,
    ClientContribution, ClientContributionOrder, DailyContribution, DailyTotals, GraphResult,
    GroupBy, HourlyUsage, ModelPerformance, ModelUsage, MonthlyUsage, SessionContribution,
    TimeMetricsReport, TokenBreakdown, UnifiedMessage, ViewSet,
};

use super::{finish_graph_result, views::AgentUsage};

fn add_token_breakdown(target: &mut TokenBreakdown, source: &TokenBreakdown) {
    *target = target
        .checked_add(source)
        .expect("token buckets exceed i64::MAX while aggregating usage");
}

fn hourly_label(hour_key: &str) -> String {
    // `hourly_report_label` returns `key[5..]` ("MM-DD HH:00"). Kept inline to
    // avoid a private cross-module call; identical slice.
    hour_key.get(5..).map(str::to_string).unwrap_or_default()
}

/// Core model-report accumulator — port of `aggregate_model_usage_entries`.
/// `push` is the per-message fold; `finish` is the finalize + cost sort.
pub(super) struct ModelEntries {
    group_by: GroupBy,
    model_map: HashMap<GroupedModelKey, ModelBucket>,
    next_sequence: usize,
}

struct ModelBucket {
    client: Arc<str>,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    session_id: Option<Arc<str>>,
    model: Arc<str>,
    providers: IdentitySet<Arc<str>>,
    // Boxed only for grouping modes that merge clients; keeps session and
    // client-scoped high-cardinality buckets free of an inline HashMap.
    #[allow(clippy::box_collection)]
    client_totals: Option<Box<HashMap<Arc<str>, ClientContributionOrder>>>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    message_count: i32,
    cost: f64,
    performance: ModelPerformance,
}

fn ordered_arc_clients(client_totals: &HashMap<Arc<str>, ClientContributionOrder>) -> String {
    let mut clients: Vec<(&str, ClientContributionOrder)> = client_totals
        .iter()
        .map(|(client, totals)| (client.as_ref(), *totals))
        .collect();
    clients.sort_by(|(left_client, left), (right_client, right)| {
        right
            .total_tokens
            .cmp(&left.total_tokens)
            .then_with(|| left.first_seen.cmp(&right.first_seen))
            .then_with(|| left_client.cmp(right_client))
    });
    clients
        .into_iter()
        .map(|(client, _)| client)
        .collect::<Vec<_>>()
        .join(", ")
}

impl ModelEntries {
    pub(super) fn new(group_by: GroupBy) -> Self {
        Self {
            group_by,
            model_map: HashMap::new(),
            next_sequence: 0,
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("model aggregation sequence exceeds usize::MAX");
        let key = GroupedModelKey::from_message(&self.group_by, msg);
        let merge_clients = key.merges_clients();
        let entry = self.model_map.entry(key).or_insert_with(|| {
            let (workspace_key, workspace_label) = if self.group_by == GroupBy::WorkspaceModel {
                let (key, label) = workspace_fields(msg);
                (key, Some(label))
            } else {
                (None, None)
            };
            ModelBucket {
                client: Arc::clone(&msg.client),
                workspace_key,
                workspace_label,
                session_id: matches!(self.group_by, GroupBy::Session | GroupBy::ClientSession)
                    .then(|| Arc::clone(&msg.session_id)),
                model: Arc::clone(&msg.model_id),
                providers: IdentitySet::one(Arc::clone(&msg.provider_id)),
                client_totals: merge_clients.then(|| Box::new(HashMap::new())),
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                message_count: 0,
                cost: 0.0,
                performance: ModelPerformance::default(),
            }
        });

        if merge_clients {
            let totals = entry
                .client_totals
                .as_mut()
                .expect("merge-client grouping has client totals")
                .entry(Arc::clone(&msg.client))
                .or_insert_with(|| ClientContributionOrder {
                    first_seen: sequence,
                    total_tokens: 0,
                });
            totals.total_tokens = totals
                .total_tokens
                .checked_add(msg.tokens.total().max(0) as u64)
                .expect("client token contribution exceeds u64::MAX");
        }

        entry.providers.insert(Arc::clone(&msg.provider_id));

        entry.input = checked_token_add(entry.input, msg.tokens.input);
        entry.output = checked_token_add(entry.output, msg.tokens.output);
        entry.cache_read = checked_token_add(entry.cache_read, msg.tokens.cache_read);
        entry.cache_write = checked_token_add(entry.cache_write, msg.tokens.cache_write);
        entry.reasoning = checked_token_add(entry.reasoning, msg.tokens.reasoning);
        entry.message_count += msg.message_count.max(0);
        entry.cost += msg.cost;
        entry
            .performance
            .record_message(positive_token_total(&msg.tokens), msg.duration_ms);
    }

    pub(super) fn finish(self) -> Vec<ModelUsage> {
        let Self { model_map, .. } = self;
        let mut entries: Vec<_> = model_map
            .into_values()
            .map(materialize_model_bucket)
            .collect();

        entries.sort_by(|a, b| {
            let cost = match (a.cost.is_nan(), b.cost.is_nan()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => b
                    .cost
                    .partial_cmp(&a.cost)
                    .unwrap_or(std::cmp::Ordering::Equal),
            };
            // Deterministic secondary keys — identical to aggregate_model_usage_entries
            // (the C1.5 BLOCKER fix, applied to both paths together).
            cost.then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.client.cmp(&b.client))
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.workspace_key.cmp(&b.workspace_key))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        entries
    }
}

fn materialize_model_bucket(mut entry: ModelBucket) -> ModelUsage {
    let total_tokens = checked_token_sum([
        entry.input.max(0),
        entry.output.max(0),
        entry.cache_read.max(0),
        entry.cache_write.max(0),
        entry.reasoning.max(0),
    ]);
    entry.performance.finalize(total_tokens);
    let provider = entry.providers.into_sorted_string();
    let merged_clients = entry
        .client_totals
        .as_ref()
        .map(|totals| ordered_arc_clients(totals));
    ModelUsage {
        client: merged_clients
            .clone()
            .unwrap_or_else(|| entry.client.to_string()),
        merged_clients,
        workspace_key: entry.workspace_key.map(|key| key.to_string()),
        workspace_label: entry.workspace_label.map(|label| label.to_string()),
        session_id: entry.session_id.map(|session| session.to_string()),
        model: entry.model.to_string(),
        provider,
        input: entry.input,
        output: entry.output,
        cache_read: entry.cache_read,
        cache_write: entry.cache_write,
        reasoning: entry.reasoning,
        message_count: entry.message_count,
        cost: entry.cost,
        performance: entry.performance,
    }
}

/// Month accumulator — port of `MonthAggregator` + the month fold.
#[derive(Default)]
pub(super) struct MonthAcc {
    models: HashSet<Arc<str>>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    message_count: i32,
    cost: f64,
}

impl MonthAcc {
    /// Month key when the message has a usable date; `None` when
    /// `date.len() < 7` (skipped, matching the old `continue`).
    pub(super) fn try_key_from_date(date: &str) -> Option<&str> {
        if date.len() >= 7 {
            Some(&date[..7])
        } else {
            None
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        self.models.insert(Arc::clone(&msg.model_id));
        self.input = checked_token_add(self.input, msg.tokens.input);
        self.output = checked_token_add(self.output, msg.tokens.output);
        self.cache_read = checked_token_add(self.cache_read, msg.tokens.cache_read);
        self.cache_write = checked_token_add(self.cache_write, msg.tokens.cache_write);
        self.message_count += msg.message_count.max(0);
        self.cost += msg.cost;
    }
}

/// Build the sorted `Vec<MonthlyUsage>` from a finished month map. `models`
/// is sorted to be byte-identical with `get_monthly_report` (the unsorted
/// HashSet->Vec nondeterminism is resolved here and in the live fold
/// together — see the C1.5 BLOCKER fix).
pub(super) fn finish_month_map(month_map: HashMap<String, MonthAcc>) -> Vec<MonthlyUsage> {
    let mut entries: Vec<MonthlyUsage> = month_map
        .into_iter()
        .map(|(month, agg)| MonthlyUsage {
            month,
            models: {
                let mut v: Vec<String> = agg
                    .models
                    .into_iter()
                    .map(|model| model.to_string())
                    .collect();
                v.sort();
                v
            },
            input: agg.input,
            output: agg.output,
            cache_read: agg.cache_read,
            cache_write: agg.cache_write,
            message_count: agg.message_count,
            cost: agg.cost,
        })
        .collect();
    entries.sort_by(|a, b| a.month.cmp(&b.month));
    entries
}

/// Hour bucket key — port of the `get_hourly_report` hour-key rule.
pub(super) fn hour_key(msg: &UnifiedMessage) -> Option<String> {
    use chrono::{Local, TimeZone};
    if msg.timestamp <= 0 {
        return None;
    }
    let ts_secs = msg.timestamp / 1000;
    match Local.timestamp_opt(ts_secs, 0) {
        chrono::LocalResult::Single(dt) => Some(dt.format("%Y-%m-%d %H:00").to_string()),
        _ => None,
    }
}

/// Hour accumulator — port of `HourAggregator` + the hour fold.
#[derive(Default)]
pub(super) struct HourAcc {
    clients: HashSet<Arc<str>>,
    models: HashSet<Arc<str>>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    message_count: i32,
    turn_count: i32,
    cost: f64,
}

impl HourAcc {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        self.clients.insert(Arc::clone(&msg.client));
        self.models.insert(Arc::clone(&msg.model_id));
        self.input = checked_token_add(self.input, msg.tokens.input);
        self.output = checked_token_add(self.output, msg.tokens.output);
        self.cache_read = checked_token_add(self.cache_read, msg.tokens.cache_read);
        self.cache_write = checked_token_add(self.cache_write, msg.tokens.cache_write);
        self.reasoning = checked_token_add(self.reasoning, msg.tokens.reasoning);
        self.message_count += msg.message_count.max(0);
        if msg.is_turn_start {
            self.turn_count += 1;
        }
        self.cost += msg.cost;
    }
}

/// Build the sorted `Vec<HourlyUsage>` from a finished hour map. Sorted by the
/// full `"YYYY-MM-DD HH:00"` key (matching `get_hourly_report`), then relabeled
/// to `"MM-DD HH:00"`.
pub(super) fn finish_hour_map(hour_map: HashMap<String, HourAcc>) -> Vec<HourlyUsage> {
    let mut entries: Vec<(String, HourlyUsage)> = Vec::with_capacity(hour_map.len());
    for (hour, agg) in hour_map {
        let entry = HourlyUsage {
            hour: hourly_label(&hour),
            clients: {
                let mut v: Vec<String> = agg
                    .clients
                    .into_iter()
                    .map(|client| client.to_string())
                    .collect();
                v.sort();
                v
            },
            models: {
                let mut v: Vec<String> = agg
                    .models
                    .into_iter()
                    .map(|model| model.to_string())
                    .collect();
                v.sort();
                v
            },
            input: agg.input,
            output: agg.output,
            cache_read: agg.cache_read,
            cache_write: agg.cache_write,
            message_count: agg.message_count,
            turn_count: agg.turn_count,
            reasoning: agg.reasoning,
            cost: agg.cost,
        };
        entries.push((hour, entry));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, entry)| entry).collect()
}

type ClientModelIdentity = (Arc<str>, Arc<str>);
type ClientProviderModelIdentity = (Arc<str>, Arc<str>, Arc<str>);

#[derive(Default)]
pub(super) struct DailyAcc {
    totals: DailyTotals,
    token_breakdown: TokenBreakdown,
    clients: HashMap<ClientModelIdentity, DailyClientContributionAcc>,
}

struct DailyClientContributionAcc {
    client: Arc<str>,
    model_id: Arc<str>,
    providers: IdentitySet<Arc<str>>,
    tokens: TokenBreakdown,
    cost: f64,
    messages: i32,
}

impl DailyAcc {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let total_tokens = msg.tokens.total();

        self.totals.tokens = checked_token_add(self.totals.tokens, total_tokens);
        self.totals.cost += msg.cost;
        self.totals.messages = self
            .totals
            .messages
            .saturating_add(msg.message_count.max(0));

        add_token_breakdown(&mut self.token_breakdown, &msg.tokens);

        let key = (Arc::clone(&msg.client), Arc::clone(&msg.model_id));
        let client_entry = self
            .clients
            .entry(key)
            .or_insert_with(|| DailyClientContributionAcc {
                client: Arc::clone(&msg.client),
                model_id: Arc::clone(&msg.model_id),
                providers: IdentitySet::default(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                messages: 0,
            });
        client_entry.providers.insert(Arc::clone(&msg.provider_id));

        add_token_breakdown(&mut client_entry.tokens, &msg.tokens);
        client_entry.cost += msg.cost;
        client_entry.messages = client_entry
            .messages
            .saturating_add(msg.message_count.max(0));
    }

    fn into_contribution(self, date: String) -> DailyContribution {
        let token_breakdown = TokenBreakdown {
            input: self.token_breakdown.input.max(0),
            output: self.token_breakdown.output.max(0),
            cache_read: self.token_breakdown.cache_read.max(0),
            cache_write: self.token_breakdown.cache_write.max(0),
            reasoning: self.token_breakdown.reasoning.max(0),
        };

        let mut clients: Vec<ClientContribution> = self
            .clients
            .into_values()
            .map(|contribution| ClientContribution {
                client: contribution.client.to_string(),
                model_id: contribution.model_id.to_string(),
                provider_id: contribution.providers.into_sorted_string(),
                tokens: TokenBreakdown {
                    input: contribution.tokens.input.max(0),
                    output: contribution.tokens.output.max(0),
                    cache_read: contribution.tokens.cache_read.max(0),
                    cache_write: contribution.tokens.cache_write.max(0),
                    reasoning: contribution.tokens.reasoning.max(0),
                },
                cost: contribution.cost.max(0.0),
                messages: contribution.messages,
            })
            .collect();
        clients.sort_by(|a, b| {
            a.client
                .cmp(&b.client)
                .then_with(|| a.model_id.cmp(&b.model_id))
                .then_with(|| a.provider_id.cmp(&b.provider_id))
        });

        DailyContribution {
            date,
            totals: DailyTotals {
                tokens: self.totals.tokens.max(0),
                cost: self.totals.cost.max(0.0),
                messages: self.totals.messages.max(0),
            },
            intensity: 0,
            token_breakdown,
            clients,
            active_time_ms: None,
        }
    }
}

pub(super) fn finish_daily_map(daily_map: HashMap<String, DailyAcc>) -> Vec<DailyContribution> {
    let mut contributions: Vec<DailyContribution> = daily_map
        .into_iter()
        .map(|(date, acc)| acc.into_contribution(date))
        .collect();
    contributions.sort_by(|a, b| a.date.cmp(&b.date));
    calculate_intensities(&mut contributions);
    contributions
}

fn calculate_intensities(contributions: &mut [DailyContribution]) {
    let max_cost = contributions
        .iter()
        .map(|c| c.totals.cost)
        .fold(0.0_f64, f64::max);

    if max_cost == 0.0 {
        return;
    }

    for contribution in contributions {
        let ratio = contribution.totals.cost / max_cost;
        contribution.intensity = if ratio >= 0.75 {
            4
        } else if ratio >= 0.5 {
            3
        } else if ratio >= 0.25 {
            2
        } else if ratio > 0.0 {
            1
        } else {
            0
        };
    }
}

pub(super) struct SessionAcc {
    totals: DailyTotals,
    token_breakdown: TokenBreakdown,
    clients: HashMap<ClientProviderModelIdentity, SessionClientContributionAcc>,
    next_sequence: usize,
    first_seen: i64,
    last_seen: i64,
}

struct SessionClientContributionAcc {
    client: Arc<str>,
    provider_id: Arc<str>,
    model_id: Arc<str>,
    tokens: TokenBreakdown,
    cost: f64,
    messages: i32,
    first_seen: usize,
}

impl Default for SessionAcc {
    fn default() -> Self {
        Self {
            totals: DailyTotals::default(),
            token_breakdown: TokenBreakdown::default(),
            clients: HashMap::with_capacity(2),
            next_sequence: 0,
            first_seen: i64::MAX,
            last_seen: i64::MIN,
        }
    }
}

/// Ordering used when selecting the representative identity for a session.
/// Larger comparable costs rank first; NaN ranks after every comparable value,
/// including negative infinity. Equal ranks retain the earlier first-seen and
/// structured-identity tie breakers at the call site.
fn compare_session_cost_rank(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (false, false) => right
            .partial_cmp(&left)
            .expect("non-NaN session costs are comparable"),
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        (true, true) => std::cmp::Ordering::Equal,
    }
}

impl SessionAcc {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("session aggregation sequence exceeds usize::MAX");
        let total_tokens = msg.tokens.total();

        self.totals.tokens = checked_token_add(self.totals.tokens, total_tokens);
        self.totals.cost += msg.cost;
        self.totals.messages = self
            .totals
            .messages
            .saturating_add(msg.message_count.max(0));

        add_token_breakdown(&mut self.token_breakdown, &msg.tokens);

        let key = (
            Arc::clone(&msg.client),
            Arc::clone(&msg.provider_id),
            Arc::clone(&msg.model_id),
        );
        let client_entry =
            self.clients
                .entry(key)
                .or_insert_with(|| SessionClientContributionAcc {
                    client: Arc::clone(&msg.client),
                    provider_id: Arc::clone(&msg.provider_id),
                    model_id: Arc::clone(&msg.model_id),
                    tokens: TokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                    first_seen: sequence,
                });

        add_token_breakdown(&mut client_entry.tokens, &msg.tokens);
        client_entry.cost += msg.cost;
        client_entry.messages = client_entry
            .messages
            .saturating_add(msg.message_count.max(0));

        let secs = if msg.timestamp.abs() > 1_000_000_000_000 {
            msg.timestamp / 1000
        } else {
            msg.timestamp
        };
        if secs < self.first_seen {
            self.first_seen = secs;
        }
        if secs > self.last_seen {
            self.last_seen = secs;
        }
    }

    fn into_contribution(self, session_id: String) -> SessionContribution {
        let top_identity = self
            .clients
            .iter()
            .min_by(|(left_identity, left), (right_identity, right)| {
                compare_session_cost_rank(left.cost, right.cost)
                    .then_with(|| left.first_seen.cmp(&right.first_seen))
                    .then_with(|| left_identity.cmp(right_identity))
            })
            .map(|(identity, _)| identity.clone())
            .expect("session accumulator contains at least one identity");
        let token_breakdown = TokenBreakdown {
            input: self.token_breakdown.input.max(0),
            output: self.token_breakdown.output.max(0),
            cache_read: self.token_breakdown.cache_read.max(0),
            cache_write: self.token_breakdown.cache_write.max(0),
            reasoning: self.token_breakdown.reasoning.max(0),
        };

        let mut clients: Vec<ClientContribution> = self
            .clients
            .into_values()
            .map(|contribution| ClientContribution {
                client: contribution.client.to_string(),
                model_id: contribution.model_id.to_string(),
                provider_id: contribution.provider_id.to_string(),
                tokens: TokenBreakdown {
                    input: contribution.tokens.input.max(0),
                    output: contribution.tokens.output.max(0),
                    cache_read: contribution.tokens.cache_read.max(0),
                    cache_write: contribution.tokens.cache_write.max(0),
                    reasoning: contribution.tokens.reasoning.max(0),
                },
                cost: contribution.cost.max(0.0),
                messages: contribution.messages,
            })
            .collect();
        clients.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.client.cmp(&b.client))
                .then_with(|| a.model_id.cmp(&b.model_id))
                .then_with(|| a.provider_id.cmp(&b.provider_id))
        });

        let first_seen = if self.first_seen == i64::MAX {
            0
        } else {
            self.first_seen
        };
        let last_seen = if self.last_seen == i64::MIN {
            0
        } else {
            self.last_seen
        };

        let (client, provider, model) = top_identity;
        SessionContribution {
            session_id,
            client: client.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            totals: DailyTotals {
                tokens: self.totals.tokens.max(0),
                cost: self.totals.cost.max(0.0),
                messages: self.totals.messages.max(0),
            },
            token_breakdown,
            clients,
            first_seen,
            last_seen,
        }
    }
}

pub(super) fn finish_session_map(
    session_map: HashMap<Arc<str>, SessionAcc>,
) -> Vec<SessionContribution> {
    let mut contributions: Vec<SessionContribution> = session_map
        .into_iter()
        .map(|(session_id, acc)| acc.into_contribution(session_id.to_string()))
        .collect();
    contributions.sort_by(|a, b| {
        b.last_seen
            .cmp(&a.last_seen)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    contributions
}

#[derive(Default)]
pub(super) struct AgentEntries {
    agents: HashMap<(Arc<str>, String), AgentUsageAcc>,
}

struct AgentUsageAcc {
    tokens: TokenBreakdown,
    cost: f64,
    message_count: i32,
}

impl AgentEntries {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let Some(agent) = msg.agent.as_ref() else {
            return;
        };

        let normalized_agent = if msg.client.as_ref() == "opencode" {
            crate::sessions::normalize_opencode_agent_name(agent)
        } else if msg.client.as_ref() == "copilot" {
            crate::sessions::normalize_copilot_agent_name(agent)
        } else {
            crate::sessions::normalize_agent_name(agent)
        };
        let entry = self
            .agents
            .entry((Arc::clone(&msg.client), normalized_agent))
            .or_insert_with(|| AgentUsageAcc {
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                message_count: 0,
            });

        add_token_breakdown(&mut entry.tokens, &msg.tokens);
        entry.cost += msg.cost;
        entry.message_count += msg.message_count.max(0);
    }

    pub(super) fn finish(self) -> Vec<AgentUsage> {
        let mut agents: Vec<AgentUsage> = self
            .agents
            .into_iter()
            .map(|((client, agent), totals)| AgentUsage {
                client: client.to_string(),
                agent,
                tokens: totals.tokens,
                cost: totals.cost,
                message_count: totals.message_count,
            })
            .collect();
        agents.sort_by(|a, b| {
            a.client
                .cmp(&b.client)
                .then_with(|| b.message_count.cmp(&a.message_count))
                .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                .then_with(|| a.agent.cmp(&b.agent))
        });
        agents
    }
}

pub(super) struct TimeBufferedViews {
    pub(super) graph: Option<GraphResult>,
    pub(super) time_metrics: Option<TimeMetricsReport>,
    pub(super) daily_contributions: Option<Vec<DailyContribution>>,
}

/// Materialize graph/time outputs from the buffered temporal projection.
/// Sessionize-derived metrics are computed once and reused by graph +
/// time-metrics outputs when both are requested.
pub(super) fn finish_graph_and_time_from_events(
    events: &[SessionTimeEvent],
    views: ViewSet,
    daily_contributions: Option<Vec<DailyContribution>>,
) -> TimeBufferedViews {
    let intervals = crate::sessionize::activity_intervals_from_time_events(
        events,
        crate::sessionize::DEFAULT_IDLE_GAP_MS,
    );
    let time_metrics_value = crate::sessionize::compute_time_metrics_for_activity(
        &intervals,
        crate::sessionize::DEFAULT_IDLE_GAP_MS,
    );
    let daily_active_time = views
        .contains(ViewSet::GRAPH)
        .then(|| crate::sessionize::compute_daily_active_time_for_activity(&intervals));

    let graph = views.contains(ViewSet::GRAPH).then(|| {
        let contributions = daily_contributions.expect("graph view requested");
        let mut result = finish_graph_result(contributions, 0);
        result.time_metrics = Some(time_metrics_value.clone());
        if let Some(daily_active_time) = &daily_active_time {
            for contribution in &mut result.contributions {
                if let Some(&ms) = daily_active_time.get(&contribution.date) {
                    contribution.active_time_ms = Some(ms);
                }
            }
        }
        result
    });

    let daily_contributions = graph.as_ref().map(|graph| graph.contributions.clone());
    let time_metrics = views
        .contains(ViewSet::TIME_METRICS)
        .then_some(TimeMetricsReport {
            metrics: time_metrics_value,
            processing_time_ms: 0,
        });

    TimeBufferedViews {
        graph,
        time_metrics,
        daily_contributions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(
        client: &str,
        provider: &str,
        session: &str,
        model: &str,
        input: i64,
    ) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            model,
            provider,
            session,
            1_735_689_600_000,
            TokenBreakdown {
                input,
                ..TokenBreakdown::default()
            },
            input as f64,
        )
    }

    #[test]
    fn model_report_preserves_structured_buckets_with_colliding_legacy_text() {
        let cases = [
            (
                GroupBy::ClientModel,
                message("a:b", "first", "same", "c", 10),
                message("a", "second", "same", "b:c", 20),
            ),
            (
                GroupBy::ClientProviderModel,
                message("a", "b:c", "same", "d", 10),
                message("a", "b", "same", "c:d", 20),
            ),
            (
                GroupBy::Session,
                message("a", "first", "b:c", "d", 10),
                message("a", "second", "b", "c:d", 20),
            ),
            (
                GroupBy::ClientSession,
                message("a", "first", "b:c", "d", 10),
                message("a", "second", "b", "c:d", 20),
            ),
        ];

        for (group_by, first, second) in cases {
            let mut entries = ModelEntries::new(group_by);
            entries.push(&first);
            entries.push(&second);
            let entries = entries.finish();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].input, 20);
            assert_eq!(entries[0].cost, 20.0);
            assert_eq!(entries[0].message_count, 1);
            assert_eq!(entries[1].input, 10);
            assert_eq!(entries[1].cost, 10.0);
            assert_eq!(entries[1].message_count, 1);
        }
    }

    #[test]
    fn model_report_keeps_unknown_and_known_workspace_buckets_distinct() {
        let mut unknown = message("client", "provider", "session", "model", 10);
        unknown.workspace_key = None;
        unknown.workspace_label = None;
        let mut known = message("client", "provider", "session", "model", 20);
        known.workspace_key = Some(Arc::from(""));
        known.workspace_label = Some(Arc::from("Empty workspace key"));

        let mut entries = ModelEntries::new(GroupBy::WorkspaceModel);
        entries.push(&unknown);
        entries.push(&known);
        let entries = entries.finish();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].workspace_key.as_deref(), Some(""));
        assert_eq!(entries[1].workspace_key, None);
    }

    fn session_message(client: &str, provider: &str, model: &str, cost: f64) -> UnifiedMessage {
        let mut message = message(client, provider, "session", model, 1);
        message.cost = cost;
        message
    }

    fn session_identity(acc: SessionAcc) -> (String, String, String) {
        let contribution = acc.into_contribution("session".to_string());
        (
            contribution.client,
            contribution.provider,
            contribution.model,
        )
    }

    #[test]
    fn session_identity_uses_final_accumulated_cost() {
        let mut acc = SessionAcc::default();
        acc.push(&session_message("eventual", "provider", "model", 4.0));
        acc.push(&session_message("early", "provider", "model", 7.0));
        acc.push(&session_message("eventual", "provider", "model", 4.0));

        assert_eq!(
            session_identity(acc),
            ("eventual".into(), "provider".into(), "model".into())
        );
    }

    #[test]
    fn session_identity_prefers_infinite_and_finite_costs_over_nan() {
        for preferred_cost in [f64::NEG_INFINITY, 0.0, f64::INFINITY] {
            let mut acc = SessionAcc::default();
            acc.push(&session_message("nan", "provider", "model", f64::NAN));
            acc.push(&session_message(
                "comparable",
                "provider",
                "model",
                preferred_cost,
            ));

            assert_eq!(session_identity(acc).0, "comparable");
        }
    }

    #[test]
    fn session_identity_breaks_equal_cost_ties_by_first_seen() {
        let mut acc = SessionAcc::default();
        acc.push(&session_message("first", "z-provider", "z-model", 2.0));
        acc.push(&session_message("second", "a-provider", "a-model", 2.0));

        assert_eq!(session_identity(acc).0, "first");
    }

    #[test]
    fn session_identity_uses_structured_identity_as_the_final_tie_break() {
        let mut acc = SessionAcc::default();
        acc.push(&session_message("z-client", "provider", "model", 2.0));
        acc.push(&session_message("a-client", "provider", "model", 2.0));
        for contribution in acc.clients.values_mut() {
            contribution.first_seen = 0;
        }

        assert_eq!(session_identity(acc).0, "a-client");
    }

    #[test]
    fn all_nan_session_identity_is_deterministic_and_does_not_panic() {
        let mut acc = SessionAcc::default();
        acc.push(&session_message("first", "z-provider", "z-model", f64::NAN));
        acc.push(&session_message(
            "second",
            "a-provider",
            "a-model",
            f64::NAN,
        ));

        assert_eq!(session_identity(acc).0, "first");
    }

    #[test]
    fn single_nan_session_identity_is_non_empty_and_does_not_panic() {
        let mut acc = SessionAcc::default();
        acc.push(&session_message("only", "provider", "model", f64::NAN));

        assert_eq!(
            session_identity(acc),
            ("only".into(), "provider".into(), "model".into())
        );
    }
}
