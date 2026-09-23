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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// A mount with its `root` expanded to an absolute filesystem path, plus the
/// config needed by the resolution layer (RFC 0010).
#[derive(Clone, Debug)]
pub struct ResolvedMount {
    /// The resolved filesystem path of the mount.
    pub path: PathBuf,
    /// The exact-path alias (a workspace-absolute virtual directory), if any.
    pub r#as: Option<String>,
    /// Whether links within the mounted docs are linted (they become sources).
    pub lint: bool,
    /// Label used to attribute diagnostics from this mount (its `as`, or
    /// `path` when there is no `as`).
    pub attribution: String,
}

/// The kind of a namespace-level mount conflict (RFC 0011).
///
/// A single kind: a fine-grained namespace-path collision. Either a mount file
/// occupies the same namespace path as a primary file, or a file and a folder
/// share a name (stem) at the same location. The specific case is carried in
/// `MountConflict::detail`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountConflictKind {
    /// A mount file/folder collides with a primary file/folder at the same
    /// namespace path (or same location + name). While unresolved, the
    /// conflicting mount file(s) are targets only (not linted).
    PathCollision,
}

/// A namespace-level conflict between a mount and the primary project
/// (RFC 0010). Reported as a `mount/conflict` error diagnostic.
#[derive(Clone, Debug)]
pub struct MountConflict {
    /// The mount's attribution (`prefix`, or `root` when there is no prefix).
    pub mount_attribution: String,
    pub kind: MountConflictKind,
    /// A human-readable description of the collision.
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct DiscoveredFolder {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub documents: Vec<WorkspaceDocument>,
    pub mounts: Vec<ResolvedMount>,
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
        // Stdin is workspace-anchored: the full workspace is indexed so the
        // piped document's links resolve against workspace documents and
        // attachments. The synthetic `<stdin>.md` document (at the workspace
        // root) is the only source that gets linted.
        let mut documents =
            collect_documents(&root, &config).map_err(crate::config::ConfigError::Io)?;
        let rel_path = PathBuf::from("<stdin>.md");
        documents.push(WorkspaceDocument {
            path: root.join(&rel_path),
            rel_path,
            text: Text::new(text),
            source: DocumentSource::Stdin,
        });
        DiscoveredFolder {
            root: root.clone(),
            config_path: find_project_config(&root),
            documents,
            mounts: resolve_mounts(&root, &config),
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
            mounts: resolve_mounts(&root, &config),
        }
    } else {
        let documents =
            collect_documents(&root, &config).map_err(crate::config::ConfigError::Io)?;
        DiscoveredFolder {
            root: root.clone(),
            config_path: find_project_config(&root),
            documents,
            mounts: resolve_mounts(&root, &config),
        }
    };

    // Stdin is MultiFile (the full workspace is indexed; the stdin document is
    // the only source). SingleFile applies only to an explicit single file on
    // disk, where cross-file diagnostics are suppressed.
    let mode = if target_is_explicit_file {
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
    // Hidden entries are excluded by default (RES-10); `core.include_hidden`
    // restores the include-everything walk (RFC 0023).
    builder.follow_links(true).hidden(!config.core.include_hidden);

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

/// The indexable document paths under `subtree` (a file or directory at or
/// below `root`), applying the same filtering as `collect_documents`:
/// hidden files, `.gitignore`, `core.ignore`, and the configured extension
/// set. The walk is rooted at `root` and pruned to `subtree` via
/// `filter_entry`, so `.gitignore` discovery matches the initial snapshot
/// walk. Used by the LSP's disk-change reconciliation (RFC 0022).
pub fn indexable_paths_under(root: &Path, subtree: &Path, config: &Config) -> Vec<PathBuf> {
    let ext_set: HashSet<String> = config.core.file_extensions.iter().cloned().collect();
    let mut builder = WalkBuilder::new(root);
    // Same hidden-file policy as `collect_documents` (RFC 0023): the
    // reconciliation walk must index exactly what the snapshot indexed.
    builder.follow_links(true).hidden(!config.core.include_hidden);

    // Same root-symlink force-add as `collect_documents`, limited to the
    // branch that can reach `subtree`.
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = path.symlink_metadata()
                && metadata.file_type().is_symlink()
                && (subtree == path || subtree.starts_with(&path))
            {
                let _ = builder.add(&path);
            }
        }
    }

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

    // Keep an entry only when it is on the path to `subtree`, is `subtree`
    // itself, or is under `subtree` — everything else is pruned, so the walk
    // visits O(depth) directories plus the subtree's contents. The closure
    // must be `'static`, so `subtree` is owned by it.
    let subtree_owned = subtree.to_path_buf();
    let builder = builder.filter_entry(move |entry| {
        let path = entry.path();
        subtree_owned.starts_with(path) || path.starts_with(&subtree_owned)
    });

    let mut paths = Vec::new();
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
        paths.push(path.to_path_buf());
    }
    paths
}

fn expand_root(root: &Path, value: &str) -> PathBuf {
    // Expand ~ to home directory; otherwise resolve relative to the workspace root.
    let expanded = if let Some(stripped) = value.strip_prefix("~/") {
        std::env::var("HOME")
            .ok()
            .and_then(|h| PathBuf::try_from(h).ok())
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(stripped)
    } else {
        root.join(value)
    };
    canonicalize_if_exists(expanded)
}

