//! Process-wide A100 cluster lease scheduler for K3 inference workloads.

use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const LEASE_QUEUE_CAPACITY_ENV: &str = "HARBOR_AI_RESOURCE_LEASE_QUEUE_CAPACITY";
const LEASE_WAIT_TIMEOUT_MS_ENV: &str = "HARBOR_AI_RESOURCE_LEASE_WAIT_TIMEOUT_MS";
const DEFAULT_LEASE_QUEUE_CAPACITY: usize = 8;
const MAX_LEASE_QUEUE_CAPACITY: usize = 64;
const DEFAULT_LEASE_WAIT_TIMEOUT_MS: u64 = 60_000;
const MAX_LEASE_WAIT_TIMEOUT_MS: u64 = 300_000;
const PRIORITY_AGING_INTERVAL: Duration = Duration::from_secs(1);
pub const AI_RESOURCE_QUEUE_MODE: &str = "ai_cluster_lease";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiWorkload {
    Yolo,
    Llm,
    CatRecordingVerifier,
    Vlm,
}

impl AiWorkload {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yolo => "yolo",
            Self::Llm => "llm",
            Self::CatRecordingVerifier => "cat_recording_verifier",
            Self::Vlm => "vlm",
        }
    }

    const fn cluster(self) -> AiCluster {
        match self {
            Self::Yolo => AiCluster::A100Cluster0,
            Self::Llm | Self::CatRecordingVerifier | Self::Vlm => AiCluster::A100Cluster1,
        }
    }

    const fn required_resources(self) -> &'static [AiResource] {
        match self {
            Self::Yolo => &[
                AiResource::A100Cluster0,
                AiResource::SpacemitOnnxRuntimeCluster0,
            ],
            Self::CatRecordingVerifier => &[
                AiResource::A100Cluster1,
                AiResource::SpacemitOnnxRuntimeCluster1,
            ],
            Self::Llm | Self::Vlm => &[AiResource::A100Cluster1],
        }
    }

    const fn resource_mask(self) -> u8 {
        let resources = self.required_resources();
        let mut mask = 0;
        let mut index = 0;
        while index < resources.len() {
            mask |= resources[index].mask();
            index += 1;
        }
        mask
    }

    const fn priority(self) -> u8 {
        match self {
            Self::Yolo | Self::Llm => 0,
            Self::CatRecordingVerifier => 5,
            Self::Vlm => 10,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Yolo => 0,
            Self::Llm => 1,
            Self::CatRecordingVerifier => 2,
            Self::Vlm => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiLeaseErrorKind {
    QueueFull,
    WaitTimeout,
    Quarantined,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiLeaseError {
    kind: AiLeaseErrorKind,
    workload: AiWorkload,
    cluster: AiCluster,
}

impl AiLeaseError {
    pub const fn kind(&self) -> AiLeaseErrorKind {
        self.kind
    }

    pub const fn code(&self) -> &'static str {
        match self.kind {
            AiLeaseErrorKind::QueueFull => "ai_resource_queue_full",
            AiLeaseErrorKind::WaitTimeout => "ai_resource_wait_timeout",
            AiLeaseErrorKind::Quarantined => "ai_resource_quarantined",
            AiLeaseErrorKind::Cancelled => "ai_resource_cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiLeaseQuarantineReason {
    ProcessExitUnconfirmed,
    InferenceTimeout,
    RuntimeFailure,
}

impl AiLeaseQuarantineReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProcessExitUnconfirmed => "process_exit_unconfirmed",
            Self::InferenceTimeout => "inference_timeout",
            Self::RuntimeFailure => "runtime_failure",
        }
    }
}

impl fmt::Display for AiLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} for {} on {}",
            self.code(),
            self.workload.as_str(),
            self.cluster.as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiCluster {
    A100Cluster0,
    A100Cluster1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiResource {
    A100Cluster0,
    A100Cluster1,
    SpacemitOnnxRuntimeCluster0,
    SpacemitOnnxRuntimeCluster1,
}

impl AiResource {
    const ALL: [Self; 4] = [
        Self::A100Cluster0,
        Self::A100Cluster1,
        Self::SpacemitOnnxRuntimeCluster0,
        Self::SpacemitOnnxRuntimeCluster1,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::A100Cluster0 => "a100_cluster_0",
            Self::A100Cluster1 => "a100_cluster_1",
            Self::SpacemitOnnxRuntimeCluster0 => "spacemit_onnx_runtime_cluster_0",
            Self::SpacemitOnnxRuntimeCluster1 => "spacemit_onnx_runtime_cluster_1",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::A100Cluster0 => 0,
            Self::A100Cluster1 => 1,
            Self::SpacemitOnnxRuntimeCluster0 => 2,
            Self::SpacemitOnnxRuntimeCluster1 => 3,
        }
    }

    const fn mask(self) -> u8 {
        1 << self.index()
    }

    const fn from_cluster(cluster: AiCluster) -> Self {
        match cluster {
            AiCluster::A100Cluster0 => Self::A100Cluster0,
            AiCluster::A100Cluster1 => Self::A100Cluster1,
        }
    }
}

impl AiCluster {
    const fn other(self) -> Self {
        match self {
            Self::A100Cluster0 => Self::A100Cluster1,
            Self::A100Cluster1 => Self::A100Cluster0,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::A100Cluster0 => "a100_cluster_0",
            Self::A100Cluster1 => "a100_cluster_1",
        }
    }

