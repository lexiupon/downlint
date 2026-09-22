//! CLI subprocess tests for `downlint info` — the read-only "what does downlint
//! see" report (RFC 0016).
//!
//! `info` is descriptive only: exit `0` on a loaded workspace (even with missing
//! folders), exit `2` on a config error (no workspace, bad args, or a schema
//! `to` that cannot be expanded). Mounts are always rooted OUTSIDE the vault
//! root (a sibling temp dir) so they are indexed only via the mount, never as
//! primary docs.

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

/// Run `downlint info [extra...] --root <root>`.
fn info(root: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    let mut full: Vec<String> = vec!["info".into()];
    for arg in extra {
        full.push(arg.to_string());
    }
    full.push("--root".into());
    full.push(root.to_str().unwrap().to_string());
    downlint().args(full).assert()
}

/// Two mounts (one linted, one not) + a schema: text output shows the `as`
/// prefixes, resolved absolute paths, lint flags, per-mount doc counts, the
/// schema's expanded path, and reconciled document totals.
#[test]
fn info_shows_mounts_schemas_and_doc_counts() {
    // Mount content lives OUTSIDE the vault root (sibling temp dirs).
    let kb = write_vault(&[("m1.md", "# M1\n"), ("m2.md", "# M2\n")]);
    let arch = write_vault(&[("m1.md", "# M1\n")]);
    let kb_canon = fs::canonicalize(kb.path()).unwrap();
    let arch_canon = fs::canonicalize(arch.path()).unwrap();

    let vault = write_vault(&[("notes/a.md", "# A\n"), ("notes/b.md", "# B\n")]);
    let config = format!(
        "[[mounts]]\npath = \"{}\"\nas = \"/kb\"\nlint = true\n\n\
         [[mounts]]\npath = \"{}\"\nas = \"/arch\"\nlint = false\n\n\
         [[schemas]]\nuri = \"icloud://assets/\"\nto = \"assets\"\nauto_verify = true\n",
        kb.path().display(),
        arch.path().display(),
    );
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();
    fs::create_dir_all(vault.path().join("assets")).unwrap();

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("mounts (2)"))
        .stdout(predicate::str::contains("/kb"))
        .stdout(predicate::str::contains("lint=yes"))
        .stdout(predicate::str::contains("2 docs"))
        .stdout(predicate::str::contains("/arch"))
        .stdout(predicate::str::contains("lint=no"))
        .stdout(predicate::str::contains("1 docs"))
        .stdout(predicate::str::contains(kb_canon.to_str().unwrap()))
        .stdout(predicate::str::contains(arch_canon.to_str().unwrap()))
        .stdout(predicate::str::contains("schemas (1)"))
        .stdout(predicate::str::contains("icloud://assets/"))
        .stdout(predicate::str::contains(
            vault.path().join("assets").to_str().unwrap(),
        ))
        .stdout(predicate::str::contains(
            "documents  5 total  (2 primary \u{b7} 3 mounted)",
        ));
}

/// A mount whose folder does not exist → `✗ missing` marker, exit 0.
#[test]
fn info_missing_mount_folder_is_exit_0() {
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    let config = "[[mounts]]\npath = \"./does-not-exist\"\nas = \"/ghost\"\nlint = true\n";
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("ghost"))
        .stdout(predicate::str::contains("\u{2717} missing"));
}

/// A schema whose `to` folder does not exist → `✗ missing` marker, exit 0.
#[test]
fn info_missing_schema_folder_is_exit_0() {
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    let config = "[[schemas]]\nuri = \"icloud://assets/\"\nto = \"./no-such-folder\"\n";
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("icloud://assets/"))
        .stdout(predicate::str::contains("\u{2717} missing"));
}

/// `--format json` → valid JSON with the report envelope.
#[test]
fn info_json_shape() {
    let kb = write_vault(&[("m1.md", "# M1\n")]);
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    let config = format!(
        "[[mounts]]\npath = \"{}\"\nas = \"/kb\"\nlint = true\n",
        kb.path().display()
    );
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    let assert = info(vault.path(), &["--format", "json"]).success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    assert!(value["workspace"].is_string());
    assert!(value["config"].is_string());
    let mounts = value["mounts"].as_array().unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0]["as"], "/kb");
    assert_eq!(mounts[0]["lint"], true);
    assert_eq!(mounts[0]["docs"], 1);
    assert!(value["documents"]["total"].is_number());
    assert!(value["conflicts"].is_array());
}

/// No `.downlint.toml` → `config` is `null` (JSON) / `(defaults)` (text), exit 0.
#[test]
fn info_no_config_is_defaults() {
    let vault = write_vault(&[("notes/a.md", "# A\n")]);

    let assert = info(vault.path(), &["--format", "json"]).success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["config"], serde_json::Value::Null);

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("config      (defaults)"));
}

/// A mount with no `as` → `as` is `null` (JSON) / `(none)` (text).
#[test]
fn info_mount_without_as_is_null() {
    let kb = write_vault(&[("m1.md", "# M1\n")]);
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    let config = format!(
        "[[mounts]]\npath = \"{}\"\nlint = true\n",
        kb.path().display()
    );
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    let assert = info(vault.path(), &["--format", "json"]).success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let mounts = value["mounts"].as_array().unwrap();
    assert_eq!(mounts[0]["as"], serde_json::Value::Null);

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("(none)"));
}

/// A namespace conflict (a mount file colliding with a primary file) is listed
/// under `conflicts` but does not change the exit code (still 0).
#[test]
fn info_conflict_is_exit_0_and_listed() {
    // Mount `as = "kb"` maps its root to namespace `kb/`; its `x.md` has
    // namespace path `kb/x.md`, which collides with the primary `kb/x.md`.
    let kb = write_vault(&[("x.md", "# M\n")]);
    let vault = write_vault(&[("kb/x.md", "# P\n")]);
    let config = format!(
        "[[mounts]]\npath = \"{}\"\nas = \"/kb\"\nlint = true\n",
        kb.path().display()
    );
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    info(vault.path(), &[])
        .success()
        .stdout(predicate::str::contains("conflicts  (1)"))
        .stdout(predicate::str::contains("/kb"));
}

/// A schema `to` referencing an unset env var is a config error → exit 2.
#[test]
fn info_schema_missing_env_var_is_exit_2() {
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    let config = "[[schemas]]\nuri = \"icloud://assets/\"\nto = \"${DOWNLINT_INFO_TEST_UNSET_9f3a}/assets\"\n";
    fs::write(vault.path().join(".downlint.toml"), config).unwrap();

    info(vault.path(), &[])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("DOWNLINT_INFO_TEST_UNSET_9f3a"))
        .stderr(predicate::str::contains("is not set"));
}

/// An invalid `--format` value is a clap error → exit 2.
#[test]
fn info_bad_format_is_exit_2() {
    let vault = write_vault(&[("notes/a.md", "# A\n")]);
    info(vault.path(), &["--format", "bogus"]).failure().code(2);
}
