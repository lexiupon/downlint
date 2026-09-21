pub mod auto_verify;
pub mod conn;
pub mod path;
pub mod prefix;
pub mod slug;
pub mod uri;
pub mod uri_sync;

use crate::config::Config;
use crate::parser::{Def, LinkLabel, Ref, Structure, SymKind};
use crate::resolution::conn::{
    AmbiguousReference, DestinationKind, ResolvedDestination, ResolvedDocument, ResolvedReference,
    UnresolvedReference,
};
use crate::resolution::path::{has_scheme, is_external_web_scheme, is_folder_link_target, path_without_extension, resolve_explicit_path, scheme_of};
use crate::resolution::prefix::PrefixIndex;
use crate::resolution::uri::{UriOutcome, UriResolver};
use crate::resolution::uri_sync::SyncRunner;
use crate::utils::{MountConflict, MountConflictKind, ResolvedMount, Workspace, WorkspaceMode};
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
    /// Path relative to the document's own root (the primary root for primary
    /// docs, the mount root for mounted docs).
    pub rel_path: PathBuf,
    pub structure: Structure,
    /// Address in the combined namespace (RFC 0010): `rel_path` for primary
    /// docs, `prefix/rel_path` (or `rel_path`) for mounted docs.
    pub namespace_rel_path: PathBuf,
    /// Mount attribution (`prefix` or `root`), `None` for primary docs.
    pub mount: Option<String>,
    /// Whether this doc's own links are linted.
    pub is_source: bool,
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

    /// Construct a primary (non-mounted) document: namespace path == `rel_path`,
    /// no mount attribution, always a source. Used by tests and the LSP/rename
    /// layers that build inputs by hand.
    pub fn primary(path: PathBuf, rel_path: PathBuf, structure: Structure) -> Self {
        Self {
            path,
            rel_path: rel_path.clone(),
            structure,
            namespace_rel_path: rel_path,
            mount: None,
            is_source: true,
        }
    }
}

/// Per-run options for URI mapping behavior. The CLI layer sets these from
/// `--allow-uri-sync`, `--no-uri-hints`, and `--uri-sync-batch-size`. Defaults
/// are conservative: no sync, hints on, batch size 50.
#[derive(Clone, Debug)]
pub struct UriOptions {
    pub allow_sync: bool,
    pub no_hints: bool,
    pub batch_size: usize,
}

impl Default for UriOptions {
    fn default() -> Self {
        Self {
            allow_sync: false,
            no_hints: false,
            batch_size: 50,
        }
    }
}

// re-export for callers that want to wire sync state without taking a
// dependency on `crate::resolution::uri_sync` directly.
pub use uri_sync::UriSyncCache;

#[derive(Clone, Debug)]
pub struct ResolveInput {
    pub root: PathBuf,
    /// All indexed documents: primary docs plus mounted docs (co-equal, RFC
    /// 0010). Each carries its namespace path, mount attribution, and source flag.
    pub documents: Vec<ResolveDocument>,
    /// The configured mounts (resolved roots + prefix + lint), used for folder
    /// links and structural-conflict detection.
    pub mounts: Vec<ResolvedMount>,
    /// Namespace-level mount conflicts detected at startup (RFC 0010). Each is
    /// reported as a `mount/conflict` error diagnostic.
    pub conflicts: Vec<MountConflict>,
    pub config: Config,
    pub single_file: bool,
    /// Case-insensitive prefix index over the stems of `documents` and `extra_documents`.
    /// Built eagerly in `from_workspace` (and constructed manually by tests).
    pub prefix_index: PrefixIndex,
    /// Resolver built from `[uri.mappings]` in `.downlint.toml`. Empty when no
    /// URI section is configured. Used by `resolve_wiki_ref` and
    /// `resolve_inline_ref` to validate `scheme://...` targets.
    pub uri_resolver: UriResolver,
    /// Per-run toggles (sync gating, hint suppression, batch size). Modified
    /// after construction by the CLI layer; resolution reads them lazily.
    pub uri_opts: UriOptions,
    /// Shared cache for sync results. The CLI constructs a fresh cache each
    /// run; the LSP layer (task #8) reuses one cache across document-change
    /// events to avoid re-forking sync subprocesses on every keystroke.
    pub uri_sync_cache: UriSyncCache,
    /// Set when URI mapping expansion failed at startup (e.g. missing env
    /// var). Resolution still proceeds with an empty resolver; the CLI
    /// surfaces the error before diagnostics are emitted.
    pub uri_error: Option<String>,
}

