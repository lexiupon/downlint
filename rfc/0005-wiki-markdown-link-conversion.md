# RFC: Bidirectional Wiki-Link ↔ Markdown-Link Conversion

## Status

Draft

## Motivation

Wiki-style links (`[[filename]]`, `![[filename]]`) are convenient in editors and Obsidian,
but they do not render in standard markdown pipelines: GitHub, GitLab, Hugo, MkDocs,
pandoc, and most static-site generators treat `[[x]]` as literal text. Authors who want
to publish their notes externally face two options today:

1. Manually rewrite every wiki link to a markdown link — error-prone and unscalable.
2. Keep their notes in Obsidian / a wiki-aware renderer — limits where the content can be
   published.

Downlint already has the machinery to *resolve* wiki links for diagnostics and LSP
features. Reusing that machinery, we can offer a **bidirectional converter** that:

- Emits a clean, relative-path markdown link that renders correctly in any standard
  markdown engine.
- Restores wiki links on the way back, preserving the user's intent when they edit again.

The converter ships as both a **CLI subcommand** (`downlint convert …`) for batch /
publish workflows, and as **LSP code actions** for in-editor link-by-link conversion.

## Proposal

### Overview

Add a `downlint convert` CLI subcommand and matching LSP code actions that translate
between wiki-style and markdown-style links, using the same resolution pipeline as the
existing diagnostics.

### CLI: `downlint convert <SUBCOMMAND> --direction <DIR> [PATH]`

Subcommand shape:

```
downlint convert <SUBCOMMAND> --direction <DIR> [--dry-run] [--output <FILE>] <PATH>...
```

`<SUBCOMMAND>` is reserved for future direction-specific knobs. In v1, the only valid
subcommand is `links`. `--direction` is **required** (no default) and accepts one of:

- `wiki-to-markdown` — rewrite `[[x]]` / `![[x]]` to markdown link syntax.
- `markdown-to-wiki` — rewrite markdown `.md` links back to wiki-link syntax.

`<PATH>...` is one or more `.md` files or directories. If any argument is a directory,
the converter recursively processes every `.md` file under it. At least one `PATH`
is required.

Flags:

- `--dry-run` — print what would change, do not write. Emits a unified-diff-style
  report to stdout.
- `--output <FILE>` — see "Output Modes" below. **Single-file mode only.**

Exit code: `0` if all convertible links succeeded, `1` if any link was refused.
Refusal warnings do **not** abort the batch. Exit code reflects the worst outcome
across all input files.

### LSP Code Actions

Four code actions, registered on the document:

1. **`Convert this wiki link to markdown`** — appears on `[[x]]` and `![[x]]` ranges.
   Operates on the single link under the cursor.
2. **`Convert all wiki links in document to markdown`** — appears at the document level
   when the file contains at least one wiki link. Operates on all wiki links in the
   document.
3. **`Convert this markdown link to wiki`** — appears on markdown link ranges pointing
   to a relative or absolute `.md` target. Operates on the single link under the
   cursor.
4. **`Convert all markdown links in document to wiki`** — appears at the document level
   when the file contains at least one convertible markdown link. Operates on all
   convertible markdown links in the document.

Workspace-wide conversion via LSP is **out of scope**. Users run the CLI for that.

#### Greyed-Out Code Actions on Refused Links

When a link **would be refused** if converted (ambiguous resolution, broken anchor,
non-`.md` markdown target on reverse), the per-link code action is still offered, but
**greyed-out** — the LSP `CodeAction` carries `disabled: { reason: string }` set to a
short refusal description. Clients that surface this show a tooltip like
"This link cannot be converted: heading 'foo' not found in target." Refused links
do not silently disappear from the action list.

The per-document code actions are similarly marked: if any link in the document would
be refused, the per-document action is offered greyed-out with a summary refusal
reason ("3 links would be refused: 2 ambiguous, 1 broken anchor"). The summary is
computed eagerly so the user sees the cost before invoking.

This keeps the user informed: refused links are visible in the action list rather
than absent, and the reason is one click away.

### Conversion Rules (Wiki → Markdown)

For each wiki link classified by the parser (i.e. **not** inside code fences or inline
code):

