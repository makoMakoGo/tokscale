fn canonicalize_provider_segment(segment: &str) -> Option<String> {
    let trimmed = segment.trim().trim_end_matches('/');
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        return None;
    }

    if let Some(canonical) = exact_canonical_provider(trimmed) {
        return Some(canonical.into());
    }

    let normalized = normalized_provider_key(trimmed);

    if let Some(canonical) = exact_canonical_provider(normalized.as_str()) {
        return Some(canonical.into());
    }

    // For unknown segments, reject if they contain digits — those are almost
    // certainly model-name fragments (e.g., "gpt-4", "claude-3") rather than
    // provider identifiers.
    if normalized.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let canonical = match normalized.as_str() {
        "" | "unknown" => return None,
        s if s.starts_with("xai") || s.starts_with("grok") || s == "supergrok" => "xai",
        s if s.starts_with("zai")
            || s.starts_with("z_ai")
            || s.starts_with("zhipu")
            || s.starts_with("bigmodel")
            || s.starts_with("open_bigmodel") =>
        {
            "zai"
        }
        s if s.starts_with("xiaomi") => "xiaomi",
        s if s.starts_with("minimax") => "minimax",
        s if s.starts_with("moonshot") => "kimi",
        s if s.starts_with("qwen") => "qwen",
        s if s.starts_with("meituan") || s.starts_with("longcat") => "meituan",
        s if s.contains("stepfun") => "stepfun",
        s if s.starts_with("doubao") => "doubao",
        s if s.starts_with("hunyuan") => "tencent",
        s if s.starts_with("baichuan") => "baichuan",
        s if s.starts_with("alibaba") => "alibaba",
        s if s.starts_with("tencent") || s.starts_with("tecent") => "tencent",
        s if s.starts_with("baidu") || s.starts_with("qianfan") || s.starts_with("wenxin") => {
            "baidu"
        }
        s if s == "opencode" || s.starts_with("opencode_") => "opencode",
        s if s.starts_with("github_cop") || s.contains("copilot") => "microsoft",
        s if s.starts_with("unisound")
            || s.starts_with("uni_sound")
            || s.starts_with("yunzhisheng")
            || s.starts_with("yun_zhi_sheng") =>
        {
            "unisound"
        }
        _ => return None,
    };

    Some(canonical.into())
}

pub fn canonical_provider(raw: &str) -> Option<String> {
    for segment in raw.trim().trim_end_matches('/').split('/') {
        let segment = segment.trim().trim_end_matches('/');
        if let Some(tag) = canonicalize_provider_segment(segment) {
            return Some(tag);
        }

        if segment.contains('.') {
            for dotted in segment.split('.') {
                if let Some(tag) = canonicalize_provider_segment(dotted) {
                    return Some(tag);
                }
            }
        }
    }

    None
}

pub fn normalize_provider_for_grouping(raw: &str) -> String {
    let trimmed = raw.trim();
    if is_owl_usage_provider(trimmed) {
        return "owl".to_string();
    }

    match canonical_provider(trimmed) {
        Some(provider) => provider,
        None => literal_provider_key(trimmed),
    }
}

