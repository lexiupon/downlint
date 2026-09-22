//! CLI subprocess tests for `downlint resolve` — the target resolution query
//! (RFC 0012).
//!
//! `resolve` reports every destination a link target resolves to, with the
//! reason each matched. It uses the existing matching rules (RES-03/04/05/06/07)
//! and predicts `check`'s behavior. Exit codes: `0` = exactly one destination
//! (or external / mapped-present), `1` = none or multiple (or unmapped /
//! mapped-missing / mapped-placeholder), `2` = bad arguments / config error.
//!
//! Uses `assert_cmd` to drive the compiled binary directly, mirroring
//! `cli_stdin_tests.rs`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
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

/// Run `downlint resolve <target> --root <root> [extra args]`.
fn resolve(root: &Path, target: &str, extra_args: &[&str]) -> assert_cmd::assert::Assert {
    let mut args: Vec<String> = vec![
        "resolve".into(),
        "--root".into(),
        root.to_str().unwrap().to_string(),
        target.to_string(),
    ];
    for arg in extra_args {
        args.push(arg.to_string());
    }
    downlint().args(args).assert()
}

/// One doc, stem match → exit 0, status `resolved`.
#[test]
fn resolve_unique_stem_resolves() {
    let temp = write_vault(&[("notes/avon.md", "# Something Else\n")]);
    resolve(temp.path(), "avon", &[])
        .success()
        .stdout(predicate::str::contains("resolved — 1 destination:"))
        .stdout(predicate::str::contains("notes/avon.md"))
        .stdout(predicate::str::contains("[stem]"));
}

/// Target matches only via H1 slug → exit 0.
#[test]
fn resolve_unique_title_resolves() {
    let temp = write_vault(&[("notes/whatever.md", "# Avon\n")]);
    resolve(temp.path(), "Avon", &[])
        .success()
        .stdout(predicate::str::contains("resolved — 1 destination:"))
        .stdout(predicate::str::contains("[title]"));
}

/// One doc matching by both stem and title → one destination, kinds unioned.
#[test]
fn resolve_stem_and_title_union_kinds() {
    let temp = write_vault(&[("avon.md", "# Avon\n")]);
    resolve(temp.path(), "Avon", &[])
        .success()
        .stdout(predicate::str::contains("[stem, title]"));
}

/// Two docs, same title → exit 1, both listed.
#[test]
fn resolve_ambiguous_multiple_destinations() {
    let temp = write_vault(&[
        ("avon-customer.md", "# Avon\n"),
        ("avon-vendor.md", "# Avon\n"),
    ]);
    resolve(temp.path(), "Avon", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("ambiguous — 2 destinations:"))
        .stdout(predicate::str::contains("avon-customer.md"))
        .stdout(predicate::str::contains("avon-vendor.md"));
}

