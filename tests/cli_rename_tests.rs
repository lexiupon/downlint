//! CLI subprocess tests for `downlint rename-file` and `downlint rename-link`
//! — RFC 0009 §"CLI Subcommands".
//!
//! Asserts on stdout/stderr/exit code for the rename subcommands. Uses
//! `assert_cmd` to drive the compiled binary directly.
//!
//! The persistent server (`downlint server --detach`) is a Phase 5 stretch
//! goal and is currently a no-op returning exit code 3. Its tests live
//! alongside when the server module lands.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

fn downlint() -> Command {
    // `assert_cmd::Command::cargo_bin` resolves the dev binary path.
    Command::cargo_bin("downlint").expect("downlint binary should build")
}

fn write_vault(pairs: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    for (rel, content) in pairs {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }
    temp
}

/// `rename-file` rewrites the on-disk file and every referencing
/// document. Verifies the standard happy path.
#[test]
fn cli_rename_file_applies_text_first_then_disk() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-file",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report.md",
            "--to",
            "topic.md",
        ])
        .assert()
        .success();

    // The source file is gone; the destination exists.
    assert!(!root.join("report.md").exists(), "report.md should have moved");
    assert!(root.join("topic.md").exists(), "topic.md should exist");

    // The referencing doc's link was rewritten.
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(
        index_content.contains("[[topic]]") && !index_content.contains("[[report]]"),
        "index.md should reference topic, got: {index_content}"
    );
}

/// `rename-file --dry-run` prints the plan without touching disk.
#[test]
fn cli_rename_file_dry_run() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-file",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report.md",
            "--to",
            "topic.md",
            "--dry-run",
        ])
        .assert()
        .success();

    // Dry-run leaves the disk untouched.
    assert!(root.join("report.md").exists());
    assert!(!root.join("topic.md").exists());
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(index_content.contains("[[report]]"));
}

/// `rename-file` for a missing source returns exit code 3.
#[test]
fn cli_rename_file_source_missing() {
    let temp = write_vault(&[("index.md", "# Index\n")]);
    let root = temp.path();
    downlint()
        .args([
            "rename-file",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "nonexistent.md",
            "--to",
            "topic.md",
        ])
        .assert()
        .code(3);
}

/// `rename-file` for an attachment (non-markdown extension) works — kind
/// is inferred from the extension.
#[test]
fn cli_rename_file_attachment() {
    let temp = write_vault(&[
        ("photo.png", "fake-png-bytes"),
        ("index.md", "see [[photo.png]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-file",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "photo.png",
            "--to",
            "pic.png",
        ])
        .assert()
        .success();

    assert!(!root.join("photo.png").exists());
    assert!(root.join("pic.png").exists());
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(index_content.contains("[[pic.png]]"));
}

/// `rename-file` rejects extension-class changes (markdown → attachment)
/// with exit code 2 (Conflict).
#[test]
fn cli_rename_file_extension_class_rejected() {
    let temp = write_vault(&[("report.md", "# Report\n")]);
    let root = temp.path();
    downlint()
        .args([
            "rename-file",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report.md",
            "--to",
            "report.pdf",
        ])
        .assert()
        .code(2);
}

/// `rename-link --from report --to topic` rewrites the link target
/// strings across the workspace.
#[test]
fn cli_rename_link_rewrites_workspace() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-link",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report",
            "--to",
            "topic",
        ])
        .assert()
        .success();

    // The file stays put; only the link target strings change.
    assert!(root.join("report.md").exists(), "report.md must remain on disk");
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(
        index_content.contains("[[topic]]") && !index_content.contains("[[report]]"),
        "index.md should reference topic, got: {index_content}"
    );
}

/// `rename-link --dry-run` prints the plan without touching disk.
#[test]
fn cli_rename_link_dry_run() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-link",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report",
            "--to",
            "topic",
            "--dry-run",
        ])
        .assert()
        .success();

    // Dry-run leaves the disk untouched.
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(index_content.contains("[[report]]"));
}

/// `rename-link` exit code 2 when --to collides with an existing file.
#[test]
fn cli_rename_link_conflict_exact() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("topic.md", "# Topic\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "rename-link",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report",
            "--to",
            "topic",
        ])
        .assert()
        .code(2);
}

/// `rename-link --from "path/to/x"` (containing a slash) is rejected by
/// the bare-identifier validation.
#[test]
fn cli_rename_link_validates_bare_identifier() {
    let temp = write_vault(&[("index.md", "# Index\n")]);
    let root = temp.path();
    downlint()
        .args([
            "rename-link",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "path/to/x",
            "--to",
            "topic",
        ])
        .assert()
        .code(3);
}

/// `downlint server --detach` is a Phase 5 stretch goal — currently
/// returns exit 3 with a clear message.
#[test]
fn cli_server_detach_not_implemented() {
    downlint()
        .args(["server", "--detach"])
        .current_dir(write_vault(&[("index.md", "# Index\n")]).path())
        .assert()
        .code(3);
}