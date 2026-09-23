use downlint::config::Config;
use downlint::diagnostics::{
    DiagnosticCode, DiagnosticConfig, DiagnosticSeverity, check_diagnostics,
};
use downlint::parser::{ParseOptions, Ref, parse_document};
use downlint::resolution::{
    ResolveDestinationKind, ResolveDocument, ResolveInput, Slug, prefix::PrefixIndex,
    resolve_links,
};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to create a document file and return a ResolveDocument.
fn write_document(root: &std::path::Path, rel: &str, content: &str) -> ResolveDocument {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    let structure = parse_document(content, ParseOptions::default());
    let rel_path = path.strip_prefix(root).unwrap().to_path_buf();
    ResolveDocument::primary(path, rel_path, structure)
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
    let input = make_input(root, docs, vec![], config, false, Some(prefix_index));
    resolve_links(input)
}

/// Build a `ResolveInput` with all the new URI fields defaulted. Used by
/// tests that don't care about `[uri.mappings]` so they don't have to spell
/// out the new fields at every site.
fn make_input(
    root: &std::path::Path,
    documents: Vec<ResolveDocument>,
    mounts: Vec<downlint::utils::ResolvedMount>,
    config: Config,
    single_file: bool,
    prefix_index: Option<PrefixIndex>,
) -> ResolveInput {
    let prefix_index = prefix_index.unwrap_or_else(|| prefix_index_for(&documents));
    ResolveInput {
        root: root.to_path_buf(),
        documents,
        mounts,
        conflicts: vec![],
        config,
        single_file,
        prefix_index,
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    }
}

/// Adapter: tests don't construct a real `Workspace`, so we pass a synthetic
/// one with an empty `DiscoveredFolder` and the default `Config` that comes
/// from `resolve_links`.
fn run_diagnostics(
    graph: &downlint::resolution::ConnectionGraph,
    config: &DiagnosticConfig,
) -> Vec<downlint::diagnostics::Diagnostic> {
    check_diagnostics(graph, config)
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::LinkBroken),
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());
    let broken = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == DiagnosticCode::LinkBroken)
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != DiagnosticCode::LinkBroken),
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
// RFC 0013 — Obsidian-compatible path resolution
// ============================================================================

/// RFC 0013: a bare wiki path `[[shared/b]]` resolves against the workspace
/// root (not the containing document's directory), with `.md` optional.
#[test]
fn bare_wiki_path_resolves_root_relative() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source = write_document(root, "notes/a.md", "[[shared/b]]\n");
    let target = write_document(root, "shared/b.md", "# B\n");

    let graph = resolve_graph(root, vec![source, target.clone()]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
        "expected bare wiki path to resolve root-relative, got: {diagnostics:?}"
    );
    let ref_ = graph
        .resolved_references
        .iter()
        .find(|r| matches!(&r.reference, Ref::Wiki { target, .. } if target == "shared/b"))
        .expect("expected resolved wiki reference");
    assert_eq!(ref_.destinations.len(), 1);
    assert_eq!(ref_.destinations[0].path, target.path);
}

/// RFC 0013: a bare wiki path resolves to the same document regardless of the
/// containing document's location (determinism — the Obsidian rationale).
#[test]
fn bare_wiki_path_deterministic_regardless_of_source() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source_sub = write_document(root, "notes/a.md", "[[shared/b]]\n");
    let source_root = write_document(root, "README.md", "[[shared/b]]\n");
    let target = write_document(root, "shared/b.md", "# B\n");

    let graph = resolve_graph(root, vec![source_sub, source_root, target.clone()]);
    let refs: Vec<_> = graph
        .resolved_references
        .iter()
        .filter(|r| matches!(&r.reference, Ref::Wiki { target, .. } if target == "shared/b"))
        .collect();
    assert_eq!(refs.len(), 2, "expected both sources to resolve, got: {refs:?}");
    for r in &refs {
        assert_eq!(r.destinations.len(), 1);
        assert_eq!(r.destinations[0].path, target.path);
    }
}

/// RFC 0013: `[[../shared/b]]` from `notes/` normalizes to `shared/b` and
/// resolves as a **document** (not the attachment fallback).
#[test]
fn dot_relative_wiki_path_resolves_as_document() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source = write_document(root, "notes/a.md", "[[../shared/b]]\n");
    let target = write_document(root, "shared/b.md", "# B\n");

    let graph = resolve_graph(root, vec![source, target.clone()]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
        "expected dot-relative wiki path to resolve, got: {diagnostics:?}"
    );
    let ref_ = graph
        .resolved_references
        .iter()
        .find(|r| matches!(&r.reference, Ref::Wiki { target, .. } if target == "../shared/b"))
        .expect("expected resolved wiki reference");
    assert_eq!(ref_.destinations.len(), 1);
    assert!(matches!(
        ref_.destinations[0].kind,
        ResolveDestinationKind::Document
    ));
    assert_eq!(ref_.destinations[0].path, target.path);
}

