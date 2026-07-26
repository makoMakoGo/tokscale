use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use chrono::{Datelike, Days, NaiveDate, Timelike, Weekday};

use crate::aggregate::UsageProjectionError;
use crate::projection::{
    ContributionDay, ContributionGrade, DailyClientInfo, DailyModelInfo, DailyUsage, HourlyUsage,
    PeriodKind, PeriodUsage, UsageGraphData, UsageProjection, UsageTokenBreakdown,
};
use crate::{ClientId, GroupBy};

// ---- period (monthly/weekly) view: folds the finished `daily` buckets ----

struct PeriodDescriptor {
    section_year: i32,
    ordinal: u32,
    label: String,
    short_label: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
}

pub(super) fn try_add_projection_tokens(
    target: &mut UsageTokenBreakdown,
    addition: &UsageTokenBreakdown,
    field: &'static str,
) -> Result<(), UsageProjectionError> {
    let updated = target
        .checked_add(addition)
        .ok_or_else(|| UsageProjectionError::new(field))?;
    updated
        .checked_total()
        .ok_or_else(|| UsageProjectionError::new(field))?;
    *target = updated;
    Ok(())
}

pub(super) fn try_add_projection_cost(
    target: &mut f64,
    addition: f64,
    field: &'static str,
) -> Result<(), UsageProjectionError> {
    let updated = *target + addition;
    if !updated.is_finite() {
        return Err(UsageProjectionError::new(field));
    }
    *target = updated;
    Ok(())
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum PeriodWorkspaceIdentity {
    Known(Arc<str>),
    Unknown,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum PeriodModelIdentity {
    Model(Arc<str>),
    ProviderModel {
        provider: Arc<str>,
        model: Arc<str>,
    },
    WorkspaceModel {
        workspace: PeriodWorkspaceIdentity,
        model: Arc<str>,
    },
}

impl PeriodModelIdentity {
    fn from_model(model: &DailyModelInfo, group_by: GroupBy) -> Self {
        match group_by {
            GroupBy::Model | GroupBy::ClientModel => Self::Model(Arc::clone(&model.model_id)),
            GroupBy::ClientProviderModel => Self::ProviderModel {
                provider: Arc::clone(&model.provider),
                model: Arc::clone(&model.model_id),
            },
            GroupBy::WorkspaceModel => Self::WorkspaceModel {
                workspace: model
                    .workspace_key
                    .as_ref()
                    .map_or(PeriodWorkspaceIdentity::Unknown, |workspace| {
                        PeriodWorkspaceIdentity::Known(Arc::clone(workspace))
                    }),
                model: Arc::clone(&model.model_id),
            },
        }
    }
}

#[derive(Default)]
struct PeriodClientBucket {
    tokens: UsageTokenBreakdown,
    cost: f64,
    models: BTreeMap<PeriodModelIdentity, DailyModelInfo>,
}

impl PeriodClientBucket {
    fn merge(
        &mut self,
        client: &DailyClientInfo,
        group_by: GroupBy,
    ) -> Result<(), UsageProjectionError> {
        try_add_projection_tokens(
            &mut self.tokens,
            &client.tokens,
            "period client token totals",
        )?;
        try_add_projection_cost(&mut self.cost, client.cost, "period client cost")?;
        for model in &client.models {
            let target = self
                .models
                .entry(PeriodModelIdentity::from_model(model, group_by))
                .or_insert_with(|| DailyModelInfo {
                    provider: Arc::clone(&model.provider),
                    model_id: Arc::clone(&model.model_id),
                    display_name: Arc::clone(&model.display_name),
                    workspace_key: model.workspace_key.clone(),
                    workspace_label: model.workspace_label.clone(),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                });
            try_add_projection_tokens(
                &mut target.tokens,
                &model.tokens,
                "period model token totals",
            )?;
            try_add_projection_cost(&mut target.cost, model.cost, "period model cost")?;
            target.messages = target
                .messages
                .checked_add(model.messages)
                .ok_or_else(|| UsageProjectionError::new("period model message count"))?;
        }
        Ok(())
    }

    fn into_projection(self) -> DailyClientInfo {
        DailyClientInfo {
            tokens: self.tokens,
            cost: self.cost,
            models: self.models.into_values().collect(),
        }
    }
}

struct PeriodUsageBucket {
    section_year: i32,
    section_label: String,
    label: String,
    short_label: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
    tokens: UsageTokenBreakdown,
    cost: f64,
    clients: BTreeMap<ClientId, PeriodClientBucket>,
    message_count: u32,
    turn_count: u32,
    active_days: u32,
}

impl PeriodUsageBucket {
    fn new(period: PeriodDescriptor) -> Self {
        Self {
            section_year: period.section_year,
            section_label: period.section_year.to_string(),
            label: period.label,
            short_label: period.short_label,
            start_date: period.start_date,
            end_date: period.end_date,
            tokens: UsageTokenBreakdown::default(),
            cost: 0.0,
            clients: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
            active_days: 0,
        }
    }

    fn merge(&mut self, day: &DailyUsage, group_by: GroupBy) -> Result<(), UsageProjectionError> {
        try_add_projection_tokens(&mut self.tokens, &day.tokens, "period token totals")?;
        try_add_projection_cost(&mut self.cost, day.cost, "period cost")?;
        self.message_count = self
            .message_count
            .checked_add(day.message_count)
            .ok_or_else(|| UsageProjectionError::new("period message count"))?;
        self.turn_count = self
            .turn_count
            .checked_add(day.turn_count)
            .ok_or_else(|| UsageProjectionError::new("period turn count"))?;
        let day_tokens = day
            .tokens
            .checked_total()
            .ok_or_else(|| UsageProjectionError::new("period active-day token total"))?;
        if day.message_count > 0 || day.turn_count > 0 || day_tokens > 0 {
            self.active_days = self
                .active_days
                .checked_add(1)
                .ok_or_else(|| UsageProjectionError::new("period active-day count"))?;
        }
        for (client_id, client) in &day.client_breakdown {
            self.clients
                .entry(*client_id)
                .or_default()
                .merge(client, group_by)?;
        }
        Ok(())
    }

    fn into_projection(self) -> PeriodUsage {
        PeriodUsage {
            section_year: self.section_year,
            section_label: self.section_label,
            label: self.label,
            short_label: self.short_label,
            start_date: self.start_date,
            end_date: self.end_date,
            tokens: self.tokens,
            cost: self.cost,
            client_breakdown: self
                .clients
                .into_iter()
                .map(|(client, bucket)| (client, bucket.into_projection()))
                .collect(),
            message_count: self.message_count,
            turn_count: self.turn_count,
            active_days: self.active_days,
        }
    }
}

fn period_descriptor(
    date: NaiveDate,
    kind: PeriodKind,
) -> Result<PeriodDescriptor, UsageProjectionError> {
    match kind {
        PeriodKind::Monthly => monthly_period_descriptor(date),
        PeriodKind::Weekly => weekly_period_descriptor(date),
    }
}

fn monthly_period_descriptor(date: NaiveDate) -> Result<PeriodDescriptor, UsageProjectionError> {
    let start_date = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
        .ok_or_else(|| UsageProjectionError::new("monthly period start date"))?;
    let end_date = if date.month() == 12 {
        let next_year = date
            .year()
            .checked_add(1)
            .ok_or_else(|| UsageProjectionError::new("monthly period year"))?;
        NaiveDate::from_ymd_opt(next_year, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    }
    .and_then(|next_month| next_month.checked_sub_days(Days::new(1)))
    .ok_or_else(|| UsageProjectionError::new("monthly period end date"))?;
    Ok(PeriodDescriptor {
        section_year: date.year(),
        ordinal: date.month(),
        label: start_date.format("%B").to_string(),
        short_label: start_date.format("%b").to_string(),
        start_date,
        end_date,
    })
}

fn weekly_period_descriptor(date: NaiveDate) -> Result<PeriodDescriptor, UsageProjectionError> {
    let iso = date.iso_week();
    let start_date = NaiveDate::from_isoywd_opt(iso.year(), iso.week(), Weekday::Mon)
        .ok_or_else(|| UsageProjectionError::new("weekly period start date"))?;
    let end_date = start_date
        .checked_add_days(Days::new(6))
        .ok_or_else(|| UsageProjectionError::new("weekly period end date"))?;
    let label = format!(
        "W{:02} {} - {}",
        iso.week(),
        start_date.format("%b %d"),
        end_date.format("%b %d")
    );
    Ok(PeriodDescriptor {
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
pub fn build_period_usage(
    usage: &UsageProjection,
    kind: PeriodKind,
) -> Result<Vec<PeriodUsage>, UsageProjectionError> {
    let mut period_map: BTreeMap<(i32, u32), PeriodUsageBucket> = BTreeMap::new();
    for day in &usage.daily {
        let period = period_descriptor(day.date, kind)?;
        let entry = period_map
            .entry((period.section_year, period.ordinal))
            .or_insert_with(|| PeriodUsageBucket::new(period));
        entry.merge(day, usage.group_by)?;
    }
    let mut periods: Vec<PeriodUsage> = period_map
        .into_values()
        .map(PeriodUsageBucket::into_projection)
        .collect();
    periods.sort_by_key(|period| std::cmp::Reverse(period.start_date));
    Ok(periods)
}

pub fn build_contribution_graph_for_today(
    daily: &[DailyUsage],
    today: NaiveDate,
) -> Result<UsageGraphData, UsageProjectionError> {
    let daily_with_totals = daily
        .iter()
        .map(|usage| {
            usage
                .tokens
                .checked_total()
                .ok_or_else(|| UsageProjectionError::new("contribution graph token total"))
                .map(|total| (usage, total))
        })
        .collect::<Result<Vec<_>, _>>()?;
    build_contribution_graph_for_today_by(
        &daily_with_totals,
        today,
        |(usage, _)| usage.date,
        |(_, total)| *total,
        |(usage, _)| usage.cost,
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
) -> Result<UsageGraphData, UsageProjectionError> {
    if daily.is_empty() {
        return Ok(UsageGraphData { weeks: vec![] });
    }
    let days_to_sunday = today.weekday().num_days_from_sunday();
    let end_date = today;
    let start_date = end_date
        .checked_sub_days(Days::new(364 + u64::from(days_to_sunday)))
        .ok_or_else(|| UsageProjectionError::new("contribution graph start date"))?;
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
        if current_date == end_date {
            break;
        }
        current_date = current_date
            .checked_add_days(Days::new(1))
            .ok_or_else(|| UsageProjectionError::new("contribution graph date traversal"))?;
    }
    Ok(UsageGraphData { weeks })
}

pub fn calculate_streaks_for_today(
    daily: &[DailyUsage],
    today: NaiveDate,
) -> Result<(u32, u32), UsageProjectionError> {
    calculate_streaks_for_today_by(daily, today, |usage| usage.date)
}

fn calculate_streaks_for_today_by<T>(
    daily: &[T],
    today: NaiveDate,
    date_of: impl Fn(&T) -> NaiveDate,
) -> Result<(u32, u32), UsageProjectionError> {
    if daily.is_empty() {
        return Ok((0, 0));
    }
    let dates: HashSet<NaiveDate> = daily.iter().map(date_of).collect();
    let mut current_streak = 0u32;
    let mut check_date = today;
    while dates.contains(&check_date) {
        current_streak = current_streak
            .checked_add(1)
            .ok_or_else(|| UsageProjectionError::new("current streak length"))?;
        check_date = check_date
            .checked_sub_days(Days::new(1))
            .ok_or_else(|| UsageProjectionError::new("current streak date traversal"))?;
    }
    if current_streak == 0 {
        let yesterday = today
            .checked_sub_days(Days::new(1))
            .ok_or_else(|| UsageProjectionError::new("current streak previous date"))?;
        check_date = yesterday;
        while dates.contains(&check_date) {
            current_streak = current_streak
                .checked_add(1)
                .ok_or_else(|| UsageProjectionError::new("current streak length"))?;
            check_date = check_date
                .checked_sub_days(Days::new(1))
                .ok_or_else(|| UsageProjectionError::new("current streak date traversal"))?;
        }
    }
    let mut longest_streak = 0u32;
    let mut sorted_dates: Vec<NaiveDate> = dates.into_iter().collect();
    sorted_dates.sort();
    let mut streak = 0u32;
    let mut prev_date: Option<NaiveDate> = None;
    for date in sorted_dates {
        if let Some(prev) = prev_date {
            if prev.checked_add_days(Days::new(1)) == Some(date) {
                streak = streak
                    .checked_add(1)
                    .ok_or_else(|| UsageProjectionError::new("longest streak length"))?;
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
    Ok((current_streak, longest_streak))
}

// ---- hourly profile helpers (time-of-day / weekday / peak) ----

/// Time-of-day period bucket for the profile view.
#[derive(Debug, Clone)]
pub struct PeriodBucket {
    pub label: &'static str,
    pub hour_range: &'static str,
    pub total_tokens: u64,
}

pub fn aggregate_by_period(
    hourly: &[HourlyUsage],
) -> Result<Vec<PeriodBucket>, UsageProjectionError> {
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
                    let entry_tokens = entry.tokens.checked_total().ok_or_else(|| {
                        UsageProjectionError::new("period-profile entry token total")
                    })?;
                    total_tokens = total_tokens
                        .checked_add(entry_tokens)
                        .ok_or_else(|| UsageProjectionError::new("period-profile token total"))?;
                }
            }
            Ok(PeriodBucket {
                label,
                hour_range,
                total_tokens,
            })
        })
        .collect()
}

pub fn find_peak_hour(
    hourly: &[HourlyUsage],
) -> Result<Option<(u32, u64, f64)>, UsageProjectionError> {
    let mut hour_totals: HashMap<u32, (u64, f64)> = HashMap::new();
    for entry in hourly {
        let hour = entry.datetime.hour();
        let entry_totals = hour_totals.entry(hour).or_insert((0, 0.0));
        let tokens = entry
            .tokens
            .checked_total()
            .ok_or_else(|| UsageProjectionError::new("peak-hour entry token total"))?;
        entry_totals.0 = entry_totals
            .0
            .checked_add(tokens)
            .ok_or_else(|| UsageProjectionError::new("peak-hour token total"))?;
        try_add_projection_cost(&mut entry_totals.1, entry.cost, "peak-hour cost")?;
    }
    Ok(hour_totals
        .into_iter()
        .max_by(
            |(hour_a, (tokens_a, cost_a)), (hour_b, (tokens_b, cost_b))| {
                tokens_a
                    .cmp(tokens_b)
                    .then_with(|| cost_a.total_cmp(cost_b))
                    .then_with(|| hour_b.cmp(hour_a))
            },
        )
        .map(|(hour, (tokens, cost))| (hour, tokens, cost)))
}
