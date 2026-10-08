# RFC 0024 — Extensionless files as inline Markdown link targets

**Status**: Proposed
**Date**: 2026-10-08
**Scope**: RES-06 attachment fallback for inline Markdown links and images
in `src/resolution/mod.rs`, LSP editor-buffer document classification, and
destination-safe attachment rename, including reference-definition URLs.
Local directory targets must have a trailing `/`; accepting directories
as attachments is deliberately removed. The two related pre-existing
bugs below must also be fixed as part of implementation. No new
configuration, diagnostic codes, file kinds, or wiki note-matching rules.
No implementation in this RFC-only change.

---

## 1. Summary

Accept an inline Markdown link to an existing file even when its target
has no extension or path separator:

```markdown
[Apache License 2.0](LICENSE)
```

After existing document resolution returns zero matches, resolve the
non-empty local target to an exact filesystem path using the existing
Markdown resolution base. If that path is a file, resolve it as an
**attachment**, just as `./LICENSE` already resolves today.

This generalizes the existing attachment fallback for inline references;
it does not introduce a special case for license files. Wiki references
retain their current attachment-candidate predicate. All local attachment
checks require a file, never merely an existing directory. A directory
link must end in `/`; otherwise report `link/broken` with a hint to add it
when the exact target path is an existing directory.

The author-facing model is:

- Markdown links identify the exact written path; no extension inference.
- Bare wiki links use existing note matching; explicit wiki attachment
  links retain path semantics.
- Directory links require a trailing `/`.
- Opening a plain file does not make it a Markdown document.
- Renaming a file changes only its references, and those rewritten
  references must still point at the moved file.

Implementation also fixes two existing bugs: opening a plain file in the
editor must not promote it into a Markdown document, and renaming an
attachment must not rewrite other links merely because they occur in a
document that references that attachment.

## 2. Motivation and current behavior

Extensionless repository files such as `LICENSE`, `NOTICE`, and `COPYING`
are normal link targets in READMEs. A relative Markdown link does not need
an extension or an explicit `./` prefix to identify a file.

Verified with the current source: given `index.md` and an existing
plain-text `LICENSE` in the same directory,

```markdown
[Apache License 2.0](LICENSE)
[Apache License 2.0](./LICENSE)
```

only the first link emits:

```text
warning: Broken link: 'LICENSE' could not be resolved [link/broken]
```

The reason is `is_attachment_candidate_path`: fallback is allowed only
for targets with a path prefix, a separator, or a non-empty basename and
extension. `LICENSE` does not qualify; `./LICENSE` does. The file contents
are irrelevant to this decision.

### Correction to the initial discussion

Inline Markdown document matching is path-based and requires the exact
path (RES-03). Verified: `[report](report)` is broken when only
`report.md` exists; `[report](report.md)` resolves as a document.

This RFC **does not add implicit Markdown extensions**. “Document first,
attachment second” preserves the existing document rules; it does not
mean that an inline `report` target becomes a match for `report.md`.
Adding such matching would be a separate design decision, including its
precedence relative to an actual extensionless `report` file.

### Why inline targets remain exact paths

- **Rendering compatibility.** Ordinary Markdown relative links point to
  the written path; GitHub does not automatically rewrite `report` to
  `report.md`. Downlint should not accept a link solely by guessing an
  extension when that link may fail when clicked in the rendered README.
- **Unambiguous filenames.** If both `report` and `report.md` exist,
  `[report](report)` identifies `report`. No inferred-extension precedence
  policy is needed.
- **Reliable diagnostics.** Inferring `.md` could hide a missing extension
  in the authored link instead of reporting it.
- **Distinct link models.** Inline Markdown links identify paths; wiki
  links identify notes and retain their existing stem/title matching.
  Thus `[report](report)` targets the file `report`,
  `[report](report.md)` targets the document `report.md`, and `[[report]]`
  can continue to match the note `report.md`.

“Document first, attachment second” classifies an existing exact-path
match correctly: an indexed Markdown file is a document with heading
validation; an extensionless file is an attachment. It is not permission
to guess a different filename. “Exact” here concerns the filename and
extension; existing decoding, normalization, and document case matching
remain unchanged.

Extension inference for a publishing system that explicitly supports it
could be considered separately, but should not be the default.

### Related bug: opening an attachment clears the warning incorrectly

