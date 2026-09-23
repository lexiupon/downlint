//! `downlint link <query>` — read-only link queries (RFC 0018; formerly the
//! top-level `graph` command, RFC 0015).
//!
//! Projects the existing `ConnectionGraph` into navigation reports:
//! `link graph <FILE>` (incoming + outgoing), `link coverage` (orphans +
//! deadends), `link unresolved`. No new resolution logic and no new
//! diagnostics — a projection layer, like `link resolve` (RFC 0012).
//!
//! The graph is built *complete*: every document's links are resolved, not
//! just the linted ones (a mount with `lint = false` is targets-only for
//! `check`, but its links still matter for navigation).
//!
//! Output is `--format text` (default, sectioned) or `--format json` (a
//! single envelope per query). Exit codes are independent of format.

use crate::cli::check::OutputFormat;
use crate::parser::Ref;
use crate::resolution::conn::{DestinationKind, ResolvedDestination, ResolvedDocument};
use crate::resolution::{ConnectionGraph, ResolveInput, resolve_links};
use crate::utils::{PositionEncoding, Workspace, WorkspaceInput, discover_workspace};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The link queries. The clap surface lives in `crate::cli` (`LinkCommand`);
/// this enum is the logic-side dispatch.
#[derive(Clone, Debug)]
pub enum LinkQuery {
    /// Show the link graph around a note: incoming (backlinks) and outgoing
    /// links.
    Graph { file: PathBuf },
    /// Notes with no incoming links (orphans) and no outgoing links
    /// (deadends).
    Coverage,
    /// List links that point at notes that don't exist.
    Unresolved,
}

#[derive(Clone, Debug)]
pub struct LinkQueryOptions {
    pub root: Option<PathBuf>,
    pub query: LinkQuery,
    pub format: OutputFormat,
}

