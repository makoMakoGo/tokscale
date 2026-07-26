//! Canonical usage indexing: one fold over `AttributedUsageRecord`s producing
//! immutable state for model-only and complete usage projections.
//!
//! The fold is group-independent: records land in canonical finest-granularity
//! buckets, and [`FrozenUsageIndex`] re-folds them into any grouping in memory
//! without rescanning inputs.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday};
use serde::{Deserialize, Serialize};

use crate::projection::{
    AgentEntry, ContributionDay, ContributionGrade, DailyClientInfo, DailyModelInfo, DailyUsage,
    HourlyModelInfo, HourlyUsage, ModelProjection, PeriodKind, PeriodUsage, UsageGraphData,
    UsageModelEntry, UsageProjection, UsageTokenBreakdown,
};
use crate::{
    aggregate::keys::{
        workspace_fields, FineHourlyModelKey, FineModelKey, GroupedModelKey, HourlyModelKey,
        IdentitySet,
    },
    AttributedUsageRecord, ClientId, ClientUniverse, GroupBy,
};

/// Sanitize a message cost: non-finite/negative -> 0 (reports never show debt).
fn sane_cost(cost: f64) -> f64 {
    if cost.is_finite() && cost > 0.0 {
        cost
    } else {
        0.0
    }
}

fn add_record_tokens(target: &mut UsageTokenBreakdown, src: &crate::TokenBreakdown) {
    let addition = UsageTokenBreakdown {
        input: src.input.max(0) as u64,
        output: src.output.max(0) as u64,
        cache_read: src.cache_read.max(0) as u64,
        cache_write: src.cache_write.max(0) as u64,
        reasoning: src.reasoning.max(0) as u64,
    };
    *target = target
        .checked_add(&addition)
        .expect("usage token buckets exceed u64::MAX while aggregating usage");
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
        .expect("usage token buckets exceed u64::MAX while aggregating usage");
}

