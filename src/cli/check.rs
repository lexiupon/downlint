use crate::config::ConfigError;
use crate::diagnostics::{
    Diagnostic, DiagnosticCode, DiagnosticConfig, DiagnosticSeverity, check_diagnostics,
};
use crate::resolution::{ResolveInput, resolve_links};
use crate::utils::{Workspace, WorkspaceInput, discover_workspace};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Debug)]
pub struct CheckOptions {
    pub root: Option<PathBuf>,
    pub format: OutputFormat,
    pub min_severity: DiagnosticSeverity,
    pub color: String,
    pub verbose: u8,
    pub fix: bool,
    pub watch: bool,
    pub stdin: bool,
    pub quiet: bool,
    pub path: Option<PathBuf>,
    pub allow_uri_sync: bool,
    pub no_uri_hints: bool,
}

pub async fn run_check(options: CheckOptions) -> i32 {
    match run_check_once(&options) {
        Ok(mut result) => {
            if options.fix {
                if let Err(error) = apply_fixes(&mut result.workspace, &result.diagnostics) {
                    eprintln!("downlint: error: {error}");
                    return 2;
                }
                match run_check_once(&options) {
                    Ok(next) => result = next,
                    Err(error) => {
                        eprintln!("downlint: error: {error}");
                        return 2;
                    }
                }
            }

            print_workspace_hints(&options, &result.workspace);

            if !options.quiet {
                emit_diagnostics(&result.diagnostics, &options.format, &result.workspace);
            }

            if options.watch {
                return run_watch(options, result.workspace.folder.root.clone());
            }

            if result.diagnostics.is_empty() { 0 } else { 1 }
        }
        Err(error) => {
            eprintln!("downlint: error: {error}");
            2
        }
    }
}

/// Friendly, non-error hints for directory-based checks: surface that no
/// `.downlint.toml` was found (and suggest `downlint init`) and/or that the
/// workspace contains no markdown. Suppressed in `--quiet`/`--stdin` mode and
/// for a single explicit file (where "no files" is not meaningful).
fn print_workspace_hints(options: &CheckOptions, workspace: &Workspace) {
    if options.quiet || options.stdin {
        return;
    }
    let dir_based = options.path.as_deref().map(|p| p.is_dir()).unwrap_or(true);
    if !dir_based {
        return;
    }
    let root = &workspace.folder.root;
    if workspace.folder.config_path.is_none() {
        eprintln!(
            "downlint: no .downlint.toml found in {}; using default config.",
            root.display()
        );
        eprintln!("downlint: run `downlint init` to create one.");
    }
    if workspace.folder.documents.is_empty() {
        eprintln!(
            "downlint: no markdown files found in {}.",
            root.display()
        );
    }
}

struct CheckResult {
    workspace: Workspace,
    diagnostics: Vec<Diagnostic>,
}

fn run_check_once(options: &CheckOptions) -> Result<CheckResult, ConfigError> {
    let workspace = build_workspace(options)?;
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(error) = input.uri_error.take() {
        return Err(ConfigError::Validation(error));
    }
    input.uri_opts.allow_sync = options.allow_uri_sync;
    input.uri_opts.no_hints = options.no_uri_hints;
    let graph = resolve_links(input);
    let diagnostics = check_diagnostics(
        &graph,
        &DiagnosticConfig {
            min_severity: options.min_severity,
        },
    );
    Ok(CheckResult {
        workspace,
        diagnostics,
    })
}

fn build_workspace(options: &CheckOptions) -> Result<Workspace, ConfigError> {
    let input = if options.stdin || options.path.as_deref() == Some(Path::new("-")) {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(ConfigError::Io)?;
        WorkspaceInput::Stdin {
            text: buffer,
            display_name: "<stdin>.md".into(),
        }
    } else {
        WorkspaceInput::Path(options.path.clone().unwrap_or_else(|| PathBuf::from(".")))
    };

    discover_workspace(input, options.root.as_deref())
}

fn emit_diagnostics(diagnostics: &[Diagnostic], format: &OutputFormat, workspace: &Workspace) {
    match format {
        OutputFormat::Text => {
            for diagnostic in diagnostics {
                let rel = diagnostic
                    .path
                    .strip_prefix(&workspace.folder.root)
                    .unwrap_or(diagnostic.path.as_path());
                println!(
                    "{}:{}: {}: {} [{}]",
                    rel.display(),
                    line_and_column(workspace, diagnostic),
                    severity_name(diagnostic.severity),
                    diagnostic.message,
                    diagnostic.code.as_str()
                );
            }
        }
        OutputFormat::Json => {
            let body = diagnostics
                .iter()
                .map(|diagnostic| {
                    json!({
                        "path": diagnostic.path,
                        "range": diagnostic.range,
                        "severity": diagnostic.severity,
                        "code": diagnostic.code.as_str(),
                        "message": diagnostic.message,
                        "related": diagnostic.related,
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_else(|_| "[]".into())
            );
        }
    }
}

fn line_and_column(workspace: &Workspace, diagnostic: &Diagnostic) -> String {
    workspace
        .folder
        .documents
        .iter()
        .find(|doc| doc.path == diagnostic.path)
        .and_then(|doc| {
            doc.text
                .to_lsp_position(diagnostic.range.start, crate::utils::PositionEncoding::Utf8)
                .ok()
        })
        .map(|position| format!("{}:{}", position.line + 1, position.character + 1))
        .unwrap_or_else(|| "1:1".into())
}

fn severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Info => "info",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Error => "error",
    }
}

fn apply_fixes(workspace: &mut Workspace, diagnostics: &[Diagnostic]) -> Result<(), ConfigError> {
    let mut grouped: HashMap<PathBuf, Vec<&Diagnostic>> = HashMap::new();
    for diagnostic in diagnostics {
        if diagnostic.code == DiagnosticCode::HeadingNbsp {
            grouped
                .entry(diagnostic.path.clone())
                .or_default()
                .push(diagnostic);
        }
    }

    for (path, fixes) in grouped {
        let mut text = fs::read_to_string(&path).map_err(ConfigError::Io)?;
        let mut ranges = fixes
            .iter()
            .map(|diagnostic| diagnostic.range)
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
        for range in ranges {
            text.replace_range(range.start..range.end, " ");
        }
        fs::write(&path, text).map_err(ConfigError::Io)?;
    }

    *workspace = discover_workspace(
        WorkspaceInput::Path(workspace.folder.root.clone()),
        Some(workspace.folder.root.as_path()),
    )?;
    Ok(())
}

fn run_watch(options: CheckOptions, root: PathBuf) -> i32 {
    let (tx, rx) = mpsc::channel();
    let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("downlint: error: {error}");
            return 2;
        }
    };
    if let Err(error) = watcher.watch(&root, RecursiveMode::Recursive) {
        eprintln!("downlint: error: {error}");
        return 2;
    }

    let debounce = Duration::from_millis(200);
    loop {
        let Ok(_) = rx.recv() else {
            return 0;
        };
        let deadline = Instant::now() + debounce;
        while Instant::now() < deadline {
            if rx.recv_timeout(Duration::from_millis(25)).is_err() {
                break;
            }
        }
        match run_check_once(&CheckOptions {
            watch: false,
            ..options.clone()
        }) {
            Ok(result) => {
                if !options.quiet {
                    emit_diagnostics(&result.diagnostics, &options.format, &result.workspace);
                }
            }
            Err(error) => eprintln!("downlint: error: {error}"),
        }
    }
}
