use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::control_plane::models::PrivacyLevel;
use crate::runtime::admin_console::{
    AdminConsoleStore, KnowledgeSettings, KnowledgeSourceRoot, RagResourceProfile,
};
use crate::runtime::knowledge_index::{KnowledgeIndexConfig, KnowledgeIndexService};
use crate::runtime::registry::DeviceRegistryStore;
use crate::runtime::task_api::{
    TaskApiService, TaskIntent, TaskRequest, TaskRequestAcceptance, TaskSource, TaskStatus,
};
use crate::runtime::task_session::TaskConversationStore;

pub const RAG_QUALITY_SCHEMA_VERSION: u32 = 1;
pub const RAG_QUALITY_CONTRACT: &str = "harborbeacon-offline-task-api-rag-quality";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RagQualityThresholds {
    pub min_schema_validity_rate: f64,
    pub min_citation_whitelist_rate: f64,
    pub min_citation_support_precision: f64,
    pub min_required_evidence_coverage: f64,
    pub min_claim_coverage_rate: f64,
    pub min_fallback_preservation_rate: f64,
    pub min_cold_warm_consistency_rate: f64,
    pub min_idempotent_replay_rate: f64,
    pub max_exact_document_violations: usize,
    pub max_duplicate_citation_violations: usize,
}

