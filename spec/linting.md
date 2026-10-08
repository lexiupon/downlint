# Downlint Linting Specification

**Status**: Living document — normative.
**Scope**: Defines *what* downlint lints and *how* link targets resolve: the link model,
resolution rules, diagnostics, and configuration semantics. Deliberately
implementation-independent: no data structures, algorithms, or module names.
**Companions**: `spec/downlint.md` (product behavior, higher level), `TODO.md` (open
issues). *How* things are built lives in the code. This document is self-contained:
the normative clauses below do not depend on any external document. Provenance for
where each rule originated is recorded in §8 (Amendment History).

---

## 0. Front Matter

### 0.1 Normative Language

The keywords "MUST", "MUST NOT", "SHOULD", and "MAY" in this document are to be interpreted
as described in RFC 2119.

### 0.2 How to Read This Document

- Every normative statement carries a stable **clause ID**:
  - `LNK-n` — link model (§2)
  - `RES-n` — resolution (§3)
  - `<category>/<rule>` semantic slug — diagnostics (§4), e.g. `link/broken`
  - configuration key name — configuration semantics (§5), e.g. `wiki.obsidian_prefix`
- Citations elsewhere (tests, RFCs, code comments, issues) MUST use clause IDs — never line
  numbers or section titles.
- Text marked *(non-normative)* — examples, notes — does not define behavior.
- **Conformance rule**: every normative clause carries a `Tests:` line citing the tests that
  pin its behavior. A clause with no test is not proven implemented. New tests added for a
  clause SHOULD be named after the clause ID (e.g. `res_05_prefix_match_single`).

### 0.3 Amendment and Citation Rules

- Clause IDs are permanent: never renumbered, reused, or silently re-scoped.
- Behavior changes enter only via a proposal (an RFC document, kept in `rfc/` while open).
  When implemented, its behavior is folded into this document **in the same change**, and
  the proposal is archived out of the working tree (git history is the record).
- A removed clause leaves a tombstone, e.g. `RES-07 — Removed by RFC 0012; see RES-09.`
- Diagnostic rule IDs are never reused; a retired rule ID leaves a tombstone. The one-time re-base from `DNLnnn` to semantic slugs (2026-09-21) is the single documented exception, recorded with its old→new mapping in §8.
- Every amendment is recorded in §8 (Amendment History).

---

## 1. Definitions

| Term | Definition |
|---|---|
| **workspace** | A root directory from which documents are discovered. The root is inferred by walking up from the target directory while the directory contains a `.downlint.toml` or `.git` marker; if no marker is found, the target directory itself is the root. `--root` overrides inference. |
| **primary folder** | A workspace folder under direct linting. In the LSP, the client's workspace folders; in the CLI, the checked directory. |
| **mount** | An additional folder declared via `[[mounts]]`, indexed co-equal with the primary project (RES-08). |
| **document** | A file whose extension (without dot) is in `core.file_extensions`, discovered in the primary project or a mount. |
| **attachment** | A non-document file that may be the target of a link (image, PDF, XLSX, …). No extension whitelist applies (RES-06). |
| **link** | One occurrence of a recognized link form (LNK-01) in a document. |
| **wiki link** | `[[target]]`, optionally with `\|alias` and/or `#heading`. |
| **embed** | `![[target]]` — a wiki link with an embedded-asset marker. |
| **markdown link** | `[text](target)`. **Image**: `![alt](target)`. |
| **reference link** | Full form `[text][label]`; collapsed form `[label][]`; shortcut form `[text]`. Resolved against link definitions `[label]: target`. |
| **target** | The raw destination text of a link, before any `#` split. For wiki links, the document part before the first unescaped `#`. |
| **anchor** | The part of a target after the first unescaped `#` (in-page or cross-document heading reference). |
| **document stem** | The file name of a document without its extension. |
| **heading ID** | The slug of a heading, per RES-01. |
| **explicit path** | A target that is slash-based or an unambiguous file name, per LNK-02. |
| **bare wiki path** | A wiki `path/file` target (contains `/`, no `./`/`../`/leading `/`), resolved against the workspace root (RFC 0013). |
| **explicit file-like target** | An explicit path, or a same-directory basename with an extension (e.g. `data.xlsx`). |
| **resolved** | A link with exactly one destination. **Ambiguous**: more than one. **Broken**: none. |
| **occurrence** | Identity of a single link instance. Two identical links at different ranges are distinct occurrences and are diagnosed independently. |
| **single-file mode** | An explicit single file on disk. Cross-file diagnostics are disabled (4.9). |
| **stdin document** | The synthetic `<stdin>.md` document created from `--stdin` input. It sits at the workspace root, is the sole lint source, and is resolved against the full workspace (RES-02). |
| **target query** | The `downlint resolve` subcommand: reports every destination a target resolves to, with the match reason (spec/downlint.md §3.1). A projection of the resolution rules — not a diagnostic. |

---

## 2. Link Model

### 2.1 LNK-01 — Recognized Link Forms

downlint MUST recognize exactly these link forms:

1. markdown inline link `[text](target)`
2. markdown image `![alt](target)`
3. wiki link `[[target]]`
4. wiki embed `![[target]]`
5. full reference link `[text][label]`
6. collapsed reference link `[label][]`
7. shortcut reference link `[text]` — recognized but **never diagnosed** (see `link/broken`)

Wiki-link parsing rules:

- The content between delimiters is split at the first **unescaped** `|` into target and
  optional display alias; the target is then split at the first **unescaped** `#` into
  optional document and optional heading.
- Markdown backslash escapes and wiki percent-encoding are decoded per component.
  `[[F\#]]` treats `#` as part of the document segment.
- `[[%5B%5Bdoc%5D%5D]]` decodes to a document name containing literal `[[doc]]`; it is not
  parsed as a nested wiki-link.
- Empty heading: `[[T#]]` records a zero-length heading and resolves to the document.
- Malformed or unterminated wiki-links are ignored by the link layer.

Markdown-link destination rules:

- The destination is the text between `(` and the matching unescaped `)`, trimmed.
- A trailing quoted title ("…", '…', or `(...)`) is stripped; pointy brackets (`<…>`) are
  removed. Otherwise the **whole** destination is kept, including spaces (e.g.
  cloud-storage filenames like `Messaging BOM - 21May26.pdf`).

Masking — never scanned for links or tags:

