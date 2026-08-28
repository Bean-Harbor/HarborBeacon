//! Durable package lifecycle confirmation state for a configured delivery zone.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runtime::secure_store_path::{SecureFileIdentity, SecureStorePath};

pub const PACKAGE_EVENT_STORE_PATH_ENV: &str = "HARBOR_K3_PACKAGE_EVENT_STORE_PATH";
const DEFAULT_STORE_PATH: &str = ".harborbeacon/package-events.json";
const MAX_STORE_BYTES: u64 = 1024 * 1024;
const MAX_CAMERAS: usize = 256;
const MAX_LIFECYCLE_EVENTS: usize = 2_048;
pub const DEFAULT_CONFIRM_FRAMES: u32 = 3;
pub const DEFAULT_CONFIRM_WINDOW_MS: u64 = 3_000;
pub const DEFAULT_MAX_RESULT_AGE_MS: u64 = 3_000;
const PACKAGE_CLEAR_CONFIRM_FRAMES: u32 = 10;
const PACKAGE_CLEAR_CONFIRM_DURATION_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PackageDeliveryZone {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Default for PackageDeliveryZone {
    fn default() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageEventConfig {
    pub camera_id: String,
    pub enabled: bool,
    pub zone: PackageDeliveryZone,
    pub confirm_frames: u32,
    pub confirm_window_ms: u64,
    pub max_result_age_ms: u64,
    pub revision: u128,
}

impl PackageEventConfig {
    pub fn new(
        camera_id: impl Into<String>,
        enabled: bool,
        zone: PackageDeliveryZone,
        revision: u128,
    ) -> Result<Self, String> {
        let config = Self {
            camera_id: camera_id.into(),
            enabled,
            zone,
            confirm_frames: DEFAULT_CONFIRM_FRAMES,
            confirm_window_ms: DEFAULT_CONFIRM_WINDOW_MS,
            max_result_age_ms: DEFAULT_MAX_RESULT_AGE_MS,
            revision,
        };
        validate_config(&config)?;
        Ok(config)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackagePresencePhase {
    #[default]
    Idle,
    Candidate,
    Present,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageEventArtifact {
    pub artifact_id: String,
    pub mime_type: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PackageLifecycleEventKind {
    Appeared,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageLifecycleEvent {
    pub event_id: String,
    pub kind: PackageLifecycleEventKind,
    pub camera_id: String,
    pub instance_id: String,
    #[serde(default)]
    pub related_event_id: Option<String>,
    pub config_revision: u128,
    pub observed_frame_epoch_ms: u64,
    pub confidence: f64,
    pub zone: PackageDeliveryZone,
    pub confirm_frames: u32,
    pub confirm_window_ms: u64,
    #[serde(default)]
    pub artifact: Option<PackageEventArtifact>,
    #[serde(default)]
    pub event_persisted: bool,
    #[serde(default)]
    pub delivered: bool,
    #[serde(default)]
    pub last_delivery_attempt_epoch_ms: u64,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PackageClearCandidate {
    count: u32,
    first_frame_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageEventState {
    pub camera_id: String,
    pub config_revision: u128,
    #[serde(default)]
    pub phase: PackagePresencePhase,
    #[serde(default)]
    pub last_sequence: Option<u64>,
    #[serde(default)]
    pub last_worker_started_epoch_ms: u64,
    #[serde(default)]
    pub candidate_count: u32,
    #[serde(default)]
    pub candidate_first_frame_epoch_ms: u64,
    #[serde(default)]
    clear_candidate: PackageClearCandidate,
    #[serde(default)]
    pub confirmed_frame_epoch_ms: u64,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub artifact: Option<PackageEventArtifact>,
    #[serde(default)]
    pub event_persisted: bool,
    #[serde(default)]
    pub delivered: bool,
    #[serde(default)]
    pub last_delivery_attempt_epoch_ms: u64,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl PackageEventState {
    fn idle(camera_id: &str, config_revision: u128) -> Self {
        Self {
            camera_id: camera_id.to_string(),
            config_revision,
            phase: PackagePresencePhase::Idle,
            last_sequence: None,
            last_worker_started_epoch_ms: 0,
            candidate_count: 0,
            candidate_first_frame_epoch_ms: 0,
            clear_candidate: PackageClearCandidate::default(),
            confirmed_frame_epoch_ms: 0,
            confidence: 0.0,
            event_id: None,
            instance_id: None,
            artifact: None,
            event_persisted: false,
            delivered: false,
            last_delivery_attempt_epoch_ms: 0,
            last_error: None,
        }
    }

    pub fn removal_confirmation_active(&self) -> bool {
        self.phase == PackagePresencePhase::Present && self.clear_candidate.count > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageDetectionBox {
    pub label: String,
    pub confidence: f64,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageDetectionObservation {
    pub sequence: u64,
    pub worker_started_epoch_ms: u64,
    pub frame_epoch_ms: u64,
    pub processed_epoch_ms: u64,
    pub observed_epoch_ms: u64,
    pub result_age_ms: u64,
    pub frame_width: u32,
    pub frame_height: u32,
    pub detections: Vec<PackageDetectionBox>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageObservationOutcome {
    Ignored,
    Idle,
    Candidate,
    Confirmed,
    Present,
    Removed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PackageEventLedger {
    #[serde(default)]
    pub configs: BTreeMap<String, PackageEventConfig>,
    #[serde(default)]
    pub states: BTreeMap<String, PackageEventState>,
    #[serde(default)]
    pub events: BTreeMap<String, PackageLifecycleEvent>,
}

#[derive(Debug, Clone)]
pub struct PackageEventStore {
    path: PathBuf,
    secure_path: Arc<SecureStorePath>,
    operation_lock: Arc<Mutex<()>>,
    bound_data_identity: Arc<Mutex<Option<SecureFileIdentity>>>,
}

impl PackageEventStore {
    pub fn try_new(path: PathBuf) -> Result<Self, String> {
        let lock_path = lock_path_for(&path)?;
        let secure_path = SecureStorePath::try_new(path, lock_path)?;
        let bound_data_identity = secure_path.open_data_read()?.map(|opened| opened.identity);
        let path = secure_path.data_path().to_path_buf();
        Ok(Self {
            path,
            secure_path: Arc::new(secure_path),
            operation_lock: Arc::new(Mutex::new(())),
            bound_data_identity: Arc::new(Mutex::new(bound_data_identity)),
        })
    }

    pub fn load(&self) -> Result<PackageEventLedger, String> {
        self.with_lock(|| self.load_unlocked())
    }

    pub fn upsert_config(&self, mut config: PackageEventConfig) -> Result<(), String> {
        validate_config(&config)?;
        self.mutate(|ledger| {
            let reset_state = match ledger.configs.get(&config.camera_id) {
                Some(current) if same_package_event_settings(current, &config) => {
                    config.revision = current.revision;
                    false
                }
                _ => true,
            };
            if reset_state {
                ledger.states.insert(
                    config.camera_id.clone(),
                    PackageEventState::idle(&config.camera_id, config.revision),
                );
            }
            ledger.configs.insert(config.camera_id.clone(), config);
            Ok(())
        })
    }

    pub fn observe(
        &self,
        camera_id: &str,
        observation: &PackageDetectionObservation,
    ) -> Result<PackageObservationOutcome, String> {
        self.with_lock(|| {
            let mut ledger = self.load_unlocked()?;
            let Some(config) = ledger.configs.get(camera_id).cloned() else {
                return Ok(PackageObservationOutcome::Ignored);
            };
            if !config.enabled {
                return Ok(PackageObservationOutcome::Ignored);
            }
            let previous_state = ledger.states.get(camera_id).cloned();
            let state = ledger
                .states
                .entry(camera_id.to_string())
                .or_insert_with(|| PackageEventState::idle(camera_id, config.revision));
            let outcome = apply_observation(&config, state, observation);
            match &outcome {
                PackageObservationOutcome::Confirmed => {
                    let event = appearance_event_from_state(&config, state)?;
                    ledger.events.insert(event.event_id.clone(), event);
                    prune_delivered_events(&mut ledger.events);
                }
                PackageObservationOutcome::Removed => {
                    let previous_state = previous_state.as_ref().ok_or_else(|| {
                        "removed package requires the previous present state".to_string()
                    })?;
                    let event = removal_event_from_state(&config, previous_state, observation)?;
                    ledger.events.insert(event.event_id.clone(), event);
                    prune_delivered_events(&mut ledger.events);
                }
                _ => {}
            }
            if ledger.states.get(camera_id) != previous_state.as_ref() {
                validate_ledger(&ledger)?;
                self.write_unlocked(&ledger)?;
            }
            Ok(outcome)
        })
    }

    pub fn pending_events(&self) -> Result<Vec<PackageLifecycleEvent>, String> {
        self.with_lock(|| {
            let mut ledger = self.load_unlocked()?;
            let mut migrated = false;
            for (camera_id, state) in &ledger.states {
                if state.phase != PackagePresencePhase::Present {
                    continue;
                }
                let Some(config) = ledger.configs.get(camera_id) else {
                    continue;
                };
                let Some(event_id) = state.event_id.as_deref() else {
                    continue;
                };
                if ledger.events.contains_key(event_id) {
                    continue;
                }
                let event = appearance_event_from_state(config, state)?;
                ledger.events.insert(event.event_id.clone(), event);
                migrated = true;
            }
            if migrated {
                prune_delivered_events(&mut ledger.events);
                validate_ledger(&ledger)?;
                self.write_unlocked(&ledger)?;
            }
            Ok(ledger
                .events
                .into_values()
                .filter(|event| !event.delivered)
                .collect())
        })
    }

    pub fn update_event(
        &self,
        event_id: &str,
        update: impl FnOnce(&mut PackageLifecycleEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        self.mutate(|ledger| {
            let (kind, camera_id) = {
                let event = ledger
                    .events
                    .get_mut(event_id)
                    .ok_or_else(|| "package lifecycle event was not found".to_string())?;
                update(event)?;
                (event.kind, event.camera_id.clone())
            };
            if kind == PackageLifecycleEventKind::Appeared {
                if let Some(state) = ledger.states.get_mut(&camera_id) {
                    if state.event_id.as_deref() == Some(event_id) {
                        let event = ledger
                            .events
                            .get(event_id)
                            .expect("updated package lifecycle event");
                        state.artifact = event.artifact.clone();
                        state.event_persisted = event.event_persisted;
                        state.delivered = event.delivered;
                        state.last_delivery_attempt_epoch_ms = event.last_delivery_attempt_epoch_ms;
                        state.last_error = event.last_error.clone();
                    }
                }
            }
            Ok(())
        })
    }

    fn mutate<T>(
        &self,
        action: impl FnOnce(&mut PackageEventLedger) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_lock(|| {
            let mut ledger = self.load_unlocked()?;
            let value = action(&mut ledger)?;
            validate_ledger(&ledger)?;
            self.write_unlocked(&ledger)?;
            Ok(value)
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _operation_guard = self
            .operation_lock
            .lock()
            .map_err(|_| "package event operation lock is poisoned".to_string())?;
        let lock_file = self.secure_path.open_lock()?;
        lock_file.lock_exclusive().map_err(|error| {
            format!(
                "failed to acquire package event lock for {}: {error}",
                self.path.display()
            )
        })?;
        let result = (|| {
            self.refresh_bound_data_identity()?;
            let value = action()?;
            self.verify_bound_data_identity()?;
            Ok(value)
        })();
        let unlock_result = FileExt::unlock(&lock_file).map_err(|error| {
            format!(
                "failed to release package event lock for {}: {error}",
                self.path.display()
            )
        });
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn refresh_bound_data_identity(&self) -> Result<(), String> {
        self.secure_path.ensure_parent_identity()?;
        let observed = self.current_data_identity()?;
        let revalidated = self.current_data_identity()?;
        if observed != revalidated {
            return Err("package event data identity changed during refresh".to_string());
        }
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "package event data identity lock is poisoned".to_string())? = observed;
        Ok(())
    }

    fn current_data_identity(&self) -> Result<Option<SecureFileIdentity>, String> {
        self.secure_path
            .open_data_read()
            .map(|opened| opened.map(|opened| opened.identity))
    }

    fn verify_bound_data_identity(&self) -> Result<(), String> {
        let expected = *self
            .bound_data_identity
            .lock()
            .map_err(|_| "package event data identity lock is poisoned".to_string())?;
        if expected != self.current_data_identity()? {
            return Err("package event data identity changed unexpectedly".to_string());
        }
        Ok(())
    }

    fn load_unlocked(&self) -> Result<PackageEventLedger, String> {
        let Some(opened) = self.secure_path.open_data_read()? else {
            return Ok(PackageEventLedger::default());
        };
        if opened.len > MAX_STORE_BYTES {
            return Err("package event store exceeds size limit".to_string());
        }
        let mut bytes = Vec::with_capacity(opened.len as usize);
        opened
            .file
            .take(MAX_STORE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("failed to read package event store: {error}"))?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("package event store exceeds size limit".to_string());
        }
        if bytes.is_empty() {
            return Err("package event store is empty".to_string());
        }
        let ledger: PackageEventLedger = serde_json::from_slice(&bytes)
            .map_err(|error| format!("package event store is invalid: {error}"))?;
        validate_ledger(&ledger)?;
        Ok(ledger)
    }

    fn write_unlocked(&self, ledger: &PackageEventLedger) -> Result<(), String> {
        let bytes = serde_json::to_vec(ledger)
            .map_err(|error| format!("failed to serialize package event state: {error}"))?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("package event store exceeds size limit".to_string());
        }
        let identity = self
            .secure_path
            .replace_data_atomically(&bytes, || Ok(()))?;
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "package event data identity lock is poisoned".to_string())? =
            Some(identity);
        Ok(())
    }
}

pub fn default_package_event_store_path() -> PathBuf {
    env::var_os(PACKAGE_EVENT_STORE_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STORE_PATH))
}

fn appearance_event_from_state(
    config: &PackageEventConfig,
    state: &PackageEventState,
) -> Result<PackageLifecycleEvent, String> {
    let event_id = state
        .event_id
        .clone()
        .ok_or_else(|| "confirmed package state requires event_id".to_string())?;
    let instance_id = state
        .instance_id
        .clone()
        .ok_or_else(|| "confirmed package state requires instance_id".to_string())?;
    Ok(PackageLifecycleEvent {
        event_id,
        kind: PackageLifecycleEventKind::Appeared,
        camera_id: state.camera_id.clone(),
        instance_id,
        related_event_id: None,
        config_revision: state.config_revision,
        observed_frame_epoch_ms: state.confirmed_frame_epoch_ms,
        confidence: state.confidence,
        zone: config.zone,
        confirm_frames: config.confirm_frames,
        confirm_window_ms: config.confirm_window_ms,
        artifact: state.artifact.clone(),
        event_persisted: state.event_persisted,
        delivered: state.delivered,
        last_delivery_attempt_epoch_ms: state.last_delivery_attempt_epoch_ms,
        last_error: state.last_error.clone(),
    })
}

fn removal_event_from_state(
    config: &PackageEventConfig,
    previous_state: &PackageEventState,
    observation: &PackageDetectionObservation,
) -> Result<PackageLifecycleEvent, String> {
    let appeared_event_id = previous_state
        .event_id
        .clone()
        .ok_or_else(|| "removed package requires appeared event_id".to_string())?;
    let instance_id = previous_state
        .instance_id
        .clone()
        .ok_or_else(|| "removed package requires instance_id".to_string())?;
    Ok(PackageLifecycleEvent {
        event_id: format!("package_removed_{}", Uuid::new_v4().simple()),
        kind: PackageLifecycleEventKind::Removed,
        camera_id: previous_state.camera_id.clone(),
        instance_id,
        related_event_id: Some(appeared_event_id),
        config_revision: previous_state.config_revision,
        observed_frame_epoch_ms: observation.frame_epoch_ms,
        confidence: previous_state.confidence,
        zone: config.zone,
        confirm_frames: PACKAGE_CLEAR_CONFIRM_FRAMES,
        confirm_window_ms: PACKAGE_CLEAR_CONFIRM_DURATION_MS,
        artifact: None,
        event_persisted: false,
        delivered: false,
        last_delivery_attempt_epoch_ms: 0,
        last_error: None,
    })
}

fn prune_delivered_events(events: &mut BTreeMap<String, PackageLifecycleEvent>) {
    let mut latest_delivered = BTreeMap::new();
    for event in events.values().filter(|event| event.delivered) {
        let key = (event.camera_id.clone(), event.kind);
        let replace = latest_delivered.get(&key).map_or(true, |current: &String| {
            let current = &events[current];
            (event.observed_frame_epoch_ms, &event.event_id)
                > (current.observed_frame_epoch_ms, &current.event_id)
        });
        if replace {
            latest_delivered.insert(key, event.event_id.clone());
        }
    }
    events.retain(|event_id, event| {
        !event.delivered
            || latest_delivered.get(&(event.camera_id.clone(), event.kind)) == Some(event_id)
    });
}

fn apply_observation(
    config: &PackageEventConfig,
    state: &mut PackageEventState,
    observation: &PackageDetectionObservation,
) -> PackageObservationOutcome {
    if state.config_revision != config.revision {
        *state = PackageEventState::idle(&config.camera_id, config.revision);
    }
    if observation.worker_started_epoch_ms == 0
        || observation.processed_epoch_ms == 0
        || observation.worker_started_epoch_ms > observation.frame_epoch_ms
        || observation.frame_epoch_ms > observation.processed_epoch_ms
        || observation.observed_epoch_ms < observation.processed_epoch_ms
        || observation
            .observed_epoch_ms
            .saturating_sub(observation.processed_epoch_ms)
            > config.max_result_age_ms
        || observation.frame_width == 0
        || observation.frame_height == 0
        || observation.result_age_ms > config.max_result_age_ms
    {
        return PackageObservationOutcome::Ignored;
    }
    if observation.worker_started_epoch_ms < state.last_worker_started_epoch_ms {
        return PackageObservationOutcome::Ignored;
    }
    if observation.worker_started_epoch_ms > state.last_worker_started_epoch_ms {
        if state.phase == PackagePresencePhase::Present {
            state.last_sequence = None;
            state.last_worker_started_epoch_ms = observation.worker_started_epoch_ms;
            state.clear_candidate = PackageClearCandidate::default();
        } else {
            *state = PackageEventState::idle(&config.camera_id, config.revision);
            state.last_worker_started_epoch_ms = observation.worker_started_epoch_ms;
        }
    }
    if state
        .last_sequence
        .is_some_and(|sequence| observation.sequence <= sequence)
    {
        return PackageObservationOutcome::Ignored;
    }
    state.last_sequence = Some(observation.sequence);
    let confidence = observation
        .detections
        .iter()
        .filter(|detection| detection.label.eq_ignore_ascii_case("package"))
        .filter(|detection| detection_center_in_zone(detection, observation, config.zone))
        .map(|detection| detection.confidence)
        .filter(|confidence| confidence.is_finite())
        .fold(None, |highest: Option<f64>, confidence| {
            Some(highest.map_or(confidence, |current| current.max(confidence)))
        });

    if state.phase == PackagePresencePhase::Present {
        if confidence.is_some() {
            state.clear_candidate = PackageClearCandidate::default();
            return PackageObservationOutcome::Present;
        }
        if state.clear_candidate.count == 0 {
            state.clear_candidate.first_frame_epoch_ms = observation.frame_epoch_ms;
        }
        state.clear_candidate.count = state.clear_candidate.count.saturating_add(1);
        let clear_duration_ms = observation
            .frame_epoch_ms
            .saturating_sub(state.clear_candidate.first_frame_epoch_ms);
        if state.clear_candidate.count < PACKAGE_CLEAR_CONFIRM_FRAMES
            || clear_duration_ms < PACKAGE_CLEAR_CONFIRM_DURATION_MS
        {
            return PackageObservationOutcome::Present;
        }
        reset_to_consumed_idle(config, state);
        return PackageObservationOutcome::Removed;
    }

    let Some(confidence) = confidence else {
        state.phase = PackagePresencePhase::Idle;
        state.candidate_count = 0;
        state.candidate_first_frame_epoch_ms = 0;
        state.clear_candidate = PackageClearCandidate::default();
        return PackageObservationOutcome::Idle;
    };

    state.clear_candidate = PackageClearCandidate::default();
    let outside_window = state.phase != PackagePresencePhase::Candidate
        || observation.frame_epoch_ms < state.candidate_first_frame_epoch_ms
        || observation.frame_epoch_ms - state.candidate_first_frame_epoch_ms
            > config.confirm_window_ms;
    if outside_window {
        state.phase = PackagePresencePhase::Candidate;
        state.candidate_count = 1;
        state.candidate_first_frame_epoch_ms = observation.frame_epoch_ms;
        state.confidence = confidence;
    } else {
        state.candidate_count = state.candidate_count.saturating_add(1);
        state.confidence = state.confidence.max(confidence);
    }
    if state.candidate_count < config.confirm_frames {
        return PackageObservationOutcome::Candidate;
    }

    state.phase = PackagePresencePhase::Present;
    state.confirmed_frame_epoch_ms = observation.frame_epoch_ms;
    state.event_id = Some(format!("package_{}", Uuid::new_v4().simple()));
    state.instance_id = Some(format!("package_instance_{}", Uuid::new_v4().simple()));
    PackageObservationOutcome::Confirmed
}

fn reset_to_consumed_idle(config: &PackageEventConfig, state: &mut PackageEventState) {
    let last_sequence = state.last_sequence;
    let last_worker_started_epoch_ms = state.last_worker_started_epoch_ms;
    *state = PackageEventState::idle(&config.camera_id, config.revision);
    state.last_sequence = last_sequence;
    state.last_worker_started_epoch_ms = last_worker_started_epoch_ms;
}

fn detection_center_in_zone(
    detection: &PackageDetectionBox,
    observation: &PackageDetectionObservation,
    zone: PackageDeliveryZone,
) -> bool {
    let center_x = (detection.x1 + detection.x2) / 2.0 / f64::from(observation.frame_width);
    let center_y = (detection.y1 + detection.y2) / 2.0 / f64::from(observation.frame_height);
    center_x.is_finite()
        && center_y.is_finite()
        && center_x >= zone.left
        && center_x <= zone.right
        && center_y >= zone.top
        && center_y <= zone.bottom
}

fn validate_ledger(ledger: &PackageEventLedger) -> Result<(), String> {
    if ledger.configs.len() > MAX_CAMERAS || ledger.states.len() > MAX_CAMERAS {
        return Err("package event store exceeds camera limit".to_string());
    }
    if ledger.events.len() > MAX_LIFECYCLE_EVENTS {
        return Err("package event store exceeds lifecycle event limit".to_string());
    }
    for (camera_id, config) in &ledger.configs {
        validate_config(config)?;
        if camera_id != &config.camera_id {
            return Err("package event config key does not match camera_id".to_string());
        }
    }
    for (camera_id, state) in &ledger.states {
        validate_camera_id(camera_id)?;
        if camera_id != &state.camera_id {
            return Err("package event state key does not match camera_id".to_string());
        }
    }
    for (event_id, event) in &ledger.events {
        validate_lifecycle_event(event)?;
        if event_id != &event.event_id {
            return Err("package lifecycle event key does not match event_id".to_string());
        }
    }
    Ok(())
}

fn validate_lifecycle_event(event: &PackageLifecycleEvent) -> Result<(), String> {
    validate_camera_id(&event.camera_id)?;
    if event.event_id.trim() != event.event_id
        || event.event_id.is_empty()
        || event.event_id.len() > 128
        || event.event_id.chars().any(char::is_control)
    {
        return Err("package lifecycle event_id is invalid".to_string());
    }
    if event.instance_id.trim() != event.instance_id
        || event.instance_id.is_empty()
        || event.instance_id.len() > 128
        || event.instance_id.chars().any(char::is_control)
    {
        return Err("package lifecycle instance_id is invalid".to_string());
    }
    match event.kind {
        PackageLifecycleEventKind::Appeared if event.related_event_id.is_some() => {
            return Err("package appeared event cannot reference another event".to_string());
        }
        PackageLifecycleEventKind::Removed
            if event
                .related_event_id
                .as_deref()
                .map_or(true, |event_id| event_id.trim().is_empty()) =>
        {
            return Err("package removed event requires appeared event_id".to_string());
        }
        _ => {}
    }
    if event.observed_frame_epoch_ms == 0 || !event.confidence.is_finite() {
        return Err("package lifecycle observation is invalid".to_string());
    }
    Ok(())
}

fn same_package_event_settings(left: &PackageEventConfig, right: &PackageEventConfig) -> bool {
    left.camera_id == right.camera_id
        && left.enabled == right.enabled
        && left.zone == right.zone
        && left.confirm_frames == right.confirm_frames
        && left.confirm_window_ms == right.confirm_window_ms
        && left.max_result_age_ms == right.max_result_age_ms
}

fn validate_config(config: &PackageEventConfig) -> Result<(), String> {
    validate_camera_id(&config.camera_id)?;
    for coordinate in [
        config.zone.left,
        config.zone.top,
        config.zone.right,
        config.zone.bottom,
    ] {
        if !coordinate.is_finite() || !(0.0..=1.0).contains(&coordinate) {
            return Err("package delivery zone coordinates must be between 0 and 1".to_string());
        }
    }
    if config.zone.left >= config.zone.right || config.zone.top >= config.zone.bottom {
        return Err("package delivery zone must have positive width and height".to_string());
    }
    if !(2..=10).contains(&config.confirm_frames) {
        return Err("package confirm_frames must be between 2 and 10".to_string());
    }
    if !(500..=10_000).contains(&config.confirm_window_ms) {
        return Err("package confirm_window_ms must be between 500 and 10000".to_string());
    }
    if !(500..=10_000).contains(&config.max_result_age_ms) {
        return Err("package max_result_age_ms must be between 500 and 10000".to_string());
    }
    Ok(())
}

fn validate_camera_id(camera_id: &str) -> Result<(), String> {
    if camera_id.trim() != camera_id
        || camera_id.is_empty()
        || camera_id.len() > 128
        || camera_id.chars().any(char::is_control)
    {
        return Err("package event camera_id is invalid".to_string());
    }
    Ok(())
}

fn lock_path_for(path: &Path) -> Result<PathBuf, String> {
    let data_name = path
        .file_name()
        .ok_or_else(|| "package event store path must include a file name".to_string())?;
    let mut lock_name = OsString::from(data_name);
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn config() -> PackageEventConfig {
        PackageEventConfig::new(
            "camera.252",
            true,
            PackageDeliveryZone {
                left: 0.25,
                top: 0.25,
                right: 0.75,
                bottom: 0.75,
            },
            1,
        )
        .expect("valid config")
    }

    fn temporary_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harborbeacon-package-event-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ))
    }

    fn observation(
        sequence: u64,
        frame_epoch_ms: u64,
        center_x: f64,
    ) -> PackageDetectionObservation {
        PackageDetectionObservation {
            sequence,
            worker_started_epoch_ms: 100.min(frame_epoch_ms),
            frame_epoch_ms,
            processed_epoch_ms: frame_epoch_ms + 20,
            observed_epoch_ms: frame_epoch_ms + 40,
            result_age_ms: 20,
            frame_width: 1_000,
            frame_height: 500,
            detections: vec![PackageDetectionBox {
                label: "package".to_string(),
                confidence: 0.88,
                x1: center_x - 25.0,
                y1: 225.0,
                x2: center_x + 25.0,
                y2: 275.0,
            }],
        }
    }

    #[test]
    fn confirms_only_after_three_fresh_in_zone_observations() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);

        assert_eq!(
            apply_observation(&config, &mut state, &observation(1, 1_000, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(2, 1_500, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(3, 2_000, 500.0)),
            PackageObservationOutcome::Confirmed
        );
        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert!(state
            .event_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn ignores_out_of_zone_stale_and_repeated_observations() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        assert_eq!(
            apply_observation(&config, &mut state, &observation(1, 1_000, 900.0)),
            PackageObservationOutcome::Idle
        );
        let mut stale = observation(2, 1_500, 500.0);
        stale.result_age_ms = config.max_result_age_ms + 1;
        assert_eq!(
            apply_observation(&config, &mut state, &stale),
            PackageObservationOutcome::Ignored
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(3, 2_000, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(3, 2_100, 500.0)),
            PackageObservationOutcome::Ignored
        );
    }

    fn empty_observation(sequence: u64, frame_epoch_ms: u64) -> PackageDetectionObservation {
        let mut observation = observation(sequence, frame_epoch_ms, 500.0);
        observation.detections.clear();
        observation
    }

    fn confirm_package(config: &PackageEventConfig, state: &mut PackageEventState) -> String {
        assert_eq!(
            apply_observation(config, state, &observation(1, 1_000, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(config, state, &observation(2, 1_500, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(config, state, &observation(3, 2_000, 500.0)),
            PackageObservationOutcome::Confirmed
        );
        state.event_id.clone().expect("confirmed event id")
    }

    #[test]
    fn present_state_does_not_emit_a_second_event_while_the_package_remains() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let event_id = confirm_package(&config, &mut state);
        let instance_id = state.instance_id.clone();
        state.delivered = true;
        state.last_error = Some("preserved delivery status".to_string());

        for sequence in 4..=20 {
            assert_eq!(
                apply_observation(
                    &config,
                    &mut state,
                    &observation(sequence, sequence * 500, 500.0),
                ),
                PackageObservationOutcome::Present
            );
        }

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert_eq!(state.event_id.as_deref(), Some(event_id.as_str()));
        assert_eq!(state.instance_id, instance_id);
        assert!(state.delivered);
        assert_eq!(
            state.last_error.as_deref(),
            Some("preserved delivery status")
        );
        assert_eq!(state.clear_candidate, PackageClearCandidate::default());
    }

    #[test]
    fn a_positive_observation_clears_a_short_absence_without_rearming() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let event_id = confirm_package(&config, &mut state);

        assert_eq!(
            apply_observation(&config, &mut state, &empty_observation(4, 2_500)),
            PackageObservationOutcome::Present
        );
        assert_eq!(state.clear_candidate.count, 1);
        assert!(state.removal_confirmation_active());
        assert_eq!(
            apply_observation(&config, &mut state, &observation(5, 3_000, 500.0)),
            PackageObservationOutcome::Present
        );

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert!(!state.removal_confirmation_active());
        assert_eq!(state.event_id.as_deref(), Some(event_id.as_str()));
        assert_eq!(state.clear_candidate, PackageClearCandidate::default());
    }

    #[test]
    fn ten_negative_observations_under_five_seconds_remain_in_removal_confirmation() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let event_id = confirm_package(&config, &mut state);

        for index in 0..PACKAGE_CLEAR_CONFIRM_FRAMES {
            assert_eq!(
                apply_observation(
                    &config,
                    &mut state,
                    &empty_observation(4 + u64::from(index), 3_000 + u64::from(index) * 400),
                ),
                PackageObservationOutcome::Present
            );
        }

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert!(state.removal_confirmation_active());
        assert_eq!(state.event_id.as_deref(), Some(event_id.as_str()));
        assert_eq!(state.clear_candidate.count, PACKAGE_CLEAR_CONFIRM_FRAMES);
        assert_eq!(
            serde_json::to_value(&state).expect("serialize removal confirmation state")["phase"],
            "present"
        );
    }

    #[test]
    fn ten_negative_observations_spanning_five_seconds_confirm_removal_and_rearm() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        confirm_package(&config, &mut state);
        state.artifact = Some(PackageEventArtifact {
            artifact_id: "artifact-1".to_string(),
            mime_type: "image/jpeg".to_string(),
            byte_size: 123,
        });
        state.event_persisted = true;
        state.delivered = true;
        state.last_delivery_attempt_epoch_ms = 2_500;
        state.last_error = Some("old delivery error".to_string());

        let mut outcome = PackageObservationOutcome::Present;
        for index in 0..PACKAGE_CLEAR_CONFIRM_FRAMES {
            outcome = apply_observation(
                &config,
                &mut state,
                &empty_observation(4 + u64::from(index), 3_000 + u64::from(index) * 600),
            );
        }

        assert_eq!(outcome, PackageObservationOutcome::Removed);
        assert_eq!(state.phase, PackagePresencePhase::Idle);
        assert_eq!(state.last_worker_started_epoch_ms, 100);
        assert_eq!(state.last_sequence, Some(13));
        assert_eq!(state.candidate_count, 0);
        assert_eq!(state.clear_candidate, PackageClearCandidate::default());
        assert_eq!(state.event_id, None);
        assert_eq!(state.instance_id, None);
        assert_eq!(state.artifact, None);
        assert!(!state.event_persisted);
        assert!(!state.delivered);
        assert_eq!(state.last_delivery_attempt_epoch_ms, 0);
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn repeated_stale_and_invalid_observations_do_not_advance_clearing() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        confirm_package(&config, &mut state);
        assert_eq!(
            apply_observation(&config, &mut state, &empty_observation(4, 2_500)),
            PackageObservationOutcome::Present
        );

        assert_eq!(
            apply_observation(&config, &mut state, &empty_observation(4, 3_000)),
            PackageObservationOutcome::Ignored
        );
        let mut stale = empty_observation(5, 3_500);
        stale.result_age_ms = config.max_result_age_ms + 1;
        assert_eq!(
            apply_observation(&config, &mut state, &stale),
            PackageObservationOutcome::Ignored
        );
        let mut invalid_size = empty_observation(6, 4_000);
        invalid_size.frame_width = 0;
        assert_eq!(
            apply_observation(&config, &mut state, &invalid_size),
            PackageObservationOutcome::Ignored
        );
        let mut invalid_timestamp = empty_observation(7, 4_500);
        invalid_timestamp.observed_epoch_ms = invalid_timestamp.processed_epoch_ms - 1;
        assert_eq!(
            apply_observation(&config, &mut state, &invalid_timestamp),
            PackageObservationOutcome::Ignored
        );
        let mut old_worker = empty_observation(8, 5_000);
        old_worker.worker_started_epoch_ms = 99;
        assert_eq!(
            apply_observation(&config, &mut state, &old_worker),
            PackageObservationOutcome::Ignored
        );

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert!(state.removal_confirmation_active());
        assert_eq!(state.last_sequence, Some(4));
        assert_eq!(state.clear_candidate.count, 1);
        assert_eq!(state.clear_candidate.first_frame_epoch_ms, 2_500);
    }

    #[test]
    fn worker_restart_preserves_the_present_event_and_resets_clearing() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let event_id = confirm_package(&config, &mut state);
        let instance_id = state.instance_id.clone();
        state.delivered = true;
        assert_eq!(
            apply_observation(&config, &mut state, &empty_observation(4, 2_500)),
            PackageObservationOutcome::Present
        );

        let mut restarted = empty_observation(1, 3_000);
        restarted.worker_started_epoch_ms = 2_500;
        assert_eq!(
            apply_observation(&config, &mut state, &restarted),
            PackageObservationOutcome::Present
        );

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert_eq!(state.last_worker_started_epoch_ms, 2_500);
        assert_eq!(state.last_sequence, Some(1));
        assert_eq!(state.event_id.as_deref(), Some(event_id.as_str()));
        assert_eq!(state.instance_id, instance_id);
        assert!(state.delivered);
        assert_eq!(state.clear_candidate.count, 1);
        assert_eq!(state.clear_candidate.first_frame_epoch_ms, 3_000);

        let mut present_again = observation(2, 3_500, 500.0);
        present_again.worker_started_epoch_ms = 2_500;
        assert_eq!(
            apply_observation(&config, &mut state, &present_again),
            PackageObservationOutcome::Present
        );
        assert_eq!(state.event_id.as_deref(), Some(event_id.as_str()));
        assert_eq!(state.clear_candidate, PackageClearCandidate::default());
    }

    #[test]
    fn rearmed_state_confirms_a_new_event_after_three_positive_observations() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let first_event_id = confirm_package(&config, &mut state);
        for index in 0..PACKAGE_CLEAR_CONFIRM_FRAMES {
            apply_observation(
                &config,
                &mut state,
                &empty_observation(4 + u64::from(index), 3_000 + u64::from(index) * 600),
            );
        }
        assert_eq!(state.phase, PackagePresencePhase::Idle);

        assert_eq!(
            apply_observation(&config, &mut state, &observation(14, 9_000, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(15, 9_500, 500.0)),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(
            apply_observation(&config, &mut state, &observation(16, 10_000, 500.0)),
            PackageObservationOutcome::Confirmed
        );

        assert_eq!(state.phase, PackagePresencePhase::Present);
        assert!(state
            .event_id
            .as_deref()
            .is_some_and(|event_id| event_id != first_event_id.as_str()));
    }

    #[test]
    fn removal_event_links_to_the_appeared_event_and_preserves_the_instance() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        confirm_package(&config, &mut state);
        let appeared = appearance_event_from_state(&config, &state).expect("appeared event");
        let present_state = state.clone();
        let mut outcome = PackageObservationOutcome::Present;
        let mut removal_observation = empty_observation(4, 3_000);
        for index in 0..PACKAGE_CLEAR_CONFIRM_FRAMES {
            removal_observation =
                empty_observation(4 + u64::from(index), 3_000 + u64::from(index) * 600);
            outcome = apply_observation(&config, &mut state, &removal_observation);
        }
        let removed = removal_event_from_state(&config, &present_state, &removal_observation)
            .expect("removed event");

        assert_eq!(outcome, PackageObservationOutcome::Removed);
        assert_eq!(removed.kind, PackageLifecycleEventKind::Removed);
        assert_eq!(removed.instance_id, appeared.instance_id);
        assert_eq!(
            removed.related_event_id.as_deref(),
            Some(appeared.event_id.as_str())
        );
        assert_eq!(
            removed.observed_frame_epoch_ms,
            removal_observation.frame_epoch_ms
        );
        assert!(!removed.delivered);
        assert_eq!(state.phase, PackagePresencePhase::Idle);
    }

    #[test]
    fn store_keeps_removal_retryable_while_rearming_for_the_next_package() {
        let root = temporary_test_root("removal-retry");
        fs::create_dir_all(&root).expect("create package event test root");
        let path = root.join("package-events.json");
        let store = PackageEventStore::try_new(path.clone()).expect("package event store");
        let config = config();
        store
            .upsert_config(config.clone())
            .expect("package event config");

        for sequence in 1..=3 {
            store
                .observe(
                    &config.camera_id,
                    &observation(sequence, sequence * 500 + 500, 500.0),
                )
                .expect("package appearance observation");
        }
        let appeared = store
            .pending_events()
            .expect("pending appearance")
            .into_iter()
            .next()
            .expect("appeared event");
        store
            .update_event(&appeared.event_id, |event| {
                event.delivered = true;
                Ok(())
            })
            .expect("mark appeared event delivered");

        for index in 0..PACKAGE_CLEAR_CONFIRM_FRAMES {
            store
                .observe(
                    &config.camera_id,
                    &empty_observation(4 + u64::from(index), 3_000 + u64::from(index) * 600),
                )
                .expect("package removal observation");
        }

        let removal = store
            .pending_events()
            .expect("pending removal")
            .into_iter()
            .find(|event| event.kind == PackageLifecycleEventKind::Removed)
            .expect("removed event");
        assert_eq!(removal.instance_id, appeared.instance_id);
        assert_eq!(
            removal.related_event_id.as_deref(),
            Some(appeared.event_id.as_str())
        );
        assert!(!removal.delivered);
        assert_eq!(
            store.load().expect("package event ledger").states[&config.camera_id].phase,
            PackagePresencePhase::Idle
        );

        let restored = PackageEventStore::try_new(path).expect("restored package event store");
        assert!(restored
            .pending_events()
            .expect("restored pending events")
            .iter()
            .any(|event| event.event_id == removal.event_id));

        for (sequence, frame_epoch_ms) in [(14, 9_000), (15, 9_500), (16, 10_000)] {
            restored
                .observe(
                    &config.camera_id,
                    &observation(sequence, frame_epoch_ms, 500.0),
                )
                .expect("next package observation");
        }
        let pending = restored.pending_events().expect("rearmed pending events");
        assert!(pending
            .iter()
            .any(|event| event.event_id == removal.event_id));
        assert!(pending.iter().any(|event| {
            event.kind == PackageLifecycleEventKind::Appeared
                && event.instance_id != removal.instance_id
        }));
    }

    #[test]
    fn state_without_clear_candidate_deserializes_with_the_default() {
        let config = config();
        let state = PackageEventState::idle(&config.camera_id, config.revision);
        let mut value = serde_json::to_value(state).expect("serialize package event state");
        value
            .as_object_mut()
            .expect("package event state object")
            .remove("clear_candidate");

        let restored: PackageEventState =
            serde_json::from_value(value).expect("deserialize legacy package event state");

        assert_eq!(restored.clear_candidate, PackageClearCandidate::default());
    }

    #[test]
    fn accepts_a_reset_sequence_from_a_new_worker_but_rejects_old_disk_output() {
        let config = config();
        let mut state = PackageEventState::idle(&config.camera_id, config.revision);
        let mut first = observation(50, 1_000, 500.0);
        first.worker_started_epoch_ms = 900;
        assert_eq!(
            apply_observation(&config, &mut state, &first),
            PackageObservationOutcome::Candidate
        );

        let mut restarted = observation(1, 2_000, 500.0);
        restarted.worker_started_epoch_ms = 1_900;
        assert_eq!(
            apply_observation(&config, &mut state, &restarted),
            PackageObservationOutcome::Candidate
        );
        assert_eq!(state.last_sequence, Some(1));

        let mut stale = observation(2, 2_500, 500.0);
        stale.worker_started_epoch_ms = 1_900;
        stale.observed_epoch_ms = stale.processed_epoch_ms + config.max_result_age_ms + 1;
        assert_eq!(
            apply_observation(&config, &mut state, &stale),
            PackageObservationOutcome::Ignored
        );
    }

    #[test]
    fn unchanged_settings_keep_the_existing_revision() {
        let current = config();
        let mut requested = current.clone();
        requested.revision += 1;

        assert!(same_package_event_settings(&current, &requested));
    }
}
