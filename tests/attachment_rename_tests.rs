mod common;

use common::write_vault;
use downlint::parser::{ParseOptions, Ref, parse_document};
use downlint::rename::{PlanInput, RenameKind, RenamePlan, attachment};
use downlint::resolution::conn::{ConnectionGraph, DestinationKind};
use downlint::resolution::{ResolveDocument, ResolveInput, prefix::PrefixIndex, resolve_links};
use std::path::{Path, PathBuf};

fn graph(root: &Path, docs: &[&str]) -> ConnectionGraph {
    let documents: Vec<_> = docs
        .iter()
        .map(|rel| {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path).unwrap();
            ResolveDocument::primary(
                path,
                PathBuf::from(rel),
                parse_document(&text, ParseOptions::default()),
            )
        })
        .collect();
    let prefix_index =
        PrefixIndex::from_entries(documents.iter().map(|doc| (doc.stem(), doc.path.clone())));
    resolve_links(ResolveInput {
        root: root.to_path_buf(),
        documents,
        mounts: vec![],
        conflicts: vec![],
        config: Default::default(),
        single_file: false,
        prefix_index,
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: Default::default(),
        uri_error: None,
    })
}

fn plan(root: &Path, graph: &ConnectionGraph, old: &str, new: &str) -> RenamePlan {
    attachment::plan(
        PlanInput {
            kind: RenameKind::Attachment,
            old: old.into(),
            new: new.into(),
            source_path: root.to_path_buf(),
            cursor_offset: None,
        },
        graph,
    )
    .unwrap()
}

fn apply(plan: &RenamePlan) {
    for document in plan.sorted_for_apply() {
        let mut text = std::fs::read_to_string(&document.path).unwrap();
        for pair in document.edits.windows(2) {
            assert!(
                pair[1].range.end <= pair[0].range.start,
                "duplicate/overlapping edits: {pair:?}"
            );
        }
        for edit in document.edits {
            text.replace_range(edit.range.start..edit.range.end, &edit.new_text);
        }
        std::fs::write(document.path, text).unwrap();
    }
    for movement in &plan.file_moves {
        std::fs::create_dir_all(movement.to.parent().unwrap()).unwrap();
        std::fs::rename(&movement.from, &movement.to).unwrap();
    }
}

fn text(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn assert_definition_destination(root: &Path, source: &str, label: &str, target: &Path) {
    let structure = parse_document(&text(root, source), ParseOptions::default());
    let definition = structure
        .index
        .link_defs
        .iter()
        .find(|definition| definition.label.text == label)
        .unwrap();
    let path = definition.url.raw.split('#').next().unwrap();
    let resolved = downlint::resolution::path::resolve_explicit_path(
        root,
        root.join(source).parent().unwrap(),
        path,
        false,
    );
    assert_eq!(resolved, target);
    assert!(resolved.is_file());
}

fn assert_attachments(graph: &ConnectionGraph, target: &Path, expected: usize) {
    let refs: Vec<_> = graph
        .resolved_references
        .iter()
        .filter(|reference| {
            reference
                .destinations
                .iter()
                .any(|destination| destination.path == target)
        })
        .collect();
    assert_eq!(
        refs.len(),
        expected,
        "references to {}: {refs:?}",
        target.display()
    );
    for reference in refs {
        assert!(
            reference
                .destinations
                .iter()
                .all(|destination| matches!(destination.kind, DestinationKind::Attachment)),
            "wrong destination: {reference:?}"
        );
    }
}

#[test]
fn attachment_rename_selects_occurrences_not_documents_or_scanner_ids() {
    let temp = write_vault(&[
        ("report", "plain attachment"),
        ("other/report", "other attachment"),
        (
            "index.md",
            "---\ntitle: Index\n---\n[report](./report#section)\n[[report]]\n[[missing]]\n[other](other/report)\n[[other/report]]\n# Index\n![report](./report)\n[[./report#section|alias]]\n",
        ),
    ]);
    let root = temp.path();
    let before = graph(root, &["index.md"]);
    let plan = plan(root, &before, "report", "topic");
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.edits[0].edits.len(), 3);
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "---\ntitle: Index\n---\n[report](topic#section)\n[[report]]\n[[missing]]\n[other](other/report)\n[[other/report]]\n# Index\n![report](topic)\n[[./topic#section|alias]]\n"
    );
    assert_attachments(&graph(root, &["index.md"]), &root.join("topic"), 3);
}

