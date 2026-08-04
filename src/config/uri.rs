use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

/// Resolved (non-Partial) URI mapping configuration. Holds the parsed entries from
/// `[[uri.mappings]]` plus the resolved config directory used to expand relative
/// `root` values. Config errors are surfaced as `UriConfigError::Validation`.
#[derive(Clone, Debug, Default)]
pub struct UriConfig {
    pub mappings: Vec<UriMapping>,
}

/// One row of `[[uri.mappings]]`. The config layer owns the raw form; the
/// resolution layer consumes the resolver-side view that knows how to expand
/// `root` (env vars, `~`, relative paths).
#[derive(Clone, Debug)]
pub struct UriMapping {
    pub prefix: String,
    pub root: String,
    pub sync_cmd: Option<Vec<String>>,
    pub sync_required: bool,
    pub sync_timeout: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialUriConfig {
    pub mappings: Option<Vec<PartialUriMapping>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialUriMapping {
    pub prefix: Option<String>,
    pub root: Option<String>,
    pub sync_cmd: Option<Vec<String>>,
    pub sync_required: Option<bool>,
    pub sync_timeout: Option<u32>,
}

#[derive(Debug)]
pub enum UriConfigError {
    Validation(String),
}

impl fmt::Display for UriConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for UriConfigError {}

/// Validate a single mapping entry. Returns the validated `UriMapping`, or a
/// `Validation` error describing the first broken rule.
pub fn finalize_mapping(
    index: usize,
    partial: PartialUriMapping,
) -> Result<UriMapping, UriConfigError> {
    let prefix = partial
        .prefix
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "uri.mappings[*].prefix is required"))?;
    let root = partial
        .root
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "uri.mappings[*].root is required"))?;
    let sync_timeout = partial.sync_timeout.unwrap_or(30);
    if sync_timeout == 0 {
        return Err(validation_at(
            index,
            "uri.mappings[*].sync_timeout must be > 0",
        ));
    }

    Ok(UriMapping {
        prefix,
        root,
        sync_cmd: partial.sync_cmd,
        sync_required: partial.sync_required.unwrap_or(false),
        sync_timeout,
    })
}

/// Validate an entire `[uri]` section.
pub fn finalize_uri(partial: PartialUriConfig) -> Result<UriConfig, UriConfigError> {
    let mut mappings = Vec::new();
    if let Some(entries) = partial.mappings {
        for (index, entry) in entries.into_iter().enumerate() {
            mappings.push(finalize_mapping(index, entry)?);
        }
    }
    Ok(UriConfig { mappings })
}

/// Project-precedence merge of two partial `[uri]` sections. Project (`high`)
/// takes the entire `mappings` list when present; otherwise `low` is used.
pub fn merge_uri(high: PartialUriConfig, low: PartialUriConfig) -> PartialUriConfig {
    PartialUriConfig {
        mappings: high.mappings.or(low.mappings),
    }
}

/// Helper: turn a `PathBuf` into a UTF-8-prefixed validation message naming the
/// offending mapping index. The intent is to make config errors easy to find.
fn validation_at(index: usize, message: &str) -> UriConfigError {
    UriConfigError::Validation(format!("uri.mappings[{index}]: {message}"))
}

