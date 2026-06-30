use ratatui::widgets::ScrollbarState;
use tokscale_core::{normalize_provider_for_grouping, ClientId};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::client_ui;
use crate::tui::config::TokscaleConfig;

pub fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000_000 {
        format!("{:.1}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        format_tokens_with_commas(tokens)
    }
}

pub fn format_tokens(tokens: u64) -> String {
    format_tokens_compact(tokens)
}

pub fn format_tokens_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

pub fn format_cost(cost: f64) -> String {
    if !cost.is_finite() || cost < 0.0 {
        return "$0.00".to_string();
    }
    if cost >= 1000.0 {
        format!("${:.1}K", cost / 1000.0)
    } else {
        format!("${:.2}", cost)
    }
}

pub fn format_cost_per_million(cost: f64, total_tokens: u64) -> String {
    if total_tokens == 0 || !cost.is_finite() || cost < 0.0 {
        return "\u{2014}".to_string();
    }

    let per_million = cost / (total_tokens as f64) * 1_000_000.0;
    format!("${:.2}", per_million)
}

/// Cache reuse multiplier: cached reads per full-price input token.
/// `cache_read / (input + cache_write)` — how many low-cost reads you
/// got for every token you paid full price (fresh input or cache write).
pub fn format_cache_hit_rate(cache_read: u64, input: u64, cache_write: u64) -> String {
    let paid = input.saturating_add(cache_write);
    if paid == 0 {
        return if cache_read > 0 {
            "∞".to_string()
        } else {
            "—".to_string()
        };
    }
    let ratio = cache_read as f64 / paid as f64;
    format!("{:.1}x", ratio)
}

pub fn format_ms_per_1k(ms_per_1k_tokens: Option<f64>) -> String {
    let Some(value) = ms_per_1k_tokens else {
        return "—".to_string();
    };
    if !value.is_finite() || value <= 0.0 {
        "—".to_string()
    } else if value >= 1000.0 {
        format!("{:.1}s", value / 1000.0)
    } else {
        format!("{:.0}ms", value)
    }
}

pub fn viewport_scrollbar_state(
    content_len: usize,
    scroll_offset: usize,
    viewport_len: usize,
) -> ScrollbarState {
    let viewport_len = viewport_len.max(1);
    ScrollbarState::new(content_len)
        .position(scrollbar_position(scroll_offset, content_len, viewport_len))
        .viewport_content_length(viewport_len)
}

fn scrollbar_position(scroll_offset: usize, content_len: usize, viewport_len: usize) -> usize {
    let max_scroll = content_len.saturating_sub(viewport_len);
    if max_scroll == 0 {
        0
    } else {
        ((scroll_offset.min(max_scroll) as u128) * (content_len.saturating_sub(1) as u128)
            / (max_scroll as u128)) as usize
    }
}

pub(crate) const MODEL_DISPLAY_MAX_WIDTH: usize = 29;

fn char_display_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

pub(crate) fn truncate_display_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    if s.width() <= max_width {
        return s.to_string();
    }

    let ellipsis = "...";
    let ellipsis_width = ellipsis.width();
    if max_width <= ellipsis_width {
        return s
            .chars()
            .scan(0usize, |width, ch| {
                let next_width = *width + char_display_width(ch);
                if next_width > max_width {
                    None
                } else {
                    *width = next_width;
                    Some(ch)
                }
            })
            .collect();
    }

    let head_width = max_width - ellipsis_width;
    let head: String = s
        .chars()
        .scan(0usize, |width, ch| {
            let next_width = *width + char_display_width(ch);
            if next_width > head_width {
                None
            } else {
                *width = next_width;
                Some(ch)
            }
        })
        .collect();
    format!("{}{}", head, ellipsis)
}

pub(crate) fn truncate_model_display_name(model: &str) -> String {
    truncate_display_width(model, MODEL_DISPLAY_MAX_WIDTH)
}

pub(crate) fn truncate_model_display_name_to(model: &str, max_width: usize) -> String {
    truncate_display_width(model, max_width)
}

pub(crate) fn get_client_display_name(client: &str) -> String {
    display_comma_list(client, get_single_client_display_name)
}

