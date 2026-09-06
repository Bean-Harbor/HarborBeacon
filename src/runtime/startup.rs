//! Startup policy shared by the packaged service and standalone admin entry.

use serde::Serialize;

use crate::service_auth::{
    gate_to_beacon_file_verifier_tokens, gate_to_beacon_verifier_tokens, model_api_verifier_token,
    VerifierTokens,
};

pub const STARTUP_PROFILE_ENV: &str = "HARBOR_BEACON_STARTUP_PROFILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupProfile {
    N1,
    N2,
}

impl StartupProfile {
    pub fn from_env() -> Result<Self, String> {
        let configured = match std::env::var(STARTUP_PROFILE_ENV) {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(_) => return Err(format!("{STARTUP_PROFILE_ENV} must be valid UTF-8")),
        };
        Self::from_values(&configured, cfg!(feature = "external-model-runtime"))
    }

    fn from_values(configured: &str, external_runtime: bool) -> Result<Self, String> {
        let compiled = if external_runtime { Self::N2 } else { Self::N1 };
        let requested = match configured.trim() {
            "" => compiled,
            "n1" => Self::N1,
            "n2" => Self::N2,
            _ => return Err(format!("{STARTUP_PROFILE_ENV} must be n1 or n2")),
        };
        if requested != compiled {
            return Err(format!(
                "{STARTUP_PROFILE_ENV} does not match the compiled runtime"
            ));
        }
        Ok(requested)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::N1 => "n1",
            Self::N2 => "n2",
        }
    }

    pub fn isolate_optional_capabilities(self) -> bool {
        self == Self::N2
    }

    pub fn optional<T>(
        self,
        result: Result<T, String>,
        reason_code: &'static str,
    ) -> Result<Option<T>, String> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(_) if self.isolate_optional_capabilities() => {
                // Configuration errors can contain paths or URLs. Publish only a stable code.
                eprintln!("startup capability unavailable: {reason_code}");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn gate_verifier(
        self,
        cli_token: Option<String>,
    ) -> Result<Option<VerifierTokens>, String> {
        let verifier = match cli_token {
            Some(token) => VerifierTokens::current_only(token),
            None if self == Self::N1 => gate_to_beacon_file_verifier_tokens(),
            None => gate_to_beacon_verifier_tokens(),
        };
        self.optional(verifier, "GATE_AUTH_UNAVAILABLE")
    }

    pub fn model_verifier(self) -> Result<Option<VerifierTokens>, String> {
        self.optional(model_api_verifier_token(), "MODEL_AUTH_UNAVAILABLE")
    }
}

/// Configuration status, not proof that an external service or model is ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupCapability {
    pub capability: &'static str,
    pub state: &'static str,
    pub reason_code: Option<&'static str>,
}

impl StartupCapability {
    pub fn configured(capability: &'static str) -> Self {
        Self {
            capability,
            state: "configured",
            reason_code: None,
        }
    }

    pub fn unavailable(capability: &'static str, reason_code: &'static str) -> Self {
        Self {
            capability,
            state: "unavailable",
            reason_code: Some(reason_code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_default_to_the_compiled_topology() {
        assert_eq!(
            StartupProfile::from_values("", false).unwrap(),
            StartupProfile::N1
        );
        assert_eq!(
            StartupProfile::from_values("", true).unwrap(),
            StartupProfile::N2
        );
        assert_eq!(
            StartupProfile::from_values("n1", false).unwrap(),
            StartupProfile::N1
        );
        assert_eq!(
            StartupProfile::from_values("n2", true).unwrap(),
            StartupProfile::N2
        );
    }

    #[test]
    fn profile_cannot_reinterpret_another_product_build() {
        assert!(StartupProfile::from_values("n2", false).is_err());
        assert!(StartupProfile::from_values("n1", true).is_err());
        let error = StartupProfile::from_values("credential-like-input", true).unwrap_err();
        assert!(!error.contains("credential-like-input"));
    }

    #[test]
    fn n2_isolates_optional_failure_but_n1_keeps_strict_startup() {
        let failed = || Err::<(), _>("private configuration details".to_string());
        assert!(StartupProfile::N2
            .optional(failed(), "TEST_UNAVAILABLE")
            .unwrap()
            .is_none());
        assert!(StartupProfile::N1
            .optional(failed(), "TEST_UNAVAILABLE")
            .is_err());
        assert_eq!(
            StartupProfile::N2
                .optional(Ok(7), "TEST_UNAVAILABLE")
                .unwrap(),
            Some(7)
        );
    }

    #[test]
    fn startup_status_does_not_claim_runtime_readiness() {
        let configured =
            serde_json::to_value(StartupCapability::configured("local_inference")).unwrap();
        assert_eq!(configured["state"], "configured");
        assert!(configured.get("ready").is_none());
        let unavailable = serde_json::to_value(StartupCapability::unavailable(
            "gate_turns",
            "GATE_AUTH_UNAVAILABLE",
        ))
        .unwrap();
        assert_eq!(unavailable["state"], "unavailable");
        assert_eq!(unavailable["reason_code"], "GATE_AUTH_UNAVAILABLE");
    }

    #[test]
    fn packaged_units_select_profiles_and_keep_the_existing_resource_recovery_barrier() {
        let n1 = include_str!("../../debian/harboros-beacon.service");
        let n2 = include_str!("../../debian/n2/harboros-beacon.service");
        assert!(n1.contains("Environment=HARBOR_BEACON_STARTUP_PROFILE=n1"));
        assert!(n2.contains("Environment=HARBOR_BEACON_STARTUP_PROFILE=n2"));
        assert!(n2.contains("ExecStartPre=+/usr/bin/systemctl restart harboros-model-runtime.service"));
        assert!(n2.contains("ExecStartPre=+/usr/lib/harborbeacon/verify-k3-generation"));
    }
}
