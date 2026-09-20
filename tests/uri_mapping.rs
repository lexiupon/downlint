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
    warm_cmd: Option<Vec<String>>,
    warm_required: bool,
    warm_timeout: u32,
) -> Config {
    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some(prefix.to_string()),
            root: Some(root.to_string()),
            warm_cmd,
            warm_required: Some(warm_required),
            warm_timeout: Some(warm_timeout),
            verify_cmd: None,
        }]),
        auto_verify: None,
    })
    .unwrap();
    config.uri = uri;
    config
}

/// Like `config_with_mapping` but with an optional `verify_cmd`.
fn config_with_mapping_and_verify(
    prefix: &str,
    root: &str,
    warm_cmd: Option<Vec<String>>,
    warm_required: bool,
    warm_timeout: u32,
    verify_cmd: Option<Vec<String>>,
) -> Config {
    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some(prefix.to_string()),
            root: Some(root.to_string()),
            warm_cmd,
            warm_required: Some(warm_required),
            warm_timeout: Some(warm_timeout),
            verify_cmd,
        }]),
        auto_verify: None,
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
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
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
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
    assert!(
        codes.contains(&DiagnosticCode::UriNoMapping),
        "expected uri/no-mapping hint, got {codes:?}"
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
    // Without mappings, no-mapping hint should be off, so no uri/no-mapping.
    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        !codes.contains(&DiagnosticCode::UriNoMapping),
        "uri/no-mapping must not fire without [uri]"
    );
}

#[test]
fn no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken() {
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
        codes.contains(&DiagnosticCode::LinkBroken),
        "link/broken must remain"
    );
    assert!(
        !codes.contains(&DiagnosticCode::UriNoMapping),
        "uri/no-mapping must be suppressed with --no-uri-hints"
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
        "warm_cmd should have created the file at {target:?}"
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
        codes.contains(&DiagnosticCode::UriSyncSkipped),
        "uri/sync-skipped must fire when warm_cmd is configured but flag is off"
    );
}

#[cfg(unix)]
#[test]
fn warm_required_with_failing_cmd_marks_broken() {
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
            warm_cmd: Some(vec!["true".to_string()]),
            warm_required: Some(false),
            warm_timeout: Some(10),
            verify_cmd: None,
        }]),
        auto_verify: None,
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

#[cfg(unix)]
#[test]
fn soft_sync_failure_emits_uri_sync_failed_alongside_link_broken() {
    // `warm_required = false` + sync ran + sync failed → link/broken broken AND
    // uri/sync-failed SyncFailureWarning. Both should be visible at min-severity=info.
    let tmp = TempDir::new().unwrap();
    let config = config_with_mapping(
        "scheme://",
        tmp.path().to_str().unwrap(),
        Some(vec!["false".to_string()]),
        false,
        10,
    );
    let body = "[[scheme://anywhere]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::LinkBroken),
        "link/broken must fire (broken link): {codes:?}"
    );
    assert!(
        codes.contains(&DiagnosticCode::UriSyncFailed),
        "uri/sync-failed must fire for soft sync failure: {codes:?}"
    );
}

#[cfg(unix)]
#[test]
fn hard_sync_failure_does_not_emit_uri_sync_failed() {
    // `warm_required = true` + sync failed → link/broken broken ONLY. uri/sync-failed is
    // reserved for soft failures.
    let tmp = TempDir::new().unwrap();
    let config = config_with_mapping(
        "scheme://",
        tmp.path().to_str().unwrap(),
        Some(vec!["false".to_string()]),
        true,
        10,
    );
    let body = "[[scheme://anywhere]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
    assert!(
        !codes.contains(&DiagnosticCode::UriSyncFailed),
        "uri/sync-failed must NOT fire for warm_required=true: {codes:?}"
    );
}

#[test]
fn missing_file_without_sync_does_not_emit_uri_sync_failed() {
    // File is missing, no sync configured → link/broken only (no soft failure
    // because sync did not run).
    let tmp = TempDir::new().unwrap();
    let config =
        config_with_mapping("scheme://", tmp.path().to_str().unwrap(), None, false, 30);
    let body = "[[scheme://anything]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Info,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
    assert!(
        !codes.contains(&DiagnosticCode::UriSyncFailed),
        "uri/sync-failed must NOT fire when sync did not run: {codes:?}"
    );
}

