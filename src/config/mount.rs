use serde::Deserialize;
use std::fmt;

/// Resolved (non-Partial) mount configuration. One entry per `[[mounts]]` row.
///
/// A **mount** brings an external local markdown folder into the resolution
/// namespace. Its documents are indexed and treated co-equal with primary
/// documents (RFC 0010). The config layer owns the raw form; the workspace /
/// resolution layer expands `path` (env vars, `~`, relative paths).
#[derive(Clone, Debug)]
pub struct Mount {
    /// The folder to mount — where the mounted files are found.
    /// Required. Expanded by the workspace layer (`~`, `$VAR`, config-relative).
    pub path: String,
    /// An optional exact-path alias. A mounted doc at `path/<rel>` is additionally
    /// reachable as `as/<rel>`. `as` is a virtual directory at the workspace root
    /// and must start with `/`. Serves as the unambiguous form when a name also
    /// exists in the primary.
    pub r#as: Option<String>,
    /// When true, links *within* the mounted documents are also linted (the
    /// mounted docs become resolution sources, not just targets). Default false.
    pub lint: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialMount {
    pub path: Option<String>,
    pub r#as: Option<String>,
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
    let path = partial
        .path
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "path is required"))?;
    let as_path = partial.r#as.filter(|value| !value.is_empty());
    if let Some(as_path) = &as_path
        && !as_path.starts_with('/')
    {
        return Err(validation_at(
            index,
            "as must be a workspace-absolute virtual directory (start with `/`)",
        ));
    }
    let lint = partial.lint.unwrap_or(false);
    Ok(Mount {
        path,
        r#as: as_path,
        lint,
    })
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

    fn partial(path: Option<&str>, as_path: Option<&str>, lint: Option<bool>) -> PartialMount {
        PartialMount {
            path: path.map(|value| value.to_string()),
            r#as: as_path.map(|value| value.to_string()),
            lint,
        }
    }

    #[test]
    fn minimal_mount_uses_defaults() {
        let mount = finalize_mount(0, partial(Some("~/kb"), None, None)).unwrap();
        assert_eq!(mount.path, "~/kb");
        assert!(mount.r#as.is_none());
        assert!(!mount.lint);
    }

    #[test]
    fn mount_with_as_and_lint() {
        let mount =
            finalize_mount(0, partial(Some("~/kb"), Some("/kb_alias"), Some(true))).unwrap();
        assert_eq!(mount.r#as.as_deref(), Some("/kb_alias"));
        assert!(mount.lint);
    }

    #[test]
    fn missing_path_errors() {
        let error = finalize_mount(2, partial(None, None, None)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("mounts[2]"), "{message}");
        assert!(message.contains("path is required"), "{message}");
    }

    #[test]
    fn empty_path_errors() {
        assert!(matches!(
            finalize_mount(0, partial(Some(""), None, None)),
            Err(MountConfigError::Validation(_))
        ));
    }

    #[test]
    fn as_must_start_with_slash() {
        let error = finalize_mount(0, partial(Some("~/kb"), Some("kb_alias"), None)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("as must be"), "{message}");
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
        assert_eq!(merged[0].path.as_deref(), Some("~/a"));
    }

    #[test]
    fn merge_mounts_falls_back_to_low() {
        let merged = merge_mounts(None, Some(vec![partial(Some("~/b"), None, None)])).unwrap();
        assert_eq!(merged[0].path.as_deref(), Some("~/b"));
    }
}