/// RFC 0013: anchors on dot-relative wiki links are validated (previously
/// silently unvalidated because the link resolved as an attachment).
#[test]
fn dot_relative_wiki_anchor_validated() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source = write_document(root, "notes/a.md", "[[../shared/b.md#missing]]\n");
    let target = write_document(root, "shared/b.md", "# B\n\n## Section\n");

    let graph = resolve_graph(root, vec![source, target]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().any(|d| d.code == DiagnosticCode::LinkBrokenAnchor),
        "expected broken-anchor for nonexistent section, got: {diagnostics:?}"
    );
}

/// RFC 0013: markdown links keep standard source-relative semantics and require
/// the extension — `[x](shared/b)` from `notes/` does not resolve to
/// `shared/b.md`.
#[test]
fn markdown_bare_path_stays_source_relative() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source = write_document(root, "notes/a.md", "[x](shared/b)\n");
    let target = write_document(root, "shared/b.md", "# B\n");

    let graph = resolve_graph(root, vec![source, target]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().any(|d| d.code == DiagnosticCode::LinkBroken),
        "expected markdown bare path (no ext) to be broken, got: {diagnostics:?}"
    );
}

/// RFC 0013: `.md` is optional for wiki path targets — `[[shared/b]]` and
/// `[[shared/b.md]]` both resolve to the same document.
#[test]
fn wiki_path_extension_optional() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let source = write_document(root, "notes/a.md", "[[shared/b]]\n[[shared/b.md]]\n");
    let target = write_document(root, "shared/b.md", "# B\n");

    let graph = resolve_graph(root, vec![source, target.clone()]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
        "expected both extension forms to resolve, got: {diagnostics:?}"
    );
    for t in ["shared/b", "shared/b.md"] {
        let ref_ = graph
            .resolved_references
            .iter()
            .find(|r| matches!(&r.reference, Ref::Wiki { target, .. } if target == t))
            .unwrap_or_else(|| panic!("expected resolved wiki reference for {t}"));
        assert_eq!(ref_.destinations.len(), 1);
        assert_eq!(ref_.destinations[0].path, target.path);
    }
}

// ============================================================================
// Non-ASCII (accented Latin) character tests
// ============================================================================

/// Verify that Slug::from_heading_text handles accented (non-ASCII) characters correctly.
/// This is a unit-level check to ensure the slug generation is deterministic
/// for non-ASCII input.
#[test]
fn slug_generation_with_accented_chars() {
    // accented letters should be preserved in the slug (they're non-ASCII letters)
    assert_eq!(
        Slug::from_heading_text("José García").as_str(),
        "josé-garcía"
    );
    assert_eq!(Slug::from_heading_text("Zoë Müller").as_str(), "zoë-müller");
    assert_eq!(
        Slug::from_heading_text("Información de contacto").as_str(),
        "información-de-contacto"
    );

    // é should also be preserved
    assert_eq!(
        Slug::from_heading_text("Renée Dubois").as_str(),
        "renée-dubois"
    );
}

/// Verify that wiki links referencing documents with non-ASCII characters
/// (accented Latin, e.g. é, í) in the title are resolved correctly.
///
/// This tests the scenario where a document has a title like "José García"
/// and another document references it via `[[José García]]`.
#[test]
fn wiki_link_with_non_ascii_title_resolves_correctly() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with accented characters in the title
    let target_md = "# José García\n\nSome content about José.\n";
    let target_path = root.join("notes/jose-garcia.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Document referencing the above via wiki link with same non-ASCII chars
    let source_md = "[[José García]]\n";
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
            ResolveDocument::primary(target_path.clone(), target_rel, target_structure),
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
        ],
        mounts: vec![],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[José García]] to resolve, but got broken links: {:?}",
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

    // Document with accented characters in both title and heading
    let target_md = "# José García\n\n## Información de contacto\n\nEmail: jose@example.com\n";
    let target_path = root.join("notes/jose-garcia.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Wiki link referencing the document and a non-ASCII heading
    let source_md = "[[José García#Información de contacto]]\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument::primary(target_path.clone(), target_rel, target_structure),
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
        ],
        mounts: vec![],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[José García#Información de contacto]] to resolve, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
}

/// Reproduce the mojibake scenario: the link text in the source file contains
/// mojibake (UTF-8 bytes misinterpreted as Latin-1), e.g. "MÃ¼ller" instead
/// of "Müller". This should NOT match the document title "Tom Müller".
#[test]
fn wiki_link_with_mojibake_does_not_match_correct_title() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Document with correct UTF-8 title
    let target_md = "# Tom Müller\n\nSome content.\n";
    let target_path = root.join("notes/tommy.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Source file with MOJIBAKE in the wiki link (UTF-8 bytes read as Latin-1)
    // "ü" (U+00FC) in UTF-8 is bytes C3 BC, which as Latin-1 is "Ã¼"
    // So "Müller" becomes "MÃ¼ller" in mojibake
    let source_md = "[[Tom MÃ¼ller]]\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument::primary(target_path.clone(), target_rel, target_structure),
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
        ],
        mounts: vec![],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    // This SHOULD be broken because the slugs don't match.
    // BUG: The error message shows double-encoded mojibake "MÃÂ¼ller"
    // instead of the expected "MÃ¼ller".
    assert_eq!(
        broken_links.len(),
        1,
        "Expected 1 broken link for mojibake mismatch (slugs don't match)"
    );
}

