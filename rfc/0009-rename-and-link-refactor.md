# RFC: Rename & Link Refactor

## Status

Accepted (with minor clarifications added during review)

## Motivation

Downlint's whole reason to exist is to keep wiki-links valid as a knowledge base evolves. Today
it can *detect* broken links (`DNL002`) and tolerate some via `obsidian_prefix` (RFC 0004), but
it cannot *fix* them. Three operations are impossible today:

1. **Rename a file on disk** (markdown or attachment — image, PDF, XLSX, anything)
   and rewrite every wiki-link / markdown-link reference that points at it.
2. **Rename a heading** and rewrite every `[[doc#heading]]` and `[text](doc.md#heading)`
   anchor reference.
3. **Rename a logical link identifier** — text-only rewrite across the workspace, no
   disk move. The CLI surfaces this; the LSP exposes it via a code action.

`spec.md` §8 describes a rename design — `textDocument/rename`, `prepareRename`, and
`workspace/didRenameFiles` are listed as implemented in the capability matrix (lines 193,
195, 209). They are not. The current `ServerState` in `src/lsp/mod.rs` advertises only
`completionProvider`, `hoverProvider`, `definitionProvider`, `referencesProvider`, and
`documentSymbolProvider`. A `grep -rn 'rename\|Rename' src/` returns matches only in
`config/mod.rs` (serde renames), `config/uri.rs` (RFC 0008 vocabulary rename), and
`diagnostics/mod.rs` (the `DNL002` code constant). No rename handler exists.

A **CLI** is also needed from v1: scripted vault reorganization, CI validation, and
non-LSP editors (Vim, scripted refactors) all benefit from a batch-rename command that
shares the rename library with the LSP.

This RFC closes the gap on all three operations and ships the CLI alongside the LSP
support.

## Goals

- Make rename work for the three element types above, including propagation across all
  referencing documents in the workspace (including `extra_folders`).
- The CLI `rename-file` accepts **any file on disk** — markdown or attachment. The
  kind-class (markdown vs attachment) is inferred from the source extension, so users
  don't have to think about it.
- Provide a CLI subcommand that exposes the same operations atomically.
- Define a clear blocking rule: when any rewritten document has a `Warning`/`Error`
  diagnostic (other than on the occurrences being rewritten), the rename is refused
  until the user fixes the underlying problem.
- Define conflict detection rules that treat on-disk collisions and resolution-graph
  collisions uniformly.
- Keep the LSP wire contract honest — advertise only what we actually implement.

## Non-Goals (v1)

- `workspace/willRenameFiles` — deferred. `didRenameFiles` is enough for v1 because the
  editor is the source of truth for the edit; downlint only needs to fix up its indexes.
- Rename of markdown reference labels (`[id]: url` / `[text][id]`) — deferred. Rare in
  modern Markdown, low payoff, and orthogonal to the file/heading rename path.
- Extension-class-changing renames (e.g. `report.md` → `report.pdf`). Rejected by the
  extension-class preservation rule with a clear error.
- Frontmatter path references (e.g. `cover: image.png` in YAML).
- CLI subcommand for heading rename — headings are editor-driven changes; no script
  use case justifies a CLI command.

## Element Types

The LSP distinguishes four internal kinds (because the `WorkspaceEdit` and the
`prepareRename` range differ). The CLI exposes two subcommands — `rename-file` and
`rename-link` — because at the CLI surface the markdown-vs-attachment distinction is
hidden behind a single inference step in `rename-file`.

### LSP Type A — Attachment (`A`)

A wiki-link whose target resolves to a non-markdown file (image, PDF, XLSX, anything
resolved via `finalize_doc_or_attachment` to `DestinationKind::Attachment`). Synonym of
spec.md §8.1's "Attachment Rename".

### LSP Type H — Heading (`H`)

A heading (H1..H6). Synonym of spec.md §8.1's "Heading Rename". **LSP only** — no
CLI subcommand, because headings are editor-driven changes with no script use case.

### LSP Type F — Markdown File (`F`)

A `.md`/`.markdown`/`.mdx` file on disk. New in this RFC.

LSP types A and F are **collapsed** at the CLI surface into a single
`downlint rename-file` subcommand. The CLI infers which internal type applies from
the source file's extension at parse time; the user does not pick.

### LSP Type L — Link Target String (`L`)

A textual identifier used as a link target at the cursor. The LSP
`textDocument/rename` handler operates on this kind (string-only, never moves files).
The CLI equivalent is `downlint rename-link`.

## LSP Wire Surface

### Capabilities to advertise

Add to the `initialize` response in `src/lsp/mod.rs`:

```json
{
    "renameProvider": { "prepareProvider": true },
    "codeActionProvider": {
        "codeActionKinds": [
            "refactor.rename.file",
            "refactor.rename.link-target",
            "refactor.rename.heading"
        ]
    },
    "workspace": {
        "fileOperations": {
            "didRename": {
                "filters": [
                    { "pattern": { "glob": "**/*.{md,markdown,mdx}" } },
                    { "pattern": { "glob": "**/*" } }
                ]
            }
        }
    }
}
```

- `renameProvider` with `prepareProvider: true` — advertises the safe string-rename
  gesture for F2 / equivalent editor shortcuts. **Only operates on the string under
  the cursor; never moves files.**
- `codeActionProvider` with three kinds — surfaces file-rename (heavy machinery:
  blocking rule, conflict checks), link-target-rename (workspace-wide string rewrite
  with safety checks), and heading-rename (slug recomputation in link anchors).
- `didRename` filters cover markdown files plus a catch-all so attachments also
  notify the server.
- `didCreate` and `didDelete` are out of scope for this RFC.
- `willRename` is not advertised (see Non-Goals).

### Methods

| Method | Operation | CLI equivalent |
|---|---|---|
| `textDocument/prepareRename` | Returns the editable range for a string at the cursor (link target, heading text). Returns `null` if nothing renameable. | n/a (internal) |
| `textDocument/rename` | Renames the string at the prepared range. **Never moves files.** Single `TextEdit`. | `downlint rename-link` (single-occurrence subset) |
| `textDocument/codeAction` kind `refactor.rename.file` | Invokes the file-rename action — moves a file on disk, propagates to all references, runs conflict + blocking checks. | `downlint rename-file` |
| `textDocument/codeAction` kind `refactor.rename.link-target` | Invokes the link-target action — workspace-wide string rewrite with conflict + blocking checks. | `downlint rename-link` |
| `textDocument/codeAction` kind `refactor.rename.heading` | Invokes the heading-rename action — recomputes slug, rewrites `#section` in all referencing links. | LSP only (no CLI subcommand) |
| `workspace/didRenameFiles` | Reacts after a file rename, rebuilds indexes, emits fresh diagnostics. | (editor-only) |

### Why `textDocument/rename` is string-only

The cursor on `[[report]]` is fundamentally ambiguous:

- It could be a filename stem (resolves to `report.md`).
- It could be an H1 title (resolves to the title of `report.md` or another doc).
- It could be a prefix of multiple files (`report.md`, `report-2024.md`,
  `report-2025.md` with `obsidian_prefix = true`).
