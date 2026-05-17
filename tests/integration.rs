use downlint::config::Config;
use downlint::diagnostics::{check_diagnostics, DiagnosticCode, DiagnosticConfig};
use downlint::parser::{ParseOptions, parse_document};
use downlint::resolution::{ResolveDocument, ResolveInput, resolve_links};
use std::fs;
use tempfile::TempDir;

/// Reproduce the false-positive broken-link warning for absolute paths to
/// attachments whose extensions are not in the default
/// `attachment_file_extensions` (e.g. `.xlsx`, `.docx`).
///
/// The files physically exist, but downlint reports them as broken because
/// `is_attachment_path()` returns `false` for unknown extensions, so the
/// on-disk existence check in `finalize_doc_or_attachment` is never reached.
#[test]
fn absolute_path_attachment_false_positive_for_unknown_extensions() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the attachment files (they physically exist)
    let xlsx_path = root.join("assets/finance-xls-2023/product-pricing/product-pricing-structure-team-input.xlsx");
    fs::create_dir_all(xlsx_path.parent().unwrap()).unwrap();
    fs::write(&xlsx_path, "").unwrap();

    let docx_path = root.join("assets/company-okrs/company-draft-okrs-2026.docx");
    fs::create_dir_all(docx_path.parent().unwrap()).unwrap();
    fs::write(&docx_path, "").unwrap();

    // Markdown with absolute-path links to those attachments
    let md = r#"
[sheet](/assets/finance-xls-2023/product-pricing/product-pricing-structure-team-input.xlsx)
[doc](/assets/company-okrs/company-draft-okrs-2026.docx)
"#;
    let path = root.join("notes/test.md");
    let rel_path = path.strip_prefix(&root).unwrap().to_path_buf();
    let structure = parse_document(md, ParseOptions::default());
    let doc = ResolveDocument {
        path,
        rel_path,
        structure,
    };

    // Use default config — .xlsx and .docx are NOT in attachment_file_extensions
    let input = ResolveInput {
        root: root.clone(),
        documents: vec![doc],
        extra_documents: vec![],
        config: Config::default(),
        single_file: false,
    };

    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(&graph, &DiagnosticConfig::default());

    // BUG: Both links are reported as broken even though the files exist
    // because .xlsx and .docx are not in the default attachment_file_extensions.
    let broken_links: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::DNL002)
        .collect();

    assert_eq!(
        broken_links.len(),
        2,
        "Expected 2 false-positive broken-link warnings for .xlsx and .docx (files exist but extensions are not recognized)"
    );

    // Verify the targets match the absolute paths
    let messages: Vec<_> = broken_links.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("product-pricing-structure-team-input.xlsx")),
        "Expected broken link for .xlsx file"
    );
    assert!(
        messages.iter().any(|m| m.contains("company-draft-okrs-2026.docx")),
        "Expected broken link for .docx file"
    );
}

/// Verify that adding .xlsx and .docx to `attachment_file_extensions`
/// resolves the false positive — the links are correctly resolved.
#[test]
fn absolute_path_attachment_resolved_when_extension_added_to_config() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Create the attachment files
    let xlsx_path = root.join("assets/data.xlsx");
    fs::create_dir_all(xlsx_path.parent().unwrap()).unwrap();
    fs::write(&xlsx_path, "").unwrap();

    let docx_path = root.join("assets/doc.docx");
    fs::create_dir_all(docx_path.parent().unwrap()).unwrap();
    fs::write(&docx_path, "").unwrap();

    let md = r#"
[sheet](/assets/data.xlsx)
[doc](/assets/doc.docx)
"#;
    let path = root.join("notes/test.md");
    let rel_path = path.strip_prefix(&root).unwrap().to_path_buf();
    let structure = parse_document(md, ParseOptions::default());
    let doc = ResolveDocument {
        path,
        rel_path,
        structure,
    };

    // Build config with extra attachment extensions
    let mut config = Config::default();
    config.core.attachment_file_extensions.push("xlsx".into());
    config.core.attachment_file_extensions.push("docx".into());

    let input = ResolveInput {
        root: root.clone(),
        documents: vec![doc],
        extra_documents: vec![],
        config,
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
        "Expected no broken-link warnings when .xlsx and .docx are in attachment_file_extensions, got: {:?}",
        broken_links
    );
}
