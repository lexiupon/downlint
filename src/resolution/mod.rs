pub mod conn;
pub mod path;
pub mod prefix;
pub mod slug;

use crate::config::Config;
use crate::parser::{Def, LinkLabel, Ref, Structure, SymKind};
use crate::resolution::conn::{
    AmbiguousReference, DestinationKind, ResolvedDestination, ResolvedDocument, ResolvedReference,
    UnresolvedReference,
};
use crate::resolution::path::{has_scheme, is_folder_link_target, path_without_extension, resolve_explicit_path};
use crate::resolution::prefix::PrefixIndex;
use crate::utils::{Workspace, WorkspaceMode};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub use conn::{
    ConnectionGraph, DestinationKind as ResolveDestinationKind,
    ResolvedDestination as ResolveDestination,
};
pub use slug::Slug;

#[derive(Clone, Debug)]
pub struct ResolveDocument {
    pub path: PathBuf,
    pub rel_path: PathBuf,
    pub structure: Structure,
}

impl ResolveDocument {
    /// File stem: filename without its extension. Returns the empty string for paths
    /// without a usable stem component. Used by the prefix index.
    pub fn stem(&self) -> String {
        self.path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(|value| value.to_string())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct ResolveInput {
    pub root: PathBuf,
    pub documents: Vec<ResolveDocument>,
    pub extra_documents: Vec<ResolveDocument>,
    pub extra_folder_roots: Vec<PathBuf>,
    pub config: Config,
    pub single_file: bool,
    /// Case-insensitive prefix index over the stems of `documents` and `extra_documents`.
    /// Built eagerly in `from_workspace` (and constructed manually by tests).
    pub prefix_index: PrefixIndex,
}

impl ResolveInput {
    pub fn from_workspace(workspace: &Workspace) -> Self {
        let documents = workspace
            .folder
            .documents
            .iter()
            .map(|doc| ResolveDocument {
                path: doc.path.clone(),
                rel_path: doc.rel_path.clone(),
                structure: crate::parser::parse_document(
                    doc.text.as_str(),
                    crate::parser::ParseOptions {
                        title_from_heading: workspace.config.core.title_from_heading,
                        heading_ids: workspace.config.core.heading_ids.enable,
                    },
                ),
            })
            .collect::<Vec<_>>();

        let extra_documents =
            load_extra_documents(&workspace.folder.extra_folders, &workspace.config);

        let prefix_index = PrefixIndex::from_entries(
            documents
                .iter()
                .map(|doc| (doc.stem(), doc.path.clone()))
                .chain(
                    extra_documents
                        .iter()
                        .map(|doc| (doc.stem(), doc.path.clone())),
                ),
        );

        Self {
            root: workspace.folder.root.clone(),
            documents,
            extra_documents,
            extra_folder_roots: workspace.folder.extra_folders.clone(),
            config: workspace.config.clone(),
            single_file: matches!(workspace.mode, WorkspaceMode::SingleFile),
            prefix_index,
        }
    }
}

pub fn resolve_links(input: ResolveInput) -> ConnectionGraph {
    let primary_documents = input
        .documents
        .iter()
        .map(|doc| index_document(doc, &input.root))
        .collect::<Vec<_>>();
    let extra_documents = input
        .extra_documents
        .iter()
        .map(|doc| index_document(doc, &input.root))
        .collect::<Vec<_>>();

    let mut graph = ConnectionGraph {
        documents: primary_documents.clone(),
        ..ConnectionGraph::default()
    };

    for doc in &primary_documents {
        resolve_document(
            doc,
            &primary_documents,
            &extra_documents,
            &input,
            &mut graph,
        );
    }

    graph
}

fn index_document(doc: &ResolveDocument, _root: &Path) -> ResolvedDocument {
    let file_stem = doc
        .path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    let title_text = doc
        .structure
        .index
        .titles
        .first()
        .map(|heading| heading.title.text.clone())
        .unwrap_or_else(|| file_stem.clone());
    let title_slug = Slug::from_heading_text(&title_text);

    let mut link_defs: HashMap<LinkLabel, Vec<ResolvedDestination>> = HashMap::new();
    let mut headings: HashMap<Slug, Vec<ResolvedDestination>> = HashMap::new();
    let mut tags: HashMap<String, Vec<ResolvedDestination>> = HashMap::new();

    for symbol in &doc.structure.symbols {
        match &symbol.kind {
            SymKind::Def(Def::Title(name)) => {
                headings
                    .entry(Slug::from_heading_text(name))
                    .or_default()
                    .push(ResolvedDestination {
                        path: doc.path.clone(),
                        kind: DestinationKind::Heading,
                        name: name.clone(),
                        range: symbol.name_range,
                    });
            }
            SymKind::Def(Def::Header(_, slug)) => {
                headings
                    .entry(slug.clone())
                    .or_default()
                    .push(ResolvedDestination {
                        path: doc.path.clone(),
                        kind: DestinationKind::Heading,
                        name: slug.as_str().to_string(),
                        range: symbol.name_range,
                    });
            }
            SymKind::Def(Def::LinkDef(label)) => {
                link_defs
                    .entry(label.clone())
                    .or_default()
                    .push(ResolvedDestination {
                        path: doc.path.clone(),
                        kind: DestinationKind::LinkDefinition,
                        name: label.0.clone(),
                        range: symbol.name_range,
                    });
            }
            SymKind::Tag(tag) => {
                tags.entry(tag.name.clone())
                    .or_default()
                    .push(ResolvedDestination {
                        path: doc.path.clone(),
                        kind: DestinationKind::Tag,
                        name: tag.name.clone(),
                        range: symbol.name_range,
                    });
            }
            _ => {}
        }
    }

    ResolvedDocument {
        path: doc.path.clone(),
        rel_path: doc.rel_path.clone(),
        structure: doc.structure.clone(),
        title_slug,
        title_text,
        file_stem,
        link_defs,
        headings,
        tags,
    }
}

fn resolve_document(
    doc: &ResolvedDocument,
    primary_docs: &[ResolvedDocument],
    extra_docs: &[ResolvedDocument],
    input: &ResolveInput,
    graph: &mut ConnectionGraph,
) {
    for symbol in &doc.structure.symbols {
        let SymKind::Ref(reference) = &symbol.kind else {
            continue;
        };

        match reference {
            Ref::Wiki {
                target,
                heading,
                is_embed: _,
            } => {
                let mut ctx = ResolveRefContext {
                    doc,
                    symbol,
                    reference,
                    input,
                    graph,
                };
                resolve_wiki_ref(
                    &mut ctx,
                    target,
                    heading.as_deref(),
                    primary_docs,
                    extra_docs,
                );
            }
            Ref::Inline {
                target,
                anchor,
                is_image: _,
            } => {
                let mut ctx = ResolveRefContext {
                    doc,
                    symbol,
                    reference,
                    input,
                    graph,
                };
                resolve_inline_ref(&mut ctx, target, anchor.as_deref(), primary_docs);
            }
            Ref::Full { label, .. } | Ref::Collapsed { label } => {
                if let Some(destinations) = doc.link_defs.get(label).cloned() {
                    graph.resolved_references.push(ResolvedReference {
                        source_path: doc.path.clone(),
                        occurrence_id: symbol.id,
                        full_range: symbol.full_range,
                        name_range: symbol.name_range,
                        reference: reference.clone(),
                        destinations,
                    });
                } else {
                    graph.unresolved_references.push(UnresolvedReference {
                        source_path: doc.path.clone(),
                        occurrence_id: symbol.id,
                        full_range: symbol.full_range,
                        name_range: symbol.name_range,
                        reference: reference.clone(),
                        target: label.0.clone(),
                        hint_payload: None,
                    });
                }
            }
            Ref::Shortcut { .. } => {}
        }
    }
}

struct ResolveRefContext<'a> {
    doc: &'a ResolvedDocument,
    symbol: &'a crate::parser::symbols::SymbolOccurrence,
    reference: &'a Ref,
    input: &'a ResolveInput,
    graph: &'a mut ConnectionGraph,
}


/// Resolve a folder link (target ending with /) against the workspace and extra folders.
fn resolve_folder_link(
    source_path: &Path,
    target: &str,
    root: &Path,
    extra_roots: &[PathBuf],
) -> Option<ResolvedDestination> {
    // Strip anchor and trailing / to get the actual directory path
    let (path_part, _anchor) = crate::resolution::path::split_anchor(target);
    let target_dir = path_part.trim_end_matches('/');
    
    let source_dir = source_path.parent().unwrap_or(root);

    // For absolute paths (starting with /), resolve from root
    let resolved = resolve_explicit_path(root, source_dir, target_dir);
    if resolved.is_dir() {
        return Some(ResolvedDestination {
            path: resolved,
            kind: DestinationKind::Directory,
            name: target_dir.to_string(),
            range: None,
        });
    }

    // Check in extra folder roots for absolute paths, matching by prefix
    if target_dir.starts_with('/') {
        // Strip leading / to get path components
        let rel = target_dir.trim_start_matches('/');
        for extra_root in extra_roots {
            // Try direct join first
            let candidate = extra_root.join(rel);
            if candidate.is_dir() {
                return Some(ResolvedDestination {
                    path: candidate,
                    kind: DestinationKind::Directory,
                    name: target_dir.to_string(),
                    range: None,
                });
            }
            // Try matching the first path component against the extra folder name
            // e.g. /avon/cases/ -> extra_root ~/projects/avon -> ~/projects/avon/cases/
            if let Some(first) = rel.split('/').next() {
                if let Some(name) = extra_root.file_name().and_then(|n| n.to_str()) {
                    if first == name {
                        // Strip the first component and join
                        let rest = rel.splitn(2, '/').nth(1).unwrap_or("");
                        let candidate = extra_root.join(rest);
                        if candidate.is_dir() {
                            return Some(ResolvedDestination {
                                path: candidate,
                                kind: DestinationKind::Directory,
                                name: target_dir.to_string(),
                                range: None,
                            });
                        }
                    }
                }
            }
        }
    }

    None
}

fn resolve_wiki_ref(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    heading: Option<&str>,
    primary_docs: &[ResolvedDocument],
    extra_docs: &[ResolvedDocument],
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if target.is_empty()
        && let Some(anchor) = heading
    {
        let slug = Slug::from_heading_text(anchor);
        let destinations = doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        if let Some(destinations) = destinations {
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations,
            });
        } else {
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: anchor.to_string(),
                hint_payload: hint_payload_for(ctx, anchor),
            });
        }
        return;
    }

    // Detect and resolve folder links (target ending with /)
    if is_folder_link_target(target) {
        if heading.is_some() {
            // Folder links with headings are invalid
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                hint_payload: None,
            });
            return;
        }
        if let Some(destination) = resolve_folder_link(&doc.path, target, &ctx.input.root, &ctx.input.extra_folder_roots) {
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations: vec![destination],
            });
        } else {
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                hint_payload: None,
            });
        }
        return;
    }


    let explicit = is_explicit_path(target);
    let primary_matches =
        find_doc_matches(primary_docs, &doc.path, &ctx.input.root, target, explicit, &[]);
    if primary_matches.len() > 1 {
        ctx.graph.ambiguous_references.push(AmbiguousReference {
            source_path: doc.path.clone(),
            occurrence_id: symbol.id,
            full_range: symbol.full_range,
            name_range: symbol.name_range,
            reference: reference.clone(),
            target: target.to_string(),
            destinations: primary_matches,
        });
        return;
    }

    let destinations = if primary_matches.is_empty() {
        find_doc_matches(extra_docs, &doc.path, &ctx.input.root, target, explicit, &ctx.input.extra_folder_roots)
    } else {
        primary_matches
    };

    // Opt-in Obsidian-style prefix matching runs only after the existing exact,
    // title-slug, and relative-path matches have returned zero candidates, and only
    // when the flag is on. We never prefix-match an explicit path or a folder-link
    // target (both early-returned above) and we never prefix-match an empty target.
    if destinations.is_empty()
        && ctx.input.config.wiki.obsidian_prefix
        && !explicit
        && !target.is_empty()
    {
        let prefix_candidates = ctx.input.prefix_index.matches(target);
        if prefix_candidates.len() == 1 {
            // Single prefix match: let finalize_doc_or_attachment handle the
            // heading/anchor lookup by feeding the candidate through as a normal
            // Document destination.
            let path = prefix_candidates[0].clone();
            let dest = ResolvedDestination {
                path,
                kind: DestinationKind::Document,
                name: String::new(),
                range: None,
            };
            finalize_doc_or_attachment(ctx, target, heading, vec![dest]);
            return;
        }
        if prefix_candidates.len() > 1 {
            let mut ambig_dests = Vec::with_capacity(prefix_candidates.len());
            for path in prefix_candidates {
                ambig_dests.push(ResolvedDestination {
                    path: path.clone(),
                    kind: DestinationKind::Document,
                    name: String::new(),
                    range: None,
                });
            }
            ctx.graph.ambiguous_references.push(AmbiguousReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                destinations: ambig_dests,
            });
            return;
        }
    }

    finalize_doc_or_attachment(ctx, target, heading, destinations);
}