#[test]
fn uri_sync_failed_suppressed_at_warning_severity() {
    // uri/sync-failed is info-level; should not appear at min-severity=warning.
    let tmp = TempDir::new().unwrap();
    let config = config_with_mapping(
        "scheme://",
        tmp.path().to_str().unwrap(),
        Some(vec!["false".to_string()]),
        false,
        10,
    );
    let body = "[[scheme://anywhere]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    let diag_config = DiagnosticConfig {
        min_severity: DiagnosticSeverity::Warning,
    };
    let opts = UriOptions::default();
    let diagnostics = run_diagnostics(&graph, tmp.path(), &diag_config, &opts);
    let codes: Vec<DiagnosticCode> = diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&DiagnosticCode::LinkBroken));
    assert!(
        !codes.contains(&DiagnosticCode::UriSyncFailed),
        "uri/sync-failed must be suppressed at warning severity: {codes:?}"
    );
}

#[cfg(unix)]
#[test]
fn verify_cmd_pass_marks_present_after_sync() {
    // warm_cmd creates the file, then verify_cmd checks it's non-empty.
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let sync_script = tmp.path().join("create.sh");
    fs::write(&sync_script, "#!/bin/sh\necho content > \"$1\"\n").unwrap();
    let mut perms = fs::metadata(&sync_script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&sync_script, perms).unwrap();

    let config = config_with_mapping_and_verify(
        "scheme://",
        assets.to_str().unwrap(),
        Some(vec![
            sync_script.to_string_lossy().to_string(),
            "{path}".to_string(),
        ]),
        false,
        10,
        // verify_cmd that succeeds when the file has content > 0 bytes.
        Some(vec!["sh".to_string(), "-c".to_string(), format!("test -s \"{path}\"", path = "{path}")]),
    );
    let body = "[[scheme://verified.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    let target = assets.join("verified.md");
    assert!(target.exists());
    assert!(
        graph
            .unresolved_references
            .iter()
            .all(|r| r.target != "scheme://verified.md"),
        "verify_cmd success should mark link resolved"
    );
}

#[cfg(unix)]
#[test]
fn verify_cmd_failure_marks_broken_even_when_file_exists() {
    // warm_cmd creates an empty file, verify_cmd fails (file is empty),
    // so the link must be reported as broken.
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();

    let sync_script = tmp.path().join("touch.sh");
    fs::write(&sync_script, "#!/bin/sh\ntouch \"$1\"\n").unwrap();
    let mut perms = fs::metadata(&sync_script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&sync_script, perms).unwrap();

    let config = config_with_mapping_and_verify(
        "scheme://",
        assets.to_str().unwrap(),
        Some(vec![
            sync_script.to_string_lossy().to_string(),
            "{path}".to_string(),
        ]),
        false,
        10,
        Some(vec!["false".to_string()]), // always fails
    );
    let body = "[[scheme://placeholder.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, true);

    assert_eq!(graph.unresolved_references.len(), 1);
    assert_eq!(
        graph.unresolved_references[0].target,
        "scheme://placeholder.md"
    );
}

#[test]
fn auto_verify_off_skips_heuristics() {
    // When `[uri].auto_verify = "off"`, no placeholder detection runs.
    // The link resolves because warm_cmd was a no-op and the file exists.
    let tmp = TempDir::new().unwrap();
    let assets = tmp.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    let target = assets.join("real.md");
    fs::write(&target, "ok").unwrap();

    let mut config = config_with_mapping("scheme://", assets.to_str().unwrap(), None, false, 30);
    // Override the default ("on") with "off".
    let mut partial = PartialUriConfig::default();
    partial.mappings = Some(vec![PartialUriMapping {
        prefix: Some("scheme://".to_string()),
        root: Some(assets.to_string_lossy().to_string()),
        warm_cmd: None,
        warm_required: None,
        warm_timeout: None,
        verify_cmd: None,
    }]);
    partial.auto_verify = Some("off".to_string());
    config.uri = finalize_uri(partial).unwrap();

    let body = "[[scheme://real.md]]\n";
    let graph = build_graph(tmp.path(), "notes.md", body, config, false);
    assert!(graph.unresolved_references.is_empty());
}

#[cfg(unix)]
#[test]
fn batch_size_three_fans_out_into_one_spawn() {
    // SyncRunner::run_for_many with batch_size=3 and three short paths
    // should produce exactly one spawn (the positional fan-out joins all
    // three paths into one argv list).
    use downlint::resolution::uri_sync::run_for_many;

    let tmp = TempDir::new().unwrap();
    let counter = tmp.path().join("count.txt");
    let counter_quoted = format!("\"{}\"", counter.to_string_lossy());

    // Script appends one line per invocation. With batch fan-out, we get
    // exactly one line for batch_size=3.
    let script = tmp.path().join("log.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'called\\n' >> {counter_quoted}\nfor p in \"$@\"; do :; done\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();

    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some("scheme://".to_string()),
            root: Some(tmp.path().to_string_lossy().to_string()),
            warm_cmd: Some(vec![
                script.to_string_lossy().to_string(),
                "{path}".to_string(),
            ]),
            warm_required: Some(false),
            warm_timeout: Some(10),
            verify_cmd: None,
        }]),
        auto_verify: None,
    })
    .unwrap();
    config.uri = uri;

    let resolver = UriResolver::new(&config.uri, tmp.path()).unwrap();
    let runner = downlint::resolution::uri_sync::SyncRunner::with_cache(
        &resolver, true, /* batch_size */ 3, downlint::resolution::UriSyncCache::new(),
    );

    let paths = vec![
        tmp.path().join("a"),
        tmp.path().join("b"),
        tmp.path().join("c"),
    ];
    let outcome = run_for_many(&runner, 0, paths);
    assert_eq!(outcome.statuses.len(), 3);
    assert!(!outcome.fell_back_to_per_file);
    assert!(!outcome.empty);

    let body = fs::read_to_string(&counter).unwrap();
    // Batch fan-out → exactly one line (one spawn). Without fan-out we'd
    // see three.
    assert_eq!(body.lines().count(), 1, "expected 1 spawn, got: {body:?}");
}

