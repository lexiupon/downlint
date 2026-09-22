//! `downlint info` — a read-only, descriptive "what does downlint see" report
//! of the resolved workspace (RFC 0016).
//!
//! Projects the resolved workspace — not the config file — into a report:
//! mounts (namespace prefix, resolved path, lint, doc count, presence), schemas
//! (uri, expanded `to`, auto_verify, verify_cmd, presence), document totals, and
//! namespace conflicts.
//!
//! It is **descriptive only**: it builds a `ResolveInput` (index + resolved
//! mounts + schema mappings) but never calls `resolve_links`, so no references
//! are resolved and a schema's `verify_cmd` is never executed. Missing folders
//! are shown as `✗ missing` markers but never affect the exit code.

use crate::cli::check::OutputFormat;
use crate::resolution::ResolveInput;
use crate::resolution::uri::expand_root;
use crate::utils::{Workspace, WorkspaceInput, discover_workspace};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct InfoOptions {
    pub root: Option<PathBuf>,
    pub format: OutputFormat,
}

pub fn run_info(options: InfoOptions) -> i32 {
    let workspace = match discover_workspace(
        WorkspaceInput::Path(PathBuf::from(".")),
        options.root.as_deref(),
    ) {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("downlint: error: {error}");
            return 2;
        }
    };
    let input = ResolveInput::from_workspace(&workspace);
    // A schema `to` that cannot be expanded (missing env var) is a startup
    // config error: `from_workspace` records it and builds an empty resolver.
    // Surface it and exit 2 — we cannot show a resolved path for that schema.
    if let Some(error) = input.uri_error.as_deref() {
        eprintln!("downlint: error: {error}");
        return 2;
    }

    let report = build_report(&workspace, &input);
    render(&report, options.format);
    0
}

// ---- Report model (Serialize for the JSON path) ----

#[derive(Serialize)]
struct InfoReport {
    version: String,
    workspace: String,
    config: Option<String>,
    file_extensions: Vec<String>,
    mounts: Vec<MountInfo>,
    schemas: Vec<SchemaInfo>,
    documents: DocCounts,
    conflicts: Vec<ConflictInfo>,
}

#[derive(Serialize)]
struct MountInfo {
    /// The `as` namespace prefix; `null` when the mount has no `as`.
    r#as: Option<String>,
    /// The resolved absolute filesystem path of the mount.
    path: String,
    lint: bool,
    /// Number of documents indexed from this mount.
    docs: usize,
    /// Whether the mount folder exists on disk.
    exists: bool,
}

#[derive(Serialize)]
struct SchemaInfo {
    uri: String,
    /// The `to` value as written in the config.
    to: String,
    /// The expanded absolute folder the `to` resolves to.
    expanded: String,
    auto_verify: bool,
    /// Whether a custom `verify_cmd` is configured.
    verify_cmd: bool,
    /// Whether the expanded folder exists on disk.
    exists: bool,
}

#[derive(Serialize)]
struct DocCounts {
    total: usize,
    primary: usize,
    mounted: usize,
}

#[derive(Serialize)]
struct ConflictInfo {
    /// The mount's attribution.
    mount: String,
    detail: String,
}

// ---- Report construction (pure over the resolved workspace) ----

