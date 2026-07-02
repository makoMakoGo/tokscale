//! Per-view accumulators derived from the old fold sites
//! (`aggregate_model_usage_entries`, `MonthAggregator`/month fold,
//! `HourAggregator`/hour fold, and the daily graph fold). Session and
//! time-metrics views still need their existing two-pass projection.

use std::collections::{HashMap, HashSet};

use crate::{
    aggregate::keys::{grouped_model_bucket_key, workspace_bucket},
    aggregator::{calculate_summary, calculate_years},
    normalize_provider_for_grouping, ordered_clients_by_token_contribution, positive_token_total,
    sessionize::SessionTimeEvent,
    ClientContribution, ClientContributionOrder, DailyContribution, DailyTotals, GraphMeta,
    GraphResult, GroupBy, HourlyUsage, ModelPerformance, ModelUsage, MonthlyUsage,
    SessionContribution, TimeMetricsReport, TokenBreakdown, UnifiedMessage, ViewSet,
};

use super::views::AgentUsage;

fn hourly_label(hour_key: &str) -> String {
    // `hourly_report_label` returns `key[5..]` ("MM-DD HH:00"). Kept inline to
    // avoid a private cross-module call; identical slice.
    hour_key.get(5..).map(str::to_string).unwrap_or_default()
}

/// Core model-report accumulator — port of `aggregate_model_usage_entries`.
/// `push` is the per-message fold; `finish` is the finalize + cost sort.
pub(super) struct ModelEntries {
    group_by: GroupBy,
    model_map: HashMap<String, ModelUsage>,
    client_totals_by_entry: HashMap<String, HashMap<String, ClientContributionOrder>>,
}

