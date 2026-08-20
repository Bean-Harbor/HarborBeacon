use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("repo root should resolve")
}

#[test]
fn beacon_unit_receives_only_role_scoped_service_credentials() {
    let root = repo_root();
    let unit = fs::read_to_string(root.join("debian/harboros-beacon.service")).unwrap();

    assert!(!unit.contains("EnvironmentFile=-/etc/default/harboros-beacon-gate"));
    assert!(unit.contains("LoadCredential=gate-to-beacon-accept-current:"));
    assert!(unit.contains("LoadCredential=gate-to-beacon-accept-previous:"));
    assert!(unit.contains("LoadCredential=beacon-to-gate-send:"));
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
