# chat-search — Mutation Testing Report

**status:** historical
**authority:** evidence

**Update — 2026-08-23** (ledger campaigns at `ed18e5c` baseline → `0942091` closing,
branch `fix/main-agent-mutant-survivors`; timeout-table entry for this crate landed in
the same branch)

| Metric | Baseline | After |
|--------|:---:|:---:|
| Viable mutants | 33 | 33 |
| Caught | 19 | **29** |
| **Missed** | **9** | **0** |
| Unviable | 4 | 4 |

## Fixed (9 mutants, every one verified KILLED before recording)

- **`query.rs` `find_start` (2):** constant-`Some(0)`/`Some(1)` replacements survived every
  boolean "does it match" assertion in the suite; exact byte-offset assertions (first literal
  occurrence, first regex match, and `None` when absent) pin the snippet-centering contract.
- **`scan.rs` snippet bounds (6):** the leading/trailing ellipsis conditions (`start > 0`,
  `end < len`) withstood relative assertions; three document shapes (match centered in long
  text → both ellipses, match at start of short text → none, match at start of a fully
  included tail → leading only) kill all six operator swaps.
- **`scan.rs` `search` root guard (1):** guard-true swallows every `read_dir` error as an
  empty result. A missing directory stays an honest empty result; a root that is a plain file
  must propagate as `SearchError::Io` — no permissions tricks needed, portable across hosts.

New tests follow the crate's sibling convention (`query/tests.rs`, `scan/tests.rs`, wired via
`#[cfg(test)] #[path = ...] mod survivor_tests;`).

## Accepted survivors (0)

None — every baseline survivor was killed.

## Process note

The crate's cold-cache baseline test phase exceeded the 3s default timeout (Tantivy link
time), killing the campaign before any mutant ran — the same signature as conversation-store.
Fixed by a `liberado-chat-search` entry in the per-package timeout table
(`crates/cli/src/mutants_cmd.rs`).
