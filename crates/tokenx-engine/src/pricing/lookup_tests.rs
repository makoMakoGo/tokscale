use super::*;

fn pricing(input: f64, output: f64) -> ModelPricing {
    ModelPricing {
        input_cost_per_token: Some(input),
        output_cost_per_token: Some(output),
        ..Default::default()
    }
}

fn lookup_with_all_catalogs(
    litellm: HashMap<String, ModelPricing>,
    openrouter: HashMap<String, ModelPricing>,
    models_dev: HashMap<String, ModelPricing>,
) -> PricingLookup {
    PricingLookup::new_with_models_dev(litellm, openrouter, models_dev)
}

#[test]
fn exact_lookup_is_case_insensitive() {
    let lookup = PricingLookup::new(
        HashMap::from([("GPT-5.3-Codex".into(), pricing(1.0, 2.0))]),
        HashMap::new(),
    );

    let result = lookup.lookup("gpt-5.3-codex").unwrap();

    assert_eq!(result.pricing_source, "LiteLLM");
    assert_eq!(result.matched_key, "GPT-5.3-Codex");
}

#[test]
fn inferred_provider_enables_provider_scoped_exact_lookup() {
    let lookup = PricingLookup::new(
        HashMap::new(),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(1.0, 2.0))]),
    );

    let result = lookup.lookup("gpt-5.3-codex").unwrap();

    assert_eq!(result.pricing_source, "OpenRouter");
    assert_eq!(result.matched_key, "openai/gpt-5.3-codex");
}

#[test]
fn observed_provider_takes_precedence_over_family_inference() {
    let lookup = PricingLookup::new(
        HashMap::from([
            ("openai/gpt-5.3-codex".into(), pricing(1.0, 2.0)),
            ("gpt-5.3-codex".into(), pricing(3.0, 4.0)),
        ]),
        HashMap::new(),
    );

    let result = lookup
        .lookup_with_provider("gpt-5.3-codex", Some("owl"))
        .unwrap();

    assert_eq!(result.matched_key, "gpt-5.3-codex");
    assert_eq!(result.pricing.input_cost_per_token, Some(3.0));
}

#[test]
fn provider_scoped_rows_across_all_catalogs_beat_unscoped_rows() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("gpt-5.3-codex".into(), pricing(1.0, 1.0))]),
        HashMap::new(),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(3.0, 3.0))]),
    );

    let result = lookup
        .lookup_with_provider("gpt-5.3-codex", Some("openai"))
        .unwrap();

    assert_eq!(result.pricing_source, "Models.dev");
    assert_eq!(result.matched_key, "openai/gpt-5.3-codex");
}

#[test]
fn catalog_order_breaks_ties_inside_provider_scoped_class() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(1.0, 1.0))]),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(2.0, 2.0))]),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(3.0, 3.0))]),
    );

    let result = lookup
        .lookup_with_provider("gpt-5.3-codex", Some("openai"))
        .unwrap();

    assert_eq!(result.pricing_source, "LiteLLM");
    assert_eq!(result.pricing.input_cost_per_token, Some(1.0));
}

#[test]
fn catalog_order_breaks_ties_inside_unscoped_class() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("mystery-model".into(), pricing(1.0, 1.0))]),
        HashMap::from([("mystery-model".into(), pricing(2.0, 2.0))]),
        HashMap::from([("mystery-model".into(), pricing(3.0, 3.0))]),
    );

    let result = lookup.lookup("mystery-model").unwrap();

    assert_eq!(result.pricing_source, "LiteLLM");
    assert_eq!(result.pricing.input_cost_per_token, Some(1.0));
}

#[test]
fn unknown_observation_uses_model_family_inference() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(1.0, 1.0))]),
        HashMap::from([("gpt-5.3-codex".into(), pricing(2.0, 2.0))]),
        HashMap::new(),
    );

    let result = lookup
        .lookup_with_provider("gpt-5.3-codex", Some("unknown"))
        .unwrap();

    assert_eq!(result.matched_key, "openai/gpt-5.3-codex");
    assert_eq!(result.pricing.input_cost_per_token, Some(1.0));
}

