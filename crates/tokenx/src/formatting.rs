use tokenx_engine::{normalize_provider_for_grouping, ClientId};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn format_tokens_compact(tokens: u64) -> String {
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

pub(crate) fn format_tokens(tokens: u64) -> String {
    format_tokens_compact(tokens)
}

pub(crate) fn format_tokens_with_commas(n: u64) -> String {
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

pub(crate) fn format_cost(cost: f64) -> String {
    if !cost.is_finite() || cost < 0.0 {
        return "$0.00".to_string();
    }
    if cost >= 1000.0 {
        format!("${:.1}K", cost / 1000.0)
    } else {
        format!("${:.2}", cost)
    }
}

pub(crate) fn format_cost_per_million(cost: f64, total_tokens: u64) -> String {
    if total_tokens == 0 || !cost.is_finite() || cost < 0.0 {
        return "\u{2014}".to_string();
    }

    let per_million = cost / (total_tokens as f64) * 1_000_000.0;
    format!("${:.2}", per_million)
}

/// Cache reuse multiplier: cached reads per full-price input token.
///
/// `cache_read / (input + cache_write)` measures how many low-cost reads were
/// obtained for each token paid at full price.
pub(crate) fn format_cache_hit_rate(cache_read: u64, input: u64, cache_write: u64) -> String {
    let paid = input
        .checked_add(cache_write)
        .expect("cache reuse denominator exceeds u64::MAX");
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

pub(crate) fn get_client_display_name(client: ClientId) -> String {
    client.display_name().to_string()
}

pub(crate) fn get_client_display_names(clients: &[ClientId]) -> String {
    clients
        .iter()
        .map(|client| get_client_display_name(*client))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn workspace_label_or_unknown(workspace: Option<&str>) -> &str {
    workspace.unwrap_or("Unknown workspace")
}

pub(crate) fn get_provider_display_name(provider: &str) -> String {
    display_comma_list(provider, get_single_provider_display_name)
}

fn get_single_provider_display_name(provider: &str) -> String {
    if is_aws_provider_for_display(provider) {
        return "AWS".to_string();
    }
    match normalize_provider_for_grouping(provider).as_str() {
        "anthropic" => "Anthropic".to_string(),
        "openai" => "OpenAI".to_string(),
        "google" => "Google".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "zai" => "Z.AI".to_string(),
        "xiaomi" => "XiaoMi".to_string(),
        "minimax" => "MiniMax".to_string(),
        "kimi" => "Kimi".to_string(),
        "qwen" => "Qwen".to_string(),
        "meituan" => "Meituan".to_string(),
        "stepfun" => "Stepfun".to_string(),
        "doubao" => "Doubao".to_string(),
        "baidu" => "Baidu".to_string(),
        "baichuan" => "Baichuan".to_string(),
        "01-ai" => "01.AI".to_string(),
        "alibaba" => "Alibaba".to_string(),
        "tencent" => "Tencent".to_string(),
        "xai" => "xAI".to_string(),
        "meta" => "Meta".to_string(),
        "mistral" => "Mistral".to_string(),
        "cohere" => "Cohere".to_string(),
        "ai21" => "AI21".to_string(),
        "perplexity" => "Perplexity".to_string(),
        "microsoft" => "Microsoft".to_string(),
        "cline" => "Cline".to_string(),
        "opencode" => "OpenCode".to_string(),
        "openrouter" => "OpenRouter".to_string(),
        "owl" => "Owl".to_string(),
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
