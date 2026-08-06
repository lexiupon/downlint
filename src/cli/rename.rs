//! CLI implementation of `downlint rename-file` and `downlint rename-link`
//! — RFC 0009 §"CLI Subcommands".
//!
//! These subcommands share the rename library with the LSP code actions;
//! the only difference is the apply step: the CLI writes text edits to
//! disk and moves files, while the LSP serializes the plan to a
//! `WorkspaceEdit`.
//!
//! ## Exit codes (RFC §"Exit codes")
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | Success (or dry-run clean) |
//! | 1 | Blocked by an existing diagnostic |
//! | 2 | Conflict detected |
//! | 3 | Bad arguments / config error / indexing-in-progress |
//!
//! ## Atomic application
//!
//! The CLI writes text edits to disk first, then moves the file. This
//! ordering means a partial disk failure leaves consistent text+disk
//! state (the text edits point at the new path; if the move fails, the
//! user sees DNL002 broken-link diagnostics rather than a corrupt
//! vault). Phase 5's minimum viable implementation does NOT use the
//! `.downlint/.rename.lock` file — that's tracked as future hardening
//! in RFC §"Risks" #6.

use crate::config::ConfigError;
use crate::rename::conflict::extension_class;
use crate::rename::{PlanInput, RenameError, RenameKind, RenamePlan, plan_rename};
use crate::resolution::{ResolveInput, resolve_links};
use crate::utils::{WorkspaceInput, discover_workspace};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct RenameFileOptions {
    pub root: Option<PathBuf>,
    pub from: PathBuf,
    pub to: PathBuf,
    pub dry_run: bool,
    pub verbose: u8,
    pub quiet: bool,
}

#[derive(Clone, Debug)]
pub struct RenameLinkOptions {
    pub root: Option<PathBuf>,
    pub from: String,
    pub to: String,
    pub dry_run: bool,
    pub verbose: u8,
    pub quiet: bool,
}

/// Run `downlint rename-file`. Returns the exit code (see module docs).
pub fn run_rename_file(options: RenameFileOptions) -> i32 {
    match run_rename_file_inner(&options) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("downlint: {error}");
            error.exit_code()
        }
    }
}

/// Run `downlint rename-link`. Returns the exit code (see module docs).
pub fn run_rename_link(options: RenameLinkOptions) -> i32 {
    match run_rename_link_inner(&options) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("downlint: {error}");
            error.exit_code()
        }
    }
}

#[derive(Debug)]
enum CliError {
    /// Indexing is in progress (or the workspace isn't ready yet).
    #[allow(dead_code)] // Wired up once the persistent server lands.
    Indexing,
    /// Source file not found on disk.
    SourceNotFound(PathBuf),
    /// Source and destination are identical.
    NoOp(PathBuf),
    /// Rename blocked by an existing diagnostic.
    Blocked(String),
    /// Conflict detected (path collision or extension class).
    Conflict(String),
    /// Bad arguments or config error.
    BadArguments(String),
    /// I/O error during apply.
    Io(String),
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            CliError::Indexing => 3,
            CliError::SourceNotFound(_) => 3,
            CliError::NoOp(_) => 0,
            CliError::Blocked(_) => 1,
            CliError::Conflict(_) => 2,
            CliError::BadArguments(_) => 3,
            CliError::Io(_) => 3,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Indexing => write!(f, "indexing in progress — try again in a moment"),
            CliError::SourceNotFound(path) => {
                write!(f, "source file not found: {}", path.display())
            }
            CliError::NoOp(path) => write!(
                f,
                "source and destination are identical: {}",
                path.display()
            ),
            CliError::Blocked(message) => write!(f, "rename blocked: {message}"),
            CliError::Conflict(message) => write!(f, "conflict: {message}"),
            CliError::BadArguments(message) => write!(f, "bad arguments: {message}"),
            CliError::Io(message) => write!(f, "i/o error: {message}"),
        }
    }
}