#[test]
fn unknown_scope_after_inference_uses_unscoped_exact_rows_only() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("private-route/mystery-model".into(), pricing(1.0, 1.0))]),
        HashMap::from([("mystery-model".into(), pricing(2.0, 2.0))]),
        HashMap::new(),
    );

    let result = lookup
        .lookup_with_provider("mystery-model", Some("unknown"))
        .unwrap();

    assert_eq!(result.matched_key, "mystery-model");
    assert_eq!(result.pricing.input_cost_per_token, Some(2.0));
}

#[test]
fn forced_source_is_an_exact_catalog_boundary() {
    let lookup = lookup_with_all_catalogs(
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(1.0, 1.0))]),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(2.0, 2.0))]),
        HashMap::from([("openai/gpt-5.3-codex".into(), pricing(3.0, 3.0))]),
    );

    let openrouter = lookup
        .lookup_with_pricing_source_and_provider(
            "gpt-5.3-codex",
            Some("OpenRouter"),
            Some("openai"),
        )
        .unwrap();
    let models_dev = lookup
        .lookup_with_pricing_source_and_provider(
            "gpt-5.3-codex",
            Some("models.dev"),
            Some("openai"),
        )
        .unwrap();

    assert_eq!(openrouter.pricing_source, "OpenRouter");
    assert_eq!(openrouter.pricing.input_cost_per_token, Some(2.0));
    assert_eq!(models_dev.pricing_source, "Models.dev");
    assert_eq!(models_dev.pricing.input_cost_per_token, Some(3.0));
    assert!(lookup
        .lookup_with_pricing_source("gpt-5.3-codex", Some("modelsdev"))
        .is_none());
}

#[test]
fn non_exact_model_ids_are_ordinary_misses() {
    let lookup = PricingLookup::new(
        HashMap::from([
            ("openai/gpt-5-preview".into(), pricing(1.0, 1.0)),
            ("some-special-model".into(), pricing(1.0, 1.0)),
            ("foo-1.2".into(), pricing(1.0, 1.0)),
            ("azure/route-model".into(), pricing(1.0, 1.0)),
        ]),
        HashMap::new(),
    );

    assert!(lookup.lookup("gpt-5").is_none());
    assert!(lookup.lookup("special-model").is_none());
    assert!(lookup.lookup("foo-1-2").is_none());
    assert!(lookup
        .lookup_with_provider("route-model", Some("openai"))
        .is_none());
}

#[test]
fn catalog_model_component_must_be_the_exact_terminal_segment() {
    let lookup = PricingLookup::new(
        HashMap::from([(
            "fireworks/accounts/openai/models/gpt-5.3-codex".into(),
            pricing(1.0, 1.0),
        )]),
        HashMap::new(),
    );

    let result = lookup
        .lookup_with_provider("gpt-5.3-codex", Some("fireworks"))
        .unwrap();
    assert_eq!(
        result.matched_key,
        "fireworks/accounts/openai/models/gpt-5.3-codex"
    );

    assert!(lookup
        .lookup_with_provider("models/gpt-5.3-codex", Some("fireworks"))
        .is_some());
}

#[test]
fn unusable_rows_are_skipped_and_explicit_zero_is_valid() {
    let lookup = PricingLookup::new(
        HashMap::from([("openai/gpt-5.3-codex".into(), ModelPricing::default())]),
        HashMap::from([(
            "openai/gpt-5.3-codex".into(),
            ModelPricing {
                input_cost_per_token: Some(0.0),
                ..Default::default()
            },
        )]),
    );

    let result = lookup.lookup("gpt-5.3-codex").unwrap();

    assert_eq!(result.pricing_source, "OpenRouter");
    assert_eq!(result.pricing.input_cost_per_token, Some(0.0));
}

#[test]
fn standalone_lookup_uses_shared_model_canonicalizer() {
    let lookup = PricingLookup::new(
        HashMap::from([("gpt-5.5".into(), pricing(1.0, 2.0))]),
        HashMap::new(),
    );

    let result = lookup.lookup("openai/GPT-5.5 (high)").unwrap();

    assert_eq!(result.matched_key, "gpt-5.5");
}

