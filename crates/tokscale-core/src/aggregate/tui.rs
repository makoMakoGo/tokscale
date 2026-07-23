//! The TUI usage aggregation: one fold over `UnifiedMessage`s producing
//! [`crate::usage_views::UsageData`] (models/agents/daily/hourly/graph/streaks).
//! `AggregationEngine` owns this accumulator when `ViewSet::TUI` is requested;
//! core report loading drives that engine instead of the CLI carrying its own
//! fold (#37). The fold is group-independent: messages land in canonical
//! finest-granularity buckets, and [`TuiAcc::project`] re-folds them into any
//! grouping's `UsageData` in memory (issue #161).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday};
use serde::{Deserialize, Serialize};

use crate::usage_views::{
    AgentEntry, ContributionDay, DailyClientCommon, DailyClientInfo, DailyModelInfo,
    DailyModelProjection, DailyUsage, DailyUsageCommon, HourlyModelInfo, HourlyModelProjection,
    HourlyUsage, HourlyUsageCommon, PeriodKind, PeriodUsage, UsageCommonData, UsageData,
    UsageGraphData, UsageGroupedData, UsageModelEntry, UsageTokenBreakdown,
};
use crate::{
    aggregate::keys::{
        workspace_fields, FineHourlyModelKey, FineModelKey, GroupedModelKey, HourlyModelKey,
        IdentitySet,
    },
    ClientContributionOrder, ClientId, GroupBy, ModelPerformance, UnifiedMessage,
};

