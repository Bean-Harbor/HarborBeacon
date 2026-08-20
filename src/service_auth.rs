//! Directional service credentials for the HarborGate HTTP/JSON boundary.

use std::env;
use std::fs;

pub const GATE_TO_BEACON_TOKEN_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN";
pub const GATE_TO_BEACON_TOKEN_FILE_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN_FILE";
pub const GATE_TO_BEACON_TOKEN_PREVIOUS_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS";
pub const GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV: &str =
    "HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS_FILE";
pub const BEACON_TO_GATE_TOKEN_ENV: &str = "HARBOR_BEACON_TO_GATE_TOKEN";
pub const BEACON_TO_GATE_TOKEN_FILE_ENV: &str = "HARBOR_BEACON_TO_GATE_TOKEN_FILE";

const LEGACY_GATE_TO_BEACON_TOKEN_ENVS: &[&str] = &["HARBORBEACON_WEB_API_TOKEN"];
const LEGACY_BEACON_TO_GATE_TOKEN_ENVS: &[&str] = &[
    "HARBORGATE_BEARER_TOKEN",
    "HARBOR_IM_GATEWAY_BEARER_TOKEN",
    "IM_AGENT_SERVICE_TOKEN",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierTokens {
    pub current: String,
    pub previous: Option<String>,
}

impl VerifierTokens {
    pub fn matches(&self, actual: &str) -> bool {
        !self.current.is_empty()
            && (constant_time_token_eq(actual, &self.current)
                || self
                    .previous
                    .as_deref()
                    .is_some_and(|previous| constant_time_token_eq(actual, previous)))
    }
}

pub fn gate_to_beacon_verifier_tokens() -> Result<VerifierTokens, String> {
    Ok(VerifierTokens {
        current: required_token(
            GATE_TO_BEACON_TOKEN_FILE_ENV,
            GATE_TO_BEACON_TOKEN_ENV,
            LEGACY_GATE_TO_BEACON_TOKEN_ENVS,
        )?,
        previous: optional_token(
            GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV,
            GATE_TO_BEACON_TOKEN_PREVIOUS_ENV,
        )?,
    })
}

pub fn beacon_to_gate_sender_token() -> Result<String, String> {
    required_token(
        BEACON_TO_GATE_TOKEN_FILE_ENV,
        BEACON_TO_GATE_TOKEN_ENV,
        LEGACY_BEACON_TO_GATE_TOKEN_ENVS,
    )
}

pub fn validate_required_service_auth() -> Result<(), String> {
    gate_to_beacon_verifier_tokens()?;
    beacon_to_gate_sender_token()?;
    Ok(())
}

fn required_token(
    file_env: &str,
    primary_env: &str,
    legacy_envs: &[&str],
) -> Result<String, String> {
    if let Some(value) = token_from_file_env(file_env)? {
        return Ok(value);
    }
    if let Some(value) = token_from_env(primary_env) {
        return Ok(value);
    }
    for legacy_env in legacy_envs {
        if let Some(value) = token_from_env(legacy_env) {
            eprintln!("warning: {legacy_env} is deprecated; prefer {primary_env}");
            return Ok(value);
        }
    }
    Err(format!("missing required service credential {primary_env}"))
}

fn optional_token(file_env: &str, primary_env: &str) -> Result<Option<String>, String> {
    if let Some(path) = env::var(file_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let value = fs::read_to_string(&path)
            .map_err(|error| {
                format!("failed to read service credential configured by {file_env}: {error}")
            })?
            .trim()
            .to_string();
        return Ok((!value.is_empty()).then_some(value));
    }
    Ok(token_from_env(primary_env))
}

fn token_from_file_env(file_env: &str) -> Result<Option<String>, String> {
    let Some(path) = env::var(file_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let value = fs::read_to_string(&path)
        .map_err(|error| {
            format!("failed to read service credential configured by {file_env}: {error}")
        })?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(format!(
            "service credential configured by {file_env} is empty"
        ));
    }
    Ok(Some(value))
}

fn token_from_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn constant_time_token_eq(actual: &str, expected: &str) -> bool {
    if actual.is_empty() || expected.is_empty() || actual.len() != expected.len() {
        return false;
    }
    actual
        .as_bytes()
        .iter()
        .zip(expected.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (*left ^ *right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::VerifierTokens;

    #[test]
    fn verifier_accepts_current_and_previous_but_not_other_domain() {
        let tokens = VerifierTokens {
            current: "gate-to-beacon-current".to_string(),
            previous: Some("gate-to-beacon-previous".to_string()),
        };

        assert!(tokens.matches("gate-to-beacon-current"));
        assert!(tokens.matches("gate-to-beacon-previous"));
        assert!(!tokens.matches("beacon-to-gate-current"));
        assert!(!tokens.matches(""));
    }

    #[test]
    fn previous_cannot_replace_missing_current() {
        let tokens = VerifierTokens {
            current: String::new(),
            previous: Some("gate-to-beacon-previous".to_string()),
        };

        assert!(!tokens.matches("gate-to-beacon-previous"));
    }
}
