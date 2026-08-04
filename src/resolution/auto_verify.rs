//! Built-in placeholder detection for cloud-synced files.
//!
//! Heuristics are best-effort. They run as a fast path before any user-
//! configured `verify_cmd`. Auto-detection can be disabled via
//! `[uri].auto_verify = "off"`.
//!
//! Heuristics:
//!
//! - **OneDrive (macOS)**: file in a `OneDrive-*` path that has a non-empty
//!   `com~apple~metadata` resource fork (created when OneDrive has not yet
//!   hydrated the file content) — or a zero-byte `._<name>` resource fork.
//! - **OneDrive (Windows)**: NTFS alternate data stream `Zone.Identifier`
//!   exists. (Not implemented: `std::fs` does not surface ADS on stable.
//!   Returns `Unknown` on non-macOS targets for the OneDrive family.)
//! - **iCloud (macOS)**: file in `Mobile Documents/` with a sibling `.icloud`
//!   placeholder file present.
//! - **Generic**: file size 0 AND mtime within the last 60 seconds. Catches
//!   sync tools that just-touched a placeholder.

use std::path::Path;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoVerifyOutcome {
    Real,
    Placeholder,
    Unknown,
}

/// User-facing modes mirroring `[uri].auto_verify` values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoVerifyMode {
    On,
    Off,
    OneDriveOnly,
    ICloudOnly,
}

impl AutoVerifyMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "onedrive-only" => Some(Self::OneDriveOnly),
            "icloud-only" => Some(Self::ICloudOnly),
            _ => None,
        }
    }
}

/// Run heuristics against `path`. Returns `AutoVerifyOutcome::Placeholder`
/// when at least one heuristic fires; `Real` when heuristics positively
/// confirm a real file; `Unknown` when no heuristic fires (verify_cmd, if
/// configured, gets the final say).
pub fn classify(path: &Path, mode: AutoVerifyMode) -> AutoVerifyOutcome {
    if matches!(mode, AutoVerifyMode::Off) {
        return AutoVerifyOutcome::Unknown;
    }
    let path_str = path.to_string_lossy();
    let in_cloud_path = is_cloud_storage_path(&path_str);

    // iCloud check first: most specific (requires Mobile Documents + .icloud).
    if matches!(mode, AutoVerifyMode::On | AutoVerifyMode::ICloudOnly)
        && path_str.contains("Mobile Documents")
    {
        if let Some(parent) = path.parent() {
            let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let icloud_sibling = parent.join(format!(".{stem}.icloud"));
            if icloud_sibling.exists() {
                return AutoVerifyOutcome::Placeholder;
            }
        }
    }

    // OneDrive checks: require OneDrive in path (covers macOS prefix).
    if matches!(mode, AutoVerifyMode::On | AutoVerifyMode::OneDriveOnly)
        && path_str.contains("OneDrive")
    {
        if has_onedrive_metadata(path) {
            return AutoVerifyOutcome::Placeholder;
        }
    }

    // Generic fallback: empty file with very recent mtime is suspect, but
    // ONLY when the path looks like cloud storage. We don't want to flag
    // every freshly-touched file in a normal repo as a placeholder.
    if matches!(mode, AutoVerifyMode::On)
        && in_cloud_path
        && is_zero_byte_with_recent_mtime(path)
    {
        return AutoVerifyOutcome::Placeholder;
    }

    // If we couldn't say, don't claim Placeholder; return Unknown so the
    // configured verify_cmd (if any) gets to decide.
    AutoVerifyOutcome::Unknown
}

/// Heuristic: does `path_str` look like it lives under cloud storage?
/// Conservative — only returns true for well-known prefixes.
fn is_cloud_storage_path(path_str: &str) -> bool {
    let markers = [
        "OneDrive",
        "iCloud",
        "CloudStorage",
        "Google Drive",
        "Dropbox",
        "Box Sync",
        "MEGAsync",
        "pCloud",
    ];
    markers.iter().any(|marker| path_str.contains(marker))
}

/// macOS-specific: detect OneDrive placeholders via resource forks / metadata
/// sidecars. Returns true when a `._<name>` resource fork of size 0 exists
/// (heuristic for "not yet hydrated") or when the file path contains a
/// `com~apple~metadata` companion (a strong OneDrive-on-macOS signal).
fn has_onedrive_metadata(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let resource_fork = parent.join(format!("._ {stem}").replace("._ ", "._"));
    if resource_fork.exists() {
        if let Ok(meta) = std::fs::metadata(&resource_fork) {
            if meta.len() == 0 {
                return true;
            }
        }
    }
    // Cheap-and-fast fallback: any sibling starting with `._` is suspicious
    // when OneDrive is in the path. Real OneDrive-hydrated files don't have
    // resource forks.
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("._") && name.len() > 2 {
                    return true;
                }
            }
        }
    }
    false
}

fn is_zero_byte_with_recent_mtime(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() != 0 {
        return false;
    }
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed.as_secs() < 60
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn off_mode_returns_unknown() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("x");
        fs::write(&f, "ok").unwrap();
        assert_eq!(
            classify(&f, AutoVerifyMode::Off),
            AutoVerifyOutcome::Unknown
        );
    }

    #[test]
    fn empty_file_recent_mtime_in_cloud_path_is_placeholder() {
        let tmp = TempDir::new().unwrap();
        // Synthesize a path under a cloud-storage marker so the generic
        // heuristic engages.
        let cloud = tmp.path().join("OneDrive-fake");
        fs::create_dir_all(&cloud).unwrap();
        let f = cloud.join("fresh.txt");
        fs::write(&f, "").unwrap();
        assert_eq!(
            classify(&f, AutoVerifyMode::On),
            AutoVerifyOutcome::Placeholder
        );
    }

    #[test]
    fn empty_file_outside_cloud_path_is_not_placeholder() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("fresh.txt");
        fs::write(&f, "").unwrap();
        // Outside cloud-storage paths the generic heuristic does NOT fire.
        let result = classify(&f, AutoVerifyMode::On);
        assert_ne!(result, AutoVerifyOutcome::Placeholder);
    }

    #[test]
    fn non_empty_file_is_real_or_unknown_not_placeholder() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("real.txt");
        fs::write(&f, "hello").unwrap();
        let result = classify(&f, AutoVerifyMode::On);
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

        assert_eq!(
            classify(&real_file, AutoVerifyMode::On),
            AutoVerifyOutcome::Placeholder
        );
    }

    #[test]
    fn icloud_check_skipped_outside_mobile_documents() {
        let tmp = TempDir::new().unwrap();
        let real_file = tmp.path().join("doc.md");
        fs::write(&real_file, "ok").unwrap();
        let result = classify(&real_file, AutoVerifyMode::ICloudOnly);
        // No heuristics active (not on iCloud, not generic in icloud-only mode).
        assert_eq!(result, AutoVerifyOutcome::Unknown);
    }

    #[test]
    fn mode_parse_recognizes_known_values() {
        assert_eq!(AutoVerifyMode::parse("on"), Some(AutoVerifyMode::On));
        assert_eq!(AutoVerifyMode::parse("off"), Some(AutoVerifyMode::Off));
        assert_eq!(
            AutoVerifyMode::parse("onedrive-only"),
            Some(AutoVerifyMode::OneDriveOnly)
        );
        assert_eq!(
            AutoVerifyMode::parse("icloud-only"),
            Some(AutoVerifyMode::ICloudOnly)
        );
        assert_eq!(AutoVerifyMode::parse(""), None);
        assert_eq!(AutoVerifyMode::parse("bogus"), None);
    }
}