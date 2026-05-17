pub mod conn;
pub mod path;
pub mod slug;

use crate::config::Config;
use crate::parser::{Def, LinkLabel, Ref, Structure, SymKind};
use crate::resolution::conn::{
    AmbiguousReference, DestinationKind, ResolvedDestination, ResolvedDocument, ResolvedReference,
    UnresolvedReference,
};
use crate::resolution::path::{has_scheme, path_without_extension, resolve_explicit_path};
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

#[derive(Clone, Debug)]
pub struct ResolveInput {
    pub root: PathBuf,
    pub documents: Vec<ResolveDocument>,
    pub extra_documents: Vec<ResolveDocument>,
    pub config: Config,
    pub single_file: bool,
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

        Self {
            root: workspace.folder.root.clone(),
            documents,
            extra_documents,
            config: workspace.config.clone(),
            single_file: matches!(workspace.mode, WorkspaceMode::SingleFile),
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
            });
        }
        return;
    }

    let explicit = is_explicit_path(target);
    let primary_matches =
        find_doc_matches(primary_docs, &doc.path, &ctx.input.root, target, explicit);
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

    let destinations = if primary_matches.is_empty() && !explicit {
        find_doc_matches(extra_docs, &doc.path, &ctx.input.root, target, false)
    } else {
        primary_matches
    };

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
            });
        }
        return;
    }

    let destinations = find_doc_matches(primary_docs, &doc.path, &ctx.input.root, target, true);
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
    if destinations.is_empty() && is_attachment_path(target, &ctx.input.config) {
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
            doc.path == explicit_path || rel_no_ext == explicit_no_ext
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

fn is_attachment_path(target: &str, config: &Config) -> bool {
    target
        .rsplit_once('.')
        .map(|(_, ext)| {
            config
                .core
                .attachment_file_extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

fn is_explicit_path(target: &str) -> bool {
    target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
        || target.contains('\\')
        || target.contains('.')
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
