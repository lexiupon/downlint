//! End-to-end tests for `[[schemas]]` (RFC 0010, Phase 2): rewrite + stat +
//! verify, no warming, no caching.

use downlint::config::schema::PartialSchema;
use downlint::config::{Config, finalize_schemas};
use downlint::diagnostics::{DiagnosticCode, DiagnosticConfig, DiagnosticSeverity};
use downlint::resolution::{ConnectionGraph, ResolveInput, UriOptions, resolve_links};
use downlint::utils::{DiscoveredFolder, Workspace, WorkspaceMode};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Build a `Config` whose `[[schemas]]` section contains a single schema.
fn config_with_schema(uri: &str, to: &str, auto_verify: Option<bool>) -> Config {
    let mut config = Config::default();
    config.schemas = finalize_schemas(vec![PartialSchema {
        uri: Some(uri.to_string()),
        to: Some(to.to_string()),
        auto_verify,
        verify_cmd: None,
    }])
    .unwrap();
    config
}

/// Build a `Config` with a single schema that has a `verify_cmd`.
fn config_with_schema_and_verify(uri: &str, to: &str, verify_cmd: Vec<String>) -> Config {
    let mut config = Config::default();
    config.schemas = finalize_schemas(vec![PartialSchema {
        uri: Some(uri.to_string()),
        to: Some(to.to_string()),
        auto_verify: None,
        verify_cmd: Some(verify_cmd),
    }])
    .unwrap();
    config
}

/// Build a `Workspace` rooted at `root` with one document (`rel_path`) whose
/// body is `body`. Used to drive end-to-end scheme resolution through
/// `resolve_links`.
fn workspace_with_doc(root: &Path, rel_path: &str, body: &str) -> Workspace {
    let doc_path = root.join(rel_path);
    fs::create_dir_all(doc_path.parent().unwrap()).unwrap();
    fs::write(&doc_path, body).unwrap();
    Workspace {
        folder: DiscoveredFolder {
            root: root.to_path_buf(),
            config_path: Some(root.join(".downlint.toml")),
            documents: vec![downlint::utils::WorkspaceDocument {
                path: doc_path.clone(),
                rel_path: PathBuf::from(rel_path),
                text: downlint::utils::Text::new(body.to_string()),
                source: downlint::utils::DocumentSource::Disk,
            }],
            mounts: vec![],
        },
        mode: WorkspaceMode::MultiFile,
        config: Config::default(),
    }
}

fn build_graph(
    root: &Path,
    rel_path: &str,
    body: &str,
    config: Config,
    allow_sync: bool,
) -> ConnectionGraph {
    let mut workspace = workspace_with_doc(root, rel_path, body);
    workspace.config = config;
    let mut input = ResolveInput::from_workspace(&workspace);
    input.uri_opts = UriOptions {
        allow_sync,
        no_hints: false,
    };
    resolve_links(input)
}

fn run_diagnostics(
    graph: &ConnectionGraph,
    config: &DiagnosticConfig,
) -> Vec<downlint::diagnostics::Diagnostic> {
    downlint::diagnostics::check_diagnostics(graph, config)
}

#[test]
fn present_file_resolves_as_attachment() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets").join("audit");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("report.xlsx");
    fs::write(&asset, "fake").unwrap();

    let config = config_with_schema("onedrive://work/", assets.to_str().unwrap(), None);
    let body = "See [[onedrive://work/report.xlsx]] for details.\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert!(
        graph.unresolved_references.is_empty(),
        "expected no unresolved refs, got {:#?}",
        graph.unresolved_references
    );
    assert_eq!(graph.resolved_references.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations[0].path, asset);
}