    const fn core_ids(self) -> [u8; 4] {
        match self {
            Self::A100Cluster0 => [8, 9, 10, 11],
            Self::A100Cluster1 => [12, 13, 14, 15],
        }
    }
}

#[derive(Debug)]
struct LeaseHolder {
    lease_id: u64,
    workload: AiWorkload,
    acquired_at: Instant,
}

#[derive(Debug)]
struct LeaseWaiter {
    ticket: u64,
    workload: AiWorkload,
    priority: u8,
    resource_mask: u8,
    enqueued_at: Instant,
}

impl LeaseWaiter {
    fn scheduling_key(&self) -> (u8, u64) {
        let aging = (self.enqueued_at.elapsed().as_millis() / PRIORITY_AGING_INTERVAL.as_millis())
            .min(u8::MAX as u128) as u8;
        (self.priority.saturating_sub(aging), self.ticket)
    }
}

#[derive(Debug, Default)]
struct ResourceMetrics {
    acquired_total: u64,
    released_total: u64,
    lease_quarantined_total: u64,
    queued_total: u64,
    queue_full_total: u64,
    timed_out_total: u64,
    total_wait_ms: u64,
    max_wait_ms: u64,
}

#[derive(Debug, Default)]
struct WorkloadMetrics {
    requested_total: u64,
    acquired_total: u64,
    released_total: u64,
    cross_cluster_parallel_acquired_total: u64,
    quarantine_rejected_total: u64,
    queued_total: u64,
    queue_full_total: u64,
    timed_out_total: u64,
    cancelled_total: u64,
    total_wait_ms: u64,
    max_wait_ms: u64,
}

#[derive(Debug, Default)]
struct ResourceState {
    holder: Option<LeaseHolder>,
    quarantine_reason: Option<AiLeaseQuarantineReason>,
    metrics: ResourceMetrics,
}

#[derive(Debug, Default)]
struct SchedulerState {
    next_ticket: u64,
    next_lease_id: u64,
    resources: [ResourceState; 4],
    waiters: VecDeque<LeaseWaiter>,
    workloads: [WorkloadMetrics; 4],
}

#[derive(Debug)]
struct AiResourceScheduler {
    state: Mutex<SchedulerState>,
    changed: Condvar,
    queue_capacity: usize,
    wait_timeout: Duration,
}

impl AiResourceScheduler {
    fn new(queue_capacity: usize, wait_timeout: Duration) -> Self {
        Self {
            state: Mutex::new(SchedulerState::default()),
            changed: Condvar::new(),
            queue_capacity: queue_capacity.clamp(1, MAX_LEASE_QUEUE_CAPACITY),
            wait_timeout: wait_timeout.min(Duration::from_millis(MAX_LEASE_WAIT_TIMEOUT_MS)),
        }
    }

    fn from_environment() -> Self {
        let queue_capacity = bounded_env_usize(
            LEASE_QUEUE_CAPACITY_ENV,
            DEFAULT_LEASE_QUEUE_CAPACITY,
            1,
            MAX_LEASE_QUEUE_CAPACITY,
        );
        let wait_timeout_ms = bounded_env_u64(
            LEASE_WAIT_TIMEOUT_MS_ENV,
            DEFAULT_LEASE_WAIT_TIMEOUT_MS,
            1,
            MAX_LEASE_WAIT_TIMEOUT_MS,
        );
        Self::new(queue_capacity, Duration::from_millis(wait_timeout_ms))
    }

    fn acquire(self: &Arc<Self>, workload: AiWorkload) -> Result<AiResourceLease, AiLeaseError> {
        self.acquire_with_control(workload, None)
    }

