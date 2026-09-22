# Downlint — Product Behavior

**Status**: Living document. Higher level than `spec/linting.md`; where the two ever differ
on behavior, `spec/linting.md` wins. This document describes the **product surface** — what
a user sees and can do. It deliberately omits implementation detail: the code is the source
of truth for *how*.

---

## 1. What downlint is

Downlint is a markdown **linter and language server** for knowledge bases. Its reason to
exist is to keep links valid as a knowledge base evolves: it detects broken and ambiguous
links, completes link targets, and rewrites every reference when a file, attachment, or
heading is renamed.

It has two surfaces that share one resolution engine and one rename engine:

- a **CLI checker** — for CI, scripts, and one-off checks
- an **LSP server** — for editors

## 2. What it checks

Link validity across wiki links, markdown links, images, embeds, heading anchors, explicit
file-like attachments, and mapped external URIs. Diagnostics are namespaced semantic slugs
(`link/*`, `heading/*`, `uri/*`); the full table is in `spec/linting.md` §4.

**The normative rule conditions — what must hold — are in `spec/linting.md`.** This
document does not restate them.

## 3. Interfaces

### 3.1 CLI

Eight subcommands. `downlint` with no subcommand runs the check.

| Command | Purpose |
|---|---|
| `downlint [PATH]` (alias `check`) | Check a file or directory; print diagnostics; exit by severity. |
| `downlint init` | Scaffold a `.downlint.toml` in the workspace root (refuses to overwrite without `--force`). |
| `downlint server` | Run the LSP server over stdio. |
| `downlint rename-file` | Move a markdown file or attachment on disk and rewrite every link pointing at it. |
| `downlint rename-link` | Rewrite a logical link-target identifier workspace-wide (no disk move). |
| `downlint resolve <TARGET>` | Show what a link target resolves to: documents, attachments, folders, URI mappings. |
| `downlint graph <QUERY>` | Read-only link-graph queries: `backlinks`, `links`, `orphans`, `deadends`, `unresolved`. |

**`check`** — key flags (defaults in parentheses):

| Flag | Default | Effect |
|---|---|---|
| `--root <DIR>` | inferred | Override workspace root. |
| `--format <text\|json>` | `text` | Output format. |
| `--min-severity <info\|warning\|error>` | `warning` | Filter output; exit code reflects the filtered set. |
| `--color <auto\|always\|never>` | `auto` | ANSI coloring. |
| `-v / --verbose <N>` | `2` | Logging to stderr (independent of `--min-severity`). |
| `--fix` | off | Apply safe fixes in place (v1: `heading/nbsp` NBSP→space), then re-run. |
| `-w / --watch` | off | Re-run after debounced file changes. |
| `--stdin` (or `-`) | off | Check one document from stdin, resolved against the current workspace. The document is linted as `<stdin>.md` at the workspace root (relative links resolve against the root); workspace documents are indexed as targets only, so only the piped document is diagnosed. |
| `--allow-uri-sync` | off | Permit a `[[schemas]]` `verify_cmd` subprocess execution (safety gate). |
| `--no-uri-hints` | off | Suppress `uri/no-mapping`. |

- **Exit codes**: `0` clean at the chosen severity · `1` issues found · `2` error
  (bad path, config parse failure, failed `--fix` write).
- **Text output**: `{rel_path}:{line}:{col}: {severity}: {message} [{CODE}]`.
- **JSON output**: array of `{path, range, severity, code, message, related}`.
- **No-config / no-files hints**: a directory-based `check` (not `--stdin`, not a single
  explicit file) prints an informational stderr hint when no `.downlint.toml` is found —
  suggesting `downlint init` — and/or when the workspace contains no markdown. Suppressed
  by `--quiet`. These never affect the exit code.

**`resolve`** — target resolution query (RFC 0012). Given a link target, list
**every** destination it resolves to, with the reason each matched. It uses the
existing matching rules exactly (RES-03/04/05/06/07, including the RFC 0013
Obsidian-compatible path interpretation) — it predicts `check`'s
behavior and introduces no new diagnostics.

| Flag | Default | Effect |
|---|---|---|
| `<TARGET>` | *(required)* | The link target (wiki-link target grammar: title/stem, explicit path, folder target, optional `#anchor`, or `scheme://…`). An empty target, or a target consisting only of `#anchor`, is a bad argument. |
| `--root <DIR>` | inferred | Override workspace root. |
| `--from <DOC>` | workspace root | Resolve **source-relative** targets (`./…`/`../…`) as if the link were in this document (must name a document in the index — primary or mounted — else exit 2). Bare wiki `path/file` and `/…` targets are root-relative and ignore `--from` (RFC 0013). |
| `--format <text\|json>` | `text` | Output format. |
| `--include-prefix` | off | Also list prefix candidates when `wiki.obsidian_prefix` is off (advisory: never affects status or exit code). |
| `--allow-uri-sync` | off | Permit `verify_cmd` subprocess execution for URI targets (safety gate). |

