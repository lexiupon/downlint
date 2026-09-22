# RFC 0013 — Obsidian-Compatible Path Resolution

**Status**: Proposal (open)
**Date**: 2026-09-23
**Follows**: RFC 0010 (Mounts and Schemes), RFC 0012 (Target Resolution Query)
**Config impact**: none (no config keys added or removed)
**Guiding principle**: *Obsidian-compatible, deterministic link resolution.*

---

## 1. Summary

downlint's path-link resolution diverges from Obsidian in three ways, all discovered
while reviewing the `resolve` subcommand (RFC 0012):

1. **Interpretation** — a bare wiki path `[[folder/note]]` (contains `/`, no `./`, no
   leading `/`) resolves against the *containing document's directory* today. Obsidian
   resolves it against the **vault root**.
2. **Extension** — an explicit path target must include the file extension to match a
   document today; the spec's promised "relative-path-without-extension equality" is dead
   code. Obsidian makes `.md` **optional** for markdown in path links.
3. **Normalization** — `.`/`..` in a path target are not normalized before comparison, so
   `[[../folder/note.md]]` fails document matching and falls through to the *attachment*
   fallback (misclassified), and its anchor is silently not validated.

This RFC amends RES-02 (Resolution Base) and RES-03 (Document Matching) so that **wiki
link** path resolution matches Obsidian's documented, deterministic rules, and adds
lexical `.`/`..` normalization. Markdown links keep standard (source-relative) semantics.
The `resolve` subcommand (RFC 0012) shares the matching code and reflects the new behavior
automatically.

---

## 2. Motivation

### 2.1 The divergences, with evidence

Obsidian's documented resolution for path links (Obsidian Help → Internal links; confirmed
by an Obsidian developer on the forum):

| Form | Resolves against |
|---|---|
| `[[./path/file]]` | the containing note's directory |
| `[[../path/file]]` | the containing note's directory |
| `[[/path/file]]` | the vault root |
| `[[path/file]]` (bare, has `/`) | **the vault root** |
| `.md` suffix | optional for markdown, **required** for non-markdown |

The developer's stated rationale (forum, "Absolute link path has higher precedence than
relative path"):

> "We want `[[A]]` to point to the same note across the vault. We don't want `[[A]]` to
> point to one note if it is contained in `Folder1/Note1.md` and to another note if it's
> contained in `Folder2/Note2.md`."

The interpretation is therefore a **strict, prefix-driven decision with no fallback**: it
does not try root-relative and then current-file-relative (or vice versa). That
determinism is the point, and the RFC encodes it as a strict if/else.

downlint today (verified against the 0.6.0 binary):

| Link in `notes/a.md` | downlint 0.6.0 | Obsidian |
|---|---|---|
| `[[./shared/b]]` | broken | resolves `notes/shared/b.md` |
| `[[./shared/b.md]]` | resolves (document) | resolves (document) |
| `[[../shared/b]]` | broken | resolves `shared/b.md` |
| `[[../shared/b.md]]` | resolves **as attachment** | resolves (document) |
| `[[../shared/b.md#section]]` | anchor **not validated** | anchor validated |
| `[[shared/b]]` | broken | resolves `shared/b.md` (root-relative) |
| `[[/shared/b]]` | resolves | resolves |

Three concrete defects fall out:

- **False positives** — extensionless and dot-relative path links that Obsidian resolves
  are reported `link/broken`.
- **Misclassification** — `[[../x.md]]` resolves as an *attachment* instead of a document.
- **False negative** — anchors on dot-relative links are silently not validated (a
  nonexistent section passes `check`).

### 2.2 The spec already promises two of the three fixes

**Extension-optional.** RES-03 states an explicit target matches on *"exact path equality
**or relative-path-without-extension equality**."* That second rule is dead code: it compares
the document's *relative* namespace path (`shared/b`) against the *absolute* resolved path
(`/vault/shared/b`), which never match for an absolute root. So the spec intends
extension-optional matching and the implementation never delivered it.

**Normalization.** LNK-02 describes a dot-relative path as *"explicit, anchored at the source
document directory, `./` normalized away after anchoring."* The implementation never
normalizes `.`/`..`, so the promised behavior is absent and `..` paths misbehave (fall through
to the attachment fallback).

The third fix — **interpretation** (bare wiki `path/file` → root-relative) — is the one the
spec currently gets *wrong*: RES-02 says all relative targets resolve against the containing
document's directory. This RFC corrects RES-02 to match Obsidian.

This RFC closes the two gaps and corrects the one contradiction, rather than working around
them.

---

## 3. Guiding principle

**Obsidian-compatible, deterministic.** For wiki links, a path target's resolution base is
decided solely by its prefix, with no fallback:

- `./…` or `../…` → the containing document's directory
- `/…` → the workspace root
- bare `…/…` (contains `/`, no prefix) → the workspace root

