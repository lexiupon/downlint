//! LSP subprocess integration tests for `textDocument/completion` config
//! handling.
//!
//! Captures the bug where the LSP completion handler hardcoded
//! `WikiCompletionStyle::TitleSlug` and `max_candidates: 50`, ignoring the
//! `[completion]` section of `.downlint.toml`. These tests drive the real
//! `downlint server` over stdio and assert the completion honors the config.
//!
//! Note: the completion `label` is always the note's H1 title text; the
//! configured `style` is observable in `textEdit.newText` (the inserted text).
//! So the assertions target `newText`.

mod common;

use common::{LspClient, file_uri};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// A workspace with the given `[completion]` TOML and two notes whose H1 titles
/// differ from their file stems (so file-stem and title-slug produce different
/// `newText`).
fn setup_workspace(completion_toml: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join(".downlint.toml"), completion_toml).unwrap();
    // alpha.md is the document being edited; its `[[` is at line 2.
    fs::write(root.join("notes/alpha.md"), "# Alpha Title\n\n[[\n").unwrap();
    fs::write(root.join("notes/beta.md"), "# Beta Title\n").unwrap();
    temp
}

/// Drive the server to a completion at the `[[` in alpha.md and return the
/// `textEdit.newText` values of the returned items.
fn completion_new_texts(root: &Path) -> Vec<String> {
    let mut client = LspClient::spawn();
    client.send("initialize", Some(1), json!({ "rootUri": file_uri(root) }));
    let init = client.response(1);
    assert!(init.get("result").is_some(), "initialize failed: {init}");
    client.send("initialized", None, json!({}));

    let alpha = root.join("notes/alpha.md");
    let alpha_text = fs::read_to_string(&alpha).unwrap();
    client.send(
        "textDocument/didOpen",
        None,
        json!({
            "textDocument": {
                "uri": file_uri(&alpha),
                "languageId": "markdown",
                "version": 1,
                "text": alpha_text
            }
        }),
    );

    // alpha.md = "# Alpha Title\n\n[[" → `[[` is at line 2, chars 0-1; the
    // cursor sits at char 2 (right after `[[`), so the needle is empty.
    client.send(
        "textDocument/completion",
        Some(2),
        json!({
            "textDocument": { "uri": file_uri(&alpha) },
            "position": { "line": 2, "character": 2 }
        }),
    );
    let comp = client.response(2);

    let items = comp["result"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|item| item["textEdit"]["newText"].as_str().map(String::from))
        .collect()
}

/// The bug (style): with `wiki.style = "file-stem"`, completion must insert the
/// file stem (`beta`), not the title slug (`beta-title`). Before the fix the
/// server hardcoded `TitleSlug`, so this fails.
#[test]
fn lsp_completion_honors_config_style() {
    let temp = setup_workspace("[completion]\nwiki = { style = \"file-stem\" }\n");
    let texts = completion_new_texts(temp.path());
    assert!(
        texts.contains(&"beta".to_string()),
        "expected file-stem 'beta' in completion, got {texts:?}"
    );
    assert!(
        !texts.contains(&"beta-title".to_string()),
        "title-slug 'beta-title' must not appear under file-stem config, got {texts:?}"
    );
}

/// The bug (candidate cap): with `candidates = 1`, completion must be capped at
/// one item even though two candidate notes exist. Before the fix the server
/// hardcoded 50, so both would be returned.
#[test]
fn lsp_completion_honors_config_candidates() {
    let temp = setup_workspace("[completion]\ncandidates = 1\nwiki = { style = \"file-stem\" }\n");
    // A second candidate so there are two to cap (alpha is the source, excluded).
    fs::write(temp.path().join("notes/gamma.md"), "# Gamma Title\n").unwrap();
    let texts = completion_new_texts(temp.path());
    assert_eq!(
        texts.len(),
        1,
        "candidates = 1 should cap completion at one item, got {texts:?}"
    );
}

/// Regression: without a `[completion]` config, the default (title-slug) is
/// used. Guards against the fix accidentally changing the default behavior.
#[test]
fn lsp_completion_defaults_to_title_slug() {
    let temp = setup_workspace(""); // empty config → all defaults
    let texts = completion_new_texts(temp.path());
    assert!(
        texts.contains(&"beta-title".to_string()),
        "expected default title-slug 'beta-title', got {texts:?}"
    );
    assert!(
        !texts.contains(&"beta".to_string()),
        "file-stem 'beta' must not appear under the default title-slug style, got {texts:?}"
    );
}

