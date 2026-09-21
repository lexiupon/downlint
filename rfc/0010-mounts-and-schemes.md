# RFC 0010 — Mounts and Schemes

**Status**: Proposal (open)
**Date**: 2026-09-21
**Refactors**: `RES-07` (External URI Mapping) → **Schemes**; `RES-08` (Cross-Folder Resolution) → **Mounts**
**Config impact**: `core.extra_folders` → `[[mounts]]`; `[uri]` / `[[uri.mappings]]` → `[[schemas]]` (caching removed)

---

## 1. Summary

Links that point outside the primary root are handled by two redesigned features that share
one config language (`root` + `prefix`):

- **Mounts** — mount an external **local** markdown folder into the namespace. Its documents
  are **indexed** and **co-equal** with primary documents (resolvable by exact path, bare name,
  or title; a same-name clash is `link/ambiguous`). Optionally lint the mounted docs' own links.
  *(Refines `RES-08`.)*
- **Schemes** — map an external **URI scheme** to a local folder. Resolved by **rewrite + stat +
  verify**; **not indexed** (a link must carry the full scheme prefix). **No caching** — `s3://`
  and the `warm_cmd` machinery are dropped. *(Refines `RES-07`.)*

This is a **refactor, not a new capability**: the same jobs, a cleaner config, honest conflict
handling, and the removal of the sync/caching complexity.

---

## 2. Motivation

### 2.1 Extra folders hide ambiguity

`RES-08` today is a **silent fallback**: primary is searched first, extra folders only on a miss,
so a name present in both resolves to primary with no signal. A linter should *surface* that
ambiguity, not hide it.

### 2.2 URI mapping carries machinery it doesn't need

`RES-07` ships a `warm_cmd` / `warm_required` / `warm_timeout` sync-and-cache layer plus three
`uri/sync-*` diagnostics. That machinery exists **only** for schemes with no local presence
(s3). iCloud and OneDrive have a local presence via the OS — the file is on disk, possibly
evicted — so downlint never needs to download anything. Supporting s3 was scope creep that is
hard to get right and has no current user.

### 2.3 No shared config language

`core.extra_folders` (a flat list) and `[[uri.mappings]]` (a table with caching fields) are two
unrelated shapes for the same job ("resolve outside the primary root").

### 2.4 A real incident

A mapping `prefix = "~/icloud/assets/"` silently never matched — a `~/…` target has no scheme, so
`RES-07`'s scheme gate rejects it before any prefix is consulted. The working form required
rewriting links to `file:///…`. A clean, explicit config makes this class of mistake impossible.

---

## 3. Goals and Non-Goals

**Goals**

- Mounts: indexed external folders, co-equal with primary, `prefix` alias, optional internal
  linting.
- Schemes: rewrite + stat + verify; drop caching and s3.
- One config language: both features are `root` + `prefix`.

**Non-Goals**

- No `s3://`, no local caching / `warm_cmd`.
- No fuzzy / tail matching (exact only; a separate future concern).
- No change to how primary-folder documents are discovered or diagnosed.
- Not a re-unification into a single abstracted table (see §10, Alternative B).

---

## 4. Mounts

### 4.1 Concept

A **mount** brings an external local folder into the resolution namespace. Its documents are
**indexed** and treated **co-equal** with primary documents.

### 4.2 Config

```toml
[[mounts]]
root   = "~/another_project/kb"
prefix = "/another_project_kb"  # optional
lint   = false                  # optional, default false
```

- `root` (required): the folder to mount — the root where the files are found. Expanded as today
  (`~`, `$VAR`, config-relative).
- `prefix` (optional): an exact-path alias. A mounted doc at `root/path/to/file.md` is also
  reachable as `prefix/path/to/file.md`. Serves as the disambiguator for same-named files.
  **Default (no `prefix`):** the mount's subfolders are treated as if in the current folder — its
  documents are reachable by their path relative to `root`, by bare stem, and by title, co-equal
  with primary documents.
