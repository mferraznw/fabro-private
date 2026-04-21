use std::sync::Arc;
use std::time::{Duration, Instant};

use fabro_http::header::{ETAG, IF_NONE_MATCH};
use fabro_model::{
    DiscoveryFuture, Model, ModelCosts, ModelDiscovery, ModelFeatures, ModelLimits, ModelMeta,
    Provider,
};
use serde::Deserialize;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct LiteLlmModelsCache {
    base_url: String,
    api_key: Option<String>,
    http: fabro_http::HttpClient,
    ttl: Duration,
    state: Arc<Mutex<CacheState>>,
}

#[derive(Default)]
struct CacheState {
    fetched_at: Option<Instant>,
    etag: Option<String>,
    models: Vec<String>,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

impl LiteLlmModelsCache {
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: Option<String>, ttl: Duration) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            http: fabro_http::http_client().expect("HTTP client should build"),
            ttl,
            state: Arc::new(Mutex::new(CacheState::default())),
        }
    }

    #[must_use]
    pub fn with_http(mut self, http: fabro_http::HttpClient) -> Self {
        self.http = http;
        self
    }

    pub async fn models(&self) -> Result<Vec<ModelMeta>, String> {
        self.model_ids()
            .await
            .map(|ids| ids.into_iter().map(litellm_model_stub).collect())
    }

    pub async fn model_ids(&self) -> Result<Vec<String>, String> {
        let cached = {
            let state = self.state.lock().await;
            state
                .fetched_at
                .filter(|fetched| fetched.elapsed() < self.ttl)
                .map(|_| state.models.clone())
        };
        if let Some(models) = cached {
            return Ok(models);
        }

        let (etag, previous) = {
            let state = self.state.lock().await;
            (state.etag.clone(), state.models.clone())
        };

        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut request = self.http.get(url);
        if let Some(api_key) = self.api_key.as_deref().filter(|key| !key.is_empty()) {
            request = request.bearer_auth(api_key);
        }
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }

        let response = request.send().await.map_err(|err| err.to_string())?;
        if response.status() == fabro_http::StatusCode::NOT_MODIFIED {
            let mut state = self.state.lock().await;
            state.fetched_at = Some(Instant::now());
            return Ok(state.models.clone());
        }
        let response = response.error_for_status().map_err(|err| err.to_string())?;
        let new_etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let parsed = response
            .json::<OpenAiModelsResponse>()
            .await
            .map_err(|err| err.to_string())?;
        let models = parsed
            .data
            .into_iter()
            .map(|entry| entry.id)
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();

        if models.is_empty() && !previous.is_empty() {
            return Ok(previous);
        }

        let mut state = self.state.lock().await;
        state.fetched_at = Some(Instant::now());
        state.etag = new_etag;
        state.models.clone_from(&models);
        Ok(models)
    }
}

#[derive(Clone)]
pub struct LiteLlmDiscovery {
    cache: LiteLlmModelsCache,
}

impl LiteLlmDiscovery {
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: Option<String>, ttl: Duration) -> Self {
        Self {
            cache: LiteLlmModelsCache::new(base_url, api_key, ttl),
        }
    }

    #[must_use]
    pub fn with_cache(cache: LiteLlmModelsCache) -> Self {
        Self { cache }
    }

    pub async fn discover_model(&self, id: &str) -> Result<Option<ModelMeta>, String> {
        let models = self.cache.model_ids().await?;
        if !models.iter().any(|model| model == id) {
            return Ok(None);
        }
        let info = self.fetch_model_info(id).await?;
        Ok(Some(model_from_info(id, &info)))
    }

    async fn fetch_model_info(&self, id: &str) -> Result<serde_json::Value, String> {
        let base = self.cache.base_url.trim_end_matches('/');
        let info_base = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{info_base}/model/info");
        let mut request = self.cache.http.get(url).query(&[("model", id)]);
        if let Some(api_key) = self.cache.api_key.as_deref().filter(|key| !key.is_empty()) {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .send()
            .await
            .map_err(|err| err.to_string())?
            .error_for_status()
            .map_err(|err| err.to_string())?;
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|err| err.to_string())
    }
}