fn build_report(workspace: &Workspace, input: &ResolveInput) -> InfoReport {
    let root = &workspace.folder.root;

    let config = workspace
        .folder
        .config_path
        .as_ref()
        .map(|path| path.to_string_lossy().replace('\\', "/"));

    let mounts = input
        .mounts
        .iter()
        .map(|mount| MountInfo {
            r#as: mount.r#as.clone(),
            path: mount.path.to_string_lossy().replace('\\', "/"),
            lint: mount.lint,
            docs: mount_doc_count(input, &mount.attribution),
            exists: mount.path.exists(),
        })
        .collect();

    let schemas = workspace
        .config
        .schemas
        .schemas
        .iter()
        .map(|schema| {
            // By the time we get here `uri_error` is None, so every `to`
            // expanded at startup expands again here. The Err arm is defensive.
            let (expanded, exists) = match expand_root(&schema.to, root) {
                Ok(path) => (path.to_string_lossy().replace('\\', "/"), path.exists()),
                Err(_) => (schema.to.clone(), false),
            };
            SchemaInfo {
                uri: schema.uri.clone(),
                to: schema.to.clone(),
                expanded,
                auto_verify: schema.auto_verify,
                verify_cmd: schema.verify_cmd.is_some(),
                exists,
            }
        })
        .collect();

    let primary = input
        .documents
        .iter()
        .filter(|doc| doc.mount.is_none())
        .count();
    let documents = DocCounts {
        total: input.documents.len(),
        primary,
        mounted: input.documents.len() - primary,
    };

    let conflicts = input
        .conflicts
        .iter()
        .map(|conflict| ConflictInfo {
            mount: conflict.mount_attribution.clone(),
            detail: conflict.detail.clone(),
        })
        .collect();

    InfoReport {
        version: crate::version::VERSION.to_string(),
        workspace: root.to_string_lossy().replace('\\', "/"),
        config,
        file_extensions: workspace.config.core.file_extensions.clone(),
        mounts,
        schemas,
        documents,
        conflicts,
    }
}

/// Count the documents attributed to a mount (by its `attribution` string).
fn mount_doc_count(input: &ResolveInput, attribution: &str) -> usize {
    input
        .documents
        .iter()
        .filter(|doc| doc.mount.as_deref() == Some(attribution))
        .count()
}

// ---- Rendering ----

fn render(report: &InfoReport, format: OutputFormat) {
    match format {
        OutputFormat::Text => println!("{}", render_text(report)),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into())
        ),
    }
}

fn render_text(report: &InfoReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("downlint {}\n", report.version));
    out.push_str(&format!("workspace   {}\n", report.workspace));
    out.push_str(&format!(
        "config      {}\n",
        report.config.as_deref().unwrap_or("(defaults)")
    ));
    out.push_str(&format!(
        "extensions  {}\n",
        report.file_extensions.join(", ")
    ));
    out.push('\n');

    out.push_str(&format!("mounts ({})\n", report.mounts.len()));
    for mount in &report.mounts {
        out.push_str(&format!(
            "  {:<10} {}  lint={}  {} docs  {}\n",
            mount.r#as.as_deref().unwrap_or("(none)"),
            mount.path,
            yesno(mount.lint),
            mount.docs,
            presence(mount.exists),
        ));
    }
    out.push('\n');

    out.push_str(&format!("schemas ({})\n", report.schemas.len()));
    for schema in &report.schemas {
        out.push_str(&format!(
            "  {}  \u{2192}  {}  auto_verify={}  verify_cmd={}  {}\n",
            schema.uri,
            schema.expanded,
            yesno(schema.auto_verify),
            yesno(schema.verify_cmd),
            presence(schema.exists),
        ));
    }
    out.push('\n');

    out.push_str(&format!(
        "documents  {} total  ({} primary \u{b7} {} mounted)\n",
        report.documents.total, report.documents.primary, report.documents.mounted
    ));

    if report.conflicts.is_empty() {
        out.push_str("conflicts  none\n");
    } else {
        out.push_str(&format!("conflicts  ({})\n", report.conflicts.len()));
        for conflict in &report.conflicts {
            out.push_str(&format!("  {}: {}\n", conflict.mount, conflict.detail));
        }
    }

    out
}

fn presence(exists: bool) -> &'static str {
    if exists {
        "\u{2713}" // ✓
    } else {
        "\u{2717} missing" // ✗ missing
    }
}

