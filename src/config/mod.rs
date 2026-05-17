pub mod project;
pub mod user;

use serde::Deserialize;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub use project::project_config_path;
pub use user::user_config_path;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub core: CoreConfig,
    pub code_action: CodeActionConfig,
    pub completion: CompletionConfig,
}

#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub file_extensions: Vec<String>,
    pub heading_ids: HeadingIdsConfig,
    pub text_sync: TextSyncKind,
    pub title_from_heading: bool,
    pub extra_folders: Vec<String>,
    pub attachment_file_extensions: Vec<String>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            file_extensions: vec!["md".into(), "markdown".into()],
            heading_ids: HeadingIdsConfig { enable: true },
            text_sync: TextSyncKind::Full,
            title_from_heading: true,
            extra_folders: Vec::new(),
            attachment_file_extensions: vec![
                "png".into(),
                "jpg".into(),
                "jpeg".into(),
                "gif".into(),
                "svg".into(),
                "pdf".into(),
                "webp".into(),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct HeadingIdsConfig {
    pub enable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TextSyncKind {
    Full,
    Incremental,
}

#[derive(Clone, Debug)]
pub struct CodeActionConfig {
    pub toc: TocConfig,
    pub create_missing_file: CreateMissingFileConfig,
}

impl Default for CodeActionConfig {
    fn default() -> Self {
        Self {
            toc: TocConfig::default(),
            create_missing_file: CreateMissingFileConfig { enable: true },
        }
    }
}

#[derive(Clone, Debug)]
pub struct TocConfig {
    pub enable: bool,
    pub include: Vec<u8>,
}

impl Default for TocConfig {
    fn default() -> Self {
        Self {
            enable: true,
            include: vec![1, 2, 3, 4, 5, 6],
        }
    }
}

#[derive(Clone, Debug)]
pub struct CreateMissingFileConfig {
    pub enable: bool,
}

#[derive(Clone, Debug)]
pub struct CompletionConfig {
    pub candidates: usize,
    pub wiki: WikiCompletionConfig,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            candidates: 50,
            wiki: WikiCompletionConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WikiCompletionConfig {
    pub style: WikiCompletionStyle,
}

impl Default for WikiCompletionConfig {
    fn default() -> Self {
        Self {
            style: WikiCompletionStyle::TitleSlug,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WikiCompletionStyle {
    TitleSlug,
    Title,
    FileStem,
    FilePathStem,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    ParseToml { path: PathBuf, message: String },
    Validation(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::ParseToml { path, message } => {
                write!(f, "failed to parse `{}`: {message}", path.display())
            }
            Self::Validation(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialConfig {
    pub core: Option<PartialCoreConfig>,
    pub code_action: Option<PartialCodeActionConfig>,
    pub completion: Option<PartialCompletionConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialCoreConfig {
    pub file_extensions: Option<Vec<String>>,
    pub heading_ids: Option<PartialHeadingIdsConfig>,
    pub text_sync: Option<TextSyncKind>,
    pub title_from_heading: Option<bool>,
    pub extra_folders: Option<Vec<String>>,
    pub attachment_file_extensions_add: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialHeadingIdsConfig {
    pub enable: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialCodeActionConfig {
    pub toc: Option<PartialTocConfig>,
    pub create_missing_file: Option<PartialCreateMissingFileConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialTocConfig {
    pub enable: Option<bool>,
    pub include: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialCreateMissingFileConfig {
    pub enable: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialCompletionConfig {
    pub candidates: Option<usize>,
    pub wiki: Option<PartialWikiCompletionConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialWikiCompletionConfig {
    pub style: Option<WikiCompletionStyle>,
}

pub fn load_effective_config(root: &Path) -> Result<Config, ConfigError> {
    let user = match user_config_path() {
        Some(path) => load_optional(path)?,
        None => None,
    }
    .unwrap_or_default();
    let project = load_optional(project_config_path(root))?.unwrap_or_default();
    finalize_config(merge_partial(project, user))
}

pub fn parse_partial_config(path: &Path) -> Result<PartialConfig, ConfigError> {
    let contents = fs::read_to_string(path).map_err(ConfigError::Io)?;
    toml::from_str::<PartialConfig>(&contents).map_err(|error| ConfigError::ParseToml {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn finalize_config(partial: PartialConfig) -> Result<Config, ConfigError> {
    let defaults = Config::default();
    let core = partial.core.unwrap_or_default();
    let code_action = partial.code_action.unwrap_or_default();
    let completion = partial.completion.unwrap_or_default();

    let file_extensions = core
        .file_extensions
        .unwrap_or_else(|| defaults.core.file_extensions.clone());
    if file_extensions.is_empty() {
        return Err(ConfigError::Validation(
            "core.file_extensions must not be empty".into(),
        ));
    }

    let toc_include = code_action
        .toc
        .as_ref()
        .and_then(|toc| toc.include.clone())
        .unwrap_or_else(|| defaults.code_action.toc.include.clone());
    if toc_include.is_empty() || toc_include.iter().any(|level| !(1..=6).contains(level)) {
        return Err(ConfigError::Validation(
            "code_action.toc.include must contain heading levels 1-6".into(),
        ));
    }

    let mut attachment_file_extensions = defaults.core.attachment_file_extensions.clone();
    if let Some(extra) = core.attachment_file_extensions_add.clone() {
        attachment_file_extensions.extend(extra);
    }
    attachment_file_extensions.sort();
    attachment_file_extensions.dedup();

    Ok(Config {
        core: CoreConfig {
            file_extensions,
            heading_ids: HeadingIdsConfig {
                enable: core
                    .heading_ids
                    .and_then(|value| value.enable)
                    .unwrap_or(defaults.core.heading_ids.enable),
            },
            text_sync: core.text_sync.unwrap_or(defaults.core.text_sync),
            title_from_heading: core
                .title_from_heading
                .unwrap_or(defaults.core.title_from_heading),
            extra_folders: core.extra_folders.unwrap_or_default(),
            attachment_file_extensions,
        },
        code_action: CodeActionConfig {
            toc: TocConfig {
                enable: code_action
                    .toc
                    .as_ref()
                    .and_then(|value| value.enable)
                    .unwrap_or(defaults.code_action.toc.enable),
                include: toc_include,
            },
            create_missing_file: CreateMissingFileConfig {
                enable: code_action
                    .create_missing_file
                    .and_then(|value| value.enable)
                    .unwrap_or(defaults.code_action.create_missing_file.enable),
            },
        },
        completion: CompletionConfig {
            candidates: completion
                .candidates
                .unwrap_or(defaults.completion.candidates)
                .max(1),
            wiki: WikiCompletionConfig {
                style: completion
                    .wiki
                    .and_then(|value| value.style)
                    .unwrap_or(defaults.completion.wiki.style),
            },
        },
    })
}

pub fn merge_partial(high: PartialConfig, low: PartialConfig) -> PartialConfig {
    PartialConfig {
        core: Some(merge_core(
            high.core.unwrap_or_default(),
            low.core.unwrap_or_default(),
        )),
        code_action: Some(merge_code_action(
            high.code_action.unwrap_or_default(),
            low.code_action.unwrap_or_default(),
        )),
        completion: Some(merge_completion(
            high.completion.unwrap_or_default(),
            low.completion.unwrap_or_default(),
        )),
    }
}

fn merge_core(high: PartialCoreConfig, low: PartialCoreConfig) -> PartialCoreConfig {
    PartialCoreConfig {
        file_extensions: high.file_extensions.or(low.file_extensions),
        heading_ids: Some(PartialHeadingIdsConfig {
            enable: high
                .heading_ids
                .and_then(|value| value.enable)
                .or(low.heading_ids.and_then(|value| value.enable)),
        }),
        text_sync: high.text_sync.or(low.text_sync),
        title_from_heading: high.title_from_heading.or(low.title_from_heading),
        extra_folders: high.extra_folders.or(low.extra_folders),
        attachment_file_extensions_add: merge_vec(
            high.attachment_file_extensions_add,
            low.attachment_file_extensions_add,
        ),
    }
}

fn merge_code_action(
    high: PartialCodeActionConfig,
    low: PartialCodeActionConfig,
) -> PartialCodeActionConfig {
    PartialCodeActionConfig {
        toc: Some(PartialTocConfig {
            enable: high
                .toc
                .as_ref()
                .and_then(|value| value.enable)
                .or(low.toc.as_ref().and_then(|value| value.enable)),
            include: high
                .toc
                .as_ref()
                .and_then(|value| value.include.clone())
                .or(low.toc.and_then(|value| value.include)),
        }),
        create_missing_file: Some(PartialCreateMissingFileConfig {
            enable: high
                .create_missing_file
                .and_then(|value| value.enable)
                .or(low.create_missing_file.and_then(|value| value.enable)),
        }),
    }
}

fn merge_completion(
    high: PartialCompletionConfig,
    low: PartialCompletionConfig,
) -> PartialCompletionConfig {
    PartialCompletionConfig {
        candidates: high.candidates.or(low.candidates),
        wiki: Some(PartialWikiCompletionConfig {
            style: high
                .wiki
                .and_then(|value| value.style)
                .or(low.wiki.and_then(|value| value.style)),
        }),
    }
}

fn merge_vec(high: Option<Vec<String>>, low: Option<Vec<String>>) -> Option<Vec<String>> {
    match (high, low) {
        (Some(high), Some(low)) => {
            let mut values = low;
            values.extend(high);
            Some(values)
        }
        (Some(high), None) => Some(high),
        (None, Some(low)) => Some(low),
        (None, None) => None,
    }
}

fn load_optional(path: PathBuf) -> Result<Option<PartialConfig>, ConfigError> {
    if path.exists() {
        parse_partial_config(&path).map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_accumulates_attachment_extensions() {
        let high = PartialConfig {
            core: Some(PartialCoreConfig {
                attachment_file_extensions_add: Some(vec!["drawio".into()]),
                ..PartialCoreConfig::default()
            }),
            ..PartialConfig::default()
        };
        let low = PartialConfig {
            core: Some(PartialCoreConfig {
                attachment_file_extensions_add: Some(vec!["mermaid".into()]),
                ..PartialCoreConfig::default()
            }),
            ..PartialConfig::default()
        };

        let config = finalize_config(merge_partial(high, low)).unwrap();
        assert!(
            config
                .core
                .attachment_file_extensions
                .contains(&"drawio".to_string())
        );
        assert!(
            config
                .core
                .attachment_file_extensions
                .contains(&"mermaid".to_string())
        );
    }
}