#[test]
fn attachment_rename_preserves_document_and_ambiguous_wiki_references() {
    for ambiguous in [false, true] {
        let temp = write_vault(&[
            ("report", "attachment"),
            ("report.md", "# Report\n"),
            (
                "index.md",
                "[report](report)\n[[report]]\n[[report.md]]\n[[missing]]\n",
            ),
        ]);
        let root = temp.path();
        let mut docs = vec!["index.md", "report.md"];
        if ambiguous {
            std::fs::create_dir_all(root.join("other")).unwrap();
            std::fs::write(root.join("other/report.md"), "# Report\n").unwrap();
            docs.push("other/report.md");
        }
        let before = graph(root, &docs);
        let plan = plan(root, &before, "report", "topic");
        assert_eq!(plan.edits[0].edits.len(), 1);
        apply(&plan);
        assert_eq!(
            text(root, "index.md"),
            "[report](topic)\n[[report]]\n[[report.md]]\n[[missing]]\n"
        );
        assert_attachments(&graph(root, &docs), &root.join("topic"), 1);
    }
}

#[test]
fn attachment_rename_explicit_wiki_remains_an_attachment_with_destination_note_collision() {
    for collision in [false, true] {
        let temp = write_vault(&[
            ("report", "attachment"),
            ("index.md", "[[./report]]\n![[./report#section|alias]]\n"),
        ]);
        let root = temp.path();
        let mut docs = vec!["index.md"];
        if collision {
            std::fs::write(root.join("topic.md"), "# Topic\n# Section\n").unwrap();
            docs.push("topic.md");
        }
        let before = graph(root, &docs);
        if collision {
            let result = attachment::plan(
                PlanInput {
                    kind: RenameKind::Attachment,
                    old: "report".into(),
                    new: "topic".into(),
                    source_path: root.to_path_buf(),
                    cursor_offset: None,
                },
                &before,
            );
            let error = result.unwrap_err();
            assert!(
                matches!(error, downlint::rename::RenameError::Conflict(_)),
                "{error:?}"
            );
            assert!(root.join("report").is_file());
            assert!(!root.join("topic").exists());
            assert_eq!(
                text(root, "index.md"),
                "[[./report]]\n![[./report#section|alias]]\n"
            );
        } else {
            let plan = plan(root, &before, "report", "topic");
            assert_eq!(plan.edits[0].edits.len(), 2);
            apply(&plan);
            assert_eq!(
                text(root, "index.md"),
                "[[./topic]]\n![[./topic#section|alias]]\n"
            );
            assert_attachments(&graph(root, &docs), &root.join("topic"), 2);
        }
    }
}

