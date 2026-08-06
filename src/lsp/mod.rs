pub mod edit;
pub mod handlers;
pub mod types;

use crate::lsp::types::{RpcError, RpcRequest, RpcResponse};
use crate::resolution::{ResolveInput, resolve_links};
use crate::utils::{PositionEncoding, Text, Workspace, WorkspaceInput, discover_workspace};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use url::Url;

#[derive(Default)]
struct ServerState {
    initialized: bool,
    shutdown_requested: bool,
    workspace: Option<Workspace>,
    graph: Option<crate::resolution::ConnectionGraph>,
    open_documents: HashMap<Url, String>,
    uri_opts: crate::resolution::UriOptions,
    /// Cache of `(mapping_index, absolute_path) -> run_for` results so the
    /// language server does not re-fork sync subprocesses on every
    /// keystroke. Populated lazily by `resolve_links`; cleared on
    /// `.downlint.toml` change (Phase 2 will surface a config watcher).
    uri_sync_cache: crate::resolution::uri_sync::UriSyncCache,
    /// True while a `resolve_links` pass is in flight. Rename operations
    /// (RFC 0009) must reject while this is set — the `ConnectionGraph`
    /// would otherwise be in an inconsistent intermediate state. Set by
    /// `initialize_state` / `refresh_graph` / `refresh_workspace_doc`
    /// around the `resolve_links` call, cleared on return.
    indexing: bool,
}


pub async fn run_server(
    _verbose: u8,
    wait_for_debugger: bool,
    uri_opts: crate::resolution::UriOptions,
) -> i32 {
    if wait_for_debugger {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    let mut state = ServerState::default();
    state.uri_opts = uri_opts;

    while let Some(message) = read_message(&mut reader) {
        let request = match serde_json::from_slice::<RpcRequest>(&message) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    RpcResponse {
                        jsonrpc: "2.0",
                        id: None,
                        result: None,
                        error: Some(RpcError {
                            code: -32700,
                            message: error.to_string(),
                        }),
                    },
                );
                continue;
            }
        };
        let code = handle_request(&mut state, &mut stdout, request);
        if let Some(code) = code {
            return code;
        }
    }

    if state.shutdown_requested { 0 } else { 1 }
}

