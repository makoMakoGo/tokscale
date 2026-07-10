//! The TUI usage aggregation: one fold over `UnifiedMessage`s producing
//! [`crate::usage_views::UsageData`] (models/agents/daily/hourly/graph/streaks).
//! `AggregationEngine` owns this accumulator when `ViewSet::TUI` is requested;
//! core report loading drives that engine instead of the CLI carrying its own
//! fold (#37).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday};

use crate::usage_views::{
    AgentEntry, ContributionDay, DailyModelInfo, DailySourceInfo, DailyUsage, HourlyModelInfo,
    HourlyUsage, PeriodKind, PeriodUsage, UsageData, UsageGraphData, UsageModelEntry,
    UsageTokenBreakdown,
};
use crate::{
    aggregate::keys::{workspace_fields, GroupedModelKey, HourlyModelKey, IdentitySet},
    sessions, ClientContributionOrder, GroupBy, ModelPerformance, UnifiedMessage,
};

fn positive_unified_token_total(tokens: &crate::TokenBreakdown) -> i64 {
    crate::positive_token_total(tokens)
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
    workspace_label: Option<&str>,
    session_id: Option<&str>,
    model: &str,
) -> String {
    match group_by {
        GroupBy::WorkspaceModel => format!(
            "{} / {model}",
            workspace_label.expect("workspace model bucket has a display label")
        ),
        GroupBy::Session | GroupBy::ClientSession => format!(
            "{} / {model}",
            session_id.expect("session model bucket has a session identity")
        ),
        GroupBy::Model | GroupBy::ClientModel | GroupBy::ClientProviderModel => model.to_string(),
    }
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
    let source = UsageTokenBreakdown {
        input: src.input.max(0) as u64,
        output: src.output.max(0) as u64,
        cache_read: src.cache_read.max(0) as u64,
        cache_write: src.cache_write.max(0) as u64,
        reasoning: src.reasoning.max(0) as u64,
    };
    *target = target
        .checked_add(&source)
        .expect("TUI token buckets exceed u64::MAX while aggregating usage");
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
    *target = target
        .checked_add(source)
        .expect("TUI token buckets exceed u64::MAX while aggregating usage");
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
                    total_tokens = total_tokens
                        .checked_add(entry.tokens.total())
                        .expect("period token total exceeds u64::MAX");
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
        buckets[weekday] = buckets[weekday]
            .checked_add(entry.tokens.total())
            .expect("weekday token total exceeds u64::MAX");
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
        entry_totals.0 = entry_totals
            .0
            .checked_add(entry.tokens.total())
            .expect("hourly token total exceeds u64::MAX");
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
    model_map: HashMap<GroupedModelKey, TuiModelBucket>,
    agent_map: HashMap<String, AgentBucket>,
    daily_map: HashMap<NaiveDate, DailyBucket>,
    hourly_map: HashMap<NaiveDateTime, HourlyBucket>,
    next_sequence: usize,
}