Verified against the current LSP: `index.md` contains
`[license](LICENSE)`, and an existing plain-text `LICENSE` contains
Markdown-looking text such as `[missing](missing)`. Initially the inline
license link emits `link/broken`. Sending `textDocument/didOpen` for
`LICENSE`, even with `languageId: "plaintext"`, clears that warning and
produces Markdown link diagnostics inside `LICENSE`.

The editor upsert path (`upsert_from_editor` /
`freshness::upsert_workspace_doc`) does not check document extension
eligibility. It inserts the attachment into `workspace.folder.documents`,
and `ResolveInput::from_workspace` parses every entry as Markdown. The
warning disappears because the attachment became a document, not because
attachment resolution worked. This is unintended and must be fixed in
this RFC, not retained as an alternate way for `LICENSE` to resolve.

After implementation, an existing `LICENSE` link should resolve as an
attachment before and after opening the file, without parsing its
contents. Opening an unsaved, not-yet-existing extensionless buffer must
not make that filesystem target resolve either.

### Related bug: attachment rename rewrites unrelated wiki links

Verified with an extensionless `report` file and this source:

```markdown
[report](./report)
[[report]]
```

A dry-run attachment rename `report` → `topic` currently rewrites both
links. Only the inline link resolves to the attachment; the bare wiki
link is unresolved when no matching note exists.

The attachment planner first selects source documents containing a
resolved attachment reference, then `edits_for_reference` scans their
whole CST. Its wiki edits are not restricted to the specific occurrence
that resolved to the attachment; inline edits also use permissive
filename matching. Document-level selection is not sufficient proof
that each link points at the renamed file.

Under wiki rules, `[[report]]` refers to a matching note such as
`report.md`, not the extensionless attachment `report`. If `report.md`
also exists, that wiki reference points at a different file. Renaming
only the attachment must leave it unchanged in either case. `topic.md`
would be the note sought by `[[topic]]`; renaming the attachment to
`topic` is not a reason to change the note identifier.

The required fix is per-occurrence destination-safe attachment rename,
not any change to wiki matching semantics.

Rename must also preserve attachment path semantics. Verified: renaming
`report` to `topic` currently rewrites `[[./report]]` to `[[topic]]`,
which becomes broken when only extensionless `topic` exists, or may
resolve to the wrong file if `topic.md` exists. It must instead retain an
explicit attachment path such as `[[./topic]]`.

Reference-definition-only documents must be included too:

```markdown
[report][ref]

[ref]: report
```

Their usages resolve to definitions, not attachment destinations in the
graph. Selecting source documents only through resolved inline/wiki
attachment references can miss this document entirely. Definition URLs
need independent exact-path checks across source documents.

## 3. Proposed rule

For `Ref::Inline`, including both links and images:

1. Keep existing routing for external web schemes, configured URI
   mappings, in-page anchors, and folder links. They must not be sent to
   this new fallback.
2. Run existing document resolution unchanged. One matching document
   resolves as today; multiple matches remain `link/ambiguous`.
3. Only when zero documents match and the document part of the target is
   non-empty, allow attachment fallback regardless of the target's shape.
4. Resolve the exact target with the existing path helper and Markdown
   resolution base (RES-02): a basename is source-directory-relative;
   a leading `/` is workspace-root-relative. Preserve existing decoding
   and path normalization. Do not search other folders, append an
   extension, or perform stem, title, prefix, or case-folded filesystem
   searches.
5. Every local attachment fallback requires `is_file()`, not `exists()`,
   including previously eligible path-shaped targets. Directories and
   special filesystem entries do not become attachments. Symlinks to
   files follow existing filesystem metadata behavior; dangling symlinks
   do not resolve.
6. A matching file resolves as the existing `DestinationKind::Attachment`.
   No new destination kind, diagnostic code, or configuration is needed.
7. If the exact fallback path is a directory but the target does not end
   in `/`, report `link/broken` with a trailing-slash hint as specified
   below. Do not silently resolve it as either an attachment or directory.
8. Otherwise keep existing unresolved-link behavior, including inline
   warning severity and suppression of missing-file links in
   single-explicit-file mode. A confirmed directory syntax mismatch is
   diagnosed in that mode too; it is not a speculative missing-file link.

Folder links with a trailing slash continue to use RES-04. A trailing `/`
requires an existing directory; a file at that path is not a directory.

