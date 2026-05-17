use crate::config::{Config, load_effective_config};
use crate::utils::path::canonicalize_if_exists;
use crate::utils::text::Text;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum WorkspaceMode {
    SingleFile,
    MultiFile,
}

#[derive(Clone, Debug)]
pub enum DocumentSource {
    Disk,
    Stdin,
}

#[derive(Clone, Debug)]
pub struct WorkspaceDocument {
    pub path: PathBuf,
    pub rel_path: PathBuf,
    pub text: Text,
    pub source: DocumentSource,
}

#[derive(Clone, Debug)]
pub struct DiscoveredFolder {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub documents: Vec<WorkspaceDocument>,
    pub extra_folders: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub folder: DiscoveredFolder,
    pub mode: WorkspaceMode,
    pub config: Config,
}

#[derive(Clone, Debug)]
pub enum WorkspaceInput {
    Path(PathBuf),
    Stdin { text: String, display_name: String },
}

pub fn discover_workspace(
    input: WorkspaceInput,
    root_override: Option<&Path>,
) -> Result<Workspace, crate::config::ConfigError> {
    let stdin_mode = matches!(input, WorkspaceInput::Stdin { .. });
    let cwd = std::env::current_dir().map_err(crate::config::ConfigError::Io)?;
    let (target_path, stdin) = match input {
        WorkspaceInput::Path(path) => (
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            },
            None,
        ),
        WorkspaceInput::Stdin { text, display_name } => {
            let synthetic = cwd.join(display_name);
            (synthetic, Some(text))
        }
    };

    let root = match root_override {
        Some(path) => path.to_path_buf(),
        None => infer_root(&target_path),
    };
    let config = load_effective_config(&root)?;
    let target_is_explicit_file = stdin.is_none() && target_path.is_file();

    let folder = if let Some(text) = stdin {
        let rel_path = PathBuf::from("<stdin>.md");
        DiscoveredFolder {
            root: root.clone(),
            config_path: find_project_config(&root),
            documents: vec![WorkspaceDocument {
                path: root.join(&rel_path),
                rel_path,
                text: Text::new(text),
                source: DocumentSource::Stdin,
            }],
            extra_folders: resolve_extra_folders(&root, &config),
        }
    } else if target_path.is_file() {
        let text = fs::read_to_string(&target_path).map_err(crate::config::ConfigError::Io)?;
        let rel_path = target_path
            .strip_prefix(&root)
            .unwrap_or(target_path.as_path())
            .to_path_buf();
        DiscoveredFolder {
            root: root.clone(),
            config_path: find_project_config(&root),
            documents: vec![WorkspaceDocument {
                path: target_path.clone(),
                rel_path,
                text: Text::new(text),
                source: DocumentSource::Disk,
            }],
            extra_folders: resolve_extra_folders(&root, &config),
        }
    } else {
        let documents =
            collect_documents(&root, &config).map_err(crate::config::ConfigError::Io)?;
        DiscoveredFolder {
            root: root.clone(),
            config_path: find_project_config(&root),
            documents,
            extra_folders: resolve_extra_folders(&root, &config),
        }
    };

    let mode = if stdin_mode || target_is_explicit_file {
        WorkspaceMode::SingleFile
    } else {
        WorkspaceMode::MultiFile
    };

    Ok(Workspace {
        folder,
        mode,
        config,
    })
}

pub fn infer_root(target: &Path) -> PathBuf {
    let base_dir = if target.is_dir() {
        target.to_path_buf()
    } else {
        target
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    let mut current = base_dir.clone();

    loop {
        if current.join(".downlint.toml").exists() || current.join(".git").exists() {
            return canonicalize_if_exists(&current);
        }

        if let Some(parent) = current.parent() {
            if parent == current {
                break;
            }
            current = parent.to_path_buf();
        } else {
            break;
        }
    }

    canonicalize_if_exists(base_dir)
}

fn find_project_config(root: &Path) -> Option<PathBuf> {
    let path = root.join(".downlint.toml");
    path.exists().then_some(path)
}

fn collect_documents(root: &Path, config: &Config) -> std::io::Result<Vec<WorkspaceDocument>> {
    let ext_set: HashSet<String> = config.core.file_extensions.iter().cloned().collect();
    let mut builder = WalkBuilder::new(root);
    builder.follow_links(true).hidden(false);

    // Find symlinked dirs in the root and add them explicitly so the walker
    // traverses them even when .gitignore excludes them.
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = path.symlink_metadata() {
                if metadata.file_type().is_symlink() {
                    let _ = builder.add(&path);
                }
            }
        }
    }

    // Build an Override matcher so negation patterns (e.g. !people) can override
    // .gitignore exclusions during traversal.
    let mut override_builder = OverrideBuilder::new(root);
    let mut exclude_patterns = Vec::new();
    for pattern in &config.core.ignore {
        if pattern.starts_with('!') {
            if let Err(e) = override_builder.add(pattern) {
                eprintln!("Warning: invalid ignore pattern '{}': {}", pattern, e);
            }
        } else {
            exclude_patterns.push(pattern.clone());
        }
    }
    if let Ok(overrides) = override_builder.build() {
        builder.overrides(overrides);
    }

    let mut docs = Vec::new();

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel_path = path.strip_prefix(root).unwrap_or(path);
        // Post-filter: exclude files matching non-negation ignore patterns
        if exclude_patterns.iter().any(|p| {
            globset::Glob::new(p.as_str())
                .ok()
                .map(|g| g.compile_matcher().is_match(rel_path))
                .unwrap_or(false)
        }) {
            continue;
        }
        let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !ext_set.contains(ext) {
            continue;
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        docs.push(WorkspaceDocument {
            path: path.to_path_buf(),
            rel_path: rel_path.to_path_buf(),
            text: Text::new(text),
            source: DocumentSource::Disk,
        });
    }

    docs.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    Ok(docs)
}

fn resolve_extra_folders(root: &Path, config: &Config) -> Vec<PathBuf> {
    config
        .core
        .extra_folders
        .iter()
        .map(|value| {
            // Expand ~ to home directory
            let expanded = if let Some(stripped) = value.strip_prefix("~/") {
            std::env::var("HOME").ok().and_then(|h| PathBuf::try_from(h).ok()).unwrap_or_else(|| PathBuf::from("/")).join(stripped)
            } else {
                root.join(value)
            };
            canonicalize_if_exists(expanded)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_root_prefers_target_parent_without_markers() {
        let path = PathBuf::from("/tmp/demo/doc.md");
        assert_eq!(infer_root(&path), PathBuf::from("/tmp/demo"));
    }
}
