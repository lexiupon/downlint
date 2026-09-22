# RFC 0017: Background, Debounced Re-indexing for the LSP Server

- **Status:** Proposed
- **Date:** 2026-07-11
- **Scope:** `src/lsp/mod.rs`, `src/lsp/handlers/mod.rs`, `src/completion/mod.rs`, `src/lib.rs`
- **Supersedes:** —

## Summary

Move the LSP server's document re-indexing (`resolve_links`) off the synchronous
request loop and into a **debounced background thread**. Today every
`textDocument/didChange` (i.e. every keystroke) triggers a full
`resolve_links` over the entire workspace *inline*, which blocks the server —
and therefore every subsequent request, including completion — for the duration
of the index. On a large workspace this is seconds per keystroke.

Concretely this RFC:

1. Shares `workspace`, `graph`, and the stdout write path between the request
   loop and a background reindexer via `Arc<Mutex<…>>`.
2. Replaces the inline `refresh_graph` on `didOpen`/`didChange`/`didClose` with
   a signal to a **debounced** background thread that performs the re-index and
   publishes the resulting diagnostics.
3. Removes the per-completion deep clone of the whole `ConnectionGraph`
   (`CompletionParams.graph` becomes a reference).
4. Removes the dead `indexing` flag / `is_indexing` helper (referenced only by
   its own unit tests).

The initial index at `initialize` stays **synchronous** (the client is waiting
for the `initialize` response anyway); only *subsequent* re-indexes move to the
background.

## Motivation (measured)

Completion itself is fast; the latency comes from the re-index that a keystroke
triggers. Measured against a real workspace of ~5,900 documents (a primary
vault plus two mounts):

| Operation | Latency |
|---|---|
| Steady-state completion (no recent edit) | ~10 ms |
| `initialize` (first full `resolve_links`) | ~5,150 ms |
| Completion **immediately after a `didChange`** | ~5,050 ms |

The last row is the bug: the `didChange` re-index (~5 s) runs inline in the
single-threaded request loop, so the completion request that follows is queued
behind it. The user types, reaches for completion, and it is frozen for the
whole re-index. Smaller workspaces re-index in well under a second, which is why
the problem is only visible on large vaults.

## Current behavior

The request loop (`run_server` → `handle_request`, `src/lsp/mod.rs`) is
synchronous and single-threaded. The relevant paths:

- `textDocument/didOpen` → `apply_open_change` → `refresh_workspace_doc` →
  `refresh_graph` → **`resolve_links` (inline)**, then `publish_diagnostics`.
- `textDocument/didChange` → `apply_text_change` → `refresh_workspace_doc` →
  `refresh_graph` → **`resolve_links` (inline)**, then `publish_diagnostics`.
- `textDocument/didClose` → `apply_close_change` → `refresh_graph` →
  **`resolve_links` (inline)**, then `publish_diagnostics`.
- `textDocument/completion` → `with_text_position` → `handlers::completion`,
  which builds `CompletionParams { graph: graph.clone(), … }` — a **deep clone
  of the entire graph** on every completion.

`refresh_graph` (`src/lsp/mod.rs`) is:

```rust
fn refresh_graph(state: &mut ServerState) {
    if let Some(workspace) = &state.workspace {
        let mut input = ResolveInput::from_workspace(workspace);
        input.uri_opts = state.uri_opts.clone();
        state.indexing = true;
        let graph = resolve_links(input);
        state.indexing = false;
        state.graph = Some(graph);
    }
}
```

`ServerState` today:

```rust
struct ServerState {
    initialized: bool,
    shutdown_requested: bool,
    workspace: Option<Workspace>,
    graph: Option<ConnectionGraph>,
    indexing: bool,
    open_documents: HashMap<Url, String>,
    uri_opts: UriOptions,
}
```

`resolve_links(ResolveInput) -> ConnectionGraph` is a pure, owned function, and
`Workspace` / `ConnectionGraph` / `Text` are plain data (no `Rc`/`RefCell`), so
all three are `Send` — safe to move across a thread boundary.

## Design

### Shared state

