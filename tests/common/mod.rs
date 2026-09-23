//! Shared helpers for the CLI/LSP subprocess test suites.
//!
//! Each integration test file is its own crate; including this module
//! (`mod common;`) gives them one shared fixture vocabulary helper
//! (RFC 0020).

use std::fs;
use tempfile::TempDir;

/// Create a temporary vault with the given `(rel_path, content)` pairs.
/// Parent directories are created as needed.
pub fn write_vault(pairs: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    for (rel, content) in pairs {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }
    temp
}
