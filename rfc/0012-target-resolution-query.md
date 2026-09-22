# RFC 0012 — Target Resolution Query (`downlint resolve`)

**Status**: Proposal (open)
**Date**: 2026-09-22
**Follows**: RFC 0010 (Mounts and Schemes), RFC 0011 (mount path collision)
**Config impact**: none (no config keys added or removed)

---

## 1. Summary

`downlint check` reports `link/ambiguous` and `link/broken` diagnostics, but answering
the follow-up question — *"what does this target actually resolve to?"* — requires
running a full check, filtering JSON diagnostics, and accepting that the answer only
exists for targets that are already linked somewhere.

This RFC adds a query subcommand:

```
downlint resolve <TARGET>
```

Given a link target (or H1 title), it runs the **same matching pipeline** as
wiki-link resolution (RES-03 / RES-05 / RES-06, co-equal across primary and mounted
documents per RFC 0010) and reports **every** destination with the **reason** each
matched. It additionally supports:

- `#anchor` suffixes — per-destination anchor existence;
- explicit paths and folder targets — document, attachment, and directory outcomes;
- URI-scheme targets — `[[schemas]]` mapping status (mapped present/missing/placeholder,
  unmapped, external web).

The command is read-only, scriptable (stable text + JSON output, precise exit codes),
and introduces **no new diagnostics** and **no new matching semantics**: it is a
projection of the existing resolution rules.

---

## 2. Motivation

### 2.1 The debugging loop is broken today

A typical `check` output:

```
areas/messaging-product.md:92:7: error: Ambiguous link: 'Avon' resolves to multiple destinations [link/ambiguous]
notes/20260224-finance-master-data-reference-files.md:21:136: warning: Broken link: 'onedrive://SinchAB/assets/master-data/customers-master-data-sap-byd-23022023.xlsx' could not be resolved [link/broken]
```

The natural next question is *"which documents?"* / *"why is the onedrive link broken?"*
Today the only answer path is:

```
downlint check --format json | jq '.[] | select(.code=="link/ambiguous") | .related'
```

which is clunky and incomplete:

- the destination list is buried in `related[]` as **absolute paths only** — no H1
  title, no match reason (stem vs title vs prefix), no mount attribution;
- it only answers for targets that are **already linked somewhere** — you cannot ask
  what `[[Avon]]` would resolve to before writing the link, or look up a title you saw
  in an H1;