fn positive_unified_token_total(tokens: &crate::TokenBreakdown) -> i64 {
    crate::positive_token_total(tokens)
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
    let addition = UsageTokenBreakdown {
        input: src.input.max(0) as u64,
        output: src.output.max(0) as u64,
        cache_read: src.cache_read.max(0) as u64,
        cache_write: src.cache_write.max(0) as u64,
        reasoning: src.reasoning.max(0) as u64,
    };
    *target = target
        .checked_add(&addition)
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

fn add_tokens(target: &mut UsageTokenBreakdown, addition: &UsageTokenBreakdown) {
    *target = target
        .checked_add(addition)
        .expect("TUI token buckets exceed u64::MAX while aggregating usage");
}

fn merge_daily_clients(
    target: &mut BTreeMap<String, DailyClientInfo>,
    clients: &BTreeMap<String, DailyClientInfo>,
) {
    for (client_key, client_info) in clients {
        let target_client = target
            .entry(client_key.clone())
            .or_insert_with(|| DailyClientInfo {
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                models: BTreeMap::new(),
            });
        add_tokens(&mut target_client.tokens, &client_info.tokens);
        target_client.cost += client_info.cost;
        for (model_key, model_info) in &client_info.models {
            let target_model = target_client
                .models
                .entry(model_key.clone())
                .or_insert_with(|| DailyModelInfo {
                    provider: model_info.provider.clone(),
                    model_id: model_info.model_id.clone(),
                    display_name: model_info.display_name.clone(),
                    workspace_key: model_info.workspace_key.clone(),
                    workspace_label: model_info.workspace_label.clone(),
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
                client_breakdown: BTreeMap::new(),
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
        merge_daily_clients(&mut entry.client_breakdown, &day.client_breakdown);
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
    build_contribution_graph_for_today_by(
        daily,
        today,
        |usage| usage.date,
        |usage| usage.tokens.total(),
        |usage| usage.cost,
    )
}

fn build_common_contribution_graph(daily: &[DailyUsageCommon]) -> UsageGraphData {
    build_contribution_graph_for_today_by(
        daily,
        Local::now().date_naive(),
        |usage| usage.date,
        |usage| usage.tokens.total(),
        |usage| usage.cost,
    )
}

fn build_contribution_graph_for_today_by<T>(
    daily: &[T],
    today: NaiveDate,
    date_of: impl Fn(&T) -> NaiveDate + Copy,
    tokens_of: impl Fn(&T) -> u64 + Copy,
    cost_of: impl Fn(&T) -> f64 + Copy,
) -> UsageGraphData {
    if daily.is_empty() {
        return UsageGraphData { weeks: vec![] };
    }
    let days_to_sunday = today.weekday().num_days_from_sunday();
    let end_date = today;
    let start_date = end_date - chrono::Duration::days(364 + days_to_sunday as i64);
    let daily_map: HashMap<NaiveDate, &T> =
        daily.iter().map(|usage| (date_of(usage), usage)).collect();
    let max_cost = daily.iter().map(cost_of).fold(0.0_f64, |a, b| a.max(b));
    let mut weeks: Vec<Vec<Option<ContributionDay>>> = Vec::new();
    let mut current_week: Vec<Option<ContributionDay>> = Vec::new();
    let mut current_date = start_date;
    while current_date <= end_date {
        let day = if let Some(usage) = daily_map.get(&current_date) {
            let raw_intensity = if max_cost > 0.0 {
                cost_of(usage) / max_cost
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
                tokens: tokens_of(usage),
                cost: cost_of(usage),
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
    calculate_streaks_for_today_by(daily, today, |usage| usage.date)
}

fn calculate_common_streaks(daily: &[DailyUsageCommon]) -> (u32, u32) {
    calculate_streaks_for_today_by(daily, Local::now().date_naive(), |usage| usage.date)
}

fn calculate_streaks_for_today_by<T>(
    daily: &[T],
    today: NaiveDate,
    date_of: impl Fn(&T) -> NaiveDate,
) -> (u32, u32) {
    if daily.is_empty() {
        return (0, 0);
    }
    let dates: HashSet<NaiveDate> = daily.iter().map(date_of).collect();
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
/// requested. `push` folds each message into canonical finest-granularity
/// buckets (group-independent); `project` re-folds them into one grouping's
/// [`UsageData`] and may be called repeatedly with different groupings —
/// switching the TUI group-by no longer rescans, reparses, or reprices local
/// clients (issue #161).
#[derive(Default, Serialize, Deserialize)]
pub struct TuiAcc {
    #[serde(with = "map_as_vec")]
    model_map: HashMap<FineModelKey, FineModelBucket>,
    #[serde(with = "map_as_vec")]
    agent_map: HashMap<AgentKey, AgentBucket>,
    #[serde(with = "map_as_vec")]
    daily_map: HashMap<NaiveDate, DailyBucket>,
    #[serde(with = "map_as_vec")]
    hourly_map: HashMap<NaiveDateTime, HourlyBucket>,
    next_sequence: usize,
}

mod map_as_vec {
    use std::{collections::HashMap, hash::Hash};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<K, V, S>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
    where
        K: Serialize,
        V: Serialize,
        S: Serializer,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub(super) fn deserialize<'de, K, V, D>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let entries = Vec::<(K, V)>::deserialize(deserializer)?;
        let mut map = HashMap::with_capacity(entries.len());
        for (key, value) in entries {
            if map.insert(key, value).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate key in canonical TUI map",
                ));
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod map_as_vec_tests {
    use std::collections::HashMap;

    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Wrapper {
        #[serde(with = "super::map_as_vec")]
        values: HashMap<String, u64>,
    }

    #[test]
    fn duplicate_canonical_map_keys_are_rejected() {
        let error =
            serde_json::from_str::<Wrapper>(r#"{"values":[["duplicate",1],["duplicate",2]]}"#)
                .unwrap_err();

        assert!(error
            .to_string()
            .contains("duplicate key in canonical TUI map"));
    }

    #[test]
    fn unique_canonical_map_keys_round_trip() {
        let parsed =
            serde_json::from_str::<Wrapper>(r#"{"values":[["first",1],["second",2]]}"#).unwrap();

        assert_eq!(parsed.values.len(), 2);
        assert_eq!(parsed.values["first"], 1);
        assert_eq!(parsed.values["second"], 2);
    }
}

/// Stores singleton groups inline and allocates only when a second value joins.
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn push(&mut self, value: T) {
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

    fn into_stable_iter_by_key<K: Ord>(
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

#[cfg(test)]
mod one_or_many_tests {
    use super::OneOrMany;

    #[test]
    fn iterates_singleton() {
        let values = OneOrMany::One((2, "second"));

        assert_eq!(
            values
                .into_stable_iter_by_key(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![(2, "second")]
        );
    }

    #[test]
    fn promotes_to_many_and_sorts_stably() {
        let mut values = OneOrMany::One((2, "second"));
        values.push((1, "first"));
        values.push((2, "third"));

        assert_eq!(
            values
                .into_stable_iter_by_key(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![(1, "first"), (2, "second"), (2, "third")]
        );
    }
}

/// Canonical `(client, provider, workspace, session, model)` bucket. Keeps
/// the additive counters plus the two creation-time attributes every
/// grouping re-derives materialized fields from: `first_seen` (arrival order
/// of the bucket's first message, for provider/label attribution and client
/// ordering tie-breaks) and `workspace_label` (the workspace DTO label of
/// that first message).
#[derive(Serialize, Deserialize)]
struct FineModelBucket {
    workspace_label: Arc<str>,
    first_seen: usize,
    tokens: UsageTokenBreakdown,
    cost: f64,
    contribution_tokens: u64,
    performance: ModelPerformance,
}

/// Grouped model bucket materialized by re-folding [`FineModelBucket`]s for
/// one `GroupBy`; converted to [`UsageModelEntry`] by
/// [`materialize_tui_model`].
struct TuiModelBucket {
    model: Arc<str>,
    providers: IdentitySet<Arc<str>>,
    client: Arc<str>,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    performance: ModelPerformance,
    sessions: IdentitySet<(Arc<str>, Arc<str>)>,
    // Boxed only for grouping modes that merge clients; keeps client-scoped
    // high-cardinality buckets free of an inline HashMap.
    #[allow(clippy::box_collection)]
    client_totals: Option<Box<HashMap<Arc<str>, ClientContributionOrder>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
enum AgentInstanceKey {
    Explicit(Arc<str>),
    Derived { client: Arc<str>, session: Arc<str> },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
struct AgentKey {
    client: Arc<str>,
    agent: Arc<str>,
}

#[derive(Serialize, Deserialize)]
struct AgentBucket {
    instances: IdentitySet<AgentInstanceKey>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    message_count: u32,
}

#[derive(Serialize, Deserialize)]
struct DailyBucket {
    date: NaiveDate,
    clients: HashMap<Arc<str>, DailyClientBucket>,
}

#[derive(Serialize, Deserialize)]
struct DailyClientBucket {
    tokens: UsageTokenBreakdown,
    cost: f64,
    message_count: u32,
    turn_count: u32,
    #[serde(with = "map_as_vec")]
    models: HashMap<FineModelKey, FineDailyModelBucket>,
}

#[derive(Serialize, Deserialize)]
struct FineDailyModelBucket {
    workspace_label: Arc<str>,
    first_seen: usize,
    tokens: UsageTokenBreakdown,
    cost: f64,
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
struct HourlyBucket {
    datetime: NaiveDateTime,
    clients: HashMap<Arc<str>, HourlyClientBucket>,
}

#[derive(Serialize, Deserialize)]
struct HourlyClientBucket {
    tokens: UsageTokenBreakdown,
    cost: f64,
    #[serde(with = "map_as_vec")]
    models: HashMap<FineHourlyModelKey, FineHourlyModelBucket>,
    message_count: u32,
    turn_count: u32,
}

#[derive(Serialize, Deserialize)]
struct FineHourlyModelBucket {
    first_seen: usize,
    tokens: UsageTokenBreakdown,
    cost: f64,
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

fn materialize_tui_model(mut bucket: TuiModelBucket) -> UsageModelEntry {
    let provider = bucket.providers.to_sorted_string();
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

fn materialize_daily_model(model: DailyModelBucket) -> DailyModelInfo {
    let provider = model.provider.to_string();
    DailyModelInfo {
        provider,
        model_id: model.model.to_string(),
        display_name: model.model.to_string(),
        workspace_key: model.workspace_key.map(|key| key.to_string()),
        workspace_label: model.workspace_label.map(|label| label.to_string()),
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
fn client_is_selected(client: &str, selected: Option<&HashSet<ClientId>>) -> bool {
    selected.is_none_or(|selected| crate::selected_client_ids_include(client, selected))
}

fn materialize_daily_common(
    bucket: &DailyBucket,
    selected: Option<&HashSet<ClientId>>,
) -> Option<DailyUsageCommon> {
    let mut clients = BTreeMap::new();
    let mut tokens = UsageTokenBreakdown::default();
    let mut cost = 0.0;
    let mut message_count = 0_u32;
    let mut turn_count = 0_u32;

    let mut selected_clients: Vec<_> = bucket
        .clients
        .iter()
        .filter(|(client, _)| client_is_selected(client, selected))
        .collect();
    selected_clients.sort_by_key(|(client, _)| *client);

    for (client, client_bucket) in selected_clients {
        clients.insert(
            client.to_string(),
            DailyClientCommon {
                tokens: client_bucket.tokens.clone(),
                cost: client_bucket.cost,
            },
        );
        add_tokens(&mut tokens, &client_bucket.tokens);
        cost += client_bucket.cost;
        message_count = message_count.saturating_add(client_bucket.message_count);
        turn_count = turn_count.saturating_add(client_bucket.turn_count);
    }
    (!clients.is_empty()).then_some(DailyUsageCommon {
        date: bucket.date,
        tokens,
        cost,
        clients,
        message_count,
        turn_count,
    })
}

fn materialize_daily_client_models(
    client_bucket: &DailyClientBucket,
    group_by: &GroupBy,
) -> BTreeMap<String, DailyModelInfo> {
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

    grouped_fine_models
        .into_iter()
        .map(|(key, fine_models)| {
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
                add_tokens(&mut grouped_model.tokens, &fine_model.tokens);
                grouped_model.cost += fine_model.cost;
                grouped_model.messages = grouped_model.messages.saturating_add(fine_model.messages);
            }
            let grouped_model =
                grouped_model.expect("daily target group contains at least one fine model bucket");
            (key.map_key(), materialize_daily_model(grouped_model))
        })
        .collect()
}

fn materialize_daily_models(
    bucket: &DailyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Option<DailyModelProjection> {
    let client_models: BTreeMap<_, _> = bucket
        .clients
        .iter()
        .filter(|(client, _)| client_is_selected(client, selected))
        .map(|(client, client_bucket)| {
            (
                client.to_string(),
                materialize_daily_client_models(client_bucket, group_by),
            )
        })
        .collect();

    (!client_models.is_empty()).then_some(DailyModelProjection {
        date: bucket.date,
        client_models,
    })
}

fn materialize_hourly_model(model: HourlyModelBucket) -> HourlyModelInfo {
    HourlyModelInfo {
        provider: model.provider.to_string(),
        model_id: model.model.to_string(),
        display_name: model.model.to_string(),
        tokens: model.tokens,
        cost: model.cost,
    }
}

/// Re-fold one hour's `(provider, model)` buckets into `group_by`'s hourly
/// models map. Only ClientProviderModel keeps the provider split; the other
/// groupings merge providers, attributing the first-created bucket's
/// provider (matching a direct grouped fold).
fn materialize_hourly_common(
    bucket: &HourlyBucket,
    selected: Option<&HashSet<ClientId>>,
) -> Option<HourlyUsageCommon> {
    let mut tokens = UsageTokenBreakdown::default();
    let mut cost = 0.0;
    let mut clients = BTreeSet::new();
    let mut message_count = 0_u32;
    let mut turn_count = 0_u32;

    let mut selected_clients: Vec<_> = bucket
        .clients
        .iter()
        .filter(|(client, _)| client_is_selected(client, selected))
        .collect();
    selected_clients.sort_by_key(|(client, _)| *client);

    for (client, client_bucket) in selected_clients {
        clients.insert(client.to_string());
        add_tokens(&mut tokens, &client_bucket.tokens);
        cost += client_bucket.cost;
        message_count = message_count.saturating_add(client_bucket.message_count);
        turn_count = turn_count.saturating_add(client_bucket.turn_count);
    }
    if clients.is_empty() {
        return None;
    }
    Some(HourlyUsageCommon {
        datetime: bucket.datetime,
        tokens,
        cost,
        clients,
        message_count,
        turn_count,
    })
}

fn materialize_hourly_models(
    bucket: &HourlyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Option<HourlyModelProjection> {
    let mut fine_models = Vec::new();
    for (client, client_bucket) in &bucket.clients {
        if client_is_selected(client, selected) {
            fine_models.extend(client_bucket.models.iter());
        }
    }
    if fine_models.is_empty() {
        return None;
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
        add_tokens(&mut grouped_model.tokens, &fine_model.tokens);
        grouped_model.cost += fine_model.cost;
    }
    let models = grouped_models
        .into_iter()
        .map(|(key, model)| (key.map_key(), materialize_hourly_model(model)))
        .collect();
    Some(HourlyModelProjection {
        datetime: bucket.datetime,
        models,
    })
}

impl TuiAcc {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, msg: &UnifiedMessage) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("TUI aggregation sequence exceeds usize::MAX");

        let msg_cost = sane_cost(msg.cost);

        let model_entry = self
            .model_map
            .entry(FineModelKey::from_message(msg))
            .or_insert_with(|| FineModelBucket {
                workspace_label: workspace_fields(msg).1,
                first_seen: sequence,
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                contribution_tokens: 0,
                performance: ModelPerformance::default(),
            });

        add_unified_tokens(&mut model_entry.tokens, &msg.tokens);
        model_entry.cost += msg_cost;
        model_entry.contribution_tokens = model_entry
            .contribution_tokens
            .checked_add(msg.tokens.total().max(0) as u64)
            .expect("client token contribution exceeds u64::MAX");
        model_entry
            .performance
            .record_message(positive_unified_token_total(&msg.tokens), msg.duration_ms);

        if let Some(agent) = msg.agent.as_ref() {
            let agent_entry = self
                .agent_map
                .entry(AgentKey {
                    client: Arc::clone(&msg.client),
                    agent: Arc::clone(agent),
                })
                .or_insert_with(|| AgentBucket {
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
                clients: HashMap::new(),
            });

            let client_entry = daily_entry
                .clients
                .entry(Arc::clone(&msg.client))
                .or_insert_with(|| DailyClientBucket {
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    message_count: 0,
                    turn_count: 0,
                    models: HashMap::new(),
                });
            add_unified_tokens(&mut client_entry.tokens, &msg.tokens);
            client_entry.cost += msg_cost;
            client_entry.message_count = client_entry
                .message_count
                .saturating_add(msg.message_count.max(0) as u32);
            if msg.is_turn_start {
                client_entry.turn_count = client_entry.turn_count.saturating_add(1);
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
                    clients: HashMap::new(),
                });
            let client_entry = hourly_entry
                .clients
                .entry(Arc::clone(&msg.client))
                .or_insert_with(|| HourlyClientBucket {
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    models: HashMap::new(),
                    message_count: 0,
                    turn_count: 0,
                });
            add_unified_tokens(&mut client_entry.tokens, &msg.tokens);
            client_entry.cost += msg_cost;
            client_entry.message_count = client_entry
                .message_count
                .saturating_add(msg.message_count.max(0) as u32);
            if msg.is_turn_start {
                client_entry.turn_count = client_entry.turn_count.saturating_add(1);
            }
            let hmodel = client_entry
                .models
                .entry(FineHourlyModelKey::from_message(msg))
                .or_insert_with(|| FineHourlyModelBucket {
                    first_seen: sequence,
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                });
            add_unified_tokens(&mut hmodel.tokens, &msg.tokens);
            hmodel.cost += msg_cost;
        }
    }

    /// Re-fold the canonical model buckets into `group_by`'s grouping. Within
    /// each merged group, fine buckets fold in creation order so floating-point
    /// sums stay deterministic across runs and repeated projections; the
    /// first-created bucket in the group attributes the single-client field,
    /// workspace label, and client first-seen (its first message is the
    /// group's first message, matching a direct grouped fold).
    fn refold_models(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
    ) -> Vec<(GroupedModelKey, TuiModelBucket)> {
        let mut grouped_fine_models: HashMap<
            GroupedModelKey,
            OneOrMany<(&FineModelKey, &FineModelBucket)>,
        > = HashMap::new();
        for (fine_key, fine_model) in &self.model_map {
            if !client_is_selected(&fine_key.client, selected) {
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
            let mut model_entry: Option<TuiModelBucket> = None;
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
                    TuiModelBucket {
                        model: Arc::clone(&fine_key.model),
                        providers: IdentitySet::default(),
                        client: Arc::clone(&fine_key.client),
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
                        .entry(Arc::clone(&fine_key.client))
                        .or_insert_with(|| ClientContributionOrder {
                            first_seen: fine_model.first_seen,
                            total_tokens: 0,
                        });
                    totals.total_tokens = totals
                        .total_tokens
                        .checked_add(fine_model.contribution_tokens)
                        .expect("client token contribution exceeds u64::MAX");
                }

                model_entry.providers.insert(Arc::clone(&fine_key.provider));

                add_tokens(&mut model_entry.tokens, &fine_model.tokens);
                model_entry.cost += fine_model.cost;
                model_entry.performance.merge(&fine_model.performance);

                model_entry
                    .sessions
                    .insert((Arc::clone(&fine_key.client), Arc::clone(&fine_key.session)));
            }
            model_buckets.push((
                key,
                model_entry.expect("target group contains at least one fine model bucket"),
            ));
        }
        model_buckets
    }

    /// Materialize one grouping's [`UsageData`] from the canonical fold
    /// state. Borrowing, so the same accumulator can be projected repeatedly
    /// with different groupings without rescanning local clients.
    pub fn project(&self, group_by: &GroupBy) -> UsageData {
        UsageData::from_projection_parts(self.project_common(), self.project_grouped(group_by))
            .expect("Common and Grouped usage projections from one accumulator must align")
    }

    /// Materialize a TUI view for a session-local subset of the clients that
    /// produced this accumulator. This is a pure projection: it never scans,
    /// reparses, or reprices input data.
    pub fn project_for_clients(
        &self,
        group_by: &GroupBy,
        selected: &HashSet<ClientId>,
    ) -> UsageData {
        UsageData::from_projection_parts(
            self.project_common_for_clients(selected),
            self.project_grouped_for_clients(group_by, selected),
        )
        .expect("Common and Grouped usage projections from one accumulator must align")
    }

    /// Materialize the Group By-independent portion of the full Client
    /// universe. Cache persistence stores this once per generation.
    pub fn project_common(&self) -> UsageCommonData {
        self.project_common_selected(None)
    }

    /// Materialize the Group By-independent portion of one Client subset.
    pub fn project_common_for_clients(&self, selected: &HashSet<ClientId>) -> UsageCommonData {
        self.project_common_selected(Some(selected))
    }

    fn project_common_selected(&self, selected: Option<&HashSet<ClientId>>) -> UsageCommonData {
        let mut agents: Vec<AgentEntry> = self
            .agent_map
            .iter()
            .filter_map(|(key, agent)| {
                if !client_is_selected(&key.client, selected) {
                    return None;
                }
                Some(AgentEntry {
                    agent: key.agent.to_string(),
                    client: key.client.to_string(),
                    tokens: agent.tokens.clone(),
                    cost: agent.cost,
                    message_count: agent.message_count,
                    instance_count: agent
                        .instances
                        .len()
                        .try_into()
                        .expect("agent instance count exceeds u32::MAX"),
                })
            })
            .collect();
        agents.sort_by(|a, b| {
            b.cost
                .total_cmp(&a.cost)
                .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                .then_with(|| a.agent.cmp(&b.agent))
                .then_with(|| a.client.cmp(&b.client))
        });

        let mut daily: Vec<DailyUsageCommon> = self
            .daily_map
            .values()
            .filter_map(|bucket| materialize_daily_common(bucket, selected))
            .collect();
        daily.sort_by_key(|usage| std::cmp::Reverse(usage.date));

        let mut hourly: Vec<HourlyUsageCommon> = self
            .hourly_map
            .values()
            .filter_map(|bucket| materialize_hourly_common(bucket, selected))
            .collect();
        hourly.sort_by_key(|usage| std::cmp::Reverse(usage.datetime));

        let mut selected_models: Vec<_> = self
            .model_map
            .iter()
            .filter(|(key, _)| client_is_selected(&key.client, selected))
            .collect();
        selected_models.sort_by_key(|(_, bucket)| bucket.first_seen);
        let mut total_token_breakdown = UsageTokenBreakdown::default();
        let mut total_cost = 0.0;
        for (_, bucket) in selected_models {
            add_tokens(&mut total_token_breakdown, &bucket.tokens);
            total_cost += bucket.cost;
        }

        let graph = build_common_contribution_graph(&daily);
        let (current_streak, longest_streak) = calculate_common_streaks(&daily);

        UsageCommonData {
            agents,
            daily,
            hourly,
            graph,
            total_tokens: total_token_breakdown.total(),
            total_cost: sane_cost(total_cost),
            current_streak,
            longest_streak,
        }
    }

    /// Materialize only the model fields reshaped by Group By for the full
    /// Client universe.
    pub fn project_grouped(&self, group_by: &GroupBy) -> UsageGroupedData {
        self.project_grouped_selected(group_by, None)
    }

    /// Materialize only the model fields reshaped by Group By for one Client
    /// subset.
    pub fn project_grouped_for_clients(
        &self,
        group_by: &GroupBy,
        selected: &HashSet<ClientId>,
    ) -> UsageGroupedData {
        self.project_grouped_selected(group_by, Some(selected))
    }

    fn project_grouped_selected(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
    ) -> UsageGroupedData {
        let mut keyed_models: Vec<_> = self
            .refold_models(group_by, selected)
            .into_iter()
            .map(|(key, bucket)| (key, materialize_tui_model(bucket)))
            .collect();
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

        let mut daily: Vec<DailyModelProjection> = self
            .daily_map
            .values()
            .filter_map(|bucket| materialize_daily_models(bucket, group_by, selected))
            .collect();
        daily.sort_by_key(|projection| std::cmp::Reverse(projection.date));

        let mut hourly: Vec<HourlyModelProjection> = self
            .hourly_map
            .values()
            .filter_map(|bucket| materialize_hourly_models(bucket, group_by, selected))
            .collect();
        hourly.sort_by_key(|projection| std::cmp::Reverse(projection.datetime));

        UsageGroupedData {
            models,
            daily,
            hourly,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

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
            let mut acc = TuiAcc::new();
            for message in &messages {
                acc.push(message);
            }
            Ok(acc.project(group_by))
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

        let daily_models = &usage.daily[0].client_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models
            .get("v1|cpm|8:opencode6:xiaomi13:mimo-v2.5-pro")
            .unwrap();
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

        let daily_models = &usage.daily[0].client_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models
            .get("v1|cpm|8:opencode6:openai7:gpt-5.5")
            .unwrap();
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

        let daily_models = &usage.daily[0].client_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("v1|cpm|8:opencode6:openai7:gpt-5.5"));
        assert!(daily_models.contains_key("v1|cpm|8:opencode9:microsoft7:gpt-5.5"));
        assert!(daily_models
            .values()
            .all(|model| model.display_name == "gpt-5.5"));
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
                Some("Builder".to_string()),
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
                Some("Builder".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 2);
        let opencode = usage
            .agents
            .iter()
            .find(|agent| agent.client == "opencode")
            .unwrap();
        assert_eq!(opencode.agent, "Builder");
        assert_eq!(opencode.message_count, 1);
        assert!((opencode.cost - 1.25).abs() < f64::EPSILON);
        assert_eq!(opencode.tokens.total(), 15);

        let roocode = usage
            .agents
            .iter()
            .find(|agent| agent.client == "roocode")
            .unwrap();
        assert_eq!(roocode.agent, "Builder");
        assert_eq!(roocode.message_count, 1);
        assert!((roocode.cost - 2.75).abs() < f64::EPSILON);
        assert_eq!(roocode.tokens.total(), 30);
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
        let claude = usage.daily[0].client_breakdown.get("claude").unwrap();
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);

        // The workspace dimension travels in structured fields; display_name
        // and model_id stay the bare canonical model (ADR 0026).
        let daily_identities: Vec<_> = claude
            .models
            .values()
            .map(|info| {
                (
                    info.display_name.clone(),
                    info.model_id.clone(),
                    info.workspace_key.clone(),
                    info.workspace_label.clone(),
                )
            })
            .collect();
        assert_eq!(
            daily_identities,
            vec![
                (
                    "claude-sonnet-4.5".to_string(),
                    "claude-sonnet-4.5".to_string(),
                    Some("/repo-a".to_string()),
                    Some("repo-a".to_string()),
                ),
                (
                    "claude-sonnet-4.5".to_string(),
                    "claude-sonnet-4.5".to_string(),
                    Some("/repo-b".to_string()),
                    Some("repo-b".to_string()),
                ),
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
        let claude = usage.daily[0].client_breakdown.get("claude").unwrap();
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
                "claude-sonnet-4.5".to_string(),
                "claude-sonnet-4.5".to_string()
            ]
        );
        let workspace_keys: Vec<_> = claude
            .models
            .values()
            .map(|info| info.workspace_key.clone())
            .collect();
        assert_eq!(
            workspace_keys,
            vec![
                Some("/srv/team-a/demo".to_string()),
                Some("/srv/team-b/demo".to_string())
            ]
        );
    }

    #[test]
    fn daily_model_identity_fields_follow_the_group_by_contract() {
        let loader = TuiUsageHarness;
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let usage = loader
                .aggregate_messages(
                    vec![make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/repo-a"),
                        Some("repo-a"),
                    )],
                    &group_by,
                )
                .unwrap();

            let models = &usage.daily[0].client_breakdown["claude"].models;
            assert_eq!(models.len(), 1);
            let info = models.values().next().unwrap();
            assert_eq!(info.model_id, "claude-sonnet-4.5");
            assert_eq!(info.display_name, "claude-sonnet-4.5");
            if group_by == GroupBy::WorkspaceModel {
                assert_eq!(info.workspace_key.as_deref(), Some("/repo-a"));
                assert_eq!(info.workspace_label.as_deref(), Some("repo-a"));
            } else {
                assert_eq!(info.workspace_key, None);
                assert_eq!(info.workspace_label, None);
            }

            let hourly = &usage.hourly[0].models;
            assert_eq!(hourly.len(), 1);
            assert_eq!(
                hourly.values().next().unwrap().model_id,
                "claude-sonnet-4.5"
            );
        }
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

        let claude = usage.daily[0].client_breakdown.get("claude").unwrap();
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
        let claude = usage.daily[0].client_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);

        let anthropic_key = "v1|cpm|6:claude9:anthropic17:claude-sonnet-4.5";
        let copilot_key = "v1|cpm|6:claude9:microsoft17:claude-sonnet-4.5";
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
    fn test_aggregate_messages_keeps_same_model_split_across_clients_in_daily_breakdown() {
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
                        "gemini",
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
        assert_eq!(usage.daily[0].client_breakdown.len(), 2);

        let claude = usage.daily[0].client_breakdown.get("claude").unwrap();
        assert_eq!(claude.cost, 1.0);
        assert_eq!(claude.models.len(), 1);
        let claude_model = claude.models.get("v1|m|17:claude-sonnet-4.5").unwrap();
        assert_eq!(claude_model.display_name, "claude-sonnet-4.5");
        assert_eq!(claude_model.tokens.total(), 15);

        let gemini = usage.daily[0].client_breakdown.get("gemini").unwrap();
        assert_eq!(gemini.cost, 2.0);
        assert_eq!(gemini.models.len(), 1);
        let gemini_model = gemini.models.get("v1|m|17:claude-sonnet-4.5").unwrap();
        assert_eq!(gemini_model.display_name, "claude-sonnet-4.5");
        assert_eq!(gemini_model.tokens.total(), 30);
    }

    #[test]
    fn test_aggregate_messages_does_not_reinterpret_opencode_agent_variants() {
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

        assert_eq!(usage.agents.len(), 2);
        assert!(usage.agents.iter().any(|agent| {
            agent.agent == "Sisyphus" && agent.client == "opencode" && agent.message_count == 1
        }));
        assert!(usage.agents.iter().any(|agent| {
            agent.agent == "Sisyphus (Ultraworker)"
                && agent.client == "opencode"
                && agent.message_count == 1
        }));
    }

    #[test]
    fn test_aggregate_messages_does_not_normalize_opencode_agent_case() {
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

        assert_eq!(usage.agents.len(), 2);
        assert!(usage.agents.iter().any(|agent| agent.agent == "Hephaestus"));
        assert!(usage.agents.iter().any(|agent| agent.agent == "hephaestus"));
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
    fn top_level_models_preserve_structured_buckets_with_colliding_legacy_text() {
        let timestamp = 1_735_689_600_000;
        let cases = [
            (
                GroupBy::ClientModel,
                collision_message("a:b", "first", "same", "c", 10, timestamp),
                collision_message("a", "second", "same", "b:c", 20, timestamp),
            ),
            (
                GroupBy::ClientProviderModel,
                collision_message("a", "b:c", "same", "d", 10, timestamp),
                collision_message("a", "b", "same", "c:d", 20, timestamp),
            ),
        ];

        for (group_by, first, second) in cases {
            let mut acc = TuiAcc::new();
            acc.push(&first);
            acc.push(&second);
            let usage = acc.project(&group_by);
            assert_eq!(usage.models.len(), 2);
            assert_eq!(usage.models[0].tokens.total(), 20);
            assert_eq!(usage.models[0].cost, 20.0);
            assert_eq!(usage.models[1].tokens.total(), 10);
            assert_eq!(usage.models[1].cost, 10.0);
        }
    }

    #[test]
    fn daily_and_hourly_maps_use_collision_free_structured_keys() {
        let timestamp = 1_735_689_600_000;
        let first = collision_message("a", "b:c", "same", "d", 10, timestamp);
        let second = collision_message("a", "b", "same", "c:d", 20, timestamp);
        let mut acc = TuiAcc::new();
        acc.push(&first);
        acc.push(&second);
        let usage = acc.project(&GroupBy::ClientProviderModel);

        let daily = &usage.daily[0].client_breakdown["a"].models;
        assert_eq!(daily.len(), 2);
        let first_daily = &daily["v1|cpm|1:a3:b:c1:d"];
        assert_eq!(first_daily.provider, "b:c");
        assert_eq!(first_daily.model_id, "d");
        assert_eq!(first_daily.display_name, "d");
        assert_eq!(first_daily.tokens.total(), 10);
        assert_eq!(first_daily.cost, 10.0);
        assert_eq!(first_daily.messages, 1);
        let second_daily = &daily["v1|cpm|1:a1:b3:c:d"];
        assert_eq!(second_daily.provider, "b");
        assert_eq!(second_daily.model_id, "c:d");
        assert_eq!(second_daily.display_name, "c:d");
        assert_eq!(second_daily.tokens.total(), 20);
        assert_eq!(second_daily.cost, 20.0);
        assert_eq!(second_daily.messages, 1);

        let hourly = &usage.hourly[0].models;
        assert_eq!(hourly.len(), 2);
        let first_hourly = &hourly["v1|pm|3:b:c1:d"];
        assert_eq!(first_hourly.provider, "b:c");
        assert_eq!(first_hourly.model_id, "d");
        assert_eq!(first_hourly.display_name, "d");
        assert_eq!(first_hourly.tokens.total(), 10);
        assert_eq!(first_hourly.cost, 10.0);
        let second_hourly = &hourly["v1|pm|1:b3:c:d"];
        assert_eq!(second_hourly.provider, "b");
        assert_eq!(second_hourly.model_id, "c:d");
        assert_eq!(second_hourly.display_name, "c:d");
        assert_eq!(second_hourly.tokens.total(), 20);
        assert_eq!(second_hourly.cost, 20.0);
    }

    #[test]
    fn hourly_client_identity_order_is_deterministic() {
        let timestamp = 1_735_689_600_000;
        let mut acc = TuiAcc::new();
        acc.push(&collision_message(
            "z-client",
            "provider",
            "session-z",
            "model",
            10,
            timestamp,
        ));
        acc.push(&collision_message(
            "a-client",
            "provider",
            "session-a",
            "model",
            20,
            timestamp,
        ));

        let usage = acc.project(&GroupBy::Model);

        assert_eq!(
            usage.hourly[0]
                .clients
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a-client", "z-client"]
        );
    }

    #[test]
    fn workspace_maps_tag_unknown_and_known_keys_separately() {
        let timestamp = 1_735_689_600_000;
        let mut unknown =
            collision_message("client", "provider", "session", "model", 10, timestamp);
        unknown.workspace_key = None;
        unknown.workspace_label = None;
        let mut known = collision_message("client", "provider", "session", "model", 20, timestamp);
        known.workspace_key = Some(Arc::from(""));
        known.workspace_label = Some(Arc::from("Empty workspace key"));

        let mut acc = TuiAcc::new();
        acc.push(&unknown);
        acc.push(&known);
        let usage = acc.project(&GroupBy::WorkspaceModel);

        assert_eq!(usage.models.len(), 2);
        let daily_models = &usage.daily[0].client_breakdown["client"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("v1|wmu|5:model"));
        assert!(daily_models.contains_key("v1|wmk|0:5:model"));
    }

    #[test]
    fn structured_session_and_agent_instance_identities_do_not_alias_delimiters() {
        let timestamp = 1_735_689_600_000;
        let model_messages = [
            collision_message("a:b", "provider", "c", "model", 10, timestamp),
            collision_message("a", "provider", "b:c", "model", 20, timestamp),
        ];
        let mut model_acc = TuiAcc::new();
        for message in &model_messages {
            model_acc.push(message);
        }
        assert_eq!(
            model_acc.project(&GroupBy::Model).models[0].session_count,
            2
        );

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
            "a",
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
        let mut agent_acc = TuiAcc::new();
        for message in [&explicit, &derived_left, &derived_right] {
            agent_acc.push(message);
        }
        assert_eq!(
            agent_acc.project(&GroupBy::Model).agents[0].instance_count,
            3
        );
    }

    // ---- group-by re-projection (issue #161) ----

    #[allow(clippy::too_many_arguments)]
    fn reprojection_message(
        client: &str,
        model: &str,
        provider: &str,
        session: &str,
        timestamp: i64,
        input: i64,
        output: i64,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
        duration_ms: Option<i64>,
        is_turn_start: bool,
        agent: Option<&str>,
    ) -> UnifiedMessage {
        let mut msg = UnifiedMessage::new_with_agent(
            client,
            model,
            provider,
            session,
            timestamp,
            crate::TokenBreakdown {
                input,
                output,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
            agent.map(str::to_string),
        );
        msg.set_workspace(
            workspace_key.map(str::to_string),
            workspace_label.map(str::to_string),
        );
        msg.duration_ms = duration_ms;
        msg.is_turn_start = is_turn_start;
        msg
    }

    /// Noon-based UTC timestamps keep day and hour buckets stable across test
    /// host time zones.
    fn reprojection_ts(day_offset: i64, hour: i64) -> i64 {
        (1_749_945_600 + day_offset * 86_400 + hour * 3_600) * 1_000
    }

    /// One corpus exercising every dimension a grouping can re-fold: shared
    /// models across clients, one client:model pair across two providers,
    /// workspaces with/without labels and an unknown workspace, two sessions
    /// per client, two days, two hours, durations, turn starts, and an agent.
    fn reprojection_corpus() -> Vec<UnifiedMessage> {
        vec![
            reprojection_message(
                "claude",
                "gpt-5.5",
                "openai",
                "s1",
                reprojection_ts(0, 12),
                100,
                50,
                0.1,
                Some("/repo-a"),
                Some("repo-a"),
                Some(1000),
                true,
                Some("builder"),
            ),
            reprojection_message(
                "codex",
                "gpt-5.5",
                "openai",
                "s2",
                reprojection_ts(0, 12),
                200,
                60,
                0.2,
                Some("/repo-a"),
                Some("repo-a"),
                Some(2000),
                false,
                None,
            ),
            reprojection_message(
                "claude",
                "gpt-5.5",
                "azure",
                "s1",
                reprojection_ts(0, 13),
                300,
                70,
                0.3,
                Some("/repo-b"),
                None,
                None,
                false,
                None,
            ),
            reprojection_message(
                "claude",
                "claude-sonnet-4.5",
                "anthropic",
                "s3",
                reprojection_ts(1, 12),
                400,
                80,
                0.4,
                None,
                None,
                Some(4000),
                true,
                None,
            ),
            reprojection_message(
                "qwen",
                "gpt-5.5",
                "openai",
                "s4",
                reprojection_ts(1, 13),
                500,
                90,
                0.5,
                Some("/repo-a"),
                Some("repo-a"),
                None,
                false,
                None,
            ),
        ]
    }

    fn reprojection_accumulator() -> TuiAcc {
        let mut acc = TuiAcc::new();
        for message in reprojection_corpus() {
            acc.push(&message);
        }
        acc
    }

    fn assert_tokens_eq(left: &UsageTokenBreakdown, right: &UsageTokenBreakdown) {
        assert_eq!(left.input, right.input);
        assert_eq!(left.output, right.output);
        assert_eq!(left.cache_read, right.cache_read);
        assert_eq!(left.cache_write, right.cache_write);
        assert_eq!(left.reasoning, right.reasoning);
    }

    fn assert_agents_eq(left: &[AgentEntry], right: &[AgentEntry]) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_eq!(left.agent, right.agent);
            assert_eq!(left.client, right.client);
            assert_tokens_eq(&left.tokens, &right.tokens);
            assert_eq!(left.cost.to_bits(), right.cost.to_bits());
            assert_eq!(left.message_count, right.message_count);
            assert_eq!(left.instance_count, right.instance_count);
        }
    }

    fn assert_graph_eq(left: &UsageGraphData, right: &UsageGraphData) {
        assert_eq!(left.weeks.len(), right.weeks.len());
        for (left_week, right_week) in left.weeks.iter().zip(&right.weeks) {
            assert_eq!(left_week.len(), right_week.len());
            for (left_day, right_day) in left_week.iter().zip(right_week) {
                match (left_day, right_day) {
                    (Some(left), Some(right)) => {
                        assert_eq!(left.date, right.date);
                        assert_eq!(left.tokens, right.tokens);
                        assert_eq!(left.cost.to_bits(), right.cost.to_bits());
                        assert_eq!(left.intensity.to_bits(), right.intensity.to_bits());
                    }
                    (None, None) => {}
                    _ => panic!("graph day presence mismatch"),
                }
            }
        }
    }

    fn assert_usage_data_eq(left: &UsageData, right: &UsageData) {
        assert_eq!(left.total_tokens, right.total_tokens);
        assert_eq!(left.total_cost.to_bits(), right.total_cost.to_bits());
        assert_eq!(left.current_streak, right.current_streak);
        assert_eq!(left.longest_streak, right.longest_streak);

        assert_eq!(left.models.len(), right.models.len());
        for (left, right) in left.models.iter().zip(&right.models) {
            assert_eq!(left.model, right.model);
            assert_eq!(left.provider, right.provider);
            assert_eq!(left.client, right.client);
            assert_eq!(left.workspace_key, right.workspace_key);
            assert_eq!(left.workspace_label, right.workspace_label);
            assert_tokens_eq(&left.tokens, &right.tokens);
            assert_eq!(left.cost.to_bits(), right.cost.to_bits());
            assert_eq!(left.performance, right.performance);
            assert_eq!(left.session_count, right.session_count);
        }

        assert_agents_eq(&left.agents, &right.agents);

        assert_eq!(left.daily.len(), right.daily.len());
        for (left, right) in left.daily.iter().zip(&right.daily) {
            assert_eq!(left.date, right.date);
            assert_tokens_eq(&left.tokens, &right.tokens);
            assert_eq!(left.cost.to_bits(), right.cost.to_bits());
            assert_eq!(left.message_count, right.message_count);
            assert_eq!(left.turn_count, right.turn_count);
            assert_eq!(left.client_breakdown.len(), right.client_breakdown.len());
            for ((left_client, left_info), (right_client, right_info)) in
                left.client_breakdown.iter().zip(&right.client_breakdown)
            {
                assert_eq!(left_client, right_client);
                assert_tokens_eq(&left_info.tokens, &right_info.tokens);
                assert_eq!(left_info.cost.to_bits(), right_info.cost.to_bits());
                assert_eq!(left_info.models.len(), right_info.models.len());
                for ((left_key, left_model), (right_key, right_model)) in
                    left_info.models.iter().zip(&right_info.models)
                {
                    assert_eq!(left_key, right_key);
                    assert_eq!(left_model.provider, right_model.provider);
                    assert_eq!(left_model.model_id, right_model.model_id);
                    assert_eq!(left_model.display_name, right_model.display_name);
                    assert_eq!(left_model.workspace_key, right_model.workspace_key);
                    assert_eq!(left_model.workspace_label, right_model.workspace_label);
                    assert_tokens_eq(&left_model.tokens, &right_model.tokens);
                    assert_eq!(left_model.cost.to_bits(), right_model.cost.to_bits());
                    assert_eq!(left_model.messages, right_model.messages);
                }
            }
        }

        assert_eq!(left.hourly.len(), right.hourly.len());
        for (left, right) in left.hourly.iter().zip(&right.hourly) {
            assert_eq!(left.datetime, right.datetime);
            assert_tokens_eq(&left.tokens, &right.tokens);
            assert_eq!(left.cost.to_bits(), right.cost.to_bits());
            assert_eq!(left.clients, right.clients);
            assert_eq!(left.message_count, right.message_count);
            assert_eq!(left.turn_count, right.turn_count);
            assert_eq!(left.models.len(), right.models.len());
            for ((left_key, left_model), (right_key, right_model)) in
                left.models.iter().zip(&right.models)
            {
                assert_eq!(left_key, right_key);
                assert_eq!(left_model.provider, right_model.provider);
                assert_eq!(left_model.model_id, right_model.model_id);
                assert_eq!(left_model.display_name, right_model.display_name);
                assert_tokens_eq(&left_model.tokens, &right_model.tokens);
                assert_eq!(left_model.cost.to_bits(), right_model.cost.to_bits());
            }
        }

        assert_graph_eq(&left.graph, &right.graph);
    }

    #[test]
    fn reprojection_is_repeatable_and_deterministic() {
        let acc = reprojection_accumulator();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let first = acc.project(&group_by);
            let second = acc.project(&group_by);
            assert_usage_data_eq(&first, &second);
        }
    }

    #[test]
    fn common_and_grouped_parts_reassemble_every_public_projection() {
        let acc = reprojection_accumulator();
        let common = acc.project_common();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let assembled =
                UsageData::from_projection_parts(common.clone(), acc.project_grouped(&group_by))
                    .unwrap();
            assert_usage_data_eq(&assembled, &acc.project(&group_by));
        }
    }

    #[test]
    fn client_scoped_common_and_grouped_parts_reassemble_without_other_clients() {
        let acc = reprojection_accumulator();
        let selected = HashSet::from([ClientId::Claude]);
        let common = acc.project_common_for_clients(&selected);
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let assembled = UsageData::from_projection_parts(
                common.clone(),
                acc.project_grouped_for_clients(&group_by, &selected),
            )
            .unwrap();
            assert_usage_data_eq(&assembled, &acc.project_for_clients(&group_by, &selected));
            assert!(assembled
                .agents
                .iter()
                .all(|agent| agent.client == "claude"));
        }
    }

    #[test]
    fn independently_built_accumulators_project_identically_across_hash_seeds() {
        // Separate accumulators create independently seeded HashMaps.
        let first = reprojection_accumulator();
        let second = reprojection_accumulator();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            assert_usage_data_eq(&first.project(&group_by), &second.project(&group_by));
        }
    }

    #[test]
    fn client_projection_matches_a_fresh_fold_of_only_the_selected_clients() {
        let corpus = reprojection_corpus();
        let full = reprojection_accumulator();
        for selected in [
            HashSet::from([ClientId::Claude]),
            HashSet::from([ClientId::Codex]),
            HashSet::from([ClientId::Claude, ClientId::Codex]),
        ] {
            let mut expected = TuiAcc::new();
            for message in &corpus {
                if crate::selected_client_ids_include(&message.client, &selected) {
                    expected.push(message);
                }
            }
            for group_by in [
                GroupBy::Model,
                GroupBy::ClientModel,
                GroupBy::ClientProviderModel,
                GroupBy::WorkspaceModel,
            ] {
                assert_usage_data_eq(
                    &full.project_for_clients(&group_by, &selected),
                    &expected.project(&group_by),
                );
            }
        }
    }

    #[test]
    fn reprojection_switching_groupings_does_not_pollute_state() {
        let acc = reprojection_accumulator();
        let baseline = acc.project(&GroupBy::Model);
        let _ = acc.project(&GroupBy::ClientProviderModel);
        let _ = acc.project(&GroupBy::WorkspaceModel);
        let rerun = acc.project(&GroupBy::Model);
        assert_usage_data_eq(&baseline, &rerun);
    }

    #[test]
    fn reprojection_preserves_group_independent_views() {
        let acc = reprojection_accumulator();
        let reference = acc.project(&GroupBy::Model);
        for group_by in [
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let projected = acc.project(&group_by);
            assert_eq!(projected.total_tokens, reference.total_tokens);
            assert!((projected.total_cost - reference.total_cost).abs() < 1e-9);
            assert_agents_eq(&projected.agents, &reference.agents);
            assert_eq!(
                (projected.current_streak, projected.longest_streak),
                (reference.current_streak, reference.longest_streak)
            );

            // Day- and hour-level rollups are accumulated per message, not
            // re-folded, so they stay bit-identical across groupings.
            let daily_rollup = |data: &UsageData| {
                data.daily
                    .iter()
                    .map(|day| {
                        (
                            day.date,
                            day.tokens.total(),
                            day.cost.to_bits(),
                            day.message_count,
                            day.turn_count,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(daily_rollup(&projected), daily_rollup(&reference));
            let hourly_rollup = |data: &UsageData| {
                data.hourly
                    .iter()
                    .map(|hour| {
                        (
                            hour.datetime,
                            hour.tokens.total(),
                            hour.cost.to_bits(),
                            hour.message_count,
                            hour.turn_count,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(hourly_rollup(&projected), hourly_rollup(&reference));
            assert_graph_eq(&projected.graph, &reference.graph);
        }
    }

    #[test]
    fn reprojection_derives_each_grouping_from_one_accumulator() {
        let corpus = reprojection_corpus();
        let day0 = corpus[0].local_date().expect("day0 local date");
        let day1 = corpus[3].local_date().expect("day1 local date");
        let day0_hour12 = timestamp_to_hour(corpus[0].timestamp).expect("day0 hour12");
        let day0_hour13 = timestamp_to_hour(corpus[2].timestamp).expect("day0 hour13");
        let mut acc = TuiAcc::new();
        for message in &corpus {
            acc.push(message);
        }

        let model = acc.project(&GroupBy::Model);
        assert_eq!(model.models.len(), 2);
        let gpt = model
            .models
            .iter()
            .find(|entry| entry.model == "gpt-5.5")
            .expect("merged gpt-5.5 entry");
        assert_eq!(gpt.provider, "azure, openai");
        assert_eq!(gpt.client, "qwen, claude, codex");
        assert_eq!(gpt.session_count, 3);
        assert_eq!(gpt.tokens.total(), 1370);
        assert!((gpt.cost - 1.1).abs() < 1e-9);
        assert_eq!(gpt.workspace_key, None);
        assert_eq!(gpt.workspace_label, None);
        assert_eq!(gpt.performance.total_duration_ms, 3000);
        assert_eq!(gpt.performance.timed_tokens, 410);
        assert_eq!(gpt.performance.sample_count, 2);
        let sonnet = model
            .models
            .iter()
            .find(|entry| entry.model == "claude-sonnet-4.5")
            .expect("claude-sonnet-4.5 entry");
        assert_eq!(sonnet.session_count, 1);
        assert_eq!(
            sonnet.performance.ms_per_1k_tokens,
            Some(4000.0 * 1000.0 / 480.0)
        );
        assert_eq!(sonnet.performance.token_coverage, 1.0);

        // Model grouping merges providers in the daily detail, attributing
        // the first-seen provider, and keeps bare-model hourly keys.
        let day0_claude = &model
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown["claude"];
        let gpt_daily = &day0_claude.models["v1|m|7:gpt-5.5"];
        assert_eq!(gpt_daily.provider, "openai");
        assert_eq!(gpt_daily.tokens.total(), 520);
        assert!((gpt_daily.cost - 0.4).abs() < 1e-9);
        let hour12 = model
            .hourly
            .iter()
            .find(|hour| hour.datetime == day0_hour12)
            .expect("day0 hour12 usage");
        assert!(hour12.models.contains_key("v1|m|7:gpt-5.5"));

        let client_model = acc.project(&GroupBy::ClientModel);
        assert_eq!(client_model.models.len(), 4);
        let claude_gpt = client_model
            .models
            .iter()
            .find(|entry| entry.model == "gpt-5.5" && entry.client == "claude")
            .expect("claude gpt-5.5 entry");
        assert_eq!(claude_gpt.provider, "azure, openai");
        assert_eq!(claude_gpt.tokens.total(), 520);
        assert_eq!(claude_gpt.session_count, 1);
        let day0_claude = &client_model
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown["claude"];
        assert!(day0_claude.models.contains_key("v1|cm|6:claude7:gpt-5.5"));

        let cpm = acc.project(&GroupBy::ClientProviderModel);
        assert_eq!(cpm.models.len(), 5);
        let day0_claude = &cpm
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown["claude"];
        assert!(day0_claude
            .models
            .contains_key("v1|cpm|6:claude6:openai7:gpt-5.5"));
        assert!(day0_claude
            .models
            .contains_key("v1|cpm|6:claude5:azure7:gpt-5.5"));
        // Only ClientProviderModel splits the hourly models map by provider.
        let cpm_hour13 = cpm
            .hourly
            .iter()
            .find(|hour| hour.datetime == day0_hour13)
            .expect("day0 hour13 usage");
        assert_eq!(cpm_hour13.models.len(), 1);
        assert!(cpm_hour13.models.contains_key("v1|pm|5:azure7:gpt-5.5"));
        let model_hour13 = model
            .hourly
            .iter()
            .find(|hour| hour.datetime == day0_hour13)
            .expect("day0 hour13 usage");
        assert!(model_hour13.models.contains_key("v1|m|7:gpt-5.5"));

        let workspace = acc.project(&GroupBy::WorkspaceModel);
        assert_eq!(workspace.models.len(), 3);
        let repo_a = workspace
            .models
            .iter()
            .find(|entry| entry.workspace_key.as_deref() == Some("/repo-a"))
            .expect("repo-a workspace entry");
        assert_eq!(repo_a.workspace_label.as_deref(), Some("repo-a"));
        assert_eq!(repo_a.client, "qwen, codex, claude");
        assert_eq!(repo_a.session_count, 3);
        assert_eq!(repo_a.tokens.total(), 1000);
        let repo_b = workspace
            .models
            .iter()
            .find(|entry| entry.workspace_key.as_deref() == Some("/repo-b"))
            .expect("repo-b workspace entry");
        assert_eq!(repo_b.workspace_label.as_deref(), Some("repo-b"));
        let unknown = workspace
            .models
            .iter()
            .find(|entry| entry.workspace_key.is_none())
            .expect("unknown workspace entry");
        assert_eq!(
            unknown.workspace_label.as_deref(),
            Some(UNKNOWN_WORKSPACE_LABEL)
        );
        let day0_claude = &workspace
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown["claude"];
        assert!(day0_claude.models.contains_key("v1|wmk|7:/repo-a7:gpt-5.5"));
        assert!(day0_claude.models.contains_key("v1|wmk|7:/repo-b7:gpt-5.5"));
        let day1_claude = &workspace
            .daily
            .iter()
            .find(|day| day.date == day1)
            .expect("day1 usage")
            .client_breakdown["claude"];
        assert!(day1_claude
            .models
            .contains_key("v1|wmu|17:claude-sonnet-4.5"));
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
