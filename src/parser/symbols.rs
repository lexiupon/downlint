use crate::parser::ast::{Ast, AstElement, AstIdx};
use crate::parser::cst::{EncodedNode, Heading, MdLink, OccurrenceId, WikiLink};
use crate::resolution::Slug;
use crate::utils::ByteRange;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LinkLabel(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolOccurrence {
    pub id: OccurrenceId,
    pub kind: SymKind,
    pub full_range: ByteRange,
    pub name_range: Option<ByteRange>,
    pub ast_idx: Option<AstIdx>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SymKind {
    Def(Def),
    Ref(Ref),
    Tag(TagSym),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Def {
    Doc,
    Title(String),
    Header(u8, Slug),
    LinkDef(LinkLabel),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ref {
    Wiki {
        target: String,
        heading: Option<String>,
        is_embed: bool,
    },
    Inline {
        target: String,
        anchor: Option<String>,
        is_image: bool,
    },
    Full {
        text: String,
        label: LinkLabel,
    },
    Collapsed {
        label: LinkLabel,
    },
    Shortcut {
        label: LinkLabel,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagSym {
    pub name: String,
}

pub fn build_symbols(ast: &Ast) -> Vec<SymbolOccurrence> {
    let mut symbols = Vec::new();
    symbols.push(SymbolOccurrence {
        id: 0,
        kind: SymKind::Def(Def::Doc),
        full_range: ByteRange::new(0, 0),
        name_range: None,
        ast_idx: None,
    });

    for (idx, element) in ast.elements.iter().enumerate() {
        match element {
            AstElement::H(heading) => {
                symbols.push(build_heading_symbol(heading, idx));
            }
            AstElement::WL(link) => {
                symbols.push(build_wiki_symbol(link, idx));
            }
            AstElement::ML(link) => {
                symbols.push(build_md_symbol(link, idx));
            }
            AstElement::MLD(link_def) => {
                symbols.push(SymbolOccurrence {
                    id: idx as OccurrenceId + 1,
                    kind: SymKind::Def(Def::LinkDef(LinkLabel(normalize_label(
                        &link_def.label.text,
                    )))),
                    full_range: link_def.label.range,
                    name_range: Some(link_def.label.range),
                    ast_idx: Some(idx),
                });
            }
            AstElement::T(tag) => {
                symbols.push(SymbolOccurrence {
                    id: idx as OccurrenceId + 1,
                    kind: SymKind::Tag(TagSym {
                        name: tag.name.text.to_ascii_lowercase(),
                    }),
                    full_range: tag.name.range,
                    name_range: Some(tag.name.range),
                    ast_idx: Some(idx),
                });
            }
        }
    }

    symbols
}

fn build_heading_symbol(heading: &Heading, idx: usize) -> SymbolOccurrence {
    let title = heading.title.text.clone();
    let kind = if heading.is_title {
        SymKind::Def(Def::Title(title))
    } else {
        SymKind::Def(Def::Header(heading.level, heading.slug.clone()))
    };

    SymbolOccurrence {
        id: idx as OccurrenceId + 1,
        kind,
        full_range: heading.scope,
        name_range: Some(heading.title.range),
        ast_idx: Some(idx),
    }
}

fn build_wiki_symbol(link: &WikiLink, idx: usize) -> SymbolOccurrence {
    SymbolOccurrence {
        id: idx as OccurrenceId + 1,
        kind: SymKind::Ref(Ref::Wiki {
            target: link.doc.as_ref().map(decode).unwrap_or_default(),
            heading: link.heading.as_ref().map(decode),
            is_embed: link.is_embed,
        }),
        full_range: match (&link.doc_range, &link.heading_range, &link.title_range) {
            (Some(range), _, _) => *range,
            (None, Some(range), _) => *range,
            (None, None, Some(range)) => *range,
            _ => ByteRange::new(0, 0),
        },
        name_range: link.doc_range.or(link.heading_range),
        ast_idx: Some(idx),
    }
}

fn build_md_symbol(link: &MdLink, idx: usize) -> SymbolOccurrence {
    let (kind, full_range, name_range) = match link {
        MdLink::Inline {
            dest,
            anchor_range,
            is_image,
            ..
        } => {
            let (target, anchor) = split_dest(&dest.text);
            (
                SymKind::Ref(Ref::Inline {
                    target,
                    anchor,
                    is_image: *is_image,
                }),
                dest.range,
                anchor_range.or(Some(dest.range)),
            )
        }
        MdLink::Full { text, label } => (
            SymKind::Ref(Ref::Full {
                text: text.text.clone(),
                label: LinkLabel(normalize_label(&label.text)),
            }),
            label.range,
            Some(label.range),
        ),
        MdLink::Collapsed { label } => (
            SymKind::Ref(Ref::Collapsed {
                label: LinkLabel(normalize_label(&label.text)),
            }),
            label.range,
            Some(label.range),
        ),
        MdLink::Shortcut { label } => (
            SymKind::Ref(Ref::Shortcut {
                label: LinkLabel(normalize_label(&label.text)),
            }),
            label.range,
            Some(label.range),
        ),
    };

    SymbolOccurrence {
        id: idx as OccurrenceId + 1,
        kind,
        full_range,
        name_range,
        ast_idx: Some(idx),
    }
}

fn decode(node: &EncodedNode) -> String {
    node.decoded.clone()
}

pub fn normalize_label(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

pub fn split_dest(dest: &str) -> (String, Option<String>) {
    if let Some((path, anchor)) = dest.split_once('#') {
        (path.to_string(), Some(anchor.to_string()))
    } else {
        (dest.to_string(), None)
    }
}
