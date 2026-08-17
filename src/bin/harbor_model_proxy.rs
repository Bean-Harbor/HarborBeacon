use std::io::Cursor;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use reqwest::Url;
use serde_json::json;
use tiny_http::{Header, Method, Response, StatusCode};

const DEFAULT_UPSTREAM: &str = "http://127.0.0.1:8792";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone)]
pub struct ModelApiProxy {
    client: Client,
    upstream: Url,
}

impl ModelApiProxy {
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("HARBOR_MODEL_API_UPSTREAM_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string());
        let upstream = validate_loopback_upstream(&raw)?;
        let timeout_ms = std::env::var("HARBOR_MODEL_API_REQUEST_TIMEOUT_MS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|error| format!("invalid request timeout: {error}"))?
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err("request timeout must be greater than zero".to_string());
        }
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|error| format!("failed to create model proxy client: {error}"))?;
        Ok(Self { client, upstream })
    }

    pub fn route(&self, method: Method, path: &str, body: &[u8]) -> Response<Cursor<Vec<u8>>> {
        match (&method, path) {
            (Method::Get, "/healthz") => self.forward(method, path, body),
            (Method::Post, "/v1/chat/completions" | "/v1/embeddings") => {
                self.forward(method, path, body)
            }
            (Method::Options, _) => Response::from_data(Vec::new()).with_status_code(204),
            _ => json_response(
                StatusCode(404),
                json!({
                    "ok": false,
                    "error": {"code": "ROUTE_NOT_FOUND", "message": "model route not found"}
                }),
            ),
        }
    }

    fn forward(&self, method: Method, path: &str, body: &[u8]) -> Response<Cursor<Vec<u8>>> {
        let url = match self.upstream.join(path.trim_start_matches('/')) {
            Ok(url) => url,
            Err(_) => {
                return proxy_error(
                    StatusCode(500),
                    "MODEL_PROXY_CONFIGURATION_ERROR",
                    "model proxy path could not be resolved",
                )
            }
        };
        let request = match method {
            Method::Get => self.client.get(url),
            Method::Post => self
                .client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_vec()),
            _ => {
                return proxy_error(
                    StatusCode(405),
                    "METHOD_NOT_ALLOWED",
                    "model proxy method is not allowed",
                )
            }
        };
        let upstream = match request.send() {
            Ok(response) => response,
            Err(_) => {
                return proxy_error(
                    StatusCode(503),
                    "MODEL_RUNTIME_UNAVAILABLE",
                    "local model runtime is unavailable",
                )
            }
        };
        let status = StatusCode(upstream.status().as_u16());
        let content_type = upstream
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json; charset=utf-8")
            .to_string();
        let response_body = match upstream.bytes() {
            Ok(value) => value.to_vec(),
            Err(_) => {
                return proxy_error(
                    StatusCode(502),
                    "MODEL_RUNTIME_READ_ERROR",
                    "local model runtime response could not be read",
                )
            }
        };
        let mut response = Response::from_data(response_body).with_status_code(status);
        response.add_header(
            Header::from_bytes("Content-Type", content_type)
                .expect("static content-type header must be valid"),
        );
        response
    }
}

fn validate_loopback_upstream(raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw.trim()).map_err(|error| format!("invalid URL: {error}"))?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return Err("model runtime upstream must be http://127.0.0.1".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err("model runtime upstream must not contain credentials or a query".to_string());
    }
    if url.port_or_known_default().is_none() {
        return Err("model runtime upstream must have a valid port".to_string());
    }
    url.set_path("/");
    url.set_fragment(None);
    Ok(url)
}

fn proxy_error(status: StatusCode, code: &str, message: &str) -> Response<Cursor<Vec<u8>>> {
    json_response(
        status,
        json!({"ok": false, "error": {"code": code, "message": message}}),
    )
}

fn json_response(status: StatusCode, payload: serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_data(
        serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"ok\":false}".to_vec()),
    )
    .with_status_code(status);
    response.add_header(
        Header::from_bytes("Content-Type", "application/json; charset=utf-8")
            .expect("static content-type header must be valid"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_model_runtime_must_be_loopback_http() {
        assert!(validate_loopback_upstream("http://127.0.0.1:8792").is_ok());
        assert!(validate_loopback_upstream("https://127.0.0.1:8792").is_err());
        assert!(validate_loopback_upstream("http://0.0.0.0:8792").is_err());
        assert!(validate_loopback_upstream("http://192.168.1.10:8792").is_err());
        assert!(validate_loopback_upstream("http://user:secret@127.0.0.1:8792").is_err());
    }

    #[test]
    fn external_model_runtime_exposes_only_required_routes() {
        let proxy = ModelApiProxy::from_env().expect("default proxy");
        let response = proxy.route(Method::Get, "/v1/models", &[]);
        assert_eq!(response.status_code(), StatusCode(404));
        let response = proxy.route(Method::Options, "/v1/chat/completions", &[]);
        assert_eq!(response.status_code(), StatusCode(204));
    }
}