fn resolve_inline_ref(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    anchor: Option<&str>,
    primary_docs: &[ResolvedDocument],
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if has_scheme(target) {
        return;
    }
    if target.is_empty()
        && let Some(anchor) = anchor
    {
        let slug = Slug::from_heading_text(anchor);
        let destinations = doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        if let Some(destinations) = destinations {
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations,
            });
        } else {
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: anchor.to_string(),
                hint_payload: None,
            });
        }
        return;
    }

    // Detect and resolve folder links (target ending with /)
    if is_folder_link_target(target) {
        if anchor.is_some() {
            // Folder links with anchors are invalid
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                hint_payload: None,
            });
            return;
        }
        if let Some(destination) = resolve_folder_link(&doc.path, target, &ctx.input.root, &ctx.input.extra_folder_roots) {
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations: vec![destination],
            });
        } else {
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                hint_payload: None,
            });
        }
        return;
    }


    let destinations = find_doc_matches(primary_docs, &doc.path, &ctx.input.root, target, true, &[]);
    finalize_doc_or_attachment(ctx, target, anchor, destinations);
}

fn finalize_doc_or_attachment(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    anchor: Option<&str>,
    mut destinations: Vec<ResolvedDestination>,
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if destinations.is_empty() && is_attachment_candidate_path(target) {
        let source_dir = doc.path.parent().unwrap_or(ctx.input.root.as_path());
        let path = resolve_explicit_path(&ctx.input.root, source_dir, target);
        if path.exists() {
            destinations.push(ResolvedDestination {
                path,
                kind: DestinationKind::Attachment,
                name: target.to_string(),
                range: None,
            });
        }
    }

    if destinations.len() > 1 {
        ctx.graph.ambiguous_references.push(AmbiguousReference {
            source_path: doc.path.clone(),
            occurrence_id: symbol.id,
            full_range: symbol.full_range,
            name_range: symbol.name_range,
            reference: reference.clone(),
            target: target.to_string(),
            destinations,
        });
        return;
    }

    if let Some(anchor) = anchor
        && let Some(destination) = destinations.first().cloned()
        && matches!(destination.kind, DestinationKind::Document)
        && let Some(target_doc) = ctx
            .graph
            .documents
            .iter()
            .find(|candidate| candidate.path == destination.path)
            .cloned()
    {
        let slug = Slug::from_heading_text(anchor);
        let heading_matches = target_doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| target_doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        if let Some(heading_matches) = heading_matches {
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations: heading_matches,
            });
            return;
        }
        ctx.graph.unresolved_references.push(UnresolvedReference {
            source_path: doc.path.clone(),
            occurrence_id: symbol.id,
            full_range: symbol.full_range,
            name_range: symbol.name_range,
            reference: reference.clone(),
            target: format!("{target}#{anchor}"),
            hint_payload: hint_payload_for(ctx, target),
        });
        return;
    }

    if destinations.is_empty() {
        if ctx.input.single_file && !target.is_empty() {
            return;
        }
        ctx.graph.unresolved_references.push(UnresolvedReference {
            source_path: doc.path.clone(),
            occurrence_id: symbol.id,
            full_range: symbol.full_range,
            name_range: symbol.name_range,
            reference: reference.clone(),
            target: target.to_string(),
            hint_payload: hint_payload_for(ctx, target),
        });
    } else {
        ctx.graph.resolved_references.push(ResolvedReference {
            source_path: doc.path.clone(),
            occurrence_id: symbol.id,
            full_range: symbol.full_range,
            name_range: symbol.name_range,
            reference: reference.clone(),
            destinations,
        });
    }
}

