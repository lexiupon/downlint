//! RFC 0024: exact inline file paths and slash-only directory links.
mod common;

use assert_cmd::Command;
use common::write_vault;
use downlint::diagnostics::{
    DiagnosticCode, DiagnosticConfig, DiagnosticSeverity, check_diagnostics,
};
use downlint::resolution::conn::DestinationKind;
use downlint::resolution::{ResolveInput, resolve_links};
use downlint::utils::{WorkspaceInput, discover_workspace};
use predicates::prelude::*;
use std::path::Path;

fn graph(root: &Path) -> downlint::resolution::ConnectionGraph {
    let ws = discover_workspace(WorkspaceInput::Path(root.to_path_buf()), Some(root)).unwrap();
    resolve_links(ResolveInput::from_workspace(&ws))
}

#[test]
fn license_and_extensionless_inline_targets_are_attachments() {
    let temp = write_vault(&[
        (
            "index.md",
            "[Apache License 2.0](LICENSE)\n[report](report)\n![image](report)\n[report](report#section)\n",
        ),
        ("LICENSE", "Apache License 2.0"),
        ("report", "# Not a document\n[missing](missing)\n"),
        ("report.md", "# Report\n"),
    ]);
    let g = graph(temp.path());
    assert_eq!(g.resolved_references.len(), 4);
    assert!(check_diagnostics(&g, &DiagnosticConfig::default()).is_empty());
    for r in &g.resolved_references {
        assert!(matches!(
            r.destinations[0].kind,
            DestinationKind::Attachment
        ));
    }
    assert!(
        !g.documents
            .iter()
            .any(|d| d.path.ends_with("LICENSE") || d.path.ends_with("report"))
    );
}

#[test]
fn inline_does_not_infer_markdown_extension_and_preserves_headings() {
    let temp = write_vault(&[
        (
            "index.md",
            "[report](report)\n[report](report.md)\n[report](report.md#missing)\n[[report]]\n",
        ),
        ("report.md", "# Report\n"),
    ]);
    let g = graph(temp.path());
    let diagnostics = check_diagnostics(&g, &DiagnosticConfig::default());
    assert_eq!(diagnostics.len(), 2);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::LinkBroken
                && d.severity == DiagnosticSeverity::Warning)
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::LinkBrokenAnchor)
    );
    assert!(
        g.resolved_references
            .iter()
            .all(|r| matches!(r.destinations[0].kind, DestinationKind::Document))
    );
}

#[test]
fn extensionless_files_are_source_relative_and_percent_decoded() {
    let temp = write_vault(&[
        (
            "docs/index.md",
            "[report](report)\n[report](../report)\n[report](/report)\n[report](report%20topic)\n[missing](missing)\n",
        ),
        ("docs/report", ""),
        ("report", ""),
        ("docs/report topic", ""),
        ("missing", ""),
    ]);
    let g = graph(temp.path());
    assert_eq!(g.resolved_references.len(), 4);
    let source_dir = g.documents[0].path.parent().unwrap();
    assert_eq!(
        g.resolved_references[0].destinations[0].path,
        source_dir.join("report")
    );
    assert_eq!(g.unresolved_references.len(), 1);
    assert_eq!(g.unresolved_references[0].target, "missing");
}

#[test]
fn directory_targets_require_slashes_and_hint_only_for_exact_directories() {
    let temp = write_vault(&[
        (
            "index.md",
            "[report](report)\n[report](./report)\n[report](/report)\n[report](docs/report)\n[report](report.md)\n[[./report]]\n[report](report#section)\n[report](report/)\n[report](./report/)\n[[./report/]]\n[missing](missing)\n[image](image.png)\n[image](image.png/)\n[web](https://example.com)\n",
        ),
        ("report/keep", ""),
        ("docs/report/keep", ""),
        ("report.md/keep", ""),
        ("image.png", ""),
    ]);
    let g = graph(temp.path());
    let diagnostics = check_diagnostics(&g, &DiagnosticConfig::default());
    let hints: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.message.contains("directory links require a trailing '/'"))
        .collect();
    assert_eq!(hints.len(), 7);
    assert!(hints.iter().all(|d| d.code == DiagnosticCode::LinkBroken));
    assert_eq!(
        hints
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Error)
            .count(),
        1
    );
    assert!(
        hints
            .iter()
            .any(|d| d.message.contains("remove the fragment"))
    );
    assert_eq!(
        g.resolved_references
            .iter()
            .filter(|r| matches!(r.destinations[0].kind, DestinationKind::Directory))
            .count(),
        3
    );
    assert_eq!(diagnostics.len(), 9);
}

