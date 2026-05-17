use std::path::{Path, PathBuf};

pub fn has_scheme(input: &str) -> bool {
    input
        .split(':')
        .next()
        .filter(|prefix| !prefix.is_empty() && prefix.chars().all(is_scheme_char))
        .is_some_and(|prefix| input.len() > prefix.len() + 1)
}

pub fn split_anchor(input: &str) -> (&str, Option<&str>) {
    if let Some((path, anchor)) = input.split_once('#') {
        (path, Some(anchor))
    } else {
        (input, None)
    }
}

pub fn path_without_extension(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = value.strip_suffix(&format!(
        ".{}",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
    )) {
        value = stripped.to_string();
    }
    value
}

pub fn resolve_explicit_path(root: &Path, source_dir: &Path, target: &str) -> PathBuf {
    if target.starts_with('/') {
        root.join(target.trim_start_matches('/'))
    } else {
        source_dir.join(target)
    }
}

fn is_scheme_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')
}
