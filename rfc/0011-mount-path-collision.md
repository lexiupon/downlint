# RFC 0011 — Mount Conflict: Fine-Grained Path Collision

**Status**: Proposal (open)
**Date**: 2026-09-21
**Follows**: RFC 0010 (Mounts and Schemes) — refines `RES-08` conflict detection
**Config impact**: none (detection-only; no config keys added or removed)

---

## 1. Summary

RFC 0010 introduced `[[mounts]]` with two **namespace-level** structural conflicts, both
detected on the **top-level entry** (the first path component):

- *Prefix conflict* — the mount's `prefix` matches a primary top-level entry.
- *Folder conflict* — a top-level folder in the mount matches a primary top-level entry.

This is **coarse**: it errors as soon as a top-level *name* is shared, even when the
individual files would coexist fine. A mount with `notes/b.md` and a primary with
`notes/a.md` conflict on `notes` — but `b.md` and `a.md` never collide. Likewise
`prefix = "/kb"` + a primary `kb/` folder errors even when every file on both sides is
distinct.

This RFC replaces the top-level-entry check with a **fine-grained namespace-path collision**
check. A `mount/conflict` fires only when:

1. **Same-path file collision** — a mount file's namespace path equals a primary file's
   namespace path; **or**
2. **File/folder name collision** — a mount file and a primary folder (or a mount folder and
   a primary file) share a name (stem, ignoring a trailing `.md`) at the same namespace
   location.

Sharing a top-level *name* is no longer a conflict by itself. `link/ambiguous` (a per-link
same-stem / same-title clash) is **not** a mount conflict and is left unchanged.

---

## 2. Motivation

### 2.1 The coarse check produces false positives

Today, merely sharing a top-level name errors out:

- `[[mounts]] root = "~/kb" prefix = "/kb"` + a primary `kb/` folder → `mount/conflict`
  (prefix), even if the mount's files and the primary's `kb/` files are all distinct.
- A mount with `notes/b.md` + a primary with `notes/a.md` → `mount/conflict` (folder) on
  `notes`, even though `b.md` and `a.md` don't collide.

The user's mental model is *"merge the mount into the namespace; only error if something
actually collides."* The coarse check doesn't match that, and it forces users to pick a
non-colliding prefix (or a non-colliding top-level folder) purely to avoid a false error.

### 2.2 The intuitive model

A mount (with or without a `prefix`) **merges** its files into the namespace at their
namespace paths. A collision is only a problem when two entries actually occupy the same
path, or when a file and a folder share a name (which the filesystem cannot represent and
which makes link resolution ambiguous). Everything else coexists.

---

## 3. Goals and Non-Goals

**Goals**
- Detect a `mount/conflict` only on an actual namespace-path collision.
- Allow a mount to merge into an existing folder (e.g. `kb/`) when no file collides.
- Keep the check consistent between the `prefix` and no-`prefix` cases.
- Protect against the file-vs-folder same-name edge case with an explicit error.

**Non-Goals**
- Changing how `link/ambiguous` works (it stays a per-link, recoverable diagnostic).
- Changing mount indexing, reachability, linting, or attribution (all unchanged from
  RFC 0010).
- Introducing any new config keys.

---

## 4. The Change

### 4.1 Namespace paths (unchanged from RFC 0010)

- A mount file's **namespace path** is `prefix/rel` when a `prefix` is set, else `rel`
  (mount-root-relative).
- A primary file's namespace path is its workspace-relative path.

### 4.2 Detection algorithm

For each mount, build four sets:

- **Mount files** — the set of mount-file namespace paths.
- **Mount folders** — the set of `(location, name)` folder entries implied by mount-file
  paths (every intermediate directory). A file `notes/kb/a.md` implies folders `(root,
  notes)` and `(notes, kb)`.
- **Primary files** — the set of primary-file namespace paths.
- **Primary folders** — the set of `(location, name)` folder entries implied by
  primary-file paths.

A file's **stem** is its filename without its extension (`Path::file_stem`):
`kb.md` → `kb`, `kb.markdown` → `kb`, `kb` → `kb`. (Matches the stem already used
for bare-stem wiki links.)

A `mount/conflict` is raised for the mount when **either** holds:

1. **Same-path file collision** — some mount-file namespace path is also a primary-file
   namespace path.
2. **File/folder name collision** — some mount file at location `L` with stem `S` has a
   matching primary folder `(L, S)`; **or** some mount folder `(L, S)` has a matching
   primary file at location `L` with stem `S`.

The check is per-mount and runs at **startup** (before linting), like today.

### 4.3 Worked examples