- It could be unresolved (no file matches at all, `DNL002`).

The LSP wire has no way to ask the user "which interpretation did you mean?" before
honoring `textDocument/rename`. If F2 silently moves files, the user can corrupt
their vault with one keystroke.

**Decision**: `textDocument/rename` is the **safe default** — it renames the string
under the cursor and never moves files. File moves are surfaced as code actions
where the user picks the operation from a menu.

### Code Actions

When the LSP client requests code actions at a cursor position, downlint offers
actions based on what the cursor resolves to:

**`refactor.rename.file`** — offered when:
- Cursor is inside a wiki-link or markdown-link target whose resolution is a single,
  unambiguous file (Document or Attachment kind, exact stem match, no prefix
  ambiguity).
- The file's extension is in `[core].file_extensions` (Document kind) or any
  non-markdown extension (Attachment kind). Cross-kind renames are not supported.
- Not offered when: the resolution is ambiguous (multiple prefix matches), the link
  is unresolved (`DNL002`), the resolution is a heading, or the file is a heading's
  anchor target.

The action prompts for the new file path/name and runs the full safety machinery:
extension-class preservation, exact-path and prefix-path collision checks, blocking
rule across rewritten documents, atomic text-first-then-disk application.

**`refactor.rename.link-target`** — offered when:
- Cursor is inside any link target string (wiki-link or markdown-link), regardless
  of whether the link is resolved, unresolved, or ambiguous. Even `DNL002` links
  get this action — useful for fixing typos that caused the broken link.
- Always offered on the target portion of any link. Not offered on the `#heading`
  portion (use `refactor.rename.heading` for that).

The action prompts for the new target string and runs the Type L logic:
resolution-conflict check (new target must not resolve to any file), blocking rule,
and per-occurrence rewrite.

**What is `refactor.rename.link-target` for?**

This code action lets you rename a *logical identifier* across the entire workspace.
Example: you have `[[report]]` scattered across 12 documents, and you want to
rename the identifier to `[[quarterly-report]]`. The file `report.md` does not move
on disk — only the link target strings change. This is distinct from `refactor.rename.file`
which moves the file on disk AND rewrites references.

Think of it as "rename this symbol everywhere" — the LSP equivalent of a global
find-and-replace with safety checks (conflict detection, blocking rule). It's also
useful for fixing broken links: if `[[repotr]]` is a typo and resolves to nothing,
you can invoke this action to rename it to `[[report]]` across all documents at once.

**`refactor.rename.heading`** — offered when:
- Cursor is on a heading text line (e.g., `## Methods` in source).
- The heading slug does not already collide with another heading in the same document.

