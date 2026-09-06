use super::*;
use crate::runtime::automation::{RuleAction, RuleConditions, RuleDefinition, RuleTrigger};
use std::collections::BTreeMap;
use std::sync::mpsc;

fn fixture() -> (PathBuf, ProductJobStore, RulesStore) {
    let root = std::env::temp_dir().join(format!("product-jobs-{}", Uuid::new_v4()));
    let jobs = ProductJobStore::new(root.join("jobs"));
    let rules = RulesStore::new(root.join("rules.json"));
    let rule = rules
        .create(
            RuleDefinition {
                name: "Evening record".into(),
                trigger: RuleTrigger::Manual,
                conditions: RuleConditions {
                    match_mode: "all".into(),
                    items: vec![],
                },
                actions: vec![RuleAction::Record {
                    message: "private/path/model-name".into(),
                }],
                expires_at: None,
            },
            1,
        )
        .unwrap();
    rules
        .preview(&rule.rule_id, 1, &BTreeMap::new(), 2)
        .unwrap();
    rules.set_status(&rule.rule_id, 1, "enabled", 3).unwrap();
    rules
        .run(
            &rule.rule_id,
            1,
            "manual:test",
            "manual",
            &BTreeMap::new(),
            4,
            |_| Ok("private/path/model-name".into()),
        )
        .unwrap();
    (root, jobs, rules)
}

#[test]
fn product_jobs_export_real_history_and_keep_result_after_reopen() {
    let (root, jobs, rules) = fixture();
    let (job, created) = jobs.create("home-1", "actor-1", "request-1", None).unwrap();
    assert!(created);
    jobs.run_export(&job.job_id, &rules);
    let completed = jobs.get("home-1", "actor-1", &job.job_id).unwrap();
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(completed.progress.completed, 1);
    let bytes = jobs.download("home-1", "actor-1", &job.job_id).unwrap();
    let exported: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(exported["executions"].as_array().unwrap().len(), 1);
    assert!(!String::from_utf8(bytes).unwrap().contains("private/path"));
    assert_eq!(
        jobs.create("home-1", "actor-1", "request-1", None)
            .unwrap()
            .0
            .job_id,
        job.job_id
    );
    drop(jobs);
    let reopened = ProductJobStore::new(root.join("jobs"));
    assert_eq!(
        reopened.get("home-1", "actor-1", &job.job_id).unwrap(),
        completed
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_jobs_running_cancel_waits_for_worker_then_next_export_succeeds() {
    let (root, jobs, rules) = fixture();
    let (job, _) = jobs.create("home-1", "actor-1", "cancel-me", None).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker_jobs = jobs.clone();
    let worker_rules = rules.clone();
    let id = job.job_id.clone();
    let worker = std::thread::spawn(move || {
        worker_jobs.run_export_observed(&id, &worker_rules, || {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
        });
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let pending = jobs.cancel("home-1", "actor-1", &job.job_id).unwrap();
    assert_eq!(pending.status, JobStatus::Running);
    assert!(pending.cancel_requested);
    assert!(jobs.download("home-1", "actor-1", &job.job_id).is_err());
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    let cancelled = jobs.get("home-1", "actor-1", &job.job_id).unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.result.is_none());
    assert!(!jobs.output_path(&job.job_id, "partial").exists());
    let (retry, _) = jobs
        .create("home-1", "actor-1", "retry-1", Some(&job.job_id))
        .unwrap();
    jobs.run_export(&retry.job_id, &rules);
    assert_eq!(
        jobs.get("home-1", "actor-1", &retry.job_id).unwrap().status,
        JobStatus::Succeeded
    );
    assert_eq!(
        jobs.cancel("home-1", "actor-1", &retry.job_id)
            .unwrap()
            .status,
        JobStatus::Succeeded
    );
    drop(jobs);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_jobs_scope_idempotency_and_recovery_are_enforced() {
    let (root, jobs, _) = fixture();
    let (job, _) = jobs.create("home-1", "actor-1", "same-key", None).unwrap();
    let other = jobs
        .create("home-1", "actor-2", "same-key", None)
        .unwrap()
        .0;
    assert_ne!(job.job_id, other.job_id);
    assert!(jobs.get("home-2", "actor-1", &job.job_id).is_err());
    assert!(jobs.cancel("home-1", "actor-2", &job.job_id).is_err());
    assert_eq!(jobs.list("home-1", "actor-2").unwrap().len(), 1);
    jobs.begin(&job.job_id).unwrap();
    drop(jobs);
    let reopened = ProductJobStore::new(root.join("jobs"));
    let recovered = reopened.get("home-1", "actor-1", &job.job_id).unwrap();
    assert_eq!(recovered.status, JobStatus::Interrupted);
    assert_eq!(
        reopened
            .get("home-1", "actor-2", &other.job_id)
            .unwrap()
            .status,
        JobStatus::Interrupted
    );
    let (retry, _) = reopened
        .create("home-1", "actor-1", "retry", Some(&job.job_id))
        .unwrap();
    assert!(reopened.create("home-1", "actor-1", "retry", None).is_err());
    assert_eq!(
        reopened
            .create("home-1", "actor-1", "retry", Some(&job.job_id))
            .unwrap()
            .0
            .job_id,
        retry.job_id
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_jobs_cancel_cannot_win_after_finalization_begins() {
    let (root, jobs, _) = fixture();
    let (job, _) = jobs.create("home", "actor", "key", None).unwrap();
    jobs.begin(&job.job_id).unwrap();
    jobs.start_finalizing(&job.job_id).unwrap();
    assert!(jobs
        .cancel("home", "actor", &job.job_id)
        .unwrap_err()
        .starts_with("CONFLICT:"));
    drop(jobs);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_jobs_invalid_store_is_preserved_and_second_writer_is_rejected() {
    let (root, jobs, _) = fixture();
    jobs.create("home", "actor", "key", None).unwrap();
    let duplicate = ProductJobStore::new(root.join("jobs"));
    assert!(duplicate.list("home", "actor").is_err());
    drop(duplicate);
    drop(jobs);
    fs::write(root.join("jobs/state.json"), b"{broken").unwrap();
    let broken = ProductJobStore::new(root.join("jobs"));
    assert!(broken.list("home", "actor").is_err());
    assert_eq!(fs::read(root.join("jobs/state.json")).unwrap(), b"{broken");
    drop(broken);
    fs::remove_dir_all(root).unwrap();
}
