# RFC 0007 — Phase 2: URI Mapping Hardening

**Status**: Draft
**Depends on**: [RFC 0006](./0006-external-asset-uri-mapping.md)
**Author**: downlint maintainers
**Target**: v1.2

> **Note**: this RFC uses `sync_cmd` / `sync_required` / `sync_timeout` and
> the subcommand name `downlint sync` throughout. Those were renamed to
> `warm_cmd` / `warm_required` / `warm_timeout` and
> `downlint warm-uri-mappings` per
> [RFC 0008](./0008-uri-warm-rename.md). The historical document below
> reflects the design vocabulary at the time of writing; see the linked RFC
> for the current user-facing names.

## Summary

Phase 1 shipped the basic `[uri.mappings]` feature (RFC 0006). This RFC closes
five gaps surfaced by Phase 1 review and the original RFC's Phase 2 / Phase 3
sections:

1. **Sync verification**: distinguish "real file on disk" from "cloud
   placeholder stub" after `sync_cmd` runs.
2. **`sync_required` diagnostic split**: separate warning (Phase 2) from
   broken-link (always) when sync fails.
3. **Batch fan-out**: make the existing `batch_size` knob actually batch.
4. **`downlint sync` subcommand**: warm caches without running validation.
5. **LSP sync flag plumbing**: let the language server honor `--allow-uri-sync`.

## Motivation

### Why now

Phase 1 shipped conservative defaults to keep the surface area small. After a
month of dogfooding (per maintainer notes), three pain points became clear:

- **OneDrive on-demand stubs report as Present**: `mdutil --enforce-locals`
  makes OneDrive *aware* of the path but doesn't guarantee the file content is
  downloaded. Users get false-positive "link is fine" diagnostics and broken
  behavior in production.
- **`sync_required = false` is misleading**: the current implementation reports
  every missing file as `DNL002`, regardless of whether `sync_required` is
  true or false. The RFC said these should behave differently; Phase 1 deferred
  the split.
- **LSP users cannot enable sync at all**: the `downlint server` subcommand
  has no `--allow-uri-sync`, so the LSP cache never populates. This silently
  regresses link validation in editor workflows.

The two remaining items (batch fan-out, sync subcommand) come from the original
RFC's Phase 2/Phase 3 sections and are no longer blocked.

## Detailed Design

### 1. Sync verification (`verify_cmd` + auto-detection)

#### 1.1 New config key

```toml
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
verify_cmd = ["file", "{path}"]   # NEW: must exit 0
sync_required = false
sync_timeout = 30
```

`verify_cmd: Option<Vec<String>>`. Optional; absent = no verification (backwards
compatible). When present, runs after `sync_cmd` completes successfully.
`{path}` substitution works identically to `sync_cmd`.

