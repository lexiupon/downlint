//! CLI subprocess tests for `downlint file rename` and `downlint link rename`
//! — RFC 0009 (subcommands renamed by RFC 0018).
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

/// `file rename` rewrites the on-disk file and every referencing
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
            "file",
            "rename",
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
            "file",
            "rename",
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

/// `file rename` for a missing source returns exit code 3.
#[test]
fn cli_rename_file_source_missing() {
    let temp = write_vault(&[("index.md", "# Index\n")]);
    let root = temp.path();
    downlint()
        .args([
            "file",
            "rename",
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

/// `file rename` for an attachment (non-markdown extension) works — kind
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
            "file",
            "rename",
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

/// `file rename` rejects extension-class changes (markdown → attachment)
/// with exit code 2 (Conflict).
#[test]
fn cli_rename_file_extension_class_rejected() {
    let temp = write_vault(&[("report.md", "# Report\n")]);
    let root = temp.path();
    downlint()
        .args([
            "file",
            "rename",
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

/// `link rename --from report --to topic` rewrites the link target
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
            "link",
            "rename",
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

/// `link rename --dry-run` prints the plan without touching disk.
#[test]
fn cli_rename_link_dry_run() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .args([
            "link",
            "rename",
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

/// `link rename` exit code 2 when --to collides with an existing file.
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
            "link",
            "rename",
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

/// `link rename --from "path/to/x"` (containing a slash) is rejected by
/// the bare-identifier validation.
#[test]
fn cli_rename_link_validates_bare_identifier() {
    let temp = write_vault(&[("index.md", "# Index\n")]);
    let root = temp.path();
    downlint()
        .args([
            "link",
            "rename",
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

/// Relative `--root` (e.g. `--root .`) must behave exactly like an absolute
/// root: the link rewrite is applied, not silently skipped while the file
/// still moves. Regression test — with a relative root the planner's
/// absolutized source path used to mismatch the graph's relative
/// destination paths, so no text edits were produced (exit 0, stale links).
#[test]
fn cli_rename_file_relative_root_rewrites_links() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .current_dir(root)
        .args([
            "file",
            "rename",
            "--root",
            ".",
            "--from",
            "report.md",
            "--to",
            "topic.md",
        ])
        .assert()
        .success();

    assert!(!root.join("report.md").exists(), "report.md should have moved");
    assert!(root.join("topic.md").exists(), "topic.md should exist");
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(
        index_content.contains("[[topic]]") && !index_content.contains("[[report]]"),
        "index.md should reference topic, got: {index_content}"
    );
}

/// Relative `--root` on `link rename`: the identifier rewrite is applied.
/// Pins the same root-consistency guarantee for the identifier path.
#[test]
fn cli_rename_link_relative_root_rewrites_identifiers() {
    let temp = write_vault(&[
        ("old-id.md", "# Old Id\n"),
        ("index.md", "see [[old-id]]\n"),
    ]);
    let root = temp.path();
    downlint()
        .current_dir(root)
        .args([
            "link",
            "rename",
            "--root",
            ".",
            "--from",
            "old-id",
            "--to",
            "new-id",
        ])
        .assert()
        .success();

    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(
        index_content.contains("[[new-id]]") && !index_content.contains("[[old-id]]"),
        "index.md should reference new-id, got: {index_content}"
    );
}

/// RFC 0019: `link rename --from report.md` (path-like) still succeeds —
/// stem-stripping is lenient by design — but prints an advisory note
/// suggesting `file rename`.
#[test]
fn cli_rename_link_path_like_from_prints_note() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    let assert = downlint()
        .args([
            "link",
            "rename",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report.md",
            "--to",
            "topic",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("looks like a file path"), "{stderr}");
    assert!(stderr.contains("file rename"), "{stderr}");
    // The rename still applied (lenient behavior preserved).
    let index_content = fs::read_to_string(root.join("index.md")).unwrap();
    assert!(index_content.contains("[[topic]]"), "{index_content}");
}

/// RFC 0019: `link rename --from report` (bare identifier, no extension)
/// prints no note.
#[test]
fn cli_rename_link_bare_from_prints_no_note() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    let assert = downlint()
        .args([
            "link",
            "rename",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report",
            "--to",
            "topic",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(!stderr.contains("looks like a file path"), "{stderr}");
}

/// RFC 0019: `file rename --from report` (no file; `report.md` exists) →
/// exit 3 with a cross-hint suggesting `link rename`.
#[test]
fn cli_rename_file_missing_source_hints_link_rename() {
    let temp = write_vault(&[
        ("report.md", "# Report\n"),
        ("index.md", "see [[report]]\n"),
    ]);
    let root = temp.path();
    let assert = downlint()
        .args([
            "file",
            "rename",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "report",
            "--to",
            "topic",
        ])
        .assert()
        .code(3);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("source file not found"), "{stderr}");
    assert!(stderr.contains("hint:"), "{stderr}");
    assert!(stderr.contains("link rename --from report"), "{stderr}");
}

/// RFC 0019: `file rename --from nonexistent` (no document stem match) →
/// exit 3, no hint.
#[test]
fn cli_rename_file_missing_source_no_match_no_hint() {
    let temp = write_vault(&[("report.md", "# Report\n")]);
    let root = temp.path();
    let assert = downlint()
        .args([
            "file",
            "rename",
            "--root",
            root.to_str().unwrap(),
            "--from",
            "nonexistent",
            "--to",
            "topic",
        ])
        .assert()
        .code(3);
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("source file not found"), "{stderr}");
    assert!(!stderr.contains("hint:"), "{stderr}");
}

/// RFC 0019: each rename command's `--help` names its sibling.
#[test]
fn rename_help_lines_mention_sibling() {
    let file_help = downlint()
        .args(["file", "rename", "--help"])
        .assert()
        .success();
    let file_stdout = String::from_utf8(file_help.get_output().stdout.clone()).unwrap();
    assert!(file_stdout.contains("link rename"), "{file_stdout}");

    let link_help = downlint()
        .args(["link", "rename", "--help"])
        .assert()
        .success();
    let link_stdout = String::from_utf8(link_help.get_output().stdout.clone()).unwrap();
    assert!(link_stdout.contains("file rename"), "{link_stdout}");
}