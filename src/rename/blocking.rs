//! Blocking rule for rename operations — RFC 0009 §"Blocking Rule".
//!
//! A rename is blocked when any document that would be rewritten contains
//! a `Warning` or `Error` diagnostic other than on the occurrence(s) being
//! rewritten. The intent: don't let a propagating rename compound existing
//! problems (broken-link cascades, ambiguities, data-flow issues).
//!
//! This module is the **scaffolding** for Phase 1 — the diagnostic
//! classification table is here, the enforcement happens in later phases.

use crate::diagnostics::DiagnosticSeverity;

/// The severity level blocking a rename. RFC §"Blocking Rule":
///
/// | Severity | Blocks? |
/// |---|---|
/// | `Error` | Yes |
/// | `Warning` | Yes |
/// | `Information` | No |
/// | `Hint` | No |
pub fn is_blocking(severity: DiagnosticSeverity) -> bool {
    matches!(
        severity,
        DiagnosticSeverity::Error | DiagnosticSeverity::Warning
    )
}

/// One offending diagnostic — the user sees this inline in the error
/// message so they can fix the underlying issue first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockingDiagnostic {
    pub path: std::path::PathBuf,
    pub code: String,
    pub message: String,
}

/// All blocking diagnostics discovered across the documents that would be
/// rewritten. Surfaced as `error: rename blocked — N occurrence(s) have
/// diagnostic <code>` (RFC §"Blocking Rule" example).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockingViolation {
    pub diagnostics: Vec<BlockingDiagnostic>,
}

impl BlockingViolation {
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// Render the violation as the user-facing message RFC §"Blocking Rule"
    /// describes. Newline-separated list of every offender.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "rename blocked — {} occurrence(s) have diagnostic(s)\n",
            self.diagnostics.len()
        ));
        for entry in &self.diagnostics {
            out.push_str(&format!(
                "  → {}  {}\n",
                entry.path.display(),
                entry.message
            ));
        }
        out.push_str("hint: fix the broken reference(s) first, then re-run the rename.");
        out
    }
}