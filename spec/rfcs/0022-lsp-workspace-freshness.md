# RFC 0022 — LSP workspace freshness: track created, changed, and deleted files without a restart

**Status**: Accepted (0.15.5)
**Date**: 2026-09-23
**Scope**: LSP server (`src/lsp/mod.rs`), `Workspace` document list
(`src/utils/workspace.rs`), LSP capabilities. No changes to the parser,
resolution rules, slugs, CLI, or diagnostic codes. The background
reindexer (RFC 0017) is reused as the single re-resolve path, extended
with a pre-resolve reconciliation step and a bounded debounce.

---

## 1. Summary

The LSP's workspace is a static snapshot: `initialize` walks the disk once
(`discover_workspace`), and every later re-index (RFC 0017) re-resolves
against that same frozen document list. Editor notifications only update
the *text* of documents already in the list — a file that did not exist at
`initialize` never enters the workspace.

Consequence: the most common fix for a `link/broken` diagnostic — creating
the missing target note — does nothing. The error persists until the LSP is
restarted, which re-runs `discover_workspace`. Deletions are the mirror
image: deleting a target note leaves the link *resolved* until restart.

This RFC makes the shared workspace track the vault's actual state through
three complementary mechanisms, all converging on the existing
"mutate shared workspace → signal the debounced reindexer" flow:

1. **Editor upsert** — `didOpen`/`didChange` for a file not yet in the
   workspace adds it (with the editor's text, even if never saved);
   `didClose` for a file that does not exist on disk removes it (no ghost
   documents from discarded buffers).
2. **File-operation notifications** — `workspace/didCreateFiles` and
   `workspace/didDeleteFiles` record the affected paths for reconciliation.
   Both are advertised via `workspace.fileOperations` capabilities.
3. **Filesystem watcher** — a `notify`-based watcher on the workspace root
   records disk changes made outside the editor (terminal, git, sync tools),
   including deletions and attachments.

Mechanisms 2 and 3 only *record* changed paths; the debounced reindexer
*reconciles* them against disk state just before each re-resolve (one
central, testable reconciliation step — §3.1).

Open editor buffers remain the source of truth: no mechanism ever
overwrites or removes a document that is currently open.

## 2. Motivation

Repro (downlint 0.15.4, Neovim):

1. `index.md` contains `[[missing]]` → `link/broken` diagnostic.
2. User creates `missing.md` (the fix) and opens it.
3. The diagnostic on `index.md` **never clears**. Restarting the LSP clears
   it.

Why: `apply_open_change`/`apply_text_change` call
`update_workspace_doc_text`, which does
`documents.iter_mut().find(|doc| doc.path == path)` — a no-op for any file
created after `initialize`. The reindexer then re-resolves the same stale
list, finds `missing` still absent, and the differential publisher
correctly emits nothing (the diagnostic set is unchanged).

The same gap applies to: files created on disk but never opened in the
editor; files created/edited/deleted outside the editor entirely; and
deletions of any kind (the mirror-image bug — stale *resolved* state).

**Client coverage matters.** Neovim's built-in LSP client does **not** send
`workspace/didCreateFiles`/`didDeleteFiles` (neovim/neovim#26045, open as of
0.10); VS Code, Emacs, and plugins such as `nvim-file-operations` do. So for
Neovim — the primary editor for this bug report — the watcher is what
covers files created on disk but never opened in the editor; the `did*`
handlers are implemented for protocol-compliant clients and are exercised
directly by the test suite. `didOpen`-based upsert (mechanism 1) works in
every client, since `:w`-ing a new buffer in Neovim still opens it.

`spec/linting.md` §6.3 already anticipates this direction: "the `did*`
notifications suffice for v1 indexing" (only `will*` is deferred). The CLI
`check --watch` mode already uses the same `notify` channel-of-events +
debounce pattern in this codebase (`src/cli/check.rs`).

## 3. Design

### 3.1 Shared invariants

- **One re-resolve path, one reconciliation point.** Mechanism 1 mutates the
  shared `Workspace` document list directly (editor text needs no disk
  read). Mechanisms 2 and 3 only append affected paths to a shared
  **pending-disk-changes** set and send the existing reindex signal. The RFC
  0017 debounced reindexer, before each re-resolve, drains the pending set
  and **reconciles** each path against disk (add/update/remove the workspace
  document), then re-resolves and the differential publisher emits what
  changed — including empty lists that clear stale client diagnostics. No
  new publish path. Reconciling at reindex time (not at event time) means
  disk reads happen after the 300 ms debounce, so in-flight writes have
  settled (inotify's `IN_CREATE` fires when a file is *opened*, before its
  content is written — reading at event time would index partial content on
  Linux).
- **Same indexability rules as the initial snapshot.** A file is reconciled
  into the workspace only if `collect_documents` would have indexed it:
  configured markdown extension, no hidden path components, not matched by
  `.gitignore` or `core.ignore`. Implementation: a single
  `ignore::WalkBuilder` rooted at the workspace root, pruned with
  `filter_entry` to the affected path (O(depth) directory visits), so
  gitignore discovery is identical to the initial walk; the same
  extension + `core.ignore` post-filter applies. A deleted file is simply
  removed if present in the workspace (it was indexable when added).
- **Normalized paths.** The workspace root is canonicalized at
  `initialize` (symlink-free, e.g. macOS `/tmp` → `/private/tmp`), and
  incoming document paths (from `didOpen`/`didChange`/`did*Files` and
  watcher events) are canonicalized the same way (parent + filename when
  the file does not exist yet) before any membership or `strip_prefix`
  check. This also fixes the pre-existing exact-`PathBuf`-equality
  mismatch in text updates for clients that canonicalize URIs (VS Code).
- **Bounded debounce (no starvation).** The RFC 0017 debounce resets its
  300 ms window on every signal; editor keystrokes are human-bounded, but a
  sync daemon or long git operation can emit a continuous event stream and
  defer the re-index indefinitely. The reindexer loop caps the wait: while
  dirty it polls at `min(300 ms, time remaining until 2 s)` and re-indexes
  when either the quiet window elapses or the 2 s cap is reached.
- **Open buffers win.** A document whose URI is in `open_documents` is
  never overwritten from disk and never removed, by any mechanism.
  (External edits to an open file are the editor's reload problem, standard
  LSP behavior.) `open_documents` moves from `ServerState` into
  `Arc<Mutex<HashMap<Url, String>>>` so the reindexer's reconciliation can
  consult it.
- **Root-scoped.** Only files under the workspace root are tracked. A
  markdown file opened in the editor but outside the root is ignored, as
  today; pending paths outside the root are dropped at reconciliation.
- **`DocumentSource::Disk` for upserted docs.** An upserted document is a
  vault file whose in-memory text is simply fresher than disk — exactly the
  state every edited document already has (text updated in place, source
  unchanged). `source` continues to drive only stdin-mode detection.

### 3.2 Mechanism 1 — editor upsert (`didOpen` / `didChange` / `didClose`)

- `didOpen`/`didChange`: after storing the text in `open_documents`, if the
  URI's path is not in `workspace.folder.documents` **and** is under the
  root, insert a `WorkspaceDocument` (path, `rel_path = path.strip_prefix(root)`,
  editor text, `source: Disk`) and keep the list sorted by `rel_path`.
  Then signal the reindexer (already done today). An upserted file that is
  later reconciled from disk (mechanisms 2/3) is skipped while open — the
  buffer text stays authoritative.
- `didClose`: remove from `open_documents` (already done), then:
  - file **exists** on disk → restore text from disk (current behavior);
  - file **does not exist** on disk → **remove** the document from the
    workspace (the buffer was a never-saved creation; keeping it would
    leave a ghost that resolves links to a nonexistent file).
  - signal the reindexer (already done).
- A file that was in the initial snapshot but deleted on disk while open is
  removed on close by the same rule — correct, since the file is gone.

### 3.3 Mechanism 2 — `workspace/didCreateFiles` / `workspace/didDeleteFiles`

- Capabilities: advertise `didCreate` and `didDelete` with the same filters
  as the existing `didRename` (markdown glob + catch-all).
- Both handlers are thin: parse the URI list, append each path to the
  pending-disk-changes set, signal the reindexer. Reconciliation (§3.1)
  does the rest — a created file is read from disk and upserted (skipped
  while open in the editor); a deleted file is removed (kept while open;
  `didClose` ghost-removal handles it later). Non-markdown paths need no
  workspace mutation (attachments are stat'd at resolve time) but still
  trigger the re-index.
- Per `spec/linting.md` §6.3, `will*` variants remain out of scope.
- **Interaction with `didRenameFiles` (RFC 0009).** An editor rename fires
  `didRenameFiles` (graph mutated in place today) *and* `didCreate` +
  `didDelete` for the same file. Reconciliation removes the old path and
  upserts the new one from disk, which also repairs a pre-existing
divergence: without it, the next re-index would rebuild the graph from the
  stale workspace and silently revert the rename. A renamed *open* file is
  keyed by its old URI in `open_documents` until the editor's next
  `didChange`/`didOpen` under the new URI, so the new path reconciles from
  disk in the interim — correct, since the rename wrote the same content to
  disk.

### 3.4 Mechanism 3 — filesystem watcher

- A `notify::RecommendedWatcher` (recursive, workspace root) runs on its own
  thread, spawned at `initialize` alongside the reindexer. The thread owns
  the watcher; its event sender (`mpsc::Sender`) and `JoinHandle` live in
  `ServerState` (`watcher_tx`, `watcher_handle`). `stop_watcher` drops the
  sender (the thread's `recv()` returns `Disconnected`, the thread exits,
  dropping the watcher) and joins — called on the same paths as
  `stop_reindexer`: the `exit` arm, stdin-EOF, and before re-spawning on
  re-initialization. (The `shutdown` request does not stop background
  threads; the server must stay responsive until `exit`.) If the watcher
  fails to start (platform error), the server logs a warning and continues
  with mechanisms 1+2 only.
- **Re-initialization.** `initialize_state` stops the old watcher and
  reindexer, creates a fresh pending-disk-changes set, clears
  `open_documents` (the workspace is re-discovered; stale open state would
  otherwise shield old paths from reconciliation), and re-spawns both with
  the new shared state.
- The watcher thread is deliberately dumb: for each event path it skips
  paths under `.git/`, appends the path to the pending-disk-changes set, and
  signals the reindexer. It takes no workspace locks.
- Reconciliation of a pending path (done by the reindexer, §3.1):
  - **file with a configured markdown extension**: if open in the editor →
    skip; else if it exists on disk → upsert (read text, add or update);
    else → remove from the workspace if indexed;
  - **file with any other extension** (attachments, etc.): no workspace
    mutation — the reindex re-stats disk at resolve time;
  - **directory**: reconcile the subtree — the pruned walk (§3.1) yields
    every indexable file under it; add/update those, and remove workspace
    documents under the subtree that no longer exist on disk and are not
    open (covers batch creates, moves, and FSEvents' directory-granular
    events).
  - the RFC 0017 debounce (300 ms, 2 s cap) coalesces bursts before any of
    this runs.
- Mounts need no watcher: `ResolveInput::from_workspace` already re-reads
  every mount root from disk on each re-index (`load_mount_documents`).
- downlint never writes files in server mode, so the watcher cannot be
  triggered by its own output.

### 3.5 Locking

No new lock nesting. The watcher thread touches only the
pending-disk-changes set (its own mutex) and the reindex signal channel.
The reindexer's reconciliation does all disk I/O (the pruned walk, file
reads) **without** holding the workspace lock, collects the resulting
mutations, then applies them under the `workspace` lock — and consults
`open_documents` under its own lock, released before taking `workspace`
(never the reverse, and never held together with `graph` or
`diagnostics_state`). The reindexer's existing order (workspace → release →
graph → diagnostics_state → stdout_guard) is unchanged; no cycle is
introduced. (Pre-existing, unchanged: `perform_reindex` holds the workspace
lock while `from_workspace` walks mount roots — this RFC does not widen
that window.)