fn find_doc_matches(
    docs: &[ResolvedDocument],
    source_path: &Path,
    root: &Path,
    target: &str,
    explicit_only: bool,
    extra_roots: &[PathBuf],
) -> Vec<ResolvedDestination> {
    let source_dir = source_path.parent().unwrap_or(root);
    let source_dir = source_dir.to_path_buf();
    let target_slug = Slug::from_heading_text(target);
    let explicit_path = resolve_explicit_path(root, &source_dir, target);
    let explicit_no_ext = path_without_extension(&explicit_path);

    let mut destinations = Vec::new();
    let mut seen = HashSet::new();
    for doc in docs {
        let rel_no_ext = path_without_extension(&doc.rel_path);
        let matches = if explicit_only || is_explicit_path(target) {
            // For explicit paths, primarily check path-based matches.
            // When searching extra docs (extra_roots non-empty), also fall back
            // to title slug matching since the target may not be a valid path.
            let path_matches = doc.path == explicit_path
                || rel_no_ext == explicit_no_ext
                || extra_roots.iter().any(|extra_root| {
                    let resolved = resolve_explicit_path(extra_root, extra_root, target);
                    let resolved_rel = resolved.strip_prefix(extra_root).unwrap_or(&resolved);
                    let resolved_no_ext = path_without_extension(resolved_rel);
                    doc.path == resolved || rel_no_ext == resolved_no_ext
                });
            // Extra fallback: title slug matching when searching extra docs
            let slug_matches = !extra_roots.is_empty() && doc.title_slug == target_slug;
            path_matches || slug_matches
        } else {
            doc.file_stem.eq_ignore_ascii_case(target)
                || doc.title_slug == target_slug
                || rel_no_ext.eq_ignore_ascii_case(target)
                || doc
                    .rel_path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(target)
        };
        if matches && seen.insert(doc.path.clone()) {
            destinations.push(ResolvedDestination {
                path: doc.path.clone(),
                kind: DestinationKind::Document,
                name: doc.title_text.clone(),
                range: None,
            });
        }
    }
    destinations
}

