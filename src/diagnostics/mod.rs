pub mod manager;
pub mod rules;

use crate::parser::Ref;
use crate::resolution::ConnectionGraph;
use crate::utils::ByteRange;
use crate::utils::Workspace;
use std::path::Path;
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
    /// Broken anchor: an in-page anchor (e.g. `[label](#foo)`) or wiki anchor
    /// (`[[#foo]]`) referenced a heading that does not exist in the target
    /// document. Distinct from `DNL002` (broken file link) because the link
    /// target was unambiguously an anchor, not a file reference.
    DNL005,
    /// Info-level: a URI-scheme link (`scheme://...`) resolved via the
    /// configured `[uri.mappings]` resolver but no matching prefix was
    /// configured. The diagnostic includes a hint pointing the user at
    /// `.downlint.toml`. Suppressed with `--no-uri-hints`.
    DNL006,
    /// Info-level: a `[uri.mappings]` entry has a `sync_cmd` configured but
    /// the run did not pass `--allow-uri-sync`. The diagnostic is emitted at
    /// most once per mapping per run; it is purely informational and is also
    /// suppressed when the user has set `--min-severity` to exclude Info.
    DNL007,
    /// Info-level: a URI mapping's `sync_cmd` ran but failed (or timed out)
    /// and the file is still missing. Only emitted for mappings with
    /// `sync_required = false` (mappings with `sync_required = true` already
    /// produce a hard DNL002 broken link). Lets users distinguish a
    /// "soft" sync failure from a real missing file.
    DNL008,
    /// Info-level: a URI mapping's `sync_cmd` would have produced an arg list
    /// exceeding the per-batch byte cap (default 128 KiB). The runner fell
    /// back to per-file spawning for that mapping. Emitted at most once per
    /// mapping per run, similar to DNL007.
    DNL009,
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

pub fn check_diagnostics(
    graph: &ConnectionGraph,
    config: &DiagnosticConfig,
    workspace: &Workspace,
    uri_opts: &crate::resolution::UriOptions,
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

    if let Some(diagnostic) = rules::sync_failure_warning(&graph.unresolved_references) {
        diagnostics.push(diagnostic);
    }

    // One-time info diagnostic when sync is configured but the gating flag
    // is off. We look at the active config, not the live runner, because
    // gating is decided at the CLI layer.
    if !uri_opts.allow_sync {
        let sync_count = workspace
            .config
            .uri
            .mappings
            .iter()
            .filter(|mapping| mapping.sync_cmd.is_some())
            .count();
        if sync_count > 0
            && let Some(diagnostic) = rules::uri_sync_skipped(
                Path::new("."),
                sync_count,
                &workspace
                    .config
                    .uri
                    .mappings
                    .iter()
                    .filter(|mapping| mapping.sync_cmd.is_some())
                    .map(|mapping| mapping.prefix.as_str())
                    .collect::<Vec<_>>(),
            )
        {
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
