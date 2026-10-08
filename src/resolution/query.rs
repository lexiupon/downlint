//! Target resolution query (RFC 0012): given a link target, report every
//! destination with the reason it matched.
//!
//! This is a projection of the existing resolution rules (RES-03/04/05/06/07):
//! the per-document matching rules live here (`match_document_kinds`) and are
//! shared with `find_doc_matches` (link resolution), so the `resolve`
//! subcommand predicts `check`'s behavior by construction.

use crate::resolution::auto_verify::{AutoVerifyOutcome, classify};
use crate::resolution::conn::ResolvedDocument;
use crate::resolution::path::{
    is_external_web_scheme, is_folder_link_target, is_root_relative,
    path_md_optional_eq, path_without_extension, percent_decode, resolve_explicit_path, scheme_of,
    split_anchor,
};
use crate::resolution::slug::Slug;
use crate::resolution::uri::UriOutcome;
use crate::resolution::{ResolveInput, is_attachment_candidate_path, is_explicit_path};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The rules by which a document matches a target (RES-03, LNK-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    /// Path-based match (explicit path, or relative path for a bare target).
    Path,
    /// File stem match (ASCII case-insensitive).
    Stem,
    /// Title-slug match (H1, RES-01).
    Title,
    /// Prefix match (RES-05).
    Prefix,
    /// Filesystem attachment fallback (RES-06).
    Attachment,
    /// Folder link target (RES-04).
    Directory,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Path => "path",
            MatchKind::Stem => "stem",
            MatchKind::Title => "title",
            MatchKind::Prefix => "prefix",
            MatchKind::Attachment => "attachment",
            MatchKind::Directory => "directory",
        }
    }
}

/// One destination of a target query.
#[derive(Clone, Debug)]
pub struct TargetDestination {
    /// Namespace-relative path for documents (primary: workspace-relative;
    /// mounted: `prefix/rel`); workspace-relative filesystem path for
    /// attachments; resolution-base-relative path for directories.
    pub path: PathBuf,
    /// The document's title (H1 when `core.title_from_heading` is on, else
    /// the stem); `None` for attachments and directories.
    pub title: Option<String>,
    /// The rules that matched, in canonical order
    /// (`path`, `stem`, `title`, `prefix`).
    pub match_kinds: Vec<MatchKind>,
    /// Mount attribution for mounted documents; `None` for primary docs,
    /// attachments, and directories.
    pub mount: Option<String>,
    /// Anchor existence for document destinations; `None` when no anchor was
    /// given or the destination is not a document.
    pub anchor: Option<bool>,
}

/// Outcome status of a target query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveStatus {
    /// Exactly one destination (document, attachment, or directory).
    Resolved,
    /// More than one destination.
    Ambiguous,
    /// No destination.
    Broken,
    /// External web scheme — resolves to itself, outside the workspace.
    External,
    /// Non-web scheme, no `[[schemas]]` prefix matched.
    Unmapped,
    /// Mapped, file exists, not a placeholder.
    MappedPresent,
    /// Mapped, file does not exist.
    MappedMissing,
    /// Mapped, file is an evicted cloud placeholder.
    MappedPlaceholder,
}

impl ResolveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolveStatus::Resolved => "resolved",
            ResolveStatus::Ambiguous => "ambiguous",
            ResolveStatus::Broken => "broken",
            ResolveStatus::External => "external",
            ResolveStatus::Unmapped => "unmapped",
            ResolveStatus::MappedPresent => "mapped-present",
            ResolveStatus::MappedMissing => "mapped-missing",
            ResolveStatus::MappedPlaceholder => "mapped-placeholder",
        }
    }

    /// Exit code contract: `0` iff the target is safe to use as a link as-is.
    pub fn exit_code(self) -> i32 {
        match self {
            ResolveStatus::Resolved | ResolveStatus::External | ResolveStatus::MappedPresent => 0,
            _ => 1,
        }
    }
}

/// Mapping report for URI-scheme targets.
#[derive(Clone, Debug)]
pub struct SchemeReport {
    pub scheme: String,
    /// The matched `[[schemas]]` prefix (`None` when unmapped/external).
    pub prefix: Option<String>,
    /// The mapped absolute filesystem path (`None` when unmapped/external).
    pub mapped_path: Option<PathBuf>,
    pub exists: Option<bool>,
    pub placeholder: Option<bool>,
    /// `verify_cmd` state: `"not-applicable"` (no mapping), `"not-configured"`
    /// (no `verify_cmd`), `"skipped"` (configured but not run — missing
    /// `--allow-uri-sync` or missing file), `"passed"`, or `"failed"`.
    pub verify: &'static str,
}

