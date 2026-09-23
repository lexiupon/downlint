pub mod auto_verify;
pub mod conn;
pub mod path;
pub mod prefix;
pub mod query;
pub mod slug;
pub mod uri;

use crate::config::Config;
use crate::parser::{Def, LinkLabel, Ref, Structure, SymKind};
use crate::resolution::conn::{
    AmbiguousReference, DestinationKind, ResolvedDestination, ResolvedDocument, ResolvedReference,
    UnresolvedReference,
};
use crate::resolution::path::{
    is_external_web_scheme, is_folder_link_target, is_root_relative,
    resolve_explicit_path, scheme_of,
};
use crate::resolution::prefix::PrefixIndex;
use crate::resolution::uri::{UriOutcome, UriResolver};
use crate::utils::{
    DocumentSource, MountConflict, MountConflictKind, ResolvedMount, Workspace, WorkspaceMode,
};
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
/// are conservative: no sync, hints on.
#[derive(Clone, Debug)]
pub struct UriOptions {
    /// Safety gate for subprocess execution (a schema's `verify_cmd`). A
    /// committed config with a `verify_cmd` MUST NOT run it without this flag.
    pub allow_sync: bool,
    pub no_hints: bool,
}

impl Default for UriOptions {
    fn default() -> Self {
        Self {
            allow_sync: false,
            no_hints: false,
        }
    }
}

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
    /// Resolver built from `[[schemas]]` in `.downlint.toml`. Empty when no
    /// schemas are configured. Used by `resolve_wiki_ref` and
    /// `resolve_inline_ref` to validate `scheme://...` targets.
    pub uri_resolver: UriResolver,
    /// Per-run toggles (hint suppression). Modified after construction by the
    /// CLI layer; resolution reads them lazily.
    pub uri_opts: UriOptions,
    /// Set when schema mapping expansion failed at startup (e.g. missing env
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

        // Primary docs: namespace path == rel_path. In stdin mode the piped
        // `<stdin>.md` document is the only source; workspace docs are indexed
        // as targets only (same mechanism as `lint = false` mounts). Otherwise
        // all primary docs are sources.
        let stdin_mode = workspace
            .folder
            .documents
            .iter()
            .any(|doc| doc.source == DocumentSource::Stdin);
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
                    is_source: stdin_mode
                        .then(|| doc.source == DocumentSource::Stdin)
                        .unwrap_or(true),
                }
            })
            .collect();

        // Primary namespace sets, used for fine-grained structural-conflict
        // detection (RFC 0011). `primary_files` is the set of primary namespace
        // paths; `primary_file_stems` is the set of (location, stem) for each
        // primary file; `primary_folders` is the set of (location, name) folder
        // entries implied by primary-file paths.
        let primary_files: HashSet<PathBuf> = workspace
            .folder
            .documents
            .iter()
            .map(|doc| doc.rel_path.clone())
            .collect();
        let mut primary_file_stems: HashSet<(PathBuf, String)> = HashSet::new();
        let mut primary_folders: HashSet<(PathBuf, String)> = HashSet::new();
        for doc in &workspace.folder.documents {
            if let Some(entry) = file_stem_entry(&doc.rel_path) {
                primary_file_stems.insert(entry);
            }
            for entry in folder_entries(&doc.rel_path) {
                primary_folders.insert(entry);
            }
        }

        let mut conflicts: Vec<MountConflict> = Vec::new();

        // Mounted docs: co-equal with primary (RFC 0010). Namespace path is
        // `as/rel` when `as` is set, else `rel`. Sources only when the
        // mount has `lint = true`. A path collision (RFC 0011) suspends linting
        // of the conflicting mount file(s).
        for mount in &workspace.folder.mounts {
            let loaded_docs = load_mount_documents(&mount.path, &workspace.config);
            let prefix = mount
                .r#as
                .as_deref()
                .map(|p| p.trim_start_matches('/').to_string());

            // Each mount doc's namespace path (prefix applied when set).
            let namespace_paths: Vec<PathBuf> = loaded_docs
                .iter()
                .map(|doc| match &prefix {
                    Some(p) => PathBuf::from(p).join(&doc.rel_path),
                    None => doc.rel_path.clone(),
                })
                .collect();

            // Mount namespace set: (location, name) per implied folder. (The
            // per-file (location, stem) is recomputed in case (b) below, since
            // it must map back to the namespace path.)
            let mut mount_folders: HashSet<(PathBuf, String)> = HashSet::new();
            for ns in &namespace_paths {
                for entry in folder_entries(ns) {
                    mount_folders.insert(entry);
                }
            }

            // Conflicting mount namespace paths (drives the suspend behavior).
            let mut conflicting_ns: HashSet<PathBuf> = HashSet::new();

            // (a) Same-path file collision: a mount file occupies the same
            //     namespace path as a primary file.
            for ns in &namespace_paths {
                if primary_files.contains(ns) {
                    conflicts.push(MountConflict {
                        mount_attribution: mount.attribution.clone(),
                        kind: MountConflictKind::PathCollision,
                        detail: format!(
                            "file `{}` collides with a primary file at the same path",
                            ns.display()
                        ),
                    });
                    conflicting_ns.insert(ns.clone());
                }
            }

            // (b) File/folder name collision: a mount file's (location, stem)
            //     matches a primary folder.
            for ns in &namespace_paths {
                if let Some(entry) = file_stem_entry(ns)
                    && primary_folders.contains(&entry)
                    && !conflicting_ns.contains(ns)
                {
                    conflicts.push(MountConflict {
                        mount_attribution: mount.attribution.clone(),
                        kind: MountConflictKind::PathCollision,
                        detail: format!(
                            "file `{}` collides with a primary folder of the same name",
                            ns.display()
                        ),
                    });
                    conflicting_ns.insert(ns.clone());
                }
            }

            // (c) File/folder name collision: a mount folder's (location, name)
            //     matches a primary file. Files under such a folder conflict too.
            let mut conflicting_mount_folders: HashSet<(PathBuf, String)> = HashSet::new();
            for entry in &mount_folders {
                if primary_file_stems.contains(entry) {
                    let (loc, name) = entry;
                    conflicts.push(MountConflict {
                        mount_attribution: mount.attribution.clone(),
                        kind: MountConflictKind::PathCollision,
                        detail: format!(
                            "folder `{}/` collides with a primary file of the same name",
                            loc.join(name).display()
                        ),
                    });
                    conflicting_mount_folders.insert(entry.clone());
                }
            }
            for ns in &namespace_paths {
                if conflicting_ns.contains(ns) {
                    continue;
                }
                for (loc, name) in &conflicting_mount_folders {
                    if ns.starts_with(loc.join(name)) {
                        conflicting_ns.insert(ns.clone());
                        break;
                    }
                }
            }

            for (loaded, ns) in loaded_docs.iter().zip(&namespace_paths) {
                let is_source = mount.lint && !conflicting_ns.contains(ns);
                documents.push(ResolveDocument {
                    path: loaded.path.clone(),
                    rel_path: loaded.rel_path.clone(),
                    structure: loaded.structure.clone(),
                    namespace_rel_path: ns.clone(),
                    mount: Some(mount.attribution.clone()),
                    is_source,
                });
            }
        }

        let prefix_index = PrefixIndex::from_entries(
            documents.iter().map(|doc| (doc.stem(), doc.path.clone())),
        );

        let mounts = workspace.folder.mounts.clone();

        let uri_resolver = match UriResolver::new(&workspace.config.schemas, &workspace.folder.root)
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