- YAML front matter (leading `---` block)
- fenced code blocks (``` / ~~~)
- inline code spans

*(non-normative: a markdown link whose URL contains encoded `[[...]]`, e.g.
`[text](%5B%5Binner%5D%5D)`, is NOT a wiki-link — only literal `[[...]]` triggers wiki
parsing.)*

Tests: `parser/scanner.rs` code-span masking tests; `obsidian_prefix_alias_form_resolves`,
`obsidian_prefix_embed_form_resolves`.

### 2.2 LNK-02 — Target Forms

A target is one of:

- **relative path** — a markdown path, resolved against the containing document's
  directory (`[x](next.md)` from `docs/guide/intro.md` → `docs/guide/next.md`)
- **workspace-absolute path** — leading `/`, resolved against the workspace root
  (`[x](/README.md)` → `<root>/README.md`)
- **dot-relative path** — `./doc`, `../doc`; explicit, anchored at the source document
  directory, `.`/`..` normalized away after anchoring
- **bare wiki path** — a wiki `path/file` (contains `/`, no `./`/`../`/leading `/`),
  resolved against the workspace root (RFC 0013)
- **in-page anchor** — `#section` (empty document part)
- **cross-document anchor** — `path#section`: the path resolves first, then the section
  inside the target document
- **URI-scheme target** — a target with a scheme (RES-07)
- **explicit file-like target** — document resolution first, then attachment (RES-06)

**Explicit-path predicate.** A target is an *explicit path* when any of:

- it starts with `/`, `./`, or `../`
- it contains `/` or `\`
- it contains a `.` whose trailing fragment is purely ASCII alphanumeric with no `-` or `_`
  adjacent to the dot

*(non-normative: `file.md` is explicit; `v2.5b-trust-region` is not — the trailing fragment
contains `-`; `.gitignore` and `foo.` are not.)*

Explicit paths bypass fuzzy document matching (RES-03) and prefix matching (RES-05).

Tests: `is_explicit_path` unit suite in `resolution/mod.rs`; `explicit_md_target_still_treated_as_explicit`.

### 2.3 LNK-03 — Excluded from Link Diagnostics

- Targets whose scheme is one of `http`, `https`, `ftp`, `ftps`, `mailto`, `tel`, `sms`,
  `irc`, `xmpp` (case-insensitive) MUST be skipped silently — no diagnostic of any kind.
  These are external web resources by design.
- Other URI-scheme targets are NOT suppressed: they go through RES-07 and may produce
  `link/broken` (and `uri/no-mapping`).
- Links inside masked regions (LNK-01) produce no diagnostics.
- Shortcut reference links with no matching definition are always suppressed (too noisy in
  prose).

Tests: `no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken` (web-scheme suppression
exercised transitively); `obsidian_prefix_off_no_hint_no_match`.

---

## 3. Resolution

### 3.1 RES-01 — Heading ID Generation

Heading IDs are GitHub-compatible slugs. Algorithm:

1. NFKC-normalize the heading text.
2. Unicode-lowercase.
3. Keep alphanumeric characters (ASCII and non-ASCII letters/digits), `-`, and `_`; drop all
   other punctuation. CJK and other non-Latin letters are preserved, never transliterated.
4. Replace each whitespace run with a single `-`; never produce consecutive `-`.
5. Strip trailing `-`.

*(non-normative: `"What's Up?"` → `whats-up`; `"中文 标题"` → `中文-标题`;
`"# 🚀 Launch"` → `launch`; `"# 42"` → `42`.)*

**Duplicate disambiguation.** When `core.heading_ids.enable` is true (default), within one
document the first heading with a given slug keeps the base slug; the Nth duplicate (N ≥ 2)
gets suffix `-N+1` (`setup`, `setup-1`, `setup-2`) in document order. When the flag is
false, all duplicates share the base slug, and an anchor to that slug resolves to **all**
duplicates as a multi-destination resolved reference (not `link/ambiguous`).

**Tolerant anchor matching.** Anchor lookup tries, in order: (1) exact slug match,
(2) tag match (lowercased ASCII), (3) *folded* slug match, where consecutive `-` are
collapsed to one. This lets a pasted em-dash heading (`2024--closest`) match slug
`2024-closest`.

Heading detection: a line whose left-trimmed start has 1–6 `#` followed by a space or
U+00A0, outside fenced code blocks.

Tests: `slug_preserves_cjk_and_strips_punctuation`, `slug_generation_with_accented_chars`,
`wiki_link_with_non_ascii_heading_anchor_resolves_correctly`,
`inline_anchor_tolerates_consecutive_dashes_in_em_dash_heading`,
`folded_collapses_consecutive_dashes`.

### 3.2 RES-02 — Resolution Base

The resolution base for a path target is decided by its prefix — a strict,
prefix-driven rule with no fallback (RFC 0013, Obsidian-compatible):

- `./…` and `../…` targets resolve against the **containing document's directory**.
- Workspace-absolute targets (`/…`) resolve against the **workspace root**.
- A **bare wiki** path target (`path/file`, contains `/`, no `./`/`../`/leading `/`)
  resolves against the **workspace root** (RFC 0013).
- A **markdown** bare path target and any basename target resolve against the
  **containing document's directory** (standard markdown, source-relative).

The `.` and `..` components of a resolved path are normalized lexically before
comparison (RFC 0013); a path that normalizes above the root matches nothing.

- The stdin document's directory is the **workspace root**: relative targets in
  `--stdin` input resolve against the root, and the full workspace (documents and
  attachments) is indexed for resolution, with workspace documents as targets only.
- Targets are percent-decoded (RFC 3986, UTF-8); malformed escapes are kept literal.
- Backslashes in link targets are treated as path separators (normalized to `/` internally).
- Stem and path comparisons are **ASCII case-insensitive** in memory, regardless of
  platform. *(non-normative: if both `Doc.md` and `doc.md` exist in one folder, a link to
  either matches both → `link/ambiguous`.)*
- Unicode normalization: precomposed and decomposed forms (e.g. `é` U+00E9 vs `e` + U+0301)
  are equivalent for matching (NFKC).

Tests: `inline_link_with_non_ascii_filename_resolves_correctly`,
`wiki_link_with_mojibake_does_not_match_correct_title`,
`percent_encoded_target_decodes_to_filesystem_path`,
`bare_wiki_path_resolves_root_relative`,
`bare_wiki_path_deterministic_regardless_of_source`,
`dot_relative_wiki_path_resolves_as_document`,
`markdown_bare_path_stays_source_relative`.

### 3.3 RES-03 — Document Matching

For a **non-explicit** wiki target, a document matches when ANY of:

1. document stem equals the target (ASCII case-insensitive)
2. document title slug equals the target slug (both slug-normalized per RES-01)
3. workspace-relative path without extension equals the target (ASCII case-insensitive)
4. full workspace-relative path (backslashes → `/`) equals the target (ASCII case-insensitive)

For an **explicit** target, matching is path-based only (RFC 0013): the resolved
candidate (per RES-02, `.`/`..` normalized) is compared against the document's
filesystem path, with the **`.md` suffix optional for wiki targets** (a `.md`
document matches a candidate with or without the `.md` suffix; a non-`.md` file
requires the exact path). A **root-relative** target (RES-02) additionally matches
the document's namespace path, which is how a mount `as` (a virtual directory
at the workspace root) is reached (RFC 0010). A path-like wiki target may
additionally fall back to a title-slug match (so a mounted document can be reached
by an explicit path). Markdown path targets match exactly (extension required).

