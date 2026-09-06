//! Execution identity and ownership for the private N2 model-runtime adapter.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::{blocking::Client, Url};
use serde_json::{json, Value};
use uuid::Uuid;

use super::ai_resource_scheduler::{AiLeaseQuarantineReason, AiResourceLease};

pub const EXECUTION_ID_HEADER: &str = "X-Harbor-Execution-Id";
pub const EXECUTION_CANCEL_PREFIX: &str = "/internal/ai/executions/";
const HISTORY_CAPACITY: usize = 128;

struct ControlState {
    cancelled: AtomicBool,
    started: AtomicBool,
    deadline: Instant,
}

#[derive(Clone)]
pub struct ExecutionControl(Arc<ControlState>);

impl ExecutionControl {
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn should_stop(&self) -> bool {
        self.is_cancelled() || Instant::now() >= self.deadline()
    }

    pub fn deadline(&self) -> Instant {
        self.0.deadline
    }

    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.0.cancelled
    }
}

struct RegistryState {
    active: HashMap<String, ExecutionControl>,
    history: VecDeque<(String, bool)>,
    capacity: usize,
}

#[derive(Clone)]
pub struct ExecutionRegistry(Arc<Mutex<RegistryState>>);

impl ExecutionRegistry {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(RegistryState {
            active: HashMap::new(),
            history: VecDeque::new(),
            capacity: capacity.clamp(1, 64),
        })))
    }

    pub fn register(
        &self,
        requested_id: Option<&str>,
        deadline: Instant,
    ) -> Result<ExecutionTicket, &'static str> {
        let id = match requested_id {
            Some(id) => canonical_execution_id(id)?,
            None => Uuid::new_v4().to_string(),
        };
        let mut state = self
            .0
            .lock()
            .map_err(|_| "EXECUTION_REGISTRY_UNAVAILABLE")?;
        if state.active.contains_key(&id) || state.history.iter().any(|(old, _)| old == &id) {
            return Err("EXECUTION_ID_CONFLICT");
        }
        if state.active.len() >= state.capacity {
            return Err("EXECUTION_QUEUE_FULL");
        }
        let control = ExecutionControl(Arc::new(ControlState {
            cancelled: AtomicBool::new(false),
            started: AtomicBool::new(false),
            deadline,
        }));
        state.active.insert(id.clone(), control.clone());
        Ok(ExecutionTicket {
            registry: self.clone(),
            id,
            control,
            finished: false,
        })
    }

    /// Cancellation requests do not acknowledge process termination.
    pub fn cancel(&self, id: &str) -> Result<Value, &'static str> {
        let id = canonical_execution_id(id)?;
        let state = self
            .0
            .lock()
            .map_err(|_| "EXECUTION_REGISTRY_UNAVAILABLE")?;
        if let Some(control) = state.active.get(&id) {
            control.0.cancelled.store(true, Ordering::Release);
        }
        status_locked(&state, &id).ok_or("EXECUTION_NOT_FOUND")
    }

    pub fn status(&self, id: &str) -> Option<Value> {
        let id = canonical_execution_id(id).ok()?;
        let state = self.0.lock().ok()?;
        status_locked(&state, &id)
    }

    pub fn snapshot(&self) -> Value {
        match self.0.lock() {
            Ok(state) => json!({
                "owner": "model_runtime", "active": state.active.len(),
                "capacity": state.capacity, "retained_completions": state.history.len(),
            }),
            Err(_) => json!({"owner": "model_runtime", "available": false}),
        }
    }

    fn finish(&self, id: &str, stopped: bool) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.active.remove(id).is_some() {
            state.history.push_back((id.to_string(), stopped));
            if state.history.len() > HISTORY_CAPACITY {
                state.history.pop_front();
            }
        }
    }
}

fn canonical_execution_id(id: &str) -> Result<String, &'static str> {
    if id.len() != 36 {
        return Err("INVALID_EXECUTION_ID");
    }
    let parsed = Uuid::parse_str(id).map_err(|_| "INVALID_EXECUTION_ID")?;
    if parsed.to_string() != id {
        return Err("INVALID_EXECUTION_ID");
    }
    Ok(id.to_string())
}

