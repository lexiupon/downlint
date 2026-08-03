use downlint::config::Config;
use downlint::diagnostics::{DiagnosticCode, DiagnosticConfig, check_diagnostics};
use downlint::parser::{ParseOptions, Ref, parse_document};
use downlint::resolution::{
    ResolveDestinationKind, ResolveDocument, ResolveInput, Slug, prefix::PrefixIndex,
    resolve_links,
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

/// Build a prefix index from the stems of the given documents.
fn prefix_index_for(docs: &[ResolveDocument]) -> PrefixIndex {
    let entries = docs.iter().map(|d| (d.stem(), d.path.clone()));
    PrefixIndex::from_entries(entries)
}

/// Helper to build a ResolveInput from a root and list of documents.
fn resolve_graph(
    root: &std::path::Path,
    docs: Vec<ResolveDocument>,
) -> downlint::resolution::ConnectionGraph {
    resolve_graph_with_config(root, docs, Config::default())
}

/// Like `resolve_graph` but lets callers customize the config (e.g. flip
/// `wiki.obsidian_prefix`).
fn resolve_graph_with_config(
    root: &std::path::Path,
    docs: Vec<ResolveDocument>,
    config: Config,
) -> downlint::resolution::ConnectionGraph {
    let prefix_index = prefix_index_for(&docs);
    let input = ResolveInput {
        root: root.to_path_buf(),
        documents: docs,
        extra_documents: vec![],
        config,
        extra_folder_roots: vec![],
        single_file: false,
        prefix_index,
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
        prefix_index: Default::default(),
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
        prefix_index: Default::default(),
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
        prefix_index: Default::default(),
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
        prefix_index: Default::default(),
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

/// Verify that wiki links whose target text contains `/` resolve in extra_folders
/// via title slug matching.
///
/// Scenario:
/// - Source doc references `[[Mateusz Know-How transfer (DevOps/ADP)]]`
/// - The target text contains `/` which makes `is_explicit_path()` return true
/// - Target doc lives in an extra folder with a different filename
/// - Target has title `# Mateusz Know-How transfer (DevOps/ADP)`
/// - Resolution should succeed via title slug matching
#[test]
fn wiki_link_with_slash_in_target_resolves_via_title_slug_in_extra_folders() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the extra project outside the main root
    let ext_root = root.parent().unwrap().join("ext_project");

    // Target document in the extra project with `/` in title
    let target_md = "# Mateusz Know-How transfer (DevOps/ADP)\n\nBio content.\n";
    let target_path = ext_root.join("notes/kt-session.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Source document in the main project with a wiki link containing `/` in target
    let source_md = "[[Mateusz Know-How transfer (DevOps/ADP)]]\n";
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
        prefix_index: Default::default(),
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[Mateusz Know-How transfer (DevOps/ADP)]] to resolve in extra_folders via title slug, but got broken links: {:?}",
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
        prefix_index: Default::default(),
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
        prefix_index: Default::default(),
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

// -------------------------------------------------------------------------
// wiki obsidian_prefix tests
// -------------------------------------------------------------------------

fn config_with_obsidian_prefix(enabled: bool) -> Config {
    let mut cfg = Config::default();
    cfg.wiki.obsidian_prefix = enabled;
    cfg
}

#[test]
fn obsidian_prefix_unique_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# 20260801 topic a sub x\n",
    );
    let index = write_document(root, "notes/index.md", "[[20260801-topic-a]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.resolved_references.len(), 1);
    assert!(
        graph
            .resolved_references[0]
            .destinations
            .iter()
            .any(|d| d.path.ends_with("20260801-topic-a-sub-x.md")),
        "expected resolution to the prefix-matched file"
    );
    assert!(graph.ambiguous_references.is_empty());
}

#[test]
fn obsidian_prefix_ambiguous_dnl001() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target_x = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );
    let target_y = write_document(
        root,
        "notes/20260801-topic-a-sub-y.md",
        "# y\n",
    );
    let index = write_document(root, "notes/index.md", "[[20260801-topic-a]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![target_x, target_y, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.ambiguous_references.len(), 1);
    assert_eq!(graph.ambiguous_references[0].target, "20260801-topic-a");
    assert!(graph.resolved_references.is_empty());
}

#[test]
fn obsidian_prefix_off_no_hint_no_match() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let other = write_document(root, "notes/abc.md", "# abc\n");
    let index = write_document(root, "notes/index.md", "[[xyz]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![other, index],
        config_with_obsidian_prefix(false),
    );
    assert_eq!(graph.unresolved_references.len(), 1);
    let payload = graph.unresolved_references[0].hint_payload.as_ref();
    assert!(
        payload.is_none() || payload.unwrap().is_empty(),
        "no partial matches → no hint payload"
    );
}