#[test]
fn attachment_rename_scans_definition_only_documents_with_exact_markdown_paths() {
    let temp = write_vault(&[
        ("report", "attachment"),
        ("other/report", "other attachment"),
        (
            "index.md",
            "[report][ref]\n[ref]: report#section \"title\"\n[other]: other/report\n[missing]: missing/report\n[web]: https://example.com/report\n",
        ),
        (
            "docs/index.md",
            "[report][ref]\n[ref]: /report#section\n[other]: report\n",
        ),
    ]);
    let root = temp.path();
    let docs = ["index.md", "docs/index.md"];
    let before = graph(root, &docs);
    assert!(before.resolved_references.iter().all(|reference| {
        reference
            .destinations
            .iter()
            .all(|destination| matches!(destination.kind, DestinationKind::LinkDefinition))
    }));
    let plan = plan(root, &before, "report", "topics/topic.pdf");
    assert_eq!(plan.edits.len(), 2);
    assert!(plan.edits.iter().all(|document| document.edits.len() == 1));
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "[report][ref]\n[ref]: topics/topic.pdf#section \"title\"\n[other]: other/report\n[missing]: missing/report\n[web]: https://example.com/report\n"
    );
    assert_eq!(
        text(root, "docs/index.md"),
        "[report][ref]\n[ref]: /topics/topic.pdf#section\n[other]: report\n"
    );
    // Independent exact path checks: no new definition graph semantics.
    assert_definition_destination(root, "index.md", "ref", &root.join("topics/topic.pdf"));
    assert_definition_destination(root, "docs/index.md", "ref", &root.join("topics/topic.pdf"));
}

#[test]
fn attachment_rename_preserves_source_root_bases_and_extension_changes() {
    let temp = write_vault(&[
        ("assets/report.png", "attachment"),
        (
            "docs/index.md",
            "[report](../assets/report.png#section)\n![report](/assets/report.png)\n[[../assets/report.png]]\n[[/assets/report.png]]\n[[assets/report.png]]\n[report][ref]\n[ref]: ../assets/report.png#section\n",
        ),
    ]);
    let root = temp.path();
    let plan = plan(
        root,
        &graph(root, &["docs/index.md"]),
        "assets/report.png",
        "docs/sub/topic.pdf",
    );
    assert_eq!(plan.edits[0].edits.len(), 6);
    apply(&plan);
    assert_eq!(
        text(root, "docs/index.md"),
        "[report](sub/topic.pdf#section)\n![report](/docs/sub/topic.pdf)\n[[./sub/topic.pdf]]\n[[/docs/sub/topic.pdf]]\n[[docs/sub/topic.pdf]]\n[report][ref]\n[ref]: sub/topic.pdf#section\n"
    );
    assert_attachments(
        &graph(root, &["docs/index.md"]),
        &root.join("docs/sub/topic.pdf"),
        5,
    );
}

#[test]
fn attachment_rename_root_wiki_path_moved_to_root_basename_keeps_root_base() {
    let temp = write_vault(&[
        ("assets/report", "attachment"),
        ("docs/index.md", "[[assets/report]]\n[[/assets/report]]\n"),
    ]);
    let root = temp.path();
    let plan = plan(
        root,
        &graph(root, &["docs/index.md"]),
        "assets/report",
        "topic",
    );
    apply(&plan);
    assert_eq!(text(root, "docs/index.md"), "[[/topic]]\n[[/topic]]\n");
    assert_attachments(&graph(root, &["docs/index.md"]), &root.join("topic"), 2);
}

#[test]
fn attachment_rename_preserves_percent_encoded_paths_fragments_and_definition_labels() {
    let temp = write_vault(&[
        ("report file.png", "attachment"),
        (
            "index.md",
            "[report](report%20file.png#old%20id)\n[[./report%20file.png#old%20id|alias]]\n[report][ref]\n[ref]: report%20file.png#old%20id\n",
        ),
    ]);
    let root = temp.path();
    let plan = plan(
        root,
        &graph(root, &["index.md"]),
        "report file.png",
        "topic file.pdf",
    );
    assert_eq!(plan.edits[0].edits.len(), 3);
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "[report](topic%20file.pdf#old%20id)\n[[./topic%20file.pdf#old%20id|alias]]\n[report][ref]\n[ref]: topic%20file.pdf#old%20id\n"
    );
    assert_definition_destination(root, "index.md", "ref", &root.join("topic file.pdf"));
    let after = graph(root, &["index.md"]);
    assert_attachments(&after, &root.join("topic file.pdf"), 2);
    assert!(
        after
            .resolved_references
            .iter()
            .any(|reference| matches!(reference.reference, Ref::Full { .. }))
    );
}

