use crate::config::WikiCompletionStyle;
use crate::resolution::{ConnectionGraph, Slug};
use crate::utils::ByteRange;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct CompletionParams {
    pub path: PathBuf,
    pub source_text: String,
    pub cursor_offset: usize,
    pub graph: ConnectionGraph,
    pub style: WikiCompletionStyle,
    pub max_candidates: usize,
}

#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub replace_range: ByteRange,
    pub insert_text: String,
}

pub fn complete_at(params: CompletionParams) -> Vec<CompletionItem> {
    let prefix = detect_prefix(&params.source_text, params.cursor_offset);
    match prefix.kind {
        PromptKind::WikiDoc => complete_docs(&params, &prefix.value),
        PromptKind::WikiHeading => complete_headings(&params, &prefix.value),
        PromptKind::Tag => complete_tags(&params, &prefix.value),
        PromptKind::None => Vec::new(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PromptKind {
    None,
    WikiDoc,
    WikiHeading,
    Tag,
}

struct Prompt {
    kind: PromptKind,
    value: String,
    range: ByteRange,
}

fn detect_prefix(input: &str, cursor_offset: usize) -> Prompt {
    let offset = cursor_offset.min(input.len());
    let prefix = &input[..offset];
    if let Some(start) = prefix.rfind("[[#") {
        return Prompt {
            kind: PromptKind::WikiHeading,
            value: prefix[start + 3..].to_string(),
            range: ByteRange::new(start + 3, offset),
        };
    }
    if let Some(start) = prefix.rfind("[[") {
        return Prompt {
            kind: PromptKind::WikiDoc,
            value: prefix[start + 2..].to_string(),
            range: ByteRange::new(start + 2, offset),
        };
    }
    if let Some(start) = prefix.rfind('#')
        && (start == 0 || !prefix.as_bytes()[start - 1].is_ascii_alphanumeric())
    {
        return Prompt {
            kind: PromptKind::Tag,
            value: prefix[start..].to_string(),
            range: ByteRange::new(start, offset),
        };
    }

    Prompt {
        kind: PromptKind::None,
        value: String::new(),
        range: ByteRange::new(offset, offset),
    }
}

fn complete_docs(params: &CompletionParams, needle: &str) -> Vec<CompletionItem> {
    let Some(source_doc) = params.graph.document_for_path(&params.path) else {
        return Vec::new();
    };
    let mut items = params
        .graph
        .documents
        .iter()
        .filter(|doc| doc.path != source_doc.path)
        .filter(|doc| {
            Slug::is_subsequence(doc.title_slug.as_str(), needle)
                || Slug::is_subsequence(&doc.file_stem, needle)
        })
        .map(|doc| CompletionItem {
            label: doc.title_text.clone(),
            detail: Some(doc.rel_path.display().to_string()),
            replace_range: detect_prefix(&params.source_text, params.cursor_offset).range,
            insert_text: match params.style {
                WikiCompletionStyle::TitleSlug => doc.title_slug.as_str().to_string(),
                WikiCompletionStyle::Title => doc.title_text.clone(),
                WikiCompletionStyle::FileStem => doc.file_stem.clone(),
                WikiCompletionStyle::FilePathStem => {
                    doc.rel_path.with_extension("").display().to_string()
                }
            },
        })
        .collect::<Vec<_>>();
    items.truncate(params.max_candidates);
    items
}

fn complete_headings(params: &CompletionParams, needle: &str) -> Vec<CompletionItem> {
    let Some(source_doc) = params.graph.document_for_path(&params.path) else {
        return Vec::new();
    };
    let range = detect_prefix(&params.source_text, params.cursor_offset).range;
    let mut items = source_doc
        .headings
        .iter()
        .filter(|(slug, _)| Slug::is_subsequence(slug.as_str(), needle))
        .flat_map(|(slug, destinations)| {
            destinations.iter().map(|destination| CompletionItem {
                label: destination.name.clone(),
                detail: Some(source_doc.rel_path.display().to_string()),
                replace_range: range,
                insert_text: slug.as_str().to_string(),
            })
        })
        .collect::<Vec<_>>();
    items.truncate(params.max_candidates);
    items
}

fn complete_tags(params: &CompletionParams, needle: &str) -> Vec<CompletionItem> {
    let range = detect_prefix(&params.source_text, params.cursor_offset).range;
    let mut tags = params
        .graph
        .documents
        .iter()
        .flat_map(|doc| doc.tags.iter())
        .filter(|(tag, _)| Slug::is_subsequence(tag, needle))
        .map(|(tag, destinations)| CompletionItem {
            label: tag.clone(),
            detail: Some(format!("{} refs", destinations.len())),
            replace_range: range,
            insert_text: tag.clone(),
        })
        .collect::<Vec<_>>();
    tags.sort_by(|left, right| left.label.cmp(&right.label));
    tags.truncate(params.max_candidates);
    tags
}