/// The result of a target query.
#[derive(Clone, Debug)]
pub struct TargetResolution {
    pub target: String,
    /// The anchor part of the target, if any.
    pub anchor: Option<String>,
    pub status: ResolveStatus,
    pub destinations: Vec<TargetDestination>,
    /// Advisory prefix candidates (only under `--include-prefix` when
    /// `wiki.obsidian_prefix` is off). Never affect `status`.
    pub prefix_candidates: Vec<TargetDestination>,
    /// Mapping report for URI targets; `None` for non-URI targets.
    pub scheme: Option<SchemeReport>,
    /// True when the target carried an anchor that is not supported (URI
    /// targets; `check` reports `link/broken-anchor` for such links).
    pub anchor_unsupported: bool,
    /// Existing local directory missing a trailing slash (RFC 0024).
    pub directory_hint: Option<String>,
}

/// Per-document matching rules (RES-03, LNK-02) — the single source of truth
/// shared by `find_doc_matches` (link resolution) and `resolve_target` (the
/// `resolve` subcommand).
///
/// Returns the kinds of rules by which `doc` matches `target`; empty when
/// there is no match.
pub fn match_document_kinds(
    doc: &ResolvedDocument,
    source_dir: &Path,
    root: &Path,
    target: &str,
    explicit_only: bool,
    is_wiki: bool,
) -> Vec<MatchKind> {
    let target_slug = Slug::from_heading_text(target);

    // `namespace_rel_path` equals `rel_path` for primary docs and is
    // `prefix/rel` (or `rel`) for mounted docs, so matching on it is co-equal
    // across primary + mounted docs (RFC 0010).
    let ns_no_ext = path_without_extension(&doc.namespace_rel_path);

    if explicit_only || is_explicit_path(target) {
        // Path-based matching (RFC 0013): the resolved candidate (correct base
        // per `resolution_base`, `.`/`..` normalized) is compared against the
        // document's filesystem path, with the `.md` suffix optional for wiki
        // targets. Root-relative targets additionally match the document's
        // namespace path, which is how a mount `prefix` (a virtual directory at
        // the workspace root) is reached (RFC 0010).
        let explicit_path = resolve_explicit_path(root, source_dir, target, is_wiki);
        let decoded = percent_decode(target);
        let mut kinds = Vec::new();
        let fs_match = path_md_optional_eq(
            &doc.path.to_string_lossy(),
            &explicit_path.to_string_lossy(),
            is_wiki,
        );
        let ns_match = if is_root_relative(&decoded, is_wiki) {
            let target_ns = decoded.trim_start_matches('/');
            path_md_optional_eq(&doc.namespace_rel_path.to_string_lossy(), target_ns, is_wiki)
        } else {
            false
        };
        if fs_match || ns_match {
            kinds.push(MatchKind::Path);
        }
        // A wiki target containing `/` may actually be a title (e.g.
        // "Team knowledge transfer (QA/DB)"); fall back to title-slug
        // matching, co-equal across primary + mounted docs. Markdown links
        // are paths only (no title fallback).
        if is_wiki && doc.title_slug == target_slug {
            kinds.push(MatchKind::Title);
        }
        kinds
    } else {
        let mut kinds = Vec::new();
        let stem_match = doc.file_stem.eq_ignore_ascii_case(target);
        let title_match = doc.title_slug == target_slug;
        let path_match = ns_no_ext.eq_ignore_ascii_case(target)
            || doc
                .namespace_rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .eq_ignore_ascii_case(target);
        if stem_match {
            // A bare target that also equals a root-level document's relative
            // path is explained by the stem; don't double-report `path`.
            kinds.push(MatchKind::Stem);
        } else if path_match {
            kinds.push(MatchKind::Path);
        }
        if title_match {
            kinds.push(MatchKind::Title);
        }
        kinds
    }
}

/// Whether `anchor` resolves inside `doc` — the same lookup the in-page /
/// cross-document anchor rules use: heading slug (strict, then the tolerant
/// folded-slug fallback, per RES-01), or tag (ASCII case-insensitive raw
/// anchor).
fn anchor_exists_in(doc: &ResolvedDocument, anchor: &str) -> bool {
    let slug = Slug::from_heading_text(anchor);
    if doc.headings.contains_key(&slug) {
        return true;
    }
    let folded = slug.folded();
    if folded != slug && doc.headings.contains_key(&folded) {
        return true;
    }
    doc.tags.contains_key(&anchor.to_ascii_lowercase())
}

