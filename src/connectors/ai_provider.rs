//! Normalized AI provider interface boundary.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const VISION_SUMMARY_MAX_TOKENS: u32 = 160;

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
    /// Optional role instruction. When unset, preserve the existing camera-analysis behavior.
    pub system_prompt: Option<String>,
    /// Qwen3.5 VLM can spend its limited output budget on hidden reasoning. Enable this only
    /// for runtimes that explicitly support OpenAI-compatible chat-template options.
    pub disable_thinking: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCompletionRequest {
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<std::time::Duration>,
    pub disable_thinking: bool,
    pub json_object_response: bool,
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
pub struct CatFrameVerificationResponse {
    pub cat_present: bool,
    pub behavior_tags: Vec<String>,
    pub summary: String,
    pub reason_code: String,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatSceneDescriptionResponse {
    pub summary: String,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CatFrameVerificationPayload {
    #[serde(default)]
    behavior_tags: Vec<String>,
    summary: String,
    reason_code: String,
}

#[derive(Debug, Deserialize)]
struct CatSceneDescriptionPayload {
    summary: String,
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
        let system_prompt = request.system_prompt.clone().unwrap_or_else(|| {
            "You are a concise Chinese security-camera analyst. Summarize what matters for a HarborBeacon user. Mention detected people count, approximate position, and whether the frame needs attention. Keep it under 80 Chinese characters.".to_string()
        });
        let user_prompt = request.user_prompt.clone().unwrap_or_else(|| {
            "请根据检测结果和图片，用中文总结当前画面。优先说明是否有人、人数、位置和是否需要关注。".to_string()
        });

        let (summary, raw_response) = self.complete_vision(
            &system_prompt,
            &format!("{user_prompt}\n\n检测结果:\n{}", request.detection_summary),
            &request.image_data_url,
            0.2,
            Some(VISION_SUMMARY_MAX_TOKENS),
            None,
            request.disable_thinking,
        )?;

        Ok(VisionSummaryResponse {
            summary,
            raw_response,
        })
    }

    pub fn verify_cat_frame(
        &self,
        image_data_url: &str,
    ) -> Result<CatFrameVerificationResponse, String> {
        let system_prompt = "You are a visual cat-presence classifier. Judge image pixels only and select exactly one reason_code. Use cat_visible when any domestic cat is visibly present, including a partial cat. Use no_cat_visible when no cat is visible. Use uncertain only when the image cannot be interpreted. Write summary in concise Chinese.";
        let user_prompt = "请只看图像选择 reason_code：画面里看见任何完整或局部的猫就选 cat_visible。只有能明确判断的行为才加入 behavior_tags，否则使用 unknown。";
        let (text, raw_response) = self.complete_vision(
            system_prompt,
            user_prompt,
            image_data_url,
            0.0,
            Some(160),
            Some(cat_frame_response_format()),
            true,
        )?;
        let json_text = extract_json_object(&text)
            .ok_or_else(|| "cat verification response did not contain a JSON object".to_string())?;
        let mut payload = serde_json::from_str::<CatFrameVerificationPayload>(json_text)
            .map_err(|error| format!("failed to parse cat verification JSON: {error}"))?;
        normalize_cat_frame_payload(&mut payload);
        validate_cat_frame_payload(&payload)?;
        Ok(CatFrameVerificationResponse {
            cat_present: payload.reason_code == "cat_visible",
            behavior_tags: payload.behavior_tags,
            summary: payload.summary,
            reason_code: payload.reason_code,
            raw_response,
        })
    }

    pub fn describe_scene_for_cat_gate(
        &self,
        image_data_url: &str,
    ) -> Result<CatSceneDescriptionResponse, String> {
        let system_prompt = "Describe only the scene and objects clearly visible in the image pixels. Explicitly name visible animals, people, vehicles, furniture, plants, bags, and screens when present. Do not speculate about hidden objects. Write one concise Chinese sentence.";
        let user_prompt = "请客观描述这张摄像头抽样帧里清晰可见的场景和物体。";
        let (text, raw_response) = self.complete_vision(
            system_prompt,
            user_prompt,
            image_data_url,
            0.0,
            Some(120),
            Some(cat_scene_description_response_format()),
            true,
        )?;
        let json_text = extract_json_object(&text).ok_or_else(|| {
            "scene description response did not contain a JSON object".to_string()
        })?;
        let payload = serde_json::from_str::<CatSceneDescriptionPayload>(json_text)
            .map_err(|error| format!("failed to parse scene description JSON: {error}"))?;
        if payload.summary.trim().is_empty() {
            return Err("scene description response omitted summary".to_string());
        }
        Ok(CatSceneDescriptionResponse {
            summary: payload.summary.trim().to_string(),
            raw_response,
        })
    }

    fn complete_vision(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        image_data_url: &str,
        temperature: f32,
        max_tokens: Option<u32>,
        response_format: Option<Value>,
        disable_thinking: bool,
    ) -> Result<(String, Value), String> {
        let mut payload = json!({
            "model": self.config.model,
            "temperature": temperature,
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
                            "text": user_prompt
                        },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": image_data_url
                            }
                        }
                    ]
                }
            ]
        });
        if let Some(max_tokens) = max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(response_format) = response_format {
            payload["response_format"] = response_format;
        }
        if disable_thinking {
            payload["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .headers(self.headers()?)
            .json(&payload)
            .send()
            .map_err(|error| format!("OpenAI-compatible request failed: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<body unavailable>".to_string());
            return Err(format!("OpenAI-compatible API error {status}: {body}"));
        }
        let raw_response = response
            .json::<Value>()
            .map_err(|error| format!("failed to parse OpenAI-compatible response: {error}"))?;
        let text = extract_message_text(&raw_response).ok_or_else(|| {
            "OpenAI-compatible response did not contain assistant text".to_string()
        })?;
        Ok((text, raw_response))
    }

    fn headers(&self) -> Result<HeaderMap, String> {
        openai_compatible_headers(&self.config.api_key)
    }
}

