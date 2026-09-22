# RFC 0016 — Workspace info command

**Status:** Proposed
**Date:** 2026-09-24
**Scope:** New read-only CLI subcommand `downlint info`. No changes to resolution
semantics, linting, or existing commands.

## 1. Motivation

Mounts and schemas are the most configuration-heavy part of `.downlint.toml`, and
the part users most often get subtly wrong (RFC 0014 was dedicated to untangling
their naming). Today there is no way to ask *"what does downlint actually see?"*
without running `check` and reverse-engineering the answer from diagnostics:

- Which mounts are active, what **absolute** folder each resolves to, and what
  namespace prefix (`as`) each contributes?
- How many documents did each mount actually index?
- Which schemas are configured, what **absolute** folder each `to` expands to,
  and is that folder present?
- Are there any namespace-level mount conflicts (RFC 0010/0011)?

`downlint info` answers all of these in one shot by projecting the **resolved**
workspace — not by echoing the config file back. It is a projection layer over
data that already exists, exactly like `resolve` (RFC 0012) and `graph`
(RFC 0015).

**Deliberately out of scope:** a validating `doctor` command (exit 1 on
problems, CI-gateable). `info` is *descriptive only* — it reports what downlint
sees, including `✗ missing` markers for absent folders, but always exits `0` on
a successfully-loaded workspace. A `doctor` is a natural thin follow-up that
reuses the same data; it is not part of this RFC.

## 2. Command shape

A single top-level command, no subcommand:

```
downlint info
downlint info --root <PATH>
downlint info --format json
```

Common flags:
- `--root <PATH>` — override the workspace root (like `check`/`resolve`/`graph`).
- `--format <text|json>` — default `text`. `info` ships with JSON from the start
  (consistent with `graph --format json`, RFC 0015 follow-up): the report is
  structured metadata, so a machine-readable form is trivial and useful.

No `--stdin` (a workspace report needs the on-disk workspace; stdin is a `check`
concept). No `--allow-uri-sync` (a read-only report must never run a schema's
`verify_cmd` subprocess — see §6).

## 3. Data source

`info` reuses the same entry points `graph` uses, but **stops before link
resolution** (it needs the index, the resolved mounts, and the schema mappings —
not the resolved link graph):

```rust
let workspace = discover_workspace(WorkspaceInput::Path(PathBuf::from(".")), root_override)?;
let input = ResolveInput::from_workspace(&workspace);   // parses docs, builds index
// NOTE: no resolve_links(input) — info never resolves references.
```

`ResolveInput::from_workspace` (`src/resolution/mod.rs:127`) already produces
everything the report needs:

| Report field | Source |
|---|---|
| workspace root | `workspace.folder.root` (`DiscoveredFolder.root`) |
| config file | `workspace.folder.config_path` (`Option<PathBuf>`) |
| file extensions | `workspace.config.core.file_extensions` |
| mounts (resolved) | `input.mounts` : `Vec<ResolvedMount>` — `path` (absolute), `r#as`, `lint`, `attribution` |
| per-mount doc count | `input.documents` grouped by `doc.mount` attribution |
| schemas | `workspace.config.schemas.schemas` : `Vec<Schema>` — `uri`, `to`, `auto_verify`, `verify_cmd` |
| schema expanded path | re-expand `to` via `expand_root(&schema.to, &workspace.folder.root)` (`src/resolution/uri.rs:211`) |
| conflicts | `input.conflicts` : `Vec<MountConflict>` — `mount_attribution`, `detail` |
| schema expansion error | `input.uri_error` (`Option<String>`) |

**Why `ResolveInput` and not just `Workspace` + `Config`:** the per-mount
document count and the namespace `conflicts` are only available after
`from_workspace` runs (it loads each mount's documents, tags them with their
`mount` attribution, and detects collisions, `src/resolution/mod.rs:189`).
Reading `Workspace`/`Config` directly would force `info` to re-walk every mount
folder and re-derive conflicts — duplicating logic. Building a `ResolveInput`
reuses it. The cost is parsing every document (no link resolution), which is the
same order of work as `check` minus resolution — fine for an interactive command.

**`info` always operates on the full workspace** (via `.` + `--root`), like
`graph` — never a single file. `WorkspaceInput::Path(PathBuf::from("."))` makes
`collect_documents` walk the whole root, so the document counts are complete.

### 3.1 Per-mount document count

Each `ResolveDocument` carries `mount: Option<String>` — `None` for primary
docs, `Some(attribution)` for mounted docs (`src/resolution/mod.rs:45`). The
attribution string equals `ResolvedMount.attribution` (`as`, or `path` when there
is no `as`, `src/utils/workspace.rs:313`). So the count for a mount `m` is:

```rust
input.documents.iter()
    .filter(|d| d.mount.as_deref() == Some(m.attribution.as_str()))
    .count()
```

Primary count = docs with `mount.is_none()`; mounted count = the rest; total =
`input.documents.len()`. (Two distinct mounts cannot share an `attribution`
without already being a namespace conflict, so grouping by attribution is
unambiguous in the normal case.)

### 3.2 Schema expanded path

`Schema.to` is stored as written (`~/…`, `${VAR}/…`, or relative-to-root). The
resolver expands it the same way at startup
(`UriResolver::new`, `src/resolution/uri.rs:63`, with `config_dir` =
`workspace.folder.root`). `info` re-expands each `to` with the same public
helper `expand_root(&schema.to, &workspace.folder.root)`
(`src/resolution/uri.rs:211`) so the report shows the **absolute** folder.

`expand_root` only fails on a **missing environment variable**
(`ExpansionError::MissingEnvVar`). A `to` pointing at a *nonexistent folder* is
not an error — `expand_root` returns the expanded path un-canonicalized. That
case is reported with a `✗ missing` marker (§5), not an error. A missing env var
is a startup config error: `from_workspace` already records it in
`input.uri_error` (and builds an empty resolver), so `info` surfaces it and exits
`2` (§6) before rendering — it cannot show a resolved path for a schema whose
`to` it cannot expand.

## 4. Report sections

Top to bottom:

1. **Header** — downlint version, workspace root (absolute), config file path
   (or `(defaults)` when no `.downlint.toml` was found), and file extensions.
   (`info` always operates on the full workspace, so there is no single-file
   mode to report.)
2. **Mounts** — one line per `ResolvedMount` (config order): the `as` prefix
   (or `(none)`), the resolved absolute `path`, `lint` (yes/no), the document
   count, and a presence marker.
3. **Schemas** — one line per `Schema` (config order): the `uri` prefix, the
   expanded absolute folder, `auto_verify` (yes/no), whether a `verify_cmd` is
   set, and a presence marker.
4. **Documents** — total, primary, mounted.
5. **Conflicts** — one line per `MountConflict` (`mount: detail`), or `none`.

Presence marker: `✓` when the folder exists on disk, `✗ missing` when it does
not. For a mount this is `ResolvedMount.path.exists()`; for a schema it is the
expanded `to`. These markers are **informational** — they never affect the exit
code (§6).

## 5. Output format

### 5.1 Text (default)

```
downlint 0.11.0
workspace   /Users/jiacao/notes
config      /Users/jiacao/notes/.downlint.toml
extensions  md, markdown

mounts (2)
  kb        /Users/jiacao/kb       lint=yes   42 docs  ✓
  (none)    /Users/jiacao/archive  lint=no     7 docs  ✓

schemas (1)
  icloud://assets/  →  /Users/jiacao/Library/Mobile Documents/.../assets  auto_verify=yes  verify_cmd=no  ✓

documents  56 total  (47 primary · 9 mounted)
conflicts  none
```

- The mount identifier column shows the `as` prefix, or `(none)` when the mount
  has no `as` (its docs are indexed at their plain relative path).
- An empty section renders its header with a count of `0` and no rows
  (e.g. `mounts (0)`), so the shape is stable for scripting.
- `conflicts` lists each as `<attribution>: <detail>`.

### 5.2 JSON (`--format json`)

A single pretty-printed object:

```json
{
  "version": "0.11.0",
  "workspace": "/Users/jiacao/notes",
  "config": "/Users/jiacao/notes/.downlint.toml",
  "file_extensions": ["md", "markdown"],
  "mounts": [
    { "as": "kb", "path": "/Users/jiacao/kb", "lint": true, "docs": 42, "exists": true }
  ],
  "schemas": [
    { "uri": "icloud://assets/", "to": "~/icloud/assets", "expanded": "/Users/jiacao/Library/.../assets",
      "auto_verify": true, "verify_cmd": false, "exists": true }
  ],
  "documents": { "total": 56, "primary": 47, "mounted": 9 },
  "conflicts": []
}
```

- `config` is `null` when no `.downlint.toml` was found.
- `mounts[].as` is `null` when the mount has no `as`.
- `conflicts[]` entries are `{ "mount": <attribution>, "detail": <string> }`.
- Keys are emitted in a stable order (the report structs are built in a fixed
  field order; `serde_json` without `preserve_order` sorts object keys
  alphabetically, matching `resolve`/`graph` JSON output).

## 6. Exit codes

`info` is descriptive, so it has only two outcomes:

| Code | Meaning |
|---|---|
| `0` | workspace loaded and report printed — **even if** some mounts/schemas point at missing folders (shown as `✗ missing`) |
| `2` | bad args / config error: no workspace found, config parse/validation error, or a schema `to` that cannot be expanded (missing env var — `input.uri_error`) |

There is **no exit `1`**: `info` never "finds" a problem the way `check`/`graph`
do. Missing folders are reported, not gated on. (A future `doctor` would own the
exit-1-on-problems behavior.)

**Security:** `info` builds `ResolveInput` but never calls `resolve_links`, so a
schema's `verify_cmd` is never executed and no URI reference is resolved. There
is no `--allow-uri-sync` flag. The only filesystem access is `exists()` on the
mount/schema folders and the document walk performed by `from_workspace`.

## 7. Code impact

- **`src/cli/mod.rs`** — add `Info(InfoArgs)` to the `Command` enum + dispatch;
  `InfoArgs { root: Option<PathBuf>, format: FormatArg }` (both `global = true`,
  matching `GraphArgs`). Map `FormatArg` → `check::OutputFormat` as the `graph`
  dispatch does.
- **`src/cli/info.rs`** (new) — `run_info(InfoOptions) -> i32`:
  - discover workspace + build `ResolveInput` (§3);
  - surface `input.uri_error` → exit `2` (as `graph` does);
  - build the report structs (§4) as pure functions over `(&Workspace, &ResolveInput)`;
  - render text or JSON (§5); return the exit code (§6).
  - Report structs are `Serialize` for the JSON path; text rendering is a small
    per-section formatter. Pure helpers (per-mount count, schema expansion,
    presence) are unit-testable.
- **No changes** to `src/resolution/*`, `src/diagnostics/*`, `src/config/*`, or
  the lint path. (`expand_root` is reused, not modified.)

## 8. Tests

- **Unit** (in `src/cli/info.rs`): per-mount document count (primary vs mounted,
  attribution grouping, a `lint = false` mount still counted), schema expansion
  (relative, `~`, missing-folder → present-but-`exists:false`), presence
  marker, conflict rendering, and the JSON envelope shape (parse the serialized
  report and assert fields).
- **CLI subprocess** (`tests/cli_info_tests.rs`, `assert_cmd` + `tempfile` +
  `predicates`), mirroring `tests/cli_graph_tests.rs`:
  - a vault with one `lint = true` mount (external root) + one `lint = false`
    mount + one schema: text output shows both mounts with correct `as`,
    resolved absolute paths, lint flags, and per-mount doc counts; the schema
    shows its expanded path; document totals reconcile (primary + mounted =
    total).
  - a mount whose `path` does not exist → `✗ missing` marker, **exit 0**.
  - a schema whose `to` folder does not exist → `✗ missing` marker, **exit 0**.
  - `--format json` → valid JSON (parse with `serde_json`), correct envelope
    keys, `config`/`as` null-cases, `conflicts` array.
  - a namespace conflict (a mount file colliding with a primary file) → listed
    under `conflicts`, **exit 0**.
  - a schema `to` referencing an unset env var → exit `2`, error on stderr.
  - `--root` honored; no `.downlint.toml` → `config` shows `(defaults)`, exit 0.

## 9. Spec impact

- **`spec/downlint.md`** — new "Workspace info" section: the command, flags,
  the report sections, the text + JSON shapes, the exit-code table (0/2 only),
  and the descriptive-only (no exit-1) note. Add `info` to the CLI command
  table and the workflows list.
- **`spec/linting.md`** — no changes (no new diagnostics, no resolution change).

## 10. Risks / open questions

1. **`info` vs `doctor` split.** This RFC ships the descriptive `info` only. The
   `✗ missing` markers are informational (exit 0). If a CI gate is wanted later,
   `doctor` reuses the same report and maps "any missing / any conflict /
   `uri_error`" to exit 1. Flagged as the likely follow-up.
2. **Cost.** `from_workspace` parses every document (no link resolution). Same
   order as `check` minus resolution; acceptable for an interactive command. A
   future `--server` integration could reuse a warm index (out of scope).
3. **Mount `path` does not expand `${VAR}`.** Pre-existing: the mount expander
   (`src/utils/workspace.rs:294`) handles `~` and relative-to-root only, while
   the schema expander (`src/resolution/uri.rs:211`) also expands env vars.
   `info` reports each as its respective expander produces it; it does not
   reconcile the two. Noted so the asymmetry is visible, not silently "fixed".
4. **JSON key order.** `serde_json` (no `preserve_order`) sorts object keys
   alphabetically, so the pretty output's key order differs from the text
   report's section order. This matches existing `resolve`/`graph` JSON and is
   fine for machine consumers; noted for anyone diffing by eye.
