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
//! - Edits are selected per resolved occurrence, never by document or basename.
//! - Wiki attachment paths keep explicit path syntax and file extensions.

use crate::parser::ast::AstElement;
use crate::parser::{MdLink, SymKind};
use crate::rename::conflict::{Conflict, extension_class};
use crate::rename::{DocumentEdit, FileMove, PlanInput, RenameError, RenamePlan, TextEdit};
use crate::resolution::ConnectionGraph;
use crate::resolution::conn::{DestinationKind, ResolvedDocument, ResolvedReference};
use crate::resolution::path::{
    is_root_relative, normalize_path, percent_decode, resolve_explicit_path,
};
use crate::utils::ByteRange;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Plan an attachment rename. Same shape as `file::plan` with a relaxed
/// extension-class rule.
pub fn plan(input: PlanInput, graph: &ConnectionGraph) -> Result<RenamePlan, RenameError> {
    let root = absolute_path(&input.source_path);
    let from = absolute_path(&input.source_path.join(&input.old));
    let to = absolute_path(&input.source_path.join(&input.new));

    // 1. Source must exist.
    if !from.exists() {
        return Err(RenameError::SourceNotFound(from));
    }

    // 2. Extension class: source must be an attachment.
    let default_extensions = default_markdown_extensions();
    let markdown_extensions = if graph.markdown_extensions.is_empty() {
        &default_extensions
    } else {
        &graph.markdown_extensions
    };
    let from_class = extension_class(&from, markdown_extensions);
    if from_class.is_markdown() {
        return Err(RenameError::Conflict(Conflict::extension_class(
            "rename-file on an attachment target; source has markdown extension. Use file rename instead.",
        )));
    }
    let to_class = extension_class(&to, markdown_extensions);
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

    // 4. A resolved occurrence, not its containing document, is the proof
    // that an inline/wiki reference targets this attachment. Ambiguous and
    // unresolved occurrences never enter this selection.
    let mut edits_by_doc: HashMap<PathBuf, Vec<TextEdit>> = HashMap::new();
    for reference in &graph.resolved_references {
        if reference.destinations.len() != 1
            || !reference.destinations.iter().all(|dest| {
                absolute_path(&dest.path) == from
                    && matches!(dest.kind, DestinationKind::Attachment)
            })
        {
            continue;
        }
        let Some(document) = graph.document_for_path(&reference.source_path) else {
            continue;
        };
        if let Some(edit) = occurrence_edit(document, reference, &to, &root) {
            if matches!(reference.reference, crate::parser::Ref::Wiki { .. }) {
                verify_destination(&edit.new_text, document, &to, &root, graph, true)?;
            } else {
                verify_destination(&edit.new_text, document, &to, &root, graph, false)?;
            }
            edits_by_doc
                .entry(document.path.clone())
                .or_default()
                .push(edit);
        }
    }

    // Definition usages resolve to labels in the graph, not to files. Check
    // every source document independently, using exact Markdown path rules.
    // Use the graph's routing context, including configured non-// prefixes.
    // Hand-built graphs without context retain RFC 0021's built-in routing.
    let empty_resolver = crate::resolution::uri::UriResolver::empty();
    let uri_resolver = graph.uri_resolver.as_ref().unwrap_or(&empty_resolver);
    for document in graph.documents.iter().filter(|doc| doc.is_source) {
        for element in &document.structure.ast.elements {
            let AstElement::MLD(definition) = element else {
                continue;
            };
            let url = &definition.url;
            let path = url_path(&url.raw);
            if path.is_empty() || uri_resolver.is_uri_target(path) || path.ends_with('/') {
                continue;
            }
            let source_dir = document.path.parent().unwrap_or(&root);
            if absolute_path(&resolve_explicit_path(&root, source_dir, path, false)) != from {
                continue;
            }
            let edit = path_edit(path, url.range.start, document, &to, &root, false);
            verify_destination(&edit.new_text, document, &to, &root, graph, false)?;
            edits_by_doc
                .entry(document.path.clone())
                .or_default()
                .push(edit);
        }
    }

    let mut edits: Vec<_> = edits_by_doc
        .into_iter()
        .map(|(path, mut edits)| {
            // Defensive deduplication also handles duplicate graph edges without
            // ever producing overlapping edits for a single occurrence.
            let mut seen = HashSet::new();
            edits.retain(|edit| seen.insert((edit.range.start, edit.range.end)));
            DocumentEdit::new(path, edits)
        })
        .collect();
    edits.sort_by(|a, b| a.path.cmp(&b.path));
    let file_moves = if from != to {
        vec![FileMove { from, to }]
    } else {
        vec![]
    };
    Ok(RenamePlan::new(edits, file_moves))
}