#[test]
fn attachment_rename_refuses_wiki_title_and_ambiguous_destination_matches() {
    for ambiguous in [false, true] {
        let temp = write_vault(&[
            ("report.png", "attachment"),
            ("index.md", "[[./report.png]]\n"),
            // Title matching is co-equal even for explicit wiki paths.
            ("other.md", "# TopicPDF\n"),
        ]);
        let root = temp.path();
        let mut docs = vec!["index.md", "other.md"];
        if ambiguous {
            std::fs::write(root.join("another.md"), "# TopicPDF\n").unwrap();
            docs.push("another.md");
        }
        let result = attachment::plan(
            PlanInput {
                kind: RenameKind::Attachment,
                old: "report.png".into(),
                new: "topic.pdf".into(),
                source_path: root.to_path_buf(),
                cursor_offset: None,
            },
            &graph(root, &docs),
        );
        assert!(
            matches!(result, Err(downlint::rename::RenameError::Conflict(_))),
            "{result:?}"
        );
        assert_eq!(text(root, "index.md"), "[[./report.png]]\n");
        assert!(root.join("report.png").is_file());
    }
}

#[test]
fn attachment_rename_to_extensionless_keeps_wiki_attachment_path() {
    let temp = write_vault(&[
        ("report.png", "attachment"),
        ("index.md", "[[report.png]]\n"),
    ]);
    let root = temp.path();
    let plan = plan(root, &graph(root, &["index.md"]), "report.png", "topic");
    apply(&plan);
    assert_eq!(text(root, "index.md"), "[[./topic]]\n");
    assert_attachments(&graph(root, &["index.md"]), &root.join("topic"), 1);
}

#[test]
fn attachment_rename_refuses_inline_case_insensitive_indexed_document_collision() {
    let temp = write_vault(&[
        ("report.png", "attachment"),
        ("index.md", "[report](report.png)\n"),
        ("other.md", "# Topic\n"),
    ]);
    let root = temp.path();
    let mut before = graph(root, &["index.md", "other.md"]);
    // An editor-indexed unsaved document (possibly with a configured custom
    // extension) can collide under document matching without a disk collision.
    let other = before
        .documents
        .iter_mut()
        .find(|doc| doc.rel_path == Path::new("other.md"))
        .unwrap();
    other.path = root.join("TOPIC.PNG");
    other.rel_path = "TOPIC.PNG".into();
    other.namespace_rel_path = "TOPIC.PNG".into();
    let result = attachment::plan(
        PlanInput {
            kind: RenameKind::Attachment,
            old: "report.png".into(),
            new: "topic.png".into(),
            source_path: root.to_path_buf(),
            cursor_offset: None,
        },
        &before,
    );
    assert!(
        matches!(result, Err(downlint::rename::RenameError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(text(root, "index.md"), "[report](report.png)\n");
}

#[test]
fn attachment_rename_bare_file_like_wiki_basename_preserves_spelling() {
    let temp = write_vault(&[("image.png", "attachment"), ("index.md", "[[image.png]]\n")]);
    let root = temp.path();
    let plan = plan(root, &graph(root, &["index.md"]), "image.png", "topic.png");
    apply(&plan);
    assert_eq!(text(root, "index.md"), "[[topic.png]]\n");
    assert_attachments(&graph(root, &["index.md"]), &root.join("topic.png"), 1);
}

#[test]
fn attachment_rename_definition_colon_filename_is_local_but_web_uri_is_not() {
    let temp = write_vault(&[
        ("report:topic", "attachment"),
        (
            "index.md",
            "[report][ref]\n[ref]: report:topic#section\n[web]: https://example.com/report:topic\n[uri]: report://topic\n",
        ),
    ]);
    let root = temp.path();
    let plan = plan(
        root,
        &graph(root, &["index.md"]),
        "report:topic",
        "topic:report",
    );
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.edits[0].edits.len(), 1);
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "[report][ref]\n[ref]: topic%3Areport#section\n[web]: https://example.com/report:topic\n[uri]: report://topic\n"
    );
    assert_definition_destination(root, "index.md", "ref", &root.join("topic:report"));
}

#[test]
fn attachment_rename_honors_configured_non_slash_uri_prefix_for_definitions() {
    use downlint::utils::{WorkspaceInput, discover_workspace};
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nuri = 'report:topic'\nto = '.'\n",
        ),
        ("report:topic", "attachment"),
        (
            "index.md",
            "[report][ref]\n[ref]: report:topic#section\n[local]: ./report:topic#section\n[web]: https://example.com/report:topic\n",
        ),
    ]);
    let root = temp.path();
    let workspace =
        discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
    let before = resolve_links(ResolveInput::from_workspace(&workspace));
    assert!(
        before
            .uri_resolver
            .as_ref()
            .unwrap()
            .is_uri_target("report:topic")
    );
    let plan = plan(root, &before, "report:topic", "topic");
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.edits[0].edits.len(), 1);
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "[report][ref]\n[ref]: report:topic#section\n[local]: topic#section\n[web]: https://example.com/report:topic\n"
    );
}

