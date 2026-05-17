pub mod ast;
pub mod comrak;
pub mod cst;
pub mod scanner;
pub mod symbols;

use crate::parser::ast::{Ast, AstElement};
use crate::parser::cst::{Cst, CstElement, Heading, Node, OccurrenceId, TextNode};
use crate::parser::scanner::scan_document;
use crate::parser::symbols::{SymbolOccurrence, build_symbols};
use crate::resolution::Slug;
use crate::utils::ByteRange;
use std::collections::HashMap;

pub use ast::AstIdx;
pub use cst::{ElementIdx, EncodedNode, MdLink, MdLinkDef, Tag, WikiLink};
pub use symbols::{Def, LinkLabel, Ref, SymKind, TagSym};

#[derive(Clone, Debug)]
pub struct ParseOptions {
    pub title_from_heading: bool,
    pub heading_ids: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            title_from_heading: true,
            heading_ids: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub titles: Vec<Heading>,
    pub headings: Vec<Heading>,
    pub headings_by_slug: HashMap<Slug, Vec<Heading>>,
    pub wiki_links: Vec<WikiLink>,
    pub md_links: Vec<MdLink>,
    pub link_defs: Vec<MdLinkDef>,
    pub tags: Vec<Tag>,
    pub yaml_front_matter: Option<TextNode>,
}

#[derive(Clone, Debug)]
pub struct Structure {
    pub cst: Cst,
    pub ast: Ast,
    pub symbols: Vec<SymbolOccurrence>,
    pub index: Index,
    pub text: crate::utils::Text,
}

pub fn parse_document(input: &str, options: ParseOptions) -> Structure {
    let text = crate::utils::Text::new(input.to_string());
    let mut next_id: OccurrenceId = 1;
    let mut elements = parse_headings(input, &options, &mut next_id);
    let scanned = scan_document(input, &mut next_id);
    if let Some(frontmatter) = scanned.frontmatter.clone() {
        elements.push(CstElement::Yml(frontmatter));
    }
    elements.extend(scanned.elements);
    elements.sort_by_key(|element| match element {
        CstElement::H(node) => node.range.start,
        CstElement::WL(node) => node.range.start,
        CstElement::ML(node) => node.range.start,
        CstElement::MLD(node) => node.range.start,
        CstElement::T(node) => node.range.start,
        CstElement::Yml(node) => node.range.start,
    });

    let cst = Cst {
        elements: elements.clone(),
        child_map: HashMap::new(),
    };
    let ast = build_ast(&elements);
    let symbols = build_symbols(&ast);
    let index = build_index(&ast, scanned.frontmatter);

    Structure {
        cst,
        ast,
        symbols,
        index,
        text,
    }
}

fn parse_headings(
    input: &str,
    options: &ParseOptions,
    next_id: &mut OccurrenceId,
) -> Vec<CstElement> {
    let mut elements = Vec::new();
    let mut offset = 0usize;
    let mut disambiguation: HashMap<String, usize> = HashMap::new();
    let mut in_fence = false;
    let mut fence_marker = "";

    for line in input.split_inclusive(['\n', '\r']) {
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if in_fence && trimmed.starts_with(fence_marker) {
                in_fence = false;
            } else if !in_fence {
                in_fence = true;
                fence_marker = if trimmed.starts_with("```") {
                    "```"
                } else {
                    "~~~"
                };
            }
            offset += line.len();
            continue;
        }
        if in_fence {
            offset += line.len();
            continue;
        }

        let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
        if !(1..=6).contains(&hashes) {
            offset += line.len();
            continue;
        }
        if trimmed.chars().nth(hashes) != Some(' ')
            && trimmed.chars().nth(hashes) != Some('\u{00a0}')
        {
            offset += line.len();
            continue;
        }

        let content_start = trimmed
            .char_indices()
            .nth(hashes + 1)
            .map(|(idx, _)| idx)
            .unwrap_or(trimmed.len());
        let title_text = trimmed[content_start..].trim().trim_end_matches('#').trim();
        let title_start = content.find(title_text).unwrap_or(hashes + 1);
        let title_range = ByteRange::new(
            offset + title_start,
            offset + title_start + title_text.len(),
        );
        let full_range = ByteRange::new(offset, offset + content.len());

        let base_slug = Slug::from_heading_text(title_text);
        let counter = disambiguation
            .entry(base_slug.as_str().to_string())
            .or_insert(0);
        let slug = if options.heading_ids && *counter > 0 {
            Slug::with_suffix(&base_slug, *counter)
        } else {
            base_slug.clone()
        };
        let suffix = if options.heading_ids && *counter > 0 {
            Some(counter.to_string())
        } else {
            None
        };
        *counter += 1;

        let heading = Heading {
            level: hashes as u8,
            is_title: options.title_from_heading && hashes == 1,
            title: TextNode {
                text: title_text.to_string(),
                range: title_range,
            },
            slug,
            disambiguation: suffix,
            scope: full_range,
        };
        elements.push(CstElement::H(Node {
            id: *next_id,
            text: content.to_string(),
            range: full_range,
            data: heading,
        }));
        *next_id += 1;
        offset += line.len();
    }

    elements
}

fn build_ast(elements: &[CstElement]) -> Ast {
    let mut ast = Vec::new();
    for element in elements {
        match element {
            CstElement::H(node) => ast.push(AstElement::H(node.data.clone())),
            CstElement::WL(node) => ast.push(AstElement::WL(node.data.clone())),
            CstElement::ML(node) => ast.push(AstElement::ML(node.data.clone())),
            CstElement::MLD(node) => ast.push(AstElement::MLD(node.data.clone())),
            CstElement::T(node) => ast.push(AstElement::T(node.data.clone())),
            CstElement::Yml(_) => {}
        }
    }
    Ast { elements: ast }
}

fn build_index(ast: &Ast, frontmatter: Option<TextNode>) -> Index {
    let mut index = Index {
        yaml_front_matter: frontmatter,
        ..Index::default()
    };

    for element in &ast.elements {
        match element {
            AstElement::H(heading) => {
                if heading.is_title {
                    index.titles.push(heading.clone());
                }
                index
                    .headings_by_slug
                    .entry(heading.slug.clone())
                    .or_default()
                    .push(heading.clone());
                index.headings.push(heading.clone());
            }
            AstElement::WL(link) => index.wiki_links.push(link.clone()),
            AstElement::ML(link) => index.md_links.push(link.clone()),
            AstElement::MLD(link_def) => index.link_defs.push(link_def.clone()),
            AstElement::T(tag) => index.tags.push(tag.clone()),
        }
    }

    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading_and_reference_symbols() {
        let structure = parse_document("# Title\n[[doc]]\n[ref]: a.md", ParseOptions::default());
        assert_eq!(structure.index.headings.len(), 1);
        assert_eq!(structure.index.wiki_links.len(), 1);
        assert_eq!(structure.index.link_defs.len(), 1);
    }
}
