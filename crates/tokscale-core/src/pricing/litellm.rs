use super::{cache, emit_diagnostic, PricingDiagnosticSink, PricingDiagnostics};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const CACHE_FILENAME: &str = "pricing-litellm.json";
const PRICING_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 200;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPricing {
    pub input_cost_per_token: Option<f64>,
    pub input_cost_per_token_above_128k_tokens: Option<f64>,
    pub input_cost_per_token_above_200k_tokens: Option<f64>,
    pub input_cost_per_token_above_256k_tokens: Option<f64>,
    pub input_cost_per_token_above_272k_tokens: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    pub output_cost_per_token_above_128k_tokens: Option<f64>,
    pub output_cost_per_token_above_200k_tokens: Option<f64>,
    pub output_cost_per_token_above_256k_tokens: Option<f64>,
    pub output_cost_per_token_above_272k_tokens: Option<f64>,
    pub cache_creation_input_token_cost: Option<f64>,
    pub cache_creation_input_token_cost_above_200k_tokens: Option<f64>,
    pub cache_read_input_token_cost: Option<f64>,
    pub cache_read_input_token_cost_above_200k_tokens: Option<f64>,
    pub cache_read_input_token_cost_above_272k_tokens: Option<f64>,
}

pub type PricingDataset = HashMap<String, ModelPricing>;

pub fn load_cached() -> Option<PricingDataset> {
    cache::load_cache(CACHE_FILENAME)
}

pub fn load_cached_any_age() -> Option<PricingDataset> {
    cache::load_cache_any_age(CACHE_FILENAME)
}

pub async fn fetch() -> Result<PricingDataset, reqwest::Error> {
    let mut diagnostics = None;
    fetch_inner(PRICING_URL, true, &mut diagnostics).await
}

pub(crate) async fn fetch_with_diagnostics(
    diagnostics: &mut PricingDiagnostics,
) -> Result<PricingDataset, reqwest::Error> {
    let mut diagnostics = Some(diagnostics);
    fetch_inner(PRICING_URL, true, &mut diagnostics).await
}

