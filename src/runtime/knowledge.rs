//! HarborBeacon-local knowledge retrieval over NAS-backed documents and images.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::control_plane::models::{
    ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
};
use crate::runtime::admin_console::{
    path_is_same_or_inside, AdminModelCenterState, KnowledgeRetrievalSettings, RagResourceProfile,
};
use crate::runtime::knowledge_index::{
    lexical_query_terms, load_embedding_store, KnowledgeEmbeddingStore, KnowledgeIndexChunk,
    KnowledgeIndexConfig, KnowledgeIndexEntry, KnowledgeIndexService, KnowledgeModality,
};
use crate::runtime::lexical_index::LexicalSearchScore;
use crate::runtime::{asr, model_center};

const DEFAULT_LIMIT: usize = 5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRetrievalStrategy {
    #[default]
    Semantic,
    Recent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeSearchRequest {
    pub query: String,
    pub rerank_query: Option<String>,
    pub configured_roots: Vec<String>,
    pub index_root: Option<String>,
    pub roots: Vec<String>,
    pub focus_paths: Vec<String>,
    pub include_documents: bool,
    pub include_images: bool,
    pub include_videos: bool,
    pub limit: usize,
    pub privacy_level: PrivacyLevel,
    pub resource_profile: RagResourceProfile,
    pub retrieval: KnowledgeRetrievalSettings,
    pub require_embeddings: bool,
    pub latency_budget_ms: Option<u64>,
    pub strategy: KnowledgeRetrievalStrategy,
    pub per_modality_limit: Option<usize>,
}

impl KnowledgeSearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            rerank_query: None,
            configured_roots: Vec::new(),
            index_root: None,
            roots: Vec::new(),
            focus_paths: Vec::new(),
            include_documents: true,
            include_images: true,
            include_videos: false,
            limit: DEFAULT_LIMIT,
            privacy_level: PrivacyLevel::StrictLocal,
            resource_profile: RagResourceProfile::CpuOnly,
            retrieval: KnowledgeRetrievalSettings::default(),
            require_embeddings: false,
            latency_budget_ms: None,
            strategy: KnowledgeRetrievalStrategy::Semantic,
            per_modality_limit: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeSearchHit {
    pub modality: String,
    pub path: String,
    pub title: String,
    pub score: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_child_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default)]
    pub snippet: Option<String>,
    /// Full indexed chunk kept only while assembling the answer prompt.  The
    /// public search response continues to expose the short `snippet`.
    #[serde(default, skip_serializing, skip_deserializing)]
    pub answer_context: String,
    #[serde(default)]
    pub matched_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default)]
    pub content_source_kinds: Vec<String>,
    #[serde(default)]
    pub content_indexed: bool,
    #[serde(default)]
    pub filename_match_used: bool,
    #[serde(default)]
    pub content_match_used: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_millis: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeSearchCitation {
    pub title: String,
    pub path: String,
    pub modality: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_child_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default)]
    pub matched_terms: Vec<String>,
    #[serde(default)]
    pub preview: Option<String>,
    /// Full indexed chunk for the answer model.  This is deliberately
    /// internal-only so the API and WebUI keep returning a compact preview.
    #[serde(default, skip_serializing, skip_deserializing)]
    pub answer_context: String,
    pub score: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_millis: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct KnowledgeSearchReplyPack {
    pub summary: String,
    #[serde(default)]
    pub citations: Vec<KnowledgeSearchCitation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeSearchResponse {
    pub query: String,
    pub roots: Vec<String>,
    pub total_matches: usize,
    #[serde(default)]
    pub documents: Vec<KnowledgeSearchHit>,
    #[serde(default)]
    pub images: Vec<KnowledgeSearchHit>,
    #[serde(default)]
    pub videos: Vec<KnowledgeSearchHit>,
    #[serde(default)]
    pub reply_pack: KnowledgeSearchReplyPack,
    #[serde(default)]
    pub supported_modalities: Vec<String>,
    #[serde(default)]
    pub pending_modalities: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub source_scope: Vec<String>,
    #[serde(default)]
    pub privacy_level: String,
    #[serde(default)]
    pub resource_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_guidance: Option<String>,
}

impl KnowledgeSearchResponse {
    pub fn degraded(
        query: impl Into<String>,
        roots: Vec<String>,
        privacy_level: PrivacyLevel,
        resource_profile: RagResourceProfile,
        reason: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let query = query.into();
        let roots = normalize_scope_strings(roots);
        let reason = reason.into();
        let message = message.into();
        let (supported_modalities, pending_modalities) = modality_support_matrix();
        Self {
            query,
            roots: roots.clone(),
            total_matches: 0,
            documents: Vec::new(),
            images: Vec::new(),
            videos: Vec::new(),
            reply_pack: KnowledgeSearchReplyPack {
                summary: message.clone(),
                citations: Vec::new(),
            },
            supported_modalities,
            pending_modalities,
            status: "degraded".to_string(),
            degraded: true,
            degraded_reason: Some(reason),
            blockers: vec![message],
            warnings: Vec::new(),
            source_scope: roots,
            privacy_level: privacy_level_as_str(privacy_level).to_string(),
            resource_profile: resource_profile.as_str().to_string(),
            empty_reason: None,
            empty_guidance: None,
        }
    }
}

pub struct KnowledgeSearchService;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SearchDiagnostics {
    indexed_videos: usize,
    indexed_video_content: usize,
    video_content_source_kinds: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KnowledgeEmptyState {
    reason: String,
    guidance: String,
}

#[derive(Debug, Clone)]
struct SearchCandidate {
    hit: KnowledgeSearchHit,
    embedding_text: String,
    semantic_only: bool,
    rrf_score: f32,
    final_score: f32,
}

impl KnowledgeSearchService {
    pub fn search(request: KnowledgeSearchRequest) -> Result<KnowledgeSearchResponse, String> {
        let query = request.query.trim().to_string();
        if query.is_empty() {
            return Err(
                "缺少知识库检索关键词，请提供 query 或更明确的自然语言检索请求。".to_string(),
            );
        }
        if !request.include_documents && !request.include_images && !request.include_videos {
            return Ok(KnowledgeSearchResponse::degraded(
                query,
                Vec::new(),
                request.privacy_level,
                request.resource_profile,
                "unsupported_modalities",
                "当前检索请求没有启用可支持的模态，至少需要文档、图片或视频之一。",
            ));
        }

        if let Some(blocker) = request_policy_blocker(&request) {
            return Ok(KnowledgeSearchResponse::degraded(
                query,
                Vec::new(),
                request.privacy_level,
                request.resource_profile,
                "blocked_resource_profile",
                blocker,
            ));
        }

        let roots = match resolve_roots(&request.configured_roots, &request.roots) {
            Ok(roots) => roots,
            Err(error) => {
                return Ok(KnowledgeSearchResponse::degraded(
                    query,
                    Vec::new(),
                    request.privacy_level,
                    request.resource_profile,
                    "source_scope_blocked",
                    error,
                ))
            }
        };
        let root_strings = roots
            .iter()
            .map(|path| normalize_search_path_text(path.to_string_lossy().as_ref()))
            .collect::<Vec<_>>();
        let focus_paths = match resolve_focus_paths(&request.focus_paths, &roots) {
            Ok(paths) => paths,
            Err(error) => {
                return Ok(KnowledgeSearchResponse::degraded(
                    query,
                    root_strings,
                    request.privacy_level,
                    request.resource_profile,
                    "focus_scope_blocked",
                    error,
                ))
            }
        };
        let query_terms = lexical_query_terms(&query);
        let index_service = match knowledge_index_service(request.index_root.as_deref()) {
            Ok(service) => service,
            Err(error) => {
                return Ok(KnowledgeSearchResponse::degraded(
                    query,
                    root_strings,
                    request.privacy_level,
                    request.resource_profile,
                    "index_root_unavailable",
                    error,
                ))
            }
        };
        let model_center_state = model_center::load_model_center_state();
        if let Some(blocker) = resource_profile_runtime_blocker(
            request.resource_profile,
            request.privacy_level,
            &model_center_state,
        ) {
            return Ok(KnowledgeSearchResponse::degraded(
                query,
                root_strings,
                request.privacy_level,
                request.resource_profile,
                "blocked_resource_profile",
                blocker,
            ));
        }
        let retrieval = request.retrieval.clone();
        let metadata_recent = request.strategy == KnowledgeRetrievalStrategy::Recent;
        let embedding_requested = !metadata_recent
            && (request.require_embeddings || retrieval.vector_weight > f32::EPSILON);
        let query_embedding = embedding_requested
            .then(|| model_center::run_query_embedding_with_state(&query, &model_center_state));
        let query_embedding_vector = query_embedding.as_ref().and_then(|execution| {
            (!execution.vector.is_empty() && execution.available)
                .then_some(execution.vector.clone())
        });
        if request.require_embeddings && !metadata_recent && query_embedding_vector.is_none() {
            return Ok(KnowledgeSearchResponse::degraded(
                query,
                root_strings,
                request.privacy_level,
                request.resource_profile,
                "embedding_unavailable",
                format!(
                    "当前检索要求 embedding，但 embedding 模型不可用：{}",
                    query_embedding
                        .as_ref()
                        .map(|execution| execution.summary.as_str())
                        .unwrap_or("embedding retrieval is disabled")
                ),
            ));
        }
        let mut warnings = Vec::new();
        let embedding_model_degraded = embedding_requested && query_embedding_vector.is_none();
        if embedding_model_degraded {
            warnings.push(format!(
                "Embedding 模型不可用，已降级为本地词法检索：{}",
                query_embedding
                    .as_ref()
                    .map(|execution| execution.summary.as_str())
                    .unwrap_or("embedding retrieval is disabled")
            ));
        }
        let mut seen_hits = HashSet::new();
        let mut candidates = Vec::new();
        let mut diagnostics = SearchDiagnostics::default();
        let mut required_embedding_total = 0usize;
        let mut required_embedding_missing = 0usize;
        for root in &roots {
            let snapshot = match index_service.load_existing(root) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return Ok(KnowledgeSearchResponse::degraded(
                        query,
                        root_strings,
                        request.privacy_level,
                        request.resource_profile,
                        "index_manifest_unavailable",
                        error,
                    ))
                }
            };
            for entry in &snapshot.manifest.entries {
                diagnostics.observe_entry(entry);
            }
            let embedding_store_path = index_service.embedding_store_path_for_root(root);
            let lexical_scores = if metadata_recent {
                None
            } else {
                match index_service.search_lexical(&snapshot, &query_terms) {
                    Ok(scores) => Some(scores),
                    Err(error) => {
                        warnings.push(format!(
                            "BM25 倒排索引不可用，已临时回退为简单词法评分；请刷新知识索引：{error}"
                        ));
                        None
                    }
                }
            };
            let embedding_store = if query_embedding_vector.is_some() {
                match load_embedding_store(&embedding_store_path) {
                    Ok(store)
                        if embedding_store_matches_query(
                            &store,
                            query_embedding
                                .as_ref()
                                .expect("query embedding exists when its vector is available"),
                        ) =>
                    {
                        store
                    }
                    Ok(store) => {
                        if !store.entries.is_empty() {
                            warnings.push(format!(
                                "Embedding cache 与当前端点或模型不一致，已跳过旧向量；请刷新知识索引：{}",
                                embedding_store_identity_summary(&store)
                            ));
                        }
                        KnowledgeEmbeddingStore::default()
                    }
                    Err(error) => {
                        warnings.push(format!(
                            "Embedding cache 读取失败，已继续使用词法分数：{error}"
                        ));
                        KnowledgeEmbeddingStore::default()
                    }
                }
            } else {
                KnowledgeEmbeddingStore::default()
            };
            let required_embedding_keys = lexical_scores
                .as_ref()
                .map(|scores| scores.keys().cloned().collect::<HashSet<_>>())
                .unwrap_or_default();
            let minimum_vector_score = retrieval
                .vector_min_score
                .min(retrieval.semantic_only_min_score);
            let embedding_scores = if let Some(query_vector) = query_embedding_vector.as_deref() {
                match index_service.search_embeddings(
                    &embedding_store_path,
                    &embedding_store,
                    query_vector,
                    minimum_vector_score,
                    &required_embedding_keys,
                ) {
                    Ok(scores) => {
                        let (required_count, missing_count) =
                            required_embedding_coverage(&required_embedding_keys, &scores);
                        required_embedding_total += required_count;
                        required_embedding_missing += missing_count;
                        scores
                    }
                    Err(error) => {
                        warnings.push(format!("磁盘向量索引读取失败，已继续使用词法分数：{error}"));
                        HashMap::new()
                    }
                }
            } else {
                HashMap::new()
            };
            let selected_payload_paths = if metadata_recent {
                Some(recent_entry_payload_paths(
                    &snapshot.manifest.entries,
                    &request,
                    &focus_paths,
                    retrieval.candidate_limit.max(request.limit).max(1),
                ))
            } else if lexical_scores.is_some() {
                let mut paths = lexical_scores
                    .as_ref()
                    .into_iter()
                    .flat_map(|scores| scores.keys())
                    .filter_map(|key| lexical_key_path(key))
                    .collect::<HashSet<_>>();
                if query_embedding_vector.is_some() {
                    for key in embedding_scores.keys() {
                        if let Some(path) = lexical_key_path(key) {
                            paths.insert(path);
                        }
                    }
                }
                Some(paths)
            } else {
                None
            };
            let payload_entries = match index_service
                .load_entry_payloads(&snapshot, selected_payload_paths.as_ref())
            {
                Ok(entries) => entries,
                Err(error) => {
                    return Ok(KnowledgeSearchResponse::degraded(
                        query,
                        root_strings,
                        request.privacy_level,
                        request.resource_profile,
                        "index_payload_unavailable",
                        error,
                    ))
                }
            };
            for entry in &payload_entries {
                if !focus_paths.is_empty() && !entry_matches_focus_paths(&entry.path, &focus_paths)
                {
                    continue;
                }
                if !modality_included(entry.modality, &request) {
                    continue;
                }
                let entry_candidates = build_hit_candidates_from_index_entry(
                    entry,
                    &query_terms,
                    lexical_scores.as_ref(),
                    query_embedding_vector.is_none() && lexical_scores.is_some(),
                    metadata_recent,
                );
                for mut candidate in entry_candidates {
                    let embedding_key =
                        embedding_key(&candidate.hit.path, candidate.hit.chunk_id.as_deref());
                    let embedding_score = query_embedding_vector
                        .as_ref()
                        .and_then(|_| embedding_scores.get(&embedding_key).copied());
                    candidate.hit.embedding_score = embedding_score;
                    if !metadata_recent && !candidate_has_retrieval_signal(&candidate, &retrieval) {
                        continue;
                    }
                    let dedupe_key = (
                        candidate.hit.modality.clone(),
                        candidate.hit.path.clone(),
                        candidate.hit.chunk_id.clone().unwrap_or_default(),
                    );
                    if seen_hits.insert(dedupe_key) {
                        candidates.push(candidate);
                    }
                }
            }
        }

        if required_embedding_total > 0 && required_embedding_missing > 0 {
            warnings.push(format!(
                "Embedding cache 覆盖不足：{} / {} 个 BM25 候选在向量存储中不存在，已仅用现有向量参与混合召回。",
                required_embedding_missing, required_embedding_total
            ));
        }

        if metadata_recent {
            rank_recent_candidates(&mut candidates);
        } else {
            apply_rrf_scores(&mut candidates, &retrieval);
            sort_candidates_by_final_score(&mut candidates);
        }
        let total_matches = file_candidate_count(&candidates);
        candidates.truncate(retrieval.candidate_limit.max(1));
        let reranked_candidate_count = if retrieval.rerank_enabled && !metadata_recent {
            let rerank_query = request
                .rerank_query
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| rerank_query_for_search(&query));
            apply_local_rerank(
                &mut candidates,
                &rerank_query,
                &retrieval,
                &model_center_state,
                &mut warnings,
            )
        } else {
            None
        };
        if let Some(reranked_count) = reranked_candidate_count {
            candidates.truncate(reranked_count);
            candidates.retain(|candidate| candidate.hit.rerank_score.is_some());
        }
        if !metadata_recent {
            sort_candidates_by_final_score(&mut candidates);
        }
        candidates = collapse_sibling_candidates(candidates);
        candidates = collapse_file_candidates(candidates);

        let limit = request.limit.clamp(1, 50);
        let selected_candidates = if let Some(per_modality_limit) = request.per_modality_limit {
            select_candidates_with_modality_quotas(
                candidates,
                limit,
                per_modality_limit.max(1),
                retrieval.mmr_enabled && !metadata_recent,
                retrieval.mmr_lambda,
                metadata_recent,
            )
        } else if retrieval.mmr_enabled && !metadata_recent {
            apply_mmr(candidates, limit, retrieval.mmr_lambda)
        } else {
            candidates.into_iter().take(limit).collect::<Vec<_>>()
        };
        let ordered_hits = selected_candidates
            .iter()
            .map(|candidate| candidate.hit.clone())
            .collect::<Vec<_>>();
        let mut documents = Vec::new();
        let mut images = Vec::new();
        let mut videos = Vec::new();
        for hit in &ordered_hits {
            match hit.modality.as_str() {
                "document" => documents.push(hit.clone()),
                // Keep audio in the established file-result collection for API
                // compatibility while preserving `modality=audio` on the hit.
                "audio" => documents.push(hit.clone()),
                "image" => images.push(hit.clone()),
                "video" => videos.push(hit.clone()),
                _ => {}
            }
        }
        let empty_state = build_empty_state(
            &request,
            &roots,
            &diagnostics,
            &model_center_state,
            total_matches,
        );
        let reply_pack = build_reply_pack(
            &query,
            total_matches,
            &ordered_hits,
            &documents,
            &images,
            &videos,
            empty_state.as_ref(),
        );
        let (supported_modalities, pending_modalities) = modality_support_matrix();

        Ok(KnowledgeSearchResponse {
            query,
            roots: root_strings.clone(),
            total_matches,
            documents,
            images,
            videos,
            reply_pack,
            supported_modalities,
            pending_modalities,
            status: if embedding_model_degraded {
                "degraded".to_string()
            } else {
                "completed".to_string()
            },
            degraded: embedding_model_degraded,
            degraded_reason: embedding_model_degraded.then(|| "embedding_unavailable".to_string()),
            blockers: Vec::new(),
            warnings,
            source_scope: root_strings,
            privacy_level: privacy_level_as_str(request.privacy_level).to_string(),
            resource_profile: request.resource_profile.as_str().to_string(),
            empty_reason: empty_state.as_ref().map(|state| state.reason.clone()),
            empty_guidance: empty_state.as_ref().map(|state| state.guidance.clone()),
        })
    }
}

