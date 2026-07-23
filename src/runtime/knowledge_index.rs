//! Local knowledge index and manifest storage for HarborBeacon search.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use markitdown::model::ConversionOptions;
use markitdown::MarkItDown;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::media_tools::{resolve_ffmpeg_bin, resolve_ffprobe_bin};
use crate::runtime::{admin_console::AdminModelCenterState, model_center};

pub const KNOWLEDGE_INDEX_ROOT_ENV: &str = "HARBOR_KNOWLEDGE_INDEX_ROOT";

const DEFAULT_INDEX_DIR: &str = ".harborbeacon/knowledge-index";
const MAX_INDEX_TEXT_BYTES: u64 = 512 * 1024;
const CHILD_CHUNK_TARGET_TOKENS: usize = 240;
const CHILD_CHUNK_OVERLAP_TOKENS: usize = 48;
const PARENT_CHUNK_TARGET_TOKENS: usize = 750;
const PARENT_CHUNK_MAX_TOKENS: usize = 900;
const INDEX_SCHEMA_VERSION: u32 = 2;
const EMBEDDING_STORE_SCHEMA_VERSION: u32 = 3;
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "json", "csv", "html", "htm", "yaml", "yml", "log", "xml", "rss",
    "atom", "pdf", "docx", "pptx", "xlsx", "zip",
];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "flac", "aac", "ogg", "opus"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
const SIDECAR_EXTENSIONS: &[&str] = &["txt", "md", "markdown", "json", "csv", "yaml", "yml"];
const MARKITDOWN_EXTENSIONS: &[&str] = &[
    "html", "htm", "xml", "rss", "atom", "pdf", "docx", "pptx", "xlsx", "zip",
];
const VIDEO_KEYFRAME_MIN_COUNT: usize = 5;
const VIDEO_KEYFRAME_MAX_COUNT: usize = 48;
const VIDEO_KEYFRAME_RETRY_OFFSETS_SECONDS: &[f64] = &[0.0, 1.0, -1.0, 2.0, -2.0];
const VIDEO_TOOL_POLL_INTERVAL: Duration = Duration::from_millis(50);
const VIDEO_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const VIDEO_FRAME_EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
const VIDEO_FRAME_QUALITY_TIMEOUT: Duration = Duration::from_secs(15);
const VIDEO_SCENE_DETECT_TIMEOUT: Duration = Duration::from_secs(120);
const VIDEO_FRAME_MIN_LUMA: f64 = 6.0;
const VIDEO_FRAME_MAX_LUMA: f64 = 238.0;
const VIDEO_FRAME_MAX_BLUR_SCORE: f64 = 12.0;
const VIDEO_SCENE_SAMPLE_FPS: f64 = 2.0;
const VIDEO_SCENE_CHANGE_THRESHOLD: f64 = 0.28;
const VIDEO_SCENE_TARGET_RATIO: f64 = 0.6;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeModality {
    Document,
    Image,
    Audio,
    Video,
}