/// No match → exit 1, status `broken`.
#[test]
fn resolve_broken_no_destination() {
    let temp = write_vault(&[("avon.md", "# Avon\n")]);
    resolve(temp.path(), "nope", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"));
}

/// Explicit workspace-absolute path → kind `path`.
#[test]
fn resolve_explicit_path_document() {
    let temp = write_vault(&[("notes/avon.md", "# Avon\n")]);
    resolve(temp.path(), "/notes/avon.md", &[])
        .success()
        .stdout(predicate::str::contains("[path]"));
}

/// Explicit file-like target, attachment exists → kind `attachment` (RES-06).
#[test]
fn resolve_explicit_path_attachment_present() {
    let temp = write_vault(&[(
        "assets/20260523-us-short-code-lifecycle/us short codes.drawio",
        "",
    )]);
    resolve(
        temp.path(),
        "/assets/20260523-us-short-code-lifecycle/us short codes.drawio",
        &[],
    )
    .success()
    .stdout(predicate::str::contains("[attachment]"));
}

/// Explicit file-like target, attachment missing → `broken`.
#[test]
fn resolve_explicit_path_attachment_missing() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    resolve(temp.path(), "/assets/does-not-exist.drawio", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"));
}

/// Folder target, directory exists → kind `directory` (RES-04).
#[test]
fn resolve_folder_target_existing() {
    let temp = write_vault(&[("notes/avon.md", "# Avon\n")]);
    resolve(temp.path(), "notes/", &[])
        .success()
        .stdout(predicate::str::contains("[directory]"));
}

/// Folder target, directory missing → `broken`.
#[test]
fn resolve_folder_target_missing() {
    let temp = write_vault(&[("notes/avon.md", "# Avon\n")]);
    resolve(temp.path(), "no-such-folder/", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"));
}

/// `Avon#onboarding` → per-destination anchor flags; exit code unchanged by
/// the anchor (advisory).
#[test]
fn resolve_anchor_reports_per_destination() {
    let temp = write_vault(&[
        ("avon-a.md", "# Avon\n## Onboarding\n"),
        ("avon-b.md", "# Avon\n## Other\n"),
    ]);
    resolve(temp.path(), "Avon#onboarding", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("ambiguous — 2 destinations:"))
        .stdout(predicate::str::contains("anchor: yes"))
        .stdout(predicate::str::contains("anchor: no"));
}

/// A mounted doc is listed with mount attribution.
#[test]
fn resolve_mount_attribution() {
    let vault = write_vault(&[("note.md", "# Note\n")]);
    let mount = TempDir::new().unwrap();
    fs::write(mount.path().join("avon-onboarding.md"), "# Avon\n").unwrap();
    // Mount root outside the vault (an in-vault folder would be double-indexed
    // as primary + mounted).
    fs::write(
        vault.path().join(".downlint.toml"),
        format!("[[mounts]]\nroot = \"{}\"\n", mount.path().display()),
    )
    .unwrap();
    resolve(vault.path(), "Avon", &[])
        .success()
        .stdout(predicate::str::contains("(mount: "));
}

/// `--from notes/a.md` + `../shared/b.md` normalizes to `shared/b.md` and
/// resolves as a **document** (RFC 0013 — dot-relative links match the document,
/// not the attachment fallback); without `--from` the same target is `broken`
/// (relative against the root, outside the workspace).
#[test]
fn resolve_from_relative_context() {
    let temp = write_vault(&[("notes/a.md", "# A\n"), ("shared/b.md", "# B\n")]);
    resolve(temp.path(), "../shared/b.md", &["--from", "notes/a.md"])
        .success()
        .stdout(predicate::str::contains("resolved — 1 destination:"))
        .stdout(predicate::str::contains("[path]"));
    resolve(temp.path(), "../shared/b.md", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"));
}

/// `--from` pointing at a nonexistent doc → exit 2.
#[test]
fn resolve_from_missing_doc_is_error() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    resolve(temp.path(), "b", &["--from", "notes/missing.md"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "--from document not found in workspace",
        ));
}

/// A target consisting only of `#anchor` → exit 2.
#[test]
fn resolve_anchor_only_target_is_error() {
    let temp = write_vault(&[("notes/a.md", "# A\n")]);
    resolve(temp.path(), "#onboarding", &[])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("in-page anchor"));
}

/// Folder target with an anchor → `broken` (RES-04: folder links do not take
/// headings).
#[test]
fn resolve_folder_with_anchor_is_broken() {
    let temp = write_vault(&[("notes/avon.md", "# Avon\n")]);
    resolve(temp.path(), "notes/#h", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"));
}

/// A title containing `/` matches via the title-slug fallback (RES-03).
#[test]
fn resolve_explicit_path_title_fallback() {
    let temp = write_vault(&[("qa/db-transfer.md", "# Team knowledge transfer (QA/DB)\n")]);
    resolve(temp.path(), "Team knowledge transfer (QA/DB)", &[])
        .success()
        .stdout(predicate::str::contains("[title]"));
}

/// Prefix-only match with `obsidian_prefix` off → `broken`, no candidates
/// shown (default).
#[test]
fn resolve_prefix_off_by_default() {
    let temp = write_vault(&[("notes/finance-master-data-files.md", "# X\n")]);
    resolve(temp.path(), "finance-master-data", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"))
        .stdout(predicate::str::contains("prefix candidate").not());
}

/// `--include-prefix` lists advisory candidates; exit stays 1, status
/// unchanged.
#[test]
fn resolve_include_prefix_shows_advisory_candidates() {
    let temp = write_vault(&[("notes/finance-master-data-files.md", "# X\n")]);
    resolve(temp.path(), "finance-master-data", &["--include-prefix"])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("broken — 0 destinations:"))
        .stdout(predicate::str::contains(
            "hint: 1 prefix candidate (enable wiki.obsidian_prefix to match):",
        ))
        .stdout(predicate::str::contains("[prefix]"));
}

/// `obsidian_prefix = true` → prefix match is a regular destination, exit 0.
#[test]
fn resolve_prefix_enabled_config() {
    let temp = write_vault(&[
        (".downlint.toml", "[wiki]\nobsidian_prefix = true\n"),
        ("notes/finance-master-data-files.md", "# X\n"),
    ]);
    resolve(temp.path(), "finance-master-data", &[])
        .success()
        .stdout(predicate::str::contains("resolved — 1 destination:"))
        .stdout(predicate::str::contains("[prefix]"));
}

/// `[[schemas]]` mapped, file present → `mapped-present`, exit 0.
#[test]
fn resolve_uri_mapped_present() {
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nprefix = \"onedrive://work/\"\nroot = \"assets\"\n",
        ),
        ("assets/master-data/customers.xlsx", "x"),
    ]);
    resolve(
        temp.path(),
        "onedrive://work/master-data/customers.xlsx",
        &[],
    )
    .success()
    .stdout(predicate::str::contains("mapped-present"));
}