async fn fetch_inner(
    url: &str,
    use_cache: bool,
    diagnostics: &mut PricingDiagnosticSink<'_>,
) -> Result<PricingDataset, reqwest::Error> {
    if use_cache {
        if let Some(cached) = load_cached() {
            return Ok(cached);
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut last_error: Option<reqwest::Error> = None;

    for attempt in 0..MAX_RETRIES {
        match client.get(url).send().await {
            Ok(response) => {
                let status = response.status();

                if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    if let Err(error) = response.error_for_status_ref() {
                        last_error = Some(error);
                    }
                    emit_diagnostic(
                        diagnostics,
                        format!(
                            "[tokscale] LiteLLM HTTP {} (attempt {}/{})",
                            status,
                            attempt + 1,
                            MAX_RETRIES
                        ),
                    );
                    if attempt == MAX_RETRIES - 1 {
                        return Err(response.error_for_status().unwrap_err());
                    }
                    let _ = response.bytes().await;
                    tokio::time::sleep(std::time::Duration::from_millis(
                        INITIAL_BACKOFF_MS * (1 << attempt),
                    ))
                    .await;
                    continue;
                }

                if !status.is_success() {
                    emit_diagnostic(diagnostics, format!("[tokscale] LiteLLM HTTP {status}"));
                    return Err(response.error_for_status().unwrap_err());
                }

                match response.json::<PricingDataset>().await {
                    Ok(data) => {
                        if let Err(e) = cache::save_cache(CACHE_FILENAME, &data) {
                            emit_diagnostic(
                                diagnostics,
                                format!(
                                    "[tokscale] Warning: Failed to cache LiteLLM pricing at {}: {}",
                                    cache::get_cache_path(CACHE_FILENAME).display(),
                                    e
                                ),
                            );
                        }
                        return Ok(data);
                    }
                    Err(e) => {
                        emit_diagnostic(
                            diagnostics,
                            format!("[tokscale] LiteLLM JSON parse failed: {e}"),
                        );
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                emit_diagnostic(
                    diagnostics,
                    format!(
                        "[tokscale] LiteLLM network error (attempt {}/{}): {}",
                        attempt + 1,
                        MAX_RETRIES,
                        e
                    ),
                );
                last_error = Some(e);
                if attempt < MAX_RETRIES - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        INITIAL_BACKOFF_MS * (1 << attempt),
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_error.expect("should have error after retries"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn retryable_status_server(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());

        thread::spawn(move || {
            for _ in 0..MAX_RETRIES {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0; 1024];
                let _ = stream.read(&mut buffer);
                let response =
                    format!("{status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(response.as_bytes());
            }
        });

        url
    }

    #[tokio::test]
    async fn fetch_returns_error_after_retryable_http_statuses() {
        let url = retryable_status_server("HTTP/1.1 503 Service Unavailable");
        let mut diagnostics = None;

        let result = fetch_inner(&url, false, &mut diagnostics).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_with_diagnostics_collects_retryable_http_statuses() {
        let url = retryable_status_server("HTTP/1.1 429 Too Many Requests");
        let mut diagnostics = Vec::new();
        let mut sink = Some(&mut diagnostics);

        let result = fetch_inner(&url, false, &mut sink).await;

        assert!(result.is_err());
        assert!(
            diagnostics
                .iter()
                .any(|line| line.contains("LiteLLM HTTP 429")),
            "diagnostics missing retryable status: {diagnostics:?}"
        );
    }

    #[test]
    fn test_deserialize_model_pricing_with_above_200k_fields() {
        let pricing: ModelPricing = serde_json::from_str(
            r#"{
                "input_cost_per_token": 0.0000015,
                "input_cost_per_token_above_200k_tokens": 0.000003,
                "output_cost_per_token": 0.0000075,
                "output_cost_per_token_above_200k_tokens": 0.000015,
                "cache_creation_input_token_cost": 0.000001875,
                "cache_creation_input_token_cost_above_200k_tokens": 0.00000375,
                "cache_read_input_token_cost": 0.00000015,
                "cache_read_input_token_cost_above_200k_tokens": 0.0000003
            }"#,
        )
        .unwrap();

        assert_eq!(pricing.input_cost_per_token, Some(0.0000015));
        assert_eq!(
            pricing.input_cost_per_token_above_200k_tokens,
            Some(0.000003)
        );
        assert_eq!(pricing.output_cost_per_token, Some(0.0000075));
        assert_eq!(
            pricing.output_cost_per_token_above_200k_tokens,
            Some(0.000015)
        );
        assert_eq!(pricing.cache_creation_input_token_cost, Some(0.000001875));
        assert_eq!(
            pricing.cache_creation_input_token_cost_above_200k_tokens,
            Some(0.00000375)
        );
        assert_eq!(pricing.cache_read_input_token_cost, Some(0.00000015));
        assert_eq!(
            pricing.cache_read_input_token_cost_above_200k_tokens,
            Some(0.0000003)
        );
    }

    #[test]
    fn test_deserialize_model_pricing_without_above_200k_fields() {
        let pricing: ModelPricing = serde_json::from_str(
            r#"{
                "input_cost_per_token": 0.00000125,
                "output_cost_per_token": 0.00001,
                "cache_creation_input_token_cost": 0.00000125,
                "cache_read_input_token_cost": 0.000000125
            }"#,
        )
        .unwrap();

        assert_eq!(pricing.input_cost_per_token, Some(0.00000125));
        assert_eq!(pricing.input_cost_per_token_above_200k_tokens, None);
        assert_eq!(pricing.output_cost_per_token, Some(0.00001));
        assert_eq!(pricing.output_cost_per_token_above_200k_tokens, None);
        assert_eq!(pricing.cache_creation_input_token_cost, Some(0.00000125));
        assert_eq!(
            pricing.cache_creation_input_token_cost_above_200k_tokens,
            None
        );
        assert_eq!(pricing.cache_read_input_token_cost, Some(0.000000125));
        assert_eq!(pricing.cache_read_input_token_cost_above_200k_tokens, None);
    }

    #[test]
    fn test_deserialize_model_pricing_with_above_272k_fields() {
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

        assert_eq!(pricing.input_cost_per_token, Some(0.000005));
        assert_eq!(
            pricing.input_cost_per_token_above_272k_tokens,
            Some(0.000010)
        );
        assert_eq!(pricing.output_cost_per_token, Some(0.000030));
        assert_eq!(
            pricing.output_cost_per_token_above_272k_tokens,
            Some(0.000045)
        );
        assert_eq!(pricing.cache_read_input_token_cost, Some(0.0000005));
        assert_eq!(
            pricing.cache_read_input_token_cost_above_272k_tokens,
            Some(0.000001)
        );
    }
}
