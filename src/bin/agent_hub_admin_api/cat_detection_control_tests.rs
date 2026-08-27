use super::*;

#[derive(Clone, Copy, Debug)]
enum SlowAutoRecordingOperation {
    Status,
    Start,
    Renew,
}

fn auto_recording_sample(sequence: u64, now: u64) -> Value {
    json!({
        "target_label": "cat",
        "sequence": sequence,
        "frame_epoch_ms": now,
        "detection_count": 1,
        "consecutive_present_frames": 1,
        "consecutive_absent_frames": 0,
        "present_since_epoch_ms": now,
        "absent_since_epoch_ms": 0,
        "detections": [{"label": "cat", "confidence": 0.95}]
    })
}

fn auto_recording_test_config() -> CatAutoRecordingConfig {
    CatAutoRecordingConfig {
        start_consecutive_frames: 1,
        start_duration_ms: 0,
        stop_consecutive_frames: 3,
        stop_duration_ms: 2_000,
    }
}

#[test]
fn controlled_profile_switch_restores_old_profile_only_after_definite_new_failure() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("controlled-profile-rollback-success");
    let (_output_root, _worker_env) =
        install_sleeping_detection_worker("controlled-profile-rollback-success");
    let old_job_id = "controlled-profile-old-sub";
    let old_lease_id = format!("detect-{old_job_id}");
    let restored_lease_id = "detect-controlled-restored-sub";
    let steps = vec![
        DetectionLeaseServerStep {
            method: "DELETE",
            path: format!("/v1/cameras/camera.252/detection-leases/{old_lease_id}"),
            request_profile: None,
            status: "200 OK",
            response: detection_lease_response("camera.252", &old_lease_id, "stopped", "sub"),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("main".to_string()),
            status: "400 Bad Request",
            response: json!({"error": {
                "code": "INVALID_STREAM_PROFILE",
                "message": "main profile is unavailable",
                "retryable": false,
                "dependency": "harborlink"
            }}),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                restored_lease_id,
                "running",
                "sub",
            ),
        },
    ];
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(steps);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "controlled-profile-rollback-success",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let api = api.with_cat_detection_retry_scheduler_config_for_test(
        super::super::CatDetectionRetrySchedulerConfig {
            worker_count: 1,
            capacity: 8,
            initial_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(30),
        },
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", true, "sub", 91)
            .expect("initial sub policy"),
    )
    .expect("persist initial policy");
    api.detection_jobs.lock().expect("detection jobs").insert(
        old_job_id.to_string(),
        sample_running_detection_job_for_profile(
            old_job_id,
            "camera.252",
            "sub",
            false,
            Some(spawn_sleeping_detection_child()),
        ),
    );

    let response = api
        .apply_cat_detection_control("camera.252", true, "main")
        .expect("profile switch response");
    assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
    api.cancel_cat_detection_retry_workers_for_test();
    server.join().expect("HarborLink server");

    assert!(response.desired_enabled);
    assert_eq!(response.desired_stream_profile, "main");
    assert_eq!(response.effective_status, "failed");
    assert_eq!(response.effective_stream_profile.as_deref(), Some("sub"));
    assert!(response
        .message
        .as_deref()
        .is_some_and(|message| message.contains("restored") && message.contains("sub")));
    let policy = api
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert_eq!(policy.stream_profile, "main");
    assert!(policy.detection_lease_create_attempt_id.is_none());
    assert!(policy
        .rollback_detection_lease_create_attempt_id
        .is_none());
    let jobs = api.detection_jobs.lock().expect("detection jobs");
    let running = jobs
        .values()
        .filter(|runtime| runtime.projection.status == "running")
        .collect::<Vec<_>>();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].projection.stream_profile, "sub");
    assert_eq!(running[0].projection.lease_id, restored_lease_id);
    drop(jobs);
    assert_eq!(requests.lock().expect("requests").len(), 3);
    cleanup_detection_children(&api);
    cleanup_test_paths(&paths);
}

#[test]
fn controlled_profile_switch_does_not_rollback_an_uncertain_new_attempt() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("controlled-profile-no-uncertain-rollback");
    let old_job_id = "controlled-profile-uncertain-old-sub";
    let old_lease_id = format!("detect-{old_job_id}");
    let steps = vec![
        DetectionLeaseServerStep {
            method: "DELETE",
            path: format!("/v1/cameras/camera.252/detection-leases/{old_lease_id}"),
            request_profile: None,
            status: "200 OK",
            response: detection_lease_response("camera.252", &old_lease_id, "stopped", "sub"),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("main".to_string()),
            status: "409 Conflict",
            response: json!({"error": {
                "code": "REQUEST_IN_PROGRESS",
                "message": "request result is unresolved",
                "retryable": true,
                "dependency": "harborlink"
            }}),
        },
    ];
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(steps);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "controlled-profile-no-uncertain-rollback",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", true, "sub", 92)
            .expect("initial sub policy"),
    )
    .expect("persist initial policy");
    api.detection_jobs.lock().expect("detection jobs").insert(
        old_job_id.to_string(),
        sample_running_detection_job_for_profile(
            old_job_id,
            "camera.252",
            "sub",
            false,
            Some(spawn_sleeping_detection_child()),
        ),
    );

    let response = api
        .apply_cat_detection_control("camera.252", true, "main")
        .expect("profile switch response");
    server.join().expect("HarborLink server");

    assert_eq!(response.effective_status, "failed");
    assert!(response.effective_stream_profile.is_none());
    assert_eq!(requests.lock().expect("requests").len(), 2);
    let policy = api
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert_eq!(
        policy.detection_lease_create_attempt_stream_profile(),
        Some("main")
    );
    assert!(policy
        .rollback_detection_lease_create_attempt_id
        .is_none());
    assert!(!api
        .detection_jobs
        .lock()
        .expect("detection jobs")
        .values()
        .any(|runtime| runtime.projection.status == "running"));
    cleanup_detection_children(&api);
    cleanup_test_paths(&paths);
}

#[test]
fn uncertain_profile_rollback_replays_stable_scope_after_restart() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("controlled-profile-rollback-restart");
    let (_output_root, _worker_env) =
        install_sleeping_detection_worker("controlled-profile-rollback-restart");
    let old_job_id = "controlled-profile-restart-old-sub";
    let old_lease_id = format!("detect-{old_job_id}");
    let restored_lease_id = "detect-controlled-restart-restored-sub";
    let steps = vec![
        DetectionLeaseServerStep {
            method: "DELETE",
            path: format!("/v1/cameras/camera.252/detection-leases/{old_lease_id}"),
            request_profile: None,
            status: "200 OK",
            response: detection_lease_response("camera.252", &old_lease_id, "stopped", "sub"),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("main".to_string()),
            status: "400 Bad Request",
            response: json!({"error": {
                "code": "INVALID_STREAM_PROFILE",
                "message": "main profile is unavailable",
                "retryable": false,
                "dependency": "harborlink"
            }}),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "503 Service Unavailable",
            response: json!({"error": {
                "code": "HARBORLINK_UNAVAILABLE",
                "message": "rollback result is unresolved",
                "retryable": true,
                "dependency": "harborlink"
            }}),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                restored_lease_id,
                "running",
                "sub",
            ),
        },
    ];
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(steps);
    let (api, first_paths) = build_test_admin_api_with_harborlink(
        "controlled-profile-rollback-restart-first",
        HarborLinkMediaClient::new(harborlink_url.clone()).expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", true, "sub", 93)
            .expect("initial sub policy"),
    )
    .expect("persist initial policy");
    api.detection_jobs.lock().expect("detection jobs").insert(
        old_job_id.to_string(),
        sample_running_detection_job_for_profile(
            old_job_id,
            "camera.252",
            "sub",
            false,
            Some(spawn_sleeping_detection_child()),
        ),
    );

    let first = api
        .apply_cat_detection_control("camera.252", true, "main")
        .expect("first profile switch response");
    assert_eq!(first.effective_status, "failed");
    assert!(first.effective_stream_profile.is_none());
    let unresolved = api
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert_eq!(unresolved.stream_profile, "main");
    assert_eq!(unresolved.rollback_stream_profile.as_deref(), Some("sub"));
    assert!(unresolved
        .rollback_detection_lease_create_attempt_id
        .is_some());
    cleanup_detection_children(&api);
    drop(api);

    let (restarted, restarted_paths) = build_test_admin_api_with_harborlink(
        "controlled-profile-rollback-restart-second",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let revision = restarted
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy")
        .updated_at_epoch_ms;
    let retry = restarted
        .coordinate_cat_detection_control("camera.252", revision)
        .expect_err("restored old profile keeps desired retry pending");
    server.join().expect("HarborLink server");

    assert!(retry.contains("restored") && retry.contains("sub"), "{retry}");
    let resolved = restarted
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert!(resolved
        .rollback_detection_lease_create_attempt_id
        .is_none());
    let requests = requests.lock().expect("requests");
    let rollback_ids = [&requests[2], &requests[3]]
        .into_iter()
        .map(|request| test_request_header(request, "X-Request-Id").expect("request ID"))
        .collect::<Vec<_>>();
    assert_eq!(rollback_ids[0], rollback_ids[1]);
    drop(requests);
    let jobs = restarted.detection_jobs.lock().expect("detection jobs");
    let running = jobs
        .values()
        .filter(|runtime| runtime.projection.status == "running")
        .collect::<Vec<_>>();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].projection.stream_profile, "sub");
    assert_eq!(running[0].projection.lease_id, restored_lease_id);
    drop(jobs);
    cleanup_detection_children(&restarted);
    cleanup_test_paths(&first_paths);
    cleanup_test_paths(&restarted_paths);
}

#[test]
fn confirmed_missing_profile_rollback_clears_marker_and_remains_failed_stopped() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("controlled-profile-rollback-missing");
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(vec![
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "400 Bad Request",
            response: json!({"error": {
                "code": "INVALID_STREAM_PROFILE",
                "message": "rollback profile is unavailable",
                "retryable": false,
                "dependency": "harborlink"
            }}),
        },
    ]);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "controlled-profile-rollback-missing",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let mut policy = CatDetectionControlPolicy::new("camera.252", true, "main", 94)
        .expect("desired main policy");
    policy
        .set_rollback_detection_lease_create_attempt(
            Some("rollback-confirmed-missing".to_string()),
            Some("sub".to_string()),
        )
        .expect("rollback marker");
    api.persist_cat_detection_policy(policy)
        .expect("persist rollback marker");

    let error = api
        .coordinate_cat_detection_control("camera.252", 94)
        .expect_err("confirmed missing rollback keeps desired reconciliation pending");
    server.join().expect("HarborLink server");

    assert!(error.contains("confirmed not created"), "{error}");
    assert_eq!(requests.lock().expect("requests").len(), 1);
    let policy = api
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert!(policy
        .rollback_detection_lease_create_attempt_id
        .is_none());
    let response = api
        .cat_detection_control_response("camera.252")
        .expect("control response");
    assert_eq!(response.effective_status, "failed");
    assert!(response.effective_stream_profile.is_none());
    cleanup_test_paths(&paths);
}

#[test]
fn put_false_persists_before_unavailable_rollback_and_replay_only_cleans_lease() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (control_path, _control_env) =
        install_cat_detection_control_test_environment("put-false-rollback-cleanup-only");
    let rollback_lease_id = "detect-put-false-rollback";
    let steps = vec![
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "503 Service Unavailable",
            response: json!({"error": {
                "code": "HARBORLINK_UNAVAILABLE",
                "message": "rollback result is unresolved",
                "retryable": true,
                "dependency": "harborlink"
            }}),
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                rollback_lease_id,
                "running",
                "sub",
            ),
        },
        DetectionLeaseServerStep {
            method: "DELETE",
            path: format!(
                "/v1/cameras/camera.252/detection-leases/{rollback_lease_id}"
            ),
            request_profile: None,
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                rollback_lease_id,
                "stopped",
                "sub",
            ),
        },
    ];
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(steps);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "put-false-rollback-cleanup-only",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let api = api.with_cat_detection_retry_scheduler_config_for_test(
        super::super::CatDetectionRetrySchedulerConfig {
            worker_count: 1,
            capacity: 8,
            initial_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(30),
        },
    );
    let mut policy = CatDetectionControlPolicy::new("camera.252", true, "main", 95)
        .expect("enabled main policy");
    policy
        .set_rollback_detection_lease_create_attempt(
            Some("put-false-unresolved-rollback".to_string()),
            Some("sub".to_string()),
        )
        .expect("rollback marker");
    api.persist_cat_detection_policy(policy)
        .expect("persist rollback marker");

    let disabled = api
        .apply_cat_detection_control("camera.252", false, "sub")
        .expect("PUT false persists before rollback resolution");
    assert!(!disabled.desired_enabled);
    assert_eq!(disabled.effective_status, "failed");
    let persisted = CatDetectionControlStore::try_new(control_path.clone())
        .expect("control store")
        .load()
        .expect("load disabled policy")["camera.252"]
        .clone();
    assert!(!persisted.desired_enabled);
    assert_eq!(persisted.stream_profile, "sub");
    assert!(persisted
        .rollback_detection_lease_create_attempt_id
        .is_some());
    assert!(!api
        .detection_jobs
        .lock()
        .expect("detection jobs")
        .values()
        .any(|runtime| runtime.child.is_some()));

    api.cancel_cat_detection_retry_workers_for_test();
    assert_eq!(
        api.coordinate_cat_detection_control("camera.252", persisted.updated_at_epoch_ms)
            .expect("cleanup-only replay converges"),
        CatDetectionControlCoordination::Converged
    );
    server.join().expect("HarborLink server");

    let observed = requests.lock().expect("requests");
    assert_eq!(observed.len(), 3);
    assert!(observed[0].starts_with("POST "));
    assert!(observed[1].starts_with("POST "));
    assert!(observed[2].starts_with(&format!(
        "DELETE /v1/cameras/camera.252/detection-leases/{rollback_lease_id} "
    )));
    drop(observed);
    let final_policy = CatDetectionControlStore::try_new(control_path)
        .expect("control store")
        .load()
        .expect("load final policy")["camera.252"]
        .clone();
    assert!(!final_policy.desired_enabled);
    assert!(final_policy
        .rollback_detection_lease_create_attempt_id
        .is_none());
    assert!(final_policy.pending_detection_lease_ids.is_empty());
    assert!(!api
        .detection_jobs
        .lock()
        .expect("detection jobs")
        .values()
        .any(|runtime| runtime.projection.status == "running" || runtime.child.is_some()));
    cleanup_detection_children(&api);
    cleanup_test_paths(&paths);
}

fn assert_rollback_spawn_failure_transfers_attempt_before_cleanup(
    prefix: &str,
    delete_status: &'static str,
) {
    let (_control_path, _control_env) = install_cat_detection_control_test_environment(prefix);
    let (_output_root, _worker_env) = install_sleeping_detection_worker(prefix);
    let rollback_lease_id = format!("detect-{prefix}-rollback");
    let desired_lease_id = format!("detect-{prefix}-desired");
    let delete_response = if delete_status == "404 Not Found" {
        json!({"error": {
            "code": "DETECTION_LEASE_NOT_FOUND",
            "message": "already removed",
            "retryable": false,
            "dependency": "harborlink"
        }})
    } else {
        detection_lease_response("camera.252", &rollback_lease_id, "stopped", "sub")
    };
    let steps = vec![
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("sub".to_string()),
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                &rollback_lease_id,
                "running",
                "sub",
            ),
        },
        DetectionLeaseServerStep {
            method: "DELETE",
            path: format!(
                "/v1/cameras/camera.252/detection-leases/{rollback_lease_id}"
            ),
            request_profile: None,
            status: delete_status,
            response: delete_response,
        },
        DetectionLeaseServerStep {
            method: "POST",
            path: "/v1/cameras/camera.252/detection-leases".to_string(),
            request_profile: Some("main".to_string()),
            status: "200 OK",
            response: detection_lease_response(
                "camera.252",
                &desired_lease_id,
                "running",
                "main",
            ),
        },
    ];
    let (harborlink_url, requests, server) = spawn_detection_lease_sequence_server(steps);
    let (api, paths) = build_test_admin_api_with_harborlink(
        prefix,
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let revision = 96;
    let mut policy = CatDetectionControlPolicy::new("camera.252", true, "main", revision)
        .expect("desired main policy");
    policy
        .set_rollback_detection_lease_create_attempt(
            Some(format!("{prefix}-rollback-attempt")),
            Some("sub".to_string()),
        )
        .expect("rollback marker");
    api.persist_cat_detection_policy(policy)
        .expect("persist rollback marker");
    let bad_python = EnvGuard::set(
        "HARBOR_K3_YOLO_PYTHON",
        "harborbeacon-definitely-missing-python",
    );

    let first_error = api
        .coordinate_cat_detection_control("camera.252", revision)
        .expect_err("rollback worker spawn fails after lease creation");
    assert!(first_error.contains("failed to start detection worker"));
    let after_cleanup = api
        .cat_detection_explicit_policy("camera.252")
        .expect("policy")
        .expect("explicit policy");
    assert!(after_cleanup
        .rollback_detection_lease_create_attempt_id
        .is_none());
    assert!(after_cleanup.pending_detection_lease_ids.is_empty());

    drop(bad_python);
    assert_eq!(
        api.coordinate_cat_detection_control("camera.252", revision)
            .expect("desired profile continues after rollback cleanup"),
        CatDetectionControlCoordination::Converged
    );
    server.join().expect("HarborLink server");

    let observed = requests.lock().expect("requests");
    assert_eq!(observed.len(), 3);
    assert!(observed[0].starts_with("POST "));
    assert!(observed[1].starts_with(&format!(
        "DELETE /v1/cameras/camera.252/detection-leases/{rollback_lease_id} "
    )));
    assert!(observed[2].starts_with("POST "));
    let profiles = [&observed[0], &observed[2]]
        .into_iter()
        .map(|request| {
            serde_json::from_str::<Value>(
                request
                    .split_once("\r\n\r\n")
                    .expect("request body separator")
                    .1,
            )
            .expect("request JSON body")["stream_profile"]
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(profiles, vec![json!("sub"), json!("main")]);
    drop(observed);
    let running = api
        .detection_jobs
        .lock()
        .expect("detection jobs")
        .values()
        .filter(|runtime| runtime.projection.status == "running")
        .map(|runtime| {
            (
                runtime.projection.stream_profile.clone(),
                runtime.projection.lease_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(running, vec![("main".to_string(), desired_lease_id)]);
    cleanup_detection_children(&api);
    cleanup_test_paths(&paths);
}

#[test]
fn rollback_spawn_failure_delete_200_clears_marker_before_desired_retry() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    assert_rollback_spawn_failure_transfers_attempt_before_cleanup(
        "rollback-spawn-delete-200",
        "200 OK",
    );
}

#[test]
fn rollback_spawn_failure_delete_404_clears_marker_before_desired_retry() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    assert_rollback_spawn_failure_transfers_attempt_before_cleanup(
        "rollback-spawn-delete-404",
        "404 Not Found",
    );
}

fn spawn_slow_auto_recording_server(
    slow_operation: SlowAutoRecordingOperation,
    expected_request_override: Option<usize>,
    trigger_epoch_ms: u64,
) -> (
    String,
    std::sync::mpsc::Receiver<()>,
    Arc<(Mutex<bool>, Condvar)>,
    Arc<AtomicUsize>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking HarborLink listener");
    let address = listener.local_addr().expect("HarborLink address");
    let expected_requests = expected_request_override.unwrap_or(match slow_operation {
        SlowAutoRecordingOperation::Renew => 3,
        SlowAutoRecordingOperation::Status | SlowAutoRecordingOperation::Start => 4,
    });
    let (slow_started_sender, slow_started_receiver) = sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let server_release = release.clone();
    let request_count = Arc::new(AtomicUsize::new(0));
    let server_request_count = request_count.clone();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut handlers = Vec::new();
        while server_request_count.load(Ordering::SeqCst) < expected_requests {
            assert!(Instant::now() < deadline, "timed out waiting for HarborLink requests");
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("HarborLink accept failed: {error}"),
            };
            server_request_count.fetch_add(1, Ordering::SeqCst);
            let handler_release = server_release.clone();
            let handler_slow_started = slow_started_sender.clone();
            handlers.push(thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .expect("HarborLink request timeout");
                let mut request_bytes = Vec::new();
                let mut expected_length = None;
                loop {
                    let mut buffer = [0_u8; 4_096];
                    let read = stream.read(&mut buffer).expect("read HarborLink request");
                    if read == 0 {
                        break;
                    }
                    request_bytes.extend_from_slice(&buffer[..read]);
                    if expected_length.is_none() {
                        if let Some(header_end) = request_bytes
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                        {
                            let headers = String::from_utf8_lossy(&request_bytes[..header_end]);
                            let content_length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            expected_length = Some(header_end + 4 + content_length);
                        }
                    }
                    if expected_length.is_some_and(|length| request_bytes.len() >= length) {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request_bytes).to_string();
                let is_camera_a = request.contains("/cameras/camera.a/");
                let is_status = request.starts_with("GET ")
                    && request.contains("/event-recordings/current ");
                let is_start = request.starts_with("POST ")
                    && request.contains("/event-recordings/current ");
                let is_renew = request.starts_with("POST ") && request.contains("/renew ");
                let is_slow = is_camera_a
                    && match slow_operation {
                        SlowAutoRecordingOperation::Status => is_status,
                        SlowAutoRecordingOperation::Start => is_start,
                        SlowAutoRecordingOperation::Renew => is_renew,
                    };
                if is_slow {
                    let _ = handler_slow_started.try_send(());
                    let (released, changed) = &*handler_release;
                    let mut released = released.lock().expect("slow request release");
                    while !*released {
                        released = changed.wait(released).expect("slow request release");
                    }
                }

                let (status, body) = if is_status {
                    ("404 Not Found", json!({"error": "not_found"}).to_string())
                } else {
                    let camera_id = if is_camera_a { "camera.a" } else { "camera.b" };
                    let request_body = request
                        .split_once("\r\n\r\n")
                        .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
                        .unwrap_or_else(|| json!({}));
                    let event_id = request_body["event_id"]
                        .as_str()
                        .unwrap_or(if is_camera_a { "cat-activity-a" } else { "cat-activity-b" });
                    let lease_id = if is_renew {
                        "event-recording-a".to_string()
                    } else {
                        format!("event-recording-{}", camera_id.replace('.', "-"))
                    };
                    let lease = HarborLinkEventRecordingLease {
                        camera_id: camera_id.to_string(),
                        lease_id,
                        event_id: event_id.to_string(),
                        owner: "harborbeacon".to_string(),
                        status: "running".to_string(),
                        stream_profile: "sub".to_string(),
                        labels: vec!["cat".to_string()],
                        started_at: "2026-08-11T00:00:00Z".to_string(),
                        updated_at: "2026-08-11T00:01:00Z".to_string(),
                        expires_at: "2026-08-11T00:06:00Z".to_string(),
                        pre_roll_seconds: 3,
                        trigger_epoch_ms,
                        artifacts: Vec::new(),
                    };
                    (
                        "200 OK",
                        serde_json::to_string(&lease).expect("serialize event recording lease"),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write HarborLink response");
            }));
        }
        for handler in handlers {
            handler.join().expect("HarborLink request handler");
        }
    });
    (
        format!("http://{address}"),
        slow_started_receiver,
        release,
        request_count,
        server,
    )
}

fn release_slow_auto_recording_request(release: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, changed) = &**release;
    *released.lock().expect("slow request release") = true;
    changed.notify_all();
}

