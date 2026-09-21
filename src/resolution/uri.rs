//! Scheme mapping engine: prefix matching, root expansion (env vars, `~`,
//! relative), and per-target resolution into an absolute filesystem path.
//!
//! Resolution is **rewrite + stat + verify** (RFC 0010, Phase 2): this module
//! performs the rewrite (prefix match + root expansion + percent-decode + join).
//! The caller follows up with a stat and, when the schema's `auto_verify` is
//! enabled, the built-in placeholder heuristics. There is no warming and no
//! caching.

use crate::config::SchemaConfig;
use crate::resolution::path::has_scheme;
use std::path::{Path, PathBuf};

/// Outcome of resolving one link target against the configured `[[schemas]]`.
#[derive(Clone, Debug)]
pub enum UriOutcome {
    /// No URI scheme detected — caller should fall through to normal resolution.
    NotApplicable,
    /// URI scheme detected, but no configured prefix matched. The caller should
    /// emit a broken-link diagnostic and (optionally) a hint pointing to
    /// `[[schemas]]`.
    NoMapping { target: String },
    /// URI scheme detected and a prefix matched. The resolved absolute path is
    /// provided so the caller can stat it and (optionally) run placeholder
    /// verification.
    Resolved {
        mapping_index: usize,
        target: String,
        relative: String,
        resolved_path: PathBuf,
    },
}

/// Matched prefix + index, before expansion.
#[derive(Clone, Debug)]
struct PrefixMatch {
    index: usize,
    relative: String,
}

/// Engine used during a single validation pass. Holds the parsed schemas plus
/// the directory used to resolve relative `root` entries (typically the dir
/// containing `.downlint.toml`).
#[derive(Clone, Debug)]
pub struct UriResolver {
    mappings: Vec<CompiledMapping>,
}

#[derive(Clone, Debug)]
struct CompiledMapping {
    canonical_prefix: String,
    prefix_without_slash: String,
    expanded_root: PathBuf,
    auto_verify: bool,
    verify_cmd: Option<Vec<String>>,
}

impl UriResolver {
    /// Build a resolver from the finalized config. `config_dir` is the directory
    /// `.downlint.toml` lives in (used to expand relative `root` values).
    /// `schemas` are sorted by descending prefix length so that the most
    /// specific prefix wins even if a less-specific one would also match.
    pub fn new(config: &SchemaConfig, config_dir: &Path) -> Result<Self, UriExpansionError> {
        let mut compiled = Vec::with_capacity(config.schemas.len());
        for (index, schema) in config.schemas.iter().enumerate() {
            let expanded_root = expand_root(&schema.root, config_dir)
                .map_err(|error| UriExpansionError { index, error })?;
            let canonical_prefix = ensure_trailing_slash(&schema.prefix);
            let prefix_without_slash = canonical_prefix
                .strip_suffix('/')
                .unwrap_or(&canonical_prefix)
                .to_string();
            compiled.push(CompiledMapping {
                canonical_prefix,
                prefix_without_slash,
                expanded_root,
                auto_verify: schema.auto_verify,
                verify_cmd: schema.verify_cmd.clone(),
            });
        }
        compiled.sort_by(|left, right| {
            right
                .canonical_prefix
                .len()
                .cmp(&left.canonical_prefix.len())
        });
        Ok(Self {
            mappings: compiled,
        })
    }