fn cat_frame_response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "cat_frame_verification",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "behavior_tags": {
                        "type": "array",
                        "minItems": 0,
                        "maxItems": 4,
                        "uniqueItems": true,
                        "items": {
                            "type": "string",
                            "enum": [
                                "walking", "running", "jumping", "eating", "drinking",
                                "playing", "resting", "grooming", "unknown"
                            ]
                        }
                    },
                    "summary": {"type": "string", "minLength": 1, "maxLength": 120},
                    "reason_code": {
                        "type": "string",
                        "enum": ["cat_visible", "no_cat_visible", "uncertain", "invalid_frame"]
                    }
                },
                "required": [
                    "behavior_tags", "summary", "reason_code"
                ],
                "additionalProperties": false
            }
        }
    })
}

fn cat_scene_description_response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "scene_description",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "minLength": 1, "maxLength": 180}
                },
                "required": ["summary"],
                "additionalProperties": false
            }
        }
    })
}

fn normalize_cat_frame_payload(payload: &mut CatFrameVerificationPayload) {
    let mut unique_tags = Vec::with_capacity(payload.behavior_tags.len());
    for tag in payload.behavior_tags.drain(..) {
        if !unique_tags.contains(&tag) {
            unique_tags.push(tag);
        }
    }
    payload.behavior_tags = unique_tags;
}

fn validate_cat_frame_payload(payload: &CatFrameVerificationPayload) -> Result<(), String> {
    if payload.summary.trim().is_empty() || payload.reason_code.trim().is_empty() {
        return Err("cat verification response omitted summary or reason_code".to_string());
    }
    let valid_reason_code = matches!(
        payload.reason_code.as_str(),
        "cat_visible" | "no_cat_visible" | "uncertain" | "invalid_frame"
    );
    if !valid_reason_code {
        return Err("cat verification response contained an invalid reason_code".to_string());
    }
    if payload.reason_code != "cat_visible" && !payload.behavior_tags.is_empty() {
        return Err("cat verification response attached behavior to a negative result".to_string());
    }
    if payload.behavior_tags.len() > 4
        || payload.behavior_tags.iter().any(|tag| {
            !matches!(
                tag.as_str(),
                "walking"
                    | "running"
                    | "jumping"
                    | "eating"
                    | "drinking"
                    | "playing"
                    | "resting"
                    | "grooming"
                    | "unknown"
            )
        })
    {
        return Err("cat verification response contained invalid behavior tags".to_string());
    }
    let mut unique_tags = payload.behavior_tags.clone();
    unique_tags.sort_unstable();
    unique_tags.dedup();
    if unique_tags.len() != payload.behavior_tags.len() {
        return Err("cat verification response contained duplicate behavior tags".to_string());
    }
    Ok(())
}