**Title.** When `core.title_from_heading` is true (default), the document's first H1 is its
title; otherwise the file stem. Title-only wiki links (`[[|My Title]]`) resolve by title
slug.

**Co-equal namespace.** Primary and mounted documents are searched together in one
namespace (RES-08): there is no primary-first tiering. A link that matches more than one
document — across the primary project and the mounts — is ambiguous (`link/ambiguous`).
Matches are deduplicated by document path.

Tests: `wiki_link_with_non_ascii_title_resolves_correctly`,
`wiki_link_explicit_path_resolves_in_mount`,
`wiki_link_with_slash_in_target_resolves_via_title_slug_in_mount`,
`mount_same_name_primary_and_mount_is_ambiguous`.

### 3.4 RES-04 — Folder Link Resolution

**Detection.** A target is a folder link when the part before the first `#` ends with `/`.
Applies to both wiki (`[[dir/]]`) and markdown (`[x](dir/)`) links. A target naming a
directory **without** a trailing slash is NOT a folder link and goes through normal
document resolution first. If no document matches and local attachment fallback finds
an existing directory, it is `link/broken` with a hint to add `/` (RFC 0024), never an
attachment. Bare wiki identifiers retain note matching and do not acquire directory
lookup.

**Resolution.**

1. Strip trailing slashes; resolve the path per RES-02.
2. If a directory exists at the resolved path, the link resolves **to the directory
   itself**. There is no index-file lookup; an empty directory is a valid target.
3. For workspace-absolute targets only, mount paths are also tried (direct join, then
   matching the first path component against the mount path's own name, then via a mount's
   `as`). The first existing directory wins; folder resolution never produces ambiguity.
4. Otherwise the link is broken (`link/broken`) — including when the path exists but is a file.

**Folder link + heading** (`[[dir/#h]]`, `[x](dir/#h)`) is invalid: the link is broken
(`link/broken`).

Tests: `folder_link_to_existing_directory_resolves`,
`folder_link_to_missing_directory_is_broken`, `folder_link_to_file_not_directory_is_broken`,
`wiki_link_to_folder_resolves`, `folder_link_with_anchor_is_unresolved`,
`folder_link_in_mount_resolves`, `non_folder_link_to_directory_name_unchanged`.

### 3.5 RES-05 — Prefix Matching

**Gate.** Prefix matching runs only for **wiki** targets, only when `wiki.obsidian_prefix`
is true (default false), only for non-empty non-explicit targets, and only after exact,
title-slug, and relative-path matching (RES-03) have returned zero candidates.

**Matching.** The target is treated as a leading prefix of document stems (primary and
mounted), ASCII case-insensitive. There is no word-boundary requirement: any leading
prefix matches; a suffix does not.

**Multiplicity.**

- 0 candidates → broken (`link/broken`)
- 1 candidate → resolved to that document (a `#heading` on the target still gets heading
  lookup on the resolved document)
- ≥ 2 candidates → ambiguous (`link/ambiguous`), with related information for every candidate.
  Ambiguity is decided before heading lookup: even if only one candidate has the heading,
  `link/ambiguous` is emitted.

**Interactions.** Alias (`[[x|title]]`) is display-only. Embeds follow the same rules.
Explicit paths and folder links short-circuit before prefix matching.

**Discoverability hint.** Regardless of the flag value, when a **wiki** link is broken and
its target is a leading prefix of at least one document stem, the `link/broken` message gains a
hint line (see `link/broken`). Markdown links never receive the hint.

Tests: `obsidian_prefix_unique_resolves`, `obsidian_prefix_multi_match_emits_link_ambiguous`,
`obsidian_prefix_off_no_hint_no_match`, `obsidian_prefix_off_with_hint`,
`obsidian_prefix_hint_caps_at_five`, `obsidian_prefix_case_insensitive`,
`obsidian_prefix_does_not_match_suffix_only`, `obsidian_prefix_explicit_path_wins`,
`obsidian_prefix_alias_form_resolves`, `obsidian_prefix_embed_form_resolves`,
`obsidian_prefix_heading_anchor`, `obsidian_prefix_single_file_mode`,
`obsidian_prefix_does_not_resolve_folder_link`, `unique_prefix_resolves`,
`ambiguous_prefix_resolves_to_multiple`, `leading_prefix_only_does_not_match_suffix`.

### 3.6 RES-06 — Local File Targets

There is **no attachment extension or filename whitelist**. Resolution order for a
local target (RFC 0024):

1. Document resolution (RES-03), unchanged. Inline Markdown requires the written
   filename: `[report](report)` does not infer `report.md`.
