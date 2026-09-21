//! `[[schemas]]` configuration: maps an external URI scheme prefix to a local
//! folder. Resolution is **rewrite + stat + verify** — no warming, no caching
//! (RFC 0010, Phase 2). This replaces the old `[uri]` / `[[uri.mappings]]`
//! section, dropping the `warm_cmd` / `warm_required` / `warm_timeout` keys and
//! the global `auto_verify` mode in favor of a per-schema `auto_verify` bool.

use serde::Deserialize;
use std::fmt;

/// Resolved (non-Partial) schema configuration. Holds the parsed entries from
/// `[[schemas]]`. Config errors are surfaced as `SchemaConfigError::Validation`.
#[derive(Clone, Debug, Default)]
pub struct SchemaConfig {
    pub schemas: Vec<Schema>,
}

/// One row of `[[schemas]]`: maps an external URI scheme prefix (e.g.
/// `icloud://assets/`) to a local folder. The link must carry the full prefix.
#[derive(Clone, Debug)]
pub struct Schema {
    /// The scheme prefix, e.g. `icloud://assets/`.
    pub prefix: String,
    /// The local folder the prefix maps to. Supports `~`, env vars, and
    /// relative-to-config-dir paths (expanded by the resolver).
    pub root: String,
    /// Run the built-in evicted-placeholder heuristics (vendor-specific: iCloud
    /// `.icloud` sibling, OneDrive `._<name>` resource fork). Default `true`.
    pub auto_verify: bool,
    /// Optional custom verification command (advanced escape hatch). Exits 0 →
    /// real file; non-zero → placeholder. `{path}` is substituted with the
    /// resolved absolute file path.
    pub verify_cmd: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialSchema {
    pub prefix: Option<String>,
    pub root: Option<String>,
    pub auto_verify: Option<bool>,
    pub verify_cmd: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum SchemaConfigError {
    Validation(String),
}

impl fmt::Display for SchemaConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SchemaConfigError {}

/// Validate a single schema entry. Returns the validated `Schema`, or a
/// `Validation` error describing the first broken rule.
pub fn finalize_schema(index: usize, partial: PartialSchema) -> Result<Schema, SchemaConfigError> {
    let prefix = partial
        .prefix
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "schemas[*].prefix is required"))?;
    let root = partial
        .root
        .filter(|value| !value.is_empty())
        .ok_or_else(|| validation_at(index, "schemas[*].root is required"))?;
    let auto_verify = partial.auto_verify.unwrap_or(true);
    Ok(Schema {
        prefix,
        root,
        auto_verify,
        verify_cmd: partial.verify_cmd,
    })
}

/// Validate an entire `[[schemas]]` section.
pub fn finalize_schemas(schemas: Vec<PartialSchema>) -> Result<SchemaConfig, SchemaConfigError> {
    let mut finalized = Vec::new();
    for (index, entry) in schemas.into_iter().enumerate() {
        finalized.push(finalize_schema(index, entry)?);
    }
    Ok(SchemaConfig { schemas: finalized })
}

/// Project-precedence merge of two partial `[[schemas]]` sections. Project
/// (`high`) takes the entire `schemas` list when present; otherwise `low` is used.
pub fn merge_schemas(
    high: Option<Vec<PartialSchema>>,
    low: Option<Vec<PartialSchema>>,
) -> Option<Vec<PartialSchema>> {
    high.or(low)
}

/// Helper: turn a message into a UTF-8-prefixed validation error naming the
/// offending schema index. The intent is to make config errors easy to find.
fn validation_at(index: usize, message: &str) -> SchemaConfigError {
    SchemaConfigError::Validation(format!("schemas[{index}]: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(
        prefix: Option<&str>,
        root: Option<&str>,
        auto_verify: Option<bool>,
    ) -> PartialSchema {
        PartialSchema {
            prefix: prefix.map(|value| value.to_string()),
            root: root.map(|value| value.to_string()),
            auto_verify,
            verify_cmd: None,
        }
    }

    #[test]
    fn minimal_schema_uses_defaults() {
        let cfg = finalize_schema(
            0,
            partial(Some("icloud://assets/"), Some("~/icloud/assets"), None),
        )
        .unwrap();
        assert_eq!(cfg.prefix, "icloud://assets/");
        assert!(cfg.auto_verify, "auto_verify defaults to true");
        assert!(cfg.verify_cmd.is_none());
    }

    #[test]
    fn auto_verify_can_be_disabled() {
        let cfg = finalize_schema(
            0,
            partial(Some("icloud://assets/"), Some("~/icloud/assets"), Some(false)),
        )
        .unwrap();
        assert!(!cfg.auto_verify);
    }

    #[test]
    fn prefix_and_root_are_required() {
        assert!(finalize_schema(0, partial(None, Some("x"), None)).is_err());
        assert!(finalize_schema(0, partial(Some("s://"), None, None)).is_err());
    }

    #[test]
    fn finalize_schemas_collects_entries() {
        let cfg = finalize_schemas(vec![
            partial(Some("a://"), Some("~/a"), None),
            partial(Some("b://"), Some("~/b"), Some(false)),
        ])
        .unwrap();
        assert_eq!(cfg.schemas.len(), 2);
        assert!(cfg.schemas[0].auto_verify);
        assert!(!cfg.schemas[1].auto_verify);
    }
}