fn handle_request(
    state: &mut ServerState,
    stdout: &mut impl Write,
    request: RpcRequest,
) -> Option<i32> {
    let method = request.method.unwrap_or_default();
    if !state.initialized && !matches!(method.as_str(), "initialize" | "shutdown" | "exit") {
        if request.id.is_some() {
            write_response(
                stdout,
                RpcResponse {
                    jsonrpc: "2.0",
                    id: request.id,
                    result: None,
                    error: Some(RpcError {
                        code: -32002,
                        message: "Server not initialized".into(),
                    }),
                },
            );
        }
        return None;
    }

    match method.as_str() {
        "initialize" => {
            let root = initialize_state(state, request.params.as_ref());
            let capabilities = json!({
                "capabilities": {
                    "positionEncoding": "utf-16",
                    "textDocumentSync": {
                        "openClose": true,
                        "change": 1,
                        "save": true
                    },
                    "completionProvider": { "triggerCharacters": ["[", "#", "("] },
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "referencesProvider": true,
                    "documentSymbolProvider": true,
                    // RFC 0009 — Rename & Link Refactor.
                    //
                    // - `renameProvider` with `prepareProvider: true` advertises
                    //   the safe string-only F2 rename gesture. Never moves files.
                    // - `codeActionProvider` advertises three rename actions; the
                    //   actual planning ships in Phases 2/3/6.
                    // - `workspace.fileOperations.didRename` triggers editor
                    //   notifications on file rename; markdown + catch-all filters
                    //   cover both documents and attachments.
                    "renameProvider": { "prepareProvider": true },
                    "codeActionProvider": {
                        "codeActionKinds": [
                            handlers::CODE_ACTION_KIND_FILE,
                            handlers::CODE_ACTION_KIND_LINK_TARGET,
                            handlers::CODE_ACTION_KIND_HEADING
                        ]
                    },
                    "workspace": {
                        "fileOperations": {
                            "didRename": {
                                "filters": [
                                    { "pattern": { "glob": "**/*.{md,markdown,mdx}" } },
                                    { "pattern": { "glob": "**/*" } }
                                ]
                            }
                        }
                    }
                },
                "serverInfo": {
                    "name": "downlint",
                    "version": crate::version::VERSION
                }
            });
            write_response(
                stdout,
                RpcResponse {
                    jsonrpc: "2.0",
                    id: request.id,
                    result: Some(capabilities),
                    error: None,
                },
            );
            if let Some(root) = root {
                tracing::info!("initialized workspace at {}", root.display());
            }
        }
        "initialized" => {
            publish_diagnostics(state, stdout);
        }
        "shutdown" => {
            state.shutdown_requested = true;
            write_response(
                stdout,
                RpcResponse {
                    jsonrpc: "2.0",
                    id: request.id,
                    result: Some(Value::Null),
                    error: None,
                },
            );
        }
        "exit" => return Some(if state.shutdown_requested { 0 } else { 1 }),
        "textDocument/didOpen" => {
            apply_open_change(state, request.params.as_ref());
            publish_diagnostics(state, stdout);
        }
        "textDocument/didChange" => {
            apply_text_change(state, request.params.as_ref());
            publish_diagnostics(state, stdout);
        }
        "textDocument/didClose" => {
            apply_close_change(state, request.params.as_ref());
            publish_diagnostics(state, stdout);
        }
        "textDocument/completion" => {
            let result = with_text_position(
                state,
                request.params.as_ref(),
                |graph, path, text, line, character| {
                    handlers::completion(graph, path, text, line, character)
                },
            )
            .unwrap_or_else(|| json!([]));
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/hover" => {
            let result = with_offset(state, request.params.as_ref(), |graph, path, _, offset| {
                handlers::hover(graph, path, offset).unwrap_or(Value::Null)
            })
            .unwrap_or(Value::Null);
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/definition" => {
            let result = with_offset(state, request.params.as_ref(), handlers::definition)
                .unwrap_or_else(|| json!([]));
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/references" => {
            let result = with_offset(state, request.params.as_ref(), |graph, path, _, offset| {
                handlers::references(graph, path, offset)
            })
            .unwrap_or_else(|| json!([]));
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/documentSymbol" => {
            let path = request
                .params
                .as_ref()
                .and_then(path_from_text_document)
                .and_then(|uri| uri.to_file_path().ok());
            let result = path
                .as_ref()
                .and_then(|path| {
                    state
                        .graph
                        .as_ref()
                        .map(|graph| handlers::document_symbols(graph, path))
                })
                .unwrap_or_else(|| json!([]));
            write_response(stdout, ok_response(request.id, result));
        }
        // RFC 0009 — Rename & Link Refactor.
        //
        // The three methods below are string-only or infrastructure-only at
        // Phase 1. The actual planner work for each `refactor.rename.*` code
        // action ships in Phases 2/3/6; for now the code-action handler
        // offers them but invoking the not-yet-implemented kinds surfaces
        // `MethodFailed` via `RenameError::NotImplemented`.
        "textDocument/prepareRename" => {
            let result = with_offset(state, request.params.as_ref(), |graph, path, text, offset| {
                match handlers::prepare_rename(graph, path, text, offset) {
                    Some(hit) => {
                        // LSP expects { range, placeholder }. We return
                        // the current string as the placeholder so editors
                        // pre-populate the rename box.
                        let range = handlers::hit_range_json(text, &hit);
                        let placeholder = match &hit {
                            handlers::PrepareRenameHit::LinkTarget { current, .. } => current.clone(),
                            handlers::PrepareRenameHit::Heading { current, .. } => current.clone(),
                        };
                        json!({
                            "range": range,
                            "placeholder": placeholder,
                        })
                    }
                    None => Value::Null,
                }
            })
            .unwrap_or(Value::Null);
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/rename" => {
            let result = with_rename_input(state, request.params.as_ref(), |graph, path, text, offset, new_name| {
                let hit = handlers::prepare_rename(graph, path, text, offset)?;
                let range = match &hit {
                    handlers::PrepareRenameHit::LinkTarget { range, .. } => *range,
                    handlers::PrepareRenameHit::Heading { range, .. } => *range,
                };
                let uri = url::Url::from_file_path(path).ok()?.to_string();
                Some(crate::lsp::edit::build_single_edit_workspace_edit(text, &uri, range, new_name))
            })
            .unwrap_or(Value::Null);
            write_response(stdout, ok_response(request.id, result));
        }
        "textDocument/codeAction" => {
            let result = with_code_action_input(state, request.params.as_ref(), |graph, path, text, range| {
                handlers::code_actions(graph, path, text, range)
            })
            .unwrap_or_default();
            write_response(stdout, ok_response(request.id, Value::Array(result)));
        }
        "workspace/didRenameFiles" => {
            // RFC 0009 Phase 4 — react after a file rename by updating
            // the graph in-place (path fields + reference destinations)
            // and re-publishing diagnostics for touched documents.
            handle_did_rename_files(state, stdout, request.params.as_ref());
        }
        _ => {
            if request.id.is_some() {
                write_response(
                    stdout,
                    RpcResponse {
                        jsonrpc: "2.0",
                        id: request.id,
                        result: None,
                        error: Some(RpcError {
                            code: -32601,
                            message: format!("method not found: {method}"),
                        }),
                    },
                );
            }
        }
    }

    None
}