2. If no document matched, inline links/images allow any non-empty target as an exact
   filesystem path, including bare `LICENSE` or `report`. Wiki links/embeds and the
   wiki-style query retain their *attachment candidate* predicate: starts with `/`,
   `./`, or `../`, contains `/` or `\`, or has a non-empty base and extension.
3. Resolve the exact filesystem path per RES-02, without extension inference or searching
   other directories. If `is_file()` succeeds, resolve as an **attachment**. File contents
   are not parsed; symlinks follow filesystem metadata.
4. If the path is a directory, report `link/broken` and a trailing-slash hint. All local
   attachment checks are file-only: `(./report)` and `(report)` are both invalid for a
   directory, while `(report/)` resolves as a directory via RES-04. This deliberately
   removes the legacy `exists()` acceptance of directories as attachments.
5. Otherwise the link is broken (`link/broken` at the reference's severity).

Bare wiki targets without a separator/extension (e.g. `report`) still never trigger
attachment fallback. External web schemes (LNK-03) are skipped, and schema targets retain
RES-07 routing. Local attachment fragments retain their existing unvalidated behavior.
Reference label usages resolve to definitions, not their URL's filesystem destination;
this change does not add definition-URL linting.

*(RFC 0013: a markdown file that matches an indexed document resolves as a **document**
(step 1), so the attachment fallback effectively applies to non-markdown files and to
file-like paths that match no indexed document.)*

Tests: `explicit_file_like_targets_resolve_as_attachments_without_config`,
`missing_explicit_file_like_targets_emit_broken_link_diagnostics`,
`explicit_markdown_document_paths_resolve_as_documents_and_headings`,
`parse_rejects_removed_attachment_extensions_key`, `license_and_extensionless_inline_targets_are_attachments`,
`inline_does_not_infer_markdown_extension_and_preserves_headings`,
`directory_targets_require_slashes_and_hint_only_for_exact_directories`,
`wiki_query_requires_directory_slashes_without_bare_file_fallback`.

### 3.7 RES-07 — Schemes (External URI Mapping)

A **scheme** (`[[schemas]]`) maps an external URI to a local folder.
Resolution is **rewrite + stat + verify** — no warming, no caching. Each schema has a
`uri` (required, e.g. `icloud://assets/`), a `to` (required), an optional
`auto_verify` (default `true`), and an optional `verify_cmd`. Schemes are **not indexed**
— a link must carry the full `uri`.

**Detection.** A target is routed to scheme resolution only when it has a
syntactic scheme (the part before the first `:` is non-empty and consists of
scheme characters `[A-Za-z0-9+-.]`) **and** at least one of: the scheme is a
web scheme (LNK-03), the target uses the `scheme://` form, or the target
matches a configured `[[schemas]]` prefix. Any other colon-bearing target
(e.g. a note title like `Team: Knowledge`) is **not** a URI — it falls
through to normal document resolution (RFC 0021). Web schemes (LNK-03) are
handled first; all other scheme targets enter scheme resolution.

**URI match.** Each `[[schemas]]` `uri` is normalized with a trailing slash. A target
matches when it equals the `uri` (slash-less) or starts with the normalized `uri`. The
**most specific** (longest) matching `uri` wins; only the first match is used.

**Path computation.** The resolved local path is `to + percent-decoded remainder`, where
the remainder is the target minus the `uri` (leading `/` trimmed). An empty remainder
resolves to the `to` folder itself.

**`to` expansion** (per schema, at startup):

1. `$VAR` / `${VAR}` are expanded; a missing variable is a startup configuration error.
2. Leading `~` expands to the user's home directory.
3. A still-relative path resolves against the directory containing the config file.

**Stat + verify.** The resolved path is stat'd; if it does not exist, the link is broken.
If it exists, placeholder detection runs:

- If `verify_cmd` is set, it runs **only** when `--allow-uri-sync` is passed — committing a
  config with arbitrary commands MUST NOT cause subprocess execution by itself. `{path}` is
  substituted with the resolved absolute file path; exit 0 → real file, non-zero/timeout/
  spawn failure → placeholder. Without `--allow-uri-sync`, `verify_cmd` is skipped (treated
  as inconclusive).
- Otherwise, when `auto_verify` is enabled (default), the built-in **vendor-specific**
  heuristics run: an iCloud `Mobile Documents` path with a sibling `.<name>.icloud`, or a
  OneDrive path with a zero-byte `._<name>` resource fork. An inconclusive heuristic
  passes. *(The old generic 0-byte + recent-mtime heuristic was dropped: it had a
  false-positive window for genuinely empty cloud files.)*

**Outcomes.**

- *Present* (exists, not a placeholder) → the link resolves as an attachment.
- *Missing or evicted placeholder* → `link/broken`.
- **No mapping** → `link/broken`, plus `uri/no-mapping` hint when at least one schema is
  configured and `--no-uri-hints` was not passed.
- **Anchor on a scheme target** (`scheme://…#anchor`) → always `link/broken-anchor`:
  anchors on external assets are not supported.

Tests: `present_file_resolves_as_attachment`, `missing_file_yields_broken_link`,
`no_mapping_emits_hint_when_schemas_configured`,
`no_mapping_emits_no_hint_when_schemas_unconfigured`,
`no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken`,
`trailing_slash_normalization_matches_both_forms`, `env_var_expansion_in_root`,
`relative_root_resolves_against_config_dir`, `more_specific_prefix_wins`,
`verify_cmd_pass_marks_present`, `verify_cmd_failure_marks_broken_even_when_file_exists`,
`verify_cmd_gated_by_allow_sync`, `icloud_evicted_placeholder_is_broken`,
`auto_verify_off_skips_heuristics`, `wiki_link_with_colon_in_title_resolves`,
`wiki_link_with_colon_in_stem_resolves`, `wiki_link_with_colon_alias_form_resolves`,
`wiki_link_with_colon_no_match_is_plain_broken`,
`markdown_link_with_colon_in_filename_resolves`.

### 3.8 RES-08 — Mounts (Co-Equal Resolution Roots)

A **mount** (`[[mounts]]`) is an additional folder indexed **co-equal** with the primary
project — not a fallback tier. Each mount has a `path` (required), an optional `as`
(workspace-absolute, e.g. `/kb`), and an optional `lint` flag (default `false`).

**Indexing.** Mounted documents are indexed alongside primary documents in one namespace.
A mounted document's *namespace path* is `as/rel` when `as` is set, else `rel`
(mount-path-relative). Primary documents' namespace path is their workspace-relative path.

**Reachability.**

- **Source-relative** targets — markdown links, and wiki `./…`/`../…` targets — resolve
  against the containing document's directory and do **not** cross into a mount.
- **Root-relative** targets — workspace-absolute (`/…`) and bare wiki `path/file` targets
  (RFC 0013) — resolve against the workspace root, or against a mount whose `as`
  matches (the `as` is a virtual directory at the workspace root).
- **Bare stem / title** wiki targets (`[[Title]]`) resolve across the whole namespace
  (primary + all mounts).

**Co-equal, not fallback.** A link matching documents in both the primary project and a
mount is ambiguous (`link/ambiguous`); there is no primary-wins tie-break. An `as`
disambiguates by giving the mounted document a distinct namespace path.

