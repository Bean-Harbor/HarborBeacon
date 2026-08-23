use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::{net::TcpListener, thread};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("repo root should resolve")
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "harborbeacon-service-auth-{label}-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temp dir");
    path
}

#[cfg(unix)]
fn write_mode_0600(path: &Path, value: &str) {
    fs::write(path, value).expect("write credential");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod credential");
}

#[test]
fn beacon_unit_receives_only_role_scoped_service_credentials() {
    let root = repo_root();
    let unit = fs::read_to_string(root.join("debian/harboros-beacon.service")).unwrap();

    assert!(!unit.contains("EnvironmentFile=-/etc/default/harboros-beacon-gate"));
    assert!(unit.contains("LoadCredential=gate-to-beacon-accept-current:"));
    assert!(unit.contains("LoadCredential=gate-to-beacon-accept-previous:"));
    assert!(unit.contains("LoadCredential=beacon-to-gate-send:"));
    assert!(unit.contains("Requires=harboros-service-auth-recovery.service"));
    assert!(unit
        .contains("After=network.target harborlink.target harboros-service-auth-recovery.service"));
    assert!(!unit.contains("gate-to-beacon.send"));
    assert!(!unit.contains("beacon-to-gate.accept-current"));
    assert!(!unit.contains("HARBOR_TASK_API_BEARER_TOKEN"));
    assert!(!unit.contains("HARBOR_MODEL_API_TOKEN"));
}

#[test]
fn beacon_postinst_does_not_write_cross_domain_credentials() {
    let root = repo_root();
    let postinst = fs::read_to_string(root.join("debian/postinst")).unwrap();

    assert!(!postinst.contains("/etc/default/harboros-beacon-gate"));
    assert!(!postinst.contains("HARBOR_TASK_API_BEARER_TOKEN=$token"));
    assert!(!postinst.contains("HARBORBEACON_WEB_API_TOKEN=$token"));
    assert!(!postinst.contains("IM_AGENT_SERVICE_TOKEN=$token"));
    assert!(!postinst.contains("append_env_if_missing \"$env_file\" \"HARBOR_MODEL_API_TOKEN\""));
    assert!(postinst.contains("chmod 0600 \"$env_file\""));
}

#[test]
fn package_dependency_and_postinst_enforce_gate_before_beacon_start() {
    let root = repo_root();
    let control = fs::read_to_string(root.join("debian/control")).unwrap();
    let postinst = fs::read_to_string(root.join("debian/postinst")).unwrap();
    let preflight = "/usr/lib/harboros-beacon/validate-harborbeacon-service-auth";

    assert!(control.contains("harboros-im-gate, harboros-service-auth-abi (>= 1)"));
    assert!(!control.contains("harboros-im-gate (>= VERSION_PLACEHOLDER)"));
    assert!(postinst.contains(preflight));
    assert!(postinst.find(preflight).unwrap() < postinst.find("systemctl restart").unwrap());
}

#[test]
fn package_includes_independent_model_token_writer() {
    let root = repo_root();
    let postinst = fs::read_to_string(root.join("debian/postinst")).unwrap();
    let workflow = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let k3_builder = fs::read_to_string(root.join("scripts/build_harbornavi_k3_deb.sh")).unwrap();
    let writer = fs::read_to_string(root.join("debian/ensure-harborbeacon-model-token")).unwrap();

    assert!(postinst.contains("/usr/lib/harboros-beacon/ensure-harborbeacon-model-token"));
    assert!(postinst.contains("HARBOR_MODEL_TOKEN_FORBIDDEN_FILES="));
    for credential in [
        "gate-to-beacon.accept-current",
        "gate-to-beacon.accept-previous",
        "beacon-to-gate.send",
    ] {
        assert!(postinst.contains(credential));
    }
    assert!(workflow.contains("ensure-harborbeacon-model-token"));
    assert!(k3_builder.contains("ensure-harborbeacon-model-token"));
    assert!(k3_builder.contains("validate-harborbeacon-service-auth"));
    assert!(k3_builder.contains("harboros-im-gate, harboros-service-auth-abi (>= 1)"));
    assert!(!k3_builder.contains("harboros-im-gate (>= ${debian_version})"));
    assert!(writer.contains("HARBOR_MODEL_API_TOKEN"));
    for forbidden in [
        "HARBOR_TASK_API_BEARER_TOKEN",
        "HARBORBEACON_WEB_API_TOKEN",
        "HARBOR_GATE_TO_BEACON_TOKEN",
        "HARBOR_BEACON_TO_GATE_TOKEN",
        "IM_AGENT_SERVICE_TOKEN",
        "/etc/harboros/service-auth",
        "/etc/default/harboros-beacon-gate",
    ] {
        assert!(!writer.contains(forbidden), "writer references {forbidden}");
    }
}

