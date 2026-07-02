//! Public helpers for graph summary metadata.

use crate::{DailyContribution, DataSummary, YearSummary};
use std::collections::HashMap;

/// Calculate summary statistics.
pub fn calculate_summary(contributions: &[DailyContribution]) -> DataSummary {
    let total_tokens: i64 = contributions.iter().map(|c| c.totals.tokens).sum();
    let total_cost = clean_total_cost(contributions.iter().map(|c| c.totals.cost).sum());
    let active_days = contributions
        .iter()
        .filter(|c| c.totals.tokens > 0 || c.totals.cost > 0.0 || c.totals.messages > 0)
        .count() as i32;
    let max_cost = clean_total_cost(
        contributions
            .iter()
            .map(|c| c.totals.cost)
            .fold(0.0, f64::max),
    );

    let mut clients_set = std::collections::HashSet::with_capacity(5);
    let mut models_set = std::collections::HashSet::with_capacity(20);

    for contribution in contributions {
        for client in &contribution.clients {
            clients_set.insert(client.client.clone());
            models_set.insert(client.model_id.clone());
        }
    }

    DataSummary {
        total_tokens,
        total_cost,
        total_days: contributions.len() as i32,
        active_days,
        average_per_day: if active_days > 0 {
            total_cost / active_days as f64
        } else {
            0.0
        },
        max_cost_in_single_day: max_cost,
        clients: {
            let mut clients: Vec<_> = clients_set.into_iter().collect();
            clients.sort();
            clients
        },
        models: {
            let mut models: Vec<_> = models_set.into_iter().collect();
            models.sort();
            models
        },
    }
}

/// Normalize `-0.0` to `0.0` so serialized reports do not display negative zero.
fn clean_total_cost(cost: f64) -> f64 {
    if cost == 0.0 {
        0.0
    } else {
        cost
    }
}

/// Calculate year summaries.
pub fn calculate_years(contributions: &[DailyContribution]) -> Vec<YearSummary> {
    let mut years_map: HashMap<String, YearAccumulator> = HashMap::with_capacity(5);

    for contribution in contributions {
        if contribution.date.len() < 4 {
            eprintln!(
                "Warning: Skipping contribution with invalid date '{}' ({} tokens, ${:.4} cost)",
                contribution.date, contribution.totals.tokens, contribution.totals.cost
            );
            continue;
        }
        let year = &contribution.date[0..4];
        let entry = years_map.entry(year.to_string()).or_default();
        entry.tokens += contribution.totals.tokens;
        entry.cost += contribution.totals.cost;

        if entry.start.is_empty() || contribution.date < entry.start {
            entry.start = contribution.date.clone();
        }
        if entry.end.is_empty() || contribution.date > entry.end {
            entry.end = contribution.date.clone();
        }
    }

    let mut years: Vec<YearSummary> = years_map
        .into_iter()
        .map(|(year, acc)| YearSummary {
            year,
            total_tokens: acc.tokens,
            total_cost: acc.cost,
            range_start: acc.start,
            range_end: acc.end,
        })
        .collect();

    years.sort_by(|a, b| a.year.cmp(&b.year));
    years
}

