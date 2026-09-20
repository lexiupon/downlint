# Downlint — Product Behavior

**Status**: Living document. Higher level than `spec/linting.md`; where the two ever differ
on behavior, `spec/linting.md` wins. This document describes the **product surface** — what
a user sees and can do. It deliberately omits implementation detail: the code is the source
of truth for *how*.

---

## 1. What downlint is

Downlint is a markdown **linter and language server** for knowledge bases. Its reason to
exist is to keep links valid as a knowledge base evolves: it detects broken and ambiguous
links, completes link targets, and rewrites every reference when a file, attachment, or
heading is renamed.

It has two surfaces that share one resolution engine and one rename engine:

- a **CLI checker** — for CI, scripts, and one-off checks
- an **LSP server** — for editors

## 2. What it checks

Link validity across wiki links, markdown links, images, embeds, heading anchors, explicit
file-like attachments, and mapped external URIs. Diagnostics `DNL001`–`DNL009` (DNL004
unassigned).

**The normative rule conditions — what must hold — are in `spec/linting.md`.** This
document does not restate them.

## 3. Interfaces

### 3.1 CLI

Five subcommands. `downlint` with no subcommand runs the check.

| Command | Purpose |
|---|---|
| `downlint [PATH]` (alias `check`) | Check a file or directory; print diagnostics; exit by severity. |
| `downlint server` | Run the LSP server over stdio. |
| `downlint rename-file` | Move a markdown file or attachment on disk and rewrite every link pointing at it. |
| `downlint rename-link` | Rewrite a logical link-target identifier workspace-wide (no disk move). |
| `downlint warm-uri-mappings` | Prepare local copies of mapped external assets so they can be validated. |

**`check`** — key flags (defaults in parentheses):

| Flag | Default | Effect |
|---|---|---|
| `--root <DIR>` | inferred | Override workspace root. |
| `--format <text\|json>` | `text` | Output format. |
| `--min-severity <info\|warning\|error>` | `warning` | Filter output; exit code reflects the filtered set. |
| `--color <auto\|always\|never>` | `auto` | ANSI coloring. |
| `-v / --verbose <N>` | `2` | Logging to stderr (independent of `--min-severity`). |
| `--fix` | off | Apply safe fixes in place (v1: DNL003 NBSP→space), then re-run. |
| `-w / --watch` | off | Re-run after debounced file changes. |
| `--stdin` (or `-`) | off | Check one document from stdin (single-file mode). |
| `--allow-uri-sync` | off | Permit `warm_cmd` subprocess execution (safety gate). |
| `--no-uri-hints` | off | Suppress DNL006. |
| `--uri-sync-batch-size <N>` | `50` | Warm batch fan-out (min 1). |

- **Exit codes**: `0` clean at the chosen severity · `1` issues found · `2` error
  (bad path, config parse failure, failed `--fix` write).
- **Text output**: `{rel_path}:{line}:{col}: {severity}: {message} [{CODE}]`.
- **JSON output**: array of `{path, range, severity, code, message, related}`.

**`rename-file` / `rename-link`** — required `--from` / `--to`; `--dry-run` prints the plan
without applying; `--root` as above.

- **Exit codes**: `0` success (or no-op) · `1` blocked by a pre-existing Warning/Error
  diagnostic · `2` conflict (path/prefix collision, extension-class mismatch) · `3` bad
  arguments / config error / source not found.
- `rename-file` infers markdown-vs-attachment from the source extension — the user never
  picks. `rename-link` rewrites a bare identifier and moves nothing.

**`warm-uri-mappings`** — requires `--allow-uri-sync` (exit `2` without it); walks every
URI-scheme link target, groups by mapping, runs `warm_cmd` in batches. It does **no
validation** — run `check` afterwards. Exit `0` all synced · `1` any failure · `2` missing
gate / config error.

### 3.2 LSP

LSP over stdio (JSON-RPC 2.0, `Content-Length` framing, UTF-16 positions). The **shipped**
surface:

- **Lifecycle**: `initialize`, `initialized`, `shutdown`, `exit`.
- **Text sync**: `didOpen`, `didChange`, `didClose` (full-text semantics).
- **Diagnostics**: push via `publishDiagnostics` on open/change/close and after
  `didRenameFiles`.
- **Completion**: wiki document (`[[foo`), wiki heading (`[[#`), and tag (`#`) prompts;
  trigger characters `[`, `#`, `(`; case-insensitive subsequence matching.