impl Default for RagQualityThresholds {
    fn default() -> Self {
        Self {
            min_schema_validity_rate: 1.0,
            min_citation_whitelist_rate: 1.0,
            min_citation_support_precision: 1.0,
            min_required_evidence_coverage: 1.0,
            min_claim_coverage_rate: 1.0,
            min_fallback_preservation_rate: 1.0,
            min_cold_warm_consistency_rate: 1.0,
            min_idempotent_replay_rate: 1.0,
            max_exact_document_violations: 0,
            max_duplicate_citation_violations: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RagQualitySuite {
    pub schema_version: u32,
    pub suite_id: String,
    pub description: String,
    pub thresholds: RagQualityThresholds,
    pub files: Vec<RagQualityFile>,
    pub cases: Vec<RagQualityCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RagQualityFile {
    pub document_id: String,
    pub relative_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RagQualityCase {
    pub case_id: String,
    pub query: String,
    #[serde(default)]
    pub focus_document_ids: Vec<String>,
    pub allowed_document_ids: Vec<String>,
    #[serde(default)]
    pub required_document_ids: Vec<String>,
    #[serde(default)]
    pub required_answer_terms: Vec<String>,
    #[serde(default)]
    pub forbidden_answer_terms: Vec<String>,
    pub expected_task_status: String,
    #[serde(default)]
    pub expected_degraded_reason: Option<String>,
    pub min_citations: usize,
    pub max_citations: usize,
    #[serde(default)]
    pub exact_document: bool,
    #[serde(default)]
    pub fallback_expected: bool,
    #[serde(default = "default_true")]
    pub include_documents: bool,
    #[serde(default)]
    pub include_images: bool,
    #[serde(default)]
    pub include_videos: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RagQualityObservation {
    pub case_id: String,
    pub schema_valid: bool,
    pub citation_document_ids: Vec<String>,
    pub allowed_document_ids: Vec<String>,
    pub required_document_ids: Vec<String>,
    pub supported_citation_count: usize,
    pub required_claim_count: usize,
    pub supported_claim_count: usize,
    pub duplicate_citation_count: usize,
    pub exact_document: bool,
    pub fallback_expected: bool,
    pub fallback_preserved: bool,
    pub cold_warm_consistent: bool,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RagQualityMetrics {
    pub case_count: usize,
    pub schema_validity_rate: f64,
    pub citation_whitelist_rate: f64,
    pub citation_support_precision: f64,
    pub required_evidence_coverage: f64,
    pub claim_coverage_rate: f64,
    pub fallback_preservation_rate: f64,
    pub cold_warm_consistency_rate: f64,
    pub idempotent_replay_rate: f64,
    pub exact_document_violations: usize,
    pub duplicate_citation_violations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RagQualityCaseResult {
    pub case_id: String,
    pub passed: bool,
    pub reasons: Vec<String>,
    pub task_status: String,
    pub response_status: Option<String>,
    pub degraded_reason: Option<String>,
    pub answer: String,
    pub citation_document_ids: Vec<String>,
    pub citation_paths: Vec<String>,
    pub cold_latency_ms: u128,
    pub warm_latency_ms: u128,
    pub cold_warm_consistent: bool,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RagQualityResourceSummary {
    pub concurrency: usize,
    pub max_queue_depth: usize,
    pub index_prepare_latency_ms: u128,
    pub cold_total_latency_ms: u128,
    pub warm_total_latency_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_process_rss_kib: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RagQualityGate {
    pub passed: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RagQualityReport {
    pub schema_version: u32,
    pub contract: String,
    pub suite_id: String,
    pub suite_digest_sha256: String,
    pub source_revision: String,
    pub generated_at_unix_ms: u128,
    pub thresholds: RagQualityThresholds,
    pub metrics: RagQualityMetrics,
    pub resources: RagQualityResourceSummary,
    pub cases: Vec<RagQualityCaseResult>,
    pub gate: RagQualityGate,
}

pub fn load_suite(path: &Path) -> Result<RagQualitySuite, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read suite {}: {error}", path.display()))?;
    let suite = serde_json::from_str::<RagQualitySuite>(&contents)
        .map_err(|error| format!("failed to parse suite {}: {error}", path.display()))?;
    validate_suite(&suite)?;
    Ok(suite)
}

pub fn validate_suite(suite: &RagQualitySuite) -> Result<(), String> {
    if suite.schema_version != RAG_QUALITY_SCHEMA_VERSION {
        return Err(format!(
            "unsupported RAG quality schema_version: {}",
            suite.schema_version
        ));
    }
    if suite.suite_id.trim().is_empty() {
        return Err("suite_id must not be empty".to_string());
    }
    if suite.thresholds != RagQualityThresholds::default() {
        return Err(
            "schema v1 thresholds are frozen and must match the contract defaults".to_string(),
        );
    }
    if suite.cases.is_empty() {
        return Err("suite must contain at least one case".to_string());
    }

    let mut document_ids = HashSet::new();
    let mut relative_paths = HashSet::new();
    for file in &suite.files {
        if file.document_id.trim().is_empty() {
            return Err("document_id must not be empty".to_string());
        }
        if !document_ids.insert(file.document_id.as_str()) {
            return Err(format!("duplicate document_id: {}", file.document_id));
        }
        validate_relative_path(&file.relative_path)?;
        if !relative_paths.insert(file.relative_path.as_str()) {
            return Err(format!("duplicate relative_path: {}", file.relative_path));
        }
    }

    let mut case_ids = HashSet::new();
    for case in &suite.cases {
        if case.case_id.trim().is_empty() || !case_ids.insert(case.case_id.as_str()) {
            return Err(format!("empty or duplicate case_id: {}", case.case_id));
        }
        if !matches!(
            case.expected_task_status.as_str(),
            "completed" | "needs_input" | "failed"
        ) {
            return Err(format!(
                "case {} has invalid expected_task_status",
                case.case_id
            ));
        }
        if case.min_citations > case.max_citations {
            return Err(format!(
                "case {} has an invalid citation range",
                case.case_id
            ));
        }
        for document_id in case
            .focus_document_ids
            .iter()
            .chain(case.allowed_document_ids.iter())
            .chain(case.required_document_ids.iter())
        {
            if !document_ids.contains(document_id.as_str()) {
                return Err(format!(
                    "case {} references unknown document_id: {}",
                    case.case_id, document_id
                ));
            }
        }
        if case
            .required_document_ids
            .iter()
            .any(|id| !case.allowed_document_ids.contains(id))
        {
            return Err(format!(
                "case {} requires evidence outside its whitelist",
                case.case_id
            ));
        }
        if case.exact_document {
            if case.focus_document_ids.is_empty() {
                return Err(format!(
                    "case {} is exact_document but has an empty focus set",
                    case.case_id
                ));
            }
            let focus = case.focus_document_ids.iter().collect::<HashSet<_>>();
            let allowed = case.allowed_document_ids.iter().collect::<HashSet<_>>();
            if focus != allowed {
                return Err(format!(
                    "case {} exact-document focus and whitelist must match",
                    case.case_id
                ));
            }
        }
    }
    Ok(())
}

pub fn run_suite(
    suite: &RagQualitySuite,
    source_revision: impl Into<String>,
) -> Result<RagQualityReport, String> {
    validate_suite(suite)?;
    let suite_digest_sha256 = suite_digest_sha256(suite)?;
    let workspace = EvaluationWorkspace::create()?;
    let path_by_document_id = write_corpus(&workspace.knowledge_root, &suite.files)?;
    let document_id_by_path = path_by_document_id
        .iter()
        .map(|(id, path)| (normalized_path(path), id.clone()))
        .collect::<HashMap<_, _>>();

    let admin_store = AdminConsoleStore::new(
        workspace.admin_path.clone(),
        DeviceRegistryStore::new(workspace.registry_path.clone()),
    );
    admin_store.save_knowledge_settings(KnowledgeSettings {
        source_roots: vec![KnowledgeSourceRoot {
            root_id: "rag-quality-v1".to_string(),
            label: "RAG quality evaluation corpus".to_string(),
            path: workspace.knowledge_root.to_string_lossy().into_owned(),
            enabled: true,
            include: Vec::new(),
            exclude: Vec::new(),
            last_indexed_at: None,
        }],
        index_root: workspace.index_root.to_string_lossy().into_owned(),
        privacy_level: PrivacyLevel::StrictLocal,
        default_resource_profile: RagResourceProfile::CpuOnly,
        retrieval: Default::default(),
        conversation: Default::default(),
    })?;
    let index_started = Instant::now();
    KnowledgeIndexService::from_config(KnowledgeIndexConfig::new(workspace.index_root.clone())?)?
        .load_or_refresh(&workspace.knowledge_root)?;
    let index_prepare_latency_ms = index_started.elapsed().as_millis();
    let service = TaskApiService::new(
        admin_store,
        TaskConversationStore::new(workspace.conversation_path.clone()),
    );

    let mut observations = Vec::with_capacity(suite.cases.len());
    let mut case_results = Vec::with_capacity(suite.cases.len());
    let mut cold_total_latency_ms = 0u128;
    let mut warm_total_latency_ms = 0u128;
    let mut max_process_rss_kib = process_rss_kib();
    for case in &suite.cases {
        let cold_request = build_request(
            case,
            &workspace.knowledge_root,
            &path_by_document_id,
            "cold",
        )?;
        let cold_started = Instant::now();
        let cold_response = service.handle_task(cold_request.clone());
        let cold_latency_ms = cold_started.elapsed().as_millis();
        cold_total_latency_ms += cold_latency_ms;

        let idempotent_replay = matches!(
            service.accept_or_replay_task(&cold_request),
            Ok(TaskRequestAcceptance::Replay(ref replay))
                if replay.task_id == cold_response.task_id
                    && response_fingerprint(replay, &document_id_by_path)
                    == response_fingerprint(&cold_response, &document_id_by_path)
        );

        let warm_request = build_request(
            case,
            &workspace.knowledge_root,
            &path_by_document_id,
            "warm",
        )?;
        let warm_started = Instant::now();
        let warm_response = service.handle_task(warm_request);
        let warm_latency_ms = warm_started.elapsed().as_millis();
        warm_total_latency_ms += warm_latency_ms;
        max_process_rss_kib = max_optional(max_process_rss_kib, process_rss_kib());

        let analyzed = analyze_case(
            case,
            &cold_response,
            &warm_response,
            &document_id_by_path,
            cold_latency_ms,
            warm_latency_ms,
            idempotent_replay,
        );
        observations.push(analyzed.observation);
        case_results.push(analyzed.result);
    }

    let (metrics, mut gate_reasons) = evaluate_observations(&observations, &suite.thresholds);
    for result in &case_results {
        for reason in &result.reasons {
            gate_reasons.push(format!("case {}: {reason}", result.case_id));
        }
    }
    gate_reasons.sort();
    gate_reasons.dedup();
    Ok(RagQualityReport {
        schema_version: RAG_QUALITY_SCHEMA_VERSION,
        contract: RAG_QUALITY_CONTRACT.to_string(),
        suite_id: suite.suite_id.clone(),
        suite_digest_sha256,
        source_revision: source_revision.into(),
        generated_at_unix_ms: now_unix_ms(),
        thresholds: suite.thresholds.clone(),
        metrics,
        resources: RagQualityResourceSummary {
            concurrency: 1,
            max_queue_depth: 0,
            index_prepare_latency_ms,
            cold_total_latency_ms,
            warm_total_latency_ms,
            max_process_rss_kib,
        },
        cases: case_results,
        gate: RagQualityGate {
            passed: gate_reasons.is_empty(),
            reasons: gate_reasons,
        },
    })
}

pub fn write_report(path: &Path, report: &RagQualityReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize report: {error}"))?;
    fs::write(path, format!("{contents}\n"))
        .map_err(|error| format!("failed to write report {}: {error}", path.display()))
}

pub fn evaluate_observations(
    observations: &[RagQualityObservation],
    thresholds: &RagQualityThresholds,
) -> (RagQualityMetrics, Vec<String>) {
    let case_count = observations.len();
    let schema_validity_rate = rate(
        observations.iter().filter(|item| item.schema_valid).count(),
        case_count,
    );
    let citation_count = observations
        .iter()
        .map(|item| item.citation_document_ids.len())
        .sum::<usize>();
    let whitelisted_citation_count = observations
        .iter()
        .map(|item| {
            item.citation_document_ids
                .iter()
                .filter(|id| item.allowed_document_ids.contains(id))
                .count()
        })
        .sum::<usize>();
    let citation_whitelist_rate = rate(whitelisted_citation_count, citation_count);
    let supported_citation_count = observations
        .iter()
        .map(|item| item.supported_citation_count)
        .sum::<usize>();
    let citation_support_precision = rate(supported_citation_count, citation_count);
    let required_evidence_count = observations
        .iter()
        .map(|item| item.required_document_ids.len())
        .sum::<usize>();
    let covered_required_evidence_count = observations
        .iter()
        .map(|item| {
            item.required_document_ids
                .iter()
                .filter(|id| item.citation_document_ids.contains(id))
                .count()
        })
        .sum::<usize>();
    let required_evidence_coverage = rate(covered_required_evidence_count, required_evidence_count);
    let required_claim_count = observations
        .iter()
        .map(|item| item.required_claim_count)
        .sum::<usize>();
    let supported_claim_count = observations
        .iter()
        .map(|item| item.supported_claim_count)
        .sum::<usize>();
    let claim_coverage_rate = rate(supported_claim_count, required_claim_count);
    let fallback_cases = observations
        .iter()
        .filter(|item| item.fallback_expected)
        .collect::<Vec<_>>();
    let fallback_preservation_rate = rate(
        fallback_cases
            .iter()
            .filter(|item| item.fallback_preserved)
            .count(),
        fallback_cases.len(),
    );
    let cold_warm_consistency_rate = rate(
        observations
            .iter()
            .filter(|item| item.cold_warm_consistent)
            .count(),
        case_count,
    );
    let idempotent_replay_rate = rate(
        observations
            .iter()
            .filter(|item| item.idempotent_replay)
            .count(),
        case_count,
    );
    let exact_document_violations = observations
        .iter()
        .filter(|item| {
            item.exact_document
                && item
                    .citation_document_ids
                    .iter()
                    .any(|id| !item.allowed_document_ids.contains(id))
        })
        .count();
    let duplicate_citation_violations = observations
        .iter()
        .map(|item| item.duplicate_citation_count)
        .sum();
    let metrics = RagQualityMetrics {
        case_count,
        schema_validity_rate,
        citation_whitelist_rate,
        citation_support_precision,
        required_evidence_coverage,
        claim_coverage_rate,
        fallback_preservation_rate,
        cold_warm_consistency_rate,
        idempotent_replay_rate,
        exact_document_violations,
        duplicate_citation_violations,
    };
    let mut reasons = Vec::new();
    minimum_reason(
        &mut reasons,
        "schema_validity_rate",
        metrics.schema_validity_rate,
        thresholds.min_schema_validity_rate,
    );
    minimum_reason(
        &mut reasons,
        "citation_whitelist_rate",
        metrics.citation_whitelist_rate,
        thresholds.min_citation_whitelist_rate,
    );
    minimum_reason(
        &mut reasons,
        "citation_support_precision",
        metrics.citation_support_precision,
        thresholds.min_citation_support_precision,
    );
    minimum_reason(
        &mut reasons,
        "required_evidence_coverage",
        metrics.required_evidence_coverage,
        thresholds.min_required_evidence_coverage,
    );
    minimum_reason(
        &mut reasons,
        "claim_coverage_rate",
        metrics.claim_coverage_rate,
        thresholds.min_claim_coverage_rate,
    );
    minimum_reason(
        &mut reasons,
        "fallback_preservation_rate",
        metrics.fallback_preservation_rate,
        thresholds.min_fallback_preservation_rate,
    );
    minimum_reason(
        &mut reasons,
        "cold_warm_consistency_rate",
        metrics.cold_warm_consistency_rate,
        thresholds.min_cold_warm_consistency_rate,
    );
    minimum_reason(
        &mut reasons,
        "idempotent_replay_rate",
        metrics.idempotent_replay_rate,
        thresholds.min_idempotent_replay_rate,
    );
    if metrics.exact_document_violations > thresholds.max_exact_document_violations {
        reasons.push(format!(
            "exact_document_violations={} exceeds maximum {}",
            metrics.exact_document_violations, thresholds.max_exact_document_violations
        ));
    }
    if metrics.duplicate_citation_violations > thresholds.max_duplicate_citation_violations {
        reasons.push(format!(
            "duplicate_citation_violations={} exceeds maximum {}",
            metrics.duplicate_citation_violations, thresholds.max_duplicate_citation_violations
        ));
    }
    (metrics, reasons)
}

struct AnalyzedCase {
    observation: RagQualityObservation,
    result: RagQualityCaseResult,
}

fn analyze_case(
    case: &RagQualityCase,
    cold_response: &crate::runtime::task_api::TaskResponse,
    warm_response: &crate::runtime::task_api::TaskResponse,
    document_id_by_path: &HashMap<String, String>,
    cold_latency_ms: u128,
    warm_latency_ms: u128,
    idempotent_replay: bool,
) -> AnalyzedCase {
    let answer = warm_response.result.data["answer"]
        .as_str()
        .unwrap_or(warm_response.result.message.as_str())
        .to_string();
    let citation_paths = warm_response.result.data["citations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|citation| citation["path"].as_str().map(ToString::to_string))
        .collect::<Vec<_>>();
    let citation_document_ids = citation_paths
        .iter()
        .map(|path| {
            document_id_by_path
                .get(&normalized_path(Path::new(path)))
                .cloned()
                .unwrap_or_else(|| format!("unmapped:{path}"))
        })
        .collect::<Vec<_>>();
    let supported_citation_count = citation_document_ids
        .iter()
        .filter(|id| case.allowed_document_ids.contains(id))
        .count();
    let supported_claim_count = case
        .required_answer_terms
        .iter()
        .filter(|term| answer.contains(term.as_str()))
        .count();
    let duplicate_citation_count =
        citation_document_ids.len() - citation_document_ids.iter().collect::<HashSet<_>>().len();
    let response_status = warm_response.result.data["status"]
        .as_str()
        .map(ToString::to_string);
    let degraded_reason = warm_response.result.data["degraded_reason"]
        .as_str()
        .map(ToString::to_string);
    let schema_valid = warm_response.result.data["kind"] == "rag.answer"
        && response_status.is_some()
        && warm_response.result.data["degraded"].is_boolean()
        && warm_response.result.data["citations"].is_array();
    let cold_warm_consistent = response_fingerprint(cold_response, document_id_by_path)
        == response_fingerprint(warm_response, document_id_by_path);
    let fallback_preserved = !case.fallback_expected
        || (warm_response.result.data["degraded"] == true
            && degraded_reason == case.expected_degraded_reason
            && citation_document_ids.len() <= case.max_citations
            && case
                .forbidden_answer_terms
                .iter()
                .all(|term| !answer.contains(term)));

    let mut reasons = Vec::new();
    let task_status = task_status_as_str(warm_response.status).to_string();
    if task_status != case.expected_task_status {
        reasons.push(format!(
            "task_status={task_status}, expected {}",
            case.expected_task_status
        ));
    }
    if degraded_reason != case.expected_degraded_reason {
        reasons.push(format!(
            "degraded_reason={degraded_reason:?}, expected {:?}",
            case.expected_degraded_reason
        ));
    }
    if citation_document_ids.len() < case.min_citations
        || citation_document_ids.len() > case.max_citations
    {
        reasons.push(format!(
            "citation_count={} outside {}..={}",
            citation_document_ids.len(),
            case.min_citations,
            case.max_citations
        ));
    }
    for document_id in &citation_document_ids {
        if !case.allowed_document_ids.contains(document_id) {
            reasons.push(format!("citation is outside the whitelist: {document_id}"));
        }
    }
    for document_id in &case.required_document_ids {
        if !citation_document_ids.contains(document_id) {
            reasons.push(format!("required evidence is missing: {document_id}"));
        }
    }
    if duplicate_citation_count > 0 {
        reasons.push(format!(
            "duplicate citation count is {duplicate_citation_count}"
        ));
    }
    for term in &case.required_answer_terms {
        if !answer.contains(term) {
            reasons.push(format!("answer is missing required term: {term}"));
        }
    }
    for term in &case.forbidden_answer_terms {
        if answer.contains(term) {
            reasons.push(format!("answer contains forbidden term: {term}"));
        }
    }
    if !schema_valid {
        reasons.push("response does not satisfy the rag.answer schema guard".to_string());
    }
    if !cold_warm_consistent {
        reasons.push("cold and warm response fingerprints differ".to_string());
    }
    if !idempotent_replay {
        reasons.push("duplicate request was not replayed identically".to_string());
    }
    if !fallback_preserved {
        reasons.push("fallback contract was not preserved".to_string());
    }

    AnalyzedCase {
        observation: RagQualityObservation {
            case_id: case.case_id.clone(),
            schema_valid,
            citation_document_ids: citation_document_ids.clone(),
            allowed_document_ids: case.allowed_document_ids.clone(),
            required_document_ids: case.required_document_ids.clone(),
            supported_citation_count,
            required_claim_count: case.required_answer_terms.len(),
            supported_claim_count,
            duplicate_citation_count,
            exact_document: case.exact_document,
            fallback_expected: case.fallback_expected,
            fallback_preserved,
            cold_warm_consistent,
            idempotent_replay,
        },
        result: RagQualityCaseResult {
            case_id: case.case_id.clone(),
            passed: reasons.is_empty(),
            reasons,
            task_status,
            response_status,
            degraded_reason,
            answer,
            citation_document_ids,
            citation_paths,
            cold_latency_ms,
            warm_latency_ms,
            cold_warm_consistent,
            idempotent_replay,
        },
    }
}

fn build_request(
    case: &RagQualityCase,
    knowledge_root: &Path,
    path_by_document_id: &HashMap<String, PathBuf>,
    phase: &str,
) -> Result<TaskRequest, String> {
    let focus_paths = case
        .focus_document_ids
        .iter()
        .map(|id| {
            path_by_document_id
                .get(id)
                .map(|path| path.to_string_lossy().into_owned())
                .ok_or_else(|| format!("unknown focus document_id: {id}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let task_id = format!("rag-quality-{}-{phase}", case.case_id);
    Ok(TaskRequest {
        task_id: task_id.clone(),
        trace_id: task_id.clone(),
        step_id: "evaluate".to_string(),
        source: TaskSource {
            user_id: "rag-quality-evaluator".to_string(),
            ..Default::default()
        },
        intent: TaskIntent {
            domain: "rag".to_string(),
            action: "answer".to_string(),
            raw_text: case.query.clone(),
        },
        entity_refs: Value::Null,
        args: json!({
            "query": case.query,
            "roots": [knowledge_root.to_string_lossy().to_string()],
            "focus_paths": focus_paths,
            "limit": case.max_citations.max(1),
            "include_documents": case.include_documents,
            "include_images": case.include_images,
            "include_videos": case.include_videos,
            "require_embeddings": false,
            "resource_profile": "cpu_only",
            "privacy_level": "strict_local"
        }),
        autonomy: Default::default(),
        message: None,
    })
}

fn response_fingerprint(
    response: &crate::runtime::task_api::TaskResponse,
    document_id_by_path: &HashMap<String, String>,
) -> Value {
    let citation_ids = response.result.data["citations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|citation| citation["path"].as_str())
        .map(|path| {
            document_id_by_path
                .get(&normalized_path(Path::new(path)))
                .cloned()
                .unwrap_or_else(|| format!("unmapped:{path}"))
        })
        .collect::<Vec<_>>();
    json!({
        "task_status": task_status_as_str(response.status),
        "executor_used": response.executor_used,
        "response_status": response.result.data["status"],
        "degraded": response.result.data["degraded"],
        "degraded_reason": response.result.data["degraded_reason"],
        "answer": response.result.data["answer"],
        "citations": citation_ids,
    })
}

fn write_corpus(
    knowledge_root: &Path,
    files: &[RagQualityFile],
) -> Result<HashMap<String, PathBuf>, String> {
    let mut result = HashMap::new();
    for file in files {
        let path = knowledge_root.join(Path::new(&file.relative_path));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        fs::write(&path, file.content.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        result.insert(file.document_id.clone(), path);
    }
    Ok(result)
}

fn validate_relative_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe relative_path: {value}"));
    }
    Ok(())
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn task_status_as_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Completed => "completed",
        TaskStatus::NeedsInput => "needs_input",
        TaskStatus::Failed => "failed",
    }
}

fn rate(passed: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        passed as f64 / total as f64
    }
}

fn minimum_reason(reasons: &mut Vec<String>, name: &str, actual: f64, minimum: f64) {
    if actual + f64::EPSILON < minimum {
        reasons.push(format!("{name}={actual:.4} is below minimum {minimum:.4}"));
    }
}

fn default_true() -> bool {
    true
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn suite_digest_sha256(suite: &RagQualitySuite) -> Result<String, String> {
    let canonical = serde_json::to_vec(suite)
        .map_err(|error| format!("failed to canonicalize evaluation suite: {error}"))?;
    let digest = Sha256::digest(canonical);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn process_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?.trim();
        value.split_whitespace().next()?.parse().ok()
    })
}

fn max_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

struct EvaluationWorkspace {
    root: PathBuf,
    knowledge_root: PathBuf,
    index_root: PathBuf,
    admin_path: PathBuf,
    registry_path: PathBuf,
    conversation_path: PathBuf,
}

impl EvaluationWorkspace {
    fn create() -> Result<Self, String> {
        let suffix = format!("{}-{}", std::process::id(), now_unix_ms());
        let root = std::env::temp_dir().join(format!("harborbeacon-rag-quality-{suffix}"));
        let knowledge_root = root.join("knowledge");
        let index_root = root.join("index");
        fs::create_dir_all(&knowledge_root)
            .map_err(|error| format!("failed to create {}: {error}", knowledge_root.display()))?;
        fs::create_dir_all(&index_root)
            .map_err(|error| format!("failed to create {}: {error}", index_root.display()))?;
        Ok(Self {
            admin_path: root.join("admin.json"),
            registry_path: root.join("registry.json"),
            conversation_path: root.join("conversation.json"),
            root,
            knowledge_root,
            index_root,
        })
    }
}

impl Drop for EvaluationWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_observation() -> RagQualityObservation {
        RagQualityObservation {
            case_id: "exact-red-cup".to_string(),
            schema_valid: true,
            citation_document_ids: vec!["red-cup".to_string()],
            allowed_document_ids: vec!["red-cup".to_string()],
            required_document_ids: vec!["red-cup".to_string()],
            supported_citation_count: 1,
            required_claim_count: 1,
            supported_claim_count: 1,
            duplicate_citation_count: 0,
            exact_document: true,
            fallback_expected: false,
            fallback_preserved: true,
            cold_warm_consistent: true,
            idempotent_replay: true,
        }
    }

    #[test]
    fn fixed_thresholds_require_perfect_safety_contracts() {
        let thresholds = RagQualityThresholds::default();

        assert_eq!(thresholds.min_schema_validity_rate, 1.0);
        assert_eq!(thresholds.min_citation_whitelist_rate, 1.0);
        assert_eq!(thresholds.min_citation_support_precision, 1.0);
        assert_eq!(thresholds.min_required_evidence_coverage, 1.0);
        assert_eq!(thresholds.min_claim_coverage_rate, 1.0);
        assert_eq!(thresholds.min_fallback_preservation_rate, 1.0);
        assert_eq!(thresholds.min_cold_warm_consistency_rate, 1.0);
        assert_eq!(thresholds.min_idempotent_replay_rate, 1.0);
        assert_eq!(thresholds.max_exact_document_violations, 0);
        assert_eq!(thresholds.max_duplicate_citation_violations, 0);
    }

    #[test]
    fn passing_observation_satisfies_the_fixed_gate() {
        let (metrics, reasons) =
            evaluate_observations(&[passing_observation()], &RagQualityThresholds::default());

        assert!(reasons.is_empty(), "unexpected gate reasons: {reasons:?}");
        assert_eq!(metrics.citation_whitelist_rate, 1.0);
        assert_eq!(metrics.required_evidence_coverage, 1.0);
        assert_eq!(metrics.exact_document_violations, 0);
    }

    #[test]
    fn cross_document_citation_blocks_the_gate() {
        let mut observation = passing_observation();
        observation
            .citation_document_ids
            .push("yellow-cup".to_string());

        let (metrics, reasons) =
            evaluate_observations(&[observation], &RagQualityThresholds::default());

        assert!(metrics.citation_whitelist_rate < 1.0);
        assert_eq!(metrics.exact_document_violations, 1);
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("citation_whitelist_rate")));
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("exact_document_violations")));
    }

    #[test]
    fn duplicate_citation_blocks_the_gate() {
        let mut observation = passing_observation();
        observation
            .citation_document_ids
            .push("red-cup".to_string());
        observation.supported_citation_count = 2;
        observation.duplicate_citation_count = 1;

        let (metrics, reasons) =
            evaluate_observations(&[observation], &RagQualityThresholds::default());

        assert_eq!(metrics.duplicate_citation_violations, 1);
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("duplicate_citation_violations")));
    }

    #[test]
    fn exact_document_case_rejects_an_empty_focus_set() {
        let suite = RagQualitySuite {
            schema_version: RAG_QUALITY_SCHEMA_VERSION,
            suite_id: "invalid-exact-document".to_string(),
            description: "invalid fixture".to_string(),
            thresholds: RagQualityThresholds::default(),
            files: vec![RagQualityFile {
                document_id: "red-cup".to_string(),
                relative_path: "docs/red-cup.md".to_string(),
                content: "red".to_string(),
            }],
            cases: vec![RagQualityCase {
                case_id: "empty-focus".to_string(),
                query: "red cup".to_string(),
                focus_document_ids: Vec::new(),
                allowed_document_ids: vec!["red-cup".to_string()],
                required_document_ids: Vec::new(),
                required_answer_terms: Vec::new(),
                forbidden_answer_terms: Vec::new(),
                expected_task_status: "completed".to_string(),
                expected_degraded_reason: None,
                min_citations: 0,
                max_citations: 1,
                exact_document: true,
                fallback_expected: false,
                include_documents: true,
                include_images: false,
                include_videos: false,
            }],
        };

        assert!(validate_suite(&suite)
            .expect_err("empty focus must fail")
            .contains("empty focus"));
    }

    #[test]
    fn versioned_suite_runs_through_the_real_task_api() {
        let suite_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rag-quality-v1/suite.json");
        let suite = load_suite(&suite_path).expect("load versioned suite");

        let report = run_suite(&suite, "test-revision").expect("run versioned suite");

        assert_eq!(report.metrics.case_count, 4);
        assert_eq!(report.resources.concurrency, 1);
        assert_eq!(report.resources.max_queue_depth, 0);
        assert_eq!(report.suite_digest_sha256.len(), 64);
        let red_cup = report
            .cases
            .iter()
            .find(|case| case.case_id == "exact-red-cup-color")
            .expect("red cup case");
        assert_eq!(red_cup.citation_document_ids, vec!["red-cup"]);
        assert!(!red_cup.answer.contains("颜色是黄色"));
    }
}
