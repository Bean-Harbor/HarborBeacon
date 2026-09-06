use std::io::Cursor;
use std::time::Duration;

use harborbeacon_local_agent::runtime::ai_execution::{
    request_execution_cancel, EXECUTION_ID_HEADER,
};
use harborbeacon_local_agent::runtime::fixed_models;
use harborbeacon_local_agent::service_auth::VerifierTokens;
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
    verifier: VerifierTokens,
}

impl ModelApiProxy {
    pub fn warm_fixed_model(&self) {
        if !fixed_models::FIXED {
            return;
        }
        let proxy = self.clone();
        std::thread::spawn(move || {
            for _ in 0..30 {
                if proxy
                    .client
                    .get("http://127.0.0.1:8792/healthz")
                    .timeout(Duration::from_secs(1))
                    .send()
                    .is_ok_and(|value| value.status().is_success())
                {
                    let body = serde_json::to_vec(&json!({
                        "model": fixed_models::CHAT_MODEL,
                        "messages": [{"role": "user", "content": "Ready."}],
                        "max_tokens": 1, "temperature": 0
                    }))
                    .unwrap();
                    let response = proxy.forward(Method::Post, "/v1/chat/completions", &body);
                    eprintln!("fixed model warmup completed: {}", response.status_code().0);
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            eprintln!("fixed model warmup failed: runtime unavailable");
        });
    }

    pub fn from_env(verifier: VerifierTokens) -> Result<Self, String> {
        let raw = if fixed_models::FIXED {
            DEFAULT_UPSTREAM.to_string()
        } else {
            std::env::var("HARBOR_MODEL_API_UPSTREAM_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_string())
        };
        let upstream = validate_loopback_upstream(&raw)?;
        let timeout_ms = if fixed_models::FIXED {
            110_000
        } else {
            std::env::var("HARBOR_MODEL_API_REQUEST_TIMEOUT_MS")
                .ok()
                .map(|value| value.parse::<u64>())
                .transpose()
                .map_err(|error| format!("invalid request timeout: {error}"))?
                .unwrap_or(DEFAULT_TIMEOUT_MS)
        };
        if timeout_ms == 0 {
            return Err("request timeout must be greater than zero".to_string());
        }
        let client = Client::builder().timeout(Duration::from_millis(timeout_ms));
        let client = if fixed_models::FIXED {
            client.no_proxy().redirect(reqwest::redirect::Policy::none())
        } else {
            client
        };
        let client = client
            .build()
            .map_err(|error| format!("failed to create model proxy client: {error}"))?;
        Ok(Self {
            client,
            upstream,
            verifier,
        })
    }

    pub fn route(
        &self,
        method: Method,
        path: &str,
        headers: &[Header],
        body: &[u8],
    ) -> Response<Cursor<Vec<u8>>> {
        if method == Method::Post
            && !headers
                .iter()
                .find(|header| header.field.equiv("Authorization"))
                .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
                .is_some_and(|token| self.verifier.matches(token))
        {
            return proxy_error(
                StatusCode(401),
                "UNAUTHORIZED",
                "model authentication required",
            );
        }
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
        let execution_id = (fixed_models::FIXED && method == Method::Post)
            .then(|| uuid::Uuid::new_v4().to_string());
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
        let mut request = match method {
            Method::Get => self.client.get(url),
            Method::Post => self
                .client
                .post(url)
                .bearer_auth(&self.verifier.current)
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
        if let Some(id) = &execution_id {
            request = request.header(EXECUTION_ID_HEADER, id);
        }
        let upstream = match request.send() {
            Ok(response) => response,
            Err(error) => {
                if !error.is_connect() {
                    self.cancel_execution(execution_id.as_deref());
                }
                return proxy_error(
                    StatusCode(503),
                    "MODEL_RUNTIME_UNAVAILABLE",
                    "local model runtime is unavailable",
                );
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
                self.cancel_execution(execution_id.as_deref());
                return proxy_error(
                    StatusCode(502),
                    "MODEL_RUNTIME_READ_ERROR",
                    "local model runtime response could not be read",
                );
            }
        };
        let mut response = Response::from_data(response_body).with_status_code(status);
        response.add_header(
            Header::from_bytes("Content-Type", content_type)
                .expect("static content-type header must be valid"),
        );
        response
    }

    fn cancel_execution(&self, execution_id: Option<&str>) {
        if let Some(id) = execution_id {
            // Runtime owns termination and the lease even if cancellation delivery fails.
            request_execution_cancel(&self.client, &self.upstream, &self.verifier.current, id);
        }
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

    #[cfg(feature = "fixed-local-models")]
    #[test]
    fn fixed_proxy_delegates_execution_ownership_and_cancels_transport_timeout() {
        use harborbeacon_local_agent::runtime::ai_resource_scheduler::{
            acquire_ai_resource_lease, ai_resource_workload_snapshot, AiWorkload,
        };
        use std::process::{Command, Stdio};
        use std::time::Instant;
        if std::env::var_os("HARBOR_PROXY_EXECUTION_FIXTURE").is_none() {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "harbor_model_proxy::tests::fixed_proxy_delegates_execution_ownership_and_cancels_transport_timeout", "--nocapture"])
                .env("HARBOR_PROXY_EXECUTION_FIXTURE", "1")
                .stdin(Stdio::null()).spawn().unwrap();
            let until = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    return;
                }
                if Instant::now() >= until {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("proxy execution fixture timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let token = "test_model_token_0123456789abcdef0123456789abcdef";
        let proxy = ModelApiProxy {
            client: Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            upstream: Url::parse(&format!("http://{}/", server.server_addr())).unwrap(),
            verifier: VerifierTokens::current_only(token).unwrap(),
        };
        let cat = acquire_ai_resource_lease(AiWorkload::CatRecordingVerifier).unwrap();
        let inference_proxy = proxy.clone();
        let inference = std::thread::spawn(move || {
            inference_proxy.route(
                Method::Post,
                "/v1/chat/completions",
                &[Header::from_bytes("Authorization", format!("Bearer {token}")).unwrap()],
                b"{}",
            )
        });
        let request = server
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .expect("proxy must not acquire the caller's scheduler lease");
        assert_eq!(request.url(), "/v1/chat/completions");
        let execution_id = request
            .headers()
            .iter()
            .find(|header| header.field.equiv(EXECUTION_ID_HEADER))
            .unwrap()
            .value
            .to_string();
        assert!(uuid::Uuid::parse_str(&execution_id).is_ok());
        assert_eq!(
            ai_resource_workload_snapshot(AiWorkload::Llm)["started_total"],
            0
        );
        request
            .respond(
                Response::from_string("{}")
                    .with_header(Header::from_bytes("X-Harbor-Execution-Stopped", "true").unwrap()),
            )
            .unwrap();
        assert_eq!(inference.join().unwrap().status_code(), StatusCode(200));
        let health_proxy = proxy.clone();
        let health =
            std::thread::spawn(move || health_proxy.route(Method::Get, "/healthz", &[], &[]));
        let request = server
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(request.url(), "/healthz");
        request.respond(Response::from_string("{}")).unwrap();
        assert_eq!(health.join().unwrap().status_code(), StatusCode(200));
        drop(cat);
        let mut timeout_proxy = proxy;
        timeout_proxy.client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let uncertain = std::thread::spawn(move || {
            timeout_proxy.forward(Method::Post, "/v1/chat/completions", b"{}")
        });
        let running = server
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let id = running
            .headers()
            .iter()
            .find(|header| header.field.equiv(EXECUTION_ID_HEADER))
            .unwrap()
            .value
            .to_string();
        let cancellation = server
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(
            cancellation.url(),
            format!("/internal/ai/executions/{id}/cancel")
        );
        assert!(cancellation
            .headers()
            .iter()
            .any(|header| header.field.equiv("Authorization")
                && header.value.as_str() == format!("Bearer {token}")));
        cancellation
            .respond(Response::from_string("{\"execution_stopped\":false}").with_status_code(202))
            .unwrap();
        assert_eq!(uncertain.join().unwrap().status_code(), StatusCode(503));
        let _ = running.respond(Response::from_string("{}"));
        assert_eq!(
            ai_resource_workload_snapshot(AiWorkload::Llm)["started_total"],
            0
        );
        drop(acquire_ai_resource_lease(AiWorkload::CatRecordingVerifier).unwrap());
    }

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
        let proxy = ModelApiProxy::from_env(
            VerifierTokens::current_only("test_model_token_0123456789abcdef0123456789abcdef")
                .unwrap(),
        )
        .expect("default proxy");
        let response = proxy.route(Method::Get, "/v1/models", &[], &[]);
        assert_eq!(response.status_code(), StatusCode(404));
        let response = proxy.route(Method::Options, "/v1/chat/completions", &[], &[]);
        assert_eq!(response.status_code(), StatusCode(204));
    }
}
