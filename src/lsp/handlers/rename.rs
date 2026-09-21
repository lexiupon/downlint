//! LSP `textDocument/prepareRename` and `textDocument/rename` handlers —
//! RFC 0009 §"LSP Wire Surface".
//!
//! Both handlers are **string-only** by design — they do not consult the
//! `ConnectionGraph`, do not move files, and do not run the blocking rule.
//! The full safety machinery lives behind the `refactor.rename.*` code
//! actions (see [`super::code_action`]).
//!
//! ## Why string-only
//!
//! The cursor on `[[report]]` is fundamentally ambiguous — it could be a
//! filename stem, an H1 title, a prefix of multiple files, or unresolved.
//! `textDocument/rename` (F2) cannot ask the user which interpretation
//! they meant. Silently moving a file on F2 is dangerous. The string-only
//! default keeps F2 safe and surfaces file moves behind explicit code
//! actions where the user picks the operation from a menu.
//!
//! ## prepareRename
//!
//! Returns a non-null range when the cursor is on a link target string or
//! a heading text occurrence. Returns `null` everywhere else.
//!
//! ## rename
//!
//! Returns a single `TextEdit` replacing the string at the prepared range.

use crate::parser::cst::{CstElement, MdLink, WikiLink};
use crate::resolution::ConnectionGraph;
use crate::utils::{ByteRange, Text};
use serde_json::{Value, json};
use std::path::PathBuf;

/// What `prepareRename` found at the cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareRenameHit {
    /// Cursor on a link target (wiki-link or markdown-link URL portion).
    /// The range covers the target string, excluding `|alias` and `#heading`.
    LinkTarget {
        range: ByteRange,
        /// The string the range covers — what F2 would replace.
        current: String,
    },
    /// Cursor on a heading text occurrence (H1..H6).
    Heading {
        range: ByteRange,
        current: String,
    },
}