fn embedding_store_matches_query(
    store: &KnowledgeEmbeddingStore,
    query: &model_center::EmbeddingExecution,
) -> bool {
    if store.entries.is_empty() {
        return true;
    }
    store.provider_key.as_deref() == Some(query.provider_key.as_str())
        && store.model_endpoint_id.as_deref() == query.model_endpoint_id.as_deref()
        && store.model_name.as_deref() == query.model_name.as_deref()
        && store.vector_dimensions == Some(query.vector.len())
}

fn embedding_store_identity_summary(store: &KnowledgeEmbeddingStore) -> String {
    format!(
        "provider={}, endpoint={}, model={}, dimensions={}",
        store.provider_key.as_deref().unwrap_or("unknown"),
        store.model_endpoint_id.as_deref().unwrap_or("unknown"),
        store.model_name.as_deref().unwrap_or("unknown"),
        store
            .vector_dimensions
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )
}

fn request_policy_blocker(request: &KnowledgeSearchRequest) -> Option<String> {
    if request.resource_profile == RagResourceProfile::CloudAllowed
        && request.privacy_level == PrivacyLevel::StrictLocal
    {
        return Some(
            "resource_profile=cloud_allowed 与 workspace strict_local 隐私策略冲突；请先在 Harbor Assistant 明确启用可审计的云策略。"
                .to_string(),
        );
    }
    if let Some(budget) = request.latency_budget_ms {
        if budget == 0 {
            return Some("latency_budget_ms 必须大于 0，不能静默回退到无预算检索。".to_string());
        }
    }
    None
}

fn resource_profile_runtime_blocker(
    resource_profile: RagResourceProfile,
    privacy_level: PrivacyLevel,
    model_center_state: &AdminModelCenterState,
) -> Option<String> {
    match resource_profile {
        RagResourceProfile::CpuOnly | RagResourceProfile::LocalGpu => None,
        RagResourceProfile::SidecarGpu => {
            if endpoint_kind_available(model_center_state, ModelEndpointKind::Sidecar) {
                None
            } else {
                Some(
                    "resource_profile=sidecar_gpu 需要可用的 sidecar 模型端点；当前模型设置未通过 readiness。"
                        .to_string(),
                )
            }
        }
        RagResourceProfile::CloudAllowed => {
            if privacy_level == PrivacyLevel::StrictLocal {
                Some("resource_profile=cloud_allowed 与 strict_local 隐私策略冲突。".to_string())
            } else if endpoint_kind_available(model_center_state, ModelEndpointKind::Cloud) {
                None
            } else {
                Some(
                    "resource_profile=cloud_allowed 需要可用的 cloud 模型端点；当前模型设置未通过 readiness。"
                        .to_string(),
                )
            }
        }
    }
}

fn endpoint_kind_available(
    model_center_state: &AdminModelCenterState,
    endpoint_kind: ModelEndpointKind,
) -> bool {
    model_center_state.endpoints.iter().any(|endpoint| {
        endpoint.endpoint_kind == endpoint_kind
            && endpoint.status != ModelEndpointStatus::Disabled
            && matches!(
                endpoint.model_kind,
                ModelKind::Embedder | ModelKind::Llm | ModelKind::Ocr | ModelKind::Vlm
            )
    })
}

fn privacy_level_as_str(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::StrictLocal => "strict_local",
        PrivacyLevel::AllowRedactedCloud => "allow_redacted_cloud",
        PrivacyLevel::AllowCloud => "allow_cloud",
    }
}

fn normalize_scope_strings(mut roots: Vec<String>) -> Vec<String> {
    roots
        .iter_mut()
        .for_each(|root| *root = root.trim().to_string());
    roots.retain(|root| !root.is_empty());
    roots.sort();
    roots.dedup();
    roots
}

fn modality_support_matrix() -> (Vec<String>, Vec<String>) {
    let mut supported = vec![
        "document".to_string(),
        "image".to_string(),
        "video".to_string(),
        "ocr".to_string(),
    ];
    let mut pending = Vec::new();
    if asr::runtime_available() {
        supported.push("audio".to_string());
        supported.push("asr".to_string());
    } else {
        pending.push("audio".to_string());
        pending.push("asr".to_string());
    }

    let model_center_state = model_center::load_model_center_state();
    let embed_ready = model_center_state.endpoints.iter().any(|endpoint| {
        endpoint.model_kind == ModelKind::Embedder
            && endpoint.status != ModelEndpointStatus::Disabled
    });
    if embed_ready {
        supported.push("embedding".to_string());
        supported.push("hybrid_retrieval".to_string());
    } else {
        pending.push("embedding".to_string());
        pending.push("hybrid_retrieval".to_string());
    }
    let vlm_ready = model_vlm_ready(&model_center_state);
    if vlm_ready {
        supported.push("vlm".to_string());
        supported.push("vlm_keyframe".to_string());
    } else {
        pending.push("vlm".to_string());
        pending.push("vlm_keyframe".to_string());
    }

    (supported, pending)
}

fn model_vlm_ready(model_center_state: &AdminModelCenterState) -> bool {
    model_center_state.endpoints.iter().any(|endpoint| {
        endpoint.model_kind == ModelKind::Vlm && endpoint.status != ModelEndpointStatus::Disabled
    })
}

fn knowledge_index_service(index_root: Option<&str>) -> Result<KnowledgeIndexService, String> {
    let Some(index_root) = index_root.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }) else {
        return Err(
            "请先在 Harbor Assistant 配置 knowledge.index_root，再运行知识库检索。".to_string(),
        );
    };
    KnowledgeIndexService::from_config(KnowledgeIndexConfig::new(PathBuf::from(index_root))?)
}