- **Code intelligence**: `hover`, `definition`, `references`, `documentSymbol`.
- **Rename**: `prepareRename` + string-only `textDocument/rename` (replaces the link-target
  or heading text at the cursor; never moves files); `workspace/didRenameFiles` keeps the
  index consistent after an editor-driven disk rename.
- **Code actions**: rename actions are offered — `refactor.rename.link-target` and
  `refactor.rename.heading` (and `refactor.rename.file` is advertised).

*(Non-normative status note: the rename code actions are offered but their end-to-end edit
path is still landing — the **CLI is the working rename surface today**. Not yet shipped:
code lenses, semantic tokens, pull diagnostics, TOC / create-missing-file code actions,
`documentHighlight`, `completionItem/resolve`, and the `workspace/didChange*` configuration
handlers. See `spec/linting.md` §6.3 and the code.)*

### 3.3 Configuration

`.downlint.toml` in the project, plus a user-level config; precedence **project > user >
defaults**; unknown keys are a hard parse error (fail-fast, exit 2).

**Normative semantics of every key: `spec/linting.md` §5.**

## 4. Rename (product behavior)

Downlint's core differentiator: rename something, and every reference is rewritten.

**Four rename kinds** (two CLI subcommands; the CLI collapses file/attachment into
`rename-file`):

| Kind | Element | Trigger |
|---|---|---|
| File | a markdown document | `rename-file` / `refactor.rename.file` |
| Attachment | a non-markdown asset (image, PDF, XLSX…) | `rename-file` / `refactor.rename.file` |
| Heading | H1–H6 heading text | `refactor.rename.heading` (LSP only — no CLI) |
| Link target | a bare identifier, no disk move | `rename-link` / `refactor.rename.link-target` / F2 |

**What gets rewritten** (file/attachment rename): the path portion of wiki links, markdown
links, and reference definitions — preserving `|alias`, `#heading`, and URL fragments.
New paths are computed relative to each referencing document (cross-subtree moves yield
`../`). References from extra folders are included.

**Heading rename** rewrites only the `#section` portion of every referencing link.

**Safety rules the user must know:**

- **Conflicts** (exit 2 / refused): the new path already exists as a different file; a
  prefix collision when `wiki.obsidian_prefix` is on; or an extension-class change
  (`.md`→`.pdf` is rejected).
- **Blocking rule** (exit 1 / refused): a rename is refused if any document that would be
  rewritten contains a Warning or Error diagnostic *other than on the very occurrence being
  rewritten*. Info never blocks. Fix the diagnostic, then re-run.
- **No force/override** in v1.
- `--dry-run` previews the full plan; application is text-first-then-disk.

## 5. Workflows

- **CI**: `downlint` as a gate; `--format json` for machines; `--min-severity error` to
  gate on errors only; `--color never` for logs.
- **Editor**: `downlint server` over stdio — push diagnostics, completion, hover/definition/
  references, string-only F2 rename; `didRenameFiles` keeps indexes consistent.
- **KB maintenance**: `rename-file` / `rename-link` to evolve the base; `--dry-run` to
  preview; the blocking rule forces cleanup of pre-existing diagnostics first.
- **Cloud assets**: declare `[[uri.mappings]]`, run `warm-uri-mappings --allow-uri-sync`
  to hydrate locally, then `check`.
- **Quick single-doc check**: `echo '# Title' | downlint -`.

## 6. What downlint is not

- **Not a formatter or general style linter** — checks are link-centric (plus DNL003).
  `textDocument/formatting` is never implemented (markdown is whitespace-sensitive).
- **Not a sync tool** — "warm" is deliberately one-way; `warm-uri-mappings` prepares local
  copies for validation only.
- **Rename non-goals**: no `willRenameFiles`; no reference-label rename; no
  extension-class-changing renames; no frontmatter path references; no CLI heading-rename
  subcommand (LSP-only by decision); no combined heading+document rename.
- **v1 LSP exclusions**: folding ranges, `workspace/symbol`, and the `will*` file-operation
  methods are deferred (see `spec/linting.md` §6.3).

## 7. Pointers

| Question | Where |
|---|---|
| What must hold (normative behavior) | `spec/linting.md` |
| What is open / not yet shipped | `TODO.md` |
| Why a decision was made | `spec/linting.md` §8 (amendment history) |
| How it is built | `src/` |
| History | `CHANGELOG.md` |