pub(crate) fn index_document(doc: &ResolveDocument, _root: &Path) -> ResolvedDocument {
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
/// `[[schemas]]`. Behavior depends on the `UriOutcome` returned by the
/// resolver:
///
/// - `NotApplicable` — no URI scheme in target; caller should fall through.
///   In practice we never get here because the entry points check
///   `is_uri_target()` first.
/// - `NoMapping` — emit a broken-link diagnostic; mark it so the diagnostics
///   layer renders a one-time hint pointing to `[[schemas]]`. Suppressed
///   when `[[schemas]]` is unconfigured or `--no-uri-hints` is set.
/// - `Resolved` — stat the resolved path; if present and not an evicted
///   placeholder (per the schema's `auto_verify` / `verify_cmd`), resolve it
///   as an attachment; otherwise emit a broken-link diagnostic.
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
            // Caller already gated on `is_uri_target(target)`; defensive only.
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
            // Only suggest configuring [[schemas]] when the user has
            // actually opted in. An empty resolver means no schemas are
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
                });
                return;
            }

            // Stat the resolved path. If it doesn't exist, the link is broken.
            let exists = resolved_path.is_file();
            // Verify (placeholder detection): a custom verify_cmd is
            // authoritative (gated by --allow-uri-sync, since it is a
            // subprocess); otherwise the built-in vendor-specific heuristics
            // (when the schema has auto_verify enabled — pure fs checks, no
            // gate needed).
            let is_placeholder = if exists {
                if let Some(cmd) = resolver.verify_cmd_for(mapping_index) {
                    if ctx.input.uri_opts.allow_sync {
                        !run_verify_cmd(cmd, &resolved_path)
                    } else {
                        // verify_cmd configured but --allow-uri-sync not
                        // passed: treat as inconclusive (not a placeholder).
                        false
                    }
                } else {
                    crate::resolution::auto_verify::classify(
                        &resolved_path,
                        resolver.auto_verify_for(mapping_index),
                    ) == crate::resolution::auto_verify::AutoVerifyOutcome::Placeholder
                }
            } else {
                false
            };

            if exists && !is_placeholder {
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
                // The file is missing, or it's an evicted cloud placeholder.
                // The link is broken in both cases.
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
                });
            }
        }
    }
}