/// A markdown inline link whose URI destination contains spaces (e.g. a
/// cloud-storage filename) resolves to the file with the space in its name.
/// Regression test for the parser truncating the destination at the first
/// space (which used to yield a `link/broken` for `.../Messaging`).
#[test]
fn markdown_link_uri_with_space_in_filename_resolves() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("Messaging BOM - 21May26.pdf");
    fs::write(&asset, "fake").unwrap();

    let config = config_with_schema("onedrive://work/assets", assets.to_str().unwrap(), None);
    // Markdown inline link (not a wiki link) with a space in the URI dest.
    let body = "[Messaging BOM - 21May26.pdf](onedrive://work/assets/Messaging BOM - 21May26.pdf)\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert!(
        graph.unresolved_references.is_empty(),
        "expected no unresolved refs, got {:#?}",
        graph.unresolved_references
    );
    assert_eq!(graph.resolved_references.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations[0].path, asset);
}

#[test]
fn missing_file_yields_broken_link() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let config = config_with_schema("onedrive://work/", assets.to_str().unwrap(), None);
    let body = "[[onedrive://work/missing.xlsx]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert_eq!(graph.unresolved_references.len(), 1);
    let unresolved = &graph.unresolved_references[0];
    assert_eq!(unresolved.target, "onedrive://work/missing.xlsx");
    // No-mapping hint should NOT fire because the prefix *does* match.
    assert!(!unresolved.uri_no_mapping_hint);

    let config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
        source_only: false,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, &config);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
}

#[test]
fn no_mapping_emits_hint_when_schemas_configured() {
    let tmp = TempDir::new().unwrap();
    // A schema is configured but doesn't match the target's scheme.
    let config = config_with_schema("onedrive://work/", tmp.path().to_str().unwrap(), None);
    let body = "[[s3://other/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert_eq!(graph.unresolved_references.len(), 1);
    assert!(graph.unresolved_references[0].uri_no_mapping_hint);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
        source_only: false,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, &diag_config);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
    assert!(
        codes.contains(&DiagnosticCode::UriNoMapping),
        "expected uri/no-mapping hint, got {codes:?}"
    );
}

#[test]
fn no_mapping_emits_no_hint_when_schemas_unconfigured() {
    let tmp = TempDir::new().unwrap();
    // `Config::default()` has no `[[schemas]]`. A URI-scheme link should still
    // resolve to a broken link, but no hint should fire.
    let body = "[[onedrive://work/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, Config::default(), false);

    assert_eq!(graph.unresolved_references.len(), 1);
    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
        source_only: false,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, &diag_config);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        !codes.contains(&DiagnosticCode::UriNoMapping),
        "uri/no-mapping must not fire without [[schemas]]"
    );
}

#[test]
fn no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken() {
    let tmp = TempDir::new().unwrap();
    let config = config_with_schema("onedrive://work/", tmp.path().to_str().unwrap(), None);
    let body = "[[s3://other/file]]\n";
    let mut workspace = workspace_with_doc(tmp.path(), "notes.md", body);
    workspace.config = config;
    let mut input = ResolveInput::from_workspace(&workspace);
    input.uri_opts = UriOptions {
        allow_sync: false,
        no_hints: true,
    };
    let graph = resolve_links(input);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
        source_only: false,
    };
    let opts = UriOptions {
        allow_sync: false,
        no_hints: true,
    };
    let diagnostics = run_diagnostics(&graph, &diag_config);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::LinkBroken),
        "link/broken must remain"
    );
    assert!(
        !codes.contains(&DiagnosticCode::UriNoMapping),
        "uri/no-mapping must be suppressed with --no-uri-hints"
    );
}

#[test]
fn trailing_slash_normalization_matches_both_forms() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // Prefix declared without a trailing slash.
    let config = config_with_schema("onedrive://work", assets.to_str().unwrap(), None);
    let body = "[[onedrive://work/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn env_var_expansion_in_root() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // SAFETY: tests run single-threaded.
    unsafe { std::env::set_var("DOWNLINT_SCHEMA_ROOT", assets.to_str().unwrap()) };
    let config = config_with_schema("scheme://", "$DOWNLINT_SCHEMA_ROOT", None);
    let body = "[[scheme://file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    unsafe { std::env::remove_var("DOWNLINT_SCHEMA_ROOT") };

    assert_eq!(graph.resolved_references.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations[0].path, asset);
}