struct TuiModelBucket {
    first_seen: usize,
    model: Arc<str>,
    providers: IdentitySet<Arc<str>>,
    client: Arc<str>,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    performance: ModelPerformance,
    sessions: IdentitySet<(Arc<str>, Arc<str>)>,
    // Boxed only for grouping modes that merge clients; keeps session and
    // client-scoped high-cardinality buckets free of an inline HashMap.
    #[allow(clippy::box_collection)]
    client_totals: Option<Box<HashMap<Arc<str>, ClientContributionOrder>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum AgentInstanceKey {
    Explicit(Arc<str>),
    Derived { client: Arc<str>, session: Arc<str> },
}

struct AgentBucket {
    clients: IdentitySet<Arc<str>>,
    instances: IdentitySet<AgentInstanceKey>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    message_count: u32,
}

struct DailyBucket {
    date: NaiveDate,
    tokens: UsageTokenBreakdown,
    cost: f64,
    sources: HashMap<Arc<str>, DailySourceBucket>,
    message_count: u32,
    turn_count: u32,
}

struct DailySourceBucket {
    tokens: UsageTokenBreakdown,
    cost: f64,
    models: HashMap<GroupedModelKey, DailyModelBucket>,
}

struct DailyModelBucket {
    first_seen: usize,
    provider: Arc<str>,
    workspace_label: Option<Arc<str>>,
    session_id: Option<Arc<str>>,
    model: Arc<str>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    messages: u64,
}

struct HourlyBucket {
    datetime: NaiveDateTime,
    tokens: UsageTokenBreakdown,
    cost: f64,
    clients: IdentitySet<Arc<str>>,
    models: HashMap<HourlyModelKey, HourlyModelBucket>,
    message_count: u32,
    turn_count: u32,
}

struct HourlyModelBucket {
    first_seen: usize,
    provider: Arc<str>,
    model: Arc<str>,
    tokens: UsageTokenBreakdown,
    cost: f64,
}

fn merge_performance(target: &mut ModelPerformance, source: ModelPerformance) {
    target.total_duration_ms = target
        .total_duration_ms
        .saturating_add(source.total_duration_ms);
    target.timed_tokens = target
        .timed_tokens
        .checked_add(source.timed_tokens)
        .expect("timed token count exceeds i64::MAX");
    target.sample_count = target.sample_count.saturating_add(source.sample_count);
}

fn merge_tui_model_bucket(
    target: &mut TuiModelBucket,
    source: TuiModelBucket,
    merge_providers: bool,
) {
    if merge_providers {
        target.providers.extend(source.providers);
    }
    target.sessions.extend(source.sessions);
    if let Some(source_totals_by_client) = source.client_totals {
        let target_totals_by_client = target
            .client_totals
            .as_mut()
            .expect("colliding merge-client TUI buckets both track client totals");
        for (client, source_totals) in *source_totals_by_client {
            match target_totals_by_client.entry(client) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(source_totals);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let totals = entry.get_mut();
                    totals.first_seen = totals.first_seen.min(source_totals.first_seen);
                    totals.total_tokens = totals
                        .total_tokens
                        .checked_add(source_totals.total_tokens)
                        .expect("client token contribution exceeds u64::MAX");
                }
            }
        }
    }
    add_tokens(&mut target.tokens, &source.tokens);
    target.cost += source.cost;
    merge_performance(&mut target.performance, source.performance);
}

fn materialize_tui_model(mut bucket: TuiModelBucket) -> UsageModelEntry {
    let provider = bucket.providers.into_sorted_string();
    let client = if let Some(client_totals) = bucket.client_totals {
        let mut clients: Vec<_> = (*client_totals).into_iter().collect();
        clients.sort_by(|(left_client, left), (right_client, right)| {
            right
                .total_tokens
                .cmp(&left.total_tokens)
                .then_with(|| left.first_seen.cmp(&right.first_seen))
                .then_with(|| left_client.cmp(right_client))
        });
        clients
            .iter()
            .map(|(client, _)| client.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        bucket.client.to_string()
    };
    bucket.performance.finalize(bucket.tokens.total() as i64);
    UsageModelEntry {
        model: bucket.model.to_string(),
        provider,
        client,
        workspace_key: bucket.workspace_key.map(|key| key.to_string()),
        workspace_label: bucket.workspace_label.map(|label| label.to_string()),
        tokens: bucket.tokens,
        cost: bucket.cost,
        performance: bucket.performance,
        session_count: bucket
            .sessions
            .len()
            .try_into()
            .expect("model session count exceeds u32::MAX"),
    }
}

fn merge_daily_model_bucket(target: &mut DailyModelBucket, source: DailyModelBucket) {
    add_tokens(&mut target.tokens, &source.tokens);
    target.cost += source.cost;
    target.messages = target
        .messages
        .checked_add(source.messages)
        .expect("daily model message count exceeds u64::MAX");
}

fn materialize_daily_model(model: DailyModelBucket, group_by: &GroupBy) -> DailyModelInfo {
    let provider = model.provider.to_string();
    let display_name = daily_source_model_display_name(
        group_by,
        model.workspace_label.as_deref(),
        model.session_id.as_deref(),
        &model.model,
    );
    let color_key = model_color_key(group_by, &provider, &model.model);
    DailyModelInfo {
        provider,
        display_name,
        color_key,
        tokens: model.tokens,
        cost: model.cost,
        messages: model.messages,
    }
}

fn materialize_daily(bucket: DailyBucket, group_by: &GroupBy) -> DailyUsage {
    let mut source_breakdown = BTreeMap::new();
    for (client, source) in bucket.sources {
        let mut models = BTreeMap::new();
        let mut risky_models = Vec::new();
        for (key, model) in source.models {
            if key.may_alias_legacy_key() {
                risky_models.push((key, model));
            } else {
                models.insert(key.public_key(), materialize_daily_model(model, group_by));
            }
        }
        if !risky_models.is_empty() {
            risky_models.sort_by_key(|(_, model)| model.first_seen);
            let mut public_models: HashMap<String, DailyModelBucket> = HashMap::new();
            for (key, model) in risky_models {
                match public_models.entry(key.public_key()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(model);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        merge_daily_model_bucket(entry.get_mut(), model);
                    }
                }
            }
            models.extend(
                public_models
                    .into_iter()
                    .map(|(key, model)| (key, materialize_daily_model(model, group_by))),
            );
        }
        source_breakdown.insert(
            client.to_string(),
            DailySourceInfo {
                tokens: source.tokens,
                cost: source.cost,
                models,
            },
        );
    }
    DailyUsage {
        date: bucket.date,
        tokens: bucket.tokens,
        cost: bucket.cost,
        source_breakdown,
        message_count: bucket.message_count,
        turn_count: bucket.turn_count,
    }
}

