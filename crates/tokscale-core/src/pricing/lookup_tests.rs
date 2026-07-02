use super::*;

/// Mock LiteLLM data matching real API responses for OpenCode Zen models
fn mock_litellm() -> HashMap<String, ModelPricing> {
    let mut m = HashMap::new();

    // === GPT-4 models (baseline) ===
    m.insert(
        "gpt-4o".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000025),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(0.00000125),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-4o-mini".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000015),
            output_cost_per_token: Some(0.0000006),
            cache_read_input_token_cost: Some(0.000000075),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-4-turbo".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00003),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // === OpenCode Zen: GPT-5 family ===
    m.insert(
        "gpt-5.2".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000175),
            output_cost_per_token: Some(0.000014),
            cache_read_input_token_cost: Some(1.75e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5.5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            input_cost_per_token_above_272k_tokens: Some(0.000010),
            output_cost_per_token: Some(0.000030),
            output_cost_per_token_above_272k_tokens: Some(0.000045),
            cache_read_input_token_cost: Some(0.0000005),
            cache_read_input_token_cost_above_272k_tokens: Some(0.000001),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5.1".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5.1-codex".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5.1-codex-max".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5-codex".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gpt-5-nano".into(),
        ModelPricing {
            input_cost_per_token: Some(5e-8),
            output_cost_per_token: Some(4e-7),
            cache_read_input_token_cost: Some(5e-9),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // === OpenCode Zen: Claude family (LiteLLM entries) ===
    m.insert(
        "claude-3-5-sonnet-20241022".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: Some(0.0000003),
            cache_creation_input_token_cost: Some(0.00000375),
            ..Default::default()
        },
    );
    m.insert(
        "claude-sonnet-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: Some(3e-7),
            cache_creation_input_token_cost: Some(0.00000375),
            ..Default::default()
        },
    );
    m.insert(
        "claude-haiku-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000001),
            output_cost_per_token: Some(0.000005),
            cache_read_input_token_cost: Some(1e-7),
            cache_creation_input_token_cost: Some(0.00000125),
            ..Default::default()
        },
    );
    m.insert(
        "bedrock/us.anthropic.claude-3-5-haiku-20241022-v1:0".into(),
        ModelPricing {
            input_cost_per_token: Some(8e-7),
            output_cost_per_token: Some(0.000004),
            cache_read_input_token_cost: Some(8e-8),
            cache_creation_input_token_cost: Some(0.000001),
            ..Default::default()
        },
    );
    m.insert(
        "claude-opus-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            cache_read_input_token_cost: Some(5e-7),
            cache_creation_input_token_cost: Some(0.00000625),
            ..Default::default()
        },
    );
    m.insert(
        "claude-opus-4-1".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000015),
            output_cost_per_token: Some(0.000075),
            cache_read_input_token_cost: Some(0.0000015),
            cache_creation_input_token_cost: Some(0.00001875),
            ..Default::default()
        },
    );

    // === OpenCode Zen: Gemini family (LiteLLM entries) ===
    m.insert(
        "gemini-3-flash".into(),
        ModelPricing {
            input_cost_per_token: Some(5e-7),
            output_cost_per_token: Some(0.000003),
            cache_read_input_token_cost: Some(5e-8),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "gemini-3-flash-preview".into(),
        ModelPricing {
            input_cost_per_token: Some(5e-7),
            output_cost_per_token: Some(0.000003),
            cache_read_input_token_cost: Some(5e-8),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "openrouter/google/gemini-3-pro-preview".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000002),
            output_cost_per_token: Some(0.000012),
            cache_read_input_token_cost: Some(2e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "vertex_ai/gemini-3-flash-preview".into(),
        ModelPricing {
            input_cost_per_token: Some(5e-7),
            output_cost_per_token: Some(0.000003),
            cache_read_input_token_cost: Some(5e-8),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // === OpenCode Zen: Grok (LiteLLM entry) ===
    m.insert(
        "xai/grok-code-fast-1-0825".into(),
        ModelPricing {
            input_cost_per_token: Some(2e-7),
            output_cost_per_token: Some(0.0000015),
            cache_read_input_token_cost: Some(2e-8),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    m.insert(
        "azure_ai/grok-code-fast-1".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000035),
            output_cost_per_token: Some(0.0000175),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "bedrock/anthropic.claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: Some(3e-7),
            cache_creation_input_token_cost: Some(0.00000375),
            ..Default::default()
        },
    );
    m.insert(
        "vertex_ai/gemini-2.5-pro".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.000005),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "google/gemini-2.5-pro".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.000005),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    m
}

/// Mock OpenRouter data matching real API responses for OpenCode Zen models
fn mock_openrouter() -> HashMap<String, ModelPricing> {
    let mut m = HashMap::new();

    // === Baseline models ===
    m.insert(
        "openai/gpt-4o".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000025),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(0.00000125),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // === OpenCode Zen: Claude (OpenRouter entries) ===
    m.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: Some(3e-7),
            cache_creation_input_token_cost: Some(0.00000375),
            ..Default::default()
        },
    );
    m.insert(
        "anthropic/claude-opus-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            cache_read_input_token_cost: Some(0.0000005),
            cache_creation_input_token_cost: Some(0.00000625),
            ..Default::default()
        },
    );
    m.insert(
        "anthropic/claude-3.5-haiku".into(),
        ModelPricing {
            input_cost_per_token: Some(8e-7),
            output_cost_per_token: Some(0.000004),
            cache_read_input_token_cost: Some(8e-8),
            cache_creation_input_token_cost: Some(0.000001),
            ..Default::default()
        },
    );

    // === OpenCode Zen: GLM family ===
    m.insert(
        "z-ai/glm-4.7".into(),
        ModelPricing {
            input_cost_per_token: Some(4e-7),
            output_cost_per_token: Some(0.0000015),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "z-ai/glm-4.6".into(),
        ModelPricing {
            input_cost_per_token: Some(3.9e-7),
            output_cost_per_token: Some(0.0000019),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    m.insert(
        "moonshotai/kimi-k2".into(),
        ModelPricing {
            input_cost_per_token: Some(4.56e-7),
            output_cost_per_token: Some(0.00000184),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "moonshotai/kimi-k2.5".into(),
        ModelPricing {
            input_cost_per_token: Some(4.5e-7),
            output_cost_per_token: Some(0.0000025),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "moonshotai/kimi-k2.6".into(),
        ModelPricing {
            input_cost_per_token: Some(9.5e-7),
            output_cost_per_token: Some(0.000004),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    m.insert(
        "moonshotai/kimi-k2-thinking".into(),
        ModelPricing {
            input_cost_per_token: Some(4e-7),
            output_cost_per_token: Some(0.00000175),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // === OpenCode Zen: Qwen family ===
    m.insert(
        "qwen/qwen3-coder".into(),
        ModelPricing {
            input_cost_per_token: Some(2.2e-7),
            output_cost_per_token: Some(9.5e-7),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    m
}

fn create_lookup() -> PricingLookup {
    PricingLookup::new(mock_litellm(), mock_openrouter())
}

// =========================================================================
// OPENCODE ZEN MODELS - GPT-5 FAMILY
// All models from https://opencode.ai/docs/zen/
// =========================================================================

#[test]
fn test_opencode_zen_gpt_5_2() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5.2").unwrap();
    assert_eq!(result.matched_key, "gpt-5.2");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5_1() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5.1").unwrap();
    assert_eq!(result.matched_key, "gpt-5.1");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5_1_codex() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5.1-codex").unwrap();
    assert_eq!(result.matched_key, "gpt-5.1-codex");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5_1_codex_max() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5.1-codex-max").unwrap();
    assert_eq!(result.matched_key, "gpt-5.1-codex-max");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5").unwrap();
    assert_eq!(result.matched_key, "gpt-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5_codex() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5-codex").unwrap();
    assert_eq!(result.matched_key, "gpt-5-codex");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gpt_5_nano() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5-nano").unwrap();
    assert_eq!(result.matched_key, "gpt-5-nano");
    assert_eq!(result.source, "LiteLLM");
}

// =========================================================================
// OPENCODE ZEN MODELS - CLAUDE FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_claude_sonnet_4_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_claude_sonnet_4() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4").unwrap();
    assert_eq!(result.matched_key, "anthropic/claude-sonnet-4");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_claude_haiku_4_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-haiku-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-haiku-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_claude_3_5_haiku() {
    let lookup = create_lookup();
    assert!(lookup.lookup("claude-3-5-haiku").is_none());
}

#[test]
fn test_opencode_zen_claude_3_5_haiku_with_dot() {
    let lookup = create_lookup();
    assert!(lookup.lookup("claude-3.5-haiku").is_none());
}

#[test]
fn test_opencode_zen_claude_opus_4_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-opus-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_claude_opus_4_1() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-opus-4-1").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-1");
    assert_eq!(result.source, "LiteLLM");
}

// =========================================================================
// OPENCODE ZEN MODELS - GLM FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_glm_4_7_free() {
    let lookup = create_lookup();
    assert!(lookup.lookup("glm-4.7-free").is_none());
}

#[test]
fn test_opencode_zen_glm_4_6() {
    let lookup = create_lookup();
    let result = lookup.lookup("glm-4.6").unwrap();
    assert_eq!(result.matched_key, "z-ai/glm-4.6");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_glm_4_7_with_hyphen() {
    let lookup = create_lookup();
    let result = lookup.lookup("glm-4-7").unwrap();
    assert_eq!(result.matched_key, "z-ai/glm-4.7");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_glm_4_6_with_hyphen() {
    let lookup = create_lookup();
    let result = lookup.lookup("glm-4-6").unwrap();
    assert_eq!(result.matched_key, "z-ai/glm-4.6");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_big_pickle_is_not_a_global_price_alias() {
    let lookup = create_lookup();
    assert!(lookup.lookup("big-pickle").is_none());
}

// =========================================================================
// OPENCODE ZEN MODELS - GEMINI FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_gemini_3_pro() {
    let lookup = create_lookup();
    let result = lookup.lookup("gemini-3-pro").unwrap();
    assert_eq!(result.matched_key, "openrouter/google/gemini-3-pro-preview");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gemini_3_flash() {
    let lookup = create_lookup();
    let result = lookup.lookup("gemini-3-flash").unwrap();
    assert_eq!(result.matched_key, "gemini-3-flash");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_opencode_zen_gemini_3_flash_preview() {
    let lookup = create_lookup();
    let result = lookup.lookup("gemini-3-flash-preview").unwrap();
    assert_eq!(result.matched_key, "gemini-3-flash-preview");
    assert_eq!(result.source, "LiteLLM");
}

// =========================================================================
// OPENCODE ZEN MODELS - KIMI FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_kimi_k2() {
    let lookup = create_lookup();
    let result = lookup.lookup("kimi-k2").unwrap();
    assert_eq!(result.matched_key, "moonshotai/kimi-k2");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_kimi_k2_thinking() {
    let lookup = create_lookup();
    let result = lookup.lookup("kimi-k2-thinking").unwrap();
    assert_eq!(result.matched_key, "moonshotai/kimi-k2-thinking");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_kimi_k2_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("kimi-k2.5").unwrap();
    assert_eq!(result.matched_key, "moonshotai/kimi-k2.5");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_kimi_k2_5_free() {
    let lookup = create_lookup();
    assert!(lookup.lookup("kimi-k2.5-free").is_none());
}

#[test]
fn test_opencode_zen_kimi_coding_plan_short_aliases() {
    let lookup = create_lookup();

    for alias in [
        "k2p5",
        "k2-p5",
        "kimi-for-coding/k2p5",
        "k2p6",
        "k2-p6",
        "kimi-k2p6",
        "kimi-for-coding/k2p6",
    ] {
        assert!(lookup.lookup(alias).is_none(), "alias {alias}");
    }

    let canonical = lookup.lookup("Kimi-K2.6").unwrap();
    assert_eq!(canonical.matched_key, "moonshotai/kimi-k2.6");
    assert_eq!(canonical.source, "OpenRouter");
}

#[test]
fn test_opencode_zen_kimi_k2_6_alias_pricing() {
    let lookup = create_lookup();
    let result = lookup.lookup("kimi-k2.6").unwrap();
    assert_eq!(result.matched_key, "moonshotai/kimi-k2.6");
    assert_eq!(result.source, "OpenRouter");
    assert_eq!(result.pricing.input_cost_per_token, Some(9.5e-7));
    assert_eq!(result.pricing.output_cost_per_token, Some(0.000004));
}

#[test]
fn test_opencode_zen_kimi_k2_6_provider_hint_from_kimi_for_coding() {
    let lookup = create_lookup();
    assert!(lookup
        .lookup_with_provider("k2p6", Some("kimi-for-coding"))
        .is_none());
}

#[test]
fn test_opencode_zen_kimi_k2_5_aliases_follow_coding_plan() {
    let lookup = create_lookup();

    assert!(lookup.lookup("k2p5").is_none());

    let dotted = lookup.lookup("kimi-k2.5").unwrap();
    assert_eq!(dotted.matched_key, "moonshotai/kimi-k2.5");
}

// =========================================================================
// OPENCODE ZEN MODELS - QWEN FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_qwen3_coder() {
    let lookup = create_lookup();
    let result = lookup.lookup("qwen3-coder").unwrap();
    assert_eq!(result.matched_key, "qwen/qwen3-coder");
    assert_eq!(result.source, "OpenRouter");
}

// =========================================================================
// OPENCODE ZEN MODELS - GROK FAMILY
// =========================================================================

#[test]
fn test_opencode_zen_grok_code() {
    let lookup = create_lookup();
    let result = lookup.lookup("grok-code").unwrap();
    assert_eq!(result.matched_key, "xai/grok-code-fast-1-0825");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_hint_prefers_matching_pricing_source() {
    let lookup = create_lookup();
    let result = lookup
        .lookup_with_provider("grok-code", Some("azure"))
        .unwrap();
    assert_eq!(result.matched_key, "azure_ai/grok-code-fast-1");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_hint_matches_nested_reseller_exact_key() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            output_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure/openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            output_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup_with_provider("gpt-4", Some("azure")).unwrap();
    assert_eq!(result.matched_key, "azure/openai/gpt-4");
    assert_eq!(result.source, "LiteLLM");
}

// Regression: a generic id whose only fuzzy-eligible remnant after suffix
// stripping is the bare word `model` (real example seen in local data:
// `model-zero-usage-v1`, `test-model`) must NOT fuzzy-match a real priced
// key like `azure_ai/model_router`. The word `model` carries no model
// identity and is on the FUZZY_BLOCKLIST.
#[test]
fn fuzzy_match_does_not_resolve_generic_model_token() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "azure_ai/model_router".into(),
        ModelPricing {
            input_cost_per_token: Some(1.4e-7),
            output_cost_per_token: Some(0.0),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new(litellm, HashMap::new());

    // The bare token must not resolve.
    assert!(lookup.lookup("model").is_none());
    // Ids that strip down to the bare `model` token must not misresolve.
    assert!(lookup.lookup("model-zero-usage-v1").is_none());
    assert!(lookup.lookup("model-nonzero-usage-v1").is_none());
    assert!(lookup.lookup("test-model").is_none());

    // But an EXACT key match is still honored — `model-router` is a real
    // model id, not a fuzzy remnant.
    let mut litellm2 = HashMap::new();
    litellm2.insert(
        "azure/model-router".into(),
        ModelPricing {
            input_cost_per_token: Some(1.4e-7),
            output_cost_per_token: Some(0.0),
            ..Default::default()
        },
    );
    let lookup2 = PricingLookup::new(litellm2, HashMap::new());
    assert_eq!(
        lookup2.lookup("model-router").unwrap().matched_key,
        "azure/model-router"
    );
}

#[test]
fn test_provider_hint_normalizes_openai_codex_alias() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-5.2-preview".into(),
        ModelPricing {
            input_cost_per_token: Some(1.0),
            ..Default::default()
        },
    );
    litellm.insert(
        "google/gpt-5.2-preview-max".into(),
        ModelPricing {
            input_cost_per_token: Some(2.0),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup_with_provider("gpt-5.2", Some("openai-codex"))
        .unwrap();
    assert_eq!(result.matched_key, "openai/gpt-5.2-preview");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_hint_matches_nested_google_segment_during_fuzzy_lookup() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openrouter/google/gemini-3-pro-preview".into(),
        ModelPricing {
            input_cost_per_token: Some(1.0),
            ..Default::default()
        },
    );
    litellm.insert(
        "vertex_ai/gemini-3-pro-preview-max".into(),
        ModelPricing {
            input_cost_per_token: Some(2.0),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup_with_provider("gemini-3-pro", Some("google"))
        .unwrap();
    assert_eq!(result.matched_key, "openrouter/google/gemini-3-pro-preview");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_cross_source_fuzzy_provider_hint_wins_over_original_provider_fallback() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "fireworks_ai/deepseek-v3-0324".into(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "deepseek/deepseek-v3-0324".into(),
        ModelPricing {
            input_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let result = lookup
        .lookup_with_provider("deepseek-v3", Some("fireworks"))
        .unwrap();
    assert_eq!(result.matched_key, "fireworks_ai/deepseek-v3-0324");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_scoped_path_does_not_strip_into_wrong_fireworks_model() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "fireworks_ai/accounts/fireworks/models/deepseek-r1-0528-distill-qwen3-8b".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000002),
            output_cost_per_token: Some(0.0000002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    assert!(
        lookup
            .lookup("accounts/fireworks/models/deepseek-v4-pro")
            .is_none(),
        "provider-scoped model paths should not be shortened into unrelated fuzzy matches"
    );
}

#[test]
fn test_provider_scoped_path_matches_exact_litellm_reseller_key() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "fireworks_ai/accounts/fireworks/models/deepseek-v4-pro".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000003),
            output_cost_per_token: Some(0.0000004),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup("accounts/fireworks/models/deepseek-v4-pro")
        .unwrap();

    assert_eq!(
        result.matched_key,
        "fireworks_ai/accounts/fireworks/models/deepseek-v4-pro"
    );
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_scoped_path_matches_exact_terminal_provider_key() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "fireworks_ai/deepseek-v4-pro".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000003),
            output_cost_per_token: Some(0.0000004),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup("accounts/fireworks/models/deepseek-v4-pro")
        .unwrap();

    assert_eq!(result.matched_key, "fireworks_ai/deepseek-v4-pro");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_provider_scoped_path_does_not_use_upstream_openrouter_exact() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "deepseek/deepseek-v4-pro".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000001),
            output_cost_per_token: Some(0.000002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(HashMap::new(), openrouter);

    assert!(
        lookup
            .lookup("accounts/fireworks/models/deepseek-v4-pro")
            .is_none(),
        "Fireworks-scoped usage should not be priced with upstream DeepSeek rates"
    );
}

// =========================================================================
// BASELINE / LEGACY TESTS
// =========================================================================

#[test]
fn test_exact_match_litellm() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-4o").unwrap();
    assert_eq!(result.matched_key, "gpt-4o");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_exact_match_gpt_5_5_litellm() {
    let lookup = create_lookup();
    let result = lookup.lookup("gpt-5.5").unwrap();
    assert_eq!(result.matched_key, "gpt-5.5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_exact_match_openrouter() {
    let lookup = create_lookup();
    let result = lookup.lookup("z-ai/glm-4.7").unwrap();
    assert_eq!(result.matched_key, "z-ai/glm-4.7");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn test_openrouter_model_part_match() {
    let lookup = create_lookup();
    let result = lookup.lookup("glm-4.7").unwrap();
    assert_eq!(result.matched_key, "z-ai/glm-4.7");
    assert_eq!(result.source, "OpenRouter");
}

#[test]
fn does_not_strip_low_suffix_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-5.1-codex-low").is_none());
}

#[test]
fn does_not_strip_high_suffix_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-4o-high").is_none());
}

#[test]
fn does_not_strip_free_suffix_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("glm-4.7-free").is_none());
}

#[test]
fn does_not_strip_xhigh_suffix_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-5.2-xhigh").is_none());
}

#[test]
fn does_not_strip_xhigh_suffix_from_gpt_5_5_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-5.5-xhigh").is_none());
}

#[test]
fn does_not_strip_xhigh_suffix_from_codex_max_to_base() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-5.1-codex-max-xhigh").is_none());
}

#[test]
fn exact_decorated_catalog_key_still_wins() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5.2".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000175),
            output_cost_per_token: Some(0.000014),
            ..Default::default()
        },
    );
    litellm.insert(
        "gpt-5.2(xhigh)".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.00002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("gpt-5.2(xhigh)").unwrap();
    assert_eq!(result.matched_key, "gpt-5.2(xhigh)");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.000003));
}

#[test]
fn does_not_strip_parenthesized_reasoning_tier_to_base() {
    let lookup = create_lookup();

    for tier in ["minimal", "low", "medium", "high", "xhigh", "auto", "none"] {
        let id = format!("gpt-5.2({tier})");
        assert!(lookup.lookup(&id).is_none(), "{id}");
    }
}

#[test]
fn does_not_strip_parenthesized_reasoning_tier_for_gpt_and_gemini() {
    let lookup = create_lookup();

    assert!(lookup.lookup("gpt-5.2(high)").is_none());
    assert!(lookup.lookup("gemini-3-pro(auto)").is_none());
}

#[test]
fn claude_direct_normalization_can_match_parenthesized_tier() {
    let lookup = create_lookup();

    let claude = lookup.lookup("claude-sonnet-4-5(high)").unwrap();
    assert_eq!(claude.matched_key, "claude-sonnet-4-5");

    let claude_dot = lookup.lookup("claude-sonnet-4.5(none)").unwrap();
    assert_eq!(claude_dot.matched_key, "claude-sonnet-4-5");
}

#[test]
fn does_not_strip_parenthesized_reasoning_tier_with_routing_prefix() {
    let lookup = create_lookup();

    assert!(lookup.lookup("myproxy-gpt-5.2(xhigh)").is_none());
    assert!(lookup.lookup("antigravity-gemini-3.1-pro(high)").is_none());
}

#[test]
fn parenthesized_unknown_value_does_not_strip_to_base() {
    let lookup = create_lookup();

    assert!(lookup.lookup("gpt-5.2(weirdgarbage)").is_none());
    assert!(lookup.lookup("gpt-5.2(1024)").is_none());
    assert!(lookup.lookup("gpt-5.2()").is_none());
    assert!(lookup.lookup("gpt-5.2-codex(invalid)").is_none());
    assert!(lookup.lookup("myproxy-gpt-5.2(invalid)").is_none());
    let claude = lookup.lookup("claude-sonnet-4-5(garbage)").unwrap();
    assert_eq!(claude.matched_key, "claude-sonnet-4-5");
    assert!(lookup.lookup("gemini-3-pro(weird)").is_none());
}

#[test]
fn parenthesized_reasoning_tier_cost_is_zero_without_exact_pricing() {
    let lookup = create_lookup();
    let tiered = lookup.calculate_cost("gpt-5.2(xhigh)", 1_000_000, 500_000, 0, 0, 0);

    assert_eq!(tiered, 0.0);
}

#[test]
fn test_normalize_opus_4_5() {
    let lookup = create_lookup();
    let result = lookup.lookup("opus-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_free_variant_normalizes_to_market_priced_claude_model() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5-free").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_free_variant_with_extra_suffix_falls_back_to_market_priced_model() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5-free-high").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_normalize_opus_4_6_prefers_4_6_over_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00002),
            output_cost_per_token: Some(0.0001),
            ..Default::default()
        },
    );
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00005),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("opus-4-6").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-6");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_normalize_opus_4_6_dot_prefers_4_6_over_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00002),
            output_cost_per_token: Some(0.0001),
            ..Default::default()
        },
    );
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00005),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("opus-4.6").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-6");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_normalize_opus_4_60_does_not_degrade_to_opus_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00002),
            output_cost_per_token: Some(0.0001),
            ..Default::default()
        },
    );
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00005),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    assert!(lookup.lookup("opus-4-60").is_none());
}