impl KnowledgeModality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct KnowledgeFileSignature {
    pub modified_unix_millis: u128,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct KnowledgeIndexChunk {
    pub chunk_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_id: Option<String>,
    #[serde(default)]
    pub section_path: Vec<String>,
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
    #[serde(default)]
    pub indexed_text: String,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct KnowledgeIndexParentChunk {
    pub parent_id: String,
    #[serde(default)]
    pub section_path: Vec<String>,
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
    #[serde(default)]
    pub child_ids: Vec<String>,
    #[serde(default)]
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct KnowledgeIndexTextSource {
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeIndexEntry {
    pub modality: KnowledgeModality,
    pub path: String,
    pub title: String,
    pub searchable_text: String,
    #[serde(default)]
    pub parent_chunks: Vec<KnowledgeIndexParentChunk>,
    #[serde(default)]
    pub chunks: Vec<KnowledgeIndexChunk>,
    #[serde(default)]
    pub text_sources: Vec<KnowledgeIndexTextSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_path: Option<String>,
    pub file_signature: KnowledgeFileSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_signature: Option<KnowledgeFileSignature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processing_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct VideoKeyframe {
    path: PathBuf,
    timestamp_seconds: f64,
    percent: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct VideoKeyframeExtraction {
    frames: Vec<VideoKeyframe>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct VideoFrameQuality {
    luminance: Option<f64>,
    blur_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct CommandCapture {
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeIndexDirectory {
    pub path: String,
    pub signature: KnowledgeFileSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeIndexManifest {
    pub schema_version: u32,
    pub root: String,
    pub root_signature: KnowledgeFileSignature,
    pub generated_at: String,
    #[serde(default)]
    pub directories: Vec<KnowledgeIndexDirectory>,
    #[serde(default)]
    pub entries: Vec<KnowledgeIndexEntry>,
}

impl Default for KnowledgeIndexManifest {
    fn default() -> Self {
        Self {
            schema_version: INDEX_SCHEMA_VERSION,
            root: String::new(),
            root_signature: KnowledgeFileSignature::default(),
            generated_at: current_timestamp(),
            directories: Vec::new(),
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KnowledgeIndexRefreshStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub reused: usize,
    pub skipped_directories: usize,
    pub rebuilt: bool,
    pub persisted: bool,
    pub persist_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KnowledgeEmbeddingWarmupStats {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub degraded: bool,
    pub persist_error: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KnowledgeIndexSnapshot {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: KnowledgeIndexManifest,
    pub stats: KnowledgeIndexRefreshStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeIndexConfig {
    pub index_root: PathBuf,
}

impl KnowledgeIndexConfig {
    pub fn from_env() -> Result<Self, String> {
        let index_root = env::var(KNOWLEDGE_INDEX_ROOT_ENV)
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(default_index_root);
        Self::new(index_root)
    }

    pub fn new(index_root: impl Into<PathBuf>) -> Result<Self, String> {
        let index_root = index_root.into();
        if index_root.as_os_str().is_empty() {
            return Err("knowledge index root cannot be empty".to_string());
        }
        Ok(Self { index_root })
    }
}

#[derive(Debug, Clone)]
pub struct KnowledgeIndexService {
    config: KnowledgeIndexConfig,
}

impl KnowledgeIndexService {
    pub fn new() -> Result<Self, String> {
        let config = KnowledgeIndexConfig::from_env()?;
        Self::from_config(config)
    }

    pub fn from_config(config: KnowledgeIndexConfig) -> Result<Self, String> {
        fs::create_dir_all(&config.index_root).map_err(|error| {
            format!(
                "failed to create knowledge index root {}: {error}",
                config.index_root.display()
            )
        })?;
        Ok(Self { config })
    }

    pub fn load_or_refresh(&self, root: &Path) -> Result<KnowledgeIndexSnapshot, String> {
        if !root.exists() {
            return Err(format!("knowledge root not found: {}", root.display()));
        }

        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let manifest_path = self.manifest_path_for_root(&root);
        let current_root_signature = directory_signature(&root)?;
        let mut old_state = load_manifest_state(&manifest_path).unwrap_or_default();

        if old_state.manifest.entries.is_empty() && old_state.manifest.directories.is_empty() {
            old_state.manifest.root = root.to_string_lossy().into_owned();
            old_state.manifest.root_signature = current_root_signature.clone();
        }

        let mut stats = KnowledgeIndexRefreshStats::default();
        stats.rebuilt =
            old_state.manifest.entries.is_empty() && old_state.manifest.directories.is_empty();
        let mut new_state = KnowledgeIndexState::new(
            root.clone(),
            manifest_path.clone(),
            current_root_signature.clone(),
        );
        refresh_directory(
            &root,
            &self.config.index_root,
            &old_state,
            &mut new_state,
            &mut stats,
        )?;

        new_state.manifest.generated_at = current_timestamp();
        new_state.manifest.root = root.to_string_lossy().into_owned();
        new_state.manifest.root_signature = current_root_signature;
        new_state.manifest.entries.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.modality.as_str().cmp(right.modality.as_str()))
        });
        new_state
            .manifest
            .directories
            .sort_by(|left, right| left.path.cmp(&right.path));

        if let Err(error) = save_manifest(&new_state.manifest_path, &new_state.manifest) {
            stats.persist_error = Some(error);
        } else {
            stats.persisted = true;
        }

        let manifest = new_state.manifest;
        stats.removed = old_state
            .manifest
            .entries
            .len()
            .saturating_sub(stats.reused + stats.updated);

        Ok(KnowledgeIndexSnapshot {
            root,
            manifest_path,
            manifest,
            stats,
        })
    }

    pub fn load_existing(&self, root: &Path) -> Result<KnowledgeIndexSnapshot, String> {
        if !root.exists() {
            return Err(format!("knowledge root not found: {}", root.display()));
        }

        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let manifest_path = self.manifest_path_for_root(&root);
        if !manifest_path.exists() {
            return Err(format!(
                "knowledge index manifest is missing for {}; queue /api/knowledge/index/run and follow Index jobs before searching",
                root.display()
            ));
        }

        let state = load_manifest_state(&manifest_path)?;
        if state.manifest.root.trim().is_empty() {
            return Err(format!(
                "knowledge index manifest is empty or incompatible for {}; queue /api/knowledge/index/run before searching",
                root.display()
            ));
        }

        Ok(KnowledgeIndexSnapshot {
            root,
            manifest_path,
            manifest: state.manifest,
            stats: KnowledgeIndexRefreshStats::default(),
        })
    }

    fn manifest_path_for_root(&self, root: &Path) -> PathBuf {
        self.config
            .index_root
            .join(format!("{}.json", root_storage_key(root)))
    }

    pub fn embedding_store_path_for_root(&self, root: &Path) -> PathBuf {
        self.config
            .index_root
            .join(format!("{}.embeddings.json", root_storage_key(root)))
    }

    pub fn embedding_warmup_candidate_count(&self, snapshot: &KnowledgeIndexSnapshot) -> usize {
        snapshot
            .manifest
            .entries
            .iter()
            .flat_map(embedding_chunks_for_entry)
            .filter(|chunk| !chunk.text.trim().is_empty())
            .count()
    }

    pub fn warm_embedding_cache(
        &self,
        snapshot: &KnowledgeIndexSnapshot,
        model_center_state: &AdminModelCenterState,
    ) -> KnowledgeEmbeddingWarmupStats {
        let mut stats = KnowledgeEmbeddingWarmupStats::default();
        let embedding_store_path = self.embedding_store_path_for_root(&snapshot.root);
        let mut store = match load_embedding_store(&embedding_store_path) {
            Ok(store) => store,
            Err(error) => {
                stats.degraded = true;
                stats.last_error = Some(error);
                KnowledgeEmbeddingStore {
                    schema_version: EMBEDDING_STORE_SCHEMA_VERSION,
                    root: snapshot.root.to_string_lossy().into_owned(),
                    ..KnowledgeEmbeddingStore::default()
                }
            }
        };
        if store.schema_version == 0 {
            store.schema_version = EMBEDDING_STORE_SCHEMA_VERSION;
        }
        store.root = snapshot.root.to_string_lossy().into_owned();

        let mut dirty = false;
        if let Some(identity) =
            model_center::embedding_endpoint_identity_with_state(model_center_state)
        {
            let identity_matches = embedding_store_matches_identity(&store, &identity);
            if store.entries.is_empty() && !identity_matches {
                store.provider_key = Some(identity.provider_key);
                store.model_endpoint_id = Some(identity.model_endpoint_id);
                store.model_name = Some(identity.model_name);
                dirty = true;
            }
        }

        if !store.entries.is_empty() {
            let probe_text = snapshot
                .manifest
                .entries
                .iter()
                .flat_map(embedding_chunks_for_entry)
                .map(|chunk| {
                    if chunk.indexed_text.trim().is_empty() {
                        chunk.text.trim().to_string()
                    } else {
                        chunk.indexed_text.trim().to_string()
                    }
                })
                .find(|text| !text.is_empty());
            if let Some(probe_text) = probe_text {
                let execution =
                    model_center::run_embedding_with_state(&probe_text, model_center_state);
                if execution.available && !execution.vector.is_empty() {
                    let execution_model_name = execution
                        .model_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let identity_matches = store.provider_key.as_deref()
                        == Some(execution.provider_key.as_str())
                        && store.model_endpoint_id.as_deref()
                            == execution.model_endpoint_id.as_deref()
                        && store.model_name.as_deref() == execution_model_name
                        && store.vector_dimensions == Some(execution.vector.len());
                    if !identity_matches {
                        store.entries.clear();
                        store.vector_dimensions = None;
                        dirty = true;
                    }
                    store.provider_key = Some(execution.provider_key);
                    store.model_endpoint_id = execution.model_endpoint_id;
                    store.model_name = execution.model_name;
                    store.vector_dimensions = Some(execution.vector.len());
                }
            }
        }

        let mut entries_by_key = store
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.key.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut active_keys = HashSet::new();

        for entry in &snapshot.manifest.entries {
            for chunk in embedding_chunks_for_entry(entry) {
                let text = if chunk.indexed_text.trim().is_empty() {
                    chunk.text.trim()
                } else {
                    chunk.indexed_text.trim()
                };
                if text.is_empty() {
                    continue;
                }
                stats.total += 1;
                let key = embedding_key_for_chunk(&entry.path, Some(chunk.chunk_id.as_str()));
                let text_hash = embedding_text_hash(text);
                active_keys.insert(key.clone());
                if let Some(existing_index) = entries_by_key.get(&key).copied() {
                    let existing = &store.entries[existing_index];
                    if existing.text_hash == text_hash && !existing.vector.is_empty() {
                        stats.skipped += 1;
                        continue;
                    }
                }

                let execution = model_center::run_embedding_with_state(text, model_center_state);
                if !execution.available || execution.vector.is_empty() {
                    stats.failed += 1;
                    stats.degraded = true;
                    stats.last_error = Some(execution.summary);
                    continue;
                }

                let vector_dimensions = execution.vector.len();
                if store
                    .vector_dimensions
                    .is_some_and(|dimensions| dimensions != vector_dimensions)
                {
                    store.entries.clear();
                    entries_by_key.clear();
                }

                store.provider_key =
                    (!execution.provider_key.trim().is_empty()).then_some(execution.provider_key);
                store.model_endpoint_id = execution.model_endpoint_id;
                store.model_name = execution.model_name;
                store.vector_dimensions = Some(vector_dimensions);

                let embedding_entry = KnowledgeEmbeddingEntry {
                    key: key.clone(),
                    path: entry.path.clone(),
                    chunk_id: Some(chunk.chunk_id.clone()),
                    text_hash,
                    vector: execution.vector,
                };
                if let Some(existing_index) = entries_by_key.get(&key).copied() {
                    store.entries[existing_index] = embedding_entry;
                } else {
                    let index = store.entries.len();
                    store.entries.push(embedding_entry);
                    entries_by_key.insert(key, index);
                }
                stats.completed += 1;
                dirty = true;
            }
        }

        let before_retain_len = store.entries.len();
        store
            .entries
            .retain(|entry| active_keys.contains(&entry.key));
        if store.entries.len() != before_retain_len {
            dirty = true;
        }
        if dirty {
            if let Err(error) = save_embedding_store(&embedding_store_path, &store) {
                stats.degraded = true;
                stats.persist_error = Some(error.clone());
                stats.last_error = Some(error);
            }
        }
        stats
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct KnowledgeEmbeddingStore {
    pub schema_version: u32,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_endpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_dimensions: Option<usize>,
    #[serde(default)]
    pub entries: Vec<KnowledgeEmbeddingEntry>,
}

fn embedding_store_matches_identity(
    store: &KnowledgeEmbeddingStore,
    identity: &model_center::EmbeddingEndpointIdentity,
) -> bool {
    store.provider_key.as_deref() == Some(identity.provider_key.as_str())
        && store.model_endpoint_id.as_deref() == Some(identity.model_endpoint_id.as_str())
        && store.model_name.as_deref() == Some(identity.model_name.as_str())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct KnowledgeEmbeddingEntry {
    pub key: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    pub text_hash: String,
    #[serde(default)]
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
struct KnowledgeIndexState {
    manifest_path: PathBuf,
    manifest: KnowledgeIndexManifest,
}

impl KnowledgeIndexState {
    fn new(root: PathBuf, manifest_path: PathBuf, root_signature: KnowledgeFileSignature) -> Self {
        Self {
            manifest_path,
            manifest: KnowledgeIndexManifest {
                schema_version: INDEX_SCHEMA_VERSION,
                root: root.to_string_lossy().into_owned(),
                root_signature,
                generated_at: current_timestamp(),
                directories: Vec::new(),
                entries: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LoadedManifestState {
    manifest: KnowledgeIndexManifest,
    entries: HashMap<String, KnowledgeIndexEntry>,
}

fn load_manifest_state(path: &Path) -> Result<LoadedManifestState, String> {
    if !path.exists() {
        return Ok(LoadedManifestState::default());
    }

    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read knowledge index manifest {}: {error}",
            path.display()
        )
    })?;
    let manifest = serde_json::from_str::<KnowledgeIndexManifest>(&text).map_err(|error| {
        format!(
            "failed to parse knowledge index manifest {}: {error}",
            path.display()
        )
    })?;
    if manifest.schema_version != INDEX_SCHEMA_VERSION {
        return Ok(LoadedManifestState::default());
    }

    let entries = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry.clone()))
        .collect::<HashMap<_, _>>();
    Ok(LoadedManifestState { manifest, entries })
}

fn save_manifest(path: &Path, manifest: &KnowledgeIndexManifest) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create knowledge index directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let payload = serde_json::to_string_pretty(manifest).map_err(|error| {
        format!(
            "failed to serialize knowledge index manifest {}: {error}",
            path.display()
        )
    })?;
    fs::write(path, payload).map_err(|error| {
        format!(
            "failed to write knowledge index manifest {}: {error}",
            path.display()
        )
    })
}

pub fn load_embedding_store(path: &Path) -> Result<KnowledgeEmbeddingStore, String> {
    if !path.exists() {
        return Ok(KnowledgeEmbeddingStore {
            schema_version: EMBEDDING_STORE_SCHEMA_VERSION,
            ..KnowledgeEmbeddingStore::default()
        });
    }

    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read knowledge embedding store {}: {error}",
            path.display()
        )
    })?;
    let store = serde_json::from_str::<KnowledgeEmbeddingStore>(&text).map_err(|error| {
        format!(
            "failed to parse knowledge embedding store {}: {error}",
            path.display()
        )
    })?;
    if store.schema_version != EMBEDDING_STORE_SCHEMA_VERSION {
        return Ok(KnowledgeEmbeddingStore {
            schema_version: EMBEDDING_STORE_SCHEMA_VERSION,
            ..KnowledgeEmbeddingStore::default()
        });
    }
    Ok(store)
}

pub fn save_embedding_store(path: &Path, store: &KnowledgeEmbeddingStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create knowledge embedding directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let payload = serde_json::to_string_pretty(store).map_err(|error| {
        format!(
            "failed to serialize knowledge embedding store {}: {error}",
            path.display()
        )
    })?;
    fs::write(path, payload).map_err(|error| {
        format!(
            "failed to write knowledge embedding store {}: {error}",
            path.display()
        )
    })
}

fn embedding_chunks_for_entry(entry: &KnowledgeIndexEntry) -> Vec<KnowledgeIndexChunk> {
    if !entry.chunks.is_empty() {
        return entry.chunks.clone();
    }
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
}

fn embedding_key_for_chunk(path: &str, chunk_id: Option<&str>) -> String {
    format!("{}::{}", path, chunk_id.unwrap_or("chunk-0001"))
}

fn embedding_text_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn refresh_directory(
    path: &Path,
    index_root: &Path,
    old_state: &LoadedManifestState,
    new_state: &mut KnowledgeIndexState,
    stats: &mut KnowledgeIndexRefreshStats,
) -> Result<(), String> {
    let current_signature = directory_signature(path)?;
    let path_key = path.to_string_lossy().into_owned();
    new_state
        .manifest
        .directories
        .push(KnowledgeIndexDirectory {
            path: path_key.clone(),
            signature: current_signature.clone(),
        });

    let entries = fs::read_dir(path).map_err(|error| {
        format!(
            "failed to read knowledge directory {}: {error}",
            path.display()
        )
    })?;
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_dir() {
            if should_skip_directory(&child) {
                continue;
            }
            refresh_directory(&child, index_root, old_state, new_state, stats)?;
            continue;
        }

        let Some(modality) = classify_path(&child) else {
            continue;
        };
        let Some(index_entry) = refresh_entry(&child, modality, index_root, old_state, stats)?
        else {
            continue;
        };
        new_state.manifest.entries.push(index_entry);
    }

    Ok(())
}
fn refresh_entry(
    path: &Path,
    modality: KnowledgeModality,
    index_root: &Path,
    old_state: &LoadedManifestState,
    stats: &mut KnowledgeIndexRefreshStats,
) -> Result<Option<KnowledgeIndexEntry>, String> {
    let path_key = path.to_string_lossy().into_owned();
    let title = path
        .file_name()
        .and_then(|item| item.to_str())
        .unwrap_or(path_key.as_str())
        .to_string();
    let file_signature = file_signature(path)?;

    match modality {
        KnowledgeModality::Document => {
            if let Some(old_entry) = old_state.entries.get(&path_key) {
                if old_entry.file_signature == file_signature {
                    stats.reused += 1;
                    return Ok(Some(old_entry.clone()));
                }
            }
            let Some(text) = load_document_text(path) else {
                return Ok(None);
            };
            let text_sources = vec![KnowledgeIndexTextSource {
                source_kind: document_source_kind(path).to_string(),
                source_path: Some(path_key.clone()),
                provider_key: markitdown_provider_key(path),
                text: text.clone(),
            }];
            let hierarchy = build_text_chunk_hierarchy(&text_sources);
            if old_state.entries.contains_key(&path_key) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
            Ok(Some(KnowledgeIndexEntry {
                modality,
                path: path_key,
                title,
                searchable_text: text,
                parent_chunks: hierarchy.parents,
                chunks: hierarchy.children,
                text_sources,
                sidecar_path: None,
                file_signature,
                sidecar_signature: None,
                processing_warnings: Vec::new(),
            }))
        }
        KnowledgeModality::Image => {
            let (sidecar_path, sidecar_signature, text_sources) = image_text_sources(path)?;
            let searchable_text = join_text_sources(&text_sources);
            let hierarchy = build_text_chunk_hierarchy(&text_sources);
            if let Some(old_entry) = old_state.entries.get(&path_key) {
                if old_entry.file_signature == file_signature
                    && old_entry.sidecar_signature == sidecar_signature
                    && old_entry.text_sources == text_sources
                {
                    stats.reused += 1;
                    return Ok(Some(old_entry.clone()));
                }
            }
            if old_state.entries.contains_key(&path_key) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
            Ok(Some(KnowledgeIndexEntry {
                modality,
                path: path_key,
                title,
                searchable_text,
                parent_chunks: hierarchy.parents,
                chunks: hierarchy.children,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
                processing_warnings: Vec::new(),
            }))
        }
        KnowledgeModality::Audio => {
            let (sidecar_path, sidecar_signature, text_sources) =
                media_text_sources(path, modality)?;
            if text_sources.is_empty() {
                return Ok(None);
            }
            let searchable_text = join_text_sources(&text_sources);
            let hierarchy = build_text_chunk_hierarchy(&text_sources);
            if let Some(old_entry) = old_state.entries.get(&path_key) {
                if old_entry.file_signature == file_signature
                    && old_entry.sidecar_signature == sidecar_signature
                    && old_entry.text_sources == text_sources
                {
                    stats.reused += 1;
                    return Ok(Some(old_entry.clone()));
                }
            }
            if old_state.entries.contains_key(&path_key) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
            Ok(Some(KnowledgeIndexEntry {
                modality,
                path: path_key,
                title,
                searchable_text,
                parent_chunks: hierarchy.parents,
                chunks: hierarchy.children,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
                processing_warnings: Vec::new(),
            }))
        }
        KnowledgeModality::Video => {
            let (sidecar_path, sidecar_signature, text_sources, processing_warnings) =
                video_text_sources(path, index_root)?;
            if text_sources.is_empty() && processing_warnings.is_empty() {
                return Ok(None);
            }
            let searchable_text = join_text_sources(&text_sources);
            let hierarchy = build_text_chunk_hierarchy(&text_sources);
            if let Some(old_entry) = old_state.entries.get(&path_key) {
                if old_entry.file_signature == file_signature
                    && old_entry.sidecar_signature == sidecar_signature
                    && old_entry.text_sources == text_sources
                    && old_entry.processing_warnings == processing_warnings
                {
                    stats.reused += 1;
                    return Ok(Some(old_entry.clone()));
                }
            }
            if old_state.entries.contains_key(&path_key) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
            Ok(Some(KnowledgeIndexEntry {
                modality,
                path: path_key,
                title,
                searchable_text,
                parent_chunks: hierarchy.parents,
                chunks: hierarchy.children,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
                processing_warnings,
            }))
        }
    }
}

fn image_text_sources(
    image_path: &Path,
) -> Result<
    (
        Option<String>,
        Option<KnowledgeFileSignature>,
        Vec<KnowledgeIndexTextSource>,
    ),
    String,
> {
    let (sidecar_path, sidecar_signature, sidecar_text) = image_sidecar_state(image_path)?;
    let mut text_sources = Vec::new();
    if !sidecar_text.is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: "sidecar".to_string(),
            source_path: sidecar_path.clone(),
            provider_key: None,
            text: sidecar_text,
        });
    }

    let ocr = model_center::run_ocr(image_path);
    if ocr.available && !ocr.text.trim().is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: "ocr".to_string(),
            source_path: None,
            provider_key: Some(ocr.provider_key),
            text: ocr.text,
        });
    }

    let vlm = model_center::run_vlm_summary(image_path);
    if vlm.available && !vlm.text.trim().is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: "vlm".to_string(),
            source_path: None,
            provider_key: Some(vlm.provider_key),
            text: vlm.text,
        });
    }

    Ok((sidecar_path, sidecar_signature, text_sources))
}

fn media_text_sources(
    media_path: &Path,
    modality: KnowledgeModality,
) -> Result<
    (
        Option<String>,
        Option<KnowledgeFileSignature>,
        Vec<KnowledgeIndexTextSource>,
    ),
    String,
> {
    let (sidecar_path, sidecar_signature, sidecar_text) = media_sidecar_state(media_path)?;
    let mut text_sources = Vec::new();
    if !sidecar_text.is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: match modality {
                KnowledgeModality::Audio => "transcript",
                KnowledgeModality::Video => "video_sidecar",
                _ => "sidecar",
            }
            .to_string(),
            source_path: sidecar_path.clone(),
            provider_key: None,
            text: sidecar_text,
        });
    }
    Ok((sidecar_path, sidecar_signature, text_sources))
}

