# RFC 0019 — Rename command cross-hints (DX)

**Status**: Proposed
**Date**: 2026-09-23
**Scope**: CLI diagnostics only — advisory hints on the two rename commands.
No changes to resolution, rename semantics, exit codes, or the LSP surface.

---

## 1. Summary

`file rename` and `link rename` share the same verb and the same flag shape
(`--from` / `--to` / `--dry-run`), differing only in the noun and in what
`--from` means: a **file path** vs a **bare identifier**. When a user picks
the wrong noun, the failure is silent or uninformative. This RFC adds three
advisory hints that convert that moment into a visible, self-correcting one:

1. `link rename` notes when `--from` looks like a file path.
2. `file rename` suggests `link rename` when the source file is missing but a
   document with that stem exists.
3. Both commands' `--help` about-lines name their sibling explicitly.

## 2. Motivation

The wrong-noun mistake is real and silent. Repro (downlint 0.15.1):

```
$ # user means: file rename --from report.md --to topic.md
$ downlint link rename --from report.md --to topic.md
$ echo $?
0                      # "success"
$ ls                   # report.md — file NOT moved
$ cat index.md
see [[topic.md]]       # links rewritten to a BROKEN target (extension included)
```

Why it happens: `link rename` strips a markdown extension from `--from` for
stem-matching (lenient by design — `[[report.md]]` is a legal link form) and
uses `--to` verbatim as the new link text. The command succeeds, performs the
wrong operation, and leaves the vault with broken links. The reverse mistake
(`file rename --from report` with no extension) fails loudly (exit 3,
"source file not found: report") but sends the user nowhere.

No naming change fixes this — the confusion lives in the argument semantics
(path vs identifier), not the command names (see RFC 0018 §7 for why the
shared verb is kept). Hints are the targeted fix.

## 3. The hints

All hints are **advisory**: they never change the exit code, never block the
operation, and are printed to stderr.

### 3.1 `link rename` — path-like `--from` note

When `--from` carries a markdown extension (per `[core].file_extensions`),
print after workspace discovery, before planning:

```
downlint: note: --from "report.md" looks like a file path; `link rename` takes a bare identifier — use `file rename` to move the file
```

- Non-fatal: the rename proceeds exactly as today (stem-stripping is
  documented planner behavior).
- Extension check is config-driven (`workspace.config.core.file_extensions`),
  case-insensitive on the extension.
- No hint for `--to`: `topic.md` is a legal bare identifier, and rewriting
  links to `[[topic.md]]` is a defensible (if unusual) choice.

### 3.2 `file rename` — source-not-found cross-hint

When `--from` does not exist on disk (exit 3, unchanged) and a document in
the workspace index has the same file stem, append a second stderr line:

```
downlint: source file not found: /abs/path/report
hint: a document with stem 'report' exists (report.md); did you mean `link rename --from report`?
```

- The stem is `Path::file_stem()` of `--from` (so `report`, `report.md`, and
  `notes/report` all yield `report`), compared **case-sensitively** —
  mirroring `link rename`'s exact-stem planner match, so the hint only fires
  when `link rename` would actually succeed.
- `(report.md)` shows the matched document's workspace-relative path (first
  match when several documents share a stem, e.g. primary + mounted).
- No hint when no document stem matches (plain source-not-found, as today).

### 3.3 Contrasting about lines

The clap `about` text (shown in `downlint file --help` /
`downlint link --help` and each command's `--help`) names the sibling:

- `file rename`: "Move a markdown file or attachment on disk and rewrite
  every link that points at it. Kind-class (markdown vs attachment) is
  inferred from the source file's extension. Use `link rename` to rewrite
  only the identifier (no disk move)."
- `link rename`: "Rewrite a logical link identifier across the workspace. No
  disk move. Use `file rename` to move the file too."

## 4. Non-goals

- **No behavior or exit-code changes.** Hints are advisory; every existing
  success/failure path is byte-identical except for the extra stderr line(s).
- **No hard error** for extension-carrying `--from` on `link rename` — the
  lenient stem-stripping is kept (it is documented planner behavior and
  `[[report.md]]` is a legal link form).
- **No `--to` hint** on either command.
- **No rewording** of `link rename`'s "source file not found: <id>" message
  (it names a file when none is involved — a separate wording issue).
- **No LSP changes.**

## 5. Implementation impact

- `src/cli/rename.rs`:
  - `CliError::SourceNotFound(PathBuf)` → `SourceNotFound { path: PathBuf,
    hint: Option<String> }`; new `CliError::hint()` accessor; both `run_*`
    entry points print `hint: {…}` as a second stderr line when present.
  - `run_rename_file_inner`: stem lookup over `workspace.folder.documents`
    in the not-found branch.
  - `run_rename_link_inner`: extension check after `build_workspace`.
- `src/cli/mod.rs`: the two `about` strings.
- Tests (`tests/cli_rename_tests.rs`): see §6.
- Docs: no spec changes (diagnostics wording is not in `spec/downlint.md`);
  README unchanged.

## 6. Tests

1. `link rename --from report.md` (file exists, link present) → exit 0,
   rename applied, **stderr contains the path-like note**.
2. `link rename --from report` (no extension) → exit 0, **no note**.
3. `file rename --from report` (no file; `report.md` exists) → exit 3,
   **stderr contains the `link rename` hint**.
4. `file rename --from nonexistent` (no stem match) → exit 3, **no hint**.
5. `file rename --help` mentions `link rename`; `link rename --help` mentions
   `file rename`.

## 7. Open questions

None.
