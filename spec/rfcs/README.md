# RFCs

Decision records for downlint: **why** a design was chosen and what was
rejected. The living spec — **what** downlint currently does — is
[`spec/downlint.md`](../downlint.md). RFCs are committed before (or with)
implementation and are never deleted; code comments cite them by number.

## Conventions

- **Numbers are monotonic and never reused**, even for abandoned or
  rejected RFCs (same practice as TC39 / Rust).
- **Next number = max + 1**, read from this index. → **0025**
- **Statuses**: `Proposed` → `Accepted` (implemented). `Superseded by NNNN`
  when a later RFC replaces the design.
- An RFC's content is a snapshot of its moment; the spec and the code are
  the current truth. If they disagree, the spec/code wins and the RFC stays
  as history.

## Index

| # | Title | Status |
|---|---|---|
| 0001–0003 | — (not in repo, not cited from code) | — |
| 0004 | Resolution behavior matrix *(title inferred from citations)* | Implemented |
| 0005–0008 | — (not in repo, not cited from code) | — |
| 0009 | Rename & Link Refactor | Implemented |
| 0010 | URI schemas & mounts config *(inferred)* | Implemented |
| 0011 | Mount conflict detection *(inferred)* | Implemented |
| 0012 | Target resolution query (`resolve`) | Implemented |
| 0013 | Anchor validation & path-based matching *(inferred)* | Implemented |
| 0014 | — (not in repo, not cited from code) | — |
| 0015 | Graph command (top-level `graph`) | Implemented; CLI surface superseded by 0018 |
| 0016 | `downlint info` report | Implemented |
| 0017 | LSP background re-indexer | Implemented |
| 0018 | CLI file/link surface | Accepted (0.15.0) |
| 0019 | Rename command cross-hints | Accepted (0.15.2) |
| 0020 | Canonical link-example vocabulary | Accepted (0.15.3) |
| 0021 | Colon-bearing link targets are not URIs | Accepted (0.15.4) |
| 0022 | LSP workspace freshness | Accepted (0.15.5) |
| 0023 | Hidden files excluded by default; `core.include_hidden` opt-in | Accepted (0.15.6) |
| [0024](0024-extensionless-inline-attachments.md) | Extensionless files as inline Markdown link targets | Accepted (0.15.7) |

> **Note on 0001–0017**: these RFCs were written before the repo adopted
> the commit-the-RFC convention and were never committed; only their numbers
> survive in code citations. Titles marked *(inferred)* are reconstructed
> from those citations. The dangling citations are resolvable via this
> index.
