use crate::parser::{LinkLabel, Ref, Structure};
use crate::resolution::Slug;
use crate::utils::{ByteRange, MountConflict};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum DestinationKind {
    Document,
    Heading,
    LinkDefinition,
    Tag,
    Attachment,
    Directory,
}

#[derive(Clone, Debug)]
pub struct ResolvedDestination {
    pub path: PathBuf,
    pub kind: DestinationKind,
    pub name: String,
    pub range: Option<ByteRange>,
}

#[derive(Clone, Debug)]
pub struct ResolvedReference {
    pub source_path: PathBuf,
    pub occurrence_id: u32,
    pub full_range: ByteRange,
    pub name_range: Option<ByteRange>,
    pub reference: Ref,
    pub destinations: Vec<ResolvedDestination>,
}

#[derive(Clone, Debug)]
pub struct UnresolvedReference {
    pub source_path: PathBuf,
    pub occurrence_id: u32,
    pub full_range: ByteRange,
    pub name_range: Option<ByteRange>,
    pub reference: Ref,
    pub target: String,
    /// True when this unresolved reference was an in-page anchor (`[text](#foo)`,
    /// `[[#foo]]`, or `[text](file.md#foo)`) that failed to resolve to a heading.
    /// The diagnostic rule uses this to emit link/broken-anchor (Broken anchor) instead of
    /// link/broken (Broken link) so the user gets an accurate diagnosis.
    pub is_anchor: bool,
    /// Optional list of file paths whose stems begin with `target`. Populated by the
    /// resolution layer when an opt-in prefix index is available; rendered as a
    /// discoverability hint in the link/broken diagnostic.
    pub hint_payload: Option<Vec<PathBuf>>,
    /// True when the link target has a URI scheme but no `[[schemas]]`
    /// entry matched it. The diagnostics layer renders a one-time hint
    /// pointing the user at `.downlint.toml`'s `[[schemas]]` section. Suppressed
    /// entirely when `[[schemas]]` is not configured (so we never change
    /// behavior for users who haven't opted in) and when `UriOptions::no_hints`
    /// is true.
    pub uri_no_mapping_hint: bool,
}

#[derive(Clone, Debug)]
pub struct AmbiguousReference {
    pub source_path: PathBuf,
    pub occurrence_id: u32,
    pub full_range: ByteRange,
    pub name_range: Option<ByteRange>,
    pub reference: Ref,
    pub target: String,
    pub destinations: Vec<ResolvedDestination>,
}

#[derive(Clone, Debug)]
pub struct ResolvedDocument {
    pub path: PathBuf,
    pub rel_path: PathBuf,
    pub structure: Structure,
    pub title_slug: Slug,
    pub title_text: String,
    pub file_stem: String,
    pub link_defs: HashMap<LinkLabel, Vec<ResolvedDestination>>,
    pub headings: HashMap<Slug, Vec<ResolvedDestination>>,
    pub tags: HashMap<String, Vec<ResolvedDestination>>,
    /// The document's address in the combined namespace (RFC 0010). For a
    /// primary doc this equals `rel_path`; for a mounted doc it is
    /// `as/rel_path` (when `as` is set) or `rel_path` (otherwise).
    /// Path-based wiki targets are matched against this, co-equal across
    /// primary and mounted docs.
    pub namespace_rel_path: PathBuf,
    /// Attribution for diagnostics emitted from a mounted doc: the mount's
    /// `as` (or `path` when there is no `as`). `None` for primary docs.
    pub mount: Option<String>,
    /// Whether this document's own links are linted. Primary docs are always
    /// sources; mounted docs are sources only when their mount has `lint = true`.
    pub is_source: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ConnectionGraph {
    pub documents: Vec<ResolvedDocument>,
    pub resolved_references: Vec<ResolvedReference>,
    pub unresolved_references: Vec<UnresolvedReference>,
    pub ambiguous_references: Vec<AmbiguousReference>,
    /// Namespace-level mount conflicts detected at startup (RFC 0010).
    pub conflicts: Vec<MountConflict>,
}

impl ConnectionGraph {
    pub fn document_for_path(&self, path: &PathBuf) -> Option<&ResolvedDocument> {
        self.documents.iter().find(|doc| &doc.path == path)
    }
}
