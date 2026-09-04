use super::*;

#[test]
fn package_detection_control_path_is_distinct_and_strict() {
    assert_eq!(
        parse_package_detection_control_camera_id(
            "/api/cameras/camera.252/package-detection/control"
        )
        .as_deref(),
        Some("camera.252")
    );
    assert!(parse_package_detection_control_camera_id(
        "/api/cameras/camera.252/package-detection/control/extra"
    )
    .is_none());
    assert!(parse_package_detection_control_camera_id(
        "/api/cameras/camera.252/cat-detection/control"
    )
    .is_none());
}

#[test]
fn package_runtime_zone_match_rejects_missing_or_stale_worker_contract() {
    let zone = PackageDeliveryZone {
        left: 0.1,
        top: 0.2,
        right: 0.8,
        bottom: 0.9,
    };
    let mut runtime = sample_running_detection_job("package-zone", "camera.252", false, None);
    runtime.projection.target_labels = vec!["package".to_string()];

    assert!(package_worker_observability_zone_matches(
        &runtime.projection,
        zone
    ));
    runtime.projection.latest_result = Some(json!({
        "frame_observability_zone": zone,
    }));
    assert!(package_worker_observability_zone_matches(
        &runtime.projection,
        zone
    ));
    runtime.projection.latest_result = Some(json!({
        "frame_observability_zone": {
            "left": 0.0,
            "top": 0.0,
            "right": 1.0,
            "bottom": 1.0,
        },
    }));
    assert!(!package_worker_observability_zone_matches(
        &runtime.projection,
        zone
    ));
    runtime.projection.latest_result = Some(json!({"ok": true}));
    assert!(!package_worker_observability_zone_matches(
        &runtime.projection,
        zone
    ));
}

#[test]
fn persisted_detector_controls_reject_both_enable_directions() {
    let cat_enabled = CatDetectionControlPolicy::new("camera.252", true, "sub", 1)
        .expect("cat policy");
    let cat_disabled = CatDetectionControlPolicy::new("camera.252", false, "sub", 2)
        .expect("cat policy");
    let package_enabled = PackageDetectionControlPolicy::new("camera.252", true, "sub", 3)
        .expect("package policy");
    let package_disabled = PackageDetectionControlPolicy::new("camera.252", false, "sub", 4)
        .expect("package policy");

    assert!(validate_detector_control_enable_conflict(
        "package",
        Some(&cat_enabled),
        Some(&package_disabled),
    )
    .is_err());
    assert!(validate_detector_control_enable_conflict(
        "cat",
        Some(&cat_disabled),
        Some(&package_enabled),
    )
    .is_err());
    assert!(validate_detector_control_enable_conflict(
        "package",
        Some(&cat_disabled),
        Some(&package_disabled),
    )
    .is_ok());
}

#[test]
fn detector_controls_reject_peer_cleanup_and_create_attempts() {
    let mut cat_draining =
        CatDetectionControlPolicy::new("camera.252", false, "sub", 1).expect("cat policy");
    cat_draining
        .set_pending_detection_lease_ids(["cat-lease".to_string()])
        .expect("cat pending lease");
    let mut package_draining =
        PackageDetectionControlPolicy::new("camera.252", false, "sub", 2)
            .expect("package policy");
    package_draining
        .set_detection_lease_create_attempt(
            Some("package-attempt".to_string()),
            Some("sub".to_string()),
        )
        .expect("package create attempt");

    assert!(validate_detector_control_enable_conflict(
        "package",
        Some(&cat_draining),
        None,
    )
    .is_err());
    assert!(validate_detector_control_enable_conflict(
        "cat",
        None,
        Some(&package_draining),
    )
    .is_err());
}

#[test]
fn package_control_defaults_to_stopped_and_disable_is_idempotent() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("detection control environment lock");
    let (_control_path, _guards) =
        install_cat_detection_control_test_environment("package-default-stopped");
    let (api, paths) = build_test_admin_api_with_harborlink(
        "package-default-stopped",
        HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
        &["camera.252"],
    );

    let initial = api
        .package_detection_control_response("camera.252")
        .expect("default response");
    assert!(!initial.explicit);
    assert!(!initial.desired_enabled);
    assert_eq!(initial.effective_status, "stopped");

    let disabled = api
        .apply_package_detection_control("camera.252", false, "sub")
        .expect("idempotent disable");
    assert!(disabled.explicit);
    assert!(!disabled.desired_enabled);
    assert_eq!(disabled.effective_status, "stopped");
    cleanup_test_paths(&paths);
}

#[test]
fn cat_and_package_control_puts_reject_the_enabled_peer() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("detection control environment lock");
    let (_control_path, _guards) =
        install_cat_detection_control_test_environment("package-bidirectional-conflict");
    let (api, paths) = build_test_admin_api_with_harborlink(
        "package-bidirectional-conflict",
        HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", true, "sub", 10)
            .expect("cat policy"),
    )
    .expect("persist cat policy");

    let package_error = api
        .apply_package_detection_control("camera.252", true, "sub")
        .expect_err("package enable must conflict");
    assert_eq!(package_error.0, StatusCode(409));

    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", false, "sub", 11)
            .expect("cat policy"),
    )
    .expect("persist cat policy");
    api.persist_package_detection_policy(
        PackageDetectionControlPolicy::new("camera.252", true, "sub", 12)
            .expect("package policy"),
    )
    .expect("persist package policy");

    let cat_error = api
        .apply_cat_detection_control("camera.252", true, "sub")
        .expect_err("cat enable must conflict");
    assert_eq!(cat_error.0, StatusCode(409));
    cleanup_test_paths(&paths);
}

#[test]
fn failed_package_start_keeps_desired_enabled_and_stable_attempt() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("detection control environment lock");
    let (_control_path, _guards) =
        install_cat_detection_control_test_environment("package-start-retry");
    let (api, paths) = build_test_admin_api_with_harborlink(
        "package-start-retry",
        HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", false, "sub", 20)
            .expect("cat disabled policy"),
    )
    .expect("persist cat disabled policy");

    let response = api
        .apply_package_detection_control("camera.252", true, "sub")
        .expect("failed start remains a valid desired-state write");
    assert!(response.desired_enabled);
    assert_eq!(response.effective_status, "failed");
    let policy = api
        .package_detection_explicit_policy("camera.252")
        .expect("package policy")
        .expect("explicit package policy");
    assert!(policy.desired_enabled);
    assert!(policy.detection_lease_create_attempt_id.is_some());
    cleanup_test_paths(&paths);
}

#[test]
fn package_control_routes_require_gate_principal_policy() {
    for method in [Method::Get, Method::Put] {
        assert!(is_gate_principal_endpoint(
            &method,
            "/api/cameras/camera.252/package-detection/control"
        ));
    }
}