pub fn run_link_query(options: LinkQueryOptions) -> i32 {
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
    // link queries never run diagnostics, so no lint behavior is affected.
    for doc in &mut input.documents {
        doc.is_source = true;
    }
    let graph = resolve_links(input);

    match &options.query {
        LinkQuery::Graph { file } => {
            let (canonical, incoming) = match backlinks_rows(&graph, &workspace, file) {
                Ok(rows) => rows,
                Err(message) => {
                    eprintln!("downlint: error: {message}");
                    return 1;
                }
            };
            let outgoing = match links_rows(&graph, &workspace, file) {
                Ok((_, rows)) => rows,
                Err(message) => {
                    eprintln!("downlint: error: {message}");
                    return 1;
                }
            };
            match options.format {
                OutputFormat::Text => {
                    print_section("incoming", &incoming);
                    print_section("outgoing", &outgoing);
                }
                OutputFormat::Json => {
                    let body = json!({
                        "query": "graph",
                        "file": canonical,
                        "incoming": to_values(&incoming),
                        "outgoing": to_values(&outgoing),
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
                    );
                }
            }
            0
        }
        LinkQuery::Coverage => {
            let orphans = orphan_rows(&graph);
            let deadends = deadend_rows(&graph);
            match options.format {
                OutputFormat::Text => {
                    print_section("orphans", &orphans);
                    print_section("deadends", &deadends);
                }
                OutputFormat::Json => {
                    let body = json!({
                        "query": "coverage",
                        "orphans": to_values(&orphans),
                        "deadends": to_values(&deadends),
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
                    );
                }
            }
            if orphans.is_empty() && deadends.is_empty() {
                0
            } else {
                1
            }
        }
        LinkQuery::Unresolved => {
            let rows = unresolved_rows(&graph);
            emit("unresolved", None, options.format, &rows, true)
        }
    }
}

/// A query result row: serializable to JSON and renderable as a text line.
trait Row: Serialize {
    fn text(&self) -> String;
}

/// Render rows as text (one line each) or a JSON envelope, and return the
/// exit code. `found_is_error` selects the report convention (0 = none,
/// 1 = found) vs. the per-file convention (always 0 once the file is found).
fn emit<R: Row>(
    query: &str,
    file: Option<String>,
    format: OutputFormat,
    rows: &[R],
    found_is_error: bool,
) -> i32 {
    match format {
        OutputFormat::Text => {
            for row in rows {
                println!("{}", row.text());
            }
        }
        OutputFormat::Json => {
            let mut body = json!({ "query": query, "results": to_values(rows) });
            if let Some(file) = file {
                body["file"] = json!(file);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
            );
        }
    }
    if found_is_error && !rows.is_empty() {
        1
    } else {
        0
    }
}

/// Serialize rows for a JSON section (an array, `[]` when empty).
fn to_values<R: Serialize>(rows: &[R]) -> Vec<Value> {
    rows.iter()
        .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
        .collect()
}

/// Render a text section: a header with the row count, then one indented
/// line per row (header only when empty).
fn print_section<R: Row>(header: &str, rows: &[R]) {
    println!("{header} ({})", rows.len());
    for row in rows {
        println!("  {}", row.text());
    }
}

#[derive(Serialize)]
struct BacklinkRow {
    source: String,
    line: u32,
    col: u32,
}
impl Row for BacklinkRow {
    fn text(&self) -> String {
        format!("{}:{}:{}", self.source, self.line, self.col)
    }
}

#[derive(Serialize)]
struct LinkRow {
    line: u32,
    col: u32,
    target: String,
    /// `resolved` | `unresolved` | `ambiguous`.
    status: String,
    /// The destination path(s) for a resolved link; `null` otherwise.
    destination: Option<String>,
}
impl Row for LinkRow {
    fn text(&self) -> String {
        let shown = match self.status.as_str() {
            "resolved" => self.destination.clone().unwrap_or_default(),
            "unresolved" => "<unresolved>".to_string(),
            _ => "<ambiguous>".to_string(),
        };
        format!(
            "{}:{}  {}  \u{2192}  {}",
            self.line, self.col, self.target, shown
        )
    }
}

#[derive(Serialize)]
struct PathRow {
    path: String,
}
impl Row for PathRow {
    fn text(&self) -> String {
        self.path.clone()
    }
}

#[derive(Serialize)]
struct UnresolvedRow {
    source: String,
    line: u32,
    col: u32,
    target: String,
}
impl Row for UnresolvedRow {
    fn text(&self) -> String {
        format!(
            "{}:{}:{}  {}",
            self.source, self.line, self.col, self.target
        )
    }
}

/// Returns the canonical namespace path of `<FILE>` plus its backlink rows.
fn backlinks_rows(
    graph: &ConnectionGraph,
    workspace: &Workspace,
    file: &Path,
) -> Result<(String, Vec<BacklinkRow>), String> {
    let doc = find_document(graph, workspace, file)
        .ok_or_else(|| format!("document not found in workspace: {}", file.display()))?;
    let canonical = doc.namespace_rel_path.to_string_lossy().replace('\\', "/");
    let mut rows: Vec<BacklinkRow> = Vec::new();
    for reference in &graph.resolved_references {
        if !has_document_edge_to(reference, &doc.path) {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push(BacklinkRow {
            source: doc_display(graph, &reference.source_path),
            line,
            col,
        });
    }
    rows.sort_by(|a, b| {
        (a.source.as_str(), a.line, a.col).cmp(&(b.source.as_str(), b.line, b.col))
    });
    Ok((canonical, rows))
}

/// Returns the canonical namespace path of `<FILE>` plus its outgoing rows.
fn links_rows(
    graph: &ConnectionGraph,
    workspace: &Workspace,
    file: &Path,
) -> Result<(String, Vec<LinkRow>), String> {
    let doc = find_document(graph, workspace, file)
        .ok_or_else(|| format!("document not found in workspace: {}", file.display()))?;
    let canonical = doc.namespace_rel_path.to_string_lossy().replace('\\', "/");
    let root = &workspace.folder.root;
    let mut rows: Vec<LinkRow> = Vec::new();

    for reference in &graph.resolved_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        let destination = reference
            .destinations
            .iter()
            .map(|dest| destination_display(graph, root, dest))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(LinkRow {
            line,
            col,
            target: target_as_written(&reference.reference),
            status: "resolved".to_string(),
            destination: Some(destination),
        });
    }
    for reference in &graph.unresolved_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push(LinkRow {
            line,
            col,
            target: reference.target.clone(),
            status: "unresolved".to_string(),
            destination: None,
        });
    }
    for reference in &graph.ambiguous_references {
        if reference.source_path != doc.path {
            continue;
        }
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push(LinkRow {
            line,
            col,
            target: reference.target.clone(),
            status: "ambiguous".to_string(),
            destination: None,
        });
    }
    rows.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
    Ok((canonical, rows))
}

fn orphan_rows(graph: &ConnectionGraph) -> Vec<PathRow> {
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
        .map(|doc| PathRow {
            path: doc.namespace_rel_path.to_string_lossy().replace('\\', "/"),
        })
        .collect()
}

fn deadend_rows(graph: &ConnectionGraph) -> Vec<PathRow> {
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
        .map(|doc| PathRow {
            path: doc.namespace_rel_path.to_string_lossy().replace('\\', "/"),
        })
        .collect()
}

fn unresolved_rows(graph: &ConnectionGraph) -> Vec<UnresolvedRow> {
    let mut rows: Vec<UnresolvedRow> = Vec::new();
    for reference in &graph.unresolved_references {
        let (line, col) = line_col(graph, &reference.source_path, reference.full_range.start);
        rows.push(UnresolvedRow {
            source: doc_display(graph, &reference.source_path),
            line,
            col,
            target: reference.target.clone(),
        });
    }
    rows.sort_by(|a, b| {
        (a.source.as_str(), a.line, a.col, a.target.as_str()).cmp(&(
            b.source.as_str(),
            b.line,
            b.col,
            b.target.as_str(),
        ))
    });
    rows
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

    fn doc(path: &str, ns: &str) -> ResolvedDocument {
        ResolvedDocument {
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
            graph.documents.push(doc(path, ns));
        }
        graph.resolved_references.push(reference(
            "/a.md",
            vec![dest("/b.md", DestinationKind::Document)],
        ));

        let orphans: Vec<String> = orphan_rows(&graph).iter().map(|r| r.path.clone()).collect();
        assert_eq!(orphans, vec!["a.md", "c.md"]); // b.md is linked; a.md and c.md are not
        let deadends: Vec<String> = deadend_rows(&graph)
            .iter()
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(deadends, vec!["b.md", "c.md"]); // only a.md has an outgoing doc link
    }

    #[test]
    fn self_link_is_not_an_orphan() {
        let mut graph = ConnectionGraph::default();
        graph.documents.push(doc("/a.md", "a.md"));
        graph.resolved_references.push(reference(
            "/a.md",
            vec![dest("/a.md", DestinationKind::Document)],
        ));
        assert!(orphan_rows(&graph).is_empty()); // self-link counts as incoming
        assert!(deadend_rows(&graph).is_empty()); // self-link counts as outgoing
    }

    #[test]
    fn unresolved_rows_format() {
        let mut graph = ConnectionGraph::default();
        graph.documents.push(doc("/a.md", "a.md"));
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
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text(), "a.md:1:1  gone");
    }

    #[test]
    fn row_text_rendering() {
        assert_eq!(
            BacklinkRow {
                source: "a.md".into(),
                line: 2,
                col: 5,
            }
            .text(),
            "a.md:2:5"
        );
        assert_eq!(
            LinkRow {
                line: 3,
                col: 1,
                target: "T".into(),
                status: "resolved".into(),
                destination: Some("t.md".into()),
            }
            .text(),
            "3:1  T  \u{2192}  t.md"
        );
        assert_eq!(
            LinkRow {
                line: 3,
                col: 1,
                target: "T".into(),
                status: "unresolved".into(),
                destination: None,
            }
            .text(),
            "3:1  T  \u{2192}  <unresolved>"
        );
        assert_eq!(
            PathRow {
                path: "x.md".into()
            }
            .text(),
            "x.md"
        );
    }

    #[test]
    fn json_envelope_shape() {
        let incoming = vec![
            BacklinkRow {
                source: "a.md".into(),
                line: 2,
                col: 5,
            },
            BacklinkRow {
                source: "b.md".into(),
                line: 7,
                col: 1,
            },
        ];
        let outgoing = vec![LinkRow {
            line: 3,
            col: 1,
            target: "T".into(),
            status: "resolved".into(),
            destination: Some("t.md".into()),
        }];
        // Mimic run_link_query's JSON body construction for `link graph`.
        let body = json!({
            "query": "graph",
            "file": "t.md",
            "incoming": to_values(&incoming),
            "outgoing": to_values(&outgoing),
        });
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&body).unwrap()).unwrap();
        assert_eq!(value["query"], "graph");
        assert_eq!(value["file"], "t.md");
        assert_eq!(value["incoming"].as_array().unwrap().len(), 2);
        assert_eq!(value["incoming"][0]["source"], "a.md");
        assert_eq!(value["incoming"][0]["line"], 2);
        assert_eq!(value["incoming"][1]["col"], 1);
        assert_eq!(value["outgoing"].as_array().unwrap().len(), 1);
        assert_eq!(value["outgoing"][0]["destination"], "t.md");
    }

    #[test]
    fn json_envelope_empty_sections_are_empty_arrays() {
        let body = json!({
            "query": "coverage",
            "orphans": to_values::<PathRow>(&[]),
            "deadends": to_values::<PathRow>(&[]),
        });
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&body).unwrap()).unwrap();
        assert!(value["orphans"].as_array().unwrap().is_empty());
        assert!(value["deadends"].as_array().unwrap().is_empty());
    }
}
