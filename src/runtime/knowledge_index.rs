//! Local knowledge index and manifest storage for HarborBeacon search.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use markitdown::model::ConversionOptions;
use markitdown::MarkItDown;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::media_tools::{resolve_ffmpeg_bin, resolve_ffprobe_bin};
use crate::runtime::{admin_console::AdminModelCenterState, model_center};

pub const KNOWLEDGE_INDEX_ROOT_ENV: &str = "HARBOR_KNOWLEDGE_INDEX_ROOT";

const DEFAULT_INDEX_DIR: &str = ".harborbeacon/knowledge-index";
const MAX_INDEX_TEXT_BYTES: u64 = 512 * 1024;
const MAX_CHUNK_LINES: usize = 4;
const MAX_CHUNK_CHARS: usize = 320;
const INDEX_SCHEMA_VERSION: u32 = 1;
const EMBEDDING_STORE_SCHEMA_VERSION: u32 = 1;
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
const VIDEO_KEYFRAME_SAMPLE_POINTS: &[u32] = &[10, 30, 50, 70, 90];

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
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
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
    pub chunks: Vec<KnowledgeIndexChunk>,
    #[serde(default)]
    pub text_sources: Vec<KnowledgeIndexTextSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_path: Option<String>,
    pub file_signature: KnowledgeFileSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_signature: Option<KnowledgeFileSignature>,
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

        let mut entries_by_key = store
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.key.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut active_keys = HashSet::new();
        let mut dirty = false;

        for entry in &snapshot.manifest.entries {
            for chunk in embedding_chunks_for_entry(entry) {
                let text = chunk.text.trim();
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

                store.provider_key =
                    (!execution.provider_key.trim().is_empty()).then_some(execution.provider_key);
                store.model_endpoint_id = execution.model_endpoint_id;
                store.model_name = execution.model_name;

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
    #[serde(default)]
    pub entries: Vec<KnowledgeEmbeddingEntry>,
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
        line_start: 1,
        line_end: entry.searchable_text.lines().count().max(1),
        text: entry.searchable_text.clone(),
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
            let chunks = build_text_chunks(&text_sources);
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
                chunks,
                text_sources,
                sidecar_path: None,
                file_signature,
                sidecar_signature: None,
            }))
        }
        KnowledgeModality::Image => {
            let (sidecar_path, sidecar_signature, text_sources) = image_text_sources(path)?;
            let searchable_text = join_text_sources(&text_sources);
            let chunks = build_text_chunks(&text_sources);
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
                chunks,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
            }))
        }
        KnowledgeModality::Audio => {
            let (sidecar_path, sidecar_signature, text_sources) =
                media_text_sources(path, modality)?;
            if text_sources.is_empty() {
                return Ok(None);
            }
            let searchable_text = join_text_sources(&text_sources);
            let chunks = build_text_chunks(&text_sources);
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
                chunks,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
            }))
        }
        KnowledgeModality::Video => {
            let (sidecar_path, sidecar_signature, text_sources) =
                video_text_sources(path, index_root)?;
            if text_sources.is_empty() {
                return Ok(None);
            }
            let searchable_text = join_text_sources(&text_sources);
            let chunks = build_text_chunks(&text_sources);
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
                chunks,
                text_sources,
                sidecar_path,
                file_signature,
                sidecar_signature,
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
    ),
    String,
> {
    let (sidecar_path, sidecar_signature, sidecar_text) = media_sidecar_state(video_path)?;
    let mut text_sources = Vec::new();
    if !sidecar_text.is_empty() {
        text_sources.push(KnowledgeIndexTextSource {
            source_kind: "video_sidecar".to_string(),
            source_path: sidecar_path.clone(),
            provider_key: None,
            text: sidecar_text,
        });
    }

    for (index, frame_path) in extract_video_keyframes(video_path, index_root)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let vlm = model_center::run_vlm_summary(&frame_path);
        if vlm.available && !vlm.text.trim().is_empty() {
            let percent = VIDEO_KEYFRAME_SAMPLE_POINTS
                .get(index)
                .copied()
                .unwrap_or_default();
            text_sources.push(KnowledgeIndexTextSource {
                source_kind: "vlm_keyframe".to_string(),
                source_path: Some(frame_path.to_string_lossy().into_owned()),
                provider_key: Some(vlm.provider_key),
                text: format!("keyframe {percent}%: {}", vlm.text.trim()),
            });
        }
    }

    Ok((sidecar_path, sidecar_signature, text_sources))
}