fn is_attachment_candidate_path(target: &str) -> bool {
    target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
        || target.contains('\\')
        || target
            .rsplit_once('.')
            .is_some_and(|(base, ext)| !base.is_empty() && !ext.is_empty())
}

fn is_explicit_path(target: &str) -> bool {
    target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
        || target.contains('\\')
        || target
            .rsplit_once('.')
            .is_some_and(|(base, ext)| {
                // Both halves must be non-empty, the extension must be purely
                // alphanumeric, and neither the trailing char of the base nor the
                // leading char of the extension may be a separator like `-` or
                // `_`. The last rule prevents treating the `.` in version-like
                // fragments such as `v2.5b` or `20260723-v2.5b-trust-region` as
                // an extension separator.
                !base.is_empty()
                    && !ext.is_empty()
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
                    && !base.ends_with(|c: char| c == '-' || c == '_')
                    && !ext.starts_with(|c: char| c == '-' || c == '_')
            })
}

fn load_extra_documents(folders: &[PathBuf], config: &Config) -> Vec<ResolveDocument> {
    let ext_set: HashSet<String> = config.core.file_extensions.iter().cloned().collect();
    let mut documents = Vec::new();
    for root in folders {
        let walker = ignore::WalkBuilder::new(root).follow_links(true).build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if !ext_set.contains(ext) {
                continue;
            }
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            documents.push(ResolveDocument {
                path: path.to_path_buf(),
                rel_path: path.strip_prefix(root).unwrap_or(path).to_path_buf(),
                structure: crate::parser::parse_document(
                    &text,
                    crate::parser::ParseOptions::default(),
                ),
            });
        }
    }
    documents
}