#[derive(Default)]
struct YearAccumulator {
    tokens: i64,
    cost: f64,
    start: String,
    end: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientContribution, DailyTotals, SessionContribution, TokenBreakdown};

    fn contribution(
        date: &str,
        tokens: i64,
        cost: f64,
        messages: i32,
        clients: Vec<ClientContribution>,
    ) -> DailyContribution {
        DailyContribution {
            date: date.to_string(),
            totals: DailyTotals {
                tokens,
                cost,
                messages,
            },
            intensity: 0,
            token_breakdown: TokenBreakdown::default(),
            clients,
            active_time_ms: None,
        }
    }

    fn client(client: &str, model: &str) -> ClientContribution {
        ClientContribution {
            client: client.to_string(),
            model_id: model.to_string(),
            provider_id: "test-provider".to_string(),
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            messages: 0,
        }
    }

    #[test]
    fn calculate_summary_empty() {
        let summary = calculate_summary(&[]);

        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_cost, 0.0);
        assert_eq!(summary.total_days, 0);
        assert_eq!(summary.active_days, 0);
        assert_eq!(summary.average_per_day, 0.0);
        assert_eq!(summary.max_cost_in_single_day, 0.0);
    }

    #[test]
    fn calculate_summary_multiple_days() {
        let contributions = vec![
            contribution(
                "2024-01-01",
                1000,
                0.05,
                1,
                vec![client("opencode", "mimo-v2.5-pro")],
            ),
            contribution(
                "2024-01-02",
                2000,
                0.10,
                2,
                vec![client("claude", "claude-sonnet-4.6")],
            ),
            contribution(
                "2024-01-03",
                1500,
                0.08,
                1,
                vec![client("opencode", "mimo-v2.5-pro")],
            ),
        ];

        let summary = calculate_summary(&contributions);

        assert_eq!(summary.total_tokens, 4500);
        assert!((summary.total_cost - 0.23).abs() < 0.0001);
        assert_eq!(summary.total_days, 3);
        assert_eq!(summary.active_days, 3);
        assert!((summary.average_per_day - 0.23 / 3.0).abs() < 0.0001);
        assert!((summary.max_cost_in_single_day - 0.10).abs() < 0.0001);
        assert_eq!(summary.clients, vec!["claude", "opencode"]);
        assert_eq!(summary.models, vec!["claude-sonnet-4.6", "mimo-v2.5-pro"]);
    }

    #[test]
    fn calculate_summary_counts_only_active_days() {
        let contributions = vec![
            contribution("2024-01-01", 1000, 0.05, 1, Vec::new()),
            contribution("2024-01-02", 0, 1.25, 0, Vec::new()),
            contribution("2024-01-03", 0, 0.0, 0, Vec::new()),
        ];

        let summary = calculate_summary(&contributions);

        assert_eq!(summary.total_days, 3);
        assert_eq!(summary.active_days, 2);
        assert!((summary.average_per_day - 0.65).abs() < 0.0001);
    }

    #[test]
    fn calculate_years_empty() {
        assert!(calculate_years(&[]).is_empty());
    }

    #[test]
    fn calculate_years_groups_and_sorts_by_year() {
        let contributions = vec![
            contribution("2023-12-31", 1000, 0.05, 1, Vec::new()),
            contribution("2024-01-01", 2000, 0.10, 1, Vec::new()),
            contribution("2024-06-15", 1500, 0.08, 1, Vec::new()),
            contribution("2025-01-01", 3000, 0.15, 1, Vec::new()),
        ];

        let years = calculate_years(&contributions);

        assert_eq!(years.len(), 3);
        assert_eq!(years[0].year, "2023");
        assert_eq!(years[1].year, "2024");
        assert_eq!(years[2].year, "2025");
        assert_eq!(years[1].total_tokens, 3500);
        assert!((years[1].total_cost - 0.18).abs() < 0.0001);
        assert_eq!(years[1].range_start, "2024-01-01");
        assert_eq!(years[1].range_end, "2024-06-15");
    }

    #[test]
    fn calculate_years_skips_invalid_short_dates() {
        let years = calculate_years(&[contribution("abc", 1000, 0.05, 1, Vec::new())]);
        assert!(years.is_empty());
    }

    #[test]
    fn session_contribution_serde_round_trip() {
        let contrib = SessionContribution {
            session_id: "019e1e27-af49-7cd1-89b7-7bad1c3f3be2".into(),
            client: "codex".into(),
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            totals: DailyTotals {
                tokens: 25298,
                cost: 0.0123,
                messages: 12,
            },
            token_breakdown: TokenBreakdown {
                input: 25_251,
                output: 47,
                cache_read: 1_920,
                cache_write: 0,
                reasoning: 40,
            },
            clients: vec![ClientContribution {
                client: "codex".into(),
                model_id: "gpt-5".into(),
                provider_id: "openai".into(),
                tokens: TokenBreakdown {
                    input: 25_251,
                    output: 47,
                    cache_read: 1_920,
                    cache_write: 0,
                    reasoning: 40,
                },
                cost: 0.0123,
                messages: 12,
            }],
            first_seen: 1_715_551_577,
            last_seen: 1_715_551_612,
        };

        let json = serde_json::to_string(&contrib).expect("serialize");
        let parsed: SessionContribution = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed, contrib);
        assert!(json.contains("\"session_id\":\"019e1e27"));
    }
}