#[test]
fn test_normalize_opus_4_7_prefers_4_7_over_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000015),
            output_cost_per_token: Some(0.000075),
            ..Default::default()
        },
    );
    litellm.insert(
        "claude-opus-4-7".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("opus-4-7").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-7");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_normalize_opus_4_7_dot_prefers_4_7_over_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000015),
            output_cost_per_token: Some(0.000075),
            ..Default::default()
        },
    );
    litellm.insert(
        "claude-opus-4-7".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("opus-4.7").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-7");
    assert_eq!(result.source, "LiteLLM");
}

/// Regression: `aws.claude-opus-4-7` (Bedrock-style id) used to degrade
/// to OpenRouter's `anthropic/claude-opus-4` ($15/$75/$1.50/$18.75 per M)
/// because `normalize_model_name` only knew 4.5/4.6 and fell through to
/// the bare `claude-opus-4` branch — which OpenRouter then resolved via
/// `model_part` index to the legacy opus 4 entry. Result was ~3x overcharge.
#[test]
fn test_aws_opus_4_7_does_not_degrade_to_opus_4() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-7".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            cache_read_input_token_cost: Some(5e-7),
            cache_creation_input_token_cost: Some(0.00000625),
            ..Default::default()
        },
    );
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000015),
            output_cost_per_token: Some(0.000075),
            cache_read_input_token_cost: Some(0.0000015),
            cache_creation_input_token_cost: Some(0.00001875),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let result = lookup.lookup("aws.claude-opus-4-7").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-7");
    assert_ne!(result.matched_key, "anthropic/claude-opus-4");

    // 8.4M input + 873K output + 41.3M cache_read + 12.1M cache_write
    // at opus-4-7 rates should be ~$160, not ~$480 (legacy opus 4).
    let cost = lookup.calculate_cost(
        "aws.claude-opus-4-7",
        8_400_000,
        873_000,
        41_300_000,
        12_100_000,
        0,
    );
    assert!(
        (140.0..=180.0).contains(&cost),
        "expected opus-4-7 priced cost around $160, got ${cost:.2}"
    );
}

