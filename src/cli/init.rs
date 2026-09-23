//! `downlint init` — scaffold a `.downlint.toml` in the workspace root.
//!
//! The generated file lists the common options *active* (at their defaults) so
//! it doubles as a self-documenting reference, and leaves the advanced options
//! *commented out* so they're discoverable without cluttering the default run.

use std::fs;
use std::path::PathBuf;

pub struct InitOptions {
    pub root: Option<PathBuf>,
    pub force: bool,
}

/// The scaffolded `.downlint.toml`. Every key is a real, parseable option; the
/// uncommented ones are the common knobs and already reflect their defaults.
const TEMPLATE: &str = r#"# downlint configuration
#
# Lives at the root of your vault/workspace. The options shown uncommented are
# the common ones and already use their defaults — edit them to taste. Advanced
# options are commented out below; uncomment as you need them.
#
#   downlint            lint this workspace
#   downlint --help     show CLI flags
#   downlint init       (re)create this file

[core]
# File extensions treated as markdown documents.
file_extensions = ["md", "markdown"]

# Glob patterns (relative to the workspace root) to skip.
ignore = []

# Derive each document's title from its first heading.
title_from_heading = true

# --- advanced ----------------------------------------------------------
# Compute stable heading IDs used by link/anchor resolution.
# heading_ids = { enable = true }
#
# How much of each document is analyzed: "full" or "incremental".
# text_sync = "full"
#
# --- mounts ------------------------------------------------------------
# Mount an external local markdown folder into the namespace. Its documents are
# indexed and co-equal with this workspace's (a same-name link is
# link/ambiguous). Repeat this block per mount.
# [[mounts]]
# path = "~/another_project/kb"
# as = "/another_project_kb"       # optional virtual path it appears as (starts with /)
# lint = false                     # also lint links within the mounted docs

[wiki]
# Obsidian-style prefix matching for [[links]]: match any file whose path ends
# with the target. Enable for Obsidian vaults.
obsidian_prefix = false

[completion]
# How many completion candidates to offer.
# candidates = 50
#
# Wiki-link completion label: "title-slug" (default), "title", "file-stem", or
# "file-path-stem".
# wiki = { style = "title-slug" }

[code_action]
# LSP code actions.
# toc = { enable = true, include = [1, 2, 3, 4, 5, 6] }
# create_missing_file = { enable = true }

# Map external-storage URI schemes to local folders (rewrite + stat + verify).
# Repeat this block per scheme. `auto_verify` (default true) runs the built-in
# evicted-placeholder heuristics; `verify_cmd` is an advanced escape hatch.
# [[schemas]]
# uri = "icloud://assets/"
# to = "~/icloud/assets"
# auto_verify = true
# verify_cmd = []
"#;

pub fn run_init(options: InitOptions) -> i32 {
    let root = options.root.clone().unwrap_or_else(|| PathBuf::from("."));
    if !root.is_dir() {
        eprintln!("downlint: error: not a directory: {}", root.display());
        return 2;
    }
    let config_path = root.join(".downlint.toml");

    if config_path.exists() && !options.force {
        eprintln!(
            "downlint: .downlint.toml already exists at {}",
            config_path.display()
        );
        eprintln!("downlint: re-run with `downlint init --force` to overwrite it.");
        return 2;
    }

    if let Err(error) = fs::write(&config_path, TEMPLATE) {
        eprintln!(
            "downlint: error: could not write {}: {error}",
            config_path.display()
        );
        return 1;
    }

    println!("downlint: created {}", config_path.display());
    println!("downlint: run `downlint` to lint this workspace.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn template_parses_as_config() {
        // The scaffolded file must be valid: parse it through the real loader.
        let parsed: crate::config::PartialConfig =
            toml::from_str(TEMPLATE).expect("init template must parse as a .downlint.toml");
        // Spot-check a couple of the active (uncommented) options landed.
        assert_eq!(
            parsed.core.as_ref().and_then(|c| c.file_extensions.as_ref()),
            Some(&vec!["md".to_string(), "markdown".to_string()])
        );
        assert_eq!(
            parsed.wiki.as_ref().and_then(|w| w.obsidian_prefix),
            Some(false)
        );
    }

    #[test]
    fn init_creates_config_in_cwd_like_root() {
        let tmp = TempDir::new().unwrap();
        let code = run_init(InitOptions {
            root: Some(tmp.path().to_path_buf()),
            force: false,
        });
        assert_eq!(code, 0);
        let written = tmp.path().join(".downlint.toml");
        assert!(written.exists());
        assert!(fs::read_to_string(&written).unwrap().starts_with("# downlint configuration"));
    }

    #[test]
    fn init_refuses_to_overwrite_without_force() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".downlint.toml");
        fs::write(&path, "# existing user config\n").unwrap();

        let code = run_init(InitOptions {
            root: Some(tmp.path().to_path_buf()),
            force: false,
        });
        assert_eq!(code, 2);
        // Untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "# existing user config\n");

        // --force overwrites.
        let code = run_init(InitOptions {
            root: Some(tmp.path().to_path_buf()),
            force: true,
        });
        assert_eq!(code, 0);
        assert!(fs::read_to_string(&path).unwrap().starts_with("# downlint configuration"));
    }

    #[test]
    fn init_rejects_missing_root() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let code = run_init(InitOptions {
            root: Some(missing),
            force: false,
        });
        assert_eq!(code, 2);
    }
}