fn merge_hourly_model_bucket(target: &mut HourlyModelBucket, source: HourlyModelBucket) {
    add_tokens(&mut target.tokens, &source.tokens);
    target.cost += source.cost;
}

fn materialize_hourly_model(model: HourlyModelBucket, group_by: &GroupBy) -> HourlyModelInfo {
    let provider = model.provider.to_string();
    HourlyModelInfo {
        provider: provider.clone(),
        display_name: hourly_model_display_name(group_by, &model.model),
        color_key: model_color_key(group_by, &provider, &model.model),
        tokens: model.tokens,
        cost: model.cost,
    }
}

fn materialize_hourly(bucket: HourlyBucket, group_by: &GroupBy) -> HourlyUsage {
    let mut models = BTreeMap::new();
    let mut risky_models = Vec::new();
    for (key, model) in bucket.models {
        if key.may_alias_legacy_key() {
            risky_models.push((key, model));
        } else {
            models.insert(key.public_key(), materialize_hourly_model(model, group_by));
        }
    }
    if !risky_models.is_empty() {
        risky_models.sort_by_key(|(_, model)| model.first_seen);
        let mut public_models: HashMap<String, HourlyModelBucket> = HashMap::new();
        for (key, model) in risky_models {
            match public_models.entry(key.public_key()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(model);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    merge_hourly_model_bucket(entry.get_mut(), model);
                }
            }
        }
        models.extend(
            public_models
                .into_iter()
                .map(|(key, model)| (key, materialize_hourly_model(model, group_by))),
        );
    }
    let clients = bucket
        .clients
        .into_vec()
        .into_iter()
        .map(|client| client.to_string())
        .collect();
    HourlyUsage {
        datetime: bucket.datetime,
        tokens: bucket.tokens,
        cost: bucket.cost,
        clients,
        models,
        message_count: bucket.message_count,
        turn_count: bucket.turn_count,
    }
}

impl TuiAcc {
    pub(super) fn new(group_by: GroupBy) -> Self {
        Self {
            group_by,
            model_map: HashMap::new(),
            agent_map: HashMap::new(),
            daily_map: HashMap::new(),
            hourly_map: HashMap::new(),
            next_sequence: 0,
        }
    }

    pub(super) fn push(&mut self, msg: &UnifiedMessage) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("TUI aggregation sequence exceeds usize::MAX");
        let group_by = &self.group_by;
        let key = GroupedModelKey::from_message(group_by, msg);
        let merge_clients = key.merges_clients();

        let msg_cost = sane_cost(msg.cost);

        let model_entry = self.model_map.entry(key).or_insert_with(|| {
            let (workspace_key, workspace_label) = if *group_by == GroupBy::WorkspaceModel {
                let (key, label) = workspace_fields(msg);
                (key, Some(label))
            } else {
                (None, None)
            };
            TuiModelBucket {
                first_seen: sequence,
                model: Arc::clone(&msg.model_id),
                providers: IdentitySet::one(Arc::clone(&msg.provider_id)),
                client: Arc::clone(&msg.client),
                workspace_key,
                workspace_label,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                performance: ModelPerformance::default(),
                sessions: IdentitySet::default(),
                client_totals: merge_clients.then(|| Box::new(HashMap::new())),
            }
        });