/// `prepareRename` — return the rename range for the element under the
/// cursor. Returns `None` when nothing renameable is there.
///
/// String-only: no resolution, no graph consultation. See module docs.
pub fn prepare_rename(
    graph: &ConnectionGraph,
    path: &PathBuf,
    text: &Text,
    offset: usize,
) -> Option<PrepareRenameHit> {
    let document = graph.documents.iter().find(|doc| &doc.path == path)?;
    for element in &document.structure.cst.elements {
        match element {
            CstElement::WL(node) if node.range.contains(offset) => {
                if let Some(hit) = wiki_link_target_hit(&node.data, offset) {
                    return Some(PrepareRenameHit::LinkTarget {
                        range: hit,
                        current: text.slice(hit).to_string(),
                    });
                }
            }
            CstElement::ML(node) if node.range.contains(offset) => {
                if let Some(hit) = md_link_target_hit(&node.data, offset) {
                    return Some(PrepareRenameHit::LinkTarget {
                        range: hit,
                        current: text.slice(hit).to_string(),
                    });
                }
            }
            CstElement::MLD(node) if node.range.contains(offset) => {
                // Reference definition: the URL portion is renameable.
                let url_range = node.data.url.range;
                if url_range.contains(offset) {
                    return Some(PrepareRenameHit::LinkTarget {
                        range: url_range,
                        current: text.slice(url_range).to_string(),
                    });
                }
            }
            CstElement::H(node) if node.range.contains(offset) => {
                let title_range = node.data.title.range;
                if title_range.contains(offset) {
                    return Some(PrepareRenameHit::Heading {
                        range: title_range,
                        current: text.slice(title_range).to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    None
}

/// Compute the wiki-link target string's range (the part between `[[`
/// and `]]`, excluding any `|alias` and `#heading`). Returns `None` when
/// the cursor is on the alias or anchor portion — those have their own
/// rename actions.
fn wiki_link_target_hit(link: &WikiLink, offset: usize) -> Option<ByteRange> {
    let doc_range = link.doc_range?;
    if !doc_range.contains(offset) {
        return None;
    }
    Some(doc_range)
}

/// Compute the markdown-link URL's range (the part between `(` and `)`,
/// excluding any `#anchor`). Returns `None` when the cursor is on the
/// anchor portion.
fn md_link_target_hit(link: &MdLink, offset: usize) -> Option<ByteRange> {
    let dest = match link {
        MdLink::Inline { dest, .. } => dest,
        _ => return None,
    };
    if !dest.range.contains(offset) {
        return None;
    }
    Some(dest.range)
}

/// Convert a `PrepareRenameHit` into an LSP `Range` JSON value (UTF-16).
pub fn hit_range_json(text: &Text, hit: &PrepareRenameHit) -> Value {
    let range = match hit {
        PrepareRenameHit::LinkTarget { range, .. } => *range,
        PrepareRenameHit::Heading { range, .. } => *range,
    };
    let start = text
        .to_lsp_position(range.start, crate::utils::PositionEncoding::Utf16)
        .unwrap_or(crate::utils::LspPosition {
            line: 0,
            character: 0,
        });
    let end = text
        .to_lsp_position(range.end, crate::utils::PositionEncoding::Utf16)
        .unwrap_or(crate::utils::LspPosition {
            line: 0,
            character: 0,
        });
    json!({
        "start": { "line": start.line, "character": start.character },
        "end": { "line": end.line, "character": end.character },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParseOptions, parse_document};
    use crate::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
    use crate::utils::Text;
    use std::path::PathBuf;

    fn build_graph(text: &str) -> (ConnectionGraph, Text) {
        let parsed = parse_document(text, ParseOptions::default());
        let doc = ResolveDocument::primary(
            PathBuf::from("/tmp/test.md"),
            PathBuf::from("test.md"),
            parsed,
        );
        let text_obj = Text::new(text);
        let prefix = PrefixIndex::from_entries(std::iter::once((
            "test".to_string(),
            doc.path.clone(),
        )));
        let input = ResolveInput {
            root: PathBuf::from("/tmp"),
            documents: vec![doc],
            mounts: vec![],
            conflicts: vec![],
            config: Default::default(),
            single_file: false,
            prefix_index: prefix,
            uri_resolver: crate::resolution::uri::UriResolver::empty(),
            uri_opts: crate::resolution::UriOptions::default(),
            uri_error: None,
        };
        (resolve_links(input), text_obj)
    }

    /// Cursor on the target string of `[[report]]` returns a link-target hit
    /// covering just the target (no `[[`, `]]`, alias, or heading).
    #[test]
    fn prepare_rename_finds_wiki_link_target() {
        let text = "See [[report]] for details.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("report").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, offset).unwrap();
        match hit {
            PrepareRenameHit::LinkTarget { range, current } => {
                assert_eq!(text_obj.slice(range), "report");
                assert_eq!(current, "report");
            }
            _ => panic!("expected link-target hit"),
        }
    }

    /// Cursor on the alias portion (`|alias`) does NOT trigger a hit —
    /// the alias has its own rename semantics (out of scope for v1).
    #[test]
    fn prepare_rename_skips_wiki_link_alias() {
        let text = "See [[report|alias]] for details.";
        let (graph, text_obj) = build_graph(text);
        let alias_offset = text.find("alias").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, alias_offset);
        assert!(hit.is_none(), "alias should not be a string-rename hit");
    }

    /// Cursor on the heading anchor (`#section`) does NOT trigger a hit —
    /// the heading rename lives behind `refactor.rename.heading`.
    #[test]
    fn prepare_rename_skips_wiki_link_anchor() {
        let text = "See [[report#section]] for details.";
        let (graph, text_obj) = build_graph(text);
        let anchor_offset = text.find("section").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, anchor_offset);
        assert!(hit.is_none(), "anchor should not be a string-rename hit");
    }

    /// Cursor on the URL portion of `[t](target.md)` returns a link-target
    /// hit covering just the URL (no `(`, `)`, or anchor).
    #[test]
    fn prepare_rename_finds_md_link_url() {
        let text = "See [t](target.md) for details.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("target").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, offset).unwrap();
        match hit {
            PrepareRenameHit::LinkTarget { range, current } => {
                assert_eq!(text_obj.slice(range), "target.md");
                assert_eq!(current, "target.md");
            }
            _ => panic!("expected link-target hit"),
        }
    }

    /// Cursor on plain prose returns `None`.
    #[test]
    fn prepare_rename_returns_null_on_plain_text() {
        let text = "Just plain prose.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("plain").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, offset);
        assert!(hit.is_none(), "plain text should not be a rename hit");
    }

    /// Cursor on a heading text occurrence returns a heading hit covering
    /// the title text only (not the `#`s).
    #[test]
    fn prepare_rename_finds_heading_text() {
        let text = "# Heading Text\n\nbody";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("Heading").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, offset).unwrap();
        match hit {
            PrepareRenameHit::Heading { range, current } => {
                assert_eq!(text_obj.slice(range), "Heading Text");
                assert_eq!(current, "Heading Text");
            }
            _ => panic!("expected heading hit"),
        }
    }

    /// Cursor on a `[id]: url` reference definition URL returns a
    /// link-target hit covering the URL portion.
    #[test]
    fn prepare_rename_finds_link_def_url() {
        let text = "See [the report][r].\n\n[r]: reports/q1.md\n";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("reports/q1").unwrap();
        let hit = prepare_rename(&graph, &PathBuf::from("/tmp/test.md"), &text_obj, offset).unwrap();
        match hit {
            PrepareRenameHit::LinkTarget { range, current } => {
                assert_eq!(text_obj.slice(range), "reports/q1.md");
                assert_eq!(current, "reports/q1.md");
            }
            _ => panic!("expected link-target hit"),
        }
    }
}