| Mount files (namespace paths) | Primary files | Conflict? | Why |
|---|---|---|---|
| `notes/b.md` | `notes/a.md` | **No** | distinct files under a shared folder |
| `notes/a.md` | `notes/a.md` | **Yes** | same-path file collision |
| `kb.md` | `kb/a.md` | **Yes** | file `kb.md` (stem `kb`) vs folder `kb/` at root |
| `kb/b.md` (prefix `/kb`) | `kb/a.md` | **No** | distinct files under `kb/` |
| `kb/b.md` (prefix `/kb`) | `kb/b.md` | **Yes** | same-path file collision |
| `kb.md` | `kb.md` | **Yes** | same-path file collision (both files, root) |

### 4.4 Suspend behavior

On a conflict, the **conflicting mount file(s)** are *targets only* (not linted), mirroring
RFC 0010's folder-conflict suspend. This keeps the mount's own links from producing a flood
of per-link `link/ambiguous` while the config is wrong. The `mount/conflict` error is still
reported so the user fixes the config.

Note: for a **same-path** collision the two files genuinely occupy one namespace path, so a
link to that path may *still* surface as `link/ambiguous` (that is a per-link concern, not a
mount error). The `mount/conflict` error is the primary, actionable signal. The exact suspend
scope (conflicting file only vs. whole mount) is an open question — see §9.

---

## 5. Diagnostics

- **`mount/conflict`** (Error) — one per colliding path, attributed to the mount. The
  `detail` names the specific collision, e.g.:
  - `file 'kb/a.md' collides with a primary file at the same path`
  - `file 'kb.md' collides with primary folder 'kb/'`
- `MountConflictKind::{Prefix, Folder}` are replaced by a single
  `MountConflictKind::PathCollision` (the two old labels are retired).
- `link/ambiguous` is unchanged and is **not** a mount conflict.

---

## 6. Spec Impact (clauses)

- **RES-08** (`spec/linting.md` §3.8): rewrite the "Structural conflicts" paragraph.
  Replace the prefix/folder top-level-entry rules with the fine-grained path-collision rules
  (§4.2). Update the `mount/conflict` row in the diagnostics table (§4) and clarify in the
  `link/ambiguous` note that it is not a mount conflict.
- **Amendment History** (`spec/linting.md` §8): add a row for RFC 0011.

---

## 7. Migration

No config change. Existing `[[mounts]]` configs are unaffected syntactically. Behavior
changes for configs that today trip the coarse check but have **no** actual collision: they
stop erroring (the mount merges). Configs with a real collision still error, now with a more
precise message. This is a behavior change, not a wire-format one.

---

## 8. Alternatives Considered

- **Keep the coarse top-level-entry check** — rejected: false positives, unintuitive, forces
  users to rename folders or pick non-colliding prefixes.
- **Per-link only (no structural error)** — rejected: a same-path collision is a config
  problem worth failing fast on; deferring it to `link/ambiguous` floods per-link errors and
  buries the real cause.
- **Error on any shared top-level folder** (current) — rejected: equivalent to the coarse
  check.

---

## 9. Open Questions

1. **Suspend scope** — on a conflict, suspend only the conflicting mount file(s) (proposal),
   the whole mount, or nothing (rely on `link/ambiguous`)? Proposal: only the conflicting
   mount file(s), targets only.
2. **File/folder collision depth** — check file-vs-folder collisions at *every* location
   (proposal), or only the top level? Proposal: every location — `notes/kb.md` vs
   `notes/kb/` is just as messy as a root-level one.
3. **Multiplicity** — one diagnostic per colliding path (proposal) or one per mount?
   Proposal: one per colliding path (more actionable).

---

## 10. Test Plan (conformance)

- `mount_distinct_files_under_shared_folder_no_conflict` — `notes/b.md` + `notes/a.md` → no
  conflict.
- `mount_same_path_file_conflict` — `notes/a.md` + `notes/a.md` → conflict.
- `mount_file_vs_folder_name_conflict` — `kb.md` + `kb/a.md` → conflict.
- `mount_prefix_distinct_files_no_conflict` — prefix `/kb`, distinct files → no conflict.
- `mount_prefix_same_path_conflict` — prefix `/kb`, same path → conflict.
- `mount_conflict_suspends_lint_of_conflicting_file` — conflicting mount file is a target
  only.
- Update the existing `mount_prefix_conflict_suspends_prefix` and
  `mount_folder_conflict_suspends_lint` tests to the new semantics.

---

## 11. Decision Requested

Approve the fine-grained path-collision detection (§4) and the single `PathCollision`
conflict kind (§5), with the suspend behavior in §4.4 and the open questions in §9 resolved
as proposed.