**Linting.** A mounted document is a *source* (its own links are diagnosed) only when its
mount has `lint = true`. Otherwise it is a *target* only. When `lint = true`, its links
resolve against the full namespace (primary + all mounts).

**Attribution.** A diagnostic whose source is a mounted document is labeled with the
mount's attribution (`as` when set, else `path`).

**Structural conflicts** (`mount/conflict`, Error). Detected at startup, per mount. A
conflict is a fine-grained **namespace-path collision** between the mount and the primary
(RFC 0011) — sharing a top-level name alone is *not* a conflict:

- *Same-path file collision*: a mount file's namespace path (`as/rel` when `as`
  is set, else `rel`) equals a primary file's namespace path.
- *File/folder name collision*: a mount file and a primary folder (or a mount folder and a
  primary file) share a name (stem, `Path::file_stem`) at the same namespace location.

While unresolved, the conflicting mount file(s) are **targets only** (not linted). A
per-link same-stem / same-title clash is *not* a mount conflict; it surfaces as
`link/ambiguous`.

Tests: `wiki_link_explicit_path_resolves_in_mount`,
`wiki_link_with_slash_in_target_resolves_via_title_slug_in_mount`,
`folder_link_in_mount_resolves`, `mount_same_name_primary_and_mount_is_ambiguous`,
`mount_prefix_disambiguates`, `mount_relative_markdown_link_does_not_cross`,
`mount_lint_true_lints_and_attributes_internal_links`,
`mount_lint_false_does_not_lint_internal_links`,
`mount_prefix_conflict_suspends_prefix`, `mount_folder_conflict_suspends_lint`.

### 3.9 RES-09 — Symlink Handling

- Symlinks are **followed** during file discovery; a symlinked `docs/` directory is
  treated as a regular folder.
- Symlinked directories directly in the workspace root are traversed even when
  `.gitignore` would exclude them.
- Symlink loops MUST NOT cause infinite recursion; broken symlinks are skipped, never a
  crash.
- Path identity is canonical: a document reached through a symlink is the same document as
  the same file reached directly.

Tests: `infer_root_prefers_target_parent_without_markers` (root inference); symlink
traversal exercised by workspace fixtures.

### 3.10 RES-10 — Exclusions

- `.gitignore` (plus `.ignore` and `.git/info/exclude`) is honored during traversal with
  standard gitignore semantics; a `.gitignore` in a subdirectory applies to that subtree.
- **Hidden files and directories are excluded by default** — dotfiles are not discovered
  as documents. `core.include_hidden = true` restores the include-everything walk. The
  hidden filter is a separate mechanism from gitignore rules and takes precedence over
  them: a `.gitignore` or `core.ignore` negation (`!…`) does **not** re-include a hidden
  file when `include_hidden` is `false` — the key is the only way back.
- `core.ignore` adds glob patterns, relative to the workspace root, **additive** to
  `.gitignore`: `**` matches zero or more path components, `*` matches within one
  component, `?` one character, `[abc]` a character class.
- Negation: a `core.ignore` pattern starting with `!` re-includes files excluded by
  earlier patterns (e.g. `["drafts/**", "!drafts/published/**"]`).
- An invalid pattern is skipped with a warning, never a crash.
- **Effect.** Excluded files are not documents: not parsed, not diagnosed, not resolvable
  as wiki-link targets. An explicit file-like link (RES-06) to an excluded-but-existing
  file still resolves as an attachment — attachment existence checks are not filtered by
  ignore patterns.

Tests: `ignore_project_overrides_user`, `ignore_negation_pattern_parses`.

---

## 4. Diagnostics

### 4.0 Code Table and Severity Rule

| Code | Description | Assigned severity | Condition (clause) |
|---|---|---|---|
| `link/ambiguous` | Ambiguous link | Error (wiki/embed) / Warning (markdown) | ≥ 2 destinations |
| `link/broken` | Broken link | Error (wiki/embed) / Warning (markdown) | no destination |
| `heading/nbsp` | Non-breaking space after heading | Warning | U+00A0 after heading marker |
| `link/broken-anchor` | Broken anchor | Warning (all link forms) | anchor did not resolve |
| `mount/conflict` | Mount conflict | Error | a mount file/folder collides with a primary file/folder at the same namespace path or location (RES-08) |
| `uri/no-mapping` | No scheme mapping | Info | unmapped scheme target, `[[schemas]]` configured |

**Severity rule.** Wiki links and embeds receive **Error** for `link/ambiguous` and `link/broken` on local
targets; markdown links, images, and reference links receive **Warning**. `link/broken-anchor` and
`heading/nbsp` are always Warning; `uri/no-mapping` is always Info. `--min-severity` filters
output only; it never changes assigned severity. The CLI default is `warning`; the LSP
always reports at `info`.

### 4.1 `link/ambiguous` — Ambiguous link

- **Condition**: a link target resolves to more than one destination — multiple matches
  across the co-equal namespace (primary + mounts, RES-08), or prefix matching with ≥ 2
  candidates (RES-05).
- **Severity**: Error (wiki/embed), Warning (markdown).
- **Message** (verbatim): `Ambiguous link: '{target}' resolves to multiple destinations`
- **Related information**: one entry per destination, carrying its path. No cap.
- **Range**: the target portion of the link (the editable destination text), falling back
  to the whole link.
- Duplicate *link definitions* with the same label do NOT produce `link/ambiguous`: the reference
  link resolves to all of them as a multi-destination resolved reference.

Tests: `obsidian_prefix_multi_match_emits_link_ambiguous`.

### 4.2 `link/broken` — Broken link

- **Condition**: a link target resolves to nothing, per §3, and the reference is not an
  anchor miss (those are `link/broken-anchor`) and not a shortcut.
- **Severity**: Error (wiki/embed), Warning (markdown).
- **Message** (ordinary missing target): `Broken link: '{target}' could not be resolved`
- **Directory mismatch** (RFC 0024): when local file fallback finds an existing directory,
  `Target '{target}' is a directory; directory links require a trailing '/'`, followed
  by `Hint: use '{target}/' to link to this directory.` The authored path spelling is
  preserved. For anchored targets, the hint also requires removal of the fragment:
  directory links do not support anchors. No hint for missing paths, files, URI targets,
  or ineligible bare wiki note identifiers. Severity remains inline Warning / wiki Error.
- **Wiki hint** (second line, wiki links only): when the broken target is a leading
  prefix of at least one document stem, the message appends
  `Hint: enable 'wiki.obsidian_prefix' to match partial filenames (candidates: …)` — up to
  5 file names, then ` (+N more)`. The hint applies regardless of the flag value and never
  to markdown links.