| Wiki syntax | Markdown output | Notes |
|---|---|---|
| `[[note]]` | `[<H1>](<path>.md)` | Title resolves from target file's H1; see "Title Source" |
| `[[note\|alias]]` | `[alias](<path>.md)` | Alias preserved verbatim |
| `[[note\|]]` | `[<H1>](<path>.md)` | Empty alias treated as no-alias |
| `[[#heading]]` | `[heading](#slug)` | Heading-only link; anchor only, no path |
| `[[note#heading]]` | `[<H1>](<path>.md#slug)` | Refused if heading does not exist; see "Anchor Validation" |
| `[[note#]]` | `[<H1>](<path>.md)` | Empty anchor treated as no-anchor |
| `![[note]]` | `![<alt>](<path>)` | Embed → image syntax |
| `![[note\|alt]]` | `![alt](<path>)` | Embed alias becomes alt text |
| `![[note.png]]` / `![[photo.jpg]]` / `![[file.pdf]]` | `![<alt>](<path>)` | Non-`.md` attachments handled identically |

#### Title Source

The default for `[[note]]` (no alias) is the **target file's H1 heading**, looked up via
the existing heading-resolution logic. If the target has no H1, the title falls back to
the **file stem** (the filename without extension) and a warning is emitted.

A new optional config knob controls this:

```toml
[convert]
title_source = "h1"   # default; resolves to target H1
title_source = "literal"  # uses the link target string verbatim (preserves round-trip)
```

`#[serde(deny_unknown_fields)]` is preserved on `PartialConvertConfig`. Unknown keys
under `[convert]` fail strict parsing.

When `title_source = "literal"`, the conversion table becomes:

| Wiki syntax | Markdown output |
|---|---|
| `[[note]]` | `[note](<path>.md)` |
| `[[note\|alias]]` | `[alias](<path>.md)` |

This restores round-trip stability for the no-alias case at the cost of uglier renders
when file stems are uninformative (e.g. `[20260801-topic-a-sub-x](note.md)`).

#### Path Resolution

The converter uses the existing `resolve_wiki_ref` pipeline (with `obsidian_prefix`
honoured when enabled in `[wiki]`), so resolution behaviour is identical to the
diagnostics. The resolved document path is then rewritten to a path **relative to the
source file**:

| Wiki target | Markdown path (relative to source) |
|---|---|
| `[[note]]` resolving to `note.md` (same dir) | `[H1](note.md)` |
| `[[./sub/note]]` resolving to `sub/note.md` | `[H1](sub/note.md)` (drop `./`) |
| `[[../sibling/note]]` resolving to `../sibling/note.md` | `[H1](../sibling/note.md)` |
| `[[/abs/path/note]]` resolving to `/abs/path/note.md` | `[H1](/abs/path/note.md)` |
| `[[note.md]]` (explicit extension) | `[H1](note.md)` |