#[test]
fn runtime_has_no_predictable_model_token_fallback() {
    let root = repo_root();
    let admin_console = fs::read_to_string(root.join("src/runtime/admin_console.rs")).unwrap();
    let service = fs::read_to_string(root.join("src/bin/harborbeacon_service.rs")).unwrap();
    let service_auth = fs::read_to_string(root.join("src/service_auth.rs")).unwrap();
    let standalone = fs::read_to_string(root.join("src/bin/harbor-model-api.rs")).unwrap();
    let production_admin_console = admin_console
        .split("#[cfg(test)]")
        .next()
        .expect("production admin console source");

    assert!(!production_admin_console.contains("harbor-local-model-token"));
    assert!(service.contains("model_api_verifier_token().unwrap_or_else"));
    assert!(service_auth.contains("{MODEL_API_TOKEN_ENV} is not configured"));
    assert!(standalone.contains("ModelApiService::from_env_and_args().unwrap_or_else"));
}

#[test]
fn ci_runs_service_auth_integration_test() {
    let workflow =
        fs::read_to_string(repo_root().join(".github/workflows/contract-pr-check.yml")).unwrap();
    assert!(workflow.contains("cargo test --locked --bin harboros-beacon -- --test-threads=1"));
    assert!(workflow.contains(
        "cargo check --locked --bin harboros-beacon --bin agent-hub-admin-api --bin assistant-task-api --bin harbor-model-api --bin benchmark-local-model-backend"
    ));
    assert!(workflow.contains(
        "cargo test --locked --bin benchmark-local-model-backend spawned_model_api_receives_only_explicit_benchmark_api_key -- --test-threads=1"
    ));
    assert!(
        workflow.contains("cargo test --locked --test service_auth_hardening -- --test-threads=1")
    );
    assert!(workflow.contains("bash tests/test_harbornavi_k3_gate_companion.sh"));
}

