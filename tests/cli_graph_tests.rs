//! CLI subprocess tests for `downlint link <query>` — the read-only link
//! queries (RFC 0018; formerly the top-level `graph` command, RFC 0015).
//!
//! `link` projects the existing `ConnectionGraph` into navigation reports:
//! `link graph <FILE>` (incoming + outgoing), `link coverage` (orphans +
//! deadends), `link unresolved`. Exit codes: `link graph` is `0` = file in
//! index, `1` = file not in index, `2` = bad args/config. `link coverage`
//! and `link unresolved` are `0` = none found, `1` = found, `2` = bad
//! args/config.
//!
//! Uses `assert_cmd` to drive the compiled binary directly, mirroring
//! `cli_resolve_tests.rs`.

mod common;

use assert_cmd::Command;
use common::write_vault;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn downlint() -> Command {
    Command::cargo_bin("downlint").expect("downlint binary should build")
}


/// Run `downlint link <args...> --root <root>`.
fn link(root: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut full: Vec<String> = vec!["link".into()];
    for arg in args {
        full.push(arg.to_string());
    }
    full.push("--root".into());
    full.push(root.to_str().unwrap().to_string());
    downlint().args(full).assert()
}

/// Extract the (trimmed) lines of a named section from sectioned text
/// output. Section rows are indented; the next section header ends the
/// section.
fn section(stdout: &str, name: &str) -> Vec<String> {
    let lines: Vec<&str> = stdout.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with(&format!("{name} (")))
        .unwrap_or_else(|| panic!("section {name:?} not found in:\n{stdout}"));
    let mut out = Vec::new();
    for line in lines.iter().skip(start + 1) {
        if line.starts_with(' ') {
            out.push(line.trim().to_string());
        } else {
            break;
        }
    }
    out
}

/// Run `link graph <file>` and return the stdout text.
fn graph_text(root: &Path, file: &str) -> (i32, String) {
    let output = downlint()
        .args(["link", "graph", file, "--root", root.to_str().unwrap()])
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

/// Two notes link to `target` (one plain, one heading-anchored) → both listed
/// in the incoming section.
#[test]
fn graph_lists_incoming_including_heading_anchored() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n## Heading\nbody\n"),
        ("notes/a.md", "# A\n- [[Target]]\n"),
        ("notes/b.md", "# B\n- [[Target#Heading]]\n"),
    ]);
    let (code, stdout) = graph_text(temp.path(), "notes/target.md");
    assert_eq!(code, 0);
    let incoming = section(&stdout, "incoming");
    assert!(incoming.iter().any(|line| line.starts_with("notes/a.md:2")), "{incoming:?}");
    assert!(incoming.iter().any(|line| line.starts_with("notes/b.md:2")), "{incoming:?}");
}

/// A note with no incoming or outgoing links → both sections render with a
/// count of 0, exit 0.
#[test]
fn graph_no_links_renders_empty_sections() {
    let temp = write_vault(&[("notes/lonely.md", "# Lonely\n")]);
    let (code, stdout) = graph_text(temp.path(), "notes/lonely.md");
    assert_eq!(code, 0);
    assert!(stdout.contains("incoming (0)"), "{stdout}");
    assert!(stdout.contains("outgoing (0)"), "{stdout}");
}

/// `<FILE>` not in the index → exit 1.
#[test]
fn graph_file_not_in_index_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    link(temp.path(), &["graph", "notes/nope.md"])
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not found"));
}

/// The outgoing section shows resolved, unresolved, and attachment references
/// with status.
#[test]
fn graph_shows_all_outgoing_with_status() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n"),
        ("notes/image.png", ""),
        (
            "notes/a.md",
            "# A\n- [[Target]]\n- [x](missing.md)\n- [pic](image.png)\n",
        ),
    ]);
    let (code, stdout) = graph_text(temp.path(), "notes/a.md");
    assert_eq!(code, 0);
    let outgoing = section(&stdout, "outgoing");
    assert!(outgoing.iter().any(|line| line.contains("Target") && line.contains("notes/target.md")), "{outgoing:?}");
    assert!(outgoing.iter().any(|line| line.contains("missing.md") && line.contains("<unresolved>")), "{outgoing:?}");
    assert!(outgoing.iter().any(|line| line.contains("image.png") && line.contains("notes/image.png")), "{outgoing:?}");
}