fn video_text_sources(
    video_path: &Path,
    index_root: &Path,
) -> Result<
    (
        Option<String>,
        Option<KnowledgeFileSignature>,
        Vec<KnowledgeIndexTextSource>,
        Vec<String>,
    ),
    String,
> {
    let (sidecar_path, sidecar_signature, sidecar_text) = media_sidecar_state(video_path)?;
    let mut text_sources = Vec::new();
    let mut processing_warnings = Vec::new();
    if !sidecar_text.is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: "video_sidecar".to_string(),
            source_path: sidecar_path.clone(),
            provider_key: None,
            text: sidecar_text,
        });
    }

    let extraction = extract_video_keyframes(video_path, index_root);
    processing_warnings.extend(extraction.warnings);
    for frame in extraction.frames {
        let vlm = model_center::run_vlm_summary(&frame.path);
        if vlm.available && !vlm.text.trim().is_empty() {
            text_sources.push(KnowledgeIndexTextSource {
                source_kind: "vlm_keyframe".to_string(),
                source_path: Some(frame.path.to_string_lossy().into_owned()),
                provider_key: Some(vlm.provider_key),
                text: format!(
                    "keyframe {}% at {:.3}s: {}",
                    format_keyframe_percent(frame.percent),
                    frame.timestamp_seconds,
                    vlm.text.trim()
                ),
            });
        } else {
            processing_warnings.push(format!(
                "VLM produced no searchable text for keyframe at {:.3}s ({:.1}%)",
                frame.timestamp_seconds, frame.percent
            ));
        }
    }

    for warning in &processing_warnings {
        eprintln!(
            "video keyframe warning for {}: {warning}",
            video_path.display()
        );
    }

    Ok((
        sidecar_path,
        sidecar_signature,
        text_sources,
        processing_warnings,
    ))
}

