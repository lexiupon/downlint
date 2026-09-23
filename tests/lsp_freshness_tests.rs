//! LSP workspace freshness tests (RFC 0022).
//!
//! The workspace snapshot is taken at `initialize`; these tests drive the
//! real `downlint server` over stdio and assert that created, changed, and
//! deleted files update diagnostics without a restart:
//!
//! - mechanism 1 (editor upsert): `didOpen` of a never-saved note resolves
//!   links to it; `didClose` of a never-saved note drops it again.
//! - mechanism 2 (file operations): `workspace/didCreateFiles` for a file
//!   created on disk but never opened.
//! - mechanism 3 (filesystem watcher): disk changes with no editor
//!   notifications at all (create and delete).
//!
//! Vocabulary per RFC 0020: `index` is the linking document, `missing` the
//! unresolved target, `topic` an existing target.

mod common;

use common::{LspClient, file_uri, write_vault};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Timeout for editor-notification mechanisms (reindex debounce only).
const EDITOR_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for watcher mechanisms (FSEvents/inotify latency + debounce).
const WATCHER_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait until a `publishDiagnostics` for `uri` arrives whose list is empty
/// (`cleared = true`) or non-empty (`cleared = false`). Returns false on
/// timeout — the assertion that matters is made by the caller.
fn wait_for_publish(client: &mut LspClient, uri: &str, cleared: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let Some(msg) = client.next_message_timeout(remaining) else {
            return false;
        };
        if msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && msg["params"]["uri"] == uri
        {
            let is_empty = msg["params"]["diagnostics"]
                .as_array()
                .map(Vec::is_empty)
                .unwrap_or(false);
            if is_empty == cleared {
                return true;
            }
        }
    }
}

/// Vault with a broken `[[missing]]` link in `index.md`.
fn broken_link_vault() -> tempfile::TempDir {
    write_vault(&[
        (".downlint.toml", ""),
        ("index.md", "# Index\n\n[[missing]]\n"),
    ])
}

/// Establish the baseline: the server must report `link/broken` for
/// `index.md` before any fix is attempted.
fn expect_initial_broken(client: &mut LspClient, root: &Path) {
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(client, &index_uri, false, EDITOR_TIMEOUT),
        "expected an initial broken-link diagnostic for index.md"
    );
}

/// Mechanism 1: a broken link is cleared when the missing target is created
/// **unsaved** in the editor (`didOpen` only — no disk write). Before the
/// fix, the document never entered the workspace snapshot, so the diagnostic
/// persisted until an LSP restart.
#[test]
fn lsp_diagnostic_cleared_when_target_created_unsaved() {
    let temp = broken_link_vault();
    let root = temp.path();
    let mut client = LspClient::initialized(root);
    expect_initial_broken(&mut client, root);

    let missing_uri = file_uri(&root.join("missing.md"));
    client.send(
        "textDocument/didOpen",
        None,
        json!({
            "textDocument": {
                "uri": &missing_uri,
                "languageId": "markdown",
                "version": 1,
                "text": "# Missing\n"
            }
        }),
    );
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(&mut client, &index_uri, true, EDITOR_TIMEOUT),
        "expected the broken-link diagnostic to clear once the target exists (unsaved)"
    );
}

/// Mechanism 2: a broken link is cleared when the target is created on disk
/// via the editor's file operations (`workspace/didCreateFiles`), without
/// ever opening it in a buffer.
#[test]
fn lsp_diagnostic_cleared_when_target_created_on_disk() {
    let temp = broken_link_vault();
    let root = temp.path();
    let mut client = LspClient::initialized(root);
    expect_initial_broken(&mut client, root);

    fs::write(root.join("missing.md"), "# Missing\n").unwrap();
    let missing_uri = file_uri(&root.join("missing.md"));
    client.send(
        "workspace/didCreateFiles",
        None,
        json!({ "files": [{ "uri": &missing_uri }] }),
    );
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(&mut client, &index_uri, true, EDITOR_TIMEOUT),
        "expected the broken-link diagnostic to clear after didCreateFiles"
    );
}

/// Mechanism 1 (ghost removal): a never-saved buffer must not leave a ghost
/// document in the workspace. Closing it re-breaks the link.
#[test]
fn lsp_ghost_doc_removed_on_close() {
    let temp = broken_link_vault();
    let root = temp.path();
    let mut client = LspClient::initialized(root);
    expect_initial_broken(&mut client, root);

    let missing_uri = file_uri(&root.join("missing.md"));
    client.send(
        "textDocument/didOpen",
        None,
        json!({
            "textDocument": {
                "uri": &missing_uri,
                "languageId": "markdown",
                "version": 1,
                "text": "# Missing\n"
            }
        }),
    );
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(&mut client, &index_uri, true, EDITOR_TIMEOUT),
        "expected the diagnostic to clear while the unsaved target is open"
    );

    client.send(
        "textDocument/didClose",
        None,
        json!({ "textDocument": { "uri": &missing_uri } }),
    );
    assert!(
        wait_for_publish(&mut client, &index_uri, false, EDITOR_TIMEOUT),
        "expected the broken-link diagnostic to return after closing the never-saved buffer"
    );
}

/// Mechanism 3: a broken link is cleared when the target is created on disk
/// with **no** editor notifications at all — the filesystem watcher must
/// pick it up. (This is the Neovim path: its built-in client does not send
/// `workspace/didCreateFiles`.)
#[test]
fn lsp_diagnostic_cleared_when_target_created_outside_editor() {
    let temp = broken_link_vault();
    let root = temp.path();
    let mut client = LspClient::initialized(root);
    expect_initial_broken(&mut client, root);

    fs::write(root.join("missing.md"), "# Missing\n").unwrap();
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(&mut client, &index_uri, true, WATCHER_TIMEOUT),
        "expected the broken-link diagnostic to clear after the target appeared on disk (watcher)"
    );
}

/// Mechanism 3 (mirror image): deleting a resolved target on disk must
/// re-raise `link/broken`. Before the fix the link stayed *resolved* until
/// an LSP restart.
#[test]
fn lsp_diagnostic_reraised_when_target_deleted() {
    let temp = write_vault(&[
        (".downlint.toml", ""),
        ("index.md", "# Index\n\n[[topic]]\n"),
        ("topic.md", "# Topic\n"),
    ]);
    let root = temp.path();
    let mut client = LspClient::initialized(root);

    // Baseline: the link resolves, so no diagnostic for index.md. Give the
    // initial publish a moment to settle before deleting.
    std::thread::sleep(Duration::from_millis(500));

    fs::remove_file(root.join("topic.md")).unwrap();
    let index_uri = file_uri(&root.join("index.md"));
    assert!(
        wait_for_publish(&mut client, &index_uri, false, WATCHER_TIMEOUT),
        "expected a broken-link diagnostic after the target was deleted (watcher)"
    );
}
