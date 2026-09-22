//! `downlint graph <query>` — read-only link-graph queries (RFC 0015).
//!
//! Projects the existing `ConnectionGraph` into navigation reports:
//! `backlinks`, `links`, `orphans`, `deadends`, `unresolved`. No new
//! resolution logic and no new diagnostics — a projection layer, like
//! `resolve` (RFC 0012).
//!
//! The graph is built *complete*: every document's links are resolved, not
//! just the linted ones (a mount with `lint = false` is targets-only for
//! `check`, but its links still matter for navigation).

use crate::parser::Ref;
use crate::resolution::conn::{DestinationKind, ResolvedDestination, ResolvedDocument};
use crate::resolution::{ConnectionGraph, ResolveInput, resolve_links};
use crate::utils::{PositionEncoding, Workspace, WorkspaceInput, discover_workspace};
use clap::Subcommand;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Subcommand, Clone, Debug)]
pub enum GraphQuery {
    /// List the notes that link to FILE (one line per link occurrence).
    Backlinks {
        /// The target note (workspace-relative or mount namespace path).
        file: PathBuf,
    },
    /// List FILE's outgoing links with their resolution status.
    Links {
        /// The source note (workspace-relative or mount namespace path).
        file: PathBuf,
    },
    /// List notes with no incoming links.
    Orphans,
    /// List notes with no outgoing links.
    Deadends,
    /// List links that point at notes that don't exist.
    Unresolved,
}

#[derive(Clone, Debug)]
pub struct GraphOptions {
    pub root: Option<PathBuf>,
    pub query: GraphQuery,
}

pub fn run_graph(options: GraphOptions) -> i32 {
    let workspace = match discover_workspace(
        WorkspaceInput::Path(PathBuf::from(".")),
        options.root.as_deref(),
    ) {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("downlint: error: {error}");
            return 2;
        }
    };
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(error) = input.uri_error.take() {
        eprintln!("downlint: error: {error}");
        return 2;
    }

    // Complete graph: see every document's links, not just linted ones.
    // `is_source` only gates which documents `resolve_links` resolves; the
    // graph command never runs diagnostics, so no lint behavior is affected.
    for doc in &mut input.documents {
        doc.is_source = true;
    }
    let graph = resolve_links(input);

    match &options.query {
        GraphQuery::Backlinks { file } => backlinks(&graph, &workspace, file),
        GraphQuery::Links { file } => links(&graph, &workspace, file),
        GraphQuery::Orphans => print_report(&orphan_paths(&graph)),
        GraphQuery::Deadends => print_report(&deadend_paths(&graph)),
        GraphQuery::Unresolved => print_report(&unresolved_rows(&graph)),
    }
}

/// Print one path/row per line; exit 0 when empty, 1 when anything was found
/// (so `downlint graph orphans || echo clean` works as a CI gate).
fn print_report(lines: &[String]) -> i32 {
    for line in lines {
        println!("{line}");
    }
    if lines.is_empty() { 0 } else { 1 }
}

fn backlinks(graph: &ConnectionGraph, workspace: &Workspace, file: &Path) -> i32 {
    let doc = match find_document(graph, workspace, file) {
        Some(doc) => doc,
        None => {
            eprintln!(
                "downlint: error: document not found in workspace: {}",
                file.display()
            );
            return 1;
        }
    };
    let mut rows: Vec<(String, u32, u32)> = Vec::new();
    for reference in &graph.resolved_references {
        if !has_document_edge_to(reference, &doc.path) {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push((doc_display(graph, &reference.source_path), line, col));
    }
    rows.sort();
    for (source, line, col) in &rows {
        println!("{source}:{line}:{col}");
    }
    0
}

fn links(graph: &ConnectionGraph, workspace: &Workspace, file: &Path) -> i32 {
    let doc = match find_document(graph, workspace, file) {
        Some(doc) => doc,
        None => {
            eprintln!(
                "downlint: error: document not found in workspace: {}",
                file.display()
            );
            return 1;
        }
    };
    let root = &workspace.folder.root;
    let mut rows: Vec<(u32, u32, String, String)> = Vec::new(); // (line, col, target, status)

    for reference in &graph.resolved_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        let status = reference
            .destinations
            .iter()
            .map(|dest| destination_display(graph, root, dest))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push((line, col, target_as_written(&reference.reference), status));
    }
    for reference in &graph.unresolved_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push((line, col, reference.target.clone(), "<unresolved>".into()));
    }
    for reference in &graph.ambiguous_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push((line, col, reference.target.clone(), "<ambiguous>".into()));
    }
    rows.sort();
    for (line, col, target, status) in &rows {
        println!("{line}:{col}  {target}  \u{2192}  {status}");
    }
    0
}