- no anchor checking, no source context for relative targets, no URI mapping status
  (missing file vs evicted placeholder vs no schema match are all "could not be
  resolved").

### 2.2 The fix loop

With `resolve`, the ambiguous-link fix loop becomes:

```
$ downlint resolve 'Avon'          # see all candidates, titles, and why they match
# pick the intended document, then disambiguate:
$ downlint rename-link --from avon-vendor --to avon-vendor-2025
```

And the URI debugging loop:

```
$ downlint resolve 'onedrive://SinchAB/assets/master-data/customers.xlsx'
mapped-missing — onedrive://SinchAB/ → /Users/x/OneDrive-Work/SinchAB/assets/master-data/customers.xlsx
  mapped file does not exist
```

instead of an opaque `link/broken`.

---

## 3. Goals and Non-Goals

**Goals**
- Answer "what does target T resolve to?" for any target, linked or not.
- Report every destination with its match reason(s), H1 title, and mount attribution.
- Use **exactly** the existing matching rules (RES-03/05/06/07) — the command must
  predict `check`'s behavior.
- Report URI-scheme mapping status (present / missing / placeholder / unmapped /
  external).
- Be scriptable: stable text and JSON output, precise exit codes.
- Support source context (`--from`) for relative targets and per-destination anchor
  checking.

**Non-Goals**
- No new diagnostics, no changes to `check` output or exit codes.
- No new matching semantics (no fuzzy/typo-tolerant matching, no new config keys).
- No LSP integration in v1 (the query layer is designed to be reusable, but no editor
  surface ships).
- No batch mode (one target per invocation).
- No anchor support on URI targets (consistent with RES-07).
- No renaming/fixing — this is a query; fixes go through `rename-link` / `rename-file`.

---

## 4. The Change

### 4.1 Command and flags

```
downlint resolve <TARGET> [options]
```

| Flag | Default | Effect |
|---|---|---|
| `<TARGET>` | *(required)* | The link target, in wiki-link target grammar: bare title/stem, explicit path, folder target (trailing `/`), optional `#anchor`, or `scheme://…`. An empty target, or a target consisting only of `#anchor`, is a bad argument (exit 2); for in-page anchors use `--from`. |
| `--root <DIR>` | inferred | Workspace root override (same inference as `check`: walk up for `.downlint.toml` / `.git`). |
| `--from <DOC>` | workspace root | Resolve relative targets as if the link were in this document (workspace-relative or absolute path; must name a document in the index — primary or mounted — else exit 2). The source directory is the document's containing directory. Without it, relative targets resolve against the workspace root — the same base as the stdin document. |
| `--format <text\|json>` | `text` | Output format. |
| `--include-prefix` | off | Also list prefix candidates when `wiki.obsidian_prefix` is `false`. Candidates are advisory: they never affect status or exit code, and are not capped (unlike `check`'s 5-name hint). A no-op when the config flag is already `true`. |
| `--allow-uri-sync` | off | Permit `verify_cmd` subprocess execution for URI targets (same safety gate as `check`). Without it, `verify_cmd` is skipped (treated as inconclusive). |
| `-v / --verbose <N>` | `2` | Logging to stderr. |
| `-q / --quiet` | off | Accepted (global flag) but ignored: a query's output is the answer. |

### 4.2 Pipeline

`resolve_target(input, source_dir, target, include_prefix, allow_sync)`:

1. **Scheme** (`has_scheme(target)`):
   - External web scheme (LNK-03 list: `http`, `https`, `ftp`, `ftps`, `mailto`, `tel`,
     `sms`, `irc`, `xmpp`) → status `external`. The target resolves to itself, outside
     the workspace; no destinations.
   - `UriResolver::resolve` → `NoMapping` → status `unmapped` (with a note distinguishing
     "no `[[schemas]]` configured" from "no prefix matched").
   - `UriResolver::resolve` → `Resolved` → stat + verify (same logic as RES-07):
     - exists and not a placeholder → `mapped-present`;
     - exists but an evicted placeholder (built-in heuristics or `verify_cmd`) →
       `mapped-placeholder`;
     - does not exist → `mapped-missing`.
   - Anchors on URI targets: the anchor is split off first (RES-07 operates on the path
     part); the mapping status is computed for the path part, and the anchor is reported
     as **not supported** — `check` would emit `link/broken-anchor` for such a link.
     The anchor does not change the mapping status.

2. **Folder target** (no scheme, path part ends with `/`, per LNK-02):
   - A folder target with an anchor is invalid (RES-04: folder links do not take
     headings) → status `broken`.
   - Otherwise resolved per RES-04 (workspace-absolute against the root, relative
     against `source_dir`, mounts reachable via their `prefix`):
     - existing directory → status `resolved`, one destination of kind `directory`;
     - missing, or a file instead of a directory → status `broken`, no destinations.

3. **Document / attachment** (everything else):
   - Split off the `#anchor` (first unescaped `#`, per LNK-02).
   - **Explicit path** (`is_explicit_path`, LNK-02): document matching by path
     (filesystem / namespace / workspace-absolute, ASCII case-insensitive, per
     RES-03) → kind `path`. A path-like target may additionally fall back to a
     title-slug match (RES-03) → kind `title` (e.g. a title containing `/`). If no
     document matches and the target is file-like (`is_attachment_candidate_path`),
     the filesystem fallback (RES-06) applies: existing file → one destination of
     kind `attachment`; missing → `broken`.
   - **Bare target**: co-equal matching across primary + mounted documents:
     - file stem, ASCII case-insensitive → kind `stem`;
     - document title slug (H1, RES-01) → kind `title`;
     - Prefix match (RES-05) → kind `prefix` — included as a regular destination when
       `wiki.obsidian_prefix = true`; when the flag is `false`, prefix candidates are
       listed only under `--include-prefix` as advisory (see §4.5). Never applied to
       explicit-path targets (RES-05).
   - A single document may match via several rules (e.g. stem **and** title);
     destinations are deduplicated by filesystem path and the match kinds are unioned.
   - **Anchor check** (document destinations only): the anchor is looked up in each
     destination's heading slugs (strict, then the tolerant folded-slug fallback, per
     RES-01) and tags (ASCII case-insensitive raw anchor) — the same lookup the
     in-page/cross-document anchor rules use. Each destination reports
     `anchor: true|false`. The anchor is **advisory**: it does not change the status or
     exit code (document resolution is the question; anchor diagnostics belong to
     `check`'s `link/broken-anchor`).
   - Status by destination count: exactly one → `resolved`; more than one →
     `ambiguous`; zero → `broken`.

### 4.3 Statuses and exit codes

| Status | Meaning | Exit |
|---|---|---|
| `resolved` | Exactly one destination (document, attachment, or directory) | `0` |
| `ambiguous` | More than one destination | `1` |
| `broken` | No destination | `1` |
| `external` | External web scheme (resolves to itself, out of scope) | `0` |
| `unmapped` | Non-web scheme, no `[[schemas]]` prefix matched | `1` |
| `mapped-present` | Mapped, file exists, not a placeholder | `0` |
| `mapped-missing` | Mapped, file does not exist | `1` |
| `mapped-placeholder` | Mapped, file is an evicted cloud placeholder | `1` |

`2` is reserved for bad arguments / config errors (consistent with `check`).

Rationale for `1` on both `broken` and `ambiguous` (decided): the scriptable contract
is *"exit 0 iff the target is safe to use as a link as-is"*; the output distinguishes
the two cases.

### 4.4 Output

**Text** — one line per destination: namespace path, H1 title, match kinds, mount
attribution, anchor status (when an anchor was given):

```
$ downlint resolve 'Avon'
ambiguous — 4 destinations:
  notes/avon-customer.md         title: Avon              [stem, title]
  notes/avon-vendor.md           title: Avon              [stem, title]
  areas/avon-2025-review.md      title: Avon 2025 Review  [title]
  /kb/avon-onboarding.md         title: Avon              [title] (mount: /kb)

$ downlint resolve 'Avon#onboarding'
ambiguous — 4 destinations:
  notes/avon-customer.md         title: Avon              [stem, title]  anchor: no
  notes/avon-vendor.md           title: Avon              [stem, title]  anchor: no
  areas/avon-2025-review.md      title: Avon 2025 Review  [title]        anchor: no
  /kb/avon-onboarding.md         title: Avon              [title] (mount: /kb)  anchor: yes

$ downlint resolve 'finance-master-data'
broken — no destination:
hint: 1 prefix candidate (enable wiki.obsidian_prefix to match):
  notes/finance-master-data-reference-files.md  [prefix]

$ downlint resolve 'onedrive://SinchAB/assets/master-data/customers.xlsx'
mapped-missing — onedrive://SinchAB/ → /Users/x/OneDrive-Work/SinchAB/assets/master-data/customers.xlsx
  mapped file does not exist

$ downlint resolve 'icloud://assets/report.pdf'
mapped-present — icloud://assets/ → /Users/x/Mobile Documents/iCloud~com~example/assets/report.pdf

$ downlint resolve 'icloud://assets/evicted.pdf'
mapped-placeholder — icloud://assets/ → /Users/x/Mobile Documents/iCloud~com~example/assets/evicted.pdf
  evicted cloud placeholder detected (iCloud)

$ downlint resolve 'https://example.com/x'
external — resolves outside the workspace
```

`resolved` prints `resolved — 1 destination:` with the single line; `unmapped` prints
`unmapped — no [[schemas]] prefix matched 'onedrive://…'` (plus, when schemas are
configured, a reminder that more-specific prefixes go first).

**JSON** — one object:

```json
{
  "target": "Avon",
  "anchor": "onboarding",
  "status": "ambiguous",
  "destinations": [
    {
      "path": "notes/avon-customer.md",
      "title": "Avon",
      "match": ["stem", "title"],
      "mount": null,
      "anchor": false
    },
    {
      "path": "/kb/avon-onboarding.md",
      "title": "Avon",
      "match": ["title"],
      "mount": "/kb",
      "anchor": true
    }
  ],
  "prefix_candidates": [],
  "scheme": null
}
```

Field notes:

- `path` — namespace-relative for documents (primary: workspace-relative; mounted:
  `prefix/rel`); workspace-relative filesystem path for attachments and directories.
- `title` — the document's title (H1 when `core.title_from_heading` is on, else the
  stem); `null` for attachments and directories.
- `match` — canonical order `["path", "stem", "title", "prefix"]` (subset, in that
  order); `attachment` and `directory` are single-element arrays.
- `anchor` (top-level) — the anchor part, or `null`.
- `destinations[].anchor` — `null` when no anchor was given or the destination is not a
  document (attachment/directory); otherwise `true|false`.
- `prefix_candidates` — populated only under `--include-prefix` when
  `wiki.obsidian_prefix` is `false`; same shape as `destinations` with
  `"match": ["prefix"]`. Never affects `status`.
- `scheme` — `null` for non-URI targets; for URI targets:
  `{"scheme": "onedrive", "mapped_path": "…", "exists": false, "placeholder": false,
  "verify": "skipped"|"passed"|"failed"|"not-configured"}` (the `verify_cmd` state;
  `skipped` when `--allow-uri-sync` is absent). For `external`, `mapped_path`,
  `exists`, and `placeholder` are `null` and `verify` is `"not-applicable"`.

### 4.5 Worked examples

| Target | Workspace | Status | Why |
|---|---|---|---|
| `Avon` | `avon-customer.md` (H1 `Avon`), `avon-vendor.md` (H1 `Avon`) | `ambiguous` | stem + title, two docs |
| `Avon` | `notes/avon.md` (stem) + `other.md` (H1 `Avon`) | `ambiguous` | one doc by stem, another by title — co-equal |
| `Avon` | only `notes/avon.md` (H1 `Avon`) | `resolved` | stem + title, one doc (kinds unioned) |
| `avon` | `notes/avon.md` | `resolved` | stem, case-insensitive |
| `/notes/avon.md` | `notes/avon.md` | `resolved` | explicit path, kind `path` |
| `/notes/avon.md` | missing | `broken` | explicit path, no doc, not file-like on disk |
| `/assets/diagram.drawio` | file exists | `resolved` | attachment fallback (RES-06) |
| `notes/` | folder exists | `resolved` | directory (RES-04) |
| `notes/` | missing | `broken` | missing directory |
| `finance-master-data` | only `notes/finance-master-data-reference-files.md`, prefix off | `broken` (+ advisory candidates under `--include-prefix`) | RES-05 gated by config |
| same, `obsidian_prefix = true` | same | `resolved` | prefix match is a regular destination |
| `../shared/b.md` with `--from notes/a.md` | `shared/b.md` | `resolved` | dot-relative against `notes/` (attachment fallback, RES-06 — same as `check`) |
| `../shared/b.md` without `--from` | `shared/b.md` | `broken` | dot-relative against root → outside the workspace |
| `#onboarding` (anchor only) | any | exit 2 | bad argument — in-page anchors need `--from` |
| `notes/#h` (folder + anchor) | any | `broken` | RES-04: folder links do not take headings |
| `onedrive://x/a.xlsx` | schema mapped, file present | `mapped-present` | RES-07 |
| `onedrive://x/a.xlsx` | schema mapped, file missing | `mapped-missing` | RES-07 |
| `icloud://assets/a.pdf` | schema mapped, `.icloud` sibling present | `mapped-placeholder` | RES-07 auto-verify |
| `onedrive://x/a.xlsx` | no schemas configured | `unmapped` | no prefix can match |
| `https://e.com/x` | any | `external` | LNK-03 |

### 4.6 Implementation shape

- `src/resolution/query.rs` (new): `MatchKind`, `TargetDestination`, `ResolveStatus`,
  `SchemeReport`, `TargetResolution`, and
  `resolve_target(&ResolveInput, source_dir, target, include_prefix, allow_sync)`.
- The per-document matching rules in `find_doc_matches` (`src/resolution/mod.rs`) are
  extracted into a shared `match_document_kinds(doc, source_dir, root, target,
  explicit, is_wiki) -> Vec<MatchKind>` so the rules are single-sourced;
  `find_doc_matches` is rebuilt on top (documents with ≥1 kind, deduped by path) and
  must preserve its exact current behavior.
- `run_verify_cmd` (currently private in `resolution/mod.rs`) becomes `pub(crate)` (or
  moves to `auto_verify.rs`) for reuse by the query.
- `src/cli/resolve.rs` (new): clap args, `discover_workspace` + `ResolveInput::from_workspace`
  (full workspace, same as `check`), text/JSON rendering, exit codes.
- `src/cli/mod.rs`: `Resolve` subcommand + dispatch.

---

## 5. Diagnostics

None. `resolve` is a query command: it emits no diagnostics and does not change any
diagnostic's condition, message, or severity. In particular, `link/ambiguous` and
`link/broken` are unchanged; `resolve` is their companion for the "which ones?"
question.

---

## 6. Spec Impact (clauses)

- **`spec/downlint.md`**:
  - Subcommand list: add `downlint resolve`.
  - New `**resolve**` section: flags (§4.1), pipeline reference (RES-03/04/05/06/07 —
    the command MUST use the existing rules, no new semantics), statuses and exit
    codes (§4.3), output formats (§4.4).
- **`spec/linting.md`**:
  - §8 Amendment History: add a row for RFC 0012.
  - No RES clause changes: the matching rules are normative as-is; the query is a
    projection of them. A one-line note in §2 (Link Model) pointing at the query
    command for destination inspection is optional polish.

---

## 7. Migration

Purely additive: a new subcommand. No config change, no wire-format change, no
behavior change to `check`, `server`, `rename-file`, or `rename-link`. Existing
scripts are unaffected.

---

## 8. Alternatives Considered

- **`jq` over `check --format json`** (status quo) — rejected: buried in diagnostics,
  absolute paths only, no titles/match reasons/mounts/anchors, and only for already
  linked targets.
- **LSP-only surface** (hover/code lens listing destinations) — deferred: useful, but
  requires an editor and cannot serve scripting; the CLI query layer is reusable by the
  LSP later.
- **Names `which` / `find` / `whereis`** — rejected: `resolve` matches the spec's
  resolved/ambiguous/broken vocabulary and the internal `resolution` module.
- **Distinct exit codes for `broken` vs `ambiguous`** (e.g. 1 vs 3) — rejected (decided):
  the useful scriptable contract is "0 iff safe to link"; the output disambiguates.
- **Prefix candidates affecting status under `--include-prefix`** — rejected: the
  command must predict `check` by default; advisory candidates stay advisory.
- **Batch mode (multiple targets)** — deferred: one target per invocation is sufficient
  for the fix loop; batching can layer on without changing the single-target contract.

---

## 9. Open Questions

1. **Anchor miss on a unique destination** — advisory only (proposal: exit stays `0`
   when the document resolves; `check` remains the authority on `link/broken-anchor`).
   Alternative: exit `1` when the anchor is missing in the unique destination.
2. **`--from` validation** — error (exit 2) when the given document does not exist in
   the workspace (proposal), or accept any path and use only its directory? Proposal:
   require an existing workspace document — a typo'd `--from` silently changing the
   resolution base is worse than an error.
3. **Text output alignment** — fixed-width columns (proposal, as in §4.4) vs
   space-separated. Proposal: aligned columns; titles are truncated to 40 chars with
   `…`.

---

## 10. Test Plan (conformance)

CLI-level (`tests/cli_resolve_tests.rs`, `assert_cmd` + `tempfile`, mirroring
`cli_stdin_tests.rs`):

- `resolve_unique_stem_resolves` — one doc, stem match → exit 0, status `resolved`.
- `resolve_unique_title_resolves` — target matches only via H1 slug → exit 0.
- `resolve_stem_and_title_union_kinds` — one doc matching both → one destination,
  `match: ["stem", "title"]`.
- `resolve_ambiguous_multiple_destinations` — two docs, same title → exit 1, both
  listed.
- `resolve_broken_no_destination` — no match → exit 1, status `broken`.
- `resolve_explicit_path_document` — `/notes/avon.md` → exit 0, kind `path`.
- `resolve_explicit_path_attachment_present` / `_missing` — RES-06 fallback.
- `resolve_folder_target_existing` / `_missing` — RES-04.
- `resolve_anchor_reports_per_destination` — `Avon#onboarding` → per-dest `anchor`
  flags, exit code unchanged by anchor.
- `resolve_mount_attribution` — mounted doc listed with `mount` attribution.
- `resolve_from_relative_context` — `--from notes/a.md` + `../shared/b` resolves
  against `notes/`; without `--from` it is `broken`.
- `resolve_from_missing_doc_is_error` — `--from` pointing at a nonexistent doc →
  exit 2.
- `resolve_anchor_only_target_is_error` — `#onboarding` without a document part →
  exit 2.
- `resolve_folder_with_anchor_is_broken` — `notes/#h` → `broken` (RES-04).
- `resolve_explicit_path_title_fallback` — a title containing `/` (e.g. `QA/DB`)
  matches via the title-slug fallback → kind `title`.
- `resolve_prefix_off_by_default` — prefix-only match, config off → `broken`, no
  candidates shown.
- `resolve_include_prefix_shows_advisory_candidates` — exit still 1, candidates
  listed, status unchanged.
- `resolve_prefix_enabled_config` — `obsidian_prefix = true` → regular destination,
  exit 0.
- `resolve_uri_mapped_present` / `_missing` / `_placeholder` — `[[schemas]]` with a
  temp root; placeholder via an iCloud-style `.icloud` sibling.
- `resolve_uri_unmapped` — non-web scheme, no matching prefix → exit 1.
- `resolve_uri_external_web` — `https://…` → exit 0, status `external`.
- `resolve_json_format_shape` — JSON object with `target`/`anchor`/`status`/
  `destinations`/`prefix_candidates`/`scheme`.

Unit-level (`src/resolution/query.rs` or `tests/integration.rs`):

- `match_document_kinds` parity test: for a corpus of (doc, target) pairs, the
  documents selected by the refactored `find_doc_matches` equal those with ≥1 kind —
  guarding the refactor.

---

## 11. Decision Requested

Approve the `downlint resolve` subcommand (§4) with: the status/exit-code table (§4.3,
including exit `1` for both `broken` and `ambiguous`), URI-scheme support in v1,
`--include-prefix` off by default with advisory-only candidates, anchor checking as
advisory, and the open questions in §9 resolved as proposed.
