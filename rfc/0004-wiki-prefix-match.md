# RFC: Opt-In Wiki-Link Filename Prefix Matching (Obsidian-style)

## Status

Draft

## Motivation

Knowledge-base authors commonly organise related documents by date or topic suffix, e.g.
`20260801-topic-a-sub-x.md`, `20260801-topic-a-sub-y.md`. From inside an index note, the
natural shorthand for that group is `[[20260801-topic-a]]` — a leading prefix shared by
several files.

Downlint v1 treats wiki-link targets as exact document stems (with case-insensitive title
fallback). Under that model `[[20260801-topic-a]]` is reported as a `DNL002` broken link even
though it clearly points at a small set of sibling documents. To reference either file
the author is forced to either:

1. Spell out the full stem (`[[20260801-topic-a-sub-x]]`), which is fragile when files are
   renamed, or
2. Add explicit aliases inside each target document's frontmatter, which is friction for
   what is essentially "type the start of the filename".

Obsidian users take this prefix shortcut for granted. Downlint should support it as an
opt-in.

## Problem

The current wiki-link resolution model in `src/resolution/mod.rs::resolve_wiki_ref` has no
notion of partial filename matching. Every target is resolved against:

- The target's own document stem (exact match).
- The document title (case-insensitive, slug-fallback).

Any other shape — including the common "leading prefix" shape — fails. There is no config
hook, no diagnostic hint, and no graceful fallback for authors who would benefit from
prefix resolution.

## Proposal

Add a new top-level `[wiki]` config section with one knob:

```toml
[wiki]
obsidian_prefix = true
```

When `obsidian_prefix` is **enabled**, wiki-link targets that do not match any document
by exact stem or title are matched against the **leading prefix of file stems**. When it
is **disabled** (the default), behavior is unchanged — and the broken-link diagnostic
gains a discoverability hint when partial matches exist (see "Discoverability Hint"
below).

### Matching Rules

With `obsidian_prefix = true`, after the existing `find_doc_matches` call (which combines
exact stem match, title-slug match, and relative-path match in one pass) returns zero
matches, prefix matching runs:

1. Treat the target string as a prefix.
2. Find every file in `ResolveInput.documents` and `ResolveInput.extra_documents`
   whose **stem** (filename without extension, as produced by the existing
   `path_without_extension()` helper in `src/resolution/path.rs`) starts with the target
   (case-insensitively, per `eq_ignore_ascii_case`).
3. If **zero** files match → unresolved, emit `DNL002`.
4. If **one** file matches → resolve to that document (existing single-destination
   success path).
5. If **two or more** files match → ambiguous, emit `DNL001` with `related` information
   pointing at each candidate path.

Stems are matched case-insensitively via `eq_ignore_ascii_case`, matching the existing
exact-stem behavior in `find_doc_matches` (see `src/resolution/mod.rs:649`). Trailing
`/` is unaffected — folder links continue to be handled by the existing
`is_folder_link_target` branch.

### Behavior Matrix

Given files `20260801-topic-a-sub-x.md` and `20260801-topic-a-sub-y.md`:

| Link | File(s) matching | `obsidian_prefix = false` | `obsidian_prefix = true` |
|---|---|---|---|
| `[[20260801-topic-a]]` | Both files | `DNL002` broken + hint listing candidates | `DNL001` ambiguous + related |
| `[[20260801-topic-a]]` | Only `…-sub-x.md` | `DNL002` broken + hint listing 1 candidate | Resolves to `…-sub-x.md` |
| `[[20260801-topic-a]]` | No matching file | `DNL002` broken, no hint | `DNL002` broken, no hint |
| `[[20260801-topic-a-sub-x]]` | `…-sub-x.md` | Resolves (exact match) | Resolves (exact match) |
| `[[20260801-topic-a\|Title]]` | Both files | `DNL002` broken + hint | `DNL001` ambiguous (alias preserved) |
| `![[20260801-topic-a]]` | Only `…-sub-x.md` | `DNL002` broken + hint | Resolves (embed, `is_embed = true`) |
| `[[20260801-topic-a#head]]` | Only `…-sub-x.md` has `#head` | `DNL002` broken + hint | Resolves to file, then anchor lookup |
| `[[20260801-topic-a#head]]` | Both files have `#head` | `DNL002` broken + hint | `DNL001` ambiguous (prefix resolution fails before heading lookup) |
| `[[folder/20260801-topic-a]]` | `folder/20260801-topic-a-sub-x.md` | Existing path-prefix resolution wins | Existing path-prefix resolution wins |
| `[[folder/]]` | Folder exists | Folder-link resolution wins | Folder-link resolution wins |
| `[[\|Title]]` | Title match found | Existing title-only behavior | Existing title-only behavior |

