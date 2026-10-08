//! RFC 0024: editor buffers do not change extension-based document classification.

mod common;

use common::{LspClient, file_uri, write_vault};
use serde_json::{Value, json};
use std::fs;
use std::time::{Duration, Instant};

// Includes filesystem watcher latency as well as the reindex debounce.
const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn editor_upsert_guards_existing_entries_and_preserves_eligible_ignored_buffers() {
    use downlint::lsp::freshness::upsert_workspace_doc;
    use downlint::utils::{WorkspaceInput, discover_workspace};
    use std::sync::{Arc, Mutex};

    let temp = write_vault(&[
        (
            ".downlint.toml",
            "[core]\nfile_extensions = [\"note\"]\nignore = [\"ignored/**\"]\n",
        ),
        ("index.note", "# Index\n"),
        ("ignored/report.note", "# Disk\n"),
    ]);
    let mut ws = discover_workspace(WorkspaceInput::Path(temp.path().to_path_buf()), None).unwrap();
    assert_eq!(ws.folder.documents.len(), 1);
    // Seed an ineligible existing entry to prove the guard also covers updates,
    // rather than only the new-document insertion branch.
    let mut plain = ws.folder.documents[0].clone();
    plain.path = ws.folder.root.join("report");
    plain.rel_path = "report".into();
    ws.folder.documents.push(plain);
    ws.folder
        .documents
        .sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let workspace = Arc::new(Mutex::new(Some(ws)));
    for name in ["report", "report.md", "report.NOTE"] {
        upsert_workspace_doc(
            &workspace,
            &temp.path().join(name),
            "# Wrong\n[[missing]]\n",
        );
    }
    for name in ["ignored/report.note", ".hidden/report.note", "missing.note"] {
        upsert_workspace_doc(&workspace, &temp.path().join(name), "# Editor\n");
    }
    let guard = workspace.lock().unwrap();
    let ws = guard.as_ref().unwrap();
    assert_eq!(ws.folder.documents.len(), 5);
    assert_eq!(
        ws.folder
            .documents
            .iter()
            .find(|doc| doc.rel_path == std::path::Path::new("report"))
            .unwrap()
            .text
            .as_str(),
        "# Index\n"
    );
    for name in ["ignored/report.note", ".hidden/report.note", "missing.note"] {
        assert_eq!(
            ws.folder
                .documents
                .iter()
                .find(|doc| doc.rel_path == std::path::Path::new(name))
                .unwrap()
                .text
                .as_str(),
            "# Editor\n"
        );
    }
}

fn open(client: &mut LspClient, uri: &str, language: &str, text: &str) {
    client.send(
        "textDocument/didOpen",
        None,
        json!({
            "textDocument": { "uri": uri, "languageId": language, "version": 1, "text": text }
        }),
    );
}

fn change(client: &mut LspClient, uri: &str, version: u64, text: &str) {
    client.send(
        "textDocument/didChange",
        None,
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        }),
    );
}

fn close(client: &mut LspClient, uri: &str) {
    client.send(
        "textDocument/didClose",
        None,
        json!({ "textDocument": { "uri": uri } }),
    );
}

/// A deliberately changed number of broken sentinel links in index.md makes
/// every reindex observable (clean, unchanged documents do not get a publish).
/// Also reject Markdown diagnostics from attachment buffers while waiting.
fn expect_diagnostics(client: &mut LspClient, uri: &str, count: usize, attachments: &[&str]) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = client
            .next_message_timeout(remaining)
            .unwrap_or_else(|| panic!("expected {count} diagnostics for {uri}"));
        if msg["method"] != "textDocument/publishDiagnostics" {
            continue;
        }
        let published_uri = msg["params"]["uri"].as_str().unwrap();
        let diagnostics = msg["params"]["diagnostics"].as_array().unwrap();
        if attachments.contains(&published_uri) {
            assert!(
                diagnostics.is_empty(),
                "attachment parsed as Markdown: {msg}"
            );
        }
        if published_uri == uri && diagnostics.len() == count {
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic["code"] == "link/broken"),
                "{msg}"
            );
            return;
        }
    }
}