/// Run a schema's `verify_cmd` against `path`. `{path}` is substituted with
/// the resolved absolute file path. Returns `true` when the command exits 0
/// (real file); `false` on non-zero exit, spawn failure, or timeout
/// (placeholder). This is the advanced escape hatch for vendor-specific
/// verification that the built-in heuristics don't cover.
fn run_verify_cmd(cmd: &[String], path: &Path) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let args: Vec<String> = cmd
        .iter()
        .map(|arg| arg.replace("{path}", &path.to_string_lossy()))
        .collect();
    let Some(first) = args.first() else {
        return false;
    };
    let Ok(mut child) = Command::new(first)
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}


/// Resolve a folder link (target ending with /) against the workspace and extra folders.
fn resolve_folder_link<'a>(
    source_dir: &Path,
    target: &str,
    root: &Path,
    mounts: &'a [ResolvedMount],
    is_wiki: bool,
) -> Option<(PathBuf, Option<&'a ResolvedMount>)> {
    // Strip the anchor; keep the trailing "/" so base selection sees a bare
    // wiki "folder/" as root-relative (RFC 0013).
    let (path_part, _anchor) = crate::resolution::path::split_anchor(target);

    let resolved = resolve_explicit_path(root, source_dir, path_part, is_wiki);
    if resolved.is_dir() {
        return Some((resolved, None));
    }

    // Check in mount roots for root-relative targets (RFC 0010, 0013): try a
    // direct join to the mount path, and (when the mount has an `as`) strip
    // the `as` first.
    if is_root_relative(path_part, is_wiki) {
        let rel = path_part.trim_matches('/');
        for mount in mounts {
            let candidate = mount.path.join(rel);
            if candidate.is_dir() {
                return Some((candidate, Some(mount)));
            }
            if let Some(prefix) = &mount.r#as {
                let prefix_rel = prefix.trim_start_matches('/');
                if let Some(rest) = rel.strip_prefix(prefix_rel) {
                    let candidate = mount.path.join(rest.trim_start_matches('/'));
                    if candidate.is_dir() {
                        return Some((candidate, Some(mount)));
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
    if ctx.input.uri_resolver.is_uri_target(target) {
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
            });
            return;
        }
        if let Some((path, _mount)) = resolve_folder_link(
            doc.path.parent().unwrap_or(ctx.input.root.as_path()),
            target,
            &ctx.input.root,
            &ctx.input.mounts,
            true,
        ) {
            let (folder_part, _anchor) = crate::resolution::path::split_anchor(target);
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations: vec![ResolvedDestination {
                    path,
                    kind: DestinationKind::Directory,
                    name: folder_part.trim_end_matches('/').to_string(),
                    range: None,
                }],
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
            finalize_doc_or_attachment(ctx, target, heading, vec![dest], true);
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

    finalize_doc_or_attachment(ctx, target, heading, destinations, true);
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
    if ctx.input.uri_resolver.is_uri_target(target) {
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
            });
            return;
        }
        if let Some((path, _mount)) = resolve_folder_link(
            doc.path.parent().unwrap_or(ctx.input.root.as_path()),
            target,
            &ctx.input.root,
            &ctx.input.mounts,
            false,
        ) {
            let (folder_part, _anchor) = crate::resolution::path::split_anchor(target);
            ctx.graph.resolved_references.push(ResolvedReference {
                source_path: doc.path.clone(),
                occurrence_id: symbol.id,
                full_range: symbol.full_range,
                name_range: symbol.name_range,
                reference: reference.clone(),
                destinations: vec![ResolvedDestination {
                    path,
                    kind: DestinationKind::Directory,
                    name: folder_part.trim_end_matches('/').to_string(),
                    range: None,
                }],
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
            });
        }
        return;
    }


    let destinations = find_doc_matches(all_docs, &doc.path, &ctx.input.root, target, true, false);
    finalize_doc_or_attachment(ctx, target, anchor, destinations, false);
}

