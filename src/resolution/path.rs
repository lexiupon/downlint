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