fn extract_video_keyframes(video_path: &Path, index_root: &Path) -> Result<Vec<PathBuf>, String> {
    let Some(ffmpeg_bin) = resolve_ffmpeg_bin() else {
        return Ok(Vec::new());
    };
    let Some(duration_seconds) = probe_video_duration_seconds(video_path) else {
        return Ok(Vec::new());
    };
    if duration_seconds <= 0.0 {
        return Ok(Vec::new());
    }

    let output_dir = video_keyframe_cache_dir(index_root, video_path);
    fs::create_dir_all(&output_dir).map_err(|error| {
        format!(
            "failed to create video keyframe cache {}: {error}",
            output_dir.display()
        )
    })?;

    let mut frames = Vec::new();
    for (index, percent) in VIDEO_KEYFRAME_SAMPLE_POINTS.iter().enumerate() {
        let timestamp = (duration_seconds * (*percent as f64 / 100.0)).max(0.0);
        let output_path = output_dir.join(format!("frame-{:02}.jpg", index + 1));
        let status = Command::new(&ffmpeg_bin)
            .arg("-y")
            .arg("-loglevel")
            .arg("error")
            .arg("-ss")
            .arg(format!("{timestamp:.3}"))
            .arg("-i")
            .arg(video_path)
            .arg("-frames:v")
            .arg("1")
            .arg("-q:v")
            .arg("3")
            .arg(&output_path)
            .status();
        match status {
            Ok(status)
                if status.success()
                    && output_path
                        .metadata()
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0) =>
            {
                frames.push(output_path);
            }
            _ => {}
        }
    }
    Ok(frames)
}

fn probe_video_duration_seconds(video_path: &Path) -> Option<f64> {
    let ffprobe_bin = resolve_ffprobe_bin()?;
    let output = Command::new(&ffprobe_bin)
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(video_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.parse::<f64>().ok().filter(|value| value.is_finite())
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

fn build_text_chunks(text_sources: &[KnowledgeIndexTextSource]) -> Vec<KnowledgeIndexChunk> {
    let mut chunks = Vec::new();
    for source in text_sources {
        chunks.extend(build_chunks_for_source(source));
    }
    chunks
}

fn build_chunks_for_source(source: &KnowledgeIndexTextSource) -> Vec<KnowledgeIndexChunk> {
    let text = source.text.as_str();
    let mut chunks = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_start_line = 1usize;
    let mut current_end_line = 0usize;
    let mut current_chars = 0usize;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim_end();
        let line_chars = line.chars().count();
        let projected_chars =
            current_chars + line_chars + if current_lines.is_empty() { 0 } else { 1 };
        if !current_lines.is_empty()
            && (current_lines.len() >= MAX_CHUNK_LINES || projected_chars > MAX_CHUNK_CHARS)
        {
            push_chunk(
                &mut chunks,
                source,
                &current_lines,
                current_start_line,
                current_end_line,
            );
            current_lines.clear();
            current_chars = 0;
            current_start_line = line_number;
        }

        current_end_line = line_number;
        current_chars = current_chars + line_chars + if current_lines.is_empty() { 0 } else { 1 };
        current_lines.push(line.to_string());
    }

    if !current_lines.is_empty() {
        push_chunk(
            &mut chunks,
            source,
            &current_lines,
            current_start_line,
            current_end_line,
        );
    } else if !text.trim().is_empty() {
        chunks.push(KnowledgeIndexChunk {
            chunk_id: "chunk-0001".to_string(),
            line_start: 1,
            line_end: 1,
            text: text.trim().to_string(),
            source_kind: source.source_kind.clone(),
            source_path: source.source_path.clone(),
        });
    }

    if chunks.is_empty() {
        chunks.push(KnowledgeIndexChunk {
            chunk_id: "chunk-0001".to_string(),
            line_start: 1,
            line_end: 1,
            text: text.trim().to_string(),
            source_kind: source.source_kind.clone(),
            source_path: source.source_path.clone(),
        });
    }

    chunks
}

fn push_chunk(
    chunks: &mut Vec<KnowledgeIndexChunk>,
    source: &KnowledgeIndexTextSource,
    lines: &[String],
    line_start: usize,
    line_end: usize,
) {
    let text = lines
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if text.is_empty() {
        return;
    }
    let chunk_id = format!("chunk-{:04}", chunks.len() + 1);
    chunks.push(KnowledgeIndexChunk {
        chunk_id,
        line_start,
        line_end,
        text,
        source_kind: source.source_kind.clone(),
        source_path: source.source_path.clone(),
    });
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
    use super::{KnowledgeIndexConfig, KnowledgeIndexService, KnowledgeModality};
    use std::fs;
    use std::path::{Path, PathBuf};
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
