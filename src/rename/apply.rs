//! Apply a [`RenamePlan`] to disk (CLI) — RFC 0009 §"Type F" step 5 and
//! §"Type L".
//!
//! The CLI is responsible for the actual disk writes — text edits first,
//! then the file move. Atomicity is handled at the CLI layer via a
//! `.downlint/.rename.lock` file (RFC §"Risks" #6). This module exposes
//! the in-memory application logic so tests can exercise it without
//! touching the filesystem.

use crate::rename::{DocumentEdit, RenamePlan, TextEdit};
use crate::utils::Text;

/// Apply a list of [`TextEdit`]s to a [`Text`] document. Edits within a
/// single document must already be sorted in reverse order (end-of-doc
/// first) so concurrent edits don't invalidate each other's offsets —
/// `RenamePlan::sorted_for_apply` does this.
pub fn apply_text_edits(text: &Text, edits: &[TextEdit]) -> Text {
    // Defensive sort: callers might pass unsorted edits (e.g. in tests).
    // This is idempotent for already-sorted edits.
    let mut ordered = edits.to_vec();
    ordered.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
    let mut current = text.clone();
    for edit in &ordered {
        current = current.replace_range(edit.range, &edit.new_text);
    }
    current
}

/// Apply every per-document edit in a [`RenamePlan`] to the supplied
/// document map. The CLI uses this with a `HashMap<PathBuf, Text>` it
/// built from the workspace before applying the disk move.
pub fn apply_text_only(
    plan: &RenamePlan,
    documents: &mut std::collections::HashMap<std::path::PathBuf, Text>,
) {
    for document in plan.sorted_for_apply() {
        apply_document_edits(&document, documents);
    }
}

fn apply_document_edits(
    document: &DocumentEdit,
    documents: &mut std::collections::HashMap<std::path::PathBuf, Text>,
) {
    let Some(text) = documents.get_mut(&document.path) else {
        return;
    };
    *text = apply_text_edits(text, &document.edits);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{ByteRange, Text};

    #[test]
    fn single_edit_replaces_substring() {
        let text = Text::new("hello world");
        let edits = vec![TextEdit {
            range: ByteRange::new(6, 11),
            new_text: "downlint".into(),
        }];
        let result = apply_text_edits(&text, &edits);
        assert_eq!(result.as_str(), "hello downlint");
    }

    #[test]
    fn reverse_sorted_edits_apply_without_offset_drift() {
        let text = Text::new("aaa bbb ccc");
        // Two edits, intentionally given out-of-order — apply_text_edits
        // must sort them and produce the right result.
        let edits = vec![
            TextEdit {
                range: ByteRange::new(0, 3),
                new_text: "XXX".into(),
            },
            TextEdit {
                range: ByteRange::new(8, 11),
                new_text: "ZZZ".into(),
            },
        ];
        let result = apply_text_edits(&text, &edits);
        assert_eq!(result.as_str(), "XXX bbb ZZZ");
    }
}