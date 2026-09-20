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
| **extra folder** | An additional folder declared via `core.extra_folders`, loaded from the primary folder's configuration. Used only as a resolution fallback (RES-08). |
| **document** | A file whose extension (without dot) is in `core.file_extensions`, discovered in a primary or extra folder. |
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
| **explicit file-like target** | An explicit path, or a same-directory basename with an extension (e.g. `data.xlsx`). |
| **resolved** | A link with exactly one destination. **Ambiguous**: more than one. **Broken**: none. |
| **occurrence** | Identity of a single link instance. Two identical links at different ranges are distinct occurrences and are diagnosed independently. |
| **single-file mode** | A folder containing a single document (or stdin input). Cross-file diagnostics are disabled (4.9). |

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
7. shortcut reference link `[text]` — recognized but **never diagnosed** (see link/broken)

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

- **relative path** — resolved against the containing document's directory
  (`[x](next.md)` from `docs/guide/intro.md` → `docs/guide/next.md`)
- **workspace-absolute path** — leading `/`, resolved against the workspace root
  (`[x](/README.md)` → `<root>/README.md`)
- **dot-relative path** — `./doc`, `../doc`; explicit, anchored at the source document
  directory, `./` normalized away after anchoring
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
  link/broken (and uri/no-mapping).
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
duplicates as a multi-destination resolved reference (not link/ambiguous).

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

- Relative targets resolve against the **containing document's directory**.
- Workspace-absolute targets (`/…`) resolve against the **workspace root**.
- Targets are percent-decoded (RFC 3986, UTF-8); malformed escapes are kept literal.
- Backslashes in link targets are treated as path separators (normalized to `/` internally).
- Stem and path comparisons are **ASCII case-insensitive** in memory, regardless of
  platform. *(non-normative: if both `Doc.md` and `doc.md` exist in one folder, a link to
  either matches both → link/ambiguous.)*
- Unicode normalization: precomposed and decomposed forms (e.g. `é` U+00E9 vs `e` + U+0301)
  are equivalent for matching (NFKC).

Tests: `inline_link_with_non_ascii_filename_resolves_correctly`,
`wiki_link_with_mojibake_does_not_match_correct_title`,
`percent_encoded_target_decodes_to_filesystem_path`.

### 3.3 RES-03 — Document Matching

For a **non-explicit** wiki target, a document matches when ANY of:

1. document stem equals the target (ASCII case-insensitive)
2. document title slug equals the target slug (both slug-normalized per RES-01)
3. workspace-relative path without extension equals the target (ASCII case-insensitive)
4. full workspace-relative path (backslashes → `/`) equals the target (ASCII case-insensitive)

For an **explicit** target, matching is path-based only: exact path equality or
relative-path-without-extension equality. When searching extra folders, a title-slug
fallback is additionally allowed.

**Title.** When `core.title_from_heading` is true (default), the document's first H1 is its
title; otherwise the file stem. Title-only wiki links (`[[|My Title]]`) resolve by title
slug.

**Tiering.** Primary-folder documents are searched first. If the primary tier yields more
than one match, the link is immediately ambiguous (link/ambiguous) and extra folders are not
consulted. If the primary tier yields zero, extra folders are searched (wiki, non-explicit
targets only — RES-08). Matches are deduplicated by document path.

Tests: `wiki_link_with_non_ascii_title_resolves_correctly`,
`wiki_link_explicit_path_resolves_in_extra_folders`,
`wiki_link_with_slash_in_target_resolves_via_title_slug_in_extra_folders`.

### 3.4 RES-04 — Folder Link Resolution

**Detection.** A target is a folder link when the part before the first `#` ends with `/`.
Applies to both wiki (`[[dir/]]`) and markdown (`[x](dir/)`) links. A target naming a
directory **without** a trailing slash is NOT a folder link and goes through normal
resolution.

**Resolution.**

1. Strip trailing slashes; resolve the path per RES-02.
2. If a directory exists at the resolved path, the link resolves **to the directory
   itself**. There is no index-file lookup; an empty directory is a valid target.