/// Notes with no incoming links are listed under orphans; the hub (which
/// links out) is an orphan because nothing links to it. Notes with no
/// outgoing links are listed under deadends.
#[test]
fn coverage_lists_orphans_and_deadends() {
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Alpha]]\n- [[Beta]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
        ("notes/beta.md", "# Beta\n"),
        ("notes/lonely.md", "# Lonely\n"),
    ]);
    let assert = link(temp.path(), &["coverage"])
        .failure()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let orphans = section(&stdout, "orphans");
    let deadends = section(&stdout, "deadends");
    assert!(orphans.contains(&"notes/hub.md".into()), "{orphans:?}");
    assert!(orphans.contains(&"notes/lonely.md".into()), "{orphans:?}");
    assert!(!orphans.contains(&"notes/alpha.md".into()), "{orphans:?}");
    assert!(!orphans.contains(&"notes/beta.md".into()), "{orphans:?}");
    assert!(deadends.contains(&"notes/alpha.md".into()), "{deadends:?}");
    assert!(deadends.contains(&"notes/beta.md".into()), "{deadends:?}");
    assert!(deadends.contains(&"notes/lonely.md".into()), "{deadends:?}");
    assert!(!deadends.contains(&"notes/hub.md".into()), "{deadends:?}");
}

/// A self-link counts as an incoming and outgoing edge → not an orphan, not
/// a deadend → both sections empty → exit 0.
#[test]
fn coverage_self_link_is_clean() {
    let temp = write_vault(&[("notes/self.md", "# Self\n- [[Self]]\n")]);
    let assert = link(temp.path(), &["coverage"]).success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("orphans (0)"), "{stdout}");
    assert!(stdout.contains("deadends (0)"), "{stdout}");
}

/// Exit 1 when only deadends are non-empty (no orphans).
#[test]
fn coverage_deadends_only_is_exit_1() {
    // hub self-links (so it is not an orphan) and links to alpha; alpha is
    // linked (not an orphan) but has no outgoing links (deadend).
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Hub]]\n- [[Alpha]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
    ]);
    let assert = link(temp.path(), &["coverage"]).failure().code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert_eq!(section(&stdout, "orphans"), Vec::<String>::new());
    assert_eq!(section(&stdout, "deadends"), vec!["notes/alpha.md"]);
}

/// Broken links are listed as `source:line:col  target`; exit 1.
#[test]
fn unresolved_lists_broken_links() {
    let temp = write_vault(&[(
        "notes/a.md",
        "# A\n- [[missing]]\n- [x](missing.md)\n",
    )]);
    link(temp.path(), &["unresolved"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("notes/a.md:2"))
        .stdout(predicate::str::contains("missing"))
        .stdout(predicate::str::contains("notes/a.md:3"))
        .stdout(predicate::str::contains("missing.md"));
}

/// A clean vault → `unresolved` is empty, exit 0.
#[test]
fn unresolved_clean_vault_is_empty_success() {
    let temp = write_vault(&[
        ("notes/a.md", "# A\n- [[B]]\n"),
        ("notes/b.md", "# B\n"),
    ]);
    link(temp.path(), &["unresolved"])
        .success()
        .stdout(predicate::str::is_empty());
}

/// A `lint = false` mount's links are visible (complete graph): the mounted
/// note's link to a primary note is reported in the incoming section.
#[test]
fn graph_sees_lint_false_mount_links() {
    let temp = TempDir::new().unwrap();
    let vault = temp.path().join("vault");
    let ext = temp.path().join("ext"); // outside the vault root
    fs::create_dir_all(vault.join("notes")).unwrap();
    fs::create_dir_all(&ext).unwrap();
    fs::write(vault.join("notes/a.md"), "# A\n").unwrap();
    fs::write(ext.join("doc.md"), "# Doc\n- [[A]]\n").unwrap();
    let config = format!(
        "[[mounts]]\npath = \"{}\"\nas = \"/ext\"\nlint = false\n",
        ext.to_str().unwrap()
    );
    fs::write(vault.join(".downlint.toml"), config).unwrap();

    link(&vault, &["graph", "notes/a.md"])
        .success()
        .stdout(predicate::str::contains("ext/doc.md:2"));
}

/// No subcommand → clap error, exit 2.
#[test]
fn link_without_subcommand_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    link(temp.path(), &[]).failure().code(2);
}

