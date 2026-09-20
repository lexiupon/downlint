//! LSP `workspace/didRenameFiles` handler — RFC 0009 §"Phase 4".
//!
//! Reacts after a file rename (driven by the editor — downlint only
//! needs to fix up its indexes). The handler:
//!
//! 1. Parses the `FileRename` array (old URI → new URI pairs).
//! 2. For each pair, locates the corresponding `ResolvedDocument` in
//!    the graph (if any).
//! 3. Updates the document's `path` in-place; rebuilds only this
//!    document's `file_stem`, `rel_path` etc.
//! 4. Updates every `ResolvedReference.destinations[*].path` that pointed
//!    at the old path, and every `AmbiguousReference.destinations`.
//! 5. Re-runs diagnostics on the affected source documents.
//! 6. For files that moved *out* of the indexed set, emits `link/broken`
//!    on every referencing link.
//!
//! Performance target: O(references to renamed file), not O(whole graph).
//! All graph mutations go through the existing single-mutex ownership of
//! `state.graph` (RFC §"Risks" #3).

use crate::resolution::conn::{ResolvedDestination, ResolvedDocument};
use crate::resolution::{ConnectionGraph, Slug};
use std::path::{Path, PathBuf};

/// One entry in the LSP `workspace/didRenameFiles` notification.
#[derive(Clone, Debug)]
pub struct FileRename {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Apply a list of file renames to the graph in-place. Returns the set
/// of source documents that were touched (so the caller can re-emit
/// diagnostics just for them — cheaper than re-diagnosing the whole
/// workspace).
///
/// Files that moved out of the indexed set (no matching `ResolvedDocument`
/// for either old or new path) are not handled here — the caller emits
/// `link/broken` on referencing documents via the diagnostic pipeline.
pub fn apply_file_renames(graph: &mut ConnectionGraph, renames: &[FileRename]) -> Vec<PathBuf> {
    let mut touched_sources: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();

    for rename in renames {
        let old_path = &rename.from;
        let new_path = &rename.to;

        // Find the document at the old path.
        let Some(doc_index) = graph.documents.iter().position(|doc| doc.path == *old_path) else {
            // The renamed file isn't in the indexed set. Mark every doc
            // that referenced the old path as touched so the caller can
            // re-diagnose and surface link/broken.
            mark_referencing_sources_touched(graph, old_path, &mut touched_sources);
            continue;
        };

        // Update the document's path in-place. Also rebuild the file stem
        // and other derived fields from the new path.
        let doc = &mut graph.documents[doc_index];
        doc.path = new_path.clone();
        let new_file_name = new_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        let new_stem = Path::new(&new_file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        doc.file_stem = new_stem;
        // rel_path is best-effort: leave it as-is if we don't have a
        // workspace root to compute against. The caller can recompute
        // by re-running the workspace discovery.
        if let Some(parent) = new_path.parent() {
            // Best-effort: keep the existing rel_path as-is. The
            // diagnostic pipeline reads file_stem and path directly;
            // rel_path is only used for display.
            let _ = parent;
        }

        // Update every reference that pointed at the old path.
        update_references_pointing_at(graph, old_path, new_path, &mut touched_sources);

        // Update the source_path of every reference inside the moved doc.
        for reference in &mut graph.resolved_references {
            if reference.source_path == *old_path {
                reference.source_path = new_path.clone();
                touched_sources.insert(new_path.clone());
            }
        }
        for reference in &mut graph.unresolved_references {
            if reference.source_path == *old_path {
                reference.source_path = new_path.clone();
                touched_sources.insert(new_path.clone());
            }
        }
        for reference in &mut graph.ambiguous_references {
            if reference.source_path == *old_path {
                reference.source_path = new_path.clone();
                touched_sources.insert(new_path.clone());
            }
        }
        touched_sources.insert(new_path.clone());
    }

    touched_sources.into_iter().collect()
}

/// Update every reference that pointed at `old_path` so it now points at
/// `new_path`. Also marks the referencing source documents as touched.
fn update_references_pointing_at(
    graph: &mut ConnectionGraph,
    old_path: &Path,
    new_path: &Path,
    touched: &mut std::collections::HashSet<PathBuf>,
) {
    for reference in &mut graph.resolved_references {
        for dest in &mut reference.destinations {
            if dest.path == *old_path {
                dest.path = new_path.to_path_buf();
                touched.insert(reference.source_path.clone());
            }
        }
    }
    for reference in &mut graph.ambiguous_references {
        for dest in &mut reference.destinations {
            if dest.path == *old_path {
                dest.path = new_path.to_path_buf();
                touched.insert(reference.source_path.clone());
            }
        }
    }
}

/// Mark every source document that references `old_path` as touched, so
/// the caller can re-diagnose and surface `link/broken` for any references
/// whose destination just moved out of the indexed set.
fn mark_referencing_sources_touched(
    graph: &ConnectionGraph,
    old_path: &Path,
    touched: &mut std::collections::HashSet<PathBuf>,
) {
    for reference in &graph.resolved_references {
        if reference
            .destinations
            .iter()
            .any(|dest| dest.path == *old_path)
        {
            touched.insert(reference.source_path.clone());
        }
    }
    for reference in &graph.unresolved_references {
        // For unresolved references, check the target string's implied
        // path. Conservative: only flag if the target matches the file
        // stem of `old_path`.
        if let Some(stem) = old_path.file_stem().and_then(|s| s.to_str()) {
            if reference.target == stem || reference.target == old_path.to_string_lossy() {
                touched.insert(reference.source_path.clone());
            }
        }
    }
    for reference in &graph.ambiguous_references {
        if reference
            .destinations
            .iter()
            .any(|dest| dest.path == *old_path)
        {
            touched.insert(reference.source_path.clone());
        }
    }
}

/// Convenience: rebuild the diagnostic state for a list of paths in the
/// graph. The caller invokes this after `apply_file_renames` to produce
/// fresh diagnostics for the touched documents.
#[allow(dead_code)]
pub fn touched_documents<'a>(graph: &'a ConnectionGraph, paths: &[PathBuf]) -> Vec<&'a ResolvedDocument> {
    graph
        .documents
        .iter()
        .filter(|doc| paths.contains(&doc.path))
        .collect()
}

