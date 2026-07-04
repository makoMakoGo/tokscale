use std::collections::HashMap;

use crate::{DailyContribution, DataSummary, GraphMeta, GraphResult, YearSummary};

/// Normalize `-0.0` to `0.0` so serialized reports do not display negative zero.
fn clean_total_cost(cost: f64) -> f64 {
    if cost == 0.0 {
        0.0
    } else {
        cost
    }
}

/// Calculate summary statistics for contribution graph output.
pub fn calculate_summary(contributions: &[DailyContribution]) -> DataSummary {
    let total_tokens = contributions
        .iter()
        .map(|c| c.totals.tokens)
        .fold(0_i64, i64::saturating_add);
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
        for source in &contribution.clients {
            clients_set.insert(source.client.clone());
            models_set.insert(source.model_id.clone());
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
            let mut v: Vec<_> = clients_set.into_iter().collect();
            v.sort();
            v
        },
        models: {
            let mut v: Vec<_> = models_set.into_iter().collect();
            v.sort();
            v
        },
    }
}

/// Calculate per-year summaries for contribution graph output.
pub fn calculate_years(contributions: &[DailyContribution]) -> Vec<YearSummary> {
    #[derive(Default)]
    struct YearAcc {
        tokens: i64,
        cost: f64,
        start: String,
        end: String,
    }

    let mut years_map: HashMap<String, YearAcc> = HashMap::with_capacity(5);

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
        entry.tokens = entry.tokens.saturating_add(contribution.totals.tokens);
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
            total_cost: clean_total_cost(acc.cost),
            range_start: acc.start,
            range_end: acc.end,
        })
        .collect();
    years.sort_by(|a, b| a.year.cmp(&b.year));
    years
}

