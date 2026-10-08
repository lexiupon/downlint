//! `downlint resolve <TARGET>` — target resolution query (RFC 0012).
//!
//! Reports every destination a link target resolves to, with the reason each
//! matched. A projection of the existing resolution rules (RES-03/04/05/06/07):
//! it predicts `check`'s behavior and introduces no new diagnostics.

use crate::cli::check::OutputFormat;
use crate::resolution::path::{has_scheme, split_anchor};
use crate::resolution::query::{
    ResolveStatus, TargetDestination, TargetResolution, resolve_target,
};
use crate::resolution::{ResolveInput, index_document};
use crate::utils::{Workspace, WorkspaceInput, discover_workspace};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ResolveOptions {
    pub root: Option<PathBuf>,
    pub from: Option<PathBuf>,
    pub format: OutputFormat,
    pub include_prefix: bool,
    pub allow_uri_sync: bool,
    pub target: String,
}

pub fn run_resolve(options: ResolveOptions) -> i32 {
    // Validate the target before doing any work.
    if options.target.is_empty() {
        eprintln!("downlint: error: empty target");
        return 2;
    }
    let (path_part, _anchor) = split_anchor(&options.target);
    if !has_scheme(&options.target) && path_part.is_empty() {
        eprintln!(
            "downlint: error: target is an in-page anchor; in-page anchors need a document context (use --from, or check the link with `downlint check`)"
        );
        return 2;
    }

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
    let mut input = ResolveInput::from_workspace(&workspace);
    if let Some(error) = input.uri_error.take() {
        eprintln!("downlint: error: {error}");
        return 2;
    }

    // Source context (--from): must name a document in the index (primary or
    // mounted); a typo'd --from silently changing the resolution base is
    // worse than an error.
    let source_dir = match &options.from {
        Some(from) => match resolve_from(&input, &workspace, from) {
            Ok(dir) => dir,
            Err(message) => {
                eprintln!("downlint: error: {message}");
                return 2;
            }
        },
        None => workspace.folder.root.clone(),
    };

    let docs: Vec<_> = input
        .documents
        .iter()
        .map(|doc| index_document(doc, &input.root))
        .collect();
    let resolution = resolve_target(
        &input,
        &docs,
        &source_dir,
        &options.target,
        options.include_prefix,
        options.allow_uri_sync,
    );

    match options.format {
        OutputFormat::Text => print_text(&resolution, &input),
        OutputFormat::Json => print_json(&resolution),
    }
    resolution.status.exit_code()
}

/// Resolve `--from` to a source directory: the containing directory of the
/// named document (matched by filesystem path or namespace path).
fn resolve_from(
    input: &ResolveInput,
    workspace: &Workspace,
    from: &Path,
) -> Result<PathBuf, String> {
    let from_abs = if from.is_absolute() {
        from.to_path_buf()
    } else {
        workspace.folder.root.join(from)
    };
    let from_ns = from.to_string_lossy().replace('\\', "/");
    let doc = input.documents.iter().find(|doc| {
        doc.path == from_abs
            || doc
                .namespace_rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .eq_ignore_ascii_case(&from_ns)
    });
    match doc {
        Some(doc) => Ok(doc
            .path
            .parent()
            .unwrap_or(input.root.as_path())
            .to_path_buf()),
        None => Err(format!(
            "--from document not found in workspace: {}",
            from.display()
        )),
    }
}

const TITLE_WIDTH: usize = 40;

fn truncate(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max {
        value.to_string()
    } else {
        chars[..max - 1].iter().collect::<String>() + "…"
    }
}

fn destination_line(dest: &TargetDestination, path_width: usize, show_anchor: bool) -> String {
    let mut line = format!("{:<width$}  ", dest.path.display(), width = path_width);
    if let Some(title) = &dest.title {
        line.push_str(&format!(
            "title: {:<width$}  ",
            truncate(title, TITLE_WIDTH),
            width = TITLE_WIDTH
        ));
    }
    let kinds: Vec<&str> = dest.match_kinds.iter().map(|kind| kind.as_str()).collect();
    line.push_str(&format!("[{}] ", kinds.join(", ")));
    if let Some(mount) = &dest.mount {
        line.push_str(&format!("(mount: {mount}) "));
    }
    if show_anchor && let Some(anchor) = dest.anchor {
        line.push_str(&format!("anchor: {}", if anchor { "yes" } else { "no" }));
    }
    line.trim_end().to_string()
}

