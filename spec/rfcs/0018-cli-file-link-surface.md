# RFC 0018 — CLI surface: `file` and `link` command groups

**Status**: Proposed
**Date**: 2026-09-23
**Scope**: CLI subcommand names and grouping only. No changes to resolution
semantics, rename semantics, diagnostics, exit-code *schemes*, or the LSP surface.

---

## 1. Summary

Regroup the CLI around two nouns — `file` and `link` — replacing the current mix of
verb-noun (`rename-file`, `rename-link`) and noun-verb (`graph <query>`) commands and the
bare top-level verb `resolve`. The `graph` namespace is removed; its queries move under
`link`, with `backlinks`+`links` consolidated into `link graph <FILE>` and
`orphans`+`deadends` consolidated into `link coverage`.

## 2. Motivation

1. **Mixed naming conventions.** The current surface mixes verb-noun
   (`rename-file`, `rename-link`) with noun-verb (`graph backlinks`). Noun-first
   grouping unifies the surface on one convention.
2. **`resolve` is misfiled.** It is a bare top-level verb, but it is a
   link-target operation ("what does this target point to?"). It belongs with the
   other link commands.
3. **`graph` is a leaky abstraction.** The namespace is named after the internal
   `ConnectionGraph` type, not a user concept. Users think in terms of *links*, not
   *the graph*.
4. **`--from` means three different things today**: a file path (`rename-file`), a bare
   identifier (`rename-link`), and a source document (`resolve`). Under noun-first
   grouping each flag's meaning is local to its noun: `file rename --from <path>` vs
   `link rename --from <identifier>` vs `link resolve --from <doc>`.
5. **Mirrors the LSP namespace.** Code actions are `refactor.rename.file` and
   `refactor.rename.link-target`; the CLI becomes `file rename` and `link rename`, so
   the two surfaces use one taxonomy.
6. **Room to grow.** New link queries (e.g. reachability) and, if the LSP-only decision
   is ever revisited, a CLI heading rename, have an obvious home without flattening the
   top level further.

## 3. Proposed surface

| Today | After | Notes |
|---|---|---|
| `downlint [PATH]` / `check` | unchanged | still the default command |
| `downlint init` | unchanged | |
| `downlint server` | unchanged | |
| `downlint info` | unchanged | |
| `downlint rename-file` | **`downlint file rename`** | no semantic change |
| `downlint rename-link` | **`downlint link rename`** | no semantic change |
| `downlint resolve <TARGET>` | **`downlint link resolve <TARGET>`** | no semantic change |
| `downlint graph backlinks <FILE>` | **`downlint link graph <FILE>`** (incoming section) | consolidated |
| `downlint graph links <FILE>` | **`downlint link graph <FILE>`** (outgoing section) | consolidated |
| `downlint graph orphans` | **`downlint link coverage`** (orphans section) | consolidated |
| `downlint graph deadends` | **`downlint link coverage`** (deadends section) | consolidated |
| `downlint graph unresolved` | **`downlint link unresolved`** | no semantic change |

Resulting top level: `check` (default), `init`, `server`, `info`, `file`, `link`.

```
downlint check
downlint file rename
downlint link rename
downlint link resolve <target>
downlint link graph <file>
downlint link coverage
downlint link unresolved
```

## 4. Command-by-command

### 4.1 `file rename` (was `rename-file`)

No semantic change. Required `--from <PATH>` / `--to <PATH>` (workspace-relative),
`--dry-run`, `--root`, `--server`, `--verbose`, `--quiet`. Kind-class (markdown vs
attachment) is still inferred from the source extension. Exit codes unchanged:
`0` success/no-op · `1` blocked by pre-existing diagnostic · `2` conflict ·
`3` bad arguments / config error / source not found.

### 4.2 `link rename` (was `rename-link`)

No semantic change. Required `--from <ID>` / `--to <ID>` (bare identifiers; `/ # | ( ) [ ]`
rejected), `--dry-run`, `--root`, `--server`, `--verbose`, `--quiet`. No disk move.
Exit codes unchanged (same scheme as `file rename`).

### 4.3 `link resolve` (was `resolve`)

No semantic change (RFC 0012): positional `<TARGET>`, `--from <DOC>`, `--root`,
`--format`, `--include-prefix`, `--allow-uri-sync`, `--verbose`. Same statuses,
exit codes, and output shapes.

