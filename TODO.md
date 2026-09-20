# Downlint — Plan & Open Issues

**Status**: Living document. The single plan for downlint: open issues and known gaps
(what to fix), planned work (what to build next), exploratory ideas (later), and what is
deliberately out of scope. This is *not* normative — behavior lives in `spec/linting.md`
and `spec/downlint.md`. Shipped features are recorded in `CHANGELOG.md`. When an item
ships, move it to `CHANGELOG.md` and remove it here.

Conventions:
- `[ ]` open · `[~]` in progress · `[x]` done (remove after next changelog pass)
- Each item names the spec clause or code area it touches, so it can be resolved against
  the normative text.
- This file replaced the former `ROADMAP.md`; that file's "Recently Shipped" section was
  always a duplicate of `CHANGELOG.md`.

---

## Open Issues & Known Gaps

Things that are wrong, missing, or inconsistent relative to what should already work.

### Diagnostics

- [ ] **DNL009 is declared but never emitted** (`spec/linting.md` §4.8). The 128 KiB argv
  batch fallback sets `BatchOutcome.fell_back_to_per_file`, but the only caller
  (`warm-uri-mappings`) ignores it — the fallback is silent. Decide: emit the diagnostic
  (per the spec's intended behavior) or drop the code and the clause.
- [ ] **No test for DNL003** (`spec/linting.md` §4.3). The NBSP-after-heading rule has no
  test pinning it.
- [ ] **No test for single-file-mode DNL002 suppression** (`spec/linting.md` §4.2/§4.9).
  Cross-file broken links are silently dropped in single-file mode; untested.
- [ ] **No test for URI-target + anchor → DNL005** (`spec/linting.md` §3.7/§4.4). Anchors
  on external assets always emit DNL005; untested.

### Rename

- [ ] **No atomic-apply guard** — the `.downlint/.rename.lock` rollback guard is not
  implemented. Apply is text-first-then-disk; a partial disk failure surfaces as DNL002
  rather than rolling back.
- [ ] **Heading rename has no usable path today** — it is LSP-only by decision (no CLI
  subcommand), but its code action is not executable (see Planned → LSP). Net effect:
  heading rename is currently unreachable end-to-end.

### CLI & docs

- [ ] **README drift** — the Rename section documents `--config` and `--allow-extra-folders`
  (default true) on the rename subcommands; neither flag exists in `src/cli/`. Fix the
  README or add the flags.
- [ ] **Windows drive-letter quirk** (`spec/linting.md` §6.2 #16) — a target like `C:\…`
  is parsed as a URI-scheme target (scheme `C`) and goes through RES-07, not path
  resolution. Decide: special-case single-letter schemes, or document as intended.
- [ ] **`--wait-for-debugger`** is a 250 ms sleep placeholder — decide real behavior or
  remove.

---

## Planned — near-term

Specified or committed for the next release but not (fully) implemented. The shipped LSP
surface is documented in `spec/downlint.md` §3.2.

### LSP

- [ ] **Rename code actions not executable end-to-end** (`spec/downlint.md` §3.2).
  `refactor.rename.link-target` and `refactor.rename.heading` are offered but carry no
  `edit` and there is no `codeAction/resolve` handler. The CLI is the working rename path
  until this lands.
- [ ] **`refactor.rename.file` not offered** — advertised in `initialize`, absent from the
  `codeAction` handler.
- [ ] **TOC code action** not implemented (`code_action.toc.*` config keys exist; no
  handler).
- [ ] **Create-Missing-File code action** not implemented (`code_action.create_missing_file.enable`
  exists; no handler).
- [ ] **Code lenses** (`textDocument/codeLens` + resolve) — link/reference counts, plus a
  TOC lens for quick regeneration.
- [ ] **Semantic tokens** (`semanticTokens/full`, `/delta`, `/range`) not implemented.
- [ ] **Pull diagnostics** (`textDocument/diagnostic`, `workspace/diagnostic`) not
  implemented — diagnostics are push-only today.
- [ ] **`documentHighlight`** not implemented.
- [ ] **`completionItem/resolve`** not implemented — items carry their data inline.
- [ ] **`didSave`** is advertised (`save: true`) but has no handler — silently ignored.
- [ ] **Workspace change handlers** not implemented: `didChangeConfiguration`,
  `didChangeWatchedFiles`, `didCreateFiles`, `didDeleteFiles`,
  `didChangeWorkspaceFolders`. Changing config/flags requires a server restart.
- [ ] **Incremental text sync not honored** — the capability advertises `change: 1`
  (incremental) but the handler only consumes full-text changes; `core.text_sync =
  "incremental"` has no effect.
- [ ] **Completion config not wired** — `completion.candidates` and `completion.wiki.style`
  exist with defaults, but the LSP handler hardcodes 50 candidates and `title-slug` style.
- [ ] **Persistent server** (`server --detach` / `--stop` / `--port`) unimplemented —
  prints "not yet implemented" and exits 3. Blocks the agent multi-rename workflow.
- [ ] **`--server` on rename subcommands** is parsed but ignored (dead flag) — wire it to
  the persistent server or remove it.
- [ ] **`workspace/symbol`** — cross-document symbol search.
- [ ] **`textDocument/foldingRange`** — headings, lists, code blocks, frontmatter.
- [ ] **`workspace/will*Files`** — `willCreateFiles` / `willRenameFiles` /
  `willDeleteFiles` with pre-computation.

### Non-LSP

- [ ] **Progress reporting** — `window/workDoneProgress/create` + `$/progress` for
  long-running operations (workspace-wide diagnostics, large renames).
- [ ] **Diagnostic caching** — cache diagnostic results per document to speed up
  re-checks.
- [ ] **Multi-root workspaces** — multiple independent LSP roots (beyond single root +
  `extra_folders`).

---

## Exploratory — later

Interesting ideas, not yet committed.

### LSP methods

- [ ] **`textDocument/declaration`** — "go to source definition" for wiki-links.
- [ ] **`textDocument/inlayHint`** — hint text for wiki-link targets, tag references.
- [ ] **`textDocument/formatting`** — heading alignment, list renumbering (markdown is
  whitespace-sensitive).
- [ ] **`textDocument/selectionRange`** — multi-cursor selection across linked documents.
- [ ] **`textDocument/linkedEditingRange`** — edit a heading title and all wiki-link
  references simultaneously.

### Features

- [ ] **Wiki-link ↔ markdown-link conversion** — bidirectional, so wiki-based notes can be
  published to standard markdown pipelines. Original design archived in git history.
- [ ] **Tag-based navigation** — `workspace/symbol` for tags, go-to-tag.
- [ ] **Document outline improvements** — better hierarchical document symbol tree.
- [ ] **Smart rename for headings** — propagate a heading rename across ALL references
  (wiki-links, markdown links, tags).
- [ ] **Mermaid / diagram support** — parse and validate Mermaid diagrams in markdown.

### Experimental

- [ ] **LSP 3.17+ enhancements** — `$/cancelRequest`, improved progress tokens.
- [ ] **Server-side completions** — pre-compute completion lists for faster responses.
- [ ] **Web-based UI** — browser-based markdown editor with downlint integration.

---

## Out of Scope / Won't Do

Deliberately not implemented.

| Feature | Reason |
|---------|--------|
| `textDocument/typeDefinition` | Markdown has no type system |
| Signature help | Not applicable to markdown |
| Document formatting (full) | Whitespace-sensitive; full reformatting risks breaking content |
| `downlint/status` notification | LSP clients don't consume this |

v1 LSP exclusions are normative in `spec/linting.md` §6.3.

---

## Resolved Decisions (2026-09-20 spec extraction)

Recorded here so the reasoning behind these calls is preserved. All resolved **in favor
of the code** (ground truth):

- `wiki.obsidian_prefix` belongs in the config schema (the bootstrap spec omitted the
  whole `[wiki]` section).
- Defaults: `core.file_extensions = ["md","markdown"]` (no `mdx`);
  `code_action.toc.include = [1..6]`; `uri.mappings[*].warm_timeout = 30`.
- DNL003 message text: "Non-breaking whitespace after heading marker".
- DNL006/007/008 multiplicity is **per-run**, not per-file.
- Hidden files/directories are **excluded** by default (`hidden(false)`).
- Exactly 9 silent web schemes: http, https, ftp, ftps, mailto, tel, sms, irc, xmpp.
- DNL005 is always Warning (even for wiki `[[#missing]]`).
- Folder links resolve to the directory itself — no index-file lookup.
- DNL009 is declared but not emitted (see Open Issues).
