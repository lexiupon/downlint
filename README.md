# Downlint

Rust implementation of a Markdown checker and LSP inspired by Marksman.

## Configuration

Downlint reads a `.downlint.toml` at the workspace root. All sections are optional;
unspecified keys fall back to defaults. Run `downlint init` to scaffold one — common
options are written active (at their defaults) and advanced options are commented out
for opt-in.

### `[wiki]`

| Key | Type | Default | Description |
|---|---|---|---|
| `obsidian_prefix` | bool | `false` | When `true`, wiki-link targets resolve against leading prefixes of file stems. For example, `[[20260801-topic-a]]` matches `20260801-topic-a-sub-x.md`. When `false` (default), a wiki-link target must equal a file stem (modulo existing title-slug and relative-path matching). When `false` and a broken wiki-link target is a leading prefix of one or more file stems, the `link/broken` diagnostic gains a hint listing the candidates so the user can discover the option. Multi-match cases (the prefix matches more than one file) emit `link/ambiguous` ambiguous-link diagnostics, regardless of the flag value. |

Example:

```toml
[wiki]
obsidian_prefix = true
```

### `[uri]` — External Asset URI Mapping

Maps URI-style wiki/markdown link targets (`onedrive://work/...`, `s3://...`) to
local filesystem roots so external assets (large PDFs/XLSX in OneDrive, files
mirrored from S3) can be validated like any other link.

| Key | Type | Default | Description |
|---|---|---|---|
| `prefix` | string | *(required)* | URI prefix to match. Trailing `/` is optional and normalized automatically. |
| `root` | string | *(required)* | Local filesystem path. Supports `~`, env vars (`$VAR`, `${VAR}`), and relative-to-config-dir paths. |
| `warm_cmd` | string[] | `null` | Per-file warming command; `{path}` is replaced with the resolved absolute file. Requires `--allow-uri-sync`. |
| `warm_required` | bool | `false` | If `true`, warming failure makes the link broken. |
| `warm_timeout` | int | `30` | Seconds to wait for `warm_cmd` per invocation. |

Mappings are checked in declaration order — put more-specific prefixes first.

#### Examples

OneDrive (macOS):

```toml
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
warm_cmd = ["mdutil", "--enforce-locals", "{path}"]
warm_required = false
```

S3 mirror (via NAS):

```toml
[[uri.mappings]]
prefix = "s3://reports/"
root = "/Volumes/NAS/s3-reports"
warm_cmd = ["aws", "s3", "cp", "{path}", "/dev/null"]
```

Local-only, no warming:

```toml
[[uri.mappings]]
prefix = "internal://docs/"
root = "./internal-docs"
```

#### CLI flags

| Flag | Effect |
|---|---|
| `--allow-uri-sync` | Permit subprocess execution of `warm_cmd` entries. **Security-sensitive**: anyone who can commit `.downlint.toml` can configure a `warm_cmd` to run on your machine. Without this flag, warming is skipped and a `uri/sync-skipped` info diagnostic is emitted per mapping. |
| `--no-uri-hints` | Suppress the `uri/no-mapping` "no URI mapping found" hint while keeping the broken-link diagnostic. |
| `--uri-sync-batch-size` | Batch size for warming invocations (default 50). Lower reduces memory, higher reduces fork overhead. |

#### Diagnostics

| Code | Severity | Meaning |
|---|---|---|
| `link/broken` | error (wiki) / warning (inline) | Broken link — URI mapped to a missing file or `warm_cmd` failed with `warm_required = true`. |
| `uri/no-mapping` | info | No `[uri.mappings]` prefix matched the URI target. Configure `[[uri.mappings]]` or set `--no-uri-hints`. |
| `uri/sync-skipped` | info | `warm_cmd` is configured but `--allow-uri-sync` was not passed. The diagnostic message names `downlint warm-uri-mappings` so you can warm them explicitly. |
| `uri/sync-failed` | info | Warming ran (`warm_required = false`) but the file is still missing — a soft warning alongside `link/broken`. |
| `uri/batch-clamped` | info | A `warm_cmd` batch's argv exceeded 128 KiB and the runner fell back to per-file. Emitted once per mapping per run. |

### `[uri]` Phase 2 — verify_cmd and heuristics

`[uri.mappings]` gained two new keys and one section-level key:

| Key | Scope | Default | Description |
|---|---|---|---|
| `verify_cmd` | per-mapping | absent | Post-warm placeholder check. Exit 0 = real file, non-zero = placeholder. Honors `{path}` substitution. |
| `auto_verify` | `[uri]` | `"on"` | Toggles the built-in heuristics. Values: `"on"`, `"off"`, `"onedrive-only"`, `"icloud-only"`. |

The heuristics detect OneDrive placeholders (resource forks on macOS),
iCloud `.icloud` siblings under `Mobile Documents/`, and 0-byte files in
cloud-storage paths. They run before `verify_cmd` so a confident "real"
classification skips the configured command.

OneDrive with `verify_cmd`:

```toml
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
warm_cmd = ["mdutil", "--enforce-locals", "{path}"]
# `file` exits 0 only when the file is fully downloaded.
verify_cmd = ["file", "{path}"]
warm_required = false
```

`rclone` batch with stdin:

```toml
[[uri.mappings]]
prefix = "s3://reports/"
root = "/Volumes/NAS/s3-reports"
# `{paths}` triggers stdin piping (one path per line).
warm_cmd = ["rclone", "copy", ":http:/s3.example/reports/{path}", "{path}"]
```

