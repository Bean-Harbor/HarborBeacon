//! Bounded shared retry scheduling for per-camera cat detection reconciliation.

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatDetectionRetryOutcome {
    Complete,
    Retry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatDetectionRetryEnqueueResult {
    Enqueued,
    Coalesced,
    Superseded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatDetectionRetryEntry {
    pub camera_id: String,
    pub revision: u128,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct CatDetectionRetrySchedulerConfig {
    pub worker_count: usize,
    pub capacity: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

pub type CatDetectionRetryTask =
    Arc<dyn Fn(CatDetectionRetryEntry) -> CatDetectionRetryOutcome + Send + Sync>;

struct ScheduledRetry {
    entry: CatDetectionRetryEntry,
    due_at: Instant,
    delay: Duration,
    task: CatDetectionRetryTask,
}

#[derive(Default)]
struct SchedulerState {
    pending: HashMap<String, ScheduledRetry>,
    active: HashMap<String, (u64, u128)>,
    latest_generation: HashMap<String, u64>,
    shutdown: bool,
}

struct SchedulerInner {
    config: CatDetectionRetrySchedulerConfig,
    state: Mutex<SchedulerState>,
    wake: Condvar,
    next_generation: AtomicU64,
    outer_owners: AtomicUsize,
    live_workers: AtomicUsize,
    joins: Mutex<Vec<thread::JoinHandle<()>>>,
}

pub struct CatDetectionRetryScheduler {
    inner: Arc<SchedulerInner>,
}

#[derive(Clone)]
pub struct CatDetectionRetrySchedulerProbe {
    inner: Weak<SchedulerInner>,
}

impl CatDetectionRetrySchedulerProbe {
    pub fn is_alive(&self) -> bool {
        self.inner.upgrade().is_some()
    }

    pub fn outer_owners(&self) -> usize {
        self.inner
            .upgrade()
            .map(|inner| inner.outer_owners.load(Ordering::SeqCst))
            .unwrap_or_default()
    }

    pub fn pending_jobs(&self) -> usize {
        self.inner
            .upgrade()
            .and_then(|inner| inner.state.lock().ok().map(|state| state.pending.len()))
            .unwrap_or_default()
    }

    pub fn active_jobs(&self) -> usize {
        self.inner
            .upgrade()
            .and_then(|inner| inner.state.lock().ok().map(|state| state.active.len()))
            .unwrap_or_default()
    }

    pub fn worker_count(&self) -> usize {
        self.inner
            .upgrade()
            .map(|inner| inner.live_workers.load(Ordering::SeqCst))
            .unwrap_or_default()
    }
}

impl Clone for CatDetectionRetryScheduler {
    fn clone(&self) -> Self {
        self.inner
            .outer_owners
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |owners| {
                owners.checked_add(1)
            })
            .expect("cat detection retry scheduler owner count overflowed");
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for CatDetectionRetryScheduler {
    fn drop(&mut self) {
        let previous =
            self.inner
                .outer_owners
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |owners| {
                    owners.checked_sub(1)
                });
        if previous == Ok(1) {
            let _ = self.shutdown_and_join();
        } else if previous.is_err() {
            eprintln!("HarborBeacon cat detection retry scheduler owner count was already zero");
        }
    }
}

impl CatDetectionRetryScheduler {
    pub fn new(config: CatDetectionRetrySchedulerConfig) -> Result<Self, String> {
        if config.worker_count == 0 || config.capacity == 0 {
            return Err("cat detection retry scheduler requires non-zero limits".to_string());
        }
        if config.max_delay < config.initial_delay {
            return Err("cat detection retry scheduler max delay is invalid".to_string());
        }
        let scheduler = Self {
            inner: Arc::new(SchedulerInner {
                config,
                state: Mutex::new(SchedulerState::default()),
                wake: Condvar::new(),
                next_generation: AtomicU64::new(0),
                outer_owners: AtomicUsize::new(1),
                live_workers: AtomicUsize::new(0),
                joins: Mutex::new(Vec::with_capacity(config.worker_count)),
            }),
        };
        for index in 0..config.worker_count {
            let inner = scheduler.inner.clone();
            let join = match thread::Builder::new()
                .name(format!("cat-detection-retry-{index}"))
                .spawn(move || run_worker(inner))
            {
                Ok(join) => join,
                Err(error) => {
                    let _ = scheduler.shutdown_and_join();
                    return Err(format!(
                        "failed to start cat detection retry scheduler: {error}"
                    ));
                }
            };
            scheduler
                .inner
                .joins
                .lock()
                .map_err(|_| "cat detection retry scheduler join state is unavailable".to_string())?
                .push(join);
        }
        Ok(scheduler)
    }

    pub fn enqueue(
        &self,
        camera_id: &str,
        revision: u128,
        delay: Duration,
        task: CatDetectionRetryTask,
    ) -> Result<CatDetectionRetryEnqueueResult, String> {
        if camera_id.is_empty() || camera_id.len() > 128 || camera_id.chars().any(char::is_control)
        {
            return Err("cat detection retry camera ID is invalid".to_string());
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "cat detection retry scheduler state is unavailable".to_string())?;
        if state.shutdown {
            return Err("cat detection retry scheduler is shut down".to_string());
        }
        let active_revision = state.active.get(camera_id).map(|(_, revision)| *revision);
        let pending_revision = state
            .pending
            .get(camera_id)
            .map(|scheduled| scheduled.entry.revision);
        if active_revision
            .into_iter()
            .chain(pending_revision)
            .any(|current| current > revision)
        {
            return Ok(CatDetectionRetryEnqueueResult::Superseded);
        }
        if active_revision == Some(revision) || pending_revision == Some(revision) {
            return Ok(CatDetectionRetryEnqueueResult::Coalesced);
        }
        let owned_cameras = state
            .active
            .keys()
            .chain(state.pending.keys())
            .map(String::as_str)
            .collect::<HashSet<_>>();
        if !owned_cameras.contains(camera_id) && owned_cameras.len() >= self.inner.config.capacity {
            return Err("cat detection retry scheduler capacity is exhausted".to_string());
        }
        let generation = self
            .inner
            .next_generation
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        state
            .latest_generation
            .insert(camera_id.to_string(), generation);
        state.pending.insert(
            camera_id.to_string(),
            ScheduledRetry {
                entry: CatDetectionRetryEntry {
                    camera_id: camera_id.to_string(),
                    revision,
                    generation,
                },
                due_at: Instant::now() + delay.min(self.inner.config.max_delay),
                delay,
                task,
            },
        );
        drop(state);
        self.inner.wake.notify_one();
        Ok(CatDetectionRetryEnqueueResult::Enqueued)
    }

    pub fn cancel(&self, camera_id: &str) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.pending.remove(camera_id);
            state.latest_generation.remove(camera_id);
            self.inner.next_generation.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.wake.notify_all();
    }

    pub fn queued_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.pending.len())
            .unwrap_or_default()
    }

    pub fn active_jobs(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.active.len())
            .unwrap_or_default()
    }

    pub fn contains_camera(&self, camera_id: &str) -> bool {
        self.inner
            .state
            .lock()
            .map(|state| {
                state.pending.contains_key(camera_id) || state.active.contains_key(camera_id)
            })
            .unwrap_or(false)
    }

    pub fn worker_count(&self) -> usize {
        self.inner.live_workers.load(Ordering::SeqCst)
    }

    pub fn probe(&self) -> CatDetectionRetrySchedulerProbe {
        CatDetectionRetrySchedulerProbe {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn shutdown_and_join(&self) -> Result<(), String> {
        {
            let mut state =
                self.inner.state.lock().map_err(|_| {
                    "cat detection retry scheduler state is unavailable".to_string()
                })?;
            state.shutdown = true;
            state.pending.clear();
            state.latest_generation.clear();
        }
        self.inner.wake.notify_all();
        let joins = self
            .inner
            .joins
            .lock()
            .map_err(|_| "cat detection retry scheduler join state is unavailable".to_string())?
            .drain(..)
            .collect::<Vec<_>>();
        let mut panicked = false;
        let current_thread = thread::current().id();
        for join in joins {
            if join.thread().id() == current_thread {
                continue;
            }
            panicked |= join.join().is_err();
        }
        if panicked {
            Err("cat detection retry scheduler worker panicked".to_string())
        } else {
            Ok(())
        }
    }
}

fn run_worker(inner: Arc<SchedulerInner>) {
    inner.live_workers.fetch_add(1, Ordering::SeqCst);
    loop {
        let scheduled = {
            let mut state = match inner.state.lock() {
                Ok(state) => state,
                Err(_) => break,
            };
            loop {
                if state.shutdown {
                    inner.live_workers.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
                let next = state
                    .pending
                    .iter()
                    .min_by_key(|(_, scheduled)| scheduled.due_at)
                    .map(|(camera_id, scheduled)| (camera_id.clone(), scheduled.due_at));
                let Some((camera_id, due_at)) = next else {
                    state = match inner.wake.wait(state) {
                        Ok(state) => state,
                        Err(_) => {
                            inner.live_workers.fetch_sub(1, Ordering::SeqCst);
                            return;
                        }
                    };
                    continue;
                };
                let wait = due_at.saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    state = match inner.wake.wait_timeout(state, wait) {
                        Ok((state, _)) => state,
                        Err(_) => {
                            inner.live_workers.fetch_sub(1, Ordering::SeqCst);
                            return;
                        }
                    };
                    continue;
                }
                let scheduled = state
                    .pending
                    .remove(&camera_id)
                    .expect("selected retry remains queued while scheduler lock is held");
                state.active.insert(
                    camera_id,
                    (scheduled.entry.generation, scheduled.entry.revision),
                );
                break scheduled;
            }
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            (scheduled.task)(scheduled.entry.clone())
        }))
        .unwrap_or(CatDetectionRetryOutcome::Retry);
        let mut state = match inner.state.lock() {
            Ok(state) => state,
            Err(_) => break,
        };
        if state.active.get(&scheduled.entry.camera_id)
            == Some(&(scheduled.entry.generation, scheduled.entry.revision))
        {
            state.active.remove(&scheduled.entry.camera_id);
        }
        if state.shutdown
            || state.latest_generation.get(&scheduled.entry.camera_id)
                != Some(&scheduled.entry.generation)
        {
            inner.wake.notify_all();
            continue;
        }
        match outcome {
            CatDetectionRetryOutcome::Complete => {
                state.latest_generation.remove(&scheduled.entry.camera_id);
            }
            CatDetectionRetryOutcome::Retry => {
                let next_delay = if scheduled.delay < inner.config.initial_delay {
                    inner.config.initial_delay
                } else {
                    scheduled
                        .delay
                        .checked_mul(2)
                        .unwrap_or(inner.config.max_delay)
                        .min(inner.config.max_delay)
                };
                state.pending.insert(
                    scheduled.entry.camera_id.clone(),
                    ScheduledRetry {
                        due_at: Instant::now() + next_delay,
                        delay: next_delay,
                        ..scheduled
                    },
                );
            }
        }
        drop(state);
        inner.wake.notify_all();
    }
    inner.live_workers.fetch_sub(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    fn config(worker_count: usize, capacity: usize) -> CatDetectionRetrySchedulerConfig {
        CatDetectionRetrySchedulerConfig {
            worker_count,
            capacity,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(8),
        }
    }

    fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(condition(), "condition did not become true before timeout");
    }

    #[test]
    fn scheduler_uses_fixed_pool_and_processes_more_than_one_hundred_cameras() {
        let scheduler = CatDetectionRetryScheduler::new(config(4, 256)).expect("scheduler");
        let completed = Arc::new(Mutex::new(HashSet::new()));
        for index in 0..128 {
            let completed = completed.clone();
            scheduler
                .enqueue(
                    &format!("camera-{index}"),
                    1,
                    Duration::ZERO,
                    Arc::new(move |entry| {
                        completed
                            .lock()
                            .expect("completed lock")
                            .insert(entry.camera_id);
                        CatDetectionRetryOutcome::Complete
                    }),
                )
                .expect("enqueue");
        }
        wait_until(Duration::from_secs(2), || {
            completed.lock().expect("completed lock").len() == 128
        });
        assert_eq!(scheduler.worker_count(), 4);
        assert!(scheduler.active_jobs() <= 4);
        scheduler.shutdown_and_join().expect("shutdown");
        assert_eq!(scheduler.worker_count(), 0);
    }

    #[test]
    fn scheduler_coalesces_same_camera_and_never_requeues_old_generation_over_new() {
        let scheduler = CatDetectionRetryScheduler::new(config(1, 8)).expect("scheduler");
        let revisions = Arc::new(Mutex::new(Vec::new()));
        let old_calls = Arc::new(AtomicUsize::new(0));
        let old_revisions = revisions.clone();
        let old_calls_for_task = old_calls.clone();
        scheduler
            .enqueue(
                "camera-1",
                1,
                Duration::ZERO,
                Arc::new(move |entry| {
                    old_revisions
                        .lock()
                        .expect("revisions lock")
                        .push(entry.revision);
                    old_calls_for_task.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    CatDetectionRetryOutcome::Retry
                }),
            )
            .expect("old enqueue");
        wait_until(Duration::from_secs(1), || {
            old_calls.load(Ordering::SeqCst) == 1
        });
        let new_revisions = revisions.clone();
        scheduler
            .enqueue(
                "camera-1",
                2,
                Duration::ZERO,
                Arc::new(move |entry| {
                    new_revisions
                        .lock()
                        .expect("revisions lock")
                        .push(entry.revision);
                    CatDetectionRetryOutcome::Complete
                }),
            )
            .expect("new enqueue");
        assert_eq!(
            scheduler
                .enqueue(
                    "camera-1",
                    2,
                    Duration::ZERO,
                    Arc::new(|_| CatDetectionRetryOutcome::Complete),
                )
                .expect("coalesced enqueue"),
            CatDetectionRetryEnqueueResult::Coalesced
        );
        wait_until(Duration::from_secs(1), || scheduler.queued_len() == 0);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(*revisions.lock().expect("revisions lock"), vec![1, 2]);
        scheduler.shutdown_and_join().expect("shutdown");
    }

    #[test]
    fn dropping_last_scheduler_owner_shuts_down_and_joins_workers() {
        let scheduler = CatDetectionRetryScheduler::new(config(2, 8)).expect("scheduler");
        let inner = Arc::downgrade(&scheduler.inner);
        let other_owner = scheduler.clone();

        drop(scheduler);
        assert!(
            inner.upgrade().is_some(),
            "another outer owner keeps the pool alive"
        );
        drop(other_owner);

        wait_until(Duration::from_secs(2), || inner.upgrade().is_none());
    }

    #[test]
    fn worker_shutdown_does_not_join_itself_or_unbalance_owners() {
        let scheduler = CatDetectionRetryScheduler::new(config(1, 8)).expect("scheduler");
        let probe = scheduler.probe();
        let worker_owner = scheduler.clone();
        let (result_sender, result_receiver) = sync_channel(1);
        scheduler
            .enqueue(
                "camera-1",
                1,
                Duration::ZERO,
                Arc::new(move |_| {
                    let result = worker_owner.shutdown_and_join();
                    let _ = result_sender.send(result);
                    CatDetectionRetryOutcome::Complete
                }),
            )
            .expect("enqueue");
        drop(scheduler);

        result_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker shutdown returned")
            .expect("worker shutdown succeeds");
        wait_until(Duration::from_secs(2), || !probe.is_alive());
        assert_eq!(probe.outer_owners(), 0);
        assert_eq!(probe.worker_count(), 0);
    }
}
