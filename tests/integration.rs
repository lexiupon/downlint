use downlint::config::Config;
use downlint::diagnostics::{DiagnosticCode, DiagnosticConfig, check_diagnostics};
use downlint::parser::{ParseOptions, Ref, parse_document};
use downlint::resolution::{
    ResolveDestinationKind, ResolveDocument, ResolveInput, Slug, resolve_links,
};
use std::fs;
use tempfile::TempDir;

/// Helper to create a document file and return a ResolveDocument.
fn write_document(root: &std::path::Path, rel: &str, content: &str) -> ResolveDocument {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    let structure = parse_document(content, ParseOptions::default());
    let rel_path = path.strip_prefix(root).unwrap().to_path_buf();
    ResolveDocument {
        path,
        rel_path,
        structure,
    }
}

/// Helper to build a ResolveInput from a root and list of documents.
fn resolve_graph(
    root: &std::path::Path,
    docs: Vec<ResolveDocument>,
) -> downlint::resolution::ConnectionGraph {
    let input = ResolveInput {
        root: root.to_path_buf(),
        documents: docs,
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![],
        single_file: false,
    };
    resolve_links(input)
}

#[test]
fn explicit_file_like_targets_resolve_as_attachments_without_config() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let absolute = root.join("assets/data.xlsx");
    fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    fs::write(&absolute, "").unwrap();

    let relative = root.join("notes/relative.docx");
    fs::create_dir_all(relative.parent().unwrap()).unwrap();
    fs::write(&relative, "").unwrap();

    let basename = root.join("notes/same-dir.csv");
    fs::write(&basename, "").unwrap();

    let doc = write_document(
        root,
        "notes/test.md",
        "\
[absolute](/assets/data.xlsx)
[relative](./relative.docx)
[basename](same-dir.csv)
",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::DNL002),
        "expected attachment links to resolve without broken-link diagnostics, got: {diagnostics:?}"
    );
    assert_eq!(graph.resolved_references.len(), 3);

    let mut paths = graph
        .resolved_references
        .iter()
        .flat_map(|reference| reference.destinations.iter())
        .map(|destination| {
            assert!(matches!(
                destination.kind,
                ResolveDestinationKind::Attachment
            ));
            destination.path.clone()
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut expected = vec![absolute, basename, relative];
    expected.sort();
    assert_eq!(paths, expected);
}

#[test]
fn missing_explicit_file_like_targets_emit_broken_link_diagnostics() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/test.md",
        "\
[relative](./missing.docx)
[basename](missing.csv)
",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());
    let broken = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::DNL002)
        .collect::<Vec<_>>();

    assert_eq!(
        broken.len(),
        2,
        "expected broken-link diagnostics for missing attachment paths"
    );

    let messages = broken
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("./missing.docx"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("missing.csv"))
    );
}

// ============================================================================
// Local link resolution tests
// ============================================================================

#[test]
fn explicit_markdown_document_paths_resolve_as_documents_and_headings() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let test_doc = write_document(
        root,
        "notes/test.md",
        "\
[doc](./guide.md)
[section](./guide.md#overview)
",
    );
    let guide_doc = write_document(
        root,
        "notes/guide.md",
        "\
# Guide

## Overview
",
    );

    let guide_path = guide_doc.path.clone();
    let graph = resolve_graph(root, vec![test_doc, guide_doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::DNL002),
        "expected explicit markdown paths to resolve cleanly, got: {diagnostics:?}"
    );

    let doc_ref = graph
        .resolved_references
        .iter()
        .find(|reference| matches!(&reference.reference, Ref::Inline { target, anchor: None, .. } if target == "./guide.md"))
        .expect("expected resolved document reference");
    assert_eq!(doc_ref.destinations.len(), 1);
    assert!(matches!(
        doc_ref.destinations[0].kind,
        ResolveDestinationKind::Document
    ));
    assert_eq!(doc_ref.destinations[0].path, guide_path);

    let section_ref = graph
        .resolved_references
        .iter()
        .find(|reference| {
            matches!(
                &reference.reference,
                Ref::Inline {
                    target,
                    anchor: Some(anchor),
                    ..
                } if target == "./guide.md" && anchor == "overview"
            )
        })
        .expect("expected resolved heading reference");
    assert_eq!(section_ref.destinations.len(), 1);
    assert!(matches!(
        section_ref.destinations[0].kind,
        ResolveDestinationKind::Heading
    ));
    assert_eq!(section_ref.destinations[0].path, guide_path);
}

// ============================================================================
// Non-ASCII / Swedish character tests
// ============================================================================