Note: the simple `aws s3 cp` per-file pattern still works — batch fan-out
is automatic when `batch_size > 1`. Only use `{paths}` for tools that
natively read a list from stdin.

### `downlint warm-uri-mappings` subcommand

Cache warming without validation. Useful as a one-shot step before running
`check` so that all external files are hydrated locally first:

```
$ downlint warm-uri-mappings --allow-uri-sync
Sync summary (76ms):
  mapping[0]: total=42 synced=42 failed=0 missing=0 timed_out=0
$ echo $?
0
```

The subcommand:

- Always requires `--allow-uri-sync` (exits 2 with a clear error otherwise).
- Walks every URI-scheme link target (both resolved and unresolved) in the
  workspace.
- Batches per mapping (positional or stdin, depending on `warm_cmd`).
- Does **not** run validation, does **not** emit `link/broken` broken-link
  diagnostics.
- Exit code 0 on success, 1 on any per-path failure, 2 on gate / config.

A short alias `downlint warm-uri` is also accepted.

### LSP `--allow-uri-sync`

The `downlint server` subcommand accepts the same three URI flags as
`downlint check`:

```
$ downlint server --allow-uri-sync --no-uri-hints --uri-sync-batch-size 10
```

These thread into `ServerState.uri_opts` at startup. Without
`--allow-uri-sync`, the LSP never runs `warm_cmd`. To change flags you
must restart the server (no `workspace/didChangeConfiguration` in Phase 2).

### Naming history

The config keys were originally `sync_cmd` / `sync_required` / `sync_timeout`
and the subcommand was `downlint sync`. They were renamed because "sync" carries misleading two-way-sync connotations. The new vocabulary is "warm" — pulling files
locally so subsequent validation is fast.

Existing configs using `sync_cmd` etc. produce a clear parse error:

```
unknown field `sync_cmd`, expected one of `prefix`, `root`,
`warm_cmd`, `warm_required`, `warm_timeout`, `verify_cmd`
```

Migration is a single `sed`:

```sh
sed -i.bak '
  s/^sync_cmd = /warm_cmd = /
  s/^sync_required = /warm_required = /
  s/^sync_timeout = /warm_timeout = /
' .downlint.toml
```

Normative behavior: [spec/linting.md](./spec/linting.md) §3.7 (RES-07) and
§4.5–4.8.

## Rename

Downlint can rename files, headings, and link-target identifiers safely
across the workspace. The LSP and CLI share a single rename library; both
call `plan_rename(input) -> RenamePlan` and then either serialize to a
`WorkspaceEdit` (LSP) or apply text-first-then-disk (CLI). The product
behavior is in [spec/downlint.md](./spec/downlint.md) §4.

### LSP gestures

| Gesture | Operation | Never moves files? |
|---|---|---|
| `textDocument/rename` (F2) | String-only at the cursor. Replaces the link target or heading text. | yes |
| `refactor.rename.file` (code action) | Moves a file on disk and rewrites every reference. | no |
| `refactor.rename.link-target` (code action) | Workspace-wide string rewrite with conflict + blocking checks. | yes (no disk move) |
| `refactor.rename.heading` (code action) | Recomputes the heading slug and rewrites every `#section` in referencing links. | yes |

The string-only F2 default is intentional — the cursor on `[[report]]`
is fundamentally ambiguous (could be a file stem, an H1 title, a prefix
of multiple files, or unresolved). `textDocument/rename` cannot ask the
user "which interpretation?" before honoring F2. File moves are
surfaced as code actions where the user picks the operation from a
menu.

### CLI

```
$ downlint rename-file --from reports/2024-q1.md --to reports/q1.md
$ downlint rename-file --from assets/diagrams/old-flow.png \
                      --to assets/diagrams/new-flow.png
$ downlint rename-link --from report --to topic
$ downlint rename-file --from old.md --to new.md --dry-run
```

`rename-file` accepts any file on disk — markdown or attachment. The
kind-class is inferred from the source extension, so users don't have
to think about it. `rename-link` rewrites a logical identifier
workspace-wide without touching disk.

Both subcommands accept `--dry-run` to print planned edits without
applying them, and `--config` to override the default config lookup.
`--allow-extra-folders` (default true) includes `extra_folders` in the
scope of the rename.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (or dry-run clean) |
| 1 | Blocked by an existing `Warning`/`Error` diagnostic on an unrelated occurrence |
| 2 | Conflict detected (path collision, prefix shadow, or extension class mismatch) |
| 3 | Bad arguments / config error / indexing in progress |

### Blocking rule

A rename is refused when any document that would be rewritten contains a
`Warning` or `Error` diagnostic other than on the occurrence(s) being
rewritten. `Information` and `Hint` diagnostics do not block. The error
message lists every blocking diagnostic inline.

```
$ downlint rename-file --from report.md --to topic.md
error: rename blocked — 1 occurrence has diagnostic `link/broken`
  → docs/index.md:42  [[2024-q1]]  Broken link: '2024-q1' could not be resolved
hint: fix the broken reference first, then re-run the rename.
```

The rationale: a propagating rename is about to mutate text in a
document. If that document already has diagnostics, the rename would
compound existing problems — propagating broken links, burying
ambiguities, or masking data-flow issues. The user must clean up
first.