#[test]
fn lookup_cache_keeps_provider_scopes_separate() {
    let lookup = PricingLookup::new(
        HashMap::from([
            ("openai/shared-model".into(), pricing(1.0, 1.0)),
            ("anthropic/shared-model".into(), pricing(2.0, 2.0)),
        ]),
        HashMap::new(),
    );

    let openai = lookup
        .lookup_with_provider("shared-model", Some("openai"))
        .unwrap();
    let anthropic = lookup
        .lookup_with_provider("shared-model", Some("anthropic"))
        .unwrap();

    assert_eq!(openai.pricing.input_cost_per_token, Some(1.0));
    assert_eq!(anthropic.pricing.input_cost_per_token, Some(2.0));
}

#[test]
fn calculate_cost_combines_reasoning_with_output_and_clamps_negative_tokens() {
    let lookup = PricingLookup::new(
        HashMap::from([("mystery-model".into(), pricing(0.5, 2.0))]),
        HashMap::new(),
    );

    let cost = lookup
        .calculate_cost("mystery-model", -10, 3, -20, -30, 2)
        .unwrap();

    assert_eq!(cost, 10.0);
}

#[test]
fn compute_cost_applies_multiple_tiers_in_order() {
    let model_pricing = ModelPricing {
        input_cost_per_token: Some(1.0),
        input_cost_per_token_above_128k_tokens: Some(2.0),
        input_cost_per_token_above_200k_tokens: Some(3.0),
        input_cost_per_token_above_256k_tokens: Some(4.0),
        input_cost_per_token_above_272k_tokens: Some(5.0),
        ..Default::default()
    };

    let cost = compute_cost(&model_pricing, 300_000, 0, 0, 0, 0).unwrap();
    let expected = 128_000.0 + 72_000.0 * 2.0 + 56_000.0 * 3.0 + 16_000.0 * 4.0 + 28_000.0 * 5.0;

    assert_eq!(cost, expected);
}

#[test]
fn compute_cost_applies_cache_tiers_per_bucket() {
    let model_pricing = ModelPricing {
        cache_read_input_token_cost: Some(1.0),
        cache_read_input_token_cost_above_200k_tokens: Some(2.0),
        cache_read_input_token_cost_above_272k_tokens: Some(3.0),
        cache_creation_input_token_cost: Some(4.0),
        cache_creation_input_token_cost_above_200k_tokens: Some(5.0),
        ..Default::default()
    };

    let cost = compute_cost(&model_pricing, 0, 0, 300_000, 300_000, 0).unwrap();
    let expected_cache_read = 200_000.0 + 72_000.0 * 2.0 + 28_000.0 * 3.0;
    let expected_cache_write = 200_000.0 * 4.0 + 100_000.0 * 5.0;

    assert_eq!(cost, expected_cache_read + expected_cache_write);
}

#[test]
fn compute_cost_ignores_invalid_prices() {
    let model_pricing = ModelPricing {
        input_cost_per_token: Some(f64::NAN),
        output_cost_per_token: Some(-1.0),
        cache_read_input_token_cost: Some(f64::INFINITY),
        ..Default::default()
    };

    assert_eq!(
        compute_cost(&model_pricing, 10, 10, 10, 10, 10).unwrap(),
        0.0
    );
}

#[test]
fn compute_cost_rejects_output_reasoning_token_overflow() {
    let model_pricing = pricing(0.0, 1.0);
    let error = compute_cost(&model_pricing, 0, i64::MAX, 0, 0, 1).unwrap_err();

    assert_eq!(error, PricingComputationError::OutputReasoningTokenOverflow);
}

#[test]
fn compute_cost_rejects_non_finite_component_cost() {
    let model_pricing = pricing(f64::MAX, 0.0);
    let error = compute_cost(&model_pricing, i64::MAX, 0, 0, 0, 0).unwrap_err();

    assert_eq!(
        error,
        PricingComputationError::NonFiniteCost { component: "input" }
    );
}
