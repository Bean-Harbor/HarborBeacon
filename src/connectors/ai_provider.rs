//! Normalized AI provider interface boundary.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderType {
    LocalSidecar,
    OpenAiCompatible,
    RemoteCloud,
    HarborOsService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl OpenAiCompatibleConfig {
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("HARBOR_OPENAI_BASE_URL").ok()?;
        let api_key = std::env::var("HARBOR_OPENAI_API_KEY").ok()?;
        let model = std::env::var("HARBOR_OPENAI_MODEL").ok()?;

        Some(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionSummaryRequest {
    pub image_data_url: String,
    pub detection_summary: String,
    pub user_prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCompletionRequest {
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<std::time::Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRequest {
    pub input: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisionSummaryResponse {
    pub summary: String,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCompletionResponse {
    pub text: String,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingResponse {
    pub embedding: Vec<f32>,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankCompatibleConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub rerank_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankRequest {
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RerankScore {
    pub index: usize,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RerankResponse {
    pub scores: Vec<RerankScore>,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionSidecarConfig {
    pub base_url: String,
}

impl VisionSidecarConfig {
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("HARBOR_VISION_SIDECAR_URL").ok()?;
        Some(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionDetectionRequest {
    pub image_path: String,
    pub label: String,
    pub min_confidence: f32,
    pub annotated_output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionDetectionResponse {
    #[serde(default)]
    pub detections: Vec<serde_json::Value>,
    #[serde(default)]
    pub annotated_image_path: Option<String>,
}

pub struct VisionSidecarClient {
    client: Client,
    config: VisionSidecarConfig,
}

impl VisionSidecarClient {
    pub fn new(config: VisionSidecarConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("failed to build vision sidecar client: {e}"))?;
        Ok(Self { client, config })
    }

    pub fn healthz(&self) -> Result<(), String> {
        let response = self
            .client
            .get(format!("{}/healthz", self.config.base_url))
            .send()
            .map_err(|e| format!("vision sidecar health check failed: {e}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "vision sidecar health check returned {}",
                response.status()
            ))
        }
    }

    pub fn detect(
        &self,
        request: &VisionDetectionRequest,
    ) -> Result<VisionDetectionResponse, String> {
        let response = self
            .client
            .post(format!("{}/analyze", self.config.base_url))
            .json(request)
            .send()
            .map_err(|e| format!("vision sidecar request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("vision sidecar error {status}: {body}"));
        }

        response
            .json()
            .map_err(|e| format!("failed to parse vision sidecar response: {e}"))
    }
}

pub struct OpenAiCompatibleVisionClient {
    client: Client,
    config: OpenAiCompatibleConfig,
}

pub struct OpenAiCompatibleTextClient {
    client: Client,
    config: OpenAiCompatibleConfig,
}

pub struct OpenAiCompatibleEmbeddingClient {
    client: Client,
    config: OpenAiCompatibleConfig,
}

pub struct RerankCompatibleClient {
    client: Client,
    config: RerankCompatibleConfig,
}

impl OpenAiCompatibleVisionClient {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| format!("failed to build OpenAI-compatible client: {e}"))?;
        Ok(Self { client, config })
    }

    pub fn describe_frame(
        &self,
        request: &VisionSummaryRequest,
    ) -> Result<VisionSummaryResponse, String> {
        let system_prompt = "You are a concise Chinese security-camera analyst. Summarize what matters for a HarborBeacon user. Mention detected people count, approximate position, and whether the frame needs attention. Keep it under 80 Chinese characters.";
        let user_prompt = request.user_prompt.clone().unwrap_or_else(|| {
            "请根据检测结果和图片，用中文总结当前画面。优先说明是否有人、人数、位置和是否需要关注。".to_string()
        });

        let payload = json!({
            "model": self.config.model,
            "temperature": 0.2,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": format!("{user_prompt}\n\n检测结果:\n{}", request.detection_summary)
                        },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": request.image_data_url
                            }
                        }
                    ]
                }
            ]
        });

        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .headers(self.headers()?)
            .json(&payload)
            .send()
            .map_err(|e| format!("OpenAI-compatible request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("OpenAI-compatible API error {status}: {body}"));
        }

        let raw_response: serde_json::Value = response
            .json()
            .map_err(|e| format!("failed to parse OpenAI-compatible response: {e}"))?;
        let summary = extract_message_text(&raw_response).ok_or_else(|| {
            "OpenAI-compatible response did not contain assistant text".to_string()
        })?;

        Ok(VisionSummaryResponse {
            summary,
            raw_response,
        })
    }

    fn headers(&self) -> Result<HeaderMap, String> {
        openai_compatible_headers(&self.config.api_key)
    }
}

impl OpenAiCompatibleTextClient {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| format!("failed to build OpenAI-compatible text client: {e}"))?;
        Ok(Self { client, config })
    }

    pub fn complete_text(
        &self,
        request: &TextCompletionRequest,
    ) -> Result<TextCompletionResponse, String> {
        let mut messages = Vec::new();
        if let Some(system_prompt) = request
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            messages.push(json!({
                "role": "system",
                "content": system_prompt,
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": request.user_prompt,
        }));

        let mut payload = serde_json::Map::new();
        payload.insert("model".to_string(), json!(self.config.model));
        payload.insert(
            "temperature".to_string(),
            json!(request.temperature.unwrap_or(0.1)),
        );
        payload.insert("messages".to_string(), json!(messages));
        if let Some(max_tokens) = request.max_tokens {
            payload.insert("max_tokens".to_string(), json!(max_tokens));
        }

        let mut request_builder = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .headers(openai_compatible_headers(&self.config.api_key)?)
            .json(&payload);
        if let Some(timeout) = request.timeout {
            request_builder = request_builder.timeout(timeout);
        }

        let response = request_builder
            .send()
            .map_err(|e| format!("OpenAI-compatible text request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("OpenAI-compatible API error {status}: {body}"));
        }

        let raw_response: serde_json::Value = response
            .json()
            .map_err(|e| format!("failed to parse OpenAI-compatible response: {e}"))?;
        let text = extract_message_text(&raw_response).ok_or_else(|| {
            "OpenAI-compatible response did not contain assistant text".to_string()
        })?;

        Ok(TextCompletionResponse { text, raw_response })
    }
}

impl OpenAiCompatibleEmbeddingClient {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| format!("failed to build OpenAI-compatible embedding client: {e}"))?;
        Ok(Self { client, config })
    }

    pub fn embed_text(&self, request: &EmbeddingRequest) -> Result<EmbeddingResponse, String> {
        let payload = json!({
            "model": self.config.model,
            "input": request.input,
        });

        let response = self
            .client
            .post(format!("{}/embeddings", self.config.base_url))
            .headers(openai_compatible_headers(&self.config.api_key)?)
            .json(&payload)
            .send()
            .map_err(|e| format!("OpenAI-compatible embedding request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("OpenAI-compatible API error {status}: {body}"));
        }

        let raw_response: serde_json::Value = response
            .json()
            .map_err(|e| format!("failed to parse OpenAI-compatible embedding response: {e}"))?;
        let embedding = extract_embedding_vector(&raw_response).ok_or_else(|| {
            "OpenAI-compatible response did not contain an embedding vector".to_string()
        })?;

        Ok(EmbeddingResponse {
            embedding,
            raw_response,
        })
    }
}

impl RerankCompatibleClient {
    pub fn new(config: RerankCompatibleConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build rerank-compatible client: {e}"))?;
        Ok(Self { client, config })
    }

    pub fn rerank(&self, request: &RerankRequest) -> Result<RerankResponse, String> {
        if request.query.trim().is_empty() {
            return Err("rerank query is empty".to_string());
        }
        if request.documents.is_empty() {
            return Err("rerank documents are empty".to_string());
        }
        let payload = json!({
            "model": self.config.model,
            "query": request.query,
            "documents": request.documents,
            "top_n": request.top_n.max(1),
        });

        let response = self
            .client
            .post(rerank_url(&self.config.base_url, &self.config.rerank_path))
            .headers(openai_compatible_headers(&self.config.api_key)?)
            .json(&payload)
            .send()
            .map_err(|e| format!("rerank-compatible request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("rerank-compatible API error {status}: {body}"));
        }

        let raw_response: Value = response
            .json()
            .map_err(|e| format!("failed to parse rerank-compatible response: {e}"))?;
        let scores = extract_rerank_scores(&raw_response);
        if scores.is_empty() {
            return Err("rerank-compatible response did not contain scores".to_string());
        }
        Ok(RerankResponse {
            scores,
            raw_response,
        })
    }
}

fn openai_compatible_headers(api_key: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    if !api_key.trim().is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| format!("invalid OpenAI-compatible auth header: {e}"))?,
        );
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

fn extract_message_text(value: &serde_json::Value) -> Option<String> {
    let message_content = value
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;

    if let Some(text) = message_content.as_str() {
        return Some(text.trim().to_string());
    }

    let parts = message_content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_string())
    }
}

fn extract_embedding_vector(value: &serde_json::Value) -> Option<Vec<f32>> {
    let values = value
        .get("data")?
        .as_array()?
        .first()?
        .get("embedding")?
        .as_array()?;
    let mut embedding = Vec::with_capacity(values.len());
    for item in values {
        embedding.push(item.as_f64()? as f32);
    }
    (!embedding.is_empty()).then_some(embedding)
}

fn rerank_url(base_url: &str, rerank_path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let path = rerank_path.trim();
    if path.is_empty() {
        format!("{base}/rerank")
    } else if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

fn extract_rerank_scores(value: &Value) -> Vec<RerankScore> {
    let Some(results) = value.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut scores = results
        .iter()
        .filter_map(|item| {
            let index = item.get("index")?.as_u64()? as usize;
            let score = item
                .get("relevance_score")
                .or_else(|| item.get("score"))?
                .as_f64()? as f32;
            score.is_finite().then_some(RerankScore { index, score })
        })
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.index.cmp(&right.index))
    });
    scores
}

