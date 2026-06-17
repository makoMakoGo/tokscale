use super::litellm::ModelPricing;
use super::{cache, emit_diagnostic, PricingDiagnosticSink, PricingDiagnostics};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

const CACHE_FILENAME: &str = "pricing-openrouter.json";
const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 200;
const MAX_CONCURRENT_REQUESTS: usize = 10;

/// Structs for `/api/v1/models` endpoint (list all models).

#[derive(Deserialize)]
struct ModelListPricing {
    prompt: String,
    completion: String,
}

#[derive(Deserialize)]
struct ModelListItem {
    id: String,
    pricing: Option<ModelListPricing>,
}

#[derive(Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelListItem>,
}

/// Structs for `/api/v1/models/{id}/endpoints` endpoint (author pricing).

#[derive(Deserialize)]
struct EndpointPricing {
    prompt: String,
    completion: String,
    #[serde(default)]
    input_cache_read: Option<String>,
    #[serde(default)]
    input_cache_write: Option<String>,
}

#[derive(Deserialize)]
struct Endpoint {
    provider_name: String,
    pricing: EndpointPricing,
}

#[derive(Deserialize)]
struct EndpointData {
    #[allow(dead_code)]
    id: String,
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize)]
struct EndpointsResponse {
    data: EndpointData,
}

/// Model ID prefix to provider name mapping.
///
/// Translates model ID prefixes like `z-ai` to their corresponding
/// provider names in the endpoints API, such as `Z.AI`.
fn get_author_provider_name(model_id: &str) -> Option<&'static str> {
    let prefix = model_id.split('/').next()?;

    match prefix.to_lowercase().as_str() {
        "z-ai" => Some("Z.AI"),
        "x-ai" => Some("xAI"),
        "anthropic" => Some("Anthropic"),
        "openai" => Some("OpenAI"),
        "google" => Some("Google"),
        "meta-llama" => Some("Meta"),
        "mistralai" => Some("Mistral"),
        "deepseek" => Some("DeepSeek"),
        "qwen" => Some("Alibaba"),
        "cohere" => Some("Cohere"),
        "perplexity" => Some("Perplexity"),
        "moonshotai" => Some("Moonshot AI"),
        "xiaomi" => Some("Xiaomi"),
        _ => None,
    }
}

pub fn load_cached() -> Option<HashMap<String, ModelPricing>> {
    cache::load_cache(CACHE_FILENAME)
}

pub fn load_cached_any_age() -> Option<HashMap<String, ModelPricing>> {
    cache::load_cache_any_age(CACHE_FILENAME)
}

fn parse_price(s: &str) -> Option<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn parse_model_list_pricing(pricing: ModelListPricing) -> Option<ModelPricing> {
    let input = parse_price(&pricing.prompt)?;
    let output = parse_price(&pricing.completion)?;
    Some(ModelPricing {
        input_cost_per_token: Some(input),
        output_cost_per_token: Some(output),
        cache_read_input_token_cost: None,
        cache_creation_input_token_cost: None,
        ..Default::default()
    })
}

async fn fetch_author_pricing(
    client: Arc<reqwest::Client>,
    model_id: String,
    semaphore: Arc<Semaphore>,
    author_name: &'static str,
) -> Result<(String, Option<ModelPricing>), String> {
    let _permit = semaphore
        .acquire()
        .await
        .expect("OpenRouter pricing semaphore should not be closed");

    let url = format!("https://openrouter.ai/api/v1/models/{}/endpoints", model_id);

    let response = client
        .get(&url)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|err| format!("{model_id}: endpoints request failed: {err}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("{model_id}: endpoints API returned {status}"));
    }

    let data: EndpointsResponse = response
        .json()
        .await
        .map_err(|err| format!("{model_id}: endpoints JSON parse failed: {err}"))?;

    // Find the endpoint from the author provider
    let author_endpoint = match data
        .data
        .endpoints
        .iter()
        .find(|e| e.provider_name == author_name)
    {
        Some(ep) => ep,
        None => return Ok((model_id, None)),
    };

    let input_cost = parse_price(&author_endpoint.pricing.prompt)
        .ok_or_else(|| format!("{model_id}: author endpoint has invalid prompt price"))?;
    let output_cost = parse_price(&author_endpoint.pricing.completion)
        .ok_or_else(|| format!("{model_id}: author endpoint has invalid completion price"))?;

    let pricing = ModelPricing {
        input_cost_per_token: Some(input_cost),
        output_cost_per_token: Some(output_cost),
        cache_read_input_token_cost: author_endpoint
            .pricing
            .input_cache_read
            .as_ref()
            .and_then(|s| parse_price(s)),
        cache_creation_input_token_cost: author_endpoint
            .pricing
            .input_cache_write
            .as_ref()
            .and_then(|s| parse_price(s)),
        ..Default::default()
    };

    Ok((model_id, Some(pricing)))
}

fn select_models_for_author_pricing(model_ids: Vec<String>) -> Vec<(String, &'static str)> {
    model_ids
        .into_iter()
        .filter_map(|id| get_author_provider_name(&id).map(|author| (id, author)))
        .collect()
}

/// Fetch all models and get author pricing for each
pub async fn fetch_all_models() -> HashMap<String, ModelPricing> {
    let mut diagnostics = None;
    fetch_all_models_with_sink(&mut diagnostics).await
}

pub(crate) async fn fetch_all_models_with_diagnostics(
    diagnostics: &mut PricingDiagnostics,
) -> HashMap<String, ModelPricing> {
    let mut diagnostics = Some(diagnostics);
    fetch_all_models_with_sink(&mut diagnostics).await
}

