# Changelog

All notable changes to **downlint** are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/), and this project
adheres to [Semantic Versioning](https://semver.org/) for post-1.0 releases.
Pre-1.0 versions may include breaking changes.

## [Unreleased]

## [0.12.0] — LSP honors `[completion]` config

### Fixed

- **LSP completion now reads `[completion]` from `.downlint.toml`.** The
  `textDocument/completion` handler previously hardcoded
  `WikiCompletionStyle::TitleSlug` and `max_candidates: 50`, so the config's
  `wiki.style` and `candidates` were silently ignored (and, since the CLI never
  calls `complete_at`, those keys were effectively dead). The handler now takes
  the configured style and candidate cap from the workspace config, falling back
  to the previous defaults when no workspace is loaded. The completion engine and
  config parsing were already correct — only the wiring was missing.

## [0.11.0] — Workspace `info` command

New `downlint info`: a read-only, descriptive report of what downlint sees — the
**resolved** workspace, not the config file. The first thing to run when debugging
mount/schema configuration.

### Added

- **`downlint info`** — projects the resolved workspace into a report:
  - **header**: version, workspace root, config file (or `(defaults)`), file extensions.
  - **mounts**: one line each — the `as` prefix (or `(none)`), the resolved absolute
    `path`, `lint`, the document count, and a presence marker.
  - **schemas**: one line each — the `uri` prefix, the expanded absolute `to`,
    `auto_verify`, whether a `verify_cmd` is set, and a presence marker.
  - **documents**: total / primary / mounted.
  - **conflicts**: one line per namespace collision (or `none`).
- **`--format <text|json>`** (default `text`) — JSON emits a single object
  `{version, workspace, config, file_extensions, mounts[], schemas[], documents,
  conflicts[]}`.
- **`--root <DIR>`** to override the workspace root.

### Notes

- `info` is **descriptive only**: exit `0` on a loaded workspace (even with missing
  folders, shown as `✗ missing`) · exit `2` on a config error (no workspace, bad args,
  or a schema `to` that cannot be expanded because an env var is unset). There is no
  exit `1` — a validating `doctor` command is a possible follow-up.
- `info` builds the index and resolved mounts but never resolves links, so a schema's
  `verify_cmd` is never executed and no `--allow-uri-sync` flag exists.

## [0.10.0] — Graph `--format json`

`downlint graph` now supports `--format <text|json>` (default `text`; the flag may
follow the subcommand, like `--root`).

### Added

- **JSON output** for all five graph queries: a single pretty-printed envelope
  `{"query": <name>, "results": [...]}` — plus `"file"` (the canonical namespace
  path) for `backlinks`/`links`. `results` is `[]` when empty.
  - `backlinks`: `{"source", "line", "col"}`
  - `links`: `{"line", "col", "target", "status", "destination"}` — `status` is
    `"resolved" | "unresolved" | "ambiguous"`; `destination` is the path(s) or `null`.
  - `orphans` / `deadends`: `{"path"}`
  - `unresolved`: `{"source", "line", "col", "target"}`
- Exit codes are **independent of `--format`**; text output is byte-identical to
  0.9.0. Errors (e.g. `<FILE>` not in the index) still go to stderr with no JSON
  envelope on stdout.

## [0.9.0] — Graph Query Commands

New `downlint graph <query>` — read-only link-graph queries that project the existing
resolution graph into navigation reports. No new diagnostics; no resolution-semantics
change.

### Added

| Query | Lists |
|---|---|
| `graph backlinks <FILE>` | Notes that link to `FILE` (one line per occurrence). |
| `graph links <FILE>` | `FILE`'s outgoing links, each with its resolution status. |
| `graph orphans` | Notes with no incoming document link. |
| `graph deadends` | Notes with no outgoing document link. |
| `graph unresolved` | Broken links as `source:line:col  target`. |

- The graph is built **complete**: every document's links are resolved, not just the
  linted ones — a `lint = false` mount's links are visible (the `check`/lint path is
  untouched).