    fn acquire_with_control(
        self: &Arc<Self>,
        workload: AiWorkload,
        control: Option<(Instant, &AtomicBool)>,
    ) -> Result<AiResourceLease, AiLeaseError> {
        let cluster = workload.cluster();
        let resource_mask = workload.resource_mask();
        let workload_index = workload.index();
        let mut deadline = control
            .map(|(deadline, _)| deadline.min(Instant::now() + self.wait_timeout))
            .unwrap_or_else(|| Instant::now() + self.wait_timeout);
        let mut state = self.lock_state();
        state.workloads[workload_index].requested_total += 1;

        if let Some(kind) = interrupted_lease_wait(control, deadline) {
            record_interrupted_wait(&mut state, workload, kind);
            return Err(AiLeaseError {
                kind,
                workload,
                cluster,
            });
        }

        if required_resources_are_quarantined(&state, workload) {
            state.workloads[workload_index].quarantine_rejected_total += 1;
            return Err(AiLeaseError {
                kind: AiLeaseErrorKind::Quarantined,
                workload,
                cluster,
            });
        }

        if can_grant_new_request(&state, workload) {
            return Ok(self.grant(&mut state, workload, resource_mask, 0));
        }

        if workload.required_resources().iter().any(|resource| {
            queued_count_for_resource(&state.waiters, *resource) >= self.queue_capacity
        }) {
            for resource in workload.required_resources() {
                state.resources[resource.index()].metrics.queue_full_total += 1;
            }
            state.workloads[workload_index].queue_full_total += 1;
            return Err(AiLeaseError {
                kind: AiLeaseErrorKind::QueueFull,
                workload,
                cluster,
            });
        }

        state.next_ticket = state.next_ticket.wrapping_add(1).max(1);
        let ticket = state.next_ticket;
        state.waiters.push_back(LeaseWaiter {
            ticket,
            workload,
            priority: workload.priority(),
            resource_mask,
            enqueued_at: Instant::now(),
        });
        for resource in workload.required_resources() {
            state.resources[resource.index()].metrics.queued_total += 1;
        }
        state.workloads[workload_index].queued_total += 1;
        if control.is_none() {
            deadline = Instant::now() + self.wait_timeout;
        }
        loop {
            if let Some(kind) = interrupted_lease_wait(control, deadline) {
                remove_waiter(&mut state.waiters, ticket);
                record_interrupted_wait(&mut state, workload, kind);
                self.changed.notify_all();
                return Err(AiLeaseError {
                    kind,
                    workload,
                    cluster,
                });
            }
            if required_resources_are_quarantined(&state, workload) {
                remove_waiter(&mut state.waiters, ticket);
                state.workloads[workload_index].quarantine_rejected_total += 1;
                self.changed.notify_all();
                return Err(AiLeaseError {
                    kind: AiLeaseErrorKind::Quarantined,
                    workload,
                    cluster,
                });
            }

            if waiter_can_be_granted(&state, ticket) {
                let waiter_index = state
                    .waiters
                    .iter()
                    .position(|waiter| waiter.ticket == ticket)
                    .expect("selected waiter index");
                let waiter = state.waiters.remove(waiter_index).expect("selected waiter");
                let waited_ms = duration_millis(waiter.enqueued_at.elapsed());
                return Ok(self.grant(&mut state, workload, waiter.resource_mask, waited_ms));
            }

            let now = Instant::now();
            if now >= deadline {
                remove_waiter(&mut state.waiters, ticket);
                for resource in workload.required_resources() {
                    state.resources[resource.index()].metrics.timed_out_total += 1;
                }
                state.workloads[workload_index].timed_out_total += 1;
                self.changed.notify_all();
                return Err(AiLeaseError {
                    kind: AiLeaseErrorKind::WaitTimeout,
                    workload,
                    cluster,
                });
            }

            let remaining = deadline.saturating_duration_since(now);
            let remaining = if control.is_some() {
                remaining.min(Duration::from_millis(20))
            } else {
                remaining
            };
            let wait_result = self.changed.wait_timeout(state, remaining);
            let (next_state, _) = wait_result.unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
        }
    }

    fn grant(
        self: &Arc<Self>,
        state: &mut SchedulerState,
        workload: AiWorkload,
        resource_mask: u8,
        waited_ms: u64,
    ) -> AiResourceLease {
        state.next_lease_id = state.next_lease_id.wrapping_add(1).max(1);
        let lease_id = state.next_lease_id;
        let acquired_at = Instant::now();
        let other_cluster_busy = state.resources
            [AiResource::from_cluster(workload.cluster().other()).index()]
        .holder
        .is_some();
        for resource in resources_in_mask(resource_mask) {
            let resource_state = &mut state.resources[resource.index()];
            resource_state.holder = Some(LeaseHolder {
                lease_id,
                workload,
                acquired_at,
            });
            resource_state.metrics.acquired_total += 1;
            resource_state.metrics.total_wait_ms = resource_state
                .metrics
                .total_wait_ms
                .saturating_add(waited_ms);
            resource_state.metrics.max_wait_ms = resource_state.metrics.max_wait_ms.max(waited_ms);
        }

        let workload_metrics = &mut state.workloads[workload.index()];
        workload_metrics.acquired_total += 1;
        if other_cluster_busy {
            workload_metrics.cross_cluster_parallel_acquired_total += 1;
        }
        workload_metrics.total_wait_ms = workload_metrics.total_wait_ms.saturating_add(waited_ms);
        workload_metrics.max_wait_ms = workload_metrics.max_wait_ms.max(waited_ms);
        AiResourceLease {
            scheduler: Arc::clone(self),
            resource_mask,
            workload,
            lease_id,
            released: false,
        }
    }

    fn release(&self, resource_mask: u8, workload: AiWorkload, lease_id: u64) {
        let mut state = self.lock_state();
        let mut released = false;
        for resource in resources_in_mask(resource_mask) {
            let resource_state = &mut state.resources[resource.index()];
            if resource_state
                .holder
                .as_ref()
                .is_some_and(|holder| holder.lease_id == lease_id)
            {
                resource_state.holder = None;
                resource_state.metrics.released_total += 1;
                released = true;
            }
        }
        if released {
            state.workloads[workload.index()].released_total += 1;
        }
        drop(state);
        if released {
            self.changed.notify_all();
        }
    }

    fn quarantine(
        &self,
        resource_mask: u8,
        workload: AiWorkload,
        lease_id: u64,
        reason: AiLeaseQuarantineReason,
    ) {
        let mut state = self.lock_state();
        let mut quarantined = false;
        for resource in resources_in_mask(resource_mask) {
            let resource_state = &mut state.resources[resource.index()];
            if resource_state
                .holder
                .as_ref()
                .is_some_and(|holder| holder.lease_id == lease_id)
            {
                resource_state.quarantine_reason = Some(reason);
                resource_state.metrics.lease_quarantined_total += 1;
                quarantined = true;
            }
        }
        drop(state);
        if quarantined {
            eprintln!(
                "K3 AI resource lease quarantined: workload={} reason={}",
                workload.as_str(),
                reason.as_str()
            );
            self.changed.notify_all();
        }
    }