impl SearchDiagnostics {
    fn observe_entry(&mut self, entry: &KnowledgeIndexEntry) {
        if entry.modality != KnowledgeModality::Video {
            return;
        }
        self.indexed_videos += 1;
        let mut entry_has_content = false;
        for source in &entry.text_sources {
            let source_kind = source.source_kind.trim().to_ascii_lowercase();
            if !source_kind.is_empty() {
                self.video_content_source_kinds.insert(source_kind);
            }
            if !source.text.trim().is_empty() {
                entry_has_content = true;
            }
        }
        if entry_has_content || !entry.searchable_text.trim().is_empty() {
            self.indexed_video_content += 1;
        }
    }
}

fn build_empty_state(
    request: &KnowledgeSearchRequest,
    roots: &[PathBuf],
    diagnostics: &SearchDiagnostics,
    model_center_state: &AdminModelCenterState,
    total_matches: usize,
) -> Option<KnowledgeEmptyState> {
    if total_matches > 0 || !request.include_videos {
        return None;
    }
    if request.include_documents || request.include_images {
        return None;
    }

    let video_files = count_video_files(roots);
    if video_files == 0 {
        return Some(KnowledgeEmptyState {
            reason: "no_video_files".to_string(),
            guidance: "已检索配置的知识源，但没有发现本地视频文件。请确认视频目录已加入知识源。"
                .to_string(),
        });
    }
    if diagnostics.indexed_videos == 0 {
        let reason = if model_vlm_ready(model_center_state) {
            "video_not_indexed"
        } else {
            "video_sidecar_or_vlm_unavailable"
        };
        return Some(KnowledgeEmptyState {
            reason: reason.to_string(),
            guidance: "已发现本地视频文件，但还没有可检索的视频内容索引。请刷新知识索引，并确认 sidecar 文本或 VLM keyframe 能力可用。".to_string(),
        });
    }
    if diagnostics.indexed_video_content == 0 {
        return Some(KnowledgeEmptyState {
            reason: "video_content_unavailable".to_string(),
            guidance: "视频文件已有索引记录，但没有 sidecar 或 keyframe 文本可用于内容检索。请补充 sidecar 或启用 VLM keyframe 索引。".to_string(),
        });
    }

    Some(KnowledgeEmptyState {
        reason: "video_content_no_match".to_string(),
        guidance:
            "视频内容索引可用，但没有命中当前关键词；可以换一个画面、人物、物品或事件描述再搜。"
                .to_string(),
    })
}

fn count_video_files(roots: &[PathBuf]) -> usize {
    roots.iter().map(|root| count_video_files_in(root)).sum()
}

fn count_video_files_in(path: &Path) -> usize {
    if path.is_file() {
        return usize::from(is_video_path(path));
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let child = entry.path();
            if child.is_dir() {
                count_video_files_in(&child)
            } else {
                usize::from(is_video_path(&child))
            }
        })
        .sum()
}

fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v"
            )
        })
        .unwrap_or(false)
}

