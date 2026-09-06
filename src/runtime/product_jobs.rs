//! Persistent, actor-scoped product work. The first executor exports Rules history.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use atomicwrites::{AllowOverwrite, AtomicFile};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::runtime::automation::RulesStore;

const MAX_JOBS: usize = 128;
const MAX_ACTIVE: usize = 4;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_EXPORT_STORAGE: u64 = 256 * 1024 * 1024;
const STORAGE: &str = "STORAGE: Task storage is unavailable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl JobStatus {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub completed: u64,
    pub total: Option<u64>,
    pub phase: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub file_name: String,
    pub record_count: u64,
    pub byte_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub at: u64,
    pub action: String,
    pub status: JobStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductJob {
    pub job_id: String,
    pub job_type: String,
    pub home_id: String,
    pub actor_id: String,
    pub status: JobStatus,
    pub progress: JobProgress,
    pub cancel_policy: String,
    pub cancel_requested: bool,
    pub created_at: u64,
    pub updated_at: u64,
    pub finished_at: Option<u64>,
    pub retry_of: Option<String>,
    pub result: Option<JobResult>,
    pub error_code: Option<String>,
    pub events: Vec<JobEvent>,
}

impl ProductJob {
    pub fn can_cancel(&self) -> bool {
        !self.status.terminal() && !self.cancel_requested && self.progress.phase != "finalizing"
    }
    pub fn can_retry(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Failed | JobStatus::Cancelled | JobStatus::Interrupted
        )
    }
    fn event(&mut self, action: &str) {
        self.updated_at = now();
        self.events.push(JobEvent {
            at: self.updated_at,
            action: action.into(),
            status: self.status,
        });
    }
    fn finish(&mut self, status: JobStatus, error: Option<&str>) {
        self.status = status;
        self.error_code = error.map(str::to_owned);
        self.event("finished");
        self.finished_at = Some(self.updated_at);
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredJob {
    job: ProductJob,
    idempotency_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    jobs: Vec<StoredJob>,
}

#[derive(Default)]
struct Inner {
    state: Option<State>,
    file_lock: Option<File>,
}

#[derive(Clone)]
pub struct ProductJobStore {
    root: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

impl ProductJobStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    // Lazy initialization isolates this optional capability from product startup.
    fn lock(&self) -> Result<MutexGuard<'_, Inner>, String> {
        let mut inner = self.inner.lock().map_err(|_| STORAGE)?;
        if inner.state.is_none() {
            fs::create_dir_all(&self.root).map_err(|_| STORAGE)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                    .map_err(|_| STORAGE)?;
            }
            let file_lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.root.join("writer.lock"))
                .map_err(|_| STORAGE)?;
            file_lock.try_lock_exclusive().map_err(|_| STORAGE)?;
            let state_path = self.root.join("state.json");
            let mut state = match File::open(&state_path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(MAX_STATE_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| STORAGE)?;
                    if bytes.len() as u64 > MAX_STATE_BYTES {
                        return Err(STORAGE.into());
                    }
                    serde_json::from_slice::<State>(&bytes).map_err(|_| STORAGE)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => State {
                    schema_version: 1,
                    jobs: vec![],
                },
                Err(_) => return Err(STORAGE.into()),
            };
            if state.schema_version != 1
                || state.jobs.len() > MAX_JOBS
                || state.jobs.iter().any(|row| {
                    !valid_id(&row.job.job_id) || row.job.job_type != "rules_history_export"
                })
            {
                return Err(STORAGE.into());
            }
            let mut recovered = false;
            for row in &mut state.jobs {
                if !row.job.status.terminal() {
                    row.job.result = None;
                    row.job
                        .finish(JobStatus::Interrupted, Some("TASK_INTERRUPTED"));
                    recovered = true;
                }
                remove_if_present(&self.output_path(&row.job.job_id, "partial"))?;
                if row.job.status != JobStatus::Succeeded {
                    remove_if_present(&self.output_path(&row.job.job_id, "json"))?;
                }
            }
            if recovered {
                self.save(&state)?;
            }
            inner.state = Some(state);
            inner.file_lock = Some(file_lock);
        }
        Ok(inner)
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let bytes = serde_json::to_vec(state).map_err(|_| STORAGE)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(STORAGE.into());
        }
        AtomicFile::new(self.root.join("state.json"), AllowOverwrite)
            .write(|file| {
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .map_err(|_| STORAGE)?;
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|_| STORAGE)?;
        Ok(())
    }

    fn change<T>(&self, change: impl FnOnce(&mut State) -> Result<T, String>) -> Result<T, String> {
        let mut inner = self.lock()?;
        let mut next = inner.state.as_ref().expect("initialized").clone();
        let result = change(&mut next)?;
        self.save(&next)?;
        inner.state = Some(next);
        Ok(result)
    }

    pub fn list(&self, home: &str, actor: &str) -> Result<Vec<ProductJob>, String> {
        let inner = self.lock()?;
        let mut jobs = inner
            .state
            .as_ref()
            .expect("initialized")
            .jobs
            .iter()
            .filter(|row| row.job.home_id == home && row.job.actor_id == actor)
            .map(|row| row.job.clone())
            .collect::<Vec<_>>();
        jobs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.job_id.cmp(&a.job_id))
        });
        Ok(jobs)
    }

    pub fn get(&self, home: &str, actor: &str, id: &str) -> Result<ProductJob, String> {
        self.list(home, actor)?
            .into_iter()
            .find(|job| job.job_id == id)
            .ok_or_else(|| "NOT_FOUND: Task not found".into())
    }

    pub fn create(
        &self,
        home: &str,
        actor: &str,
        key: &str,
        retry_of: Option<&str>,
    ) -> Result<(ProductJob, bool), String> {
        if !valid_id(key) || home.is_empty() || actor.is_empty() {
            return Err("VALIDATION: A request key is required".into());
        }
        self.change(|state| {
            if let Some(existing) = state.jobs.iter().find(|row| {
                row.job.home_id == home && row.job.actor_id == actor && row.idempotency_key == key
            }) {
                if existing.job.retry_of.as_deref() != retry_of {
                    return Err("CONFLICT: Request key already used for different work".into());
                }
                return Ok((existing.job.clone(), false));
            }
            if let Some(id) = retry_of {
                let source = scoped_mut(state, home, actor, id)?;
                if !source.can_retry() {
                    return Err("CONFLICT: This task cannot be retried".into());
                }
            }
            let active = state
                .jobs
                .iter()
                .filter(|row| !row.job.status.terminal())
                .count();
            let used: u64 = state
                .jobs
                .iter()
                .filter_map(|row| row.job.result.as_ref())
                .map(|result| result.byte_count)
                .sum();
            if state.jobs.len() >= MAX_JOBS
                || active >= MAX_ACTIVE
                || used + ((active + 1) as u64 * MAX_EXPORT_BYTES) > MAX_EXPORT_STORAGE
            {
                return Err("CAPACITY: Task capacity reached".into());
            }
            let timestamp = now();
            let mut job = ProductJob {
                job_id: format!("job_{}", Uuid::new_v4().simple()),
                job_type: "rules_history_export".into(),
                home_id: home.into(),
                actor_id: actor.into(),
                status: JobStatus::Queued,
                progress: JobProgress {
                    completed: 0,
                    total: None,
                    phase: "queued".into(),
                },
                cancel_policy: "until_finalizing".into(),
                cancel_requested: false,
                created_at: timestamp,
                updated_at: timestamp,
                finished_at: None,
                retry_of: retry_of.map(str::to_owned),
                result: None,
                error_code: None,
                events: vec![],
            };
            job.event(if retry_of.is_some() {
                "retried"
            } else {
                "created"
            });
            state.jobs.push(StoredJob {
                job: job.clone(),
                idempotency_key: key.into(),
            });
            Ok((job, true))
        })
    }

    pub fn cancel(&self, home: &str, actor: &str, id: &str) -> Result<ProductJob, String> {
        self.change(|state| {
            let job = scoped_mut(state, home, actor, id)?;
            if job.status.terminal() || job.cancel_requested {
                return Ok(job.clone());
            }
            if !job.can_cancel() {
                return Err("CONFLICT: Finishing export; cancellation is unavailable".into());
            }
            job.cancel_requested = true;
            job.event("cancel_requested");
            if job.status == JobStatus::Queued {
                job.finish(JobStatus::Cancelled, None);
            }
            Ok(job.clone())
        })
    }

    fn begin(&self, id: &str) -> Result<bool, String> {
        self.change(|state| {
            let job = job_mut(state, id)?;
            if job.status != JobStatus::Queued {
                return Ok(false);
            }
            job.status = JobStatus::Running;
            job.progress.phase = "reading".into();
            job.event("started");
            Ok(true)
        })
    }

    fn progress(&self, id: &str, completed: u64, total: u64) -> Result<(), String> {
        self.change(|state| {
            let job = job_mut(state, id)?;
            if job.cancel_requested {
                return Err("CANCELLED".into());
            }
            if job.status != JobStatus::Running {
                return Err("CONFLICT: Task is no longer running".into());
            }
            job.progress = JobProgress {
                completed,
                total: Some(total),
                phase: "exporting".into(),
            };
            job.updated_at = now();
            Ok(())
        })
    }

    fn start_finalizing(&self, id: &str) -> Result<(), String> {
        self.change(|state| {
            let job = job_mut(state, id)?;
            if job.cancel_requested {
                return Err("CANCELLED".into());
            }
            if job.status != JobStatus::Running {
                return Err("CONFLICT: Task is no longer running".into());
            }
            job.progress.phase = "finalizing".into();
            job.event("finalizing");
            Ok(())
        })
    }

    pub fn fail_to_start(&self, id: &str) {
        let _ = self.change(|state| {
            let job = job_mut(state, id)?;
            if !job.status.terminal() {
                job.finish(JobStatus::Failed, Some("TASK_START_FAILED"));
            }
            Ok(())
        });
    }

    pub fn run_export(&self, id: &str, rules: &RulesStore) {
        self.run_export_observed(id, rules, || {});
    }

    fn run_export_observed(&self, id: &str, rules: &RulesStore, observed: impl FnOnce()) {
        match self.begin(id) {
            Ok(true) => {}
            Ok(false) => return,
            Err(_) => {
                self.fail_to_start(id);
                return;
            }
        }
        let outcome = self.export(id, rules, observed);
        if let Err(error) = outcome {
            let cleaned = remove_if_present(&self.output_path(id, "partial"))
                .and_then(|_| remove_if_present(&self.output_path(id, "json")))
                .is_ok();
            let _ = self.change(|state| {
                let job = job_mut(state, id)?;
                if !job.status.terminal() {
                    job.result = None;
                    if error == "CANCELLED" && cleaned {
                        job.finish(JobStatus::Cancelled, None);
                    } else {
                        job.finish(
                            JobStatus::Failed,
                            Some(if cleaned {
                                "EXPORT_FAILED"
                            } else {
                                "EXPORT_CLEANUP_FAILED"
                            }),
                        );
                    }
                }
                Ok(())
            });
        }
    }

    fn export(&self, id: &str, rules: &RulesStore, observed: impl FnOnce()) -> Result<(), String> {
        let rows = rules.history_snapshot()?;
        let total = rows.len() as u64;
        self.progress(id, 0, total)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.output_path(id, "partial"))
            .map_err(|_| STORAGE)?;
        let mut writer = BufWriter::new(file);
        write!(
            writer,
            "{{\"format_version\":1,\"generated_at\":{},\"executions\":[",
            now()
        )
        .map_err(|_| STORAGE)?;
        observed();
        for (index, row) in rows.iter().enumerate() {
            if index % 64 == 0 {
                self.progress(id, index as u64, total)?;
            }
            if index > 0 {
                writer.write_all(b",").map_err(|_| STORAGE)?;
            }
            // Export product facts only, never executor messages, request payloads or paths.
            serde_json::to_writer(&mut writer, &json!({
                "run_id": row.run_id, "rule_id": row.rule_id, "revision": row.revision,
                "started_at": row.started_at, "ended_at": row.ended_at,
                "status": public_run_status(&row.status),
                "actions_succeeded": row.actions.iter().filter(|action| action.status == "succeeded").count(),
                "actions_total": row.actions.len(),
            })).map_err(|_| STORAGE)?;
            if index % 64 == 0 {
                writer.flush().map_err(|_| STORAGE)?;
                if writer.get_ref().metadata().map_err(|_| STORAGE)?.len() > MAX_EXPORT_BYTES {
                    return Err("CAPACITY: Export is too large".into());
                }
            }
        }
        writer.write_all(b"]}").map_err(|_| STORAGE)?;
        writer.flush().map_err(|_| STORAGE)?;
        let byte_count = writer.get_ref().metadata().map_err(|_| STORAGE)?.len();
        if byte_count > MAX_EXPORT_BYTES {
            return Err("CAPACITY: Export is too large".into());
        }
        self.progress(id, total, total)?;
        self.start_finalizing(id)?;
        writer.get_ref().sync_all().map_err(|_| STORAGE)?;
        drop(writer);
        fs::rename(
            self.output_path(id, "partial"),
            self.output_path(id, "json"),
        )
        .map_err(|_| STORAGE)?;
        self.change(|state| {
            let job = job_mut(state, id)?;
            job.result = Some(JobResult {
                file_name: "rules-history.json".into(),
                record_count: total,
                byte_count,
            });
            job.progress.phase = "complete".into();
            job.finish(JobStatus::Succeeded, None);
            Ok(())
        })
    }

    pub fn download(&self, home: &str, actor: &str, id: &str) -> Result<Vec<u8>, String> {
        let job = self.get(home, actor, id)?;
        if job.status != JobStatus::Succeeded {
            return Err("CONFLICT: Export is not available".into());
        }
        let mut bytes = Vec::new();
        File::open(self.output_path(id, "json"))
            .map_err(|_| "NOT_FOUND: Export is unavailable; create a new export")?
            .take(MAX_EXPORT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| STORAGE)?;
        if bytes.len() as u64 > MAX_EXPORT_BYTES
            || job.result.as_ref().map(|result| result.byte_count) != Some(bytes.len() as u64)
        {
            return Err(STORAGE.into());
        }
        Ok(bytes)
    }

    fn output_path(&self, id: &str, extension: &str) -> PathBuf {
        self.root.join(format!("{id}.{extension}"))
    }
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
}

fn job_mut<'a>(state: &'a mut State, id: &str) -> Result<&'a mut ProductJob, String> {
    state
        .jobs
        .iter_mut()
        .find(|row| row.job.job_id == id)
        .map(|row| &mut row.job)
        .ok_or_else(|| "NOT_FOUND: Task not found".into())
}

fn scoped_mut<'a>(
    state: &'a mut State,
    home: &str,
    actor: &str,
    id: &str,
) -> Result<&'a mut ProductJob, String> {
    let job = job_mut(state, id)?;
    if job.home_id != home || job.actor_id != actor {
        return Err("NOT_FOUND: Task not found".into());
    }
    Ok(job)
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(STORAGE.into()),
    }
}

fn public_run_status(status: &str) -> &'static str {
    match status {
        "completed" => "succeeded",
        "running" => "running",
        "skipped" => "skipped",
        "interrupted" => "interrupted",
        _ => "failed",
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
#[path = "product_jobs_tests.rs"]
mod tests;