- A *document link* is a reference whose destination is an indexed document, reached
  directly or via a heading (`[[Note#H]]`); attachments, folders, tags, and link
  definitions are not note links. Ambiguous references are not confirmed links to any
  note.
- `<FILE>` matches by workspace-relative or namespace path (same rules as
  `resolve --from`); not-in-index is exit 1.
- Exit codes: `backlinks`/`links` `0` in-index · `1` not-in-index · `2` error;
  `orphans`/`deadends`/`unresolved` `0` none · `1` found · `2` error (CI-gateable,
  e.g. `downlint graph orphans || echo clean`).
- No `--stdin`, no `--allow-uri-sync` (a read-only query never runs a schema's
  `verify_cmd`; URI targets are stat-only).

## [0.8.0] — Mounts/Schemas Config Field Rename

The `[[mounts]]` and `[[schemas]]` keys are renamed so each is self-evident and the two
sections no longer share a vocabulary. No behavior change — this is a config-key rename.

### Changed (breaking)

| Feature | old key | new key |
|---|---|---|
| mount disk folder | `root` | `path` |
| mount virtual path | `prefix` | `as` |
| schema URI | `prefix` | `uri` |
| schema disk folder | `root` | `to` |

A mount is a disk `path` exposed `as` a virtual path; a schema is a `uri` that resolves
`to` a disk folder. `lint`, `auto_verify`, and `verify_cmd` are unchanged.

**Migration** — existing `.downlint.toml` files using the old keys fail to parse
(`deny_unknown_fields`); rename the keys mechanically:

```diff
 [[mounts]]
-root = "~/kb"
-prefix = "/kb"
+path = "~/kb"
+as = "/kb"

 [[schemas]]
-prefix = "onedrive://xyz/"
-root = "downloads/onedrive"
+uri = "onedrive://xyz/"
+to = "downloads/onedrive"
```

## [0.7.0] — Obsidian-Compatible Path Resolution

Wiki link path resolution now matches Obsidian's documented, deterministic rules:
bare `[[folder/note]]` links resolve against the vault root, the `.md` suffix is
optional, and `.`/`..` are normalized. This fixes false-positive broken links, the
dot-relative attachment misclassification, and silently-unvalidated anchors.

### Changed

- **Wiki path links now resolve Obsidian-compatibly** (behavior change, RFC 0013). The
  resolution base is decided by the target's prefix, with no fallback:
  - `./…` / `../…` → the containing document's directory
  - `/…` and **bare `path/file`** (wiki) → the workspace root
  - markdown links keep standard source-relative semantics
  - `.`/`..` are normalized lexically before comparison
- **`.md` is optional** for wiki path targets (a `.md` document matches a candidate with
  or without the suffix; non-`.md` files require the extension). This fixes the
  previously-dead extensionless matching rule.
- **Dot-relative wiki links resolve as documents** (previously misclassified as
  attachments), and their anchors are now validated (new `link/broken-anchor` possible
  where a section is missing).
- **Mount prefixes** are reachable from bare wiki `path/file` targets (like `/…`
  targets).
- **`downlint resolve`**: `--from` now only affects source-relative (`./…`/`../…`)
  targets; bare wiki `path/file` and `/…` targets are root-relative and ignore `--from`.

### Impact

Some links that only "worked" under the previous source-relative interpretation of bare
wiki paths now report `link/broken` (they were false negatives against Obsidian).
Extensionless and dot-relative wiki path links now resolve.

## [0.6.0] — Target Resolution Query

New `downlint resolve` subcommand: given a link target, it reports every
destination the target resolves to (and the rule each matched), so
`link/ambiguous` and `link/broken` diagnostics can be debugged from the
command line.

### Added