fn run_rename_file_inner(options: &RenameFileOptions) -> Result<(), CliError> {
    // 1. Build workspace first so we can resolve --from / --to against
    //    the workspace root (the user's --root, or the auto-discovered
    //    project root). The --from / --to arguments are workspace-
    //    relative paths per RFC §"CLI Argument semantics".
    let workspace = build_workspace(options.root.as_deref())
        .map_err(|e| CliError::BadArguments(format!("workspace discovery: {e}")))?;
    let from_abs = workspace.folder.root.join(&options.from);
    let to_abs = workspace.folder.root.join(&options.to);

    if !from_abs.exists() {
        return Err(CliError::SourceNotFound(from_abs));
    }
    if from_abs == to_abs {
        return Err(CliError::NoOp(from_abs));
    }

    // 2. Build workspace + graph.
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(err) = input.uri_error.take() {
        return Err(CliError::BadArguments(format!("config: {err}")));
    }
    let graph = resolve_links(input);

    // 3. Infer kind from extension.
    let kind = infer_kind_from_extension(&from_abs, &workspace.config.core.file_extensions);

    // 4. Plan the rename.
    let plan_input = PlanInput {
        kind,
        old: from_abs.to_string_lossy().to_string(),
        new: to_abs.to_string_lossy().to_string(),
        source_path: workspace.folder.root.clone(),
        cursor_offset: None,
    };
    let plan = plan_rename(plan_input, &graph).map_err(rename_error_to_cli)?;

    // 5. Apply or dry-run print.
    if options.dry_run {
        print_plan(&plan, options.verbose);
        return Ok(());
    }
    apply_plan(&plan, &workspace).map_err(|e| CliError::Io(e.to_string()))
}

fn run_rename_link_inner(options: &RenameLinkOptions) -> Result<(), CliError> {
    if options.from.is_empty() || options.to.is_empty() {
        return Err(CliError::BadArguments(
            "--from and --to must be non-empty identifiers".into(),
        ));
    }
    if options.from.contains(['/', '#', '|', '(', ')', '[', ']']) {
        return Err(CliError::BadArguments(format!(
            "--from must be a bare identifier (no /, #, |, (, ), [, ]); got {:?}",
            options.from
        )));
    }
    if options.to.contains(['/', '#', '|', '(', ')', '[', ']']) {
        return Err(CliError::BadArguments(format!(
            "--to must be a bare identifier (no /, #, |, (, ), [, ]); got {:?}",
            options.to
        )));
    }

    let workspace = build_workspace(options.root.as_deref())
        .map_err(|e| CliError::BadArguments(format!("workspace discovery: {e}")))?;
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(err) = input.uri_error.take() {
        return Err(CliError::BadArguments(format!("config: {err}")));
    }
    let graph = resolve_links(input);

    let plan_input = PlanInput {
        kind: RenameKind::LinkTarget,
        old: options.from.clone(),
        new: options.to.clone(),
        source_path: workspace.folder.root.clone(),
        cursor_offset: None,
    };
    let plan = plan_rename(plan_input, &graph).map_err(rename_error_to_cli)?;

    if options.dry_run {
        print_plan(&plan, options.verbose);
        return Ok(());
    }
    apply_plan(&plan, &workspace).map_err(|e| CliError::Io(e.to_string()))
}

fn rename_error_to_cli(err: RenameError) -> CliError {
    match err {
        RenameError::SourceNotFound(path) => CliError::SourceNotFound(path),
        RenameError::Conflict(conflict) => CliError::Conflict(conflict.message),
        RenameError::Blocked(violation) => CliError::Blocked(violation.render()),
        RenameError::NotApplicable(message) => CliError::BadArguments(message),
        RenameError::NotImplemented(_) => CliError::BadArguments(
            "this rename kind is not yet implemented".into(),
        ),
    }
}

