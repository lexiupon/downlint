use serde::Deserialize;
use std::fmt;

/// Resolved (non-Partial) mount configuration. One entry per `[[mounts]]` row.
///
/// A **mount** brings an external local markdown folder into the resolution
/// namespace. Its documents are indexed and treated co-equal with primary
/// documents (RFC 0010). The config layer owns the raw form; the workspace /
/// resolution layer expands `root` (env vars, `~`, relative paths).
#[derive(Clone, Debug)]
pub struct Mount {
    /// The folder to mount — the root where the mounted files are found.
    /// Required. Expanded by the workspace layer (`~`, `$VAR`, config-relative).
    pub root: String,
    /// An optional exact-path alias. A mounted doc at `root/path/to/file.md` is
    /// additionally reachable as `prefix/path/to/file.md`. The prefix is a
    /// virtual directory at the workspace root and must start with `/`.
    /// Serves as the unambiguous form when a name also exists in the primary.
    pub prefix: Option<String>,
    /// When true, links *within* the mounted documents are also linted (the
    /// mounted docs become resolution sources, not just targets). Default false.
    pub lint: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialMount {
    pub root: Option<String>,
    pub prefix: Option<String>,
    pub lint: Option<bool>,
}

#[derive(Debug)]
pub enum MountConfigError {
    Validation(String),
}

impl fmt::Display for MountConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for MountConfigError {}

/// Validate a single `[[mounts]]` entry. Returns the validated `Mount`, or a
/// `Validation` error naming the first broken rule.
pub fn finalize_mount(index: usize, partial: PartialMount) -> Result<Mount, MountConfigError> {
    let root = partial
        .root
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "root is required"))?;
    let prefix = partial.prefix.filter(|value| !value.is_empty());
    if let Some(prefix) = &prefix
        && !prefix.starts_with('/')
    {
        return Err(validation_at(
            index,
            "prefix must be a workspace-absolute virtual directory (start with `/`)",
        ));
    }
    let lint = partial.lint.unwrap_or(false);
    Ok(Mount { root, prefix, lint })
}

/// Validate the whole `[[mounts]]` section.
pub fn finalize_mounts(entries: Vec<PartialMount>) -> Result<Vec<Mount>, MountConfigError> {
    entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| finalize_mount(index, entry))
        .collect()
}

/// Project-precedence merge of two partial `[[mounts]]` sections. Project
/// (`high`) takes the entire list when present; otherwise `low` is used.
pub fn merge_mounts(
    high: Option<Vec<PartialMount>>,
    low: Option<Vec<PartialMount>>,
) -> Option<Vec<PartialMount>> {
    high.or(low)
}

fn validation_at(index: usize, message: &str) -> MountConfigError {
    MountConfigError::Validation(format!("mounts[{index}]: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(root: Option<&str>, prefix: Option<&str>, lint: Option<bool>) -> PartialMount {
        PartialMount {
            root: root.map(|value| value.to_string()),
            prefix: prefix.map(|value| value.to_string()),
            lint,
        }
    }

    #[test]
    fn minimal_mount_uses_defaults() {
        let mount = finalize_mount(0, partial(Some("~/kb"), None, None)).unwrap();
        assert_eq!(mount.root, "~/kb");
        assert!(mount.prefix.is_none());
        assert!(!mount.lint);
    }

    #[test]
    fn mount_with_prefix_and_lint() {
        let mount = finalize_mount(0, partial(Some("~/kb"), Some("/kb_alias"), Some(true))).unwrap();
        assert_eq!(mount.prefix.as_deref(), Some("/kb_alias"));
        assert!(mount.lint);
    }

    #[test]
    fn missing_root_errors() {
        let error = finalize_mount(2, partial(None, None, None)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("mounts[2]"), "{message}");
        assert!(message.contains("root is required"), "{message}");
    }

    #[test]
    fn empty_root_errors() {
        assert!(matches!(
            finalize_mount(0, partial(Some(""), None, None)),
            Err(MountConfigError::Validation(_))
        ));
    }

    #[test]
    fn prefix_must_start_with_slash() {
        let error = finalize_mount(0, partial(Some("~/kb"), Some("kb_alias"), None)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("prefix"), "{message}");
        assert!(message.contains("/"), "{message}");
    }

    #[test]
    fn finalize_mounts_validates_each_entry() {
        let entries = vec![
            partial(Some("~/a"), None, None),
            partial(None, None, None), // missing root
        ];
        let error = finalize_mounts(entries).unwrap_err();
        assert!(error.to_string().contains("mounts[1]"));
    }

    #[test]
    fn merge_mounts_high_wins() {
        let high = Some(vec![partial(Some("~/a"), None, None)]);
        let low = Some(vec![partial(Some("~/b"), None, None)]);
        let merged = merge_mounts(high, low).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].root.as_deref(), Some("~/a"));
    }

    #[test]
    fn merge_mounts_falls_back_to_low() {
        let merged = merge_mounts(None, Some(vec![partial(Some("~/b"), None, None)])).unwrap();
        assert_eq!(merged[0].root.as_deref(), Some("~/b"));
    }
}
