# Downlint Implementation Spec

## Goal

Reimplement the original [Marksman](https://github.com/artempyanykh/marksman) - a Markdown Language Server Protocol (LSP) originally written in F# - as **Downlint** in Rust. This spec documents the original F# implementation of marksman for reference and faithful reproduction.

---

## 1. Architecture Overview

### 1.1 Components

```
downlint (CLI binary)
├── `downlint` / `downlint check`  → Standalone diagnostic checker (default)
├── `downlint server`              → LSP server (stdin/stdout)
└── LSP Protocol Layer            → JSON-RPC transport + LSP type system
```

### 1.2 Core Data Flow

```
Markdown Text
    → Parse (comrak + raw-source sidecar scanner)
    → CST (Concrete Syntax Tree)
    → AST (Abstract Syntax Tree)
    → Symbol Occurrences (Defs, Refs, Tags with source ranges)
    → Connection Graph (resolved/unresolved references)
    → LSP Features (completion, definition, references, diagnostics, etc.)
```

### 1.3 Workspace Model

```
Workspace
├── User Config (~/.config/downlint/config.toml)
├── Primary Folders (from LSP workspace folders or .downlint.toml detection)
│   └── Folder { docs, attachments, config, connection graph }
└── Extra Folders (declared in .downlint.toml extra_folders)
    └── Folder { docs, attachments }
```

- **MultiFile Folder**: Standard folder with many markdown documents. Full feature set: cross-file diagnostics, completion, rename, etc.
- **SingleFile Folder**: Single document outside any workspace root. Limited features:
  - ✅ Hover (intra-doc only)
  - ✅ Wiki-link completion (intra-doc headings only)
  - ❌ No cross-file diagnostics (broken links, ambiguous links)
  - ❌ No completion for cross-doc refs
  - ❌ No rename across files
- **Workspace Folders**: Evict single-file folders they enclose
- **Extra Folders**: Loaded from primary folder config; cross-folder resolution fallback

### 1.4 Server Startup Flow (LSP)

When `downlint server` is invoked, it must follow the LSP lifecycle. The server
cannot know the workspace roots, project config, client capabilities, text sync kind,
or file-watcher strategy until the client sends `initialize`.

**Phase 0 — Process bootstrap (before LSP `initialize`)**:
1. Parse server CLI arguments (`--verbose`, `--wait-for-debugger`, global flags).
2. Initialize stderr logging/tracing only. Never write logs to stdout because stdout is
   reserved for JSON-RPC framing.
3. Create the JSON-RPC stdin/stdout transport.
4. Enter the pre-initialized message loop. Before `initialize`, accept only:
   - `initialize`
   - `shutdown`
   - `exit`
   Other requests receive `ServerNotInitialized` / `InvalidRequest`.

**Phase 1 — Handle `initialize` request**:
1. Read `InitializeParams` and client capabilities.
2. Negotiate position encoding (default UTF-16; see §2.8) and text sync mode.
3. Determine workspace roots from, in order:
   - `workspaceFolders` if present and non-empty
   - `rootUri`
   - `rootPath`
   - current directory as a last resort
4. Load and validate user config.
5. For each workspace root, find `.downlint.toml`, load and validate project config,
   and merge configs: `initializationOptions` > project > user > defaults (see §9.3).
6. Return `InitializeResult` with only capabilities supported by the effective config
   and the client's advertised capabilities.

**Phase 2 — Handle `initialized` notification**:
1. Register dynamic file watchers via `client/registerCapability` if the client supports
   dynamic registration. Use `RelativePattern` rooted at each workspace folder.
2. Start the diagnostics manager task and workspace indexing tasks.
3. Scan markdown files matching configured extensions (`**/*.{md,markdown}` by default)
   while respecting `.gitignore` (§10.8).
4. Parse each document into comrak AST + sidecar CST (§3), build AST and symbol occurrences,
   then resolve links into the connection graph.
5. Publish initial diagnostics as documents finish indexing. The server remains responsive;
   long workspace scans must not block the JSON-RPC receive loop.

**Phase 3 — Normal message loop**:
1. Process text synchronization, completion, hover, rename, code actions, code lenses,
   semantic tokens, diagnostics, and workspace notifications through registered handlers.
2. Queue document changes through the diagnostics manager channel. Rapid edits are debounced
   and coalesced.
3. Re-index changed documents incrementally and update the connection graph.

**Error handling during startup/initialization**:
- CLI argument error before transport starts → print to stderr and exit with code 2.
- User/project config parse failure during `initialize` → return an LSP error response for
  `initialize`, log to stderr, and exit with code 2.
- Workspace root not found → fall back to current directory and log a warning.
- File read errors during indexing → skip file, log warning to stderr, keep server alive.
- Markdown syntax is best-effort: comrak parses CommonMark-compatible structure and the
  sidecar scanner skips malformed constructs instead of aborting the document.

### 1.5 Key Differences from Marksman

| Aspect | Marksman (F#) | Downlint (Rust) |
|--------|---------------|-----------------|
| **Language** | F# (.NET) | Rust |
| **Parser** | Markdig (.NET library) | comrak (Rust-native) |
| **CLI default** | `marksman server` (LSP) | `downlint check` (diagnostics) |
| **Config file** | `.marksman.toml` | `.downlint.toml` |
| **User config** | `~/.config/marksman/config.toml` | `~/.config/downlint/config.toml` |
| **Wiki-link parsing** | Markdig inline parser extension | comrak + raw-source sidecar scanner |
| **Text sync default** | Full | Full |
| **Severity threshold** | All diagnostics shown | `warning` by default (shows warnings + errors) |
| **Watch mode** | `marksman watch` | `downlint --watch` / `downlint -w` |
| **Auto-fix** | Not supported | `--fix` for safe fixes |

**Design philosophy difference**: marksman prioritizes editor integration (LSP server is the default). Downlint prioritizes CLI usability (diagnostics are the default; LSP server is explicit).

This means:
- `marksman` with no args → hangs waiting for LSP input
- `downlint` with no args → shows diagnostics immediately

For editor users, both work identically when invoked explicitly (`marksman server` / `downlint server`).

### 1.6 Quick Start

The most common commands:

```bash
downlint                    # check current directory for diagnostics (default)
downlint docs/              # check a specific folder
downlint note.md            # check a single file
downlint --format json      # JSON output for CI pipelines
downlint --min-severity error  # only show errors, suppress warnings
downlint --fix              # apply safe fixes such as NBSP-after-heading-marker
downlint --watch            # re-run diagnostics on file changes
echo '# Title' | downlint - # check a single stdin document
downlint server             # start LSP server (for editor integration)
```

See §11 for the full CLI reference and all available flags.

---

## 2. LSP Protocol Layer

### 2.1 Transport

- **Protocol**: JSON-RPC 2.0 (`"jsonrpc": "2.0"`)
- **Framing**: Header-delimited (`Content-Length: N\r\n\r\n{...}`)
- **Streams**: stdin (input) and stdout (output)
- **Message Types**:
  - `Request`: `{ id, method, params }`
  - `Notification`: `{ method, params }` (no id)
  - `Response`: `{ id, result }` or `{ id, error }`
- **Error Codes**: ParseError(-32700), InvalidRequest(-32600), MethodNotFound(-32601), InvalidParams(-32602), InternalError(-32603), ServerError(-32000 to -32099), ServerNotInitialized(-32002), RequestCancelled(-32800), ContentModified(-32801)

### 2.2 LSP v1 Capability Matrix

Downlint advertises only features it actually supports for the current client. Handlers for
unadvertised methods should still return `MethodNotFound` or a clear empty result where the LSP
requires a graceful response.

| Area | Method | v1 status | Notes |
|------|--------|-----------|-------|
| Lifecycle | `initialize` | Implemented | Required first request; returns negotiated capabilities |
| Lifecycle | `initialized` | Implemented | Starts watcher registration and indexing |
| Lifecycle | `shutdown` | Implemented | Request → `null` response; see §2.7 |
| Lifecycle | `exit` | Implemented | Notification; process termination policy in §2.7 |
| Text sync | `textDocument/didOpen` | Implemented | Adds/overlays open document |
| Text sync | `textDocument/didChange` | Implemented | Full sync default; incremental supported after negotiation |
| Text sync | `textDocument/didClose` | Implemented | Removes overlay, reloads disk copy if needed, clears stale diagnostics |
| Text sync | `textDocument/didSave` | Implemented | Triggers re-read/re-index when necessary |
| Text sync | `textDocument/willSave` | Not advertised | No save-assist behavior in v1 |
| Text sync | `textDocument/willSaveWaitUntil` | Not advertised | Do not register a stub capability; clients should not call it |
| Code intelligence | `textDocument/hover` | Implemented | Links, headings, tags, attachments |
| Code intelligence | `textDocument/completion` | Implemented | Trigger chars: `[`, `#`, `(` |
| Code intelligence | `completionItem/resolve` | Implemented | Adds detail/documentation when needed |
| Code intelligence | `textDocument/definition` | Implemented | Returns one or more `Location`s |
| Code intelligence | `textDocument/references` | Implemented | Uses occurrence IDs, not deduplicated symbols |
| Code intelligence | `textDocument/documentHighlight` | Implemented | Same-document highlights |
| Symbols | `textDocument/documentSymbol` | Implemented | Hierarchical headings |
| Symbols | `workspace/symbol` | Deferred | See §16.2 |
| Refactoring/actions | `textDocument/rename` | Implemented | Headings, reference labels, attachments |
| Refactoring/actions | `textDocument/prepareRename` | Implemented | Returns exact editable range |
| Refactoring/actions | `textDocument/codeAction` | Implemented | TOC and create-missing-file actions |
| Refactoring/actions | `codeAction/resolve` | Implemented | Lazily computes workspace edits |
| Code lens | `textDocument/codeLens` | Implemented | v1 includes link/reference counts |
| Code lens | `codeLens/resolve` | Implemented | Lazily resolves counts/details |
| Diagnostics | `textDocument/publishDiagnostics` | Implemented | Push diagnostics for classic clients |
| Diagnostics | `textDocument/diagnostic` | Implemented | Pull diagnostics when client supports it |
| Diagnostics | `workspace/diagnostic` | Implemented | Pull diagnostics for workspace |
| Semantic tokens | `textDocument/semanticTokens/full` | Implemented | Links, headings, tags, frontmatter |
| Semantic tokens | `textDocument/semanticTokens/full/delta` | Implemented | Stable result IDs per document version |
| Semantic tokens | `textDocument/semanticTokens/range` | Implemented | Range-limited tokenization |
| Workspace | `workspace/didChangeWatchedFiles` | Implemented | Re-index create/change/delete events |
| Workspace | `workspace/didChangeConfiguration` | Implemented | Re-merge config and re-index affected roots |
| Workspace | `workspace/didChangeWorkspaceFolders` | Implemented | Add/remove primary folders |
| Workspace | `workspace/didCreateFiles` | Implemented | Update indexes after client-side file creation |
| Workspace | `workspace/didRenameFiles` | Implemented | Update paths and link indexes after rename |
| Workspace | `workspace/didDeleteFiles` | Implemented | Remove docs/attachments and publish clears |
| Workspace | `workspace/willCreateFiles` | Deferred | Not advertised in v1 |
| Workspace | `workspace/willRenameFiles` | Deferred | Not advertised in v1 |
| Workspace | `workspace/willDeleteFiles` | Deferred | Not advertised in v1 |
| Workspace | `workspace/executeCommand` | Implemented | Commands backing code actions |

### 2.3 Text Sync Modes

- **Full**: Entire document content sent on every change (default, most reliable)
- **Incremental**: Only changed ranges sent (with RangeLength)

Effective value: LSP `initializationOptions` → project config → user config → default (`full`).
The server advertises the negotiated mode in `InitializeResult.textDocumentSync`.

### 2.4 Client Capability Negotiation

**InitializeParams** contains:

> **Note**: All LSP JSON keys use `camelCase`. Rust struct fields use `snake_case` with
> `#[serde(rename = "...")]` attributes for wire-level serialization. See the full pattern
> in §2.6.

- `process_id`, `client_info` {name, version}
- `root_path`/`root_uri`, `workspace_folders`
- `initialization_options` (custom per-client, e.g., `preferred_text_sync_kind`)
- `capabilities` (full ClientCapabilities object)

**Server Capabilities** (advertised in `InitializeResult` when supported):

- `positionEncoding`: negotiated value when using LSP 3.17+ (`utf-16` by default)
- `textDocumentSync`: `{ openClose: true, change, save: true }`
  - Do **not** advertise `willSave` or `willSaveWaitUntil` in v1.
- `completionProvider`: `{ triggerCharacters: ["[", "#", "("] }`
- `hoverProvider`: true
- `definitionProvider`: true
- `referencesProvider`: true
- `documentHighlightProvider`: true
- `documentSymbolProvider`: true
- `workspaceSymbolProvider`: false in v1
- `codeActionProvider`: true / resolve provider if client supports `codeAction/resolve`
- `codeLensProvider`: `{ resolveProvider: true }`
- `semanticTokensProvider`: `{ legend, range: true, full: { delta: true } }`
- `renameProvider`: `{ prepareProvider: true }` if client supports prepare rename
- `diagnosticProvider`: document + workspace diagnostics when client supports pull diagnostics
- `workspace.workspaceFolders`: `{ supported: true, changeNotifications: true }`
- `workspace.fileOperations`: advertise `didCreate`, `didRename`, and `didDelete` only; `will*`
  operations are deferred and not advertised in v1.
- `executeCommandProvider`: commands backing code actions (TOC generation, create missing file)

**Not Applicable to Markdown** (never implemented in marksman, not planned for Downlint):

| Method | Reason |
|--------|--------|
| `textDocument/declaration` | Markdown has no "declaration" concept |
| `textDocument/typeDefinition` | Same as above |
| `textDocument/inlayHint` | Not applicable to markdown |
| `textDocument/formatting` | Markdown is whitespace-sensitive |
| `textDocument/signatureHelp` | Not applicable to markdown |

**Deferred** (planned for a future release):

| Method | Reason |
|--------|--------|
| `textDocument/foldingRange` | Partial — heading folding only (H1-H6) |
| `workspace/symbol` | Cross-document symbol search — low value for markdown workflows |

**Concurrency**: The LSP server handles concurrent `textDocument/didChange` requests by
queueing them through the diagnostics manager channel. Rapid edits are coalesced into a
single diagnostic update (200ms debounce). Cancel support is not yet implemented.

**Custom Extensions**:

- `downlint/status` — Custom notification with `{ state: string, doc_count: int }`
  - Not implemented in v1 (LSP clients don't consume this)
  - See §14.1 for excluded features (no plans to implement)

**Client Handling Quirks:**

- VSCode: sends `shutdown` then closes connection (no `exit`) — handle connection close
- Neovim: sends `shutdown` + `exit`; also drops rename edits for unloaded buffers
- Emacs: sends `shutdown` + `exit` then closes
- Incremental sync bugs in some editors — default to `full` sync
- RelativePattern required for extra folder watching — absolute string silently fails in neovim
- Transient broken-link diagnostic after cross-folder rename — clears on `:wa`

### 2.5 File Watching

- Registered after the `initialized` notification via `client/registerCapability` when the
  client supports dynamic registration.
- Glob patterns: `**/*.{md,markdown}` for markdown files by default, expanded by configured
  markdown extensions, plus configured attachment extensions and `.downlint.toml`.
- Uses `RelativePattern` form rooted at each workspace/extra folder (not plain absolute strings)
  for correct cross-folder matching.
- Events: Create, Change, Delete.
- Extra folders get separate watcher registrations.
- If the client does not support dynamic registration, Downlint relies on client-sent static
  `workspace/didChangeWatchedFiles` events and explicit text document notifications.

### 2.6 JSON Serialization

All LSP types use `serde_json` for serialization/deserialization with the following conventions:

**Transport Framing**: JSON-RPC 2.0 over stdin/stdout uses `Content-Length` header framing
(`Content-Length: N\r\n\r\n{...}`). This is handled by the JSON-RPC library; the server
must read from stdin and write to stdout using this protocol.

**Serialization Crate**: `serde` with `derive(Serialize, Deserialize)` — all LSP struct fields
use `snake_case` with `#[serde(rename = "...")]` attributes for wire-level camelCase keys.

**Property Naming**: camelCase — Rust struct fields use `snake_case` with `#[serde(rename = "...")]` attributes for wire-level camelCase keys
**Null Handling**: `skip_serializing_if = "Option::is_none"` — null/None values omitted
**Type Coercion**: Strict — no quoted numbers as numbers, no unquoted numbers as strings

Example:
```rust
#[derive(Serialize, Deserialize)]
struct CompletionParams {
    #[serde(rename = "textDocument")]
    text_document: TextDocumentIdentifier,
    #[serde(rename = "position")]
    position: Position,
}
```

---

### 2.7 Server Shutdown and Graceful Exit

LSP shutdown is a two-message protocol: the client sends a `shutdown` **request**, the
server replies with `result: null`, and the client then sends an `exit` **notification** or
closes the transport. The server never sends `exit`.

**Shutdown request sequence**:

1. **Enter shutting-down state**: Stop accepting new normal requests. In-flight requests may
   finish; new requests other than `exit` receive `InvalidRequest` or `RequestCancelled`.
2. **Quiesce background work**: Stop scheduling new indexing/diagnostic jobs. Cancel or drain
   queued jobs that have not started.
3. **Flush diagnostic clears/updates when possible**: Drain the diagnostics manager briefly and
   send pending `textDocument/publishDiagnostics` notifications. If the client disconnected,
   log the failure and continue.
4. **Close file watchers**: Drop watcher handles and dynamic registration state.
5. **Release workspace state**: Drop parsed documents, indexes, connection graphs, and open-file
   overlays.
6. **Respond to `shutdown`**: Send JSON-RPC response `{ "result": null }`.
7. **Wait for termination**: Exit with code `0` on a subsequent `exit` notification or EOF on the
   LSP input stream after successful shutdown.

**Exit notification policy**:

- `exit` after successful `shutdown` → exit code `0`.
- `exit` before successful `shutdown` → exit code `1` (protocol error), as recommended by LSP.
- Client closes the connection after successful `shutdown` without sending `exit` → exit code `0`
  (common in VSCode).
- Client disconnects before `shutdown` completes → best-effort cleanup, then exit code `1` unless
  shutdown had already succeeded.

**OS signal handling**:

- **Unix (SIGTERM/SIGINT)**: A `tokio::signal` handler triggers the same cleanup steps as shutdown,
  but no LSP response is sent because there may be no requesting client message.
- **Windows (Ctrl+C)**: Tokio maps console Ctrl+C to the same cleanup path.
- **Force kill (SIGKILL/kill -9)**: No graceful cleanup is possible.

**Edge cases**:

- `shutdown` before `initialize` is valid enough to answer with `null` and exit cleanly after
  `exit`/EOF; there may simply be no workspace state to drop.
- If a diagnostic publish is in progress when shutdown begins, it either completes or is cancelled
  as a whole. Never emit partial JSON-RPC messages.
- Shutdown must not block indefinitely on slow indexing. Use bounded timeouts for background task
  joins; dropping in-memory state is acceptable after the response is sent.

**Example shutdown flow**:

```
Client                    Server
  |                         |
  |--- shutdown ----------> |  Enter shutting-down state
  |                         |  Drain/cancel background work
  |                         |  Close watchers and drop state
  |<-- result: null ------- |
  |--- exit --------------> |  Process exits (code 0)
```

**Error handling during shutdown**:

- Diagnostic publish or watcher-close errors are logged to stderr and do not prevent shutdown.
- A panic during cleanup is still a crash and may produce a non-zero exit.

---

### 2.8 LSP Position Encoding and Internal Ranges

Downlint uses **byte offsets internally** and converts to/from LSP positions only at the
protocol boundary.

```rust
struct ByteRange { start: usize, end: usize } // UTF-8 byte offsets, [start, end)
```

**Negotiation**:

- If the client advertises `general.positionEncodings`, prefer `utf-16` when available.
- If `utf-16` is absent but `utf-8` is present, use `utf-8` and include
  `positionEncoding = "utf-8"` in `InitializeResult`.
- If the client does not advertise position encodings, use the LSP default: `utf-16`.
- `utf-32` is not a v1 target unless required by a client; Markdown editor clients overwhelmingly
  support UTF-16 or UTF-8.

**Conversion rules**:

- LSP line/character positions are zero-based.
- Internal line maps store the byte start of each line and the line-ending width (`\n`, `\r\n`, or
  final EOF).
- UTF-16 character counts are computed over Unicode scalar values for the line prefix using
  `char::len_utf16()`; emoji and supplementary-plane characters count as 2.
- UTF-8 position encoding uses byte counts from the start of the line.
- Incoming LSP positions are clamped only when the LSP spec requires it; otherwise invalid
  positions return `InvalidParams`.
- All scanner and parser ranges must align to UTF-8 character boundaries.

**comrak source positions**:

comrak `Sourcepos` values are 1-based line/column positions. In comrak 0.52.0, columns are
byte-based by default and can be character-based with `parse.sourcepos_chars`. Downlint still
normalizes all comrak spans to `ByteRange` and relies on the sidecar scanner for exact inline
component ranges. Do not pass comrak columns directly to LSP responses.

---

## 3. Markdown Parsing Pipeline

### 3.1 Parser Architecture

The original F# marksman used [Markdig](https://github.com/xoofx/markdig). Downlint uses
**comrak plus a raw-source sidecar scanner**. comrak owns CommonMark/GFM block and inline
semantics; the sidecar scanner owns Downlint-specific occurrences and exact source ranges.

**Pinned comrak version for v1**: `comrak = "=0.52.0"`.

Relevant comrak facts for this spec:

- `comrak::nodes::NodeWikiLink` contains only `url: String`; it does not expose document,
  heading, title/display text, embed status, or component ranges.
- Wikilinks are enabled with `extension.wikilinks_title_after_pipe` or
  `extension.wikilinks_title_before_pipe`. `wikilinks_title_after_pipe` treats
  `[[url|link label]]` as URL first and takes precedence if both are enabled.
- comrak does not define Obsidian-style embed semantics for `![[...]]`; Downlint must not
  rely on comrak to identify embeds.
- comrak has no tag syntax for `#tag`.
- comrak emits unresolved shortcut/collapsed reference links as text unless a broken-link
  callback synthesizes a destination, and that still loses the original reference-link form.
- comrak `Sourcepos` is useful for blocks, but inline source ranges are not sufficient for
  LSP edits because Downlint needs subcomponent ranges such as only the wiki doc name, only
  the anchor, or only a reference label.

**Why a sidecar scanner is required**:

A pure comrak AST walk cannot faithfully implement Marksman-style LSP behavior because it loses
or never exposes information Downlint needs:

1. **Exact edit ranges** — rename needs to edit only `section` in `[[doc#section|title]]`, not
   the entire link node.
2. **Syntax form preservation** — full refs `[text][id]`, collapsed refs `[id][]`, shortcut refs
   `[id]`, inline links, and images need different diagnostics and rename edits.
3. **Unresolved references** — broken shortcut/collapsed refs must still be represented as
   occurrences even when comrak leaves them as text.
4. **Embed detection** — `![[asset.png]]` is semantically different from `[[asset.png]]` and
   must be diagnosed/resolved with the same precision.
5. **Tags** — tags are not Markdown syntax and must be found in text while skipping code spans,
   code blocks, autolinks, link destinations, and frontmatter.
6. **LSP position correctness** — all output ranges must be byte-accurate internally and then
   converted to the negotiated LSP encoding (§2.8).

**Data flow**:

```
Input text
    → build LineMap + ByteRange utilities
    → comrak::parse_document()            (CommonMark/GFM AST, block structure, headings)
    → sidecar_scan(raw text, comrak mask) (wiki-links, md links/refs, link defs, tags, ranges)
    → build_cst()                         (merged comrak nodes + scanner occurrences)
    → build_ast()                         (decoded, simplified elements)
    → build_symbol_occurrences()          (defs, refs, tags with occurrence IDs)
    → resolve_occurrences()               (connection graph)
```

**Layer 1 — comrak (CommonMark/GFM)**:

Handles standard Markdown: headings, paragraphs, lists, links that comrak can resolve,
tables, strikethrough, code blocks/spans, frontmatter, footnotes, etc.

```rust
let mut opts = comrak::Options::default();
opts.extension.strikethrough = true;
opts.extension.table = true;
opts.extension.autolink = true;
opts.extension.tasklist = true;
opts.extension.front_matter_delimiter = Some("---".to_owned());

// Downlint scanner owns wikilink semantics in v1. Keep comrak wikilink extensions
// disabled unless a future renderer-specific feature explicitly needs them.
opts.extension.wikilinks_title_after_pipe = false;
opts.extension.wikilinks_title_before_pipe = false;
```

**Layer 2 — Sidecar scanner/lexer**:

The scanner walks the original UTF-8 source and emits `ScanToken`s with `ByteRange`s. It uses
comrak-derived block ranges as an exclusion/masking layer so it does not report links/tags inside
code blocks, raw HTML blocks, or frontmatter unless a feature explicitly opts in.

```rust
enum ScanTokenKind {
    WikiLink(WikiLinkScan),
    MarkdownInlineLink(MdInlineLinkScan),
    MarkdownReference(MdReferenceScan),
    MarkdownLinkDefinition(MdLinkDefScan),
    Tag(TagScan),
}

struct ScanToken {
    id: OccurrenceId,
    kind: ScanTokenKind,
    full_range: ByteRange,
}
```

**Scanner phases**:

1. **Line/block mask construction**
   - Build a `LineMap` and mark ranges for fenced code, indented code, HTML blocks, frontmatter,
     and existing TOC marker regions.
   - Prefer comrak block `Sourcepos` for block classification; use a small raw pre-scan for
     constructs where component-level ranges are needed.
2. **Inline state machine**
   - Walk unmasked text byte-by-byte on UTF-8 character boundaries.
   - Track states: normal text, inline code span, autolink (`<scheme:...>`), HTML tag/comment,
     bracket label, link destination, and escaped character.
   - Never emit nested links; if a candidate overlaps an already accepted link occurrence, keep
     the outer CommonMark-valid occurrence and discard the inner candidate.
3. **Wiki-link scanner**
   - Recognize optional embed bang immediately before `[[`: `![[...]]`.
   - Read until the first unescaped `]]`; nested wiki-links are not parsed recursively.
   - Preserve ranges for: bang, opening/closing delimiters, target, doc segment, heading segment,
     title/display segment, and full link.
4. **Markdown link/reference scanner**
   - Recognize inline links/images: `[text](dest "title")`, `![alt](dest)`.
   - Recognize full refs `[text][label]`, collapsed refs `[label][]`, and shortcut refs `[label]`.
   - Recognize link definitions at block start: `[label]: dest "title"`.
   - Keep raw label text and normalized `LinkLabel` separately.
5. **Tag scanner**
   - Recognize `#[A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)*` in normal text.
   - Require a left boundary of start-of-line or a non-word character, and a right boundary that
     is not another tag character.
   - Do not emit a tag for an ATX heading opener (`# Heading`), inside link destinations, inside
     autolinks/URLs, or inside code spans/blocks.

### 3.2 Wiki-Link Scanning and Enrichment

Downlint's canonical wiki-link model comes from the sidecar scanner, not from
`NodeWikiLink.url`.

**Input**: raw source syntax such as `![[my-doc#section|My Title]]`.

**Parsing logic**:

1. Detect `![[` vs `[[` to set `is_embed` and record `bang_range`.
2. Extract the raw content between delimiters.
3. Split content at the first **unescaped** `|` into `target` and optional display/title.
4. Split `target` at the first **unescaped** `#` into optional document and optional heading.
5. Decode Markdown backslash escapes and wiki percent-encoding per component.
6. Preserve raw and decoded values; diagnostics display decoded wiki text, while edits use raw
   component ranges.

**Examples**:

- `[[doc]]` → `{ doc: Some("doc"), heading: None, title: None, is_embed: false }`
- `[[#section]]` → `{ doc: None, heading: Some("section"), title: None, is_embed: false }`
- `[[doc#section]]` → `{ doc: Some("doc"), heading: Some("section"), title: None, is_embed: false }`
- `[[doc|title]]` → `{ doc: Some("doc"), heading: None, title: Some("title"), is_embed: false }`
- `[[#section|title]]` → `{ doc: None, heading: Some("section"), title: Some("title"), is_embed: false }`
- `[[|title]]` → `{ doc: None, heading: None, title: Some("title"), is_embed: false }`
- `![[asset.png]]` → `{ doc: Some("asset.png"), heading: None, title: None, is_embed: true }`

**Edge cases**:

- Empty heading: `[[T#]]` records a zero-length heading segment and resolves to the document if
  no empty heading exists.
- Escaped delimiter characters: `[[F\#]]` treats `#` as part of the document segment.
- Double-encoded text: `[[%5B%5Bdoc%5D%5D]]` decodes to a document name containing literal
  `[[doc]]`; it is not parsed as a nested wiki-link.
- Malformed or unterminated wiki-links are ignored by the symbol layer but may be surfaced by a
  future syntax diagnostic.

### 3.3 Tag Parsing

Tags are emitted by the sidecar scanner because comrak has no `#tag` syntax.

**Detection**: scan normal text for `#[A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)*`, supporting subtags
like `#doc/rust`.

**Tag Element in CST**:
```rust
struct Tag { name: TextNode }  // e.g., "#rust", "#doc/rust"

enum CstElement {
  // ... other variants ...
  T(Node<Tag>),              // Tag element with source position
}
```

**Tag Element in AST / symbols**:
```rust
struct TagSym { name: String }  // decoded, normalized lowercase
```

**Tag References**: Tags can be referenced in wiki-links (`[[#tag]]`) and markdown links
(`[text](#tag)`). These are tracked as `IntraRef::IntraTag(Slug)` for resolution.

**Edge Cases**:
- `#` inside code blocks or code spans — not detected
- `#tag` with spaces — not a valid tag (`#my tag` → only `#my` detected)
- Multiple tags on one line — each detected independently (`#rust #cli` → two tag elements)
- ATX heading opener — not a tag (`# Heading` opener is ignored)
- Tag in heading text — detected when it is part of the heading content (`# Heading #tag`)

### 3.4 CST (Concrete Syntax Tree)

```rust
// All parser/scanner ranges are UTF-8 byte offsets into the source text.
struct ByteRange { start: usize, end: usize }
type ElementIdx = usize;
type OccurrenceId = u32;

// Rust equivalent of the original F# Node<'A> type. Text is stored directly
// for diagnostics/display, while range is the source of truth for edits.
struct Node<T> {
  id: OccurrenceId,
  text: String,
  range: ByteRange,
  data: T,
}

// Raw text node — used throughout CST for position-annotated text.
struct TextNode { text: String, range: ByteRange }

enum CstElement {
  H(Node<Heading>),          // Heading
  WL(Node<WikiLink>),        // Wiki-link
  ML(Node<MdLink>),          // Markdown link or reference occurrence
  MLD(Node<MdLinkDef>),      // Link definition
  T(Node<Tag>),              // Tag
  YML(TextNode),             // YAML frontmatter
}

struct Heading {
  level: u8,                 // 1-6
  is_title: bool,            // level <= 1 && title_from_heading
  title: TextNode,
  slug: Slug,                // GitHub-compatible generated slug
  disambiguation: Option<String>,  // Duplicate heading ID suffix
  scope: ByteRange,          // Section scope
}

struct WikiLink {
  doc: Option<WikiEncodedNode>,
  heading: Option<WikiEncodedNode>,
  title: Option<WikiEncodedNode>,
  is_embed: bool,
  doc_range: Option<ByteRange>,
  heading_range: Option<ByteRange>,
  title_range: Option<ByteRange>,
}

enum MdLink {
  IL { text: TextNode, dest: TextNode, title: Option<TextNode>, anchor_range: Option<ByteRange> },
  RF { text: TextNode, label: TextNode },       // Full ref: [text][label]
  RC { label: TextNode },                       // Collapsed: [label][]
  RS { label: TextNode },                       // Shortcut: [label]
}

// Raw tag — position-annotated, before decoding.
struct Tag { name: TextNode }

struct MdLinkDef { label: TextNode, url: UrlEncodedNode, title: Option<TextNode> }

struct Cst {
  elements: Vec<CstElement>,
  child_map: HashMap<ElementIdx, Vec<ElementIdx>>,  // Heading hierarchy (by index)
}
```

### 3.5 AST (Abstract Syntax Tree)

CST → AST conversion:

- Decodes URL/Wiki encoded strings
- Splits URLs at `#` for anchors
- Filters out empty text elements
- Filters out YAML frontmatter
- Converts MdLink variants to simplified forms

```rust
// AST-level elements — decoded, simplified, ready for symbol resolution
enum AstElement {
  H(Heading),        // { level, is_title, text, id: Slug }
  WL(WikiLink),      // { doc, heading, title, is_embed, component ranges }
  ML(MdLink),        // { text, url: Option<String>, anchor: Option<String> }
  MR(MdRef),         // Full | Collapsed | Shortcut of string
  MLD(MdLinkDef),    // { label, url, title: Option<String> }
  T(Tag),            // { name: String }
}

enum MdRef {
  Full(String, Dest),
  Collapsed(Dest),
  Shortcut(Dest),
}
```

### 3.6 Symbols

```rust
/// Symbol identity is occurrence-based. Do not store symbols in a HashSet as the
/// primary model: two identical links at different ranges must remain distinct.
struct SymbolOccurrence {
  id: OccurrenceId,
  scope: Scope,
  kind: SymKind,
  full_range: ByteRange,
  name_range: Option<ByteRange>,    // editable subrange, if any
  ast_idx: Option<AstIdx>,
}

enum SymKind {
  Def(Def),
  Ref(Ref),
  Tag(TagSym),
}

enum Def {
  Doc,                              // Document itself (always present)
  Title(String),                    // H1 heading (if title_from_heading)
  Header(u8, Slug),                 // H2+ heading
  LinkDef(LinkLabel),               // Reference link definition
}

enum IntraRef {
  IntraSection(Slug),               // [[#heading]] or [text](#anchor)
  IntraLinkDef(LinkLabel),          // [label] reference
  IntraTag(Slug),                   // [[#tag]] or [text](#tag)
}

/// A reference from one document to a symbol in another.
enum Ref {
  /// Wiki-link reference: [[doc]], [[doc#heading]], or ![[asset.png]]
  Wiki { target: String, heading: Option<String>, is_embed: bool },
  /// Markdown link reference: [text](url) or [text](#anchor)
  Inline { target: String, anchor: Option<String>, is_image: bool },
  /// Full reference link: [text][label]
  Full { text: String, label: LinkLabel },
  /// Collapsed reference link: [label][]
  Collapsed { label: LinkLabel },
  /// Shortcut reference link: [label]
  Shortcut { label: LinkLabel },
}

enum CrossRef {
  CrossDoc(String),                 // [[doc]] or [text](doc.md)
  CrossSection { doc: String, section: Slug },  // [[doc#section]]
}

/// A decoded tag symbol (used in SymKind and diagnostics)
struct TagSym { name: String }
```

### 3.7 Structure (Unified View)

```rust
struct Structure {
  cst: Cst,
  ast: Ast,
  symbols: Vec<SymbolOccurrence>,
  c2a: HashMap<ElementIdx, AstIdx>,          // CST index → AST index mapping
  a2s: HashMap<AstIdx, Vec<OccurrenceId>>,   // AST index → symbol occurrence IDs
}
```

`symbols` is intentionally a `Vec`: duplicate links, duplicate headings, and repeated tags
must preserve one occurrence per source range for references, highlights, code lenses, rename,
and diagnostics. Secondary indexes may use maps/sets, but they must point back to occurrence IDs.

### 3.8 Index

```rust
struct Index {
  titles: Vec<Heading>,
  headings: Vec<Heading>,
  headings_by_slug: HashMap<Slug, Vec<Heading>>,  // Fast lookup - Vec handles duplicates
  wiki_links: Vec<WikiLink>,
  md_links: Vec<MdLink>,
  link_defs: Vec<MdLinkDef>,
  tags: Vec<Tag>,
  yaml_front_matter: Option<TextNode>,
}
```

**Queries**: `link_at_pos`, `decl_at_pos`, `filter_headings_by_slug`, `try_find_link_def`, etc.

---

## 4. Link Types and Resolution

### 4.1 Heading ID Generation

Downlint uses **GitHub-compatible heading slugs** for v1. The implementation should be tested
against `github-slugger`-style fixtures and GitHub documentation examples.

```
Algorithm (Slug::from_heading_text):
  1. Extract rendered plain text from the heading's inline content
     - Markdown markup is removed, text content remains (`_italics_` → `italics`)
     - Inline code contributes its literal text
  2. Trim leading/trailing whitespace from the rendered heading text
  3. Lowercase using Unicode lowercase mapping
  4. Remove punctuation/symbol characters that GitHub removes
     - Preserve Unicode letters/numbers, spaces, `-`, and `_`
     - Preserve CJK and other non-Latin letters; do not transliterate
  5. Replace each remaining whitespace character with `-`
  6. Do not apply extra transliteration or separator collapsing beyond GitHub-compatible behavior
  7. If the slug duplicates an earlier heading in the same document, append `-1`, `-2`, ...

Examples:
  "# Doc 1"                         → "doc-1"
  "## This'll be _Helpful_"          → "thisll-be-helpful"
  "## 45"                            → "45"
  "## Привет non-latin 你好"          → "привет-non-latin-你好"
  "## 日本語の見出し"                   → "日本語の見出し"
  "## Setup" (1st/2nd/3rd)           → "setup", "setup-1", "setup-2"
```

**Heading ID Disambiguation** (when `heading_ids.enable = true`):

- First occurrence: no suffix
- Duplicates: `-1`, `-2`, etc. appended in document order
- Changing heading order can change duplicate suffixes; rename must update references accordingly

### 4.2 URL/Path Resolution

**URL Parsing**: Split at the first unescaped `#` → `{ path_or_url, anchor }`.

**Relative path convention**:

Markdown links follow the Markdown ecosystem convention: relative paths are resolved relative
to the **source document's directory**, not the workspace root. The workspace root only bounds
file discovery, config lookup, `.gitignore` handling, and root-absolute links.

Examples for source document `docs/guide/intro.md` in workspace `/repo`:

| Link | Filesystem base |
|------|-----------------|
| `[x](next.md)` | `/repo/docs/guide/next.md` |
| `[x](../api/ref.md)` | `/repo/docs/api/ref.md` |
| `[x](/README.md)` | `/repo/README.md` (workspace-root absolute) |
| `[x](#section)` | same document |

**Internal Reference Detection**:

- URI with a scheme (`https:`, `http:`, `mailto:`, etc.) → external, no diagnostic
- No extension → internal document/title/path candidate
- Configured markdown extension (`.md`, `.markdown`, etc.) → internal document
- Configured attachment extension (`.pdf`, `.jpg`, etc.) → attachment
- Unknown local-looking extension (`.json`, `.csv`, etc. not configured) → unresolved local resource; diagnostic policy in §6

**InternName** (user-provided link target):

- `src`: source document ID
- `name`: raw link text (may be title, path, or file reference)
- `Slug::from_heading_text` / `Slug::from_link_text`: converts text to GitHub-compatible slug for matching
- `try_as_path()`: attempts to interpret as file path

**InternPath** (3 variants):

- `ExactAbs`: workspace-root absolute path (`/docs/doc.md`)
- `ExactRel`: relative path anchored at the source document directory (`../folder/doc.md`, `./doc.md`)
- `Approx`: wiki-style or simple suffix/path candidate used for fuzzy document lookup (`folder/doc`, `doc`)

**FileLinkKind** (4 resolution strategies):

- `FilePath`: exact file path match
- `FileName`: filename only (`doc1.md`)
- `FileStem`: stem without extension (`doc1`)
- `Title`: document title slug

**DocLink** (resolved reference to document):

- `Explicit(FileLink)` - resolved through matching
- `Implicit(Doc)` - implicit (e.g., intra-section ref)

### 4.3 Symbol Resolution

**Dest** (resolved target) - 5 variants:

1. `Doc` - a document
2. `Attachment` - binary file
3. `Heading` - section heading
4. `LinkDef` - link definition
5. `Tag` - tag element

**Resolution Flow**:

```
1. Resolve in primary folder (via Conn graph)
   → if found → return results
2. If empty → try extra_folders (for CrossRef only)
   → Filter docs by InternName
   → Match doc slug and/or section slug
3. If still empty → try attachment resolution
   → Look up by InternName in folder's attachment registry
   → Return Dest::Attachment
```

**Unknown Extension Handling**: When a local-looking link target has an extension that is
neither a markdown extension nor a configured attachment extension, resolution returns no match
and diagnostics are produced by link kind:

- Wiki-link (`[[...]]` or `![[...]]`) → **Error** broken link
- Markdown inline link/image (`[...](...)` / `![...](...)`) → **Warning** broken or unknown local resource
- Shortcut reference (`[label]`) with no definition → **Suppressed** (too noisy and ambiguous)
- External URI with a scheme (`https://`, `mailto:`, etc.) → **Suppressed** as intentionally external

### 4.4 Cross-Folder Resolution

**Rules**:

- Primary folder searched first; extra folders only as fallback
- Only `CrossRef` (not `IntraRef`) can cross folders
- Non-transitive: A→B→C chaining not supported
- Extra-folder docs don't get diagnostics from primary session

**Markdown Link Path Constraints for Cross-Folder**:

Markdown links with path syntax follow current-document-relative semantics and do not use
extra-folder fuzzy fallback. Cross-folder fallback is for wiki-style document references and
other `CrossRef` forms that are not explicit filesystem-relative paths.

| Link form | Resolved as | Cross-folder fallback? |
|-----------|-------------|------------------------|
| `[text](doc-b.md)` | `ExactRel` - anchored to source document directory | **No** |
| `[text](some-folder/doc-b.md)` | `ExactRel` - anchored to source document directory | **No** |
| `[text](../folderB/doc-b.md)` | `ExactRel` - anchored to source document directory | **No** |
| `[text](/doc-b.md)` | `ExactAbs` - anchored to workspace root | **No** |
| `[[doc-b]]` | `Approx` / title/stem lookup | **Yes, after primary tier misses** |

---

## 5. Completion System

### 5.1 Completable Types

**Complete Elements**: `WikiLink`, `Heading`, `MdLink`, `MdLinkDef`, `Tag`

**Partial Elements** (while typing):

- `WikiLink(dest, heading, range)` - `[[...]]` or `[[...|#...]]`
- `InlineLink(text, path, anchor, range)` - `[text](path#anchor)`
- `ReferenceLink(label, range)` - `[label]`
- `TagOpening(cursor_pos)` - `#` alone

### 5.2 Prompt Classification

| Prompt Type | Context | Example |
|-------------|---------|---------|
| `WikiDoc` | Document in `[[doc]]` | `[[foo` |
| `WikiHeadingInSrcDoc` | Section in `[[#heading]]` | `[[#` |
| `WikiHeadingInOtherDoc` | Section in `[[doc#heading]]` | `[[doc#` |
| `Reference` | Link definition label | `[foo` |
| `InlineDoc` | Markdown path | `[text](foo` |
| `InlineAnchorInSrcDoc` | Heading in `[text](#heading)` | `[text](#` |
| `InlineAnchorInOtherDoc` | Heading in `[text](path#anchor)` | `[text](path#` |
| `Tag` | Tag completion | `#` |

### 5.3 Completion Styles

| Style | Output | Example |
|-------|--------|---------|
| `title-slug` | Slugified title | `[[my-note]]` |
| `title` | Raw title (Obsidian-compatible) | `[[My Note]]` |
| `file-stem` | Filename without extension | `[[doc1]]` |
| `file-path-stem` | Path without extension | `[[sub/doc1]]` |

Default: `title-slug` if `title_from_heading=true`, otherwise `file-stem`

### 5.4 Candidate Finding

**Doc Candidates**:

1. Fuzzy filter in primary folder (see algorithm below)
2. If no match → try extra_folders
3. Exclude source document

**Heading Candidates**:

1. Determine target docs (src_doc if no doc specified)
2. Filter headings by slug subsequence
3. Deduplicate heading names (by display text)

**LinkDef Candidates**:

- Filter by label subsequence match in current doc

**Tag Candidates**:

- Collect all tags from primary + extra folders
- Case-insensitive subsequence match
- Count usages per tag (for sort ordering)

**Fuzzy Matching Algorithm**:

The completion system uses a **case-insensitive subsequence** algorithm (not Levenshtein):

- Checks if *all characters* of the input appear in the target **in order**, but not necessarily contiguous
- Case-insensitive comparison
- Empty input matches everything

Examples:
- `"mn"` matches `"markdown"` — 'm' at pos 0, 'n' at pos 7 ✓
- `"mn"` does NOT match `"md"` — 'm' at pos 0, but no 'n' after ✗
- `"mar"` matches `"markdown"` — contiguous substring is also a valid subsequence ✓

This is implemented as a helper function — `fn is_subsequence(haystack: &str, needle: &str) -> bool` —
a simple O(n) greedy scan.

### 5.5 Completion Item Construction

- **FilterText**: For fuzzy matching (title for wiki, encoded path for inline)
- **Label**: Display text
- **TextEdit**: Replaces the partial element range
- **Detail**: Additional info (file path, reference count for tags)
- **SortText**: For ordering
- Max candidates: configurable (default 50)

---

## 6. Diagnostics

### 6.1 Diagnostic Types

**Severity Assignment Rules** — how severity is determined based on link type:

| Diagnostic | Code | WikiLink / Embed Severity | MarkdownLink / Image Severity | Other |
|------------|------|---------------------------|-------------------------------|-------|
| `AmbiguousLink` | DNL001 | **Error** | Warning | N/A |
| `BrokenLink` | DNL002 | **Error** | Warning | N/A |
| `NonBreakableWhitespace` | DNL003 | N/A | N/A | Warning |

> **Key rule**: WikiLinks and embed wiki-links (`[[...]]`, `![[...]]`) receive **Error**
> severity for broken/ambiguous local targets. Markdown links/images (`[...](...)`,
> `![...](...)`) receive **Warning** severity. `--min-severity` filters output only; it does
> not change assigned severity.

### 6.2 Diagnostic Rules

**BrokenLink**: Link cannot be resolved

- WikiLinks (`[[...]]`) and embed wiki-links (`![[...]]`) → Error
- MarkdownLinks and images (`[...](...)`, `![...](...)`) to local-looking unresolved targets → Warning
- MarkdownLinks/images to external URIs with schemes (`https:`, `http:`, `mailto:`, etc.) → suppressed
- MarkdownLinks/images to configured attachment extensions resolve as attachments; missing attachment → Warning
- MarkdownLinks/images to unknown local file extensions → Warning unless an explicit suppression rule applies
- Inline shortcut links (`[text]`) with no matching definition → **always suppressed** from diagnostics
  - Shortcut links are ambiguous and very noisy in prose

**AmbiguousLink**: Multiple destinations resolve

- Includes `DiagnosticRelatedInformation` for all resolved destinations

**NonBreakableWhitespace**: `\u00a0` after heading markers

- Always Warning
- "Non-breaking whitespace used instead of regular whitespace"

**Embed Links** (`![[...]]`):

- Detected by the sidecar scanner via the `![[` prefix; do not rely on comrak for this.
- Resolved and diagnosed like wiki-links, with `is_embed = true` preserved for future rendering.
- Broken or ambiguous embed targets produce DNL002/DNL001 with **Error** severity.
- Embeds may target documents or attachments. Missing attachments are still broken links.

### 6.3 Diagnostic Computation

**Per-Folder**:

```
for each document in folder:
  check_links(folder, extra_folders, doc)
  check_non_breaking_whitespace(doc)
```

**Per-Workspace**:

```
for each primary folder:
  extra_folders = Workspace.extra_folders_for(folder)
  folder_diag = FolderDiag::mk(folder, extra_folders)
```

**Rules**:

- Only checked if folder has multiple files (single-file mode skips cross-file diags)
- Extra-folder documents NOT diagnosed from primary session
- Non-transitive: no chaining through extra folders

### 6.4 Diagnostic Output & Severity

All diagnostic messages use this format:

| Code | WikiLink Message | MarkdownLink Message |
|------|-----------------|---------------------|
| DNL001 | `Ambiguous link: 'target' resolves to multiple destinations` | `Ambiguous link: 'target' resolves to multiple destinations` |
| DNL002 | `Broken link: 'target' could not be resolved` | `Broken link: 'target' could not be resolved` |
| DNL003 | `Non-breaking whitespace after heading marker` | N/A |

**AmbiguousLink detail** — includes `DiagnosticRelatedInformation` listing each destination:
```json
{
  "message": "Ambiguous link: 'doc' resolves to multiple destinations",
  "related": [
    { "message": "docs/doc.md", "location": { "uri": "file:///.../docs/doc.md" } },
    { "message": "assets/doc.md", "location": { "uri": "file:///.../assets/doc.md" } }
  ]
}
```

**BrokenLink detail** — the `target` in the message is the raw link text (decoded for wiki-links, raw for markdown links).

**BrokenLink JSON example**:
```json
{
  "message": "Broken link: 'missing' could not be resolved",
  "code": "DNL002"
}
```

**CLI text output format** — see §11.2 for full details.

**Severity Filtering** — how `--min-severity` controls what is shown:

| `--min-severity` | Shown |
|------------------|-------|
| `error` | DNL001 (Error), DNL002 with Error severity |
| `warning` (default) | All errors + all warnings, including DNL003 |
| `info` | All diagnostics, including any future info-level diagnostics |

> **Key distinction**: The severity assignment rules (WikiLink → Error, MarkdownLink → Warning)
> are fixed. `--min-severity` only filters the output. Even at `warning`, wiki-link errors
> are always shown because they are assigned `error` severity, not `warning`.

### 6.5 Diagnostic Publishing (LSP)

**DiagnosticsManager**: A `tokio::task` spawned on server start, driven by a `tokio::sync::mpsc` channel.

- Debounces updates (200ms grace period) — coalesces rapid edits into a single publish
- Accumulates state changes — tracks which documents need updates via a `HashSet<Uri>`
- Computes differential updates (`calc_diagnostics_update`) — compares against previous state
- Publishes only changed documents via `textDocument/publishDiagnostics` notification

**Concurrency model**: Single async task owns the diagnostics state. All LSP requests
(`textDocument/diagnostic`, `workspace/diagnostic`) route through this task via channels.
No shared mutable state — prevents race conditions on concurrent edits.

**Concurrent edit handling**: When multiple `textDocument/didChange` requests arrive in
quick succession, they are queued through the mpsc channel and coalesced into a single
diagnostic update. The 200ms debounce window ensures that rapid typing doesn't trigger
a diagnostic publish for every keystroke. Cancel support (`$/cancelRequest`) is not yet
implemented — in-flight requests complete normally.

**Triggers**:

- `text_document/did_change`, `did_open`, `did_close`
- `workspace/did_create_files`, `did_delete_files`
- `workspace/did_change_watched_files` (extra folder changes)

**Document lifecycle edge case**: If a document is closed and then reopened with different content, the old diagnostic state is fully cleared before new diagnostics are computed. No stale diagnostics persist across close/reopen cycles.

---

## 7. Code Actions

### 7.1 Table of Contents (TOC)

**Config**: `code_action.toc.enable` (default: true), `code_action.toc.include` (default: [1,2,3,4,5,6])

**Markers**: `<!--toc:start-->` ... `<!--toc:end-->`

**Insertion Modes**:

1. `DocumentBeginning` — Insert at top (if no markers found)
2. `Replacing` — Replace existing TOC between markers
3. `After` — After frontmatter or title (if no markers found)

**Rendering**:

```markdown
<!--toc:start-->
- [Title](#title-slug)
  - [Subtitle](#subtitle-slug)
    - [Deep Section](#deep-section-slug)
<!--toc:end-->
```

**Rendering Rules**:

- Nesting depth limited by `toc.include` array (e.g., [1,2,3] = H1-H3 only)
- Indentation: 2 spaces per level
- Links use slug-based anchors (not title text)
- Auto-inserts `<!--toc:start-->` / `<!--toc:end-->` markers if missing
- Skips headings inside code blocks, frontmatter, and existing TOC regions

**Smart behavior**:

- Only offers action if generated TOC differs from existing (line-by-line comparison)
- Adds empty lines as needed for readability
- Preserves document structure (frontmatter, headings before TOC)
- If markers exist but TOC is unchanged, action is **not** offered (no-op prevention)

### 7.2 Create Missing File

**Config**: `code_action.create_missing_file.enable` (default: true)

**Conditions**:

- Cursor over a broken link element
- Link resolves to non-existent document
- Not an attachment extension
- Not in extra folder

**Action**: Creates new markdown file with inferred filename from link target

### 7.3 Code Lenses (v1)

Downlint includes code lenses in v1 for lightweight reference visibility.

**Provider**: `textDocument/codeLens` with `codeLens/resolve`.

**Lens locations**:

- Document title / H1: total inbound references to the document
- Headings: inbound references to that heading slug
- Link definitions: number of reference-link occurrences using the label
- Tags: number of tag occurrences and references, when tags are indexed

**Behavior**:

- Initial `textDocument/codeLens` returns unresolved lenses with stable occurrence IDs in `data`.
- `codeLens/resolve` computes the display command lazily from the current connection graph.
- Lenses are disabled in single-file mode except for same-document heading/tag counts.
- Counts are occurrence-based; duplicate references in one file count separately.
- If the client does not support code lenses, no work is scheduled.

**Example command titles**:

- `3 references`
- `1 reference`
- `No references`

---

## 8. Refactoring

### 8.1 Rename Types

**Markdown Reference Labels** (`ML`, `MLD`):

- Renames label in all references and definition
- Validation: no `\n`, `[`, `]`, `(`, `)`

**Heading Rename** (`H`):

- **H1 title**: Updates heading text + all wiki-links + markdown-link anchors
  - Propagates to `referencing_folders`
- **H2+ headings**: Updates heading text + same-doc links + cross-folder links
  - Propagates to `referencing_folders`
- Validation: no `\n`, `#`

**Attachment Rename** (`WL` with `Dest::Attachment`):

1. Rename file on disk
2. Update all wiki-link references

- Extension preserved automatically
- Validation: no `\n`, `[`, `]`, `(`, `)`

### 8.2 Workspace Edit Construction

Multi-file renames require coordinating edits across multiple documents. The LSP server
constructs a `WorkspaceEdit` that may contain changes for several files in a single operation.

**Construction process**:

1. **Collect edits per document**: For each document affected by a rename, compute the text
   edits needed (e.g., updating a heading title in all wiki-link anchors)
2. **Sort within each document**: Edits are sorted in reverse order (end-of-doc first)
   to prevent offset shifts from invalidating subsequent edits
3. **Group by file**: Organize edits into a `HashMap<Uri, Vec<TextEdit>>` per file
4. **Choose format**: Use `DocumentChanges` (modern LSP 3.16+) when the client supports
   it, falling back to `Changes` for older clients

**Example — Renaming a heading across files**:

```rust
// Rename "Old Title" → "New Title" affects:
// - docs/a.md: heading text + 2 wiki-link anchors
// - docs/b.md: 1 wiki-link anchor  
// - assets/c.md: 1 markdown link anchor

WorkspaceEdit {
    changes: Some({
        "file:///docs/a.md": [
            TextEdit { range: H1_range, new_text: "New Title" },
            TextEdit { range: anchor_1_range, new_text: "new-title" },
            TextEdit { range: anchor_2_range, new_text: "new-title" },
        ],
        "file:///docs/b.md": [
            TextEdit { range: anchor_range, new_text: "new-title" },
        ],
        "file:///assets/c.md": [
            TextEdit { range: anchor_range, new_text: "new-title" },
        ],
    }),
    document_changes: None,  // Use DocumentChanges when client supports it
}
```

**Validation**: Before sending the workspace edit, validate that all target ranges
still exist in the current document state. If any range has shifted (due to concurrent edits),
the rename should be rejected with a `MethodFailed` error including the prepare-rename result.

### 8.3 Prepare Rename

Returns the rename range for the element:

- WikiLink: attachment name range
- MarkdownLink/Def: label range
- Heading: title range

---

## 9. Configuration System

### 9.1 Config File Locations

| Level | Path |
|-------|------|
| User | `~/.config/downlint/config.toml` (Linux) — respects `XDG_CONFIG_HOME` env var (default: `~/.config`) |
| User | `~/Library/Application Support/downlint/config.toml` (macOS) |
| User | `%APPDATA%\downlint\config.toml` (Windows) |
| Project | `.downlint.toml` (project root) |

**Base precedence**: Project > User > Defaults. LSP `initializationOptions` and CLI flags can override specific runtime settings; see §9.3.

### 9.2 `.downlint.toml` Schema

The project-level config file uses this structure:

> **Note on `paranoid` mode**: The original F# marksman included a `paranoid = false` flag for
> debugging — it validated incremental results against from-scratch computation on every change.
> For the Rust implementation, this is a dev-only build-time feature (equivalent to
> `#[cfg(debug_assertions)]`). It has no runtime cost in release builds and is not exposed as
> a config option.

```toml
# .downlint.toml — project-level configuration
# Precedence: project > user > defaults

[core]
# Additional markdown file extensions (replaces defaults when set)
file_extensions = ["md", "markdown", "mdx"]

# GitHub-compatible heading ID disambiguation for duplicate headings
heading_ids.enable = true

# Text sync mode: "full" | "incremental"
text_sync = "full"

# Treat H1 as document title
title_from_heading = true

# Cross-folder resolution (relative to this file's directory)
extra_folders = ["../shared-notes", "assets/wiki"]

# Attachment extensions to add (appended to defaults)
attachment_file_extensions_add = ["drawio", "mermaid"]

# NOT exposed as a config option (dev-only build-time feature)
# paranoid = true  # cfg(debug_assertions): validates incremental results against from-scratch

[code_action]
# Table of contents generation
toc.enable = true
toc.include = [1, 2, 3]  # Only include H1-H3

# Auto-create missing files from broken links
create_missing_file.enable = true

[completion]
# Max completion candidates per request
candidates = 50

# Wiki-link completion style: "title-slug" | "title" | "file-stem" | "file-path-stem"
wiki.style = "title-slug"
```

**Config Section Hierarchy**:

| Section | Purpose |
|---------|--------|
| `[core]` | Core markdown behavior: extensions, heading IDs, text sync, title handling |
| `[code_action]` | Code action controls: TOC generation, missing file creation |
| `[completion]` | Completion behavior: candidate limits, wiki-link display style |

### 9.3 Merge Logic

Parsing uses **partial config structs** where every user-specified field is optional. After all
levels are merged, Downlint materializes a fully-resolved `Config` by filling defaults and running
validation. This is required so TOML like `heading_ids.enable = true`, `toc.include = [...]`, and
`wiki.style = "title-slug"` matches the Rust data model.

**Precedence by mode**:

- CLI check/watch/fix/stdin: CLI flags > project config > user config > defaults
- LSP server: `initializationOptions` > project config > user config > defaults

**Partial schema shape**:

```rust
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialConfig {
    core: Option<PartialCoreConfig>,
    code_action: Option<PartialCodeActionConfig>,
    completion: Option<PartialCompletionConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialCoreConfig {
    file_extensions: Option<Vec<String>>,
    heading_ids: Option<PartialHeadingIdsConfig>,
    text_sync: Option<TextSyncKind>,
    title_from_heading: Option<bool>,
    extra_folders: Option<Vec<String>>,
    attachment_file_extensions_add: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialHeadingIdsConfig { enable: Option<bool> }

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialCodeActionConfig {
    toc: Option<PartialTocConfig>,
    create_missing_file: Option<PartialCreateMissingFileConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialTocConfig { enable: Option<bool>, include: Option<Vec<u8>> }

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialCreateMissingFileConfig { enable: Option<bool> }

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialCompletionConfig {
    candidates: Option<usize>,
    wiki: Option<PartialWikiCompletionConfig>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PartialWikiCompletionConfig { style: Option<WikiCompletionStyle> }
```

**General Merge Rule**: For `Option<T>` fields, the higher-precedence value wins if present;
otherwise falls through to the lower-precedence value.

```rust
fn merge_option<T>(hi: Option<T>, low: Option<T>) -> Option<T> { hi.or(low) }
```

**Nested Merge Rule**: Nested tables merge recursively. For example, project config can set
`toc.include` without resetting user config's `toc.enable`.

**Special Accumulation Rule**: `attachment_file_extensions_add` **accumulates**
(both values are merged, with higher-precedence appended last):

```rust
fn merge_extensions_add(hi: Option<Vec<String>>, low: Option<Vec<String>>) -> Option<Vec<String>> {
    match (hi, low) {
        (Some(h), Some(l)) => Some([l, h].concat()),  // low first, then hi
        (Some(h), None) => Some(h),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}
```

**Finalization**: After merging partial configs, construct non-optional runtime structs:

```rust
struct Config { core: CoreConfig, code_action: CodeActionConfig, completion: CompletionConfig }
struct CoreConfig {
    file_extensions: Vec<String>,
    heading_ids: HeadingIdsConfig,
    text_sync: TextSyncKind,
    title_from_heading: bool,
    extra_folders: Vec<String>,
    attachment_file_extensions: Vec<String>,
}
struct HeadingIdsConfig { enable: bool }
```

### 9.4 Config Validation Policy

Downlint uses **strict validation** for configuration files. Invalid or unknown keys produce
a clear error at startup — no silent fallbacks.

**How it works**: Config structs use `#[serde(deny_unknown_fields)]` to reject unknown keys at
parse time. This is a `serde` derive attribute that causes `Deserialize::deserialize()` to return
an error when an unrecognized field is encountered.

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialCoreConfig {
    file_extensions: Option<Vec<String>>,
    heading_ids: Option<PartialHeadingIdsConfig>,
    text_sync: Option<TextSyncKind>,
    title_from_heading: Option<bool>,
    // ...
}

struct PartialHeadingIdsConfig {
    enable: Option<bool>,
}
```

**Unknown Keys**:

- Any key not defined in the schema (§9.2) produces a **parse error** at startup
- Error message format: `downlint: error: unknown key 'foo' in .downlint.toml, line 5`
- The server exits with code 2; the CLI prints the error to stderr

**Invalid Values**:

- Type mismatches (e.g., string where integer expected) produce a parse error
- Out-of-range values (e.g., `toc.include = [0, 7]`) produce a validation error
- Empty arrays for non-empty-required fields produce a validation error

**Validation Timing**:

- Config is validated **before** any processing begins (fail-fast)
- Both user config and project config are validated independently
- If either file is invalid, the server/CLI exits with code 2 and an error message
- No partial loading — if user config is invalid, project config is never read

**Error Message Format**:

```
downlint: error: failed to parse `.downlint.toml`
downlint: note: unknown key 'foo' at line 5, column 3
downlint: note: did you mean 'file_extensions'? (typo detection)
```

**Error recovery**: None. The tool exits immediately with a clear error message.
Users must fix the config file before downlint will run. This prevents silent misconfiguration
from producing confusing results.

> **Design principle**: A broken config should fail loudly and immediately. Users should never
> wonder why downlint isn't working — the error message tells them exactly what's wrong.

---

## 10. Infrastructure

### 10.1 Path Types

| Type | Description | Example |
|------|-------------|---------|
| `AbsPath` | Absolute path | `/home/user/note.md` or `C:\note.md` |
| `RelPath` | Relative path | `docs/note.md` |
| `LocalPath` | Abs or Rel | `Abs(AbsPath)` or `Rel(RelPath)` |
| `RootPath` | Folder root path | Wrapper for folder root |
| `RootedRelPath` | Path relative to root | `{ root, path: Option<RelPath> }` |
| `CanonDocPath` | Canonical doc path (no extension) | `docs/note` from `docs/note.md` — used in link resolution and config path matching |

### 10.2 Suffix Tree

Trie-based structure for prefix-based path matching:

- Keys stored in reversed form for suffix matching
- Used in `FolderLookup::docs_by_path`
- Operations: `add`, `remove`, `filter_matching_values`

### 10.3 Connection Graph (Conn)

Tracks symbol resolution relationships across a folder's documents.

**Graph type**: Custom adjacency-list graph (no external dependency). A dedicated `petgraph`
dependency is under consideration for v2, but the current implementation uses a hand-rolled
`HashMap<NodeIndex, Vec<(NodeIndex, T)>>` for zero-dependency and full control over
serialization/deserialization of graph state.

```rust
// Minimal adjacency-list graph for resolved/unresolved edges
struct Graph<T> {
    /// Adjacency list: node → Vec<(neighbor, edge_data)>
    edges: HashMap<NodeIndex, Vec<(NodeIndex, T)>>,
}

/// Node index — compact identifier for graph nodes
/// Uses `u32` for memory efficiency; supports ~4 billion nodes per folder
type NodeIndex = u32;
```

**Conceptual Model**:

```
┌─────────────────────────────────────────────────────┐
│                    Conn (per folder)                 │
│                                                      │
│  refs: Scope → Vec<SymbolOccurrence> (refs)         │
│  defs: Scope → Vec<SymbolOccurrence> (defs)         │
│  tags: Scope → Vec<SymbolOccurrence> (tag registry) │
│                                                      │
│  resolved: Graph<ResolvedEdge> (Ref occurrence → Def)│
│  unresolved: Graph<Unresolved> (broken refs)         │
│  ref_deps: Scope → Vec<(Scope, CrossRef)> (deps)    │
│  last_touched: HashSet<OccurrenceId> (dirty tracking)│
└─────────────────────────────────────────────────────┘
         │                        │
         ▼                        ▼
   Scope A (doc1.md)      Scope B (doc2.md)
   - defs: [H1, H2]       - defs: [H1]
   - refs: [WL→doc2]      - refs: [ML→doc1#h2]
```

**How it works**:

- Each `Scope` = `(folder_id, doc_id)` — unique identifier for a document in a folder
- `refs` / `defs` / `tags` map scopes to occurrence records (fast lookup by scope)
- `resolved` / `unresolved` track occurrence-to-occurrence link relationships (graph edges)
- `ref_deps` records cross-document dependencies for incremental invalidation
- `last_touched` marks which occurrence IDs changed since the last check

```rust
struct Conn {
    /// All reference occurrences grouped by their source scope
    refs: HashMap<Scope, Vec<SymbolOccurrence>>,

    /// All definition occurrences grouped by their defining scope
    defs: HashMap<Scope, Vec<SymbolOccurrence>>,

    /// Tag occurrences grouped by scope
    tags: HashMap<Scope, Vec<SymbolOccurrence>>,

    /// Resolved edges (reference occurrence → destination occurrence)
    resolved: Graph<ResolvedEdge>,

    /// Unresolved reference occurrences
    unresolved: Graph<Unresolved>,

    /// Cross-document dependency edges for incremental tracking
    ref_deps: HashMap<Scope, Vec<(Scope, CrossRef)>>,

    /// Set of last-touched occurrence IDs for change tracking
    last_touched: HashSet<OccurrenceId>,
}

/// A scope is a folder + document combination
/// `doc: None` represents the folder root scope (for folder-level symbols like
/// tags defined at the folder root, or link definitions not tied to a specific file)
/// `doc: Some(DocId)` represents a specific document within the folder
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Scope {
    folder: FolderId,
    doc: Option<DocId>,
}

struct ResolvedEdge {
    from: OccurrenceId,  // reference occurrence
    to: OccurrenceId,    // definition/tag/attachment occurrence or synthetic destination
}

/// An unresolved reference occurrence with its intended target
struct Unresolved {
    occurrence: OccurrenceId,
    scope: Scope,
    ref_type: RefType,
    target_name: String,
}
```

**Oracle**:

- `resolve_to_occurrences(Ref) → Vec<OccurrenceId>` — Find destination occurrences
- `resolve_in_scope(Scope, DefKey) → Vec<OccurrenceId>` — Find definitions in scope

**Incremental Updates**:

When a document changes, only affected scopes are recomputed:
1. Mark `last_touched` occurrence IDs as dirty
2. Rebuild resolved/unresolved graphs for affected scopes
3. Propagate changes through `ref_deps` edges
4. Diff against previous state to produce minimal update

### 10.4 Occurrence Maps

Downlint must not collapse equal-looking symbols that occur at different source ranges.
The primary store is occurrence-based:

```rust
type OccurrenceMap = HashMap<Scope, Vec<SymbolOccurrence>>;
type OccurrenceLookup = HashMap<OccurrenceId, SymbolOccurrence>;
```

**Usage in Connection Graph (§10.3)**:

- `refs: HashMap<Scope, Vec<SymbolOccurrence>>` — reference occurrences grouped by source scope
- `defs: HashMap<Scope, Vec<SymbolOccurrence>>` — definition occurrences grouped by defining scope
- `tags: HashMap<Scope, Vec<SymbolOccurrence>>` — tag occurrences grouped by scope

Secondary indexes may use sets for lookup keys, but values must be occurrence IDs:

```rust
type DefIndex = HashMap<(Scope, DefKey), Vec<OccurrenceId>>;
type RefIndex = HashMap<(Scope, RefKey), Vec<OccurrenceId>>;
```

**Incremental Updates via `difference`**:

When a document changes, compare occurrence vectors by stable occurrence keys
(`kind`, normalized name, and `ByteRange`). This preserves duplicates while still allowing
minimal graph updates.

```rust
fn difference(
  old: &[SymbolOccurrence],
  new: &[SymbolOccurrence],
) -> OccurrenceDiff
```

**Why not `HashSet<Sym>`?** A set would collapse repeated links like two identical `[[doc]]`
references in one file. LSP references, highlights, code lenses, diagnostics, and rename all
need one occurrence per source range.

### 10.5 Text Handling

**LineMap**: Array of line start/end byte offsets and line-ending widths

- O(log n) line lookup by byte offset (binary search)
- Byte offset ↔ LSP position conversion using negotiated encoding (§2.8)
- UTF-16 conversion scans the target line prefix and counts `char::len_utf16()`
- Handles `\n`, `\r\n`, bare `\r`, and final lines without trailing newline

**Text**: Immutable document content wrapped in `Arc<String>` with a `LineMap`

- Content is immutable once created; edits produce new `Text` instances
- `Arc` enables sharing parsed content across the connection graph and diagnostics manager
  without cloning — the same document can be referenced by multiple scopes
- Operations: `substring(range)`, `line_content(line)`, `cutout(range)`, `char_at(position)`
- `apply_text_change(changes, text)` — Apply LSP text changes, returns new `Text`

### 10.6 Name Encoding

**Slug** - GitHub-compatible heading/document slug for matching:

- Lowercase rendered plain text, remove GitHub-excluded punctuation/symbols, replace whitespace with hyphens
- Preserve Unicode letters/numbers (including CJK); do not transliterate
- `"My Note Title"` → `"my-note-title"`
- `"Привет non-latin 你好"` → `"привет-non-latin-你好"`
- `is_sub_sequence`: Fuzzy subsequence match
- `is_sub_string`: Exact substring match

**UrlEncoded**: Standard URI encoding (`percent-encoding`)

**WikiEncoded**: Wiki-specific encoding (`#`→`%23`, `[`→`%5B`, `]`→`%5D`, `|`→`%7C`, `?`→`%3F`)

**LinkLabel** - Normalized for matching:

- Normalize Unicode, lowercase, trim
- Collapse consecutive whitespace

---

### 10.7 Symlink Handling

Directory traversal and file discovery must handle symlinks carefully:

**Rules**:

- **Follow symlinks for file discovery**: When scanning for markdown files, follow symbolic links
  to include linked documents in the workspace. A symlinked `docs/` directory should be treated
  as a regular folder.
- **Detect symlink loops**: During recursive directory traversal, track the canonical path of each
  directory visited. If a symlink resolves to an already-visited canonical path, skip it to avoid
  infinite recursion.
- **Canonicalize paths**: Use `std::fs::canonicalize()` (or equivalent) to resolve symlinks when
  building the connection graph. This ensures that `[ref](doc.md)]` resolved through a symlinked
  path matches the same document accessed directly.

**Implementation guidance**:

```rust
// Pseudocode for safe recursive directory traversal
fn scan_dir(dir: &Path, visited: &mut HashSet<CanonicalPath>) {
    let canonical = dir.canonicalize()?;
    if !visited.insert(canonical.clone()) {
        return; // Loop detected — skip
    }
    for entry in dir.read_dir()? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, visited);
        } else if is_markdown(&path) {
            collect_document(path);
        }
    }
}
```

**Edge cases**:
- Self-referencing symlinks (`docs -> docs`) — caught by visited set
- Circular symlinks (A -> B -> A) — caught by visited set
- Symlinked files (not directories) — included in document collection normally
- Broken symlinks — skip with a warning, do not crash

### 10.8 .gitignore Support

File discovery respects `.gitignore` patterns to exclude files that would be ignored by git:

**Rules**:

- **Parse `.gitignore`** from the workspace root and all parent directories up to the filesystem root
- **Apply patterns** in order — later rules override earlier ones (standard gitignore semantics)
- **Include** files matching `!` (negation) patterns even if previously excluded
- **Exclude** files matching standard ignore patterns (`*.log`, `node_modules/*`, etc.)
- **Include hidden files** — `.hidden.md` is included in file discovery by default (no special treatment for dotfiles)

**Implementation**:

- Use the `ignore` crate (`ignore::gitignore::Gitignore`) for standard gitignore parsing
- Each folder may have its own `.gitignore`; patterns from parent directories apply recursively
- The `--root` override also overrides `.gitignore` resolution (search starts from the specified root)

**Edge cases**:
- `.gitignore` in a subdirectory only applies to files within that directory (and descendants)
- Empty `.gitignore` files are valid and have no effect
- Missing `.gitignore` is normal — not an error
- Binary files (attachments) are never excluded by `.gitignore` rules

**Example**:

```bash
# .gitignore in workspace root
*.log
vendor/

# Result: *.md files in vendor/ are excluded from workspace
docs/note.md → included
vendor/docs/guide.md → excluded
```

---

## 11. CLI

### 11.0 Quick Reference

A one-line summary of all flags before diving into details:

| Category | Flags |
|----------|-------|
| **Global** | `--help`, `--version`, `-q` / `--quiet` |
| **Check** (default) | `<PATH>`, `--root`, `--format`, `--min-severity`, `--color`, `--verbose`, `--fix`, `-w` / `--watch`, `--stdin`, `-` |
| **Server** | `--verbose`, `--wait-for-debugger` |

> **Note**: `downlint` and `downlint check` are equivalent — both run the diagnostic
> checker. The "Check" row refers to the default behavior (no subcommand), not a
> separate subcommand. See §11.2 for full details.

**Exit Codes**:

| Code | Meaning |
|------|--------|
| `0` | Clean — no diagnostics at the chosen severity level |
| `1` | Issues found — diagnostics exist at the chosen severity level |
| `2` | Error — config parse failure, path not found, or I/O error |

### 11.1 Global Flags

The following flags are available on all subcommands:

| Flag | Description |
|------|-------------|
| `--help` | Print usage information and exit |
| `--version` | Print version number and exit |
| `-q`, `--quiet` | Suppress all **stdout** output (diagnostics, JSON body). Errors still go to stderr. Only exit code matters (exit 0 = clean, 1 = issues) |

> **Note**: Global flags take precedence over all other flags — no processing occurs when
> `--help` or `--version` is provided. `--quiet` suppresses stdout only — stderr still
> shows errors and warnings (see §13.2). `--quiet` is also available on `downlint server`
> (suppresses nothing meaningful; the server writes to stderr, not stdout).

**Precedence**: Global flags are defined once and inherited by all subcommands.

### 11.2 `downlint` (default — check)

Standalone diagnostic checker. This is the default behavior when no subcommand is given.

**Usage**:

```
downlint [--root <DIR>] [--format <text|json>] [--min-severity <info|warning|error>]
         [--color <auto|always|never>] [--verbose <LEVEL>] [--fix]
         [-w|--watch] [--stdin|-] [<PATH>]
```

**Arguments**:

- `<PATH>` — File or directory to check (default: current directory)
  - If the directory contains no markdown files, exits with code 0 and prints nothing

**Flags**:

- `--root <DIR>` — Workspace root override (inferred: `.downlint.toml` → `.git` → parent)
  Full algorithm documented in the "Root Inference Algorithm" subsection below.
- `--format <text|json>` — Output format (default: text)
- `--min-severity <info|warning|error>` — Filter diagnostics by severity level (default: `warning`)
  > At the default `warning`, warnings and errors are shown, including DNL003. Wiki-link errors
  > (DNL002) are always shown because they're assigned `error` severity.
- `--color <auto|always|never>` — Control ANSI color output (default: `auto`)
  - `auto`: Color only when stdout is a TTY (detected via `std::io::IsTerminal`); no color when piped
  - `always`: Always emit ANSI color codes (useful for CI with colored terminals)
  - `never`: Never emit color codes (safe for pipelines and log files)
  > Controls ANSI codes on both stdout (diagnostics) and stderr (error messages).
- `--verbose <LEVEL>` / `-v` — Logging level to stderr (default: 2)
  > Controls `tracing-subscriber` output. Same flag as §11.3 (server).
- `--fix` — Apply safe fixes in place, then re-run diagnostics.
  - v1 fix: replace non-breaking whitespace after heading markers (DNL003) with regular spaces.
  - Mutually exclusive with `--stdin` and `--watch` in v1.
- `-w`, `--watch` — Watch files and re-run diagnostics after changes.
  - Uses the same root/config/file discovery as check mode.
  - Debounces file events by 200ms.
  - Mutually exclusive with `--stdin` and `--fix` in v1.
- `--stdin` / `-` — Read one Markdown document from stdin.
  - Uses a synthetic path `<stdin>.md` and single-file mode by default.
  - No cross-file diagnostics unless a future `--stdin-path`-style option is added.
  - Diagnostics still go to stdout/text or stdout/JSON according to `--format`.

> **Note**: `downlint` does NOT read from stdin by default. Use `downlint --stdin` or
> `downlint -` explicitly: `echo "# Title" | downlint -`.
>
> **Mutual exclusivity**: `--quiet` and `--format json` are mutually exclusive.
> When `--quiet` is set, no stdout output is produced — including the JSON body.
> Users who need machine-readable JSON output for CI should **not** use `--quiet`.
> The exit code still reflects the diagnostic state regardless of `--quiet`.

**Error Output Format**: All errors use the same stderr format (see §13.2 for details).

**Root Inference Algorithm** (applied when `--root` is not specified):

When no `--root` is given, the workspace root is determined by searching upward
from the target path:

```
Target directory/file
    ↓
1. Look for .downlint.toml          → use that directory as root
    ↓ (not found)
2. Look for .git/                   → use that directory as root
    ↓ (not found)
3. Use parent directory as root    → recurse upward until .git or filesystem root
    ↓
4. Filesystem root reached        → use the file's parent directory (or the directory itself if target is a directory)
```

**Examples**:

| Target | Root Found | Result |
|--------|-----------|--------|
| `./project/docs/note.md` | `.git/` at `./project/` | `./project/` |
| `./project/docs/.downlint.toml` | `.downlint.toml` at `./project/docs/` | `./project/docs/` |
| `./standalone.md` | No `.git`, no `.downlint.toml` | `./` (parent of the file) |

**Important**: The root determines config discovery, file discovery, `.gitignore` handling,
extra-folder paths, and `/root-absolute.md` links. Ordinary relative Markdown links like
`[ref](other.md)` are resolved relative to the **source document's directory**.

**Global flags** — see §11.1.

> **Note**: Global flags take precedence over all other flags — no processing occurs when
> `--help` or `--version` is provided. `--quiet` suppresses stdout only; errors and warnings
> still go to stderr (see §13.2).

**Exit Codes**:

- `0` — Clean: no diagnostics at the **chosen severity level** (not "no diagnostics at all")
  > With `--min-severity error`: exits 0 if no errors, even if warnings exist
  > With `--min-severity warning` (default): exits 0 if no warnings or errors
  > With `--min-severity info`: exits 0 only if no diagnostics of any kind
- `1` — Issues found at the **chosen severity level**
  > Exit code reflects **filtered** output, not raw diagnostic count
- `2` — Error (path not found, config parse failure, failed write during `--fix`)

**`--fix` exit behavior**:

1. Compute diagnostics.
2. Apply safe fixes for fixable diagnostics at or above the chosen severity threshold.
3. Re-read/re-parse changed files.
4. Exit `0` if no remaining diagnostics at the chosen severity level, `1` if non-fixable
   diagnostics remain, or `2` if a write fails.

**`--watch` behavior**:

- Prints an initial diagnostic run, then prints subsequent runs after debounced file changes.
- Watches markdown files, config files, and configured attachment extensions.
- Ctrl+C exits cleanly with code `0`; watcher setup failures exit `2`.

**`--stdin` behavior**:

- Reads all stdin before parsing.
- Uses synthetic display path `<stdin>.md`.
- Runs single-file diagnostics only: NBSP, intra-document anchors/tags/link definitions, and
  syntax constructs discoverable within the input. No cross-file broken-link diagnostics.

**Exit code for file-level errors**: When a single file fails to parse (invalid markdown)
or is unreadable (permission denied), the exit code follows the same rules:

- If no other diagnostics exist in the workspace → exit `0` (the parse failure is a
  warning, not a diagnostic — it's logged but doesn't count as a "problem" with the docs)
- If other diagnostics exist (e.g., broken links in other files) → exit `1`

This means: a single broken markdown file in an otherwise clean workspace does **not**
cause a non-zero exit. The exit code reflects the state of the *documents*, not the
*file system*.

**Text Output Format**:

```
{file}:{line}:{col}: {severity}: {message} [DNL{NN}]
```

where `{NN}` is the two-digit code number. Full examples:
- `[DNL001]` — AmbiguousLink
- `[DNL002]` — BrokenLink
- `[DNL003]` — NonBreakableWhitespace

Example: `docs/note.md:14:3: error: Broken link to 'missing' [DNL002]`

### 11.3 `downlint server`

Starts the LSP server on stdin/stdout (JSON-RPC 2.0). Editor users invoke this command.

**Flags**:

- `--verbose` / `-v <LEVEL>` — Logging level to stderr (default: 2)
  > Controls `tracing-subscriber` output for LSP server operations. Same flag as §11.2 (check).
- `--wait-for-debugger` — Pause until a debugger attaches

**Global Flags** (inherited from §11.1):

`--help`, `--version`, `-q` / `--quiet` — see §11.1.

### 11.4 Logging Strategy

> **Note**: The `--verbose` flag on `downlint server` is the same logging flag as
> on `downlint check` (§11.2). Both use `tracing-subscriber` with the same
> `RUST_LOG` filter syntax.

**Crate**: `tracing` + `tracing-subscriber` (layered, structured logging)

**Interaction with `--min-severity`**: Logging (`--verbose`) and diagnostic filtering (`--min-severity`)
are completely independent:

- `--verbose 5 --min-severity error` → Full trace logging + only error-level diagnostics
- `--verbose 0 --min-severity info` → Silent logging + all diagnostics
- `--verbose` controls stderr; `--min-severity` controls stdout (or JSON body).

**Interaction with `--help` / `--version`**: These flags take precedence over all other flags.
If provided, the command prints help/version and exits immediately without any other processing.

---

## 12. Test Strategy

### 12.1 Framework & Tools

- **`cargo test`** — Rust's built-in test framework for unit tests (`#[test]`)
- **`insta`** — Snapshot testing crate. Snapshots stored in `_snapshots/` as JSON.
  - Full snapshots: `assert_snapshot!(output)` for large formatted outputs
  - Inline snapshots: `assert_snapshot!("expected line 1\nexpected line 2")` for small assertions
  - Snapshot updates: `cargo insta review` for interactive diff review
- **`criterion`** — Benchmarking for performance regression detection
- **`cargo-coverage`** (or `llvm-cov`) — Code coverage measurement

### 12.2 Test File Organization

Tests are organized **by feature/module** (not by test type), making it easy to see full coverage for each component:

| Component | Primary Test File | Focus Areas |
|-----------|-------------------|-------------|
| **Parser & AST** | `parser_tests.rs` | CST parsing, headings, comrak integration |
| | `scanner_tests.rs` | Sidecar scanner: wiki-links, markdown links/refs, tags, component ranges |
| | `ast_tests.rs` | CST→AST conversion, symbol occurrence generation |
| | `semantic_tests.rs` | Semantic token delta encoding |
| **Completion** | `compl_tests.rs` | Candidates, partial elements, styles |
| **References** | `refs_tests.rs` | Resolution, cross-folder, ambiguity |
| **Diagnostics** | `diag_tests.rs` | Broken links, edge cases, extra folders |
| **Features** | `code_action_tests.rs` | TOC generation, insertion, create missing file |
| | `refactor_tests.rs` | Rename (labels, headings, attachments) |
| | `lenses_tests.rs` | Code lens generation |
| **System** | `server_tests.rs` | Server negotiation, text sync, capability handling |
| | `workspace_tests.rs` | Folder management, extra folders, document lifecycle |
| | `state_tests.rs` | Client capabilities, file watching |
| | `conn_tests.rs` | Graph updates, incremental |
| **Utilities** | `config_tests.rs` | TOML parsing, merging |
| | `paths_tests.rs` | Path resolution |
| | `text_tests.rs` | Text utilities, byte offset ↔ UTF-16/UTF-8 LSP positions |
| | `check_tests.rs` | CLI behavior, `--fix`, `--watch`, `--stdin` |
| | `gitignore_tests.rs` | Pattern matching |
| | `symbols_tests.rs` | Symbol querying |
| | `misc_tests.rs` | Slug generation, utilities |
| | `mmap_tests.rs` | Multimap operations |
| | `suffix_tree_tests.rs` | Suffix tree operations |

**Naming Convention**:
- **Test files**: `{module}_tests.rs` (e.g., `parser_tests.rs`, `refactor_tests.rs`)
- **Test groups**: Nested `mod` blocks for test suites (e.g., `mod heading_tests`, `mod wiki_link_tests`)
- **Test functions**: `snake_case` with descriptive verbs: `test_parse_empty()`, `test_cross_file_diag_on_broken_wiki_links()`

### 12.3 Test Helpers & Fixtures

#### Core Builder Functions

```rust
// Build a test document with flexible configuration
fn fake_doc(content: &str, path: Option<&str>, root: Option<&str>, config: Option<&Config>) -> Doc

// Build a test folder from documents  
fn fake_folder(docs: impl IntoIterator<Item = Doc>, config: Option<&Config>) -> Folder
```

#### Key Utilities

| Function | Purpose | Example |
|----------|---------|---------|
| `fake_doc(content)` | Single doc test | `fake_doc("# Title")` |
| `fake_folder(docs)` | Folder with docs | `fake_folder([doc1, doc2])` |
| `check_snapshot(conn)` | Full snapshot assert | Compare formatted `Conn` graph state after an operation |
| `check_inline_snapshot(fmt, things)` | Inline snapshot assert | Compare array of strings (e.g., diagnostic messages) |
| `path_to_uri(path)` | Convert path to URI | `"fake.md"` → `"file:///.../fake.md"` |

#### Insta Snapshot Examples

```rust
// Full snapshot — assert entire formatted output
#[test]
fn test_parse_heading_with_wiki_link() {
    let doc = fake_doc("# Title\\n[[ref]]", Some("test.md"), None, None);
    let cst = parse_cst(&doc);
    assert_snapshot!(cst);
    // Produces: tests/snapshots/parser_tests__parse_heading_with_wiki_link.snap
}

// Inline snapshot — small expected values
#[test]
fn test_slug_generation() {
    let slug = Slug::from_heading_text("My Note Title!");
    assert_snapshot!(slug.as_str(), "my-note-title");
}

// Diff review workflow
// 1. Run tests: cargo test
// 2. Review changes: cargo insta review
// 3. Accept or reject interactively
```

#### Test Scenario Patterns

**Single-Document**:
```rust
let doc = fake_doc("# Title\n[[ref]]", None, None, None);
let folder = Folder::single_file(doc, None);
// Test behavior specific to single-file folders
```

**Multi-Document Workspace**:
```rust
let doc1 = fake_doc("[[doc2]]", Some("doc1.md"), None, None);
let doc2 = fake_doc("# Doc 2", Some("doc2.md"), None, None);
let folder = fake_folder(vec![doc1, doc2]);
// Test cross-file resolution
```

**Extra-Folder Scenario**:
```rust
let primary_doc = fake_doc("[[extra-doc]]", Some("main.md"), None, None);
let primary_folder = fake_folder(vec![primary_doc]);

let extra_doc = fake_doc("# Extra Doc", Some("extra-doc.md"), Some("extra"), None);
let extra_folder = fake_folder(vec![extra_doc]);

let result = check_folder(&primary_folder, &[&extra_folder]);
// Verify references resolve across boundaries
```

### 12.4 Test Categories & Strategies

#### Unit Tests (Direct Assertion-Based)
Tests that validate single functions or small components:
- `config_tests.rs` — TOML parsing, validation, config merging
- `paths_tests.rs` — Path normalization, URI conversion, relative path resolution
- `text_tests.rs` — Line-column position calculation, text range operations
- `misc_tests.rs` — Utility functions (slug generation, string operations)
- `mmap_tests.rs` / `suffix_tree_tests.rs` — Core data structure operations

Strategy: Use `assert_eq!()`, `assert!(...)` for direct assertions.

#### Snapshot Tests (Parser & AST Output)
Tests that validate complex, multi-line formatted outputs against baselines:
- `parser_tests.rs` — CST output (symbols, positions, ranges)
- `scanner_tests.rs` — raw-source scanner tokens and exact component ranges
- `ast_tests.rs` — AST structure, symbol occurrence generation
- `compl_tests.rs` — Completion candidate formatting
- `semantic_tests.rs` — Semantic token delta format

Strategy: Use `insta::assert_snapshot!()` for full output; inline snapshots for small expected values.
**Snapshot updates**: `cargo insta review` — review diffs before committing.

#### Integration Tests (Multi-Component)
Tests exercising multiple components together:
- `refs_tests.rs` — Reference resolution with incremental updates
- `diag_tests.rs` — Diagnostics across single/multi-file folders with extra folders
- `refactor_tests.rs` — Rename operations affecting multiple documents
- `workspace_tests.rs` — Folder management, document add/remove/update
- `conn_tests.rs` — Connection graph state after operations

Strategy: Use FakeDoc/FakeFolder builders to construct scenarios; assert on aggregated results.

### 12.5 Key Integration Test Scenarios

- **Snapshot-based**: Connection graph state after operations
- **Cross-folder**: Extra folder resolution, mutual references
- **Incremental**: Doc add/remove/change → Conn graph diff matches from-scratch
- **Validation mode** (dev build): Validate incremental results match from-scratch computation
- **Edge cases**: Non-breaking whitespace, emoji in headings, YAML frontmatter, math blocks

---

## 13. Operational Concerns

### 13.1 Performance Expectations

All measurements assume a release build (`cargo build --release`).

- **Startup time**: < 50ms for `downlint check` on a single file (no LSP server)
- **Diagnostic computation**: < 100ms for a typical document (~500k chars)
  - For large documents (> 500 pages), expect ~200-500ms (proportional to content)
  - For a typical workspace (100 files), expect ~5-20s total (sequential processing)
- **LSP server cold start**: < 200ms for first `initialize` request (workspace with ~50 files)
- **Memory**: < 50MB for typical workspaces (~100 markdown files)
  - LSP server with full workspace: ~30-80MB (depends on file count and wiki-link density)

**Optimization targets**: Single-file checks should be sub-50ms. Workspace-wide checks
are expected to be slower but should avoid quadratic behavior in link resolution.

**File size limits**: Files > 1MB may cause performance issues. The parser handles them
but there's no explicit size limit in v1. Large files are processed but may slow down
diagnostic computation.

### 13.2 Error Handling Strategy

| Error Type | Behavior | Exit Code |
|------------|----------|-----------|
| Config parse failure (`.downlint.toml` invalid) | Print error to stderr, abort | `2` |
| I/O error (path not found) | Print error to stderr, abort | `2` |
| File parse failure (invalid markdown) | Skip file, print warning to stderr | `0` or `1` — see §11.2 for exit code rules |
| Permission denied (can't read file) | Print error to stderr, skip file | `0` or `1` — see §11.2 for exit code rules |

**Binary file in workspace**: If a non-markdown/binary file is opened via LSP `didOpen`,
Downlint should ignore it unless its language ID or extension is configured as Markdown. Never
feed arbitrary binary data into the parser; log at debug level and produce no diagnostic.

**Error output**: All errors use stderr with the format `downlint: error: <message>`.
Diagnostics use stdout (or JSON body). Logging uses `--verbose` level. All three are
independent: errors always go to stderr, diagnostics go to stdout/JSON, logging goes
to stderr only when `--verbose` is set.

### 13.3 Development Tooling

Standard Rust tooling is expected:

- `cargo fmt` — Formatting (enforced in CI via `cargo fmt --check`)
- `cargo clippy` — Linting (enforced in CI, no warnings accepted)
- `cargo test` — Unit + integration tests (enforced in CI)
- `cargo build --release` — Release builds (benchmarking, perf testing)

**CI Pipeline**:
```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build --release
```

> The `--` after `cargo clippy` passes `-D warnings` to clippy (not cargo). This is
> required because clap-like argument parsing in cargo would otherwise consume the `--`.

**Editor Integration**: `rust-analyzer` for IDE support (completion, go-to-definition, diagnostics).

---

## 14. v1 Scope & Edge Cases

### 14.1 Excluded Features (v1)

The following features are **intentionally excluded** from the initial release.

#### Implementation Scope

| Feature | Reason | Future? |
|---------|--------|-------|
| `downlint/status` notification | LSP clients don't consume this — not applicable | No |
| `textDocument/foldingRange` | Useful but not required for diagnostics/link workflows | Deferred to v2 |
| `workspace/symbol` | Cross-document symbol search is lower value than references/completion | Deferred to v2 |
| `workspace/willCreateFiles` / `willRenameFiles` / `willDeleteFiles` | Requires pre-operation edit planning; `did*` notifications are enough for v1 indexing | Deferred |

**Included in v1**: code lenses, semantic tokens, pull/push diagnostics, `--fix`, `--watch`,
`--stdin`/`-`, and multi-workspace-folder indexing as described in §2 and §11.

**Design Principle**: v1 focuses on diagnostics, completion, references, rename, code actions,
code lenses, and practical CLI workflows. Everything else can be added iteratively.

---

### 14.2 Platform Considerations

- **Windows paths**: Backslashes (`\`) in links should be normalized to forward slashes
  by Downlint path resolution; do not rely on comrak for filesystem semantics.
- **Case sensitivity**: macOS (default HFS+/APFS) and Windows (NTFS) are case-insensitive;
  Linux is case-sensitive. Link resolution treats `Doc.md` and `doc.md` as the same file
  on case-insensitive filesystems. **Tradeoff**: if both `Doc.md` and `doc.md` exist in
  the same folder, links to either will produce an **ambiguous link** diagnostic (DNL001)
  since both files match the resolved name. On Linux, only the exact-case match resolves.
- **Path separators**: Always use forward slashes internally; convert to OS-native only for
  filesystem operations.

### 14.3 Implementation Edge Cases

#### Heading & Slug Generation

1. **Emoji in headings** — UTF-16/LSP ranges must handle surrogate pairs; GitHub-style slugging removes most emoji from the slug text. `"# 🚀 Launch"` → `"launch"`.
2. **CJK characters in headings** — preserved, not transliterated. `"# 中文标题"` → `"中文标题"`.
3. **Special characters** — GitHub-excluded punctuation is stripped. `"# What's Up?"` → `"whats-up"`.
4. **Consecutive separators** — preserve GitHub-compatible behavior; do not invent extra collapsing beyond the slug fixture behavior.
5. **Leading/trailing whitespace** — trimmed before processing. `"#   Title  "` → `"title"`.
6. **Numbers only** — `"# 42"` → `"42"` (numeric headings preserved as-is).
7. **GitHub disambiguation** — duplicate headings get `-1`, `-2` suffixes; the first occurrence keeps the base slug.

#### Link Resolution

8. **Trailing slash in directory** — `[[docs/]]` (with trailing slash) should not match a file named `docs.md`. Directory-style paths are invalid for file links
9. **Dot-relative paths** — `[[./doc]]` and `[x](./doc.md)` are explicit relative paths resolved from the source document directory; the `./` prefix is normalized away after anchoring.
10. **Double-encoded wiki-links** — `[[%5B%5Bdoc%5D%5D]]` produces a URL containing literal `[[` text, not a nested wiki-link. The decoded URL is treated as a document name
11. **Anchor links** — `[text](#section)` resolves to a heading/tag/link definition in the same document. `[text](other.md#section)` resolves `other.md` relative to the source document directory, then resolves `section` inside that target document.
12. **Unknown local extensions** — `[ref](data.json)` is treated as an attachment if `json` is in the configured attachment list; otherwise it is a **Warning** severity unresolved local resource unless an explicit suppression rule applies. External URIs with schemes remain suppressed.
13. **Unicode normalization** — `é` (U+00E9, precomposed) and `e\u0301` (U+0065 + combining accent) should be treated as equivalent for matching where normalization is required. Use the `unicode-normalization` crate for NFC/NFKC; `unicode-segmentation` is for grapheme boundaries, not normalization.

#### Wiki-Links & Embeds

14. **Embed prefix** — `![[doc]]` is detected by the sidecar scanner checking for `![[`; this sets `is_embed: true` without relying on comrak wikilink behavior.
15. **Title-only wiki-link** — `[[|My Title]]` has no doc, no heading, just a title. Used for display purposes; the resolved link target is still looked up by title slug
16. **Escaped wiki-link delimiters** — `[[F\#]]` — the `\#` is not treated as a heading delimiter. The sidecar scanner preserves raw ranges and decodes escapes per component.
17. **Nested wiki-link text** — `[text](%5B%5Binner%5D%5D)` — a markdown link whose URL contains encoded `[[...]]` is NOT treated as a wiki-link. Only `[[...]]` syntax triggers wiki-link parsing

#### Tags

18. **Tag in code blocks** — `\`\`\`\n#tag\n\`\`\`` — tags inside code blocks are NOT detected. The sidecar scanner skips comrak-masked code block ranges.
19. **Tag in heading text** — In `# Heading #tag`, the leading `#` is Markdown heading syntax and ignored. The trailing `#tag` appears in heading content and IS detected as a tag by the sidecar scanner.
20. **Subtag nesting** — `#rust/cli` and `#rust/cli/help` are distinct tags. Resolution treats them as separate symbols (string-equal, not hierarchical). For completion, `#rust/` can be used as a prefix to list all `#rust/*` children — this enables hierarchical completion but does not affect resolution or diagnostics.
21. **Case sensitivity** — `#Rust` and `#rust` are the same tag (case-insensitive matching). Normalized to lowercase for storage and lookup

#### Diagnostic Edge Cases

22. **Non-breaking whitespace** — `\u00a0` after `#` heading markers (e.g., `#\u00a0Title`) produces DNL003. Common when copying from word processors
23. **Broken embed link** — `![[nonexistent]]` produces DNL002 Error, the same as a broken wiki-link. Embeds are intentional, but unresolved embeds are still broken references.
24. **Ambiguous cross-folder** — primary folders are searched before extra folders. A primary match wins and does not become ambiguous with an extra-folder match. Ambiguity is reported when multiple destinations exist within the active search tier (multiple primary matches, or multiple extra-folder matches when the primary tier has no match).
25. **Shortcut link ambiguity** — `[text]` could be an image alt-text or a reference link. Always suppressed from diagnostics (too noisy)

---

## 15. Appendix — Target Project Structure

> **Note**: This is the *target* project structure for guidance during implementation.
> The actual codebase may differ as the project evolves.

### 15.1 Project Structure

Downlint is a **dual crate**: both a library (`downlint`) and a binary (`downlint`).

> **Why dual crate?** The library exposes a public API for test fixtures and potential
> downstream consumers (other crates, tools). The binary is the CLI entry point.
> For a single-binary tool, this adds minimal scaffolding overhead and makes it easy
> to export functions for testing without exposing internal implementation details.
>
> If the primary consumers are just tests, you could simplify to a single crate with
> `src/main.rs` re-exporting from `mod` blocks. The dual-crate layout is chosen for
> cleanliness and future-proofing (e.g., if another tool wants to use `downlint` as
> a library).

```
downlint/
├── Cargo.toml          # Workspace root: [package], [dependencies], [dev-dependencies]
├── src/
│   ├── lib.rs          # Library crate: public API (parse, check, resolve, complete)
│   ├── main.rs         # Binary crate: CLI entry point (clap subcommand dispatch)
│   ├── cli/            # CLI argument parsing and subcommand routing
│   │   ├── mod.rs      # Subcommand definitions (clap), global flags
│   │   └── check.rs    # `downlint` default logic (root inference, output)
│   ├── lsp/            # LSP server implementation
│   │   ├── mod.rs      # LSP server lifecycle
│   │   ├── server.rs   # Server struct, runtime setup (tokio)
│   │   ├── handlers/   # LSP method handlers
│   │   │   └── mod.rs  # Handler routing and registration
│   │   └── types.rs    # LSP type definitions (serde-annotated)
│   ├── parser/         # Markdown parsing (comrak integration, scanner, CST/AST)
│   │   ├── mod.rs      # Public parser API
│   │   ├── comrak.rs   # comrak integration, options setup
│   │   ├── scanner.rs  # Raw-source sidecar scanner for links/tags/ranges
│   │   ├── cst.rs      # CST construction from comrak AST + scanner tokens
│   │   ├── ast.rs      # AST construction, decoding
│   │   └── symbols.rs  # Symbol occurrence extraction (defs, refs, tags)
│   ├── resolution/     # Link resolution and connection graph
│   │   ├── mod.rs      # Public resolution API
│   │   ├── slug.rs     # Slug generation, encoding
│   │   ├── path.rs     # URL/path resolution logic
│   │   └── conn.rs     # Connection graph (per-folder)
│   ├── diagnostics/    # Diagnostic rules and publishing
│   │   ├── mod.rs      # Diagnostic rules, severity assignment
│   │   ├── rules.rs    # BrokenLink, AmbiguousLink, NonBreakableWhitespace
│   │   └── manager.rs  # DiagnosticsManager (debounce, publish)
│   ├── completion/     # Completion item construction
│   │   ├── mod.rs      # Completion item construction
│   │   └── candidates.rs # Candidate finding, fuzzy matching
│   ├── config/         # Configuration parsing and merging
│   │   ├── mod.rs      # Config parsing (TOML)
│   │   ├── user.rs     # User config loading (XDG paths)
│   │   └── project.rs  # Project config (.downlint.toml)
│   └── utils/          # Shared utilities
│       ├── mod.rs      # Shared utilities
│       └── text.rs     # LineMap, Text operations
├── tests/
│   └── integration.rs  # Integration test entry point
└── snapshots/          # Insta snapshot files (git-ignored)
```

**Cargo.toml structure** (dual-crate):

```toml
[package]
name = "downlint"
version = "0.1.0"
edition = "2024"
description = "A Markdown LSP and CLI diagnostic tool (Rust reimplementation of marksman)"
license = "MIT"
repository = "https://github.com/<user>/downlint"
readme = "README.md"
categories = ["command-line-utilities", "parsing"]

[lib]
name = "downlint"
path = "src/lib.rs"

[[bin]]
name = "downlint"
path = "src/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
comrak = "=0.52.0"
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
toml = "0.8"
notify = "8"
ignore = "0.4"
unicode-normalization = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
insta = { version = "1", features = ["json"] }
criterion = "0.5"
```

**Library public API** (`src/lib.rs` — exported types and functions):

```rust
// Public API for downstream consumers (tests, other crates)
pub mod parser;
pub mod resolution;
pub mod diagnostics;
pub mod completion;
pub mod config;

// Core entry points
pub use parser::{parse_document, ParseOptions};
pub use resolution::{resolve_links, resolve_reference, ConnectionGraph};
pub use diagnostics::{check_diagnostics, Diagnostic, DiagnosticConfig};
pub use completion::{complete_at, CompletionParams, CompletionItem};
pub use config::{Config, ConfigError};
```

**Dependency strategy**:

- **Minimum dependencies**: `serde`, `serde_json`, `comrak`, `tokio`, `clap`, `toml`, `notify`, `ignore`, `unicode-normalization`, `tracing`
- **Optional dependencies**: `petgraph` (graph operations, v2 — see §16.3)
- **Dev dependencies**: `insta` (snapshots), `criterion` (benchmarks)

> **Rule of thumb**: Prefer standard library + minimal dependencies. External crates should
> solve problems the stdlib can't (e.g., `comrak` for markdown parsing, not for path handling).

### 15.2 Distribution

Downlint targets multiple distribution channels:

| Channel | Method | Command |
|---------|--------|--------|
| **crates.io** | Publish to Rust package registry | `cargo publish` |
| **Homebrew** | Tap formula with binary release | `brew install <user>/downlint/downlint` |
| **GitHub Releases** | Pre-built binaries for macOS/Linux/Windows | Download from releases page |
| **cargo install** | Build from source | `cargo install downlint` |

**Release process**:
1. Tag version in git (`v0.1.0`)
2. CI builds binaries for all platforms (cross-compilation via `cross` or GitHub Actions)
3. Publish to crates.io (`cargo publish`)
4. Create GitHub release with attached binaries
5. Update Homebrew formula (via PR or automated tap)

---

## 16. Future Roadmap

The following features are planned for post-v1 releases.

### 16.1 CLI Enhancements (post-v1)

`--fix`, `--watch`, `--stdin`, and `-` are included in v1. Post-v1 CLI work should focus on
additional safe fixes, richer watch output, stdin path metadata, and CI-oriented summaries.

| Feature | Purpose |
|---------|---------|
| Additional fixes | Extend `--fix` beyond DNL003 when edits are unambiguous |
| `--stdin-path <PATH>` | Let stdin diagnostics participate in workspace-relative resolution |
| Watch UI modes | Compact/clear-screen output for long-running watch sessions |

### 16.2 LSP Enhancements (v2)

| Method | Reason |
|--------|--------|
| `textDocument/foldingRange` | Partial — heading folding only (H1-H6) |
| `workspace/symbol` | Cross-document symbol search — low value for markdown workflows |

### 16.3 Dependency Additions (v2)

| Crate | Purpose |
|-------|--------|
| `petgraph` | Graph operations (replaces hand-rolled adjacency list in §10.3) |
