//! The TUI usage aggregation: one fold over `UnifiedMessage`s producing
//! [`crate::usage_views::UsageData`] (models/agents/daily/hourly/graph/streaks).
//! `AggregationEngine` owns this accumulator when `ViewSet::TUI` is requested;
//! CLI code drives that engine instead of carrying its own fold (#37).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday};

use crate::usage_views::{
    AgentEntry, ContributionDay, DailyModelInfo, DailySourceInfo, DailyUsage, HourlyModelInfo,
    HourlyUsage, PeriodKind, PeriodUsage, UsageData, UsageGraphData, UsageModelEntry,
    UsageTokenBreakdown,
};
use crate::{
    aggregate::keys::{
        daily_source_model_key, grouped_model_bucket_key, hourly_model_key, workspace_bucket,
    },
    normalize_model_for_grouping, normalize_provider_for_grouping,
    ordered_clients_by_token_contribution, sessions, ClientContributionOrder, GroupBy,
    ModelPerformance, UnifiedMessage,
};

pub use crate::aggregate::keys::UNKNOWN_WORKSPACE_LABEL;

fn positive_unified_token_total(tokens: &crate::TokenBreakdown) -> i64 {
    tokens.input.max(0)
        + tokens.output.max(0)
        + tokens.cache_read.max(0)
        + tokens.cache_write.max(0)
        + tokens.reasoning.max(0)
}

fn grouped_model_display_label(
    group_by: &GroupBy,
    workspace_label: Option<&str>,
    session_id: Option<&str>,
    model: &str,
) -> String {
    match group_by {
        GroupBy::WorkspaceModel => workspace_label
            .map(|label| format!("{label} / {model}"))
            .unwrap_or_else(|| model.to_string()),
        GroupBy::Session | GroupBy::ClientSession => session_id
            .map(|session_id| format!("{session_id} / {model}"))
            .unwrap_or_else(|| model.to_string()),
        GroupBy::Model | GroupBy::ClientModel | GroupBy::ClientProviderModel => model.to_string(),
    }
}

fn daily_source_model_display_name(
    group_by: &GroupBy,
    workspace_label: &str,
    session_id: &str,
    model: &str,
) -> String {
    grouped_model_display_label(group_by, Some(workspace_label), Some(session_id), model)
}

fn model_color_key(_group_by: &GroupBy, _provider_id: &str, model: &str) -> String {
    // All GroupBy variants currently reduce to the bare model name.
    model.to_string()
}

fn hourly_model_display_name(group_by: &GroupBy, model: &str) -> String {
    grouped_model_display_label(group_by, None, None, model)
}

/// Sanitize a message cost: non-finite/negative -> 0 (the TUI never shows debt).
fn sane_cost(cost: f64) -> f64 {
    if cost.is_finite() && cost > 0.0 {
        cost
    } else {
        0.0
    }
}

fn add_unified_tokens(target: &mut UsageTokenBreakdown, src: &crate::TokenBreakdown) {
    target.input = target.input.saturating_add(src.input.max(0) as u64);
    target.output = target.output.saturating_add(src.output.max(0) as u64);
    target.cache_read = target
        .cache_read
        .saturating_add(src.cache_read.max(0) as u64);
    target.cache_write = target
        .cache_write
        .saturating_add(src.cache_write.max(0) as u64);
    target.reasoning = target.reasoning.saturating_add(src.reasoning.max(0) as u64);
}

/// Convert Unix ms timestamp to a NaiveDateTime truncated to the hour (local tz).
fn timestamp_to_hour(timestamp_ms: i64) -> Option<NaiveDateTime> {
    if timestamp_ms <= 0 {
        return None;
    }
    let ts_secs = timestamp_ms / 1000;
    match Local.timestamp_opt(ts_secs, 0) {
        chrono::LocalResult::Single(dt) => {
            let naive = dt.naive_local();
            Some(
                naive
                    .date()
                    .and_hms_opt(naive.hour(), 0, 0)
                    .unwrap_or(naive),
            )
        }
        _ => None,
    }
}

/// Derive an hour-truncated NaiveDateTime from `msg.timestamp` when present,
/// otherwise fall back to the message's local-date 00:00 bucket so messages
/// with missing timestamps are not silently dropped from hourly aggregation.
fn hour_bucket_with_fallback(
    timestamp_ms: i64,
    fallback_date: Option<NaiveDate>,
) -> Option<NaiveDateTime> {
    if let Some(dt) = timestamp_to_hour(timestamp_ms) {
        return Some(dt);
    }
    fallback_date.and_then(|d| d.and_hms_opt(0, 0, 0))
}