#[test]
fn test_unknown_future_opus_minor_does_not_degrade_to_opus_4() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000015),
            output_cost_per_token: Some(0.000075),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(HashMap::new(), openrouter);

    assert!(lookup.lookup("claude-opus-4-8").is_none());
    assert!(lookup.lookup("aws.claude-opus-4-8").is_none());
}

#[test]
fn test_normalize_opus_14_6_does_not_map_to_4_6() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00005),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    assert!(lookup.lookup("opus-14-6").is_none());
}

#[test]
fn test_normalize_sonnet_14_5_does_not_map_to_4_5() {
    assert_eq!(normalize_model_name("sonnet-14-5"), None);
}

#[test]
fn test_normalize_haiku_14_5_does_not_map_to_4_5() {
    assert_eq!(normalize_model_name("haiku-14-5"), None);
}

// =========================================================================
// Generalized Claude family/major/minor normalization (PR #634 rework)
// =========================================================================

/// Synthetic dataset mirroring real LiteLLM/OpenRouter key shapes, with
/// deliberately adversarial gaps: bedrock-style `us.anthropic.` keys exist
/// for opus but not sonnet, and OpenRouter carries a pricier opus `-fast`
/// variant that the old fallbacks degraded other families onto.
fn claude_family_fixture() -> PricingLookup {
    fn p(input: f64, output: f64) -> ModelPricing {
        ModelPricing {
            input_cost_per_token: Some(input),
            output_cost_per_token: Some(output),
            ..Default::default()
        }
    }

    let mut litellm = HashMap::new();
    litellm.insert("claude-opus-4".to_string(), p(15e-6, 75e-6));
    litellm.insert("claude-opus-4-1".to_string(), p(15e-6, 75e-6));
    litellm.insert("claude-opus-4-5".to_string(), p(5e-6, 25e-6));
    litellm.insert("claude-opus-4-6".to_string(), p(5e-6, 25e-6));
    litellm.insert("claude-opus-4-7".to_string(), p(5e-6, 25e-6));
    litellm.insert("claude-opus-4-8".to_string(), p(5e-6, 25e-6));
    litellm.insert("claude-sonnet-4".to_string(), p(3e-6, 15e-6));
    litellm.insert("claude-sonnet-4-5".to_string(), p(3e-6, 15e-6));
    litellm.insert("claude-sonnet-4-6".to_string(), p(3e-6, 15e-6));
    litellm.insert("claude-haiku-4-5".to_string(), p(1e-6, 5e-6));
    litellm.insert("us.anthropic.claude-opus-4-8".to_string(), p(5e-6, 25e-6));
    litellm.insert("vertex_ai/claude-sonnet-4-6".to_string(), p(3e-6, 15e-6));

    let mut openrouter = HashMap::new();
    openrouter.insert("anthropic/claude-opus-4".to_string(), p(15e-6, 75e-6));
    openrouter.insert("anthropic/claude-opus-4.8".to_string(), p(5e-6, 25e-6));
    openrouter.insert("anthropic/claude-opus-4.8-fast".to_string(), p(7e-6, 30e-6));
    openrouter.insert("anthropic/claude-sonnet-4.6".to_string(), p(3e-6, 15e-6));
    openrouter.insert("anthropic/claude-haiku-4.5".to_string(), p(1e-6, 5e-6));
    openrouter.insert("anthropic/claude-fable-5".to_string(), p(5e-6, 25e-6));

    PricingLookup::new(litellm, openrouter)
}

#[test]
fn test_normalize_minor_generalizes_across_families() {
    assert_eq!(
        normalize_model_name("claude-sonnet-4-7"),
        Some("claude-sonnet-4-7".into())
    );
    assert_eq!(
        normalize_model_name("sonnet-4.7"),
        Some("claude-sonnet-4-7".into())
    );
    assert_eq!(
        normalize_model_name("claude-haiku-4-6"),
        Some("claude-haiku-4-6".into())
    );
    assert_eq!(
        normalize_model_name("haiku-4.6"),
        Some("claude-haiku-4-6".into())
    );
    assert_eq!(
        normalize_model_name("claude-opus-4-9"),
        Some("claude-opus-4-9".into())
    );
    assert_eq!(
        normalize_model_name("opus-4.9"),
        Some("claude-opus-4-9".into())
    );
    assert_eq!(
        normalize_model_name("opus-5-2"),
        Some("claude-opus-5-2".into())
    );
}

#[test]
fn test_normalize_reversed_order_all_families() {
    assert_eq!(
        normalize_model_name("claude-4-8-opus"),
        Some("claude-opus-4-8".into())
    );
    assert_eq!(
        normalize_model_name("4-8-opus"),
        Some("claude-opus-4-8".into())
    );
    assert_eq!(
        normalize_model_name("claude-4-6-sonnet"),
        Some("claude-sonnet-4-6".into())
    );
    assert_eq!(
        normalize_model_name("claude-4-5-haiku"),
        Some("claude-haiku-4-5".into())
    );
}

#[test]
fn test_normalize_bare_modern_major() {
    assert_eq!(
        normalize_model_name("claude-sonnet-5"),
        Some("claude-sonnet-5".into())
    );
    assert_eq!(
        normalize_model_name("claude-opus-5"),
        Some("claude-opus-5".into())
    );
    assert_eq!(
        normalize_model_name("fable-5"),
        Some("claude-fable-5".into())
    );
    assert_eq!(
        normalize_model_name("claude-fable-5[1m]"),
        Some("claude-fable-5".into())
    );
}

/// Boundary contract preserved from main's hardcoded matcher: two-digit
/// minors and majors, zero minors, undelimited versions, and dated forms
/// must not normalize to a coarser key. (PR #634's original parser
/// degraded `opus-4-60` to `claude-opus-4`; main's contract is None.)
#[test]
fn test_normalize_modern_claude_boundaries() {
    assert_eq!(normalize_model_name("opus-4-60"), None);
    assert_eq!(normalize_model_name("sonnet-4-60"), None);
    assert_eq!(normalize_model_name("opus-14-6"), None);
    assert_eq!(normalize_model_name("opus4"), None);
    assert_eq!(normalize_model_name("opus-4x"), None);
    assert_eq!(normalize_model_name("opus-3"), None);
    assert_eq!(normalize_model_name("claude-sonnet-5-0"), None);
    assert_eq!(normalize_model_name("claude-opus-4-20250514"), None);
}

/// Legacy 3.x ids are not normalized by the modern Claude parser.
#[test]
fn test_normalize_legacy_line_not_hijacked_by_modern_parser() {
    assert_eq!(normalize_model_name("claude-3-5-sonnet"), None);
    assert_eq!(normalize_model_name("claude-3-7-sonnet-20250219"), None);
    assert_eq!(normalize_model_name("claude-3-5-haiku-20241022"), None);
}

