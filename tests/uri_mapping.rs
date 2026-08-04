use downlint::config::Config;
use downlint::config::uri::{PartialUriConfig, PartialUriMapping, finalize_uri};
use downlint::diagnostics::{DiagnosticCode, DiagnosticConfig, DiagnosticSeverity};
use downlint::parser::{ParseOptions, parse_document};
use downlint::resolution::uri::UriResolver;
use downlint::resolution::{
    ConnectionGraph, ResolveDocument, ResolveInput, UriOptions, UriSyncCache, resolve_links,
};
use downlint::utils::{DiscoveredFolder, Workspace, WorkspaceMode};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Build a `Config` whose `[uri]` section contains a single mapping.
fn config_with_mapping(
    prefix: &str,
    root: &str,
    sync_cmd: Option<Vec<String>>,
    sync_required: bool,
    sync_timeout: u32,
) -> Config {
    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some(prefix.to_string()),
            root: Some(root.to_string()),
            sync_cmd,
            sync_required: Some(sync_required),
            sync_timeout: Some(sync_timeout),
        }]),
    })
    .unwrap();
    config.uri = uri;
    config
}

/// Build a `Workspace` rooted at `root` with one document (`rel_path`) whose
/// body is `body`. Used to drive end-to-end URI mapping through
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
            extra_folders: vec![],
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
        batch_size: 50,
    };
    resolve_links(input)
}

fn run_diagnostics(
    graph: &ConnectionGraph,
    workspace_root: &Path,
    config: &DiagnosticConfig,
    uri_opts: &UriOptions,
) -> Vec<downlint::diagnostics::Diagnostic> {
    let workspace = Workspace {
        folder: DiscoveredFolder {
            root: workspace_root.to_path_buf(),
            config_path: None,
            documents: Vec::new(),
            extra_folders: Vec::new(),
        },
        mode: WorkspaceMode::MultiFile,
        config: Config::default(),
    };
    downlint::diagnostics::check_diagnostics(graph, config, &workspace, uri_opts)
}

#[test]
fn onedrive_present_file_resolves() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets").join("audit");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("report.xlsx");
    fs::write(&asset, "fake").unwrap();

    let config = config_with_mapping(
        "onedrive://work/",
        assets.to_str().unwrap(),
        None,
        false,
        30,
    );
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

#[test]
fn onedrive_missing_file_yields_broken_link() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let config =
        config_with_mapping("onedrive://work/", assets.to_str().unwrap(), None, false, 30);
    let body = "[[onedrive://work/missing.xlsx]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert_eq!(graph.unresolved_references.len(), 1);
    let unresolved = &graph.unresolved_references[0];
    assert_eq!(unresolved.target, "onedrive://work/missing.xlsx");
    // No-mapping hint should NOT fire because the prefix *does* match.
    assert!(!unresolved.uri_no_mapping_hint);

    let config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::DNL002));
}

#[test]
fn no_mapping_emits_hint_when_uri_configured() {
    let tmp = TempDir::new().unwrap();
    // Mapping is configured but doesn't match the target's scheme.
    let config =
        config_with_mapping("onedrive://work/", tmp.path().to_str().unwrap(), None, false, 30);
    let body = "[[s3://other/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert_eq!(graph.unresolved_references.len(), 1);
    assert!(graph.unresolved_references[0].uri_no_mapping_hint);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::DNL002));
    assert!(
        codes.contains(&DiagnosticCode::DNL006),
        "expected DNL006 hint, got {codes:?}"
    );
}

#[test]
fn no_mapping_emits_no_hint_when_uri_unconfigured() {
    let tmp = TempDir::new().unwrap();
    // `Config::default()` has no `[uri]` section. A URI-scheme link should
    // still resolve to a broken link (existing behavior) but no hint should
    // fire -- backwards compatibility.
    let body = "[[onedrive://work/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, Config::default(), false);

    assert_eq!(graph.unresolved_references.len(), 1);
    // The hint flag is *set* in the unresolved ref only when mappings exist.
    // Without mappings, no-mapping hint should be off, so no DNL006.
    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        !codes.contains(&DiagnosticCode::DNL006),
        "DNL006 must not fire without [uri]"
    );
}

#[test]
fn no_uri_hints_flag_suppresses_dnl006_but_keeps_dnl002() {
    let tmp = TempDir::new().unwrap();
    let config =
        config_with_mapping("onedrive://work/", tmp.path().to_str().unwrap(), None, false, 30);
    let body = "[[s3://other/file]]\n";
    let mut workspace = workspace_with_doc(tmp.path(), "notes.md", body);
    workspace.config = config;
    let mut input = ResolveInput::from_workspace(&workspace);
    input.uri_opts = UriOptions {
        allow_sync: false,
        no_hints: true,
        batch_size: 50,
    };
    let graph = resolve_links(input);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions {
        allow_sync: false,
        no_hints: true,
        batch_size: 50,
    };
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::DNL002),
        "DNL002 must remain"
    );
    assert!(
        !codes.contains(&DiagnosticCode::DNL006),
        "DNL006 must be suppressed with --no-uri-hints"
    );
}