impl ResolveInput {
    pub fn from_workspace(workspace: &Workspace) -> Self {
        let parse_options = crate::parser::ParseOptions {
            title_from_heading: workspace.config.core.title_from_heading,
            heading_ids: workspace.config.core.heading_ids.enable,
        };

        // Primary docs: namespace path == rel_path, always sources.
        let mut documents: Vec<ResolveDocument> = workspace
            .folder
            .documents
            .iter()
            .map(|doc| {
                let rel_path = doc.rel_path.clone();
                ResolveDocument {
                    path: doc.path.clone(),
                    rel_path: rel_path.clone(),
                    structure: crate::parser::parse_document(doc.text.as_str(), parse_options),
                    namespace_rel_path: rel_path,
                    mount: None,
                    is_source: true,
                }
            })
            .collect();

        // The primary project's top-level entry names (folders and files),
        // used for structural-conflict detection (RFC 0010).
        let primary_top_level: HashSet<String> = workspace
            .folder
            .documents
            .iter()
            .filter_map(|doc| doc.rel_path.components().next())
            .filter_map(|component| component.as_os_str().to_str())
            .map(|name| name.to_string())
            .collect();

        let mut conflicts: Vec<MountConflict> = Vec::new();

        // Mounted docs: co-equal with primary (RFC 0010). Namespace path is
        // `prefix/rel` when a prefix is set, else `rel`. Sources only when the
        // mount has `lint = true`. A prefix conflict suspends the prefix;
        // a folder conflict suspends linting of the conflicting folder.
        for mount in &workspace.folder.mounts {
            let loaded_docs = load_mount_documents(&mount.root, &workspace.config);
            let mount_top_level_folders: HashSet<String> = loaded_docs
                .iter()
                .filter_map(|doc| doc.rel_path.components().next())
                .filter_map(|component| component.as_os_str().to_str())
                .map(|name| name.to_string())
                .collect();

            // Prefix conflict: the prefix's first component matches a primary
            // top-level entry.
            let prefix_first = mount
                .prefix
                .as_deref()
                .map(|prefix| {
                    prefix
                        .trim_start_matches('/')
                        .split('/')
                        .next()
                        .unwrap_or("")
                        .to_string()
                });
            let prefix_conflict = prefix_first
                .as_deref()
                .filter(|first| !first.is_empty())
                .is_some_and(|first| primary_top_level.contains(first));

            // Folder conflict: a top-level folder in the mount matches a
            // primary top-level folder.
            let conflicting_folders: HashSet<String> = mount_top_level_folders
                .iter()
                .filter(|folder| primary_top_level.contains(*folder))
                .cloned()
                .collect();

            if prefix_conflict
                && let Some(first) = &prefix_first
            {
                let prefix = mount.prefix.as_deref().unwrap_or("");
                conflicts.push(MountConflict {
                    mount_attribution: mount.attribution.clone(),
                    kind: MountConflictKind::Prefix,
                    detail: format!("prefix `{prefix}` collides with primary path `{first}`"),
                });
            }
            for folder in &conflicting_folders {
                conflicts.push(MountConflict {
                    mount_attribution: mount.attribution.clone(),
                    kind: MountConflictKind::Folder,
                    detail: format!("top-level folder `{folder}` collides with a primary folder"),
                });
            }

            let apply_prefix = !prefix_conflict;
            for loaded in loaded_docs {
                let namespace_rel_path = if apply_prefix
                    && let Some(prefix) = &mount.prefix
                {
                    PathBuf::from(prefix.trim_start_matches('/')).join(&loaded.rel_path)
                } else {
                    loaded.rel_path.clone()
                };
                // A doc under a conflicting folder is a target only (not linted).
                let under_conflict = loaded
                    .rel_path
                    .components()
                    .next()
                    .and_then(|component| component.as_os_str().to_str())
                    .is_some_and(|first| conflicting_folders.contains(first));
                let is_source = mount.lint && !under_conflict;
                documents.push(ResolveDocument {
                    path: loaded.path,
                    rel_path: loaded.rel_path,
                    structure: loaded.structure,
                    namespace_rel_path,
                    mount: Some(mount.attribution.clone()),
                    is_source,
                });
            }
        }

        let prefix_index = PrefixIndex::from_entries(
            documents.iter().map(|doc| (doc.stem(), doc.path.clone())),
        );

        let mounts = workspace.folder.mounts.clone();

        let uri_resolver = match UriResolver::new(&workspace.config.uri, &workspace.folder.root)
        {
            Ok(resolver) => resolver,
            Err(error) => {
                // Per RFC: env-var expansion failures fail fast at startup.
                // The CLI layer catches this and surfaces it as a config error.
                return ResolveInput {
                    root: workspace.folder.root.clone(),
                    documents,
                    mounts,
                    conflicts,
                    config: workspace.config.clone(),
                    single_file: matches!(workspace.mode, WorkspaceMode::SingleFile),
                    prefix_index,
                    uri_resolver: UriResolver::empty(),
                    uri_opts: UriOptions::default(),
                    uri_sync_cache: UriSyncCache::new(),
                    uri_error: Some(error.to_string()),
                };
            }
        };

        Self {
            root: workspace.folder.root.clone(),
            documents,
            mounts,
            conflicts,
            config: workspace.config.clone(),
            single_file: matches!(workspace.mode, WorkspaceMode::SingleFile),
            prefix_index,
            uri_resolver,
            uri_opts: UriOptions::default(),
            uri_sync_cache: UriSyncCache::new(),
            uri_error: None,
        }
    }
}