fn extract_json_object(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end).then_some(&trimmed[start..=end])
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
        if request.disable_thinking {
            payload.insert(
                "chat_template_kwargs".to_string(),
                json!({"enable_thinking": false}),
            );
        }
        if request.json_object_response {
            payload.insert(
                "response_format".to_string(),
                json!({"type": "json_object"}),
            );
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
        Self::new_with_timeout(config, std::time::Duration::from_secs(45))
    }

    pub fn new_with_timeout(
        config: OpenAiCompatibleConfig,
        timeout: std::time::Duration,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(timeout.max(std::time::Duration::from_millis(1)))
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
            "texts": request.documents,
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
    let Some(results) = value
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
    else {
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
    use super::{
        cat_frame_response_format, cat_scene_description_response_format, extract_embedding_vector,
        extract_json_object, extract_message_text, extract_rerank_scores,
        normalize_cat_frame_payload, validate_cat_frame_payload, CatFrameVerificationPayload,
        VISION_SUMMARY_MAX_TOKENS,
    };
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
    fn vision_summary_generation_is_bounded() {
        assert_eq!(VISION_SUMMARY_MAX_TOKENS, 160);
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

    #[test]
    fn extract_rerank_scores_supports_tei_array_shape() {
        let response = json!([
            {"index": 0, "score": 0.31},
            {"index": 1, "score": 0.91}
        ]);

        let scores = extract_rerank_scores(&response);
        assert_eq!(scores[0].index, 1);
        assert!((scores[0].score - 0.91).abs() < f32::EPSILON);
    }

    #[test]
    fn cat_verification_json_is_extracted_and_parsed_strictly() {
        let text = "```json\n{\"behavior_tags\":[\"walking\"],\"summary\":\"猫在房间内走动\",\"reason_code\":\"cat_visible\"}\n```";
        let payload = serde_json::from_str::<CatFrameVerificationPayload>(
            extract_json_object(text).expect("json object"),
        )
        .expect("valid verification payload");

        assert_eq!(payload.reason_code, "cat_visible");
    }

    #[test]
    fn cat_verification_schema_bounds_model_generated_arrays() {
        let response_format = cat_frame_response_format();
        let schema = &response_format["json_schema"]["schema"];

        assert_eq!(response_format["type"], json!("json_schema"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["behavior_tags"]["maxItems"], json!(4));
        assert_eq!(
            cat_scene_description_response_format()["json_schema"]["schema"]["required"],
            json!(["summary"])
        );
    }

    #[test]
    fn cat_verification_payload_rejects_invalid_reason_code() {
        let valid = CatFrameVerificationPayload {
            behavior_tags: Vec::new(),
            summary: "未看到猫".to_string(),
            reason_code: "no_cat_visible".to_string(),
        };
        validate_cat_frame_payload(&valid).expect("consistent negative result");

        let invalid = CatFrameVerificationPayload {
            reason_code: "unsupported".to_string(),
            ..valid
        };
        assert!(validate_cat_frame_payload(&invalid)
            .expect_err("invalid reason code")
            .contains("invalid reason_code"));
    }

    #[test]
    fn cat_verification_payload_rejects_negative_behavior_and_reason_mismatch() {
        let negative_with_behavior = CatFrameVerificationPayload {
            behavior_tags: vec!["walking".to_string()],
            summary: "未看到猫".to_string(),
            reason_code: "no_cat_visible".to_string(),
        };
        assert!(validate_cat_frame_payload(&negative_with_behavior)
            .expect_err("negative result must not contain behavior")
            .contains("behavior"));

        let invalid_behavior = CatFrameVerificationPayload {
            behavior_tags: vec!["sleeping".to_string()],
            summary: "看到猫".to_string(),
            reason_code: "cat_visible".to_string(),
        };
        assert!(validate_cat_frame_payload(&invalid_behavior)
            .expect_err("behavior tag must be supported")
            .contains("behavior tags"));
    }

    #[test]
    fn cat_verification_payload_normalizes_duplicate_model_output() {
        let mut payload = CatFrameVerificationPayload {
            behavior_tags: vec![
                "eating".to_string(),
                "eating".to_string(),
                "unknown".to_string(),
            ],
            summary: "看到猫".to_string(),
            reason_code: "cat_visible".to_string(),
        };

        normalize_cat_frame_payload(&mut payload);

        assert_eq!(payload.behavior_tags, vec!["eating", "unknown"]);
        validate_cat_frame_payload(&payload).expect("normalized payload is valid");
    }
}