### Interaction with Existing Wiki-Link Forms

- **Alias** (`[[x\|title]]`): the alias is display-only and does not affect prefix matching.
  Match by `x` against file stems; if it uniquely resolves, render with the alias.
- **Embed** (`![[x]]`): same matching rules; `is_embed = true` is preserved through to
  resolution.
- **Heading anchor** (`[[x#head]]`): resolve `x` via prefix matching (or hint, when
  disabled), then perform the existing heading lookup on the resolved document.
- **Path-prefixed** (`[[folder/x]]`): the `folder/` portion is an explicit path and short-
  circuits the prefix matcher. Prefix matching only applies to the trailing segment after
  the final `/`. (See "Open Questions" for an alternative interpretation.)
- **Folder link** (`[[folder/]]`): trailing-slash folder resolution, defined by RFC 0003,
  is unchanged and runs before prefix matching.
- **Title-only** (`[[\|Title]]`): unchanged.
- **External scheme** (`[[https://...]]`): suppressed, unchanged.
- **Embed with prefix** (`![[20260801-topic-a]]`): treated as embed; otherwise identical to
  `[[20260801-topic-a]]`.

## Discoverability Hint

When a broken wiki-link target is a leading prefix of **at least one** file stem in the
workspace, the `DNL002` diagnostic message gains a second-line hint pointing the author
at the option and listing the candidates. The hint applies regardless of the
`obsidian_prefix` value — see Hint Rules below for the precise conditions.

```
Broken link: '20260801-topic-a' could not be resolved
Hint: enable 'wiki.obsidian_prefix' to match partial filenames
  (candidates: 20260801-topic-a-sub-x.md, 20260801-topic-a-sub-y.md)
```

If there are more than five candidates, only the first five are listed and the line is
suffixed with `(+N more)`. The hint is appended with a single `\n` so editors render it
on the next line of the diagnostic popup.

### Hint Rules

- **Trigger**: wiki-link is broken (post `find_doc_matches` returns zero matches **and**
  the attachment fallback in `finalize_doc_or_attachment` also fails) **and** ≥1 file
  stem in the workspace starts with the target.
- **Apply in the flag-off and flag-on-but-zero-match cases**: when the flag is off and
  the link is broken with ≥1 prefix candidate, the hint helps the user discover the
  option. When the flag is on, prefix matching either resolves the link, emits `DNL001`
  ambiguous, or returns zero matches — in which case the same hint is shown (so the
  user understands that even with prefix matching enabled, no file starts with this
  target). The ambiguous case (`DNL001`) is already handled by `related` information
  on the ambiguous diagnostic; the hint is redundant there but harmless.
- **Skip when 0 partial matches exist**: the link is genuinely broken and the hint would
  be noise.
- **Skip for non-wiki references** (markdown links, full/collapsed/shortcut): the prefix
  semantic is wiki-link-specific.
- **Apply to embed wiki-links**: embed syntax is a sub-form of wiki-link and authors
  benefit equally from the hint. The hint does not change the resolved kind — embeds
  still emit `DNL002` when broken, with the hint appended.
- **Cap of 5 candidates in the hint**, with `(+N more)` suffix.

### Implementation Sketch

In `src/resolution/`, alongside the existing `ResolvedDocument` collection, build a
prefix index once per workspace refresh:

```rust
// key: stem prefix (every prefix of every stem)
// value: list of document paths whose stem starts with that prefix
//
// Built eagerly; total size O(sum of stem lengths), well-bounded for any
// realistic vault. Lookup is O(1) HashMap lookup.
prefix_index: HashMap<String, Vec<PathBuf>>,
```

On unresolved wiki-link, after the existing resolution paths fail, look up the target in
`prefix_index`. The result determines the next step:

- Empty (0 candidates) → fall through to existing `DNL002` with no hint.
- Non-empty + flag off → keep `DNL002`, enrich `message` with the hint.
- Non-empty + flag on, 1 candidate → resolve to that document.
- Non-empty + flag on, ≥2 candidates → push an `AmbiguousReference` (existing
  `DNL001` path). The hint path does not run in this branch (the ambiguous diagnostic
  carries the candidate list in `related`).

The hint candidate list is read from `prefix_index`, with a cap of 5 applied at the
diagnostic-rule site.

## Configuration Schema

```toml
[wiki]
obsidian_prefix = true
```

Defaults: `obsidian_prefix = false`.

`#[serde(deny_unknown_fields)]` is preserved on `PartialWikiConfig`. Unknown keys under
`[wiki]` continue to fail strict parsing.

## Implementation