fn finalize_doc_or_attachment(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    anchor: Option<&str>,
    mut destinations: Vec<ResolvedDestination>,
    is_wiki: bool,
) {
    let doc = ctx.doc;
    let symbol = ctx.symbol;
    let reference = ctx.reference;
    if destinations.is_empty() && is_attachment_candidate_path(target) {
        let source_dir = doc.path.parent().unwrap_or(ctx.input.root.as_path());
        let path = resolve_explicit_path(&ctx.input.root, source_dir, target, is_wiki);
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
    let mut destinations = Vec::new();
    let mut seen = HashSet::new();
    for doc in docs {
        // Matching rules are single-sourced in `query::match_document_kinds`
        // (shared with the `resolve` subcommand, RFC 0012).
        let matched = !query::match_document_kinds(
            doc,
            source_dir,
            root,
            target,
            explicit_only,
            is_wiki,
        )
        .is_empty();
        if matched && seen.insert(doc.path.clone()) {
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

/// The (location, stem) of a file: its parent directory and its file stem
/// (`Path::file_stem`). Returns `None` if the path has no usable stem. Used for
/// fine-grained mount conflict detection (RFC 0011).
fn file_stem_entry(file_path: &Path) -> Option<(PathBuf, String)> {
    let location = file_path.parent()?.to_path_buf();
    let stem = file_path.file_stem()?.to_str()?;
    Some((location, stem.to_string()))
}

/// The (location, name) folder entries implied by a file's path — every
/// intermediate directory. For `notes/kb/a.md`, returns [(`notes`, `kb`),
/// (``, `notes`)]. Used for fine-grained mount conflict detection (RFC 0011).
fn folder_entries(file_path: &Path) -> Vec<(PathBuf, String)> {
    let mut entries = Vec::new();
    let mut current = file_path;
    while let Some(dir) = current.parent() {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            let location = dir.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            entries.push((location, name.to_string()));
        }
        current = dir;
    }
    entries
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
    use super::query;
    use super::conn::ResolvedDocument;
    use super::{find_doc_matches, index_document, is_explicit_path, ResolveDocument};
    use std::path::PathBuf;

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

    /// Parity guard for the `match_document_kinds` refactor (RFC 0012): for a
    /// corpus of (doc, target) pairs, the documents selected by
    /// `find_doc_matches` equal those with ≥1 match kind, in the same order.
    #[test]
    fn find_doc_matches_parity_with_match_document_kinds() {
        let parse_options = crate::parser::ParseOptions::default();
        let root = PathBuf::from("/root");
        let docs: Vec<ResolvedDocument> = [
            ("avon.md", "# Avon\n"),
            ("notes/avon.md", "# Avon 2025 Review\n"),
            ("notes/other.md", "# Avon\n"),
            ("qa/db-transfer.md", "# Team knowledge transfer (QA/DB)\n"),
        ]
        .into_iter()
        .map(|(rel, text)| {
            let path = root.join(rel);
            let rel_path = PathBuf::from(rel);
            let structure = crate::parser::parse_document(text, parse_options);
            index_document(
                &ResolveDocument::primary(path, rel_path, structure),
                &root,
            )
        })
        .collect();
        let source_dir = root.clone();

        let targets = [
            "avon", // stem of root avon.md; title of notes/other.md → two docs
            "Avon", // case-insensitive stem + title
            "avon-2025-review", // title slug of notes/avon.md
            "/notes/avon.md", // explicit workspace-absolute path
            "notes/avon", // explicit relative path without extension
            "avon.md", // explicit file-like (source-relative)
            "Team knowledge transfer (QA/DB)", // title containing `/`
            "nope", // no match
        ];
        for target in targets {
            let via_find: Vec<PathBuf> = find_doc_matches(&docs, &root.join("src.md"), &root, target, false, true)
                .into_iter()
                .map(|destination| destination.path)
                .collect();
            let via_kinds: Vec<PathBuf> = docs
                .iter()
                .filter(|doc| {
                    !query::match_document_kinds(doc, &source_dir, &root, target, false, true)
                        .is_empty()
                })
                .map(|doc| doc.path.clone())
                .collect();
            assert_eq!(via_find, via_kinds, "parity for target {target:?}");
        }
    }
}