// ---- period (monthly/weekly) view: folds the finished `daily` buckets ----

struct PeriodDescriptor {
    section_year: i32,
    ordinal: u32,
    label: String,
    short_label: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
}

fn add_tokens(target: &mut UsageTokenBreakdown, source: &UsageTokenBreakdown) {
    target.input = target.input.saturating_add(source.input);
    target.output = target.output.saturating_add(source.output);
    target.cache_read = target.cache_read.saturating_add(source.cache_read);
    target.cache_write = target.cache_write.saturating_add(source.cache_write);
    target.reasoning = target.reasoning.saturating_add(source.reasoning);
}

fn merge_daily_sources(
    target: &mut BTreeMap<String, DailySourceInfo>,
    source: &BTreeMap<String, DailySourceInfo>,
) {
    for (source_key, source_info) in source {
        let target_source = target
            .entry(source_key.clone())
            .or_insert_with(|| DailySourceInfo {
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                models: BTreeMap::new(),
            });
        add_tokens(&mut target_source.tokens, &source_info.tokens);
        target_source.cost += source_info.cost;
        for (model_key, model_info) in &source_info.models {
            let target_model = target_source
                .models
                .entry(model_key.clone())
                .or_insert_with(|| DailyModelInfo {
                    provider: model_info.provider.clone(),
                    display_name: model_info.display_name.clone(),
                    color_key: model_info.color_key.clone(),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                });
            add_tokens(&mut target_model.tokens, &model_info.tokens);
            target_model.cost += model_info.cost;
            target_model.messages = target_model.messages.saturating_add(model_info.messages);
        }
    }
}

fn period_descriptor(date: NaiveDate, kind: PeriodKind) -> Option<PeriodDescriptor> {
    match kind {
        PeriodKind::Monthly => monthly_period_descriptor(date),
        PeriodKind::Weekly => weekly_period_descriptor(date),
    }
}

fn monthly_period_descriptor(date: NaiveDate) -> Option<PeriodDescriptor> {
    let start_date = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)?;
    let end_date = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)?
    }
    .checked_sub_days(Days::new(1))?;
    Some(PeriodDescriptor {
        section_year: date.year(),
        ordinal: date.month(),
        label: start_date.format("%B").to_string(),
        short_label: start_date.format("%b").to_string(),
        start_date,
        end_date,
    })
}

fn weekly_period_descriptor(date: NaiveDate) -> Option<PeriodDescriptor> {
    let iso = date.iso_week();
    let start_date = NaiveDate::from_isoywd_opt(iso.year(), iso.week(), Weekday::Mon)?;
    let end_date = start_date.checked_add_days(Days::new(6))?;
    let label = format!(
        "W{:02} {} - {}",
        iso.week(),
        start_date.format("%b %d"),
        end_date.format("%b %d")
    );
    Some(PeriodDescriptor {
        section_year: iso.year(),
        ordinal: iso.week(),
        label,
        short_label: format!("W{:02}", iso.week()),
        start_date,
        end_date,
    })
}

/// Build monthly or weekly usage by folding the already-aggregated `daily`
/// buckets. See ADR 0010 for the coarse/fine boundary rationale.
pub fn build_period_usage(daily: &[DailyUsage], kind: PeriodKind) -> Vec<PeriodUsage> {
    let mut period_map: BTreeMap<(i32, u32), PeriodUsage> = BTreeMap::new();
    for day in daily {
        let Some(period) = period_descriptor(day.date, kind) else {
            continue;
        };
        let entry = period_map
            .entry((period.section_year, period.ordinal))
            .or_insert_with(|| PeriodUsage {
                section_year: period.section_year,
                section_label: period.section_year.to_string(),
                label: period.label,
                short_label: period.short_label,
                start_date: period.start_date,
                end_date: period.end_date,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                source_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
                active_days: 0,
            });
        add_tokens(&mut entry.tokens, &day.tokens);
        entry.cost += day.cost;
        entry.message_count = entry.message_count.saturating_add(day.message_count);
        entry.turn_count = entry.turn_count.saturating_add(day.turn_count);
        if day.message_count > 0 || day.turn_count > 0 || day.tokens.total() > 0 {
            entry.active_days = entry.active_days.saturating_add(1);
        }
        merge_daily_sources(&mut entry.source_breakdown, &day.source_breakdown);
    }
    let mut periods: Vec<PeriodUsage> = period_map.into_values().collect();
    periods.sort_by_key(|period| std::cmp::Reverse(period.start_date));
    periods
}

