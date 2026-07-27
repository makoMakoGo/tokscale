use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use super::{UsageProjection, UsageTokenBreakdown};
use crate::tui::model_family::ModelFamily;
use tokenx_engine::ClientId;

/// Cache-token share rounded to the one decimal place shown by Overview.
///
/// Keeping the displayed precision in the value makes tier checks and text
/// rendering use the same user-visible number at threshold boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CacheRate(u16);

impl CacheRate {
    const TENTHS_PER_PERCENT: u16 = 10;
    const MAX_TENTHS: u128 = 100 * Self::TENTHS_PER_PERCENT as u128;

    pub(crate) fn from_tokens(cache_read: u64, total: u64) -> Self {
        if total == 0 {
            return Self::default();
        }

        let total = u128::from(total);
        let rounded_tenths = (u128::from(cache_read) * Self::MAX_TENTHS + total / 2) / total;
        Self(rounded_tenths.min(Self::MAX_TENTHS) as u16)
    }

    pub(crate) fn reaches(self, percent: u64) -> bool {
        u64::from(self.0) >= percent.saturating_mul(u64::from(Self::TENTHS_PER_PERCENT))
    }
}

impl fmt::Display for CacheRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}%",
            self.0 / Self::TENTHS_PER_PERCENT,
            self.0 % Self::TENTHS_PER_PERCENT
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RankedUsage {
    pub(crate) id: String,
    pub(crate) tokens: u64,
    pub(crate) cost: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct RankedClientUsage {
    pub(crate) client: ClientId,
    pub(crate) tokens: u64,
    pub(crate) cost: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct RankedFamilyUsage {
    pub(crate) family: ModelFamily,
    pub(crate) tokens: u64,
    pub(crate) cost: f64,
}

/// Stable, theme-independent data consumed by the Overview snapshot.
///
/// The summary is rebuilt only when the installed usage projection changes;
/// terminal ticks, resizes, and theme changes never need to fold daily usage.
#[derive(Debug, Clone, Default)]
pub(crate) struct OverviewSummary {
    pub(crate) tokens: UsageTokenBreakdown,
    pub(crate) cache_rate: CacheRate,
    pub(crate) active_days: usize,
    pub(crate) peak_daily_tokens: u64,
    pub(crate) peak_daily_cost: f64,
    pub(crate) model_count: usize,
    pub(crate) client_count: usize,
    pub(crate) main_session_count: usize,
    pub(crate) favorite_model: Option<RankedUsage>,
    pub(crate) favorite_client: Option<RankedClientUsage>,
    pub(crate) favorite_family: Option<RankedFamilyUsage>,
}

impl OverviewSummary {
    pub(crate) fn derive(data: &UsageProjection, main_session_count: usize) -> Self {
        let mut summary = Self {
            main_session_count,
            ..Self::default()
        };
        let mut models = BTreeMap::<String, Aggregate>::new();
        let mut clients = BTreeMap::<ClientId, Aggregate>::new();
        let mut families = BTreeMap::<ModelFamily, Aggregate>::new();

        for day in &data.daily {
            let daily_tokens = day.tokens.total();
            summary.tokens = summary
                .tokens
                .checked_add(&day.tokens)
                .expect("overview summary token buckets exceed u64::MAX");
            summary.peak_daily_tokens = summary.peak_daily_tokens.max(daily_tokens);
            if daily_tokens > 0 {
                summary.active_days += 1;
            }
            if day.cost.is_finite() {
                summary.peak_daily_cost = summary.peak_daily_cost.max(day.cost.max(0.0));
            }

            for (client_id, client) in &day.client_breakdown {
                clients
                    .entry(*client_id)
                    .or_default()
                    .add(client.tokens.total(), client.cost);

                for model in &client.models {
                    let model_tokens = model.tokens.total();
                    models
                        .entry(model.model_id.to_string())
                        .or_default()
                        .add(model_tokens, model.cost);
                    families
                        .entry(ModelFamily::from_model_id(&model.model_id))
                        .or_default()
                        .add(model_tokens, model.cost);
                }
            }
        }

        summary.model_count = models.len();
        summary.client_count = clients.len();
        summary.cache_rate =
            CacheRate::from_tokens(summary.tokens.cache_read, summary.tokens.total());
        summary.favorite_model = favorite_named(models);
        summary.favorite_client = favorite_client(clients);
        summary.favorite_family = favorite_family(families);
        summary
    }
}

#[derive(Debug, Clone, Default)]
struct Aggregate {
    tokens: u64,
    cost: f64,
}

impl Aggregate {
    fn add(&mut self, tokens: u64, cost: f64) {
        self.tokens = self.tokens.saturating_add(tokens);
        if cost.is_finite() {
            self.cost += cost.max(0.0);
        }
    }
}

fn compare_rank<K: Ord>(
    left_key: &K,
    left: &Aggregate,
    right_key: &K,
    right: &Aggregate,
) -> Ordering {
    left.tokens
        .cmp(&right.tokens)
        .then_with(|| left.cost.total_cmp(&right.cost))
        .then_with(|| right_key.cmp(left_key))
}

fn favorite_named(entries: BTreeMap<String, Aggregate>) -> Option<RankedUsage> {
    entries
        .into_iter()
        .max_by(|(left_id, left), (right_id, right)| compare_rank(left_id, left, right_id, right))
        .map(|(id, aggregate)| RankedUsage {
            id,
            tokens: aggregate.tokens,
            cost: aggregate.cost,
        })
}

fn favorite_client(entries: BTreeMap<ClientId, Aggregate>) -> Option<RankedClientUsage> {
    entries
        .into_iter()
        .max_by(|(left_id, left), (right_id, right)| compare_rank(left_id, left, right_id, right))
        .map(|(client, aggregate)| RankedClientUsage {
            client,
            tokens: aggregate.tokens,
            cost: aggregate.cost,
        })
}

fn favorite_family(entries: BTreeMap<ModelFamily, Aggregate>) -> Option<RankedFamilyUsage> {
    entries
        .into_iter()
        .max_by(|(left_family, left), (right_family, right)| {
            compare_rank(left_family, left, right_family, right)
        })
        .map(|(family, aggregate)| RankedFamilyUsage {
            family,
            tokens: aggregate.tokens,
            cost: aggregate.cost,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::{DailyClientInfo, DailyModelInfo, DailyUsage};
    use chrono::NaiveDate;

    type TestModel<'a> = (&'a str, UsageTokenBreakdown, f64);
    type TestClient<'a> = (&'a str, Vec<TestModel<'a>>);

    fn tokens(input: u64) -> UsageTokenBreakdown {
        UsageTokenBreakdown {
            input,
            ..UsageTokenBreakdown::default()
        }
    }

    fn cached_tokens(input: u64, cache_read: u64, cache_write: u64) -> UsageTokenBreakdown {
        UsageTokenBreakdown {
            input,
            cache_read,
            cache_write,
            ..UsageTokenBreakdown::default()
        }
    }

    fn day(date: &str, clients: Vec<TestClient<'_>>) -> DailyUsage {
        let mut client_breakdown = BTreeMap::new();
        let mut day_tokens = UsageTokenBreakdown::default();
        let mut day_cost = 0.0;

        for (client_id, models) in clients {
            let mut client_models = Vec::new();
            let mut client_tokens = UsageTokenBreakdown::default();
            let mut client_cost = 0.0;
            for (model_id, model_tokens, cost) in models {
                client_tokens = client_tokens
                    .checked_add(&model_tokens)
                    .expect("test client token buckets exceed u64::MAX");
                day_tokens = day_tokens
                    .checked_add(&model_tokens)
                    .expect("test daily token buckets exceed u64::MAX");
                if cost.is_finite() {
                    client_cost += cost;
                    day_cost += cost;
                }
                client_models.push(DailyModelInfo {
                    provider: "".into(),
                    model_id: model_id.into(),
                    display_name: model_id.into(),
                    workspace_key: None,
                    workspace_label: None,
                    tokens: model_tokens,
                    cost,
                    messages: 1,
                });
            }
            client_breakdown.insert(
                ClientId::from_str(client_id).expect("test client must be accepted"),
                DailyClientInfo {
                    tokens: client_tokens,
                    cost: client_cost,
                    models: client_models,
                },
            );
        }

        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: day_tokens,
            cost: day_cost,
            client_breakdown,
            message_count: 1,
            turn_count: 1,
        }
    }

    #[test]
    fn derives_stable_overview_metrics_and_rankings() {
        let data = UsageProjection {
            daily: vec![
                day(
                    "2026-07-20",
                    vec![(
                        "claude",
                        vec![
                            ("gpt-5.5", cached_tokens(100, 20, 5), 2.0),
                            ("qwq-32b", tokens(50), f64::NAN),
                        ],
                    )],
                ),
                day(
                    "2026-07-21",
                    vec![("codex", vec![("gpt-5.5", tokens(200), 3.0)])],
                ),
                day("2026-07-22", Vec::new()),
            ],
            ..UsageProjection::default()
        };

        let summary = OverviewSummary::derive(&data, 7);

        assert_eq!(summary.tokens.total(), 375);
        assert_eq!(summary.cache_rate, CacheRate::from_tokens(20, 375));
        assert_eq!(summary.active_days, 2);
        assert_eq!(summary.peak_daily_tokens, 200);
        assert_eq!(summary.peak_daily_cost, 3.0);
        assert_eq!(summary.model_count, 2);
        assert_eq!(summary.client_count, 2);
        assert_eq!(summary.main_session_count, 7);

        let model = summary.favorite_model.unwrap();
        assert_eq!(model.id, "gpt-5.5");
        assert_eq!(model.tokens, 325);
        assert_eq!(model.cost, 5.0);

        let client = summary.favorite_client.unwrap();
        assert_eq!(client.client, ClientId::Codex);
        assert_eq!(client.tokens, 200);

        let family = summary.favorite_family.unwrap();
        assert_eq!(family.family, ModelFamily::Gpt);
        assert_eq!(family.tokens, 325);
        assert_eq!(family.cost, 5.0);
    }

    #[test]
    fn cache_rate_uses_one_decimal_for_display_and_thresholds() {
        let rounded_to_fifty = CacheRate::from_tokens(4_996, 10_000);
        let displayed_below_fifty = CacheRate::from_tokens(4_960, 10_000);

        assert_eq!(rounded_to_fifty.to_string(), "50.0%");
        assert!(rounded_to_fifty.reaches(50));
        assert_eq!(displayed_below_fifty.to_string(), "49.6%");
        assert!(!displayed_below_fifty.reaches(50));

        for threshold in [50, 80, 90, 95, 99] {
            let boundary = threshold * 100;
            assert!(CacheRate::from_tokens(boundary - 4, 10_000).reaches(threshold));
            assert!(!CacheRate::from_tokens(boundary - 6, 10_000).reaches(threshold));
        }
        assert_eq!(CacheRate::from_tokens(1, 0), CacheRate::default());
    }
}
