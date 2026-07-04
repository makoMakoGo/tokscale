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
