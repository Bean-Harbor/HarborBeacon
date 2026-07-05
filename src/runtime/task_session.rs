//! Persistent conversation state for Task API multi-step flows.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::control_plane::approvals::{ApprovalStatus, ApprovalTicket};
use crate::control_plane::events::EventRecord;
use crate::control_plane::media::{MediaAsset, MediaSession, MediaSessionStatus, ShareLink};
use crate::control_plane::tasks::{ArtifactRecord, ConversationSession, TaskRun, TaskStepRun};
use crate::runtime::admin_console::default_rtsp_port;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CAMERA_CONNECT_OPERATION: &str = "camera.connect";
const CAMERA_RECORD_CLIP_CONFIRMATION_OPERATION: &str = "camera.record_clip_confirmation";
const GENERAL_MESSAGE_LOOP_OPERATION: &str = "general.message_loop";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskCandidate {
    pub candidate_id: String,
    pub name: String,
    pub ip: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default = "default_rtsp_port")]
    pub port: u16,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
    #[serde(default)]
    pub requires_auth: bool,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskConnect {
    #[serde(default)]
    pub resume_token: String,
    pub name: String,
    pub ip: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default = "default_rtsp_port")]
    pub port: u16,
    #[serde(default)]
    pub snapshot_url: Option<String>,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
    #[serde(default)]
    pub requires_auth: bool,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskClipConfirmation {
    #[serde(default)]
    pub resume_token: String,
    #[serde(default)]
    pub clip_media_asset_id: String,
    #[serde(default)]
    pub clip_path: String,
    #[serde(default)]
    pub clip_mime_type: String,
    #[serde(default)]
    pub cover_path: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RecentClipPlaybackState {
    #[serde(default)]
    pub clip_media_asset_id: String,
    #[serde(default)]
    pub clip_path: String,
    #[serde(default)]
    pub clip_mime_type: String,
    #[serde(default)]
    pub cover_path: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub captured_at_epoch_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RecentFamilyMemoryEventState {
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub camera_id: String,
    #[serde(default)]
    pub event_type: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub referenced_at_epoch_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskGeneralMessageLoop {
    #[serde(default)]
    pub resume_token: String,
    #[serde(default)]
    pub original_goal: String,
    #[serde(default)]
    pub latest_user_intent_text: String,
    #[serde(default)]
    pub last_clarification_prompt: String,
    #[serde(default)]
    pub selected_candidate_action: Option<String>,
    #[serde(default)]
    pub camera_hint: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_assistant: Option<PendingTaskHomeAssistantClarification>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskHomeAssistantClarification {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_hint: Option<String>,
    #[serde(default)]
    pub candidates: Vec<PendingTaskHomeAssistantCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskHomeAssistantCandidate {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskSelectionItem {
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskSelectionState {
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub items: Vec<PendingTaskSelectionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PendingTaskResumeState {
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub resume_token: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskConversationState {
    #[serde(default)]
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_selection: Option<PendingTaskSelectionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_resume: Option<PendingTaskResumeState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_candidates: Vec<PendingTaskCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_connect: Option<PendingTaskConnect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_clip_playback: Option<RecentClipPlaybackState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_family_memory_event: Option<RecentFamilyMemoryEventState>,
    #[serde(default)]
    pub last_scan_cidr: String,
}

impl TaskConversationState {
    pub fn camera_pending_candidates(&self) -> Vec<PendingTaskCandidate> {
        camera_candidates_from_selection(self.pending_selection.as_ref())
            .unwrap_or_else(|| self.pending_candidates.clone())
    }

    pub fn set_camera_pending_candidates(&mut self, candidates: Vec<PendingTaskCandidate>) {
        self.pending_selection =
            (!candidates.is_empty()).then(|| pending_selection_from_camera_candidates(&candidates));
        self.pending_candidates.clear();
    }

    pub fn retain_camera_pending_candidates<F>(&mut self, mut retain: F)
    where
        F: FnMut(&PendingTaskCandidate) -> bool,
    {
        let candidates = self
            .camera_pending_candidates()
            .into_iter()
            .filter(|candidate| retain(candidate))
            .collect();
        self.set_camera_pending_candidates(candidates);
    }

    pub fn camera_pending_connect(&self) -> Option<PendingTaskConnect> {
        pending_connect_from_resume(self.pending_resume.as_ref())
            .or_else(|| self.pending_connect.clone())
    }

    pub fn set_camera_pending_connect(&mut self, pending: Option<PendingTaskConnect>) {
        self.pending_resume = pending.as_ref().map(pending_resume_from_camera_connect);
        self.pending_connect = None;
    }

    pub fn clip_pending_confirmation(&self) -> Option<PendingTaskClipConfirmation> {
        pending_clip_confirmation_from_resume(self.pending_resume.as_ref())
    }

    pub fn set_clip_pending_confirmation(&mut self, pending: Option<PendingTaskClipConfirmation>) {
        if pending.is_some()
            || matches!(
                self.pending_resume.as_ref(),
                Some(current) if current.operation == CAMERA_RECORD_CLIP_CONFIRMATION_OPERATION
            )
        {
            self.pending_resume = pending.as_ref().map(pending_resume_from_clip_confirmation);
        }
    }

    pub fn general_message_loop(&self) -> Option<PendingTaskGeneralMessageLoop> {
        pending_general_message_loop_from_resume(self.pending_resume.as_ref())
    }

    pub fn set_general_message_loop(&mut self, pending: Option<PendingTaskGeneralMessageLoop>) {
        if pending.is_some()
            || matches!(
                self.pending_resume.as_ref(),
                Some(current) if current.operation == GENERAL_MESSAGE_LOOP_OPERATION
            )
        {
            self.pending_resume = pending
                .as_ref()
                .map(pending_resume_from_general_message_loop);
        }
    }

    pub fn recent_clip_playback(&self) -> Option<RecentClipPlaybackState> {
        self.recent_clip_playback.clone()
    }

    pub fn set_recent_clip_playback(&mut self, recent_clip: Option<RecentClipPlaybackState>) {
        self.recent_clip_playback = recent_clip;
    }

    pub fn recent_family_memory_event(&self) -> Option<RecentFamilyMemoryEventState> {
        self.recent_family_memory_event.clone()
    }

    pub fn set_recent_family_memory_event(
        &mut self,
        recent_event: Option<RecentFamilyMemoryEventState>,
    ) {
        self.recent_family_memory_event = recent_event;
    }
}

fn pending_selection_from_camera_candidates(
    candidates: &[PendingTaskCandidate],
) -> PendingTaskSelectionState {
    PendingTaskSelectionState {
        operation: CAMERA_CONNECT_OPERATION.to_string(),
        items: candidates
            .iter()
            .map(|candidate| PendingTaskSelectionItem {
                item_id: candidate.candidate_id.clone(),
                payload: serde_json::to_value(candidate).unwrap_or(Value::Null),
            })
            .collect(),
    }
}

fn camera_candidates_from_selection(
    selection: Option<&PendingTaskSelectionState>,
) -> Option<Vec<PendingTaskCandidate>> {
    let selection = selection?;
    if selection.operation != CAMERA_CONNECT_OPERATION {
        return None;
    }
    Some(
        selection
            .items
            .iter()
            .filter_map(|item| {
                serde_json::from_value::<PendingTaskCandidate>(item.payload.clone()).ok()
            })
            .collect(),
    )
}

fn pending_resume_from_camera_connect(pending: &PendingTaskConnect) -> PendingTaskResumeState {
    PendingTaskResumeState {
        operation: CAMERA_CONNECT_OPERATION.to_string(),
        resume_token: pending.resume_token.clone(),
        payload: serde_json::to_value(pending).unwrap_or(Value::Null),
    }
}

fn pending_resume_from_clip_confirmation(
    pending: &PendingTaskClipConfirmation,
) -> PendingTaskResumeState {
    PendingTaskResumeState {
        operation: CAMERA_RECORD_CLIP_CONFIRMATION_OPERATION.to_string(),
        resume_token: pending.resume_token.clone(),
        payload: serde_json::to_value(pending).unwrap_or(Value::Null),
    }
}

fn pending_resume_from_general_message_loop(
    pending: &PendingTaskGeneralMessageLoop,
) -> PendingTaskResumeState {
    PendingTaskResumeState {
        operation: GENERAL_MESSAGE_LOOP_OPERATION.to_string(),
        resume_token: pending.resume_token.clone(),
        payload: serde_json::to_value(pending).unwrap_or(Value::Null),
    }
}

fn pending_connect_from_resume(
    pending_resume: Option<&PendingTaskResumeState>,
) -> Option<PendingTaskConnect> {
    let pending_resume = pending_resume?;
    if pending_resume.operation != CAMERA_CONNECT_OPERATION {
        return None;
    }
    let mut pending =
        serde_json::from_value::<PendingTaskConnect>(pending_resume.payload.clone()).ok()?;
    if pending.resume_token.trim().is_empty() {
        pending.resume_token = pending_resume.resume_token.clone();
    }
    Some(pending)
}

fn pending_clip_confirmation_from_resume(
    pending_resume: Option<&PendingTaskResumeState>,
) -> Option<PendingTaskClipConfirmation> {
    let pending_resume = pending_resume?;
    if pending_resume.operation != CAMERA_RECORD_CLIP_CONFIRMATION_OPERATION {
        return None;
    }
    let mut pending =
        serde_json::from_value::<PendingTaskClipConfirmation>(pending_resume.payload.clone())
            .ok()?;
    if pending.resume_token.trim().is_empty() {
        pending.resume_token = pending_resume.resume_token.clone();
    }
    Some(pending)
}

fn pending_general_message_loop_from_resume(
    pending_resume: Option<&PendingTaskResumeState>,
) -> Option<PendingTaskGeneralMessageLoop> {
    let pending_resume = pending_resume?;
    if pending_resume.operation != GENERAL_MESSAGE_LOOP_OPERATION {
        return None;
    }
    let mut pending =
        serde_json::from_value::<PendingTaskGeneralMessageLoop>(pending_resume.payload.clone())
            .ok()?;
    if pending.resume_token.trim().is_empty() {
        pending.resume_token = pending_resume.resume_token.clone();
    }
    Some(pending)
}

fn normalize_task_conversation_state(state: &mut TaskConversationState) {
    if state.pending_selection.is_none() && !state.pending_candidates.is_empty() {
        state.pending_selection = Some(pending_selection_from_camera_candidates(
            &state.pending_candidates,
        ));
    }
    if state.pending_resume.is_none() {
        if let Some(pending) = state.pending_connect.clone() {
            state.pending_resume = Some(pending_resume_from_camera_connect(&pending));
        }
    }
    state.pending_candidates.clear();
    state.pending_connect = None;
}

fn persisted_task_conversation_state(state: &TaskConversationState) -> TaskConversationState {
    let mut normalized = state.clone();
    normalize_task_conversation_state(&mut normalized);
    normalized
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskSessionStateEnvelope {
    #[serde(default = "default_task_session_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_task_session_namespace")]
    pub namespace: String,
    #[serde(default = "default_task_session_flow_type")]
    pub flow_type: String,
    #[serde(default)]
    pub flow_state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct TaskConversationFile {
    #[serde(default)]
    conversations: HashMap<String, TaskConversationState>,
    #[serde(default)]
    sessions: HashMap<String, ConversationSession>,
    #[serde(default)]
    task_runs: HashMap<String, TaskRun>,
    #[serde(default)]
    task_steps: HashMap<String, TaskStepRun>,
    #[serde(default)]
    artifacts: HashMap<String, ArtifactRecord>,
    #[serde(default)]
    media_assets: HashMap<String, MediaAsset>,
    #[serde(default)]
    approvals: HashMap<String, ApprovalTicket>,
    #[serde(default)]
    events: HashMap<String, EventRecord>,
    #[serde(default)]
    media_sessions: HashMap<String, MediaSession>,
    #[serde(default)]
    share_links: HashMap<String, ShareLink>,
}

#[derive(Debug, Clone)]
pub struct TaskConversationStore {
    path: PathBuf,
}

impl TaskConversationStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self, key: &str) -> Result<TaskConversationState, String> {
        let file = self.load_file()?;
        let mut state =
            file.conversations
                .get(key)
                .cloned()
                .unwrap_or_else(|| TaskConversationState {
                    key: key.to_string(),
                    ..Default::default()
                });
        normalize_task_conversation_state(&mut state);
        Ok(state)
    }

    pub fn save(&self, state: &TaskConversationState) -> Result<(), String> {
        if state.key.trim().is_empty() {
            return Err("conversation key 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        let stored_state = persisted_task_conversation_state(state);
        file.conversations
            .insert(stored_state.key.clone(), stored_state);
        self.save_file(&file)
    }

    pub fn clear(&self, key: &str) -> Result<(), String> {
        let mut file = self.load_file()?;
        file.conversations.remove(key);
        self.save_file(&file)
    }

    pub fn load_for_session(
        &self,
        session_id: &str,
        key_hint: Option<&str>,
    ) -> Result<Option<TaskConversationState>, String> {
        let file = self.load_file()?;
        if let Some(session) = file.sessions.get(session_id) {
            if let Some(state) = conversation_state_from_session(session, key_hint) {
                return Ok(Some(state));
            }
        }

        let Some(key) = non_empty_string(key_hint) else {
            return Ok(None);
        };
        Ok(file.conversations.get(key).cloned().map(|mut state| {
            if state.key.trim().is_empty() {
                state.key = key.to_string();
            }
            normalize_task_conversation_state(&mut state);
            state
        }))
    }

    pub fn save_for_session(
        &self,
        session: &ConversationSession,
        state: &TaskConversationState,
    ) -> Result<(), String> {
        if session.session_id.trim().is_empty() {
            return Err("session_id 不能为空".to_string());
        }
        if state.key.trim().is_empty() {
            return Err("conversation key 不能为空".to_string());
        }

        let mut file = self.load_file()?;
        let mut session = session.clone();
        let stored_state = persisted_task_conversation_state(state);
        let envelope = envelope_from_conversation_state(&stored_state, Some(&session))?;
        session.state = serde_json::to_value(&envelope).map_err(|error| {
            format!(
                "failed to serialize Task conversation session state {}: {error}",
                self.path.display()
            )
        })?;
        file.sessions.insert(session.session_id.clone(), session);
        file.conversations
            .insert(stored_state.key.clone(), stored_state);
        self.save_file(&file)
    }

    pub fn clear_for_session(
        &self,
        session_id: &str,
        key_hint: Option<&str>,
    ) -> Result<(), String> {
        let mut file = self.load_file()?;
        if let Some(session) = file.sessions.get_mut(session_id) {
            session.state = Value::Null;
            session.resume_token = None;
        }
        if let Some(key) = non_empty_string(key_hint) {
            file.conversations.remove(key);
        }
        self.save_file(&file)
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<ConversationSession>, String> {
        let file = self.load_file()?;
        Ok(file.sessions.get(session_id).cloned())
    }

    pub fn save_session(&self, session: &ConversationSession) -> Result<(), String> {
        if session.session_id.trim().is_empty() {
            return Err("session_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.sessions
            .insert(session.session_id.clone(), session.clone());
        self.save_file(&file)
    }

    pub fn load_task_run(&self, task_id: &str) -> Result<Option<TaskRun>, String> {
        let file = self.load_file()?;
        Ok(file.task_runs.get(task_id).cloned())
    }

    pub fn save_task_run(&self, task_run: &TaskRun) -> Result<(), String> {
        if task_run.task_id.trim().is_empty() {
            return Err("task_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.task_runs
            .insert(task_run.task_id.clone(), task_run.clone());
        self.save_file(&file)
    }

    pub fn recent_task_runs_for_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskRun>, String> {
        if session_id.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let file = self.load_file()?;
        let mut runs = file
            .task_runs
            .values()
            .filter(|task_run| task_run.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.task_id.cmp(&left.task_id))
        });
        runs.truncate(limit);
        Ok(runs)
    }

    pub fn load_task_step(&self, step_id: &str) -> Result<Option<TaskStepRun>, String> {
        let file = self.load_file()?;
        Ok(file.task_steps.get(step_id).cloned())
    }

    pub fn recent_task_steps(&self, limit: usize) -> Result<Vec<TaskStepRun>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let file = self.load_file()?;
        let mut steps = file.task_steps.values().cloned().collect::<Vec<_>>();
        steps.sort_by(|left, right| {
            left.ended_at
                .cmp(&right.ended_at)
                .then(left.started_at.cmp(&right.started_at))
                .then(left.step_id.cmp(&right.step_id))
        });
        if steps.len() > limit {
            steps = steps.split_off(steps.len() - limit);
        }
        Ok(steps)
    }

    pub fn save_task_step(&self, task_step: &TaskStepRun) -> Result<(), String> {
        if task_step.step_id.trim().is_empty() {
            return Err("step_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.task_steps
            .insert(task_step.step_id.clone(), task_step.clone());
        self.save_file(&file)
    }

    pub fn load_media_asset(&self, asset_id: &str) -> Result<Option<MediaAsset>, String> {
        let file = self.load_file()?;
        Ok(file.media_assets.get(asset_id).cloned())
    }

    pub fn list_media_assets(&self) -> Result<Vec<MediaAsset>, String> {
        let file = self.load_file()?;
        let mut media_assets = file.media_assets.values().cloned().collect::<Vec<_>>();
        media_assets.sort_by(|left, right| {
            left.captured_at
                .cmp(&right.captured_at)
                .then(left.asset_id.cmp(&right.asset_id))
        });
        Ok(media_assets)
    }

    pub fn save_media_asset(&self, media_asset: &MediaAsset) -> Result<(), String> {
        if media_asset.asset_id.trim().is_empty() {
            return Err("asset_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.media_assets
            .insert(media_asset.asset_id.clone(), media_asset.clone());
        self.save_file(&file)
    }

    pub fn load_media_session(
        &self,
        media_session_id: &str,
    ) -> Result<Option<MediaSession>, String> {
        let file = self.load_file()?;
        Ok(file.media_sessions.get(media_session_id).cloned())
    }

    pub fn list_media_sessions(&self) -> Result<Vec<MediaSession>, String> {
        let file = self.load_file()?;
        let mut media_sessions = file.media_sessions.values().cloned().collect::<Vec<_>>();
        media_sessions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then(left.media_session_id.cmp(&right.media_session_id))
        });
        Ok(media_sessions)
    }

    pub fn save_media_session(&self, media_session: &MediaSession) -> Result<(), String> {
        if media_session.media_session_id.trim().is_empty() {
            return Err("media_session_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.media_sessions.insert(
            media_session.media_session_id.clone(),
            media_session.clone(),
        );
        self.save_file(&file)
    }

    pub fn close_media_session(
        &self,
        media_session_id: &str,
        ended_at: Option<String>,
    ) -> Result<Option<MediaSession>, String> {
        let mut file = self.load_file()?;
        let Some(media_session) = file.media_sessions.get_mut(media_session_id) else {
            return Ok(None);
        };
        media_session.status = MediaSessionStatus::Closed;
        if ended_at.is_some() {
            media_session.ended_at = ended_at;
        }
        let updated = media_session.clone();
        self.save_file(&file)?;
        Ok(Some(updated))
    }

    pub fn load_share_link(&self, share_link_id: &str) -> Result<Option<ShareLink>, String> {
        let file = self.load_file()?;
        Ok(file.share_links.get(share_link_id).cloned())
    }

    pub fn list_share_links(&self) -> Result<Vec<ShareLink>, String> {
        let file = self.load_file()?;
        let mut share_links = file.share_links.values().cloned().collect::<Vec<_>>();
        share_links.sort_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then(left.share_link_id.cmp(&right.share_link_id))
        });
        Ok(share_links)
    }

    pub fn find_share_link_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<ShareLink>, String> {
        let file = self.load_file()?;
        Ok(file
            .share_links
            .values()
            .find(|share_link| share_link.token_hash == token_hash)
            .cloned())
    }

    pub fn save_share_link(&self, share_link: &ShareLink) -> Result<(), String> {
        if share_link.share_link_id.trim().is_empty() {
            return Err("share_link_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.share_links
            .insert(share_link.share_link_id.clone(), share_link.clone());
        self.save_file(&file)
    }

    pub fn save_share_link_bundle(
        &self,
        media_session: &MediaSession,
        share_link: &ShareLink,
    ) -> Result<(), String> {
        if media_session.media_session_id.trim().is_empty() {
            return Err("media_session_id 不能为空".to_string());
        }
        if share_link.share_link_id.trim().is_empty() {
            return Err("share_link_id 不能为空".to_string());
        }
        if share_link.media_session_id != media_session.media_session_id {
            return Err("share_link.media_session_id 必须匹配 media_session_id".to_string());
        }

        let mut file = self.load_file()?;
        file.media_sessions.insert(
            media_session.media_session_id.clone(),
            media_session.clone(),
        );
        file.share_links
            .insert(share_link.share_link_id.clone(), share_link.clone());
        self.save_file(&file)
    }

    pub fn revoke_share_link(
        &self,
        share_link_id: &str,
        revoked_at: Option<String>,
    ) -> Result<Option<ShareLink>, String> {
        let mut file = self.load_file()?;
        let Some(share_link) = file.share_links.get_mut(share_link_id) else {
            return Ok(None);
        };
        if revoked_at.is_some() {
            share_link.revoked_at = revoked_at;
        }
        let updated = share_link.clone();
        self.save_file(&file)?;
        Ok(Some(updated))
    }

    pub fn artifacts_for_task(&self, task_id: &str) -> Result<Vec<ArtifactRecord>, String> {
        let file = self.load_file()?;
        let mut artifacts = file
            .artifacts
            .values()
            .filter(|artifact| artifact.task_id == task_id)
            .cloned()
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
        Ok(artifacts)
    }

    pub fn replace_artifacts_for_step(
        &self,
        task_id: &str,
        step_id: Option<&str>,
        artifacts: &[ArtifactRecord],
    ) -> Result<(), String> {
        let mut file = self.load_file()?;
        file.artifacts.retain(|_, artifact| {
            !(artifact.task_id == task_id && artifact.step_id.as_deref() == step_id)
        });
        for artifact in artifacts {
            if artifact.artifact_id.trim().is_empty() {
                return Err("artifact_id 不能为空".to_string());
            }
            file.artifacts
                .insert(artifact.artifact_id.clone(), artifact.clone());
        }
        self.save_file(&file)
    }

    pub fn approvals_for_task(&self, task_id: &str) -> Result<Vec<ApprovalTicket>, String> {
        let file = self.load_file()?;
        let mut approvals = file
            .approvals
            .values()
            .filter(|approval| approval.task_id == task_id)
            .cloned()
            .collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then(left.approval_id.cmp(&right.approval_id))
        });
        Ok(approvals)
    }

    pub fn pending_approvals(&self) -> Result<Vec<ApprovalTicket>, String> {
        let file = self.load_file()?;
        let mut approvals = file
            .approvals
            .values()
            .filter(|approval| approval.status == ApprovalStatus::Pending)
            .cloned()
            .collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then(left.approval_id.cmp(&right.approval_id))
        });
        Ok(approvals)
    }

    pub fn load_approval(&self, approval_id: &str) -> Result<Option<ApprovalTicket>, String> {
        let file = self.load_file()?;
        Ok(file.approvals.get(approval_id).cloned())
    }

    pub fn save_approval(&self, approval: &ApprovalTicket) -> Result<(), String> {
        if approval.approval_id.trim().is_empty() {
            return Err("approval_id 不能为空".to_string());
        }
        let mut file = self.load_file()?;
        file.approvals
            .insert(approval.approval_id.clone(), approval.clone());
        self.save_file(&file)
    }

    pub fn resolve_pending_approvals(
        &self,
        task_id: &str,
        approver_user_id: Option<String>,
        decided_at: Option<String>,
    ) -> Result<Vec<ApprovalTicket>, String> {
        let mut file = self.load_file()?;
        let mut updated = Vec::new();
        for approval in file.approvals.values_mut() {
            if approval.task_id != task_id || approval.status != ApprovalStatus::Pending {
                continue;
            }
            approval.status = ApprovalStatus::Approved;
            if approver_user_id.is_some() {
                approval.approver_user_id = approver_user_id.clone();
            }
            if decided_at.is_some() {
                approval.decided_at = decided_at.clone();
            }
            updated.push(approval.clone());
        }
        self.save_file(&file)?;
        Ok(updated)
    }

    pub fn update_approval_status(
        &self,
        approval_id: &str,
        status: ApprovalStatus,
        approver_user_id: Option<String>,
        decided_at: Option<String>,
    ) -> Result<Option<ApprovalTicket>, String> {
        let mut file = self.load_file()?;
        let Some(approval) = file.approvals.get_mut(approval_id) else {
            return Ok(None);
        };
        approval.status = status;
        if approver_user_id.is_some() {
            approval.approver_user_id = approver_user_id;
        }
        if decided_at.is_some() {
            approval.decided_at = decided_at;
        }
        let updated = approval.clone();
        self.save_file(&file)?;
        Ok(Some(updated))
    }

    pub fn events_for_task(&self, task_id: &str) -> Result<Vec<EventRecord>, String> {
        let file = self.load_file()?;
        let mut events = file
            .events
            .values()
            .filter(|event| event.source_id == task_id)
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.occurred_at
                .cmp(&right.occurred_at)
                .then(left.event_id.cmp(&right.event_id))
        });
        Ok(events)
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<EventRecord>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let file = self.load_file()?;
        let mut events = file.events.values().cloned().collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.occurred_at
                .cmp(&right.occurred_at)
                .then(left.event_id.cmp(&right.event_id))
        });
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
        Ok(events)
    }

    pub fn replace_events_for_step(
        &self,
        task_id: &str,
        step_id: Option<&str>,
        events: &[EventRecord],
    ) -> Result<(), String> {
        let mut file = self.load_file()?;
        file.events.retain(|_, event| {
            !(event.source_id == task_id && event.causation_id.as_deref() == step_id)
        });
        for event in events {
            if event.event_id.trim().is_empty() {
                return Err("event_id 不能为空".to_string());
            }
            file.events.insert(event.event_id.clone(), event.clone());
        }
        self.save_file(&file)
    }

    fn load_file(&self) -> Result<TaskConversationFile, String> {
        if !self.path.exists() {
            return Ok(TaskConversationFile::default());
        }

        let text = fs::read_to_string(&self.path).map_err(|error| {
            format!(
                "failed to read Task conversation state {}: {error}",
                self.path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|error| {
            format!(
                "failed to parse Task conversation state {}: {error}",
                self.path.display()
            )
        })
    }

    fn save_file(&self, file: &TaskConversationFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create Task conversation directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        let payload = serde_json::to_string_pretty(file).map_err(|error| {
            format!(
                "failed to serialize Task conversation state {}: {error}",
                self.path.display()
            )
        })?;
        fs::write(&self.path, payload).map_err(|error| {
            format!(
                "failed to write Task conversation state {}: {error}",
                self.path.display()
            )
        })
    }
}

fn conversation_state_from_session(
    session: &ConversationSession,
    key_hint: Option<&str>,
) -> Option<TaskConversationState> {
    if session.state.is_null() {
        return None;
    }

    if let Ok(envelope) = serde_json::from_value::<TaskSessionStateEnvelope>(session.state.clone())
    {
        if !envelope.flow_state.is_null() {
            if let Ok(mut state) =
                serde_json::from_value::<TaskConversationState>(envelope.flow_state)
            {
                backfill_conversation_key(&mut state, session, key_hint);
                normalize_task_conversation_state(&mut state);
                return Some(state);
            }
        }
    }

    let mut state: TaskConversationState = serde_json::from_value(session.state.clone()).ok()?;
    backfill_conversation_key(&mut state, session, key_hint);
    normalize_task_conversation_state(&mut state);
    Some(state)
}

fn envelope_from_conversation_state(
    state: &TaskConversationState,
    session: Option<&ConversationSession>,
) -> Result<TaskSessionStateEnvelope, String> {
    let stored_state = persisted_task_conversation_state(state);
    let mut flow_state = serde_json::to_value(&stored_state)
        .map_err(|error| format!("failed to serialize Task conversation flow state: {error}"))?;

    if let Some(flow_state_object) = flow_state.as_object_mut() {
        if stored_state.key.trim().is_empty() {
            flow_state_object.remove("key");
        } else if let Some(session) = session {
            let matches_conversation = non_empty_string(Some(session.conversation_id.as_str()))
                .map(|value| value == stored_state.key)
                .unwrap_or(false);
            let matches_session = non_empty_string(Some(session.session_id.as_str()))
                .map(|value| value == stored_state.key)
                .unwrap_or(false);
            let matches_user = non_empty_string(Some(session.user_id.as_str()))
                .map(|value| value == stored_state.key)
                .unwrap_or(false);
            if matches_conversation || matches_session || matches_user {
                flow_state_object.remove("key");
            }
        }
    }

    Ok(TaskSessionStateEnvelope {
        schema_version: default_task_session_schema_version(),
        namespace: default_task_session_namespace(),
        flow_type: default_task_session_flow_type(),
        flow_state,
    })
}

pub fn session_state_value_from_conversation(
    state: &TaskConversationState,
    session: Option<&ConversationSession>,
) -> Result<Value, String> {
    let envelope = envelope_from_conversation_state(state, session)?;
    serde_json::to_value(envelope)
        .map_err(|error| format!("failed to serialize Task conversation session state: {error}"))
}

fn backfill_conversation_key(
    state: &mut TaskConversationState,
    session: &ConversationSession,
    key_hint: Option<&str>,
) {
    if state.key.trim().is_empty() {
        state.key = non_empty_string(key_hint)
            .or_else(|| non_empty_string(Some(session.conversation_id.as_str())))
            .or_else(|| non_empty_string(Some(session.session_id.as_str())))
            .or_else(|| non_empty_string(Some(session.user_id.as_str())))
            .unwrap_or_default()
            .to_string();
    }
}

fn default_task_session_schema_version() -> u32 {
    1
}

fn default_task_session_namespace() -> String {
    "task_api".to_string()
}

fn default_task_session_flow_type() -> String {
    "camera_onboarding".to_string()
}

fn non_empty_string(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{json, Value};

    use super::{
        PendingTaskCandidate, PendingTaskClipConfirmation, PendingTaskConnect,
        PendingTaskGeneralMessageLoop, RecentClipPlaybackState, TaskConversationState,
        TaskConversationStore, TaskSessionStateEnvelope,
    };
    use crate::control_plane::approvals::{ApprovalStatus, ApprovalTicket};
    use crate::control_plane::events::{EventRecord, EventSeverity, EventSourceKind};
    use crate::control_plane::media::{
        MediaAsset, MediaAssetKind, MediaDeliveryMode, MediaSession, MediaSessionKind,
        MediaSessionStatus, ShareAccessScope, ShareLink, StorageTargetKind,
    };
    use crate::control_plane::tasks::{
        ArtifactKind, ArtifactRecord, ConversationSession, ExecutionRoute, TaskRun, TaskRunStatus,
        TaskStepRun, TaskStepRunStatus,
    };
    use crate::orchestrator::contracts::RiskLevel;

    fn unique_store_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}.json"))
    }

    #[test]
    fn conversation_store_round_trips_state() {
        let path = unique_store_path("harborbeacon-task-conversations");
        let store = TaskConversationStore::new(&path);
        let pending_candidate = PendingTaskCandidate {
            candidate_id: "cand-1".to_string(),
            name: "Gate Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: Some("Entry".to_string()),
            port: 554,
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: true,
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
        };
        let pending_connect = PendingTaskConnect {
            resume_token: "resume-1".to_string(),
            name: "Gate Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: Some("Entry".to_string()),
            port: 554,
            snapshot_url: None,
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: true,
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
        };
        let mut state = TaskConversationState {
            key: "chat-demo".to_string(),
            last_scan_cidr: "192.168.1.0/24".to_string(),
            ..Default::default()
        };
        state.set_camera_pending_candidates(vec![pending_candidate.clone()]);
        state.set_camera_pending_connect(Some(pending_connect.clone()));

        store.save(&state).expect("save");
        let loaded = store.load("chat-demo").expect("load");

        assert_eq!(loaded, state);
        assert_eq!(loaded.camera_pending_candidates(), vec![pending_candidate]);
        assert_eq!(loaded.camera_pending_connect(), Some(pending_connect));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn runtime_records_round_trip() {
        let path = unique_store_path("harborbeacon-task-runtime");
        let store = TaskConversationStore::new(&path);

        let session = ConversationSession {
            session_id: "sess-1".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "feishu".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-1".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_1".to_string(),
            last_message_id: "om_1".to_string(),
            chat_type: "p2p".to_string(),
            state: json!({"pending_candidates": 1}),
            resume_token: Some("resume-1".to_string()),
            expires_at: None,
        };
        store.save_session(&session).expect("save session");

        let task_run = TaskRun {
            task_id: "task-1".to_string(),
            workspace_id: "home-1".to_string(),
            session_id: "sess-1".to_string(),
            source_channel: "feishu".to_string(),
            domain: "camera".to_string(),
            action: "connect".to_string(),
            intent_text: "接入 1".to_string(),
            entity_refs: json!({"candidate_index": 1}),
            args: json!({"resume_token": "resume-1"}),
            autonomy_level: "supervised".to_string(),
            status: TaskRunStatus::NeedsInput,
            risk_level: RiskLevel::Medium,
            requires_approval: false,
            started_at: Some("1710000000".to_string()),
            completed_at: None,
            metadata: json!({"trace_id": "trace-1"}),
        };
        store.save_task_run(&task_run).expect("save task");

        let task_step = TaskStepRun {
            step_id: "step-1".to_string(),
            task_id: "task-1".to_string(),
            trace_id: "trace-1".to_string(),
            route_key: "gw_route_1".to_string(),
            domain: "camera".to_string(),
            operation: "connect".to_string(),
            route: ExecutionRoute::Local,
            executor_used: "camera_hub_service".to_string(),
            status: TaskStepRunStatus::Blocked,
            input_payload: json!({"candidate_index": 1}),
            output_payload: json!({"prompt": "密码 xxxxxx"}),
            error_code: None,
            error_message: None,
            audit_ref: Some("audit-1".to_string()),
            started_at: Some("1710000000".to_string()),
            ended_at: Some("1710000001".to_string()),
        };
        store.save_task_step(&task_step).expect("save step");

        let artifacts = vec![ArtifactRecord {
            artifact_id: "artifact-1".to_string(),
            task_id: "task-1".to_string(),
            trace_id: "trace-1".to_string(),
            step_id: Some("step-1".to_string()),
            route_key: "gw_route_1".to_string(),
            artifact_kind: ArtifactKind::Json,
            label: "候选设备".to_string(),
            mime_type: "application/json".to_string(),
            media_asset_id: None,
            path: None,
            url: None,
            metadata: Value::Null,
        }];
        store
            .replace_artifacts_for_step("task-1", Some("step-1"), &artifacts)
            .expect("save artifacts");

        assert_eq!(
            store.load_session("sess-1").expect("load session"),
            Some(session)
        );
        assert_eq!(
            store.load_task_run("task-1").expect("load task"),
            Some(task_run)
        );
        assert_eq!(
            store.load_task_step("step-1").expect("load step"),
            Some(task_step)
        );
        assert_eq!(
            store.artifacts_for_task("task-1").expect("load artifacts"),
            artifacts
        );

        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn load_for_session_prefers_session_state_and_backfills_key() {
        let path = unique_store_path("harborbeacon-task-session-state");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-1".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "im_bridge".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-1".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_state".to_string(),
            last_message_id: "om_state".to_string(),
            chat_type: "group".to_string(),
            state: json!({
                "pending_candidates": [{
                    "candidate_id": "cand-1",
                    "name": "Gate Cam",
                    "ip": "192.168.1.20",
                    "port": 554
                }],
                "last_scan_cidr": "192.168.1.0/24"
            }),
            resume_token: None,
            expires_at: None,
        };
        store.save_session(&session).expect("save session");

        let loaded = store
            .load_for_session("sess-1", Some("chat-1"))
            .expect("load")
            .expect("state");

        assert_eq!(loaded.key, "chat-1");
        assert_eq!(loaded.camera_pending_candidates().len(), 1);
        assert!(loaded.pending_selection.is_some());
        assert!(loaded.pending_candidates.is_empty());
        assert_eq!(loaded.last_scan_cidr, "192.168.1.0/24");
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn load_for_session_reads_envelope_state() {
        let path = unique_store_path("harborbeacon-task-session-envelope");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-1".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "im_bridge".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-1".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_envelope".to_string(),
            last_message_id: "om_envelope".to_string(),
            chat_type: "group".to_string(),
            state: serde_json::to_value(TaskSessionStateEnvelope {
                schema_version: 1,
                namespace: "task_api".to_string(),
                flow_type: "camera_onboarding".to_string(),
                flow_state: json!({
                    "pending_candidates": [{
                        "candidate_id": "cand-1",
                        "name": "Gate Cam",
                        "ip": "192.168.1.20",
                        "port": 554
                    }],
                    "last_scan_cidr": "192.168.1.0/24"
                }),
            })
            .expect("encode envelope"),
            resume_token: None,
            expires_at: None,
        };
        store.save_session(&session).expect("save session");

        let loaded = store
            .load_for_session("sess-1", Some("chat-1"))
            .expect("load")
            .expect("state");

        assert_eq!(loaded.key, "chat-1");
        assert_eq!(loaded.camera_pending_candidates().len(), 1);
        assert!(loaded.pending_selection.is_some());
        assert!(loaded.pending_candidates.is_empty());
        assert_eq!(loaded.last_scan_cidr, "192.168.1.0/24");
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn load_for_session_backfills_pending_resume_from_legacy_connect_state() {
        let path = unique_store_path("harborbeacon-task-session-resume");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-1".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "im_bridge".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-1".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_resume".to_string(),
            last_message_id: "om_resume".to_string(),
            chat_type: "group".to_string(),
            state: json!({
                "pending_connect": {
                    "resume_token": "resume-1",
                    "name": "Gate Cam",
                    "ip": "192.168.1.20",
                    "port": 554,
                    "snapshot_url": "http://192.168.1.20/snapshot.jpg",
                    "rtsp_paths": ["/live"],
                    "requires_auth": true
                }
            }),
            resume_token: None,
            expires_at: None,
        };
        store.save_session(&session).expect("save session");

        let loaded = store
            .load_for_session("sess-1", Some("chat-1"))
            .expect("load")
            .expect("state");

        assert!(loaded.pending_resume.is_some());
        assert!(loaded.pending_connect.is_none());
        assert_eq!(
            loaded.camera_pending_connect(),
            Some(PendingTaskConnect {
                resume_token: "resume-1".to_string(),
                name: "Gate Cam".to_string(),
                ip: "192.168.1.20".to_string(),
                room: None,
                port: 554,
                snapshot_url: Some("http://192.168.1.20/snapshot.jpg".to_string()),
                rtsp_paths: vec!["/live".to_string()],
                requires_auth: true,
                vendor: None,
                model: None,
            })
        );
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn save_for_session_preserves_resume_token_for_camera_connect_continuation() {
        let path = unique_store_path("harborbeacon-task-session-resume-token");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-resume".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "feishu".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-resume".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_resume".to_string(),
            last_message_id: "om_resume".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut state = TaskConversationState {
            key: "chat-resume".to_string(),
            ..Default::default()
        };
        state.set_camera_pending_connect(Some(PendingTaskConnect {
            resume_token: "resume-opaque-1".to_string(),
            name: "Gate Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: Some("Entry".to_string()),
            port: 554,
            snapshot_url: Some("http://192.168.1.20/snapshot.jpg".to_string()),
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: true,
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
        }));

        store
            .save_for_session(&session, &state)
            .expect("save for session");

        let loaded = store
            .load_for_session("sess-resume", Some("chat-resume"))
            .expect("load")
            .expect("state");

        assert_eq!(
            loaded.camera_pending_connect(),
            Some(PendingTaskConnect {
                resume_token: "resume-opaque-1".to_string(),
                name: "Gate Cam".to_string(),
                ip: "192.168.1.20".to_string(),
                room: Some("Entry".to_string()),
                port: 554,
                snapshot_url: Some("http://192.168.1.20/snapshot.jpg".to_string()),
                rtsp_paths: vec!["/live".to_string()],
                requires_auth: true,
                vendor: Some("Demo".to_string()),
                model: Some("X1".to_string()),
            })
        );
        assert_eq!(loaded.key, "chat-resume");
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn save_for_session_round_trips_clip_confirmation_pending_resume() {
        let path = unique_store_path("harborbeacon-task-session-clip-confirmation");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-clip".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-clip".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip".to_string(),
            last_message_id: "om_clip".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let pending = PendingTaskClipConfirmation {
            resume_token: "resume-clip-1".to_string(),
            clip_media_asset_id: "asset-clip-1".to_string(),
            clip_path: "captures/clip-1.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/clip-1/frame-1.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
        };
        let mut state = TaskConversationState {
            key: "chat-clip".to_string(),
            ..Default::default()
        };
        state.set_clip_pending_confirmation(Some(pending.clone()));

        store
            .save_for_session(&session, &state)
            .expect("save clip confirmation");

        let loaded = store
            .load_for_session("sess-clip", Some("chat-clip"))
            .expect("load")
            .expect("state");

        assert_eq!(loaded.clip_pending_confirmation(), Some(pending));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn save_for_session_round_trips_recent_clip_playback_state() {
        let path = unique_store_path("harborbeacon-task-session-recent-clip-playback");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-recent-clip".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-recent-clip".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_recent_clip".to_string(),
            last_message_id: "om_recent_clip".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let recent_clip = RecentClipPlaybackState {
            clip_media_asset_id: "asset-clip-recent-1".to_string(),
            clip_path: "captures/clip-recent-1.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/clip-recent-1/frame-1.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
            captured_at_epoch_ms: 1_710_000_000_000,
        };
        let mut state = TaskConversationState {
            key: "chat-recent-clip".to_string(),
            ..Default::default()
        };
        state.set_recent_clip_playback(Some(recent_clip.clone()));

        store
            .save_for_session(&session, &state)
            .expect("save recent clip playback");

        let loaded = store
            .load_for_session("sess-recent-clip", Some("chat-recent-clip"))
            .expect("load")
            .expect("state");

        assert_eq!(loaded.recent_clip_playback(), Some(recent_clip));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn save_for_session_round_trips_general_message_loop_pending_resume() {
        let path = unique_store_path("harborbeacon-task-session-general-message-loop");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-general".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-general".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_general".to_string(),
            last_message_id: "om_general".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let pending = PendingTaskGeneralMessageLoop {
            resume_token: "resume-general-1".to_string(),
            original_goal: "帮我看一下门口".to_string(),
            latest_user_intent_text: "帮我看一下门口".to_string(),
            last_clarification_prompt: "你是想拍一张，还是录一段门口摄像头？".to_string(),
            selected_candidate_action: None,
            camera_hint: Some("front-door".to_string()),
            query: None,
            home_assistant: None,
        };
        let mut state = TaskConversationState {
            key: "chat-general".to_string(),
            ..Default::default()
        };
        state.set_general_message_loop(Some(pending.clone()));

        store
            .save_for_session(&session, &state)
            .expect("save general message loop");

        let loaded = store
            .load_for_session("sess-general", Some("chat-general"))
            .expect("load")
            .expect("state");

        assert_eq!(loaded.general_message_loop(), Some(pending));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn recent_task_runs_for_session_is_bounded_and_session_scoped() {
        let path = unique_store_path("harborbeacon-task-session-recent-runs");
        let store = TaskConversationStore::new(&path);

        for (task_id, session_id, started_at) in [
            ("task-1", "sess-a", "1710000001"),
            ("task-2", "sess-a", "1710000003"),
            ("task-3", "sess-b", "1710000004"),
            ("task-4", "sess-a", "1710000002"),
            ("task-5", "sess-a", "1710000005"),
        ] {
            store
                .save_task_run(&TaskRun {
                    task_id: task_id.to_string(),
                    workspace_id: "home-1".to_string(),
                    session_id: session_id.to_string(),
                    source_channel: "weixin".to_string(),
                    domain: "general".to_string(),
                    action: "message".to_string(),
                    intent_text: format!("intent-{task_id}"),
                    entity_refs: Value::Null,
                    args: Value::Null,
                    autonomy_level: "supervised".to_string(),
                    status: TaskRunStatus::Completed,
                    risk_level: RiskLevel::Low,
                    requires_approval: false,
                    started_at: Some(started_at.to_string()),
                    completed_at: Some(started_at.to_string()),
                    metadata: Value::Null,
                })
                .expect("save task run");
        }

        let recent = store
            .recent_task_runs_for_session("sess-a", 3)
            .expect("recent task runs");

        assert_eq!(
            recent
                .iter()
                .map(|run| run.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["task-5", "task-2", "task-4"]
        );
        assert!(recent.iter().all(|run| run.session_id == "sess-a"));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn save_for_session_updates_session_state_and_legacy_map() {
        let path = unique_store_path("harborbeacon-task-session-save");
        let store = TaskConversationStore::new(&path);
        let session = ConversationSession {
            session_id: "sess-1".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "im_bridge".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-1".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_save".to_string(),
            last_message_id: "om_save".to_string(),
            chat_type: "group".to_string(),
            state: Value::Null,
            resume_token: Some("resume-1".to_string()),
            expires_at: None,
        };
        let pending_candidate = PendingTaskCandidate {
            candidate_id: "cand-1".to_string(),
            name: "Gate Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: None,
            port: 554,
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: false,
            vendor: None,
            model: None,
        };
        let mut state = TaskConversationState {
            key: "chat-1".to_string(),
            last_scan_cidr: "192.168.1.0/24".to_string(),
            ..Default::default()
        };
        state.set_camera_pending_candidates(vec![pending_candidate.clone()]);

        store
            .save_for_session(&session, &state)
            .expect("save for session");

        let saved_session = store
            .load_session("sess-1")
            .expect("load session")
            .expect("session");
        assert_eq!(saved_session.state["schema_version"], 1);
        assert_eq!(saved_session.state["namespace"], "task_api");
        assert_eq!(saved_session.state["flow_type"], "camera_onboarding");
        assert_eq!(
            saved_session.state["flow_state"]["last_scan_cidr"],
            "192.168.1.0/24"
        );
        assert!(saved_session.state["flow_state"]["pending_selection"].is_object());
        assert!(saved_session.state["flow_state"]["pending_candidates"].is_null());
        assert_eq!(saved_session.resume_token.as_deref(), Some("resume-1"));
        let loaded = store.load("chat-1").expect("legacy load");
        assert_eq!(loaded, state);
        assert_eq!(loaded.camera_pending_candidates(), vec![pending_candidate]);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn approval_and_event_records_round_trip() {
        let path = unique_store_path("harborbeacon-task-governance");
        let store = TaskConversationStore::new(&path);
        let approval = ApprovalTicket {
            approval_id: "approval-1".to_string(),
            task_id: "task-1".to_string(),
            trace_id: "trace-1".to_string(),
            route_key: "gw_route_1".to_string(),
            policy_ref: "camera.connect".to_string(),
            requester_user_id: "user-1".to_string(),
            approver_user_id: None,
            status: ApprovalStatus::Pending,
            reason: "camera.connect requires approval".to_string(),
            requested_at: Some("1710000000".to_string()),
            decided_at: None,
        };
        store.save_approval(&approval).expect("save approval");
        store
            .replace_events_for_step(
                "task-1",
                Some("step-1"),
                &[EventRecord {
                    event_id: "event-1".to_string(),
                    workspace_id: "home-1".to_string(),
                    source_kind: EventSourceKind::Task,
                    source_id: "task-1".to_string(),
                    event_type: "task.needs_input".to_string(),
                    severity: EventSeverity::Warning,
                    payload: json!({"message": "需要审批"}),
                    correlation_id: Some("trace-1".to_string()),
                    causation_id: Some("step-1".to_string()),
                    occurred_at: Some("1710000001".to_string()),
                    ingested_at: Some("1710000001".to_string()),
                }],
            )
            .expect("save events");

        let approvals = store.approvals_for_task("task-1").expect("load approvals");
        let events = store.events_for_task("task-1").expect("load events");

        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);
        assert_eq!(approvals[0].trace_id, "trace-1");
        assert_eq!(approvals[0].route_key, "gw_route_1");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "task.needs_input");

        let updated = store
            .resolve_pending_approvals(
                "task-1",
                Some("approver-1".to_string()),
                Some("1710000002".to_string()),
            )
            .expect("resolve approvals");
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].status, ApprovalStatus::Approved);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn media_assets_round_trip() {
        let path = unique_store_path("harborbeacon-media-assets");
        let store = TaskConversationStore::new(&path);
        let media_asset = MediaAsset {
            asset_id: "asset-1".to_string(),
            workspace_id: "home-1".to_string(),
            device_id: Some("cam-1".to_string()),
            asset_kind: MediaAssetKind::Snapshot,
            storage_target: StorageTargetKind::LocalDisk,
            storage_uri: "snapshots/cam-1/1710000000000.jpg".to_string(),
            mime_type: "image/jpeg".to_string(),
            byte_size: 1024,
            checksum: Some("sha256:demo".to_string()),
            captured_at: Some("1710000000000".to_string()),
            started_at: None,
            ended_at: None,
            derived_from_asset_id: None,
            tags: vec!["snapshot".to_string(), "camera".to_string()],
            metadata: json!({
                "task_id": "task-1",
            }),
        };

        store
            .save_media_asset(&media_asset)
            .expect("save media asset");

        assert_eq!(
            store.load_media_asset("asset-1").expect("load media asset"),
            Some(media_asset.clone())
        );
        assert_eq!(
            store.list_media_assets().expect("list media assets"),
            vec![media_asset]
        );

        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn share_link_records_round_trip_and_can_be_revoked() {
        let path = unique_store_path("harborbeacon-share-links");
        let store = TaskConversationStore::new(&path);
        let media_session = MediaSession {
            media_session_id: "media-session-1".to_string(),
            device_id: "cam-1".to_string(),
            stream_profile_id: "cam-1::stream::primary".to_string(),
            session_kind: MediaSessionKind::Share,
            delivery_mode: MediaDeliveryMode::Hls,
            opened_by_user_id: Some("user-1".to_string()),
            status: MediaSessionStatus::Active,
            share_link_id: Some("share-link-1".to_string()),
            started_at: Some("1710000000".to_string()),
            ended_at: None,
            metadata: json!({
                "task_id": "task-1",
            }),
        };
        let share_link = ShareLink {
            share_link_id: "share-link-1".to_string(),
            media_session_id: media_session.media_session_id.clone(),
            token_hash: "token-hash-1".to_string(),
            access_scope: ShareAccessScope::PublicLink,
            expires_at: Some("1710003600".to_string()),
            revoked_at: None,
        };

        store
            .save_share_link_bundle(&media_session, &share_link)
            .expect("save share bundle");

        assert_eq!(
            store
                .load_media_session("media-session-1")
                .expect("load session"),
            Some(media_session.clone())
        );
        assert_eq!(
            store
                .load_share_link("share-link-1")
                .expect("load share link"),
            Some(share_link.clone())
        );
        assert_eq!(
            store
                .find_share_link_by_token_hash("token-hash-1")
                .expect("find share link"),
            Some(share_link.clone())
        );

        let revoked = store
            .revoke_share_link("share-link-1", Some("1710001800".to_string()))
            .expect("revoke")
            .expect("share link");
        assert_eq!(revoked.revoked_at.as_deref(), Some("1710001800"));

        let closed = store
            .close_media_session("media-session-1", Some("1710001800".to_string()))
            .expect("close session")
            .expect("media session");
        assert_eq!(closed.status, MediaSessionStatus::Closed);
        assert_eq!(closed.ended_at.as_deref(), Some("1710001800"));

        let _ = fs::remove_file(store.path());
    }
}
