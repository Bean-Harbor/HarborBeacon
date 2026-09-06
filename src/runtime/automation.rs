//! Event-driven automation execution.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::connectors::home_assistant::{
    home_assistant_service_action_is_allowlisted, validate_home_assistant_service_fields,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationTrigger {
    Event,
    Schedule,
    Manual,
}

const MAX_RULES: usize = 100;
const MAX_ACTIONS: usize = 20;
const MAX_CONDITIONS: usize = 20;
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RUN_HISTORY_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleTrigger {
    Manual,
    Event { event_type: String },
    State { entity_id: String, to: String },
    Schedule { interval_seconds: u64 },
}

impl<'de> Deserialize<'de> for RuleTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // An internally tagged unit variant otherwise ignores unexpected fields.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum StrictTrigger {
            Manual {},
            Event { event_type: String },
            State { entity_id: String, to: String },
            Schedule { interval_seconds: u64 },
        }
        Ok(match StrictTrigger::deserialize(deserializer)? {
            StrictTrigger::Manual {} => Self::Manual,
            StrictTrigger::Event { event_type } => Self::Event { event_type },
            StrictTrigger::State { entity_id, to } => Self::State { entity_id, to },
            StrictTrigger::Schedule { interval_seconds } => Self::Schedule { interval_seconds },
        })
    }
}