fn normalize_search_path_text(value: &str) -> String {
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    value.strip_prefix(r"\\?\").unwrap_or(value).to_string()
}

fn resolve_roots(
    configured_roots: &[String],
    request_roots: &[String],
) -> Result<Vec<PathBuf>, String> {
    let configured = configured_roots
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    if configured.is_empty() {
        return Err("请先在 Harbor Assistant 配置并启用至少一个知识源目录。".to_string());
    }

    let requested = request_roots
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let raw_roots = if requested.is_empty() {
        configured.clone()
    } else {
        let mut allowed = Vec::new();
        for requested_root in requested {
            let inside_configured = configured
                .iter()
                .any(|configured_root| path_is_same_or_inside(&requested_root, configured_root));
            if !inside_configured {
                return Err(format!(
                    "请求的知识源目录未在 Harbor Assistant 启用，不能扩权检索：{requested_root}"
                ));
            }
            allowed.push(requested_root);
        }
        allowed
    };

    let mut roots = Vec::new();
    for root in raw_roots {
        let root = PathBuf::from(root);
        if root.as_os_str().is_empty() {
            continue;
        }
        if root.exists() {
            roots.push(root.canonicalize().unwrap_or(root));
        }
    }

    if roots.is_empty() {
        return Err(
            "未找到可检索的已配置知识源目录；请先通过 Harbor Assistant 配置并确认目录存在。"
                .to_string(),
        );
    }

    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn resolve_focus_paths(focus_paths: &[String], roots: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut resolved = Vec::new();
    for focus_path in focus_paths
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
    {
        let candidate = PathBuf::from(focus_path);
        if !candidate.exists() {
            return Err(format!("请求的知识检索 focus_path 不存在：{focus_path}"));
        }
        let canonical = candidate.canonicalize().unwrap_or(candidate);
        let canonical_string = normalize_search_path_text(canonical.to_string_lossy().as_ref());
        let inside_scope = roots.iter().any(|root| {
            let root_string = normalize_search_path_text(root.to_string_lossy().as_ref());
            path_is_same_or_inside(&canonical_string, &root_string)
        });
        if !inside_scope {
            return Err(format!(
                "请求的知识检索 focus_path 不在已启用知识源范围内：{focus_path}"
            ));
        }
        resolved.push(canonical_string);
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn entry_matches_focus_paths(entry_path: &str, focus_paths: &[String]) -> bool {
    let entry_path = normalize_search_path_text(entry_path);
    focus_paths
        .iter()
        .any(|focus_path| path_is_same_or_inside(&entry_path, focus_path))
}

/// Build a HarborBeacon-owned hit from an indexed entry, preserving the stable
/// response shape used by `TaskResponse.result.data`.
fn build_hit_candidates_from_index_entry(
    entry: &KnowledgeIndexEntry,
    query_terms: &[String],
    lexical_scores: Option<&HashMap<String, LexicalSearchScore>>,
    indexed_lexical_only: bool,
    metadata_recent: bool,
) -> Vec<SearchCandidate> {
    let path = Path::new(&entry.path);
    let chunks = if entry.chunks.is_empty() {
        vec![KnowledgeIndexChunk {
            chunk_id: "chunk-0001".to_string(),
            parent_id: None,
            previous_id: None,
            next_id: None,
            section_path: Vec::new(),
            line_start: 1,
            line_end: entry.searchable_text.lines().count().max(1),
            text: entry.searchable_text.clone(),
            indexed_text: entry.searchable_text.clone(),
            source_kind: entry.modality.as_str().to_string(),
            source_path: entry.sidecar_path.clone(),
        }]
    } else {
        entry.chunks.clone()
    };

    let candidates = chunks
        .iter()
        .filter_map(|chunk| {
            let indexed_text = if chunk.indexed_text.trim().is_empty() {
                chunk.text.as_str()
            } else {
                chunk.indexed_text.as_str()
            };
            let parent = chunk.parent_id.as_deref().and_then(|parent_id| {
                entry
                    .parent_chunks
                    .iter()
                    .find(|parent| parent.parent_id == parent_id)
            });
            let lexical_key = embedding_key(
                &normalize_search_path_text(&entry.path),
                Some(chunk.chunk_id.as_str()),
            );
            let lexical_score =
                lexical_scores.map(|scores| scores.get(&lexical_key).copied().unwrap_or_default());
            if indexed_lexical_only
                && lexical_score
                    .map(|score| score.normalized)
                    .unwrap_or_default()
                    <= f32::EPSILON
            {
                return None;
            }
            build_hit_candidate(
                path,
                entry.modality,
                Some(indexed_text),
                parent.map(|parent| parent.text.as_str()),
                query_terms,
                Some(chunk),
                parent.map(|parent| parent.line_start),
                parent.map(|parent| parent.line_end),
                Some(entry.file_signature.modified_unix_millis),
                lexical_score,
                metadata_recent,
            )
        })
        .collect::<Vec<_>>();
    if metadata_recent {
        candidates.into_iter().take(1).collect()
    } else {
        candidates
    }
}

fn build_reply_pack(
    query: &str,
    total_matches: usize,
    ordered_hits: &[KnowledgeSearchHit],
    documents: &[KnowledgeSearchHit],
    images: &[KnowledgeSearchHit],
    videos: &[KnowledgeSearchHit],
    empty_state: Option<&KnowledgeEmptyState>,
) -> KnowledgeSearchReplyPack {
    let citations = ordered_hits
        .iter()
        .map(|hit| KnowledgeSearchCitation {
            title: hit.title.clone(),
            path: hit.path.clone(),
            modality: hit.modality.clone(),
            chunk_id: hit.chunk_id.clone(),
            parent_id: hit.parent_id.clone(),
            matched_child_ids: hit.matched_child_ids.clone(),
            line_start: hit.line_start,
            line_end: hit.line_end,
            matched_terms: hit.matched_terms.clone(),
            preview: hit.snippet.clone(),
            answer_context: hit.answer_context.clone(),
            score: hit.score,
            lexical_score: hit.lexical_score,
            bm25_score: hit.bm25_score,
            embedding_score: hit.embedding_score,
            hybrid_score: hit.hybrid_score,
            rerank_score: hit.rerank_score,
            provenance: hit.provenance.clone(),
            source_path: hit.source_path.clone(),
            modified_unix_millis: hit.modified_unix_millis,
        })
        .collect::<Vec<_>>();
    let summary = build_reply_summary(query, total_matches, documents, images, videos, empty_state);
    KnowledgeSearchReplyPack { summary, citations }
}

fn build_reply_summary(
    query: &str,
    total_matches: usize,
    documents: &[KnowledgeSearchHit],
    images: &[KnowledgeSearchHit],
    videos: &[KnowledgeSearchHit],
    empty_state: Option<&KnowledgeEmptyState>,
) -> String {
    if total_matches == 0 {
        if let Some(empty_state) = empty_state {
            return format!(
                "已检索知识库，但没有找到与“{}”相关的视频内容。{}",
                query, empty_state.guidance
            );
        }
        return format!(
            "已检索知识库，但暂时没有找到与“{}”相关的文档、图片、音频、视频或 OCR 线索。",
            query
        );
    }

    let mut parts = Vec::new();
    let document_count = documents
        .iter()
        .filter(|hit| hit.modality == "document")
        .count();
    let audio_count = documents
        .iter()
        .filter(|hit| hit.modality == "audio")
        .count();
    if document_count > 0 {
        parts.push(format!("{document_count} 个文档片段"));
    }
    if audio_count > 0 {
        parts.push(format!("{audio_count} 个音频片段"));
    }
    if !images.is_empty() {
        parts.push(format!("{} 张图片", images.len()));
    }
    if !videos.is_empty() {
        parts.push(format!("{} 个视频片段", videos.len()));
    }
    let visible = documents.len() + images.len() + videos.len();
    if visible < total_matches {
        format!(
            "已找到与“{}”相关的 {}，当前展示 {} 条可引用结果。",
            query,
            parts.join("和"),
            visible
        )
    } else {
        format!("已找到与“{}”相关的 {}。", query, parts.join("和"))
    }
}

fn build_hit_candidate(
    path: &Path,
    modality: KnowledgeModality,
    searchable_text: Option<&str>,
    parent_context: Option<&str>,
    query_terms: &[String],
    chunk: Option<&KnowledgeIndexChunk>,
    context_line_start: Option<usize>,
    context_line_end: Option<usize>,
    modified_unix_millis: Option<u128>,
    indexed_lexical_score: Option<LexicalSearchScore>,
    metadata_recent: bool,
) -> Option<SearchCandidate> {
    let display_path = normalize_search_path_text(path.to_string_lossy().as_ref());
    let title = path
        .file_name()
        .and_then(|item| item.to_str())
        .unwrap_or(display_path.as_str())
        .to_string();
    let path_lower = display_path.to_lowercase();
    let title_lower = title.to_lowercase();
    let searchable_lower = searchable_text.map(str::to_lowercase);
    let allow_name_match = matches!(
        modality,
        KnowledgeModality::Document | KnowledgeModality::Audio
    );

    let mut score = 0;
    let mut filename_match_used = false;
    let mut content_match_used = false;
    let mut matched_terms = Vec::new();
    let content_source_kinds = content_source_kinds_for_chunk(chunk);
    let content_derived_source = is_content_derived_source(modality, &content_source_kinds);
    for term in query_terms {
        let normalized = term.to_lowercase();
        let mut matched = false;
        if allow_name_match && title_lower.contains(&normalized) {
            score += 32;
            matched = true;
            filename_match_used = true;
        } else if allow_name_match && path_lower.contains(&normalized) {
            score += 18;
            matched = true;
            filename_match_used = true;
        }
        if let Some(text) = searchable_lower.as_ref() {
            if text.contains(&normalized) {
                score += match modality {
                    KnowledgeModality::Document => 24,
                    KnowledgeModality::Image => 20,
                    KnowledgeModality::Audio => 18,
                    KnowledgeModality::Video => 18,
                };
                matched = true;
                content_match_used |= content_derived_source;
            }
        }
        if matched {
            matched_terms.push(term.clone());
        }
    }
    matched_terms.sort();
    matched_terms.dedup();

    let content_indexed = match modality {
        KnowledgeModality::Image => content_source_kinds
            .iter()
            .any(|kind| matches!(kind.as_str(), "vlm" | "ocr")),
        KnowledgeModality::Video => content_source_kinds
            .iter()
            .any(|kind| matches!(kind.as_str(), "vlm_keyframe" | "video_sidecar")),
        _ => searchable_text.is_some_and(|text| !text.trim().is_empty()),
    };
    let lexical_score = indexed_lexical_score
        .map(|score| score.normalized)
        .unwrap_or_else(|| (score as f32 / 100.0).clamp(0.0, 1.0));
    let semantic_only = lexical_score <= f32::EPSILON
        && matches!(
            modality,
            KnowledgeModality::Image | KnowledgeModality::Video
        )
        && content_indexed
        && searchable_text.is_some_and(|text| !text.trim().is_empty());

    if !metadata_recent
        && lexical_score <= f32::EPSILON
        && !semantic_only
        && !searchable_text.is_some_and(|text| !text.trim().is_empty())
    {
        return None;
    }

    let score = indexed_lexical_score
        .map(|value| (value.normalized * 100.0).round() as u32)
        .unwrap_or(score);

    Some(SearchCandidate {
        embedding_text: searchable_text.unwrap_or_default().to_string(),
        semantic_only,
        rrf_score: 0.0,
        final_score: 0.0,
        hit: KnowledgeSearchHit {
            modality: modality.as_str().to_string(),
            path: display_path,
            title,
            score,
            lexical_score: Some(lexical_score),
            bm25_score: indexed_lexical_score.map(|score| score.raw),
            embedding_score: None,
            hybrid_score: Some(lexical_score),
            rerank_score: None,
            chunk_id: chunk.map(|item| item.chunk_id.clone()),
            parent_id: chunk.and_then(|item| item.parent_id.clone()),
            matched_child_ids: chunk
                .map(|item| vec![item.chunk_id.clone()])
                .unwrap_or_default(),
            line_start: context_line_start.or_else(|| chunk.map(|item| item.line_start)),
            line_end: context_line_end.or_else(|| chunk.map(|item| item.line_end)),
            snippet: searchable_text.and_then(|text| build_snippet(text, &matched_terms)),
            answer_context: parent_context
                .or(searchable_text)
                .unwrap_or_default()
                .to_string(),
            matched_terms,
            provenance: chunk
                .map(|item| item.source_kind.clone())
                .filter(|value| !value.trim().is_empty()),
            source_path: chunk.and_then(|item| item.source_path.clone()),
            content_source_kinds,
            content_indexed,
            filename_match_used,
            content_match_used,
            modified_unix_millis,
        },
    })
}

fn content_source_kinds_for_chunk(chunk: Option<&KnowledgeIndexChunk>) -> Vec<String> {
    let Some(chunk) = chunk else {
        return Vec::new();
    };
    let source_kind = chunk.source_kind.trim().to_ascii_lowercase();
    if source_kind.is_empty() {
        Vec::new()
    } else {
        vec![source_kind]
    }
}

fn is_content_derived_source(modality: KnowledgeModality, content_source_kinds: &[String]) -> bool {
    match modality {
        KnowledgeModality::Document => true,
        KnowledgeModality::Image => content_source_kinds
            .iter()
            .any(|kind| matches!(kind.as_str(), "vlm" | "ocr")),
        KnowledgeModality::Audio => content_source_kinds.iter().any(|kind| kind == "transcript"),
        KnowledgeModality::Video => content_source_kinds
            .iter()
            .any(|kind| matches!(kind.as_str(), "vlm_keyframe" | "video_sidecar")),
    }
}

fn embedding_key(path: &str, chunk_id: Option<&str>) -> String {
    format!("{}::{}", path, chunk_id.unwrap_or("chunk-0001"))
}

fn required_embedding_coverage(
    required_keys: &HashSet<String>,
    embedding_scores: &HashMap<String, f32>,
) -> (usize, usize) {
    (
        required_keys.len(),
        required_keys
            .iter()
            .filter(|key| !embedding_scores.contains_key(*key))
            .count(),
    )
}

fn lexical_key_path(key: &str) -> Option<String> {
    key.rsplit_once("::")
        .map(|(path, _)| normalize_search_path_text(path))
        .filter(|path| !path.is_empty())
}

fn recent_entry_payload_paths(
    entries: &[KnowledgeIndexEntry],
    request: &KnowledgeSearchRequest,
    focus_paths: &[String],
    limit: usize,
) -> HashSet<String> {
    let mut eligible = entries
        .iter()
        .filter(|entry| modality_included(entry.modality, request))
        .filter(|entry| {
            focus_paths.is_empty() || entry_matches_focus_paths(&entry.path, focus_paths)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .file_signature
            .modified_unix_millis
            .cmp(&left.file_signature.modified_unix_millis)
            .then(left.path.cmp(&right.path))
    });
    eligible
        .into_iter()
        .take(limit)
        .map(|entry| normalize_search_path_text(&entry.path))
        .collect()
}

fn modality_included(modality: KnowledgeModality, request: &KnowledgeSearchRequest) -> bool {
    match modality {
        KnowledgeModality::Document => request.include_documents,
        KnowledgeModality::Image => request.include_images,
        KnowledgeModality::Audio => request.include_documents,
        KnowledgeModality::Video => request.include_videos,
    }
}

fn candidate_has_retrieval_signal(
    candidate: &SearchCandidate,
    settings: &KnowledgeRetrievalSettings,
) -> bool {
    if candidate
        .hit
        .lexical_score
        .is_some_and(|score| score > f32::EPSILON && score >= settings.lexical_min_score)
    {
        return true;
    }
    candidate.hit.embedding_score.is_some_and(|score| {
        if candidate.semantic_only {
            score >= settings.semantic_only_min_score
        } else {
            score >= settings.vector_min_score
        }
    })
}

fn candidate_vector_rank_score(
    candidate: &SearchCandidate,
    settings: &KnowledgeRetrievalSettings,
) -> Option<f32> {
    let score = candidate.hit.embedding_score?;
    let threshold = if candidate.semantic_only {
        settings.semantic_only_min_score
    } else {
        settings.vector_min_score
    };
    (score >= threshold).then_some(score)
}

fn apply_rrf_scores(candidates: &mut [SearchCandidate], settings: &KnowledgeRetrievalSettings) {
    let mut lexical_order = (0..candidates.len())
        .filter(|index| candidates[*index].hit.lexical_score.unwrap_or_default() > 0.0)
        .collect::<Vec<_>>();
    lexical_order.sort_by(|left, right| {
        candidates[*right]
            .hit
            .lexical_score
            .unwrap_or_default()
            .total_cmp(&candidates[*left].hit.lexical_score.unwrap_or_default())
            .then_with(|| candidate_tie_break(&candidates[*left], &candidates[*right]))
    });

    let mut vector_order = (0..candidates.len())
        .filter(|index| candidate_vector_rank_score(&candidates[*index], settings).is_some())
        .collect::<Vec<_>>();
    vector_order.sort_by(|left, right| {
        candidate_vector_rank_score(&candidates[*right], settings)
            .unwrap_or_default()
            .total_cmp(
                &candidate_vector_rank_score(&candidates[*left], settings).unwrap_or_default(),
            )
            .then_with(|| candidate_tie_break(&candidates[*left], &candidates[*right]))
    });

    let rrf_k = settings.rrf_k.max(1.0);
    let mut scores = vec![0.0f32; candidates.len()];
    for (rank, index) in lexical_order.iter().copied().enumerate() {
        scores[index] += settings.lexical_weight / (rrf_k + rank as f32 + 1.0);
    }
    for (rank, index) in vector_order.iter().copied().enumerate() {
        scores[index] += settings.vector_weight / (rrf_k + rank as f32 + 1.0);
    }
    let active_weight = (if lexical_order.is_empty() {
        0.0
    } else {
        settings.lexical_weight
    }) + if vector_order.is_empty() {
        0.0
    } else {
        settings.vector_weight
    };
    let max_possible = (active_weight / (rrf_k + 1.0)).max(f32::EPSILON);
    for (index, candidate) in candidates.iter_mut().enumerate() {
        let normalized = (scores[index] / max_possible).clamp(0.0, 1.0);
        candidate.rrf_score = normalized;
        candidate.final_score = normalized;
        candidate.hit.hybrid_score = Some(normalized);
        candidate.hit.score = score_to_compat(normalized);
    }
}

fn apply_local_rerank(
    candidates: &mut [SearchCandidate],
    rerank_query: &str,
    settings: &KnowledgeRetrievalSettings,
    model_center_state: &AdminModelCenterState,
    warnings: &mut Vec<String>,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let top_k = settings.rerank_top_k.min(candidates.len()).max(1);
    let documents = candidates
        .iter()
        .take(top_k)
        .map(rerank_passage_for_candidate)
        .collect::<Vec<_>>();
    let execution =
        model_center::run_rerank_with_state(rerank_query, &documents, top_k, model_center_state);
    if !execution.available {
        warnings.push(format!(
            "Reranker 不可用，已保留 RRF 排序：{}",
            execution.summary
        ));
        return None;
    }

    for candidate in candidates.iter_mut().take(top_k) {
        candidate.final_score = (0.25 * candidate.rrf_score).clamp(0.0, 1.0);
        candidate.hit.score = score_to_compat(candidate.final_score);
        candidate.hit.rerank_score = None;
    }

    let valid_scores = execution
        .scores
        .into_iter()
        .filter(|score| {
            score.index < top_k
                && score.score.is_finite()
                && score.score >= settings.rerank_min_score
        })
        .collect::<Vec<_>>();
    let max_raw_score = valid_scores
        .iter()
        .map(|score| score.score)
        .max_by(f32::total_cmp)
        .unwrap_or_default();

    let mut applied = 0usize;
    for score in valid_scores {
        let rerank_score = (score.score / max_raw_score.max(f32::EPSILON)).clamp(0.0, 1.0);
        let candidate = &mut candidates[score.index];
        candidate.hit.rerank_score = Some(rerank_score);
        candidate.final_score = (0.75 * rerank_score + 0.25 * candidate.rrf_score).clamp(0.0, 1.0);
        candidate.hit.score = score_to_compat(candidate.final_score);
        applied += 1;
    }

    if applied == 0 {
        warnings.push("Reranker 返回分数均低于阈值，已移除全部候选。".to_string());
        Some(top_k)
    } else {
        Some(top_k)
    }
}

fn rerank_query_for_search(query: &str) -> String {
    let normalized = query
        .trim()
        .trim_matches(|character: char| matches!(character, '?' | '？' | '。' | '！' | '!'));
    let mut topic = normalized;
    let mut document_list = false;
    for suffix in [
        "的是哪些文章",
        "的是哪些文档",
        "有哪些文章",
        "有哪些文档",
        "哪些文章",
        "哪些文档",
        "的文章",
        "的文档",
        "文章",
        "文档",
    ] {
        if let Some(value) = topic.strip_suffix(suffix) {
            topic = value.trim();
            document_list = true;
            break;
        }
    }
    if document_list {
        for prefix in [
            "请列出",
            "列出",
            "搜索",
            "查找",
            "找出",
            "有哪些",
            "描述",
            "关于",
        ] {
            if let Some(value) = topic.strip_prefix(prefix) {
                topic = value.trim();
                break;
            }
        }
    }
    if document_list && !topic.is_empty() {
        format!("文章的主要主题是{topic}")
    } else {
        query.to_string()
    }
}

fn rerank_passage_for_candidate(candidate: &SearchCandidate) -> String {
    let mut parts = vec![
        format!("title: {}", candidate.hit.title),
        format!("modality: {}", candidate.hit.modality),
    ];
    if let Some(preview) = candidate.hit.snippet.as_deref() {
        if !preview.trim().is_empty() {
            parts.push(format!("preview: {preview}"));
        }
    }
    if !candidate.embedding_text.trim().is_empty() {
        parts.push(format!(
            "text: {}",
            truncate_chars(&candidate.embedding_text, 1200)
        ));
    }
    parts.join("\n")
}

fn apply_mmr(
    mut candidates: Vec<SearchCandidate>,
    limit: usize,
    lambda: f32,
) -> Vec<SearchCandidate> {
    let lambda = lambda.clamp(0.0, 1.0);
    let mut selected = Vec::new();
    while !candidates.is_empty() && selected.len() < limit {
        let best_index = (0..candidates.len())
            .max_by(|left, right| {
                let left_score = mmr_candidate_score(&candidates[*left], &selected, lambda);
                let right_score = mmr_candidate_score(&candidates[*right], &selected, lambda);
                left_score
                    .total_cmp(&right_score)
                    .then_with(|| candidate_tie_break(&candidates[*right], &candidates[*left]))
            })
            .unwrap_or(0);
        selected.push(candidates.remove(best_index));
    }
    selected
}

fn select_candidates_with_modality_quotas(
    candidates: Vec<SearchCandidate>,
    limit: usize,
    per_modality_limit: usize,
    mmr_enabled: bool,
    mmr_lambda: f32,
    recent: bool,
) -> Vec<SearchCandidate> {
    let mut selected = Vec::new();
    for modality in ["document", "image", "video"] {
        let pool = candidates
            .iter()
            .filter(|candidate| candidate.hit.modality == modality)
            .cloned()
            .collect::<Vec<_>>();
        let mut chosen = if mmr_enabled {
            apply_mmr(pool, per_modality_limit, mmr_lambda)
        } else {
            pool.into_iter().take(per_modality_limit).collect()
        };
        selected.append(&mut chosen);
    }
    if selected.len() < limit {
        let mut selected_keys = selected
            .iter()
            .map(|candidate| {
                (
                    candidate.hit.modality.clone(),
                    candidate.hit.path.clone(),
                    candidate.hit.chunk_id.clone().unwrap_or_default(),
                )
            })
            .collect::<HashSet<_>>();
        for candidate in &candidates {
            if selected.len() >= limit {
                break;
            }
            let key = (
                candidate.hit.modality.clone(),
                candidate.hit.path.clone(),
                candidate.hit.chunk_id.clone().unwrap_or_default(),
            );
            if selected_keys.insert(key) {
                selected.push(candidate.clone());
            }
        }
    }
    if recent {
        selected.sort_by(|left, right| {
            right
                .hit
                .modified_unix_millis
                .cmp(&left.hit.modified_unix_millis)
                .then_with(|| candidate_tie_break(left, right))
        });
    } else {
        sort_candidates_by_final_score(&mut selected);
    }
    selected.truncate(limit);
    selected
}

fn collapse_sibling_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    let mut collapsed = Vec::<SearchCandidate>::new();
    let mut parent_positions = HashMap::<(String, String, String), usize>::new();
    for candidate in candidates {
        let Some(parent_id) = candidate.hit.parent_id.clone() else {
            collapsed.push(candidate);
            continue;
        };
        let key = (
            candidate.hit.modality.clone(),
            candidate.hit.path.clone(),
            parent_id,
        );
        if let Some(position) = parent_positions.get(&key).copied() {
            let existing = &mut collapsed[position];
            for child_id in candidate.hit.matched_child_ids {
                if !existing.hit.matched_child_ids.contains(&child_id) {
                    existing.hit.matched_child_ids.push(child_id);
                }
            }
            for term in candidate.hit.matched_terms {
                if !existing.hit.matched_terms.contains(&term) {
                    existing.hit.matched_terms.push(term);
                }
            }
            continue;
        }
        parent_positions.insert(key, collapsed.len());
        collapsed.push(candidate);
    }
    collapsed
}

fn collapse_file_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    let mut collapsed = Vec::<SearchCandidate>::new();
    let mut file_positions = HashMap::<(String, String), usize>::new();
    for candidate in candidates {
        let key = (candidate.hit.modality.clone(), candidate.hit.path.clone());
        if let Some(position) = file_positions.get(&key).copied() {
            let existing = &mut collapsed[position];
            if let Some(chunk_id) = candidate.hit.chunk_id {
                if !existing.hit.matched_child_ids.contains(&chunk_id) {
                    existing.hit.matched_child_ids.push(chunk_id);
                }
            }
            for child_id in candidate.hit.matched_child_ids {
                if !existing.hit.matched_child_ids.contains(&child_id) {
                    existing.hit.matched_child_ids.push(child_id);
                }
            }
            for term in candidate.hit.matched_terms {
                if !existing.hit.matched_terms.contains(&term) {
                    existing.hit.matched_terms.push(term);
                }
            }
            for source_kind in candidate.hit.content_source_kinds {
                if !existing.hit.content_source_kinds.contains(&source_kind) {
                    existing.hit.content_source_kinds.push(source_kind);
                }
            }
            existing.hit.filename_match_used |= candidate.hit.filename_match_used;
            existing.hit.content_match_used |= candidate.hit.content_match_used;
            continue;
        }
        file_positions.insert(key, collapsed.len());
        collapsed.push(candidate);
    }
    collapsed
}

fn file_candidate_count(candidates: &[SearchCandidate]) -> usize {
    candidates
        .iter()
        .map(|candidate| {
            (
                candidate.hit.modality.clone(),
                normalize_search_path_text(&candidate.hit.path),
            )
        })
        .collect::<HashSet<_>>()
        .len()
}

fn rank_recent_candidates(candidates: &mut [SearchCandidate]) {
    for candidate in candidates.iter_mut() {
        candidate.final_score = 1.0;
        candidate.hit.score = score_to_compat(1.0);
        candidate.hit.hybrid_score = None;
    }
    candidates.sort_by(|left, right| {
        right
            .hit
            .modified_unix_millis
            .cmp(&left.hit.modified_unix_millis)
            .then_with(|| candidate_tie_break(left, right))
    });
}

fn mmr_candidate_score(
    candidate: &SearchCandidate,
    selected: &[SearchCandidate],
    lambda: f32,
) -> f32 {
    let redundancy = selected
        .iter()
        .map(|item| candidate_overlap_similarity(candidate, item))
        .fold(0.0f32, f32::max);
    lambda * candidate.final_score - (1.0 - lambda) * redundancy
}

fn candidate_overlap_similarity(left: &SearchCandidate, right: &SearchCandidate) -> f32 {
    let left_terms = mmr_token_set(left);
    let right_terms = mmr_token_set(right);
    if left_terms.is_empty() || right_terms.is_empty() {
        return 0.0;
    }
    let intersection = left_terms.intersection(&right_terms).count() as f32;
    let union = left_terms.union(&right_terms).count() as f32;
    if union <= f32::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

fn mmr_token_set(candidate: &SearchCandidate) -> HashSet<String> {
    let text = format!("{} {}", candidate.hit.title, candidate.embedding_text);
    lexical_query_terms(&text).into_iter().collect()
}

fn sort_candidates_by_final_score(candidates: &mut [SearchCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .final_score
            .total_cmp(&left.final_score)
            .then_with(|| candidate_tie_break(left, right))
    });
}

fn candidate_tie_break(left: &SearchCandidate, right: &SearchCandidate) -> std::cmp::Ordering {
    right
        .rrf_score
        .total_cmp(&left.rrf_score)
        .then_with(|| {
            right
                .hit
                .lexical_score
                .unwrap_or_default()
                .total_cmp(&left.hit.lexical_score.unwrap_or_default())
        })
        .then_with(|| left.hit.line_start.cmp(&right.hit.line_start))
        .then_with(|| left.hit.title.cmp(&right.hit.title))
        .then_with(|| left.hit.path.cmp(&right.hit.path))
}

fn score_to_compat(score: f32) -> u32 {
    (score.clamp(0.0, 1.0) * 1000.0).round() as u32
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in text.chars().take(max_chars) {
        output.push(ch);
    }
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn build_snippet(text: &str, matched_terms: &[String]) -> Option<String> {
    let lowercase = text.to_lowercase();
    let first_match = matched_terms
        .iter()
        .filter_map(|term| lowercase.find(&term.to_lowercase()))
        .min()?;
    let start = clamp_to_char_boundary(text, first_match.saturating_sub(24));
    let end = clamp_to_char_boundary(text, (first_match + 72).min(text.len()));
    let snippet = text[start..end]
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string();
    (!snippet.is_empty()).then_some(snippet)
}

fn clamp_to_char_boundary(text: &str, index: usize) -> usize {
    let mut candidate = index.min(text.len());
    while candidate > 0 && !text.is_char_boundary(candidate) {
        candidate -= 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{json, Value};

    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
        PrivacyLevel,
    };
    use crate::runtime::admin_console::{
        AdminConsoleState, AdminModelCenterState, KnowledgeRetrievalSettings,
    };

    use super::{
        candidate_has_retrieval_signal, candidate_vector_rank_score, required_embedding_coverage,
        KnowledgeIndexConfig, KnowledgeIndexService, KnowledgeSearchHit, KnowledgeSearchRequest,
        KnowledgeSearchService, SearchCandidate,
    };

    static INDEX_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn embedding_coverage_only_counts_required_bm25_keys() {
        let required = HashSet::from([
            "/knowledge/alpha.md::chunk-0001".to_string(),
            "/knowledge/beta.md::chunk-0001".to_string(),
        ]);
        let scores = HashMap::from([
            ("/knowledge/alpha.md::chunk-0001".to_string(), 0.9),
            ("/knowledge/vector-only.md::chunk-0001".to_string(), 0.8),
        ]);

        assert_eq!(required_embedding_coverage(&required, &scores), (2, 1));

        let complete_scores = HashMap::from([
            ("/knowledge/alpha.md::chunk-0001".to_string(), 0.9),
            ("/knowledge/beta.md::chunk-0001".to_string(), 0.7),
        ]);
        assert_eq!(
            required_embedding_coverage(&required, &complete_scores),
            (2, 0)
        );
    }

    fn quota_candidate(
        modality: &str,
        title: &str,
        score: f32,
        modified_unix_millis: u128,
    ) -> SearchCandidate {
        SearchCandidate {
            embedding_text: title.to_string(),
            semantic_only: false,
            rrf_score: score,
            final_score: score,
            hit: KnowledgeSearchHit {
                modality: modality.to_string(),
                path: format!("/knowledge/{title}"),
                title: title.to_string(),
                score: (score * 1000.0) as u32,
                lexical_score: Some(score),
                bm25_score: Some(score),
                embedding_score: Some(score),
                hybrid_score: Some(score),
                rerank_score: None,
                chunk_id: Some("chunk-0001".to_string()),
                parent_id: None,
                matched_child_ids: Vec::new(),
                line_start: Some(1),
                line_end: Some(1),
                snippet: Some(title.to_string()),
                answer_context: title.to_string(),
                matched_terms: Vec::new(),
                provenance: None,
                source_path: None,
                content_source_kinds: Vec::new(),
                content_indexed: true,
                filename_match_used: false,
                content_match_used: true,
                modified_unix_millis: Some(modified_unix_millis),
            },
        }
    }

    #[test]
    fn retrieval_signal_uses_normalized_scores_and_falls_back_to_available_channel() {
        let mut settings = KnowledgeRetrievalSettings::default();
        settings.lexical_min_score = 0.4;
        settings.vector_min_score = 0.4;
        settings.semantic_only_min_score = 0.4;
        let mut candidate = quota_candidate("document", "candidate.md", 1.0, 10);

        candidate.hit.lexical_score = Some(0.3);
        candidate.hit.bm25_score = Some(10.0);
        candidate.hit.embedding_score = Some(0.3);
        assert!(!candidate_has_retrieval_signal(&candidate, &settings));

        candidate.hit.embedding_score = Some(0.5);
        assert!(candidate_has_retrieval_signal(&candidate, &settings));

        candidate.hit.embedding_score = None;
        candidate.hit.lexical_score = Some(0.5);
        assert!(candidate_has_retrieval_signal(&candidate, &settings));

        candidate.hit.lexical_score = None;
        candidate.hit.embedding_score = Some(0.3);
        assert!(!candidate_has_retrieval_signal(&candidate, &settings));

        candidate.hit.lexical_score = Some(0.5);
        assert!(candidate_has_retrieval_signal(&candidate, &settings));
        assert_eq!(candidate_vector_rank_score(&candidate, &settings), None);
    }

    #[test]
    fn modality_quotas_prevent_documents_from_crowding_out_images() {
        let candidates = vec![
            quota_candidate("document", "doc-1", 1.0, 10),
            quota_candidate("document", "doc-2", 0.9, 9),
            quota_candidate("document", "doc-3", 0.8, 8),
            quota_candidate("image", "image-1", 0.7, 7),
            quota_candidate("image", "image-2", 0.6, 6),
        ];

        let selected =
            super::select_candidates_with_modality_quotas(candidates, 4, 2, false, 0.7, false);

        assert_eq!(
            selected
                .iter()
                .filter(|candidate| candidate.hit.modality == "document")
                .count(),
            2
        );
        assert_eq!(
            selected
                .iter()
                .filter(|candidate| candidate.hit.modality == "image")
                .count(),
            2
        );
    }

    #[test]
    fn sibling_child_hits_collapse_to_one_parent_context() {
        let mut first = quota_candidate("document", "spring.md", 1.0, 10);
        first.hit.parent_id = Some("parent-0001".to_string());
        first.hit.chunk_id = Some("chunk-0001".to_string());
        first.hit.matched_child_ids = vec!["chunk-0001".to_string()];
        first.hit.answer_context = "完整春季章节上下文".to_string();

        let mut second = quota_candidate("document", "spring.md", 0.9, 10);
        second.hit.parent_id = Some("parent-0001".to_string());
        second.hit.chunk_id = Some("chunk-0002".to_string());
        second.hit.matched_child_ids = vec!["chunk-0002".to_string()];
        second.hit.answer_context = "完整春季章节上下文".to_string();

        let collapsed = super::collapse_sibling_candidates(vec![first, second]);

        assert_eq!(collapsed.len(), 1);
        assert_eq!(
            collapsed[0].hit.matched_child_ids,
            vec!["chunk-0001", "chunk-0002"]
        );
        assert_eq!(collapsed[0].hit.answer_context, "完整春季章节上下文");
    }

    #[test]
    fn file_slots_follow_ranked_chunks_across_parents() {
        let mut first = quota_candidate("document", "spring.md", 1.0, 10);
        first.hit.parent_id = Some("parent-0001".to_string());
        first.hit.chunk_id = Some("chunk-0001".to_string());
        first.hit.matched_child_ids = vec!["chunk-0001".to_string()];
        first.hit.matched_terms = vec!["春天".to_string()];

        let mut second = quota_candidate("document", "spring.md", 0.9, 10);
        second.hit.parent_id = Some("parent-0002".to_string());
        second.hit.chunk_id = Some("chunk-0008".to_string());
        second.hit.matched_child_ids = vec!["chunk-0008".to_string()];
        second.hit.matched_terms = vec!["花朵".to_string()];
        second.final_score = 1.1;
        second.hit.score = 1000;

        let third = quota_candidate("document", "summer.md", 0.8, 9);
        let mut ranked = vec![first, second, third];
        super::sort_candidates_by_final_score(&mut ranked);
        let collapsed = super::collapse_file_candidates(ranked);

        assert_eq!(collapsed.len(), 2);
        assert_eq!(collapsed[0].hit.path, "/knowledge/spring.md");
        assert_eq!(collapsed[0].hit.chunk_id.as_deref(), Some("chunk-0008"));
        assert_eq!(collapsed[0].final_score, 1.1);
        assert_eq!(
            collapsed[0].hit.matched_child_ids,
            vec!["chunk-0008", "chunk-0001"]
        );
        assert_eq!(collapsed[0].hit.matched_terms, vec!["花朵", "春天"]);
        assert_eq!(collapsed[1].hit.path, "/knowledge/summer.md");
    }

    #[test]
    fn modality_quotas_fill_unused_capacity_after_reserving_each_modality() {
        let candidates = vec![
            quota_candidate("document", "doc-1", 1.0, 10),
            quota_candidate("document", "doc-2", 0.9, 9),
            quota_candidate("document", "doc-3", 0.8, 8),
            quota_candidate("image", "image-1", 0.7, 7),
        ];

        let selected =
            super::select_candidates_with_modality_quotas(candidates, 4, 2, false, 0.7, false);

        assert_eq!(selected.len(), 4);
        assert_eq!(
            selected
                .iter()
                .filter(|candidate| candidate.hit.modality == "image")
                .count(),
            1
        );
    }

    #[test]
    fn recent_strategy_orders_selected_files_by_modified_time() {
        let mut candidates = vec![
            quota_candidate("document", "older.md", 0.0, 100),
            quota_candidate("document", "newer.md", 0.0, 300),
            quota_candidate("document", "middle.md", 0.0, 200),
        ];

        super::rank_recent_candidates(&mut candidates);
        assert!(candidates.iter().all(|candidate| candidate.hit.score > 0));

        let selected =
            super::select_candidates_with_modality_quotas(candidates, 3, 3, false, 0.7, true);

        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.hit.title.as_str())
                .collect::<Vec<_>>(),
            vec!["newer.md", "middle.md", "older.md"]
        );
    }

    fn unique_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn cleanup_dir(path: &Path) {
        if path.exists() {
            let _ = fs::remove_dir_all(path);
        }
    }

    fn build_search_index(root: &Path, index_root: &Path) {
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.to_path_buf()).expect("knowledge index config"),
        )
        .expect("knowledge index service");
        let snapshot = service
            .load_or_refresh(root)
            .expect("build knowledge index");
        let model_center_state = crate::runtime::model_center::load_model_center_state();
        let _ = service.warm_embedding_cache(&snapshot, &model_center_state);
    }

    fn warm_existing_search_index(root: &Path, index_root: &Path) {
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.to_path_buf()).expect("knowledge index config"),
        )
        .expect("knowledge index service");
        let snapshot = service.load_existing(root).expect("load knowledge index");
        let model_center_state = crate::runtime::model_center::load_model_center_state();
        let _ = service.warm_embedding_cache(&snapshot, &model_center_state);
    }

    #[test]
    fn search_requires_existing_index_manifest_instead_of_refreshing() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-no-manifest");
        let index_root = unique_dir("harborbeacon-knowledge-index-no-manifest");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(root.join("docs").join("sakura.md"), "樱花计划").expect("write doc");

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "樱花".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("knowledge search");

        assert!(response.degraded);
        assert_eq!(
            response.degraded_reason.as_deref(),
            Some("index_manifest_unavailable")
        );
        assert_eq!(response.total_matches, 0);
        assert!(response
            .blockers
            .iter()
            .any(|item| item.contains("/api/knowledge/index/run")));
        assert!(!index_root
            .read_dir()
            .expect("list index root")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".json")));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    fn write_mock_model_center_state(
        path: &Path,
        mock_ocr_text: &str,
        mock_vlm_text: Option<&str>,
    ) {
        let mut endpoints = vec![ModelEndpoint {
            model_endpoint_id: "ocr-mock".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Ocr,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "tesseract".to_string(),
            model_name: "mock-ocr".to_string(),
            capability_tags: vec!["ocr".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "mock_text": mock_ocr_text,
            }),
        }];
        let mut route_policies = vec![ModelRoutePolicy {
            route_policy_id: "retrieval.ocr".to_string(),
            workspace_id: "home-1".to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "image".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec!["local".to_string(), "cloud".to_string()],
            status: "active".to_string(),
            metadata: json!({}),
        }];
        if let Some(mock_vlm_text) = mock_vlm_text {
            endpoints.push(ModelEndpoint {
                model_endpoint_id: "vlm-mock".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Vlm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "mock-vlm".to_string(),
                capability_tags: vec!["vlm".to_string(), "multimodal".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_text": mock_vlm_text,
                }),
            });
            route_policies.push(ModelRoutePolicy {
                route_policy_id: "retrieval.vision_summary".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "multimodal".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            });
        }
        let state = AdminConsoleState {
            models: AdminModelCenterState {
                endpoints,
                route_policies,
                ..AdminModelCenterState::default()
            },
            ..AdminConsoleState::default()
        };
        fs::write(
            path,
            serde_json::to_vec_pretty(&state).expect("serialize admin state"),
        )
        .expect("write admin state");
    }

    fn write_mock_model_center_state_with_embed(path: &Path, mock_embeddings: Value) {
        let state = AdminConsoleState {
            models: AdminModelCenterState {
                endpoints: vec![ModelEndpoint {
                    model_endpoint_id: "embed-mock".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Embedder,
                    endpoint_kind: ModelEndpointKind::Local,
                    provider_key: "openai_compatible".to_string(),
                    model_name: "mock-embed".to_string(),
                    capability_tags: vec!["embeddings".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({
                        "mock_embeddings": mock_embeddings,
                    }),
                }],
                route_policies: vec![ModelRoutePolicy {
                    route_policy_id: "retrieval.embed".to_string(),
                    workspace_id: "home-1".to_string(),
                    domain_scope: "retrieval".to_string(),
                    modality: "text".to_string(),
                    privacy_level: PrivacyLevel::StrictLocal,
                    local_preferred: true,
                    max_cost_per_run: None,
                    fallback_order: vec!["local".to_string(), "cloud".to_string()],
                    status: "active".to_string(),
                    metadata: json!({}),
                }],
                ..AdminModelCenterState::default()
            },
            ..AdminConsoleState::default()
        };
        fs::write(
            path,
            serde_json::to_vec_pretty(&state).expect("serialize admin state"),
        )
        .expect("write admin state");
    }

    fn write_mock_model_center_state_with_rerank(path: &Path, scores: Value) {
        let state = AdminConsoleState {
            models: AdminModelCenterState {
                endpoints: vec![ModelEndpoint {
                    model_endpoint_id: "rerank-local".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Reranker,
                    endpoint_kind: ModelEndpointKind::Local,
                    provider_key: "rerank_compatible".to_string(),
                    model_name: "mock-reranker".to_string(),
                    capability_tags: vec!["rerank".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({
                        "mock_rerank_scores": scores,
                    }),
                }],
                route_policies: vec![ModelRoutePolicy {
                    route_policy_id: "retrieval.rerank".to_string(),
                    workspace_id: "home-1".to_string(),
                    domain_scope: "retrieval".to_string(),
                    modality: "text".to_string(),
                    privacy_level: PrivacyLevel::StrictLocal,
                    local_preferred: true,
                    max_cost_per_run: None,
                    fallback_order: vec!["local".to_string(), "sidecar".to_string()],
                    status: "active".to_string(),
                    metadata: json!({"capability": "rerank"}),
                }],
                ..AdminModelCenterState::default()
            },
            ..AdminConsoleState::default()
        };
        fs::write(
            path,
            serde_json::to_vec_pretty(&state).expect("serialize admin state"),
        )
        .expect("write admin state");
    }

    #[test]
    fn search_returns_document_and_image_matches() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            root.join("docs").join("spring-sakura.md"),
            "今年花园里的樱花开得很盛，适合做春季归档。",
        )
        .expect("write doc");
        fs::write(root.join("images").join("garden.jpg"), b"not-really-a-jpeg")
            .expect("write image");
        fs::write(
            root.join("images").join("garden.json"),
            r#"{"caption":"花园里的樱花树","tags":["spring","sakura"]}"#,
        )
        .expect("write sidecar");
        build_search_index(&root, &index_root);

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "樱花".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("knowledge search");

        assert_eq!(response.total_matches, 2);
        assert_eq!(response.documents.len(), 1);
        assert_eq!(response.images.len(), 1);
        assert!(response.documents[0].path.ends_with("spring-sakura.md"));
        assert!(response.images[0].path.ends_with("garden.jpg"));
        assert_eq!(response.images[0].matched_terms, vec!["樱花".to_string()]);
        assert_eq!(response.reply_pack.citations.len(), 2);
        assert_eq!(response.reply_pack.citations[0].title, "spring-sakura.md");
        assert_eq!(response.reply_pack.citations[0].modality, "document");
        assert!(response.reply_pack.citations[0]
            .preview
            .as_deref()
            .unwrap_or_default()
            .contains("樱花"));
        assert!(response.reply_pack.citations[0].chunk_id.is_some());
        assert_eq!(response.reply_pack.citations[0].line_start, Some(1));
        assert_eq!(response.reply_pack.citations[1].title, "garden.jpg");
        assert_eq!(response.reply_pack.citations[1].modality, "image");

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn child_retrieval_restores_the_full_parent_context() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-parent-context");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(&root).expect("create corpus root");
        fs::create_dir_all(&index_root).expect("create index root");
        let background = format!("背景说明：{}", "春".repeat(210));
        let decision = format!("最终决定采用海港协议七号。{}", "花".repeat(90));
        fs::write(
            root.join("decision.md"),
            format!("# 项目决策\n{background}\n{decision}"),
        )
        .expect("write parent context document");
        build_search_index(&root, &index_root);

        let mut retrieval = KnowledgeRetrievalSettings::default();
        retrieval.vector_weight = 0.0;
        retrieval.rerank_enabled = false;
        retrieval.mmr_enabled = false;
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "海港协议七号".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            retrieval,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("parent child knowledge search");

        assert_eq!(response.documents.len(), 1);
        let hit = &response.documents[0];
        assert_eq!(hit.parent_id.as_deref(), Some("parent-0001"));
        assert_eq!(hit.matched_child_ids.len(), 1);
        assert!(hit.answer_context.contains("背景说明"));
        assert!(hit.answer_context.contains("海港协议七号"));
        assert!(hit
            .snippet
            .as_deref()
            .unwrap_or_default()
            .contains("海港协议七号"));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn lexical_only_search_skips_embedding_degradation() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-lexical-only");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(&root).expect("create corpus root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(root.join("spring.md"), "spring flowers and warm wind").expect("write document");
        build_search_index(&root, &index_root);

        let mut request = KnowledgeSearchRequest::new("spring");
        request.configured_roots = vec![root.to_string_lossy().into_owned()];
        request.roots = request.configured_roots.clone();
        request.index_root = Some(index_root.to_string_lossy().into_owned());
        request.include_images = false;
        request.retrieval.lexical_weight = 1.0;
        request.retrieval.vector_weight = 0.0;
        request.retrieval.rerank_enabled = false;

        let response = KnowledgeSearchService::search(request).expect("knowledge search");

        assert_eq!(response.status, "completed");
        assert!(!response.degraded);
        assert!(response.warnings.is_empty());
        assert_eq!(response.documents.len(), 1);

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn lexical_only_search_uses_bm25_length_normalization() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-bm25");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(&root).expect("create corpus root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            root.join("concise.md"),
            "最终决定采用海港协议七号处理设备升级。",
        )
        .expect("write concise document");
        fs::write(
            root.join("verbose.md"),
            format!(
                "{}最后曾提到海港协议七号，但不是最终决定。",
                "常规设备升级说明。".repeat(180)
            ),
        )
        .expect("write verbose document");
        build_search_index(&root, &index_root);

        let mut request = KnowledgeSearchRequest::new("海港协议七号");
        request.configured_roots = vec![root.to_string_lossy().into_owned()];
        request.roots = request.configured_roots.clone();
        request.index_root = Some(index_root.to_string_lossy().into_owned());
        request.include_images = false;
        request.retrieval.lexical_weight = 1.0;
        request.retrieval.vector_weight = 0.0;
        request.retrieval.rerank_enabled = false;

        let response = KnowledgeSearchService::search(request).expect("BM25 knowledge search");

        assert_eq!(response.status, "completed");
        assert!(!response.degraded);
        assert!(response.warnings.is_empty());
        assert_eq!(response.documents.len(), 2);
        assert_eq!(response.documents[0].title, "concise.md");
        assert!(
            response.documents[0].lexical_score.unwrap_or_default()
                > response.documents[1].lexical_score.unwrap_or_default()
        );
        assert!(index_root
            .read_dir()
            .expect("list index root")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .ends_with(".lexical-v2.tantivy")));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn search_returns_chunk_grounded_document_snippet() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-rag");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            root.join("docs").join("multi-section.md"),
            "第一段是背景介绍。\n第二段仍然是背景。\n第三段继续铺垫。\n第四段保持上下文。\n第五段明确提到樱花季文档整理与引用。\n第六段补充引用来源。",
        )
        .expect("write doc");
        build_search_index(&root, &index_root);

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "樱花".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("knowledge search");

        assert_eq!(response.documents.len(), 1);
        let hit = &response.documents[0];
        assert_eq!(hit.title, "multi-section.md");
        assert_eq!(hit.chunk_id.as_deref(), Some("chunk-0001"));
        assert_eq!(hit.parent_id.as_deref(), Some("parent-0001"));
        assert_eq!(hit.matched_child_ids, vec!["chunk-0001"]);
        assert_eq!(hit.line_start, Some(1));
        assert_eq!(hit.line_end, Some(6));
        assert!(!hit.answer_context.trim().is_empty());
        assert!(hit
            .snippet
            .as_deref()
            .unwrap_or_default()
            .contains("樱花季"));
        assert_eq!(response.reply_pack.citations.len(), 1);
        assert_eq!(
            response.reply_pack.citations[0].chunk_id.as_deref(),
            Some("chunk-0001")
        );
        assert_eq!(
            response.reply_pack.citations[0].parent_id.as_deref(),
            Some("parent-0001")
        );
        assert_eq!(
            response.reply_pack.citations[0].matched_child_ids,
            vec!["chunk-0001"]
        );
        assert_eq!(response.reply_pack.citations[0].line_start, Some(1));
        assert_eq!(response.reply_pack.citations[0].line_end, Some(6));
        assert_eq!(
            response.reply_pack.citations[0].answer_context,
            hit.answer_context
        );
        assert!(response.reply_pack.citations[0]
            .preview
            .as_deref()
            .unwrap_or_default()
            .contains("樱花季"));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn search_deduplicates_repeated_roots_and_keeps_stable_order() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-dedupe");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            root.join("docs").join("alpha-note.md"),
            "alpha note about spring",
        )
        .expect("doc");
        fs::write(
            root.join("docs").join("beta-note.md"),
            "beta note about spring",
        )
        .expect("doc");
        fs::write(root.join("images").join("alpha.png"), b"image").expect("image");
        fs::write(
            root.join("images").join("alpha.json"),
            r#"{"caption":"alpha spring view"}"#,
        )
        .expect("sidecar");
        build_search_index(&root, &index_root);

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "spring".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![
                root.to_string_lossy().into_owned(),
                root.to_string_lossy().into_owned(),
            ],
            include_documents: true,
            include_images: true,
            limit: 10,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("knowledge search");

        assert_eq!(response.documents.len(), 2);
        assert_eq!(response.images.len(), 1);
        assert_eq!(response.total_matches, 3);
        assert_eq!(response.documents[0].title, "alpha-note.md");
        assert_eq!(response.documents[1].title, "beta-note.md");
        assert_eq!(response.images[0].title, "alpha.png");
        assert_eq!(response.reply_pack.citations.len(), 3);
        assert_eq!(response.reply_pack.citations[0].title, "alpha-note.md");
        assert_eq!(response.reply_pack.citations[1].title, "alpha.png");
        assert_eq!(response.reply_pack.citations[2].title, "beta-note.md");

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn hybrid_retrieval_uses_embedding_store_to_break_lexical_ties() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-hybrid");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-embed").join("state.json");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("docs").join("a-note.md"), "樱花 会议 纪要").expect("doc a");
        fs::write(root.join("docs").join("b-note.md"), "整理 计划 清单").expect("doc b");

        write_mock_model_center_state_with_embed(
            &admin_state_path,
            json!({
                "樱花整理": [1.0, 0.0],
                "樱花 会议 纪要": [0.05, 0.95],
                "整理 计划 清单": [0.98, 0.02]
            }),
        );

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "樱花整理".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 10,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("hybrid search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.documents.len(), 2);
        assert_eq!(response.documents[0].title, "b-note.md");
        assert_eq!(response.documents[1].title, "a-note.md");
        assert!(response.documents[0].embedding_score.unwrap_or_default() > 0.9);
        assert!(response.documents[0].hybrid_score.unwrap_or_default() > 0.5);
        assert!(
            response.reply_pack.citations[0]
                .embedding_score
                .unwrap_or_default()
                > 0.9
        );
        assert!(response
            .supported_modalities
            .iter()
            .any(|item| item == "hybrid_retrieval"));
        assert!(index_root
            .read_dir()
            .expect("list index root")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .ends_with(".embeddings.json")));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn document_list_rerank_query_targets_the_document_theme() {
        assert_eq!(
            super::rerank_query_for_search("春天的文章"),
            "文章的主要主题是春天"
        );
        assert_eq!(
            super::rerank_query_for_search("描述春天的是哪些文章？"),
            "文章的主要主题是春天"
        );
        assert_eq!(
            super::rerank_query_for_search("这篇文章如何描写春天？"),
            "这篇文章如何描写春天？"
        );
    }

    #[test]
    fn rerank_reorders_reply_pack_citations_after_rrf() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-rerank");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-rerank").join("state.json");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("docs").join("a-note.md"), "春季 整理 alpha").expect("doc a");
        fs::write(root.join("docs").join("b-note.md"), "春季 整理 beta").expect("doc b");
        write_mock_model_center_state_with_rerank(&admin_state_path, json!([0.002, 0.04]));

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "春季整理".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("rerank search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.documents[0].title, "b-note.md");
        assert_eq!(response.reply_pack.citations[0].title, "b-note.md");
        assert!(
            response.reply_pack.citations[0]
                .rerank_score
                .unwrap_or_default()
                > 0.9
        );

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn reranker_unavailable_keeps_rrf_results_with_warning() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-rerank-degraded");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(root.join("docs").join("a-note.md"), "春季 整理 alpha").expect("doc a");
        fs::write(root.join("docs").join("b-note.md"), "春季 整理 beta").expect("doc b");
        build_search_index(&root, &index_root);

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "春季整理".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("rerank degraded search");

        assert_eq!(response.documents.len(), 2);
        assert_eq!(response.reply_pack.citations.len(), 2);
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("Reranker 不可用")));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn reranker_rejects_all_candidates_below_threshold() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-rerank-reject-all");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-rerank-reject-all").join("state.json");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("docs").join("a-note.md"), "spring archive alpha").expect("doc a");
        fs::write(root.join("docs").join("b-note.md"), "spring archive beta").expect("doc b");
        write_mock_model_center_state_with_rerank(&admin_state_path, json!([0.0]));

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let mut retrieval = KnowledgeRetrievalSettings::default();
        retrieval.rerank_top_k = 1;
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "spring archive".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            retrieval,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("rerank reject-all search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert!(response.documents.is_empty());
        assert!(response.reply_pack.citations.is_empty());
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("均低于阈值")));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn reranker_drops_unranked_tail_when_top_candidate_passes() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-rerank-drop-tail");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-rerank-drop-tail").join("state.json");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("docs").join("a-note.md"), "spring archive alpha").expect("doc a");
        fs::write(root.join("docs").join("b-note.md"), "spring archive beta").expect("doc b");
        write_mock_model_center_state_with_rerank(&admin_state_path, json!([1.0]));

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let mut retrieval = KnowledgeRetrievalSettings::default();
        retrieval.rerank_top_k = 1;
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "spring archive".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: true,
            include_images: false,
            limit: 5,
            retrieval,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("rerank drop-tail search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.documents.len(), 1);
        assert_eq!(response.reply_pack.citations.len(), 1);
        assert!(response.documents[0].rerank_score.is_some());

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn search_surfaces_sidecar_and_ocr_provenance_for_image_hits() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-image-provenance");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path = unique_dir("harborbeacon-admin-model-center").join("state.json");
        fs::create_dir_all(root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("images").join("gate.jpg"), b"fake-image").expect("write image");
        fs::write(
            root.join("images").join("gate.txt"),
            "front gate camera overview",
        )
        .expect("write sidecar");
        write_mock_model_center_state(&admin_state_path, "plate ABC123 from OCR", None);

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let sidecar_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "front".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("sidecar search");
        let ocr_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "ABC123".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("ocr search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(sidecar_response.images.len(), 1);
        assert_eq!(
            sidecar_response.images[0].provenance.as_deref(),
            Some("sidecar")
        );
        assert!(sidecar_response.images[0]
            .source_path
            .as_deref()
            .unwrap_or_default()
            .ends_with("gate.txt"));
        assert_eq!(
            sidecar_response.reply_pack.citations[0]
                .provenance
                .as_deref(),
            Some("sidecar")
        );
        assert!(sidecar_response
            .supported_modalities
            .iter()
            .any(|item| item == "ocr"));

        assert_eq!(ocr_response.images.len(), 1);
        assert_eq!(ocr_response.images[0].provenance.as_deref(), Some("ocr"));
        assert!(ocr_response.images[0].source_path.is_none());
        assert_eq!(
            ocr_response.reply_pack.citations[0].provenance.as_deref(),
            Some("ocr")
        );
        assert!(ocr_response
            .pending_modalities
            .iter()
            .any(|item| item == "vlm"));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn search_surfaces_vlm_provenance_for_image_hits() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-vlm");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path = unique_dir("harborbeacon-admin-model-center-vlm").join("state.json");
        fs::create_dir_all(root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("images").join("porch.jpg"), b"fake-image").expect("write image");
        write_mock_model_center_state(
            &admin_state_path,
            "",
            Some("门口地面有一个快递箱和一把折叠雨伞"),
        );

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "快递箱".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("vlm search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.images.len(), 1);
        assert_eq!(response.images[0].provenance.as_deref(), Some("vlm"));
        assert_eq!(
            response.reply_pack.citations[0].provenance.as_deref(),
            Some("vlm")
        );
        assert!(response
            .supported_modalities
            .iter()
            .any(|item| item == "vlm"));
        assert!(!response.pending_modalities.iter().any(|item| item == "vlm"));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn image_search_does_not_match_filename_or_path() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-image-name-exclusion");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-name-exclusion").join("state.json");
        fs::create_dir_all(root.join("images").join("spring-folder")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(
            root.join("images")
                .join("spring-folder")
                .join("spring-photo.jpg"),
            b"fake-image",
        )
        .expect("write image");
        write_mock_model_center_state(&admin_state_path, "", Some("室内桌面上有一个黑色水杯"));

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "spring".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("image search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.images.len(), 0);
        assert_eq!(response.total_matches, 0);

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn image_search_matches_vlm_content_and_marks_content_provenance() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-image-content-match");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-content-match").join("state.json");
        fs::create_dir_all(root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        fs::write(root.join("images").join("neutral-name.jpg"), b"fake-image")
            .expect("write image");
        write_mock_model_center_state(
            &admin_state_path,
            "",
            Some("春天的公园里有绿色草地和盛开的花"),
        );

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        build_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "春天".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("image search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.images.len(), 1);
        assert_eq!(response.images[0].provenance.as_deref(), Some("vlm"));
        assert_eq!(response.images[0].content_source_kinds, vec!["vlm"]);
        assert!(response.images[0].content_indexed);
        assert!(!response.images[0].filename_match_used);

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn image_search_can_use_embedding_for_content_text_without_lexical_match() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-image-semantic-match");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-semantic-image").join("state.json");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        let image_path = root.join("content-photo-real-001.jpg");
        fs::write(&image_path, b"fake-image").expect("write image");
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service.load_or_refresh(&root).expect("seed manifest path");
        fs::write(
            snapshot.manifest_path,
            serde_json::to_string_pretty(&json!({
                "schema_version": 2,
                "root": root.to_string_lossy(),
                "root_signature": {
                    "modified_unix_millis": 0,
                    "size_bytes": 0
                },
                "generated_at": "200",
                "directories": [],
                "entries": [{
                    "modality": "image",
                    "path": image_path.to_string_lossy(),
                    "title": "content-photo-real-001.jpg",
                    "searchable_text": "a large tree with pink flowers",
                    "chunks": [{
                        "chunk_id": "chunk-0001",
                        "line_start": 1,
                        "line_end": 1,
                        "text": "a large tree with pink flowers",
                        "source_kind": "vlm"
                    }],
                    "text_sources": [{
                        "source_kind": "vlm",
                        "provider_key": "mock-vlm",
                        "text": "a large tree with pink flowers"
                    }],
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 10
                    }
                }]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        write_mock_model_center_state_with_embed(
            &admin_state_path,
            json!({
                "春天": [1.0, 0.0],
                "a large tree with pink flowers": [0.98, 0.02]
            }),
        );

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        warm_existing_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "春天".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("image semantic search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.images.len(), 1);
        assert_eq!(response.images[0].provenance.as_deref(), Some("vlm"));
        assert_eq!(response.images[0].content_source_kinds, vec!["vlm"]);
        assert!(response.images[0].content_indexed);
        assert!(!response.images[0].filename_match_used);
        assert!(response.images[0].matched_terms.is_empty());
        assert_eq!(response.images[0].lexical_score, Some(0.0));
        assert!(
            response.images[0].embedding_score.unwrap_or_default() > 0.9,
            "expected semantic image match to be driven by embedding score"
        );

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn image_search_rejects_low_confidence_semantic_only_match() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-image-semantic-low-confidence");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let admin_state_path =
            unique_dir("harborbeacon-admin-model-center-semantic-image-low").join("state.json");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::create_dir_all(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        )
        .expect("create admin state dir");
        let image_path = root.join("demo-status-card.png");
        fs::write(&image_path, b"fake-image").expect("write image");
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service.load_or_refresh(&root).expect("seed manifest path");
        fs::write(
            snapshot.manifest_path,
            serde_json::to_string_pretty(&json!({
                "schema_version": 2,
                "root": root.to_string_lossy(),
                "root_signature": {
                    "modified_unix_millis": 0,
                    "size_bytes": 0
                },
                "generated_at": "200",
                "directories": [],
                "entries": [{
                    "modality": "image",
                    "path": image_path.to_string_lossy(),
                    "title": "demo-status-card.png",
                    "searchable_text": "harbor demo status card with runtime ready text",
                    "chunks": [{
                        "chunk_id": "chunk-0001",
                        "line_start": 1,
                        "line_end": 1,
                        "text": "harbor demo status card with runtime ready text",
                        "source_kind": "vlm"
                    }],
                    "text_sources": [{
                        "source_kind": "vlm",
                        "provider_key": "mock-vlm",
                        "text": "harbor demo status card with runtime ready text"
                    }],
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 10
                    }
                }]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        write_mock_model_center_state_with_embed(
            &admin_state_path,
            json!({
                "春天的照片": [1.0, 0.0],
                "harbor demo status card with runtime ready text": [0.32, 0.947417]
            }),
        );

        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_state_path);
        warm_existing_search_index(&root, &index_root);
        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "春天的照片".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("image semantic search");
        std::env::remove_var("HARBOR_ADMIN_STATE_PATH");

        assert_eq!(response.images.len(), 0);
        assert_eq!(response.total_matches, 0);

        cleanup_dir(&root);
        cleanup_dir(&index_root);
        cleanup_dir(
            admin_state_path
                .parent()
                .expect("admin state path parent directory"),
        );
    }

    #[test]
    fn video_search_uses_sidecar_content_without_filename_guessing() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-video-sidecar");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(root.join("videos")).expect("create videos");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(root.join("videos").join("spring-clip.mp4"), b"fake-video").expect("write video");
        fs::write(
            root.join("videos").join("spring-clip.json"),
            r#"{"summary":"garage camera clip: courier delivered a box at the door"}"#,
        )
        .expect("write video sidecar");
        build_search_index(&root, &index_root);

        let sidecar_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "courier".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: false,
            include_videos: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("video sidecar search");
        let filename_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "spring".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: false,
            include_videos: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("video filename search");

        assert_eq!(sidecar_response.videos.len(), 1);
        assert_eq!(
            sidecar_response.videos[0].provenance.as_deref(),
            Some("video_sidecar")
        );
        assert_eq!(
            sidecar_response.videos[0].content_source_kinds,
            vec!["video_sidecar"]
        );
        assert!(sidecar_response.videos[0].content_indexed);
        assert!(sidecar_response.videos[0].content_match_used);
        assert!(!sidecar_response.videos[0].filename_match_used);
        assert_eq!(filename_response.videos.len(), 0);
        assert_eq!(
            filename_response.empty_reason.as_deref(),
            Some("video_content_no_match")
        );

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn video_search_focus_paths_restrict_follow_up_scope() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-video-focus");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        let video_dir = root.join("videos");
        fs::create_dir_all(&video_dir).expect("create videos");
        fs::create_dir_all(&index_root).expect("create index root");
        let first_video = video_dir.join("a-porch.mp4");
        let second_video = video_dir.join("b-garage.mp4");
        fs::write(&first_video, b"fake-video-a").expect("write first video");
        fs::write(&second_video, b"fake-video-b").expect("write second video");
        fs::write(
            video_dir.join("a-porch.json"),
            r#"{"summary":"courier left a parcel near the front door"}"#,
        )
        .expect("write first sidecar");
        fs::write(
            video_dir.join("b-garage.json"),
            r#"{"summary":"courier left a parcel near the garage shelf"}"#,
        )
        .expect("write second sidecar");
        build_search_index(&root, &index_root);

        let first_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "courier parcel".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: false,
            include_videos: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("video search");
        let focused_response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "courier parcel".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            focus_paths: vec![first_video.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: false,
            include_videos: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("focused video search");

        assert_eq!(first_response.videos.len(), 2);
        assert_eq!(focused_response.videos.len(), 1);
        assert_eq!(
            focused_response.videos[0].path,
            first_video.to_string_lossy()
        );
        assert!(focused_response.videos[0].content_match_used);

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn video_search_surfaces_vlm_keyframe_provenance() {
        let _guard = INDEX_TEST_LOCK.lock().expect("lock");
        let root = unique_dir("harborbeacon-knowledge-video-vlm");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&index_root).expect("create index root");
        let video_path = root.join("porch-clip.mp4");
        let frame_path = index_root.join("video-keyframes").join("frame-01.jpg");
        fs::create_dir_all(frame_path.parent().expect("frame parent")).expect("frame dir");
        fs::write(&video_path, b"fake-video").expect("write video");
        fs::write(&frame_path, b"fake-frame").expect("write frame");
        let frame_path_text = frame_path.to_string_lossy().into_owned();
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service.load_or_refresh(&root).expect("seed manifest path");
        fs::write(
            snapshot.manifest_path,
            serde_json::to_string_pretty(&json!({
                "schema_version": 2,
                "root": root.to_string_lossy(),
                "root_signature": {
                    "modified_unix_millis": 0,
                    "size_bytes": 0
                },
                "generated_at": "200",
                "directories": [],
                "entries": [{
                    "modality": "video",
                    "path": video_path.to_string_lossy(),
                    "title": "porch-clip.mp4",
                    "searchable_text": "keyframe 30%: 门口地面有一个快递箱",
                    "chunks": [{
                        "chunk_id": "chunk-0001",
                        "line_start": 1,
                        "line_end": 1,
                        "text": "keyframe 30%: 门口地面有一个快递箱",
                        "source_kind": "vlm_keyframe",
                        "source_path": frame_path_text.clone()
                    }],
                    "text_sources": [{
                        "source_kind": "vlm_keyframe",
                        "source_path": frame_path_text.clone(),
                        "provider_key": "mock-vlm",
                        "text": "keyframe 30%: 门口地面有一个快递箱"
                    }],
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 10
                    }
                }]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");

        let response = KnowledgeSearchService::search(KnowledgeSearchRequest {
            query: "快递箱".to_string(),
            configured_roots: vec![root.to_string_lossy().into_owned()],
            index_root: Some(index_root.to_string_lossy().into_owned()),
            roots: vec![root.to_string_lossy().into_owned()],
            include_documents: false,
            include_images: false,
            include_videos: true,
            limit: 5,
            ..KnowledgeSearchRequest::new("")
        })
        .expect("video vlm search");

        assert_eq!(response.videos.len(), 1);
        assert_eq!(
            response.videos[0].provenance.as_deref(),
            Some("vlm_keyframe")
        );
        assert_eq!(
            response.videos[0].source_path.as_deref(),
            Some(frame_path_text.as_str())
        );
        assert_eq!(
            response.videos[0].content_source_kinds,
            vec!["vlm_keyframe"]
        );
        assert!(response.videos[0].content_indexed);
        assert!(response.videos[0].content_match_used);
        assert!(!response.videos[0].filename_match_used);
        assert_eq!(response.reply_pack.citations.len(), 1);
        assert_eq!(response.reply_pack.citations[0].modality, "video");
        assert!(response.reply_pack.summary.contains("视频片段"));

        cleanup_dir(&root);
        cleanup_dir(&index_root);
    }
}