    fn snapshot(&self) -> Value {
        let state = self.lock_state();
        json!({
            "kind": "k3_ai_resource_scheduler_v2",
            "mode": "bounded_aging_priority_fifo_lease",
            "priority_aging_interval_ms": duration_millis(PRIORITY_AGING_INTERVAL),
            "model_residency_holds_lease": false,
            "cluster_1_policy": "request_scoped_shared_llm_cat_verifier_vlm",
            "atomic_multi_resource_leases": true,
            "queue_capacity_per_cluster": self.queue_capacity,
            "queue_capacity_per_resource": self.queue_capacity,
            "wait_timeout_ms": duration_millis(self.wait_timeout),
            "cpu_fallback_enabled": false,
            "clusters": [
                cluster_snapshot(&state, AiCluster::A100Cluster0),
                cluster_snapshot(&state, AiCluster::A100Cluster1),
            ],
            "resource_domains": {
                "spacemit_onnx_runtime_cluster_0": resource_domain_snapshot(
                    &state,
                    AiResource::SpacemitOnnxRuntimeCluster0,
                ),
                "spacemit_onnx_runtime_cluster_1": resource_domain_snapshot(
                    &state,
                    AiResource::SpacemitOnnxRuntimeCluster1,
                ),
            },
            "workloads": {
                "yolo": workload_snapshot(&state, AiWorkload::Yolo, self),
                "llm": workload_snapshot(&state, AiWorkload::Llm, self),
                "cat_recording_verifier": workload_snapshot(
                    &state,
                    AiWorkload::CatRecordingVerifier,
                    self,
                ),
                "vlm": workload_snapshot(&state, AiWorkload::Vlm, self),
            },
            "metadata_only": true,
            "secret_scan": "clean",
        })
    }

    fn workload_snapshot(&self, workload: AiWorkload) -> Value {
        let state = self.lock_state();
        workload_snapshot(&state, workload, self)
    }

    fn lock_state(&self) -> MutexGuard<'_, SchedulerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug)]
pub struct AiResourceLease {
    scheduler: Arc<AiResourceScheduler>,
    resource_mask: u8,
    workload: AiWorkload,
    lease_id: u64,
    released: bool,
}

impl Drop for AiResourceLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.scheduler
            .release(self.resource_mask, self.workload, self.lease_id);
        self.released = true;
    }
}

impl AiResourceLease {
    pub fn quarantine(mut self, reason: AiLeaseQuarantineReason) {
        self.scheduler
            .quarantine(self.resource_mask, self.workload, self.lease_id, reason);
        self.released = true;
    }
}

pub fn acquire_ai_resource_lease(workload: AiWorkload) -> Result<AiResourceLease, AiLeaseError> {
    global_scheduler().acquire(workload)
}

pub fn acquire_ai_resource_lease_until(
    workload: AiWorkload,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<AiResourceLease, AiLeaseError> {
    global_scheduler().acquire_with_control(workload, Some((deadline, cancelled)))
}

fn interrupted_lease_wait(
    control: Option<(Instant, &AtomicBool)>,
    deadline: Instant,
) -> Option<AiLeaseErrorKind> {
    control.and_then(|(_, cancelled)| {
        if cancelled.load(Ordering::Acquire) {
            Some(AiLeaseErrorKind::Cancelled)
        } else if Instant::now() >= deadline {
            Some(AiLeaseErrorKind::WaitTimeout)
        } else {
            None
        }
    })
}

fn record_interrupted_wait(
    state: &mut SchedulerState,
    workload: AiWorkload,
    kind: AiLeaseErrorKind,
) {
    if kind == AiLeaseErrorKind::Cancelled {
        state.workloads[workload.index()].cancelled_total += 1;
    } else {
        state.workloads[workload.index()].timed_out_total += 1;
        for resource in workload.required_resources() {
            state.resources[resource.index()].metrics.timed_out_total += 1;
        }
    }
}

pub fn ai_resource_scheduler_snapshot() -> Value {
    global_scheduler().snapshot()
}

pub fn ai_resource_workload_snapshot(workload: AiWorkload) -> Value {
    global_scheduler().workload_snapshot(workload)
}

fn global_scheduler() -> &'static Arc<AiResourceScheduler> {
    static SCHEDULER: OnceLock<Arc<AiResourceScheduler>> = OnceLock::new();
    SCHEDULER.get_or_init(|| Arc::new(AiResourceScheduler::from_environment()))
}

fn cluster_snapshot(state: &SchedulerState, cluster: AiCluster) -> Value {
    let resource = AiResource::from_cluster(cluster);
    let resource_state = &state.resources[resource.index()];
    let holder = resource_state.holder.as_ref().map(|holder| {
        json!({
            "workload": holder.workload.as_str(),
            "lease_age_ms": duration_millis(holder.acquired_at.elapsed()),
        })
    });
    let waiting = waiting_by_workload(state, resource);
    json!({
        "cluster_id": cluster.as_str(),
        "core_ids": cluster.core_ids(),
        "busy": holder.is_some(),
        "holder": holder,
        "quarantined": resource_state.quarantine_reason.is_some(),
        "quarantine_reason": resource_state.quarantine_reason.map(AiLeaseQuarantineReason::as_str),
        "queue_depth": queued_count_for_resource(&state.waiters, resource),
        "waiting_by_workload": waiting,
        "acquired_total": resource_state.metrics.acquired_total,
        "released_total": resource_state.metrics.released_total,
        "lease_quarantined_total": resource_state.metrics.lease_quarantined_total,
        "queued_total": resource_state.metrics.queued_total,
        "queue_full_total": resource_state.metrics.queue_full_total,
        "timed_out_total": resource_state.metrics.timed_out_total,
        "total_wait_ms": resource_state.metrics.total_wait_ms,
        "max_wait_ms": resource_state.metrics.max_wait_ms,
    })
}

