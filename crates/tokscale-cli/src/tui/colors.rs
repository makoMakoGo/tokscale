use ratatui::style::Color;
use tokscale_core::ClientId;

#[cfg(test)]
use super::contrast::contrast_ratio;
use super::contrast::{ensure_contrast, WCAG_AA_TEXT_CONTRAST};
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

/// Theme-resolved identity colors. Construction performs all contrast work so
/// render-time lookups remain pure array indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdentityPalette {
    models: [Color; ModelFamily::COUNT],
    clients: [Color; ClientId::COUNT],
    unknown_client: Color,
}

impl IdentityPalette {
    pub(crate) fn resolve(backgrounds: [Color; 3], blend_target: Color) -> Self {
        let adapt = |brand_color| {
            backgrounds
                .into_iter()
                .fold(brand_color, |color, background| {
                    ensure_contrast(color, background, blend_target, WCAG_AA_TEXT_CONTRAST)
                })
        };

        Self {
            models: ModelFamily::ALL.map(|family| adapt(model_brand_color(family))),
            clients: ClientId::ALL.map(|client| adapt(client_brand_color(client))),
            unknown_client: adapt(UNKNOWN_CLIENT_COLOR),
        }
    }

    pub(crate) fn model(&self, family: ModelFamily) -> Color {
        self.models[family.index()]
    }

    pub(crate) fn client(&self, client: Option<ClientId>) -> Color {
        client.map_or(self.unknown_client, |client| self.clients[client as usize])
    }
}

/// Returns the catalog brand color for an already-classified model family.
/// Model identity is the sole input; route attribution never changes it.
fn model_brand_color(family: ModelFamily) -> Color {
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

fn client_brand_color(client: ClientId) -> Color {
    parse_catalog_color(client.color())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_model_families_use_fixed_brand_colors() {
        let cases = [
            (ModelFamily::Gpt, GPT_COLOR),
            (ModelFamily::Claude, CLAUDE_COLOR),
            (ModelFamily::Gemini, GEMINI_COLOR),
            (ModelFamily::Xai, XAI_COLOR),
            (ModelFamily::Glm, GLM_COLOR),
            (ModelFamily::Deepseek, DEEPSEEK_COLOR),
            (ModelFamily::Qwen, QWEN_COLOR),
            (ModelFamily::Kimi, KIMI_COLOR),
            (ModelFamily::Minimax, MINIMAX_COLOR),
            (ModelFamily::Mimo, MIMO_COLOR),
            (ModelFamily::Mistral, MISTRAL_COLOR),
            (ModelFamily::Unknown, UNKNOWN_MODEL_COLOR),
        ];

        for (family, expected) in cases {
            assert_eq!(model_brand_color(family), expected, "family: {family:?}");
        }
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
        assert_eq!(
            client_brand_color(ClientId::OpenCode),
            Color::Rgb(0, 168, 232)
        );
        assert_eq!(client_brand_color(ClientId::Droid), Color::Rgb(31, 29, 28));
        assert_eq!(client_brand_color(ClientId::Mux), Color::Rgb(23, 23, 23));
        assert_eq!(client_brand_color(ClientId::Grok), Color::Rgb(23, 23, 23));
        assert_eq!(client_brand_color(ClientId::Zcode), Color::Rgb(17, 24, 39));
        assert_eq!(
            client_brand_color(ClientId::CommandCode),
            Color::Rgb(17, 24, 39)
        );
        assert_eq!(
            client_brand_color(ClientId::OpenClaw),
            Color::Rgb(239, 68, 68)
        );
    }

    #[test]
    fn every_catalog_client_color_is_parseable() {
        for client in ClientId::iter() {
            parse_catalog_color(client.color());
        }
    }

    #[test]
    fn identity_palette_adapts_dark_and_unknown_colors_for_its_surfaces() {
        let backgrounds = [
            Color::Rgb(10, 22, 38),
            Color::Rgb(13, 30, 50),
            Color::Rgb(17, 48, 82),
        ];
        let palette = IdentityPalette::resolve(backgrounds, Color::Rgb(224, 239, 255));
        let raw = client_brand_color(ClientId::Droid);
        let adjusted = palette.client(Some(ClientId::Droid));

        assert!(
            contrast_ratio(raw, backgrounds[0]) < WCAG_AA_TEXT_CONTRAST,
            "test fixture must begin below the policy threshold"
        );
        assert_ne!(adjusted, raw);
        for background in backgrounds {
            for color in [
                palette.model(ModelFamily::Qwen),
                adjusted,
                palette.client(None),
            ] {
                assert!(
                    contrast_ratio(color, background) >= WCAG_AA_TEXT_CONTRAST,
                    "{color:?} does not meet contrast on {background:?}"
                );
            }
        }
    }
}