/// Regression (B1): a bedrock-style sonnet id must never be billed at an
/// opus key. Before the family guard, `us.anthropic.claude-sonnet-4-6-v1:0`
/// suffix-stripped down to `us.anthropic.claude` and fuzzy-matched the
/// dataset's `us.anthropic.claude-opus-4-8` entry ($5/M instead of $3/M).
#[test]
fn test_bedrock_sonnet_never_billed_as_opus() {
    let lookup = claude_family_fixture();
    let result = lookup
        .lookup("us.anthropic.claude-sonnet-4-6-v1:0")
        .unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-6");
    assert_eq!(result.pricing.input_cost_per_token, Some(3e-6));
}

/// Regression (B2): reversed-order sonnet ids must resolve to the sonnet
/// key, not cross-family. Before reversed-order parsing was generalized
/// beyond opus, `claude-4-6-sonnet` stripped down to `claude` and
/// fuzzy-matched `anthropic/claude-opus-4.8-fast`.
#[test]
fn test_reversed_sonnet_resolves_canonical_not_cross_family() {
    let lookup = claude_family_fixture();
    for id in ["claude-4-6-sonnet", "4-6-sonnet"] {
        let result = lookup.lookup(id).unwrap();
        assert_eq!(result.matched_key, "claude-sonnet-4-6", "id: {id}");
    }
    let result = lookup.lookup("claude-4-5-haiku").unwrap();
    assert_eq!(result.matched_key, "claude-haiku-4-5");
}

/// Regression (B3): the never-degrade contract that
/// `test_unknown_future_opus_minor_does_not_degrade_to_opus_4` pins for
/// opus now holds for sonnet and haiku too. Unknown minors previously
/// degraded: `sonnet-4-7` -> claude-sonnet-4.6, `haiku-4-6` ->
/// claude-haiku-4.5 (and with real data even claude-3.5-haiku).
#[test]
fn test_unknown_sonnet_haiku_minor_does_not_degrade() {
    let lookup = claude_family_fixture();
    for id in [
        "sonnet-4-7",
        "claude-sonnet-4-7",
        "sonnet-4-60",
        "haiku-4-6",
        "claude-haiku-4-6",
    ] {
        assert!(lookup.lookup(id).is_none(), "id {id} must not degrade");
    }
}

/// Regression (B4): major >= 5 ids resolve to a dataset-known exact id
/// when one exists, else None — never to a different major. Previously
/// `claude-opus-5` resolved to `anthropic/claude-opus-4.8-fast` and
/// `sonnet-5`/`claude-sonnet-5-0` to sonnet 4.6, while bare `opus-5`
/// happened to return None only because of a fuzzy length cutoff.
#[test]
fn test_major_five_never_resolves_to_different_major() {
    let lookup = claude_family_fixture();
    for id in [
        "claude-opus-5",
        "opus-5",
        "opus-5-2",
        "sonnet-5",
        "claude-sonnet-5-0",
    ] {
        assert!(
            lookup.lookup(id).is_none(),
            "id {id} must not resolve to a 4.x key"
        );
    }

    // fable-5 is dataset-known (OpenRouter) and resolves in all forms.
    for id in [
        "claude-fable-5",
        "fable-5",
        "claude-fable-5[1m]",
        "anthropic/claude-fable-5",
    ] {
        let result = lookup.lookup(id).unwrap();
        assert_eq!(result.matched_key, "anthropic/claude-fable-5", "id: {id}");
    }
}

/// When the dataset later gains a major-5 key, the same ids resolve to it
/// with no code change — the "known version" decision is dataset-driven.
#[test]
fn test_major_five_resolves_once_dataset_knows_it() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-5".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            output_cost_per_token: Some(0.00005),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new(litellm, HashMap::new());

    for id in ["claude-opus-5", "opus-5", "aws.claude-opus-5-thinking"] {
        let result = lookup.lookup(id).unwrap();
        assert_eq!(result.matched_key, "claude-opus-5", "id: {id}");
    }
}

/// Known minors keep resolving across the id shapes seen in the wild:
/// dotted versions, vendor prefixes, tier/feature suffixes.
#[test]
fn test_known_minor_shapes_resolve_per_family() {
    let lookup = claude_family_fixture();
    let cases = [
        ("opus-4-8", "claude-opus-4-8"),
        ("opus-4.8", "claude-opus-4-8"),
        ("aws.claude-opus-4-8", "claude-opus-4-8"),
        ("claude-opus-4-8-thinking", "claude-opus-4-8"),
        ("claude-sonnet-4-6", "claude-sonnet-4-6"),
        ("claude-sonnet-4.6", "anthropic/claude-sonnet-4.6"),
        ("sonnet-4-6", "claude-sonnet-4-6"),
        ("sonnet-4.6", "claude-sonnet-4-6"),
        ("aws.claude-sonnet-4-6-v1", "claude-sonnet-4-6"),
        ("claude-sonnet-4-6-thinking", "claude-sonnet-4-6"),
        ("haiku-4-5", "claude-haiku-4-5"),
        ("haiku-4.5", "claude-haiku-4-5"),
        ("vertex_ai/claude-sonnet-4-6", "vertex_ai/claude-sonnet-4-6"),
    ];
    for (id, expected) in cases {
        let result = lookup.lookup(id).unwrap();
        assert_eq!(result.matched_key, expected, "id: {id}");
    }
}

/// Ported from PR #634: the next opus minor must prefer its own key over
/// the bare `claude-opus-4` catch-all, in dashed and dotted forms.
#[test]
fn test_normalize_opus_4_8_prefers_4_8_over_4() {
    let lookup = claude_family_fixture();
    for id in ["opus-4-8", "opus-4.8"] {
        let result = lookup.lookup(id).unwrap();
        assert_eq!(result.matched_key, "claude-opus-4-8", "id: {id}");
        assert_eq!(result.source, "LiteLLM");
    }
}

/// Ported from PR #634: `aws.claude-opus-4-8` must not degrade to
/// OpenRouter's legacy `anthropic/claude-opus-4` (~3x overcharge).
#[test]
fn test_aws_opus_4_8_does_not_degrade_to_opus_4() {
    let lookup = claude_family_fixture();
    let result = lookup.lookup("aws.claude-opus-4-8").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-8");

    // 8.4M input + 873K output at opus-4-8 rates is ~$64, not ~$191
    // (legacy opus 4 at $15/$75 per M).
    let cost = lookup.calculate_cost("aws.claude-opus-4-8", 8_400_000, 873_000, 0, 0, 0);
    assert!(
        (60.0..=70.0).contains(&cost),
        "expected opus-4-8 priced cost around $64, got ${cost:.2}"
    );
}

/// Regression (post-#634 catalog audit, bug 1): retired `claude-2.x` ids
/// (present in historical usage logs, absent from every pricing dataset)
/// must resolve to None, not to a modern model's price. Earlier
/// suffix-eroding logic reduced `claude-2.1` to bare `claude`
/// (the "2.1" segment failed the all-digits version check), which then
/// fuzzy-matched `anthropic/claude-opus-4.7-fast` at $30/$150. The #634
/// family veto was bypassed because `claude-2.1` carries no
/// opus/sonnet/haiku/fable token.
#[test]
fn claude_2x_never_fuzzy_matches_modern_models() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4.7-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(30e-6),
            output_cost_per_token: Some(150e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new(HashMap::new(), openrouter);

    for id in ["claude-2.1", "claude-2.0", "claude", "anthropic"] {
        assert!(
            lookup.lookup(id).is_none(),
            "id {id} must resolve unpriced, never to another model's price"
        );
    }
}

/// Positive control for the claude-2.x guards: when a dataset actually
/// prices `claude-2.1`, it still resolves — the guards only block the
/// erosion-to-bare-brand path, not legitimate dataset hits.
#[test]
fn claude_2x_still_resolves_when_dataset_prices_it() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-2.1".to_string(),
        ModelPricing {
            input_cost_per_token: Some(8e-6),
            output_cost_per_token: Some(24e-6),
            ..Default::default()
        },
    );
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4.7-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(30e-6),
            output_cost_per_token: Some(150e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new(litellm, openrouter);

    let result = lookup.lookup("claude-2.1").unwrap();
    assert_eq!(result.matched_key, "claude-2.1");
    assert_eq!(result.pricing.input_cost_per_token, Some(8e-6));
}

/// Regression (post-#634 catalog audit, bug 2): `claude-opus-4-6-fast`
/// must hit the canonical OpenRouter `anthropic/claude-opus-4.6-fast`
/// key ($30/$150) via separator normalization, not Models.dev's reseller
/// `venice/claude-opus-4-6-fast` markup ($36/$180). Previously the
/// models.dev model-part pass ran before the version-normalized
/// OpenRouter exact pass in `lookup_auto`.
#[test]
fn canonical_fast_price_beats_reseller_markup() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4.6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(30e-6),
            output_cost_per_token: Some(150e-6),
            ..Default::default()
        },
    );
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "venice/claude-opus-4-6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(36e-6),
            output_cost_per_token: Some(180e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), openrouter, models_dev);

    let result = lookup.lookup("claude-opus-4-6-fast").unwrap();
    assert_eq!(result.matched_key, "anthropic/claude-opus-4.6-fast");
    assert_eq!(result.pricing.input_cost_per_token, Some(30e-6));
}

/// Regression (#707 review): a provider hint pins the lookup to that
/// provider's catalog. The canonical-source reorder asserted by
/// `canonical_fast_price_beats_reseller_markup` only applies to unhinted
/// lookups; with `provider_id = Some("venice")` the provider-scoped
/// models.dev pass must win over OpenRouter's unscoped `anthropic/...`
/// row, so provider-aware callers get the hinted provider's price.
#[test]
fn provider_hint_keeps_models_dev_provider_key_over_unscoped_canonical() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4.6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(30e-6),
            output_cost_per_token: Some(150e-6),
            ..Default::default()
        },
    );
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "venice/claude-opus-4-6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(36e-6),
            output_cost_per_token: Some(180e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), openrouter, models_dev);

    let hinted = lookup
        .lookup_with_provider("claude-opus-4-6-fast", Some("venice"))
        .unwrap();
    assert_eq!(hinted.matched_key, "venice/claude-opus-4-6-fast");
    assert_eq!(hinted.pricing.input_cost_per_token, Some(36e-6));

    // Unhinted lookups keep the canonical resolution.
    let unhinted = lookup.lookup("claude-opus-4-6-fast").unwrap();
    assert_eq!(unhinted.matched_key, "anthropic/claude-opus-4.6-fast");
    assert_eq!(unhinted.pricing.input_cost_per_token, Some(30e-6));
}

