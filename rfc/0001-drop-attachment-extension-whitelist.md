# RFC: Drop Attachment Extension Whitelist, Resolve Explicit File-Like Targets Directly

## Status

Draft

## Motivation

Downlint currently requires local attachment extensions to be whitelisted before it will check
whether the referenced file exists on disk. This produces false-positive `DNL002` broken-link
warnings for perfectly valid explicit file references such as spreadsheets, documents, archives,
and other non-markdown assets.

In `~/kb-sinch`, links like:

```markdown
[pricing](/assets/finance-xls-2023/product-pricing/product-pricing-structure-team-input.xlsx)
[okrs](/assets/company-okrs/company-draft-okrs-2026.docx)
```

are reported as broken even though the files exist, because `.xlsx` and `.docx` are not part of
the built-in attachment extension allowlist.

This is a bad default for two reasons:

1. Explicit file-like link targets already communicate user intent.
   - `./report.xlsx`, `../assets/logo.svg`, `/docs/brief.pdf`, and `data.csv` are not fuzzy
     title-style references. They are concrete filesystem targets.
2. Users should not need config churn just to link common asset types.
   - Requiring `core.attachment_file_extensions_add` to silence false positives turns routine
     linking into an allowlist maintenance problem.

## Problem

The current whitelist approach has three core problems:

1. **False positives by default** — Existing explicit file references are flagged as broken unless
   their extension happens to be in the allowlist.
2. **Unbounded maintenance** — New file types always require config updates.
3. **Weak disambiguation value** — The allowlist is doing path-intent detection poorly. The shape
   of the target is the stronger signal.

## Proposal

For **explicit file-like local targets**, Downlint should always check the resolved filesystem path
after document resolution fails, regardless of extension.

An explicit file-like target is either:

- A slash-based path: starts with `/`, `./`, `../`, or contains `/` or `\`
- A same-directory basename with an extension, such as `data.xlsx`

Resolution order stays the same:

1. Try document resolution first.
2. If no document matched and the target is explicit file-like, resolve the filesystem path.
3. If the path exists, resolve it as `Attachment`.
4. If the path does not exist, report it as broken.

The attachment extension whitelist is removed entirely.

### Behavior Change

| Scenario | Before | After |
|---|---|---|
| `[](data.xlsx)` — file exists | Broken unless extension is whitelisted | Resolved as attachment |
| `[](./data.xlsx)` — file exists | Broken unless extension is whitelisted | Resolved as attachment |
| `[](./data.xlsx)` — file missing | Broken unless extension is whitelisted | Broken |
| `[](./notes/todo.md)` — document exists | Resolved as document | Resolved as document |
| `[](./notes/todo.md#next)` — heading exists | Resolved to heading | Resolved to heading |
| `[](intro)` | Existing inline-link behavior | Unchanged |
| `[](https://example.com)` | Skipped | Skipped |

### What Stays the Same

- Document resolution still runs before attachment fallback.
- Targets without path separators and without an extension, such as `intro`, do not trigger
  attachment filesystem fallback.
- Scheme-based links such as `https:` and `mailto:` are still skipped.
- Anchor resolution still only applies when the resolved destination is a document.

## Implementation

### Changes to `src/resolution/mod.rs`

Replace the whitelist-based attachment gate in `finalize_doc_or_attachment()` with a dedicated
helper such as:

```rust
fn is_attachment_candidate_path(target: &str) -> bool {
    target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
        || target.contains('\\')
        || target
            .rsplit_once('.')
            .is_some_and(|(base, ext)| !base.is_empty() && !ext.is_empty())
}
```

Use that helper only for attachment fallback. Do not reuse the broader `is_explicit_path()`
document-matching heuristic, because that helper treats any `.` anywhere in the target as
path-like.

### Changes to `src/config/mod.rs`

- Remove `attachment_file_extensions` from `CoreConfig`
- Remove `attachment_file_extensions_add` from `PartialCoreConfig`
- Remove the merge/finalization logic for attachment extensions
- Replace the old config accumulation test with a regression that proves the removed key now fails
  strict parsing

### Changes to `src/resolution/path.rs`

No changes are required. `resolve_explicit_path()` already resolves root-absolute and relative
paths correctly.

## Breaking Change

This change is intentionally breaking for config files.

Downlint uses `#[serde(deny_unknown_fields)]` for config parsing. Once
`core.attachment_file_extensions_add` is removed from the schema, existing `.downlint.toml` files
that still contain that key will fail to parse at startup.

There is no compatibility shim in this proposal. Users must remove the key.

## Migration

1. Delete `core.attachment_file_extensions_add` from `.downlint.toml`.
2. Rerun Downlint.
3. Explicit file-like links now resolve without any attachment extension config.

## Risks

1. **More filesystem checks** — Unresolved explicit file-like targets now perform an existence
   check. This is limited to targets that already failed document resolution.
2. **Broader attachment fallback surface** — A basename like `report.v2` now qualifies as a
   file-like target. This is acceptable because document resolution still runs first.
3. **Config breakage** — Old configs using the removed key will error until updated. This is
   acceptable because strict config parsing already makes removed keys a breaking change.

## Alternatives Considered

1. **Expand the default whitelist** — Still requires ongoing maintenance and never covers all real
   file types.
2. **Keep the whitelist and add a fallback** — Adds indirection without preserving meaningful
   behavior.
3. **Slash-only fallback** — Misses common same-directory links such as `data.xlsx`.