## 4. Behavior matrix

| Scenario | Before | After |
|---|---|---|
| `[[missing]]` broken; create `missing.md` **unsaved** in editor (`didOpen`) | stale until restart | **cleared** (buffer indexed) |
| `[[missing]]` broken; create `missing.md` on disk via editor, never open it | stale until restart | **cleared** (`didCreateFiles` in compliant clients; watcher in Neovim) |
| `[[missing]]` broken; create `missing.md` in terminal / git / sync tool | stale until restart | **cleared** (watcher) |
| Close a never-saved new buffer | n/a (never indexed) | ghost removed; link broken again |
| Link resolved; delete target on disk (any source) | stays resolved until restart | **`link/broken` reappears** |
| Attachment link resolved; delete the image on disk | stays resolved until restart | **`link/broken` reappears** (re-stat) |
| Edit an open buffer while the same file changes externally | n/a | editor text wins (no overwrite) |
| Open a markdown file outside the workspace root | ignored | ignored (unchanged) |
| Edit an existing doc's link to an existing target | cleared | cleared (unchanged, RFC 0017) |

## 5. Alternatives considered

1. **Full re-discovery per event** (what `check --watch` does). Rejected:
   it re-reads the entire vault per debounce window and — fatally — would
   discard unsaved buffer text, regressing the exact case mechanism 1
   fixes.