`.md` is optional for `.md` documents in path targets (the suffix specifically — a
non-`.md` file always requires its extension). `.` and `..` components are normalized
lexically before comparison.

Markdown links (`[x](…)`) are **not** changed in interpretation: they follow standard
markdown relative semantics (source-relative). Only the `.`/`..` normalization applies to
them (a pure correctness fix). *(Non-normative: if Obsidian's markdown-link behavior is
found to differ from standard markdown, that is a separate follow-up.)*

---

## 4. Design

### 4.1 Interpretation — RES-02 amendment (wiki-only)

Introduce a single base-selection rule, `resolution_base`, applied **consistently** to
document matching, folder links, and the attachment fallback:

```
fn resolution_base(target, source_dir, root, is_wiki) -> base:
    if target starts with "./" or "../":   return source_dir
    if target starts with "/":             return root
    if is_wiki and target contains "/":    return root      # bare wiki path → root-relative
    return source_dir                                       # markdown bare path / basename
```

- **Changed**: bare *wiki* `path/file` now resolves against the root (was source_dir).
- **Unchanged**: `./…`, `../…`, `/…`, and all markdown-link interpretation.

`resolve_explicit_path` becomes `base.join(percent_decode(target))` where `base` comes from
`resolution_base`; the existing `if starts_with('/')` special case is subsumed. All call
sites (document matching, `resolve_folder_link`, the RES-06 attachment fallback) pass
`is_wiki` and use the shared base, so a bare `[[folder/image.png]]` attachment and a bare
`[[folder/]]` folder link are root-relative, matching bare wiki document paths.

### 4.2 Extension-optional matching — RES-03 amendment (wiki-only)

For an explicit **wiki** path target, a document matches when its path equals the resolved
candidate, with the **`.md` suffix optional**:

- candidate `…/b` matches document `…/b.md` (`.md` omitted)
- candidate `…/b.md` matches document `…/b.md` (`.md` present)
- candidate `…/b.canvas` matches document `…/b.canvas` only (non-`.md`: extension required)

The document index holds the files named by `core.file_extensions` (default `md`), so the
rule is *"the `.md` suffix is optional"* — a `.md` document matches a candidate with or
without the `.md` suffix, while a document with any other extension requires the exact path.
Concretely, the comparison is on the path with a trailing `.md` stripped from both sides,
permitted only when the candidate has no extension or a `.md` extension. This replaces the
dead `rel_ns_match`/`abs_ns_match` pair with a single, correct rule and keeps the co-equal
primary+mounted namespace (RFC 0010) intact.

**Mount prefixes.** Namespace (mount-prefix) matching now applies to *all* root-relative
targets — both bare wiki `prefix/rel` and workspace-absolute `/prefix/rel` — not only the
absolute form as today. A bare target that matches both a primary document at
`root/prefix/rel` and a mounted document with namespace `prefix/rel` is co-equal
`link/ambiguous` (RFC 0010), as intended.

For an explicit **markdown** path target, matching stays exact (extension required) —
standard markdown.

The wiki title-slug fallback for path-like targets (so a mounted document can be reached by
an explicit path) is unchanged.

### 4.3 Normalization — `.`/`..` (wiki and markdown)

The resolved candidate path is normalized lexically (collapse `.` and resolve `..`
components) before any comparison. This is filesystem-free and safe: a candidate that
normalizes above the root simply matches no document (all documents are under the root) and
is reported broken. Effects:

- `[[../shared/b.md]]` from `notes/` → candidate `notes/../shared/b.md` → `shared/b.md` →
  matches the **document** (not the attachment fallback).
- Anchors on such links are now validated against the resolved document (fixes the silent
  false-negative).
- `[[./shared/b]]` → `shared/b` → matches `shared/b.md` (extension-optional, wiki).

### 4.4 Consequences

- **Attachment fallback (RES-06)** — markdown files that match a document now resolve as
  documents; the attachment fallback effectively applies to non-markdown files and to
  file-like paths that match no indexed document. RES-06 is amended to say so.
- **`resolve` subcommand (RFC 0012)** — shares `match_document_kinds`, so it reflects the
  new behavior automatically. Note the `--from` semantic change: `--from` sets the source
  directory, which now only affects `./…`/`../…` targets; a bare wiki `path/file` is
  root-relative regardless of `--from`. `resolve`'s spec examples are updated accordingly.
- **Anchors** — cross-document anchors on path links are validated once the target resolves
  to a document (existing anchor logic, now reachable for dot-relative links).

---

## 5. Spec amendments

