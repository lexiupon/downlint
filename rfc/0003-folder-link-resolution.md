# RFC: Support Folder Link Resolution

## Status

Draft

## Motivation

In knowledge bases and documentation projects, it is common to organize related content into
directories and reference those directories as logical units. For example:

```markdown
See the [UK Shortcode Fixed Fee Audit case](/projects/avon/cases/20250911-uk-shortcode-fixed-fee-audit/)
for details.
```

Here the trailing `/` signals a link to a **folder** rather than a specific file. This pattern
appears frequently in:

- **Case studies / project folders** — A directory containing a `README.md`, supporting docs,
  and assets, referenced as a single unit.
- **Team / people directories** — A folder containing all docs about a team or person.
- **Feature documentation** — A directory with multiple related files, linked from an index.

Currently, downlint only resolves links to **files** (documents, headings, attachments). Links
ending with `/` that target directories are always reported as broken, even when the directory
exists and contains valid content.

In `~/kb-sinch`, for example:

```markdown
[case](../../../projects/avon/cases/20250911-uk-shortcode-fixed-fee-audit/)
```

This link targets a real directory but is flagged as broken because downlint has no concept of
directory destinations.

## Problem

The current resolution model has three gaps:

1. **No directory destination type** — `ResolveDestinationKind` has `Document`, `Heading`, and
   `Attachment`, but no `Directory` variant.
2. **Directory filtering in discovery** — `collect_documents()` and `load_extra_documents()`
   filter with `path.is_file()`, so directories are never added to the resolution graph.
3. **No trailing-slash convention** — Markdown links ending with `/` are treated the same as
   links without `/`, with no special handling for directory targets.

## Proposal

Add a new `Directory` destination kind and resolve links ending with `/` against known
directories in the workspace and extra folders.

### Link Syntax

A link is considered a **folder link** when its target path ends with `/`:

```markdown
[link text](./cases/20250911-audit/)          → relative folder
[link text](/projects/avon/cases/20250911-audit/)  → absolute folder (from root)
[link text](cases/20250911-audit/)             → relative folder (no trailing /, not a folder link)
```

Wiki-links can also reference folders using the same convention:

```markdown
[[/cases/20250911-audit/]]
```

### Resolution Rules

1. **Target detection** — A link target ending with `/` is a folder link candidate.
2. **Path resolution** — Resolve the path relative to the source document (for relative paths)
   or the workspace root (for absolute paths starting with `/`).
3. **Existence check** — The link resolves successfully if a directory exists at the resolved
   path in the workspace or any extra folder.
4. **No content required** — The directory does not need to contain any specific files (e.g.,
   `README.md`). An empty directory is a valid target.
5. **Symlink support** — Symlinked directories are followed (consistent with existing
   `follow_links(true)` behavior).

### New Destination Kind

Add `Directory` to `ResolveDestinationKind`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum ResolveDestinationKind {
    Document,
    Heading,
    Attachment,
    Directory,  // NEW
}
```

### Diagnostic Behavior

| Scenario | Diagnostic |
|----------|------------|
| Folder link → existing directory | No diagnostic (resolved) |
| Folder link → non-existent directory | `DNL002` broken link |
| Folder link → existing file (not dir) | `DNL002` broken link (mismatch) |
| Non-folder link → existing directory | No diagnostic (treated as doc link, may or may not resolve) |

### Example

Given this workspace:

```
project/
├── .downlint.toml
├── README.md
├── cases/
│   ├── 20250911-audit/
│   │   ├── README.md
│   │   └── findings.md
│   └── 20250912-review/
│       └── notes.md
└── docs/
    └── index.md
