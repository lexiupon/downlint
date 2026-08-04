//! Integration tests for the `downlint sync` subcommand. Run end-to-end
//! via `assert_cmd` so we exercise the real CLI wiring.

use assert_cmd::Command;
use predicates::str::contains;
use std::fs;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("downlint").expect("downlint binary")
}

#[cfg(unix)]
fn write_shell_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

#[test]
fn sync_without_flag_exits_with_error() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(".downlint.toml"),
        r#"
[[uri.mappings]]
prefix = "scheme://"
root = "./assets"
sync_cmd = ["true"]
"#,
    )
    .unwrap();

    bin()
        .current_dir(tmp.path())
        .arg("sync")
        .assert()
        .failure()
        .code(2)
        .stderr(contains("requires --allow-uri-sync"));
}

#[test]
fn sync_with_flag_no_targets_is_noop() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(".downlint.toml"),
        r#"
[[uri.mappings]]
prefix = "scheme://"
root = "./assets"
sync_cmd = ["true"]
"#,
    )
    .unwrap();

    // No markdown files referencing the URI; nothing to sync.
    bin()
        .current_dir(tmp.path())
        .args(["sync", "--allow-uri-sync"])
        .assert()
        .success()
        .code(0);
}

#[cfg(unix)]
#[test]
fn sync_runs_sync_cmd_and_creates_files() {
    let tmp = TempDir::new().unwrap();
    let script = write_shell_script(
        tmp.path(),
        "touch.sh",
        "#!/bin/sh\ntouch \"$1\"\n",
    );
    fs::write(
        tmp.path().join(".downlint.toml"),
        format!(
            r#"
[[uri.mappings]]
prefix = "scheme://"
root = "./assets"
sync_cmd = ["{}", "{{path}}"]
"#,
            script.to_string_lossy()
        ),
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("assets")).unwrap();
    fs::write(
        tmp.path().join("notes.md"),
        "[[scheme://synced]]\n[[scheme://also-synced]]\n",
    )
    .unwrap();

    bin()
        .current_dir(tmp.path())
        .args(["sync", "--allow-uri-sync"])
        .assert()
        .success()
        .code(0)
        .stdout(contains("total=2"));

    // Files now exist on disk.
    assert!(tmp.path().join("assets/synced").exists());
    assert!(tmp.path().join("assets/also-synced").exists());
}

#[cfg(unix)]
#[test]
fn sync_exit_code_reflects_failure() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(".downlint.toml"),
        r#"
[[uri.mappings]]
prefix = "scheme://"
root = "./assets"
sync_cmd = ["false"]
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("assets")).unwrap();
    fs::write(
        tmp.path().join("notes.md"),
        "[[scheme://will-fail]]\n",
    )
    .unwrap();

    bin()
        .current_dir(tmp.path())
        .args(["sync", "--allow-uri-sync"])
        .assert()
        .failure()
        .code(1)
        .stdout(contains("failed=1"));
}