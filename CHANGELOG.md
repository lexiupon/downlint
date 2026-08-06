# Changelog

All notable changes to **downlint** are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/), and this project
adheres to [Semantic Versioning](https://semver.org/) for post-1.0 releases.
Pre-1.0 versions may include breaking changes; see the linked RFCs for design
context.

## [Unreleased]

### Added

**Rename & Link Refactor (RFC 0009)** — downlint can now rename files,
headings, and link identifiers safely across the workspace. Closes the
gap between detecting broken links (DNL002) and fixing them. The LSP
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
    broken link (`DNL002`), or renaming a logical identifier across
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

### Notes

- The persistent server (`downlint server --detach / --stop`) is
  documented as a stretch goal in RFC 0009 §"Persistent Server Mode".
  Phase 5 ships the CLI subcommands and the in-process planner; the
  detached-server optimization for agent workflows is a future hardening.
- The `.downlint/.rename.lock` atomic-application guard (RFC §"Risks"
  #6) is not yet implemented. The current apply path writes text edits
  first and then moves the file — partial failures leave a recoverable
  state (DNL002 surfaces the gap) but no automatic rollback.
- No new diagnostic codes. RFC 0009 reuses existing severity levels.

## [0.2.0] — External Asset URI Mapping

The first user-facing feature after v0.1.0. Adds `[uri.mappings]` for
validating wiki/markdown links that point at external assets (OneDrive, S3,
NAS, etc.) via a configurable mapping table. Ships in three phases:
[RFC 0006](./rfc/0006-external-asset-uri-mapping.md) (basic feature),
[RFC 0007](./rfc/0007-uri-mapping-phase-2.md) (hardening), and
[RFC 0008](./rfc/0008-uri-warm-rename.md) (vocabulary rename).

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
mount; the safety model is per-target. Renamed from `sync_cmd` per RFC 0008.

**`--allow-uri-sync` gate** — subprocess execution of `warm_cmd` requires
this explicit flag on `check`, `server`, and `warm-uri-mappings`. Without
it, warming is skipped and a `DNL007` info diagnostic is emitted per
mapping. The flag exists to prevent `.downlint.toml` from running
arbitrary commands without user consent.

**`downlint warm-uri-mappings` subcommand** — cache warming without
validation. Walks every URI-scheme link target (both resolved and
unresolved), groups by mapping, and runs each mapping's `warm_cmd` in
batches. Prints a per-mapping summary. Always requires `--allow-uri-sync`
(exits 2 otherwise). Renamed from `downlint sync` per RFC 0008; alias
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

**Vocabulary rename (RFC 0008)** — the `sync_*` config keys and `downlint sync`
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
- `[wiki].obsidian_prefix` opt-in for Obsidian-style prefix matching
  (RFC 0004).
- Folder link resolution (`[[folder/]]`) and symlink following
  (RFC 0003).
- File-extension whitelist dropped in favor of configurable
  `[core].file_extensions` (RFC 0001).
- Custom ignore patterns via `[core].ignore`.
- LSP server with completion, hover, definition, references, document
  symbols, code lens, code actions.
- Diagnostic output in text and JSON formats.
- `--fix` mode for safe DNL003 auto-correction.
- `--watch` mode for incremental re-validation.
- `--stdin` mode for piping single documents.
- Project + user config layers with strict validation
  (`deny_unknown_fields`) per §9.4 of `spec.md`.

---

## Versioning Policy

Pre-1.0 (current): versions `0.x.0`. Breaking changes are documented in
the `Changed` section and accompanied by a migration note. The project
promises no wire-format stability for `.downlint.toml` until 1.0.

Post-1.0: standard SemVer. Breaking changes bump the minor version.

[Unreleased]: https://github.com/your-org/downlint/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/your-org/downlint/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/your-org/downlint/releases/tag/v0.1.0