fn initialize_state(state: &mut ServerState, params: Option<&Value>) -> Option<PathBuf> {
    let root = infer_root_from_initialize(params);
    let root =
        root.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let workspace =
        discover_workspace(WorkspaceInput::Path(root.clone()), Some(root.as_path())).ok()?;
    let mut input = ResolveInput::from_workspace(&workspace);
    // Plumb the cache + opts through so refresh_graph reuses the same cache
    // across LSP events. The cache is `Arc<Mutex<...>>`, so the runner's
    // mutations are visible to `state.uri_sync_cache` without further action.
    input.uri_sync_cache = state.uri_sync_cache.clone();
    input.uri_opts = state.uri_opts.clone();
    state.indexing = true;
    let graph = resolve_links(input);
    state.indexing = false;
    state.workspace = Some(workspace);
    state.graph = Some(graph);
    state.initialized = true;
    Some(root)
}

/// Returns true while a `resolve_links` pass is in flight. Rename handlers
/// (RFC 0009) consult this to refuse work during indexing — a half-built
/// graph could produce incorrect rewrites. Stable to call from any thread
/// that already holds `&ServerState` (the underlying bool is mutated only
/// synchronously by the LSP event loop today).
#[allow(dead_code)] // Wired up in RFC 0009 Phase 1 (rename handlers).
fn is_indexing(state: &ServerState) -> bool {
    state.indexing
}

fn publish_diagnostics(state: &ServerState, stdout: &mut impl Write) {
    let Some(graph) = &state.graph else {
        return;
    };
    let Some(workspace) = &state.workspace else {
        return;
    };
    for (path, diagnostics) in handlers::diagnostics(graph, workspace, &state.uri_opts) {
        let Some(uri) = Url::from_file_path(&path).ok() else {
            continue;
        };
        write_notification(
            stdout,
            "textDocument/publishDiagnostics",
            json!({
                "uri": uri.to_string(),
                "diagnostics": diagnostics,
            }),
        );
    }
}

fn apply_open_change(state: &mut ServerState, params: Option<&Value>) {
    let Some(params) = params else {
        return;
    };
    let uri = params
        .get("textDocument")
        .and_then(|value| value.get("uri"))
        .and_then(Value::as_str)
        .and_then(|value| Url::parse(value).ok());
    let text = params
        .get("textDocument")
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(uri), Some(text)) = (uri, text) {
        state.open_documents.insert(uri.clone(), text);
        refresh_workspace_doc(state, &uri);
    }
}

fn apply_text_change(state: &mut ServerState, params: Option<&Value>) {
    let Some(params) = params else {
        return;
    };
    let uri = params
        .get("textDocument")
        .and_then(|value| value.get("uri"))
        .and_then(Value::as_str)
        .and_then(|value| Url::parse(value).ok());
    let change = params
        .get("contentChanges")
        .and_then(Value::as_array)
        .and_then(|changes| changes.last())
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(uri), Some(text)) = (uri, change) {
        state.open_documents.insert(uri.clone(), text);
        refresh_workspace_doc(state, &uri);
    }
}

fn apply_close_change(state: &mut ServerState, params: Option<&Value>) {
    let Some(uri) = params
        .and_then(path_from_text_document)
        .and_then(|uri| Url::parse(uri.as_str()).ok())
    else {
        return;
    };
    state.open_documents.remove(&uri);
    if let Some(workspace) = &mut state.workspace
        && let Ok(path) = uri.to_file_path()
        && let Some(doc) = workspace
            .folder
            .documents
            .iter_mut()
            .find(|doc| doc.path == path)
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        doc.text = Text::new(text);
    }
    refresh_graph(state);
}

