//! Kaomoji portraits for the Overview snapshot's favorite model family:
//! one original artwork per family, painted in the family's brand color.

use ratatui::prelude::*;

use crate::tui::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Family {
    Gpt,
    Claude,
    Gemini,
    Glm,
    Deepseek,
    Qwen,
    Kimi,
    Minimax,
    Mimo,
    Unknown,
}

pub(super) fn family_of(model_id: &str) -> Family {
    let model = model_id.to_ascii_lowercase();
    if model.contains("claude") {
        Family::Claude
    } else if model.contains("gemini") {
        Family::Gemini
    } else if model.contains("glm") || model.contains("chatglm") {
        Family::Glm
    } else if model.contains("deepseek") {
        Family::Deepseek
    } else if model.contains("qwen") {
        Family::Qwen
    } else if model.contains("kimi") || model.contains("moonshot") {
        Family::Kimi
    } else if model.contains("minimax") {
        Family::Minimax
    } else if model.contains("mimo") {
        Family::Mimo
    } else if model.contains("gpt")
        || model.contains("codex")
        || model.contains("openai")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        Family::Gpt
    } else {
        Family::Unknown
    }
}

pub(super) fn display_name(family: Family) -> &'static str {
    match family {
        Family::Gpt => "gpt",
        Family::Claude => "claude",
        Family::Gemini => "gemini",
        Family::Glm => "glm",
        Family::Deepseek => "deepseek",
        Family::Qwen => "qwen",
        Family::Kimi => "kimi",
        Family::Minimax => "minimax",
        Family::Mimo => "mimo",
        Family::Unknown => "???",
    }
}

/// Fixed brand color per family (logo primary colors), run through the
/// theme's color-mode mapping so legacy terminals degrade gracefully.
pub(super) fn family_color(app: &App, family: Family) -> Color {
    let color = match family {
        Family::Gpt => Color::Rgb(16, 163, 127),
        Family::Claude => Color::Rgb(217, 119, 87),
        Family::Gemini => Color::Rgb(142, 124, 240),
        Family::Glm => Color::Rgb(232, 232, 232),
        Family::Deepseek => Color::Rgb(77, 107, 254),
        Family::Qwen => Color::Rgb(97, 92, 237),
        Family::Kimi => Color::Rgb(192, 192, 192),
        Family::Minimax => Color::Rgb(228, 58, 58),
        Family::Mimo => Color::Rgb(255, 105, 0),
        Family::Unknown => Color::Gray,
    };
    app.theme.color(color)
}

const GPT: &[&str] = &["     ╲", "  (¬‿¬)╮", "   ⁄|~|⁄"];
const CLAUDE: &[&str] = &["    ✧", "  ╭────╮ ✧", "  (｡•ᴗ•｡)", "   \\∪∪/"];
const GEMINI: &[&str] = &["  ✦    ✦", "  (◕‿◕)✦", "   /||\\"];
const GLM: &[&str] = &["   ___", "  (⌐■_■)▤", "   /|  |\\"];
const DEEPSEEK: &[&str] = &["  ～～～", " (｡•́︿•̀｡)", "   ～|～"];
const QWEN: &[&str] = &["   ☁", "  (｡•̀ᴗ•́｡)☁", "   /|\\"];
const KIMI: &[&str] = &["   ☾", "  (｡･ω･｡)☾", "   /|\\"];
const MINIMAX: &[&str] = &["   /\\  /\\", "  (｡•̀ᴗ•́)◆", "   /|  |\\"];
const MIMO: &[&str] = &["   ___", "  (｡•ω•｡)¤", "   /|  |\\"];
const UNKNOWN: &[&str] = &["  [■_■]", "  (•_•)", "   /|\\"];

pub(super) fn portrait(family: Family) -> &'static [&'static str] {
    match family {
        Family::Gpt => GPT,
        Family::Claude => CLAUDE,
        Family::Gemini => GEMINI,
        Family::Glm => GLM,
        Family::Deepseek => DEEPSEEK,
        Family::Qwen => QWEN,
        Family::Kimi => KIMI,
        Family::Minimax => MINIMAX,
        Family::Mimo => MIMO,
        Family::Unknown => UNKNOWN,
    }
}

/// Every line is padded on the right to the family block's width so the
/// artwork's authored left-edge alignment survives per-line centering
/// (left-padding each line independently was the misalignment bug).
pub(super) fn lines(app: &App, family: Family) -> Vec<Line<'static>> {
    let art = portrait(family);
    let block_width = art.iter().map(|row| row.chars().count()).max().unwrap_or(0);
    let color = family_color(app, family);
    art.iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = row
                .chars()
                .map(|ch| Span::styled(ch.to_string(), Style::default().fg(color)))
                .collect();
            spans.push(Span::raw(" ".repeat(block_width - row.chars().count())));
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_lines_share_one_block_width_per_family() {
        for family in [
            Family::Gpt,
            Family::Claude,
            Family::Gemini,
            Family::Glm,
            Family::Deepseek,
            Family::Qwen,
            Family::Kimi,
            Family::Minimax,
            Family::Mimo,
            Family::Unknown,
        ] {
            let art = portrait(family);
            let width = art.iter().map(|row| row.chars().count()).max().unwrap();
            assert!(
                art.iter().all(|row| row.chars().count() <= width),
                "portrait rows must fit the block width"
            );
            assert!(width <= 16, "portrait too wide for the column: {width}");
        }
    }

    #[test]
    fn family_detection_covers_the_major_model_ids() {
        assert_eq!(family_of("gpt-5.5"), Family::Gpt);
        assert_eq!(family_of("codex-mini-latest"), Family::Gpt);
        assert_eq!(family_of("o3"), Family::Gpt);
        assert_eq!(family_of("claude-opus-4-7"), Family::Claude);
        assert_eq!(family_of("gemini-2.5-pro"), Family::Gemini);
        assert_eq!(family_of("glm-4.6"), Family::Glm);
        assert_eq!(family_of("deepseek-v3.2"), Family::Deepseek);
        assert_eq!(family_of("qwen3-coder-plus"), Family::Qwen);
        assert_eq!(family_of("kimi-k2"), Family::Kimi);
        assert_eq!(family_of("minimax-m3"), Family::Minimax);
        assert_eq!(family_of("mimo-v2.5-pro"), Family::Mimo);
        assert_eq!(family_of("llama-4-scout"), Family::Unknown);
        assert_eq!(family_of("mistral-large-3"), Family::Unknown);
    }
}