For wiki links and wiki embeds, keep the existing attachment-candidate
predicate and all note matching rules unchanged. In particular, bare
`[[LICENSE]]` does not gain an extensionless-file fallback, while
`[[./LICENSE]]` retains its existing file behavior. Eligible wiki
attachment paths and the wiki-style resolution query also use file-only
checks: directories without a trailing `/` no longer qualify as
attachments. Bare wiki note matching retains priority and is not replaced
by directory lookup.

### Directory diagnostics and compatibility break

For inline targets, and wiki targets eligible for attachment fallback,
when document matching returned zero candidates and the exact resolved
path is a directory, report the existing `link/broken` diagnostic with a
message identifying the directory mismatch and a hint such as:

```text
Target './report' is a directory; directory links require a trailing '/'
Hint: use './report/' to link to this directory.
```

This is an invalid-link condition, not a new diagnostic code or severity
policy: inline Markdown retains Warning and wiki retains Error. The hint
preserves the authored path spelling, inserting `/` before any fragment.
Do not emit it for a missing path, a file, a rejected URI scheme, or a
bare wiki note identifier that was not eligible for attachment fallback.
If a fragment is present, explain that directory links do not support
anchors; adding `/` alone does not make that anchored link valid.

This deliberately breaks compatibility for directory targets previously
accepted by `exists()`, including `./report`, `/report`, `docs/report`,
and directory names that resemble files such as `report.md`. They must
be written with a trailing `/`. File links keep their existing path
semantics, and no opt-in restores the old directory-as-attachment behavior.

The wiki-style `link resolve` query must likewise return `broken` for
such directory targets and show the trailing-slash guidance in text
output, without introducing a Markdown mode or broadening bare wiki
attachment eligibility.

### Attachment semantics

Extensionless files are ordinary attachments, not documents to parse.
Their contents are not inspected for Markdown, titles, or headings.
Document indexing filters (hidden files, gitignore, `core.ignore`) do not
filter exact attachment checks, consistent with RES-06/RES-10.

A fragment on an attachment follows existing local attachment behavior;
this RFC does not add fragment validation or a new anchor diagnostic.
The normal document-heading validation path remains unchanged.

Full/collapsed reference usages continue to resolve to their link
definitions, not to the filesystem destination in that definition. The
current symbol model does not emit an inline reference for a definition
URL. Adding lint diagnostics or graph filesystem destinations for
reference-link definitions is a separate parser/resolution change and
is not part of this RFC. This does not exclude the independent exact-path
checks required for definition URL rename below. Shortcuts and undefined
labels likewise keep their current behavior.

### Required LSP classification fix

- Editor `didOpen` / `didChange` must insert or update Markdown documents
  only when the path's extension belongs to the effective
  `core.file_extensions`, consistent with normal document discovery.
  An arbitrary editor language ID must not promote a non-document path.
- Non-document open buffers must not enter the resolution document set,
  be parsed as Markdown, supply note/heading matches, or receive Markdown
  diagnostics. Editor close and disk reconciliation must preserve this
  boundary as well. Open-buffer bookkeeping alone is not document
  classification.
- Existing plain files resolve only through the filesystem attachment
  fallback. Unsaved attachment text is not evidence that a file exists.
- Preserve configured Markdown extensions and never-saved Markdown
  document upserts from RFC 0022. This fix must not introduce a blanket
  “must exist on disk” requirement for Markdown editor buffers, or change
  hidden/ignore handling for otherwise eligible Markdown editor buffers.

### Required attachment rename fix

- Select edits by the specific reference occurrence and its resolved
  destination, not merely by the containing document or file basename.
  A link is eligible only when it resolves to the attachment being moved.
- Preserve unresolved links, ambiguous links, links to other documents,
  and same-named attachments in other directories.
- In the example above, rename the target of `[report](./report)` to the
  exact path for `topic`, and leave `[[report]]` unchanged whether
  `report.md` exists or not. Existing path rendering may emit `topic`
  rather than `./topic`; both identify the intended file. Preserving the
  literal `./` spelling is not a new requirement.
- Every rewritten link must resolve to the moved file under that link
  syntax's rules after the planned edits and file move. Selecting the
  right occurrence is not sufficient if the replacement changes its
  interpretation.
