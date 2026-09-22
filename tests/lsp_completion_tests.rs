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

use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

struct LspClient {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl LspClient {
    fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_downlint");
        let mut child = Command::new(bin)
            .arg("server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn downlint server");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = BufReader::new(child.stdout.take().expect("stdout pipe"));
        LspClient {
            child,
            stdin,
            stdout,
        }
    }

    /// Send a request (`id = Some`) or a notification (`id = None`).
    fn send(&mut self, method: &str, id: Option<u64>, params: serde_json::Value) {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Some(id) = id {
            msg["id"] = json!(id);
        }
        let body = msg.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(frame.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read frames until one carrying `expected_id` arrives, skipping
    /// notifications (e.g. `publishDiagnostics`).
    fn response(&mut self, expected_id: u64) -> serde_json::Value {
        loop {
            let body = read_frame(&mut self.stdout).expect("server closed stdout");
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            if value.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                return value;
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one Content-Length-framed LSP message body as a string.
fn read_frame(reader: &mut impl BufRead) -> Option<String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).to_string())
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

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