fn get_single_client_display_name(client: &str) -> String {
    let config = TokscaleConfig::load();
    if let Some(name) = config.get_client_display_name(client) {
        return name.to_string();
    }
    let client_lower = client.to_lowercase();
    if let Some(client_id) = ClientId::from_str(&client_lower) {
        return client_ui::display_name(client_id).to_string();
    }
    client.to_string()
}

pub(crate) fn get_provider_display_name(provider: &str) -> String {
    display_comma_list(provider, get_single_provider_display_name)
}

fn get_single_provider_display_name(provider: &str) -> String {
    let config = TokscaleConfig::load();
    if let Some(name) = config.get_provider_display_name(provider) {
        return name.to_string();
    }
    if is_aws_provider_for_display(provider) {
        return "AWS".to_string();
    }
    match normalize_provider_for_grouping(provider).as_str() {
        "anthropic" => "Anthropic".to_string(),
        "openai" => "OpenAI".to_string(),
        "google" => "Google".to_string(),
        "cursor" => "Cursor".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "zai" => "Z.AI".to_string(),
        "xiaomi" => "XiaoMi".to_string(),
        "minimax" => "MiniMax".to_string(),
        "kimi" => "Kimi".to_string(),
        "qwen" => "Qwen".to_string(),
        "meituan" => "Meituan".to_string(),
        "stepfun" => "Stepfun".to_string(),
        "doubao" => "Doubao".to_string(),
        "alibaba" => "Alibaba".to_string(),
        "tencent" => "Tencent".to_string(),
        "xai" => "xAI".to_string(),
        "meta" => "Meta".to_string(),
        "mistral" => "Mistral".to_string(),
        "cohere" => "Cohere".to_string(),
        "opencode" => "OpenCode".to_string(),
        "owl" => "Owl".to_string(),
        "github-copilot" => "GitHub Copilot".to_string(),
        "commandcode" => "Command".to_string(),
        "unisound" => "UniSound".to_string(),
        _ => provider.to_string(),
    }
}

fn is_aws_provider_for_display(provider: &str) -> bool {
    provider
        .to_lowercase()
        .split('/')
        .any(is_aws_provider_segment_for_display)
}

fn is_aws_provider_segment_for_display(segment: &str) -> bool {
    matches!(segment, "amazon" | "aws" | "bedrock")
        || segment.starts_with("amazon-")
        || segment.starts_with("amazon_")
        || segment.starts_with("aws-")
        || segment.starts_with("aws_")
        || segment.starts_with("bedrock-")
        || segment.starts_with("bedrock_")
        || segment.ends_with("-bedrock")
        || segment.ends_with("_bedrock")
}

