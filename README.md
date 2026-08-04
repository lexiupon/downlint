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

See [rfc/0006-external-asset-uri-mapping.md](./rfc/0006-external-asset-uri-mapping.md)
for the full specification.
