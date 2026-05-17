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
    Some(Diagnostic {
        path: reference.source_path.clone(),
        range: reference.name_range.unwrap_or(reference.full_range),
        severity: severity_for_ref(&reference.reference),
        code: DiagnosticCode::DNL002,
        message: format!("Broken link: '{}' could not be resolved", reference.target),
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