#[test]
fn attachment_rename_refuses_replacement_routed_to_configured_uri() {
    use downlint::utils::{WorkspaceInput, discover_workspace};
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nuri = 'topic:assets'\nto = '.'\n",
        ),
        ("report:assets/image.png", "attachment"),
        ("index.md", "[[report:assets/image.png]]\n"),
    ]);
    let root = temp.path();
    let workspace =
        discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
    let before = resolve_links(ResolveInput::from_workspace(&workspace));
    assert_attachments(&before, &root.join("report:assets/image.png"), 1);
    let result = attachment::plan(
        PlanInput {
            kind: RenameKind::Attachment,
            old: "report:assets/image.png".into(),
            new: "topic:assets/image.png".into(),
            source_path: root.to_path_buf(),
            cursor_offset: None,
        },
        &before,
    );
    // Wiki symbols decode before URI routing: percent encoding cannot conceal
    // a configured scheme prefix and must not produce a misdirected edit.
    assert!(
        matches!(result, Err(downlint::rename::RenameError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(text(root, "index.md"), "[[report:assets/image.png]]\n");
}

#[test]
fn attachment_rename_uses_configured_markdown_extensions() {
    let temp = write_vault(&[
        ("report.png", "attachment"),
        ("index.md", "[report](report.png)\n"),
    ]);
    let root = temp.path();
    let mut before = graph(root, &["index.md"]);
    before.markdown_extensions = vec!["md".into(), "png".into()];
    let result = attachment::plan(
        PlanInput {
            kind: RenameKind::Attachment,
            old: "report.png".into(),
            new: "topic.pdf".into(),
            source_path: root.to_path_buf(),
            cursor_offset: None,
        },
        &before,
    );
    assert!(
        matches!(result, Err(downlint::rename::RenameError::Conflict(_))),
        "{result:?}"
    );
}

#[test]
fn attachment_rename_definition_label_matching_filename_preserves_label_and_usage() {
    let temp = write_vault(&[
        ("report", "attachment"),
        (
            "index.md",
            "[report][report]\n[report]: report\n[report topic]: report\n  [other report]:   report#section\n",
        ),
    ]);
    let root = temp.path();
    let plan = plan(root, &graph(root, &["index.md"]), "report", "topic");
    apply(&plan);
    assert_eq!(
        text(root, "index.md"),
        "[report][report]\n[report]: topic\n[report topic]: topic\n  [other report]:   topic#section\n"
    );
    assert_definition_destination(root, "index.md", "report", &root.join("topic"));
    assert_definition_destination(root, "index.md", "report topic", &root.join("topic"));
}

#[test]
fn attachment_rename_punctuation_extensions_keep_explicit_wiki_semantics() {
    for destination in ["topic._pdf", "topic.p-df"] {
        for note_exists in [false, true] {
            let temp = write_vault(&[("image.png", "attachment"), ("index.md", "[[image.png]]\n")]);
            let root = temp.path();
            let note = format!("{destination}.md");
            let mut docs = vec!["index.md"];
            if note_exists {
                std::fs::write(root.join(&note), "# Unrelated\n").unwrap();
                docs.push(&note);
            }
            let plan = plan(root, &graph(root, &docs), "image.png", destination);
            apply(&plan);
            assert!(text(root, "index.md").starts_with("[[./topic."));
            assert_attachments(&graph(root, &docs), &root.join(destination), 1);
        }
    }
}

#[test]
fn attachment_rename_mounted_sources_and_definition_only_sources_use_correct_bases() {
    use downlint::utils::{WorkspaceInput, discover_workspace};
    // Exclude the mount from the primary document set, as a distinct root.
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[core]\nignore = ['mounted/**']\n[[mounts]]\npath = 'mounted'\nas = '/kb'\nlint = true\n",
        ),
        ("report", "attachment"),
        ("index.md", ""),
        (
            "mounted/index.md",
            "[report](../report)\n[[../report]]\n[[/report]]\n",
        ),
        (
            "mounted/definitions.md",
            "[report][ref]\n[ref]: /report#section\n",
        ),
    ]);
    let root = temp.path();
    let load = || {
        let ws = discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
        resolve_links(ResolveInput::from_workspace(&ws))
    };
    let plan = plan(root, &load(), "report", "topics/topic.pdf");
    apply(&plan);
    assert_eq!(
        text(root, "mounted/index.md"),
        "[report](../topics/topic.pdf)\n[[../topics/topic.pdf]]\n[[/topics/topic.pdf]]\n"
    );
    assert_eq!(
        text(root, "mounted/definitions.md"),
        "[report][ref]\n[ref]: /topics/topic.pdf#section\n"
    );
    let after = load();
    assert!(after.unresolved_references.is_empty());
    let target = root.join("topics/topic.pdf").canonicalize().unwrap();
    let moved: Vec<_> = after
        .resolved_references
        .iter()
        .filter(|r| {
            r.destinations.iter().any(|d| {
                matches!(d.kind, DestinationKind::Attachment)
                    && d.path.canonicalize().unwrap() == target
            })
        })
        .collect();
    assert_eq!(moved.len(), 3);
}