/// Verify that a workspace-absolute wiki link resolves to a mounted document
/// via its namespace path (RFC 0010: mounts are co-equal, not a fallback).
///
/// Scenario:
/// - A mount (no prefix) at `../ext_project` contains `people/john-doe.md`
/// - Source doc in the main project references `[[/people/john-doe|Some Name]]`
/// - The workspace-absolute target matches the mounted doc's namespace path
#[test]
fn wiki_link_explicit_path_resolves_in_mount() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the mounted project outside the main root
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    // Target document in the mounted project
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

    // The mounted doc is co-equal: it lives in the main index with its
    // namespace path (mount-root-relative, no prefix) and mount attribution.
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: target_rel,
        structure: target_structure,
        namespace_rel_path: std::path::PathBuf::from("people/john-doe"),
        mount: Some("ext_project".to_string()),
        is_source: false,
    };

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
            target_doc,
        ],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: false,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[/people/john-doe|Some Name]] to resolve in the mount, but got broken links: {:?}",
        broken_links
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );

    // Verify the resolved reference points to the mounted doc
    assert_eq!(graph.resolved_references.len(), 1);
    let ref_dest = &graph.resolved_references[0];
    assert_eq!(ref_dest.destinations.len(), 1);
    assert_eq!(ref_dest.destinations[0].path, target_path);
}

/// Verify that a wiki link whose target text contains `/` resolves to a
/// mounted document via title-slug matching (RFC 0010).
///
/// Scenario:
/// - Source doc references `[[Team knowledge transfer (QA/DB)]]`
/// - The target text contains `/` which makes `is_explicit_path()` return true
/// - Target doc lives in a mount with a different filename
/// - Target has title `# Team knowledge transfer (QA/DB)`
/// - Resolution should succeed via title slug matching
#[test]
fn wiki_link_with_slash_in_target_resolves_via_title_slug_in_mount() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the extra project outside the main root
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    // Target document in the extra project with `/` in title
    let target_md = "# Team knowledge transfer (QA/DB)\n\nBio content.\n";
    let target_path = ext_root.join("notes/kt-session.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Source document in the main project with a wiki link containing `/` in target
    let source_md = "[[Team knowledge transfer (QA/DB)]]\n";
    let source_path = root.join("notes/reference.md");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&ext_root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: target_rel,
        structure: target_structure,
        namespace_rel_path: std::path::PathBuf::from("notes/kt-session"),
        mount: Some("ext_project".to_string()),
        is_source: false,
    };
    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
            target_doc,
        ],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: false,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected wiki link [[Team knowledge transfer (QA/DB)]] to resolve in the mount via title slug, but got broken links: {:?}",
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

    // Document with accented characters in the filename (no spaces)
    let target_md = "# Zoë Müller\n\nSome content.\n";
    let target_path = root.join("notes/Zoë-Müller.md");
    fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    fs::write(&target_path, target_md).unwrap();

    // Document referencing via inline markdown link with non-ASCII in the path
    // Using angle brackets to properly handle the non-ASCII URL
    let source_md = "[Zoë Müller](<Zoë-Müller.md>)\n";
    let source_path = root.join("notes/reference.md");
    fs::write(&source_path, source_md).unwrap();

    let target_structure = parse_document(target_md, ParseOptions::default());
    let source_structure = parse_document(source_md, ParseOptions::default());

    let target_rel = target_path.strip_prefix(&root).unwrap().to_path_buf();
    let source_rel = source_path.strip_prefix(&root).unwrap().to_path_buf();

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![
            ResolveDocument::primary(target_path.clone(), target_rel, target_structure),
            ResolveDocument::primary(source_path.clone(), source_rel, source_structure),
        ],
        mounts: vec![],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();

    assert!(
        broken_links.is_empty(),
        "Expected inline link [Zoë Müller](<Zoë-Müller.md>) to resolve, but got broken links: {:?}",
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    assert_eq!(broken.len(), 1, "folder link with anchor should be unresolved");
}

#[test]
fn folder_link_in_mount_resolves() {
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
        mounts: vec![downlint::utils::ResolvedMount {
            path: extra_root.clone(),
            r#as: None,
            lint: false,
            attribution: "extra".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
        "folder link to directory in a mount should resolve"
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
fn obsidian_prefix_multi_match_emits_link_ambiguous() {
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
    let index = write_document(root, "notes/index.md", "[[missing]]\n");
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());
    let link_broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    assert_eq!(link_broken.len(), 1);
    assert!(
        link_broken[0].message.contains("Hint: enable 'wiki.obsidian_prefix'"),
        "expected hint in link/broken message, got: {}",
        link_broken[0].message
    );
    assert!(
        link_broken[0].message.contains("20260801-topic-a-sub-x.md"),
        "expected candidate filename in hint, got: {}",
        link_broken[0].message
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

    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());
    let link_broken = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::LinkBroken)
        .unwrap();
    assert!(link_broken.message.contains("(+1 more)"));
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
        mounts: vec![],
        conflicts: vec![],
        config: config_with_obsidian_prefix(true),
        single_file: true,
        prefix_index,
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
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
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());
    assert!(
        diagnostics.iter().all(|d| d.code != DiagnosticCode::LinkBroken),
        "folder-link must not emit link/broken"
    );
}

