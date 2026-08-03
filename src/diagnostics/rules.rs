use crate::diagnostics::{
    Diagnostic, DiagnosticCode, DiagnosticSeverity, RelatedInformation, severity_for_ref,
};
use crate::parser::Ref;
use crate::resolution::conn::{AmbiguousReference, UnresolvedReference};
use crate::utils::ByteRange;
use std::path::Path;

pub fn broken_link(reference: &UnresolvedReference) -> Option<Diagnostic> {
    if matches!(reference.reference, Ref::Shortcut { .. }) {
        return None;
    }
    // In-page anchor miss → distinct diagnostic so the user knows the link was
    // an anchor, not a missing file reference.
    if reference.is_anchor {
        // For cross-document anchors, `target` is "path#anchor"; for in-page
        // anchors it's just "anchor". Show whichever is more useful.
        let display = if reference.target.contains('#') {
            // Cross-document: keep the `path#anchor` form so the user can
            // locate the offending file at a glance.
            reference.target.clone()
        } else if reference.target.starts_with('#') {
            reference.target.clone()
        } else {
            format!("#{}", reference.target)
        };
        return Some(Diagnostic {
            path: reference.source_path.clone(),
            range: reference.name_range.unwrap_or(reference.full_range),
            severity: DiagnosticSeverity::Warning,
            code: DiagnosticCode::DNL005,
            message: format!("Broken anchor: '{display}' could not be resolved"),
            related: Vec::new(),
        });
    }
    let mut message = format!("Broken link: '{}' could not be resolved", reference.target);
    if matches!(reference.reference, Ref::Wiki { .. }) {
        if let Some(payload) = reference.hint_payload.as_ref() {
            if !payload.is_empty() {
                let total = payload.len();
                let cap = 5usize;
                let shown = total.min(cap);
                let names: Vec<String> = payload
                    .iter()
                    .take(shown)
                    .map(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name.to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    })
                    .collect();
                let more = if total > cap {
                    format!(" (+{} more)", total - cap)
                } else {
                    String::new()
                };
                message.push_str(&format!(
                    "\nHint: enable 'wiki.obsidian_prefix' to match partial filenames (candidates: {}{})",
                    names.join(", "),
                    more,
                ));
            }
        }
    }
    Some(Diagnostic {
        path: reference.source_path.clone(),
        range: reference.name_range.unwrap_or(reference.full_range),
        severity: severity_for_ref(&reference.reference),
        code: DiagnosticCode::DNL002,
        message,
        related: Vec::new(),
    })
}

pub fn ambiguous_link(reference: &AmbiguousReference) -> Option<Diagnostic> {
    Some(Diagnostic {
        path: reference.source_path.clone(),
        range: reference.name_range.unwrap_or(reference.full_range),
        severity: severity_for_ref(&reference.reference),
        code: DiagnosticCode::DNL001,
        message: format!(
            "Ambiguous link: '{}' resolves to multiple destinations",
            reference.target
        ),
        related: reference
            .destinations
            .iter()
            .map(|destination| RelatedInformation {
                path: destination.path.clone(),
                message: destination.path.display().to_string(),
            })
            .collect(),
    })
}

pub fn non_breaking_space(path: &Path, input: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut offset = 0usize;
    for line in input.split_inclusive(['\n', '\r']) {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
        if hashes > 0 && trimmed.chars().nth(hashes) == Some('\u{00a0}') {
            let start = offset + hashes;
            diagnostics.push(Diagnostic {
                path: path.to_path_buf(),
                range: ByteRange::new(start, start + '\u{00a0}'.len_utf8()),
                severity: DiagnosticSeverity::Warning,
                code: DiagnosticCode::DNL003,
                message: "Non-breaking whitespace after heading marker".into(),
                related: Vec::new(),
            });
        }
        offset += line.len();
    }
    diagnostics
}