fn extract_video_keyframes(
    video_path: &Path,
    index_root: &Path,
) -> VideoKeyframeExtraction {
    let mut result = VideoKeyframeExtraction::default();
    let Some(ffmpeg_bin) = resolve_ffmpeg_bin() else {
        result
            .warnings
            .push("FFmpeg is unavailable; no video keyframes were extracted".to_string());
        return result;
    };
    let duration_seconds = match probe_video_duration_seconds(video_path) {
        Ok(duration) => duration,
        Err(error) => {
            result.warnings.push(error);
            return result;
        }
    };
    if duration_seconds <= 0.0 {
        result.warnings.push(format!(
            "video duration is not positive: {duration_seconds}"
        ));
        return result;
    }

    let output_dir = video_keyframe_cache_dir(index_root, video_path);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        result.warnings.push(format!(
            "failed to create video keyframe cache {}: {error}",
            output_dir.display()
        ));
        return result;
    }

    let uniform_targets = video_keyframe_targets(duration_seconds);
    let targets = match detect_video_scene_timestamps(
        &ffmpeg_bin,
        video_path,
        duration_seconds,
    ) {
        Ok(scene_timestamps) => merge_video_keyframe_targets(
            duration_seconds,
            &uniform_targets,
            &scene_timestamps,
        ),
        Err(error) => {
            result.warnings.push(format!(
                "content-aware scene detection failed; using time-based keyframes: {error}"
            ));
            uniform_targets
        }
    };

    for (index, target_timestamp) in targets.into_iter().enumerate() {
        let output_path = output_dir.join(format!("frame-{:03}.jpg", index + 1));
        let _ = fs::remove_file(&output_path);
        let mut attempt_errors = Vec::new();
        let mut attempted_timestamps = Vec::new();
        let mut selected = None;

        for offset in VIDEO_KEYFRAME_RETRY_OFFSETS_SECONDS {
            let timestamp = (target_timestamp + offset)
                .clamp(0.0, (duration_seconds - 0.001).max(0.0));
            if attempted_timestamps
                .iter()
                .any(|previous: &f64| (*previous - timestamp).abs() < 0.001)
            {
                continue;
            }
            attempted_timestamps.push(timestamp);
            let _ = fs::remove_file(&output_path);

            match extract_video_frame(
                &ffmpeg_bin,
                video_path,
                timestamp,
                &output_path,
            ) {
                Ok(()) => {}
                Err(error) => {
                    attempt_errors.push(format!("{timestamp:.3}s: {error}"));
                    continue;
                }
            }

            match probe_video_frame_quality(&ffmpeg_bin, &output_path) {
                Ok(quality) => {
                    if let Some(reason) = video_frame_rejection_reason(quality) {
                        attempt_errors.push(format!("{timestamp:.3}s: {reason}"));
                        let _ = fs::remove_file(&output_path);
                        continue;
                    }
                }
                Err(error) => {
                    result.warnings.push(format!(
                        "frame quality check unavailable at {timestamp:.3}s; accepted frame: {error}"
                    ));
                }
            }

            selected = Some(timestamp);
            break;
        }

        if let Some(timestamp_seconds) = selected {
            result.frames.push(VideoKeyframe {
                path: output_path,
                timestamp_seconds,
                percent: timestamp_seconds / duration_seconds * 100.0,
            });
        } else {
            result.warnings.push(format!(
                "failed to extract an acceptable keyframe near {target_timestamp:.3}s after {} attempts: {}",
                attempted_timestamps.len(),
                attempt_errors.join(" | ")
            ));
        }
    }

    if result.frames.is_empty() {
        result.warnings.push(format!(
            "no usable keyframes were extracted from {}",
            video_path.display()
        ));
    } else if result.frames.len() < video_keyframe_count(duration_seconds) {
        result.warnings.push(format!(
            "partially extracted video keyframes: {}/{} succeeded",
            result.frames.len(),
            video_keyframe_count(duration_seconds)
        ));
    }

    result
}

fn video_keyframe_count(duration_seconds: f64) -> usize {
    let count = if duration_seconds <= 60.0 {
        VIDEO_KEYFRAME_MIN_COUNT
    } else if duration_seconds <= 600.0 {
        (duration_seconds / 30.0).ceil() as usize
    } else if duration_seconds <= 3_600.0 {
        (duration_seconds / 120.0).ceil().max(20.0) as usize
    } else {
        (duration_seconds / 300.0).ceil().max(30.0) as usize
    };
    count.clamp(VIDEO_KEYFRAME_MIN_COUNT, VIDEO_KEYFRAME_MAX_COUNT)
}

fn video_keyframe_targets(duration_seconds: f64) -> Vec<f64> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return Vec::new();
    }
    let count = video_keyframe_count(duration_seconds);
    (0..count)
        .map(|index| duration_seconds * ((index as f64 + 0.5) / count as f64))
        .collect()
}

fn detect_video_scene_timestamps(
    ffmpeg_bin: &str,
    video_path: &Path,
    duration_seconds: f64,
) -> Result<Vec<f64>, String> {
    let mut command = Command::new(ffmpeg_bin);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("info")
        .arg("-i")
        .arg(video_path)
        .arg("-an")
        .arg("-sn")
        .arg("-vf")
        .arg(format!(
            "fps={VIDEO_SCENE_SAMPLE_FPS:.1},scale=320:-2,select=gt(scene\\,{VIDEO_SCENE_CHANGE_THRESHOLD:.2}),metadata=print"
        ))
        .arg("-vsync")
        .arg("vfr")
        .arg("-f")
        .arg("null")
        .arg("-");
    let capture = run_command_with_timeout(&mut command, VIDEO_SCENE_DETECT_TIMEOUT)?;
    if !capture.success {
        return Err(format!(
            "FFmpeg scene detector exited unsuccessfully: {}",
            compact_command_error(&capture.stderr)
        ));
    }
    Ok(parse_video_scene_timestamps(
        &capture.stderr,
        duration_seconds,
    ))
}

fn parse_video_scene_timestamps(output: &str, duration_seconds: f64) -> Vec<f64> {
    let mut timestamps = output
        .lines()
        .filter_map(|line| parse_metric_after(line, "pts_time:"))
        .filter(|timestamp| {
            timestamp.is_finite()
                && *timestamp > 0.0
                && *timestamp < duration_seconds
        })
        .collect::<Vec<_>>();
    timestamps.sort_by(f64::total_cmp);
    timestamps.dedup_by(|left, right| (*left - *right).abs() < 0.05);
    timestamps
}

fn merge_video_keyframe_targets(
    duration_seconds: f64,
    uniform_targets: &[f64],
    scene_timestamps: &[f64],
) -> Vec<f64> {
    if uniform_targets.is_empty() || scene_timestamps.is_empty() {
        return uniform_targets.to_vec();
    }

    let target_count = uniform_targets.len();
    let mut scenes = scene_timestamps
        .iter()
        .copied()
        .filter(|timestamp| {
            timestamp.is_finite()
                && *timestamp > 0.0
                && *timestamp < duration_seconds
        })
        .collect::<Vec<_>>();
    scenes.sort_by(f64::total_cmp);
    scenes.dedup_by(|left, right| (*left - *right).abs() < 0.05);
    if scenes.is_empty() {
        return uniform_targets.to_vec();
    }

    let scene_budget = ((target_count as f64 * VIDEO_SCENE_TARGET_RATIO).ceil() as usize)
        .clamp(1, target_count)
        .min(scenes.len());
    let mut selected = evenly_spaced_values(&scenes, scene_budget);
    let mut remaining_uniform = uniform_targets.to_vec();

    while selected.len() < target_count && !remaining_uniform.is_empty() {
        let best_index = remaining_uniform
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                minimum_timestamp_distance(**left, &selected)
                    .total_cmp(&minimum_timestamp_distance(**right, &selected))
                    .then_with(|| right.total_cmp(left))
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        let candidate = remaining_uniform.swap_remove(best_index);
        if selected
            .iter()
            .all(|existing| (*existing - candidate).abs() >= 0.05)
        {
            selected.push(candidate);
        }
    }

    if selected.len() < target_count {
        for candidate in uniform_targets {
            if selected
                .iter()
                .all(|existing| (*existing - *candidate).abs() >= 0.05)
            {
                selected.push(*candidate);
                if selected.len() == target_count {
                    break;
                }
            }
        }
    }

    selected.sort_by(f64::total_cmp);
    selected.truncate(target_count);
    selected
}