fn seed_active_auto_recording(api: &AdminApi, camera_id: &str, lease_id: &str) {
    let state = CatRecordingReconciliationState {
        camera_id: camera_id.to_string(),
        phase: CatRecordingReconciliationPhase::Active,
        created_at_epoch_ms: cat_auto_recording_epoch_ms(),
        last_sequence: Some(0),
        event_id: Some(format!("cat-activity-{camera_id}")),
        lease_id: Some(lease_id.to_string()),
        stream_profile: Some("sub".to_string()),
        last_renewed_epoch_ms: 0,
        ..Default::default()
    };
    api.cat_recording_reconciliation_store
        .upsert(state.clone())
        .expect("persist active recording");
    api.cat_auto_recording
        .lock()
        .expect("auto-recording state")
        .insert(camera_id.to_string(), state);
}

#[test]
fn slow_auto_recording_harborlink_io_does_not_block_another_camera() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat auto-recording env lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("cross-camera-auto-recording");
    let _auto_enabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "true");

    for slow_operation in [
        SlowAutoRecordingOperation::Status,
        SlowAutoRecordingOperation::Start,
        SlowAutoRecordingOperation::Renew,
    ] {
        let sample_epoch_ms = cat_auto_recording_epoch_ms() as u64;
        let (base_url, slow_started, release, _, server) =
            spawn_slow_auto_recording_server(slow_operation, None, sample_epoch_ms);
        let (api, paths) = build_test_admin_api_with_harborlink(
            &format!("cross-camera-auto-recording-{slow_operation:?}"),
            HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
            &["camera.a", "camera.b"],
        );
        if matches!(slow_operation, SlowAutoRecordingOperation::Renew) {
            seed_active_auto_recording(&api, "camera.a", "event-recording-a");
        }
        let camera_a_api = api.clone();
        let camera_a_sample = auto_recording_sample(1, sample_epoch_ms);
        let camera_a = thread::spawn(move || {
            camera_a_api.process_cat_detection_result(
                "camera.a",
                "sub",
                Some(&camera_a_sample),
                auto_recording_test_config(),
            )
        });
        slow_started
            .recv_timeout(Duration::from_secs(1))
            .expect("camera A slow HarborLink request started");

        let camera_b_api = api.clone();
        let camera_b_sample = auto_recording_sample(1, sample_epoch_ms);
        let (camera_b_sender, camera_b_receiver) = sync_channel(1);
        let camera_b = thread::spawn(move || {
            let result = camera_b_api.process_cat_detection_result(
                "camera.b",
                "sub",
                Some(&camera_b_sample),
                auto_recording_test_config(),
            );
            camera_b_sender.send(result).expect("camera B result");
        });
        let camera_b_result = camera_b_receiver.recv_timeout(Duration::from_secs(2));
        release_slow_auto_recording_request(&release);
        camera_a
            .join()
            .expect("camera A worker")
            .expect("camera A processing");
        camera_b.join().expect("camera B worker");
        server.join().expect("HarborLink server");

        camera_b_result
            .expect("camera B was blocked by camera A auto-recording I/O")
            .expect("camera B processing");
        cleanup_test_paths(&paths);
    }
}

#[test]
fn same_camera_auto_recording_processing_remains_serialized() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat auto-recording env lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("same-camera-auto-recording");
    let _auto_enabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "true");
    let sample_epoch_ms = cat_auto_recording_epoch_ms() as u64;
    let (base_url, slow_started, release, request_count, server) =
        spawn_slow_auto_recording_server(
            SlowAutoRecordingOperation::Status,
            Some(2),
            sample_epoch_ms,
        );
    let (api, paths) = build_test_admin_api_with_harborlink(
        "same-camera-auto-recording",
        HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
        &["camera.a"],
    );
    let first_api = api.clone();
    let first_sample = auto_recording_sample(1, sample_epoch_ms);
    let first = thread::spawn(move || {
        first_api.process_cat_detection_result(
            "camera.a",
            "sub",
            Some(&first_sample),
            auto_recording_test_config(),
        )
    });
    slow_started
        .recv_timeout(Duration::from_secs(1))
        .expect("first camera A request started");
    let second_api = api.clone();
    let second_sample = auto_recording_sample(1, sample_epoch_ms);
    let (second_sender, second_receiver) = sync_channel(1);
    let second = thread::spawn(move || {
        let result = second_api.process_cat_detection_result(
            "camera.a",
            "sub",
            Some(&second_sample),
            auto_recording_test_config(),
        );
        second_sender.send(result).expect("second result");
    });
    assert!(matches!(
        second_receiver.recv_timeout(Duration::from_millis(150)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    release_slow_auto_recording_request(&release);
    first
        .join()
        .expect("first camera A worker")
        .expect("first camera A processing");
    second_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("second camera A completed after release")
        .expect("second camera A processing");
    second.join().expect("second camera A worker");
    server.join().expect("HarborLink server");
    cleanup_test_paths(&paths);
}

#[test]
fn late_auto_recording_start_result_does_not_overwrite_newer_state() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat auto-recording env lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("late-auto-recording-result");
    let _auto_enabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "true");
    let sample_epoch_ms = cat_auto_recording_epoch_ms() as u64;
    let (base_url, slow_started, release, _, server) = spawn_slow_auto_recording_server(
        SlowAutoRecordingOperation::Start,
        Some(2),
        sample_epoch_ms,
    );
    let (api, paths) = build_test_admin_api_with_harborlink(
        "late-auto-recording-result",
        HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
        &["camera.a"],
    );
    let request_api = api.clone();
    let sample = auto_recording_sample(1, sample_epoch_ms);
    let request = thread::spawn(move || {
        request_api.process_cat_detection_result(
            "camera.a",
            "sub",
            Some(&sample),
            auto_recording_test_config(),
        )
    });
    slow_started
        .recv_timeout(Duration::from_secs(1))
        .expect("slow start request started");

    let newer_state = CatRecordingReconciliationState {
        camera_id: "camera.a".to_string(),
        phase: CatRecordingReconciliationPhase::Active,
        created_at_epoch_ms: cat_auto_recording_epoch_ms(),
        last_sequence: Some(999),
        event_id: Some("cat-activity-newer".to_string()),
        lease_id: Some("event-recording-newer".to_string()),
        stream_profile: Some("sub".to_string()),
        last_renewed_epoch_ms: cat_auto_recording_epoch_ms(),
        ..Default::default()
    };
    let supersede_api = api.clone();
    let supersede_state = newer_state.clone();
    let (superseded_sender, superseded_receiver) = sync_channel(1);
    let supersede = thread::spawn(move || {
        supersede_api
            .cat_recording_reconciliation_store
            .upsert(supersede_state.clone())
            .expect("persist newer state");
        supersede_api
            .cat_auto_recording
            .lock()
            .expect("auto-recording state")
            .insert("camera.a".to_string(), supersede_state);
        superseded_sender.send(()).expect("newer state installed");
    });
    let superseded = superseded_receiver.recv_timeout(Duration::from_secs(2));
    release_slow_auto_recording_request(&release);
    let request_result = request.join().expect("auto-recording request");
    supersede.join().expect("state supersede worker");
    server.join().expect("HarborLink server");

    superseded.expect("slow HarborLink call retained global auto-recording mutex");
    assert!(request_result.is_err(), "late result must be rejected");
    assert_eq!(
        api.cat_auto_recording
            .lock()
            .expect("auto-recording state")
            .get("camera.a"),
        Some(&newer_state)
    );
    cleanup_test_paths(&paths);
}

fn spawn_completed_detection_child() -> Child {
    #[cfg(windows)]
    let mut child = Command::new("cmd.exe")
        .args(["/C", "exit", "0"])
        .spawn()
        .expect("completed detection child");
    #[cfg(not(windows))]
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("completed detection child");
    let deadline = Instant::now() + Duration::from_secs(1);
    while child
        .try_wait()
        .expect("inspect completed detection child")
        .is_none()
    {
        assert!(Instant::now() < deadline, "detection child did not exit");
        thread::sleep(Duration::from_millis(2));
    }
    child
}

fn spawn_detection_monitor_cleanup_server(
    statuses: Vec<u16>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
    let address = listener.local_addr().expect("HarborLink address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let server = thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().expect("lease cleanup request");
            let request = read_test_http_request(&mut stream);
            assert!(request.starts_with("DELETE /v1/cameras/"));
            server_requests.lock().expect("requests").push(request);
            let (status_line, code, retryable) = match status {
                404 => ("404 Not Found", "DETECTION_LEASE_NOT_FOUND", false),
                500 => ("500 Internal Server Error", "CLEANUP_FAILED", true),
                _ => panic!("unsupported cleanup status"),
            };
            let body = json!({
                "error": {
                    "code": code,
                    "message": "cleanup result",
                    "retryable": retryable,
                    "dependency": "harborlink"
                }
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("lease cleanup response");
        }
    });
    (format!("http://{address}"), requests, server)
}

fn spawn_timed_detection_monitor_cleanup_server(
    statuses: Vec<u16>,
) -> (String, Arc<Mutex<Vec<Instant>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
    let address = listener.local_addr().expect("HarborLink address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let server = thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().expect("lease cleanup request");
            let request = read_test_http_request(&mut stream);
            assert!(request.starts_with(
                "DELETE /v1/cameras/camera.252/detection-leases/detect-control-owned-retry "
            ));
            server_requests
                .lock()
                .expect("requests")
                .push(Instant::now());
            let (status_line, code, retryable) = match status {
                404 => ("404 Not Found", "DETECTION_LEASE_NOT_FOUND", false),
                500 => ("500 Internal Server Error", "CLEANUP_FAILED", true),
                _ => panic!("unsupported cleanup status"),
            };
            let body = json!({
                "error": {
                    "code": code,
                    "message": "cleanup result",
                    "retryable": retryable,
                    "dependency": "harborlink"
                }
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("lease cleanup response");
        }
    });
    (format!("http://{address}"), requests, server)
}

fn spawn_blocking_detection_monitor_cleanup_server(
) -> (
    String,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
    let address = listener.local_addr().expect("HarborLink address");
    let (started_sender, started_receiver) = sync_channel(1);
    let (release_sender, release_receiver) = sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("lease cleanup request");
        let request = read_test_http_request(&mut stream);
        assert!(request.starts_with(
            "DELETE /v1/cameras/camera.252/detection-leases/detect-blocked-cleanup "
        ));
        started_sender.send(()).expect("cleanup started");
        release_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("release cleanup response");
        let body = json!({
            "error": {
                "code": "DETECTION_LEASE_NOT_FOUND",
                "message": "already deleted",
                "retryable": false,
                "dependency": "harborlink"
            }
        })
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            )
            .expect("lease cleanup response");
    });
    (
        format!("http://{address}"),
        started_receiver,
        release_sender,
        server,
    )
}

fn spawn_public_detection_create_then_cleanup_server(
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
    let address = listener.local_addr().expect("HarborLink address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let server = thread::spawn(move || {
        let (mut create, _) = listener.accept().expect("detection create request");
        let request = read_test_http_request(&mut create);
        assert!(request.starts_with("POST /v1/cameras/camera.252/detection-leases "));
        server_requests.lock().expect("requests").push(request);
        let body = detection_lease_response(
            "camera.252",
            "detect-public-natural-exit",
            "running",
            "sub",
        )
        .to_string();
        create
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            )
            .expect("detection create response");

        let (mut cleanup, _) = listener.accept().expect("lease cleanup request");
        let request = read_test_http_request(&mut cleanup);
        assert!(request.starts_with(
            "DELETE /v1/cameras/camera.252/detection-leases/detect-public-natural-exit "
        ));
        server_requests.lock().expect("requests").push(request);
        let body = json!({
            "error": {
                "code": "DETECTION_LEASE_NOT_FOUND",
                "message": "already deleted",
                "retryable": false,
                "dependency": "harborlink"
            }
        })
        .to_string();
        cleanup
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            )
            .expect("lease cleanup response");
    });
    (format!("http://{address}"), requests, server)
}

fn install_exiting_detection_worker(prefix: &str) -> (PathBuf, Vec<EnvGuard>) {
    let output_root = std::env::temp_dir().join(format!(
        "harborbeacon-exiting-worker-{prefix}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&output_root).expect("create worker output root");
    let worker = output_root.join("exiting_worker.py");
    fs::write(&worker, "raise SystemExit(0)\n").expect("write exiting worker");
    let python = if cfg!(windows) { "python" } else { "python3" };
    let guards = vec![
        EnvGuard::set("HARBOR_K3_YOLO_PYTHON", python),
        EnvGuard::set(
            "HARBOR_K3_YOLO_WORKER",
            worker.to_str().expect("UTF-8 worker path"),
        ),
        EnvGuard::set(
            "HARBOR_K3_YOLO_OUTPUT_ROOT",
            output_root.to_str().expect("UTF-8 output root"),
        ),
        EnvGuard::set("HARBOR_K3_YOLO_PROVIDER", "cpu"),
    ];
    (output_root, guards)
}

fn insert_public_job_with_completed_child(api: &AdminApi, job_id: &str, lease_id: &str) {
    insert_public_job_with_completed_child_for_camera(
        api,
        job_id,
        "camera.252",
        lease_id,
    );
}

fn insert_public_job_with_completed_child_for_camera(
    api: &AdminApi,
    job_id: &str,
    camera_id: &str,
    lease_id: &str,
) {
    let mut runtime = sample_running_detection_job(job_id, camera_id, false, None);
    runtime.projection.lease_id = lease_id.to_string();
    runtime.child = Some(spawn_completed_detection_child());
    runtime.detection_lease_cleanup_confirmed = false;
    api.detection_jobs
        .lock()
        .expect("detection jobs")
        .insert(job_id.to_string(), runtime);
}

#[test]
fn public_detection_child_exit_cleans_lease_without_explicit_policy() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("public-child-monitor-cleanup");
    let (_output_root, _worker_env) =
        install_exiting_detection_worker("public-child-monitor-cleanup");
    let (harborlink_url, requests, server) =
        spawn_public_detection_create_then_cleanup_server();
    let (api, paths) = build_test_admin_api_with_harborlink(
        "public-child-monitor-cleanup",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let job_id = api
        .start_detection_job(controlled_detection_config(), false)
        .expect("public detection create")
        .projection
        .job_id;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        api.monitor_detection_jobs_once()
            .expect("monitor terminal detection job");
        let cleanup_confirmed = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .get(&job_id)
            .is_some_and(|runtime| runtime.detection_lease_cleanup_confirmed);
        if cleanup_confirmed {
            break;
        }
        assert!(Instant::now() < deadline, "public child cleanup did not converge");
        thread::sleep(Duration::from_millis(10));
    }
    server.join().expect("HarborLink server");

    let jobs = api.detection_jobs.lock().expect("detection jobs");
    let runtime = jobs.get(&job_id).expect("completed public job");
    assert_eq!(runtime.projection.status, "completed");
    assert!(runtime.detection_lease_cleanup_confirmed);
    assert_eq!(requests.lock().expect("requests").len(), 2);
    cleanup_test_paths(&paths);
}

#[test]
fn public_detection_child_cleanup_retries_500_then_converges_on_404() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("public-child-monitor-retry");
    let (harborlink_url, requests, server) =
        spawn_detection_monitor_cleanup_server(vec![500, 404]);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "public-child-monitor-retry",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    insert_public_job_with_completed_child(
        &api,
        "public-child-retry",
        "detect-public-retry",
    );

    let first_attempt_at = Instant::now();
    api.monitor_detection_jobs_once_at(first_attempt_at)
        .expect("first monitor pass");
    {
        let jobs = api.detection_jobs.lock().expect("detection jobs");
        let runtime = jobs.get("public-child-retry").expect("public job");
        assert!(!runtime.detection_lease_cleanup_confirmed);
        assert!(runtime
            .projection
            .message
            .as_deref()
            .is_some_and(|message| message.contains("cleanup was incomplete")));
    }
    api.monitor_detection_jobs_once_at(first_attempt_at + Duration::from_secs(1))
        .expect("second monitor pass");
    server.join().expect("HarborLink server");

    let jobs = api.detection_jobs.lock().expect("detection jobs");
    let runtime = jobs.get("public-child-retry").expect("public job");
    assert!(runtime.detection_lease_cleanup_confirmed);
    assert!(!runtime
        .projection
        .message
        .as_deref()
        .is_some_and(|message| message.contains("cleanup was incomplete")));
    assert_eq!(requests.lock().expect("requests").len(), 2);
    cleanup_test_paths(&paths);
}