fn display_comma_list<F>(value: &str, format_segment: F) -> String
where
    F: Fn(&str) -> String,
{
    if !value.contains(',') {
        return format_segment(value);
    }

    let mut labels = Vec::new();
    for segment in value.split(',') {
        let label = format_segment(segment.trim());
        if !labels.iter().any(|existing| existing == &label) {
            labels.push(label);
        }
    }

    labels.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollbar_position_maps_bottom_offset_to_last_position() {
        assert_eq!(scrollbar_position(15, 20, 5), 19);
    }

    #[test]
    fn scrollbar_position_keeps_top_at_zero() {
        assert_eq!(scrollbar_position(0, 20, 5), 0);
    }

    #[test]
    fn scrollbar_position_clamps_overscroll_to_bottom() {
        assert_eq!(scrollbar_position(999, 20, 5), 19);
    }

    #[test]
    fn scrollbar_position_single_page_stays_at_zero() {
        assert_eq!(scrollbar_position(0, 5, 10), 0);
    }

    #[test]
    fn scrollbar_position_uses_wide_math_for_large_lengths() {
        let content_len = usize::MAX;
        let viewport_len = 2;
        let max_scroll = content_len - viewport_len;

        assert_eq!(scrollbar_position(0, content_len, viewport_len), 0);
        assert_eq!(
            scrollbar_position(max_scroll, content_len, viewport_len),
            usize::MAX - 1
        );
        assert_eq!(
            scrollbar_position(usize::MAX, content_len, viewport_len),
            usize::MAX - 1
        );
    }

    #[test]
    fn viewport_scrollbar_state_handles_zero_viewport() {
        let state = viewport_scrollbar_state(20, 5, 0);

        assert_eq!(
            state,
            ScrollbarState::new(20)
                .position(5)
                .viewport_content_length(1)
        );
    }

    #[test]
    fn provider_display_formats_each_segment_in_merged_list() {
        assert_eq!(
            get_provider_display_name("openai, openai-codex, amazon-bedrock"),
            "OpenAI, AWS"
        );
    }

    #[test]
    fn provider_display_formats_coding_plan_aliases() {
        let cases = [
            ("zai", "Z.AI"),
            ("zai-coding-plan", "Z.AI"),
            ("zai-coding-pln", "Z.AI"),
            ("zhipuai-coding-plan", "Z.AI"),
            ("zhipu", "Z.AI"),
            ("xiaomi-token-plan-cn", "XiaoMi"),
            ("xiaomi-token-plan-sgp", "XiaoMi"),
            ("minimax-code-cn", "MiniMax"),
            ("minimax-cn-coding-plan", "MiniMax"),
            ("moonshotai", "Kimi"),
            ("moonshot-coding-plan", "Kimi"),
            ("kimi", "Kimi"),
            ("kimi-code", "Kimi"),
            ("kimi-for-coding", "Kimi"),
            ("qwen", "Qwen"),
            ("qwen-coding-plan", "Qwen"),
            ("meituan", "Meituan"),
            ("longcat-coding-plan", "Meituan"),
            ("stepfun", "Stepfun"),
            ("stepfun_ai", "Stepfun"),
            ("stepfun-coding-plan", "Stepfun"),
            ("doubao-coding-plan", "Doubao"),
            ("alibaba-coding-plan-cn", "Alibaba"),
            ("tencent-coding-plan", "Tencent"),
            ("tecent-coding-plan", "Tencent"),
            ("openai-pro", "OpenAI"),
            ("openai-owl", "Owl"),
            ("openai-owlc", "Owl"),
            ("opencode", "OpenCode"),
            ("opencode-go", "OpenCode"),
            ("opencode-zen", "OpenCode"),
            ("commandcode", "Command"),
            ("command-code", "Command"),
            ("command_code", "Command"),
            ("unisound", "UniSound"),
            ("yunzhisheng", "UniSound"),
            ("amazon", "AWS"),
            ("aws", "AWS"),
            ("bedrock", "AWS"),
            ("amazon-bedrock", "AWS"),
            ("anthropic-bedrock", "AWS"),
            ("bedrock/anthropic", "AWS"),
            ("openrouter/amazon", "AWS"),
            ("not-aws", "not-aws"),
            ("awesome-provider", "awesome-provider"),
        ];

        for (provider, expected) in cases {
            assert_eq!(get_provider_display_name(provider), expected);
        }
    }

    #[test]
    fn provider_display_dedups_after_formatting_merged_aliases() {
        assert_eq!(
            get_provider_display_name("xiaomi, xiaomi-token-plan-cn, xiaomi-token-plan-sgp"),
            "XiaoMi"
        );
        assert_eq!(
            get_provider_display_name("zai, zhipuai-coding-plan, minimax-code-cn"),
            "Z.AI, MiniMax"
        );
        assert_eq!(
            get_provider_display_name("moonshotai, kimi-for-coding"),
            "Kimi"
        );
        assert_eq!(
            get_provider_display_name("opencode, opencode-go, opencode-zen"),
            "OpenCode"
        );
        assert_eq!(
            get_provider_display_name("anthropic-bedrock, amazon-bedrock, aws"),
            "AWS"
        );
    }

    #[test]
    fn client_display_formats_each_segment_in_merged_list() {
        assert_eq!(get_client_display_name("openclaw"), "OpenClaw");
        assert_eq!(
            get_client_display_name("opencode, codex, kiro, unknown-client"),
            "OpenCode, Codex, Kiro, unknown-client"
        );
    }
}
