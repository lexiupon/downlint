# RFC: External Asset URI Mapping

## Status

Draft

> **Note**: this RFC originally used `sync_cmd`, `sync_required`, and
> `sync_timeout` as config field names. Those were renamed to `warm_cmd`,
> `warm_required`, and `warm_timeout` per [RFC 0008](./0008-uri-warm-rename.md).
> The historical document below reflects the original design vocabulary;
> see the linked RFC for the current user-facing names.

## Motivation

In `~/kb-work`, large binary assets (PDFs, XLSX files >500KB) have been moved from the
git-tracked `assets/` folder to OneDrive at
`~/Library/CloudStorage/OneDrive-Work/assets/`. References in markdown use a URI-style
path:

```markdown
[[OneDrive-Work/assets/20250911-example-audit/Example report.xlsx]]
```

Downlint currently treats these as broken links because:

1. The files no longer exist in the git-tracked workspace.
2. Downlint has no way to know that `OneDrive-Work/...` maps to a local filesystem path.

This creates friction: every time large assets are added or moved, downlint reports them as
broken links even though they exist and are accessible.

## Problem

The current approach has several core problems:

### 1. No URI scheme awareness in wiki links

Downlint's `resolve_inline_ref()` checks for URI schemes (e.g., `https://`, `http://`) and
treats them as external — no diagnostic is emitted. However, `resolve_wiki_ref()` does **not**
have this check, so wiki links with URI schemes still get validated against local files and
reported as broken.

### 2. No configurable external path mapping

There is no mechanism to tell downlint: "this prefix maps to that local folder." For example:

- `OneDrive-Work/assets/` → `~/Library/CloudStorage/OneDrive-Work/assets/`
- `s3://cost-reports/` → `./external/s3/cost-reports/`

Without this mapping, downlint can never validate external assets.

### 3. Multiple accounts / buckets per scheme

A single scheme-level mapping is insufficient:

- **OneDrive**: Multiple accounts may be mounted at different local paths.
  - `onedrive://work/` → `~/Library/CloudStorage/OneDrive-Work`
  - `onedrive://personal/` → `~/Documents/OneDrive-Personal`
- **S3**: Multiple buckets with different local prefixes.
  - `s3://bucket-a/reports/` → `./external/s3/bucket-a/`
  - `s3://bucket-b/assets/` → `/Volumes/NAS/s3-bucket-b/`

A scheme-based mapping cannot express this many-to-one relationship.

### 4. Sync uncertainty for cloud storage

For cloud-synced folders (OneDrive, Google Drive, Dropbox), there is no reliable CLI guarantee
that files are locally available:

- `mdutil --enforce-locals` on macOS affects Spotlight metadata but does not guarantee
  file download (on-demand files may still be cloud placeholders).
- No standard CLI for forcing S3/Google Drive sync.

Downlint needs a way to express sync intent and handle partial availability gracefully.
Crucially, sync must operate on a **per-file** basis — attempting to sync an entire OneDrive
mount or a large S3 prefix per link would be dangerous, slow, and would saturate the
network.

### 5. Link volume

A single doc can reference hundreds of external assets. Spawning one sync subprocess per link
would multiply cost by the number of links and overwhelm cloud-sync daemons. Sync must be
**batched** and its results **cached**.

## Proposal

### Overview

Add a `[uri]` section to `.downlint.toml` that maps URI prefixes to local filesystem roots.
When downlint encounters a wiki-link or markdown-link target matching a configured prefix, it:

1. Strips the prefix and resolves the remaining path against the mapped root.
2. Checks if the file exists on the local filesystem.
3. Optionally runs a per-file sync command before validation, batched and cached.

Sync requires explicit opt-in via a CLI flag (`--allow-uri-sync`) so that committing a
`.downlint.toml` with arbitrary `sync_cmd` entries cannot cause unintended subprocess
execution on machines that haven't opted in.

### Config Schema

```toml
[uri]
# A list of prefix-to-root mappings.
# Links are matched against prefixes in order; the first match wins.

[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
sync_required = false
sync_timeout = 30

[[uri.mappings]]
prefix = "onedrive://personal/"
root = "~/Documents/OneDrive-Personal"
sync_cmd = null
sync_required = false

[[uri.mappings]]
prefix = "s3://cost-reports/"
root = "./external/s3/cost-reports"
sync_cmd = null
sync_required = false

[[uri.mappings]]
prefix = "internal://contracts/"
root = "$VOLUMES/nas/contracts"
sync_cmd = ["aws", "s3", "cp", "{path}", "/dev/null"]
sync_required = true
sync_timeout = 60
```