#[test]
fn terminal_cleanup_persists_pending_before_blocking_delete_and_clears_on_success() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (control_path, _control_env) =
        install_cat_detection_control_test_environment("terminal-pending-before-delete");
    let (harborlink_url, started, release, server) =
        spawn_blocking_detection_monitor_cleanup_server();
    let (api, paths) = build_test_admin_api_with_harborlink(
        "terminal-pending-before-delete",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", false, "sub", 31)
            .expect("disabled policy"),
    )
    .expect("persist disabled policy");
    insert_public_job_with_completed_child(
        &api,
        "blocked-terminal-job",
        "detect-blocked-cleanup",
    );
    let monitor_api = api.clone();
    let monitor = thread::spawn(move || monitor_api.monitor_detection_jobs_once());

    started
        .recv_timeout(Duration::from_secs(2))
        .expect("DELETE reached HarborLink");
    let during_delete = CatDetectionControlStore::try_new(control_path.clone())
        .expect("control store")
        .load()
        .expect("load control store");
    assert_eq!(
        during_delete["camera.252"].pending_detection_lease_ids,
        vec!["detect-blocked-cleanup".to_string()]
    );

    release.send(()).expect("release DELETE");
    monitor
        .join()
        .expect("monitor thread")
        .expect("monitor pass");
    server.join().expect("HarborLink server");
    let after_success = CatDetectionControlStore::try_new(control_path)
        .expect("control store")
        .load()
        .expect("load control store");
    assert!(after_success["camera.252"]
        .pending_detection_lease_ids
        .is_empty());
    cleanup_test_paths(&paths);
}

#[test]
fn package_terminal_cleanup_is_not_owned_by_cat_control() {
    let mut cat = sample_running_detection_job("terminal-cat", "camera.252", false, None);
    cat.projection.status = "completed".to_string();
    let mut package =
        sample_running_detection_job("terminal-package", "camera.252", false, None);
    package.projection.status = "completed".to_string();
    package.projection.target_labels = vec!["package".to_string()];
    let jobs = HashMap::from([
        (cat.projection.job_id.clone(), cat),
        (package.projection.job_id.clone(), package),
    ]);

    let intents = terminal_detection_lease_cleanup_intents(&jobs);
    let cat = intents
        .iter()
        .find(|intent| intent.job_id == "terminal-cat")
        .expect("cat cleanup intent");
    let package = intents
        .iter()
        .find(|intent| intent.job_id == "terminal-package")
        .expect("package cleanup intent");

    assert!(cat_control_owns_detection_cleanup(cat));
    assert!(!cat_control_owns_detection_cleanup(package));
}

#[test]
fn failed_terminal_cleanup_is_recovered_from_pending_after_admin_restart() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (control_path, _control_env) =
        install_cat_detection_control_test_environment("terminal-pending-restart");
    let (first_url, _, first_server) = spawn_detection_monitor_cleanup_server(vec![500]);
    let (api, first_paths) = build_test_admin_api_with_harborlink(
        "terminal-pending-restart-first",
        HarborLinkMediaClient::new(first_url).expect("HarborLink client"),
        &["camera.252"],
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", false, "sub", 32)
            .expect("disabled policy"),
    )
    .expect("persist disabled policy");
    insert_public_job_with_completed_child(
        &api,
        "restart-terminal-job",
        "detect-restart-cleanup",
    );

    api.monitor_detection_jobs_once().expect("failed cleanup pass");
    first_server.join().expect("first HarborLink server");
    let failed = CatDetectionControlStore::try_new(control_path.clone())
        .expect("control store")
        .load()
        .expect("load failed cleanup policy");
    assert_eq!(
        failed["camera.252"].pending_detection_lease_ids,
        vec!["detect-restart-cleanup".to_string()]
    );
    drop(api);

    let (recovery_url, _, recovery_server) =
        spawn_detection_monitor_cleanup_server(vec![404]);
    let (restarted, restarted_paths) = build_test_admin_api_with_harborlink(
        "terminal-pending-restart-second",
        HarborLinkMediaClient::new(recovery_url).expect("HarborLink client"),
        &["camera.252"],
    );
    assert_eq!(
        restarted
            .coordinate_cat_detection_control("camera.252", 32)
            .expect("restart cleanup reconciliation"),
        CatDetectionControlCoordination::Converged
    );
    recovery_server.join().expect("recovery HarborLink server");
    let recovered = CatDetectionControlStore::try_new(control_path)
        .expect("control store")
        .load()
        .expect("load recovered policy");
    assert!(recovered["camera.252"]
        .pending_detection_lease_ids
        .is_empty());
    cleanup_test_paths(&first_paths);
    cleanup_test_paths(&restarted_paths);
}

#[test]
fn explicit_terminal_cleanup_has_one_retry_owner_across_monitor_passes() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (control_path, _control_env) =
        install_cat_detection_control_test_environment("terminal-control-owned-retry");
    let (harborlink_url, requests, server) =
        spawn_timed_detection_monitor_cleanup_server(vec![500, 500, 500, 500, 404]);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "terminal-control-owned-retry",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252"],
    );
    let api = api.with_cat_detection_retry_scheduler_config_for_test(
        super::super::CatDetectionRetrySchedulerConfig {
            worker_count: 1,
            capacity: 8,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(400),
        },
    );
    api.persist_cat_detection_policy(
        CatDetectionControlPolicy::new("camera.252", false, "sub", 81)
            .expect("disabled policy"),
    )
    .expect("persist disabled policy");
    let blocker_started = Arc::new((Mutex::new(false), Condvar::new()));
    let blocker_release = Arc::new((Mutex::new(false), Condvar::new()));
    let task_started = blocker_started.clone();
    let task_release = blocker_release.clone();
    api.cat_detection_retry_scheduler
        .as_ref()
        .expect("test retry scheduler")
        .enqueue(
            "scheduler-blocker",
            1,
            Duration::ZERO,
            Arc::new(move |_| {
                let (started, started_changed) = &*task_started;
                *started.lock().expect("blocker started") = true;
                started_changed.notify_all();
                let (released, release_changed) = &*task_release;
                let mut released = released.lock().expect("blocker release");
                while !*released {
                    released = release_changed.wait(released).expect("blocker release");
                }
                harborbeacon_local_agent::runtime::cat_detection_retry_scheduler::CatDetectionRetryOutcome::Complete
            }),
        )
        .expect("enqueue scheduler blocker");
    {
        let (started, changed) = &*blocker_started;
        let started = started.lock().expect("blocker started");
        let (started, timeout) = changed
            .wait_timeout_while(started, Duration::from_secs(2), |started| !*started)
            .expect("wait for scheduler blocker");
        assert!(*started && !timeout.timed_out(), "scheduler blocker did not start");
    }
    insert_public_job_with_completed_child(
        &api,
        "control-owned-terminal-job",
        "detect-control-owned-retry",
    );

    let fake_now = Instant::now();
    api.monitor_detection_jobs_once_at(fake_now)
        .expect("initial monitor cleanup");
    assert_eq!(requests.lock().expect("requests").len(), 1);
    assert!(api
        .detection_lease_cleanup_retries
        .lock()
        .expect("monitor retry state")
        .is_empty());
    assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
    for seconds in [1, 2, 4, 8, 16, 32, 64] {
        api.monitor_detection_jobs_once_at(fake_now + Duration::from_secs(seconds))
            .expect("interleaved monitor pass");
        api.cat_detection_control_response("camera.252")
            .expect("GET projection coalesces the same control revision");
        assert!(api.cat_detection_retry_queue_len_for_test() <= 1);
    }
    assert_eq!(
        requests.lock().expect("requests").len(),
        1,
        "monitor became a second retry owner"
    );
    assert!(api
        .detection_lease_cleanup_retries
        .lock()
        .expect("monitor retry state")
        .is_empty());

    api.idle_cat_detection_control_retry("camera.252");
    {
        let (released, changed) = &*blocker_release;
        *released.lock().expect("blocker release") = true;
        changed.notify_all();
    }
    let idle_deadline = Instant::now() + Duration::from_secs(2);
    while api
        .cat_detection_retry_scheduler_probe_for_test()
        .active_jobs()
        != 0
        && Instant::now() < idle_deadline
    {
        thread::yield_now();
    }
    assert_eq!(
        api.cat_detection_retry_scheduler_probe_for_test()
            .active_jobs(),
        0,
        "scheduler blocker did not exit"
    );
    api.enqueue_cat_detection_control_retry("camera.252", 81, Duration::from_millis(50));
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut fake_seconds = 128;
    while requests.lock().expect("requests").len() < 5 && Instant::now() < deadline {
        api.monitor_detection_jobs_once_at(fake_now + Duration::from_secs(fake_seconds))
            .expect("monitor remains observation-only during control retry");
        fake_seconds += 1;
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(requests.lock().expect("requests").len(), 5);
    server.join().expect("HarborLink server");
    let convergence_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < convergence_deadline {
        let converged = api
            .cat_detection_explicit_policy("camera.252")
            .expect("control policy")
            .is_some_and(|policy| policy.pending_detection_lease_ids.is_empty())
            && !api.cat_detection_retry_contains_camera_for_test("camera.252");
        if converged {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let requests = requests.lock().expect("requests");
    let gaps = requests
        .windows(2)
        .map(|window| window[1].duration_since(window[0]))
        .collect::<Vec<_>>();
    assert!(gaps[0] >= Duration::from_millis(40));
    assert!(gaps[1] >= Duration::from_millis(80));
    assert!(gaps[2] >= Duration::from_millis(170));
    assert!(gaps[3] >= Duration::from_millis(350));
    drop(requests);
    let policy = CatDetectionControlStore::try_new(control_path)
        .expect("control store")
        .load()
        .expect("load converged policy")
        .remove("camera.252")
        .expect("camera policy");
    assert!(policy.pending_detection_lease_ids.is_empty());
    let jobs = api.detection_jobs.lock().expect("jobs");
    let runtime = &jobs["control-owned-terminal-job"];
    assert!(runtime.detection_lease_cleanup_confirmed);
    assert_eq!(runtime.projection.status, "stopped");
    drop(jobs);
    api.cancel_cat_detection_retry_workers_for_test();
    cleanup_test_paths(&paths);
}

#[test]
fn terminal_cleanup_backoff_is_per_lease_bounded_and_does_not_block_other_camera() {
    let _env_lock = cat_auto_recording_env_lock()
        .lock()
        .expect("cat detection environment lock");
    let (_control_path, _control_env) =
        install_cat_detection_control_test_environment("terminal-cleanup-backoff");
    let (harborlink_url, requests, server) =
        spawn_detection_monitor_cleanup_server(vec![500, 404, 500, 404]);
    let (api, paths) = build_test_admin_api_with_harborlink(
        "terminal-cleanup-backoff",
        HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
        &["camera.252", "camera.b"],
    );
    insert_public_job_with_completed_child_for_camera(
        &api,
        "backoff-camera-a",
        "camera.252",
        "detect-backoff-a",
    );
    let started_at = Instant::now();

    api.monitor_detection_jobs_once_at(started_at)
        .expect("first camera A failure");
    insert_public_job_with_completed_child_for_camera(
        &api,
        "backoff-camera-b",
        "camera.b",
        "detect-backoff-b",
    );
    api.monitor_detection_jobs_once_at(started_at + Duration::from_millis(100))
        .expect("camera B cleanup while A backs off");
    assert_eq!(requests.lock().expect("requests").len(), 2);
    assert!(api.detection_jobs.lock().expect("jobs")["backoff-camera-b"]
        .detection_lease_cleanup_confirmed);

    api.monitor_detection_jobs_once_at(started_at + Duration::from_millis(999))
        .expect("A is not due before one second");
    assert_eq!(requests.lock().expect("requests").len(), 2);
    api.monitor_detection_jobs_once_at(started_at + Duration::from_secs(1))
        .expect("A retries at one second");
    assert_eq!(requests.lock().expect("requests").len(), 3);
    api.monitor_detection_jobs_once_at(started_at + Duration::from_secs(2))
        .expect("second failure backs off for two seconds");
    assert_eq!(requests.lock().expect("requests").len(), 3);
    api.monitor_detection_jobs_once_at(started_at + Duration::from_secs(3))
        .expect("A retries after bounded exponential delay");
    server.join().expect("HarborLink server");

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("/cameras/camera.252/"));
    assert!(requests[1].contains("/cameras/camera.b/"));
    assert!(api.detection_jobs.lock().expect("jobs")["backoff-camera-a"]
        .detection_lease_cleanup_confirmed);
    cleanup_test_paths(&paths);
}

#[test]
fn terminal_cleanup_retry_delay_caps_at_sixty_seconds() {
    let now = Instant::now();
    let mut state = None;
    for expected_seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
        let next = super::super::DetectionLeaseCleanupRetryState::after_failure(state, now);
        assert_eq!(
            next.next_attempt_at.duration_since(now),
            Duration::from_secs(expected_seconds)
        );
        state = Some(next);
    }
}