- **Range**: the target portion of the link, falling back to the whole link.
- **Suppressed** (no diagnostic):
  - external web schemes (LNK-03);
  - shortcut reference links — always;
  - single-file mode (explicit file on disk): unresolved non-empty, non-folder,
    non-anchor targets are not reported (cross-file diagnostics are disabled — 4.9),
    except a confirmed directory-without-slash mismatch, which is diagnosed even then.
    Stdin is NOT single-file mode: the stdin document is resolved against the full
    workspace and unresolved targets are reported.
- **Produces** for: missing documents, missing inline local files or wiki file-like
  attachments, and directory targets without a trailing slash (RES-06),
  missing directories / file-instead-of-directory (RES-04), folder link + heading
  (RES-04), unmapped non-web URI schemes (RES-07), mapped-but-missing assets (RES-07).

Tests: `missing_explicit_file_like_targets_emit_broken_link_diagnostics`,
`onedrive_missing_file_yields_broken_link`, `folder_link_to_missing_directory_is_broken`,
`obsidian_prefix_off_with_hint`, `obsidian_prefix_hint_caps_at_five`.

### 4.3 `heading/nbsp` — Non-breaking space after heading

- **Condition**: a line that **starts** with one or more `#` (indented lines do not
  trigger) and whose character immediately after the `#` run is U+00A0. `# Title`
  (regular space) does not trigger; `# Title` with an NBSP does.
- **Severity**: always Warning. One diagnostic per offending line.
- **Message** (verbatim): `Non-breaking whitespace after heading marker`
- **Range**: exactly the NBSP character.
- **Fixable**: `--fix` replaces the NBSP with a regular space.

Tests: *(none yet — coverage gap; see §8)*.

### 4.4 `link/broken-anchor` — Broken anchor

- **Condition**: an anchor failed to resolve after tolerant matching (RES-01):
  - in-page anchor (`[[#h]]`, `[x](#h)`) — no matching heading or tag in the document;
  - cross-document anchor (`[x](doc.md#h)`, `[[doc#h]]`) — document resolved, heading
    missing in it;
  - URI target with a non-empty anchor (RES-07) — always;
  - folder link + heading (RES-04) — always.
- **Severity**: always Warning, for every link form.
- **Message** (verbatim): `Broken anchor: '{display}' could not be resolved`, where
  `display` is `path#anchor` for cross-document anchors and `#anchor` for in-page anchors.
- **Range**: the anchor portion of the link (or the document part for `[[#h]]`), falling
  back to the whole link.

Tests: `inline_anchor_to_existing_heading_resolves`,
`inline_anchor_to_missing_heading_emits_dnl005_not_dnl002`,
`wiki_anchor_to_missing_heading_emits_dnl005_not_dnl002`,
`inline_anchor_cross_document_miss_emits_dnl005`,
`inline_anchor_tolerant_miss_still_emits_dnl005`.

### 4.5 `uri/no-mapping` — No scheme mapping

- **Condition**: a non-web URI-scheme target matched no `[[schemas]]` `uri`, at
  least one schema is configured, and `--no-uri-hints` was not passed.
- **Multiplicity**: at most **one per run**, attached to the first hint-eligible link.
- **Severity**: Info. The underlying `link/broken` for the same link is still emitted; `uri/no-mapping` is
  additive.
- **Message** (verbatim):
  `No scheme mapping found for '{target}'. Configure [[schemas]] in .downlint.toml, e.g.:`
  followed by an example `[[schemas]]` block and
  `Suppress this hint with --no-uri-hints.`
- **Suppressed** when `[[schemas]]` is unconfigured (repos without the feature see zero change).

Tests: `no_mapping_emits_hint_when_schemas_configured`,
`no_mapping_emits_no_hint_when_schemas_unconfigured`,
`no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken`.

### 4.6 Computation

- **Per occurrence**: each unresolved or ambiguous occurrence produces its own diagnostic;
  there is no deduplication across occurrences.
- **Order** (per folder): broken links (`link/broken`, `link/broken-anchor`) → ambiguous links (`link/ambiguous`) →
  `uri/no-mapping` → `mount/conflict` → `heading/nbsp` (per document).
- **Filtering**: after computation, diagnostics below `--min-severity` are dropped from
  output. CLI exit code reflects the filtered set.
- **Single-file mode** (explicit file on disk): cross-file diagnostics are disabled.
  Unresolved non-empty, non-folder, non-anchor targets are silently dropped (no
  `link/broken`), except confirmed existing directories used without a trailing `/`
  (RFC 0024); those mismatches, in-page anchors, and folder links are still diagnosed;
  `heading/nbsp` still runs.
- **Stdin** is workspace-anchored, not single-file: the full workspace is indexed
  (documents as targets only), the stdin document is the sole source, and all of its
  diagnostics are computed as in multi-file mode. Per-document rules (`heading/nbsp`)
  run only on the stdin document — workspace documents are not diagnosed.
- **Extra-folder documents** are not diagnosed from the primary session (RES-08).

Tests: `obsidian_prefix_single_file_mode`, `no_uri_sync_mapping_means_skipped_diagnostic`
(run-level aggregation), `cli_single_file_on_disk_still_suppresses_cross_file_broken_links`,
`cli_stdin_workspace_docs_are_targets_only`, `cli_stdin_heading_nbsp_in_workspace_doc_does_not_leak`.

---

## 5. Configuration Semantics

### 5.1 Files and Precedence

- **Project config**: `.downlint.toml` in the workspace root.
- **User config**: `~/Library/Application Support/downlint/config.toml` (macOS),
  `%APPDATA%\downlint\config.toml` (Windows),
  `$XDG_CONFIG_HOME/downlint/config.toml` or `~/.config/downlint/config.toml` (other).
- **Precedence**: project > user > built-in defaults. Merge is field-level: a key present
  in the project config wins; absent keys fall through to the user value, then the
  default. Exception: `[[schemas]]` and `[[mounts]]` — when the project config defines
  the list, it **replaces** the user's list wholesale (no per-entry merge).

### 5.2 Validation

- Config is validated **before** any processing (fail-fast). Any invalid file causes
  exit code 2 with an error message; there is no partial loading.
- Unknown keys are a parse error (`deny_unknown_fields` on every section). Removing a key
  from the schema is therefore a breaking change: e.g. the legacy keys
  `core.attachment_file_extensions_add`, `[uri]`, and `[[uri.mappings]]` fail parsing.