fn evenly_spaced_values(values: &[f64], count: usize) -> Vec<f64> {
    if count == 0 || values.is_empty() {
        return Vec::new();
    }
    if values.len() <= count {
        return values.to_vec();
    }
    (0..count)
        .map(|index| {
            let value_index =
                (((index as f64 + 0.5) * values.len() as f64 / count as f64).floor() as usize)
                    .min(values.len() - 1);
            values[value_index]
        })
        .collect()
}

fn minimum_timestamp_distance(timestamp: f64, selected: &[f64]) -> f64 {
    selected
        .iter()
        .map(|existing| (*existing - timestamp).abs())
        .min_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn extract_video_frame(
    ffmpeg_bin: &str,
    video_path: &Path,
    timestamp_seconds: f64,
    output_path: &Path,
) -> Result<(), String> {
    let mut command = Command::new(ffmpeg_bin);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{timestamp_seconds:.3}"))
        .arg("-i")
        .arg(video_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-an")
        .arg("-sn")
        .arg("-frames:v")
        .arg("1")
        .arg("-q:v")
        .arg("3")
        .arg(output_path);
    let capture = run_command_with_timeout(&mut command, VIDEO_FRAME_EXTRACT_TIMEOUT)?;
    if !capture.success {
        return Err(format!(
            "FFmpeg exited unsuccessfully: {}",
            compact_command_error(&capture.stderr)
        ));
    }
    if !output_path
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    {
        return Err("FFmpeg did not produce a non-empty image".to_string());
    }
    Ok(())
}

fn probe_video_frame_quality(
    ffmpeg_bin: &str,
    frame_path: &Path,
) -> Result<VideoFrameQuality, String> {
    let mut command = Command::new(ffmpeg_bin);
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("info")
        .arg("-i")
        .arg(frame_path)
        .arg("-vf")
        .arg("signalstats,metadata=print,blurdetect=block_width=32:block_height=32:block_pct=80")
        .arg("-frames:v")
        .arg("1")
        .arg("-f")
        .arg("null")
        .arg("-");
    let capture = run_command_with_timeout(&mut command, VIDEO_FRAME_QUALITY_TIMEOUT)?;
    if !capture.success {
        return Err(format!(
            "FFmpeg quality probe failed: {}",
            compact_command_error(&capture.stderr)
        ));
    }
    let quality = parse_video_frame_quality(&capture.stderr);
    if quality.luminance.is_none() && quality.blur_score.is_none() {
        return Err("FFmpeg returned no luminance or blur metrics".to_string());
    }
    Ok(quality)
}

fn parse_video_frame_quality(output: &str) -> VideoFrameQuality {
    VideoFrameQuality {
        luminance: parse_metric_after(output, "lavfi.signalstats.YAVG="),
        blur_score: parse_metric_after(output, "blur mean:"),
    }
}

fn parse_metric_after(output: &str, marker: &str) -> Option<f64> {
    output.lines().find_map(|line| {
        let (_, value) = line.split_once(marker)?;
        value
            .split_whitespace()
            .next()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|metric| metric.is_finite())
    })
}

fn video_frame_rejection_reason(quality: VideoFrameQuality) -> Option<String> {
    if let Some(luminance) = quality.luminance {
        if luminance < VIDEO_FRAME_MIN_LUMA {
            return Some(format!(
                "frame is too dark (YAVG {luminance:.3} < {VIDEO_FRAME_MIN_LUMA:.1})"
            ));
        }
        if luminance > VIDEO_FRAME_MAX_LUMA {
            return Some(format!(
                "frame is too bright (YAVG {luminance:.3} > {VIDEO_FRAME_MAX_LUMA:.1})"
            ));
        }
    }
    if let Some(blur_score) = quality.blur_score {
        if blur_score > VIDEO_FRAME_MAX_BLUR_SCORE {
            return Some(format!(
                "frame is too blurry (score {blur_score:.3} > {VIDEO_FRAME_MAX_BLUR_SCORE:.1})"
            ));
        }
    }
    None
}

fn probe_video_duration_seconds(video_path: &Path) -> Result<f64, String> {
    let Some(ffprobe_bin) = resolve_ffprobe_bin() else {
        return Err("FFprobe is unavailable; video duration cannot be read".to_string());
    };
    let mut command = Command::new(&ffprobe_bin);
    command
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(video_path);
    let capture = run_command_with_timeout(&mut command, VIDEO_PROBE_TIMEOUT)?;
    if !capture.success {
        return Err(format!(
            "FFprobe failed for {}: {}",
            video_path.display(),
            compact_command_error(&capture.stderr)
        ));
    }
    capture
        .stdout
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| {
            format!(
                "FFprobe returned an invalid duration for {}: {}",
                video_path.display(),
                compact_command_error(&capture.stdout)
            )
        })
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<CommandCapture, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start media tool: {error}"))?;
    let mut stdout_reader = spawn_child_pipe_reader(child.stdout.take());
    let mut stderr_reader = spawn_child_pipe_reader(child.stderr.take());
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = finish_child_pipe_reader(stdout_reader.take());
                let stderr = finish_child_pipe_reader(stderr_reader.take());
                return Ok(CommandCapture {
                    success: status.success(),
                    stdout,
                    stderr,
                });
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(VIDEO_TOOL_POLL_INTERVAL);
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let stdout = finish_child_pipe_reader(stdout_reader.take());
                let stderr = finish_child_pipe_reader(stderr_reader.take());
                return Err(format!(
                    "media tool timed out after {:.1}s: {}{}",
                    timeout.as_secs_f64(),
                    compact_command_error(&stdout),
                    compact_command_error(&stderr)
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed while waiting for media tool: {error}"));
            }
        }
    }
}

fn spawn_child_pipe_reader<T>(pipe: Option<T>) -> Option<thread::JoinHandle<String>>
where
    T: Read + Send + 'static,
{
    pipe.map(|mut pipe| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        })
    })
}

fn finish_child_pipe_reader(reader: Option<thread::JoinHandle<String>>) -> String {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

fn compact_command_error(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "no diagnostic output".to_string()
    } else {
        compact.chars().take(500).collect()
    }
}

fn format_keyframe_percent(percent: f64) -> String {
    if (percent - percent.round()).abs() < 0.05 {
        format!("{:.0}", percent)
    } else {
        format!("{percent:.1}")
    }
}

fn video_keyframe_cache_dir(index_root: &Path, video_path: &Path) -> PathBuf {
    let canonical = video_path
        .canonicalize()
        .unwrap_or_else(|_| video_path.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let key = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    index_root.join("video-keyframes").join(key)
}

fn image_sidecar_state(
    image_path: &Path,
) -> Result<(Option<String>, Option<KnowledgeFileSignature>, String), String> {
    media_sidecar_state(image_path)
}

fn media_sidecar_state(
    media_path: &Path,
) -> Result<(Option<String>, Option<KnowledgeFileSignature>, String), String> {
    let Some(stem) = media_path.file_stem().and_then(|item| item.to_str()) else {
        return Ok((None, None, String::new()));
    };
    let Some(parent) = media_path.parent() else {
        return Ok((None, None, String::new()));
    };

    for extension in SIDECAR_EXTENSIONS {
        let candidate = parent.join(format!("{stem}.{extension}"));
        if !candidate.exists() {
            continue;
        }
        let Some(text) = load_text_file(&candidate) else {
            return Ok((
                Some(candidate.to_string_lossy().into_owned()),
                Some(file_signature(&candidate)?),
                String::new(),
            ));
        };
        return Ok((
            Some(candidate.to_string_lossy().into_owned()),
            Some(file_signature(&candidate)?),
            text,
        ));
    }

    Ok((None, None, String::new()))
}

fn load_text_file(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_INDEX_TEXT_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn load_document_text(path: &Path) -> Option<String> {
    if should_normalize_with_markitdown(path) {
        if let Some(normalized) = normalize_document_with_markitdown(path) {
            return Some(normalized);
        }
    }
    load_text_file(path)
}

fn should_normalize_with_markitdown(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
        .is_some_and(|extension| MARKITDOWN_EXTENSIONS.contains(&extension))
}

fn normalize_document_with_markitdown(path: &Path) -> Option<String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()));
    let path_text = path.to_string_lossy().into_owned();
    let converter = MarkItDown::new();
    let result = converter
        .convert(
            &path_text,
            Some(ConversionOptions {
                file_extension: extension,
                url: None,
                llm_client: None,
                llm_model: None,
            }),
        )
        .ok()
        .flatten()?;
    let text = result.text_content.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn document_source_kind(path: &Path) -> &'static str {
    if should_normalize_with_markitdown(path) {
        "normalized_markdown"
    } else {
        "document"
    }
}

fn markitdown_provider_key(path: &Path) -> Option<String> {
    should_normalize_with_markitdown(path).then(|| "markitdown".to_string())
}

fn join_text_sources(text_sources: &[KnowledgeIndexTextSource]) -> String {
    text_sources
        .iter()
        .map(|source| source.text.as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Default)]
struct KnowledgeChunkHierarchy {
    parents: Vec<KnowledgeIndexParentChunk>,
    children: Vec<KnowledgeIndexChunk>,
}

#[derive(Debug, Clone)]
struct ChunkSourceLine {
    number: usize,
    text: String,
    section_path: Vec<String>,
}

