//! CLI subprocess tests for `downlint graph` — the read-only link-graph
//! queries (RFC 0015).
//!
//! `graph` projects the existing `ConnectionGraph` into navigation reports:
//! `backlinks`, `links`, `orphans`, `deadends`, `unresolved`. Exit codes:
//! per-file commands (`backlinks`/`links`) are `0` = file in index, `1` = file
//! not in index, `2` = bad args/config. Report commands (`orphans`/`deadends`/
//! `unresolved`) are `0` = none found, `1` = found, `2` = bad args/config.
//!
//! Uses `assert_cmd` to drive the compiled binary directly, mirroring
//! `cli_resolve_tests.rs`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn downlint() -> Command {
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

/// Run `downlint graph <args...> --root <root>`.
fn graph(root: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut full: Vec<String> = vec!["graph".into()];
    for arg in args {
        full.push(arg.to_string());
    }
    full.push("--root".into());
    full.push(root.to_str().unwrap().to_string());
    downlint().args(full).assert()
}

/// Two notes link to `target` (one plain, one heading-anchored) → both listed.
#[test]
fn backlinks_lists_sources_including_heading_anchored() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n## Heading\nbody\n"),
        ("notes/a.md", "# A\n- [[Target]]\n"),
        ("notes/b.md", "# B\n- [[Target#Heading]]\n"),
    ]);
    graph(
        temp.path(),
        &["backlinks", "notes/target.md"],
    )
    .success()
    .stdout(predicate::str::contains("notes/a.md:2"))
    .stdout(predicate::str::contains("notes/b.md:2"));
}

/// A note with no incoming links → empty output, exit 0.
#[test]
fn backlinks_no_incoming_is_empty_success() {
    let temp = write_vault(&[("notes/lonely.md", "# Lonely\n")]);
    graph(temp.path(), &["backlinks", "notes/lonely.md"])
        .success()
        .stdout(predicate::str::is_empty());
}

/// `<FILE>` not in the index → exit 1.
#[test]
fn backlinks_file_not_in_index_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    graph(temp.path(), &["backlinks", "notes/nope.md"])
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not found"));
}

/// `links` shows resolved, unresolved, and attachment references with status.
#[test]
fn links_shows_all_outgoing_with_status() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n"),
        ("notes/img.png", ""),
        (
            "notes/a.md",
            "# A\n- [[Target]]\n- [x](missing.md)\n- [pic](img.png)\n",
        ),
    ]);
    graph(temp.path(), &["links", "notes/a.md"])
        .success()
        .stdout(predicate::str::contains("Target"))
        .stdout(predicate::str::contains("notes/target.md"))
        .stdout(predicate::str::contains("missing.md"))
        .stdout(predicate::str::contains("<unresolved>"))
        .stdout(predicate::str::contains("img.png"))
        .stdout(predicate::str::contains("notes/img.png"));
}

/// `links` on a file not in the index → exit 1.
#[test]
fn links_file_not_in_index_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    graph(temp.path(), &["links", "notes/nope.md"])
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not found"));
}

/// Notes with no incoming links are listed; the hub (which links out) is an
/// orphan because nothing links to it.
#[test]
fn orphans_lists_unlinked_notes() {
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Alpha]]\n- [[Beta]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
        ("notes/beta.md", "# Beta\n"),
        ("notes/lonely.md", "# Lonely\n"),
    ]);
    graph(temp.path(), &["orphans"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("notes/hub.md"))
        .stdout(predicate::str::contains("notes/lonely.md"))
        .stdout(predicate::str::contains("notes/alpha.md").not())
        .stdout(predicate::str::contains("notes/beta.md").not());
}

/// A self-link counts as an incoming edge → not an orphan → exit 0.
#[test]
fn orphans_self_link_is_not_an_orphan() {
    let temp = write_vault(&[("notes/self.md", "# Self\n- [[Self]]\n")]);
    graph(temp.path(), &["orphans"]).success().stdout(
        predicate::str::is_empty(),
    );
}

/// Notes with no outgoing document links are listed; the hub is not.
#[test]
fn deadends_lists_notes_without_outgoing_links() {
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Alpha]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
    ]);
    graph(temp.path(), &["deadends"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("notes/alpha.md"))
        .stdout(predicate::str::contains("notes/hub.md").not());
}