### 4.4 `link graph <FILE>` (was `graph backlinks <FILE>` + `graph links <FILE>`)

One command, both directions. `<FILE>` matching, the *document link* definition,
self-link counting, and the **complete graph** build (every document's links resolved,
including `lint = false` mounts) all carry over from RFC 0015 unchanged.

- **Text output** (default): two sections, each sorted, 1-based:
  ```
  incoming (2)
    notes/b.md:3:1
    notes/c.md:10:5
  outgoing (3)
    2:1  [[c]]  →  notes/c.md
    5:1  [[x]]  →  <unresolved>
    8:1  [[a|A]]  →  <ambiguous>
  ```
  (Line formats are exactly today's `backlinks` and `links` formats, under section
  headers. An empty section renders its header with a count of `0` and no rows.)
- **JSON output**: one envelope
  `{"query": "graph", "file": <canonical namespace path>, "incoming": [...],
  "outgoing": [...]}`. `incoming[]` entries are today's `backlinks` shape
  `{"source", "line", "col"}`; `outgoing[]` entries are today's `links` shape
  `{"line", "col", "target", "status", "destination"}`. Both arrays are `[]` when
  empty.
- **Exit codes**: `0` `<FILE>` in index (either or both sections may be empty) ·
  `1` `<FILE>` not in index · `2` bad args / config error. Independent of
  `--format`, as today.

Rationale for consolidating: an agent that wants one direction picks the JSON field;
there is no per-direction ergonomics that requires two subcommands, and consolidation
removes the need to name an "outgoing links" subcommand (the `link links` awkwardness).

### 4.5 `link coverage` (was `graph orphans` + `graph deadends`)

One "how well does the link web cover your notes" report. The orphan/deadend
definitions (document links only, self-links count) carry over from RFC 0015
unchanged.

- **Text output** (default):
  ```
  orphans (1)
    notes/old-draft.md
  deadends (2)
    notes/stub.md
    notes/leaf.md
  ```
  (One note path per line, sorted; empty sections render header + `0`.)
- **JSON output**: `{"query": "coverage", "orphans": [{"path"}],
  "deadends": [{"path"}]}`.
- **Exit codes**: `0` neither list non-empty · `1` at least one list non-empty ·
  `2` bad args / config error — preserving today's CI-gate contract
  (`downlint link coverage || echo clean`).

Rationale for consolidating: orphans and deadends are two sections of one audit
("where are the gaps in the link web"), and "coverage" frames the command as a
property of the web, which fits the `link` noun better than two note-returning
subcommands would.

### 4.6 `link unresolved` (was `graph unresolved`)

No semantic change. Every broken link as `source:line:col  target`;
`0` none found · `1` found · `2` bad args / config error. JSON envelope
`{"query": "unresolved", "results": [...]}` unchanged.

Stays separate from `link coverage` on purpose: it returns *links* (fix: repair the
target), while `coverage` returns *notes* (fix: restructure the vault).

### 4.7 Removed

Top-level `graph`, `resolve`, `rename-file`, `rename-link`. No replacement aliases
(see §6).

## 5. What does not change

- `check` (including as the no-subcommand default), `init`, `server`, `info`.
- All resolution semantics (`spec/linting.md` RES rules), all rename semantics
  (conflicts, blocking rule, text-first-then-disk apply), all diagnostics.
- The LSP surface: code action names (`refactor.rename.file`,
  `refactor.rename.link-target`, `refactor.rename.heading`) are unchanged — the CLI
  now *mirrors* them rather than diverging.
- Exit-code *schemes* per command (only the command names and two consolidated
  output shapes change).
- The complete-graph build and `<FILE>` matching rules (RFC 0015).

## 6. Backward compatibility

This is a **breaking change** to the CLI surface. The project is pre-1.0 and the
CHANGELOG explicitly permits breaking changes pre-1.0; agent workflows that script
these commands are internal and current.

**Decision: hard cutover.** No deprecated aliases. Rationale: aliases would keep two
taxonomies alive in docs, help text, and muscle memory for no external-compat benefit;
the commands are young (RFC 0012/0015/0016 era) and pre-1.0.

*Alternative (rejected):* keep old names as hidden aliases for one release, printing a
stderr deprecation warning. Revisit only if an external consumer surfaces.