fn default_markdown_extensions() -> Vec<String> {
    vec!["md".into(), "markdown".into()]
}

fn absolute_path(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&std::env::current_dir().unwrap_or_default().join(path))
    };
    // Canonicalize directory aliases (e.g. /var vs /private/var on macOS),
    // including parents of a not-yet-existing destination. Do not dereference
    // the final filename: a symlink attachment is itself the moved entry.
    let mut ancestor = path.parent().unwrap_or(&path);
    loop {
        if let Ok(base) = ancestor.canonicalize() {
            return normalize_path(&base.join(path.strip_prefix(ancestor).unwrap_or(&path)));
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return path,
        }
    }
}

fn occurrence_edit(
    document: &ResolvedDocument,
    reference: &ResolvedReference,
    to: &Path,
    root: &Path,
) -> Option<TextEdit> {
    // Symbol IDs are AST index + 1; scanner CST IDs are a different namespace.
    // Frontmatter is not in the AST and headings get scanner IDs first. Use
    // the symbol's AST pointer and source ranges, not a CST node ID/index.
    let symbol = document.structure.symbols.iter().find(|symbol| {
        symbol.id == reference.occurrence_id
            && symbol.full_range == reference.full_range
            && matches!(&symbol.kind, SymKind::Ref(kind) if kind == &reference.reference)
    })?;
    let element = document.structure.ast.elements.get(symbol.ast_idx?)?;
    match element {
        AstElement::WL(link) => {
            let doc = link.doc.as_ref()?;
            Some(path_edit(
                &doc.raw,
                link.doc_range?.start,
                document,
                to,
                root,
                true,
            ))
        }
        AstElement::ML(MdLink::Inline { dest, .. }) => Some(path_edit(
            url_path(&dest.text),
            dest.range.start,
            document,
            to,
            root,
            false,
        )),
        _ => None,
    }
}

/// Replace only the path bytes; fragments (including their encoding), titles,
/// aliases and definition labels are left byte-for-byte intact.
fn path_edit(
    original: &str,
    start: usize,
    document: &ResolvedDocument,
    to: &Path,
    root: &Path,
    is_wiki: bool,
) -> TextEdit {
    let decoded = percent_decode(original);
    let source_dir = document.path.parent().unwrap_or(root);
    let root_relative = is_root_relative(&decoded, is_wiki);
    let base = if root_relative { root } else { source_dir };
    let mut target = relative_path(to, base);
    if decoded.starts_with('/') {
        target.insert(0, '/');
    } else if is_wiki {
        if root_relative {
            // A root-based wiki path moved to a root basename must retain
            // its base (and must not turn into a bare note identifier).
            if !target.contains('/') || target.starts_with("../") {
                target.insert(0, '/');
            }
        } else if !target.starts_with("../")
            && (decoded.starts_with("./")
                || target.contains('/')
                || !crate::resolution::is_explicit_path(&target))
        {
            // Keep bare file-like basenames (image.png -> topic.png). A path
            // with separators otherwise switches to root-relative semantics;
            // an extensionless basename switches to note matching.
            target.insert_str(0, "./");
        }
    }
    TextEdit {
        range: ByteRange::new(start, start + original.len()),
        new_text: encode_path(&target, !is_wiki || original.contains('%')),
    }
}