/// Compute the hint payload (prefix-candidate file paths) for an unresolved wiki-link
/// target. Returns `Some(candidates)` if the workspace contains any file whose stem
/// starts with `target`; `None` otherwise.
fn hint_payload_for(ctx: &ResolveRefContext<'_>, target: &str) -> Option<Vec<PathBuf>> {
    let candidates = ctx.input.prefix_index.matches(target);
    if candidates.is_empty() {
        None
    } else {
        Some(candidates.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::is_explicit_path;

    /// A target ending in a normal extension (e.g. `file.md`) is an explicit path.
    #[test]
    fn explicit_path_md_extension() {
        assert!(is_explicit_path("file.md"));
        assert!(is_explicit_path("./file.md"));
        assert!(is_explicit_path("../sibling/file.md"));
    }

    /// A bare stem with no `.` is not explicit (it relies on title/stem/prefix match).
    #[test]
    fn explicit_path_no_dot_is_not_explicit() {
        assert!(!is_explicit_path("guide"));
        assert!(!is_explicit_path("20260723-v2-neb-step1-result"));
    }

    /// Stems with `.` followed by a non-alphanumeric suffix (e.g. version numbers
    /// like `v2.5b-trust-region` where the trailing fragment contains `-`) are
    /// NOT treated as explicit paths. The `.` is only considered an extension
    /// separator when the trailing fragment is purely alphanumeric, so the
    /// prefix matcher can rescue targets like `20260723-v2.5b-trust-region`.
    #[test]
    fn explicit_path_internal_dot_is_not_explicit() {
        // Trailing fragment contains `-`, so the `.` is not an extension separator.
        assert!(!is_explicit_path("20260723-v2.5b-trust-region"));
        assert!(!is_explicit_path("note.v2.5b-draft"));
        assert!(!is_explicit_path("foo.bar-baz"));
        // Extension contains an underscore, so the `.` is not an extension separator.
        assert!(!is_explicit_path("foo.bar_baz"));
    }

    /// Targets that contain `/` or `\` are always explicit regardless of `.`.
    #[test]
    fn explicit_path_separators_are_explicit() {
        assert!(is_explicit_path("sub/file"));
        assert!(is_explicit_path("sub/file.md"));
        assert!(is_explicit_path("a\\b"));
        assert!(is_explicit_path("/abs/path"));
    }

    /// Edge cases for the extension-separator rule: a trailing dot or a leading dot
    /// do not produce a valid extension, so the target is not explicit.
    #[test]
    fn explicit_path_edge_cases() {
        // Trailing dot: extension is empty.
        assert!(!is_explicit_path("foo."));
        // Leading dot: base is empty.
        assert!(!is_explicit_path(".gitignore"));
        // Only the dot.
        assert!(!is_explicit_path("."));
    }
}