#[test]
fn attachment_rename_discovery_configured_extension_checks_destination_too() {
    use downlint::utils::{WorkspaceInput, discover_workspace};
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[core]\nfile_extensions = ['md', 'note']\n",
        ),
        ("report.png", "attachment"),
        ("index.md", "[report](report.png)\n"),
        ("report.note", "# Report\n"),
    ]);
    let root = temp.path();
    let ws = discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
    let before = resolve_links(ResolveInput::from_workspace(&ws));
    for (old, new) in [("report.png", "topic.note"), ("report.note", "topic.pdf")] {
        let result = attachment::plan(
            PlanInput {
                kind: RenameKind::Attachment,
                old: old.into(),
                new: new.into(),
                source_path: root.to_path_buf(),
                cursor_offset: None,
            },
            &before,
        );
        assert!(matches!(
            result,
            Err(downlint::rename::RenameError::Conflict(_))
        ));
    }
}

#[test]
fn attachment_rename_duplicate_graph_edges_do_not_duplicate_edits() {
    let temp = write_vault(&[
        ("report", "attachment"),
        ("index.md", "[report](./report)\n[[./report]]\n"),
    ]);
    let root = temp.path();
    let mut before = graph(root, &["index.md"]);
    before
        .resolved_references
        .extend(before.resolved_references.clone());
    let plan = plan(root, &before, "report", "topic");
    assert_eq!(plan.edits[0].edits.len(), 2);
    apply(&plan);
    assert_attachments(&graph(root, &["index.md"]), &root.join("topic"), 2);
}
