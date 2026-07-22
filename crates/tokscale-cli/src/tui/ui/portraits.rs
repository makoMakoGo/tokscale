//! Kaomoji portraits for the Overview snapshot's favorite model family:
//! one original artwork per family, painted in the family's brand color.

use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use crate::tui::data::OverviewFamily;

pub(super) fn display_name(family: OverviewFamily) -> &'static str {
    match family {
        OverviewFamily::Gpt => "gpt",
        OverviewFamily::Claude => "claude",
        OverviewFamily::Gemini => "gemini",
        OverviewFamily::Glm => "glm",
        OverviewFamily::Deepseek => "deepseek",
        OverviewFamily::Qwen => "qwen",
        OverviewFamily::Kimi => "kimi",
        OverviewFamily::Minimax => "minimax",
        OverviewFamily::Mimo => "mimo",
        OverviewFamily::Unknown => "???",
    }
}

pub(super) fn slogan(family: OverviewFamily) -> &'static str {
    match family {
        OverviewFamily::Gpt => "最后还得找我~",
        OverviewFamily::Claude => "You are absolutely right!",
        OverviewFamily::Gemini => "你真是太棒了",
        OverviewFamily::Mimo => "我流口水",
        OverviewFamily::Minimax => "我不爱刷榜",
        OverviewFamily::Qwen => "我这次是真学会了",
        OverviewFamily::Kimi => "我不是区",
        OverviewFamily::Glm => "蒸馏之神,不解释",
        OverviewFamily::Deepseek => "杂鱼 杂鱼",
        OverviewFamily::Unknown => "……",
    }
}

/// Fixed brand color per family (logo primary colors), run through the
/// theme's color-mode mapping so legacy terminals degrade gracefully.
pub(super) fn family_color(app: &App, family: OverviewFamily) -> Color {
    let color = match family {
        OverviewFamily::Gpt => Color::Rgb(16, 163, 127),
        OverviewFamily::Claude => Color::Rgb(217, 119, 87),
        OverviewFamily::Gemini => Color::Rgb(142, 124, 240),
        OverviewFamily::Glm => Color::Rgb(232, 232, 232),
        OverviewFamily::Deepseek => Color::Rgb(77, 107, 254),
        OverviewFamily::Qwen => Color::Rgb(97, 92, 237),
        OverviewFamily::Kimi => Color::Rgb(192, 192, 192),
        OverviewFamily::Minimax => Color::Rgb(228, 58, 58),
        OverviewFamily::Mimo => Color::Rgb(255, 105, 0),
        OverviewFamily::Unknown => Color::Gray,
    };
    app.theme.color(color)
}

/// Overview card artwork uses one fixed three-row visual contract.
pub(super) const PORTRAIT_HEIGHT: usize = 3;
type Portrait = [&'static str; PORTRAIT_HEIGHT];

const GPT: Portrait = ["     ╲", "  (¬‿¬)╮", "   ⁄|~|⁄"];
const CLAUDE: Portrait = ["  ╭────╮ ✧", "  (｡•ᴗ•｡)", "   \\∪∪/"];
const GEMINI: Portrait = ["  ✦    ✦", "  (◕‿◕)✦", "   /||\\"];
const GLM: Portrait = ["   ___", "  (⌐■_■)▤", "   /|  |\\"];
const DEEPSEEK: Portrait = ["  ～～～", " (｡•́︿•̀｡)", "   ～|～"];
const QWEN: Portrait = ["   ☁", "  (｡•̀ᴗ•́｡)☁", "   /|\\"];
const KIMI: Portrait = ["   ☾", "  (｡･ω･｡)☾", "   /|\\"];
const MINIMAX: Portrait = ["   /\\  /\\", "  (｡•̀ᴗ•́)◆", "   /|  |\\"];
const MIMO: Portrait = ["   ___", "  (｡•ω•｡)¤", "   /|  |\\"];
const UNKNOWN: Portrait = ["  [■_■]", "  (•_•)", "   /|\\"];
const ROW_PADDING: &str = "                ";

pub(super) fn portrait(family: OverviewFamily) -> [&'static str; PORTRAIT_HEIGHT] {
    match family {
        OverviewFamily::Gpt => GPT,
        OverviewFamily::Claude => CLAUDE,
        OverviewFamily::Gemini => GEMINI,
        OverviewFamily::Glm => GLM,
        OverviewFamily::Deepseek => DEEPSEEK,
        OverviewFamily::Qwen => QWEN,
        OverviewFamily::Kimi => KIMI,
        OverviewFamily::Minimax => MINIMAX,
        OverviewFamily::Mimo => MIMO,
        OverviewFamily::Unknown => UNKNOWN,
    }
}

/// Every line is padded on the right to the family block's width so the
/// artwork's authored left-edge alignment survives per-line centering
/// (left-padding each line independently was the misalignment bug).
pub(super) fn lines(app: &App, family: OverviewFamily) -> [Line<'static>; PORTRAIT_HEIGHT] {
    styled_lines(family, family_color(app, family))
}

fn styled_lines(family: OverviewFamily, color: Color) -> [Line<'static>; PORTRAIT_HEIGHT] {
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
            OverviewFamily::Gpt,
            OverviewFamily::Claude,
            OverviewFamily::Gemini,
            OverviewFamily::Glm,
            OverviewFamily::Deepseek,
            OverviewFamily::Qwen,
            OverviewFamily::Kimi,
            OverviewFamily::Minimax,
            OverviewFamily::Mimo,
            OverviewFamily::Unknown,
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
            portrait(OverviewFamily::Qwen)
                .iter()
                .any(|row| row.chars().count() != UnicodeWidthStr::width(*row)),
            "fixture must retain a combining-mark row that exercises display width"
        );
    }
}
