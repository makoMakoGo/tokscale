use std::collections::HashMap;

use ratatui::style::Color;
use tokscale_core::ClientId;

use super::config::TokscaleConfig;
use super::data::ModelUsage;

fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

fn parse_catalog_color(hex: &str) -> Color {
    parse_hex_color(hex).expect("client catalog colors are validated as #RRGGBB")
}

pub fn get_model_color(_model: &str) -> Color {
    get_provider_shade("unknown", 0)
}

/// Returns the shade for a given `(provider, rank)` pair.
/// Honors `[colors.providers]` config overrides at every rank by deriving
/// a 7-step lighten-to-white palette from the override base color.
pub fn get_provider_shade(provider: &str, rank: usize) -> Color {
    let provider_key = provider.to_lowercase();
    if let Some(base) = TokscaleConfig::load()
        .get_provider_color_hex(&provider_key)
        .and_then(parse_hex_color)
    {
        return shade_from_base(base, rank);
    }

    let palette: &[(u8, u8, u8)] = match provider_key.as_str() {
        s if s.contains("anthropic") => &ANTHROPIC_SHADES,
        s if s.contains("openai") => &OPENAI_SHADES,
        s if s.contains("google") || s.contains("gemini") => &GOOGLE_SHADES,
        s if s.contains("deepseek") => &DEEPSEEK_SHADES,
        s if s.contains("xai") || s.contains("grok") => &XAI_SHADES,
        s if s.contains("meta") || s.contains("llama") => &META_SHADES,
        s if s.contains("cursor") => &CURSOR_SHADES,
        _ => &UNKNOWN_SHADES,
    };

    let idx = rank.min(palette.len() - 1);
    let (r, g, b) = palette[idx];
    Color::Rgb(r, g, b)
}

