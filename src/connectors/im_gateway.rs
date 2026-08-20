//! HarborGate status client for HarborBeacon admin surfaces.

use std::env;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::service_auth::beacon_to_gate_sender_token;

pub const CONTRACT_VERSION: &str = "2.0";
pub const IM_GATEWAY_BASE_URL_ENV: &str = "HARBORGATE_BASE_URL";
pub const LEGACY_IM_GATEWAY_BASE_URL_ENV: &str = "HARBOR_IM_GATEWAY_BASE_URL";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GatewayPlatformCapabilities {
    #[serde(default)]
    pub reply: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub attachments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GatewayPlatformStatus {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub capabilities: GatewayPlatformCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GatewayStatusResponse {
    #[serde(default)]
    #[serde(alias = "channels")]
    pub platforms: Vec<GatewayPlatformStatus>,
    #[serde(default)]
    pub manage_url: String,
    #[serde(default)]
    pub gateway_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayClientConfig {
    pub base_url: String,
    pub bearer_token: String,
}

impl GatewayClientConfig {
    pub fn from_env() -> Result<Self, String> {
        let base_url =
            env_var_with_legacy_alias(IM_GATEWAY_BASE_URL_ENV, LEGACY_IM_GATEWAY_BASE_URL_ENV)
                .ok_or_else(|| format!("missing required env var {IM_GATEWAY_BASE_URL_ENV}"))?;
        let bearer_token = beacon_to_gate_sender_token()?;
        Self::new(base_url, bearer_token)
    }

    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, String> {
        let base_url = base_url.into().trim().to_string();
        if base_url.is_empty() {
            return Err("HarborGate base URL cannot be empty".to_string());
        }
        let bearer_token = bearer_token.into().trim().to_string();
        if bearer_token.is_empty() {
            return Err("HarborGate bearer token cannot be empty".to_string());
        }
        Ok(Self {
            base_url,
            bearer_token,
        })
    }
}

fn env_var_with_legacy_alias(primary: &str, legacy: &str) -> Option<String> {
    if let Some(value) = env::var(primary)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }

    env::var(legacy)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .inspect(|_| {
            eprintln!("warning: {legacy} is deprecated; prefer {primary}");
        })
}

pub struct GatewayStatusClient {
    client: Client,
    config: GatewayClientConfig,
}

impl GatewayStatusClient {
    pub fn new() -> Result<Self, String> {
        let config = GatewayClientConfig::from_env()?;
        Self::from_config(config)
    }

    pub fn from_config(config: GatewayClientConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("failed to build HarborGate status client: {error}"))?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &GatewayClientConfig {
        &self.config
    }

    pub fn fetch_status(&self) -> Result<GatewayStatusResponse, String> {
        let response = self
            .client
            .get(status_endpoint(&self.config.base_url))
            .header(
                "Authorization",
                format!("Bearer {}", self.config.bearer_token),
            )
            .header("X-Contract-Version", CONTRACT_VERSION)
            .send()
            .map_err(|error| format!("HarborGate status request failed: {error}"))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|error| format!("failed to read HarborGate status response: {error}"))?;
        if !status.is_success() {
            return Err(format!(
                "HarborGate status request failed with HTTP {}: {}",
                status.as_u16(),
                body
            ));
        }
        serde_json::from_str(&body)
            .map_err(|error| format!("failed to parse HarborGate status response: {error}"))
    }
}

fn status_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/gateway/status") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/gateway/status")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::{
        GatewayClientConfig, GatewayStatusClient, GatewayStatusResponse, CONTRACT_VERSION,
    };

    #[test]
    fn fetch_status_sends_auth_and_contract_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || -> String {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_http_request(&mut stream);
            let body = r#"{"platforms":[{"platform":"feishu","enabled":true,"connected":true,"display_name":"HarborBeacon Bot","capabilities":{"reply":true,"update":true,"attachments":true}}]}"#;
            write_http_response(&mut stream, 200, "OK", body);
            request
        });

        let client = GatewayStatusClient::from_config(
            GatewayClientConfig::new(format!("http://{address}"), "test-token").expect("config"),
        )
        .expect("client");

        let response = client.fetch_status().expect("status");
        let request = server.join().expect("server thread").to_lowercase();

        assert!(request.starts_with("get /api/gateway/status http/1.1"));
        assert!(request.contains("authorization: bearer test-token"));
        assert!(request.contains(&format!(
            "x-contract-version: {}",
            CONTRACT_VERSION.to_lowercase()
        )));
        assert_eq!(
            response,
            GatewayStatusResponse {
                platforms: vec![super::GatewayPlatformStatus {
                    platform: "feishu".to_string(),
                    enabled: true,
                    connected: true,
                    display_name: "HarborBeacon Bot".to_string(),
                    capabilities: super::GatewayPlatformCapabilities {
                        reply: true,
                        update: true,
                        attachments: true,
                    },
                }],
                manage_url: String::new(),
                gateway_base_url: String::new(),
            }
        );
    }

    #[test]
    fn fetch_status_accepts_redacted_channels_shape() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let _request = read_http_request(&mut stream);
            let body = r#"{"ok":true,"gateway_version":"0.1.0","channels":[{"platform":"feishu","enabled":true,"connected":false,"display_name":"HarborBeacon Bot","capabilities":{"reply":true,"update":false,"attachments":false}}]}"#;
            write_http_response(&mut stream, 200, "OK", body);
        });

        let client = GatewayStatusClient::from_config(
            GatewayClientConfig::new(format!("http://{address}"), "test-token").expect("config"),
        )
        .expect("client");

        let response = client.fetch_status().expect("status");
        server.join().expect("server thread");

        assert_eq!(
            response,
            GatewayStatusResponse {
                platforms: vec![super::GatewayPlatformStatus {
                    platform: "feishu".to_string(),
                    enabled: true,
                    connected: false,
                    display_name: "HarborBeacon Bot".to_string(),
                    capabilities: super::GatewayPlatformCapabilities {
                        reply: true,
                        update: false,
                        attachments: false,
                    },
                }],
                manage_url: String::new(),
                gateway_base_url: String::new(),
            }
        );
    }

    #[test]
    fn config_requires_non_empty_values() {
        assert!(GatewayClientConfig::new("", "token").is_err());
        assert!(GatewayClientConfig::new("http://127.0.0.1:4176", "").is_err());
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buffer = [0_u8; 4096];
        let mut request_bytes = Vec::new();
        loop {
            let bytes_read = stream.read(&mut buffer).expect("read request");
            if bytes_read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&buffer[..bytes_read]);
            if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&request_bytes).to_string()
    }

    fn write_http_response(
        stream: &mut std::net::TcpStream,
        status_code: u16,
        reason: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status_code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    }
}