fn orphan_paths(graph: &ConnectionGraph) -> Vec<String> {
    let linked: HashSet<&Path> = graph
        .resolved_references
        .iter()
        .flat_map(|reference| reference.destinations.iter())
        .filter(|dest| is_document_edge(dest))
        .map(|dest| dest.path.as_path())
        .collect();
    graph
        .documents
        .iter()
        .filter(|doc| !linked.contains(doc.path.as_path()))
        .map(|doc| doc.namespace_rel_path.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn deadend_paths(graph: &ConnectionGraph) -> Vec<String> {
    let linking: HashSet<&Path> = graph
        .resolved_references
        .iter()
        .filter(|reference| reference.destinations.iter().any(is_document_edge))
        .map(|reference| reference.source_path.as_path())
        .collect();
    graph
        .documents
        .iter()
        .filter(|doc| !linking.contains(doc.path.as_path()))
        .map(|doc| doc.namespace_rel_path.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn unresolved_rows(graph: &ConnectionGraph) -> Vec<String> {
    let mut rows: Vec<(String, u32, u32, String)> = Vec::new();
    for reference in &graph.unresolved_references {
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push((
            doc_display(graph, &reference.source_path),
            line,
            col,
            reference.target.clone(),
        ));
    }
    rows.sort();
    rows.into_iter()
        .map(|(source, line, col, target)| format!("{source}:{line}:{col}  {target}"))
        .collect()
}

/// A document edge points at a note: the destination is an indexed document,
/// reached directly (`Document`) or via a heading (`Heading`). Attachments,
/// folders, tags, and link definitions are not note links.
fn is_document_edge(dest: &ResolvedDestination) -> bool {
    matches!(
        dest.kind,
        DestinationKind::Document | DestinationKind::Heading
    )
}

fn has_document_edge_to(
    reference: &crate::resolution::conn::ResolvedReference,
    target: &Path,
) -> bool {
    reference
        .destinations
        .iter()
        .any(|dest| dest.path == target && is_document_edge(dest))
}

/// Match `<FILE>` to an indexed document: by filesystem path (absolute, or
/// joined to the workspace root) or by namespace path (case-insensitive) —
/// the same rules as `resolve --from`.
fn find_document<'a>(
    graph: &'a ConnectionGraph,
    workspace: &Workspace,
    file: &Path,
) -> Option<&'a ResolvedDocument> {
    let file_abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        workspace.folder.root.join(file)
    };
    let file_ns = file.to_string_lossy().replace('\\', "/");
    graph.documents.iter().find(|doc| {
        doc.path == file_abs
            || doc
                .namespace_rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .eq_ignore_ascii_case(&file_ns)
    })
}

/// 1-based `line:col` for a byte offset in the source document, via the same
/// helper `check` uses (`Text::to_lsp_position`).
fn line_col(graph: &ConnectionGraph, source_path: &Path, offset: usize) -> (u32, u32) {
    graph
        .document_for_path(&source_path.to_path_buf())
        .and_then(|doc| {
            doc.structure
                .text
                .to_lsp_position(offset, PositionEncoding::Utf8)
                .ok()
        })
        .map(|position| (position.line + 1, position.character + 1))
        .unwrap_or((1, 1))
}

/// A document's address: its namespace path (workspace-relative for primary
/// docs, `as/rel` for mounted docs).
fn doc_display(graph: &ConnectionGraph, path: &Path) -> String {
    graph
        .document_for_path(&path.to_path_buf())
        .map(|doc| doc.namespace_rel_path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
}

/// A destination's display path: a document's namespace path, a tag's `#name`,
/// or (attachments, folders) the workspace-relative path.
fn destination_display(graph: &ConnectionGraph, root: &Path, dest: &ResolvedDestination) -> String {
    match dest.kind {
        DestinationKind::Tag => format!("#{}", dest.name),
        _ => graph
            .document_for_path(&dest.path.to_path_buf())
            .map(|doc| doc.namespace_rel_path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| {
                dest.path
                    .strip_prefix(root)
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| dest.path.to_string_lossy().replace('\\', "/"))
            }),
    }
}

/// The target as written at the link site: the wiki/inline target, or the
/// reference-style label.
fn target_as_written(reference: &Ref) -> String {
    match reference {
        Ref::Wiki { target, .. } => target.clone(),
        Ref::Inline { target, .. } => target.clone(),
        Ref::Full { label, .. } | Ref::Collapsed { label } | Ref::Shortcut { label } => {
            label.0.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::LinkLabel;
    use crate::resolution::conn::{ResolvedReference, UnresolvedReference};
    use crate::utils::ByteRange;

    fn dest(path: &str, kind: DestinationKind) -> ResolvedDestination {
        ResolvedDestination {
            path: PathBuf::from(path),
            kind,
            name: String::new(),
            range: None,
        }
    }

    fn reference(source: &str, destinations: Vec<ResolvedDestination>) -> ResolvedReference {
        ResolvedReference {
            source_path: PathBuf::from(source),
            occurrence_id: 1,
            full_range: ByteRange::new(0, 0),
            name_range: None,
            reference: Ref::Wiki {
                target: "x".into(),
                heading: None,
                is_embed: false,
            },
            destinations,
        }
    }

    #[test]
    fn document_edge_kinds() {
        assert!(is_document_edge(&dest("/a.md", DestinationKind::Document)));
        assert!(is_document_edge(&dest("/a.md", DestinationKind::Heading)));
        assert!(!is_document_edge(&dest(
            "/a.png",
            DestinationKind::Attachment
        )));
        assert!(!is_document_edge(&dest("/dir", DestinationKind::Directory)));
        assert!(!is_document_edge(&dest("/a.md", DestinationKind::Tag)));
        assert!(!is_document_edge(&dest(
            "/a.md",
            DestinationKind::LinkDefinition
        )));
    }

    #[test]
    fn heading_anchored_link_is_an_edge() {
        let reference = reference(
            "/src.md",
            vec![dest("/target.md", DestinationKind::Heading)],
        );
        assert!(has_document_edge_to(&reference, Path::new("/target.md")));
        assert!(!has_document_edge_to(&reference, Path::new("/other.md")));
    }

    #[test]
    fn attachment_is_not_an_edge() {
        let reference = reference("/src.md", vec![dest("/a.png", DestinationKind::Attachment)]);
        assert!(!has_document_edge_to(&reference, Path::new("/a.png")));
    }

    #[test]
    fn target_written_from_ref_kinds() {
        assert_eq!(
            target_as_written(&Ref::Wiki {
                target: "Note#H".into(),
                heading: Some("H".into()),
                is_embed: false,
            }),
            "Note#H"
        );
        assert_eq!(
            target_as_written(&Ref::Inline {
                target: "a/b.md".into(),
                anchor: None,
                is_image: false,
            }),
            "a/b.md"
        );
        assert_eq!(
            target_as_written(&Ref::Shortcut {
                label: LinkLabel("lbl".into()),
            }),
            "lbl"
        );
    }

    #[test]
    fn orphan_and_deadend_computation() {
        // a.md links to b.md; c.md links nowhere; b.md links nowhere.
        let mut graph = ConnectionGraph::default();
        for (path, ns) in [("/a.md", "a.md"), ("/b.md", "b.md"), ("/c.md", "c.md")] {
            graph.documents.push(ResolvedDocument {
                path: PathBuf::from(path),
                rel_path: PathBuf::from(ns),
                structure: crate::parser::parse_document("", Default::default()),
                title_slug: Default::default(),
                title_text: String::new(),
                file_stem: String::new(),
                link_defs: Default::default(),
                headings: Default::default(),
                tags: Default::default(),
                namespace_rel_path: PathBuf::from(ns),
                mount: None,
                is_source: true,
            });
        }
        graph.resolved_references.push(reference(
            "/a.md",
            vec![dest("/b.md", DestinationKind::Document)],
        ));

        let orphans = orphan_paths(&graph);
        assert_eq!(orphans, vec!["a.md", "c.md"]); // b.md is linked; a.md and c.md are not
        let deadends = deadend_paths(&graph);
        assert_eq!(deadends, vec!["b.md", "c.md"]); // only a.md has an outgoing doc link
    }

    #[test]
    fn self_link_is_not_an_orphan() {
        let mut graph = ConnectionGraph::default();
        graph.documents.push(ResolvedDocument {
            path: PathBuf::from("/a.md"),
            rel_path: PathBuf::from("a.md"),
            structure: crate::parser::parse_document("", Default::default()),
            title_slug: Default::default(),
            title_text: String::new(),
            file_stem: String::new(),
            link_defs: Default::default(),
            headings: Default::default(),
            tags: Default::default(),
            namespace_rel_path: PathBuf::from("a.md"),
            mount: None,
            is_source: true,
        });
        graph.resolved_references.push(reference(
            "/a.md",
            vec![dest("/a.md", DestinationKind::Document)],
        ));
        assert!(orphan_paths(&graph).is_empty()); // self-link counts as incoming
        assert!(deadend_paths(&graph).is_empty()); // self-link counts as outgoing
    }

    #[test]
    fn unresolved_rows_format() {
        let mut graph = ConnectionGraph::default();
        graph.documents.push(ResolvedDocument {
            path: PathBuf::from("/a.md"),
            rel_path: PathBuf::from("a.md"),
            structure: crate::parser::parse_document("", Default::default()),
            title_slug: Default::default(),
            title_text: String::new(),
            file_stem: String::new(),
            link_defs: Default::default(),
            headings: Default::default(),
            tags: Default::default(),
            namespace_rel_path: PathBuf::from("a.md"),
            mount: None,
            is_source: true,
        });
        graph.unresolved_references.push(UnresolvedReference {
            source_path: PathBuf::from("/a.md"),
            occurrence_id: 1,
            full_range: ByteRange::new(0, 0),
            name_range: None,
            reference: Ref::Wiki {
                target: "gone".into(),
                heading: None,
                is_embed: false,
            },
            target: "gone".into(),
            is_anchor: false,
            hint_payload: None,
            uri_no_mapping_hint: false,
        });
        let rows = unresolved_rows(&graph);
        assert_eq!(rows, vec!["a.md:1:1  gone".to_string()]);
    }
}