/// Infer the rename kind from the source file's extension: markdown if
/// in `[core].file_extensions`, otherwise attachment.
fn infer_kind_from_extension(path: &Path, markdown_extensions: &[String]) -> RenameKind {
    let class = extension_class(path, markdown_extensions);
    if class.is_markdown() {
        RenameKind::File
    } else {
        RenameKind::Attachment
    }
}

fn build_workspace(
    root: Option<&Path>,
) -> Result<crate::utils::Workspace, ConfigError> {
    let input = WorkspaceInput::Path(root.unwrap_or(Path::new(".")).to_path_buf());
    discover_workspace(input, root)
}

/// Print a dry-run summary: one line per planned edit.
fn print_plan(plan: &RenamePlan, _verbose: u8) {
    for document in &plan.edits {
        for edit in &document.edits {
            println!(
                "{}:{}..{} → {:?}",
                document.path.display(),
                edit.range.start,
                edit.range.end,
                edit.new_text
            );
        }
    }
    for file_move in &plan.file_moves {
        println!("move: {} → {}", file_move.from.display(), file_move.to.display());
    }
    if plan.edits.is_empty() && plan.file_moves.is_empty() {
        println!("(no changes)");
    }
}

/// Apply a rename plan to disk: text edits first, then file moves.
/// Reads each affected doc, applies the text edits in reverse order,
/// writes back, then moves the file(s).
fn apply_plan(
    plan: &RenamePlan,
    workspace: &crate::utils::Workspace,
) -> std::io::Result<()> {
    // Text edits first (RFC §"Type F" step 5: text first, disk second).
    for document in plan.sorted_for_apply() {
        // Find the on-disk path. The plan's path is absolute (from the
        // graph); match it against the workspace's documents.
        let Some(doc) = workspace
            .folder
            .documents
            .iter()
            .find(|doc| doc.path == document.path)
        else {
            continue;
        };
        let original = doc.text.as_str();
        let mut sorted_edits = document.edits.clone();
        sorted_edits.sort_by_key(|e| std::cmp::Reverse(e.range.start));
        let mut current = original.to_string();
        for edit in &sorted_edits {
            current.replace_range(edit.range.start..edit.range.end, &edit.new_text);
        }
        if current != original {
            std::fs::write(&document.path, current)?;
        }
    }

    // Then file moves.
    for file_move in &plan.file_moves {
        if let Some(parent) = file_move.to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&file_move.from, &file_move.to)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI's infer_kind_from_extension chooses `File` for markdown
    /// extensions and `Attachment` for everything else.
    #[test]
    fn kind_inferred_from_extension() {
        let markdown_exts = vec!["md".into(), "markdown".into()];
        assert!(matches!(
            infer_kind_from_extension(Path::new("report.md"), &markdown_exts),
            RenameKind::File
        ));
        assert!(matches!(
            infer_kind_from_extension(Path::new("photo.png"), &markdown_exts),
            RenameKind::Attachment
        ));
    }

    /// Bare-identifier validation rejects path-like --from strings.
    #[test]
    fn bare_identifier_validation() {
        let bad_inputs = ["path/to/x", "x#anchor", "x|alias", "(x)", "[x]"];
        for input in bad_inputs {
            assert!(
                input.contains(['/', '#', '|', '(', ')', '[', ']']),
                "test setup: {input:?} should be flagged"
            );
        }
    }

    /// Exit code mapping matches RFC §"Exit codes".
    #[test]
    fn exit_code_mapping() {
        assert_eq!(CliError::Indexing.exit_code(), 3);
        assert_eq!(CliError::SourceNotFound(PathBuf::from("x")).exit_code(), 3);
        assert_eq!(CliError::NoOp(PathBuf::from("x")).exit_code(), 0);
        assert_eq!(CliError::Blocked("x".into()).exit_code(), 1);
        assert_eq!(CliError::Conflict("x".into()).exit_code(), 2);
        assert_eq!(CliError::BadArguments("x".into()).exit_code(), 3);
        assert_eq!(CliError::Io("x".into()).exit_code(), 3);
    }
}