/// Document matching has priority over filesystem attachment fallback. Wiki
/// './topic' can match topic.md or a document title; even Markdown exact paths
/// match indexed documents case-insensitively. The move changes no indexed
/// documents, so this graph's document set is also the post-move set. Refuse
/// the entire plan if an edit would silently change its destination kind.
fn verify_destination(
    replacement: &str,
    document: &ResolvedDocument,
    to: &Path,
    root: &Path,
    graph: &ConnectionGraph,
    is_wiki: bool,
) -> Result<(), RenameError> {
    // Wiki symbols decode the authored doc part before document matching;
    // inline matching receives the raw destination.
    let target = if is_wiki {
        percent_decode(replacement)
    } else {
        replacement.to_string()
    };
    let source_dir = absolute_path(document.path.parent().unwrap_or(root));
    let source_dir = source_dir.as_path();
    let empty_resolver = crate::resolution::uri::UriResolver::empty();
    if graph
        .uri_resolver
        .as_ref()
        .unwrap_or(&empty_resolver)
        .is_uri_target(&target)
    {
        return Err(RenameError::Conflict(Conflict::exact_path(
            to.to_path_buf(),
            format!(
                "cannot preserve attachment destination: rewritten target '{replacement}' in {} is routed as a URI",
                document.path.display()
            ),
        )));
    }
    for candidate in &graph.documents {
        let mut canonical_candidate = candidate.clone();
        canonical_candidate.path = absolute_path(&candidate.path);
        if !crate::resolution::query::match_document_kinds(
            &canonical_candidate,
            source_dir,
            root,
            &target,
            !is_wiki || crate::resolution::is_explicit_path(&target),
            is_wiki,
        )
        .is_empty()
        {
            return Err(RenameError::Conflict(Conflict::exact_path(
                candidate.path.clone(),
                format!(
                    "cannot preserve attachment destination in {}: rewritten target '{}' matches Markdown document {} instead of moved attachment {}",
                    document.path.display(),
                    replacement,
                    candidate.path.display(),
                    to.display(),
                ),
            )));
        }
    }
    if absolute_path(&resolve_explicit_path(root, source_dir, &target, is_wiki)) != to {
        return Err(RenameError::Conflict(Conflict::exact_path(
            to.to_path_buf(),
            format!(
                "cannot preserve attachment destination for rewritten target '{replacement}' in {}",
                document.path.display()
            ),
        )));
    }
    Ok(())
}

fn url_path(url: &str) -> &str {
    // '#' is the local resolver's fragment delimiter. Query strings are not
    // separately interpreted by local attachment resolution.
    url.split_once('#').map_or(url, |(path, _)| path)
}

fn relative_path(target: &Path, base: &Path) -> String {
    let target = absolute_path(target);
    let base = absolute_path(base);
    let target: Vec<_> = target.components().collect();
    let base: Vec<_> = base.components().collect();
    let common = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); base.len() - common];
    parts.extend(
        target[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

fn encode_path(path: &str, encode_unicode: bool) -> String {
    let mut encoded = String::new();
    for ch in path.chars() {
        let safe = ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_' | '~');
        // Wiki paths may use literal spaces and Unicode, but their structural
        // delimiters and '%' must still be escaped. Markdown URLs are encoded.
        if safe || (!encode_unicode && (ch == ' ' || !ch.is_ascii())) {
            encoded.push(ch);
        } else {
            let mut bytes = [0; 4];
            for byte in ch.encode_utf8(&mut bytes).as_bytes() {
                use std::fmt::Write;
                write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
            }
        }
    }
    encoded
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
            documents.push(ResolveDocument::primary(path.clone(), rel_path, structure));
        }
        // Attachments live on disk too — the planner checks `from.exists()`.
        for (rel, bytes) in attachments {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let prefix =
            PrefixIndex::from_entries(documents.iter().map(|d| (d.stem(), d.path.clone())));
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