fn resource_domain_snapshot(state: &SchedulerState, resource: AiResource) -> Value {
    let resource_state = &state.resources[resource.index()];
    let holder = resource_state.holder.as_ref().map(|holder| {
        json!({
            "workload": holder.workload.as_str(),
            "lease_age_ms": duration_millis(holder.acquired_at.elapsed()),
        })
    });
    json!({
        "resource_id": resource.as_str(),
        "busy": holder.is_some(),
        "holder": holder,
        "quarantined": resource_state.quarantine_reason.is_some(),
        "quarantine_reason": resource_state.quarantine_reason.map(AiLeaseQuarantineReason::as_str),
        "queue_depth": queued_count_for_resource(&state.waiters, resource),
        "waiting_by_workload": waiting_by_workload(state, resource),
        "acquired_total": resource_state.metrics.acquired_total,
        "released_total": resource_state.metrics.released_total,
        "lease_quarantined_total": resource_state.metrics.lease_quarantined_total,
        "queued_total": resource_state.metrics.queued_total,
        "queue_full_total": resource_state.metrics.queue_full_total,
        "timed_out_total": resource_state.metrics.timed_out_total,
        "total_wait_ms": resource_state.metrics.total_wait_ms,
        "max_wait_ms": resource_state.metrics.max_wait_ms,
    })
}