pub fn build_contribution_graph(daily: &[DailyUsage]) -> UsageGraphData {
    build_contribution_graph_for_today(daily, Local::now().date_naive())
}

pub fn build_contribution_graph_for_today(
    daily: &[DailyUsage],
    today: NaiveDate,
) -> UsageGraphData {
    if daily.is_empty() {
        return UsageGraphData { weeks: vec![] };
    }
    let days_to_sunday = today.weekday().num_days_from_sunday();
    let end_date = today;
    let start_date = end_date - chrono::Duration::days(364 + days_to_sunday as i64);
    let daily_map: HashMap<NaiveDate, &DailyUsage> = daily.iter().map(|d| (d.date, d)).collect();
    let max_cost = daily.iter().map(|d| d.cost).fold(0.0_f64, |a, b| a.max(b));
    let mut weeks: Vec<Vec<Option<ContributionDay>>> = Vec::new();
    let mut current_week: Vec<Option<ContributionDay>> = Vec::new();
    let mut current_date = start_date;
    while current_date <= end_date {
        let day = if let Some(usage) = daily_map.get(&current_date) {
            let raw_intensity = if max_cost > 0.0 {
                usage.cost / max_cost
            } else {
                0.0
            };
            let intensity = if raw_intensity.is_finite() {
                raw_intensity.clamp(0.0, 1.0)
            } else {
                0.0
            };
            Some(ContributionDay {
                date: current_date,
                tokens: usage.tokens.total(),
                cost: usage.cost,
                intensity,
            })
        } else {
            Some(ContributionDay {
                date: current_date,
                tokens: 0,
                cost: 0.0,
                intensity: 0.0,
            })
        };
        current_week.push(day);
        if current_date.weekday() == chrono::Weekday::Sat || current_date == end_date {
            weeks.push(current_week);
            current_week = Vec::new();
        }
        current_date += chrono::Duration::days(1);
    }
    UsageGraphData { weeks }
}

pub fn calculate_streaks(daily: &[DailyUsage]) -> (u32, u32) {
    calculate_streaks_for_today(daily, Local::now().date_naive())
}

pub fn calculate_streaks_for_today(daily: &[DailyUsage], today: NaiveDate) -> (u32, u32) {
    if daily.is_empty() {
        return (0, 0);
    }
    let dates: HashSet<NaiveDate> = daily.iter().map(|d| d.date).collect();
    let mut current_streak = 0u32;
    let mut check_date = today;
    while dates.contains(&check_date) {
        current_streak += 1;
        check_date -= chrono::Duration::days(1);
    }
    if current_streak == 0 {
        let yesterday = today - chrono::Duration::days(1);
        check_date = yesterday;
        while dates.contains(&check_date) {
            current_streak += 1;
            check_date -= chrono::Duration::days(1);
        }
    }
    let mut longest_streak = 0u32;
    let mut sorted_dates: Vec<NaiveDate> = dates.into_iter().collect();
    sorted_dates.sort();
    let mut streak = 0u32;
    let mut prev_date: Option<NaiveDate> = None;
    for date in sorted_dates {
        if let Some(prev) = prev_date {
            if date == prev + chrono::Duration::days(1) {
                streak += 1;
            } else {
                longest_streak = longest_streak.max(streak);
                streak = 1;
            }
        } else {
            streak = 1;
        }
        prev_date = Some(date);
    }
    longest_streak = longest_streak.max(streak);
    (current_streak, longest_streak)
}

// ---- hourly profile helpers (time-of-day / weekday / peak) ----

/// Time-of-day period bucket for the profile view.
#[derive(Debug, Clone)]
pub struct PeriodBucket {
    pub label: &'static str,
    pub hour_range: &'static str,
    pub total_tokens: u64,
}

/// Weekday bucket for the profile view.
#[derive(Debug, Clone)]
pub struct WeekdayBucket {
    pub day: &'static str,
    pub total_tokens: u64,
}