- **`downlint resolve <TARGET>`** — a read-only query that reports every destination a link
  target resolves to, with the rule each matched by (`path`, `stem`, `title`, `prefix`,
  `attachment`, `directory`), mirroring `check`'s resolution rules (RES-03/04/05/06/07). The
  per-document matching rules are single-sourced in `resolution::query` and shared with link
  resolution (parity-tested), so `resolve` predicts `check`'s behavior by construction.
  - Statuses: `resolved` (one destination), `ambiguous` (multiple), `broken` (none), plus
    URI-scheme statuses `external`, `unmapped`, `mapped-present`, `mapped-missing`, and
    `mapped-placeholder` for `[[schemas]]` targets.
  - Exit codes: `0` = safe to link as-is (one destination, external, or mapped-present),
    `1` = none or multiple (or unmapped / mapped-missing / mapped-placeholder), `2` = bad
    arguments or config error.
  - Options: `--root` (workspace root; inferred when omitted), `--from <DOC>` (resolve
    relative targets as if the link were in that document), `--format text|json`,
    `--include-prefix` (list prefix candidates when `wiki.obsidian_prefix` is off — advisory
    only, never changes the status), and `--allow-uri-sync` (permit `verify_cmd` execution
    for URI targets).
  - `TARGET#anchor` reports, per document destination, whether the heading/tag exists
    (advisory — the exit code follows the destination count).
  - JSON output: one object `{target, anchor, status, destinations[], prefix_candidates[],
    scheme}` for scripting.

## [0.5.0] — Workspace-Anchored Stdin

`downlint check --stdin` is now a workspace-anchored check: piped text is resolved
against the current workspace and link diagnostics are reported on `<stdin>.md`,
making single-line link validation scriptable. Also fixes markdown link
destinations containing spaces.

### Changed

- **`--stdin` is now a workspace-anchored check** (behavior change). Piped text is linted as a
  synthetic `<stdin>.md` document at the workspace root and resolved against the full workspace —
  documents, attachments, folder links, in-page anchors, URI schemes, and mount prefixes. Link
  diagnostics are reported on `<stdin>.md` (e.g. `link/broken`), and the exit code follows the
  usual contract (`0` clean / `1` issues found), so the output is scriptable:
  `echo '- [[/assets/diagram.drawio]]' | downlint check --stdin`. Workspace documents are indexed
  as targets only: their own diagnostics (broken links, `heading/nbsp`) do not surface in a stdin
  check. Previously stdin ran in single-file mode and silently dropped all cross-file link
  diagnostics. Single-file mode (an explicit file on disk) is unchanged.

### Fixed

- **Markdown link destinations containing spaces** (e.g. cloud-storage filenames
  like `Messaging BOM - 21May26.pdf`) are no longer truncated at the first space. The
  parser now keeps the whole destination unless a trailing *quoted* title (or pointy
  `<…>` brackets) is present. Previously such links resolved to the fragment before the
  space and were reported as `link/broken`.

## [0.4.0] — Fine-Grained Mount Conflicts

RFC 0011 refines `mount/conflict` detection from a coarse top-level-name check to a
fine-grained namespace-path collision. No config change.

### Changed

- **Mount conflict detection is now fine-grained** (RFC 0011, behavior change, no config
  change). `mount/conflict` no longer fires on a shared *top-level name*; it fires only on
  an actual namespace-path collision — a mount file at the same path as a primary file, or a
  file/folder sharing a name (stem) at the same location. A mount now **merges** into an
  existing folder (e.g. `kb/`) when no file collides, instead of erroring. A conflicting
  mount file is a target only (not linted). `link/ambiguous` is unchanged and is not a mount
  conflict.

## [0.3.0] — Mounts, Schemes & Rename

RFC 0010 lands in full: **Mounts** (co-equal resolution roots) and **Schemes**
(external URI mapping via rewrite + stat + verify), alongside the Rename & Link
Refactor and the re-base of diagnostic codes to semantic slugs. This is a
breaking release: `core.extra_folders`, `[uri]`/`[[uri.mappings]]`, and the URI
warming machinery are removed (no aliases, no deprecation window). Normative
behavior is in `spec/linting.md` (§3.7 RES-07, §3.8 RES-08) and
`spec/downlint.md`.

