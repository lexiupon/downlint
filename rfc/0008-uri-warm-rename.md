# RFC 0008 — Rename `sync_*` → `warm_*` for URI Mapping

**Status**: Accepted
**Depends on**: [RFC 0006](./0006-external-asset-uri-mapping.md), [RFC 0007](./0007-uri-mapping-phase-2.md)
**Author**: downlint maintainers
**Target**: v1.2 (this release)

## Summary

The `[uri]` feature uses the word "sync" in three places where "warm" is more
accurate. This RFC renames:

| Before | After | Where |
|---|---|---|
| `[[uri.mappings]].sync_cmd` | `[[uri.mappings]].warm_cmd` | Config (per-mapping) |
| `[[uri.mappings]].sync_required` | `[[uri.mappings]].warm_required` | Config (per-mapping) |
| `[[uri.mappings]].sync_timeout` | `[[uri.mappings]].warm_timeout` | Config (per-mapping) |
| `downlint sync` (subcommand) | `downlint warm-uri-mappings` | CLI |
| `DNL007` diagnostic message | Updated wording | Diagnostics |

Internal types (`SyncDecision`, `SyncRunner`, `PathStatus`, etc.) and private
fields (`sync_mapping_count`) keep their existing names. They are
implementation details — what users see is config, CLI, and diagnostics.

## Motivation

### Why the word "sync" is wrong

`sync` carries too much baggage:

1. **Two-way sync** (Obsidian Sync, Notion sync) — users reasonably expect
   "downlint sync" to push local changes to a remote, or vice versa.
   downlint does no such thing.
2. **Metadata sync** (sync an index of link statuses) — implies state
   caching, which is a side effect at best.
3. **Run the `sync_cmd`** — the only accurate interpretation, but you have
   to already know about the config key to reach it.

What the command actually does: pulls external files locally so subsequent
validation is fast. That's "warming the cache" or "prefetching." The word
**"warm"** is unambiguous: prepare data for later use, no two-way semantics.

### Why now

- Phase 1 + Phase 2 just shipped; the user-facing surface is small enough
  to rename without massive fallout.
- The feature is pre-1.0; the project has not promised any wire-format
  stability for `.downlint.toml`.
- All current users of `[uri]` are early adopters who can update their
  configs trivially.

## Detailed Changes

### 1. Config keys (per-mapping)

```toml
# Before (RFC 0006, 0007):
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
sync_cmd = ["mdutil", "--enforce-locals", "{path}"]
sync_required = false
sync_timeout = 30

# After (RFC 0008):
[[uri.mappings]]
prefix = "onedrive://work/"
root = "~/Library/CloudStorage/OneDrive-Work/assets"
warm_cmd = ["mdutil", "--enforce-locals", "{path}"]
warm_required = false
warm_timeout = 30
```

Same names for `prefix`, `root`, `verify_cmd`, and the section-level
`auto_verify`. Only the three `sync_*` keys change.

### 2. Subcommand name

```
# Before:
$ downlint sync --allow-uri-sync

# After:
$ downlint warm-uri-mappings --allow-uri-sync
```

The new name reads as a verb phrase ("warm [the] URI mappings"). It is
unambiguous about what runs (URI mappings) and what happens to them
(warming).

The `--allow-uri-sync` flag name stays as-is. Renaming it to
`--allow-uri-warm` would be technically more consistent, but the CLI flag
appears in the help text of `check` and `server` (which don't have a
"warm" verb in their language), and renaming three places for one
vocabulary fix has worse ROI than leaving the flag alone. The flag name
is "the gate that lets URI-mapping subprocess execution happen," which
is true regardless of whether the user calls the subcommand `sync` or
`warm-uri-mappings`.

### 3. DNL007 diagnostic message

```
# Before:
"Skipped sync for 1 [uri.mappings] entry/entries (--allow-uri-sync not
 passed). Affected prefixes: scheme://. Re-run with --allow-uri-sync
 to execute their `sync_cmd` (per-file)."

# After:
"URI mappings skipped: 1 entry has a `warm_cmd` but --allow-uri-sync
 was not passed. Affected prefixes: scheme://. Run
 'downlint warm-uri-mappings --allow-uri-sync' to warm them."
```

The new message:

- Uses the new vocabulary (`warm_cmd`, not `sync_cmd`).
- Names the actual subcommand the user should run.
- Replaces the imperative "Re-run" with the more informative "Run" +
  the literal command.

### 4. Internal names (NOT renamed)

