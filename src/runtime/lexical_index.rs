use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::doc;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{Index, IndexWriter, TantivyDocument};

const LEXICAL_INDEX_SCHEMA_VERSION: u32 = 2;
const WRITER_MEMORY_BYTES: usize = 50_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LexicalIndexMetadata {
    schema_version: u32,
    root: String,
    generated_at: String,
}

#[derive(Clone, Copy)]
struct LexicalFields {
    key: Field,
    modality: Field,
    document_content: Field,
    image_content: Field,
    audio_content: Field,
    video_content: Field,
    title: Field,
    path: Field,
}

pub struct LexicalIndexBuilder {
    directory: PathBuf,
    writer: IndexWriter,
    fields: LexicalFields,
    metadata: LexicalIndexMetadata,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct LexicalSearchScore {
    pub raw: f32,
    pub normalized: f32,
}

impl LexicalIndexBuilder {
    pub fn create(directory: &Path, root: &str, generated_at: &str) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(|error| {
            format!(
                "failed to create knowledge lexical index directory {}: {error}",
                directory.display()
            )
        })?;
        let schema = lexical_schema();
        let index = if directory.join("meta.json").exists() {
            Index::open_in_dir(directory).map_err(|error| {
                format!(
                    "failed to open knowledge lexical index {}: {error}",
                    directory.display()
                )
            })?
        } else {
            Index::create_in_dir(directory, schema.clone()).map_err(|error| {
                format!(
                    "failed to create knowledge lexical index {}: {error}",
                    directory.display()
                )
            })?
        };
        if index.schema() != schema {
            return Err(format!(
                "knowledge lexical index {} has an incompatible schema; rebuild the index directory",
                directory.display()
            ));
        }
        let fields = lexical_fields(&schema)?;
        let writer = index.writer(WRITER_MEMORY_BYTES).map_err(|error| {
            format!(
                "failed to create knowledge lexical index writer {}: {error}",
                directory.display()
            )
        })?;
        writer.delete_all_documents().map_err(|error| {
            format!(
                "failed to reset knowledge lexical index {}: {error}",
                directory.display()
            )
        })?;
        Ok(Self {
            directory: directory.to_path_buf(),
            writer,
            fields,
            metadata: LexicalIndexMetadata {
                schema_version: LEXICAL_INDEX_SCHEMA_VERSION,
                root: root.to_string(),
                generated_at: generated_at.to_string(),
            },
        })
    }

    pub fn add_document(
        &mut self,
        key: &str,
        modality: &str,
        content_terms: &str,
        title_terms: &str,
        path_terms: &str,
    ) -> Result<(), String> {
        let content_field = match modality {
            "image" => self.fields.image_content,
            "audio" => self.fields.audio_content,
            "video" => self.fields.video_content,
            _ => self.fields.document_content,
        };
        let mut document = doc!(
            self.fields.key => key,
            self.fields.modality => modality,
            self.fields.title => title_terms,
            self.fields.path => path_terms,
        );
        document.add_text(content_field, content_terms);
        self.writer.add_document(document).map_err(|error| {
            format!(
                "failed to add a document to knowledge lexical index {}: {error}",
                self.directory.display()
            )
        })?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), String> {
        self.writer.commit().map_err(|error| {
            format!(
                "failed to commit knowledge lexical index {}: {error}",
                self.directory.display()
            )
        })?;
        let metadata_path = metadata_path(&self.directory);
        let payload = serde_json::to_vec(&self.metadata).map_err(|error| {
            format!(
                "failed to serialize knowledge lexical index metadata {}: {error}",
                metadata_path.display()
            )
        })?;
        fs::write(&metadata_path, payload).map_err(|error| {
            format!(
                "failed to write knowledge lexical index metadata {}: {error}",
                metadata_path.display()
            )
        })
    }
}