// ============================================================================
// In-page anchor link tests
//
// Markdown `[text](#anchor)` and `[[#anchor]]` links are in-page anchor
// references, not file references. The diagnostic for an unresolved anchor
// must use a distinct code (`link/broken-anchor`) and message, not the generic
// `link/broken` "Broken link" message that is reserved for missing files.
// Tolerant matching (collapsing consecutive `-`) lets near-miss anchors still
// resolve.
// ============================================================================

#[test]
fn inline_anchor_to_existing_heading_resolves() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/page.md",
        "\
## Overview

[link to overview](#overview)
",
    );
    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        graph.resolved_references.iter().any(|r| matches!(
            r.reference,
            Ref::Inline { ref target, ref anchor, .. } if target.is_empty() && anchor.as_deref() == Some("overview")
        )),
        "expected the inline anchor link to resolve, got refs: {:?}",
        graph.resolved_references
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::LinkBroken && d.code != DiagnosticCode::LinkBrokenAnchor),
        "expected no broken-link or broken-anchor diagnostics, got: {:?}",
        diagnostics.iter().map(|d| (&d.code, d.message.as_str())).collect::<Vec<_>>()
    );
}

#[test]
fn inline_anchor_to_missing_heading_emits_link_broken_anchor_not_link_broken() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/page.md",
        "\
## Overview

[link](#appendix-a2)
",
    );
    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert_eq!(graph.resolved_references.len(), 0);
    assert_eq!(graph.unresolved_references.len(), 1);

    let link_broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    let link_broken_anchor: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
        .collect();

    assert!(
        link_broken.is_empty(),
        "inline anchor miss must NOT emit link/broken (broken file link), got: {:?}",
        link_broken.iter().map(|d| d.message.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(link_broken_anchor.len(), 1, "expected exactly one link/broken-anchor diagnostic");
    assert!(
        link_broken_anchor[0].message.starts_with("Broken anchor:"),
        "expected message to start with 'Broken anchor:', got: {:?}",
        link_broken_anchor[0].message
    );
    assert!(
        link_broken_anchor[0].message.contains("appendix-a2"),
        "expected message to contain the anchor, got: {:?}",
        link_broken_anchor[0].message
    );
}

#[test]
fn wiki_anchor_to_missing_heading_emits_link_broken_anchor_not_link_broken() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/page.md",
        "\
## Overview

[[#appendix-a2]]
",
    );
    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let link_broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    let link_broken_anchor: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
        .collect();

    assert!(
        link_broken.is_empty(),
        "wiki anchor miss must NOT emit link/broken, got: {:?}",
        link_broken.iter().map(|d| d.message.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(link_broken_anchor.len(), 1);
    assert!(link_broken_anchor[0].message.starts_with("Broken anchor:"));
}

/// Regression test: the original bug from `~/projects2/wang-lei`. The link
/// `[Appendix A2](#a2-cuga-bimetallic-for-c₂-at-high-rates-nat-commun-1551466-2024--closest-analog-to-our-cu-zn)`
/// references a heading that exists. The heading slug (after downlint's
/// normalization) is the same as the link anchor after collapsing consecutive
/// dashes (`2024--closest` → `2024-closest`). Tolerant matching must rescue
/// this case.
#[test]
fn inline_anchor_tolerates_consecutive_dashes_in_em_dash_heading() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/20260719-wang-natcomm-2024-2025-lessons-and-opportunities.md",
        "\
some text [Appendix A2](#a2-cuga-bimetallic-for-c₂-at-high-rates-nat-commun-1551466-2024--closest-analog-to-our-cu-zn) below

## A2. CuGa bimetallic for C₂⁺ at high rates (*Nat. Commun.* 15:51466, 2024) — closest analog to our Cu-Zn
",
    );
    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        graph.resolved_references.iter().any(|r| matches!(
            r.reference,
            Ref::Inline { ref target, ref anchor, .. } if target.is_empty() && anchor.as_deref() == Some("a2-cuga-bimetallic-for-c₂-at-high-rates-nat-commun-1551466-2024--closest-analog-to-our-cu-zn")
        )),
        "expected tolerant matching to resolve the em-dash heading anchor"
    );
    // The link resolves via tolerant matching. With strict matching only, this
    // would still emit a link/broken-anchor. With tolerant matching, we resolve cleanly.
    // We don't assert anything about the resolved destination kind here — the
    // existing convention is to accept either Document or Heading; the key is
    // that the link is no longer reported as broken.
    assert!(
        graph.unresolved_references.is_empty(),
        "tolerant matching should have resolved the anchor; unresolved: {:?}",
        graph.unresolved_references
    );
    let link_broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    let link_broken_anchor: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
        .collect();
    assert!(link_broken.is_empty(), "must not emit link/broken for a resolved anchor");
    assert!(link_broken_anchor.is_empty(), "must not emit link/broken-anchor for a resolved anchor");
}

