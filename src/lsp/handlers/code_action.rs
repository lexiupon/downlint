//! LSP `textDocument/codeAction` handler — RFC 0009 §"Code Actions".
//!
//! Surfaces three rename actions:
//!
//! - `refactor.rename.file` — moves a file on disk and rewrites references.
//! - `refactor.rename.link-target` — workspace-wide string rewrite.
//! - `refactor.rename.heading` — heading slug recomputation.
//!
//! Phase 1 only **offers** these actions based on cursor context. The
//! actual planning for each kind ships in Phase 2 (heading), Phase 3
//! (file), and Phase 6 (link-target). Invoking a not-yet-implemented
//! action surfaces a `MethodFailed` error via [`crate::rename::RenameError::NotImplemented`].
//!
//! ## Why both `textDocument/rename` and `refactor.rename.link-target`?
//!
//! They do the same operation (string rename at the cursor) but surface
//! through different UI paths:
//!
//! - `textDocument/rename` is the F2 binding. Users with muscle memory hit
//!   F2 and get the string rename without thinking.
//! - `refactor.rename.link-target` is the command-palette entry. Users who
//!   search for "rename" find it explicitly.
//!
//! Both are safe. The redundancy is intentional.

use crate::parser::cst::{CstElement, MdLink, WikiLink};
use crate::resolution::ConnectionGraph;
use crate::utils::{ByteRange, Text};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The LSP code-action kinds we surface. Keep in sync with the
/// `codeActionProvider.codeActionKinds` array in the initialize response.
pub const KIND_FILE: &str = "refactor.rename.file";
pub const KIND_LINK_TARGET: &str = "refactor.rename.link-target";
pub const KIND_HEADING: &str = "refactor.rename.heading";

/// What `codeAction` found at the cursor. Each variant maps to the kinds
/// the LSP should offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeActionAvailability {
    /// Cursor on a link target — offer `refactor.rename.link-target`.
    LinkTarget,
    /// Cursor on a heading text — offer `refactor.rename.heading`.
    Heading,
}

/// Determine which (if any) rename code actions to offer at the cursor.
///
/// Today (Phase 1) this is a small resolver-free check on the cursor
/// position — does it land on a link target or a heading text? Later
/// phases augment this with resolution state to decide whether
/// `refactor.rename.file` should also be offered (cursor on a link that
/// resolves uniquely to one file).
pub fn code_actions(
    graph: &ConnectionGraph,
    path: &PathBuf,
    _text: &Text,
    range: ByteRange,
) -> Vec<Value> {
    let mut actions = Vec::new();
    let Some(document) = graph.documents.iter().find(|doc| &doc.path == path) else {
        return actions;
    };

    for element in &document.structure.cst.elements {
        match element {
            CstElement::WL(node) if node.range.overlaps(&range) => {
                if wiki_link_target_range(&node.data).is_some_and(|r| r.overlaps(&range)) {
                    actions.push(action_json(KIND_LINK_TARGET, "Rename link target"));
                }
            }
            CstElement::ML(node) if node.range.overlaps(&range) => {
                if md_link_url_range(&node.data).is_some_and(|r| r.overlaps(&range)) {
                    actions.push(action_json(KIND_LINK_TARGET, "Rename link target"));
                }
            }
            CstElement::MLD(node) if node.range.overlaps(&range) => {
                if node.data.url.range.overlaps(&range) {
                    actions.push(action_json(KIND_LINK_TARGET, "Rename link target"));
                }
            }
            CstElement::H(node) if node.range.overlaps(&range) => {
                if node.data.title.range.overlaps(&range) {
                    actions.push(action_json(KIND_HEADING, "Rename heading"));
                }
            }
            _ => {}
        }
    }

    actions
}

fn wiki_link_target_range(link: &WikiLink) -> Option<ByteRange> {
    link.doc_range
}

fn md_link_url_range(link: &MdLink) -> Option<ByteRange> {
    match link {
        MdLink::Inline { dest, .. } => Some(dest.range),
        _ => None,
    }
}

fn action_json(kind: &str, title: &str) -> Value {
    json!({
        "title": title,
        "kind": kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParseOptions, parse_document};
    use crate::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
    use crate::utils::{ByteRange, Text};
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

    /// Cursor on the target of `[[report]]` offers
    /// `refactor.rename.link-target`.
    #[test]
    fn offers_link_target_action_on_wiki_link() {
        let text = "See [[report]] for details.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("report").unwrap();
        let actions = code_actions(
            &graph,
            &PathBuf::from("/tmp/test.md"),
            &text_obj,
            ByteRange::new(offset, offset + "report".len()),
        );
        assert!(
            actions
                .iter()
                .any(|a| a.get("kind").and_then(|v| v.as_str()) == Some(KIND_LINK_TARGET)),
            "expected link-target action, got: {actions:?}"
        );
    }

    /// Cursor on a heading text offers `refactor.rename.heading`.
    #[test]
    fn offers_heading_action_on_heading() {
        let text = "## Methods\n\nbody";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("Methods").unwrap();
        let actions = code_actions(
            &graph,
            &PathBuf::from("/tmp/test.md"),
            &text_obj,
            ByteRange::new(offset, offset + "Methods".len()),
        );
        assert!(
            actions
                .iter()
                .any(|a| a.get("kind").and_then(|v| v.as_str()) == Some(KIND_HEADING)),
            "expected heading action, got: {actions:?}"
        );
    }

    /// Cursor on plain prose offers no rename actions.
    #[test]
    fn offers_nothing_on_plain_text() {
        let text = "Just plain prose.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("plain").unwrap();
        let actions = code_actions(
            &graph,
            &PathBuf::from("/tmp/test.md"),
            &text_obj,
            ByteRange::new(offset, offset + "plain".len()),
        );
        assert!(actions.is_empty(), "expected no actions on plain prose, got: {actions:?}");
    }

    /// Cursor on a wiki-link alias does NOT offer the link-target action —
    /// aliases have their own rename semantics (out of scope for v1).
    #[test]
    fn skips_link_target_action_on_alias() {
        let text = "See [[report|alias]] for details.";
        let (graph, text_obj) = build_graph(text);
        let alias_offset = text.find("alias").unwrap();
        let actions = code_actions(
            &graph,
            &PathBuf::from("/tmp/test.md"),
            &text_obj,
            ByteRange::new(alias_offset, alias_offset + "alias".len()),
        );
        assert!(
            !actions
                .iter()
                .any(|a| a.get("kind").and_then(|v| v.as_str()) == Some(KIND_LINK_TARGET)),
            "alias should not be a link-target rename target"
        );
    }

    /// Cursor on the URL portion of `[t](target.md)` offers
    /// `refactor.rename.link-target`.
    #[test]
    fn offers_link_target_action_on_md_link() {
        let text = "See [t](target.md) for details.";
        let (graph, text_obj) = build_graph(text);
        let offset = text.find("target").unwrap();
        let actions = code_actions(
            &graph,
            &PathBuf::from("/tmp/test.md"),
            &text_obj,
            ByteRange::new(offset, offset + "target.md".len()),
        );
        assert!(
            actions
                .iter()
                .any(|a| a.get("kind").and_then(|v| v.as_str()) == Some(KIND_LINK_TARGET)),
            "expected link-target action on md link URL"
        );
    }
}