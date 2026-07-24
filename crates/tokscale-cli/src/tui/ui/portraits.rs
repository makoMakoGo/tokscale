//! Kaomoji portraits for the Overview snapshot's favorite model family:
//! one original artwork per family, painted in the family's brand color.

use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use crate::tui::colors;
use crate::tui::model_family::ModelFamily;

pub(super) fn display_name(family: ModelFamily) -> &'static str {
    match family {
        ModelFamily::Gpt => "gpt",
        ModelFamily::Claude => "claude",
        ModelFamily::Gemini => "gemini",
        ModelFamily::Xai => "xai",
        ModelFamily::Glm => "glm",
        ModelFamily::Deepseek => "deepseek",
        ModelFamily::Qwen => "qwen",
        ModelFamily::Kimi => "kimi",
        ModelFamily::Minimax => "minimax",
        ModelFamily::Mimo => "mimo",
        ModelFamily::Mistral => "mistral",
        ModelFamily::Unknown => "???",
    }
}

pub(super) fn slogan(family: ModelFamily) -> &'static str {
    match family {
        ModelFamily::Gpt => "最后还得找我~",
        ModelFamily::Claude => "You are absolutely right!",
        ModelFamily::Gemini => "你真是太棒了",
        ModelFamily::Xai => "最大限度求真",
        ModelFamily::Mimo => "我流口水",
        ModelFamily::Minimax => "我不爱刷榜",
        ModelFamily::Qwen => "我这次是真学会了",
        ModelFamily::Kimi => "我不是区",
        ModelFamily::Glm => "蒸馏之神,不解释",
        ModelFamily::Deepseek => "杂鱼 杂鱼",
        ModelFamily::Mistral => "风往哪吹？",
        ModelFamily::Unknown => "……",
    }
}

/// Fixed brand color per family (logo primary colors), adapted through the
/// shared identity-color path for surface contrast.
pub(super) fn family_color(app: &App, family: ModelFamily) -> Color {
    colors::resolve_family_color(family, &app.theme)
}

/// Overview card artwork uses one fixed three-row visual contract.
pub(super) const PORTRAIT_HEIGHT: usize = 3;
type Portrait = [&'static str; PORTRAIT_HEIGHT];

const GPT: Portrait = ["     ╲", "  (¬‿¬)╮", "   ⁄|~|⁄"];
const CLAUDE: Portrait = ["   ╭─ ✦ ─╮", "  (˶ᵔ ᵕ ᵔ˶)", "    /| |\\"];
const GEMINI: Portrait = ["  ✦    ✦", "  (◕‿◕)✦", "   /||\\"];
const XAI: Portrait = ["    𝕏", "  (¬‿¬)✕", "   /|\\"];
const GLM: Portrait = ["   ___", "  (⌐■_■)▤", "   /|  |\\"];
const DEEPSEEK: Portrait = ["  ～～～", " (｡•́︿•̀｡)", "   ～|～"];
const QWEN: Portrait = ["   ☁", "  (｡•̀ᴗ•́｡)☁", "   /|\\"];
const KIMI: Portrait = ["   ☾", "  (｡･ω･｡)☾", "   /|\\"];
const MINIMAX: Portrait = ["   /\\  /\\", "  (｡•̀ᴗ•́)◆", "   /|  |\\"];
const MIMO: Portrait = ["   ___", "  (｡•ω•｡)¤", "   /|  |\\"];
const MISTRAL: Portrait = ["  ≋≋≋", " (•̀ᴗ•́)≋", "   /|\\"];
const UNKNOWN: Portrait = ["  [■_■]", "  (•_•)", "   /|\\"];
const ROW_PADDING: &str = "                ";

pub(super) fn portrait(family: ModelFamily) -> [&'static str; PORTRAIT_HEIGHT] {
    match family {
        ModelFamily::Gpt => GPT,
        ModelFamily::Claude => CLAUDE,
        ModelFamily::Gemini => GEMINI,
        ModelFamily::Xai => XAI,
        ModelFamily::Glm => GLM,
        ModelFamily::Deepseek => DEEPSEEK,
        ModelFamily::Qwen => QWEN,
        ModelFamily::Kimi => KIMI,
        ModelFamily::Minimax => MINIMAX,
        ModelFamily::Mimo => MIMO,
        ModelFamily::Mistral => MISTRAL,
        ModelFamily::Unknown => UNKNOWN,
    }
}

/// Every line is padded on the right to the family block's width so the
/// artwork's authored left-edge alignment survives per-line centering
/// (left-padding each line independently was the misalignment bug).
pub(super) fn lines(app: &App, family: ModelFamily) -> [Line<'static>; PORTRAIT_HEIGHT] {
    styled_lines(family, family_color(app, family))
}

fn styled_lines(family: ModelFamily, color: Color) -> [Line<'static>; PORTRAIT_HEIGHT] {
    let art = portrait(family);
    let block_width = art
        .iter()
        .map(|row| UnicodeWidthStr::width(*row))
        .max()
        .unwrap_or(0);
    art.map(|row| {
        let padding_width = block_width - UnicodeWidthStr::width(row);
        Line::from(vec![
            Span::styled(row, Style::default().fg(color)),
            Span::raw(&ROW_PADDING[..padding_width]),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_lines_have_fixed_height_and_equal_display_width() {
        for family in [
            ModelFamily::Gpt,
            ModelFamily::Claude,
            ModelFamily::Gemini,
            ModelFamily::Xai,
            ModelFamily::Glm,
            ModelFamily::Deepseek,
            ModelFamily::Qwen,
            ModelFamily::Kimi,
            ModelFamily::Minimax,
            ModelFamily::Mimo,
            ModelFamily::Mistral,
            ModelFamily::Unknown,
        ] {
            let art = portrait(family);
            let width = art
                .iter()
                .map(|row| UnicodeWidthStr::width(*row))
                .max()
                .unwrap();
            assert!(width <= 16, "portrait too wide for the column: {width}");

            let lines = styled_lines(family, Color::White);
            assert_eq!(lines.len(), PORTRAIT_HEIGHT);
            assert!(lines.iter().all(|line| line.width() == width));
            assert!(
                lines.iter().all(|line| line.spans.len() == 2),
                "each static row should render as one styled span plus padding"
            );
        }

        assert!(
            portrait(ModelFamily::Qwen)
                .iter()
                .any(|row| row.chars().count() != UnicodeWidthStr::width(*row)),
            "fixture must retain a combining-mark row that exercises display width"
        );
    }
}