#[test]
fn batch_size_one_preserves_per_file_behavior() {
    // batch_size=1 keeps Phase-1 behavior: one spawn per file. With three
    // paths and batch_size=1 we expect three invocations.
    use downlint::resolution::uri_sync::run_for_many;

    let tmp = TempDir::new().unwrap();
    let counter = tmp.path().join("count.txt");
    let counter_quoted = format!("\"{}\"", counter.to_string_lossy());

    let script = tmp.path().join("log.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf 'called\\n' >> {counter_quoted}\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&script).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some("scheme://".to_string()),
            root: Some(tmp.path().to_string_lossy().to_string()),
            warm_cmd: Some(vec![
                script.to_string_lossy().to_string(),
                "{path}".to_string(),
            ]),
            warm_required: Some(false),
            warm_timeout: Some(10),
            verify_cmd: None,
        }]),
        auto_verify: None,
    })
    .unwrap();
    config.uri = uri;

    let resolver = UriResolver::new(&config.uri, tmp.path()).unwrap();
    let runner = downlint::resolution::uri_sync::SyncRunner::with_cache(
        &resolver, true, /* batch_size */ 1, downlint::resolution::UriSyncCache::new(),
    );

    let paths = vec![
        tmp.path().join("a"),
        tmp.path().join("b"),
        tmp.path().join("c"),
    ];
    let outcome = run_for_many(&runner, 0, paths);
    assert_eq!(outcome.statuses.len(), 3);
    assert!(!outcome.fell_back_to_per_file);

    let body = fs::read_to_string(&counter).unwrap();
    // batch_size=1 → three spawns (one per path).
    assert_eq!(body.lines().count(), 3, "expected 3 spawns, got: {body:?}");
}

#[cfg(unix)]
#[test]
fn batch_clamps_to_per_file_when_argv_exceeds_limit() {
    // Make the substituted {path} argument size exceed MAX_BATCH_BYTES by
    // using a warm_cmd with multiple {path} slots and a long dummy arg. We
    // don't need the path to actually exist on disk — the clamp triggers
    // purely on the constructed argv byte length, before any spawn.
    use downlint::resolution::uri_sync::{MAX_BATCH_BYTES, run_for_many};

    let tmp = TempDir::new().unwrap();
    // Fake long path string that the runner will substitute into {path}.
    // We don't actually create the file; the clamp is based on byte length.
    let long_path = std::path::PathBuf::from(format!(
        "{}/{}",
        tmp.path().to_string_lossy(),
        "x".repeat(MAX_BATCH_BYTES)
    ));

    // warm_cmd has multiple {path} slots to push the constructed argv
    // length over MAX_BATCH_BYTES when there are 2+ paths.
    let mut config = Config::default();
    let uri = finalize_uri(PartialUriConfig {
        mappings: Some(vec![PartialUriMapping {
            prefix: Some("scheme://".to_string()),
            root: Some(tmp.path().to_string_lossy().to_string()),
            warm_cmd: Some(vec![
                "true".to_string(),
                "{path}".to_string(),
                "{path}".to_string(),
            ]),
            warm_required: Some(false),
            warm_timeout: Some(10),
            verify_cmd: None,
        }]),
        auto_verify: None,
    })
    .unwrap();
    config.uri = uri;

    let resolver = UriResolver::new(&config.uri, tmp.path()).unwrap();
    let runner = downlint::resolution::uri_sync::SyncRunner::with_cache(
        &resolver, true, 2, downlint::resolution::UriSyncCache::new(),
    );
    let paths = vec![long_path.clone(), long_path];
    let outcome = run_for_many(&runner, 0, paths);
    assert!(
        outcome.fell_back_to_per_file,
        "argv over MAX_BATCH_BYTES should trigger fallback"
    );
    assert_eq!(outcome.statuses.len(), 2);
}
