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
}

pub async fn run_server(_verbose: u8, wait_for_debugger: bool) -> i32 {
    if wait_for_debugger {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    let mut state = ServerState::default();

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
                    "documentSymbolProvider": true
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
    let graph = resolve_links(ResolveInput::from_workspace(&workspace));
    state.workspace = Some(workspace);
    state.graph = Some(graph);
    state.initialized = true;
    Some(root)
}

fn publish_diagnostics(state: &ServerState, stdout: &mut impl Write) {
    let Some(graph) = &state.graph else {
        return;
    };
    for (path, diagnostics) in handlers::diagnostics(graph) {
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
        state.graph = Some(resolve_links(ResolveInput::from_workspace(workspace)));
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
