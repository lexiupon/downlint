pub mod edit;
pub mod handlers;
pub mod types;

use crate::lsp::types::{RpcError, RpcRequest, RpcResponse};
use crate::resolution::{ConnectionGraph, ResolveInput, UriOptions, resolve_links};
use crate::utils::{PositionEncoding, Text, Workspace, WorkspaceInput, discover_workspace};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use url::Url;

/// Debounce window for the background reindexer: a re-index fires this long
/// after the *last* edit, so a burst of keystrokes coalesces into one pass
/// (RFC 0017).
const REINDEX_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Default)]
struct ServerState {
    initialized: bool,
    shutdown_requested: bool,
    open_documents: HashMap<Url, String>,
    uri_opts: UriOptions,
    // Shared with the background reindexer (RFC 0017). Each is wrapped so the
    // request loop and the reindexer never touch the same data without a lock.
    workspace: Arc<Mutex<Option<Workspace>>>,
    graph: Arc<Mutex<Option<ConnectionGraph>>>,
    /// Serializes writes to `io::stdout()` so the loop and the reindexer never
    /// interleave a `Content-Length` header with another message's body.
    stdout_guard: Arc<Mutex<()>>,
    /// The diagnostics last published per document (path → diagnostics JSON).
    /// Used to detect when a document's diagnostics change — including when
    /// they are *cleared* — so an empty list is published to clear the
    /// client's stale diagnostics.
    diagnostics_state:
        Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<serde_json::Value>>>>,
    // Background reindexer handles (None until `initialize`).
    reindex_tx: Option<mpsc::Sender<()>>,
    reindex_shutdown: Option<mpsc::Sender<()>>,
    reindex_handle: Option<JoinHandle<()>>,
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
    let mut state = ServerState {
        uri_opts,
        ..Default::default()
    };

    while let Some(message) = read_message(&mut reader) {
        let request = match serde_json::from_slice::<RpcRequest>(&message) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    state.stdout_guard.as_ref(),
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
        let code = handle_request(&mut state, request);
        if let Some(code) = code {
            stop_reindexer(&mut state);
            return code;
        }
    }

    stop_reindexer(&mut state);
    if state.shutdown_requested { 0 } else { 1 }
}