fn workload_snapshot(
    state: &SchedulerState,
    workload: AiWorkload,
    scheduler: &AiResourceScheduler,
) -> Value {
    let metrics = &state.workloads[workload.index()];
    let cluster = workload.cluster();
    let cluster_resource = AiResource::from_cluster(cluster);
    let cluster_state = &state.resources[cluster_resource.index()];
    let required_resource_ids = workload
        .required_resources()
        .iter()
        .map(|resource| resource.as_str())
        .collect::<Vec<_>>();
    let blocked_resource_ids = workload
        .required_resources()
        .iter()
        .filter(|resource| state.resources[resource.index()].holder.is_some())
        .map(|resource| resource.as_str())
        .collect::<Vec<_>>();
    json!({
        "workload": workload.as_str(),
        "cluster_id": cluster.as_str(),
        "core_ids": cluster.core_ids(),
        "busy": cluster_state.holder.is_some(),
        "holder_workload": cluster_state.holder.as_ref().map(|holder| holder.workload.as_str()),
        "queue_depth": queued_count_for_resource(&state.waiters, cluster_resource),
        "required_resource_ids": required_resource_ids,
        "blocked_resource_ids": blocked_resource_ids,
        "atomic_resource_lease": workload.required_resources().len() > 1,
        "mode": "bounded_aging_priority_fifo_lease",
        "queue_capacity": scheduler.queue_capacity,
        "wait_timeout_ms": duration_millis(scheduler.wait_timeout),
        "started_total": metrics.acquired_total,
        "completed_total": metrics.released_total,
        "busy_total": metrics.queued_total + metrics.queue_full_total + metrics.quarantine_rejected_total,
        "failed_total": metrics.queue_full_total + metrics.timed_out_total + metrics.quarantine_rejected_total,
        "cancelled_total": metrics.cancelled_total,
        "requested_total": metrics.requested_total,
        "cross_cluster_parallel_acquired_total": metrics.cross_cluster_parallel_acquired_total,
        "quarantine_rejected_total": metrics.quarantine_rejected_total,
        "queued_total": metrics.queued_total,
        "queue_full_total": metrics.queue_full_total,
        "timed_out_total": metrics.timed_out_total,
        "total_wait_ms": metrics.total_wait_ms,
        "max_wait_ms": metrics.max_wait_ms,
        "cpu_fallback_enabled": false,
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

fn waiting_by_workload(
    state: &SchedulerState,
    resource: AiResource,
) -> serde_json::Map<String, Value> {
    [
        AiWorkload::Yolo,
        AiWorkload::Llm,
        AiWorkload::CatRecordingVerifier,
        AiWorkload::Vlm,
    ]
    .into_iter()
    .map(|workload| {
        (
            workload.as_str().to_string(),
            json!(state
                .waiters
                .iter()
                .filter(|waiter| {
                    waiter.workload == workload && waiter.resource_mask & resource.mask() != 0
                })
                .count()),
        )
    })
    .collect()
}

fn resources_in_mask(resource_mask: u8) -> impl Iterator<Item = AiResource> {
    AiResource::ALL
        .into_iter()
        .filter(move |resource| resource_mask & resource.mask() != 0)
}

fn resources_are_free(state: &SchedulerState, resource_mask: u8) -> bool {
    resources_in_mask(resource_mask).all(|resource| {
        let resource_state = &state.resources[resource.index()];
        resource_state.holder.is_none() && resource_state.quarantine_reason.is_none()
    })
}

fn required_resources_are_quarantined(state: &SchedulerState, workload: AiWorkload) -> bool {
    workload.required_resources().iter().any(|resource| {
        state.resources[resource.index()]
            .quarantine_reason
            .is_some()
    })
}

fn resources_overlap(left: u8, right: u8) -> bool {
    left & right != 0
}

fn can_grant_new_request(state: &SchedulerState, workload: AiWorkload) -> bool {
    let resource_mask = workload.resource_mask();
    resources_are_free(state, resource_mask)
        && !state.waiters.iter().any(|waiter| {
            resources_overlap(waiter.resource_mask, resource_mask)
                && resources_are_free(state, waiter.resource_mask)
                && waiter.scheduling_key() <= (workload.priority(), u64::MAX)
        })
}

fn waiter_can_be_granted(state: &SchedulerState, ticket: u64) -> bool {
    let Some(waiter) = state.waiters.iter().find(|waiter| waiter.ticket == ticket) else {
        return false;
    };
    resources_are_free(state, waiter.resource_mask)
        && !state.waiters.iter().any(|other| {
            other.ticket != waiter.ticket
                && resources_overlap(other.resource_mask, waiter.resource_mask)
                && resources_are_free(state, other.resource_mask)
                && other.scheduling_key() < waiter.scheduling_key()
        })
}

fn queued_count_for_resource(waiters: &VecDeque<LeaseWaiter>, resource: AiResource) -> usize {
    waiters
        .iter()
        .filter(|waiter| waiter.resource_mask & resource.mask() != 0)
        .count()
}

fn remove_waiter(waiters: &mut VecDeque<LeaseWaiter>, ticket: u64) {
    if let Some(index) = waiters.iter().position(|waiter| waiter.ticket == ticket) {
        waiters.remove(index);
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn bounded_env_usize(name: &str, default: usize, minimum: usize, maximum: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.clamp(minimum, maximum))
        .unwrap_or(default)
}

fn bounded_env_u64(name: &str, default: u64, minimum: u64, maximum: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(minimum, maximum))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    fn wait_for_queue_depth(scheduler: &AiResourceScheduler, depth: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if scheduler.snapshot()["clusters"][1]["queue_depth"] == json!(depth) {
                return;
            }
            thread::yield_now();
        }
        panic!("queue did not reach depth {depth}");
    }

    #[test]
    fn different_four_core_clusters_can_be_leased_together() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(1)));
        let yolo = scheduler.acquire(AiWorkload::Yolo).expect("YOLO lease");
        let llm = scheduler.acquire(AiWorkload::Llm).expect("LLM lease");

        let snapshot = scheduler.snapshot();
        assert_eq!(snapshot["clusters"][0]["core_ids"], json!([8, 9, 10, 11]));
        assert_eq!(snapshot["clusters"][1]["core_ids"], json!([12, 13, 14, 15]));
        assert_eq!(snapshot["clusters"][0]["busy"], true);
        assert_eq!(snapshot["clusters"][1]["busy"], true);

        drop((yolo, llm));
    }

    #[test]
    fn yolo_and_cat_verifier_use_independent_cluster_runtime_domains() {
        let scheduler = Arc::new(AiResourceScheduler::new(1, Duration::from_millis(10)));
        let yolo = scheduler.acquire(AiWorkload::Yolo).expect("YOLO lease");
        let cat = scheduler
            .acquire(AiWorkload::CatRecordingVerifier)
            .expect("classifier must run on cluster one while YOLO holds cluster zero");

        let snapshot = scheduler.snapshot();
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_0"]["holder"]["workload"],
            "yolo",
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_1"]["holder"]["workload"],
            "cat_recording_verifier",
        );
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]
                ["cross_cluster_parallel_acquired_total"],
            1
        );

        drop((cat, yolo));
    }

    #[test]
    fn snapshot_reports_cluster_scoped_runtime_domain_requirements() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(1)));
        let yolo = scheduler.acquire(AiWorkload::Yolo).expect("YOLO lease");

        let snapshot = scheduler.snapshot();
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_0"]["busy"],
            true
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_0"]["holder"]["workload"],
            "yolo"
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_1"]["busy"],
            false
        );
        assert_eq!(
            snapshot["workloads"]["yolo"]["required_resource_ids"],
            json!(["a100_cluster_0", "spacemit_onnx_runtime_cluster_0"])
        );
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]["required_resource_ids"],
            json!(["a100_cluster_1", "spacemit_onnx_runtime_cluster_1"])
        );
        assert_eq!(
            snapshot["workloads"]["llm"]["required_resource_ids"],
            json!(["a100_cluster_1"])
        );

        drop(yolo);
        assert_eq!(
            scheduler.snapshot()["resource_domains"]["spacemit_onnx_runtime_cluster_0"]["busy"],
            false
        );
    }

    #[test]
    fn quarantining_classifier_blocks_only_cluster_one_runtime_domain() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_millis(10)));
        let yolo = scheduler.acquire(AiWorkload::Yolo).expect("YOLO lease");
        let classifier = scheduler
            .acquire(AiWorkload::CatRecordingVerifier)
            .expect("classifier lease");

        classifier.quarantine(AiLeaseQuarantineReason::ProcessExitUnconfirmed);

        let error = scheduler
            .acquire(AiWorkload::CatRecordingVerifier)
            .expect_err("quarantined classifier domain must fail closed");
        assert_eq!(error.kind(), AiLeaseErrorKind::Quarantined);
        assert_eq!(error.code(), "ai_resource_quarantined");
        let llm_error = scheduler
            .acquire(AiWorkload::Llm)
            .expect_err("unconfirmed classifier process must fence cluster one");
        assert_eq!(llm_error.kind(), AiLeaseErrorKind::Quarantined);
        let snapshot = scheduler.snapshot();
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_1"]["quarantined"],
            true
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_1"]["quarantine_reason"],
            "process_exit_unconfirmed"
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_1"]
                ["lease_quarantined_total"],
            1
        );
        assert_eq!(
            snapshot["resource_domains"]["spacemit_onnx_runtime_cluster_0"]["quarantined"],
            false
        );
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]["quarantine_rejected_total"],
            1
        );
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]["failed_total"],
            1
        );
        assert_eq!(snapshot["workloads"]["llm"]["quarantine_rejected_total"], 1);
        assert_eq!(snapshot["workloads"]["llm"]["failed_total"], 1);
        assert_eq!(snapshot["workloads"]["yolo"]["holder_workload"], "yolo");

        drop(yolo);
    }

    #[test]
    fn queued_cluster_one_waiter_fails_immediately_when_domain_is_quarantined() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(2)));
        let classifier = scheduler
            .acquire(AiWorkload::CatRecordingVerifier)
            .expect("classifier lease");
        let (result_tx, result_rx) = mpsc::channel();
        let waiter_scheduler = Arc::clone(&scheduler);
        let waiter = thread::spawn(move || {
            result_tx
                .send(waiter_scheduler.acquire(AiWorkload::Llm))
                .expect("report LLM result");
        });
        wait_for_queue_depth(&scheduler, 1);

        classifier.quarantine(AiLeaseQuarantineReason::ProcessExitUnconfirmed);

        let error = result_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("queued waiter must wake after quarantine")
            .expect_err("queued waiter must fail closed");
        assert_eq!(error.kind(), AiLeaseErrorKind::Quarantined);
        assert_eq!(scheduler.snapshot()["clusters"][1]["queue_depth"], 0);
        waiter.join().expect("LLM waiter");
    }

    #[test]
    fn cat_recording_verifier_is_visible_and_shares_cluster_one() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_millis(10)));
        let holder = scheduler.acquire(AiWorkload::Llm).expect("LLM lease");

        let error = scheduler
            .acquire(AiWorkload::CatRecordingVerifier)
            .expect_err("classifier must serialize with LLM");
        assert_eq!(error.kind(), AiLeaseErrorKind::WaitTimeout);

        let snapshot = scheduler.snapshot();
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]["cluster_id"],
            "a100_cluster_1"
        );
        assert_eq!(
            snapshot["workloads"]["cat_recording_verifier"]["core_ids"],
            json!([12, 13, 14, 15])
        );
        drop(holder);
    }

    #[test]
    fn interactive_llm_precedes_earlier_background_vlm_waiter() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(2)));
        let holder = scheduler.acquire(AiWorkload::Llm).expect("holder lease");
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_vlm_tx, release_vlm_rx) = mpsc::channel();
        let (release_llm_tx, release_llm_rx) = mpsc::channel();

        let vlm_scheduler = Arc::clone(&scheduler);
        let vlm_acquired = acquired_tx.clone();
        let vlm_thread = thread::spawn(move || {
            let lease = vlm_scheduler.acquire(AiWorkload::Vlm).expect("VLM lease");
            vlm_acquired.send(AiWorkload::Vlm).expect("report VLM");
            release_vlm_rx.recv().expect("release VLM");
            drop(lease);
        });
        wait_for_queue_depth(&scheduler, 1);

        let llm_scheduler = Arc::clone(&scheduler);
        let llm_thread = thread::spawn(move || {
            let lease = llm_scheduler.acquire(AiWorkload::Llm).expect("LLM lease");
            acquired_tx.send(AiWorkload::Llm).expect("report LLM");
            release_llm_rx.recv().expect("release LLM");
            drop(lease);
        });
        wait_for_queue_depth(&scheduler, 2);
        drop(holder);

        assert_eq!(
            acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            AiWorkload::Llm
        );
        release_llm_tx.send(()).unwrap();
        assert_eq!(
            acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            AiWorkload::Vlm
        );
        release_vlm_tx.send(()).unwrap();
        llm_thread.join().unwrap();
        vlm_thread.join().unwrap();
    }

    #[test]
    fn bounded_queue_rejects_excess_waiters() {
        let scheduler = Arc::new(AiResourceScheduler::new(1, Duration::from_secs(2)));
        let holder = scheduler.acquire(AiWorkload::Llm).expect("holder lease");
        let waiter_scheduler = Arc::clone(&scheduler);
        let waiter = thread::spawn(move || waiter_scheduler.acquire(AiWorkload::Vlm));
        wait_for_queue_depth(&scheduler, 1);

        let error = scheduler
            .acquire(AiWorkload::Llm)
            .expect_err("queue must be bounded");
        assert_eq!(error.kind(), AiLeaseErrorKind::QueueFull);
        drop(holder);
        drop(waiter.join().unwrap().expect("queued VLM lease"));
    }

    #[test]
    fn aged_background_work_cannot_be_starved_by_continuous_llm_requests() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(2)));
        let holder = scheduler.acquire(AiWorkload::Llm).unwrap();
        let other = Arc::clone(&scheduler);
        let (acquired, received) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let lease = other.acquire(AiWorkload::Vlm).unwrap();
            acquired.send(()).unwrap();
            drop(lease);
        });
        wait_for_queue_depth(&scheduler, 1);
        {
            let mut state = scheduler.lock_state();
            state.waiters.front_mut().unwrap().enqueued_at =
                Instant::now() - Duration::from_secs(11);
        }
        drop(holder);
        let llm = scheduler.acquire(AiWorkload::Llm).unwrap();
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(llm);
        waiter.join().unwrap();
    }

    #[test]
    fn timed_out_waiter_is_removed_from_queue() {
        let scheduler = Arc::new(AiResourceScheduler::new(2, Duration::from_millis(10)));
        let holder = scheduler.acquire(AiWorkload::Llm).expect("holder lease");

        let error = scheduler
            .acquire(AiWorkload::Vlm)
            .expect_err("VLM wait must time out");
        assert_eq!(error.kind(), AiLeaseErrorKind::WaitTimeout);
        assert_eq!(scheduler.snapshot()["clusters"][1]["queue_depth"], 0);
        drop(holder);
    }

    #[test]
    fn cancelled_execution_leaves_queue_without_granting_resources() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(2)));
        let holder = scheduler.acquire(AiWorkload::Llm).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let flag = cancellation.clone();
        let other = scheduler.clone();
        let waiter = thread::spawn(move || {
            other.acquire_with_control(
                AiWorkload::CatRecordingVerifier,
                Some((Instant::now() + Duration::from_secs(2), &flag)),
            )
        });
        wait_for_queue_depth(&scheduler, 1);
        cancellation.store(true, Ordering::Release);
        assert_eq!(
            waiter.join().unwrap().unwrap_err().kind(),
            AiLeaseErrorKind::Cancelled
        );
        assert_eq!(scheduler.snapshot()["clusters"][1]["queue_depth"], 0);
        assert_eq!(
            scheduler.snapshot()["workloads"]["cat_recording_verifier"]["started_total"],
            0
        );
        assert_eq!(
            scheduler.snapshot()["workloads"]["cat_recording_verifier"]["cancelled_total"],
            1
        );
        drop(holder);
        drop(scheduler.acquire(AiWorkload::CatRecordingVerifier).unwrap());
    }

    #[test]
    fn expired_or_cancelled_execution_cannot_take_even_a_free_resource() {
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_secs(2)));
        let flag = AtomicBool::new(false);
        let deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            scheduler
                .acquire_with_control(AiWorkload::Llm, Some((deadline, &flag)))
                .unwrap_err()
                .kind(),
            AiLeaseErrorKind::WaitTimeout
        );
        flag.store(true, Ordering::Release);
        assert_eq!(
            scheduler
                .acquire_with_control(
                    AiWorkload::Llm,
                    Some((Instant::now() + Duration::from_secs(1), &flag))
                )
                .unwrap_err()
                .kind(),
            AiLeaseErrorKind::Cancelled
        );
        assert_eq!(scheduler.snapshot()["clusters"][1]["busy"], false);
    }

    #[test]
    fn execution_owner_must_confirm_stopped_before_lease_release() {
        use crate::runtime::ai_execution::ExecutionLease;
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_millis(5)));
        let execution = ExecutionLease::new(scheduler.acquire(AiWorkload::Llm).unwrap());
        assert_eq!(
            scheduler
                .acquire(AiWorkload::CatRecordingVerifier)
                .unwrap_err()
                .kind(),
            AiLeaseErrorKind::WaitTimeout
        );
        execution.confirm_stopped();
        drop(scheduler.acquire(AiWorkload::CatRecordingVerifier).unwrap());
        assert_eq!(scheduler.snapshot()["clusters"][1]["quarantined"], false);
    }

    #[test]
    fn unexpected_execution_owner_drop_quarantines_the_shared_cluster() {
        use crate::runtime::ai_execution::ExecutionLease;
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_millis(5)));
        drop(ExecutionLease::new(
            scheduler.acquire(AiWorkload::Llm).unwrap(),
        ));
        assert_eq!(
            scheduler
                .acquire(AiWorkload::CatRecordingVerifier)
                .unwrap_err()
                .kind(),
            AiLeaseErrorKind::Quarantined
        );
        assert_eq!(
            scheduler.snapshot()["clusters"][1]["quarantine_reason"],
            "process_exit_unconfirmed"
        );
        drop(scheduler.acquire(AiWorkload::Yolo).unwrap());
    }

    #[test]
    fn confirmed_process_exit_does_not_clear_runtime_failure_quarantine() {
        use crate::runtime::ai_execution::ExecutionLease;
        let scheduler = Arc::new(AiResourceScheduler::new(4, Duration::from_millis(5)));
        ExecutionLease::new(scheduler.acquire(AiWorkload::CatRecordingVerifier).unwrap())
            .quarantine(AiLeaseQuarantineReason::RuntimeFailure);
        assert_eq!(
            scheduler.acquire(AiWorkload::Llm).unwrap_err().kind(),
            AiLeaseErrorKind::Quarantined
        );
        assert_eq!(
            scheduler.snapshot()["clusters"][1]["quarantine_reason"],
            "runtime_failure"
        );
    }
}
