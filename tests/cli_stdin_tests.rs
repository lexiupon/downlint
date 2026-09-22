//! CLI subprocess tests for `downlint check --stdin` — workspace-anchored
//! stdin checks.
//!
//! Stdin input is linted as a synthetic `<stdin>.md` document at the
//! workspace root, resolved against the full workspace (documents,
//! attachments, folder links, anchors). Workspace documents are indexed as
//! targets only: their own diagnostics do not surface in a stdin check.
//!
//! Uses `assert_cmd` to drive the compiled binary directly, mirroring
//! `cli_rename_tests.rs`.

use assert_cmd::Command;
use predicates::prelude::*;
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

/// Pipe a document to `downlint check --stdin --root <root>` and assert.
fn check_stdin(root: &std::path::Path, stdin: &str) -> assert_cmd::assert::Assert {
    downlint()
        .args(["check", "--root", root.to_str().unwrap(), "--stdin"])
        .write_stdin(stdin)
        .assert()
}

/// A workspace-absolute wiki link to an existing attachment (with spaces in
/// the filename) resolves cleanly: exit 0, no output.
#[test]
fn cli_stdin_wiki_link_to_existing_attachment_resolves() {
    let temp = write_vault(&[(
        "assets/20260523-us-short-code-lifecycle/us short codes.drawio",
        "",
    )]);
    check_stdin(
        temp.path(),
        "- [[/assets/20260523-us-short-code-lifecycle/us short codes.drawio]]\n",
    )
    .success()
    .stdout(predicate::str::is_empty());
}

/// The same link with the attachment missing is `link/broken` on
/// `<stdin>.md`: exit 1.
#[test]
fn cli_stdin_wiki_link_to_missing_attachment_is_broken() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    check_stdin(
        temp.path(),
        "- [[/assets/20260523-us-short-code-lifecycle/does-not-exist.drawio]]\n",
    )
    .failure()
    .stdout(predicate::str::contains(
        "<stdin>.md:1:5: error: Broken link: '/assets/20260523-us-short-code-lifecycle/does-not-exist.drawio' could not be resolved [link/broken]",
    ));
}

/// A wiki link to an existing workspace note resolves: exit 0, no output.
#[test]
fn cli_stdin_wiki_link_to_existing_note_resolves() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    check_stdin(temp.path(), "- [[note]]\n")
        .success()
        .stdout(predicate::str::is_empty());
}

/// A wiki link to a missing note is `link/broken`: exit 1.
#[test]
fn cli_stdin_wiki_link_to_missing_note_is_broken() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    check_stdin(temp.path(), "- [[missing-note]]\n")
        .failure()
        .stdout(predicate::str::contains(
            "<stdin>.md:1:5: error: Broken link: 'missing-note' could not be resolved [link/broken]",
        ));
}

/// Workspace documents are targets only in stdin mode: a broken link inside a
/// workspace note does not surface when checking via stdin, but does when
/// checking the directory.
#[test]
fn cli_stdin_workspace_docs_are_targets_only() {
    let temp = write_vault(&[
        ("note.md", "# Note\n"),
        ("broken.md", "# Broken\n- [[/assets/nope.png]]\n"),
    ]);
    // Via stdin: only the piped document is diagnosed.
    check_stdin(temp.path(), "- [[note]]\n")
        .success()
        .stdout(predicate::str::is_empty());
    // Via directory check: the workspace doc's broken link is reported.
    downlint()
        .args(["check", "--root", temp.path().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "broken.md:2:5: error: Broken link: '/assets/nope.png' could not be resolved [link/broken]",
        ));
}

/// In-page anchor misses are still diagnosed in stdin mode.
#[test]
fn cli_stdin_in_page_anchor_miss_is_broken_anchor() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    check_stdin(temp.path(), "# Heading\n- [[#nope]]\n")
        .failure()
        .stdout(predicate::str::contains(
            "<stdin>.md:2:6: warning: Broken anchor: '#nope' could not be resolved [link/broken-anchor]",
        ));
}

/// Folder links are still diagnosed in stdin mode: a missing directory is
/// `link/broken`, an existing directory resolves.
#[test]
fn cli_stdin_folder_links_still_diagnosed() {
    let temp = write_vault(&[("note.md", "# Note\n"), ("assets/keep.txt", "")]);
    check_stdin(temp.path(), "- [[/no-such-folder/]]\n")
        .failure()
        .stdout(predicate::str::contains(
            "<stdin>.md:1:5: error: Broken link: '/no-such-folder/' could not be resolved [link/broken]",
        ));
    check_stdin(temp.path(), "- [[/assets/]]\n")
        .success()
        .stdout(predicate::str::is_empty());
}

/// `heading/nbsp` runs on the piped document in stdin mode.
#[test]
fn cli_stdin_heading_nbsp_in_piped_content_is_diagnosed() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    check_stdin(temp.path(), "#\u{a0}Bad Heading\n")
        .failure()
        .stdout(predicate::str::contains(
            "<stdin>.md:1:2: warning: Non-breaking whitespace after heading marker [heading/nbsp]",
        ));
}

/// `heading/nbsp` in a workspace document does not leak into a stdin check.
#[test]
fn cli_stdin_heading_nbsp_in_workspace_doc_does_not_leak() {
    let temp = write_vault(&[("note.md", "# Note\n"), ("nbsp.md", "#\u{a0}Bad Heading\n")]);
    // The directory check reports the workspace doc's NBSP heading...
    downlint()
        .args(["check", "--root", temp.path().to_str().unwrap()])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "nbsp.md:1:2: warning: Non-breaking whitespace after heading marker [heading/nbsp]",
        ));
    // ...but the stdin check does not surface it.
    check_stdin(temp.path(), "- [[note]]\n")
        .success()
        .stdout(predicate::str::is_empty());
}

/// Explicit single-file mode (a file on disk) still suppresses cross-file
/// `link/broken`: a broken wiki link in the checked file produces no output.
#[test]
fn cli_single_file_on_disk_still_suppresses_cross_file_broken_links() {
    let temp = write_vault(&[
        ("note.md", "# Note\n"),
        ("orphan.md", "# Orphan\n- [[missing-note]]\n"),
    ]);
    downlint()
        .args([
            "check",
            "--root",
            temp.path().to_str().unwrap(),
            temp.path().join("orphan.md").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}