impl ModelDiscovery for LiteLlmDiscovery {
    fn discover<'a>(&'a self, id: &'a str) -> DiscoveryFuture<'a> {
        Box::pin(async move { self.discover_model(id).await })
    }
}

#[must_use]
pub fn litellm_model_stub(id: String) -> ModelMeta {
    let family = id
        .split(['/', ':'])
        .next_back()
        .and_then(|name| name.split('-').next())
        .filter(|value| !value.is_empty())
        .unwrap_or("litellm")
        .to_string();
    Model {
        display_name: id.clone(),
        id,
        provider: Provider::OpenAiCompatible,
        family,
        limits: ModelLimits {
            context_window: 128_000,
            max_output: Some(16_384),
        },
        training: None,
        knowledge_cutoff: None,
        features: ModelFeatures {
            tools: true,
            vision: true,
            reasoning: false,
            effort: false,
        },
        costs: ModelCosts {
            input_cost_per_mtok: None,
            output_cost_per_mtok: None,
            cache_input_cost_per_mtok: None,
        },
        estimated_output_tps: None,
        aliases: Vec::new(),
        default: false,
    }
}

fn model_from_info(id: &str, info: &serde_json::Value) -> ModelMeta {
    let mut model = litellm_model_stub(id.to_string());
    let model_info = info
        .get("model_info")
        .and_then(|value| value.get(0).or(Some(value)));
    let object = model_info
        .or_else(|| info.get("data").and_then(|value| value.get(0)))
        .unwrap_or(info);
    model.limits.context_window = read_i64(
        object,
        &["context_window", "max_input_tokens", "max_context_tokens"],
    )
    .unwrap_or(model.limits.context_window);
    model.limits.max_output =
        read_i64(object, &["max_output_tokens", "max_tokens"]).or(model.limits.max_output);
    model.features.tools = read_bool(
        object,
        &[
            "supports_function_calling",
            "supports_tools",
            "function_calling",
        ],
    )
    .unwrap_or(model.features.tools);
    model.features.vision =
        read_bool(object, &["supports_vision", "vision"]).unwrap_or(model.features.vision);
    model.costs.input_cost_per_mtok =
        read_f64(object, &["input_cost_per_1m_tokens", "input_cost_per_1m"])
            .or_else(|| read_f64(object, &["input_cost_per_token"]).map(|cost| cost * 1_000_000.0));
    model.costs.output_cost_per_mtok =
        read_f64(object, &["output_cost_per_1m_tokens", "output_cost_per_1m"]).or_else(|| {
            read_f64(object, &["output_cost_per_token"]).map(|cost| cost * 1_000_000.0)
        });
    model
}

fn read_i64(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|value| value.as_i64().or_else(|| value.as_u64()?.try_into().ok()))
    })
}

fn read_f64(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_f64))
}

fn read_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_bool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_from_info_maps_capabilities_and_costs() {
        let model = model_from_info(
            "anthropic/claude",
            &serde_json::json!({
                "model_info": {
                    "context_window": 200_000,
                    "max_output_tokens": 8192,
                    "supports_function_calling": false,
                    "supports_vision": true,
                    "input_cost_per_token": 0.000_003,
                    "output_cost_per_token": 0.000_015
                }
            }),
        );

        assert_eq!(model.limits.context_window, 200_000);
        assert_eq!(model.limits.max_output, Some(8192));
        assert!(!model.features.tools);
        assert!(model.features.vision);
        assert_eq!(model.costs.input_cost_per_mtok, Some(3.0));
        assert_eq!(model.costs.output_cost_per_mtok, Some(15.0));
    }
}