/// Regression (#707 review, cubic follow-up): the provider-hint pin must
/// also beat the unscoped OpenRouter MODEL-PART fallback, not just the
/// separator-normalized passes. When the hinted provider's models.dev key
/// shares the dotted model-part spelling that OpenRouter already indexes
/// (here both `claude-opus-4.6-fast`), an unscoped model-part match would
/// otherwise return `anthropic/...` before the provider-scoped pass ran.
#[test]
fn provider_hint_beats_unscoped_openrouter_model_part_for_dotted_id() {
    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4.6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(30e-6),
            output_cost_per_token: Some(150e-6),
            ..Default::default()
        },
    );
    let mut models_dev = HashMap::new();
    // Hinted provider's key uses the SAME dotted spelling OpenRouter
    // indexes as a model-part — this is what makes the unscoped model-part
    // pass fire first without the fix.
    models_dev.insert(
        "venice/claude-opus-4.6-fast".to_string(),
        ModelPricing {
            input_cost_per_token: Some(36e-6),
            output_cost_per_token: Some(180e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), openrouter, models_dev);

    // Hinted dotted lookup must pin to venice, not the canonical OpenRouter
    // model-part it also matches.
    let hinted = lookup
        .lookup_with_provider("claude-opus-4.6-fast", Some("venice"))
        .unwrap();
    assert_eq!(hinted.matched_key, "venice/claude-opus-4.6-fast");
    assert_eq!(hinted.pricing.input_cost_per_token, Some(36e-6));

    // Unhinted dotted lookup keeps the canonical OpenRouter resolution.
    let unhinted = lookup.lookup("claude-opus-4.6-fast").unwrap();
    assert_eq!(unhinted.matched_key, "anthropic/claude-opus-4.6-fast");
    assert_eq!(unhinted.pricing.input_cost_per_token, Some(30e-6));

    // A hint for a provider with no matching key must still fall through to
    // the canonical resolution rather than returning None.
    let no_match = lookup
        .lookup_with_provider("claude-opus-4.6-fast", Some("groq"))
        .unwrap();
    assert_eq!(no_match.matched_key, "anthropic/claude-opus-4.6-fast");
    assert_eq!(no_match.pricing.input_cost_per_token, Some(30e-6));
}

/// Regression (#707 review): the anthropic-first preference in the
/// models.dev model-part index must only choose among priced keys. An
/// unpriced (all-None) `anthropic/<model>` row must not shadow a priced
/// reseller row, which would bill the model at zero cost.
#[test]
fn unpriced_anthropic_models_dev_key_does_not_shadow_priced_reseller() {
    let mut models_dev = HashMap::new();
    models_dev.insert("anthropic/model-x".to_string(), ModelPricing::default());
    models_dev.insert(
        "reseller/model-x".to_string(),
        ModelPricing {
            input_cost_per_token: Some(36e-6),
            output_cost_per_token: Some(180e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let result = lookup.lookup("model-x").unwrap();
    assert_eq!(result.matched_key, "reseller/model-x");
    assert_eq!(result.pricing.input_cost_per_token, Some(36e-6));
}

#[test]
fn zero_priced_models_dev_key_is_a_catalog_result() {
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "opencode/glm-4.7-free".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.0),
            output_cost_per_token: Some(0.0),
            cache_read_input_token_cost: Some(0.0),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let model_part = lookup.lookup("glm-4.7-free").unwrap();
    assert_eq!(model_part.source, "Models.dev");
    assert_eq!(model_part.matched_key, "opencode/glm-4.7-free");
    assert_eq!(model_part.pricing.input_cost_per_token, Some(0.0));

    let full_key = lookup.lookup("opencode/glm-4.7-free").unwrap();
    assert_eq!(full_key.source, "Models.dev");
    assert_eq!(full_key.matched_key, "opencode/glm-4.7-free");
    assert_eq!(full_key.pricing.output_cost_per_token, Some(0.0));
}

#[test]
fn models_dev_bare_model_prefers_original_provider_over_zero_priced_route() {
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "github/gpt-4o-mini".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.0),
            output_cost_per_token: Some(0.0),
            cache_read_input_token_cost: Some(0.0),
            ..Default::default()
        },
    );
    models_dev.insert(
        "openai/gpt-4o-mini".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.15e-6),
            output_cost_per_token: Some(0.60e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let result = lookup.lookup("gpt-4o-mini").unwrap();
    assert_eq!(result.matched_key, "openai/gpt-4o-mini");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.15e-6));

    let github = lookup
        .lookup_with_provider("gpt-4o-mini", Some("github"))
        .unwrap();
    assert_eq!(github.matched_key, "github/gpt-4o-mini");
    assert_eq!(github.pricing.input_cost_per_token, Some(0.0));
}

#[test]
fn models_dev_bare_model_uses_canonical_provider_alias_for_original_choice() {
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "venice/mistral-small-2603".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.1875e-6),
            output_cost_per_token: Some(0.75e-6),
            ..Default::default()
        },
    );
    models_dev.insert(
        "mistral/mistral-small-2603".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.15e-6),
            output_cost_per_token: Some(0.60e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let result = lookup.lookup("mistral-small-2603").unwrap();
    assert_eq!(result.matched_key, "mistral/mistral-small-2603");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.15e-6));
}

/// After the lookup_auto reorder, models.dev must remain the long-tail
/// fallback for ids no canonical source knows.
#[test]
fn models_dev_still_covers_long_tail_after_reorder() {
    let mut models_dev = HashMap::new();
    models_dev.insert(
        "someprovider/exotic-model-9".to_string(),
        ModelPricing {
            input_cost_per_token: Some(2e-6),
            output_cost_per_token: Some(6e-6),
            ..Default::default()
        },
    );
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let result = lookup.lookup("exotic-model-9").unwrap();
    assert_eq!(result.matched_key, "someprovider/exotic-model-9");
    assert_eq!(result.pricing.input_cost_per_token, Some(2e-6));
}

/// Regression (post-#634 catalog audit, bug 2b): when multiple models.dev
/// providers share a model part, the winner must be deterministic and
/// prefer the canonical `anthropic/` namespace. Previously the winner
/// depended on HashMap iteration order (with real data a reseller key beat
/// the canonical Anthropic key because shorter keys were inserted last).
#[test]
fn models_dev_provider_choice_is_deterministic_and_prefers_anthropic() {
    let price = ModelPricing {
        input_cost_per_token: Some(0.8e-6),
        output_cost_per_token: Some(4e-6),
        ..Default::default()
    };
    // Adversarial insertion order: the non-canonical provider first.
    let mut models_dev = HashMap::new();
    models_dev.insert("302ai/claude-sonnet-4.6".to_string(), price.clone());
    models_dev.insert("anthropic/claude-sonnet-4.6".to_string(), price.clone());
    let lookup = PricingLookup::new_with_models_dev(HashMap::new(), HashMap::new(), models_dev);

    let result = lookup.lookup("claude-sonnet-4.6").unwrap();
    assert_eq!(result.matched_key, "anthropic/claude-sonnet-4.6");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.8e-6));
}

#[test]
fn test_blocklist_auto() {
    let lookup = create_lookup();
    assert!(lookup.lookup("auto").is_none());
}

#[test]
fn test_blocklist_mini() {
    let lookup = create_lookup();
    assert!(lookup.lookup("mini").is_none());
}

#[test]
fn test_force_source_litellm() {
    let lookup = create_lookup();
    let result = lookup
        .lookup_with_source("gpt-4o", Some("litellm"))
        .unwrap();
    assert_eq!(result.source, "LiteLLM");
    assert_eq!(result.matched_key, "gpt-4o");
}

#[test]
fn test_force_source_openrouter() {
    let lookup = create_lookup();
    let result = lookup
        .lookup_with_source("gpt-4o", Some("openrouter"))
        .unwrap();
    assert_eq!(result.source, "OpenRouter");
    assert_eq!(result.matched_key, "openai/gpt-4o");
}

#[test]
fn test_case_insensitive() {
    let lookup = create_lookup();
    let result = lookup.lookup("GPT-4O").unwrap();
    assert_eq!(result.matched_key, "gpt-4o");
}

#[test]
fn test_fuzzy_match_gemini() {
    let lookup = create_lookup();
    let result = lookup.lookup("gemini-3-pro").unwrap();
    assert_eq!(result.matched_key, "openrouter/google/gemini-3-pro-preview");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_tier_suffix_with_fuzzy() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gemini-3-pro-high").is_none());
}

#[test]
fn test_nonexistent_model() {
    let lookup = create_lookup();
    assert!(lookup.lookup("nonexistent-model-xyz").is_none());
}

#[test]
fn does_not_strip_codex_suffix_to_base() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    assert!(lookup.lookup("gpt-5-codex").is_none());
    assert!(lookup.lookup("gpt-5-codex-max").is_none());
}

#[test]
fn does_not_strip_stacked_suffixes_to_base() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: Some(1.25e-7),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    assert!(lookup.lookup("gpt-5-codex-high").is_none());
    assert!(lookup.lookup("gpt-5-codex-max-xhigh").is_none());
}

#[test]
fn test_fallback_suffix_prefers_exact_match() {
    // If the exact model exists, it should be used (no fallback)
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00000125),
            output_cost_per_token: Some(0.00001),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );
    litellm.insert(
        "gpt-5-codex".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000002), // Different price to verify which one is used
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    // Should use the exact match, not fall back
    let result = lookup.lookup("gpt-5-codex").unwrap();
    assert_eq!(result.matched_key, "gpt-5-codex");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.000002));
}

#[test]
fn test_normalize_version_separator() {
    assert_eq!(
        normalize_version_separator("glm-4-7"),
        Some("glm-4.7".into())
    );
    assert_eq!(
        normalize_version_separator("glm-4-6"),
        Some("glm-4.6".into())
    );
    assert_eq!(normalize_version_separator("claude-3-5-haiku"), None);
    assert_eq!(
        normalize_version_separator("gpt-5-1-codex"),
        Some("gpt-5.1-codex".into())
    );
    assert_eq!(normalize_version_separator("gpt-4o"), None);
    assert_eq!(normalize_version_separator("claude-sonnet"), None);
    assert_eq!(normalize_version_separator("big-pickle"), None);
}

#[test]
fn test_normalize_version_separator_preserves_dates() {
    assert_eq!(normalize_version_separator("2024-11-20"), None);
    assert_eq!(normalize_version_separator("model-2024-11-20"), None);
    assert_eq!(
        normalize_version_separator("claude-3-5-sonnet-20241022"),
        None
    );
    assert_eq!(normalize_version_separator("sonnet-20241022"), None);
    assert_eq!(normalize_version_separator("model-20241022-v1"), None);
}

#[test]
fn test_is_fuzzy_eligible() {
    assert!(!is_fuzzy_eligible("auto"));
    assert!(!is_fuzzy_eligible("mini"));
    assert!(!is_fuzzy_eligible("chat"));
    assert!(!is_fuzzy_eligible("base"));
    assert!(!is_fuzzy_eligible("abc"));
    assert!(is_fuzzy_eligible("gpt-4o"));
    // Bare brand tokens carry no model information: a fuzzy hit from them
    // can land on any model of the brand, so they are blocklisted.
    assert!(!is_fuzzy_eligible("claude"));
    assert!(!is_fuzzy_eligible("anthropic"));
}