    /// Convenience for tests + callers that don't have any schemas configured.
    pub fn empty() -> Self {
        Self {
            mappings: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Whether the schema at `index` has `auto_verify` enabled (run the
    /// built-in evicted-placeholder heuristics after a successful stat).
    pub fn auto_verify_for(&self, index: usize) -> bool {
        self.mappings.get(index).map(|m| m.auto_verify).unwrap_or(false)
    }

    /// The schema at `index`'s custom verification command, if any.
    pub fn verify_cmd_for(&self, index: usize) -> Option<&[String]> {
        self.mappings.get(index).and_then(|m| m.verify_cmd.as_deref())
    }

    /// Resolve a single link target. Pure: no subprocess execution, no fs::metadata
    /// calls. The caller follows up with a stat and (optionally) verification.
    pub fn resolve(&self, target: &str) -> UriOutcome {
        if !has_scheme(target) {
            return UriOutcome::NotApplicable;
        }
        match self.first_match(target) {
            Some(matched) => self.build_outcome(target, matched),
            None => UriOutcome::NoMapping {
                target: target.to_string(),
            },
        }
    }

    fn first_match(&self, target: &str) -> Option<PrefixMatch> {
        for (index, mapping) in self.mappings.iter().enumerate() {
            if target == mapping.prefix_without_slash
                || target.starts_with(mapping.canonical_prefix.as_str())
            {
                let relative = if target == mapping.prefix_without_slash {
                    String::new()
                } else {
                    target[mapping.canonical_prefix.len()..].to_string()
                };
                return Some(PrefixMatch {
                    index,
                    relative,
                });
            }
        }
        None
    }

    fn build_outcome(&self, target: &str, matched: PrefixMatch) -> UriOutcome {
        let mapping = &self.mappings[matched.index];
        let relative = matched.relative.trim_start_matches('/');
        // Percent-decode the relative portion so `resolved_path` is a usable
        // filesystem path. The original `target` field on the outcome keeps
        // its encoded form for display/diagnostic purposes.
        let relative_decoded = crate::resolution::path::percent_decode(relative);
        let resolved_path = if relative_decoded.is_empty() {
            mapping.expanded_root.clone()
        } else {
            mapping.expanded_root.join(&relative_decoded)
        };
        UriOutcome::Resolved {
            mapping_index: matched.index,
            target: target.to_string(),
            relative: relative.to_string(),
            resolved_path,
        }
    }
}

/// Per-mapping expansion failure. Surfaced at startup so misconfigured `root`
/// values fail fast rather than producing silent broken links.
#[derive(Debug)]
pub struct UriExpansionError {
    pub index: usize,
    pub error: ExpansionError,
}

impl std::fmt::Display for UriExpansionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "schemas[{}].root: {}", self.index, self.error)
    }
}

impl std::error::Error for UriExpansionError {}

#[derive(Debug)]
pub enum ExpansionError {
    MissingEnvVar(String),
    NoHomeDirectory,
}

impl std::fmt::Display for ExpansionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEnvVar(name) => write!(f, "environment variable `${name}` is not set"),
            Self::NoHomeDirectory => write!(f, "`~` expansion requires a home directory"),
        }
    }
}

/// Expand `$VAR`, `${VAR}`, then `~`, then resolve relative paths against
/// `config_dir`. Surfaces env-var failures explicitly.
pub fn expand_root(raw: &str, config_dir: &Path) -> Result<PathBuf, ExpansionError> {
    let after_env = expand_env_vars(raw)?;
    let after_home = expand_home(&after_env)?;
    let expanded = if after_home.as_os_str().is_empty() {
        after_home
    } else if after_home.is_absolute() {
        after_home
    } else {
        config_dir.join(after_home)
    };
    Ok(expanded)
}

fn expand_env_vars(input: &str) -> Result<PathBuf, ExpansionError> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // Consume one following character verbatim. This protects against
            // Windows-style escapes inside `${VAR}`-style values such as
            // `C:\Users\$USER\foo`.
            if let Some(next) = chars.next() {
                result.push(next);
            }
            continue;
        }
        if ch != '$' {
            result.push(ch);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            let name: String = chars
                .by_ref()
                .take_while(|value| *value != '}')
                .collect();
            let value = std::env::var(&name)
                .map_err(|_| ExpansionError::MissingEnvVar(name.clone()))?;
            result.push_str(&value);
            continue;
        }
        let mut name = String::new();
        while let Some(&next) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                name.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if name.is_empty() {
            result.push('$');
            continue;
        }
        let value = std::env::var(&name)
            .map_err(|_| ExpansionError::MissingEnvVar(name.clone()))?;
        result.push_str(&value);
    }
    Ok(PathBuf::from(result))
}