```

In `docs/index.md`:

```markdown
[Case Audit](../cases/20250911-audit/)        → ✅ resolved (directory exists)
[Case Review](../cases/20250912-review/)       → ✅ resolved (directory exists)
[Missing Case](../cases/20250999-missing/)     → ❌ DNL002 (directory doesn't exist)
[Case Audit](../cases/20250911-audit)          → ❓ resolved as doc link (may resolve to README.md if configured)
```

## Implementation

### Changes to `src/resolution/conn.rs`

Add `Directory` variant to `ResolveDestinationKind`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum ResolveDestinationKind {
    Document,
    Heading,
    Attachment,
    Directory,
}
```

### Changes to `src/resolution/mod.rs`

1. **Collect directories** — During workspace discovery, track directories alongside files.
   Add a `directories: HashSet<PathBuf>` field to `ResolveInput` or a parallel collection.

2. **Detect folder links** — In the reference resolution logic, check if the target ends with
   `/`. If so, treat it as a folder link candidate.

3. **Resolve folder links** — For folder link candidates, check if the resolved path exists as
   a directory in the workspace or extra folders. If yes, add a `Directory` destination.

```rust
// Pseudocode for folder link resolution
fn resolve_folder_link(
    source_path: &Path,
    target: &str,
    root: &Path,
    extra_roots: &[PathBuf],
) -> Vec<ResolvedDestination> {
    // Strip trailing /
    let target_path = target.trim_end_matches('/');

    // Resolve relative to source
    let resolved = resolve_path(source_path, target_path);

    // Check in workspace root
    let mut destinations = Vec::new();
    if resolved.is_dir() {
        destinations.push(ResolvedDestination {
            path: resolved.clone(),
            kind: ResolveDestinationKind::Directory,
        });
    }

    // Check in extra folders
    for extra_root in extra_roots {
        let candidate = extra_root.join(target_path);
        if candidate.is_dir() {
            destinations.push(ResolvedDestination {
                path: candidate,
                kind: ResolveDestinationKind::Directory,
            });
        }
    }

    destinations
}
```

### Changes to `src/resolution/path.rs`

No changes needed — existing path resolution logic handles the path computation. The new
logic only adds the directory existence check.

### Changes to `src/diagnostics/`

No changes needed — `DNL002` (broken link) already handles unresolved references. Folder links
that don't resolve will naturally emit `DNL002`.

## Backward Compatibility

This change is **fully backward compatible**:

- Existing links without trailing `/` are unaffected
- No changes to config schema
- No changes to existing destination kinds
- New `Directory` kind is additive

## Risks

1. **Ambiguity with file links** — A link like `./cases/` could theoretically match both a
   directory `cases/` and a file `cases` (no extension). The trailing `/` convention disambiguates
   this — only links ending with `/` are treated as folder links.

2. **Performance** — Tracking directories adds overhead during workspace discovery. Mitigation:
   directories are only tracked at the top level of each workspace root, not recursively.
   Sub-directories are discovered as part of normal file traversal.

3. **Empty directories** — An empty directory is a valid link target. This is intentional —
   the directory may be populated later, or it may serve as a placeholder. If this is undesirable,
   we could add a config option `folder_links.require_non_empty = true`.

## Alternatives Considered

1. **Require `README.md` in every linked folder** — This would make folder links resolve to the
   `README.md` inside the folder. Simpler to implement (reuse existing document resolution) but
   adds a convention requirement that many projects don't follow.

2. **Special config flag** — `folder_links.enable = true` in `.downlint.toml`. Adds complexity
   for a feature that should arguably be on by default (trailing `/` is a standard convention).

3. **Auto-detect from link text** — If the link text matches a directory name, treat it as a
   folder link. Too fragile and error-prone.

4. **Resolve to first file in directory** — Similar to requiring `README.md` but less predictable.
   Which file is "first"? Alphabetical? Most recent? Ambiguous.

## Open Questions

1. Should folder links support heading anchors? (e.g., `./cases/audit/#summary`) — Probably not,
   since folders don't have headings. If needed, the link should point to a specific file.

2. Should we emit a warning when a folder link target exists as both a file and a directory?
   (e.g., `cases/` is both a directory and `cases` is a file.) — The trailing `/` convention
   should prevent this, but edge cases may exist.

3. Should folder links be supported in wiki-link syntax? (`[[/cases/audit/]]`) — Yes, for
   consistency. The trailing `/` convention applies to both markdown and wiki links.
