//! Shadow workflow compiler for stable HarborBeacon procedures.
//!
//! The compiler turns a frozen workflow spec into an internal candidate only.
//! It never executes tools and never bypasses the existing policy/router/audit
//! path.

use std::collections::HashSet;

use crate::orchestrator::contracts::{Action, RiskLevel};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SYSTEM_DIAGNOSTICS_WORKFLOW_ID: &str = "system_diagnostics_v1";
pub const WORKFLOW_COMPILER_MIN_CONFIDENCE: u8 = 60;

const SYSTEM_DIAGNOSTICS_WORKFLOW_SPEC_JSON: &str = r#"
{
  "id": "system_diagnostics_v1",
  "version": "1.0.0",
  "summary": "Shadow workflow for HarborBeacon system diagnostics and readiness routing.",
  "min_confidence": 60,
  "deny_phrases": [
    "重启", "启动", "停止", "启动服务", "停止服务", "删除", "移动文件", "复制文件",
    "写入", "连接摄像头", "接入摄像头", "开灯", "关灯",
    "天气", "新闻", "股票", "行情", "门锁", "开锁", "解锁",
    "restart", "stop service", "start service", "delete", "remove",
    "move file", "copy file", "write file", "connect camera",
    "weather", "news", "stock", "unlock", "lock"
  ],
  "nodes": [
    {
      "id": "evt_preflight",
      "candidate_kind": "evt_preflight",
      "phrases": ["EVT预检", "压测预检", "做一下EVT预检", "运行EVTpreflight", "preflight", "stress preflight", "pre-stress check"],
      "keywords_all": ["预检"],
      "confidence": 96,
      "read_only": true,
      "reason": "system diagnostics workflow matched an EVT preflight request"
    },
    {
      "id": "evt_evidence_bundle",
      "candidate_kind": "evt_evidence_bundle",
      "phrases": ["生成压测证据", "生成EVT证据", "EVT证据包", "压测证据包", "evidence bundle"],
      "keywords_all": ["证据"],
      "confidence": 95,
      "read_only": true,
      "reason": "system diagnostics workflow matched an EVT evidence bundle request"
    },
    {
      "id": "evt_readiness",
      "candidate_kind": "evt_readiness",
      "phrases": ["EVT就绪", "EVT状态", "EVT好了吗", "压测前状态", "压测状态", "压测ready了吗", "K3压测readiness", "K3 readiness", "stress readiness"],
      "keywords_any": ["EVT", "evt", "压测", "stress"],
      "keywords_all": ["状态"],
      "confidence": 94,
      "read_only": true,
      "reason": "system diagnostics workflow matched an EVT readiness request"
    },
    {
      "id": "weixin_status",
      "candidate_kind": "system_readiness",
      "phrases": ["微信状态", "微信通了吗", "微信连了吗", "微信能用吗", "WeChat status", "Weixin status", "weixin connected"],
      "keywords_all": ["微信"],
      "confidence": 91,
      "read_only": true,
      "reason": "system diagnostics workflow matched a Weixin status request"
    },
    {
      "id": "home_assistant_status",
      "candidate_kind": "system_readiness",
      "phrases": ["HA状态", "HA正常吗", "HA实体同步了吗", "Home Assistant状态", "Home Assistant ready", "home assistant health"],
      "keywords_any": ["HA", "homeassistant"],
      "confidence": 90,
      "read_only": true,
      "reason": "system diagnostics workflow matched a Home Assistant status request"
    },
    {
      "id": "semantic_router_status",
      "candidate_kind": "system_readiness",
      "phrases": ["semantic router状态", "semantic-router health", "语义路由状态", "语义路由正常吗", "semantic router ready"],
      "keywords_any": ["semanticrouter", "语义路由"],
      "confidence": 89,
      "read_only": true,
      "reason": "system diagnostics workflow matched a semantic router status request"
    },
    {
      "id": "system_readiness",
      "candidate_kind": "system_readiness",
      "phrases": ["状态", "诊断", "系统状态", "当前状态", "健康状态", "现在K3好了吗", "K3好了没", "Harbor状态", "网关状态", "默认通知目标状态", "readiness", "diagnostics", "health status", "gateway status"],
      "keywords_any": ["状态", "诊断", "健康", "ready", "readiness", "diagnostics", "health", "K3", "k3", "gateway", "网关", "通知目标"],
      "confidence": 84,
      "read_only": true,
      "reason": "system diagnostics workflow matched a general system readiness request"
    }
  ]
}
"#;