impl ModelEntries {
    pub(super) fn new(group_by: GroupBy) -> Self {
        Self {
            group_by,
            model_map: HashMap::new(),
            client_totals_by_entry: HashMap::new(),
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let group_by = &self.group_by;
        let canonical_model_id = msg.model_id.to_string();
        let provider = normalize_provider_for_grouping(&msg.provider_id);
        let (workspace_group_key, workspace_key, workspace_label) = workspace_bucket(msg);
        let (key, merge_clients) = grouped_model_bucket_key(
            group_by,
            &msg.client,
            &provider,
            &workspace_group_key,
            &msg.session_id,
            &canonical_model_id,
        );
        let session_grouped = matches!(group_by, GroupBy::Session | GroupBy::ClientSession);
        let entry = self
            .model_map
            .entry(key.clone())
            .or_insert_with(|| ModelUsage {
                client: msg.client.to_string(),
                merged_clients: if merge_clients {
                    Some(msg.client.to_string())
                } else {
                    None
                },
                workspace_key: if matches!(group_by, GroupBy::WorkspaceModel) {
                    workspace_key.clone()
                } else {
                    None
                },
                workspace_label: if matches!(group_by, GroupBy::WorkspaceModel) {
                    Some(workspace_label.clone())
                } else {
                    None
                },
                session_id: if session_grouped {
                    Some(msg.session_id.to_string())
                } else {
                    None
                },
                model: canonical_model_id.clone(),
                provider: provider.clone(),
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
                message_count: 0,
                cost: 0.0,
                performance: ModelPerformance::default(),
            });

        if merge_clients {
            let client_totals = self.client_totals_by_entry.entry(key.clone()).or_default();
            let client_count = client_totals.len();
            let totals = client_totals
                .entry(msg.client.to_string())
                .or_insert_with(|| ClientContributionOrder {
                    first_seen: client_count,
                    total_tokens: 0,
                });
            totals.total_tokens = totals
                .total_tokens
                .saturating_add(msg.tokens.total().max(0) as u64);
        }

        if *group_by != GroupBy::ClientProviderModel
            && !entry.provider.split(", ").any(|p| p == provider)
        {
            entry.provider = format!("{}, {}", entry.provider, provider);
        }

        entry.input += msg.tokens.input;
        entry.output += msg.tokens.output;
        entry.cache_read += msg.tokens.cache_read;
        entry.cache_write += msg.tokens.cache_write;
        entry.reasoning += msg.tokens.reasoning;
        entry.message_count += msg.message_count.max(0);
        entry.cost += msg.cost;
        entry
            .performance
            .record_message(positive_token_total(&msg.tokens), msg.duration_ms);
    }

    pub(super) fn finish(self) -> Vec<ModelUsage> {
        let Self {
            model_map,
            client_totals_by_entry,
            ..
        } = self;
        let mut entries: Vec<ModelUsage> = model_map
            .into_iter()
            .map(|(key, mut entry)| {
                if let Some(client_totals) = client_totals_by_entry.get(&key) {
                    let ordered_clients = ordered_clients_by_token_contribution(client_totals);
                    entry.client = ordered_clients.clone();
                    if let Some(merged_clients) = &mut entry.merged_clients {
                        *merged_clients = ordered_clients;
                    }
                }

                let total_tokens = entry.input.max(0)
                    + entry.output.max(0)
                    + entry.cache_read.max(0)
                    + entry.cache_write.max(0)
                    + entry.reasoning.max(0);
                entry.performance.finalize(total_tokens);
                let mut providers: Vec<&str> = entry.provider.split(", ").collect();
                providers.sort_unstable();
                providers.dedup();
                entry.provider = providers.join(", ");
                entry
            })
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

/// Month accumulator — port of `MonthAggregator` + the month fold.
#[derive(Default)]
pub(super) struct MonthAcc {
    models: HashSet<String>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    message_count: i32,
    cost: f64,
}

impl MonthAcc {
    /// `(month_key, Some(acc))` when the message has a usable month; `None`
    /// when `date.len() < 7` (skipped, matching the old `continue`).
    pub(super) fn try_key(msg: &UnifiedMessage) -> Option<String> {
        let date = msg.date_string();
        if date.len() >= 7 {
            Some(date[..7].to_string())
        } else {
            None
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        self.models.insert(msg.model_id.to_string());
        self.input += msg.tokens.input;
        self.output += msg.tokens.output;
        self.cache_read += msg.tokens.cache_read;
        self.cache_write += msg.tokens.cache_write;
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
                let mut v: Vec<String> = agg.models.into_iter().collect();
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
pub(super) fn hour_key(msg: &UnifiedMessage) -> String {
    use chrono::{Local, TimeZone};
    if msg.timestamp > 0 {
        let ts_secs = msg.timestamp / 1000;
        match Local.timestamp_opt(ts_secs, 0) {
            chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:00").to_string(),
            _ => format!("{} 00:00", msg.date_string()),
        }
    } else {
        format!("{} 00:00", msg.date_string())
    }
}

/// Hour accumulator — port of `HourAggregator` + the hour fold.
#[derive(Default)]
pub(super) struct HourAcc {
    clients: HashSet<String>,
    models: HashSet<String>,
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
        self.clients.insert(msg.client.to_string());
        self.models.insert(msg.model_id.to_string());
        self.input += msg.tokens.input;
        self.output += msg.tokens.output;
        self.cache_read += msg.tokens.cache_read;
        self.cache_write += msg.tokens.cache_write;
        self.reasoning += msg.tokens.reasoning;
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
                let mut v: Vec<String> = agg.clients.into_iter().collect();
                v.sort();
                v
            },
            models: {
                let mut v: Vec<String> = agg.models.into_iter().collect();
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

#[derive(Default)]
pub(super) struct DailyAcc {
    totals: DailyTotals,
    token_breakdown: TokenBreakdown,
    clients: HashMap<String, ClientContribution>,
}

impl DailyAcc {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let total_tokens = msg
            .tokens
            .input
            .saturating_add(msg.tokens.output)
            .saturating_add(msg.tokens.cache_read)
            .saturating_add(msg.tokens.cache_write)
            .saturating_add(msg.tokens.reasoning);

        self.totals.tokens = self.totals.tokens.saturating_add(total_tokens);
        self.totals.cost += msg.cost;
        self.totals.messages = self
            .totals
            .messages
            .saturating_add(msg.message_count.max(0));

        self.token_breakdown.input = self.token_breakdown.input.saturating_add(msg.tokens.input);
        self.token_breakdown.output = self
            .token_breakdown
            .output
            .saturating_add(msg.tokens.output);
        self.token_breakdown.cache_read = self
            .token_breakdown
            .cache_read
            .saturating_add(msg.tokens.cache_read);
        self.token_breakdown.cache_write = self
            .token_breakdown
            .cache_write
            .saturating_add(msg.tokens.cache_write);
        self.token_breakdown.reasoning = self
            .token_breakdown
            .reasoning
            .saturating_add(msg.tokens.reasoning);

        let model_id = msg.model_id.as_ref();
        let key = format!("{}:{}", msg.client, model_id);
        let provider_id = normalize_provider_for_grouping(&msg.provider_id);
        let client_entry = self
            .clients
            .entry(key)
            .or_insert_with(|| ClientContribution {
                client: msg.client.to_string(),
                model_id: model_id.to_string(),
                provider_id: provider_id.clone(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                messages: 0,
            });

        if !client_entry
            .provider_id
            .split(", ")
            .any(|p| p == provider_id)
        {
            client_entry.provider_id = format!("{}, {}", client_entry.provider_id, provider_id);
        }

        client_entry.tokens.input = client_entry.tokens.input.saturating_add(msg.tokens.input);
        client_entry.tokens.output = client_entry.tokens.output.saturating_add(msg.tokens.output);
        client_entry.tokens.cache_read = client_entry
            .tokens
            .cache_read
            .saturating_add(msg.tokens.cache_read);
        client_entry.tokens.cache_write = client_entry
            .tokens
            .cache_write
            .saturating_add(msg.tokens.cache_write);
        client_entry.tokens.reasoning = client_entry
            .tokens
            .reasoning
            .saturating_add(msg.tokens.reasoning);
        client_entry.cost += msg.cost;
        client_entry.messages = client_entry
            .messages
            .saturating_add(msg.message_count.max(0));

        let mut providers: Vec<&str> = client_entry.provider_id.split(", ").collect();
        providers.sort_unstable();
        providers.dedup();
        client_entry.provider_id = providers.join(", ");
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
            .map(|mut contribution| {
                contribution.tokens.input = contribution.tokens.input.max(0);
                contribution.tokens.output = contribution.tokens.output.max(0);
                contribution.tokens.cache_read = contribution.tokens.cache_read.max(0);
                contribution.tokens.cache_write = contribution.tokens.cache_write.max(0);
                contribution.tokens.reasoning = contribution.tokens.reasoning.max(0);
                contribution.cost = contribution.cost.max(0.0);
                contribution
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

fn finish_graph_result(
    contributions: Vec<DailyContribution>,
    processing_time_ms: u32,
) -> GraphResult {
    let summary = calculate_summary(&contributions);
    let years = calculate_years(&contributions);
    let date_range_start = contributions
        .first()
        .map(|c| c.date.clone())
        .unwrap_or_default();
    let date_range_end = contributions
        .last()
        .map(|c| c.date.clone())
        .unwrap_or_default();

    GraphResult {
        meta: GraphMeta {
            generated_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            date_range_start,
            date_range_end,
            processing_time_ms,
        },
        summary,
        years,
        contributions,
        time_metrics: None,
    }
}

pub(super) struct SessionAcc {
    totals: DailyTotals,
    token_breakdown: TokenBreakdown,
    clients: HashMap<String, ClientContribution>,
    top_client: String,
    top_provider: String,
    top_model: String,
    top_cost: f64,
    first_seen: i64,
    last_seen: i64,
}

impl Default for SessionAcc {
    fn default() -> Self {
        Self {
            totals: DailyTotals::default(),
            token_breakdown: TokenBreakdown::default(),
            clients: HashMap::with_capacity(2),
            top_client: String::new(),
            top_provider: String::new(),
            top_model: String::new(),
            top_cost: f64::NEG_INFINITY,
            first_seen: i64::MAX,
            last_seen: i64::MIN,
        }
    }
}

impl SessionAcc {
    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let total_tokens = msg
            .tokens
            .input
            .saturating_add(msg.tokens.output)
            .saturating_add(msg.tokens.cache_read)
            .saturating_add(msg.tokens.cache_write)
            .saturating_add(msg.tokens.reasoning);

        self.totals.tokens = self.totals.tokens.saturating_add(total_tokens);
        self.totals.cost += msg.cost;
        self.totals.messages = self
            .totals
            .messages
            .saturating_add(msg.message_count.max(0));

        self.token_breakdown.input = self.token_breakdown.input.saturating_add(msg.tokens.input);
        self.token_breakdown.output = self
            .token_breakdown
            .output
            .saturating_add(msg.tokens.output);
        self.token_breakdown.cache_read = self
            .token_breakdown
            .cache_read
            .saturating_add(msg.tokens.cache_read);
        self.token_breakdown.cache_write = self
            .token_breakdown
            .cache_write
            .saturating_add(msg.tokens.cache_write);
        self.token_breakdown.reasoning = self
            .token_breakdown
            .reasoning
            .saturating_add(msg.tokens.reasoning);

        let model_id = msg.model_id.as_ref();
        let provider_id = normalize_provider_for_grouping(&msg.provider_id);
        let key = format!("{}:{}:{}", msg.client, provider_id, model_id);
        let client_entry = self
            .clients
            .entry(key)
            .or_insert_with(|| ClientContribution {
                client: msg.client.to_string(),
                model_id: model_id.to_string(),
                provider_id: provider_id.clone(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                messages: 0,
            });
        client_entry.tokens.input = client_entry.tokens.input.saturating_add(msg.tokens.input);
        client_entry.tokens.output = client_entry.tokens.output.saturating_add(msg.tokens.output);
        client_entry.tokens.cache_read = client_entry
            .tokens
            .cache_read
            .saturating_add(msg.tokens.cache_read);
        client_entry.tokens.cache_write = client_entry
            .tokens
            .cache_write
            .saturating_add(msg.tokens.cache_write);
        client_entry.tokens.reasoning = client_entry
            .tokens
            .reasoning
            .saturating_add(msg.tokens.reasoning);
        client_entry.cost += msg.cost;
        client_entry.messages = client_entry
            .messages
            .saturating_add(msg.message_count.max(0));

        if client_entry.cost > self.top_cost {
            self.top_cost = client_entry.cost;
            self.top_client = client_entry.client.clone();
            self.top_provider = client_entry.provider_id.clone();
            self.top_model = client_entry.model_id.clone();
        }

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
            .map(|mut contribution| {
                contribution.tokens.input = contribution.tokens.input.max(0);
                contribution.tokens.output = contribution.tokens.output.max(0);
                contribution.tokens.cache_read = contribution.tokens.cache_read.max(0);
                contribution.tokens.cache_write = contribution.tokens.cache_write.max(0);
                contribution.tokens.reasoning = contribution.tokens.reasoning.max(0);
                contribution.cost = contribution.cost.max(0.0);
                contribution
            })
            .collect();
        clients.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.client.cmp(&b.client))
                .then_with(|| a.model_id.cmp(&b.model_id))
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

        SessionContribution {
            session_id,
            client: self.top_client,
            provider: self.top_provider,
            model: self.top_model,
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
    session_map: HashMap<String, SessionAcc>,
) -> Vec<SessionContribution> {
    let mut contributions: Vec<SessionContribution> = session_map
        .into_iter()
        .map(|(session_id, acc)| acc.into_contribution(session_id))
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
    agents: HashMap<(String, String), AgentUsage>,
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
        let client = msg.client.to_string();
        let entry = self
            .agents
            .entry((client.clone(), normalized_agent.clone()))
            .or_insert_with(|| AgentUsage {
                client,
                agent: normalized_agent,
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                message_count: 0,
            });

        entry.tokens.input += msg.tokens.input;
        entry.tokens.output += msg.tokens.output;
        entry.tokens.cache_read += msg.tokens.cache_read;
        entry.tokens.cache_write += msg.tokens.cache_write;
        entry.tokens.reasoning += msg.tokens.reasoning;
        entry.cost += msg.cost;
        entry.message_count += 1;
    }

    pub(super) fn finish(self) -> Vec<AgentUsage> {
        let mut agents: Vec<AgentUsage> = self.agents.into_values().collect();
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

pub(super) struct BufferedViews {
    pub(super) graph: Option<GraphResult>,
    pub(super) time_metrics: Option<TimeMetricsReport>,
    pub(super) daily_contributions: Option<Vec<DailyContribution>>,
}

/// Materialize the buffered two-pass views. Sessionize-derived metrics are
/// computed once and reused by graph + time-metrics outputs when both are
/// requested.
pub(super) fn finish_time_buffered_views(
    events: &[SessionTimeEvent],
    views: ViewSet,
    daily_contributions: Option<Vec<DailyContribution>>,
) -> BufferedViews {
    let needs_session_metrics =
        views.contains(ViewSet::GRAPH) || views.contains(ViewSet::TIME_METRICS);
    let (time_metrics_value, daily_active_time) = if needs_session_metrics {
        let intervals = crate::sessionize::sessionize_time_events(
            events,
            crate::sessionize::DEFAULT_IDLE_GAP_MS,
        );
        let metrics = crate::sessionize::compute_time_metrics(
            &intervals,
            crate::sessionize::DEFAULT_IDLE_GAP_MS,
        );
        let daily_active_time = views
            .contains(ViewSet::GRAPH)
            .then(|| crate::sessionize::compute_daily_active_time(&intervals));
        (Some(metrics), daily_active_time)
    } else {
        (None, None)
    };

    let graph = views.contains(ViewSet::GRAPH).then(|| {
        let contributions = daily_contributions.expect("graph view requested");
        let mut result = finish_graph_result(contributions, 0);
        result.time_metrics = time_metrics_value.clone();
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
        .then(|| TimeMetricsReport {
            metrics: time_metrics_value.expect("time metrics requested"),
            processing_time_ms: 0,
        });

    BufferedViews {
        graph,
        time_metrics,
        daily_contributions,
    }
}