- An explicit wiki attachment reference such as `[[./report]]` must remain
  a path-shaped attachment reference, e.g. `[[./topic]]`, not `[[topic]]`.
  Preserve or generate the necessary path prefix and any file extension;
  do not apply Markdown-note extension stripping to attachments. Verify
  this both with and without a same-named `topic.md` document. Respect
  source-relative versus root-relative wiki path bases for moved files.
- Preserve fragments and URL encoding under existing rename rules, and
  ensure each occurrence is edited at most once.
- Check reference-definition URLs independently across all source
  documents, including documents with no inline/wiki attachment
  references. Resolve each definition URL with exact Markdown path
  semantics and rewrite its URL only if it points at the moved file.
  Preserve the reference label and its usages. This rename validation
  does not add lint diagnostics for definition URLs or filesystem
  destinations to their label-reference graph edges.
- A definition-only document containing `[report][ref]` and
  `[ref]: report` must become `[ref]: topic` when the attachment moves;
  another inline link must not be required to make that document eligible.

## 4. Behavior matrix

Unless stated otherwise, files are alongside the source document, and
checks use a workspace or stdin rather than single-explicit-file mode.

| Case | Before | Proposed |
|---|---|---|
| `[Apache License 2.0](LICENSE)`, file `LICENSE` exists | warning | attachment, no warning |
| `[report](report)`, extensionless file `report` exists | warning | attachment, no warning |
| `[report](report)`, only `report.md` exists | warning | warning (no implicit `.md`) |
| `[report](report)`, both `report` and `report.md` exist | warning | attachment `report`; `report.md` is not an inline match |
| `[report](report.md)`, indexed `report.md` exists | document | document, unchanged |
| `[report](report.md#missing)`, document exists without heading | broken anchor | broken anchor, unchanged; no attachment retry |
| `[missing](missing)`, no matching file | warning | warning, unchanged |
| `[image](image.png)`, file exists | attachment | attachment, unchanged |
| `[license](./LICENSE)` or `[license](docs/LICENSE)`, file exists | attachment | attachment, unchanged |
| `[report](report)`, `report` is only a directory | warning | broken + trailing-slash hint |
| `[report](./report)`, `report` is only a directory | attachment | broken + trailing-slash hint (breaking change) |
| `[report](report/)` or `[report](./report/)`, directory exists | directory | directory, unchanged |
| `[report](report/)`, only file `report` exists | broken | broken, unchanged |
| `[[./report]]`, `report` is only a directory | attachment | broken + trailing-slash hint (breaking change) |
| `[[./report/]]`, directory exists | directory | directory, unchanged |
| `![image](report)`, extensionless file exists | warning | attachment |
| `[report](report#section)`, extensionless file exists | warning | attachment; fragment is not validated, as for existing local attachments |
| `[[report]]`, only extensionless `report` exists | broken | broken, unchanged |
| `[[report]]`, `report.md` exists | document | document, unchanged |
| `[[./report]]`, extensionless file exists | attachment | attachment, unchanged |
| `[web](https://example.com)` | skipped | skipped, unchanged |

From `docs/index.md`, `[report](report)` checks `docs/report`, not a
workspace-root `report`. `[report](/report)` retains the workspace-root
base. Stdin remains rooted at the workspace root. No new mount search or
virtual-prefix attachment mapping is introduced.

For directory `report`, both `report` and `./report` are invalid file
links; use `report/` or `./report/`. Both trailing-slash spellings resolve
as `Directory`, never as `Attachment`. The compatibility break removes
rather than preserves the old path-spelling exception.

## 5. Alternatives considered

1. **Require `./LICENSE`.** This is a valid workaround but imposes a
   downlint-specific spelling on a normal relative Markdown link.
2. **Allowlist `LICENSE`, `NOTICE`, and `COPYING`.** Rejected: attachment
   support is already extension-agnostic, and arbitrary extensionless
   filenames are equally valid. The file's existence, not its name,
   should determine whether the link is broken.
3. **Remove the predicate for wiki links too.** Rejected for this RFC:
   wiki targets are note identifiers as well as paths. Expanding their
   fallback would change existing note-resolution behavior unnecessarily.
4. **Prefer the filesystem over document matching.** Rejected: would
   turn indexed Markdown documents into attachments and bypass heading
   validation. The existing document-first order remains.