- **Statuses and exit codes**: `0` = `resolved` (exactly one destination),
  `external` (web scheme, LNK-03), or `mapped-present`; `1` = `broken` (no
  destination), `ambiguous` (more than one), `unmapped`, `mapped-missing`, or
  `mapped-placeholder`; `2` = bad arguments / config error. The scriptable
  contract: exit 0 iff the target is safe to use as a link as-is.
- **Destinations** report: namespace path, H1 title, match kinds (`path`,
  `stem`, `title`, `prefix` in canonical order; `attachment` and `directory` are
  single), mount attribution, and per-destination anchor existence (advisory —
  it does not change status or exit code).
- **URI targets** (RES-07): the mapping status is reported — `mapped-present`,
  `mapped-missing`, `mapped-placeholder` (rewrite + stat + verify), `unmapped`,
  or `external`. Anchors on URI targets are not supported (reported as a note;
  `check` emits `link/broken-anchor` for such links).
- **Text output**: `{status} — {n} destination(s):` followed by one line per
  destination. **JSON output**: a single object with `target`, `anchor`,
  `status`, `destinations[]`, `prefix_candidates[]`, and `scheme`.

**`graph`** — read-only link-graph queries (RFC 0015). Projects the existing
resolution graph into navigation reports. It introduces no new diagnostics and
changes no resolution semantics. The graph is built **complete**: every
document's links are resolved, not just the linted ones — a mount with
`lint = false` is targets-only for `check`, but its links still appear here.

| Query | Lists |
|---|---|
| `backlinks <FILE>` | Every reference in any note that resolves to `FILE` (one line per occurrence). |
| `links <FILE>` | All of `FILE`'s outgoing references, each with its resolution status. |
| `orphans` | Notes with no incoming document link. |
| `deadends` | Notes with no outgoing document link. |
| `unresolved` | Every broken link, as `source:line:col  target`.

- **`<FILE>`** (backlinks/links) names a document by workspace-relative path
  (primary) or namespace path (mounted; the `as` prefix with its leading `/`
  stripped), matched case-insensitive — the same rules as `resolve --from`.
  A `<FILE>` not in the index is an error (exit 1). The path shown in
  `backlinks` output is exactly what you pass back in.
- **Document link**: a reference counts as a link to a note when its
  destination is an indexed document, reached directly or via a heading
  (`[[Note#H]]`). Attachments, folders, tags, and link definitions are not note
  links. Ambiguous references are not confirmed links to any note (they
  contribute to no query; `check` reports them as `link/ambiguous`).
- **`links` vs `deadends`**: `links <A>` shows *all* outgoing references;
  `deadends` counts *document links* only. A note whose only outgoing references
  are broken or non-document targets appears in both `links <A>` and `deadends`.
- **Self-links** count in both directions (a self-linking note is neither an
  orphan nor a deadend).