These keep their existing names because they describe internal mechanisms,
not user-facing surface:

- `SyncDecision::{Ran, Skipped, NotApplicable}` — describes what the
  runner decided to do internally.
- `SyncRunner` — implementation type.
- `PathStatus::{Present, Missing, SyncFailed, SyncTimedOut}` — outcome
  names. `SyncFailed` and `SyncTimedOut` describe what `warm_cmd` did
  internally; renaming them would touch every error message and test.
- `sync_mapping_count` (private method on `UriResolver`) — local helper.

If a future RFC wants to rename these too (for vocabulary consistency
inside the codebase), it's an internal change that doesn't affect users.

## Migration Path

### Clean break

The old keys produce a **hard parse error** at startup. No alias, no
fallback:

```
$ downlint check
downlint: error: uri.mappings[0]: unknown field `sync_cmd`, expected one of
            `prefix`, `root`, `warm_cmd`, `warm_required`, `warm_timeout`,
            `verify_cmd` (note: `sync_cmd` was renamed to `warm_cmd` in
            RFC 0008)
```

The error message names the new field and points at the RFC. Users
fix their config once.

### Why hard break over soft fallback

- **Code simplicity**: zero alias logic, zero deprecation warnings,
  zero migration tests.
- **Discoverability**: a parse error is impossible to miss. A
  deprecation warning gets lost in stderr and stays in configs forever.
- **User base**: small. The `[uri]` feature shipped in v1.1 / v1.2.
  Anyone using it is paying attention.
- **Project stage**: pre-1.0, no wire-format promises.

If we ever need to soften this (e.g., if a downstream tool reads
`.downlint.toml` and breaks), a follow-up RFC can add an alias.

### Search-and-replace guide

For users updating their configs:

```sh
# One-line sed for the common case:
sed -i.bak '
  s/^sync_cmd = /warm_cmd = /
  s/^sync_required = /warm_required = /
  s/^sync_timeout = /warm_timeout = /
' .downlint.toml
```

## Acceptance Criteria

- [ ] `PartialUriMapping` has fields `warm_cmd`, `warm_required`,
      `warm_timeout` (no `sync_*`).
- [ ] TOML with `sync_cmd` produces a clear parse error.
- [ ] `downlint warm-uri-mappings` exists and has the same behavior as
      `downlint sync` did.
- [ ] `downlint sync` (old name) errors out with a clear migration
      message pointing at `warm-uri-mappings`.
- [ ] DNL007 message uses `warm_cmd` and names the new subcommand.
- [ ] README and ROADMAP updated.
- [ ] RFC 0006 and RFC 0007 amended with a brief "renamed in RFC 0008"
      note in their config tables.

## Resolved Design Questions

1. **Should we keep `sync` as a deprecated alias for one release?**
   **No.** Subcommand aliases cost clap argument space and confuse
   `--help` output. The error message is sufficient.

2. **Rename `--allow-uri-sync` to `--allow-uri-warm`?**
   **No.** Same vocabulary reasoning applies, but the flag appears in
   `check` and `server` help where "sync" is still the natural noun.
   The rename would be technically consistent but practically noisier.

3. **Rename internal types (`SyncRunner`, `SyncDecision`)?**
   **No.** They describe internal mechanisms, not user-visible surface.
   Worth a follow-up RFC if/when the codebase grows further.

4. **Should the `--allow-uri-sync` gate still apply to `warm-uri-mappings`?**
   **Yes, unchanged.** The safety model is the same: the gate prevents
   `.downlint.toml` from executing arbitrary commands without explicit
   user consent.

5. **Update RFC 0006 and RFC 0007 to show the new names?**
   **Yes, with a brief "renamed in RFC 0008" note.** Don't rewrite
   the historical RFCs — they describe the design at the time. Just
   add a paragraph noting the rename and pointing at this RFC.

## Out of Scope

- Renaming `diagnostics::manager.rs` (pre-existing dead code).
- Streaming progress for `warm-uri-mappings` (Phase 3+).
- `workspace/didChangeConfiguration` for LSP flags (deferred).
- Renaming `SyncRunner` / `SyncDecision` internal types.

## Migration Impact Estimate

Based on the project's adoption:

- Existing `[uri.mappings]` configs in the wild: probably a handful at
  most (the feature shipped recently).
- Scripts calling `downlint sync`: very few — the subcommand is new.
- Test fixtures in this repo: ~15 sites that need updating.

Total migration cost for the project itself: ~30 minutes. External user
cost: one `sed` command per config.