fn merge_daily_clients(
    target: &mut BTreeMap<ClientId, DailyClientInfo>,
    clients: &BTreeMap<ClientId, DailyClientInfo>,
) {
    for (client_key, client_info) in clients {
        let target_client = target
            .entry(*client_key)
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

#[derive(Debug, Clone, Copy)]
struct ContributionLogMad {
    center: f64,
    scale: Option<f64>,
    max_tokens: u64,
}

impl ContributionLogMad {
    fn from_tokens(tokens: &[u64]) -> Option<Self> {
        let max_tokens = tokens.iter().copied().max()?;
        let mut logs: Vec<f64> = tokens
            .iter()
            .copied()
            .filter(|tokens| *tokens > 0)
            .map(|tokens| (tokens as f64).ln())
            .collect();
        if logs.is_empty() {
            return None;
        }
        logs.sort_unstable_by(f64::total_cmp);
        let center = median_of_sorted(&logs);

        let mut deviations: Vec<f64> = logs.iter().map(|value| (value - center).abs()).collect();
        deviations.sort_unstable_by(f64::total_cmp);
        let mad = median_of_sorted(&deviations);
        let scale = if mad > 0.0 {
            Some(mad)
        } else {
            let positive_deviations: Vec<f64> = deviations
                .into_iter()
                .filter(|deviation| *deviation > 0.0)
                .collect();
            (!positive_deviations.is_empty()).then(|| median_of_sorted(&positive_deviations))
        };

        Some(Self {
            center,
            scale,
            max_tokens,
        })
    }

    fn grade(self, tokens: u64) -> ContributionGrade {
        if tokens == 0 {
            return ContributionGrade::Empty;
        }
        if tokens == self.max_tokens {
            return ContributionGrade::Peak;
        }
        let Some(scale) = self.scale else {
            return ContributionGrade::Peak;
        };

        let value = (tokens as f64).ln();
        if value < self.center - scale {
            ContributionGrade::Low
        } else if value < self.center {
            ContributionGrade::Medium
        } else if value < self.center + scale {
            ContributionGrade::High
        } else {
            ContributionGrade::Peak
        }
    }
}

fn median_of_sorted(values: &[f64]) -> f64 {
    debug_assert!(!values.is_empty());
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
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
    let visible_active_tokens: Vec<u64> = daily_map
        .iter()
        .filter(|(date, _)| **date >= start_date && **date <= end_date)
        .map(|(_, usage)| tokens_of(usage))
        .filter(|tokens| *tokens > 0)
        .collect();
    let grade_scale = ContributionLogMad::from_tokens(&visible_active_tokens);
    let mut weeks: Vec<Vec<Option<ContributionDay>>> = Vec::new();
    let mut current_week: Vec<Option<ContributionDay>> = Vec::new();
    let mut current_date = start_date;
    while current_date <= end_date {
        let day = if let Some(usage) = daily_map.get(&current_date) {
            let tokens = tokens_of(usage);
            let grade = grade_scale.map_or(ContributionGrade::Empty, |scale| scale.grade(tokens));
            Some(ContributionDay {
                date: current_date,
                tokens,
                cost: cost_of(usage),
                grade,
            })
        } else {
            Some(ContributionDay {
                date: current_date,
                tokens: 0,
                cost: 0.0,
                grade: ContributionGrade::Empty,
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

pub fn calculate_streaks_for_today(daily: &[DailyUsage], today: NaiveDate) -> (u32, u32) {
    calculate_streaks_for_today_by(daily, today, |usage| usage.date)
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

/// Write-only lifecycle owner for canonical usage indexing.
///
/// `next_sequence` exists only while messages are folded. Finishing consumes
/// the builder, so a generation can never retain mutable accumulation state.
#[derive(Default)]
pub(crate) struct UsageIndexBuilder {
    index: FrozenUsageIndex,
    next_sequence: usize,
}

/// Immutable canonical usage index installed in a generation.
///
/// The materialized maps preserve the single-fold projection performance
/// design while excluding write-only lifecycle state such as the next
/// first-seen sequence.
#[derive(Default, Serialize, Deserialize)]
pub struct FrozenUsageIndex {
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

#[derive(Default, Serialize, Deserialize)]
struct UsageTotalsBucket {
    tokens: UsageTokenBreakdown,
    cost: f64,
}

impl UsageTotalsBucket {
    fn push(&mut self, msg: &AttributedUsageRecord, msg_cost: f64) {
        add_record_tokens(&mut self.tokens, &msg.tokens);
        self.cost += msg_cost;
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
                    "duplicate key in canonical usage-index map",
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
            .contains("duplicate key in canonical usage-index map"));
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
enum AgentInstanceKey {
    Explicit(Arc<str>),
    Derived { client: ClientId, session: Arc<str> },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
struct AgentKey {
    client: ClientId,
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
    clients: HashMap<ClientId, DailyClientBucket>,
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
    clients: HashMap<ClientId, HourlyClientBucket>,
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

fn materialize_model(bucket: ModelBucket) -> UsageModelEntry {
    let provider = bucket.providers.to_sorted_string();
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
    UsageModelEntry {
        model_id: bucket.model.to_string(),
        display_name: bucket.model.to_string(),
        provider,
        clients,
        workspace_key: bucket.workspace_key.map(|key| key.to_string()),
        workspace_label: bucket.workspace_label.map(|label| label.to_string()),
        tokens: bucket.tokens,
        cost: bucket.cost,
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
fn client_is_selected(client: ClientId, selected: Option<&HashSet<ClientId>>) -> bool {
    selected.is_none_or(|selected| selected.contains(&client))
}

fn materialize_daily_usage(
    bucket: &DailyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Option<DailyUsage> {
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
        client_breakdown.insert(
            *client,
            DailyClientInfo {
                tokens: client_bucket.tokens.clone(),
                cost: client_bucket.cost,
                models: materialize_daily_client_models(client_bucket, group_by),
            },
        );
        add_tokens(&mut tokens, &client_bucket.tokens);
        cost += client_bucket.cost;
        message_count = message_count.saturating_add(client_bucket.message_count);
        turn_count = turn_count.saturating_add(client_bucket.turn_count);
    }
    (!client_breakdown.is_empty()).then_some(DailyUsage {
        date: bucket.date,
        tokens,
        cost,
        client_breakdown,
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
fn materialize_hourly_usage(
    bucket: &HourlyBucket,
    group_by: &GroupBy,
    selected: Option<&HashSet<ClientId>>,
) -> Option<HourlyUsage> {
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
        add_tokens(&mut tokens, &client_bucket.tokens);
        cost += client_bucket.cost;
        message_count = message_count.saturating_add(client_bucket.message_count);
        turn_count = turn_count.saturating_add(client_bucket.turn_count);
        fine_models.extend(client_bucket.models.iter());
    }
    if clients.is_empty() {
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
    Some(HourlyUsage {
        datetime: bucket.datetime,
        tokens,
        cost,
        clients,
        models,
        message_count,
        turn_count,
    })
}

impl UsageIndexBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, msg: &AttributedUsageRecord) {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("usage indexing sequence exceeds usize::MAX");

        let msg_cost = sane_cost(msg.cost);
        let client = msg.client;

        self.index
            .usage_totals_by_client
            .entry(client)
            .or_default()
            .push(msg, msg_cost);

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

        add_record_tokens(&mut model_entry.tokens, &msg.tokens);
        model_entry.cost += msg_cost;
        model_entry.contribution_tokens = model_entry
            .contribution_tokens
            .checked_add(msg.tokens.total().max(0) as u64)
            .expect("client token contribution exceeds u64::MAX");
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
            add_record_tokens(&mut agent_entry.tokens, &msg.tokens);
            agent_entry.cost += msg_cost;
            agent_entry.message_count = agent_entry
                .message_count
                .saturating_add(msg.message_count.max(0) as u32);
            let instance_key = msg.agent_instance.as_ref().map_or_else(
                || AgentInstanceKey::Derived {
                    client,
                    session: Arc::clone(&msg.session_id),
                },
                |instance| AgentInstanceKey::Explicit(Arc::clone(instance)),
            );
            agent_entry.instances.insert(instance_key);
        }

        if let Some(date) = msg.local_date() {
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
            add_record_tokens(&mut client_entry.tokens, &msg.tokens);
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
            add_record_tokens(&mut model_info.tokens, &msg.tokens);
            model_info.cost += msg_cost;
            model_info.messages = model_info
                .messages
                .saturating_add(msg.message_count.max(0) as u64);
        }

        if let Some(bucket) = timestamp_to_hour(msg.timestamp) {
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
            add_record_tokens(&mut client_entry.tokens, &msg.tokens);
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
            add_record_tokens(&mut hmodel.tokens, &msg.tokens);
            hmodel.cost += msg_cost;
        }
    }

    pub(crate) fn finish(self) -> FrozenUsageIndex {
        self.index
    }
}

impl FrozenUsageIndex {
    pub fn new() -> Self {
        Self::default()
    }

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
                client_bucket.tokens.checked_total().ok_or(
                    UsageIndexValidationError::TokenTotalsOverflow {
                        index: HOURLY,
                        client: *client,
                    },
                )?;
                let mut model_totals = UsageTokenBreakdown::default();
                for model in client_bucket.models.values() {
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

        Ok(())
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
    ) -> Vec<(GroupedModelKey, ModelBucket)> {
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
                        .expect("client token contribution exceeds u64::MAX");
                }

                model_entry.providers.insert(Arc::clone(&fine_key.provider));

                add_tokens(&mut model_entry.tokens, &fine_model.tokens);
                model_entry.cost += fine_model.cost;

                model_entry
                    .sessions
                    .insert((fine_key.client, Arc::clone(&fine_key.session)));
            }
            model_buckets.push((
                key,
                model_entry.expect("target group contains at least one fine model bucket"),
            ));
        }
        model_buckets
    }

    /// Materialize models and their aggregate totals without date-dependent
    /// timeline, graph, or streak work.
    pub fn project_models(&self, group_by: &GroupBy) -> ModelProjection {
        self.project_models_selected(group_by, None)
    }

    /// Materialize models and totals for a client subset without acquiring or
    /// mutating canonical generation state.
    pub fn project_models_for_clients(
        &self,
        group_by: &GroupBy,
        selected: &HashSet<ClientId>,
    ) -> ModelProjection {
        self.project_models_selected(group_by, Some(selected))
    }

    fn project_models_selected(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
    ) -> ModelProjection {
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
            add_tokens(&mut total_token_breakdown, &totals.tokens);
            total_cost += totals.cost;
        }

        let mut keyed_models: Vec<_> = self
            .refold_models(group_by, selected)
            .into_iter()
            .map(|(key, bucket)| (key, materialize_model(bucket)))
            .collect();
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

        ModelProjection {
            models: keyed_models.into_iter().map(|(_, model)| model).collect(),
            total_tokens: total_token_breakdown.total(),
            total_cost: sane_cost(total_cost),
        }
    }

    /// Materialize one grouping's [`UsageProjection`] from the canonical fold
    /// state. Borrowing, so the same accumulator can be projected repeatedly
    /// with different groupings without rescanning local clients.
    pub fn project_usage(&self, group_by: &GroupBy, effective_date: NaiveDate) -> UsageProjection {
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
    ) -> UsageProjection {
        self.project_usage_selected(group_by, Some(selected), effective_date)
    }

    fn project_usage_selected(
        &self,
        group_by: &GroupBy,
        selected: Option<&HashSet<ClientId>>,
        effective_date: NaiveDate,
    ) -> UsageProjection {
        let ModelProjection {
            models,
            total_tokens,
            total_cost,
        } = self.project_models_selected(group_by, selected);

        let mut agents: Vec<AgentEntry> = self
            .agent_map
            .iter()
            .filter_map(|(key, agent)| {
                if !client_is_selected(key.client, selected) {
                    return None;
                }
                Some(AgentEntry {
                    agent: key.agent.to_string(),
                    client: key.client,
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

        let mut daily: Vec<DailyUsage> = self
            .daily_map
            .values()
            .filter_map(|bucket| materialize_daily_usage(bucket, group_by, selected))
            .collect();
        daily.sort_by_key(|usage| std::cmp::Reverse(usage.date));

        let mut hourly: Vec<HourlyUsage> = self
            .hourly_map
            .values()
            .filter_map(|bucket| materialize_hourly_usage(bucket, group_by, selected))
            .collect();
        hourly.sort_by_key(|usage| std::cmp::Reverse(usage.datetime));

        let graph = build_contribution_graph_for_today(&daily, effective_date);
        let (current_streak, longest_streak) = calculate_streaks_for_today(&daily, effective_date);

        UsageProjection {
            models,
            agents,
            daily,
            hourly,
            graph,
            total_tokens,
            total_cost,
            current_streak,
            longest_streak,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    use chrono::NaiveDate;

    use super::*;
    use crate::aggregate::keys::UNKNOWN_WORKSPACE_LABEL;
    use crate::records::AttributedUsageRecord;

    struct TuiUsageHarness;

    fn projection_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
    }

    impl TuiUsageHarness {
        fn aggregate_messages(
            &self,
            messages: Vec<AttributedUsageRecord>,
            group_by: &GroupBy,
        ) -> Result<UsageProjection, String> {
            let mut acc = UsageIndexBuilder::new();
            for message in &messages {
                acc.push(message);
            }
            let acc = acc.finish();
            Ok(acc.project_usage(group_by, projection_date()))
        }
    }

    fn make_workspace_message(
        client: ClientId,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
    ) -> AttributedUsageRecord {
        let mut msg = AttributedUsageRecord::new(
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
        client: ClientId,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
    ) -> AttributedUsageRecord {
        AttributedUsageRecord::new(
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

    fn daily_usage(date: NaiveDate, tokens: u64, cost: f64) -> DailyUsage {
        DailyUsage {
            date,
            tokens: UsageTokenBreakdown {
                input: tokens,
                ..UsageTokenBreakdown::default()
            },
            cost,
            client_breakdown: BTreeMap::new(),
            message_count: 1,
            turn_count: 1,
        }
    }

    fn contribution_day(graph: &UsageGraphData, date: NaiveDate) -> &ContributionDay {
        graph
            .weeks
            .iter()
            .flatten()
            .flatten()
            .find(|day| day.date == date)
            .expect("graph must contain the requested visible date")
    }

    #[test]
    fn projection_uses_the_explicit_effective_date() {
        let activity_date = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let tokens = UsageTokenBreakdown {
            input: 10,
            ..UsageTokenBreakdown::default()
        };
        let mut index = FrozenUsageIndex::new();
        index.daily_map.insert(
            activity_date,
            DailyBucket {
                date: activity_date,
                clients: HashMap::from([(
                    ClientId::Amp,
                    DailyClientBucket {
                        tokens,
                        cost: 0.0,
                        message_count: 1,
                        turn_count: 1,
                        models: HashMap::new(),
                    },
                )]),
            },
        );

        let on_activity_date = index.project_usage(&GroupBy::Model, activity_date);
        let two_days_later = index.project_usage(
            &GroupBy::Model,
            NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        );

        assert_eq!(on_activity_date.current_streak, 1);
        assert_eq!(two_days_later.current_streak, 0);
        let activity_cell = on_activity_date
            .graph
            .weeks
            .last()
            .unwrap()
            .last()
            .unwrap()
            .as_ref()
            .unwrap();
        assert_eq!(activity_cell.date, activity_date);
        assert_eq!(activity_cell.tokens, 10);
        assert_eq!(activity_cell.cost, 0.0);
        assert_eq!(activity_cell.grade, ContributionGrade::Peak);
        assert_eq!(
            two_days_later
                .graph
                .weeks
                .last()
                .unwrap()
                .last()
                .unwrap()
                .as_ref()
                .unwrap()
                .date,
            NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
        );
    }

    #[test]
    fn contribution_graph_colors_unpriced_activity_by_tokens() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let lower_date = today.pred_opt().unwrap();
        let daily = [
            daily_usage(lower_date, 25, 0.0),
            daily_usage(today, 100, 0.0),
        ];

        let graph = build_contribution_graph_for_today(&daily, today);
        let lower = contribution_day(&graph, lower_date);
        let peak = contribution_day(&graph, today);

        assert_eq!(lower.tokens, 25);
        assert_eq!(lower.cost, 0.0);
        assert!(lower.grade > ContributionGrade::Empty);
        assert_eq!(peak.tokens, 100);
        assert_eq!(peak.cost, 0.0);
        assert_eq!(peak.grade, ContributionGrade::Peak);
    }

    #[test]
    fn contribution_graph_ignores_off_window_history_when_assigning_grades() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let days_to_sunday = today.weekday().num_days_from_sunday();
        let start_date = today - chrono::Duration::days(364 + days_to_sunday as i64);
        let off_window_date = start_date.pred_opt().unwrap();
        let visible_daily = [
            daily_usage(today - chrono::Duration::days(2), 10, 0.1),
            daily_usage(today - chrono::Duration::days(1), 100, 1.0),
            daily_usage(today, 1_000, 10.0),
        ];
        let mut daily = visible_daily.to_vec();
        daily.push(daily_usage(off_window_date, 1_000_000_000, 1_000_000.0));

        let baseline = build_contribution_graph_for_today(&visible_daily, today);
        let graph = build_contribution_graph_for_today(&daily, today);
        for usage in &visible_daily {
            assert_eq!(
                contribution_day(&graph, usage.date).grade,
                contribution_day(&baseline, usage.date).grade
            );
        }
        assert!(graph
            .weeks
            .iter()
            .flatten()
            .flatten()
            .all(|day| day.date != off_window_date));
    }

    #[test]
    fn contribution_graph_cost_does_not_influence_equal_token_grades() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let expensive_date = today.pred_opt().unwrap();
        let free_date = expensive_date.pred_opt().unwrap();
        let daily = [
            daily_usage(free_date.pred_opt().unwrap(), 25, 0.01),
            daily_usage(free_date, 50, 0.0),
            daily_usage(expensive_date, 50, 10_000.0),
            daily_usage(today, 100, 1.0),
        ];

        let graph = build_contribution_graph_for_today(&daily, today);
        let expensive = contribution_day(&graph, expensive_date);
        let free = contribution_day(&graph, free_date);

        assert_eq!(expensive.tokens, free.tokens);
        assert_eq!(expensive.cost, 10_000.0);
        assert_eq!(free.cost, 0.0);
        assert_eq!(expensive.grade, ContributionGrade::High);
        assert_eq!(free.grade, ContributionGrade::High);
    }

    #[test]
    fn contribution_graph_log_mad_assigns_all_four_active_grades() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let tokens = [1, 2, 4, 16, 256, 1_024, 8_192, 131_072];
        let expected_grades = [
            ContributionGrade::Low,
            ContributionGrade::Low,
            ContributionGrade::Medium,
            ContributionGrade::Medium,
            ContributionGrade::High,
            ContributionGrade::High,
            ContributionGrade::Peak,
            ContributionGrade::Peak,
        ];
        let daily: Vec<DailyUsage> = tokens
            .iter()
            .enumerate()
            .map(|(index, token_total)| {
                daily_usage(
                    today - chrono::Duration::days((tokens.len() - 1 - index) as i64),
                    *token_total,
                    0.0,
                )
            })
            .collect();

        let graph = build_contribution_graph_for_today(&daily, today);
        for (usage, expected) in daily.iter().zip(expected_grades) {
            assert_eq!(contribution_day(&graph, usage.date).grade, expected);
        }
    }

    #[test]
    fn contribution_graph_log_mad_resists_one_large_visible_outlier() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let tokens = [1, 2, 4, 8, 16, 32, 64, 1_u64 << 60];
        let expected_grades = [
            ContributionGrade::Low,
            ContributionGrade::Low,
            ContributionGrade::Medium,
            ContributionGrade::Medium,
            ContributionGrade::High,
            ContributionGrade::High,
            ContributionGrade::Peak,
            ContributionGrade::Peak,
        ];
        let daily: Vec<DailyUsage> = tokens
            .iter()
            .enumerate()
            .map(|(index, token_total)| {
                daily_usage(
                    today - chrono::Duration::days((tokens.len() - 1 - index) as i64),
                    *token_total,
                    0.0,
                )
            })
            .collect();

        let graph = build_contribution_graph_for_today(&daily, today);
        for (usage, expected) in daily.iter().zip(expected_grades) {
            assert_eq!(contribution_day(&graph, usage.date).grade, expected);
        }
    }

    #[test]
    fn contribution_graph_handles_zero_mad_degeneracies() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let one_day = [daily_usage(today, 7, 0.0)];
        let one_day_graph = build_contribution_graph_for_today(&one_day, today);
        assert_eq!(
            contribution_day(&one_day_graph, today).grade,
            ContributionGrade::Peak
        );

        let all_equal = [
            daily_usage(today - chrono::Duration::days(2), 42, 1.0),
            daily_usage(today - chrono::Duration::days(1), 42, 2.0),
            daily_usage(today, 42, 3.0),
        ];
        let all_equal_graph = build_contribution_graph_for_today(&all_equal, today);
        assert!(all_equal.iter().all(|usage| {
            contribution_day(&all_equal_graph, usage.date).grade == ContributionGrade::Peak
        }));

        let fallback_daily = [
            daily_usage(today - chrono::Duration::days(3), 8, 0.0),
            daily_usage(today - chrono::Duration::days(2), 8, 1.0),
            daily_usage(today - chrono::Duration::days(1), 8, 2.0),
            daily_usage(today, 64, 3.0),
        ];
        let fallback_graph = build_contribution_graph_for_today(&fallback_daily, today);
        for usage in &fallback_daily[..3] {
            assert_eq!(
                contribution_day(&fallback_graph, usage.date).grade,
                ContributionGrade::High
            );
        }
        assert_eq!(
            contribution_day(&fallback_graph, today).grade,
            ContributionGrade::Peak
        );
    }

    #[test]
    fn contribution_graph_reserves_grade_zero_for_zero_tokens() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let zero_date = today.pred_opt().unwrap();
        let daily = [
            daily_usage(zero_date, 0, 1_000_000.0),
            daily_usage(today, 1, 0.0),
        ];

        let graph = build_contribution_graph_for_today(&daily, today);
        let zero = contribution_day(&graph, zero_date);
        let active = contribution_day(&graph, today);

        assert_eq!(zero.tokens, 0);
        assert_eq!(zero.cost, 1_000_000.0);
        assert_eq!(zero.grade, ContributionGrade::Empty);
        assert_eq!(active.tokens, 1);
        assert_eq!(active.cost, 0.0);
        assert_eq!(active.grade, ContributionGrade::Peak);
    }

    #[test]
    fn contribution_graph_retains_visible_token_and_cost_fields() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let daily = [daily_usage(today, 100, 1.25)];

        let graph = build_contribution_graph_for_today(&daily, today);
        let visible = contribution_day(&graph, today);

        assert_eq!(visible.tokens, 100);
        assert_eq!(visible.cost, 1.25);
    }

    #[test]
    fn test_aggregate_messages_model_grouping_uses_finalized_provider_ids() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        ClientId::OpenCode,
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        ClientId::OpenCode,
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
        assert_eq!(usage.models[0].model_id, "mimo-v2.5-pro");
        assert_eq!(usage.models[0].display_name, "mimo-v2.5-pro");
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
                        ClientId::OpenCode,
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        ClientId::OpenCode,
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

        let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
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
                    ClientId::OpenCode,
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
        assert_eq!(usage.models[0].model_id, "gpt-5.5");
        assert_eq!(usage.models[0].display_name, "gpt-5.5");
        assert_eq!(usage.models[0].provider, "openai");

        let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
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
                        ClientId::OpenCode,
                        "gpt-5.5",
                        "openai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        ClientId::OpenCode,
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

        let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
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
                        ClientId::Claude,
                        "kimi-for-coding",
                        "kimi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::OpenCode,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::RooCode,
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
            .find(|agent| agent.client == ClientId::OpenCode)
            .unwrap();
        assert_eq!(opencode.agent, "Builder");
        assert_eq!(opencode.message_count, 1);
        assert!((opencode.cost - 1.25).abs() < f64::EPSILON);
        assert_eq!(opencode.tokens.total(), 15);

        let roocode = usage
            .agents
            .iter()
            .find(|agent| agent.client == ClientId::RooCode)
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
                        ClientId::OpenCode,
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
                        ClientId::Codex,
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
                        ClientId::Pi,
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
        assert_eq!(
            usage.models[0].clients,
            [ClientId::Pi, ClientId::Codex, ClientId::OpenCode]
        );
    }

    #[test]
    fn test_aggregate_messages_groups_by_workspace_and_model() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        ClientId::Claude,
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.25,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        ClientId::Qwen,
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
        assert_eq!(usage.models[0].model_id, "claude-sonnet-4.5");
        assert_eq!(usage.models[0].display_name, "claude-sonnet-4.5");
        assert_eq!(usage.models[0].clients, [ClientId::Claude, ClientId::Qwen]);
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
                        ClientId::Claude,
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
                        ClientId::Claude,
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("unknown-workspace"),
                        Some("unknown-workspace"),
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
                        ClientId::Claude,
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
        let claude = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Claude)
            .unwrap();
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);

        // The workspace dimension travels in structured fields; display_name
        // and model_id stay the bare canonical model (ADR 0010).
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
                        ClientId::Claude,
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/srv/team-a/demo"),
                        Some("demo"),
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
        let claude = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Claude)
            .unwrap();
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
                        ClientId::Claude,
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

            let models = &usage.daily[0].client_breakdown[&ClientId::Claude].models;
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
                        ClientId::Claude,
                        "c",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("a:b"),
                        Some("workspace-ab"),
                    ),
                    make_workspace_message(
                        ClientId::Claude,
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
                && model.model_id == "c"
                && (model.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("a")
                && model.model_id == "b:c"
                && (model.cost - 2.0).abs() < f64::EPSILON
        }));

        let claude = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Claude)
            .unwrap();
        assert_eq!(claude.models.len(), 2);
    }

    #[test]
    fn test_aggregate_messages_client_provider_model_splits_providers_in_daily_breakdown() {
        let loader = TuiUsageHarness;
        let usage = loader
            .aggregate_messages(
                vec![
                    AttributedUsageRecord::new(
                        ClientId::Claude,
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
                    AttributedUsageRecord::new(
                        ClientId::Claude,
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
        let claude = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Claude)
            .unwrap();
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
                    AttributedUsageRecord::new(
                        ClientId::Claude,
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
                    AttributedUsageRecord::new(
                        ClientId::Gemini,
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

        let claude = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Claude)
            .unwrap();
        assert_eq!(claude.cost, 1.0);
        assert_eq!(claude.models.len(), 1);
        let claude_model = claude.models.get("v1|m|17:claude-sonnet-4.5").unwrap();
        assert_eq!(claude_model.display_name, "claude-sonnet-4.5");
        assert_eq!(claude_model.tokens.total(), 15);

        let gemini = usage.daily[0]
            .client_breakdown
            .get(&ClientId::Gemini)
            .unwrap();
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
            AttributedUsageRecord::new_with_agent(
                ClientId::OpenCode,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::OpenCode,
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
            agent.agent == "Sisyphus"
                && agent.client == ClientId::OpenCode
                && agent.message_count == 1
        }));
        assert!(usage.agents.iter().any(|agent| {
            agent.agent == "Sisyphus (Ultraworker)"
                && agent.client == ClientId::OpenCode
                && agent.message_count == 1
        }));
    }

    #[test]
    fn test_aggregate_messages_does_not_normalize_opencode_agent_case() {
        let loader = TuiUsageHarness;
        let messages = vec![
            AttributedUsageRecord::new_with_agent(
                ClientId::OpenCode,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::OpenCode,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::Claude,
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
            AttributedUsageRecord::new_with_agent(
                ClientId::Claude,
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
        client: ClientId,
        provider: &str,
        session: &str,
        model: &str,
        input: i64,
        timestamp: i64,
    ) -> AttributedUsageRecord {
        AttributedUsageRecord::new(
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
    fn top_level_models_preserve_structured_buckets_with_colliding_display_text() {
        let timestamp = 1_735_689_600_000;
        let cases = [
            (
                GroupBy::ClientModel,
                collision_message(ClientId::Codex, "first", "same", "c", 10, timestamp),
                collision_message(ClientId::Amp, "second", "same", "b:c", 20, timestamp),
            ),
            (
                GroupBy::ClientProviderModel,
                collision_message(ClientId::Amp, "b:c", "same", "d", 10, timestamp),
                collision_message(ClientId::Amp, "b", "same", "c:d", 20, timestamp),
            ),
        ];

        for (group_by, first, second) in cases {
            let mut acc = UsageIndexBuilder::new();
            acc.push(&first);
            acc.push(&second);
            let acc = acc.finish();
            let usage = acc.project_usage(&group_by, projection_date());
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
        let first = collision_message(ClientId::Amp, "b:c", "same", "d", 10, timestamp);
        let second = collision_message(ClientId::Amp, "b", "same", "c:d", 20, timestamp);
        let mut acc = UsageIndexBuilder::new();
        acc.push(&first);
        acc.push(&second);
        let acc = acc.finish();
        let usage = acc.project_usage(&GroupBy::ClientProviderModel, projection_date());

        let daily = &usage.daily[0].client_breakdown[&ClientId::Amp].models;
        assert_eq!(daily.len(), 2);
        let first_daily = &daily["v1|cpm|3:amp3:b:c1:d"];
        assert_eq!(first_daily.provider, "b:c");
        assert_eq!(first_daily.model_id, "d");
        assert_eq!(first_daily.display_name, "d");
        assert_eq!(first_daily.tokens.total(), 10);
        assert_eq!(first_daily.cost, 10.0);
        assert_eq!(first_daily.messages, 1);
        let second_daily = &daily["v1|cpm|3:amp1:b3:c:d"];
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
        let mut acc = UsageIndexBuilder::new();
        acc.push(&collision_message(
            ClientId::Zed,
            "provider",
            "session-z",
            "model",
            10,
            timestamp,
        ));
        acc.push(&collision_message(
            ClientId::Amp,
            "provider",
            "session-a",
            "model",
            20,
            timestamp,
        ));

        let acc = acc.finish();
        let usage = acc.project_usage(&GroupBy::Model, projection_date());

        assert_eq!(
            usage.hourly[0]
                .clients
                .iter()
                .map(|client| client.as_str())
                .collect::<Vec<_>>(),
            ["amp", "zed"]
        );
    }

    #[test]
    fn workspace_maps_tag_unknown_and_known_keys_separately() {
        let timestamp = 1_735_689_600_000;
        let mut unknown = collision_message(
            ClientId::Codex,
            "provider",
            "session",
            "model",
            10,
            timestamp,
        );
        unknown.workspace_key = None;
        unknown.workspace_label = None;
        let mut known = collision_message(
            ClientId::Codex,
            "provider",
            "session",
            "model",
            20,
            timestamp,
        );
        known.workspace_key = Some(Arc::from(""));
        known.workspace_label = Some(Arc::from("Empty workspace key"));

        let mut acc = UsageIndexBuilder::new();
        acc.push(&unknown);
        acc.push(&known);
        let acc = acc.finish();
        let usage = acc.project_usage(&GroupBy::WorkspaceModel, projection_date());

        assert_eq!(usage.models.len(), 2);
        let daily_models = &usage.daily[0].client_breakdown[&ClientId::Codex].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("v1|wmu|5:model"));
        assert!(daily_models.contains_key("v1|wmk|0:5:model"));
    }

    #[test]
    fn structured_session_and_agent_instance_identities_do_not_alias_delimiters() {
        let timestamp = 1_735_689_600_000;
        let model_messages = [
            collision_message(ClientId::Codex, "provider", "c", "model", 10, timestamp),
            collision_message(ClientId::Amp, "provider", "b:c", "model", 20, timestamp),
        ];
        let mut model_acc = UsageIndexBuilder::new();
        for message in &model_messages {
            model_acc.push(message);
        }
        let model_acc = model_acc.finish();
        assert_eq!(
            model_acc
                .project_usage(&GroupBy::Model, projection_date())
                .models[0]
                .session_count,
            2
        );

        let mut explicit = AttributedUsageRecord::new_with_agent(
            ClientId::Amp,
            "model",
            "provider",
            "session",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        explicit.set_agent_instance(Some("a:b:c".to_string()));
        let derived_left = AttributedUsageRecord::new_with_agent(
            ClientId::Amp,
            "model",
            "provider",
            "c",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        let derived_right = AttributedUsageRecord::new_with_agent(
            ClientId::Amp,
            "model",
            "provider",
            "b:c",
            timestamp,
            crate::TokenBreakdown::default(),
            0.0,
            Some("builder".to_string()),
        );
        let mut agent_acc = UsageIndexBuilder::new();
        for message in [&explicit, &derived_left, &derived_right] {
            agent_acc.push(message);
        }
        let agent_acc = agent_acc.finish();
        assert_eq!(
            agent_acc
                .project_usage(&GroupBy::Model, projection_date())
                .agents[0]
                .instance_count,
            3
        );
    }

    // ---- group-by re-projection (issue #161) ----

    #[allow(clippy::too_many_arguments)]
    fn reprojection_message(
        client: ClientId,
        model: &str,
        provider: &str,
        session: &str,
        timestamp: i64,
        input: i64,
        output: i64,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
        is_turn_start: bool,
        agent: Option<&str>,
    ) -> AttributedUsageRecord {
        let mut msg = AttributedUsageRecord::new_with_agent(
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
    /// per client, two days, two hours, turn starts, and an agent.
    fn reprojection_corpus() -> Vec<AttributedUsageRecord> {
        vec![
            reprojection_message(
                ClientId::Claude,
                "gpt-5.5",
                "openai",
                "s1",
                reprojection_ts(0, 12),
                100,
                50,
                0.1,
                Some("/repo-a"),
                Some("repo-a"),
                true,
                Some("builder"),
            ),
            reprojection_message(
                ClientId::Codex,
                "gpt-5.5",
                "openai",
                "s2",
                reprojection_ts(0, 12),
                200,
                60,
                0.2,
                Some("/repo-a"),
                Some("repo-a"),
                false,
                None,
            ),
            reprojection_message(
                ClientId::Claude,
                "gpt-5.5",
                "azure",
                "s1",
                reprojection_ts(0, 13),
                300,
                70,
                0.3,
                Some("/repo-b"),
                None,
                false,
                None,
            ),
            reprojection_message(
                ClientId::Claude,
                "claude-sonnet-4.5",
                "anthropic",
                "s3",
                reprojection_ts(1, 12),
                400,
                80,
                0.4,
                None,
                None,
                true,
                None,
            ),
            reprojection_message(
                ClientId::Qwen,
                "gpt-5.5",
                "openai",
                "s4",
                reprojection_ts(1, 13),
                500,
                90,
                0.5,
                Some("/repo-a"),
                Some("repo-a"),
                false,
                None,
            ),
        ]
    }

    fn reprojection_index() -> FrozenUsageIndex {
        let mut acc = UsageIndexBuilder::new();
        for message in reprojection_corpus() {
            acc.push(&message);
        }
        acc.finish()
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
                        assert_eq!(left.grade, right.grade);
                    }
                    (None, None) => {}
                    _ => panic!("graph day presence mismatch"),
                }
            }
        }
    }

    fn assert_usage_data_eq(left: &UsageProjection, right: &UsageProjection) {
        assert_eq!(left.total_tokens, right.total_tokens);
        assert_eq!(left.total_cost.to_bits(), right.total_cost.to_bits());
        assert_eq!(left.current_streak, right.current_streak);
        assert_eq!(left.longest_streak, right.longest_streak);

        assert_eq!(left.models.len(), right.models.len());
        for (left, right) in left.models.iter().zip(&right.models) {
            assert_eq!(left.model_id, right.model_id);
            assert_eq!(left.display_name, right.display_name);
            assert_eq!(left.provider, right.provider);
            assert_eq!(left.clients, right.clients);
            assert_eq!(left.workspace_key, right.workspace_key);
            assert_eq!(left.workspace_label, right.workspace_label);
            assert_tokens_eq(&left.tokens, &right.tokens);
            assert_eq!(left.cost.to_bits(), right.cost.to_bits());
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
        let acc = reprojection_index();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let first = acc.project_usage(&group_by, projection_date());
            let second = acc.project_usage(&group_by, projection_date());
            assert_usage_data_eq(&first, &second);
        }
    }

    #[test]
    fn totals_use_canonical_full_and_client_scoped_folds() {
        let acc = reprojection_index();

        let full = acc.project_usage(&GroupBy::Model, projection_date());
        assert_eq!(full.total_tokens, 1_850);
        assert_eq!(full.total_cost.to_bits(), 1.5_f64.to_bits());

        let all_selected = acc.project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Claude, ClientId::Codex, ClientId::Qwen]),
            projection_date(),
        );
        assert_eq!(all_selected.total_tokens, full.total_tokens);
        assert_eq!(all_selected.total_cost.to_bits(), full.total_cost.to_bits());

        let claude_and_codex = acc.project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Claude, ClientId::Codex]),
            projection_date(),
        );
        assert_eq!(claude_and_codex.total_tokens, 1_260);
        assert_eq!(claude_and_codex.total_cost.to_bits(), 1.0_f64.to_bits());

        let qwen = acc.project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Qwen]),
            projection_date(),
        );
        assert_eq!(qwen.total_tokens, 590);
        assert_eq!(qwen.total_cost.to_bits(), 0.5_f64.to_bits());
    }

    #[test]
    fn model_only_projection_matches_complete_projection_models_and_totals() {
        let acc = reprojection_index();
        let selected = HashSet::from([ClientId::Claude, ClientId::Codex]);

        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let complete = acc.project_usage(&group_by, projection_date());
            let models = acc.project_models(&group_by);

            assert_eq!(models.models, complete.models);
            assert_eq!(models.total_tokens, complete.total_tokens);
            assert_eq!(models.total_cost.to_bits(), complete.total_cost.to_bits());

            let complete = acc.project_usage_for_clients(&group_by, &selected, projection_date());
            let models = acc.project_models_for_clients(&group_by, &selected);

            assert_eq!(models.models, complete.models);
            assert_eq!(models.total_tokens, complete.total_tokens);
            assert_eq!(models.total_cost.to_bits(), complete.total_cost.to_bits());
        }
    }

    #[test]
    fn frozen_index_round_trip_preserves_full_and_subset_totals() {
        let acc = reprojection_index();
        let encoded = serde_json::to_vec(&acc).expect("serialize frozen usage index");
        let restored: FrozenUsageIndex =
            serde_json::from_slice(&encoded).expect("deserialize frozen usage index");

        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            assert_usage_data_eq(
                &acc.project_usage(&group_by, projection_date()),
                &restored.project_usage(&group_by, projection_date()),
            );
        }

        let selected = HashSet::from([ClientId::Claude, ClientId::Codex]);
        assert_usage_data_eq(
            &acc.project_usage_for_clients(&GroupBy::Model, &selected, projection_date()),
            &restored.project_usage_for_clients(&GroupBy::Model, &selected, projection_date()),
        );
    }

    #[test]
    fn frozen_index_serialization_excludes_builder_sequence_state() {
        let mut builder = UsageIndexBuilder::new();
        builder.push(&reprojection_corpus()[0]);
        assert_eq!(builder.next_sequence, 1);

        let encoded = serde_json::to_value(builder.finish()).expect("serialize frozen usage index");
        let object = encoded.as_object().expect("usage index must be an object");
        assert!(!object.contains_key("next_sequence"));
        assert!(!object.contains_key("nextSequence"));
    }

    #[test]
    fn frozen_index_validation_accepts_coherent_materialized_totals() {
        let index = reprojection_index();
        let universe =
            ClientUniverse::new([ClientId::Claude, ClientId::Codex, ClientId::Qwen]).unwrap();

        assert_eq!(index.validate(&universe), Ok(()));
    }

    #[test]
    fn frozen_index_validation_rejects_an_indexed_client_outside_the_universe() {
        let index = reprojection_index();
        let universe = ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap();

        assert_eq!(
            index.validate(&universe),
            Err(UsageIndexValidationError::IndexedClientOutsideUniverse {
                index: "usage_totals_by_client",
                client: ClientId::Qwen,
            })
        );
    }

    #[test]
    fn frozen_index_validation_rejects_disagreeing_model_totals() {
        let mut index = reprojection_index();
        index
            .model_map
            .values_mut()
            .next()
            .expect("corpus has a model bucket")
            .tokens
            .input += 1;
        let universe =
            ClientUniverse::new([ClientId::Claude, ClientId::Codex, ClientId::Qwen]).unwrap();

        assert!(matches!(
            index.validate(&universe),
            Err(UsageIndexValidationError::TokenTotalsMismatch {
                index: "model_map",
                ..
            })
        ));
    }

    #[test]
    fn independently_built_indexes_project_identically_across_hash_seeds() {
        // Separate accumulators create independently seeded HashMaps.
        let first = reprojection_index();
        let second = reprojection_index();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            assert_usage_data_eq(
                &first.project_usage(&group_by, projection_date()),
                &second.project_usage(&group_by, projection_date()),
            );
        }
    }

    #[test]
    fn client_projection_matches_a_fresh_fold_of_only_the_selected_clients() {
        let corpus = reprojection_corpus();
        let full = reprojection_index();
        for selected in [
            HashSet::from([ClientId::Claude]),
            HashSet::from([ClientId::Codex]),
            HashSet::from([ClientId::Claude, ClientId::Codex]),
        ] {
            let mut expected = UsageIndexBuilder::new();
            for message in &corpus {
                if selected.contains(&message.client) {
                    expected.push(message);
                }
            }
            let expected = expected.finish();
            for group_by in [
                GroupBy::Model,
                GroupBy::ClientModel,
                GroupBy::ClientProviderModel,
                GroupBy::WorkspaceModel,
            ] {
                assert_usage_data_eq(
                    &full.project_usage_for_clients(&group_by, &selected, projection_date()),
                    &expected.project_usage(&group_by, projection_date()),
                );
            }
        }
    }

    #[test]
    fn reprojection_switching_groupings_does_not_pollute_state() {
        let acc = reprojection_index();
        let baseline = acc.project_usage(&GroupBy::Model, projection_date());
        let _ = acc.project_usage(&GroupBy::ClientProviderModel, projection_date());
        let _ = acc.project_usage(&GroupBy::WorkspaceModel, projection_date());
        let rerun = acc.project_usage(&GroupBy::Model, projection_date());
        assert_usage_data_eq(&baseline, &rerun);
    }

    #[test]
    fn reprojection_preserves_group_independent_views() {
        let acc = reprojection_index();
        let reference = acc.project_usage(&GroupBy::Model, projection_date());
        for group_by in [
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let projected = acc.project_usage(&group_by, projection_date());
            assert_eq!(projected.total_tokens, reference.total_tokens);
            assert!((projected.total_cost - reference.total_cost).abs() < 1e-9);
            assert_agents_eq(&projected.agents, &reference.agents);
            assert_eq!(
                (projected.current_streak, projected.longest_streak),
                (reference.current_streak, reference.longest_streak)
            );

            // Day- and hour-level rollups are accumulated per message, not
            // re-folded, so they stay bit-identical across groupings.
            let daily_rollup = |data: &UsageProjection| {
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
            let hourly_rollup = |data: &UsageProjection| {
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
    fn reprojection_derives_each_grouping_from_one_frozen_index() {
        let corpus = reprojection_corpus();
        let day0 = corpus[0].local_date().expect("day0 local date");
        let day1 = corpus[3].local_date().expect("day1 local date");
        let day0_hour12 = timestamp_to_hour(corpus[0].timestamp).expect("day0 hour12");
        let day0_hour13 = timestamp_to_hour(corpus[2].timestamp).expect("day0 hour13");
        let mut acc = UsageIndexBuilder::new();
        for message in &corpus {
            acc.push(message);
        }
        let acc = acc.finish();

        let model = acc.project_usage(&GroupBy::Model, projection_date());
        assert_eq!(model.models.len(), 2);
        let gpt = model
            .models
            .iter()
            .find(|entry| entry.model_id == "gpt-5.5")
            .expect("merged gpt-5.5 entry");
        assert_eq!(gpt.provider, "azure, openai");
        assert_eq!(
            gpt.clients,
            [ClientId::Qwen, ClientId::Claude, ClientId::Codex]
        );
        assert_eq!(gpt.session_count, 3);
        assert_eq!(gpt.tokens.total(), 1370);
        assert!((gpt.cost - 1.1).abs() < 1e-9);
        assert_eq!(gpt.workspace_key, None);
        assert_eq!(gpt.workspace_label, None);
        let sonnet = model
            .models
            .iter()
            .find(|entry| entry.model_id == "claude-sonnet-4.5")
            .expect("claude-sonnet-4.5 entry");
        assert_eq!(sonnet.session_count, 1);

        // Model grouping merges providers in the daily detail, attributing
        // the first-seen provider, and keeps bare-model hourly keys.
        let day0_claude = &model
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown[&ClientId::Claude];
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

        let client_model = acc.project_usage(&GroupBy::ClientModel, projection_date());
        assert_eq!(client_model.models.len(), 4);
        let claude_gpt = client_model
            .models
            .iter()
            .find(|entry| {
                entry.model_id == "gpt-5.5" && entry.clients.as_slice() == [ClientId::Claude]
            })
            .expect("claude gpt-5.5 entry");
        assert_eq!(claude_gpt.provider, "azure, openai");
        assert_eq!(claude_gpt.tokens.total(), 520);
        assert_eq!(claude_gpt.session_count, 1);
        let day0_claude = &client_model
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown[&ClientId::Claude];
        assert!(day0_claude.models.contains_key("v1|cm|6:claude7:gpt-5.5"));

        let cpm = acc.project_usage(&GroupBy::ClientProviderModel, projection_date());
        assert_eq!(cpm.models.len(), 5);
        let day0_claude = &cpm
            .daily
            .iter()
            .find(|day| day.date == day0)
            .expect("day0 usage")
            .client_breakdown[&ClientId::Claude];
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

        let workspace = acc.project_usage(&GroupBy::WorkspaceModel, projection_date());
        assert_eq!(workspace.models.len(), 3);
        let repo_a = workspace
            .models
            .iter()
            .find(|entry| entry.workspace_key.as_deref() == Some("/repo-a"))
            .expect("repo-a workspace entry");
        assert_eq!(repo_a.workspace_label.as_deref(), Some("repo-a"));
        assert_eq!(
            repo_a.clients,
            [ClientId::Qwen, ClientId::Codex, ClientId::Claude]
        );
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
            .client_breakdown[&ClientId::Claude];
        assert!(day0_claude.models.contains_key("v1|wmk|7:/repo-a7:gpt-5.5"));
        assert!(day0_claude.models.contains_key("v1|wmk|7:/repo-b7:gpt-5.5"));
        let day1_claude = &workspace
            .daily
            .iter()
            .find(|day| day.date == day1)
            .expect("day1 usage")
            .client_breakdown[&ClientId::Claude];
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
