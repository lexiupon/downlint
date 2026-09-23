pub mod edit;
pub mod freshness;
pub mod handlers;
pub mod types;

use crate::lsp::types::{RpcError, RpcRequest, RpcResponse};
use crate::resolution::{ConnectionGraph, ResolveInput, UriOptions, resolve_links};
use crate::utils::{
    PositionEncoding, Text, Workspace, WorkspaceInput, discover_workspace, to_root_form,
};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use url::Url;

/// Debounce window for the background reindexer: a re-index fires this long
/// after the *last* edit, so a burst of keystrokes coalesces into one pass
/// (RFC 0017).
const REINDEX_DEBOUNCE: Duration = Duration::from_millis(300);

/// Cap on how long a continuous stream of changes (e.g. a sync daemon or a
/// long git operation) may defer a re-index: while dirty, the reindexer
/// re-indexes at least every 2 s (RFC 0022).
const REINDEX_MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct ServerState {
    initialized: bool,
    shutdown_requested: bool,
    // Shared with the background reindexer (RFC 0017) and the disk-change
    // reconciliation (RFC 0022). Each is wrapped so the request loop, the
    // reindexer, and the watcher never touch the same data without a lock.
    open_documents: Arc<Mutex<HashMap<Url, String>>>,
    uri_opts: UriOptions,
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
    /// Paths with pending disk changes to reconcile before the next re-index
    /// (RFC 0022): appended by the watcher and the `did*Files` handlers,
    /// drained by the reindexer.
    pending_disk_changes: Arc<Mutex<std::collections::HashSet<PathBuf>>>,
    // Background reindexer handles (None until `initialize`).
    reindex_tx: Option<mpsc::Sender<()>>,
    reindex_shutdown: Option<mpsc::Sender<()>>,
    reindex_handle: Option<JoinHandle<()>>,
    // Filesystem watcher (None until `initialize`, RFC 0022). The watcher
    // owns the event channel's sender: dropping it disconnects the channel
    // and ends the watcher thread.
    watcher: Option<RecommendedWatcher>,
    watcher_handle: Option<JoinHandle<()>>,
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
            stop_background(&mut state);
            return code;
        }
    }

    stop_background(&mut state);
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
            // RFC 0009 + RFC 0022: file-operation notifications. `didRename`
            // keeps the graph consistent after an editor rename; `didCreate`
            // / `didDelete` feed the disk-change reconciliation (the same
            // filters cover documents and attachments).
            let file_op_filters = json!([
                { "pattern": { "glob": "**/*.{md,markdown,mdx}" } },
                { "pattern": { "glob": "**/*" } }
            ]);
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
                            "didCreate": { "filters": file_op_filters },
                            "didRename": { "filters": file_op_filters },
                            "didDelete": { "filters": file_op_filters }
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
                .and_then(|uri| uri.to_file_path().ok())
                .and_then(|raw_path| {
                    let root = state
                        .workspace
                        .lock()
                        .unwrap()
                        .as_ref()?
                        .folder
                        .root
                        .clone();
                    to_root_form(&root, &raw_path)
                });
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
        // RFC 0022 — editor file creation/deletion: record the paths for
        // disk-change reconciliation on the next re-index.
        "workspace/didCreateFiles" | "workspace/didDeleteFiles" => {
            handle_did_file_ops(state, request.params.as_ref());
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
    // The root keeps the client's path form (RFC 0022): published URIs must
    // match the form the client uses for its buffers. Incoming paths are
    // converted to this form via `to_root_form`.
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
    // Re-initialization: stop the old background threads, reset the per-
    // session shared state (RFC 0022), and spawn fresh ones.
    stop_background(state);
    *state.open_documents.lock().unwrap() = HashMap::new();
    state.pending_disk_changes = Arc::default();
    spawn_reindexer(state);
    spawn_watcher(state, &root);
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
        state.open_documents.lock().unwrap().insert(uri.clone(), text.clone());
        upsert_from_editor(state, &uri, &text);
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
        state.open_documents.lock().unwrap().insert(uri.clone(), text.clone());
        upsert_from_editor(state, &uri, &text);
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
    state.open_documents.lock().unwrap().remove(&uri);
    if let Ok(raw_path) = uri.to_file_path() {
        let mut guard = state.workspace.lock().unwrap();
        // Convert to the root's path form so the path compares equal to
        // indexed document paths (RFC 0022).
        if let Some(ws) = guard.as_mut()
            && let Some(path) = to_root_form(&ws.folder.root, &raw_path)
        {
            if path.exists() {
                // Restore the document's text from disk (the editor's
                // unsaved changes are discarded on close).
                if let Ok(text) = std::fs::read_to_string(&path)
                    && let Some(doc) = ws.folder.documents.iter_mut().find(|doc| doc.path == path)
                {
                    doc.text = Text::new(text);
                }
            } else if ws.folder.documents.iter().any(|doc| doc.path == path) {
                // The file does not exist on disk: the buffer was a
                // never-saved creation (or the file was deleted while
                // open). Drop the document so it stops resolving links
                // (RFC 0022).
                ws.folder.documents.retain(|doc| doc.path != path);
            }
        }
    }
    signal_reindex(state);
}

