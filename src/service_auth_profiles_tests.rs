use super::*;
use std::process::Command;

const CURRENT: &str = "env_current_0123456789abcdef0123456789abcdef";
const PREVIOUS: &str = "env_previous_0123456789abcdef0123456789abcdef";
const SENDER: &str = "env_sender_0123456789abcdef0123456789abcdef";
const FILE_CURRENT: &str = "file_current_0123456789abcdef0123456789abcdef";
const FILE_PREVIOUS: &str = "file_previous_0123456789abcdef0123456789abcdef";
const FILE_SENDER: &str = "file_sender_0123456789abcdef0123456789abcdef";
const CHILD_CASE: &str = "HARBOR_SERVICE_AUTH_TEST_CHILD";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = env::temp_dir().join(format!("harbor-service-auth-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, value: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, value).unwrap();
        path
    }

    fn command(&self, operation: &str) -> Command {
        // Only child processes receive synthetic credentials; parallel tests never mutate env.
        let mut command = Command::new(env::current_exe().unwrap());
        command.args([
            "--exact",
            "service_auth::profile_tests::credential_child_case",
            "--nocapture",
        ]);
        for key in [
            CREDENTIALS_DIRECTORY_ENV,
            GATE_TO_BEACON_TOKEN_ENV,
            GATE_TO_BEACON_TOKEN_FILE_ENV,
            GATE_TO_BEACON_TOKEN_PREVIOUS_ENV,
            GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV,
            BEACON_TO_GATE_TOKEN_ENV,
            BEACON_TO_GATE_TOKEN_FILE_ENV,
        ]
        .into_iter()
        .chain(LEGACY_GATE_TO_BEACON_TOKEN_ENVS.iter().copied())
        .chain(LEGACY_BEACON_TO_GATE_TOKEN_ENVS.iter().copied())
        {
            command.env_remove(key);
        }
        command.env(CHILD_CASE, operation);
        command
    }

    fn with_env_fallbacks(&self, operation: &str) -> Command {
        let mut command = self.command(operation);
        command
            .env(GATE_TO_BEACON_TOKEN_ENV, CURRENT)
            .env(GATE_TO_BEACON_TOKEN_PREVIOUS_ENV, PREVIOUS)
            .env(BEACON_TO_GATE_TOKEN_ENV, SENDER);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_child(mut command: Command, expected: &str, previous: Option<&str>) {
    command
        .env("HARBOR_SERVICE_AUTH_TEST_EXPECTED", expected)
        .env("HARBOR_SERVICE_AUTH_TEST_PREVIOUS", previous.unwrap_or(""));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "credential child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
fn credential_child_case() {
    let Ok(operation) = env::var(CHILD_CASE) else {
        return;
    };
    let expected = env::var("HARBOR_SERVICE_AUTH_TEST_EXPECTED").unwrap();
    let result = match operation.as_str() {
        "inbound" => gate_to_beacon_verifier_tokens(),
        "strict" => gate_to_beacon_file_verifier_tokens(),
        "outbound" => beacon_to_gate_sender_token().map(|current| VerifierTokens {
            current,
            previous: None,
        }),
        _ => panic!("unknown credential test operation"),
    };
    if let Some(source) = expected.strip_prefix("error:") {
        let error = result.expect_err("configured credential must fail closed");
        assert!(
            error.contains(source),
            "unexpected credential error: {error}"
        );
        for secret in [
            CURRENT,
            PREVIOUS,
            SENDER,
            FILE_CURRENT,
            FILE_PREVIOUS,
            FILE_SENDER,
        ] {
            assert!(!error.contains(secret));
        }
    } else {
        let verifier = result.unwrap();
        assert_eq!(verifier.current, expected);
        let previous = env::var("HARBOR_SERVICE_AUTH_TEST_PREVIOUS").unwrap();
        assert_eq!(
            verifier.previous.as_deref(),
            (!previous.is_empty()).then_some(previous.as_str())
        );
    }
}

#[test]
fn disabled_verifier_rejects_every_token_including_empty() {
    let verifier = VerifierTokens::disabled();
    assert!(verifier.current.is_empty());
    assert!(verifier.previous.is_none());
    for actual in ["", CURRENT, PREVIOUS, SENDER, "invalid"] {
        assert!(!verifier.matches(actual));
    }
}

#[test]
fn unrelated_systemd_credentials_preserve_primary_env_for_all_slots() {
    let fixture = Fixture::new();
    fixture.write("harborlink-local-token", "unrelated");
    fixture.write("edge-assertion-key", "unrelated");
    for (operation, expected, previous) in [
        ("inbound", CURRENT, Some(PREVIOUS)),
        ("outbound", SENDER, None),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
        assert_child(command, expected, previous);
    }
}

#[test]
fn unrelated_systemd_credentials_preserve_legacy_env_without_previous() {
    let fixture = Fixture::new();
    let mut inbound = fixture.command("inbound");
    inbound
        .env(CREDENTIALS_DIRECTORY_ENV, &fixture.0)
        .env(LEGACY_GATE_TO_BEACON_TOKEN_ENVS[0], CURRENT);
    assert_child(inbound, CURRENT, None);
    for legacy in LEGACY_BEACON_TO_GATE_TOKEN_ENVS {
        let mut outbound = fixture.command("outbound");
        outbound
            .env(CREDENTIALS_DIRECTORY_ENV, &fixture.0)
            .env(legacy, SENDER);
        assert_child(outbound, SENDER, None);
    }
}

#[test]
fn unrelated_systemd_credentials_preserve_valid_explicit_files() {
    let fixture = Fixture::new();
    fixture.write("harborlink-local-token", "unrelated");
    let current = fixture.write("explicit-current", FILE_CURRENT);
    let previous = fixture.write("explicit-previous", FILE_PREVIOUS);
    let sender = fixture.write("explicit-sender", FILE_SENDER);
    for (operation, expected, expected_previous) in [
        ("inbound", FILE_CURRENT, Some(FILE_PREVIOUS)),
        ("outbound", FILE_SENDER, None),
        ("strict", "error:HARBOR_GATE_TO_BEACON_TOKEN_FILE", None),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command
            .env(CREDENTIALS_DIRECTORY_ENV, &fixture.0)
            .env(GATE_TO_BEACON_TOKEN_FILE_ENV, &current)
            .env(GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV, &previous)
            .env(BEACON_TO_GATE_TOKEN_FILE_ENV, &sender);
        assert_child(command, expected, expected_previous);
    }
}

#[test]
fn unrelated_systemd_credentials_do_not_hide_invalid_explicit_files() {
    for (key, operation) in [
        (GATE_TO_BEACON_TOKEN_FILE_ENV, "inbound"),
        (GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV, "inbound"),
        (BEACON_TO_GATE_TOKEN_FILE_ENV, "outbound"),
    ] {
        for invalid in [b"short".as_slice(), &[0xff]] {
            let fixture = Fixture::new();
            let path = fixture.write("invalid-explicit", invalid);
            let mut command = fixture.with_env_fallbacks(operation);
            command
                .env(CREDENTIALS_DIRECTORY_ENV, &fixture.0)
                .env(key, &path);
            assert_child(command, &format!("error:{key}"), None);
        }
    }
}

#[test]
fn explicit_files_without_systemd_directory_override_env() {
    let fixture = Fixture::new();
    let current = fixture.write("current", FILE_CURRENT);
    let previous = fixture.write("previous", FILE_PREVIOUS);
    let sender = fixture.write("sender", FILE_SENDER);
    for (operation, expected, expected_previous) in [
        ("inbound", FILE_CURRENT, Some(FILE_PREVIOUS)),
        ("strict", FILE_CURRENT, Some(FILE_PREVIOUS)),
        ("outbound", FILE_SENDER, None),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command
            .env(GATE_TO_BEACON_TOKEN_FILE_ENV, &current)
            .env(GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV, &previous)
            .env(BEACON_TO_GATE_TOKEN_FILE_ENV, &sender);
        assert_child(command, expected, expected_previous);
    }
}

#[test]
fn systemd_rotation_files_override_explicit_files_and_env() {
    let fixture = Fixture::new();
    fixture.write(GATE_TO_BEACON_CURRENT_CREDENTIAL, FILE_CURRENT);
    fixture.write(GATE_TO_BEACON_PREVIOUS_CREDENTIAL, FILE_PREVIOUS);
    fixture.write(BEACON_TO_GATE_SEND_CREDENTIAL, FILE_SENDER);
    let override_path = fixture.write("explicit-override", CURRENT);
    for (operation, expected, previous) in [
        ("inbound", FILE_CURRENT, Some(FILE_PREVIOUS)),
        ("strict", FILE_CURRENT, Some(FILE_PREVIOUS)),
        ("outbound", FILE_SENDER, None),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
        for key in [
            GATE_TO_BEACON_TOKEN_FILE_ENV,
            GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV,
            BEACON_TO_GATE_TOKEN_FILE_ENV,
        ] {
            command.env(key, &override_path);
        }
        assert_child(command, expected, previous);
    }
}

#[test]
fn empty_systemd_previous_disables_rotation_without_env_fallback() {
    let fixture = Fixture::new();
    fixture.write(GATE_TO_BEACON_CURRENT_CREDENTIAL, FILE_CURRENT);
    fixture.write(GATE_TO_BEACON_PREVIOUS_CREDENTIAL, "\n");
    for operation in ["inbound", "strict"] {
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
        assert_child(command, FILE_CURRENT, None);
    }
}

#[test]
fn explicit_missing_files_fail_even_with_valid_env() {
    let fixture = Fixture::new();
    for (key, operation) in [
        (GATE_TO_BEACON_TOKEN_FILE_ENV, "inbound"),
        (GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV, "inbound"),
        (BEACON_TO_GATE_TOKEN_FILE_ENV, "outbound"),
    ] {
        for with_systemd in [false, true] {
            let mut command = fixture.with_env_fallbacks(operation);
            command.env(key, fixture.0.join("missing"));
            if with_systemd {
                command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
            }
            assert_child(command, &format!("error:{key}"), None);
        }
    }
}

#[test]
fn invalid_explicit_and_systemd_files_never_fall_back_to_env() {
    for (key, name, operation) in [
        (
            GATE_TO_BEACON_TOKEN_FILE_ENV,
            GATE_TO_BEACON_CURRENT_CREDENTIAL,
            "inbound",
        ),
        (
            GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV,
            GATE_TO_BEACON_PREVIOUS_CREDENTIAL,
            "inbound",
        ),
        (
            BEACON_TO_GATE_TOKEN_FILE_ENV,
            BEACON_TO_GATE_SEND_CREDENTIAL,
            "outbound",
        ),
    ] {
        for invalid in [
            b"short".as_slice(),
            b"contains.invalid.character.0123456789abcdef",
            &[0xff],
        ] {
            let fixture = Fixture::new();
            fixture.write(GATE_TO_BEACON_CURRENT_CREDENTIAL, FILE_CURRENT);
            let path = fixture.write(name, invalid);
            for implicit in [false, true] {
                let mut command = fixture.with_env_fallbacks(operation);
                if implicit {
                    command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
                } else {
                    command.env(key, &path);
                }
                assert_child(command, &format!("error:{key}"), None);
            }
        }
    }
}

#[test]
fn empty_required_files_fail_but_explicit_empty_previous_is_allowed() {
    let fixture = Fixture::new();
    let path = fixture.write("empty", "\n");
    for (key, operation) in [
        (GATE_TO_BEACON_TOKEN_FILE_ENV, "inbound"),
        (BEACON_TO_GATE_TOKEN_FILE_ENV, "outbound"),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(key, &path);
        assert_child(command, &format!("error:{key}"), None);
    }
    let mut command = fixture.with_env_fallbacks("inbound");
    command.env(GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV, &path);
    assert_child(command, CURRENT, None);
}

#[test]
fn unreadable_implicit_files_do_not_fall_back_to_env() {
    for (name, key, operation) in [
        (
            GATE_TO_BEACON_CURRENT_CREDENTIAL,
            GATE_TO_BEACON_TOKEN_FILE_ENV,
            "inbound",
        ),
        (
            GATE_TO_BEACON_PREVIOUS_CREDENTIAL,
            GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV,
            "inbound",
        ),
        (
            BEACON_TO_GATE_SEND_CREDENTIAL,
            BEACON_TO_GATE_TOKEN_FILE_ENV,
            "outbound",
        ),
    ] {
        let fixture = Fixture::new();
        if name != GATE_TO_BEACON_CURRENT_CREDENTIAL {
            fixture.write(GATE_TO_BEACON_CURRENT_CREDENTIAL, FILE_CURRENT);
        }
        fs::create_dir(fixture.0.join(name)).unwrap();
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
        assert_child(command, &format!("error:{key}"), None);
    }
}

#[test]
fn strict_file_only_requires_current_and_previous_files_despite_env() {
    let fixture = Fixture::new();
    assert_child(
        fixture.with_env_fallbacks("strict"),
        &format!("error:{GATE_TO_BEACON_TOKEN_FILE_ENV}"),
        None,
    );
    let mut command = fixture.with_env_fallbacks("strict");
    command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
    assert_child(
        command,
        &format!("error:{GATE_TO_BEACON_TOKEN_FILE_ENV}"),
        None,
    );
    fixture.write(GATE_TO_BEACON_CURRENT_CREDENTIAL, FILE_CURRENT);
    let mut command = fixture.with_env_fallbacks("strict");
    command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
    assert_child(
        command,
        &format!("error:{GATE_TO_BEACON_TOKEN_PREVIOUS_FILE_ENV}"),
        None,
    );
}

#[test]
fn missing_current_cannot_be_replaced_by_previous_env() {
    let fixture = Fixture::new();
    let mut command = fixture.command("inbound");
    command.env(GATE_TO_BEACON_TOKEN_PREVIOUS_ENV, PREVIOUS);
    assert_child(command, &format!("error:{GATE_TO_BEACON_TOKEN_ENV}"), None);
    assert_child(
        fixture.command("outbound"),
        &format!("error:{BEACON_TO_GATE_TOKEN_ENV}"),
        None,
    );
}

#[test]
fn invalid_previous_env_is_not_silently_ignored() {
    let fixture = Fixture::new();
    let mut command = fixture.with_env_fallbacks("inbound");
    command
        .env(CREDENTIALS_DIRECTORY_ENV, &fixture.0)
        .env(GATE_TO_BEACON_TOKEN_PREVIOUS_ENV, "short");
    assert_child(
        command,
        &format!("error:{GATE_TO_BEACON_TOKEN_PREVIOUS_ENV}"),
        None,
    );
}

#[test]
fn unrelated_systemd_directory_does_not_make_missing_credentials_optional() {
    let fixture = Fixture::new();
    for (operation, key) in [
        ("inbound", GATE_TO_BEACON_TOKEN_ENV),
        ("outbound", BEACON_TO_GATE_TOKEN_ENV),
    ] {
        let mut command = fixture.command(operation);
        command.env(CREDENTIALS_DIRECTORY_ENV, &fixture.0);
        assert_child(command, &format!("error:{key}"), None);
    }
}

#[test]
fn primary_env_precedes_legacy_and_invalid_primary_fails_closed() {
    let fixture = Fixture::new();
    for (operation, primary, legacy, expected, previous) in [
        (
            "inbound",
            GATE_TO_BEACON_TOKEN_ENV,
            LEGACY_GATE_TO_BEACON_TOKEN_ENVS[0],
            CURRENT,
            Some(PREVIOUS),
        ),
        (
            "outbound",
            BEACON_TO_GATE_TOKEN_ENV,
            LEGACY_BEACON_TO_GATE_TOKEN_ENVS[0],
            SENDER,
            None,
        ),
    ] {
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(legacy, FILE_CURRENT);
        assert_child(command, expected, previous);
        let mut command = fixture.with_env_fallbacks(operation);
        command.env(primary, "short").env(legacy, FILE_CURRENT);
        assert_child(command, &format!("error:{primary}"), None);
    }
}
