use crate::parser::{LinkLabel, Ref, Structure};
use crate::resolution::Slug;
use crate::utils::ByteRange;
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
}

#[derive(Clone, Debug, Default)]
pub struct ConnectionGraph {
    pub documents: Vec<ResolvedDocument>,
    pub resolved_references: Vec<ResolvedReference>,
    pub unresolved_references: Vec<UnresolvedReference>,
    pub ambiguous_references: Vec<AmbiguousReference>,
}

impl ConnectionGraph {
    pub fn document_for_path(&self, path: &PathBuf) -> Option<&ResolvedDocument> {
        self.documents.iter().find(|doc| &doc.path == path)
    }
}
