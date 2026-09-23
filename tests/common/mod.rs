//! Shared helpers for the CLI/LSP subprocess test suites.
//!
//! Each integration test file is its own crate; including this module
//! (`mod common;`) gives them one shared fixture vocabulary helper
//! (RFC 0020) and the LSP subprocess client. Not every crate uses every
//! helper, so dead-code warnings are suppressed here.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Create a temporary vault with the given `(rel_path, content)` pairs.
/// Parent directories are created as needed.
pub fn write_vault(pairs: &[(&str, &str)]) -> TempDir {
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

/// An LSP client driving the real `downlint server` over stdio.
///
/// All reads go through a single background reader thread feeding a channel,
/// so callers can use either the blocking [`LspClient::next_message`] or the
/// timeout-bounded [`LspClient::next_message_timeout`] without mixing readers
/// on the pipe.
pub struct LspClient {
    child: Child,
    stdin: std::process::ChildStdin,
    messages: mpsc::Receiver<String>,
}

impl LspClient {
    pub fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_downlint");
        let mut child = Command::new(bin)
            .arg("server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn downlint server");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = child.stdout.take().expect("stdout pipe");
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(body) = read_frame(&mut reader) {
                if tx.send(body).is_err() {
                    break;
                }
            }
        });
        LspClient {
            child,
            stdin,
            messages: rx,
        }
    }

    /// Send a request (`id = Some`) or a notification (`id = None`).
    pub fn send(&mut self, method: &str, id: Option<u64>, params: Value) {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Some(id) = id {
            msg["id"] = json!(id);
        }
        let body = msg.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(frame.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read the next message (response or notification), in arrival order.
    /// Blocks until a message arrives; panics after 30 s so a dead server
    /// cannot hang the suite forever.
    pub fn next_message(&mut self) -> Value {
        match self
            .messages
            .recv_timeout(Duration::from_secs(30))
        {
            Ok(body) => serde_json::from_str(&body).unwrap(),
            Err(error) => panic!("no LSP message within 30 s: {error}"),
        }
    }

    /// Like [`LspClient::next_message`], but returns `None` when no message
    /// arrives within `timeout`. Required for assertions about messages that
    /// *may not* arrive (e.g. "the diagnostic was cleared" — the cleared
    /// publish is the thing being awaited, and its absence must fail the
    /// test, not hang it).
    pub fn next_message_timeout(&mut self, timeout: Duration) -> Option<Value> {
        self.messages
            .recv_timeout(timeout)
            .ok()
            .map(|body| serde_json::from_str(&body).unwrap())
    }

    /// Read frames until one carrying `expected_id` arrives, skipping
    /// notifications (e.g. `publishDiagnostics`).
    pub fn response(&mut self, expected_id: u64) -> Value {
        loop {
            let value = self.next_message();
            if value.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                return value;
            }
        }
    }

    /// Drive the server to `initialize` + `initialized` for `root` and return
    /// the client. Panics if the server rejects initialization.
    pub fn initialized(root: &Path) -> Self {
        let mut client = Self::spawn();
        client.send(
            "initialize",
            Some(1),
            json!({ "rootUri": file_uri(root) }),
        );
        let init = client.response(1);
        assert!(
            init.get("result").is_some(),
            "initialize failed: {init}"
        );
        client.send("initialized", None, json!({}));
        client
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

pub fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}
