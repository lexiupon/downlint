//! Public CLI contract for RFC 0024 attachment moves.
mod common;

use assert_cmd::Command;
use common::write_vault;
use predicates::prelude::*;

#[test]
fn cli_attachment_move_rewrites_definition_only_and_leaves_note_links() {
    let temp = write_vault(&[
        ("report", "plain file"),
        ("report.md", "# Report\n"),
        ("index.md", "[report](report)\n[[report]]\n"),
        ("docs/index.md", "[report][ref]\n[ref]: ../report#section\n"),
    ]);
    Command::cargo_bin("downlint")
        .unwrap()
        .current_dir(temp.path())
        .args(["file", "rename", "--from", "report", "--to", "topic"])
        .assert()
        .success();
    assert!(!temp.path().join("report").exists());
    assert!(temp.path().join("topic").is_file());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("index.md")).unwrap(),
        "[report](topic)\n[[report]]\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("docs/index.md")).unwrap(),
        "[report][ref]\n[ref]: ../topic#section\n"
    );
    Command::cargo_bin("downlint")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "."])
        .assert()
        .success();
}

#[test]
fn cli_unsafe_wiki_attachment_destination_refuses_without_changes() {
    let temp = write_vault(&[
        ("report", "plain file"),
        ("topic.md", "# Topic\n"),
        ("index.md", "[[./report]]\n"),
    ]);
    Command::cargo_bin("downlint")
        .unwrap()
        .current_dir(temp.path())
        .args(["file", "rename", "--from", "report", "--to", "topic"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot preserve attachment destination",
        ));
    assert!(temp.path().join("report").is_file());
    assert!(!temp.path().join("topic").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("index.md")).unwrap(),
        "[[./report]]\n"
    );
}