/// Convenience: produce a default `PartialUriConfig::default()`. Mirrors the
/// style of the rest of the config module.
#[allow(dead_code)]
pub(crate) fn empty_config_dir() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping_with(mappings: Vec<PartialUriMapping>) -> PartialUriConfig {
        PartialUriConfig {
            mappings: Some(mappings),
        }
    }

    fn partial(
        prefix: Option<&str>,
        root: Option<&str>,
        sync_cmd: Option<Vec<String>>,
        sync_required: Option<bool>,
        sync_timeout: Option<u32>,
    ) -> PartialUriMapping {
        PartialUriMapping {
            prefix: prefix.map(|value| value.to_string()),
            root: root.map(|value| value.to_string()),
            sync_cmd,
            sync_required,
            sync_timeout,
        }
    }

    #[test]
    fn minimal_mapping_uses_defaults() {
        let cfg = finalize_mapping(
            0,
            partial(
                Some("onedrive://work/"),
                Some("~/Library/CloudStorage/OneDrive-Work/assets"),
                None,
                None,
                None,
            ),
        )
        .unwrap();
        assert_eq!(cfg.prefix, "onedrive://work/");
        assert!(!cfg.sync_required);
        assert_eq!(cfg.sync_timeout, 30);
        assert!(cfg.sync_cmd.is_none());
    }

    #[test]
    fn sync_required_and_timeout_override_defaults() {
        let cfg = finalize_mapping(
            0,
            partial(
                Some("s3://reports/"),
                Some("./external/s3"),
                Some(vec!["aws".into(), "s3".into(), "cp".into(), "{path}".into()]),
                Some(true),
                Some(60),
            ),
        )
        .unwrap();
        assert!(cfg.sync_required);
        assert_eq!(cfg.sync_timeout, 60);
        assert_eq!(
            cfg.sync_cmd,
            Some(vec!["aws".into(), "s3".into(), "cp".into(), "{path}".into()])
        );
    }

    #[test]
    fn missing_prefix_errors_with_index() {
        let error = finalize_mapping(
            2,
            partial(None, Some("./foo"), None, None, None),
        )
        .unwrap_err();
        match error {
            UriConfigError::Validation(message) => {
                assert!(message.contains("uri.mappings[2]"));
                assert!(message.contains("prefix"));
            }
        }
    }

    #[test]
    fn missing_root_errors() {
        let error = finalize_mapping(
            0,
            partial(Some("scheme://"), None, None, None, None),
        )
        .unwrap_err();
        assert!(matches!(error, UriConfigError::Validation(_)));
    }

    #[test]
    fn empty_prefix_errors() {
        let error = finalize_mapping(
            0,
            partial(Some(""), Some("./foo"), None, None, None),
        )
        .unwrap_err();
        assert!(matches!(error, UriConfigError::Validation(_)));
    }

    #[test]
    fn zero_timeout_errors() {
        let error = finalize_mapping(
            0,
            partial(Some("scheme://"), Some("./foo"), None, None, Some(0)),
        )
        .unwrap_err();
        match error {
            UriConfigError::Validation(message) => {
                assert!(message.contains("sync_timeout"));
            }
        }
    }

    #[test]
    fn finalize_uri_collects_errors_index() {
        let cfg = finalize_uri(mapping_with(vec![
            partial(Some("scheme://"), Some("./foo"), None, None, None),
            partial(None, Some("./bar"), None, None, None),
        ]))
        .unwrap_err();
        assert!(matches!(cfg, UriConfigError::Validation(_)));
    }

    #[test]
    fn finalize_uri_empty_mappings_succeeds() {
        let cfg = finalize_uri(mapping_with(vec![])).unwrap();
        assert!(cfg.mappings.is_empty());
    }

    #[test]
    fn finalize_uri_missing_section_yields_empty_config() {
        let cfg = finalize_uri(PartialUriConfig::default()).unwrap();
        assert!(cfg.mappings.is_empty());
    }

    #[test]
    fn merge_uri_project_wins_when_present() {
        let project = mapping_with(vec![partial(
            Some("project://"),
            Some("./p"),
            None,
            None,
            None,
        )]);
        let user = mapping_with(vec![partial(
            Some("user://"),
            Some("./u"),
            None,
            None,
            None,
        )]);
        let merged = merge_uri(project, user);
        let mappings = merged.mappings.unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].prefix.as_deref(), Some("project://"));
    }

    #[test]
    fn merge_uri_falls_back_to_user_when_project_absent() {
        let project = PartialUriConfig::default();
        let user = mapping_with(vec![partial(
            Some("user://"),
            Some("./u"),
            None,
            None,
            None,
        )]);
        let merged = merge_uri(project, user);
        let mappings = merged.mappings.unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].prefix.as_deref(), Some("user://"));
    }

    #[test]
    fn merge_uri_both_absent_yields_none() {
        let merged = merge_uri(PartialUriConfig::default(), PartialUriConfig::default());
        assert!(merged.mappings.is_none());
    }
}
