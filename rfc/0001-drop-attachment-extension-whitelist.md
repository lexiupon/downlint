# RFC: Drop Attachment Extension Whitelist, Resolve Explicit Paths Directly

## Status

Draft

## Motivation

Downlint currently requires file extensions to be whitelisted via `attachment_file_extensions` (default: `png`, `jpg`, `jpeg`, `gif`, `svg`, `pdf`, `webp`) before it will check whether a linked file exists on disk. Any extension not in the list (e.g. `.xlsx`, `.docx`, `.csv`, `.zip`) is silently skipped, producing false-positive "broken link" warnings (DNL002) even when the file physically exists.

**Reproduction**: In `~/kb-sinch`, links like:

```markdown
[pricing](/assets/finance-xls-2023/product-pricing/product-pricing-structure-team-input.xlsx)
[okrs](/assets/company-okrs/company-draft-okrs-2026.docx)
```

are reported as broken despite the files existing, because `.xlsx` and `.docx` are not in the default whitelist.

Users must manually add a `.downlint.toml` with `attachment_file_extensions_add` to work around this.

## Problem

The whitelist approach has several issues:

1. **False positives by default** — Any non-image/non-pdf attachment is flagged as broken out of the box.
2. **Maintenance burden** — Users must anticipate and declare every file type they link to.
3. **No real benefit** — The whitelist was intended to distinguish "attachment files" from "links to other markdown documents that might be resolved by slug/title matching." But explicit paths (starting with `/`, `./`, `../`) already signal the user's intent clearly.

## Proposal

For **explicit paths** (links starting with `/`, `./`, `../`, or containing `/` or `\`), always check whether the resolved filesystem path exists, regardless of extension. If the file exists, resolve the link. If it doesn't, report it as broken.

The `attachment_file_extensions` whitelist would be removed entirely.

### Behavior change

| Scenario | Before | After |
|---|---|---|
| `[](./data.xlsx)` — file exists | Broken (unless whitelisted) | Resolved |
| `[](./data.xlsx)` — file missing | Broken (unless whitelisted) | Broken |
| `[](./notes/todo.md)` — file exists | Resolved (as document) | Resolved (as document) |
| `[](./notes/todo.md)` — file missing | Broken (as document) | Broken (as document) |
| `[](./guide/intro)` — no extension | Resolved by slug/stem match | Resolved by slug/stem match (unchanged) |
| `[](https://example.com)` | Skipped (has scheme) | Skipped (has scheme, unchanged) |

The key insight: **explicit paths already disambiguate**. If the user writes `[](./foo.xlsx)`, they are explicitly pointing to a file, not asking for fuzzy document matching. We should just check if that file exists.

### What stays the same

- **Extensionless / implicit links** (e.g. `[see intro](intro)`) still use slug/stem/title fuzzy matching against known documents. No filesystem check is added here — this is where the old whitelist served a purpose (avoiding unnecessary disk checks for non-existent planned documents).
- **Scheme-based links** (e.g. `https://`, `mailto:`) are still skipped.
- **Anchor resolution** within documents is unchanged.

## Implementation

### Changes to `src/resolution/mod.rs`

In `finalize_doc_or_attachment`, replace the `is_attachment_path()` guard with a check for explicit paths:

```rust
fn finalize_doc_or_attachment(
    ctx: &mut ResolveRefContext<'_>,
    target: &str,
    anchor: Option<&str>,
    mut destinations: Vec<ResolvedDestination>,
) {
    if destinations.is_empty() && is_explicit_path(target) {
        let source_dir = doc.path.parent().unwrap_or(ctx.input.root.as_path());
        let path = resolve_explicit_path(&ctx.input.root, source_dir, target);
        if path.exists() {
            destinations.push(ResolvedDestination {
                path,
                kind: DestinationKind::Attachment,
                name: target.to_string(),
                range: None,
            });
        }
    }
    // ... rest unchanged
}
```

The existing `is_explicit_path()` function already covers this:

```rust
fn is_explicit_path(target: &str) -> bool {
    target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
        || target.contains('\\')
        || target.contains('.')
}
```

### Changes to `src/config/mod.rs`

- Remove `attachment_file_extensions` from `CoreConfig`.
- Remove `attachment_file_extensions_add` from `ProjectCoreConfig`.
- Remove the merge/extend logic for `attachment_file_extensions`.
- Update any config tests that reference the field.

### Changes to `src/resolution/path.rs`

No changes needed — `resolve_explicit_path` already handles absolute and relative paths correctly.

## Migration

- The `attachment_file_extensions` and `attachment_file_extensions_add` config options become no-ops (or are removed outright).
- Existing `.downlint.toml` files with these options can keep them; they would simply be ignored.
- A deprecation warning could be emitted if either key is present in user config.

## Risks

1. **Performance**: Checking the filesystem for every unresolved explicit path could be slower than the whitelist approach. Mitigation: this only runs for links that failed document matching, which is typically a small number.
2. **False negatives**: A link like `[](./foo)` (no extension) that happens to match a filesystem file but was intended as a document slug match. Mitigation: extensionless links go through `find_doc_matches` first; the filesystem check only runs if no document matched.
3. **Breaking change**: Users relying on the whitelist to *suppress* filesystem checks for certain extensions. Mitigation: this is unlikely, and the new behavior is more intuitive.

## Alternatives considered

1. **Expand the default whitelist** — Add common extensions like `xlsx`, `docx`, `csv`, `zip`, etc. This is a band-aid; new extensions will always appear.
2. **Invert the whitelist to a denylist** — Check all extensions except explicitly excluded ones. This requires maintaining a denylist and is less intuitive.
3. **Keep the whitelist but add a fallback** — Check the filesystem for any unresolved explicit path even if the extension isn't whitelisted. This is essentially the same as the proposed change but keeps the whitelist as a no-op.
