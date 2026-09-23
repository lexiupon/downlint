//! Built-in placeholder detection for cloud-synced files.
//!
//! Heuristics are best-effort and **vendor-specific** (RFC 0010, Phase 2).
//! They run as a fast path before any user-configured `verify_cmd`. Auto-
//! detection is enabled per-schema via `[[schemas]] auto_verify` (default
//! `true`). The old generic zero-byte + recent-mtime heuristic was dropped:
//! it had a false-positive window for genuinely empty cloud files.
//!
//! Heuristics:
//!
//! - **OneDrive (macOS)**: a zero-byte `._<name>` resource fork sibling
//!   (created when OneDrive has not yet hydrated the file content).
//! - **iCloud (macOS)**: file in `Mobile Documents/` with a sibling
//!   `.<name>.icloud` placeholder file present.

use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoVerifyOutcome {
    Real,
    Placeholder,
    Unknown,
}

/// Run the vendor-specific placeholder heuristics against `path`. Returns
/// `AutoVerifyOutcome::Placeholder` when a heuristic fires; `Unknown` when no
/// heuristic fires (a configured `verify_cmd`, if any, gets the final say).
/// When `enabled` is false, returns `Unknown` immediately.
pub fn classify(path: &Path, enabled: bool) -> AutoVerifyOutcome {
    if !enabled {
        return AutoVerifyOutcome::Unknown;
    }
    let path_str = path.to_string_lossy();

    // iCloud check: most specific (requires Mobile Documents + .icloud).
    if path_str.contains("Mobile Documents") {
        if let Some(parent) = path.parent() {
            let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let icloud_sibling = parent.join(format!(".{stem}.icloud"));
            if icloud_sibling.exists() {
                return AutoVerifyOutcome::Placeholder;
            }
        }
    }

    // OneDrive check: require OneDrive in path (covers the macOS prefix).
    if path_str.contains("OneDrive") && has_onedrive_resource_fork(path) {
        return AutoVerifyOutcome::Placeholder;
    }

    // If we couldn't say, don't claim Placeholder; return Unknown so the
    // configured verify_cmd (if any) gets to decide.
    AutoVerifyOutcome::Unknown
}

/// macOS-specific: detect a OneDrive placeholder via a zero-byte `._<name>`
/// resource fork sibling (created when OneDrive has not yet hydrated the file
/// content). Only the *specific* file's resource fork is inspected — a sibling
/// `._other` from a different file must not implicate this one.
fn has_onedrive_resource_fork(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let resource_fork = parent.join(format!("._{stem}"));
    match std::fs::metadata(&resource_fork) {
        Ok(meta) => meta.len() == 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn disabled_returns_unknown() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("x");
        fs::write(&f, "ok").unwrap();
        assert_eq!(classify(&f, false), AutoVerifyOutcome::Unknown);
    }

    #[test]
    fn non_empty_file_is_not_placeholder() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("real.txt");
        fs::write(&f, "hello").unwrap();
        let result = classify(&f, true);
        assert_ne!(result, AutoVerifyOutcome::Placeholder);
    }

    #[test]
    fn icloud_sibling_marks_placeholder() {
        let tmp = TempDir::new().unwrap();
        let mobiledocs = tmp.path().join("Mobile Documents").join("~cloud~");
        fs::create_dir_all(&mobiledocs).unwrap();
        let real_file = mobiledocs.join("doc.md");
        fs::write(&real_file, "ok").unwrap();
        let placeholder = mobiledocs.join(".doc.md.icloud");
        fs::write(&placeholder, "").unwrap();

        assert_eq!(classify(&real_file, true), AutoVerifyOutcome::Placeholder);
    }

    #[test]
    fn icloud_check_skipped_outside_mobile_documents() {
        let tmp = TempDir::new().unwrap();
        let real_file = tmp.path().join("doc.md");
        fs::write(&real_file, "ok").unwrap();
        // No heuristics active (not on iCloud).
        assert_eq!(classify(&real_file, true), AutoVerifyOutcome::Unknown);
    }

    #[test]
    fn onedrive_resource_fork_marks_placeholder() {
        let tmp = TempDir::new().unwrap();
        let onedrive = tmp.path().join("OneDrive-Work");
        fs::create_dir_all(&onedrive).unwrap();
        let real_file = onedrive.join("report.pdf");
        fs::write(&real_file, "ok").unwrap();
        // A zero-byte resource fork sibling signals "not yet hydrated".
        let fork = onedrive.join("._report.pdf");
        fs::write(&fork, "").unwrap();

        assert_eq!(classify(&real_file, true), AutoVerifyOutcome::Placeholder);
    }

    #[test]
    fn onedrive_resource_fork_of_other_file_does_not_implicate() {
        let tmp = TempDir::new().unwrap();
        let onedrive = tmp.path().join("OneDrive-Work");
        fs::create_dir_all(&onedrive).unwrap();
        // `a.pdf` is a placeholder (has a zero-byte resource fork sibling).
        let a = onedrive.join("a.pdf");
        fs::write(&a, "").unwrap();
        fs::write(onedrive.join("._a.pdf"), "").unwrap();
        // `b.pdf` is a real, hydrated file with no resource fork of its own.
        let b = onedrive.join("b.pdf");
        fs::write(&b, "real content").unwrap();

        assert_eq!(classify(&a, true), AutoVerifyOutcome::Placeholder);
        // Regression: b.pdf must NOT be implicated by a.pdf's resource fork.
        assert_ne!(classify(&b, true), AutoVerifyOutcome::Placeholder);
    }
}