#[test]
fn terminal_cleanup_snapshot_keeps_more_than_sixty_four_jobs_reclaimable() {
    let mut jobs = HashMap::new();
    for index in 0..65 {
        let job_id = format!("completed-job-{index:02}");
        let mut runtime = sample_running_detection_job(&job_id, "camera.252", false, None);
        runtime.projection.status = "completed".to_string();
        runtime.projection.lease_id = format!("detect-completed-{index:02}");
        runtime.detection_lease_cleanup_confirmed = false;
        jobs.insert(job_id, runtime);
    }

    let intents = super::super::terminal_detection_lease_cleanup_intents(&jobs);
    assert_eq!(intents.len(), 65);
    for intent in &intents {
        assert!(super::super::commit_detection_lease_cleanup_result(
            &mut jobs,
            intent,
            Ok(())
        ));
    }
    prune_detection_job_history(&mut jobs);

    assert!(super::super::detection_lease_responsibility_ids(
        "camera.252",
        Vec::new(),
        &jobs
    )
    .is_empty());
    assert!(jobs.len() <= MAX_DETECTION_JOB_HISTORY);
}

    #[test]
    fn slow_camera_cleanup_does_not_block_another_camera_detection_get() {
        let _env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat detection environment lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cross-camera-cleanup");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let address = listener.local_addr().expect("HarborLink address");
        let (request_started_sender, request_started_receiver) = sync_channel(1);
        let (release_sender, release_receiver) = sync_channel(1);
        let harborlink_server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("slow DELETE request");
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).expect("read slow DELETE request");
            let request = String::from_utf8_lossy(&buffer[..read]);
            assert!(request.starts_with(
                "DELETE /v1/cameras/camera.a/detection-leases/detect-yolo-camera-a "
            ));
            request_started_sender.send(()).expect("signal slow DELETE");
            release_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("release slow DELETE");
            let body = json!({
                "camera_id": "camera.a",
                "lease_id": "detect-yolo-camera-a",
                "status": "stopped",
                "stream_profile": "sub",
                "local_rtsp_url": null,
                "started_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:01:00Z",
                "expires_at": "2026-08-11T00:01:00Z",
                "pre_roll_seconds": 3,
                "pre_roll_ready": true
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("finish slow DELETE");
        });
        let (api, paths) = build_test_admin_api_with_harborlink(
            "cross-camera-cleanup",
            HarborLinkMediaClient::new(format!("http://{address}"))
                .expect("HarborLink client"),
            &["camera.a", "camera.b"],
        );
        api.detection_jobs.lock().expect("detection jobs").extend([
            (
                "yolo-camera-a".to_string(),
                sample_running_detection_job("yolo-camera-a", "camera.a", false, None),
            ),
            (
                "yolo-camera-b".to_string(),
                sample_running_detection_job("yolo-camera-b", "camera.b", false, None),
            ),
        ]);
        let cleanup_api = api.clone();
        let cleanup = thread::spawn(move || {
            cleanup_api.apply_cat_detection_control("camera.a", false, "sub")
        });
        request_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("camera A DELETE started");
        let get_api = api.clone();
        let (get_sender, get_receiver) = sync_channel(1);
        let get = thread::spawn(move || {
            let (status, _) = response_json(get_api.handle_get_detection_job(
                "/api/vision/detection-jobs/yolo-camera-b",
                &detection_gate_principal(RoleKind::Admin),
            ));
            get_sender.send(status).expect("camera B GET result");
        });
        let camera_b_result = get_receiver.recv_timeout(Duration::from_millis(200));
        release_sender.send(()).expect("release camera A DELETE");
        harborlink_server.join().expect("HarborLink server");
        cleanup
            .join()
            .expect("camera A cleanup")
            .expect("camera A control response");
        get.join().expect("camera B GET");

        assert_eq!(camera_b_result.expect("camera B was not globally blocked"), StatusCode(200));
        cleanup_test_paths(&paths);
    }

    #[test]
    fn public_detection_delete_rejects_explicit_enabled_ownership_without_harborlink_call() {
        let _env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat detection environment lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("public-delete-owned");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let job_id = "yolo-public-delete-owned-job";
        let lease_id = format!("detect-{job_id}");
        let (base_url, stop_count, server) =
            spawn_detection_lease_stop_server("camera.252", &lease_id);
        let (api, paths) = build_test_admin_api_with_harborlink(
            "public-delete-owned",
            HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 1)
                .expect("enabled control policy"),
        )
        .expect("persist enabled control policy");
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(
                job_id,
                "camera.252",
                false,
                Some(spawn_sleeping_detection_child()),
            ),
        );

        let (status, body) = response_json(api.handle_stop_detection_job(
            &format!("/api/vision/detection-jobs/{job_id}"),
            &detection_gate_principal(RoleKind::Admin),
        ));
        let (projection_status, had_child, child_running) =
            detection_child_state_and_cleanup(&api, job_id);
        server.join().expect("HarborLink server");

        assert_eq!(status, StatusCode(409));
        assert!(body["error"].as_str().unwrap_or_default().contains("enabled"));
        assert_eq!(stop_count.load(Ordering::SeqCst), 0);
        assert_eq!(projection_status, "running");
        assert!(had_child);
        assert!(child_running);
        cleanup_test_paths(&paths);
    }

    #[test]
    fn public_detection_delete_corrupt_store_is_redacted_and_has_no_side_effect() {
        let _env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat detection environment lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("public-delete-corrupt");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let job_id = "yolo-public-delete-corrupt-job";
        let lease_id = format!("detect-{job_id}");
        let (base_url, stop_count, server) =
            spawn_detection_lease_stop_server("camera.252", &lease_id);
        let (api, paths) = build_test_admin_api_with_harborlink(
            "public-delete-corrupt",
            HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let secret_path = "/var/lib/private/cat-controls.json";
        *api.cat_detection_control_store_load_error
            .lock()
            .expect("control load error") = Some(format!(
            "failed to read {secret_path}: permission denied (os error 13)"
        ));
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(
                job_id,
                "camera.252",
                false,
                Some(spawn_sleeping_detection_child()),
            ),
        );

        let (status, body) = response_json(api.handle_stop_detection_job(
            &format!("/api/vision/detection-jobs/{job_id}"),
            &detection_gate_principal(RoleKind::Admin),
        ));
        let serialized = body.to_string();
        let (projection_status, had_child, child_running) =
            detection_child_state_and_cleanup(&api, job_id);
        server.join().expect("HarborLink server");

        assert_eq!(status, StatusCode(503));
        assert_eq!(
            body["error"]["code"],
            "CAT_DETECTION_CONTROL_UNAVAILABLE"
        );
        assert!(!serialized.contains(secret_path));
        assert!(!serialized.contains("permission denied"));
        assert_eq!(stop_count.load(Ordering::SeqCst), 0);
        assert_eq!(projection_status, "running");
        assert!(had_child);
        assert!(child_running);
        cleanup_test_paths(&paths);
    }

    #[test]
    fn public_detection_delete_serializes_with_new_enabled_policy_revision() {
        let _env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat detection environment lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("public-delete-serialized");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let job_id = "yolo-public-delete-serialized-job";
        let lease_id = format!("detect-{job_id}");
        let (base_url, stop_count, server) =
            spawn_detection_lease_stop_server("camera.252", &lease_id);
        let (api, paths) = build_test_admin_api_with_harborlink(
            "public-delete-serialized",
            HarborLinkMediaClient::new(base_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(
                job_id,
                "camera.252",
                false,
                Some(spawn_sleeping_detection_child()),
            ),
        );
        let camera_lock = api
            .cat_detection_control_camera_lock("camera.252")
            .expect("camera coordination lock");
        let camera_guard = camera_lock.lock().expect("hold camera coordination");
        let request_api = api.clone();
        let request = thread::spawn(move || {
            response_json(request_api.handle_stop_detection_job(
                &format!("/api/vision/detection-jobs/{job_id}"),
                &detection_gate_principal(RoleKind::Admin),
            ))
        });
        thread::sleep(Duration::from_millis(100));
        assert_eq!(stop_count.load(Ordering::SeqCst), 0);
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 2)
                .expect("new enabled policy"),
        )
        .expect("persist new enabled revision while holding coordination");
        drop(camera_guard);

        let (status, _) = request.join().expect("public delete request");
        let (projection_status, had_child, child_running) =
            detection_child_state_and_cleanup(&api, job_id);
        server.join().expect("HarborLink server");

        assert_eq!(status, StatusCode(409));
        assert_eq!(stop_count.load(Ordering::SeqCst), 0);
        assert_eq!(projection_status, "running");
        assert!(had_child);
        assert!(child_running);
        cleanup_test_paths(&paths);
    }

    #[test]
    fn cat_detection_control_routes_use_the_gate_principal_policy() {
        for method in [Method::Get, Method::Put] {
            assert!(is_gate_principal_endpoint(
                &method,
                "/api/cameras/camera%2F252/cat-detection/control"
            ));
        }
        assert!(!is_gate_principal_endpoint(
            &Method::Get,
            "/api/cameras/camera.252/cat-detection/control/extra"
        ));
    }

    #[test]
    fn cat_detection_control_get_without_policy_projects_stopped_runtime() {
        let _env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let _control_path = EnvGuard::set(
            "HARBOR_K3_CAT_DETECTION_CONTROL_PATH",
            unique_store_path("cat-detection-control-get")
                .to_str()
                .expect("UTF-8 control path"),
        );
        let _reconciliation_path = EnvGuard::set(
            "HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH",
            unique_store_path("cat-detection-control-reconciliation")
                .to_str()
                .expect("UTF-8 reconciliation path"),
        );
        let _validation_path = EnvGuard::set(
            "HARBOR_K3_CAT_RECORDING_VALIDATION_STORE_PATH",
            unique_store_path("cat-detection-control-validation")
                .to_str()
                .expect("UTF-8 validation path"),
        );
        let (api, _paths) = build_test_admin_api("cat-detection-control-get");
        api.admin_store
            .registry_store()
            .save_devices(&[CameraDevice::new(
                "camera/252",
                "Camera 252",
                "rtsp://camera.invalid/sub",
            )])
            .expect("save test camera");
        let server = Server::http("127.0.0.1:0").expect("admin test server");
        let base_url = format!("http://{}", server.server_addr());
        let server_thread = thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(3))
                .expect("receive admin request")
                .expect("admin request");
            api.handle(request);
        });

        let response = Client::builder()
            .build()
            .expect("HTTP client")
            .get(format!(
                "{base_url}/api/cameras/camera%2F252/cat-detection/control"
            ))
            .bearer_auth("service-token")
            .header("X-Harbor-Principal-Source", "harboros")
            .header("X-Harbor-Principal-Id", "harboros:uid:1000")
            .header("X-Harbor-Principal-Roles", "FULL_ADMIN")
            .header("X-Harbor-Workspace-Id", "home-1")
            .send()
            .expect("control response");
        let status = response.status();
        let body: Value = response.json().expect("control json");
        server_thread.join().expect("admin server");

        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(
            body.as_object()
                .expect("control object")
                .keys()
                .cloned()
                .collect::<HashSet<_>>(),
            HashSet::from([
                "camera_id".to_string(),
                "explicit".to_string(),
                "desired_enabled".to_string(),
                "desired_stream_profile".to_string(),
                "effective_status".to_string(),
                "effective_stream_profile".to_string(),
                "job_id".to_string(),
                "updated_at".to_string(),
                "message".to_string(),
            ])
        );
        assert_eq!(body["camera_id"], "camera/252");
        assert_eq!(body["explicit"], false);
        assert_eq!(body["desired_enabled"], false);
        assert_eq!(body["desired_stream_profile"], "sub");
        assert_eq!(body["effective_status"], "stopped");
        assert!(body["effective_stream_profile"].is_null());
        assert!(body["job_id"].is_null());
        assert!(body["updated_at"].is_null());
        assert!(body["message"].is_null());
    }

    #[test]
    fn cat_detection_control_without_policy_projects_existing_running_job() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-running-projection");
        let (api, _paths) = build_test_admin_api("cat-control-running-projection");
        save_test_camera(&api, "camera.252");
        api.detection_jobs.lock().expect("detection jobs").insert(
            "pre-control-worker".to_string(),
            sample_running_detection_job("pre-control-worker", "camera.252", false, None),
        );

        let response = api
            .cat_detection_control_response("camera.252")
            .expect("control response");

        assert!(!response.explicit);
        assert!(response.desired_enabled);
        assert_eq!(response.desired_stream_profile, "sub");
        assert_eq!(response.effective_status, "running");
        assert_eq!(response.effective_stream_profile.as_deref(), Some("sub"));
        assert_eq!(response.job_id.as_deref(), Some("pre-control-worker"));
        assert!(response.updated_at.is_none());
    }

    #[test]
    fn cat_detection_control_enable_is_idempotent_and_disable_persists_before_stop() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-idempotent");
        let (mut api, _paths) = build_test_admin_api("cat-control-idempotent");
        save_test_camera(&api, "camera.252");
        let lease_id = "detect-cat-control-idempotent";
        let steps = vec![
            DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "running", "sub"),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            },
        ];
        let (harborlink_url, harborlink_requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        api.harborlink_media =
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client");
        let (_output_root, _worker_env) = install_sleeping_detection_worker("cat-control");
        let api_for_cleanup = api.clone();
        let (base_url, admin_server) = spawn_admin_test_server(api, 4);
        let client = Client::builder().build().expect("HTTP client");
        let url = format!("{base_url}/api/cameras/camera.252/cat-detection/control");

        let first = gate_admin_request(&client, reqwest::Method::PUT, url.clone())
            .json(&json!({"enabled": true, "stream_profile": "sub"}))
            .send()
            .expect("first enable response");
        let first_status = first.status();
        let first_body: Value = first.json().expect("first enable json");
        let second = gate_admin_request(&client, reqwest::Method::PUT, url.clone())
            .json(&json!({"enabled": true, "stream_profile": "sub"}))
            .send()
            .expect("second enable response");
        let second_status = second.status();
        let second_body: Value = second.json().expect("second enable json");
        let get = gate_admin_request(&client, reqwest::Method::GET, url.clone())
            .send()
            .expect("get response");
        let get_status = get.status();
        let get_body: Value = get.json().expect("get json");
        let disabled = gate_admin_request(&client, reqwest::Method::PUT, url.clone())
            .json(&json!({"enabled": false, "stream_profile": "sub"}))
            .send()
            .expect("disable response");
        let disabled_status = disabled.status();
        let disabled_body: Value = disabled.json().expect("disable json");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(first_status, reqwest::StatusCode::OK);
        assert_eq!(second_status, reqwest::StatusCode::OK);
        assert_eq!(get_status, reqwest::StatusCode::OK);
        assert_eq!(disabled_status, reqwest::StatusCode::OK);
        assert_eq!(first_body["explicit"], true);
        assert_eq!(first_body["desired_enabled"], true);
        assert_eq!(first_body["effective_status"], "running");
        assert_eq!(first_body["effective_stream_profile"], "sub");
        assert_eq!(second_body["job_id"], first_body["job_id"]);
        assert_eq!(get_body, second_body);
        assert_eq!(disabled_body["explicit"], true);
        assert_eq!(disabled_body["desired_enabled"], false);
        assert_eq!(disabled_body["effective_status"], "stopped");
        assert!(disabled_body["effective_stream_profile"].is_null());
        assert!(disabled_body["job_id"].is_null());
        assert_eq!(
            first_body
                .as_object()
                .expect("first response object")
                .keys()
                .collect::<HashSet<_>>(),
            disabled_body
                .as_object()
                .expect("disabled response object")
                .keys()
                .collect::<HashSet<_>>()
        );
        assert_eq!(harborlink_requests.lock().expect("requests").len(), 2);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("load persisted controls");
        assert!(!persisted["camera.252"].desired_enabled);
        assert!(!api_for_cleanup
            .cat_detection_may_run("camera.252")
            .expect("read explicit control"));
        api_for_cleanup
            .ensure_live_managed_detection_job("camera.252", "sub")
            .expect("explicit disable must block live restart");
        let now = cat_auto_recording_epoch_ms();
        let mut blocked_runtime =
            sample_running_detection_job("explicitly-disabled-runtime", "camera.252", true, None);
        blocked_runtime.projection.expires_at = "2000-01-01T00:00:00Z".to_string();
        api_for_cleanup
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .insert(blocked_runtime.projection.job_id.clone(), blocked_runtime);
        api_for_cleanup
            .renew_auto_recording_detection_leases()
            .expect("explicit disable must block lease renewal");
        api_for_cleanup
            .process_cat_detection_result(
                "camera.252",
                "sub",
                Some(&json!({
                    "sequence": 1,
                    "frame_epoch_ms": now,
                    "detection_count": 1,
                    "consecutive_present_frames": 1,
                    "consecutive_absent_frames": 0,
                    "present_since_epoch_ms": now,
                    "absent_since_epoch_ms": 0,
                    "max_confidence": 0.9
                })),
                CatAutoRecordingConfig {
                    start_consecutive_frames: 1,
                    start_duration_ms: 0,
                    stop_consecutive_frames: 1,
                    stop_duration_ms: 0,
                },
            )
            .expect("explicit disable must block automatic recording start");
        cleanup_detection_children(&api_for_cleanup);
    }

    #[test]
    fn cat_detection_control_disable_failure_keeps_false_policy_and_reports_failed() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-stop-failure");
        let (mut api, _paths) = build_test_admin_api("cat-control-stop-failure");
        api = api.with_cat_detection_retry_scheduler_for_test();
        save_test_camera(&api, "camera.252");
        let lease_id = "detect-cat-control-stop-failure";
        let steps = vec![
            DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "running", "sub"),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "500 Internal Server Error",
                response: json!({"error": "simulated stop failure"}),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            },
        ];
        let (harborlink_url, _requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        api.harborlink_media =
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client");
        let (_output_root, _worker_env) = install_sleeping_detection_worker("cat-control-failure");
        let api_for_cleanup = api.clone();
        let (base_url, admin_server) = spawn_admin_test_server(api, 3);
        let client = Client::builder().build().expect("HTTP client");
        let url = format!("{base_url}/api/cameras/camera.252/cat-detection/control");

        let enabled = gate_admin_request(&client, reqwest::Method::PUT, url.clone())
            .json(&json!({"enabled": true, "stream_profile": "sub"}))
            .send()
            .expect("enable response");
        assert_eq!(enabled.status(), reqwest::StatusCode::OK);
        let disabled = gate_admin_request(&client, reqwest::Method::PUT, url.clone())
            .json(&json!({"enabled": false, "stream_profile": "sub"}))
            .send()
            .expect("disable response");
        let disabled_status = disabled.status();
        let disabled_body: Value = disabled.json().expect("disable json");
        let pending_after_failed_stop = CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .load()
            .expect("load persisted controls")["camera.252"]
            .pending_detection_lease_ids
            .clone();
        thread::sleep(Duration::from_millis(1_500));
        let retried = gate_admin_request(&client, reqwest::Method::GET, url)
            .send()
            .expect("retried control response");
        let retried_status = retried.status();
        let retried_body: Value = retried.json().expect("retried control json");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(disabled_status, reqwest::StatusCode::OK);
        assert_eq!(disabled_body["desired_enabled"], false);
        assert_eq!(disabled_body["effective_status"], "failed");
        assert_eq!(pending_after_failed_stop, vec![lease_id.to_string()]);
        assert!(disabled_body["message"]
            .as_str()
            .is_some_and(|message| message.contains("cleanup was incomplete")));
        assert_eq!(retried_status, reqwest::StatusCode::OK);
        assert_eq!(retried_body["desired_enabled"], false);
        assert_eq!(retried_body["effective_status"], "stopped");
        assert!(retried_body["message"].is_null());
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("load persisted controls");
        assert!(!persisted["camera.252"].desired_enabled);
        cleanup_detection_children(&api_for_cleanup);
        api_for_cleanup.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_initialization_recovers_true_and_keeps_false_stopped() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-recovery");
        CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .upsert(
                CatDetectionControlPolicy::new(
                    "camera.252",
                    true,
                    "main",
                    cat_auto_recording_epoch_ms() as u128,
                )
                .expect("valid recovery policy"),
            )
            .expect("persist recovery policy");
        let lease_id = "detect-cat-control-recovery";
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(vec![DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("main".to_string()),
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "running", "main"),
            }]);
        let (_output_root, _worker_env) = install_sleeping_detection_worker("cat-control-recovery");
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-recovery",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        api.start_cat_detection_control_recovery();

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && !api
                .detection_jobs
                .lock()
                .expect("detection jobs")
                .values()
                .any(|runtime| runtime.projection.status == "running")
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");
        api.ensure_live_managed_detection_job("camera.252", "sub")
            .expect("live hook must preserve explicit profile ownership");

        let jobs = api.detection_jobs.lock().expect("detection jobs");
        let recovered = jobs
            .values()
            .find(|runtime| runtime.projection.status == "running")
            .expect("desired true policy must recover a detection job");
        assert_eq!(recovered.projection.camera_id, "camera.252");
        assert_eq!(recovered.projection.stream_profile, "main");
        assert!(!recovered.projection.managed_by_live);
        drop(jobs);
        assert_eq!(requests.lock().expect("requests").len(), 1);
        cleanup_detection_children(&api);
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_initialization_does_not_start_false_policy() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-false-recovery");
        CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .upsert(
                CatDetectionControlPolicy::new(
                    "camera.252",
                    false,
                    "sub",
                    cat_auto_recording_epoch_ms() as u128,
                )
                .expect("valid disabled policy"),
            )
            .expect("persist disabled policy");
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-false-recovery",
            HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        api.start_cat_detection_control_recovery();

        thread::sleep(Duration::from_millis(150));

        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .is_empty());
        let response = api
            .cat_detection_control_response("camera.252")
            .expect("control response");
        assert!(response.explicit);
        assert!(!response.desired_enabled);
        assert_eq!(response.effective_status, "stopped");
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_disable_clears_memory_only_recording_sample() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) = install_cat_detection_control_test_environment(
            "cat-control-memory-only-recording-stop",
        );
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-memory-only-recording-stop",
            HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let sample = CatRecordingReconciliationState {
            camera_id: "camera.252".to_string(),
            last_sequence: Some(42),
            stream_profile: Some("sub".to_string()),
            ..CatRecordingReconciliationState::default()
        };
        api.cat_auto_recording
            .lock()
            .expect("cat auto-recording state")
            .insert(sample.camera_id.clone(), sample);

        let response = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("memory-only state must not block disabling detection");

        assert_eq!(response.effective_status, "stopped");
        assert!(!api
            .cat_auto_recording
            .lock()
            .expect("cat auto-recording state")
            .contains_key("camera.252"));
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_disable_drains_recording_without_stopping_it() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-recording-stop");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "event-recording-control-stop";
        let event_id = "cat-activity-control-stop";
        let artifact =
            sample_cat_recording_artifact("recordings~cat-control-natural-drain", event_id);
        let mut terminal =
            event_recording_lease_response("camera.252", lease_id, event_id, "stopped");
        terminal["artifacts"] = json!([artifact]);
        let steps = vec![
            DetectionLeaseServerStep {
                method: "GET",
                path: format!("/v1/cameras/camera.252/event-recordings/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: event_recording_lease_response(
                    "camera.252",
                    lease_id,
                    event_id,
                    "running",
                ),
            },
            DetectionLeaseServerStep {
                method: "GET",
                path: format!("/v1/cameras/camera.252/event-recordings/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: event_recording_lease_response(
                    "camera.252",
                    lease_id,
                    event_id,
                    "running",
                ),
            },
            DetectionLeaseServerStep {
                method: "GET",
                path: format!("/v1/cameras/camera.252/event-recordings/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: terminal,
            },
        ];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (mut api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-recording-stop",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api = api.with_cat_detection_retry_scheduler_for_test();
        let reconciliation_path = unique_store_path("cat-control-recording-stop-state");
        api.cat_recording_reconciliation_store =
            CatRecordingReconciliationStore::new(reconciliation_path);
        api.cat_recording_validation_mode = CatRecordingValidationMode::Shadow;
        api.cat_recording_validation_store = CatRecordingValidationStore::new(unique_store_path(
            "cat-control-recording-natural-drain-validation",
        ));
        let recording = CatRecordingReconciliationState {
            camera_id: "camera.252".to_string(),
            phase: CatRecordingReconciliationPhase::Active,
            created_at_epoch_ms: cat_auto_recording_epoch_ms(),
            event_id: Some(event_id.to_string()),
            lease_id: Some(lease_id.to_string()),
            stream_profile: Some("sub".to_string()),
            detection_evidence: vec![CatDetectionEvidence {
                sequence: 1,
                frame_epoch_ms: 1_786_060_800_000,
                confidence_ppm: 900_000,
            }],
            ..Default::default()
        };
        api.cat_recording_reconciliation_store
            .upsert(recording.clone())
            .expect("persist recording state");
        api.cat_auto_recording = Arc::new(Mutex::new(HashMap::from([(
            recording.camera_id.clone(),
            recording,
        )])));

        let response = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disabled policy starts natural recording drain");
        assert_eq!(response.effective_status, "stopping");
        api.cat_auto_recording_tick(CatAutoRecordingConfig {
            start_consecutive_frames: 1,
            start_duration_ms: 0,
            stop_consecutive_frames: 1,
            stop_duration_ms: 0,
        })
        .expect("disabled auto-recording tick preserves natural drain");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api
                .cat_auto_recording
                .lock()
                .expect("recording state")
                .contains_key("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        drop(requests);
        assert!(!api
            .cat_auto_recording
            .lock()
            .expect("recording state")
            .contains_key("camera.252"));
        assert!(api
            .cat_recording_reconciliation_store
            .load()
            .expect("recording store")
            .is_empty());
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(
            api.cat_recording_validation_store
                .list_latest()
                .expect("validation records")[0]
                .artifact_id,
            "recordings~cat-control-natural-drain"
        );
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
    }

    #[test]
    fn cat_detection_control_disable_recovers_lost_pending_start_and_drains() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-lost-recording-start");
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let harborlink_addr = listener.local_addr().expect("HarborLink address");
        let pending_event_id = Arc::new(Mutex::new(None::<String>));
        let server_event_id = pending_event_id.clone();
        let observed_requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = observed_requests.clone();
        let lease_id = "event-recording-lost-control-start";
        let harborlink_server = thread::spawn(move || {
            for step in 0..4 {
                let (mut stream, _) = listener.accept().expect("HarborLink accept");
                let mut buffer = [0_u8; 8192];
                let read = stream.read(&mut buffer).expect("read HarborLink request");
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                server_requests
                    .lock()
                    .expect("observed requests")
                    .push(request.clone());
                match step {
                    0 => {
                        assert!(request
                            .starts_with("GET /v1/cameras/camera.252/event-recordings/current "));
                        let body = json!({"error":"not_found"}).to_string();
                        stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                )
                                .as_bytes(),
                            )
                            .expect("write no current recording");
                    }
                    1 => {
                        assert!(request
                            .starts_with("POST /v1/cameras/camera.252/event-recordings/current "));
                        // HarborLink started the lease, but its response was lost.
                    }
                    2 => {
                        assert!(request
                            .starts_with("GET /v1/cameras/camera.252/event-recordings/current "));
                        let event_id = server_event_id
                            .lock()
                            .expect("pending event id")
                            .clone()
                            .expect("pending event id recorded before disable");
                        let body = event_recording_lease_response(
                            "camera.252",
                            lease_id,
                            &event_id,
                            "running",
                        )
                        .to_string();
                        stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                )
                                .as_bytes(),
                            )
                            .expect("write recovered recording");
                    }
                    _ => {
                        assert!(request.starts_with(&format!(
                            "GET /v1/cameras/camera.252/event-recordings/{lease_id} "
                        )));
                        let event_id = server_event_id
                            .lock()
                            .expect("pending event id")
                            .clone()
                            .expect("pending event id recorded before terminal reconciliation");
                        let artifact = sample_cat_recording_artifact(
                            "recordings~cat-control-lost-start",
                            &event_id,
                        );
                        let mut stopped = event_recording_lease_response(
                            "camera.252",
                            lease_id,
                            &event_id,
                            "stopped",
                        );
                        stopped["artifacts"] = json!([artifact]);
                        let body = stopped.to_string();
                        stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(), body
                                )
                                .as_bytes(),
                            )
                            .expect("write naturally completed recording");
                    }
                }
            }
        });
        let (mut api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-lost-recording-start",
            HarborLinkMediaClient::new(format!("http://{harborlink_addr}"))
                .expect("HarborLink client"),
            &["camera.252"],
        );
        api = api.with_cat_detection_retry_scheduler_for_test();
        let reconciliation_path = unique_store_path("cat-control-lost-recording-start-state");
        api.cat_recording_reconciliation_store =
            CatRecordingReconciliationStore::new(reconciliation_path);
        let validation_path = unique_store_path("cat-control-lost-recording-start-validation");
        api.cat_recording_validation_mode = CatRecordingValidationMode::Shadow;
        api.cat_recording_validation_store =
            CatRecordingValidationStore::new(validation_path.clone());
        api.cat_auto_recording = Arc::new(Mutex::new(HashMap::new()));
        let sample_epoch_ms = cat_auto_recording_epoch_ms() as u64;
        let sample = json!({
            "target_label": "cat", "sequence": 1, "frame_epoch_ms": sample_epoch_ms,
            "detection_count": 1, "consecutive_present_frames": 1,
            "consecutive_absent_frames": 0, "present_since_epoch_ms": sample_epoch_ms,
            "absent_since_epoch_ms": 0,
            "detections": [{"label":"cat", "confidence":0.95}]
        });

        api.process_cat_detection_result(
            "camera.252",
            "sub",
            Some(&sample),
            CatAutoRecordingConfig {
                start_consecutive_frames: 1,
                start_duration_ms: 0,
                stop_consecutive_frames: 3,
                stop_duration_ms: 2_000,
            },
        )
        .expect_err("lost recording start response surfaces to caller");
        *pending_event_id.lock().expect("pending event id") = api
            .cat_recording_reconciliation_store
            .load()
            .expect("pending recording state")
            .get("camera.252")
            .and_then(|state| state.event_id.clone());

        let response = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disable reconciles ambiguous recording start");

        assert_eq!(response.effective_status, "stopping");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api
                .cat_auto_recording
                .lock()
                .expect("recording state")
                .contains_key("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");
        let requests = observed_requests.lock().expect("requests");
        assert_eq!(requests.len(), 4);
        assert!(requests
            .iter()
            .all(|request| !request.starts_with("DELETE ")));
        drop(requests);
        assert!(api
            .cat_recording_reconciliation_store
            .load()
            .expect("recording store")
            .is_empty());
        assert!(!api
            .cat_auto_recording
            .lock()
            .expect("recording state")
            .contains_key("camera.252"));
        let validations = api
            .cat_recording_validation_store
            .list_latest()
            .expect("validation records");
        assert_eq!(validations.len(), 1);
        assert_eq!(
            validations[0].artifact_id,
            "recordings~cat-control-lost-start"
        );
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_retries_failed_recording_artifact_registration() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-artifact-retry");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "event-recording-artifact-retry";
        let event_id = "cat-activity-artifact-retry";
        let artifact =
            sample_cat_recording_artifact("recordings~cat-control-artifact-retry", event_id);
        let mut terminal =
            event_recording_lease_response("camera.252", lease_id, event_id, "stopped");
        terminal["artifacts"] = serde_json::to_value(vec![artifact]).expect("terminal artifacts");
        let steps = vec![
            DetectionLeaseServerStep {
                method: "GET",
                path: format!("/v1/cameras/camera.252/event-recordings/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: terminal.clone(),
            },
            DetectionLeaseServerStep {
                method: "GET",
                path: format!("/v1/cameras/camera.252/event-recordings/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: terminal,
            },
        ];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (mut api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-artifact-retry",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api = api.with_cat_detection_retry_scheduler_for_test();
        let reconciliation_path = unique_store_path("cat-control-artifact-retry-state");
        api.cat_recording_reconciliation_store =
            CatRecordingReconciliationStore::new(reconciliation_path);
        let validation_path = unique_store_path("cat-control-artifact-retry-validation");
        api.cat_recording_validation_mode = CatRecordingValidationMode::Shadow;
        api.cat_recording_validation_store =
            CatRecordingValidationStore::new(validation_path.clone());
        fs::create_dir(&validation_path).expect("block validation data path");
        let recording = CatRecordingReconciliationState {
            camera_id: "camera.252".to_string(),
            phase: CatRecordingReconciliationPhase::Active,
            created_at_epoch_ms: cat_auto_recording_epoch_ms(),
            event_id: Some(event_id.to_string()),
            lease_id: Some(lease_id.to_string()),
            stream_profile: Some("sub".to_string()),
            detection_evidence: vec![CatDetectionEvidence {
                sequence: 2,
                frame_epoch_ms: 1_786_060_800_100,
                confidence_ppm: 910_000,
            }],
            ..Default::default()
        };
        api.cat_recording_reconciliation_store
            .upsert(recording.clone())
            .expect("persist recording state");
        api.cat_auto_recording = Arc::new(Mutex::new(HashMap::from([(
            recording.camera_id.clone(),
            recording,
        )])));

        let response = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disabled policy persists despite validation failure");
        assert_eq!(response.effective_status, "failed");
        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        fs::remove_dir(&validation_path).expect("restore validation data path");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api
                .cat_auto_recording
                .lock()
                .expect("recording state")
                .contains_key("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        drop(requests);
        assert!(!api
            .cat_auto_recording
            .lock()
            .expect("recording state")
            .contains_key("camera.252"));
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_cleans_pending_non_running_leases() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-non-running-leases");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_ids = ["detect-completed", "detect-expired", "detect-failed"];
        let steps = lease_ids
            .iter()
            .map(|lease_id| DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            })
            .collect();
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-non-running-leases",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", false, "sub", 7).expect("disabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.iter().map(ToString::to_string))
            .expect("pending leases");
        api.cat_detection_control_store
            .as_ref()
            .expect("control store")
            .upsert(policy.clone())
            .expect("persist policy");
        api.cat_detection_controls
            .lock()
            .expect("controls")
            .insert("camera.252".to_string(), policy.clone());
        let mut jobs = api.detection_jobs.lock().expect("detection jobs");
        for (status, lease_id) in [
            ("completed", "detect-completed"),
            ("expired", "detect-expired"),
            ("failed", "detect-failed"),
        ] {
            let mut runtime = sample_running_detection_job(lease_id, "camera.252", false, None);
            runtime.projection.status = status.to_string();
            runtime.projection.lease_id = lease_id.to_string();
            jobs.insert(lease_id.to_string(), runtime);
        }
        drop(jobs);

        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", 7)
                .expect("reconcile pending leases"),
            CatDetectionControlCoordination::Converged
        );
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(requests.lock().expect("requests").len(), 3);
        let persisted = api
            .cat_detection_control_store
            .as_ref()
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
    }

    #[test]
    fn cat_detection_control_retry_404_confirms_lost_delete_and_stops_runtime() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-lost-delete-response");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "detect-lost-delete-response";
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let harborlink_addr = listener.local_addr().expect("HarborLink address");
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = request_count.clone();
        let harborlink_server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("HarborLink accept");
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).expect("read HarborLink request");
                assert!(
                    String::from_utf8_lossy(&buffer[..read]).starts_with(&format!(
                        "DELETE /v1/cameras/camera.252/detection-leases/{lease_id} "
                    ))
                );
                server_request_count.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    // HarborLink deleted the lease, but the response was lost.
                    continue;
                }
                let body = json!({"error":"not_found"}).to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        )
                        .as_bytes(),
                    )
                    .expect("write already absent response");
            }
        });
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-lost-delete-response",
            HarborLinkMediaClient::new(format!("http://{harborlink_addr}"))
                .expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let mut runtime =
            sample_running_detection_job("lost-delete-response", "camera.252", false, None);
        runtime.projection.lease_id = lease_id.to_string();
        api.detection_jobs
            .lock()
            .expect("detection jobs")
            .insert(runtime.projection.job_id.clone(), runtime);

        let initial = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disabled policy persists after lost delete response");
        assert_eq!(initial.effective_status, "failed");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && !CatDetectionControlStore::try_new(control_path.clone())
                .expect("control store")
                .load()
                .expect("controls")["camera.252"]
                .pending_detection_lease_ids
                .is_empty()
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let runtime = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .get("lost-delete-response")
            .expect("detection runtime")
            .projection
            .clone();
        assert_eq!(runtime.status, "stopped");
        assert!(runtime.message.is_none());
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
        assert_eq!(
            api.cat_detection_control_reconciliation
                .lock()
                .expect("reconciliation")
                .get("camera.252")
                .expect("reconciliation state")
                .effective_status,
            "stopped"
        );
        thread::sleep(Duration::from_millis(1_200));
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_running_worker_cleans_old_responsibility_without_restart() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-running-old-cleanup");
        let old_lease_id = "detect-aaa-old-responsibility";
        let healthy_lease_id = "detect-zzz-healthy-worker";
        let steps = vec![DetectionLeaseServerStep {
            method: "DELETE",
            path: format!("/v1/cameras/camera.252/detection-leases/{old_lease_id}"),
            request_profile: None,
            status: "200 OK",
            response: detection_lease_response("camera.252", old_lease_id, "stopped", "sub"),
        }];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-running-old-cleanup",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 51).expect("policy");
        policy
            .set_pending_detection_lease_ids([old_lease_id.to_string()])
            .expect("old pending lease");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        {
            let mut jobs = api.detection_jobs.lock().expect("detection jobs");
            let mut healthy =
                sample_running_detection_job("healthy-worker", "camera.252", false, None);
            healthy.projection.lease_id = healthy_lease_id.to_string();
            healthy.projection.stream_profile = "sub".to_string();
            let mut old = sample_running_detection_job("old-worker", "camera.252", false, None);
            old.projection.lease_id = old_lease_id.to_string();
            old.projection.status = "stopped".to_string();
            old.detection_lease_cleanup_confirmed = false;
            jobs.insert("healthy-worker".to_string(), healthy);
            jobs.insert("old-worker".to_string(), old);
        }

        let response = api
            .cat_detection_control_response("camera.252")
            .expect("running response");
        assert_eq!(response.effective_status, "running");
        assert_eq!(response.job_id.as_deref(), Some("healthy-worker"));
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api
                .cat_detection_explicit_policy("camera.252")
                .expect("policy")
                .expect("explicit policy")
                .pending_detection_lease_ids
                .contains(&old_lease_id.to_string())
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "DELETE /v1/cameras/camera.252/detection-leases/{old_lease_id} "
        )));
        drop(requests);
        let jobs = api.detection_jobs.lock().expect("detection jobs");
        let healthy = jobs.get("healthy-worker").expect("healthy worker");
        assert_eq!(healthy.projection.status, "running");
        assert_eq!(healthy.projection.lease_id, healthy_lease_id);
        assert!(!healthy.detection_lease_cleanup_confirmed);
        assert!(
            jobs.get("old-worker")
                .expect("old runtime")
                .detection_lease_cleanup_confirmed
        );
        drop(jobs);
        let retry_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < retry_deadline
            && api.cat_detection_retry_contains_camera_for_test("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_disable_captures_stopped_runtime_with_unconfirmed_cleanup() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-stopped-unconfirmed");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "detect-stopped-unconfirmed";
        let steps = vec![
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "500 Internal Server Error",
                response: json!({"error": "live stop cleanup failed"}),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "500 Internal Server Error",
                response: json!({"error": "control cleanup failed"}),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            },
        ];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-stopped-unconfirmed",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let mut runtime =
            sample_running_detection_job("stopped-unconfirmed", "camera.252", true, None);
        runtime.projection.lease_id = lease_id.to_string();
        api.detection_jobs
            .lock()
            .expect("detection jobs")
            .insert(runtime.projection.job_id.clone(), runtime);

        api.stop_live_managed_detection_job("camera.252")
            .expect("local worker stop remains observable after lease failure");
        let stopped = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .get("stopped-unconfirmed")
            .expect("stopped runtime")
            .projection
            .clone();
        assert_eq!(stopped.status, "stopped");
        assert!(stopped
            .message
            .as_deref()
            .is_some_and(|message| message.contains("cleanup was incomplete")));

        let initial = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disabled policy persists while retry is pending");
        assert_eq!(initial.effective_status, "failed");
        let persisted_after_put = CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .load()
            .expect("controls");
        assert_eq!(
            persisted_after_put["camera.252"].pending_detection_lease_ids,
            vec![lease_id.to_string()]
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && !CatDetectionControlStore::try_new(control_path.clone())
                .expect("control store")
                .load()
                .expect("controls")["camera.252"]
                .pending_detection_lease_ids
                .is_empty()
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(requests.lock().expect("requests").len(), 3);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        let stopped = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .get("stopped-unconfirmed")
            .expect("stopped runtime")
            .projection
            .clone();
        assert_eq!(stopped.status, "stopped");
        assert!(stopped.message.is_none());
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_disable_deduplicates_full_pending_runtime_overlap() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-full-overlap");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_ids = (0..64)
            .map(|index| format!("detect-overlap-{index:02}"))
            .collect::<Vec<_>>();
        let (harborlink_url, requests, harborlink_server) = spawn_detection_cleanup_boundary_server(
            "camera.252",
            lease_ids.clone(),
            control_path.clone(),
        );
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-full-overlap",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 31).expect("enabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.clone())
            .expect("64 pending leases");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        install_unconfirmed_detection_runtimes(&api, "camera.252", &lease_ids);

        let result = api.apply_cat_detection_control("camera.252", false, "sub");
        harborlink_server.join().expect("HarborLink server");
        let response = result.expect("overlapping responsibilities remain within capacity");

        assert_eq!(response.effective_status, "stopped");
        assert_eq!(requests.lock().expect("requests").len(), 64);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(!persisted["camera.252"].desired_enabled);
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .filter(|runtime| runtime.projection.camera_id == "camera.252")
            .all(|runtime| runtime.detection_lease_cleanup_confirmed));
    }

    #[test]
    fn cat_detection_control_capacity_rejects_65th_responsibility_before_post() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-capacity-admission");
        let _provider = EnvGuard::set("HARBOR_K3_YOLO_PROVIDER", "cpu");
        let lease_ids = (0..64)
            .map(|index| format!("detect-capacity-{index:02}"))
            .collect::<Vec<_>>();
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-capacity-admission",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 32).expect("enabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.clone())
            .expect("64 pending leases");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        install_unconfirmed_detection_runtimes(&api, "camera.252", &lease_ids);
        let config = DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        };

        let result = api.start_detection_job(config, false);
        harborlink_server.join().expect("HarborLink server");
        let (status, error) = result.expect_err("65th cleanup responsibility must be rejected");

        assert_eq!(status, StatusCode(409));
        assert!(error.contains("64") && error.contains("cleanup"), "{error}");
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cat_detection_control_capacity_ignores_confirmed_and_empty_runtime_leases() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-capacity-exclusions");
        let _provider = EnvGuard::set("HARBOR_K3_YOLO_PROVIDER", "cpu");
        let lease_ids = (0..63)
            .map(|index| format!("detect-capacity-allowed-{index:02}"))
            .collect::<Vec<_>>();
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-capacity-exclusions",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 33).expect("enabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.clone())
            .expect("63 pending leases");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        install_unconfirmed_detection_runtimes(&api, "camera.252", &lease_ids);
        {
            let mut jobs = api.detection_jobs.lock().expect("detection jobs");
            let mut confirmed =
                sample_running_detection_job("confirmed-capacity", "camera.252", false, None);
            confirmed.projection.status = "stopped".to_string();
            confirmed.projection.lease_id = "detect-confirmed-capacity".to_string();
            confirmed.detection_lease_cleanup_confirmed = true;
            jobs.insert("confirmed-capacity".to_string(), confirmed);
            let mut empty =
                sample_running_detection_job("empty-capacity", "camera.252", false, None);
            empty.projection.status = "stopped".to_string();
            empty.projection.lease_id.clear();
            jobs.insert("empty-capacity".to_string(), empty);
        }
        let config = DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        };

        let result = api.start_detection_job(config, false);
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(
            result.expect_err("fake HarborLink rejects POST").0,
            StatusCode(502)
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cat_detection_control_capacity_unresolved_attempt_blocks_public_create() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-capacity-attempt-public");
        let _provider = EnvGuard::set("HARBOR_K3_YOLO_PROVIDER", "cpu");
        let lease_ids = (0..63)
            .map(|index| format!("detect-capacity-attempt-public-{index:02}"))
            .collect::<Vec<_>>();
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-capacity-attempt-public",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 34).expect("enabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.clone())
            .expect("63 pending leases");
        policy
            .set_detection_lease_create_attempt(
                Some("capacity-public-unresolved".to_string()),
                Some("sub".to_string()),
            )
            .expect("unresolved create attempt");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        install_unconfirmed_detection_runtimes(&api, "camera.252", &lease_ids);

        let (base_url, admin_server) = spawn_admin_test_server(api, 1);
        let response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("public start response");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cat_detection_control_capacity_unresolved_attempt_replay_uses_reserved_slot() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-capacity-attempt-replay");
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-capacity-attempt-replay");
        let lease_ids = (0..63)
            .map(|index| format!("detect-capacity-attempt-replay-{index:02}"))
            .collect::<Vec<_>>();
        let replayed_lease_id = "detect-capacity-attempt-replayed";
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(vec![DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "200 OK",
                response: detection_lease_response(
                    "camera.252",
                    replayed_lease_id,
                    "running",
                    "sub",
                ),
            }]);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-capacity-attempt-replay",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let attempt_id = "capacity-controlled-unresolved";
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 35).expect("enabled policy");
        policy
            .set_pending_detection_lease_ids(lease_ids.clone())
            .expect("63 pending leases");
        policy
            .set_detection_lease_create_attempt(
                Some(attempt_id.to_string()),
                Some("sub".to_string()),
            )
            .expect("unresolved create attempt");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        install_unconfirmed_detection_runtimes(&api, "camera.252", &lease_ids);
        let config = DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        };

        let started = api
            .start_controlled_detection_job_locked(config, false)
            .expect("same attempt replay uses its existing responsibility slot");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(started.projection.lease_id, replayed_lease_id);
        let observed = requests.lock().expect("requests");
        assert_eq!(observed.len(), 1);
        assert!(test_request_header(&observed[0], "X-Request-Id")
            .is_some_and(|request_id| request_id.contains(attempt_id)));
        drop(observed);
        assert!(api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .is_none());
        cleanup_detection_children(&api);
    }

    #[test]
    fn cat_detection_control_create_reuses_request_id_after_lost_responses_and_restart() {
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-create-idempotency");
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-create-idempotency");
        let revision = 41;
        let policy = CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
            .expect("enabled policy");
        let control_store =
            CatDetectionControlStore::try_new(control_path.clone()).expect("control store");
        control_store.upsert(policy).expect("persist policy");
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_idempotency_replay_server();
        let harborlink = HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client");
        let mut recovered_api = None;

        for attempt in 0..3 {
            let (api, _paths) = build_test_admin_api_with_harborlink(
                &format!("cat-control-create-idempotency-restart-{attempt}"),
                harborlink.clone(),
                &["camera.252"],
            );
            match api.coordinate_cat_detection_control("camera.252", revision) {
                Ok(CatDetectionControlCoordination::Converged) => {
                    recovered_api = Some(api);
                    break;
                }
                Ok(CatDetectionControlCoordination::Draining) => {
                    panic!("enabled policy cannot be draining")
                }
                Ok(CatDetectionControlCoordination::Superseded) => {
                    panic!("stable revision must not be superseded")
                }
                Err(_) => {}
            }
        }
        harborlink_server.join().expect("HarborLink server");
        let api = recovered_api.expect("idempotent replay must recover the created lease");
        let observed = requests.lock().expect("HarborLink requests");
        assert_eq!(observed.len(), 3);
        let request_ids = observed
            .iter()
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<HashSet<_>>();
        assert_eq!(
            request_ids.len(),
            1,
            "request IDs must survive retries/restart"
        );
        drop(observed);
        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .any(|runtime| runtime.projection.lease_id == "detect-idempotency-replay"));
        assert!(CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls")["camera.252"]
            .detection_lease_create_attempt_id
            .is_none());
        cleanup_detection_children(&api);
    }

    #[test]
    fn cat_detection_control_put_true_preserves_unresolved_create_attempt_scope() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-put-true-attempt");
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-put-true-attempt");
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_attempt_resolution_server(1, false);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-put-true-attempt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 71).expect("enabled policy"),
        )
        .expect("persist enabled policy");
        assert!(api
            .coordinate_cat_detection_control("camera.252", 71)
            .is_err());
        let unresolved_attempt = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .expect("unresolved create attempt");

        let response = api
            .apply_cat_detection_control("camera.252", true, "sub")
            .expect("PUT true reconciles prior create attempt");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(response.effective_status, "running");
        let observed = requests.lock().expect("requests");
        let request_ids = observed
            .iter()
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<HashSet<_>>();
        assert_eq!(observed.len(), 2);
        assert_eq!(request_ids.len(), 1);
        assert!(request_ids
            .iter()
            .next()
            .is_some_and(|request_id| request_id.contains(&unresolved_attempt)));
        drop(observed);
        assert!(api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .is_none());
        cleanup_detection_children(&api);
    }

    #[test]
    fn cat_detection_control_replays_same_lease_attempt_before_accepting_public_worker() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) = install_cat_detection_control_test_environment(
            "cat-control-healthy-public-same-attempt",
        );
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-healthy-public-same-attempt");
        let (harborlink_url, requests, harborlink_server) =
            spawn_public_worker_with_unresolved_control_attempt_server("detect-public-healthy");
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-healthy-public-same-attempt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let (base_url, admin_server) = spawn_admin_test_server(api.clone(), 1);
        let public_response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("legacy public start response");
        admin_server.join().expect("admin server");
        assert!(public_response.status().is_success());

        let revision = 91;
        let mut policy = CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
            .expect("enabled policy");
        policy
            .set_detection_lease_create_attempt(
                Some("healthy-public-same-control-attempt".to_string()),
                Some("sub".to_string()),
            )
            .expect("unresolved controlled create attempt");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        let unresolved_attempt = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .expect("unresolved create attempt");

        let unresolved = api
            .cat_detection_control_response("camera.252")
            .expect("control response");
        assert_eq!(unresolved.effective_status, "failed");
        assert!(unresolved
            .message
            .as_deref()
            .is_some_and(|message| message.contains("unresolved")));
        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", revision)
                .expect("same lease replay converges"),
            CatDetectionControlCoordination::Converged
        );
        harborlink_server.join().expect("HarborLink server");

        let observed = requests.lock().expect("requests");
        assert_eq!(observed.len(), 2);
        let request_ids = observed
            .iter()
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<Vec<_>>();
        assert_ne!(request_ids[0], request_ids[1]);
        assert!(request_ids[1].contains(&unresolved_attempt));
        drop(observed);

        let final_policy = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy");
        assert!(final_policy.detection_lease_create_attempt_id.is_none());
        assert!(final_policy.pending_detection_lease_ids.is_empty());
        let jobs = api.detection_jobs.lock().expect("detection jobs");
        let running = jobs
            .values()
            .filter(|runtime| runtime.projection.status == "running")
            .collect::<Vec<_>>();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].projection.lease_id, "detect-public-healthy");
        drop(jobs);
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("settled control response")
                .effective_status,
            "running"
        );
        cleanup_detection_children(&api);
    }

    #[test]
    fn cat_detection_control_cleans_different_attempt_lease_without_stopping_public_worker() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) = install_cat_detection_control_test_environment(
            "cat-control-healthy-public-different-attempt",
        );
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-healthy-public-different-attempt");
        let (harborlink_url, requests, harborlink_server) =
            spawn_public_worker_with_unresolved_control_attempt_server("detect-control-unresolved");
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-healthy-public-different-attempt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let (base_url, admin_server) = spawn_admin_test_server(api.clone(), 1);
        let public_response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("legacy public start response");
        admin_server.join().expect("admin server");
        assert!(public_response.status().is_success());

        let revision = 92;
        let mut policy = CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
            .expect("enabled policy");
        policy
            .set_detection_lease_create_attempt(
                Some("healthy-public-different-control-attempt".to_string()),
                Some("sub".to_string()),
            )
            .expect("unresolved controlled create attempt");
        api.persist_cat_detection_policy(policy)
            .expect("persist enabled policy");
        let unresolved_attempt = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .expect("unresolved create attempt");

        let unresolved = api
            .cat_detection_control_response("camera.252")
            .expect("control response");
        assert_eq!(unresolved.effective_status, "failed");
        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", revision)
                .expect("different lease replay and cleanup converge"),
            CatDetectionControlCoordination::Converged
        );
        harborlink_server.join().expect("HarborLink server");

        let observed = requests.lock().expect("requests");
        assert_eq!(observed.len(), 3);
        let post_request_ids = observed
            .iter()
            .filter(|request| request.starts_with("POST "))
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<Vec<_>>();
        assert_ne!(post_request_ids[0], post_request_ids[1]);
        assert!(post_request_ids[1].contains(&unresolved_attempt));
        assert!(observed[2].starts_with(
            "DELETE /v1/cameras/camera.252/detection-leases/detect-control-unresolved "
        ));
        drop(observed);

        let final_policy = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy");
        assert!(final_policy.detection_lease_create_attempt_id.is_none());
        assert!(final_policy.pending_detection_lease_ids.is_empty());
        let jobs = api.detection_jobs.lock().expect("detection jobs");
        let running = jobs
            .values()
            .filter(|runtime| runtime.projection.status == "running")
            .collect::<Vec<_>>();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].projection.lease_id, "detect-public-healthy");
        drop(jobs);
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("settled control response")
                .effective_status,
            "running"
        );
        cleanup_detection_children(&api);
    }

    #[test]
    fn cat_detection_control_put_false_keeps_unresolved_attempt_until_replay_cleanup() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-put-false-attempt");
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_attempt_resolution_server(2, true);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-put-false-attempt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 81).expect("enabled policy"),
        )
        .expect("persist enabled policy");
        assert!(api
            .coordinate_cat_detection_control("camera.252", 81)
            .is_err());
        let unresolved_attempt = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy")
            .detection_lease_create_attempt_id
            .expect("unresolved create attempt");

        let disabled = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("PUT false persists desired state despite unresolved create");
        assert_eq!(disabled.effective_status, "failed");
        let failed_policy = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy");
        assert!(!failed_policy.desired_enabled);
        assert_eq!(
            failed_policy.detection_lease_create_attempt_id.as_deref(),
            Some(unresolved_attempt.as_str())
        );
        api.cat_detection_control_reconciliation
            .lock()
            .expect("reconciliation")
            .remove("camera.252");
        let recovered_projection = api
            .cat_detection_control_response("camera.252")
            .expect("cold projection remains diagnostic");
        assert_eq!(recovered_projection.effective_status, "failed");
        assert!(recovered_projection
            .message
            .as_deref()
            .is_some_and(|message| message.contains("create result is unresolved")));
        api.cancel_cat_detection_retry_workers_for_test();

        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", failed_policy.updated_at_epoch_ms,)
                .expect("replay resolves and cleans possible lease"),
            CatDetectionControlCoordination::Converged
        );
        harborlink_server.join().expect("HarborLink server");

        let observed = requests.lock().expect("requests");
        let post_request_ids = observed
            .iter()
            .filter(|request| request.starts_with("POST "))
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<HashSet<_>>();
        assert_eq!(observed.len(), 4);
        assert_eq!(post_request_ids.len(), 1);
        drop(observed);
        let final_policy = api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .expect("explicit policy");
        assert!(final_policy.detection_lease_create_attempt_id.is_none());
        assert!(final_policy.pending_detection_lease_ids.is_empty());
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
    }

    #[test]
    fn cat_detection_control_rotates_create_request_id_after_cleanup_and_definite_failure() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-create-attempt-rotation");
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-create-attempt-rotation");
        let lease_id = "detect-attempt-rotation";
        let steps = vec![
            DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "running", "sub"),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            },
            DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "400 Bad Request",
                response: json!({
                    "error": {
                        "code": "INVALID_REQUEST",
                        "message": "confirmed not created",
                        "retryable": false,
                        "dependency": "harborlink"
                    }
                }),
            },
            DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "400 Bad Request",
                response: json!({
                    "error": {
                        "code": "INVALID_REQUEST",
                        "message": "confirmed not created",
                        "retryable": false,
                        "dependency": "harborlink"
                    }
                }),
            },
        ];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-create-attempt-rotation",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let revision = 51;
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
                .expect("enabled policy"),
        )
        .expect("persist policy");

        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", revision)
                .expect("first create converges"),
            CatDetectionControlCoordination::Converged
        );
        api.stop_detection_jobs_for_camera_locked("camera.252")
            .expect("cleanup first lease");
        assert!(api
            .coordinate_cat_detection_control("camera.252", revision)
            .is_err());
        assert!(api
            .coordinate_cat_detection_control("camera.252", revision)
            .is_err());
        harborlink_server.join().expect("HarborLink server");

        let observed = requests.lock().expect("requests");
        let request_ids = observed
            .iter()
            .filter(|request| request.starts_with("POST "))
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<Vec<_>>();
        assert_eq!(request_ids.len(), 3);
        assert_ne!(request_ids[0], request_ids[1]);
        assert_ne!(request_ids[1], request_ids[2]);
        drop(observed);
        cleanup_detection_children(&api);
    }

    #[test]
    fn public_detection_posts_keep_independent_request_scopes() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-public-request-scope");
        let steps = (0..2)
            .map(|_| DetectionLeaseServerStep {
                method: "POST",
                path: "/v1/cameras/camera.252/detection-leases".to_string(),
                request_profile: Some("sub".to_string()),
                status: "400 Bad Request",
                response: json!({"error": "confirmed not created"}),
            })
            .collect();
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-public-request-scope",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let (base_url, admin_server) = spawn_admin_test_server(api.clone(), 2);
        let client = Client::builder().build().expect("HTTP client");
        for _ in 0..2 {
            let response = gate_admin_request(
                &client,
                reqwest::Method::POST,
                format!("{base_url}/api/vision/detection-jobs"),
            )
            .json(&json!({
                "camera_id": "camera.252",
                "target_label": "cat",
                "stream_profile": "sub"
            }))
            .send()
            .expect("start response");
            assert!(
                response.status().is_client_error()
                    || response.status() == reqwest::StatusCode::BAD_GATEWAY
            );
        }
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        let request_ids = requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
            .collect::<Vec<_>>();
        assert_eq!(request_ids.len(), 2);
        assert_ne!(request_ids[0], request_ids[1]);
        assert!(api
            .cat_detection_explicit_policy("camera.252")
            .expect("policy")
            .is_none());
    }

    #[test]
    fn cat_detection_control_retries_lease_cleanup_after_worker_initialization_failure() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) = install_cat_detection_control_test_environment(
            "cat-control-worker-initialization-cleanup",
        );
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("worker-initialization-cleanup");
        let bad_python = EnvGuard::set(
            "HARBOR_K3_YOLO_PYTHON",
            "harborbeacon-definitely-missing-python",
        );
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_initialization_recovery_server(
                DetectionInitializationCleanupFailure::ErrorResponse,
            );
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-worker-initialization-cleanup",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let revision = 21;
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
                .expect("enabled policy"),
        )
        .expect("persist enabled policy");
        let config = DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        };

        let (_, error) = api
            .start_detection_job(config, false)
            .expect_err("worker initialization must fail");

        assert!(error.contains("failed to start detection worker"));
        let persisted = CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .load()
            .expect("controls");
        assert_eq!(
            persisted["camera.252"].pending_detection_lease_ids,
            vec!["detect-initialization-failed".to_string()]
        );
        let failed = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .find(|runtime| runtime.projection.lease_id == "detect-initialization-failed")
            .expect("failed initialization runtime")
            .projection
            .clone();
        assert_eq!(failed.status, "failed");
        assert!(failed
            .message
            .as_deref()
            .is_some_and(|message| message.contains("cleanup was incomplete")));
        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        drop(bad_python);

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && !api
                .detection_jobs
                .lock()
                .expect("detection jobs")
                .values()
                .any(|runtime| {
                    runtime.projection.lease_id == "detect-initialization-recovered"
                        && runtime.projection.status == "running"
                })
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(requests.lock().expect("requests").len(), 4);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .any(|runtime| {
                runtime.projection.lease_id == "detect-initialization-recovered"
                    && runtime.projection.status == "running"
            }));
        cleanup_detection_children(&api);
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn detection_create_success_is_persisted_before_local_initialization_cleanup() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-create-transfer");
        let (_output_root, _worker_env) =
            install_sleeping_detection_worker("cat-control-create-transfer");
        let _bad_python = EnvGuard::set(
            "HARBOR_K3_YOLO_PYTHON",
            "harborbeacon-definitely-missing-python",
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let address = listener.local_addr().expect("HarborLink address");
        let (delete_seen_sender, delete_seen_receiver) = sync_channel(1);
        let (release_delete_sender, release_delete_receiver) = sync_channel(1);
        let server = thread::spawn(move || {
            let (mut create, _) = listener.accept().expect("create request");
            let request = read_test_http_request(&mut create);
            assert!(request.starts_with("POST /v1/cameras/camera.252/detection-leases "));
            let body = serde_json::to_string(&detection_lease_response(
                "camera.252",
                "detect-durable-transfer",
                "running",
                "sub",
            ))
            .expect("lease json");
            create
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("create response");

            let (mut cleanup, _) = listener.accept().expect("cleanup request");
            let request = read_test_http_request(&mut cleanup);
            assert!(request.starts_with(
                "DELETE /v1/cameras/camera.252/detection-leases/detect-durable-transfer "
            ));
            delete_seen_sender.send(()).expect("delete observed");
            release_delete_receiver.recv().expect("release delete response");
            let body = r#"{"error":{"code":"CLEANUP_FAILED","message":"cleanup failed","retryable":true,"dependency":"harborlink"}}"#;
            cleanup
                .write_all(
                    format!(
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("cleanup response");
        });
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-create-transfer",
            HarborLinkMediaClient::new(format!("http://{address}")).expect("HarborLink client"),
            &["camera.252"],
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 101)
                .expect("enabled policy"),
        )
        .expect("persist policy");
        let start_api = api.clone();
        let start = thread::spawn(move || {
            start_api.start_controlled_detection_job_locked(
                DetectionJobConfig {
                    camera_id: "camera.252".to_string(),
                    target_label: "cat".to_string(),
                    ttl_seconds: 300,
                    max_fps: 5.0,
                    confidence: 0.35,
                    stream_profile: "sub".to_string(),
                },
                false,
            )
        });
        delete_seen_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("cleanup request observed");
        let persisted_before_cleanup = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls")["camera.252"]
            .clone();
        release_delete_sender.send(()).expect("release cleanup");
        assert!(start.join().expect("start thread").is_err());
        server.join().expect("HarborLink server");

        assert_eq!(
            persisted_before_cleanup.pending_detection_lease_ids,
            vec!["detect-durable-transfer".to_string()]
        );
        assert!(persisted_before_cleanup
            .detection_lease_create_attempt_id
            .is_none());
    }

    #[test]
    fn detection_create_errors_reuse_attempt_unless_not_created_is_confirmed() {
        use super::super::DetectionLeaseCreateFailureDisposition as Disposition;

        let contract_error = |status_code, code: &str, retryable| {
            serde_json::to_string(&json!({
                "statusCode": status_code,
                "code": code,
                "message": "redacted",
                "retryable": retryable,
                "dependency": "harborlink"
            }))
            .expect("contract error")
        };

        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                503,
                "HARBORLINK_UNAVAILABLE",
                true,
            )),
            Disposition::RetrySameAttempt
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                409,
                "REQUEST_IN_PROGRESS",
                true,
            )),
            Disposition::RetrySameAttempt
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                409,
                "DETECTION_LEASE_ACTIVE",
                false,
            )),
            Disposition::Conflict
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                409,
                "REQUEST_ID_CONFLICT",
                false,
            )),
            Disposition::Conflict
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                404,
                "CAMERA_NOT_FOUND",
                false,
            )),
            Disposition::ConfirmedNotCreated
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                400,
                "INVALID_STREAM_PROFILE",
                false,
            )),
            Disposition::ConfirmedNotCreated
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                404,
                "HARBORLINK_UNAVAILABLE",
                false,
            )),
            Disposition::RetrySameAttempt
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(&contract_error(
                502,
                "HARBORLINK_INVALID_RESPONSE",
                false,
            )),
            Disposition::RetrySameAttempt
        );
        assert_eq!(
            super::super::classify_detection_lease_create_failure(
                "HarborLink did not prepare the requested detection pre-roll"
            ),
            Disposition::RetrySameAttempt
        );
    }

    #[derive(Clone, Copy)]
    enum InvalidDetectionLeaseResponse {
        EmptyLeaseId,
        MismatchedCamera,
        MismatchedProfile,
        InvalidStatus,
    }

    fn spawn_invalid_then_valid_detection_create_server(
        invalid_body: bool,
        invalid_pre_roll: bool,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let address = listener.local_addr().expect("HarborLink address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().expect("detection create request");
                let request = read_test_http_request(&mut stream);
                server_requests.lock().expect("requests").push(request);
                let body = if index == 0 && invalid_body {
                    "not-json".to_string()
                } else {
                    let mut lease = detection_lease_response(
                        "camera.252",
                        "detect-idempotent-response",
                        "running",
                        "sub",
                    );
                    if index == 0 && invalid_pre_roll {
                        lease["pre_roll_ready"] = Value::Bool(false);
                    }
                    lease.to_string()
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        )
                        .as_bytes(),
                    )
                    .expect("detection create response");
            }
        });
        (format!("http://{address}"), requests, server)
    }

    fn spawn_invalid_detection_create_response_server(
        fault: InvalidDetectionLeaseResponse,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HarborLink listener");
        let address = listener.local_addr().expect("HarborLink address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("detection create request");
            let request = read_test_http_request(&mut stream);
            assert!(request.starts_with("POST /v1/cameras/camera.252/detection-leases "));
            let mut lease = detection_lease_response(
                "camera.252",
                "detect-invalid-ownership",
                "running",
                "sub",
            );
            match fault {
                InvalidDetectionLeaseResponse::EmptyLeaseId => lease["lease_id"] = json!(""),
                InvalidDetectionLeaseResponse::MismatchedCamera => {
                    lease["camera_id"] = json!("camera.other")
                }
                InvalidDetectionLeaseResponse::MismatchedProfile => {
                    lease["stream_profile"] = json!("main")
                }
                InvalidDetectionLeaseResponse::InvalidStatus => {
                    lease["status"] = json!("stopped")
                }
            }
            let body = lease.to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .expect("detection create response");
        });
        (format!("http://{address}"), server)
    }

    fn controlled_detection_config() -> DetectionJobConfig {
        DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        }
    }

    #[test]
    fn controlled_create_reuses_attempt_after_invalid_body_and_pre_roll_response() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        for (suffix, invalid_body, invalid_pre_roll) in
            [("body", true, false), ("pre-roll", false, true)]
        {
            let (_control_path, _control_env) = install_cat_detection_control_test_environment(
                &format!("cat-control-invalid-create-{suffix}"),
            );
            let (_output_root, _worker_env) =
                install_sleeping_detection_worker(&format!("invalid-create-{suffix}"));
            let (harborlink_url, requests, server) =
                spawn_invalid_then_valid_detection_create_server(invalid_body, invalid_pre_roll);
            let (api, _paths) = build_test_admin_api_with_harborlink(
                &format!("cat-control-invalid-create-{suffix}"),
                HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
                &["camera.252"],
            );
            api.persist_cat_detection_policy(
                CatDetectionControlPolicy::new("camera.252", true, "sub", 201)
                    .expect("enabled policy"),
            )
            .expect("persist policy");

            api.start_controlled_detection_job_locked(controlled_detection_config(), false)
                .expect_err("first response is uncertain");
            let unresolved_attempt = api
                .cat_detection_explicit_policy("camera.252")
                .expect("policy")
                .expect("explicit policy")
                .detection_lease_create_attempt_id
                .expect("uncertain response retains attempt");
            api.start_controlled_detection_job_locked(controlled_detection_config(), false)
                .expect("same-attempt replay succeeds");
            server.join().expect("HarborLink server");

            let request_ids = requests
                .lock()
                .expect("requests")
                .iter()
                .map(|request| test_request_header(request, "X-Request-Id").expect("request id"))
                .collect::<Vec<_>>();
            assert_eq!(request_ids.len(), 2);
            assert_eq!(request_ids[0], request_ids[1]);
            assert!(request_ids[0].contains(&unresolved_attempt));
            let policy = api
                .cat_detection_explicit_policy("camera.252")
                .expect("policy")
                .expect("explicit policy");
            assert!(policy.detection_lease_create_attempt_id.is_none());
            assert!(policy.pending_detection_lease_ids.is_empty());
            assert_eq!(
                api.detection_jobs
                    .lock()
                    .expect("jobs")
                    .values()
                    .filter(|runtime| runtime.projection.status == "running")
                    .count(),
                1
            );
            cleanup_detection_children(&api);
        }
    }

    #[test]
    fn controlled_create_rejects_untrusted_lease_identity_before_ownership_or_worker() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        for (index, fault) in [
            InvalidDetectionLeaseResponse::EmptyLeaseId,
            InvalidDetectionLeaseResponse::MismatchedCamera,
            InvalidDetectionLeaseResponse::MismatchedProfile,
            InvalidDetectionLeaseResponse::InvalidStatus,
        ]
        .into_iter()
        .enumerate()
        {
            let (_control_path, _control_env) = install_cat_detection_control_test_environment(
                &format!("cat-control-invalid-identity-{index}"),
            );
            let (_output_root, _worker_env) =
                install_sleeping_detection_worker(&format!("invalid-identity-{index}"));
            let (harborlink_url, server) = spawn_invalid_detection_create_response_server(fault);
            let (api, _paths) = build_test_admin_api_with_harborlink(
                &format!("cat-control-invalid-identity-{index}"),
                HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
                &["camera.252"],
            );
            api.persist_cat_detection_policy(
                CatDetectionControlPolicy::new("camera.252", true, "sub", 301 + index as u128)
                    .expect("enabled policy"),
            )
            .expect("persist policy");

            api.start_controlled_detection_job_locked(controlled_detection_config(), false)
                .expect_err("untrusted response must not start a worker");
            server.join().expect("HarborLink server");

            let policy = api
                .cat_detection_explicit_policy("camera.252")
                .expect("policy")
                .expect("explicit policy");
            assert!(policy.detection_lease_create_attempt_id.is_some());
            assert!(policy.pending_detection_lease_ids.is_empty());
            assert!(api.detection_jobs.lock().expect("jobs").is_empty());
        }
    }

    #[test]
    fn cat_detection_control_retries_lost_cleanup_response_after_output_initialization_failure() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) = install_cat_detection_control_test_environment(
            "cat-control-output-initialization-cleanup",
        );
        let (output_root, _worker_env) =
            install_sleeping_detection_worker("output-initialization-cleanup");
        let blocked_output_root = output_root.join("blocked-output-root");
        fs::write(&blocked_output_root, b"not a directory").expect("block output root");
        let bad_output = EnvGuard::set(
            "HARBOR_K3_YOLO_OUTPUT_ROOT",
            blocked_output_root.to_str().expect("UTF-8 output root"),
        );
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_initialization_recovery_server(
                DetectionInitializationCleanupFailure::LostResponse,
            );
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-output-initialization-cleanup",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let revision = 22;
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", revision)
                .expect("enabled policy"),
        )
        .expect("persist enabled policy");
        let config = DetectionJobConfig {
            camera_id: "camera.252".to_string(),
            target_label: "cat".to_string(),
            ttl_seconds: 300,
            max_fps: 5.0,
            confidence: 0.35,
            stream_profile: "sub".to_string(),
        };

        let (_, error) = api
            .start_detection_job(config, false)
            .expect_err("output initialization must fail");

        assert!(error.contains("failed to create detection output directory"));
        let persisted = CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .load()
            .expect("controls");
        assert_eq!(
            persisted["camera.252"].pending_detection_lease_ids,
            vec!["detect-initialization-failed".to_string()]
        );
        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .any(|runtime| {
                runtime.projection.lease_id == "detect-initialization-failed"
                    && runtime.projection.status == "failed"
            }));
        drop(bad_output);

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && !api
                .detection_jobs
                .lock()
                .expect("detection jobs")
                .values()
                .any(|runtime| {
                    runtime.projection.lease_id == "detect-initialization-recovered"
                        && runtime.projection.status == "running"
                })
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(requests.lock().expect("requests").len(), 4);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        assert!(api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .values()
            .any(|runtime| {
                runtime.projection.lease_id == "detect-initialization-recovered"
                    && runtime.projection.status == "running"
            }));
        cleanup_detection_children(&api);
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn cat_detection_control_prune_retains_unconfirmed_lease_for_disable_cleanup() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-prune-unconfirmed");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "detect-prune-unconfirmed";
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(vec![DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            }]);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-prune-unconfirmed",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut jobs = api.detection_jobs.lock().expect("detection jobs");
        let mut target =
            sample_running_detection_job("prune-unconfirmed", "camera.252", false, None);
        target.projection.status = "stopped".to_string();
        target.projection.lease_id = lease_id.to_string();
        target.projection.updated_at = "2000-01-01T00:00:00Z".to_string();
        target.detection_lease_cleanup_confirmed = false;
        jobs.insert(target.projection.job_id.clone(), target);
        for index in 0..MAX_DETECTION_JOB_HISTORY {
            let job_id = format!("prune-confirmed-{index}");
            let mut runtime = sample_running_detection_job(
                &job_id,
                &format!("other-camera-{index}"),
                false,
                None,
            );
            runtime.projection.status = "completed".to_string();
            runtime.projection.updated_at = format!("2026-08-18T00:{:02}:00Z", index % 60);
            runtime.detection_lease_cleanup_confirmed = true;
            jobs.insert(job_id, runtime);
        }

        prune_detection_job_history(&mut jobs);

        assert!(jobs.contains_key("prune-unconfirmed"));
        drop(jobs);
        let response = api
            .apply_cat_detection_control("camera.252", false, "sub")
            .expect("disable cleans retained lease");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(response.effective_status, "stopped");
        assert_eq!(requests.lock().expect("requests").len(), 1);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        let target = api
            .detection_jobs
            .lock()
            .expect("detection jobs")
            .get("prune-unconfirmed")
            .expect("retained runtime")
            .projection
            .clone();
        assert_eq!(target.status, "stopped");
        assert!(target.message.is_none());
    }

    #[test]
    fn cat_detection_control_recovery_retries_persisted_disabled_lease() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-disabled-recovery");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "detect-disabled-recovery";
        let mut policy =
            CatDetectionControlPolicy::new("camera.252", false, "sub", 8).expect("disabled policy");
        policy
            .set_pending_detection_lease_ids(vec![lease_id.to_string()])
            .expect("pending lease");
        CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .upsert(policy)
            .expect("persist disabled policy");
        let steps = vec![
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "500 Internal Server Error",
                response: json!({"error": "temporary delete failure"}),
            },
            DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            },
        ];
        let (harborlink_url, requests, harborlink_server) =
            spawn_detection_lease_sequence_server(steps);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-disabled-recovery",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        api.start_cat_detection_control_recovery();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && !CatDetectionControlStore::try_new(control_path.clone())
                .expect("control store")
                .load()
                .expect("controls")["camera.252"]
                .pending_detection_lease_ids
                .is_empty()
        {
            thread::sleep(Duration::from_millis(20));
        }
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(requests.lock().expect("requests").len(), 2);
        let persisted = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("controls");
        assert!(persisted["camera.252"]
            .pending_detection_lease_ids
            .is_empty());
        assert_eq!(
            api.cat_detection_control_response("camera.252")
                .expect("control response")
                .effective_status,
            "stopped"
        );
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn public_detection_job_post_rejects_unresolved_rollback_before_harborlink() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-public-post-rollback");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-public-post-rollback",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let mut policy = CatDetectionControlPolicy::new("camera.252", true, "main", 95)
            .expect("enabled policy");
        policy
            .set_rollback_detection_lease_create_attempt(
                Some("rollback-public-post-block".to_string()),
                Some("sub".to_string()),
            )
            .expect("rollback marker");
        api.persist_cat_detection_policy(policy.clone())
            .expect("persist rollback marker");

        let (base_url, admin_server) = spawn_admin_test_server(api.clone(), 2);
        let client = Client::builder().build().expect("HTTP client");
        let unresolved = gate_admin_request(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("unresolved rollback response");
        assert_eq!(unresolved.status(), reqwest::StatusCode::CONFLICT);
        let unresolved_body = unresolved.json::<Value>().expect("rollback error body");
        assert_eq!(
            unresolved_body["error"]["code"],
            "CAT_DETECTION_ROLLBACK_UNRESOLVED"
        );
        assert_eq!(
            unresolved_body["error"]["message"],
            "Cat detection profile rollback is unresolved for this camera."
        );

        policy
            .set_rollback_detection_lease_create_attempt(None, None)
            .expect("clear rollback marker");
        api.persist_cat_detection_policy(policy)
            .expect("persist cleared rollback marker");
        let controlled = gate_admin_request(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("controlled ownership response");
        assert_eq!(controlled.status(), reqwest::StatusCode::CONFLICT);

        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn public_detection_job_post_is_blocked_by_explicit_disable() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-public-post-disabled");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .upsert(
                CatDetectionControlPolicy::new("camera.252", false, "sub", 9)
                    .expect("disabled policy"),
            )
            .expect("persist policy");
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-public-post-disabled",
            HarborLinkMediaClient::new("http://127.0.0.1:9").expect("HarborLink client"),
            &["camera.252"],
        );
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);
        let response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("start response");
        admin_server.join().expect("admin server");

        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    }

    #[test]
    fn public_detection_job_post_serializes_with_camera_control() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-public-post-lock");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let (api, _paths) = build_test_admin_api("cat-control-public-post-lock");
        save_test_camera(&api, "camera.252");
        api.detection_jobs.lock().expect("detection jobs").insert(
            "existing-post-worker".to_string(),
            sample_running_detection_job("existing-post-worker", "camera.252", false, None),
        );
        let camera_lock = api
            .cat_detection_control_camera_lock("camera.252")
            .expect("camera lock");
        let camera_guard = camera_lock.lock().expect("hold camera lock");
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);
        let (result_sender, result_receiver) = sync_channel(1);
        let request = thread::spawn(move || {
            let response = gate_admin_request(
                &Client::builder().build().expect("HTTP client"),
                reqwest::Method::POST,
                format!("{base_url}/api/vision/detection-jobs"),
            )
            .json(&json!({
                "camera_id": "camera.252",
                "target_label": "cat",
                "stream_profile": "sub"
            }))
            .send()
            .expect("start response");
            result_sender.send(response.status()).expect("send status");
        });

        assert!(matches!(
            result_receiver.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(camera_guard);
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("response after control lock release"),
            reqwest::StatusCode::OK
        );
        request.join().expect("request thread");
        admin_server.join().expect("admin server");
    }

    #[test]
    fn public_detection_renew_is_blocked_by_explicit_disable_without_harborlink_request() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-renew-disabled");
        CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .upsert(
                CatDetectionControlPolicy::new("camera.252", false, "sub", 91)
                    .expect("disabled policy"),
            )
            .expect("persist disabled policy");
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-renew-disabled",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let job_id = "yolo-renew-disabled";
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(job_id, "camera.252", false, None),
        );
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);

        let response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs/{job_id}/renew"),
        )
        .json(&json!({"ttl_seconds": 300}))
        .send()
        .expect("renew response");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn public_detection_renew_corrupt_store_is_redacted_and_has_no_side_effect() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-renew-corrupt");
        fs::write(&control_path, b"{not-valid-json").expect("corrupt control store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&control_path, fs::Permissions::from_mode(0o600))
                .expect("secure control store permissions");
        }
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-renew-corrupt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let job_id = "yolo-renew-corrupt";
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(job_id, "camera.252", false, None),
        );
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);

        let response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs/{job_id}/renew"),
        )
        .json(&json!({"ttl_seconds": 300}))
        .send()
        .expect("renew response");
        let status = response.status();
        let body: Value = response.json().expect("renew error json");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "CAT_DETECTION_CONTROL_UNAVAILABLE");
        let public_body = body.to_string();
        assert!(!public_body.contains("store is invalid"));
        assert!(!public_body.contains(&control_path.to_string_lossy().to_string()));
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn public_detection_renew_waits_for_camera_control_and_observes_persisted_false() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-renew-lock");
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-renew-lock",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 101).expect("enabled policy"),
        )
        .expect("persist enabled policy");
        let job_id = "yolo-renew-lock";
        api.detection_jobs.lock().expect("detection jobs").insert(
            job_id.to_string(),
            sample_running_detection_job(job_id, "camera.252", false, None),
        );
        let camera_lock = api
            .cat_detection_control_camera_lock("camera.252")
            .expect("camera lock");
        let camera_guard = camera_lock.lock().expect("hold camera lock");
        let (base_url, admin_server) = spawn_admin_test_server(api.clone(), 1);
        let (status_sender, status_receiver) = sync_channel(1);
        let request = thread::spawn(move || {
            let response = gate_admin_request(
                &Client::builder().build().expect("HTTP client"),
                reqwest::Method::POST,
                format!("{base_url}/api/vision/detection-jobs/{job_id}/renew"),
            )
            .json(&json!({"ttl_seconds": 300}))
            .send()
            .expect("renew response");
            status_sender.send(response.status()).expect("renew status");
        });

        assert!(matches!(
            status_receiver.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 102)
                .expect("disabled policy"),
        )
        .expect("persist disabled policy while control lock is held");
        drop(camera_guard);
        assert_eq!(
            status_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("renew completes after camera control"),
            reqwest::StatusCode::CONFLICT
        );
        request.join().expect("renew request");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn put_live_and_auto_recording_share_camera_coordination() {
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-entry-locks");
        let (api, _paths) = build_test_admin_api("cat-control-entry-locks");
        save_test_camera(&api, "camera.252");
        let policy = CatDetectionControlPolicy::new("camera.252", false, "sub", 13)
            .expect("disabled policy");
        api.cat_detection_control_store
            .as_ref()
            .expect("control store")
            .upsert(policy.clone())
            .expect("persist policy");
        api.cat_detection_controls
            .lock()
            .expect("controls")
            .insert("camera.252".to_string(), policy);
        let camera_lock = api
            .cat_detection_control_camera_lock("camera.252")
            .expect("camera lock");
        let camera_guard = camera_lock.lock().expect("hold camera lock");
        let barrier = Arc::new(Barrier::new(4));
        let (done_sender, done_receiver) = sync_channel(3);
        let mut workers = Vec::new();

        {
            let api = api.clone();
            let barrier = barrier.clone();
            let done_sender = done_sender.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                let _ = api.apply_cat_detection_control("camera.252", false, "sub");
                done_sender.send("put").expect("PUT completion");
            }));
        }
        {
            let api = api.clone();
            let barrier = barrier.clone();
            let done_sender = done_sender.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                let _ = api.ensure_live_managed_detection_job("camera.252", "sub");
                done_sender.send("live").expect("live completion");
            }));
        }
        {
            let api = api.clone();
            let barrier = barrier.clone();
            let done_sender = done_sender.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                let _ = api.process_cat_detection_result(
                    "camera.252",
                    "sub",
                    None,
                    CatAutoRecordingConfig {
                        start_consecutive_frames: 1,
                        start_duration_ms: 0,
                        stop_consecutive_frames: 1,
                        stop_duration_ms: 0,
                    },
                );
                done_sender.send("auto").expect("auto completion");
            }));
        }

        barrier.wait();
        assert!(matches!(
            done_receiver.recv_timeout(Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(camera_guard);
        let completed = (0..3)
            .map(|_| {
                done_receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("entry completes after lock release")
            })
            .collect::<HashSet<_>>();
        for worker in workers {
            worker.join().expect("entry worker");
        }

        assert_eq!(completed, HashSet::from(["put", "live", "auto"]));
    }

    #[test]
    fn cat_detection_control_get_refreshes_expired_runtime_and_schedules_retry() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-get-drift");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let lease_id = "detect-get-drift";
        let (harborlink_url, _requests, harborlink_server) =
            spawn_detection_lease_sequence_server(vec![DetectionLeaseServerStep {
                method: "DELETE",
                path: format!("/v1/cameras/camera.252/detection-leases/{lease_id}"),
                request_profile: None,
                status: "200 OK",
                response: detection_lease_response("camera.252", lease_id, "stopped", "sub"),
            }]);
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-get-drift",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let api = api.with_cat_detection_retry_scheduler_for_test();
        let policy =
            CatDetectionControlPolicy::new("camera.252", true, "sub", 10).expect("enabled policy");
        CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .upsert(policy.clone())
            .expect("persist policy");
        api.cat_detection_controls
            .lock()
            .expect("controls")
            .insert("camera.252".to_string(), policy);
        let mut expired = sample_running_detection_job("get-drift", "camera.252", false, None);
        expired.projection.lease_id = lease_id.to_string();
        expired.projection.expires_at = "2000-01-01T00:00:00Z".to_string();
        api.detection_jobs
            .lock()
            .expect("detection jobs")
            .insert(expired.projection.job_id.clone(), expired);
        api.set_cat_detection_reconciliation("camera.252", "running", None)
            .expect("cached reconciliation");
        let api_state = api.clone();
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);
        let response = gate_admin_request(
            &Client::builder().build().expect("HTTP client"),
            reqwest::Method::GET,
            format!("{base_url}/api/cameras/camera.252/cat-detection/control"),
        )
        .send()
        .expect("control response");
        let status = response.status();
        let body: Value = response.json().expect("control json");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body["effective_status"], "failed");
        assert!(body["job_id"].is_null());
        assert!(api_state.cat_detection_retry_contains_camera_for_test("camera.252"));
        api_state.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(api_state.cat_detection_retry_worker_count_for_test(), 0);
    }

    #[test]
    fn stale_control_revision_cannot_overwrite_newer_state() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-stale-revision");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let (api, _paths) = build_test_admin_api("cat-control-stale-revision");
        save_test_camera(&api, "camera.252");
        let policy =
            CatDetectionControlPolicy::new("camera.252", false, "sub", 12).expect("newer policy");
        api.cat_detection_control_store
            .as_ref()
            .expect("control store")
            .upsert(policy.clone())
            .expect("persist policy");
        api.cat_detection_controls
            .lock()
            .expect("controls")
            .insert("camera.252".to_string(), policy);
        api.set_cat_detection_reconciliation("camera.252", "stopping", None)
            .expect("newer state");

        assert_eq!(
            api.coordinate_cat_detection_control("camera.252", 11)
                .expect("stale revision is ignored"),
            CatDetectionControlCoordination::Superseded
        );
        assert_eq!(
            api.cat_detection_control_reconciliation
                .lock()
                .expect("reconciliation")
                .get("camera.252")
                .expect("state")
                .effective_status,
            "stopping"
        );
    }

    #[test]
    fn stale_retry_enqueue_cannot_downgrade_a_newer_pending_revision() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-retry-revision-race");
        let (api, _paths) = build_test_admin_api("cat-control-retry-revision-race");
        let api = api.with_cat_detection_retry_scheduler_for_test();
        save_test_camera(&api, "camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 22)
                .expect("newer disabled policy"),
        )
        .expect("persist newer policy");

        api.schedule_cat_detection_control_retry("camera.252", 22);
        api.schedule_cat_detection_control_retry("camera.252", 21);

        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        assert!(api.cat_detection_retry_queue_len_for_test() <= 1);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api.cat_detection_retry_contains_camera_for_test("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
        assert_eq!(
            api.cat_detection_control_reconciliation
                .lock()
                .expect("reconciliation")["camera.252"]
                .effective_status,
            "stopped"
        );
        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(api.cat_detection_retry_worker_count_for_test(), 0);
    }

    #[test]
    fn cat_detection_retry_delay_is_bounded_and_worker_is_unique() {
        let mut delay = super::super::CAT_DETECTION_CONTROL_RETRY_INITIAL_SECONDS;
        for _ in 0..16 {
            delay = super::super::next_cat_detection_control_retry_delay(delay);
        }
        assert_eq!(
            delay,
            super::super::CAT_DETECTION_CONTROL_RETRY_MAX_SECONDS
        );

        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-unique-retry");
        let _auto_disabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "false");
        let (api, _paths) = build_test_admin_api("cat-control-unique-retry");
        let api = api.with_cat_detection_retry_scheduler_for_test();
        save_test_camera(&api, "camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 2).expect("disabled policy"),
        )
        .expect("persist disabled policy");
        for _ in 0..128 {
            api.schedule_cat_detection_control_retry("camera.252", 2);
        }
        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        assert!(api.cat_detection_retry_queue_len_for_test() <= 1);
        assert_eq!(api.cat_detection_retry_worker_count_for_test(), 2);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api.cat_detection_retry_contains_camera_for_test("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
        assert_eq!(api.cat_detection_retry_active_jobs_for_test(), 0);

        api.schedule_cat_detection_control_retry("camera.252", 2);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api.cat_detection_retry_contains_camera_for_test("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(api.cat_detection_retry_worker_count_for_test(), 0);
    }

    #[test]
    fn cat_detection_retry_worker_exits_when_revision_is_superseded() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-stale-retry-exit");
        let (api, _paths) = build_test_admin_api("cat-control-stale-retry-exit");
        let api = api.with_cat_detection_retry_scheduler_for_test();
        save_test_camera(&api, "camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 2).expect("disabled policy"),
        )
        .expect("persist disabled policy");

        api.schedule_cat_detection_control_retry("camera.252", 1);
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));
        api.cancel_cat_detection_retry_workers_for_test();
    }

    #[test]
    fn pending_real_admin_retry_task_does_not_keep_scheduler_alive() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-pending-drop");
        let (api, _paths) = build_test_admin_api("cat-control-pending-drop");
        let api = api.with_cat_detection_retry_scheduler_config_for_test(
            super::super::CatDetectionRetrySchedulerConfig {
                worker_count: 1,
                capacity: 8,
                initial_delay: Duration::from_secs(30),
                max_delay: Duration::from_secs(30),
            },
        );
        save_test_camera(&api, "camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 901)
                .expect("enabled policy"),
        )
        .expect("persist policy");
        let probe = api.cat_detection_retry_scheduler_probe_for_test();

        api.enqueue_cat_detection_control_retry(
            "camera.252",
            901,
            Duration::from_secs(30),
        );
        assert_eq!(probe.pending_jobs(), 1);
        let owners_after_enqueue = probe.outer_owners();
        drop(api);
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.is_alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(owners_after_enqueue, 1, "retry task captured scheduler owner");
        assert!(!probe.is_alive(), "pending retry kept scheduler pool alive");
        assert_eq!(probe.worker_count(), 0);
        assert_eq!(probe.pending_jobs(), 0);
    }

    #[test]
    fn active_real_admin_retry_task_releases_when_last_external_owner_drops() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-active-drop");
        let (api, _paths) = build_test_admin_api("cat-control-active-drop");
        let api = api.with_cat_detection_retry_scheduler_config_for_test(
            super::super::CatDetectionRetrySchedulerConfig {
                worker_count: 1,
                capacity: 8,
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(10),
            },
        );
        save_test_camera(&api, "camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", true, "sub", 902)
                .expect("enabled policy"),
        )
        .expect("persist policy");
        let camera_lock = api
            .cat_detection_control_camera_lock("camera.252")
            .expect("camera lock");
        let camera_guard = camera_lock.lock().expect("camera guard");
        let probe = api.cat_detection_retry_scheduler_probe_for_test();
        api.enqueue_cat_detection_control_retry("camera.252", 902, Duration::ZERO);
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.active_jobs() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(probe.active_jobs(), 1, "real retry task did not become active");
        let owners_while_active = probe.outer_owners();

        let (dropped_sender, dropped_receiver) = sync_channel(1);
        let dropping = thread::spawn(move || {
            drop(api);
            dropped_sender.send(()).expect("drop completion");
        });
        let dropped_before_release = dropped_receiver
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        drop(camera_guard);
        dropped_receiver
            .recv_timeout(Duration::from_secs(2))
            .ok();
        dropping.join().expect("drop thread");
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.is_alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(owners_while_active, 1, "active task captured scheduler owner");
        assert!(!dropped_before_release, "last owner did not join active worker");
        assert!(!probe.is_alive(), "active retry kept scheduler pool alive");
        assert_eq!(probe.worker_count(), 0);
        assert_eq!(probe.pending_jobs(), 0);
    }

    #[test]
    fn retry_execution_context_defers_nested_schedule_to_worker_outcome() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-detached-retry");
        let (api, _paths) = build_test_admin_api("cat-control-detached-retry");
        let api = api.with_cat_detection_retry_scheduler_config_for_test(
            super::super::CatDetectionRetrySchedulerConfig {
                worker_count: 1,
                capacity: 8,
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(10),
            },
        );
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("missing-camera", true, "sub", 903)
                .expect("enabled policy"),
        )
        .expect("persist policy");
        let probe = api.cat_detection_retry_scheduler_probe_for_test();
        let execution = super::super::CatDetectionRetryExecutionContext::new(&api);

        assert_eq!(probe.outer_owners(), 1);
        execution
            .api
            .schedule_cat_detection_control_retry("missing-camera", 903);
        assert_eq!(probe.pending_jobs(), 0, "nested retry scheduled recursively");
        assert_eq!(
            execution.execute(super::super::CatDetectionRetryEntry {
                camera_id: "missing-camera".to_string(),
                revision: 903,
                generation: 1,
            }),
            super::super::CatDetectionRetryOutcome::Retry
        );
        assert_eq!(probe.pending_jobs(), 0, "worker outcome did not own requeue");

        drop(execution);
        drop(api);
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.is_alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!probe.is_alive());
    }

    #[test]
    fn cancelled_retry_worker_cannot_remove_replacement_generation() {
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-retry-generation");
        let (api, _paths) = build_test_admin_api("cat-control-retry-generation");
        let api = api.with_cat_detection_retry_scheduler_for_test();
        save_test_camera(&api, "camera.252");

        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 1)
                .expect("first policy revision"),
        )
        .expect("persist first policy revision");

        api.schedule_cat_detection_control_retry("camera.252", 1);
        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        api.idle_cat_detection_control_retry("camera.252");
        api.persist_cat_detection_policy(
            CatDetectionControlPolicy::new("camera.252", false, "sub", 2)
                .expect("replacement policy revision"),
        )
        .expect("persist replacement policy revision");
        api.schedule_cat_detection_control_retry("camera.252", 2);
        assert!(api.cat_detection_retry_contains_camera_for_test("camera.252"));
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && api.cat_detection_retry_contains_camera_for_test("camera.252")
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!api.cat_detection_retry_contains_camera_for_test("camera.252"));

        api.cancel_cat_detection_retry_workers_for_test();
        assert_eq!(api.cat_detection_retry_worker_count_for_test(), 0);
    }

    #[test]
    fn cat_detection_control_rejects_invalid_camera_profile_tail_and_device_principal() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (_control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-rejections");
        let (api, _paths) = build_test_admin_api("cat-control-rejections");
        save_test_camera(&api, "camera/252");
        let (base_url, admin_server) = spawn_admin_test_server(api, 5);
        let client = Client::builder().build().expect("HTTP client");
        let known_url = format!("{base_url}/api/cameras/camera%2F252/cat-detection/control");

        let invalid_profile = gate_admin_request(&client, reqwest::Method::PUT, known_url.clone())
            .json(&json!({"enabled": true, "stream_profile": "third"}))
            .send()
            .expect("invalid profile response");
        let normalized_profile =
            gate_admin_request(&client, reqwest::Method::PUT, known_url.clone())
                .json(&json!({"enabled": true, "stream_profile": "SUB"}))
                .send()
                .expect("non-canonical profile response");
        let unknown_camera = gate_admin_request(
            &client,
            reqwest::Method::PUT,
            format!("{base_url}/api/cameras/camera.unknown/cat-detection/control"),
        )
        .json(&json!({"enabled": true, "stream_profile": "sub"}))
        .send()
        .expect("unknown camera response");
        let trailing_segment =
            gate_admin_request(&client, reqwest::Method::GET, format!("{known_url}/extra"))
                .send()
                .expect("trailing segment response");
        let device_principal = client
            .put(known_url)
            .bearer_auth("service-token")
            .header("X-Harbor-Principal-Source", "harbornavi-device")
            .header(
                "X-Harbor-Principal-Id",
                "harbornavi-device:0123456789abcdef0123456789abcdef",
            )
            .header("X-Harbor-Principal-Roles", "CAMERA_VIEW")
            .header("X-Harbor-Workspace-Id", "home-1")
            .header("X-Harbor-Camera-Scope", "camera/252")
            .json(&json!({"enabled": true, "stream_profile": "sub"}))
            .send()
            .expect("device principal response");
        admin_server.join().expect("admin server");

        assert_eq!(invalid_profile.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            normalized_profile.status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        assert_eq!(unknown_camera.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(trailing_segment.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(device_principal.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[test]
    fn cat_detection_control_put_rejects_registered_edge_whitespace_camera_id() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-edge-whitespace");
        let plain_policy = CatDetectionControlPolicy::new("camera", false, "main", 17)
            .expect("plain camera policy");
        CatDetectionControlStore::try_new(control_path.clone())
            .expect("control store")
            .upsert(plain_policy.clone())
            .expect("persist plain camera policy");
        let (api, _paths) = build_test_admin_api("cat-control-edge-whitespace");
        save_test_camera(&api, "camera");
        save_test_camera(&api, " camera ");
        let (base_url, admin_server) = spawn_admin_test_server(api, 1);
        let client = Client::builder().build().expect("HTTP client");

        let response = gate_admin_request(
            &client,
            reqwest::Method::PUT,
            format!("{base_url}/api/cameras/%20camera%20/cat-detection/control"),
        )
        .json(&json!({"enabled": false, "stream_profile": "sub"}))
        .send()
        .expect("edge-whitespace response");
        admin_server.join().expect("admin server");

        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let policies = CatDetectionControlStore::try_new(control_path)
            .expect("control store")
            .load()
            .expect("load control store");
        assert_eq!(policies.len(), 1, "edge ID must not create another policy");
        assert_eq!(policies.get("camera"), Some(&plain_policy));
        assert!(!policies.contains_key(" camera "));
    }

    #[test]
    fn cat_detection_control_corrupt_store_is_diagnostic_and_not_overwritten() {
        let _gate_env_lock = gate_principal_env_lock()
            .lock()
            .expect("gate principal env lock");
        let _auto_env_lock = cat_auto_recording_env_lock()
            .lock()
            .expect("cat auto-recording env lock");
        let _token = EnvGuard::set(HARBORBEACON_WEB_API_TOKEN_ENV, "service-token");
        let (control_path, _control_env) =
            install_cat_detection_control_test_environment("cat-control-corrupt");
        let corrupt_bytes = b"{not-valid-json";
        fs::write(&control_path, corrupt_bytes).expect("write corrupt control store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&control_path, fs::Permissions::from_mode(0o600))
                .expect("secure control store permissions");
        }
        let (harborlink_url, request_count, harborlink_server) =
            spawn_harborlink_request_counter_server();
        let (api, _paths) = build_test_admin_api_with_harborlink(
            "cat-control-corrupt",
            HarborLinkMediaClient::new(harborlink_url).expect("HarborLink client"),
            &["camera.252"],
        );
        let _auto_enabled = EnvGuard::set("HARBOR_K3_CAT_AUTO_RECORD_ENABLED", "true");
        let mut runtime = sample_running_detection_job("corrupt-runtime", "camera.252", true, None);
        runtime.projection.expires_at = "2000-01-01T00:00:00Z".to_string();
        api.detection_jobs
            .lock()
            .expect("detection jobs")
            .insert("corrupt-runtime".to_string(), runtime);
        assert!(api.cat_detection_may_run("camera.252").is_err());
        assert!(api
            .ensure_live_managed_detection_job("camera.252", "sub")
            .is_err());
        assert!(api.stop_live_managed_detection_job("camera.252").is_err());
        assert!(api.renew_auto_recording_detection_leases().is_err());
        let now = cat_auto_recording_epoch_ms();
        assert!(api
            .process_cat_detection_result(
                "camera.252",
                "sub",
                Some(&json!({
                    "sequence": 1,
                    "frame_epoch_ms": now,
                    "detection_count": 1,
                    "consecutive_present_frames": 1,
                    "consecutive_absent_frames": 0,
                    "present_since_epoch_ms": now,
                    "absent_since_epoch_ms": 0,
                    "max_confidence": 0.9
                })),
                CatAutoRecordingConfig {
                    start_consecutive_frames: 1,
                    start_duration_ms: 0,
                    stop_consecutive_frames: 1,
                    stop_duration_ms: 0,
                },
            )
            .is_err());
        assert_eq!(
            api.detection_jobs.lock().expect("detection jobs")["corrupt-runtime"]
                .projection
                .status,
            "running"
        );
        let (base_url, admin_server) = spawn_admin_test_server(api, 3);
        let client = Client::builder().build().expect("HTTP client");
        let url = format!("{base_url}/api/cameras/camera.252/cat-detection/control");

        let get = gate_admin_request(&client, reqwest::Method::GET, url.clone())
            .send()
            .expect("diagnostic get response");
        let get_status = get.status();
        let get_body: Value = get.json().expect("diagnostic get json");
        let put = gate_admin_request(&client, reqwest::Method::PUT, url)
            .json(&json!({"enabled": true, "stream_profile": "sub"}))
            .send()
            .expect("corrupt store put response");
        let put_status = put.status();
        let put_body: Value = put.json().expect("corrupt store put json");
        let public_post = gate_admin_request(
            &client,
            reqwest::Method::POST,
            format!("{base_url}/api/vision/detection-jobs"),
        )
        .json(&json!({
            "camera_id": "camera.252",
            "target_label": "cat",
            "stream_profile": "sub"
        }))
        .send()
        .expect("corrupt store public POST response");
        let public_post_status = public_post.status();
        let public_post_body: Value = public_post.json().expect("public POST json");
        admin_server.join().expect("admin server");
        harborlink_server.join().expect("HarborLink server");

        assert_eq!(get_status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            get_body["error"]["code"],
            "CAT_DETECTION_CONTROL_UNAVAILABLE"
        );
        assert_eq!(
            get_body["error"]["message"],
            "Cat detection control is temporarily unavailable."
        );
        assert_eq!(put_status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(put_body["error"], get_body["error"]);
        assert_eq!(public_post_status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(public_post_body["error"], get_body["error"]);
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
        let public_bodies = format!("{get_body}{put_body}{public_post_body}");
        assert!(!public_bodies.contains("store is invalid"));
        assert!(!public_bodies.contains(&control_path.to_string_lossy().to_string()));
        assert_eq!(
            fs::read(control_path).expect("read corrupt store"),
            corrupt_bytes
        );
    }