#[test]
fn k3_builder_validates_companion_gate_before_build_and_records_evidence() {
    let root = repo_root();
    let builder = fs::read_to_string(root.join("scripts/build_harbornavi_k3_deb.sh")).unwrap();
    let validator =
        fs::read_to_string(root.join("scripts/validate_harbornavi_k3_gate_deb.sh")).unwrap();
    let focused_test =
        fs::read_to_string(root.join("tests/test_harbornavi_k3_gate_companion.sh")).unwrap();

    let required_input = builder
        .find("HARBORNAVI_GATE_DEB")
        .expect("K3 builder must require a companion Gate deb");
    let validation = builder
        .find("validate_harbornavi_k3_gate_deb.sh")
        .expect("K3 builder must validate the companion Gate deb");
    let first_cargo_build = builder
        .find("cargo build --release")
        .expect("K3 builder cargo build");
    assert!(required_input < first_cargo_build);
    assert!(validation < first_cargo_build);
    assert!(builder.contains("-L \"$gate_companion_source\""));
    assert!(builder
        .contains("cp --no-dereference -- \"$gate_companion_source\" \"$gate_companion_deb\""));

    for contract in [
        "-L \"$gate_deb\"",
        "Package",
        "harboros-im-gate",
        "Architecture",
        "riscv64",
        "harboros-service-auth-abi",
        "dpkg-deb --fsys-tarfile",
        "tar --numeric-owner --list --verbose",
        "archive_owner\" == \"0/0",
        "/usr/bin/harboros-im-gate",
        "/etc/systemd/system/harboros-im-gate.service",
        "/etc/systemd/system/harboros-service-auth-recovery.service",
        "/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env",
        "bash -n \"$credential_helper\"",
        "require_safe_executable_mode \"$gate_binary\"",
        "require_safe_executable_mode \"$credential_helper\"",
        "require_safe_unit_mode \"$main_unit\"",
        "require_safe_unit_mode \"$recovery_unit\"",
        "require_effective_unit_dependency \"$main_unit\" Requires",
        "require_effective_unit_dependency \"$main_unit\" After",
        "require_single_service_assignment \"$recovery_unit\" Type oneshot",
        "require_safe_remain_after_exit \"$recovery_unit\"",
        "ensure-harborbeacon-token-env recover",
    ] {
        assert!(
            validator.contains(contract),
            "validator is missing {contract}"
        );
    }

    for evidence in [
        "gate_companion_deb=${gate_companion_basename}",
        "gate_companion_version=${gate_companion_version}",
        "gate_companion_sha256=${gate_companion_sha256}",
        "gate_companion_service_auth_abi=${service_auth_abi}",
        "cp -- \"$gate_companion_deb\" \"$gate_companion_output\"",
    ] {
        assert!(
            builder.contains(evidence),
            "K3 manifest is missing {evidence}"
        );
    }

    for rejection_case in [
        "missing-main-unit",
        "missing-main-requires",
        "missing-main-after",
        "wrong-recovery-type",
        "duplicate-exec-start",
        "unsafe-executable-mode",
        "unsafe-unit-mode",
        "remain-after-exit-yes",
        "non-root-owner",
        "--owner=123",
        "--group=456",
    ] {
        assert!(
            focused_test.contains(rejection_case),
            "focused K3 test is missing {rejection_case}"
        );
    }
}

#[cfg(unix)]
#[test]
fn service_auth_preflight_accepts_rotation_state_and_rejects_invalid_current() {
    let root = repo_root();
    let validator = root.join("debian/validate-harborbeacon-service-auth");
    let auth_dir = temp_dir("preflight");
    fs::set_permissions(&auth_dir, fs::Permissions::from_mode(0o700)).expect("chmod auth dir");
    let gate_to_beacon = "gate_token_0123456789abcdef0123456789abcdef";
    let beacon_to_gate = "beacon_token_0123456789abcdef0123456789abcdef";
    write_mode_0600(
        &auth_dir.join("gate-to-beacon.accept-current"),
        gate_to_beacon,
    );
    write_mode_0600(&auth_dir.join("gate-to-beacon.accept-previous"), "");
    write_mode_0600(&auth_dir.join("beacon-to-gate.send"), beacon_to_gate);

    let valid = Command::new("bash")
        .arg(&validator)
        .env("HARBOR_SERVICE_AUTH_DIR", &auth_dir)
        .output()
        .expect("run credential preflight");
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );

    write_mode_0600(&auth_dir.join("gate-to-beacon.accept-current"), "short");
    let invalid = Command::new("bash")
        .arg(&validator)
        .env("HARBOR_SERVICE_AUTH_DIR", &auth_dir)
        .output()
        .expect("run credential preflight");
    assert!(!invalid.status.success());

    write_mode_0600(
        &auth_dir.join("gate-to-beacon.accept-current"),
        gate_to_beacon,
    );
    write_mode_0600(&auth_dir.join("beacon-to-gate.send"), gate_to_beacon);
    let reused_domain = Command::new("bash")
        .arg(&validator)
        .env("HARBOR_SERVICE_AUTH_DIR", &auth_dir)
        .output()
        .expect("run credential preflight");
    assert!(!reused_domain.status.success());
    fs::remove_dir_all(auth_dir).expect("remove temp dir");
}