The action prompts for the new heading text, computes the new slug, rewrites all
`#section` portions in referencing links across the workspace, and updates the
heading text in the source document. LSP only — no CLI subcommand (headings are
editor-driven changes; you'd never run `downlint rename-heading` from a script).

**Why both `textDocument/rename` and `refactor.rename.link-target`?**

They do the same operation (string rename at the cursor) but surface through
different UI paths:

- `textDocument/rename` is the F2 binding. Users with muscle memory hit F2 and get
  the string rename without thinking.
- `refactor.rename.link-target` is the command-palette entry. Users who search for
  "rename" find it explicitly.

Both are safe. The redundancy is intentional and helps discoverability without
adding complexity.

## Matching Cursor → `textDocument/rename`

The `textDocument/rename` handler receives a cursor offset and a string range from
`prepareRename`. It returns a single `TextEdit` replacing the string at that range.
**It does not consult the connection graph** — the resolution of the link is
irrelevant to this handler.

`prepareRename` returns a non-null range when the cursor is on:

- A link target string (the portion of `[[target]]` or `[text](target)` between the
  brackets/parens, excluding `|alias` and `#heading`).
- A heading text occurrence in source.

`prepareRename` returns `null` when the cursor is on plain prose, code spans, link
display text (`[text]`), frontmatter, or any other position where a string rename
doesn't make sense.

The handler does no resolution, no conflict checks, no blocking-rule evaluation. It
is a pure string-replace operation. All safety machinery lives in the code actions.

### Preservation rules

The rename replaces the target/URL portion only; surrounding syntax is preserved
verbatim:

| Cursor position | Old | New |
|---|---|---|
| Wiki-link target | `[[title]]` | `[[title2]]` |
| Wiki-link with alias | `[[title\|alias]]` | `[[title2\|alias]]` |
| Wiki-link with heading anchor | `[[title#head]]` | `[[title2#head]]` |
| Wiki-link with both | `[[title#head\|alias]]` | `[[title2#head\|alias]]` |
| Markdown-link URL | `[t](title.md)` | `[t](title2.md)` |
| Markdown-link URL with anchor | `[t](title.md#head)` | `[t](title2.md#head)` |
| Heading text | `# Heading Text` | `# New Heading Text` |

These apply regardless of resolution state — a broken link, ambiguous link, or
title-resolved link all behave the same. The resolver is run on the new string
*after* the rename; whatever it resolves to (or fails to resolve to) is the user's
next problem, not the rename's.

## Type A — Attachment Rename

**Invocation**: `downlint rename-file` (CLI) or `textDocument/codeAction` kind
`refactor.rename.file` (LSP).

**Procedure**:

1. Build the `ConnectionGraph` for the workspace + `extra_folders`.
2. Collect every occurrence that resolves to the source file (Attachment destinations).
3. Run conflict checks (see below). On conflict, reject.
4. Run the blocking rule. On block, reject.
5. Apply: write text edits to every rewritten document, then move the file on disk
   (text first so a partial disk failure leaves consistent state).

**Conflict detection** (downlint-owned, not delegated to the editor):

- **Exact path collision**: the new on-disk path must not already exist as a different
  file. If it does, the rename is rejected.
- **Prefix collision**: with `obsidian_prefix = true`, the new stem must not be a
  leading prefix of any other file in the workspace. If it is, reject. (Example:
  renaming `report-2024.pdf` → `report.pdf` while `report-2025.pdf` exists would create
  a `DNL001` ambiguity for `[[report.pdf]]` — reject.)
- **Extension-class preservation**: the rename must preserve the file's extension
  class (attachment ↔ attachment). A `.pdf` → `.docx` rename is fine (both attachments);
  a `.pdf` → `.md` rename is rejected.

## Type H — Heading Rename

**LSP only.** No CLI subcommand — headings are editor-driven changes with no script
use case. The `refactor.rename.heading` code action is the sole interface.

`spec.md` §8.1 distinguishes H1 from H2+ in its propagation rule. The distinction adds
no value at the rename layer — the propagation is the same: walk the graph, update every
referencing link's `#section` slug. H1 and H2+ rename identically.

**Validation**:
- No `\n`, no `#` in `newName`.
- The new heading text produces a slug that does not collide with an existing slug in
  the same document. On collision, reject with `MethodFailed`.

**Slug update**: when the heading text changes, every link target's `#section` slug also
changes. Downlint computes the new slug from the new heading text and emits a `TextEdit`
replacing only the `#section` portion of every referencing link — not the entire link.

**Scope**: The rename operates on the heading's `#section` slug in link targets. It
does NOT:
- Rewrite the heading text in the source document's frontmatter (if any).
- Auto-disambiguate slug collisions (rejects with `MethodFailed`).
- Handle the case where the heading is deleted (tracked as future work).

**H1 title side-effect**: if `[core] title_from_heading = true` and the renamed heading
is the document's H1, the document's title changes too. This affects resolution of
title-only wiki-links `[[|Title]]` and any frontmatter `title:` aliases that get
re-slugged. Downlint re-runs the resolver for affected documents after the rename and
emits fresh diagnostics; it does not auto-edit title-only links (title resolution is
intentionally tolerant and not a rename target).

## Type F — Markdown File Rename

**Invocation**: `downlint rename-file` (CLI) or `textDocument/codeAction` kind
`refactor.rename.file` (LSP).

**Procedure**:

1. Build the `ConnectionGraph` for the workspace + `extra_folders`.
2. Collect every occurrence that resolves to the source file (Document destinations).
3. Run conflict checks. On conflict, reject.
4. Run the blocking rule. On block, reject.
5. Apply: write text edits to every rewritten document, then move the file on disk.

The LSP `codeAction/resolve` returns a `WorkspaceEdit` containing both the file
rename (as a `RenameFile` document change) and per-document `TextEdit[]` for every
occurrence that resolves to the file. The editor applies both atomically.

**What counts as "referencing this file"**:

A document `D_ref` references the file at path `P_old` if any of `D_ref`'s occurrences
resolve through the `ConnectionGraph` to a destination whose `path == P_old`:

- Wiki-link `[[stem]]` or `[[path/to/stem]]` whose resolved destination is `P_old`.
- Markdown-link `[text](url)` whose `url` resolves to `P_old`.
- Reference definition `[id]: url` whose `url` resolves to `P_old` (the URL is edited;
  the `[text][id]` references are not — they continue to use the definition).

**Example — reference-link definitions**:

Before renaming `reports/q1.md` → `reports/q1-2024.md`:

```markdown
See [the report][r] for details.
See also [the other report][r2].

[r]: reports/q1.md
[r2]: reports/q1.md "Q1 2024"
```

After:

```markdown
See [the report][r] for details.
See also [the other report][r2].

[r]: reports/q1-2024.md
[r2]: reports/q1-2024.md "Q1 2024"
```

The URLs in the definitions are rewritten. The references in the body are not — they
still use the labels `r` and `r2`, and the definitions still resolve them.

**Conflict detection** (downlint-owned):

- **Exact path collision**: the new on-disk path must not already exist as a different
  file.
- **Prefix collision**: with `obsidian_prefix = true`, the new stem must not be a
  leading prefix of any other file in the workspace, and the new stem must not have
  any *other* file as a leading prefix (the reverse direction — preventing `report.md`
  from being renamed to `report-2024.md` when `report.md` already exists and
  `[[report]]` would suddenly start matching `report-2024.md` only via prefix).
- **Extension-class preservation**: the rename must preserve the source file's
  extension class. A markdown file (extension in `[core].file_extensions`) can only
  be renamed to another markdown file; an attachment can only be renamed to another
  attachment. A `report.md` → `report.pdf` rename is rejected with `MethodFailed`.

**Extension-Class Preservation** (applies to both LSP Type A and Type F):

The source file's class is determined by its extension:

| Source extension | Class | Allowed rename target extensions |
|---|---|---|
| `.md`, `.markdown`, `.mdx`, or anything in `[core].file_extensions` | Markdown | Same set as source |
| Anything else | Attachment | Anything else (any non-markdown extension) |

The CLI infers this at parse time and applies the corresponding conflict check
internally. The LSP `refactor.rename.file` code-action handler does the same based on
the file's actual extension when the cursor resolves to a `Document` or `Attachment`
destination.

**Prefix-resolved links — promoted to full stem**:

If `obsidian_prefix = true` and `[[prefix]]` resolves uniquely to `P_old` (via prefix
match), the rename rewrites the link target to the new full stem. This is a deliberate
**promotion**: a prefix-only reference becomes a full-stem reference. Reasoning: after
the rename, there is no longer any ambiguity — the file's new name is the only thing
the link could point at — so silently widening the target is safe and is what the user
expects when they rename the file. (This reverses the conservative stance in earlier
drafts of this RFC.)

If `obsidian_prefix = false`, the same link is `DNL002` broken before the rename (no
file matches the prefix without the flag). After the rename, the link remains broken
unless the user also types the full new stem. This is the existing DNL002 behavior; the
rename does not change it.

**Cross-folder & `extra_folders`**: references from documents inside `extra_folders` work
the same as references from the primary workspace — the `ConnectionGraph` indexes
everything, and `documents` / `extra_documents` are queried uniformly.

**Path rewriting** for each rewritten occurrence:

| Link form | Edit |
|---|---|
| `[[stem]]` | Replace the entire target range with the new stem |
| `[[path/to/stem]]` | Replace only the trailing segment after the last `/` |
| `[[stem\|alias]]` | Replace only the `stem` portion, preserve the alias verbatim |
| `[[stem#heading]]` | Replace only the `stem` portion, preserve `#heading` |
| `[[stem#heading\|alias]]` | Replace only the `stem` portion, preserve `#heading\|alias` |
| `[text](path.md)` | Replace only the path portion of the URL, preserve the rest of the URL (query/fragment if any) |
| `[text](path.md#head)` | Replace only the `path.md` portion, preserve `#head` |
| `[text][id]` + `[id]: path.md` | Edit the definition URL only; do not touch references |

The new path is computed relative to the referencing document's directory. Cross-subtree
moves produce `../`-laden paths; this is fine, the resolver already handles `..`.

## Type L — Link Target String Rename

**Invocation**:
- `textDocument/rename` (LSP) — string-only, operates on the cursor's string range.
  No resolution, no file moves, no blocking rule.
- `textDocument/codeAction` kind `refactor.rename.link-target` (LSP) — workspace-wide
  rewrite with full safety machinery.
- `downlint rename-link` (CLI) — workspace-wide rewrite.

The string-only `textDocument/rename` variant is a thin wrapper: it does no
resolution, no conflict checks, no blocking-rule evaluation. It just returns one
`TextEdit` replacing the string at the prepared range. This is by design — see "Why
`textDocument/rename` is string-only" above.

The code-action variant and the CLI variant run the full Type L logic:

**Procedure**:

1. Find every occurrence in the workspace where the link target string equals `--from`
   (or the prepared `oldName` for the LSP code action) — exact match, modulo extension
   (`report`, `report.md`, `report.markdown` all match).
2. For each occurrence, resolve it through the `ConnectionGraph`. Keep only occurrences
   whose resolution target is the file `old.md` (the unique exact-stem match for
   `--from`). Drop occurrences that are:
   - Unresolved (no file at all)
   - Ambiguous (multiple matches — with `obsidian_prefix = true`)
   - Title-resolved to a different file
   - Inside code spans / code blocks / frontmatter
3. **Conflict check**: `--to` must not resolve to any file. Specifically:
   - `--to` (with any markdown extension appended) must not be an existing file on disk.
   - With `obsidian_prefix = true`, `--to` must not be a leading prefix of any existing
     file's stem.
   - With `obsidian_prefix = true`, `--to` must not have any other file as a leading
     prefix of it (the reverse direction).
   - `--to` must not match any document's title slug.
4. **Blocking check** (see Blocking Rule below): every rewritten document must have a
   clean diagnostic state — no `Warning`/`Error` outside the rewritten occurrences.
5. Rewrite the target string portion of every kept occurrence (preserving `|alias`,
   `#heading`, etc.) from `--from` to `--to`. Disk is not touched.

This kind is symmetric with Type F: `--from` is a string that resolves to one file, the
rewrite updates every link that resolves to that file, and the *new* string must not
resolve to anything. The pair of operations (rename the file from `report.md` →
`topic.md`, then rename the link from `report` → `topic`) does the same thing in two
steps; the file-rename path is just a shorthand when the file actually moves.

## Precondition: Indexing Not In Progress

**All rename operations (LSP and CLI) require indexing to be complete.** If a rename
is requested while indexing is in progress, the operation is rejected immediately with
a clear message: "Indexing in progress — try again in a moment."

This prevents inconsistent `ConnectionGraph` state from producing incorrect
rewrites. The LSP handler returns `MethodFailed`; the CLI exits with code 3.

## Blocking Rule

**A rename is blocked if any document that would be rewritten contains a `Warning` or
`Error` diagnostic other than on the occurrence(s) being rewritten.**

This rule applies to:
- The `refactor.rename.file` code action.
- The `refactor.rename.link-target` code action.
- The `downlint rename-file` CLI subcommand.
- The `downlint rename-link` CLI subcommand.

It does **not** apply to `textDocument/rename`, which is a pure string-replace
operation with no resolution, no file moves, and no propagation. The string-only
rename cannot introduce broken-link cascades, so the blocking rule is unnecessary.

The rationale: a propagating rename is about to mutate text in a document. If that
document already has diagnostics, the rename would compound existing problems —
propagating broken links, burying ambiguities, or masking data-flow issues. The user
must clean up first.

What counts:

| Severity | Blocks? |
|---|---|
| `Error` (e.g. `DNL002`) | **Yes** |
| `Warning` (e.g. `DNL001`, `DNL005`) | **Yes** |
| `Information` (e.g. `DNL006`, `DNL007`, `DNL008`, `DNL009`) | No |
| `Hint` | No |

Scope: "document that would be rewritten" — every document that contains at least one
occurrence being edited. The blocking check inspects all *other* occurrences in those
documents. An occurrence being rewritten that itself has a diagnostic does not block
itself (we're about to fix it via the rewrite); but other diagnostics in the same
document do block.

**Example**: renaming `report-2024.md` → `yearly-2024.md` with `obsidian_prefix = true`:

- `[d](report-2024)` in `index.md` resolves to `report-2024.md`, no diagnostic. Will be
  rewritten.
- `[d](report)` in `index.md` is `DNL001` ambiguous (resolves to both `report.md` and
  `report-2024.md` via prefix). This is an *unrelated* diagnostic in a document that
  will be rewritten.
- → Rename blocked. User must first resolve `[d](report)` (e.g. change it to
  `[d](report-2024)` explicitly, or fix whatever the intent is), then re-run.

**Counter-example**: renaming `report.md` → `topic.md` with `obsidian_prefix = false`:

- `[d](report)` in `index.md` resolves uniquely to `report.md` (no other file matches
  without the flag). No diagnostic. Will be rewritten.
- `[d](2024-q1)]` in `index.md` is `DNL002` broken, pointing at a non-existent file
  (`2024-q1.md`). This is an unrelated diagnostic in `index.md` which we are rewriting.
- → Rename blocked.

That's the cost of the broad rule: a single unrelated diagnostic anywhere in a document
you're touching blocks. The benefit: documents stay clean, and the user never sees
"rename succeeded but doc is still in a bad state."

When blocked, the error message lists every blocking diagnostic inline:

```
error: rename blocked — 1 occurrence has diagnostic DNL002
  → docs/index.md:42  [[2024-q1]]  Broken link: '2024-q1' could not be resolved
hint: fix the broken reference first, then re-run the rename.
```

## prepareRename

Returns the rename range for the element under the cursor. Because
`textDocument/rename` is string-only, `prepareRename` is also string-only — it does
not consult the connection graph or the file system.

| Cursor position | Range returned |
|---|---|
| Inside a wiki-link target string (between `[[` and `\|`/`#`/`]]`) | The target string's range, excluding `\|alias` and `#heading` |
| Inside a markdown-link URL portion (between `(` and `)`/`#`) | The URL's range, excluding `#anchor` |
| Inside a heading text occurrence | The heading text's range |
| Anything else | `null` |

`null` (rather than a range) is the LSP signal that the cursor is not on a renameable
element. Editors turn this into "no rename available" in the UI.

The code actions do **not** use `prepareRename`. They compute their own availability
based on resolution (see "Code Actions" above).

## workspace/didRenameFiles

When the server receives this notification:

1. Parse the `FileRename` array (old URI → new URI pairs).
2. For each pair, locate the corresponding `ResolvedDocument` in the graph (if any).
3. Update the document's `path` in-place; rebuild only this document's `file_stem`,
   `rel_path`, etc. (cheaper than rebuilding the whole graph).
4. Update every `ResolvedReference.destinations[*].path` that pointed at the old path,
   and every `ResolvedDestination.path` in `AmbiguousReference.destinations`.
5. Re-run diagnostics on the affected source documents.
6. For files that moved *out* of the indexed set (e.g. moved outside `extra_folders`),
   emit `DNL002` on every referencing link.

Performance: O(references to renamed file), not O(whole graph).

## CLI Subcommands

Two new top-level subcommands: `downlint rename-file` and `downlint rename-link`.

### Persistent Server Mode (Agent Optimization)

For agent workflows that perform multiple renames in sequence, repeated re-indexing
is expensive. The CLI supports an optional persistent server mode:

**`downlint server --detach [--port 0]`**

Starts an LSP server in the background for the current project. The server:
- Reads the project root from `.downlint.toml` (or workspace root) and starts indexing
  immediately.
- Listens on a TCP port (default: auto-assign, reported to stdout). Use `--port N`
  for a fixed port.
- Writes a `.downlint/.server.pid` file containing the PID and port for discovery.
  On startup, checks if the recorded PID is still alive; if not, removes the stale
  file and starts fresh.
- Exits after 5 minutes of idle time (no rename requests), or on SIGTERM/SIGINT.
- Supports concurrent rename requests from the same project.
- Uses **file watching** (fsnotify on Unix) to detect external file changes
  (e.g., `mv`, git checkout, another tool) and triggers partial reindexing of
  affected files. This keeps the graph consistent even when files change outside
  the editor or downlint.

**Request / Response Protocol**

The server acts as a **planning service**; the CLI acts as the **execution service**.
When `--server` is used:

1. CLI sends a JSON request to the server with `--from`, `--to`, and operation type
   (`rename-file` or `rename-link`).
2. Server plans the rename using its live graph (no re-indexing cost) and returns
   a `RenamePlan` JSON object containing the list of text edits and file moves.
3. CLI receives the plan and applies it locally (writes text edits, moves files).
4. Before applying, the CLI re-verifies `--from` exists on disk. If it doesn't,
   the CLI fails with: "Workspace changed — the file has moved since planning."

This division of labor means `--server` saves the indexing cost (the expensive part)
but the CLI still does the apply work. For agent workflows doing 10+ renames, the
savings are significant — indexing happens once, and all renames share the graph.

**`downlint rename-file --server`** (and `--server` for `rename-link`)

Detects if a server is running for the current project (by checking `.downlint/.server.pid`)
and uses it instead of building a fresh `ConnectionGraph`. The CLI connects to the server
via TCP, sends a rename request, receives the plan, and applies it locally.

When `--server` is used:
- No re-indexing cost — the graph is already built and maintained by the server.
- Multiple sequential renames share a single indexing cost.
- If no server is detected, the CLI falls back to the default behavior (build graph, do rename, exit).
- If a server is detected but unresponsive, the CLI exits with code 3 and a message:
  "Server unresponsive — try `downlint server --stop` then retry."
- If the server is mid-reindex (after detecting a file change), the CLI exits with
  code 3 and a message: "Server re-indexing — try again in a moment."

**`downlint server --stop`**

Stops the running server for the current project by sending a shutdown request.

**Agent workflow example:**

```bash
# One-time indexing cost
$ downlint server --detach

# Multiple renames — no re-indexing
$ downlint rename-file --from a.md --to b.md --server
$ downlint rename-file --from c.md --to d.md --server
$ downlint rename-link --from old --to new --server

# Clean up
$ downlint server --stop
```

This is an **opt-in optimization**. The default CLI behavior (no `--server` flag) remains
unchanged: build the `ConnectionGraph`, perform the rename, exit. The `--server` flag
is primarily useful for agent workflows and scripted multi-rename operations.

### `downlint rename-file`

Moves a file on disk and rewrites every wiki/markdown link that points at it.
Operates on markdown files and attachments alike; the kind-class is inferred from the
source file's extension.

**Synopsis**:

```
downlint rename-file --from <PATH> --to <PATH>
                      [--dry-run] [--allow-extra-folders] [--config <PATH>]
                      [--server]
```

**Argument semantics**:

| Flag | Meaning |
|---|---|
| `--from` | Workspace-relative path to an existing file (markdown or attachment) |
| `--to` | Workspace-relative path for the new location |
| `--dry-run` | Print the planned edits without applying them; exit 0 if no blocks, 1 otherwise |
| `--allow-extra-folders` | Include `extra_folders` in the search (default true) |
| `--config` | Path to `.downlint.toml` (default: workspace root) |
| `--server` | Use a persistent LSP server for the current project (if running) |

The `--server` flag is described in **Persistent Server Mode** above.

**Procedure**:

1. Verify `--from` resolves to an existing file on disk (any extension).
2. Infer the kind-class from `--from`'s extension: markdown if it's in
   `[core].file_extensions`, otherwise attachment.
3. Build the `ConnectionGraph` for the workspace + `extra_folders`.
4. Collect every occurrence that resolves to `--from` (Type F or Type A resolution
   rules, depending on the inferred kind).
5. Run the **conflict checks** for the inferred kind:
   - Exact path collision.
   - Prefix collision when `obsidian_prefix = true`.
   - Extension-class preservation: the new path's extension must belong to the same
     class as the source (markdown ↔ markdown, attachment ↔ attachment).
   On conflict, print a structured error and exit 2.
6. Run the **blocking rule** over rewritten documents. On block, print the blocking
   diagnostic and exit 1.
7. `--dry-run`: print each edit (`<file>:<line>:<col>  <before> → <after>`) and exit 0.
8. Apply: write text edits to every rewritten document, then move the file on disk
   (in that order — text first so a partial disk failure leaves consistent state).
9. Exit 0 on success.

### `downlint rename-link`

Rewrites a logical link identifier across the workspace. No disk move.

**Synopsis**:

```
downlint rename-link --from <STRING> --to <STRING>
                      [--dry-run] [--allow-extra-folders] [--config <PATH>]
                      [--server]
```

**Argument semantics**:

| Flag | Meaning |
|---|---|
| `--from` | A bare identifier (link target string). No `/`, `#`, `\|`, `(`, `)`, `[`, `]`. |
| `--to` | A new bare identifier, same constraints |
| `--dry-run` | Print the planned edits without applying them |
| `--allow-extra-folders` | Include `extra_folders` in the search (default true) |
| `--config` | Path to `.downlint.toml` (default: workspace root) |
| `--server` | Use a persistent LSP server for the current project (if running) |

The `--server` flag is described in **Persistent Server Mode** above.

**Procedure**:

1. Verify `--from` is a non-empty bare identifier.
2. Build the `ConnectionGraph`.
3. Collect every link occurrence whose target string equals `--from` (modulo extension)
   and whose resolution target is the unique exact-stem file for `--from`. Drop
   occurrences that are unresolved, ambiguous, title-resolved elsewhere, or inside
   masked spans.
4. Run the **conflict checks** for `--to` (must not resolve to any file — exact,
   prefix-if-flag, or title).
5. Run the **blocking rule**.
6. `--dry-run`: print each edit and exit 0.
7. Apply: write text edits only (no disk move). Exit 0 on success.

### Workspace scope (both subcommands)

Both subcommands operate over the **whole workspace + `extra_folders`**, never a
single file. This matches `extra_folders` semantics and is the only way the rename
can be safe (a file rename that doesn't see `extra_folders` references would leave
those broken).

### Exit codes (both subcommands)

| Code | Meaning |
|---|---|
| 0 | Success (or dry-run with no blocks/conflicts) |
| 1 | Blocked by an existing diagnostic (blocking rule fired) |
| 2 | Conflict detected (path collision or resolution collision) |
| 3 | Bad arguments / config error — includes `--from` not found on disk, invalid identifiers, **or indexing in progress** |

### Examples

```
# Rename a markdown file and propagate
$ downlint rename-file --from reports/2024-q1.md --to reports/q1.md

# Rename an attachment (class is inferred from .png extension)
$ downlint rename-file \
    --from assets/diagrams/old-flow.png \
    --to assets/diagrams/new-flow.png

# Rename a logical link identifier (no disk move)
$ downlint rename-link --from report --to topic

# Preview without applying
$ downlint rename-file --from old.md --to new.md --dry-run
```

## Shared Rename Library

The code-action handlers, the `workspace/didRenameFiles` handler, and the CLI
subcommands all call into a single library crate `src/rename/mod.rs`. The
`textDocument/rename` handler is intentionally **not** part of this library — it is
a thin string-replace operation with no safety machinery.

```
src/rename/
    mod.rs           — public API: plan_rename(input) -> RenamePlan
    file.rs          — Type F (markdown file) planning
    attachment.rs    — Type A (attachment) planning
    link.rs          — Type L planning (workspace-wide string rewrite)
    heading.rs       — Type H planning (slug recomputation, link anchor updates)
    conflict.rs      — exact-path + prefix-collision + extension-class checks
    blocking.rs      — blocking-rule evaluation
    apply.rs         — apply RenamePlan to disk + text
```

CLI `rename-file` dispatches to either `file.rs` or `attachment.rs` based on the
source extension; the dispatcher in `mod.rs` does the inference so the CLI main
doesn't have to. CLI `rename-link` dispatches to `link.rs`.

The `RenamePlan` is a value object that captures every edit and every check. The LSP
serializes it to a `WorkspaceEdit` (used by both code actions); the CLI serializes
it to disk writes. Both call `plan_rename(input)` first, then either serialize (LSP)
or apply (CLI).

The persistent server (`downlint server`) runs the same `plan_rename(input)` function
internally — it receives rename requests over TCP, plans the edits, and returns the
result. The server's role is purely to keep the `ConnectionGraph` alive across multiple
invocations; the rename logic itself is identical to the CLI's in-process behavior.

This means:
- A single set of tests covers both surfaces for the file/heading/link-rewrite
  operations.
- Bug fixes apply uniformly.
- The `--dry-run` flag in the CLI is literally "call `plan_rename`, print, don't
  apply".

The string-only `textDocument/rename` LSP handler is implemented separately as a
simple wrapper around the parser's byte ranges — no library needed because there's
no resolution, no conflict check, no blocking rule.

## Configuration

No new config keys. Rename behavior is uniform; only `[wiki].obsidian_prefix` (RFC 0004)
and `[core].file_extensions` affect the rename logic.

## Implementation Plan

### Files

| File | Change |
|---|---|
| `src/lsp/mod.rs` | Add `rename` (string-only), `prepareRename` (string-only), `codeAction`, `didRenameFiles` handlers; extend `initialize` capabilities |
| `src/lsp/edit.rs` | New — `WorkspaceEdit` builder helper (documentChanges vs changes selection, reverse-order sorting) |
| `src/lsp/handlers/rename.rs` | New — prepareRename + string-only rename handlers |
| `src/lsp/handlers/code_action.rs` | New — `refactor.rename.file`, `refactor.rename.link-target`, and `refactor.rename.heading` actions |
| `src/lsp/handlers/workspace.rs` | New — `didRenameFiles` implementation |
| `src/lsp/server.rs` | New — persistent LSP server lifecycle (start, stop, idle timeout, TCP binding) |
| `src/rename/` | New — shared rename library (see Shared Rename Library) |
| `src/main.rs` | New `rename-file`, `rename-link`, and `server` subcommands wiring into the CLI dispatcher |
| `src/diagnostics/mod.rs` | No new codes |
| `src/parser/mod.rs` | No change — byte ranges already preserved |

### Phases

**Phase 1 — Shared library + code-action plumbing + string-only LSP rename**. Validates
the library API and the LSP code-action + rename plumbing. The string-only
`textDocument/rename` handler ships in this phase as a thin wrapper. CLI is not yet
wired up. ~400 LOC + tests.

**Phase 2 — Type H heading rename (code action + library)**. Adds slug recomputation
and byte-precise edits into link targets. Exposes the requirement that heading
edits live in the source document (not the linking document). LSP code action
`refactor.rename.heading` ships in this phase. ~400 LOC + tests.

**Phase 3 — Type F markdown file rename (code action + library)**. The biggest piece.
Adds path rewriting, `extra_folders` traversal, and prefix-collision detection for
the `refactor.rename.file` action. ~600 LOC + tests.

**Phase 4 — `workspace/didRenameFiles`**. Wires up the file-operation notification.
Trivial after Phase 3 because Phases 1–3 already build the per-document `TextEdit`s
needed. ~100 LOC + tests.

**Phase 5 — CLI subcommands + persistent server**. Wires the library into `downlint
rename-file` and `downlint rename-link`. Adds the `downlint server` subcommand for
persistent LSP server mode. ~400 LOC + tests.

**Phase 6 — Type L code action + CLI `rename-link` finalization**. Adds the
`refactor.rename.link-target` action and ensures CLI `rename-link` matches. ~200
LOC + tests.

### Tests

| Test | Phase | Notes |
|---|---|---|
| `code_action_rename_file_attachment` | 1 | `refactor.rename.file` on `[[photo.png]]` → file moved, refs rewritten |
| `code_action_rename_file_attachment_many_refs` | 1 | Verify reverse-order sort within the ref doc |
| `code_action_rename_file_attachment_alias_preserved` | 1 | `[[old.png\|alt]]` → `[[new.png\|alt]]` |
| `code_action_rename_file_conflict_exact_path` | 1 | Pre-existing file at new path → reject |
| `code_action_rename_file_conflict_prefix` | 1 | `obsidian_prefix=true`, new stem is prefix of other file → reject |
| `code_action_rename_file_not_offered_on_broken_link` | 1 | Cursor on `[[nonexistent]]` → no `refactor.rename.file` action |
| `code_action_rename_file_not_offered_on_ambiguous` | 1 | `obsidian_prefix=true`, ambiguous link → no `refactor.rename.file` action |
| `code_action_rename_link_target_offered_on_broken_link` | 1 | Cursor on `[[nonexistent]]` → `refactor.rename.link-target` offered |
| `code_action_rename_link_target_offered_on_resolved` | 1 | Cursor on `[[report]]` → both actions offered |
| `code_action_rename_link_target_resolves` | 1 | Invoking `refactor.rename.link-target` runs Type L logic |
| `lsp_rename_string_on_link_target` | 1 | F2 on `[[report]]`, types `topic` → `[[topic]]`, file not moved |
| `lsp_rename_string_on_broken_link` | 1 | F2 works on `[[nonexistent]]` |
| `lsp_rename_string_preserves_alias` | 1 | F2 on `[[report\|alias]]` → `[[topic\|alias]]` |
| `lsp_rename_string_preserves_anchor` | 1 | F2 on `[[report#head]]` → `[[topic#head]]` |
| `prepare_rename_null_on_plain_text` | 1 | Cursor on prose returns null |
| `prepare_rename_range_on_link_target` | 1 | Range covers only the target string |
| `prepare_rename_range_on_heading_text` | 2 | Range covers only the heading text |
| `code_action_rename_heading_offered_on_heading` | 2 | Cursor on `## Methods` → `refactor.rename.heading` offered |
| `code_action_rename_heading_not_on_link_target` | 2 | Cursor on `[[doc#section]]` target → no heading action |
| `rename_heading_same_doc` | 2 | Heading + wiki-link to it in same doc, both updated |
| `rename_heading_cross_doc` | 2 | Two ref docs, both updated |
| `rename_heading_slug_collision_rejected` | 2 | Reject when slug collides |
| `rename_heading_markdown_link_anchor` | 2 | `[t](d.md#old)` → `[t](d.md#new)` |
| `rename_heading_preserves_query_fragment` | 2 | `[t](d.md?ref=1#old)` → `[t](d.md?ref=1#new)` |
| `code_action_rename_file_no_refs` | 3 | Empty WorkspaceEdit (just disk move) |
| `code_action_rename_file_one_ref` | 3 | One TextEdit |
| `code_action_rename_file_many_refs_across_extra_folders` | 3 | Refs in `extra_folders` are updated |
| `code_action_rename_file_prefix_unique_promoted_to_full_stem` | 3 | `obsidian_prefix=true`, link rewritten to full new stem |
| `code_action_rename_file_prefix_unique_no_flag_broken` | 3 | `obsidian_prefix=false`, link stays `DNL002` |
| `code_action_rename_file_preserves_heading_anchor` | 3 | `[[old#head]]` → `[[new#head]]` |
| `code_action_rename_file_markdown_link` | 3 | `[t](old.md)` → `[t](new.md)` |
| `code_action_rename_file_path_relative_rewrite` | 3 | Cross-subtree rename produces `../`-laden path |
| `code_action_rename_file_conflict_exact_path` | 3 | Reject when new path collides |
| `code_action_rename_file_conflict_reverse_prefix` | 3 | `report.md` → `report-2024.md` when `[[report]]` would re-resolve |
| `code_action_rename_file_extension_class_rejected` | 3 | `report.md` → `report.pdf` rejected (markdown → attachment) |
| `code_action_rename_file_blocked_by_unrelated_dnl002` | 3 | Doc has `DNL002` on a different link → block |
| `code_action_rename_file_blocked_by_dnl001_obsidian` | 3 | `[[report]]` `DNL001` in a doc that's rewritten → block |
| `code_action_rename_file_allowed_when_unrelated_diag_is_info` | 3 | `DNL006`/`DNL007` doesn't block |
| `code_action_rename_file_blocked_when_indexing` | 3 | Indexing in progress → MethodFailed |
| `code_action_rename_link_target_blocked_when_indexing` | 6 | Indexing in progress → MethodFailed |
| `did_rename_files_updates_graph` | 4 | Move file, graph reflects new path, no broken-link diagnostic |
| `did_rename_files_out_of_scope` | 4 | Move file out of `extra_folders`, refs now broken |
| `cli_rename_file_dry_run` | 5 | Exit 0, prints edits, no changes |
| `cli_rename_file_applies_text_first_then_disk` | 5 | Crash safety: text edits applied before disk move |
| `cli_rename_file_exit_codes` | 5 | Exit 0/1/2/3 as specified |
| `cli_rename_file_indexing_blocked` | 5 | Indexing in progress → exit 3 with message |
| `cli_rename_file_attachment` | 5 | `downlint rename-file --from foo.png --to bar.png` (no `--kind` flag) |
| `cli_server_start_stop` | 5 | `downlint server --detach` starts, `--stop` shuts down |
| `cli_server_idle_timeout` | 5 | Server exits after 5 min idle |
| `cli_rename_server_fallback` | 5 | No server running → fallback to default behavior |
| `cli_rename_server_unresponsive` | 5 | Server not responding → exit 3 with message |
| `cli_rename_file_multi_server` | 5 | Three sequential renames via --server, single indexing cost |
| `cli_rename_server_stale_pid_recovery` | 5 | Stale PID file detected and removed on server start |
| `cli_rename_server_fallback_on_stale_pid` | 5 | CLI detects stale PID and falls back to default behavior |
| `cli_rename_server_reindex_in_progress` | 5 | File changed externally, server re-indexing → exit 3 |
| `cli_rename_server_race_on_external_change` | 5 | File moved between planning and applying → CLI fails with message |
| `cli_rename_link_resolves_uniquely` | 6 | `--from report` only rewrites occurrences resolving uniquely to `report.md` |
| `cli_rename_link_drops_ambiguous` | 6 | `obsidian_prefix=true`, ambiguous occurrences are not touched |
| `cli_rename_link_conflict_exact` | 6 | `--to topic` when `topic.md` exists → reject |
| `cli_rename_link_conflict_prefix_flag` | 6 | `obsidian_prefix=true`, `--to topic` is a prefix of `topic-2024.md` → reject |
| `cli_rename_link_conflict_title` | 6 | `--to Some Title` matches an H1 → reject |
| `cli_rename_link_blocked_by_unrelated_diag` | 6 | Block and exit 1 |

### Integration test scaffolding

Most tests use a tiny in-memory vault (2–5 files) loaded via the same harness
`tests/integration.rs` already uses. New test files:
- `tests/rename_tests.rs` — LSP rename behavior (mirrors `refactor_tests.rs` mentioned
  in spec.md:2391).
- `tests/cli_rename_tests.rs` — CLI subprocess tests, asserting stdout/stderr/exit
  code.

**Expanded test coverage for previously missing gaps:**

| Area | Tests Added |
|---|---|
| Prefix-resolved link promotion | `code_action_rename_file_prefix_unique_promoted_to_full_stem` (Phase 3) |
| Reverse prefix collision | `code_action_rename_file_conflict_reverse_prefix` (Phase 3) — ensures `report.md` → `report-2024.md` is rejected when `[[report]]` would re-resolve |
| Extension-class preservation | `code_action_rename_file_extension_class_rejected` (Phase 3) — `report.md` → `report.pdf` |
| Indexing guard | `code_action_rename_file_blocked_when_indexing` (Phase 3), `cli_rename_file_indexing_blocked` (Phase 5) |
| Atomic CLI application | `cli_rename_file_applies_text_first_then_disk` (Phase 5) — verifies text edits are written before disk move |
| `refactor.rename.heading` | `code_action_rename_heading_offered_on_heading` (Phase 2), `rename_heading_slug_collision_rejected` (Phase 2) |
| `refactor.rename.link-target` | `code_action_rename_link_target_offered_on_broken_link` (Phase 1), `cli_rename_link_resolves_uniquely` (Phase 6) |
| `didRenameFiles` | `did_rename_files_updates_graph` (Phase 4), `did_rename_files_out_of_scope` (Phase 4) |

## Backward Compatibility

- New LSP methods are additive — clients that don't send `textDocument/rename`,
  `textDocument/codeAction`, or `workspace/didRenameFiles` are unaffected.
- The new `workspace.fileOperations.didRename` capability triggers editors to send
  notifications on file rename; clients that ignore capabilities don't notice.
- **Behavior change**: `textDocument/rename` previously did not exist in downlint;
  it is now advertised. The semantics are string-only (never moves files). Editors
  that bind F2 to `textDocument/rename` will now perform string renames on
  previously-unsupported cursor positions (e.g. link targets). This is additive —
  there is no prior behavior to break.
- The CLI subcommands are brand-new; no existing command behavior changes.
- No changes to existing diagnostic codes, resolution rules, or config keys.

## Risks

1. **Slug collision on heading rename.** Mitigated by rejecting the rename — but a
   user with many duplicate-heading files may find rename unusable. Future RFC could
   add auto-suffix selection (e.g. `Heading (2)`).
2. **Path rewriting producing ugly `../` chains.** Cross-subtree moves of widely-
   referenced files can produce brittle paths. Mitigation: keep relative paths in v1
   (existing resolver behavior); future RFC could add an absolute-path preference.
3. **`didRenameFiles` race.** If the server is mid-resolution when the notification
   arrives, the graph update could conflict. Mitigation: serialize all graph
   mutations through a single mutex.
4. **Blocking rule false positives.** A document with one unrelated broken link
   blocks renames everywhere in that document. Mitigation: future RFC could relax
   to "blocking diagnostics must be on occurrences that share a destination with the
   rename target". Out of scope for v1.
5. **Extension-class mismatch.** A user renaming `report.md` → `report.pdf` trips the
   extension-class preservation rule. The error message must name the source class
   (markdown) and target class (attachment) so the user understands the mismatch.
6. **Atomic CLI application.** The CLI writes text edits first, then moves the file.
   If the disk move fails mid-way, text edits have been applied but the file is still
   at the old location — leading to broken links. Mitigation: write text edits to
   `.downlint/.rename.lock` (content-addressed state per rename operation). On CLI
   startup, detect unfinished lock files and offer rollback. The lock file contains:
   the list of modified documents, the old→new file path mapping, and a checksum
   so partial writes are detectable. Concurrent renames are prevented by a simple
   file lock (flock) on `.downlint/.rename.lock`.
7. **F2 discoverability shift.** Users who relied on F2 to rename files (the old
   `textDocument/rename` semantics) will be surprised when F2 only renames strings.
   Mitigation: advertise the `refactor.rename.file` code action prominently in the
   `codeActionProvider` capability kinds list, document it in README, and consider
   adding a one-time migration hint when an old workspace loads.
8. **Rename during indexing.** If a rename operation starts while indexing is in
   progress, the `ConnectionGraph` may be inconsistent. Mitigation: reject all
   rename operations (both CLI and LSP) when indexing is in progress with a clear
   message: "Indexing in progress — try again in a moment." The LSP handler returns
   `MethodFailed`; the CLI exits with code 3. The persistent server (`downlint
   server`) mitigates this for agent workflows by ensuring indexing happens once
   and the graph stays consistent across multiple rename requests.
9. **Persistent server port conflict.** If two `downlint server` instances try to
   bind the same port, the second fails. Mitigation: use `--port 0` for auto-assign
   (OS picks an available port, reported to stdout). The `.downlint/.server.pid` file
   prevents stale server detection issues.
10. **Persistent server resource leak.** If the server process crashes without cleaning
    up `.downlint/.server.pid`, a stale PID file remains. Mitigation: on server start,
    check if the recorded PID is still alive; if not, remove the stale file. On CLI
    `--server` usage, detect stale files and warn/fallback.
11. **Planning-execution race.** The server plans the rename, then the CLI applies
    it. Between these two steps, the file system could change (e.g., another process
    renames a file). Mitigation: the CLI re-verifies `--from` exists on disk right
    before applying. If it doesn't, the CLI fails with "Workspace changed — the file
    has moved since planning." For multi-step agent workflows, this means each rename
    should be idempotent — if it fails due to a race, the agent can retry.

## Alternatives Considered

1. **One mega-handler with a giant match on element type.** Rejected — each type has
   different propagation logic; splitting keeps each handler testable in isolation.
2. **Use `willRenameFiles` to compute and apply edits before the disk move.** Rejected
   for v1 — the editor is the source of truth for the disk move, and computing edits
   *before* the move introduces version skew if the user cancels.
3. **Skip prefix-resolved links during file rename and warn.** Earlier draft of this
   RFC took this stance. Replaced with the current "promote to full stem" rule after
   user feedback that the silent widening is the expected behavior and the alternatives
   are either over-cautious or under-clear.
4. **CLI as a thin wrapper around an LSP client.** Rejected — adds an LSP round-trip
   to a CLI invocation; direct library use is simpler and faster.
5. **Defer the CLI to v2.** Rejected — scripted vault reorganization is a primary
   use case and the library is already needed for the LSP work.
6. **`textDocument/rename` resolves the cursor and renames the backing file.** This
   is the natural LSP convention (F2 renames the symbol). Rejected because the
   cursor on `[[report]]` is fundamentally ambiguous: it could be a file stem, an
   H1 title, a prefix of multiple files, or unresolved. F2 cannot ask the user
   which interpretation they want, and silently moving a file on F2 is dangerous.
   The current layout makes `textDocument/rename` string-only (safe default) and
   surfaces file renames behind `refactor.rename.file` (explicit opt-in).
7. **`textDocument/rename` is not advertised; only code actions exist.** Rejected
   as too aggressive — it loses F2 discoverability for the common case of fixing
   typos in link targets. The current layout keeps F2 working for the safe
   operation.
8. **Single CLI subcommand with `--kind` flag.** Rejected — the user has to remember
   to type `--kind` and `--kind link` vs `--kind file` is non-obvious for users who
   don't care about the distinction. Two subcommands (`rename-file`, `rename-link`)
   are unambiguous and discoverable via tab completion.
9. **Per-link "rename anyway" override.** Rejected for v1 — if the user really wants
   to force a rename despite a blocking diagnostic, they can fix the diagnostic and
   re-run.
10. **Allow renames during indexing.** Rejected — the `ConnectionGraph` may be
    inconsistent mid-index, producing incorrect rewrites. The indexing guard is a
    simple pre-condition check that rejects all rename operations until indexing
    completes. For the persistent server, the indexing guard checks the server's
    indexing state: if the server is mid-reindex (after detecting a file change),
    renames are blocked with "Server re-indexing — try again in a moment."
11. **Persistent server as default CLI mode.** Rejected for v1 — the simple
    single-invocation model (`build graph → rename → exit`) is easier to reason
    about, debug, and integrate into scripts. The server mode is an opt-in
    optimization for agent workflows.

## Future Work

- `workspace/willRenameFiles` — pre-compute edits and offer them as a `WorkspaceEdit`
  in the `willRename` response so editors can preview.
- Reference-label rename (Type `ML`/`MLD`).
- Heading rename auto-disambiguation when slug collision is detected (offer
  `Heading (2)` as the new name).
- Slug-collision warning during rename preparation (currently a hard reject).
- Per-link "rename anyway" override for blocking diagnostics.
- `[rename] require_clean_docs = false` opt-out from the blocking rule.
- CLI subcommand for heading rename (if a script use case emerges).

## Open Questions

1. **What happens when the renamed file's *content* changes the document title (H1)
   and the new title slug no longer matches the file stem?** Out of scope for v1
   (title and stem are independent identifiers); tracked as future work.
2. **Does the blocking rule also apply to `didRenameFiles`?** The notification handler
   doesn't apply renames, it only updates indexes — so no blocking is needed. But
   should `didRenameFiles` re-emit the blocking diagnostic on affected documents so
   the user knows? Tracked as future work.
3. **Should `refactor.rename.heading` be LSP-only or get a CLI subcommand?**
   Decision: LSP only. Headings are editor-driven changes; no script use case
   justifies a CLI command. This is reflected in the Non-Goals section.

## Out of Scope

- Markdown formatting changes during rename (whitespace, link rewrapping).
- Renaming tags (e.g. `#topic`).
- Renaming headings *and* their containing document in one operation (Type F then
  Type H). Two separate user gestures.
- Frontmatter path references (e.g. `cover: image.png` in YAML).