5. **Add implicit `.md` matching at the same time.** Rejected as the
   default: it can accept links that fail in ordinary Markdown rendering,
   hide missing extensions, and require a collision policy for `report`
   versus `report.md`. A publishing-system-specific feature would be a
   separate change to RES-03, not necessary to fix `LICENSE`.
6. **Keep `exists()` for old attachment spellings.** Rejected: preserving
   `(./report)` as accepted for a directory while rejecting `(report)`
   makes equivalent paths behave differently. Require `is_file()` for
   all local attachment fallbacks and `/` for directories, accepting the
   compatibility break with actionable migration hints.

## 6. Implementation boundaries

- `src/resolution/mod.rs`: make fallback eligibility reference-kind-aware
  in `finalize_doc_or_attachment`. Retain the old predicate for wiki
  references; allow non-empty inline targets. Use `is_file()` for all
  local attachment candidates and distinguish an existing directory to
  produce the trailing-slash diagnostic. Keep path resolution in the
  existing helper; do not route web/schema targets into local fallback.
- `src/diagnostics/rules.rs` and unresolved-reference metadata: carry the
  directory mismatch and suggested spelling to the existing broken-link
  diagnostic without conflating it with prefix or URI hints. Ensure the
  known-directory diagnostic is not lost to missing-file suppression.
- `src/resolution/query.rs` and CLI query rendering: retain wiki-style
  matching, eligibility, and path bases, but use file-only attachment
  checks and surface directory guidance for eligible directory paths.
  Do **not** broaden fallback to all bare targets; a Markdown query mode
  would require a separate surface decision.
- CLI `check` and stdin inherit the fallback through shared resolution.
  Graph and navigation consumers should see the normal attachment
  destination rather than a new special category.
- `src/lsp/mod.rs` and `src/lsp/freshness.rs`: guard editor document upserts
  using the effective configured document extensions and preserve the
  boundary on change/close/reconciliation. Keep attachment buffer text
  out of the Markdown document set; preserve unsaved Markdown support.
- `src/rename/attachment.rs` and the shared edit helpers in
  `src/rename/file.rs`: replace document-wide permissive rewriting for
  attachment moves with per-occurrence destination checks. Correlate
  graph occurrences with source elements through symbols and their
  `ast_idx` / source ranges. Symbol occurrence IDs and scanner CST node
  IDs are assigned differently: do not assume numeric equality or that
  an AST index directly indexes the CST (frontmatter is omitted from AST).
  Preserve attachment path semantics in replacement text. Independently
  scan definition URLs across source documents with exact path validation,
  rather than limiting them to graph-selected documents. If shared helpers
  change, verify existing Markdown-file rename behavior remains intact.
- No eager indexing or parsing of all non-Markdown files, new completion
  source, configuration, release bump, or diagnostic code.

## 7. Planned spec changes

Apply these **with implementation**, not while this RFC is Proposed:

- RES-06: distinguish inline fallback (any non-empty local target) from
  wiki fallback (the existing attachment-candidate predicate). Retain
  the document-first order and exact path requirement. All local
  attachment checks require files, not directories.
- RES-04 and `link/broken`: require trailing `/` for local directory
  links and document the directory-mismatch hint, severity, fragment
  caveat, and applicability in single-explicit-file mode.
- RES-03: explicitly document the bare inline target example showing
  that `.md` is not inferred; no matching-rule change.
- LNK-02 / diagnostic conditions: clarify that inline local file targets
  need not have an extension or explicit path prefix.
- `spec/downlint.md`: clarify the wiki-style semantics of `link resolve`
  where necessary, so it is not advertised as predicting every inline
  Markdown link. Record the attachment fallback expansion in the living
  product spec.
- LSP document classification: document the extension-based boundary for
  editor buffers, consistent with the existing definition of a document.
  Opening an attachment must not promote it into a document.
- `spec/downlint.md` rename contract: attachment moves rewrite only
  references to the moved attachment and preserve their destination after
  the move, including explicit wiki attachment paths and independently
  checked reference-definition URLs in definition-only documents.
- Record the directory compatibility break and trailing-slash migration
  in the release notes when implementation is released.
- Add the verification tests and amendment entry to `spec/linting.md`.

## 8. Verification plan