### Changes to `src/config/mod.rs`

- Add `WikiConfig { obsidian_prefix: bool }` to `Config`.
- Add `PartialWikiConfig { obsidian_prefix: Option<bool> }` with `deny_unknown_fields`.
- Add `merge_wiki()` and a `finalize_config` arm.
- Default: `WikiConfig { obsidian_prefix: false }`.

### Changes to `src/resolution/mod.rs`

1. Build `prefix_index: HashMap<String, Vec<PathBuf>>` once per `ResolveInput`
   construction, keyed by every leading prefix of every stem in `ResolveInput.documents`
   and `ResolveInput.extra_documents`. Stems are produced by the existing
   `path_without_extension()` helper for consistency with `find_doc_matches`. The flag
   `WikiConfig.obsidian_prefix` is read via `input.config.wiki.obsidian_prefix` — no
   new plumbing required, since `ResolveInput` already carries the full `Config`.

2. In `resolve_wiki_ref`, after the existing folder-link early-return and after the
   `find_doc_matches` calls return zero matches, insert:
   - If `WikiConfig.obsidian_prefix` is on, perform the prefix scan over
     `prefix_index`.
     - If 1 match → resolve as a single document (push to `resolved_references`).
     - If ≥2 matches → push an `AmbiguousReference` with destinations drawn from
       `prefix_index` and return.
     - If 0 matches → fall through to existing attachment fallback in
       `finalize_doc_or_attachment`.
   - If the flag is off, fall through directly to attachment fallback.
3. In the same fall-through site, **before** the existing `UnresolvedReference` push,
   always perform a `prefix_index` lookup so the hint path can read candidates
   regardless of flag state. The lookup result is attached as `hint_payload:
   Option<Vec<PathBuf>>` on the `UnresolvedReference` (see "Hint Path" below). This
   lookup runs even when `obsidian_prefix = true` because the flag-on / zero-matches
   case still benefits from the hint (otherwise the user has enabled the flag and the
   link is still broken — confusing without a hint).

The execution order in `resolve_wiki_ref` becomes:

```
folder-link check
  → find_doc_matches(primary)
    → find_doc_matches(extra)
      → prefix scan (if flag on, or always for hint lookup)
        → finalize_doc_or_attachment (attachment fallback)
          → UnresolvedReference push (with optional hint_payload)
```

### Changes to `src/resolution/mod.rs` (hint path)

Add a `hint_payload: Option<Vec<PathBuf>>` field to `UnresolvedReference` (defined in
`src/resolution/conn.rs`). When the prefix scan returns ≥1 candidate during the
`UnresolvedReference` push, populate this field with the candidate list. When 0
candidates exist, leave it `None`.

The diagnostic rule reads the field and formats the second-line message — keeping
`broken_link` a simple function of its inputs.

### Changes to `src/diagnostics/rules.rs`

In `broken_link`, when `reference` is a wiki-link variant and `hint_payload` is
`Some(candidates)`:

1. Take up to 5 candidates (cap at the rule site, not the resolution site, to keep the
   resolution payload size-bounded — but note: the payload itself can be the full list
   for `related` reuse).
2. Format the hint message:

   ```
   Hint: enable 'wiki.obsidian_prefix' to match partial filenames (candidates: <list>)
   ```

   or with truncation suffix `(+N more)` if more than 5 candidates were available.

3. Append the formatted hint to `message` with a single `\n` separator.

### Changes to `tests/integration.rs`

Add tests covering:

- Unique prefix resolves with the flag on.
- Ambiguous prefix produces `DNL001` with the flag on.
- Broken link with candidates produces `DNL002` plus hint with the flag off.
- Broken link without candidates produces plain `DNL002` (no hint) with the flag off.
- Alias form `[[x\|title]]` resolves via prefix.
- Embed form `![[x]]` resolves via prefix.
- Heading anchor `[[x#h]]` resolves to prefix file, then heading.
- Path-prefix `[[folder/x]]` does not invoke prefix matching.
- Folder-link `[[folder/]]` does not invoke prefix matching.
- Hint candidate cap (6 candidates → 5 shown + `(+1 more)`).
- Case-insensitive prefix matching (`[[202608-TOPIC-A]]` matches `…-sub-x.md`).
- Single-file mode (`ResolveInput.single_file = true`) still builds and uses
  `prefix_index` from `documents` and `extra_documents` — prefix matching applies
  uniformly across workspace and single-file CLI invocations.

## Single-File Mode

`ResolveInput` carries a `single_file: bool` flag set from `WorkspaceMode::SingleFile`.
The prefix index is still built from `documents` and `extra_documents` regardless of this
flag, so prefix matching behaves identically whether invoked from the LSP (full workspace)
or from the CLI on a single file. This is consistent with the existing
`find_doc_matches` behavior, which does not branch on `single_file`.

