//! Conflict detection for file/attachment renames — RFC 0009 §"Conflict
//! detection" (Type A and Type F).

use std::path::PathBuf;

/// The kind of conflict detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    /// The new on-disk path already exists as a different file.
    ExactPath,
    /// With `obsidian_prefix = true`, the new stem is a leading prefix of
    /// an existing file's stem, or vice versa — either would create a
    /// `link/ambiguous` ambiguity for an existing prefix-resolved link.
    PrefixCollision,
    /// The source file's extension class (markdown ↔ markdown, attachment
    /// ↔ attachment) cannot be preserved — e.g. `report.md` → `report.pdf`.
    ExtensionClass,
}

/// A conflict detected during planning. Carries the diagnostic message
/// the LSP / CLI surface to the user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub kind: ConflictKind,
    pub message: String,
    /// The conflicting path (when applicable). `ExactPath` and
    /// `PrefixCollision` populate this; `ExtensionClass` leaves it `None`.
    pub path: Option<PathBuf>,
}

impl Conflict {
    pub fn exact_path(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ConflictKind::ExactPath,
            message: message.into(),
            path: Some(path),
        }
    }

    pub fn prefix_collision(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            kind: ConflictKind::PrefixCollision,
            message: message.into(),
            path: Some(path),
        }
    }

    pub fn extension_class(message: impl Into<String>) -> Self {
        Self {
            kind: ConflictKind::ExtensionClass,
            message: message.into(),
            path: None,
        }
    }
}

/// Determine the extension class of a file path: markdown (extension in
/// `[core].file_extensions`) vs attachment (anything else). The CLI and
/// the LSP code-action handler both use this to enforce the
/// extension-class preservation rule.
pub fn extension_class(path: &std::path::Path, markdown_extensions: &[String]) -> ExtensionClass {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    match ext {
        Some(value) if markdown_extensions.iter().any(|candidate| candidate == &value) => {
            ExtensionClass::Markdown
        }
        _ => ExtensionClass::Attachment,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionClass {
    Markdown,
    Attachment,
}

impl ExtensionClass {
    pub fn is_markdown(self) -> bool {
        matches!(self, ExtensionClass::Markdown)
    }
}