// =========================================================================
// PROVIDER PREFERENCE TESTS
// =========================================================================

#[test]
fn test_provider_preference_grok_prefers_xai_over_azure() {
    let lookup = create_lookup();
    let result = lookup.lookup("grok-code").unwrap();
    assert_eq!(result.matched_key, "xai/grok-code-fast-1-0825");
    assert_eq!(result.source, "LiteLLM");
    assert!(!result.matched_key.starts_with("azure"));
}

/// Test that documents the exact before/after behavior for grok-code provider preference.
/// This test explicitly verifies that the original provider (xai/) is preferred over resellers (azure_ai/).
#[test]
fn test_grok_code_prefers_xai_over_azure() {
    // =========================================================================
    // BEFORE FIX: grok-code → azure_ai/grok-code-fast-1 ($3.50/$17.50) ❌ reseller
    // AFTER FIX:  grok-code → xai/grok-code-fast-1-0825 ($0.20/$1.50) ✅ original provider
    //
    // The azure_ai/ prefix indicates a reseller (Azure AI marketplace), which typically
    // has higher prices. The xai/ prefix indicates the original provider (X.AI/Grok),
    // which offers lower direct pricing. Our lookup should prefer the original provider.
    // =========================================================================

    let mut litellm = HashMap::new();

    // Reseller entry: azure_ai/ prefix with higher prices ($3.50/$17.50 per 1M tokens)
    litellm.insert(
        "azure_ai/grok-code-fast-1".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.0000035),  // $3.50/1M tokens
            output_cost_per_token: Some(0.0000175), // $17.50/1M tokens
            cache_read_input_token_cost: None,
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    // Original provider entry: xai/ prefix with lower prices ($0.20/$1.50 per 1M tokens)
    litellm.insert(
        "xai/grok-code-fast-1-0825".to_string(),
        ModelPricing {
            input_cost_per_token: Some(0.0000002),  // $0.20/1M tokens
            output_cost_per_token: Some(0.0000015), // $1.50/1M tokens
            cache_read_input_token_cost: Some(0.00000002),
            cache_creation_input_token_cost: None,
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("grok-code").unwrap();

    // Must prefer xai (original provider) over azure_ai (reseller)
    assert!(
        result.matched_key.starts_with("xai/"),
        "Expected xai/ prefix (original provider) but got: {}. \
             The lookup should prefer original providers over resellers.",
        result.matched_key
    );
    assert_eq!(
        result.matched_key, "xai/grok-code-fast-1-0825",
        "Should match the xai/grok-code-fast-1-0825 entry, not azure_ai/grok-code-fast-1"
    );

    // Verify we got the lower price (original provider)
    let pricing = &result.pricing;
    assert!(
        pricing.input_cost_per_token.unwrap() < 0.000001,
        "Input cost should be ~$0.20/1M (0.0000002), not ~$3.50/1M (reseller price)"
    );
    assert!(
        pricing.output_cost_per_token.unwrap() < 0.000005,
        "Output cost should be ~$1.50/1M (0.0000015), not ~$17.50/1M (reseller price)"
    );
}

#[test]
fn test_provider_preference_gemini_prefers_google_over_vertex() {
    let lookup = create_lookup();
    let result = lookup.lookup("gemini-2.5-pro").unwrap();
    assert_eq!(result.matched_key, "google/gemini-2.5-pro");
    assert_eq!(result.source, "LiteLLM");
    assert!(!result.matched_key.starts_with("vertex_ai"));
}

#[test]
fn test_is_original_provider() {
    assert!(is_original_provider("xai/grok-code"));
    assert!(is_original_provider("anthropic/claude-3"));
    assert!(is_original_provider("openai/gpt-4"));
    assert!(is_original_provider("google/gemini"));
    assert!(is_original_provider("x-ai/grok"));
    assert!(is_original_provider("mistral/mistral-small"));
    assert!(is_original_provider("zai/glm-4.7"));
    assert!(is_original_provider("zhipuai/glm-4.7"));
    assert!(!is_original_provider("azure_ai/grok"));
    assert!(!is_original_provider("bedrock/anthropic"));
    assert!(!is_original_provider("vertex_ai/gemini"));
    assert!(!is_original_provider("vertex-ai/gemini"));
    assert!(!is_original_provider("openrouter/openai/gpt-4"));
    assert!(!is_original_provider("unknown-provider/model"));
}

#[test]
fn test_is_reseller_provider() {
    assert!(is_reseller_provider("azure_ai/grok-code"));
    assert!(is_reseller_provider("azure/openai/gpt-4"));
    assert!(is_reseller_provider("bedrock/anthropic.claude"));
    assert!(is_reseller_provider("vertex_ai/gemini"));
    assert!(is_reseller_provider("together_ai/llama"));
    assert!(is_reseller_provider("groq/llama"));
    assert!(!is_reseller_provider("xai/grok"));
    assert!(!is_reseller_provider("anthropic/claude"));
    assert!(!is_reseller_provider("openai/gpt-4"));
}

// =========================================================================
// COST CALCULATION TESTS
// =========================================================================

#[test]
fn test_calculate_cost_gpt_5_2() {
    let lookup = create_lookup();
    // 1M input, 500K output tokens
    let cost = lookup.calculate_cost("gpt-5.2", 1_000_000, 500_000, 0, 0, 0);
    // input: 1M * 0.00000175 = 1.75, output: 500K * 0.000014 = 7.0
    assert!((cost - 8.75).abs() < 0.001);
}

#[test]
fn test_calculate_cost_claude_sonnet_4_5() {
    let lookup = create_lookup();
    // 100K input, 50K output, 200K cache read
    let cost = lookup.calculate_cost("claude-sonnet-4-5", 100_000, 50_000, 200_000, 0, 0);
    // input: 100K * 0.000003 = 0.30, output: 50K * 0.000015 = 0.75, cache: 200K * 3e-7 = 0.06
    assert!((cost - 1.11).abs() < 0.001);
}

#[test]
fn test_compute_cost_tiered_boundary_at_200k_uses_base_rates() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token": 0.000001,
                "input_cost_per_token_above_200k_tokens": 0.000002,
                "output_cost_per_token": 0.000003,
                "output_cost_per_token_above_200k_tokens": 0.000004
            }"#,
    )
    .unwrap();

    let cost = compute_cost(&pricing, 200_000, 200_000, 0, 0, 0);
    let expected = 200_000.0 * 0.000001 + 200_000.0 * 0.000003;

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_above_200k_splits_input_and_output() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token": 0.000001,
                "input_cost_per_token_above_200k_tokens": 0.000002,
                "output_cost_per_token": 0.000003,
                "output_cost_per_token_above_200k_tokens": 0.000004
            }"#,
    )
    .unwrap();

    let cost = compute_cost(&pricing, 200_001, 200_001, 0, 0, 0);
    let expected =
        (200_000.0 * 0.000001 + 1.0 * 0.000002) + (200_000.0 * 0.000003 + 1.0 * 0.000004);

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_above_272k_splits_gpt_5_5_tokens() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token": 0.000005,
                "input_cost_per_token_above_272k_tokens": 0.000010,
                "output_cost_per_token": 0.000030,
                "output_cost_per_token_above_272k_tokens": 0.000045,
                "cache_read_input_token_cost": 0.0000005,
                "cache_read_input_token_cost_above_272k_tokens": 0.000001
            }"#,
    )
    .unwrap();

    let cost = compute_cost(&pricing, 272_001, 272_001, 272_001, 0, 0);
    let expected = (272_000.0 * 0.000005 + 1.0 * 0.000010)
        + (272_000.0 * 0.000030 + 1.0 * 0.000045)
        + (272_000.0 * 0.0000005 + 1.0 * 0.000001);

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_uses_multiple_thresholds_in_order() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token": 0.000001,
                "input_cost_per_token_above_128k_tokens": 0.000002,
                "input_cost_per_token_above_256k_tokens": 0.000003,
                "input_cost_per_token_above_272k_tokens": 0.000004
            }"#,
    )
    .unwrap();

    let cost = compute_cost(&pricing, 300_000, 0, 0, 0, 0);
    let expected = (128_000.0 * 0.000001)
        + (128_000.0 * 0.000002)
        + (16_000.0 * 0.000003)
        + (28_000.0 * 0.000004);

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_is_applied_per_bucket() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token": 0.000001,
                "input_cost_per_token_above_200k_tokens": 0.000002,
                "output_cost_per_token": 0.000003,
                "output_cost_per_token_above_200k_tokens": 0.000004
            }"#,
    )
    .unwrap();

    let cost = compute_cost(&pricing, 200_001, 200_000, 0, 0, 0);
    let expected = (200_000.0 * 0.000001 + 1.0 * 0.000002) + (200_000.0 * 0.000003);

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_missing_base_input_only_charges_above_threshold() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "input_cost_per_token_above_200k_tokens": 0.000002
            }"#,
    )
    .unwrap();

    let at_threshold = compute_cost(&pricing, 200_000, 0, 0, 0, 0);
    let above_threshold = compute_cost(&pricing, 200_001, 0, 0, 0, 0);

    assert_eq!(at_threshold, 0.0);
    assert!((above_threshold - 0.000002).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_cache_read_applies_split() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "cache_read_input_token_cost": 0.0000001,
                "cache_read_input_token_cost_above_200k_tokens": 0.0000002
            }"#,
    )
    .unwrap();

    let at_threshold = compute_cost(&pricing, 0, 0, 200_000, 0, 0);
    let above_threshold = compute_cost(&pricing, 0, 0, 200_001, 0, 0);

    assert!((at_threshold - (200_000.0 * 0.0000001)).abs() < 1e-12);
    assert!((above_threshold - (200_000.0 * 0.0000001 + 0.0000002)).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_cache_write_applies_split() {
    let pricing: ModelPricing = serde_json::from_str(
        r#"{
                "cache_creation_input_token_cost": 0.0000003,
                "cache_creation_input_token_cost_above_200k_tokens": 0.0000004
            }"#,
    )
    .unwrap();

    let at_threshold = compute_cost(&pricing, 0, 0, 0, 200_000, 0);
    let above_threshold = compute_cost(&pricing, 0, 0, 0, 200_001, 0);

    assert!((at_threshold - (200_000.0 * 0.0000003)).abs() < 1e-12);
    assert!((above_threshold - (200_000.0 * 0.0000003 + 0.0000004)).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_without_above_rate_uses_base_for_all_tokens() {
    let pricing = ModelPricing {
        input_cost_per_token: Some(0.000001),
        ..Default::default()
    };

    let cost = compute_cost(&pricing, 250_000, 0, 0, 0, 0);

    assert!((cost - (250_000.0 * 0.000001)).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_invalid_above_rate_falls_back_to_base() {
    let pricing_negative = ModelPricing {
        input_cost_per_token: Some(0.000001),
        input_cost_per_token_above_200k_tokens: Some(-0.000002),
        ..Default::default()
    };
    let pricing_infinite = ModelPricing {
        input_cost_per_token: Some(0.000001),
        input_cost_per_token_above_200k_tokens: Some(f64::INFINITY),
        ..Default::default()
    };
    let pricing_nan = ModelPricing {
        input_cost_per_token: Some(0.000001),
        input_cost_per_token_above_200k_tokens: Some(f64::NAN),
        ..Default::default()
    };

    let expected = 200_001.0 * 0.000001;
    assert!((compute_cost(&pricing_negative, 200_001, 0, 0, 0, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_infinite, 200_001, 0, 0, 0, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_nan, 200_001, 0, 0, 0, 0) - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_reasoning_boundary_at_200k_uses_base_output_rate() {
    let pricing = ModelPricing {
        output_cost_per_token: Some(0.000003),
        output_cost_per_token_above_200k_tokens: Some(0.000004),
        ..Default::default()
    };

    let cost = compute_cost(&pricing, 0, 199_999, 0, 0, 1);
    let expected = 200_000.0 * 0.000003;

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_invalid_above_rate_falls_back_to_base_output_reasoning() {
    let pricing_negative = ModelPricing {
        output_cost_per_token: Some(0.000003),
        output_cost_per_token_above_200k_tokens: Some(-0.000004),
        ..Default::default()
    };
    let pricing_infinite = ModelPricing {
        output_cost_per_token: Some(0.000003),
        output_cost_per_token_above_200k_tokens: Some(f64::INFINITY),
        ..Default::default()
    };
    let pricing_nan = ModelPricing {
        output_cost_per_token: Some(0.000003),
        output_cost_per_token_above_200k_tokens: Some(f64::NAN),
        ..Default::default()
    };

    let expected = 200_001.0 * 0.000003;
    assert!((compute_cost(&pricing_negative, 0, 199_999, 0, 0, 2) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_infinite, 0, 199_999, 0, 0, 2) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_nan, 0, 199_999, 0, 0, 2) - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_invalid_above_rate_falls_back_to_base_cache_read() {
    let pricing_negative = ModelPricing {
        cache_read_input_token_cost: Some(0.0000001),
        cache_read_input_token_cost_above_200k_tokens: Some(-0.0000002),
        ..Default::default()
    };
    let pricing_infinite = ModelPricing {
        cache_read_input_token_cost: Some(0.0000001),
        cache_read_input_token_cost_above_200k_tokens: Some(f64::INFINITY),
        ..Default::default()
    };
    let pricing_nan = ModelPricing {
        cache_read_input_token_cost: Some(0.0000001),
        cache_read_input_token_cost_above_200k_tokens: Some(f64::NAN),
        ..Default::default()
    };

    let expected = 200_001.0 * 0.0000001;
    assert!((compute_cost(&pricing_negative, 0, 0, 200_001, 0, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_infinite, 0, 0, 200_001, 0, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_nan, 0, 0, 200_001, 0, 0) - expected).abs() < 1e-12);
}

#[test]
fn test_compute_cost_tiered_invalid_above_rate_falls_back_to_base_cache_write() {
    let pricing_negative = ModelPricing {
        cache_creation_input_token_cost: Some(0.0000003),
        cache_creation_input_token_cost_above_200k_tokens: Some(-0.0000004),
        ..Default::default()
    };
    let pricing_infinite = ModelPricing {
        cache_creation_input_token_cost: Some(0.0000003),
        cache_creation_input_token_cost_above_200k_tokens: Some(f64::INFINITY),
        ..Default::default()
    };
    let pricing_nan = ModelPricing {
        cache_creation_input_token_cost: Some(0.0000003),
        cache_creation_input_token_cost_above_200k_tokens: Some(f64::NAN),
        ..Default::default()
    };

    let expected = 200_001.0 * 0.0000003;
    assert!((compute_cost(&pricing_negative, 0, 0, 0, 200_001, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_infinite, 0, 0, 0, 200_001, 0) - expected).abs() < 1e-12);
    assert!((compute_cost(&pricing_nan, 0, 0, 0, 200_001, 0) - expected).abs() < 1e-12);
}

#[test]
fn test_provider_prefixed_non_opus_prefers_exact_openrouter_without_tier_advantage() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000123),
            output_cost_per_token: Some(0.0000456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-sonnet-4").unwrap();
    assert_eq!(resolved.source, "OpenRouter");
    assert_eq!(resolved.matched_key, "anthropic/claude-sonnet-4");
}

#[test]
fn test_provider_prefixed_exact_litellm_beats_stripped_generic_match() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            ..Default::default()
        },
    );
    litellm.insert(
        "openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let resolved = lookup.lookup("openai/gpt-4").unwrap();
    assert_eq!(resolved.source, "LiteLLM");
    assert_eq!(resolved.matched_key, "openai/gpt-4");
}

#[test]
fn test_provider_prefixed_override_requires_valid_base_and_above_pair() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4".into(),
        ModelPricing {
            // Above tier exists, but corresponding base is missing.
            // This must not qualify for provider-prefixed override.
            input_cost_per_token: None,
            input_cost_per_token_above_200k_tokens: Some(0.00002),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000123),
            output_cost_per_token: Some(0.0000456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-sonnet-4").unwrap();
    assert_eq!(resolved.source, "OpenRouter");
    assert_eq!(resolved.matched_key, "anthropic/claude-sonnet-4");
}

#[test]
fn test_provider_prefixed_override_rejects_invalid_base_even_with_above() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(f64::NAN),
            input_cost_per_token_above_200k_tokens: Some(0.00002),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000123),
            output_cost_per_token: Some(0.0000456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-sonnet-4").unwrap();
    assert_eq!(resolved.source, "OpenRouter");
    assert_eq!(resolved.matched_key, "anthropic/claude-sonnet-4");
}

#[test]
fn test_provider_prefixed_override_allows_zero_base_with_valid_above() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4".into(),
        ModelPricing {
            // Policy: base=0 with valid above is a valid tier pair.
            input_cost_per_token: Some(0.0),
            input_cost_per_token_above_200k_tokens: Some(0.00002),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000123),
            output_cost_per_token: Some(0.0000456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-sonnet-4").unwrap();
    assert_eq!(resolved.source, "LiteLLM");
    assert_eq!(resolved.matched_key, "claude-sonnet-4");
}

#[test]
fn test_provider_prefixed_cache_only_tier_keeps_exact_openrouter() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4".into(),
        ModelPricing {
            cache_read_input_token_cost: Some(0.0000001),
            cache_read_input_token_cost_above_200k_tokens: Some(0.0000002),
            cache_creation_input_token_cost: Some(0.0000003),
            cache_creation_input_token_cost_above_200k_tokens: Some(0.0000004),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-sonnet-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000123),
            output_cost_per_token: Some(0.0000456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-sonnet-4").unwrap();
    assert_eq!(resolved.source, "OpenRouter");
    assert_eq!(resolved.matched_key, "anthropic/claude-sonnet-4");
}

#[test]
fn test_provider_prefixed_opus_4_6_prefers_litellm_tiered_pricing() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.00001),
            input_cost_per_token_above_200k_tokens: Some(0.00002),
            output_cost_per_token: Some(0.00005),
            output_cost_per_token_above_200k_tokens: Some(0.00006),
            cache_read_input_token_cost: Some(0.000001),
            cache_read_input_token_cost_above_200k_tokens: Some(0.000002),
            cache_creation_input_token_cost: Some(0.000003),
            cache_creation_input_token_cost_above_200k_tokens: Some(0.000004),
            ..Default::default()
        },
    );

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.123),
            output_cost_per_token: Some(0.456),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let resolved = lookup.lookup("anthropic/claude-opus-4-6").unwrap();
    assert_eq!(resolved.source, "LiteLLM");
    assert_eq!(resolved.matched_key, "claude-opus-4-6");

    let cost = lookup.calculate_cost("anthropic/claude-opus-4-6", 200_001, 0, 0, 0, 0);
    let expected = 200_000.0 * 0.00001 + 0.00002;
    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_anthropic_prefixed_sonnet_variant_uses_canonical_pricing() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-sonnet-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            cache_read_input_token_cost: Some(0.0000003),
            cache_creation_input_token_cost: Some(0.00000375),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let resolved = lookup.lookup("anthropic/claude-4-6-sonnet").unwrap();
    assert_eq!(resolved.source, "LiteLLM");
    assert_eq!(resolved.matched_key, "claude-sonnet-4-6");

    let cost = lookup.calculate_cost("anthropic/claude-4-6-sonnet", 100, 20, 10, 5, 0);
    let expected = 100.0 * 0.000003 + 20.0 * 0.000015 + 10.0 * 0.0000003 + 5.0 * 0.00000375;
    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_anthropic_prefixed_haiku_variant_uses_canonical_pricing() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-haiku-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0000008),
            output_cost_per_token: Some(0.000004),
            cache_read_input_token_cost: Some(0.00000008),
            cache_creation_input_token_cost: Some(0.000001),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let resolved = lookup.lookup("anthropic/claude-4-5-haiku").unwrap();
    assert_eq!(resolved.source, "LiteLLM");
    assert_eq!(resolved.matched_key, "claude-haiku-4-5");

    let cost = lookup.calculate_cost("anthropic/claude-4-5-haiku", 100, 20, 10, 5, 0);
    let expected = 100.0 * 0.0000008 + 20.0 * 0.000004 + 10.0 * 0.00000008 + 5.0 * 0.000001;
    assert!((cost - expected).abs() < 1e-12);
}