fn handle_request(state: &mut ServerState, request: RpcRequest) -> Option<i32> {
    let method = request.method.unwrap_or_default();
    if !state.initialized && !matches!(method.as_str(), "initialize" | "shutdown" | "exit") {
        if request.id.is_some() {
            write_response(
                state.stdout_guard.as_ref(),
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
                state.stdout_guard.as_ref(),
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
            publish_current_diagnostics(state);
        }
        "shutdown" => {
            state.shutdown_requested = true;
            write_response(
                state.stdout_guard.as_ref(),
                RpcResponse {
                    jsonrpc: "2.0",
                    id: request.id,
                    result: Some(Value::Null),
                    error: None,
                },
            );
        }
        "exit" => return Some(if state.shutdown_requested { 0 } else { 1 }),
        // RFC 0017: edits no longer re-index inline. They update the shared
        // workspace text and signal the debounced background reindexer, which
        // runs `resolve_links` off the request loop and publishes the
        // resulting diagnostics.
        "textDocument/didOpen" => {
            apply_open_change(state, request.params.as_ref());
        }
        "textDocument/didChange" => {
            apply_text_change(state, request.params.as_ref());
        }
        "textDocument/didClose" => {
            apply_close_change(state, request.params.as_ref());
        }
        "textDocument/completion" => {
            // Honor the [completion] config (style + candidate cap) instead of
            // hardcoding the defaults. Falls back to the defaults when no
            // workspace is loaded (in which case with_text_position returns
            // empty anyway, so the values are unused).
            let (style, max_candidates) = state
                .workspace
                .lock()
                .unwrap()
                .as_ref()
                .map(|ws| {
                    (
                        ws.config.completion.wiki.style,
                        ws.config.completion.candidates,
                    )
                })
                .unwrap_or((crate::config::WikiCompletionStyle::TitleSlug, 50));
            let result = with_text_position(
                state,
                request.params.as_ref(),
                |graph, path, text, line, character| {
                    handlers::completion(graph, path, text, line, character, style, max_candidates)
                },
            )
            .unwrap_or_else(|| json!([]));
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
        }
        "textDocument/hover" => {
            let result = with_offset(state, request.params.as_ref(), |graph, path, _, offset| {
                handlers::hover(graph, path, offset).unwrap_or(Value::Null)
            })
            .unwrap_or(Value::Null);
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
        }
        "textDocument/definition" => {
            let result = with_offset(state, request.params.as_ref(), handlers::definition)
                .unwrap_or_else(|| json!([]));
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
        }
        "textDocument/references" => {
            let result = with_offset(state, request.params.as_ref(), |graph, path, _, offset| {
                handlers::references(graph, path, offset)
            })
            .unwrap_or_else(|| json!([]));
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
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
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|graph| handlers::document_symbols(graph, path))
                })
                .unwrap_or_else(|| json!([]));
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
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
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
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
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, result));
        }
        "textDocument/codeAction" => {
            let result = with_code_action_input(state, request.params.as_ref(), |graph, path, text, range| {
                handlers::code_actions(graph, path, text, range)
            })
            .unwrap_or_default();
            write_response(state.stdout_guard.as_ref(), ok_response(request.id, Value::Array(result)));
        }
        "workspace/didRenameFiles" => {
            // RFC 0009 Phase 4 — react after a file rename by updating
            // the graph in-place (path fields + reference destinations)
            // and re-publishing diagnostics for touched documents.
            handle_did_rename_files(state, request.params.as_ref());
        }
        _ => {
            if request.id.is_some() {
                write_response(
                    state.stdout_guard.as_ref(),
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
    // Plumb the opts through so the background reindexer reuses them (RFC 0017).
    input.uri_opts = state.uri_opts.clone();
    // The initial index is synchronous: the client is waiting for the
    // `initialize` response, so the first graph must be ready before we reply.
    let graph = resolve_links(input);
    *state.workspace.lock().unwrap() = Some(workspace);
    *state.graph.lock().unwrap() = Some(graph);
    state.initialized = true;
    // Stop any existing reindexer (re-initialization) before spawning a new one.
    stop_reindexer(state);
    spawn_reindexer(state);
    Some(root)
}

/// Publish diagnostics for `graph`, sending only what changed since the last
/// publish (tracked in `diagnostics_state`). A document whose diagnostics are
/// *cleared* gets an empty list, which tells the client to drop its stale
/// diagnostics (e.g. a link that was broken while typing and is resolved once
/// a completion is accepted).
///
/// On the very first publish (empty state) only documents that *have*
/// diagnostics are sent, so a large workspace does not emit one empty
/// notification per clean document at startup.
fn publish_diagnostics_for_graph(
    graph: &ConnectionGraph,
    diagnostics_state: &Mutex<std::collections::HashMap<std::path::PathBuf, Vec<Value>>>,
    stdout_guard: &Mutex<()>,
) {
    // Build the full set of diagnostics: every document, with an empty list for
    // documents that have none.
    let mut new_diags: std::collections::HashMap<std::path::PathBuf, Vec<Value>> = graph
        .documents
        .iter()
        .map(|doc| (doc.path.clone(), Vec::new()))
        .collect();
    for (path, diagnostics) in handlers::diagnostics(graph) {
        if let Some(entry) = new_diags.get_mut(&path) {
            *entry = diagnostics.as_array().cloned().unwrap_or_default();
        }
    }

    let mut state = diagnostics_state.lock().unwrap();
    let first_publish = state.is_empty();
    for (path, diagnostics) in &new_diags {
        let should_publish = if first_publish {
            !diagnostics.is_empty()
        } else {
            state
                .get(path)
                .map(|previous| previous != diagnostics)
                .unwrap_or(true)
        };
        if should_publish {
            let Some(uri) = Url::from_file_path(path).ok() else {
                continue;
            };
            write_notification(
                stdout_guard,
                "textDocument/publishDiagnostics",
                json!({
                    "uri": uri.to_string(),
                    "diagnostics": diagnostics,
                }),
            );
        }
    }
    *state = new_diags;
}

/// Publish diagnostics for the current graph (request-loop convenience wrapper).
fn publish_current_diagnostics(state: &ServerState) {
    let graph = state.graph.lock().unwrap();
    if let Some(graph) = graph.as_ref() {
        publish_diagnostics_for_graph(
            graph,
            state.diagnostics_state.as_ref(),
            state.stdout_guard.as_ref(),
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
        update_workspace_doc_text(state, &uri);
        signal_reindex(state);
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
        update_workspace_doc_text(state, &uri);
        signal_reindex(state);
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
    // Restore the document's text from disk (the editor's unsaved changes are
    // discarded on close).
    if let Ok(path) = uri.to_file_path()
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let mut guard = state.workspace.lock().unwrap();
        if let Some(ws) = guard.as_mut()
            && let Some(doc) = ws.folder.documents.iter_mut().find(|doc| doc.path == path)
        {
            doc.text = Text::new(text);
        }
    }
    signal_reindex(state);
}

/// Update a single document's text in the shared workspace from the
/// editor-provided text in `open_documents`. Holds the workspace lock only for
/// the brief mutation (RFC 0017).
fn update_workspace_doc_text(state: &ServerState, uri: &Url) {
    let Some(text) = state.open_documents.get(uri) else {
        return;
    };
    let Ok(path) = uri.to_file_path() else {
        return;
    };
    let mut guard = state.workspace.lock().unwrap();
    if let Some(ws) = guard.as_mut()
        && let Some(doc) = ws.folder.documents.iter_mut().find(|doc| doc.path == path)
    {
        doc.text = Text::new(text.clone());
    }
}

/// Signal the background reindexer that the workspace changed (RFC 0017). A
/// no-op until `initialize` has spawned the reindexer.
fn signal_reindex(state: &ServerState) {
    if let Some(tx) = &state.reindex_tx {
        let _ = tx.send(());
    }
}

fn with_offset<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, &PathBuf, &Text, usize) -> T,
) -> Option<T> {
    let graph_guard = state.graph.lock().unwrap();
    let graph = graph_guard.as_ref()?;
    let uri = params
        .and_then(path_from_text_document)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let document = workspace_guard
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
    let graph_guard = state.graph.lock().unwrap();
    let graph = graph_guard.as_ref()?;
    let uri = params
        .and_then(path_from_text_document)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let document = workspace_guard
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
    let graph_guard = state.graph.lock().unwrap();
    let graph = graph_guard.as_ref()?;
    let params = params?;
    let uri = path_from_text_document(params)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let document = workspace_guard
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
fn handle_did_rename_files(state: &mut ServerState, params: Option<&Value>) {
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
    {
        let mut guard = state.graph.lock().unwrap();
        if let Some(graph) = guard.as_mut() {
            handlers::workspace::apply_file_renames(graph, &renames);
        }
    }
    // Re-publish diagnostics for the affected documents. The graph
    // mutation above already touched the relevant state; this surfaces
    // it to the editor.
    publish_current_diagnostics(state);
}

/// Resolve the LSP `textDocument/codeAction` request shape: textDocument
/// URI, cursor selection range. Used by the rename code-action handler.
fn with_code_action_input<T>(
    state: &ServerState,
    params: Option<&Value>,
    handler: impl FnOnce(&crate::resolution::ConnectionGraph, &PathBuf, &Text, crate::utils::ByteRange) -> T,
) -> Option<T> {
    let graph_guard = state.graph.lock().unwrap();
    let graph = graph_guard.as_ref()?;
    let params = params?;
    let uri = path_from_text_document(params)
        .and_then(|value| Url::parse(value.as_str()).ok())?;
    let path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let document = workspace_guard
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

fn write_notification(guard: &Mutex<()>, method: &str, params: Value) {
    let message = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    write_message(guard, &message);
}

fn write_response(guard: &Mutex<()>, response: RpcResponse<'_>) {
    let message = serde_json::to_value(response).unwrap_or_else(|_| json!({}));
    write_message(guard, &message);
}

/// Write one LSP message to `io::stdout()`, atomically with respect to the
/// other writer (the background reindexer). The `Content-Length` header, body,
/// and flush all happen while holding `guard`, so two threads never interleave
/// a header with another message's body (RFC 0017).
fn write_message(guard: &Mutex<()>, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let _guard = guard.lock().unwrap();
    let mut stdout = io::stdout();
    let _ = write!(&mut stdout, "Content-Length: {}\r\n\r\n", body.len());
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

// --- Background reindexer (RFC 0017) -------------------------------------

/// Spawn the debounced background reindexer. Called from `initialize_state`
/// after the initial (synchronous) index. Clones the shared `Arc`s and a copy
/// of `uri_opts` into the new thread.
fn spawn_reindexer(state: &mut ServerState) {
    let (reindex_tx, reindex_rx) = mpsc::channel::<()>();
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let workspace = Arc::clone(&state.workspace);
    let graph = Arc::clone(&state.graph);
    let stdout_guard = Arc::clone(&state.stdout_guard);
    let diagnostics_state = Arc::clone(&state.diagnostics_state);
    let uri_opts = state.uri_opts.clone();
    let handle = thread::spawn(move || {
        reindexer_loop(
            workspace,
            graph,
            stdout_guard,
            diagnostics_state,
            uri_opts,
            reindex_rx,
            shutdown_rx,
        );
    });
    state.reindex_tx = Some(reindex_tx);
    state.reindex_shutdown = Some(shutdown_tx);
    state.reindex_handle = Some(handle);
}

/// Signal the reindexer to stop and join it. Idempotent and safe to call when
/// no reindexer is running. Called on every `run_server` exit path and before
/// re-spawning on re-initialization.
fn stop_reindexer(state: &mut ServerState) {
    if let Some(tx) = state.reindex_shutdown.take() {
        let _ = tx.send(());
    }
    if let Some(handle) = state.reindex_handle.take() {
        let _ = handle.join();
    }
    state.reindex_tx = None;
}

/// Debounce loop: coalesce a burst of edit signals into one re-index that fires
/// `REINDEX_DEBOUNCE` after the *last* signal. Exits on shutdown or when the
/// signal channel is disconnected.
fn reindexer_loop(
    workspace: Arc<Mutex<Option<Workspace>>>,
    graph: Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: Arc<Mutex<()>>,
    diagnostics_state: Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<Value>>>>,
    uri_opts: UriOptions,
    reindex_rx: mpsc::Receiver<()>,
    shutdown_rx: mpsc::Receiver<()>,
) {
    let mut dirty = false;
    loop {
        match reindex_rx.recv_timeout(REINDEX_DEBOUNCE) {
            Ok(()) => dirty = true, // an edit happened; reset the window
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty {
                    perform_reindex(
                        &workspace,
                        &graph,
                        &stdout_guard,
                        &diagnostics_state,
                        &uri_opts,
                    );
                    dirty = false;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
    }
}

/// Perform one re-index: snapshot the workspace (brief lock), run
/// `resolve_links` with NO lock held, swap the graph in, then publish the
/// resulting diagnostics.
fn perform_reindex(
    workspace: &Arc<Mutex<Option<Workspace>>>,
    graph: &Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: &Arc<Mutex<()>>,
    diagnostics_state: &Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<Value>>>>,
    uri_opts: &UriOptions,
) {
    let input = {
        let ws = workspace.lock().unwrap();
        ws.as_ref().map(|ws| {
            let mut input = ResolveInput::from_workspace(ws);
            input.uri_opts = uri_opts.clone();
            input
        })
    };
    let Some(input) = input else {
        return;
    };
    let new_graph = resolve_links(input);
    *graph.lock().unwrap() = Some(new_graph);
    if let Some(current) = graph.lock().unwrap().as_ref() {
        publish_diagnostics_for_graph(current, diagnostics_state.as_ref(), stdout_guard.as_ref());
    }
}

#[cfg(test)]
mod reindexer_tests {
    use super::*;

    /// `perform_reindex` with no workspace is a no-op: the graph cell is left
    /// untouched (still `None`) and nothing is written to stdout.
    #[test]
    fn perform_reindex_without_workspace_is_noop() {
        let workspace: Arc<Mutex<Option<Workspace>>> = Arc::new(Mutex::new(None));
        let graph: Arc<Mutex<Option<ConnectionGraph>>> = Arc::new(Mutex::new(None));
        let stdout_guard: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
        let diagnostics_state: Arc<
            Mutex<std::collections::HashMap<std::path::PathBuf, Vec<serde_json::Value>>>,
        > = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let uri_opts = UriOptions::default();
        perform_reindex(
            &workspace,
            &graph,
            &stdout_guard,
            &diagnostics_state,
            &uri_opts,
        );
        assert!(graph.lock().unwrap().is_none());
    }

    /// A default `ServerState` has no reindexer running (all handles `None`)
    /// and an empty graph cell — the shared-state guard is additive.
    #[test]
    fn default_state_has_no_reindexer() {
        let state = ServerState::default();
        assert!(state.reindex_tx.is_none());
        assert!(state.reindex_shutdown.is_none());
        assert!(state.reindex_handle.is_none());
        assert!(state.graph.lock().unwrap().is_none());
    }

    /// `stop_reindexer` is a safe no-op when no reindexer is running (it must
    /// not panic or block on a missing handle).
    #[test]
    fn stop_reindexer_without_running_is_noop() {
        let mut state = ServerState::default();
        stop_reindexer(&mut state);
        assert!(state.reindex_handle.is_none());
    }
}
