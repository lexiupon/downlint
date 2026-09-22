# Downlint

A Rust Markdown checker and language server for wiki-link note vaults. The LSP
surface is inspired by Marksman; the resolution model (Obsidian-style
`[[wiki-links]]`, mounts, URI schemas) is downlint's own.

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

### `[[mounts]]` — Co-Equal Resolution Roots

Mount an additional folder so its documents resolve alongside the primary project —
for cross-project wikilinks, or a shared asset folder that is also a note vault.
Mounted documents are **co-equal** with primary documents (not a fallback): a link
matching documents in both is `link/ambiguous`.

| Key | Type | Default | Description |
|---|---|---|---|
| `root` | string | *(required)* | The folder to mount. Supports `~`, env vars, and relative-to-config-dir paths. |
| `prefix` | string | *(none)* | Workspace-absolute virtual path (e.g. `/kb`) that reaches this mount. Without a prefix, the mount's subfolders behave as if in the current folder. |
| `lint` | bool | `false` | When `true`, the mount's documents are linted as sources (their own links are diagnosed, resolved against the full namespace). |

Example:

```toml
# Mount a second vault, reachable by bare title and via `/kb/...`.
[[mounts]]
root = "~/vaults/work"
prefix = "/kb"

# Mount a shared asset folder that is also a note vault, and lint it.
[[mounts]]
root = "~/notes"
lint = true
```

Relative links never cross into a mount; workspace-absolute links reach a mount via its
`prefix`; bare stem/title wiki links resolve across the whole namespace. A mount whose
`prefix` or top-level folder collides with the primary project emits a `mount/conflict`
(Error) diagnostic; the conflicting namespace is suspended until the config is corrected.

### `[[schemas]]` — External Asset Scheme Mapping

Maps URI-style wiki/markdown link targets (`icloud://assets/...`,
`onedrive://work/...`) to local filesystem roots so external assets (large
PDFs/XLSX in iCloud/OneDrive) can be validated like any other link. Resolution
is **rewrite + stat + verify** — downlint never downloads or hydrates files; it
only checks that the resolved path exists and isn't an evicted cloud placeholder.

| Key | Type | Default | Description |
|---|---|---|---|
| `prefix` | string | *(required)* | Scheme prefix to match (e.g. `icloud://assets/`). Trailing `/` is optional and normalized automatically. |
| `root` | string | *(required)* | Local filesystem path. Supports `~`, env vars (`$VAR`, `${VAR}`), and relative-to-config-dir paths. |
| `auto_verify` | bool | `true` | Run the built-in evicted-placeholder heuristics (vendor-specific: iCloud `.icloud` sibling, OneDrive `._<name>` resource fork). |
| `verify_cmd` | string[] | `null` | Custom placeholder check (advanced). Exit 0 = real file, non-zero = placeholder. Honors `{path}` substitution. Requires `--allow-uri-sync`. |

Schemas are checked in declaration order — put more-specific prefixes first.

#### Examples

iCloud (macOS):

```toml
[[schemas]]
prefix = "icloud://assets/"
root = "~/icloud/assets"
```

OneDrive with a custom `verify_cmd`:

```toml
[[schemas]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
# `file` exits 0 only when the file is fully downloaded.
verify_cmd = ["file", "{path}"]
```

#### CLI flags

| Flag | Effect |
|---|---|
| `--allow-uri-sync` | Permit subprocess execution of a `verify_cmd`. **Security-sensitive**: anyone who can commit `.downlint.toml` can configure a `verify_cmd` to run on your machine. Without this flag, `verify_cmd` is skipped (treated as inconclusive). |
| `--no-uri-hints` | Suppress the `uri/no-mapping` "no scheme mapping found" hint while keeping the broken-link diagnostic. |

#### Diagnostics

| Code | Severity | Meaning |
|---|---|---|
| `link/broken` | error (wiki) / warning (inline) | Broken link — scheme mapped to a missing file or an evicted placeholder. |
| `uri/no-mapping` | info | No `[[schemas]]` prefix matched the scheme target. Configure `[[schemas]]` or set `--no-uri-hints`. |

The built-in heuristics detect OneDrive placeholders (zero-byte `._<name>`
resource fork on macOS) and iCloud `.icloud` siblings under `Mobile Documents/`.
They run only when `auto_verify` is enabled (default) and only when no
`verify_cmd` is set.

### LSP `--allow-uri-sync`

The `downlint server` subcommand accepts the same URI flags as `downlint check`:

```
$ downlint server --allow-uri-sync --no-uri-hints
```

These thread into `ServerState.uri_opts` at startup. Without `--allow-uri-sync`,
the LSP never runs a `verify_cmd`. To change flags you must restart the server.

Normative behavior: [spec/linting.md](./spec/linting.md) §3.7 (RES-07) and §4.5.

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
applying them. Mounted documents (`[[mounts]]`) are in the rename scope
alongside the primary project.

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