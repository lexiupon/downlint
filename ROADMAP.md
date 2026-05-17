# Downlint Roadmap

This file tracks planned features and LSP methods for future versions of **Downlint**.
See [spec.md](./spec.md) for the current v1 implementation spec.

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