- `lint` (optional, default `false`): also lint links *within* the mounted documents (§4.5).

### 4.3 Resolution — co-equal, not fallback

Mounted documents are added to the resolution index **alongside** primary documents. A target is
matched against the **combined** set:

- exactly one candidate → resolved;
- more than one (e.g. the same stem/title in primary and a mount) → **`link/ambiguous`**;
- none → `link/broken`.

**How each target form reaches a mount** (extends `RES-02`/`RES-03`):

| Target form | Resolves against | Reaches a mount? |
|---|---|---|
| relative `path/to/file.md` | the containing document's directory | only if the *linking* doc is itself in that mount |
| workspace-absolute `/kb/…` | workspace root, **or** a mount whose `prefix` matches | **yes** — via the prefix |
| `[[Title]]` / bare stem | the whole namespace (primary + all mounts) | **yes** — co-equal |

Consequences:

- **Relative paths do not cross mounts.** A relative link in a primary doc resolves within the
  primary; `../kb/foo.md` does not reach a mount. To reach a mount by path, use its `prefix`
  (workspace-absolute).
- **The `prefix` is a virtual directory at the workspace root.** A workspace-absolute target
  starting with a mount `prefix` maps to that mount; the prefix must therefore be a *free* name
  (a collision with an existing primary path is a `mount/conflict`, §4.6).
- **Global title/stem can be ambiguous.** A `[[Title]]` or bare stem present in both primary and a
  mount is `link/ambiguous` (the intended co-equal behavior). The explicit `prefix` path
  (`[[/another_project_kb/path/to/note]]`) is the unambiguous form.

**Behavior change from `RES-08`:** today a same-name bare link resolves to primary *silently*
(fallback). Under this RFC it is **`link/ambiguous`**. The `prefix` path is the unambiguous form.

### 4.4 Diagnostics

- Per-link same-*file* clash → `link/ambiguous` (reused).
- Namespace-level conflict (prefix or folder) → `mount/conflict` (Error, blocking) — §4.6.

### 4.5 Linting mounted documents