fn result(client: &mut LspClient, method: &str, params: Value) -> Value {
    let requested_uri = params["textDocument"]["uri"].clone();
    client.send(method, Some(10), params);
    loop {
        let msg = client.next_message();
        // Publishing precedes the response once the observed reindex has
        // finished, so catch attachment diagnostics even if index.md's
        // publish happened to arrive first in the unordered diagnostics map.
        if method == "textDocument/documentSymbol"
            && msg["method"] == "textDocument/publishDiagnostics"
            && msg["params"]["uri"] == requested_uri
        {
            assert!(
                msg["params"]["diagnostics"].as_array().unwrap().is_empty(),
                "{msg}"
            );
        }
        if msg["id"] == 10 {
            return msg["result"].clone();
        }
    }
}

fn symbols(client: &mut LspClient, uri: &str) -> Value {
    result(
        client,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    )
}

fn definition(client: &mut LspClient, uri: &str, line: u64) -> Value {
    result(
        client,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": if line == 0 { 10 } else { 3 } }
        }),
    )
}

fn index_text(target: &str, sentinels: usize) -> String {
    format!(
        "[report]({target}#section)\n[[{target}]]\n[[{target}#section]]\n{}",
        "[[missing]]\n".repeat(sentinels)
    )
}

#[test]
fn existing_plain_files_stay_attachments_across_open_change_and_close() {
    // LICENSE retains the reported real-world filename; report covers the
    // general extensionless case. Even a Markdown language ID cannot opt in.
    for (target, language) in [
        ("LICENSE", "plaintext"),
        ("report", "markdown"),
        ("image.png", "markdown"),
    ] {
        let temp = write_vault(&[
            (".downlint.toml", ""),
            ("index.md", &index_text(target, 1)),
            (target, "# Section\n\n[[missing]]\n"),
        ]);
        let root = temp.path();
        let index_uri = file_uri(&root.join("index.md"));
        let target_uri = file_uri(&root.join(target));
        let mut client = LspClient::initialized(root);
        // Inline file+fragment resolves without heading validation; bare wiki
        // links must not acquire a note/heading destination from file contents.
        let wiki_broken = if target == "image.png" { 0 } else { 2 };
        expect_diagnostics(&mut client, &index_uri, wiki_broken + 1, &[&target_uri]);
        for stage in 2..=4 {
            match stage {
                2 => open(
                    &mut client,
                    &target_uri,
                    language,
                    "# Section\n\n[missing](missing)\n",
                ),
                3 => change(&mut client, &target_uri, 2, "# Section\n\n[[missing]]\n"),
                _ => close(&mut client, &target_uri),
            }
            change(
                &mut client,
                &index_uri,
                stage as u64,
                &index_text(target, stage),
            );
            expect_diagnostics(&mut client, &index_uri, wiki_broken + stage, &[&target_uri]);
            assert_eq!(symbols(&mut client, &target_uri), json!([]));
            for line in 0..=2 {
                assert_eq!(
                    definition(&mut client, &index_uri, line),
                    json!([]),
                    "plain file supplied a document/heading destination"
                );
            }
        }
    }
}

#[test]
fn never_saved_extensionless_buffer_does_not_resolve_missing_file() {
    let temp = write_vault(&[
        (".downlint.toml", ""),
        ("index.md", &index_text("report", 1)),
    ]);
    let index_uri = file_uri(&temp.path().join("index.md"));
    let target_uri = file_uri(&temp.path().join("report"));
    let mut client = LspClient::initialized(temp.path());
    expect_diagnostics(&mut client, &index_uri, 4, &[&target_uri]);
    for stage in 2..=4 {
        match stage {
            2 => open(
                &mut client,
                &target_uri,
                "markdown",
                "# Section\n[[missing]]\n",
            ),
            3 => change(
                &mut client,
                &target_uri,
                2,
                "# Section\n[missing](missing)\n",
            ),
            _ => close(&mut client, &target_uri),
        }
        change(
            &mut client,
            &index_uri,
            stage as u64,
            &index_text("report", stage),
        );
        expect_diagnostics(&mut client, &index_uri, 3 + stage, &[&target_uri]);
        assert_eq!(symbols(&mut client, &target_uri), json!([]));
        assert_eq!(definition(&mut client, &index_uri, 0), json!([]));
        assert!(!temp.path().join("report").exists());
    }
}