#[test]
fn inline_anchor_tolerant_miss_still_emits_link_broken_anchor() {
    // Tolerant matching only rescues near-misses where folding dashes matches
    // a real heading. A genuinely unknown anchor must still be flagged.
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let doc = write_document(
        root,
        "notes/page.md",
        "\
## Overview

[link](#missing)
",
    );
    let graph = resolve_graph(root, vec![doc]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert_eq!(graph.unresolved_references.len(), 1);
    let link_broken_anchor: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
        .collect();
    assert_eq!(link_broken_anchor.len(), 1, "a truly unknown anchor must still emit link/broken-anchor");
    assert!(
        link_broken_anchor[0]
            .message
            .contains("missing"),
        "link/broken-anchor message should contain the anchor text"
    );
}

#[test]
fn inline_anchor_cross_document_miss_emits_link_broken_anchor() {
    // Cross-doc anchor: file resolves, but the heading within it doesn't.
    // Must use link/broken-anchor (anchor), not link/broken (file).
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let target = write_document(
        root,
        "notes/other.md",
        "\
## Overview

some content
",
    );
    let source = write_document(
        root,
        "notes/source.md",
        "[link](./other.md#appendix-a2)\n",
    );
    let graph = resolve_graph(root, vec![target, source]);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    let link_broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    let link_broken_anchor: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
        .collect();

    assert!(
        link_broken.is_empty(),
        "cross-doc anchor miss must NOT emit link/broken, got: {:?}",
        link_broken.iter().map(|d| d.message.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(link_broken_anchor.len(), 1);
    assert!(link_broken_anchor[0].message.starts_with("Broken anchor:"));
}

/// A bare-stem wiki link matching a primary doc and a mounted doc (same stem)
/// is `link/ambiguous` (RFC 0010: co-equal, not silent primary-wins).
#[test]
fn mount_same_name_primary_and_mount_is_ambiguous() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    let primary_foo = write_document(&root, "notes/foo.md", "# Foo\n\nPrimary.\n");
    let target_md = "# Foo\n\nMounted.\n";
    let target_path = ext_root.join("foo.md");
    fs::write(&target_path, target_md).unwrap();
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: std::path::PathBuf::from("foo.md"),
        structure: parse_document(target_md, ParseOptions::default()),
        namespace_rel_path: std::path::PathBuf::from("foo"),
        mount: Some("ext_project".to_string()),
        is_source: false,
    };
    let source = write_document(&root, "notes/reference.md", "[[foo]]\n");

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![primary_foo, target_doc, source],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: false,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    assert_eq!(graph.ambiguous_references.len(), 1, "expected one ambiguous reference");
    assert_eq!(graph.ambiguous_references[0].destinations.len(), 2);
}

/// A same-stem clash is ambiguous, but the mount `prefix` path disambiguates.
#[test]
fn mount_prefix_disambiguates() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    let primary_foo = write_document(&root, "notes/foo.md", "# Foo\n\nPrimary.\n");
    let target_md = "# Foo\n\nMounted.\n";
    let target_path = ext_root.join("foo.md");
    fs::write(&target_path, target_md).unwrap();
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: std::path::PathBuf::from("foo.md"),
        structure: parse_document(target_md, ParseOptions::default()),
        namespace_rel_path: std::path::PathBuf::from("kb/foo"),
        mount: Some("/kb".to_string()),
        is_source: false,
    };
    let source = write_document(&root, "notes/reference.md", "[[foo]]\n[[/kb/foo]]\n");

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![primary_foo, target_doc, source],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: Some("/kb".to_string()),
            lint: false,
            attribution: "/kb".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    // [[foo]] is ambiguous (primary + mounted both have stem "foo")
    assert_eq!(graph.ambiguous_references.len(), 1, "expected [[foo]] to be ambiguous");
    // [[/kb/foo]] resolves to the mounted doc via the prefix
    assert_eq!(graph.resolved_references.len(), 1, "expected [[/kb/foo]] to resolve");
    assert_eq!(graph.resolved_references[0].destinations[0].path, target_path);
}

/// A relative markdown link in a primary doc does not cross into a mount
/// (RFC 0010: relative paths resolve against the containing doc's directory).
#[test]
fn mount_relative_markdown_link_does_not_cross() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    let target_md = "# Foo\n\nMounted.\n";
    let target_path = ext_root.join("foo.md");
    fs::write(&target_path, target_md).unwrap();
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: std::path::PathBuf::from("foo.md"),
        structure: parse_document(target_md, ParseOptions::default()),
        namespace_rel_path: std::path::PathBuf::from("foo"),
        mount: Some("ext_project".to_string()),
        is_source: false,
    };
    // Relative link resolves to notes/foo.md (filesystem), which does not exist.
    let source = write_document(&root, "notes/reference.md", "[foo](foo.md)\n");

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![source, target_doc],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: false,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    assert!(
        graph.resolved_references.is_empty(),
        "relative link must not cross into the mount"
    );
    assert_eq!(graph.unresolved_references.len(), 1);
}

/// A mounted doc with `lint = true` is a source: its internal links are
/// diagnosed, and the diagnostic is attributed to the mount (RFC 0010).
#[test]
fn mount_lint_true_lints_and_attributes_internal_links() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    let target_md = "# Foo\n\n[[missing]]\n";
    let target_path = ext_root.join("foo.md");
    fs::write(&target_path, target_md).unwrap();
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: std::path::PathBuf::from("foo.md"),
        structure: parse_document(target_md, ParseOptions::default()),
        namespace_rel_path: std::path::PathBuf::from("foo"),
        mount: Some("ext_project".to_string()),
        is_source: true, // lint = true
    };

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![target_doc],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: true,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());
    let broken: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::LinkBroken)
        .collect();
    assert_eq!(broken.len(), 1, "expected the mounted doc's broken link to be diagnosed");
    assert_eq!(broken[0].mount.as_deref(), Some("ext_project"));
}