### Added

**Rename & Link Refactor** — downlint can now rename files,
headings, and link identifiers safely across the workspace. Closes the
gap between detecting broken links (`link/broken`) and fixing them. The LSP
and CLI share a single rename library (`src/rename/`); both call
`plan_rename(input) -> RenamePlan` and then either serialize to a
`WorkspaceEdit` (LSP) or apply text-first-then-disk (CLI).

#### LSP wire surface

- `textDocument/prepareRename` — returns the editable range for a
  string at the cursor (link target or heading text), `null` otherwise.
- `textDocument/rename` — string-only rename. **Never moves files.**
  This is the safe F2 default; file moves are surfaced as code actions.
- `textDocument/codeAction` — three new kinds:
  - `refactor.rename.file` — moves a file on disk and rewrites every
    reference (markdown file or attachment; class is inferred from the
    source extension). Full safety machinery: extension-class
    preservation, exact-path and prefix-collision checks, blocking rule
    across rewritten documents, atomic text-first-then-disk.
  - `refactor.rename.link-target` — workspace-wide string rewrite with
    conflict + blocking checks. Useful for fixing typos that caused a
    broken link (`link/broken`), or renaming a logical identifier across
    many documents.
  - `refactor.rename.heading` — recomputes the heading slug and
    rewrites every `#section` in referencing links across the
    workspace.
- `workspace/didRenameFiles` — reacts to editor-driven file renames by
  updating the graph in-place (O(references to renamed file), not
  O(whole graph)) and re-publishing diagnostics.

#### CLI subcommands

- `downlint rename-file --from <PATH> --to <PATH> [--dry-run]` —
  renames a markdown file or attachment and propagates to references.
  Kind-class is inferred from the source extension.
- `downlint rename-link --from <STRING> --to <STRING> [--dry-run]` —
  rewrites a logical link identifier across the workspace without
  moving files.

Exit codes (both subcommands):

| Code | Meaning |
|---|---|
| 0 | Success (or dry-run clean) |
| 1 | Blocked by an existing `Warning`/`Error` diagnostic on an unrelated occurrence |
| 2 | Conflict detected (path collision, prefix shadow, or extension class mismatch) |
| 3 | Bad arguments / config error / indexing in progress |

#### Blocking rule

A rename is refused when any document that would be rewritten contains a
`Warning` or `Error` diagnostic other than on the occurrence(s) being
rewritten. `Information` and `Hint` diagnostics do not block. The error
message lists every blocking diagnostic inline. `textDocument/rename` is
the only surface exempted from this rule (string-only — cannot introduce
broken-link cascades).

#### Indexing guard

All rename operations (LSP and CLI) require indexing to be complete. If
a rename is requested mid-index, the operation is rejected with a clear
message: "Indexing in progress — try again in a moment." The LSP
returns `MethodFailed`; the CLI exits with code 3. The indexing state is
tracked in `ServerState::indexing` and the `is_indexing()` helper.

#### Config scaffolding and friendlier empty runs

- `downlint init` — scaffolds a `.downlint.toml` in the workspace root (cwd, or
  `--root <DIR>`). Common options are written active (at their defaults) so the file
  doubles as a reference; advanced options are commented out for opt-in. Refuses to
  overwrite an existing `.downlint.toml` unless `--force`. Exit `0` created · `2`
  already exists (no `--force`) or not a directory.
- A directory-based `check` now prints an informational stderr hint when no
  `.downlint.toml` is found (suggesting `downlint init`) and/or when the workspace
  contains no markdown. Suppressed by `--quiet`; never affects the exit code.

#### Mounts (co-equal resolution roots)

