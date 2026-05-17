use std::path::{Path, PathBuf};

pub fn project_config_path(root: &Path) -> PathBuf {
    root.join(".downlint.toml")
}