/// RFC 0017: a completion request must be served WITHOUT waiting for the
/// post-edit re-index. Before the fix, `didChange` triggered an inline
/// `resolve_links` that blocked the request loop, so the completion response
/// arrived only *after* the re-index's `publishDiagnostics`. Now the re-index
/// runs in a debounced background thread, so the completion response is written
/// immediately — before any post-edit diagnostics.
///
/// This is an *ordering* assertion (not a timing one), so it holds even on a
/// small vault: after a `didChange`, the completion response must be the first
/// message, not a `publishDiagnostics`.
#[test]
fn lsp_completion_not_blocked_by_reindex() {
    let temp = setup_workspace("");
    let root = temp.path();
    let alpha = root.join("notes/alpha.md");
    let alpha_text = fs::read_to_string(&alpha).unwrap();

    let mut client = LspClient::spawn();
    client.send("initialize", Some(1), json!({ "rootUri": file_uri(root) }));
    let init = client.response(1);
    assert!(init.get("result").is_some(), "initialize failed: {init}");
    client.send("initialized", None, json!({}));

    // Flush the initial diagnostics with a marker request: reading until the
    // marker response consumes every `publishDiagnostics` the `initialized`
    // handler emitted, leaving the stream clear.
    client.send(
        "textDocument/completion",
        Some(99),
        json!({
            "textDocument": { "uri": file_uri(&alpha) },
            "position": { "line": 2, "character": 2 }
        }),
    );
    client.response(99);

    // Edit the document (triggers the background re-index) and immediately
    // request completion.
    client.send(
        "textDocument/didChange",
        None,
        json!({
            "textDocument": { "uri": file_uri(&alpha), "version": 2 },
            "contentChanges": [{ "text": alpha_text }]
        }),
    );
    client.send(
        "textDocument/completion",
        Some(2),
        json!({
            "textDocument": { "uri": file_uri(&alpha) },
            "position": { "line": 2, "character": 2 }
        }),
    );

    // The completion response must arrive BEFORE any post-edit
    // `publishDiagnostics`. Read in arrival order until the completion
    // response; assert no diagnostic was seen first.
    let mut saw_diagnostic_before_completion = false;
    loop {
        let value = client.next_message();
        if value.get("id").and_then(|v| v.as_u64()) == Some(2) {
            break;
        }
        if value.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
            saw_diagnostic_before_completion = true;
        }
    }
    assert!(
        !saw_diagnostic_before_completion,
        "completion was blocked behind a post-edit re-index (a publishDiagnostics arrived before the completion response)"
    );
}

/// A broken link that is fixed by a `didChange` must have its diagnostic
/// *cleared* (an empty `publishDiagnostics`), not left stale in the client.
/// Regression for "completion accepted but the old broken-link diagnostic
/// persists".
#[test]
fn lsp_diagnostics_cleared_when_link_fixed() {
    let root = setup_workspace("");
    let mut client = LspClient::spawn();
    client.send(
        "initialize",
        Some(1),
        json!({ "rootUri": file_uri(root.path()) }),
    );
    let init = client.response(1);
    assert!(init.get("result").is_some(), "initialize failed: {init}");
    client.send("initialized", None, json!({}));

    let alpha = root.path().join("notes/alpha.md");
    let uri = file_uri(&alpha);
    // Open alpha with a *complete* broken link (`[[missing]]` — no such note),
    // which the re-indexer resolves to a `link/broken` diagnostic.
    client.send(
        "textDocument/didOpen",
        None,
        json!({
            "textDocument": { "uri": &uri, "languageId": "markdown", "version": 1, "text": "# Alpha Title\n\n[[missing]]\n" }
        }),
    );
    // Wait for the initial broken-link diagnostic for alpha.
    let mut saw_broken = false;
    for _ in 0..500 {
        let msg = client.next_message();
        if msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && msg["params"]["uri"] == uri
            && msg["params"]["diagnostics"]
                .as_array()
                .map(|d| !d.is_empty())
                .unwrap_or(false)
        {
            saw_broken = true;
            break;
        }
    }
    assert!(
        saw_broken,
        "expected a broken-link diagnostic after didOpen"
    );

    // Fix the link (didChange with the full text).
    client.send(
        "textDocument/didChange",
        None,
        json!({
            "textDocument": { "uri": &uri, "version": 2 },
            "contentChanges": [{ "text": "# Alpha Title\n\n[[Beta Title]]\n" }]
        }),
    );
    // Wait for the cleared (empty) diagnostic for alpha.
    let mut saw_cleared = false;
    for _ in 0..500 {
        let msg = client.next_message();
        if msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && msg["params"]["uri"] == uri
            && msg["params"]["diagnostics"]
                .as_array()
                .map(|d| d.is_empty())
                .unwrap_or(false)
        {
            saw_cleared = true;
            break;
        }
    }
    assert!(
        saw_cleared,
        "expected an empty diagnostic (cleared) after fixing the link"
    );
}
