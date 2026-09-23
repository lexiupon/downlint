//! Shared rename library — RFC 0009.
//!
//! This crate is the single source of truth for rename planning. The LSP
//! code-action handlers, the LSP `workspace/didRenameFiles` handler, the
//! persistent server, and the CLI `rename-file` / `rename-link` subcommands
//! all dispatch through `plan_rename` here.
//!
//! The library is structured by rename element type (the four LSP-internal
//! kinds from RFC §"Element Types"):
//!
//! - [`file`] — LSP Type F: a markdown file on disk.
//! - [`attachment`] — LSP Type A: an attachment (image, PDF, …).
//! - [`link`] — LSP Type L: a textual link-target identifier, no disk move.
//! - [`heading`] — LSP Type H: a heading text occurrence in source.
//!
//! Cross-cutting concerns:
//!
//! - [`conflict`] — exact-path / prefix-collision / extension-class checks
//!   shared by Type F and Type A.
//! - [`blocking`] — the "blocking rule" that refuses a rename when any
//!   rewritten document has a `Warning` / `Error` diagnostic outside the
//!   occurrences being rewritten.
//! - [`apply`] — apply a [`RenamePlan`] to disk (CLI) or serialize it as a
//!   `WorkspaceEdit` (LSP).
//!
//! Today (Phase 1) only the scaffolding is in place: types are defined, the
//! dispatcher routes to per-type modules, and per-type modules return
//! `RenameError::NotImplemented` until their respective phases land.

pub mod apply;
pub mod attachment;
pub mod blocking;
pub mod conflict;
pub mod file;
pub mod heading;
pub mod link;

use crate::resolution::ConnectionGraph;
use crate::utils::ByteRange;
use std::path::PathBuf;

/// A single byte-precise text edit. Applied by [`apply::apply_rename`] (CLI)
/// or grouped into an LSP `WorkspaceEdit` (code actions).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub range: ByteRange,
    pub new_text: String,
}

/// A move of one file on disk from `from` to `to`. The text edits for every
/// referencing document are emitted separately in [`RenamePlan::edits`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// The kind of element the rename targets. Mirrors RFC §"Element Types".
///
/// `Attachment` and `File` are **collapsed** at the CLI surface into a single
/// `downlint rename-file` subcommand; the dispatcher in [`plan_rename`]
/// infers which internal kind applies from the source file's extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameKind {
    /// LSP Type F — markdown file on disk.
    File,
    /// LSP Type A — attachment on disk.
    Attachment,
    /// LSP Type L — textual link-target identifier (no disk move).
    LinkTarget,
    /// LSP Type H — heading text occurrence (no disk move).
    Heading,
}

/// Input to [`plan_rename`].
///
/// `kind` + `old` + `new` describe what to rename. `source_path` is the
/// workspace-relative path of the file containing the cursor (used by
/// heading and link-target kinds to compute byte ranges and resolver
/// context). For file / attachment renames it is informational only —
/// the source path is in `old`.
#[derive(Clone, Debug)]
pub struct PlanInput {
    pub kind: RenameKind,
    pub old: String,
    pub new: String,
    pub source_path: PathBuf,
    pub cursor_offset: Option<usize>,
}

/// Value object capturing every edit and every disk move produced by a
/// rename plan. The LSP serializes this as a `WorkspaceEdit` (used by code
/// actions); the CLI serializes it as disk writes via [`apply::apply_rename`].
///
/// Both surfaces call `plan_rename` first, then either serialize (LSP) or
/// apply (CLI). This means a single set of tests covers both surfaces for
/// the file/heading/link-rewrite operations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenamePlan {
    /// Per-document edits. Edits within a single document must be sorted
    /// reverse-order (end-of-doc first) before applying so concurrent edits
    /// don't invalidate each other's offsets — see [`crate::lsp::edit`].
    pub edits: Vec<DocumentEdit>,
    /// Disk moves to apply AFTER the text edits (text first, disk second —
    /// RFC §"Type F" step 5).
    pub file_moves: Vec<FileMove>,
}

/// A document's contribution to a [`RenamePlan`]. Edits within `edits` are
/// sorted in reverse order at apply time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentEdit {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

impl DocumentEdit {
    pub fn new(path: PathBuf, edits: Vec<TextEdit>) -> Self {
        Self { path, edits }
    }
}

impl RenamePlan {
    pub fn new(edits: Vec<DocumentEdit>, file_moves: Vec<FileMove>) -> Self {
        Self { edits, file_moves }
    }

    /// Sort edits within each document in reverse order (end-of-doc first).
    /// This is the canonical ordering for applying non-overlapping edits
    /// without offset invalidation — see [`crate::lsp::edit::build_workspace_edit`].
    pub fn sorted_for_apply(&self) -> Vec<DocumentEdit> {
        let mut sorted: Vec<DocumentEdit> = self.edits.clone();
        for document in &mut sorted {
            document
                .edits
                .sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
        }
        sorted
    }
}

/// Errors produced by [`plan_rename`]. Each variant maps to a specific
/// RFC §"Exit codes" / `MethodFailed` outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenameError {
    /// A `Warning` / `Error` diagnostic on an unrelated occurrence blocks
    /// the rename. The carrying [`BlockingViolation`] lists every offender.
    /// → LSP `MethodFailed`, CLI exit 1.
    Blocked(BlockingViolation),
    /// An exact-path or prefix-collision or extension-class mismatch.
    /// → LSP `MethodFailed`, CLI exit 2.
    Conflict(Conflict),
    /// The source file (`old`) does not exist on disk. → CLI exit 3.
    SourceNotFound(PathBuf),
    /// The cursor is not positioned on a renameable element (or the
    /// requested `kind` doesn't match what's under the cursor).
    /// → LSP `MethodFailed` (for code actions), CLI exit 3.
    NotApplicable(String),
    /// The library is still being built — every other type's planner is
    /// implemented in a later phase of RFC 0009.
    NotImplemented(&'static str),
}

pub use blocking::BlockingViolation;
pub use conflict::{Conflict, ConflictKind};

/// Plan a rename operation. The dispatcher infers per-type behavior from
/// [`PlanInput::kind`] and dispatches to the per-type module.
///
/// Phase 1 only wires the scaffolding. Real planners land in Phases 2–6:
/// - Phase 2: `RenameKind::Heading`
/// - Phase 3: `RenameKind::File` and `RenameKind::Attachment`
/// - Phase 6: `RenameKind::LinkTarget`
pub fn plan_rename(input: PlanInput, graph: &ConnectionGraph) -> Result<RenamePlan, RenameError> {
    match input.kind {
        RenameKind::File => file::plan(input, graph),
        RenameKind::Attachment => attachment::plan(input, graph),
        RenameKind::LinkTarget => link::plan(input, graph),
        RenameKind::Heading => heading::plan(input, graph),
    }
}