## Backward Compatibility

This change is **fully backward compatible**:

- Default value of `obsidian_prefix` is `false`.
- Existing `.downlint.toml` files without `[wiki]` continue to work unchanged.
- `#[serde(deny_unknown_fields)]` ensures unknown future keys under `[wiki]` fail loudly
  rather than silently being ignored.
- No change to existing diagnostic codes or messages for users who don't enable the flag
  (other than the new second-line hint, which is informational and additive).

## Risks

1. **Short-target collisions** — A link like `[[todo]]` with the flag on would attempt
   prefix matching against every `todo-*.md` in the vault. With many matches, the result
   is `DNL001` ambiguous, which is correct but noisy. Mitigation: the diagnostic still
   surfaces a list of candidates via `related`, so the user understands what to type.
   We do not propose a per-pattern allowlist in this RFC — that can be added later if
   needed.
2. **Performance** — Building `prefix_index` is `O(sum of stem lengths)`. For a 10k-file
   vault with average stem length 30, that's ~300k entries in the worst case. Acceptable
   in-memory. Lookup is `O(1)` HashMap.
3. **Cross-platform stem handling** — Stems are produced by the existing
   `path_without_extension()` helper in `src/resolution/path.rs`, which already
   normalizes backslashes to forward slashes. The prefix index uses the same helper so
   that cross-platform path separators do not affect matching. No new normalization
   required.
4. **Hint message length** — The capped list (5 candidates) keeps diagnostics short.
   Editors that render multi-line messages should handle this; if a particular client
   truncates, the `related` field carries the same information in a structured form.
5. **Heading-anchor interaction** — `[[prefix#head]]` resolves `prefix` first, then
   performs heading lookup. If the prefix is ambiguous, `DNL001` is emitted even when
   only one of the candidates has the heading. We accept this for v1; resolving
   "any candidate containing the heading" would require N heading lookups and is out
   of scope.

## Alternatives Considered

1. **Always-on prefix matching** — Matches the user's mental model of "this is how
   wiki-links work". Rejected because: (a) it's a silent behavior change for existing
   vaults; (b) it can mask typing mistakes (e.g. `[[todo]]` suddenly matching several
   files); (c) opt-in keeps the door open to disable if a vault proves problematic.
2. **Per-pattern allowlist** — `obsidian_prefix_patterns = ["2026*", "meeting-*"]`.
   More precise but adds a second config knob and a glob engine. Deferred — the global
   on/off is enough for v1.
3. **Suffix matching** — `[[topic-a-sub-x]]` matching `20260801-topic-a-sub-x.md`.
   Obsidian's plugin ecosystem does this, but it's a different mental model (the user
   typed the "interesting" part of the filename). Out of scope.
4. **Auto-detect from link shape** — If the target contains no `/` and no `#`, treat
   it as prefix-eligible. Magic and surprising. Rejected.
5. **Use existing `AmbiguousReference` for the hint path too** — i.e. when the flag is
   off and candidates exist, still emit `DNL001` rather than `DNL002 + hint`. Rejected
   because the link IS broken under the user's chosen rule set; the diagnostic must
   reflect that.

## Open Questions

1. **Path-prefixed prefix matching** — Should `[[folder/20260801-topic-a]]` (a folder
   prefix + a stem prefix) match `folder/20260801-topic-a-sub-x.md`? Proposal says no —
   path-prefix wins because the user typed an explicit folder. Alternative: apply prefix
   matching to the trailing segment after the last `/`. Resolution: keep the simpler
   rule for v1 (path-prefix wins); revisit if users complain.
2. **Title-prefix matching** — Should `[[My Note]]` (title match today) also trigger
   prefix matching against titles? Proposal says no — prefix matching is stem-only,
   matching Obsidian's plugin semantics. Title resolution stays as-is.
3. **Per-folder scopes** — Obsidian allows vault-wide prefix matching but some users
   prefer per-folder. Out of scope for v1; document as future work.

## Future Work

- Optional `obsidian_prefix_patterns: Vec<Glob>` to restrict which targets trigger
  prefix matching (mitigates risk #1 for large vaults).
- Optional `obsidian_prefix_min_length: usize` to require a minimum prefix length (e.g.
  ≥4 chars) before prefix matching activates.
- Suffix-based matching as a separate opt-in knob.
- Per-folder scope configuration.

## Out of Scope

- Wiki-link alias style preferences (e.g. always render `[[x\|Title]]`).
- Embed rendering and asset pipeline.
- Cross-vault resolution.