//! Contextual-integrity inspired privacy gate for cloud-bound model calls.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::control_plane::models::{PrivacyLevel, PrivacyTransformRecord};
use crate::runtime::knowledge::KnowledgeSearchCitation;

pub const PRIVACY_GATEWAY_POLICY_VERSION: &str = "harbor_privacy_gateway_ci_v1";

const FACT_MAX_CHARS: usize = 360;
const TASK_MAX_CHARS: usize = 180;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyFlowRecord {
    pub sender_role: String,
    pub subject_role: String,
    pub recipient_kind: String,
    pub information_types: Vec<String>,
    pub purpose: String,
    pub consent_basis: String,
    pub destination: String,
    pub privacy_level: PrivacyLevel,
    pub decision: String,
    pub policy_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyGatewayDecision {
    pub decision: String,
    pub cloud_allowed: bool,
    pub risk_level: String,
    pub reason: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SanitizedCitation {
    pub citation_ref: usize,
    pub modality: String,
    pub source_hash: String,
    pub chunk_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<String>,
    pub fact: String,
    #[serde(default)]
    pub risk_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticCapsule {
    pub capsule_id: String,
    pub task: String,
    #[serde(default)]
    pub facts: Vec<SanitizedCitation>,
    #[serde(default)]
    pub source_hashes: Vec<String>,
    #[serde(default)]
    pub transform_steps: Vec<String>,
    pub redacted: bool,
    pub policy_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyGatewayEvaluation {
    pub flow: PrivacyFlowRecord,
    pub decision: PrivacyGatewayDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_capsule: Option<SemanticCapsule>,
    pub privacy_transform: PrivacyTransformRecord,
}

pub struct PrivacyGateway;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyGatewayEvalCase {
    pub case_id: String,
    pub query: String,
    pub privacy_level: PrivacyLevel,
    #[serde(default)]
    pub citations: Vec<KnowledgeSearchCitation>,
    pub expected_decision: String,
    #[serde(default)]
    pub expected_information_types: Vec<String>,
    #[serde(default)]
    pub expected_risk_level: Option<String>,
    #[serde(default)]
    pub expected_capsule: Option<bool>,
    #[serde(default)]
    pub leak_markers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyGatewayEvalCaseResult {
    pub case_id: String,
    pub passed: bool,
    pub decision: String,
    pub expected_decision: String,
    pub risk_level: String,
    pub source_leak_count: usize,
    pub blocked_or_degraded: bool,
    pub high_risk: bool,
    #[serde(default)]
    pub missing_information_types: Vec<String>,
    #[serde(default)]
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrivacyGatewayEvalReport {
    pub kind: String,
    pub policy_version: String,
    pub total_cases: usize,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub source_leak_count: usize,
    pub blocked_or_degraded_count: usize,
    pub high_risk_count: usize,
    pub passed: bool,
    #[serde(default)]
    pub cases: Vec<PrivacyGatewayEvalCaseResult>,
}

impl PrivacyGateway {
    pub fn evaluate_rag_answer_cloud_context(
        query: &str,
        citations: &[KnowledgeSearchCitation],
        privacy_level: PrivacyLevel,
        workspace_id: &str,
        source_ref: &str,
        sender_role: &str,
    ) -> PrivacyGatewayEvaluation {
        let task = sanitize_free_text(query, TASK_MAX_CHARS)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "answer from provided citations".to_string());
        let facts = citations
            .iter()
            .enumerate()
            .filter_map(|(index, citation)| sanitize_citation(index + 1, citation))
            .collect::<Vec<_>>();
        let source_hashes = facts
            .iter()
            .map(|citation| citation.source_hash.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let information_types = classify_information_types(query, citations, &facts);
        let risk_level = risk_level_for_types(&information_types);
        let mut warnings = Vec::new();
        let mut decision = match privacy_level {
            PrivacyLevel::StrictLocal => PrivacyGatewayDecision {
                decision: "blocked_or_degraded".to_string(),
                cloud_allowed: false,
                risk_level: risk_level.clone(),
                reason: "strict_local blocks cloud execution".to_string(),
                warnings: Vec::new(),
            },
            PrivacyLevel::AllowRedactedCloud => {
                if facts.is_empty() {
                    PrivacyGatewayDecision {
                        decision: "blocked_or_degraded".to_string(),
                        cloud_allowed: false,
                        risk_level: risk_level.clone(),
                        reason: "semantic capsule has no sanitized facts".to_string(),
                        warnings: vec![
                            "privacy gateway did not emit a cloud-safe capsule".to_string()
                        ],
                    }
                } else if information_types.iter().any(|kind| kind == "credential") {
                    PrivacyGatewayDecision {
                        decision: "blocked_or_degraded".to_string(),
                        cloud_allowed: false,
                        risk_level: "high".to_string(),
                        reason: "credential-like material is not allowed in redacted cloud mode"
                            .to_string(),
                        warnings: vec![
                            "credential-like material was detected and cloud execution was skipped"
                                .to_string(),
                        ],
                    }
                } else {
                    PrivacyGatewayDecision {
                        decision: "allow_redacted_cloud".to_string(),
                        cloud_allowed: true,
                        risk_level: risk_level.clone(),
                        reason: "redacted semantic capsule generated".to_string(),
                        warnings: Vec::new(),
                    }
                }
            }
            PrivacyLevel::AllowCloud => PrivacyGatewayDecision {
                decision: "allow_cloud".to_string(),
                cloud_allowed: true,
                risk_level: risk_level.clone(),
                reason: "workspace policy allows cloud; contextual integrity evidence recorded"
                    .to_string(),
                warnings: Vec::new(),
            },
        };
        warnings.append(&mut decision.warnings);
        decision.warnings = warnings.clone();

        let capsule = (!facts.is_empty()).then(|| SemanticCapsule {
            capsule_id: format!("capsule-{}", short_sha256(&format!("{source_ref}:{task}"))),
            task,
            facts,
            source_hashes: source_hashes.clone(),
            transform_steps: vec![
                "hash_source_refs".to_string(),
                "redact_paths_urls_credentials".to_string(),
                format!("truncate_facts_to_{FACT_MAX_CHARS}_chars"),
                "emit_task_minimal_semantic_capsule".to_string(),
            ],
            redacted: true,
            policy_version: PRIVACY_GATEWAY_POLICY_VERSION.to_string(),
        });

        let transform_steps = capsule
            .as_ref()
            .map(|capsule| json!(capsule.transform_steps))
            .unwrap_or_else(|| json!(["cloud_blocked_no_capsule"]));
        let output_ref = json!({
            "capsule_id": capsule.as_ref().map(|capsule| capsule.capsule_id.clone()),
            "source_hashes": source_hashes,
            "redacted": true,
            "metadata_only": true,
        });
        let flow = PrivacyFlowRecord {
            sender_role: normalize_role(sender_role, "user"),
            subject_role: "household_context".to_string(),
            recipient_kind: "cloud_model".to_string(),
            information_types,
            purpose: "retrieval.answer".to_string(),
            consent_basis: consent_basis_for_level(privacy_level).to_string(),
            destination: destination_for_level(privacy_level).to_string(),
            privacy_level,
            decision: decision.decision.clone(),
            policy_version: PRIVACY_GATEWAY_POLICY_VERSION.to_string(),
        };
        let privacy_transform = PrivacyTransformRecord {
            privacy_transform_id: format!(
                "privacy-transform-{}",
                short_sha256(&format!("{source_ref}:{}", flow.decision))
            ),
            workspace_id: workspace_id.to_string(),
            source_kind: "rag_answer_context".to_string(),
            source_ref: source_ref.to_string(),
            transform_steps,
            output_ref,
            policy_version: PRIVACY_GATEWAY_POLICY_VERSION.to_string(),
            created_at: Some(now_unix_string()),
        };

        PrivacyGatewayEvaluation {
            flow,
            decision,
            semantic_capsule: capsule,
            privacy_transform,
        }
    }
}

pub fn privacy_gateway_eval_cases() -> Vec<PrivacyGatewayEvalCase> {
    vec![
        PrivacyGatewayEvalCase {
            case_id: "strict_local_blocks_cloud".to_string(),
            query: "summarize the family note".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            citations: vec![eval_citation(
                "family-note.md",
                r"C:\Users\Bean\family-note.md",
                "The family garden plan is ready.",
                Some(r"C:\Users\Bean\family-note.md"),
            )],
            expected_decision: "blocked_or_degraded".to_string(),
            expected_information_types: vec!["knowledge_context".to_string()],
            expected_risk_level: Some("low".to_string()),
            expected_capsule: Some(true),
            leak_markers: vec![r"C:\Users\Bean".to_string(), "family-note.md".to_string()],
        },
        PrivacyGatewayEvalCase {
            case_id: "redacted_cloud_generates_capsule".to_string(),
            query: "what does the garden plan say".to_string(),
            privacy_level: PrivacyLevel::AllowRedactedCloud,
            citations: vec![eval_citation(
                "garden-plan.md",
                r"C:\Users\Bean\garden-plan.md",
                "The garden plan is ready. Raw source C:\\Users\\Bean\\garden-plan.md https://example.test/private",
                Some("https://example.test/private"),
            )],
            expected_decision: "allow_redacted_cloud".to_string(),
            expected_information_types: vec!["knowledge_context".to_string()],
            expected_risk_level: Some("low".to_string()),
            expected_capsule: Some(true),
            leak_markers: vec![
                r"C:\Users\Bean".to_string(),
                "garden-plan.md".to_string(),
                "https://example.test/private".to_string(),
            ],
        },
        PrivacyGatewayEvalCase {
            case_id: "credential_blocks_redacted_cloud".to_string(),
            query: "what is the garage api key".to_string(),
            privacy_level: PrivacyLevel::AllowRedactedCloud,
            citations: vec![eval_citation(
                "garage-secret.md",
                r"C:\Users\Bean\garage-secret.md",
                "The garage integration api_key=secret-123 should never leave the home.",
                Some(r"C:\Users\Bean\garage-secret.md"),
            )],
            expected_decision: "blocked_or_degraded".to_string(),
            expected_information_types: vec!["credential".to_string(), "home_security".to_string()],
            expected_risk_level: Some("high".to_string()),
            expected_capsule: Some(true),
            leak_markers: vec![
                "api_key=secret-123".to_string(),
                "secret-123".to_string(),
                r"C:\Users\Bean".to_string(),
                "garage-secret.md".to_string(),
            ],
        },
        PrivacyGatewayEvalCase {
            case_id: "child_health_home_security_risk_tags".to_string(),
            query: "child health and camera status".to_string(),
            privacy_level: PrivacyLevel::AllowRedactedCloud,
            citations: vec![eval_citation(
                "home-status.md",
                r"C:\Users\Bean\home-status.md",
                "Child location, health status, home camera, and front door status are in this note.",
                Some(r"C:\Users\Bean\home-status.md"),
            )],
            expected_decision: "allow_redacted_cloud".to_string(),
            expected_information_types: vec![
                "child_location".to_string(),
                "health_status".to_string(),
                "home_security".to_string(),
            ],
            expected_risk_level: Some("medium".to_string()),
            expected_capsule: Some(true),
            leak_markers: vec![r"C:\Users\Bean".to_string(), "home-status.md".to_string()],
        },
        PrivacyGatewayEvalCase {
            case_id: "allow_cloud_records_evidence_only".to_string(),
            query: "summarize the device note".to_string(),
            privacy_level: PrivacyLevel::AllowCloud,
            citations: vec![eval_citation(
                "device-note.md",
                r"C:\Users\Bean\device-note.md",
                "Harbor can answer from controlled cloud fallback.",
                Some(r"C:\Users\Bean\device-note.md"),
            )],
            expected_decision: "allow_cloud".to_string(),
            expected_information_types: vec!["knowledge_context".to_string()],
            expected_risk_level: Some("low".to_string()),
            expected_capsule: Some(true),
            leak_markers: vec![r"C:\Users\Bean".to_string(), "device-note.md".to_string()],
        },
    ]
}

pub fn evaluate_privacy_gateway_cases(
    cases: &[PrivacyGatewayEvalCase],
) -> PrivacyGatewayEvalReport {
    let mut results = Vec::new();
    let mut source_leak_count = 0usize;
    let mut blocked_or_degraded_count = 0usize;
    let mut high_risk_count = 0usize;

    for case in cases {
        let evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            &case.query,
            &case.citations,
            case.privacy_level,
            "home-1",
            &case.case_id,
            "owner",
        );
        let evidence = evaluation.evidence_value(case.expected_capsule.unwrap_or(false));
        let serialized_capsule =
            serde_json::to_string(&evaluation.semantic_capsule).unwrap_or_else(|_| String::new());
        let serialized_evidence =
            serde_json::to_string(&evidence).unwrap_or_else(|_| String::new());
        let mut failures = Vec::new();

        if evaluation.decision.decision != case.expected_decision {
            failures.push(format!(
                "decision expected {} but got {}",
                case.expected_decision, evaluation.decision.decision
            ));
        }
        if let Some(expected_risk) = case.expected_risk_level.as_ref() {
            if &evaluation.decision.risk_level != expected_risk {
                failures.push(format!(
                    "risk_level expected {expected_risk} but got {}",
                    evaluation.decision.risk_level
                ));
            }
        }
        if let Some(expect_capsule) = case.expected_capsule {
            if evaluation.semantic_capsule.is_some() != expect_capsule {
                failures.push(format!(
                    "semantic_capsule expected {expect_capsule} but got {}",
                    evaluation.semantic_capsule.is_some()
                ));
            }
        }
        let missing_information_types = case
            .expected_information_types
            .iter()
            .filter(|expected| !evaluation.flow.information_types.contains(*expected))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_information_types.is_empty() {
            failures.push(format!(
                "missing information_types: {}",
                missing_information_types.join(",")
            ));
        }
        let leaks = case
            .leak_markers
            .iter()
            .filter(|marker| {
                !marker.trim().is_empty()
                    && (serialized_capsule.contains(marker.as_str())
                        || serialized_evidence.contains(marker.as_str()))
            })
            .count();
        if leaks > 0 {
            failures.push(format!("{leaks} source leak marker(s) found"));
        }

        let blocked_or_degraded = evaluation.decision.decision == "blocked_or_degraded";
        let high_risk = evaluation.decision.risk_level == "high";
        source_leak_count += leaks;
        blocked_or_degraded_count += usize::from(blocked_or_degraded);
        high_risk_count += usize::from(high_risk);

        results.push(PrivacyGatewayEvalCaseResult {
            case_id: case.case_id.clone(),
            passed: failures.is_empty(),
            decision: evaluation.decision.decision,
            expected_decision: case.expected_decision.clone(),
            risk_level: evaluation.decision.risk_level,
            source_leak_count: leaks,
            blocked_or_degraded,
            high_risk,
            missing_information_types,
            failures,
        });
    }

    let passed_cases = results.iter().filter(|case| case.passed).count();
    let total_cases = results.len();
    let failed_cases = total_cases.saturating_sub(passed_cases);
    PrivacyGatewayEvalReport {
        kind: "privacy_gateway_eval_report".to_string(),
        policy_version: PRIVACY_GATEWAY_POLICY_VERSION.to_string(),
        total_cases,
        passed_cases,
        failed_cases,
        source_leak_count,
        blocked_or_degraded_count,
        high_risk_count,
        passed: failed_cases == 0 && source_leak_count == 0,
        cases: results,
    }
}

impl SemanticCapsule {
    pub fn to_cloud_prompt(&self) -> String {
        let mut lines = vec![
            format!("Harbor Privacy Gateway capsule: {}", self.capsule_id),
            "Answer only from the numbered facts below.".to_string(),
            "Do not infer identities, locations, credentials, file paths, or extra household details beyond the facts.".to_string(),
            "Every substantive statement must include a [n] citation marker.".to_string(),
            String::new(),
            format!("Task: {}", self.task),
            String::new(),
            "Facts:".to_string(),
        ];
        for fact in &self.facts {
            lines.push(format!(
                "[{}] modality={} chunk_hash={} risk_tags={} fact={}",
                fact.citation_ref,
                fact.modality,
                fact.chunk_hash,
                if fact.risk_tags.is_empty() {
                    "none".to_string()
                } else {
                    fact.risk_tags.join(",")
                },
                fact.fact
            ));
        }
        lines.join("\n")
    }
}

impl PrivacyGatewayEvaluation {
    pub fn evidence_value(&self, capsule_prompt_used: bool) -> Value {
        let fact_count = self
            .semantic_capsule
            .as_ref()
            .map(|capsule| capsule.facts.len())
            .unwrap_or_default();
        let source_hashes = self
            .semantic_capsule
            .as_ref()
            .map(|capsule| capsule.source_hashes.clone())
            .unwrap_or_default();
        json!({
            "kind": "privacy_gateway.rag_answer",
            "decision": self.decision.decision,
            "reason": self.decision.reason,
            "risk_level": self.decision.risk_level,
            "cloud_allowed": self.decision.cloud_allowed,
            "capsule_prompt_used": capsule_prompt_used,
            "policy_version": self.flow.policy_version,
            "privacy_transform_id": self.privacy_transform.privacy_transform_id,
            "information_types": self.flow.information_types,
            "redacted": true,
            "metadata_only": true,
            "capsule_fact_count": fact_count,
            "source_hashes": source_hashes,
            "warnings": self.decision.warnings,
        })
    }
}

fn sanitize_citation(
    citation_ref: usize,
    citation: &KnowledgeSearchCitation,
) -> Option<SanitizedCitation> {
    let preview = citation.preview.as_deref().unwrap_or_default();
    let fact = sanitize_free_text(preview, FACT_MAX_CHARS)?;
    if fact.trim().is_empty() {
        return None;
    }
    let source_hash = short_sha256(&format!(
        "{}:{}:{}",
        citation.title,
        citation.path,
        citation.source_path.as_deref().unwrap_or_default()
    ));
    let chunk_hash = short_sha256(&format!(
        "{}:{}:{}",
        citation.path,
        citation.chunk_id.as_deref().unwrap_or("chunk"),
        citation_ref
    ));
    let line_range = match (citation.line_start, citation.line_end) {
        (Some(start), Some(end)) => Some(format!("{start}-{end}")),
        (Some(start), None) => Some(start.to_string()),
        _ => None,
    };
    let risk_tags = classify_text(&format!(
        "{} {} {}",
        preview, citation.title, citation.modality
    ));
    Some(SanitizedCitation {
        citation_ref,
        modality: citation.modality.clone(),
        source_hash,
        chunk_hash,
        line_range,
        fact,
        risk_tags,
    })
}

fn sanitize_free_text(value: &str, max_chars: usize) -> Option<String> {
    let tokens = value
        .split_whitespace()
        .map(|token| {
            if should_redact_token(token) {
                "[redacted]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>();
    let mut text = tokens.join(" ");
    if text.trim().is_empty() {
        return None;
    }
    if text.chars().count() > max_chars {
        text = text.chars().take(max_chars).collect();
    }
    Some(text)
}

fn should_redact_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("rtsp://")
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("bearer")
        || lower.contains("\\\\")
        || lower.contains("c:\\")
        || lower.contains("/mnt/")
        || lower.contains("/home/")
        || lower.contains(".harborbeacon/")
}

fn classify_information_types(
    query: &str,
    citations: &[KnowledgeSearchCitation],
    facts: &[SanitizedCitation],
) -> Vec<String> {
    let mut tags = classify_text(query).into_iter().collect::<BTreeSet<_>>();
    for citation in citations {
        tags.extend(classify_text(
            citation.preview.as_deref().unwrap_or_default(),
        ));
        tags.extend(classify_text(&citation.title));
        tags.extend(classify_text(&citation.modality));
    }
    for fact in facts {
        tags.extend(fact.risk_tags.iter().cloned());
    }
    if tags.is_empty() {
        tags.insert("knowledge_context".to_string());
    }
    tags.into_iter().collect()
}

fn classify_text(value: &str) -> Vec<String> {
    let lower = value.to_ascii_lowercase();
    let mut tags = BTreeSet::new();
    if contains_any(
        &lower,
        &[
            "password",
            "passwd",
            "api_key",
            "apikey",
            "token",
            "credential",
            "secret",
        ],
    ) || value.contains("密码")
        || value.contains("凭据")
        || value.contains("密钥")
    {
        tags.insert("credential".to_string());
    }
    if contains_any(&lower, &["child", "kid", "minor", "location"])
        || value.contains("孩子")
        || value.contains("儿童")
        || value.contains("位置")
        || value.contains("在哪")
    {
        if contains_any(&lower, &["child", "kid", "minor", "location"])
            || value.contains("孩子")
            || value.contains("儿童")
        {
            tags.insert("child_location".to_string());
        }
    }
    if contains_any(
        &lower,
        &[
            "health",
            "medical",
            "medicine",
            "hospital",
            "diagnosis",
            "elderly",
        ],
    ) || value.contains("健康")
        || value.contains("生病")
        || value.contains("药")
        || value.contains("老人")
    {
        tags.insert("health_status".to_string());
    }
    if contains_any(
        &lower,
        &["camera", "lock", "unlock", "visitor", "garage", "door"],
    ) || value.contains("摄像头")
        || value.contains("门锁")
        || value.contains("访客")
        || value.contains("车库")
        || value.contains("门口")
    {
        tags.insert("home_security".to_string());
    }
    if contains_any(&lower, &["phone", "email", "address", "passport"])
        || value.contains("手机号")
        || value.contains("地址")
        || value.contains("身份证")
    {
        tags.insert("personal_identity".to_string());
    }
    tags.into_iter().collect()
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn risk_level_for_types(types: &[String]) -> String {
    if types.iter().any(|kind| kind == "credential") {
        "high".to_string()
    } else if types.iter().any(|kind| {
        matches!(
            kind.as_str(),
            "child_location" | "health_status" | "home_security" | "personal_identity"
        )
    }) {
        "medium".to_string()
    } else {
        "low".to_string()
    }
}

fn consent_basis_for_level(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::StrictLocal => "none",
        PrivacyLevel::AllowRedactedCloud => "workspace_policy_redacted_cloud",
        PrivacyLevel::AllowCloud => "workspace_policy_cloud",
    }
}

fn destination_for_level(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::StrictLocal => "local_only",
        PrivacyLevel::AllowRedactedCloud | PrivacyLevel::AllowCloud => "cloud_model",
    }
}

fn normalize_role(value: &str, default: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        default.to_string()
    } else {
        sanitize_free_text(value, 64).unwrap_or_else(|| default.to_string())
    }
}

fn short_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_unix_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn eval_citation(
    title: &str,
    path: &str,
    preview: &str,
    source_path: Option<&str>,
) -> KnowledgeSearchCitation {
    KnowledgeSearchCitation {
        title: title.to_string(),
        path: path.to_string(),
        modality: "document".to_string(),
        chunk_id: Some("chunk-1".to_string()),
        line_start: Some(1),
        line_end: Some(1),
        matched_terms: Vec::new(),
        preview: Some(preview.to_string()),
        score: 100,
        lexical_score: None,
        embedding_score: None,
        hybrid_score: None,
        rerank_score: None,
        provenance: Some("document".to_string()),
        source_path: source_path.map(ToString::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation() -> KnowledgeSearchCitation {
        KnowledgeSearchCitation {
            title: "family-note.md".to_string(),
            path: r"C:\Users\Bean\family-note.md".to_string(),
            modality: "document".to_string(),
            chunk_id: Some("chunk-1".to_string()),
            line_start: Some(1),
            line_end: Some(2),
            matched_terms: Vec::new(),
            preview: Some(
                "The garden plan is ready. Raw file C:\\Users\\Bean\\notes.md https://example.test/source"
                    .to_string(),
            ),
            score: 80,
            lexical_score: None,
            embedding_score: None,
            hybrid_score: None,
            rerank_score: None,
            provenance: None,
            source_path: Some("https://example.test/private/source".to_string()),
        }
    }

    #[test]
    fn redacted_cloud_emits_capsule_without_paths_or_urls() {
        let evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            "What is in C:\\Users\\Bean\\family-note.md?",
            &[citation()],
            PrivacyLevel::AllowRedactedCloud,
            "home-1",
            "task-1",
            "owner",
        );

        assert_eq!(evaluation.decision.decision, "allow_redacted_cloud");
        let capsule = evaluation.semantic_capsule.expect("semantic capsule");
        assert_eq!(capsule.facts.len(), 1);
        assert_eq!(capsule.source_hashes[0].len(), 12);
        let serialized = serde_json::to_string(&capsule).expect("capsule json");
        assert!(!serialized.contains("C:\\Users"));
        assert!(!serialized.contains("https://example.test"));
        assert!(!serialized.contains("token=abc123"));
        assert!(serialized.contains("[redacted]"));
    }

    #[test]
    fn strict_local_blocks_cloud_decision() {
        let evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            "summarize",
            &[citation()],
            PrivacyLevel::StrictLocal,
            "home-1",
            "task-1",
            "owner",
        );

        assert_eq!(evaluation.decision.decision, "blocked_or_degraded");
        assert!(!evaluation.decision.cloud_allowed);
        assert_eq!(evaluation.flow.destination, "local_only");
    }

    #[test]
    fn sensitive_information_types_are_marked_higher_risk() {
        let mut sensitive = citation();
        sensitive.preview = Some(
            "Child location, health status, home camera, and password are in this note."
                .to_string(),
        );
        let evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            "child health and camera status",
            &[sensitive],
            PrivacyLevel::AllowRedactedCloud,
            "home-1",
            "task-1",
            "owner",
        );

        assert_eq!(evaluation.decision.risk_level, "high");
        assert!(evaluation
            .flow
            .information_types
            .contains(&"child_location".to_string()));
        assert!(evaluation
            .flow
            .information_types
            .contains(&"health_status".to_string()));
        assert!(evaluation
            .flow
            .information_types
            .contains(&"credential".to_string()));
        assert_eq!(evaluation.decision.decision, "blocked_or_degraded");
    }

    #[test]
    fn cloud_prompt_uses_numbered_capsule_facts() {
        let evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            "summarize the plan",
            &[citation()],
            PrivacyLevel::AllowRedactedCloud,
            "home-1",
            "task-1",
            "owner",
        );
        let prompt = evaluation
            .semantic_capsule
            .expect("semantic capsule")
            .to_cloud_prompt();

        assert!(prompt.contains("Harbor Privacy Gateway capsule"));
        assert!(prompt.contains("[1]"));
        assert!(!prompt.contains("family-note.md"));
        assert!(!prompt.contains("C:\\Users"));
    }

    #[test]
    fn privacy_gateway_eval_pack_passes_without_source_leaks() {
        let report = evaluate_privacy_gateway_cases(&privacy_gateway_eval_cases());

        assert!(report.passed, "{report:#?}");
        assert_eq!(report.failed_cases, 0);
        assert_eq!(report.source_leak_count, 0);
        assert!(report.blocked_or_degraded_count >= 2);
        assert!(report.high_risk_count >= 1);
        assert_eq!(report.policy_version, PRIVACY_GATEWAY_POLICY_VERSION);
    }
}
