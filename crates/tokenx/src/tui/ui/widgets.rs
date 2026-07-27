use ratatui::widgets::{Cell, ScrollbarState};

pub(crate) use crate::formatting::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_tokens,
    format_tokens_with_commas, get_client_display_name, get_client_display_names,
    get_provider_display_name, truncate_display_width, truncate_model_display_name_to,
    workspace_label_or_unknown, MODEL_DISPLAY_MAX_WIDTH,
};
use crate::tui::themes::Theme;

pub(crate) fn total_tokens_cell(total_tokens: u64, theme: &Theme) -> Cell<'static> {
    Cell::from(format_tokens(total_tokens)).style(theme.metric_total_style())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokenx_engine::ClientId;

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
            ("z.ai", "Z.AI"),
            ("bigmodel.cn", "Z.AI"),
            ("open.bigmodel.cn", "Z.AI"),
            ("zhipuai-coding-plan", "Z.AI"),
            ("zhipu", "Z.AI"),
            ("xiaomi-token-plan-cn", "XiaoMi"),
            ("xiaomi-token-plan-sgp", "XiaoMi"),
            ("minimax-code-cn", "MiniMax"),
            ("minimax-cn-coding-plan", "MiniMax"),
            ("moonshotai", "Kimi"),
            ("moonshot-coding-plan", "Kimi"),
            ("moonshot-ai", "Kimi"),
            ("kimi", "Kimi"),
            ("kimi-code", "Kimi"),
            ("kimi-for-coding", "Kimi"),
            ("qwen", "Qwen"),
            ("qwen-portal", "Qwen"),
            ("qwen-coding-plan", "Qwen"),
            ("meituan", "Meituan"),
            ("longcat-coding-plan", "Meituan"),
            ("stepfun", "Stepfun"),
            ("stepfun_ai", "Stepfun"),
            ("stepfun-coding-plan", "Stepfun"),
            ("doubao-coding-plan", "Doubao"),
            ("qianfan", "Baidu"),
            ("baichuan-ai", "Baichuan"),
            ("01-ai", "01.AI"),
            ("zeroone", "01.AI"),
            ("alibaba-coding-plan-cn", "Alibaba"),
            ("tencent-coding-plan", "Tencent"),
            ("tecent-coding-plan", "Tencent"),
            ("xai-oauth", "xAI"),
            ("grok", "xAI"),
            ("grok-oauth", "xAI"),
            ("vertex", "Google"),
            ("vertex-ai", "Google"),
            ("google-gemini-cli", "Google"),
            ("google-antigravity", "Google"),
            ("azure", "Microsoft"),
            ("azure-ai", "Microsoft"),
            ("azure_ai", "Microsoft"),
            ("github-copilot", "Microsoft"),
            ("copilot-chat", "Microsoft"),
            ("openai-pro", "OpenAI"),
            ("openai-native", "OpenAI"),
            ("cline", "Cline"),
            ("openai-owl", "Owl"),
            ("openai-owlc", "Owl"),
            ("opencode", "OpenCode"),
            ("opencode-go", "OpenCode"),
            ("opencode-zen", "OpenCode"),
            ("openrouter", "OpenRouter"),
            ("openrouter/google", "OpenRouter"),
            ("commandcode", "Command"),
            ("command-code", "Command"),
            ("command_code", "Command"),
            ("unisound", "UniSound"),
            ("yunzhisheng", "UniSound"),
            ("ai21labs", "AI21"),
            ("perplexity", "Perplexity"),
            ("cohere", "Cohere"),
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
            get_provider_display_name("github-copilot, copilot-chat"),
            "Microsoft"
        );
        assert_eq!(
            get_provider_display_name("azure, azure-ai, azure_ai, github-copilot"),
            "Microsoft"
        );
        assert_eq!(
            get_provider_display_name("xai-oauth, grok, grok-oauth"),
            "xAI"
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
    fn client_display_uses_typed_catalog_identities() {
        assert_eq!(get_client_display_name(ClientId::OpenClaw), "OpenClaw");
        assert_eq!(
            get_client_display_names(&[ClientId::OpenCode, ClientId::Codex, ClientId::Kiro]),
            "OpenCode, Codex, Kiro"
        );
    }
}