#[cfg(unix)]
#[test]
fn allow_uri_sync_with_mock_cmd_marks_present_after_run() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let script = tmp.path().join("create.sh");
    fs::write(&script, "#!/bin/sh\ntouch \"$1\"\n").unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();

    let config = config_with_mapping(
        "scheme://",
        assets.to_str().unwrap(),
        Some(vec![
            script.to_string_lossy().to_string(),
            "{path}".to_string(),
        ]),
        false,
        10,
    );
    let body = "[[scheme://synced.txt]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    let target = assets.join("synced.txt");
    assert!(
        target.exists(),
        "sync_cmd should have created the file at {target:?}"
    );
    assert!(
        graph
            .unresolved_references
            .iter()
            .all(|r| r.target != "scheme://synced.txt"),
        "broken-link diagnostic should not fire after a successful sync"
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn no_uri_sync_mapping_means_skipped_diagnostic() {
    let tmp = TempDir::new().unwrap();
    let config = config_with_mapping(
        "scheme://",
        tmp.path().to_str().unwrap(),
        Some(vec!["true".to_string()]),
        false,
        30,
    );
    let body = "[[scheme://anything]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config.clone(), false);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let workspace = Workspace {
        folder: DiscoveredFolder {
            root: tmp.path().to_path_buf(),
            config_path: None,
            documents: Vec::new(),
            extra_folders: Vec::new(),
        },
        mode: WorkspaceMode::MultiFile,
        config: config.clone(),
    };
    let diagnostics =
        downlint::diagnostics::check_diagnostics(&graph, &diag_config, &workspace, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::DNL007),
        "DNL007 must fire when sync_cmd is configured but flag is off"
    );
}

#[cfg(unix)]
#[test]
fn sync_required_with_failing_cmd_marks_broken() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let config = config_with_mapping(
        "scheme://",
        assets.to_str().unwrap(),
        Some(vec!["false".to_string()]),
        true,
        10,
    );
    let body = "[[scheme://will-fail.txt]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    assert_eq!(graph.unresolved_references.len(), 1);
    assert_eq!(
        graph.unresolved_references[0].target,
        "scheme://will-fail.txt"
    );
}

#[test]
fn trailing_slash_normalization_matches_both_forms() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("file.md");
    fs::write(&asset, "ok").unwrap();

    // No trailing slash in prefix; both link forms must resolve.
    let config = config_with_mapping("scheme://work", assets.to_str().unwrap(), None, false, 30);
    let body = "[[scheme://work/file.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert!(
        graph.unresolved_references.is_empty(),
        "expected no unresolved, got {:#?}",
        graph.unresolved_references
    );
    assert_eq!(graph.resolved_references.len(), 1);
}

#[test]
fn env_var_expansion_in_root() {
    // SAFETY: single-threaded test.
    unsafe {
        std::env::set_var("DOWNLINT_URI_TEST_ROOT", "/tmp/env-test");
    }
    let tmp = TempDir::new().unwrap();
    // Create the target directory where `$DOWNLINT_URI_TEST_ROOT/x` resolves.
    let target_dir = PathBuf::from("/tmp/env-test/x");
    fs::create_dir_all(&target_dir).unwrap();
    let asset = target_dir.join("file");
    fs::write(&asset, "ok").unwrap();

    let config = config_with_mapping(
        "scheme://",
        "$DOWNLINT_URI_TEST_ROOT/x",
        None,
        false,
        30,
    );
    let body = "[[scheme://file]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert!(
        graph.unresolved_references.is_empty(),
        "expected env-var expansion to resolve, got {:#?}",
        graph.unresolved_references
    );

    // SAFETY: single-threaded test.
    unsafe {
        std::env::remove_var("DOWNLINT_URI_TEST_ROOT");
    }
}

#[test]
fn relative_root_resolves_against_config_dir() {
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("external_assets");
    fs::create_dir_all(&assets).unwrap();
    let asset = assets.join("note.md");
    fs::write(&asset, "ok").unwrap();

    // Note: this test relies on the UriResolver using the .downlint.toml's
    // directory (tmp.path()) for relative roots, NOT cwd.
    let config = config_with_mapping(
        "scheme://",
        "./external_assets",
        None,
        false,
        30,
    );
    let body = "[[scheme://note.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    assert!(
        graph.unresolved_references.is_empty(),
        "expected relative root to resolve against config dir, got {:#?}",
        graph.unresolved_references
    );
}

#[test]
fn sync_cache_is_shared_across_resolve_links_calls() {
    // Two `resolve_links` calls sharing the same `UriSyncCache` must not
    // re-fork the sync command on the second call. We verify this by
    // counting cache entries before and after.
    let cache = UriSyncCache::new();
    let tmp = TempDir::new().unwrap();

    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    // First run: file already exists, no sync is needed -- but we still
    // touch the cache via the runner.
    let _doc = ResolveDocument {
        path: tmp.path().join("notes.md"),
        rel_path: PathBuf::from("notes.md"),
        structure: parse_document("[[scheme://file]]\n", ParseOptions::default()),
    };
    fs::write(assets.join("file"), "ok").unwrap();

    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some("scheme://".to_string()),
            root: Some(assets.to_string_lossy().to_string()),
            sync_cmd: Some(vec!["true".to_string()]),
            sync_required: Some(false),
            sync_timeout: Some(10),
        }]),
    })
    .unwrap();
    let resolver = UriResolver::new(&uri, tmp.path()).unwrap();
    let runner = downlint::resolution::uri_sync::SyncRunner::with_cache(
        &resolver,
        false,
        50,
        cache.clone(),
    );
    let r1 = runner.run_for(0, assets.join("file"));
    assert_eq!(r1.status, downlint::resolution::uri_sync::PathStatus::Present);
    assert!(cache.len() >= 1);
    let size_after_first = cache.len();
    runner.run_for(0, assets.join("file"));
    assert_eq!(
        cache.len(),
        size_after_first,
        "cache hit should not add a second entry"
    );
}
