# RFC 0020 — Canonical link-example vocabulary for tests and docs

**Status**: Accepted
**Date**: 2026-09-23
**Scope**: Test fixtures and documentation examples only. No changes to
`src/`, resolution behavior, diagnostics, or the LSP surface.

---

## 1. Summary

Test and doc examples currently use many different names for the same
fixture roles — seven different names for "an unresolved target", five+
source/target pairs for the same rename scenario, three attachment names —
and the `write_vault` helper is copy-pasted into five test files. This RFC
defines a small canonical vocabulary (one name per role), a rule for when
variation is legitimate, and an explicit keep-list of asserted variation.
It also extracts the shared `write_vault` helper into `tests/common/`.

## 2. Motivation

Inventory of `[[…]]` examples across `tests/` (2026-09-23):

- **"Unresolved target" has 7+ names**: `nonexistent` (6×),
  `does-not-exist` (5×), `missing-note`, `Gone` (2×), `broken`, `xyz`,
  `#nope`, `nope.png`.
- **The same rename scenario wears different pairs per file**:
  `report`/`topic`, `old`/`new`, `old-id`/`new-id`, `A`/`B`, `Target`/`Gone`.
- **Attachments**: `photo.png`, `pic.png`, `img.png`, `nope.png`.
- **`write_vault` is copy-pasted** (byte-identical, verified by checksum)
  into `cli_graph_tests.rs`, `cli_info_tests.rs`, `cli_rename_tests.rs`,
  `cli_resolve_tests.rs`, `cli_stdin_tests.rs`.

Costs: a reviewer re-learns the fixture vocabulary per file; an error message
or spec example can't be pattern-matched against a test; copy-paste drift
between suites is invisible.

Two false positives excluded from the inventory: `[[mounts]]`/`[[schemas]]`
are **TOML config section headers** in fixtures, not wiki links; and
`[[Tom MÃ¼ller]]` is a **deliberate mojibake test** (documented at its
call site).

## 3. The rule

> **Vary only what the test asserts on.**

If a test checks *behavior* (resolution, rewrite, exit code, diagnostic),
the names are incidental → normalize to the canonical vocabulary. If a test
checks name *shape* (link syntax, encoding, scheme, special characters,
count, path form), the variation is the assertion → keep it (keep-list, §5).

## 4. Canonical vocabulary

| Role | Canonical | Replaces |
|---|---|---|
| Standard document | `report.md` / `[[report]]` | `note` (stdin suite), `foo` (integration) |
| Rename target | `topic.md` / `[[topic]]` | `new` (the `old`/`new` pair in rename_tests) |
| Linking document | `index.md` | (already mostly so) |
| Unresolved target | `[[missing]]` | `nonexistent`, `does-not-exist`, `missing-note`, `Gone`, `broken`, `xyz` (the unresolved link in `obsidian_prefix_off_no_hint_no_match`) |
| Unresolved markdown link | `[x](missing.md)` | (already so in graph suite) |
| Unresolved attachment | `missing.png` | `nope.png` |
| Bad heading in valid file | `#missing` | `#nope` |
| Resolved attachment | `image.png` | `photo.png`, `pic.png`, `img.png` |
| Nested document | `notes/report.md` | `notes/old.md` |
| Link-rename identifiers | `old-id` / `new-id` | (already so) |

Semantic **role names** are already canonical and stay: `target` (graph
query target), `hub`, `alpha`, `beta`, `lonely`, `self` (coverage roles),
`reference.md` (the referencing note in the Unicode family), `nope.md`
(file-not-in-index — a distinct role from unresolved-link), and the generic
note convention `a.md`/`b.md`/`c.md`.

## 5. Keep-list (asserted variation — do not touch)

- **Link syntax variants**: `[[report.md]]`, `[[report#section]]`,
  `[[report|alias]]`, `[[report#section|alias]]`, `[[report.md|alias]]`,
  and the heading-only link *form* `[[#…]]` (the form is asserted; the
  heading name itself normalizes to `#missing` when incidental).
- **Scheme URIs**: `scheme://`, `onedrive://`, `icloud://`, `s3://`
  (`tests/schemas.rs`).
- **Unicode / encoding**: `José García`, `Zoë Müller`, `Tom MÃ¼ller`
  (deliberate mojibake), `Team knowledge transfer (QA/DB)` (parens + spaces
  in a filename).
- **Real-world families** (internally consistent; the shape is the point):
  `20260801-topic-a*` and `20260723-v2.5b-*` (incl. the prefix-ambiguity
  pair `20260723-v2.5b-trust-region` / `20260723-v2.5b-`),
  `20260523-us-short-code-lifecycle/*` (spaces in filenames).
- **Prefix-matching mechanism**: `xyz-topic.md` in
  `obsidian_prefix_does_not_match_suffix_only` — the unrelated prefix is
  the assertion (a suffix-only match must not resolve).
- **Mount / namespace families**: `shared/b*` (incl. `../shared/b`),
  `/kb/foo`, `/people/john-doe|Some Name`, `/cases/audit/`, `folder/`,
  `ext/doc.md` — namespace and traversal resolution is the assertion.
- **LSP stem/slug mechanism** (`lsp_completion_tests.rs`): `alpha`/`beta`/
  `gamma` with H1 titles "Alpha Title" etc. — the stem-vs-slug difference
  is the assertion.
- **Attachment-extension tests**: `relative.docx`, `same-dir.csv` — the
  extension class is the assertion.
- **TOML config sections**: `[[mounts]]`, `[[schemas]]` — not links.
- **Conceptual notation** in spec/README: `[[Note#H]]` (grammar
  illustration), `[[wiki-links]]` (prose).

## 6. Per-file scope

| File | Changes |
|---|---|
| `tests/common/mod.rs` (new) | Shared `write_vault` (extracted verbatim) |
| `tests/cli_{graph,info,rename,resolve,stdin}_tests.rs` | Use `common::write_vault`; drop local copies |
| `tests/cli_rename_tests.rs` | `photo.png`/`pic.png` → `image.png` |
| `tests/rename_tests.rs` | `nonexistent` → `missing`; the one `old`/`new` + `#head` test → `report`/`topic` + `#section` |
| `tests/cli_graph_tests.rs` | `Gone` → `missing`; `img.png` → `image.png` |
| `tests/cli_stdin_tests.rs` | `note` → `report`; `missing-note` → `missing`; `nope.png` → `missing.png`; `#nope` → `#missing` |
| `tests/lsp_completion_tests.rs` | `broken` → `missing` |
| `tests/integration.rs` | `does-not-exist` → `missing` (5×); `#nope` → `#missing`; `[[xyz]]` → `[[missing]]` (unresolved); incidental `foo` → `report` (context-checked per test); `xyz-topic.md` kept |
| `tests/cli_info_tests.rs`, `tests/cli_resolve_tests.rs`, `tests/schemas.rs` | Helper only; no name changes |
| `spec/downlint.md`, `README.md` | README blocked-rename example: `[[2024-q1]]` → `[[missing]]`; any other incidental examples aligned |

## 7. Non-goals

- **No `src/` changes** — its examples (`report`/`report.md`) are already
  canonical.
- **No RFC rewrites** — RFCs are historical snapshots.
- **No behavior, diagnostic, or exit-code changes.**
- **No renaming of semantic role names** (§4) or keep-list families (§5).

## 8. Verification

1. Full `cargo test` (12 suites) — a pure fixture-text change; anything
   failing means a rename crossed into asserted variation.
2. `cargo clippy --all-targets` clean on changed files.
3. Re-run the inventory grep: every remaining non-canonical name must be
   explainable by the keep-list.
