pub mod manager;
pub mod rules;

use crate::parser::Ref;
use crate::resolution::ConnectionGraph;
use crate::utils::ByteRange;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
pub enum DiagnosticCode {
    /// Ambiguous link: a reference resolves to more than one destination.
    #[serde(rename = "link/ambiguous")]
    LinkAmbiguous,
    /// Broken link: a reference resolves to no destination (target missing).
    #[serde(rename = "link/broken")]
    LinkBroken,
    /// Non-breaking whitespace (U+00A0) immediately after a heading marker.
    #[serde(rename = "heading/nbsp")]
    HeadingNbsp,
    /// Broken anchor: an in-page anchor (e.g. `[label](#foo)`) or wiki anchor
    /// (`[[#foo]]`) referenced a heading that does not exist in the target
    /// document. Distinct from `link/broken` (broken file link) because the
    /// link target was unambiguously an anchor, not a file reference.
    #[serde(rename = "link/broken-anchor")]
    LinkBrokenAnchor,
    /// Info-level: a URI-scheme link (`scheme://...`) resolved via the
    /// configured `[[schemas]]` resolver but no matching prefix was
    /// configured. The diagnostic includes a hint pointing the user at
    /// `.downlint.toml`. Suppressed with `--no-uri-hints`.
    #[serde(rename = "uri/no-mapping")]
    UriNoMapping,
    /// Error: a mount's `prefix` or top-level folder collides with the primary
    /// project (RFC 0010). The conflicting namespace is suspended until the
    /// config is corrected.
    #[serde(rename = "mount/conflict")]
    MountConflict,
}

impl DiagnosticCode {
    /// The stable, user-facing rule id (e.g. `link/broken`). This is the
    /// string emitted in CLI/LSP output and cited in the spec.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::LinkAmbiguous => "link/ambiguous",
            Self::LinkBroken => "link/broken",
            Self::HeadingNbsp => "heading/nbsp",
            Self::LinkBrokenAnchor => "link/broken-anchor",
            Self::UriNoMapping => "uri/no-mapping",
            Self::MountConflict => "mount/conflict",
        }
    }
}

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Debug shows the slug (not the variant name) so `{:?}` in logs and test-failure
// messages reads the same as user-facing output.
impl std::fmt::Debug for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RelatedInformation {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub range: ByteRange,
    pub severity: DiagnosticSeverity,
    pub code: DiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedInformation>,
    /// The mount this diagnostic was emitted from (its `prefix`, or `root`
    /// when there is no prefix). `None` for primary docs (RFC 0010).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DiagnosticConfig {
    pub min_severity: DiagnosticSeverity,
    /// When true, per-document rules (e.g. `heading/nbsp`) run only on source
    /// documents. The CLI sets this in stdin mode, where workspace documents
    /// are indexed for link resolution but must not be diagnosed.
    pub source_only: bool,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self {
            min_severity: DiagnosticSeverity::Warning,
            source_only: false,
        }
    }
}

pub fn check_diagnostics(
    graph: &ConnectionGraph,
    config: &DiagnosticConfig,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for unresolved in &graph.unresolved_references {
        if let Some(diagnostic) = rules::broken_link(unresolved) {
            diagnostics.push(diagnostic);
        }
    }

    for ambiguous in &graph.ambiguous_references {
        if let Some(diagnostic) = rules::ambiguous_link(ambiguous) {
            diagnostics.push(diagnostic);
        }
    }

    if let Some(diagnostic) = rules::uri_no_mapping_hint(&graph.unresolved_references) {
        diagnostics.push(diagnostic);
    }

    for document in &graph.documents {
        if config.source_only && !document.is_source {
            continue;
        }
        diagnostics.extend(rules::non_breaking_space(
            &document.path,
            document.structure.text.as_str(),
        ));
    }

    // Namespace-level mount conflicts (RFC 0010): each is a config-level error
    // pointing at the offending mount.
    for conflict in &graph.conflicts {
        diagnostics.push(Diagnostic {
            path: PathBuf::from("."),
            range: ByteRange::new(0, 0),
            severity: DiagnosticSeverity::Error,
            code: DiagnosticCode::MountConflict,
            message: format!("Mount conflict: {}", conflict.detail),
            related: Vec::new(),
            mount: Some(conflict.mount_attribution.clone()),
        });
    }

    // Attribute each diagnostic to its mount (RFC 0010): a diagnostic whose
    // source path is a mounted doc is labeled with that mount's attribution.
    for diagnostic in &mut diagnostics {
        if diagnostic.mount.is_none() {
            diagnostic.mount = graph
                .document_for_path(&diagnostic.path)
                .and_then(|doc| doc.mount.clone());
        }
    }

    diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity >= config.min_severity)
        .collect()
}

pub fn severity_for_ref(reference: &Ref) -> DiagnosticSeverity {
    match reference {
        Ref::Wiki { .. } => DiagnosticSeverity::Error,
        Ref::Inline { .. } => DiagnosticSeverity::Warning,
        Ref::Full { .. } | Ref::Collapsed { .. } => DiagnosticSeverity::Warning,
        Ref::Shortcut { .. } => DiagnosticSeverity::Info,
    }
}
