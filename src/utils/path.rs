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

pub fn uri_for_path(path: impl AsRef<Path>) -> Option<Url> {
    Url::from_file_path(path.as_ref()).ok()
}
