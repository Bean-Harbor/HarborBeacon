//! Directional service credentials for the HarborGate HTTP/JSON boundary.

use std::env;
use std::fs;
use std::path::PathBuf;

use constant_time_eq::constant_time_eq;

pub const GATE_TO_BEACON_TOKEN_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN";
pub const GATE_TO_BEACON_TOKEN_FILE_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN_FILE";
pub const GATE_TO_BEACON_TOKEN_PREVIOUS_ENV: &str = "HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS";
pub const GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV: &str =
    "HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS_FILE";
pub const BEACON_TO_GATE_TOKEN_ENV: &str = "HARBOR_BEACON_TO_GATE_TOKEN";
pub const BEACON_TO_GATE_TOKEN_FILE_ENV: &str = "HARBOR_BEACON_TO_GATE_TOKEN_FILE";
pub const MODEL_API_TOKEN_ENV: &str = "HARBOR_MODEL_API_TOKEN";

const MIN_TOKEN_LEN: usize = 32;
const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const GATE_TO_BEACON_CURRENT_CREDENTIAL: &str = "gate-to-beacon-accept-current";
const GATE_TO_BEACON_PREVIOUS_CREDENTIAL: &str = "gate-to-beacon-accept-previous";
const BEACON_TO_GATE_SEND_CREDENTIAL: &str = "beacon-to-gate-send";

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
    pub fn current_only(current: impl Into<String>) -> Result<Self, String> {
        let current = validate_configured_token(current.into(), "bearer token")?;
        Ok(Self {
            current,
            previous: None,
        })
    }

    pub fn matches(&self, actual: &str) -> bool {
        // Evaluate both configured slots. Comparison work depends on configured
        // token lengths, never on secret contents or which slot matches.
        let current_matches = constant_time_token_eq(actual, &self.current);
        let previous_matches = self
            .previous
            .as_deref()
            .map(|previous| constant_time_token_eq(actual, previous))
            .unwrap_or(false);
        !self.current.is_empty() & (current_matches | previous_matches)
    }
}

pub fn gate_to_beacon_file_verifier_tokens() -> Result<VerifierTokens, String> {
    Ok(VerifierTokens {
        current: required_file_token(GATE_TO_BEACON_TOKEN_FILE_ENV)?,
        previous: optional_file_token(GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV)?,
    })
}

pub fn model_api_verifier_token() -> Result<VerifierTokens, String> {
    let token = token_from_env(MODEL_API_TOKEN_ENV)
        .ok_or_else(|| format!("{MODEL_API_TOKEN_ENV} is not configured"))?;
    let token = validate_configured_token(token, MODEL_API_TOKEN_ENV)?;
    Ok(VerifierTokens {
        current: token,
        previous: None,
    })
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
        return validate_configured_token(value, file_env);
    }
    if let Some(value) = token_from_env(primary_env) {
        return validate_configured_token(value, primary_env);
    }
    for legacy_env in legacy_envs {
        if let Some(value) = token_from_env(legacy_env) {
            eprintln!("warning: {legacy_env} is deprecated; prefer {primary_env}");
            return validate_configured_token(value, legacy_env);
        }
    }
    Err(format!("missing required service credential {primary_env}"))
}

fn optional_token(file_env: &str, primary_env: &str) -> Result<Option<String>, String> {
    if let Some(path) = credential_file_path(file_env) {
        let value = fs::read_to_string(&path)
            .map_err(|error| {
                format!("failed to read service credential configured by {file_env}: {error}")
            })?
            .trim()
            .to_string();
        return if value.is_empty() {
            Ok(None)
        } else {
            validate_configured_token(value, file_env).map(Some)
        };
    }
    token_from_env(primary_env)
        .map(|value| validate_configured_token(value, primary_env).map(Some))
        .unwrap_or(Ok(None))
}

fn required_file_token(file_env: &str) -> Result<String, String> {
    token_from_file_env(file_env)?
        .ok_or_else(|| format!("missing required service credential file setting {file_env}"))
        .and_then(|value| validate_configured_token(value, file_env))
}

