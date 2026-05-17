use crate::completion::{CompletionParams, complete_at};
use crate::diagnostics::{DiagnosticConfig, DiagnosticSeverity, check_diagnostics};
use crate::resolution::ConnectionGraph;
use crate::utils::{PositionEncoding, Text};
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn completion(
    graph: &ConnectionGraph,
    path: PathBuf,
    text: &Text,
    line: u32,
    character: u32,
) -> Value {
    let offset = match text.byte_offset(
        &lsp_types::Position::new(line, character),
        PositionEncoding::Utf16,
    ) {
        Ok(offset) => offset,
        Err(_) => return json!([]),
    };
    let items = complete_at(CompletionParams {
        path,
        source_text: text.as_str().to_string(),
        cursor_offset: offset,
        graph: graph.clone(),
        style: crate::config::WikiCompletionStyle::TitleSlug,
        max_candidates: 50,
    });
    json!(
        items
            .into_iter()
            .map(|item| json!({
                "label": item.label,
                "detail": item.detail,
                "textEdit": {
                    "range": range_json(text, item.replace_range),
                    "newText": item.insert_text,
                },
            }))
            .collect::<Vec<_>>()
    )
}

pub fn hover(graph: &ConnectionGraph, path: &PathBuf, offset: usize) -> Option<Value> {
    let doc = graph.documents.iter().find(|doc| &doc.path == path)?;
    for symbol in &doc.structure.symbols {
        if symbol.full_range.contains(offset) {
            return Some(json!({
                "contents": {
                    "kind": "markdown",
                    "value": format!("`{:?}`", symbol.kind)
                }
            }));
        }
    }
    None
}

pub fn definition(graph: &ConnectionGraph, path: &PathBuf, _text: &Text, offset: usize) -> Value {
    let matches = graph
        .resolved_references
        .iter()
        .filter(|reference| &reference.source_path == path)
        .filter(|reference| reference.full_range.contains(offset))
        .flat_map(|reference| reference.destinations.iter())
        .filter_map(|destination| {
            let target_doc = graph.documents.iter().find(|doc| doc.path == destination.path)?;
            Some(json!({
                "uri": url::Url::from_file_path(&destination.path).ok()?.to_string(),
                "range": range_json(&target_doc.structure.text, destination.range.unwrap_or_default()),
            }))
        })
        .collect::<Vec<_>>();
    json!(matches)
}

pub fn references(graph: &ConnectionGraph, path: &PathBuf, offset: usize) -> Value {
    let mut results = Vec::new();
    let Some(doc) = graph.documents.iter().find(|doc| &doc.path == path) else {
        return json!(results);
    };
    let Some(target_symbol) = doc
        .structure
        .symbols
        .iter()
        .find(|symbol| symbol.full_range.contains(offset))
    else {
        return json!(results);
    };

    for reference in &graph.resolved_references {
        let hit = reference.destinations.iter().any(|destination| {
            destination.path == *path && destination.range == target_symbol.name_range
        });
        if hit {
            results.push(json!({
                "uri": url::Url::from_file_path(&reference.source_path).ok().map(|uri| uri.to_string()),
                "range": range_json(&doc.structure.text, reference.name_range.unwrap_or(reference.full_range)),
            }));
        }
    }

    json!(results)
}

pub fn document_symbols(graph: &ConnectionGraph, path: &PathBuf) -> Value {
    let Some(doc) = graph.documents.iter().find(|doc| &doc.path == path) else {
        return json!([]);
    };
    let symbols = doc
        .structure
        .index
        .headings
        .iter()
        .map(|heading| {
            json!({
                "name": heading.title.text,
                "kind": 13,
                "range": range_json(&doc.structure.text, heading.scope),
                "selectionRange": range_json(&doc.structure.text, heading.title.range),
            })
        })
        .collect::<Vec<_>>();
    json!(symbols)
}

pub fn diagnostics(graph: &ConnectionGraph) -> Vec<(PathBuf, Value)> {
    let diagnostics = check_diagnostics(
        graph,
        &DiagnosticConfig {
            min_severity: DiagnosticSeverity::Info,
        },
    );
    let mut grouped: std::collections::HashMap<PathBuf, Vec<Value>> =
        std::collections::HashMap::new();
    for diagnostic in diagnostics {
        let Some(doc) = graph
            .documents
            .iter()
            .find(|doc| doc.path == diagnostic.path)
        else {
            continue;
        };
        grouped
            .entry(diagnostic.path.clone())
            .or_default()
            .push(json!({
                "range": range_json(&doc.structure.text, diagnostic.range),
                "severity": match diagnostic.severity {
                    DiagnosticSeverity::Error => 1,
                    DiagnosticSeverity::Warning => 2,
                    DiagnosticSeverity::Info => 3,
                },
                "code": format!("{:?}", diagnostic.code),
                "message": diagnostic.message,
                "relatedInformation": diagnostic.related.into_iter().filter_map(|related| {
                    Some(json!({
                        "location": {
                            "uri": url::Url::from_file_path(related.path).ok()?.to_string(),
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 0 }
                            }
                        },
                        "message": related.message,
                    }))
                }).collect::<Vec<_>>(),
            }));
    }

    grouped
        .into_iter()
        .map(|(path, entries)| (path, json!(entries)))
        .collect()
}

fn range_json(text: &Text, range: crate::utils::ByteRange) -> Value {
    let start = text
        .to_lsp_position(range.start, PositionEncoding::Utf16)
        .unwrap_or(crate::utils::LspPosition {
            line: 0,
            character: 0,
        });
    let end = text
        .to_lsp_position(range.end, PositionEncoding::Utf16)
        .unwrap_or(crate::utils::LspPosition {
            line: 0,
            character: 0,
        });
    json!({
        "start": { "line": start.line, "character": start.character },
        "end": { "line": end.line, "character": end.character },
    })
}
