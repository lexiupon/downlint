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


/// Returns true if the target path (before any anchor) ends with /,
/// indicating the link intends to reference a directory.
pub fn is_folder_link_target(target: &str) -> bool {
    let (path_part, _anchor) = split_anchor(target);
    path_part.ends_with('/')
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

pub fn percent_decode(input: &str) -> String {
    // Decode percent-encoded UTF-8 bytes back into a Unicode String.
    // Per RFC 3986 §2.5, percent-encoded sequences in URIs represent
    // octets; for non-ASCII payloads (CJK, accented Latin, emoji) those
    // octets are the bytes of a UTF-8 encoding of the original text.
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    bytes.push(byte);
                    continue;
                }
            }
            // Malformed escape: keep the literal characters.
            bytes.push(ch as u32 as u8);
            for hex_ch in hex.chars() {
                bytes.push(hex_ch as u32 as u8);
            }
        } else {
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            bytes.extend_from_slice(encoded.as_bytes());
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn resolve_explicit_path(
    root: &Path,
    source_dir: &Path,
    target: &str,
    is_wiki: bool,
) -> PathBuf {
    let decoded = percent_decode(target);
    let base = resolution_base(&decoded, source_dir, root, is_wiki);
    // Strip a leading `/` so an absolute target joins onto the root rather than
    // replacing it (`Path::join` with an absolute path discards the base).
    let rel = decoded.trim_start_matches('/');
    normalize_path(&base.join(rel))
}

/// The directory a path target resolves against (RFC 0013). Strict,
/// prefix-driven, no fallback:
/// - `./…` / `../…` → the containing document's directory
/// - `/…` → the workspace root
/// - bare wiki `…/…` (contains `/`, no prefix) → the workspace root
/// - otherwise (markdown bare path / basename) → the containing document's dir
pub fn resolution_base<'a>(
    decoded: &str,
    source_dir: &'a Path,
    root: &'a Path,
    is_wiki: bool,
) -> &'a Path {
    if decoded.starts_with("./") || decoded.starts_with("../") {
        source_dir
    } else if decoded.starts_with('/') {
        root
    } else if is_wiki && decoded.contains('/') {
        root
    } else {
        source_dir
    }
}

/// True when a path target resolves against the workspace root (RFC 0013):
/// a leading `/`, or a bare wiki path (contains `/`, no `./`/`../` prefix).
/// Root-relative targets are the ones that may reach a mount via its `prefix`
/// (RFC 0010).
pub fn is_root_relative(decoded: &str, is_wiki: bool) -> bool {
    decoded.starts_with('/')
        || (is_wiki
            && decoded.contains('/')
            && !decoded.starts_with("./")
            && !decoded.starts_with("../"))
}

/// Lexically normalize a path: drop `.` components and resolve `..` against the
/// preceding component (filesystem-free). A `..` with nothing to cancel is kept,
/// so the result may point above the base — callers simply find no match.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// True if two `/`-separated path strings refer to the same file, treating a
/// trailing `.md` as optional when `is_wiki` (Obsidian rule: `.md` is optional
/// for markdown, required otherwise). Case-insensitive.
pub fn path_md_optional_eq(a: &str, b: &str, is_wiki: bool) -> bool {
    let a = a.replace('\\', "/").to_ascii_lowercase();
    let b = b.replace('\\', "/").to_ascii_lowercase();
    if a == b {
        return true;
    }
    if !is_wiki {
        return false;
    }
    let a_no_ext = Path::new(&a).extension().is_none();
    let b_no_ext = Path::new(&b).extension().is_none();
    (format!("{a}.md") == b && a_no_ext) || (format!("{b}.md") == a && b_no_ext)
}

fn is_scheme_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')
}

/// Returns the scheme portion of a `scheme://...` or `scheme:...` target.
/// Returns `None` if `input` has no scheme or only an empty prefix.
pub fn scheme_of(input: &str) -> Option<&str> {
    let prefix = input.split(':').next()?;
    if prefix.is_empty() || !prefix.chars().all(is_scheme_char) {
        return None;
    }
    if input.len() <= prefix.len() + 1 {
        return None;
    }
    Some(prefix)
}

/// True for schemes that downlint never treats as "broken": the target
/// points outside the workspace by design (a public web URL, an email
/// address, etc.) and the user is responsible for its validity. These
/// references resolve to themselves, regardless of whether a
/// `[uri.mappings]` entry matches.
pub fn is_external_web_scheme(scheme: &str) -> bool {
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "ftps" | "mailto" | "tel" | "sms" | "irc" | "xmpp"
    )
}