Use `tests/common/mod.rs::write_vault`. Follow RFC 0020 vocabulary:
`report`, `index`, `missing`, and `image.png`; retain `LICENSE` only in the
explicit regression test for the reported real-world README pattern.

1. Integration resolution tests: existing bare `LICENSE` and `report`
   resolve as attachments, not documents; missing `missing` retains
   `link/broken` with warning severity.
2. Source-base tests: nested source resolves its sibling `report`; a
   root-only file is not a fallback for a nested bare link.
3. Precedence/regression tests: `report.md` still resolves as a document
   with heading validation; bare `report` with only `report.md` remains
   broken; both files present resolves inline `report` as an attachment.
4. Syntax coverage: inline image; confirm existing attachment-fragment
   behavior is preserved. Add a reference-link regression to confirm
   label resolution is unchanged, without asserting new lint validation
   of the definition URL. Definition URL rename has separate tests below.
5. Filesystem/directory cases: an existing directory yields `link/broken`
   and a trailing-slash hint for inline `report`, `./report`, `/report`,
   `docs/report`, and a file-like directory name such as `report.md`.
   Hint only when the exact resolved path is a directory; test a file,
   missing path, and rejected URI target for absence of the hint. Both
   `report/` and `./report/` resolve as directories; a file with trailing
   slash is broken. Cover anchored directory links and known-directory
   diagnostics in single-explicit-file mode. Symlink-to-file,
   symlink-to-directory, and dangling-symlink cases use platform-appropriate
   tests. Hidden/ignored files remain eligible for exact attachment checks.
6. Wiki/query regressions: bare wiki references to extensionless files
   remain broken and note matching is unchanged. Explicit wiki file
   attachments still resolve; eligible directory paths without `/` become
   broken with guidance, and trailing-slash directories still resolve.
   The wiki-style CLI query reflects the same file/directory distinction
   without acquiring bare extensionless attachment fallback.
7. CLI workspace and stdin regressions: the README link produces no
   diagnostic when the file exists; missing targets still warn. Do not
   rely on single-explicit-file mode, which suppresses unresolved file
   links today.
8. LSP classification regressions: an existing `LICENSE` stays an
   attachment across `didOpen`, `didChange`, and `didClose`, even when its
   text contains Markdown links or headings. Test plaintext and Markdown
   language IDs on a non-document path: neither promotes it. Assert no
   document/heading destinations or Markdown diagnostics for the plain
   file, and no new bare wiki match from opening it. A not-yet-existing
   extensionless buffer must leave inline attachment links broken.
   Preserve unsaved `.md` and configured-extension document behavior.
   Verify create/delete diagnostics refresh through existing attachment
   mechanisms; unrelated freshness gaps should be recorded separately.
9. Attachment rename regressions: with extensionless `report` and both
   `[report](./report)` and `[[report]]` in `index.md`, moving `report` to
   `topic` rewrites only the inline attachment link. Repeat with
   `report.md` present to verify its wiki reference remains unchanged.
   Test same-named files in different directories, unresolved/ambiguous
   links in the selected source, fragments/encoding, and individually
   checked definition URLs. Require `[[./report]]` → `[[./topic]]` (or an
   equivalent explicit path), never bare `[[topic]]`; test with `topic.md`
   present and absent, extension changes within attachment class, and
   cross-directory moves under wiki path-base rules. A document containing
   only `[report][ref]` and `[ref]: report` must receive the definition URL
   edit without an inline/wiki attachment reference; its labels stay
   unchanged, and definitions for other files stay untouched. Include
   frontmatter/headings to detect symbol/CST ID mismatches. Assert exact
   edits, no duplicate/overlapping edits, and destinations after applying
   the plan. Preserve Markdown-file rename tests. Graph/navigation smoke
   tests should see attachment destinations.
10. Full `cargo test` and `cargo clippy --all-targets`; no new warnings.

## 9. Review questions

- Exact filenames, including extensionless files, are the proposed
  default for inline links. Any future publishing-system-specific
  extension inference needs a separate RFC rather than expanding this
  fallback.
- Should `downlint link resolve` eventually expose an explicit Markdown
  mode? Its wiki-style matching and path bases remain unchanged here;
  only its file/directory fallback distinction changes.
- Directory spelling is settled: all local attachment checks require
  files, directory links require trailing `/`, and the compatibility
  break is accepted. URI schema resolution retains its separate rules.