- **Default (`lint = false`)**: mounted docs are **targets only** — links *to* them are checked;
  links *within* them are not. (Matches `RES-08`: "documents in extra folders are not diagnosed
  from the primary session.")
- **`lint = true`**: mounted docs are also **sources** — their internal links are resolved against
  the full namespace (primary + all mounts) and diagnosed.
- **Attribution:** every diagnostic emitted from a mounted doc is labeled with the mount it came
  from (its `prefix`, or `root` when there is no prefix), so a `link/broken` in a mounted doc is
  clearly distinguished from one in a primary doc.

### 4.6 Structural conflicts (error, blocking)

Two **namespace-level** conflicts are **errors** — not per-link warnings:

- **`mount/conflict` (prefix)** — the mount's `prefix` matches a path that already exists in the
  primary project. While unresolved, the `prefix` is **not applied** (the mount's docs are not
  reachable via it).
- **`mount/conflict` (folder)** — a top-level folder in the mount has the same name as a top-level
  folder in the primary project. (Top-level only; a deeper same-*stem* in *different* folders is
  per-link `link/ambiguous`.) While unresolved, files under the conflicting folder are **not
  linted** (their links are not diagnosed).

This is distinct from a **per-link** same-*file* clash, which remains `link/ambiguous` (recoverable,
per-link). Rationale: a folder/prefix collision means the *namespace* is misconfigured (fix the
config); a file clash means one *link* is ambiguous (fix the link). Suspending the conflicting
namespace avoids a flood of per-link `link/ambiguous` while the config is wrong.

---

## 5. Schemes

### 5.1 Concept

A **scheme** maps an external URI scheme to a local folder. Resolution is **rewrite + stat +
verify**. Schemes are **not indexed** — a link must carry the full scheme prefix.

### 5.2 Config

```toml
[[schemas]]
prefix = "icloud://assets/"
root   = "~/icloud/assets"
auto_verify = true              # optional, default true
# verify_cmd = [...]            # optional
```

- `prefix` (required): the scheme prefix, e.g. `icloud://assets/`.
- `root` (required): the local folder the prefix maps to.
- `auto_verify` (optional, default `true`): run the built-in evicted-placeholder heuristics — the
  **vendor-specific markers only** (iCloud `.icloud` sibling, OneDrive `._<name>` resource fork).
  Collapsed from the old four modes to a bool. The old *generic* zero-byte+recent-mtime heuristic is
  dropped (it had a false-positive window for genuinely empty cloud files); the vendor markers are
  reliable, so default-on is safe.
- `verify_cmd` (optional): a custom verification command (advanced escape hatch).

### 5.3 Resolution

1. Target must carry the scheme prefix (in full).
2. Strip the prefix, percent-decode the remainder, join to `root`.
3. Stat the result. Present → resolved; absent → `link/broken`.
4. If `auto_verify`, run the placeholder heuristics; a detected placeholder (an evicted cloud
   file) is treated as **absent** → `link/broken`, with a hint that it may be an un-hydrated cloud
   file.

A scheme target matching no `[[schemas]]` entry → `link/broken` + `uri/no-mapping` hint
(unchanged).

*Evicted placeholder:* iCloud/OneDrive can store only a stub locally (bytes in the cloud). The path
exists but the content is absent until the OS hydrates it on first open. `auto_verify` detects the
stub via the vendor-specific markers (iCloud's `.icloud` sibling, OneDrive's `._<name>` resource
fork) so downlint can tell *genuinely missing* from *present but evicted*.

### 5.4 What is dropped

- `warm_cmd`, `warm_required`, `warm_timeout` — **no caching.**
- `uri/sync-skipped`, `uri/sync-failed`, `uri/batch-clamped` — **tombstoned.**
- `s3://` support — no local presence, no current user; revisit when a case exists.
- `auto_verify` mode granularity (`on`/`off`/`onedrive-only`/`icloud-only`) → **bool.**
- The *generic* zero-byte+recent-mtime placeholder heuristic — false-positive risk; vendor-specific
  markers only.

### 5.5 Diagnostics (remaining)

- `uri/no-mapping` (Info) — scheme target, no matching scheme.
- `link/broken` (severity per the rule) — mapped but absent, or an evicted placeholder.

---

## 6. Unified Config

```toml
[core]
file_extensions = ["md", "markdown"]
ignore = []
# (heading_ids, text_sync, title_from_heading unchanged)

# Mounts: external LOCAL markdown, indexed, co-equal with primary.
[[mounts]]
root   = "~/another_project/kb"
prefix = "/another_project_kb"  # optional
lint   = false                  # optional

# Schemes: external URIs -> local folder, rewrite + stat + verify.
[[schemas]]
prefix = "icloud://assets/"
root   = "~/icloud/assets"
auto_verify = true
```

Both are `root` + `prefix`; the table name is the only explicit distinction (indexed vs. not),
and the prefix form (path vs. scheme) is self-evident.

---

## 7. Resolution Algorithm

```
1. scheme?            -> Schemes: rewrite + stat + verify          (RES-07)
2. anchor-only?       -> heading resolution
3. folder-link?       -> folder resolution
4. index match        -> RES-03 over (primary + mounted) docs      (co-equal)
5. obsidian prefix    -> RES-05 (opt-in)
```

The `RES-08` **fallback step is gone**: mounted docs live in the step-4 index, co-equal with
primary. Schemes keep their step-1 position (scheme-gated, pre-index).

---

## 8. Migration

| Old | New | Behavior change |
|---|---|---|
| `core.extra_folders = ["A"]` | `[[mounts]] root = "A"` | same-name bare links: silent primary-wins → `link/ambiguous` |
| `[[uri.mappings]] prefix="S://" root="R" warm_cmd=…` | `[[schemas]] prefix="S://" root="R"` | caching fields dropped (no-op for iCloud/OneDrive) |
| `[[uri.mappings]] prefix="~/…/"` (dead) | delete | was never valid |
| `[uri].auto_verify = "<mode>"` | `[[schemas]] auto_verify = true` | modes → bool |

Old keys are **removed** (no backward-compatibility alias); the table is conversion guidance for
existing configs.

---

## 9. Rollout

- **Phase 1 — Mounts.** Replace `core.extra_folders` with `[[mounts]]`. Co-equal semantics +
  `link/ambiguous` on clash. Re-scope `RES-08`.
- **Phase 2 — Schemes.** Replace `[[uri.mappings]]` / `[uri]` with `[[schemas]]`; drop caching +
  `uri/sync-*`. Re-scope `RES-07`.

Phases 1 and 2 are independent and can ship separately. Old keys are removed (no backward-compat
alias).

---

## 10. Alternatives Considered

- **A. Status quo.** Rejected: silent-fallback ambiguity + caching complexity + two config shapes
  all remain.
- **B. Single `[[roots]]` table (the earlier, dropped proposal).** Rejected: it unified indexed
  documents and external assets under one 4-axis abstraction, blurring a real distinction, and
  retained the caching machinery. Two clearly-named tables are clearer.
- **C. This RFC.** Two features, one config language, honest conflicts, no caching.

---

## 11. Open Questions

1. ~~Mount `prefix` optional vs. required~~ — **resolved: optional** (default: subfolders treated
   as in the current folder).
2. ~~`auto_verify` per-schema vs. global~~ — **resolved: per-schema, default true, vendor-specific
   markers only** (generic heuristic dropped).
3. ~~`mount/conflict` (folder) scope~~ — **resolved: top-level only.** Deeper same-*stem* in
   different folders → per-link `link/ambiguous`.
4. `lint = true` scope — resolve mounted docs' links against the full namespace (proposed) vs.
   only their own mount.
5. Evicted placeholder — `link/broken` (proposed) vs. a distinct Info diagnostic.

---

## 12. Spec Impact (clauses)

Per `§0.3`, clause IDs are permanent; this RFC **re-scopes** two (recorded below and in the spec's
Amendment History) and tombstones three diagnostics.

- **`RES-08` → Mounts**: re-scoped from "cross-folder fallback" to "co-equal indexed mounts"
  (§4). Fallback semantics removed.
- **`RES-07` → Schemes**: re-scoped to drop caching; `warm_*` removed; s3 out of scope (§5).
- **§1 Definitions**: `extra folder` → `mount`; add `mounted document`, `scheme`.
- **§4 Diagnostics**: tombstone `uri/sync-skipped`, `uri/sync-failed`, `uri/batch-clamped`; keep
  `uri/no-mapping`; add `mount/conflict` (Error).
- **§5 Configuration**: `core.extra_folders` → `[[mounts]]`; `[uri]` → `[[schemas]]`.
- **§8 Amendment History**: record this refactor and the re-scoping.

---

## 13. Test Plan (conformance)

Every changed clause carries a `Tests:` line.

- **Mounts**: co-equal resolution; same-name primary+mount → `link/ambiguous`; `prefix` path
  disambiguates; bare/title/relative resolution of a mounted doc; `lint` on (internal links
  diagnosed) vs. off.
- **Schemes**: rewrite + stat; absent → `link/broken`; evicted placeholder → `link/broken` (+
  hint); no-mapping → `uri/no-mapping`; caching fields absent from the config schema.
- **Behavior deltas**: fallback→`link/ambiguous` (mounts); caching dropped (schemes); old config
  keys (`core.extra_folders`, `[[uri.mappings]]`, `[uri]`) are rejected/ignored.

---

## 14. Decision Requested

Approve the two-feature refactor and the `root` + `prefix` config. Phase 1 (mounts) and Phase 2
(schemes) can each be accepted and shipped independently.
