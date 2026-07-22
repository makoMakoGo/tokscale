use ratatui::style::Color;
use tokscale_core::ClientId;

use super::model_family::ModelFamily;

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

pub fn get_client_color(client: &str) -> Color {
    let client_key = client.trim().to_lowercase();
    let Some(client_id) = ClientId::from_str(&client_key) else {
        return UNKNOWN_CLIENT_COLOR;
    };
    parse_catalog_color(client_id.color())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn client_color_uses_catalog_for_known_clients() {
        assert_eq!(get_client_color("opencode"), Color::Rgb(0, 168, 232));
        assert_eq!(get_client_color("droid"), Color::Rgb(31, 29, 28));
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
}