### Configuration Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `prefix` | string | *(required)* | URI prefix to match against link targets. Links starting with this prefix will be resolved against the mapped `root`. Trailing slash is optional and normalized automatically (see *Prefix Normalization* below). |
| `root` | string | *(required)* | Local filesystem path to resolve link targets against. Supports `~` expansion, environment-variable expansion, and relative paths (relative to `.downlint.toml`). |
| `sync_cmd` | string[] | `null` | CLI command to run per batched sync pass before validation. `{path}` is replaced with the resolved full file path. Sync only runs when `--allow-uri-sync` is passed. |
| `sync_required` | bool | `false` | If `true`, sync failure causes the affected link to be reported as broken in the diagnostic output. If `false`, sync failure emits a warning and validation continues (the link is reported as broken if the file is still missing after sync). |
| `sync_timeout` | int | `30` | Maximum seconds to wait for the sync command to complete. |

### Link Resolution Behavior

When downlint validates a link target:

1. **Check for URI scheme**: If the target starts with a recognized URI pattern
   (`[a-zA-Z][a-zA-Z0-9+.-]*://`), look up matching mappings.
2. **Find first matching prefix**: Iterate through `uri.mappings` in order; the first
   `prefix` that matches the start of the target string wins.
   - **No matching mapping**: if the target has a URI scheme but no configured prefix
     matches, downlint reports the link as **broken** (URI-scheme targets cannot be
     resolved against the local workspace) and emits a **hint** pointing the user to
     `[uri.mappings]` in `.downlint.toml`. The hint is suppressed if validation is
     disabled for this file (e.g., via `exclude`) or if `--no-uri-hints` is passed.
3. **Resolve against root**: Strip the matched prefix from the target and join with the
   `root`. Apply path normalization and expansion (see *Path Resolution*).
