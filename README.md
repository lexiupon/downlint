# Downlint

Rust implementation of a Markdown checker and LSP inspired by Marksman.

## Configuration

Downlint reads a `.downlint.toml` at the workspace root. All sections are optional;
unspecified keys fall back to defaults.

### `[wiki]`

| Key | Type | Default | Description |
|---|---|---|---|
| `obsidian_prefix` | bool | `false` | When `true`, wiki-link targets resolve against leading prefixes of file stems. For example, `[[20260801-topic-a]]` matches `20260801-topic-a-sub-x.md`. When `false` (default), a wiki-link target must equal a file stem (modulo existing title-slug and relative-path matching). When `false` and a broken wiki-link target is a leading prefix of one or more file stems, the `DNL002` diagnostic gains a hint listing the candidates so the user can discover the option. Multi-match cases (the prefix matches more than one file) emit `DNL001` ambiguous-link diagnostics, regardless of the flag value. |

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
| `sync_cmd` | string[] | `null` | Per-file sync command; `{path}` is replaced with the resolved absolute file. Requires `--allow-uri-sync`. |
| `sync_required` | bool | `false` | If `true`, sync failure makes the link broken. |
| `sync_timeout` | int | `30` | Seconds to wait for `sync_cmd` per invocation. |

Mappings are checked in declaration order — put more-specific prefixes first.

#### Examples

OneDrive (macOS):

```toml
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
sync_required = false
```

S3 mirror (via NAS):

```toml
[[uri.mappings]]
prefix = "s3://reports/"
root = "/Volumes/NAS/s3-reports"
sync_cmd = ["aws", "s3", "cp", "{path}", "/dev/null"]
```

Local-only, no sync:

```toml
[[uri.mappings]]
prefix = "internal://docs/"
root = "./internal-docs"
```

#### CLI flags

| Flag | Effect |
|---|---|
| `--allow-uri-sync` | Permit subprocess execution of `sync_cmd` entries. **Security-sensitive**: anyone who can commit `.downlint.toml` can configure a `sync_cmd` to run on your machine. Without this flag, sync is skipped and a `DNL007` info diagnostic is emitted per mapping. |
| `--no-uri-hints` | Suppress the `DNL006` "no URI mapping found" hint while keeping the broken-link diagnostic. |
| `--uri-sync-batch-size` | Batch size for sync invocations (default 50). Lower reduces memory, higher reduces fork overhead. |

#### Diagnostics

| Code | Severity | Meaning |
|---|---|---|
| `DNL002` | error (wiki) / warning (inline) | Broken link — URI mapped to a missing file or sync_failed with `sync_required = true`. |
| `DNL006` | info | No `[uri.mappings]` prefix matched the URI target. Configure `[[uri.mappings]]` or set `--no-uri-hints`. |
| `DNL007` | info | `sync_cmd` is configured but `--allow-uri-sync` was not passed. |
| `DNL008` | info | Sync ran (`sync_required = false`) but the file is still missing — a soft warning alongside `DNL002`. |
| `DNL009` | info | A `sync_cmd` batch's argv exceeded 128 KiB and the runner fell back to per-file. Emitted once per mapping per run. |

### `[uri]` Phase 2

`[uri.mappings]` gained two new keys and one section-level key:

| Key | Scope | Default | Description |
|---|---|---|---|
| `verify_cmd` | per-mapping | absent | Post-sync placeholder check. Exit 0 = real file, non-zero = placeholder. Honors `{path}` substitution. |
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
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
# `file` exits 0 only when the file is fully downloaded.
verify_cmd = ["file", "{path}"]
sync_required = false
```

`rclone` batch with stdin:

```toml
[[uri.mappings]]
prefix = "s3://reports/"
root = "/Volumes/NAS/s3-reports"
# `{paths}` triggers stdin piping (one path per line).
sync_cmd = ["rclone", "copy", ":http:/s3.example/reports/{path}", "{path}"]
```

Note: the simple `aws s3 cp` per-file pattern still works — batch fan-out
is automatic when `batch_size > 1`. Only use `{paths}` for tools that
natively read a list from stdin.

### `downlint sync` subcommand

Cache warming without validation:

```
$ downlint sync --allow-uri-sync
Sync summary (76ms):
  mapping[0]: total=42 synced=42 failed=0 missing=0 timed_out=0
$ echo $?
0
```

The subcommand:

- Always requires `--allow-uri-sync` (exits 2 with a clear error otherwise).
- Walks every URI-scheme link target (both resolved and unresolved) in the
  workspace.
- Batches per mapping (positional or stdin, depending on `sync_cmd`).
- Does **not** run validation, does **not** emit `DNL002` broken-link
  diagnostics.
- Exit code 0 on success, 1 on any per-path failure, 2 on gate / config.

### LSP `--allow-uri-sync`

The `downlint server` subcommand accepts the same three URI flags as
`downlint check`:

```
$ downlint server --allow-uri-sync --no-uri-hints --uri-sync-batch-size 10
```

These thread into `ServerState.uri_opts` at startup. Without
`--allow-uri-sync`, the LSP never runs `sync_cmd`. To change flags you
must restart the server (no `workspace/didChangeConfiguration` in Phase 2).

See [rfc/0007-uri-mapping-phase-2.md](./rfc/0007-uri-mapping-phase-2.md)
for the Phase 2 specification and the original
[rfc/0006-external-asset-uri-mapping.md](./rfc/0006-external-asset-uri-mapping.md)
for Phase 1.