pub fn aggregate_by_period(hourly: &[HourlyUsage]) -> Vec<PeriodBucket> {
    let periods: [(&str, &str, Vec<usize>); 4] = [
        ("Morning", "05:00-11:59", (5..=11).collect()),
        ("Daytime", "12:00-16:59", (12..=16).collect()),
        ("Evening", "17:00-21:59", (17..=21).collect()),
        ("Night", "22:00-04:59", vec![22, 23, 0, 1, 2, 3, 4]),
    ];
    periods
        .iter()
        .map(|(label, hour_range, hours)| {
            let mut total_tokens = 0u64;
            for entry in hourly {
                let hour = entry.datetime.hour() as usize;
                if hours.contains(&hour) {
                    total_tokens = total_tokens.saturating_add(entry.tokens.total());
                }
            }
            PeriodBucket {
                label,
                hour_range,
                total_tokens,
            }
        })
        .collect()
}

pub fn aggregate_by_weekday(hourly: &[HourlyUsage]) -> Vec<WeekdayBucket> {
    let weekdays = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let mut buckets: Vec<u64> = vec![0; 7];
    for entry in hourly {
        let weekday = entry.datetime.weekday().num_days_from_monday() as usize;
        buckets[weekday] = buckets[weekday].saturating_add(entry.tokens.total());
    }
    weekdays
        .iter()
        .enumerate()
        .map(|(i, day)| WeekdayBucket {
            day,
            total_tokens: buckets[i],
        })
        .collect()
}

pub fn find_peak_hour(hourly: &[HourlyUsage]) -> Option<(u32, u64, f64)> {
    let mut hour_totals: HashMap<u32, (u64, f64)> = HashMap::new();
    for entry in hourly {
        let hour = entry.datetime.hour();
        let entry_totals = hour_totals.entry(hour).or_insert((0, 0.0));
        entry_totals.0 = entry_totals.0.saturating_add(entry.tokens.total());
        entry_totals.1 += entry.cost;
    }
    hour_totals
        .into_iter()
        .max_by(
            |(hour_a, (tokens_a, cost_a)), (hour_b, (tokens_b, cost_b))| {
                tokens_a
                    .cmp(tokens_b)
                    .then_with(|| cost_a.total_cmp(cost_b))
                    .then_with(|| hour_b.cmp(hour_a))
            },
        )
        .map(|(hour, (tokens, cost))| (hour, tokens, cost))
}

/// TUI usage accumulator owned by `AggregationEngine` when `ViewSet::TUI` is
/// requested. `push` is the per-message fold; `finish` sorts and derives the
/// graph + streaks from the finished daily buckets.
pub(super) struct TuiAcc {
    group_by: GroupBy,
    model_map: HashMap<String, UsageModelEntry>,
    agent_map: HashMap<String, AgentEntry>,
    agent_clients: HashMap<String, BTreeSet<String>>,
    agent_instances: HashMap<String, HashSet<String>>,
    daily_map: HashMap<NaiveDate, DailyUsage>,
    hourly_map: HashMap<NaiveDateTime, HourlyUsage>,
    model_session_ids: HashMap<String, HashSet<String>>,
    client_totals_by_model: HashMap<String, HashMap<String, ClientContributionOrder>>,
}