#[test]
fn relative_root_resolves_against_config_dir() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // A relative root resolves against the config file's directory (the
    // workspace root, where `.downlint.toml` lives).
    let config = config_with_schema("scheme://", "assets", None);
    let body = "[[scheme://file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert_eq!(graph.resolved_references.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations[0].path, asset);
}

#[test]
fn more_specific_prefix_wins() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    let bucket_a = tmp.path().join("bucket-a");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&bucket_a).unwrap();
    let asset = bucket_a.join("file.md");
    fs::write(&asset, "hi").unwrap();

    let mut config = Config::default();
    config.schemas = finalize_schemas(vec![
        PartialSchema {
            uri: Some("onedrive://work/".to_string()),
            to: Some(work.to_str().unwrap().to_string()),
            auto_verify: None,
            verify_cmd: None,
        },
        PartialSchema {
            uri: Some("onedrive://work/bucket-a/".to_string()),
            to: Some(bucket_a.to_str().unwrap().to_string()),
            auto_verify: None,
            verify_cmd: None,
        },
    ])
    .unwrap();

    let body = "[[onedrive://work/bucket-a/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert_eq!(graph.resolved_references.len(), 1);
    assert_eq!(graph.resolved_references[0].destinations[0].path, asset);
}

#[cfg(unix)]
#[test]
fn verify_cmd_pass_marks_present() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // `true` always exits 0 → real file.
    let config = config_with_schema_and_verify("scheme://", assets.to_str().unwrap(), vec!["true".into()]);
    let body = "[[scheme://file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);
    assert_eq!(graph.resolved_references.len(), 1);
}

#[cfg(unix)]
#[test]
fn verify_cmd_failure_marks_broken_even_when_file_exists() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // `false` always exits non-zero → placeholder, even though the file exists.
    let config = config_with_schema_and_verify("scheme://", assets.to_str().unwrap(), vec!["false".into()]);
    let body = "[[scheme://file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);
    assert_eq!(graph.unresolved_references.len(), 1);
    assert_eq!(graph.unresolved_references[0].target, "scheme://file.md");
}

#[cfg(unix)]
#[test]
fn verify_cmd_gated_by_allow_sync() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "hi").unwrap();

    // `false` would mark a placeholder, but without --allow-uri-sync the
    // verify_cmd is skipped (treated as inconclusive) → the file resolves.
    let config = config_with_schema_and_verify("scheme://", assets.to_str().unwrap(), vec!["false".into()]);
    let body = "[[scheme://file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn icloud_evicted_placeholder_is_broken() {
    let tmp = TempDir::new().unwrap();
    // Synthesize an iCloud Mobile Documents path with a `.icloud` sibling.
    let mobiledocs = tmp.path().join("Mobile Documents").join("~cloud~");
    fs::create_dir_all(&mobiledocs).unwrap();
    let asset = mobiledocs.join("doc.md");
    fs::write(&asset, "ok").unwrap();
    fs::write(mobiledocs.join(".doc.md.icloud"), "").unwrap();

    let config = config_with_schema("icloud://", mobiledocs.to_str().unwrap(), None);
    let body = "[[icloud://doc.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    // auto_verify defaults to true; the `.icloud` sibling marks it a placeholder.
    assert_eq!(graph.unresolved_references.len(), 1);
}

#[test]
fn auto_verify_off_skips_heuristics() {
    let tmp = TempDir::new().unwrap();
    let mobiledocs = tmp.path().join("Mobile Documents").join("~cloud~");
    fs::create_dir_all(&mobiledocs).unwrap();
    let asset = mobiledocs.join("doc.md");
    fs::write(&asset, "ok").unwrap();
    fs::write(mobiledocs.join(".doc.md.icloud"), "").unwrap();

    // auto_verify = false → the `.icloud` sibling is ignored → the file resolves.
    let config = config_with_schema("icloud://", mobiledocs.to_str().unwrap(), Some(false));
    let body = "[[icloud://doc.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert_eq!(graph.resolved_references.len(), 1);
}