| Exit code | Interpretation |
|---|---|
| `0` | File is "real" (not a placeholder). Mark `Present`. |
| Non-zero | Placeholder / unreadable. Treat as `Missing`. |
| Spawn failure | Treat as `Missing` (don't differentiate from non-zero exit). |

`verify_timeout` reuses `sync_timeout` for v1.2 (no new key).

#### 1.2 Auto-detection (built-in heuristics)

Runs **before** `verify_cmd` as a fast path. Configurable via:

```toml
[uri]
auto_verify = "on"   # default: "on"
# Alternatives: "off", "onedrive-only", "icloud-only"
```

Heuristics, applied to the resolved absolute path:

| Platform / Provider | Signal | Action |
|---|---|---|
| macOS, OneDrive | `xattr -p com.apple.metadata:_kCFContainerItemIdentifier` succeeds | Treat as placeholder |
| macOS, OneDrive | sibling `._<name>` resource fork with size 0 | Treat as placeholder |
| Windows, OneDrive | NTFS alternate data stream `Zone.Identifier` exists | Treat as placeholder |
| macOS, iCloud | path under `Mobile Documents/` AND sibling `.icloud` placeholder | Treat as placeholder |
| Generic | file size 0 AND mtime within last 60 seconds | Treat as placeholder |

The heuristics are best-effort and may produce false positives (real files
reported as placeholders) or false negatives (placeholders reported as real).
Both are recoverable: false positive → user sets `verify_cmd` or sets
`auto_verify = "off"`; false negative → user discovers via production breakage
and configures `verify_cmd`.

When auto-detection says "real," `verify_cmd` does **not** run (saves a fork).
When auto-detection says "placeholder," `verify_cmd` does **not** run either —
the answer is already "Missing." Configurable `verify_cmd` is the override path.

#### 1.3 Outcome precedence

```
auto_verify? ─no─▶ skip verification
       │
      yes
       │
       ├── heuristic says placeholder ─▶ Missing
       │
       ├── heuristic says real ─▶ Present
       │
       └── heuristic inconclusive ─▶ run verify_cmd?
                                       │
                                       ├── verify_cmd set ─▶ its exit code
                                       │
                                       └── verify_cmd not set ─▶ Present
```

If `auto_verify = "off"`, only `verify_cmd` runs (or skip if absent).

### 2. `sync_required` diagnostic split (`DNL008`)

#### 2.1 Current behavior (Phase 1)

`sync_required = false` + sync ran + file still missing → `DNL002` broken link.
This conflates "real failure" with "soft warning."

#### 2.2 New behavior

`sync_required = false` + sync ran + file still missing → `DNL002` broken link
**AND** `DNL008 SyncFailureWarning` (info level).

`sync_required = true` + sync ran + file still missing → `DNL002` broken link
only (current behavior; sync was supposed to guarantee presence).

`sync_required = true` + sync ran successfully + verify_cmd says placeholder →
`DNL002` broken link **AND** `DNL008` (warning that placeholder was detected;
without `verify_cmd` we'd report Present, which is wrong).

#### 2.3 `DNL008` diagnostic shape

```rust
Diagnostic {
    code: DNL008,
    severity: Info,
    message: "Sync completed but '{target}' is still missing or unreadable on disk.
              Treat as broken per your sync_required setting.",
}
```

Emitted at the source file's first matching reference (one per file, capped at
5 mappings to avoid noise — same pattern as the existing `obsidian_prefix`
hint).

### 3. Batch fan-out in `SyncRunner`

#### 3.1 Two-tier strategy

**Tier A: positional `{path}` expansion** (default, used when `sync_cmd`
contains `{path}` but not `{paths}`).

`batch_size = N` → in a single spawn, repeat the args list N times, substituting
one path per `{path}` occurrence. Example with template
`["aws", "s3", "cp", "{path}", "/dev/null"]`, `batch_size = 3`, paths
`["/tmp/a", "/tmp/b", "/tmp/c"]`:

```
aws s3 cp /tmp/a /dev/null /tmp/b /dev/null /tmp/c /dev/null
```

Shell-free. No parser complexity. Bounded by `ARG_MAX`.

**Tier B: `{paths}` newline-separated stdin** (used when `sync_cmd` contains
`{paths}` — opt-in).

`batch_size` paths are joined with `\n` and piped to stdin. The args template
has `{path}` (or `{paths}`) substituted with empty string for that one
invocation. `aws s3 sync --paths-from-stdin` and `rclone` are the canonical
use cases.

```toml
sync_cmd = ["rclone", "copy", ":paths:/dst/"]
# rclone reads paths from stdin, one per line.
```

**Backward compatibility**: `batch_size = 1` (default for old configs that
explicitly set it) → exactly Phase 1 behavior, one spawn per file. **But the
default changes from 50 to 50 only if `sync_cmd` is present**; empty `sync_cmd`
unchanged.

#### 3.2 `ARG_MAX` clamp

Hard cap: `MAX_BATCH_BYTES = 131_072` (128 KiB) — below typical `ARG_MAX`
(256 KiB / 2 MiB) and well below Windows `CreateProcess` limit (32 KiB).

When constructed arg list (binary + substituted args, UTF-8 byte length) would
exceed `MAX_BATCH_BYTES`, the runner falls back to per-file spawning and
emits a one-time info diagnostic per mapping:

```
DNL009 BatchClamped: mapping 'onedrive://work/' has paths that exceed 128 KiB
of args; falling back to per-file sync. Reduce batch_size or shorten paths.
```

The diagnostic is suppressed after the first emission per mapping per run
(LSP-cached like `DNL007`).

#### 3.3 Outcome aggregation

`SyncRunner::run_for` returns one `CachedPathResult` per `(mapping_index,
absolute_path)`, regardless of whether the path was part of a batch spawn.
Cache key unchanged. A batch spawn that succeeds populates N entries; a batch
spawn that fails populates N entries with `SyncFailed`. Timeouts are detected
at the batch level (the runner kills the whole child if the deadline passes
mid-batch) and reported as `SyncTimedOut` for every path in that batch.

### 4. `downlint sync` subcommand

#### 4.1 Syntax

```
downlint sync [--allow-uri-sync] [--no-uri-hints]
              [--uri-sync-batch-size N] [--min-severity LEVEL]
              [--root PATH] [--quiet]
```

#### 4.2 Behavior

1. Discover workspace as `check` does.
2. Walk every `[[uri.mappings]]` entry, find all URI-scheme link targets in
   the workspace.
3. Resolve each via `UriResolver` (no anchor support; out of scope).
4. Group by mapping; fan out via `SyncRunner` with `batch_size`.
5. **Always requires `--allow-uri-sync`** or exits 2 with a clear message:
   `downlint: error: 'sync' requires --allow-uri-sync (safety gate)`.
6. Output per-mapping summary:
   ```
   sync: onedrive://work/  ✓ 42 paths  ✗ 1 failed  ⏱ 0 timed-out  (3.2s)
   sync: s3://reports/     ✓ 7 paths   ✗ 0 failed  ⏱ 0 timed-out  (1.1s)
   ```
7. Exit 0 if all mappings succeeded; 1 otherwise.

#### 4.3 What it does **not** do

- Does not run validation.
- Does not emit `DNL002` broken-link diagnostics.
- Does not touch files outside each mapping's `root`.
- Does not modify the LSP cache (LSP has its own).
- Does not support `--watch` mode (use `--watch` on `check` if you want that).

### 5. LSP sync flag plumbing

#### 5.1 New `ServerArgs` flags

```rust
struct ServerArgs {
    #[arg(long, short = 'v', default_value_t = 2)]
    verbose: u8,
    #[arg(long)]
    wait_for_debugger: bool,
    /// Permit subprocess execution of `sync_cmd` entries defined under
    /// `[uri.mappings]`. Same semantics as `downlint check --allow-uri-sync`.
    #[arg(long = "allow-uri-sync", action = ArgAction::SetTrue)]
    allow_uri_sync: bool,
    /// Suppress the "no URI mapping found" hint diagnostic.
    #[arg(long = "no-uri-hints", action = ArgAction::SetTrue)]
    no_uri_hints: bool,
    /// Batch size for per-file `sync_cmd` invocations across external
    /// mappings.
    #[arg(long = "uri-sync-batch-size", default_value_t = 50)]
    uri_sync_batch_size: usize,
}
```

#### 5.2 Plumbing

`crate::lsp::run_server` gains `uri_opts: UriOptions` parameter. On `initialize`,
populate `ServerState.uri_opts` from the args. The existing `refresh_graph`
already reads from `state.uri_opts`, so no further plumbing is needed.

The `workspace/didChangeConfiguration` LSP notification is **not** implemented
in this RFC — clients must restart the server to change flags. This is
documented as a known limitation; editors that need it can use the existing
restart mechanism.

## Acceptance Criteria

### 1. `verify_cmd` + auto-detection

- [ ] New `verify_cmd` config key parses and validates (non-empty, same
      `sync_timeout` constraint).
- [ ] `auto_verify` config key parses with default `"on"`.
- [ ] `SyncRunner::run_verify` (or inline in `run_sync`) honors exit codes.
- [ ] OneDrive / iCloud heuristics are unit-tested with synthetic files
      (using `tempfile` + `xattr` if available, or skipped on unsupported
      platforms via `#[cfg]`).
- [ ] When `auto_verify = "on"` and heuristic says real, `verify_cmd` is
      not invoked (verified via test that counts spawns).
- [ ] When `verify_cmd` is set and exits non-zero, link is reported as
      broken.

### 2. `DNL008` diagnostic

- [ ] New `DiagnosticCode::DNL008` variant.
- [ ] `UnresolvedReference` (or sidecar) carries `sync_required: bool`.
- [ ] `check_diagnostics` emits `DNL008` only when `sync_required = false`
      and the per-link sync outcome was `SyncFailed` or `SyncTimedOut`.
- [ ] Capped at 5 per file (same pattern as `obsidian_prefix` hint).
- [ ] Suppressed when `--min-severity = warning`.

### 3. Batch fan-out

- [ ] `batch_size > 1` with no `{paths}` in `sync_cmd` → Tier A positional
      expansion.
- [ ] `batch_size > 1` with `{paths}` in `sync_cmd` → Tier B stdin piping.
- [ ] `batch_size = 1` → identical to Phase 1 behavior (one spawn per file).
- [ ] When constructed arg list exceeds `MAX_BATCH_BYTES`, fallback to
      per-file + emit `DNL009 BatchClamped` once per mapping per run.
- [ ] Tests: `batch_size=3` with three short paths → exactly one spawn;
      the spawn's argv matches expected positional expansion.

### 4. `downlint sync`

- [ ] New `downlint sync` subcommand registered in CLI.
- [ ] Without `--allow-uri-sync`, exits 2 with a clear error message.
- [ ] With flag, runs sync for every mapping in the workspace.
- [ ] Per-mapping summary printed to stdout.
- [ ] Exit code 0 on success, 1 on any failure.

### 5. LSP sync flag plumbing

- [ ] `downlint server --allow-uri-sync` populates `ServerState.uri_opts`.
- [ ] LSP smoke test (manual or scripted): server starts, sync cache fills
      when configured.
- [ ] `--no-uri-hints` and `--uri-sync-batch-size` also accepted.
- [ ] Default behavior unchanged (sync off in LSP without flags).

## Resolved Design Questions

1. **Placeholder detection: verify_cmd only, auto only, or both?**
   **Both, in this order: auto → verify_cmd → fallback**. Auto is a fast path
   and best-effort; verify_cmd is the authoritative override. Users who need
   certainty set verify_cmd; users who don't get heuristic answers with the
   documented failure modes.

2. **Batch fan-out: positional, stdin, or both?**
   **Positional default, stdin opt-in via `{paths}` placeholder.** Positional
   is simpler and shell-free; stdin handles the `rclone`-style case without
   forcing users to write shell wrappers.

3. **Should `downlint sync` modify files outside a mapping's `root`?**
   **No.** Same security model as Phase 1: sync runs only against resolved
   paths inside a configured `root`. The CLI does no extra traversal.

4. **Should `DNL008` include the sync command that failed?**
   **No.** Echoing the command would leak user-configured secrets (e.g.,
   `aws s3 cp … --profile prod-credentials`). The diagnostic names the
   mapping (which is already in `.downlint.toml`) and the path, which is
   enough to find the failure.

5. **What happens when a Phase 1 config is loaded by a Phase 2 build?**
   **No migration needed.** New keys default to absent, old configs parse
   identically. `batch_size = 1` users get the same behavior as before
   (positional expansion with N=1 ≡ single-file spawn).

6. **`DNL009 BatchClamped` per-run or per-process lifetime?**
   **Per-run for the CLI; lifetime-shared for the LSP via the existing
   `UriSyncCache`.** Same model as `DNL007`.

## Out of Scope for Phase 2

These are tracked for a future RFC (v1.3+):

- **Auto-tuning `batch_size` per mapping** — optimization, no correctness
  gain.
- **Glob patterns for prefixes** — explicitly rejected in Phase 1.
- **`std::env::set_var` race in tests** — cosmetic; only matters if a future
  test reuses the same env var name.
- **Windows backslash handling in `expand_env_vars`** — niche; fix when a
  Windows user reports it.
- **Deleting dead `diagnostics::manager.rs`** — pre-existing dead code.
- **Streaming progress for `downlint sync`** — `rclone`-style progress bars.
  Phase 2 prints a final summary; streaming is Phase 3.
- **`workspace/didChangeConfiguration` for LSP flags** — must restart server.

## Migration Path

Phase 2 is fully backward compatible. Existing Phase 1 configs:

- `[[uri.mappings]]` with `sync_cmd` set → same behavior with `batch_size = 1`
  (positional expansion of one path ≡ per-file spawn).
- `auto_verify = "on"` (new default) runs heuristics; if a user's sync tool
  already produces real files, heuristics say "real" and there is no
  regression. If a user's sync tool produces placeholders, heuristics catch
  them — a behavior improvement, not a regression.
- `DNL008` only fires when `sync_required = false` and sync ran; the user
  must already be running with `--allow-uri-sync` to hit this case.

The only behavior change visible to default users (no `[uri]` section): zero.