/// Broken links are listed as `source:line:col  target`; exit 1.
#[test]
fn unresolved_lists_broken_links() {
    let temp = write_vault(&[(
        "notes/a.md",
        "# A\n- [[Gone]]\n- [x](missing.md)\n",
    )]);
    graph(temp.path(), &["unresolved"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("notes/a.md:2"))
        .stdout(predicate::str::contains("Gone"))
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
    graph(temp.path(), &["unresolved"])
        .success()
        .stdout(predicate::str::is_empty());
}

/// A `lint = false` mount's links are visible (complete graph): the mounted
/// note's link to a primary note is reported as a backlink.
#[test]
fn backlinks_sees_lint_false_mount_links() {
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

    graph(&vault, &["backlinks", "notes/a.md"])
        .success()
        .stdout(predicate::str::contains("ext/doc.md:2"));
}

/// No subcommand → clap error, exit 2.
#[test]
fn graph_without_subcommand_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    graph(temp.path(), &[]).failure().code(2);
}

/// `backlinks` with no `<FILE>` → clap error, exit 2.
#[test]
fn backlinks_without_file_fails() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    graph(temp.path(), &["backlinks"]).failure().code(2);
}

/// `--format json`: backlinks emits an envelope with `query`, `file`, and
/// `results[]` of `{source, line, col}`; exit code matches text mode.
#[test]
fn backlinks_json_envelope() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n"),
        ("notes/a.md", "# A\n- [[Target]]\n"),
    ]);
    graph(
        temp.path(),
        &["backlinks", "notes/target.md", "--format", "json"],
    )
    .success()
    .stdout(predicate::str::contains("\"query\": \"backlinks\""))
    .stdout(predicate::str::contains("\"file\": \"notes/target.md\""))
    .stdout(predicate::str::contains("\"results\""))
    .stdout(predicate::str::contains("\"source\": \"notes/a.md\""))
    .stdout(predicate::str::contains("\"line\": 2"));
}

/// `--format json`: links emits `status` + `destination` (null when unresolved).
#[test]
fn links_json_status_and_destination() {
    let temp = write_vault(&[
        ("notes/target.md", "# Target\n"),
        ("notes/a.md", "# A\n- [[Target]]\n- [x](missing.md)\n"),
    ]);
    graph(
        temp.path(),
        &["links", "notes/a.md", "--format", "json"],
    )
    .success()
    .stdout(predicate::str::contains("\"query\": \"links\""))
    .stdout(predicate::str::contains("\"status\": \"resolved\""))
    .stdout(predicate::str::contains("\"destination\": \"notes/target.md\""))
    .stdout(predicate::str::contains("\"status\": \"unresolved\""))
    .stdout(predicate::str::contains("\"destination\": null"));
}

/// `--format json`: orphans emits `results[]` of `{path}`; exit 1 when found.
#[test]
fn orphans_json_envelope() {
    let temp = write_vault(&[
        ("notes/hub.md", "# Hub\n- [[Alpha]]\n"),
        ("notes/alpha.md", "# Alpha\n"),
    ]);
    graph(temp.path(), &["orphans", "--format", "json"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"query\": \"orphans\""))
        .stdout(predicate::str::contains("\"path\": \"notes/hub.md\""))
        .stdout(predicate::str::contains("\"file\"").not());
}

/// `--format json`: unresolved emits `{source, line, col, target}`.
#[test]
fn unresolved_json_envelope() {
    let temp = write_vault(&[("notes/a.md", "# A\n- [[Gone]]\n")]);
    graph(temp.path(), &["unresolved", "--format", "json"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"query\": \"unresolved\""))
        .stdout(predicate::str::contains("\"source\": \"notes/a.md\""))
        .stdout(predicate::str::contains("\"target\": \"Gone\""));
}

/// `--format json` is valid JSON (parses) and an empty result is `[]`.
#[test]
fn json_output_is_valid_and_empty_is_empty_array() {
    let temp = write_vault(&[("notes/a.md", "# A\n- [[B]]\n"), ("notes/b.md", "# B\n")]);
    let root = temp.path().to_str().unwrap().to_string();
    let assert = downlint()
        .args([
            "graph",
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