/// `link graph` with no `<FILE>` → clap error, exit 2.
#[test]
fn graph_without_file_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    link(temp.path(), &["graph"]).failure().code(2);
}

/// `--format json`: `link graph` emits an envelope with `query`, `file`,
/// `incoming[]` of `{source, line, col}`, and `outgoing[]`; exit code
/// matches text mode.
#[test]
fn graph_json_envelope() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n"),
        ("notes/a.md", "# A\n- [[Target]]\n- [x](missing.md)\n"),
        ("notes/c.md", "# C\n- [[A]]\n"),
    ]);
    link(
        temp.path(),
        &["graph", "notes/a.md", "--format", "json"],
    )
    .success()
    .stdout(predicate::str::contains("\"query\": \"graph\""))
    .stdout(predicate::str::contains("\"file\": \"notes/a.md\""))
    .stdout(predicate::str::contains("\"incoming\""))
    .stdout(predicate::str::contains("\"outgoing\""))
    .stdout(predicate::str::contains("\"source\": \"notes/c.md\""))
    .stdout(predicate::str::contains("\"status\": \"resolved\""))
    .stdout(predicate::str::contains("\"destination\": \"notes/target.md\""))
    .stdout(predicate::str::contains("\"status\": \"unresolved\""))
    .stdout(predicate::str::contains("\"destination\": null"));
}

/// `--format json`: `link coverage` emits `orphans[]` and `deadends[]` of
/// `{path}`; exit 1 when found; no `file` key.
#[test]
fn coverage_json_envelope() {
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Alpha]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
    ]);
    link(temp.path(), &["coverage", "--format", "json"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"query\": \"coverage\""))
        .stdout(predicate::str::contains("\"orphans\""))
        .stdout(predicate::str::contains("\"deadends\""))
        .stdout(predicate::str::contains("\"path\": \"notes/hub.md\""))
        .stdout(predicate::str::contains("\"file\"").not());
}

/// `--format json`: `link unresolved` emits `{source, line, col, target}`.
#[test]
fn unresolved_json_envelope() {
    let temp = write_vault(&[("notes/a.md", "# A\n- [[missing]]\n")]);
    link(temp.path(), &["unresolved", "--format", "json"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"query\": \"unresolved\""))
        .stdout(predicate::str::contains("\"source\": \"notes/a.md\""))
        .stdout(predicate::str::contains("\"target\": \"missing\""));
}

/// `--format json` is valid JSON (parses) and an empty result is `[]`.
#[test]
fn json_output_is_valid_and_empty_is_empty_array() {
    let temp = write_vault(&[("notes/a.md", "# A\n- [[B]]\n"), ("notes/b.md", "# B\n")]);
    let root = temp.path().to_str().unwrap().to_string();
    let assert = downlint()
        .args([
            "link",
            "unresolved",
            "--format",
            "json",
            "--root",
            root.as_str(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"results\": []"));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["query"], "unresolved");
    assert!(parsed["results"].as_array().unwrap().is_empty());
}

/// `--format json`: `link graph` with no links at all → both arrays are `[]`.
#[test]
fn graph_json_empty_sections_are_empty_arrays() {
    let temp = write_vault(&[("notes/lonely.md", "# Lonely\n")]);
    let root = temp.path().to_str().unwrap().to_string();
    let assert = downlint()
        .args([
            "link",
            "graph",
            "notes/lonely.md",
            "--format",
            "json",
            "--root",
            root.as_str(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["query"], "graph");
    assert!(parsed["incoming"].as_array().unwrap().is_empty());
    assert!(parsed["outgoing"].as_array().unwrap().is_empty());
}
