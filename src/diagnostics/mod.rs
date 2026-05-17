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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum DiagnosticCode {
    DNL001,
    DNL002,
    DNL003,
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
}

#[derive(Clone, Debug)]
pub struct DiagnosticConfig {
    pub min_severity: DiagnosticSeverity,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self {
            min_severity: DiagnosticSeverity::Warning,
        }
    }
}

pub fn check_diagnostics(graph: &ConnectionGraph, config: &DiagnosticConfig) -> Vec<Diagnostic> {
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

    for document in &graph.documents {
        diagnostics.extend(rules::non_breaking_space(
            &document.path,
            document.structure.text.as_str(),
        ));
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