fn refresh_workspace_doc(state: &mut ServerState, uri: &Url) {
    if let Some(workspace) = &mut state.workspace
        && let Ok(path) = uri.to_file_path()
        && let Some(text) = state.open_documents.get(uri)
        && let Some(doc) = workspace
            .folder
            .documents
            .iter_mut()
            .find(|doc| doc.path == path)
    {
        doc.text = Text::new(text.clone());
    }
    refresh_graph(state);
}

fn refresh_graph(state: &mut ServerState) {
    if let Some(workspace) = &state.workspace {
        let mut input = ResolveInput::from_workspace(workspace);
        // Share the cache so sync results from prior passes persist.
        input.uri_sync_cache = state.uri_sync_cache.clone();
        input.uri_opts = state.uri_opts.clone();
        state.indexing = true;
        let graph = resolve_links(input);
        state.indexing = false;
        state.graph = Some(graph);
    }
}

fn with_offset<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, &PathBuf, &Text, usize) -> T,
) -> Option<T> {
    let graph = state.graph.as_ref()?;
    let uri = params
        .and_then(path_from_text_document)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let document = state
        .workspace
        .as_ref()?
        .folder
        .documents
        .iter()
        .find(|doc| doc.path == path)?;
    let position = params.and_then(|value| value.get("position")).cloned()?;
    let line = position.get("line")?.as_u64()? as u32;
    let character = position.get("character")?.as_u64()? as u32;
    let offset = document
        .text
        .byte_offset(
            &lsp_types::Position::new(line, character),
            PositionEncoding::Utf16,
        )
        .ok()?;
    Some(handler(graph, &path, &document.text, offset))
}

fn with_text_position<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, PathBuf, &Text, u32, u32) -> T,
) -> Option<T> {
    let graph = state.graph.as_ref()?;
    let uri = params
        .and_then(path_from_text_document)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let document = state
        .workspace
        .as_ref()?
        .folder
        .documents
        .iter()
        .find(|doc| doc.path == path)?;
    let position = params?.get("position")?;
    let line = position.get("line")?.as_u64()? as u32;
    let character = position.get("character")?.as_u64()? as u32;
    Some(handler(graph, path, &document.text, line, character))
}

/// Resolve the LSP `textDocument/rename` request shape: textDocument URI,
/// cursor position, and `newName`. Returns the absolute byte offset of
/// the cursor and the requested new name. Used by the string-only F2
/// rename handler — Phase 1.
fn with_rename_input<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, &PathBuf, &Text, usize, &str) -> Option<T>,
) -> Option<T> {
    let graph = state.graph.as_ref()?;
    let params = params?;
    let uri = path_from_text_document(params)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let document = state
        .workspace
        .as_ref()?
        .folder
        .documents
        .iter()
        .find(|doc| doc.path == path)?;
    let position = params.get("position")?;
    let line = position.get("line")?.as_u64()? as u32;
    let character = position.get("character")?.as_u64()? as u32;
    let offset = document
        .text
        .byte_offset(
            &lsp_types::Position::new(line, character),
            PositionEncoding::Utf16,
        )
        .ok()?;
    let new_name = params.get("newName").and_then(Value::as_str)?;
    handler(graph, &path, &document.text, offset, new_name)
}

/// Handle `workspace/didRenameFiles`: apply each rename to the graph
/// in-place and re-publish diagnostics for the touched documents. O(r)
/// per rename where r is the number of references to the moved file
/// (RFC §"Performance").
fn handle_did_rename_files(state: &mut ServerState, stdout: &mut impl Write, params: Option<&Value>) {
    let Some(params) = params else {
        return;
    };
    let Some(files) = params.get("files").and_then(Value::as_array) else {
        return;
    };
    let mut renames = Vec::new();
    for entry in files {
        let Some(old_uri) = entry.get("oldUri").and_then(Value::as_str) else {
            continue;
        };
        let Some(new_uri) = entry.get("newUri").and_then(Value::as_str) else {
            continue;
        };
        let Ok(old_url) = Url::parse(old_uri) else {
            continue;
        };
        let Ok(new_url) = Url::parse(new_uri) else {
            continue;
        };
        let Ok(old_path) = old_url.to_file_path() else {
            continue;
        };
        let Ok(new_path) = new_url.to_file_path() else {
            continue;
        };
        renames.push(handlers::workspace::FileRename {
            from: old_path,
            to: new_path,
        });
    }
    if let Some(graph) = state.graph.as_mut() {
        handlers::workspace::apply_file_renames(graph, &renames);
    }
    // Re-publish diagnostics for the affected documents. The graph
    // mutation above already touched the relevant state; this surfaces
    // it to the editor.
    publish_diagnostics(state, stdout);
}