/// Resolve a link target against the workspace (RFC 0012).
///
/// `docs` is the indexed document set (primary + mounted, as produced by
/// `index_document`); `source_dir` is the resolution base for relative
/// targets (the containing document's directory, or the workspace root).
/// `include_prefix` lists prefix candidates when `wiki.obsidian_prefix` is
/// off (advisory only); `allow_sync` gates `verify_cmd` execution for URI
/// targets.
pub fn resolve_target(
    input: &ResolveInput,
    docs: &[ResolvedDocument],
    source_dir: &Path,
    target: &str,
    include_prefix: bool,
    allow_sync: bool,
) -> TargetResolution {
    // 1. Scheme targets (RES-07).
    if input.uri_resolver.is_uri_target(target) {
        return resolve_uri_target(input, target, allow_sync);
    }

    let (path_part, anchor) = split_anchor(target);
    let anchor = anchor.map(str::to_string);

    // 2. Folder targets (RES-04).
    if is_folder_link_target(target) {
        if anchor.is_some() {
            // Folder links do not take headings (RES-04).
            return TargetResolution {
                target: target.to_string(),
                anchor,
                status: ResolveStatus::Broken,
                destinations: Vec::new(),
                prefix_candidates: Vec::new(),
                scheme: None,
                anchor_unsupported: false,
                directory_hint: None,
            };
        }
        if let Some((fs_path, mount)) =
            super::resolve_folder_link(source_dir, target, &input.root, &input.mounts, true)
        {
            let base = mount
                .map(|m| m.path.as_path())
                .unwrap_or(input.root.as_path());
            let mount_attribution = mount.map(|m| m.attribution.clone());
            return TargetResolution {
                target: target.to_string(),
                anchor,
                status: ResolveStatus::Resolved,
                destinations: vec![directory_destination(&fs_path, base, mount_attribution)],
                prefix_candidates: Vec::new(),
                scheme: None,
                anchor_unsupported: false,
                directory_hint: None,
            };
        }
        return TargetResolution {
            target: target.to_string(),
            anchor,
            status: ResolveStatus::Broken,
            destinations: Vec::new(),
            prefix_candidates: Vec::new(),
            scheme: None,
            anchor_unsupported: false,
            directory_hint: None,
        };
    }

    // 3. Document / attachment.
    let explicit = is_explicit_path(path_part);
    let mut destinations: Vec<TargetDestination> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for doc in docs {
        let kinds = match_document_kinds(doc, source_dir, &input.root, path_part, explicit, true);
        if kinds.is_empty() || !seen.insert(doc.path.clone()) {
            continue;
        }
        destinations.push(TargetDestination {
            path: doc.namespace_rel_path.clone(),
            title: Some(doc.title_text.clone()),
            match_kinds: kinds,
            mount: doc.mount.clone(),
            anchor: anchor.as_ref().map(|a| anchor_exists_in(doc, a)),
        });
    }

    // Attachment fallback (RES-06): file-like targets only, after document
    // resolution.
    if destinations.is_empty() && is_attachment_candidate_path(path_part) {
        let fs_path = resolve_explicit_path(&input.root, source_dir, path_part, true);
        if fs_path.is_dir() {
            return TargetResolution {
                target: target.to_string(),
                directory_hint: Some(super::path::directory_link_hint(path_part, anchor.as_deref())),
                anchor,
                status: ResolveStatus::Broken,
                destinations: Vec::new(),
                prefix_candidates: Vec::new(),
                scheme: None,
                anchor_unsupported: false,
            };
        }
        if fs_path.is_file() {
            destinations.push(TargetDestination {
                path: fs_path
                    .strip_prefix(&input.root)
                    .unwrap_or(&fs_path)
                    .to_path_buf(),
                title: None,
                match_kinds: vec![MatchKind::Attachment],
                mount: None,
                anchor: None,
            });
        }
    }

    // Prefix matching (RES-05): a fallback that runs only when the exact,
    // stem, title, and path matches found nothing; never for explicit targets.
    let mut prefix_candidates: Vec<TargetDestination> = Vec::new();
    if destinations.is_empty() && !explicit && !path_part.is_empty() {
        let prefix_enabled = input.config.wiki.obsidian_prefix;
        if prefix_enabled || include_prefix {
            for path in input.prefix_index.matches(path_part) {
                let Some(doc) = docs.iter().find(|d| &d.path == path) else {
                    continue;
                };
                let dest = TargetDestination {
                    path: doc.namespace_rel_path.clone(),
                    title: Some(doc.title_text.clone()),
                    match_kinds: vec![MatchKind::Prefix],
                    mount: doc.mount.clone(),
                    anchor: anchor.as_ref().map(|a| anchor_exists_in(doc, a)),
                };
                if prefix_enabled {
                    destinations.push(dest);
                } else {
                    prefix_candidates.push(dest);
                }
            }
        }
    }

    let status = match destinations.len() {
        0 => ResolveStatus::Broken,
        1 => ResolveStatus::Resolved,
        _ => ResolveStatus::Ambiguous,
    };

    TargetResolution {
        target: target.to_string(),
        anchor,
        status,
        destinations,
        prefix_candidates,
        scheme: None,
        anchor_unsupported: false,
        directory_hint: None,
    }
}