fn status_locked(state: &RegistryState, id: &str) -> Option<Value> {
    if let Some(control) = state.active.get(id) {
        let status = if control.is_cancelled() {
            "cancel_requested"
        } else if control.0.started.load(Ordering::Acquire) {
            "running"
        } else {
            "queued"
        };
        return Some(json!({"execution_id": id, "state": status, "execution_stopped": false}));
    }
    state
        .history
        .iter()
        .find(|(old, _)| old == id)
        .map(|(_, stopped)| {
            json!({"execution_id": id,
            "state": if *stopped { "completed" } else { "exit_unconfirmed" },
            "execution_stopped": stopped})
        })
}

pub struct ExecutionTicket {
    registry: ExecutionRegistry,
    id: String,
    control: ExecutionControl,
    finished: bool,
}

impl ExecutionTicket {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn control(&self) -> ExecutionControl {
        self.control.clone()
    }

    pub fn mark_started(&self) {
        self.control.0.started.store(true, Ordering::Release);
    }

    pub fn finish(mut self, stopped: bool) {
        self.registry.finish(&self.id, stopped);
        self.finished = true;
    }
}

impl Drop for ExecutionTicket {
    fn drop(&mut self) {
        if !self.finished {
            self.registry
                .finish(&self.id, !self.control.0.started.load(Ordering::Acquire));
        }
    }
}

/// An execution owner must explicitly confirm completion before releasing AI resources.
pub struct ExecutionLease(Option<AiResourceLease>);

impl ExecutionLease {
    pub fn new(lease: AiResourceLease) -> Self {
        Self(Some(lease))
    }

    pub fn confirm_stopped(mut self) {
        drop(self.0.take());
    }

    pub fn quarantine(mut self, reason: AiLeaseQuarantineReason) {
        if let Some(lease) = self.0.take() {
            lease.quarantine(reason);
        }
    }
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        if let Some(lease) = self.0.take() {
            lease.quarantine(AiLeaseQuarantineReason::ProcessExitUnconfirmed);
        }
    }
}

/// Best-effort cancellation delivery only; a successful response is not a stop receipt.
pub fn request_execution_cancel(client: &Client, upstream: &Url, token: &str, id: &str) -> bool {
    if canonical_execution_id(id).is_err()
        || upstream.scheme() != "http"
        || upstream.host_str() != Some("127.0.0.1")
    {
        return false;
    }
    let Ok(url) = upstream.join(&format!("{EXECUTION_CANCEL_PREFIX}{id}/cancel")) else {
        return false;
    };
    client
        .post(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(2))
        .send()
        .is_ok_and(|response| response.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_queue_entry_never_claims_execution_has_stopped_early() {
        let registry = ExecutionRegistry::new(2);
        let ticket = registry
            .register(None, Instant::now() + Duration::from_secs(1))
            .unwrap();
        let id = ticket.id().to_string();
        assert_eq!(registry.status(&id).unwrap()["state"], "queued");
        assert_eq!(registry.cancel(&id).unwrap()["execution_stopped"], false);
        assert!(ticket.control().should_stop());
        drop(ticket);
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], true);
    }

    #[test]
    fn stale_cancel_does_not_affect_another_execution() {
        let registry = ExecutionRegistry::new(1);
        let old = registry
            .register(None, Instant::now() + Duration::from_secs(1))
            .unwrap();
        let id = old.id().to_string();
        old.mark_started();
        old.finish(true);
        let new = registry
            .register(None, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(registry.cancel(&id).unwrap()["execution_stopped"], true);
        assert!(!new.control().is_cancelled());
        assert!(registry.register(Some(&id), Instant::now()).is_err());
        assert!(registry.cancel(&Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn unexpected_active_drop_is_not_a_stop_confirmation() {
        let registry = ExecutionRegistry::new(1);
        let ticket = registry.register(None, Instant::now()).unwrap();
        let id = ticket.id().to_string();
        assert!(ticket.control().should_stop());
        ticket.mark_started();
        drop(ticket);
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], false);
    }

    #[test]
    fn registry_is_bounded_and_rejects_invalid_or_duplicate_ids() {
        let registry = ExecutionRegistry::new(1);
        assert!(registry
            .register(Some("not-a-uuid"), Instant::now())
            .is_err());
        let first = registry.register(None, Instant::now()).unwrap();
        assert!(registry.register(Some(first.id()), Instant::now()).is_err());
        assert!(registry.register(None, Instant::now()).is_err());
        drop(first);
        for _ in 0..HISTORY_CAPACITY + 1 {
            registry
                .register(None, Instant::now())
                .unwrap()
                .finish(true);
        }
        assert_eq!(
            registry.snapshot()["retained_completions"],
            HISTORY_CAPACITY
        );
    }
}
