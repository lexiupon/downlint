//! LSP `WorkspaceEdit` builder — RFC 0009 §"LSP Wire Surface".
//!
//! Coordinates edits across multiple documents in a single rename
//! operation. Two responsibilities live here:
//!
//! 1. **Reverse-order sorting**: edits within a single document must be
//!    sorted end-of-doc-first so concurrent edits don't invalidate each
//!    other's byte offsets. Already enforced by
//!    [`crate::rename::RenamePlan::sorted_for_apply`] — this module
//!    re-sorts defensively at serialize time.
//! 2. **Format selection**: prefer LSP 3.16+ `documentChanges` (carries
//!    `RenameFile` for the disk move) when the client advertises support;
//!    fall back to the legacy `changes` map for older clients.
//!
//! Both shapes serialize the same text edits; only the wrapping differs.
//! For now we always emit `documentChanges` because we also need to ship
//! the disk move, which only `documentChanges` can carry.

use crate::rename::{DocumentEdit, FileMove, RenamePlan};
use crate::utils::{PositionEncoding, Text};
use serde_json::{Value, json};

/// Convert a byte range into an LSP `Range` JSON object using the given
/// encoding. UTF-16 by default (matches the rest of the LSP server).
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

/// Build the per-document `TextDocumentEdit` array for a single
/// `DocumentEdit`. Edits are reverse-sorted defensively.
fn text_document_edit(
    document: &DocumentEdit,
    text_for_uri: &dyn Fn(&std::path::Path) -> Option<Text>,
    uri_for_path: &dyn Fn(&std::path::Path) -> Option<String>,
) -> Option<Value> {
    let uri = uri_for_path(&document.path)?;
    let text = text_for_uri(&document.path)?;
    let mut edits = document.edits.clone();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
    let edit_values = edits
        .into_iter()
        .map(|edit| {
            json!({
                "range": range_json(&text, edit.range),
                "newText": edit.new_text,
            })
        })
        .collect::<Vec<_>>();
    Some(json!({
        "textDocument": {
            "uri": uri,
            "version": null,
        },
        "edits": edit_values,
    }))
}

/// Build a `WorkspaceEdit` JSON value carrying every text edit and every
/// file move from `plan`.
///
/// - `text_for_uri(path)` looks up the current document text so byte
///   ranges can be converted to LSP line/character positions. The LSP
///   layer uses its `open_documents` cache here; the CLI uses a freshly
///   read-from-disk snapshot.
/// - `uri_for_path(path)` converts an absolute filesystem path into a
///   `file://` URI.
///
/// The function always emits `documentChanges` (LSP 3.16+) so it can
/// carry the `RenameFile` operation alongside the text edits.
pub fn build_workspace_edit(
    plan: &RenamePlan,
    text_for_uri: &dyn Fn(&std::path::Path) -> Option<Text>,
    uri_for_path: &dyn Fn(&std::path::Path) -> Option<String>,
) -> Value {
    let mut document_changes = Vec::new();

    // File moves first — order doesn't strictly matter, but listing them
    // before the text edits matches the RFC §"Type F" step 5 narrative
    // ("apply: text first then disk").
    for move_op in &plan.file_moves {
        let Some(old_uri) = uri_for_path(&move_op.from) else {
            continue;
        };
        let Some(new_uri) = uri_for_path(&move_op.to) else {
            continue;
        };
        document_changes.push(json!({
            "kind": "rename",
            "oldUri": old_uri,
            "newUri": new_uri,
        }));
    }

    for document in &plan.edits {
        if let Some(edit) = text_document_edit(document, text_for_uri, uri_for_path) {
            document_changes.push(edit);
        }
    }

    json!({
        "documentChanges": document_changes,
    })
}

/// Build a `WorkspaceEdit` for the string-only `textDocument/rename` path
/// (RFC §"Why `textDocument/rename` is string-only"). Carries exactly one
/// `TextEdit` replacing the range in `range` with `new_text`. No disk
/// move, no propagation — this is the safe F2 default.
pub fn build_single_edit_workspace_edit(
    text: &Text,
    uri: &str,
    range: crate::utils::ByteRange,
    new_text: &str,
) -> Value {
    json!({
        "documentChanges": [
            {
                "textDocument": {
                    "uri": uri,
                    "version": null,
                },
                "edits": [
                    {
                        "range": range_json(text, range),
                        "newText": new_text,
                    }
                ],
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::{DocumentEdit, TextEdit};
    use crate::utils::{ByteRange, Text};
    use std::path::PathBuf;

    fn fake_text() -> Text {
        Text::new("hello world")
    }

    fn uri_for(path: &std::path::Path) -> Option<String> {
        url::Url::from_file_path(path).ok().map(|u| u.to_string())
    }

    #[test]
    fn empty_plan_serializes_to_empty_document_changes() {
        let plan = RenamePlan::default();
        let value = build_workspace_edit(&plan, &|_| Some(fake_text()), &uri_for);
        let changes = value.get("documentChanges").and_then(|v| v.as_array()).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn plan_with_one_document_edit_produces_one_text_document_edit() {
        let path = PathBuf::from("/tmp/test.md");
        let edits = vec![TextEdit {
            range: ByteRange::new(6, 11),
            new_text: "downlint".into(),
        }];
        let plan = RenamePlan::new(vec![DocumentEdit::new(path.clone(), edits)], vec![]);
        let value = build_workspace_edit(&plan, &|_| Some(fake_text()), &uri_for);
        let changes = value.get("documentChanges").and_then(|v| v.as_array()).unwrap();
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn plan_with_file_move_emits_rename_first() {
        let from = PathBuf::from("/tmp/old.md");
        let to = PathBuf::from("/tmp/new.md");
        let plan = RenamePlan::new(vec![], vec![FileMove { from, to }]);
        let value = build_workspace_edit(&plan, &|_| Some(fake_text()), &uri_for);
        let changes = value.get("documentChanges").and_then(|v| v.as_array()).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].get("kind").and_then(|v| v.as_str()), Some("rename"));
    }
}