#[derive(Debug)]
struct ParentChunkDraft {
    lines: Vec<ChunkSourceLine>,
    section_path: Vec<String>,
}

fn build_text_chunk_hierarchy(
    text_sources: &[KnowledgeIndexTextSource],
) -> KnowledgeChunkHierarchy {
    let mut hierarchy = KnowledgeChunkHierarchy::default();
    for source in text_sources {
        let parent_drafts = build_parent_drafts_for_source(source);
        let source_child_start = hierarchy.children.len();
        for draft in parent_drafts {
            let parent_id = format!("parent-{:04}", hierarchy.parents.len() + 1);
            let parent_text = join_chunk_source_lines(&draft.lines);
            if parent_text.is_empty() {
                continue;
            }
            let parent_index = hierarchy.parents.len();
            hierarchy.parents.push(KnowledgeIndexParentChunk {
                parent_id: parent_id.clone(),
                section_path: draft.section_path.clone(),
                line_start: draft.lines.first().map(|line| line.number).unwrap_or(1),
                line_end: draft.lines.last().map(|line| line.number).unwrap_or(1),
                text: parent_text,
                child_ids: Vec::new(),
                source_kind: source.source_kind.clone(),
                source_path: source.source_path.clone(),
            });
            let child_drafts = build_child_drafts(&draft.lines);
            for child_lines in child_drafts {
                let text = join_chunk_source_lines(&child_lines);
                if text.is_empty() {
                    continue;
                }
                let indexed_text = build_child_indexed_text(&draft.section_path, &text);
                let chunk_id = format!("chunk-{:04}", hierarchy.children.len() + 1);
                hierarchy.parents[parent_index]
                    .child_ids
                    .push(chunk_id.clone());
                hierarchy.children.push(KnowledgeIndexChunk {
                    chunk_id,
                    parent_id: Some(parent_id.clone()),
                    previous_id: None,
                    next_id: None,
                    section_path: draft.section_path.clone(),
                    line_start: child_lines.first().map(|line| line.number).unwrap_or(1),
                    line_end: child_lines.last().map(|line| line.number).unwrap_or(1),
                    text,
                    indexed_text,
                    source_kind: source.source_kind.clone(),
                    source_path: source.source_path.clone(),
                });
            }
        }
        link_adjacent_children(&mut hierarchy.children[source_child_start..]);
    }
    hierarchy
}

fn build_parent_drafts_for_source(source: &KnowledgeIndexTextSource) -> Vec<ParentChunkDraft> {
    let mut drafts = Vec::new();
    let mut current_lines = Vec::new();
    let mut current_tokens = 0usize;
    let mut heading_stack = Vec::<String>::new();

    for (index, raw_line) in source.text.lines().enumerate() {
        let text = raw_line.trim_end().to_string();
        let heading = markdown_heading(&text);
        if heading.is_some() && !current_lines.is_empty() {
            push_parent_draft(&mut drafts, &mut current_lines);
            current_tokens = 0;
        }
        if let Some((level, title)) = heading {
            heading_stack.truncate(level.saturating_sub(1));
            heading_stack.push(title);
        }
        let source_line = ChunkSourceLine {
            number: index + 1,
            text,
            section_path: heading_stack.clone(),
        };
        for segment in split_source_line(&source_line, PARENT_CHUNK_MAX_TOKENS) {
            let line_tokens = estimated_token_count(&segment.text).max(1);
            let projected_tokens = current_tokens.saturating_add(line_tokens);
            let at_paragraph_boundary = segment.text.trim().is_empty();
            if !current_lines.is_empty()
                && (projected_tokens > PARENT_CHUNK_MAX_TOKENS
                    || (current_tokens >= PARENT_CHUNK_TARGET_TOKENS
                        && at_paragraph_boundary))
            {
                push_parent_draft(&mut drafts, &mut current_lines);
                current_tokens = 0;
            }
            current_lines.push(segment);
            current_tokens = current_tokens.saturating_add(line_tokens);
        }
    }
    push_parent_draft(&mut drafts, &mut current_lines);
    drafts
}

fn push_parent_draft(
    drafts: &mut Vec<ParentChunkDraft>,
    current_lines: &mut Vec<ChunkSourceLine>,
) {
    if current_lines.iter().all(|line| line.text.trim().is_empty()) {
        current_lines.clear();
        return;
    }
    let section_path = current_lines
        .iter()
        .find(|line| !line.section_path.is_empty())
        .map(|line| line.section_path.clone())
        .unwrap_or_default();
    drafts.push(ParentChunkDraft {
        lines: std::mem::take(current_lines),
        section_path,
    });
}

fn build_child_drafts(lines: &[ChunkSourceLine]) -> Vec<Vec<ChunkSourceLine>> {
    let mut drafts = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0usize;

    for line in lines {
        for segment in split_source_line(line, CHILD_CHUNK_TARGET_TOKENS) {
            let line_tokens = estimated_token_count(&segment.text).max(1);
            if !current.is_empty()
                && current_tokens.saturating_add(line_tokens) > CHILD_CHUNK_TARGET_TOKENS
            {
                drafts.push(current.clone());
                current = trailing_overlap_lines(&current, CHILD_CHUNK_OVERLAP_TOKENS);
                current_tokens = estimated_lines_token_count(&current);
            }
            current.push(segment);
            current_tokens = current_tokens.saturating_add(line_tokens);
        }
    }
    if !current.is_empty() && current.iter().any(|line| !line.text.trim().is_empty()) {
        drafts.push(current);
    }
    drafts
}

fn trailing_overlap_lines(
    lines: &[ChunkSourceLine],
    token_budget: usize,
) -> Vec<ChunkSourceLine> {
    let mut overlap = Vec::new();
    let mut tokens = 0usize;
    for line in lines.iter().rev() {
        let line_tokens = estimated_token_count(&line.text).max(1);
        if overlap.is_empty() && line_tokens > token_budget {
            overlap.push(source_line_tail(line, token_budget));
            break;
        }
        if !overlap.is_empty() && tokens.saturating_add(line_tokens) > token_budget {
            break;
        }
        overlap.push(line.clone());
        tokens = tokens.saturating_add(line_tokens);
        if tokens >= token_budget {
            break;
        }
    }
    overlap.reverse();
    overlap
}

fn split_source_line(line: &ChunkSourceLine, token_budget: usize) -> Vec<ChunkSourceLine> {
    if estimated_token_count(&line.text) <= token_budget || line.text.is_empty() {
        return vec![line.clone()];
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;
    let mut ascii_run = 0usize;
    for character in line.text.chars() {
        let token_increment = if character.is_ascii_alphanumeric() || character == '_' {
            let previous_tokens = ascii_run.div_ceil(4);
            ascii_run += 1;
            ascii_run.div_ceil(4).saturating_sub(previous_tokens)
        } else {
            ascii_run = 0;
            usize::from(!character.is_whitespace())
        };
        if !current.is_empty()
            && current_tokens.saturating_add(token_increment) > token_budget
        {
            segments.push(ChunkSourceLine {
                number: line.number,
                text: std::mem::take(&mut current),
                section_path: line.section_path.clone(),
            });
            current_tokens = 0;
            ascii_run = usize::from(character.is_ascii_alphanumeric() || character == '_');
        }
        current.push(character);
        current_tokens = current_tokens.saturating_add(token_increment);
    }
    if !current.is_empty() {
        segments.push(ChunkSourceLine {
            number: line.number,
            text: current,
            section_path: line.section_path.clone(),
        });
    }
    segments
}

fn source_line_tail(line: &ChunkSourceLine, token_budget: usize) -> ChunkSourceLine {
    split_source_line(line, token_budget)
        .pop()
        .unwrap_or_else(|| line.clone())
}

fn link_adjacent_children(children: &mut [KnowledgeIndexChunk]) {
    for index in 0..children.len() {
        children[index].previous_id = index
            .checked_sub(1)
            .and_then(|previous| children.get(previous))
            .map(|chunk| chunk.chunk_id.clone());
        children[index].next_id = children
            .get(index + 1)
            .map(|chunk| chunk.chunk_id.clone());
    }
}

fn join_chunk_source_lines(lines: &[ChunkSourceLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn build_child_indexed_text(section_path: &[String], text: &str) -> String {
    if section_path.is_empty() {
        return text.to_string();
    }
    format!("章节：{}\n{text}", section_path.join(" > "))
}

fn estimated_lines_token_count(lines: &[ChunkSourceLine]) -> usize {
    lines
        .iter()
        .map(|line| estimated_token_count(&line.text).max(1))
        .sum()
}

fn estimated_token_count(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut ascii_run = 0usize;
    let flush_ascii_run = |tokens: &mut usize, ascii_run: &mut usize| {
        if *ascii_run > 0 {
            *tokens = tokens.saturating_add((*ascii_run).div_ceil(4));
            *ascii_run = 0;
        }
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            ascii_run += 1;
            continue;
        }
        flush_ascii_run(&mut tokens, &mut ascii_run);
        if !character.is_whitespace() {
            tokens = tokens.saturating_add(1);
        }
    }
    flush_ascii_run(&mut tokens, &mut ascii_run);
    tokens
}

fn markdown_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|character| *character == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let title = trimmed.get(level..)?.trim();
    (!title.is_empty()).then(|| (level, title.to_string()))
}

fn file_signature(path: &Path) -> Result<KnowledgeFileSignature, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to read metadata for {}: {error}", path.display()))?;
    Ok(KnowledgeFileSignature {
        modified_unix_millis: metadata
            .modified()
            .ok()
            .and_then(system_time_to_millis)
            .unwrap_or_default(),
        size_bytes: metadata.len(),
    })
}