The request loop and the background reindexer both need `workspace` (the
reindexer reads it to build a `ResolveInput`; the loop mutates a document's
`text` on edits), `graph` (the loop reads it for every query; the reindexer
swaps it; `didRenameFiles` mutates it in place), and the stdout write path
(both write LSP messages). Each is wrapped so the two threads never touch the
same data without synchronization:

```rust
struct ServerState {
    initialized: bool,
    shutdown_requested: bool,
    open_documents: HashMap<Url, String>,   // loop-only
    uri_opts: UriOptions,                   // loop-only (cloned into the reindexer)

    // Shared with the background reindexer:
    workspace: Arc<Mutex<Option<Workspace>>>,
    graph: Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: Arc<Mutex<()>>,           // serializes writes to io::stdout()

    // Background reindexer handles (None until `initialize`):
    reindex_tx: Option<mpsc::Sender<()>>,
    reindex_shutdown: Option<mpsc::Sender<()>>,
    reindex_handle: Option<JoinHandle<()>>,
}
```

Notes:

- `stdout_guard` is a `Mutex<()>` — a pure write lock. Every LSP message is
  written to a fresh `io::stdout()` handle **while holding the guard**, so the
  `Content-Length` header + body + flush of one message are atomic with respect
  to the other thread. This avoids requiring `StdoutLock` to be `Send` and
  avoids framing corruption. `write_message` / `write_response` /
  `write_notification` gain a `&Mutex<()>` (or `&Arc<Mutex<()>>`) parameter.
- The `indexing: bool` field is **removed** (see below).
- `ServerState::default()` still works: `Arc<Mutex<Option<_>>>` and the
  `Option` handles all implement `Default`.

### The background reindexer

Spawned once, at the end of `initialize_state`, from a clone of the shared
`Arc`s plus a copy of `uri_opts`. It runs a debounce loop on a plain
`std::thread` (the server is already running under a tokio runtime, but a plain
OS thread is simpler and keeps `resolve_links` off the async executor):

```rust
const REINDEX_DEBOUNCE: Duration = Duration::from_millis(300);

fn reindexer_loop(
    workspace: Arc<Mutex<Option<Workspace>>>,
    graph: Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: Arc<Mutex<()>>,
    uri_opts: UriOptions,
    reindex_rx: mpsc::Receiver<()>,
    shutdown_rx: mpsc::Receiver<()>,
) {
    let mut dirty = false;
    loop {
        match reindex_rx.recv_timeout(REINDEX_DEBOUNCE) {
            Ok(()) => dirty = true,                       // an edit happened; reset the window
            Err(RecvTimeoutError::Timeout) => {
                if dirty {
                    perform_reindex(&workspace, &graph, &stdout_guard, &uri_opts);
                    dirty = false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
    }
}
```

Debounce semantics:

- Each `didChange`/`didOpen`/`didClose` sends one `()` on `reindex_tx`.
- Rapid signals keep resetting the window (`recv_timeout` returns `Ok` and the
  loop waits another full `REINDEX_DEBOUNCE`), so a burst of keystrokes
  coalesces into **one** re-index that fires ~300 ms after the *last* edit.
- While the user types continuously, no re-index runs (the graph is stale but
  the user is editing, not reading diagnostics); when they pause, it catches up.
- A signal that arrives *during* an in-flight re-index is queued and triggers
  one more re-index afterwards (the workspace changed mid-index), still
  coalesced.

`perform_reindex` holds **no lock during `resolve_links`**:

```rust
fn perform_reindex(
    workspace: &Arc<Mutex<Option<Workspace>>>,
    graph: &Arc<Mutex<Option<ConnectionGraph>>>,
    stdout_guard: &Arc<Mutex<()>>,
    uri_opts: &UriOptions,
) {
    // 1. Snapshot the input under a brief workspace lock (clones document text).
    let input = {
        let ws = workspace.lock().unwrap();
        ws.as_ref().map(|ws| {
            let mut input = ResolveInput::from_workspace(ws);
            input.uri_opts = uri_opts.clone();
            input
        })
    };
    let Some(input) = input else { return };

    // 2. Resolve with NO lock held (the expensive part, seconds on large vaults).
    let new_graph = resolve_links(input);

    // 3. Swap the graph in under a brief lock — queries now see the fresh graph.
    *graph.lock().unwrap() = Some(new_graph);

    // 4. Publish diagnostics for the now-current graph (stdout guard per message).
    //    Reading back from the cell (rather than `&new_graph`, which was moved in
    //    step 3) keeps the published diagnostics consistent with what queries see.
    if let Some(current) = graph.lock().unwrap().as_ref() {
        publish_diagnostics_graph(current, stdout_guard);
    }
}
```

