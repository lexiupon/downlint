# RFC 0023 — Hidden files are excluded by default; `core.include_hidden` opt-in

**Status**: Proposed
**Date**: 2026-09-23
**Scope**: workspace discovery (`src/utils/workspace.rs`), configuration
(`src/config/mod.rs`). No changes to the parser, resolution rules, slugs,
diagnostics, rename, or the LSP surface (the LSP inherits the change via
the shared walk).

---

## 1. Summary

The spec (RES-10) says:

> **Hidden files and directories are excluded by default** — dotfiles are
> not discovered as documents.

The code does the opposite. Both walkers run
`WalkBuilder::…hidden(false)` — i.e. *do not ignore* hidden entries — so
dotfiles and dot-directories are discovered, parsed, linted, and resolvable
as link targets. `hidden(false)` has been in `collect_documents` since the
initial commit; `hidden(true)` never existed in the file.

This RFC makes the code match the spec: hidden entries are excluded by
default, and a new config key `core.include_hidden` (bool, default `false`)
restores the current include-everything behavior for users who keep notes
in dotfiles or dot-directories.

## 2. Motivation

Real-world noise (Obsidian-shaped vault, verified against the 0.15.5
binary — all three links resolve, exit 0):

| Link | Target | Effect today |
|---|---|---|
| `[[old]]` | `.trash/old.md` | Obsidian's local trash keeps deleted notes as `.md` files; they stay indexed, so links to trashed notes silently resolve, trashed notes get linted, and they pollute completions and the graph |
| `[[t]]` | `.obsidian/templates/t.md` | plugin/templates markdown indexed as first-class documents |
| `[[.hidden]]` | `.hidden.md` | dotfile notes indexed (some users want this — hence the opt-in) |

The error is also enshrined in the amendment history: the first row of
`spec/linting.md` §8 claims "the code won: … hidden files excluded by
default" — a false statement about the code, which records the spec's claim
as the code's behavior. After this RFC, that row becomes true.

## 3. The rule

1. Both walkers (`collect_documents` and `indexable_paths_under`, the
   RFC 0022 reconciliation walk) use `hidden(!config.core.include_hidden)`.
   Default `false` → `hidden(true)` → dotfiles and dot-directories are not
   discovered as documents.
2. New config key `core.include_hidden` (bool, default `false`), following
   the existing `core.*` pattern (parse with `unwrap_or(default)`, merge
   with `high.or(low)`). `true` → `hidden(false)` → the pre-0.15.6
   behavior, exactly.
3. **No other mechanism re-includes hidden files.** Verified against the
   `ignore` crate: the walker's hidden filter is a separate mechanism from
   gitignore/override rules and takes precedence over them — a
   `core.ignore` negation (`!.hidden.md`) or a `.gitignore` negation does
   **not** re-include a hidden file when `include_hidden = false`. The
   opt-in key is the only way back.
4. **Force-added root symlinks are unaffected.** The root-symlink force-add
   (so a symlinked dir at the vault root is traversed even when
   `.gitignore` excludes it) enumerates the root with raw `read_dir` and
   calls `builder.add(…)`. Verified: an explicitly added hidden symlink at
   the root **is** traversed under `hidden(true)`. An explicit root-level
   entry is a deliberate configuration choice, so it stays reachable
   regardless of `include_hidden`.
5. `.gitignore` discovery is unaffected: the `ignore` crate reads
   `.gitignore`/`.ignore` files as part of its gitignore machinery
   regardless of the hidden setting (verified: a gitignored non-hidden file
   is still excluded under `hidden(true)`).

## 4. Behavior matrix

| Case | Before (0.15.5) | After (default) | After (`include_hidden = true`) |
|---|---|---|---|
| `[[old]]` → `.trash/old.md` | resolves | **broken** | resolves |
| `[[t]]` → `.obsidian/templates/t.md` | resolves | **broken** | resolves |
| `[[.hidden]]` → `.hidden.md` | resolves | **broken** | resolves |
| `[[plain]]` → `plain.md` | resolves | resolves | resolves |
| gitignored `secret.md` | excluded | excluded | excluded |
| `core.ignore = ["!.hidden.md"]` | resolves (hidden walked anyway) | **broken** (negation cannot re-include) | resolves |
| hidden symlink at root (force-added) | traversed | traversed | traversed |

## 5. Alternatives considered

1. **Fix the spec to match the code** (declare hidden files included).
   Rejected: the spec has said "excluded" since the bootstrap, and
   `.trash`/`.obsidian` noise is a real trust problem for `link/broken` —
   a link to a trashed note resolving as live is exactly the kind of false
   negative the diagnostic exists to catch.
2. **Exclude with no opt-in.** Rejected: verified that no ignore mechanism
   can re-include hidden files, so users with dotfile notes would have *no*
   way to link to them. The spec's "excluded **by default**" implies an
   override; the key makes that true.
3. **Per-path opt-in via `core.ignore` negation.** Rejected: the crate's
   hidden filter takes precedence over override rules (verified), so this
   would require reimplementing hidden handling outside the walker — more
   code, and a second source of truth for what "hidden" means.

## 6. Non-goals

- No change to the LSP freshness mechanisms (RFC 0022) — reconciliation
  keeps using the same walk, so snapshot and reconciliation stay in sync by
  construction.
- No change to attachment existence checks (RES-06): those already bypass
  ignore filtering, and they do so for hidden paths too (a link to
  `.assets/img.png` still resolves as an attachment when the file exists —
  same as today).
- No change to the CLI `resolve`/`graph`/`info` surfaces beyond what the
  shared walk implies.
- No new diagnostics.

## 7. Spec changes

- RES-10: keep "excluded by default"; add the `core.include_hidden` opt-in
  sentence and the note that gitignore/`core.ignore` negation cannot
  re-include hidden files.
- §5.3 key table: new `core.include_hidden` row (default `false`).
- §8: amendment history row.

## 8. Verification

1. Unit: `indexable_paths_under` filtering test flipped — hidden file and
   hidden directory excluded by default; a `include_hidden = true` variant
   restores inclusion.
2. Config unit tests: `include_hidden` parses, defaults to `false`, and
   merges with project-over-user precedence (mirroring the `ignore`-key
   tests).
3. Integration (CLI): `[[.hidden]]` with `.hidden.md` present →
   `link/broken`, exit 1; same vault with `core.include_hidden = true` →
   resolves, exit 0.
4. Full `cargo test` + `cargo clippy --all-targets` (no new warnings).