/// Verify that Slug::from_heading_text handles Swedish characters correctly.
/// This is a unit-level check to ensure the slug generation is deterministic
/// for non-ASCII input.
#[test]
fn slug_generation_with_swedish_chars() {
    // ö should be preserved in the slug (it's a non-ASCII letter)
    assert_eq!(
        Slug::from_heading_text("Jon Sjöstrand").as_str(),
        "jon-sjöstrand"
    );
    assert_eq!(Slug::from_heading_text("Jon Männik").as_str(), "jon-männik");
    assert_eq!(
        Slug::from_heading_text("Kontaktinformation").as_str(),
        "kontaktinformation"
    );

    // å should also be preserved
    assert_eq!(
        Slug::from_heading_text("Åke Åkerlund").as_str(),
        "åke-åkerlund"
    );
}

/// Verify that wiki links referencing documents with non-ASCII characters
/// (Swedish ö, ä) in the title are resolved correctly.
///
/// This tests the scenario where a document has a title like "Jon Sjöstrand"
/// and another document references it via `[[Jon Sjöstrand]]`.
#[test]
fn wiki_link_with_non_ascii_title_resolves_correctly() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with Swedish characters in the title
    let target_md = "# Jon Sjöstrand\n\nSome content about Jon.\n";
    let target_path = root.join("notes/jon-sjostrand.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Document referencing the above via wiki link with same non-ASCII chars
    let source_md = "[[Jon Sjöstrand]]\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    // Parse both documents
    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument {
                path: target_path.clone(),
                rel_path: target_rel,
                structure: target_structure,
            },
            ResolveDocument {
                path: source_path.clone(),
                rel_path: source_rel,
                structure: source_structure,
            },
        ],
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[Jon Sjöstrand]] to resolve, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
}

/// Verify that wiki links with non-ASCII heading anchors resolve correctly.
#[test]
fn wiki_link_with_non_ascii_heading_anchor_resolves_correctly() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with Swedish characters in both title and heading
    let target_md = "# Jon Sjöstrand\n\n## Kontaktinformation\n\nEmail: jon@example.com\n";
    let target_path = root.join("notes/jon-sjostrand.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Wiki link referencing the document and a non-ASCII heading
    let source_md = "[[Jon Sjöstrand#Kontaktinformation]]\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument {
                path: target_path.clone(),
                rel_path: target_rel,
                structure: target_structure,
            },
            ResolveDocument {
                path: source_path.clone(),
                rel_path: source_rel,
                structure: source_structure,
            },
        ],
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[Jon Sjöstrand#Kontaktinformation]] to resolve, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
}

/// Reproduce the mojibake scenario: the link text in the source file contains
/// mojibake (UTF-8 bytes misinterpreted as Latin-1), e.g. "SjÃ¶strand" instead
/// of "Sjöstrand". This should NOT match the document title "Jon Sjöstrand".
#[test]
fn wiki_link_with_mojibake_does_not_match_correct_title() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with correct UTF-8 title
    let target_md = "# Tommy Sjöstrand\n\nSome content.\n";
    let target_path = root.join("notes/tommy.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Source file with MOJIBAKE in the wiki link (UTF-8 bytes read as Latin-1)
    // "ö" (U+00F6) in UTF-8 is bytes C3 B6, which as Latin-1 is "Ã¶"
    // So "Sjöstrand" becomes "SjÃ¶strand" in mojibake
    let source_md = "[[Tommy SjÃ¶strand]]\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument {
                path: target_path.clone(),
                rel_path: target_rel,
                structure: target_structure,
            },
            ResolveDocument {
                path: source_path.clone(),
                rel_path: source_rel,
                structure: source_structure,
            },
        ],
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    // This SHOULD be broken because the slugs don't match.
    // BUG: The error message shows double-encoded mojibake "SjÃÂ¶strand"
    // instead of the expected "SjÃ¶strand".
    assert_eq!(
        broken_links.len(),
        1,
        "Expected 1 broken link for mojibake mismatch (slugs don't match)"
    );
}