#[cfg(unix)]
#[test]
fn model_token_writer_is_idempotent_private_and_atomic() {
    let root = repo_root();
    let writer = root.join("debian/ensure-harborbeacon-model-token");
    let dir = temp_dir("model-token");
    let env_file = dir.join("harboros-beacon");

    let first = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .output()
        .expect("run model token writer");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_bytes = fs::read(&env_file).expect("read generated env");
    let first_text = String::from_utf8_lossy(&first_bytes);
    let token = first_text
        .lines()
        .find_map(|line| line.strip_prefix("HARBOR_MODEL_API_TOKEN="))
        .expect("model token assignment");
    assert!(token.len() >= 32);
    assert!(token
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')));
    assert_eq!(
        fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let second = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .output()
        .expect("rerun model token writer");
    assert!(second.status.success());
    assert_eq!(first_bytes, fs::read(&env_file).unwrap());

    let original = b"OTHER=value\n";
    fs::write(&env_file, original).expect("replace env with pre-migration state");
    let failed = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .env("HARBOR_MODEL_TOKEN_FAILPOINT", "after_rename")
        .output()
        .expect("run model token failpoint");
    assert!(!failed.status.success());
    assert_eq!(fs::read(&env_file).unwrap().as_slice(), original);
    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[cfg(unix)]
#[test]
fn model_token_writer_rotates_service_token_reuse_without_secret_output() {
    let root = repo_root();
    let writer = root.join("debian/ensure-harborbeacon-model-token");
    let dir = temp_dir("model-token-domain-isolation");
    let env_file = dir.join("harboros-beacon");
    let current = dir.join("gate-to-beacon.accept-current");
    let previous = dir.join("gate-to-beacon.accept-previous");
    let sender = dir.join("beacon-to-gate.send");
    let reused = "legacy_shared_token_0123456789abcdef0123456789abcdef";
    let other = "directional_token_0123456789abcdef0123456789abcdef";

    write_mode_0600(
        &env_file,
        &format!("OTHER=value\n  HARBOR_MODEL_API_TOKEN = \"{reused}\"  \n"),
    );
    write_mode_0600(&current, &format!("{other}\n"));
    write_mode_0600(&previous, &format!("{reused}\n"));
    write_mode_0600(&sender, &format!("{reused}\n"));
    let forbidden_files = format!(
        "{}:{}:{}",
        current.display(),
        previous.display(),
        sender.display()
    );

    let first = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .env("HARBOR_MODEL_TOKEN_FORBIDDEN_FILES", &forbidden_files)
        .output()
        .expect("rotate reused model token");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_bytes = fs::read(&env_file).expect("read rotated model token env");
    let first_text = String::from_utf8_lossy(&first_bytes);
    let rotated = first_text
        .lines()
        .find_map(|line| line.strip_prefix("HARBOR_MODEL_API_TOKEN="))
        .expect("rotated model token assignment");
    assert_ne!(rotated, reused);
    assert_ne!(rotated, other);
    assert!(!first_text.contains(reused));
    assert_eq!(first_text.matches("HARBOR_MODEL_API_TOKEN=").count(), 1);
    for output in [&first.stdout, &first.stderr] {
        let output = String::from_utf8_lossy(output);
        assert!(!output.contains(reused));
        assert!(!output.contains(rotated));
    }

    let second = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .env("HARBOR_MODEL_TOKEN_FORBIDDEN_FILES", &forbidden_files)
        .output()
        .expect("rerun isolated model token writer");
    assert!(second.status.success());
    assert_eq!(first_bytes, fs::read(&env_file).unwrap());

    fs::set_permissions(&sender, fs::Permissions::from_mode(0o644))
        .expect("weaken sender credential mode");
    let tampered = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .env("HARBOR_MODEL_TOKEN_FORBIDDEN_FILES", &forbidden_files)
        .output()
        .expect("reject weak isolation credential");
    assert!(!tampered.status.success());
    assert_eq!(first_bytes, fs::read(&env_file).unwrap());
    let tampered_log = format!(
        "{}{}",
        String::from_utf8_lossy(&tampered.stdout),
        String::from_utf8_lossy(&tampered.stderr)
    );
    assert!(!tampered_log.contains(reused));
    assert!(!tampered_log.contains(rotated));

    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[cfg(unix)]
#[test]
fn model_token_writer_parses_safe_environment_syntax_and_rejects_invalid_known_key() {
    let root = repo_root();
    let writer = root.join("debian/ensure-harborbeacon-model-token");
    let dir = temp_dir("model-token-parser");
    let token = "quoted_model_token_0123456789abcdef0123456789abcdef";

    for (name, assignment) in [
        (
            "double-quoted",
            format!("  HARBOR_MODEL_API_TOKEN = \"{token}\"  \n"),
        ),
        (
            "single-quoted",
            format!("\tHARBOR_MODEL_API_TOKEN\t=\t'{token}'\t\n"),
        ),
        ("unquoted", format!("HARBOR_MODEL_API_TOKEN={token}\n")),
    ] {
        let env_file = dir.join(name);
        let original = format!("OTHER=value\n{assignment}");
        write_mode_0600(&env_file, &original);
        let result = Command::new("bash")
            .arg(&writer)
            .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
            .output()
            .expect("run model token parser");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_to_string(&env_file).unwrap(), original);
    }

    for (name, assignment) in [
        ("short-value", " HARBOR_MODEL_API_TOKEN = \"short\" \n"),
        (
            "malformed-assignment",
            "HARBOR_MODEL_API_TOKEN value-without-equals\n",
        ),
        (
            "unsupported-export",
            "export HARBOR_MODEL_API_TOKEN=valid_but_unsupported_0123456789abcdef\n",
        ),
    ] {
        let env_file = dir.join(name);
        let original = format!("OTHER=value\n{assignment}");
        write_mode_0600(&env_file, &original);
        let result = Command::new("bash")
            .arg(&writer)
            .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
            .output()
            .expect("run invalid model token parser");
        assert!(!result.status.success());
        assert_eq!(fs::read_to_string(&env_file).unwrap(), original);
    }

    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[cfg(unix)]
#[test]
fn model_token_writer_rejects_symlink_target() {
    let root = repo_root();
    let writer = root.join("debian/ensure-harborbeacon-model-token");
    let dir = temp_dir("model-token-symlink");
    let protected = dir.join("protected");
    let env_file = dir.join("harboros-beacon");
    fs::write(&protected, "do-not-change\n").expect("write protected file");
    symlink(&protected, &env_file).expect("create symlink");

    let result = Command::new("bash")
        .arg(&writer)
        .env("HARBORBEACON_RUNTIME_ENV_FILE", &env_file)
        .output()
        .expect("run model token writer");
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(&protected).unwrap(), "do-not-change\n");
    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[test]
fn legacy_aliases_are_confined_to_their_original_direction() {
    let root = repo_root();
    let source = fs::read_to_string(root.join("src/service_auth.rs")).unwrap();
    let gate_to_beacon = source
        .split("const LEGACY_GATE_TO_BEACON_TOKEN_ENVS")
        .nth(1)
        .and_then(|tail| tail.split("const LEGACY_BEACON_TO_GATE_TOKEN_ENVS").next())
        .expect("gate-to-beacon alias block");
    let beacon_to_gate = source
        .split("const LEGACY_BEACON_TO_GATE_TOKEN_ENVS")
        .nth(1)
        .and_then(|tail| tail.split("#[derive").next())
        .expect("beacon-to-gate alias block");

    assert!(gate_to_beacon.contains("HARBORBEACON_WEB_API_TOKEN"));
    assert!(!gate_to_beacon.contains("HARBORGATE_BEARER_TOKEN"));
    assert!(!gate_to_beacon.contains("IM_AGENT_SERVICE_TOKEN"));
    assert!(beacon_to_gate.contains("HARBORGATE_BEARER_TOKEN"));
    assert!(beacon_to_gate.contains("HARBOR_IM_GATEWAY_BEARER_TOKEN"));
    assert!(beacon_to_gate.contains("IM_AGENT_SERVICE_TOKEN"));
    assert!(!beacon_to_gate.contains("HARBORBEACON_WEB_API_TOKEN"));
}

#[cfg(unix)]
struct ChildGuard(Child);

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
fn reserve_loopback_addr() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    listener.local_addr().expect("reserved address")
}

#[cfg(unix)]
fn spawn_packaged_beacon(
    dir: &Path,
    current_path: &Path,
    previous_path: &Path,
    sender_path: &Path,
    bind: std::net::SocketAddr,
) -> ChildGuard {
    let binary = env!("CARGO_BIN_EXE_harboros-beacon");
    let mut command = Command::new(binary);
    command
        .current_dir(dir)
        .args([
            "--bind",
            &bind.to_string(),
            "--admin-state",
            dir.join("admin-console.json").to_str().unwrap(),
            "--device-registry",
            dir.join("device-registry.json").to_str().unwrap(),
            "--conversations",
            dir.join("conversations.json").to_str().unwrap(),
            "--harbor-assistant-dist",
            dir.join("webui").to_str().unwrap(),
            "--public-origin",
            "http://127.0.0.1",
        ])
        .env_remove("HARBOR_TASK_API_BEARER_TOKEN")
        .env_remove("HARBORBEACON_SERVICE_TOKEN")
        .env_remove("HARBOR_GATE_TO_BEACON_TOKEN")
        .env_remove("HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS")
        .env_remove("HARBORBEACON_WEB_API_TOKEN")
        .env("HARBOR_GATE_TO_BEACON_TOKEN_FILE", current_path)
        .env("HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS_FILE", previous_path)
        .env("HARBOR_BEACON_TO_GATE_TOKEN_FILE", sender_path)
        .env(
            "HARBOR_MODEL_API_TOKEN",
            "model_token_0123456789abcdef0123456789abcdef",
        )
        .env("HARBOR_MODEL_API_BACKEND", "semantic_router")
        .env("HARBORBEACON_SOUTHBOUND_MODE", "harborlink")
        .env("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false")
        .env("HARBOR_K3_CAT_RECORDING_VALIDATION_MODE", "off")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    ChildGuard(command.spawn().expect("spawn harboros-beacon"))
}

#[cfg(unix)]
fn wait_for_beacon(guard: &mut ChildGuard, base_url: &str) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
        .expect("health client");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = guard.0.try_wait().expect("inspect beacon process") {
            let stderr = guard
                .0
                .stderr
                .take()
                .and_then(|mut stderr| {
                    use std::io::Read;
                    let mut output = String::new();
                    stderr.read_to_string(&mut output).ok().map(|_| output)
                })
                .unwrap_or_default();
            panic!("harboros-beacon exited before readiness ({status}): {stderr}");
        }
        if client
            .get(format!("{base_url}/healthz"))
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "harboros-beacon readiness timeout"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn wait_for_exit(guard: &mut ChildGuard, timeout: Duration) -> (std::process::ExitStatus, String) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = guard.0.try_wait().expect("inspect child process") {
            let mut stderr_text = String::new();
            if let Some(mut stderr) = guard.0.stderr.take() {
                use std::io::Read;
                stderr
                    .read_to_string(&mut stderr_text)
                    .expect("read child stderr");
            }
            return (status, stderr_text);
        }
        assert!(Instant::now() < deadline, "child process did not exit");
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn turn_status(base_url: &str, token: Option<&str>) -> (reqwest::StatusCode, serde_json::Value) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("turn client");
    let mut request = client
        .post(format!("{base_url}/api/web/turns"))
        .header("X-Contract-Version", "2.0")
        .header("Content-Type", "application/json")
        .body(r#"{"trace_id":"runtime-auth-test"}"#);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().expect("turn response");
    let status = response.status();
    let payload = response.json().expect("turn response json");
    (status, payload)
}

#[cfg(unix)]
#[test]
fn packaged_beacon_turn_ingress_uses_current_and_previous_credential_files() {
    let dir = temp_dir("runtime-turn-auth");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod test dir");
    let current = "gate_current_0123456789abcdef0123456789abcdef";
    let previous = "gate_previous_0123456789abcdef0123456789abcdef";
    let wrong = "wrong_domain_0123456789abcdef0123456789abcdef";
    let current_path = dir.join("gate-to-beacon.accept-current");
    let previous_path = dir.join("gate-to-beacon.accept-previous");
    let sender_path = dir.join("beacon-to-gate.send");
    write_mode_0600(&current_path, current);
    write_mode_0600(&previous_path, previous);
    write_mode_0600(
        &sender_path,
        "beacon_sender_0123456789abcdef0123456789abcdef",
    );

    let bind = reserve_loopback_addr();
    let base_url = format!("http://{bind}");
    let mut beacon = spawn_packaged_beacon(&dir, &current_path, &previous_path, &sender_path, bind);
    wait_for_beacon(&mut beacon, &base_url);

    for token in [current, previous] {
        let (status, payload) = turn_status(&base_url, Some(token));
        assert_eq!(status, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
    }
    for token in [Some(wrong), None] {
        let (status, payload) = turn_status(&base_url, token);
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(payload["error"]["code"], "SERVICE_AUTH_FAILED");
    }

    drop(beacon);
    fs::remove_dir_all(dir).expect("remove runtime auth test dir");
}

#[cfg(unix)]
#[test]
fn packaged_beacon_fails_closed_when_current_credential_file_is_missing() {
    let dir = temp_dir("runtime-turn-auth-missing-current");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod test dir");
    let missing_current = dir.join("missing-gate-to-beacon.accept-current");
    let previous_path = dir.join("gate-to-beacon.accept-previous");
    let sender_path = dir.join("beacon-to-gate.send");
    write_mode_0600(&previous_path, "");
    write_mode_0600(
        &sender_path,
        "beacon_sender_0123456789abcdef0123456789abcdef",
    );

    let mut beacon = spawn_packaged_beacon(
        &dir,
        &missing_current,
        &previous_path,
        &sender_path,
        reserve_loopback_addr(),
    );
    let (status, stderr) = wait_for_exit(&mut beacon, Duration::from_secs(5));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("HARBOR_GATE_TO_BEACON_TOKEN_FILE"));
    assert!(!stderr.contains("beacon_sender_0123456789abcdef0123456789abcdef"));

    drop(beacon);
    fs::remove_dir_all(dir).expect("remove missing-current test dir");
}

#[cfg(unix)]
#[test]
fn standalone_model_api_fails_closed_without_model_token() {
    let dir = temp_dir("standalone-model-auth");
    let binary = env!("CARGO_BIN_EXE_harbor-model-api");
    let mut model_api = ChildGuard(
        Command::new(binary)
            .current_dir(&dir)
            .args([
                "--bind",
                &reserve_loopback_addr().to_string(),
                "--backend",
                "semantic_router",
            ])
            .env_remove("HARBOR_MODEL_API_TOKEN")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn standalone model API"),
    );

    let (status, stderr) = wait_for_exit(&mut model_api, Duration::from_secs(5));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("HARBOR_MODEL_API_TOKEN is not configured"));

    drop(model_api);
    fs::remove_dir_all(dir).expect("remove standalone model auth dir");
}