3. For workspace-absolute targets only, extra folder roots are also tried (direct join,
   then matching the first path component against the extra root's own name). The first
   existing directory wins; folder resolution never produces ambiguity.
4. Otherwise the link is broken (link/broken) — including when the path exists but is a file.

**Folder link + heading** (`[[dir/#h]]`, `[x](dir/#h)`) is invalid: the link is broken
(link/broken).

Tests: `folder_link_to_existing_directory_resolves`,
`folder_link_to_missing_directory_is_broken`, `folder_link_to_file_not_directory_is_broken`,
`wiki_link_to_folder_resolves`, `folder_link_with_anchor_is_unresolved`,
`folder_link_in_extra_folder_resolves`, `non_folder_link_to_directory_name_unchanged`.

### 3.5 RES-05 — Prefix Matching

**Gate.** Prefix matching runs only for **wiki** targets, only when `wiki.obsidian_prefix`
is true (default false), only for non-empty non-explicit targets, and only after exact,
title-slug, and relative-path matching (RES-03) have returned zero candidates.

**Matching.** The target is treated as a leading prefix of document stems (primary and
extra folders), ASCII case-insensitive. There is no word-boundary requirement: any leading
prefix matches; a suffix does not.

**Multiplicity.**

- 0 candidates → broken (link/broken)
- 1 candidate → resolved to that document (a `#heading` on the target still gets heading
  lookup on the resolved document)
- ≥ 2 candidates → ambiguous (link/ambiguous), with related information for every candidate.
  Ambiguity is decided before heading lookup: even if only one candidate has the heading,
  link/ambiguous is emitted.

**Interactions.** Alias (`[[x|title]]`) is display-only. Embeds follow the same rules.
Explicit paths and folder links short-circuit before prefix matching.

**Discoverability hint.** Regardless of the flag value, when a **wiki** link is broken and
its target is a leading prefix of at least one document stem, the link/broken message gains a
hint line (see link/broken). Markdown links never receive the hint.

Tests: `obsidian_prefix_unique_resolves`, `obsidian_prefix_ambiguous_link_ambiguous`,
`obsidian_prefix_off_no_hint_no_match`, `obsidian_prefix_off_with_hint`,
`obsidian_prefix_hint_caps_at_five`, `obsidian_prefix_case_insensitive`,
`obsidian_prefix_does_not_match_suffix_only`, `obsidian_prefix_explicit_path_wins`,
`obsidian_prefix_alias_form_resolves`, `obsidian_prefix_embed_form_resolves`,
`obsidian_prefix_heading_anchor`, `obsidian_prefix_single_file_mode`,
`obsidian_prefix_does_not_resolve_folder_link`, `unique_prefix_resolves`,
`ambiguous_prefix_resolves_to_multiple`, `leading_prefix_only_does_not_match_suffix`.

### 3.6 RES-06 — Explicit File-Like Targets

There is **no attachment extension whitelist**. Resolution order for a local-looking
target:

1. Document resolution (RES-03).
2. If no document matched and the target is an *attachment candidate* — it starts with
   `/`, `./`, or `../`, contains `/` or `\`, or is a basename with a non-empty base and
   extension — the resolved filesystem path is checked.
3. If the file exists, the link resolves as an **attachment**.
4. If the file does not exist, the link is broken (link/broken at the reference's severity).

Targets without a path separator and without an extension (e.g. `intro`) never trigger the
attachment fallback. External web schemes (LNK-03) are still skipped.

Tests: `explicit_file_like_targets_resolve_as_attachments_without_config`,
`missing_explicit_file_like_targets_emit_broken_link_diagnostics`,
`explicit_markdown_document_paths_resolve_as_documents_and_headings`,
`parse_rejects_removed_attachment_extensions_key`.

### 3.7 RES-07 — External URI Mapping

**Detection.** A target has a URI scheme when the part before the first `:` is non-empty
and consists of scheme characters `[A-Za-z0-9+-.]`. Web schemes (LNK-03) are handled
first; all other scheme targets enter URI resolution.

**Mapping match.** Each `[[uri.mappings]]` prefix is normalized with a trailing slash. A
target matches when it equals the prefix (slash-less) or starts with the normalized
prefix. The **most specific** (longest) matching prefix wins; only the first match is used.

**Path computation.** The resolved local path is `root + percent-decoded remainder`, where
the remainder is the target minus the prefix (leading `/` trimmed). An empty remainder
resolves to the root itself.

**Root expansion** (per mapping, at startup):

1. `$VAR` / `${VAR}` are expanded; a missing variable is a startup configuration error.
2. Leading `~` expands to the user's home directory.
3. A still-relative path resolves against the directory containing the config file.

**Warming.** `warm_cmd` runs **only** when `--allow-uri-sync` is passed — committing a
config with arbitrary commands MUST NOT cause subprocess execution by itself.

- `{path}` occurrences in arguments are replaced with the resolved absolute file path.
- If any argument contains `{paths}`, the command runs in **stdin mode**: one path per line
  on stdin, `{path}` substituted empty.
- The command runs with a deadline of `warm_timeout` seconds; on timeout the child is
  killed and the outcome is *timed out*.
- Outcome: exit 0 + file exists → *present*; exit 0 + missing → *missing*; non-zero exit
  or spawn failure → *failed*.
- **Verification** (placeholder detection), in order: if `verify_cmd` is set, it runs
  after a successful warm (exit 0 = real file; non-zero/timeout/spawn failure = missing).
  Otherwise the `uri.auto_verify` heuristics apply: OneDrive zero-byte `._<name>` resource
  fork (or any `._*` sibling under a OneDrive path), iCloud `Mobile Documents` path with a
  sibling `.<name>.icloud`, or generic 0-byte file with recent mtime under a known
  cloud-storage path. An inconclusive heuristic passes.

**Outcomes.**

- *Present* → the link resolves as an attachment.
- *Missing* → link/broken. Additionally uri/sync-failed iff `warm_required = false` AND warming ran AND
  the outcome was *failed* or *timed out*. With `warm_required = true`, only link/broken is
  emitted (hard failure).
- **No mapping** → link/broken, plus uri/no-mapping hint when at least one mapping is configured and
  `--no-uri-hints` was not passed.
- **Anchor on a URI target** (`scheme://…#anchor`) → always link/broken-anchor: anchors on external
  assets are not supported.

**Batching.** Warming is batched, never per-link: paths are fanned out per subprocess in
chunks of `--uri-sync-batch-size` (default 50, minimum 1). When a chunk's constructed
argv would exceed 128 KiB, the runner falls back to per-file spawning; the `{paths}`
stdin tier is exempt from the cap. *(Non-normative status: the fallback currently occurs
silently — see uri/batch-clamped.)*

**Caching.** Within one LSP server, warm results are cached per (mapping, path) and shared
across requests; each CLI run starts fresh.

Tests: `onedrive_present_file_resolves`, `onedrive_missing_file_yields_broken_link`,
`no_mapping_emits_hint_when_uri_configured`, `no_mapping_emits_no_hint_when_uri_unconfigured`,
`no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken`,
`allow_uri_sync_with_mock_cmd_marks_present_after_run`,
`no_uri_sync_mapping_means_skipped_diagnostic`,
`warm_required_with_failing_cmd_marks_broken`,
`soft_sync_failure_emits_uri_sync_failed_alongside_link_broken`,
`trailing_slash_normalization_matches_both_forms`, `env_var_expansion_in_root`,
`relative_root_resolves_against_config_dir`, `more_specific_prefix_wins`,
`verify_cmd_pass_marks_present_after_sync`,
`verify_cmd_failure_marks_broken_even_when_file_exists`,
`auto_verify_off_skips_heuristics`, `batch_size_three_fans_out_into_one_spawn`,
`batch_clamps_to_per_file_when_argv_exceeds_limit`.

### 3.8 RES-08 — Cross-Folder Resolution

- Primary folders are searched before extra folders; extra folders are a **fallback only**.
- Only wiki (non-explicit) targets may resolve into extra folders. Markdown links with
  path syntax are anchored to the source document and never use the extra-folder fallback.
- A primary-tier match wins; a link is never ambiguous *across* tiers. Ambiguity arises
  only within the active tier.
- Resolution is non-transitive: no chaining A → B → C through extra folders.
- Documents in extra folders are not diagnosed from the primary session.

Tests: `wiki_link_explicit_path_resolves_in_extra_folders`,
`folder_link_in_extra_folder_resolves`.

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
  as documents.
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

| Code | Name | Assigned severity | Condition (clause) |
|---|---|---|---|
| link/ambiguous | AmbiguousLink | Error (wiki/embed) / Warning (markdown) | ≥ 2 destinations |
| link/broken | BrokenLink | Error (wiki/embed) / Warning (markdown) | no destination |
| heading/nbsp | NonBreakableWhitespace | Warning | U+00A0 after heading marker |
| link/broken-anchor | BrokenAnchor | Warning (all link forms) | anchor did not resolve |
| uri/no-mapping | UriNoMappingHint | Info | unmapped URI target, mappings configured |
| uri/sync-skipped | UriSyncSkipped | Info | warm mappings present, sync not allowed |
| uri/sync-failed | SyncFailureWarning | Info | soft warm failure |
| uri/batch-clamped | BatchClamped | Info | declared; **not emitted** (see 4.8) |

**Severity rule.** Wiki links and embeds receive **Error** for `link/ambiguous` and `link/broken` on local
targets; markdown links, images, and reference links receive **Warning**. `link/broken-anchor` and
`heading/nbsp` are always Warning; the four `uri/*` rules are always Info. `--min-severity` filters
output only; it never changes assigned severity. The CLI default is `warning`; the LSP
always reports at `info`.

### 4.1 link/ambiguous — AmbiguousLink

- **Condition**: a link target resolves to more than one destination — multiple primary
  matches, multiple extra-folder matches (when the primary tier is empty), or prefix
  matching with ≥ 2 candidates (RES-05).
- **Severity**: Error (wiki/embed), Warning (markdown).
- **Message** (verbatim): `Ambiguous link: '{target}' resolves to multiple destinations`
- **Related information**: one entry per destination, carrying its path. No cap.
- **Range**: the target portion of the link (the editable destination text), falling back
  to the whole link.
- Duplicate *link definitions* with the same label do NOT produce link/ambiguous: the reference
  link resolves to all of them as a multi-destination resolved reference.

Tests: `obsidian_prefix_ambiguous_link_ambiguous`.

### 4.2 link/broken — BrokenLink

- **Condition**: a link target resolves to nothing, per §3, and the reference is not an
  anchor miss (those are link/broken-anchor) and not a shortcut.
- **Severity**: Error (wiki/embed), Warning (markdown).
- **Message** (verbatim): `Broken link: '{target}' could not be resolved`
- **Wiki hint** (second line, wiki links only): when the broken target is a leading
  prefix of at least one document stem, the message appends
  `Hint: enable 'wiki.obsidian_prefix' to match partial filenames (candidates: …)` — up to
  5 file names, then ` (+N more)`. The hint applies regardless of the flag value and never
  to markdown links.
- **Range**: the target portion of the link, falling back to the whole link.
- **Suppressed** (no diagnostic):
  - external web schemes (LNK-03);
  - shortcut reference links — always;
  - single-file mode: unresolved non-empty, non-folder, non-anchor targets are not
    reported (cross-file diagnostics are disabled — 4.9).
- **Produces** for: missing documents, missing explicit file-like attachments (RES-06),
  missing directories / file-instead-of-directory (RES-04), folder link + heading
  (RES-04), unmapped non-web URI schemes (RES-07), mapped-but-missing assets (RES-07).

Tests: `missing_explicit_file_like_targets_emit_broken_link_diagnostics`,
`onedrive_missing_file_yields_broken_link`, `folder_link_to_missing_directory_is_broken`,
`obsidian_prefix_off_with_hint`, `obsidian_prefix_hint_caps_at_five`.

### 4.3 heading/nbsp — NonBreakableWhitespace

- **Condition**: a line that **starts** with one or more `#` (indented lines do not
  trigger) and whose character immediately after the `#` run is U+00A0. `# Title`
  (regular space) does not trigger; `# Title` with an NBSP does.
- **Severity**: always Warning. One diagnostic per offending line.
- **Message** (verbatim): `Non-breaking whitespace after heading marker`
- **Range**: exactly the NBSP character.
- **Fixable**: `--fix` replaces the NBSP with a regular space.

Tests: *(none yet — coverage gap; see §8)*.

### 4.4 link/broken-anchor — BrokenAnchor

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

### 4.5 uri/no-mapping — UriNoMappingHint

- **Condition**: a non-web URI-scheme target matched no `[[uri.mappings]]` prefix, at
  least one mapping is configured, and `--no-uri-hints` was not passed.
- **Multiplicity**: at most **one per run**, attached to the first hint-eligible link.
- **Severity**: Info. The underlying link/broken for the same link is still emitted; uri/no-mapping is
  additive.
- **Message** (verbatim):
  `No URI mapping found for '{target}'. Configure [[uri.mappings]] in .downlint.toml, e.g.:`
  followed by an example `[[uri.mappings]]` block and
  `Suppress this hint with --no-uri-hints.`
- **Suppressed** when `[uri]` is unconfigured (repos without the feature see zero change).

Tests: `no_mapping_emits_hint_when_uri_configured`,
`no_mapping_emits_no_hint_when_uri_unconfigured`,
`no_uri_hints_flag_suppresses_uri_no_mapping_but_keeps_link_broken`.

### 4.6 uri/sync-skipped — UriSyncSkipped

- **Condition**: at least one `[[uri.mappings]]` entry has `warm_cmd` and the run did not
  pass `--allow-uri-sync`.
- **Multiplicity**: one aggregated diagnostic per run, listing all affected mappings.
- **Location**: workspace root (path `.`), range `(0, 0)` — no source position is
  meaningful for a global configuration warning.
- **Severity**: Info.
- **Message** (verbatim template):
  `URI mappings skipped: {N} {entry|entries} {has|have} a 'warm_cmd' but --allow-uri-sync was not passed. Affected prefixes: {list}{more}.`
  followed by `Run 'downlint warm-uri-mappings --allow-uri-sync' to warm them.` — prefix
  list capped at 3, then ` (+N more)`.

Tests: `no_uri_sync_mapping_means_skipped_diagnostic`.

### 4.7 uri/sync-failed — SyncFailureWarning

- **Condition**: warming ran (with `--allow-uri-sync`), the outcome was *failed* or
  *timed out*, the file is still missing on disk, and `warm_required = false`.
- **Multiplicity**: at most **one per run**, attached to the first soft-failure link.
- **Severity**: Info. Emitted **alongside** the link/broken for the same link — it distinguishes
  a soft failure from a hard one. Mappings with `warm_required = true` produce only link/broken.
- **Message** (verbatim):
  `Sync completed but '{target}' is still missing on disk. The link is reported as broken; with 'warm_required = false', this is a soft warning rather than a hard failure.`
- The failed command is never echoed in any diagnostic (secrets).

Tests: `soft_sync_failure_emits_uri_sync_failed_alongside_link_broken`,
`warm_required_with_failing_cmd_marks_broken`.

### 4.8 uri/batch-clamped — BatchClamped *(declared, not emitted)*

- **Intended condition**: a warm batch's constructed argv exceeds the 128 KiB cap and the
  runner falls back to per-file spawning.
- **Current status**: the code is declared and the fallback mechanism exists, but **no
  diagnostic is emitted** — the fallback occurs silently. This clause records the reserved
  code and intended behavior; emission is unimplemented.
- The code MUST NOT be reused for any other purpose.

Tests: `batch_clamps_to_per_file_when_argv_exceeds_limit` (asserts the fallback flag only,
no diagnostic).

### 4.9 Computation

- **Per occurrence**: each unresolved or ambiguous occurrence produces its own diagnostic;
  there is no deduplication across occurrences.
- **Order** (per folder): broken links (`link/broken`, `link/broken-anchor`) → ambiguous links (`link/ambiguous`) →
  uri/no-mapping → uri/sync-failed → uri/sync-skipped → heading/nbsp (per document).
- **Filtering**: after computation, diagnostics below `--min-severity` are dropped from
  output. CLI exit code reflects the filtered set.
- **Single-file mode**: cross-file diagnostics are disabled. Unresolved non-empty,
  non-folder, non-anchor targets are silently dropped (no link/broken); in-page anchors and
  folder links are still diagnosed; heading/nbsp still runs.
- **Extra-folder documents** are not diagnosed from the primary session (RES-08).

Tests: `obsidian_prefix_single_file_mode`, `no_uri_sync_mapping_means_skipped_diagnostic`
(run-level aggregation).

---

## 5. Configuration Semantics

### 5.1 Files and Precedence

- **Project config**: `.downlint.toml` in the workspace root.
- **User config**: `~/Library/Application Support/downlint/config.toml` (macOS),
  `%APPDATA%\downlint\config.toml` (Windows),
  `$XDG_CONFIG_HOME/downlint/config.toml` or `~/.config/downlint/config.toml` (other).
- **Precedence**: project > user > built-in defaults. Merge is field-level: a key present
  in the project config wins; absent keys fall through to the user value, then the
  default. Exception: `uri.mappings` — when the project config defines the list, it
  **replaces** the user's list wholesale (no per-entry merge).

### 5.2 Validation

- Config is validated **before** any processing (fail-fast). Any invalid file causes
  exit code 2 with an error message; there is no partial loading.
- Unknown keys are a parse error (`deny_unknown_fields` on every section). Removing a key
  from the schema is therefore a breaking change: e.g. the legacy keys
  `core.attachment_file_extensions_add` and `[[uri.mappings]] sync_cmd` (renamed to
  `warm_cmd`) fail parsing.
- Specific validations:
  - `core.file_extensions` must be non-empty.
  - `code_action.toc.include` must be non-empty and contain only levels 1–6.
  - `completion.candidates` is floored to 1.
  - `uri.mappings[*].prefix` and `.root` are required and non-empty.
  - `uri.mappings[*].warm_timeout` must be > 0.
  - `uri.auto_verify` must be one of `on | off | onedrive-only | icloud-only`.
  - A `root` that fails expansion (missing `$VAR`, no home directory) is a startup error.

Tests: `parse_rejects_removed_attachment_extensions_key`, `uri_missing_prefix_yields_indexed_error`,
`uri_zero_timeout_errors`, `ignore_project_overrides_user`.

### 5.3 Per-Key Clauses

| Key | Default | Normative effect |
|---|---|---|
| `core.file_extensions` | `["md", "markdown"]` | Defines *document* (§1). Replaces defaults when set. |
| `core.heading_ids.enable` | `true` | RES-01 duplicate disambiguation. |
| `core.text_sync` | `"full"` (`"full" \| "incremental"`) | LSP text-sync mode (non-linting; see `spec/downlint.md`). |
| `core.title_from_heading` | `true` | RES-03 title = first H1. |
| `core.extra_folders` | `[]` | RES-08 (and RES-04 extra-root check). Relative to the config file's directory. |
| `core.ignore` | `[]` | RES-10. |
| `wiki.obsidian_prefix` | `false` | RES-05. |
| `code_action.toc.enable` / `code_action.toc.include` | `true` / `[1,2,3,4,5,6]` | Non-linting (TOC code action). |
| `code_action.create_missing_file.enable` | `true` | Non-linting (code action). |
| `completion.candidates` | `50` | Non-linting. |
| `completion.wiki.style` | `"title-slug"` (`"title-slug" \| "title" \| "file-stem" \| "file-path-stem"`) | Non-linting. |
| `uri.auto_verify` | `"on"` (`"on" \| "off" \| "onedrive-only" \| "icloud-only"`) | RES-07 placeholder heuristics. |
| `uri.mappings[*].prefix` | required | RES-07 prefix match. |
| `uri.mappings[*].root` | required | RES-07 root expansion. |
| `uri.mappings[*].warm_cmd` | absent | RES-07 warming; drives uri/sync-skipped count. |
| `uri.mappings[*].warm_required` | `false` | RES-07 hard vs soft failure (uri/sync-failed). |
| `uri.mappings[*].warm_timeout` | `30` (seconds) | RES-07 warm deadline; also the verify deadline. |
| `uri.mappings[*].verify_cmd` | absent | RES-07 authoritative placeholder check. |

CLI flags that affect linting behavior: `--min-severity` (default `warning`),
`--allow-uri-sync`, `--no-uri-hints`, `--uri-sync-batch-size` (default 50, min 1),
`--fix` (heading/nbsp only). See `spec/downlint.md` §3.1.

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
11. `![[nonexistent]]` produces link/broken at **Error** severity, like a broken wiki-link.
12. Ambiguity never crosses tiers (RES-08): a primary match wins over extra-folder
    candidates.
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
  `did*` notifications suffice for v1 indexing.
- Wiki-link ↔ markdown-link conversion — **not implemented**; the known
  future direction for publishing wiki-based notes (see `TODO.md`).
- uri/batch-clamped emission (4.8) — mechanism present, diagnostic unimplemented.

---

## 7. Non-Normative Appendix

### Worked Examples

1. `[[20260801-topic-a]]` with files `20260801-topic-a-sub-x.md` and
   `20260801-topic-a-sub-y.md`:
   - `wiki.obsidian_prefix = false` → link/broken (Error) + hint listing both candidates.
   - `wiki.obsidian_prefix = true` → link/ambiguous (Error) with related info for both files.
   - Only one file exists, flag on → resolves to it.
2. `[report](./data/report.xlsx)` with the file present → resolved attachment, no
   diagnostic. File missing → link/broken (Warning).
3. `[x](onedrive://work/big.pdf)` with a matching mapping, file present after warm →
   resolved attachment. No mapping configured at all → link/broken (no uri/no-mapping hint).

---

## 8. Amendment History

| Date | RFC | Clauses changed |
|---|---|---|
| 2026-09-20 | — | Initial extraction from the bootstrap `spec.md` and implemented RFCs 0001–0004, 0006–0009. Where the bootstrap spec and code disagreed, the code won: `[wiki]` section added to §5; `core.file_extensions` default `["md","markdown"]`; `code_action.toc.include` default `[1..6]`; `warm_timeout` default 30; DNL003 message text; DNL006/007/008 multiplicity is per-run; hidden files excluded by default; exactly 9 silent web schemes; DNL009 declared but not emitted; folder links resolve to the directory itself (no index-file lookup); DNL005 always Warning. |
| 2026-09-20 | — | Bootstrap `spec.md` and `rfc/0001`–`0009` removed from the working tree (folded in; git history is the record). Open issues moved to `TODO.md`. |
| 2026-09-21 | — | Diagnostic codes re-based from opaque `DNLnnn` to namespaced semantic slugs. Old→new: `DNL001`→`link/ambiguous`, `DNL002`→`link/broken`, `DNL003`→`heading/nbsp`, `DNL005`→`link/broken-anchor`, `DNL006`→`uri/no-mapping`, `DNL007`→`uri/sync-skipped`, `DNL008`→`uri/sync-failed`, `DNL009`→`uri/batch-clamped`. `DNL004` was never assigned and is retired with the numeric scheme. Severity is unchanged (still a separate axis). Wire-format breaking change: emitted `code` strings changed. |

**Known coverage gaps** (clauses without tests yet): `heading/nbsp`; `uri/batch-clamped` (by design, until
emission lands); single-file-mode `link/broken` suppression; URI-target + anchor → `link/broken-anchor`.