fn yesno(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> InfoReport {
        InfoReport {
            version: "0.0.0".into(),
            workspace: "/vault".into(),
            config: Some("/vault/.downlint.toml".into()),
            file_extensions: vec!["md".into()],
            mounts: vec![
                MountInfo {
                    r#as: Some("kb".into()),
                    path: "/vault/kb".into(),
                    lint: true,
                    docs: 42,
                    exists: true,
                },
                MountInfo {
                    r#as: None,
                    path: "/vault/archive".into(),
                    lint: false,
                    docs: 7,
                    exists: false,
                },
            ],
            schemas: vec![SchemaInfo {
                uri: "icloud://assets/".into(),
                to: "~/icloud/assets".into(),
                expanded: "/home/u/icloud/assets".into(),
                auto_verify: true,
                verify_cmd: false,
                exists: true,
            }],
            documents: DocCounts {
                total: 56,
                primary: 47,
                mounted: 9,
            },
            conflicts: vec![ConflictInfo {
                mount: "kb".into(),
                detail: "file `kb/x.md` collides".into(),
            }],
        }
    }

    #[test]
    fn presence_and_yesno() {
        assert_eq!(presence(true), "\u{2713}");
        assert_eq!(presence(false), "\u{2717} missing");
        assert_eq!(yesno(true), "yes");
        assert_eq!(yesno(false), "no");
    }

    #[test]
    fn text_report_sections() {
        let text = render_text(&sample_report());
        assert!(text.contains("downlint 0.0.0"));
        assert!(text.contains("workspace   /vault"));
        assert!(text.contains("config      /vault/.downlint.toml"));
        assert!(text.contains("extensions  md"));
        assert!(text.contains("mounts (2)"));
        assert!(text.contains("kb"));
        assert!(text.contains("(none)"));
        assert!(text.contains("lint=yes"));
        assert!(text.contains("lint=no"));
        assert!(text.contains("42 docs"));
        assert!(text.contains("\u{2717} missing")); // archive mount
        assert!(text.contains("schemas (1)"));
        assert!(text.contains("icloud://assets/"));
        assert!(text.contains("/home/u/icloud/assets"));
        assert!(text.contains("auto_verify=yes"));
        assert!(text.contains("verify_cmd=no"));
        assert!(text.contains("documents  56 total  (47 primary \u{b7} 9 mounted)"));
        assert!(text.contains("conflicts  (1)"));
        assert!(text.contains("kb: file `kb/x.md` collides"));
    }

    #[test]
    fn text_report_no_config_and_no_conflicts() {
        let mut report = sample_report();
        report.config = None;
        report.conflicts.clear();
        let text = render_text(&report);
        assert!(text.contains("config      (defaults)"));
        assert!(text.contains("conflicts  none"));
    }

    #[test]
    fn json_report_shape() {
        let value: serde_json::Value = serde_json::to_value(&sample_report()).expect("serializes");
        assert_eq!(value["version"], "0.0.0");
        assert_eq!(value["workspace"], "/vault");
        assert_eq!(value["config"], "/vault/.downlint.toml");
        assert_eq!(value["file_extensions"][0], "md");
        // mounts
        let mounts = value["mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0]["as"], "kb");
        assert_eq!(mounts[0]["docs"], 42);
        assert_eq!(mounts[0]["exists"], true);
        assert_eq!(mounts[1]["as"], serde_json::Value::Null); // no `as`
        assert_eq!(mounts[1]["exists"], false);
        // schemas
        let schemas = value["schemas"].as_array().unwrap();
        assert_eq!(schemas[0]["uri"], "icloud://assets/");
        assert_eq!(schemas[0]["to"], "~/icloud/assets");
        assert_eq!(schemas[0]["expanded"], "/home/u/icloud/assets");
        assert_eq!(schemas[0]["verify_cmd"], false);
        // documents
        assert_eq!(value["documents"]["total"], 56);
        assert_eq!(value["documents"]["primary"], 47);
        assert_eq!(value["documents"]["mounted"], 9);
        // conflicts
        let conflicts = value["conflicts"].as_array().unwrap();
        assert_eq!(conflicts[0]["mount"], "kb");
        assert_eq!(conflicts[0]["detail"], "file `kb/x.md` collides");
    }

    #[test]
    fn json_empty_sections_are_empty_arrays() {
        let mut report = sample_report();
        report.mounts.clear();
        report.schemas.clear();
        report.conflicts.clear();
        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert!(value["mounts"].as_array().unwrap().is_empty());
        assert!(value["schemas"].as_array().unwrap().is_empty());
        assert!(value["conflicts"].as_array().unwrap().is_empty());
    }
}