        if merge_clients {
            let totals = model_entry
                .client_totals
                .as_mut()
                .expect("merge-client TUI grouping has client totals")
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

        model_entry.providers.insert(Arc::clone(&msg.provider_id));

        add_unified_tokens(&mut model_entry.tokens, &msg.tokens);
        model_entry.cost += msg_cost;
        model_entry
            .performance
            .record_message(positive_unified_token_total(&msg.tokens), msg.duration_ms);

        model_entry
            .sessions
            .insert((Arc::clone(&msg.client), Arc::clone(&msg.session_id)));

        if let Some(agent) = msg.agent.as_ref() {
            let normalized_agent = if msg.client.as_ref() == "opencode" {
                sessions::normalize_opencode_agent_name(agent)
            } else if msg.client.as_ref() == "copilot" {
                sessions::normalize_copilot_agent_name(agent)
            } else {
                sessions::normalize_agent_name(agent)
            };
            let agent_entry =
                self.agent_map
                    .entry(normalized_agent)
                    .or_insert_with(|| AgentBucket {
                        clients: IdentitySet::default(),
                        instances: IdentitySet::default(),
                        tokens: UsageTokenBreakdown::default(),
                        cost: 0.0,
                        message_count: 0,
                    });
            add_unified_tokens(&mut agent_entry.tokens, &msg.tokens);
            agent_entry.cost += msg_cost;
            agent_entry.message_count = agent_entry
                .message_count
                .saturating_add(msg.message_count.max(0) as u32);
            agent_entry.clients.insert(Arc::clone(&msg.client));
            let instance_key = msg.agent_instance.as_ref().map_or_else(
                || AgentInstanceKey::Derived {
                    client: Arc::clone(&msg.client),
                    session: Arc::clone(&msg.session_id),
                },
                |instance| AgentInstanceKey::Explicit(Arc::clone(instance)),
            );
            agent_entry.instances.insert(instance_key);
        }

        if let Some(date) = msg.local_date() {
            let daily_entry = self.daily_map.entry(date).or_insert_with(|| DailyBucket {
                date,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                sources: HashMap::new(),
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
                .sources
                .entry(Arc::clone(&msg.client))
                .or_insert_with(|| DailySourceBucket {
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    models: HashMap::new(),
                });
            add_unified_tokens(&mut source_entry.tokens, &msg.tokens);
            source_entry.cost += msg_cost;

            let daily_model_key = GroupedModelKey::from_message(group_by, msg);
            let model_info = source_entry
                .models
                .entry(daily_model_key)
                .or_insert_with(|| DailyModelBucket {
                    first_seen: sequence,
                    provider: Arc::clone(&msg.provider_id),
                    workspace_label: (*group_by == GroupBy::WorkspaceModel)
                        .then(|| workspace_fields(msg).1),
                    session_id: matches!(group_by, GroupBy::Session | GroupBy::ClientSession)
                        .then(|| Arc::clone(&msg.session_id)),
                    model: Arc::clone(&msg.model_id),
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

        if let Some(bucket) = timestamp_to_hour(msg.timestamp) {
            let hourly_entry = self
                .hourly_map
                .entry(bucket)
                .or_insert_with(|| HourlyBucket {
                    datetime: bucket,
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    clients: IdentitySet::default(),
                    models: HashMap::new(),
                    message_count: 0,
                    turn_count: 0,
                });
            add_unified_tokens(&mut hourly_entry.tokens, &msg.tokens);
            hourly_entry.cost += msg_cost;
            hourly_entry.clients.insert(Arc::clone(&msg.client));
            hourly_entry.message_count += msg.message_count.max(0) as u32;
            if msg.is_turn_start {
                hourly_entry.turn_count += 1;
            }
            let hkey = HourlyModelKey::from_message(group_by, msg);
            let hmodel = hourly_entry
                .models
                .entry(hkey)
                .or_insert_with(|| HourlyModelBucket {
                    first_seen: sequence,
                    provider: Arc::clone(&msg.provider_id),
                    model: Arc::clone(&msg.model_id),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                });
            add_unified_tokens(&mut hmodel.tokens, &msg.tokens);
            hmodel.cost += msg_cost;
        }
    }

    pub(super) fn finish(self) -> UsageData {
        let Self {
            group_by,
            model_map,
            agent_map,
            daily_map,
            hourly_map,
            ..
        } = self;

        let mut keyed_models = Vec::with_capacity(model_map.len());
        let mut risky_models = Vec::new();
        for (key, bucket) in model_map {
            if key.may_alias_legacy_key() {
                risky_models.push((key, bucket));
            } else {
                keyed_models.push((key.public_key(), materialize_tui_model(bucket)));
            }
        }
        if !risky_models.is_empty() {
            risky_models.sort_by_key(|(_, bucket)| bucket.first_seen);
            let mut public_models: HashMap<String, TuiModelBucket> = HashMap::new();
            for (key, bucket) in risky_models {
                match public_models.entry(key.public_key()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(bucket);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        merge_tui_model_bucket(
                            entry.get_mut(),
                            bucket,
                            group_by != GroupBy::ClientProviderModel,
                        );
                    }
                }
            }
            keyed_models.extend(
                public_models
                    .into_iter()
                    .map(|(key, bucket)| (key, materialize_tui_model(bucket))),
            );
        }
        keyed_models.sort_by(|(a_key, a), (b_key, b)| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.client.cmp(&b.client))
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.workspace_key.cmp(&b.workspace_key))
                .then_with(|| a_key.cmp(b_key))
        });
        let models: Vec<UsageModelEntry> =
            keyed_models.into_iter().map(|(_, model)| model).collect();

        let mut agents: Vec<AgentEntry> = agent_map
            .into_iter()
            .map(|(agent_name, agent)| AgentEntry {
                agent: agent_name,
                clients: agent.clients.into_sorted_string(),
                tokens: agent.tokens,
                cost: agent.cost,
                message_count: agent.message_count,
                instance_count: agent
                    .instances
                    .len()
                    .try_into()
                    .expect("agent instance count exceeds u32::MAX"),
            })
            .collect();
        agents.sort_by(|a, b| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                .then_with(|| a.agent.cmp(&b.agent))
        });

        let mut daily: Vec<DailyUsage> = daily_map
            .into_values()
            .map(|bucket| materialize_daily(bucket, &group_by))
            .collect();
        daily.sort_by_key(|b| std::cmp::Reverse(b.date));

        let mut hourly: Vec<HourlyUsage> = hourly_map
            .into_values()
            .map(|bucket| materialize_hourly(bucket, &group_by))
            .collect();
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::NaiveDate;

    use super::*;
    use crate::aggregate::keys::UNKNOWN_WORKSPACE_LABEL;
    use crate::sessions::UnifiedMessage;

    struct TuiUsageHarness;

    impl TuiUsageHarness {
        fn aggregate_messages(
            &self,
            messages: Vec<UnifiedMessage>,
            group_by: &GroupBy,
        ) -> Result<UsageData, String> {
            let mut acc = TuiAcc::new(group_by.clone());
            for message in &messages {
                acc.push(message);
            }
            Ok(acc.finish())
        }
    }

    fn make_workspace_message(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
    ) -> UnifiedMessage {
        let mut msg = UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
        );
        msg.set_workspace(
            workspace_key.map(str::to_string),
            workspace_label.map(str::to_string),
        );
        msg
    }