#[cfg(test)]
mod tests {
    use super::{extract_embedding_vector, extract_message_text, extract_rerank_scores};
    use serde_json::json;

    #[test]
    fn extract_message_text_supports_string_content() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": "画面中有 1 人"
                }
            }]
        });

        assert_eq!(
            extract_message_text(&response).as_deref(),
            Some("画面中有 1 人")
        );
    }

    #[test]
    fn extract_message_text_supports_array_content() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "画面中有 2 人"},
                        {"type": "text", "text": "其中一人位于左侧"}
                    ]
                }
            }]
        });

        assert_eq!(
            extract_message_text(&response).as_deref(),
            Some("画面中有 2 人\n其中一人位于左侧")
        );
    }

    #[test]
    fn extract_embedding_vector_supports_openai_shape() {
        let response = json!({
            "data": [{
                "embedding": [0.25, -0.5, 0.75],
                "index": 0,
            }]
        });

        assert_eq!(
            extract_embedding_vector(&response),
            Some(vec![0.25f32, -0.5f32, 0.75f32])
        );
    }

    #[test]
    fn extract_rerank_scores_supports_relevance_score() {
        let response = json!({
            "results": [
                {"index": 1, "relevance_score": 0.82},
                {"index": 0, "relevance_score": 0.21}
            ]
        });

        let scores = extract_rerank_scores(&response);
        assert_eq!(scores[0].index, 1);
        assert!((scores[0].score - 0.82).abs() < f32::EPSILON);
    }

    #[test]
    fn extract_rerank_scores_supports_score() {
        let response = json!({
            "results": [
                {"index": 0, "score": 0.4},
                {"index": 2, "score": 0.7}
            ]
        });

        let scores = extract_rerank_scores(&response);
        assert_eq!(scores[0].index, 2);
        assert!((scores[0].score - 0.7).abs() < f32::EPSILON);
    }
}