/// Generates a 7-step monochromatic palette from `base` by interpolating
/// toward white. Factors roughly match the end-of-ramp lightness of the
/// hardcoded palettes so overrides feel visually consistent.
fn shade_from_base(base: Color, rank: usize) -> Color {
    const FACTORS: [f32; 7] = [0.00, 0.11, 0.22, 0.33, 0.44, 0.56, 0.67];
    let Color::Rgb(r, g, b) = base else {
        return base;
    };
    let idx = rank.min(FACTORS.len() - 1);
    let f = FACTORS[idx];
    let lerp = |c: u8| -> u8 {
        let c = c as f32;
        (c + (255.0 - c) * f).round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(lerp(r), lerp(g), lerp(b))
}

const ANTHROPIC_SHADES: [(u8, u8, u8); 7] = [
    (218, 119, 86),  // #DA7756
    (223, 136, 107), // #DF886B
    (227, 153, 128), // #E39980
    (232, 170, 149), // #E8AA95
    (236, 184, 166), // #ECB8A6
    (239, 197, 183), // #EFC5B7
    (243, 210, 199), // #F3D2C7
];

const OPENAI_SHADES: [(u8, u8, u8); 7] = [
    (16, 185, 129),  // #10B981
    (18, 208, 145),  // #12D091
    (20, 232, 162),  // #14E8A2
    (41, 236, 172),  // #29ECAC
    (61, 238, 179),  // #3DEEB3
    (97, 241, 193),  // #61F1C1
    (133, 244, 208), // #85F4D0
];

const GOOGLE_SHADES: [(u8, u8, u8); 7] = [
    (59, 130, 246),  // #3B82F6
    (83, 146, 247),  // #5392F7
    (108, 161, 248), // #6CA1F8
    (132, 177, 249), // #84B1F9
    (153, 190, 250), // #99BEFA
    (172, 202, 251), // #ACCAFB
    (190, 214, 252), // #BED6FC
];

const DEEPSEEK_SHADES: [(u8, u8, u8); 7] = [
    (6, 182, 212),   // #06B6D4
    (7, 203, 237),   // #07CBED
    (21, 215, 248),  // #15D7F8
    (45, 219, 249),  // #2DDBF9
    (66, 223, 250),  // #42DFFA
    (85, 226, 250),  // #55E2FA
    (105, 229, 251), // #69E5FB
];

const XAI_SHADES: [(u8, u8, u8); 7] = [
    (234, 179, 8),   // #EAB308
    (247, 192, 21),  // #F7C015
    (248, 199, 45),  // #F8C72D
    (249, 205, 70),  // #F9CD46
    (249, 211, 91),  // #F9D35B
    (250, 216, 110), // #FAD86E
    (251, 221, 129), // #FBDD81
];

const META_SHADES: [(u8, u8, u8); 7] = [
    (99, 102, 241),  // #6366F1
    (122, 125, 243), // #7A7DF3
    (146, 148, 245), // #9294F5
    (169, 171, 247), // #A9ABF7
    (189, 190, 249), // #BDBEF9
    (207, 208, 251), // #CFD0FB
    (225, 226, 252), // #E1E2FC
];

const CURSOR_SHADES: [(u8, u8, u8); 7] = [
    (139, 92, 246),  // #8B5CF6
    (154, 114, 247), // #9A72F7
    (169, 135, 248), // #A987F8
    (184, 156, 250), // #B89CFA
    (199, 177, 251), // #C7B1FB
    (215, 199, 252), // #D7C7FC
    (230, 220, 253), // #E6DCFD
];

/// Neutral gray ramp for providers that don't match any known palette.
/// Still produces distinct shades per rank instead of collapsing to white.
const UNKNOWN_SHADES: [(u8, u8, u8); 7] = [
    (136, 136, 136), // #888888
    (156, 156, 156), // #9C9C9C
    (176, 176, 176), // #B0B0B0
    (196, 196, 196), // #C4C4C4
    (212, 212, 212), // #D4D4D4
    (228, 228, 228), // #E4E4E4
    (244, 244, 244), // #F4F4F4
];
const UNKNOWN_CLIENT_COLOR: Color = Color::Rgb(136, 136, 136);

pub fn provider_color_key(provider: &str) -> &str {
    provider
        .split(", ")
        .map(str::trim)
        .find(|segment| !segment.is_empty())
        .unwrap_or("unknown")
}

pub fn get_client_color(client: &str) -> Color {
    let client_key = client.trim().to_lowercase();
    if let Some(color) = TokscaleConfig::load()
        .get_client_color_hex(&client_key)
        .and_then(parse_hex_color)
    {
        return color;
    }
    let Some(client_id) = ClientId::from_str(&client_key) else {
        return UNKNOWN_CLIENT_COLOR;
    };
    parse_catalog_color(client_id.color())
}

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
        let provider = provider_color_key(&m.provider);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::{usage_views::UsageTokenBreakdown, ClientId, ModelPerformance};

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
    fn empty_provider_uses_unknown_shade_key() {
        let map = build_model_shade_map(&[model_usage("", "u2")]);

        assert!(map.contains_key(&model_shade_key("unknown", "u2")));
        assert!(!map.contains_key(&model_shade_key("unisound", "u2")));
    }

    #[test]
    fn merged_provider_uses_first_provider_shade_key() {
        let map = build_model_shade_map(&[model_usage("openai, anthropic", "shared-model")]);

        assert!(map.contains_key(&model_shade_key("openai", "shared-model")));
        assert!(!map.contains_key(&model_shade_key("anthropic", "shared-model")));
    }

    #[test]
    fn shade_from_base_rank_0_equals_base() {
        let base = Color::Rgb(255, 0, 0);
        assert_eq!(shade_from_base(base, 0), base);
    }

    #[test]
    fn shade_from_base_lightens_monotonically_toward_white() {
        let base = Color::Rgb(0, 0, 0);
        let mut prev_r: u8 = 0;
        for rank in 0..7 {
            let Color::Rgb(r, _, _) = shade_from_base(base, rank) else {
                panic!("expected Rgb")
            };
            assert!(
                r >= prev_r,
                "shade at rank {} should not be darker than rank {}",
                rank,
                rank - 1
            );
            prev_r = r;
        }
    }

    #[test]
    fn shade_from_base_clamps_beyond_palette_length() {
        let base = Color::Rgb(100, 100, 100);
        assert_eq!(shade_from_base(base, 100), shade_from_base(base, 6));
    }

    #[test]
    fn shade_from_base_passes_through_non_rgb() {
        assert_eq!(shade_from_base(Color::Indexed(42), 5), Color::Indexed(42));
    }

    #[test]
    fn unknown_provider_returns_gray_ramp_not_pure_white() {
        let rank_0 = get_provider_shade("some-new-provider", 0);
        let rank_3 = get_provider_shade("some-new-provider", 3);
        assert_ne!(rank_0, rank_3);
        assert_ne!(rank_0, Color::Rgb(255, 255, 255));
    }

    #[test]
    fn client_color_uses_catalog_for_known_clients() {
        assert_eq!(get_client_color("opencode"), Color::Rgb(0, 168, 232));
        assert_eq!(get_client_color("droid"), Color::Rgb(31, 29, 28));
        assert_eq!(get_client_color("openclaw"), Color::Rgb(239, 68, 68));
    }

    #[test]
    fn provider_color_key_uses_unknown_or_first_provider() {
        assert_eq!(provider_color_key(""), "unknown");
        assert_eq!(provider_color_key("   "), "unknown");
        assert_eq!(provider_color_key("openai"), "openai");
        assert_eq!(provider_color_key("openai, anthropic"), "openai");
        assert_eq!(provider_color_key(" , anthropic"), "anthropic");
    }

    #[test]
    fn cursor_provider_has_distinct_shades_per_rank() {
        let rank_0 = get_provider_shade("cursor", 0);
        let rank_6 = get_provider_shade("cursor", 6);
        assert_ne!(rank_0, rank_6);
    }

    #[test]
    fn get_provider_shade_saturates_at_palette_end() {
        let last = get_provider_shade("anthropic", 6);
        let past_end = get_provider_shade("anthropic", 99);
        assert_eq!(last, past_end);
    }

    #[test]
    fn get_provider_shade_fuzzy_matching() {
        assert_eq!(
            get_provider_shade("test-anthropic", 0),
            get_provider_shade("anthropic", 0)
        );
        assert_eq!(
            get_provider_shade("company-google", 0),
            get_provider_shade("google", 0)
        );
        assert_eq!(
            get_provider_shade("openrouter-gemini-prod", 0),
            get_provider_shade("google", 0)
        );
        assert_eq!(
            get_provider_shade("deepseek-api", 0),
            get_provider_shade("deepseek", 0)
        );
        assert_eq!(
            get_provider_shade("meta-llama-endpoint", 0),
            get_provider_shade("meta", 0)
        );
    }

    #[test]
    fn parse_hex_color_rejects_non_ascii_hex_without_panicking() {
        assert_eq!(parse_hex_color("ééé"), None);
    }

    #[test]
    fn every_catalog_client_color_is_parseable() {
        for client in ClientId::iter() {
            parse_catalog_color(client.color());
        }
    }
}
