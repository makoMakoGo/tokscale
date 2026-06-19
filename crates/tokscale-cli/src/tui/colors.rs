use std::collections::HashMap;

use ratatui::style::Color;

use super::data::ModelUsage;
use super::ui::widgets::{get_provider_from_model, get_provider_shade};

pub fn model_shade_key(provider: &str, model: &str) -> String {
    format!("{provider}\0{model}")
}

/// Builds a `(provider, model) -> Color` map where each provider's models are
/// cost-ranked; rank 0 (highest cost) gets the base provider color and later
/// ranks get progressively lighter shades.
///
/// Aggregates cost per (provider, model) so the same model appearing in
/// multiple group-by buckets (e.g. `GroupBy::WorkspaceModel`) doesn't inflate
/// the rank count. Ties on cost are resolved by model name so shade assignment
/// stays deterministic across refreshes.
pub fn build_model_shade_map(models: &[ModelUsage]) -> HashMap<String, Color> {
    let mut by_provider: HashMap<&str, HashMap<&str, f64>> = HashMap::new();
    for m in models {
        let provider = provider_color_key(&m.provider, &m.model);
        let cost = if m.cost.is_finite() { m.cost } else { 0.0 };
        *by_provider
            .entry(provider)
            .or_default()
            .entry(m.model.as_str())
            .or_insert(0.0) += cost;
    }

    let mut map = HashMap::new();
    for (provider, models_map) in by_provider {
        let mut ranked: Vec<(&str, f64)> = models_map.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (rank, (name, _)) in ranked.iter().enumerate() {
            map.insert(
                model_shade_key(provider, name),
                get_provider_shade(provider, rank),
            );
        }
    }
    map
}

fn provider_color_key<'a>(provider: &'a str, model: &'a str) -> &'a str {
    if provider.is_empty() || provider.contains(", ") {
        get_provider_from_model(model)
    } else {
        provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::{usage_views::UsageTokenBreakdown, ModelPerformance};

    fn model_usage(provider: &str, model: &str) -> ModelUsage {
        ModelUsage {
            model: model.to_string(),
            provider: provider.to_string(),
            client: "test".to_string(),
            workspace_key: None,
            workspace_label: None,
            tokens: UsageTokenBreakdown::default(),
            cost: 1.0,
            performance: ModelPerformance::default(),
            session_count: 1,
        }
    }

    #[test]
    fn empty_provider_u2_uses_unisound_shade_key() {
        let map = build_model_shade_map(&[model_usage("", "u2")]);

        assert!(map.contains_key(&model_shade_key("unisound", "u2")));
        assert!(!map.contains_key(&model_shade_key("unknown", "u2")));
    }

    #[test]
    fn empty_provider_legacy_tui_aliases_keep_provider_shade_keys() {
        let cases = [
            ("codex-mini-latest", "openai"),
            ("text-embedding-3-small", "openai"),
            ("whisper-1", "openai"),
            ("auto", "cursor"),
            ("cursor-small", "cursor"),
            ("bedrock/anthropic.claude-sonnet-4", "anthropic"),
            ("us.anthropic.claude-3-5-sonnet-20241022-v1:0", "anthropic"),
        ];

        for (model, provider) in cases {
            let map = build_model_shade_map(&[model_usage("", model)]);

            assert!(map.contains_key(&model_shade_key(provider, model)));
            assert!(!map.contains_key(&model_shade_key("unknown", model)));
        }
    }
}
