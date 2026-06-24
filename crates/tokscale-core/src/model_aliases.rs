pub(crate) const DEEPSEEK_V4_PRO_BETA_ALIAS: &str = "model1";
pub(crate) const DEEPSEEK_V4_FLASH_BETA_ALIAS: &str = "model2";

const CLAUDE_FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

pub(crate) fn is_deepseek_v4_beta_alias(model: &str) -> bool {
    let lower = model.trim().to_lowercase();
    let model_part = lower
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(&lower);

    matches!(
        model_part,
        DEEPSEEK_V4_PRO_BETA_ALIAS | DEEPSEEK_V4_FLASH_BETA_ALIAS
    )
}

pub(crate) fn canonicalize_source_model_id(model: &str) -> Option<String> {
    let lower = model.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }

    canonicalize_modern_claude_source_model(&lower)
        .or_else(|| canonicalize_openai_source_model(&lower).map(str::to_string))
        .or_else(|| canonicalize_glm_source_model(&lower).map(str::to_string))
        .or_else(|| canonicalize_qwen_source_model(&lower))
        .or_else(|| canonicalize_kimi_source_model(&lower).map(str::to_string))
        .or_else(|| canonicalize_grok_source_model(&lower).map(str::to_string))
        .or_else(|| canonicalize_longcat_source_model(&lower).map(str::to_string))
}

fn canonical_model_segment(model: &str) -> &str {
    model
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(model)
}

fn canonicalize_openai_source_model(model: &str) -> Option<&'static str> {
    match canonical_model_segment(model) {
        "gpt-5.5-fast" => Some("gpt-5.5"),
        _ => None,
    }
}

fn canonicalize_glm_source_model(model: &str) -> Option<&'static str> {
    let model = canonical_model_segment(model);
    if matches!(model, "glm-4.7-free" | "glm-4.7:free" | "glm-4.7 (free)") {
        return Some("glm-4.7");
    }

    let base = model
        .strip_suffix("-high")
        .or_else(|| model.strip_suffix("-medium"))
        .or_else(|| model.strip_suffix("-fast"))
        .or_else(|| model.strip_suffix("-sub2api-pro"))
        .unwrap_or(model);

    if matches!(base, "glm-4.7-free" | "glm-4.7:free" | "glm-4.7 (free)") {
        Some("glm-4.7")
    } else {
        None
    }
}

fn canonicalize_qwen_source_model(model: &str) -> Option<String> {
    let model = canonical_model_segment(model);
    if !model.starts_with("qwen") {
        return None;
    }

    strip_qwen_date_suffix(model).map(str::to_string)
}

fn canonicalize_kimi_source_model(model: &str) -> Option<&'static str> {
    match canonical_model_segment(model) {
        "k2p5" | "k2-p5" | "kimi-for-coding/k2p5" | "kimi-for-coding/k2-p5" => Some("kimi-k2.5"),
        "k2p6" | "k2-p6" | "kimi-k2p6" | "kimi-for-coding/k2p6" | "kimi-for-coding/k2-p6" => {
            Some("kimi-k2.6")
        }
        "kimi-k2.5-thinking" => Some("kimi-k2-thinking"),
        "kimi-for-coding" => Some("kimi-k2.5"),
        "kimi-k2.5-nvfp4" | "kimi-k2-instruct-0905" => Some("kimi-k2.5"),
        _ => None,
    }
}

fn canonicalize_grok_source_model(model: &str) -> Option<&'static str> {
    match canonical_model_segment(model) {
        "grok-composer-2.5" => Some("composer-2.5"),
        "grok-composer-2.5-fast" => Some("composer-2.5-fast"),
        _ => None,
    }
}

pub(crate) fn canonicalize_longcat_source_model(model: &str) -> Option<&'static str> {
    if model == "longcat-flash-3b" {
        return Some("longcat-flash-3b");
    }

    model
        .strip_prefix("longcat-flash-3b-all-quant-")
        .filter(|suffix| !suffix.is_empty())
        .map(|_| "longcat-flash-3b")
}

fn canonicalize_modern_claude_source_model(model: &str) -> Option<String> {
    let model = canonical_model_segment(model);
    let model = model.strip_suffix("-thinking").unwrap_or(model);
    let parts: Vec<&str> = model
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect();

    for window in parts.windows(3) {
        if CLAUDE_FAMILIES.contains(&window[0])
            && is_modern_claude_major(window[1])
            && is_single_digit_minor(window[2])
        {
            return Some(format!("claude-{}-{}.{}", window[0], window[1], window[2]));
        }
        if is_modern_claude_major(window[0])
            && is_single_digit_minor(window[1])
            && CLAUDE_FAMILIES.contains(&window[2])
        {
            return Some(format!("claude-{}-{}.{}", window[2], window[0], window[1]));
        }
    }

    for (idx, part) in parts.iter().enumerate() {
        if !CLAUDE_FAMILIES.contains(part) {
            continue;
        }
        if let Some(major) = parts
            .get(idx + 1)
            .copied()
            .filter(|part| is_modern_claude_major(part))
        {
            let next_part = parts.get(idx + 2).copied();
            if next_part.is_none_or(is_compact_date) {
                return Some(format!("claude-{part}-{major}"));
            }
        }
        if idx >= 1
            && is_modern_claude_major(parts[idx - 1])
            && (idx < 2 || !parts[idx - 2].bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Some(format!("claude-{part}-{}", parts[idx - 1]));
        }
    }

    None
}