#[test]
fn bare_wiki_stays_note_only_and_document_matching_has_priority() {
    let temp = write_vault(&[
        ("index.md", "[[report]]\n[[./report]]\n[[missing]]\n"),
        ("report", ""),
        ("missing/keep", ""),
    ]);
    let g = graph(temp.path());
    assert_eq!(g.resolved_references.len(), 1);
    assert_eq!(g.unresolved_references.len(), 2);
    assert!(
        g.unresolved_references
            .iter()
            .all(|r| r.directory_hint.is_none())
    );
    let temp = write_vault(&[
        ("index.md", "[[./report]]\n"),
        ("report.md", "# Report\n"),
        ("report/keep", ""),
    ]);
    let g = graph(temp.path());
    assert!(g.unresolved_references.is_empty());
    assert!(matches!(
        g.resolved_references[0].destinations[0].kind,
        DestinationKind::Document
    ));
}

#[test]
fn hidden_and_ignored_plain_files_remain_exact_attachment_targets() {
    let temp = write_vault(&[
        (".downlint.toml", "[core]\nignore = [\"report\"]\n"),
        ("index.md", "[report](report)\n[report](.report)\n"),
        ("report", ""),
        (".report", ""),
    ]);
    let g = graph(temp.path());
    assert_eq!(g.resolved_references.len(), 2);
    assert!(g.unresolved_references.is_empty());
}

#[test]
fn cli_workspace_stdin_and_single_file_directory_diagnostics() {
    let temp = write_vault(&[
        ("index.md", "[license](LICENSE)\n"),
        ("LICENSE", ""),
        ("report/keep", ""),
    ]);
    Command::cargo_bin("downlint")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "."])
        .assert()
        .success();
    Command::cargo_bin("downlint")
        .unwrap()
        .args(["check", "--stdin", "--root"])
        .arg(temp.path())
        .write_stdin("[license](LICENSE)\n")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    Command::cargo_bin("downlint")
        .unwrap()
        .args(["check", "--stdin", "--root"])
        .arg(temp.path())
        .write_stdin("[report](./report)\n[missing](missing)\n")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Hint: use './report/'"))
        .stdout(predicate::str::contains("Broken link: 'missing'"));
    std::fs::write(
        temp.path().join("index.md"),
        "[report](./report)\n[missing](missing)\n",
    )
    .unwrap();
    Command::cargo_bin("downlint")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "index.md"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Hint: use './report/'"))
        .stdout(predicate::str::contains("Broken link: 'missing'").not());
}

#[test]
fn wiki_query_requires_directory_slashes_without_bare_file_fallback() {
    let temp = write_vault(&[("index.md", ""), ("report/keep", ""), ("topic", "")]);
    for target in ["./report", "/report", "./report#section"] {
        Command::cargo_bin("downlint")
            .unwrap()
            .args(["link", "resolve", "--root"])
            .arg(temp.path())
            .arg(target)
            .assert()
            .code(1)
            .stdout(predicate::str::contains(
                "directory links require a trailing '/'",
            ))
            .stdout(predicate::str::contains("broken — 0 destinations"));
    }
    let json = Command::cargo_bin("downlint")
        .unwrap()
        .args(["link", "resolve", "--root"])
        .arg(temp.path())
        .args(["./report", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(json["status"], "broken");
    assert!(json["destinations"].as_array().unwrap().is_empty());
    assert!(
        json["directory_hint"]
            .as_str()
            .unwrap()
            .contains("'./report/'")
    );
    Command::cargo_bin("downlint")
        .unwrap()
        .args(["link", "resolve", "--root"])
        .arg(temp.path())
        .arg("./report/")
        .assert()
        .success()
        .stdout(predicate::str::contains("[directory]"));
    Command::cargo_bin("downlint")
        .unwrap()
        .args(["link", "resolve", "--root"])
        .arg(temp.path())
        .arg("topic")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("directory links").not());
}

#[cfg(unix)]
#[test]
fn symlink_metadata_distinguishes_files_directories_and_missing_targets() {
    let temp = write_vault(&[
        (
            "index.md",
            "[report](report)\n[report](topic)\n[missing](missing)\n",
        ),
        ("image.png", ""),
        ("docs/keep", ""),
    ]);
    std::os::unix::fs::symlink("image.png", temp.path().join("report")).unwrap();
    std::os::unix::fs::symlink("docs", temp.path().join("topic")).unwrap();
    std::os::unix::fs::symlink("missing.png", temp.path().join("missing")).unwrap();
    let g = graph(temp.path());
    assert_eq!(g.resolved_references.len(), 1);
    assert_eq!(g.unresolved_references.len(), 2);
    assert_eq!(
        g.unresolved_references
            .iter()
            .filter(|r| r.directory_hint.is_some())
            .count(),
        1
    );
}