#[test]
fn obsidian_prefix_off_with_hint() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );

    let index = write_document(root, "notes/index.md", "[[20260801-topic-a]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(false),
    );
    assert_eq!(graph.unresolved_references.len(), 1);
    let payload = graph.unresolved_references[0]
        .hint_payload
        .as_ref()
        .expect("hint payload expected");
    assert_eq!(payload.len(), 1);

    // Check the diagnostic message contains the hint line.
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());
    let dnl002: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();
    assert_eq!(dnl002.len(), 1);
    assert!(
        dnl002[0].message.contains("Hint: enable 'wiki.obsidian_prefix'"),
        "expected hint in DNL002 message, got: {}",
        dnl002[0].message
    );
    assert!(
        dnl002[0].message.contains("20260801-topic-a-sub-x.md"),
        "expected candidate filename in hint, got: {}",
        dnl002[0].message
    );
}

#[test]
fn obsidian_prefix_hint_caps_at_five() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let mut docs: Vec<ResolveDocument> = Vec::new();
    for i in 0..6 {
        docs.push(write_document(
            root,
            &format!("notes/20260801-topic-{i:02}.md"),
            "# file\n",
        ));
    }
    let index = write_document(root, "notes/index.md", "[[20260801-topic]]\n");
    docs.push(index);
    let graph = resolve_graph_with_config(
        root,
        docs,
        config_with_obsidian_prefix(false),
    );
    let payload = graph.unresolved_references[0]
        .hint_payload
        .as_ref()
        .expect("hint payload");
    assert_eq!(payload.len(), 6);

    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());
    let dnl002 = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::DNL002)
        .unwrap();
    assert!(dnl002.message.contains("(+1 more)"));
}

/// Regression test: wiki-link targets that contain an internal `.` (e.g. version
/// numbers like `v2.5b`) used to be classified as explicit paths and skip the
/// `wiki.obsidian_prefix` fallback, leaving real links unresolved even though the
/// target file exists. With the fix, the `.` is only treated as an extension
/// separator when it is the last `.` and both the base and the extension are
/// non-empty, so `[[20260723-v2.5b-trust-region]]` now prefix-matches the file
/// `notes/20260723-v2.5b-trust-region.md`.
#[test]
fn obsidian_prefix_target_with_internal_dot_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let target = write_document(
        root,
        "notes/20260723-v2.5b-trust-region.md",
        "# 20260723 v2.5b trust region\n",
    );

    // The link target matches the file stem exactly, but the stem contains an
    // internal `.` (in `v2.5b`) that previously caused the resolver to treat the
    // target as explicit and skip prefix matching.
    let index = write_document(
        root,
        "notes/INDEX.md",
        "\
| 20260723 | result | trust region restart | [[20260723-v2.5b-trust-region]] |
",
    );

    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );

    assert_eq!(
        graph.resolved_references.len(),
        1,
        "expected exactly one resolved reference, got unresolved={}, ambiguous={}",
        graph.unresolved_references.len(),
        graph.ambiguous_references.len(),
    );
    assert!(
        graph
            .resolved_references[0]
            .destinations
            .iter()
            .any(|d| d.path.ends_with("20260723-v2.5b-trust-region.md")),
        "expected the resolved destination to be 20260723-v2.5b-trust-region.md, got: {:?}",
        graph.resolved_references[0].destinations
    );
    assert!(
        graph.unresolved_references.is_empty(),
        "expected no unresolved references, got: {:?}",
        graph.unresolved_references
    );
    assert!(graph.ambiguous_references.is_empty());
}

/// Even a partial prefix that contains the internal `.` (e.g. `20260723-v2.5b`,
/// which is a leading prefix of `20260723-v2.5b-trust-region`) should now resolve
/// because the `.` in `5b-trust-region` is not a valid extension separator.
#[test]
fn obsidian_prefix_target_with_internal_dot_prefix_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let target = write_document(
        root,
        "notes/20260723-v2.5b-trust-region.md",
        "# x\n",
    );

    // `20260723-v2.5b-` is a leading prefix of the file stem and contains an
    // internal `.`. The trailing fragment `trust-region` contains a `-`, so
    // the `.` is not treated as an extension separator and the prefix matcher
    // is allowed to run.
    let index = write_document(
        root,
        "notes/INDEX.md",
        "[[20260723-v2.5b-]]\n",
    );

    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );

    assert_eq!(graph.resolved_references.len(), 1);
    assert!(
        graph.resolved_references[0]
            .destinations
            .iter()
            .any(|d| d.path.ends_with("20260723-v2.5b-trust-region.md")),
        "expected the resolved destination to be 20260723-v2.5b-trust-region.md, got: {:?}",
        graph.resolved_references[0].destinations
    );
    assert!(graph.unresolved_references.is_empty());
    assert!(graph.ambiguous_references.is_empty());
}