4. **Sync deferral**: If `sync_cmd` is configured but `--allow-uri-sync` was **not**
   passed on the CLI, emit an info-level diagnostic per affected link ("external sync
   skipped: pass `--allow-uri-sync`") and skip the sync. File existence is still checked.
5. **Batched sync**: If `sync_cmd` is configured and `--allow-uri-sync` was passed:
   - Collect all resolved full file paths across the run.
   - Group by the `sync_cmd` they map to (so each mapping has its own queue).
   - Execute one subprocess per batch, with each `{path}` placeholder receiving all paths
     in that batch. See *Batching & Caching*.
6. **File existence check**: After (or instead of) sync, if the resolved path exists on
   disk, mark the link as valid. Otherwise:
   - If `sync_required = true` → report the link as **broken** in the diagnostic output.
   - If `sync_required = false` → emit a warning, then report the link as broken if the
     file is still missing; otherwise mark valid.

### URI Detection

A target is considered to have a URI scheme if it matches the pattern
`[a-zA-Z][a-zA-Z0-9+.-]*://...`. (RFC 3986 additionally permits `%` in schemes; this is
sufficient for practical use and matches the existing `has_scheme()` helper in
`resolve_inline_ref()`.)

This check is performed only when validation runs for the link — never as a separate
discovery pass. Repositories without `[uri]` configured do not enable URI scheme
detection on their own, and validation continues to treat URI-scheme targets as broken
local links in those repositories.

### Prefix Normalization

Prefix matching is forgiving:

- Trailing slashes on `prefix` are optional. `onedrive://work` and `onedrive://work/` both
  match `onedrive://work/foo.xlsx`.
- The matched slice that is stripped from the target is the canonical form (with trailing
  slash) so that path joining yields `root + "/foo.xlsx"`, not `root + "foo.xlsx"`.
- Mappings are checked in declaration order. Put more-specific prefixes first
  (e.g., `onedrive://work/bucket-a/` before `onedrive://work/`).

### Path Resolution

Resolved paths are expanded in this order:

1. **Environment variables**: All `$VAR` and `${VAR}` occurrences in `root` are replaced
   with `std::env::var("VAR")`. Missing variables cause a config error at startup.
2. **`~` expansion**: A leading `~` (or `~/...`) is replaced with the user's home
   directory. Cross-platform expansion uses the `dirs` crate
   (`dirs::home_dir()`), not `HOME` env var, so Windows builds resolve correctly.
3. **Relative roots**: Paths that are not absolute after steps 1–2 are resolved relative
   to the directory containing `.downlint.toml`.

Example:

```toml
# .downlint.toml at ~/kb-work/.downlint.toml, with VOLUMES=/Volumes in env
[[uri.mappings]]
prefix = "internal://contracts/"
root = "$VOLUMES/nas/contracts"
# After expansion: /Volumes/nas/contracts
```

### Batching & Caching

Sync is **never** per-link and **never** per-root. Instead:

1. After URI resolution, collect all distinct resolved file paths across the entire run,
   grouped by their parent mapping.
2. For each group, run the configured `sync_cmd` **once per batch**. The implementation
   passes all paths in the batch to the command by substituting each `{path}` with the
   batch's path list (command-specific: see below).
3. Cache results. A per-run cache stores, for each batch, whether sync succeeded and
   whether each individual file is present after sync.
4. Subsequent link validations within the same run consult the cache; they do not spawn
   additional subprocesses.

#### Substituting `{path}` with a batch

`{path}` is replaced with the resolved full file path. When multiple paths are in a
batch, the command is invoked once per path (the batch is fanned out internally), so
the *total* subprocess count is bounded by the number of paths in the group, not by
the number of links × mappings. The CLI flag `--uri-sync-batch-size` controls how many
paths are submitted per batch when the command accepts a list (e.g., `aws s3 cp` with
multiple `--include` filters); the default is 50. For commands like `mdutil` that
operate on a single path, batching collapses to one invocation per path.

#### LSP integration

The language server must share this cache. The first request resolves and syncs as
above; subsequent document-change events reuse cached results unless the resolved path
or mapping configuration changed. Cache invalidation keys on the absolute resolved
path; file deletions outside downlint are detected by re-stat'ing the path only on
explicit requests (Phase 2). This avoids re-running sync on every keystroke.

### Default Behavior (No Mappings Configured)

If `[uri]` is not present in `.downlint.toml`:

- All links (wiki and markdown) are validated against the local workspace as before.
- No special handling for URI-scheme targets.
- **No breaking change** to existing behavior.

### Sync Command Execution

When sync runs:

1. `sync_cmd[0]` is the binary name (e.g., `mdutil`, `aws`).
2. Each `{path}` occurrence in any argument is replaced with the resolved full file
   path (already expanded and absolute).
3. The command runs with a deadline of `sync_timeout` seconds.
4. Exit code 0 = success; any other exit code = failure.
5. If the binary is not found, treat as failure (and emit the warning/error per
   `sync_required`).
6. Only runs when `--allow-uri-sync` is passed on the CLI.

### `--allow-uri-sync` Semantics

`--allow-uri-sync` is a global CLI flag. When absent:

- All sync activity is skipped.
- An info-level diagnostic is emitted once per mapping that *would have* synced, naming
  it, so users can find the flag quickly.

When present:

- All configured `sync_cmd` entries execute (subject to batching above).
- This is documented as a *security-sensitive* flag in `--help` and the man page, because
  `.downlint.toml` controls which subprocesses are spawned. See *Security*.

### Security

`.downlint.toml` is a regular file in the workspace. Anyone who can commit it can
configure a `sync_cmd` that runs arbitrary code on any machine that runs
`downlint check --allow-uri-sync`. This is acceptable for personal repos but is a real
risk for shared repos and CI:

- `.downlint.toml` is **never** silently trusted for sync. `--allow-uri-sync` is the gate.
- CI runners should not pass `--allow-uri-sync` unless the repo is vetted.
- The flag is opt-in per invocation; there is no config-level "always allow".

### Examples

#### Example 1: Basic OneDrive mapping

```toml
[uri]
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
sync_required = false
```

Link: `[[onedrive://work/20250911-example-audit/report.xlsx]]`

Resolution: `~/Library/CloudStorage/OneDrive-Work/assets/20250911-example-audit/report.xlsx`

#### Example 2: S3 mapping with sync

```toml
[uri]
[[uri.mappings]]
prefix = "s3://reports/"
root = "/Volumes/NAS/s3-reports"
sync_cmd = ["aws", "s3", "cp", "{path}", "/dev/null"]
sync_required = false
```

Link: `[[s3://reports/q4-2025/financials.xlsx]]`

Resolution: `/Volumes/NAS/s3-reports/q4-2025/financials.xlsx`

#### Example 3: Local-only (no sync)

```toml
[uri]
[[uri.mappings]]
prefix = "internal://docs/"
root = "./internal-docs"
sync_cmd = null
sync_required = false
```

Link: `[[internal://docs/api-reference.md]]`

Resolution: `./internal-docs/api-reference.md` (relative to `.downlint.toml`)

## Trade-offs

### Pros

1. **Real validation**: Downlint can actually verify that external assets exist on disk,
   not just suppress warnings.
2. **Flexible**: Prefix-based mapping handles multiple accounts, buckets, and paths per
   scheme.
3. **Opt-in**: No breaking change — if `[uri]` is not configured, behavior is unchanged.
4. **Safe by default**: `--allow-uri-sync` is required for any subprocess execution; the
   security surface is gated by a CLI flag.
5. **Per-file sync**: Sync targets the resolved file path, not the entire root, so it
   doesn't accidentally trigger bulk downloads.
6. **Batched and cached**: One subprocess per batch (not per link), with results reused
   across the run and across LSP events.
7. **Cross-platform**: `~` expansion uses `dirs::home_dir()` so Windows, macOS, and Linux
   resolve correctly.
8. **Extensible**: New external asset types (S3, GCS, local NAS, etc.) can be added
   without code changes.

### Cons

1. **Sync is not guaranteed**: `mdutil --enforce-locals` and similar tools do not
   guarantee file download. On-demand cloud files may still be placeholders.
2. **Partial sync is the worst case**: Some files in a batch may sync and others not,
   producing inconsistent per-link outcomes within a single run.
3. **Config complexity**: More config options = more surface area for bugs and questions.
4. **Performance**: Batched subprocess execution and stat checks add overhead, but it
   is bounded (one subprocess per batch, not per link).
5. **Platform specificity**: OneDrive paths differ between macOS, Windows, and Linux.
   Users need platform-specific configs for cross-machine repos.

### Mitigations

1. **Sync tolerance**: `sync_required = false` prevents CI failures when sync is
   unreliable.
2. **Timeout protection**: `sync_timeout` prevents hanging on slow/unresponsive sync
   tools.
3. **Per-file granularity**: Sync targets the resolved file, so a partial sync manifests
   as a per-file broken-link diagnostic rather than a single catastrophic failure.
4. **Cache reuse**: LSP and CLI both reuse batch results within a run.
5. **Clear defaults**: `null` for `sync_cmd` and `false` for `sync_required` mean the
   mapping feature is opt-in and safe by default. `--allow-uri-sync` is the explicit
   gate for any subprocess execution.
6. **Documentation**: Provide examples for common platforms and cloud services.

## Migration Path

### Phase 1: Basic URI mapping (this RFC)

- Add `[uri.mappings]` config section with `prefix`, `root`, `sync_cmd`,
  `sync_required`, `sync_timeout`.
- Implement prefix matching (with automatic trailing-slash normalization).
- Implement `~`, env-var, and relative-path resolution using `dirs::home_dir()`.
- Add `has_scheme()` check to `resolve_wiki_ref()` (so URI-scheme wiki links are
  recognized even without mappings).
- Implement per-file sync gated by `--allow-uri-sync`, with batching and caching.
- Report per-link broken-link diagnostics when sync fails or files are missing.

### Phase 2: Sync verification (future)

- After running `sync_cmd`, verify that the resolved path contains an actual file
  (not a cloud placeholder).
- For OneDrive: check for `.com-one-drive` markers or use Finder metadata to detect
  on-demand files.

### Phase 3: CLI integration (future)

- Add `downlint sync` subcommand to trigger sync for all configured mappings without
  running validation.
- Expand `--uri-sync-batch-size` knob; consider auto-tuning per mapping.

## Resolved Design Questions

1. **Multiple sync commands per mapping?** No. One `sync_cmd` per mapping. Users needing
   multiple steps can wrap them in a script.
2. **Glob patterns for prefixes?** No, not in Phase 1. Prefix matching only. Globs add
   complexity without a clear current need.
3. **Hint when no mapping matches a URI link?** Yes. Emit a hint
   ("No URI mapping found for `<target>`. Configure `[uri.mappings]` in
   `.downlint.toml`.") alongside the broken-link diagnostic for any URI-scheme target
   that doesn't match a configured prefix. The hint is gated by validation actually
   running on that link (so excluded files are silent) and can be suppressed globally
   with `--no-uri-hints`.
4. **Environment-variable expansion in `root`?** Yes. `$VAR` and `${VAR}` are
   expanded before `~` expansion. Missing variables produce a config error at startup,
   not a silent failure.