fn resolve_mounts(root: &Path, config: &Config) -> Vec<ResolvedMount> {
    config
        .mounts
        .iter()
        .map(|mount| ResolvedMount {
            attribution: mount
                .r#as
                .clone()
                .unwrap_or_else(|| mount.path.clone()),
            path: expand_root(root, &mount.path),
            r#as: mount.r#as.clone(),
            lint: mount.lint,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn infer_root_prefers_target_parent_without_markers() {
        let path = PathBuf::from("/tmp/demo/doc.md");
        assert_eq!(infer_root(&path), PathBuf::from("/tmp/demo"));
    }

    /// `indexable_paths_under` applies the same filtering as
    /// `collect_documents` (RFC 0022 + RFC 0023): `.gitignore`d files,
    /// `core.ignore`d files, non-markdown files, and — by default — hidden
    /// files and directories are excluded; `core.include_hidden = true`
    /// restores inclusion of hidden entries.
    #[test]
    fn indexable_paths_under_applies_snapshot_filtering() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::create_dir_all(root.join(".trash")).unwrap();
        fs::write(root.join("notes/plain.md"), "# Plain\n").unwrap();
        fs::write(root.join("notes/.hidden.md"), "# Hidden\n").unwrap();
        fs::write(root.join(".trash/old.md"), "# Trashed\n").unwrap();
        fs::write(root.join("notes/gitignored.md"), "# Gitignored\n").unwrap();
        fs::write(root.join("notes/ignored-by-config.md"), "# Ignored\n").unwrap();
        fs::write(root.join("notes/attachment.png"), "x").unwrap();
        // A `.git` dir is required for the ignore crate to honor
        // `.gitignore` files (same for the snapshot walk and this helper).
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "gitignored.md\n").unwrap();

        let mut config = Config::default();
        // `core.ignore` patterns are relative to the workspace root.
        config.core.ignore = vec!["notes/ignored-by-config.md".into()];

        // File-level: a single plain note under the subtree.
        let plain = root.join("notes/plain.md");
        let found = indexable_paths_under(root, &plain, &config);
        assert_eq!(found, vec![plain.clone()]);

        // File-level: hidden notes are excluded by default (RES-10), as are
        // notes inside hidden directories; each other excluded kind yields
        // nothing.
        let hidden = root.join("notes/.hidden.md");
        let trashed = root.join(".trash/old.md");
        for excluded in [
            &hidden,
            &trashed,
            &root.join("notes/gitignored.md"),
            &root.join("notes/ignored-by-config.md"),
            &root.join("notes/attachment.png"),
        ] {
            assert_eq!(
                indexable_paths_under(root, excluded, &config),
                Vec::<PathBuf>::new(),
                "{} should be excluded",
                excluded.display()
            );
        }

        // Directory-level: the subtree reconcile sees the plain note only.
        let notes = root.join("notes");
        let found = indexable_paths_under(root, &notes, &config);
        assert_eq!(found, vec![plain.clone()]);

        // A hidden directory as the subtree itself yields nothing.
        let trash = root.join(".trash");
        assert_eq!(indexable_paths_under(root, &trash, &config), Vec::<PathBuf>::new());

        // `include_hidden = true` restores the include-everything walk
        // (RFC 0023): hidden files and hidden directories come back, while
        // gitignore/config/extension filtering still applies.
        config.core.include_hidden = true;
        assert_eq!(indexable_paths_under(root, &hidden, &config), vec![hidden.clone()]);
        assert_eq!(indexable_paths_under(root, &trashed, &config), vec![trashed.clone()]);
        let mut found = indexable_paths_under(root, &notes, &config);
        found.sort();
        assert_eq!(found, vec![hidden.clone(), plain]);
        // gitignore still excludes (hidden filtering is orthogonal to it).
        assert_eq!(
            indexable_paths_under(root, &root.join("notes/gitignored.md"), &config),
            Vec::<PathBuf>::new()
        );
    }

    /// Snapshot level (RFC 0023): `discover_workspace` excludes hidden files
    /// and directories by default, and `core.include_hidden = true` in the
    /// vault's `.downlint.toml` restores the include-everything walk.
    #[test]
    fn discover_workspace_hidden_policy() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".trash")).unwrap();
        fs::write(root.join("plain.md"), "# Plain\n").unwrap();
        fs::write(root.join(".hidden.md"), "# Hidden\n").unwrap();
        fs::write(root.join(".trash/old.md"), "# Trashed\n").unwrap();
        fs::write(root.join(".downlint.toml"), "").unwrap();

        let stems = |ws: &Workspace| -> Vec<String> {
            ws.folder
                .documents
                .iter()
                .map(|doc| doc.rel_path.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };

        // Default: hidden file and hidden directory are not documents.
        let ws = discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
        assert_eq!(stems(&ws), vec!["plain.md".to_string()]);

        // Opt-in: `core.include_hidden = true` restores them.
        fs::write(root.join(".downlint.toml"), "[core]\ninclude_hidden = true\n").unwrap();
        let ws = discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
        let mut stems = stems(&ws);
        stems.sort();
        assert_eq!(stems, vec![".hidden.md".to_string(), "old.md".to_string(), "plain.md".to_string()]);
    }
}
