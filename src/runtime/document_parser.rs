//! Lightweight, in-process document parsing for the knowledge index.
//!
//! Parsers produce a stable intermediate representation. Chunking and indexing
//! intentionally remain outside this module so a remote/heavy parser can be
//! introduced later without changing the knowledge pipeline.

use std::fs;
use std::path::Path;

use markitdown::model::ConversionOptions;
use markitdown::MarkItDown;
use serde::{Deserialize, Serialize};

const MAX_NATIVE_TEXT_BYTES: u64 = 512 * 1024;
const NATIVE_PARSER_KEY: &str = "harbor_native:v1";
const MARKITDOWN_PARSER_KEY: &str = "markitdown:0.1.11:v1";
const NATIVE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "json", "csv", "yaml", "yml", "log",
];
const MARKITDOWN_EXTENSIONS: &[&str] = &[
    "html", "htm", "xml", "rss", "atom", "pdf", "docx", "pptx", "xlsx", "zip",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParsedBlockKind {
    Heading,
    Paragraph,
    List,
    Table,
    Code,
    StructuredData,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedBlock {
    pub block_type: ParsedBlockKind,
    pub text: String,
    #[serde(default)]
    pub section_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_end: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedAsset {
    pub asset_id: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedDocument {
    pub parser_key: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub blocks: Vec<ParsedBlock>,
    #[serde(default)]
    pub assets: Vec<ParsedAsset>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub requires_advanced_parser: bool,
}

impl ParsedDocument {
    pub fn failed(parser_key: String, warning: String) -> Self {
        Self {
            parser_key,
            source_kind: "document_parse_failed".to_string(),
            title: None,
            blocks: Vec::new(),
            assets: Vec::new(),
            warnings: vec![warning],
            requires_advanced_parser: false,
        }
    }

    pub fn searchable_text(&self) -> String {
        self.blocks
            .iter()
            .map(|block| block.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn provider_key(&self) -> String {
        self.parser_key
            .split(':')
            .next()
            .unwrap_or(self.parser_key.as_str())
            .to_string()
    }
}

trait DocumentParser {
    fn parser_key(&self) -> &'static str;
    fn supports(&self, extension: &str) -> bool;
    fn parse(&self, path: &Path) -> Result<ParsedDocument, String>;
}

struct NativeDocumentParser;

impl DocumentParser for NativeDocumentParser {
    fn parser_key(&self) -> &'static str {
        NATIVE_PARSER_KEY
    }

    fn supports(&self, extension: &str) -> bool {
        NATIVE_EXTENSIONS.contains(&extension)
    }

    fn parse(&self, path: &Path) -> Result<ParsedDocument, String> {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("failed to inspect document {}: {error}", path.display()))?;
        if metadata.len() > MAX_NATIVE_TEXT_BYTES {
            return Err(format!(
                "document {} exceeds the lightweight parser limit of {} bytes",
                path.display(),
                MAX_NATIVE_TEXT_BYTES
            ));
        }

        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read document {}: {error}", path.display()))?;
        let (mut text, mut warnings) = match String::from_utf8(bytes) {
            Ok(text) => (text, Vec::new()),
            Err(error) => (
                String::from_utf8_lossy(error.as_bytes()).into_owned(),
                vec!["document contained invalid UTF-8; invalid bytes were replaced".to_string()],
            ),
        };
        let extension = extension(path);
        let block_type = match extension.as_str() {
            "json" => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => {
                    text = serde_json::to_string_pretty(&value).unwrap_or(text);
                    ParsedBlockKind::StructuredData
                }
                Err(error) => {
                    warnings.push(format!(
                        "JSON structure could not be validated; indexed as plain text: {error}"
                    ));
                    ParsedBlockKind::StructuredData
                }
            },
            "csv" => ParsedBlockKind::Table,
            _ => ParsedBlockKind::Paragraph,
        };

        let blocks = if matches!(extension.as_str(), "md" | "markdown") {
            markdown_blocks(&text)
        } else if text.trim().is_empty() {
            Vec::new()
        } else {
            vec![ParsedBlock {
                block_type,
                text: text.trim().to_string(),
                section_path: Vec::new(),
                page_number: None,
                source_start: Some(0),
                source_end: Some(text.len()),
                asset_refs: Vec::new(),
            }]
        };

        Ok(ParsedDocument {
            parser_key: self.parser_key().to_string(),
            source_kind: if matches!(extension.as_str(), "md" | "markdown") {
                "structured_markdown".to_string()
            } else {
                "structured_document".to_string()
            },
            title: first_heading(&blocks),
            blocks,
            assets: Vec::new(),
            warnings,
            requires_advanced_parser: false,
        })
    }
}

struct MarkItDownDocumentParser;

impl DocumentParser for MarkItDownDocumentParser {
    fn parser_key(&self) -> &'static str {
        MARKITDOWN_PARSER_KEY
    }

    fn supports(&self, extension: &str) -> bool {
        MARKITDOWN_EXTENSIONS.contains(&extension)
    }

    fn parse(&self, path: &Path) -> Result<ParsedDocument, String> {
        let extension = extension(path);
        let converter = MarkItDown::new();
        let converted = converter
            .convert(
                &path.to_string_lossy(),
                Some(ConversionOptions {
                    file_extension: Some(format!(".{extension}")),
                    url: None,
                    llm_client: None,
                    llm_model: None,
                }),
            )
            .map_err(|error| format!("failed to parse document {}: {error}", path.display()))?
            .ok_or_else(|| format!("parser returned no result for {}", path.display()))?;
        let text = converted.text_content.trim().to_string();
        let blocks = markdown_blocks(&text);
        let mut warnings = Vec::new();
        let mut requires_advanced_parser = false;

        if extension == "pdf" {
            let source_size = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
            if pdf_needs_advanced_parser(source_size, &text) {
                requires_advanced_parser = true;
                warnings.push(
                    "PDF yielded very little text and may be scanned or use a complex layout; \
                     advanced OCR/layout parsing is recommended"
                        .to_string(),
                );
            }
        }
        if text.is_empty() {
            warnings.push("document parser produced no searchable text".to_string());
        }

        Ok(ParsedDocument {
            parser_key: self.parser_key().to_string(),
            source_kind: "normalized_markdown".to_string(),
            title: first_heading(&blocks),
            blocks,
            assets: Vec::new(),
            warnings,
            requires_advanced_parser,
        })
    }
}

pub struct DocumentParserRegistry {
    parsers: Vec<Box<dyn DocumentParser>>,
}

impl Default for DocumentParserRegistry {
    fn default() -> Self {
        Self {
            parsers: vec![
                Box::new(NativeDocumentParser),
                Box::new(MarkItDownDocumentParser),
            ],
        }
    }
}

impl DocumentParserRegistry {
    pub fn parser_key_for_path(&self, path: &Path) -> Option<String> {
        let extension = extension(path);
        self.parsers
            .iter()
            .find(|parser| parser.supports(&extension))
            .map(|parser| parser.parser_key().to_string())
    }

    pub fn parse(&self, path: &Path) -> Result<ParsedDocument, String> {
        let extension = extension(path);
        let parser = self
            .parsers
            .iter()
            .find(|parser| parser.supports(&extension))
            .ok_or_else(|| {
                format!(
                    "no lightweight document parser is registered for {}",
                    path.display()
                )
            })?;
        parser.parse(path)
    }
}

pub fn parser_key_for_path(path: &Path) -> Option<String> {
    DocumentParserRegistry::default().parser_key_for_path(path)
}

pub fn parse_document(path: &Path) -> Result<ParsedDocument, String> {
    DocumentParserRegistry::default().parse(path)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn markdown_blocks(text: &str) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    let mut section_path = Vec::<String>::new();
    let mut paragraph = Vec::<(usize, &str)>::new();
    let mut in_code_block = false;

    for (line_index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            paragraph.push((line_index, line));
            continue;
        }
        if !in_code_block {
            if let Some((level, title)) = markdown_heading(line) {
                flush_paragraph(&mut blocks, &mut paragraph, &section_path);
                section_path.truncate(level.saturating_sub(1));
                section_path.push(title.to_string());
                blocks.push(ParsedBlock {
                    block_type: ParsedBlockKind::Heading,
                    text: line.trim().to_string(),
                    section_path: section_path.clone(),
                    page_number: None,
                    source_start: Some(line_index),
                    source_end: Some(line_index),
                    asset_refs: Vec::new(),
                });
                continue;
            }
            if line.trim().is_empty() {
                flush_paragraph(&mut blocks, &mut paragraph, &section_path);
                continue;
            }
        }
        paragraph.push((line_index, line));
    }
    flush_paragraph(&mut blocks, &mut paragraph, &section_path);
    blocks
}

fn flush_paragraph(
    blocks: &mut Vec<ParsedBlock>,
    paragraph: &mut Vec<(usize, &str)>,
    section_path: &[String],
) {
    if paragraph.is_empty() {
        return;
    }
    let text = paragraph
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if text.is_empty() {
        paragraph.clear();
        return;
    }
    let block_type = if text.starts_with("```") {
        ParsedBlockKind::Code
    } else if text.lines().all(|line| {
        matches!(
            line.trim_start().chars().next(),
            Some('-' | '*' | '+')
        )
    }) {
        ParsedBlockKind::List
    } else if text.lines().filter(|line| line.contains('|')).count() >= 2 {
        ParsedBlockKind::Table
    } else {
        ParsedBlockKind::Paragraph
    };
    blocks.push(ParsedBlock {
        block_type,
        text,
        section_path: section_path.to_vec(),
        page_number: None,
        source_start: paragraph.first().map(|(line, _)| *line),
        source_end: paragraph.last().map(|(line, _)| *line),
        asset_refs: Vec::new(),
    });
    paragraph.clear();
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|character| *character == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let title = trimmed[level..].trim();
    (!title.is_empty()).then_some((level, title))
}

fn first_heading(blocks: &[ParsedBlock]) -> Option<String> {
    blocks
        .iter()
        .find(|block| block.block_type == ParsedBlockKind::Heading)
        .map(|block| block.text.trim_start_matches('#').trim().to_string())
}

fn pdf_needs_advanced_parser(source_size: u64, extracted_text: &str) -> bool {
    let effective_chars = extracted_text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    source_size > 64 * 1024 && effective_chars < 80
}

#[cfg(test)]
mod tests {
    use super::{
        markdown_blocks, parse_document, parser_key_for_path, pdf_needs_advanced_parser,
        ParsedBlockKind,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn markdown_parser_preserves_heading_paths_and_block_types() {
        let blocks = markdown_blocks(
            "# Guide\n\nIntro\n\n## Setup\n\n- first\n- second\n\n| A | B |\n| - | - |",
        );

        assert_eq!(blocks[0].block_type, ParsedBlockKind::Heading);
        assert_eq!(blocks[1].section_path, vec!["Guide"]);
        assert_eq!(blocks[2].section_path, vec!["Guide", "Setup"]);
        assert_eq!(blocks[3].block_type, ParsedBlockKind::List);
        assert_eq!(blocks[4].block_type, ParsedBlockKind::Table);
    }

    #[test]
    fn native_json_parser_normalizes_valid_json() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("harbor-parser-{suffix}.json"));
        fs::write(&path, r#"{"name":"HarborOS","enabled":true}"#).unwrap();

        let parsed = parse_document(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(parsed.parser_key, "harbor_native:v1");
        assert!(parsed.searchable_text().contains("\"name\": \"HarborOS\""));
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn registry_routes_lightweight_and_office_formats() {
        assert_eq!(
            parser_key_for_path(std::path::Path::new("data.json")).as_deref(),
            Some("harbor_native:v1")
        );
        assert_eq!(
            parser_key_for_path(std::path::Path::new("manual.docx")).as_deref(),
            Some("markitdown:0.1.11:v1")
        );
        assert!(parser_key_for_path(std::path::Path::new("archive.bin")).is_none());
    }

    #[test]
    fn pdf_detection_only_flags_large_low_text_documents() {
        assert!(pdf_needs_advanced_parser(128 * 1024, "few words"));
        assert!(!pdf_needs_advanced_parser(8 * 1024, "few words"));
        assert!(!pdf_needs_advanced_parser(
            128 * 1024,
            &"searchable content ".repeat(10)
        ));
    }
}
