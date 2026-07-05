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
    load_embedding_store, KnowledgeEmbeddingStore, KnowledgeIndexChunk, KnowledgeIndexConfig,
    KnowledgeIndexEntry, KnowledgeIndexService, KnowledgeModality,
};
use crate::runtime::model_center;

const DEFAULT_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeSearchRequest {
    pub query: String,
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
}

impl KnowledgeSearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
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
    pub embedding_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default)]
    pub snippet: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeSearchCitation {
    pub title: String,
    pub path: String,
    pub modality: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default)]
    pub matched_terms: Vec<String>,
    #[serde(default)]
    pub preview: Option<String>,
    pub score: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_score: Option<f32>,
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
        let query_terms = build_query_terms(&query);
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
        let query_embedding = model_center::run_embedding_with_state(&query, &model_center_state);
        let query_embedding_vector = (!query_embedding.vector.is_empty()
            && query_embedding.available)
            .then_some(query_embedding.vector.clone());
        if request.require_embeddings && query_embedding_vector.is_none() {
            return Ok(KnowledgeSearchResponse::degraded(
                query,
                root_strings,
                request.privacy_level,
                request.resource_profile,
                "embedding_unavailable",
                format!(
                    "当前检索要求 embedding，但 embedding 模型不可用：{}",
                    query_embedding.summary
                ),
            ));
        }
        let mut warnings = Vec::new();
        let embedding_model_degraded = query_embedding_vector.is_none();
        if embedding_model_degraded {
            warnings.push(format!(
                "Embedding 模型不可用，已降级为本地词法检索：{}",
                query_embedding.summary
            ));
        }
        let mut seen_hits = HashSet::new();
        let mut candidates = Vec::new();
        let mut diagnostics = SearchDiagnostics::default();
        let mut cached_embedding_total = 0usize;
        let mut cached_embedding_missing = 0usize;
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
            let embedding_store_path = index_service.embedding_store_path_for_root(root);
            let embedding_store = if query_embedding_vector.is_some() {
                match load_embedding_store(&embedding_store_path) {
                    Ok(store) => store,
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
            let mut embedding_vectors = HashMap::new();
            for entry in embedding_store
                .entries
                .iter()
                .filter(|entry| !entry.vector.is_empty())
            {
                embedding_vectors.insert(entry.key.clone(), entry.vector.clone());
                embedding_vectors.insert(
                    embedding_key(
                        &normalize_search_path_text(&entry.path),
                        entry.chunk_id.as_deref(),
                    ),
                    entry.vector.clone(),
                );
            }
            for entry in &snapshot.manifest.entries {
                diagnostics.observe_entry(entry);
                if !focus_paths.is_empty() && !entry_matches_focus_paths(&entry.path, &focus_paths)
                {
                    continue;
                }
                if !modality_included(entry.modality, &request) {
                    continue;
                }
                for mut candidate in build_hit_candidates_from_index_entry(entry, &query_terms) {
                    let embedding_key =
                        embedding_key(&candidate.hit.path, candidate.hit.chunk_id.as_deref());
                    let embedding_score =
                        query_embedding_vector.as_deref().and_then(|query_vector| {
                            cached_embedding_total += 1;
                            embedding_vectors.get(&embedding_key).map(|chunk_vector| {
                                cosine_similarity(query_vector, chunk_vector).clamp(0.0, 1.0)
                            })
                        });
                    if query_embedding_vector.is_some() && embedding_score.is_none() {
                        cached_embedding_missing += 1;
                    }
                    candidate.hit.embedding_score = embedding_score;
                    if !candidate_has_retrieval_signal(&candidate, &retrieval) {
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

        if cached_embedding_total > 0 && cached_embedding_missing > 0 {
            warnings.push(format!(
                "Embedding cache 覆盖不足：{} / {} 个候选缺少 cached chunk embedding，已仅用现有缓存参与向量召回。",
                cached_embedding_missing, cached_embedding_total
            ));
        }

        apply_rrf_scores(&mut candidates, &retrieval);
        sort_candidates_by_final_score(&mut candidates);
        let total_matches = candidates.len();
        candidates.truncate(retrieval.candidate_limit.max(1));
        if retrieval.rerank_enabled {
            apply_local_rerank(
                &mut candidates,
                &query,
                &retrieval,
                &model_center_state,
                &mut warnings,
            );
        }
        sort_candidates_by_final_score(&mut candidates);

        let limit = request.limit.clamp(1, 10);
        let selected_candidates = if retrieval.mmr_enabled {
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
    let mut pending = vec!["audio".to_string()];

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
) -> Vec<SearchCandidate> {
    let path = Path::new(&entry.path);
    let chunks = if entry.chunks.is_empty() {
        vec![KnowledgeIndexChunk {
            chunk_id: "chunk-0001".to_string(),
            line_start: 1,
            line_end: entry.searchable_text.lines().count().max(1),
            text: entry.searchable_text.clone(),
            source_kind: entry.modality.as_str().to_string(),
            source_path: entry.sidecar_path.clone(),
        }]
    } else {
        entry.chunks.clone()
    };

    chunks
        .iter()
        .filter_map(|chunk| {
            build_hit_candidate(
                path,
                entry.modality,
                Some(chunk.text.as_str()),
                query_terms,
                Some(chunk),
            )
        })
        .collect()
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
            line_start: hit.line_start,
            line_end: hit.line_end,
            matched_terms: hit.matched_terms.clone(),
            preview: hit.snippet.clone(),
            score: hit.score,
            lexical_score: hit.lexical_score,
            embedding_score: hit.embedding_score,
            hybrid_score: hit.hybrid_score,
            rerank_score: hit.rerank_score,
            provenance: hit.provenance.clone(),
            source_path: hit.source_path.clone(),
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
            "已检索知识库，但暂时没有找到与“{}”相关的文档、图片、视频或 OCR 线索。",
            query
        );
    }

    let mut parts = Vec::new();
    if !documents.is_empty() {
        parts.push(format!("{} 个文档片段", documents.len()));
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
    query_terms: &[String],
    chunk: Option<&KnowledgeIndexChunk>,
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
    let semantic_only = score == 0
        && matches!(
            modality,
            KnowledgeModality::Image | KnowledgeModality::Video
        )
        && content_indexed
        && searchable_text.is_some_and(|text| !text.trim().is_empty());

    if score == 0 && !semantic_only && !searchable_text.is_some_and(|text| !text.trim().is_empty())
    {
        return None;
    }

    let lexical_score = (score as f32 / 100.0).clamp(0.0, 1.0);

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
            embedding_score: None,
            hybrid_score: Some(lexical_score),
            rerank_score: None,
            chunk_id: chunk.map(|item| item.chunk_id.clone()),
            line_start: chunk.map(|item| item.line_start),
            line_end: chunk.map(|item| item.line_end),
            snippet: searchable_text.and_then(|text| build_snippet(text, &matched_terms)),
            matched_terms,
            provenance: chunk
                .map(|item| item.source_kind.clone())
                .filter(|value| !value.trim().is_empty()),
            source_path: chunk.and_then(|item| item.source_path.clone()),
            content_source_kinds,
            content_indexed,
            filename_match_used,
            content_match_used,
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

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || right.is_empty() || left.len() != right.len() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (lhs, rhs) in left.iter().zip(right.iter()) {
        dot += lhs * rhs;
        left_norm += lhs * lhs;
        right_norm += rhs * rhs;
    }
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

fn modality_included(modality: KnowledgeModality, request: &KnowledgeSearchRequest) -> bool {
    match modality {
        KnowledgeModality::Document => request.include_documents,
        KnowledgeModality::Image => request.include_images,
        KnowledgeModality::Audio => false,
        KnowledgeModality::Video => request.include_videos,
    }
}

fn candidate_has_retrieval_signal(
    candidate: &SearchCandidate,
    settings: &KnowledgeRetrievalSettings,
) -> bool {
    if candidate.hit.lexical_score.unwrap_or_default() > 0.0 {
        return true;
    }
    let embedding_score = candidate.hit.embedding_score.unwrap_or_default();
    if candidate.semantic_only {
        embedding_score >= settings.semantic_only_min_score
    } else {
        embedding_score >= settings.vector_min_score
    }
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
    query: &str,
    settings: &KnowledgeRetrievalSettings,
    model_center_state: &AdminModelCenterState,
    warnings: &mut Vec<String>,
) {
    if candidates.is_empty() {
        return;
    }
    let top_k = settings.rerank_top_k.min(candidates.len()).max(1);
    let documents = candidates
        .iter()
        .take(top_k)
        .map(rerank_passage_for_candidate)
        .collect::<Vec<_>>();
    let execution =
        model_center::run_rerank_with_state(query, &documents, top_k, model_center_state);
    if !execution.available {
        warnings.push(format!(
            "Reranker 不可用，已保留 RRF 排序：{}",
            execution.summary
        ));
        return;
    }

    for candidate in candidates.iter_mut().take(top_k) {
        candidate.final_score = (0.25 * candidate.rrf_score).clamp(0.0, 1.0);
        candidate.hit.score = score_to_compat(candidate.final_score);
        candidate.hit.rerank_score = None;
    }

    let mut applied = 0usize;
    for score in execution.scores {
        if score.index >= top_k
            || !score.score.is_finite()
            || score.score < settings.rerank_min_score
        {
            continue;
        }
        let rerank_score = score.score.clamp(0.0, 1.0);
        let candidate = &mut candidates[score.index];
        candidate.hit.rerank_score = Some(rerank_score);
        candidate.final_score = (0.75 * rerank_score + 0.25 * candidate.rrf_score).clamp(0.0, 1.0);
        candidate.hit.score = score_to_compat(candidate.final_score);
        applied += 1;
    }

    if applied == 0 {
        for candidate in candidates.iter_mut().take(top_k) {
            candidate.final_score = candidate.rrf_score;
            candidate.hit.score = score_to_compat(candidate.final_score);
            candidate.hit.rerank_score = None;
        }
        warnings.push("Reranker 返回分数低于阈值，已保留 RRF 排序。".to_string());
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
    build_query_terms(&text).into_iter().collect()
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

fn build_query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut cjk = String::new();

    for ch in query.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if !cjk.is_empty() {
                push_cjk_terms(&cjk, &mut terms);
                cjk.clear();
            }
            ascii.push(ch);
            continue;
        }
        if !ascii.is_empty() {
            push_ascii_term(&ascii, &mut terms);
            ascii.clear();
        }
        if is_cjk(ch) {
            cjk.push(ch);
        } else if !cjk.is_empty() {
            push_cjk_terms(&cjk, &mut terms);
            cjk.clear();
        }
    }

    if !ascii.is_empty() {
        push_ascii_term(&ascii, &mut terms);
    }
    if !cjk.is_empty() {
        push_cjk_terms(&cjk, &mut terms);
    }

    if terms.is_empty() {
        let fallback = query.trim().to_string();
        if !fallback.is_empty() {
            terms.push(fallback);
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn push_ascii_term(term: &str, terms: &mut Vec<String>) {
    let value = term.trim();
    if value.len() >= 2 && !is_stop_term(value) {
        terms.push(value.to_string());
    }
}

fn push_cjk_terms(term: &str, terms: &mut Vec<String>) {
    let chars = term.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return;
    }

    let full = chars.iter().collect::<String>();
    if !is_stop_term(&full) {
        terms.push(full.clone());
    }

    if chars.len() < 2 {
        return;
    }

    for window in chars.windows(2) {
        let token = window.iter().collect::<String>();
        if !is_stop_term(&token) {
            terms.push(token);
        }
    }
}

fn is_stop_term(term: &str) -> bool {
    matches!(
        term.trim(),
        "文件"
            | "文档"
            | "图片"
            | "照片"
            | "资料"
            | "内容"
            | "搜索"
            | "检索"
            | "查找"
            | "找到"
            | "相关"
            | "有关"
            | "search"
            | "find"
            | "files"
            | "file"
            | "image"
            | "images"
            | "photo"
            | "photos"
            | "document"
            | "documents"
    )
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF
    )
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{json, Value};

    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
        PrivacyLevel,
    };
    use crate::runtime::admin_console::{AdminConsoleState, AdminModelCenterState};

    use super::{
        KnowledgeIndexConfig, KnowledgeIndexService, KnowledgeSearchRequest, KnowledgeSearchService,
    };

    static INDEX_TEST_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(hit.chunk_id.as_deref(), Some("chunk-0002"));
        assert_eq!(hit.line_start, Some(5));
        assert_eq!(hit.line_end, Some(6));
        assert!(hit
            .snippet
            .as_deref()
            .unwrap_or_default()
            .contains("樱花季"));
        assert_eq!(response.reply_pack.citations.len(), 1);
        assert_eq!(
            response.reply_pack.citations[0].chunk_id.as_deref(),
            Some("chunk-0002")
        );
        assert_eq!(response.reply_pack.citations[0].line_start, Some(5));
        assert_eq!(response.reply_pack.citations[0].line_end, Some(6));
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
        write_mock_model_center_state_with_rerank(&admin_state_path, json!([0.2, 0.95]));

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
                "schema_version": 1,
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
                "schema_version": 1,
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
                "schema_version": 1,
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
