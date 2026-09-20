# Downlint — Open Issues & Known Gaps

**Status**: Living document. Tracks work that is specified or promised but not (fully)
implemented, plus known gaps and decisions still open. This is *not* normative — behavior
lives in `spec/linting.md` and `spec/downlint.md`. When an item ships, move it to
`CHANGELOG.md` and remove it here.

Conventions:
- `[ ]` open · `[~]` in progress · `[x]` done (remove after next changelog pass)
- Each item names the spec clause or code area it touches, so it can be resolved against
  the normative text.

---

## Diagnostics

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

## LSP — specified but not implemented

These appear in the bootstrap capability matrix / ROADMAP but have no handler in `src/lsp/`
today. The shipped surface is documented in `spec/downlint.md` §3.2.

- [ ] **Rename code actions not executable end-to-end** (`spec/downlint.md` §3.2).
  `refactor.rename.link-target` and `refactor.rename.heading` are offered but carry no
  `edit` and there is no `codeAction/resolve` handler. `refactor.rename.file` is advertised
  in capabilities but not offered by the handler. The CLI is the working rename path until
  this lands.
- [ ] **`refactor.rename.file` not offered** — advertised in `initialize`, absent from the
  `codeAction` handler (RFC 0009 Phase 3).
- [ ] **TOC code action** not implemented (`code_action.toc.*` config keys exist; no
  handler).
- [ ] **Create-Missing-File code action** not implemented (`code_action.create_missing_file.enable`
  exists; no handler).
- [ ] **Code lenses** (`textDocument/codeLens` + resolve) not implemented — link/reference
  counts.
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
  prints "not yet implemented" and exits 3. Blocks the agent multi-rename workflow
  (RFC 0009 Phase 5).
- [ ] **`--server` on rename subcommands** is parsed but ignored (dead flag).
- [ ] **`--wait-for-debugger`** is a 250 ms sleep placeholder — decide real behavior or
  remove.

## Rename

- [ ] **No atomic-apply guard** — RFC 0009 risk #6 (`.downlint/.rename.lock` rollback) is
  not implemented. Apply is text-first-then-disk; a partial disk failure surfaces as DNL002
  rather than rolling back.
- [ ] **Heading rename has no usable path today** — it is LSP-only by decision (no CLI
  subcommand), but its code action is not executable (see LSP section). Net effect: heading
  rename is currently unreachable end-to-end.

## CLI & docs

- [ ] **README drift** — the Rename section documents `--config` and `--allow-extra-folders`
  (default true) on the rename subcommands; neither flag exists in `src/cli/`. Fix the
  README or add the flags.
- [ ] **Windows drive-letter quirk** (`spec/linting.md` §6.2 #16) — a target like `C:\…`
  is parsed as a URI-scheme target (scheme `C`) and goes through RES-07, not path
  resolution. Decide: special-case single-letter schemes, or document as intended.

## Future / deferred (not near-term)

- [ ] **Wiki-link ↔ markdown-link conversion** (was RFC 0005) — bidirectional conversion
  so wiki-based notes can be published to standard markdown pipelines. Not implemented;
  the design is in git history (`rfc/0005`).
- [ ] **v2 LSP methods** (see `ROADMAP.md`): `workspace/symbol`,
  `textDocument/foldingRange`, and the `workspace/will*Files` pre-operation methods.

---

## Decisions resolved during the spec extraction (2026-09-20)

Recorded here so the reasoning isn't lost when the bootstrap `spec.md` and `rfc/` are
removed. All resolved **in favor of the code** (ground truth):

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