fn print_destination_lines(destinations: &[TargetDestination], show_anchor: bool) -> Vec<String> {
    let path_width = destinations
        .iter()
        .map(|dest| dest.path.display().to_string().len())
        .max()
        .unwrap_or(0)
        .max(20);
    destinations
        .iter()
        .map(|dest| destination_line(dest, path_width, show_anchor))
        .collect()
}

fn print_text(resolution: &TargetResolution, input: &ResolveInput) {
    let show_anchor = resolution.anchor.is_some();

    match resolution.status {
        ResolveStatus::External => {
            println!(
                "external — '{}' resolves outside the workspace",
                resolution.target
            );
        }
        ResolveStatus::Unmapped => {
            if input.uri_resolver.is_empty() {
                println!(
                    "unmapped — no [[schemas]] configured for '{}'",
                    resolution.target
                );
            } else {
                println!(
                    "unmapped — no [[schemas]] prefix matched '{}' (more-specific prefixes go first)",
                    resolution.target
                );
            }
        }
        ResolveStatus::MappedPresent
        | ResolveStatus::MappedMissing
        | ResolveStatus::MappedPlaceholder => {
            if let Some(report) = &resolution.scheme
                && let Some(mapped) = &report.mapped_path
            {
                let prefix = report.prefix.as_deref().unwrap_or(&report.scheme);
                println!(
                    "{} — {} → {}",
                    resolution.status.as_str(),
                    prefix,
                    mapped.display()
                );
                match resolution.status {
                    ResolveStatus::MappedMissing => {
                        println!("  mapped file does not exist");
                    }
                    ResolveStatus::MappedPlaceholder => {
                        println!("  evicted cloud placeholder detected");
                    }
                    _ => {}
                }
            }
        }
        _ => {
            let count = resolution.destinations.len();
            let noun = if count == 1 {
                "destination"
            } else {
                "destinations"
            };
            println!("{} — {count} {noun}:", resolution.status.as_str());
            for line in print_destination_lines(&resolution.destinations, show_anchor) {
                println!("  {line}");
            }
            if !resolution.prefix_candidates.is_empty() {
                let count = resolution.prefix_candidates.len();
                let noun = if count == 1 {
                    "candidate"
                } else {
                    "candidates"
                };
                println!("hint: {count} prefix {noun} (enable wiki.obsidian_prefix to match):");
                for line in print_destination_lines(&resolution.prefix_candidates, false) {
                    println!("  {line}");
                }
            }
        }
    }

    if let Some(hint) = &resolution.directory_hint {
        println!("  {hint}");
    }

    if resolution.anchor_unsupported
        && let Some(anchor) = &resolution.anchor
    {
        println!(
            "  note: anchor '#{anchor}' not supported on URI targets (check reports link/broken-anchor)"
        );
    }
}

fn destination_json(dest: &TargetDestination) -> serde_json::Value {
    json!({
        "path": dest.path,
        "title": dest.title,
        "match": dest.match_kinds.iter().map(|kind| kind.as_str()).collect::<Vec<_>>(),
        "mount": dest.mount,
        "anchor": dest.anchor,
    })
}

fn print_json(resolution: &TargetResolution) {
    let scheme = resolution.scheme.as_ref().map(|report| {
        json!({
            "scheme": report.scheme,
            "prefix": report.prefix,
            "mapped_path": report.mapped_path,
            "exists": report.exists,
            "placeholder": report.placeholder,
            "verify": report.verify,
        })
    });
    let body = json!({
        "target": resolution.target,
        "anchor": resolution.anchor,
        "status": resolution.status.as_str(),
        "destinations": resolution.destinations.iter().map(destination_json).collect::<Vec<_>>(),
        "prefix_candidates": resolution.prefix_candidates.iter().map(destination_json).collect::<Vec<_>>(),
        "scheme": scheme,
        "directory_hint": resolution.directory_hint,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
    );
}