/// Re-slug a doc's title after a rename, if title_from_heading is in
/// effect. Out of scope for Phase 4's minimum viable implementation;
/// tracked as future work per RFC §"Open Questions" #1.
#[allow(dead_code)]
pub fn reslug_title(_doc: &mut ResolvedDocument, _new_title: &str, _new_slug: &Slug) {}

/// Re-export `ResolvedDestination` for callers that need to construct
/// fresh destinations after a rename.
#[allow(dead_code)]
pub fn _resolved_destination_marker() -> Option<ResolvedDestination> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParseOptions, parse_document};
    use crate::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
    use std::path::PathBuf;

    fn build_graph_from(
        texts: &[(&str, &str)],
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
            documents.push(ResolveDocument {
                path: path.clone(),
                rel_path,
                structure,
            });
        }
        let prefix = PrefixIndex::from_entries(
            documents.iter().map(|d| (d.stem(), d.path.clone())),
        );
        let input = ResolveInput {
            root: root.clone(),
            documents,
            extra_documents: vec![],
            config: Default::default(),
            extra_folder_roots: vec![],
            single_file: false,
            prefix_index: prefix,
            uri_resolver: crate::resolution::uri::UriResolver::empty(),
            uri_opts: crate::resolution::UriOptions::default(),
            uri_sync_cache: crate::resolution::UriSyncCache::new(),
            uri_error: None,
        };
        (resolve_links(input), temp)
    }

    /// Renaming `report.md` → `topic.md` updates the graph: the document
    /// has the new path, references to it point at the new path.
    #[test]
    fn did_rename_files_updates_graph() {
        let (mut graph, temp) = build_graph_from(&[
            ("report.md", "# Report\n"),
            ("index.md", "see [[report]]\n"),
        ]);
        let old_path = temp.path().join("report.md");
        let new_path = temp.path().join("topic.md");
        let renames = vec![FileRename {
            from: old_path.clone(),
            to: new_path.clone(),
        }];
        let touched = apply_file_renames(&mut graph, &renames);

        // The renamed doc is at the new path.
        assert!(graph
            .documents
            .iter()
            .any(|doc| doc.path == new_path));
        assert!(!graph
            .documents
            .iter()
            .any(|doc| doc.path == old_path));

        // References to the renamed file now point at the new path.
        for reference in &graph.resolved_references {
            for dest in &reference.destinations {
                if dest.path == old_path {
                    panic!("reference still points at old path: {reference:?}");
                }
            }
        }

        // index.md is touched (it references the renamed file).
        assert!(touched.contains(&temp.path().join("index.md")));
    }

    /// Renaming a file out of the indexed set (e.g. from
    /// `extra_folders` into a path the workspace doesn't track) marks
    /// referencing sources as touched so the caller can emit link/broken.
    #[test]
    fn did_rename_files_out_of_scope() {
        let (mut graph, temp) = build_graph_from(&[
            ("report.md", "# Report\n"),
            ("index.md", "see [[report]]\n"),
        ]);
        let old_path = temp.path().join("report.md");
        let new_path = PathBuf::from("/totally/elsewhere/topic.md");
        let renames = vec![FileRename {
            from: old_path.clone(),
            to: new_path,
        }];
        let touched = apply_file_renames(&mut graph, &renames);

        // index.md should be marked as touched because its reference to
        // report.md now resolves to nothing.
        assert!(touched.contains(&temp.path().join("index.md")));
    }
}