- **Flags**: `--root <DIR>` (override workspace root; may follow the subcommand).
  No `--stdin` (queries need the full index) and no `--allow-uri-sync` (a
  read-only query never runs a schema's `verify_cmd`; URI targets are stat-only).
- **Exit codes**: `backlinks`/`links` — `0` `<FILE>` in index (list may be
  empty) · `1` `<FILE>` not in index · `2` bad args / config error.
  `orphans`/`deadends`/`unresolved` — `0` none found · `1` found · `2` bad args /
  config error (so `downlint graph orphans || echo clean` gates CI).
- **Output**: one line per result, sorted, 1-based. `backlinks`:
  `{source}:{line}:{col}`. `links`: `{line}:{col}  {target}  →  {destination | <unresolved> | <ambiguous>}`.
  `orphans`/`deadends`: one note path per line. `unresolved`:
  `{source}:{line}:{col}  {target}`. Empty result → no output.

**`init`** — scaffold a `.downlint.toml` in the workspace root (cwd, or `--root <DIR>`).
Common options are written active (at their defaults) so the file doubles as a reference;
advanced options are commented out for opt-in. Refuses to overwrite an existing
`.downlint.toml` unless `--force`. Exit `0` created · `2` already exists (no `--force`) or
not a directory.

**`rename-file` / `rename-link`** — required `--from` / `--to`; `--dry-run` prints the plan
without applying; `--root` as above.

- **Exit codes**: `0` success (or no-op) · `1` blocked by a pre-existing Warning/Error
  diagnostic · `2` conflict (path/prefix collision, extension-class mismatch) · `3` bad
  arguments / config error / source not found.
- `rename-file` infers markdown-vs-attachment from the source extension — the user never
  picks. `rename-link` rewrites a bare identifier and moves nothing.

### 3.2 LSP

LSP over stdio (JSON-RPC 2.0, `Content-Length` framing, UTF-16 positions). The **shipped**
surface:

- **Lifecycle**: `initialize`, `initialized`, `shutdown`, `exit`.
- **Text sync**: `didOpen`, `didChange`, `didClose` (full-text semantics).
- **Diagnostics**: push via `publishDiagnostics` on open/change/close and after
  `didRenameFiles`; each diagnostic carries `source: "downlint"` so editors can
  attribute it alongside other servers (e.g. Marksman).
- **Completion**: wiki document (`[[foo`), wiki heading (`[[#`), and tag (`#`) prompts;
  trigger characters `[`, `#`, `(`; case-insensitive subsequence matching.
- **Code intelligence**: `hover`, `definition`, `references`, `documentSymbol`.
- **Rename**: `prepareRename` + string-only `textDocument/rename` (replaces the link-target
  or heading text at the cursor; never moves files); `workspace/didRenameFiles` keeps the
  index consistent after an editor-driven disk rename.
- **Code actions**: rename actions are offered — `refactor.rename.link-target` and
  `refactor.rename.heading` (and `refactor.rename.file` is advertised).

*(Non-normative status note: the rename code actions are offered but their end-to-end edit
path is still landing — the **CLI is the working rename surface today**. Not yet shipped:
code lenses, semantic tokens, pull diagnostics, TOC / create-missing-file code actions,
`documentHighlight`, `completionItem/resolve`, and the `workspace/didChange*` configuration
handlers. See `spec/linting.md` §6.3 and the code.)*

### 3.3 Configuration

`.downlint.toml` in the project, plus a user-level config; precedence **project > user >
defaults**; unknown keys are a hard parse error (fail-fast, exit 2).

**Normative semantics of every key: `spec/linting.md` §5.**

## 4. Rename (product behavior)

Downlint's core differentiator: rename something, and every reference is rewritten.

**Four rename kinds** (two CLI subcommands; the CLI collapses file/attachment into
`rename-file`):

| Kind | Element | Trigger |
|---|---|---|
| File | a markdown document | `rename-file` / `refactor.rename.file` |
| Attachment | a non-markdown asset (image, PDF, XLSX…) | `rename-file` / `refactor.rename.file` |
| Heading | H1–H6 heading text | `refactor.rename.heading` (LSP only — no CLI) |
| Link target | a bare identifier, no disk move | `rename-link` / `refactor.rename.link-target` / F2 |

**What gets rewritten** (file/attachment rename): the path portion of wiki links, markdown
links, and reference definitions — preserving `|alias`, `#heading`, and URL fragments.
New paths are computed relative to each referencing document (cross-subtree moves yield
`../`). References from mounts are included.

**Heading rename** rewrites only the `#section` portion of every referencing link.

**Safety rules the user must know:**

- **Conflicts** (exit 2 / refused): the new path already exists as a different file; a
  prefix collision when `wiki.obsidian_prefix` is on; or an extension-class change
  (`.md`→`.pdf` is rejected).
- **Blocking rule** (exit 1 / refused): a rename is refused if any document that would be
  rewritten contains a Warning or Error diagnostic *other than on the very occurrence being
  rewritten*. Info never blocks. Fix the diagnostic, then re-run.
- **No force/override** in v1.
- `--dry-run` previews the full plan; application is text-first-then-disk.

## 5. Workflows

- **CI**: `downlint` as a gate; `--format json` for machines; `--min-severity error` to
  gate on errors only; `--color never` for logs.
- **Editor**: `downlint server` over stdio — push diagnostics, completion, hover/definition/
  references, string-only F2 rename; `didRenameFiles` keeps indexes consistent.
- **KB maintenance**: `rename-file` / `rename-link` to evolve the base; `--dry-run` to
  preview; the blocking rule forces cleanup of pre-existing diagnostics first.
- **Graph navigation**: `graph backlinks` / `graph links` for per-note link panels;
  `graph orphans` / `graph deadends` / `graph unresolved` for KB audits and CI gates.
- **Cloud assets**: declare `[[schemas]]` to map a URI to a local folder;
  `check` stats the resolved path and (per `auto_verify`) flags evicted placeholders.
- **Quick single-doc check**: `echo '# Title' | downlint -`.

## 6. What downlint is not

- **Not a formatter or general style linter** — checks are link-centric (plus `heading/nbsp`).
  `textDocument/formatting` is never implemented (markdown is whitespace-sensitive).
- **Not a sync tool** — downlint only *validates* external assets (stat + verify); it never
  downloads or hydrates them. A missing or evicted cloud file is reported as `link/broken`.
- **Rename non-goals**: no `willRenameFiles`; no reference-label rename; no
  extension-class-changing renames; no frontmatter path references; no CLI heading-rename
  subcommand (LSP-only by decision); no combined heading+document rename.
- **v1 LSP exclusions**: folding ranges, `workspace/symbol`, and the `will*` file-operation
  methods are deferred (see `spec/linting.md` §6.3).

## 7. Pointers

| Question | Where |
|---|---|
| What must hold (normative behavior) | `spec/linting.md` |
| What is open / not yet shipped | `TODO.md` |
| Why a decision was made | `spec/linting.md` §8 (amendment history) |
| How it is built | `src/` |
| History | `CHANGELOG.md` |
