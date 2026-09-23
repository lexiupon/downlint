# AGENTS.md

Guidance for coding agents working in this repo.

## Release process

1. Implementation commit first, then a `Release X.Y.Z — <title>` commit.
2. Bump `version` in `Cargo.toml` only (`src/version.rs` reads
   `env!("CARGO_PKG_VERSION")`). Add the CHANGELOG entry at the top.
3. **Run a cargo command (e.g. `cargo build`) *before* the release
   commit**, so `Cargo.lock` is synced in the same commit. The lock has
   drifted twice (0.15.2, 0.15.3) because the build ran *after* the
   commit and regenerated the lock into an uncommitted change.
4. Work happens on `dev`; `main` receives squash merges. No tags by
   default; when tagging, use `vX.Y.Z` on the main squash commit.

## RFC process

- RFCs live in `spec/rfcs/`, numbered **monotonically, never reused**.
  Next number = max + 1, read from the index in `spec/rfcs/README.md`.
- **Commit the RFC before (or with) implementation.** RFCs 0001–0017
  were never committed and are lost; only their numbers survive in code
  citations.
- Statuses: `Proposed` → `Accepted` (implemented). `spec/downlint.md` is
  the living spec (what); RFCs are decision records (why).

## Test conventions

- Canonical link-example vocabulary (RFC 0020): `report`/`topic` (rename
  pair), `index` (linking doc), `missing` (unresolved target),
  `image.png`/`missing.png` (attachments), `old-id`/`new-id`
  (link-rename). **Vary only what the test asserts on** — syntax
  variants, schemes, Unicode/mojibake, real-world families, and graph
  role names are asserted variation; see the keep-list in RFC 0020.
- Shared fixture helper: `tests/common/mod.rs` (`write_vault`). Don't
  re-copy it into new test files.

## Verification

- `cargo test` (12 suites) and `cargo clippy --all-targets`.
- Clippy has pre-existing warnings in `src/` and `tests/schemas.rs`
  (unused `FileMove` import, `opts` variables, lints in `resolution/`
  and `parser/`) — don't chase them; just don't add new ones.