pub(crate) fn finalized_provider_id(raw_provider: &str, model_id: &str) -> String {
    let raw_provider = raw_provider.trim();
    if is_owl_usage_provider(raw_provider) {
        return "owl".to_string();
    }

    canonical_provider(raw_provider)
        .or_else(|| first_literal_provider_tag(raw_provider))
        .or_else(|| inferred_provider_from_model(model_id).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Resolve optional provider attribution while a usage record is being
/// parsed. A non-empty observed value is preserved because it may encode a
/// router or reseller path; absent attribution is inferred from the model and
/// otherwise represented as `unknown`. Provider resolution must not decide
/// whether an otherwise valid usage record exists.
pub(crate) fn observed_provider_id(raw_provider: &str, model_id: &str) -> String {
    let raw_provider = raw_provider.trim();
    if !raw_provider.is_empty()
        && !raw_provider.eq_ignore_ascii_case("unknown")
        && !(raw_provider.starts_with('<') && raw_provider.ends_with('>'))
    {
        return raw_provider.to_string();
    }

    inferred_provider_from_model(model_id)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalized_provider_key(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_ascii() {
        let bytes = trimmed.as_bytes();
        let needs_normalization = bytes.iter().any(|byte| {
            byte.is_ascii_uppercase()
                || *byte == b'-'
                || *byte == b'.'
                || byte.is_ascii_whitespace()
        });
        if !needs_normalization {
            return trimmed.to_string();
        }

        let mut normalized = String::with_capacity(trimmed.len());
        for byte in bytes {
            let mapped = if *byte == b'-' || *byte == b'.' || byte.is_ascii_whitespace() {
                b'_'
            } else {
                byte.to_ascii_lowercase()
            };
            normalized.push(mapped as char);
        }
        return normalized;
    }

    trimmed
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch == '-' || ch == '.' || ch.is_ascii_whitespace() {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

fn exact_canonical_provider(normalized: &str) -> Option<&'static str> {
    let canonical = match normalized {
        "" | "unknown" => return None,
        "x_ai" | "xai" | "xai_oauth" | "grok" | "grok_oauth" | "grok_cli" | "supergrok" => "xai",
        "z_ai" | "zai" | "zhipu" | "zhipuai" | "zhipu_ai" | "zhipu_coding_plan" | "bigmodel"
        | "bigmodel_cn" | "open_bigmodel_cn" => "zai",
        "moonshot"
        | "moonshotai"
        | "moonshot_ai"
        | "moonshot_coding_plan"
        | "kimi"
        | "kimi_code"
        | "kimi_for_coding"
        | "kimi_coding_plan" => "kimi",
        "xiaomi" | "mimo" => "xiaomi",
        "meituan" | "longcat" => "meituan",
        "doubao" => "doubao",
        "tencent" | "tecent" | "tencent_cloud" | "hunyuan" | "hy3" | "hy3_preview" => "tencent",
        "baidu" | "qianfan" | "wenxin" => "baidu",
        "baichuan" | "baichuan_ai" => "baichuan",
        "01ai" | "01_ai" | "zeroone" | "zero_one" | "zero_one_ai" | "lingyiwanwu" => "01-ai",
        "meta" | "meta_llama" => "meta",
        "microsoft" => "microsoft",
        "azure" | "azure_ai" => "microsoft",
        "anthropic" => "anthropic",
        "together" | "together_ai" => "together",
        "fireworks" | "fireworks_ai" => "fireworks",
        "google" | "gemini" | "vertex" | "vertex_ai" | "google_vertex" | "google_gemini_cli"
        | "google_antigravity" => "google",
        "openai" | "openai_codex" | "openai_native" | "openai_pro" | "chatgpt" => "openai",
        "opencode" => "opencode",
        "openrouter" => "openrouter",
        "bedrock" => "bedrock",
        "aws" | "amazon" => "aws",
        "groq" => "groq",
        "cohere" => "cohere",
        "perplexity" => "perplexity",
        "github_copilot" | "copilot_chat" => "microsoft",
        "commandcode" | "command_code" => "commandcode",
        "unisound" | "uni_sound" | "yunzhisheng" | "yun_zhi_sheng" => "unisound",
        "minimax" | "minimaxai" | "minimax_ai" => "minimax",
        "mistral" | "mistralai" => "mistral",
        "pandora_deepseek" | "deepseek_ai" => "deepseek",
        "qwen" | "qwen_portal" => "qwen",
        "ai21" | "ai21labs" | "ai21_labs" => "ai21",
        _ => return None,
    };

    Some(canonical)
}

fn literal_provider_key(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_ascii() {
        let bytes = trimmed.as_bytes();
        if !bytes.iter().any(|byte| byte.is_ascii_uppercase()) {
            return trimmed.to_string();
        }

        let mut lowered = String::with_capacity(trimmed.len());
        for byte in bytes {
            lowered.push(byte.to_ascii_lowercase() as char);
        }
        return lowered;
    }

    trimmed.to_lowercase()
}

fn literal_provider_tag(segment: &str) -> Option<String> {
    let trimmed = segment.trim().trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("unknown")
        || (trimmed.starts_with('<') && trimmed.ends_with('>'))
        || trimmed.chars().any(|ch| ch.is_ascii_digit())
    {
        return None;
    }

    Some(literal_provider_key(trimmed))
}

fn first_literal_provider_tag(raw: &str) -> Option<String> {
    for segment in raw.trim().trim_end_matches('/').split('/') {
        let segment = segment.trim().trim_end_matches('/');
        if canonicalize_provider_segment(segment).is_some() {
            continue;
        }

        if segment.contains('.')
            && segment
                .split('.')
                .any(|dotted| canonicalize_provider_segment(dotted).is_some())
        {
            continue;
        }

        if let Some(tag) = literal_provider_tag(segment) {
            return Some(tag);
        }
    }

    None
}

pub(crate) fn is_owl_usage_provider(raw: &str) -> bool {
    let trimmed = raw.trim();
    contains_delimited_ignore_ascii_case(trimmed, "owl")
        || contains_delimited_ignore_ascii_case(trimmed, "owlc")
}

pub fn provider_tags(raw: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut push_tag = |tag: String| {
        if !tags.iter().any(|existing| existing == &tag) {
            tags.push(tag);
        }
    };

    for segment in raw.trim().trim_end_matches('/').split('/') {
        let segment = segment.trim().trim_end_matches('/');
        if let Some(tag) = canonicalize_provider_segment(segment) {
            push_tag(tag);
            continue;
        }

        if segment.contains('.') {
            let mut found_dotted_tag = false;
            for dotted in segment.split('.') {
                if let Some(tag) = canonicalize_provider_segment(dotted) {
                    push_tag(tag);
                    found_dotted_tag = true;
                }
            }
            if found_dotted_tag {
                continue;
            }
        }

        if let Some(tag) = literal_provider_tag(segment) {
            push_tag(tag);
        }
    }

    tags
}

pub fn key_provider_tags(dataset_key: &str) -> Vec<String> {
    let key_parts: Vec<&str> = dataset_key.split('/').collect();
    if key_parts.len() < 2 {
        return Vec::new();
    }

    let mut tags = Vec::new();
    let mut push_all = |value: &str| {
        for tag in provider_tags(value) {
            if !tags.iter().any(|existing| existing == &tag) {
                tags.push(tag);
            }
        }
    };

    for segment in &key_parts[..key_parts.len() - 1] {
        push_all(segment);
    }
    for dotted in key_parts[key_parts.len() - 1].split('.') {
        push_all(dotted);
    }

    tags
}

pub fn matches_provider_hint(dataset_key: &str, provider_id: Option<&str>) -> bool {
    let Some(provider_id) = provider_id else {
        return false;
    };

    let hint_tags = provider_tags(provider_id);
    matches_provider_hint_with_tags(dataset_key, &hint_tags)
}

pub fn matches_provider_hint_with_tags(dataset_key: &str, hint_tags: &[String]) -> bool {
    if hint_tags.is_empty() {
        return false;
    }

    let key_tags = key_provider_tags(dataset_key);
    if key_tags.is_empty() {
        return false;
    }

    key_tags
        .iter()
        .any(|key_tag| hint_tags.iter().any(|hint_tag| hint_tag == key_tag))
}

fn contains_delimited(haystack: &str, needle: &str) -> bool {
    for (pos, _) in haystack.match_indices(needle) {
        let before_ok = pos == 0 || !haystack.as_bytes()[pos - 1].is_ascii_alphanumeric();
        let after_pos = pos + needle.len();
        let after_ok =
            after_pos == haystack.len() || !haystack.as_bytes()[after_pos].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn contains_delimited_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }

    for pos in 0..=haystack.len() - needle.len() {
        if !haystack[pos..pos + needle.len()].eq_ignore_ascii_case(needle) {
            continue;
        }

        let before_ok = pos == 0 || !haystack[pos - 1].is_ascii_alphanumeric();
        let after_pos = pos + needle.len();
        let after_ok = after_pos == haystack.len() || !haystack[after_pos].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }

    false
}

pub(crate) fn provider_override_from_model(model: &str) -> Option<&'static str> {
    if crate::model_aliases::is_deepseek_v4_beta_alias(model) {
        Some("deepseek")
    } else {
        None
    }
}

pub(crate) fn provider_override_from_model_and_provider(
    model: &str,
    provider: &str,
) -> Option<&'static str> {
    if let Some(provider) = provider_override_from_model(model) {
        return Some(provider);
    }

    if canonical_provider(provider).as_deref() == Some("commandcode") {
        return Some("commandcode");
    }

    if canonical_provider(provider).as_deref() == Some("anthropic") && !is_anthropic_model(model) {
        return inferred_provider_from_model(model)
            .filter(|provider| *provider != "anthropic")
            .or(Some("unknown"));
    }

    None
}

pub fn inferred_provider_from_model(model: &str) -> Option<&'static str> {
    let lower = model.to_lowercase();
    let model_part = lower
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(&lower);

    if let Some(provider) = provider_override_from_model(model) {
        return Some(provider);
    }

    if model_part == "u2" {
        return Some("unisound");
    }

    if lower.contains("glm")
        || lower.contains("zhipu")
        || lower.contains("z-ai")
        || lower.contains("z.ai")
    {
        return Some("zai");
    }

    if lower.contains("mimo") || lower.contains("xiaomi") {
        return Some("xiaomi");
    }

    if lower.contains("kimi")
        || lower.contains("moonshot")
        || matches_model_family(model_part, "k3")
    {
        return Some("kimi");
    }

    if lower.contains("longcat") || lower.contains("meituan") {
        return Some("meituan");
    }

    if contains_delimited(&lower, "hunyuan") || contains_delimited(&lower, "hy3") {
        return Some("tencent");
    }

    if contains_delimited(&lower, "doubao") {
        return Some("doubao");
    }

    if contains_delimited(&lower, "ernie")
        || contains_delimited(&lower, "qianfan")
        || contains_delimited(&lower, "wenxin")
    {
        return Some("baidu");
    }

    if model_part.starts_with("baichuan") || contains_delimited(&lower, "baichuan") {
        return Some("baichuan");
    }

    if model_part.starts_with("yi-")
        || model_part.starts_with("yi_")
        || contains_delimited(&lower, "01-ai")
        || contains_delimited(&lower, "01.ai")
        || contains_delimited(&lower, "01_ai")
    {
        return Some("01-ai");
    }

    if is_anthropic_model(model) {
        return Some("anthropic");
    }

    if lower.contains("gpt")
        || lower.contains("openai")
        || contains_delimited(&lower, "codex")
        || lower.contains("text-embedding")
        || lower.contains("dall-e")
        || lower.contains("whisper")
        || contains_delimited(&lower, "tts")
        || contains_delimited(&lower, "o1")
        || contains_delimited(&lower, "o3")
        || contains_delimited(&lower, "o4")
    {
        return Some("openai");
    }

    if lower.contains("gemini") || lower.contains("google") {
        return Some("google");
    }

    if lower.contains("grok") || matches_model_family(model_part, "composer") {
        return Some("xai");
    }

    if lower.contains("deepseek") {
        return Some("deepseek");
    }

    if lower.contains("minimax") {
        return Some("minimax");
    }

    if contains_delimited(&lower, "cohere")
        || is_cohere_command_model_part(model_part)
        || matches_model_family(model_part, "c4ai-aya")
    {
        return Some("cohere");
    }

    if contains_delimited(&lower, "jamba")
        || model_part == "jamba"
        || model_part.starts_with("jamba-")
        || contains_delimited(&lower, "ai21")
        || model_part.starts_with("j2-")
        || model_part.starts_with("jurassic-")
    {
        return Some("ai21");
    }

    if contains_delimited(&lower, "perplexity")
        || model_part == "sonar"
        || model_part.starts_with("sonar-")
    {
        return Some("perplexity");
    }

    if lower.contains("mistral") || lower.contains("mixtral") {
        return Some("mistral");
    }

    if lower.contains("llama") || contains_delimited(&lower, "meta") {
        return Some("meta");
    }

    if lower.contains("qwen")
        || model_part == "qwq"
        || model_part.starts_with("qwq-")
        || model_part == "qvq"
        || model_part.starts_with("qvq-")
    {
        return Some("qwen");
    }

    if lower.contains("stepfun")
        || matches_model_family(model_part, "step-1")
        || matches_model_family(model_part, "step-2")
        || matches_model_family(model_part, "step-3")
    {
        return Some("stepfun");
    }

    None
}

fn is_cohere_command_model_part(model_part: &str) -> bool {
    matches_command_family(model_part, "command-r", true)
        || matches_command_family(model_part, "command-a", false)
        || matches_command_family(model_part, "command-light", false)
        || matches_command_family(model_part, "command-nightly", false)
        || matches_command_family(model_part, "command-xlarge", false)
        || matches_command_family(model_part, "command-medium", false)
}

fn matches_command_family(model_part: &str, family: &str, allow_digit_suffix: bool) -> bool {
    if model_part == family {
        return true;
    }

    let Some(suffix) = model_part.strip_prefix(family) else {
        return false;
    };

    matches_family_suffix(suffix)
        || (allow_digit_suffix && suffix.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
}

fn matches_model_family(model_part: &str, family: &str) -> bool {
    if model_part == family {
        return true;
    }

    model_part
        .strip_prefix(family)
        .is_some_and(matches_family_suffix)
}

fn matches_family_suffix(suffix: &str) -> bool {
    suffix.starts_with('-') || suffix.starts_with('.') || suffix.starts_with('_')
}

pub fn is_anthropic_model(model: &str) -> bool {
    let lower = model.trim().to_lowercase();
    let model_part = lower
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(&lower);
    let anthropic_model_part = model_part
        .strip_prefix("anthropic.")
        .or_else(|| model_part.split_once(".anthropic.").map(|(_, model)| model))
        .unwrap_or(model_part);

    anthropic_model_part.starts_with("claude-")
        || anthropic_model_part.starts_with("opus-")
        || anthropic_model_part.starts_with("sonnet-")
        || anthropic_model_part.starts_with("haiku-")
        || anthropic_model_part.starts_with("fable-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_provider_resolution_never_gates_usage_identity() {
        assert_eq!(
            observed_provider_id("bedrock/anthropic", "claude-opus-4.6"),
            "bedrock/anthropic"
        );
        assert_eq!(observed_provider_id("", "gpt-5.5"), "openai");
        assert_eq!(observed_provider_id("", "k3"), "kimi");
        assert_eq!(
            observed_provider_id("unknown", "claude-opus-4.6"),
            "anthropic"
        );
        assert_eq!(observed_provider_id("", "private-preview"), "unknown");
    }

    #[test]
    fn test_provider_tags_normalize_known_aliases() {
        let cases = [
            ("openai-codex", vec!["openai"]),
            ("gemini", vec!["google"]),
            ("vertex", vec!["google"]),
            ("vertex-ai", vec!["google"]),
            ("google-gemini-cli", vec!["google"]),
            ("google-antigravity", vec!["google"]),
            ("azure", vec!["microsoft"]),
            ("azure-ai", vec!["microsoft"]),
            ("azure_ai", vec!["microsoft"]),
            ("microsoft", vec!["microsoft"]),
            ("fireworks", vec!["fireworks"]),
            ("fireworks-ai", vec!["fireworks"]),
            ("together", vec!["together"]),
            ("together-ai", vec!["together"]),
            ("Meta-Llama", vec!["meta"]),
            ("MistralAI", vec!["mistral"]),
            ("MiniMax", vec!["minimax"]),
            ("Kimi", vec!["kimi"]),
            ("kimi-for-coding", vec!["kimi"]),
            ("moonshotai", vec!["kimi"]),
            ("moonshot-ai", vec!["kimi"]),
            ("Xiaomi", vec!["xiaomi"]),
            ("LongCat", vec!["meituan"]),
            ("hy3", vec!["tencent"]),
            ("Hunyuan", vec!["tencent"]),
            ("Qianfan", vec!["baidu"]),
            ("Baichuan-AI", vec!["baichuan"]),
            ("01-ai", vec!["01-ai"]),
            ("zeroone", vec!["01-ai"]),
            ("AI21Labs", vec!["ai21"]),
            ("xai-oauth", vec!["xai"]),
            ("grok", vec!["xai"]),
            ("grok oauth", vec!["xai"]),
            ("z.ai", vec!["zai"]),
            ("bigmodel.cn", vec!["zai"]),
            ("open.bigmodel.cn", vec!["zai"]),
            ("stepfun_ai", vec!["stepfun"]),
            ("stepfun-coding-plan", vec!["stepfun"]),
            ("opencode-go", vec!["opencode"]),
            ("opencode-zen", vec!["opencode"]),
            ("openai-pro", vec!["openai"]),
            ("command-code", vec!["commandcode"]),
            ("command_code", vec!["commandcode"]),
            ("UniSound", vec!["unisound"]),
            ("yunzhisheng", vec!["unisound"]),
            ("openrouter/google", vec!["openrouter", "google"]),
            ("bedrock/anthropic", vec!["bedrock", "anthropic"]),
            ("venice", vec!["venice"]),
            ("anthropic-bedrock", vec!["anthropic-bedrock"]),
        ];

        for (raw, expected) in cases {
            assert_eq!(provider_tags(raw), expected);
        }
    }

    #[test]
    fn test_canonical_provider_returns_first_canonical_tag() {
        assert_eq!(canonical_provider("openai-codex"), Some("openai".into()));
        assert_eq!(
            canonical_provider("openrouter/google"),
            Some("openrouter".into())
        );
        assert_eq!(
            canonical_provider("pandora-deepseek"),
            Some("deepseek".into())
        );
        assert_eq!(canonical_provider("UniSound"), Some("unisound".into()));
        assert_eq!(canonical_provider("yunzhisheng"), Some("unisound".into()));
        assert_eq!(canonical_provider("openai-pro"), Some("openai".into()));
        assert_eq!(canonical_provider("<synthetic>"), None);
        assert_eq!(canonical_provider("unknown"), None);
    }

    #[test]
    fn test_normalize_provider_for_grouping_collapses_display_aliases() {
        let cases = [
            ("zai", "zai"),
            ("zhipuai-coding-plan", "zai"),
            ("xiaomi-token-plan-cn", "xiaomi"),
            ("minimax-code-cn", "minimax"),
            ("moonshotai", "kimi"),
            ("moonshot-coding-plan", "kimi"),
            ("moonshot-ai", "kimi"),
            ("kimi-for-coding", "kimi"),
            ("xai-oauth", "xai"),
            ("grok", "xai"),
            ("grok-oauth", "xai"),
            ("z.ai", "zai"),
            ("bigmodel.cn", "zai"),
            ("open.bigmodel.cn", "zai"),
            ("pandora-deepseek", "deepseek"),
            ("deepseek-ai", "deepseek"),
            ("deepseek_ai", "deepseek"),
            ("qwen-portal", "qwen"),
            ("qwen-coding-plan", "qwen"),
            ("meituan", "meituan"),
            ("longcat-coding-plan", "meituan"),
            ("hy3", "tencent"),
            ("hunyuan-api", "tencent"),
            ("qianfan", "baidu"),
            ("baichuan-ai", "baichuan"),
            ("01-ai", "01-ai"),
            ("zero-one-ai", "01-ai"),
            ("ai21labs", "ai21"),
            ("stepfun_ai", "stepfun"),
            ("stepfun-coding-plan", "stepfun"),
            ("doubao-coding-plan", "doubao"),
            ("alibaba-coding-plan-cn", "alibaba"),
            ("tecent-coding-plan", "tencent"),
            ("vertex", "google"),
            ("vertex-ai", "google"),
            ("google-gemini-cli", "google"),
            ("google-antigravity", "google"),
            ("azure", "microsoft"),
            ("azure-ai", "microsoft"),
            ("azure_ai", "microsoft"),
            ("microsoft", "microsoft"),
            ("github-copilot", "microsoft"),
            ("copilot-chat", "microsoft"),
            ("Anthropic", "anthropic"),
            ("OpenAI-Codex", "openai"),
            ("openai-native", "openai"),
            ("Gemini", "google"),
            ("MistralAI", "mistral"),
            ("Meta-Llama", "meta"),
            ("fireworks-ai", "fireworks"),
            ("together_ai", "together"),
            ("openai-pro", "openai"),
            ("openai_owl", "owl"),
            ("openai-owl", "owl"),
            ("openai-owlc", "owl"),
            ("foo/owl/bar", "owl"),
            ("provider.owlc", "owl"),
            ("bowl", "bowl"),
            ("scowl", "scowl"),
            ("owlish", "owlish"),
            ("owlc2", "owlc2"),
            ("opencode", "opencode"),
            ("opencode-go", "opencode"),
            ("opencode-zen", "opencode"),
            ("command-code", "commandcode"),
            ("command_code", "commandcode"),
            ("UniSound", "unisound"),
            ("yunzhisheng", "unisound"),
            ("Anthropic-Bedrock", "anthropic-bedrock"),
        ];

        for (raw, expected) in cases {
            assert_eq!(normalize_provider_for_grouping(raw), expected);
        }
    }

    #[test]
    fn test_key_provider_tags_extract_nested_provider_segments() {
        assert_eq!(
            key_provider_tags("openrouter/google/gemini-3-pro-preview"),
            vec!["openrouter", "google"]
        );
        assert_eq!(
            key_provider_tags("bedrock/anthropic.claude-sonnet-4"),
            vec!["bedrock", "anthropic"]
        );
    }

    #[test]
    fn test_matches_provider_hint_for_known_aliases_and_nested_keys() {
        assert!(matches_provider_hint(
            "openai/gpt-5.2-preview",
            Some("openai-codex")
        ));
        assert!(matches_provider_hint(
            "openrouter/google/gemini-3-pro-preview",
            Some("google")
        ));
        assert!(matches_provider_hint(
            "azure/openai/gpt-4",
            Some("microsoft")
        ));
        assert!(matches_provider_hint(
            "fireworks_ai/deepseek-v3-0324",
            Some("fireworks")
        ));
        assert!(!matches_provider_hint("openai/gpt-4", Some("anthropic")));
    }

    #[test]
    fn test_inferred_provider_from_model_recognizes_stepfun() {
        assert_eq!(inferred_provider_from_model("stepfun-v2"), Some("stepfun"));
    }

    #[test]
    fn test_inferred_provider_from_model() {
        assert_eq!(
            inferred_provider_from_model("claude-sonnet-4"),
            Some("anthropic")
        );
        assert_eq!(inferred_provider_from_model("opus-4.5"), Some("anthropic"));
        assert_eq!(inferred_provider_from_model("sonnet-4"), Some("anthropic"));
        assert_eq!(inferred_provider_from_model("haiku-3"), Some("anthropic"));
        assert_eq!(inferred_provider_from_model("fable-5"), Some("anthropic"));
        assert_eq!(
            inferred_provider_from_model("anthropic.fable-5"),
            Some("anthropic")
        );
        assert_eq!(inferred_provider_from_model("gpt-5.2"), Some("openai"));
        assert_eq!(inferred_provider_from_model("gpt-5.5"), Some("openai"));
        assert_eq!(
            inferred_provider_from_model("gemini-2.5-pro"),
            Some("google")
        );
        assert_eq!(
            inferred_provider_from_model("grok-code-fast-1"),
            Some("xai")
        );
        assert_eq!(inferred_provider_from_model("composer-2.5"), Some("xai"));
        assert_eq!(
            inferred_provider_from_model("deepseek-v3"),
            Some("deepseek")
        );
        assert_eq!(inferred_provider_from_model("model1"), Some("deepseek"));
        assert_eq!(
            inferred_provider_from_model("deepseek/model2"),
            Some("deepseek")
        );
        assert_eq!(inferred_provider_from_model("glm-5.1"), Some("zai"));
        assert_eq!(
            inferred_provider_from_model("anthropic/glm-5.1"),
            Some("zai")
        );
        assert_eq!(
            inferred_provider_from_model("mimo-v2.5-pro"),
            Some("xiaomi")
        );
        assert_eq!(
            inferred_provider_from_model("xiaomi/mimo-v2.5-pro"),
            Some("xiaomi")
        );
        assert_eq!(
            inferred_provider_from_model("kimi-for-coding"),
            Some("kimi")
        );
        assert_eq!(
            inferred_provider_from_model("longcat-flash-thinking"),
            Some("meituan")
        );
        assert_eq!(
            inferred_provider_from_model("hy3-preview-agent"),
            Some("tencent")
        );
        assert_eq!(
            inferred_provider_from_model("tencent/Hy3-preview"),
            Some("tencent")
        );
        assert_eq!(
            inferred_provider_from_model("hunyuan-a13b-instruct"),
            Some("tencent")
        );
        assert_eq!(
            inferred_provider_from_model("doubao-seed-2-0-pro"),
            Some("doubao")
        );
        assert_eq!(
            inferred_provider_from_model("ERNIE-4.5-300B-A47B"),
            Some("baidu")
        );
        assert_eq!(
            inferred_provider_from_model("baichuan4-turbo"),
            Some("baichuan")
        );
        assert_eq!(inferred_provider_from_model("yi-large"), Some("01-ai"));
        assert_eq!(
            inferred_provider_from_model("01-ai/yi-34b-chat"),
            Some("01-ai")
        );
        assert_eq!(
            inferred_provider_from_model("MiniMax-M2.1"),
            Some("minimax")
        );
        assert_eq!(
            inferred_provider_from_model("command-r-plus"),
            Some("cohere")
        );
        assert_eq!(
            inferred_provider_from_model("command-r7b-12-2024"),
            Some("cohere")
        );
        assert_eq!(
            inferred_provider_from_model("command-a-03-2025"),
            Some("cohere")
        );
        assert_eq!(
            inferred_provider_from_model("c4ai-aya-expanse-32b"),
            Some("cohere")
        );
        assert_eq!(
            inferred_provider_from_model("jamba-1.5-large"),
            Some("ai21")
        );
        assert_eq!(inferred_provider_from_model("j2-ultra"), Some("ai21"));
        assert_eq!(inferred_provider_from_model("jurassic-2-en"), Some("ai21"));
        assert_eq!(
            inferred_provider_from_model("sonar-pro"),
            Some("perplexity")
        );
        assert_eq!(
            inferred_provider_from_model("mixtral-8x7b"),
            Some("mistral")
        );
        assert_eq!(
            inferred_provider_from_model("mistral-large"),
            Some("mistral")
        );
        assert_eq!(inferred_provider_from_model("llama-3"), Some("meta"));
        assert_eq!(inferred_provider_from_model("qwen3-coder"), Some("qwen"));
        assert_eq!(inferred_provider_from_model("qwq-32b"), Some("qwen"));
        assert_eq!(
            inferred_provider_from_model("qvq-72b-preview"),
            Some("qwen")
        );
        assert_eq!(
            inferred_provider_from_model("step-3.7-flash"),
            Some("stepfun")
        );
        assert_eq!(inferred_provider_from_model("u2"), Some("unisound"));
        assert_eq!(
            inferred_provider_from_model("unisound/u2"),
            Some("unisound")
        );
        assert_eq!(inferred_provider_from_model("foo/u2"), Some("unisound"));
        assert_eq!(
            inferred_provider_from_model("codex-mini-latest"),
            Some("openai")
        );
        assert_eq!(
            inferred_provider_from_model("text-embedding-3-small"),
            Some("openai")
        );
        assert_eq!(inferred_provider_from_model("dall-e-3"), Some("openai"));
        assert_eq!(inferred_provider_from_model("whisper-1"), Some("openai"));
        assert_eq!(inferred_provider_from_model("tts-1"), Some("openai"));
        assert_eq!(
            inferred_provider_from_model("gpt-4o-mini-tts"),
            Some("openai")
        );
        assert_eq!(
            inferred_provider_from_model("anthropic.claude-sonnet-4"),
            Some("anthropic")
        );
        assert_eq!(
            inferred_provider_from_model("bedrock/anthropic.claude-sonnet-4"),
            Some("anthropic")
        );
        assert_eq!(
            inferred_provider_from_model("us.anthropic.claude-3-5-sonnet-20241022-v1:0"),
            Some("anthropic")
        );
        assert_eq!(inferred_provider_from_model("unknown-model"), None);
    }

    #[test]
    fn test_provider_override_from_exact_historical_model_aliases() {
        assert_eq!(provider_override_from_model("model1"), Some("deepseek"));
        assert_eq!(
            provider_override_from_model("deepseek/model2"),
            Some("deepseek")
        );
        assert_eq!(
            provider_override_from_model("anthropic/model1"),
            Some("deepseek")
        );
        assert_eq!(provider_override_from_model("model10"), None);
        assert_eq!(provider_override_from_model("my-model1"), None);
    }

    #[test]
    fn test_provider_override_rejects_anthropic_for_non_anthropic_models() {
        assert_eq!(
            provider_override_from_model_and_provider("glm-5.1", "anthropic"),
            Some("zai")
        );
        assert_eq!(
            provider_override_from_model_and_provider("mimo-v2.5-pro", "anthropic"),
            Some("xiaomi")
        );
        assert_eq!(
            provider_override_from_model_and_provider("model1", "some-reseller"),
            Some("deepseek")
        );
        assert_eq!(
            provider_override_from_model_and_provider("unknown-model", "anthropic"),
            Some("unknown")
        );
        assert_eq!(
            provider_override_from_model_and_provider("claude-opus-4.5", "anthropic"),
            None
        );
        assert_eq!(
            provider_override_from_model_and_provider("glm-5.1", "openrouter"),
            None
        );
        assert_eq!(
            provider_override_from_model_and_provider("qwen3.7-max", "command-code"),
            Some("commandcode")
        );
    }

    #[test]
    fn test_inferred_provider_no_false_positives() {
        assert_eq!(inferred_provider_from_model("protocol1-fast"), None);
        assert_eq!(inferred_provider_from_model("proto3-server"), None);
        assert_eq!(inferred_provider_from_model("co4pilot-v2"), None);
        assert_eq!(inferred_provider_from_model("metadata-model"), None);
        assert_eq!(inferred_provider_from_model("metamorphic-v1"), None);
        assert_eq!(inferred_provider_from_model("model10"), None);
        assert_eq!(inferred_provider_from_model("my-model1"), None);
        assert_eq!(inferred_provider_from_model("foo-u2"), None);
        assert_eq!(inferred_provider_from_model("u20"), None);
        assert_eq!(inferred_provider_from_model("codexchange"), None);
        assert_eq!(inferred_provider_from_model("mitts-model"), None);
        assert_eq!(inferred_provider_from_model("pickle-model"), None);
        assert_eq!(inferred_provider_from_model("big-pickle"), None);
        assert_eq!(inferred_provider_from_model("hy30-preview"), None);
        assert_eq!(inferred_provider_from_model("seed-2-0-pro"), None);
        assert_eq!(inferred_provider_from_model("myi-large"), None);
        assert_eq!(inferred_provider_from_model("bernie-4"), None);
        assert_eq!(inferred_provider_from_model("notcohere-model"), None);
        assert_eq!(inferred_provider_from_model("notperplexity-model"), None);
        assert_eq!(inferred_provider_from_model("jambalaya-model"), None);
        assert_eq!(inferred_provider_from_model("notjamba-model"), None);
        assert_eq!(inferred_provider_from_model("command-code"), None);
        assert_eq!(inferred_provider_from_model("command-agent"), None);
        assert_eq!(inferred_provider_from_model("command-router"), None);
        assert_eq!(inferred_provider_from_model("command-lightyear"), None);
        assert_eq!(inferred_provider_from_model("c4ai-ayaya-32b"), None);
        assert_eq!(inferred_provider_from_model("step-10-preview"), None);
        assert_eq!(inferred_provider_from_model("step-by-step"), None);
        assert_eq!(inferred_provider_from_model("sonarqube-model"), None);
        assert_eq!(
            inferred_provider_from_model("notanthropic.claude-sonnet-4"),
            None
        );
    }

    #[test]
    fn test_inferred_provider_boundary_matches() {
        assert_eq!(inferred_provider_from_model("o1-preview"), Some("openai"));
        assert_eq!(inferred_provider_from_model("o3-mini"), Some("openai"));
        assert_eq!(inferred_provider_from_model("o4-mini"), Some("openai"));
        assert_eq!(inferred_provider_from_model("meta-llama-3"), Some("meta"));
    }

    #[test]
    fn test_provider_tags_mistral_alias() {
        assert_eq!(provider_tags("mistral"), vec!["mistral"]);
        assert_eq!(provider_tags("mistralai"), vec!["mistral"]);
    }

    #[test]
    fn test_matches_provider_hint_mistral_keys() {
        assert!(matches_provider_hint(
            "mistralai/mistral-large",
            Some("mistral")
        ));
        assert!(matches_provider_hint(
            "mistralai/mixtral-8x7b",
            Some("mistralai")
        ));
    }

    #[test]
    fn test_provider_tags_ai21_with_digits() {
        assert_eq!(provider_tags("ai21"), vec!["ai21"]);
    }

    #[test]
    fn test_matches_provider_hint_none_and_empty() {
        assert!(!matches_provider_hint("openai/gpt-4", None));
        assert!(!matches_provider_hint("openai/gpt-4", Some("")));
        assert!(!matches_provider_hint("openai/gpt-4", Some("unknown")));
    }

    #[test]
    fn test_custom_provider_tags_are_literal_without_canonical_identity() {
        // Common provider labels canonicalize as usual.
        assert_eq!(canonical_provider("anthropic"), Some("anthropic".into()));
        assert_eq!(canonical_provider("openai"), Some("openai".into()));
        assert_eq!(canonical_provider("openai-codex"), Some("openai".into()));
        assert_eq!(canonical_provider("google"), Some("google".into()));
        assert_eq!(canonical_provider("microsoft"), Some("microsoft".into()));
        assert_eq!(canonical_provider("azure"), Some("microsoft".into()));
        assert_eq!(canonical_provider("azure_ai"), Some("microsoft".into()));
        assert_eq!(canonical_provider("fireworks_ai"), Some("fireworks".into()));
        assert_eq!(canonical_provider("together_ai"), Some("together".into()));
        assert_eq!(canonical_provider("meta_llama"), Some("meta".into()));
        assert_eq!(canonical_provider("mistralai"), Some("mistral".into()));
        assert_eq!(
            canonical_provider("github-copilot"),
            Some("microsoft".into())
        );
        assert_eq!(
            canonical_provider("github_copilot"),
            Some("microsoft".into())
        );

        assert_eq!(canonical_provider("venice"), None);
        assert_eq!(provider_tags("venice"), vec!["venice"]);
        assert_eq!(
            provider_tags("Anthropic-Bedrock"),
            vec!["anthropic-bedrock"]
        );

        // A provider value that looks like a model fragment (contains digits)
        // or a placeholder is not treated as a provider.
        assert_eq!(canonical_provider("tool-local-model-4o"), None);
        assert!(provider_tags("tool-local-model-4o").is_empty());
        assert_eq!(canonical_provider("<unset>"), None);
        assert!(provider_tags("<unset>").is_empty());
    }
}
