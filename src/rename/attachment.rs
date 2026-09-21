//! LSP Type A — attachment rename — RFC 0009 §"Type A".
//!
//! Renames an attachment (image, PDF, XLSX, anything not markdown) and
//! rewrites every link that resolves to it. The CLI infers attachment vs
//! markdown from the source file's extension; the LSP code-action handler
//! does the same based on the resolution kind.
//!
//! ## Algorithm
//!
//! Mirrors `file::plan` but with the Attachment destination kind and a
//! relaxed extension-class rule (any non-markdown extension is allowed).
//!
//! ## Differences from Type F
//!
//! - Extension-class preservation is `attachment ↔ attachment` (not just
//!   markdown).
//! - **No prefix-collision check for attachments**: prefix resolution is
//!   a markdown-file feature; attachments always resolve by exact path.
//! - Reference label `[id]: url` rewriting is supported (URL portion only).
//! - Path rewriting is identical to Type F.

use crate::parser::cst::{CstElement, MdLink};
use crate::rename::conflict::{Conflict, ExtensionClass, extension_class};
use crate::rename::file::edits_for_reference;
use crate::rename::{FileMove, PlanInput, RenameError, RenamePlan};
use crate::resolution::conn::DestinationKind;
use crate::resolution::ConnectionGraph;
use std::path::PathBuf;

/// Plan an attachment rename. Same shape as `file::plan` with a relaxed
/// extension-class rule.
pub fn plan(input: PlanInput, graph: &ConnectionGraph) -> Result<RenamePlan, RenameError> {
    let from = input.source_path.join(&input.old);
    let to = input.source_path.join(&input.new);

    // 1. Source must exist.
    if !from.exists() {
        return Err(RenameError::SourceNotFound(from));
    }

    // 2. Extension class: source must be an attachment.
    let from_class = extension_class(&from, &default_markdown_extensions());
    if from_class.is_markdown() {
        return Err(RenameError::Conflict(Conflict::extension_class(format!(
            "rename-file on an attachment target; source has markdown extension. Use file rename instead."
        ))));
    }
    let to_class = extension_class(&to, &default_markdown_extensions());
    if to_class.is_markdown() {
        return Err(RenameError::Conflict(Conflict::extension_class(
            "attachments cannot be renamed to markdown files (extension class must be preserved)",
        )));
    }

    // 3. Exact-path collision.
    if to.exists() && to != from {
        return Err(RenameError::Conflict(Conflict::exact_path(
            to.clone(),
            format!("destination path already exists: {}", to.display()),
        )));
    }

    // 4. Find references resolving to `from` as Attachment destinations.
    let from_abs = absolute_path(&from);
    let mut edits_by_doc: std::collections::HashMap<PathBuf, Vec<_>> =
        std::collections::HashMap::new();

    for reference in &graph.resolved_references {
        let points_at_from = reference
            .destinations
            .iter()
            .any(|dest| dest.path == from_abs && matches!(dest.kind, DestinationKind::Attachment));
        if !points_at_from {
            continue;
        }
        let Some(document) = graph
            .documents
            .iter()
            .find(|doc| doc.path == reference.source_path)
        else {
            continue;
        };
        let edits = edits_for_reference(document, &from_abs, &to, graph);
        if !edits.is_empty() {
            edits_by_doc
                .entry(reference.source_path.clone())
                .or_default()
                .extend(edits);
        }
    }

    let edits = edits_by_doc
        .into_iter()
        .map(|(path, edits)| {
            crate::rename::DocumentEdit::new(path, edits)
        })
        .collect();
    let file_moves = if from_abs != to {
        vec![FileMove {
            from: from_abs,
            to,
        }]
    } else {
        vec![]
    };
    Ok(RenamePlan::new(edits, file_moves))
}

fn default_markdown_extensions() -> Vec<String> {
    vec!["md".into(), "markdown".into()]
}

fn absolute_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Compile-time assertion that the link/heading types we share with
/// `file::plan` via `edits_for_reference` keep working with the same CST
/// shape. If the parser ever changes the wiki-link / md-link types, this
/// will surface the breakage here too.
#[allow(dead_code)]
fn _types_compile() {
    fn _accept(_e: &CstElement, _l: &MdLink) {}
    let _ = ExtensionClass::Attachment;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParseOptions, parse_document};
    use crate::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
    use std::path::PathBuf;

    fn build_graph(
        texts: &[(&str, &str)],
        attachments: &[(&str, &[u8])],
    ) -> (ConnectionGraph, tempfile::TempDir) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let mut documents = Vec::new();
        for (rel, content) in texts {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            let structure = parse_document(content, ParseOptions::default());
            let rel_path = PathBuf::from(rel);
            documents.push(ResolveDocument::primary(
                path.clone(),
                rel_path,
                structure,
            ));
        }
        // Attachments live on disk too — the planner checks `from.exists()`.
        for (rel, bytes) in attachments {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let prefix = PrefixIndex::from_entries(
            documents.iter().map(|d| (d.stem(), d.path.clone())),
        );
        let input = ResolveInput {
            root: root.clone(),
            documents,
            mounts: vec![],
            conflicts: vec![],
            config: Default::default(),
            single_file: false,
            prefix_index: prefix,
            uri_resolver: crate::resolution::uri::UriResolver::empty(),
            uri_opts: crate::resolution::UriOptions::default(),
            uri_sync_cache: crate::resolution::UriSyncCache::new(),
            uri_error: None,
        };
        (resolve_links(input), temp)
    }

    /// Renaming `photo.png` updates every wiki-link that references it.
    #[test]
    fn rename_attachment() {
        let (graph, temp) = build_graph(
            &[("index.md", "see [[photo.png]]\n")],
            &[("photo.png", b"fake-png-bytes")],
        );
        let from = temp.path().join("photo.png");
        let to = temp.path().join("pic.png");
        let input = PlanInput {
            kind: crate::rename::RenameKind::Attachment,
            old: from.to_string_lossy().to_string(),
            new: to.to_string_lossy().to_string(),
            source_path: temp.path().to_path_buf(),
            cursor_offset: None,
        };
        let plan = plan(input, &graph).expect("rename should succeed");
        assert_eq!(plan.file_moves.len(), 1);
        let index_edits = plan
            .edits
            .iter()
            .find(|d| d.path == temp.path().join("index.md"))
            .expect("expected edits for index.md");
        assert_eq!(index_edits.edits.len(), 1);
    }

    /// Attachment → markdown rename is rejected by the extension-class
    /// preservation rule.
    #[test]
    fn rename_attachment_extension_class_rejected() {
        let (graph, temp) = build_graph(
            &[("index.md", "see [[photo.png]]\n")],
            &[("photo.png", b"fake-png-bytes")],
        );
        let from = temp.path().join("photo.png");
        let to = temp.path().join("photo.md");
        let input = PlanInput {
            kind: crate::rename::RenameKind::Attachment,
            old: from.to_string_lossy().to_string(),
            new: to.to_string_lossy().to_string(),
            source_path: temp.path().to_path_buf(),
            cursor_offset: None,
        };
        let err = plan(input, &graph).unwrap_err();
        assert!(matches!(err, RenameError::Conflict(_)), "got {err:?}");
    }
}