- `[[mounts]]` replaces `core.extra_folders` (the old key is removed, not
  aliased). Each entry has a `root` (required), an optional workspace-absolute
  `prefix` (e.g. `/kb`), and an optional `lint` flag (default `false`).
- Mounted documents are indexed **co-equal** with the primary project (no
  primary-wins fallback): a link matching documents in both is `link/ambiguous`.
- Relative links do not cross into a mount; workspace-absolute links reach a mount
  via its `prefix`; bare stem/title wiki links resolve across the whole namespace.
- A mounted document is linted as a source only when its mount has `lint = true`.
- Diagnostics from mounted documents carry a `mount` attribution (`prefix` or `root`).
- New `mount/conflict` (Error) diagnostic for structural prefix/folder collisions,
  with suspend behavior (conflicting prefix not applied / conflicting folder not
  linted).

#### Schemes (external URI mapping)

- `[[schemas]]` replaces `[uri]` / `[[uri.mappings]]` (the old keys are removed, not
  aliased). Each entry has a `prefix` (e.g. `icloud://assets/`) and a `root` (both
  required), plus optional `auto_verify` (default `true`) and `verify_cmd`.
- Resolution is **rewrite + stat + verify**: downlint maps the URI to a local path,
  stats it, and (per `auto_verify`) checks for evicted cloud placeholders. It never
  downloads or hydrates files.
- `auto_verify` is now a per-schema bool (was a global string enum) and runs
  vendor-specific heuristics only (iCloud `.icloud` sibling, OneDrive `._<name>`
  resource fork); the generic 0-byte + recent-mtime heuristic is dropped.
- `verify_cmd` is an advanced per-schema escape hatch, gated by `--allow-uri-sync`.

### Changed

- **Diagnostic codes re-based to semantic slugs** (breaking, wire format). The opaque
  `DNLnnn` codes are now namespaced slugs: `DNL001`→`link/ambiguous`,
  `DNL002`→`link/broken`, `DNL003`→`heading/nbsp`, `DNL005`→`link/broken-anchor`,
  `DNL006`→`uri/no-mapping`. Emitted `code` strings in CLI text/JSON and LSP
  `publishDiagnostics` change accordingly; severity is unchanged (still a separate
  field). Full mapping in `spec/linting.md` §8.
- LSP diagnostics now carry `source: "downlint"` so editors attribute them to
  downlint (matching how other servers, e.g. Marksman, label their diagnostics).

### Removed

- **URI warming** (breaking). The `warm_cmd` / `warm_required` / `warm_timeout`
  config keys, the `downlint warm-uri-mappings` subcommand, the
  `--uri-sync-batch-size` flag, and the `uri/sync-skipped` / `uri/sync-failed` /
  `uri/batch-clamped` diagnostics are all removed. downlint is read-only: it
  validates external assets (stat + verify) but never downloads or hydrates them.
  A missing or evicted cloud file is reported as `link/broken`.
- `core.extra_folders` (replaced by `[[mounts]]`).
- `[uri]` / `[[uri.mappings]]` (replaced by `[[schemas]]`).

### Notes

- The persistent server (`downlint server --detach / --stop`) is a stretch
  goal. The CLI subcommands and in-process planner ship now; the
  detached-server optimization for agent workflows is future hardening.
- The `.downlint/.rename.lock` atomic-application guard is not yet
  implemented. The current apply path writes text edits first and then
  moves the file — partial failures leave a recoverable state (`link/broken`
  surfaces the gap) but no automatic rollback.

## [0.2.0] — External Asset URI Mapping

The first user-facing feature after v0.1.0. Adds `[uri.mappings]` for
validating wiki/markdown links that point at external assets (OneDrive, S3,
NAS, etc.) via a configurable mapping table. Shipped in three phases:
basic feature, hardening, and a vocabulary rename. Normative behavior is
in `spec/linting.md` §3.7 and §4.5–4.8.

### Added