/// Verify that wiki links with explicit paths resolve against extra_folders.
///
/// Scenario:
/// - Source doc in the main project references `[[/people/john-doe|Some Name]]`
/// - Target doc lives in an extra folder (`../ext_project/people/john-doe.md`)
/// - Target has title `# John Doe`
#[test]
fn wiki_link_explicit_path_resolves_in_extra_folders() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the extra project outside the main root
    let ext_root = root.parent().unwrap().join("ext_project");

    // Target document in the extra project
    let target_md = "# John Doe\n\nBio content.\n";
    let target_path = ext_root.join("people/john-doe.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Source document in the main project with explicit wiki link
    let source_md = "[[/people/john-doe|Some Name]]\n";
    let source_path = root.join("notes/reference.md");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&ext_root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![ResolveDocument {
            path: source_path.clone(),
            rel_path: source_rel,
            structure: source_structure,
        }],
        extra_documents: vec![ResolveDocument {
            path: target_path.clone(),
            rel_path: target_rel,
            structure: target_structure,
        }],
        config: Config::default(),
        extra_folder_roots: vec![ext_root.clone()],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[/people/john-doe|Some Name]] to resolve in extra_folders, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );

    // Verify the resolved reference points to the extra project doc
    assert_eq!(graph.resolved_references.len(), 1);
    let ref_dest = &graph.resolved_references[0];
    assert_eq!(ref_dest.destinations.len(), 1);
    assert_eq!(ref_dest.destinations[0].path, target_path);
}
#[test]
fn inline_link_with_non_ascii_filename_resolves_correctly() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with Swedish characters in the filename (no spaces)
    let target_md = "# Jon Männik\n\nSome content.\n";
    let target_path = root.join("notes/Jon-Männik.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Document referencing via inline markdown link with non-ASCII in the path
    // Using angle brackets to properly handle the non-ASCII URL
    let source_md = "[Jon Männik](<Jon-Männik.md>)\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument {
                path: target_path.clone(),
                rel_path: target_rel,
                structure: target_structure,
            },
            ResolveDocument {
                path: source_path.clone(),
                rel_path: source_rel,
                structure: source_structure,
            },
        ],
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected inline link [Jon Männik](<Jon-Männik.md>) to resolve, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// Folder link resolution tests
// ============================================================================

#[test]
fn folder_link_to_existing_directory_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("cases/20250911-audit")).unwrap();
    fs::write(root.join("cases/20250911-audit/README.md"), "# Audit").unwrap();

    let doc = write_document(
        root,
        "docs/index.md",
        "[Case Audit](../cases/20250911-audit/)",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::DNL002),
        "folder link to existing directory should not produce broken link diagnostic"
    );
    assert_eq!(graph.resolved_references.len(), 1);
    assert!(matches!(
        graph.resolved_references[0].destinations[0].kind,
        ResolveDestinationKind::Directory
    ));
}

#[test]
fn folder_link_to_missing_directory_is_broken() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "docs/index.md",
        "[Missing Case](../cases/20250999-missing/)",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();
    assert_eq!(broken.len(), 1, "folder link to missing directory should be broken");
}

#[test]
fn folder_link_to_file_not_directory_is_broken() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::write(root.join("cases"), "some content").unwrap();

    let doc = write_document(
        root,
        "docs/index.md",
        "[Cases](../cases/)",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();
    assert_eq!(broken.len(), 1, "folder link to a file should be broken");
}

#[test]
fn wiki_link_to_folder_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("cases/audit")).unwrap();

    let doc = write_document(
        root,
        "docs/index.md",
        "[[/cases/audit/]]",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::DNL002),
        "wiki link to existing folder should resolve"
    );
}

#[test]
fn folder_link_with_anchor_is_unresolved() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("cases/audit")).unwrap();

    let doc = write_document(
        root,
        "docs/index.md",
        "[Case Audit](../cases/audit/#summary)",
    );

    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();
    assert_eq!(broken.len(), 1, "folder link with anchor should be unresolved");
}

#[test]
fn folder_link_in_extra_folder_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    let extra_root = temp.path().join("extra");

    fs::create_dir_all(extra_root.join("shared/templates")).unwrap();

    let doc = write_document(
        &root,
        "docs/index.md",
        "[Templates](/shared/templates/)",
    );

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![doc],
        extra_documents: vec![],
        config: Config::default(),
        extra_folder_roots: vec![extra_root.clone()],
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::DNL002),
        "folder link to directory in extra folder should resolve"
    );
}

#[test]
fn non_folder_link_to_directory_name_unchanged() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("cases")).unwrap();

    // Link without trailing slash - should NOT be treated as a folder link
    let doc = write_document(
        root,
        "docs/index.md",
        "[Cases](../cases)",
    );

    let graph = resolve_graph(root, vec![doc]);
    let dir_resolved: Vec<_> = graph
        .resolved_references
        .iter()
        .flat_map(|r| r.destinations.iter())
        .filter(|d| matches!(d.kind, ResolveDestinationKind::Directory))
        .collect();
    assert!(
        dir_resolved.is_empty(),
        "link without trailing slash should NOT resolve as a directory"
    );
}
