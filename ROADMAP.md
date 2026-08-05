# Downlint Roadmap

This file tracks planned features and LSP methods for future versions of **Downlint**.
See [spec.md](./spec.md) for the current v1 implementation spec and
[CHANGELOG.md](./CHANGELOG.md) for release-by-release notes.

---

## Recently Shipped

### `[uri]` — External Asset URI Mapping (RFC 0006)

Maps URI-style wiki-link targets (`onedrive://work/...`, `s3://reports/...`) to
local filesystem roots so cloud-stored assets can be validated like any other link.

- **Order-based prefix matching**: more-specific prefixes win.
- **Trailing-slash normalization**: `onedrive://work` and `onedrive://work/` both match.
- **Cross-platform home expansion**: `~` uses `dirs::home_dir()` (Windows-safe).
- **Env-var expansion in `root`**: `$VAR` and `${VAR}` are expanded at startup.
- **Per-file sync (`{path}` placeholder)**: never syncs an entire mount.
- **Batched + cached sync**: results shared across CLI runs and LSP keystrokes.
- **Explicit `--allow-uri-sync` gate**: subprocess execution requires opt-in.
- **`--no-uri-hints` opt-out**: disables the "no URI mapping found" hint.
- **Diagnostics**: `DNL006` (no-mapping hint) and `DNL007` (sync skipped notice),
  both info-level.

See [rfc/0006-external-asset-uri-mapping.md](./rfc/0006-external-asset-uri-mapping.md)
for the rationale and [rfc/0007-uri-mapping-phase-2.md](./rfc/0007-uri-mapping-phase-2.md)
for Phase 2 follow-ups.

### `[uri]` Phase 2 — Sync Hardening (RFC 0007)

Five follow-ups from the Phase 1 review and the original RFC's Phase 2/3
sections, all shipped together:

- **`verify_cmd` per mapping**: post-warm placeholder detection via a
  user-configured command (e.g. `["file", "{path}"]`). Authoritative
  override when heuristics disagree.
- **Auto-detection heuristics**: built-in checks for OneDrive resource
  forks, iCloud `.icloud` siblings, and 0-byte cloud files. Toggled via
  `[uri].auto_verify = "on" | "off" | "onedrive-only" | "icloud-only"`.
- **`DNL008 SyncFailureWarning`**: info-level diagnostic that distinguishes
  `warm_required = false` (soft) from `warm_required = true` (hard)
  warm failures. Emitted alongside `DNL002` only when the user opted into
  lenient warming.
- **Batch fan-out**: `batch_size` now actually batches. Two tiers:
  positional `{path}` (default) and `{paths}` newline-stdin (opt-in for
  `rclone` / `aws s3 sync`). 128 KiB argv cap with one-time `DNL009`
  fallback diagnostic.
- **`downlint warm-uri-mappings` subcommand**: cache-warming runner.
  Always requires `--allow-uri-sync`; prints per-mapping summary; exits
  1 on any failure.
- **LSP warming flags**: `downlint server --allow-uri-sync` (and
  `--no-uri-hints`, `--uri-sync-batch-size`) now thread through to
  `ServerState` so the LSP can honor the user's warming intent.

### `[uri]` Phase 2.1 — Vocabulary Rename (RFC 0008)

The Phase 1 / Phase 2 release used the word "sync" everywhere, which
implies two-way sync (Obsidian Sync, Notion sync) that downlint doesn't
do. RFC 0008 renamed the user-facing surface to use "warm" instead:

- `sync_cmd` → `warm_cmd`
- `sync_required` → `warm_required`
- `sync_timeout` → `warm_timeout`
- `downlint sync` → `downlint warm-uri-mappings` (alias `warm-uri`)
- `DNL007` diagnostic message updated to name the new subcommand.

Internal types (`SyncRunner`, `SyncDecision`) keep their existing names
per RFC 0008's "internal names" section. The `--allow-uri-sync` flag
name stays the same — it's the safety gate regardless of which verb the
subcommand uses.

Migration: clean break. Old keys produce a parse error listing the new
field names. One-line `sed` for users updating existing configs.

---

## Version 1 (Current)

v1 focuses on core features: diagnostics, completion, rename, and basic code actions.
All v1 details are in [spec.md](./spec.md).

---

## Version 2 — Planned Features

### LSP Methods

| Method | Priority | Notes |
|--------|----------|-------|
| `textDocument/codeLens` (link counts) | Medium | Show reference counts on links/headings. Adds complexity but useful for navigation |
| `textDocument/codeLens` (TOC lens) | Medium | Display TOC as a code lens for quick regeneration |
| `workspace/symbol` (cross-doc search) | Medium | Search symbols across all documents in workspace. Niche but valuable for large wikis |
| `textDocument/foldingRange` | Medium | Full folding support: headings, lists, code blocks, frontmatter |

### New Features

| Feature | Notes |
|---------|-------|
| Progress reporting | `window/workDoneProgress/create` + `$/progress` for long-running operations (workspace-wide diagnostics, large renames) |
| File operation support (improved) | Better handling of `willCreateFiles`, `willRenameFiles`, `willDeleteFiles` with pre-computation |
| Configuration change notification | `workspace/didChangeConfiguration` — respond to editor config changes without restart |

### Improvements

| Area | Notes |
|------|-------|
| Incremental sync | Improve reliability for incremental text sync (currently defaults to full) |
| Diagnostic caching | Cache diagnostic results per document to speed up re-checks |
| Multi-root workspaces | Support multiple independent LSP roots (beyond single root + extra_folders) |

---

## Version 3 — Future / Exploratory

### LSP Methods

| Method | Notes |
|--------|-------|
| `textDocument/declaration` | Markdown doesn't have a "declaration" concept per se, but could map to "go to source definition" for wiki-links |
| `textDocument/inlayHint` | Show hint text for wiki-link targets, tag references |
| `textDocument/formatting` | Markdown formatting is tricky (whitespace-sensitive). Could support heading alignment, list renumbering |
| `textDocument/selectionRange` | Multi-cursor selection across linked documents |
| `textDocument/linkedEditingRange` | Edit heading title and all wiki-link references simultaneously |

### New Features

| Feature | Notes |
|---------|-------|
| Tag-based navigation | `workspace/symbol` for tags, go-to-tag functionality |
| Document outline improvements | Better hierarchical document symbol tree |
| Smart rename for headings | Rename heading title and propagate across ALL references (wiki-links, markdown links, tags) |
| Mermaid / diagram support | Parse and validate Mermaid diagrams in markdown |

### Experimental

| Feature | Notes |
|---------|-------|
| LSP 3.17+ enhancements | `$/cancelRequest`, improved progress tokens |
| Server-side completions | Pre-compute completion lists for faster responses |
| Web-based UI | Browser-based markdown editor with downlint integration |

---

## Excluded Features (Unlikely to be Implemented)

These features are **unlikely** to be added to downlint:

| Feature | Reason |
|---------|--------|
| `textDocument/typeDefinition` | Markdown has no type system |
| Signature help | Not applicable to markdown |
| Document formatting (full) | Markdown is whitespace-sensitive; full reformatting risks breaking content |
| `downlint/status` notification | LSP clients don't consume this; not useful for editor UX |

---

## How to Read This Roadmap

- **v2 Planned Features** — Likely to ship in the next major release
- **v3 Future / Exploratory** — Interesting ideas, not yet committed
- **Excluded Features** — Intentionally out of scope

Priorities may shift as the project evolves. This is a living document.