**`[uri]` configuration section** — maps URI-style link targets
(`onedrive://work/...`, `s3://reports/...`, `internal://docs/...`) to local
filesystem roots. Mappings support env-var expansion (`$VAR`, `${VAR}`),
`~` expansion (via `dirs::home_dir()` for Windows compatibility), and
relative paths (resolved against `.downlint.toml`'s directory).

```toml
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
warm_cmd = ["mdutil", "--enforce-locals", "{path}"]
warm_required = false
warm_timeout = 30
verify_cmd = ["file", "{path}"]   # Phase 2
```

**Per-file warming** — `warm_cmd` is invoked once per resolved file (with
`{path}` substituted to the absolute path). Sync never spans an entire
mount; the safety model is per-target. The key was originally named `sync_cmd`.

**`--allow-uri-sync` gate** — subprocess execution of `warm_cmd` requires
this explicit flag on `check`, `server`, and `warm-uri-mappings`. Without
it, warming is skipped and a `DNL007` info diagnostic is emitted per
mapping. The flag exists to prevent `.downlint.toml` from running
arbitrary commands without user consent.

**`downlint warm-uri-mappings` subcommand** — cache warming without
validation. Walks every URI-scheme link target (both resolved and
unresolved), groups by mapping, and runs each mapping's `warm_cmd` in
batches. Prints a per-mapping summary. Always requires `--allow-uri-sync`
(exits 2 otherwise). Originally named `downlint sync`; the alias
`downlint warm-uri` is accepted.

**`verify_cmd` per mapping** (Phase 2) — post-warm placeholder detection.
Exits 0 → file is real; non-zero → treat as `Missing`. Honors `{path}`
substitution and `warm_timeout`. Authoritative override of auto-detection.

**Auto-detection heuristics** (Phase 2) — built-in placeholder checks for
OneDrive resource forks (macOS), iCloud `.icloud` siblings under
`Mobile Documents/`, and 0-byte files in cloud-storage paths. Configurable
via `[uri].auto_verify = "on" | "off" | "onedrive-only" | "icloud-only"`
(default: `"on"`). Runs before `verify_cmd` as a fast path.

**Batch fan-out** (Phase 2) — `batch_size` (CLI: `--uri-sync-batch-size`,
default 50) now actually batches. Two tiers:
- Positional `{path}` (default): the args template is repeated with each
  path substituted.
- `{paths}` newline-stdin (opt-in): one path per line on stdin, for tools
  like `rclone` / `aws s3 sync`.
- 128 KiB argv cap (`MAX_BATCH_BYTES`); on overflow, the runner falls
  back to per-file invocation and emits a one-time `DNL009 BatchClamped`
  info diagnostic per mapping.

**LSP `--allow-uri-sync` / `--no-uri-hints` / `--uri-sync-batch-size`** —
`downlint server` now accepts the same three URI flags as `check`. They
thread into `ServerState.uri_opts` at startup; without `--allow-uri-sync`,
the LSP never runs `warm_cmd`. To change flags you must restart the server
(no `workspace/didChangeConfiguration` yet).

**Shared `UriSyncCache`** — sync results are cached across LSP
document-change events via `Arc<Mutex<BTreeMap<...>>>`, avoiding
re-forking `warm_cmd` on every keystroke. Cleared on `.downlint.toml`
change.

### New Diagnostics

| Code | Severity | Meaning |
|------|----------|---------|
| `DNL005` | Warning | BrokenAnchor — anchor (`#section`, `[[#section]]`, `path#section`) failed to resolve to a heading. Distinct from `DNL002` because the link target was unambiguously an anchor. |
| `DNL006` | Info | UriNoMappingHint — URI-scheme link validated, but no `[[uri.mappings]]` prefix matched. Includes a config example. Suppressed with `--no-uri-hints`. |
| `DNL007` | Info | UriSyncSkipped — `warm_cmd` is configured but `--allow-uri-sync` was not passed. Message names `downlint warm-uri-mappings --allow-uri-sync` so users can warm explicitly. |
| `DNL008` | Info | SyncFailureWarning — `warm_cmd` ran (with `warm_required = false`) but the file is still missing. Soft warning alongside `DNL002`. `warm_required = true` continues to produce only `DNL002`. |
| `DNL009` | Info | BatchClamped — `warm_cmd` batch's argv exceeded 128 KiB and the runner fell back to per-file. One per mapping per run. |

### Changed

**Vocabulary rename** — the `sync_*` config keys and `downlint sync`
subcommand were misleading (implied two-way sync). Renamed to `warm_*` /
`downlint warm-uri-mappings` (alias `downlint warm-uri`). Internal types
(`SyncRunner`, `SyncDecision`, `PathStatus` variants) keep their names.
The `--allow-uri-sync` flag name stays as-is — it's the safety gate
regardless of which verb the subcommand uses.

### Migration from pre-0.2.0

There is no `sync_*` config to migrate if you were not already using
`[uri]` from a pre-release. If you were, the rename is a clean break
(parse error on old keys) and a one-line `sed`:

```sh
sed -i.bak '
  s/^sync_cmd = /warm_cmd = /
  s/^sync_required = /warm_required = /
  s/^sync_timeout = /warm_timeout = /
' .downlint.toml
```

### Security Note

The `--allow-uri-sync` flag is the only thing standing between a `.downlint.toml`
you've reviewed and arbitrary command execution. Anyone who can commit
`.downlint.toml` can configure a `warm_cmd` to run on machines that pass
the flag. CI runners should not pass `--allow-uri-sync` unless the repo
has been audited. The `verify_cmd` and `auto_verify` mechanisms are
advisory best-effort heuristics; they reduce false positives from cloud
placeholders but cannot catch a malicious `warm_cmd`.

## [0.1.0] — Initial Public Release

Baseline feature set: Markdown checker + LSP with completion, definition,
references, hover, document symbols, code actions (TOC generation,
missing-file creation), and diagnostics.

### Diagnostics in this release

| Code | Severity | Meaning |
|------|----------|---------|
| `DNL001` | Error / Warning | Ambiguous link — multiple destinations match. |
| `DNL002` | Error / Warning | Broken link — target file missing. |
| `DNL003` | Warning | Non-breaking whitespace after heading marker. |

### Features

- Wiki-link (`[[...]]`) and embed (`![[...]]`) resolution with title-slug
  and relative-path matching.
- Markdown-link (`[...](...)`) and image (`![...](...)`) resolution with
  attachment fall-through for file-like targets.
- Heading-anchor links (`#section`, `[[#section]]`, `path#section`).
- Tag scanning and cross-document tag references.
- Cross-folder resolution via `[core].extra_folders`.
- `[wiki].obsidian_prefix` opt-in for Obsidian-style prefix matching.
- Folder link resolution (`[[folder/]]`) and symlink following.
- File-extension whitelist dropped in favor of configurable
  `[core].file_extensions`.
- Custom ignore patterns via `[core].ignore`.
- LSP server with completion, hover, definition, references, document
  symbols, code lens, code actions.
- Diagnostic output in text and JSON formats.
- `--fix` mode for safe DNL003 auto-correction.
- `--watch` mode for incremental re-validation.
- `--stdin` mode for piping single documents.
- Project + user config layers with strict validation
  (`deny_unknown_fields`) per the config validation rules (now `spec/linting.md` §5.2).

---

## Versioning Policy

Pre-1.0 (current): versions `0.x.0`. Breaking changes are documented in
the `Changed` section and accompanied by a migration note. The project
promises no wire-format stability for `.downlint.toml` until 1.0.

Post-1.0: standard SemVer. Breaking changes bump the minor version.

[Unreleased]: https://github.com/your-org/downlint/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/your-org/downlint/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/your-org/downlint/releases/tag/v0.1.0