/// Regression test for #336: subscription-based resellers (e.g. Perplexity) with
/// all-None pricing should not shadow valid entries during provider-aware lookup.
/// `perplexity/anthropic/claude-opus-4-6` matches provider hint "anthropic" via
/// its path segments, but has no per-token pricing. The lookup must fall through
/// to the exact `claude-opus-4-6` entry that has real pricing data.
#[test]
fn test_none_pricing_reseller_does_not_shadow_real_entry() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            cache_read_input_token_cost: Some(0.0000005),
            cache_creation_input_token_cost: Some(0.00000625),
            ..Default::default()
        },
    );
    // Perplexity entry: matches "anthropic" hint but has no pricing
    litellm.insert(
        "perplexity/anthropic/claude-opus-4-6".into(),
        ModelPricing::default(),
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    // With provider hint "anthropic", should find the real entry, not perplexity
    let result = lookup.lookup_with_provider("claude-opus-4-6", Some("anthropic"));
    assert!(result.is_some(), "lookup should succeed");
    let result = result.unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-6");
    assert!(result.pricing.input_cost_per_token.is_some());

    // Cost should be non-zero
    let cost = lookup.calculate_cost("claude-opus-4-6", 100_000, 50_000, 0, 0, 0);
    assert!(cost > 0.0, "cost should be positive, got {}", cost);
}

