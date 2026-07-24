use ratatui::style::Color;
use tokscale_core::ClientId;

#[cfg(test)]
use super::contrast::contrast_ratio;
use super::contrast::{ensure_contrast, WCAG_AA_TEXT_CONTRAST};
use super::model_family::ModelFamily;
use super::themes::Theme;

const GPT_COLOR: Color = Color::Rgb(16, 163, 127);
const CLAUDE_COLOR: Color = Color::Rgb(217, 119, 87);
const GEMINI_COLOR: Color = Color::Rgb(142, 124, 240);
const XAI_COLOR: Color = Color::Rgb(255, 79, 163);
const GLM_COLOR: Color = Color::Rgb(232, 232, 232);
const DEEPSEEK_COLOR: Color = Color::Rgb(77, 107, 254);
const QWEN_COLOR: Color = Color::Rgb(97, 92, 237);
const KIMI_COLOR: Color = Color::Rgb(246, 200, 95);
const MINIMAX_COLOR: Color = Color::Rgb(228, 58, 58);
const MIMO_COLOR: Color = Color::Rgb(255, 105, 0);
const MISTRAL_COLOR: Color = Color::Rgb(255, 112, 0);
const UNKNOWN_MODEL_COLOR: Color = Color::Rgb(136, 136, 136);
const UNKNOWN_CLIENT_COLOR: Color = Color::Rgb(136, 136, 136);

/// Returns the fixed brand color for a canonical model id.
///
/// Model identity is the sole input: provider and route attribution must not
/// affect presentation. Models outside the supported families use an explicit
/// neutral color rather than borrowing a provider color.
pub fn model_color(model_id: &str) -> Color {
    family_color(ModelFamily::from_model_id(model_id))
}

/// Returns the fixed brand color for an already-classified model family.
pub(crate) fn family_color(family: ModelFamily) -> Color {
    match family {
        ModelFamily::Gpt => GPT_COLOR,
        ModelFamily::Claude => CLAUDE_COLOR,
        ModelFamily::Gemini => GEMINI_COLOR,
        ModelFamily::Xai => XAI_COLOR,
        ModelFamily::Glm => GLM_COLOR,
        ModelFamily::Deepseek => DEEPSEEK_COLOR,
        ModelFamily::Qwen => QWEN_COLOR,
        ModelFamily::Kimi => KIMI_COLOR,
        ModelFamily::Minimax => MINIMAX_COLOR,
        ModelFamily::Mimo => MIMO_COLOR,
        ModelFamily::Mistral => MISTRAL_COLOR,
        ModelFamily::Unknown => UNKNOWN_MODEL_COLOR,
    }
}

/// Resolves a model-family identity color for text or chart marks rendered on
/// the theme's non-selected panel and table surfaces.
///
/// The fixed family RGB remains the identity source. The theme makes only the
/// smallest luminance adjustment needed for readable contrast across those
/// surfaces.
pub(crate) fn resolve_model_color(model_id: &str, theme: &Theme) -> Color {
    resolve_identity_color(model_color(model_id), theme)
}

pub(crate) fn resolve_family_color(family: ModelFamily, theme: &Theme) -> Color {
    resolve_identity_color(family_color(family), theme)
}

fn parse_catalog_color(hex: &str) -> Color {
    let hex = hex
        .strip_prefix('#')
        .expect("client catalog colors are validated as #RRGGBB");
    assert_eq!(
        hex.len(),
        6,
        "client catalog colors are validated as #RRGGBB"
    );
    let component = |range| {
        u8::from_str_radix(&hex[range], 16).expect("client catalog colors are validated as #RRGGBB")
    };
    Color::Rgb(component(0..2), component(2..4), component(4..6))
}

fn get_client_color(client: &str) -> Color {
    let client_key = client.trim().to_lowercase();
    let Some(client_id) = ClientId::from_str(&client_key) else {
        return UNKNOWN_CLIENT_COLOR;
    };
    parse_catalog_color(client_id.color())
}

/// Resolves a client catalog brand color for the active terminal theme.
///
/// Catalog RGB is authoritative. It is retained unless WCAG text contrast on a
/// non-selected panel or table surface requires the smallest possible blend
/// toward the theme foreground.
pub(crate) fn resolve_client_color(client: &str, theme: &Theme) -> Color {
    resolve_identity_color(get_client_color(client), theme)
}