    #[allow(clippy::too_many_arguments)]
    fn make_message_with_tokens(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
    ) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_735_689_600_000,
            crate::TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            0.0,
        )
    }

    #[test]
    fn test_aggregate_messages_model_grouping_uses_finalized_provider_ids() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].model, "mimo-v2.5-pro");
        assert_eq!(usage.models[0].provider, "xiaomi");
        assert_eq!(usage.models[0].cost, 3.0);
    }

    #[test]
    fn test_aggregate_messages_client_provider_model_uses_finalized_provider_ids() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].provider, "xiaomi");
        assert_eq!(usage.models[0].cost, 3.0);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models.get("opencode:xiaomi:mimo-v2.5-pro").unwrap();
        assert_eq!(daily_model.provider, "xiaomi");
        assert_eq!(daily_model.display_name, "mimo-v2.5-pro");
    }

    #[test]
    fn test_client_provider_model_daily_detail_label_matches_models_tab() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![make_workspace_message(
                    "opencode",
                    "gpt-5.5",
                    "openai",
                    "session-1",
                    1.0,
                    None,
                    None,
                )],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].model, "gpt-5.5");
        assert_eq!(usage.models[0].provider, "openai");

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models.get("opencode:openai:gpt-5.5").unwrap();
        assert_eq!(daily_model.provider, "openai");
        assert_eq!(daily_model.display_name, "gpt-5.5");
    }

    #[test]
    fn test_client_provider_model_keeps_same_model_distinct_by_provider() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "microsoft",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("opencode:openai:gpt-5.5"));
        assert!(daily_models.contains_key("opencode:microsoft:gpt-5.5"));
        assert!(daily_models
            .values()
            .all(|model| model.display_name == "gpt-5.5"));
    }

    #[test]
    fn test_session_grouping_splits_daily_models_by_session() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::Session,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("session-1:gpt-5.5"));
        assert!(daily_models.contains_key("session-2:gpt-5.5"));
        assert_eq!(
            daily_models["session-1:gpt-5.5"].display_name,
            "session-1 / gpt-5.5"
        );
        assert_eq!(
            daily_models["session-2:gpt-5.5"].display_name,
            "session-2 / gpt-5.5"
        );
    }

    #[test]
    fn test_aggregate_messages_uses_finalized_kimi_provider() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "kimi-for-coding",
                        "kimi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "claude",
                        "kimi-for-coding",
                        "kimi",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].provider, "kimi");
        assert_eq!(usage.models[0].cost, 3.0);
    }

    #[test]
    fn test_aggregate_messages_builds_agent_usage() {
        let loader = TuiUsageHarness;
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-sonnet-4",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.25,
                Some("builder".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "roocode",
                "claude-sonnet-4",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                crate::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.75,
                Some("builder".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Builder");
        assert_eq!(usage.agents[0].clients, "opencode, roocode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
        assert_eq!(usage.agents[0].tokens.total(), 45);
    }

    #[test]
    fn test_aggregate_messages_orders_model_clients_by_total_tokens() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_message_with_tokens(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-opencode",
                        10,
                        0,
                        0,
                        0,
                        0,
                    ),
                    make_message_with_tokens(
                        "codex",
                        "gpt-5.5",
                        "openai",
                        "session-codex",
                        30,
                        0,
                        0,
                        0,
                        0,
                    ),
                    make_message_with_tokens(
                        "pi",
                        "gpt-5.5",
                        "openai",
                        "session-pi",
                        100,
                        0,
                        0,
                        0,
                        0,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].client, "pi, codex, opencode");
    }

    #[test]
    fn test_aggregate_messages_groups_by_workspace_and_model() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.25,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        "qwen",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.75,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].workspace_key.as_deref(), Some("/repo-a"));
        assert_eq!(usage.models[0].workspace_label.as_deref(), Some("repo-a"));
        assert_eq!(usage.models[0].model, "claude-sonnet-4.5");
        assert_eq!(usage.models[0].client, "claude, qwen");
        assert_eq!(usage.models[0].session_count, 2);
        assert_eq!(usage.models[0].cost, 4.0);
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_keeps_unknown_bucket_visible() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].workspace_key, None);
        assert_eq!(
            usage.models[0].workspace_label.as_deref(),
            Some(UNKNOWN_WORKSPACE_LABEL)
        );
        assert_eq!(usage.models[0].session_count, 2);
        assert_eq!(usage.models[0].cost, 3.0);
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_keeps_real_unknown_workspace_separate() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("unknown-workspace"),
                        Some("unknown-workspace"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("unknown-workspace")
                && model.workspace_label.as_deref() == Some("unknown-workspace")
                && (model.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.is_none()
                && model.workspace_label.as_deref() == Some(UNKNOWN_WORKSPACE_LABEL)
                && (model.cost - 2.0).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_splits_daily_models_by_workspace() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("/repo-b"),
                        Some("repo-b"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);
        let daily_display_names: Vec<_> = claude
            .models
            .values()
            .map(|info| info.display_name.clone())
            .collect();
        assert_eq!(
            daily_display_names,
            vec![
                "repo-a / claude-sonnet-4.5".to_string(),
                "repo-b / claude-sonnet-4.5".to_string()
            ]
        );
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_disambiguates_identical_labels() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/srv/team-a/demo"),
                        Some("demo"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("/srv/team-b/demo"),
                        Some("demo"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);

        // Keys must differ even though display names are identical
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);

        let display_names: Vec<_> = claude
            .models
            .values()
            .map(|info| info.display_name.clone())
            .collect();
        assert_eq!(
            display_names,
            vec![
                "demo / claude-sonnet-4.5".to_string(),
                "demo / claude-sonnet-4.5".to_string()
            ]
        );
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_avoids_separator_key_collisions() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "c",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("a:b"),
                        Some("workspace-ab"),
                    ),
                    make_workspace_message(
                        "claude",
                        "b:c",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("a"),
                        Some("workspace-a"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("a:b")
                && model.model == "c"
                && (model.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("a")
                && model.model == "b:c"
                && (model.cost - 2.0).abs() < f64::EPSILON
        }));

        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);
    }

    #[test]
    fn test_aggregate_messages_client_provider_model_splits_providers_in_daily_breakdown() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1_735_689_600_000,
                        crate::TokenBreakdown {
                            input: 10,
                            output: 5,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        1.0,
                    ),
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "microsoft",
                        "session-2",
                        1_735_689_600_000,
                        crate::TokenBreakdown {
                            input: 20,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        2.0,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);

        let anthropic_key = "claude:anthropic:claude-sonnet-4.5";
        let copilot_key = "claude:microsoft:claude-sonnet-4.5";
        let anthropic_model = claude.models.get(anthropic_key).unwrap();
        assert_eq!(anthropic_model.display_name, "claude-sonnet-4.5");
        assert_eq!(anthropic_model.provider, "anthropic");
        assert_eq!(anthropic_model.tokens.total(), 15);
        assert_eq!(anthropic_model.messages, 1);

        let copilot_model = claude.models.get(copilot_key).unwrap();
        assert_eq!(copilot_model.display_name, "claude-sonnet-4.5");
        assert_eq!(copilot_model.provider, "microsoft");
        assert_eq!(copilot_model.tokens.total(), 30);
        assert_eq!(copilot_model.messages, 1);
    }

    #[test]
    fn test_aggregate_messages_keeps_same_model_split_across_sources_in_daily_breakdown() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1_735_689_600_000,
                        crate::TokenBreakdown {
                            input: 10,
                            output: 5,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        1.0,
                    ),
                    UnifiedMessage::new(
                        "cursor",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        1_735_689_600_000,
                        crate::TokenBreakdown {
                            input: 20,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        2.0,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        assert_eq!(usage.daily[0].source_breakdown.len(), 2);

        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.cost, 1.0);
        assert_eq!(claude.models.len(), 1);
        let claude_model = claude.models.get("claude-sonnet-4.5").unwrap();
        assert_eq!(claude_model.display_name, "claude-sonnet-4.5");
        assert_eq!(claude_model.tokens.total(), 15);

        let cursor = usage.daily[0].source_breakdown.get("cursor").unwrap();
        assert_eq!(cursor.cost, 2.0);
        assert_eq!(cursor.models.len(), 1);
        let cursor_model = cursor.models.get("claude-sonnet-4.5").unwrap();
        assert_eq!(cursor_model.display_name, "claude-sonnet-4.5");
        assert_eq!(cursor_model.tokens.total(), 30);
    }

    #[test]
    fn test_aggregate_messages_merges_oh_my_opencode_agent_variants() {
        let loader = TuiUsageHarness;
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 100,
                    cache_write: 20,
                    reasoning: 0,
                },
                1.5,
                Some("Sisyphus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                crate::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 200,
                    cache_write: 40,
                    reasoning: 0,
                },
                2.5,
                Some("Sisyphus (Ultraworker)".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Sisyphus");
        assert_eq!(usage.agents[0].clients, "opencode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
        assert_eq!(usage.agents[0].tokens.total(), 405);
    }

    #[test]
    fn test_aggregate_messages_merges_opencode_agent_case_variants() {
        let loader = TuiUsageHarness;
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.5,
                Some("Hephaestus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                crate::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.5,
                Some("hephaestus".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Hephaestus");
        assert_eq!(usage.agents[0].clients, "opencode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_aggregate_messages_does_not_merge_omo_variants_for_non_opencode_clients() {
        let loader = TuiUsageHarness;
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "claude",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.5,
                Some("Sisyphus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "claude",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                crate::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.5,
                Some("Sisyphus (Ultraworker)".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 2);
        assert!(usage.agents.iter().any(|agent| agent.agent == "Sisyphus"));
        assert!(usage
            .agents
            .iter()
            .any(|agent| agent.agent == "Sisyphus (Ultraworker)"));
    }

    fn collision_message(
        client: &str,
        provider: &str,
        session: &str,
        model: &str,
        input: i64,
        timestamp: i64,
    ) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            model,
            provider,
            session,
            timestamp,
            crate::TokenBreakdown {
                input,
                ..crate::TokenBreakdown::default()
            },
            input as f64,
        )
    }

    #[test]
    fn legacy_public_key_collisions_are_explicitly_coalesced_without_dropping_totals() {
        let timestamp = 1_735_689_600_000;
        let cases = [
            (
                GroupBy::ClientModel,
                collision_message("a:b", "first", "same", "c", 10, timestamp),
                collision_message("a", "second", "same", "b:c", 20, timestamp),
                "a:b",
                "c",
                "first, second",
                2,
            ),
            (
                GroupBy::ClientProviderModel,
                collision_message("a", "b:c", "same", "d", 10, timestamp),
                collision_message("a", "b", "same", "c:d", 20, timestamp),
                "a",
                "d",
                "b:c",
                1,
            ),
            (
                GroupBy::Session,
                collision_message("a", "first", "b:c", "d", 10, timestamp),
                collision_message("a", "second", "b", "c:d", 20, timestamp),
                "a",
                "d",
                "first, second",
                2,
            ),
            (
                GroupBy::ClientSession,
                collision_message("a", "first", "b:c", "d", 10, timestamp),
                collision_message("a", "second", "b", "c:d", 20, timestamp),
                "a",
                "d",
                "first, second",
                2,
            ),
        ];

        for (group_by, first, second, client, model, provider, sessions) in cases {
            let mut acc = TuiAcc::new(group_by);
            acc.push(&first);
            acc.push(&second);
            let usage = acc.finish();
            assert_eq!(usage.models.len(), 1);
            assert_eq!(usage.models[0].client, client);
            assert_eq!(usage.models[0].model, model);
            assert_eq!(usage.models[0].provider, provider);
            assert_eq!(usage.models[0].tokens.total(), 30);
            assert_eq!(usage.models[0].cost, 30.0);
            assert_eq!(usage.models[0].session_count, sessions);
        }
    }

    #[test]
    fn daily_and_hourly_collision_materialization_preserves_first_fields_and_all_totals() {
        let timestamp = 1_735_689_600_000;
        let first = collision_message("a", "b:c", "same", "d", 10, timestamp);
        let second = collision_message("a", "b", "same", "c:d", 20, timestamp);
        let mut acc = TuiAcc::new(GroupBy::ClientProviderModel);
        acc.push(&first);
        acc.push(&second);
        let usage = acc.finish();

        let daily = &usage.daily[0].source_breakdown["a"].models;
        assert_eq!(daily.len(), 1);
        let daily_model = &daily["a:b:c:d"];
        assert_eq!(daily_model.provider, "b:c");
        assert_eq!(daily_model.display_name, "d");
        assert_eq!(daily_model.tokens.total(), 30);
        assert_eq!(daily_model.cost, 30.0);
        assert_eq!(daily_model.messages, 2);

        let hourly = &usage.hourly[0].models;
        assert_eq!(hourly.len(), 1);
        let hourly_model = &hourly["b:c:d"];
        assert_eq!(hourly_model.provider, "b:c");
        assert_eq!(hourly_model.display_name, "d");
        assert_eq!(hourly_model.tokens.total(), 30);
        assert_eq!(hourly_model.cost, 30.0);
    }

    #[test]
    fn structured_session_and_agent_instance_identities_do_not_alias_delimiters() {
        let timestamp = 1_735_689_600_000;
        let model_messages = [
            collision_message("a:b", "provider", "c", "model", 10, timestamp),
            collision_message("a", "provider", "b:c", "model", 20, timestamp),
        ];
        let mut model_acc = TuiAcc::new(GroupBy::Model);
        for message in &model_messages {
            model_acc.push(message);
        }
        assert_eq!(model_acc.finish().models[0].session_count, 2);

        let mut explicit = UnifiedMessage::new_with_agent(
            "a",
            "model",
            "provider",
            "session",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        explicit.set_agent_instance(Some("a:b:c".to_string()));
        let derived_left = UnifiedMessage::new_with_agent(
            "a:b",
            "model",
            "provider",
            "c",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        let derived_right = UnifiedMessage::new_with_agent(
            "a",
            "model",
            "provider",
            "b:c",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        let mut agent_acc = TuiAcc::new(GroupBy::Model);
        for message in [&explicit, &derived_left, &derived_right] {
            agent_acc.push(message);
        }
        assert_eq!(agent_acc.finish().agents[0].instance_count, 3);
    }

    #[test]
    fn session_grouping_uses_public_identity_as_the_final_sort_tie_break() {
        let timestamp = 1_735_689_600_000;
        let mut session_b =
            collision_message("client", "provider", "session-b", "model", 20, timestamp);
        session_b.cost = 1.0;
        let mut session_a =
            collision_message("client", "provider", "session-a", "model", 10, timestamp);
        session_a.cost = 1.0;

        let mut acc = TuiAcc::new(GroupBy::Session);
        acc.push(&session_b);
        acc.push(&session_a);
        let usage = acc.finish();

        assert_eq!(usage.models.len(), 2);
        assert_eq!(usage.models[0].tokens.total(), 10);
        assert_eq!(usage.models[1].tokens.total(), 20);
    }

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