/// A mounted doc with `lint = false` (default) is a target only: its internal
/// links are NOT diagnosed (RFC 0010).
#[test]
fn mount_lint_false_does_not_lint_internal_links() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();

    let target_md = "# Foo\n\n[[missing]]\n";
    let target_path = ext_root.join("foo.md");
    fs::write(&target_path, target_md).unwrap();
    let target_doc = ResolveDocument {
        path: target_path.clone(),
        rel_path: std::path::PathBuf::from("foo.md"),
        structure: parse_document(target_md, ParseOptions::default()),
        namespace_rel_path: std::path::PathBuf::from("foo"),
        mount: Some("ext_project".to_string()),
        is_source: false, // lint = false
    };

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![target_doc],
        mounts: vec![downlint::utils::ResolvedMount {
            path: ext_root.clone(),
            r#as: None,
            lint: false,
            attribution: "ext_project".to_string(),
        }],
        conflicts: vec![],
        config: Config::default(),
        single_file: false,
        prefix_index: Default::default(),
        uri_resolver: downlint::resolution::uri::UriResolver::empty(),
        uri_opts: downlint::resolution::UriOptions::default(),
        uri_error: None,
    };

    let graph = resolve_links(input);
    assert!(
        graph.unresolved_references.is_empty(),
        "lint=false mounted doc must not be linted"
    );
}