/// `[[schemas]]` mapped, file missing → `mapped-missing`, exit 1.
#[test]
fn resolve_uri_mapped_missing() {
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nprefix = \"onedrive://work/\"\nroot = \"assets\"\n",
        ),
        ("assets/other.xlsx", "x"),
    ]);
    resolve(
        temp.path(),
        "onedrive://work/master-data/customers.xlsx",
        &[],
    )
    .failure()
    .code(1)
    .stdout(predicate::str::contains("mapped-missing"))
    .stdout(predicate::str::contains("mapped file does not exist"));
}

/// `[[schemas]]` mapped, iCloud-style `.icloud` sibling → `mapped-placeholder`,
/// exit 1.
#[test]
fn resolve_uri_mapped_placeholder() {
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nprefix = \"icloud://assets/\"\nroot = \"Mobile Documents\"\n",
        ),
        ("Mobile Documents/evicted.pdf", "x"),
    ]);
    // The iCloud heuristic requires a `.icloud` sibling next to the file.
    fs::write(temp.path().join("Mobile Documents/.evicted.pdf.icloud"), "").unwrap();
    resolve(temp.path(), "icloud://assets/evicted.pdf", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("mapped-placeholder"))
        .stdout(predicate::str::contains(
            "evicted cloud placeholder detected",
        ));
}

/// Non-web scheme, no matching prefix → `unmapped`, exit 1.
#[test]
fn resolve_uri_unmapped() {
    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[[schemas]]\nprefix = \"onedrive://work/\"\nroot = \"assets\"\n",
        ),
        ("assets/a.xlsx", "x"),
    ]);
    resolve(temp.path(), "icloud://other/x.pdf", &[])
        .failure()
        .code(1)
        .stdout(predicate::str::contains("unmapped"));
}

/// External web scheme → `external`, exit 0.
#[test]
fn resolve_uri_external_web() {
    let temp = write_vault(&[("note.md", "# Note\n")]);
    resolve(temp.path(), "https://example.com/x", &[])
        .success()
        .stdout(predicate::str::contains("external"));
}

/// JSON output: one object with the documented shape.
#[test]
fn resolve_json_format_shape() {
    let temp = write_vault(&[
        ("avon-customer.md", "# Avon\n## Onboarding\n"),
        ("avon-vendor.md", "# Avon\n"),
    ]);
    let output = resolve(temp.path(), "Avon#onboarding", &["--format", "json"])
        .failure()
        .code(1)
        .stdout(predicate::str::is_empty().not())
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value =
        serde_json::from_slice(&output).expect("resolve --format json must emit one JSON object");
    assert_eq!(body["target"], "Avon#onboarding");
    assert_eq!(body["anchor"], "onboarding");
    assert_eq!(body["status"], "ambiguous");
    assert_eq!(body["destinations"].as_array().unwrap().len(), 2);
    let first = &body["destinations"][0];
    assert_eq!(first["path"], "avon-customer.md");
    assert_eq!(first["title"], "Avon");
    assert_eq!(first["match"][0], "title");
    assert_eq!(first["mount"], serde_json::Value::Null);
    assert_eq!(first["anchor"], true);
    assert_eq!(body["prefix_candidates"].as_array().unwrap().len(), 0);
    assert_eq!(body["scheme"], serde_json::Value::Null);
}

/// `--root` override is honored (the test process cwd is not the vault).
#[test]
fn resolve_root_override_honored() {
    let temp = write_vault(&[("avon.md", "# Avon\n")]);
    let root_str = temp.path().to_str().unwrap().to_string();
    let cwd_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // From a different cwd, only --root can find the vault.
    downlint()
        .current_dir(&cwd_root)
        .args(["resolve", "--root", &root_str, "Avon"])
        .assert()
        .success()
        .stdout(predicate::str::contains("resolved — 1 destination:"));
}
