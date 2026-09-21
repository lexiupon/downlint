//! LSP Type H — heading rename — RFC 0009 §"Type H".
//!
//! Renames a heading text occurrence in the source document and rewrites
//! every `#section` slug in workspace-wide links that resolves to it.
//!
//! ## Algorithm
//!
//! 1. Locate the heading being renamed: `[source_path, old_slug]` matches
//!    one entry in `ResolvedDocument.headings` for the doc identified by
//!    `PlanInput.source_path`. The `name_range` of the matching
//!    `ResolvedDestination` is the heading title's byte range — the
//!    identifier we use to recognize referencing links.
//!
//! 2. Compute the new slug from `PlanInput.new`.
//!
//! 3. Validate:
//!    - `new` contains no `\n` or `#`.
//!    - The new slug does not collide with another heading's slug in the
//!      same document.
//!
//! 4. For every `ResolvedReference` whose destinations include the
//!    heading (matched by path + range), look up the source document's
//!    CST and emit byte-precise `TextEdit`s for the `#section` portion of
//!    the corresponding wiki-link or markdown-link.
//!
//! 5. Add the heading-text edit in the source document itself.
//!
//! ## Scope (RFC §"Type H")
//!
//! - H1 vs H2+ distinction adds no value at this layer — the propagation
//!   is the same. They rename identically.
//! - The rename does NOT rewrite title-only `[[|Title]]` references or
//!   frontmatter `title:` aliases (RFC §"H1 title side-effect"). After
//!   the rename the resolver re-runs and emits fresh diagnostics; the
//!   user fixes those themselves.

use crate::parser::cst::{CstElement, MdLink};
use crate::rename::{DocumentEdit, PlanInput, RenameError, RenamePlan, TextEdit};
use crate::resolution::Slug;
use crate::resolution::conn::DestinationKind;
use crate::resolution::ConnectionGraph;
use std::collections::HashMap;
use std::path::PathBuf;

/// Plan a heading rename. See module docs for the algorithm.
pub fn plan(input: PlanInput, graph: &ConnectionGraph) -> Result<RenamePlan, RenameError> {
    // 1. Validate `new` contains no forbidden characters.
    if input.new.contains('\n') || input.new.contains('#') {
        return Err(RenameError::NotApplicable(format!(
            "heading text cannot contain '\\n' or '#': got {:?}",
            input.new
        )));
    }
    if input.new.is_empty() {
        return Err(RenameError::NotApplicable(
            "heading text cannot be empty".into(),
        ));
    }

    let old_slug = Slug::from(input.old.as_str());
    let new_slug = Slug::from_heading_text(&input.new);

    // 2. Locate the heading in the source document.
    let heading_doc = graph
        .documents
        .iter()
        .find(|doc| doc.path == input.source_path)
        .ok_or_else(|| {
            RenameError::NotApplicable(format!(
                "source document not found in graph: {}",
                input.source_path.display()
            ))
        })?;
    let heading_destinations: Vec<_> = heading_doc
        .headings
        .get(&old_slug)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|dest| {
            dest.path == input.source_path && matches!(dest.kind, DestinationKind::Heading)
        })
        .collect();
    if heading_destinations.is_empty() {
        return Err(RenameError::NotApplicable(format!(
            "no heading with slug {:?} found in {}",
            old_slug.as_str(),
            input.source_path.display()
        )));
    }
    if heading_destinations.len() > 1 {
        return Err(RenameError::NotApplicable(format!(
            "heading slug {:?} is ambiguous in {} ({} candidates) — disambiguate first",
            old_slug.as_str(),
            input.source_path.display(),
            heading_destinations.len()
        )));
    }
    let heading_destination = &heading_destinations[0];
    let heading_range = heading_destination
        .range
        .ok_or_else(|| RenameError::NotApplicable("heading has no title range".into()))?;

    // 3. Reject slug collisions within the same document. The renamed
    //    heading's new slug must not collide with another heading's slug
    //    in the same doc — RFC §"Type H Validation".
    for (other_slug, _) in heading_doc.headings.iter() {
        if other_slug == &old_slug {
            continue;
        }
        if other_slug.as_str() == new_slug.as_str() {
            return Err(RenameError::NotApplicable(format!(
                "heading slug {:?} collides with another heading's slug in {}",
                new_slug.as_str(),
                input.source_path.display()
            )));
        }
    }

    // 4. Walk the resolved references. For each one whose destinations
    //    include our heading, look up the CST node in the source doc
    //    and emit the per-occurrence edit.
    let mut edits_by_doc: HashMap<PathBuf, Vec<TextEdit>> = HashMap::new();

    for reference in &graph.resolved_references {
        let points_at_heading = reference
            .destinations
            .iter()
            .any(|dest| dest.path == input.source_path && dest.range == Some(heading_range));
        if !points_at_heading {
            continue;
        }
        let Some(document) = graph
            .documents
            .iter()
            .find(|doc| doc.path == reference.source_path)
        else {
            continue;
        };
        let edits = edits_for_reference(document, &old_slug, &new_slug);
        if !edits.is_empty() {
            edits_by_doc
                .entry(reference.source_path.clone())
                .or_default()
                .extend(edits);
        }
    }

    // 5. Add the heading-text edit in the source document itself.
    edits_by_doc
        .entry(input.source_path.clone())
        .or_default()
        .push(TextEdit {
            range: heading_range,
            new_text: input.new.clone(),
        });

    let edits = edits_by_doc
        .into_iter()
        .map(|(path, edits)| DocumentEdit::new(path, edits))
        .collect();
    Ok(RenamePlan::new(edits, vec![]))
}