/// Distinct files under a shared folder do NOT conflict (RFC 0011): a mount
/// merges into the namespace, and only an actual path collision errors. A
/// non-conflicting mount doc with `lint = true` is still linted.
#[test]
fn mount_distinct_files_under_shared_folder_no_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary has `notes/a.md`; mount has `notes/b.md` (distinct files).
    let primary_notes = root.join("notes");
    fs::create_dir_all(&primary_notes).unwrap();
    fs::write(primary_notes.join("a.md"), "# A\n").unwrap();
    let mount_notes = ext_root.join("notes");
    fs::create_dir_all(&mount_notes).unwrap();
    fs::write(mount_notes.join("b.md"), "# B\n\n[[missing]]\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("notes/a.md"),
                rel_path: PathBuf::from("notes/a.md"),
                text: downlint::utils::Text::new("# A\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: None,
                lint: true,
                attribution: "ext_project".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert!(input.conflicts.is_empty(), "distinct files must not conflict");

    let graph = resolve_links(input);
    // No conflict -> the mount doc is linted (lint=true), so its broken link
    // is diagnosed.
    let b_doc = graph
        .documents
        .iter()
        .find(|d| d.rel_path == PathBuf::from("notes/b.md"))
        .unwrap();
    assert!(b_doc.is_source, "non-conflicting mount doc must be a source");
    assert_eq!(graph.unresolved_references.len(), 1);
}

/// A prefix that shares a folder name with the primary is NOT a conflict when
/// the files are distinct (RFC 0011): the prefix is applied and the mount
/// merges into the folder.
#[test]
fn mount_prefix_distinct_files_no_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary has `kb/a.md`; mount has prefix `/kb` and doc `b.md`
    // (namespace path `kb/b.md`, distinct from `kb/a.md`).
    let primary_kb = root.join("kb");
    fs::create_dir_all(&primary_kb).unwrap();
    fs::write(primary_kb.join("a.md"), "# A\n").unwrap();
    fs::write(ext_root.join("b.md"), "# B\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("kb/a.md"),
                rel_path: PathBuf::from("kb/a.md"),
                text: downlint::utils::Text::new("# A\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: Some("/kb".to_string()),
                lint: false,
                attribution: "kb".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert!(input.conflicts.is_empty(), "distinct files must not conflict");

    let graph = resolve_links(input);
    // The prefix is applied: the mount doc's namespace path is `kb/b.md`.
    let b_doc = graph
        .documents
        .iter()
        .find(|d| d.rel_path == PathBuf::from("b.md"))
        .unwrap();
    assert_eq!(b_doc.namespace_rel_path, PathBuf::from("kb/b.md"));
}

/// A mount file at the same namespace path as a primary file is a
/// `mount/conflict` (RFC 0011).
#[test]
fn mount_same_path_file_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary and mount both have `notes/a.md`.
    let primary_notes = root.join("notes");
    fs::create_dir_all(&primary_notes).unwrap();
    fs::write(primary_notes.join("a.md"), "# A\n").unwrap();
    let mount_notes = ext_root.join("notes");
    fs::create_dir_all(&mount_notes).unwrap();
    fs::write(mount_notes.join("a.md"), "# A2\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("notes/a.md"),
                rel_path: PathBuf::from("notes/a.md"),
                text: downlint::utils::Text::new("# A\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: None,
                lint: true,
                attribution: "ext_project".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert_eq!(input.conflicts.len(), 1);
    assert_eq!(
        input.conflicts[0].kind,
        downlint::utils::MountConflictKind::PathCollision
    );
    assert!(input.conflicts[0].detail.contains("notes/a.md"));
}

/// A mount file whose stem matches a primary folder name (same location) is a
/// `mount/conflict` (RFC 0011) — protects from file-vs-folder edge cases.
#[test]
fn mount_file_vs_folder_name_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary has a `kb/` folder (via `kb/a.md`); mount has a file `kb.md`
    // (stem `kb`) at the root.
    let primary_kb = root.join("kb");
    fs::create_dir_all(&primary_kb).unwrap();
    fs::write(primary_kb.join("a.md"), "# A\n").unwrap();
    fs::write(ext_root.join("kb.md"), "# KB\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("kb/a.md"),
                rel_path: PathBuf::from("kb/a.md"),
                text: downlint::utils::Text::new("# A\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: None,
                lint: false,
                attribution: "ext_project".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert_eq!(input.conflicts.len(), 1);
    assert_eq!(
        input.conflicts[0].kind,
        downlint::utils::MountConflictKind::PathCollision
    );
    assert!(input.conflicts[0].detail.contains("kb.md"));
}

/// A mount folder whose name matches a primary file (same location) is a
/// `mount/conflict` (RFC 0011); files under that folder are targets only.
#[test]
fn mount_folder_vs_file_name_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary has a file `kb.md` (at root); mount has a `kb/` folder (via
    // `kb/a.md`) with a broken link.
    fs::write(root.join("kb.md"), "# KB\n").unwrap();
    let mount_kb = ext_root.join("kb");
    fs::create_dir_all(&mount_kb).unwrap();
    fs::write(mount_kb.join("a.md"), "# A\n\n[[missing]]\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("kb.md"),
                rel_path: PathBuf::from("kb.md"),
                text: downlint::utils::Text::new("# KB\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: None,
                lint: true,
                attribution: "ext_project".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert_eq!(input.conflicts.len(), 1);
    assert_eq!(
        input.conflicts[0].kind,
        downlint::utils::MountConflictKind::PathCollision
    );

    let graph = resolve_links(input);
    // The file under the conflicting folder is a target only (not linted).
    let a_doc = graph
        .documents
        .iter()
        .find(|d| d.rel_path == PathBuf::from("kb/a.md"))
        .unwrap();
    assert!(!a_doc.is_source, "file under conflicting folder must not be a source");
    // Its broken link is not diagnosed.
    assert!(graph.unresolved_references.is_empty());
}

/// A prefix that places a mount file at the same namespace path as a primary
/// file is a `mount/conflict` (RFC 0011).
#[test]
fn mount_prefix_same_path_conflict() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary has `kb/b.md`; mount has prefix `/kb` and doc `b.md`
    // (namespace path `kb/b.md`, same as the primary).
    let primary_kb = root.join("kb");
    fs::create_dir_all(&primary_kb).unwrap();
    fs::write(primary_kb.join("b.md"), "# B\n").unwrap();
    fs::write(ext_root.join("b.md"), "# B2\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("kb/b.md"),
                rel_path: PathBuf::from("kb/b.md"),
                text: downlint::utils::Text::new("# B\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: Some("/kb".to_string()),
                lint: false,
                attribution: "kb".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert_eq!(input.conflicts.len(), 1);
    assert_eq!(
        input.conflicts[0].kind,
        downlint::utils::MountConflictKind::PathCollision
    );
    assert!(input.conflicts[0].detail.contains("kb/b.md"));
}

/// A conflicting mount file is a target only (not linted) — its own broken
/// links are not diagnosed while the config is wrong (RFC 0011).
#[test]
fn mount_conflict_suspends_lint_of_conflicting_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let ext_tmp = TempDir::new().unwrap();
    let ext_root = ext_tmp.path().to_path_buf();
    fs::create_dir_all(&ext_root).unwrap();

    // Primary and mount both have `notes/a.md`; the mount's copy has a broken
    // link.
    let primary_notes = root.join("notes");
    fs::create_dir_all(&primary_notes).unwrap();
    fs::write(primary_notes.join("a.md"), "# A\n").unwrap();
    let mount_notes = ext_root.join("notes");
    fs::create_dir_all(&mount_notes).unwrap();
    fs::write(mount_notes.join("a.md"), "# A2\n\n[[missing]]\n").unwrap();

    let workspace = downlint::utils::Workspace {
        folder: downlint::utils::DiscoveredFolder {
            root: root.clone(),
            config_path: None,
            documents: vec![downlint::utils::WorkspaceDocument {
                path: root.join("notes/a.md"),
                rel_path: PathBuf::from("notes/a.md"),
                text: downlint::utils::Text::new("# A\n"),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![downlint::utils::ResolvedMount {
                path: ext_root.clone(),
                r#as: None,
                lint: true,
                attribution: "ext_project".to_string(),
            }],
        },
        mode: downlint::utils::WorkspaceMode::MultiFile,
        config: Config::default(),
    };

    let input = ResolveInput::from_workspace(&workspace);
    assert_eq!(input.conflicts.len(), 1);

    let graph = resolve_links(input);
    // The conflicting mount file is a target only (not linted).
    let mount_doc = graph
        .documents
        .iter()
        .find(|d| d.rel_path == PathBuf::from("notes/a.md") && d.mount.is_some())
        .unwrap();
    assert!(!mount_doc.is_source, "conflicting mount file must not be a source");
    // Its broken link is not diagnosed.
    assert!(graph.unresolved_references.is_empty());
}

// --- Colon in wiki-link targets (RFC 0021) ---
//
// A `:` in a target is not a URI scheme separator unless the target is a
// known web scheme, uses the `scheme://` form, or matches a configured
// `[[schemas]]` prefix. The colon shape is the asserted variation here
// (RFC 0020 keep-list rule), so the fixture names are not canonicalized.

/// A wiki link whose target contains a colon resolves via title-slug
/// matching: a document titled "Team: Knowledge" is reached by
/// `[[Team: Knowledge]]` (slug `team-knowledge` on both sides).
#[test]
fn wiki_link_with_colon_in_title_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let target_md = "# Team: Knowledge\n\nContent.\n";
    let source_md = "[[Team: Knowledge]]\n";

    let input = make_input(
        &root,
        vec![
            write_document(&root, "notes/team-knowledge.md", target_md),
            write_document(&root, "index.md", source_md),
        ],
        vec![],
        Config::default(),
        false,
        None,
    );
    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::LinkBroken),
        "Expected [[Team: Knowledge]] to resolve by title slug, got broken links: {:?}",
        diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

/// A wiki link to a file whose stem contains a colon resolves via stem
/// matching. The document's H1 is deliberately a different title so only
/// the stem rule can match.
#[test]
fn wiki_link_with_colon_in_stem_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let target_md = "# The Team Page\n\nContent.\n";
    let source_md = "[[Team: Knowledge]]\n";

    let input = make_input(
        &root,
        vec![
            write_document(&root, "Team: Knowledge.md", target_md),
            write_document(&root, "index.md", source_md),
        ],
        vec![],
        Config::default(),
        false,
        None,
    );
    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::LinkBroken),
        "Expected [[Team: Knowledge]] to resolve by stem, got broken links: {:?}",
        diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

/// The alias form `[[Team: Knowledge|alias]]` resolves the same as the
/// bare target.
#[test]
fn wiki_link_with_colon_alias_form_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let target_md = "# Team: Knowledge\n\nContent.\n";
    let source_md = "[[Team: Knowledge|the team page]]\n";

    let input = make_input(
        &root,
        vec![
            write_document(&root, "notes/team-knowledge.md", target_md),
            write_document(&root, "index.md", source_md),
        ],
        vec![],
        Config::default(),
        false,
        None,
    );
    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::LinkBroken),
        "Expected [[Team: Knowledge|the team page]] to resolve, got broken links: {:?}",
        diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

/// A colon target with no matching document is a plain broken link — it
/// must NOT be routed through RES-07, so no `uri/no-mapping` hint appears
/// even when `[[schemas]]` are configured.
#[test]
fn wiki_link_with_colon_no_match_is_plain_broken() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // A schema is configured but must not be consulted for a colon target.
    let mut config = Config::default();
    config.schemas = downlint::config::finalize_schemas(vec![
        downlint::config::schema::PartialSchema {
            uri: Some("onedrive://work/".to_string()),
            to: Some(root.to_str().unwrap().to_string()),
            ..Default::default()
        },
    ])
    .unwrap();

    let source_md = "[[Team: Knowledge]]\n";
    let input = make_input(
        &root,
        vec![write_document(&root, "index.md", source_md)],
        vec![],
        config.clone(),
        false,
        None,
    );
    // Rebuild the input with the configured resolver (make_input defaults
    // to an empty one).
    let input = ResolveInput {
        uri_resolver: downlint::resolution::uri::UriResolver::new(&config.schemas, &root)
            .unwrap(),
        ..input
    };

    let graph = resolve_links(input);
    // Info severity so the `uri/no-mapping` hint (if wrongly emitted) is
    // visible to the assertion below.
    let diagnostics = run_diagnostics(
        &graph,
        &DiagnosticConfig {
            min_severity: DiagnosticSeverity::Info,
            source_only: false,
        },
    );
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::LinkBroken),
        "expected link/broken for the unmatched colon target, got {codes:?}"
    );
    assert!(
        !codes.contains(&DiagnosticCode::UriNoMapping),
        "a colon target must not be treated as an unmapped URI, got {codes:?}"
    );
}

/// A markdown link whose destination contains a colon resolves as a file
/// path (the `:` is not a scheme separator for file-like destinations).
#[test]
fn markdown_link_with_colon_in_filename_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let source_md = "[the team page](Team: Knowledge.md)\n";

    let input = make_input(
        &root,
        vec![
            write_document(&root, "Team: Knowledge.md", "# The Team Page\n"),
            write_document(&root, "index.md", source_md),
        ],
        vec![],
        Config::default(),
        false,
        None,
    );
    let graph = resolve_links(input);
    let diagnostics = run_diagnostics(&graph, &DiagnosticConfig::default());

    assert!(
        diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::LinkBroken),
        "Expected [x](Team: Knowledge.md) to resolve, got broken links: {:?}",
        diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(graph.resolved_references.len(), 1);
}
