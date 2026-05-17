# RFC: Add Ignore Patterns to `.downlint.toml`

## Status

Draft

## Motivation

Downlint currently relies solely on `.gitignore` for file exclusion during workspace discovery.
While this works for most projects, it creates friction in several real-world scenarios:

1. **Non-git projects** — Markdown-only workspaces (notes, docs, blogs) may not have a `.gitignore`
   file at all, leaving no way to exclude directories like `drafts/` or `archive/`.
2. **Separation of concerns** — `.gitignore` governs version control. Users may want to track
   `vendor/` or `node_modules/docs/` in git but exclude them from linting.
3. **Per-project overrides** — Parent `.gitignore` patterns may be too broad or too narrow for
   linting purposes. A project might want to exclude `staging/` from linting while keeping it
   versioned.
4. **CI/CD pipelines** — Build artifacts or generated docs (e.g., `docs/api/`) may need exclusion
   from linting without affecting git behavior.

In `~/kb-sinch`, for example, directories like `drafts/` and `review/` contain work-in-progress
markdown that should be tracked in git but not linted. Currently, the only option is to add these
to `.gitignore`, which prevents them from being versioned.

## Problem

The current approach has three core problems:

1. **No non-git exclusion** — Without a `.gitignore`, there is zero mechanism to skip files or
   directories during workspace discovery.
2. **Coupled concerns** — Forcing users to use `.gitignore` for linting exclusion couples version
   control policy with linting policy. These are orthogonal concerns.
3. **No config-level control** — `.downlint.toml` already governs `file_extensions`, `extra_folders`,
   and other discovery-related settings. Adding ignore patterns there provides a single source of
   truth for all workspace configuration.

## Proposal

Add an `ignore` key under `[core]` in `.downlint.toml` that accepts a list of glob patterns.
These patterns are applied in addition to (not instead of) `.gitignore` rules.

### Config Schema

```toml
[core]
ignore = [
    "drafts/**",
    "archive/**",
    "*.tmp.md",
    "node_modules/**",
]
```

### Behavior

- Patterns follow standard glob syntax (same as `.gitignore`):
  - `**` matches zero or more path components
  - `*` matches within a single path component
  - `?` matches a single character
  - `[abc]` matches character classes
- Patterns are relative to the workspace root
- Patterns are additive to `.gitignore` — both sources are respected
- Negation with `!` is supported (e.g., `!drafts/published/**` to re-include a subdirectory)
- Patterns apply to both file discovery during CLI check and LSP workspace indexing

### Resolution Order

```
1. Walk workspace with ignore crate (respects .gitignore)
2. Apply custom ignore patterns from .downlint.toml
3. Filter by configured file_extensions
4. Yield documents for parsing/diagnostics
```

### Example

Given this workspace:

```
project/
├── .downlint.toml
├── .gitignore
├── docs/
│   ├── guide.md
│   └── api.md
├── drafts/
│   ├── proposal.md
│   └── notes.md
└── archive/
    └── old.md
```

With `.downlint.toml`:

```toml
[core]
ignore = ["drafts/**", "archive/**"]
```

And `.gitignore`:

```
*.log
build/
```

Result:
- `docs/guide.md` → included
- `docs/api.md` → included
- `drafts/proposal.md` → excluded (custom ignore)
- `drafts/notes.md` → excluded (custom ignore)
- `archive/old.md` → excluded (custom ignore)
- `*.log` files → excluded (.gitignore)

### Negation Example

```toml
[core]
ignore = [
    "drafts/**",
    "!drafts/published/**",
]
```

Result:
- `drafts/proposal.md` → excluded
- `drafts/published/final.md` → included (negation overrides)

## Implementation

### Changes to `src/config/mod.rs`

Add `ignore` to `CoreConfig` and `PartialCoreConfig`:

```rust
#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub file_extensions: Vec<String>,
    pub heading_ids: HeadingIdsConfig,
    pub text_sync: TextSyncKind,
    pub title_from_heading: bool,
    pub extra_folders: Vec<String>,
    pub ignore: Vec<String>,          // NEW
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialCoreConfig {
    pub file_extensions: Option<Vec<String>>,
    pub heading_ids: Option<PartialHeadingIdsConfig>,
    pub text_sync: Option<TextSyncKind>,
    pub title_from_heading: Option<bool>,
    pub extra_folders: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,  // NEW
}
```

Update `finalize_config()` and `merge_core()` accordingly. Default is `Vec::new()`.

### Changes to `src/utils/workspace.rs`

Modify `collect_documents()` to apply custom ignore patterns via `WalkBuilder`:

```rust
fn collect_documents(root: &Path, config: &Config) -> std::io::Result<Vec<WorkspaceDocument>> {
    let ext_set: HashSet<String> = config.core.file_extensions.iter().cloned().collect();
    let mut builder = WalkBuilder::new(root);
    builder.follow_links(true).hidden(false);

    // Apply custom ignore patterns from config
    for pattern in &config.core.ignore {
        builder.add_filter(pattern);
    }

    // ... rest of document collection
}
```

The `ignore` crate's `WalkBuilder::add_filter()` accepts glob patterns and integrates them
with the existing `.gitignore` parsing.

### Changes to `src/config/project.rs`

No changes required — config loading already handles `.downlint.toml`.

### Changes to `src/lsp/` (file watching)

Update file watcher glob patterns to exclude custom ignore patterns. The watcher currently
uses `**/*.{md,markdown}`; it should respect the same ignore list to avoid watching excluded
files.

## Backward Compatibility

This change is **fully backward compatible**:

- Existing `.downlint.toml` files without `core.ignore` continue to work unchanged
- The default value is an empty list, which has no effect
- No breaking changes to the config schema

## Risks

1. **Pattern conflicts** — Custom ignore patterns that conflict with `.gitignore` negation
   rules could produce surprising results. Mitigation: document that custom patterns are
   additive, not replacement.
2. **Performance** — Each custom pattern adds overhead to `WalkBuilder`. Mitigation: patterns
   are compiled once per workspace discovery, not per-file. The `ignore` crate handles this
   efficiently.
3. **Pattern syntax ambiguity** — Users may expect different glob semantics (e.g., shell-style
   vs gitignore-style). Mitigation: document that patterns follow the `ignore` crate's syntax,
   which is gitignore-compatible.

## Alternatives Considered

1. **`exclude` instead of `ignore`** — Semantically equivalent; `ignore` was chosen to match
   the crate name and `.gitignore` terminology.
2. **Separate config file (`.downlintignore`)** — Adds a second config file to maintain. The
   `.downlint.toml` approach keeps all config in one place, consistent with other settings.
3. **Only support `.gitignore`** — The status quo. Does not solve the non-git and separation-of-concerns problems.
4. **CLI-only `--ignore` flag** — Useful for one-off checks but doesn't help LSP or repeated
   invocations. Config-based approach covers both.
5. **Regex patterns instead of globs** — More powerful but harder to write and debug. Globs
   are familiar to users from `.gitignore` and `.dockerignore`.