## 7. Alternatives considered

| Option | Verdict |
|---|---|
| Keep the flat verb-noun surface (`rename-file`, `rename-link`, `resolve`, `graph …`) | Rejected: mixed conventions; `resolve` misfiled; `graph` named after an internal type. |
| `graph resolve` / `graph rename` | Rejected: `resolve`/`rename` are per-target operations; `graph` denotes the collection (orphans/deadends are global properties). "Rename a graph" reads wrong. |
| `link connected <FILE>` instead of `link graph <FILE>` | Rejected (narrowly): "connected" leans transitive/reachability in graph parlance; reserving it keeps the name free for a future hop-depth / path-between query. `graph` is also the spec's existing vocabulary ("link graph"). |
| `link to <FILE>` / `link from <FILE>` | Rejected: collides with the `--from`/`--to` flag vocabulary used by `rename` and `resolve`. |
| `link links <FILE>` | Rejected: double "link link". |
| A `note` namespace for orphans/deadends (`note orphans`) | Rejected: a third noun for two commands; breaks the `file`/`link` taxonomy. |
| Keep `link orphans` / `link deadends` as separate subcommands | Viable fallback; accepted in favor of `link coverage`, which consolidates and frames the query as a property of the link web. |

## 8. Non-goals

- No LSP changes.
- No new resolution or rename semantics; no new diagnostics.
- Heading rename remains **LSP-only** per the current decision
  (`spec/downlint.md` §6: "no CLI heading-rename subcommand"). This RFC does not
  revisit that decision; if it is revisited, `heading rename` is the natural slot
  (a third noun, added then).
- No whole-vault graph view / visualization. `link graph <FILE>` requires `<FILE>`;
  a no-argument vault-wide form is a possible future addition and is deliberately
  not claimed here.
- Reachability queries (`link connected a.md b.md`, `--depth`) are out of scope; the
  name is reserved.

## 9. Implementation impact

Code (all mechanical — the rename/resolution engines in `src/rename/` and
`src/resolution/` are untouched):

- `src/cli/mod.rs` — clap `Command` enum: replace `RenameFile`/`RenameLink`/`Resolve`/
  `Graph` with `File { #[command(subcommand)] }` and `Link { #[command(subcommand)] }`
  groups; update dispatch in `run()`.
- `src/cli/rename.rs` — split into `src/cli/file.rs` (`file rename`) and
  `src/cli/link.rs` (`link rename`); shared plan/apply helpers stay shared.
- `src/cli/resolve.rs` — moves under the `link` group (module may become
  `src/cli/link_resolve.rs` or stay `resolve.rs` re-exported).
- `src/cli/graph.rs` — `backlinks`+`links` merge into one query producing both
  sections; `orphans`+`deadends` merge into `coverage`; `unresolved` unchanged.
  Output builders gain section headers / split JSON arrays.
- Tests: command-string updates in `tests/cli_rename_tests.rs`,
  `tests/cli_resolve_tests.rs`, `tests/cli_graph_tests.rs`, `tests/integration.rs`,
  `tests/schemas.rs`; new tests for the consolidated `link graph` (both sections,
  empty-section rendering, JSON shape) and `link coverage` (exit 1 when either list
  is non-empty).
- No shell-completion artifacts exist to regenerate (clap without `clap_complete`).

Docs:

- `spec/downlint.md` — §3.1 command table ("Nine subcommands" → six top-level
  entries) and the `resolve`/`graph` sections (rewritten as `link resolve` /
  `link graph` / `link coverage` / `link unresolved`); §4 rename-kind table
  (triggers become `file rename` / `link rename`); §5 workflows
  (`graph backlinks` → `link graph`, etc.).
- `README.md` — Rename/CLI examples and the command table.
- `CHANGELOG.md` — entry under the next 0.x release (breaking, pre-1.0).
- `TODO.md` — any item referencing the old names.

## 10. Open questions

None blocking. Two notes for implementation time:

1. Module layout for the `link` group (one `link.rs` with submodules vs
   `link/{mod,resolve,graph,coverage}.rs`) — implementer's choice.
2. Whether `link graph`'s section headers should be suppressible for machine text
   parsing (JSON is unaffected). Default: no flag; sections are stable and greppable.
