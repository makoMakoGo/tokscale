use std::cmp::Ordering;
use std::collections::BTreeMap;

use tokscale_core::inferred_provider_from_model;

use super::{TokenBreakdown, UsageData};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OverviewFamily {
    Gpt,
    Claude,
    Gemini,
    Glm,
    Deepseek,
    Qwen,
    Kimi,
    Minimax,
    Mimo,
    Unknown,
}

impl OverviewFamily {
    pub(crate) fn from_model_id(model_id: &str) -> Self {
        match inferred_provider_from_model(model_id) {
            Some("openai") => Self::Gpt,
            Some("anthropic") => Self::Claude,
            Some("google") => Self::Gemini,
            Some("zai") => Self::Glm,
            Some("deepseek") => Self::Deepseek,
            Some("qwen") => Self::Qwen,
            Some("kimi") => Self::Kimi,
            Some("minimax") => Self::Minimax,
            Some("xiaomi") => Self::Mimo,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RankedUsage {
    pub(crate) id: String,
    pub(crate) tokens: u64,
    pub(crate) cost: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct RankedFamilyUsage {
    pub(crate) family: OverviewFamily,
    pub(crate) tokens: u64,
    pub(crate) cost: f64,
}

/// Stable, theme-independent data consumed by the Overview snapshot.
///
/// The summary is rebuilt only when the installed report projection changes;
/// terminal ticks, resizes, and theme changes never need to fold daily usage.
#[derive(Debug, Clone, Default)]
pub(crate) struct OverviewSummary {
    pub(crate) tokens: TokenBreakdown,
    pub(crate) active_days: usize,
    pub(crate) peak_daily_tokens: u64,
    pub(crate) peak_daily_cost: f64,
    pub(crate) model_count: usize,
    pub(crate) client_count: usize,
    pub(crate) main_session_count: usize,
    pub(crate) favorite_model: Option<RankedUsage>,
    pub(crate) favorite_client: Option<RankedUsage>,
    pub(crate) favorite_family: Option<RankedFamilyUsage>,
}

impl OverviewSummary {
    pub(crate) fn derive(data: &UsageData, main_session_count: usize) -> Self {
        let mut summary = Self {
            main_session_count,
            ..Self::default()
        };
        let mut models = BTreeMap::<String, Aggregate>::new();
        let mut clients = BTreeMap::<String, Aggregate>::new();
        let mut families = BTreeMap::<OverviewFamily, Aggregate>::new();

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
                    .entry(client_id.clone())
                    .or_default()
                    .add(client.tokens.total(), client.cost);

                for model in client.models.values() {
                    let model_tokens = model.tokens.total();
                    models
                        .entry(model.model_id.clone())
                        .or_default()
                        .add(model_tokens, model.cost);
                    families
                        .entry(OverviewFamily::from_model_id(&model.model_id))
                        .or_default()
                        .add(model_tokens, model.cost);
                }
            }
        }

        summary.model_count = models.len();
        summary.client_count = clients.len();
        summary.favorite_model = favorite_named(models);
        summary.favorite_client = favorite_named(clients);
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

fn favorite_family(entries: BTreeMap<OverviewFamily, Aggregate>) -> Option<RankedFamilyUsage> {
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

    type TestModel<'a> = (&'a str, u64, f64);
    type TestClient<'a> = (&'a str, Vec<TestModel<'a>>);

    fn tokens(input: u64) -> TokenBreakdown {
        TokenBreakdown {
            input,
            ..TokenBreakdown::default()
        }
    }

    fn day(date: &str, clients: Vec<TestClient<'_>>) -> DailyUsage {
        let mut client_breakdown = BTreeMap::new();
        let mut day_tokens = TokenBreakdown::default();
        let mut day_cost = 0.0;

        for (client_id, models) in clients {
            let mut client_models = BTreeMap::new();
            let mut client_tokens = TokenBreakdown::default();
            let mut client_cost = 0.0;
            for (model_id, input, cost) in models {
                client_tokens.input = client_tokens.input.saturating_add(input);
                day_tokens.input = day_tokens.input.saturating_add(input);
                if cost.is_finite() {
                    client_cost += cost;
                    day_cost += cost;
                }
                client_models.insert(
                    model_id.to_string(),
                    DailyModelInfo {
                        provider: String::new(),
                        model_id: model_id.to_string(),
                        display_name: model_id.to_string(),
                        color_key: model_id.to_string(),
                        workspace_key: None,
                        workspace_label: None,
                        tokens: tokens(input),
                        cost,
                        messages: 1,
                    },
                );
            }
            client_breakdown.insert(
                client_id.to_string(),
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
        let data = UsageData {
            daily: vec![
                day(
                    "2026-07-20",
                    vec![(
                        "claude",
                        vec![("gpt-5.5", 100, 2.0), ("qwq-32b", 50, f64::NAN)],
                    )],
                ),
                day("2026-07-21", vec![("codex", vec![("gpt-5.5", 200, 3.0)])]),
                day("2026-07-22", Vec::new()),
            ],
            ..UsageData::default()
        };

        let summary = OverviewSummary::derive(&data, 7);

        assert_eq!(summary.tokens.total(), 350);
        assert_eq!(summary.active_days, 2);
        assert_eq!(summary.peak_daily_tokens, 200);
        assert_eq!(summary.peak_daily_cost, 3.0);
        assert_eq!(summary.model_count, 2);
        assert_eq!(summary.client_count, 2);
        assert_eq!(summary.main_session_count, 7);

        let model = summary.favorite_model.unwrap();
        assert_eq!(model.id, "gpt-5.5");
        assert_eq!(model.tokens, 300);
        assert_eq!(model.cost, 5.0);

        let client = summary.favorite_client.unwrap();
        assert_eq!(client.id, "codex");
        assert_eq!(client.tokens, 200);

        let family = summary.favorite_family.unwrap();
        assert_eq!(family.family, OverviewFamily::Gpt);
        assert_eq!(family.tokens, 300);
        assert_eq!(family.cost, 5.0);
    }

    #[test]
    fn family_detection_covers_the_overview_portraits() {
        for (model_id, family) in [
            ("gpt-5.5", OverviewFamily::Gpt),
            ("codex-mini-latest", OverviewFamily::Gpt),
            ("o3", OverviewFamily::Gpt),
            ("claude-opus-4-7", OverviewFamily::Claude),
            ("gemini-2.5-pro", OverviewFamily::Gemini),
            ("glm-4.6", OverviewFamily::Glm),
            ("deepseek-v3.2", OverviewFamily::Deepseek),
            ("qwen3-coder-plus", OverviewFamily::Qwen),
            ("qwq-32b", OverviewFamily::Qwen),
            ("qvq-max", OverviewFamily::Qwen),
            ("kimi-k2", OverviewFamily::Kimi),
            ("k3-thinking", OverviewFamily::Kimi),
            ("minimax-m3", OverviewFamily::Minimax),
            ("mimo-v2.5-pro", OverviewFamily::Mimo),
            ("llama-4-scout", OverviewFamily::Unknown),
            ("mistral-large-3", OverviewFamily::Unknown),
        ] {
            assert_eq!(
                OverviewFamily::from_model_id(model_id),
                family,
                "{model_id}"
            );
        }
    }
}