fn optional_file_token(file_env: &str) -> Result<Option<String>, String> {
    let path = credential_file_path(file_env)
        .ok_or_else(|| format!("missing required service credential file setting {file_env}"))?;
    let value = fs::read_to_string(&path)
        .map_err(|error| {
            format!("failed to read service credential configured by {file_env}: {error}")
        })?
        .trim()
        .to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        validate_configured_token(value, file_env).map(Some)
    }
}

fn token_from_file_env(file_env: &str) -> Result<Option<String>, String> {
    let Some(path) = credential_file_path(file_env) else {
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

fn credential_file_path(file_env: &str) -> Option<PathBuf> {
    let credential_name = match file_env {
        GATE_TO_BEACON_TOKEN_FILE_ENV => GATE_TO_BEACON_CURRENT_CREDENTIAL,
        GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV => GATE_TO_BEACON_PREVIOUS_CREDENTIAL,
        BEACON_TO_GATE_TOKEN_FILE_ENV => BEACON_TO_GATE_SEND_CREDENTIAL,
        _ => return configured_file_path(file_env),
    };
    credential_file_path_from_values(
        &env::var(CREDENTIALS_DIRECTORY_ENV).unwrap_or_default(),
        &env::var(file_env).unwrap_or_default(),
        credential_name,
    )
}

fn configured_file_path(file_env: &str) -> Option<PathBuf> {
    env::var(file_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn credential_file_path_from_values(
    credentials_directory: &str,
    configured_file: &str,
    credential_name: &str,
) -> Option<PathBuf> {
    let credentials_directory = credentials_directory.trim();
    if !credentials_directory.is_empty() {
        return Some(PathBuf::from(credentials_directory).join(credential_name));
    }
    let configured_file = configured_file.trim();
    (!configured_file.is_empty()).then(|| PathBuf::from(configured_file))
}

fn token_from_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_configured_token(value: String, source: &str) -> Result<String, String> {
    if value.len() < MIN_TOKEN_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("{source} is invalid"));
    }
    Ok(value)
}

fn constant_time_token_eq(actual: &str, expected: &str) -> bool {
    constant_time_eq(actual.as_bytes(), expected.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{credential_file_path_from_values, VerifierTokens};
    use std::path::PathBuf;

    const VALID_CURRENT: &str = "gate_current_0123456789abcdef0123456789abcdef";

    #[test]
    fn systemd_credentials_directory_takes_precedence_over_configured_file() {
        assert_eq!(
            credential_file_path_from_values(
                "/run/credentials/harboros-beacon.service",
                "/tmp/explicit-token",
                "beacon-to-gate-send",
            ),
            Some(PathBuf::from(
                "/run/credentials/harboros-beacon.service/beacon-to-gate-send",
            ))
        );
        assert_eq!(
            credential_file_path_from_values("", "/tmp/explicit-token", "unused"),
            Some(PathBuf::from("/tmp/explicit-token"))
        );
        assert_eq!(credential_file_path_from_values("", "", "unused"), None);
    }

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

    #[test]
    fn current_only_verifier_rejects_invalid_configured_tokens() {
        assert!(VerifierTokens::current_only(VALID_CURRENT).is_ok());
        assert!(VerifierTokens::current_only("short").is_err());
        assert!(
            VerifierTokens::current_only("contains.invalid.character.0123456789abcdef").is_err()
        );
    }

    #[test]
    fn verifier_rejects_equal_length_mismatches_at_any_position() {
        let verifier = VerifierTokens::current_only(VALID_CURRENT).unwrap();
        for candidate in [
            "xate_current_0123456789abcdef0123456789abcdef",
            "gate_current_0123456789abcxef0123456789abcdef",
            "gate_current_0123456789abcdef0123456789abcdeg",
        ] {
            assert_eq!(candidate.len(), VALID_CURRENT.len());
            assert!(!verifier.matches(candidate));
        }
    }
}
