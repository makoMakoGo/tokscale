//! Kaomoji portraits for the Overview snapshot's favorite model: one
//! original artwork per model family, colored through theme roles.

use ratatui::prelude::*;

use crate::tui::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Gpt,
    Claude,
    Gemini,
    Glm,
    Deepseek,
    Qwen,
    Kimi,
    Llama,
    Mistral,
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
    } else if model.contains("llama") {
        Family::Llama
    } else if model.contains("mistral") || model.contains("mixtral") {
        Family::Mistral
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

const GPT: &[&str] = &["      ✧", "   /\\_/\\", "  (｡♥‿♥｡)✧", "   /|⌨|\\"];
const CLAUDE: &[&str] = &["    ✧", "  ╭────╮ ✧", "  (｡•ᴗ•｡)", "   \\∪∪/"];
const GEMINI: &[&str] = &["  ✦    ✦", "  (◕‿◕)✦", "   /||\\"];
const GLM: &[&str] = &["   ___", "  (⌐■_■)▤", "   /|  |\\"];
const DEEPSEEK: &[&str] = &["  ～～～", " (｡•́︿•̀｡)", "   ～|～"];
const QWEN: &[&str] = &["   ☁", "  (｡•̀ᴗ•́｡)☁", "   /|\\"];
const KIMI: &[&str] = &["   ☾", "  (｡･ω･｡)☾", "   /|\\"];
const LLAMA: &[&str] = &["  (  )", "  (｡◕‿◕)♧", " /|    |\\"];
const MISTRAL: &[&str] = &["   ≈", "  (｡•̀ᴗ-)✧", "   /|\\"];
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
        Family::Llama => LLAMA,
        Family::Mistral => MISTRAL,
        Family::Unknown => UNKNOWN,
    }
}

/// Accessories take the accent color, faces and bodies stay in the theme
/// foreground, so the portraits follow every theme.
const ACCENT_CHARS: &[char] = &['✧', '✦', '☾', '☁', '⌨', '♧', '▤', '～', '≈', '■'];

pub(super) fn lines(app: &App, family: Family) -> Vec<Line<'static>> {
    portrait(family)
        .iter()
        .map(|row| {
            Line::from(
                row.chars()
                    .map(|ch| {
                        if ACCENT_CHARS.contains(&ch) {
                            Span::styled(ch.to_string(), Style::default().fg(app.theme.accent))
                        } else {
                            Span::styled(ch.to_string(), Style::default().fg(app.theme.foreground))
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(family_of("llama-4-scout"), Family::Llama);
        assert_eq!(family_of("mistral-large-3"), Family::Mistral);
        assert_eq!(family_of("some-future-model"), Family::Unknown);
    }
}