impl TuiAcc {
    pub(super) fn new(group_by: GroupBy) -> Self {
        Self {
            group_by,
            model_map: HashMap::new(),
            agent_map: HashMap::new(),
            agent_clients: HashMap::new(),
            agent_instances: HashMap::new(),
            daily_map: HashMap::new(),
            hourly_map: HashMap::new(),
            model_session_ids: HashMap::new(),
            client_totals_by_model: HashMap::new(),
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let group_by = &self.group_by;
        let normalized_model = normalize_model_for_grouping(&msg.model_id);
        let provider = normalize_provider_for_grouping(&msg.provider_id);
        let (workspace_group_key, workspace_key, workspace_label) = workspace_bucket(msg);
        let (key, merge_clients) = grouped_model_bucket_key(
            group_by,
            &msg.client,
            &provider,
            &workspace_group_key,
            &msg.session_id,
            &normalized_model,
        );

        let msg_cost = sane_cost(msg.cost);

        let model_entry = self
            .model_map
            .entry(key.clone())
            .or_insert_with(|| UsageModelEntry {
                model: normalized_model.clone(),
                provider: provider.clone(),
                client: msg.client.to_string(),
                workspace_key: if *group_by == GroupBy::WorkspaceModel {
                    workspace_key.clone()
                } else {
                    None
                },
                workspace_label: if *group_by == GroupBy::WorkspaceModel {
                    Some(workspace_label.clone())
                } else {
                    None
                },
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                performance: ModelPerformance::default(),
                session_count: 0,
            });

        if merge_clients {
            let client_totals = self.client_totals_by_model.entry(key.clone()).or_default();
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
            && !model_entry.provider.split(", ").any(|p| p == provider)
        {
            model_entry.provider = format!("{}, {}", model_entry.provider, provider);
        }

        add_unified_tokens(&mut model_entry.tokens, &msg.tokens);
        model_entry.cost += msg_cost;
        model_entry
            .performance
            .record_message(positive_unified_token_total(&msg.tokens), msg.duration_ms);

        let session_key = format!("{}:{}", msg.client, msg.session_id);
        let model_sessions = self.model_session_ids.entry(key).or_default();
        if model_sessions.insert(session_key) {
            model_entry.session_count += 1;
        }

        if let Some(agent) = msg.agent.as_ref() {
            let normalized_agent = if msg.client.as_ref() == "opencode" {
                sessions::normalize_opencode_agent_name(agent)
            } else if msg.client.as_ref() == "copilot" {
                sessions::normalize_copilot_agent_name(agent)
            } else {
                sessions::normalize_agent_name(agent)
            };
            let agent_entry = self
                .agent_map
                .entry(normalized_agent.clone())
                .or_insert_with(|| AgentEntry {
                    agent: normalized_agent.clone(),
                    clients: String::new(),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    message_count: 0,
                    instance_count: 0,
                });
            add_unified_tokens(&mut agent_entry.tokens, &msg.tokens);
            agent_entry.cost += msg_cost;
            agent_entry.message_count = agent_entry
                .message_count
                .saturating_add(msg.message_count.max(0) as u32);
            self.agent_clients
                .entry(normalized_agent.clone())
                .or_default()
                .insert(msg.client.to_string());
            let instance_key = msg
                .agent_instance
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}:{}", msg.client, msg.session_id));
            self.agent_instances
                .entry(normalized_agent)
                .or_default()
                .insert(instance_key);
        }

        if let Some(date) = msg.local_date() {
            let daily_entry = self.daily_map.entry(date).or_insert_with(|| DailyUsage {
                date,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                source_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            });
            add_unified_tokens(&mut daily_entry.tokens, &msg.tokens);
            daily_entry.cost += msg_cost;
            daily_entry.message_count += msg.message_count.max(0) as u32;
            if msg.is_turn_start {
                daily_entry.turn_count += 1;
            }

            let source_entry = daily_entry
                .source_breakdown
                .entry(msg.client.to_string())
                .or_insert_with(|| DailySourceInfo {
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    models: BTreeMap::new(),
                });
            add_unified_tokens(&mut source_entry.tokens, &msg.tokens);
            source_entry.cost += msg_cost;

            let daily_model_key = daily_source_model_key(
                group_by,
                &msg.client,
                &workspace_group_key,
                &provider,
                &msg.session_id,
                &normalized_model,
            );
            let model_info = source_entry
                .models
                .entry(daily_model_key)
                .or_insert_with(|| DailyModelInfo {
                    provider: provider.clone(),
                    display_name: daily_source_model_display_name(
                        group_by,
                        &workspace_label,
                        &msg.session_id,
                        &normalized_model,
                    ),
                    color_key: model_color_key(group_by, &provider, &normalized_model),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                });
            add_unified_tokens(&mut model_info.tokens, &msg.tokens);
            model_info.cost += msg_cost;
            model_info.messages = model_info
                .messages
                .saturating_add(msg.message_count.max(0) as u64);
        }

        if let Some(bucket) = hour_bucket_with_fallback(msg.timestamp, msg.local_date()) {
            let hourly_entry = self
                .hourly_map
                .entry(bucket)
                .or_insert_with(|| HourlyUsage {
                    datetime: bucket,
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    clients: BTreeSet::new(),
                    models: BTreeMap::new(),
                    message_count: 0,
                    turn_count: 0,
                });
            add_unified_tokens(&mut hourly_entry.tokens, &msg.tokens);
            hourly_entry.cost += msg_cost;
            hourly_entry.clients.insert(msg.client.to_string());
            hourly_entry.message_count += msg.message_count.max(0) as u32;
            if msg.is_turn_start {
                hourly_entry.turn_count += 1;
            }
            let hkey = hourly_model_key(group_by, &provider, &normalized_model);
            let hmodel = hourly_entry
                .models
                .entry(hkey)
                .or_insert_with(|| HourlyModelInfo {
                    provider: provider.clone(),
                    display_name: hourly_model_display_name(group_by, &normalized_model),
                    color_key: model_color_key(group_by, &provider, &normalized_model),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                });
            add_unified_tokens(&mut hmodel.tokens, &msg.tokens);
            hmodel.cost += msg_cost;
        }
    }

    pub(super) fn finish(self) -> UsageData {
        let Self {
            model_map,
            mut agent_map,
            agent_clients,
            agent_instances,
            daily_map,
            hourly_map,
            client_totals_by_model,
            ..
        } = self;

        let mut models: Vec<UsageModelEntry> = model_map
            .into_iter()
            .map(|(key, mut model)| {
                let provider = {
                    let mut providers: Vec<&str> = model.provider.split(", ").collect();
                    providers.sort_unstable();
                    providers.dedup();
                    providers.join(", ")
                };
                model.provider = provider;
                if let Some(client_totals) = client_totals_by_model.get(&key) {
                    model.client = ordered_clients_by_token_contribution(client_totals);
                }
                model.performance.finalize(model.tokens.total() as i64);
                model
            })
            .collect();
        models.sort_by(|a, b| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider.cmp(&b.provider))
        });

