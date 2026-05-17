use crate::resolution::Slug;
use crate::utils::ByteRange;

pub type OccurrenceId = u32;
pub type ElementIdx = usize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node<T> {
    pub id: OccurrenceId,
    pub text: String,
    pub range: ByteRange,
    pub data: T,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextNode {
    pub text: String,
    pub range: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedNode {
    pub raw: String,
    pub decoded: String,
    pub range: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heading {
    pub level: u8,
    pub is_title: bool,
    pub title: TextNode,
    pub slug: Slug,
    pub disambiguation: Option<String>,
    pub scope: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WikiLink {
    pub doc: Option<EncodedNode>,
    pub heading: Option<EncodedNode>,
    pub title: Option<EncodedNode>,
    pub is_embed: bool,
    pub doc_range: Option<ByteRange>,
    pub heading_range: Option<ByteRange>,
    pub title_range: Option<ByteRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MdLink {
    Inline {
        text: TextNode,
        dest: TextNode,
        title: Option<TextNode>,
        anchor_range: Option<ByteRange>,
        is_image: bool,
    },
    Full {
        text: TextNode,
        label: TextNode,
    },
    Collapsed {
        label: TextNode,
    },
    Shortcut {
        label: TextNode,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MdLinkDef {
    pub label: TextNode,
    pub url: EncodedNode,
    pub title: Option<TextNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub name: TextNode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CstElement {
    H(Node<Heading>),
    WL(Node<WikiLink>),
    ML(Node<MdLink>),
    MLD(Node<MdLinkDef>),
    T(Node<Tag>),
    Yml(TextNode),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Cst {
    pub elements: Vec<CstElement>,
    pub child_map: std::collections::HashMap<ElementIdx, Vec<ElementIdx>>,
}