fn directory_signature(path: &Path) -> Result<KnowledgeFileSignature, String> {
    file_signature(path)
}

fn classify_path(path: &Path) -> Option<KnowledgeModality> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if DOCUMENT_EXTENSIONS.contains(&extension.as_str()) && !is_media_sidecar(path) {
        return Some(KnowledgeModality::Document);
    }
    if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return Some(KnowledgeModality::Image);
    }
    if AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return Some(KnowledgeModality::Audio);
    }
    if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Some(KnowledgeModality::Video);
    }
    None
}

fn is_media_sidecar(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|item| item.to_str()) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    IMAGE_EXTENSIONS
        .iter()
        .chain(AUDIO_EXTENSIONS.iter())
        .chain(VIDEO_EXTENSIONS.iter())
        .any(|extension| parent.join(format!("{stem}.{extension}")).exists())
}

fn should_skip_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|item| item.to_str()) else {
        return false;
    };
    matches!(name, ".git" | ".svn" | "node_modules" | "target")
}

fn default_index_root() -> PathBuf {
    PathBuf::from(DEFAULT_INDEX_DIR)
}

pub fn root_storage_key(root: &Path) -> String {
    let canonical = root
        .canonicalize()
        .ok()
        .unwrap_or_else(|| root.to_path_buf());
    let normalized = canonical.to_string_lossy().to_string();
    let digest = Sha256::digest(normalized.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn current_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn system_time_to_millis(value: SystemTime) -> Option<u128> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::{
        build_text_chunk_hierarchy, detect_video_scene_timestamps, extract_video_keyframes,
        embedding_store_matches_identity, load_embedding_store,
        format_keyframe_percent, merge_video_keyframe_targets, parse_video_frame_quality,
        parse_video_scene_timestamps, video_frame_rejection_reason, video_keyframe_count,
        video_keyframe_targets, KnowledgeIndexConfig, KnowledgeIndexService,
        KnowledgeEmbeddingStore, KnowledgeIndexTextSource, KnowledgeModality, VideoFrameQuality,
        EMBEDDING_STORE_SCHEMA_VERSION,
    };
    use crate::runtime::model_center::EmbeddingEndpointIdentity;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    #[test]
    fn embedding_store_identity_rejects_configured_alias_for_actual_runtime_model() {
        let store = KnowledgeEmbeddingStore {
            provider_key: Some("qwen".to_string()),
            model_endpoint_id: Some("embed-local-openai-compatible".to_string()),
            model_name: Some("Qwen/Qwen3-Embedding-0.6B".to_string()),
            ..KnowledgeEmbeddingStore::default()
        };
        let identity = EmbeddingEndpointIdentity {
            provider_key: "qwen".to_string(),
            model_endpoint_id: "embed-local-openai-compatible".to_string(),
            model_name: "/models/jina-embeddings-v2-base-zh".to_string(),
        };

        assert!(!embedding_store_matches_identity(&store, &identity));
    }

    #[test]
    fn embedding_store_v2_is_invalidated_before_runtime_identity_rebuild() {
        let root = unique_dir("harborbeacon-embedding-store-v2");
        fs::create_dir_all(&root).expect("create embedding store test root");
        let path = root.join("store.embeddings.json");
        fs::write(
            &path,
            r#"{
                "schema_version": 2,
                "root": "/knowledge",
                "provider_key": "qwen",
                "model_endpoint_id": "embed-local-openai-compatible",
                "model_name": "Qwen/Qwen3-Embedding-0.6B",
                "vector_dimensions": 768,
                "entries": [{
                    "key": "/knowledge/doc.txt::chunk-0001",
                    "path": "/knowledge/doc.txt",
                    "chunk_id": "chunk-0001",
                    "text_hash": "legacy",
                    "vector": [0.1, 0.2]
                }]
            }"#,
        )
        .expect("write legacy embedding store");

        let store = load_embedding_store(&path).expect("load embedding store");

        assert_eq!(store.schema_version, EMBEDDING_STORE_SCHEMA_VERSION);
        assert!(store.entries.is_empty());
        cleanup_dir(&root);
    }

    #[test]
    fn parent_child_chunking_preserves_sections_overlap_and_adjacency() {
        let first_paragraph = "春".repeat(180);
        let second_paragraph = "花".repeat(180);
        let source = KnowledgeIndexTextSource {
            source_kind: "normalized_markdown".to_string(),
            source_path: Some("/knowledge/spring.md".to_string()),
            provider_key: Some("markitdown".to_string()),
            text: format!(
                "# 春季\n{first_paragraph}\n{second_paragraph}\n## 氛围\n公园里的人们正在散步。"
            ),
        };

        let hierarchy = build_text_chunk_hierarchy(&[source]);

        assert_eq!(hierarchy.parents.len(), 2);
        assert!(hierarchy.children.len() >= 3);
        assert_eq!(hierarchy.parents[0].section_path, vec!["春季"]);
        assert_eq!(
            hierarchy.parents[1].section_path,
            vec!["春季", "氛围"]
        );
        assert!(hierarchy.parents[0].child_ids.len() >= 2);
        assert!(hierarchy.children.iter().all(|child| child.parent_id.is_some()));
        assert_eq!(hierarchy.children[0].previous_id, None);
        assert_eq!(
            hierarchy.children[0].next_id,
            Some(hierarchy.children[1].chunk_id.clone())
        );
        assert_eq!(
            hierarchy.children[1].previous_id,
            Some(hierarchy.children[0].chunk_id.clone())
        );
        assert!(hierarchy.children[1].text.contains('春'));
        assert!(hierarchy.children[1].text.contains('花'));
        assert!(hierarchy.children[1].indexed_text.contains("章节：春季"));
    }

    #[test]
    fn video_keyframe_budget_scales_with_duration_without_boundary_regression() {
        assert_eq!(video_keyframe_count(30.0), 5);
        assert_eq!(video_keyframe_count(60.0), 5);
        assert_eq!(video_keyframe_count(300.0), 10);
        assert_eq!(video_keyframe_count(600.0), 20);
        assert_eq!(video_keyframe_count(601.0), 20);
        assert_eq!(video_keyframe_count(3_600.0), 30);
        assert_eq!(video_keyframe_count(3_601.0), 30);
        assert_eq!(video_keyframe_count(14_400.0), 48);
        assert_eq!(video_keyframe_count(86_400.0), 48);
    }

    #[test]
    fn video_keyframe_targets_are_centered_and_keep_true_percentages() {
        let targets = video_keyframe_targets(100.0);
        assert_eq!(targets, vec![10.0, 30.0, 50.0, 70.0, 90.0]);
        assert_eq!(format_keyframe_percent(30.0), "30");
        assert_eq!(format_keyframe_percent(31.25), "31.2");
        assert!(video_keyframe_targets(0.0).is_empty());
        assert!(video_keyframe_targets(f64::NAN).is_empty());
    }

    #[test]
    fn video_frame_quality_parser_and_filters_reject_unusable_frames() {
        let parsed = parse_video_frame_quality(
            "lavfi.signalstats.YAVG=94.6634\n[blurdetect] blur mean: 5.5236030\n",
        );
        assert_eq!(
            parsed,
            VideoFrameQuality {
                luminance: Some(94.6634),
                blur_score: Some(5.523603),
            }
        );
        assert!(video_frame_rejection_reason(parsed).is_none());
        assert!(video_frame_rejection_reason(VideoFrameQuality {
            luminance: Some(4.0),
            blur_score: Some(1.0),
        })
        .expect("dark rejection")
        .contains("too dark"));
        assert!(video_frame_rejection_reason(VideoFrameQuality {
            luminance: Some(250.0),
            blur_score: Some(1.0),
        })
        .expect("bright rejection")
        .contains("too bright"));
        assert!(video_frame_rejection_reason(VideoFrameQuality {
            luminance: Some(100.0),
            blur_score: Some(20.0),
        })
        .expect("blur rejection")
        .contains("too blurry"));
    }

    #[test]
    fn video_scene_parser_and_target_merge_preserve_scenes_and_coverage() {
        let parsed = parse_video_scene_timestamps(
            "[Parsed_metadata] frame:0 pts:20 pts_time:2.0\n\
             [Parsed_metadata] frame:1 pts:50 pts_time:5.0\n\
             [Parsed_metadata] frame:2 pts:50 pts_time:5.02\n\
             [Parsed_metadata] frame:3 pts:120 pts_time:12.0\n",
            10.0,
        );
        assert_eq!(parsed, vec![2.0, 5.0]);

        let uniform = video_keyframe_targets(100.0);
        let merged = merge_video_keyframe_targets(
            100.0,
            &uniform,
            &[5.0, 25.0, 55.0, 75.0, 95.0],
        );
        assert_eq!(merged.len(), uniform.len());
        assert!(merged.contains(&5.0));
        assert!(merged.contains(&55.0));
        assert!(merged.contains(&95.0));
        assert!(merged.windows(2).all(|items| items[0] < items[1]));
    }

    #[test]
    fn video_scene_detection_uses_real_ffmpeg_when_available() {
        if which::which("ffmpeg").is_err() {
            return;
        }
        let workspace = unique_dir("harborbeacon-video-scenes-real");
        let video_path = workspace.join("scene-cuts.mp4");
        fs::create_dir_all(&workspace).expect("create workspace");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostdin",
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:size=320x240:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:size=320x240:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:size=320x240:rate=10:duration=2",
                "-filter_complex",
                "[0:v][1:v][2:v]concat=n=3:v=1:a=0",
                "-c:v",
                "mpeg4",
            ])
            .arg(&video_path)
            .status()
            .expect("run ffmpeg scene fixture");
        assert!(status.success());

        let timestamps =
            detect_video_scene_timestamps("ffmpeg", &video_path, 6.0)
                .expect("detect scene timestamps");
        assert!(
            timestamps.iter().any(|timestamp| (*timestamp - 2.0).abs() <= 0.6),
            "missing first scene cut: {timestamps:?}"
        );
        assert!(
            timestamps.iter().any(|timestamp| (*timestamp - 4.0).abs() <= 0.6),
            "missing second scene cut: {timestamps:?}"
        );

        cleanup_dir(&workspace);
    }

    #[test]
    fn video_keyframe_extraction_uses_real_ffmpeg_when_available() {
        if which::which("ffmpeg").is_err() || which::which("ffprobe").is_err() {
            return;
        }
        let workspace = unique_dir("harborbeacon-video-keyframe-real");
        let index_root = workspace.join("index");
        let video_path = workspace.join("sample.mp4");
        fs::create_dir_all(&index_root).expect("create index root");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostdin",
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=10",
                "-t",
                "12",
                "-c:v",
                "mpeg4",
            ])
            .arg(&video_path)
            .status()
            .expect("run ffmpeg fixture");
        assert!(status.success());

        let extraction = extract_video_keyframes(&video_path, &index_root);
        assert_eq!(
            extraction.frames.len(),
            5,
            "unexpected extraction warnings: {:?}",
            extraction.warnings
        );
        assert!(
            extraction
                .frames
                .iter()
                .all(|frame| frame.path.is_file()
                    && frame
                        .path
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() > 0))
        );
        assert!(
            extraction
                .frames
                .windows(2)
                .all(|frames| frames[0].timestamp_seconds < frames[1].timestamp_seconds)
        );

        cleanup_dir(&workspace);
    }

    #[test]
    fn incremental_refresh_updates_changed_files_and_reuses_unchanged_entries() {
        let knowledge_root = unique_dir("harborbeacon-knowledge-index-root");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("docs").join("sakura-notes.md"),
            "今年花园里的樱花开得很盛，适合做春季归档。",
        )
        .expect("write doc");
        fs::write(
            knowledge_root.join("docs").join("stable-note.md"),
            "这是一条保持不变的知识索引笔记。",
        )
        .expect("write stable doc");

        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let first = service
            .load_or_refresh(&knowledge_root)
            .expect("first index build");
        assert_eq!(first.stats.added, 2);
        assert!(first.stats.persisted);
        assert_eq!(first.manifest.entries.len(), 2);
        assert!(
            first
                .manifest
                .entries
                .iter()
                .any(|entry| entry.modality == KnowledgeModality::Document
                    && !entry.chunks.is_empty())
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            knowledge_root.join("docs").join("sakura-notes.md"),
            "今年花园里的樱花开得更盛，适合做春季归档和分享。",
        )
        .expect("update doc");
        fs::write(
            knowledge_root.join("docs").join("spring-guide.md"),
            "春季知识索引补充笔记。",
        )
        .expect("add doc");

        let second = service
            .load_or_refresh(&knowledge_root)
            .expect("second index refresh");
        assert!(second.stats.updated >= 1);
        assert!(second.stats.added >= 1);
        assert!(second.stats.reused >= 1);
        assert!(second
            .manifest
            .entries
            .iter()
            .any(|entry| entry.path.ends_with("spring-guide.md")));

        cleanup_dir(&knowledge_root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn sidecar_metadata_is_persisted_for_image_entries() {
        let knowledge_root = unique_dir("harborbeacon-knowledge-index-sidecar");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(knowledge_root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("images").join("gate.jpg"),
            b"fake-image",
        )
        .expect("write image");
        fs::write(
            knowledge_root.join("images").join("gate.yaml"),
            "caption: front gate\nlabels:\n  - entry\n  - camera\n",
        )
        .expect("write sidecar");

        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service
            .load_or_refresh(&knowledge_root)
            .expect("index refresh");
        let image = snapshot
            .manifest
            .entries
            .iter()
            .find(|entry| entry.modality == KnowledgeModality::Image)
            .expect("image entry");
        let expected_sidecar = knowledge_root
            .join("images")
            .join("gate.yaml")
            .canonicalize()
            .unwrap_or_else(|_| knowledge_root.join("images").join("gate.yaml"));
        let expected_sidecar = expected_sidecar.to_string_lossy().into_owned();

        assert_eq!(
            image.sidecar_path.as_deref(),
            Some(expected_sidecar.as_str())
        );
        assert!(image.searchable_text.contains("front gate"));
        assert!(image.searchable_text.contains("entry"));

        cleanup_dir(&knowledge_root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn media_sidecars_index_audio_and_video_without_indexing_sidecars_as_documents() {
        let knowledge_root = unique_dir("harborbeacon-knowledge-index-media-sidecar");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(knowledge_root.join("media")).expect("create media");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("media").join("doorbell.mp3"),
            b"fake-audio",
        )
        .expect("write audio");
        fs::write(
            knowledge_root.join("media").join("doorbell.txt"),
            "front door audio transcript: courier arrived at 09:15",
        )
        .expect("write audio transcript");
        fs::write(knowledge_root.join("media").join("clip.mp4"), b"fake-video")
            .expect("write video");
        fs::write(
            knowledge_root.join("media").join("clip.json"),
            r#"{"summary":"garage video sidecar","timestamp":"00:00:12","frame":"car entered"}"#,
        )
        .expect("write video sidecar");
        fs::write(
            knowledge_root.join("media").join("opaque.wav"),
            b"no-sidecar",
        )
        .expect("write opaque audio");

        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service
            .load_or_refresh(&knowledge_root)
            .expect("index refresh");

        assert_eq!(snapshot.manifest.entries.len(), 2);
        assert!(snapshot.manifest.entries.iter().all(|entry| !entry
            .path
            .ends_with("doorbell.txt")
            && !entry.path.ends_with("clip.json")
            && !entry.path.ends_with("opaque.wav")));

        let audio = snapshot
            .manifest
            .entries
            .iter()
            .find(|entry| entry.modality == KnowledgeModality::Audio)
            .expect("audio entry");
        assert!(audio.searchable_text.contains("courier arrived"));
        assert_eq!(audio.text_sources[0].source_kind, "transcript");
        assert!(audio
            .sidecar_path
            .as_deref()
            .is_some_and(|path| path.ends_with("doorbell.txt")));
        assert!(!audio.chunks.is_empty());

        let video = snapshot
            .manifest
            .entries
            .iter()
            .find(|entry| entry.modality == KnowledgeModality::Video)
            .expect("video entry");
        assert!(video.searchable_text.contains("garage video sidecar"));
        assert_eq!(video.text_sources[0].source_kind, "video_sidecar");
        assert!(video
            .sidecar_path
            .as_deref()
            .is_some_and(|path| path.ends_with("clip.json")));
        assert!(!video.chunks.is_empty());

        cleanup_dir(&knowledge_root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn repeated_refreshes_keep_manifest_path_stable() {
        let knowledge_root = unique_dir("harborbeacon-knowledge-index-stable");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("docs").join("one.md"),
            "稳定排序测试内容。",
        )
        .expect("write doc");

        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let first = service.load_or_refresh(&knowledge_root).expect("first");
        let second = service.load_or_refresh(&knowledge_root).expect("second");
        assert_eq!(first.manifest_path, second.manifest_path);
        assert_eq!(first.manifest.entries, second.manifest.entries);
        assert!(second.stats.reused >= 1);

        cleanup_dir(&knowledge_root);
        cleanup_dir(&index_root);
    }

    #[test]
    fn html_documents_are_normalized_before_indexing() {
        let knowledge_root = unique_dir("harborbeacon-knowledge-index-html");
        let index_root = unique_dir("harborbeacon-knowledge-index-store");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("docs").join("garden.html"),
            "<html><body><h1>樱花整理</h1><p>春季归档清单。</p></body></html>",
        )
        .expect("write html");

        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).expect("config"),
        )
        .expect("service");
        let snapshot = service
            .load_or_refresh(&knowledge_root)
            .expect("index refresh");
        let document = snapshot
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path.ends_with("garden.html"))
            .expect("normalized html entry");

        assert_eq!(document.modality, KnowledgeModality::Document);
        assert!(document.searchable_text.contains("樱花整理"));
        assert!(document.searchable_text.contains("春季归档清单"));
        assert!(!document.searchable_text.contains("<html>"));
        assert_eq!(
            document.text_sources[0].provider_key.as_deref(),
            Some("markitdown")
        );
        assert_eq!(document.text_sources[0].source_kind, "normalized_markdown");

        cleanup_dir(&knowledge_root);
        cleanup_dir(&index_root);
    }
}