The `path` is the **full resolved file path including extension**. We do not strip the
`.md` suffix. This makes the markdown link work in renderers that don't auto-append
extensions (most static-site generators don't).

For non-`.md` attachments (images, PDFs), the resolved extension is preserved.

#### Anchor Validation

If a wiki link has a heading anchor (`[[note#heading]]`), the converter:

1. Resolves `note` to a document.
2. Looks up `heading` in the resolved document using the **same slugify logic** the
   rest of downlint uses for `[[note#head]]` resolution.
3. **If the heading is not found**, the link is **refused** (see "Refusal & Warnings").
   The whole link is left as wiki syntax — anchor and all — so the user sees exactly
   which link needs attention.

Slug normalization must match downlint's existing slugify rules. The converter calls
the same `slugify_heading` helper that `resolve_wiki_ref` calls for heading lookup;
otherwise emitted anchors will be broken in the rendered output.

For markdown-link reverse direction, anchor validation is symmetric: a markdown link
with an anchor that doesn't resolve to any heading in the target file is refused.

#### Code Fences & Inline Code

The converter only operates on links that the parser classifies as actual link tokens.
Links inside fenced code blocks (` ``` ... ``` `) or inline code spans (`` `…` ``) are
**not** converted, regardless of syntax.

### Conversion Rules (Markdown → Wiki)

For each markdown link classified by the parser (outside code):

| Markdown | Wiki output |
|---|---|
| `[title](./path.md)` (relative, ends in `.md`) | `[[./path\|title]]` (alias preserved) |
| `[title](../path.md)` | `[[../path\|title]]` |
| `[title](/abs/path.md)` | `[[/abs/path\|title]]` |
| `[title](path.md)` (no leading `.` or `/`) | `[[path\|title]]` |
| `[title](note.md#slug)` | `[[note#slug\|title]]` |
| `[title](#slug)` | `[[#slug\|title]]` |
| `[title](https://example.com/x.md)` | (no-op) — external scheme, left alone |
| `[title](path/without/extension)` | (no-op) — not a markdown file target, left alone |
| `![alt](./photo.png)` | `![[./photo.png\|alt]]` |
| `![alt](path)` where `path` has non-`.md` extension | `![[path\|alt]]` |

**Markdown links whose target is not a relative or absolute path ending in `.md` (or a
known attachment extension for embeds) are left as-is.** This is the "no-op for already
in markdown link" rule. The reverse direction only converts clearly-convertible links;
ambiguous cases are left to the user.

The reverse direction does not resolve the target to verify it exists. Wiki links
without a resolvable target are valid (they get `DNL002`), and round-trip stability
is more important than validation here. The diagnostics layer still reports broken
links on the converted wiki output.

### Refusal & Warnings

When a link cannot be safely converted, it is **left untouched** in the output and a
warning is emitted. Per-link refusal — never whole-batch refusal.

| Cause | Wiki → Markdown | Markdown → Wiki |
|---|---|---|
| Ambiguous resolution (e.g. prefix matches multiple files) | Refused; warning | n/a — markdown → wiki does not resolve |
| Target file not found | Refused; warning | Allowed — wiki links can be broken |
| Broken anchor (heading not in target) | Refused; warning | Refused; warning |
| Frontmatter / code-fence / inline-code link | Skipped silently (not a link token) | Skipped silently |

Warnings are reported:

- **CLI**: to stderr, one line per warning, with `file:line:col:` prefix and the link
  text.
- **LSP**: as `Warning`-severity diagnostics on the source document, with the same
  message format. Refused links still surface a greyed-out code action with a
  `disabled: { reason: string }` (see "Greyed-Out Code Actions on Refused Links").

### Output Modes

`downlint convert` writes files in place by default. The `--output` flag changes the
destination **in single-file mode only**:

- **Single-file mode** (exactly one `PATH` argument, and that `PATH` is a file):
  `--output <FILE>` writes the converted content to `FILE` instead of overwriting
  the input.
- **Batch mode** (more than one `PATH` argument, or any `PATH` is a directory):
  `--output` is **rejected** with an error. Batch users who want a separate output
  directory should use the shell: copy the input tree first, then run in place on
  the copy. We deliberately do not support `--output DIR` for batch in v1 because
  it adds non-trivial path-collision and overwrite-confirmation logic that is
  better designed separately if requested.

In all cases, the converter emits a summary to stderr:

```
Converted 142 links across 17 files. Refused 3 links (see warnings).
```

### Dry Run

`--dry-run` prints a unified-diff-style report to stdout showing every change that
would be made, and writes nothing. Exit code reflects whether any changes would be
made (`0` = no changes, `1` = changes pending). This is the standard "preview" mode
for file-rewriting tools.

### Idempotency

Both directions are idempotent:

- Running `wiki-to-markdown` twice: the second pass sees no wiki links, no changes.
- Running `markdown-to-wiki` twice: the second pass sees only wiki links, no changes.

Running `wiki-to-markdown` then `markdown-to-wiki` on the same content does **not**
always round-trip back to the original wiki form. Known lossy cases:

1. **`[[note]]` with `title_source = "h1"`** — converted to `[H1](note.md)`, reversed
   to `[[note]]`. The H1 text is lost in the reversed wiki link; only the target is
   preserved.
2. **Shortened wiki links with `obsidian_prefix = true`** — `[[prefix]]` resolves to
   `full-name.md` and becomes `[H1](full-name.md)`. Reversing produces `[[full-name]]`,
   not the original `[[prefix]]`. Downlint cannot recover the original target string
   from the resolved path alone.
3. **`![[note.png]]` (embed with non-`.md` extension)** — `![alt](note.png)` reverses
   to `![[note.png|alt]]`. The leading `!` is preserved by the parser as an embed
   marker, so the `alt` is reconstructed. Round-trip is lossless here.

These cases are documented as known limitations. The CLI summary lists the count of
lossy conversions when relevant.

## Configuration Schema

New `[convert]` section:

```toml
[convert]
title_source = "h1"       # or "literal"; default "h1"
```

`#[serde(deny_unknown_fields)]` is preserved on `PartialConvertConfig`. Unknown keys
under `[convert]` fail strict parsing.

The existing `[wiki]` and other sections are unchanged. The converter honours
`[wiki].obsidian_prefix` when resolving wiki targets, so users who have enabled
prefix matching get the same resolution in the converter.

## Implementation

### `src/convert/mod.rs` (new)

A new top-level module exposing:

```rust
pub fn convert_document(
    source: &str,
    source_path: &Path,
    direction: ConvertDirection,
    config: &ConvertConfig,
    workspace: &ResolveInput,
) -> ConvertResult;

pub enum ConvertDirection { WikiToMarkdown, MarkdownToWiki }

pub struct ConvertResult {
    pub output: String,
    pub converted: usize,
    pub refused: Vec<RefusedLink>,
    pub warnings: Vec<ConvertWarning>,
}

pub struct RefusedLink {
    pub range: Range,
    pub reason: RefusalReason,
    pub message: String,
}
```

The function walks the parsed token stream, classifies each link, and applies the
conversion rules above. Conversion happens **at the token level**, not via regex over
raw text — this guarantees we never touch code-fence content.

### `src/convert/path.rs` (new)

Path-rewriting helpers:

```rust
fn rewrite_path_to_relative(
    resolved: &Path,
    source: &Path,
) -> PathBuf;

fn markdown_path_for(
    resolved: &Path,
    is_embed: bool,
) -> String;
```

`rewrite_path_to_relative` produces a path relative to `source` with `./` stripped
when the path doesn't cross directories. Cross-platform separator handling follows the
existing `src/resolution/` conventions.

### `src/cli/convert.rs` (new)

CLI plumbing:

- `downlint convert links --direction <DIR> [--dry-run] [--output <FILE>] <PATH>...`
  — dispatches to `convert_document` for each file. `--output <FILE>` is accepted
  only in single-file mode (one `PATH`, a file). `--dry-run` writes nothing and
  emits the diff report to stdout. Warnings printed to stderr with
  `file:line:col:` prefix.

### `src/lsp/code_actions.rs` (extended)

Add the four code actions (per-link + per-document, both directions). Code actions
are gated on link kind: the markdown conversion action only appears on wiki-link
ranges, and vice versa. Refused links still surface a code action with the LSP
`disabled` field set to a short refusal description (see "Greyed-Out Code Actions
on Refused Links"). The same logic applies to per-document actions when any
contained link would be refused.

### `src/config/mod.rs`

- Add `ConvertConfig { title_source: TitleSource }` to `Config`.
- Add `PartialConvertConfig { title_source: Option<TitleSource> }` with
  `deny_unknown_fields`.
- `TitleSource` enum: `H1` (default), `Literal`.
- Add `merge_convert()` and a `finalize_config` arm.

### `src/parser/` (no changes expected)

The parser already classifies wiki links, embeds, and markdown links, and tracks
code-fence / inline-code context. The converter reads these classifications directly.
If classification gaps are discovered during implementation, they are fixed in the
parser, not worked around in the converter.

## Tests

### Unit tests (`src/convert/`)

- All conversion-table cases above, including aliases, anchors, embeds, attachments,
  empty-alias, heading-only, empty-anchor.
- Title resolution: H1 found, H1 missing → fallback to stem, `title_source = "literal"`
  bypass.
- Anchor validation: existing heading passes, missing heading refuses, slug mismatch
  refuses.
- Path rewriting: same-dir, subdir (`./` stripped), parent (`../` preserved),
  absolute (leading `/` preserved), explicit `.md` extension preserved.
- Code-fence / inline-code: link inside ` ``` ` and inside `` ` ` `` is not converted.
- Frontmatter: link inside `---` block is not converted.
- Idempotency: running each direction twice yields no changes.

### Integration tests (`tests/integration.rs`)

- CLI: `downlint convert links --direction wiki-to-markdown ./fixtures/simple` produces
  expected output files; exit code is `0`; warnings emitted to stderr for refused
  links.
- CLI: `--dry-run` writes nothing and exits `1` when changes are pending.
- CLI: `--output <FILE>` in single-file mode writes to the given path instead of
  in place. In batch mode, `--output` is rejected with an error.
- CLI: ambiguous prefix (`obsidian_prefix = true`, multiple matches) refuses one link,
  converts others, exits `1`.
- CLI: broken anchor refuses the link, emits warning, exits `1`.
- LSP: code action on a wiki-link range produces the expected text edit.
- LSP: refused link surfaces a greyed-out code action with `disabled.reason`. Per-document
  action similarly greyed-out when any contained link would be refused.

## Backward Compatibility

This change is **fully additive**:

- No existing CLI commands or flags change.
- No existing diagnostic codes or messages change.
- No existing LSP methods change (only new code actions are registered).
- New `[convert]` config section defaults are non-breaking.
- Files not touched by the converter are byte-identical to the input.

## Risks

1. **Lossy round-trip (C1, C2 from the discussion).** Documented above. Users who
   care about rename propagation across markdown links should stay on wiki links, or
   use the `title_source = "literal"` knob to keep round-trip stable at the cost of
   uglier rendered titles.
2. **Refusal granularity.** Refusing the whole link on broken anchor (rather than
   dropping the anchor) is more conservative but blocks useful conversions on a
   single typo. We chose this for v1 — predictable, easy to debug. Future work:
   add a `--lenient-anchors` flag that drops the anchor instead.
3. **H1 ambiguity.** If a target file has multiple H1-equivalent headings (rare but
   possible), the title resolution picks the first. Documented in `Title Source`.
4. **Performance for large vaults.** Conversion is O(file size) per file, with
   resolution lookups O(1) amortized. A 10k-file vault should convert in well under
   a second on commodity hardware.
5. **Symlinks and case-insensitive filesystems.** The converter resolves paths
   literally — symlinks are followed once for resolution, but the resulting markdown
   link uses the **resolved** path, not the symlink path. On case-insensitive
   filesystems (macOS HFS+/APFS by default), `[[Note]]` and `[[note]]` resolve to the
   same file and produce the same markdown output. On case-sensitive filesystems
   (Linux ext4/btrfs default), they may resolve to different files. The converter
   does not normalize across cases; users on case-sensitive filesystems who mix case
   will see whatever the filesystem returns. This is documented as a known
   limitation, not solved in v1.
6. **Cross-platform line endings.** The converter preserves the input file's line
   endings (CRLF vs LF). No normalization is performed.
7. **Concurrent file modifications.** The converter reads each file once, converts,
   writes once. There is no locking; concurrent external edits during conversion are
   not handled. Standard CLI tool behaviour.

## Alternatives Considered

1. **One-way only (wiki → markdown).** Simpler, covers the publish use case. Rejected
   because users editing in markdown form will want to round-trip back to wiki syntax
   for in-editor features.
2. **Refuse the whole batch on any refusal.** Safer but hostile — one typo blocks
   thousands of conversions. Rejected in favour of per-link refusal with warnings.
3. **Drop anchor instead of refusing the link.** Easier to ship, but the rendered
   output silently loses information. Rejected in favour of explicit refusal with
   warning, surfacing the problem to the user.
4. **Markdown → wiki always resolves and validates.** Catches broken links early but
   breaks round-trip stability (you can't reverse the conversion without the original
   target). Rejected.
5. **Always emit `[<stem>](path.md)` (no H1 lookup).** Simplest, fully round-trip
   stable. Rejected because the rendered output is often ugly (`[20260801-topic-a-sub-x](note.md)`).
   The `title_source = "literal"` config knob covers users who want this.

## Open Questions

1. **Per-document conversion code action vs per-workspace.** We decided
   workspace-wide is CLI-only. If users request it, we can add a third code action
   gated on a workspace-wide confirmation prompt.
2. **`![[note]]` where `note` is a folder.** Obsidian supports folder embeds. We do
   not currently model folder embeds in resolution. Out of scope for v1.
3. **Block-level references.** `[[note#^block-id]]` (Obsidian block links) are not
   currently resolved by downlint. Out of scope until block IDs are added to the
   parser.

## Future Work

- `--lenient-anchors` flag for users who prefer dropping anchors over refusing links.
- Workspace-wide LSP code action with confirmation prompt.
- Block-ID support in resolution, then in conversion.
- Per-pattern title overrides (e.g. always use file stem for `daily/*.md`).
- A separate RFC for LSP rename propagation after markdown conversion.

## Out of Scope

- Converting Obsidian callouts / embeds beyond images and PDFs.
- Rewriting non-link content.
- Touching frontmatter.
- Resolving cross-vault or remote links.
- Auto-running the converter on save (LSP `onDidChangeContent` hook).