| Location | Change |
|---|---|
| LNK-02 (Target Forms) | Clarify that the *relative path* form is source-relative for **markdown** links; a bare **wiki** `path/file` is a distinct form interpreted root-relative (see RES-02). `./`/`../` remain source-relative. |
| RES-02 (Resolution Base) | Replace "relative targets resolve against the containing document's directory" with the `resolution_base` rule (4.1). |
| RES-03 (Document Matching) | Replace the dead "relative-path-without-extension equality" with the extension-optional rule (4.2); note it is wiki-only. |
| RES-06 (Explicit File-Like Targets) | Note markdown resolves as a document when it matches; attachment fallback is for non-markdown / unmatched file-like paths. |
| RFC 0010 Reachability (mounts) | Split "Relative targets": markdown links and wiki `./`/`../` stay source-relative (do not cross into a mount); a bare wiki `path/file` is root-relative and reaches a mount via `prefix` like a workspace-absolute target. |
| Glossary | Update "explicit path" / "relative path" entries to reflect root-relative bare wiki paths. |
| downlint.md (`resolve`) | Update `--from` semantics and worked examples. |
| Amendment history | Add RFC 0013 row. |

---

## 6. Worked examples

Vault:

```
/README.md
/shared/b.md          (H1 "B", heading "## Section")
/notes/a.md
/notes/shared/c.md
```

All targets are wiki links. "from" is the containing document.

| Target | from | base | candidate (normalized) | result |
|---|---|---|---|---|
| `[[shared/b]]` | `notes/a.md` | root | `shared/b` → `shared/b.md` | resolved (document, extension-optional) |
| `[[shared/b]]` | `README.md` | root | `shared/b` → `shared/b.md` | resolved (document) |
| `[[notes/shared/c]]` | `notes/a.md` | root | `notes/shared/c` → `.md` | resolved (document) |
| `[[./shared/c]]` | `notes/a.md` | source | `notes/shared/c` → `.md` | resolved (document) |
| `[[../shared/b]]` | `notes/a.md` | source | `notes/../shared/b` → `shared/b.md` | resolved (document) |
| `[[../shared/b.md#section]]` | `notes/a.md` | source | `shared/b.md` | resolved (document), anchor validated |
| `[[../shared/b.md#nope]]` | `notes/a.md` | source | `shared/b.md` | resolved (document), `link/broken-anchor` |
| `[[/shared/b]]` | any | root | `shared/b` → `.md` | resolved (document) |
| `[[shared/missing]]` | any | root | `shared/missing` | broken |
| `[[../outside]]` | `README.md` | source | normalizes above root | broken |

Markdown links (unchanged interpretation, normalization applied):

| Target | from | result |
|---|---|---|
| `[x](shared/c.md)` | `notes/a.md` | resolved (source-relative, exact) |
| `[x](../shared/b.md)` | `notes/a.md` | resolved (document, normalized) |
| `[x](shared/b)` | `notes/a.md` | broken (markdown requires extension) |

`resolve` subcommand:

| Command | result |
|---|---|
| `resolve 'shared/b' --from notes/a.md` | resolved (root-relative; `--from` does not affect bare wiki targets) |
| `resolve '../shared/b' --from notes/a.md` | resolved (source-relative; `--from` matters) |
| `resolve './shared/c' --from notes/a.md` | resolved (source-relative) |

---

## 7. Impact on existing behavior and tests

- **Behavior changes** (intended, to match Obsidian):
  - Bare wiki `path/file` links now resolve root-relative (some previously-broken links
    resolve; some links that only "worked" under the old source-relative interpretation now
    break — they were false negatives against Obsidian).
  - Extensionless wiki path links resolve (previously broken).
  - Dot-relative wiki `.md` links resolve as documents (previously attachments); their
    anchors are now validated (new `link/broken-anchor` possible).
- **Unchanged**: markdown link interpretation, bare (no-`/`) stem/title matching, prefix
  matching (RES-05), URI schemes (RES-07), mount co-equality (RFC 0010).
- **Folder links** follow the shared `resolution_base`, so a bare wiki `[[folder/]]` becomes
  root-relative (a `./`/`../`/`/` folder link is unchanged).
- **Tests**: existing tests asserting the old source-relative bare-wiki interpretation or
  the attachment misclassification are updated to the new behavior; new tests cover the
  worked examples in §6. The `find_doc_matches`/`match_document_kinds` parity test (RFC
  0012) is extended for the new rules.

---

## 8. Out of scope

- **Markdown link interpretation** — stays source-relative (standard markdown).
- **Bare (no-`/`) stem resolution** — global stem/title matching (RES-03 non-explicit
  branch) is unchanged; the "root wins on duplicate stems" question is separate.
- **New diagnostics / config keys** — none introduced.
- **Obsidian markdown-link parity** — flagged as a possible follow-up only if evidence
  shows Obsidian diverges from standard markdown for `[x](…)`.

---

## 9. Open questions

1. *(Resolved by §4.1.)* The `resolution_base` root-relative rule for bare wiki paths applies
   consistently to document matching, folder links, and the attachment fallback — a bare
   `[[folder/]]` and a bare `[[folder/image.png]]` are root-relative, matching bare wiki
   document paths.
2. Is there any Obsidian setting that switches bare wiki paths to source-relative (which
   would argue for a config opt-in)? Current evidence says no — the behavior is fixed.