fn expand_home(input: &Path) -> Result<PathBuf, ExpansionError> {
    let as_str = input.to_string_lossy();
    if as_str == "~" {
        return dirs::home_dir().ok_or(ExpansionError::NoHomeDirectory);
    }
    if let Some(stripped) = as_str.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or(ExpansionError::NoHomeDirectory)?;
        return Ok(home.join(stripped));
    }
    if let Some(stripped) = as_str.strip_prefix("~\\") {
        let home = dirs::home_dir().ok_or(ExpansionError::NoHomeDirectory)?;
        return Ok(home.join(stripped));
    }
    Ok(input.to_path_buf())
}

fn ensure_trailing_slash(prefix: &str) -> String {
    if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::PartialSchema;
    use crate::config::finalize_schemas;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn config_with_mapping(prefix: &str, root: &str) -> SchemaConfig {
        finalize_schemas(vec![PartialSchema {
            prefix: Some(prefix.to_string()),
            root: Some(root.to_string()),
            ..Default::default()
        }])
        .unwrap()
    }

    #[test]
    fn non_uri_target_returns_not_applicable() {
        let dir = tempdir();
        let resolver = UriResolver::new(&config_with_mapping("onedrive://work/", "./assets"), dir.path())
            .unwrap();
        match resolver.resolve("local/file.md") {
            UriOutcome::NotApplicable => {}
            other => panic!("expected NotApplicable, got {other:?}"),
        }
    }

    #[test]
    fn matched_prefix_resolves_relative_path() {
        let dir = tempdir();
        let assets = dir.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let resolver = UriResolver::new(
            &config_with_mapping("onedrive://work/", "./assets"),
            dir.path(),
        )
        .unwrap();
        match resolver.resolve("onedrive://work/2025/report.xlsx") {
            UriOutcome::Resolved {
                mapping_index,
                resolved_path,
                ..
            } => {
                assert_eq!(mapping_index, 0);
                assert_eq!(resolved_path, assets.join("2025/report.xlsx"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn trailing_slash_normalization_matches_both_forms() {
        let dir = tempdir();
        let resolver = UriResolver::new(
            &config_with_mapping("onedrive://work", "./assets"),
            dir.path(),
        )
        .unwrap();
        assert!(matches!(
            resolver.resolve("onedrive://work/file.md"),
            UriOutcome::Resolved { .. }
        ));
    }

    #[test]
    fn no_matching_prefix_emits_no_mapping() {
        let dir = tempdir();
        let resolver = UriResolver::new(
            &config_with_mapping("onedrive://work/", "./assets"),
            dir.path(),
        )
        .unwrap();
        match resolver.resolve("s3://other/file.md") {
            UriOutcome::NoMapping { target } => {
                assert_eq!(target, "s3://other/file.md");
            }
            other => panic!("expected NoMapping, got {other:?}"),
        }
    }

    #[test]
    fn more_specific_prefix_wins() {
        let dir = tempdir();
        let cfg = finalize_schemas(vec![
            PartialSchema {
                prefix: Some("onedrive://work/".to_string()),
                root: Some("./work".to_string()),
                ..Default::default()
            },
            PartialSchema {
                prefix: Some("onedrive://work/bucket-a/".to_string()),
                root: Some("./bucket-a".to_string()),
                ..Default::default()
            },
        ])
        .unwrap();
        let resolver = UriResolver::new(&cfg, dir.path()).unwrap();
        // More-specific prefix (declared second but wins via longest-first ordering).
        match resolver.resolve("onedrive://work/bucket-a/file.md") {
            UriOutcome::Resolved { resolved_path, .. } => {
                assert_eq!(resolved_path, dir.path().join("bucket-a/file.md"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn env_var_expansion() {
        // SAFETY: tests run single-threaded; we set and immediately read.
        unsafe {
            std::env::set_var("DOWNLINT_TEST_ROOT", "/tmp/env-test");
        }
        let dir = tempdir();
        let resolver =
            UriResolver::new(&config_with_mapping("scheme://", "$DOWNLINT_TEST_ROOT/x"), dir.path())
                .unwrap();
        match resolver.resolve("scheme://file") {
            UriOutcome::Resolved { resolved_path, .. } => {
                assert_eq!(resolved_path, PathBuf::from("/tmp/env-test/x/file"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        // SAFETY: same as above.
        unsafe {
            std::env::remove_var("DOWNLINT_TEST_ROOT");
        }
    }

    #[test]
    fn missing_env_var_surfaces_error() {
        // SAFETY: tests run single-threaded.
        unsafe {
            std::env::remove_var("DOWNLINT_TEST_MISSING");
        }
        let dir = tempdir();
        let result = UriResolver::new(
            &config_with_mapping("scheme://", "$DOWNLINT_TEST_MISSING/x"),
            dir.path(),
        );
        let err = result.unwrap_err();
        assert_eq!(err.index, 0);
        assert!(matches!(
            err.error,
            ExpansionError::MissingEnvVar(name) if name == "DOWNLINT_TEST_MISSING"
        ));
    }

    #[test]
    fn tilde_expansion_uses_dirs_home() {
        let dir = tempdir();
        let home = dirs::home_dir().expect("home dir required");
        let resolver =
            UriResolver::new(&config_with_mapping("scheme://", "~/assets"), dir.path()).unwrap();
        match resolver.resolve("scheme://file") {
            UriOutcome::Resolved { resolved_path, .. } => {
                assert_eq!(resolved_path, home.join("assets/file"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn auto_verify_for_reflects_schema() {
        let dir = tempdir();
        let cfg = finalize_schemas(vec![
            PartialSchema {
                prefix: Some("a://".to_string()),
                root: Some("./a".to_string()),
                auto_verify: Some(false),
                ..Default::default()
            },
            PartialSchema {
                prefix: Some("b://".to_string()),
                root: Some("./b".to_string()),
                auto_verify: None, // default true
                ..Default::default()
            },
        ])
        .unwrap();
        let resolver = UriResolver::new(&cfg, dir.path()).unwrap();
        // Both schemas are present; check auto_verify per index.
        assert!(resolver.auto_verify_for(0) != resolver.auto_verify_for(1));
    }

    #[test]
    fn empty_resolver_returns_empty_outcomes() {
        let dir = tempdir();
        let resolver = UriResolver::new(&SchemaConfig::default(), dir.path()).unwrap();
        assert!(resolver.is_empty());
        assert!(matches!(
            resolver.resolve("onedrive://work/file"),
            UriOutcome::NoMapping { .. }
        ));
    }

    #[test]
    fn percent_encoded_target_decodes_to_filesystem_path() {
        let dir = tempdir();
        let resolver = UriResolver::new(
            &config_with_mapping(
                "file:///Users/alice/icloud/assets/",
                "/Users/alice/icloud/assets",
            ),
            dir.path(),
        )
        .unwrap();

        // A realistic percent-encoded URL.
        let target = "file:///Users/alice/icloud/assets/vendor/product%20example%20V1.3.pptx";
        let outcome = resolver.resolve(target);
        let resolved = match outcome {
            UriOutcome::Resolved { resolved_path, .. } => resolved_path,
            other => panic!("expected Resolved, got {other:?}"),
        };
        // The resolved path should be the actual filesystem filename:
        // percent-decoded (spaces), not the encoded form.
        let as_string = resolved.to_string_lossy().into_owned();
        assert!(as_string.contains(' '), "expected decoded space, got {as_string:?}");
        assert!(!as_string.contains("%20"), "%20 should be decoded, got {as_string:?}");
        assert!(
            as_string.ends_with("product example V1.3.pptx"),
            "expected decoded filename, got {as_string:?}"
        );
    }
}