fn directory_destination(fs_path: &Path, base: &Path, mount: Option<String>) -> TargetDestination {
    TargetDestination {
        path: fs_path.strip_prefix(base).unwrap_or(fs_path).to_path_buf(),
        title: None,
        match_kinds: vec![MatchKind::Directory],
        mount,
        anchor: None,
    }
}

/// URI-scheme target query (RES-07): rewrite + stat + verify, reported as a
/// mapping status instead of a diagnostic.
fn resolve_uri_target(input: &ResolveInput, target: &str, allow_sync: bool) -> TargetResolution {
    let (path_part, anchor) = split_anchor(target);
    let anchor = anchor.map(str::to_string);
    let scheme = scheme_of(path_part).unwrap_or_default().to_string();
    let anchor_unsupported = anchor.is_some();

    // External web URLs resolve to themselves, outside the workspace (LNK-03).
    if is_external_web_scheme(&scheme) {
        return TargetResolution {
            target: target.to_string(),
            anchor,
            status: ResolveStatus::External,
            destinations: Vec::new(),
            prefix_candidates: Vec::new(),
            scheme: Some(SchemeReport {
                scheme,
                prefix: None,
                mapped_path: None,
                exists: None,
                placeholder: None,
                verify: "not-applicable",
            }),
            anchor_unsupported,
            directory_hint: None,
        };
    }

    let unmapped = |scheme: String| TargetResolution {
        target: target.to_string(),
        anchor: anchor.clone(),
        status: ResolveStatus::Unmapped,
        destinations: Vec::new(),
        prefix_candidates: Vec::new(),
        scheme: Some(SchemeReport {
            scheme,
            prefix: None,
            mapped_path: None,
            exists: None,
            placeholder: None,
            verify: "not-applicable",
        }),
        anchor_unsupported,
        directory_hint: None,
    };

    match input.uri_resolver.resolve(path_part) {
        UriOutcome::NotApplicable => unmapped(scheme),
        UriOutcome::NoMapping { .. } => unmapped(scheme),
        UriOutcome::Resolved {
            mapping_index,
            resolved_path,
            ..
        } => {
            let exists = resolved_path.is_file();
            let (placeholder, verify) = if exists {
                if let Some(cmd) = input.uri_resolver.verify_cmd_for(mapping_index) {
                    if allow_sync {
                        let passed = crate::resolution::run_verify_cmd(cmd, &resolved_path);
                        (!passed, if passed { "passed" } else { "failed" })
                    } else {
                        // verify_cmd configured but --allow-uri-sync not
                        // passed: inconclusive (not a placeholder).
                        (false, "skipped")
                    }
                } else {
                    let outcome = classify(
                        &resolved_path,
                        input.uri_resolver.auto_verify_for(mapping_index),
                    );
                    (outcome == AutoVerifyOutcome::Placeholder, "not-configured")
                }
            } else {
                // Missing file: no verification runs.
                (
                    false,
                    if input.uri_resolver.verify_cmd_for(mapping_index).is_some() {
                        "skipped"
                    } else {
                        "not-configured"
                    },
                )
            };

            let status = if !exists {
                ResolveStatus::MappedMissing
            } else if placeholder {
                ResolveStatus::MappedPlaceholder
            } else {
                ResolveStatus::MappedPresent
            };

            TargetResolution {
                target: target.to_string(),
                anchor,
                status,
                destinations: Vec::new(),
                prefix_candidates: Vec::new(),
                scheme: Some(SchemeReport {
                    scheme,
                    prefix: Some(input.uri_resolver.prefix_for(mapping_index).to_string()),
                    mapped_path: Some(resolved_path),
                    exists: Some(exists),
                    placeholder: Some(placeholder),
                    verify,
                }),
                anchor_unsupported,
                directory_hint: None,
            }
        }
    }
}