fn is_modern_claude_major(value: &str) -> bool {
    value.len() == 1 && value.as_bytes()[0].is_ascii_digit() && value.as_bytes()[0] >= b'4'
}

fn is_single_digit_minor(value: &str) -> bool {
    value.len() == 1 && value.as_bytes()[0].is_ascii_digit() && value.as_bytes()[0] != b'0'
}

fn is_compact_date(value: &str) -> bool {
    value.len() == 8 && value.starts_with("20") && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn strip_qwen_date_suffix(model: &str) -> Option<&str> {
    if model.len() > 11 {
        let potential_date = &model[model.len() - 10..];
        let bytes = potential_date.as_bytes();
        if bytes[4] == b'-'
            && bytes[7] == b'-'
            && potential_date
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
            && model.as_bytes()[model.len() - 11] == b'-'
        {
            return Some(&model[..model.len() - 11]);
        }
    }

    if model.len() > 9 {
        let potential_date = &model[model.len() - 8..];
        if potential_date.bytes().all(|byte| byte.is_ascii_digit())
            && model.as_bytes()[model.len() - 9] == b'-'
        {
            return Some(&model[..model.len() - 9]);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_modern_claude_source_models_to_dotted_ids() {
        let cases = [
            ("claude-opus-4-6", "claude-opus-4.6"),
            ("claude-opus-4.6", "claude-opus-4.6"),
            ("claude-opus-4-6-thinking", "claude-opus-4.6"),
            ("anthropic/claude-4-6-sonnet", "claude-sonnet-4.6"),
            ("anthropic/claude-4-5-haiku", "claude-haiku-4.5"),
            ("openrouter/anthropic/claude-4-6-opus", "claude-opus-4.6"),
            ("claude-sonnet-4-20250514", "claude-sonnet-4"),
            ("claude-fable-5", "claude-fable-5"),
            ("anthropic/claude-5-fable", "claude-fable-5"),
        ];

        for (raw, expected) in cases {
            assert_eq!(canonicalize_source_model_id(raw).as_deref(), Some(expected));
        }
    }

    #[test]
    fn does_not_canonicalize_legacy_claude_three_line() {
        assert_eq!(canonicalize_source_model_id("claude-3-5-sonnet"), None);
        assert_eq!(
            canonicalize_source_model_id("claude-3-5-sonnet-20241022"),
            None
        );
    }

    #[test]
    fn canonicalizes_source_specific_kimi_and_grok_ids() {
        assert_eq!(
            canonicalize_source_model_id("k2p5").as_deref(),
            Some("kimi-k2.5")
        );
        assert_eq!(
            canonicalize_source_model_id("k2p6").as_deref(),
            Some("kimi-k2.6")
        );
        assert_eq!(
            canonicalize_source_model_id("kimi-for-coding/k2p6").as_deref(),
            Some("kimi-k2.6")
        );
        assert_eq!(
            canonicalize_source_model_id("grok-composer-2.5-fast").as_deref(),
            Some("composer-2.5-fast")
        );
    }

    #[test]
    fn canonicalizes_source_specific_openai_glm_and_qwen_ids() {
        let cases = [
            ("gpt-5.5-fast", "gpt-5.5"),
            ("openai/gpt-5.5-fast", "gpt-5.5"),
            ("glm-4.7-free", "glm-4.7"),
            ("glm-4.7:free-fast", "glm-4.7"),
            ("glm-4.7 (free)-medium", "glm-4.7"),
            ("opencode/glm-4.7-free-sub2api-pro", "glm-4.7"),
            ("qwen3.7-max-2026-05-20", "qwen3.7-max"),
            ("qwen/qwen3.7-max-20260520", "qwen3.7-max"),
        ];

        for (raw, expected) in cases {
            assert_eq!(canonicalize_source_model_id(raw).as_deref(), Some(expected));
        }

        assert_eq!(canonicalize_source_model_id("qwen3.7-max-2605"), None);
        assert_eq!(canonicalize_source_model_id("qwen3.7-max-05-20"), None);
        assert_eq!(
            canonicalize_source_model_id("gpt-5.1-codex-max-xhigh"),
            None
        );
    }
}