- Specific validations:
  - `core.file_extensions` must be non-empty.
  - `code_action.toc.include` must be non-empty and contain only levels 1–6.
  - `completion.candidates` is floored to 1.
  - `[[schemas]][*].uri` and `.to` are required and non-empty.
  - A `to` that fails expansion (missing `$VAR`, no home directory) is a startup error.

Tests: `parse_rejects_removed_attachment_extensions_key`, `schema_missing_uri_yields_indexed_error`,
`ignore_project_overrides_user`.

### 5.3 Per-Key Clauses

| Key | Default | Normative effect |
|---|---|---|
| `core.file_extensions` | `["md", "markdown"]` | Defines *document* (§1). Replaces defaults when set. |
| `core.heading_ids.enable` | `true` | RES-01 duplicate disambiguation. |
| `core.text_sync` | `"full"` (`"full" \| "incremental"`) | LSP text-sync mode (non-linting; see `spec/downlint.md`). |
| `core.title_from_heading` | `true` | RES-03 title = first H1. |
| `[[mounts]]` | `[]` | RES-08 (and RES-04 mount-path check). Each entry: `path` (required), `as` (optional, workspace-absolute), `lint` (optional, default `false`). `path` is relative to the config file's directory. |
| `core.ignore` | `[]` | RES-10. |
| `core.include_hidden` | `false` | RES-10: walk hidden files/directories as documents. |
| `wiki.obsidian_prefix` | `false` | RES-05. |
| `code_action.toc.enable` / `code_action.toc.include` | `true` / `[1,2,3,4,5,6]` | Non-linting (TOC code action). |
| `code_action.create_missing_file.enable` | `true` | Non-linting (code action). |
| `completion.candidates` | `50` | Non-linting. |
| `completion.wiki.style` | `"title-slug"` (`"title-slug" \| "title" \| "file-stem" \| "file-path-stem"`) | Non-linting. |
| `[[schemas]]` | `[]` | RES-07. Each entry: `uri` (required, e.g. `icloud://assets/`), `to` (required), `auto_verify` (optional, default `true`), `verify_cmd` (optional). `to` is relative to the config file's directory. |

CLI flags that affect linting behavior: `--min-severity` (default `warning`),
`--allow-uri-sync`, `--no-uri-hints`, `--uri-sync-batch-size` (default 50, min 1),
`--fix` (`heading/nbsp` only). See `spec/downlint.md` §3.1.

---

## 6. Guarantees and Edge Cases

### 6.1 Determinism

Given the same workspace contents, configuration, and filesystem state (including file
mtimes, which the placeholder heuristics consult), the same set of diagnostics MUST be
produced.

### 6.2 Edge Cases

1. `[[docs/]]` (trailing slash) is a folder link and MUST NOT match a file named `docs.md`.
2. `[[./doc]]` and `[x](./doc.md)` are explicit relative paths; `./` is normalized away
   after anchoring.
3. `[[%5B%5Bdoc%5D%5D]]` decodes to a document name containing literal `[[` — not a nested
   wiki-link.
4. `[text](#section)` resolves in the same document (heading or tag);
   `[text](other.md#section)` resolves `other.md` first, then the section inside it.
5. `[ref](data.json)` is an attachment candidate regardless of extension (RES-06).
6. Emoji in headings are dropped by slugging (`# 🚀 Launch` → `launch`); CJK is preserved.
7. `[[|My Title]]` (title-only) resolves by title slug.
8. `[[F\#]]` — escaped `#` is part of the document segment.
9. Tags inside code blocks are not detected; a trailing `#tag` in heading content is.
10. Subtags (`#rust/cli`, `#rust/cli/help`) are distinct, string-equal symbols; tag
    matching is case-insensitive.
11. `![[nonexistent]]` produces `link/broken` at **Error** severity, like a broken wiki-link.
12. The namespace is co-equal (RES-08): a link matching documents in both the primary
    project and a mount is ambiguous; there is no primary-wins tie-break.
13. `[text]` shortcut links are always suppressed from diagnostics.
14. Backslashes in link targets are treated as path separators.
15. Stem/path matching is ASCII case-insensitive in memory, on every platform.
16. A single-letter scheme (e.g. a Windows drive path `C:\…`) is treated as a
    URI-scheme target, not a path — it goes through RES-07. *(Known quirk.)*
17. File-level parse or read failures are reported as warnings and do not count toward
    the CLI exit code.

### 6.3 Out of Scope

- `textDocument/foldingRange`, `workspace/symbol` — deferred to v2.
- `workspace/willCreateFiles` / `willRenameFiles` / `willDeleteFiles` — deferred;
  the `did*` notifications plus the filesystem watcher keep the LSP index fresh
  (RFC 0022).
- Wiki-link ↔ markdown-link conversion — **not implemented**; the known
  future direction for publishing wiki-based notes (see `TODO.md`).
---

## 7. Non-Normative Appendix

### Worked Examples

1. `[[20260801-topic-a]]` with files `20260801-topic-a-sub-x.md` and
   `20260801-topic-a-sub-y.md`:
   - `wiki.obsidian_prefix = false` → `link/broken` (Error) + hint listing both candidates.
   - `wiki.obsidian_prefix = true` → `link/ambiguous` (Error) with related info for both files.
   - Only one file exists, flag on → resolves to it.
2. `[report](./data/report.xlsx)` with the file present → resolved attachment, no
   diagnostic. File missing → `link/broken` (Warning).
3. `[x](icloud://assets/big.pdf)` with a matching schema, file present → resolved
   attachment. No schema configured at all → `link/broken` (no `uri/no-mapping` hint).

---

## 8. Amendment History