The workspace lock is held only across `ResolveInput::from_workspace` (which
clones document text). On a large vault that is a few hundred milliseconds, and
it happens ~300 ms after the user *pauses* — not during typing — so it does not
stall keystrokes in practice.

### Request-loop changes

- `initialize` → `initialize_state` builds the **initial** graph synchronously
  (unchanged), stores it in the shared `graph` cell, and **spawns the
  reindexer** (creating the channels and the thread).
- `initialized` → `publish_diagnostics` for the initial graph (unchanged, still
  in the loop).
- `textDocument/didOpen` → `apply_open_change` updates the shared `workspace`
  document text + `open_documents`, then **sends a reindex signal**. No inline
  `resolve_links`, no inline `publish_diagnostics` (the reindexer publishes).
- `textDocument/didChange` → `apply_text_change` does the same (update text +
  signal).
- `textDocument/didClose` → `apply_close_change` restores the document text from
  disk + removes from `open_documents`, then signals.
- `textDocument/completion`, `hover`, `definition`, `references`,
  `documentSymbol`, `prepareRename`, `rename`, `codeAction` → the `with_*`
  helpers now read the graph via `state.graph.lock().unwrap().as_ref()` (and the
  workspace via `state.workspace.lock().unwrap().as_ref()` where they need a
  document's text) and pass the borrowed `&ConnectionGraph` to the handler. Both
  locks are held only for the duration of the (fast, read-only) handler.

  **Lock ordering / deadlock:** the reindexer never holds the workspace and graph
  locks simultaneously (it snapshots the workspace, releases it, runs
  `resolve_links` lock-free, then swaps the graph). Only the `with_*` helpers
  hold both at once (graph, then workspace). No thread acquires them in the
  opposite order, so there is no lock cycle.
- `workspace/didRenameFiles` → **unchanged**: it mutates the shared graph in
  place under the graph lock (`apply_file_renames`) and publishes diagnostics in
  the loop. It does **not** signal the reindexer (see "Known limitation" below).
- `shutdown` / `exit` and the end-of-stdin path → **stop and join the
  reindexer** before returning the exit code (see "Shutdown").

`refresh_graph` and `refresh_workspace_doc` are removed; their bodies are split
between the `apply_*_change` functions (workspace text update + signal) and the
reindexer (the actual `resolve_links`).

### Completion clone removal

`CompletionParams.graph` changes from an owned `ConnectionGraph` to a
`&'a ConnectionGraph` (the struct gains a lifetime parameter), and
`complete_at` takes `&CompletionParams`. The handler passes the borrowed graph
with no clone:

```rust
let items = complete_at(&CompletionParams {
    path,
    source_text: text.as_str().to_string(),
    cursor_offset: offset,
    graph,            // &ConnectionGraph — no clone
    style,
    max_candidates,
});
```

`complete_docs` / `complete_headings` / `complete_tags` already take
`&CompletionParams` and only read `params.graph`, so they are unaffected. The
only other references are the `src/lib.rs` re-export and the single handler call
site — no test call sites.

### `indexing` flag removal

`indexing: bool`, the `is_indexing` helper, and the `indexing_guard_tests`
module are removed. `is_indexing` is not called by any production code (only by
its own tests); it was a placeholder for RFC 0009 rename guards that never got
wired. If rename handlers later need an "is a re-index in flight" guard, they
can use an `AtomicBool` shared with the reindexer — out of scope here.

### Shutdown

`run_server` stops and joins the reindexer before returning, on **every** exit
path (the `exit` method and the end-of-stdin fallthrough). `initialize_state`
also stops any *existing* reindexer before spawning a new one, so a client that
re-sends `initialize` does not leak the first thread:

```rust
fn stop_reindexer(state: &mut ServerState) {
    if let Some(tx) = state.reindex_shutdown.take() {
        let _ = tx.send(());
    }
    if let Some(handle) = state.reindex_handle.take() {
        let _ = handle.join();
    }
}
```

If a re-index is in flight when `exit` arrives, the join waits for it to finish
(up to the re-index duration). That is a rare, one-time exit cost and never
corrupts state; the reindexer performs no partial swap (it swaps the whole
graph in one locked step).

## Behavior changes

- **Diagnostics timing:** diagnostics now refresh ~300 ms after the user stops
  editing, rather than on every keystroke. This matches how most editors feel
  and is the intended trade-off for responsive typing.
- **Query staleness while typing:** `completion`/`hover`/`definition`/etc. read
  the graph as of the last *completed* re-index. While actively typing the graph
  can be up to ~300 ms + one re-index stale; it is always current after a pause.
  In practice completion is driven by the document *index* (paths/titles/stems),
  which a body edit rarely changes, so this is not observable for the common
  case.
- **Startup unchanged:** the initial `resolve_links` at `initialize` is still
  synchronous, so first-use latency is the same as today.
- **Completion:** identical results, minus the deep clone (faster on large
  graphs).

## Non-goals

- Making the *initial* index asynchronous (a possible follow-up; the client is
  blocked on the `initialize` response regardless).
- Incremental re-indexing (re-resolving only the changed document). The
  background + debounce approach already removes the interactive stall;
  incremental indexing is a separate, larger change.
- Publishing only *changed* diagnostics (the reindexer publishes the full set,
  same as today's per-keystroke publish).
- Changing how `didChange` text is applied (`apply_text_change` is unchanged).

## Known limitation (pre-existing, not made worse)

`workspace/didRenameFiles` updates the **graph** in place but not the
**workspace**. The next text-change re-index rebuilds the graph from the
workspace, which does not reflect the rename — so a rename's in-place fix is
dropped on the next edit. This exists in the current synchronous design too
(the next `refresh_graph` has the same effect); the background reindexer does
not change the outcome, only *when* the next rebuild happens. Fixing it (update
the workspace on rename, or re-index on rename) is a separate follow-up.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Data race on `workspace`/`graph`/stdout | All three behind `Arc<Mutex<…>>`; `resolve_links` runs with no lock held, on a private snapshot. |
| Interleaved/corrupted stdout framing | Every message written atomically under `stdout_guard`; each write is header + body + flush. |
| Reindexer outlives the process / writes after exit | `stop_reindexer` (signal + join) on every exit path before `run_server` returns. |
| Keystroke stalls while the reindexer snapshots the workspace | Snapshot happens ~300 ms after the user pauses, not during typing; worst case a few hundred ms. |
| Rename vs. reindex conflict | Unchanged from today (see Known limitation); `didRenameFiles` does not signal the reindexer. |
| Flaky timing tests | Thresholds chosen with a large margin (see Test plan); plus deterministic unit tests for the debounce. |

## Test plan

**Unit (debounce + swap):**
- The debounce loop coalesces N rapid signals into one `perform_reindex` call
  (drive a fake `reindex_rx`/`shutdown_rx`, count invocations).
- A shutdown signal terminates the loop promptly.
- `perform_reindex` with no workspace is a no-op (graph untouched).

**Integration (LSP subprocess, extending `tests/lsp_completion_tests.rs`
harness):**
- After a `didChange`, a `textDocument/completion` request is served **quickly**
  (assert round-trip well under the old inline re-index time, with a generous
  margin so it is not flaky), proving the re-index no longer blocks the loop.
- Diagnostics are still published after an edit (a `publishDiagnostics`
  notification arrives within the debounce window + re-index time).
- The existing completion-config tests (style + candidates) still pass
  unchanged.

**Regression:**
- Full `cargo test` green (all 306 existing tests).
- `cargo clippy` / `cargo fmt --check` clean on touched files.
- Manual timing check against a large real workspace: completion right after a
  keystroke drops from ~5 s to ~tens of ms.

## Compatibility

- No LSP protocol changes; no config changes; no CLI changes.
- The only observable change is diagnostics/query timing (refreshes after a
  short pause instead of per keystroke) and faster completion.
- Pre-1.0; released as a minor bump (0.13.0) since it changes runtime behavior.