2. **`didSave`-only upsert.** Rejected: diagnostics already update live
   while typing (RFC 0017); requiring a save to index a new note would be
   an inconsistent, surprising subset of that.
3. **Polling mtimes.** Rejected: `notify` is already a dependency, the CLI
   watch mode proves it, and event-driven is cheaper and faster.
4. **`workspace/willCreateFiles`/`willDeleteFiles`.** Rejected: already
   deferred by `spec/linting.md` §6.3; `did*` suffices because downlint
   never mutates files itself.
5. **Watcher-only (no upsert).** Rejected: the watcher sees only disk
   state and cannot index an unsaved buffer — the primary repro.
6. **Read from disk at event time** (watcher/did* handlers mutate the
   workspace directly). Rejected: inotify's `IN_CREATE` fires when the file
   is opened for writing, before content lands — event-time reads would
   index partial files on Linux; and it would duplicate the
   add/update/remove logic across three call sites instead of one
   reconciliation function.

## 6. Non-goals

- No `will*` file-operation handlers.
- No external-change reload for open buffers (the editor's job).
- No config key to disable the watcher (add if a platform proves
  problematic).
- No change to mounts (already re-read per re-index), stdin mode, the CLI,
  or resolution semantics.
- No tracking of files outside the workspace root.

## 7. Spec changes

- `spec/downlint.md` §3.2 (LSP): text-sync bullet extended — the index
  tracks created/changed/deleted files via editor notifications
  (`didOpen`/`didChange` upsert, `didCreateFiles`/`didDeleteFiles`) and a
  debounced filesystem watcher; open buffers are the source of truth;
  `didClose` drops never-saved buffers.
- `spec/linting.md` §6.3: the `did*`-suffices note updated —
  `didCreateFiles`/`didDeleteFiles` are now implemented; `will*` remains
  deferred.
- `spec/linting.md` §8: amendment-history row.

## 8. Verification

1. New subprocess LSP suite `tests/lsp_freshness_tests.rs` (the shared
   `LspClient` moves to `tests/common/mod.rs` with a timeout-bounded
   reader so "never arrives" assertions cannot hang):
   - `lsp_diagnostic_cleared_when_target_created_unsaved` — mechanism 1
     (didOpen of a new file, no disk write).
   - `lsp_diagnostic_cleared_when_target_created_on_disk` — mechanism 2
     (file written to disk + `didCreateFiles`, never opened).
   - `lsp_ghost_doc_removed_on_close` — mechanism 1 (create unsaved,
     `didClose` → link broken again).
   - `lsp_diagnostic_cleared_when_target_created_outside_editor` —
     mechanism 3 (file written to disk, no editor notifications).
   - `lsp_diagnostic_reraised_when_target_deleted` — mechanism 3 (delete
     target on disk → `link/broken` reappears).
2. Existing suites unchanged and green, including
   `lsp_diagnostics_cleared_when_link_fixed` (RFC 0017 regression).
3. Unit test for the pruned-walk indexability predicate (hidden file,
   `.gitignore`d file, and `core.ignore`d file are all excluded; a plain
   note is included) and for the bounded debounce (a continuous signal
   stream still re-indexes within the 2 s cap).
4. Full `cargo test` + `cargo clippy --all-targets` (no new warnings).