| Date | RFC | Clauses changed |
|---|---|---|
| 2026-09-20 | — | Initial extraction from the bootstrap `spec.md` and implemented RFCs 0001–0004, 0006–0009. Where the bootstrap spec and code disagreed, the code won: `[wiki]` section added to §5; `core.file_extensions` default `["md","markdown"]`; `code_action.toc.include` default `[1..6]`; `warm_timeout` default 30; DNL003 message text; DNL006/007/008 multiplicity is per-run; hidden files excluded by default; exactly 9 silent web schemes; DNL009 declared but not emitted; folder links resolve to the directory itself (no index-file lookup); DNL005 always Warning. |
| 2026-09-20 | — | Bootstrap `spec.md` and `rfc/0001`–`0009` removed from the working tree (folded in; git history is the record). Open issues moved to `TODO.md`. |
| 2026-09-21 | — | Diagnostic codes re-based from opaque `DNLnnn` to namespaced semantic slugs. Old→new: `DNL001`→`link/ambiguous`, `DNL002`→`link/broken`, `DNL003`→`heading/nbsp`, `DNL005`→`link/broken-anchor`, `DNL006`→`uri/no-mapping`, `DNL007`→`uri/sync-skipped`, `DNL008`→`uri/sync-failed`, `DNL009`→`uri/batch-clamped`. `DNL004` was never assigned and is retired with the numeric scheme. Severity is unchanged (still a separate axis). Wire-format breaking change: emitted `code` strings changed. |
| 2026-09-21 | 0010 (Phase 1) | RES-08 re-scoped from “Cross-Folder Resolution” (fallback) to “Mounts” (co-equal, indexed resolution roots): `core.extra_folders` replaced by `[[mounts]]` (`root`, optional `prefix`, optional `lint`); mounted documents indexed co-equal with primary (no primary-wins tiering); relative links do not cross mounts; workspace-absolute links reach a mount via its `prefix`; bare stem/title resolve across the whole namespace; `lint` controls whether a mounted doc is a source; diagnostics from mounted docs carry mount attribution. New diagnostic `mount/conflict` (Error) for structural prefix/folder collisions, with suspend behavior. `link/ambiguous` condition updated (co-equal namespace). |
| 2026-09-21 | 0010 (Phase 2) | RES-07 re-scoped from “External URI Mapping” (warming) to “Schemes” (rewrite + stat + verify): `[uri]` / `[[uri.mappings]]` replaced by `[[schemas]]` (`prefix`, `root`, optional `auto_verify` bool default `true`, optional `verify_cmd`); warming removed (`warm_cmd` / `warm_required` / `warm_timeout` dropped); no caching; the `warm-uri-mappings` subcommand removed; `auto_verify` is now a per-schema bool running vendor-specific heuristics only (the generic 0-byte + recent-mtime heuristic dropped); `verify_cmd` gated by `--allow-uri-sync`. Diagnostics `uri/sync-skipped`, `uri/sync-failed`, `uri/batch-clamped` tombstoned; `uri/no-mapping` kept (re-scoped to schemes). |
| 2026-09-21 | 0011 | RES-08 `mount/conflict` re-scoped from a coarse top-level-entry check (prefix/folder name match) to a fine-grained **namespace-path collision**: a same-path file collision, or a file/folder sharing a name (stem) at the same location. Sharing a top-level name alone is no longer a conflict (a mount merges into an existing folder when no file collides). `MountConflictKind::{Prefix,Folder}` replaced by a single `PathCollision`. Conflicting mount file(s) are targets only (not linted). `link/ambiguous` is explicitly *not* a mount conflict. |
| 2026-09-22 | 0012 | Added the `downlint resolve` target resolution query (spec/downlint.md §3.1): a read-only projection of the existing matching rules (RES-03/04/05/06/07) — no new diagnostics, no new matching semantics. The per-document matching rules are single-sourced (`match_document_kinds`), shared by link resolution and the query; `find_doc_matches` is rebuilt on top with unchanged behavior. |
| 2026-09-23 | 0013 | Obsidian-compatible path resolution. RES-02 re-scoped to a strict prefix-driven base rule (no fallback): `./…`/`../…` → containing document's directory; `/…` and **bare wiki** `path/file` → workspace root; markdown bare paths/basenames → containing document's directory (unchanged). `.`/`..` normalized lexically before comparison. RES-03 explicit matching re-scoped: the resolved candidate is compared with the **`.md` suffix optional for wiki targets** (fixes the previously-dead extensionless rule); root-relative targets also match the namespace path (mount `prefix` access, now for bare wiki paths too, not only `/…`). RES-06: a markdown file matching an indexed document resolves as a document (attachment fallback is for non-markdown / unmatched file-like paths). LNK-02 adds the **bare wiki path** form. Consequences: extensionless and dot-relative wiki path links resolve; `[[../x.md]]` resolves as a document (not attachment) and its anchor is validated. |
| 2026-09-23 | 0014 | Config key rename for clarity (no behavior change). `[[mounts]]`: `root`→`path`, `prefix`→`as`. `[[schemas]]`: `prefix`→`uri`, `root`→`to`. Each key is now self-evident and the two sections no longer share a vocabulary (a mount is a disk `path` exposed `as` a virtual path; a schema is a `uri` that resolves `to` a disk folder). Breaking: existing configs using the old keys fail to parse (`deny_unknown_fields`); migration is a mechanical key rename. The internal `ResolvedMount` fields are renamed to match (`path`/`as`). |
| 2026-09-23 | 0022 | LSP workspace freshness (spec/downlint.md §3.2): the LSP index is no longer a static `initialize` snapshot — `didOpen`/`didChange` upsert new documents (even unsaved buffers), `didClose` drops never-saved buffers, `workspace/didCreateFiles`/`didDeleteFiles` are reconciled against disk on the next re-index, and a filesystem watcher covers out-of-editor disk changes (created files become resolvable; deleted files re-raise `link/broken`). Open editor buffers are never overwritten or removed by reconciliation. The re-index debounce is capped (2 s max wait) so a continuous stream of changes cannot starve it. No linting-rule changes. |
| 2026-10-08 | 0024 | RES-06: bare extensionless inline file targets resolve exactly as attachments, without implicit `.md`; all local attachment checks require files and existing directories without trailing `/` report `link/broken` with migration guidance, including single-file mode. Wiki note matching is unchanged; the wiki-style query shares file-only checks. LSP editor buffers enter the Markdown document set only for configured extensions. Attachment rename edits only actual destinations, preserves wiki attachment path semantics, and independently rewrites definition URLs in definition-only sources. |
| 2026-09-23 | 0023 | RES-10 made true in code: hidden files and directories are now actually excluded by default (the walk ran `hidden(false)` since the initial commit, contradicting the spec — `.trash/`, `.obsidian/`, and dotfile notes were indexed and resolvable). New key `core.include_hidden` (default `false`) restores the include-everything walk; gitignore/`core.ignore` negation cannot re-include hidden files (the walker's hidden filter takes precedence over ignore rules). Force-added root symlinks are unaffected. |

**Known coverage gaps** (clauses without tests yet): scheme-target + anchor →
`link/broken-anchor`.
