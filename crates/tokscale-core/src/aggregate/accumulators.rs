//! Per-view accumulators ported verbatim from the old fold sites
//! (`aggregate_model_usage_entries`, `MonthAggregator`/month fold,
//! `HourAggregator`/hour fold). The graph/session/time-metrics views are
//! inherently two-pass; the engine buffers messages and replays the existing
//! `aggregator::`/`sessionize::` functions (see `engine.rs`).

use std::collections::{HashMap, HashSet};

use crate::{
    aggregator, normalize_model_for_grouping, normalize_provider_for_grouping,
    ordered_clients_by_token_contribution, positive_token_total, workspace_bucket,
    workspace_model_bucket_key, ClientContributionOrder, DailyContribution, GraphResult, GroupBy,
    HourlyUsage, ModelPerformance, ModelUsage, MonthlyUsage, SessionContribution,
    TimeMetricsReport, UnifiedMessage, ViewSet,
};

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
        let normalized = normalize_model_for_grouping(&msg.model_id);
        let provider = normalize_provider_for_grouping(&msg.provider_id);
        let (workspace_group_key, workspace_key, workspace_label) = workspace_bucket(msg);
        let (key, merge_clients) = match group_by {
            GroupBy::Model => (normalized.clone(), true),
            GroupBy::ClientModel => (format!("{}:{}", msg.client, normalized), false),
            GroupBy::ClientProviderModel => {
                (format!("{}:{}:{}", msg.client, provider, normalized), false)
            }
            GroupBy::WorkspaceModel => (
                workspace_model_bucket_key(&workspace_group_key, &normalized),
                true,
            ),
            GroupBy::Session => (format!("{}:{}", msg.session_id, normalized), false),
            GroupBy::ClientSession => (
                format!("{}:{}:{}", msg.client, msg.session_id, normalized),
                false,
            ),
        };
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
                model: normalized.clone(),
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
        self.models
            .insert(normalize_model_for_grouping(&msg.model_id));
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
        self.models
            .insert(normalize_model_for_grouping(&msg.model_id));
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

pub(super) struct BufferedViews {
    pub(super) graph: Option<GraphResult>,
    pub(super) session_contributions: Option<Vec<SessionContribution>>,
    pub(super) time_metrics: Option<TimeMetricsReport>,
    pub(super) daily_contributions: Option<Vec<DailyContribution>>,
}

/// Materialize the buffered two-pass views. Sessionize-derived metrics are
/// computed once and reused by graph + time-metrics outputs when both are
/// requested.
pub(super) fn finish_buffered_views(messages: &[UnifiedMessage], views: ViewSet) -> BufferedViews {
    let needs_session_metrics =
        views.contains(ViewSet::GRAPH) || views.contains(ViewSet::TIME_METRICS);
    let (time_metrics_value, daily_active_time) = if needs_session_metrics {
        let intervals =
            crate::sessionize::sessionize(messages, crate::sessionize::DEFAULT_IDLE_GAP_MS);
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
        let contributions = aggregator::aggregate_by_date(messages.to_vec());
        let mut result = aggregator::generate_graph_result(contributions, 0);
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
    let session_contributions = views
        .contains(ViewSet::SESSIONS)
        .then(|| aggregator::aggregate_by_session(messages.to_vec()));
    let time_metrics = views
        .contains(ViewSet::TIME_METRICS)
        .then(|| TimeMetricsReport {
            metrics: time_metrics_value.expect("time metrics requested"),
            processing_time_ms: 0,
        });

    BufferedViews {
        graph,
        session_contributions,
        time_metrics,
        daily_contributions,
    }
}
