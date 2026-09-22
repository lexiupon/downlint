# RFC 0014 — Rename `mounts` / `schemas` Config Fields for Clarity

**Status**: Proposal (open)
**Date**: 2026-09-23
**Follows**: RFC 0010 (Mounts and Schemes), RFC 0011 (Mount Path Collision)
**Config impact**: **breaking** — four config keys renamed (see §3)
**Guiding principle**: *a config file should be self-evident without the spec.*

---

## 1. Summary

`[[mounts]]` and `[[schemas]]` both use the keys `root` and `prefix`, but the two
features use them with **different roles and kinds**, which is a recurring source of
confusion:

| | `root` | `prefix` |
|---|---|---|
| **mount** | the disk folder to index (physical) | a workspace path the mount appears as (virtual) |
| **schema** | the disk folder the URI resolves to (physical) | a URI the link carries (external) |

`root` is a *disk folder* in both, but it is the **source** of a mount (files live
there) and the **destination** of a schema (the URI rewrites to it). `prefix` is a
*workspace path* in a mount but a *URI* in a schema. Reading either section requires
remembering which feature you are in.

This RFC renames the four keys so each is self-evident from its name, and so the two
sections no longer share a vocabulary:

| Feature | old key | new key | meaning |
|---|---|---|---|
| mount | `root` | **`path`** | the disk folder to index |
| mount | `prefix` | **`as`** | the virtual path the mount appears as (optional) |
| schema | `prefix` | **`uri`** | the URI prefix a link must carry |
| schema | `root` | **`to`** | the disk folder the URI resolves to |

After the rename each section reads as a sentence:

```toml
[[mounts]]
path = "~/kb"               # mount the disk folder ~/kb
as   = "/kb"                #   and expose it as /kb in the workspace

[[schemas]]
uri  = "onedrive://xyz/"    # a link carrying onedrive://xyz/ …
to   = "downloads/onedrive" #   resolves to downloads/onedrive
```

The two destination keys use different prepositions on purpose: a mount *appears
as* a path (aliasing), a URI *resolves to* a folder (rewriting).

## 2. Motivation

- **Shared `root`/`prefix` with different roles.** The same two words appear in both
  sections but mean different things (source vs. destination; workspace path vs. URI).
  Every reader re-derives the meaning per section.
- **The mount's `prefix` is the most confusing.** It is a *path* that starts with `/`
  (so it looks like a disk location) yet it is the *virtual* address — the opposite of
  the schema's `prefix`, which is a URI.
- **Goal.** Rename so the keys are self-evident and the sections stop sharing a
  vocabulary. No behavior changes — this is a pure config-key rename.

## 3. The Change

### 3.1 Key mapping

| Feature | old | new |
|---|---|---|
| mount disk folder | `root` | `path` |
| mount virtual path | `prefix` | `as` |
| schema URI | `prefix` | `uri` |
| schema disk folder | `root` | `to` |

Unchanged: `lint` (mount), `auto_verify` and `verify_cmd` (schema).

### 3.2 Semantics (unchanged)

- **mount `path`** (was `root`): required. The folder to index; expanded by the
  workspace layer (`~`, `$VAR`, config-relative).
- **mount `as`** (was `prefix`): optional. A mounted doc at `path/<rel>` is
  additionally reachable as `as/<rel>`. Must start with `/` (a virtual directory at
  the workspace root). Serves as the unambiguous form when a name also exists in the
  primary.
- **schema `uri`** (was `prefix`): required. The URI prefix a link target must start
  with (e.g. `onedrive://xyz/`). Most-specific (longest) prefix wins.
- **schema `to`** (was `root`): required. The local folder the URI rewrites to;
  expanded by the resolver (`~`, `$VAR`, config-relative).

### 3.3 Rust field for `as`

`as` is a Rust reserved keyword, so the struct field is the raw identifier `r#as`.
Serde maps `r#as` to the TOML key `as` (verified: `r#as` deserializes from `as = …`
with no explicit rename). Code references the field as `mount.r#as`.

## 4. Breaking Change & Migration

Existing `.downlint.toml` files using the old keys will fail to parse
(`deny_unknown_fields` rejects the old names). Migration is a mechanical key rename:

| old line | new line |
|---|---|
| `root = "…"` (in `[[mounts]]`) | `path = "…"` |
| `prefix = "…"` (in `[[mounts]]`) | `as = "…"` |
| `prefix = "…"` (in `[[schemas]]`) | `uri = "…"` |
| `root = "…"` (in `[[schemas]]`) | `to = "…"` |

Pre-1.0, so a hard rename (no dual-key compatibility window) is acceptable. The
`downlint init` template is updated to emit the new keys.

## 5. Spec Amendments

- **spec/linting.md**
  - **RES-08 (Mounts)**: `root`→`path`, `prefix`→`as` (namespace path, reachability,
    attribution, collision wording).
  - **RES-07 (Schemes)**: `prefix`→`uri`, `root`→`to` (prefix match, root expansion,
    `uri/no-mapping` condition, validation rule).
  - **RES-03 / RES-04**: passing references to a mount's `prefix` / mount roots.
  - **§5.2 Validation** and **§5.3 Per-Key Clauses**: the `[[mounts]]` / `[[schemas]]`
    key references.
  - **§8 Amendment History**: new 0014 row (the 0010/0011 rows are historical and stay).
  - *(No glossary entries name these keys, so the glossary is unchanged.)*
- **spec/downlint.md** — the `[[schemas]]` prose reference ("map a scheme prefix" →
  "map a URI"). The `init` section does not reproduce the template, so no template keys
  change there.

## 6. Code Impact

- `src/config/mount.rs` — `Mount`/`PartialMount` fields, `finalize_mount` + error
  messages, doc comments, unit tests.
- `src/config/schema.rs` — `Schema`/`PartialSchema` fields, `finalize_schema` + error
  messages, doc comments, unit tests.
- `src/resolution/mod.rs`, `src/resolution/query.rs`, `src/resolution/uri.rs` — field
  accesses (`mount.root`→`mount.path`, `mount.prefix`→`mount.r#as`,
  `schema.root`→`schema.to`, `schema.prefix`→`schema.uri`).
- `src/cli/init.rs` — scaffolded template.
- Test fixtures (TOML strings and `PartialSchema` constructors) in
  `src/config/mod.rs`, `src/diagnostics/rules.rs`, `tests/cli_resolve_tests.rs`,
  `tests/schemas.rs`.

## 7. Out of Scope

- No change to resolution behavior, matching, diagnostics, or the `resolve` subcommand.
- No dual-key compatibility (old keys are removed, not deprecated in place).
- No rename of `lint`, `auto_verify`, or `verify_cmd`.