        for (agent, clients) in agent_clients {
            if let Some(agent_entry) = agent_map.get_mut(&agent) {
                agent_entry.clients = clients.into_iter().collect::<Vec<_>>().join(", ");
            }
        }
        for (agent, instances) in agent_instances {
            if let Some(agent_entry) = agent_map.get_mut(&agent) {
                agent_entry.instance_count = instances.len() as u32;
            }
        }

        let mut agents: Vec<AgentEntry> = agent_map.into_values().collect();
        agents.sort_by(|a, b| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                .then_with(|| a.agent.cmp(&b.agent))
        });

        let mut daily: Vec<DailyUsage> = daily_map.into_values().collect();
        daily.sort_by_key(|b| std::cmp::Reverse(b.date));

        let mut hourly: Vec<HourlyUsage> = hourly_map.into_values().collect();
        hourly.sort_by_key(|b| std::cmp::Reverse(b.datetime));

        let total_tokens: u64 = models.iter().map(|m| m.tokens.total()).sum();
        let total_cost: f64 = models
            .iter()
            .map(|m| if m.cost.is_finite() { m.cost } else { 0.0 })
            .sum();

        let graph = build_contribution_graph(&daily);
        let (current_streak, longest_streak) = calculate_streaks(&daily);

        UsageData {
            models,
            agents,
            daily,
            hourly,
            graph: Some(graph),
            total_tokens,
            total_cost: sane_cost(total_cost),
            loading: false,
            error: None,
            current_streak,
            longest_streak,
        }
    }
}

/// Compatibility helper for callers/tests that still pass a finished message
/// vector. The fold itself is owned by `TuiAcc` and driven through the same
/// `push`/`finish` shape as `AggregationEngine`.
pub fn aggregate_usage_data(messages: Vec<UnifiedMessage>, group_by: &GroupBy) -> UsageData {
    let mut acc = TuiAcc::new(group_by.clone());
    for msg in &messages {
        acc.push(msg);
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::NaiveDate;

    use super::*;

    fn hourly(hour: u32, input_tokens: u64, cost: f64) -> HourlyUsage {
        HourlyUsage {
            datetime: NaiveDate::from_ymd_opt(2024, 6, 10)
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
            tokens: UsageTokenBreakdown {
                input: input_tokens,
                ..UsageTokenBreakdown::default()
            },
            cost,
            clients: BTreeSet::new(),
            models: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }
    }

    #[test]
    fn find_peak_hour_breaks_token_ties_deterministically() {
        let high_cost = vec![hourly(8, 100, 2.0), hourly(12, 100, 3.0)];
        assert_eq!(find_peak_hour(&high_cost), Some((12, 100, 3.0)));

        let earliest_hour = vec![hourly(10, 100, 2.0), hourly(8, 100, 2.0)];
        assert_eq!(find_peak_hour(&earliest_hour), Some((8, 100, 2.0)));
    }
}