pub fn resolve_links(input: ResolveInput) -> ConnectionGraph {
    // All docs (primary + mounted) are indexed co-equal (RFC 0010).
    let all_documents = input
        .documents
        .iter()
        .map(|doc| index_document(doc, &input.root))
        .collect::<Vec<_>>();

    let mut graph = ConnectionGraph {
        documents: all_documents.clone(),
        conflicts: input.conflicts.clone(),
        ..ConnectionGraph::default()
    };

    for doc in &all_documents {
        // Mounted docs with `lint = false` are targets only — their own links
        // are not diagnosed.
        if !doc.is_source {
            continue;
        }
        resolve_document(doc, &all_documents, &input, &mut graph);
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
        namespace_rel_path: doc.namespace_rel_path.clone(),
        mount: doc.mount.clone(),
        is_source: doc.is_source,
    }
}

fn resolve_document(
    doc: &ResolvedDocument,
    all_docs: &[ResolvedDocument],
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
                resolve_wiki_ref(&mut ctx, target, heading.as_deref(), all_docs);
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
                resolve_inline_ref(&mut ctx, target, anchor.as_deref(), all_docs);
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
                        is_anchor: false,
                        hint_payload: None,
                        uri_no_mapping_hint: false,
                        sync_was_soft_failure: false,
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


/// Resolve a URI-scheme target (`scheme://...`) using the configured
/// `[uri.mappings]`. Behavior depends on the `UriOutcome` returned by the
/// resolver:
///
/// - `NotApplicable` — no URI scheme in target; caller should fall through.
///   In practice we never get here because the entry points check
///   `has_scheme()` first.
/// - `NoMapping` — emit a broken-link diagnostic; mark it so the diagnostics
///   layer renders a one-time hint pointing to `[uri.mappings]`. Suppressed
///   when `[uri]` is unconfigured or `--no-uri-hints` is set.
/// - `Resolved` — run sync (if configured + allowed), then either resolve
///   (file present) or emit a broken-link diagnostic. Sync failure with
///   `warm_required = true` also produces a broken diagnostic; otherwise it
///   produces a warning-level status and validation continues.
///
/// `anchor` is the heading/anchor portion of the link (e.g. `#section`).
/// URI-scheme links with non-empty anchors are reported as broken —
/// anchors on external assets are not supported.
fn resolve_uri_target(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    anchor: Option<&str>,
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    let resolver = &ctx.input.uri_resolver;

    let outcome = resolver.resolve(target);
    match outcome {
        UriOutcome::NotApplicable => {
            // Caller already gated on `has_scheme(target)`; defensive only.
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: target.to_string(),
                is_anchor: false,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
            });
        }
        UriOutcome::NoMapping { target: raw } => {
            // External web URLs (http, https, mailto, ftp, ...) are not
            // "broken" — they reference resources outside the workspace by
            // design. Skip them silently rather than emitting a link/broken that
            // the user can do nothing about.
            if let Some(scheme) = scheme_of(&raw) {
                if is_external_web_scheme(scheme) {
                    return;
                }
            }
            // Only suggest configuring [uri.mappings] when the user has
            // actually opted in. An empty resolver means no mappings are
            // configured; hinting there would add noise with no fix path.
            let hint = !resolver.is_empty() && !ctx.input.uri_opts.no_hints;
            ctx.graph.unresolved_references.push(UnresolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                target: raw,
                is_anchor: anchor.is_some(),
                hint_payload: None,
                uri_no_mapping_hint: hint,
                sync_was_soft_failure: false,
            });
        }
        UriOutcome::Resolved {
            mapping_index,
            target: raw,
            relative: _,
            resolved_path,
        } => {
            // Anchors on external assets are not supported — they would
            // require opening the resolved file and parsing headings, which
            // is out of scope for Phase 1.
            if anchor.is_some() {
                ctx.graph.unresolved_references.push(UnresolvedReference {
                    source_path: doc.path.clone(),
                    occurrence_id: symbol.id,
                    full_range: symbol.full_range,
                    name_range: symbol.name_range,
                    reference: reference.clone(),
                    target: raw,
                    is_anchor: true,
                    hint_payload: None,
                    uri_no_mapping_hint: false,
                    sync_was_soft_failure: false,
                });
                return;
            }

            let runner = SyncRunner::with_cache(
                resolver,
                ctx.input.uri_opts.allow_sync,
                ctx.input.uri_opts.batch_size,
                ctx.input.uri_sync_cache.clone(),
            );
            let result = runner.run_for(mapping_index, resolved_path.clone());

            let present = matches!(
                result.status,
                crate::resolution::uri_sync::PathStatus::Present
            );

            if present {
                ctx.graph.resolved_references.push(ResolvedReference {
                    source_path: doc.path.clone(),
                    occurrence_id: symbol.id,
                    full_range: symbol.full_range,
                    name_range: symbol.name_range,
                    reference: reference.clone(),
                    destinations: vec![ResolvedDestination {
                        path: resolved_path,
                        kind: DestinationKind::Attachment,
                        name: raw,
                        range: None,
                    }],
                });
            } else {
                // Sync ran (or would have, if `--allow-uri-sync` had been
                // passed) and the file is still missing, or sync itself
                // failed/timed out. The link is reported as broken in all
                // cases. When the mapping has `warm_required = false`, we
                // additionally flag this as a soft failure so the
                // diagnostics layer can emit uri/sync-failed alongside the
                // regular link/broken.
                let warm_required = resolver
                    .sync_config(mapping_index)
                    .map(|sync| sync.required)
                    .unwrap_or(false);
                let sync_ran = matches!(
                    result.decision,
                    crate::resolution::uri_sync::SyncDecision::Ran
                );
                let sync_failed = matches!(
                    result.status,
                    crate::resolution::uri_sync::PathStatus::SyncFailed
                        | crate::resolution::uri_sync::PathStatus::SyncTimedOut
                );
                let soft_failure = !warm_required && sync_ran && sync_failed;
                ctx.graph.unresolved_references.push(UnresolvedReference {
                    source_path: doc.path.clone(),
                    occurrence_id: symbol.id,
                    full_range: symbol.full_range,
                    name_range: symbol.name_range,
                    reference: reference.clone(),
                    target: raw,
                    is_anchor: false,
                    hint_payload: None,
                    uri_no_mapping_hint: false,
                    sync_was_soft_failure: soft_failure,
                });
            }
        }
    }
}