fn resolve_identity_color(color: Color, theme: &Theme) -> Color {
    [
        theme.surface.panel,
        theme.surface.row_alt,
        theme.surface.row_current,
    ]
    .into_iter()
    .fold(color, |color, background| {
        ensure_contrast(color, background, theme.text.primary, WCAG_AA_TEXT_CONTRAST)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, TuiConfig};
    use crate::tui::themes::ThemeName;

    fn app_with_theme(theme_name: ThemeName) -> App {
        let mut app = App::new_with_cached_data(
            TuiConfig {
                theme: Some(theme_name.as_str().to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                clients: None,
                since: None,
                until: None,
                year: None,
                initial_tab: None,
            },
            None,
        )
        .unwrap();
        app.theme = Theme::from_name(theme_name);
        app
    }

    #[test]
    fn canonical_model_families_use_fixed_brand_colors() {
        let cases = [
            ("gpt-5.5", GPT_COLOR),
            ("claude-opus-4.6", CLAUDE_COLOR),
            ("gemini-2.5-pro", GEMINI_COLOR),
            ("grok-code-fast-1", XAI_COLOR),
            ("composer-2.5", XAI_COLOR),
            ("glm-4.6", GLM_COLOR),
            ("deepseek-v3.2", DEEPSEEK_COLOR),
            ("qwen3-coder-plus", QWEN_COLOR),
            ("kimi-k2", KIMI_COLOR),
            ("minimax-m3", MINIMAX_COLOR),
            ("mimo-v2.5-pro", MIMO_COLOR),
            ("mistral-large-3", MISTRAL_COLOR),
        ];

        for (model_id, expected) in cases {
            assert_eq!(model_color(model_id), expected, "model: {model_id}");
        }
    }

    #[test]
    fn route_does_not_replace_a_recognized_model_family() {
        assert_eq!(model_color("amazon-bedrock/claude-opus-4.6"), CLAUDE_COLOR);
        assert_eq!(model_color("openrouter/qwen3-coder-plus"), QWEN_COLOR);
    }

    #[test]
    fn unknown_model_family_uses_explicit_neutral_color() {
        assert_eq!(model_color("llama-4-scout"), UNKNOWN_MODEL_COLOR);
        assert_eq!(model_color(""), UNKNOWN_MODEL_COLOR);
    }

    #[test]
    fn family_brand_colors_are_unique() {
        let colors = [
            GPT_COLOR,
            CLAUDE_COLOR,
            GEMINI_COLOR,
            XAI_COLOR,
            GLM_COLOR,
            DEEPSEEK_COLOR,
            QWEN_COLOR,
            KIMI_COLOR,
            MINIMAX_COLOR,
            MIMO_COLOR,
            MISTRAL_COLOR,
            UNKNOWN_MODEL_COLOR,
        ];

        for (index, color) in colors.iter().enumerate() {
            assert!(
                !colors[index + 1..].contains(color),
                "duplicate family color: {color:?}"
            );
        }
    }

    #[test]
    fn every_model_family_meets_contrast_on_render_surfaces() {
        let models = [
            "gpt-5.5",
            "claude-opus-4.6",
            "gemini-2.5-pro",
            "grok-code-fast-1",
            "glm-4.6",
            "deepseek-v3.2",
            "qwen3-coder-plus",
            "kimi-k2",
            "minimax-m3",
            "mimo-v2.5-pro",
            "mistral-large-3",
            "unknown-model",
        ];

        for &theme_name in ThemeName::all() {
            let theme = Theme::from_name(theme_name);
            for model_id in models {
                let color = resolve_model_color(model_id, &theme);
                for (surface, background) in [
                    ("panel", theme.surface.panel),
                    ("alternate row", theme.surface.row_alt),
                    ("current row", theme.surface.row_current),
                ] {
                    let ratio = contrast_ratio(color, background);
                    assert!(
                        ratio >= WCAG_AA_TEXT_CONTRAST,
                        "{theme_name:?} {model_id} uses {color:?} on {surface} at contrast {ratio:.3}"
                    );
                }
            }
        }
    }

    #[test]
    fn client_color_uses_catalog_for_known_clients() {
        assert_eq!(get_client_color("opencode"), Color::Rgb(0, 168, 232));
        assert_eq!(get_client_color("droid"), Color::Rgb(31, 29, 28));
        assert_eq!(get_client_color("mux"), Color::Rgb(23, 23, 23));
        assert_eq!(get_client_color("grok"), Color::Rgb(23, 23, 23));
        assert_eq!(get_client_color("zcode"), Color::Rgb(17, 24, 39));
        assert_eq!(get_client_color("commandcode"), Color::Rgb(17, 24, 39));
        assert_eq!(get_client_color("openclaw"), Color::Rgb(239, 68, 68));
    }

    #[test]
    fn unknown_client_uses_explicit_neutral_color() {
        assert_eq!(get_client_color("not-a-client"), UNKNOWN_CLIENT_COLOR);
    }

    #[test]
    fn every_catalog_client_color_is_parseable() {
        for client in ClientId::iter() {
            parse_catalog_color(client.color());
        }
    }

    #[test]
    fn dark_catalog_client_color_is_adjusted_for_theme_background() {
        let app = app_with_theme(ThemeName::Blue);
        let raw = get_client_color("droid");
        let adjusted = app.client_color("droid");

        assert!(
            contrast_ratio(raw, app.theme.surface.panel) < WCAG_AA_TEXT_CONTRAST,
            "test fixture must begin below the policy threshold"
        );
        assert_ne!(adjusted, raw);
        assert!(contrast_ratio(adjusted, app.theme.surface.panel) >= WCAG_AA_TEXT_CONTRAST);
    }

    #[test]
    fn every_catalog_client_color_meets_contrast_on_render_surfaces() {
        for &theme_name in ThemeName::all() {
            let app = app_with_theme(theme_name);
            for client_id in ClientId::iter() {
                let color = app.client_color(client_id.as_str());
                for (surface, background) in [
                    ("panel", app.theme.surface.panel),
                    ("alternate row", app.theme.surface.row_alt),
                    ("current row", app.theme.surface.row_current),
                ] {
                    let ratio = contrast_ratio(color, background);
                    assert!(
                        ratio >= WCAG_AA_TEXT_CONTRAST,
                        "{theme_name:?} {} uses {color:?} on {surface} at contrast {ratio:.3}",
                        client_id.as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn exact_rgb_background_match_is_blended_to_rgb_contrast() {
        let background = Color::Rgb(13, 17, 23);
        let foreground = Color::Rgb(201, 209, 217);

        let adjusted = ensure_contrast(background, background, foreground, WCAG_AA_TEXT_CONTRAST);

        assert!(matches!(adjusted, Color::Rgb(..)));
        assert_ne!(adjusted, background);
        assert!(contrast_ratio(adjusted, background) >= WCAG_AA_TEXT_CONTRAST);
    }
}