/// Upsert a workspace document from editor text (RFC 0022): a file opened or
/// edited in the editor that is not yet in the workspace snapshot is added
/// (even if never saved to disk), so links to it resolve without a restart.
fn upsert_from_editor(state: &ServerState, uri: &Url, text: &str) {
    let Ok(raw_path) = uri.to_file_path() else {
        return;
    };
    freshness::upsert_workspace_doc(&state.workspace, &raw_path, text);
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
    let raw_path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let ws = workspace_guard.as_ref()?;
    // Convert to the root's path form so the lookup matches indexed
    // document paths regardless of the client's symlink form (RFC 0022).
    let path = to_root_form(&ws.folder.root, &raw_path)?;
    let document = ws
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
    let raw_path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let ws = workspace_guard.as_ref()?;
    // Convert to the root's path form so the lookup matches indexed
    // document paths regardless of the client's symlink form (RFC 0022).
    let path = to_root_form(&ws.folder.root, &raw_path)?;
    let document = ws
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
    let raw_path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let ws = workspace_guard.as_ref()?;
    // Convert to the root's path form so the lookup matches indexed
    // document paths regardless of the client's symlink form (RFC 0022).
    let path = to_root_form(&ws.folder.root, &raw_path)?;
    let document = ws
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

/// Handle `workspace/didCreateFiles` / `workspace/didDeleteFiles` (RFC
/// 0022): record each path for disk-change reconciliation on the next
/// re-index. The reconciliation reads the created file from disk (or drops
/// the deleted one) — open buffers are never touched.
fn handle_did_file_ops(state: &ServerState, params: Option<&Value>) {
    let Some(files) = params
        .and_then(|params| params.get("files"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let Some(root) = state
        .workspace
        .lock()
        .unwrap()
        .as_ref()
        .map(|ws| ws.folder.root.clone())
    else {
        return;
    };
    let mut added = false;
    {
        let mut pending = state.pending_disk_changes.lock().unwrap();
        for entry in files {
            let Some(uri_str) = entry.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let Ok(uri) = Url::parse(uri_str) else {
                continue;
            };
            let Ok(raw_path) = uri.to_file_path() else {
                continue;
            };
            // Convert to the root's path form (RFC 0022).
            let Some(path) = to_root_form(&root, &raw_path) else {
                continue;
            };
            added |= pending.insert(path);
        }
    }
    if added {
        signal_reindex(state);
    }
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
    let raw_path = uri.to_file_path().ok()?;
    let workspace_guard = state.workspace.lock().unwrap();
    let ws = workspace_guard.as_ref()?;
    // Convert to the root's path form so the lookup matches indexed
    // document paths regardless of the client's symlink form (RFC 0022).
    let path = to_root_form(&ws.folder.root, &raw_path)?;
    let document = ws
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

// --- Background reindexer (RFC 0017) + disk-change reconciliation (RFC 0022) ---

/// Shared state for the background reindexer: the workspace/graph cells,
/// the stdout guard, the diagnostics state, the pending disk changes, the
/// open documents, and the per-run URI options. Cheap to clone (`Arc`s +
/// `Clone`) into the reindexer thread.
struct ReindexerShared {
    workspace: Arc<Mutex<Option<Workspace>>>,
    graph: Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: Arc<Mutex<()>>,
    diagnostics_state: Arc<Mutex<std::collections::HashMap<std::path::PathBuf, Vec<Value>>>>,
    pending_disk_changes: Arc<Mutex<std::collections::HashSet<PathBuf>>>,
    open_documents: Arc<Mutex<HashMap<Url, String>>>,
    uri_opts: UriOptions,
}

/// Spawn the debounced background reindexer. Called from `initialize_state`
/// after the initial (synchronous) index. Clones the shared `Arc`s and a copy
/// of `uri_opts` into the new thread.
fn spawn_reindexer(state: &mut ServerState) {
    let (reindex_tx, reindex_rx) = mpsc::channel::<()>();
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let shared = ReindexerShared {
        workspace: Arc::clone(&state.workspace),
        graph: Arc::clone(&state.graph),
        stdout_guard: Arc::clone(&state.stdout_guard),
        diagnostics_state: Arc::clone(&state.diagnostics_state),
        pending_disk_changes: Arc::clone(&state.pending_disk_changes),
        open_documents: Arc::clone(&state.open_documents),
        uri_opts: state.uri_opts.clone(),
    };
    let handle = thread::spawn(move || {
        reindexer_loop(shared, reindex_rx, shutdown_rx);
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

/// Stop the filesystem watcher (RFC 0022): dropping the watcher disconnects
/// the event channel, the watcher thread's `recv` loop ends, and the join
/// returns. Idempotent and safe to call when no watcher is running.
fn stop_watcher(state: &mut ServerState) {
    state.watcher.take();
    if let Some(handle) = state.watcher_handle.take() {
        let _ = handle.join();
    }
}

/// Stop both background threads (reindexer + watcher). Called on every
/// `run_server` exit path and before re-spawning on re-initialization.
fn stop_background(state: &mut ServerState) {
    stop_watcher(state);
    stop_reindexer(state);
}

/// Debounce loop: coalesce a burst of edit signals into one re-index that
/// fires `REINDEX_DEBOUNCE` after the *last* signal. While dirty, the wait is
/// capped at `REINDEX_MAX_WAIT` so a continuous stream of changes (sync
/// daemon, long git operation) cannot starve the re-index indefinitely
/// (RFC 0022). Exits on shutdown or when the signal channel is disconnected.
fn reindexer_loop(
    shared: ReindexerShared,
    reindex_rx: mpsc::Receiver<()>,
    shutdown_rx: mpsc::Receiver<()>,
) {
    let mut dirty_since: Option<Instant> = None;
    loop {
        let wait = match dirty_since {
            Some(since) => REINDEX_DEBOUNCE.min(REINDEX_MAX_WAIT.saturating_sub(since.elapsed())),
            None => REINDEX_DEBOUNCE,
        };
        match reindex_rx.recv_timeout(wait) {
            Ok(()) => {
                dirty_since.get_or_insert(Instant::now()); // an edit happened
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty_since.is_some() {
                    perform_reindex(&shared);
                    dirty_since = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
    }
}

/// Perform one re-index: reconcile pending disk changes (RFC 0022), snapshot
/// the workspace (brief lock), run `resolve_links` with NO lock held, swap
/// the graph in, then publish the resulting diagnostics.
fn perform_reindex(shared: &ReindexerShared) {
    freshness::reconcile_pending_changes(
        &shared.workspace,
        &shared.open_documents,
        &shared.pending_disk_changes,
    );
    let input = {
        let ws = shared.workspace.lock().unwrap();
        ws.as_ref().map(|ws| {
            let mut input = ResolveInput::from_workspace(ws);
            input.uri_opts = shared.uri_opts.clone();
            input
        })
    };
    let Some(input) = input else {
        return;
    };
    let new_graph = resolve_links(input);
    *shared.graph.lock().unwrap() = Some(new_graph);
    if let Some(current) = shared.graph.lock().unwrap().as_ref() {
        publish_diagnostics_for_graph(
            current,
            shared.diagnostics_state.as_ref(),
            shared.stdout_guard.as_ref(),
        );
    }
}

// --- Filesystem watcher (RFC 0022) -----------------------------------------

/// Spawn the filesystem watcher on the workspace root. The watcher itself
/// (which owns the event channel's sender) stays in `ServerState`; the
/// spawned thread only consumes events. Stopping means dropping the watcher
/// from `stop_watcher`, which disconnects the channel and ends the thread.
/// If the watcher cannot start, the server logs a warning and continues
/// with the editor-notification mechanisms only.
fn spawn_watcher(state: &mut ServerState, root: &Path) {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!("file watcher unavailable: {error}; disk changes outside the editor will not be tracked");
            return;
        }
    };
    if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
        tracing::warn!("file watcher failed to watch {}: {error}", root.display());
        return;
    }
    let pending_disk_changes = Arc::clone(&state.pending_disk_changes);
    let reindex_tx = state.reindex_tx.clone();
    let root = root.to_path_buf();
    // The watcher stays in `ServerState` (it owns the event channel's
    // sender); the thread only consumes `rx`. Dropping the watcher from
    // `stop_watcher` disconnects the channel and ends the thread.
    let handle = thread::spawn(move || {
        watcher_loop(rx, pending_disk_changes, reindex_tx, root);
    });
    state.watcher = Some(watcher);
    state.watcher_handle = Some(handle);
}

/// Watcher thread: deliberately dumb. For each event path it records the
/// path in the pending-disk-changes set and signals the reindexer, which
/// reconciles against disk after the debounce window. Takes no workspace
/// locks. Exits when the event channel disconnects (the watcher was dropped
/// by `stop_watcher`).
fn watcher_loop(
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    pending_disk_changes: Arc<Mutex<std::collections::HashSet<PathBuf>>>,
    reindex_tx: Option<mpsc::Sender<()>>,
    root: PathBuf,
) {
    for result in rx {
        let Ok(event) = result else {
            continue;
        };
        let mut added = false;
        {
            let mut pending = pending_disk_changes.lock().unwrap();
            for path in &event.paths {
                // Skip `.git` internals (churn during commits/checkout).
                if path.iter().any(|component| component == ".git") {
                    continue;
                }
                // Convert to the root's path form: FSEvents reports
                // canonical paths while the root may not be canonical
                // (RFC 0022).
                if let Some(path) = to_root_form(&root, path) {
                    added |= pending.insert(path);
                }
            }
        }
        if added && let Some(tx) = &reindex_tx {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod reindexer_tests {
    use super::*;

    /// `perform_reindex` with no workspace is a no-op: the graph cell is left
    /// untouched (still `None`) and nothing is written to stdout.
    #[test]
    fn perform_reindex_without_workspace_is_noop() {
        let shared = ReindexerShared {
            workspace: Arc::new(Mutex::new(None)),
            graph: Arc::new(Mutex::new(None)),
            stdout_guard: Arc::new(Mutex::new(())),
            diagnostics_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_disk_changes: Arc::new(Mutex::new(std::collections::HashSet::new())),
            open_documents: Arc::new(Mutex::new(HashMap::new())),
            uri_opts: UriOptions::default(),
        };
        perform_reindex(&shared);
        assert!(shared.graph.lock().unwrap().is_none());
    }

    /// A default `ServerState` has no reindexer or watcher running (all
    /// handles `None`) and an empty graph cell — the shared-state guard is
    /// additive.
    #[test]
    fn default_state_has_no_reindexer() {
        let state = ServerState::default();
        assert!(state.reindex_tx.is_none());
        assert!(state.reindex_shutdown.is_none());
        assert!(state.reindex_handle.is_none());
        assert!(state.watcher.is_none());
        assert!(state.watcher_handle.is_none());
        assert!(state.graph.lock().unwrap().is_none());
    }

    /// `stop_background` is a safe no-op when no background threads are
    /// running (it must not panic or block on missing handles).
    #[test]
    fn stop_background_without_running_is_noop() {
        let mut state = ServerState::default();
        stop_background(&mut state);
        assert!(state.reindex_handle.is_none());
        assert!(state.watcher_handle.is_none());
    }

    /// Bounded debounce (RFC 0022): a continuous stream of signals (a sync
    /// daemon, a long git operation) must not starve the re-index
    /// indefinitely — while dirty, the reindexer re-indexes within
    /// `REINDEX_MAX_WAIT` even though the quiet window never elapses.
    #[test]
    fn reindexer_reindexes_under_continuous_signal_stream() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.md"), "# A\n").unwrap();
        let workspace = discover_workspace(
            WorkspaceInput::Path(temp.path().to_path_buf()),
            Some(temp.path()),
        )
        .unwrap();
        let workspace: Arc<Mutex<Option<Workspace>>> =
            Arc::new(Mutex::new(Some(workspace)));
        let graph: Arc<Mutex<Option<ConnectionGraph>>> = Arc::new(Mutex::new(None));

        let (tx, rx) = mpsc::channel::<()>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
        let graph_for_test = Arc::clone(&graph);
        let shared = ReindexerShared {
            workspace,
            graph,
            stdout_guard: Arc::new(Mutex::new(())),
            diagnostics_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_disk_changes: Arc::new(Mutex::new(std::collections::HashSet::new())),
            open_documents: Arc::new(Mutex::new(HashMap::new())),
            uri_opts: UriOptions::default(),
        };
        let handle = thread::spawn(move || {
            reindexer_loop(shared, rx, shutdown_rx);
        });

        // Signal every 100 ms — the 300 ms quiet window never elapses, so
        // only the max-wait cap can trigger the re-index.
        let start = Instant::now();
        let cap = REINDEX_MAX_WAIT + Duration::from_secs(1);
        while graph_for_test.lock().unwrap().is_none() {
            assert!(start.elapsed() < cap, "re-index starved by a continuous signal stream");
            tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed <= REINDEX_MAX_WAIT + Duration::from_millis(500),
            "re-index took {elapsed:?}; expected within the {REINDEX_MAX_WAIT:?} cap"
        );

        shutdown_tx.send(()).unwrap();
        handle.join().unwrap();
    }
}
