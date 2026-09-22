# RFC 0015 — Graph query commands

**Status:** Proposed
**Date:** 2026-09-23
**Scope:** New read-only CLI subcommand `downlint graph <query>`. No changes to
resolution semantics, linting, or existing commands.

## 1. Motivation

downlint already builds a complete link graph (`ConnectionGraph`) for `check` and
`resolve`, but exposes it only as *diagnostics* (broken/ambiguous links) and as a
*single-target* lookup (`resolve`). There is no way to ask the natural
knowledge-base navigation questions:

- **backlinks** — which notes link to a given note? (Obsidian's "Backlinks" panel)
- **links** — what does a given note link out to? (Obsidian's "Outgoing links" panel)
- **orphans** — which notes have no incoming links? (KB hygiene / entry-point audit)
- **deadends** — which notes have no outgoing links? (KB hygiene)
- **unresolved** — a clean, scriptable list of broken links (vs. `check`'s mixed
  diagnostics).

All five are **pure in-memory queries** over the existing `ConnectionGraph`
(`src/resolution/conn.rs`). No new resolution logic is introduced; this is a
projection layer, exactly like `resolve` (RFC 0012).

## 2. Command shape

A single `graph` umbrella subcommand with five queries. `graph` (not `diagnose`)
because these are *queries over the link graph*, not diagnostics — `check` owns
diagnostics. Five new top-level commands would clutter the CLI; `graph` is the
established vocabulary (Obsidian's graph view / backlinks). `resolve` stays
top-level (single-target lookup, already shipped).

```
downlint graph backlinks <FILE>
downlint graph links <FILE>
downlint graph orphans
downlint graph deadends
downlint graph unresolved
```

Common flags: `--root <PATH>` (workspace root, like `check`/`resolve`). No
`--stdin` (these queries need the full workspace index; stdin is a `check`
concept). No `--allow-uri-sync` (a read-only query must not run a schema's
`verify_cmd` subprocess — see §6 Security).

## 3. The graph model

### 3.1 Complete graph (all documents resolve)

`resolve_links` (`src/resolution/mod.rs:337`) skips documents whose `is_source`
is false — i.e. mounted docs whose mount has `lint = false`. Their references
never enter the graph. For a *navigation* query that is wrong: a note mounted
with `lint = false` still has links, and those links matter for backlinks /
orphans / deadends.

**Decision:** the graph command builds a *complete* graph by forcing
`is_source = true` on every document before calling `resolve_links`:

```rust
let mut input = ResolveInput::from_workspace(&workspace);
for doc in &mut input.documents {
    doc.is_source = true; // graph queries see every doc's links, not just linted ones
}
let graph = resolve_links(input);
```

This is localized to the graph command; the `check`/lint path is untouched
(`check` still skips `lint = false` mounts, so no diagnostic behavior changes).
`is_source` has no other consumer in the query path (the diagnostics layer's
`is_source` filter is never reached, since the graph command does not run
diagnostics).

### 3.2 Document edge

Nodes are indexed documents. A **document edge A→B** (B is an indexed document)
exists when document A has a reference with a destination `d` such that:

```
d.path == B.path  &&  d.kind ∈ { Document, Heading }
```

- `d.path` is the destination's **filesystem path**. For the two kinds the edge
  test considers (`Document`, `Heading`), it is the *target document's* path
  (`find_doc_matches` pushes `path: doc.path.clone()` for the matched target doc,
  `src/resolution/mod.rs:1201`; `index_document` does the same for headings,
  `src/resolution/mod.rs:391`). Attachment/Directory destinations use the resolved
  file/folder path instead — and are excluded by the kind filter.
- `Heading` must be included: `[[Note#Heading]]` resolves to a `Heading`-kind
  destination whose `path` is the containing note (`src/resolution/mod.rs:1119`).
  Filtering on `Document` alone would silently drop every heading-anchored
  backlink, which is the common case.
- `Attachment`, `Directory`, `Tag`, and `LinkDefinition` destinations are
  excluded: they are not "links to a note".
- Edges are defined over **resolved** references only. An *ambiguous*
  reference (`[[Ambig]]` matching several docs) is not a confirmed edge to any
  of them: it contributes to neither `backlinks` nor the orphan/deadend counts.
  `check` already surfaces it as `link/ambiguous`.

### 3.3 `<FILE>` argument (backlinks / links)

`<FILE>` names a document by **workspace-relative path** (primary docs) or
**namespace path** (mounted docs), matched case-insensitive, exactly like
`resolve --from` (`src/cli/resolve.rs:94`). It must name a document in the
index; a typo'd `<FILE>` is an error (exit 1), not a silent empty result. No
title/stem fuzzy matching in v1 (clear error message instead). For a mounted
document the namespace path is the `as` prefix (leading `/` stripped, per the
mount namespace convention, `src/resolution/mod.rs:192`) joined with the doc's
relative path — with `as = "/kb"`, pass `kb/doc.md`. The path shown in
`backlinks` output is exactly what you pass back in (same matching rules as
`resolve --from`).

## 4. Per-command semantics

### 4.1 `backlinks <FILE>`

List every document edge A→FILE, i.e. each reference in any document A whose
destination is FILE (direct or via heading). One line per *reference occurrence*
(a note linking twice is listed twice, at each location).

### 4.2 `links <FILE>`

List **all** outgoing references from FILE (not just document edges) — resolved
document links, broken links, attachments, folders — each with its resolution
status. This is the full "outgoing links" panel. (See §4.5 for the deliberate
divergence between `links` and `deadends`.)

Implementation note: `UnresolvedReference`/`AmbiguousReference` carry a plain
`target: String`; a resolved reference's target-as-written is extracted from its
`Ref` (`Wiki.target`, `Inline.target`, or the reference-style `label`).

### 4.3 `orphans`

Documents with **no incoming document edge** (no reference in any document
resolves to them). A self-link counts as an incoming edge (a note linking to
itself is not an orphan). The entry/root note is typically an orphan by design —
the report is informational.

### 4.4 `deadends`

Documents with **no outgoing document edge** (no reference from them resolves to
any document). See §4.5.

### 4.5 `unresolved`

Every reference in `graph.unresolved_references` (truly broken links), as
`source:line:col  target-as-written`. Ambiguous links are a separate category
(they *do* resolve, to several targets) and are excluded here; `check` reports
them as `link/ambiguous`.

**`links` vs `deadends` (documented divergence):** `links <A>` shows *all*
outgoing references; `deadends` counts *document edges* only. A note whose only
outgoing references are broken links or non-document targets therefore appears
in `links <A>` (with `<unresolved>` / attachment status) **and** in `deadends`.
This is intentional: a broken or image-only link does not let you navigate to
another note, so the note is a navigation deadend even though it has outgoing
references. `links` makes the reason visible.

## 5. Output format

Line-oriented, sorted, deterministic, 1-based. Paths are the document's
workspace-relative path (primary) or namespace path (mounted). `line:col` is
derived from the reference's `full_range.start` via
`structure.text.to_lsp_position(offset, PositionEncoding::Utf8)`
(`src/utils/text.rs:201`), the same helper `check` uses
(`src/cli/check.rs:186`).

- `backlinks <FILE>`:
  ```
  notes/a.md:12:5
  notes/b.md:7:1
  ```
- `links <FILE>`:
  ```
  12:5  Target        →  notes/target.md
  15:2  missing.md    →  <unresolved>
  18:1  Ambig         →  <ambiguous>
  20:3  pic.png       →  images/pic.png
  ```
  (target-as-written, then status: resolved destination path, `<unresolved>`,
  or `<ambiguous>`.)
- `orphans` / `deadends`:
  ```
  notes/orphan1.md
  notes/orphan2.md
  ```
- `unresolved`:
  ```
  notes/a.md:15:2  missing.md
  notes/b.md:20:1  gone.md
  ```

Empty result → no output (the exit code carries the signal). Text-only in v1;
`--format json` is a follow-up (see §8).

## 6. Exit codes

Following the project's 0/1/2 convention:

| Command | 0 | 1 | 2 |
|---|---|---|---|
| `backlinks` / `links` | `<FILE>` is in the index (list may be empty) | `<FILE>` not in the index | bad args / config error |
| `orphans` / `deadends` / `unresolved` | none found | ≥1 found | bad args / config error |

The per-file commands' exit code reflects *was the argument valid*; the report
commands' exit code reflects *were any found* (so `downlint graph orphans \|\|
echo clean` works as a CI gate).

**Security:** the graph command uses the default `UriOptions`
(`allow_sync = false`), so a schema's `verify_cmd` is **never** executed
(`src/resolution/mod.rs:628` gates it on `allow_sync`). URI-scheme references
are resolved to their mapped path (stat only). There is no `--allow-uri-sync`
flag on `graph`.

## 7. Code impact

- **`src/cli/mod.rs`** — add `Graph(GraphArgs)` to the `Command` enum + dispatch;
  `GraphArgs` with a nested `GraphQuery` subcommand enum
  (`Backlinks { file }`, `Links { file }`, `Orphans`, `Deadends`, `Unresolved`)
  and a `--root` flag.
- **`src/cli/graph.rs`** (new) — `run_graph(GraphOptions) -> i32`:
  - discover workspace (`discover_workspace`, as in `resolve.rs`),
  - build the complete graph (§3.1),
  - resolve `<FILE>` to a `ResolvedDocument` (reuse the `resolve_from` matching
    pattern, `src/cli/resolve.rs:94`),
  - run the query over `ConnectionGraph` (§3.2, §4),
  - format + print (§5), return the exit code (§6).
  - Query helpers (document-edge test, line:col rendering) as pure functions for
    unit testing.
- **No changes** to `src/resolution/*`, `src/diagnostics/*`, or the lint path.

## 8. Tests

- **Unit** (in `src/cli/graph.rs`): document-edge classification (Document vs
  Heading vs Attachment/Tag/Directory), `<FILE>` matching (fs path, namespace
  path, case-insensitivity, not-found), line:col rendering, exit-code mapping.
- **CLI subprocess** (`tests/cli_graph_tests.rs`, `assert_cmd` + `tempfile` +
  `predicates`), mirroring `tests/cli_resolve_tests.rs`:
  - `backlinks` lists the correct sources (incl. a heading-anchored link);
  - `backlinks` on a file with no incoming links → empty, exit 0;
  - `backlinks`/`links` on a file not in the index → exit 1;
  - `links` shows resolved + unresolved + attachment references;
  - `orphans` lists notes with no incoming links (a self-link is not an orphan);
  - `deadends` lists notes with no outgoing document links;
  - `unresolved` lists broken links as `source:line:col  target`;
  - a `lint = false` mount's links are visible (complete-graph check);
  - `--root` honored; bad args → exit 2.

## 9. Spec impact

- **`spec/downlint.md`** — new "Graph queries" section: the five subcommands,
  output format, exit-code table, the document-edge definition, the
  complete-graph note, and the `links`/`deadends` divergence.
- **`spec/linting.md`** — no changes (no new diagnostics, no resolution-semantics
  change).

## 10. Risks / open questions

1. **`links` vs `deadends` divergence** (§4.5) is the main semantic judgment
   call. Alternative: define `deadends` as "no outgoing references at all" so it
   matches `links` exactly. Chosen: document-edge-based (navigation-focused).
   Flagged for user sign-off.
2. **Text-only output.** `--format json` is deferred. If scripting is a near-term
   need, it should be added in the same release (the structs serialize trivially
   with `serde_json`, already a dependency).
3. **`<FILE>` is path-only.** Title/stem fuzzy matching is a possible extension;
   deferred to keep the argument unambiguous.
4. **Cost.** Building the complete graph re-parses and re-resolves every
   document (same cost as `check`). Fine for interactive use; a future
   `--server` integration could reuse a warm index (out of scope).
5. **Self-links** count as incoming edges (orphans). Unlikely to matter in
   practice; noted for completeness.