#[test]
fn none_pricing_provider_match_does_not_strip_latest_suffix_to_priced_candidate() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-6-20250301".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            ..Default::default()
        },
    );
    litellm.insert(
        "perplexity/anthropic/claude-opus-4-6-20250301".into(),
        ModelPricing::default(),
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    let result = lookup.lookup_with_provider("claude-opus-4-6-latest", Some("anthropic"));
    assert!(result.is_none());
}

#[test]
fn test_none_pricing_exact_litellm_does_not_shadow_openrouter_model_part() {
    let mut litellm = HashMap::new();
    litellm.insert("claude-opus-4-6".into(), ModelPricing::default());

    let mut openrouter = HashMap::new();
    openrouter.insert(
        "anthropic/claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000005),
            output_cost_per_token: Some(0.000025),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, openrouter);
    let result = lookup.lookup("claude-opus-4-6").unwrap();

    assert_eq!(result.source, "OpenRouter");
    assert_eq!(result.matched_key, "anthropic/claude-opus-4-6");

    let cost = lookup.calculate_cost("claude-opus-4-6", 100, 20, 0, 0, 0);
    assert!(cost > 0.0, "cost should use priced fallback, got {cost}");
}

#[test]
fn test_none_pricing_provider_exact_does_not_shadow_stripped_priced_entry() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "anthropic/claude-sonnet-4-5".into(),
        ModelPricing::default(),
    );
    litellm.insert(
        "claude-sonnet-4-5".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000003),
            output_cost_per_token: Some(0.000015),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("anthropic/claude-sonnet-4-5").unwrap();

    assert_eq!(result.source, "LiteLLM");
    assert_eq!(result.matched_key, "claude-sonnet-4-5");

    let cost = lookup.calculate_cost("anthropic/claude-sonnet-4-5", 100, 20, 0, 0, 0);
    assert!(
        cost > 0.0,
        "cost should use stripped priced entry, got {cost}"
    );
}

#[test]
fn test_zero_pricing_exact_entry_is_usable() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "free-model".into(),
        ModelPricing {
            input_cost_per_token: Some(0.0),
            output_cost_per_token: Some(0.0),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup.lookup("free-model").unwrap();

    assert_eq!(result.matched_key, "free-model");
    assert_eq!(lookup.calculate_cost("free-model", 100, 20, 0, 0, 0), 0.0);
}

#[test]
fn test_calculate_cost_tiered_all_buckets_with_reasoning_threshold_crossing() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "claude-opus-4-6".into(),
        ModelPricing {
            input_cost_per_token: Some(0.000001),
            input_cost_per_token_above_200k_tokens: Some(0.000002),
            output_cost_per_token: Some(0.000003),
            output_cost_per_token_above_200k_tokens: Some(0.000004),
            cache_read_input_token_cost: Some(0.0000001),
            cache_read_input_token_cost_above_200k_tokens: Some(0.0000002),
            cache_creation_input_token_cost: Some(0.0000003),
            cache_creation_input_token_cost_above_200k_tokens: Some(0.0000004),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let cost = lookup.calculate_cost("claude-opus-4-6", 200_001, 199_999, 200_001, 200_001, 2);

    let expected_input = 200_000.0 * 0.000001 + 0.000002;
    let expected_output = 200_000.0 * 0.000003 + 0.000004; // output + reasoning = 200_001
    let expected_cache_read = 200_000.0 * 0.0000001 + 0.0000002;
    let expected_cache_write = 200_000.0 * 0.0000003 + 0.0000004;
    let expected = expected_input + expected_output + expected_cache_read + expected_cache_write;

    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn test_calculate_cost_unknown_model() {
    let lookup = create_lookup();
    let cost = lookup.calculate_cost("nonexistent-model", 1_000_000, 500_000, 0, 0, 0);
    assert_eq!(cost, 0.0);
}

// =========================================================================
// INTELLIGENT PREFIX/SUFFIX STRIPPING TESTS
// =========================================================================

#[test]
fn test_antigravity_prefix_gemini_3_flash() {
    let lookup = create_lookup();
    assert!(lookup.lookup("antigravity-gemini-3-flash").is_none());
}

#[test]
fn test_antigravity_prefix_gemini_3_pro() {
    let lookup = create_lookup();
    assert!(lookup.lookup("antigravity-gemini-3-pro").is_none());
}

#[test]
fn test_antigravity_prefix_with_tier_suffix() {
    let lookup = create_lookup();
    assert!(lookup.lookup("antigravity-gemini-3-pro-high").is_none());
}

#[test]
fn claude_direct_normalization_can_match_inside_prefixed_string() {
    let lookup = create_lookup();
    let result = lookup.lookup("antigravity-claude-sonnet-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
    assert_eq!(result.source, "LiteLLM");
}

#[test]
fn test_antigravity_prefix_gpt() {
    let lookup = create_lookup();
    assert!(lookup.lookup("antigravity-gpt-4o").is_none());
}

#[test]
fn test_antigravity_prefix_case_insensitive() {
    let lookup = create_lookup();
    assert!(lookup.lookup("Antigravity-gpt-4o").is_none());
}

#[test]
fn test_antigravity_cost_calculation() {
    let lookup = create_lookup();
    let cost_with_prefix =
        lookup.calculate_cost("antigravity-gpt-5.2", 1_000_000, 500_000, 0, 0, 0);
    assert_eq!(cost_with_prefix, 0.0);
}

// New tests for intelligent detection

#[test]
fn test_unknown_prefix_generic() {
    let lookup = create_lookup();
    assert!(lookup.lookup("myplugin-gpt-4o").is_none());
}

#[test]
fn claude_direct_normalization_can_match_inside_two_segment_prefix() {
    let lookup = create_lookup();
    let result = lookup.lookup("router-v2-claude-sonnet-4-5").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
}

#[test]
fn claude_direct_normalization_can_match_thinking_suffix() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5-thinking").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
}

#[test]
fn claude_direct_normalization_can_match_stacked_suffixes() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-opus-4-5-thinking-pro").unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-5");
}

#[test]
fn claude_direct_normalization_can_match_prefixed_thinking_suffix() {
    let lookup = create_lookup();
    let result = lookup
        .lookup("antigravity-claude-opus-4-5-thinking")
        .unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-5");
}

#[test]
fn claude_direct_normalization_can_match_prefixed_stacked_suffixes() {
    let lookup = create_lookup();
    let result = lookup
        .lookup("antigravity-claude-opus-4-5-thinking-high")
        .unwrap();
    assert_eq!(result.matched_key, "claude-opus-4-5");
}

#[test]
fn test_no_false_positive_valid_model() {
    let lookup = create_lookup();
    // gpt-4o-mini is a valid model, should NOT strip "gpt"
    let result = lookup.lookup("gpt-4o-mini").unwrap();
    assert_eq!(result.matched_key, "gpt-4o-mini");
}

#[test]
fn claude_direct_normalization_can_match_high_suffix() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5-high").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
}

#[test]
fn claude_direct_normalization_can_match_xhigh_suffix() {
    let lookup = create_lookup();
    let result = lookup.lookup("claude-sonnet-4-5-xhigh").unwrap();
    assert_eq!(result.matched_key, "claude-sonnet-4-5");
}

#[test]
fn does_not_strip_low_suffix_from_gpt_model() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-4o-low").is_none());
}

#[test]
fn does_not_strip_codex_suffix_from_gpt_model() {
    let lookup = create_lookup();
    assert!(lookup.lookup("gpt-5.2-codex").is_none());
}

#[test]
fn test_provider_hint_empty_and_unknown_treated_as_none() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure_ai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    let r_none = lookup.lookup_with_provider("gpt-4", None).unwrap();
    let r_empty = lookup.lookup_with_provider("gpt-4", Some("")).unwrap();
    let r_unknown = lookup
        .lookup_with_provider("gpt-4", Some("unknown"))
        .unwrap();

    assert_eq!(r_none.matched_key, r_empty.matched_key);
    assert_eq!(r_none.matched_key, r_unknown.matched_key);
}

#[test]
fn test_provider_hint_mistralai_matches_mistral_keys() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "mistralai/mistral-large".into(),
        ModelPricing {
            input_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup_with_provider("mistral-large", Some("mistral"))
        .unwrap();
    assert_eq!(result.matched_key, "mistralai/mistral-large");
}

#[test]
fn test_provider_hint_minimax_matches_minimax_keys() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "minimax/minimax-m2.1".into(),
        ModelPricing {
            input_cost_per_token: Some(0.002),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let result = lookup
        .lookup_with_provider("MiniMax-M2.1", Some("minimax"))
        .unwrap();
    assert_eq!(result.matched_key, "minimax/minimax-m2.1");
}

#[test]
fn test_prefixed_model_with_conflicting_provider_uses_provider_aware_path() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure/openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    let r_azure = lookup
        .lookup_with_provider("openai/gpt-4", Some("azure"))
        .unwrap();
    assert_eq!(
        r_azure.matched_key, "azure/openai/gpt-4",
        "should prefer azure key when provider_id=azure"
    );

    let r_openai = lookup
        .lookup_with_provider("openai/gpt-4", Some("openai"))
        .unwrap();
    assert_eq!(
        r_openai.matched_key, "openai/gpt-4",
        "should use exact prefixed key when provider_id matches prefix"
    );

    let r_none = lookup.lookup_with_provider("openai/gpt-4", None).unwrap();
    assert_eq!(
        r_none.matched_key, "openai/gpt-4",
        "should use exact prefixed key when no provider hint"
    );
}

#[test]
fn test_prefixed_model_conflicting_provider_falls_back_to_stripped() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );
    litellm.insert(
        "gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.001),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    let r = lookup
        .lookup_with_provider("openai/gpt-4", Some("azure"))
        .unwrap();
    assert_eq!(
        r.matched_key, "gpt-4",
        "with no azure-specific key, should fall back to stripped generic"
    );
}

#[test]
fn test_compound_provider_hint_prefers_reseller_over_prefix() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );
    litellm.insert(
        "azure/openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.02),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());
    let r = lookup
        .lookup_with_provider("openai/gpt-4", Some("azure/openai"))
        .unwrap();
    assert_eq!(
        r.matched_key, "azure/openai/gpt-4",
        "compound hint azure/openai should prefer azure-specific key over openai/ prefix"
    );
}

#[test]
fn test_source_and_provider_normalizes_unknown_hint() {
    let mut litellm = HashMap::new();
    litellm.insert(
        "openai/gpt-4".into(),
        ModelPricing {
            input_cost_per_token: Some(0.01),
            ..Default::default()
        },
    );

    let lookup = PricingLookup::new(litellm, HashMap::new());

    let r_unknown = lookup
        .lookup_with_source_and_provider("openai/gpt-4", None, Some("unknown"))
        .unwrap();
    let r_none = lookup
        .lookup_with_source_and_provider("openai/gpt-4", None, None)
        .unwrap();
    assert_eq!(
        r_unknown.matched_key, r_none.matched_key,
        "unknown hint via source_and_provider should behave like None"
    );
}
