use anyhow::Result;

pub(crate) fn run_pricing_lookup(
    model_id: &str,
    json: bool,
    provider: Option<&str>,
    no_spinner: bool,
) -> Result<()> {
    use colored::Colorize;
    use indicatif::ProgressBar;
    use indicatif::ProgressStyle;
    use tokio::runtime::Runtime;
    use tokscale_core::pricing::PricingService;

    if model_id.eq_ignore_ascii_case("list-overrides") {
        return run_pricing_list_overrides(json);
    }

    let provider_normalized = provider.map(|p| p.to_lowercase());
    if let Some(ref p) = provider_normalized {
        if p != "custom" && p != "litellm" && p != "openrouter" && p != "models.dev" {
            println!(
                "\n  {}",
                format!("Invalid provider: {}", provider.unwrap_or("")).red()
            );
            println!(
                "{}\n",
                "  Valid providers: custom, litellm, openrouter, models.dev".bright_black()
            );
            std::process::exit(1);
        }
    }

    let spinner = if no_spinner {
        None
    } else {
        let provider_label = provider.map(|p| format!(" from {}", p)).unwrap_or_default();
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::default_spinner());
        pb.set_message(format!("Fetching pricing data{}...", provider_label));
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        Some(pb)
    };

    let rt = Runtime::new()?;
    let result = match rt.block_on(async {
        let svc = PricingService::get_or_init().await?;
        Ok::<_, String>(svc.lookup_with_source(model_id, provider_normalized.as_deref()))
    }) {
        Ok(result) => result,
        Err(err) => {
            if let Some(pb) = spinner {
                pb.finish_and_clear();
            }
            if json {
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct ErrorOutput {
                    error: String,
                    model_id: String,
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ErrorOutput {
                        error: err,
                        model_id: model_id.to_string(),
                    })?
                );
                std::process::exit(1);
            }
            return Err(anyhow::anyhow!(err));
        }
    };

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    if json {
        match result {
            Some(pricing) => {
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct PricingValues {
                    input_cost_per_token: f64,
                    output_cost_per_token: f64,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    cache_read_input_token_cost: Option<f64>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    cache_creation_input_token_cost: Option<f64>,
                }

                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct PricingOutput {
                    model_id: String,
                    matched_key: String,
                    source: String,
                    pricing: PricingValues,
                }

                let output = PricingOutput {
                    model_id: model_id.to_string(),
                    matched_key: pricing.matched_key,
                    source: pricing.source,
                    pricing: PricingValues {
                        input_cost_per_token: pricing.pricing.input_cost_per_token.unwrap_or(0.0),
                        output_cost_per_token: pricing.pricing.output_cost_per_token.unwrap_or(0.0),
                        cache_read_input_token_cost: pricing.pricing.cache_read_input_token_cost,
                        cache_creation_input_token_cost: pricing
                            .pricing
                            .cache_creation_input_token_cost,
                    },
                };

                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            None => {
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct ErrorOutput {
                    error: String,
                    model_id: String,
                }

                let output = ErrorOutput {
                    error: "Model not found".to_string(),
                    model_id: model_id.to_string(),
                };

                println!("{}", serde_json::to_string_pretty(&output)?);
                std::process::exit(1);
            }
        }
    } else {
        match result {
            Some(pricing) => {
                println!("\n  Pricing for: {}", model_id.bold());
                println!("  Matched key: {}", pricing.matched_key);
                let source_label = match pricing.source.to_lowercase().as_str() {
                    "custom" => "Custom",
                    "litellm" => "LiteLLM",
                    "openrouter" => "OpenRouter",
                    "models.dev" => "Models.dev",
                    _ => pricing.source.as_str(),
                };
                println!("  Source: {}", source_label);
                println!();
                let input = pricing.pricing.input_cost_per_token.unwrap_or(0.0);
                let output = pricing.pricing.output_cost_per_token.unwrap_or(0.0);
                println!("  Input:  ${:.2} / 1M tokens", input * 1_000_000.0);
                println!("  Output: ${:.2} / 1M tokens", output * 1_000_000.0);
                if let Some(cache_read) = pricing.pricing.cache_read_input_token_cost {
                    println!(
                        "  Cache Read:  ${:.2} / 1M tokens",
                        cache_read * 1_000_000.0
                    );
                }
                if let Some(cache_write) = pricing.pricing.cache_creation_input_token_cost {
                    println!(
                        "  Cache Write: ${:.2} / 1M tokens",
                        cache_write * 1_000_000.0
                    );
                }
                println!();
            }
            None => {
                println!("\n  {}\n", format!("Model not found: {}", model_id).red());
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

pub(crate) fn run_pricing_list_overrides(json: bool) -> Result<()> {
    use colored::Colorize;
    use tokscale_core::pricing::custom::CustomPricing;
    use tokscale_core::pricing::ModelPricing;

    fn per_million(value: Option<f64>) -> Option<f64> {
        value.map(|v| v * 1_000_000.0)
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct OverrideEntry {
        model_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_cost_per_million_tokens: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_cost_per_million_tokens: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_read_input_token_cost_per_million_tokens: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_creation_input_token_cost_per_million_tokens: Option<f64>,
    }

    fn entry(model_id: &str, pricing: &ModelPricing) -> OverrideEntry {
        OverrideEntry {
            model_id: model_id.to_string(),
            input_cost_per_million_tokens: per_million(pricing.input_cost_per_token),
            output_cost_per_million_tokens: per_million(pricing.output_cost_per_token),
            cache_read_input_token_cost_per_million_tokens: per_million(
                pricing.cache_read_input_token_cost,
            ),
            cache_creation_input_token_cost_per_million_tokens: per_million(
                pricing.cache_creation_input_token_cost,
            ),
        }
    }

    let path = CustomPricing::default_path();
    let overrides = CustomPricing::load_from_path(&path);
    let mut entries: Vec<OverrideEntry> = overrides
        .entries()
        .map(|(model_id, pricing)| entry(model_id, pricing))
        .collect();
    entries.sort_by(|a, b| a.model_id.cmp(&b.model_id));

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Output {
            path: String,
            count: usize,
            models: Vec<OverrideEntry>,
        }

        println!(
            "{}",
            serde_json::to_string_pretty(&Output {
                path: path.display().to_string(),
                count: entries.len(),
                models: entries,
            })?
        );
        return Ok(());
    }

    if entries.is_empty() {
        println!(
            "\n  {}\n  Tried: {}\n",
            "No custom pricing overrides loaded".yellow(),
            path.display()
        );
        return Ok(());
    }

    println!("\n  {}", "Custom pricing overrides".bold());
    println!("  Path: {}", path.display());
    println!("  Loaded once at startup; restart tokscale after editing this file.");
    println!();

    for entry in entries {
        println!("  {}", entry.model_id.bold());
        if let Some(input) = entry.input_cost_per_million_tokens {
            println!("    Input:  ${:.2} / 1M tokens", input);
        }
        if let Some(output) = entry.output_cost_per_million_tokens {
            println!("    Output: ${:.2} / 1M tokens", output);
        }
        if let Some(cache_read) = entry.cache_read_input_token_cost_per_million_tokens {
            println!("    Cache Read:  ${:.2} / 1M tokens", cache_read);
        }
        if let Some(cache_write) = entry.cache_creation_input_token_cost_per_million_tokens {
            println!("    Cache Write: ${:.2} / 1M tokens", cache_write);
        }
    }
    println!();

    Ok(())
}