pub(crate) fn finish_graph_result(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientContribution, DailyTotals, TokenBreakdown};

    fn contribution(date: &str, tokens: i64, cost: f64, messages: i32) -> DailyContribution {
        DailyContribution {
            date: date.to_string(),
            totals: DailyTotals {
                tokens,
                cost,
                messages,
            },
            intensity: 0,
            token_breakdown: TokenBreakdown::default(),
            clients: Vec::new(),
            active_time_ms: None,
        }
    }

    fn contribution_with_client(
        date: &str,
        tokens: i64,
        cost: f64,
        model_id: &str,
        client: &str,
    ) -> DailyContribution {
        let mut contribution = contribution(date, tokens, cost, 1);
        contribution.token_breakdown = TokenBreakdown {
            input: tokens / 2,
            output: tokens / 2,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        contribution.clients.push(ClientContribution {
            client: client.to_string(),
            model_id: model_id.to_string(),
            provider_id: "test-provider".to_string(),
            tokens: contribution.token_breakdown.clone(),
            cost,
            messages: 1,
        });
        contribution
    }

    #[test]
    fn test_calculate_summary_empty() {
        let contributions = Vec::new();
        let summary = calculate_summary(&contributions);

        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_cost, 0.0);
        assert_eq!(summary.total_days, 0);
        assert_eq!(summary.active_days, 0);
        assert_eq!(summary.average_per_day, 0.0);
        assert_eq!(summary.max_cost_in_single_day, 0.0);
    }

    #[test]
    fn test_calculate_summary_single_day() {
        let contributions = vec![contribution_with_client(
            "2024-01-01",
            1000,
            0.05,
            "claude-sonnet-4.6",
            "opencode",
        )];
        let summary = calculate_summary(&contributions);

        assert_eq!(summary.total_tokens, 1000);
        assert_eq!(summary.total_cost, 0.05);
        assert_eq!(summary.total_days, 1);
        assert_eq!(summary.active_days, 1);
        assert_eq!(summary.average_per_day, 0.05);
        assert_eq!(summary.max_cost_in_single_day, 0.05);
        assert_eq!(summary.clients, vec!["opencode"]);
        assert_eq!(summary.models, vec!["claude-sonnet-4.6"]);
    }

    #[test]
    fn test_calculate_summary_multiple_days() {
        let contributions = vec![
            contribution_with_client("2024-01-01", 1000, 0.05, "claude-sonnet-4.6", "opencode"),
            contribution_with_client("2024-01-02", 2000, 0.10, "gpt-4", "claude"),
            contribution_with_client("2024-01-03", 1500, 0.08, "claude-sonnet-4.6", "opencode"),
        ];
        let summary = calculate_summary(&contributions);

        assert_eq!(summary.total_tokens, 4500);
        assert!((summary.total_cost - 0.23).abs() < 0.0001);
        assert_eq!(summary.total_days, 3);
        assert_eq!(summary.active_days, 3);
        assert!((summary.average_per_day - 0.23 / 3.0).abs() < 0.0001);
        assert!((summary.max_cost_in_single_day - 0.10).abs() < 0.0001);
        assert_eq!(summary.clients, vec!["claude", "opencode"]);
        assert_eq!(summary.models, vec!["claude-sonnet-4.6", "gpt-4"]);
    }

    #[test]
    fn test_calculate_summary_with_zero_token_days() {
        let contributions = vec![
            contribution("2024-01-01", 1000, 0.05, 1),
            contribution("2024-01-02", 0, 0.0, 0),
        ];

        let summary = calculate_summary(&contributions);
        assert_eq!(summary.total_days, 2);
        assert_eq!(summary.active_days, 1);
        assert!((summary.average_per_day - 0.05).abs() < 0.0001);
    }

    #[test]
    fn test_calculate_summary_counts_cost_only_days_as_active() {
        let contributions = vec![
            contribution("2024-01-01", 1000, 0.05, 1),
            contribution("2024-01-02", 0, 1.25, 0),
            contribution("2024-01-03", 0, 0.0, 0),
        ];

        let summary = calculate_summary(&contributions);
        assert_eq!(summary.total_days, 3);
        assert_eq!(summary.active_days, 2);
        assert!((summary.average_per_day - 0.65).abs() < 0.0001);
    }

    #[test]
    fn test_calculate_summary_saturates_total_tokens() {
        let contributions = vec![
            contribution("2024-01-01", i64::MAX, 0.05, 1),
            contribution("2024-01-02", 1, 0.10, 1),
        ];

        let summary = calculate_summary(&contributions);
        assert_eq!(summary.total_tokens, i64::MAX);
    }

    #[test]
    fn test_calculate_years_empty() {
        let contributions = Vec::new();
        let years = calculate_years(&contributions);
        assert_eq!(years.len(), 0);
    }

    #[test]
    fn test_calculate_years_single_year() {
        let contributions = vec![
            contribution("2024-01-01", 1000, 0.05, 1),
            contribution("2024-06-15", 2000, 0.10, 1),
            contribution("2024-12-31", 1500, 0.08, 1),
        ];
        let years = calculate_years(&contributions);

        assert_eq!(years.len(), 1);
        assert_eq!(years[0].year, "2024");
        assert_eq!(years[0].total_tokens, 4500);
        assert!((years[0].total_cost - 0.23).abs() < 0.0001);
        assert_eq!(years[0].range_start, "2024-01-01");
        assert_eq!(years[0].range_end, "2024-12-31");
    }

    #[test]
    fn test_calculate_years_multiple_years() {
        let contributions = vec![
            contribution("2023-12-31", 1000, 0.05, 1),
            contribution("2024-01-01", 2000, 0.10, 1),
            contribution("2024-06-15", 1500, 0.08, 1),
            contribution("2025-01-01", 3000, 0.15, 1),
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
    fn test_calculate_years_year_boundary() {
        let contributions = vec![
            contribution("2024-12-31", 1000, 0.05, 1),
            contribution("2025-01-01", 2000, 0.10, 1),
        ];
        let years = calculate_years(&contributions);

        assert_eq!(years.len(), 2);
        assert_eq!(years[0].year, "2024");
        assert_eq!(years[0].total_tokens, 1000);
        assert_eq!(years[1].year, "2025");
        assert_eq!(years[1].total_tokens, 2000);
    }

    #[test]
    fn test_calculate_years_invalid_date() {
        let contributions = vec![contribution("abc", 1000, 0.05, 1)];

        let years = calculate_years(&contributions);
        assert_eq!(years.len(), 0);
    }

    #[test]
    fn test_calculate_years_saturates_tokens_and_cleans_negative_zero_cost() {
        let contributions = vec![
            contribution("2024-01-01", i64::MAX, -0.0, 1),
            contribution("2024-01-02", 1, 0.0, 1),
        ];

        let years = calculate_years(&contributions);
        assert_eq!(years.len(), 1);
        assert_eq!(years[0].total_tokens, i64::MAX);
        assert_eq!(clean_total_cost(-0.0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(years[0].total_cost.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn test_finish_graph_result_empty() {
        let contributions = Vec::new();
        let result = finish_graph_result(contributions, 100);

        assert_eq!(result.contributions.len(), 0);
        assert_eq!(result.summary.total_tokens, 0);
        assert_eq!(result.years.len(), 0);
        assert_eq!(result.meta.processing_time_ms, 100);
        assert_eq!(result.meta.date_range_start, "");
        assert_eq!(result.meta.date_range_end, "");
    }

    #[test]
    fn test_finish_graph_result_with_data() {
        let contributions = vec![
            contribution_with_client("2024-01-01", 1000, 0.05, "claude-sonnet-4.6", "opencode"),
            contribution_with_client("2024-01-02", 2000, 0.10, "gpt-4", "claude"),
        ];
        let result = finish_graph_result(contributions, 150);

        assert_eq!(result.contributions.len(), 2);
        assert_eq!(result.summary.total_tokens, 3000);
        assert_eq!(result.years.len(), 1);
        assert_eq!(result.meta.processing_time_ms, 150);
        assert_eq!(result.meta.date_range_start, "2024-01-01");
        assert_eq!(result.meta.date_range_end, "2024-01-02");
        assert_eq!(result.meta.version, env!("CARGO_PKG_VERSION"));
    }
}