#[test]
fn configured_extensions_and_unsaved_markdown_still_supply_documents_and_headings() {
    for (target, language) in [("report.note", "plaintext"), ("report.md", "plaintext")] {
        let temp = write_vault(&[
            (
                ".downlint.toml",
                "[core]\nfile_extensions = [\"md\", \"note\"]\n",
            ),
            ("index.md", &format!("[report]({target}#section)\n")),
        ]);
        let index_uri = file_uri(&temp.path().join("index.md"));
        let target_uri = file_uri(&temp.path().join(target));
        let mut client = LspClient::initialized(temp.path());
        expect_diagnostics(&mut client, &index_uri, 1, &[]);
        open(&mut client, &target_uri, language, "# Section\n");
        expect_diagnostics(&mut client, &index_uri, 0, &[]);
        assert_eq!(symbols(&mut client, &target_uri)[0]["name"], "Section");
        assert_eq!(definition(&mut client, &index_uri, 0)[0]["uri"], target_uri);
        // Changes to eligible unsaved documents remain authoritative.
        change(&mut client, &target_uri, 2, "# New section\n");
        // Anchor diagnostics use a different code, so observe the graph via
        // a changed sentinel count, then check the updated symbols directly.
        change(
            &mut client,
            &index_uri,
            2,
            &format!("[report]({target}#new-section)\n[[missing]]\n"),
        );
        expect_diagnostics(&mut client, &index_uri, 1, &[]);
        assert_eq!(symbols(&mut client, &target_uri)[0]["name"], "New section");
        close(&mut client, &target_uri);
        expect_diagnostics(&mut client, &index_uri, 2, &[]);
        assert_eq!(symbols(&mut client, &target_uri), json!([]));
    }
}

#[test]
fn attachment_create_delete_watcher_refreshes_without_editor_notifications() {
    let temp = write_vault(&[
        (".downlint.toml", ""),
        ("index.md", "[report](report#section)\n"),
    ]);
    let index_uri = file_uri(&temp.path().join("index.md"));
    let target_uri = file_uri(&temp.path().join("report"));
    let mut client = LspClient::initialized(temp.path());
    expect_diagnostics(&mut client, &index_uri, 1, &[&target_uri]);
    fs::write(temp.path().join("report"), "# Section\n[[missing]]\n").unwrap();
    expect_diagnostics(&mut client, &index_uri, 0, &[&target_uri]);
    assert_eq!(symbols(&mut client, &target_uri), json!([]));
    fs::remove_file(temp.path().join("report")).unwrap();
    expect_diagnostics(&mut client, &index_uri, 1, &[&target_uri]);
}

#[test]
fn attachment_create_delete_notifications_refresh_diagnostics_even_while_open() {
    let temp = write_vault(&[
        (".downlint.toml", ""),
        ("index.md", "[report](report#section)\n"),
    ]);
    let index_uri = file_uri(&temp.path().join("index.md"));
    let target_uri = file_uri(&temp.path().join("report"));
    let mut client = LspClient::initialized(temp.path());
    expect_diagnostics(&mut client, &index_uri, 1, &[&target_uri]);
    open(
        &mut client,
        &target_uri,
        "markdown",
        "# Section\n[[missing]]\n",
    );
    fs::write(temp.path().join("report"), "# Section\n[[missing]]\n").unwrap();
    client.send(
        "workspace/didCreateFiles",
        None,
        json!({ "files": [{ "uri": &target_uri }] }),
    );
    expect_diagnostics(&mut client, &index_uri, 0, &[&target_uri]);
    assert_eq!(symbols(&mut client, &target_uri), json!([]));
    fs::remove_file(temp.path().join("report")).unwrap();
    client.send(
        "workspace/didDeleteFiles",
        None,
        json!({ "files": [{ "uri": &target_uri }] }),
    );
    expect_diagnostics(&mut client, &index_uri, 1, &[&target_uri]);
    assert_eq!(symbols(&mut client, &target_uri), json!([]));
}
