//! Disk-change reconciliation for the LSP workspace (RFC 0022).
//!
//! The editor upsert path (`didOpen`/`didChange`) mutates the shared
//! workspace directly. File-operation notifications (`didCreateFiles` /
//! `didDeleteFiles`) and the filesystem watcher only record affected paths
//! in a shared pending set; the debounced reindexer drains that set and
//! calls [`reconcile_pending_changes`] just before each re-resolve, so disk
//! reads happen after the debounce window and in-flight writes have settled.

use crate::utils::{
    DocumentSource, Text, Workspace, WorkspaceDocument, indexable_paths_under, to_root_form,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use url::Url;

/// Reconcile the shared workspace with disk state for the pending change
/// paths (RFC 0022).
///
/// All disk I/O (pruned walks, file reads) happens **without** the
/// workspace lock; the resulting mutations are applied under it. Open
/// editor buffers are never overwritten or removed — the editor's text is
/// authoritative while a document is open.
pub fn reconcile_pending_changes(
    workspace: &Arc<Mutex<Option<Workspace>>>,
    open_documents: &Arc<Mutex<HashMap<Url, String>>>,
    pending: &Arc<Mutex<HashSet<PathBuf>>>,
) {
    let paths: HashSet<PathBuf> = {
        let mut guard = pending.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    if paths.is_empty() {
        return;
    }

    // Snapshot the metadata needed to decide mutations (brief lock).
    let (root, config, indexed) = {
        let guard = workspace.lock().unwrap();
        let Some(ws) = guard.as_ref() else {
            return;
        };
        (
            ws.folder.root.clone(),
            ws.config.clone(),
            ws.folder
                .documents
                .iter()
                .map(|doc| doc.path.clone())
                .collect::<HashSet<_>>(),
        )
    };

    // Paths open in the editor, in the root's path form (RFC 0022).
    let open: HashSet<PathBuf> = open_documents
        .lock()
        .unwrap()
        .keys()
        .filter_map(|uri| uri.to_file_path().ok())
        .filter_map(|path| to_root_form(&root, &path))
        .collect();

    // Desired on-disk state: every indexable file touched by a pending path,
    // with its current disk text. A pending path that no longer exists
    // (deleted file) contributes nothing here — removal is handled below.
    let mut desired: HashMap<PathBuf, String> = HashMap::new();
    for path in &paths {
        if !path.starts_with(&root) {
            continue;
        }
        let files = if path.is_dir() {
            indexable_paths_under(&root, path, &config)
        } else if path.is_file() {
            // The pruned walk yields the file itself iff it passes the same
            // indexability rules as the initial snapshot.
            indexable_paths_under(&root, path, &config)
        } else {
            Vec::new()
        };
        for file in files {
            if open.contains(&file) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&file) {
                desired.insert(file, text);
            }
        }
    }

    // Compute the mutations.
    let mut to_add: Vec<WorkspaceDocument> = Vec::new();
    let mut to_update: Vec<(PathBuf, String)> = Vec::new();
    for (path, text) in &desired {
        if indexed.contains(path) {
            to_update.push((path.clone(), text.clone()));
        } else {
            let rel_path = path
                .strip_prefix(&root)
                .unwrap_or(path.as_path())
                .to_path_buf();
            to_add.push(WorkspaceDocument {
                path: path.clone(),
                rel_path,
                text: Text::new(text),
                source: DocumentSource::Disk,
            });
        }
    }
    let mut to_remove: Vec<PathBuf> = Vec::new();
    for path in &paths {
        if !path.starts_with(&root) {
            continue;
        }
        let is_dir = path.is_dir();
        let candidates = indexed
            .iter()
            .filter(|doc| (is_dir && doc.starts_with(path)) || (!is_dir && *doc == path))
            .collect::<Vec<_>>();
        for doc in candidates {
            if !doc.exists() && !open.contains(doc) && !desired.contains_key(doc) {
                to_remove.push(doc.clone());
            }
        }
    }

    if to_add.is_empty() && to_update.is_empty() && to_remove.is_empty() {
        return;
    }

    // Apply under the workspace lock.
    let mut guard = workspace.lock().unwrap();
    let Some(ws) = guard.as_mut() else {
        return;
    };
    let documents = &mut ws.folder.documents;
    for (path, text) in to_update {
        if let Some(doc) = documents.iter_mut().find(|doc| doc.path == path) {
            doc.text = Text::new(text);
        }
    }
    for path in to_remove {
        documents.retain(|doc| doc.path != path);
    }
    for doc in to_add {
        if documents.iter().any(|existing| existing.path == doc.path) {
            continue;
        }
        // Keep the list sorted by rel_path (as `collect_documents` does).
        let pos = documents
            .binary_search_by(|existing| existing.rel_path.cmp(&doc.rel_path))
            .unwrap_or_else(|err| err);
        documents.insert(pos, doc);
    }
}

/// Insert or update a workspace document from editor text (RFC 0022).
///
/// Called from `didOpen`/`didChange`: if the document is already indexed its
/// text is replaced; if it is under the workspace root and not yet indexed
/// it is inserted (keeping the `rel_path` sort order). Both operations require
/// an extension in the effective `core.file_extensions`, just like discovery
/// (RFC 0024). Editor language IDs and disk existence do not determine document
/// eligibility; otherwise eligible hidden/ignored editor buffers remain allowed.
/// The incoming path is converted to the root's path form first, so clients
/// that canonicalize URIs (or not) both match. Paths outside the root are ignored.
pub fn upsert_workspace_doc(workspace: &Arc<Mutex<Option<Workspace>>>, path: &Path, text: &str) {
    let mut guard = workspace.lock().unwrap();
    let Some(ws) = guard.as_mut() else {
        return;
    };
    let Some(path) = to_root_form(&ws.folder.root, path) else {
        return;
    };
    // Guard updates as well as insertions: open-buffer bookkeeping must not
    // promote attachments into the Markdown resolution document set.
    let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
        return;
    };
    if !ws
        .config
        .core
        .file_extensions
        .iter()
        .any(|value| value == ext)
    {
        return;
    }
    if let Some(doc) = ws.folder.documents.iter_mut().find(|doc| doc.path == path) {
        doc.text = Text::new(text);
        return;
    }
    let rel_path = path
        .strip_prefix(&ws.folder.root)
        .unwrap_or(path.as_path())
        .to_path_buf();
    let doc = WorkspaceDocument {
        path: path.clone(),
        rel_path: rel_path.clone(),
        text: Text::new(text),
        source: DocumentSource::Disk,
    };
    let pos = ws
        .folder
        .documents
        .binary_search_by(|existing| existing.rel_path.cmp(&rel_path))
        .unwrap_or_else(|err| err);
    ws.folder.documents.insert(pos, doc);
}
