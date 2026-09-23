use std::path::{Path, PathBuf};
use url::Url;

pub fn abs_path(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub fn canonicalize_if_exists(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf())
}

/// Canonicalize a path for index membership checks (RFC 0022): resolve
/// symlinks in the deepest existing ancestor, then rejoin the remaining
/// components. A not-yet-existing file (an unsaved editor buffer) is
/// canonicalized via its parent so it compares equal to paths produced by a
/// walk of the canonicalized workspace root.
pub fn canonicalize_for_index(path: &Path) -> PathBuf {
    let mut target = path;
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(target) {
            Ok(mut canonical) => {
                for component in suffix.into_iter().rev() {
                    canonical.push(component);
                }
                return canonical;
            }
            Err(_) => {
                let name = match target.file_name() {
                    Some(name) => name.to_os_string(),
                    None => return path.to_path_buf(),
                };
                let parent = match target.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent,
                    _ => return path.to_path_buf(),
                };
                suffix.push(name);
                target = parent;
            }
        }
    }
}

/// Convert a path to the workspace root's path form (RFC 0022).
///
/// Clients are not consistent about symlink resolution in URIs (VS Code
/// canonicalizes, Neovim does not; macOS FSEvents reports canonical paths
/// while the client may use `/var/...`). Document paths are stored in the
/// root-as-given form, so every incoming path (editor URI, watcher event)
/// is converted to that form before use: if it already starts with `root`,
/// it is used as-is; otherwise its canonicalized form is matched against the
/// canonicalized root and the suffix is remapped onto `root`. Returns `None`
/// when the path is not under the root.
pub fn to_root_form(root: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(stripped) = path.strip_prefix(root) {
        return Some(root.join(stripped));
    }
    let canonical_root = canonicalize_for_index(root);
    let canonical_path = canonicalize_for_index(path);
    canonical_path
        .strip_prefix(&canonical_root)
        .ok()
        .map(|stripped| root.join(stripped))
}

pub fn uri_for_path(path: impl AsRef<Path>) -> Option<Url> {
    Url::from_file_path(path.as_ref()).ok()
}
