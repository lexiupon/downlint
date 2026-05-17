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

fn percent_decode(input: &str) -> String {
    let mut result = Vec::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push(ch);
            result.extend(hex.chars());
        } else {
            result.push(ch);
        }
    }
    result.into_iter().collect()
}

pub fn resolve_explicit_path(root: &Path, source_dir: &Path, target: &str) -> PathBuf {
    let decoded = percent_decode(target);
    if decoded.starts_with('/') {
        root.join(decoded.trim_start_matches('/'))
    } else {
        source_dir.join(decoded)
    }
}

fn is_scheme_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')
}