async fn fetch_all_models_with_sink(
    diagnostics: &mut PricingDiagnosticSink<'_>,
) -> HashMap<String, ModelPricing> {
    if let Some(cached) = load_cached() {
        return cached;
    }

    let client = Arc::new(
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("valid OpenRouter HTTP client configuration"),
    );

    let mut last_error: Option<String> = None;

    let model_items: Vec<ModelListItem> = 'retry: {
        for attempt in 0..MAX_RETRIES {
            let response = match client
                .get(MODELS_URL)
                .header("Content-Type", "application/json")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = Some(format!("network error: {}", e));
                    if attempt < MAX_RETRIES - 1 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            INITIAL_BACKOFF_MS * (1 << attempt),
                        ))
                        .await;
                    }
                    continue;
                }
            };

            let status = response.status();
            if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                last_error = Some(format!("HTTP {}", status));
                let _ = response.bytes().await;
                if attempt < MAX_RETRIES - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        INITIAL_BACKOFF_MS * (1 << attempt),
                    ))
                    .await;
                }
                continue;
            }

            if !status.is_success() {
                emit_diagnostic(
                    diagnostics,
                    format!("[tokscale] OpenRouter models API returned {status}"),
                );
                break 'retry Vec::new();
            }

            let data: ModelsListResponse = match response.json().await {
                Ok(d) => d,
                Err(e) => {
                    emit_diagnostic(
                        diagnostics,
                        format!("[tokscale] OpenRouter models JSON parse failed: {e}"),
                    );
                    break 'retry Vec::new();
                }
            };

            break 'retry data.data;
        }

        if let Some(err) = &last_error {
            emit_diagnostic(
                diagnostics,
                format!(
                    "[tokscale] OpenRouter fetch failed after {} retries: {}",
                    MAX_RETRIES, err
                ),
            );
        }
        Vec::new()
    };

    if model_items.is_empty() {
        return HashMap::new();
    }

    let mut result = HashMap::new();
    let mut model_ids = Vec::with_capacity(model_items.len());
    for model in model_items {
        if let Some(pricing) = model.pricing.and_then(parse_model_list_pricing) {
            result.insert(model.id.clone(), pricing);
        }
        model_ids.push(model.id);
    }

    let models_for_author_pricing = select_models_for_author_pricing(model_ids);

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    let mut handles = Vec::with_capacity(models_for_author_pricing.len());

    for (model_id, author_name) in models_for_author_pricing {
        let client = Arc::clone(&client);
        let sem = Arc::clone(&semaphore);

        let handle =
            tokio::spawn(
                async move { fetch_author_pricing(client, model_id, sem, author_name).await },
            );

        handles.push(handle);
    }

    for handle in handles {
        match handle.await {
            Ok(Ok((model_id, Some(pricing)))) => {
                result.insert(model_id, pricing);
            }
            Ok(Ok((_model_id, None))) => {}
            Ok(Err(err)) => emit_diagnostic(
                diagnostics,
                format!("[tokscale] OpenRouter author pricing skipped: {err}"),
            ),
            Err(err) => emit_diagnostic(
                diagnostics,
                format!("[tokscale] OpenRouter author pricing task failed: {err}"),
            ),
        }
    }

    if !result.is_empty() {
        if let Err(e) = cache::save_cache(CACHE_FILENAME, &result) {
            emit_diagnostic(
                diagnostics,
                format!(
                    "[tokscale] Warning: Failed to cache OpenRouter pricing at {}: {}",
                    cache::get_cache_path(CACHE_FILENAME).display(),
                    e
                ),
            );
        }
    }

    result
}

pub async fn fetch_all_mapped() -> HashMap<String, ModelPricing> {
    fetch_all_models().await
}

pub(crate) async fn fetch_all_mapped_with_diagnostics(
    diagnostics: &mut PricingDiagnostics,
) -> HashMap<String, ModelPricing> {
    fetch_all_models_with_diagnostics(diagnostics).await
}

#[cfg(test)]
mod tests {
    use super::{
        get_author_provider_name, parse_model_list_pricing, select_models_for_author_pricing,
        ModelListPricing,
    };

    #[test]
    fn maps_xiaomi_models_to_openrouter_author_provider() {
        assert_eq!(
            get_author_provider_name("xiaomi/mimo-v2.5-pro"),
            Some("Xiaomi")
        );
    }

    #[test]
    fn selects_only_models_with_author_provider_for_endpoint_enrichment() {
        let selected = select_models_for_author_pricing(vec![
            "relace/relace-apply-3".to_string(),
            "unknown/no-price".to_string(),
            "openai/gpt-5".to_string(),
        ]);

        let selected_ids: Vec<&str> = selected.iter().map(|(id, _)| id.as_str()).collect();

        assert!(selected_ids.contains(&"openai/gpt-5"));
        assert!(!selected_ids.contains(&"relace/relace-apply-3"));
        assert!(!selected_ids.contains(&"unknown/no-price"));
    }

    #[test]
    fn parses_model_list_pricing_as_baseline_pricing() {
        let pricing = parse_model_list_pricing(ModelListPricing {
            prompt: "0.00000085".to_string(),
            completion: "0.00000125".to_string(),
        })
        .expect("valid model list pricing");

        assert_eq!(pricing.input_cost_per_token, Some(0.00000085));
        assert_eq!(pricing.output_cost_per_token, Some(0.00000125));
    }
}