/// Resolve a folder link (target ending with /) against the workspace and extra folders.
fn resolve_folder_link(
    source_path: &Path,
    target: &str,
    root: &Path,
    mounts: &[ResolvedMount],
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

    // Check in mount roots for absolute paths (RFC 0010): try a direct join to
    // the mount root, and (when the mount has a prefix) strip the prefix first.
    if target_dir.starts_with('/') {
        let rel = target_dir.trim_start_matches('/');
        for mount in mounts {
            let candidate = mount.root.join(rel);
            if candidate.is_dir() {
                return Some(ResolvedDestination {
                    path: candidate,
                    kind: DestinationKind::Directory,
                    name: target_dir.to_string(),
                    range: None,
                });
            }
            if let Some(prefix) = &mount.prefix {
                let prefix_rel = prefix.trim_start_matches('/');
                if let Some(rest) = rel.strip_prefix(prefix_rel) {
                    let candidate = mount.root.join(rest.trim_start_matches('/'));
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

    None
}

fn resolve_wiki_ref(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    heading: Option<&str>,
    all_docs: &[ResolvedDocument],
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if has_scheme(target) {
        resolve_uri_target(ctx, target, heading);
        return;
    }
    if target.is_empty()
        && let Some(anchor) = heading
    {
        let slug = Slug::from_heading_text(anchor);
        // Strict slug lookup first.
        let strict = doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        // Tolerant fallback: see `resolve_inline_ref` for the rationale.
        let destinations = strict.or_else(|| {
            let folded = slug.folded();
            if folded == slug {
                None
            } else {
                doc.headings.get(&folded).cloned()
            }
        });
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
                is_anchor: true,
                hint_payload: hint_payload_for(ctx, anchor),
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
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
                is_anchor: false,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
            });
            return;
        }
        if let Some(destination) = resolve_folder_link(&doc.path, target, &ctx.input.root, &ctx.input.mounts) {
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
                is_anchor: false,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
            });
        }
        return;
    }


    let explicit = is_explicit_path(target);
    // Co-equal matching over primary + mounted docs (RFC 0010): more than one
    // candidate is `link/ambiguous`; there is no primary-first fallback.
    let destinations = find_doc_matches(all_docs, &doc.path, &ctx.input.root, target, explicit, true);
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
    all_docs: &[ResolvedDocument],
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if has_scheme(target) {
        resolve_uri_target(ctx, target, anchor);
        return;
    }
    if target.is_empty()
        && let Some(anchor) = anchor
    {
        let slug = Slug::from_heading_text(anchor);
        // Strict slug lookup first.
        let strict = doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        // Tolerant fallback: if the strict lookup misses, retry against the
        // same map using a folded slug (consecutive `-` collapsed). This lets
        // a hand-written anchor with `--` (e.g. copy-pasted from an em-dash
        // heading) still resolve when the canonical heading slug uses `-`.
        let destinations = strict.or_else(|| {
            let folded = slug.folded();
            if folded == slug {
                None
            } else {
                doc.headings.get(&folded).cloned()
            }
        });
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
                is_anchor: true,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
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
                is_anchor: false,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
            });
            return;
        }
        if let Some(destination) = resolve_folder_link(&doc.path, target, &ctx.input.root, &ctx.input.mounts) {
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
                is_anchor: false,
                hint_payload: None,
                uri_no_mapping_hint: false,
                sync_was_soft_failure: false,
            });
        }
        return;
    }


    let destinations = find_doc_matches(all_docs, &doc.path, &ctx.input.root, target, true, false);
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
        // Strict slug lookup first.
        let strict = target_doc
            .headings
            .get(&slug)
            .cloned()
            .or_else(|| target_doc.tags.get(&anchor.to_ascii_lowercase()).cloned());
        // Tolerant fallback: see `resolve_inline_ref` for the rationale.
        let heading_matches = strict.or_else(|| {
            let folded = slug.folded();
            if folded == slug {
                None
            } else {
                target_doc.headings.get(&folded).cloned()
            }
        });
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
            is_anchor: true,
            hint_payload: hint_payload_for(ctx, target),
            uri_no_mapping_hint: false,
            sync_was_soft_failure: false,
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
            is_anchor: false,
            hint_payload: hint_payload_for(ctx, target),
            uri_no_mapping_hint: false,
            sync_was_soft_failure: false,
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
    is_wiki: bool,
) -> Vec<ResolvedDestination> {
    let source_dir = source_path.parent().unwrap_or(root);
    let source_dir = source_dir.to_path_buf();
    let target_slug = Slug::from_heading_text(target);
    let explicit_path = resolve_explicit_path(root, &source_dir, target);
    let explicit_no_ext = path_without_extension(&explicit_path);
    // For a workspace-absolute target (`/kb/notes/foo`), its namespace path is
    // the target without the leading slash. This is how a mount `prefix` (a
    // virtual directory at the workspace root) is reached (RFC 0010).
    let target_namespace_no_ext = target
        .strip_prefix('/')
        .map(|value| path_without_extension(Path::new(value)));

    let mut destinations = Vec::new();
    let mut seen = HashSet::new();
    for doc in docs {
        // `namespace_rel_path` equals `rel_path` for primary docs and is
        // `prefix/rel` (or `rel`) for mounted docs, so matching on it is
        // co-equal across primary + mounted docs (RFC 0010).
        let ns_no_ext = path_without_extension(&doc.namespace_rel_path);
        let matches = if explicit_only || is_explicit_path(target) {
            // Path-based matching:
            //  (a) filesystem match (markdown links, source-relative);
            //  (b) resolved-path namespace match;
            //  (c) workspace-absolute namespace match (mount prefix access).
            let fs_match = doc.path == explicit_path;
            let rel_ns_match = ns_no_ext.eq_ignore_ascii_case(&explicit_no_ext);
            let abs_ns_match = target_namespace_no_ext
                .as_ref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&ns_no_ext));
            // A wiki target containing `/` may actually be a title (e.g.
            // "Team knowledge transfer (QA/DB)"); fall back to title-slug
            // matching, co-equal across primary + mounted docs. Markdown links
            // are paths only (no title fallback).
            let title_match = is_wiki && doc.title_slug == target_slug;
            fs_match || rel_ns_match || abs_ns_match || title_match
        } else {
            doc.file_stem.eq_ignore_ascii_case(target)
                || doc.title_slug == target_slug
                || ns_no_ext.eq_ignore_ascii_case(target)
                || doc
                    .namespace_rel_path
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

/// A document loaded from a mount root, before its namespace path / attribution
/// / source flag are attached by the caller (RFC 0010).
struct LoadedDoc {
    path: PathBuf,
    rel_path: PathBuf,
    structure: Structure,
}

fn load_mount_documents(mount_root: &Path, config: &Config) -> Vec<LoadedDoc> {
    let ext_set: HashSet<String> = config.core.file_extensions.iter().cloned().collect();
    let mut documents = Vec::new();
    let walker = ignore::WalkBuilder::new(mount_root).follow_links(true).build();
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
        documents.push(LoadedDoc {
            path: path.to_path_buf(),
            rel_path: path.strip_prefix(mount_root).unwrap_or(path).to_path_buf(),
            structure: crate::parser::parse_document(
                &text,
                crate::parser::ParseOptions {
                    title_from_heading: config.core.title_from_heading,
                    heading_ids: config.core.heading_ids.enable,
                },
            ),
        });
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