/// Walk a document's CST and emit byte-precise edits for every link
/// whose `#section` portion's slug matches `old_slug`. Used when a
/// resolved reference's destination is the heading being renamed — we
/// then look at every link in the source doc whose anchor resolves to
/// that heading.
///
/// We don't rely on the reference's `full_range` matching a particular
/// CST node range (the parser's symbol `full_range` covers the target
/// portion only — the doc portion for `[[guide#h]]`, the heading portion
/// for `[[#h]]`). Instead we iterate every wiki-link and md-link in the
/// doc and emit edits for those whose anchor decodes to the old slug.
fn edits_for_reference(
    document: &crate::resolution::conn::ResolvedDocument,
    old_slug: &Slug,
    new_slug: &Slug,
) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    for element in &document.structure.cst.elements {
        match element {
            CstElement::WL(node) => {
                let link = &node.data;
                if let (Some(heading_node), Some(heading_range)) =
                    (&link.heading, link.heading_range)
                {
                    if Slug::from(heading_node.decoded.as_str()).as_str() == old_slug.as_str() {
                        edits.push(TextEdit {
                            range: heading_range,
                            new_text: new_slug.as_str().to_string(),
                        });
                    }
                }
            }
            CstElement::ML(node) => {
                if let MdLink::Inline {
                    anchor_range: Some(anchor_range),
                    ..
                } = &node.data
                {
                    let dest = match &node.data {
                        MdLink::Inline { dest, .. } => dest,
                        _ => unreachable!(),
                    };
                    if let Some((_, anchor_text)) = dest.text.split_once('#') {
                        if Slug::from(anchor_text).as_str() == old_slug.as_str() {
                            edits.push(TextEdit {
                                range: *anchor_range,
                                new_text: new_slug.as_str().to_string(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ParseOptions, parse_document};
    use crate::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
    use std::path::PathBuf;

    /// Build a graph from a list of `(rel_path, content)` pairs. Files
    /// are written under a `TempDir` so they're easy to clean up.
    fn build_graph_from(
        texts: &[(&str, &str)],
    ) -> (ConnectionGraph, tempfile::TempDir) {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let mut documents = Vec::new();
        for (rel, content) in texts {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            let structure = parse_document(content, ParseOptions::default());
            let rel_path = PathBuf::from(rel);
            documents.push(ResolveDocument::primary(
                path.clone(),
                rel_path,
                structure,
            ));
        }
        let prefix = PrefixIndex::from_entries(
            documents.iter().map(|d| (d.stem(), d.path.clone())),
        );
        let input = ResolveInput {
            root: root.clone(),
            documents,
            mounts: vec![],
            conflicts: vec![],
            config: Default::default(),
            single_file: false,
            prefix_index: prefix,
            uri_resolver: crate::resolution::uri::UriResolver::empty(),
            uri_opts: crate::resolution::UriOptions::default(),
            uri_error: None,
        };
        (resolve_links(input), temp)
    }

    /// Renaming a heading emits an edit replacing the heading text in the
    /// source document AND an edit replacing the `#section` portion of
    /// the same-doc anchor link `[[#methods]]`.
    #[test]
    fn rename_heading_same_doc() {
        let (graph, temp) = build_graph_from(&[(
            "guide.md",
            "# Guide\n\n## Methods\n\nbody\n\nsee [[#methods]]\n",
        )]);
        let path = temp.path().join("guide.md");
        let plan = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "methods".into(),
                new: "Approach".into(),
                source_path: path.clone(),
                cursor_offset: None,
            },
            &graph,
        )
        .expect("rename plan should succeed");
        let doc_edits = plan
            .edits
            .iter()
            .find(|d| d.path == path)
            .expect("expected edits for guide.md");
        assert_eq!(
            doc_edits.edits.len(),
            2,
            "expected heading + anchor edits, got {doc_edits:?}"
        );
    }

    /// Renaming a heading rejects when `new` contains `#`.
    #[test]
    fn rename_heading_rejects_hash_in_new() {
        let (graph, temp) = build_graph_from(&[("guide.md", "# Guide\n\n## Methods\n")]);
        let path = temp.path().join("guide.md");
        let result = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "methods".into(),
                new: "Approach #2".into(),
                source_path: path,
                cursor_offset: None,
            },
            &graph,
        );
        assert!(matches!(result, Err(RenameError::NotApplicable(_))));
    }

    /// Renaming a heading rejects when the new slug collides with another
    /// heading's slug in the same doc.
    #[test]
    fn rename_heading_slug_collision_rejected() {
        let (graph, temp) = build_graph_from(&[(
            "guide.md",
            "# Guide\n\n## Methods\n\n## Approach\n",
        )]);
        let path = temp.path().join("guide.md");
        let result = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "methods".into(),
                new: "Approach".into(),
                source_path: path,
                cursor_offset: None,
            },
            &graph,
        );
        assert!(matches!(result, Err(RenameError::NotApplicable(_))));
    }

    /// Renaming a heading that doesn't exist returns NotApplicable.
    #[test]
    fn rename_heading_not_found() {
        let (graph, temp) = build_graph_from(&[("guide.md", "# Guide\n\n## Methods\n")]);
        let path = temp.path().join("guide.md");
        let result = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "nonexistent".into(),
                new: "Whatever".into(),
                source_path: path,
                cursor_offset: None,
            },
            &graph,
        );
        assert!(matches!(result, Err(RenameError::NotApplicable(_))));
    }

    /// Cross-doc heading rename: references in a separate doc get their
    /// `#section` portion rewritten.
    #[test]
    fn rename_heading_cross_doc() {
        let (graph, temp) = build_graph_from(&[
            ("guide.md", "# Guide\n\n## Methods\n"),
            ("index.md", "see [[guide#methods]]\n"),
        ]);
        let guide_path = temp.path().join("guide.md");
        let plan = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "methods".into(),
                new: "Approach".into(),
                source_path: guide_path,
                cursor_offset: None,
            },
            &graph,
        )
        .expect("rename should succeed");
        // Should produce edits in both guide.md (heading) and index.md (anchor).
        assert_eq!(plan.edits.len(), 2);
    }

    /// Markdown-link anchor edits: `[t](guide.md#methods)` rewrites the
    /// `#methods` portion only.
    #[test]
    fn rename_heading_markdown_link_anchor() {
        let (graph, temp) = build_graph_from(&[
            ("guide.md", "# Guide\n\n## Methods\n"),
            ("index.md", "see [t](guide.md#methods)\n"),
        ]);
        let guide_path = temp.path().join("guide.md");
        let plan = plan(
            PlanInput {
                kind: crate::rename::RenameKind::Heading,
                old: "methods".into(),
                new: "Approach".into(),
                source_path: guide_path,
                cursor_offset: None,
            },
            &graph,
        )
        .expect("rename should succeed");
        let index_edits = plan
            .edits
            .iter()
            .find(|d| d.path == temp.path().join("index.md"))
            .expect("expected edits for index.md");
        // Single edit replacing the `#methods` portion of the URL.
        assert_eq!(index_edits.edits.len(), 1);
    }
}