/// Sanity check: real `.md` extensions must still be classified as explicit
/// (so the `obsidian_prefix` fallback is skipped and the explicit path matcher
/// runs). This guards against an over-broad fix.
#[test]
fn explicit_md_target_still_treated_as_explicit() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let target = write_document(
        root,
        "notes/20260801-topic-a.md",
        "# a\n",
    );

    let index = write_document(
        root,
        "notes/INDEX.md",
        "[[20260801-topic-a.md]]\n",
    );

    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );

    assert_eq!(
        graph.resolved_references.len(),
        1,
        "expected the explicit .md target to resolve via the path matcher"
    );
    assert!(graph.unresolved_references.is_empty());
    assert!(graph.ambiguous_references.is_empty());
}

#[test]
fn obsidian_prefix_alias_form_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );

    let index = write_document(
        root,
        "notes/index.md",
        "[[20260801-topic-a|Title]]\n",
    );
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn obsidian_prefix_embed_form_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );

    let index = write_document(
        root,
        "notes/index.md",
        "![[20260801-topic-a]]\n",
    );
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.resolved_references.len(), 1);
    // Per RFC 0004 behavior matrix: `![[20260801-topic-a]]` resolves with
    // `is_embed = true` preserved on the resolved reference.
    match &graph.resolved_references[0].reference {
        Ref::Wiki { is_embed, .. } => assert!(*is_embed, "embed form must preserve is_embed"),
        other => panic!("expected Ref::Wiki, got {:?}", other),
    }
}

#[test]
fn obsidian_prefix_heading_anchor() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "## Section\nbody\n",
    );

    let index = write_document(
        root,
        "notes/index.md",
        "[[20260801-topic-a#Section]]\n",
    );
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.resolved_references.len(), 1);
    assert!(
        matches!(
            graph.resolved_references[0].destinations[0].kind,
            ResolveDestinationKind::Heading
        ),
        "expected heading destination"
    );
}

#[test]
fn obsidian_prefix_explicit_path_wins() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    // No file named 20260801-topic-a exists, only the full stem.
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );
    // `[[20260801-topic-a]]` contains a `.` because of the structure? No, it's
    // fine. But `is_explicit_path` triggers on `.` — our target has no `.`
    // so it's not "explicit". Use a target that IS explicit: `notes/20260801`.
    let index = write_document(
        root,
        "notes/index.md",
        "[[notes/20260801]]\n",
    );
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    // `notes/20260801` is an explicit path that doesn't exist → falls through to
    // attachment fallback (none) → unresolved. Prefix matching does NOT apply.
    assert_eq!(graph.unresolved_references.len(), 1);
}

#[test]
fn obsidian_prefix_case_insensitive() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-TOPIC-A-sub-x.md",
        "# x\n",
    );

    let index = write_document(root, "notes/index.md", "[[20260801-topic-a]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn obsidian_prefix_does_not_match_suffix_only() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(root, "notes/xyz-topic.md", "# x\n");

    let index = write_document(root, "notes/index.md", "[[topic]]\n");
    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert!(
        graph.resolved_references.is_empty(),
        "suffix-only match must NOT resolve"
    );
    assert_eq!(graph.unresolved_references.len(), 1);
}

#[test]
fn obsidian_prefix_single_file_mode() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let target = write_document(
        root,
        "notes/20260801-topic-a-sub-x.md",
        "# x\n",
    );

    let index = write_document(root, "notes/index.md", "[[20260801-topic-a]]\n");
    let prefix_index = prefix_index_for(&[target.clone(), index.clone()]);
    let input = ResolveInput {
        root: root.to_path_buf(),
        documents: vec![target, index],
        extra_documents: vec![],
        config: config_with_obsidian_prefix(true),
        extra_folder_roots: vec![],
        single_file: true,
        prefix_index,
    };
    let graph = resolve_links(input);
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn obsidian_prefix_does_not_resolve_folder_link() {
    // Regression for RFC 0004 behavior matrix: a folder-link (`[[folder/]]`) must
    // resolve via the existing folder-link branch, never via the prefix index. The
    // prefix branch is reached only after the folder-link early-return; the trailing
    // slash is never a valid file stem prefix.
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("cases/audit")).unwrap();

    // Plant a file whose stem would match a hypothetical prefix of the folder name
    // so that the prefix branch would try to resolve if it were reached.
    let target = write_document(root, "cases-audit.md", "# decoy\n");
    let index = write_document(root, "docs/index.md", "[[/cases/audit/]]");

    let graph = resolve_graph_with_config(
        root,
        vec![target, index],
        config_with_obsidian_prefix(true),
    );
    assert_eq!(
        graph.resolved_references.len(),
        1,
        "folder-link must resolve to the folder, not to a prefix-matched file"
    );
    assert!(
        graph.unresolved_references.is_empty(),
        "folder-link must not be reported as unresolved"
    );
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());
    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::DNL002),
        "folder-link must not emit DNL002"
    );
}
