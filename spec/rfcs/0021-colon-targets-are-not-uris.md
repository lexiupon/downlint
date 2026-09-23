# RFC 0021 — Colon-bearing link targets are not URIs

**Status**: Accepted
**Date**: 2026-09-23
**Scope**: RES-07 scheme detection in `src/resolution/` (link resolution and
the `resolve` subcommand). No changes to the parser, slugs, diagnostics,
rename, or the LSP surface.

---

## 1. Summary

A link target containing a colon — e.g. `[[Team: Knowledge]]` — is
misclassified as a URI-scheme target and routed to RES-07, where it comes
back `NoMapping` and is reported as `link/broken` (plus a `uri/no-mapping`
hint when `[[schemas]]` are configured). Title-slug, stem, and alias
matching never run, so a document titled or named `Team: Knowledge` is
unreachable by any link form.

This RFC tightens scheme detection: a target is routed to scheme resolution
only when it is genuinely URI-like —

1. its scheme is a known external web scheme (the LNK-03 list), or
2. it uses the `scheme://` form, or
3. it matches a configured `[[schemas]]` prefix.

Everything else falls through to normal document/attachment resolution.

## 2. Motivation

Repro (downlint 0.15.3):

```
$ cat notes/team-knowledge.md
# Team: Knowledge

$ cat index.md
[[Team: Knowledge]]

$ downlint check
index.md:1:1  link/broken  Broken link: 'Team: Knowledge' could not be
                            resolved
```

Why it happens: RES-07 detection is purely syntactic — "a target has a URI
scheme when the part before the first `:` is non-empty and consists of
scheme characters `[A-Za-z0-9+-.]`." `Team: Knowledge` satisfies that
(prefix `Team`), so all three resolution entry points
(`resolve_wiki_ref`, `resolve_inline_ref`, and the `resolve` subcommand's
`resolve_target`) hand it to `UriResolver`, which finds no matching
`[[schemas]]` prefix and reports `NoMapping`. The slug machinery would have
matched fine (`Team: Knowledge` → `team-knowledge` on both sides); it is
never reached.

Colons in titles are common in real vaults (`Team: Knowledge`,
`Q&A: 2025`, `Note: do not merge`), and Obsidian — the link model downlint
follows — treats `[[Team: Knowledge]]` as an ordinary note link.

## 3. The rule

A target is a **URI-scheme target** (routed to RES-07) iff it has a
syntactic scheme (`scheme_of` succeeds) **and** at least one of:

1. **Web scheme** — the scheme is in the LNK-03 list (`http`, `https`,
   `ftp`, `ftps`, `mailto`, `tel`, `sms`, `irc`, `xmpp`,
   case-insensitive). These must keep being skipped silently; their
   scheme-specific part never contains a space, so the rule cannot
   misfire on a note title.
2. **`scheme://` form** — the target contains `://`. This is the URI
   grammar the spec and the `[[schemas]]` examples use
   (`icloud://assets/`, `onedrive://work/`). Unmapped `://` targets
   (`s3://other/file.md`) keep their current behavior: broken, with the
   `uri/no-mapping` hint when schemas are configured.
3. **Configured prefix** — a `[[schemas]]` `uri` prefix matches the target
   (the same longest-prefix match `UriResolver::resolve` uses). This
   preserves schemas declared without `//` (e.g. `uri = "team:assets/"`).

All other colon targets — including `Team: Knowledge` and
`Team:Knowledge` (no space) — fall through to normal resolution: explicit
path, stem, title-slug, prefix, and attachment rules apply as usual.

Implementation: `UriResolver::is_uri_target(target)` (with a
`has_mapping(target)` helper reusing the existing prefix match) replaces
the `has_scheme(target)` gate at the three entry points. `has_scheme`
stays as the resolver's own internal gate.

## 4. Behavior matrix

| Target | Before | After |
|---|---|---|
| `[[Team: Knowledge]]`, doc titled `Team: Knowledge` exists | broken | **resolves** (title slug) |
| `[[Team: Knowledge]]`, file `Team: Knowledge.md` exists | broken | **resolves** (stem) |
| `[[Team: Knowledge\|alias]]`, doc exists | broken | **resolves** |
| `[[Team: Knowledge]]`, no doc, no schemas | broken | broken (unchanged) |
| `[[Team: Knowledge]]`, no doc, schemas configured | broken + `uri/no-mapping` | **broken only** |
| `[x](Team: Knowledge.md)`, file exists | broken | **resolves** |
| `[[s3://other/file.md]]`, schemas configured | broken + hint | broken + hint (unchanged) |
| `[[onedrive://work/x]]`, schema configured | mapped | mapped (unchanged) |
| `[[mailto:foo@bar.com]]` | skipped silently | skipped silently (unchanged) |
| `[[team:assets/x]]`, schema `uri = "team:assets/"` | mapped | mapped (unchanged) |
| `[[team:other/x]]`, schema `uri = "team:assets/"` | broken + hint | broken (hint dropped) |

The only lost signal is the `uri/no-mapping` hint for a non-`//` scheme
that does not match any configured prefix. That hint pointed at
`[[schemas]]` configuration that (by premise) already exists, so the
broken-link diagnostic remains the actionable one.

## 5. Alternatives considered

1. **Require no whitespace in the scheme-specific part**
   (`Team: Knowledge` has a space → not a URI). Rejected: it leaves
   `Team:Knowledge` (no space) broken — the bug report is about colons in
   targets, not about the space after them — and it encodes a URI
   grammar downlint never documented.
2. **Require `://` for every non-web scheme.** Rejected: it silently
   breaks schemas declared without `//` (the `uri` field is an arbitrary
   prefix string; matching is prefix-based, not grammar-based). The
   configured-prefix check (rule 3) covers the same cases without that
   regression.
3. **Tighten `has_scheme` itself.** Rejected: `has_scheme` is a pure
   syntactic predicate shared by `UriResolver::resolve` and the CLI
   argument check; making it config-aware would couple the predicate to
   the resolver. The routing decision belongs at the resolution layer,
   where the resolver is available.

## 6. Non-goals

- No change to slug generation (colons were always stripped there).
- No change to the parser (the scanner already passes targets through
  verbatim).
- No new diagnostics, flags, or config keys.
- No change to Windows drive-letter targets (`C:\…`): they remain broken
  links, with or without the hint.

## 7. Spec changes

- RES-07 **Detection** paragraph rewritten to the three-rule form.
- LNK-02/LNK-03 unchanged (web-scheme suppression is untouched).
- New tests listed under RES-07.

## 8. Verification

1. New tests in `tests/integration.rs` (failing before the fix):
   `wiki_link_with_colon_in_title_resolves`,
   `wiki_link_with_colon_in_stem_resolves`,
   `wiki_link_with_colon_alias_form_resolves`,
   `wiki_link_with_colon_no_match_is_plain_broken`,
   `markdown_link_with_colon_in_filename_resolves`.
2. Existing `tests/schemas.rs` suite unchanged and green — the
   `://`-form and configured-prefix paths are untouched.
3. Full `cargo test` + `cargo clippy --all-targets`.