const ALLOWED_GENERAL_MESSAGE_CANDIDATES: [&str; 32] = [
    "capability_summary",
    "clarify",
    "conversation_continue",
    "conversation_boundary",
    "conversation_repair",
    "conversation_cancel",
    "conversation_clarify_continue",
    "camera_replay_recent_clip",
    "camera_snapshot",
    "camera_record_clip",
    "knowledge_search",
    "rag_answer",
    "ha_service_action",
    "vision_event_summary",
    "vision_event_notify_latest",
    "vlm_describe_latest_event",
    "vlm_describe_event",
    "family_memory_summary",
    "system_readiness",
    "evt_readiness",
    "evt_preflight",
    "evt_evidence_bundle",
    "family_timeline_summary",
    "family_timeline_query",
    "guardian_rule_proposal",
    "guardian_rule_list",
    "guardian_rule_enable",
    "guardian_rule_pause",
    "guardian_status",
    "family_memory_confirm",
    "family_memory_favorite",
    "family_memory_hide",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub id: String,
    pub version: String,
    pub summary: String,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: u8,
    #[serde(default)]
    pub deny_phrases: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub candidate_kind: String,
    #[serde(default)]
    pub phrases: Vec<String>,
    #[serde(default)]
    pub keywords_any: Vec<String>,
    #[serde(default)]
    pub keywords_all: Vec<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub action: Option<Action>,
    pub confidence: u8,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowCandidate {
    pub workflow_id: String,
    pub node_id: String,
    pub candidate_kind: String,
    pub confidence: u8,
    pub reason: String,
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

impl WorkflowCandidate {
    pub fn has_unsafe_action(&self) -> bool {
        self.action
            .as_ref()
            .is_some_and(|action| !workflow_action_is_read_only(action))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvalCase {
    pub id: String,
    pub input: String,
    #[serde(default)]
    pub expected_candidate_kind: Option<String>,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEvalCaseResult {
    pub id: String,
    pub input: String,
    pub expected_candidate_kind: Option<String>,
    pub candidate_kind: Option<String>,
    pub confidence: Option<u8>,
    pub correct: bool,
    pub low_confidence_rejection: bool,
    pub unsafe_action: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEvalReport {
    pub workflow_id: String,
    pub total_cases: usize,
    pub correct_cases: usize,
    pub accuracy: f64,
    pub low_confidence_rejections: usize,
    pub unauthorized_action_count: usize,
    pub passed: bool,
    pub cases: Vec<WorkflowEvalCaseResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowShadowEvidenceReport {
    pub kind: String,
    pub workflow_id: String,
    pub source: String,
    pub redacted: bool,
    pub total_cases: usize,
    pub matched_current_plan_cases: usize,
    pub low_confidence_rejections: usize,
    pub unauthorized_action_count: usize,
    pub passed: bool,
    pub cases: Vec<WorkflowShadowEvidenceCase>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowShadowEvidenceCase {
    pub case_id: String,
    pub input_sha256_12: String,
    pub input_redacted: bool,
    pub current_plan_kind: Option<String>,
    pub compiler_candidate_kind: Option<String>,
    pub compiler_confidence: Option<u8>,
    pub matched_current_plan: bool,
    pub read_only: bool,
    pub unsafe_action: bool,
    pub low_confidence_rejection: bool,
    pub outcome: String,
}

pub fn parse_workflow_spec(input: &str) -> Result<WorkflowSpec, String> {
    let spec: WorkflowSpec = serde_json::from_str(input)
        .map_err(|error| format!("INVALID_WORKFLOW_SPEC_JSON: {error}"))?;
    validate_workflow_spec(spec)
}

pub fn system_diagnostics_workflow_spec() -> WorkflowSpec {
    parse_workflow_spec(SYSTEM_DIAGNOSTICS_WORKFLOW_SPEC_JSON)
        .expect("embedded system diagnostics workflow spec must be valid")
}

pub fn compile_system_diagnostics_candidate(raw_text: &str) -> Option<WorkflowCandidate> {
    let spec = system_diagnostics_workflow_spec();
    compile_workflow_candidate(&spec, raw_text)
}

pub fn compile_workflow_candidate(
    spec: &WorkflowSpec,
    raw_text: &str,
) -> Option<WorkflowCandidate> {
    let normalized = normalize_workflow_text(raw_text);
    if normalized.is_empty() || deny_phrase_matches(&normalized, &spec.deny_phrases) {
        return None;
    }

    let (node, score) = best_node_match(spec, &normalized)?;
    if score < spec.min_confidence {
        return None;
    }

    Some(WorkflowCandidate {
        workflow_id: spec.id.clone(),
        node_id: node.id.clone(),
        candidate_kind: node.candidate_kind.clone(),
        confidence: score.min(100),
        reason: node.reason.clone(),
        read_only: node.read_only
            && node
                .action
                .as_ref()
                .is_none_or(workflow_action_is_read_only),
        action: node.action.clone(),
    })
}

pub fn system_diagnostics_eval_cases() -> Vec<WorkflowEvalCase> {
    vec![
        eval_case("status", "状态", Some("system_readiness")),
        eval_case("diagnostics", "诊断", Some("system_readiness")),
        eval_case("system_status", "系统状态", Some("system_readiness")),
        eval_case("current_status", "当前状态", Some("system_readiness")),
        eval_case("health_status", "健康状态", Some("system_readiness")),
        eval_case("system_health", "系统健康怎么样", Some("system_readiness")),
        eval_case("k3_ready", "现在 K3 好了吗", Some("system_readiness")),
        eval_case("k3_status", "K3 状态怎么样", Some("system_readiness")),
        eval_case("k3_health", "K3 当前健康状态", Some("system_readiness")),
        eval_case("k3_ok", "K3好了没", Some("system_readiness")),
        eval_case("harbor_status", "Harbor 状态", Some("system_readiness")),
        eval_case(
            "harbor_diagnostics",
            "Harbor 系统诊断",
            Some("system_readiness"),
        ),
        eval_case("gateway_status", "网关状态", Some("system_readiness")),
        eval_case("gateway_ok", "网关通了吗", Some("system_readiness")),
        eval_case(
            "gateway_english",
            "gateway status",
            Some("system_readiness"),
        ),
        eval_case(
            "default_target_status",
            "默认通知目标状态",
            Some("system_readiness"),
        ),
        eval_case("weixin_status", "微信通了吗", Some("system_readiness")),
        eval_case("weixin_state", "微信状态", Some("system_readiness")),
        eval_case("weixin_connected", "微信连了吗", Some("system_readiness")),
        eval_case("weixin_available", "微信能用吗", Some("system_readiness")),
        eval_case("weixin_robot", "微信机器人正常吗", Some("system_readiness")),
        eval_case("wechat_status", "WeChat status", Some("system_readiness")),
        eval_case(
            "weixin_status_en",
            "Weixin status",
            Some("system_readiness"),
        ),
        eval_case(
            "weixin_connected_en",
            "weixin connected?",
            Some("system_readiness"),
        ),
        eval_case("ha_status", "HA 正常吗", Some("system_readiness")),
        eval_case("ha_state", "HA 状态", Some("system_readiness")),
        eval_case(
            "ha_entity_sync",
            "HA 实体同步了吗",
            Some("system_readiness"),
        ),
        eval_case(
            "home_assistant_status",
            "Home Assistant 状态",
            Some("system_readiness"),
        ),
        eval_case(
            "home_assistant_ready",
            "Home Assistant ready?",
            Some("system_readiness"),
        ),
        eval_case(
            "home_assistant_health",
            "home assistant health",
            Some("system_readiness"),
        ),
        eval_case(
            "homeassistant_ready",
            "homeassistant ready",
            Some("system_readiness"),
        ),
        eval_case("ha_short_ready", "HA ready 了吗", Some("system_readiness")),
        eval_case(
            "semantic_router_status",
            "semantic router 状态怎么样",
            Some("system_readiness"),
        ),
        eval_case(
            "semantic_router_ready",
            "semantic router ready",
            Some("system_readiness"),
        ),
        eval_case(
            "semantic_router_health",
            "semantic-router health",
            Some("system_readiness"),
        ),
        eval_case(
            "semantic_router_cn_status",
            "语义路由状态",
            Some("system_readiness"),
        ),
        eval_case(
            "semantic_router_cn_ok",
            "语义路由正常吗",
            Some("system_readiness"),
        ),
        eval_case(
            "semantic_router_policy",
            "semantic router 的本地状态",
            Some("system_readiness"),
        ),
        eval_case("evt_ready", "压测前状态怎么样", Some("evt_readiness")),
        eval_case("evt_status", "EVT 状态", Some("evt_readiness")),
        eval_case("evt_ok", "EVT 好了吗", Some("evt_readiness")),
        eval_case("evt_readiness", "EVT 就绪了吗", Some("evt_readiness")),
        eval_case("stress_status", "压测状态", Some("evt_readiness")),
        eval_case("stress_ready", "压测 ready 了吗", Some("evt_readiness")),
        eval_case(
            "k3_stress_readiness",
            "K3 压测 readiness",
            Some("evt_readiness"),
        ),
        eval_case(
            "stress_readiness_en",
            "stress readiness",
            Some("evt_readiness"),
        ),
        eval_case(
            "evt_preflight",
            "帮我做一下 EVT 预检",
            Some("evt_preflight"),
        ),
        eval_case("stress_preflight", "做一下压测预检", Some("evt_preflight")),
        eval_case(
            "run_evt_preflight",
            "运行 EVT preflight",
            Some("evt_preflight"),
        ),
        eval_case("preflight", "preflight", Some("evt_preflight")),
        eval_case(
            "pre_stress_check",
            "pre-stress check",
            Some("evt_preflight"),
        ),
        eval_case(
            "evt_evidence",
            "生成压测证据包",
            Some("evt_evidence_bundle"),
        ),
        eval_case(
            "evt_evidence_bundle",
            "EVT 证据包",
            Some("evt_evidence_bundle"),
        ),
        eval_case(
            "stress_evidence",
            "生成压测证据",
            Some("evt_evidence_bundle"),
        ),
        eval_case(
            "evt_evidence_cn",
            "生成 EVT 证据",
            Some("evt_evidence_bundle"),
        ),
        eval_case(
            "evidence_bundle_en",
            "evidence bundle",
            Some("evt_evidence_bundle"),
        ),
        eval_case("restart_ssh", "重启 SSH", None),
        eval_case(
            "restart_beacon_service",
            "帮我重启 harboros-beacon.service",
            None,
        ),
        eval_case("start_service", "启动服务", None),
        eval_case("stop_service", "停止 semantic router service", None),
        eval_case("connect_camera", "连接摄像头", None),
        eval_case("add_camera", "接入摄像头", None),
        eval_case("delete_file", "删除这个文件", None),
        eval_case("move_file", "移动这个文件", None),
        eval_case("copy_file", "复制文件", None),
        eval_case("write_config", "写入配置", None),
        eval_case("turn_on_light", "开灯", None),
        eval_case("turn_off_light", "关灯", None),
        eval_case("weather_status", "天气状态怎么样", None),
        eval_case("news", "今天新闻", None),
        eval_case("stock", "股票行情", None),
        eval_case("unlock_door", "解锁门锁", None),
        eval_case("knowledge_search", "查一下樱花计划", None),
        eval_case("camera_snapshot", "看一下门口摄像头", None),
    ]
}

pub fn evaluate_workflow(spec: &WorkflowSpec, cases: &[WorkflowEvalCase]) -> WorkflowEvalReport {
    let mut results = Vec::with_capacity(cases.len());
    let mut correct_cases = 0usize;
    let mut low_confidence_rejections = 0usize;
    let mut unauthorized_action_count = 0usize;

    for case in cases {
        let normalized = normalize_workflow_text(&case.input);
        let best_score = if deny_phrase_matches(&normalized, &spec.deny_phrases) {
            None
        } else {
            best_node_match(spec, &normalized).map(|(_, score)| score)
        };
        let candidate = compile_workflow_candidate(spec, &case.input);
        let unsafe_action = candidate
            .as_ref()
            .is_some_and(WorkflowCandidate::has_unsafe_action);
        let candidate_kind = candidate
            .as_ref()
            .map(|candidate| candidate.candidate_kind.clone());
        let confidence = candidate.as_ref().map(|candidate| candidate.confidence);
        let low_confidence_rejection = case.expected_candidate_kind.is_some()
            && candidate.is_none()
            && best_score.is_some_and(|score| score < spec.min_confidence);
        let correct = match (&case.expected_candidate_kind, &candidate_kind) {
            (Some(expected), Some(actual)) => {
                expected == actual && confidence.unwrap_or(0) >= case.min_confidence
            }
            (None, None) => true,
            _ => false,
        } && !unsafe_action;

        if correct {
            correct_cases += 1;
        }
        if low_confidence_rejection {
            low_confidence_rejections += 1;
        }
        if unsafe_action {
            unauthorized_action_count += 1;
        }

        results.push(WorkflowEvalCaseResult {
            id: case.id.clone(),
            input: case.input.clone(),
            expected_candidate_kind: case.expected_candidate_kind.clone(),
            candidate_kind,
            confidence,
            correct,
            low_confidence_rejection,
            unsafe_action,
            reason: candidate
                .as_ref()
                .map(|candidate| candidate.reason.clone())
                .unwrap_or_else(|| "no workflow candidate".to_string()),
        });
    }

    let accuracy = if cases.is_empty() {
        1.0
    } else {
        correct_cases as f64 / cases.len() as f64
    };

    WorkflowEvalReport {
        workflow_id: spec.id.clone(),
        total_cases: cases.len(),
        correct_cases,
        accuracy,
        low_confidence_rejections,
        unauthorized_action_count,
        passed: correct_cases == cases.len() && unauthorized_action_count == 0,
        cases: results,
    }
}

pub fn build_workflow_shadow_evidence_report(
    spec: &WorkflowSpec,
    cases: &[WorkflowEvalCase],
) -> WorkflowShadowEvidenceReport {
    let eval_report = evaluate_workflow(spec, cases);
    let mut evidence_cases = Vec::with_capacity(cases.len());

    for case in cases {
        let normalized = normalize_workflow_text(&case.input);
        let best_score = if deny_phrase_matches(&normalized, &spec.deny_phrases) {
            None
        } else {
            best_node_match(spec, &normalized).map(|(_, score)| score)
        };
        let candidate = compile_workflow_candidate(spec, &case.input);
        let compiler_candidate_kind = candidate
            .as_ref()
            .map(|candidate| candidate.candidate_kind.clone());
        let matched_current_plan = match (&case.expected_candidate_kind, &compiler_candidate_kind) {
            (Some(expected), Some(actual)) => expected == actual,
            (None, None) => true,
            _ => false,
        };
        let unsafe_action = candidate
            .as_ref()
            .is_some_and(WorkflowCandidate::has_unsafe_action);
        let low_confidence_rejection = case.expected_candidate_kind.is_some()
            && candidate.is_none()
            && best_score.is_some_and(|score| score < spec.min_confidence);
        let outcome = if unsafe_action {
            "unsafe_action"
        } else if matched_current_plan {
            "matched_current_plan"
        } else if low_confidence_rejection {
            "low_confidence_rejection"
        } else {
            "mismatch"
        };

        evidence_cases.push(WorkflowShadowEvidenceCase {
            case_id: case.id.clone(),
            input_sha256_12: short_sha256(&case.input),
            input_redacted: true,
            current_plan_kind: case.expected_candidate_kind.clone(),
            compiler_candidate_kind,
            compiler_confidence: candidate.as_ref().map(|candidate| candidate.confidence),
            matched_current_plan,
            read_only: candidate
                .as_ref()
                .map(|candidate| candidate.read_only)
                .unwrap_or(true),
            unsafe_action,
            low_confidence_rejection,
            outcome: outcome.to_string(),
        });
    }

    WorkflowShadowEvidenceReport {
        kind: "workflow_compiler_shadow_evidence_v1".to_string(),
        workflow_id: spec.id.clone(),
        source: "embedded_system_diagnostics_eval_cases".to_string(),
        redacted: true,
        total_cases: eval_report.total_cases,
        matched_current_plan_cases: eval_report.correct_cases,
        low_confidence_rejections: eval_report.low_confidence_rejections,
        unauthorized_action_count: eval_report.unauthorized_action_count,
        passed: eval_report.passed,
        cases: evidence_cases,
    }
}

fn validate_workflow_spec(spec: WorkflowSpec) -> Result<WorkflowSpec, String> {
    if spec.id.trim().is_empty() {
        return Err("INVALID_WORKFLOW_SPEC: id is required".to_string());
    }
    if spec.nodes.is_empty() {
        return Err("INVALID_WORKFLOW_SPEC: at least one node is required".to_string());
    }
    let mut ids = HashSet::new();
    for node in &spec.nodes {
        if node.id.trim().is_empty() {
            return Err("INVALID_WORKFLOW_SPEC: node id is required".to_string());
        }
        if !ids.insert(node.id.as_str()) {
            return Err(format!(
                "INVALID_WORKFLOW_SPEC: duplicate node id {}",
                node.id
            ));
        }
        if !ALLOWED_GENERAL_MESSAGE_CANDIDATES.contains(&node.candidate_kind.as_str()) {
            return Err(format!(
                "INVALID_WORKFLOW_SPEC: unsupported candidate kind {}",
                node.candidate_kind
            ));
        }
        if node.phrases.is_empty() && node.keywords_any.is_empty() && node.keywords_all.is_empty() {
            return Err(format!(
                "INVALID_WORKFLOW_SPEC: node {} has no match criteria",
                node.id
            ));
        }
        if node
            .action
            .as_ref()
            .is_some_and(|action| !workflow_action_is_read_only(action))
        {
            return Err(format!(
                "UNSAFE_WORKFLOW_ACTION: node {} contains a non-read-only action",
                node.id
            ));
        }
    }
    Ok(spec)
}

fn best_node_match<'a>(spec: &'a WorkflowSpec, normalized: &str) -> Option<(&'a WorkflowNode, u8)> {
    let mut best: Option<(&WorkflowNode, u8)> = None;
    for node in &spec.nodes {
        let Some(score) = node_score(node, normalized) else {
            continue;
        };
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((node, score));
        }
    }
    best
}

fn node_score(node: &WorkflowNode, normalized: &str) -> Option<u8> {
    let mut score = 0u8;

    for phrase in &node.phrases {
        let normalized_phrase = normalize_workflow_text(phrase);
        if normalized_phrase.is_empty() {
            continue;
        }
        if normalized == normalized_phrase {
            score = score.max(node.confidence.saturating_add(5).min(100));
        } else if normalized.contains(&normalized_phrase) {
            score = score.max(node.confidence);
        }
    }

    let all_match = !node.keywords_all.is_empty()
        && node
            .keywords_all
            .iter()
            .all(|keyword| normalized.contains(&normalize_workflow_text(keyword)));
    let any_match = !node.keywords_any.is_empty()
        && node
            .keywords_any
            .iter()
            .any(|keyword| normalized.contains(&normalize_workflow_text(keyword)));

    match (!node.keywords_all.is_empty(), !node.keywords_any.is_empty()) {
        (true, true) if all_match && any_match => {
            score = score.max(node.confidence.saturating_sub(5));
        }
        (true, false) if all_match => {
            score = score.max(node.confidence.saturating_sub(5));
        }
        (false, true) if any_match => {
            score = score.max(node.confidence.saturating_sub(15));
        }
        _ => {}
    }

    (score > 0).then_some(score)
}

fn deny_phrase_matches(normalized: &str, deny_phrases: &[String]) -> bool {
    deny_phrases.iter().any(|phrase| {
        let normalized_phrase = normalize_workflow_text(phrase);
        !normalized_phrase.is_empty() && normalized.contains(&normalized_phrase)
    })
}

fn workflow_action_is_read_only(action: &Action) -> bool {
    if action.risk_level != RiskLevel::Low || action.requires_approval {
        return false;
    }
    matches!(
        (
            action.domain.trim().to_ascii_lowercase().as_str(),
            action.operation.trim().to_ascii_lowercase().as_str(),
        ),
        ("service", "status")
            | ("files", "search")
            | ("files", "list")
            | ("files", "stat")
            | ("files", "read_text")
            | ("diagnostics", "read")
    )
}

fn normalize_workflow_text(input: &str) -> String {
    input
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, ':' | '：' | '-' | '_' | '/' | '\\'))
        .collect()
}

fn short_sha256(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn eval_case(id: &str, input: &str, expected_candidate_kind: Option<&str>) -> WorkflowEvalCase {
    WorkflowEvalCase {
        id: id.to_string(),
        input: input.to_string(),
        expected_candidate_kind: expected_candidate_kind.map(str::to_string),
        min_confidence: WORKFLOW_COMPILER_MIN_CONFIDENCE,
    }
}

fn default_min_confidence() -> u8 {
    WORKFLOW_COMPILER_MIN_CONFIDENCE
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        build_workflow_shadow_evidence_report, compile_system_diagnostics_candidate,
        compile_workflow_candidate, evaluate_workflow, parse_workflow_spec,
        system_diagnostics_eval_cases, system_diagnostics_workflow_spec,
    };

    #[test]
    fn workflow_compiler_reports_invalid_spec_json() {
        let error = parse_workflow_spec("{not-json").expect_err("invalid JSON should fail");
        assert!(error.starts_with("INVALID_WORKFLOW_SPEC_JSON"));
    }

    #[test]
    fn workflow_compiler_system_diagnostics_matches_status_phrases() {
        let cases = [
            ("状态", "system_readiness"),
            ("现在 K3 好了吗", "system_readiness"),
            ("微信通了吗", "system_readiness"),
            ("HA 正常吗", "system_readiness"),
            ("semantic router 状态怎么样", "system_readiness"),
            ("压测前状态怎么样", "evt_readiness"),
            ("帮我做一下 EVT 预检", "evt_preflight"),
            ("生成压测证据包", "evt_evidence_bundle"),
        ];

        for (input, expected) in cases {
            let candidate =
                compile_system_diagnostics_candidate(input).expect("candidate should match");
            assert_eq!(candidate.candidate_kind, expected);
            assert!(candidate.read_only);
            assert!(!candidate.has_unsafe_action());
        }
    }

    #[test]
    fn workflow_compiler_system_diagnostics_blocks_execution_requests() {
        for input in ["重启 SSH", "连接摄像头", "删除这个文件", "开灯"] {
            assert!(compile_system_diagnostics_candidate(input).is_none());
        }
    }

    #[test]
    fn workflow_compiler_rejects_unsafe_actions_in_specs() {
        let spec = json!({
            "id": "bad",
            "version": "1.0.0",
            "summary": "bad",
            "nodes": [{
                "id": "restart",
                "candidate_kind": "system_readiness",
                "phrases": ["状态"],
                "confidence": 90,
                "read_only": false,
                "reason": "bad",
                "action": {
                    "domain": "service",
                    "operation": "restart",
                    "resource": {"service_name": "ssh"},
                    "args": {},
                    "risk_level": "HIGH",
                    "requires_approval": true,
                    "dry_run": false
                }
            }]
        });
        let error = parse_workflow_spec(&spec.to_string()).expect_err("unsafe action should fail");
        assert!(error.starts_with("UNSAFE_WORKFLOW_ACTION"));
    }

    #[test]
    fn workflow_compiler_eval_pack_passes() {
        let spec = system_diagnostics_workflow_spec();
        let report = evaluate_workflow(&spec, &system_diagnostics_eval_cases());
        assert!(report.passed, "{report:#?}");
        assert_eq!(report.unauthorized_action_count, 0);
        assert!(report.total_cases >= 50);
    }

    #[test]
    fn workflow_compiler_shadow_evidence_is_redacted() {
        let spec = system_diagnostics_workflow_spec();
        let report = build_workflow_shadow_evidence_report(&spec, &system_diagnostics_eval_cases());
        let text = serde_json::to_string(&report).expect("serialize evidence");

        assert!(report.passed, "{report:#?}");
        assert!(report.redacted);
        assert!(report.cases.iter().all(|case| case.input_redacted));
        assert!(report
            .cases
            .iter()
            .all(|case| case.input_sha256_12.len() == 12));
        assert!(!text.contains("微信通了吗"));
        assert!(!text.contains("重启 SSH"));
    }

    #[test]
    fn workflow_compiler_unknown_text_has_no_candidate() {
        let spec = system_diagnostics_workflow_spec();
        assert!(compile_workflow_candidate(&spec, "今天天气怎么样").is_none());
    }
}