/// Resolve the LSP `textDocument/codeAction` request shape: textDocument
/// URI, cursor selection range. Used by the rename code-action handler.
fn with_code_action_input<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, &PathBuf, &Text, crate::utils::ByteRange) -> T,
) -> Option<T> {
    let graph = state.graph.as_ref()?;
    let params = params?;
    let uri = path_from_text_document(params)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let document = state
        .workspace
        .as_ref()?
        .folder
        .documents
        .iter()
        .find(|doc| doc.path == path)?;
    let range_value = params.get("range")?;
    let start = range_value.get("start")?;
    let end = range_value.get("end")?;
    let start_line = start.get("line")?.as_u64()? as u32;
    let start_character = start.get("character")?.as_u64()? as u32;
    let end_line = end.get("line")?.as_u64()? as u32;
    let end_character = end.get("character")?.as_u64()? as u32;
    let start_pos = lsp_types::Position::new(start_line, start_character);
    let end_pos = lsp_types::Position::new(end_line, end_character);
    let start_offset = document
        .text
        .byte_offset(&start_pos, PositionEncoding::Utf16)
        .ok()?;
    let end_offset = document
        .text
        .byte_offset(&end_pos, PositionEncoding::Utf16)
        .ok()?;
    let range = crate::utils::ByteRange::new(start_offset, end_offset);
    Some(handler(graph, &path, &document.text, range))
}

fn path_from_text_document(params: &Value) -> Option<Url> {
    params
        .get("textDocument")
        .and_then(|value| value.get("uri"))
        .and_then(Value::as_str)
        .and_then(|value| Url::parse(value).ok())
}

fn infer_root_from_initialize(params: Option<&Value>) -> Option<PathBuf> {
    let params = params?;
    if let Some(folders) = params.get("workspaceFolders").and_then(Value::as_array)
        && let Some(path) = folders
            .first()
            .and_then(|folder| folder.get("uri"))
            .and_then(Value::as_str)
            .and_then(parse_file_uri)
    {
        return Some(path);
    }
    params
        .get("rootUri")
        .and_then(Value::as_str)
        .and_then(parse_file_uri)
        .or_else(|| {
            params
                .get("rootPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
}

fn parse_file_uri(value: &str) -> Option<PathBuf> {
    Url::parse(value).ok()?.to_file_path().ok()
}

fn ok_response(id: Option<Value>, result: Value) -> RpcResponse<'static> {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn write_notification(stdout: &mut impl Write, method: &str, params: Value) {
    let message = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    write_message(stdout, &message);
}

fn write_response(stdout: &mut impl Write, response: RpcResponse<'_>) {
    let message = serde_json::to_value(response).unwrap_or_else(|_| json!({}));
    write_message(stdout, &message);
}

fn write_message(stdout: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let _ = write!(stdout, "Content-Length: {}\r\n\r\n", body.len());
    let _ = stdout.write_all(&body);
    let _ = stdout.flush();
}

fn read_message(reader: &mut impl BufRead) -> Option<Vec<u8>> {
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
    Some(body)
}

#[cfg(test)]
mod indexing_guard_tests {
    use super::*;

    /// Default ServerState reports `is_indexing == false`. The guard is
    /// additive: every existing LSP entry point must continue to operate as
    /// if no guard existed when no `resolve_links` pass is in flight.
    #[test]
    fn default_state_is_not_indexing() {
        let state = ServerState::default();
        assert!(!is_indexing(&state));
    }

    /// Manually toggling the flag (which is what `initialize_state` /
    /// `refresh_graph` do around `resolve_links`) is observed by
    /// `is_indexing`. This is the contract rename handlers will rely on.
    #[test]
    fn manual_flip_is_observed() {
        let mut state = ServerState::default();
        assert!(!is_indexing(&state));
        state.indexing = true;
        assert!(is_indexing(&state));
        state.indexing = false;
        assert!(!is_indexing(&state));
    }

    /// `refresh_graph` on a default (uninitialized) state is a no-op: it
    /// guards on `state.workspace` being set. The indexing flag must stay
    /// `false` — the guard must not "leak" `true` from a pass that never
    /// ran. This is a regression test against accidentally flipping the
    /// flag outside the `if let` block.
    #[test]
    fn refresh_graph_without_workspace_leaves_flag_clear() {
        let mut state = ServerState::default();
        // No workspace set — the function takes the early return path.
        refresh_graph(&mut state);
        assert!(!is_indexing(&state));
    }
}