impl RuleTrigger {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Event { .. } => "event",
            Self::State { .. } => "state",
            Self::Schedule { .. } => "schedule",
        }
    }

    pub fn matches_event(&self, candidate: &str) -> bool {
        matches!(self, Self::Event { event_type } if event_type == candidate)
    }

    pub fn matches_state(&self, candidate_entity: &str, candidate_state: &str) -> bool {
        !unavailable(candidate_state)
            && matches!(self, Self::State { entity_id, to } if entity_id == candidate_entity && to == candidate_state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleCondition {
    pub entity_id: String,
    pub operator: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleConditions {
    pub match_mode: String,
    pub items: Vec<RuleCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleAction {
    Record {
        message: String,
    },
    HomeAssistant {
        entity_id: String,
        domain: String,
        service: String,
        fields: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuleDefinition {
    pub name: String,
    pub trigger: RuleTrigger,
    pub conditions: RuleConditions,
    pub actions: Vec<RuleAction>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuleRecord {
    pub rule_id: String,
    pub revision: u64,
    pub previewed_revision: Option<u64>,
    pub status: String,
    pub definition: RuleDefinition,
    pub created_at: u64,
    pub updated_at: u64,
    pub next_run_at: Option<u64>,
    #[serde(default)]
    pub activation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RulePreview {
    pub rule_id: String,
    pub revision: u64,
    pub conditions_matched: bool,
    pub actions: Vec<RuleAction>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleActionResult {
    pub index: usize,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleRun {
    pub run_id: String,
    pub rule_id: String,
    pub revision: u64,
    pub trigger_id: String,
    pub trigger_kind: String,
    pub status: String,
    pub reason: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub conditions_matched: bool,
    pub actions: Vec<RuleActionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesState {
    schema_version: u32,
    rules: BTreeMap<String, RuleRecord>,
    runs: Vec<RuleRun>,
}

impl Default for RulesState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rules: BTreeMap::new(),
            runs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RulesStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl RulesStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn acquire(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.lock
            .lock()
            .map_err(|_| "STORAGE: Rules store lock is unavailable".into())
    }

    fn load(&self) -> Result<RulesState, String> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RulesState::default());
            }
            Err(_) => return Err("STORAGE: Cannot open rules store".into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "STORAGE: Cannot read rules store")?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("STORAGE: Rules store exceeds the storage limit".into());
        }
        if bytes.is_empty() {
            return Ok(RulesState::default());
        }
        let state: RulesState = serde_json::from_slice(&bytes)
            .map_err(|_| "STORAGE: Rules store contains invalid data")?;
        if state.schema_version != 1
            || state
                .rules
                .values()
                .filter(|rule| rule.status != "deleted")
                .count()
                > MAX_RULES
        {
            return Err("STORAGE: Unsupported rules store schema or size".into());
        }
        for (id, rule) in &state.rules {
            if id != &rule.rule_id
                || rule.revision == 0
                || !matches!(
                    rule.status.as_str(),
                    "draft" | "enabled" | "paused" | "expired" | "deleted"
                )
                || (rule.status == "enabled" && rule.previewed_revision != Some(rule.revision))
                || validate_definition(&rule.definition, None).is_err()
            {
                return Err("STORAGE: Rules store contains an invalid rule".into());
            }
        }
        Ok(state)
    }

    fn save(&self, state: &RulesState) -> Result<(), String> {
        let bytes = serde_json::to_vec(state).map_err(|_| "STORAGE: Cannot encode rules store")?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("STORAGE: Rules store exceeds the storage limit".into());
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|_| "STORAGE: Cannot create rules store directory")?;
        let temporary = parent.join(format!(".rules-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|_| "STORAGE: Cannot create rules store transaction")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "STORAGE: Cannot persist rules store transaction")?;
            drop(file);
            fs::rename(&temporary, &self.path)
                .map_err(|_| "STORAGE: Cannot commit rules store transaction")?;
            #[cfg(unix)]
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| "STORAGE: Cannot synchronize rules store directory")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn load_at(&self, now: u64) -> Result<RulesState, String> {
        let mut state = self.load()?;
        let mut changed = false;
        for rule in state.rules.values_mut() {
            if !matches!(rule.status.as_str(), "expired" | "deleted")
                && rule
                    .definition
                    .expires_at
                    .is_some_and(|expiry| expiry <= now)
            {
                rule.status = "expired".into();
                rule.next_run_at = None;
                rule.updated_at = now;
                changed = true;
            }
        }
        if changed {
            self.save(&state)?;
        }
        Ok(state)
    }

    pub fn list(&self, now: u64) -> Result<Vec<RuleRecord>, String> {
        let _guard = self.acquire()?;
        let mut rules: Vec<_> = self.load_at(now)?.rules.into_values().collect();
        rules.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.rule_id.cmp(&right.rule_id))
        });
        Ok(rules)
    }

    pub fn get(&self, id: &str, now: u64) -> Result<RuleRecord, String> {
        let _guard = self.acquire()?;
        self.load_at(now)?
            .rules
            .get(id)
            .cloned()
            .ok_or_else(not_found)
    }

    pub fn create(&self, definition: RuleDefinition, now: u64) -> Result<RuleRecord, String> {
        validate_definition(&definition, Some(now))?;
        let _guard = self.acquire()?;
        let mut state = self.load_at(now)?;
        if state
            .rules
            .values()
            .filter(|rule| rule.status != "deleted")
            .count()
            >= MAX_RULES
        {
            return Err("VALIDATION: A rules store supports at most 100 rules".into());
        }
        let record = RuleRecord {
            rule_id: format!("rule_{}", Uuid::new_v4().simple()),
            revision: 1,
            previewed_revision: None,
            status: "draft".into(),
            definition,
            created_at: now,
            updated_at: now,
            next_run_at: None,
            activation_id: None,
        };
        state.rules.insert(record.rule_id.clone(), record.clone());
        self.save(&state)?;
        Ok(record)
    }

    pub fn update(
        &self,
        id: &str,
        revision: u64,
        definition: RuleDefinition,
        now: u64,
    ) -> Result<RuleRecord, String> {
        validate_definition(&definition, Some(now))?;
        let _guard = self.acquire()?;
        let mut state = self.load_at(now)?;
        let rule = state.rules.get_mut(id).ok_or_else(not_found)?;
        check_revision(rule, revision)?;
        not_deleted(rule)?;
        rule.revision = rule
            .revision
            .checked_add(1)
            .ok_or("CONFLICT: Rule revision limit reached")?;
        rule.previewed_revision = None;
        rule.status = "draft".into();
        rule.definition = definition;
        rule.updated_at = now;
        rule.next_run_at = None;
        rule.activation_id = None;
        let result = rule.clone();
        self.save(&state)?;
        Ok(result)
    }

    pub fn preview(
        &self,
        id: &str,
        revision: u64,
        context: &BTreeMap<String, String>,
        now: u64,
    ) -> Result<RulePreview, String> {
        let _guard = self.acquire()?;
        let mut state = self.load_at(now)?;
        let rule = state.rules.get_mut(id).ok_or_else(not_found)?;
        check_revision(rule, revision)?;
        not_deleted(rule)?;
        validate_definition(&rule.definition, Some(now))?;
        let mut warnings = Vec::new();
        if rule.definition.conditions.items.iter().any(|condition| {
            context
                .get(&condition.entity_id)
                .is_none_or(|value| unavailable(value))
        }) {
            warnings.push(
                "Some condition entities are unavailable; their conditions do not match".into(),
            );
        }
        if rule
            .definition
            .actions
            .iter()
            .any(|action| matches!(action, RuleAction::HomeAssistant { .. }))
        {
            warnings.push(
                "Home Assistant actions require a configured connection and available entities"
                    .into(),
            );
        }
        let result = RulePreview {
            rule_id: rule.rule_id.clone(),
            revision,
            conditions_matched: evaluate_conditions(&rule.definition.conditions, context),
            actions: rule.definition.actions.clone(),
            warnings,
        };
        rule.previewed_revision = Some(revision);
        rule.updated_at = now;
        self.save(&state)?;
        Ok(result)
    }

    pub fn set_status(
        &self,
        id: &str,
        revision: u64,
        status: &str,
        now: u64,
    ) -> Result<RuleRecord, String> {
        if !matches!(status, "draft" | "enabled" | "paused" | "deleted") {
            return Err("VALIDATION: Requested rule status is not supported".into());
        }
        let _guard = self.acquire()?;
        let mut state = self.load_at(now)?;
        let rule = state.rules.get_mut(id).ok_or_else(not_found)?;
        check_revision(rule, revision)?;
        if status == "deleted" && rule.status == "deleted" {
            return Ok(rule.clone());
        }
        not_deleted(rule)?;
        match status {
            "enabled" => {
                if rule.status == "expired"
                    || rule
                        .definition
                        .expires_at
                        .is_some_and(|expiry| expiry <= now)
                {
                    return Err("CONFLICT: An expired rule must be edited before enabling".into());
                }
                if rule.previewed_revision != Some(revision) {
                    return Err(
                        "CONFLICT: Preview the current rule revision before enabling".into(),
                    );
                }
                validate_definition(&rule.definition, Some(now))?;
                if rule.status != "enabled" {
                    rule.activation_id = Some(Uuid::new_v4().to_string());
                    rule.next_run_at = match rule.definition.trigger {
                        RuleTrigger::Schedule { interval_seconds } => {
                            Some(next_interval(now, interval_seconds)?)
                        }
                        _ => None,
                    };
                }
            }
            "paused" if !matches!(rule.status.as_str(), "enabled" | "paused") => {
                return Err("CONFLICT: Only an enabled rule can be paused".into());
            }
            "draft" => rule.previewed_revision = None,
            _ => {}
        }
        if status != "enabled" {
            rule.next_run_at = None;
        }
        rule.status = status.into();
        rule.updated_at = now;
        let result = rule.clone();
        self.save(&state)?;
        Ok(result)
    }

    pub fn history_snapshot(&self) -> Result<Vec<RuleRun>, String> {
        let _guard = self.acquire()?;
        Ok(self.load()?.runs)
    }

    pub fn history(&self, id: &str) -> Result<Vec<RuleRun>, String> {
        let _guard = self.acquire()?;
        let state = self.load()?;
        if !state.rules.contains_key(id) {
            return Err(not_found());
        }
        Ok(state
            .runs
            .into_iter()
            .rev()
            .filter(|run| run.rule_id == id)
            .collect())
    }

    pub fn due(&self, now: u64) -> Result<Vec<RuleRecord>, String> {
        let _guard = self.acquire()?;
        let mut due: Vec<_> = self
            .load_at(now)?
            .rules
            .into_values()
            .filter(|rule| {
                rule.status == "enabled"
                    && matches!(rule.definition.trigger, RuleTrigger::Schedule { .. })
                    && rule.next_run_at.is_some_and(|time| time <= now)
            })
            .collect();
        due.sort_by_key(|rule| rule.next_run_at);
        Ok(due)
    }

    pub fn run<F>(
        &self,
        id: &str,
        revision: u64,
        trigger_id: &str,
        trigger_kind: &str,
        context: &BTreeMap<String, String>,
        now: u64,
        executor: F,
    ) -> Result<RuleRun, String>
    where
        F: FnMut(&RuleAction) -> Result<String, String>,
    {
        self.run_for_activation(
            id,
            revision,
            trigger_id,
            trigger_kind,
            context,
            now,
            None,
            executor,
        )
    }

    pub fn run_for_activation<F>(
        &self,
        id: &str,
        revision: u64,
        trigger_id: &str,
        trigger_kind: &str,
        context: &BTreeMap<String, String>,
        now: u64,
        expected_activation: Option<&str>,
        mut executor: F,
    ) -> Result<RuleRun, String>
    where
        F: FnMut(&RuleAction) -> Result<String, String>,
    {
        bounded_text(trigger_id, "Trigger id", 256)?;
        let _guard = self.acquire()?;
        let mut state = self.load_at(now)?;
        if let Some(previous) = state
            .runs
            .iter()
            .find(|run| run.rule_id == id && run.trigger_id == trigger_id)
        {
            if previous.revision != revision || previous.trigger_kind != trigger_kind {
                return Err("CONFLICT: Trigger id was already used for another rule revision or trigger kind".into());
            }
            return Ok(previous.clone());
        }
        let rule = state.rules.get_mut(id).ok_or_else(not_found)?;
        check_revision(rule, revision)?;
        if expected_activation
            .is_some_and(|expected| rule.activation_id.as_deref() != Some(expected))
        {
            return Err("CONFLICT: Rule activation changed before trigger execution".into());
        }
        if rule.status != "enabled" {
            return Err("CONFLICT: Only an enabled rule can execute".into());
        }
        if rule.definition.trigger.kind() != trigger_kind {
            return Err("VALIDATION: Trigger kind does not match the rule".into());
        }
        ensure_run_capacity(&state, MAX_RUN_HISTORY_BYTES)?;
        let rule = state.rules.get_mut(id).ok_or_else(not_found)?;
        if let RuleTrigger::Schedule { interval_seconds } = rule.definition.trigger {
            if rule.next_run_at.is_none_or(|time| time > now) {
                return Err("CONFLICT: Scheduled rule is not due".into());
            }
            // Skip missed intervals instead of replaying an unbounded backlog after downtime.
            rule.next_run_at = Some(next_interval(now, interval_seconds)?);
            rule.updated_at = now;
        }
        let definition = rule.definition.clone();
        let conditions_matched = evaluate_conditions(&definition.conditions, context);
        let run = RuleRun {
            run_id: format!("rule_run_{}", Uuid::new_v4().simple()),
            rule_id: id.into(),
            revision,
            trigger_id: trigger_id.into(),
            trigger_kind: trigger_kind.into(),
            status: if conditions_matched {
                "unknown"
            } else {
                "skipped"
            }
            .into(),
            reason: if conditions_matched {
                "Execution started; unfinished actions are not replayed after interruption"
            } else {
                "Rule conditions did not match"
            }
            .into(),
            started_at: now,
            ended_at: if conditions_matched { None } else { Some(now) },
            conditions_matched,
            actions: definition
                .actions
                .iter()
                .enumerate()
                .map(|(index, _)| RuleActionResult {
                    index,
                    status: "skipped".into(),
                    message: if conditions_matched {
                        "Action has not started"
                    } else {
                        "Rule conditions did not match"
                    }
                    .into(),
                })
                .collect(),
        };
        let run_index = state.runs.len();
        state.runs.push(run);
        self.save(&state)?;
        if !conditions_matched {
            return Ok(state.runs[run_index].clone());
        }
        for (index, action) in definition.actions.iter().enumerate() {
            // Persist uncertainty before invoking an adapter so a crash cannot cause an automatic retry.
            state.runs[run_index].actions[index].status = "unknown".into();
            state.runs[run_index].actions[index].message =
                "Action started; outcome may be unknown after interruption".into();
            self.save(&state)?;
            let outcome = match action {
                RuleAction::Record { message } => Ok(message.clone()),
                RuleAction::HomeAssistant { .. } => executor(action),
            };
            let result = &mut state.runs[run_index].actions[index];
            match outcome {
                Ok(message) => {
                    result.status = "succeeded".into();
                    result.message = bounded_result(&message);
                }
                Err(message) => {
                    result.status = if message.starts_with("UNKNOWN:") {
                        "unknown"
                    } else {
                        "failed"
                    }
                    .into();
                    result.message =
                        bounded_result(message.strip_prefix("UNKNOWN:").unwrap_or(&message).trim());
                }
            }
            self.save(&state)?;
        }
        let run = &mut state.runs[run_index];
        let succeeded = run
            .actions
            .iter()
            .filter(|action| action.status == "succeeded")
            .count();
        let unknown = run.actions.iter().any(|action| action.status == "unknown");
        let (status, reason) = if succeeded == run.actions.len() {
            ("completed", "All actions completed")
        } else if succeeded > 0 {
            ("partial", "Some actions did not complete successfully")
        } else if unknown {
            (
                "unknown",
                "One or more action outcomes could not be confirmed",
            )
        } else {
            ("failed", "All actions failed")
        };
        run.status = status.into();
        run.reason = reason.into();
        run.ended_at = Some(now);
        let result = run.clone();
        self.save(&state)?;
        Ok(result)
    }
}

fn not_found() -> String {
    "NOT_FOUND: Rule does not exist".into()
}

fn check_revision(rule: &RuleRecord, revision: u64) -> Result<(), String> {
    if rule.revision != revision {
        Err("CONFLICT: Rule revision changed; reload before continuing".into())
    } else {
        Ok(())
    }
}

fn not_deleted(rule: &RuleRecord) -> Result<(), String> {
    if rule.status == "deleted" {
        Err("CONFLICT: A deleted rule cannot be changed".into())
    } else {
        Ok(())
    }
}

fn next_interval(now: u64, interval: u64) -> Result<u64, String> {
    now.checked_add(interval)
        .ok_or_else(|| "VALIDATION: Scheduled time is out of range".into())
}

fn bounded_text(value: &str, label: &str, maximum: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(format!(
            "VALIDATION: {label} must contain 1 to {maximum} bytes without control characters"
        ));
    }
    Ok(())
}

fn validate_entity(entity_id: &str) -> Result<(), String> {
    bounded_text(entity_id, "Entity id", 128)?;
    let Some((domain, object_id)) = entity_id.split_once('.') else {
        return Err("VALIDATION: Entity id must have domain.object_id format".into());
    };
    if domain.is_empty()
        || object_id.is_empty()
        || !domain
            .chars()
            .chain(object_id.chars())
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err("VALIDATION: Entity id must have domain.object_id format".into());
    }
    Ok(())
}

fn validate_definition(definition: &RuleDefinition, now: Option<u64>) -> Result<(), String> {
    bounded_text(&definition.name, "Rule name", 128)?;
    if now.is_some_and(|now| definition.expires_at.is_some_and(|expiry| expiry <= now)) {
        return Err("VALIDATION: Rule expiry must be in the future".into());
    }
    match &definition.trigger {
        RuleTrigger::Manual => {}
        RuleTrigger::Event { event_type } => {
            bounded_text(event_type, "Event type", 128)?;
            if !event_type
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            {
                return Err("VALIDATION: Event type contains unsupported characters".into());
            }
        }
        RuleTrigger::State { entity_id, to } => {
            validate_entity(entity_id)?;
            bounded_text(to, "Target state", 256)?;
            if unavailable(to) {
                return Err(
                    "VALIDATION: A state trigger cannot target an unavailable state".into(),
                );
            }
        }
        RuleTrigger::Schedule { interval_seconds } => {
            if !(10..=31_536_000).contains(interval_seconds) {
                return Err(
                    "VALIDATION: Schedule interval must be between 10 and 31536000 seconds".into(),
                );
            }
        }
    }
    if !matches!(definition.conditions.match_mode.as_str(), "all" | "any")
        || definition.conditions.items.len() > MAX_CONDITIONS
    {
        return Err("VALIDATION: Conditions require all/any mode and at most 20 items".into());
    }
    for condition in &definition.conditions.items {
        validate_entity(&condition.entity_id)?;
        bounded_text(&condition.value, "Condition value", 256)?;
        if !matches!(
            condition.operator.as_str(),
            "eq" | "ne" | "gt" | "gte" | "lt" | "lte"
        ) {
            return Err("VALIDATION: Unsupported condition operator".into());
        }
        if matches!(condition.operator.as_str(), "gt" | "gte" | "lt" | "lte")
            && finite_number(&condition.value).is_none()
        {
            return Err("VALIDATION: Numeric comparison requires a finite number".into());
        }
    }
    if definition.actions.is_empty() || definition.actions.len() > MAX_ACTIONS {
        return Err("VALIDATION: A rule requires between 1 and 20 actions".into());
    }
    for action in &definition.actions {
        match action {
            RuleAction::Record { message } => bounded_text(message, "Record message", 1024)?,
            RuleAction::HomeAssistant {
                entity_id,
                domain,
                service,
                fields,
            } => {
                validate_entity(entity_id)?;
                if !entity_id.starts_with(&format!("{domain}."))
                    || service == "toggle"
                    || !home_assistant_service_action_is_allowlisted(domain, service)
                    || !matches!(service.as_str(), "turn_on" | "turn_off")
                {
                    return Err("VALIDATION: Only explicit low-risk Home Assistant on/off actions are allowed".into());
                }
                validate_home_assistant_service_fields(fields)
                    .map_err(|message| format!("VALIDATION: {message}"))?;
                if !fields.is_object() || fields.to_string().len() > 8192 {
                    return Err(
                        "VALIDATION: Home Assistant fields must be an object of at most 8192 bytes"
                            .into(),
                    );
                }
                if fields.as_object().unwrap().keys().any(|key| {
                    matches!(
                        key.as_str(),
                        "entity_id" | "device_id" | "area_id" | "target" | "floor_id" | "label_id"
                    )
                }) {
                    return Err(
                        "VALIDATION: Home Assistant fields cannot override the action target"
                            .into(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn unavailable(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value.eq_ignore_ascii_case("unknown")
        || value.eq_ignore_ascii_case("unavailable")
}

fn finite_number(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn condition_matches(condition: &RuleCondition, context: &BTreeMap<String, String>) -> bool {
    let Some(actual) = context
        .get(&condition.entity_id)
        .filter(|value| !unavailable(value))
    else {
        return false;
    };
    match condition.operator.as_str() {
        "eq" => actual == &condition.value,
        "ne" => actual != &condition.value,
        operator => {
            let (Some(actual), Some(expected)) =
                (finite_number(actual), finite_number(&condition.value))
            else {
                return false;
            };
            match operator {
                "gt" => actual > expected,
                "gte" => actual >= expected,
                "lt" => actual < expected,
                "lte" => actual <= expected,
                _ => false,
            }
        }
    }
}

pub fn evaluate_conditions(
    conditions: &RuleConditions,
    context: &BTreeMap<String, String>,
) -> bool {
    if !matches!(conditions.match_mode.as_str(), "all" | "any") {
        return false;
    }
    if conditions.items.is_empty() {
        return true;
    }
    match conditions.match_mode.as_str() {
        "any" => conditions
            .items
            .iter()
            .any(|condition| condition_matches(condition, context)),
        _ => conditions
            .items
            .iter()
            .all(|condition| condition_matches(condition, context)),
    }
}

fn bounded_result(message: &str) -> String {
    message
        .chars()
        .filter(|ch| !ch.is_control())
        .take(1024)
        .collect()
}

fn ensure_run_capacity(state: &RulesState, limit: usize) -> Result<(), String> {
    let size = serde_json::to_vec(state)
        .map_err(|_| "STORAGE: Cannot encode rules store")?
        .len();
    if size >= limit {
        return Err(
            "STORAGE: Run history capacity reached; existing rules remain manageable".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestStore {
        directory: PathBuf,
        store: RulesStore,
    }

    impl TestStore {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!("harbor-rules-{}", Uuid::new_v4()));
            Self {
                store: RulesStore::new(directory.join("rules.json")),
                directory,
            }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            if self.directory.is_dir() {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }
    }

    fn definition() -> RuleDefinition {
        RuleDefinition {
            name: "Local check".into(),
            trigger: RuleTrigger::Manual,
            conditions: RuleConditions {
                match_mode: "all".into(),
                items: Vec::new(),
            },
            actions: vec![RuleAction::Record {
                message: "Condition matched".into(),
            }],
            expires_at: None,
        }
    }

    fn enable(store: &RulesStore, definition: RuleDefinition, now: u64) -> RuleRecord {
        let rule = store.create(definition, now).unwrap();
        store
            .preview(&rule.rule_id, rule.revision, &BTreeMap::new(), now)
            .unwrap();
        store
            .set_status(&rule.rule_id, rule.revision, "enabled", now)
            .unwrap()
    }

    #[test]
    fn rule_lifecycle_requires_preview_and_reconfirmation() {
        let test = TestStore::new();
        assert!(!test.directory.exists());
        assert!(test.store.list(100).unwrap().is_empty());
        let rule = test.store.create(definition(), 100).unwrap();
        assert_eq!(rule.status, "draft");
        assert!(test
            .store
            .set_status(&rule.rule_id, 1, "enabled", 101)
            .unwrap_err()
            .starts_with("CONFLICT:"));
        let preview = test
            .store
            .preview(&rule.rule_id, 1, &BTreeMap::new(), 101)
            .unwrap();
        assert!(preview.conditions_matched);
        test.store
            .set_status(&rule.rule_id, 1, "enabled", 102)
            .unwrap();
        test.store
            .set_status(&rule.rule_id, 1, "paused", 103)
            .unwrap();
        test.store
            .set_status(&rule.rule_id, 1, "enabled", 104)
            .unwrap();
        let mut changed = definition();
        changed.actions = vec![RuleAction::Record {
            message: "Changed".into(),
        }];
        let updated = test.store.update(&rule.rule_id, 1, changed, 105).unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.status, "draft");
        assert_eq!(updated.previewed_revision, None);
        assert!(test
            .store
            .update(&rule.rule_id, 1, definition(), 106)
            .unwrap_err()
            .starts_with("CONFLICT:"));
        assert!(test
            .store
            .set_status(&rule.rule_id, 2, "enabled", 106)
            .is_err());
        let reopened = RulesStore::new(test.directory.join("rules.json"));
        assert_eq!(reopened.get(&rule.rule_id, 106).unwrap(), updated);
    }

    #[test]
    fn conditions_handle_combinations_numbers_and_unavailable_without_guessing() {
        let condition = RuleCondition {
            entity_id: "sensor.temperature".into(),
            operator: "gt".into(),
            value: "20".into(),
        };
        let mut conditions = RuleConditions {
            match_mode: "all".into(),
            items: vec![condition.clone()],
        };
        let mut context = BTreeMap::from([("sensor.temperature".into(), "21.5".into())]);
        assert!(evaluate_conditions(&conditions, &context));
        for value in ["unknown", "unavailable", "NaN", "infinity", ""] {
            context.insert("sensor.temperature".into(), value.into());
            assert!(!evaluate_conditions(&conditions, &context));
        }
        conditions.items[0].operator = "ne".into();
        context.clear();
        assert!(!evaluate_conditions(&conditions, &context));
        conditions.items.push(RuleCondition {
            entity_id: "switch.test".into(),
            operator: "eq".into(),
            value: "on".into(),
        });
        context.insert("switch.test".into(), "on".into());
        assert!(!evaluate_conditions(&conditions, &context));
        conditions.match_mode = "any".into();
        assert!(evaluate_conditions(&conditions, &context));
    }

    #[test]
    fn record_runs_are_real_persisted_history_and_replay_does_not_execute_again() {
        let test = TestStore::new();
        let rule = enable(&test.store, definition(), 100);
        let run = test
            .store
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| panic!("Record must not call HA"),
            )
            .unwrap();
        assert_eq!(run.status, "completed");
        assert_eq!(run.actions[0].status, "succeeded");
        assert_eq!(run.actions[0].message, "Condition matched");
        let reopened = RulesStore::new(test.directory.join("rules.json"));
        let replay = reopened
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                102,
                |_| panic!("No replay"),
            )
            .unwrap();
        assert_eq!(run, replay);
        assert_eq!(reopened.history(&rule.rule_id).unwrap(), vec![run]);
        test.store
            .update(&rule.rule_id, 1, definition(), 103)
            .unwrap();
        assert!(test
            .store
            .run(
                &rule.rule_id,
                2,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                104,
                |_| Ok("never".into())
            )
            .unwrap_err()
            .starts_with("CONFLICT:"));
        test.store
            .set_status(&rule.rule_id, 2, "deleted", 104)
            .unwrap();
        assert_eq!(test.store.history(&rule.rule_id).unwrap().len(), 1);
        assert!(test
            .store
            .preview(&rule.rule_id, 2, &BTreeMap::new(), 105)
            .is_err());
    }

    #[test]
    fn partial_failure_preserves_individual_results_and_success_is_not_replayed() {
        let test = TestStore::new();
        let mut def = definition();
        def.actions.push(RuleAction::HomeAssistant {
            entity_id: "light.test".into(),
            domain: "light".into(),
            service: "turn_on".into(),
            fields: json!({}),
        });
        def.actions.push(RuleAction::Record {
            message: "After failure".into(),
        });
        let rule = enable(&test.store, def, 100);
        let mut calls = 0;
        let run = test
            .store
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| {
                    calls += 1;
                    Err("Home Assistant unavailable".into())
                },
            )
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(run.status, "partial");
        assert_eq!(
            run.actions
                .iter()
                .map(|a| a.status.as_str())
                .collect::<Vec<_>>(),
            vec!["succeeded", "failed", "succeeded"]
        );
        test.store
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                102,
                |_| {
                    calls += 1;
                    Ok("bad replay".into())
                },
            )
            .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn disabled_and_unmatched_rules_do_not_call_executor() {
        let test = TestStore::new();
        let mut def = definition();
        def.conditions.items.push(RuleCondition {
            entity_id: "switch.test".into(),
            operator: "eq".into(),
            value: "on".into(),
        });
        let rule = enable(&test.store, def, 100);
        let run = test
            .store
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| panic!("Unmatched"),
            )
            .unwrap();
        assert_eq!(run.status, "skipped");
        assert!(!run.conditions_matched);
        assert_eq!(run.actions[0].status, "skipped");
        test.store
            .set_status(&rule.rule_id, 1, "paused", 102)
            .unwrap();
        assert!(test
            .store
            .run(
                &rule.rule_id,
                1,
                "manual-2",
                "manual",
                &BTreeMap::new(),
                103,
                |_| panic!("Paused")
            )
            .is_err());
    }

    #[test]
    fn schedule_misses_only_one_run_and_expiry_is_persisted() {
        let test = TestStore::new();
        let mut def = definition();
        def.trigger = RuleTrigger::Schedule {
            interval_seconds: 10,
        };
        def.expires_at = Some(1000);
        let rule = enable(&test.store, def, 100);
        assert_eq!(rule.next_run_at, Some(110));
        assert!(test.store.due(109).unwrap().is_empty());
        assert_eq!(test.store.due(500).unwrap().len(), 1);
        test.store
            .run(
                &rule.rule_id,
                1,
                "schedule-110",
                "schedule",
                &BTreeMap::new(),
                500,
                |_| panic!("Record"),
            )
            .unwrap();
        assert!(test.store.due(500).unwrap().is_empty());
        assert_eq!(
            test.store.get(&rule.rule_id, 501).unwrap().next_run_at,
            Some(510)
        );
        assert!(test.store.due(1000).unwrap().is_empty());
        assert_eq!(
            test.store.get(&rule.rule_id, 1000).unwrap().status,
            "expired"
        );
        assert!(test
            .store
            .set_status(&rule.rule_id, 1, "enabled", 1000)
            .is_err());
    }

    #[test]
    fn invalid_and_unknown_schema_fields_are_rejected() {
        let mut value = serde_json::to_value(definition()).unwrap();
        value["run_without_confirmation"] = json!(true);
        assert!(serde_json::from_value::<RuleDefinition>(value).is_err());
        assert!(serde_json::from_value::<RuleTrigger>(
            json!({"kind":"manual", "command":"unsafe"})
        )
        .is_err());
        let test = TestStore::new();
        let mut def = definition();
        def.trigger = RuleTrigger::Schedule {
            interval_seconds: 9,
        };
        assert!(test
            .store
            .create(def, 100)
            .unwrap_err()
            .starts_with("VALIDATION:"));
        let mut def = definition();
        def.expires_at = Some(100);
        assert!(test.store.create(def, 100).is_err());
        for (domain, service, fields) in [
            ("lock", "unlock", json!({})),
            ("light", "toggle", json!({})),
            ("light", "turn_on", json!({"device_id":"another-device"})),
        ] {
            let mut def = definition();
            def.actions = vec![RuleAction::HomeAssistant {
                entity_id: format!("{domain}.test"),
                domain: domain.into(),
                service: service.into(),
                fields,
            }];
            assert!(test.store.create(def, 100).is_err());
        }
        assert!(test.store.list(100).unwrap().is_empty());
    }

    #[test]
    fn storage_failure_never_commits_a_creation_or_runs_an_action() {
        let test = TestStore::new();
        fs::create_dir_all(&test.directory).unwrap();
        fs::write(test.directory.join("parent-file"), b"not a directory").unwrap();
        let broken = RulesStore::new(test.directory.join("parent-file/rules.json"));
        assert!(broken
            .create(definition(), 100)
            .unwrap_err()
            .starts_with("STORAGE:"));
        assert!(!test.directory.join("parent-file/rules.json").exists());
    }

    #[test]
    fn unfinished_execution_survives_restart_without_replay() {
        let test = TestStore::new();
        let rule = enable(&test.store, definition(), 100);
        let run = test
            .store
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| panic!("Record"),
            )
            .unwrap();
        let mut state: RulesState =
            serde_json::from_slice(&fs::read(test.directory.join("rules.json")).unwrap()).unwrap();
        state.runs[0].status = "unknown".into();
        state.runs[0].ended_at = None;
        state.runs[0].actions[0].status = "unknown".into();
        fs::write(
            test.directory.join("rules.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        let reopened = RulesStore::new(test.directory.join("rules.json"));
        let replay = reopened
            .run(
                &rule.rule_id,
                1,
                "manual-1",
                "manual",
                &BTreeMap::new(),
                102,
                |_| panic!("Interrupted actions must not replay"),
            )
            .unwrap();
        assert_eq!(replay.run_id, run.run_id);
        assert_eq!(replay.status, "unknown");
        assert_eq!(replay.actions[0].status, "unknown");
    }

    #[test]
    fn clones_serialize_replay_without_duplicate_effects() {
        let test = TestStore::new();
        let mut def = definition();
        def.actions = vec![RuleAction::HomeAssistant {
            entity_id: "switch.test".into(),
            domain: "switch".into(),
            service: "turn_on".into(),
            fields: json!({}),
        }];
        let rule = enable(&test.store, def, 100);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let store = test.store.clone();
            let rule_id = rule.rule_id.clone();
            let calls = calls.clone();
            threads.push(std::thread::spawn(move || {
                store
                    .run(
                        &rule_id,
                        1,
                        "same-trigger",
                        "manual",
                        &BTreeMap::new(),
                        101,
                        |_| {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Ok("Executed".into())
                        },
                    )
                    .unwrap()
            }));
        }
        let runs: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(runs.iter().all(|run| run.run_id == runs[0].run_id));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn unknown_adapter_outcomes_are_not_reported_as_success_or_retried() {
        let test = TestStore::new();
        let mut def = definition();
        def.actions = vec![RuleAction::HomeAssistant {
            entity_id: "light.test".into(),
            domain: "light".into(),
            service: "turn_on".into(),
            fields: json!({}),
        }];
        let rule = enable(&test.store, def, 100);
        let run = test
            .store
            .run(
                &rule.rule_id,
                1,
                "uncertain-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| Err("UNKNOWN: Device response timed out".into()),
            )
            .unwrap();
        assert_eq!(run.status, "unknown");
        assert_eq!(run.actions[0].status, "unknown");
        assert_eq!(run.actions[0].message, "Device response timed out");
        assert_eq!(
            test.store
                .run(
                    &rule.rule_id,
                    1,
                    "uncertain-1",
                    "manual",
                    &BTreeMap::new(),
                    102,
                    |_| panic!("Unknown effects must not replay")
                )
                .unwrap(),
            run
        );
    }

    #[test]
    fn failed_post_execution_write_keeps_the_persisted_unknown_outcome() {
        let test = TestStore::new();
        let mut def = definition();
        def.actions = vec![RuleAction::HomeAssistant {
            entity_id: "switch.test".into(),
            domain: "switch".into(),
            service: "turn_off".into(),
            fields: json!({}),
        }];
        let rule = enable(&test.store, def, 100);
        let path = test.directory.join("rules.json");
        let retained = test.directory.join("retained.json");
        let error = test
            .store
            .run(
                &rule.rule_id,
                1,
                "interrupted-1",
                "manual",
                &BTreeMap::new(),
                101,
                |_| {
                    fs::rename(&path, &retained).unwrap();
                    fs::create_dir(&path).unwrap();
                    Ok("Device switched off".into())
                },
            )
            .unwrap_err();
        assert!(error.starts_with("STORAGE:"));
        fs::remove_dir(&path).unwrap();
        fs::rename(&retained, &path).unwrap();
        let reopened = RulesStore::new(path);
        let run = reopened
            .run(
                &rule.rule_id,
                1,
                "interrupted-1",
                "manual",
                &BTreeMap::new(),
                102,
                |_| panic!("Do not replay after persistence failure"),
            )
            .unwrap();
        assert_eq!(run.status, "unknown");
        assert_eq!(run.actions[0].status, "unknown");
        assert_eq!(run.ended_at, None);
    }

    #[test]
    fn deleted_rules_release_active_capacity_without_losing_tombstones() {
        let test = TestStore::new();
        let mut state = RulesState::default();
        for index in 0..MAX_RULES {
            let rule = RuleRecord {
                rule_id: format!("rule_{index}"),
                revision: 1,
                previewed_revision: None,
                status: "draft".into(),
                definition: definition(),
                created_at: 100,
                updated_at: 100,
                next_run_at: None,
                activation_id: None,
            };
            state.rules.insert(rule.rule_id.clone(), rule);
        }
        test.store.save(&state).unwrap();
        assert!(test
            .store
            .create(definition(), 101)
            .unwrap_err()
            .starts_with("VALIDATION:"));
        test.store.set_status("rule_0", 1, "deleted", 101).unwrap();
        test.store.create(definition(), 102).unwrap();
        assert_eq!(test.store.list(102).unwrap().len(), MAX_RULES + 1);
        assert_eq!(test.store.get("rule_0", 102).unwrap().status, "deleted");
    }

    #[test]
    fn run_capacity_guard_does_not_block_existing_rule_management() {
        let test = TestStore::new();
        let rule = enable(&test.store, definition(), 100);
        let state = test.store.load().unwrap();
        assert!(ensure_run_capacity(&state, 1)
            .unwrap_err()
            .contains("existing rules remain manageable"));
        test.store
            .set_status(&rule.rule_id, 1, "paused", 101)
            .unwrap();
        test.store
            .set_status(&rule.rule_id, 1, "deleted", 102)
            .unwrap();
        assert_eq!(
            test.store.get(&rule.rule_id, 102).unwrap().status,
            "deleted"
        );
    }

    #[test]
    fn event_and_state_matching_are_exact_and_trigger_kind_cannot_be_replaced() {
        assert!(RuleTrigger::Event {
            event_type: "package.appeared".into()
        }
        .matches_event("package.appeared"));
        assert!(!RuleTrigger::Event {
            event_type: "package.appeared".into()
        }
        .matches_event("other.event"));
        let trigger = RuleTrigger::State {
            entity_id: "sensor.test".into(),
            to: "on".into(),
        };
        assert!(trigger.matches_state("sensor.test", "on"));
        assert!(!trigger.matches_state("sensor.other", "on"));
        assert!(!trigger.matches_state("sensor.test", "unavailable"));
        let test = TestStore::new();
        let rule = enable(&test.store, definition(), 100);
        assert!(test
            .store
            .run(
                &rule.rule_id,
                1,
                "wrong-kind",
                "event",
                &BTreeMap::new(),
                101,
                |_| panic!("Wrong kind")
            )
            .unwrap_err()
            .starts_with("VALIDATION:"));
    }

    #[test]
    fn validation_covers_action_condition_and_text_limits() {
        let test = TestStore::new();
        let mut too_many_actions = definition();
        too_many_actions.actions = vec![
            RuleAction::Record {
                message: "item".into()
            };
            MAX_ACTIONS + 1
        ];
        assert!(test.store.create(too_many_actions, 100).is_err());
        let mut too_many_conditions = definition();
        too_many_conditions.conditions.items = vec![
            RuleCondition {
                entity_id: "sensor.test".into(),
                operator: "eq".into(),
                value: "on".into()
            };
            MAX_CONDITIONS + 1
        ];
        assert!(test.store.create(too_many_conditions, 100).is_err());
        let mut invalid_name = definition();
        invalid_name.name = "x".repeat(129);
        assert!(test.store.create(invalid_name, 100).is_err());
        let mut value = serde_json::to_value(definition()).unwrap();
        value["conditions"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<RuleDefinition>(value).is_err());
        assert!(serde_json::from_value::<RuleAction>(
            json!({"kind":"record", "message":"test", "command":"unsafe"})
        )
        .is_err());
    }
}