pub fn search(
    directory: &Path,
    root: &str,
    generated_at: &str,
    query_terms: &[String],
    limit: usize,
) -> Result<HashMap<String, LexicalSearchScore>, String> {
    if query_terms.is_empty() || limit == 0 {
        return Ok(HashMap::new());
    }
    let metadata = load_metadata(directory)?;
    if metadata.schema_version != LEXICAL_INDEX_SCHEMA_VERSION {
        return Err(format!(
            "knowledge lexical index {} uses schema {}, expected {}",
            directory.display(),
            metadata.schema_version,
            LEXICAL_INDEX_SCHEMA_VERSION
        ));
    }
    if metadata.root != root || metadata.generated_at != generated_at {
        return Err("knowledge lexical index does not match the current manifest".to_string());
    }

    let index = Index::open_in_dir(directory).map_err(|error| {
        format!(
            "failed to open knowledge lexical index {}: {error}",
            directory.display()
        )
    })?;
    let schema = index.schema();
    let fields = lexical_fields(&schema)?;
    let reader = index.reader().map_err(|error| {
        format!(
            "failed to open knowledge lexical index reader {}: {error}",
            directory.display()
        )
    })?;
    let searcher = reader.searcher();
    let mut parser = QueryParser::for_index(
        &index,
        vec![
            fields.document_content,
            fields.image_content,
            fields.audio_content,
            fields.video_content,
            fields.title,
            fields.path,
        ],
    );
    parser.set_field_boost(fields.document_content, 1.2);
    parser.set_field_boost(fields.image_content, 1.0);
    parser.set_field_boost(fields.audio_content, 0.9);
    parser.set_field_boost(fields.video_content, 0.9);
    parser.set_field_boost(fields.title, 2.5);
    parser.set_field_boost(fields.path, 1.5);
    let query_text = query_terms.join(" ");
    let query = parser.parse_query(&query_text).map_err(|error| {
        format!(
            "failed to parse knowledge lexical query for {}: {error}",
            directory.display()
        )
    })?;
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(limit.saturating_mul(4)))
        .map_err(|error| {
            format!(
                "failed to search knowledge lexical index {}: {error}",
                directory.display()
            )
        })?;

    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, address) in top_docs {
        let document = searcher.doc::<TantivyDocument>(address).map_err(|error| {
            format!(
                "failed to read knowledge lexical result from {}: {error}",
                directory.display()
            )
        })?;
        let Some(key) = document
            .get_first(fields.key)
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        let modality = document
            .get_first(fields.modality)
            .and_then(|value| value.as_str())
            .unwrap_or("document");
        hits.push((score, key.to_string(), modality.to_string()));
    }
    let mut maximum_by_modality = HashMap::<String, f32>::new();
    for (score, _, modality) in &hits {
        maximum_by_modality
            .entry(modality.clone())
            .and_modify(|maximum| *maximum = maximum.max(*score))
            .or_insert(*score);
    }
    let mut normalized_hits = hits
        .into_iter()
        .map(|(score, key, modality)| {
            let maximum = maximum_by_modality
                .get(&modality)
                .copied()
                .unwrap_or(1.0)
                .max(f32::EPSILON);
            let modality_weight = match modality.as_str() {
                "image" => 1.0,
                "audio" | "video" => 0.9,
                _ => 1.2,
            };
            let normalized = (score / maximum * modality_weight / 1.2).clamp(0.0, 1.0);
            let stable_score = (normalized * 100_000.0).round() / 100_000.0;
            (
                LexicalSearchScore {
                    raw: score,
                    normalized: stable_score,
                },
                key,
            )
        })
        .collect::<Vec<_>>();
    normalized_hits.sort_by(|left, right| {
        right
            .0
            .normalized
            .total_cmp(&left.0.normalized)
            .then_with(|| left.1.cmp(&right.1))
    });
    normalized_hits.truncate(limit);
    Ok(normalized_hits
        .into_iter()
        .map(|(score, key)| (key, score))
        .collect())
}

fn lexical_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("key", STRING | STORED);
    builder.add_text_field("modality", STRING | STORED);
    builder.add_text_field("document_content", TEXT);
    builder.add_text_field("image_content", TEXT);
    builder.add_text_field("audio_content", TEXT);
    builder.add_text_field("video_content", TEXT);
    builder.add_text_field("title", TEXT);
    builder.add_text_field("path", TEXT);
    builder.build()
}

fn lexical_fields(schema: &Schema) -> Result<LexicalFields, String> {
    Ok(LexicalFields {
        key: schema.get_field("key").map_err(|error| error.to_string())?,
        modality: schema
            .get_field("modality")
            .map_err(|error| error.to_string())?,
        document_content: schema
            .get_field("document_content")
            .map_err(|error| error.to_string())?,
        image_content: schema
            .get_field("image_content")
            .map_err(|error| error.to_string())?,
        audio_content: schema
            .get_field("audio_content")
            .map_err(|error| error.to_string())?,
        video_content: schema
            .get_field("video_content")
            .map_err(|error| error.to_string())?,
        title: schema
            .get_field("title")
            .map_err(|error| error.to_string())?,
        path: schema
            .get_field("path")
            .map_err(|error| error.to_string())?,
    })
}

fn metadata_path(directory: &Path) -> PathBuf {
    directory.join("knowledge-metadata.json")
}

fn load_metadata(directory: &Path) -> Result<LexicalIndexMetadata, String> {
    let path = metadata_path(directory);
    let payload = fs::read(&path).map_err(|error| {
        format!(
            "failed to read knowledge lexical index metadata {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_slice(&payload).map_err(|error| {
        format!(
            "failed to parse knowledge lexical index metadata {}: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{search, LexicalIndexBuilder};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn equal_document_matches_keep_equal_scores_across_other_modalities() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("harbor-lexical-tie-{nonce}"));
        let mut builder =
            LexicalIndexBuilder::create(&directory, "/knowledge", "1").expect("builder");
        builder
            .add_document("alpha", "document", "alpha note about spring", "", "")
            .expect("alpha");
        builder
            .add_document("beta", "document", "beta note about spring", "", "")
            .expect("beta");
        builder
            .add_document("image", "image", "alpha spring view", "", "")
            .expect("image");
        builder.finish().expect("finish");

        let scores =
            search(&directory, "/knowledge", "1", &["spring".to_string()], 10).expect("search");

        assert_eq!(scores.get("alpha"), scores.get("beta"));
        assert!(scores["alpha"].raw > 0.0);
        assert!(scores["alpha"].normalized > scores["image"].normalized);
        let _ = fs::remove_dir_all(directory);
    }
}
